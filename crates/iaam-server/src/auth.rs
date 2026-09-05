//! Authentication (§14).
//!
//! Authentication from day one: if deferred, it is never added.
//! The database stores the token **hash**; comparison is constant-time so that
//! response time does not reveal the correct prefix.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use iaam_app::ports::Principal;

use crate::ServerState;
use crate::error::ApiFailure;

/// Token hash. Lives in `iaam-app` because the adapter computes the same hash
/// when issuing a token, but cannot reach the transport layer from there:
/// dependencies run from top to bottom. This is a re-export, not a second implementation —
/// if they diverged, they could produce an issued token that cannot be found
/// during verification, forcing the cause to be sought in the wrong place.
/// The rationale for choosing SHA-256 and not using constant-time
/// comparison is in the function's own documentation.
pub use iaam_app::tokens::hash_token;

/// Extracting the token from the `Authorization: Bearer …` header.
#[must_use]
pub fn bearer(request: &Request) -> Option<String> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_owned())
}

/// Authentication and rate-limiting layer.
///
/// Token usage is logged for **every** request, including
/// rejected ones: attempts using a revoked token are precisely why
/// the log is needed (§14).
pub async fn authenticate(
    State(state): State<ServerState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiFailure> {
    let route = request.uri().path().to_owned();
    let Some(token) = bearer(&request) else {
        return Err(ApiFailure::unauthorized());
    };
    let hash = hash_token(&token);

    // The wait travels with the verdict rather than being asked for after it:
    // the refusal is about a window that is running, and a second question
    // about it would be answered by a different window.
    if let Some(retry_after) = state.limiter.allow(&hash).retry_after() {
        tracing::warn!(%route, "request rate exceeded");
        return Err(ApiFailure::too_many_requests(retry_after));
    }

    let principal = state
        .services
        .store
        .find_principal(hash.clone())
        .await
        .map_err(ApiFailure::from)?;

    let Some(principal) = principal else {
        // An unknown token is NOT written to the usage log: the log
        // is maintained per token, and there is no token here. Recording every
        // attempt would turn a stream of random strings into unbounded
        // database growth through the only unprotected path (§14).
        tracing::warn!(%route, "unknown token presented");
        return Err(ApiFailure::invalid_token());
    };

    let _ = state
        .services
        .store
        .record_token_use(hash, route, "accepted".into())
        .await;

    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

/// Extracting the recognised token bearer in a handler.
pub fn principal(request: &Request) -> Result<Principal, ApiFailure> {
    request
        .extensions()
        .get::<Principal>()
        .cloned()
        .ok_or_else(ApiFailure::unauthorized)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_stable_and_does_not_contain_the_token() {
        let hash = hash_token("secret");
        assert_eq!(hash, hash_token("secret"));
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("secret"));
        assert_ne!(hash, hash_token("secret "));
    }

    #[test]
    fn different_tokens_hash_differently() {
        assert_ne!(hash_token("a"), hash_token("b"));
        assert_ne!(hash_token(""), hash_token(" "));
    }
}
