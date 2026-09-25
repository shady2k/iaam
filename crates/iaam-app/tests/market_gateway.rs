//! The market adapter sends through the gateway: it keeps no retries and no
//! pacing of its own, only the hashing of a successful body.
//!
//! Time is fake and the endpoint scripted, so nothing here sleeps or touches
//! the network.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iaam_app::adapters::market::HttpOutbound;
use iaam_app::error::AppError;
use iaam_app::ports::OutboundHttp;
use iaam_http::gateway::{BUDGETS, Clock, Sleeper, Transport};
use iaam_http::{Destination, Gateway, HttpError, HttpRequest, HttpResponse};
use iaam_market::cbr::key_rate::key_rate_request;
use time::macros::date;

/// A clock that moves only when something sleeps on it.
struct FakeTime {
    now: Mutex<Instant>,
    slept: Mutex<Vec<Duration>>,
}

impl FakeTime {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Instant::now()),
            slept: Mutex::new(Vec::new()),
        })
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
        *self.now.lock().expect("clock") += delay;
        Box::pin(async {})
    }
}

/// An endpoint that answers from a script, then with a default status.
struct Scripted {
    script: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
    default_status: u16,
    sent: Arc<Mutex<usize>>,
}

impl Scripted {
    fn answering(default_status: u16) -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            default_status,
            sent: Arc::new(Mutex::new(0)),
        }
    }

    fn then(self, answer: Result<HttpResponse, HttpError>) -> Self {
        self.script.lock().expect("script").push_back(answer);
        self
    }
}

impl Transport for Scripted {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        *self.sent.lock().expect("sent") += 1;
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

/// The adapter over a gateway with the documented budgets and fake time, and
/// the count of requests that reached the endpoint.
fn adapter(time: &Arc<FakeTime>, endpoint: Scripted) -> (HttpOutbound, Arc<Mutex<usize>>) {
    let sent = Arc::clone(&endpoint.sent);
    let gateway = Gateway::with_parts(
        endpoint,
        BUDGETS,
        Arc::clone(time) as Arc<dyn Clock>,
        Arc::clone(time) as Arc<dyn Sleeper>,
    )
    .expect("the documented table is valid");
    (HttpOutbound::new(Arc::new(gateway)), sent)
}

fn sent(count: &Mutex<usize>) -> usize {
    *count.lock().expect("sent")
}

fn moex() -> HttpRequest {
    HttpRequest::get(
        Destination::MoexIss,
        "/iss/history/engines/stock/markets/bonds/securities/SU00000RMFS0.json",
    )
}

#[tokio::test]
async fn a_503_from_moex_is_retried_through_the_gateway() {
    let time = FakeTime::new();
    let (adapter, endpoint) = adapter(&time, Scripted::answering(200).then(Ok(status(503))));

    let response = adapter.send(moex()).await.expect("the retry succeeded");

    assert_eq!(response.status, 200);
    assert_eq!(sent(&endpoint), 2);
    // The gateway's first backoff, waited on the gateway's clock.
    assert_eq!(time.slept(), [Duration::from_secs(1)]);
}

/// The key rate is a SOAP POST, sent once unless marked safe to repeat; it
/// only reads, so a transient failure of it is retried like a GET.
#[tokio::test]
async fn a_503_from_the_cbr_key_rate_is_retried_through_the_gateway() {
    let time = FakeTime::new();
    let (adapter, endpoint) = adapter(&time, Scripted::answering(200).then(Ok(status(503))));

    let response = adapter
        .send(key_rate_request(
            date!(2026 - 01 - 01),
            date!(2026 - 02 - 01),
        ))
        .await
        .expect("the retry succeeded");

    assert_eq!(response.status, 200);
    assert_eq!(sent(&endpoint), 2);
}

/// A source that names a wait longer than the gateway holds a caller is
/// deferred: still "unreachable, retry later", with the wait it named.
#[tokio::test]
async fn a_source_that_asks_for_a_long_wait_is_unreachable_until_then() {
    let named = Duration::from_secs(16 * 60);
    let time = FakeTime::new();
    let (adapter, endpoint) = adapter(
        &time,
        Scripted::answering(200).then(Ok(HttpResponse {
            retry_after: Some(named),
            ..status(503)
        })),
    );
    adapter
        .send(moex())
        .await
        .expect_err("the first call learns the wait");

    let deferred = adapter
        .send(moex())
        .await
        .expect_err("the next call is deferred");

    assert_eq!(sent(&endpoint), 1, "nothing is sent while the wait runs");
    match deferred {
        AppError::SourceUnreachable {
            origin,
            retry_after,
            ..
        } => {
            assert_eq!(origin, "moex-iss");
            assert_eq!(retry_after, Some(named));
        }
        other => panic!("expected an unreachable source, got {other:?}"),
    }
}

#[tokio::test]
async fn a_successful_body_is_returned_with_its_sha256() {
    let time = FakeTime::new();
    let (adapter, _) = adapter(
        &time,
        Scripted::answering(200).then(Ok(HttpResponse {
            body: b"abc".to_vec(),
            ..status(200)
        })),
    );

    let response = adapter.send(moex()).await.expect("2xx is a success");

    assert_eq!(response.body, b"abc");
    // SHA-256 of "abc", the FIPS 180-2 test vector.
    assert_eq!(
        response.raw_hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[tokio::test]
async fn a_permanent_refusal_is_reported_with_its_status_and_not_retried() {
    let time = FakeTime::new();
    let (adapter, endpoint) = adapter(&time, Scripted::answering(404));

    let refused = adapter.send(moex()).await.expect_err("404 is a refusal");

    assert_eq!(sent(&endpoint), 1);
    match refused {
        AppError::SourceRefused { origin, detail } => {
            assert_eq!(origin, "moex-iss");
            assert!(detail.contains("404"), "{detail}");
        }
        other => panic!("expected a refusal by the source, got {other:?}"),
    }
}

#[tokio::test]
async fn a_source_that_keeps_failing_is_reported_as_worth_retrying_later() {
    let time = FakeTime::new();
    let (adapter, endpoint) = adapter(&time, Scripted::answering(503));

    let refused = adapter
        .send(moex())
        .await
        .expect_err("every attempt failed");

    assert_eq!(sent(&endpoint), 5);
    match refused {
        AppError::SourceUnreachable {
            origin,
            detail,
            retry_after,
        } => {
            assert_eq!(origin, "moex-iss");
            assert!(detail.contains("503"), "{detail}");
            // The gateway's own wait survives the adapter rather than a guess.
            assert!(retry_after.is_some_and(|wait| wait > Duration::ZERO));
        }
        other => panic!("expected an unreachable source, got {other:?}"),
    }
}

#[tokio::test]
async fn each_market_destination_is_named_after_its_source() {
    let time = FakeTime::new();
    let (adapter, _) = adapter(&time, Scripted::answering(404));
    let requests = [
        (
            HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp"),
            "cbr",
        ),
        (
            HttpRequest::get(
                Destination::CbrDailyInfo,
                "/DailyInfoWebServ/DailyInfo.asmx",
            ),
            "cbr",
        ),
        (
            HttpRequest::get(Destination::TinvestContract, "/contracts/operations.proto"),
            "tinvest-contract",
        ),
    ];

    for (request, expected) in requests {
        match adapter.send(request).await {
            Err(AppError::SourceRefused { origin, .. }) => assert_eq!(origin, expected),
            other => panic!("expected a refusal by {expected}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_request_without_a_budget_is_refused_without_sending() {
    let time = FakeTime::new();
    let (adapter, endpoint) = adapter(&time, Scripted::answering(200));
    let broker = HttpRequest::get(Destination::TinkoffProd, "/accounts");

    let refused = adapter.send(broker).await.expect_err("no budget for it");

    assert_eq!(sent(&endpoint), 0);
    // A request without a budget is our defect, not the source's answer.
    assert!(matches!(refused, AppError::Store(_)), "{refused:?}");
    assert!(refused.to_string().contains("no budget"), "{refused}");
}
