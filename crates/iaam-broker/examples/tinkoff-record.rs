//! Capture raw responses from the T-Invest sandbox API.
//!
//! This command exists only to prepare frozen samples: it takes an existing
//! access record from the database, but never prints or stores the decrypted
//! token. After anonymisation, the samples are used by tests without a network.
//!
//! ```text
//! IAAM_DATABASE=/path/to/iaam.sqlite \
//! IAAM_BROKER_KEY_FILE=/path/to/broker.key \
//! nix develop -c cargo run -p iaam-broker --example tinkoff-record
//! ```
//!
//! The sample directory is set by `IAAM_TINKOFF_FIXTURES_DIR` and defaults to
//! `tests/fixtures/api/`. The interval length in whole days is set by
//! `IAAM_TINKOFF_INTERVAL_DAYS` and defaults to 30.

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use iaam_broker::credentials::{BrokerScope, Key, SealedToken, open};
use iaam_broker::environment::Environment;
use iaam_http::client::HttpClient;
use iaam_http::{Destination, HttpRequest, RequestBody};
use iaam_store::SqliteStore;
use iaam_store::broker_access::SoleOwner;
use iaam_store::documents::BrokerCode;
use serde_json::{Value, json};

const BROKER: &str = "tinkoff";
const FIXTURES_DIR_ENV: &str = "IAAM_TINKOFF_FIXTURES_DIR";
const INTERVAL_DAYS_ENV: &str = "IAAM_TINKOFF_INTERVAL_DAYS";
const DEFAULT_INTERVAL_DAYS: u64 = 30;

const ACCOUNTS_METHOD: &str = "tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts";
const PORTFOLIO_METHOD: &str =
    "tinkoff.public.invest.api.contract.v1.OperationsService/GetPortfolio";
const OPERATIONS_METHOD: &str =
    "tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let fixtures_dir = env::var_os(FIXTURES_DIR_ENV)
        .map_or_else(|| "tests/fixtures/api/".into(), std::path::PathBuf::from);
    let interval_days = interval_days()?;
    let (from, to) = interval(interval_days)?;

    let store = SqliteStore::open(&required_path("IAAM_DATABASE")?)?;
    let key = Key::from_file(&required_path("IAAM_BROKER_KEY_FILE")?)?;
    let owner = match store.sole_token_owner()? {
        SoleOwner::Single(owner) => owner,
        SoleOwner::None => {
            return Err(io::Error::other("the database has no broker-access owner").into());
        }
        SoleOwner::Several => {
            return Err(io::Error::other(
                "the database has several owners: the command refuses to choose one",
            )
            .into());
        }
    };
    let broker = BrokerCode::parse(BROKER)
        .ok_or_else(|| io::Error::other("the internal broker code is empty"))?;
    let access = store
        .find_broker_access(owner, &broker, Environment::Sandbox.code())?
        .ok_or_else(|| io::Error::other("sandbox access to tinkoff is not configured"))?;
    if BrokerScope::parse(&access.scope) != Some(BrokerScope::ReadOnly) {
        return Err(io::Error::other("tinkoff access is not read-only").into());
    }

    let (nonce, ciphertext) = access.sealed_parts();
    let token = open(&key, &SealedToken::of(nonce.to_vec(), ciphertext.to_vec()))?;
    let client = HttpClient::new();

    // Request only open accounts: a closed account is unsuitable for the
    // following calls and would make the sample set non-deterministic.
    let accounts = fetch_raw(
        &client,
        Destination::TinkoffSandbox,
        ACCOUNTS_METHOD,
        token.expose(),
        json!({"status": "ACCOUNT_STATUS_OPEN"}),
    )
    .await?;
    write_fixture(&fixtures_dir, "tinkoff-accounts.json", &accounts)?;
    let account_id = account_id(&accounts)?;

    let portfolio = fetch_raw(
        &client,
        Destination::TinkoffSandbox,
        PORTFOLIO_METHOD,
        token.expose(),
        json!({"accountId": account_id.as_str()}),
    )
    .await?;
    write_fixture(&fixtures_dir, "tinkoff-portfolio.json", &portfolio)?;

    let operations = fetch_raw(
        &client,
        Destination::TinkoffSandbox,
        OPERATIONS_METHOD,
        token.expose(),
        json!({
            "accountId": account_id.as_str(),
            "from": from,
            "to": to,
            "limit": 1000,
        }),
    )
    .await?;
    // Freezing only the first page would make the file look complete while
    // leaving later operations outside the interval.
    ensure_complete_operations(&operations)?;
    write_fixture(&fixtures_dir, "tinkoff-operations.json", &operations)?;

    Ok(())
}

fn required_path(variable: &str) -> Result<std::path::PathBuf, io::Error> {
    env::var_os(variable)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("environment variable {variable} is not set")))
}

fn interval_days() -> Result<u64, io::Error> {
    let Some(value) = env::var_os(INTERVAL_DAYS_ENV) else {
        return Ok(DEFAULT_INTERVAL_DAYS);
    };
    let value = value
        .into_string()
        .map_err(|_| io::Error::other(format!("{INTERVAL_DAYS_ENV} is not UTF-8")))?;
    let days = value.parse::<u64>().map_err(|_| {
        io::Error::other(format!(
            "{INTERVAL_DAYS_ENV} must be a whole number of days"
        ))
    })?;
    if days == 0 {
        return Err(io::Error::other(format!(
            "{INTERVAL_DAYS_ENV} must be greater than zero"
        )));
    }
    Ok(days)
}

fn interval(days: u64) -> Result<(String, String), io::Error> {
    let to = OffsetDateTime::now_utc();
    let days = i64::try_from(days)
        .map_err(|_| io::Error::other("the interval is too large for the calendar"))?;
    let from = to
        .checked_sub(Duration::days(days))
        .ok_or_else(|| io::Error::other("the interval extends before the start of the calendar"))?;

    let from = from.format(&Rfc3339).map_err(|error| {
        io::Error::other(format!("the interval start cannot be formatted: {error}"))
    })?;
    let to = to.format(&Rfc3339).map_err(|error| {
        io::Error::other(format!("the interval end cannot be formatted: {error}"))
    })?;
    Ok((from, to))
}

async fn fetch_raw(
    client: &HttpClient,
    destination: Destination,
    method: &str,
    token: &str,
    body: Value,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let request = HttpRequest::post(
        destination,
        method,
        RequestBody::Json(serde_json::to_string(&body)?),
    )
    .with_bearer(token);
    let response = client.send(&request).await?;
    if !(200..=299).contains(&response.status) {
        // The refusal body is written neither to a file nor to the error: the
        // gateway is not required to separate diagnostics from owner data.
        return Err(io::Error::other(format!("{method} returned HTTP {}", response.status)).into());
    }
    Ok(response.body)
}

fn account_id(body: &[u8]) -> Result<String, Box<dyn Error>> {
    let response: Value = serde_json::from_slice(body)?;
    let accounts = response
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("GetAccounts response has no account array"))?;
    if accounts.is_empty() {
        return Err(io::Error::other("GetAccounts returned an empty account list").into());
    }
    if accounts.len() > 1 {
        return Err(io::Error::other(
            "GetAccounts returned several accounts: the command refuses to choose one",
        )
        .into());
    }
    accounts
        .first()
        .and_then(|account| account.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("GetAccounts response has no account identifier").into())
}

fn ensure_complete_operations(body: &[u8]) -> Result<(), Box<dyn Error>> {
    let response: Value = serde_json::from_slice(body)?;
    let has_next = response
        .get("hasNext")
        .or_else(|| response.get("has_next"))
        .and_then(Value::as_bool)
        .ok_or_else(|| io::Error::other("operations response has no pagination flag"))?;
    if has_next {
        return Err(io::Error::other(
            "GetOperationsByCursor returned an incomplete page: sample not written",
        )
        .into());
    }
    Ok(())
}

fn write_fixture(dir: &Path, name: &str, body: &[u8]) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    fs::write(&path, body)?;
    eprintln!("response captured: {}", path.display());
    Ok(())
}
