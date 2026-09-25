use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::credentials::BrokerToken;
use crate::environment::{Environment, Method};
use iaam_http::gateway::Transport;
use iaam_http::{Destination, Gateway, GatewayError, HttpRequest, HttpResponse, RequestBody};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

/// T-Invest HTTP gateway errors.
///
/// Variants contain no token: even an accidental `Debug` or `Display` of the
/// error must not turn a remote refusal into an access leak.
#[derive(Debug, Error)]
pub enum TinkoffError {
    /// T-Invest failed transiently — a 5xx, a 429, a network fault, an open
    /// breaker, a deadline — and went on failing through the gateway's
    /// retries. The same call is worth making again after `retry_after`.
    #[error(
        "T-Invest is unreachable after {attempts} attempts (last status {status:?}); retry after {retry_after:?}"
    )]
    Unreachable {
        /// The last status T-Invest answered with; `None` when it did not answer.
        status: Option<u16>,
        /// Requests actually sent for this call.
        attempts: u32,
        /// When trying again is worth it.
        retry_after: Duration,
    },
    /// The gateway rejected the presented token.
    #[error("T-Invest token is invalid")]
    InvalidToken,
    /// The selected environment does not provide this method.
    #[error("method {method:?} is unavailable in environment {environment:?}")]
    MethodUnavailable {
        /// Method unavailable in this environment.
        method: Method,
        /// Selected gateway environment.
        environment: Environment,
    },
    /// The gateway returned a code the client cannot accept.
    #[error("unexpected HTTP status {status}: {body}")]
    UnexpectedStatus {
        /// HTTP status code.
        status: u16,
        /// Response body after removing the presented token.
        body: String,
    },
    /// A paginated response cannot be fetched further.
    #[error("paginated response is truncated: gateway reported a next item without a cursor")]
    PartialResponse,
    /// A successful response does not match the method's minimum schema.
    #[error("T-Invest gateway response could not be parsed")]
    MalformedResponse,
    /// The request could not be serialized to JSON before sending.
    #[error("could not serialize request to the T-Invest gateway")]
    RequestSerialization,
    /// The transport could not be set up; retrying would meet the same fault.
    ///
    /// Contains no token: `HttpError` is designed not to contain one.
    #[error(transparent)]
    Transport(#[from] iaam_http::HttpError),
    /// The outbound gateway refused the call before sending it: a method key
    /// without a budget, or a budget table that fails its own checks. A
    /// fault of this build, not of T-Invest.
    #[error("the outbound gateway refused the call: {0}")]
    Gateway(GatewayError),
}

/// The outbound gateway, as the T-Invest client sees it.
///
/// A trait object rather than a type parameter: the gateway is built once
/// per process over the production transport and handed down as it is, while
/// a test hands down one over a scripted T-Invest on a fake clock, and the
/// channel factory that opens clients should not have to become generic to
/// allow both.
pub trait OutboundGateway: Send + Sync {
    /// `Gateway::send`, boxed.
    fn send<'a>(
        &'a self,
        method: &'static str,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>>;
}

impl<T: Transport> OutboundGateway for Gateway<T> {
    fn send<'a>(
        &'a self,
        method: &'static str,
        request: &'a HttpRequest,
        deadline: Option<Instant>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, GatewayError>> + Send + 'a>> {
        Box::pin(Self::send(self, method, request, deadline))
    }
}

/// The header T-Invest is believed to name its limit's reset in. Declaring a
/// header it does not send costs nothing: the retry falls back to backoff.
const RESET_HEADER: &str = "x-ratelimit-reset";

/// Request an operations page with cursor pagination.
///
/// Dates and enum values remain strings: this layer handles transport, while
/// field meanings and operation parsing belong to the next layer.
#[derive(Debug, Clone, Serialize)]
pub struct GetOperationsByCursorRequest {
    /// Account identifier.
    #[serde(rename = "accountId")]
    pub account_id: String,
    /// FIGI or UID of the instrument.
    #[serde(rename = "instrumentId", skip_serializing_if = "Option::is_none")]
    pub instrument_id: Option<String>,
    /// Period start in UTC, formatted as RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Period end in UTC, formatted as RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Cursor at the start of the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Page-size limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    /// Filter by operation kinds.
    #[serde(rename = "operationTypes", skip_serializing_if = "Vec::is_empty")]
    pub operation_types: Vec<String>,
    /// Filter by operation state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Do not return commissions.
    #[serde(rename = "withoutCommissions")]
    pub without_commissions: bool,
    /// Do not return trades.
    #[serde(rename = "withoutTrades")]
    pub without_trades: bool,
    /// Do not return overnight operations.
    #[serde(rename = "withoutOvernights")]
    pub without_overnights: bool,
}

impl GetOperationsByCursorRequest {
    /// Create the first account page request with gateway defaults.
    #[must_use]
    pub fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            instrument_id: None,
            from: None,
            to: None,
            cursor: None,
            limit: None,
            operation_types: Vec::new(),
            state: None,
            without_commissions: false,
            without_trades: false,
            without_overnights: false,
        }
    }
}

/// HTTP client for T-Invest REST API methods.
///
/// Sends every request through the process's one outbound gateway, which
/// paces it under the budget of its service and retries transient failures.
pub struct TinkoffClient {
    environment: Environment,
    token: BrokerToken,
    gateway: Arc<dyn OutboundGateway>,
}

impl TinkoffClient {
    /// Create a client over the process's gateway.
    #[must_use]
    pub fn new(
        environment: Environment,
        token: BrokerToken,
        gateway: Arc<dyn OutboundGateway>,
    ) -> Self {
        Self {
            environment,
            token,
            gateway,
        }
    }

    /// Return the raw response body from `UsersService/GetAccounts`.
    pub async fn get_accounts(&self) -> Result<String, TinkoffError> {
        self.post(
            Method::Accounts,
            "UsersService",
            "UsersService/GetAccounts",
            json!({}),
            None,
        )
        .await
    }

    /// Return the raw response body from `OperationsService/GetPortfolio`.
    ///
    /// No attempt starts, and no wait for one runs, past `deadline`.
    pub async fn get_portfolio(
        &self,
        account_id: &str,
        deadline: Option<Instant>,
    ) -> Result<String, TinkoffError> {
        self.post(
            Method::Portfolio,
            "OperationsService",
            "OperationsService/GetPortfolio",
            json!({ "accountId": account_id }),
            deadline,
        )
        .await
    }

    /// Return the raw page body from `OperationsService/GetOperationsByCursor`.
    ///
    /// No attempt starts, and no wait for one runs, past `deadline`.
    pub async fn get_operations_by_cursor(
        &self,
        request: &GetOperationsByCursorRequest,
        deadline: Option<Instant>,
    ) -> Result<String, TinkoffError> {
        let body = self
            .post(
                Method::Operations,
                "OperationsService",
                "OperationsService/GetOperationsByCursor",
                serde_json::to_value(request).map_err(|_| TinkoffError::RequestSerialization)?,
                deadline,
            )
            .await?;
        validate_cursor_page(&body)?;
        Ok(body)
    }

    /// `service` names the budget the call draws on, as T-Invest states its
    /// limits: per service, not per method.
    async fn post(
        &self,
        method: Method,
        service: &'static str,
        path: &str,
        body: Value,
        deadline: Option<Instant>,
    ) -> Result<String, TinkoffError> {
        ensure_method_available(self.environment, method)?;
        let body = serde_json::to_string(&body).map_err(|_| TinkoffError::RequestSerialization)?;
        let request = Self::request(self.environment, path, body, self.token.expose());
        let response = self
            .gateway
            .send(service, &request, deadline)
            .await
            .map_err(|error| gateway_error(error, self.token.expose()))?;
        String::from_utf8(response.body).map_err(|_| TinkoffError::MalformedResponse)
    }

    // The environment supplies the base through `Environment`, not
    // `Destination`: sandbox and production are different addresses for
    // one destination, and share a trust anchor.
    fn request(environment: Environment, path: &str, body: String, token: &str) -> HttpRequest {
        HttpRequest::post(destination_for(environment), path, RequestBody::Json(body))
            .with_bearer(token)
            .with_reset_header(RESET_HEADER)
    }
}

/// The environment selects the destination, not a URL suffix.
///
/// Sandbox and production have **different hosts**
/// (`sandbox-invest-public-api.tbank.ru` versus
/// `invest-public-api.tbank.ru`), so substituting one by trimming the base is
/// impossible—the request would go to the wrong place and receive a plausible
/// response from another environment.
const fn destination_for(environment: Environment) -> Destination {
    match environment {
        Environment::Prod => Destination::TinkoffProd,
        Environment::Sandbox => Destination::TinkoffSandbox,
    }
}
fn ensure_method_available(environment: Environment, method: Method) -> Result<(), TinkoffError> {
    if environment.serves(method) {
        Ok(())
    } else {
        Err(TinkoffError::MethodUnavailable {
            method,
            environment,
        })
    }
}

/// What a gateway refusal means for T-Invest.
///
/// A transient failure the gateway gave up on is "unreachable, retry later";
/// only a permanent refusal is read as T-Invest's own answer.
fn gateway_error(error: GatewayError, token: &str) -> TinkoffError {
    if let Some(retry_after) = error.retry_after() {
        return TinkoffError::Unreachable {
            status: error.status(),
            attempts: error.attempts(),
            retry_after,
        };
    }
    match error {
        GatewayError::Rejected { status, body, .. } => {
            classify_rejection(status, &String::from_utf8_lossy(body.as_bytes()), token)
        }
        GatewayError::Transport { error, .. } => TinkoffError::Transport(error),
        other => TinkoffError::Gateway(other),
    }
}

/// A status a retry would only repeat: 401 and 403, or a token code in the
/// body, name the token; anything else is kept with its body, the token cut
/// out of it.
fn classify_rejection(status: u16, body: &str, token: &str) -> TinkoffError {
    match status {
        401 | 403 => TinkoffError::InvalidToken,
        _ if body_contains_token_code(body) => TinkoffError::InvalidToken,
        _ => TinkoffError::UnexpectedStatus {
            status,
            body: redact_token(body, token),
        },
    }
}

fn body_contains_token_code(body: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str(body) else {
        return false;
    };
    ["description", "code", "message"]
        .iter()
        .any(|field| fields.get(*field).is_some_and(value_is_token_code))
}

fn value_is_token_code(value: &Value) -> bool {
    match value {
        Value::String(value) => value == "40003" || value == "70001",
        Value::Number(value) => value.to_string() == "40003" || value.to_string() == "70001",
        Value::Array(_) | Value::Object(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn redact_token(body: &str, token: &str) -> String {
    if token.is_empty() {
        body.to_owned()
    } else {
        body.replace(token, "<token hidden>")
    }
}

#[derive(Deserialize)]
struct CursorPage {
    #[serde(rename = "hasNext", alias = "has_next")]
    has_next: bool,
    #[serde(rename = "nextCursor", alias = "next_cursor")]
    next_cursor: Option<String>,
}

fn validate_cursor_page(body: &str) -> Result<(), TinkoffError> {
    let page: CursorPage =
        serde_json::from_str(body).map_err(|_| TinkoffError::MalformedResponse)?;
    if page.has_next && page.next_cursor.as_deref().is_none_or(str::is_empty) {
        return Err(TinkoffError::PartialResponse);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use iaam_http::gateway::{ATTEMPTS, BUDGETS, Clock, FIRST_BACKOFF, Sleeper};
    use iaam_http::resilience::{Outcome, RetryPolicy};
    use iaam_http::{Gateway, HttpError, HttpResponse};

    use super::*;
    use crate::credentials::{Key, open, seal};

    const TOKEN: &str = "secret-token-42";

    /// A clock that moves only when something sleeps on it.
    struct FakeTime {
        now: Mutex<Instant>,
        slept: Mutex<Vec<Duration>>,
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

    /// A T-Invest that answers from a script and remembers what it was asked.
    struct FakeTinvest {
        script: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
        paths: Arc<Mutex<Vec<String>>>,
    }

    impl Transport for FakeTinvest {
        async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            self.paths.lock().expect("paths").push(request.url());
            self.script
                .lock()
                .expect("script")
                .pop_front()
                .expect("the script ran out: more requests than expected")
        }
    }

    fn answer(status: u16, body: &str) -> Result<HttpResponse, HttpError> {
        Ok(HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
            retry_after: None,
        })
    }

    fn client(
        answers: Vec<Result<HttpResponse, HttpError>>,
    ) -> (TinkoffClient, Arc<Mutex<Vec<String>>>, Arc<FakeTime>) {
        let time = Arc::new(FakeTime {
            now: Mutex::new(Instant::now()),
            slept: Mutex::new(Vec::new()),
        });
        let paths = Arc::new(Mutex::new(Vec::new()));
        let gateway = Arc::new(
            Gateway::with_parts(
                FakeTinvest {
                    script: Mutex::new(answers.into()),
                    paths: Arc::clone(&paths),
                },
                BUDGETS,
                Arc::clone(&time) as Arc<dyn Clock>,
                Arc::clone(&time) as Arc<dyn Sleeper>,
            )
            .expect("the documented table is valid"),
        );
        let key = Key::from_bytes([3; 32]);
        let token = open(&key, &seal(&key, TOKEN)).expect("token opens");
        let client = TinkoffClient::new(Environment::Prod, token, gateway);
        (client, paths, time)
    }

    fn sent(paths: &Mutex<Vec<String>>) -> usize {
        paths.lock().expect("paths").len()
    }

    #[tokio::test]
    async fn a_rejected_token_is_not_retried_and_its_error_carries_no_token() {
        let (client, gateway, time) =
            client(vec![answer(401, &format!(r#"{{"message":"{TOKEN}"}}"#))]);

        let error = client.get_accounts().await.expect_err("401 is a refusal");

        assert!(matches!(error, TinkoffError::InvalidToken), "{error:?}");
        assert_eq!(sent(&gateway), 1, "a rejected token was sent again");
        assert!(time.slept.lock().expect("sleeps").is_empty());
        assert!(!error.to_string().contains(TOKEN));
        assert!(!format!("{error:?}").contains(TOKEN));
    }

    #[tokio::test]
    async fn a_forbidden_answer_is_an_invalid_token_too() {
        let (client, gateway, _) = client(vec![answer(403, "{}")]);

        let error = client.get_accounts().await.expect_err("403 is a refusal");

        assert!(matches!(error, TinkoffError::InvalidToken), "{error:?}");
        assert_eq!(sent(&gateway), 1);
    }

    #[tokio::test]
    async fn a_server_error_is_retried_until_it_passes() {
        let (client, gateway, _) = client(vec![
            answer(500, "{}"),
            answer(500, "{}"),
            answer(200, r#"{"accounts":[]}"#),
        ]);

        let body = client
            .get_accounts()
            .await
            .expect("the third attempt passes");

        assert_eq!(body, r#"{"accounts":[]}"#);
        assert_eq!(sent(&gateway), 3);
    }

    /// A transient failure that outlasts the retries is "unreachable, try
    /// again later", with the time to try again, not a refusal.
    #[tokio::test]
    async fn a_server_error_that_outlasts_the_retries_is_unreachable_with_a_time() {
        let answers = (0..ATTEMPTS).map(|_| answer(503, "{}")).collect();
        let (client, gateway, _) = client(answers);

        let error = client
            .get_accounts()
            .await
            .expect_err("every attempt failed");

        let expected =
            RetryPolicy::new(ATTEMPTS, FIRST_BACKOFF).delay(ATTEMPTS, &Outcome::status(503));
        assert!(
            matches!(
                error,
                TinkoffError::Unreachable {
                    status: Some(503),
                    attempts: ATTEMPTS,
                    retry_after,
                } if retry_after == expected
            ),
            "{error:?}"
        );
        assert_eq!(sent(&gateway), ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn a_network_failure_that_outlasts_the_retries_is_unreachable() {
        let answers = (0..ATTEMPTS).map(|_| Err(HttpError::Network)).collect();
        let (client, _, _) = client(answers);

        let error = client
            .get_accounts()
            .await
            .expect_err("every attempt failed");

        assert!(
            matches!(error, TinkoffError::Unreachable { status: None, .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_transport_that_cannot_be_built_stays_a_transport_error() {
        let (client, _, _) = client(vec![Err(HttpError::ClientNotBuilt("no".to_owned()))]);

        let error = client.get_accounts().await.expect_err("nothing was sent");

        assert!(matches!(error, TinkoffError::Transport(_)), "{error:?}");
    }

    #[tokio::test]
    async fn another_client_error_is_an_unexpected_status_with_the_token_hidden() {
        let (client, _, _) = client(vec![answer(
            400,
            &format!(r#"{{"message":"bad field near {TOKEN}"}}"#),
        )]);

        let error = client
            .get_portfolio("account", None)
            .await
            .expect_err("400");

        assert!(
            matches!(&error, TinkoffError::UnexpectedStatus { status: 400, body }
                if body.contains("bad field") && !body.contains(TOKEN)),
            "{error:?}"
        );
    }

    /// Each method draws on the budget T-Invest states for its service.
    #[tokio::test]
    async fn each_method_names_its_service_budget() {
        let answers = (0..51)
            .map(|_| answer(200, r#"{"hasNext":false,"items":[]}"#))
            .collect();
        let (client, _, time) = client(answers);

        for _ in 0..25 {
            client.get_accounts().await.expect("accounts");
        }
        for _ in 0..25 {
            client
                .get_portfolio("account", None)
                .await
                .expect("portfolio");
        }
        assert!(time.slept.lock().expect("sleeps").is_empty());
        client
            .get_operations_by_cursor(&GetOperationsByCursorRequest::new("account"), None)
            .await
            .expect("operations");
        assert!(
            time.slept.lock().expect("sleeps").is_empty(),
            "operations waited on the users budget"
        );
    }

    #[tokio::test]
    async fn an_operations_page_is_returned_as_it_came_and_a_truncated_one_refused() {
        let page = r#"{"hasNext":true,"nextCursor":"next","items":[]}"#;
        let (client, _, _) = client(vec![
            answer(200, page),
            answer(200, r#"{"hasNext":true,"items":[]}"#),
        ]);
        let request = GetOperationsByCursorRequest::new("account");

        assert_eq!(
            client
                .get_operations_by_cursor(&request, None)
                .await
                .expect("page"),
            page
        );
        assert!(matches!(
            client.get_operations_by_cursor(&request, None).await,
            Err(TinkoffError::PartialResponse)
        ));
    }

    #[tokio::test]
    async fn the_twenty_sixth_accounts_call_in_a_minute_waits() {
        let answers = (0..26).map(|_| answer(200, "{}")).collect();
        let (client, _, time) = client(answers);

        for _ in 0..26 {
            client.get_accounts().await.expect("accounts");
        }

        assert_eq!(
            *time.slept.lock().expect("sleeps"),
            [Duration::from_secs(60)]
        );
    }

    #[test]
    fn the_request_declares_the_reset_header_t_invest_sends() {
        let request = TinkoffClient::request(
            Environment::Prod,
            "UsersService/GetAccounts",
            "{}".to_owned(),
            TOKEN,
        );
        assert_eq!(request.reset_header(), Some("x-ratelimit-reset"));
    }

    #[test]
    fn classifies_rejections_and_token_codes() {
        assert!(matches!(
            classify_rejection(401, "{}", TOKEN),
            TinkoffError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(403, "{}", TOKEN),
            TinkoffError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(400, r#"{"description":"40003"}"#, TOKEN),
            TinkoffError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(400, r#"{"description":"70001"}"#, TOKEN),
            TinkoffError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(400, r#"{"code":70001}"#, TOKEN),
            TinkoffError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(400, r#"{"message":"40003"}"#, TOKEN),
            TinkoffError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(404, r#"{"data":{"quantity":70001}}"#, TOKEN),
            TinkoffError::UnexpectedStatus { status: 404, .. }
        ));
    }

    #[test]
    fn preserves_an_unexpected_status_body() {
        let error = classify_rejection(422, r#"{"message":"gateway failed"}"#, TOKEN);
        assert!(matches!(
            error,
            TinkoffError::UnexpectedStatus { status: 422, body }
                if body == r#"{"message":"gateway failed"}"#
        ));
    }

    #[test]
    fn an_empty_token_leaves_the_body_as_it_was() {
        assert_eq!(redact_token("body", ""), "body");
        assert_eq!(redact_token("a secret b", "secret"), "a <token hidden> b");
    }

    #[test]
    fn refuses_methods_absent_from_the_selected_environment() {
        let error = ensure_method_available(Environment::Sandbox, Method::BrokerReport)
            .expect_err("report is absent in the sandbox");
        assert!(matches!(error, TinkoffError::MethodUnavailable { .. }));
        assert!(ensure_method_available(Environment::Sandbox, Method::Portfolio).is_ok());
    }

    fn method_url(environment: Environment, path: &str) -> String {
        HttpRequest::post(
            destination_for(environment),
            path,
            RequestBody::Json("{}".to_owned()),
        )
        .url()
    }
    #[test]
    fn builds_method_url_from_environment_base_url() {
        assert_eq!(
            method_url(Environment::Prod, "OperationsService/GetPortfolio"),
            "https://invest-public-api.tbank.ru/rest/OperationsService/GetPortfolio"
        );
        assert_eq!(
            method_url(Environment::Sandbox, "UsersService/GetAccounts"),
            "https://sandbox-invest-public-api.tbank.ru/rest/UsersService/GetAccounts"
        );
    }

    #[test]
    fn rejects_an_incomplete_cursor_page() {
        assert!(matches!(
            validate_cursor_page(r#"{"hasNext":true,"items":[]}"#),
            Err(TinkoffError::PartialResponse)
        ));
        assert!(matches!(
            validate_cursor_page(r#"{"hasNext":true,"nextCursor":""}"#),
            Err(TinkoffError::PartialResponse)
        ));
        assert!(validate_cursor_page(r#"{"hasNext":true,"nextCursor":"next"}"#).is_ok());
        assert!(validate_cursor_page(r#"{"hasNext":false,"items":[]}"#).is_ok());
    }
}
