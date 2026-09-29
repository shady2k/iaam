use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

    async fn send_observed<'a>(
        &'a self,
        request: &'a HttpRequest,
        handoff: Box<dyn FnOnce() -> Result<(), HttpError> + Send + 'a>,
        observe: Box<dyn FnOnce(u16, Option<Duration>) + Send + 'a>,
    ) -> Result<HttpResponse, HttpError> {
        let answer = self
            .answers
            .lock()
            .expect("answers")
            .pop_front()
            .unwrap_or_else(|| Ok(status(self.default_status)));
        if matches!(&answer, Err(HttpError::RequestNotBuilt(_))) {
            return answer;
        }
        handoff()?;
        self.sent
            .lock()
            .expect("sent requests")
            .push((request.destination(), self.time.wall()));
        if let Ok(response) = &answer {
            observe(response.status, response.retry_after);
        }
        answer
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
        handoff: Box<dyn FnOnce() -> Result<(), HttpError> + Send + 'a>,
        observe: Box<dyn FnOnce(u16, Option<Duration>) + Send + 'a>,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        self.sends.fetch_add(1, Ordering::SeqCst);
        async move {
            handoff()?;
            observe(429, Some(Duration::MAX));
            Err(HttpError::Timeout)
        }
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

#[derive(Clone)]
struct DelayedPanickingTransport {
    sent: Arc<AtomicUsize>,
    time: Arc<FakeTime>,
    delay: Duration,
}

impl Transport for DelayedPanickingTransport {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        panic!("send_observed is the required path");
    }

    async fn send_observed<'a>(
        &'a self,
        _request: &'a HttpRequest,
        handoff: Box<dyn FnOnce() -> Result<(), HttpError> + Send + 'a>,
        _observe: Box<dyn FnOnce(u16, Option<Duration>) + Send + 'a>,
    ) -> Result<HttpResponse, HttpError> {
        self.time.advance(self.delay);
        handoff()?;
        self.sent.fetch_add(1, Ordering::SeqCst);
        panic!("invented transport panic after delayed handoff");
    }
}

#[derive(Clone)]
struct RateLimitThenBuildErrorTransport {
    sent: Arc<AtomicUsize>,
}

impl Transport for RateLimitThenBuildErrorTransport {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        if self.sent.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(status(429))
        } else {
            Err(HttpError::ClientNotBuilt(
                "invented post-handoff client failure".to_owned(),
            ))
        }
    }
}

#[derive(Clone)]
struct BreakTallyTransport {
    sent: Arc<AtomicUsize>,
    directory: PathBuf,
    status: u16,
    retry_after: Option<Duration>,
    break_once: Arc<AtomicBool>,
}

impl Transport for BreakTallyTransport {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        if self.break_once.swap(false, Ordering::SeqCst) {
            std::fs::create_dir(self.directory.join("outbound-tally.tmp"))
                .expect("temporary tally path blocked");
        }
        Ok(HttpResponse {
            status: self.status,
            body: Vec::new(),
            retry_after: self.retry_after,
        })
    }
}

struct ProcessHoldingTransport {
    ready: PathBuf,
}

impl Transport for ProcessHoldingTransport {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        std::fs::write(&self.ready, b"pending").expect("pending marker written");
        std::future::pending().await
    }
}

fn status(status: u16) -> HttpResponse {
    HttpResponse {
        status,
        body: Vec::new(),
        retry_after: None,
    }
}

fn try_gateway<T: Transport + 'static>(
    transport: T,
    time: &Arc<FakeTime>,
    tally: &Path,
) -> Result<Gateway<T>, GatewayError> {
    Gateway::with_parts_in_directory(
        transport,
        BUDGETS,
        Arc::clone(time) as Arc<dyn Clock>,
        Arc::clone(time) as Arc<dyn Sleeper>,
        BrokerEgress::On,
        tally.parent().expect("tally has a directory"),
    )
}

fn gateway<T: Transport + 'static>(transport: T, time: &Arc<FakeTime>, tally: &Path) -> Gateway<T> {
    try_gateway(transport, time, tally).expect("documented budgets and tally are valid")
}

fn empty_tally(directory: &TempDir) -> PathBuf {
    let tally = directory.file("outbound-tally");
    std::fs::write(&tally, "").expect("empty tally created");
    tally
}

fn operations() -> HttpRequest {
    HttpRequest::post(
        Destination::TinkoffProd,
        "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor",
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
            .send(&operations(), None)
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
    let first = tokio::spawn(async move { first_gateway.send(&operations(), None).await });
    transport.first_arrived.notified().await;
    let second_gateway = Arc::clone(&gateway);
    let second = tokio::spawn(async move { second_gateway.send(&operations(), None).await });
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
            .send(&operations(), None)
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
        .send(&operations(), None)
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
        .send(&operations(), None)
        .await
        .expect_err("429 pauses");
    assert!(matches!(
        first,
        GatewayError::BrokerHostPaused { attempts: 1, .. }
    ));
    time.step_wall(Duration::from_secs(7 * 24 * 60 * 60));

    let blocked = gateway
        .send(&operations(), None)
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
        .send(&operations(), None)
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
        .send(&operations(), None)
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
        .send(&operations(), None)
        .await
        .expect_err("first 429 pauses");
    assert!(matches!(
        first,
        GatewayError::BrokerHostPaused { retry_after, .. }
            if retry_after == Duration::from_secs(60)
    ));
    time.advance(Duration::from_secs(60));
    let second = gateway
        .send(&operations(), None)
        .await
        .expect_err("second 429 closes");
    assert!(matches!(
        second,
        GatewayError::BrokerHostPaused { retry_after, .. }
            if retry_after == Duration::from_secs(30 * 60)
    ));
    let closed = gateway
        .send(&operations(), None)
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
            .send(&finam_session(), None)
            .await
            .expect_err("Finam refusal returned");
        assert!(matches!(
            refused,
            GatewayError::Rejected { status: 401, .. }
        ));
    }
    let closed = gateway
        .send(&finam_session(), None)
        .await
        .expect_err("Finam closure blocks");
    assert!(matches!(
        closed,
        GatewayError::BrokerHostClosed { attempts: 0, .. }
    ));

    let tinkoff = gateway
        .send(&operations(), None)
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
        .send(&operations(), None)
        .await
        .expect("initial state persisted");
    std::fs::write(&temporary, "partial new state").expect("leftover temporary written");
    gateway
        .send(&operations(), None)
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
            .send(&operations(), None)
            .await
            .expect("request within rolling ceiling sent");
    }
    let oldest = Duration::from_secs(1_800_000_000 + 60);
    let expected_wait = oldest + Duration::from_secs(24 * 60 * 60) - time.boot();
    let refused = gateway
        .send(&operations(), None)
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
        .send(&operations(), None)
        .await
        .expect("oldest request aged out of the rolling day");
    assert_eq!(transport.sent().len(), 1_001);
}

#[tokio::test]
async fn a_not_handed_off_reservation_is_rolled_back_before_daily_exhaustion() {
    let directory = TempDir::create("rollback-before-ceiling");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    transport
        .answers
        .lock()
        .expect("answers")
        .push_back(Err(HttpError::RequestNotBuilt(
            "invented pre-handoff failure".to_owned(),
        )));
    let gateway = gateway(transport.clone(), &time, &tally);

    let first = gateway
        .send(&operations(), None)
        .await
        .expect_err("the planted pre-handoff failure is returned");
    assert!(matches!(first, GatewayError::Transport { attempts: 1, .. }));
    for index in 0..1_000 {
        gateway
            .send(&operations(), None)
            .await
            .unwrap_or_else(|error| {
                panic!("successful send {index} was charged for the rolled-back attempt: {error}")
            });
    }
    let exhausted = gateway
        .send(&operations(), None)
        .await
        .expect_err("the request after 1,000 real handoffs is refused");
    assert!(matches!(
        exhausted,
        GatewayError::DailyCeiling { ceiling: 1_000, .. }
    ));
}

#[tokio::test]
async fn invalid_path_spellings_and_aliases_are_refused_naming_the_path() {
    let directory = TempDir::create("paths");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let build = |path: &Path| {
        Gateway::with_parts_in_directory(
            RecordingTransport::answering(&time),
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            BrokerEgress::On,
            path,
        )
    };

    let relative = PathBuf::from("relative-directory");
    let noncanonical = directory.path.join(".");
    let missing = directory.file("missing");
    let mut invalid = vec![relative, noncanonical, missing];

    #[cfg(unix)]
    {
        let symlink_target = directory.file("symlink-target");
        std::fs::create_dir(&symlink_target).expect("symlink target directory created");
        std::fs::write(symlink_target.join("outbound-tally"), "")
            .expect("symlink target tally created");
        let symlink = directory.file("directory-symlink");
        std::os::unix::fs::symlink(&symlink_target, &symlink).expect("symlink created");
        invalid.push(symlink);

        let hard_directory = directory.file("hard-directory");
        std::fs::create_dir(&hard_directory).expect("hard-link directory created");
        let target = directory.file("hard-target");
        std::fs::write(&target, "").expect("hard-link target created");
        std::fs::hard_link(&target, hard_directory.join("outbound-tally"))
            .expect("hard link created");
        invalid.push(hard_directory);
    }

    for path in invalid {
        let Err(error) = build(&path) else {
            panic!("path alias was accepted: {}", path.display());
        };
        assert!(
            error.to_string().contains("outbound-tally")
                || error.to_string().contains(&path.display().to_string()),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a_corrupt_tally_is_refused_without_transport_and_names_the_path() {
    let directory = TempDir::create("corrupt");
    let tally = directory.file("outbound-tally");
    std::fs::write(&tally, "not a tally\n").expect("corrupt tally written");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    let refused = gateway
        .send(&operations(), None)
        .await
        .expect_err("corrupt tally refuses");
    assert!(matches!(refused, GatewayError::TallyCorrupt { .. }));
    assert!(refused.to_string().contains(&tally.display().to_string()));
    assert!(transport.sent().is_empty());
}
#[tokio::test]
async fn every_older_tally_format_is_refused_with_a_recreation_remedy() {
    for (label, contents) in [
        (
            "v1",
            "iaam-outbound-tally-v1\nhost\thttps://invest-public-api.tbank.ru/rest\t1\t1\t1\t-\n",
        ),
        (
            "v2",
            "iaam-outbound-tally-v2\nboot\ttest-boot\nhost\thttps://invest-public-api.tbank.ru/rest\t-\t\t-\t-\t-\t-\t\t\t-\n",
        ),
        (
            "v3",
            "iaam-outbound-tally-v3\nboot\ttest-boot\nhigh-water\t1\n",
        ),
    ] {
        let directory = TempDir::create(&format!("{label}-refused"));
        let tally = directory.file("outbound-tally");
        std::fs::write(&tally, contents).expect("older tally written");
        let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        let transport = RecordingTransport::answering(&time);
        let gateway = gateway(transport.clone(), &time, &tally);

        let refused = match gateway.send(&operations(), None).await {
            Ok(_) => panic!("{label} tally was accepted"),
            Err(error) => error,
        };
        let message = refused.to_string();

        assert!(matches!(refused, GatewayError::TallyCorrupt { .. }));
        assert!(message.contains(label), "{message}");
        assert!(
            message.contains("no older tally format was deployed"),
            "{message}"
        );
        assert!(message.contains("recreate"), "{message}");
        assert!(transport.sent().is_empty());
    }
}

#[tokio::test]
async fn a_broker_budget_cannot_be_selected_independently_of_the_request_path() {
    let directory = TempDir::create("request-budget");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    let request = HttpRequest::post(
        Destination::TinkoffProd,
        "/tinkoff.public.invest.api.contract.v1.UsersService/Invented",
        RequestBody::Json("{}".to_owned()),
    )
    .with_request_allowance(RequestAllowance::new(u32::MAX));
    let refused = gateway
        .send(&request, None)
        .await
        .expect_err("an unmapped broker path has no caller-selected budget");

    assert!(matches!(refused, GatewayError::UnknownBudget { .. }));
    assert!(transport.sent().is_empty());
}

#[tokio::test]
async fn finam_account_path_near_misses_are_refused_without_sending() {
    let directory = TempDir::create("finam-path-near-misses");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    for path in [
        "/v1/accounts/",
        "/v1/accounts//transactions",
        "/v1/accounts/account/child",
        "/v1/accounts/account/child/transactions",
        "/v1/accounts/./transactions",
        "/v1/accounts/../transactions",
        "/v1/accounts/account?mode=full",
        "/v1/accounts/account#fragment",
    ] {
        let request = HttpRequest::get(Destination::FinamApi, path)
            .with_request_allowance(RequestAllowance::new(u32::MAX));
        let refused = gateway
            .send(&request, None)
            .await
            .expect_err("a near-miss Finam path has no budget");
        assert!(
            matches!(
                refused,
                GatewayError::UnknownBudget {
                    destination: Destination::FinamApi,
                    path: ref refused_path
                } if refused_path == path
            ),
            "{path}: {refused:?}"
        );
    }
    assert!(transport.sent().is_empty());
}

#[tokio::test]
async fn caller_cancellation_does_not_cancel_status_commit() {
    let directory = TempDir::create("caller-cancel");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let sent = Arc::new(AtomicUsize::new(0));
    let first_arrived = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let transport = HoldingTransport {
        sent: Arc::clone(&sent),
        first_arrived: Arc::clone(&first_arrived),
        release_first: Arc::clone(&release_first),
    };
    let gateway = Arc::new(gateway(transport, &time, &tally));
    let arrived = first_arrived.notified();
    let task_gateway = Arc::clone(&gateway);
    let caller = tokio::spawn(async move { task_gateway.send(&operations(), None).await });

    arrived.await;
    caller.abort();
    release_first.notify_one();

    let refused = gateway
        .send(&operations(), None)
        .await
        .expect_err("the detached 429 status pauses the next caller");
    assert!(matches!(refused, GatewayError::BrokerHostPaused { .. }));
    assert_eq!(sent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_transport_panic_leaves_a_durable_unresolved_attempt() {
    let directory = TempDir::create("panic-pending");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let sent = Arc::new(AtomicUsize::new(0));
    let gateway = gateway(
        DelayedPanickingTransport {
            sent: Arc::clone(&sent),
            time: Arc::clone(&time),
            delay: Duration::from_secs(61),
        },
        &time,
        &tally,
    );

    let first = gateway
        .send(&operations(), None)
        .await
        .expect_err("transport task panic is reported");
    assert!(matches!(first, GatewayError::TallyUnavailable { .. }));

    let second = gateway
        .send(&operations(), None)
        .await
        .expect_err("the unresolved attempt closes the host");
    assert!(
        matches!(
            second,
            GatewayError::BrokerHostClosed {
                reason: "an earlier request has no committed status",
                retry_after,
                ..
            } if retry_after == Duration::from_secs(90)
        ),
        "{second:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_tally_commit_failure_latches_until_a_pause_is_persisted() {
    let directory = TempDir::create("commit-latch");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let sent = Arc::new(AtomicUsize::new(0));
    let gateway = gateway(
        BreakTallyTransport {
            sent: Arc::clone(&sent),
            directory: directory.path.clone(),
            status: 200,
            retry_after: None,
            break_once: Arc::new(AtomicBool::new(true)),
        },
        &time,
        &tally,
    );

    let first = gateway
        .send(&operations(), None)
        .await
        .expect_err("status commit fails");
    assert!(matches!(first, GatewayError::TallyUnavailable { .. }));
    std::fs::remove_dir(directory.file("outbound-tally.tmp"))
        .expect("temporary obstruction removed");

    let second = gateway
        .send(&operations(), None)
        .await
        .expect_err("the in-memory latch first persists a pause");
    assert!(
        matches!(second, GatewayError::BrokerHostClosed { .. }),
        "{second:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_successful_response_never_installs_its_named_delay() {
    let directory = TempDir::create("successful-named-delay");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    transport
        .answers
        .lock()
        .expect("answers")
        .push_back(Ok(HttpResponse {
            status: 200,
            body: Vec::new(),
            retry_after: Some(Duration::from_secs(600)),
        }));
    let gateway = gateway(transport.clone(), &time, &tally);

    gateway
        .send(&operations(), None)
        .await
        .expect("successful response is returned");
    gateway
        .send(&operations(), None)
        .await
        .expect("the successful response's named delay is ignored");

    assert_eq!(transport.sent().len(), 2);
}

#[tokio::test]
async fn a_failed_429_commit_replays_its_status_delay_and_rate_limit() {
    let directory = TempDir::create("failed-429-commit");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let sent = Arc::new(AtomicUsize::new(0));
    let gateway = gateway(
        BreakTallyTransport {
            sent: Arc::clone(&sent),
            directory: directory.path.clone(),
            status: 429,
            retry_after: Some(Duration::from_secs(600)),
            break_once: Arc::new(AtomicBool::new(true)),
        },
        &time,
        &tally,
    );

    let first = gateway
        .send(&operations(), None)
        .await
        .expect_err("the planted 429 commit fails");
    assert!(matches!(first, GatewayError::TallyUnavailable { .. }));
    std::fs::remove_dir(directory.file("outbound-tally.tmp"))
        .expect("temporary obstruction removed");

    let replayed = gateway
        .send(&operations(), None)
        .await
        .expect_err("the latched 429 is persisted before another handoff");
    assert!(
        matches!(
            replayed,
            GatewayError::BrokerHostPaused {
                retry_after,
                ..
            } if retry_after == Duration::from_secs(600)
        ),
        "{replayed:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 1);

    time.advance(Duration::from_secs(600));
    let second_429 = gateway
        .send(&operations(), None)
        .await
        .expect_err("the second 429 closes the endpoint");
    assert!(
        second_429
            .retry_after()
            .is_some_and(|delay| delay == Duration::from_secs(30 * 60)),
        "{second_429:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_short_retry_after_still_persists_the_sixty_second_floor() {
    let directory = TempDir::create("short-429-delay");
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
            retry_after: Some(Duration::from_secs(5)),
        }));
    let gateway = gateway(transport, &time, &tally);

    let refused = gateway
        .send(&operations(), None)
        .await
        .expect_err("429 pauses the endpoint");
    assert!(
        matches!(
            refused,
            GatewayError::BrokerHostPaused {
                retry_after,
                ..
            } if retry_after == Duration::from_secs(60)
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn an_unresolved_attempt_after_a_recent_429_closes_for_thirty_minutes() {
    let directory = TempDir::create("recent-429-unresolved");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    transport
        .answers
        .lock()
        .expect("answers")
        .extend([Ok(status(429)), Err(HttpError::Network)]);
    let gateway = gateway(transport.clone(), &time, &tally);

    let first = gateway
        .send(&operations(), None)
        .await
        .expect_err("the first 429 pauses the endpoint");
    assert!(matches!(first, GatewayError::BrokerHostPaused { .. }));
    time.advance(Duration::from_secs(9 * 60));

    let unresolved = gateway
        .send(&operations(), None)
        .await
        .expect_err("the unresolved attempt after a recent 429 closes the endpoint");
    assert!(
        matches!(
            unresolved,
            GatewayError::BrokerHostClosed {
                retry_after,
                ..
            } if retry_after == Duration::from_secs(30 * 60)
        ),
        "{unresolved:?}"
    );
    assert_eq!(transport.sent().len(), 2);
}

#[tokio::test]
async fn expiry_after_a_recent_429_closes_for_thirty_minutes() {
    let directory = TempDir::create("recent-429-expired-pending");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let sent = Arc::new(AtomicUsize::new(0));
    let gateway = gateway(
        RateLimitThenBuildErrorTransport {
            sent: Arc::clone(&sent),
        },
        &time,
        &tally,
    );

    let first = gateway
        .send(&operations(), None)
        .await
        .expect_err("the first 429 pauses the endpoint");
    assert!(matches!(first, GatewayError::BrokerHostPaused { .. }));
    time.advance(Duration::from_secs(60));

    let handed_off = gateway
        .send(&operations(), None)
        .await
        .expect_err("the planted post-handoff failure leaves a pending request");
    assert!(matches!(handed_off, GatewayError::Transport { .. }));
    time.advance(Duration::from_secs(90));

    let expired = gateway
        .send(&operations(), None)
        .await
        .expect_err("the recent 429 turns pending expiry into a closure");
    assert!(
        matches!(
            expired,
            GatewayError::BrokerHostClosed {
                retry_after,
                ..
            } if retry_after == Duration::from_secs(30 * 60)
        ),
        "{expired:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn truncating_a_committed_tally_is_refused_without_transport() {
    let directory = TempDir::create("truncated-current-tally");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    gateway
        .send(&operations(), None)
        .await
        .expect("the first generation is committed");
    std::fs::write(&tally, "").expect("the tally is truncated");

    let refused = gateway
        .send(&operations(), None)
        .await
        .expect_err("a truncated committed tally is refused");
    assert!(matches!(refused, GatewayError::TallyCorrupt { .. }));
    assert_eq!(transport.sent().len(), 1);
}

#[tokio::test]
async fn rolling_back_only_the_tally_is_refused_without_transport() {
    let directory = TempDir::create("rolled-back-current-tally");
    let tally = empty_tally(&directory);
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);

    gateway
        .send(&operations(), None)
        .await
        .expect("the first generation is committed");
    let older = std::fs::read(&tally).expect("the older tally is read");
    gateway
        .send(&operations(), None)
        .await
        .expect("a newer generation is committed");
    std::fs::write(&tally, older).expect("only the tally is rolled back");

    let refused = gateway
        .send(&operations(), None)
        .await
        .expect_err("a tally-generation mismatch is refused");
    assert!(matches!(refused, GatewayError::TallyCorrupt { .. }));
    assert_eq!(transport.sent().len(), 2);
}

#[test]
fn pending_attempt_child() {
    let Some(directory) = std::env::var_os("IAAM_PENDING_CHILD_DIRECTORY") else {
        return;
    };
    let ready = PathBuf::from(
        std::env::var_os("IAAM_PENDING_CHILD_READY").expect("child ready path configured"),
    );
    let tally = PathBuf::from(directory).join("outbound-tally");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        let gateway = gateway(ProcessHoldingTransport { ready }, &time, &tally);
        let _ = gateway.send(&operations(), None).await;
    });
}

#[tokio::test]
async fn process_death_leaves_the_pending_attempt_closed_for_ninety_seconds() {
    let directory = TempDir::create("process-death");
    let tally = empty_tally(&directory);
    let ready = directory.file("pending-ready");
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "pending_attempt_child", "--nocapture"])
        .env("IAAM_PENDING_CHILD_DIRECTORY", &directory.path)
        .env("IAAM_PENDING_CHILD_READY", &ready)
        .spawn()
        .expect("pending child started");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "pending child did not hand off");
        assert_eq!(child.try_wait().expect("child status"), None);
        std::thread::sleep(Duration::from_millis(1));
    }
    child.kill().expect("pending child killed");
    child.wait().expect("pending child reaped");

    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_120));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, &tally);
    let refused = gateway
        .send(&operations(), None)
        .await
        .expect_err("the killed attempt remains unresolved");
    assert!(
        matches!(
            refused,
            GatewayError::BrokerHostClosed {
                reason: "an earlier request has no committed status",
                retry_after,
                ..
            } if retry_after == Duration::from_secs(90)
        ),
        "{refused:?}"
    );
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
            .send(&operations(), None)
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
            .send(&finam_session(), None)
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
        .send(&operations(), None)
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
        .send(&operations(), None)
        .await
        .expect_err("broker egress is off");
    assert!(matches!(refused, GatewayError::BrokerEgressOff));
    off.send(
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
