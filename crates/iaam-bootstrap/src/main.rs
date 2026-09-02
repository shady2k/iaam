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
use iaam_http::client::HttpClient;
use iaam_http::resilience::{RateLimiter as MarketRateLimiter, RetryPolicy};
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use zeroize::Zeroizing;

use crate::config::Config;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "iaam", about = "The iaam service and local administration CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the iaam server.
    Serve,
    /// Claim a fresh instance and print its owner token once.
    Claim {
        #[arg(long)]
        label: String,
    },
    /// Manage API tokens.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Manage broker credentials and access.
    Broker {
        #[command(subcommand)]
        command: BrokerCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// Issue a token for the existing sole owner.
    Issue {
        #[arg(long)]
        label: String,
        #[arg(long, value_enum, default_value_t = TokenScopeArg::Owner)]
        scope: TokenScopeArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TokenScopeArg {
    Owner,
    Agent,
    ReadOnly,
}

impl From<TokenScopeArg> for Scope {
    fn from(scope: TokenScopeArg) -> Self {
        match scope {
            TokenScopeArg::Owner => Self::Owner,
            TokenScopeArg::Agent => Self::Agent,
            TokenScopeArg::ReadOnly => Self::ReadOnly,
        }
    }
}

#[derive(Debug, Subcommand)]
enum BrokerCommand {
    /// Manage the broker encryption key.
    Key {
        #[command(subcommand)]
        command: BrokerKeyCommand,
    },
    /// Manage broker access credentials.
    Access {
        #[command(subcommand)]
        command: BrokerAccessCommand,
    },
}

#[derive(Debug, Subcommand)]
enum BrokerKeyCommand {
    /// Generate a broker encryption key at IAAM_BROKER_KEY_FILE.
    Generate,
    /// Re-encrypt all broker access with a new key.
    Rotate {
        #[arg(long)]
        old: std::path::PathBuf,
        #[arg(long)]
        new: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum BrokerAccessCommand {
    /// Add a broker credential read from standard input.
    Add {
        #[arg(long)]
        broker: String,
        #[arg(long, value_enum)]
        environment: BrokerEnvironmentArg,
    },
    /// Replace an active broker credential from standard input.
    Rotate {
        #[arg(long)]
        broker: String,
        #[arg(long, value_enum)]
        environment: BrokerEnvironmentArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BrokerEnvironmentArg {
    Prod,
    Sandbox,
}

impl From<BrokerEnvironmentArg> for Environment {
    fn from(environment: BrokerEnvironmentArg) -> Self {
        match environment {
            BrokerEnvironmentArg::Prod => Self::Prod,
            BrokerEnvironmentArg::Sandbox => Self::Sandbox,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum BrokerKeyError {
    #[error("key file {path} not found; run `iaam broker key generate`")]
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
    reject_legacy_environment()?;
    let cli = Cli::parse();
    let config = Config::from_env()?;

    match cli.command {
        Command::Serve => serve(config).await,
        Command::Claim { label } => {
            let store = SqliteStore::open(&config.database)?;
            let admin = SqliteAdapter::new(store);
            let token = claim_owner(&admin, &label).await?;
            println!("{token}");
            Ok(())
        }
        Command::Token {
            command: TokenCommand::Issue { label, scope },
        } => {
            let store = SqliteStore::open(&config.database)?;
            let admin = SqliteAdapter::new(store);
            let token = issue_token(&admin, &label, scope.into()).await?;
            println!("{token}");
            Ok(())
        }
        Command::Broker {
            command:
                BrokerCommand::Key {
                    command: BrokerKeyCommand::Generate,
                },
        } => {
            let path = broker_key_path(&config)?;
            Key::create_at(&path)?;
            println!("key created: {}", path.display());
            Ok(())
        }
        Command::Broker {
            command:
                BrokerCommand::Key {
                    command: BrokerKeyCommand::Rotate { old, new },
                },
        } => {
            let mut store = SqliteStore::open(&config.database)?;
            let old_key = read_broker_key(&old).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("old key could not be read: {error}"),
                )
            })?;
            let new_key = read_broker_key(&new).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("new key could not be read: {error}"),
                )
            })?;
            let rotated = provision::rotate_broker_access(&mut store, &old_key, &new_key)?;
            println!("broker accesses re-encrypted: {rotated}");
            Ok(())
        }
        Command::Broker {
            command:
                BrokerCommand::Access {
                    command:
                        BrokerAccessCommand::Add {
                            broker,
                            environment,
                        },
                },
        } => {
            let mut store = SqliteStore::open(&config.database)?;
            let key = read_broker_key(&broker_key_path(&config)?)?;
            let id = provision::add_broker_access(
                &mut store,
                &key,
                &broker,
                environment.into(),
                &read_token()?,
            )?;
            println!(
                "broker access {broker} ({}) provisioned: {id}",
                Environment::from(environment).code()
            );
            Ok(())
        }
        Command::Broker {
            command:
                BrokerCommand::Access {
                    command:
                        BrokerAccessCommand::Rotate {
                            broker,
                            environment,
                        },
                },
        } => {
            let mut store = SqliteStore::open(&config.database)?;
            let key = read_broker_key(&broker_key_path(&config)?)?;
            let id = provision::replace_broker_access(
                &mut store,
                &key,
                &broker,
                environment.into(),
                &read_token()?,
            )?;
            println!(
                "broker access {broker} ({}) replaced: {id}",
                Environment::from(environment).code()
            );
            Ok(())
        }
    }
}

fn legacy_replacement(is_set: impl Fn(&str) -> bool) -> Option<(&'static str, &'static str)> {
    [
        ("IAAM_ISSUE_OWNER_TOKEN", "token issue"),
        ("IAAM_ADD_BROKER_ACCESS", "broker access add"),
        ("IAAM_GENERATE_BROKER_KEY", "broker key generate"),
        (
            "IAAM_BROKER_KEY_OLD_FILE",
            "broker key rotate --old <path> --new <path>",
        ),
        (
            "IAAM_BROKER_KEY_NEW_FILE",
            "broker key rotate --old <path> --new <path>",
        ),
    ]
    .into_iter()
    .find(|(variable, _)| is_set(variable))
}

fn reject_legacy_environment() -> Result<(), Box<dyn std::error::Error>> {
    if let Some((variable, command)) =
        legacy_replacement(|variable| std::env::var_os(variable).is_some())
    {
        return Err(
            format!("environment variable {variable} was replaced by `iaam {command}`").into(),
        );
    }
    Ok(())
}

async fn serve(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    // Logging is mandatory: without it, acceptance debugging is impossible.
    // Sensitive fields are never logged — only the token's hash, never the
    // token itself, reaches the log (§14).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let store = SqliteStore::open(&config.database)?;
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
    let (router, _api) = build(state)?;

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address = %config.listen, "server started");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
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

/// Claim an instance and issue its first owner token.
///
/// Deciding that the instance is unclaimed and creating the token are one
/// atomic operation, and it lives behind `TokenAdmin`: this command names it
/// and prints its result, nothing more. Assembling a token record here as well
/// would be a second implementation of credential issuance — and the one that
/// mints the owner's token, so a change to issuance would pass it by in
/// silence.
///
/// The token is printed once. It is nowhere else: not in a log, and not in the
/// database, which keeps only its hash (§14).
async fn claim_owner(
    admin: &dyn TokenAdmin,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let issued = admin.claim_owner(label.to_owned()).await?;
    Ok(issued.token)
}

/// Issue a token for the existing sole owner. The token itself is printed
/// once and never stored: only its hash is kept in the database.
async fn issue_token(
    admin: &dyn TokenAdmin,
    label: &str,
    scope: Scope,
) -> Result<String, Box<dyn std::error::Error>> {
    let owner = match admin.sole_owner().await? {
        SoleOwner::Single(owner) => owner,
        SoleOwner::None => {
            return Err("instance has no owner: run `iaam claim --label <label>` first".into());
        }
        SoleOwner::Several => {
            return Err(
                "multiple owners in database: choosing which one should receive \
                        a token is impossible. These are signs of corruption in \
                        a single-user system — inspect the database, not the command"
                    .into(),
            );
        }
    };
    let issued = admin.issue_token(owner, label.to_owned(), scope).await?;
    Ok(issued.token)
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::{
        BrokerAccessCommand, BrokerCommand, BrokerEnvironmentArg, Cli, Command, SqliteAdapter,
        TokenCommand, TokenScopeArg, claim_owner, format_error_chain, legacy_replacement,
        read_broker_key,
    };
    use clap::Parser;

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
        assert!(text.contains("iaam broker key generate"));
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
    fn cli_parses_nested_token_scope() {
        let cli = Cli::try_parse_from([
            "iaam",
            "token",
            "issue",
            "--label",
            "Main",
            "--scope",
            "read-only",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Command::Token {
                command: TokenCommand::Issue {
                    scope: TokenScopeArg::ReadOnly,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cli_parses_broker_access_rotate_without_a_token_argument() {
        let cli = Cli::try_parse_from([
            "iaam",
            "broker",
            "access",
            "rotate",
            "--broker",
            "tinkoff",
            "--environment",
            "sandbox",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Command::Broker {
                command: BrokerCommand::Access {
                    command: BrokerAccessCommand::Rotate {
                        broker,
                        environment: BrokerEnvironmentArg::Sandbox,
                    },
                },
            } if broker == "tinkoff"
        ));
    }

    #[test]
    fn legacy_variables_name_their_replacement_commands() {
        let cases = [
            ("IAAM_ISSUE_OWNER_TOKEN", "token issue"),
            ("IAAM_ADD_BROKER_ACCESS", "broker access add"),
            ("IAAM_GENERATE_BROKER_KEY", "broker key generate"),
            (
                "IAAM_BROKER_KEY_OLD_FILE",
                "broker key rotate --old <path> --new <path>",
            ),
            (
                "IAAM_BROKER_KEY_NEW_FILE",
                "broker key rotate --old <path> --new <path>",
            ),
        ];

        for (variable, command) in cases {
            assert_eq!(
                legacy_replacement(|candidate| candidate == variable),
                Some((variable, command))
            );
        }
    }

    /// The property defended here is that exactly one of two simultaneous
    /// claims wins. Moving issuance behind `TokenAdmin` did not change it: two
    /// operating-system threads, one barrier, two connections to one file. Each
    /// thread drives the async port on a runtime of its own, because the race
    /// has to happen between threads rather than between two tasks that a
    /// single-threaded executor would interleave for them.
    #[test]
    fn concurrent_claims_leave_one_owner_and_refuse_the_other() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let path = std::env::temp_dir().join(format!(
            "iaam-bootstrap-concurrent-claim-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let first = SqliteAdapter::new(iaam_store::SqliteStore::open(&path).unwrap());
        let second = SqliteAdapter::new(iaam_store::SqliteStore::open(&path).unwrap());
        let barrier = Arc::new(Barrier::new(2));

        let claim = |adapter: SqliteAdapter, label: &'static str, barrier: Arc<Barrier>| {
            thread::spawn(move || {
                // The runtime is built before the barrier: the threads must meet
                // at the claim itself, not at each other's start-up.
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                barrier.wait();
                runtime
                    .block_on(claim_owner(&adapter, label))
                    .map_err(|error| error.to_string())
            })
        };
        let first_thread = claim(first, "Main", Arc::clone(&barrier));
        let second_thread = claim(second, "Savings", Arc::clone(&barrier));

        let first_result = first_thread.join().unwrap();
        let second_result = second_thread.join().unwrap();
        let results = [first_result, second_result];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let refusal = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one claim must be refused");
        assert!(refusal.to_string().contains("instance is already claimed"));

        let check = iaam_store::SqliteStore::open(&path).unwrap();
        assert!(matches!(
            check.sole_token_owner().unwrap(),
            iaam_store::broker_access::SoleOwner::Single(_)
        ));
        std::fs::remove_file(path).unwrap();
    }
}
