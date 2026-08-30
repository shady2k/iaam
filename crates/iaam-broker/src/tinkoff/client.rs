use crate::credentials::BrokerToken;
use crate::environment::{Environment, Method};
use iaam_http::client::HttpClient;
use iaam_http::{Destination, HttpRequest, RequestBody};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

/// T-Invest HTTP gateway errors.
///
/// Variants contain no token: even an accidental `Debug` or `Display` of the
/// error must not turn a remote refusal into an access leak.
#[derive(Debug, Error)]
pub enum TinkoffError {
    /// Connection was not established or the response body was not read fully.
    #[error("T-Invest gateway network refusal")]
    Network,
    /// The gateway temporarily rate-limited the request.
    #[error("T-Invest gateway rate-limited the request")]
    RateLimited,
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
    /// The transport did not deliver the request: network, timeout, or client construction.
    ///
    /// Contains no token: `HttpError` is designed not to contain one.
    #[error(transparent)]
    Transport(#[from] iaam_http::HttpError),
}

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
pub struct TinkoffClient {
    environment: Environment,
    token: BrokerToken,
    http: HttpClient,
}

impl TinkoffClient {
    /// Create a client with the same pinned trust root as the probe.
    pub fn new(environment: Environment, token: BrokerToken) -> Result<Self, TinkoffError> {
        Ok(Self {
            environment,
            token,
            http: HttpClient::new(),
        })
    }

    /// Return the raw response body from `UsersService/GetAccounts`.
    pub async fn get_accounts(&self) -> Result<String, TinkoffError> {
        self.post(Method::Accounts, "UsersService/GetAccounts", json!({}))
            .await
    }

    /// Return the raw response body from `OperationsService/GetPortfolio`.
    pub async fn get_portfolio(&self, account_id: &str) -> Result<String, TinkoffError> {
        self.post(
            Method::Portfolio,
            "OperationsService/GetPortfolio",
            json!({ "accountId": account_id }),
        )
        .await
    }

    /// Return the raw page body from `OperationsService/GetOperationsByCursor`.
    pub async fn get_operations_by_cursor(
        &self,
        request: &GetOperationsByCursorRequest,
    ) -> Result<String, TinkoffError> {
        let body = self
            .post(
                Method::Operations,
                "OperationsService/GetOperationsByCursor",
                serde_json::to_value(request).map_err(|_| TinkoffError::RequestSerialization)?,
            )
            .await?;
        validate_cursor_page(&body)?;
        Ok(body)
    }

    async fn post(&self, method: Method, path: &str, body: Value) -> Result<String, TinkoffError> {
        ensure_method_available(self.environment, method)?;
        // The environment supplies the base through `Environment`, not
        // `Destination`: sandbox and production are different addresses for
        // one destination, and share a trust anchor.
        let request = HttpRequest::post(
            destination_for(self.environment),
            path,
            RequestBody::Json(
                serde_json::to_string(&body).map_err(|_| TinkoffError::RequestSerialization)?,
            ),
        )
        .with_bearer(self.token.expose());
        let response = self.http.send(&request).await?;
        let body = String::from_utf8(response.body).map_err(|_| TinkoffError::MalformedResponse)?;
        classify_response_with_token(response.status, &body, self.token.expose())?;
        Ok(body)
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

fn classify_response(status: u16, body: &str) -> Result<(), TinkoffError> {
    match status {
        429 => Err(TinkoffError::RateLimited),
        401 | 403 => Err(TinkoffError::InvalidToken),
        200..=299 => Ok(()),
        _ if body_contains_token_code(body) => Err(TinkoffError::InvalidToken),
        _ => Err(TinkoffError::UnexpectedStatus {
            status,
            body: body.to_owned(),
        }),
    }
}

fn classify_response_with_token(status: u16, body: &str, token: &str) -> Result<(), TinkoffError> {
    match classify_response(status, body) {
        Err(TinkoffError::UnexpectedStatus { status, .. }) => Err(TinkoffError::UnexpectedStatus {
            status,
            body: redact_token(body, token),
        }),
        result => result,
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
    use super::*;

    #[test]
    fn classifies_gateway_statuses_and_token_codes() {
        assert!(matches!(
            classify_response(429, "{}"),
            Err(TinkoffError::RateLimited)
        ));
        assert!(matches!(
            classify_response(401, "{}"),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(matches!(
            classify_response(403, "{}"),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(matches!(
            classify_response(500, r#"{"description":"40003"}"#),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(matches!(
            classify_response(500, r#"{"description":"70001"}"#),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(classify_response(200, r#"{"code":"40003"}"#).is_ok());
        assert!(classify_response(200, r#"{"positions":[{"quantity":70001}]}"#).is_ok());
        assert!(matches!(
            classify_response(500, r#"{"code":70001}"#),
            Err(TinkoffError::InvalidToken)
        ));
        let error = classify_response(500, r#"{"data":{"quantity":70001}}"#)
            .expect_err("nested value is not a refusal code");
        assert!(matches!(
            error,
            TinkoffError::UnexpectedStatus { status: 500, .. }
        ));
    }

    #[test]
    fn preserves_unexpected_status_body_without_treating_success_as_error() {
        assert!(classify_response(200, r#"{"ok":true}"#).is_ok());
        let error = classify_response(500, r#"{"message":"gateway failed"}"#)
            .expect_err("unexpected code must be a refusal");
        assert!(matches!(
            error,
            TinkoffError::UnexpectedStatus { status: 500, body }
                if body == r#"{"message":"gateway failed"}"#
        ));
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

    #[test]
    fn error_text_never_contains_the_token() {
        let token = "secret-token-42";
        let error =
            classify_response_with_token(500, &format!(r#"{{"message":"{token}"}}"#), token)
                .expect_err("response code must be a refusal");
        assert!(!error.to_string().contains(token));
    }
}
