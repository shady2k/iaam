use fs2::FileExt;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;

const HEADER: &str = "iaam-outbound-tally-v1";
const MINUTE_NANOS: u128 = 60_000_000_000;
const SECOND_NANOS: u128 = 1_000_000_000;
const DAY_NANOS: u128 = 86_400_000_000_000;
pub(crate) const DAILY_CEILING: u32 = 1_000;

#[derive(Debug, Error)]
pub(crate) enum TallyError {
    #[error("could not {action} the file: {source}")]
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
    DailyCeiling {
        reset_at: SystemTime,
        retry_after: Duration,
    },
}

#[derive(Default)]
struct HostState {
    last_send: Option<u128>,
    day: u64,
    count: u32,
    /// Reserved in version 1 for task .1.2's per-host closure. Reading and
    /// writing it now lets that task add behavior without changing the file.
    closed_until: Option<u128>,
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
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|source| TallyError::File {
                action: "open",
                source,
            })?;
        FileExt::lock_exclusive(&file).map_err(|source| TallyError::File {
            action: "lock",
            source,
        })?;

        let decision = self.decide_locked(&mut file, &request);
        let unlocked = FileExt::unlock(&file).map_err(|source| TallyError::File {
            action: "unlock",
            source,
        });
        match (decision, unlocked) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(decision), Ok(())) => Ok(decision),
        }
    }

    fn decide_locked(
        &self,
        file: &mut File,
        request: &DecisionRequest<'_>,
    ) -> Result<TallyDecision, TallyError> {
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
        file.seek(SeekFrom::Start(0))
            .map_err(|source| TallyError::File {
                action: "seek in",
                source,
            })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| TallyError::File {
                action: "read",
                source,
            })?;
        let text = String::from_utf8(bytes)
            .map_err(|_| TallyError::Corrupt("the file is not UTF-8".to_owned()))?;
        let mut state = State::parse(&text)?;
        state.prune_sends(now_nanos);

        let host_state = state.hosts.entry(host.to_owned()).or_default();
        if host_state.day != day {
            host_state.day = day;
            host_state.count = 0;
        }
        if host_state.count >= DAILY_CEILING {
            let reset_nanos = u128::from(day + 1) * DAY_NANOS;
            return Ok(TallyDecision::DailyCeiling {
                reset_at: UNIX_EPOCH + duration_from_nanos(reset_nanos),
                retry_after: duration_from_nanos(reset_nanos.saturating_sub(now_nanos)),
            });
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
            return Ok(TallyDecision::Wait(duration_from_nanos(wait)));
        }

        sent.push(now_nanos);
        host_state.last_send = Some(now_nanos);
        host_state.count += 1;
        let encoded = state.encode()?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| TallyError::File {
                action: "seek in",
                source,
            })?;
        file.set_len(0).map_err(|source| TallyError::File {
            action: "truncate",
            source,
        })?;
        file.write_all(encoded.as_bytes())
            .map_err(|source| TallyError::File {
                action: "write",
                source,
            })?;
        file.sync_data().map_err(|source| TallyError::File {
            action: "persist",
            source,
        })?;
        Ok(TallyDecision::Send)
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
                    validate_atom(host, number)?;
                    if state.hosts.contains_key(*host) {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} repeats host {host:?}"
                        )));
                    }
                    state.hosts.insert(
                        (*host).to_owned(),
                        HostState {
                            last_send: parse_optional_nanos(last_send, number)?,
                            day: parse_number(day, number, "UTC day")?,
                            count: parse_number(count, number, "daily count")?,
                            closed_until: parse_optional_nanos(closed_until, number)?,
                        },
                    );
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
                    let parsed = if timestamps.is_empty() {
                        Vec::new()
                    } else {
                        timestamps
                            .split(',')
                            .map(|value| parse_number(value, number, "send time"))
                            .collect::<Result<Vec<_>, _>>()?
                    };
                    if parsed.windows(2).any(|pair| pair[0] > pair[1]) {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} has send times out of order"
                        )));
                    }
                    state.sends.insert(key, parsed);
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

    fn prune_sends(&mut self, now_nanos: u128) {
        let cutoff = now_nanos.saturating_sub(MINUTE_NANOS);
        self.sends.retain(|_, sent| {
            sent.retain(|at| *at > cutoff);
            !sent.is_empty()
        });
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
            for (index, at) in sent.iter().enumerate() {
                if index != 0 {
                    text.push(',');
                }
                text.push_str(&at.to_string());
            }
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

fn parse_optional_nanos(value: &str, line: usize) -> Result<Option<u128>, TallyError> {
    if value == "-" {
        Ok(None)
    } else {
        parse_number(value, line, "time").map(Some)
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
