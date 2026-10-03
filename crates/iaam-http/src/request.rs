//! Outgoing request description (§3.1).
//!
//! A request description is data, not an action: it is built and checked
//! without a network, which is why source crates need not know the transport
//! at all. `HttpClient` handles sending.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// Everything but RFC 3986's unreserved characters (§2.3: letters, digits,
/// `-`, `.`, `_`, `~`), which are never encoded. Encoding them is legal but
/// not equivalent everywhere: the live Finam API routed a query named
/// `interval%2Estart%5Ftime` away from the API (iaam-xzz5.1).
const QUERY: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Path-segment encoding that keeps `@` literal: RFC 3986 (§3.3) admits
/// `@` in a path segment, and Finam's asset symbols are of the form
/// `TICKER@MIC` (iaam-vg8te.1.3). Everything else outside the unreserved
/// set is encoded, exactly as [`QUERY`]'s account-path rule does.
const SYMBOL_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'@');
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use zeroize::Zeroizing;

use crate::destination::Destination;

/// Request method. Extend as needed: a variant unused by any source is
/// unchecked code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// Request body together with its content type: the type cannot be forgotten,
/// because it is not separate from the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestBody {
    Json(String),
    /// A SOAP envelope comes here too: it needs no separate variant;
    /// `SOAPAction` distinguishes it from XML (see `soap_action`).
    Xml(String),
}

impl RequestBody {
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        match self {
            Self::Json(_) => "application/json",
            Self::Xml(_) => "text/xml; charset=utf-8",
        }
    }

    #[must_use]
    pub fn payload(&self) -> &str {
        match self {
            Self::Json(body) | Self::Xml(body) => body,
        }
    }
}

/// Presented secret.
///
/// `Debug` is implemented manually and prints a placeholder. A derived
/// `Debug` would print the token in the first refusal log, while `Zeroizing`
/// erases the copy on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    #[must_use]
    pub fn new(value: &str) -> Self {
        Self(Zeroizing::new(value.to_owned()))
    }

    /// The only point where the secret is exposed as a string. Named this way
    /// so the call is conspicuous in review.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// How a request's token is written into its `Authorization` header. Each
/// source publishes its own form, and a server that expects the other one
/// refuses every call: T-Invest documents `Bearer <token>`, Finam the bare
/// token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// `Authorization: Bearer <token>`.
    Bearer,
    /// `Authorization: <token>`, with no scheme word.
    Bare,
}
/// One caller's finite allowance of transport attempts.
///
/// Clones share the same count. The request carries the handle into the
/// gateway, where retries and first attempts consume it alike immediately
/// before the transport is called.
#[derive(Debug, Clone)]
pub struct RequestAllowance {
    state: Arc<RequestAllowanceState>,
}

#[derive(Debug)]
struct RequestAllowanceState {
    ceiling: u32,
    remaining: AtomicU32,
}

impl RequestAllowance {
    #[must_use]
    pub fn new(ceiling: u32) -> Self {
        Self {
            state: Arc::new(RequestAllowanceState {
                ceiling,
                remaining: AtomicU32::new(ceiling),
            }),
        }
    }

    #[must_use]
    pub fn ceiling(&self) -> u32 {
        self.state.ceiling
    }

    pub(crate) fn take(&self) -> bool {
        self.state
            .remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    pub(crate) fn give_back(&self) {
        self.state.remaining.fetch_add(1, Ordering::SeqCst);
    }
}

/// Complete description of an outgoing request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    destination: Destination,
    method: HttpMethod,
    path: String,
    wire_path: Option<String>,
    query: Vec<(String, String)>,
    body: Option<RequestBody>,
    bearer: Option<Secret>,
    auth_scheme: AuthScheme,
    soap_action: Option<String>,
    reset_header: Option<&'static str>,
    idempotent: bool,
    allowance: Option<RequestAllowance>,
    /// The stable access identity an authorized read is keyed on in the
    /// response cache, when the caller binds one: the long-lived secret,
    /// not the rotating session token. Never sent on the wire; only its
    /// digest may enter a cache key.
    cache_identity: Option<Secret>,
}

impl HttpRequest {
    #[must_use]
    pub fn get(destination: Destination, path: &str) -> Self {
        Self::new(destination, HttpMethod::Get, path, None)
    }

    #[must_use]
    pub fn post(destination: Destination, path: &str, body: RequestBody) -> Self {
        Self::new(destination, HttpMethod::Post, path, Some(body))
    }

    /// Build a GET whose one caller-supplied path segment is encoded on the
    /// wire while [`Self::path`] retains the raw spelling for policy checks.
    #[must_use]
    pub fn get_with_encoded_path_segment(
        destination: Destination,
        prefix: &str,
        segment: &str,
        suffix: &str,
    ) -> Self {
        Self::get_with_encoded_segment(destination, prefix, segment, suffix, NON_ALPHANUMERIC)
    }

    /// Build a GET whose caller-supplied symbol segment is encoded on the
    /// wire with `@` kept literal, while [`Self::path`] retains the raw
    /// spelling for policy checks. RFC 3986 (§3.3) admits `@` in a path
    /// segment, and Finam's asset symbols are of the form `TICKER@MIC`
    /// (iaam-vg8te.1.3); everything else outside the unreserved set is
    /// encoded exactly as [`Self::get_with_encoded_path_segment`] does.
    #[must_use]
    pub fn get_with_symbol_path_segment(
        destination: Destination,
        prefix: &str,
        symbol: &str,
    ) -> Self {
        Self::get_with_encoded_segment(destination, prefix, symbol, "", SYMBOL_SEGMENT)
    }

    fn get_with_encoded_segment(
        destination: Destination,
        prefix: &str,
        segment: &str,
        suffix: &str,
        set: &'static AsciiSet,
    ) -> Self {
        let path = format!("{prefix}{segment}{suffix}");
        let encoded = utf8_percent_encode(segment, set);
        let wire_path = format!("{prefix}{encoded}{suffix}");
        let mut request = Self::get(destination, &path);
        request.wire_path = Some(wire_path);
        request
    }

    fn new(
        destination: Destination,
        method: HttpMethod,
        path: &str,
        body: Option<RequestBody>,
    ) -> Self {
        Self {
            destination,
            method,
            path: path.to_owned(),
            wire_path: None,
            query: Vec::new(),
            body,
            bearer: None,
            auth_scheme: AuthScheme::Bearer,
            soap_action: None,
            reset_header: None,
            allowance: None,
            cache_identity: None,
            // A GET reads; any other method may act, and acting twice is
            // not undone by a later success.
            idempotent: matches!(method, HttpMethod::Get),
        }
    }

    #[must_use]
    pub fn with_query(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.to_owned(), value.to_owned()));
        self
    }

    #[must_use]
    pub fn with_bearer(mut self, token: &str) -> Self {
        self.bearer = Some(Secret::new(token));
        self.auth_scheme = AuthScheme::Bearer;
        self
    }

    /// The token as the whole `Authorization` value, with no scheme word:
    /// the form Finam publishes (`Authorization: <token>`).
    #[must_use]
    pub fn with_bare_token(mut self, token: &str) -> Self {
        self.bearer = Some(Secret::new(token));
        self.auth_scheme = AuthScheme::Bare;
        self
    }

    /// Bind the stable access identity the response cache keys this read
    /// on. The identity is the caller-bound long-lived secret — the access
    /// that owns the answer — not the rotating session token a read is
    /// presented with: a renewed token must not re-send a read the cache
    /// already holds for the same access. The identity never leaves the
    /// request description; the cache digests it, and no cache file holds
    /// it.
    #[must_use]
    pub fn with_cache_identity(mut self, identity: &str) -> Self {
        self.cache_identity = Some(Secret::new(identity));
        self
    }

    /// `SOAPAction` header. Required by CBR: without it the service returns a
    /// refusal rather than a parse error, and the reason is not obvious.
    #[must_use]
    pub fn with_soap_action(mut self, action: &str) -> Self {
        self.soap_action = Some(action.to_owned());
        self
    }

    /// A response header in which the source names, in seconds, when its
    /// limit resets (T-Invest's `x-ratelimit-reset`). Read only when the
    /// response carries no `Retry-After`, and then used the same way: the
    /// source knows its own window better than our backoff guesses it.
    #[must_use]
    pub const fn with_reset_header(mut self, name: &'static str) -> Self {
        self.reset_header = Some(name);
        self
    }

    /// Mark the request as safe to send more than once: the gateway retries
    /// only such a request. For a POST that only reads, such as a T-Invest
    /// RPC or a CBR SOAP query; never for one that orders, moves or writes.
    #[must_use]
    pub const fn idempotent(mut self) -> Self {
        self.idempotent = true;
        self
    }
    /// Count every transport attempt for this request against `allowance`.
    #[must_use]
    pub fn with_request_allowance(mut self, allowance: RequestAllowance) -> Self {
        self.allowance = Some(allowance);
        self
    }

    #[must_use]
    pub const fn is_idempotent(&self) -> bool {
        self.idempotent
    }

    #[must_use]
    pub const fn destination(&self) -> Destination {
        self.destination
    }

    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn body(&self) -> Option<&RequestBody> {
        self.body.as_ref()
    }

    /// The token the request carries, whatever its scheme.
    #[must_use]
    pub const fn bearer(&self) -> Option<&Secret> {
        self.bearer.as_ref()
    }

    #[must_use]
    pub const fn auth_scheme(&self) -> AuthScheme {
        self.auth_scheme
    }

    /// The `Authorization` header value exactly as it is sent, or `None`
    /// when the request carries no token.
    #[must_use]
    pub fn authorization(&self) -> Option<Secret> {
        self.bearer.as_ref().map(|token| match self.auth_scheme {
            AuthScheme::Bearer => Secret::new(&format!("Bearer {}", token.expose())),
            AuthScheme::Bare => Secret::new(token.expose()),
        })
    }

    /// The stable access identity bound to this request, or `None` when
    /// the caller bound none and the presented credential keys the cache.
    #[must_use]
    pub fn cache_identity(&self) -> Option<&Secret> {
        self.cache_identity.as_ref()
    }

    #[must_use]
    pub fn soap_action(&self) -> Option<&str> {
        self.soap_action.as_deref()
    }

    #[must_use]
    pub const fn reset_header(&self) -> Option<&'static str> {
        self.reset_header
    }
    pub(crate) const fn allowance(&self) -> Option<&RequestAllowance> {
        self.allowance.as_ref()
    }

    /// Complete request URL.
    #[must_use]
    pub fn url(&self) -> String {
        let base = self.destination.base_url().trim_end_matches('/');
        let path = self.wire_path.as_deref().unwrap_or(&self.path);
        let path = path.trim_start_matches('/');
        let mut url = format!("{base}/{path}");
        if !self.query.is_empty() {
            url.push('?');
            let encoded: Vec<String> = self
                .query
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        utf8_percent_encode(key, QUERY),
                        utf8_percent_encode(value, QUERY)
                    )
                })
                .collect();
            url.push_str(&encoded.join("&"));
        }
        url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post() -> HttpRequest {
        HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
            RequestBody::Json("{}".to_owned()),
        )
    }

    #[test]
    fn a_get_is_idempotent_by_default() {
        assert!(HttpRequest::get(Destination::MoexIss, "/iss/index.json").is_idempotent());
    }

    #[test]
    fn a_post_is_not_idempotent_unless_marked() {
        assert!(!post().is_idempotent());
    }

    #[test]
    fn a_post_marked_idempotent_is_idempotent() {
        assert!(post().idempotent().is_idempotent());
    }

    #[test]
    fn an_allowance_spends_exactly_its_ceiling_and_can_refund_an_unsent_attempt() {
        let allowance = RequestAllowance::new(2);

        assert_eq!(allowance.ceiling(), 2);
        assert!(allowance.take());
        assert!(allowance.take());
        assert!(!allowance.take());

        allowance.give_back();
        assert!(allowance.take());
        assert!(!allowance.take());
    }

    #[test]
    fn a_request_names_no_reset_header_unless_told() {
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");
        assert_eq!(request.reset_header(), None);
    }

    #[test]
    fn a_declared_reset_header_is_kept_by_name() {
        let request =
            HttpRequest::get(Destination::TinkoffProd, "/").with_reset_header("x-ratelimit-reset");
        assert_eq!(request.reset_header(), Some("x-ratelimit-reset"));
    }

    #[test]
    fn a_url_joins_base_and_path_without_doubling_the_slash() {
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");
        assert_eq!(request.url(), "https://iss.moex.com/iss/history.json");
    }

    #[test]
    fn an_encoded_path_segment_keeps_its_raw_policy_path() {
        let request = HttpRequest::get_with_encoded_path_segment(
            Destination::FinamApi,
            "/v1/accounts/",
            "Main Account",
            "/transactions",
        );

        assert_eq!(request.path(), "/v1/accounts/Main Account/transactions");
        assert_eq!(
            request.url(),
            "https://api.finam.ru/v1/accounts/Main%20Account/transactions"
        );
    }

    #[test]
    fn a_symbol_path_segment_keeps_the_at_literal_on_the_wire() {
        // RFC 3986 §3.3 admits `@` in a path segment, and Finam's asset
        // symbols are of the form `TICKER@MIC` (iaam-vg8te.1.3): the wire
        // URL spells the symbol exactly as the channel read it, while
        // everything else outside the unreserved set is still encoded, as
        // for the account paths.
        let request = HttpRequest::get_with_symbol_path_segment(
            Destination::FinamApi,
            "/v1/assets/",
            "SBER@MISX",
        );

        assert_eq!(request.path(), "/v1/assets/SBER@MISX");
        assert_eq!(request.url(), "https://api.finam.ru/v1/assets/SBER@MISX");
    }

    #[test]
    fn a_symbol_path_segment_still_encodes_what_the_wire_cannot_carry() {
        let request = HttpRequest::get_with_symbol_path_segment(
            Destination::FinamApi,
            "/v1/assets/",
            "Main Share@MISX",
        );

        assert_eq!(request.path(), "/v1/assets/Main Share@MISX");
        assert_eq!(
            request.url(),
            "https://api.finam.ru/v1/assets/Main%20Share@MISX"
        );
    }

    #[test]
    fn a_query_keeps_the_unreserved_characters_as_they_are() {
        // RFC 3986 §2.3: `-`, `.`, `_` and `~` are never encoded. The live
        // Finam API routed `interval%2Estart%5Ftime` away from the API
        // (iaam-xzz5.1); reserved characters are still encoded.
        let request = HttpRequest::get(Destination::FinamApi, "/v1/accounts/Main/transactions")
            .with_query("interval.start_time", "2026-09-01T00:00:00Z")
            .with_query("a_b~c", "x y&z=1");

        assert_eq!(
            request.url(),
            "https://api.finam.ru/v1/accounts/Main/transactions\
             ?interval.start_time=2026-09-01T00%3A00%3A00Z\
             &a_b~c=x%20y%26z%3D1"
        );
    }

    #[test]
    fn an_empty_query_leaves_no_dangling_question_mark() {
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");
        assert!(!request.url().contains('?'));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        let request = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp")
            .with_query("name", "Австралийский доллар")
            .with_query("range", "a b");
        let url = request.url();
        assert!(!url.contains(' '), "spaces must be escaped: {url}");
        assert!(!url.contains('Д'), "Cyrillic must be escaped: {url}");
        assert!(url.contains("range=a%20b"), "{url}");
    }

    #[test]
    fn a_bearer_secret_never_appears_in_debug_output() {
        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/OperationsService/GetOperationsByCursor",
            RequestBody::Json("{}".to_owned()),
        )
        .with_bearer("t.SUPER-SECRET-VALUE");
        let printed = format!("{request:?}");
        assert!(
            !printed.contains("SUPER-SECRET-VALUE"),
            "secret leaked into Debug: {printed}"
        );
    }

    #[test]
    fn request_body_payload_preserves_json_and_xml_contents() {
        let json = RequestBody::Json(r#"{"cursor":7}"#.to_owned());
        let xml = RequestBody::Xml("<Envelope/>".to_owned());

        assert_eq!(json.payload(), r#"{"cursor":7}"#);
        assert_eq!(xml.payload(), "<Envelope/>");
    }

    #[test]
    fn secret_expose_returns_the_original_token() {
        let secret = Secret::new("token-value");

        assert_eq!(secret.expose(), "token-value");
    }

    #[test]
    fn a_bearer_request_retains_its_token_for_transport() {
        let request = HttpRequest::get(Destination::MoexIss, "/").with_bearer("bearer-token");

        assert_eq!(request.bearer().map(Secret::expose), Some("bearer-token"));
    }

    #[test]
    fn a_bearer_request_is_authorized_with_the_bearer_scheme() {
        let request = HttpRequest::get(Destination::MoexIss, "/").with_bearer("bearer-token");

        assert_eq!(
            request.authorization().as_ref().map(Secret::expose),
            Some("Bearer bearer-token")
        );
    }

    #[test]
    fn a_bare_token_request_is_authorized_with_the_token_alone() {
        let request = HttpRequest::get(Destination::FinamApi, "/").with_bare_token("jwt-value");

        assert_eq!(
            request.authorization().as_ref().map(Secret::expose),
            Some("jwt-value")
        );
        assert_eq!(request.bearer().map(Secret::expose), Some("jwt-value"));
    }

    #[test]
    fn a_request_without_a_token_carries_no_authorization() {
        assert!(
            HttpRequest::get(Destination::MoexIss, "/")
                .authorization()
                .is_none()
        );
    }

    #[test]
    fn secret_debug_is_redacted_but_not_empty() {
        assert_eq!(
            format!("{:?}", Secret::new("token-value")),
            "Secret(<redacted>)"
        );
    }

    #[test]
    fn the_sandbox_is_a_different_host_not_a_different_path() {
        assert_ne!(
            Destination::TinkoffProd.base_url(),
            Destination::TinkoffSandbox.base_url()
        );
        assert!(Destination::TinkoffSandbox.base_url().contains("sandbox"));
        assert!(!Destination::TinkoffProd.base_url().contains("sandbox"));
    }

    #[test]
    fn every_destination_serves_https() {
        for destination in Destination::ALL {
            assert!(
                destination.base_url().starts_with("https://"),
                "{destination:?} does not use HTTPS"
            );
        }
    }
}
