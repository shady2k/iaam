//! Transport response and refusals.

use thiserror::Error;

/// Endpoint response: status and body as received.
///
/// The body is not parsed or re-encoded here: CBR responds in
/// `windows-1251`, MOEX in UTF-8, and that knowledge belongs to the source
/// crate, not the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Body as a UTF-8 string. Sources with another encoding do not use this
    /// method; they take `body` and re-encode it themselves.
    #[must_use]
    pub fn text_utf8(&self) -> Option<&str> {
        core::str::from_utf8(&self.body).ok()
    }
}

/// Transport refusal.
///
/// Variants carry neither response body nor presented secret: response
/// classification belongs to the source, and a transport refusal must not
/// become a data leak.
#[derive(Debug, Error)]
pub enum HttpError {
    #[error("network refusal")]
    Network,
    #[error("response timed out")]
    Timeout,
    #[error("client was not built: {0}")]
    ClientNotBuilt(String),
    #[error("embedded trust root was not parsed: {0}")]
    TrustAnchorNotParsed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_utf8_returns_the_exact_valid_body() {
        let response = HttpResponse {
            status: 200,
            body: "response".as_bytes().to_vec(),
        };

        assert_eq!(response.text_utf8(), Some("response"));
    }

    #[test]
    fn text_utf8_rejects_invalid_utf8() {
        let response = HttpResponse {
            status: 200,
            body: vec![0xff, 0xfe],
        };

        assert_eq!(response.text_utf8(), None);
    }
}
