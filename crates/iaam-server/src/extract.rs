//! Extractors that refuse in the documented shape.
//!
//! axum's own `Query`, `Json`, `Path` and `Bytes` reject a request with a
//! `text/plain` body of their own devising. A client would then have to parse
//! two encodings of the same event, and — worse — the generated contract would
//! be describing a refusal the server does not actually send: every operation
//! declares `ApiError`, and `docs/agent-skill/SKILL.md` tells an agent to read
//! the contract instead of asking what a refusal looks like.
//!
//! The wrappers here delegate the parsing and translate the failure into
//! `ApiFailure`, so a rejected query parameter and a rejected body field come
//! back in the same shape as a validation error raised inside a handler.
//!
//! **What is never returned is the value that failed.** serde's own text quotes
//! it — `invalid type: string "…", expected u32` — and a rejected body is
//! exactly the kind of thing that carries the owner's data. Only names are
//! taken out of serde's message: the field it was reading, and the type it
//! wanted. Everything else is a sentence of ours.

use std::fmt::Display;

use axum::body::Bytes;
use axum::extract::path::ErrorKind;
use axum::extract::rejection::{BytesRejection, FailedToBufferBody, PathRejection};
use axum::extract::{FromRequest, FromRequestParts, Path, Request};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::{ApiError, ApiFailure};

/// Query parameters, refused as `ApiError`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiFailure;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        // The same deserialiser axum uses, so accepted requests are accepted
        // exactly as before; only the failure path differs.
        let deserializer =
            serde_urlencoded::Deserializer::new(form_urlencoded::parse(query.as_bytes()));
        serde_path_to_error::deserialize(deserializer)
            .map(Self)
            .map_err(|error| unreadable(Subject::QueryParameter, &error))
    }
}

/// A JSON body, refused as `ApiError`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiFailure;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json_content_type(request.headers()) {
            return Err(unsupported_media_type());
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(unreadable_body)?;
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        let value = serde_path_to_error::deserialize(&mut deserializer)
            .map_err(|error| unreadable(Subject::BodyField, &error))?;
        // Trailing content after the document is a malformed body, not a valid
        // one: accepting it would let two different requests mean the same
        // thing to us and different things to the client.
        deserializer
            .end()
            .map_err(|_| malformed_body("the body carries content after the JSON document"))?;
        Ok(Self(value))
    }
}

/// A path parameter, refused as `ApiError`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiPath<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiFailure;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(unreadable_path)
    }
}

/// A raw body, refused as `ApiError`.
///
/// The bodies this wraps are workbooks and CSV, parsed by the handler itself.
/// Only the buffering can fail here, and it is the same failure `ApiJson` has:
/// a body larger than the limit, or one that could not be read to the end.
#[derive(Debug, Clone, Default)]
pub struct ApiBytes(pub Bytes);

impl<S> FromRequest<S> for ApiBytes
where
    S: Send + Sync,
{
    type Rejection = ApiFailure;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Bytes::from_request(request, state)
            .await
            .map(Self)
            .map_err(unreadable_body)
    }
}

/// Which part of the request a failure is about. It reaches the caller only as
/// a word in the message; the field name is reported separately.
#[derive(Debug, Clone, Copy)]
enum Subject {
    QueryParameter,
    BodyField,
}

impl Subject {
    const fn noun(self) -> &'static str {
        match self {
            Self::QueryParameter => "query parameter",
            Self::BodyField => "body field",
        }
    }

    const fn whole(self) -> &'static str {
        match self {
            Self::QueryParameter => "the query string",
            Self::BodyField => "the body",
        }
    }
}

fn invalid_request(field: Option<String>, expected: Option<String>, message: String) -> ApiFailure {
    ApiFailure::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError {
            code: "invalid_request".to_owned(),
            message,
            field,
            expected,
            actual: None,
            correlation_id: None,
        },
    )
}

/// A `415`: there is nothing to deserialise, so `422` would be a lie about
/// what was wrong. The status differs; the shape does not.
fn unsupported_media_type() -> ApiFailure {
    ApiFailure::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ApiError {
            code: "unsupported_media_type".to_owned(),
            message: "a JSON body must be sent with Content-Type: application/json".to_owned(),
            field: None,
            expected: Some("application/json".to_owned()),
            actual: None,
            correlation_id: None,
        },
    )
}

fn malformed_body(message: &str) -> ApiFailure {
    ApiFailure::new(
        StatusCode::BAD_REQUEST,
        ApiError::simple("malformed_request", message.to_owned()),
    )
}

/// The body could not be buffered. Neither outcome is a deserialisation
/// failure, so neither is a `422`.
fn unreadable_body(rejection: BytesRejection) -> ApiFailure {
    match rejection {
        BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_)) => {
            ApiFailure::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                ApiError::simple("payload_too_large", "the request body exceeds the limit"),
            )
        }
        _ => malformed_body("the request body could not be read to the end"),
    }
}

/// A path parameter that does not parse.
///
/// `Path` distinguishes the caller's mistake from ours: a segment that will not
/// parse is a `422`, while a missing capture or an unsupported target type is a
/// defect in the route and stays a `500`.
fn unreadable_path(rejection: PathRejection) -> ApiFailure {
    let PathRejection::FailedToDeserializePathParams(failure) = &rejection else {
        return route_defect(&rejection);
    };
    // The value is in `kind` for most variants and is deliberately dropped:
    // it is the caller's, and naming what was expected is enough to act on.
    match failure.kind() {
        ErrorKind::ParseErrorAtKey {
            key, expected_type, ..
        } => invalid_request(
            Some(key.clone()),
            Some((*expected_type).to_owned()),
            format!("path parameter {key} could not be read"),
        ),
        ErrorKind::DeserializeError { key, .. } => invalid_request(
            Some(key.clone()),
            None,
            format!("path parameter {key} could not be read"),
        ),
        ErrorKind::InvalidUtf8InPathParam { key } => invalid_request(
            Some(key.clone()),
            Some("valid UTF-8".to_owned()),
            format!("path parameter {key} is not valid UTF-8"),
        ),
        _ => route_defect(&rejection),
    }
}

/// The route asked for something the request cannot supply: our defect, not the
/// caller's. Reported like any other invariant violation — a code, and the
/// detail left in the log.
fn route_defect(rejection: &PathRejection) -> ApiFailure {
    tracing::error!(rejection = %rejection, "path parameters do not match the route");
    ApiFailure::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        ApiError::simple(
            "invariant_violated",
            "result cannot be returned: the route's path parameters do not match its handler",
        ),
    )
}

/// Translates a deserialisation failure into the documented shape, taking from
/// serde only the names: the path it was reading and the type it wanted.
fn unreadable<E: Display>(subject: Subject, error: &serde_path_to_error::Error<E>) -> ApiFailure {
    let inner = error.inner().to_string();
    let inner = strip_position(&inner);
    let path = error.path().to_string();
    let path = (path != ".").then_some(path);

    if let Some(name) = missing_field(inner) {
        let field = match &path {
            Some(prefix) => format!("{prefix}.{name}"),
            None => name.to_owned(),
        };
        return invalid_request(
            Some(field.clone()),
            Some("a value".to_owned()),
            format!("required {} {field} is missing", subject.noun()),
        );
    }

    let message = match &path {
        Some(field) => format!("{} {field} could not be read", subject.noun()),
        None => format!("{} could not be read", subject.whole()),
    };
    invalid_request(path, expected_type(inner), message)
}

/// `serde` writes a missing field as ``missing field `contour` `` and has done
/// since the trait was stabilised. The name is ours — it is a field of our own
/// request type — so taking it is not taking the caller's data.
fn missing_field(message: &str) -> Option<&str> {
    message
        .strip_prefix("missing field `")
        .and_then(|rest| rest.split('`').next())
        .filter(|name| !name.is_empty())
}

/// The tail of ``invalid type: string "…", expected u32``.
///
/// `serde` puts the value first and the expectation last, so the **last**
/// separator is the one that divides them. The result is refused if it carries
/// a quote: an expectation is written by an `Expected` implementation and has
/// none, while a quoted fragment means the split landed inside a value.
fn expected_type(message: &str) -> Option<String> {
    const LIMIT: usize = 120;
    let expected = message.rsplit_once(", expected ")?.1.trim();
    if expected.is_empty() || expected.contains('"') || expected.len() > LIMIT {
        return None;
    }
    Some(expected.to_owned())
}

/// `serde_json` appends « at line 3 column 17» to every message. The position
/// is of no use to a caller that is not looking at the bytes it sent, and it
/// would have to be carried through every comparison below.
fn strip_position(message: &str) -> &str {
    match message.rfind(" at line ") {
        Some(index) => &message[..index],
        None => message,
    }
}

/// axum's own rule, without the `mime` dependency: `application/json`, or any
/// `application/…+json`, with parameters such as `charset` ignored.
fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(CONTENT_TYPE) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let essence = value.split(';').next().unwrap_or(value).trim();
    let Some((kind, subtype)) = essence.split_once('/') else {
        return false;
    };
    kind.eq_ignore_ascii_case("application")
        && (subtype.eq_ignore_ascii_case("json")
            || subtype
                .rsplit_once('+')
                .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_field_is_named() {
        assert_eq!(missing_field("missing field `contour`"), Some("contour"));
        assert_eq!(missing_field("invalid digit found in string"), None);
    }

    #[test]
    fn the_expectation_is_taken_and_the_value_is_not() {
        // The value sits before the last separator and must not survive it.
        assert_eq!(
            expected_type("invalid type: integer `4`, expected a string"),
            Some("a string".to_owned())
        );
        assert_eq!(expected_type("invalid digit found in string"), None);
    }

    #[test]
    fn a_quoted_value_is_refused_rather_than_returned() {
        // A value containing the separator would otherwise push the split into
        // the caller's own text.
        assert_eq!(
            expected_type(r#"invalid type: string ", expected nothing", expected u32"#),
            Some("u32".to_owned())
        );
        assert_eq!(
            expected_type(r#"invalid type: string "x", expected "quoted""#),
            None
        );
    }

    #[test]
    fn the_json_position_is_dropped() {
        assert_eq!(
            strip_position("missing field `contour` at line 1 column 23"),
            "missing field `contour`"
        );
        assert_eq!(
            strip_position("missing field `contour`"),
            "missing field `contour`"
        );
    }

    #[test]
    fn only_json_content_types_are_accepted() {
        let mut headers = HeaderMap::new();
        assert!(!is_json_content_type(&headers));
        headers.insert(CONTENT_TYPE, "application/json".parse().expect("header"));
        assert!(is_json_content_type(&headers));
        headers.insert(
            CONTENT_TYPE,
            "application/json; charset=utf-8".parse().expect("header"),
        );
        assert!(is_json_content_type(&headers));
        headers.insert(
            CONTENT_TYPE,
            "application/merge-patch+json".parse().expect("header"),
        );
        assert!(is_json_content_type(&headers));
        headers.insert(CONTENT_TYPE, "text/csv".parse().expect("header"));
        assert!(!is_json_content_type(&headers));
    }
}
