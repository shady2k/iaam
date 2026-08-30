//! Error responses.
//!
//! A validation error is a `422` specifying the field, expected and received
//! values (§13). An invariant violation is returned externally as a `500`
//! with a correlation identifier and **without** a number: a result cannot be returned
//! after a proven violation of the identity (§15.2).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use iaam_app::error::AppError;
use serde::Serialize;
use utoipa::ToSchema;

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
/// The body is in a `Box`: every handler returns `Result<T, ApiFailure>`,
/// and `clippy::result_large_err` rightly objects to an error variant
/// being 150 bytes in size on every successful path.
#[derive(Debug)]
pub struct ApiFailure {
    pub status: StatusCode,
    pub body: Box<ApiError>,
}

impl ApiFailure {
    #[must_use]
    pub fn new(status: StatusCode, body: ApiError) -> Self {
        Self {
            status,
            body: Box::new(body),
        }
    }

    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ApiError::simple("unauthorized", "a valid token is required"),
        )
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
        (self.status, Json(*self.body)).into_response()
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
            // Reconciliation and perimeter assessment fail for the same reason,
            // as projection: the journal slice is unusable. Only the
            // code is returned externally, with details in the log.
            | AppError::Reconciliation(_)
            | AppError::Perimeter(_)
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
