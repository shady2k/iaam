//! Outgoing request description (§3.1).
//!
//! A request description is data, not an action: it is built and checked
//! without a network, which is why source crates need not know the transport
//! at all. `HttpClient` handles sending.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
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
    query: Vec<(String, String)>,
    body: Option<RequestBody>,
    bearer: Option<Secret>,
    auth_scheme: AuthScheme,
    soap_action: Option<String>,
    reset_header: Option<&'static str>,
    idempotent: bool,
    allowance: Option<RequestAllowance>,
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
            query: Vec::new(),
            body,
            bearer: None,
            auth_scheme: AuthScheme::Bearer,
            soap_action: None,
            reset_header: None,
            allowance: None,
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
        let path = self.path.trim_start_matches('/');
        let mut url = format!("{base}/{path}");
        if !self.query.is_empty() {
            url.push('?');
            let encoded: Vec<String> = self
                .query
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        utf8_percent_encode(key, NON_ALPHANUMERIC),
                        utf8_percent_encode(value, NON_ALPHANUMERIC)
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
