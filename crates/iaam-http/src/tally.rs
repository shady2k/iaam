use fs2::FileExt;
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, fstat, fsync, open, openat, renameat, statat,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::gateway::BootTime;

const HEADER_V1: &str = "iaam-outbound-tally-v1";
const HEADER_V2: &str = "iaam-outbound-tally-v2";
const HEADER: &str = "iaam-outbound-tally-v3";
pub(crate) const TALLY_FILE: &str = "outbound-tally";
const TALLY_LOCK_FILE: &str = "outbound-tally.lock";
const TALLY_TEMP_FILE: &str = "outbound-tally.tmp";
const MINUTE_NANOS: u128 = 60_000_000_000;
const SECOND_NANOS: u128 = 1_000_000_000;
const DEPARTURE_SPACING_NANOS: u128 = SECOND_NANOS;
const REFUSAL_WINDOW_NANOS: u128 = 10 * MINUTE_NANOS;
const RATE_LIMIT_PAUSE_NANOS: u128 = MINUTE_NANOS;
const CLOSURE_NANOS: u128 = 30 * MINUTE_NANOS;
const DAY_NANOS: u128 = 24 * 60 * MINUTE_NANOS;
const FIRST_SEND_WAIT_NANOS: u128 = MINUTE_NANOS;
const UNRESOLVED_ATTEMPT_NANOS: u128 = 90 * SECOND_NANOS;
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
    #[error("the egress directory is invalid: {0}")]
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
    Paused { retry_after: Duration },
}

#[derive(Clone, Copy)]
pub(crate) enum ClosureReason {
    Refusals,
    RateLimits,
    RepeatedResponses,
    UnresolvedAttempt,
}

impl ClosureReason {
    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Refusals => "three refusals within ten minutes",
            Self::RateLimits => "two 429 responses within ten minutes",
            Self::RepeatedResponses => "repeated broker refusals",
            Self::UnresolvedAttempt => "an earlier request has no committed status",
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::Refusals => "refusals",
            Self::RateLimits => "rate-limits",
            Self::RepeatedResponses => "repeated-responses",
            Self::UnresolvedAttempt => "unresolved-attempt",
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
    pending_since: Option<u128>,
}

#[derive(Default)]
struct State {
    boot_id: Option<String>,
    boot_high_water: Option<u128>,
    hosts: BTreeMap<String, HostState>,
    sends: BTreeMap<(String, String), Vec<u128>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn of(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        }
    }
}

#[derive(Debug)]
struct EgressDirectoryInner {
    path: PathBuf,
    descriptor: OwnedFd,
    identity: FileIdentity,
}

/// A retained descriptor for the one directory that owns the tally and every
/// endpoint lock. Every child is opened relative to this descriptor with
/// `O_NOFOLLOW`, then its inode is compared with the directory entry.
#[derive(Clone, Debug)]
pub(crate) struct EgressDirectory(Arc<EgressDirectoryInner>);

impl EgressDirectory {
    pub(crate) fn open(path: &Path) -> Result<Self, TallyError> {
        if !path.is_absolute() {
            return Err(TallyError::InvalidPath(
                "use an absolute path to the egress directory".to_owned(),
            ));
        }
        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|source| file_error("open the egress directory", source))?;
        let opened = fstat(&descriptor)
            .map_err(|source| file_error("inspect the open egress directory", source))?;
        let named = statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| file_error("inspect the named egress directory", source))?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::Directory {
            return Err(TallyError::InvalidPath(
                "the egress path must name a directory".to_owned(),
            ));
        }
        let identity = FileIdentity::of(&opened);
        if identity != FileIdentity::of(&named) {
            return Err(TallyError::InvalidPath(
                "the egress directory changed while it was opened".to_owned(),
            ));
        }
        Ok(Self(Arc::new(EgressDirectoryInner {
            path: path.to_owned(),
            descriptor,
            identity,
        })))
    }

    pub(crate) fn tally_path(&self) -> PathBuf {
        self.0.path.join(TALLY_FILE)
    }

    fn verify(&self) -> Result<(), TallyError> {
        let named = statat(CWD, &self.0.path, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| file_error("verify the egress directory", source))?;
        if self.0.identity != FileIdentity::of(&named) {
            return Err(TallyError::InvalidPath(
                "the egress directory was replaced; restore the mounted directory before retrying"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn open_child(
        &self,
        name: &str,
        flags: OFlags,
        action: &'static str,
    ) -> Result<(File, FileIdentity), TallyError> {
        self.verify()?;
        let descriptor = openat(
            &self.0.descriptor,
            name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|source| file_error(action, source))?;
        let opened = fstat(&descriptor)
            .map_err(|source| file_error("inspect an open egress file", source))?;
        let named = statat(&self.0.descriptor, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| file_error("inspect a named egress file", source))?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
            return Err(TallyError::InvalidPath(format!(
                "{} must be an ordinary file",
                self.0.path.join(name).display()
            )));
        }
        if opened.st_nlink != 1 {
            return Err(TallyError::InvalidPath(format!(
                "{} must have exactly one hard link",
                self.0.path.join(name).display()
            )));
        }
        let identity = FileIdentity::of(&opened);
        if identity != FileIdentity::of(&named) {
            return Err(TallyError::InvalidPath(format!(
                "{} changed while it was opened",
                self.0.path.join(name).display()
            )));
        }
        Ok((File::from(descriptor), identity))
    }

    fn verify_child(&self, name: &str, identity: FileIdentity) -> Result<(), TallyError> {
        self.verify()?;
        let named = statat(&self.0.descriptor, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| file_error("verify an egress file", source))?;
        if identity != FileIdentity::of(&named) {
            return Err(TallyError::InvalidPath(format!(
                "{} was replaced while it was in use",
                self.0.path.join(name).display()
            )));
        }
        Ok(())
    }
}

fn file_error(action: &'static str, source: rustix::io::Errno) -> TallyError {
    TallyError::File {
        action,
        source: std::io::Error::from_raw_os_error(source.raw_os_error()),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OutboundTally {
    directory: EgressDirectory,
}

/// The process's lifetime ownership of one broker endpoint.
///
/// The PID and lock inode are checked before every decision. A forked child
/// cannot use the inherited file description as its own ownership: it drops
/// that copy and must acquire a new lock, which the living parent still holds.
#[derive(Debug)]
pub(crate) struct EndpointOwner {
    tally: OutboundTally,
    lock_name: String,
    lock_identity: FileIdentity,
    lock: File,
    pid: u32,
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
        directory: EgressDirectory,
        endpoint: &'static str,
        lock_stem: &str,
    ) -> Result<Self, TallyError> {
        let tally = OutboundTally::new(directory.clone())?;
        let lock_name = format!("{lock_stem}.owner");
        let (mut lock, lock_identity) = directory.open_child(
            &lock_name,
            OFlags::RDWR | OFlags::CREATE,
            "open the endpoint owner lock",
        )?;
        match FileExt::try_lock_exclusive(&lock) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(TallyError::EndpointOwned { endpoint });
            }
            Err(source) => {
                return Err(TallyError::File {
                    action: "lock the endpoint owner lock",
                    source,
                });
            }
        }
        let pid = std::process::id();
        lock.set_len(0).map_err(|source| TallyError::File {
            action: "truncate the endpoint owner record",
            source,
        })?;
        lock.seek(SeekFrom::Start(0))
            .and_then(|_| writeln!(lock, "{pid}"))
            .and_then(|_| lock.sync_all())
            .map_err(|source| TallyError::File {
                action: "persist the endpoint owner PID",
                source,
            })?;
        Ok(Self {
            tally,
            lock_name,
            lock_identity,
            lock,
            pid,
        })
    }

    pub(crate) fn belongs_to_current_process(&self) -> bool {
        self.pid == std::process::id()
    }

    pub(crate) fn tally(&self) -> Result<OutboundTally, TallyError> {
        self.tally
            .directory
            .verify_child(&self.lock_name, self.lock_identity)?;
        let opened = fstat(self.lock.as_fd())
            .map_err(|source| file_error("inspect the endpoint owner lock", source))?;
        if FileIdentity::of(&opened) != self.lock_identity {
            return Err(TallyError::InvalidPath(
                "the held endpoint owner lock changed identity".to_owned(),
            ));
        }
        Ok(self.tally.clone())
    }
}

impl OutboundTally {
    pub(crate) fn validate(directory: &EgressDirectory) -> Result<(), TallyError> {
        let _ = Self::new(directory.clone())?;
        Ok(())
    }

    fn new(directory: EgressDirectory) -> Result<Self, TallyError> {
        let (file, _) =
            directory.open_child(TALLY_FILE, OFlags::RDONLY, "open the outbound tally")?;
        drop(file);
        Ok(Self { directory })
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
            state.prepare_boot(boot)?;
            state.prune(now_nanos);
            let host_state = state.host_for_boot(host, now_nanos);
            host_state.pending_since = None;
            host_state.last_send = Some(now_nanos);
            let named_pause = retry_after.map(|delay| delay.as_nanos().min(DAY_NANOS));
            if let Some(pause) = named_pause {
                let paused_until = now_nanos.saturating_add(pause);
                if host_state
                    .paused_until
                    .is_none_or(|until| paused_until >= until)
                {
                    host_state.paused_until = Some(paused_until);
                    host_state.paused_for = Some(pause);
                }
            }
            match status {
                429 => {
                    host_state.rate_limits.push(now_nanos);
                    let pause = named_pause
                        .unwrap_or(RATE_LIMIT_PAUSE_NANOS)
                        .max(RATE_LIMIT_PAUSE_NANOS);
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
                }
                _ => {}
            }
            let response_blocked_until = match (
                host_state.paused_until,
                host_state
                    .closed_until
                    .filter(|_| host_state.paused_until.is_some()),
            ) {
                (Some(pause), Some(closure)) => Some(pause.max(closure)),
                (pause, None) => pause,
                (None, Some(_)) => None,
            };
            let decision =
                response_blocked_until.map_or(TallyResponseDecision::Recorded, |until| {
                    TallyResponseDecision::Paused {
                        retry_after: duration_from_nanos(until.saturating_sub(now_nanos)),
                    }
                });
            Ok((decision, true))
        })
    }

    pub(crate) fn record_unknown_outcome(
        &self,
        host: &str,
        boot: &BootTime,
    ) -> Result<Duration, TallyError> {
        let now_nanos = boot.elapsed().as_nanos();
        self.transact(|state| {
            state.prepare_boot(boot)?;
            state.prune(now_nanos);
            let host_state = state.host_for_boot(host, now_nanos);
            let pending_since = host_state.pending_since.take().unwrap_or(now_nanos);
            let closed_until = pending_since
                .saturating_add(UNRESOLVED_ATTEMPT_NANOS)
                .max(now_nanos.saturating_add(RATE_LIMIT_PAUSE_NANOS));
            host_state.closed_until = Some(
                host_state
                    .closed_until
                    .unwrap_or_default()
                    .max(closed_until),
            );
            host_state.closed_reason = Some(ClosureReason::UnresolvedAttempt);
            Ok((
                duration_from_nanos(closed_until.saturating_sub(now_nanos)),
                true,
            ))
        })
    }
    pub(crate) fn record_not_handed_off(
        &self,
        host: &str,
        budget_key: &str,
        boot: &BootTime,
    ) -> Result<(), TallyError> {
        let now_nanos = boot.elapsed().as_nanos();
        self.transact(|state| {
            state.prepare_boot(boot)?;
            state.prune(now_nanos);
            let pending_since = {
                let host_state = state.host_for_boot(host, now_nanos);
                let pending_since = host_state.pending_since.take();
                if pending_since.is_some()
                    && host_state.daily_sends.last() == pending_since.as_ref()
                {
                    host_state.daily_sends.pop();
                }
                pending_since
            };
            if let Some(pending_since) = pending_since
                && let Some(sends) = state
                    .sends
                    .get_mut(&(host.to_owned(), budget_key.to_owned()))
                && sends.last() == Some(&pending_since)
            {
                sends.pop();
            }
            Ok(((), true))
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
            .get_mut(host)
            .ok_or_else(|| TallyError::Corrupt("the endpoint state was not created".to_owned()))?;
        if let Some(pending_since) = host_state.pending_since {
            let until = pending_since.saturating_add(UNRESOLVED_ATTEMPT_NANOS);
            if until > now_nanos {
                return Ok((
                    TallyDecision::Closed {
                        reason: ClosureReason::UnresolvedAttempt,
                        retry_after: duration_from_nanos(until - now_nanos),
                    },
                    changed,
                ));
            }
            host_state.pending_since = None;
            changed = true;
        }
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
        host_state.daily_sends.push(now_nanos);
        host_state.boot_wait_until = None;
        host_state.pending_since = Some(now_nanos);
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
        let (lock, lock_identity) = self.directory.open_child(
            TALLY_LOCK_FILE,
            OFlags::RDWR | OFlags::CREATE,
            "open the tally lock file",
        )?;
        FileExt::lock_exclusive(&lock).map_err(|source| TallyError::File {
            action: "lock the tally lock file",
            source,
        })?;
        self.directory
            .verify_child(TALLY_LOCK_FILE, lock_identity)?;

        let result = (|| {
            let (mut state, tally_identity) = self.read_state()?;
            let (value, changed) = update(&mut state)?;
            if changed {
                self.persist(&state, tally_identity)?;
            } else {
                self.directory.verify_child(TALLY_FILE, tally_identity)?;
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

    fn read_state(&self) -> Result<(State, FileIdentity), TallyError> {
        let (mut file, identity) =
            self.directory
                .open_child(TALLY_FILE, OFlags::RDONLY, "open the tally file")?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| TallyError::File {
                action: "read the tally file",
                source,
            })?;
        self.directory.verify_child(TALLY_FILE, identity)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| TallyError::Corrupt("the file is not UTF-8".to_owned()))?;
        Ok((State::parse(&text)?, identity))
    }

    fn persist(&self, state: &State, expected: FileIdentity) -> Result<(), TallyError> {
        self.directory.verify_child(TALLY_FILE, expected)?;
        let encoded = state.encode()?;
        let (mut file, temporary_identity) = self.directory.open_child(
            TALLY_TEMP_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
            "open the temporary tally file",
        )?;
        file.write_all(encoded.as_bytes())
            .map_err(|source| TallyError::File {
                action: "write the temporary tally file",
                source,
            })?;
        file.sync_all().map_err(|source| TallyError::File {
            action: "persist the temporary tally file",
            source,
        })?;
        self.directory
            .verify_child(TALLY_TEMP_FILE, temporary_identity)?;
        self.directory.verify_child(TALLY_FILE, expected)?;
        renameat(
            &self.directory.0.descriptor,
            TALLY_TEMP_FILE,
            &self.directory.0.descriptor,
            TALLY_FILE,
        )
        .map_err(|source| file_error("replace the tally file", source))?;
        fsync(&self.directory.0.descriptor)
            .map_err(|source| file_error("persist the tally directory", source))
    }
}

impl State {
    fn parse(text: &str) -> Result<Self, TallyError> {
        if text.is_empty() {
            return Ok(Self::default());
        }
        let mut lines = text.lines();
        let header = lines.next();
        if header == Some(HEADER_V1) {
            return Err(TallyError::Corrupt(
                "v1 cannot prove the rolling 24-hour history; remove it and recreate the outbound tally"
                    .to_owned(),
            ));
        }
        if header != Some(HEADER) && header != Some(HEADER_V2) {
            return Err(TallyError::Corrupt(format!("missing header {HEADER:?}")));
        }
        let mut state = Self::default();
        for (index, line) in lines.enumerate() {
            let number = index + 2;
            let fields: Vec<_> = line.split('\t').collect();
            match fields.as_slice() {
                ["boot", boot_id] if state.boot_id.is_none() => {
                    validate_atom(boot_id, number)?;
                    state.boot_id = Some((*boot_id).to_owned());
                }
                ["high-water", high_water] if header == Some(HEADER) => {
                    if state.boot_high_water.is_some() {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} repeats the boot high-water mark"
                        )));
                    }
                    state.boot_high_water =
                        Some(parse_number(high_water, number, "boot high-water mark")?);
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
                ] if header == Some(HEADER_V2) => {
                    let closed_until = parse_optional_nanos(closed_until, number)?;
                    let closed_reason = parse_optional_reason(closed_reason, number)?;
                    let paused_until = parse_optional_nanos(paused_until, number)?;
                    let paused_for = parse_optional_nanos(paused_for, number)?;
                    validate_optional_pairs(
                        number,
                        closed_until,
                        closed_reason,
                        paused_until,
                        paused_for,
                    )?;
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
                            pending_since: None,
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
                    pending_since,
                ] if header == Some(HEADER) => {
                    let closed_until = parse_optional_nanos(closed_until, number)?;
                    let closed_reason = parse_optional_reason(closed_reason, number)?;
                    let paused_until = parse_optional_nanos(paused_until, number)?;
                    let paused_for = parse_optional_nanos(paused_for, number)?;
                    validate_optional_pairs(
                        number,
                        closed_until,
                        closed_reason,
                        paused_until,
                        paused_for,
                    )?;
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
                            pending_since: parse_optional_nanos(pending_since, number)?,
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
        if state.boot_id.is_none() {
            return Err(TallyError::Corrupt("the tally has no boot id".to_owned()));
        }
        if header == Some(HEADER) && state.boot_high_water.is_none() {
            return Err(TallyError::Corrupt(
                "the v3 tally has no boot high-water mark".to_owned(),
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
        let now = boot.elapsed().as_nanos();
        let changed_boot = self.boot_id.as_deref() != Some(boot.id())
            || self
                .boot_high_water
                .is_some_and(|high_water| now < high_water);
        if !changed_boot {
            let changed = self.boot_high_water != Some(now);
            self.boot_high_water = Some(now);
            return Ok(changed);
        }
        self.boot_id = Some(boot.id().to_owned());
        self.boot_high_water = Some(now);
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
            host.pending_since = host.pending_since.map(|_| now);
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
        let high_water = self.boot_high_water.ok_or_else(|| {
            TallyError::Corrupt("the tally has no boot high-water mark".to_owned())
        })?;
        validate_atom(boot_id, 0)?;
        let mut text = String::from(HEADER);
        text.push('\n');
        text.push_str("boot\t");
        text.push_str(boot_id);
        text.push('\n');
        text.push_str("high-water\t");
        text.push_str(&high_water.to_string());
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
            text.push('\t');
            text.push_str(&format_optional_nanos(state.pending_since));
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

fn parse_optional_reason(value: &str, line: usize) -> Result<Option<ClosureReason>, TallyError> {
    match value {
        "-" => Ok(None),
        "refusals" => Ok(Some(ClosureReason::Refusals)),
        "rate-limits" => Ok(Some(ClosureReason::RateLimits)),
        "repeated-responses" => Ok(Some(ClosureReason::RepeatedResponses)),
        "unresolved-attempt" => Ok(Some(ClosureReason::UnresolvedAttempt)),
        _ => Err(TallyError::Corrupt(format!(
            "line {line} has invalid closure reason {value:?}"
        ))),
    }
}

fn validate_optional_pairs(
    line: usize,
    closed_until: Option<u128>,
    closed_reason: Option<ClosureReason>,
    paused_until: Option<u128>,
    paused_for: Option<u128>,
) -> Result<(), TallyError> {
    if closed_until.is_some() != closed_reason.is_some() {
        return Err(TallyError::Corrupt(format!(
            "line {line} must carry a closure time and reason together"
        )));
    }
    if paused_until.is_some() != paused_for.is_some() {
        return Err(TallyError::Corrupt(format!(
            "line {line} must carry a pause time and duration together"
        )));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "iaam-tally-unit-{label}-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path)
                .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
            std::fs::write(path.join(TALLY_FILE), "")
                .unwrap_or_else(|error| panic!("create tally: {error}"));
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tally(label: &str) -> (TempDir, EgressDirectory, OutboundTally) {
        let temporary = TempDir::new(label);
        let directory = EgressDirectory::open(&temporary.0)
            .unwrap_or_else(|error| panic!("open egress directory: {error}"));
        let tally = OutboundTally::new(directory.clone())
            .unwrap_or_else(|error| panic!("open tally: {error}"));
        (temporary, directory, tally)
    }

    fn ready_send(tally: &OutboundTally, host: &str, at: Duration) -> BootTime {
        let first = BootTime::new("test-boot", at);
        assert!(matches!(
            tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &first),
            Ok(TallyDecision::Wait(wait)) if wait == Duration::from_secs(60)
        ));
        let ready = BootTime::new("test-boot", at + Duration::from_secs(60));
        assert!(matches!(
            tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &ready),
            Ok(TallyDecision::Send)
        ));
        ready
    }

    #[test]
    fn a_shorter_overlapping_retry_after_never_shortens_the_longer_pause() {
        let (_temporary, _directory, tally) = tally("overlapping-pause");
        let host = "https://broker.invalid";
        let sent = ready_send(&tally, host, Duration::from_secs(10_000));
        assert!(matches!(
            tally.record_response(
                host,
                503,
                Some(Duration::from_secs(120)),
                &sent
            ),
            Ok(TallyResponseDecision::Paused { retry_after })
                if retry_after == Duration::from_secs(120)
        ));

        let later = BootTime::new("test-boot", sent.elapsed() + Duration::from_secs(10));
        assert!(matches!(
            tally.record_response(
                host,
                503,
                Some(Duration::from_secs(30)),
                &later
            ),
            Ok(TallyResponseDecision::Paused { retry_after })
                if retry_after == Duration::from_secs(110)
        ));
        assert!(matches!(
            tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &later),
            Ok(TallyDecision::Paused { retry_after })
                if retry_after == Duration::from_secs(110)
        ));
    }

    #[test]
    fn an_inherited_owner_pid_must_reacquire() {
        let (_temporary, directory, _tally) = tally("owner-pid");
        let mut owner =
            EndpointOwner::acquire(directory, "https://broker.invalid", "outbound-tally.test")
                .unwrap_or_else(|error| panic!("acquire endpoint owner: {error}"));
        let current = std::process::id();
        owner.pid = if current == u32::MAX { 0 } else { current + 1 };

        assert!(!owner.belongs_to_current_process());
    }

    #[test]
    fn pause_and_closure_end_at_their_exact_equality_instants() {
        let (_pause_temporary, _pause_directory, pause_tally) = tally("pause-boundary");
        let host = "https://pause.invalid";
        let sent = ready_send(&pause_tally, host, Duration::from_secs(20_000));
        pause_tally
            .record_response(host, 429, None, &sent)
            .unwrap_or_else(|error| panic!("record 429: {error}"));
        let before_pause_end = BootTime::new(
            "test-boot",
            sent.elapsed() + Duration::from_secs(60) - Duration::from_nanos(1),
        );
        assert!(matches!(
            pause_tally.decide_and_record(
                host,
                "method",
                100,
                Duration::from_secs(60),
                &before_pause_end
            ),
            Ok(TallyDecision::Paused { retry_after })
                if retry_after == Duration::from_nanos(1)
        ));
        let pause_end = BootTime::new("test-boot", sent.elapsed() + Duration::from_secs(60));
        assert!(matches!(
            pause_tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &pause_end),
            Ok(TallyDecision::Send)
        ));

        let (_closure_temporary, _closure_directory, closure_tally) = tally("closure-boundary");
        let host = "https://closure.invalid";
        let first = ready_send(&closure_tally, host, Duration::from_secs(30_000));
        closure_tally
            .record_response(host, 400, None, &first)
            .unwrap_or_else(|error| panic!("record first refusal: {error}"));
        for offset in [1_u64, 2] {
            let at = BootTime::new("test-boot", first.elapsed() + Duration::from_secs(offset));
            assert!(matches!(
                closure_tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &at),
                Ok(TallyDecision::Send)
            ));
            closure_tally
                .record_response(host, 400, None, &at)
                .unwrap_or_else(|error| panic!("record refusal {offset}: {error}"));
        }
        let closure_started = first.elapsed() + Duration::from_secs(2);
        let before_closure_end = BootTime::new(
            "test-boot",
            closure_started + Duration::from_secs(30 * 60) - Duration::from_nanos(1),
        );
        assert!(matches!(
            closure_tally.decide_and_record(
                host,
                "method",
                100,
                Duration::from_secs(60),
                &before_closure_end
            ),
            Ok(TallyDecision::Closed { retry_after, .. })
                if retry_after == Duration::from_nanos(1)
        ));
        let closure_end =
            BootTime::new("test-boot", closure_started + Duration::from_secs(30 * 60));
        assert!(matches!(
            closure_tally.decide_and_record(
                host,
                "method",
                100,
                Duration::from_secs(60),
                &closure_end
            ),
            Ok(TallyDecision::Send)
        ));
    }
}
