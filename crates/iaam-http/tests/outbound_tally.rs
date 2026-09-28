use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iaam_http::gateway::{BUDGETS, Clock, Sleeper, Transport};
use iaam_http::{
    BrokerEgress, Destination, Gateway, GatewayError, HttpError, HttpRequest, HttpResponse,
    RequestBody,
};

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
    wall: Mutex<SystemTime>,
    slept: Mutex<Vec<Duration>>,
}

impl FakeTime {
    fn at(wall: SystemTime) -> Arc<Self> {
        Arc::new(Self {
            monotonic: Mutex::new(Instant::now()),
            wall: Mutex::new(wall),
            slept: Mutex::new(Vec::new()),
        })
    }

    fn advance(&self, by: Duration) {
        *self.monotonic.lock().expect("monotonic clock") += by;
        *self.wall.lock().expect("wall clock") += by;
    }

    fn wall(&self) -> SystemTime {
        *self.wall.lock().expect("wall clock")
    }
}

impl Clock for FakeTime {
    fn now(&self) -> Instant {
        *self.monotonic.lock().expect("monotonic clock")
    }

    fn now_utc(&self) -> SystemTime {
        self.wall()
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
}

impl RecordingTransport {
    fn answering(time: &Arc<FakeTime>) -> Self {
        Self {
            time: Arc::clone(time),
            sent: Arc::new(Mutex::new(Vec::new())),
            answers: Arc::new(Mutex::new(VecDeque::new())),
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
            .unwrap_or_else(|| {
                Ok(HttpResponse {
                    status: 200,
                    body: Vec::new(),
                    retry_after: None,
                })
            })
    }
}

fn gateway(
    transport: RecordingTransport,
    time: &Arc<FakeTime>,
    egress: BrokerEgress,
) -> Gateway<RecordingTransport> {
    Gateway::with_parts(
        transport,
        BUDGETS,
        Arc::clone(time) as Arc<dyn Clock>,
        Arc::clone(time) as Arc<dyn Sleeper>,
        egress,
    )
    .expect("documented budgets are valid")
}

fn operations() -> HttpRequest {
    HttpRequest::post(
        Destination::TinkoffProd,
        "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
        RequestBody::Json("{}".to_owned()),
    )
    .idempotent()
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
async fn two_gateways_and_a_rebuilt_gateway_share_spacing_and_the_minute_budget() {
    let directory = TempDir::create("shared");
    let tally = directory.file("tally");
    std::fs::write(&tally, "").expect("empty tally created");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let first = gateway(
        transport.clone(),
        &time,
        BrokerEgress::On {
            tally: tally.clone(),
        },
    );
    let second = gateway(
        transport.clone(),
        &time,
        BrokerEgress::On {
            tally: tally.clone(),
        },
    );

    for index in 0..51 {
        let current = if index % 2 == 0 { &first } else { &second };
        current
            .send("OperationsService", &operations(), None)
            .await
            .expect("request sent");
    }
    let rebuilt = gateway(transport.clone(), &time, BrokerEgress::On { tally });
    rebuilt
        .send("OperationsService", &operations(), None)
        .await
        .expect("request sent after rebuild");

    let sent: Vec<_> = transport.sent().into_iter().map(|(_, at)| at).collect();
    assert_eq!(sent.len(), 52);
    for pair in sent.windows(2) {
        assert!(elapsed(pair[1], pair[0]) >= Duration::from_secs(1));
    }
    assert_never_more_than(50, Duration::from_secs(60), &sent);
}

#[tokio::test]
async fn the_thousand_and_first_broker_send_of_a_utc_day_is_refused_without_transport() {
    let directory = TempDir::create("daily");
    let tally = directory.file("tally");
    std::fs::write(&tally, "").expect("empty tally created");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let gateway = gateway(transport.clone(), &time, BrokerEgress::On { tally });

    for _ in 0..1_000 {
        gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect("request within daily ceiling sent");
    }
    let refused = gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("daily ceiling refuses the next request");

    let GatewayError::DailyCeiling {
        resets_at,
        retry_after,
        ..
    } = &refused
    else {
        panic!("expected daily ceiling, got {refused}");
    };
    let seconds_today = time
        .wall()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock after epoch")
        .as_secs()
        % 86_400;
    let expected_wait = Duration::from_secs(86_400 - seconds_today);
    assert_eq!(*retry_after, expected_wait);
    assert_eq!(
        resets_at,
        &httpdate::fmt_http_date(time.wall() + expected_wait)
    );
    let message = refused.to_string();
    assert!(message.contains("1000"), "{message}");
    assert!(message.contains("resets at"), "{message}");
    assert_eq!(transport.sent().len(), 1_000);

    time.advance(expected_wait);
    gateway
        .send("OperationsService", &operations(), None)
        .await
        .expect("the next UTC day has a fresh ceiling");
    assert_eq!(transport.sent().len(), 1_001);
}

#[tokio::test]
async fn missing_corrupt_and_unopenable_tallies_refuse_without_sending_and_name_the_path() {
    let directory = TempDir::create("broken");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let corrupt = directory.file("corrupt-tally");
    std::fs::write(&corrupt, "not a tally\n").expect("corrupt tally written");
    let descending = directory.file("descending-tally");
    std::fs::write(
        &descending,
        "iaam-outbound-tally-v1\nbudget\thost\tOperationsService\t2,1\n",
    )
    .expect("descending tally written");
    let empty_host = directory.file("empty-host-tally");
    std::fs::write(&empty_host, "iaam-outbound-tally-v1\nhost\t\t-\t0\t0\t-\n")
        .expect("empty host tally written");
    let missing = directory.file("missing-tally");

    for (path, detail) in [
        (missing, None),
        (corrupt, None),
        (descending, Some("out of order")),
        (empty_host, Some("line 2")),
        (directory.path.clone(), None),
    ] {
        let gateway = gateway(
            transport.clone(),
            &time,
            BrokerEgress::On {
                tally: path.clone(),
            },
        );
        let refused = gateway
            .send("OperationsService", &operations(), None)
            .await
            .expect_err("bad tally refuses the request");
        let message = refused.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        if let Some(detail) = detail {
            assert!(message.contains(detail), "{message}");
        }
    }
    assert!(transport.sent().is_empty());
}

#[tokio::test]
async fn egress_switch_applies_only_to_broker_destinations() {
    let directory = TempDir::create("switch");
    let time = FakeTime::at(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let transport = RecordingTransport::answering(&time);
    let off = gateway(transport.clone(), &time, BrokerEgress::Off);

    let refused = off
        .send("OperationsService", &operations(), None)
        .await
        .expect_err("broker egress is off");
    assert!(matches!(refused, GatewayError::BrokerEgressOff));
    assert!(refused.to_string().contains("IAAM_BROKER_EGRESS"));
    assert!(transport.sent().is_empty());

    off.send(
        "market",
        &HttpRequest::get(Destination::MoexIss, "/iss/history.json"),
        None,
    )
    .await
    .expect("MOEX passes while broker egress is off");

    let tally = directory.file("tally");
    std::fs::write(&tally, "").expect("empty tally created");
    let on = gateway(transport.clone(), &time, BrokerEgress::On { tally });
    on.send("OperationsService", &operations(), None)
        .await
        .expect("broker request passes through the tally");

    let corrupt = directory.file("corrupt");
    std::fs::write(&corrupt, "broken").expect("corrupt tally written");
    let on_with_corrupt_tally = gateway(
        transport.clone(),
        &time,
        BrokerEgress::On { tally: corrupt },
    );
    on_with_corrupt_tally
        .send(
            "rates",
            &HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp"),
            None,
        )
        .await
        .expect("CBR does not consult the broker tally");

    let destinations: Vec<_> = transport
        .sent()
        .into_iter()
        .map(|(destination, _)| destination)
        .collect();
    assert_eq!(
        destinations,
        [
            Destination::MoexIss,
            Destination::TinkoffProd,
            Destination::CbrScripts,
        ]
    );
}
