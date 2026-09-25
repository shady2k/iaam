//! One-off live check of the T-Invest channel.
//!
//! Not a test: tests do not touch the network. This command exists to verify
//! that the embedded trust root actually validates the gateway chain and to
//! capture response samples for frozen fixtures.
//!
//! ```text
//! nix develop -c cargo run -p iaam-broker --example tinkoff-probe
//! ```
//!
//! Without a token, `401` is expected; that is enough to show that the TLS
//! handshake completed. With a token, pass the file path through
//! `IAAM_TINKOFF_SANDBOX_TOKEN_FILE`; the token itself never appears in the
//! command-line arguments, because the process list is visible to the whole machine.

use std::env;
use std::fs;

use iaam_http::client::HttpClient;
use iaam_http::{Destination, Gateway, GatewayError, HttpRequest, RequestBody};

// The ordinary method at the sandbox address is the method recommended by
// T-Invest. The sandbox method at this same address returns `40003`, namely a
// complaint about the token rather than a complaint about the route.
const METHOD: &str = "tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A separate process, so a gateway of its own: it shares no budget with
    // a running server, and a probe is one request.
    let gateway = Gateway::new(HttpClient::new())?;
    let mut request = HttpRequest::post(
        Destination::TinkoffSandbox,
        METHOD,
        RequestBody::Json("{}".to_owned()),
    )
    .with_reset_header("x-ratelimit-reset");

    match env::var("IAAM_TINKOFF_SANDBOX_TOKEN_FILE") {
        Ok(path) => {
            let token = fs::read_to_string(&path)?;
            println!("token read from {path}");
            request = request.with_bearer(token.trim());
        }
        Err(_) => {
            println!("no token: 401 is expected and confirms only that the chain was validated")
        }
    }

    // The gateway returns only a success; the expected 401 comes back as a
    // rejection, and its status is the whole answer.
    match gateway.send("UsersService", &request, None).await {
        Ok(response) => println!("HTTP {}", response.status),
        Err(GatewayError::Rejected { status, .. }) => println!("HTTP {status}"),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
