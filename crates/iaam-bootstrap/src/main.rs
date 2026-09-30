//! Composition root (§3.2).
//!
//! The only place that knows about both transport and adapters.
//! The architecture guard verifies that this remains true.

mod bundle;
mod config;
mod instance;
mod provision;

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::market::HttpOutbound;
use iaam_app::adapters::profile_ledger::StoreVersionLedger;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ingest::profile::ProfileCatalogue;
use iaam_app::ports::{
    BrokerChannelFactory, BrokerVault, ClassificationRuleStore, Scope, SoleOwner, SystemClock,
    TokenAdmin,
};
use iaam_broker::credentials::Key;
use iaam_broker::environment::Environment;
use iaam_http::{BrokerEgress, Gateway};
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use zeroize::Zeroizing;

use crate::config::{Config, Place, PlaceSource, Places};
use crate::instance::{ensure_private_directory, open_database};
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
    /// Show where the instance's database and broker key live.
    Status,
    /// Claim a fresh instance and print its owner token once.
    ///
    /// The one command that creates a database: the file, and its
    /// directory with mode 0700, come into being here and nowhere else.
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
    /// Move an instance's transferable state in and out of a file (§14).
    ///
    /// A local administration command and not a route: see the doc comment
    /// on `crate::bundle` for why an export is not the same act as reading a
    /// report and does not belong beside them on the HTTP surface.
    Bundle {
        #[command(subcommand)]
        command: BundleCommand,
    },
}

#[derive(Debug, Subcommand)]
enum BundleCommand {
    /// Export the sole owner's bundle to a file.
    Export {
        /// Where to write the archive. Refused if it already exists.
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Restore an archive written by `bundle export` into this instance.
    Import {
        /// The archive to read.
        #[arg(long)]
        input: std::path::PathBuf,
        /// Required when this instance already holds journal facts: an
        /// explicit acknowledgement that the archive is merged into them
        /// rather than restored into an empty database.
        #[arg(long)]
        merge: bool,
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
    /// Generate a broker encryption key at the broker key place (the
    /// default place, or IAAM_BROKER_KEY_FILE when set).
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
    execute(cli.command, |name| std::env::var(name).ok()).await
}

/// Runs one command against one resolved instance.
///
/// The resolver decides both places for every command; `status` and `broker
/// key generate` need the places alone, while the commands that open the
/// instance's database resolve the whole configuration. No arm creates
/// anything by accident: creation belongs to `claim` (the database, with
/// its directory) and to `broker key generate` (the key, with its
/// directory). Every command that finds no database refuses, naming the
/// place it looked at, having created no file and no directory.
async fn execute<F>(command: Command, get: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn(&str) -> Option<String>,
{
    match command {
        Command::Serve => serve(Config::from_lookup(&get)?).await,
        Command::Status => {
            let places = Places::from_lookup(&get)?;
            print!("{}", status_report(&places));
            Ok(())
        }
        Command::Broker {
            command:
                BrokerCommand::Key {
                    command: BrokerKeyCommand::Generate,
                },
        } => {
            let places = Places::from_lookup(&get)?;
            generate_key(&places).await
        }
        Command::Claim { label } => {
            let Places {
                database,
                broker_key: _,
            } = Places::from_lookup(&get)?;
            ensure_private_directory(&database.path, "the instance's database")?;
            let store = SqliteStore::open(&database.path)?;
            let admin = SqliteAdapter::new(store);
            let token = claim_owner(&admin, &label).await?;
            println!("{token}");
            Ok(())
        }
        Command::Token {
            command: TokenCommand::Issue { label, scope },
        } => {
            let config = Config::from_lookup(&get)?;
            let store = open_database(&config.database)?;
            let admin = SqliteAdapter::new(store);
            let token = issue_token(&admin, &label, scope.into()).await?;
            println!("{token}");
            Ok(())
        }
        Command::Broker {
            command:
                BrokerCommand::Key {
                    command: BrokerKeyCommand::Rotate { old, new },
                },
        } => {
            let config = Config::from_lookup(&get)?;
            let mut store = open_database(&config.database)?;
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
            let config = Config::from_lookup(&get)?;
            let mut store = open_database(&config.database)?;
            let key = read_broker_key(&config.broker_key.path)?;
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
            let config = Config::from_lookup(&get)?;
            let mut store = open_database(&config.database)?;
            let key = read_broker_key(&config.broker_key.path)?;
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
        Command::Bundle {
            command: BundleCommand::Export { output },
        } => {
            let config = Config::from_lookup(&get)?;
            let store = open_database(&config.database)?;
            let summary = bundle::export_to_file(&store, &output)?;
            println!("{summary}");
            Ok(())
        }
        Command::Bundle {
            command: BundleCommand::Import { input, merge },
        } => {
            let config = Config::from_lookup(&get)?;
            let mut store = open_database(&config.database)?;
            let report = bundle::import_from_file(&mut store, &input, merge)?;
            println!("{report}");
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

    let store = open_database(&config.database)?;
    let broker_key = broker_key_for_serve(&config.broker_key)?;
    let market_store = open_database(&config.database)?;
    // The one gateway of the process. Each broker endpoint is owned by one
    // process for this gateway's lifetime; boot-clock budgets, spacing and the
    // rolling 24-hour ceiling persist in the configured outbound tally.
    let broker_egress = BrokerEgress::from_env()?;
    let gateway = Arc::new(Gateway::production(broker_egress, &config.database)?);
    let http = Arc::new(HttpOutbound::new(gateway.clone()));

    // Assembled once, here, because the catalogue belongs to the deployment.
    // Bundled profiles always; the operator's directory only where he named
    // one, and never from a default path.
    //
    // Bound against this instance's own record before the store is handed on,
    // and before anything reads a document. A version is a name for a content
    // (decision 0019 §5): a profile whose content changed under a version this
    // instance already recorded is refused here, rather than stamping a second
    // content on a `ParserVersion` that facts in the journal already carry.
    let profiles = Arc::new(
        match &config.source_profiles {
            Some(directory) => ProfileCatalogue::with_local(directory),
            None => ProfileCatalogue::bundled(),
        }
        .bound_by(&mut StoreVersionLedger::new(&store)),
    );
    // Published on the catalogue route as well, but an operator whose import
    // stopped working reads the log first, and a refusal nobody sees is the
    // silence this whole arrangement exists to break.
    for refused in profiles.refused() {
        tracing::warn!(
            profile = refused.id.as_deref().unwrap_or("unnamed"),
            reason = %refused.reason,
            "a source profile was refused and will not read documents"
        );
    }

    // The same adapter serves as both fact storage and broker-access
    // storage: both use one database connection, and a second instance
    // would mean a second writer. Every broker channel it opens sends through
    // the process's one gateway, built above, so channels and market sources
    // draw on the same budgets.
    let adapter = Arc::new(SqliteAdapter::with_broker_key(
        store,
        broker_key,
        gateway.clone(),
    ));
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
        profiles,
        running_syncs: iaam_app::sync::RunningSyncs::default(),
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

/// Reads the broker key for `serve`, which may run without one.
///
/// The key place is always resolved, but `serve` still starts without
/// encryption when the default place holds no file: an instance that never
/// talks to a broker has none, and the routes that would need it answer
/// `not_configured` until a key is generated and the service restarted. A
/// place named by `IAAM_BROKER_KEY_FILE` is a different fact — the operator
/// said the key is there, so a missing file is a refusal, not a silent
/// start without encryption.
fn broker_key_for_serve(place: &Place) -> Result<Option<Key>, Box<dyn std::error::Error>> {
    match place.source {
        PlaceSource::Variable => Ok(Some(read_broker_key(&place.path)?)),
        PlaceSource::Default if place.path.is_file() => Ok(Some(read_broker_key(&place.path)?)),
        PlaceSource::Default => Ok(None),
    }
}

/// The report `iaam status` prints: where the instance's database and
/// broker key live, whether each file is there, and who chose the place.
/// Nothing is read from inside the database and no secret is read at all —
/// this command exists to be safe to run before anything exists.
fn status_report(places: &Places) -> String {
    format!(
        "database: {} ({}; {})\nbroker key: {} ({}; {})\n",
        places.database.path.display(),
        place_source_text(places.database.source, "IAAM_DATABASE"),
        existence_text(places.database.path.exists()),
        places.broker_key.path.display(),
        place_source_text(places.broker_key.source, "IAAM_BROKER_KEY_FILE"),
        existence_text(places.broker_key.path.exists()),
    )
}

/// `iaam broker key generate`: writes the key at the resolved place, but
/// only onto an instance that exists — the database is opened first, so a
/// command refused for a missing instance has created no file and no
/// directory, not even the key's.
async fn generate_key(places: &Places) -> Result<(), Box<dyn std::error::Error>> {
    open_database(&places.database)?;
    ensure_private_directory(&places.broker_key.path, "the broker key file")?;
    Key::create_at(&places.broker_key.path)?;
    println!("key created: {}", places.broker_key.path.display());
    Ok(())
}

fn place_source_text(source: PlaceSource, variable: &'static str) -> &'static str {
    match source {
        PlaceSource::Variable => variable,
        PlaceSource::Default => "default",
    }
}

fn existence_text(exists: bool) -> &'static str {
    if exists { "present" } else { "absent" }
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
        BrokerAccessCommand, BrokerCommand, BrokerEnvironmentArg, BrokerKeyCommand, BundleCommand,
        Cli, Command, Config, Places, SqliteAdapter, TokenCommand, TokenScopeArg, claim_owner,
        execute, format_error_chain, legacy_replacement, read_broker_key, serve, status_report,
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
    fn cli_parses_bundle_export_and_import() {
        let export =
            Cli::try_parse_from(["iaam", "bundle", "export", "--output", "out.json"]).unwrap();
        assert!(matches!(
            export.command,
            Command::Bundle {
                command: BundleCommand::Export { output }
            } if output == std::path::Path::new("out.json")
        ));

        let import =
            Cli::try_parse_from(["iaam", "bundle", "import", "--input", "in.json", "--merge"])
                .unwrap();
        assert!(matches!(
            import.command,
            Command::Bundle {
                command: BundleCommand::Import { input, merge: true }
            } if input == std::path::Path::new("in.json")
        ));

        let import_no_merge =
            Cli::try_parse_from(["iaam", "bundle", "import", "--input", "in.json"]).unwrap();
        assert!(matches!(
            import_no_merge.command,
            Command::Bundle {
                command: BundleCommand::Import { merge: false, .. }
            }
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

    /// A lookup closure over a private temporary home: it stands in for the
    /// environment, so the real home directory is never touched.
    fn lookup_with(values: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |name| {
            owned
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn places_with(values: &[(&str, &str)]) -> Places {
        Places::from_lookup(lookup_with(values)).unwrap()
    }

    fn config_with(values: &[(&str, &str)]) -> Config {
        Config::from_lookup(lookup_with(values)).unwrap()
    }

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("iaam-bootstrap-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn directory_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn token_issue_without_a_database_refuses_and_creates_nothing() {
        let home = temp_home("token-refusal");
        let database = home.join(".local/share/iaam/iaam.db");
        let home_str = home.to_str().unwrap();
        let values = [("HOME", home_str)];
        let lookup = lookup_with(&values);

        let error = execute(
            Command::Token {
                command: TokenCommand::Issue {
                    label: "Main".to_owned(),
                    scope: TokenScopeArg::Owner,
                },
            },
            lookup,
        )
        .await
        .unwrap_err();

        let text = format_error_chain(error.as_ref());
        assert!(text.contains("no database at"), "{text}");
        assert!(
            text.contains(database.display().to_string().as_str()),
            "{text}"
        );
        assert!(text.contains("iaam claim"), "{text}");
        assert!(
            !database.exists(),
            "the refused command must not create the file"
        );
        assert!(
            !home.join(".local").exists(),
            "no directory may appear either"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn bundle_export_without_a_database_refuses_and_creates_nothing() {
        let home = temp_home("bundle-refusal");
        let database = home.join(".local/share/iaam/iaam.db");
        let output = home.join("out.bundle.json");
        let home_str = home.to_str().unwrap();
        let values = [("HOME", home_str)];
        let lookup = lookup_with(&values);

        let error = execute(
            Command::Bundle {
                command: BundleCommand::Export {
                    output: output.clone(),
                },
            },
            lookup,
        )
        .await
        .unwrap_err();

        let text = format_error_chain(error.as_ref());
        assert!(text.contains("no database at"), "{text}");
        assert!(
            text.contains(database.display().to_string().as_str()),
            "{text}"
        );
        assert!(!database.exists());
        assert!(!output.exists(), "no archive was written either");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn broker_access_add_without_a_database_refuses_before_reading_input() {
        let home = temp_home("access-refusal");
        let database = home.join(".local/share/iaam/iaam.db");
        let home_str = home.to_str().unwrap();
        let values = [("HOME", home_str)];
        let lookup = lookup_with(&values);

        let error = execute(
            Command::Broker {
                command: BrokerCommand::Access {
                    command: BrokerAccessCommand::Add {
                        broker: "tinkoff".to_owned(),
                        environment: BrokerEnvironmentArg::Sandbox,
                    },
                },
            },
            lookup,
        )
        .await
        .unwrap_err();

        let text = format_error_chain(error.as_ref());
        assert!(text.contains("no database at"), "{text}");
        assert!(
            text.contains(database.display().to_string().as_str()),
            "{text}"
        );
        assert!(!database.exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn serve_without_a_database_refuses_and_creates_nothing() {
        let home = temp_home("serve-refusal");
        let database = home.join(".local/share/iaam/iaam.db");
        let home_str = home.to_str().unwrap();
        let values = [("HOME", home_str)];

        let error = serve(config_with(&values)).await.unwrap_err();

        let text = format_error_chain(error.as_ref());
        assert!(text.contains("no database at"), "{text}");
        assert!(
            text.contains(database.display().to_string().as_str()),
            "{text}"
        );
        assert!(!database.exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn claim_creates_the_database_and_its_private_directory() {
        let home = temp_home("claim-creates");
        let database = home.join(".local/share/iaam/iaam.db");
        let home_str = home.to_str().unwrap();
        let values = [("HOME", home_str)];
        let lookup = lookup_with(&values);

        execute(
            Command::Claim {
                label: "Main".to_owned(),
            },
            lookup,
        )
        .await
        .expect("claim creates the instance");

        assert!(database.is_file(), "claim created the database");
        assert_eq!(
            directory_mode(&home.join(".local/share/iaam")),
            0o700,
            "the instance directory is private"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn a_set_variable_moves_the_instance_and_the_default_place_stays_untouched() {
        let home = temp_home("override");
        let home_str = home.to_str().unwrap();
        let database = home.join("other.db");
        let values = [
            ("IAAM_DATABASE", database.to_str().unwrap()),
            ("HOME", home_str),
        ];
        let lookup = lookup_with(&values);

        execute(
            Command::Claim {
                label: "Main".to_owned(),
            },
            lookup,
        )
        .await
        .expect("claim honours the override");

        assert!(database.is_file());
        assert!(
            !home.join(".local").exists(),
            "the default place was not created"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn broker_key_generate_without_an_instance_refuses_and_creates_nothing() {
        let home = temp_home("generate-refusal");
        let database = home.join(".local/share/iaam/iaam.db");
        let home_str = home.to_str().unwrap();
        let values = [("HOME", home_str)];
        let lookup = lookup_with(&values);

        let error = execute(
            Command::Broker {
                command: BrokerCommand::Key {
                    command: BrokerKeyCommand::Generate,
                },
            },
            lookup,
        )
        .await
        .unwrap_err();

        let text = format_error_chain(error.as_ref());
        assert!(text.contains("no database at"), "{text}");
        assert!(
            text.contains(database.display().to_string().as_str()),
            "{text}"
        );
        assert!(text.contains("iaam claim"), "{text}");
        assert!(!database.exists());
        assert!(
            !home.join(".config").exists(),
            "a refused generate creates not even the key directory"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn broker_key_generate_creates_its_directory_and_never_overwrites() {
        let home = temp_home("key-generate");
        let key = home.join(".config/iaam/broker-key");

        // The documented order: the instance is claimed, then the key is
        // generated for it.
        execute(
            Command::Claim {
                label: "Main".to_owned(),
            },
            lookup_with(&[("HOME", home.to_str().unwrap())]),
        )
        .await
        .expect("claim creates the instance");
        execute(
            Command::Broker {
                command: BrokerCommand::Key {
                    command: BrokerKeyCommand::Generate,
                },
            },
            lookup_with(&[("HOME", home.to_str().unwrap())]),
        )
        .await
        .expect("generate creates the key");

        assert!(key.is_file(), "the key was written at the resolved place");
        assert_eq!(directory_mode(&home.join(".config/iaam")), 0o700);

        let error = execute(
            Command::Broker {
                command: BrokerCommand::Key {
                    command: BrokerKeyCommand::Generate,
                },
            },
            lookup_with(&[("HOME", home.to_str().unwrap())]),
        )
        .await
        .unwrap_err();
        assert!(
            format_error_chain(error.as_ref()).contains("already exists"),
            "an existing key is never overwritten"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn status_reports_places_existence_and_source() {
        let home = temp_home("status");
        let database = home.join(".local/share/iaam/iaam.db");
        let key = home.join(".config/iaam/broker-key");

        let report = status_report(&places_with(&[("HOME", home.to_str().unwrap())]));
        assert!(
            report.contains(database.display().to_string().as_str()),
            "{report}"
        );
        assert!(report.contains("(default; absent)"), "{report}");
        assert!(
            report.contains(key.display().to_string().as_str()),
            "{report}"
        );

        // After the creating command, status says the database is present —
        // and names a variable-set place by its variable.
        execute(
            Command::Claim {
                label: "Main".to_owned(),
            },
            lookup_with(&[("HOME", home.to_str().unwrap())]),
        )
        .await
        .unwrap();
        let report = status_report(&places_with(&[
            ("HOME", home.to_str().unwrap()),
            ("IAAM_DATABASE", database.to_str().unwrap()),
        ]));
        assert!(report.contains("(IAAM_DATABASE; present)"), "{report}");
        std::fs::remove_dir_all(&home).unwrap();
    }
}
