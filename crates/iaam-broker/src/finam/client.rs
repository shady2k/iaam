use std::sync::Arc;
use std::time::Duration;

use crate::credentials::BrokerToken;
use iaam_http::client::HttpClient;
use iaam_http::gateway::Transport;
use iaam_http::{Destination, Gateway, GatewayError, HttpRequest};
use serde_json::Value;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, Time};

/// Budget key of `AccountsService/GetAccount`. Finam states its limit per
/// method, so each method draws on a budget of its own.
const GET_ACCOUNT: &str = "AccountsService.GetAccount";

/// Budget key of `AccountsService/Transactions`.
const TRANSACTIONS: &str = "AccountsService.Transactions";

/// HTTP access errors for the Finam Trade API.
///
/// No variant carries the token: a rejected body is kept with it hidden.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FinamError {
    /// The request could not be sent at all; retrying meets the same fault.
    #[error("Finam gateway network refusal")]
    Network,
    /// Finam kept answering 429 through every retry.
    #[error("Finam gateway rate-limited the request; retry after {retry_after:?}")]
    RateLimited { retry_after: Duration },
    /// Finam kept failing transiently, or the circuit to it is open; the same
    /// request may succeed after `retry_after`.
    #[error(
        "Finam is unavailable after {attempts} attempts (last status {status:?}); retry after {retry_after:?}"
    )]
    Unavailable {
        status: Option<u16>,
        attempts: u32,
        retry_after: Duration,
    },
    #[error("Finam token is invalid")]
    InvalidToken,
    #[error("unexpected HTTP status {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },
    /// The gateway refused the call before sending it: its budget table has
    /// no row for Finam, which is a fault of this build, not of the request.
    #[error("the outbound gateway refused the Finam call: {reason}")]
    Gateway { reason: String },
    #[error("Finam paginated response is truncated: next-page token is missing")]
    PartialResponse,
    #[error("successful response does not match the JSON schema")]
    MalformedResponse,
}

/// Finam HTTP client returning raw response bodies.
///
/// Every request goes through the process's one outbound gateway, which owns
/// the budget, the retries and the breaker; this client only describes the
/// request and classifies what comes back.
pub struct FinamClient<T = HttpClient> {
    token: BrokerToken,
    gateway: Arc<Gateway<T>>,
}

impl<T: Transport> FinamClient<T> {
    /// Create a client over the shared gateway; the token remains in a
    /// zeroizing wrapper.
    #[must_use]
    pub const fn new(token: BrokerToken, gateway: Arc<Gateway<T>>) -> Self {
        Self { token, gateway }
    }

    /// Return the raw body of the account's current portfolio.
    pub async fn get_portfolio(&self, account_id: &str) -> Result<String, FinamError> {
        self.get(GET_ACCOUNT, &format!("/v1/accounts/{account_id}"), &[])
            .await
    }

    /// Return the raw body of a transaction page for an interval.
    pub async fn get_transactions(
        &self,
        account_id: &str,
        from: Date,
        to: Date,
    ) -> Result<String, FinamError> {
        let query = [
            ("interval.start_time", rfc3339_midnight(from)),
            ("interval.end_time", rfc3339_midnight(to)),
        ];
        let body = self
            .get(
                TRANSACTIONS,
                &format!("/v1/accounts/{account_id}/transactions"),
                &query,
            )
            .await?;
        validate_transactions_page(&body)?;
        Ok(body)
    }

    async fn get(
        &self,
        method: &'static str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<String, FinamError> {
        let mut request =
            HttpRequest::get(Destination::FinamApi, path).with_bearer(self.token.expose());
        for (key, value) in query {
            request = request.with_query(key, value);
        }
        let response = self
            .gateway
            .send(method, &request, None)
            .await
            .map_err(|error| classify_refusal(error, self.token.expose()))?;
        String::from_utf8(response.body).map_err(|_| FinamError::MalformedResponse)
    }
}

fn rfc3339_midnight(date: Date) -> String {
    OffsetDateTime::new_utc(date, Time::MIDNIGHT)
        .format(&Rfc3339)
        .unwrap_or_else(|_| format!("{date}T00:00:00Z"))
}

/// Finam's meaning of a call the gateway did not complete.
fn classify_refusal(error: GatewayError, token: &str) -> FinamError {
    if let Some(retry_after) = error.retry_after() {
        return match error.status() {
            Some(429) => FinamError::RateLimited { retry_after },
            status => FinamError::Unavailable {
                status,
                attempts: error.attempts(),
                retry_after,
            },
        };
    }
    match error {
        GatewayError::Rejected { status, body, .. } => {
            classify_rejection(status, body.as_bytes(), token)
        }
        GatewayError::Transport { .. } => FinamError::Network,
        other => FinamError::Gateway {
            reason: other.to_string(),
        },
    }
}

/// Finam's meaning of a status the gateway would not retry.
fn classify_rejection(status: u16, body: &[u8], token: &str) -> FinamError {
    match status {
        401 | 403 => FinamError::InvalidToken,
        status => FinamError::UnexpectedStatus {
            status,
            body: redact_token(&String::from_utf8_lossy(body), token),
        },
    }
}

fn validate_transactions_page(body: &str) -> Result<(), FinamError> {
    let value: Value = serde_json::from_str(body).map_err(|_| FinamError::MalformedResponse)?;
    let has_more = value
        .get("hasMore")
        .or_else(|| value.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let next_page_token = value
        .get("nextPageToken")
        .or_else(|| value.get("next_page_token"))
        .and_then(Value::as_str);
    if has_more && next_page_token.is_none_or(str::is_empty) {
        return Err(FinamError::PartialResponse);
    }
    Ok(())
}

fn redact_token(body: &str, token: &str) -> String {
    if token.is_empty() {
        body.to_owned()
    } else {
        body.replace(token, "<token hidden>")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use iaam_http::gateway::{BUDGETS, Budget, Clock, MethodScope, Sleeper, Transport};
    use iaam_http::{Destination, Gateway, HttpError, HttpRequest, HttpResponse};
    use time::macros::date;

    use super::{FinamClient, FinamError, classify_rejection, validate_transactions_page};
    use crate::credentials::{Key, open, seal};

    const TOKEN: &str = "finam-invented-secret";

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

    /// An endpoint that answers from a script, then with a default status,
    /// and remembers each request it received.
    struct Scripted {
        script: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
        default_status: u16,
        received: Mutex<Vec<HttpRequest>>,
    }

    impl Scripted {
        fn answering(default_status: u16) -> Self {
            Self {
                script: Mutex::new(VecDeque::new()),
                default_status,
                received: Mutex::new(Vec::new()),
            }
        }

        fn then(self, status: u16, body: &str) -> Self {
            self.script
                .lock()
                .expect("script")
                .push_back(Ok(response(status, body)));
            self
        }

        fn then_fault(self, fault: HttpError) -> Self {
            self.script.lock().expect("script").push_back(Err(fault));
            self
        }
    }

    /// The gateway owns its transport; the test keeps a second handle to see
    /// what reached the endpoint.
    struct Shared(Arc<Scripted>);

    impl Transport for Shared {
        async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            let endpoint = &self.0;
            endpoint
                .received
                .lock()
                .expect("received")
                .push(request.clone());
            endpoint
                .script
                .lock()
                .expect("script")
                .pop_front()
                .unwrap_or_else(|| Ok(response(endpoint.default_status, "")))
        }
    }

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
            retry_after: None,
        }
    }

    fn client_over(
        budgets: &'static [Budget],
        endpoint: &Arc<Scripted>,
    ) -> (FinamClient<Shared>, Arc<FakeTime>) {
        let time = FakeTime::new();
        let gateway = Gateway::with_parts(
            Shared(Arc::clone(endpoint)),
            budgets,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
        )
        .expect("the budget table is valid");
        let key = Key::from_bytes([7; 32]);
        let token = open(&key, &seal(&key, TOKEN)).expect("an invented token");
        (FinamClient::new(token, Arc::new(gateway)), time)
    }

    fn assert_no_token(error: &FinamError) {
        assert!(!error.to_string().contains(TOKEN), "{error}");
        assert!(!format!("{error:?}").contains(TOKEN), "{error:?}");
    }

    #[tokio::test]
    async fn a_503_is_retried_through_the_gateway() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(503, "")
                .then(200, r#"{"id":"Main"}"#),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        let body = client
            .get_portfolio("Main")
            .await
            .expect("the retry succeeds");

        assert_eq!(body, r#"{"id":"Main"}"#);
        assert_eq!(endpoint.received.lock().expect("received").len(), 2);
        assert_eq!(time.slept(), vec![iaam_http::gateway::FIRST_BACKOFF]);
    }

    #[tokio::test]
    async fn a_401_is_an_invalid_token_and_is_not_retried() {
        let endpoint = Arc::new(Scripted::answering(401).then(401, TOKEN));
        let (client, time) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("401 is refused");

        assert_eq!(error, FinamError::InvalidToken);
        assert_eq!(endpoint.received.lock().expect("received").len(), 1);
        assert!(time.slept().is_empty());
        assert_no_token(&error);
    }

    #[tokio::test]
    async fn a_403_is_an_invalid_token_too() {
        let endpoint = Arc::new(Scripted::answering(403));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 02 - 01))
            .await
            .expect_err("403 is refused");

        assert_eq!(error, FinamError::InvalidToken);
        assert_eq!(endpoint.received.lock().expect("received").len(), 1);
    }

    #[tokio::test]
    async fn a_503_to_the_end_says_when_to_try_again_without_the_token() {
        let endpoint = Arc::new(Scripted::answering(503));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("every attempt fails");

        assert_eq!(
            endpoint.received.lock().expect("received").len(),
            iaam_http::gateway::ATTEMPTS as usize
        );
        match &error {
            FinamError::Unavailable {
                status,
                attempts,
                retry_after,
            } => {
                assert_eq!(*status, Some(503));
                assert_eq!(*attempts, iaam_http::gateway::ATTEMPTS);
                assert!(!retry_after.is_zero());
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert_no_token(&error);
    }

    #[tokio::test]
    async fn a_429_to_the_end_is_rate_limited_with_its_wait() {
        let endpoint = Arc::new(Scripted::answering(429));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("every attempt is throttled");

        match error {
            FinamError::RateLimited { retry_after } => assert!(!retry_after.is_zero()),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rejected_body_is_kept_with_the_token_hidden() {
        let endpoint = Arc::new(Scripted::answering(400).then(400, &format!("bad {TOKEN}")));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("400 is refused");

        assert_eq!(
            error,
            FinamError::UnexpectedStatus {
                status: 400,
                body: "bad <token hidden>".to_owned(),
            }
        );
        assert_no_token(&error);
    }

    #[tokio::test]
    async fn requests_keep_their_shape_and_carry_the_token_as_bearer() {
        let endpoint = Arc::new(Scripted::answering(200).then(200, "{}").then(200, "{}"));
        let (client, _) = client_over(BUDGETS, &endpoint);

        client.get_portfolio("Main").await.expect("portfolio");
        client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 02 - 01))
            .await
            .expect("transactions");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received[0].destination(), Destination::FinamApi);
        assert_eq!(received[0].url(), "https://api.finam.ru/v1/accounts/Main");
        assert_eq!(
            received[1].url(),
            "https://api.finam.ru/v1/accounts/Main/transactions\
             ?interval%2Estart%5Ftime=2024%2D01%2D01T00%3A00%3A00Z\
             &interval%2Eend%5Ftime=2024%2D02%2D01T00%3A00%3A00Z"
        );
        for request in received.iter() {
            assert_eq!(request.bearer().map(|secret| secret.expose()), Some(TOKEN));
        }
    }

    #[tokio::test]
    async fn a_client_that_cannot_be_built_is_a_network_refusal_not_retried() {
        let endpoint = Arc::new(
            Scripted::answering(200).then_fault(HttpError::ClientNotBuilt("invented".to_owned())),
        );
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("no client, no request");

        assert_eq!(error, FinamError::Network);
        assert_eq!(endpoint.received.lock().expect("received").len(), 1);
    }

    #[tokio::test]
    async fn a_network_fault_to_the_end_is_unavailable_with_no_status() {
        let endpoint = Arc::new(Scripted::answering(200));
        for _ in 0..iaam_http::gateway::ATTEMPTS {
            endpoint
                .script
                .lock()
                .expect("script")
                .push_back(Err(HttpError::Network));
        }
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("every attempt faults");

        assert!(
            matches!(
                error,
                FinamError::Unavailable { status: None, attempts, .. }
                    if attempts == iaam_http::gateway::ATTEMPTS
            ),
            "{error:?}"
        );
    }

    /// A table with no Finam row: a build fault the gateway catches.
    static NO_FINAM: &[Budget] = &[Budget {
        destination: Destination::MoexIss,
        scope: MethodScope::Shared,
        documented: None,
        used: 1,
        window: Duration::from_secs(1),
    }];

    #[tokio::test]
    async fn a_missing_budget_row_is_a_gateway_refusal_and_sends_nothing() {
        let endpoint = Arc::new(Scripted::answering(200));
        let (client, _) = client_over(NO_FINAM, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("no budget, no request");

        assert!(matches!(error, FinamError::Gateway { .. }), "{error:?}");
        assert!(endpoint.received.lock().expect("received").is_empty());
        assert_no_token(&error);
    }

    /// One Finam request a minute per method: tight enough to show which
    /// budget a call draws on.
    static ONE_PER_METHOD: &[Budget] = &[Budget {
        destination: Destination::FinamApi,
        scope: MethodScope::EachMethod,
        documented: Some(200),
        used: 1,
        window: Duration::from_secs(60),
    }];

    #[tokio::test]
    async fn each_method_draws_on_its_own_budget() {
        let endpoint = Arc::new(Scripted::answering(200).then(200, "{}").then(200, "{}"));
        let (client, time) = client_over(ONE_PER_METHOD, &endpoint);

        client.get_portfolio("Main").await.expect("portfolio");
        client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 02 - 01))
            .await
            .expect("transactions");
        assert!(time.slept().is_empty(), "two methods, two budgets");

        client.get_portfolio("Main").await.expect("portfolio again");
        assert_eq!(time.slept(), vec![Duration::from_secs(60)]);
    }

    #[test]
    fn classifies_auth_and_unexpected_statuses() {
        assert!(matches!(
            classify_rejection(401, b"", "secret"),
            FinamError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(403, b"", "secret"),
            FinamError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(404, b"failure", "secret"),
            FinamError::UnexpectedStatus { status: 404, .. }
        ));
    }

    #[test]
    fn unexpected_status_never_prints_the_token() {
        let error = classify_rejection(404, b"upstream secret", "secret");
        assert!(!error.to_string().contains("secret"));
        assert!(!format!("{error:?}").contains("secret"));
    }

    #[test]
    fn refuses_a_page_that_claims_more_without_a_token() {
        assert!(matches!(
            validate_transactions_page(r#"{"hasMore":true,"transactions":[]}"#),
            Err(FinamError::PartialResponse)
        ));
    }
}
