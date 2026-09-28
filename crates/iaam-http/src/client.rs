//! Sending requests.
//!
//! A client is built once per destination: building a `reqwest` client
//! creates a connection pool and parses the trust anchor, so doing this for
//! every request would discard both.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::destination::Destination;
use crate::request::{HttpMethod, HttpRequest};
use crate::resilience::parse_retry_after;
use crate::response::{HttpError, HttpResponse};
use crate::trust::{ConfiguredClient, client_for};

/// Response wait limit.
///
/// Explicit because `reqwest` has no default timeout; without one, a stalled
/// endpoint would become a background job that hangs forever.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// No server-named wait can extend gateway arithmetic beyond one day.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Outgoing request client.
///
/// Built only inside this crate, so outside it an `HttpClient` exists only
/// inside the gateway `Gateway::production` returns: a caller holding one
/// could send through the public `Transport` trait past every rule of the
/// gateway. For the same reason it has no `Default`.
pub struct HttpClient {
    pool: Mutex<HashMap<Destination, ConfiguredClient>>,
}

impl HttpClient {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn client_for(
        &self,
        destination: Destination,
    ) -> Result<ConfiguredClient, HttpError> {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = pool.get(&destination) {
            return Ok(existing.clone());
        }
        let built = client_for(destination)?;
        pool.insert(destination, built.clone());
        Ok(built)
    }

    #[cfg(test)]
    pub(crate) fn pool_len(&self) -> usize {
        self.pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Send a request and return its status and body.
    ///
    /// The response status is **not classified** here: 401 has different
    /// meaning at a broker gateway and an exchange, and interpretation
    /// belongs to the source.
    ///
    /// Private to this crate: outside it the only way to send is
    /// `Gateway::send`, which the `Transport` impl serves.
    pub(crate) async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.send_observed(request, Box::new(|_, _| {})).await
    }

    /// Send while publishing status and Retry-After before consuming the body.
    pub(crate) async fn send_observed(
        &self,
        request: &HttpRequest,
        observe: Box<dyn FnOnce(u16, Option<Duration>) + Send + '_>,
    ) -> Result<HttpResponse, HttpError> {
        self.send_to_url(request, request.url(), REQUEST_TIMEOUT, observe)
            .await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn send_to_base(
        &self,
        request: &HttpRequest,
        base_url: &str,
        timeout: Duration,
    ) -> Result<HttpResponse, HttpError> {
        let request_url = request.url();
        let destination_base = request.destination().base_url().trim_end_matches('/');
        let suffix = request_url
            .strip_prefix(destination_base)
            .unwrap_or(request_url.as_str());
        let url = format!("{}{suffix}", base_url.trim_end_matches('/'));
        self.send_to_url(request, url, timeout, Box::new(|_, _| {}))
            .await
    }

    async fn send_to_url(
        &self,
        request: &HttpRequest,
        url: String,
        timeout: Duration,
        observe: Box<dyn FnOnce(u16, Option<Duration>) + Send + '_>,
    ) -> Result<HttpResponse, HttpError> {
        let client = self.client_for(request.destination())?;
        let built = build_at(&client.0, request, &url, timeout)?;
        let mut response = client
            .0
            .execute(built)
            .await
            .map_err(classify_transport_error)?;
        let status = response.status().as_u16();
        let successful = response.status().is_success();
        let retry_after = named_delay(
            response.headers(),
            request.reset_header(),
            SystemTime::now(),
        );
        observe(status, retry_after);
        // A status line that arrived is never lost: after a non-2xx status a
        // body that fails or stalls ends the read with what arrived.
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(error) if successful => return Err(classify_transport_error(error)),
                Err(_) => break,
            }
        }
        Ok(HttpResponse {
            status,
            body,
            retry_after,
        })
    }
}

/// The delay the source named: its `Retry-After`, else the reset header the
/// request declared.
///
/// Delay-seconds or an HTTP-date, the date read against `now`, the instant
/// the answer arrived. A value in neither form or an absent header becomes
/// `None`, and the retry policy falls back to its computed backoff. Only the
/// parsed delay leaves here, never the header value.
fn named_delay(
    headers: &reqwest::header::HeaderMap,
    reset_header: Option<&str>,
    now: SystemTime,
) -> Option<Duration> {
    let seconds = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_retry_after(value, now))
            .map(|delay| delay.min(MAX_RETRY_AFTER))
    };
    seconds(reqwest::header::RETRY_AFTER.as_str()).or_else(|| reset_header.and_then(seconds))
}

/// The request exactly as it goes on the wire: method, URL, timeout and
/// headers. Separate from sending so what a broker receives can be checked
/// without a network.
#[cfg(test)]
fn build(client: &reqwest::Client, request: &HttpRequest) -> Result<reqwest::Request, HttpError> {
    build_at(client, request, &request.url(), REQUEST_TIMEOUT)
}

fn build_at(
    client: &reqwest::Client,
    request: &HttpRequest,
    url: &str,
    timeout: Duration,
) -> Result<reqwest::Request, HttpError> {
    let mut builder = match request.method() {
        HttpMethod::Get => client.get(url),
        HttpMethod::Post => client.post(url),
    };
    builder = builder.timeout(timeout);
    if let Some(value) = authorization_header(request)? {
        builder = builder.header(reqwest::header::AUTHORIZATION, value);
    }
    if let Some(action) = request.soap_action() {
        builder = builder.header("SOAPAction", format!("\"{action}\""));
    }
    if let Some(body) = request.body() {
        builder = builder
            .header("Content-Type", body.content_type())
            .body(body.payload().to_owned());
    }
    builder
        .build()
        .map_err(|error| HttpError::RequestNotBuilt(error.to_string()))
}

/// The `Authorization` header as it goes on the wire, marked sensitive so
/// `reqwest` never prints it. A token that is not a valid header value is a
/// local fault, refused before anything is sent.
fn authorization_header(
    request: &HttpRequest,
) -> Result<Option<reqwest::header::HeaderValue>, HttpError> {
    let Some(authorization) = request.authorization() else {
        return Ok(None);
    };
    let mut value =
        reqwest::header::HeaderValue::from_str(authorization.expose()).map_err(|_| {
            HttpError::RequestNotBuilt("the token is not a valid Authorization value".to_owned())
        })?;
    value.set_sensitive(true);
    Ok(Some(value))
}

fn classify_transport_error(error: reqwest::Error) -> HttpError {
    if error.is_timeout() {
        HttpError::Timeout
    } else {
        HttpError::Network
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::request::RequestBody;

    #[test]
    fn a_client_is_built_once_per_destination() {
        let client = HttpClient::new();
        let first = client.pool_len();
        let _ = client.client_for(Destination::MoexIss).expect("client");
        let _ = client.client_for(Destination::MoexIss).expect("client");
        assert_eq!(first, 0);
        assert_eq!(client.pool_len(), 1, "second request built a second client");
    }

    #[test]
    fn distinct_destinations_get_distinct_clients() {
        let client = HttpClient::new();
        let _ = client.client_for(Destination::MoexIss).expect("client");
        let _ = client.client_for(Destination::TinkoffProd).expect("client");
        assert_eq!(client.pool_len(), 2);
    }

    #[test]
    fn the_two_gateway_environments_do_not_share_a_client() {
        let client = HttpClient::new();
        let _ = client.client_for(Destination::TinkoffProd).expect("client");
        let _ = client
            .client_for(Destination::TinkoffSandbox)
            .expect("client");
        assert_eq!(
            client.pool_len(),
            2,
            "sandbox and production are different hosts; sharing a client would route the request incorrectly"
        );
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> reqwest::header::HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| {
                (
                    reqwest::header::HeaderName::from_static(name),
                    reqwest::header::HeaderValue::from_static(value),
                )
            })
            .collect()
    }

    #[test]
    fn retry_after_is_the_named_delay() {
        let named = named_delay(&headers(&[("retry-after", "12")]), None, SystemTime::now());
        assert_eq!(named, Some(Duration::from_secs(12)));
    }

    #[test]
    fn retry_after_is_clamped_before_gateway_time_arithmetic() {
        let named = named_delay(
            &headers(&[("retry-after", "18446744073709551615")]),
            None,
            SystemTime::now(),
        );
        assert_eq!(named, Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn a_declared_reset_header_is_read_when_retry_after_is_absent() {
        let named = named_delay(
            &headers(&[("x-ratelimit-reset", "17")]),
            Some("x-ratelimit-reset"),
            SystemTime::now(),
        );
        assert_eq!(named, Some(Duration::from_secs(17)));
    }

    #[test]
    fn retry_after_wins_over_a_declared_reset_header() {
        let named = named_delay(
            &headers(&[("retry-after", "3"), ("x-ratelimit-reset", "17")]),
            Some("x-ratelimit-reset"),
            SystemTime::now(),
        );
        assert_eq!(named, Some(Duration::from_secs(3)));
    }

    #[test]
    fn retry_after_as_an_http_date_is_the_named_delay() {
        // 2026-10-21 07:28:00 UTC.
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(1_792_567_680))
            .expect("fixture time is representable");
        let named = named_delay(
            &headers(&[("retry-after", "Wed, 21 Oct 2026 07:33:00 GMT")]),
            None,
            now,
        );
        assert_eq!(named, Some(Duration::from_secs(300)));
    }

    #[test]
    fn an_undeclared_reset_header_is_ignored() {
        let named = named_delay(
            &headers(&[("x-ratelimit-reset", "17")]),
            None,
            SystemTime::now(),
        );
        assert_eq!(named, None);
    }

    fn configured_client(request: &HttpRequest) -> reqwest::Client {
        client_for(request.destination())
            .expect("the configured client builds")
            .0
    }

    fn wire(request: &HttpRequest) -> reqwest::Request {
        build(&configured_client(request), request).expect("the request builds")
    }

    #[test]
    fn a_bearer_request_goes_out_with_the_bearer_scheme() {
        let request = HttpRequest::get(Destination::TinkoffProd, "/").with_bearer("t.token");
        let sent = wire(&request);
        let value = sent
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("an Authorization header");

        assert_eq!(value.to_str().expect("ascii"), "Bearer t.token");
        assert!(value.is_sensitive());
    }

    #[test]
    fn a_bare_token_goes_out_as_the_whole_header_value() {
        let request = HttpRequest::get(Destination::FinamApi, "/").with_bare_token("jwt.value");
        let sent = wire(&request);
        let value = sent
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("an Authorization header");

        assert_eq!(value.to_str().expect("ascii"), "jwt.value");
        assert!(value.is_sensitive());
    }

    #[test]
    fn a_request_without_a_token_goes_out_without_authorization() {
        let request = HttpRequest::post(
            Destination::FinamApi,
            "/v1/sessions",
            RequestBody::Json("{}".to_owned()),
        );
        let sent = wire(&request);

        assert!(sent.headers().get(reqwest::header::AUTHORIZATION).is_none());
        assert_eq!(sent.url().as_str(), "https://api.finam.ru/v1/sessions");
        assert_eq!(
            sent.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .map(|value| value.to_str().expect("ascii")),
            Some("application/json")
        );
    }

    #[test]
    fn a_token_that_is_not_a_header_value_is_refused_before_sending() {
        let request = HttpRequest::get(Destination::FinamApi, "/").with_bare_token("bad\ntoken");

        assert!(matches!(
            build(&configured_client(&request), &request),
            Err(HttpError::RequestNotBuilt(_))
        ));
    }

    #[test]
    fn a_soap_request_carries_its_action_header() {
        let request = HttpRequest::post(
            Destination::CbrDailyInfo,
            "/DailyInfoWebServ/DailyInfo.asmx",
            RequestBody::Xml("<soap:Envelope/>".to_owned()),
        )
        .with_soap_action("http://web.cbr.ru/KeyRateXML");
        assert_eq!(request.soap_action(), Some("http://web.cbr.ru/KeyRateXML"));
        assert_eq!(
            request.body().map(RequestBody::content_type),
            Some("text/xml; charset=utf-8")
        );
    }
}
