//! Gateway: the one path every outbound call takes.
//!
//! A source crate describes a request; the gateway decides whether, when and
//! how often it is sent. Everything that protects a destination from this
//! process — and this process from a destination — lives here, in one place,
//! so a caller cannot forget a rule by calling the transport directly:
//!
//! - a **budget** per destination and method, taken from the documented limit;
//! - **one request in flight** per host, so two destinations on one host
//!   (the two CBR services) share one lane and one budget;
//! - **retries** of transient refusals, with the policy of `resilience`;
//! - a **circuit breaker** per host;
//! - an optional **deadline** no attempt or wait may cross, an attempt in
//!   flight included;
//! - a **reset the destination named**, obeyed by every caller of its host;
//! - a **structured event** for every long wait, retry, transient refusal and
//!   breaker change, carrying no header, body or secret.
//!
//! Time comes from an injected clock and sleeper, so every rule is checked on
//! a fake clock: a test that sleeps for a minute to prove a per-minute budget
//! is a test nobody runs.

use std::collections::{HashMap, VecDeque};
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Mutex;

use crate::client::HttpClient;
use crate::destination::Destination;
use crate::request::{HttpRequest, Secret};
use crate::resilience::{MAX_NAMED_WAIT, Outcome, Retry, RetryPolicy, is_transient};
use crate::response::{HttpError, HttpResponse};

/// The window every per-minute limit below is stated over.
const MINUTE: Duration = Duration::from_secs(60);

/// Attempts per call, the first included.
pub const ATTEMPTS: u32 = 5;

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

/// Source of the current instant.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Waits out a delay.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
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

/// What the gateway sends through. `HttpClient` in production; a scripted
/// endpoint in tests, so no test touches the network.
pub trait Transport: Send + Sync {
    fn send<'a>(
        &'a self,
        request: &'a HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a;
}

impl Transport for HttpClient {
    fn send<'a>(
        &'a self,
        request: &'a HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        Self::send(self, request)
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
        method: &'static str,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>>;
}

impl<T: Transport> Outbound for Gateway<T> {
    fn send<'a>(
        &'a self,
        method: &'static str,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>> {
        Box::pin(Self::send(self, method, request, deadline))
    }
}

/// Refusal from the gateway.
///
/// Carries statuses, counts and delays only: never a header value and never
/// the presented secret, so it can be logged as it is.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("no budget is recorded for method {method} at {destination:?}")]
    UnknownBudget {
        destination: Destination,
        method: &'static str,
    },
    #[error("the budget table is invalid: {0}")]
    InvalidBudgets(String),
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
            Self::UnknownBudget { .. } => "unknown budget",
            Self::InvalidBudgets(_) => "invalid budgets",
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
            | Self::Deferred { retry_after, .. } => Some(*retry_after),
            Self::UnknownBudget { .. }
            | Self::InvalidBudgets(_)
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
            | Self::Deferred { status, .. } => *status,
            Self::Rejected { status, .. } => Some(*status),
            Self::UnknownBudget { .. }
            | Self::InvalidBudgets(_)
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
            | Self::Rejected { attempts, .. }
            | Self::Transport { attempts, .. } => *attempts,
            Self::UnknownBudget { .. } | Self::InvalidBudgets(_) => 0,
        }
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

/// What stands in a rejected body where the bearer secret was.
const REDACTED: &[u8] = b"<redacted>";

impl RejectedBody {
    fn without_secret(body: &[u8], secret: Option<&Secret>) -> Self {
        let secret = secret.map_or(&[][..], |secret| secret.expose().as_bytes());
        if secret.is_empty() {
            return Self(body.to_vec());
        }
        let mut kept = Vec::with_capacity(body.len());
        let mut rest = body;
        while let Some((&first, tail)) = rest.split_first() {
            if let Some(after) = rest.strip_prefix(secret) {
                kept.extend_from_slice(REDACTED);
                rest = after;
            } else {
                kept.push(first);
                rest = tail;
            }
        }
        Self(kept)
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

    /// The row covering a method key, and the key its budget is kept under.
    ///
    /// A named row wins over a shared one.
    fn lookup(
        &self,
        destination: Destination,
        method: &'static str,
    ) -> Result<(Budget, Option<&'static str>), GatewayError> {
        let find = |wanted: MethodScope| {
            self.rows
                .iter()
                .find(|row| row.destination == destination && row.scope == wanted)
        };
        if let Some(row) = find(MethodScope::Named(method)) {
            return Ok((*row, Some(method)));
        }
        if let Some(row) = find(MethodScope::Shared) {
            return Ok((*row, None));
        }
        Err(GatewayError::UnknownBudget {
            destination,
            method,
        })
    }
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

/// The outbound gateway.
///
/// Built once per process and shared: the budgets, the in-flight rule and the
/// breaker are state of this value, and a second gateway would be a second,
/// independent allowance against the same destination.
pub struct Gateway<T> {
    transport: T,
    budgets: BudgetTable,
    retry: RetryPolicy,
    clock: Arc<dyn Clock>,
    sleeper: Arc<dyn Sleeper>,
    /// Keyed by `Destination::base_url`, the host a request reaches.
    lanes: HashMap<&'static str, Mutex<Lane>>,
}

impl Gateway<HttpClient> {
    /// The production gateway over the real transport: the one way a process
    /// gets it, because `HttpClient` cannot be built outside this crate.
    /// Called once per process, in `serve`, and shared from there.
    ///
    /// # Errors
    /// `GatewayError::InvalidBudgets` when the table in this module is wrong.
    pub fn production() -> Result<Self, GatewayError> {
        Self::new(HttpClient::new())
    }
}

impl<T: Transport> Gateway<T> {
    /// A gateway with the documented budgets, system clock and tokio timer
    /// over `transport`.
    ///
    /// # Errors
    /// `GatewayError::InvalidBudgets` when the table in this module is wrong.
    pub fn new(transport: T) -> Result<Self, GatewayError> {
        Self::with_parts(
            transport,
            BUDGETS,
            Arc::new(SystemClock),
            Arc::new(TokioSleeper),
        )
    }

    /// A gateway over explicit parts.
    ///
    /// # Errors
    /// `GatewayError::InvalidBudgets` when `budgets` fails its checks.
    pub fn with_parts(
        transport: T,
        budgets: &'static [Budget],
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
    ) -> Result<Self, GatewayError> {
        Ok(Self {
            transport,
            budgets: BudgetTable::new(budgets)?,
            retry: RetryPolicy::new(ATTEMPTS, FIRST_BACKOFF),
            clock,
            sleeper,
            lanes: Destination::ALL
                .into_iter()
                .map(|destination| (destination.base_url(), Mutex::new(Lane::default())))
                .collect(),
        })
    }

    /// Send a request under every rule of the gateway.
    ///
    /// `method` names the budget the request draws on, as the budget table
    /// spells it (`"OperationsService"`). A 2xx response is returned as it
    /// came; interpreting its body stays with the source.
    ///
    /// # Errors
    /// See `GatewayError`.
    pub async fn send(
        &self,
        method: &'static str,
        request: &HttpRequest,
        deadline: Option<Instant>,
    ) -> Result<HttpResponse, GatewayError> {
        let result = self.send_unlogged(method, request, deadline).await;
        if let Err(refusal) = &result
            && let Some(wait) = refusal.retry_after()
        {
            tracing::warn!(
                destination = ?request.destination(),
                method,
                attempt = refusal.attempts(),
                wait_ms = millis(wait),
                status = refusal.status(),
                refusal = refusal.kind(),
                "outbound call refused"
            );
        }
        result
    }

    async fn send_unlogged(
        &self,
        method: &'static str,
        request: &HttpRequest,
        deadline: Option<Instant>,
    ) -> Result<HttpResponse, GatewayError> {
        let destination = request.destination();
        let (budget, key) = self.budgets.lookup(destination, method)?;
        let lane = &self.lanes[destination.base_url()];
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
            // The lane is held for the breaker check, the budget and named
            // waits, the request and the breaker update, and released for
            // the backoff: a destination that is refusing is not made to wait
            // for a call that is only waiting itself, and a failure that
            // opens the breaker is recorded before another call can slip in.
            // A named reset is waited here, under the lane, by every caller.
            let (decision, outcome, body) = {
                let asked = self.clock.now();
                let mut lane = lane.lock().await;
                waits(
                    attempts + 1,
                    self.clock.now().saturating_duration_since(asked),
                    "lane",
                );
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
                let budget_wait = lane.budget_wait(key, &budget, now);
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
                    self.sleeper.sleep(wait).await;
                }
                lane.record_start(key, &budget, self.clock.now());
                attempts += 1;
                let Some(answer) = self
                    .by_deadline(self.transport.send(request), deadline)
                    .await
                else {
                    // Abandoned in flight: dropping the request cancels it.
                    // The wait offered is the one a timed-out attempt earns.
                    let retry_after = self
                        .retry
                        .delay(attempts, &Outcome::Transport(HttpError::Timeout));
                    return Err(cut(attempts, status, retry_after));
                };
                let (outcome, body) = match answer {
                    Ok(response) if (200..300).contains(&response.status) => {
                        if lane.record_success() {
                            tracing::info!(
                                destination = ?destination,
                                method,
                                attempt = attempts,
                                status = response.status,
                                "breaker closed"
                            );
                        }
                        return Ok(response);
                    }
                    Ok(response) => {
                        status = Some(response.status);
                        let outcome = match response.retry_after {
                            Some(after) => Outcome::status_with_retry_after(response.status, after),
                            None => Outcome::status(response.status),
                        };
                        (outcome, response.body)
                    }
                    Err(error) => {
                        status = None;
                        (Outcome::Transport(error), Vec::new())
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
                    self.retry.decide(attempts, &outcome)
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
                (decision, outcome, body)
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
                    self.sleeper.sleep(delay).await;
                }
                Retry::GiveUp => {
                    let body = RejectedBody::without_secret(&body, request.bearer());
                    return Err(self.refusal(destination, attempts, outcome, body));
                }
            }
        }
    }

    /// `work`, unless the deadline falls first: then `None`, at the deadline.
    ///
    /// The deadline's sleep is started only once `work` has not finished on
    /// its first poll, so an answer that is already there costs no timer. An
    /// answer the clock stamps after the deadline is treated as not there by
    /// it: a transport may move a clock without waiting on the sleeper.
    async fn by_deadline<F: Future>(
        &self,
        work: F,
        deadline: Option<Instant>,
    ) -> Option<F::Output> {
        let Some(deadline) = deadline else {
            return Some(work.await);
        };
        let mut work = pin!(work);
        let mut limit = None;
        let answer = poll_fn(|context| {
            if let Poll::Ready(answer) = work.as_mut().poll(context) {
                return Poll::Ready(Some(answer));
            }
            let limit = limit.get_or_insert_with(|| {
                self.sleeper
                    .sleep(deadline.saturating_duration_since(self.clock.now()))
            });
            limit.as_mut().poll(context).map(|()| None)
        })
        .await;
        answer.filter(|_| self.clock.now() <= deadline)
    }

    /// The error for a call that ended on `outcome`.
    fn refusal(
        &self,
        destination: Destination,
        attempts: u32,
        outcome: Outcome,
        body: RejectedBody,
    ) -> GatewayError {
        if is_transient(&outcome) {
            return GatewayError::Exhausted {
                destination,
                status: match outcome {
                    Outcome::Status { status, .. } => Some(status),
                    Outcome::Transport(_) => None,
                },
                attempts,
                retry_after: self.retry.delay(attempts, &outcome),
            };
        }
        match outcome {
            Outcome::Status { status, .. } => GatewayError::Rejected {
                destination,
                status,
                attempts,
                body,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::resilience::MAX_NAMED_WAIT;

    /// A clock on tokio's paused timer: it moves only when every task waits
    /// on it, so a sleep of a minute takes no real time, and a request still
    /// in flight really is overtaken by a deadline that falls first.
    ///
    /// `advance` jumps the clock between calls, when nothing waits on it.
    struct FakeTime {
        offset: StdMutex<Duration>,
        slept: StdMutex<Vec<Duration>>,
    }

    impl FakeTime {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                offset: StdMutex::new(Duration::ZERO),
                slept: StdMutex::new(Vec::new()),
            })
        }

        fn advance(&self, by: Duration) {
            *self.offset.lock().expect("clock") += by;
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
    }

    impl Sleeper for FakeTime {
        fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async move {
                tokio::time::sleep(delay).await;
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
        }
    }

    fn gateway(time: &Arc<FakeTime>, transport: Scripted) -> Gateway<Scripted> {
        Gateway::with_parts(
            transport,
            BUDGETS,
            Arc::clone(time) as Arc<dyn Clock>,
            Arc::clone(time) as Arc<dyn Sleeper>,
        )
        .expect("the documented table is valid")
    }

    fn operations() -> HttpRequest {
        HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
            crate::RequestBody::Json("{}".to_owned()),
        )
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
    async fn operations_service_never_exceeds_fifty_in_any_minute() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let start = time.now();

        for _ in 0..160 {
            gateway
                .send("OperationsService", &operations(), None)
                .await
                .expect("sent");
        }

        let sent = gateway.transport.sent_at();
        assert_eq!(sent.len(), 160);
        assert_never_more_than(50, MINUTE, &sent);
        // The budget holds the 51st back for the minute, not for longer.
        assert_eq!(sent[49], start);
        assert_eq!(sent[50], start + MINUTE);
    }

    #[tokio::test(start_paused = true)]
    async fn users_service_never_exceeds_twenty_five_in_any_minute() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = users();

        for _ in 0..60 {
            gateway
                .send("UsersService", &request, None)
                .await
                .expect("sent");
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
            gateway
                .send("OperationsService", &operations(), None)
                .await
                .expect("sent");
        }
        gateway
            .send("UsersService", &operations(), None)
            .await
            .expect("sent");

        assert_eq!(gateway.transport.sent_at().last(), Some(&start));
    }

    #[tokio::test(start_paused = true)]
    async fn finam_gives_each_method_its_own_hundred_a_minute() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::get(Destination::FinamApi, "/v1/accounts");
        let start = time.now();

        for _ in 0..100 {
            gateway
                .send("AccountsService.GetAccount", &request, None)
                .await
                .expect("sent");
        }
        for _ in 0..100 {
            gateway
                .send("AccountsService.Transactions", &request, None)
                .await
                .expect("sent");
        }
        gateway
            .send("AccountsService.GetAccount", &request, None)
            .await
            .expect("sent");

        let sent = gateway.transport.sent_at();
        assert_eq!(sent[199], start, "a second method waited on the first");
        assert_eq!(
            sent[200],
            start + MINUTE,
            "the 101st call of a method did not wait"
        );
    }

    #[test]
    fn finam_budgets_every_method_it_is_called_with_by_name() {
        for method in [
            "AccountsService.GetAccount",
            "AccountsService.Transactions",
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

    #[tokio::test(start_paused = true)]
    async fn a_finam_method_missing_from_the_table_is_refused_without_sending() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::get(Destination::FinamApi, "/v1/accounts");

        let refused = gateway
            .send("AccountsService.Invented", &request, None)
            .await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::UnknownBudget {
                    destination: Destination::FinamApi,
                    method: "AccountsService.Invented"
                })
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

        for method in ["history", "securities", "history"] {
            gateway.send(method, &request, None).await.expect("sent");
        }

        let sent = gateway.transport.sent_at();
        assert_eq!(sent[1] - sent[0], Duration::from_millis(100));
        assert_eq!(sent[2] - sent[1], Duration::from_millis(100));
    }

    #[tokio::test(start_paused = true)]
    async fn a_method_missing_from_the_table_is_refused_without_sending() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));

        let refused = gateway
            .send("InstrumentsService", &operations(), None)
            .await
            .expect_err("no budget is recorded for it");

        assert!(matches!(
            refused,
            GatewayError::UnknownBudget {
                destination: Destination::TinkoffProd,
                method: "InstrumentsService"
            }
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
        )
        .expect("a one-row table is valid");
        let request = HttpRequest::get(Destination::TinvestContract, "/contract.proto");

        let refused = gateway.send("contract", &request, None).await;

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
        .idempotent()
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_concurrent_call_to_a_destination_waits_for_the_first() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let (operations, users) = (operations(), users());

        let (first, second) = tokio::join!(
            gateway.send("OperationsService", &operations, None),
            gateway.send("UsersService", &users, None),
        );

        first.expect("sent");
        second.expect("sent");
        assert_eq!(gateway.transport.sent_count(), 2);
        assert_eq!(gateway.transport.most_in_flight.load(Ordering::SeqCst), 1);
    }

    /// The control for the test above: the fake endpoint does see two
    /// requests at once when the rule allows it.
    #[tokio::test(start_paused = true)]
    async fn calls_to_different_destinations_are_in_flight_together() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let operations = operations();
        let finam = HttpRequest::get(Destination::FinamApi, "/v1/accounts");

        let (first, second) = tokio::join!(
            gateway.send("OperationsService", &operations, None),
            gateway.send("AccountsService.GetAccount", &finam, None),
        );

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
            gateway.send("rates", &scripts, None),
            gateway.send("key_rate", &daily_info, None),
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

        gateway.send("rates", &scripts, None).await.expect("sent");
        gateway
            .send("key_rate", &daily_info, None)
            .await
            .expect("sent");

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
    async fn each_transient_status_is_retried_after_the_first_backoff() {
        for transient in [429, 500, 502, 503, 504] {
            let time = FakeTime::new();
            let gateway = gateway(
                &time,
                Scripted::answering(&time, 200).then(Ok(status(transient))),
            );

            let response = gateway.send("OperationsService", &operations(), None).await;

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
    async fn a_network_failure_and_a_timeout_are_retried() {
        for failure in [HttpError::Network, HttpError::Timeout] {
            let time = FakeTime::new();
            let gateway = gateway(&time, Scripted::answering(&time, 200).then(Err(failure)));

            let response = gateway.send("OperationsService", &operations(), None).await;

            assert_eq!(response.expect("the retry succeeded").status, 200);
            assert_eq!(gateway.transport.sent_count(), 2);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn each_permanent_status_returns_at_once() {
        for permanent in [400, 401, 403, 404, 422] {
            let time = FakeTime::new();
            let gateway = gateway(&time, Scripted::answering(&time, permanent));

            let refused = gateway
                .send("OperationsService", &operations(), None)
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
            .send("OperationsService", &operations(), None)
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
            .send("OperationsService", &request, None)
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
            .send("OperationsService", &operations(), None)
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
    async fn a_transient_failure_is_tried_five_times_with_doubling_backoff() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 503));

        let refused = gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect_err("every attempt failed");

        assert_eq!(gateway.transport.sent_count(), 5);
        assert_eq!(
            time.slept(),
            [1, 2, 4, 8].map(Duration::from_secs),
            "exponential from one second"
        );
        assert!(refused.is_transient());
        assert_eq!(refused.status(), Some(503));
        assert_eq!(refused.attempts(), 5);
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(16)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_not_marked_idempotent_is_sent_once() {
        for failure in [Ok(status(503)), Err(HttpError::Timeout)] {
            let time = FakeTime::new();
            let gateway = gateway(&time, Scripted::answering(&time, 200).then(failure));
            let transfer = HttpRequest::post(
                Destination::FinamApi,
                "/v1/accounts/transfer",
                crate::RequestBody::Json("{}".to_owned()),
            );

            let refused = gateway
                .send("AccountsService.Transactions", &transfer, None)
                .await
                .expect_err("a second send could do the thing twice");

            assert_eq!(gateway.transport.sent_count(), 1);
            assert!(
                time.slept().is_empty(),
                "it waited for a retry it may not make"
            );
            assert!(
                matches!(refused, GatewayError::Exhausted { attempts: 1, .. }),
                "{refused:?}"
            );
            assert_eq!(refused.retry_after(), Some(FIRST_BACKOFF));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_network_failures_carry_no_status() {
        let time = FakeTime::new();
        let transport = (0..5).fold(Scripted::answering(&time, 200), |script, _| {
            script.then(Err(HttpError::Network))
        });
        let gateway = gateway(&time, transport);

        let refused = gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect_err("every attempt failed");

        assert!(refused.is_transient());
        assert_eq!(refused.status(), None);
        assert_eq!(refused.attempts(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn a_named_delay_wins_over_the_computed_backoff() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(429, Duration::from_secs(7))))
                .then(Ok(with_retry_after(503, Duration::from_millis(250)))),
        );

        gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect("the third attempt succeeded");

        assert_eq!(
            time.slept(),
            [Duration::from_secs(7), Duration::from_millis(250)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_named_delay_is_waited_in_full_up_to_fifteen_minutes() {
        for named in [Duration::from_secs(300), MAX_NAMED_WAIT] {
            let time = FakeTime::new();
            let gateway = gateway(
                &time,
                Scripted::answering(&time, 200).then(Ok(with_retry_after(429, named))),
            );

            gateway
                .send("OperationsService", &operations(), None)
                .await
                .expect("the retry succeeded");

            assert_eq!(time.slept(), [named]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_named_delay_over_fifteen_minutes_is_returned_not_waited() {
        let time = FakeTime::new();
        let long = MAX_NAMED_WAIT + Duration::from_secs(1);
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).then(Ok(with_retry_after(429, long))),
        );

        let refused = gateway
            .send("OperationsService", &operations(), None)
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
    async fn a_named_delay_holds_back_every_caller_of_the_lane() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(429, Duration::from_secs(30)))),
        );
        let (operations, users) = (operations(), users());
        let start = time.now();

        let (first, second) = tokio::join!(
            gateway.send("OperationsService", &operations, None),
            gateway.send("UsersService", &users, None),
        );

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
    async fn a_named_delay_holds_back_a_call_made_after_the_one_that_learned_it() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(503, Duration::from_secs(20)))),
        );
        let start = time.now();
        let once = HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
            crate::RequestBody::Json("{}".to_owned()),
        );

        let refused = gateway
            .send("OperationsService", &once, None)
            .await
            .expect_err("sent once and refused");
        assert_eq!(refused.retry_after(), Some(Duration::from_secs(20)));
        gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect("sent after the reset");

        assert_eq!(
            gateway.transport.sent_at()[1],
            start + Duration::from_secs(20)
        );
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
            .send("UsersService", &users(), None)
            .await
            .expect_err("the reset is fifty minutes away");

        assert_eq!(gateway.transport.sent_count(), 1);
        assert!(
            matches!(refused, GatewayError::Deferred { attempts: 0, .. }),
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
            .send("OperationsService", &operations(), Some(deadline))
            .await
            .expect_err("the reset falls after the deadline");

        assert!(time.slept().is_empty());
        assert!(
            matches!(refused, GatewayError::DeadlineReached { attempts: 1, .. }),
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
            .send("OperationsService", &operations(), None)
            .await
            .expect("2xx is a success");

        assert_eq!(response, answer);
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
            .send("OperationsService", &request, None)
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
        gateway.send("OperationsService", &operations(), None).await
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
    async fn a_permanent_refusal_does_not_count_towards_the_breaker() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 404));

        for _ in 0..6 {
            let refused = call(&gateway).await;
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
        let finam = HttpRequest::get(Destination::FinamApi, "/v1/accounts");

        gateway
            .send("AccountsService.GetAccount", &finam, None)
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
        let (first, second) = tokio::join!(
            gateway.send("OperationsService", &operations, None),
            gateway.send("UsersService", &users, None),
        );

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
            .send("OperationsService", &operations(), Some(time.now()))
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
            .send("OperationsService", &operations(), Some(deadline))
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

        let refused = gateway
            .send("OperationsService", &operations(), Some(deadline))
            .await;

        assert!(matches!(
            refused,
            Err(GatewayError::DeadlineReached { attempts: 2, .. })
        ));
        assert_eq!(gateway.transport.sent_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_budget_wait_that_would_end_past_the_deadline_is_not_waited() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        for _ in 0..50 {
            call(&gateway).await.expect("within the budget");
        }
        let deadline = time.now() + Duration::from_secs(30);

        let refused = gateway
            .send("OperationsService", &operations(), Some(deadline))
            .await
            .expect_err("the budget frees a request only in a minute");

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
        assert_eq!(refused.retry_after(), Some(MINUTE));
        assert_eq!(gateway.transport.sent_count(), 50);
        assert!(time.slept().is_empty());
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
            .send("OperationsService", &operations(), Some(deadline))
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
            .send(
                "OperationsService",
                &operations(),
                Some(start + Duration::from_secs(10)),
            )
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

        let refused = gateway
            .send("OperationsService", &operations(), Some(deadline))
            .await;

        assert!(
            matches!(
                refused,
                Err(GatewayError::DeadlineReached { attempts: 1, .. })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_deadline_with_room_leaves_the_retries_alone() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200).then(Ok(status(503))));
        let deadline = time.now() + Duration::from_millis(1001);

        gateway
            .send("OperationsService", &operations(), Some(deadline))
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
            let cut = gateway
                .send("OperationsService", &operations(), Some(deadline))
                .await;
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
                "method=\"OperationsService\"",
                "attempt=1",
                "wait_ms=1000",
                "status=503",
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_over_a_second_is_logged_at_info_with_its_reason() {
        let log = Log::capture();
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(status(503)))
                .then(Ok(status(503)))
                .then(Ok(with_retry_after(429, Duration::from_secs(30)))),
        );

        call(&gateway).await.expect("the fourth attempt succeeded");

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
        assert!(waits[1].contains("reason=\"named reset\""), "{waits:?}");
        assert!(waits[1].contains("wait_ms=30000"), "{waits:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_budget_wait_is_logged_with_its_reason() {
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
            &["reason=\"budget\"", "wait_ms=60000", "attempt=1"],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_for_the_lane_is_logged_with_its_reason() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200).taking(Duration::from_secs(5)),
        );
        let (operations, users) = (operations(), users());
        let log = Log::capture();

        let (first, second) = tokio::join!(
            gateway.send("OperationsService", &operations, None),
            gateway.send("UsersService", &users, None),
        );

        first.expect("sent");
        second.expect("sent");
        log.assert_line(
            "INFO",
            "outbound call waits",
            &["reason=\"lane\"", "wait_ms=5000", "method=\"UsersService\""],
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
                "attempt=5",
                "wait_ms=16000",
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
        let _ = gateway
            .send("OperationsService", &operations(), Some(late.now()))
            .await;
        log.assert_line("WARN", "refusal=\"deadline\"", &["outbound call refused"]);

        let long = MAX_NAMED_WAIT + Duration::from_secs(1);
        let gateway = self::gateway(
            &late,
            Scripted::answering(&late, 200).then(Ok(with_retry_after(429, long))),
        );
        let _ = call(&gateway).await;
        log.assert_line(
            "WARN",
            "refusal=\"deferred\"",
            &["outbound call refused", "status=429"],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_secret_and_no_body_reach_the_log() {
        let time = FakeTime::new();
        let leaking = || {
            Ok(HttpResponse {
                body: b"t.invented-token account Main balance".to_vec(),
                ..with_retry_after(503, Duration::from_secs(2))
            })
        };
        let script = (0..5 * ATTEMPTS).fold(Scripted::answering(&time, 200), |script, _| {
            script.then(leaking())
        });
        let gateway = gateway(
            &time,
            script.then(Ok(HttpResponse {
                body: b"t.invented-token".to_vec(),
                ..status(401)
            })),
        );
        let request = operations().with_bearer("t.invented-token");
        let log = Log::capture();

        for _ in 0..6 {
            let _ = gateway.send("OperationsService", &request, None).await;
        }
        time.advance(COOL_DOWN);
        let _ = gateway.send("OperationsService", &request, None).await;

        let text = log.text();
        assert!(
            text.contains("breaker opened"),
            "the scenario logged nothing: {text}"
        );
        assert!(!text.contains("invented-token"), "{text}");
        assert!(!text.contains("balance"), "{text}");
    }

    // --- production parts -------------------------------------------------

    #[test]
    fn the_production_gateway_can_be_shared_between_tasks() {
        fn shared<T: Send + Sync>(_: &T) {}
        fn spawnable<F: Future + Send>(_: F) {}
        let gateway = Gateway::production().expect("the documented table is valid");
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        shared(&gateway);
        // Built, never polled: nothing is sent.
        spawnable(gateway.send("history", &request, None));
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_holds_the_gateway_as_one_object_safe_outbound() {
        let time = FakeTime::new();
        let gateway = Arc::new(gateway(&time, Scripted::answering(&time, 200)));
        let held: Arc<dyn Outbound> = Arc::clone(&gateway) as Arc<dyn Outbound>;

        let response = held
            .send("OperationsService", &operations(), None)
            .await
            .expect("sent");

        assert_eq!(response.status, 200);
        assert_eq!(gateway.transport.sent_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_outbound_trait_keeps_every_rule_of_the_gateway() {
        let time = FakeTime::new();
        let held: Arc<dyn Outbound> = Arc::new(gateway(&time, Scripted::answering(&time, 200)));

        let refused = held.send("InstrumentsService", &operations(), None).await;

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
}
