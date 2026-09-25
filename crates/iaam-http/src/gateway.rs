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
//! - an optional **deadline** no attempt or wait may cross.
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
use crate::request::HttpRequest;
use crate::resilience::{Outcome, Retry, RetryPolicy, is_transient};
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

/// Which method keys a budget row covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodScope {
    /// Exactly this method key, with its own budget.
    Named(&'static str),
    /// Every method key the caller names, each with its own budget of this
    /// size. Finam states its limit per method, not per host.
    EachMethod,
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
    // Finam, any method: documented 200/min per method, use 100/min.
    Budget {
        destination: Destination::FinamApi,
        scope: MethodScope::EachMethod,
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

    /// When a transient refusal is worth trying again; `None` for a
    /// permanent one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Exhausted { retry_after, .. }
            | Self::DeadlineReached { retry_after, .. }
            | Self::CircuitOpen { retry_after, .. } => Some(*retry_after),
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
            Self::Exhausted { status, .. } | Self::DeadlineReached { status, .. } => *status,
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
/// logged, and a body may carry the owner's data.
#[derive(Clone, PartialEq, Eq)]
pub struct RejectedBody(Vec<u8>);

impl RejectedBody {
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
    /// A named row wins over a per-method one, which wins over a shared one.
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
        if let Some(row) =
            find(MethodScope::Named(method)).or_else(|| find(MethodScope::EachMethod))
        {
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

    fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= BREAKER_FAILURES {
            self.open_until = Some(now + BREAKER_COOL_DOWN);
        }
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.open_until = None;
    }

    fn record_start(&mut self, key: Option<&'static str>, budget: &Budget, at: Instant) {
        let sent = self.sent.entry(key).or_default();
        sent.push_back(at);
        while sent.len() > budget.used as usize {
            sent.pop_front();
        }
    }
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

impl<T: Transport> Gateway<T> {
    /// The production gateway: documented budgets, system clock, tokio timer.
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
        loop {
            // The lane is held for the breaker check, the budget wait, the
            // request and the breaker update, and released for the backoff:
            // a destination that is refusing is not made to wait for a call
            // that is only waiting itself, and a failure that opens the
            // breaker is recorded before another call can slip in.
            let (decision, outcome, body) = {
                let mut lane = lane.lock().await;
                if let Some(retry_after) = lane.refusing(self.clock.now()) {
                    return Err(GatewayError::CircuitOpen {
                        destination,
                        attempts,
                        retry_after,
                    });
                }
                let now = self.clock.now();
                let wait = lane.budget_wait(key, &budget, now);
                if crosses(now + wait) {
                    return Err(cut(attempts, status, wait));
                }
                if !wait.is_zero() {
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
                        lane.record_success();
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
                if decision == Retry::GiveUp && is_transient(&outcome) {
                    lane.record_failure(self.clock.now());
                }
                (decision, outcome, body)
            };
            match decision {
                // A call cut short by its deadline is left out of the breaker
                // count: it did not use up its retries, so it proved no more
                // than one failed attempt does.
                Retry::After(delay) if crosses(self.clock.now() + delay) => {
                    return Err(cut(attempts, status, delay));
                }
                Retry::After(delay) => self.sleeper.sleep(delay).await,
                Retry::GiveUp => return Err(self.refusal(destination, attempts, outcome, body)),
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
        body: Vec<u8>,
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
                body: RejectedBody(body),
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
                .send("Accounts", &request, None)
                .await
                .expect("sent");
        }
        for _ in 0..100 {
            gateway.send("Trades", &request, None).await.expect("sent");
        }
        gateway
            .send("Accounts", &request, None)
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
        scope: MethodScope::EachMethod,
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
            gateway.send("Accounts", &finam, None),
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
    async fn a_named_delay_is_still_capped() {
        let time = FakeTime::new();
        let gateway = gateway(
            &time,
            Scripted::answering(&time, 200)
                .then(Ok(with_retry_after(429, Duration::from_secs(3600)))),
        );

        gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect("the retry succeeded");

        assert_eq!(time.slept(), [crate::resilience::MAX_BACKOFF]);
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
            .send("Accounts", &finam, None)
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

    // --- production parts -------------------------------------------------

    #[test]
    fn the_production_gateway_can_be_shared_between_tasks() {
        fn shared<T: Send + Sync>(_: &T) {}
        fn spawnable<F: Future + Send>(_: F) {}
        let gateway = Gateway::new(HttpClient::new()).expect("the documented table is valid");
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");

        shared(&gateway);
        // Built, never polled: nothing is sent.
        spawnable(gateway.send("history", &request, None));
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
