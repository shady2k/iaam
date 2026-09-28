//! The trust anchor is defined here and only here (§14).
//!
//! Trust policy is declared in **one destination table**, not scattered
//! across source crates. “Global” means one place of control, not one merged
//! set of anchors: an embedded root applies exactly to the endpoint it serves.
//!
//! T-Invest has its own anchor because the Ministry of Digital Development
//! root is absent from public stores, and pinning was the only way to connect.
//! MOEX (ZeroSSL) and CBR (HARICA) use public CA certificates; there is
//! nothing to embed, and pinning a public DV CA would break when the issuer
//! changes without buying us anything.
//!
//! Authentication is disabled for no destination. Only the source of the
//! anchor changes.

use reqwest::{Certificate, Client};

use crate::destination::Destination;
use crate::response::HttpError;

/// Ministry of Digital Development root certificate.
///
/// `include_str!`, rather than reading a file at startup: a file beside the
/// program is easier to replace than binary contents, and the trust anchor
/// is exactly what an attacker would replace first.
pub const RUSSIAN_TRUSTED_ROOT_CA_PEM: &str = include_str!("../certs/russian-trusted-root-ca.pem");

/// Count certificates in an arbitrary PEM bundle.
fn certificate_count_in_pem(pem: &str) -> usize {
    pem.matches("BEGIN CERTIFICATE").count()
}

/// Number of certificates in a PEM bundle.
#[must_use]
pub fn certificate_count(pem: &str) -> usize {
    certificate_count_in_pem(pem)
}

/// Source of a destination's trust anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchors {
    /// Public roots. The endpoint is signed by a public CA.
    WebRoots,
    /// Exactly one embedded root; public roots are disabled.
    Pinned(&'static str),
}

/// Client built with the selected trust policy.
#[derive(Clone)]
pub(crate) struct ConfiguredClient(pub(crate) Client);

/// Trust anchor for a destination.
///
/// The `impl` lives here rather than beside `Destination`: the endpoint base
/// is needed to build URLs and is unrelated to trust, while the anchor is
/// needed only when building a client. Different concerns belong in different
/// modules; they are in the same crate, so this additional `impl` is legal.
impl Destination {
    #[must_use]
    pub const fn anchors(self) -> Anchors {
        match self {
            // Both gateway environments use one certificate authority.
            Self::TinkoffProd | Self::TinkoffSandbox => {
                Anchors::Pinned(RUSSIAN_TRUSTED_ROOT_CA_PEM)
            }
            Self::FinamApi
            | Self::MoexIss
            | Self::CbrScripts
            | Self::CbrDailyInfo
            // The contract is hosted elsewhere, and the gateway's embedded
            // root does not apply: ordinary roots are the exact trust policy
            // appropriate here.
            | Self::TinvestContract => Anchors::WebRoots,
        }
    }
}

fn client_anchors(destination: Destination) -> Anchors {
    destination.anchors()
}

/// Build a client for a destination's trust anchor.
pub(crate) fn client_for(destination: Destination) -> Result<ConfiguredClient, HttpError> {
    let anchors = client_anchors(destination);
    let builder = Client::builder()
        .tls_backend_rustls()
        // Redirects and dependency-level retries would turn one gateway
        // reservation into several wire requests.
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never());
    let builder = match anchors {
        Anchors::WebRoots => builder,
        Anchors::Pinned(pem) => {
            let root = Certificate::from_pem(pem.as_bytes())
                .map_err(|error| HttpError::TrustAnchorNotParsed(error.to_string()))?;
            // `only`, not `merge`: `merge` would add our root to public roots,
            // leaving the client trusting the entire public internet for an
            // endpoint that does not need it.
            builder.tls_certs_only([root])
        }
    };
    let client = builder
        .build()
        .map_err(|error| HttpError::ClientNotBuilt(error.to_string()))?;
    Ok(ConfiguredClient(client))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_count_counts_each_certificate_in_a_pem_bundle() {
        assert_eq!(certificate_count_in_pem(""), 0);
        assert_eq!(
            certificate_count_in_pem(
                "-----BEGIN CERTIFICATE-----\nfirst\n-----END CERTIFICATE-----"
            ),
            1
        );
        assert_eq!(
            certificate_count_in_pem(
                "-----BEGIN CERTIFICATE-----\nfirst\n-----END CERTIFICATE-----\n\
                 -----BEGIN CERTIFICATE-----\nsecond\n-----END CERTIFICATE-----"
            ),
            2
        );
    }

    #[test]
    fn client_build_uses_the_destination_trust_policy() {
        assert_eq!(
            client_anchors(Destination::TinkoffProd),
            Anchors::Pinned(RUSSIAN_TRUSTED_ROOT_CA_PEM)
        );
        assert_eq!(client_anchors(Destination::MoexIss), Anchors::WebRoots);
    }

    #[tokio::test]
    async fn configured_client_returns_a_redirect_without_following_it() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("first request");
            let mut request = [0_u8; 1024];
            let _ = first.read(&mut request).expect("first request read");
            write!(
                first,
                "HTTP/1.1 302 Found\r\nLocation: http://{address}/followed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("redirect response");
            drop(first);

            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            loop {
                match listener.accept() {
                    Ok((mut second, _)) => {
                        let _ = second.read(&mut request).expect("redirected request read");
                        second
                            .write_all(
                                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .expect("redirect target response");
                        return 2;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop_rx.try_recv().is_ok() {
                            return 1;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept redirect target: {error}"),
                }
            }
        });

        let client = client_for(Destination::MoexIss).expect("configured client");
        let response = client
            .0
            .get(format!("http://{address}/start"))
            .send()
            .await
            .expect("redirect response returned");
        let _ = stop_tx.send(());

        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert_eq!(server.join().expect("server joined"), 1);
    }
}
