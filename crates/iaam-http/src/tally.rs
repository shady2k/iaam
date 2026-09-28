use fs2::FileExt;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;

const HEADER: &str = "iaam-outbound-tally-v1";
const MINUTE_NANOS: u128 = 60_000_000_000;
const SECOND_NANOS: u128 = 1_000_000_000;
const REFUSAL_WINDOW_NANOS: u128 = 10 * MINUTE_NANOS;
const RATE_LIMIT_PAUSE_NANOS: u128 = MINUTE_NANOS;
const CLOSURE_NANOS: u128 = 30 * MINUTE_NANOS;
const DAY_NANOS: u128 = 86_400_000_000_000;
pub(crate) const DAILY_CEILING: u32 = 1_000;

#[derive(Debug, Error)]
pub(crate) enum TallyError {
    #[error("could not {action}: {source}")]
    File {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("the file is corrupt: {0}")]
    Corrupt(String),
    #[error("the UTC clock is before the Unix epoch")]
    ClockBeforeEpoch,
}

pub(crate) enum TallyDecision {
    Send,
    Wait(Duration),
    Paused {
        reopens_at: SystemTime,
        retry_after: Duration,
    },
    Closed {
        reason: ClosureReason,
        reopens_at: SystemTime,
        retry_after: Duration,
    },
    DailyCeiling {
        reset_at: SystemTime,
        retry_after: Duration,
    },
}

pub(crate) enum TallyResponseDecision {
    Recorded,
    RateLimited {
        reopens_at: SystemTime,
        retry_after: Duration,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum ClosureReason {
    Refusals,
    RateLimits,
    RepeatedResponses,
}

impl ClosureReason {
    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Refusals => "three refusals within ten minutes",
            Self::RateLimits => "two 429 responses within ten minutes",
            Self::RepeatedResponses => "repeated broker refusals",
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::Refusals => "refusals",
            Self::RateLimits => "rate-limits",
            Self::RepeatedResponses => "repeated-responses",
        }
    }
}

#[derive(Default)]
struct HostState {
    last_send: Option<u128>,
    day: u64,
    count: u32,
    closed_until: Option<u128>,
    closed_reason: Option<ClosureReason>,
    paused_until: Option<u128>,
    refusals: Vec<u128>,
    rate_limits: Vec<u128>,
}

#[derive(Default)]
struct State {
    hosts: BTreeMap<String, HostState>,
    sends: BTreeMap<(String, String), Vec<u128>>,
}

pub(crate) struct OutboundTally {
    path: PathBuf,
}
struct DecisionRequest<'a> {
    host: &'a str,
    budget_key: &'a str,
    budget_limit: u32,
    budget_window: Duration,
    now: SystemTime,
}

impl OutboundTally {
    pub(crate) fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
        }
    }

    pub(crate) fn check_host(
        &self,
        host: &str,
        now: SystemTime,
    ) -> Result<Option<TallyDecision>, TallyError> {
        let now_nanos = unix_nanos(now)?;
        self.transact(|state| {
            let changed = state.prune(now_nanos);
            let decision = state
                .hosts
                .get(host)
                .map(|host_state| Self::active_refusal(host_state, now_nanos))
                .transpose()?
                .flatten();
            Ok((decision, changed))
        })
    }

    pub(crate) fn decide_and_record(
        &self,
        host: &str,
        budget_key: &str,
        budget_limit: u32,
        budget_window: Duration,
        now: SystemTime,
    ) -> Result<TallyDecision, TallyError> {
        let request = DecisionRequest {
            host,
            budget_key,
            budget_limit,
            budget_window,
            now,
        };
        self.transact(|state| Self::decide(state, &request))
    }

    pub(crate) fn record_response(
        &self,
        host: &str,
        status: u16,
        retry_after: Option<Duration>,
        now: SystemTime,
    ) -> Result<TallyResponseDecision, TallyError> {
        let now_nanos = unix_nanos(now)?;
        self.transact(|state| {
            let changed = state.prune(now_nanos);
            let host_state = state.hosts.entry(host.to_owned()).or_default();
            match status {
                429 => {
                    host_state.rate_limits.push(now_nanos);
                    let pause = retry_after
                        .map_or(RATE_LIMIT_PAUSE_NANOS, |delay| delay.as_nanos())
                        .max(RATE_LIMIT_PAUSE_NANOS);
                    let paused_until = now_nanos.saturating_add(pause);
                    host_state.paused_until = Some(
                        host_state
                            .paused_until
                            .unwrap_or_default()
                            .max(paused_until),
                    );
                    if host_state.rate_limits.len() >= 2 {
                        let closed_until = now_nanos.saturating_add(CLOSURE_NANOS);
                        host_state.closed_until = Some(
                            host_state
                                .closed_until
                                .unwrap_or_default()
                                .max(closed_until),
                        );
                        host_state.closed_reason = Some(ClosureReason::RateLimits);
                    }
                    let reopens_nanos = host_state
                        .paused_until
                        .into_iter()
                        .chain(host_state.closed_until)
                        .max()
                        .unwrap_or(paused_until);
                    Ok((
                        TallyResponseDecision::RateLimited {
                            reopens_at: system_time_from_nanos(reopens_nanos)?,
                            retry_after: duration_from_nanos(
                                reopens_nanos.saturating_sub(now_nanos),
                            ),
                        },
                        true,
                    ))
                }
                400..=499 => {
                    host_state.refusals.push(now_nanos);
                    if host_state.refusals.len() >= 3 {
                        let closed_until = now_nanos.saturating_add(CLOSURE_NANOS);
                        host_state.closed_until = Some(
                            host_state
                                .closed_until
                                .unwrap_or_default()
                                .max(closed_until),
                        );
                        host_state.closed_reason = Some(ClosureReason::Refusals);
                    }
                    Ok((TallyResponseDecision::Recorded, true))
                }
                _ => Ok((TallyResponseDecision::Recorded, changed)),
            }
        })
    }

    fn decide(
        state: &mut State,
        request: &DecisionRequest<'_>,
    ) -> Result<(TallyDecision, bool), TallyError> {
        let DecisionRequest {
            host,
            budget_key,
            budget_limit,
            budget_window,
            now,
        } = *request;
        let now_nanos = unix_nanos(now)?;
        let day = u64::try_from(now_nanos / DAY_NANOS)
            .map_err(|_| TallyError::Corrupt("UTC day does not fit in the tally".to_owned()))?;
        let changed = state.prune(now_nanos);

        let host_state = state.hosts.entry(host.to_owned()).or_default();
        if let Some(decision) = Self::active_refusal(host_state, now_nanos)? {
            return Ok((decision, changed));
        }
        if host_state.day != day {
            host_state.day = day;
            host_state.count = 0;
        }
        if host_state.count >= DAILY_CEILING {
            let reset_nanos = u128::from(day + 1) * DAY_NANOS;
            return Ok((
                TallyDecision::DailyCeiling {
                    reset_at: system_time_from_nanos(reset_nanos)?,
                    retry_after: duration_from_nanos(reset_nanos.saturating_sub(now_nanos)),
                },
                changed,
            ));
        }

        let spacing_wait = host_state.last_send.map_or(0, |last| {
            last.saturating_add(SECOND_NANOS).saturating_sub(now_nanos)
        });
        let key = (host.to_owned(), budget_key.to_owned());
        let sent = state.sends.entry(key).or_default();
        let budget_wait = if sent.len() >= budget_limit as usize {
            sent[0]
                .saturating_add(budget_window.as_nanos())
                .saturating_sub(now_nanos)
        } else {
            0
        };
        let wait = spacing_wait.max(budget_wait);
        if wait != 0 {
            return Ok((TallyDecision::Wait(duration_from_nanos(wait)), changed));
        }

        sent.push(now_nanos);
        host_state.last_send = Some(now_nanos);
        host_state.count += 1;
        Ok((TallyDecision::Send, true))
    }

    fn active_refusal(
        host_state: &HostState,
        now_nanos: u128,
    ) -> Result<Option<TallyDecision>, TallyError> {
        if let Some(until) = host_state.closed_until.filter(|until| *until > now_nanos) {
            return Ok(Some(TallyDecision::Closed {
                reason: host_state
                    .closed_reason
                    .unwrap_or(ClosureReason::RepeatedResponses),
                reopens_at: system_time_from_nanos(until)?,
                retry_after: duration_from_nanos(until - now_nanos),
            }));
        }
        if let Some(until) = host_state.paused_until.filter(|until| *until > now_nanos) {
            return Ok(Some(TallyDecision::Paused {
                reopens_at: system_time_from_nanos(until)?,
                retry_after: duration_from_nanos(until - now_nanos),
            }));
        }
        Ok(None)
    }

    fn transact<R>(
        &self,
        update: impl FnOnce(&mut State) -> Result<(R, bool), TallyError>,
    ) -> Result<R, TallyError> {
        let lock_path = sibling_with_suffix(&self.path, ".lock")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| TallyError::File {
                action: "open the lock file",
                source,
            })?;
        FileExt::lock_exclusive(&lock).map_err(|source| TallyError::File {
            action: "lock the lock file",
            source,
        })?;

        let result = (|| {
            let mut state = self.read_state()?;
            let (value, changed) = update(&mut state)?;
            if changed {
                self.persist(&state)?;
            }
            Ok(value)
        })();
        let unlocked = FileExt::unlock(&lock).map_err(|source| TallyError::File {
            action: "unlock the lock file",
            source,
        });
        match (result, unlocked) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn read_state(&self) -> Result<State, TallyError> {
        let mut file = File::open(&self.path).map_err(|source| TallyError::File {
            action: "open the tally file",
            source,
        })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| TallyError::File {
                action: "read the tally file",
                source,
            })?;
        let text = String::from_utf8(bytes)
            .map_err(|_| TallyError::Corrupt("the file is not UTF-8".to_owned()))?;
        State::parse(&text)
    }

    fn persist(&self, state: &State) -> Result<(), TallyError> {
        let encoded = state.encode()?;
        let temporary = sibling_with_suffix(&self.path, ".tmp")?;
        let permissions = fs::metadata(&self.path)
            .map_err(|source| TallyError::File {
                action: "read the tally file permissions",
                source,
            })?
            .permissions();
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|source| TallyError::File {
                action: "open the temporary tally file",
                source,
            })?;
        file.set_permissions(permissions)
            .map_err(|source| TallyError::File {
                action: "set the temporary tally file permissions",
                source,
            })?;
        file.write_all(encoded.as_bytes())
            .map_err(|source| TallyError::File {
                action: "write the temporary tally file",
                source,
            })?;
        file.sync_all().map_err(|source| TallyError::File {
            action: "persist the temporary tally file",
            source,
        })?;
        fs::rename(&temporary, &self.path).map_err(|source| TallyError::File {
            action: "replace the tally file",
            source,
        })?;
        let directory_path = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(directory_path)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| TallyError::File {
                action: "persist the tally directory",
                source,
            })
    }
}

impl State {
    fn parse(text: &str) -> Result<Self, TallyError> {
        if text.is_empty() {
            return Ok(Self::default());
        }
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(TallyError::Corrupt(format!("missing header {HEADER:?}")));
        }
        let mut state = Self::default();
        for (index, line) in lines.enumerate() {
            let number = index + 2;
            let fields: Vec<_> = line.split('\t').collect();
            match fields.as_slice() {
                ["host", host, last_send, day, count, closed_until] => {
                    let closed_until = parse_optional_nanos(closed_until, number)?;
                    Self::insert_host(
                        &mut state,
                        host,
                        number,
                        HostState {
                            last_send: parse_optional_nanos(last_send, number)?,
                            day: parse_number(day, number, "UTC day")?,
                            count: parse_number(count, number, "daily count")?,
                            closed_until,
                            closed_reason: closed_until.map(|_| ClosureReason::RepeatedResponses),
                            ..HostState::default()
                        },
                    )?;
                }
                [
                    "host",
                    host,
                    last_send,
                    day,
                    count,
                    closed_until,
                    closed_reason,
                    paused_until,
                    refusals,
                    rate_limits,
                ] => {
                    let closed_until = parse_optional_nanos(closed_until, number)?;
                    let closed_reason = parse_optional_reason(closed_reason, number)?;
                    if closed_until.is_some() != closed_reason.is_some() {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} must carry a closure time and reason together"
                        )));
                    }
                    Self::insert_host(
                        &mut state,
                        host,
                        number,
                        HostState {
                            last_send: parse_optional_nanos(last_send, number)?,
                            day: parse_number(day, number, "UTC day")?,
                            count: parse_number(count, number, "daily count")?,
                            closed_until,
                            closed_reason,
                            paused_until: parse_optional_nanos(paused_until, number)?,
                            refusals: parse_timestamps(refusals, number, "refusal time")?,
                            rate_limits: parse_timestamps(rate_limits, number, "rate-limit time")?,
                        },
                    )?;
                }
                ["budget", host, budget_key, timestamps] => {
                    validate_atom(host, number)?;
                    validate_atom(budget_key, number)?;
                    let key = ((*host).to_owned(), (*budget_key).to_owned());
                    if state.sends.contains_key(&key) {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} repeats budget {budget_key:?} for {host:?}"
                        )));
                    }
                    state
                        .sends
                        .insert(key, parse_timestamps(timestamps, number, "send time")?);
                }
                _ => {
                    return Err(TallyError::Corrupt(format!(
                        "line {number} has an unknown record shape"
                    )));
                }
            }
        }
        Ok(state)
    }

    fn insert_host(
        state: &mut Self,
        host: &str,
        number: usize,
        host_state: HostState,
    ) -> Result<(), TallyError> {
        validate_atom(host, number)?;
        if state.hosts.insert(host.to_owned(), host_state).is_some() {
            return Err(TallyError::Corrupt(format!(
                "line {number} repeats host {host:?}"
            )));
        }
        Ok(())
    }

    fn prune(&mut self, now_nanos: u128) -> bool {
        let mut changed = false;
        let send_cutoff = now_nanos.saturating_sub(MINUTE_NANOS);
        self.sends.retain(|_, sent| {
            let previous = sent.len();
            sent.retain(|at| *at > send_cutoff);
            changed |= sent.len() != previous;
            let keep = !sent.is_empty();
            changed |= !keep;
            keep
        });
        let refusal_cutoff = now_nanos.saturating_sub(REFUSAL_WINDOW_NANOS);
        for state in self.hosts.values_mut() {
            let previous_refusals = state.refusals.len();
            state.refusals.retain(|at| *at > refusal_cutoff);
            changed |= state.refusals.len() != previous_refusals;
            let previous_rate_limits = state.rate_limits.len();
            state.rate_limits.retain(|at| *at > refusal_cutoff);
            changed |= state.rate_limits.len() != previous_rate_limits;
            if state.closed_until.is_some_and(|until| until <= now_nanos) {
                state.closed_until = None;
                state.closed_reason = None;
                changed = true;
            }
            if state.paused_until.is_some_and(|until| until <= now_nanos) {
                state.paused_until = None;
                changed = true;
            }
        }
        changed
    }

    fn encode(&self) -> Result<String, TallyError> {
        let mut text = String::from(HEADER);
        text.push('\n');
        for (host, state) in &self.hosts {
            validate_atom(host, 0)?;
            text.push_str("host\t");
            text.push_str(host);
            text.push('\t');
            text.push_str(&format_optional_nanos(state.last_send));
            text.push('\t');
            text.push_str(&state.day.to_string());
            text.push('\t');
            text.push_str(&state.count.to_string());
            text.push('\t');
            text.push_str(&format_optional_nanos(state.closed_until));
            text.push('\t');
            text.push_str(state.closed_reason.map_or("-", ClosureReason::token));
            text.push('\t');
            text.push_str(&format_optional_nanos(state.paused_until));
            text.push('\t');
            format_timestamps(&mut text, &state.refusals);
            text.push('\t');
            format_timestamps(&mut text, &state.rate_limits);
            text.push('\n');
        }
        for ((host, budget_key), sent) in &self.sends {
            validate_atom(host, 0)?;
            validate_atom(budget_key, 0)?;
            text.push_str("budget\t");
            text.push_str(host);
            text.push('\t');
            text.push_str(budget_key);
            text.push('\t');
            format_timestamps(&mut text, sent);
            text.push('\n');
        }
        Ok(text)
    }
}

fn unix_nanos(now: SystemTime) -> Result<u128, TallyError> {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .map_err(|_| TallyError::ClockBeforeEpoch)
}

fn duration_from_nanos(nanos: u128) -> Duration {
    let seconds = u64::try_from(nanos / SECOND_NANOS).unwrap_or(u64::MAX);
    let subsecond = if seconds == u64::MAX {
        999_999_999
    } else {
        u32::try_from(nanos % SECOND_NANOS).unwrap_or(999_999_999)
    };
    Duration::new(seconds, subsecond)
}

fn system_time_from_nanos(nanos: u128) -> Result<SystemTime, TallyError> {
    UNIX_EPOCH
        .checked_add(duration_from_nanos(nanos))
        .ok_or_else(|| {
            TallyError::Corrupt("a recorded time is outside the system clock".to_owned())
        })
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> Result<PathBuf, TallyError> {
    let file_name = path.file_name().ok_or_else(|| TallyError::File {
        action: "derive an adjacent tally path",
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the tally path has no file name",
        ),
    })?;
    let mut sibling = OsString::from(file_name);
    sibling.push(OsStr::new(suffix));
    Ok(path.with_file_name(sibling))
}

fn parse_optional_nanos(value: &str, line: usize) -> Result<Option<u128>, TallyError> {
    if value == "-" {
        Ok(None)
    } else {
        parse_number(value, line, "time").map(Some)
    }
}

fn parse_optional_reason(value: &str, line: usize) -> Result<Option<ClosureReason>, TallyError> {
    match value {
        "-" => Ok(None),
        "refusals" => Ok(Some(ClosureReason::Refusals)),
        "rate-limits" => Ok(Some(ClosureReason::RateLimits)),
        "repeated-responses" => Ok(Some(ClosureReason::RepeatedResponses)),
        _ => Err(TallyError::Corrupt(format!(
            "line {line} has invalid closure reason {value:?}"
        ))),
    }
}

fn parse_timestamps(value: &str, line: usize, field: &str) -> Result<Vec<u128>, TallyError> {
    let parsed = if value.is_empty() {
        Vec::new()
    } else {
        value
            .split(',')
            .map(|value| parse_number(value, line, field))
            .collect::<Result<Vec<_>, _>>()?
    };
    if parsed.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err(TallyError::Corrupt(format!(
            "line {line} has {field}s out of order"
        )));
    }
    Ok(parsed)
}

fn format_timestamps(text: &mut String, timestamps: &[u128]) {
    for (index, at) in timestamps.iter().enumerate() {
        if index != 0 {
            text.push(',');
        }
        text.push_str(&at.to_string());
    }
}

fn format_optional_nanos(value: Option<u128>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

fn parse_number<T>(value: &str, line: usize, field: &str) -> Result<T, TallyError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| TallyError::Corrupt(format!("line {line} has invalid {field} {value:?}")))
}

fn validate_atom(value: &str, line: usize) -> Result<(), TallyError> {
    if value.is_empty() || value.contains(['\t', '\n', '\r']) {
        let location = if line == 0 {
            "a value to write".to_owned()
        } else {
            format!("line {line}")
        };
        return Err(TallyError::Corrupt(format!(
            "{location} contains an invalid empty or control-delimited value"
        )));
    }
    Ok(())
}
