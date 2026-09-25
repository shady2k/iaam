//! Sending requests.
//!
//! A client is built once per destination: building a `reqwest` client
//! creates a connection pool and parses the trust anchor, so doing this for
//! every request would discard both.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::destination::Destination;
use crate::request::{HttpMethod, HttpRequest};
use crate::resilience::parse_retry_after;
use crate::response::{HttpError, HttpResponse};
use crate::trust::{ConfiguredClient, client_for};

/// Response wait limit.
///
/// Explicit because `reqwest` has no default timeout; without one, a stalled
/// endpoint would become a background job that hangs forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Outgoing request client.
pub struct HttpClient {
    pool: Mutex<HashMap<Destination, ConfiguredClient>>,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    #[must_use]
    pub fn new() -> Self {
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
    pub async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let client = self.client_for(request.destination())?;
        let mut builder = match request.method() {
            HttpMethod::Get => client.0.get(request.url()),
            HttpMethod::Post => client.0.post(request.url()),
        };
        builder = builder.timeout(REQUEST_TIMEOUT);
        if let Some(secret) = request.bearer() {
            builder = builder.bearer_auth(secret.expose());
        }
        if let Some(action) = request.soap_action() {
            builder = builder.header("SOAPAction", format!("\"{action}\""));
        }
        if let Some(body) = request.body() {
            builder = builder
                .header("Content-Type", body.content_type())
                .body(body.payload().to_owned());
        }
        let response = builder.send().await.map_err(classify_transport_error)?;
        let status = response.status().as_u16();
        // Read before `bytes()` consumes the response.
        let retry_after = named_delay(response.headers(), request.reset_header());
        let body = response
            .bytes()
            .await
            .map_err(classify_transport_error)?
            .to_vec();
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
/// Only the delay-seconds form is understood, so a value in another form (an
/// HTTP-date) or an absent header both become `None`, and the retry policy
/// falls back to its computed backoff. Only the parsed delay leaves here, never
/// the header value.
fn named_delay(
    headers: &reqwest::header::HeaderMap,
    reset_header: Option<&str>,
) -> Option<Duration> {
    let seconds = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after)
    };
    seconds(reqwest::header::RETRY_AFTER.as_str()).or_else(|| reset_header.and_then(seconds))
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
        let named = named_delay(&headers(&[("retry-after", "12")]), None);
        assert_eq!(named, Some(Duration::from_secs(12)));
    }

    #[test]
    fn a_declared_reset_header_is_read_when_retry_after_is_absent() {
        let named = named_delay(
            &headers(&[("x-ratelimit-reset", "17")]),
            Some("x-ratelimit-reset"),
        );
        assert_eq!(named, Some(Duration::from_secs(17)));
    }

    #[test]
    fn retry_after_wins_over_a_declared_reset_header() {
        let named = named_delay(
            &headers(&[("retry-after", "3"), ("x-ratelimit-reset", "17")]),
            Some("x-ratelimit-reset"),
        );
        assert_eq!(named, Some(Duration::from_secs(3)));
    }

    #[test]
    fn an_undeclared_reset_header_is_ignored() {
        let named = named_delay(&headers(&[("x-ratelimit-reset", "17")]), None);
        assert_eq!(named, None);
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
