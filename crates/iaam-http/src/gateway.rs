//! Gateway: the one path every outbound call takes.
//!
//! A source crate describes a request; the gateway decides whether, when and
//! how often it is sent. Everything that protects a destination from this
//! machine — and the machine from a destination — lives here, so a caller
//! cannot forget a rule by calling the transport directly:
//!
//! - a **budget** per destination and method, taken from the documented limit;
//! - for brokers, a boot-clock **outbound tally**, one-second host spacing,
//!   rolling 24-hour ceiling and one lifetime owner per endpoint;
//! - for MOEX and the CBR, **one request in flight** per host;
//! - **retries** of transient refusals, with the policy of `resilience`;
//! - an in-process **circuit breaker** per host;
//! - an optional **deadline** no attempt or wait may cross, an attempt in
//!   flight included;
//! - an in-process **reset the destination named**, obeyed by every caller of
//!   its host;
//! - a **structured event** for every long wait, retry, refusal by the
//!   destination and breaker change, carrying no header, body or secret.
//!
//! Time comes from an injected clock and sleeper, so every rule is checked on
//! a fake clock: a test that sleeps for a minute to prove a per-minute budget
//! is a test nobody runs.

use std::collections::{HashMap, VecDeque};
use std::future::{Future, poll_fn};
use std::path::{Path, PathBuf};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime};

use thiserror::Error;
use tokio::sync::Mutex;

use crate::cache::{CacheKey, ResponseCache};
use crate::client::HttpClient;
use crate::destination::Destination;
use crate::egress::{BROKER_EGRESS_ENV, BrokerEgress, egress_directory_for};
use crate::request::{HttpRequest, Secret};
use crate::resilience::{MAX_NAMED_WAIT, Outcome, Retry, RetryPolicy, is_transient};
use crate::response::{HttpError, HttpResponse};
use crate::tally::{
    ClosureReason, DAILY_CEILING, EgressDirectory, EndpointOwner, OutboundTally, TALLY_RETENTION,
    TallyDecision, TallyError, TallyResponseDecision,
};

/// The window every per-minute limit below is stated over.
const MINUTE: Duration = Duration::from_secs(60);

/// Attempts per broker call, the first included.
pub const ATTEMPTS: u32 = 3;

/// Attempts per non-broker call, preserving the established MOEX and CBR policy.
const NON_BROKER_ATTEMPTS: u32 = 5;

/// The wait after the first failed attempt; it doubles after each further one
/// up to `resilience::MAX_BACKOFF`.
pub const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// Calls in a row that must fail, after their retries, to open the breaker.
pub const BREAKER_FAILURES: u32 = 5;

/// How long an open breaker refuses calls without sending them. Long enough
/// for a broker's maintenance window or a lifted ban, short enough that a
/// daily synchronisation still finishes on the same run.
pub const BREAKER_COOL_DOWN: Duration = Duration::from_secs(5 * 60);

/// The shortest wait logged. A wait up to it is the ordinary pace of a
/// call; a longer one is what an operator watching a slow synchronisation
/// needs to see the reason of.
const LOGGED_WAIT: Duration = Duration::from_secs(1);

/// Which method keys a budget row covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodScope {
    /// Exactly this method key, with its own budget. A key no row names is
    /// refused: a key invented by a caller would otherwise be a fresh budget.
    Named(&'static str),
    /// Every method key, all drawing on one budget. The pacing the market
    /// sources had before the gateway was per source, not per method.
    Shared,
}

/// One row of the budget table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub destination: Destination,
    pub scope: MethodScope,
    /// The limit the destination documents over `window`, where it documents
    /// one. Kept beside the used value so a reader sees the margin taken.
    pub documented: Option<u32>,
    /// Requests this process allows itself over `window`.
    pub used: u32,
    pub window: Duration,
}

/// The documented limits and the share of each this process uses.
///
/// Half of every documented limit: the documented number is the point at
/// which the destination starts refusing, and the owner's other tools (a
/// terminal, a mobile app) spend from the same account's allowance.
pub const BUDGETS: &[Budget] = &[
    // T-Invest REST, OperationsService: documented 100/min, use 50/min.
    Budget {
        destination: Destination::TinkoffProd,
        scope: MethodScope::Named("OperationsService"),
        documented: Some(100),
        used: 50,
        window: MINUTE,
    },
    // T-Invest REST, UsersService: documented 50/min, use 25/min.
    Budget {
        destination: Destination::TinkoffProd,
        scope: MethodScope::Named("UsersService"),
        documented: Some(50),
        used: 25,
        window: MINUTE,
    },
    // The sandbox is a separate host with the same published limits.
    Budget {
        destination: Destination::TinkoffSandbox,
        scope: MethodScope::Named("OperationsService"),
        documented: Some(100),
        used: 50,
        window: MINUTE,
    },
    Budget {
        destination: Destination::TinkoffSandbox,
        scope: MethodScope::Named("UsersService"),
        documented: Some(50),
        used: 25,
        window: MINUTE,
    },
    // Finam states its limit per method, 200/min each; use 100/min. One row
    // per method called, so a new call needs a row before it can be sent.
    Budget {
        destination: Destination::FinamApi,
        scope: MethodScope::Named("AccountsService.GetAccount"),
        documented: Some(200),
        used: 100,
        window: MINUTE,
    },
    Budget {
        destination: Destination::FinamApi,
        scope: MethodScope::Named("AccountsService.Transactions"),
        documented: Some(200),
        used: 100,
        window: MINUTE,
    },
    Budget {
        destination: Destination::FinamApi,
        scope: MethodScope::Named("AssetsService.GetAsset"),
        documented: Some(200),
        used: 100,
        window: MINUTE,
    },
    // The exchange of the secret for a session JWT, which Finam calls next.
    Budget {
        destination: Destination::FinamApi,
        scope: MethodScope::Named("AuthService.Sessions"),
        documented: Some(200),
        used: 100,
        window: MINUTE,
    },
    // MOEX ISS and the CBR document no limit. The pacing the market adapter
    // used before the gateway — one request per 100 ms — is kept as it was.
    Budget {
        destination: Destination::MoexIss,
        scope: MethodScope::Shared,
        documented: None,
        used: 1,
        window: Duration::from_millis(100),
    },
    Budget {
        destination: Destination::CbrScripts,
        scope: MethodScope::Shared,
        documented: None,
        used: 1,
        window: Duration::from_millis(100),
    },
    Budget {
        destination: Destination::CbrDailyInfo,
        scope: MethodScope::Shared,
        documented: None,
        used: 1,
        window: Duration::from_millis(100),
    },
    // The published T-Invest contract on raw.githubusercontent.com documents
    // no limit. It is read rarely and by hand, and one request a second
    // cannot load anybody; a destination with no row is refused outright.
    Budget {
        destination: Destination::TinvestContract,
        scope: MethodScope::Shared,
        documented: None,
        used: 1,
        window: Duration::from_secs(1),
    },
];

/// One reading of Linux's boot clock and the boot it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootTime {
    id: String,
    elapsed: Duration,
}

impl BootTime {
    #[must_use]
    pub fn new(id: impl Into<String>, elapsed: Duration) -> Self {
        Self {
            id: id.into(),
            elapsed,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// Sources in-process monotonic time and persistent Linux boot time.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn now_boot(&self) -> Result<BootTime, String>;
    /// Wall-clock time for a record that must outlive the process. The
    /// response cache stamps its entries with it, so an answer survives a
    /// restart and expires an hour later whatever the boot was. The
    /// system clock answers; a test clock moves it with its own offset.
    fn now_unix(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Waits out a delay.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;
fn require_host_boottime(offsets: &str) -> Result<(), String> {
    let mut found = false;
    for line in offsets.lines() {
        let mut fields = line.split_whitespace();
        let Some(clock) = fields.next() else {
            continue;
        };
        let Some(seconds) = fields.next() else {
            return Err(format!("malformed time namespace offset: {line:?}"));
        };
        let Some(nanos) = fields.next() else {
            return Err(format!("malformed time namespace offset: {line:?}"));
        };
        if fields.next().is_some() {
            return Err(format!("malformed time namespace offset: {line:?}"));
        }
        if clock == "boottime" {
            found = true;
            if seconds != "0" || nanos != "0" {
                return Err(format!(
                    "broker egress refuses a time namespace with boottime offset {seconds} {nanos}"
                ));
            }
        }
    }
    found
        .then_some(())
        .ok_or_else(|| "/proc/self/timens_offsets has no boottime record".to_owned())
}

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn now_boot(&self) -> Result<BootTime, String> {
        let offsets = std::fs::read_to_string("/proc/self/timens_offsets")
            .map_err(|error| format!("read /proc/self/timens_offsets: {error}"))?;
        require_host_boottime(&offsets)?;
        let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|error| format!("read /proc/sys/kernel/random/boot_id: {error}"))?;
        let id = id.trim();
        if id.is_empty() {
            return Err("/proc/sys/kernel/random/boot_id is empty".to_owned());
        }
        #[cfg(target_os = "linux")]
        {
            let reading = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
            let seconds = u64::try_from(reading.tv_sec)
                .map_err(|_| "CLOCK_BOOTTIME returned negative seconds".to_owned())?;
            let nanos = u32::try_from(reading.tv_nsec)
                .map_err(|_| "CLOCK_BOOTTIME returned invalid nanoseconds".to_owned())?;
            Ok(BootTime::new(id, Duration::new(seconds, nanos)))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(
                "CLOCK_BOOTTIME is required and this operating system does not provide it"
                    .to_owned(),
            )
        }
    }
}

/// A sleeper on the tokio timer the application already runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(delay))
    }
}

/// What the gateway sends through. `HttpClient` in production; scripted
/// endpoints in gateway tests.
pub trait Transport: Send + Sync {
    fn send<'a>(
        &'a self,
        request: &'a HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a;

    /// Send while exposing the transport hand-off and the status line —
    /// status, named wait and the headers an operator reads the call by —
    /// before the body is consumed.
    ///
    /// Scripted transports get the safe default. `HttpClient` overrides it so
    /// the hand-off is acknowledged after request construction and immediately
    /// before execution, and a broker stop signal is persisted from the status
    /// line even when the body later stalls or fails.
    fn send_observed<'a>(
        &'a self,
        request: &'a HttpRequest,
        handoff: Box<dyn FnOnce() -> Result<(), HttpError> + Send + 'a>,
        observe: Box<dyn FnOnce(HttpResponse) + Send + 'a>,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        async move {
            handoff()?;
            let response = self.send(request).await?;
            let mut observed = response.clone();
            observed.body = Vec::new();
            observe(observed);
            Ok(response)
        }
    }
}

impl Transport for HttpClient {
    fn send<'a>(
        &'a self,
        request: &'a HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        Self::send(self, request)
    }

    fn send_observed<'a>(
        &'a self,
        request: &'a HttpRequest,
        handoff: Box<dyn FnOnce() -> Result<(), HttpError> + Send + 'a>,
        observe: Box<dyn FnOnce(HttpResponse) + Send + 'a>,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        Self::send_observed(self, request, handoff, observe)
    }
}

/// The gateway as a caller holds it.
///
/// Object-safe, so an adapter stores `Arc<dyn Outbound>` whatever the
/// gateway's transport is: the production one and a test's scripted one are
/// one type to it, and no generic climbs through the layers above.
pub trait Outbound: Send + Sync {
    /// `Gateway::send`, boxed.
    fn send<'a>(
        &'a self,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>>;
}

impl<T: Transport + 'static> Outbound for Gateway<T> {
    fn send<'a>(
        &'a self,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>> {
        Box::pin(Self::send(self, request, deadline))
    }
}

/// Refusal from the gateway.
///
/// Carries statuses, counts and delays, plus the three destination-authored
/// response headers an operator reads a refusal by (`Rejected`'s `location`,
/// `content_type` and `request_id`): the response body is boxed in
/// `RejectedBody`, whose `Debug` prints its length only, and no field
/// carries the presented secret, so it can be logged as it is.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("no budget is recorded for path {path:?} at {destination:?}")]
    UnknownBudget {
        destination: Destination,
        path: String,
    },
    #[error("the budget table is invalid: {0}")]
    InvalidBudgets(String),
    /// `Gateway::production` was called before in this process.
    #[error(
        "a process has one gateway and this one has built it already: a second would duplicate its in-process lanes, waits and breakers (docs/deployment.md §1.1)"
    )]
    SecondGateway,
    /// The process explicitly disabled every broker destination.
    #[error(
        "broker egress is disabled by {BROKER_EGRESS_ENV}; set it to `on` before sending broker requests, and the tally is found beside this instance's database"
    )]
    BrokerEgressOff,
    /// The instance's database could not be resolved, so the egress directory
    /// that holds the broker tally cannot be placed beside it.
    #[error(
        "the egress directory could not be placed beside the database {database}: {reason}",
        database = .database.display()
    )]
    EgressPlace { database: PathBuf, reason: String },
    /// A gateway built without the instance's database cannot open the broker
    /// tally, which lives in a directory derived from that database.
    #[error(
        "broker egress is `on`, but the broker tally lives beside the instance's database and this constructor takes no database; build the gateway with `Gateway::production(broker_egress, database)` instead"
    )]
    BrokerEgressWithoutDatabase,
    /// The tally could not be opened, locked, read, written or persisted.
    #[error(
        "outbound tally {path} is unavailable: {reason}; restore access to that path before retrying",
        path = .path.display()
    )]
    TallyUnavailable { path: PathBuf, reason: String },
    /// The tally's versioned records could not be parsed.
    #[error(
        "outbound tally {path} is corrupt: {reason}; restore valid contents at that path before retrying",
        path = .path.display()
    )]
    TallyCorrupt { path: PathBuf, reason: String },
    /// The instance's response cache could not be placed or created beside
    /// the instance's database, so reads would be sent again every time.
    #[error(
        "the response cache for the database {database} could not be made: {reason}; the cache lives beside the database like its egress directory, and the process needs leave to create it",
        database = .database.display()
    )]
    CacheUnavailable { database: PathBuf, reason: String },
    /// Another process owns this endpoint's lifetime lock.
    #[error(
        "broker endpoint {endpoint} is owned by another iaam process; use that process or stop it before retrying"
    )]
    BrokerEndpointOwned {
        destination: Destination,
        endpoint: &'static str,
    },
    /// This host has spent its rolling 24-hour allowance.
    #[error(
        "{destination:?} reached the daily ceiling of {ceiling} broker requests; it resets at {resets_at}"
    )]
    DailyCeiling {
        destination: Destination,
        ceiling: u32,
        resets_at: String,
        retry_after: Duration,
    },
    /// A broker host is under a persisted pause after a 429 response.
    #[error(
        "broker host {host} is paused after status 429 and reopens at {reopens_at} ({attempts} attempts sent); retry then"
    )]
    BrokerHostPaused {
        destination: Destination,
        host: &'static str,
        reopens_at: String,
        retry_after: Duration,
        attempts: u32,
    },
    /// A broker host is under a persisted closure after repeated refusals.
    #[error(
        "broker host {host} is closed after {reason} and reopens at {reopens_at} ({attempts} attempts sent); retry then"
    )]
    BrokerHostClosed {
        destination: Destination,
        host: &'static str,
        reason: &'static str,
        reopens_at: String,
        retry_after: Duration,
        status: Option<u16>,
        attempts: u32,
    },
    /// A broker request did not carry the sync-scoped allowance. Broker
    /// clients have no uncounted path: every transport attempt must belong to
    /// one sync.
    #[error(
        "{destination:?} broker request has no sync request allowance; open the request through a broker sync"
    )]
    MissingRequestAllowance { destination: Destination },
    /// This sync has sent every transport attempt it is allowed to send.
    #[error(
        "{destination:?} request allowance reached its ceiling of {ceiling} attempts; the range is too long for one sync; narrow it and sync again"
    )]
    RequestCeiling {
        destination: Destination,
        ceiling: u32,
    },
    /// Every attempt failed transiently. Worth trying again after
    /// `retry_after`.
    #[error(
        "{destination:?} failed {attempts} attempts (last status {status:?}); retry after {retry_after:?}"
    )]
    Exhausted {
        destination: Destination,
        /// The last status; `None` when the last attempt got no response.
        status: Option<u16>,
        attempts: u32,
        retry_after: Duration,
    },
    /// The next attempt, or the wait before it, would have crossed the
    /// caller's deadline, so the call stopped without it.
    #[error(
        "{destination:?}: deadline reached after {attempts} attempts (last status {status:?}); the next could start in {retry_after:?}"
    )]
    DeadlineReached {
        destination: Destination,
        /// The last status; `None` when no attempt got a response.
        status: Option<u16>,
        attempts: u32,
        /// The wait that did not fit before the deadline.
        retry_after: Duration,
    },
    /// The destination failed too many calls in a row; nothing was sent.
    #[error(
        "{destination:?} is refused for {retry_after:?} after repeated failures ({attempts} attempts sent)"
    )]
    CircuitOpen {
        destination: Destination,
        /// Attempts this call had sent before the breaker refused it.
        attempts: u32,
        retry_after: Duration,
    },
    /// The destination named a wait longer than the gateway holds a caller
    /// (`resilience::MAX_NAMED_WAIT`), so the call returned instead of
    /// waiting it out.
    #[error(
        "{destination:?} asked not to be called for {retry_after:?} (last status {status:?}, {attempts} attempts sent)"
    )]
    Deferred {
        destination: Destination,
        /// The last status; `None` when this call sent nothing.
        status: Option<u16>,
        attempts: u32,
        retry_after: Duration,
    },
    /// The destination answered with a status a retry would only repeat.
    #[error("{destination:?} refused the request with status {status} after {attempts} attempts")]
    Rejected {
        destination: Destination,
        status: u16,
        attempts: u32,
        body: RejectedBody,
        /// Where a 3xx refusal points, as the destination sent it, cut of
        /// the presented secret.
        location: Option<String>,
        /// The `Content-Type` the destination named for the refused body.
        content_type: Option<String>,
        /// The request id the destination answered with, cut of the
        /// presented secret.
        request_id: Option<String>,
    },
    /// The transport could not be set up; retrying would meet the same fault.
    #[error("{destination:?} could not be reached after {attempts} attempts: {error}")]
    Transport {
        destination: Destination,
        error: HttpError,
        attempts: u32,
    },
}

impl GatewayError {
    /// Whether the same call may succeed later.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        self.retry_after().is_some()
    }

    /// The refusal's name in the log, for the transient ones logged.
    const fn kind(&self) -> &'static str {
        match self {
            Self::Exhausted { .. } => "exhausted",
            Self::DeadlineReached { .. } => "deadline",
            Self::CircuitOpen { .. } => "circuit open",
            Self::Deferred { .. } => "deferred",
            Self::DailyCeiling { .. } => "daily ceiling",
            Self::BrokerHostPaused { .. } => "broker host paused",
            Self::BrokerHostClosed { .. } => "broker host closed",
            Self::MissingRequestAllowance { .. } => "missing request allowance",
            Self::RequestCeiling { .. } => "request ceiling",
            Self::BrokerEgressOff => "broker egress off",
            Self::EgressPlace { .. } => "egress place",
            Self::BrokerEgressWithoutDatabase => "broker egress without database",
            Self::TallyUnavailable { .. } => "tally unavailable",
            Self::TallyCorrupt { .. } => "tally corrupt",
            Self::CacheUnavailable { .. } => "cache unavailable",
            Self::BrokerEndpointOwned { .. } => "broker endpoint owned",
            Self::UnknownBudget { .. } => "unknown budget",
            Self::InvalidBudgets(_) => "invalid budgets",
            Self::SecondGateway => "second gateway",
            Self::Rejected { .. } => "rejected",
            Self::Transport { .. } => "transport",
        }
    }

    /// When a transient refusal is worth trying again; `None` for a
    /// permanent one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Exhausted { retry_after, .. }
            | Self::DeadlineReached { retry_after, .. }
            | Self::CircuitOpen { retry_after, .. }
            | Self::Deferred { retry_after, .. }
            | Self::DailyCeiling { retry_after, .. }
            | Self::BrokerHostPaused { retry_after, .. }
            | Self::BrokerHostClosed { retry_after, .. } => Some(*retry_after),
            Self::BrokerEgressOff
            | Self::EgressPlace { .. }
            | Self::BrokerEgressWithoutDatabase
            | Self::TallyUnavailable { .. }
            | Self::TallyCorrupt { .. }
            | Self::CacheUnavailable { .. }
            | Self::BrokerEndpointOwned { .. }
            | Self::UnknownBudget { .. }
            | Self::InvalidBudgets(_)
            | Self::SecondGateway
            | Self::MissingRequestAllowance { .. }
            | Self::RequestCeiling { .. }
            | Self::Rejected { .. }
            | Self::Transport { .. } => None,
        }
    }

    /// The last status the destination answered with, if it answered.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        match self {
            Self::Exhausted { status, .. }
            | Self::DeadlineReached { status, .. }
            | Self::Deferred { status, .. }
            | Self::BrokerHostClosed { status, .. } => *status,
            Self::BrokerHostPaused { .. } => Some(429),
            Self::Rejected { status, .. } => Some(*status),
            Self::BrokerEgressOff
            | Self::EgressPlace { .. }
            | Self::BrokerEgressWithoutDatabase
            | Self::TallyUnavailable { .. }
            | Self::TallyCorrupt { .. }
            | Self::CacheUnavailable { .. }
            | Self::BrokerEndpointOwned { .. }
            | Self::DailyCeiling { .. }
            | Self::UnknownBudget { .. }
            | Self::InvalidBudgets(_)
            | Self::SecondGateway
            | Self::MissingRequestAllowance { .. }
            | Self::RequestCeiling { .. }
            | Self::CircuitOpen { .. }
            | Self::Transport { .. } => None,
        }
    }

    /// Requests actually sent for this call.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        match self {
            Self::Exhausted { attempts, .. }
            | Self::DeadlineReached { attempts, .. }
            | Self::CircuitOpen { attempts, .. }
            | Self::Deferred { attempts, .. }
            | Self::BrokerHostPaused { attempts, .. }
            | Self::BrokerHostClosed { attempts, .. }
            | Self::Rejected { attempts, .. }
            | Self::Transport { attempts, .. } => *attempts,
            Self::BrokerEgressOff
            | Self::EgressPlace { .. }
            | Self::BrokerEgressWithoutDatabase
            | Self::TallyUnavailable { .. }
            | Self::TallyCorrupt { .. }
            | Self::CacheUnavailable { .. }
            | Self::BrokerEndpointOwned { .. }
            | Self::DailyCeiling { .. }
            | Self::UnknownBudget { .. }
            | Self::InvalidBudgets(_)
            | Self::SecondGateway
            | Self::MissingRequestAllowance { .. }
            | Self::RequestCeiling { .. } => 0,
        }
    }

    /// Whether this refusal came from the broker-egress switch or tally.
    #[must_use]
    pub const fn is_broker_egress_refusal(&self) -> bool {
        matches!(
            self,
            Self::BrokerEgressOff
                | Self::EgressPlace { .. }
                | Self::BrokerEgressWithoutDatabase
                | Self::TallyUnavailable { .. }
                | Self::TallyCorrupt { .. }
                | Self::BrokerEndpointOwned { .. }
                | Self::DailyCeiling { .. }
                | Self::BrokerHostPaused { .. }
                | Self::BrokerHostClosed { .. }
        )
    }
}

/// The body of a rejected request.
///
/// Kept for the source, which alone knows what a broker's refusal body means
/// (a T-Invest error code, say). `Debug` prints its length only: a refusal is
/// logged, and a body may carry the owner's data. The request's bearer secret
/// is cut out before the body is kept: a destination that echoes the token
/// back would otherwise hand it to every reader of the error.
#[derive(Clone, PartialEq, Eq)]
pub struct RejectedBody(Vec<u8>);

/// What stands in a rejected body or header where the bearer secret was.
const REDACTED_TEXT: &str = "<redacted>";

const REDACTED: &[u8] = REDACTED_TEXT.as_bytes();

/// The response headers a refused line reads back: where a redirect
/// points, the body's type, and the request id the destination named.
///
/// Each is cut of the request's bearer secret at capture: a destination
/// that echoes the presented token back in a header would otherwise hand
/// it to every reader of the log. Carries no body and no status: the
/// rejection itself does.
#[derive(Debug, Clone, Default)]
struct RefusalHeaders {
    location: Option<String>,
    content_type: Option<String>,
    request_id: Option<String>,
}

impl RefusalHeaders {
    fn of(response: &HttpResponse, secret: Option<&Secret>) -> Self {
        Self {
            location: without_secret(response.location.as_deref(), secret),
            content_type: without_secret(response.content_type.as_deref(), secret),
            request_id: without_secret(response.request_id.as_deref(), secret),
        }
    }
}

/// `value` with every occurrence of the presented secret cut out, or
/// `None` when the header is absent. No secret, or an empty one, leaves
/// the value as it was. The cache uses the same redaction before it
/// persists a header, so a source that echoes the token cannot hand it
/// to a later reader of the cache file either.
pub(crate) fn without_secret(value: Option<&str>, secret: Option<&Secret>) -> Option<String> {
    let value = value?;
    let Some(secret) = secret.map(Secret::expose) else {
        return Some(value.to_owned());
    };
    if secret.is_empty() {
        return Some(value.to_owned());
    }
    Some(value.replace(secret, REDACTED_TEXT))
}

/// `bytes` with every occurrence of the presented secret cut out, else
/// the bytes as they were. No secret, or an empty one, leaves the bytes
/// as they were. The one byte-level redaction of the crate: a refusal
/// body kept for the source and a response body stored beside the
/// database both pass through here, so a source that echoes the token
/// back cannot hand it to a later reader of either.
pub(crate) fn redacted_bytes(bytes: &[u8], secret: Option<&Secret>) -> Vec<u8> {
    let secret = secret.map_or(&[][..], |secret| secret.expose().as_bytes());
    if secret.is_empty() {
        return bytes.to_vec();
    }
    let mut kept = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&first, tail)) = rest.split_first() {
        if let Some(after) = rest.strip_prefix(secret) {
            kept.extend_from_slice(REDACTED);
            rest = after;
        } else {
            kept.push(first);
            rest = tail;
        }
    }
    kept
}

impl RejectedBody {
    fn without_secret(body: &[u8], secret: Option<&Secret>) -> Self {
        Self(redacted_bytes(body, secret))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for RejectedBody {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RejectedBody(<{} bytes>)", self.0.len())
    }
}

/// The budget table, checked once at construction.
#[derive(Debug, Clone)]
pub struct BudgetTable {
    rows: &'static [Budget],
}

impl BudgetTable {
    /// Check the rows: a budget of zero requests, or of a zero window, is a
    /// missing input rather than "unlimited" or "never"; a row above its
    /// documented limit defeats the reason the row exists; and two rows for
    /// the same key leave which one applies to the order of the table.
    ///
    /// # Errors
    /// `GatewayError::InvalidBudgets` naming the first offending row.
    pub fn new(rows: &'static [Budget]) -> Result<Self, GatewayError> {
        for (index, row) in rows.iter().enumerate() {
            if row.used == 0 || row.window.is_zero() {
                return Err(GatewayError::InvalidBudgets(format!(
                    "{:?} {:?} allows no requests",
                    row.destination, row.scope
                )));
            }
            if row
                .documented
                .is_some_and(|documented| row.used > documented)
            {
                return Err(GatewayError::InvalidBudgets(format!(
                    "{:?} {:?} uses more than its documented limit",
                    row.destination, row.scope
                )));
            }
            if is_broker_destination(row.destination) && row.window > TALLY_RETENTION {
                return Err(GatewayError::InvalidBudgets(format!(
                    "{:?} {:?} has a window longer than the outbound tally retention",
                    row.destination, row.scope
                )));
            }
            if rows[..index]
                .iter()
                .any(|earlier| earlier.destination == row.destination && earlier.scope == row.scope)
            {
                return Err(GatewayError::InvalidBudgets(format!(
                    "{:?} {:?} is listed twice",
                    row.destination, row.scope
                )));
            }
        }
        Ok(Self { rows })
    }

    /// The row covering the request path, and the key its budget is kept under.
    ///
    /// A broker caller supplies no independent key: destination and path are
    /// the wire identity. Non-broker destinations retain their shared row.
    fn lookup(
        &self,
        request: &HttpRequest,
    ) -> Result<(Budget, Option<&'static str>), GatewayError> {
        let destination = request.destination();
        let key = broker_budget_key(destination, request.path());
        let wanted = key.map_or(MethodScope::Shared, MethodScope::Named);
        let row = self
            .rows
            .iter()
            .find(|row| row.destination == destination && row.scope == wanted);
        row.map(|row| (*row, key))
            .ok_or_else(|| GatewayError::UnknownBudget {
                destination,
                path: request.path().to_owned(),
            })
    }
}

fn broker_budget_key(destination: Destination, path: &str) -> Option<&'static str> {
    match destination {
        Destination::TinkoffProd | Destination::TinkoffSandbox => {
            match path.trim_start_matches('/') {
                "tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts"
                | "UsersService/GetAccounts" => Some("UsersService"),
                "tinkoff.public.invest.api.contract.v1.OperationsService/GetPortfolio"
                | "tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor"
                | "OperationsService/GetPortfolio"
                | "OperationsService/GetOperationsByCursor" => Some("OperationsService"),
                _ => None,
            }
        }
        Destination::FinamApi => {
            let path = path.trim_start_matches('/');
            if path.bytes().any(|byte| byte.is_ascii_control())
                || path.contains(['?', '#', '\\', '%'])
                || path.split('/').any(|segment| matches!(segment, "." | ".."))
            {
                return None;
            }
            if matches!(path, "v1/sessions" | "v1/sessions/details") {
                return Some("AuthService.Sessions");
            }
            if let Some(asset) = path.strip_prefix("v1/assets/") {
                return (!asset.is_empty() && !asset.contains('/'))
                    .then_some("AssetsService.GetAsset");
            }
            let account_path = path.strip_prefix("v1/accounts/")?;
            if let Some(account) = account_path.strip_suffix("/transactions") {
                return (!account.is_empty() && !account.contains('/'))
                    .then_some("AccountsService.Transactions");
            }
            (!account_path.is_empty() && !account_path.contains('/'))
                .then_some("AccountsService.GetAccount")
        }
        Destination::MoexIss
        | Destination::CbrScripts
        | Destination::CbrDailyInfo
        | Destination::TinvestContract => None,
    }
}

#[derive(Debug, Clone, Copy)]
enum TallyLatch {
    Unknown,
    Response {
        status: u16,
        retry_after: Option<Duration>,
    },
}

/// What the gateway remembers about one host.
///
/// Kept per host rather than per destination: the host is what sees the
/// load, and `CbrScripts` and `CbrDailyInfo` are one host. A shared budget
/// key of either therefore draws on one log, and a failure of either counts
/// towards one breaker.
#[derive(Debug, Default)]
struct Lane {
    /// Start instants of the most recent requests, per budget key: at most
    /// `used` of them, the oldest first.
    sent: HashMap<Option<&'static str>, VecDeque<Instant>>,
    /// Calls in a row that failed transiently after all their attempts.
    failures: u32,
    /// Until when the breaker refuses calls, once it has opened.
    open_until: Option<Instant>,
    /// Until when the destination asked not to be called: a `Retry-After` or
    /// reset it named. Every budget key of the host lives in this lane, so
    /// the one instant holds back every caller of the lane and of each key,
    /// including one that arrives after the call that learned it returned.
    not_before: Option<Instant>,
    /// The endpoint owner lock. Broker decisions, transport and response-status
    /// recording all run while this lane is held, so the next decision cannot
    /// overtake the previous response.
    owner: Option<EndpointOwner>,
    /// A tally write failed after handoff. The observed response is retained
    /// until that exact status and delay, or a conservative unknown outcome,
    /// has been durably written.
    tally_latched: Option<TallyLatch>,
    /// Set while a detached attempt unwinds. Its drop guard publishes the
    /// failure before releasing this lane, so the next holder durably adopts
    /// the unresolved attempt instead of letting its short expiry send.
    detached_attempt_failed: Arc<AtomicBool>,
}

struct DetachedAttemptGuard {
    failed: Arc<AtomicBool>,
    armed: bool,
}

impl DetachedAttemptGuard {
    fn new(lane: &Lane) -> Self {
        Self {
            failed: Arc::clone(&lane.detached_attempt_failed),
            armed: true,
        }
    }

    fn complete(mut self) {
        self.armed = false;
    }
}

impl Drop for DetachedAttemptGuard {
    fn drop(&mut self) {
        if self.armed {
            self.failed.store(true, Ordering::SeqCst);
        }
    }
}

impl Lane {
    /// How long a request under `key` must wait before it may start.
    ///
    /// A log of the last `used` start instants rather than a refilling token
    /// bucket: a bucket that starts full lets a burst of `used` through and
    /// refills `used` more within the same window, so it can send nearly
    /// twice the budget in one window. The log cannot: the next request
    /// starts no earlier than one window after the request `used` places
    /// before it.
    fn budget_wait(&self, key: Option<&'static str>, budget: &Budget, now: Instant) -> Duration {
        let Some(sent) = self.sent.get(&key) else {
            return Duration::ZERO;
        };
        match sent.front() {
            Some(oldest) if sent.len() >= budget.used as usize => {
                (*oldest + budget.window).saturating_duration_since(now)
            }
            _ => Duration::ZERO,
        }
    }

    /// How long the breaker still refuses calls; `None` when it lets them
    /// through.
    ///
    /// Once the cool-down is over the failure count is left as it was, so the
    /// first call through decides: a success closes the breaker, and a
    /// failure opens it again at once rather than after five more.
    fn refusing(&self, now: Instant) -> Option<Duration> {
        self.open_until
            .and_then(|until| until.checked_duration_since(now))
            .filter(|left| !left.is_zero())
    }

    /// How long the destination's named reset still holds calls back.
    fn named_wait(&self, now: Instant) -> Duration {
        self.not_before
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(now))
    }

    /// Remember a reset the destination named; an earlier one it named
    /// before does not shorten a later one.
    fn hold_until(&mut self, until: Instant) {
        self.not_before = self.not_before.max(Some(until));
    }

    /// Count a failed call; `true` when it opened the breaker.
    fn record_failure(&mut self, now: Instant) -> bool {
        self.failures = self.failures.saturating_add(1);
        let opens = self.failures >= BREAKER_FAILURES;
        if opens {
            self.open_until = Some(now + BREAKER_COOL_DOWN);
        }
        opens
    }

    /// Count a success; `true` when it closed a breaker that had opened.
    fn record_success(&mut self) -> bool {
        self.failures = 0;
        self.open_until.take().is_some()
    }

    fn record_start(&mut self, key: Option<&'static str>, budget: &Budget, at: Instant) {
        let sent = self.sent.entry(key).or_default();
        sent.push_back(at);
        while sent.len() > budget.used as usize {
            sent.pop_front();
        }
    }
}

/// A wait in whole milliseconds, as the log carries it.
fn millis(wait: Duration) -> u64 {
    u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)
}

const fn is_broker_destination(destination: Destination) -> bool {
    match destination {
        Destination::TinkoffProd | Destination::TinkoffSandbox | Destination::FinamApi => true,
        Destination::MoexIss
        | Destination::CbrScripts
        | Destination::CbrDailyInfo
        | Destination::TinvestContract => false,
    }
}
const fn endpoint_lock_name(destination: Destination) -> Option<&'static str> {
    match destination {
        Destination::TinkoffProd => Some("tinkoff-prod"),
        Destination::TinkoffSandbox => Some("tinkoff-sandbox"),
        Destination::FinamApi => Some("finam"),
        Destination::MoexIss
        | Destination::CbrScripts
        | Destination::CbrDailyInfo
        | Destination::TinvestContract => None,
    }
}

fn tally_gateway_error(destination: Destination, path: &Path, error: TallyError) -> GatewayError {
    match error {
        TallyError::Corrupt(reason) => GatewayError::TallyCorrupt {
            path: path.to_owned(),
            reason,
        },
        TallyError::EndpointOwned { endpoint } => GatewayError::BrokerEndpointOwned {
            destination,
            endpoint,
        },
        other => GatewayError::TallyUnavailable {
            path: path.to_owned(),
            reason: other.to_string(),
        },
    }
}

/// Whether this process has built its production gateway.
static PRODUCTION_BUILT: AtomicBool = AtomicBool::new(false);

/// The outbound gateway.
///
/// Built once per server process and shared: non-broker pacing, named waits
/// and breakers are state of this value. Broker accounting and each endpoint's
/// lifetime owner instead belong to the tally in the directory
/// [`egress_directory_for`] derives from the instance's database.
pub struct Gateway<T> {
    transport: Arc<T>,
    budgets: BudgetTable,
    retry: RetryPolicy,
    non_broker_retry: RetryPolicy,
    clock: Arc<dyn Clock>,
    sleeper: Arc<dyn Sleeper>,
    broker_egress: BrokerEgress,
    egress_directory: Option<EgressDirectory>,
    /// The response cache beside the instance's database: carried by the
    /// production gateway alone, and by the test seam that builds what
    /// production builds. A gateway built without a database has none.
    cache: Option<ResponseCache>,
    /// Keyed by `Destination::base_url`, the host a request reaches.
    lanes: HashMap<&'static str, Arc<Mutex<Lane>>>,
}

impl Gateway<HttpClient> {
    /// The production gateway over the real transport: the one way a process
    /// gets it, because `HttpClient` cannot be built outside this crate. Each
    /// executable calls it once and shares the result.
    ///
    /// A second call in the same process is refused with
    /// `GatewayError::SecondGateway`: the breakers and named waits are state
    /// of the value. Broker budgets live in the instance's tally, in the
    /// directory [`egress_directory_for`] derives from `database`: one
    /// database is one tally, so one iaam instance is one tally.
    ///
    /// The response cache lives beside the database too, whatever the broker
    /// egress switch says: a read this gateway answered within the hour is
    /// answered from the cache instead of being sent again.
    ///
    /// # Errors
    /// `GatewayError::SecondGateway` on every call after the first;
    /// `GatewayError::InvalidBudgets` when the table in this module is wrong;
    /// `GatewayError::EgressPlace` when the database cannot be resolved,
    /// `GatewayError::TallyUnavailable` when the derived place is unusable,
    /// and `GatewayError::CacheUnavailable` when the cache cannot be placed
    /// or created beside the database.
    pub fn production(broker_egress: BrokerEgress, database: &Path) -> Result<Self, GatewayError> {
        // Claimed before building: two threads racing here must not both
        // see the flag clear. A table that fails its checks fails the same
        // way on a second call, so nothing is lost by not releasing it.
        if PRODUCTION_BUILT.swap(true, Ordering::SeqCst) {
            return Err(GatewayError::SecondGateway);
        }
        Self::with_parts_for_database_with_response_cache(
            HttpClient::new(),
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            broker_egress,
            database,
        )
    }
}

/// Mint the fresh zero tally of the instance's first broker enabling, beside
/// its database.
///
/// The one writer is `iaam broker connect`'s first enabling: the owner said
/// this instance talks to a broker, the pair beside the database does not
/// exist yet (or is the empty pair a process creates when it finds the place
/// missing), and without a fresh pair the very first checking call would sit
/// at the conservative daily ceiling for a day. See
/// `OutboundTally::initialize_fresh_pair` for what freshness proves. A pair
/// that already holds anything governs: the answer is `false` and nothing is
/// written, so running `connect` again can never restore an allowance.
///
/// # Errors
/// `GatewayError::EgressPlace` when the database cannot be resolved;
/// `GatewayError::TallyUnavailable` when the pair cannot be opened or
/// persisted.
pub fn initialize_fresh_tally(database: &Path, clock: &dyn Clock) -> Result<bool, GatewayError> {
    let place = egress_directory_for(database).map_err(|error| GatewayError::EgressPlace {
        database: database.to_owned(),
        reason: error.to_string(),
    })?;
    let directory = EgressDirectory::open_or_create(&place).map_err(|error| {
        GatewayError::TallyUnavailable {
            path: place.join(crate::tally::TALLY_FILE),
            reason: error.to_string(),
        }
    })?;
    let tally = OutboundTally::new(directory.clone()).map_err(|error| {
        tally_gateway_error(Destination::FinamApi, &directory.tally_path(), error)
    })?;
    tally
        .initialize_fresh_pair(clock)
        .map_err(|error| tally_gateway_error(Destination::FinamApi, &directory.tally_path(), error))
}

impl<T: Transport + 'static> Gateway<T> {
    /// A gateway with the documented budgets, system clock and tokio timer
    /// over `transport`.
    ///
    /// This constructor takes no database, so it cannot open the broker tally
    /// that lives beside one: it is for non-broker use, and broker egress
    /// `on` is refused. To send broker requests, build the gateway with
    /// [`Gateway::production`] or the `*_for_database`/`*_in_directory`
    /// test seams, which take where the tally lives.
    ///
    /// # Errors
    /// `GatewayError::InvalidBudgets` when the table in this module is wrong;
    /// `GatewayError::BrokerEgressWithoutDatabase` when `broker_egress` is
    /// `BrokerEgress::On`.
    pub fn new(transport: T, broker_egress: BrokerEgress) -> Result<Self, GatewayError> {
        Self::with_parts(
            transport,
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            broker_egress,
        )
    }
    /// Test seam for the documented budgets in an isolated egress directory.
    #[doc(hidden)]
    pub fn new_in_directory(
        transport: T,
        broker_egress: BrokerEgress,
        directory: &Path,
    ) -> Result<Self, GatewayError> {
        Self::with_parts_in_directory(
            transport,
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            broker_egress,
            directory,
        )
    }
    /// Test seam for the documented budgets over an instance's database, the
    /// way [`Gateway::production`] finds its tally.
    ///
    /// # Errors
    /// As [`Self::with_parts_for_database`].
    #[doc(hidden)]
    pub fn new_for_database(
        transport: T,
        broker_egress: BrokerEgress,
        database: &Path,
    ) -> Result<Self, GatewayError> {
        Self::with_parts_for_database(
            transport,
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            broker_egress,
            database,
        )
    }

    /// A gateway over explicit parts. This constructor takes no database, so
    /// it cannot open the broker tally that lives beside one: broker egress
    /// `on` is refused, and broker use goes through
    /// [`Self::with_parts_for_database`] or [`Self::with_parts_in_directory`].
    ///
    /// # Errors
    /// `GatewayError::InvalidBudgets` when `budgets` fails its checks;
    /// `GatewayError::BrokerEgressWithoutDatabase` when `broker_egress` is
    /// `BrokerEgress::On`.
    pub fn with_parts(
        transport: T,
        budgets: &'static [Budget],
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
        broker_egress: BrokerEgress,
    ) -> Result<Self, GatewayError> {
        if broker_egress == BrokerEgress::On {
            return Err(GatewayError::BrokerEgressWithoutDatabase);
        }
        Self::assemble(transport, budgets, clock, sleeper, broker_egress, None)
    }

    /// A gateway over explicit parts whose broker tally lives where
    /// [`egress_directory_for`] places it: beside `database`, created when
    /// missing. This is the production placement. The response cache is not
    /// carried here — stands that replay one request many times build this
    /// constructor — and [`Self::with_parts_for_database_with_response_cache`]
    /// is the seam that adds it.
    ///
    /// # Errors
    /// `GatewayError::InvalidBudgets` when `budgets` fails its checks;
    /// `GatewayError::EgressPlace` when the database cannot be resolved;
    /// `GatewayError::TallyUnavailable` when the derived place is unsafe.
    pub fn with_parts_for_database(
        transport: T,
        budgets: &'static [Budget],
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
        broker_egress: BrokerEgress,
        database: &Path,
    ) -> Result<Self, GatewayError> {
        let egress_directory = if broker_egress == BrokerEgress::On {
            let place =
                egress_directory_for(database).map_err(|error| GatewayError::EgressPlace {
                    database: database.to_owned(),
                    reason: error.to_string(),
                })?;
            let directory = EgressDirectory::open_or_create(&place).map_err(|error| {
                GatewayError::TallyUnavailable {
                    path: place.join(crate::tally::TALLY_FILE),
                    reason: error.to_string(),
                }
            })?;
            OutboundTally::validate(&directory).map_err(|error| {
                tally_gateway_error(Destination::FinamApi, &directory.tally_path(), error)
            })?;
            Some(directory)
        } else {
            None
        };
        Self::assemble(
            transport,
            budgets,
            clock,
            sleeper,
            broker_egress,
            egress_directory,
        )
    }

    /// Test seam for everything [`Gateway::production`] builds — the
    /// documented budgets, the broker tally beside the instance's database
    /// and the response cache beside it — over the parts a test injects.
    /// [`Gateway::production`] is this over the real transport and the
    /// system clock; [`Self::with_parts_for_database`] stays cache-off for
    /// stands that replay one request many times.
    ///
    /// # Errors
    /// As [`Self::with_parts_for_database`], and
    /// `GatewayError::CacheUnavailable` when the cache cannot be placed or
    /// created beside the database.
    #[doc(hidden)]
    pub fn with_parts_for_database_with_response_cache(
        transport: T,
        budgets: &'static [Budget],
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
        broker_egress: BrokerEgress,
        database: &Path,
    ) -> Result<Self, GatewayError> {
        let gateway = Self::with_parts_for_database(
            transport,
            budgets,
            clock,
            sleeper,
            broker_egress,
            database,
        )?;
        gateway.with_response_cache_beside(database)
    }

    /// Carries the response cache derived from `database`, the way the
    /// production gateway does.
    ///
    /// # Errors
    /// `GatewayError::CacheUnavailable` when the cache place cannot be
    /// derived from the database or the directory cannot be created there.
    fn with_response_cache_beside(mut self, database: &Path) -> Result<Self, GatewayError> {
        let place = crate::cache::cache_directory_for(database).map_err(|error| {
            GatewayError::CacheUnavailable {
                database: database.to_owned(),
                reason: error.to_string(),
            }
        })?;
        let cache = ResponseCache::open(&place, Arc::clone(&self.clock)).map_err(|error| {
            GatewayError::CacheUnavailable {
                database: database.to_owned(),
                reason: error.to_string(),
            }
        })?;
        self.cache = Some(cache);
        Ok(self)
    }

    /// Test seam for an isolated egress-directory mount: the directory must
    /// exist already and is only opened, never created.
    #[doc(hidden)]
    pub fn with_parts_in_directory(
        transport: T,
        budgets: &'static [Budget],
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
        broker_egress: BrokerEgress,
        directory_path: &Path,
    ) -> Result<Self, GatewayError> {
        let egress_directory = if broker_egress == BrokerEgress::On {
            let directory = EgressDirectory::open(directory_path).map_err(|error| {
                GatewayError::TallyUnavailable {
                    path: directory_path.join(crate::tally::TALLY_FILE),
                    reason: error.to_string(),
                }
            })?;
            OutboundTally::validate(&directory).map_err(|error| {
                tally_gateway_error(Destination::FinamApi, &directory.tally_path(), error)
            })?;
            Some(directory)
        } else {
            None
        };
        Self::assemble(
            transport,
            budgets,
            clock,
            sleeper,
            broker_egress,
            egress_directory,
        )
    }

    /// The one constructor body: every placement above resolves the directory
    /// (or none, when broker egress is off) and hands it here.
    fn assemble(
        transport: T,
        budgets: &'static [Budget],
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
        broker_egress: BrokerEgress,
        egress_directory: Option<EgressDirectory>,
    ) -> Result<Self, GatewayError> {
        Ok(Self {
            transport: Arc::new(transport),
            budgets: BudgetTable::new(budgets)?,
            retry: RetryPolicy::new(ATTEMPTS, FIRST_BACKOFF),
            non_broker_retry: RetryPolicy::new(NON_BROKER_ATTEMPTS, FIRST_BACKOFF),
            clock,
            sleeper,
            broker_egress,
            egress_directory,
            // The response cache is carried only where it is asked for:
            // the production gateway and the seam that builds what the
            // production gateway builds.
            cache: None,
            lanes: Destination::ALL
                .into_iter()
                .map(|destination| {
                    (
                        destination.base_url(),
                        Arc::new(Mutex::new(Lane::default())),
                    )
                })
                .collect(),
        })
    }

    /// Send a request under every rule of the gateway.
    ///
    /// A 2xx response is returned as it came; interpreting its body stays
    /// with the source. Broker budgets are selected only from the destination
    /// and request path.
    ///
    /// # Errors
    /// See `GatewayError`.
    pub async fn send(
        &self,
        request: &HttpRequest,
        deadline: Option<Instant>,
    ) -> Result<HttpResponse, GatewayError> {
        let method = request.path();
        let started = self.clock.now();
        let result = self.send_inner(request, deadline).await;
        if let Err(refusal) = &result {
            // Every refused call says what was sent and what came back: the
            // URL as it went out (the token never travels in it), the
            // attempt, the wait, the status. The body stays out: it may
            // carry the owner's data.
            let elapsed_ms = millis(self.clock.now() - started);
            if let Some(wait) = refusal.retry_after() {
                tracing::warn!(
                    destination = ?request.destination(),
                    method,
                    url = %request.url(),
                    attempt = refusal.attempts(),
                    elapsed_ms,
                    wait_ms = millis(wait),
                    status = refusal.status(),
                    request_id = "-",
                    refusal = refusal.kind(),
                    "outbound call refused"
                );
            } else if let GatewayError::Rejected {
                destination,
                status,
                attempts,
                location,
                content_type,
                request_id,
                ..
            } = refusal
            {
                // The destination's own "no" (a redirect, a 401 for a
                // revoked token, a 404): where a redirect points, the
                // body's type, and the request id its support asks for.
                tracing::warn!(
                    destination = ?destination,
                    method,
                    url = %request.url(),
                    attempt = attempts,
                    elapsed_ms,
                    status,
                    request_id = request_id.as_deref().unwrap_or("-"),
                    location = location.as_deref().unwrap_or("-"),
                    content_type = content_type.as_deref().unwrap_or("-"),
                    refusal = refusal.kind(),
                    "outbound call refused"
                );
            }
        }
        result
    }

    /// The body of `send`: every rule, attempt and progress line, and the
    /// answered line for a call that got its 2xx. `send` adds the refused
    /// line, which reads the whole call.
    async fn send_inner(
        &self,
        request: &HttpRequest,
        deadline: Option<Instant>,
    ) -> Result<HttpResponse, GatewayError> {
        let started = self.clock.now();
        let destination = request.destination();
        let method = request.path();
        let broker = is_broker_destination(destination);
        let retry = if broker {
            &self.retry
        } else {
            &self.non_broker_retry
        };
        let (budget, key) = self.budgets.lookup(request)?;
        if broker && self.broker_egress == BrokerEgress::Off {
            return Err(GatewayError::BrokerEgressOff);
        }
        let allowance = if broker {
            Some(
                request
                    .allowance()
                    .ok_or(GatewayError::MissingRequestAllowance { destination })?,
            )
        } else {
            None
        };
        // The response cache, when this gateway carries one: a read the
        // gateway answered within the hour is answered from the store
        // beside the database. Nothing is sent, no ceiling allowance is
        // spent and no retry is run; the lane is not even taken. The
        // lookup comes after every check of the request itself, so a call
        // refused here is refused exactly as before the cache existed.
        let cache_key = self.cache.as_ref().and_then(|_cache| CacheKey::of(request));
        // The stored answer under `key`, logged like an answered call, or
        // `None` when the store holds none. Used twice: once before the
        // lane is taken, and again once the lane is held, so a caller
        // queued behind the very send that stored the answer serves it
        // instead of spending its allowance and sending the same read.
        let answer_from_cache = |cache: &ResponseCache, key: &CacheKey| -> Option<HttpResponse> {
            let stored = cache.lookup(key)?;
            tracing::info!(
                destination = ?destination,
                method,
                url = %request.url(),
                attempt = 0,
                status = stored.status,
                elapsed_ms = millis(self.clock.now() - started),
                request_id = without_secret(stored.request_id.as_deref(), request.bearer())
                    .as_deref()
                    .unwrap_or("-"),
                "outbound call answered from cache"
            );
            Some(stored)
        };
        if let (Some(cache), Some(key)) = (&self.cache, &cache_key) {
            if let Some(stored) = answer_from_cache(cache, key) {
                return Ok(stored);
            }
        }
        let lane_lock = &self.lanes[destination.base_url()];
        let mut attempts = 0_u32;
        let mut status = None;
        // An attempt that would start at or after the deadline would end past
        // it, and so would any wait whose end is the start of that attempt.
        let crosses = |start: Instant| deadline.is_some_and(|deadline| start >= deadline);
        let cut = |attempts, status, retry_after| GatewayError::DeadlineReached {
            destination,
            status,
            attempts,
            retry_after,
        };
        // Destination, method key, attempt, wait and status only: never a
        // header, a body or the request, so no secret can reach the log.
        let waits = |attempt: u32, wait: Duration, reason: &'static str| {
            if wait > LOGGED_WAIT {
                tracing::info!(
                    destination = ?destination,
                    method,
                    attempt,
                    wait_ms = millis(wait),
                    reason,
                    "outbound call waits"
                );
            }
        };
        let retries = |attempt: u32, wait: Duration, status: Option<u16>| {
            tracing::warn!(
                destination = ?destination,
                method,
                attempt,
                wait_ms = millis(wait),
                status,
                "outbound call retries"
            );
        };
        loop {
            // The in-process lane is the endpoint permit. Broker ownership,
            // tally decision, transport and response-status recording all
            // happen while it is held; waits release it. The former 100 ms
            // decision-to-handoff bound is unnecessary: another process
            // cannot own this endpoint, and another local call cannot enter
            // until the status has been recorded.
            let (decision, outcome, body, answered) = {
                let asked = self.clock.now();
                // A caller queued behind a long call or a named wait gives up
                // at its deadline rather than when the lane frees; dropping
                // the lock future leaves the queue as it was.
                let Some(mut lane) = self
                    .by_deadline(Arc::clone(lane_lock).lock_owned(), deadline)
                    .await
                else {
                    let retry_after =
                        retry.delay(attempts, &Outcome::Transport(HttpError::Timeout));
                    return Err(cut(attempts, status, retry_after));
                };
                waits(
                    attempts + 1,
                    self.clock.now().saturating_duration_since(asked),
                    "lane",
                );
                if broker
                    && lane
                        .owner
                        .as_ref()
                        .is_some_and(|owner| !owner.belongs_to_current_process())
                {
                    lane.owner = None;
                }
                if broker && lane.owner.is_none() {
                    let directory = self
                        .egress_directory
                        .as_ref()
                        .ok_or(GatewayError::BrokerEgressOff)?;
                    let lock_name =
                        endpoint_lock_name(destination).ok_or(GatewayError::BrokerEgressOff)?;
                    let owner = EndpointOwner::acquire(
                        directory.clone(),
                        destination.base_url(),
                        lock_name,
                    )
                    .map_err(|error| {
                        tally_gateway_error(destination, &directory.tally_path(), error)
                    })?;
                    owner
                        .tally()
                        .and_then(|tally| {
                            tally.adopt_pending(destination.base_url(), self.clock.as_ref())
                        })
                        .map_err(|error| {
                            tally_gateway_error(destination, &directory.tally_path(), error)
                        })?;
                    lane.owner = Some(owner);
                }
                if broker && lane.detached_attempt_failed.load(Ordering::SeqCst) {
                    let directory = self
                        .egress_directory
                        .as_ref()
                        .ok_or(GatewayError::BrokerEgressOff)?;
                    let owner = lane.owner.as_ref().ok_or(GatewayError::BrokerEgressOff)?;
                    owner
                        .tally()
                        .and_then(|tally| {
                            tally.record_join_failure(destination.base_url(), self.clock.as_ref())
                        })
                        .map_err(|error| {
                            tally_gateway_error(destination, &directory.tally_path(), error)
                        })?;
                    lane.detached_attempt_failed.store(false, Ordering::SeqCst);
                }
                if broker && let Some(latch) = lane.tally_latched {
                    let directory = self
                        .egress_directory
                        .as_ref()
                        .ok_or(GatewayError::BrokerEgressOff)?;
                    let owner = lane.owner.as_ref().ok_or(GatewayError::BrokerEgressOff)?;
                    let tally = owner.tally().map_err(|error| {
                        tally_gateway_error(destination, &directory.tally_path(), error)
                    })?;
                    match latch {
                        TallyLatch::Response {
                            status: latched_status,
                            retry_after: latched_retry_after,
                        } => {
                            let recorded = tally
                                .record_response(
                                    destination.base_url(),
                                    latched_status,
                                    latched_retry_after,
                                    self.clock.as_ref(),
                                )
                                .map_err(|error| {
                                    tally_gateway_error(destination, &directory.tally_path(), error)
                                })?;
                            lane.tally_latched = None;
                            match recorded {
                                TallyResponseDecision::Paused { retry_after } => {
                                    return Err(GatewayError::BrokerHostPaused {
                                        destination,
                                        host: destination.base_url(),
                                        reopens_at: format!("{retry_after:?} on the boot clock"),
                                        retry_after,
                                        attempts,
                                    });
                                }
                                TallyResponseDecision::Recorded => {
                                    return Err(GatewayError::TallyUnavailable {
                                        path: directory.tally_path(),
                                        reason: format!(
                                            "the previously observed broker status {latched_status} was persisted; retry the request"
                                        ),
                                    });
                                }
                            }
                        }
                        TallyLatch::Unknown => {
                            let retry_after = tally
                                .record_unknown_outcome(destination.base_url(), self.clock.as_ref())
                                .map_err(|error| {
                                    tally_gateway_error(destination, &directory.tally_path(), error)
                                })?;
                            lane.tally_latched = None;
                            return Err(GatewayError::BrokerHostClosed {
                                destination,
                                host: destination.base_url(),
                                reason: ClosureReason::UnresolvedAttempt.description(),
                                reopens_at: format!("{retry_after:?} on the boot clock"),
                                retry_after,
                                status: None,
                                attempts,
                            });
                        }
                    }
                }
                if let Some(retry_after) = lane.refusing(self.clock.now()) {
                    return Err(GatewayError::CircuitOpen {
                        destination,
                        attempts,
                        retry_after,
                    });
                }
                let now = self.clock.now();
                let named = lane.named_wait(now);
                if named > MAX_NAMED_WAIT {
                    return Err(GatewayError::Deferred {
                        destination,
                        status,
                        attempts,
                        retry_after: named,
                    });
                }
                let budget_wait = if broker {
                    Duration::ZERO
                } else {
                    lane.budget_wait(key, &budget, now)
                };
                let wait = named.max(budget_wait);
                if crosses(now + wait) {
                    return Err(cut(attempts, status, wait));
                }
                if !wait.is_zero() {
                    let reason = if named >= budget_wait {
                        "named reset"
                    } else {
                        "budget"
                    };
                    waits(attempts + 1, wait, reason);
                    if broker {
                        drop(lane);
                        if self
                            .by_deadline(self.sleeper.sleep(wait), deadline)
                            .await
                            .is_none()
                        {
                            return Err(cut(attempts, status, wait));
                        }
                        continue;
                    }
                    if self
                        .by_deadline(self.sleeper.sleep(wait), deadline)
                        .await
                        .is_none()
                    {
                        return Err(cut(attempts, status, wait));
                    }
                }
                // A caller that was queued behind the very send that stored
                // its answer re-reads the store now that it holds the lane,
                // before any allowance is spent or anything is sent: two
                // concurrent identical reads must not both reach the wire
                // within the hour. A caller that waited and then reacquired
                // the lane reaches this same point again and re-reads too.
                if let (Some(cache), Some(key)) = (&self.cache, &cache_key) {
                    if let Some(stored) = answer_from_cache(cache, key) {
                        return Ok(stored);
                    }
                }
                if broker {
                    let directory = self
                        .egress_directory
                        .as_ref()
                        .ok_or(GatewayError::BrokerEgressOff)?;
                    let Some(allowance) = allowance else {
                        return Err(GatewayError::MissingRequestAllowance { destination });
                    };
                    if !allowance.take() {
                        return Err(GatewayError::RequestCeiling {
                            destination,
                            ceiling: allowance.ceiling(),
                        });
                    }
                    let owner = lane.owner.as_ref().ok_or(GatewayError::BrokerEgressOff)?;
                    let owner_tally = owner.tally().map_err(|error| {
                        tally_gateway_error(destination, &directory.tally_path(), error)
                    })?;
                    let tally_decision = match owner_tally.decide_and_record(
                        destination.base_url(),
                        key.unwrap_or("*"),
                        budget.used,
                        budget.window,
                        self.clock.as_ref(),
                    ) {
                        Ok(decision) => decision,
                        Err(error) => {
                            allowance.give_back();
                            return Err(tally_gateway_error(
                                destination,
                                &directory.tally_path(),
                                error,
                            ));
                        }
                    };
                    match tally_decision {
                        TallyDecision::Send => {}
                        TallyDecision::Paused { retry_after } => {
                            allowance.give_back();
                            return Err(GatewayError::BrokerHostPaused {
                                destination,
                                host: destination.base_url(),
                                reopens_at: format!("{retry_after:?} on the boot clock"),
                                retry_after,
                                attempts,
                            });
                        }
                        TallyDecision::Closed {
                            reason,
                            retry_after,
                        } => {
                            allowance.give_back();
                            return Err(GatewayError::BrokerHostClosed {
                                destination,
                                host: destination.base_url(),
                                reason: reason.description(),
                                reopens_at: format!("{retry_after:?} on the boot clock"),
                                retry_after,
                                status: matches!(reason, ClosureReason::RateLimits).then_some(429),
                                attempts,
                            });
                        }
                        TallyDecision::DailyCeiling { retry_after } => {
                            allowance.give_back();
                            return Err(GatewayError::DailyCeiling {
                                destination,
                                ceiling: DAILY_CEILING,
                                resets_at: format!("{retry_after:?} on the boot clock"),
                                retry_after,
                            });
                        }
                        TallyDecision::Wait(wait) => {
                            allowance.give_back();
                            if crosses(self.clock.now() + wait) {
                                return Err(cut(attempts, status, wait));
                            }
                            waits(attempts + 1, wait, "outbound tally");
                            drop(lane);
                            if self
                                .by_deadline(self.sleeper.sleep(wait), deadline)
                                .await
                                .is_none()
                            {
                                return Err(cut(attempts, status, wait));
                            }
                            continue;
                        }
                    }
                } else {
                    lane.record_start(key, &budget, self.clock.now());
                }
                attempts += 1;
                let (mut lane, mut answer, handoff, observed, unknown, cleanup_error) = if broker {
                    let directory = self
                        .egress_directory
                        .as_ref()
                        .ok_or(GatewayError::BrokerEgressOff)?;
                    let owner = lane.owner.as_ref().ok_or(GatewayError::BrokerEgressOff)?;
                    let owner_tally = owner.tally().map_err(|error| {
                        tally_gateway_error(destination, &directory.tally_path(), error)
                    })?;
                    let transport = Arc::clone(&self.transport);
                    let request = request.clone();
                    let clock = Arc::clone(&self.clock);
                    let tally_path = directory.tally_path();
                    let task = tokio::spawn(async move {
                        let mut lane = lane;
                        // This guard is created after the lane, so panic
                        // unwinding publishes the failure before releasing it.
                        let attempt = DetachedAttemptGuard::new(&lane);
                        let handoff =
                            Arc::new(std::sync::Mutex::new(None::<Result<(), GatewayError>>));
                        let callback_handoff = Arc::clone(&handoff);
                        let handoff_clock = Arc::clone(&clock);
                        let handoff_tally = owner_tally.clone();
                        let handoff_path = tally_path.clone();
                        let acknowledge = Box::new(move || {
                            let result = handoff_tally
                                .acknowledge_handoff(destination.base_url(), handoff_clock.as_ref())
                                .map_err(|error| {
                                    tally_gateway_error(destination, &handoff_path, error)
                                });
                            let failed = result.is_err();
                            *callback_handoff
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                            if failed {
                                Err(HttpError::RequestNotBuilt(
                                    "the outbound tally did not acknowledge hand-off".to_owned(),
                                ))
                            } else {
                                Ok(())
                            }
                        });
                        let observed = Arc::new(std::sync::Mutex::new(
                            None::<(HttpResponse, Result<TallyResponseDecision, GatewayError>)>,
                        ));
                        let callback_observed = Arc::clone(&observed);
                        let callback_clock = Arc::clone(&clock);
                        let callback_tally = owner_tally.clone();
                        let callback_path = tally_path.clone();
                        let observe = Box::new(move |observed: HttpResponse| {
                            let retry_after =
                                observed.retry_after.map(|delay| delay.min(TALLY_RETENTION));
                            let recorded = callback_tally
                                .record_response(
                                    destination.base_url(),
                                    observed.status,
                                    retry_after,
                                    callback_clock.as_ref(),
                                )
                                .map_err(|error| {
                                    tally_gateway_error(destination, &callback_path, error)
                                });
                            *callback_observed
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some((observed, recorded));
                        });
                        let answer = transport
                            .send_observed(&request, acknowledge, observe)
                            .await;
                        let handoff = handoff
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                        let observed = observed
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                        let unknown =
                            if matches!(answer, Err(HttpError::Network | HttpError::Timeout))
                                && handoff.as_ref().is_some_and(Result::is_ok)
                                && observed.is_none()
                            {
                                Some(
                                    owner_tally
                                        .record_unknown_outcome(
                                            destination.base_url(),
                                            clock.as_ref(),
                                        )
                                        .map_err(|error| {
                                            tally_gateway_error(destination, &tally_path, error)
                                        }),
                                )
                            } else {
                                None
                            };
                        let cleanup_error = if matches!(
                            answer,
                            Err(HttpError::ClientNotBuilt(_)
                                | HttpError::TrustAnchorNotParsed(_)
                                | HttpError::RequestNotBuilt(_))
                        ) && handoff.is_none()
                            && observed.is_none()
                        {
                            owner_tally
                                .record_not_handed_off(destination.base_url(), clock.as_ref())
                                .map_err(|error| {
                                    tally_gateway_error(destination, &tally_path, error)
                                })
                                .err()
                        } else {
                            None
                        };
                        if let Some((line, Err(_))) = observed.as_ref() {
                            lane.tally_latched = Some(if (200..300).contains(&line.status) {
                                TallyLatch::Unknown
                            } else {
                                TallyLatch::Response {
                                    status: line.status,
                                    retry_after: line.retry_after,
                                }
                            });
                        } else if handoff.as_ref().is_some_and(Result::is_err)
                            || unknown.as_ref().is_some_and(Result::is_err)
                            || cleanup_error.is_some()
                        {
                            lane.tally_latched = Some(TallyLatch::Unknown);
                        }
                        attempt.complete();
                        (
                            lane,
                            Some(answer),
                            handoff,
                            observed,
                            unknown,
                            cleanup_error,
                        )
                    });
                    // Arm the detached attempt before starting the caller's
                    // deadline race. This gives an immediately-ready
                    // transport/status commit the same first-poll semantics
                    // as `by_deadline` gives an inline future, while a
                    // transport that suspends remains owned by the task.
                    tokio::task::yield_now().await;
                    let Some(joined) = self.by_deadline(task, deadline).await else {
                        let retry_after =
                            retry.delay(attempts, &Outcome::Transport(HttpError::Timeout));
                        return Err(cut(attempts, status, retry_after));
                    };
                    joined.map_err(|error| GatewayError::TallyUnavailable {
                        path: directory.tally_path(),
                        reason: format!(
                            "the detached broker attempt failed before resolving its durable pending record: {error}"
                        ),
                    })?
                } else {
                    (
                        lane,
                        self.by_deadline(self.transport.send(request), deadline)
                            .await,
                        None,
                        None,
                        None,
                        None,
                    )
                };
                if let Some(Err(error)) = handoff {
                    return Err(error);
                }
                if let Some(error) = cleanup_error {
                    return Err(error);
                }
                answer = answer.map(|result| {
                    result.map(|mut response| {
                        response.retry_after =
                            response.retry_after.map(|delay| delay.min(TALLY_RETENTION));
                        response
                    })
                });
                if let Some((observed_line, recorded)) = observed {
                    match recorded {
                        Ok(TallyResponseDecision::Recorded) => {}
                        Ok(TallyResponseDecision::Paused { retry_after }) => {
                            return Err(GatewayError::BrokerHostPaused {
                                destination,
                                host: destination.base_url(),
                                reopens_at: format!("{retry_after:?} on the boot clock"),
                                retry_after,
                                attempts,
                            });
                        }
                        Err(error) => return Err(error),
                    }
                    // A status line is an endpoint answer even when its body
                    // later stalls or fails. Keep a 4xx/5xx from becoming a
                    // retryable transport error and losing the broker's stop
                    // signal. The line carries the headers the refused log
                    // reads by.
                    if !(200..300).contains(&observed_line.status) && !matches!(answer, Some(Ok(_)))
                    {
                        answer = Some(Ok(observed_line));
                    }
                } else if let Some(recorded) = unknown {
                    match recorded {
                        Ok(retry_after) => {
                            return Err(GatewayError::BrokerHostClosed {
                                destination,
                                host: destination.base_url(),
                                reason: ClosureReason::UnresolvedAttempt.description(),
                                reopens_at: format!("{retry_after:?} on the boot clock"),
                                retry_after,
                                status: None,
                                attempts,
                            });
                        }
                        Err(error) => return Err(error),
                    }
                } else if broker && matches!(answer, Some(Ok(_))) {
                    let directory = self
                        .egress_directory
                        .as_ref()
                        .ok_or(GatewayError::BrokerEgressOff)?;
                    return Err(tally_gateway_error(
                        destination,
                        &directory.tally_path(),
                        TallyError::Corrupt(
                            "the transport returned a response without exposing its status"
                                .to_owned(),
                        ),
                    ));
                }
                let Some(answer) = answer else {
                    let retry_after =
                        retry.delay(attempts, &Outcome::Transport(HttpError::Timeout));
                    return Err(cut(attempts, status, retry_after));
                };
                let (outcome, body, answered) = match answer {
                    Ok(mut response) => {
                        if (200..300).contains(&response.status) {
                            // The moment this answer was observed: a live
                            // send carries the stamp its transport gave it,
                            // and a transport that does not stamp is dated
                            // by the gateway's wall clock — the same clock
                            // the store stamps entries with, so a caller can
                            // date a live answer and a served one on one
                            // scale.
                            response
                                .observed_at
                                .get_or_insert_with(|| self.clock.now_unix());
                            // The answer is kept under the request's key, so
                            // the same read inside the hour is answered
                            // from the store instead of being sent again.
                            // The presented credential and the cache
                            // identity are cut out before anything is
                            // persisted, so a cache file never holds one.
                            if let (Some(cache), Some(key)) = (&self.cache, &cache_key) {
                                cache.store(
                                    key,
                                    &response,
                                    request.bearer(),
                                    request.cache_identity(),
                                );
                            }
                            if lane.record_success() {
                                tracing::info!(
                                    destination = ?destination,
                                    method,
                                    attempt = attempts,
                                    status = response.status,
                                    "breaker closed"
                                );
                            }
                            // Every outbound call to every destination
                            // leaves an answered line: where it went, what
                            // came back, and how long it took.
                            tracing::info!(
                                destination = ?destination,
                                method,
                                url = %request.url(),
                                attempt = attempts,
                                status = response.status,
                                elapsed_ms = millis(self.clock.now() - started),
                                request_id = without_secret(
                                    response.request_id.as_deref(),
                                    request.bearer(),
                                )
                                .as_deref()
                                .unwrap_or("-"),
                                "outbound call answered"
                            );
                            return Ok(response);
                        }
                        status = Some(response.status);
                        let outcome = match response.retry_after {
                            Some(after) => Outcome::status_with_retry_after(response.status, after),
                            None => Outcome::status(response.status),
                        };
                        let answered = RefusalHeaders::of(&response, request.bearer());
                        (outcome, response.body, answered)
                    }
                    Err(error) => {
                        status = None;
                        (
                            Outcome::Transport(error),
                            Vec::new(),
                            RefusalHeaders::default(),
                        )
                    }
                };
                if let Some(named) = outcome.named_delay()
                    && is_transient(&outcome)
                {
                    lane.hold_until(self.clock.now() + named);
                }
                // A request that may act is sent once: a lost answer does not
                // say the action was not taken.
                let decision = if request.is_idempotent() {
                    retry.decide(attempts, &outcome)
                } else {
                    Retry::GiveUp
                };
                // Only a call that failed transiently to the end counts: a
                // permanent refusal says our request is wrong, not that the
                // destination is down.
                if decision == Retry::GiveUp
                    && is_transient(&outcome)
                    && lane.record_failure(self.clock.now())
                {
                    tracing::warn!(
                        destination = ?destination,
                        method,
                        attempt = attempts,
                        wait_ms = millis(BREAKER_COOL_DOWN),
                        status,
                        "breaker opened"
                    );
                }
                (decision, outcome, body, answered)
            };
            match decision {
                // Waited at the top of the loop, under the lane, where every
                // other caller waits it too.
                Retry::After(delay) if outcome.named_delay().is_some() => {
                    retries(attempts, delay, status);
                }
                // A call cut short by its deadline is left out of the breaker
                // count: it did not use up its retries, so it proved no more
                // than one failed attempt does.
                Retry::After(delay) if crosses(self.clock.now() + delay) => {
                    return Err(cut(attempts, status, delay));
                }
                Retry::After(delay) => {
                    retries(attempts, delay, status);
                    waits(attempts + 1, delay, "backoff");
                    if self
                        .by_deadline(self.sleeper.sleep(delay), deadline)
                        .await
                        .is_none()
                    {
                        return Err(cut(attempts, status, delay));
                    }
                }
                Retry::GiveUp => {
                    let body = RejectedBody::without_secret(&body, request.bearer());
                    return Err(self.refusal(destination, attempts, outcome, body, answered));
                }
            }
        }
    }

    /// `work`, unless the deadline falls first: then `None`, at the deadline.
    ///
    /// Every wait of a call goes through here — the lane, a budget, a named
    /// reset, a backoff and the attempt itself — so none of them outlives the
    /// deadline. At a tie the deadline wins: it is polled before `work`, and
    /// what `work` gives at or after the deadline, by the clock, is treated as
    /// not there, since a transport or a late timer may move a clock without
    /// waiting on the deadline's sleep.
    ///
    /// The deadline's sleep is started only once `work` has not finished on
    /// its first poll, so an answer that is already there costs no timer.
    async fn by_deadline<F: Future>(
        &self,
        work: F,
        deadline: Option<Instant>,
    ) -> Option<F::Output> {
        let Some(deadline) = deadline else {
            return Some(work.await);
        };
        let mut work = pin!(work);
        let mut limit: Option<Pin<Box<dyn Future<Output = ()> + Send + '_>>> = None;
        poll_fn(|context| {
            if let Some(limit) = limit.as_mut()
                && limit.as_mut().poll(context).is_ready()
            {
                return Poll::Ready(None);
            }
            if self.clock.now() >= deadline {
                return Poll::Ready(None);
            }
            if let Poll::Ready(answer) = work.as_mut().poll(context) {
                return Poll::Ready((self.clock.now() < deadline).then_some(answer));
            }
            let limit = limit.get_or_insert_with(|| {
                self.sleeper
                    .sleep(deadline.saturating_duration_since(self.clock.now()))
            });
            limit.as_mut().poll(context).map(|()| None)
        })
        .await
    }

    /// The error for a call that ended on `outcome`.
    fn refusal(
        &self,
        destination: Destination,
        attempts: u32,
        outcome: Outcome,
        body: RejectedBody,
        headers: RefusalHeaders,
    ) -> GatewayError {
        if is_transient(&outcome) {
            return GatewayError::Exhausted {
                destination,
                status: match outcome {
                    Outcome::Status { status, .. } => Some(status),
                    Outcome::Transport(_) => None,
                },
                attempts,
                retry_after: if is_broker_destination(destination) {
                    self.retry.delay(attempts, &outcome)
                } else {
                    self.non_broker_retry.delay(attempts, &outcome)
                },
            };
        }
        match outcome {
            Outcome::Status { status, .. } => GatewayError::Rejected {
                destination,
                status,
                attempts,
                body,
                location: headers.location,
                content_type: headers.content_type,
                request_id: headers.request_id,
            },
            Outcome::Transport(error) => GatewayError::Transport {
                destination,
                error,
                attempts,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn host_boottime_accepts_only_a_zero_namespace_offset() {
        require_host_boottime("monotonic 0 0\nboottime 0 0\n").expect("host boot clock accepted");
        let refused = require_host_boottime("monotonic 0 0\nboottime 1 0\n")
            .expect_err("namespaced boot clock refused");
        assert!(refused.contains("boottime offset 1 0"), "{refused}");
    }

    use super::*;
    use crate::RequestAllowance;
    use crate::resilience::MAX_NAMED_WAIT;

    #[test]
    fn broker_egress_refusals_are_distinguished_from_other_gateway_errors() {
        assert!(GatewayError::BrokerEgressOff.is_broker_egress_refusal());
        assert!(
            !GatewayError::UnknownBudget {
                destination: Destination::MoexIss,
                path: "/iss/history.json".to_owned(),
            }
            .is_broker_egress_refusal()
        );
    }

    /// A clock on tokio's paused timer: it moves only when every task waits
    /// on it, so a sleep of a minute takes no real time, and a request still
    /// in flight really is overtaken by a deadline that falls first.
    ///
    /// `advance` jumps the clock between calls, when nothing waits on it.
    struct FakeTime {
        offset: StdMutex<Duration>,
        monotonic_origin: Instant,
        slept: StdMutex<Vec<Duration>>,
        /// How much later than asked each sleep ends, by the clock: a timer
        /// that fires late, as one does on a loaded or suspended host.
        lag: StdMutex<Duration>,
    }

    impl FakeTime {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                offset: StdMutex::new(Duration::ZERO),
                monotonic_origin: tokio::time::Instant::now().into_std(),
                slept: StdMutex::new(Vec::new()),
                lag: StdMutex::new(Duration::ZERO),
            })
        }

        fn advance(&self, by: Duration) {
            *self.offset.lock().expect("clock") += by;
        }

        fn lagging(&self, by: Duration) {
            *self.lag.lock().expect("lag") = by;
        }

        /// The sleeps that ran to their end; one abandoned early (a
        /// deadline that lost the race) is not a wait anybody made.
        fn slept(&self) -> Vec<Duration> {
            self.slept.lock().expect("sleeps").clone()
        }
    }

    impl Clock for FakeTime {
        fn now(&self) -> Instant {
            tokio::time::Instant::now().into_std() + *self.offset.lock().expect("clock")
        }

        fn now_boot(&self) -> Result<BootTime, String> {
            Ok(BootTime::new(
                "test-boot",
                self.now().saturating_duration_since(self.monotonic_origin),
            ))
        }

        fn now_unix(&self) -> SystemTime {
            SystemTime::now() + *self.offset.lock().expect("clock")
        }
    }

    impl Sleeper for FakeTime {
        fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                self.advance(*self.lag.lock().expect("lag"));
                self.slept.lock().expect("sleeps").push(delay);
            })
        }
    }

    /// An endpoint that answers from a script, then with a default status,
    /// and remembers when each request reached it.
    struct Scripted {
        time: Arc<FakeTime>,
        script: StdMutex<VecDeque<Result<HttpResponse, HttpError>>>,
        default_status: u16,
        sent: StdMutex<Vec<(Destination, Instant)>>,
        in_flight: AtomicUsize,
        most_in_flight: AtomicUsize,
        /// How long each request stays in flight, on the paused timer.
        latency: Duration,
        /// How far the clock jumps while a request is in flight, as a fake
        /// endpoint elsewhere in the tree moves its clock without waiting.
        jump: Duration,
    }

    impl Scripted {
        fn answering(time: &Arc<FakeTime>, default_status: u16) -> Self {
            Self {
                time: Arc::clone(time),
                script: StdMutex::new(VecDeque::new()),
                default_status,
                sent: StdMutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                most_in_flight: AtomicUsize::new(0),
                latency: Duration::ZERO,
                jump: Duration::ZERO,
            }
        }

        fn taking(self, latency: Duration) -> Self {
            Self { latency, ..self }
        }

        fn jumping(self, jump: Duration) -> Self {
            Self { jump, ..self }
        }

        fn then(self, answer: Result<HttpResponse, HttpError>) -> Self {
            self.script.lock().expect("script").push_back(answer);
            self
        }

        fn sent_at(&self) -> Vec<Instant> {
            self.sent
                .lock()
                .expect("sent")
                .iter()
                .map(|(_, at)| *at)
                .collect()
        }

        fn sent_count(&self) -> usize {
            self.sent.lock().expect("sent").len()
        }
    }

    impl Transport for Scripted {
        async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            self.sent
                .lock()
                .expect("sent")
                .push((request.destination(), self.time.now()));
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_in_flight.fetch_max(now, Ordering::SeqCst);
            // Give a concurrent call every chance to start while this one is
            // in flight.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            if !self.latency.is_zero() {
                tokio::time::sleep(self.latency).await;
            }
            self.time.advance(self.jump);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            self.script
                .lock()
                .expect("script")
                .pop_front()
                .unwrap_or_else(|| Ok(status(self.default_status)))
        }
    }

    fn status(status: u16) -> HttpResponse {
        HttpResponse {
            status,
            body: Vec::new(),
            retry_after: None,
            ..Default::default()
        }
    }

    /// An invented instance database in a fresh directory, the way a real
    /// one exists before the gateway is built over it.
    fn instance_database(label: &str) -> (PathBuf, PathBuf) {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "iaam-http-instance-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).expect("instance directory created");
        let database = directory.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database file written");
        (directory, database)
    }

    fn broker_egress_directory() -> PathBuf {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "iaam-http-gateway-test-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).expect("egress directory created");
        let initialized = format!(
            "iaam-outbound-tally-v4\ngeneration\t0\nboot\ttest-boot\nhigh-water\t0\nhost\t{}\t-\t\t-\t-\t-\t-\t\t\t-\t-\t-\nhost\t{}\t-\t\t-\t-\t-\t-\t\t\t-\t-\t-\nhost\t{}\t-\t\t-\t-\t-\t-\t\t\t-\t-\t-\n",
            Destination::TinkoffProd.base_url(),
            Destination::TinkoffSandbox.base_url(),
            Destination::FinamApi.base_url(),
        );
        std::fs::write(directory.join(crate::tally::TALLY_FILE), initialized)
            .expect("initialized tally created");
        std::fs::write(directory.join(crate::tally::GENERATION_FILE), "0\n")
            .expect("tally generation created");
        directory
    }

    fn gateway<T: Transport + 'static>(time: &Arc<FakeTime>, transport: T) -> Gateway<T> {
        let directory = broker_egress_directory();
        Gateway::with_parts_in_directory(
            transport,
            BUDGETS,
            Arc::clone(time) as Arc<dyn Clock>,
            Arc::clone(time) as Arc<dyn Sleeper>,
            BrokerEgress::On,
            &directory,
        )
        .expect("the documented table is valid")
    }

    fn operations() -> HttpRequest {
        HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor",
            crate::RequestBody::Json("{}".to_owned()),
        )
        .with_request_allowance(RequestAllowance::new(u32::MAX))
        // A read-only RPC: T-Invest reads over POST.
        .idempotent()
    }

    /// Assert no `window` holds more than `limit` of the instants.
    fn assert_never_more_than(limit: usize, window: Duration, sent: &[Instant]) {
        for (index, start) in sent.iter().enumerate() {
            if let Some(later) = sent.get(index + limit) {
                assert!(
                    later.duration_since(*start) >= window,
                    "request {} started {:?} after request {index}: more than {limit} in {window:?}",
                    index + limit,
                    later.duration_since(*start)
                );
            }
        }
    }

    // --- budget -----------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn request_allowance_refuses_the_attempt_after_its_ceiling() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let allowance = RequestAllowance::new(300);
        let request = operations().with_request_allowance(allowance.clone());

        for attempt in 1..=300 {
            if let Err(error) = gateway.send(&request, None).await {
                panic!("attempt {attempt} inside the allowance failed: {error}");
            }
        }
        let result = gateway.send(&request, None).await;
        let Err(error) = result else {
            panic!("the next attempt was sent");
        };

        assert_eq!(gateway.transport.sent_count(), 300);
        let message = error.to_string();
        assert!(message.contains("300"), "{message}");
        assert!(message.contains("narrow"), "{message}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_broker_request_without_an_allowance_never_reaches_transport() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor",
            crate::RequestBody::Json("{}".to_owned()),
        )
        .idempotent();

        let result = gateway.send(&request, None).await;
        assert!(matches!(
            result,
            Err(GatewayError::MissingRequestAllowance {
                destination: Destination::TinkoffProd
            })
        ));
        assert_eq!(gateway.transport.sent_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_spend_the_same_allowance_as_first_attempts() {
        let time = FakeTime::new();
        let transport = Scripted::answering(&time, 200)
            .then(Ok(status(500)))
            .then(Ok(status(500)))
            .then(Ok(status(200)));
        let gateway = gateway(&time, transport);
        let allowance = RequestAllowance::new(3);
        let request = operations().with_request_allowance(allowance.clone());

        if let Err(error) = gateway.send(&request, None).await {
            panic!("the third attempt should succeed: {error}");
        }
        let result = gateway.send(&request, None).await;
        let Err(error) = result else {
            panic!("three retries did not spend the allowance");
        };

        assert_eq!(gateway.transport.sent_count(), 3);
        assert!(error.to_string().contains("3"));
    }

    #[tokio::test(start_paused = true)]
    async fn separate_allowances_do_not_share_attempts() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let first = RequestAllowance::new(1);
        let second = RequestAllowance::new(1);

        if let Err(error) = gateway
            .send(&operations().with_request_allowance(first), None)
            .await
        {
            panic!("first sync did not send: {error}");
        }
        if let Err(error) = gateway
            .send(&operations().with_request_allowance(second), None)
            .await
        {
            panic!("second sync did not send: {error}");
        }

        assert_eq!(gateway.transport.sent_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn broker_spacing_starts_when_the_previous_status_is_committed() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).jumping(Duration::from_secs(7)),
        );
        let start = time.now();

        gateway.send(&operations(), None).await.expect("first sent");
        gateway
            .send(&operations(), None)
            .await
            .expect("second sent");

        assert_eq!(
            gateway.transport.sent_at(),
            vec![start, start + Duration::from_secs(8)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn operations_service_never_exceeds_fifty_in_any_minute() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let start = time.now();

        for _ in 0..160 {
            gateway.send(&operations(), None).await.expect("sent");
        }

        let sent = gateway.transport.sent_at();
        assert_eq!(sent.len(), 160);
        assert_never_more_than(50, MINUTE, &sent);
        // The endpoint permit makes the old 100 ms hand-off margin
        // unnecessary: departures themselves are one second apart.
        assert_eq!(sent[49], start + Duration::from_secs(49));
        assert_eq!(sent[50], start + MINUTE);
    }

    #[tokio::test(start_paused = true)]
    async fn the_method_window_retains_its_oldest_reservation() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let start = time.now();
        for _ in 0..50 {
            gateway
                .send(&operations(), None)
                .await
                .expect("send inside method budget");
        }

        gateway
            .send(&operations(), None)
            .await
            .expect("send after the method window");

        let sent = gateway.transport.sent_at();
        assert_eq!(sent[50], start + MINUTE);
        assert_eq!(time.slept().last(), Some(&Duration::from_secs(11)));
    }

    #[tokio::test(start_paused = true)]
    async fn users_service_never_exceeds_twenty_five_in_any_minute() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = users();

        for _ in 0..60 {
            gateway.send(&request, None).await.expect("sent");
        }

        let sent = gateway.transport.sent_at();
        assert_never_more_than(25, MINUTE, &sent);
        assert_eq!(sent[25], sent[0] + MINUTE);
    }

    #[tokio::test(start_paused = true)]
    async fn two_t_invest_services_do_not_share_a_budget() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let start = time.now();

        for _ in 0..50 {
            gateway.send(&operations(), None).await.expect("sent");
        }
        gateway.send(&users(), None).await.expect("sent");

        assert_eq!(
            gateway.transport.sent_at().last(),
            Some(&(start + Duration::from_secs(50))),
            "the users method waited for the operations method's budget"
        );
    }

    #[test]
    fn finam_budgets_every_method_it_is_called_with_by_name() {
        for method in [
            "AccountsService.GetAccount",
            "AccountsService.Transactions",
            "AssetsService.GetAsset",
            "AuthService.Sessions",
        ] {
            let row = BUDGETS
                .iter()
                .find(|row| {
                    row.destination == Destination::FinamApi
                        && row.scope == MethodScope::Named(method)
                })
                .unwrap_or_else(|| panic!("{method} has no row"));
            assert_eq!((row.documented, row.used), (Some(200), 100), "{method}");
        }
    }

    #[test]
    fn a_finam_asset_path_is_budgeted_as_the_asset_read() {
        assert_eq!(
            broker_budget_key(Destination::FinamApi, "/v1/assets/SBER@MISX"),
            Some("AssetsService.GetAsset")
        );
    }

    #[test]
    fn a_finam_asset_path_that_is_not_one_segment_is_refused_the_budget() {
        let refused: &[&str] = &[
            // Not an asset path at all, or a symbol missing.
            "/v1/assets",
            "/v1/assets/",
            "/v1/invented",
            // Dot segments, and a second segment.
            "/v1/assets/.",
            "/v1/assets/..",
            "/v1/assets/SBER@MISX/extra",
            // What a raw path may never carry.
            "/v1/assets/SBER%40MISX",
            "/v1/assets/SBER?MISX",
            "/v1/assets/SBER#MISX",
            "/v1/assets/SBER\\MISX",
            "/v1/assets/SBER\nMISX",
        ];
        for path in refused {
            assert_eq!(
                broker_budget_key(Destination::FinamApi, path),
                None,
                "{path}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_finam_method_missing_from_the_table_is_refused_without_sending() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::get(Destination::FinamApi, "/v1/invented")
            .with_request_allowance(RequestAllowance::new(u32::MAX));

        let refused = gateway.send(&request, None).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::UnknownBudget {
                    destination: Destination::FinamApi,
                    ref path
                }) if path == "/v1/invented"
            ),
            "{refused:?}"
        );
        assert_eq!(gateway.transport.sent_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn moex_keeps_one_request_per_hundred_milliseconds() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        for _ in 0..3 {
            gateway.send(&request, None).await.expect("sent");
        }

        let sent = gateway.transport.sent_at();
        assert_eq!(sent[1] - sent[0], Duration::from_millis(100));
        assert_eq!(sent[2] - sent[1], Duration::from_millis(100));
    }

    #[tokio::test(start_paused = true)]
    async fn a_method_missing_from_the_table_is_refused_without_sending() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::get(Destination::TinkoffProd, "/InstrumentsService/Invented")
            .with_request_allowance(RequestAllowance::new(u32::MAX));
        let refused = gateway
            .send(&request, None)
            .await
            .expect_err("no budget is recorded for it");

        assert!(matches!(
            refused,
            GatewayError::UnknownBudget {
                destination: Destination::TinkoffProd,
                path
            } if path == "/InstrumentsService/Invented"
        ));
        assert_eq!(gateway.transport.sent_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_destination_missing_from_the_table_is_refused_without_sending() {
        let time = FakeTime::new();
        // Every destination has a row in the documented table, so the missing
        // one is made with a table that holds only MOEX.
        let moex_only: &'static [Budget] = Box::leak(Box::new([Budget {
            destination: Destination::MoexIss,
            scope: MethodScope::Shared,
            documented: None,
            used: 1,
            window: Duration::from_millis(100),
        }]));
        let gateway = Gateway::with_parts(
            Scripted::answering(&time, 200),
            moex_only,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            BrokerEgress::Off,
        )
        .expect("a one-row table is valid");
        let request = HttpRequest::get(Destination::TinvestContract, "/contract.proto");

        let refused = gateway.send(&request, None).await;

        assert!(matches!(refused, Err(GatewayError::UnknownBudget { .. })));
        assert_eq!(gateway.transport.sent_count(), 0);
    }

    #[test]
    fn every_destination_has_a_row_in_the_documented_table() {
        for destination in Destination::ALL {
            assert!(
                BUDGETS.iter().any(|row| row.destination == destination),
                "{destination:?} has no budget: every call to it would be refused"
            );
        }
    }

    fn table_with(row: Budget) -> Result<BudgetTable, GatewayError> {
        BudgetTable::new(Box::leak(Box::new([row])))
    }

    const ROW: Budget = Budget {
        destination: Destination::FinamApi,
        scope: MethodScope::Named("AccountsService.GetAccount"),
        documented: Some(10),
        used: 5,
        window: MINUTE,
    };

    // --- one in flight ----------------------------------------------------

    fn users() -> HttpRequest {
        HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
            crate::RequestBody::Json("{}".to_owned()),
        )
        .with_request_allowance(RequestAllowance::new(u32::MAX))
        .idempotent()
    }

    /// The fake endpoint does see two requests at once when their hosts have
    /// independent lanes.
    #[tokio::test(start_paused = true)]
    async fn calls_to_different_destinations_are_in_flight_together() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let operations = operations();
        let finam = HttpRequest::get(Destination::FinamApi, "/v1/accounts/account-id")
            .with_request_allowance(RequestAllowance::new(u32::MAX));

        let (first, second) =
            tokio::join!(gateway.send(&operations, None), gateway.send(&finam, None),);

        first.expect("sent");
        second.expect("sent");
        assert_eq!(gateway.transport.most_in_flight.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_two_cbr_destinations_share_one_host_and_so_one_lane() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let scripts = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp");
        let daily_info = HttpRequest::get(Destination::CbrDailyInfo, "/DailyInfoWebServ/");

        let (first, second) = tokio::join!(
            gateway.send(&scripts, None),
            gateway.send(&daily_info, None),
        );

        first.expect("sent");
        second.expect("sent");
        assert_eq!(gateway.transport.most_in_flight.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_two_cbr_destinations_draw_on_one_budget() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let scripts = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp");
        let daily_info = HttpRequest::get(Destination::CbrDailyInfo, "/DailyInfoWebServ/");

        gateway.send(&scripts, None).await.expect("sent");
        gateway.send(&daily_info, None).await.expect("sent");

        let sent = gateway.transport.sent_at();
        assert_eq!(sent[1] - sent[0], Duration::from_millis(100));
    }

    // --- retries ----------------------------------------------------------

    fn with_retry_after(status: u16, after: Duration) -> HttpResponse {
        HttpResponse {
            retry_after: Some(after),
            ..self::status(status)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn each_broker_gateway_failure_is_retried_after_the_first_backoff() {
        for transient in [500, 502, 503, 504] {
            let time = FakeTime::new();
            let gateway = gateway(
                &time,
                Scripted::answering(&time, 200).then(Ok(status(transient))),
            );

            let response = gateway.send(&operations(), None).await;

            assert_eq!(
                response.expect("the retry succeeded").status,
                200,
                "status {transient}"
            );
            assert_eq!(gateway.transport.sent_count(), 2, "status {transient}");
            assert_eq!(time.slept(), [Duration::from_secs(1)], "status {transient}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_broker_429_returns_a_minute_pause_without_retrying() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 429));

        let refused = gateway
            .send(&operations(), None)
            .await
            .expect_err("the host is paused");

        assert!(matches!(
            refused,
            GatewayError::BrokerHostPaused {
                attempts: 1,
                retry_after,
                ..
            } if retry_after == MINUTE
        ));
        assert_eq!(gateway.transport.sent_count(), 1);
        assert!(time.slept().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_on_a_server_error_is_persisted_before_returning() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(503, Duration::from_secs(30)))),
        );

        let first = gateway
            .send(&operations(), None)
            .await
            .expect_err("retry-after pauses even on a server error");
        let second = gateway
            .send(&operations(), None)
            .await
            .expect_err("the durable pause prevents another send");

        assert!(matches!(
            first,
            GatewayError::BrokerHostPaused { attempts: 1, retry_after, .. }
                if retry_after == Duration::from_secs(30)
        ));
        assert!(matches!(
            second,
            GatewayError::BrokerHostPaused { attempts: 0, retry_after, .. }
                if retry_after == Duration::from_secs(30)
        ));
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_network_failure_or_timeout_closes_without_a_one_second_retry() {
        for failure in [HttpError::Network, HttpError::Timeout] {
            let time = FakeTime::new();
            let gateway = gateway(&time, Scripted::answering(&time, 200).then(Err(failure)));

            let refused = gateway
                .send(&operations(), None)
                .await
                .expect_err("an outcome without a status is unresolved");

            assert!(matches!(
                refused,
                GatewayError::BrokerHostClosed {
                    reason: "an earlier request has no committed status",
                    attempts: 1,
                    retry_after,
                    ..
                } if retry_after == Duration::from_secs(90)
            ));
            assert_eq!(gateway.transport.sent_count(), 1);
            assert!(time.slept().is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn each_permanent_status_returns_at_once() {
        for permanent in [400, 401, 403, 404, 422] {
            let time = FakeTime::new();
            let gateway = gateway(&time, Scripted::answering(&time, permanent));

            let refused = gateway
                .send(&operations(), None)
                .await
                .expect_err("a permanent status is a refusal");

            assert!(
                matches!(
                    refused,
                    GatewayError::Rejected { status, attempts: 1, .. } if status == permanent
                ),
                "status {permanent}: {refused:?}"
            );
            assert!(!refused.is_transient());
            assert_eq!(refused.retry_after(), None);
            assert_eq!(gateway.transport.sent_count(), 1, "status {permanent}");
            assert!(time.slept().is_empty(), "status {permanent}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejection_keeps_the_body_for_the_source_to_read() {
        let time = FakeTime::new();
        let body = br#"{"code":"40003"}"#.to_vec();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                body: body.clone(),
                ..status(401)
            })),
        );

        let refused = gateway
            .send(&operations(), None)
            .await
            .expect_err("401 is a refusal");

        match refused {
            GatewayError::Rejected { body: kept, .. } => assert_eq!(kept.as_bytes(), body),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejected_body_is_kept_without_the_bearer_secret() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                body: br#"{"token":"t.invented","again":"t.invented"}"#.to_vec(),
                ..status(400)
            })),
        );
        let request = operations().with_bearer("t.invented");

        let refused = gateway
            .send(&request, None)
            .await
            .expect_err("400 is a refusal");

        match refused {
            GatewayError::Rejected { body, .. } => assert_eq!(
                body.as_bytes(),
                br#"{"token":"<redacted>","again":"<redacted>"}"#
            ),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_secret_leaves_a_rejected_body_as_it_came() {
        let body = RejectedBody::without_secret(b"refused", Some(&Secret::new("")));
        assert_eq!(body.as_bytes(), b"refused");
    }

    #[test]
    fn a_rejected_body_with_no_bearer_is_kept_as_it_came() {
        assert_eq!(
            RejectedBody::without_secret(b"refused", None).as_bytes(),
            b"refused"
        );
    }

    #[test]
    fn a_rejected_body_prints_its_length_only() {
        let body = RejectedBody(b"Main".to_vec());
        assert_eq!(format!("{body:?}"), "RejectedBody(<4 bytes>)");
    }

    #[tokio::test(start_paused = true)]
    async fn a_transport_that_cannot_be_built_is_not_retried() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Err(HttpError::ClientNotBuilt("no roots".into()))),
        );

        let refused = gateway
            .send(&operations(), None)
            .await
            .expect_err("a client that cannot be built stays unbuilt");

        assert!(matches!(
            refused,
            GatewayError::Transport { attempts: 1, .. }
        ));
        assert!(!refused.is_transient());
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_broker_transient_failure_is_tried_at_most_three_times() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));

        let refused = gateway
            .send(&operations(), None)
            .await
            .expect_err("every attempt failed");

        assert_eq!(gateway.transport.sent_count(), 3);
        assert_eq!(
            time.slept(),
            [Duration::from_secs(1), Duration::from_secs(2)],
            "exponential backoff"
        );
        assert!(refused.is_transient());
        assert_eq!(refused.status(), Some(503));
        assert_eq!(refused.attempts(), 3);
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(4)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_broker_transient_failure_keeps_five_attempts() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        let refused = gateway
            .send(&request, None)
            .await
            .expect_err("every attempt failed");

        assert_eq!(gateway.transport.sent_count(), 5);
        assert_eq!(
            time.slept(),
            [1, 2, 4, 8].map(Duration::from_secs),
            "the established non-broker policy stays unchanged"
        );
        assert_eq!(refused.attempts(), 5);
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(16)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_not_marked_idempotent_is_sent_once() {
        for (failure, unknown) in [(Ok(status(503)), false), (Err(HttpError::Timeout), true)] {
            let time = FakeTime::new();
            let gateway = gateway(&time, Scripted::answering(&time, 200).then(failure));
            let transfer = HttpRequest::post(
                Destination::FinamApi,
                "/v1/accounts/account-id/transactions",
                crate::RequestBody::Json("{}".to_owned()),
            )
            .with_request_allowance(RequestAllowance::new(u32::MAX));

            let refused = gateway
                .send(&transfer, None)
                .await
                .expect_err("a second send could do the thing twice");

            assert_eq!(gateway.transport.sent_count(), 1);
            assert!(time.slept().is_empty());
            if unknown {
                assert!(matches!(refused, GatewayError::BrokerHostClosed { .. }));
            } else {
                assert!(matches!(
                    refused,
                    GatewayError::Exhausted { attempts: 1, .. }
                ));
                assert_eq!(refused.retry_after(), Some(FIRST_BACKOFF));
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_network_failure_carries_no_status_and_is_not_retried() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Err(HttpError::Network)),
        );

        let refused = gateway
            .send(&operations(), None)
            .await
            .expect_err("the outcome has no committed status");

        assert!(refused.is_transient());
        assert_eq!(refused.status(), None);
        assert_eq!(refused.attempts(), 1);
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_broker_named_delay_wins_over_the_computed_backoff() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(429, Duration::from_secs(7))))
                .then(Ok(with_retry_after(503, Duration::from_millis(250)))),
        );
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        gateway
            .send(&request, None)
            .await
            .expect("the third attempt succeeded");

        assert_eq!(
            time.slept(),
            [Duration::from_secs(7), Duration::from_millis(250)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_broker_named_delay_is_waited_in_full_up_to_fifteen_minutes() {
        for named in [Duration::from_secs(300), MAX_NAMED_WAIT] {
            let time = FakeTime::new();
            let gateway = gateway(
                &time,
                Scripted::answering(&time, 200).then(Ok(with_retry_after(429, named))),
            );
            let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

            gateway
                .send(&request, None)
                .await
                .expect("the retry succeeded");

            assert_eq!(time.slept(), [named]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_broker_named_delay_over_fifteen_minutes_is_returned_not_waited() {
        let time = FakeTime::new();
        let long = MAX_NAMED_WAIT + Duration::from_secs(1);
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(with_retry_after(429, long))),
        );
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        let refused = gateway
            .send(&request, None)
            .await
            .expect_err("the gateway holds nobody that long");

        assert!(time.slept().is_empty());
        assert_eq!(gateway.transport.sent_count(), 1);
        assert!(
            matches!(
                refused,
                GatewayError::Deferred {
                    status: Some(429),
                    attempts: 1,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(refused.retry_after(), Some(long));
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_broker_named_delay_holds_back_every_caller_of_the_lane() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(429, Duration::from_secs(30)))),
        );
        let rates = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp");
        let soap = HttpRequest::get(Destination::CbrDailyInfo, "/DailyInfoWebServ/");
        let start = time.now();

        let (first, second) = tokio::join!(gateway.send(&rates, None), gateway.send(&soap, None),);

        first.expect("sent");
        second.expect("sent");
        let sent = gateway.transport.sent_at();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0], start);
        assert!(
            sent[1..]
                .iter()
                .all(|at| *at >= start + Duration::from_secs(30)),
            "a caller went ahead of the named reset: {:?}",
            sent.iter().map(|at| *at - start).collect::<Vec<_>>()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_named_delay_refuses_later_calls_until_the_persisted_pause_ends() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(503, Duration::from_secs(20)))),
        );
        let once = HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor",
            crate::RequestBody::Json("{}".to_owned()),
        )
        .with_request_allowance(RequestAllowance::new(u32::MAX));

        let first = gateway
            .send(&once, None)
            .await
            .expect_err("sent once and paused");
        let blocked = gateway
            .send(&operations(), None)
            .await
            .expect_err("the pause is not waited inside the process");
        assert_eq!(first.retry_after(), Some(Duration::from_secs(20)));
        assert!(matches!(
            blocked,
            GatewayError::BrokerHostPaused {
                attempts: 0,
                retry_after,
                ..
            } if retry_after == Duration::from_secs(20)
        ));
        assert_eq!(gateway.transport.sent_count(), 1);

        time.advance(Duration::from_secs(20));
        gateway
            .send(&operations(), None)
            .await
            .expect("sent after the reset");
        assert_eq!(gateway.transport.sent_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_later_call_is_refused_rather_than_held_past_fifteen_minutes() {
        let time = FakeTime::new();
        let long = Duration::from_secs(3600);
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(with_retry_after(429, long))),
        );
        let _ = call(&gateway).await;
        time.advance(Duration::from_secs(600));

        let refused = gateway
            .send(&users(), None)
            .await
            .expect_err("the reset is fifty minutes away");

        assert_eq!(gateway.transport.sent_count(), 1);
        assert!(
            matches!(refused, GatewayError::BrokerHostPaused { attempts: 0, .. }),
            "{refused:?}"
        );
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(3000)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_named_delay_that_would_end_past_the_deadline_is_not_waited() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(429, Duration::from_secs(90)))),
        );
        let deadline = time.now() + Duration::from_secs(60);

        let refused = gateway
            .send(&operations(), Some(deadline))
            .await
            .expect_err("the reset falls after the deadline");

        assert!(time.slept().is_empty());
        assert!(
            matches!(refused, GatewayError::BrokerHostPaused { attempts: 1, .. }),
            "{refused:?}"
        );
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(90)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_is_returned_as_it_came() {
        let time = FakeTime::new();
        let answer = HttpResponse {
            body: b"{}".to_vec(),
            ..status(204)
        };
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(answer.clone())),
        );

        let response = gateway
            .send(&operations(), None)
            .await
            .expect("2xx is a success");

        // Everything the transport gave is returned as it came, with the
        // one addition: the moment a caller dates the answer by.
        let mut expected = answer;
        expected.observed_at = response.observed_at;
        assert_eq!(response, expected);
        assert!(response.observed_at.is_some(), "a live answer is dated");
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_carries_neither_the_token_nor_the_body() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                body: b"account Main balance".to_vec(),
                ..status(403)
            })),
        );
        let request = operations().with_bearer("t.invented-token");

        let refused = gateway
            .send(&request, None)
            .await
            .expect_err("403 is a refusal");

        let rendered = format!("{refused} {refused:?}");
        assert!(!rendered.contains("invented-token"), "{rendered}");
        assert!(!rendered.contains("balance"), "{rendered}");
    }

    // --- circuit breaker --------------------------------------------------

    /// A script of `calls` calls whose every attempt fails with 503.
    fn failing_calls(script: Scripted, calls: usize) -> Scripted {
        (0..calls * ATTEMPTS as usize).fold(script, |script, _| script.then(Ok(status(503))))
    }

    async fn call(gateway: &Gateway<Scripted>) -> Result<HttpResponse, GatewayError> {
        gateway.send(&operations(), None).await
    }

    const COOL_DOWN: Duration = Duration::from_secs(5 * 60);

    #[tokio::test(start_paused = true)]
    async fn five_failed_calls_open_the_breaker_and_it_refuses_without_sending() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        for _ in 0..5 {
            assert!(matches!(
                call(&gateway).await,
                Err(GatewayError::Exhausted { .. })
            ));
        }
        let sent = gateway.transport.sent_count();

        let refused = call(&gateway).await.expect_err("the breaker is open");

        assert!(
            matches!(
                refused,
                GatewayError::CircuitOpen {
                    destination: Destination::TinkoffProd,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(
            gateway.transport.sent_count(),
            sent,
            "an open breaker sent a request"
        );
        assert!(refused.is_transient());
        assert_eq!(refused.retry_after(), Some(COOL_DOWN));
    }

    #[tokio::test(start_paused = true)]
    async fn four_failed_calls_leave_the_breaker_closed() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        for _ in 0..4 {
            let _ = call(&gateway).await;
        }
        let sent = gateway.transport.sent_count();

        let _ = call(&gateway).await;

        assert_eq!(gateway.transport.sent_count(), sent + ATTEMPTS as usize);
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_between_failures_starts_the_count_again() {
        let time = FakeTime::new();
        let script = failing_calls(Scripted::answering(&time, 503), 4).then(Ok(status(200)));
        let gateway = gateway(&time, script);
        for _ in 0..4 {
            let _ = call(&gateway).await;
        }
        call(&gateway).await.expect("the fifth call succeeds");
        for _ in 0..4 {
            let _ = call(&gateway).await;
        }
        let sent = gateway.transport.sent_count();

        let tenth = call(&gateway).await;

        assert!(
            matches!(tenth, Err(GatewayError::Exhausted { .. })),
            "{tenth:?}"
        );
        assert_eq!(gateway.transport.sent_count(), sent + ATTEMPTS as usize);
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_broker_permanent_refusal_does_not_count_towards_the_breaker() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 404));
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        for _ in 0..6 {
            let refused = gateway.send(&request, None).await;
            assert!(
                matches!(refused, Err(GatewayError::Rejected { .. })),
                "{refused:?}"
            );
        }
        assert_eq!(gateway.transport.sent_count(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn an_open_breaker_leaves_other_destinations_alone() {
        let time = FakeTime::new();
        let gateway = gateway(&time, failing_calls(Scripted::answering(&time, 200), 5));
        for _ in 0..5 {
            let _ = call(&gateway).await;
        }
        let finam = HttpRequest::get(Destination::FinamApi, "/v1/accounts/account-id")
            .with_request_allowance(RequestAllowance::new(u32::MAX));

        gateway
            .send(&finam, None)
            .await
            .expect("Finam is not the destination that failed");
    }

    #[tokio::test(start_paused = true)]
    async fn the_breaker_refuses_for_the_whole_cool_down_and_then_lets_a_call_through() {
        let time = FakeTime::new();
        let gateway = gateway(&time, failing_calls(Scripted::answering(&time, 200), 5));
        for _ in 0..5 {
            let _ = call(&gateway).await;
        }

        time.advance(COOL_DOWN - Duration::from_millis(1));
        let early = call(&gateway).await;
        assert!(
            matches!(early, Err(GatewayError::CircuitOpen { .. })),
            "{early:?}"
        );
        assert_eq!(
            early.expect_err("open").retry_after(),
            Some(Duration::from_millis(1))
        );

        time.advance(Duration::from_millis(1));
        call(&gateway).await.expect("the cool-down is over");
        call(&gateway).await.expect("a success closed the breaker");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_right_after_the_cool_down_opens_the_breaker_again() {
        let time = FakeTime::new();
        let gateway = gateway(&time, failing_calls(Scripted::answering(&time, 200), 6));
        for _ in 0..5 {
            let _ = call(&gateway).await;
        }
        time.advance(COOL_DOWN);
        assert!(matches!(
            call(&gateway).await,
            Err(GatewayError::Exhausted { .. })
        ));
        let sent = gateway.transport.sent_count();

        let refused = call(&gateway).await;

        assert!(
            matches!(refused, Err(GatewayError::CircuitOpen { .. })),
            "{refused:?}"
        );
        assert_eq!(gateway.transport.sent_count(), sent);
    }

    #[tokio::test(start_paused = true)]
    async fn a_breaker_that_opens_during_a_call_stops_its_remaining_attempts() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        let (operations, users) = (operations(), users());
        for _ in 0..4 {
            let _ = call(&gateway).await;
        }
        let sent = gateway.transport.sent_count();

        // Two calls at once: the first to finish failing opens the breaker,
        // and the other's next attempt is refused rather than sent.
        let (first, second) =
            tokio::join!(gateway.send(&operations, None), gateway.send(&users, None),);

        let refused = [first, second]
            .into_iter()
            .filter(|result| matches!(result, Err(GatewayError::CircuitOpen { .. })))
            .count();
        assert_eq!(refused, 1);
        assert!(gateway.transport.sent_count() < sent + 2 * ATTEMPTS as usize);
    }

    // --- deadline ---------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_deadline_already_reached_sends_nothing() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));

        let refused = gateway
            .send(&operations(), Some(time.now()))
            .await
            .expect_err("the deadline is now");

        assert!(
            matches!(refused, GatewayError::DeadlineReached { attempts: 0, .. }),
            "{refused:?}"
        );
        assert_eq!(gateway.transport.sent_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_backoff_that_would_end_past_the_deadline_is_not_waited() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        let deadline = time.now() + Duration::from_millis(2500);

        let refused = gateway
            .send(&operations(), Some(deadline))
            .await
            .expect_err("the third attempt would start past the deadline");

        // Attempts at 0 s and 1 s; the third would start at 3 s.
        assert_eq!(gateway.transport.sent_count(), 2);
        assert_eq!(time.slept(), [Duration::from_secs(1)]);
        assert!(gateway.transport.sent_at().iter().all(|at| *at < deadline));
        assert!(
            matches!(
                refused,
                GatewayError::DeadlineReached {
                    attempts: 2,
                    status: Some(503),
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(2)));
        let message = refused.to_string();
        assert!(message.contains("deadline reached"), "{message}");
        assert!(message.contains("2 attempts"), "{message}");
    }

    #[tokio::test(start_paused = true)]
    async fn no_attempt_starts_exactly_at_the_deadline() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        let deadline = time.now() + Duration::from_secs(3);

        let refused = gateway.send(&operations(), Some(deadline)).await;

        assert!(matches!(
            refused,
            Err(GatewayError::DeadlineReached { attempts: 2, .. })
        ));
        assert_eq!(gateway.transport.sent_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_outbound_tally_wait_that_would_end_past_the_deadline_is_not_waited() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        for _ in 0..50 {
            call(&gateway).await.expect("within the budget");
        }
        let slept_before_refusal = time.slept();
        let deadline = time.now() + Duration::from_secs(6);

        let refused = gateway
            .send(&operations(), Some(deadline))
            .await
            .expect_err("the tally budget frees a request only at the rolling minute boundary");

        assert!(
            matches!(
                refused,
                GatewayError::DeadlineReached {
                    attempts: 0,
                    status: None,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(11)));
        assert_eq!(gateway.transport.sent_count(), 50);
        assert_eq!(time.slept(), slept_before_refusal);
    }

    #[tokio::test(start_paused = true)]
    async fn an_attempt_in_flight_at_the_deadline_is_abandoned_at_the_deadline() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).taking(Duration::from_secs(30)),
        );
        let deadline = time.now() + Duration::from_secs(10);

        let refused = gateway
            .send(&operations(), Some(deadline))
            .await
            .expect_err("the answer would come twenty seconds past the deadline");

        assert_eq!(
            time.now(),
            deadline,
            "the call did not return at its deadline"
        );
        assert!(
            matches!(
                refused,
                GatewayError::DeadlineReached {
                    attempts: 1,
                    status: None,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert!(refused.is_transient());
    }

    #[tokio::test(start_paused = true)]
    async fn an_attempt_that_answers_before_the_deadline_is_kept() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).taking(Duration::from_secs(9)),
        );
        let start = time.now();

        gateway
            .send(&operations(), Some(start + Duration::from_secs(10)))
            .await
            .expect("the answer came a second before the deadline");

        assert_eq!(time.now(), start + Duration::from_secs(9));
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_stamped_after_the_deadline_is_not_taken() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).jumping(Duration::from_secs(4 * 60)),
        );
        let deadline = time.now() + Duration::from_secs(60);

        let refused = gateway.send(&operations(), Some(deadline)).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::DeadlineReached { attempts: 1, .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_at_the_exact_deadline_is_not_taken() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).taking(Duration::from_secs(10)),
        );
        let deadline = time.now() + Duration::from_secs(10);

        let refused = gateway.send(&operations(), Some(deadline)).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::DeadlineReached { attempts: 1, .. })
            ),
            "the deadline did not win the tie: {refused:?}"
        );
        assert_eq!(time.now(), deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_stamped_exactly_at_the_deadline_is_not_taken() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).jumping(Duration::from_secs(60)),
        );
        let deadline = time.now() + Duration::from_secs(60);

        let refused = gateway.send(&operations(), Some(deadline)).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::DeadlineReached { attempts: 1, .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_for_the_lane_ends_at_the_deadline() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).taking(Duration::from_secs(60)),
        );
        let holding = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp");
        let queued = HttpRequest::get(Destination::CbrDailyInfo, "/DailyInfoWebServ/");
        let deadline = time.now() + Duration::from_secs(10);

        let (first, (second, returned)) = tokio::join!(gateway.send(&holding, None), async {
            let refused = gateway.send(&queued, Some(deadline)).await;
            (refused, time.now())
        },);

        first.expect("the call holding the lane had no deadline");
        assert_eq!(returned, deadline, "the queued call outlived its deadline");
        assert!(
            matches!(
                second,
                Err(GatewayError::DeadlineReached {
                    attempts: 0,
                    status: None,
                    ..
                })
            ),
            "{second:?}"
        );
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_outbound_tally_wait_whose_timer_fires_late_ends_at_the_deadline() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        for _ in 0..50 {
            call(&gateway).await.expect("within the budget");
        }
        let deadline = time.now() + Duration::from_secs(20);
        time.lagging(Duration::from_secs(30));

        let refused = gateway.send(&operations(), Some(deadline)).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::DeadlineReached {
                    attempts: 0,
                    status: None,
                    ..
                })
            ),
            "{refused:?}"
        );
        assert_eq!(gateway.transport.sent_count(), 50);
    }

    #[tokio::test(start_paused = true)]
    async fn a_broker_429_returns_without_waiting_even_if_timer_lags() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(with_retry_after(429, MINUTE))),
        );
        let deadline = time.now() + Duration::from_secs(100);
        time.lagging(MINUTE);

        let refused = gateway.send(&operations(), Some(deadline)).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::BrokerHostPaused { attempts: 1, .. })
            ),
            "{refused:?}"
        );
        assert_eq!(gateway.transport.sent_count(), 1);
        assert!(time.slept().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_backoff_whose_timer_fires_late_ends_at_the_deadline() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200).then(Ok(status(503))));
        let deadline = time.now() + Duration::from_secs(2);
        time.lagging(Duration::from_secs(5));

        let refused = gateway.send(&operations(), Some(deadline)).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::DeadlineReached {
                    attempts: 1,
                    status: Some(503),
                    ..
                })
            ),
            "{refused:?}"
        );
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deadline_with_room_leaves_the_retries_alone() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200).then(Ok(status(503))));
        let deadline = time.now() + Duration::from_millis(1_101);

        gateway
            .send(&operations(), Some(deadline))
            .await
            .expect("the retry starts before the deadline");

        assert_eq!(gateway.transport.sent_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_cut_short_by_its_deadline_does_not_count_towards_the_breaker() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        for _ in 0..5 {
            let deadline = time.now() + Duration::from_millis(500);
            let cut = gateway.send(&operations(), Some(deadline)).await;
            assert!(
                matches!(cut, Err(GatewayError::DeadlineReached { .. })),
                "{cut:?}"
            );
        }
        let sent = gateway.transport.sent_count();

        let _ = call(&gateway).await;

        assert_eq!(gateway.transport.sent_count(), sent + ATTEMPTS as usize);
    }

    // --- logs -------------------------------------------------------------

    thread_local! {
        /// What this test's thread logged, while a `Log` holds it.
        static CAPTURED: std::cell::RefCell<Option<Vec<u8>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Everything this thread logs while it is held, as the operator's log
    /// shows it.
    ///
    /// One global subscriber writing into a per-thread buffer, rather than a
    /// subscriber set per test: tracing caches each event's interest for the
    /// whole process, and a test thread with no subscriber of its own can
    /// cache "never" for an event another thread's scoped subscriber wants.
    struct Log;

    struct Sink;

    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            CAPTURED.with(|captured| {
                if let Some(lines) = captured.borrow_mut().as_mut() {
                    lines.extend_from_slice(bytes);
                }
            });
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Log {
        fn capture() -> Self {
            static INSTALLED: std::sync::Once = std::sync::Once::new();
            INSTALLED.call_once(|| {
                let subscriber = tracing_subscriber::fmt()
                    .with_writer(|| Sink)
                    .with_ansi(false)
                    .without_time()
                    .with_max_level(tracing::Level::INFO)
                    .finish();
                tracing::subscriber::set_global_default(subscriber)
                    .expect("no other test installs a subscriber");
            });
            CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
            Self
        }

        fn text(&self) -> String {
            let lines = CAPTURED.with(|captured| captured.borrow().clone().unwrap_or_default());
            String::from_utf8(lines).expect("UTF-8")
        }

        /// The one line holding `message`, which must hold every fragment.
        fn assert_line(&self, level: &str, message: &str, fragments: &[&str]) {
            let text = self.text();
            let line = text
                .lines()
                .find(|line| line.contains(message))
                .unwrap_or_else(|| panic!("no {message:?} in the log:\n{text}"));
            assert!(line.contains(level), "{line}");
            for fragment in fragments {
                assert!(line.contains(fragment), "no {fragment:?} in {line}");
            }
        }
    }

    impl Drop for Log {
        fn drop(&mut self) {
            CAPTURED.with(|captured| *captured.borrow_mut() = None);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_is_logged_at_warn_with_its_fields() {
        let log = Log::capture();
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200).then(Ok(status(503))));

        call(&gateway).await.expect("the retry succeeded");

        log.assert_line(
            "WARN",
            "outbound call retries",
            &[
                "destination=TinkoffProd",
                "method=\"/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor\"",
                "attempt=1",
                "wait_ms=1000",
                "status=503",
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_broker_wait_over_a_second_is_logged_at_info_with_its_reason() {
        let log = Log::capture();
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(status(503)))
                .then(Ok(status(503)))
                .then(Ok(with_retry_after(429, Duration::from_secs(30)))),
        );
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        gateway
            .send(&request, None)
            .await
            .expect("the fourth attempt succeeded");

        let waits: Vec<String> = log
            .text()
            .lines()
            .filter(|line| line.contains("outbound call waits"))
            .map(str::to_owned)
            .collect();
        // The first backoff is one second, not over it.
        assert_eq!(waits.len(), 2, "{waits:?}");
        assert!(waits[0].contains("INFO"), "{waits:?}");
        assert!(waits[0].contains("reason=\"backoff\""), "{waits:?}");
        assert!(waits[0].contains("wait_ms=2000"), "{waits:?}");
        // The wait before the third attempt, after the second failed.
        assert!(waits[0].contains("attempt=3"), "{waits:?}");
        assert!(waits[1].contains("reason=\"named reset\""), "{waits:?}");
        assert!(waits[1].contains("wait_ms=30000"), "{waits:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_outbound_tally_wait_is_logged_with_its_reason() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        for _ in 0..50 {
            call(&gateway).await.expect("within the budget");
        }
        let log = Log::capture();

        call(&gateway).await.expect("sent after the minute");

        log.assert_line(
            "INFO",
            "outbound call waits",
            &["reason=\"outbound tally\"", "wait_ms=11000", "attempt=1"],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_for_the_lane_is_logged_with_its_reason() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).taking(Duration::from_secs(5)),
        );
        let holding = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp");
        let queued = HttpRequest::get(Destination::CbrDailyInfo, "/DailyInfoWebServ/");
        let log = Log::capture();

        let (first, second) =
            tokio::join!(gateway.send(&holding, None), gateway.send(&queued, None),);

        first.expect("sent");
        second.expect("sent");
        log.assert_line(
            "INFO",
            "outbound call waits",
            &[
                "reason=\"lane\"",
                "wait_ms=5000",
                "method=\"/DailyInfoWebServ/\"",
                "attempt=1",
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_breaker_opening_and_closing_is_logged() {
        let time = FakeTime::new();
        let gateway = gateway(&time, failing_calls(Scripted::answering(&time, 200), 5));
        let log = Log::capture();
        for _ in 0..5 {
            let _ = call(&gateway).await;
        }
        log.assert_line(
            "WARN",
            "breaker opened",
            &["destination=TinkoffProd", "wait_ms=300000", "status=503"],
        );
        assert_eq!(log.text().matches("breaker opened").count(), 1);

        time.advance(COOL_DOWN);
        call(&gateway).await.expect("the cool-down is over");

        log.assert_line("INFO", "breaker closed", &["destination=TinkoffProd"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_with_the_breaker_closed_logs_no_closing() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200).then(Ok(status(503))));
        let log = Log::capture();

        call(&gateway).await.expect("the retry succeeded");

        assert!(!log.text().contains("breaker closed"), "{}", log.text());
    }

    #[tokio::test(start_paused = true)]
    async fn each_transient_refusal_is_logged_at_warn_with_its_kind() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));
        let log = Log::capture();
        for _ in 0..5 {
            let _ = call(&gateway).await;
        }
        let _ = call(&gateway).await;

        log.assert_line(
            "WARN",
            "refusal=\"exhausted\"",
            &[
                "outbound call refused",
                "attempt=3",
                "wait_ms=4000",
                "status=503",
            ],
        );
        log.assert_line(
            "WARN",
            "refusal=\"circuit open\"",
            &["outbound call refused", "attempt=0", "wait_ms=300000"],
        );

        let late = FakeTime::new();
        let gateway = self::gateway(&late, Scripted::answering(&late, 200));
        let _ = gateway.send(&operations(), Some(late.now())).await;
        log.assert_line("WARN", "refusal=\"deadline\"", &["outbound call refused"]);

        let long = MAX_NAMED_WAIT + Duration::from_secs(1);
        let gateway = self::gateway(
            &late,
            Scripted::answering(&late, 200).then(Ok(with_retry_after(429, long))),
        );
        let _ = call(&gateway).await;
        log.assert_line(
            "WARN",
            "refusal=\"broker host paused\"",
            &[
                "outbound call refused",
                "attempt=1",
                "wait_ms=901000",
                "status=429",
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_permanent_refusal_is_logged_once_at_warn_with_its_fields() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 404));
        let log = Log::capture();

        let _ = call(&gateway).await;

        log.assert_line(
            "WARN",
            "refusal=\"rejected\"",
            &[
                "outbound call refused",
                "destination=TinkoffProd",
                "method=\"/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor\"",
                "status=404",
                "attempt=1",
            ],
        );
        assert_eq!(log.text().matches("outbound call refused").count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn no_secret_and_no_body_reach_the_log() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                body: b"t.invented-token balance".to_vec(),
                ..status(401)
            })),
        );
        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor",
            crate::RequestBody::Json(r#"{"session":"s.invented-session"}"#.to_owned()),
        )
        .with_bearer("t.invented-token")
        .with_request_allowance(RequestAllowance::new(u32::MAX))
        .idempotent();
        let log = Log::capture();

        let _ = gateway.send(&request, None).await;

        let refused = log.text();
        assert!(refused.contains("refusal=\"rejected\""), "{refused}");
        assert!(!refused.contains("invented-token"), "{refused}");
        assert!(!refused.contains("invented-session"), "{refused}");
        assert!(!refused.contains("balance"), "{refused}");

        // The success line names the wire URL: a secret that travelled in
        // the query, the header or the body would reach it too.
        let log = Log::capture();
        gateway.send(&request, None).await.expect("answered");
        let answered = log.text();
        assert!(answered.contains("outbound call answered"), "{answered}");
        assert!(!answered.contains("invented-token"), "{answered}");
        assert!(!answered.contains("invented-session"), "{answered}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_destination_echoing_the_token_in_a_header_is_redacted_in_the_log() {
        let log = Log::capture();
        let time = FakeTime::new();
        let refusing = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                location: Some(
                    "https://invest-public-api.tbank.ru/again?t=t.invented-token".to_owned(),
                ),
                request_id: Some("t.invented-token".to_owned()),
                content_type: Some("text/html".to_owned()),
                ..status(308)
            })),
        );
        let request = operations().with_bearer("t.invented-token");

        let _ = refusing.send(&request, None).await;

        let refused = log.text();
        assert!(refused.contains("outbound call refused"), "{refused}");
        assert!(!refused.contains("invented-token"), "{refused}");
        assert!(
            refused.contains("location=\"https://invest-public-api.tbank.ru/again?t=<redacted>\""),
            "{refused}"
        );
        assert!(refused.contains("request_id=\"<redacted>\""), "{refused}");

        // The answered line cuts the token out of the id the same way.
        let log = Log::capture();
        let answering = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                request_id: Some("t.invented-token".to_owned()),
                ..status(200)
            })),
        );
        answering.send(&request, None).await.expect("answered");
        let answered = log.text();
        assert!(answered.contains("outbound call answered"), "{answered}");
        assert!(!answered.contains("invented-token"), "{answered}");
    }

    /// A transport that publishes a status line and then loses the body:
    /// the shape a stalled or truncated body arrives in.
    struct StatusLineThenBodyFailure {
        status: HttpResponse,
    }

    impl Transport for StatusLineThenBodyFailure {
        async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            Err(HttpError::Timeout)
        }

        fn send_observed<'a>(
            &'a self,
            _request: &'a HttpRequest,
            handoff: Box<dyn FnOnce() -> Result<(), HttpError> + Send + 'a>,
            observe: Box<dyn FnOnce(HttpResponse) + Send + 'a>,
        ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
            let status = self.status.clone();
            async move {
                handoff()?;
                observe(status);
                Err(HttpError::Timeout)
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_two_hundred_status_line_whose_body_is_lost_is_not_a_success() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            StatusLineThenBodyFailure {
                status: HttpResponse {
                    request_id: Some("req-invented-3".to_owned()),
                    ..status(200)
                },
            },
        );

        let refused = gateway.send(&operations(), None).await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::Exhausted {
                    attempts: ATTEMPTS,
                    ..
                })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_status_line_survives_a_lost_body_with_its_headers() {
        let log = Log::capture();
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            StatusLineThenBodyFailure {
                status: HttpResponse {
                    location: Some("/v1/moved".to_owned()),
                    request_id: Some("req-invented-4".to_owned()),
                    content_type: Some("text/html".to_owned()),
                    ..status(308)
                },
            },
        );

        let refused = gateway.send(&operations(), None).await;

        assert!(
            matches!(&refused, Err(GatewayError::Rejected { status: 308, .. })),
            "{refused:?}"
        );
        log.assert_line(
            "WARN",
            "refusal=\"rejected\"",
            &[
                "status=308",
                "location=\"/v1/moved\"",
                "content_type=\"text/html\"",
                "request_id=\"req-invented-4\"",
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_redirect_refusal_is_logged_with_the_location_it_pointed_at() {
        let log = Log::capture();
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                location: Some(
                    "/v1/accounts/act/transactions?interval.start_time=2026-09-01".to_owned(),
                ),
                content_type: Some("text/html".to_owned()),
                ..status(308)
            })),
        );

        let _ = call(&gateway).await;

        log.assert_line(
            "WARN",
            "refusal=\"rejected\"",
            &[
                "outbound call refused",
                "url=https://invest-public-api.tbank.ru/rest/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor",
                "status=308",
                "attempt=1",
                "elapsed_ms=",
                "request_id=\"-\"",
                "location=\"/v1/accounts/act/transactions?interval.start_time=2026-09-01\"",
                "content_type=\"text/html\"",
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_successful_call_is_logged_at_info_with_its_wire_url_and_request_id() {
        let log = Log::capture();
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(HttpResponse {
                request_id: Some("req-invented-2".to_owned()),
                ..status(200)
            })),
        );

        call(&gateway).await.expect("answered");

        log.assert_line(
            "INFO",
            "outbound call answered",
            &[
                "destination=TinkoffProd",
                "method=\"/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor\"",
                "url=https://invest-public-api.tbank.ru/rest/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor",
                "status=200",
                "attempt=1",
                "elapsed_ms=0",
                "request_id=\"req-invented-2\"",
            ],
        );
    }

    /// One shared-scope row per destination: the walk's paths name no real
    /// budget row, and a table of its own keeps the walk out of the
    /// documented table's per-method keys.
    static WALK_BUDGETS: &[Budget] = &[
        Budget {
            destination: Destination::TinkoffProd,
            scope: MethodScope::Shared,
            documented: None,
            used: 1000,
            window: MINUTE,
        },
        Budget {
            destination: Destination::TinkoffSandbox,
            scope: MethodScope::Shared,
            documented: None,
            used: 1000,
            window: MINUTE,
        },
        Budget {
            destination: Destination::FinamApi,
            scope: MethodScope::Shared,
            documented: None,
            used: 1000,
            window: MINUTE,
        },
        Budget {
            destination: Destination::MoexIss,
            scope: MethodScope::Shared,
            documented: None,
            used: 1000,
            window: MINUTE,
        },
        Budget {
            destination: Destination::CbrScripts,
            scope: MethodScope::Shared,
            documented: None,
            used: 1000,
            window: MINUTE,
        },
        Budget {
            destination: Destination::CbrDailyInfo,
            scope: MethodScope::Shared,
            documented: None,
            used: 1000,
            window: MINUTE,
        },
        Budget {
            destination: Destination::TinvestContract,
            scope: MethodScope::Shared,
            documented: None,
            used: 1000,
            window: MINUTE,
        },
    ];

    fn walk_gateway(time: &Arc<FakeTime>, transport: Scripted) -> Gateway<Scripted> {
        let directory = broker_egress_directory();
        Gateway::with_parts_in_directory(
            transport,
            WALK_BUDGETS,
            Arc::clone(time) as Arc<dyn Clock>,
            Arc::clone(time) as Arc<dyn Sleeper>,
            BrokerEgress::On,
            &directory,
        )
        .expect("the walk budget table is valid")
    }

    /// A GET the walk sends to every destination. Brokers carry the sync
    /// allowance their lane demands; no other destination asks for one.
    fn walk_request(destination: Destination) -> HttpRequest {
        let request = HttpRequest::get(destination, "/log-walk");
        if is_broker_destination(destination) {
            request.with_request_allowance(RequestAllowance::new(u32::MAX))
        } else {
            request
        }
    }

    #[tokio::test(start_paused = true)]
    async fn every_destination_logs_its_success_and_its_refusal() {
        for destination in Destination::ALL {
            // Success: one line at info, naming where the call went.
            let log = Log::capture();
            let time = FakeTime::new();
            let gateway = walk_gateway(
                &time,
                Scripted::answering(&time, 200).then(Ok(HttpResponse {
                    request_id: Some("req-walk-success".to_owned()),
                    ..status(200)
                })),
            );
            let request = walk_request(destination);
            gateway
                .send(&request, None)
                .await
                .unwrap_or_else(|error| panic!("{destination:?} success: {error}"));
            let text = log.text();
            let answered: Vec<&str> = text
                .lines()
                .filter(|line| line.contains("outbound call answered"))
                .collect();
            assert_eq!(answered.len(), 1, "{destination:?}: {text}");
            let line = answered[0];
            for fragment in [
                "INFO",
                &format!("destination={destination:?}"),
                "method=\"/log-walk\"",
                &format!(
                    "url={}/log-walk",
                    destination.base_url().trim_end_matches('/')
                ),
                "status=200",
                "attempt=1",
                "elapsed_ms=",
                "request_id=\"req-walk-success\"",
            ] {
                assert!(
                    line.contains(fragment),
                    "{destination:?}: no {fragment:?} in {line}"
                );
            }

            // Refusal: one line at warn, the same fields with the status.
            let log = Log::capture();
            let time = FakeTime::new();
            let gateway = walk_gateway(&time, Scripted::answering(&time, 404));
            let refused = gateway.send(&request, None).await;
            assert!(
                matches!(refused, Err(GatewayError::Rejected { status: 404, .. })),
                "{destination:?}: {refused:?}"
            );
            let text = log.text();
            let rejected: Vec<&str> = text
                .lines()
                .filter(|line| line.contains("outbound call refused"))
                .collect();
            assert_eq!(rejected.len(), 1, "{destination:?}: {text}");
            let line = rejected[0];
            for fragment in [
                "WARN",
                &format!("destination={destination:?}"),
                &format!(
                    "url={}/log-walk",
                    destination.base_url().trim_end_matches('/')
                ),
                "status=404",
                "attempt=1",
                "refusal=\"rejected\"",
            ] {
                assert!(
                    line.contains(fragment),
                    "{destination:?}: no {fragment:?} in {line}"
                );
            }
        }
    }

    // --- production parts -------------------------------------------------

    // The only test in this binary that calls `production`: `cargo test`
    // runs every test of a binary in one process, and the second call in a
    // process is refused.
    #[test]
    fn a_process_builds_one_production_gateway() {
        let (_directory, database) = instance_database("production");
        let first = Gateway::production(BrokerEgress::Off, &database);
        let second = Gateway::production(BrokerEgress::Off, &database);

        assert!(first.is_ok(), "the first gateway was refused");
        let Err(refused) = second else {
            panic!("a second production gateway was built");
        };
        assert!(
            matches!(refused, GatewayError::SecondGateway),
            "{refused:?}"
        );
        assert!(!refused.is_transient());
        let message = refused.to_string();
        assert!(message.contains("one gateway"), "{message}");
        assert!(message.contains("docs/deployment.md §1.1"), "{message}");
    }

    #[test]
    fn a_gateway_without_a_database_refuses_broker_egress() {
        let _kept = instance_database("without-database");

        let refused = match Gateway::new(HttpClient::new(), BrokerEgress::On) {
            Err(refused) => refused,
            Ok(_) => panic!("a constructor without a database cannot open a tally"),
        };
        assert!(
            matches!(refused, GatewayError::BrokerEgressWithoutDatabase),
            "{refused:?}"
        );
        let message = refused.to_string();
        assert!(message.contains("production"), "{message}");
        let refused = match Gateway::with_parts(
            HttpClient::new(),
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            BrokerEgress::On,
        ) {
            Err(refused) => refused,
            Ok(_) => panic!("with_parts takes no database either"),
        };
        assert!(
            matches!(refused, GatewayError::BrokerEgressWithoutDatabase),
            "{refused:?}"
        );
    }

    #[test]
    fn a_database_anchored_gateway_creates_its_place_beside_the_database() {
        let (directory, database) = instance_database("for-database");
        let time = FakeTime::new();

        let gateway = Gateway::with_parts_for_database(
            Scripted::answering(&time, 200),
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            BrokerEgress::On,
            &database,
        )
        .expect("the place beside the database is created");

        let place = directory.join("iaam.sqlite.egress");
        let mode = std::os::unix::fs::MetadataExt::mode(
            &std::fs::metadata(&place).expect("the place exists"),
        );
        assert_eq!(mode & 0o777, 0o700, "the place is private to its owner");
        let tally = std::fs::read(place.join(crate::tally::TALLY_FILE))
            .expect("the fresh tally record exists");
        assert!(tally.is_empty(), "a fresh record is empty, not pre-filled");
        std::mem::forget(gateway);
    }

    #[test]
    fn the_production_gateway_can_be_shared_between_tasks() {
        fn shared<T: Send + Sync>(_: &T) {}
        fn spawnable<F: Future + Send>(_: F) {}
        // The production type, built as `production` builds it, without
        // spending this process's one call.
        let gateway = Gateway::new(HttpClient::new(), BrokerEgress::Off)
            .expect("the documented table is valid");
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        shared(&gateway);
        // Built, never polled: nothing is sent.
        spawnable(gateway.send(&request, None));
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_holds_the_gateway_as_one_object_safe_outbound() {
        let time = FakeTime::new();
        let gateway = Arc::new(gateway(&time, Scripted::answering(&time, 200)));
        let held: Arc<dyn Outbound> = Arc::clone(&gateway) as Arc<dyn Outbound>;

        let response = held.send(&operations(), None).await.expect("sent");

        assert_eq!(response.status, 200);
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_outbound_trait_keeps_every_rule_of_the_gateway() {
        let time = FakeTime::new();
        let held: Arc<dyn Outbound> = Arc::new(gateway(&time, Scripted::answering(&time, 200)));

        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/unmapped",
            crate::RequestBody::Json("{}".to_owned()),
        )
        .with_request_allowance(RequestAllowance::new(u32::MAX));
        let refused = held.send(&request, None).await;

        assert!(matches!(refused, Err(GatewayError::UnknownBudget { .. })));
    }

    #[test]
    fn the_system_clock_reads_the_current_instant() {
        let before = Instant::now();
        let read = SystemClock.now();
        assert!(before <= read && read <= Instant::now());
    }

    #[tokio::test(start_paused = true)]
    async fn the_tokio_sleeper_waits_the_whole_delay() {
        let before = tokio::time::Instant::now();

        TokioSleeper.sleep(Duration::from_secs(7)).await;

        assert_eq!(before.elapsed(), Duration::from_secs(7));
    }

    #[test]
    fn the_documented_table_is_valid() {
        assert!(BudgetTable::new(BUDGETS).is_ok());
    }

    #[test]
    fn a_budget_of_zero_requests_is_refused_at_construction() {
        let refused = table_with(Budget { used: 0, ..ROW });
        assert!(matches!(refused, Err(GatewayError::InvalidBudgets(_))));
    }

    #[test]
    fn a_budget_over_a_zero_window_is_refused_at_construction() {
        let refused = table_with(Budget {
            window: Duration::ZERO,
            ..ROW
        });
        assert!(matches!(refused, Err(GatewayError::InvalidBudgets(_))));
    }

    #[test]
    fn a_budget_above_its_documented_limit_is_refused_at_construction() {
        let refused = table_with(Budget { used: 11, ..ROW });
        assert!(matches!(refused, Err(GatewayError::InvalidBudgets(_))));
    }

    #[test]
    fn a_budget_at_its_documented_limit_is_accepted() {
        assert!(table_with(Budget { used: 10, ..ROW }).is_ok());
    }
    #[test]
    fn a_broker_budget_longer_than_the_tally_retention_is_refused() {
        let refused = table_with(Budget {
            window: Duration::from_secs(24 * 60 * 60 + 1),
            ..ROW
        });
        assert!(matches!(refused, Err(GatewayError::InvalidBudgets(_))));
    }

    #[test]
    fn a_key_listed_twice_is_refused_at_construction() {
        let refused = BudgetTable::new(Box::leak(Box::new([ROW, ROW])));
        assert!(matches!(refused, Err(GatewayError::InvalidBudgets(_))));
    }

    #[test]
    fn every_budget_in_the_table_is_at_most_half_its_documented_limit() {
        for row in BUDGETS {
            if let Some(documented) = row.documented {
                assert!(
                    row.used * 2 <= documented,
                    "{:?} {:?} uses more than half",
                    row.destination,
                    row.scope
                );
            }
        }
    }

    #[test]
    fn every_refusal_names_its_kind_for_the_warn_log() {
        let cases: Vec<(GatewayError, &str)> = vec![
            (
                GatewayError::Exhausted {
                    destination: Destination::FinamApi,
                    status: None,
                    attempts: 1,
                    retry_after: Duration::ZERO,
                },
                "exhausted",
            ),
            (
                GatewayError::DeadlineReached {
                    destination: Destination::FinamApi,
                    status: None,
                    attempts: 1,
                    retry_after: Duration::ZERO,
                },
                "deadline",
            ),
            (
                GatewayError::CircuitOpen {
                    destination: Destination::FinamApi,
                    attempts: 1,
                    retry_after: Duration::ZERO,
                },
                "circuit open",
            ),
            (
                GatewayError::Deferred {
                    destination: Destination::FinamApi,
                    status: None,
                    attempts: 1,
                    retry_after: Duration::ZERO,
                },
                "deferred",
            ),
            (
                GatewayError::DailyCeiling {
                    destination: Destination::FinamApi,
                    ceiling: 1,
                    resets_at: "tomorrow".to_owned(),
                    retry_after: Duration::ZERO,
                },
                "daily ceiling",
            ),
            (
                GatewayError::BrokerHostPaused {
                    destination: Destination::FinamApi,
                    host: "api.finam.ru",
                    reopens_at: "soon".to_owned(),
                    retry_after: Duration::ZERO,
                    attempts: 1,
                },
                "broker host paused",
            ),
            (
                GatewayError::BrokerHostClosed {
                    destination: Destination::FinamApi,
                    host: "api.finam.ru",
                    reason: "repeated refusals",
                    reopens_at: "soon".to_owned(),
                    retry_after: Duration::ZERO,
                    status: None,
                    attempts: 1,
                },
                "broker host closed",
            ),
            (
                GatewayError::MissingRequestAllowance {
                    destination: Destination::FinamApi,
                },
                "missing request allowance",
            ),
            (
                GatewayError::RequestCeiling {
                    destination: Destination::FinamApi,
                    ceiling: 1,
                },
                "request ceiling",
            ),
            (GatewayError::BrokerEgressOff, "broker egress off"),
            (
                GatewayError::EgressPlace {
                    database: PathBuf::from("iaam.db"),
                    reason: "unresolved".to_owned(),
                },
                "egress place",
            ),
            (
                GatewayError::BrokerEgressWithoutDatabase,
                "broker egress without database",
            ),
            (
                GatewayError::TallyUnavailable {
                    path: PathBuf::from("outbound-tally"),
                    reason: "unavailable".to_owned(),
                },
                "tally unavailable",
            ),
            (
                GatewayError::TallyCorrupt {
                    path: PathBuf::from("outbound-tally"),
                    reason: "corrupt".to_owned(),
                },
                "tally corrupt",
            ),
            (
                GatewayError::BrokerEndpointOwned {
                    destination: Destination::FinamApi,
                    endpoint: "api.finam.ru",
                },
                "broker endpoint owned",
            ),
            (
                GatewayError::UnknownBudget {
                    destination: Destination::FinamApi,
                    path: "/v1/sessions".to_owned(),
                },
                "unknown budget",
            ),
            (
                GatewayError::InvalidBudgets("invented".to_owned()),
                "invalid budgets",
            ),
            (GatewayError::SecondGateway, "second gateway"),
            (
                GatewayError::Rejected {
                    destination: Destination::FinamApi,
                    status: 500,
                    attempts: 1,
                    body: RejectedBody::without_secret(b"invented", None),
                    location: None,
                    content_type: None,
                    request_id: None,
                },
                "rejected",
            ),
            (
                GatewayError::Transport {
                    destination: Destination::FinamApi,
                    error: HttpError::Network,
                    attempts: 1,
                },
                "transport",
            ),
        ];

        for (refusal, kind) in &cases {
            assert_eq!(refusal.kind(), *kind, "{refusal:?}");
        }
        // The log vocabulary is distinct: two refusals sharing a kind would
        // make the warn log unreadable as a table.
        let mut kinds = cases.iter().map(|(_, kind)| *kind).collect::<Vec<_>>();
        kinds.sort_unstable();
        let distinct = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), distinct, "every refusal names its own kind");
    }

    #[test]
    fn the_database_anchored_constructors_name_a_database_that_does_not_resolve() {
        let (directory, _database) = instance_database("ctor-unresolved");
        let absent = directory.join("absent.sqlite");

        let error =
            initialize_fresh_tally(&absent, &SystemClock).expect_err("the place does not derive");
        match &error {
            GatewayError::EgressPlace { database, .. } => {
                assert_eq!(database, &absent, "the refusal names the database");
            }
            other => panic!("an unresolvable database is an EgressPlace: {other:?}"),
        }

        let refused = match Gateway::with_parts_for_database(
            HttpClient::new(),
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            BrokerEgress::On,
            &absent,
        ) {
            Err(refused) => refused,
            Ok(_) => panic!("the production constructor refuses the same database"),
        };
        assert!(
            matches!(refused, GatewayError::EgressPlace { .. }),
            "{refused:?}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn an_obstructed_tally_record_is_named_by_the_constructors() {
        use std::os::unix::fs::DirBuilderExt;

        let (directory, database) = instance_database("ctor-obstructed");
        let place = egress_directory_for(&database).expect("the place derives");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&place)
            .expect("the place is made");
        // The record's name is taken by a directory: opening it as a file
        // fails before anything is read or written.
        std::fs::create_dir(place.join(crate::tally::TALLY_FILE)).expect("the obstruction is made");

        let error = initialize_fresh_tally(&database, &SystemClock)
            .expect_err("an obstructed record is unavailable");
        match &error {
            GatewayError::TallyUnavailable { path, .. } => {
                assert_eq!(path, &place.join(crate::tally::TALLY_FILE));
            }
            other => panic!("an obstructed record is TallyUnavailable: {other:?}"),
        }

        let refused = match Gateway::with_parts_for_database(
            HttpClient::new(),
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            BrokerEgress::On,
            &database,
        ) {
            Err(refused) => refused,
            Ok(_) => panic!("the production constructor refuses the same obstruction"),
        };
        assert!(
            matches!(refused, GatewayError::TallyUnavailable { .. }),
            "{refused:?}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_generation_record_that_cannot_be_opened_is_refused_before_any_call() {
        use std::os::unix::fs::DirBuilderExt;

        let (directory, database) = instance_database("ctor-generation");
        let place = egress_directory_for(&database).expect("the place derives");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&place)
            .expect("the place is made");
        // The generation record's name is taken by a directory: the pair
        // opens, the generation cannot, and the constructor stops before
        // any broker call.
        std::fs::create_dir(place.join(crate::tally::GENERATION_FILE))
            .expect("the obstruction is made");

        let refused = match Gateway::with_parts_for_database(
            HttpClient::new(),
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
            BrokerEgress::On,
            &database,
        ) {
            Err(refused) => refused,
            Ok(_) => panic!("a generation record that cannot be opened is refused"),
        };
        match &refused {
            GatewayError::TallyUnavailable { path, reason } => {
                assert_eq!(path, &place.join(crate::tally::TALLY_FILE));
                assert!(
                    reason.contains("open the outbound tally generation"),
                    "{reason}"
                );
            }
            other => panic!("an unopenable generation record is TallyUnavailable: {other:?}"),
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }

    // --- response cache ---------------------------------------------------

    /// An answer a scripted endpoint gave, with a body to tell answers
    /// apart.
    fn answered(status: u16, body: &[u8]) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_vec(),
            ..Default::default()
        }
    }

    /// The gateway a production process builds — documented budgets, the
    /// tally beside the invented instance's database and the response
    /// cache beside it — over the parts the test injects.
    fn gateway_with_cache<T: Transport + 'static>(
        time: &Arc<FakeTime>,
        transport: T,
        label: &str,
    ) -> Gateway<T> {
        let (_directory, database) = instance_database(label);
        Gateway::with_parts_for_database_with_response_cache(
            transport,
            BUDGETS,
            Arc::clone(time) as Arc<dyn Clock>,
            Arc::clone(time) as Arc<dyn Sleeper>,
            BrokerEgress::Off,
            &database,
        )
        .expect("the documented table is valid")
    }

    /// The same over a broker destination, whose tally must be minted
    /// fresh first: an invented instance's first enabling.
    fn broker_gateway_with_cache<T: Transport + 'static>(
        time: &Arc<FakeTime>,
        transport: T,
        label: &str,
    ) -> Gateway<T> {
        let (_directory, database) = instance_database(label);
        initialize_fresh_tally(&database, time.as_ref()).expect("the fresh tally was minted");
        Gateway::with_parts_for_database_with_response_cache(
            transport,
            BUDGETS,
            Arc::clone(time) as Arc<dyn Clock>,
            Arc::clone(time) as Arc<dyn Sleeper>,
            BrokerEgress::On,
            &database,
        )
        .expect("the documented table is valid")
    }

    /// A Finam read: a GET the account's access token authorizes.
    fn finam_read(bearer: &str) -> HttpRequest {
        HttpRequest::get(Destination::FinamApi, "/v1/accounts/ACC123")
            .with_bare_token(bearer)
            .with_request_allowance(RequestAllowance::new(3))
    }

    /// Finam's session exchange: the secret rides the body alone, and the
    /// method is marked safe to repeat because minting a session has no
    /// effect on the account.
    fn finam_exchange(secret: &str) -> HttpRequest {
        HttpRequest::post(
            Destination::FinamApi,
            "/v1/sessions",
            crate::RequestBody::Json(format!(r#"{{"secret":"{secret}"}}"#)),
        )
        .with_request_allowance(RequestAllowance::new(3))
        .idempotent()
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_answered_once_is_answered_from_the_cache_for_an_hour() {
        let time = FakeTime::new();
        let transport =
            Scripted::answering(&time, 200).then(Ok(answered(200, b"<history>pages</history>")));
        let gateway = gateway_with_cache(&time, transport, "read-hour");
        let request = HttpRequest::get(
            Destination::MoexIss,
            "/iss/history/engines/stock/markets/bonds/boards/TQCB/securities/SU26238RMFS4.json",
        )
        .with_query("from", "2026-08-01");

        let first = gateway.send(&request, None).await.expect("first sent");
        let second = gateway.send(&request, None).await.expect("second answered");

        assert_eq!(
            gateway.transport.sent_count(),
            1,
            "the second read was answered from the cache"
        );
        assert_eq!(
            second.status, first.status,
            "the cached answer is the answer given"
        );
        assert_eq!(
            second.body, first.body,
            "the cached answer is the answer given"
        );
        // Both answers carry a moment, and the served one keeps the moment
        // it was first fetched — between the first answer's and the serve's.
        let observed = first.observed_at.expect("a live answer is dated");
        let kept = second.observed_at.expect("a served answer keeps its date");
        assert!(kept >= observed, "the kept moment is not before the fetch");
        assert!(
            kept <= time.now_unix(),
            "the kept moment is the fetch's, not the serve's"
        );

        time.advance(crate::cache::CACHE_TTL + Duration::from_secs(1));
        gateway
            .send(&request, None)
            .await
            .expect("sent after the hour");
        assert_eq!(
            gateway.transport.sent_count(),
            2,
            "an hour later the read is sent again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_read_is_sent_every_time() {
        let time = FakeTime::new();
        let gateway = broker_gateway_with_cache(&time, Scripted::answering(&time, 200), "non-read");
        let request = HttpRequest::post(
            Destination::FinamApi,
            "/v1/accounts/ACC123/transactions",
            crate::RequestBody::Json("{}".to_owned()),
        )
        .with_bare_token("test-bearer")
        .with_request_allowance(RequestAllowance::new(3));

        gateway.send(&request, None).await.expect("first sent");
        gateway.send(&request, None).await.expect("second sent");

        assert_eq!(
            gateway.transport.sent_count(),
            2,
            "a request that may act is sent again whatever the answer was"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_credential_exchange_is_never_cached() {
        let time = FakeTime::new();
        let transport = Scripted::answering(&time, 200)
            .then(Ok(answered(200, br#"{"token":"session-one"}"#)))
            .then(Ok(answered(200, br#"{"token":"session-two"}"#)));
        let gateway = broker_gateway_with_cache(&time, transport, "exchange");
        let request = finam_exchange("test-finam-secret");

        gateway
            .send(&request, None)
            .await
            .expect("first exchange sent");
        let second = gateway
            .send(&request, None)
            .await
            .expect("second exchange sent");

        assert_eq!(
            gateway.transport.sent_count(),
            2,
            "an exchange of a secret is sent every time"
        );
        assert_eq!(
            second.body, br#"{"token":"session-two"}"#,
            "the second exchange got its own answer"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn one_access_credential_never_serves_another() {
        let time = FakeTime::new();
        let transport = Scripted::answering(&time, 200)
            .then(Ok(answered(200, b"first-access")))
            .then(Ok(answered(200, b"second-access")));
        let gateway = broker_gateway_with_cache(&time, transport, "isolation");

        gateway
            .send(&finam_read("test-bearer-one"), None)
            .await
            .expect("the first access was sent");
        let second = gateway
            .send(&finam_read("test-bearer-two"), None)
            .await
            .expect("the second access was sent");

        assert_eq!(
            gateway.transport.sent_count(),
            2,
            "a new credential is a new send"
        );
        assert_eq!(
            second.body, b"second-access",
            "the first access's answer did not serve the second"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cached_answer_consumes_no_ceiling_allowance() {
        let time = FakeTime::new();
        let transport = Scripted::answering(&time, 200).then(Ok(answered(200, b"holdings")));
        let gateway = broker_gateway_with_cache(&time, transport, "allowance");
        let request = HttpRequest::get(Destination::FinamApi, "/v1/accounts/ACC123")
            .with_bare_token("test-bearer")
            .with_request_allowance(RequestAllowance::new(1));

        let first = gateway
            .send(&request, None)
            .await
            .expect("the first read went out");
        let second = gateway
            .send(&request, None)
            .await
            .expect("the second read was answered without spending the last attempt");

        assert_eq!(first.body, second.body);
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn two_concurrent_identical_reads_reach_the_wire_once() {
        let time = FakeTime::new();
        let transport = Scripted::answering(&time, 200).then(Ok(answered(200, b"<history/>")));
        let gateway = broker_gateway_with_cache(&time, transport, "concurrent");
        let request = HttpRequest::get(Destination::FinamApi, "/v1/accounts/ACC123")
            .with_bare_token("test-bearer");
        // One shared allowance of one: whatever the second caller does, it
        // must be served from the store, not from a second send.
        let allowance = RequestAllowance::new(1);
        let first_call = request.clone().with_request_allowance(allowance.clone());
        let second_call = request.with_request_allowance(allowance.clone());

        let (first, second) = tokio::join!(
            gateway.send(&first_call, None),
            gateway.send(&second_call, None),
        );

        let first = first.expect("the first read was sent");
        let second = second.expect("the second read was answered");
        assert_eq!(
            gateway.transport.sent_count(),
            1,
            "two concurrent identical reads both reached the wire"
        );
        assert_eq!(
            first.body, b"<history/>",
            "the wire answered the first call"
        );
        assert_eq!(
            second.body, first.body,
            "both callers received the same answer"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_destination_echoing_the_token_is_not_stored_in_the_cache() {
        let time = FakeTime::new();
        let token = "test-bearer-token-Q1w2e3r4";
        let identity = "test-cache-identity-Z9y8x7w6";
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json")
            .with_bearer(token)
            .with_cache_identity(identity);
        let transport = Scripted::answering(&time, 200).then(Ok(HttpResponse {
            status: 200,
            body: format!(r#"{{ "history": ["{token}"] }}"#).into_bytes(),
            request_id: Some(token.to_owned()),
            ..Default::default()
        }));
        let (_directory, database) = instance_database("cache-echo");
        let gateway = Gateway::with_parts_for_database_with_response_cache(
            transport,
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            BrokerEgress::Off,
            &database,
        )
        .expect("the documented table is valid");

        gateway
            .send(&request, None)
            .await
            .expect("the read was sent and answer stored");

        let place = crate::cache::cache_directory_for(&database).expect("the cache place");
        let mut files = 0;
        for entry in std::fs::read_dir(&place).expect("cache read").flatten() {
            files += 1;
            let bytes = std::fs::read(entry.path()).expect("entry read");
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(token),
                "the echoed token leaked into {:?}",
                entry.path()
            );
            assert!(
                !text.contains(identity),
                "the cache identity leaked into {:?}",
                entry.path()
            );
        }
        assert!(files > 0, "the cache stored something to inspect");
    }

    #[tokio::test(start_paused = true)]
    async fn a_cached_answer_is_logged_like_an_answered_call() {
        let log = Log::capture();
        let time = FakeTime::new();
        let transport = Scripted::answering(&time, 200).then(Ok(HttpResponse {
            status: 200,
            body: b"<history/>".to_vec(),
            request_id: Some("moex-req-7".to_owned()),
            ..Default::default()
        }));
        let gateway = gateway_with_cache(&time, transport, "log");
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        gateway.send(&request, None).await.expect("first sent");
        gateway.send(&request, None).await.expect("second answered");

        log.assert_line(
            "INFO",
            "outbound call answered from cache",
            &[
                "destination=MoexIss",
                "method=\"/iss/history.json\"",
                "url=https://iss.moex.com/iss/history.json",
                "attempt=0",
                "status=200",
                "request_id=\"moex-req-7\"",
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_cache_survives_a_restart() {
        let time = FakeTime::new();
        let transport = Scripted::answering(&time, 200).then(Ok(answered(200, b"<history/>")));
        let (_directory, database) = instance_database("cache-restart");
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        let first = Gateway::with_parts_for_database_with_response_cache(
            transport,
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            BrokerEgress::Off,
            &database,
        )
        .expect("the documented table is valid");
        first
            .send(&request, None)
            .await
            .expect("sent by the first process");
        drop(first);

        // A restarted process: a fresh gateway over the same database, with
        // a transport that has no answer scripted — whatever it returns,
        // it must not have been sent.
        let second = Gateway::with_parts_for_database_with_response_cache(
            Scripted::answering(&time, 200),
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            BrokerEgress::Off,
            &database,
        )
        .expect("the documented table is valid");
        second
            .send(&request, None)
            .await
            .expect("answered after the restart");

        assert_eq!(
            second.transport.sent_count(),
            0,
            "the restarted gateway sent nothing for the read it had cached"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_gateway_without_a_database_does_not_cache() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        gateway.send(&request, None).await.expect("first sent");
        gateway.send(&request, None).await.expect("second sent");

        assert_eq!(
            gateway.transport.sent_count(),
            2,
            "without a database there is no cache"
        );
    }

    #[test]
    fn a_cache_beside_a_missing_database_is_refused_by_name() {
        let time = FakeTime::new();
        let missing =
            std::env::temp_dir().join(format!("iaam-missing-cache-db-{}", std::process::id()));
        let refused = Gateway::with_parts_for_database_with_response_cache(
            Scripted::answering(&time, 200),
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            BrokerEgress::Off,
            &missing,
        );

        let Err(error) = refused else {
            panic!("a gateway over a missing database was built");
        };
        assert!(
            matches!(error, GatewayError::CacheUnavailable { .. }),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("response cache"), "{message}");
        assert!(message.contains("does not exist"), "{message}");
    }
}
