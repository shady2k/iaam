use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iaam_http::gateway::{BUDGETS, BootTime, Clock, Sleeper, Transport};
use iaam_http::{
    BrokerEgress, Destination, Gateway, GatewayError, HttpError, HttpRequest, HttpResponse,
    RequestAllowance, RequestBody,
};
use tokio::sync::Notify;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn create(label: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "iaam-outbound-tally-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("temporary directory created");
        Self { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct FakeTime {
    monotonic: Mutex<Instant>,
    boot: Mutex<Duration>,
    boot_id: Mutex<String>,
    wall: Mutex<SystemTime>,
    slept: Mutex<Vec<Duration>>,
}

impl FakeTime {
    fn at(wall: SystemTime) -> Arc<Self> {
        Arc::new(Self {
            monotonic: Mutex::new(Instant::now()),
            boot: Mutex::new(Duration::from_secs(1_800_000_000)),
            boot_id: Mutex::new("boot-a".to_owned()),
            wall: Mutex::new(wall),
            slept: Mutex::new(Vec::new()),
        })
    }

    fn advance(&self, by: Duration) {
        *self.monotonic.lock().expect("monotonic clock") += by;
        *self.boot.lock().expect("boot clock") += by;
        *self.wall.lock().expect("wall clock") += by;
    }

    fn step_wall(&self, by: Duration) {
        *self.wall.lock().expect("wall clock") += by;
    }

    fn reboot(&self, boot_id: &str) {
        *self.boot_id.lock().expect("boot id") = boot_id.to_owned();
        *self.boot.lock().expect("boot clock") = Duration::ZERO;
    }

    fn boot(&self) -> Duration {
        *self.boot.lock().expect("boot clock")
    }

    fn wall(&self) -> SystemTime {
        *self.wall.lock().expect("wall clock")
    }
}

impl Clock for FakeTime {
    fn now(&self) -> Instant {
        *self.monotonic.lock().expect("monotonic clock")
    }

    fn now_boot(&self) -> Result<BootTime, String> {
        Ok(BootTime::new(
            self.boot_id.lock().expect("boot id").clone(),
            self.boot(),
        ))
    }
}

impl Sleeper for FakeTime {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.advance(delay);
            self.slept.lock().expect("sleeps").push(delay);
        })
    }
}

#[derive(Clone)]
struct RecordingTransport {
    time: Arc<FakeTime>,
    sent: Arc<Mutex<Vec<(Destination, SystemTime)>>>,
    answers: Arc<Mutex<VecDeque<Result<HttpResponse, HttpError>>>>,
    default_status: u16,
}

impl RecordingTransport {
    fn answering(time: &Arc<FakeTime>) -> Self {
        Self::answering_status(time, 200)
    }

    fn answering_status(time: &Arc<FakeTime>, status: u16) -> Self {
        Self {
            time: Arc::clone(time),
            sent: Arc::new(Mutex::new(Vec::new())),
            answers: Arc::new(Mutex::new(VecDeque::new())),
            default_status: status,
        }
    }

    fn sent(&self) -> Vec<(Destination, SystemTime)> {
        self.sent.lock().expect("sent requests").clone()
    }
}

impl Transport for RecordingTransport {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.sent
            .lock()
            .expect("sent requests")
            .push((request.destination(), self.time.wall()));
        self.answers
            .lock()
            .expect("answers")
            .pop_front()
            .unwrap_or_else(|| Ok(status(self.default_status)))
    }
}

#[derive(Clone)]
struct StatusThenBodyError {
    sends: Arc<AtomicUsize>,
}

impl Transport for StatusThenBodyError {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Err(HttpError::Timeout)
    }

    fn send_observed<'a>(
        &'a self,
        _request: &'a HttpRequest,
        observe: Box<dyn FnOnce(u16, Option<Duration>) + Send + 'a>,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        self.sends.fetch_add(1, Ordering::SeqCst);
        observe(429, Some(Duration::MAX));
        async { Err(HttpError::Timeout) }
    }
}

#[derive(Clone)]
struct HoldingTransport {
    sent: Arc<AtomicUsize>,
    first_arrived: Arc<Notify>,
    release_first: Arc<Notify>,
}

impl Transport for HoldingTransport {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let index = self.sent.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            self.first_arrived.notify_one();
            self.release_first.notified().await;
            return Ok(status(429));
        }
        Ok(status(200))
    }
}

fn status(status: u16) -> HttpResponse {
    HttpResponse {
        status,
        body: Vec::new(),
        retry_after: None,
    }
}

fn try_gateway<T: Transport>(
    transport: T,
    time: &Arc<FakeTime>,
    tally: &Path,
) -> Result<Gateway<T>, GatewayError> {
    Gateway::with_parts(
        transport,
        BUDGETS,
        Arc::clone(time) as Arc<dyn Clock>,
        Arc::clone(time) as Arc<dyn Sleeper>,
        BrokerEgress::On {
            tally: tally.to_owned(),
        },
    )
}

fn gateway<T: Transport>(transport: T, time: &Arc<FakeTime>, tally: &Path) -> Gateway<T> {
    try_gateway(transport, time, tally).expect("documented budgets and tally are valid")
}

fn empty_tally(directory: &TempDir) -> PathBuf {
    let tally = directory.file("tally");
    std::fs::write(&tally, "").expect("empty tally created");
    tally
}

fn operations() -> HttpRequest {
    HttpRequest::post(
        Destination::TinkoffProd,
        "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
        RequestBody::Json("{}".to_owned()),
    )
    .idempotent()
    .with_request_allowance(RequestAllowance::new(u32::MAX))
}

fn finam_session() -> HttpRequest {
    HttpRequest::get(Destination::FinamApi, "/v1/sessions")
        .with_request_allowance(RequestAllowance::new(u32::MAX))
}

fn elapsed(later: SystemTime, earlier: SystemTime) -> Duration {
    later.duration_since(earlier).expect("time moves forward")
}

fn assert_never_more_than(limit: usize, window: Duration, sent: &[SystemTime]) {
    for (index, start) in sent.iter().enumerate() {
        if let Some(later) = sent.get(index + limit) {
            assert!(
                elapsed(*later, *start) >= window,
                "request {} started {:?} after request {index}: more than {limit} in {window:?}",
                index + limit,
                elapsed(*later, *start)
            );
        }
    }
}

#[tokio::test]
async fn first_send_waits_sixty_seconds_then_spacing_and_minute_budget_hold() {
    let directory = TempDir::create("windows");
    let tally = empty_tally(&directory);
    let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let time = FakeTime::at(start);
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    for _ in 0..52 {
        gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect("request within the budget sent");
    }

    let sent: Vec<_> = transport.sent().into_iter().map(|(_, at)| at).collect();
    assert_eq!(elapsed(sent[0], start), Duration::from_secs(60));
    for pair in sent.windows(2) {
        assert!(elapsed(pair[1], pair[0]) >= Duration::from_secs(1));
    }
    assert_never_more_than(50, Duration::from_secs(60), &sent);
}

#[tokio::test]
async fn the_next_decision_sees_the_previous_status_before_it_can_send() {
    let directory = TempDir::create("status-order");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = HoldingTransport {
        sent: Arc::new(AtomicUsize::new(0)),
        first_arrived: Arc::new(Notify::new()),
        release_first: Arc::new(Notify::new()),
    };
    let gateway = Arc::new(gateway(transport.clone(), &time, &tally));

    let first_gateway = Arc::clone(&gateway);
    let first = tokio::spawn(async move {
        first_gateway
            .send("OperationsService", &operations(), None)
            .await
    });
    transport.first_arrived.notified().await;
    let second_gateway = Arc::clone(&gateway);
    let second = tokio::spawn(async move {
        second_gateway
            .send("OperationsService", &operations(), None)
            .await
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    transport.release_first.notify_one();

    let first = first
        .await
        .expect("first task joined")
        .expect_err("429 pauses");
    let second = second
        .await
        .expect("second task joined")
        .expect_err("the recorded 429 blocks the next decision");
    assert!(matches!(
        first,
        GatewayError::BrokerHostPaused { attempts: 1, .. }
    ));
    assert!(matches!(
        second,
        GatewayError::BrokerHostPaused { attempts: 0, .. }
    ));
    assert_eq!(transport.sent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_boot_id_change_restarts_an_active_closure_in_full() {
    let directory = TempDir::create("reboot-closure");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering_status(&time, 400);
    let gateway = gateway(transport.clone(), &time, &tally);

    for _ in 0..3 {
        let refused = gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect_err("broker refusal returned");
        assert!(matches!(
            refused,
            GatewayError::Rejected { status: 400, .. }
        ));
    }
    time.advance(Duration::from_secs(10 * 60));
    time.reboot("boot-b");

    let blocked = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("closure restarts after reboot");
    assert!(matches!(
        blocked,
        GatewayError::BrokerHostClosed {
            retry_after,
            attempts: 0,
            ..
        } if retry_after == Duration::from_secs(30 * 60)
    ));
    assert_eq!(transport.sent().len(), 3);
}

#[tokio::test]
async fn a_wall_clock_step_does_not_expire_a_boot_clock_pause() {
    let directory = TempDir::create("wall-step");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering_status(&time, 429);
    let gateway = gateway(transport.clone(), &time, &tally);

    let first = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("429 pauses");
    assert!(matches!(
        first,
        GatewayError::BrokerHostPaused { attempts: 1, .. }
    ));
    time.step_wall(Duration::from_secs(7 * 24 * 60 * 60));

    let blocked = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("wall time does not release the pause");
    assert!(matches!(
        blocked,
        GatewayError::BrokerHostPaused {
            retry_after,
            attempts: 0,
            ..
        } if retry_after == Duration::from_secs(60)
    ));
    assert_eq!(transport.sent().len(), 1);
}

#[tokio::test]
async fn retry_after_is_clamped_to_one_day_in_the_tally() {
    let directory = TempDir::create("retry-after-clamp");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    transport
        .answers
        .lock()
        .expect("answers")
        .push_back(Ok(HttpResponse {
            status: 429,
            body: Vec::new(),
            retry_after: Some(Duration::MAX),
        }));
    let gateway = gateway(transport, &time, &tally);

    let refused = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("429 pauses");
    assert!(matches!(
        refused,
        GatewayError::BrokerHostPaused { retry_after, .. }
            if retry_after == Duration::from_secs(24 * 60 * 60)
    ));
}

#[tokio::test]
async fn a_429_status_is_persisted_when_consuming_its_body_fails() {
    let directory = TempDir::create("status-before-body");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = StatusThenBodyError {
        sends: Arc::new(AtomicUsize::new(0)),
    };
    let gateway = gateway(transport.clone(), &time, &tally);

    let refused = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("the observed 429 pauses even though its body failed");

    assert!(matches!(
        refused,
        GatewayError::BrokerHostPaused {
            retry_after,
            attempts: 1,
            ..
        } if retry_after == Duration::from_secs(24 * 60 * 60)
    ));
    assert_eq!(transport.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_429_pause_and_second_429_closure_are_persisted() {
    let directory = TempDir::create("rate-limit");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering_status(&time, 429);
    let gateway = gateway(transport.clone(), &time, &tally);

    let first = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("first 429 pauses");
    assert!(matches!(
        first,
        GatewayError::BrokerHostPaused { retry_after, .. }
            if retry_after == Duration::from_secs(60)
    ));
    time.advance(Duration::from_secs(60));
    let second = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("second 429 closes");
    assert!(matches!(
        second,
        GatewayError::BrokerHostPaused { retry_after, .. }
            if retry_after == Duration::from_secs(30 * 60)
    ));
    let closed = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("closure blocks");
    assert!(matches!(
        closed,
        GatewayError::BrokerHostClosed { attempts: 0, .. }
    ));
    assert_eq!(transport.sent().len(), 2);
}

#[tokio::test]
async fn three_finam_refusals_close_only_the_finam_endpoint() {
    let directory = TempDir::create("finam-refusals");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering_status(&time, 401);
    let gateway = gateway(transport.clone(), &time, &tally);

    for _ in 0..3 {
        let refused = gateway
            .send("AuthService.Sessions", &finam_session(), None)
            .await
            .expect_err("Finam refusal returned");
        assert!(matches!(
            refused,
            GatewayError::Rejected { status: 401, .. }
        ));
    }
    let closed = gateway
        .send("AuthService.Sessions", &finam_session(), None)
        .await
        .expect_err("Finam closure blocks");
    assert!(matches!(
        closed,
        GatewayError::BrokerHostClosed { attempts: 0, .. }
    ));

    let tinkoff = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("transport still answers 401 for T-Invest");
    assert!(matches!(
        tinkoff,
        GatewayError::Rejected { status: 401, .. }
    ));
}

#[tokio::test]
async fn a_leftover_temporary_file_never_replaces_the_complete_tally() {
    let directory = TempDir::create("interrupted-write");
    let tally = empty_tally(&directory);
    let temporary = directory.file("tally.tmp");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect("initial state persisted");
    std::fs::write(&temporary, "partial new state").expect("leftover temporary written");
    gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect("complete tally remains authoritative");

    assert_eq!(transport.sent().len(), 2);
}

#[tokio::test]
async fn the_thousand_and_first_send_in_any_rolling_day_is_refused() {
    let directory = TempDir::create("rolling-day");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    for _ in 0..1_000 {
        gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect("request within rolling ceiling sent");
    }
    let oldest = Duration::from_secs(1_800_000_000 + 60);
    let expected_wait = oldest + Duration::from_secs(24 * 60 * 60) - time.boot();
    let refused = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("rolling ceiling refuses request 1001");
    assert!(matches!(
        refused,
        GatewayError::DailyCeiling {
            ceiling: 1_000,
            retry_after,
            ..
        } if retry_after == expected_wait
    ));
    assert_eq!(transport.sent().len(), 1_000);

    time.advance(expected_wait);
    gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect("oldest request aged out of the rolling day");
    assert_eq!(transport.sent().len(), 1_001);
}

#[tokio::test]
async fn invalid_path_spellings_and_aliases_are_refused_naming_the_path() {
    let directory = TempDir::create("paths");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));

    let relative = PathBuf::from("relative-tally");
    let noncanonical = directory.path.join(".").join("tally");
    let missing = directory.file("missing");
    let mut invalid = vec![relative, noncanonical, missing];

    #[cfg(unix)]
    {
        let symlink = directory.file("tally-symlink");
        std::os::unix::fs::symlink(&tally, &symlink).expect("symlink created");
        invalid.push(symlink);
        let hardlink = directory.file("tally-hardlink");
        std::fs::hard_link(&tally, &hardlink).expect("hard link created");
        invalid.push(hardlink);
    }

    for path in invalid {
        let Err(error) = try_gateway(RecordingTransport::answering(&time), &time, &path) else {
            panic!("path alias was accepted: {}", path.display());
        };
        assert!(
            error.to_string().contains(&path.display().to_string()),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a_corrupt_tally_is_refused_without_transport_and_names_the_path() {
    let directory = TempDir::create("corrupt");
    let tally = directory.file("tally");
    std::fs::write(&tally, "not a tally\n").expect("corrupt tally written");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    let refused = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("corrupt tally refuses");
    assert!(matches!(refused, GatewayError::TallyCorrupt { .. }));
    assert!(refused.to_string().contains(&tally.display().to_string()));
    assert!(transport.sent().is_empty());
}

#[test]
fn endpoint_owner_child() {
    let Some(tally) = std::env::var_os("IAAM_ENDPOINT_OWNER_CHILD_TALLY") else {
        return;
    };
    let tally = PathBuf::from(tally);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        let transport = RecordingTransport::answering(&time);
        let gateway = gateway(transport.clone(), &time, &tally);

        let owned = gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect_err("parent owns T-Invest production");
        let message = owned.to_string();
        assert!(matches!(
            owned,
            GatewayError::BrokerEndpointOwned {
                destination: Destination::TinkoffProd,
                ..
            }
        ));
        assert!(
            message.contains(Destination::TinkoffProd.base_url()),
            "{message}"
        );

        gateway
            .send("AuthService.Sessions", &finam_session(), None)
            .await
            .expect("Finam remains available to the child process");
        assert_eq!(
            transport
                .sent()
                .into_iter()
                .map(|(destination, _)| destination)
                .collect::<Vec<_>>(),
            [Destination::FinamApi]
        );
    });
}

#[tokio::test]
async fn another_process_is_refused_for_owned_endpoint_but_can_use_another() {
    let directory = TempDir::create("process-owner");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport, &time, &tally);
    gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect("parent acquires T-Invest production");

    let output = Command::new(std::env::current_exe().expect("test executable"))
        .arg("--exact")
        .arg("endpoint_owner_child")
        .arg("--nocapture")
        .env("IAAM_ENDPOINT_OWNER_CHILD_TALLY", &tally)
        .output()
        .expect("child process ran");
    assert!(
        output.status.success(),
        "child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn egress_switch_applies_only_to_broker_destinations() {
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let off = Gateway::with_parts(
        transport.clone(),
        BUDGETS,
        Arc::clone(&time) as Arc<dyn Clock>,
        Arc::clone(&time) as Arc<dyn Sleeper>,
        BrokerEgress::Off,
    )
    .expect("budgets valid");

    let refused = off
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("broker egress is off");
    assert!(matches!(refused, GatewayError::BrokerEgressOff));
    off.send(
        "market",
        &HttpRequest::get(Destination::MoexIss, "/iss/history.json"),
        None,
    )
    .await
    .expect("MOEX is not a broker endpoint");
    assert_eq!(
        transport
            .sent()
            .into_iter()
            .map(|(destination, _)| destination)
            .collect::<Vec<_>>(),
        [Destination::MoexIss]
    );
}
