use fs2::FileExt;
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, fstat, fsync, open, openat, renameat, statat,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;

use crate::client::REQUEST_TIMEOUT;
use crate::destination::Destination;
use crate::gateway::{BootTime, Clock};

const HEADER: &str = "iaam-outbound-tally-v4";
pub(crate) const TALLY_FILE: &str = "outbound-tally";
const TALLY_LOCK_FILE: &str = "outbound-tally.lock";
const TALLY_TEMP_FILE: &str = "outbound-tally.tmp";
pub(crate) const GENERATION_FILE: &str = "outbound-tally-generation";
const GENERATION_TEMP_FILE: &str = "outbound-tally-generation.tmp";
const MINUTE_NANOS: u128 = 60_000_000_000;
const SECOND_NANOS: u128 = 1_000_000_000;
const DEPARTURE_SPACING_NANOS: u128 = SECOND_NANOS;
const REFUSAL_WINDOW_NANOS: u128 = 10 * MINUTE_NANOS;
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(60);
const RATE_LIMIT_PAUSE_NANOS: u128 = RATE_LIMIT_PAUSE.as_nanos();
const CLOSURE_NANOS: u128 = 30 * MINUTE_NANOS;
const DAY_NANOS: u128 = 24 * 60 * MINUTE_NANOS;
/// How long an endpoint stays closed after a request whose outcome was never
/// recorded (an adopted pending attempt, a failed detached task, and every
/// such closure again after a boot change). One hour by the owner's decision
/// of 2026-09-29, replacing the 24-hour `Retry-After` clamp.
const UNRESOLVED_CLOSURE_NANOS: u128 = 60 * MINUTE_NANOS;
const FIRST_SEND_WAIT_NANOS: u128 = MINUTE_NANOS;
const UNRESOLVED_ATTEMPT: Duration = REQUEST_TIMEOUT.saturating_add(RATE_LIMIT_PAUSE);
const UNRESOLVED_ATTEMPT_NANOS: u128 = UNRESOLVED_ATTEMPT.as_nanos();
const _: () = assert!(UNRESOLVED_ATTEMPT.as_secs() == 90 && UNRESOLVED_ATTEMPT.subsec_nanos() == 0);
pub(crate) const TALLY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const DAILY_CEILING: u32 = 1_000;
const BROKER_HOSTS: [&str; 3] = [
    Destination::TinkoffProd.base_url(),
    Destination::TinkoffSandbox.base_url(),
    Destination::FinamApi.base_url(),
];

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

    const fn duration_after_boot_change(self) -> u128 {
        match self {
            Self::UnresolvedAttempt => UNRESOLVED_CLOSURE_NANOS,
            Self::Refusals | Self::RateLimits | Self::RepeatedResponses => CLOSURE_NANOS,
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
    pending_budget_key: Option<String>,
}

#[derive(Default)]
struct State {
    generation: u64,
    boot_id: Option<String>,
    boot_high_water: Option<u128>,
    hosts: BTreeMap<String, HostState>,
    sends: BTreeMap<(String, String), Vec<u128>>,
    pair_was_empty: bool,
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
    generation_high_water: AtomicU64,
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
            generation_high_water: AtomicU64::new(0),
        })))
    }

    pub(crate) fn tally_path(&self) -> PathBuf {
        self.0.path.join(TALLY_FILE)
    }

    /// Opens the instance's egress directory, creating the place when it does
    /// not exist yet.
    ///
    /// The place is a directory beside the database, created with mode 0700
    /// (the owner bits survive every umask), so no step needs root. The fresh
    /// empty tally record is created the way the documented
    /// `install -m 0600 /dev/null` step created it: an empty pair is the
    /// documented conservative recovery state, never truncating an existing
    /// record. Every check of [`Self::open`] then applies to whatever was
    /// found or created.
    pub(crate) fn open_or_create(path: &Path) -> Result<Self, TallyError> {
        match std::fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(TallyError::File {
                    action: "create the egress directory beside the database",
                    source,
                });
            }
        }
        let directory = Self::open(path)?;
        let (record, _) = directory.open_child(
            TALLY_FILE,
            OFlags::RDWR | OFlags::CREATE,
            "create the outbound tally record",
        )?;
        drop(record);
        Ok(directory)
    }

    fn verify(&self) -> Result<(), TallyError> {
        let named = statat(CWD, &self.0.path, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| file_error("verify the egress directory", source))?;
        if self.0.identity != FileIdentity::of(&named) {
            return Err(TallyError::InvalidPath(
                "the egress directory beside the database was replaced; restore it before retrying"
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

    pub(crate) fn new(directory: EgressDirectory) -> Result<Self, TallyError> {
        let (file, _) =
            directory.open_child(TALLY_FILE, OFlags::RDONLY, "open the outbound tally")?;
        drop(file);
        let (generation, _) = directory.open_child(
            GENERATION_FILE,
            OFlags::RDWR | OFlags::CREATE,
            "open the outbound tally generation",
        )?;
        drop(generation);
        Ok(Self { directory })
    }

    /// Mint the fresh zero tally of the instance's first broker enabling.
    ///
    /// A missing or empty pair is the conservative recovery state: on first
    /// use iaam records a full day of attempts for every broker endpoint, so a
    /// lost record restores no allowance. That is right for a *lost* record —
    /// and wrong for the instance's first `connect`, which would spend its
    /// first day refused. The first enabling therefore writes a pair that
    /// proves its own freshness: the current format, the current boot, a
    /// matched generation, and no recorded attempt anywhere — every ceiling
    /// still holds, and nothing is spent.
    ///
    /// The mint never overwrites a non-empty pair and answers `false` then:
    /// whatever the pair holds governs, exactly as for `serve`. The empty-pair
    /// check runs under the tally lock, so a concurrent decision either
    /// happened before the mint saw an empty file or after it saw a full one.
    ///
    /// # Errors
    /// The pair could not be opened, read, written or persisted; or the
    /// generation record carries content beside an empty tally.
    pub(crate) fn initialize_fresh_pair(&self, clock: &dyn Clock) -> Result<bool, TallyError> {
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
            let (mut state, tally_identity, generation_identity) = self.read_state()?;
            if !state.pair_was_empty {
                return Ok(false);
            }
            let boot = read_boot(clock)?;
            state.generation = state.generation.checked_add(1).ok_or_else(|| {
                TallyError::Corrupt("the tally generation is exhausted".to_owned())
            })?;
            state.boot_id = Some(boot.id().to_owned());
            state.boot_high_water = Some(boot.elapsed().as_nanos());
            // Every endpoint stands at zero: no send, no wait, no closure, no
            // pause. The rows exist so the first decision on each finds its
            // host already held at zero instead of inserting it at the
            // boot-change wait, while a later boot change keeps the documented
            // first-send wait.
            for host in BROKER_HOSTS {
                state.hosts.insert(host.to_owned(), HostState::default());
            }
            self.persist(&state, tally_identity, generation_identity)?;
            self.directory
                .0
                .generation_high_water
                .fetch_max(state.generation, Ordering::AcqRel);
            Ok(true)
        })();
        let unlocked = FileExt::unlock(&lock).map_err(|source| TallyError::File {
            action: "unlock the tally lock file",
            source,
        });
        match (result, unlocked) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(minted), Ok(())) => Ok(minted),
        }
    }

    pub(crate) fn decide_and_record(
        &self,
        host: &str,
        budget_key: &str,
        budget_limit: u32,
        budget_window: Duration,
        clock: &dyn Clock,
    ) -> Result<TallyDecision, TallyError> {
        self.transact(|state| {
            let boot = read_boot(clock)?;
            let request = DecisionRequest {
                host,
                budget_key,
                budget_limit,
                budget_window,
                boot: &boot,
            };
            Self::decide(state, &request)
        })
    }

    pub(crate) fn acknowledge_handoff(
        &self,
        host: &str,
        clock: &dyn Clock,
    ) -> Result<(), TallyError> {
        self.transact(|state| {
            let boot = read_boot(clock)?;
            let now_nanos = boot.elapsed().as_nanos();
            let mut changed = state.prepare_boot(&boot)?;
            if state
                .hosts
                .get(host)
                .is_some_and(|host_state| host_state.pending_since.is_some())
            {
                state.move_pending_to(host, now_nanos)?;
                changed = true;
            }
            changed |= state.prune(now_nanos);
            Ok(((), changed))
        })
    }

    pub(crate) fn adopt_pending(&self, host: &str, clock: &dyn Clock) -> Result<(), TallyError> {
        self.transact(|state| {
            let boot = read_boot(clock)?;
            let now_nanos = boot.elapsed().as_nanos();
            let mut changed = state.prepare_boot(&boot)?;
            if state
                .hosts
                .get(host)
                .is_some_and(|host_state| host_state.pending_since.is_some())
            {
                state.move_pending_to(host, now_nanos)?;
                state.prune(now_nanos);
                let host_state = state.host_for_boot(host, now_nanos);
                host_state.pending_since = None;
                host_state.pending_budget_key = None;
                let closed_until = now_nanos.saturating_add(UNRESOLVED_CLOSURE_NANOS);
                host_state.closed_until = Some(
                    host_state
                        .closed_until
                        .unwrap_or_default()
                        .max(closed_until),
                );
                host_state.closed_reason = Some(ClosureReason::UnresolvedAttempt);
                changed = true;
            } else {
                changed |= state.prune(now_nanos);
            }
            Ok(((), changed))
        })
    }

    pub(crate) fn record_join_failure(
        &self,
        host: &str,
        clock: &dyn Clock,
    ) -> Result<(), TallyError> {
        self.transact(|state| {
            let boot = read_boot(clock)?;
            let now_nanos = boot.elapsed().as_nanos();
            state.prepare_boot(&boot)?;
            if state
                .hosts
                .get(host)
                .is_some_and(|host_state| host_state.pending_since.is_some())
            {
                state.move_pending_to(host, now_nanos)?;
            }
            state.prune(now_nanos);
            let host_state = state.host_for_boot(host, now_nanos);
            host_state.pending_since = None;
            host_state.pending_budget_key = None;
            let closed_until = now_nanos.saturating_add(UNRESOLVED_CLOSURE_NANOS);
            host_state.closed_until = Some(
                host_state
                    .closed_until
                    .unwrap_or_default()
                    .max(closed_until),
            );
            host_state.closed_reason = Some(ClosureReason::UnresolvedAttempt);
            Ok(((), true))
        })
    }

    pub(crate) fn record_response(
        &self,
        host: &str,
        status: u16,
        retry_after: Option<Duration>,
        clock: &dyn Clock,
    ) -> Result<TallyResponseDecision, TallyError> {
        self.transact(|state| {
            let boot = read_boot(clock)?;
            let now_nanos = boot.elapsed().as_nanos();
            state.prepare_boot(&boot)?;
            if !state
                .hosts
                .get(host)
                .is_some_and(|state| state.pending_since.is_some())
            {
                return Err(TallyError::Corrupt(format!(
                    "endpoint {host:?} returned a status without a pending request"
                )));
            }
            state.move_pending_to(host, now_nanos)?;
            state.prune(now_nanos);
            let host_state = state.host_for_boot(host, now_nanos);
            host_state.pending_since = None;
            host_state.pending_budget_key = None;
            let named_pause = (!(200..300).contains(&status))
                .then(|| retry_after.map(|delay| delay.as_nanos().min(DAY_NANOS)))
                .flatten();
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
        clock: &dyn Clock,
    ) -> Result<Duration, TallyError> {
        self.transact(|state| {
            let boot = read_boot(clock)?;
            let now_nanos = boot.elapsed().as_nanos();
            state.prepare_boot(&boot)?;
            state.move_pending_to(host, now_nanos)?;
            state.prune(now_nanos);
            let host_state = state.host_for_boot(host, now_nanos);
            let pending_since = host_state.pending_since.take().unwrap_or(now_nanos);
            host_state.pending_budget_key = None;
            let (closed_until, reason) = if host_state.rate_limits.is_empty() {
                (
                    pending_since
                        .saturating_add(UNRESOLVED_ATTEMPT_NANOS)
                        .max(now_nanos.saturating_add(RATE_LIMIT_PAUSE_NANOS)),
                    ClosureReason::UnresolvedAttempt,
                )
            } else {
                (
                    now_nanos.saturating_add(CLOSURE_NANOS),
                    ClosureReason::RateLimits,
                )
            };
            host_state.closed_until = Some(
                host_state
                    .closed_until
                    .unwrap_or_default()
                    .max(closed_until),
            );
            host_state.closed_reason = Some(reason);
            Ok((
                duration_from_nanos(closed_until.saturating_sub(now_nanos)),
                true,
            ))
        })
    }

    pub(crate) fn record_not_handed_off(
        &self,
        host: &str,
        clock: &dyn Clock,
    ) -> Result<(), TallyError> {
        self.transact(|state| {
            let boot = read_boot(clock)?;
            let now_nanos = boot.elapsed().as_nanos();
            state.prepare_boot(&boot)?;
            state.prune(now_nanos);
            state.rollback_pending(host)?;
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
        if let Some(pending_since) = state
            .hosts
            .get(host)
            .and_then(|host_state| host_state.pending_since)
        {
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
            state.move_pending_to(host, now_nanos)?;
            let host_state = state.hosts.get_mut(host).ok_or_else(|| {
                TallyError::Corrupt("the endpoint state was not created".to_owned())
            })?;
            host_state.pending_since = None;
            host_state.pending_budget_key = None;
            changed = true;
            if !host_state.rate_limits.is_empty() {
                host_state.closed_until = Some(now_nanos.saturating_add(CLOSURE_NANOS));
                host_state.closed_reason = Some(ClosureReason::RateLimits);
            }
        }
        let host_state = state
            .hosts
            .get_mut(host)
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
        host_state.daily_sends.push(now_nanos);
        host_state.boot_wait_until = None;
        host_state.pending_since = Some(now_nanos);
        host_state.pending_budget_key = Some(budget_key.to_owned());
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
            let (mut state, tally_identity, generation_identity) = self.read_state()?;
            let (value, changed) = update(&mut state)?;
            if changed {
                state.generation = state.generation.checked_add(1).ok_or_else(|| {
                    TallyError::Corrupt("the tally generation is exhausted".to_owned())
                })?;
                self.persist(&state, tally_identity, generation_identity)?;
                self.directory
                    .0
                    .generation_high_water
                    .fetch_max(state.generation, Ordering::AcqRel);
            } else {
                self.directory.verify_child(TALLY_FILE, tally_identity)?;
                self.directory
                    .verify_child(GENERATION_FILE, generation_identity)?;
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

    fn read_state(&self) -> Result<(State, FileIdentity, FileIdentity), TallyError> {
        let (mut tally_file, tally_identity) =
            self.directory
                .open_child(TALLY_FILE, OFlags::RDONLY, "open the tally file")?;
        let mut tally_bytes = Vec::new();
        tally_file
            .read_to_end(&mut tally_bytes)
            .map_err(|source| TallyError::File {
                action: "read the tally file",
                source,
            })?;
        self.directory.verify_child(TALLY_FILE, tally_identity)?;
        let tally_text = String::from_utf8(tally_bytes)
            .map_err(|_| TallyError::Corrupt("the tally file is not UTF-8".to_owned()))?;

        let (mut generation_file, generation_identity) = self.directory.open_child(
            GENERATION_FILE,
            OFlags::RDONLY,
            "open the outbound tally generation",
        )?;
        let mut generation_text = String::new();
        generation_file
            .read_to_string(&mut generation_text)
            .map_err(|source| TallyError::File {
                action: "read the outbound tally generation",
                source,
            })?;
        self.directory
            .verify_child(GENERATION_FILE, generation_identity)?;

        let mut state = State::parse(&tally_text)?;
        let process_generation = self
            .directory
            .0
            .generation_high_water
            .load(Ordering::Acquire);
        if tally_text.is_empty() {
            if !generation_text.is_empty() {
                return Err(TallyError::Corrupt(
                    "the outbound tally was emptied without its generation record; empty or restore both records together during a controlled stop"
                        .to_owned(),
                ));
            }
            state.generation = process_generation;
        } else {
            let recorded_generation = parse_generation(&generation_text)?;
            if recorded_generation != state.generation {
                return Err(TallyError::Corrupt(format!(
                    "the outbound tally generation rolled back from {recorded_generation} to {}; restore the current tally and generation record before retrying",
                    state.generation
                )));
            }
            if state.generation < process_generation {
                return Err(TallyError::Corrupt(format!(
                    "the tally pair generation {} is older than this process generation high-water {process_generation}; no older tally format was deployed, so restore the current pair before retrying",
                    state.generation
                )));
            }
            self.directory
                .0
                .generation_high_water
                .fetch_max(state.generation, Ordering::AcqRel);
        }
        Ok((state, tally_identity, generation_identity))
    }

    fn persist(
        &self,
        state: &State,
        expected_tally: FileIdentity,
        expected_generation: FileIdentity,
    ) -> Result<(), TallyError> {
        self.directory.verify_child(TALLY_FILE, expected_tally)?;
        self.directory
            .verify_child(GENERATION_FILE, expected_generation)?;
        let encoded = state.encode()?;
        let (mut tally_file, tally_temporary_identity) = self.directory.open_child(
            TALLY_TEMP_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
            "open the temporary tally file",
        )?;
        tally_file
            .write_all(encoded.as_bytes())
            .map_err(|source| TallyError::File {
                action: "write the temporary tally file",
                source,
            })?;
        tally_file.sync_all().map_err(|source| TallyError::File {
            action: "persist the temporary tally file",
            source,
        })?;

        let (mut generation_file, generation_temporary_identity) = self.directory.open_child(
            GENERATION_TEMP_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
            "open the temporary tally generation",
        )?;
        writeln!(generation_file, "{}", state.generation).map_err(|source| TallyError::File {
            action: "write the temporary tally generation",
            source,
        })?;
        generation_file
            .sync_all()
            .map_err(|source| TallyError::File {
                action: "persist the temporary tally generation",
                source,
            })?;

        self.directory
            .verify_child(TALLY_TEMP_FILE, tally_temporary_identity)?;
        self.directory
            .verify_child(GENERATION_TEMP_FILE, generation_temporary_identity)?;
        self.directory.verify_child(TALLY_FILE, expected_tally)?;
        self.directory
            .verify_child(GENERATION_FILE, expected_generation)?;
        renameat(
            &self.directory.0.descriptor,
            GENERATION_TEMP_FILE,
            &self.directory.0.descriptor,
            GENERATION_FILE,
        )
        .map_err(|source| file_error("replace the tally generation", source))?;
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
            return Ok(Self {
                pair_was_empty: true,
                ..Self::default()
            });
        }
        let mut lines = text.lines();
        let header = lines.next();
        if header != Some(HEADER) {
            return Err(TallyError::Corrupt(format!(
                "tally format {header:?} is not the current {HEADER:?}; no older tally format was deployed, so recreate the tally and generation record during a controlled stop"
            )));
        }
        let mut state = Self::default();
        let mut generation = None;
        for (index, line) in lines.enumerate() {
            let number = index + 2;
            let fields: Vec<_> = line.split('\t').collect();
            match fields.as_slice() {
                ["generation", value] if generation.is_none() => {
                    generation = Some(parse_number(value, number, "generation")?);
                }
                ["boot", boot_id] if state.boot_id.is_none() => {
                    validate_atom(boot_id, number)?;
                    state.boot_id = Some((*boot_id).to_owned());
                }
                ["high-water", high_water] if state.boot_high_water.is_none() => {
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
                    pending_since,
                    pending_budget_key,
                ] => {
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
                    let pending_since = parse_optional_nanos(pending_since, number)?;
                    let pending_budget_key =
                        parse_optional_atom(pending_budget_key, number, "pending budget key")?;
                    if pending_since.is_some() != pending_budget_key.is_some() {
                        return Err(TallyError::Corrupt(format!(
                            "line {number} must carry the pending time and budget key together"
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
                            pending_since,
                            pending_budget_key,
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
                        "line {number} has an unknown or repeated record shape"
                    )));
                }
            }
        }
        state.generation = generation
            .ok_or_else(|| TallyError::Corrupt("the tally has no generation".to_owned()))?;
        if state.boot_id.is_none() {
            return Err(TallyError::Corrupt("the tally has no boot id".to_owned()));
        }
        if state.boot_high_water.is_none() {
            return Err(TallyError::Corrupt(
                "the tally has no boot high-water mark".to_owned(),
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

    fn move_pending_to(&mut self, host: &str, now_nanos: u128) -> Result<(), TallyError> {
        let Some((pending_since, budget_key)) = self.hosts.get(host).and_then(|state| {
            state
                .pending_since
                .zip(state.pending_budget_key.as_ref())
                .map(|(pending_since, budget_key)| (pending_since, budget_key.clone()))
        }) else {
            return Ok(());
        };
        let moved_to = pending_since.max(now_nanos);
        let key = (host.to_owned(), budget_key);
        let method_last = self.sends.get(&key).and_then(|sent| sent.last()).copied();
        let daily_last = self
            .hosts
            .get(host)
            .and_then(|host_state| host_state.daily_sends.last())
            .copied();
        if method_last.is_some_and(|at| at != pending_since)
            || daily_last.is_some_and(|at| at != pending_since)
        {
            return Err(TallyError::Corrupt(format!(
                "endpoint {host:?} pending request does not name the latest method and daily timestamps"
            )));
        }

        let sent = self.sends.entry(key).or_default();
        if let Some(method) = sent.last_mut() {
            *method = moved_to;
        } else {
            sent.push(moved_to);
        }
        let host_state = self
            .hosts
            .get_mut(host)
            .ok_or_else(|| TallyError::Corrupt(format!("endpoint {host:?} state disappeared")))?;
        if let Some(daily) = host_state.daily_sends.last_mut() {
            *daily = moved_to;
        } else {
            host_state.daily_sends.push(moved_to);
        }
        host_state.last_send = Some(host_state.last_send.unwrap_or_default().max(moved_to));
        host_state.pending_since = Some(moved_to);
        Ok(())
    }

    fn rollback_pending(&mut self, host: &str) -> Result<(), TallyError> {
        let Some((pending_since, budget_key)) = self.hosts.get(host).and_then(|state| {
            state
                .pending_since
                .zip(state.pending_budget_key.as_deref())
                .map(|(pending, key)| (pending, key.to_owned()))
        }) else {
            return Ok(());
        };
        let host_state = self
            .hosts
            .get_mut(host)
            .ok_or_else(|| TallyError::Corrupt(format!("endpoint {host:?} state disappeared")))?;
        if host_state.daily_sends.last() == Some(&pending_since) {
            host_state.daily_sends.pop();
        }
        host_state.pending_since = None;
        host_state.pending_budget_key = None;
        if let Some(sends) = self.sends.get_mut(&(host.to_owned(), budget_key))
            && sends.last() == Some(&pending_since)
        {
            sends.pop();
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
            host.closed_until = match (host.closed_until, host.closed_reason) {
                (Some(_), Some(reason)) => {
                    Some(now.saturating_add(reason.duration_after_boot_change()))
                }
                (None, None) => None,
                _ => {
                    return Err(TallyError::Corrupt(
                        "an endpoint closure must carry its reason".to_owned(),
                    ));
                }
            };
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
        if self.pair_was_empty {
            for host in BROKER_HOSTS {
                self.hosts.insert(
                    host.to_owned(),
                    HostState {
                        last_send: Some(now),
                        daily_sends: vec![now; DAILY_CEILING as usize],
                        ..HostState::default()
                    },
                );
            }
            self.pair_was_empty = false;
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
            let hosts = &self.hosts;
            self.sends.retain(|(host, budget_key), sent| {
                let pending = hosts.get(host).and_then(|state| {
                    state
                        .pending_since
                        .zip(state.pending_budget_key.as_deref())
                        .filter(|(_, pending_key)| *pending_key == budget_key)
                        .map(|(at, _)| at)
                });
                let previous = sent.len();
                sent.retain(|at| *at > send_cutoff || pending == Some(*at));
                changed |= sent.len() != previous;
                let keep = !sent.is_empty();
                changed |= !keep;
                keep
            });
        }
        let refusal_cutoff = now_nanos.checked_sub(REFUSAL_WINDOW_NANOS);
        for state in self.hosts.values_mut() {
            if let Some(send_cutoff) = send_cutoff {
                let pending = state.pending_since;
                let previous_daily = state.daily_sends.len();
                state
                    .daily_sends
                    .retain(|at| *at > send_cutoff || pending == Some(*at));
                changed |= state.daily_sends.len() != previous_daily;
            }
            if let Some(refusal_cutoff) = refusal_cutoff {
                let previous_refusals = state.refusals.len();
                state.refusals.retain(|at| *at > refusal_cutoff);
                changed |= state.refusals.len() != previous_refusals;
                let previous_rate_limits = state.rate_limits.len();
                state.rate_limits.retain(|at| *at >= refusal_cutoff);
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
        text.push_str("generation\t");
        text.push_str(&self.generation.to_string());
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
            text.push('\t');
            if let Some(pending_budget_key) = state.pending_budget_key.as_deref() {
                validate_atom(pending_budget_key, 0)?;
                text.push_str(pending_budget_key);
            } else {
                text.push('-');
            }
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

fn read_boot(clock: &dyn Clock) -> Result<BootTime, TallyError> {
    clock.now_boot().map_err(TallyError::Clock)
}

fn parse_generation(text: &str) -> Result<u64, TallyError> {
    let value = text.strip_suffix('\n').unwrap_or(text);
    if value.is_empty() || value.contains('\n') {
        return Err(TallyError::Corrupt(
            "the outbound tally generation record is empty or malformed; restore the current tally and generation record before retrying"
                .to_owned(),
        ));
    }
    parse_number(value, 0, "outbound tally generation")
}

fn parse_optional_atom(
    value: &str,
    line: usize,
    field: &str,
) -> Result<Option<String>, TallyError> {
    if value == "-" {
        return Ok(None);
    }
    validate_atom(value, line)?;
    if value.is_empty() {
        return Err(TallyError::Corrupt(format!(
            "line {line} has invalid {field}"
        )));
    }
    Ok(Some(value.to_owned()))
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
    use std::sync::{Mutex, mpsc};
    use std::time::Instant;

    use super::*;

    impl Clock for BootTime {
        fn now(&self) -> std::time::Instant {
            std::time::Instant::now()
        }

        fn now_boot(&self) -> Result<BootTime, String> {
            Ok(self.clone())
        }
    }

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
        tally
            .transact(|state| {
                let now = later.elapsed().as_nanos();
                state
                    .sends
                    .entry((host.to_owned(), "method".to_owned()))
                    .or_default()
                    .push(now);
                let host_state = state
                    .hosts
                    .get_mut(host)
                    .expect("the first response created host state");
                host_state.daily_sends.push(now);
                host_state.pending_since = Some(now);
                host_state.pending_budget_key = Some("method".to_owned());
                Ok(((), true))
            })
            .expect("an overlapping admitted request is persisted");
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

    #[test]
    fn boot_clock_is_not_sampled_until_the_tally_lock_is_held() {
        struct SignallingClock {
            sampled: Mutex<Option<mpsc::Sender<()>>>,
        }

        impl Clock for SignallingClock {
            fn now(&self) -> Instant {
                Instant::now()
            }

            fn now_boot(&self) -> Result<BootTime, String> {
                if let Some(sampled) = self
                    .sampled
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    sampled.send(()).expect("clock sample observed");
                }
                Ok(BootTime::new("test-boot", Duration::from_secs(1)))
            }
        }

        let (_temporary, directory, tally) = tally("clock-inside-lock");
        let (lock, _) = directory
            .open_child(
                TALLY_LOCK_FILE,
                OFlags::RDWR | OFlags::CREATE,
                "open the test tally lock",
            )
            .expect("tally lock opened");
        lock.lock_exclusive().expect("test holds tally lock");
        let (sampled_tx, sampled_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let clock = Arc::new(SignallingClock {
            sampled: Mutex::new(Some(sampled_tx)),
        });
        let thread_tally = tally.clone();
        let thread_clock = Arc::clone(&clock);
        let worker = std::thread::spawn(move || {
            started_tx.send(()).expect("worker started");
            thread_tally.decide_and_record(
                "https://broker.invalid",
                "method",
                100,
                Duration::from_secs(60),
                thread_clock.as_ref(),
            )
        });
        started_rx.recv().expect("worker reached the transaction");
        assert!(
            sampled_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "the boot clock was sampled before the tally lock became available"
        );

        FileExt::unlock(&lock).expect("test releases tally lock");
        sampled_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("clock sampled after lock acquisition");
        assert!(matches!(
            worker.join().expect("worker did not panic"),
            Ok(TallyDecision::Wait(wait)) if wait == Duration::from_secs(60)
        ));
    }

    #[test]
    fn replacing_the_egress_directory_is_refused() {
        let temporary = TempDir::new("replace-directory");
        let directory = EgressDirectory::open(&temporary.0).expect("egress directory opened");
        let moved = temporary.0.with_extension("replaced");
        std::fs::rename(&temporary.0, &moved).expect("original directory moved");
        std::fs::create_dir(&temporary.0).expect("replacement directory created");

        let refused = directory.verify().expect_err("replacement must be refused");
        assert!(
            matches!(&refused, TallyError::InvalidPath(reason) if reason.contains("replaced")),
            "{refused}"
        );
        std::fs::remove_dir_all(&moved).expect("moved directory removed");
    }

    #[test]
    fn open_or_create_creates_the_place_beside_the_database() {
        let outside = TempDir::new("create-outside");
        let fresh = outside.0.join("fresh");
        std::fs::create_dir(&fresh).expect("the database's directory exists");
        let place = fresh.join("iaam.sqlite.egress");

        let directory =
            EgressDirectory::open_or_create(&place).expect("fresh place created and opened");

        let mode =
            std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&place).expect("place exists"));
        assert_eq!(mode & 0o777, 0o700, "the place is private to its owner");
        let record = std::fs::read(directory.tally_path()).expect("tally record exists");
        assert!(record.is_empty(), "the fresh tally record is empty");
    }

    #[test]
    fn open_or_create_never_truncates_an_existing_record() {
        let outside = TempDir::new("create-existing");
        let place = outside.0.join("iaam.sqlite.egress");
        std::fs::create_dir(&place).expect("place created");
        std::fs::write(
            place.join(TALLY_FILE),
            "iaam-outbound-tally-v4\ngeneration\t7\n",
        )
        .expect("existing tally written");

        let directory = EgressDirectory::open_or_create(&place).expect("existing place opened");

        let record = std::fs::read(directory.tally_path()).expect("tally record read");
        assert!(
            record.starts_with(b"iaam-outbound-tally-v4\ngeneration\t7"),
            "an existing record must not be touched: {}",
            String::from_utf8_lossy(&record)
        );
    }

    #[test]
    fn open_or_create_names_the_failed_creation_when_the_parent_is_missing() {
        let outside = TempDir::new("create-orphan");
        let place = outside.0.join("absent-parent").join("iaam.sqlite.egress");

        let refused = EgressDirectory::open_or_create(&place)
            .expect_err("a place whose parent is missing is not created");

        // The refusal names the creation that failed, not a later open: the
        // database's directory always exists by the time the gateway is
        // built, so a missing parent is a real misconfiguration to report.
        assert!(
            matches!(
                refused,
                TallyError::File {
                    action: "create the egress directory beside the database",
                    ..
                }
            ),
            "{refused}"
        );
    }

    #[test]
    fn open_or_create_refuses_a_file_where_the_place_belongs() {
        let outside = TempDir::new("create-file");
        let place = outside.0.join("iaam.sqlite.egress");
        std::fs::write(&place, "").expect("plain file written");

        let refused = EgressDirectory::open_or_create(&place)
            .expect_err("a file must not become the egress place");

        // The refusal names the open that failed, whatever its shape: the
        // place is unusable and nothing was created inside it.
        let message = refused.to_string();
        assert!(message.contains("egress directory"), "{refused}");
    }

    #[test]
    fn replacing_a_held_child_record_is_refused() {
        let temporary = TempDir::new("replace-child");
        let directory = EgressDirectory::open(&temporary.0).expect("egress directory opened");
        let (_held, identity) = directory
            .open_child(TALLY_FILE, OFlags::RDONLY, "open held tally")
            .expect("held tally opened");
        let moved = temporary.0.join("old-outbound-tally");
        std::fs::rename(temporary.0.join(TALLY_FILE), &moved).expect("old tally moved");
        std::fs::write(temporary.0.join(TALLY_FILE), "").expect("replacement tally created");

        let refused = directory
            .verify_child(TALLY_FILE, identity)
            .expect_err("child replacement must be refused");
        assert!(
            matches!(&refused, TallyError::InvalidPath(reason) if reason.contains("replaced")),
            "{refused}"
        );
    }
    #[test]
    fn recording_an_unknown_outcome_moves_the_reservation_before_clearing_it() {
        let (_temporary, _directory, tally) = tally("unknown-outcome-moves-reservation");
        let host = "https://broker.invalid";
        let sent = ready_send(&tally, host, Duration::from_secs(40_000));
        let resolved = BootTime::new("test-boot", sent.elapsed() + Duration::from_secs(61));

        tally
            .record_unknown_outcome(host, &resolved)
            .unwrap_or_else(|error| panic!("record unknown outcome: {error}"));
        let (method, daily, pending) = tally
            .transact(|state| {
                let host_state = state.hosts.get(host).expect("host state");
                Ok((
                    (
                        state
                            .sends
                            .get(&(host.to_owned(), "method".to_owned()))
                            .and_then(|sent| sent.last())
                            .copied(),
                        host_state.daily_sends.last().copied(),
                        host_state.pending_since,
                    ),
                    false,
                ))
            })
            .expect("read moved reservation");

        assert_eq!(method, Some(resolved.elapsed().as_nanos()));
        assert_eq!(daily, Some(resolved.elapsed().as_nanos()));
        assert_eq!(pending, None);
    }

    #[test]
    fn expiring_a_pending_attempt_moves_its_timestamps_before_clearing_it() {
        let (_temporary, _directory, tally) = tally("expiry-moves-reservation");
        let host = "https://broker.invalid";
        let sent = ready_send(&tally, host, Duration::from_secs(50_000));
        tally
            .transact(|state| {
                let pending = state
                    .hosts
                    .get(host)
                    .and_then(|host_state| host_state.pending_since)
                    .expect("pending reservation");
                let host_state = state.hosts.get_mut(host).expect("host state");
                host_state.daily_sends = vec![pending; DAILY_CEILING as usize];
                Ok(((), true))
            })
            .expect("fill the rolling day");
        let expired = BootTime::new(
            "test-boot",
            sent.elapsed() + UNRESOLVED_ATTEMPT + Duration::from_nanos(1),
        );

        assert!(matches!(
            tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &expired),
            Ok(TallyDecision::DailyCeiling { .. })
        ));
        let (method, daily, pending) = tally
            .transact(|state| {
                let host_state = state.hosts.get(host).expect("host state");
                Ok((
                    (
                        state
                            .sends
                            .get(&(host.to_owned(), "method".to_owned()))
                            .and_then(|sent| sent.last())
                            .copied(),
                        host_state.daily_sends.last().copied(),
                        host_state.pending_since,
                    ),
                    false,
                ))
            })
            .expect("read expired reservation");

        assert_eq!(method, Some(expired.elapsed().as_nanos()));
        assert_eq!(daily, Some(expired.elapsed().as_nanos()));
        assert_eq!(pending, None);
    }

    #[test]
    fn adoption_reinserts_a_pruned_reservation_and_closes_for_one_hour() {
        let (_temporary, _directory, tally) = tally("adopt-pruned-reservation");
        let host = "https://broker.invalid";
        let sent = ready_send(&tally, host, Duration::from_secs(60_000));
        let adopted = BootTime::new(
            "test-boot",
            sent.elapsed() + TALLY_RETENTION + Duration::from_secs(1),
        );
        let (retained_method, retained_daily) = tally
            .transact(|state| {
                let changed = state.prune(adopted.elapsed().as_nanos());
                let host_state = state.hosts.get(host).expect("host state");
                Ok((
                    (
                        state
                            .sends
                            .get(&(host.to_owned(), "method".to_owned()))
                            .and_then(|sent| sent.last())
                            .copied(),
                        host_state.daily_sends.last().copied(),
                    ),
                    changed,
                ))
            })
            .expect("prune old timestamps");
        assert_eq!(retained_method, Some(sent.elapsed().as_nanos()));
        assert_eq!(retained_daily, Some(sent.elapsed().as_nanos()));
        assert!(matches!(
            tally.decide_and_record(
                "https://other-broker.invalid",
                "other-method",
                100,
                Duration::from_secs(60),
                &adopted,
            ),
            Ok(TallyDecision::Wait(wait)) if wait == Duration::from_secs(60)
        ));
        tally
            .transact(|state| {
                state.sends.remove(&(host.to_owned(), "method".to_owned()));
                Ok(((), true))
            })
            .expect("plant an already-pruned reservation");

        tally
            .adopt_pending(host, &adopted)
            .unwrap_or_else(|error| panic!("adopt pruned reservation: {error}"));
        let (method, daily, pending, closed_until) = tally
            .transact(|state| {
                let host_state = state.hosts.get(host).expect("host state");
                Ok((
                    (
                        state
                            .sends
                            .get(&(host.to_owned(), "method".to_owned()))
                            .and_then(|sent| sent.last())
                            .copied(),
                        host_state.daily_sends.last().copied(),
                        host_state.pending_since,
                        host_state.closed_until,
                    ),
                    false,
                ))
            })
            .expect("read adopted reservation");
        let adopted_nanos = adopted.elapsed().as_nanos();

        assert_eq!(method, Some(adopted_nanos));
        assert_eq!(daily, Some(adopted_nanos));
        assert_eq!(pending, None);
        assert_eq!(closed_until, Some(adopted_nanos + UNRESOLVED_CLOSURE_NANOS));
    }

    #[test]
    fn a_pending_attempt_refuses_mismatched_remaining_evidence() {
        for mismatch in ["method", "daily"] {
            let label = format!("pending-mismatch-{mismatch}");
            let (_temporary, _directory, tally) = tally(&label);
            let host = "https://broker.invalid";
            let sent = ready_send(&tally, host, Duration::from_secs(65_000));
            tally
                .transact(|state| {
                    let earlier = sent.elapsed().as_nanos().saturating_sub(1);
                    if mismatch == "method" {
                        *state
                            .sends
                            .get_mut(&(host.to_owned(), "method".to_owned()))
                            .and_then(|sent| sent.last_mut())
                            .expect("method reservation") = earlier;
                    } else {
                        *state
                            .hosts
                            .get_mut(host)
                            .and_then(|state| state.daily_sends.last_mut())
                            .expect("daily reservation") = earlier;
                    }
                    Ok(((), true))
                })
                .expect("plant mismatched evidence");

            let adopted = BootTime::new("test-boot", sent.elapsed() + Duration::from_secs(1));
            let refused = tally.adopt_pending(host, &adopted);
            assert!(
                matches!(&refused, Err(TallyError::Corrupt(reason)) if reason.contains("latest method and daily")),
                "{mismatch}: {refused:?}"
            );
        }
    }

    #[test]
    fn an_empty_record_pair_starts_each_endpoint_at_the_day_ceiling() {
        let (_temporary, _directory, tally) = tally("empty-pair-day-ceiling");
        let started = BootTime::new("test-boot", Duration::from_secs(70_000));
        for host in BROKER_HOSTS {
            assert!(matches!(
                tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &started),
                Ok(TallyDecision::DailyCeiling { retry_after })
                    if retry_after == TALLY_RETENTION
            ));
        }
        let boundary = BootTime::new("test-boot", started.elapsed() + TALLY_RETENTION);

        for host in BROKER_HOSTS {
            assert!(matches!(
                tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &boundary),
                Ok(TallyDecision::Send)
            ));
        }
    }

    #[test]
    fn a_process_refuses_a_matching_pair_rolled_below_its_generation_high_water() {
        let (temporary, _directory, tally) = tally("matching-pair-rollback");
        let host = "https://broker.invalid";
        let first = ready_send(&tally, host, Duration::from_secs(80_000));
        tally
            .record_response(host, 200, None, &first)
            .unwrap_or_else(|error| panic!("record first response: {error}"));
        let older_tally = std::fs::read(temporary.0.join(TALLY_FILE))
            .unwrap_or_else(|error| panic!("read older tally: {error}"));
        let older_generation = std::fs::read(temporary.0.join(GENERATION_FILE))
            .unwrap_or_else(|error| panic!("read older generation: {error}"));
        let second = BootTime::new("test-boot", first.elapsed() + Duration::from_secs(1));
        assert!(matches!(
            tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &second),
            Ok(TallyDecision::Send)
        ));
        tally
            .record_response(host, 200, None, &second)
            .unwrap_or_else(|error| panic!("record second response: {error}"));

        std::fs::write(temporary.0.join(TALLY_FILE), older_tally)
            .unwrap_or_else(|error| panic!("restore older tally: {error}"));
        std::fs::write(temporary.0.join(GENERATION_FILE), older_generation)
            .unwrap_or_else(|error| panic!("restore older generation: {error}"));
        let refused =
            tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &second);

        let Err(TallyError::Corrupt(reason)) = refused else {
            panic!("matching pair rollback was not refused as corrupt");
        };
        assert!(reason.contains("process generation high-water"), "{reason}");
    }

    #[test]
    fn the_first_enabling_wrapper_mints_beside_the_database_once() {
        let outside = TempDir::new("wrapper-mint");
        let database = outside.0.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database file written");
        let place = crate::egress_directory_for(&database).expect("place derived");
        assert!(!place.exists(), "a fresh instance has no egress place");

        let minted = super::super::initialize_fresh_tally(
            &database,
            &BootTime::new("wrapper-boot", Duration::from_secs(1_000)),
        )
        .expect("the first enabling wrapper mints");
        assert!(minted, "a missing pair is minted");
        let text = std::fs::read_to_string(place.join(TALLY_FILE)).expect("the pair reads");
        assert!(!text.is_empty(), "the pair is no longer the empty pair");

        let again = super::super::initialize_fresh_tally(
            &database,
            &BootTime::new("wrapper-boot", Duration::from_secs(1_000)),
        )
        .expect("the second call still reads the pair");
        assert!(!again, "the wrapper never overwrites a pair that governs");
    }

    #[test]
    fn the_first_enabling_mints_a_pair_that_holds_no_attempt() {
        let (_temporary, directory, tally) = tally("fresh-mint");
        let clock = BootTime::new("boot-1", Duration::from_secs(5_000));

        let minted = tally
            .initialize_fresh_pair(&clock)
            .expect("the first enabling mints the fresh pair");

        assert!(minted, "an empty pair is minted");
        let text =
            std::fs::read_to_string(directory.tally_path()).expect("the minted tally record reads");
        assert!(!text.is_empty(), "the pair is no longer empty");
        let state = State::parse(&text).expect("the minted pair parses");
        assert_eq!(state.boot_id.as_deref(), Some("boot-1"));
        for host in BROKER_HOSTS {
            let held = state
                .hosts
                .get(host)
                .expect("every broker endpoint is held");
            assert!(held.daily_sends.is_empty(), "{host} holds no attempt");
            assert!(held.last_send.is_none());
            assert!(held.boot_wait_until.is_none());
            assert!(held.closed_until.is_none());
            assert!(held.paused_until.is_none());
        }
    }

    #[test]
    fn a_minted_pair_sends_at_once_under_every_ceiling() {
        let (_temporary, _directory, tally) = tally("fresh-sends");
        let host = BROKER_HOSTS[0];
        let clock = BootTime::new("boot-1", Duration::from_secs(5_000));
        tally
            .initialize_fresh_pair(&clock)
            .expect("the first enabling mints the fresh pair");

        let decision = tally
            .decide_and_record(host, "method", 100, Duration::from_secs(60), &clock)
            .expect("the first decision decides");

        assert!(
            matches!(decision, TallyDecision::Send),
            "the checking call goes out at once, not at the conservative ceiling"
        );
    }

    #[test]
    fn a_mint_never_overwrites_a_pair_that_governs() {
        let (_temporary, _directory, tally) = tally("mint-keeps");
        let host = BROKER_HOSTS[0];
        let clock = BootTime::new("boot-1", Duration::from_secs(5_000));
        tally
            .initialize_fresh_pair(&clock)
            .expect("the first enabling mints the fresh pair");
        assert!(matches!(
            tally.decide_and_record(host, "method", 100, Duration::from_secs(60), &clock),
            Ok(TallyDecision::Send)
        ));

        let minted = tally
            .initialize_fresh_pair(&clock)
            .expect("a later mint still reads the pair");

        assert!(
            !minted,
            "whatever the pair holds governs; the mint never restores an allowance"
        );
    }

    #[test]
    fn a_lock_file_that_cannot_be_opened_is_named_before_anything_is_minted() {
        let (temporary, directory, tally) = tally("mint-lock-obstructed");
        // The lock file's name is taken by a directory: the mint cannot
        // even take its lock, and nothing is read or written.
        std::fs::create_dir(temporary.0.join(TALLY_LOCK_FILE)).expect("the obstruction is made");
        let clock = BootTime::new("boot-1", Duration::from_secs(5_000));

        let refused = tally
            .initialize_fresh_pair(&clock)
            .expect_err("a lock that cannot be opened is a refusal");
        let text = refused.to_string();
        assert!(text.contains("open the tally lock file"), "{text}");

        // The empty pair stands: no mint happened behind the failed lock.
        let record = std::fs::read_to_string(directory.tally_path()).expect("the record reads");
        assert!(record.is_empty(), "the pair was not minted: {record}");
    }

    #[test]
    fn a_pair_that_fails_to_read_under_the_held_lock_is_reported_as_itself() {
        let (temporary, directory, tally) = tally("mint-unreadable-pair");
        // An empty tally beside a generation record is the documented
        // corruption: the mint takes its lock, reads the pair, and refuses.
        std::fs::write(
            temporary.0.join(GENERATION_FILE),
            "a generation record that is not a number\n",
        )
        .expect("the corrupt generation record is written");
        let clock = BootTime::new("boot-1", Duration::from_secs(5_000));

        let refused = tally
            .initialize_fresh_pair(&clock)
            .expect_err("a pair that cannot be read under the lock is a refusal");
        let text = refused.to_string();
        assert!(
            text.contains("emptied without its generation record"),
            "{text}"
        );

        // The corrupt pair stands untouched: the mint never wrote.
        let record = std::fs::read_to_string(directory.tally_path()).expect("the record reads");
        assert!(record.is_empty(), "the pair was not minted: {record}");
    }
}
