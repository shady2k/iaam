//! Composition root (§3.2).
//!
//! The only place that knows about both transport and adapters.
//! The architecture guard verifies that this remains true.

mod config;
mod provision;

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::market::HttpOutbound;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{
    BrokerChannelFactory, BrokerVault, ClassificationRuleStore, Scope, SoleOwner, SystemClock,
    TokenAdmin,
};
use iaam_broker::credentials::Key;
use iaam_broker::environment::Environment;
use iaam_core::ids::OwnerId;
use iaam_http::client::HttpClient;
use iaam_http::resilience::{RateLimiter as MarketRateLimiter, RetryPolicy};
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use zeroize::Zeroizing;

use crate::config::Config;

#[derive(Debug, thiserror::Error)]
enum BrokerKeyError {
    #[error(
        "key file {path} not found; set IAAM_GENERATE_BROKER_KEY=1 \
         or run make broker-key"
    )]
    Missing {
        path: String,
        #[source]
        source: iaam_broker::credentials::CryptoError,
    },
    #[error(
        "key file {path} exists but is unreadable or has an invalid format; \
         do not create a new one over it: that would make all provisioned \
         accesses unreadable"
    )]
    Existing {
        path: String,
        #[source]
        source: iaam_broker::credentials::CryptoError,
    },
}

fn read_broker_key(path: &std::path::Path) -> Result<Key, BrokerKeyError> {
    let path_text = path.display().to_string();
    let missing = matches!(
        std::fs::metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    Key::from_file(path).map_err(|source| {
        if missing {
            BrokerKeyError::Missing {
                path: path_text,
                source,
            }
        } else {
            BrokerKeyError::Existing {
                path: path_text,
                source,
            }
        }
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum RotationConfigError {
    #[error("set IAAM_BROKER_KEY_OLD_FILE for rotation")]
    MissingOld,
    #[error("set IAAM_BROKER_KEY_NEW_FILE for rotation")]
    MissingNew,
}

fn rotation_paths(
    old: Option<std::ffi::OsString>,
    new: Option<std::ffi::OsString>,
) -> Result<Option<(std::path::PathBuf, std::path::PathBuf)>, RotationConfigError> {
    match (old, new) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(RotationConfigError::MissingOld),
        (Some(_), None) => Err(RotationConfigError::MissingNew),
        (Some(old), Some(new)) => Ok(Some((
            std::path::PathBuf::from(old),
            std::path::PathBuf::from(new),
        ))),
    }
}

fn format_error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str("\ncause: ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn report_error(error: &dyn std::error::Error) {
    eprintln!("error: {}", format_error_chain(error));
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            report_error(error.as_ref());
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Logging is mandatory: without it, acceptance debugging is impossible.
    // Sensitive fields are never logged — only the token's hash, never the
    // token itself, reaches the log (§14).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let mut store = SqliteStore::open(&config.database)?;

    // One-time owner-token issuance: without it there is no way into the
    // system, and authentication cannot be deferred (§14).
    if let Ok(label) = std::env::var("IAAM_ISSUE_OWNER_TOKEN") {
        // Adapter without an encryption key: token issuance does not need
        // the key, and requiring it here would mean a lost token could not
        // be recovered until the broker was configured.
        let admin = SqliteAdapter::new(store);
        let token = issue_owner_token(&admin, &label).await?;
        println!("{token}");
        return Ok(());
    }

    // Re-encryption changes no key file: the new key has already been
    // prepared by the owner, while the old one remains available for
    // rollback.
    // The full set of ciphertexts is built first; then storage replaces it
    // in one transaction.
    if let Some((old_path, new_path)) = rotation_paths(
        std::env::var_os("IAAM_BROKER_KEY_OLD_FILE"),
        std::env::var_os("IAAM_BROKER_KEY_NEW_FILE"),
    )
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?
    {
        let old_key = read_broker_key(&old_path).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("old key could not be read: {error}"),
            )
        })?;
        let new_key = read_broker_key(&new_path).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("new key could not be read: {error}"),
            )
        })?;
        let rotated = provision::rotate_broker_access(&mut store, &old_key, &new_key)?;
        println!("broker accesses re-encrypted: {rotated}");
        return Ok(());
    }

    // Provisioning the encryption key for broker access. The program creates
    // the key itself and never returns it: what a person has not seen cannot
    // be forwarded or written to the wrong place (§14).
    if std::env::var("IAAM_GENERATE_BROKER_KEY").is_ok() {
        let path = broker_key_path(&config)?;
        Key::create_at(&path)?;
        println!("key created: {}", path.display());
        return Ok(());
    }

    // Receiving a broker token. Read it from standard input rather than a
    // command-line argument: the process list is visible to the whole
    // machine, while shell history outlives the session.
    if let Ok(broker) = std::env::var("IAAM_ADD_BROKER_ACCESS") {
        let key = read_broker_key(&broker_key_path(&config)?)?;
        // The environment is explicit and has no default. A default here
        // would silently store a sandbox token as production: the gateway
        // would refuse the first request, and the refusal text would not
        // reveal the environment — verified against a live gateway.
        let environment = broker_environment()?;
        let id =
            provision::add_broker_access(&mut store, &key, &broker, environment, &read_token()?)?;
        println!(
            "broker access {broker} ({}) provisioned: {id}",
            environment.code()
        );
        return Ok(());
    }

    // The broker-access encryption key is optional: without it the server
    // starts and broker routes return 503. A configured but unreadable key
    // is different: it is a configuration typo, and silent startup would
    // hide it until the first access was provisioned.
    let broker_key = config
        .broker_key
        .as_deref()
        .map(read_broker_key)
        .transpose()?;
    let market_store = SqliteStore::open(&config.database)?;
    let http = Arc::new(HttpOutbound::new(
        HttpClient::new(),
        RetryPolicy::new(4, std::time::Duration::from_millis(100)),
        Arc::new(MarketRateLimiter::new(std::time::Duration::from_millis(
            100,
        ))),
    ));

    // The same adapter serves as both fact storage and broker-access
    // storage: both use one database connection, and a second instance
    // would mean a second writer.
    let adapter = Arc::new(SqliteAdapter::with_broker_key(store, broker_key));
    let broker: Arc<dyn BrokerVault> = adapter.clone();
    let channels: Arc<dyn BrokerChannelFactory> = adapter.clone();
    let rules: Arc<dyn ClassificationRuleStore> = adapter.clone();
    let broker_dictionary: Arc<dyn iaam_app::ports::BrokerDictionary> = adapter.clone();
    let tokens: Arc<dyn TokenAdmin> = adapter.clone();
    let services = Arc::new(AppServices {
        store: adapter.clone(),
        directory: adapter.clone(),
        broker,
        tokens,
        clock: Arc::new(SystemClock),
        channels,
        categories: adapter,
        rules,
        http,
        broker_dictionary,
        market_store: Arc::new(tokio::sync::Mutex::new(market_store)),
    });
    let limiter = Arc::new(RateLimiter::new(config.rate_limit, config.rate_window));
    let state = ServerState::new(services, limiter);

    // Claiming the instance: until an owner exists, a one-time code is
    // generated. It is printed to stderr — stdout carries command results,
    // and a secret must not be mixed into it. `iaam-server` decides whether
    // a code is needed: the code is state of its route; printing remains
    // here because the program, not the library, must print it (§14).
    if let Some(code) = iaam_server::claim::arm(&state).await? {
        eprintln!("no owner in database: the instance has not been claimed.");
        eprintln!("claim code (valid for 15 minutes, one-time): {code}");
        eprintln!("exchange it for an owner token in one request:");
        eprintln!("  POST /v1/claim {{\"code\": \"…\", \"label\": \"laptop\"}}");
    }

    let (router, _api) = build(state);

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address = %config.listen, "server started");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Broker environment from the environment. Missing variable means refusal.
fn broker_environment() -> Result<Environment, Box<dyn std::error::Error>> {
    let value = std::env::var("IAAM_BROKER_ENVIRONMENT").map_err(|_| {
        "variable IAAM_BROKER_ENVIRONMENT is not set: prod or sandbox. \
         Environments have different tokens, and no choice can be made for you"
    })?;
    Environment::parse(&value)
        .ok_or_else(|| format!("unknown broker environment {value}: use prod or sandbox").into())
}

/// Path to the key file. Missing variable means refusal, not a default.
fn broker_key_path(config: &Config) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    config
        .broker_key
        .clone()
        .ok_or_else(|| "variable IAAM_BROKER_KEY_FILE is not set".into())
}

/// Read the token from standard input.
///
/// Returned in zeroizing memory: the plaintext token lives only until
/// encryption and does not remain in freed process memory.
fn read_token() -> Result<Zeroizing<String>, Box<dyn std::error::Error>> {
    // The prompt goes to stderr: stdout carries the command's response,
    // and the prompt must not be mixed into it.
    eprintln!("paste the broker token and finish input (Ctrl-D):");
    let mut token = Zeroizing::new(String::new());
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut token)?;
    Ok(token)
}

/// Issue an owner token. The token itself is printed **once** and never
/// stored: only its hash is kept in the database.
///
/// **This is the recovery door when the token is lost.** Recovery must
/// require proof stronger than what is being recovered; here that proof is
/// access to the machine console: anyone who can run this command can
/// already read the database file.
/// Therefore the door remains open, but remains console-only — there is no
/// API route that issues owner tokens, and there never will be.
///
/// The owner is read from the database rather than created on every call.
/// The previous version always called `OwnerId::new_random()`: a second run
/// created a second owner, the first owner's portfolio appeared to vanish —
/// its token still worked, but there were no events for the new owner — and
/// `sole_token_owner` began returning `Several`, refusing to provision
/// broker access.
async fn issue_owner_token(
    admin: &dyn TokenAdmin,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let owner = match admin.sole_owner().await? {
        SoleOwner::Single(owner) => owner,
        // No owner — first issuance: the instance is claimed by the
        // console, bypassing the one-time code.
        SoleOwner::None => OwnerId::new_random(),
        SoleOwner::Several => {
            return Err(
                "multiple owners in database: choosing which one should receive \
                        a token is impossible. These are signs of corruption in \
                        a single-user system — inspect the database, not the command"
                    .into(),
            );
        }
    };
    let issued = admin
        .issue_token(owner, label.to_owned(), Scope::Owner)
        .await?;
    Ok(issued.token)
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::{RotationConfigError, format_error_chain, read_broker_key, rotation_paths};

    #[test]
    fn missing_broker_key_explains_generation_command() {
        let path = std::env::temp_dir().join(format!(
            "iaam-bootstrap-missing-broker-key-{}",
            std::process::id()
        ));
        let error = match read_broker_key(&path) {
            Ok(_) => panic!("test key file unexpectedly exists"),
            Err(error) => error,
        };

        let text = format_error_chain(&error);
        assert!(text.contains("IAAM_GENERATE_BROKER_KEY=1"));
        assert!(text.contains("make broker-key"));
        assert!(text.contains("key file"));
        assert!(!text.contains("KeyFileUnreadable"));
        assert!(!text.contains("Invalid {"));
    }

    #[test]
    fn invalid_existing_broker_key_warns_against_replacement() {
        let path = std::env::temp_dir().join(format!(
            "iaam-bootstrap-invalid-broker-key-{}",
            std::process::id()
        ));
        if let Err(error) = std::fs::write(&path, "not-base64") {
            panic!("could not prepare test key file: {error}");
        }

        let error = match read_broker_key(&path) {
            Ok(_) => panic!("corrupted test key was unexpectedly accepted"),
            Err(error) => error,
        };
        let _ = std::fs::remove_file(&path);

        let text = format_error_chain(&error);
        assert!(text.contains("exists"));
        assert!(text.contains("invalid format"));
        assert!(text.contains("do not create a new one over it"));
        assert!(!text.contains("IAAM_GENERATE_BROKER_KEY=1"));
        assert!(!text.contains("Invalid {"));
    }

    #[test]
    fn rotation_requires_both_paths_and_never_echoes_values() {
        assert_eq!(
            rotation_paths(None, Some("new-secret-path".into())),
            Err(RotationConfigError::MissingOld)
        );
        assert_eq!(
            rotation_paths(Some("old-secret-path".into()), None),
            Err(RotationConfigError::MissingNew)
        );
        let old_error = rotation_paths(None, Some("new-secret-path".into())).unwrap_err();
        let new_error = rotation_paths(Some("old-secret-path".into()), None).unwrap_err();
        assert!(!old_error.to_string().contains("new-secret-path"));
        assert!(!new_error.to_string().contains("old-secret-path"));
    }
}
