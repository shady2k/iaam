use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iaam_app::AppServices;
use iaam_app::adapters::finam::FinamChannel;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::adapters::tinkoff::TinkoffChannel;
use iaam_app::ports::{Clock as AppClock, Principal, Scope};
use iaam_app::sync::{BROKER_SYNC_REQUEST_CEILING, BrokerSyncRequest};
use iaam_broker::credentials::{BrokerToken, Key, open, seal};
use iaam_broker::environment::Environment;
use iaam_broker::finam::FinamClient;
use iaam_broker::operation_kind::{OperationKindDictionary, seed_for};
use iaam_broker::tinkoff::TinkoffClient;

use iaam_core::ids::{AccountId, OwnerId, SourceId};

use iaam_http::gateway::{BUDGETS, Budget, Clock, MethodScope, Sleeper, Transport};
use iaam_http::test_support::{HttpClientHarness, LoopbackReply, LoopbackServer};
use iaam_http::{
    BrokerEgress, Destination, Gateway, GatewayError, HttpRequest, HttpResponse, Outbound,
    RequestAllowance, RequestBody,
};

use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const MINUTE: Duration = Duration::from_secs(60);
const CLOSURE: Duration = Duration::from_secs(30 * 60);
const REFUSAL_WINDOW: Duration = Duration::from_secs(10 * 60);
const DAILY_CEILING: usize = 1_000;
const DEFAULT_PAUSE_SECONDS: u64 = 60;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
struct SendRecord {
    logical_at: Duration,
    wire_at: Duration,
    host: String,
    method: String,
    sync: u64,
    call: u64,
    status: u16,
    pause_for: Option<Duration>,
}

#[derive(Clone, Debug)]
struct PairMeasurement {
    host: String,
    method: String,
    reached: usize,
    ceiling: usize,
}

#[derive(Clone, Debug, Default)]
struct Measurements {
    pairs: Vec<PairMeasurement>,
    worst_pair: Option<usize>,
    max_second: Option<usize>,
    max_day: usize,
    max_sync: usize,
    max_closure_span: usize,
    max_pause_span: usize,
    max_attempts: usize,
    locally_refused: usize,
}

fn max_window_by(
    records: &[&SendRecord],
    window: Duration,
    at: impl Fn(&SendRecord) -> Duration,
) -> usize {
    let mut points: Vec<_> = records.iter().map(|record| at(record)).collect();
    points.sort_unstable();
    let mut end = 0;
    let mut maximum = 0;
    for start in 0..points.len() {
        end = end.max(start);
        while end < points.len() && points[end] < points[start].saturating_add(window) {
            end += 1;
        }
        maximum = maximum.max(end - start);
    }
    maximum
}

fn budget_limit(host: &str, method: &str) -> Result<usize, String> {
    BUDGETS
        .iter()
        .find(|row| {
            row.destination.base_url() == host
                && matches!(row.scope, MethodScope::Named(name) if name == method)
        })
        .map(|row| row.used as usize)
        .ok_or_else(|| format!("unknown broker budget pair ({host}, {method})"))
}

fn method_for_target(target: &str) -> Option<&'static str> {
    if target.contains("UsersService/") {
        Some("UsersService")
    } else if target.contains("OperationsService/") {
        Some("OperationsService")
    } else if target.ends_with("/v1/sessions") || target.ends_with("/v1/sessions/details") {
        Some("AuthService.Sessions")
    } else if target.contains("/transactions") {
        Some("AccountsService.Transactions")
    } else if target.contains("/v1/accounts/") {
        Some("AccountsService.GetAccount")
    } else {
        None
    }
}

fn measure(
    records: &[SendRecord],
    locally_refused: usize,
    wire_max_second: Option<usize>,
) -> Result<Measurements, String> {
    let mut measured = Measurements {
        max_second: wire_max_second,
        locally_refused,
        ..Measurements::default()
    };
    let mut methods: BTreeMap<(&str, &str), Vec<&SendRecord>> = BTreeMap::new();
    let mut syncs: BTreeMap<u64, usize> = BTreeMap::new();
    let mut calls: BTreeMap<u64, usize> = BTreeMap::new();
    let mut hosts: BTreeMap<&str, Vec<&SendRecord>> = BTreeMap::new();
    for record in records {
        methods
            .entry((&record.host, &record.method))
            .or_default()
            .push(record);
        if record.sync != 0 {
            *syncs.entry(record.sync).or_default() += 1;
        }
        *calls.entry(record.call).or_default() += 1;
        hosts.entry(&record.host).or_default().push(record);
    }
    for ((host, method), pair_records) in methods {
        measured.pairs.push(PairMeasurement {
            host: host.to_owned(),
            method: method.to_owned(),
            reached: max_window_by(&pair_records, MINUTE, |record| record.logical_at),
            ceiling: budget_limit(host, method)?,
        });
    }
    measured.worst_pair = measured
        .pairs
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            (left.reached * right.ceiling).cmp(&(right.reached * left.ceiling))
        })
        .map(|(index, _)| index);
    measured.max_sync = syncs.values().copied().max().unwrap_or(0);
    measured.max_attempts = calls.values().copied().max().unwrap_or(0);

    for mut host_records in hosts.into_values() {
        measured.max_day = measured
            .max_day
            .max(max_window_by(&host_records, DAY, |record| {
                record.logical_at
            }));
        host_records.sort_by_key(|record| record.logical_at);
        let mut refusals = VecDeque::new();
        let mut rate_limits = VecDeque::new();
        for (index, record) in host_records.iter().enumerate() {
            let queue = if record.status == 429 {
                &mut rate_limits
            } else if (400..=499).contains(&record.status) {
                &mut refusals
            } else {
                continue;
            };
            while queue
                .front()
                .is_some_and(|earlier| record.logical_at.saturating_sub(*earlier) >= REFUSAL_WINDOW)
            {
                queue.pop_front();
            }
            queue.push_back(record.logical_at);
            let closes = (record.status == 429 && queue.len() >= 2)
                || (record.status != 429 && queue.len() >= 3);
            if closes {
                let sends = host_records[index + 1..]
                    .iter()
                    .take_while(|later| {
                        later.logical_at < record.logical_at.saturating_add(CLOSURE)
                    })
                    .count();
                measured.max_closure_span = measured.max_closure_span.max(sends);
            }
            if record.status == 429 {
                let pause = record.pause_for.unwrap_or(MINUTE).max(MINUTE).min(DAY);
                let sends = host_records[index + 1..]
                    .iter()
                    .take_while(|later| later.logical_at < record.logical_at.saturating_add(pause))
                    .count();
                measured.max_pause_span = measured.max_pause_span.max(sends);
            }
        }
    }
    Ok(measured)
}

fn violations(measured: &Measurements) -> Vec<String> {
    let mut found = Vec::new();
    if measured.max_second.is_some_and(|reached| reached > 1) {
        found.push("one-second spacing".to_owned());
    }
    for pair in &measured.pairs {
        if pair.reached > pair.ceiling {
            found.push(format!("minute budget {} {}", pair.host, pair.method));
        }
    }
    if measured.max_day > DAILY_CEILING {
        found.push("rolling 24-hour ceiling".to_owned());
    }
    if measured.max_sync > BROKER_SYNC_REQUEST_CEILING as usize {
        found.push("sync ceiling".to_owned());
    }
    if measured.max_closure_span != 0 {
        found.push("closure".to_owned());
    }
    if measured.max_pause_span != 0 {
        found.push("pause".to_owned());
    }
    if measured.max_attempts > 3 {
        found.push("attempt ceiling".to_owned());
    }
    found
}

fn assert_within_ceilings(mode: &str, measured: &Measurements) {
    let found = violations(measured);
    assert!(found.is_empty(), "{mode}: {found:?}: {measured:?}");
}

fn print_header() {
    println!(
        "mode                       endpoint                                         sec(wire)  minute(method; all pairs checked)               24h       sync      closure  pause    attempts  local"
    );
    println!(
        "                                                                                                                          every governed cell is reached/ceiling; `wire-row` points to the endpoint's wire-spacing row"
    );
}

fn print_row(mode: &str, endpoint: &str, measured: &Measurements) {
    let minute = measured.worst_pair.map_or_else(
        || format!("-/- ({} pairs)", measured.pairs.len()),
        |index| {
            let pair = &measured.pairs[index];
            format!(
                "{}={}/{} ({} pairs)",
                pair.method,
                pair.reached,
                pair.ceiling,
                measured.pairs.len()
            )
        },
    );
    let second = measured
        .max_second
        .map_or_else(|| "wire-row".to_owned(), |reached| format!("{reached}/1"));
    println!(
        "{mode:<26} {endpoint:<48} {second:>9}  {minute:<48} {:>4}/1000  {:>3}/300  {:>3}/0    {:>3}/0   {:>3}/3    {:>3}/-",
        measured.max_day,
        measured.max_sync,
        measured.max_closure_span,
        measured.max_pause_span,
        measured.max_attempts,
        measured.locally_refused,
    );
}

fn planted_record(
    at: Duration,
    host: &'static str,
    method: &'static str,
    sync: u64,
    call: u64,
) -> SendRecord {
    SendRecord {
        logical_at: at,
        wire_at: at,
        host: host.to_owned(),
        method: method.to_owned(),
        sync,
        call,
        status: 200,
        pause_for: None,
    }
}

#[test]
fn checker_rejects_every_independently_planted_breach() {
    let host = Destination::TinkoffProd.base_url();
    let start = Duration::from_secs(1_000);

    let second = vec![
        planted_record(start, host, "OperationsService", 1, 1),
        planted_record(
            start + Duration::from_millis(999),
            host,
            "OperationsService",
            2,
            2,
        ),
    ];
    let measured =
        measure(&second, 0, Some(2)).unwrap_or_else(|error| panic!("measure spacing: {error}"));
    assert!(violations(&measured).contains(&"one-second spacing".to_owned()));

    let minute: Vec<_> = (0..26)
        .map(|index| {
            planted_record(
                start + Duration::from_secs(index * 2),
                host,
                "UsersService",
                index + 1,
                index + 1,
            )
        })
        .collect();
    let measured =
        measure(&minute, 0, Some(1)).unwrap_or_else(|error| panic!("measure minute: {error}"));
    assert!(
        violations(&measured)
            .iter()
            .any(|name| name.contains("minute budget"))
    );

    let unknown = vec![planted_record(start, host, "InventedService", 1, 1)];
    assert!(
        measure(&unknown, 0, Some(1))
            .expect_err("an unknown method must fail the checker")
            .contains("unknown broker budget pair")
    );

    let day: Vec<_> = (0..=DAILY_CEILING)
        .map(|index| {
            planted_record(
                start + Duration::from_secs(index as u64),
                host,
                "OperationsService",
                index as u64 + 1,
                index as u64 + 1,
            )
        })
        .collect();
    let measured = measure(&day, 0, Some(1)).unwrap_or_else(|error| panic!("measure day: {error}"));
    assert!(violations(&measured).contains(&"rolling 24-hour ceiling".to_owned()));

    let sync: Vec<_> = (0..=BROKER_SYNC_REQUEST_CEILING)
        .map(|index| {
            planted_record(
                start + Duration::from_secs(u64::from(index)),
                host,
                "OperationsService",
                1,
                u64::from(index) + 1,
            )
        })
        .collect();
    let measured =
        measure(&sync, 0, Some(1)).unwrap_or_else(|error| panic!("measure sync: {error}"));
    assert!(violations(&measured).contains(&"sync ceiling".to_owned()));

    let attempts: Vec<_> = (0..4)
        .map(|index| {
            planted_record(
                start + Duration::from_secs(index),
                host,
                "OperationsService",
                index + 1,
                1,
            )
        })
        .collect();
    let measured =
        measure(&attempts, 0, Some(1)).unwrap_or_else(|error| panic!("measure attempts: {error}"));
    assert!(violations(&measured).contains(&"attempt ceiling".to_owned()));

    let mut closure: Vec<_> = (0..4)
        .map(|index| {
            planted_record(
                start + Duration::from_secs(index * 61),
                host,
                "OperationsService",
                index + 1,
                index + 1,
            )
        })
        .collect();
    for record in &mut closure[..3] {
        record.status = 400;
    }
    let measured =
        measure(&closure, 0, Some(1)).unwrap_or_else(|error| panic!("measure closure: {error}"));
    assert!(violations(&measured).contains(&"closure".to_owned()));

    let mut pause = vec![
        planted_record(start, host, "OperationsService", 1, 1),
        planted_record(
            start + Duration::from_secs(61),
            host,
            "OperationsService",
            2,
            2,
        ),
    ];
    pause[0].status = 429;
    pause[0].pause_for = Some(Duration::from_secs(120));
    let measured =
        measure(&pause, 0, Some(1)).unwrap_or_else(|error| panic!("measure pause: {error}"));
    assert!(violations(&measured).contains(&"pause".to_owned()));
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "iaam-ceiling-proof-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path)
            .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct FakeTime {
    origin: Instant,
    monotonic: Mutex<Instant>,
    boot_id: Mutex<String>,
    boot_elapsed: Mutex<Duration>,
    wall: Mutex<SystemTime>,
}

impl FakeTime {
    fn new() -> Arc<Self> {
        let origin = Instant::now();
        Arc::new(Self {
            origin,
            monotonic: Mutex::new(origin),
            boot_id: Mutex::new("proof-boot-a".to_owned()),
            boot_elapsed: Mutex::new(Duration::from_secs(10_000)),
            wall: Mutex::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
        })
    }

    fn advance(&self, by: Duration) {
        *self
            .monotonic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += by;
        *self
            .boot_elapsed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += by;
    }

    fn logical_now(&self) -> Duration {
        self.now().saturating_duration_since(self.origin)
    }

    fn change_boot(&self, id: &str) {
        *self
            .boot_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = id.to_owned();
        *self
            .boot_elapsed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Duration::from_secs(10);
    }

    fn set_wall(&self, wall: SystemTime) {
        *self
            .wall
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = wall;
    }
}

impl Clock for FakeTime {
    fn now(&self) -> Instant {
        *self
            .monotonic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn now_boot(&self) -> Result<iaam_http::gateway::BootTime, String> {
        let id = self
            .boot_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let elapsed = *self
            .boot_elapsed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(iaam_http::gateway::BootTime::new(id, elapsed))
    }
}

impl Sleeper for FakeTime {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.advance(delay) })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SleepMode {
    Logical,
    Wire,
    Deadline,
}

struct WireSleeper {
    clock: Arc<FakeTime>,
}

impl Sleeper for WireSleeper {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if delay <= Duration::from_secs(1) {
                tokio::time::sleep(delay).await;
            }
            self.clock.advance(delay);
        })
    }
}

struct DeadlineSleeper {
    clock: Arc<FakeTime>,
}

impl Sleeper for DeadlineSleeper {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if delay > MINUTE {
                tokio::time::sleep(delay).await;
            } else {
                self.clock.advance(delay);
            }
        })
    }
}

struct CallContext {
    next_call: AtomicU64,
    active_call: AtomicU64,
    active_sync: AtomicU64,
    arrivals: AtomicUsize,
    locally_refused: AtomicUsize,
    pause_seconds: AtomicU64,
}

impl CallContext {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            next_call: AtomicU64::new(1),
            active_call: AtomicU64::new(0),
            active_sync: AtomicU64::new(0),
            arrivals: AtomicUsize::new(0),
            locally_refused: AtomicUsize::new(0),
            pause_seconds: AtomicU64::new(DEFAULT_PAUSE_SECONDS),
        })
    }
}

struct ObservedOutbound<T: Transport> {
    gateway: Arc<Gateway<T>>,
    context: Arc<CallContext>,
}

impl<T: Transport> Outbound for ObservedOutbound<T> {
    fn send<'a>(
        &'a self,
        method: &'static str,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>> {
        let call = self.context.next_call.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.context.active_call.store(call, Ordering::SeqCst);
            let before = self.context.arrivals.load(Ordering::SeqCst);
            let result = self.gateway.send(method, request, deadline).await;

            if self.context.arrivals.load(Ordering::SeqCst) == before {
                self.context.locally_refused.fetch_add(1, Ordering::SeqCst);
            }
            self.context.active_call.store(0, Ordering::SeqCst);
            result
        })
    }
}

struct Scenario {
    outbound: Arc<dyn Outbound>,
    server: LoopbackServer,
    clock: Arc<FakeTime>,
    context: Arc<CallContext>,
    records: Arc<Mutex<Vec<SendRecord>>>,
    _directory: TempDir,
}

impl Scenario {
    fn new(
        label: &str,
        destination: Destination,
        replies: impl IntoIterator<Item = LoopbackReply>,
        timeout: Duration,
        egress: bool,
    ) -> Self {
        Self::build(
            label,
            destination,
            replies,
            timeout,
            egress,
            SleepMode::Logical,
        )
    }

    fn new_wire_spacing(
        label: &str,
        destination: Destination,
        replies: impl IntoIterator<Item = LoopbackReply>,
    ) -> Self {
        Self::build(
            label,
            destination,
            replies,
            Duration::from_secs(5),
            true,
            SleepMode::Wire,
        )
    }

    fn new_sync(
        label: &str,
        destination: Destination,
        replies: impl IntoIterator<Item = LoopbackReply>,
    ) -> Self {
        Self::build(
            label,
            destination,
            replies,
            Duration::from_secs(5),
            true,
            SleepMode::Deadline,
        )
    }

    fn build(
        label: &str,
        destination: Destination,
        replies: impl IntoIterator<Item = LoopbackReply>,
        timeout: Duration,
        egress: bool,
        sleep_mode: SleepMode,
    ) -> Self {
        let directory = TempDir::new(label);
        let tally = directory.path("tally");
        if egress {
            std::fs::write(&tally, "")
                .unwrap_or_else(|error| panic!("create {}: {error}", tally.display()));
        }
        let clock = FakeTime::new();
        let context = CallContext::new();
        if sleep_mode == SleepMode::Deadline {
            context.active_sync.store(1, Ordering::SeqCst);
        }
        let records = Arc::new(Mutex::new(Vec::new()));
        let wire_origin = Instant::now();
        let observed_clock = Arc::clone(&clock);
        let observed_context = Arc::clone(&context);
        let observed_records = Arc::clone(&records);
        let host = destination.base_url().to_owned();
        let server = LoopbackServer::start_observed(replies, move |target, status| {
            let method = method_for_target(target)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("unknown:{target}"));
            let pause_for = (status == 429).then(|| {
                Duration::from_secs(
                    observed_context
                        .pause_seconds
                        .load(Ordering::SeqCst)
                        .max(DEFAULT_PAUSE_SECONDS),
                )
            });
            observed_records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(SendRecord {
                    logical_at: observed_clock.logical_now(),
                    wire_at: Instant::now().saturating_duration_since(wire_origin),
                    host: host.clone(),
                    method,
                    sync: observed_context.active_sync.load(Ordering::SeqCst),
                    call: observed_context.active_call.load(Ordering::SeqCst),
                    status,
                    pause_for,
                });
            observed_context.arrivals.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap_or_else(|error| panic!("start loopback server: {error}"));
        let transport = HttpClientHarness::new(&server).with_timeout(timeout);
        let sleeper: Arc<dyn Sleeper> = match sleep_mode {
            SleepMode::Wire => Arc::new(WireSleeper {
                clock: Arc::clone(&clock),
            }),
            SleepMode::Deadline => Arc::new(DeadlineSleeper {
                clock: Arc::clone(&clock),
            }),
            SleepMode::Logical => Arc::clone(&clock) as Arc<dyn Sleeper>,
        };
        let gateway = Gateway::with_parts(
            transport,
            BUDGETS,
            Arc::clone(&clock) as Arc<dyn Clock>,
            sleeper,
            if egress {
                BrokerEgress::On { tally }
            } else {
                BrokerEgress::Off
            },
        )
        .unwrap_or_else(|error| panic!("build proof gateway: {error}"));
        let outbound = Arc::new(ObservedOutbound {
            gateway: Arc::new(gateway),
            context: Arc::clone(&context),
        });
        Self {
            outbound,
            server,
            clock,
            context,
            records,
            _directory: directory,
        }
    }

    fn records(&self) -> Vec<SendRecord> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn locally_refused(&self) -> usize {
        self.context.locally_refused.load(Ordering::SeqCst)
    }

    fn measured(&self, wire_max_second: Option<usize>) -> Measurements {
        measure(&self.records(), self.locally_refused(), wire_max_second)
            .unwrap_or_else(|error| panic!("measure loopback arrivals: {error}"))
    }

    fn wire_max_second(&self) -> usize {
        let records = self.records();
        let references: Vec<_> = records.iter().collect();
        max_window_by(&references, Duration::from_secs(1), |record| record.wire_at)
    }

    fn verify_and_print(
        &self,
        mode: &str,
        endpoint: Destination,
        wire_max_second: Option<usize>,
    ) -> Measurements {
        let measured = self.measured(wire_max_second);
        assert_within_ceilings(mode, &measured);
        print_row(mode, endpoint.base_url(), &measured);
        measured
    }
}

fn direct_request(destination: Destination, path: &str) -> HttpRequest {
    HttpRequest::post(destination, path, RequestBody::Json("{}".to_owned()))
        .idempotent()
        .with_request_allowance(RequestAllowance::new(BROKER_SYNC_REQUEST_CEILING))
}

fn direct_path(destination: Destination) -> &'static str {
    match destination {
        Destination::TinkoffProd | Destination::TinkoffSandbox => {
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations"
        }
        Destination::FinamApi => "/v1/accounts/account",
        Destination::MoexIss
        | Destination::CbrScripts
        | Destination::CbrDailyInfo
        | Destination::TinvestContract => panic!("not a broker endpoint"),
    }
}

fn direct_method(destination: Destination) -> &'static str {
    match destination {
        Destination::TinkoffProd | Destination::TinkoffSandbox => "OperationsService",
        Destination::FinamApi => "AccountsService.GetAccount",
        Destination::MoexIss
        | Destination::CbrScripts
        | Destination::CbrDailyInfo
        | Destination::TinvestContract => panic!("not a broker endpoint"),
    }
}

async fn direct_send(
    scenario: &Scenario,
    destination: Destination,
) -> Result<HttpResponse, GatewayError> {
    let request = direct_request(destination, direct_path(destination));
    scenario
        .outbound
        .send(direct_method(destination), &request, None)
        .await
}

async fn exercise_wire_spacing(destination: Destination) -> Scenario {
    let scenario = Scenario::new_wire_spacing(
        "wire-spacing",
        destination,
        [
            LoopbackReply::complete(200, "{}"),
            LoopbackReply::complete(200, "{}"),
        ],
    );
    direct_send(&scenario, destination)
        .await
        .unwrap_or_else(|error| panic!("first wire-spaced send: {error}"));
    direct_send(&scenario, destination)
        .await
        .unwrap_or_else(|error| panic!("second wire-spaced send: {error}"));
    assert_eq!(scenario.server.requests_received(), 2);
    assert_eq!(
        scenario.wire_max_second(),
        1,
        "loopback received two requests inside one real second"
    );
    scenario
}

fn token() -> BrokerToken {
    let key = Key::from_bytes([7; 32]);
    open(&key, &seal(&key, "invented-proof-token"))
        .unwrap_or_else(|error| panic!("invented token: {error}"))
}

fn dictionary(broker: &str) -> OperationKindDictionary {
    let (_, rows) = seed_for(broker).unwrap_or_else(|| panic!("dictionary seed for {broker}"));
    let (dictionary, unreadable) = OperationKindDictionary::build(rows.iter().copied());
    assert!(
        unreadable.is_empty(),
        "unreadable dictionary rows: {unreadable:?}"
    );
    dictionary
}

struct FixedAppClock;

impl AppClock for FixedAppClock {
    fn today(&self) -> Date {
        date!(2026 - 01 - 01)
    }
}

async fn services() -> (AppServices, Principal, AccountId) {
    let raw = SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}"));
    let adapter = Arc::new(SqliteAdapter::new(raw));
    let services = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        Arc::new(FixedAppClock),
    );
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    services
        .store
        .upsert_account(
            owner,
            iaam_app::ports::AccountView {
                id: account,
                title: "Proof account".to_owned(),
                institution: Some("Invented broker".to_owned()),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("seed proof account: {error}"));
    let principal = Principal {
        token_id: uuid::Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    };
    (services, principal, account)
}

async fn exercise_permanent_refusal(destination: Destination) -> Scenario {
    let scenario = Scenario::new(
        "permanent-refusal",
        destination,
        std::iter::repeat_n(LoopbackReply::complete(400, "refused"), 3),
        Duration::from_secs(2),
        true,
    );
    for _ in 0..4 {
        let _ = direct_send(&scenario, destination).await;
    }
    assert_eq!(scenario.server.requests_received(), 3);
    assert!(scenario.locally_refused() >= 1);
    scenario
}

async fn exercise_rate_limit(
    label: &str,
    destination: Destination,
    reply: LoopbackReply,
    pause: Duration,
) -> Scenario {
    let scenario = Scenario::new(
        label,
        destination,
        [reply.clone(), reply],
        Duration::from_millis(40),
        true,
    );
    scenario
        .context
        .pause_seconds
        .store(pause.as_secs(), Ordering::SeqCst);
    let _ = direct_send(&scenario, destination).await;
    scenario.clock.advance(pause.max(MINUTE));
    let _ = direct_send(&scenario, destination).await;
    let _ = direct_send(&scenario, destination).await;
    assert_eq!(scenario.server.requests_received(), 2);
    assert!(scenario.locally_refused() >= 1);
    scenario
}

async fn exercise_body_rate_limit(label: &str, reply: LoopbackReply) -> Scenario {
    let scenario = Scenario::new(
        label,
        Destination::FinamApi,
        [reply],
        Duration::from_millis(40),
        true,
    );
    let _ = direct_send(&scenario, Destination::FinamApi).await;
    let _ = direct_send(&scenario, Destination::FinamApi).await;
    assert_eq!(scenario.server.requests_received(), 1);
    assert!(scenario.locally_refused() >= 1);
    scenario
}

async fn exercise_finam_renewal() -> Scenario {
    let scenario = Scenario::new(
        "finam-renewal",
        Destination::FinamApi,
        [
            LoopbackReply::complete(200, r#"{"token":"session-a"}"#),
            LoopbackReply::complete(401, "expired"),
            LoopbackReply::complete(200, r#"{"token":"session-b"}"#),
            LoopbackReply::complete(401, "expired"),
            LoopbackReply::complete(401, "expired"),
        ],
        Duration::from_secs(2),
        true,
    );
    let client = FinamClient::new(token(), Arc::clone(&scenario.outbound));
    for _ in 0..2 {
        let _ = client
            .get_account_ids(&RequestAllowance::new(BROKER_SYNC_REQUEST_CEILING))
            .await;
    }
    assert_eq!(scenario.server.requests_received(), 5);
    assert!(scenario.locally_refused() >= 1);
    scenario
}

async fn exercise_transient(label: &str, reply: LoopbackReply) -> Scenario {
    let scenario = Scenario::new(
        label,
        Destination::TinkoffProd,
        std::iter::repeat_n(reply, 3),
        Duration::from_millis(30),
        true,
    );
    let result = direct_send(&scenario, Destination::TinkoffProd).await;
    assert!(result.is_err(), "transient failure unexpectedly succeeded");
    assert_eq!(scenario.server.requests_received(), 3);
    scenario
}

async fn exercise_redirects() -> (Scenario, Scenario) {
    let same = Scenario::new(
        "same-host-redirect",
        Destination::TinkoffProd,
        [
            LoopbackReply::redirect(307, direct_path(Destination::TinkoffProd)),
            LoopbackReply::redirect(307, direct_path(Destination::TinkoffProd)),
            LoopbackReply::complete(200, "followed"),
        ],
        Duration::from_secs(2),
        true,
    );
    let _ = direct_send(&same, Destination::TinkoffProd).await;
    assert_eq!(same.server.requests_received(), 1);

    let cross_target = LoopbackServer::start([LoopbackReply::complete(200, "leaked")])
        .unwrap_or_else(|error| panic!("cross-host loopback: {error}"));
    let cross = Scenario::new(
        "cross-host-redirect",
        Destination::TinkoffSandbox,
        [LoopbackReply::redirect(
            307,
            &format!("{}/leaked", cross_target.base_url()),
        )],
        Duration::from_secs(2),
        true,
    );
    let _ = direct_send(&cross, Destination::TinkoffSandbox).await;
    assert_eq!(cross.server.requests_received(), 1);
    assert_eq!(cross_target.requests_received(), 0);
    drop(cross_target);
    (same, cross)
}

async fn exercise_daily_ceiling() -> Scenario {
    let scenario = Scenario::new(
        "rolling-day",
        Destination::TinkoffSandbox,
        std::iter::repeat_n(LoopbackReply::complete(200, "{}"), DAILY_CEILING),
        Duration::from_secs(2),
        true,
    );
    for index in 0..=DAILY_CEILING {
        let result = direct_send(&scenario, Destination::TinkoffSandbox).await;
        if index < DAILY_CEILING {
            assert!(
                result.is_ok(),
                "send {index} was refused before the daily ceiling"
            );
        } else {
            assert!(
                matches!(result, Err(GatewayError::DailyCeiling { .. })),
                "send beyond the daily ceiling was not refused: {result:?}"
            );
        }
    }
    let records = scenario.records();
    assert_eq!(records.len(), DAILY_CEILING);
    let span = records
        .last()
        .zip(records.first())
        .map(|(last, first)| last.logical_at.saturating_sub(first.logical_at))
        .unwrap_or(Duration::ZERO);
    assert!(
        span < DAY,
        "the proof did not reach the ceiling inside 24 hours"
    );
    scenario
}

async fn exercise_tinkoff_sync() -> Scenario {
    let mut replies = Vec::with_capacity(BROKER_SYNC_REQUEST_CEILING as usize);
    replies.push(LoopbackReply::complete(
        200,
        r#"{"accounts":[{"id":"account"}]}"#,
    ));
    for cursor in 1..BROKER_SYNC_REQUEST_CEILING {
        replies.push(LoopbackReply::complete(
            200,
            format!("{{\"hasNext\":true,\"nextCursor\":\"cursor-{cursor}\",\"items\":[]}}"),
        ));
    }
    let scenario = Scenario::new_sync("tinkoff-real-sync", Destination::TinkoffProd, replies);
    let client = TinkoffClient::new(Environment::Prod, token(), Arc::clone(&scenario.outbound));
    let channel = TinkoffChannel::new(client, SourceId::new_random(), dictionary("tinkoff"));
    let (services, principal, account) = services().await;
    let result = iaam_app::sync::sync_broker(
        &services,
        &principal,
        &channel,
        BrokerSyncRequest {
            broker_code: iaam_store::documents::BrokerCode::parse("tinkoff")
                .unwrap_or_else(|| panic!("tinkoff broker code")),
            account,
            from: date!(2026 - 01 - 01),
            to: date!(2026 - 12 - 31),
        },
    )
    .await;
    assert!(result.is_err(), "endless T-Invest pages completed a sync");
    assert_eq!(
        scenario.server.requests_received(),
        BROKER_SYNC_REQUEST_CEILING as usize,
        "real T-Invest sync stopped early: {result:?}; accepted={}; targets={:?}; local={}",
        scenario.server.connections_accepted(),
        scenario.server.request_targets(),
        scenario.locally_refused()
    );
    assert!(scenario.locally_refused() >= 1);
    scenario
}

fn script_finam_intervals(replies: &mut Vec<LoopbackReply>, full_page: &str, from: Date, to: Date) {
    if replies.len() >= BROKER_SYNC_REQUEST_CEILING as usize {
        return;
    }
    if from == to {
        replies.push(LoopbackReply::complete(200, r#"{"transactions":[]}"#));
        return;
    }
    replies.push(LoopbackReply::complete(200, full_page.to_owned()));
    let whole_days = (to - from).whole_days();
    let middle = from + time::Duration::days(whole_days / 2);
    script_finam_intervals(replies, full_page, from, middle);
    if replies.len() >= BROKER_SYNC_REQUEST_CEILING as usize {
        return;
    }
    let after_middle = middle
        .next_day()
        .unwrap_or_else(|| panic!("proof Finam split has a next day"));
    script_finam_intervals(replies, full_page, after_middle, to);
}

async fn exercise_finam_sync() -> Scenario {
    let full_page = format!(
        "{{\"transactions\":[{}]}}",
        std::iter::repeat_n("{}", 1_000)
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut replies = Vec::with_capacity(BROKER_SYNC_REQUEST_CEILING as usize);
    replies.push(LoopbackReply::complete(200, r#"{"token":"session-proof"}"#));
    replies.push(LoopbackReply::complete(
        200,
        r#"{"account_ids":["account"]}"#,
    ));
    script_finam_intervals(
        &mut replies,
        &full_page,
        date!(2026 - 01 - 01),
        date!(2026 - 12 - 31),
    );
    let scenario = Scenario::new_sync("finam-real-sync", Destination::FinamApi, replies);
    let client = FinamClient::new(token(), Arc::clone(&scenario.outbound));
    let channel = FinamChannel::new(client, SourceId::new_random(), dictionary("finam"));
    let (services, principal, account) = services().await;
    let result = iaam_app::sync::sync_broker(
        &services,
        &principal,
        &channel,
        BrokerSyncRequest {
            broker_code: iaam_store::documents::BrokerCode::parse("finam")
                .unwrap_or_else(|| panic!("finam broker code")),
            account,
            from: date!(2026 - 01 - 01),
            to: date!(2026 - 12 - 31),
        },
    )
    .await;
    assert!(result.is_err(), "endless Finam pages completed a sync");
    assert_eq!(
        scenario.server.requests_received(),
        BROKER_SYNC_REQUEST_CEILING as usize,
        "real Finam sync stopped early: {result:?}; accepted={}; targets={:?}; local={}",
        scenario.server.connections_accepted(),
        scenario.server.request_targets(),
        scenario.locally_refused()
    );
    assert!(scenario.locally_refused() >= 1);
    scenario
}

async fn exercise_concurrent_callers() -> Scenario {
    let scenario = Scenario::new(
        "concurrent-callers",
        Destination::TinkoffProd,
        [
            LoopbackReply::held(200, "{}"),
            LoopbackReply::complete(200, "{}"),
        ],
        Duration::from_secs(5),
        true,
    );
    let first_outbound = Arc::clone(&scenario.outbound);
    let first = tokio::spawn(async move {
        let request = direct_request(
            Destination::TinkoffProd,
            direct_path(Destination::TinkoffProd),
        );
        first_outbound
            .send("OperationsService", &request, None)
            .await
    });
    wait_for_arrivals(&scenario, 1).await;

    let second_started = Arc::new(AtomicBool::new(false));
    let task_started = Arc::clone(&second_started);
    let second_outbound = Arc::clone(&scenario.outbound);
    let second = tokio::spawn(async move {
        task_started.store(true, Ordering::SeqCst);
        let request = direct_request(
            Destination::TinkoffProd,
            direct_path(Destination::TinkoffProd),
        );
        second_outbound
            .send("OperationsService", &request, None)
            .await
    });
    while !second_started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        scenario.server.requests_received(),
        1,
        "the second caller departed before the first status was committed"
    );
    scenario.server.release_one();
    first
        .await
        .unwrap_or_else(|error| panic!("first caller task: {error}"))
        .unwrap_or_else(|error| panic!("first caller: {error}"));
    second
        .await
        .unwrap_or_else(|error| panic!("second caller task: {error}"))
        .unwrap_or_else(|error| panic!("second caller: {error}"));
    let records = scenario.records();
    assert_eq!(records.len(), 2);
    assert!(records[1].logical_at.saturating_sub(records[0].logical_at) >= Duration::from_secs(1));
    scenario
}

async fn wait_for_arrivals(scenario: &Scenario, wanted: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if scenario.server.requests_received() >= wanted {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("loopback server did not receive {wanted} requests"));
}

async fn exercise_boot_change() -> Scenario {
    let scenario = Scenario::new(
        "boot-change",
        Destination::TinkoffProd,
        std::iter::repeat_n(LoopbackReply::complete(400, "refused"), 4),
        Duration::from_secs(2),
        true,
    );
    for _ in 0..3 {
        let _ = direct_send(&scenario, Destination::TinkoffProd).await;
    }
    let before = scenario.server.requests_received();
    scenario.clock.change_boot("proof-boot-b");
    let _ = direct_send(&scenario, Destination::TinkoffProd).await;
    scenario.clock.advance(CLOSURE - Duration::from_secs(1));
    let _ = direct_send(&scenario, Destination::TinkoffProd).await;
    assert_eq!(scenario.server.requests_received(), before);
    scenario.clock.advance(Duration::from_secs(1));
    let _ = direct_send(&scenario, Destination::TinkoffProd).await;
    assert_eq!(scenario.server.requests_received(), before + 1);
    scenario
}

async fn exercise_wall_change() -> Scenario {
    let scenario = Scenario::new(
        "wall-change",
        Destination::TinkoffSandbox,
        [
            LoopbackReply::complete(429, "limited"),
            LoopbackReply::complete(200, "{}"),
        ],
        Duration::from_secs(2),
        true,
    );
    let _ = direct_send(&scenario, Destination::TinkoffSandbox).await;
    scenario
        .clock
        .set_wall(UNIX_EPOCH + Duration::from_secs(4_000_000_000));
    let _ = direct_send(&scenario, Destination::TinkoffSandbox).await;
    scenario.clock.set_wall(UNIX_EPOCH);
    let _ = direct_send(&scenario, Destination::TinkoffSandbox).await;
    assert_eq!(scenario.server.requests_received(), 1);
    scenario.clock.advance(MINUTE);
    direct_send(&scenario, Destination::TinkoffSandbox)
        .await
        .unwrap_or_else(|error| panic!("send after boot-clock pause: {error}"));
    assert_eq!(scenario.server.requests_received(), 2);
    scenario
}

fn assert_path_aliases_refused() {
    let directory = TempDir::new("path-aliases");
    let server = LoopbackServer::start(std::iter::empty())
        .unwrap_or_else(|error| panic!("path loopback: {error}"));
    let clock = FakeTime::new();
    let build = |path: PathBuf| {
        Gateway::with_parts(
            HttpClientHarness::new(&server),
            BUDGETS,
            Arc::clone(&clock) as Arc<dyn Clock>,
            Arc::clone(&clock) as Arc<dyn Sleeper>,
            BrokerEgress::On { tally: path },
        )
    };

    assert!(build(PathBuf::from("relative-tally")).is_err());

    let symlink_target = directory.path("symlink-target");
    let symlink = directory.path("symlink");
    std::fs::write(&symlink_target, "").unwrap_or_else(|error| panic!("symlink target: {error}"));
    std::os::unix::fs::symlink(&symlink_target, &symlink)
        .unwrap_or_else(|error| panic!("create tally symlink: {error}"));
    assert!(build(symlink).is_err());

    let hard_target = directory.path("hard-target");
    let hard_alias = directory.path("hard-alias");
    std::fs::write(&hard_target, "").unwrap_or_else(|error| panic!("hard-link target: {error}"));
    std::fs::hard_link(&hard_target, &hard_alias)
        .unwrap_or_else(|error| panic!("create tally hard link: {error}"));
    assert!(build(hard_alias).is_err());
    assert_eq!(server.requests_received(), 0);
}

fn wait_for_file(path: &Path, child: &mut Child, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|error| panic!("inspect {what}: {error}"))
        {
            panic!("{what} exited before its marker: {status}");
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn process_child() {
    let Ok(role) = std::env::var("IAAM_CEILING_PROCESS_ROLE") else {
        return;
    };
    let tally = PathBuf::from(
        std::env::var("IAAM_CEILING_PROCESS_TALLY")
            .unwrap_or_else(|error| panic!("child tally: {error}")),
    );
    let ready = PathBuf::from(
        std::env::var("IAAM_CEILING_PROCESS_READY")
            .unwrap_or_else(|error| panic!("child ready: {error}")),
    );
    let done = PathBuf::from(
        std::env::var("IAAM_CEILING_PROCESS_DONE")
            .unwrap_or_else(|error| panic!("child done: {error}")),
    );
    let result = PathBuf::from(
        std::env::var("IAAM_CEILING_PROCESS_RESULT")
            .unwrap_or_else(|error| panic!("child result: {error}")),
    );
    let server = LoopbackServer::start([LoopbackReply::complete(200, "{}")])
        .unwrap_or_else(|error| panic!("child loopback: {error}"));
    let clock = FakeTime::new();
    let gateway = Gateway::with_parts(
        HttpClientHarness::new(&server),
        BUDGETS,
        Arc::clone(&clock) as Arc<dyn Clock>,
        Arc::clone(&clock) as Arc<dyn Sleeper>,
        BrokerEgress::On { tally },
    )
    .unwrap_or_else(|error| panic!("child gateway: {error}"));

    if role == "owner" {
        gateway
            .send(
                "OperationsService",
                &direct_request(
                    Destination::TinkoffProd,
                    direct_path(Destination::TinkoffProd),
                ),
                None,
            )
            .await
            .unwrap_or_else(|error| panic!("owner send: {error}"));
        std::fs::write(&ready, "ready")
            .unwrap_or_else(|error| panic!("owner ready marker: {error}"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done.exists() {
            assert!(
                Instant::now() < deadline,
                "owner timed out waiting for contender"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::write(&result, format!("tinkoff={}\n", server.requests_received()))
            .unwrap_or_else(|error| panic!("owner result: {error}"));
        return;
    }

    assert_eq!(role, "contender");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "contender timed out waiting for owner"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let owned = gateway
        .send(
            "OperationsService",
            &direct_request(
                Destination::TinkoffProd,
                direct_path(Destination::TinkoffProd),
            ),
            None,
        )
        .await;
    assert!(matches!(
        owned,
        Err(GatewayError::BrokerEndpointOwned { .. })
    ));
    assert_eq!(server.requests_received(), 0);
    gateway
        .send(
            "AccountsService.GetAccount",
            &direct_request(Destination::FinamApi, direct_path(Destination::FinamApi)),
            None,
        )
        .await
        .unwrap_or_else(|error| panic!("contender Finam send: {error}"));
    std::fs::write(
        &result,
        format!("tinkoff=0\nfinam={}\nlocal=1\n", server.requests_received()),
    )
    .unwrap_or_else(|error| panic!("contender result: {error}"));
    std::fs::write(&done, "done").unwrap_or_else(|error| panic!("contender done marker: {error}"));
}

fn process_measurements() -> (Measurements, Measurements) {
    let directory = TempDir::new("processes");
    let tally = directory.path("tally");
    let ready = directory.path("ready");
    let done = directory.path("done");
    let owner_result = directory.path("owner-result");
    let contender_result = directory.path("contender-result");
    std::fs::write(&tally, "").unwrap_or_else(|error| panic!("process tally: {error}"));
    let executable = std::env::current_exe().unwrap_or_else(|error| panic!("test binary: {error}"));
    let child = |role: &str, result: &Path| {
        Command::new(&executable)
            .args(["--exact", "process_child", "--nocapture"])
            .env("IAAM_CEILING_PROCESS_ROLE", role)
            .env("IAAM_CEILING_PROCESS_TALLY", &tally)
            .env("IAAM_CEILING_PROCESS_READY", &ready)
            .env("IAAM_CEILING_PROCESS_DONE", &done)
            .env("IAAM_CEILING_PROCESS_RESULT", result)
            .spawn()
            .unwrap_or_else(|error| panic!("spawn {role} child: {error}"))
    };
    let mut owner = child("owner", &owner_result);
    wait_for_file(&ready, &mut owner, "owner child");
    let mut contender = child("contender", &contender_result);
    let contender_status = contender
        .wait()
        .unwrap_or_else(|error| panic!("wait contender: {error}"));
    assert!(
        contender_status.success(),
        "contender failed: {contender_status}"
    );
    let owner_status = owner
        .wait()
        .unwrap_or_else(|error| panic!("wait owner: {error}"));
    assert!(owner_status.success(), "owner failed: {owner_status}");
    let owner_text = std::fs::read_to_string(&owner_result)
        .unwrap_or_else(|error| panic!("read owner result: {error}"));
    let contender_text = std::fs::read_to_string(&contender_result)
        .unwrap_or_else(|error| panic!("read contender result: {error}"));
    assert_eq!(owner_text, "tinkoff=1\n");
    assert_eq!(contender_text, "tinkoff=0\nfinam=1\nlocal=1\n");

    let tinkoff = measure(
        &[planted_record(
            Duration::from_secs(60),
            Destination::TinkoffProd.base_url(),
            "OperationsService",
            1,
            1,
        )],
        1,
        None,
    )
    .unwrap_or_else(|error| panic!("measure process T-Invest: {error}"));
    let finam = measure(
        &[planted_record(
            Duration::from_secs(60),
            Destination::FinamApi.base_url(),
            "AccountsService.GetAccount",
            1,
            1,
        )],
        0,
        None,
    )
    .unwrap_or_else(|error| panic!("measure process Finam: {error}"));
    assert_within_ceilings("process-owner", &tinkoff);
    assert_within_ceilings("process-other-endpoint", &finam);
    (tinkoff, finam)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executable_ceiling_proof() {
    print_header();

    for destination in [
        Destination::TinkoffProd,
        Destination::TinkoffSandbox,
        Destination::FinamApi,
    ] {
        let scenario = exercise_wire_spacing(destination).await;
        let reached = scenario.wire_max_second();
        scenario.verify_and_print("wire-spacing", destination, Some(reached));
    }

    for destination in [
        Destination::TinkoffProd,
        Destination::TinkoffSandbox,
        Destination::FinamApi,
    ] {
        let scenario = exercise_permanent_refusal(destination).await;
        scenario.verify_and_print("always-400", destination, None);
    }

    let finam_renewal = exercise_finam_renewal().await;
    finam_renewal.verify_and_print("always-401-renewal", Destination::FinamApi, None);

    let retry_after = exercise_rate_limit(
        "retry-after",
        Destination::TinkoffProd,
        LoopbackReply::complete(429, "limited").with_header("Retry-After", "120"),
        Duration::from_secs(120),
    )
    .await;
    retry_after.verify_and_print("429-retry-after", Destination::TinkoffProd, None);

    let no_header = exercise_rate_limit(
        "no-retry-after",
        Destination::TinkoffSandbox,
        LoopbackReply::complete(429, "limited"),
        MINUTE,
    )
    .await;
    no_header.verify_and_print("429-no-header", Destination::TinkoffSandbox, None);

    let reset_body = exercise_body_rate_limit(
        "reset-body",
        LoopbackReply::truncated_body(429, "partial").with_header("Retry-After", "60"),
    )
    .await;
    reset_body.verify_and_print("429-reset-body", Destination::FinamApi, None);

    let stalled_body = exercise_body_rate_limit(
        "stalled-body",
        LoopbackReply::stalled_body(429, "partial").with_header("Retry-After", "60"),
    )
    .await;
    stalled_body.verify_and_print("429-stalled-body", Destination::FinamApi, None);

    let server_error = exercise_transient("always-500", LoopbackReply::complete(500, "down")).await;
    server_error.verify_and_print("always-500", Destination::TinkoffProd, None);

    let timeout = exercise_transient("timeouts", LoopbackReply::held(200, "late")).await;
    timeout.verify_and_print("timeouts", Destination::TinkoffProd, None);

    let (same_redirect, cross_redirect) = exercise_redirects().await;
    same_redirect.verify_and_print("redirect-same-host", Destination::TinkoffProd, None);
    cross_redirect.verify_and_print("redirect-cross-host", Destination::TinkoffSandbox, None);

    let tinkoff_sync = exercise_tinkoff_sync().await;
    let tinkoff_measured =
        tinkoff_sync.verify_and_print("endless-pages-sync", Destination::TinkoffProd, None);
    assert_eq!(
        tinkoff_measured.max_sync,
        BROKER_SYNC_REQUEST_CEILING as usize
    );

    let finam_sync = exercise_finam_sync().await;
    let finam_measured =
        finam_sync.verify_and_print("endless-pages-sync", Destination::FinamApi, None);
    assert_eq!(
        finam_measured.max_sync,
        BROKER_SYNC_REQUEST_CEILING as usize
    );

    let daily = exercise_daily_ceiling().await;
    let daily_measured = daily.verify_and_print("rolling-24h", Destination::TinkoffSandbox, None);
    assert_eq!(daily_measured.max_day, DAILY_CEILING);

    let concurrent = exercise_concurrent_callers().await;
    concurrent.verify_and_print("concurrent-callers", Destination::TinkoffProd, None);

    let boot = exercise_boot_change().await;
    boot.verify_and_print("boot-id-change", Destination::TinkoffProd, None);

    let wall = exercise_wall_change().await;
    wall.verify_and_print("wall-clock-change", Destination::TinkoffSandbox, None);

    let egress_off = Scenario::new(
        "egress-off",
        Destination::TinkoffProd,
        [LoopbackReply::complete(200, "{}")],
        Duration::from_secs(2),
        false,
    );
    let _ = direct_send(&egress_off, Destination::TinkoffProd).await;
    assert_eq!(egress_off.server.requests_received(), 0);
    egress_off.verify_and_print("egress-off", Destination::TinkoffProd, None);

    assert_path_aliases_refused();

    let (process_tinkoff, process_finam) = process_measurements();
    print_row(
        "process-owner",
        Destination::TinkoffProd.base_url(),
        &process_tinkoff,
    );
    print_row(
        "process-other-endpoint",
        Destination::FinamApi.base_url(),
        &process_finam,
    );
}

#[test]
fn documented_budget_table_keeps_half_the_published_limits() {
    for Budget {
        documented, used, ..
    } in BUDGETS
    {
        if let Some(published) = documented {
            assert_eq!(*used * 2, *published);
        }
    }
}
