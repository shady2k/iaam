use std::time::Duration;

use iaam_http::test_support::{HttpClientHarness, LoopbackReply, LoopbackServer};
use iaam_http::{BrokerEgress, Destination, Gateway, GatewayError, HttpRequest};

#[tokio::test]
async fn a_redirect_returns_to_the_gateway_as_one_refusal() {
    let server = LoopbackServer::start([
        LoopbackReply::redirect(307, "/second"),
        LoopbackReply::redirect(307, "/third"),
        LoopbackReply::complete(200, "followed"),
    ])
    .expect("loopback server");
    let gateway = Gateway::new(HttpClientHarness::new(&server), BrokerEgress::Off)
        .expect("valid gateway budgets");
    let request = HttpRequest::get(Destination::MoexIss, "/first");

    let refusal = gateway
        .send(&request, None)
        .await
        .expect_err("a redirect is a refusal, not a transparent second request");

    assert!(matches!(
        refusal,
        GatewayError::Rejected {
            status: 307,
            attempts: 1,
            ..
        }
    ));
    assert_eq!(server.requests_received(), 1);
    assert_eq!(server.request_targets(), ["/first"]);
}

#[tokio::test]
async fn the_server_records_every_request_in_order() {
    let server = LoopbackServer::start([
        LoopbackReply::complete(200, "first"),
        LoopbackReply::complete(200, "second"),
    ])
    .expect("loopback server");
    let client = HttpClientHarness::new(&server);

    client
        .send(&HttpRequest::get(Destination::MoexIss, "/one"))
        .await
        .expect("first response");
    client
        .send(&HttpRequest::get(Destination::MoexIss, "/two"))
        .await
        .expect("second response");

    assert_eq!(server.requests_received(), 2);
    assert_eq!(server.connections_accepted(), 2);
    assert_eq!(server.request_targets(), ["/one", "/two"]);
}

#[tokio::test]
async fn a_rate_limit_status_survives_a_truncated_body() {
    let server = LoopbackServer::start([
        LoopbackReply::truncated_body(429, "partial").with_header("Retry-After", "60")
    ])
    .expect("loopback server");
    let client = HttpClientHarness::new(&server);
    let request = HttpRequest::get(Destination::MoexIss, "/reset");

    let response = client
        .send(&request)
        .await
        .expect("the received status must survive a body failure");

    assert_eq!(response.status, 429);
    assert_eq!(response.body, b"partial");
    assert_eq!(response.retry_after, Some(Duration::from_secs(60)));
    assert_eq!(server.requests_received(), 1);
}

#[tokio::test]
async fn a_success_with_a_truncated_body_remains_a_transport_failure() {
    let server = LoopbackServer::start([LoopbackReply::truncated_body(200, "partial")])
        .expect("loopback server");
    let client = HttpClientHarness::new(&server);
    let request = HttpRequest::get(Destination::MoexIss, "/reset");

    let error = client
        .send(&request)
        .await
        .expect_err("a partial success body is not a successful response");

    assert!(matches!(error, iaam_http::HttpError::Network));
    assert_eq!(server.requests_received(), 1);
}

#[tokio::test]
async fn a_rate_limit_status_survives_a_stalled_body_timeout() {
    let server = LoopbackServer::start([
        LoopbackReply::stalled_body(429, "partial").with_header("Retry-After", "60")
    ])
    .expect("loopback server");
    let client = HttpClientHarness::new(&server).with_timeout(Duration::from_millis(50));
    let request = HttpRequest::get(Destination::MoexIss, "/stalled");

    let response = client
        .send(&request)
        .await
        .expect("the received status must survive a body timeout");

    assert_eq!(response.status, 429);
    assert_eq!(response.body, b"partial");
    assert_eq!(response.retry_after, Some(Duration::from_secs(60)));
    assert_eq!(server.requests_received(), 1);
}

#[tokio::test]
async fn retry_after_delay_seconds_are_clamped_to_one_day() {
    let server =
        LoopbackServer::start([LoopbackReply::complete(429, "limited")
            .with_header("Retry-After", &u64::MAX.to_string())])
        .expect("loopback server");
    let client = HttpClientHarness::new(&server);
    let request = HttpRequest::get(Destination::MoexIss, "/limited");

    let response = client.send(&request).await.expect("rate-limit response");

    assert_eq!(response.status, 429);
    assert_eq!(
        response.retry_after,
        Some(Duration::from_secs(24 * 60 * 60))
    );
    assert_eq!(server.requests_received(), 1);
}

#[tokio::test]
async fn reset_header_delay_is_clamped_to_one_day() {
    let server = LoopbackServer::start([LoopbackReply::complete(429, "limited")
        .with_header("x-ratelimit-reset", &u64::MAX.to_string())])
    .expect("loopback server");
    let client = HttpClientHarness::new(&server);
    let request =
        HttpRequest::get(Destination::MoexIss, "/limited").with_reset_header("x-ratelimit-reset");

    let response = client.send(&request).await.expect("rate-limit response");

    assert_eq!(response.status, 429);
    assert_eq!(
        response.retry_after,
        Some(Duration::from_secs(24 * 60 * 60))
    );
    assert_eq!(server.requests_received(), 1);
}
