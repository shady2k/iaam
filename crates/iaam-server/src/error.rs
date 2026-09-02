//! Error responses.
//!
//! A validation error is a `422` specifying the field, expected and received
//! values (§13). An invariant violation is returned externally as a `500`
//! with a correlation identifier and **without** a number: a result cannot be returned
//! after a proven violation of the identity (§15.2).

use axum::Json;
use axum::body::{Body, Bytes};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue, VARY, WWW_AUTHENTICATE};
use axum::response::{IntoResponse, Response};
use iaam_app::error::AppError;
use serde::Serialize;
use utoipa::ToSchema;

const UNAUTHORIZED_BODY: &[u8] = br#"{"code":"unauthorized","message":"a token is issued at the console by iaam claim --label <label>; no API route issues one"}"#;

/// Error response body.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiError {
    /// Machine-readable code. The agent parses this, not the text.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Request field that caused the rejection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// Correlation identifier: used to find the invariant violation
    /// in the logs. Nothing else is returned externally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl ApiError {
    #[must_use]
    pub fn simple(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            field: None,
            expected: None,
            actual: None,
            correlation_id: None,
        }
    }
}

/// Handler error.
///
/// Ordinary errors keep their structured body boxed so successful handler paths
/// do not carry a large result variant. Authentication refusals use the static
/// serialised body because the missing-header path is not rate-limited.
#[derive(Debug)]
pub struct ApiFailure {
    pub status: StatusCode,
    body: ApiFailureBody,
    challenge: &'static str,
}

#[derive(Debug)]
enum ApiFailureBody {
    Json(Box<ApiError>),
    Static(&'static [u8]),
}

impl ApiFailure {
    #[must_use]
    pub fn new(status: StatusCode, body: ApiError) -> Self {
        Self {
            status,
            body: ApiFailureBody::Json(Box::new(body)),
            challenge: "",
        }
    }

    /// Nothing was presented.
    ///
    /// The bare challenge is the correct one here: RFC 6750 `invalid_token`
    /// describes a token that was supplied and refused, and there is no token
    /// to describe.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self::refusal("Bearer")
    }

    /// A credential was presented and rejected.
    #[must_use]
    pub fn invalid_token() -> Self {
        Self::refusal("Bearer error=\"invalid_token\"")
    }

    /// Both refusals carry the same body: which of the two occurred is the
    /// caller's business, and telling a stranger whether a guessed token exists
    /// is not something the body should do. The challenge differs because the
    /// protocol requires it of the response, not of the explanation.
    fn refusal(challenge: &'static str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: ApiFailureBody::Static(UNAUTHORIZED_BODY),
            challenge,
        }
    }

    #[must_use]
    pub fn forbidden(scope: &str) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ApiError::simple(
                "forbidden",
                format!("the token's permissions ({scope}) do not allow this operation"),
            ),
        )
    }

    #[must_use]
    pub fn too_many_requests() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ApiError::simple("rate_limited", "too many requests"),
        )
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let mut response = match self.body {
            ApiFailureBody::Json(body) => (self.status, Json(*body)).into_response(),
            ApiFailureBody::Static(body) => Response::builder()
                .status(self.status)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(Bytes::from_static(body)))
                .expect("static error response is valid"),
        };
        if !self.challenge.is_empty() {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static(self.challenge));
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
                .headers_mut()
                .insert(VARY, HeaderValue::from_static("Authorization"));
        }
        response
    }
}

impl From<AppError> for ApiFailure {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Invalid {
                ref field,
                ref expected,
                ref actual,
            } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    field: Some(field.clone()),
                    expected: Some(expected.clone()),
                    actual: Some(actual.clone()),
                    correlation_id: None,
                },
            ),
            AppError::CategoryGroupRetired { ref id } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: "invalid_request".into(),
                    message: error.to_string(),
                    field: Some("group".into()),
                    expected: Some("an active category group".into()),
                    actual: Some(id.clone()),
                    correlation_id: None,
                },
            ),
            // An active record already exists: retrying the request will not replace it,
            // and a `500` would send the owner looking for a fault instead of revoking
            // the old record.
            AppError::Conflict { ref what } => Self::new(
                StatusCode::CONFLICT,
                ApiError::simple("already_exists", what.clone()),
            ),
            AppError::NotFound { what, ref id } => Self::new(
                StatusCode::NOT_FOUND,
                ApiError::simple("not_found", format!("not found: {what} {id}")),
            ),
            AppError::Invariant { correlation, .. } => {
                // Details remain in the log: only the code
                // and correlation identifier are returned externally.
                tracing::error!(%correlation, error = %error, "projection invariant violated");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError {
                        code: "invariant_violated".into(),
                        message: "result cannot be returned: internal invariant violated"
                            .into(),
                        field: None,
                        expected: None,
                        actual: None,
                        correlation_id: Some(correlation.to_string()),
                    },
                )
            }
            AppError::DirectoryInvariant { correlation, .. } => {
                // Details remain in the log: only the code
                // and correlation identifier are returned externally.
                tracing::error!(%correlation, error = %error, "reference-data invariant violated");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError {
                        code: "directory_invariant_violated".into(),
                        message: "result cannot be returned: reference-data invariant violated"
                            .into(),
                        field: None,
                        expected: None,
                        actual: None,
                        correlation_id: Some(correlation.to_string()),
                    },
                )
            }
            // The feature is not enabled by configuration, rather than broken: retrying
            // the request will not fix it, so use 503 stating what
            // exactly to set. The text names the environment variable:
            // «service unavailable» without a reason cannot be acted upon.
            AppError::NotConfigured { what } => {
                let message = if what == "broker access encryption" {
                    format!("{what} is not configured: set IAAM_BROKER_KEY_FILE and restart the server")
                } else {
                    format!("{what} is not configured")
                };
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    ApiError::simple("not_configured", message),
                )
            }
            AppError::Store(_)
            | AppError::Projection(_)
            // Money-flow arithmetic overflow makes the journal slice unusable:
            // it is our defect, not the request's, so only the code goes out and
            // the detail stays in the log.
            | AppError::MoneyFlow(_)
            // Reconciliation and perimeter assessment fail for the same reason,
            // as projection: the journal slice is unusable. Only the
            // code is returned externally, with details in the log.
            | AppError::Reconciliation(_)
            | AppError::Perimeter(_)
            // A journal whose correction links do not resolve is unusable for the
            // same reason, and it is our defect rather than the request's.
            | AppError::Correction(_)
            // The scheduler failed while listing active documents: this is a
            // server failure, not a request error.
            | AppError::Schedule(_)
            // Failure of the randomness source is also a `500`: no secret was issued,
            // and this is a server failure, not a request error. Retrying the request
            // makes sense, but substituting a fallback generator does not,
            // therefore a denial is returned rather than a token (§14).
            | AppError::Random(_) => {
                tracing::error!(error = %error, "scenario was not completed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError::simple(error.code(), error.to_string()),
                )
            }
        }
    }
}
