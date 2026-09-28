use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::adapters::tinkoff::TinkoffChannel;
use iaam_app::ports::{
    BrokerChannel, BrokerError, BrokerRequestContext, Clock as AppClock, ParsedOperations,
    PortfolioAsOf, PortfolioSnapshot, Principal, Scope,
};
use iaam_app::sync::{BROKER_SYNC_REQUEST_CEILING, BrokerSyncRequest};
use iaam_broker::credentials::{BrokerToken, Key, open, seal};
use iaam_broker::environment::Environment;
use iaam_broker::finam::FinamClient;
use iaam_broker::operation_kind::{OperationKindDictionary, seed_for};
use iaam_broker::tinkoff::TinkoffClient;
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, OwnerId, SourceId};
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_http::gateway::{BUDGETS, Budget, Clock, MethodScope, Sleeper, Transport};
use iaam_http::{
    BrokerEgress, Destination, Gateway, GatewayError, HttpError, HttpRequest, HttpResponse,
    Outbound, RequestAllowance, RequestBody,
};
use iaam_ingest::dedup::IdentityScope;
use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const MINUTE: Duration = Duration::from_secs(60);
const CLOSURE: Duration = Duration::from_secs(30 * 60);
const REFUSAL_WINDOW: Duration = Duration::from_secs(10 * 60);
const DAILY_CEILING: usize = 1_000;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
struct SendRecord {
    at: SystemTime,
    host: String,
    method: String,
    sync: u64,
    call: u64,
    status: Option<u16>,
    pause_for: Option<Duration>,
}

#[derive(Clone, Debug, Default)]
struct Measurements {
    max_second: usize,
    max_minute: usize,
    minute_method: String,
    minute_ceiling: usize,
    max_day: usize,
    max_sync: usize,
    max_closure_span: usize,
    max_pause_span: usize,
    max_attempts: usize,
}

fn nanos(at: SystemTime) -> u128 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos()
}

fn max_window(records: &[&SendRecord], window: Duration) -> usize {
    let mut points: Vec<_> = records.iter().map(|record| nanos(record.at)).collect();
    points.sort_unstable();
    let width = window.as_nanos();
    let mut end = 0;
    let mut maximum = 0;
    for start in 0..points.len() {
        end = end.max(start);
        while end < points.len() && points[end] < points[start].saturating_add(width) {
            end += 1;
        }
        maximum = maximum.max(end - start);
    }
    maximum
}

fn budget_limit(host: &str, method: &str) -> usize {
    let row = BUDGETS.iter().find(|row| {
        row.destination.base_url() == host
            && matches!(row.scope, MethodScope::Named(name) if name == method)
    });
    row.map_or(usize::MAX, |budget| budget.used as usize)
}

fn measure(records: &[SendRecord]) -> Measurements {
    let mut measured = Measurements::default();

    let mut methods: BTreeMap<(&str, &str), Vec<&SendRecord>> = BTreeMap::new();
    let mut days: BTreeMap<(&str, u128), usize> = BTreeMap::new();
    let mut syncs: BTreeMap<u64, usize> = BTreeMap::new();
    let mut calls: BTreeMap<u64, usize> = BTreeMap::new();
    let mut hosts: BTreeMap<&str, Vec<&SendRecord>> = BTreeMap::new();
    for record in records {
        methods
            .entry((&record.host, &record.method))
            .or_default()
            .push(record);
        *days
            .entry((&record.host, nanos(record.at) / DAY.as_nanos()))
            .or_default() += 1;
        *syncs.entry(record.sync).or_default() += 1;
        *calls.entry(record.call).or_default() += 1;
        hosts.entry(&record.host).or_default().push(record);
    }
    for ((host, method), records) in methods {
        let count = max_window(&records, MINUTE);
        if count > measured.max_minute {
            measured.max_minute = count;
            measured.minute_method = method.to_owned();
            measured.minute_ceiling = budget_limit(host, method);
        }
    }
    measured.max_day = days.values().copied().max().unwrap_or(0);
    measured.max_sync = syncs.values().copied().max().unwrap_or(0);
    measured.max_attempts = calls.values().copied().max().unwrap_or(0);

    for mut host_records in hosts.into_values() {
        measured.max_second = measured
            .max_second
            .max(max_window(&host_records, Duration::from_secs(1)));
        host_records.sort_by_key(|record| nanos(record.at));
        let mut refusals = VecDeque::new();
        let mut rate_limits = VecDeque::new();
        for (index, record) in host_records.iter().enumerate() {
            let at = nanos(record.at);
            if let Some(status) = record.status {
                let queue = if status == 429 {
                    &mut rate_limits
                } else if (400..=499).contains(&status) {
                    &mut refusals
                } else {
                    continue;
                };
                while queue
                    .front()
                    .is_some_and(|earlier| at.saturating_sub(*earlier) >= REFUSAL_WINDOW.as_nanos())
                {
                    queue.pop_front();
                }
                queue.push_back(at);
                let closes =
                    (status == 429 && queue.len() >= 2) || (status != 429 && queue.len() >= 3);
                if closes {
                    let after = host_records[index + 1..]
                        .iter()
                        .take_while(|later| nanos(later.at) < at + CLOSURE.as_nanos())
                        .count();
                    measured.max_closure_span = measured.max_closure_span.max(after);
                }
                if status == 429 {
                    let pause = record.pause_for.unwrap_or(MINUTE).max(MINUTE);
                    let after = host_records[index + 1..]
                        .iter()
                        .take_while(|later| nanos(later.at) < at + pause.as_nanos())
                        .count();
                    measured.max_pause_span = measured.max_pause_span.max(after);
                }
            }
        }
    }
    measured
}

fn assert_within_ceilings(mode: &str, measured: &Measurements) {
    assert!(
        measured.max_second <= 1,
        "{mode}: more than one send in one second: {measured:?}"
    );
    assert!(
        measured.max_minute <= measured.minute_ceiling,
        "{mode}: minute budget exceeded: {measured:?}"
    );
    assert!(
        measured.max_day <= DAILY_CEILING,
        "{mode}: daily ceiling exceeded: {measured:?}"
    );
    assert!(
        measured.max_sync <= BROKER_SYNC_REQUEST_CEILING as usize,
        "{mode}: sync ceiling exceeded: {measured:?}"
    );
    assert_eq!(
        measured.max_closure_span, 0,
        "{mode}: send during closure: {measured:?}"
    );
    assert_eq!(
        measured.max_pause_span, 0,
        "{mode}: send during 429 pause: {measured:?}"
    );
    assert!(
        measured.max_attempts <= 3,
        "{mode}: retry ceiling exceeded: {measured:?}"
    );
}

fn planted_record(at: SystemTime, sync: u64, call: u64) -> SendRecord {
    SendRecord {
        at,
        host: Destination::TinkoffProd.base_url().to_owned(),
        method: "OperationsService".to_owned(),
        sync,
        call,
        status: None,
        pause_for: None,
    }
}

#[test]
fn checker_rejects_an_independent_planted_breach_of_every_ceiling() {
    let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);

    let second = vec![
        planted_record(start, 1, 1),
        planted_record(start + Duration::from_millis(999), 2, 2),
    ];
    assert!(measure(&second).max_second > 1, "one-second breach escaped");

    let mut other_host = planted_record(start, 2, 2);
    other_host.host = Destination::TinkoffSandbox.base_url().to_owned();
    assert_eq!(
        measure(&[planted_record(start, 1, 1), other_host]).max_second,
        1,
        "simultaneous departures to different hosts shared one second window"
    );

    let minute: Vec<_> = (0..51)
        .map(|index| {
            planted_record(
                start + Duration::from_millis(index * 1_100),
                index + 1,
                index + 1,
            )
        })
        .collect();
    assert!(measure(&minute).max_minute > 50, "minute breach escaped");

    let day_start = UNIX_EPOCH + Duration::from_secs(20_000 * 86_400);
    let day: Vec<_> = (0..1_001)
        .map(|index| {
            planted_record(
                day_start + Duration::from_secs(index * 61),
                index + 1,
                index + 1,
            )
        })
        .collect();
    assert!(
        measure(&day).max_day > DAILY_CEILING,
        "daily breach escaped"
    );

    let sync: Vec<_> = (0..=BROKER_SYNC_REQUEST_CEILING)
        .map(|index| {
            planted_record(
                start + Duration::from_secs(u64::from(index) * 61),
                1,
                u64::from(index) + 1,
            )
        })
        .collect();
    assert!(
        measure(&sync).max_sync > BROKER_SYNC_REQUEST_CEILING as usize,
        "sync breach escaped"
    );

    let attempts: Vec<_> = (0..4)
        .map(|index| planted_record(start + Duration::from_secs(index * 61), index + 1, 1))
        .collect();
    assert!(
        measure(&attempts).max_attempts > 3,
        "attempt breach escaped"
    );

    let mut closure: Vec<_> = (0..4)
        .map(|index| {
            planted_record(
                start + Duration::from_secs(index * 61),
                index + 1,
                index + 1,
            )
        })
        .collect();
    for record in &mut closure[..3] {
        record.status = Some(400);
    }
    assert!(
        measure(&closure).max_closure_span > 0,
        "post-closure send escaped"
    );

    let mut pause = vec![
        planted_record(start, 1, 1),
        planted_record(start + Duration::from_secs(61), 2, 2),
    ];
    pause[0].status = Some(429);
    pause[0].pause_for = Some(Duration::from_secs(120));
    assert!(measure(&pause).max_pause_span > 0, "post-429 send escaped");
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
    monotonic: Mutex<Instant>,
    wall: Mutex<SystemTime>,
}

impl FakeTime {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            monotonic: Mutex::new(Instant::now()),
            wall: Mutex::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
        })
    }

    fn advance(&self, by: Duration) {
        *self
            .monotonic
            .lock()
            .unwrap_or_else(|error| error.into_inner()) += by;
        *self.wall.lock().unwrap_or_else(|error| error.into_inner()) += by;
    }

    fn wall(&self) -> SystemTime {
        *self.wall.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl Clock for FakeTime {
    fn now(&self) -> Instant {
        *self
            .monotonic
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn now_utc(&self) -> SystemTime {
        self.wall()
    }
}

impl Sleeper for FakeTime {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.advance(delay) })
    }
}

#[derive(Clone, Copy, Debug)]
enum AnswerMode {
    Status(u16, Option<Duration>),
    Timeout,
    FinamRenewal401,
    TinkoffCursor,
    FinamFullPages,
    Success,
}

struct CallContext {
    next_call: AtomicU64,
    active_call: AtomicU64,
    active_sync: AtomicU64,
}

impl CallContext {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            next_call: AtomicU64::new(1),
            active_call: AtomicU64::new(0),
            active_sync: AtomicU64::new(1),
        })
    }
}

struct ScriptedTransport {
    mode: AnswerMode,
    time: Arc<FakeTime>,
    context: Arc<CallContext>,
    records: Arc<Mutex<Vec<SendRecord>>>,
    sequence: AtomicU64,
    full_page: Arc<Vec<u8>>,
}

impl ScriptedTransport {
    fn new(mode: AnswerMode, time: Arc<FakeTime>, context: Arc<CallContext>) -> Self {
        let full_page = format!(
            "{{\"transactions\":[{}]}}",
            std::iter::repeat_n("{}", 1_000)
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes();
        Self {
            mode,
            time,
            context,
            records: Arc::new(Mutex::new(Vec::new())),
            sequence: AtomicU64::new(0),
            full_page: Arc::new(full_page),
        }
    }

    fn records(&self) -> Arc<Mutex<Vec<SendRecord>>> {
        Arc::clone(&self.records)
    }

    fn method(request: &HttpRequest) -> &'static str {
        let url = request.url();
        if url.contains("UsersService/") {
            "UsersService"
        } else if url.contains("OperationsService/") {
            "OperationsService"
        } else if url.ends_with("/v1/sessions") || url.ends_with("/v1/sessions/details") {
            "AuthService.Sessions"
        } else if url.contains("/transactions") {
            "AccountsService.Transactions"
        } else {
            "AccountsService.GetAccount"
        }
    }

    fn success_body(&self, request: &HttpRequest, sequence: u64) -> Vec<u8> {
        let url = request.url();
        if url.ends_with("/v1/sessions") {
            format!("{{\"token\":\"session-{sequence}\"}}").into_bytes()
        } else if url.ends_with("/v1/sessions/details") {
            br#"{"account_ids":["account"]}"#.to_vec()
        } else if url.contains("UsersService/GetAccounts") {
            br#"{"accounts":[{"id":"account"}]}"#.to_vec()
        } else if url.contains("GetOperationsByCursor") {
            br#"{"hasNext":false,"items":[]}"#.to_vec()
        } else if url.contains("/transactions") {
            br#"{"transactions":[]}"#.to_vec()
        } else {
            br#"{}"#.to_vec()
        }
    }
}

impl Transport for ScriptedTransport {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let (answer, pause_for) = match self.mode {
            AnswerMode::Status(status, retry_after) => (
                Ok(HttpResponse {
                    status,
                    body: Vec::new(),
                    retry_after,
                }),
                (status == 429).then_some(retry_after.unwrap_or(MINUTE).max(MINUTE)),
            ),
            AnswerMode::Timeout => (Err(HttpError::Timeout), None),
            AnswerMode::FinamRenewal401 => {
                if request.url().ends_with("/v1/sessions") {
                    (
                        Ok(HttpResponse {
                            status: 200,
                            body: self.success_body(request, sequence),
                            retry_after: None,
                        }),
                        None,
                    )
                } else {
                    (
                        Ok(HttpResponse {
                            status: 401,
                            body: Vec::new(),
                            retry_after: None,
                        }),
                        None,
                    )
                }
            }
            AnswerMode::TinkoffCursor if request.url().contains("GetOperationsByCursor") => (
                Ok(HttpResponse {
                    status: 200,
                    body: format!(
                        "{{\"hasNext\":true,\"nextCursor\":\"cursor-{sequence}\",\"items\":[]}}"
                    )
                    .into_bytes(),
                    retry_after: None,
                }),
                None,
            ),
            AnswerMode::FinamFullPages if request.url().contains("/transactions") => (
                Ok(HttpResponse {
                    status: 200,
                    body: self.full_page.as_ref().clone(),
                    retry_after: None,
                }),
                None,
            ),
            AnswerMode::TinkoffCursor | AnswerMode::FinamFullPages | AnswerMode::Success => (
                Ok(HttpResponse {
                    status: 200,
                    body: self.success_body(request, sequence),
                    retry_after: None,
                }),
                None,
            ),
        };
        let status = answer.as_ref().ok().map(|response| response.status);
        self.records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(SendRecord {
                at: self.time.wall(),
                host: request.destination().base_url().to_owned(),
                method: Self::method(request).to_owned(),
                sync: self.context.active_sync.load(Ordering::SeqCst),
                call: self.context.active_call.load(Ordering::SeqCst),
                status,
                pause_for,
            });
        answer
    }
}

struct ObservedOutbound {
    gateway: Arc<Gateway<ScriptedTransport>>,
    context: Arc<CallContext>,
}

impl Outbound for ObservedOutbound {
    fn send<'a>(
        &'a self,
        method: &'static str,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>> {
        let call = self.context.next_call.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.context.active_call.store(call, Ordering::SeqCst);
            let result = self.gateway.send(method, request, deadline).await;
            self.context.active_call.store(0, Ordering::SeqCst);
            result
        })
    }
}

struct Harness {
    outbound: Arc<dyn Outbound>,
    time: Arc<FakeTime>,
    context: Arc<CallContext>,
    records: Arc<Mutex<Vec<SendRecord>>>,
}

fn harness(mode: AnswerMode, tally: &Path, egress: bool) -> Harness {
    if egress {
        std::fs::write(tally, "")
            .unwrap_or_else(|error| panic!("create {}: {error}", tally.display()));
    }
    let time = FakeTime::new();
    let context = CallContext::new();
    let transport = ScriptedTransport::new(mode, Arc::clone(&time), Arc::clone(&context));
    let records = transport.records();
    let gateway = Gateway::with_parts(
        transport,
        BUDGETS,
        Arc::clone(&time) as Arc<dyn Clock>,
        Arc::clone(&time) as Arc<dyn Sleeper>,
        if egress {
            BrokerEgress::On {
                tally: tally.to_owned(),
            }
        } else {
            BrokerEgress::Off
        },
    )
    .unwrap_or_else(|error| panic!("gateway: {error}"));
    let outbound = Arc::new(ObservedOutbound {
        gateway: Arc::new(gateway),
        context: Arc::clone(&context),
    });
    Harness {
        outbound,
        time,
        context,
        records,
    }
}

fn shared_outbound(
    time: &Arc<FakeTime>,
    context: &Arc<CallContext>,
    tally: &Path,
) -> (Arc<dyn Outbound>, Arc<Mutex<Vec<SendRecord>>>) {
    let transport =
        ScriptedTransport::new(AnswerMode::Success, Arc::clone(time), Arc::clone(context));
    let records = transport.records();
    let gateway = Gateway::with_parts(
        transport,
        BUDGETS,
        Arc::clone(time) as Arc<dyn Clock>,
        Arc::clone(time) as Arc<dyn Sleeper>,
        BrokerEgress::On {
            tally: tally.to_owned(),
        },
    )
    .unwrap_or_else(|error| panic!("shared gateway: {error}"));
    (
        Arc::new(ObservedOutbound {
            gateway: Arc::new(gateway),
            context: Arc::clone(context),
        }),
        records,
    )
}

fn token() -> BrokerToken {
    let key = Key::from_bytes([7; 32]);
    open(&key, &seal(&key, "invented-proof-token"))
        .unwrap_or_else(|error| panic!("invented token: {error}"))
}

async fn run_tinkoff(harness: &Harness, environment: Environment, repetitions: usize) {
    let client = TinkoffClient::new(environment, token(), Arc::clone(&harness.outbound));
    for sync in 1..=repetitions as u64 {
        harness.context.active_sync.store(sync, Ordering::SeqCst);
        let _ = client
            .get_accounts(&RequestAllowance::new(BROKER_SYNC_REQUEST_CEILING))
            .await;
        harness.time.advance(Duration::from_secs(30 * 60));
    }
}

async fn run_finam(harness: &Harness, repetitions: usize) {
    let client = FinamClient::new(token(), Arc::clone(&harness.outbound));
    for sync in 1..=repetitions as u64 {
        harness.context.active_sync.store(sync, Ordering::SeqCst);
        let _ = client
            .get_account_ids(&RequestAllowance::new(BROKER_SYNC_REQUEST_CEILING))
            .await;
        harness.time.advance(Duration::from_secs(30 * 60));
    }
}

fn snapshot(records: &Arc<Mutex<Vec<SendRecord>>>) -> Vec<SendRecord> {
    records
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn print_header() {
    println!(
        "mode                     host                                             sec   minute(method)                         day       sync    closure pause attempts"
    );
    println!(
        "                                                                                           each cell is reached/ceiling"
    );
}

fn print_row(mode: &str, host: &str, measured: &Measurements) {
    let method = if measured.minute_method.is_empty() {
        "-"
    } else {
        &measured.minute_method
    };
    let minute = format!(
        "{method}={}/{}",
        measured.max_minute, measured.minute_ceiling
    );
    println!(
        "{mode:<24} {host:<48} {:>3}/1 {minute:<38} {:>4}/1000 {:>3}/300 {:>2}/0 {:>2}/0 {:>2}/3",
        measured.max_second,
        measured.max_day,
        measured.max_sync,
        measured.max_closure_span,
        measured.max_pause_span,
        measured.max_attempts,
    );
}

#[tokio::test]
async fn simulated_failure_modes_stay_under_every_ceiling() {
    print_header();
    let modes = [
        ("always-400", AnswerMode::Status(400, None)),
        (
            "429-retry-after",
            AnswerMode::Status(429, Some(Duration::from_secs(120))),
        ),
        ("429-no-header", AnswerMode::Status(429, None)),
        ("always-500", AnswerMode::Status(500, None)),
        ("transport-timeout", AnswerMode::Timeout),
    ];
    for (mode_name, mode) in modes {
        for (host_name, environment) in [
            (Destination::TinkoffProd.base_url(), Environment::Prod),
            (Destination::TinkoffSandbox.base_url(), Environment::Sandbox),
        ] {
            let directory = TempDir::new(mode_name);
            let proof = harness(mode, &directory.path("tally"), true);
            run_tinkoff(&proof, environment, 48).await;
            let measured = measure(&snapshot(&proof.records));
            assert_within_ceilings(mode_name, &measured);
            print_row(mode_name, host_name, &measured);
        }
        let directory = TempDir::new(mode_name);
        let proof = harness(mode, &directory.path("tally"), true);
        run_finam(&proof, 48).await;
        let measured = measure(&snapshot(&proof.records));
        assert_within_ceilings(mode_name, &measured);
        print_row(mode_name, Destination::FinamApi.base_url(), &measured);
    }

    let directory = TempDir::new("finam-401");
    let proof = harness(AnswerMode::FinamRenewal401, &directory.path("tally"), true);
    run_finam(&proof, 48).await;
    let measured = measure(&snapshot(&proof.records));
    assert_within_ceilings("finam-renewal-401", &measured);
    print_row(
        "finam-renewal-401",
        Destination::FinamApi.base_url(),
        &measured,
    );
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

#[tokio::test]
async fn endless_full_pages_stop_inside_the_sync_ceiling() {
    print_header();
    for (host, environment) in [
        (Destination::TinkoffProd.base_url(), Environment::Prod),
        (Destination::TinkoffSandbox.base_url(), Environment::Sandbox),
    ] {
        let directory = TempDir::new("cursor");
        let proof = harness(AnswerMode::TinkoffCursor, &directory.path("tally"), true);
        let client = TinkoffClient::new(environment, token(), Arc::clone(&proof.outbound));
        let channel = TinkoffChannel::new(client, SourceId::new_random(), dictionary("tinkoff"));
        let context = BrokerRequestContext {
            deadline: None,
            allowance: &RequestAllowance::new(BROKER_SYNC_REQUEST_CEILING),
        };
        let _ = channel
            .fetch_operations(
                AccountId::new_random(),
                "account",
                date!(2026 - 01 - 01),
                date!(2026 - 12 - 31),
                context,
            )
            .await;
        let measured = measure(&snapshot(&proof.records));
        assert_within_ceilings("tinkoff-endless-cursor", &measured);
        assert_eq!(measured.max_sync, BROKER_SYNC_REQUEST_CEILING as usize);
        print_row("endless-cursor", host, &measured);
    }

    let directory = TempDir::new("finam-pages");
    let proof = harness(AnswerMode::FinamFullPages, &directory.path("tally"), true);
    let client = FinamClient::new(token(), Arc::clone(&proof.outbound));
    let _ = client
        .get_transactions(
            "account",
            date!(2026 - 01 - 01),
            date!(2026 - 12 - 31),
            &RequestAllowance::new(BROKER_SYNC_REQUEST_CEILING),
        )
        .await;
    let measured = measure(&snapshot(&proof.records));
    assert_within_ceilings("finam-endless-pages", &measured);
    print_row(
        "endless-full-pages",
        Destination::FinamApi.base_url(),
        &measured,
    );
}

#[tokio::test]
async fn two_gateways_and_a_rebuild_share_one_tally() {
    let directory = TempDir::new("gateways");
    let tally = directory.path("tally");
    let first = harness(AnswerMode::Success, &tally, true);
    let (second, second_records) = shared_outbound(&first.time, &first.context, &tally);
    let allowance = RequestAllowance::new(10);
    let request = HttpRequest::post(
        Destination::TinkoffProd,
        "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
        RequestBody::Json("{}".to_owned()),
    )
    .idempotent()
    .with_request_allowance(allowance);
    first
        .outbound
        .send("OperationsService", &request, None)
        .await
        .unwrap_or_else(|error| panic!("first gateway: {error}"));
    second
        .send("OperationsService", &request, None)
        .await
        .unwrap_or_else(|error| panic!("second gateway: {error}"));
    let (rebuilt, rebuilt_records) = shared_outbound(&first.time, &first.context, &tally);
    rebuilt
        .send("OperationsService", &request, None)
        .await
        .unwrap_or_else(|error| panic!("rebuilt gateway send: {error}"));

    let mut records = snapshot(&first.records);
    records.extend(snapshot(&second_records));
    records.extend(snapshot(&rebuilt_records));
    assert_eq!(records.len(), 3);
    let measured = measure(&records);
    assert_within_ceilings("shared-tally-rebuild", &measured);
    print_row(
        "shared-tally-rebuild",
        Destination::TinkoffProd.base_url(),
        &measured,
    );
}

#[tokio::test]
async fn egress_off_sends_nothing() {
    let directory = TempDir::new("egress-off");
    let proof = harness(AnswerMode::Success, &directory.path("unused"), false);
    run_tinkoff(&proof, Environment::Prod, 1).await;
    let records = snapshot(&proof.records);
    assert!(
        records.is_empty(),
        "egress-off requests reached the double: {records:?}"
    );
    let measured = measure(&records);
    assert_within_ceilings("egress-off", &measured);
    print_row("egress-off", Destination::TinkoffProd.base_url(), &measured);
}

struct FixedAppClock;

impl AppClock for FixedAppClock {
    fn today(&self) -> Date {
        date!(2026 - 01 - 01)
    }
}

struct CeilingChannel {
    outbound: Arc<dyn Outbound>,
}

#[async_trait]
impl BrokerChannel for CeilingChannel {
    async fn fetch_account_numbers(
        &self,
        _context: BrokerRequestContext<'_>,
    ) -> Result<Vec<String>, BrokerError> {
        Ok(vec!["broker-account".to_owned()])
    }

    async fn fetch_operations(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _from: Date,
        _to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, BrokerError> {
        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
            RequestBody::Json("{}".to_owned()),
        )
        .idempotent()
        .with_request_allowance(context.allowance.clone());
        // This deliberately omits the per-call deadline: this channel isolates
        // the one allowance minted by the real sync. The broker-client
        // scenarios above exercise the real adapters' request paths.
        loop {
            match self
                .outbound
                .send("OperationsService", &request, None)
                .await
            {
                Ok(_) => {}
                Err(GatewayError::RequestCeiling { ceiling, .. }) => {
                    return Err(BrokerError::RequestCeiling {
                        broker: "tinkoff".to_owned(),
                        ceiling,
                    });
                }
                Err(error) => {
                    return Err(BrokerError::Unreachable {
                        broker: "tinkoff".to_owned(),
                        detail: error.to_string(),
                        retry_after: error.retry_after(),
                    });
                }
            }
        }
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _at: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Requested,
            claims: Vec::new(),
            refused: Vec::new(),
        })
    }

    fn channel(&self) -> SourceChannel {
        SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("ceiling-proof".to_owned()),
            document: None,
        }
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Account
    }
}

#[tokio::test]
async fn the_real_sync_mints_exactly_one_three_hundred_attempt_allowance() {
    let directory = TempDir::new("real-sync");
    let proof = harness(AnswerMode::Success, &directory.path("tally"), true);
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
        .unwrap_or_else(|error| panic!("seed account: {error}"));
    let principal = Principal {
        token_id: uuid::Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    };
    let result = iaam_app::sync::sync_broker(
        &services,
        &principal,
        &CeilingChannel {
            outbound: Arc::clone(&proof.outbound),
        },
        BrokerSyncRequest {
            broker_code: iaam_store::documents::BrokerCode::parse("tinkoff")
                .unwrap_or_else(|| panic!("broker code")),
            account,
            from: date!(2026 - 01 - 01),
            to: date!(2026 - 01 - 31),
        },
    )
    .await;
    assert!(
        matches!(result, Err(iaam_app::error::AppError::SyncRequestCeiling { ceiling, .. }) if ceiling == BROKER_SYNC_REQUEST_CEILING),
        "unexpected sync result: {result:?}"
    );
    let measured = measure(&snapshot(&proof.records));
    assert_within_ceilings("real-sync", &measured);
    assert_eq!(measured.max_sync, BROKER_SYNC_REQUEST_CEILING as usize);
    print_row("real-sync", Destination::TinkoffProd.base_url(), &measured);
}

#[derive(Clone)]
struct ProcessTransport {
    record: PathBuf,
}

impl Transport for ProcessTransport {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        use std::io::Write;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.record)
            .unwrap_or_else(|error| panic!("open {}: {error}", self.record.display()));
        writeln!(file, "{}\t{}", request.destination().base_url(), now)
            .unwrap_or_else(|error| panic!("write {}: {error}", self.record.display()));
        Ok(HttpResponse {
            status: 200,
            body: Vec::new(),
            retry_after: None,
        })
    }
}

#[tokio::test]
async fn two_process_child() {
    let Ok(tally) = std::env::var("IAAM_CEILING_CHILD_TALLY") else {
        return;
    };
    let record = PathBuf::from(
        std::env::var("IAAM_CEILING_CHILD_RECORD")
            .unwrap_or_else(|error| panic!("child record path: {error}")),
    );
    let gateway = Gateway::new(
        ProcessTransport { record },
        BrokerEgress::On {
            tally: PathBuf::from(tally),
        },
    )
    .unwrap_or_else(|error| panic!("child gateway: {error}"));
    let allowance = RequestAllowance::new(3);
    let request = HttpRequest::post(
        Destination::TinkoffProd,
        "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
        RequestBody::Json("{}".to_owned()),
    )
    .idempotent()
    .with_request_allowance(allowance);
    for _ in 0..3 {
        gateway
            .send("OperationsService", &request, None)
            .await
            .unwrap_or_else(|error| panic!("child send: {error}"));
    }
}

#[test]
fn two_real_processes_share_spacing_and_budget() {
    let directory = TempDir::new("processes");
    let tally = directory.path("tally");
    std::fs::write(&tally, "").unwrap_or_else(|error| panic!("create tally: {error}"));
    let executable = std::env::current_exe().unwrap_or_else(|error| panic!("test binary: {error}"));
    let mut children = Vec::new();
    for number in 0..2 {
        let record = directory.path(&format!("child-{number}"));
        let child = Command::new(&executable)
            .args(["--exact", "two_process_child", "--nocapture"])
            .env("IAAM_CEILING_CHILD_TALLY", &tally)
            .env("IAAM_CEILING_CHILD_RECORD", &record)
            .spawn()
            .unwrap_or_else(|error| panic!("spawn child {number}: {error}"));
        children.push((child, record));
    }
    let mut records = Vec::new();
    for (mut child, record) in children {
        let status = child
            .wait()
            .unwrap_or_else(|error| panic!("wait child: {error}"));
        assert!(status.success(), "child failed: {status}");
        let text = std::fs::read_to_string(&record)
            .unwrap_or_else(|error| panic!("read {}: {error}", record.display()));
        for line in text.lines() {
            let (host, at) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("child row: {line}"));
            let at = at
                .parse::<u128>()
                .unwrap_or_else(|error| panic!("child time: {error}"));
            let seconds = u64::try_from(at / 1_000_000_000).unwrap_or(u64::MAX);
            let sub = u32::try_from(at % 1_000_000_000).unwrap_or(u32::MAX);
            records.push(SendRecord {
                at: UNIX_EPOCH + Duration::new(seconds, sub),
                host: host.to_owned(),
                method: "OperationsService".to_owned(),
                sync: 1,
                call: records.len() as u64 + 1,
                status: Some(200),
                pause_for: None,
            });
        }
    }
    records.sort_by_key(|record| nanos(record.at));
    let minimum_gap = records
        .windows(2)
        .map(|pair| {
            pair[1]
                .at
                .duration_since(pair[0].at)
                .unwrap_or(Duration::ZERO)
        })
        .min()
        .unwrap_or(Duration::ZERO);
    assert!(
        minimum_gap >= Duration::from_secs(1),
        "two-processes: recording doubles observed a minimum broker-send gap of {minimum_gap:?}, below the 1s ceiling"
    );
    let measured = measure(&records);
    assert_within_ceilings("two-processes", &measured);
    assert_eq!(records.len(), 6);
    print_row(
        "two-processes",
        Destination::TinkoffProd.base_url(),
        &measured,
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
