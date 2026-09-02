//! Live channel check against the T-Invest sandbox.
//!
//! **A second verification mode.** Ordinary `cargo test` never touches the
//! network: it parses frozen samples, so it is repeatable, fast, and
//! independent of someone else's availability. This file is compiled only
//! with the `sandbox` feature and answers a different question—not “do we
//! parse correctly?” but “has the world changed beneath us?”: is the gateway
//! alive, is the embedded trust root valid, and does the broker accept the
//! configured access?
//!
//! ```text
//! nix develop -c cargo test -p iaam-broker --features sandbox
//! ```
//!
//! The method is **ordinary**, not from `SandboxService`. T-Invest offers two
//! approaches: call ordinary methods at the sandbox address (recommended), or
//! use sandbox methods. Mixing approaches—an ordinary method at the production
//! address or a sandbox method at the sandbox address—elicits `40003`, a token
//! complaint, so the investigator blames the token rather than the route.
//!
//! Requires configured access: `IAAM_DATABASE` and `IAAM_BROKER_KEY_FILE` point
//! to the database and key, while the access itself is configured with
//! `IAAM_ADD_BROKER_ACCESS=tinkoff`. Missing any of them is a **refusal**, not
//! a skip: the mode was requested explicitly, and silently checking nothing
//! would lie with a green run.
#![cfg(feature = "sandbox")]

use std::path::PathBuf;

use iaam_broker::credentials::{BrokerScope, Key, SealedToken, open};
use iaam_broker::environment::Environment;
use iaam_http::client::HttpClient;
use iaam_http::{Destination, HttpRequest, RequestBody};
use iaam_store::SqliteStore;
use iaam_store::broker_access::SoleOwner;
use iaam_store::documents::BrokerCode;

fn required(variable: &str) -> PathBuf {
    PathBuf::from(std::env::var(variable).unwrap_or_else(|_| {
        panic!("sandbox mode requested, but {variable} is not set; nothing to check")
    }))
}

/// Whether the response status is successful.
///
/// A free function rather than an `is_*` trait method on `u16`: clippy requires
/// `is_*` to take `self` by reference, but borrowing a two-byte number is
/// unnecessary. `reqwest::StatusCode::is_success` is no longer available—the
/// transport returns a number.
const fn status_is_success(status: u16) -> bool {
    status >= 200 && status <= 299
}

#[tokio::test]
async fn the_sandbox_accepts_the_provisioned_access() {
    // Check the whole chain at once: key from file, ciphertext from database,
    // decryption, embedded trust root, and authorisation header. Each link is
    // covered separately by ordinary tests; this proves they meet at the real
    // gateway.
    let store = SqliteStore::open(&required("IAAM_DATABASE")).expect("database opened");
    let key = Key::from_file(&required("IAAM_BROKER_KEY_FILE")).expect("key read");
    let SoleOwner::Single(owner) = store.sole_token_owner().expect("owner read") else {
        panic!("database has no sole owner; issue an owner token first");
    };
    let broker = BrokerCode::parse("tinkoff").expect("broker code");
    // Select the environment explicitly: production access must not be used
    // against the sandbox, and “whatever is found” would mean the wrong route.
    let access = store
        .find_broker_access(owner, &broker, Environment::Sandbox.code())
        .expect("access read")
        .expect("Tinkoff sandbox access is not configured: run `iaam broker access add` locally");

    assert_eq!(
        BrokerScope::parse(&access.scope),
        Some(BrokerScope::ReadOnly),
        "access is not read-only; do not use it with the broker"
    );

    let (nonce, ciphertext) = access.sealed_parts();
    let token = open(&key, &SealedToken::of(nonce.to_vec(), ciphertext.to_vec()))
        .expect("access decrypted");

    let request = HttpRequest::post(
        Destination::TinkoffSandbox,
        "tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
        RequestBody::Json("{}".to_owned()),
    )
    .with_bearer(token.expose());
    let response = HttpClient::new()
        .send(&request)
        .await
        .expect("gateway responded");

    // Include the response body deliberately: “HTTP 500” alone cannot
    // distinguish a broken gateway from an invalid token, and this one message
    // is all the investigation gets. The body contains no secret: the token is
    // not returned there.
    let status = response.status;
    let body = String::from_utf8(response.body).unwrap_or_default();
    assert!(
        status_is_success(status),
        "sandbox rejected configured access: HTTP {status}: {body}"
    );
}
