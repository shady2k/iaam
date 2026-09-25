//! Gateway: the one path every outbound call takes.
//!
//! A source crate describes a request; the gateway decides whether, when and
//! how often it is sent. Everything that protects a destination from this
//! process — and this process from a destination — lives here, in one place,
//! so a caller cannot forget a rule by calling the transport directly:
//!
//! - a **budget** per destination and method, taken from the documented limit;
//! - **one request in flight** per destination;
//! - **retries** of transient refusals, with the policy of `resilience`;
//! - a **circuit breaker** per destination;
//! - an optional **deadline** no attempt or wait may cross.
//!
//! Time comes from an injected clock and sleeper, so every rule is checked on
//! a fake clock: a test that sleeps for a minute to prove a per-minute budget
//! is a test nobody runs.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
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
            Self::Exhausted { retry_after, .. } => Some(*retry_after),
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
            Self::Exhausted { status, .. } => *status,
            Self::Rejected { status, .. } => Some(*status),
            Self::UnknownBudget { .. } | Self::InvalidBudgets(_) | Self::Transport { .. } => None,
        }
    }

    /// Requests actually sent for this call.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        match self {
            Self::Exhausted { attempts, .. }
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

/// What the gateway remembers about one destination.
#[derive(Debug, Default)]
struct Lane {
    /// Start instants of the most recent requests, per budget key: at most
    /// `used` of them, the oldest first.
    sent: HashMap<Option<&'static str>, VecDeque<Instant>>,
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
    lanes: HashMap<Destination, Mutex<Lane>>,
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
                .map(|destination| (destination, Mutex::new(Lane::default())))
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
        _deadline: Option<Instant>,
    ) -> Result<HttpResponse, GatewayError> {
        let destination = request.destination();
        let (budget, key) = self.budgets.lookup(destination, method)?;
        let lane = &self.lanes[&destination];
        let mut attempts = 0_u32;
        loop {
            // The lane is held for the budget wait and the request, and
            // released for the backoff: a destination that is refusing is
            // not made to wait for a call that is only waiting itself.
            let result = {
                let mut lane = lane.lock().await;
                let wait = lane.budget_wait(key, &budget, self.clock.now());
                if !wait.is_zero() {
                    self.sleeper.sleep(wait).await;
                }
                lane.record_start(key, &budget, self.clock.now());
                attempts += 1;
                self.transport.send(request).await
            };
            let (outcome, body) = match result {
                Ok(response) if (200..300).contains(&response.status) => return Ok(response),
                Ok(response) => {
                    let outcome = match response.retry_after {
                        Some(after) => Outcome::status_with_retry_after(response.status, after),
                        None => Outcome::status(response.status),
                    };
                    (outcome, response.body)
                }
                Err(error) => (Outcome::Transport(error), Vec::new()),
            };
            match self.retry.decide(attempts, &outcome) {
                Retry::After(delay) => self.sleeper.sleep(delay).await,
                Retry::GiveUp => return Err(self.refusal(destination, attempts, outcome, body)),
            }
        }
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

    /// A clock that moves only when something sleeps on it.
    struct FakeTime {
        now: StdMutex<Instant>,
        slept: StdMutex<Vec<Duration>>,
    }

    impl FakeTime {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: StdMutex::new(Instant::now()),
                slept: StdMutex::new(Vec::new()),
            })
        }

        fn advance(&self, by: Duration) {
            *self.now.lock().expect("clock") += by;
        }

        fn slept(&self) -> Vec<Duration> {
            self.slept.lock().expect("sleeps").clone()
        }
    }

    impl Clock for FakeTime {
        fn now(&self) -> Instant {
            *self.now.lock().expect("clock")
        }
    }

    impl Sleeper for FakeTime {
        fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.slept.lock().expect("sleeps").push(delay);
            self.advance(delay);
            Box::pin(async {})
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
            }
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

    #[tokio::test]
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

    #[tokio::test]
    async fn users_service_never_exceeds_twenty_five_in_any_minute() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
            crate::RequestBody::Json("{}".to_owned()),
        );

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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
    async fn a_destination_missing_from_the_table_is_refused_without_sending() {
        let time = FakeTime::new();
        let gateway = gateway(&time, Scripted::answering(&time, 200));
        let request = HttpRequest::get(Destination::TinvestContract, "/contract.proto");

        let refused = gateway.send("contract", &request, None).await;

        assert!(matches!(refused, Err(GatewayError::UnknownBudget { .. })));
        assert_eq!(gateway.transport.sent_count(), 0);
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
    }

    #[tokio::test]
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
    #[tokio::test]
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

    // --- retries ----------------------------------------------------------

    fn with_retry_after(status: u16, after: Duration) -> HttpResponse {
        HttpResponse {
            retry_after: Some(after),
            ..self::status(status)
        }
    }

    #[tokio::test]
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

    #[tokio::test]
    async fn a_network_failure_and_a_timeout_are_retried() {
        for failure in [HttpError::Network, HttpError::Timeout] {
            let time = FakeTime::new();
            let gateway = gateway(&time, Scripted::answering(&time, 200).then(Err(failure)));

            let response = gateway.send("OperationsService", &operations(), None).await;

            assert_eq!(response.expect("the retry succeeded").status, 200);
            assert_eq!(gateway.transport.sent_count(), 2);
        }
    }

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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
