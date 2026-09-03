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
use iaam_app::error::{AppError, FieldRejection, json_pointer};
use serde::Serialize;
use utoipa::ToSchema;

use crate::action_catalog::ActionCatalog;
use crate::dto::{InputAlternativeDto, ResolutionOptionDto};
use crate::routes::{input_alternative_dto, resolution_option_dto};

const UNAUTHORIZED_BODY: &[u8] = br#"{"code":"unauthorized","message":"a token is issued at the console by iaam claim --label <label>; no API route issues one"}"#;

/// Error response body.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiError {
    /// Machine-readable code. The agent parses this, not the text.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Request field that caused the rejection, in the dotted-and-indexed form
    /// a person reads: `event[3].schema_version`.
    ///
    /// Kept beside `pointer` rather than replaced by it, for two reasons. It is
    /// the string `message` quotes, so dropping it would leave the sentence
    /// naming something the payload no longer contains; and it is the form the
    /// specification, the action queue's prose and the operator's own notes all
    /// use, so a payload carrying only the pointer would rename a field that has
    /// a name. `pointer` is the same fact addressed mechanically — the two are
    /// derived from one string at one place and cannot disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The same field as an RFC 6901 JSON pointer: `/event/3/schema_version`.
    ///
    /// What makes a rejection actionable without reading documentation. A client
    /// holds the body it just sent; a pointer applies to that body directly,
    /// while `event[3].schema_version` first has to be parsed into a path, and
    /// the parsing is a place to be wrong about — whether `[3]` is an index or a
    /// member so named, whether a dot separates members or belongs to one.
    ///
    /// Present wherever `field` is, and absent with it. It addresses the request
    /// as sent, so a rejection about something that is not a body member — a
    /// query parameter, a token's scope — points where the field name says, and
    /// no further.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// What the field admits, in prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// What arrived. Never more of the request than `field` names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// The literal values the field admits, when it admits a closed set.
    ///
    /// The same [`InputAlternativeDto`] the action queue publishes for a missing
    /// input, and deliberately not a vocabulary of this payload's own: a field
    /// whose values appear both in the queue and in a refusal must not be
    /// described two ways.
    ///
    /// Absent where the field is not a choice, which is most fields and every
    /// date, title and amount. A rejection that always carried a list would have
    /// to invent one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<InputAlternativeDto>,
    /// Calls that would produce a value this field accepts.
    ///
    /// For the rejections whose remedy is a request rather than a value: no
    /// string written into the field would settle it, and the caller has to go
    /// somewhere else first. Carried in the [`ResolutionOptionDto`] an action's
    /// target uses, so the operation, its address and the fields it still wants
    /// read exactly as they do in the queue.
    ///
    /// Absent almost always, and absent honestly: most refusals have no next
    /// call, and one manufactured to fill the field would send a caller to a
    /// route that cannot help it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolutions: Vec<ResolutionOptionDto>,
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
            pointer: None,
            expected: None,
            actual: None,
            alternatives: Vec::new(),
            resolutions: Vec::new(),
            correlation_id: None,
        }
    }

    /// Name the field the rejection is about, and derive its pointer.
    ///
    /// The only way `field` is set anywhere in this crate, which is what keeps
    /// the pointer honest: a rejection cannot name one field in prose and
    /// address another mechanically, because it never writes the pointer itself.
    #[must_use]
    pub fn about(mut self, field: impl Into<String>) -> Self {
        let field = field.into();
        self.pointer = Some(json_pointer(&field));
        self.field = Some(field);
        self
    }

    /// What the field admits, in prose.
    #[must_use]
    pub fn expecting(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }

    /// What arrived.
    ///
    /// Separate from [`Self::expecting`] because a field can be missing: there
    /// is then an expectation and nothing to quote, and a rejection that had to
    /// supply both would quote a neighbouring value instead.
    #[must_use]
    pub fn receiving(mut self, actual: impl Into<String>) -> Self {
        self.actual = Some(actual.into());
        self
    }

    /// The closed set of values the field admits.
    #[must_use]
    pub fn admitting(mut self, alternatives: Vec<InputAlternativeDto>) -> Self {
        self.alternatives = alternatives;
        self
    }

    /// The calls that would produce an acceptable value.
    #[must_use]
    pub fn resolved_by(mut self, resolutions: Vec<ResolutionOptionDto>) -> Self {
        self.resolutions = resolutions;
        self
    }

    /// The identifier the log entry for this failure carries.
    #[must_use]
    pub fn correlated(mut self, correlation: impl std::fmt::Display) -> Self {
        self.correlation_id = Some(correlation.to_string());
        self
    }
}

/// The body of a field rejection, with whatever of it the transport can address.
fn field_rejection_body(
    rejection: &FieldRejection,
    message: String,
    catalog: Option<&ActionCatalog>,
) -> ApiError {
    let resolutions = match catalog {
        Some(catalog) => rejection
            .resolutions
            .iter()
            .map(|resolution| {
                resolution_option_dto(resolution.operation, &resolution.request, catalog)
            })
            .collect(),
        // A route that can refuse with a next call resolves it through
        // `ApiFailure::from_app`. The blanket conversion has no catalog to
        // reach and drops the call rather than inventing an address for it: a
        // dropped call costs the caller a lookup in the queue, an invented one
        // costs it a request to a route that is not there.
        None => Vec::new(),
    };
    ApiError::simple("invalid_request", message)
        .about(rejection.field.clone())
        .expecting(rejection.expected.clone())
        .receiving(rejection.actual.clone())
        .admitting(
            rejection
                .alternatives
                .iter()
                .map(input_alternative_dto)
                .collect(),
        )
        .resolved_by(resolutions)
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
        Self::render(error, None)
    }
}

impl ApiFailure {
    /// Convert an application error, resolving any next call it offers.
    ///
    /// The catalog is what turns an [`iaam_app::actions::OperationKey`] into a
    /// route a caller can send to, and it reaches a handler as an extension —
    /// which the blanket [`From`] conversion, running wherever a `?` happens to
    /// be, cannot ask for. So a route that can refuse with a remedy converts
    /// through here; every other route keeps using `?` and loses nothing,
    /// because every other refusal has no remedy to lose.
    #[must_use]
    pub fn from_app(error: AppError, catalog: &ActionCatalog) -> Self {
        Self::render(error, Some(catalog))
    }

    fn render(error: AppError, catalog: Option<&ActionCatalog>) -> Self {
        match error {
            AppError::Invalid {
                ref field,
                ref expected,
                ref actual,
            } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::simple(error.code(), error.to_string())
                    .about(field.clone())
                    .expecting(expected.clone())
                    .receiving(actual.clone()),
            ),
            AppError::InvalidField(ref rejection) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                field_rejection_body(rejection, error.to_string(), catalog),
            ),
            AppError::CategoryGroupRetired { ref id } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::simple("invalid_request", error.to_string())
                    .about("group")
                    .expecting("an active category group")
                    .receiving(id.clone()),
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
                    ApiError::simple(
                        "invariant_violated",
                        "result cannot be returned: internal invariant violated",
                    )
                    .correlated(correlation),
                )
            }
            AppError::DirectoryInvariant { correlation, .. } => {
                // Details remain in the log: only the code
                // and correlation identifier are returned externally.
                tracing::error!(%correlation, error = %error, "reference-data invariant violated");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError::simple(
                        "directory_invariant_violated",
                        "result cannot be returned: reference-data invariant violated",
                    )
                    .correlated(correlation),
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
            // A total that would not add up is our defect too: the request named
            // a scope and a date, and neither of them can make two currencies
            // meet inside one figure.
            | AppError::AssetSnapshot(_)
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
