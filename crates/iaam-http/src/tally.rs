use fs2::FileExt;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::gateway::BootTime;

const HEADER_V1: &str = "iaam-outbound-tally-v1";
const HEADER: &str = "iaam-outbound-tally-v2";
const MINUTE_NANOS: u128 = 60_000_000_000;
const SECOND_NANOS: u128 = 1_000_000_000;
const DEPARTURE_SPACING_NANOS: u128 = SECOND_NANOS;
const REFUSAL_WINDOW_NANOS: u128 = 10 * MINUTE_NANOS;
const RATE_LIMIT_PAUSE_NANOS: u128 = MINUTE_NANOS;
const CLOSURE_NANOS: u128 = 30 * MINUTE_NANOS;
const DAY_NANOS: u128 = 24 * 60 * MINUTE_NANOS;
const FIRST_SEND_WAIT_NANOS: u128 = MINUTE_NANOS;
pub(crate) const TALLY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const DAILY_CEILING: u32 = 1_000;

#[derive(Debug, Error)]
pub(crate) enum TallyError {
    #[error("could not {action}: {source}")]
    File {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("the tally path is invalid: {0}")]
    InvalidPath(String),
    #[error("the file is corrupt: {0}")]
    Corrupt(String),
    #[error("could not read the boot clock: {0}")]
    Clock(String),
    #[error("endpoint {endpoint} is owned by another process")]
    EndpointOwned { endpoint: &'static str },
}

pub(crate) enum TallyDecision {
    Send,
    Wait(Duration),
    Paused {
        retry_after: Duration,
    },
    Closed {
        reason: ClosureReason,
        retry_after: Duration,
    },
    DailyCeiling {
        retry_after: Duration,
    },
}

pub(crate) enum TallyResponseDecision {
    Recorded,
    RateLimited { retry_after: Duration },
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
    daily_sends: Vec<u128>,
    closed_until: Option<u128>,
    closed_reason: Option<ClosureReason>,
    paused_until: Option<u128>,
    paused_for: Option<u128>,
    refusals: Vec<u128>,
    rate_limits: Vec<u128>,
    boot_wait_until: Option<u128>,
}

#[derive(Default)]
struct State {
    boot_id: Option<String>,
    hosts: BTreeMap<String, HostState>,
    sends: BTreeMap<(String, String), Vec<u128>>,
}

#[derive(Debug)]
pub(crate) struct OutboundTally {
    path: PathBuf,
}

/// The process's lifetime ownership of one broker endpoint.
///
/// The lock is acquired without waiting. It remains in this value beside the
/// validated tally path, so a process cannot decide under one path spelling and
/// send under another.
#[derive(Debug)]
pub(crate) struct EndpointOwner {
    tally: OutboundTally,
    _lock: File,
}

struct DecisionRequest<'a> {
    host: &'a str,
    budget_key: &'a str,
    budget_limit: u32,
    budget_window: Duration,
    boot: &'a BootTime,
}

impl EndpointOwner {
    pub(crate) fn acquire(
        path: &Path,
        endpoint: &'static str,
        lock_name: &str,
    ) -> Result<Self, TallyError> {
        let tally = OutboundTally::new(path)?;
        let lock_path = sibling_with_suffix(&tally.path, &format!(".{lock_name}.owner"))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| TallyError::File {
                action: "open the endpoint owner lock",
                source,
            })?;
        match FileExt::try_lock_exclusive(&lock) {
            Ok(()) => Ok(Self { tally, _lock: lock }),
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                Err(TallyError::EndpointOwned { endpoint })
            }
            Err(source) => Err(TallyError::File {
                action: "lock the endpoint owner lock",
                source,
            }),
        }
    }

    pub(crate) const fn tally(&self) -> &OutboundTally {
        &self.tally
    }
}

impl OutboundTally {
    pub(crate) fn validate(path: &Path) -> Result<(), TallyError> {
        validate_tally_path(path).map(|_| ())
    }

    fn new(path: &Path) -> Result<Self, TallyError> {
        Ok(Self {
            path: validate_tally_path(path)?,
        })
    }

    pub(crate) fn decide_and_record(
        &self,
        host: &str,
        budget_key: &str,
        budget_limit: u32,
        budget_window: Duration,
        boot: &BootTime,
    ) -> Result<TallyDecision, TallyError> {
        let request = DecisionRequest {
            host,
            budget_key,
            budget_limit,
            budget_window,
            boot,
        };
        self.transact(|state| Self::decide(state, &request))
    }

    pub(crate) fn record_response(
        &self,
        host: &str,
        status: u16,
        retry_after: Option<Duration>,
        boot: &BootTime,
    ) -> Result<TallyResponseDecision, TallyError> {
        let now_nanos = boot.elapsed().as_nanos();
        self.transact(|state| {
            let mut changed = state.prepare_boot(boot)?;
            changed |= state.prune(now_nanos);
            let host_state = state.host_for_boot(host, now_nanos);
            match status {
                429 => {
                    host_state.rate_limits.push(now_nanos);
                    let pause = retry_after
                        .map_or(RATE_LIMIT_PAUSE_NANOS, |delay| delay.as_nanos())
                        .clamp(RATE_LIMIT_PAUSE_NANOS, DAY_NANOS);
                    let paused_until = now_nanos.saturating_add(pause);
                    if host_state
                        .paused_until
                        .is_none_or(|until| paused_until >= until)
                    {
                        host_state.paused_until = Some(paused_until);
                        host_state.paused_for = Some(pause);
                    }
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
            boot,
        } = *request;
        let now_nanos = boot.elapsed().as_nanos();
        let mut changed = state.prepare_boot(boot)?;
        changed |= state.prune(now_nanos);

        state.host_for_boot(host, now_nanos);
        let host_state = state
            .hosts
            .get(host)
            .ok_or_else(|| TallyError::Corrupt("the endpoint state was not created".to_owned()))?;
        if let Some(decision) = Self::active_refusal(host_state, now_nanos) {
            return Ok((decision, true));
        }
        if let Some(until) = host_state
            .boot_wait_until
            .filter(|until| *until > now_nanos)
        {
            return Ok((
                TallyDecision::Wait(duration_from_nanos(until - now_nanos)),
                true,
            ));
        }
        if host_state.daily_sends.len() >= DAILY_CEILING as usize {
            let reset_nanos = host_state.daily_sends[0].saturating_add(DAY_NANOS);
            return Ok((
                TallyDecision::DailyCeiling {
                    retry_after: duration_from_nanos(reset_nanos.saturating_sub(now_nanos)),
                },
                changed,
            ));
        }

        let spacing_wait = host_state.last_send.map_or(0, |last| {
            last.saturating_add(DEPARTURE_SPACING_NANOS)
                .saturating_sub(now_nanos)
        });
        let key = (host.to_owned(), budget_key.to_owned());
        let sent = state.sends.entry(key).or_default();
        let budget_wait = if sent.len() >= budget_limit as usize {
            sent[sent.len() - budget_limit as usize]
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
        let host_state = state
            .hosts
            .get_mut(host)
            .ok_or_else(|| TallyError::Corrupt("the endpoint state disappeared".to_owned()))?;
        host_state.last_send = Some(now_nanos);
        host_state.daily_sends.push(now_nanos);
        host_state.boot_wait_until = None;
        Ok((TallyDecision::Send, true))
    }

    fn active_refusal(host_state: &HostState, now_nanos: u128) -> Option<TallyDecision> {
        if let Some(until) = host_state.closed_until.filter(|until| *until > now_nanos) {
            return Some(TallyDecision::Closed {
                reason: host_state
                    .closed_reason
                    .unwrap_or(ClosureReason::RepeatedResponses),
                retry_after: duration_from_nanos(until - now_nanos),
            });
        }
        host_state
            .paused_until
            .filter(|until| *until > now_nanos)
            .map(|until| TallyDecision::Paused {
                retry_after: duration_from_nanos(until - now_nanos),
            })
    }

    fn transact<R>(
        &self,
        update: impl FnOnce(&mut State) -> Result<(R, bool), TallyError>,
    ) -> Result<R, TallyError> {
        validate_tally_path(&self.path)?;
        let lock_path = sibling_with_suffix(&self.path, ".lock")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| TallyError::File {
                action: "open the tally lock file",
                source,
            })?;
        FileExt::lock_exclusive(&lock).map_err(|source| TallyError::File {
            action: "lock the tally lock file",
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
            action: "unlock the tally lock file",
            source,
        });
        match (result, unlocked) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn read_state(&self) -> Result<State, TallyError> {
        validate_tally_path(&self.path)?;
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
        let header = lines.next();
        if header != Some(HEADER) && header != Some(HEADER_V1) {
            return Err(TallyError::Corrupt(format!("missing header {HEADER:?}")));
        }
        let mut state = Self::default();
        let mut first_record = true;
        for (index, line) in lines.enumerate() {
            let number = index + 2;
            let fields: Vec<_> = line.split('\t').collect();
            match fields.as_slice() {
                ["boot", boot_id] if first_record && header == Some(HEADER) => {
                    validate_atom(boot_id, number)?;
                    state.boot_id = Some((*boot_id).to_owned());
                }
                ["host", host, last_send, day, count, closed_until]
                    if header == Some(HEADER_V1) =>
                {
                    let closed_until = parse_optional_nanos(closed_until, number)?;
                    let count = parse_number::<usize>(count, number, "daily count")?;
                    let _ = parse_number::<u64>(day, number, "UTC day")?;
                    Self::insert_host(
                        &mut state,
                        host,
                        number,
                        HostState {
                            last_send: parse_optional_nanos(last_send, number)?,
                            daily_sends: vec![0; count],
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
                ] if header == Some(HEADER_V1) => {
                    let closed_until = parse_optional_nanos(closed_until, number)?;
                    let closed_reason = parse_optional_reason(closed_reason, number)?;
                    if closed_until.is_some() != closed_reason.is_some() {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} must carry a closure time and reason together"
                        )));
                    }
                    let count = parse_number::<usize>(count, number, "daily count")?;
                    let _ = parse_number::<u64>(day, number, "UTC day")?;
                    let paused_until = parse_optional_nanos(paused_until, number)?;
                    Self::insert_host(
                        &mut state,
                        host,
                        number,
                        HostState {
                            last_send: parse_optional_nanos(last_send, number)?,
                            daily_sends: vec![0; count],
                            closed_until,
                            closed_reason,
                            paused_until,
                            paused_for: paused_until.map(|_| RATE_LIMIT_PAUSE_NANOS),
                            refusals: parse_timestamps(refusals, number, "refusal time")?,
                            rate_limits: parse_timestamps(rate_limits, number, "rate-limit time")?,
                            boot_wait_until: None,
                        },
                    )?;
                }
                [
                    "host",
                    host,
                    last_send,
                    daily_sends,
                    closed_until,
                    closed_reason,
                    paused_until,
                    paused_for,
                    refusals,
                    rate_limits,
                    boot_wait_until,
                ] if header == Some(HEADER) => {
                    let closed_until = parse_optional_nanos(closed_until, number)?;
                    let closed_reason = parse_optional_reason(closed_reason, number)?;
                    let paused_until = parse_optional_nanos(paused_until, number)?;
                    let paused_for = parse_optional_nanos(paused_for, number)?;
                    if closed_until.is_some() != closed_reason.is_some() {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} must carry a closure time and reason together"
                        )));
                    }
                    if paused_until.is_some() != paused_for.is_some() {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} must carry a pause time and duration together"
                        )));
                    }
                    Self::insert_host(
                        &mut state,
                        host,
                        number,
                        HostState {
                            last_send: parse_optional_nanos(last_send, number)?,
                            daily_sends: parse_timestamps(daily_sends, number, "daily send time")?,
                            closed_until,
                            closed_reason,
                            paused_until,
                            paused_for,
                            refusals: parse_timestamps(refusals, number, "refusal time")?,
                            rate_limits: parse_timestamps(rate_limits, number, "rate-limit time")?,
                            boot_wait_until: parse_optional_nanos(boot_wait_until, number)?,
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
            first_record = false;
        }
        if header == Some(HEADER) && state.boot_id.is_none() {
            return Err(TallyError::Corrupt(
                "the v2 tally has no boot id".to_owned(),
            ));
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

    fn prepare_boot(&mut self, boot: &BootTime) -> Result<bool, TallyError> {
        validate_atom(boot.id(), 0)?;
        if self.boot_id.as_deref() == Some(boot.id()) {
            return Ok(false);
        }
        let now = boot.elapsed().as_nanos();
        self.boot_id = Some(boot.id().to_owned());
        for host in self.hosts.values_mut() {
            host.last_send = host.last_send.map(|_| now);
            host.daily_sends.fill(now);
            host.closed_until = host.closed_until.map(|_| now.saturating_add(CLOSURE_NANOS));
            host.paused_until = host
                .paused_until
                .map(|_| now.saturating_add(host.paused_for.unwrap_or(RATE_LIMIT_PAUSE_NANOS)));
            host.refusals.fill(now);
            host.rate_limits.fill(now);
            host.boot_wait_until = Some(now.saturating_add(FIRST_SEND_WAIT_NANOS));
        }
        for sent in self.sends.values_mut() {
            sent.fill(now);
        }
        Ok(true)
    }

    fn host_for_boot(&mut self, host: &str, now_nanos: u128) -> &mut HostState {
        self.hosts
            .entry(host.to_owned())
            .or_insert_with(|| HostState {
                boot_wait_until: Some(now_nanos.saturating_add(FIRST_SEND_WAIT_NANOS)),
                ..HostState::default()
            })
    }

    fn prune(&mut self, now_nanos: u128) -> bool {
        let mut changed = false;
        let send_cutoff = now_nanos.checked_sub(TALLY_RETENTION.as_nanos());
        if let Some(send_cutoff) = send_cutoff {
            self.sends.retain(|_, sent| {
                let previous = sent.len();
                sent.retain(|at| *at > send_cutoff);
                changed |= sent.len() != previous;
                let keep = !sent.is_empty();
                changed |= !keep;
                keep
            });
        }
        let refusal_cutoff = now_nanos.checked_sub(REFUSAL_WINDOW_NANOS);
        for state in self.hosts.values_mut() {
            if let Some(send_cutoff) = send_cutoff {
                let previous_daily = state.daily_sends.len();
                state.daily_sends.retain(|at| *at > send_cutoff);
                changed |= state.daily_sends.len() != previous_daily;
            }
            if let Some(refusal_cutoff) = refusal_cutoff {
                let previous_refusals = state.refusals.len();
                state.refusals.retain(|at| *at > refusal_cutoff);
                changed |= state.refusals.len() != previous_refusals;
                let previous_rate_limits = state.rate_limits.len();
                state.rate_limits.retain(|at| *at > refusal_cutoff);
                changed |= state.rate_limits.len() != previous_rate_limits;
            }
            if state.closed_until.is_some_and(|until| until <= now_nanos) {
                state.closed_until = None;
                state.closed_reason = None;
                changed = true;
            }
            if state.paused_until.is_some_and(|until| until <= now_nanos) {
                state.paused_until = None;
                state.paused_for = None;
                changed = true;
            }
            if state
                .boot_wait_until
                .is_some_and(|until| until <= now_nanos)
            {
                state.boot_wait_until = None;
                changed = true;
            }
        }
        changed
    }

    fn encode(&self) -> Result<String, TallyError> {
        let boot_id = self
            .boot_id
            .as_deref()
            .ok_or_else(|| TallyError::Corrupt("the tally has no boot id".to_owned()))?;
        validate_atom(boot_id, 0)?;
        let mut text = String::from(HEADER);
        text.push('\n');
        text.push_str("boot\t");
        text.push_str(boot_id);
        text.push('\n');
        for (host, state) in &self.hosts {
            validate_atom(host, 0)?;
            text.push_str("host\t");
            text.push_str(host);
            text.push('\t');
            text.push_str(&format_optional_nanos(state.last_send));
            text.push('\t');
            format_timestamps(&mut text, &state.daily_sends);
            text.push('\t');
            text.push_str(&format_optional_nanos(state.closed_until));
            text.push('\t');
            text.push_str(state.closed_reason.map_or("-", ClosureReason::token));
            text.push('\t');
            text.push_str(&format_optional_nanos(state.paused_until));
            text.push('\t');
            text.push_str(&format_optional_nanos(state.paused_for));
            text.push('\t');
            format_timestamps(&mut text, &state.refusals);
            text.push('\t');
            format_timestamps(&mut text, &state.rate_limits);
            text.push('\t');
            text.push_str(&format_optional_nanos(state.boot_wait_until));
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

fn validate_tally_path(path: &Path) -> Result<PathBuf, TallyError> {
    if !path.is_absolute() {
        return Err(TallyError::InvalidPath(
            "use an absolute canonical path".to_owned(),
        ));
    }
    let link_metadata = fs::symlink_metadata(path).map_err(|source| TallyError::File {
        action: "inspect the tally path",
        source,
    })?;
    if link_metadata.file_type().is_symlink() {
        return Err(TallyError::InvalidPath(
            "the tally file must not be a symbolic link".to_owned(),
        ));
    }
    if !link_metadata.is_file() {
        return Err(TallyError::InvalidPath(
            "the tally path must name an ordinary file".to_owned(),
        ));
    }
    #[cfg(unix)]
    if link_metadata.nlink() != 1 {
        return Err(TallyError::InvalidPath(
            "the tally file must have exactly one hard link".to_owned(),
        ));
    }
    let canonical = fs::canonicalize(path).map_err(|source| TallyError::File {
        action: "canonicalize the tally path",
        source,
    })?;
    if canonical != path {
        return Err(TallyError::InvalidPath(format!(
            "use its canonical spelling {}",
            canonical.display()
        )));
    }
    Ok(canonical)
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
