//! `iaam broker connect`: one command from a pasted token to a checked,
//! stored credential with the instance's broker requests on.
//!
//! The command is the owner's one act for connecting a broker: it finds the
//! instance by the default places, creates the encryption key when there is
//! none, reads the token hidden on a terminal, sends one read-only check
//! through the process's one gateway (the same tally and ceilings as every
//! broker request), and only after the broker answered stores the credential
//! and turns the stored egress switch on. A refusal by the broker stores
//! nothing and changes nothing: the switch is as the owner left it.
//!
//! The token is never an argument, never printed, never logged and never
//! stored in plaintext; the database grep in `docs/deployment.md` §6.3 holds
//! for this command as for `broker access add`.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use iaam_broker::credentials::{BrokerToken, Key, open, seal};
use iaam_broker::environment::Environment;
use iaam_broker::finam::FinamClient;
use iaam_broker::tinkoff::{TinkoffClient, parse_account_ids};
use iaam_http::gateway::ATTEMPTS;
use iaam_http::gateway::SystemClock;
use iaam_http::{GatewayError, Outbound, RequestAllowance, initialize_fresh_tally};
use iaam_store::SqliteStore;
use iaam_store::broker_access::SoleOwner;
use iaam_store::documents::BrokerCode;
use rustix::fd::BorrowedFd;
use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcsetattr};
use zeroize::Zeroizing;

use crate::config::Place;
use crate::instance::ensure_private_directory;
use crate::provision::{self, ProvisionError};

/// What `connect` refuses with. Every text says what is missing, who supplies
/// it and the command that supplies it; none names the token.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(
        "unknown broker {broker:?}: this build connects `finam` and `tinkoff`; \
         run `iaam broker connect <broker>` with one of them"
    )]
    UnsupportedBroker { broker: String },
    #[error("Finam connects in production only: it has no sandbox, so `--sandbox` does not apply")]
    FinamHasNoSandbox,
    #[error(
        "the token is empty: paste the token from the broker's own token page \
         and press Enter"
    )]
    TokenEmpty,
    #[error("instance has no owner: run `iaam claim` first")]
    NoOwner,
    #[error("multiple owners: choosing which one should receive access is impossible")]
    SeveralOwners,
    #[error(
        "an active credential for {broker} ({environment}) already exists: \
         `{command} --replace` checks the new token the same way and replaces it"
    )]
    ExistsReplace {
        broker: String,
        environment: String,
        command: String,
    },
    #[error("{message}")]
    Check { message: String },
    #[error("the access was not stored: {0}")]
    Provision(#[from] ProvisionError),
    #[error(transparent)]
    Store(#[from] iaam_store::StoreError),
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error("the fresh tally could not be written: {0}")]
    Tally(GatewayError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// How the checking call can fail, before the words are chosen.
#[derive(Debug)]
enum CheckFailure {
    /// The broker answered "no" to the credential itself.
    TokenRefused,
    /// Transient: a later call may succeed.
    Unreachable(String),
    /// The endpoint is paused or closed, and names when it reopens.
    Deferred(String),
    /// The instance's own allowance refused before the transport.
    Ceiling(String),
    /// Everything else, already fit to print.
    Other(String),
}

/// The broker names this command connects, with the words printed for them.
pub fn broker_display(
    broker: &str,
    environment: Environment,
) -> Option<(&'static str, &'static str)> {
    match (broker, environment) {
        ("finam", Environment::Prod) => Some(("finam", "Finam")),
        ("tinkoff", Environment::Prod) => Some(("tinkoff", "T-Invest")),
        ("tinkoff", Environment::Sandbox) => Some(("tinkoff", "T-Invest sandbox")),
        _ => None,
    }
}

/// Connect one broker.
///
/// The order is the safety order: every refusal above the check leaves the
/// instance as it was; the check itself sends nothing but one read-only
/// request; the stored writes happen only after the broker said yes. The
/// fresh zero tally of a first enabling is minted before the check (it is
/// what lets the check go out at once), and it is minted only while the
/// database does not yet vouch for a tally and the pair beside the database
/// is empty — the mint never overwrites a pair that governs, so no path
/// restores an allowance.
///
/// Everything one `connect` run needs, as the CLI arm resolved it.
pub struct ConnectRun<'a> {
    pub store: &'a mut SqliteStore,
    pub key: &'a Key,
    /// The instance's database place: the fresh tally of a first enabling is
    /// minted beside it.
    pub database: &'a Path,
    pub gateway: Arc<dyn Outbound>,
    pub broker: &'a str,
    pub environment: Environment,
    pub token: Zeroizing<String>,
    pub replace: bool,
}

/// # Errors
/// Every refusal in [`ConnectError`]; the check's own refusals come back as
/// [`ConnectError::Check`] with the plain-words reason.
pub async fn run(
    ConnectRun {
        store,
        key,
        database,
        gateway,
        broker,
        environment,
        token,
        replace,
    }: ConnectRun<'_>,
) -> Result<String, ConnectError> {
    if token.trim().is_empty() {
        // Refused before anything is sent: an empty paste must not become a
        // network round trip, and a missing input is an error, never a
        // default.
        return Err(ConnectError::TokenEmpty);
    }
    let Some((code, display)) = broker_display(broker, environment) else {
        if broker == "finam" {
            return Err(ConnectError::FinamHasNoSandbox);
        }
        return Err(ConnectError::UnsupportedBroker {
            broker: broker.to_owned(),
        });
    };
    let owner = match store.sole_token_owner()? {
        SoleOwner::Single(owner) => owner,
        SoleOwner::None => return Err(ConnectError::NoOwner),
        SoleOwner::Several => return Err(ConnectError::SeveralOwners),
    };
    let code = BrokerCode::parse(code).ok_or(ConnectError::UnsupportedBroker {
        broker: broker.to_owned(),
    })?;
    if !replace
        && store
            .find_broker_access(owner, &code, environment.code())?
            .is_some()
    {
        // With `--replace` this is not a refusal but the way in: the same
        // check runs below, and the existing rotate path replaces the
        // credential only after the broker accepted the new token.
        return Err(ConnectError::ExistsReplace {
            broker: broker.to_owned(),
            environment: environment.code().to_owned(),
            command: format!(
                "iaam broker connect {broker}{}",
                if environment == Environment::Sandbox {
                    " --sandbox"
                } else {
                    ""
                }
            ),
        });
    }
    if store.broker_egress()?.first_enabled_at.is_none() {
        initialize_fresh_tally(database, &SystemClock).map_err(ConnectError::Tally)?;
    }

    let accounts = check_access(gateway, broker, environment, &token)
        .await
        .map_err(|failure| ConnectError::Check {
            message: explain_check_failure(display, broker, environment, &failure),
        })?;

    // One transaction in the store: the credential and the enabling commit
    // together, so no failure between them can leave a credential stored
    // beside a switch that is still off.
    provision::store_credential_and_enable_egress(
        store,
        key,
        broker,
        environment,
        &token,
        replace,
    )?;
    Ok(format!(
        "{display} connected: the token sees {accounts} {}. \
         Broker requests are on; turn them off with `iaam broker off`.",
        if accounts == 1 { "account" } else { "accounts" },
    ))
}

/// Wrap the pasted token in the hiding wrapper.
///
/// `BrokerToken` has no constructor from plaintext outside `iaam-broker`, so
/// one round of `seal` and `open` with a key that exists for this call alone
/// builds it the way the tests do: the plaintext never grows a second
/// representation and the wrapper's `Debug` stays "hidden".
fn hidden(plain: &Zeroizing<String>) -> BrokerToken {
    let wrapper_key = Key::from_bytes([0_u8; 32]);
    open(&wrapper_key, &seal(&wrapper_key, plain.as_str()))
        .expect("a token sealed and opened in one call is the same token")
}

/// The one read-only call that checks a credential.
///
/// T-Invest is asked for its accounts (`UsersService/GetAccounts`); Finam's
/// session exchange runs and the accounts it names are read
/// (`POST /v1/sessions`, `POST /v1/sessions/details`). Every attempt goes
/// through the process's one gateway: the same budgets, tally and endpoint
/// ownership as a synchronisation.
async fn check_access(
    gateway: Arc<dyn Outbound>,
    broker: &str,
    environment: Environment,
    token: &Zeroizing<String>,
) -> Result<usize, CheckFailure> {
    let allowance = RequestAllowance::new(ATTEMPTS);
    // One client per check, one wrapper for it: `BrokerToken` is not `Clone`
    // and the plaintext is needed exactly once, here.
    match broker {
        "finam" => {
            let client = FinamClient::new(hidden(token), gateway);
            let ids = client
                .get_account_ids(&allowance)
                .await
                .map_err(failure_from_finam)?;
            Ok(ids.len())
        }
        "tinkoff" => {
            let client = TinkoffClient::new(environment, hidden(token), gateway);
            let body = client
                .get_accounts(&allowance)
                .await
                .map_err(failure_from_tinkoff)?;
            let ids =
                parse_account_ids(&body).map_err(|error| CheckFailure::Other(error.to_string()))?;
            Ok(ids.len())
        }
        _ => Err(CheckFailure::Other(format!(
            "unknown broker {broker:?}: this build connects `finam` and `tinkoff`"
        ))),
    }
}

fn failure_from_tinkoff(error: iaam_broker::tinkoff::TinkoffError) -> CheckFailure {
    use iaam_broker::tinkoff::TinkoffError as Error;
    match error {
        Error::InvalidToken => CheckFailure::TokenRefused,
        Error::Unreachable {
            attempts,
            retry_after,
            ..
        } => CheckFailure::Unreachable(format!(
            "it stayed unreachable through {attempts} attempts; trying again is worth it in about {}",
            human(retry_after)
        )),
        Error::Gateway(gateway) => failure_from_gateway(&gateway),
        Error::Transport(http) => {
            CheckFailure::Other(format!("the transport could not be built: {http}"))
        }
        Error::UnexpectedStatus { status, .. } => unexpected_status(status),
        Error::MalformedResponse => CheckFailure::Other(
            "the accounts answer was not the shape a connection check reads; \
             check that the token belongs to this environment"
                .to_owned(),
        ),
        Error::RequestCeiling { ceiling } => CheckFailure::Other(format!(
            "the connection check spent its ceiling of {ceiling} attempts"
        )),
        Error::RequestSerialization | Error::MethodUnavailable { .. } | Error::PartialResponse => {
            CheckFailure::Other(
                "the check could not be sent as built; this is a fault of this build".to_owned(),
            )
        }
    }
}

fn failure_from_finam(error: iaam_broker::finam::FinamError) -> CheckFailure {
    use iaam_broker::finam::FinamError as Error;
    match error {
        Error::InvalidToken => CheckFailure::TokenRefused,
        Error::Unavailable {
            attempts,
            retry_after,
            ..
        } => CheckFailure::Unreachable(format!(
            "it stayed unreachable through {attempts} attempts; trying again is worth it in about {}",
            human(retry_after)
        )),
        Error::EgressRefused { reason, .. } => CheckFailure::Ceiling(reason),
        Error::Gateway { reason } => CheckFailure::Other(reason),
        Error::UnexpectedStatus { status, .. } => unexpected_status(status),
        Error::MalformedResponse => CheckFailure::Other(
            "the session answer was not the shape a connection check reads; \
             check that the token belongs to Finam production"
                .to_owned(),
        ),
        Error::TransportNotBuilt { reason } => {
            CheckFailure::Other(format!("the transport could not be built: {reason}"))
        }
        Error::RequestCeiling { ceiling } => CheckFailure::Other(format!(
            "the connection check spent its ceiling of {ceiling} attempts"
        )),
        Error::InvalidAccountId | Error::PartialResponse => CheckFailure::Other(
            "the check could not be sent as built; this is a fault of this build".to_owned(),
        ),
    }
}

fn failure_from_gateway(error: &GatewayError) -> CheckFailure {
    match error {
        GatewayError::DailyCeiling {
            resets_at,
            retry_after,
            ..
        } => CheckFailure::Ceiling(format!(
            "this instance's broker allowance is at its rolling-day ceiling \
             (a missing or emptied tally is treated as a spent day); it resets at {resets_at}, \
             in about {}",
            human(*retry_after)
        )),
        GatewayError::BrokerHostPaused {
            reopens_at,
            retry_after,
            ..
        } => CheckFailure::Deferred(format!(
            "the endpoint is paused after a rate limit and reopens at {reopens_at}, in about {}",
            human(*retry_after)
        )),
        GatewayError::BrokerHostClosed {
            reopens_at,
            reason,
            retry_after,
            ..
        } => CheckFailure::Deferred(format!(
            "the endpoint is closed after {reason} and reopens at {reopens_at}, in about {}",
            human(*retry_after)
        )),
        GatewayError::BrokerEndpointOwned { endpoint, .. } => CheckFailure::Other(format!(
            "another iaam process owns the {endpoint} endpoint; stop that process before connecting"
        )),
        GatewayError::CircuitOpen { retry_after, .. } => CheckFailure::Deferred(format!(
            "the endpoint is refused for {} after repeated failures",
            human(*retry_after)
        )),
        GatewayError::Exhausted {
            attempts,
            retry_after,
            ..
        } => CheckFailure::Unreachable(format!(
            "it stayed unreachable through {attempts} attempts; trying again is worth it in about {}",
            human(*retry_after)
        )),
        other => CheckFailure::Other(other.to_string()),
    }
}

fn unexpected_status(status: u16) -> CheckFailure {
    if status == 401 || status == 403 {
        CheckFailure::TokenRefused
    } else {
        CheckFailure::Other(format!(
            "the broker answered status {status}; check that the token belongs to this environment"
        ))
    }
}

fn explain_check_failure(
    display: &str,
    broker: &str,
    environment: Environment,
    failure: &CheckFailure,
) -> String {
    let again = format!(
        "`iaam broker connect {broker}{}`",
        if environment == Environment::Sandbox {
            " --sandbox"
        } else {
            ""
        }
    );
    match failure {
        CheckFailure::TokenRefused => format!(
            "{display} refused the token. Nothing was stored and the switch is as it was: \
             check the token for this environment and run {again} again."
        ),
        CheckFailure::Unreachable(detail) => format!(
            "{display} could not be reached: {detail}. Nothing was stored; \
             run {again} again when it is reachable."
        ),
        CheckFailure::Deferred(detail) => format!(
            "{display} is not taking requests now: {detail}. Nothing was stored; \
             run {again} again after it reopens."
        ),
        CheckFailure::Ceiling(detail) => format!(
            "the instance's own broker allowance refused the check: {detail}. \
             Nothing was stored and nothing was sent."
        ),
        CheckFailure::Other(detail) => {
            format!("the connection check failed: {detail}. Nothing was stored.")
        }
    }
}

/// A wait in words: under a minute reads as "moments", longer as minutes and
/// hours. Durations are shown, never the token.
fn human(wait: std::time::Duration) -> String {
    let minutes = wait.as_secs() / 60;
    if minutes < 1 {
        "under a minute".to_owned()
    } else if minutes < 60 {
        format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
    } else {
        format!(
            "{} hour{} {} minutes",
            minutes / 60,
            if minutes / 60 == 1 { "" } else { "s" },
            minutes % 60
        )
    }
}

/// The encryption key at the resolved place, created when there is none.
///
/// The same two steps `iaam broker key generate` runs: the key's directory is
/// made private first, then the key is created at the place — and an existing
/// file is never overwritten; it is read, or refused with the same warning
/// `broker key generate` gives when it cannot be read.
///
/// # Errors
/// The key place holds a file that is unreadable or not a key.
pub fn key_for_connect(place: &Place) -> Result<Key, Box<dyn std::error::Error>> {
    if place.path.is_file() {
        return Ok(crate::read_broker_key(&place.path)?);
    }
    ensure_private_directory(&place.path, "the broker key file")?;
    // `Key::create_at` makes the file and hands back nothing: the key that
    // this command uses is the one read back from the place, exactly the one
    // `serve` will read from it later.
    Key::create_at(&place.path)?;
    Ok(crate::read_broker_key(&place.path)?)
}

/// The byte a Ctrl-C press becomes while the hidden read has `ISIG`
/// cleared: the signal character arrives as data, so it can be refused
/// here instead of killing the process.
const CANCEL: char = '\u{3}';

/// The refusal for a cancelled paste: nothing was stored, nothing was
/// sent, and the command that asks again is named.
fn cancel_refused() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "cancelled at Ctrl-C: nothing was stored and nothing was sent; \
         run `iaam broker connect <broker>` to paste the token again",
    )
}

/// A line carrying the cancel byte is a cancellation, never a token.
fn without_cancel(
    read: Result<Zeroizing<String>, std::io::Error>,
) -> Result<Zeroizing<String>, std::io::Error> {
    match read {
        Ok(line) if line.contains(CANCEL) => Err(cancel_refused()),
        other => other,
    }
}

/// Restores the saved terminal modes whenever the hidden read ends: a
/// return, a refusal, or an error from the read itself. The terminal must
/// not keep a hidden echo because a paste failed.
struct RestoreTermios {
    fd: BorrowedFd<'static>,
    saved: Termios,
}

impl Drop for RestoreTermios {
    fn drop(&mut self) {
        if let Err(error) = tcsetattr(self.fd, OptionalActions::Drain, &self.saved) {
            eprintln!(
                "warning: the terminal echo could not be restored: {error}; \
                 run `reset` in this terminal before typing anything secret"
            );
        }
    }
}

/// Read the token from standard input, hidden on a terminal.
///
/// On a terminal the echo is switched off for the read and restored
/// afterwards, whatever the read returned — the saved terminal modes are
/// restored by a guard on every return path, so a refusal or an error
/// cannot leave the terminal silent. `ISIG` is cleared with the echo: a
/// Ctrl-C during the read arrives as the byte [`CANCEL`] and is refused as
/// a cancellation that stored nothing, instead of killing the process with
/// the echo still off. From a pipe or a file — how the tests and the
/// documentation runs feed the token — the line is read plainly; a Ctrl-C
/// byte in it is the same cancellation, never part of a token. In both
/// modes the token is one line, returned in zeroizing memory.
///
/// # Errors
/// Standard input could not be read, or the terminal state could not be
/// changed (the read is not attempted with the echo left as it was: a
/// terminal whose state cannot be changed cannot promise a hidden read).
pub fn read_token_hidden(
    display: &str,
    environment: Environment,
) -> Result<Zeroizing<String>, std::io::Error> {
    eprintln!(
        "paste the {display} token for the {} environment and press Enter (it stays hidden):",
        environment.code()
    );
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return without_cancel(read_line(&stdin));
    }
    let fd: BorrowedFd<'_> = rustix::stdio::stdin();
    let original: Termios =
        tcgetattr(fd).map_err(|error| hidden_input_refused("read the terminal state", error))?;
    let mut concealed = original.clone();
    concealed.local_modes.remove(LocalModes::ECHO);
    // `ISIG` cleared with the echo: a Ctrl-C during the read arrives as
    // the byte [`CANCEL`] — refused below — instead of killing the
    // process with the echo still off.
    concealed.local_modes.remove(LocalModes::ISIG);
    let restore = RestoreTermios {
        fd,
        saved: original,
    };
    tcsetattr(fd, OptionalActions::Drain, &concealed)
        .map_err(|error| hidden_input_refused("hide the input", error))?;
    let read = without_cancel(read_line(&stdin));
    drop(restore);
    eprintln!();
    read
}

fn hidden_input_refused(action: &'static str, error: rustix::io::Errno) -> std::io::Error {
    std::io::Error::other(format!(
        "{action} failed: {error}; a hidden read needs a working terminal, \
         or pipe the token in without a terminal"
    ))
}

fn read_line(stdin: &std::io::Stdin) -> Result<Zeroizing<String>, std::io::Error> {
    let mut line = Zeroizing::new(String::new());
    stdin.read_line(&mut line)?;
    let trimmed = line.trim_end_matches(['\n', '\r']).to_owned();
    Ok(Zeroizing::new(trimmed))
}

/// What `status` learns about the instance's database at its place.
#[derive(Debug)]
pub(crate) enum DatabaseStanding {
    /// No database at the place: no instance, so no stored word anywhere.
    Absent,
    /// A database file is there, but the instance cannot be read from it.
    Unreachable { cause: String },
    /// The database opened, and this is its broker word.
    Read { broker_lines: String },
}

/// Examines the database's place, then opens it through the one
/// non-creating open every command shares.
///
/// `std::fs::metadata` decides absence before anything is opened, so a
/// permission error is never read as absence (`NotFound` and a non-file
/// are absence; every other look is unreachable with its cause), and the
/// open is `instance::open_database` — a database removed between the look
/// and the open cannot be re-created here, because `status` holds no
/// creating open at all.
pub(crate) fn database_standing(database: &Place) -> DatabaseStanding {
    match std::fs::metadata(&database.path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DatabaseStanding::Absent,
        Err(cause) => DatabaseStanding::Unreachable {
            cause: cause.to_string(),
        },
        Ok(metadata) if !metadata.is_file() => DatabaseStanding::Absent,
        Ok(_) => match crate::instance::open_database(database) {
            Ok(store) => match read_broker_lines(&store) {
                Ok(broker_lines) => DatabaseStanding::Read { broker_lines },
                Err(error) => DatabaseStanding::Unreachable {
                    cause: error.to_string(),
                },
            },
            Err(error) => DatabaseStanding::Unreachable {
                cause: error.to_string(),
            },
        },
    }
}

/// The broker lines of an opened database: whether the stored switch is on
/// and which brokers are connected.
fn read_broker_lines(store: &SqliteStore) -> Result<String, iaam_store::StoreError> {
    let setting = store.broker_egress()?;
    let connected = store
        .active_broker_environments()?
        .into_iter()
        .map(|(broker, environment)| format!("{broker} ({environment})"))
        .collect::<Vec<_>>();
    Ok(format!(
        "broker requests: {}\nconnected brokers: {}\n",
        if setting.enabled { "on" } else { "off" },
        if connected.is_empty() {
            "none".to_owned()
        } else {
            connected.join(", ")
        }
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use iaam_broker::credentials::Key;
    use iaam_broker::environment::Environment;
    use iaam_core::ids::OwnerId;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::sync::PoisonError;
    use std::time::Duration;

    use iaam_http::egress_directory_for;
    use iaam_http::gateway::Transport;
    use iaam_http::{BrokerEgress, Destination, Gateway, HttpError, HttpRequest, HttpResponse};
    use iaam_store::SqliteStore;
    use iaam_store::tokens::{TokenRecord, TokenScope};
    use zeroize::Zeroizing;

    use super::*;

    /// An invented token for the fixtures. Never a real credential.
    const TOKEN: &str = "connect-test-token-invented-0001";

    /// A scratch instance: a directory with a database in it, its egress
    /// directory already created, and a sole owner in the database.
    struct Instance {
        root: PathBuf,
        database: PathBuf,
        store: SqliteStore,
        _owner: OwnerId,
    }

    impl Instance {
        fn new(label: &str) -> Self {
            Self::with_owners(label, 1)
        }

        /// A scratch instance with `owners` distinct owners: none is the
        /// unclaimed instance, two are the corruption `connect` refuses.
        fn with_owners(label: &str, owners: usize) -> Self {
            let root = std::env::temp_dir().join(format!(
                "iaam-connect-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).expect("scratch root created");
            let database = root.join("iaam.db");
            let store = SqliteStore::open(&database).expect("the instance's store opens");
            let mut seeded: Vec<OwnerId> = Vec::new();
            for index in 0..owners {
                let owner = OwnerId::new_random();
                store
                    .insert_token(
                        &TokenRecord {
                            id: uuid::Uuid::new_v4(),
                            owner,
                            label: "console".to_owned(),
                            scope: TokenScope::Owner,
                            revoked: false,
                        },
                        &format!("hash-invented-{index}"),
                    )
                    .expect("an owner is seeded");
                seeded.push(owner);
            }
            let _owner = seeded.first().copied().unwrap_or_else(OwnerId::new_random);
            let place = egress_directory_for(&database).expect("the database resolves");
            std::fs::create_dir(&place).expect("the egress directory is created");
            // The empty pair the production gateway's `open_or_create` writes
            // when it finds the place missing.
            std::fs::write(place.join("outbound-tally"), "").expect("the empty tally is written");
            Self {
                root,
                database,
                store,
                _owner,
            }
        }

        fn key(&self) -> Key {
            Key::from_bytes([9; 32])
        }
    }

    impl Drop for Instance {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A broker endpoint that answers from a script: no socket and no live
    /// broker, while the gateway above it is the real one — budgets, tally
    /// and endpoint ownership all apply.
    struct ScriptedBroker {
        replies: std::sync::Mutex<VecDeque<HttpResponse>>,
    }

    impl Transport for ScriptedBroker {
        fn send<'a>(
            &'a self,
            _request: &'a HttpRequest,
        ) -> impl Future<Output = Result<HttpResponse, iaam_http::HttpError>> + Send + 'a {
            let next = self
                .replies
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or(HttpResponse {
                    status: 500,
                    body: b"unscripted request".to_vec(),
                    retry_after: None,
                });
            async move { Ok(next) }
        }
    }

    fn body(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
            retry_after: None,
        }
    }

    /// A gateway over the scripted broker, keeping the instance's real tally
    /// placement.
    struct Broker {
        gateway: Arc<dyn Outbound>,
    }

    fn broker_answering(replies: Vec<HttpResponse>, instance: &Instance) -> Broker {
        let place = egress_directory_for(&instance.database).expect("the egress place derives");
        let gateway = Arc::new(
            Gateway::new_in_directory(
                ScriptedBroker {
                    replies: std::sync::Mutex::new(replies.into_iter().collect()),
                },
                BrokerEgress::On,
                &place,
            )
            .expect("the test gateway builds"),
        );
        Broker { gateway }
    }

    async fn run_connect(
        instance: &mut Instance,
        key: &Key,
        broker: Broker,
        broker_name: &str,
        environment: Environment,
        replace: bool,
    ) -> Result<String, ConnectError> {
        run(ConnectRun {
            store: &mut instance.store,
            key,
            database: &instance.database,
            gateway: broker.gateway,
            broker: broker_name,
            environment,
            token: Zeroizing::new(TOKEN.to_owned()),
            replace,
        })
        .await
    }

    fn futures_block(
        future: impl std::future::Future<Output = Result<String, ConnectError>>,
    ) -> Result<String, ConnectError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the test runtime builds")
            .block_on(future)
    }

    fn accounts_reply(count: usize) -> String {
        let accounts: Vec<String> = (0..count)
            .map(|index| format!(r#"{{"id":"invited-account-{index}"}}"#))
            .collect();
        format!(r#"{{"accounts":[{}]}}"#, accounts.join(","))
    }

    #[tokio::test]
    async fn a_broker_that_says_yes_stores_the_credential_and_turns_the_switch_on() {
        let mut instance = Instance::new("yes");
        let key = instance.key();
        let broker = broker_answering(vec![body(200, &accounts_reply(2))], &instance);

        let message = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            false,
        )
        .await
        .expect("the connection succeeds");

        assert!(
            message.contains("T-Invest connected: the token sees 2 accounts."),
            "{message}"
        );
        assert!(
            message.contains("`iaam broker off`"),
            "the message names the way off: {message}"
        );
        let broker_code = BrokerCode::parse("tinkoff").expect("the code parses");
        assert!(
            instance
                .store
                .find_broker_access(instance._owner, &broker_code, "prod")
                .expect("the store reads")
                .is_some(),
            "the credential is stored"
        );
        let setting = instance.store.broker_egress().expect("the switch reads");
        assert!(
            setting.enabled,
            "the switch is on after the broker said yes"
        );
        assert!(
            setting.first_enabled_at.is_some(),
            "the first enabling is recorded"
        );
        let tally = std::fs::read_to_string(
            egress_directory_for(&instance.database)
                .unwrap()
                .join("outbound-tally"),
        )
        .expect("the tally reads");
        assert!(!tally.is_empty(), "the first enabling minted a fresh tally");
    }

    #[tokio::test]
    async fn a_refusing_broker_stores_nothing_and_leaves_the_switch_as_it_was() {
        let mut instance = Instance::new("refused");
        instance
            .store
            .set_broker_egress_enabled(false)
            .expect("the switch starts off");
        let key = instance.key();
        let broker = broker_answering(vec![body(401, r#"{"code":"unauthorized"}"#)], &instance);

        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            false,
        )
        .await
        .expect_err("a refused token is a refusal");

        let text = error.to_string();
        assert!(text.contains("refused the token"), "{text}");
        assert!(
            !text.contains(TOKEN),
            "the refusal never names the token: {text}"
        );
        let broker_code = BrokerCode::parse("tinkoff").expect("the code parses");
        assert!(
            instance
                .store
                .find_broker_access(instance._owner, &broker_code, "prod")
                .expect("the store reads")
                .is_none(),
            "nothing is stored"
        );
        assert!(
            !instance.store.broker_egress().unwrap().enabled,
            "the switch is as it was"
        );
    }

    #[tokio::test]
    async fn an_existing_credential_refuses_and_replace_runs_the_same_check() {
        let mut instance = Instance::new("replace");
        let key = instance.key();
        provision::add_broker_access(
            &mut instance.store,
            &key,
            "tinkoff",
            Environment::Prod,
            "the-old-invented-token",
        )
        .expect("the old credential is seeded");
        let broker = broker_answering(vec![body(200, &accounts_reply(1))], &instance);

        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            false,
        )
        .await
        .expect_err("an existing credential refuses");

        assert!(
            error.to_string().contains("--replace"),
            "the refusal names the way in: {error}"
        );

        let broker = broker_answering(vec![body(200, &accounts_reply(1))], &instance);
        let message = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            true,
        )
        .await
        .expect("the replacement succeeds after the same check");
        assert!(message.contains("sees 1 account"), "{message}");
        assert!(
            instance
                .store
                .broker_access_history(instance._owner)
                .expect("the history reads")
                .len()
                == 1,
            "the credential is replaced, not stacked beside the old one"
        );
    }

    #[tokio::test]
    async fn a_deleted_tally_after_the_first_enabling_is_the_conservative_state() {
        let mut instance = Instance::new("conservative");
        let key = instance.key();
        {
            let broker = broker_answering(vec![body(200, &accounts_reply(1))], &instance);
            run_connect(
                &mut instance,
                &key,
                broker,
                "tinkoff",
                Environment::Prod,
                false,
            )
            .await
            .expect("the first connection succeeds");
        }
        let place = egress_directory_for(&instance.database).unwrap();
        std::fs::remove_dir_all(&place).expect("the egress directory is deleted");
        // What the production gateway does when it finds the place missing:
        // the directory and the empty pair are created, and an empty pair is
        // the conservative state.
        std::fs::create_dir(&place).expect("the egress directory is recreated");
        std::fs::write(place.join("outbound-tally"), "").expect("the empty tally is written");

        let broker = broker_answering(vec![body(200, &accounts_reply(1))], &instance);
        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Sandbox,
            false,
        )
        .await
        .expect_err("a deleted tally restores no allowance");

        let text = error.to_string();
        assert!(
            text.contains("daily ceiling") || text.contains("allowance"),
            "the conservative day is named: {text}"
        );
    }

    #[test]
    fn a_cancel_byte_is_a_cancellation_never_a_token() {
        let cancelled = without_cancel(Ok(Zeroizing::new("tok\u{3}en".to_owned())))
            .expect_err("a Ctrl-C byte is a cancellation");
        assert_eq!(
            cancelled.kind(),
            std::io::ErrorKind::Interrupted,
            "{cancelled}"
        );
        let text = cancelled.to_string();
        assert!(text.contains("cancelled"), "{text}");
        assert!(text.contains("nothing was stored"), "{text}");

        let kept = without_cancel(Ok(Zeroizing::new("a plain paste".to_owned())))
            .expect("an ordinary line is kept");
        assert_eq!(kept.as_str(), "a plain paste");

        let failure = without_cancel(Err(std::io::Error::other("the pipe closed")))
            .expect_err("a read error passes through unchanged");
        assert_eq!(failure.to_string(), "the pipe closed");
    }

    #[test]
    fn an_empty_paste_is_refused_before_anything_is_sent() {
        let mut instance = Instance::new("empty-token");
        let key = instance.key();
        let no_broker = broker_answering(vec![], &instance);
        let error = futures_block(async {
            run(ConnectRun {
                store: &mut instance.store,
                key: &key,
                database: &instance.database,
                gateway: no_broker.gateway,
                broker: "finam",
                environment: Environment::Prod,
                token: Zeroizing::new("   ".to_owned()),
                replace: false,
            })
            .await
        })
        .expect_err("an empty paste is a missing input");
        assert!(error.to_string().contains("token is empty"), "{error}");
        assert!(
            instance
                .store
                .broker_access_history(instance._owner)
                .expect("the history reads")
                .is_empty(),
            "nothing is stored"
        );
    }

    #[test]
    fn finam_has_no_sandbox_and_unknown_brokers_are_named() {
        let mut instance = Instance::new("arguments");
        let key = instance.key();
        let no_broker = broker_answering(vec![], &instance);
        let error = futures_block(run_connect(
            &mut instance,
            &key,
            no_broker,
            "finam",
            Environment::Sandbox,
            false,
        ))
        .expect_err("Finam has no sandbox");
        assert!(error.to_string().contains("no sandbox"), "{error}");

        let mut instance = Instance::new("arguments2");
        let key = instance.key();
        let no_broker = broker_answering(vec![], &instance);
        let error = futures_block(run_connect(
            &mut instance,
            &key,
            no_broker,
            "alpaca",
            Environment::Prod,
            false,
        ))
        .expect_err("an unknown broker is named");
        assert!(error.to_string().contains("finam"), "{error}");
    }

    #[test]
    fn an_absent_or_non_file_database_stands_as_absent() {
        let instance = Instance::new("status-absent");
        let absent = crate::config::Place {
            path: instance.root.join("absent.db"),
            source: crate::config::PlaceSource::Default,
        };
        assert!(
            matches!(database_standing(&absent), DatabaseStanding::Absent),
            "no database, no stored word"
        );

        std::fs::create_dir_all(instance.root.join("directory.db")).unwrap();
        let directory = crate::config::Place {
            path: instance.root.join("directory.db"),
            source: crate::config::PlaceSource::Default,
        };
        assert!(
            matches!(database_standing(&directory), DatabaseStanding::Absent),
            "a directory in the database's place is absent, not unreachable"
        );
    }

    #[test]
    fn an_unreachable_database_names_its_cause_instead_of_absence() {
        use std::os::unix::fs::PermissionsExt;
        let instance = Instance::new("status-unreachable");
        let database = crate::config::Place {
            path: instance.database.clone(),
            source: crate::config::PlaceSource::Default,
        };
        // The directory is made unreadable: the file is there, but nothing
        // about it can be examined — exactly the case the old is-file check
        // read as absence.
        let parent = instance.database.parent().unwrap().to_owned();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
        let standing = database_standing(&database);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();

        match &standing {
            DatabaseStanding::Unreachable { cause } => {
                assert!(cause.contains("Permission denied"), "{cause}");
            }
            other => panic!("an unreachable database is not absent: {other:?}"),
        }

        // An examinable but unreadable file is unreachable too: the cause
        // is the open's refusal, still not absence.
        std::fs::set_permissions(&instance.database, std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let standing = database_standing(&database);
        std::fs::set_permissions(&instance.database, std::fs::Permissions::from_mode(0o600))
            .unwrap();
        match &standing {
            DatabaseStanding::Unreachable { cause } => {
                assert!(cause.contains("cannot open the database"), "{cause}");
            }
            other => panic!("an unreadable database is not absent: {other:?}"),
        }
    }

    #[test]
    fn a_read_database_reports_the_switch_and_the_connected_brokers() {
        let mut instance = Instance::new("status-read");
        let database = crate::config::Place {
            path: instance.database.clone(),
            source: crate::config::PlaceSource::Default,
        };
        let standing = database_standing(&database);
        let DatabaseStanding::Read { broker_lines } = standing else {
            panic!("a claimed database reads: {standing:?}");
        };
        assert!(
            broker_lines.contains("broker requests: off"),
            "{broker_lines}"
        );
        assert!(
            broker_lines.contains("connected brokers: none"),
            "{broker_lines}"
        );

        let key = instance.key();
        let broker = broker_answering(vec![body(200, &accounts_reply(1))], &instance);
        futures_block(run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            false,
        ))
        .expect("the connection succeeds");

        let standing = database_standing(&database);
        let DatabaseStanding::Read { broker_lines } = standing else {
            panic!("a connected database reads");
        };
        assert!(
            broker_lines.contains("broker requests: on"),
            "{broker_lines}"
        );
        assert!(
            broker_lines.contains("connected brokers: tinkoff (prod)"),
            "{broker_lines}"
        );
    }
    // --- the owners a connect acts on -------------------------------------

    #[tokio::test]
    async fn an_unclaimed_instance_refuses_connect_before_anything_is_sent() {
        let mut instance = Instance::with_owners("no-owner", 0);
        let key = instance.key();
        let broker = broker_answering(vec![], &instance);

        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            false,
        )
        .await
        .expect_err("an instance without an owner is refused");

        let text = error.to_string();
        assert!(text.contains("run `iaam claim` first"), "{text}");
        assert!(
            instance
                .store
                .broker_access_history(instance._owner)
                .expect("the history reads")
                .is_empty(),
            "nothing is stored"
        );
    }

    #[tokio::test]
    async fn a_database_with_two_owners_is_refused_as_corruption() {
        let mut instance = Instance::with_owners("two-owners", 2);
        let key = instance.key();
        let broker = broker_answering(vec![], &instance);

        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            false,
        )
        .await
        .expect_err("two owners are a refusal, never a choice");

        assert!(error.to_string().contains("multiple owners"), "{error}");
    }

    #[tokio::test]
    async fn an_existing_sandbox_credential_names_the_sandbox_replace_command() {
        let mut instance = Instance::new("sandbox-replace");
        let key = instance.key();
        provision::add_broker_access(
            &mut instance.store,
            &key,
            "tinkoff",
            Environment::Sandbox,
            "the-old-invented-token",
        )
        .expect("the sandbox credential is seeded");
        let broker = broker_answering(vec![], &instance);

        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Sandbox,
            false,
        )
        .await
        .expect_err("an existing sandbox credential refuses");

        let text = error.to_string();
        assert!(
            text.contains("`iaam broker connect tinkoff --sandbox --replace`"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_replace_over_nothing_stored_checks_the_token_then_refuses_to_store() {
        let mut instance = Instance::new("replace-empty");
        let key = instance.key();
        // The broker says yes: the check passes, and the replace still finds
        // no stored credential to replace.
        let broker = broker_answering(vec![body(200, &accounts_reply(1))], &instance);

        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "tinkoff",
            Environment::Prod,
            true,
        )
        .await
        .expect_err("a replace with nothing stored is a refusal");

        let text = error.to_string();
        assert!(text.contains("the access was not stored"), "{text}");
        assert!(
            instance
                .store
                .broker_access_history(instance._owner)
                .expect("the history reads")
                .is_empty(),
            "nothing was stored by the refused replace"
        );
        assert!(
            !instance.store.broker_egress().unwrap().enabled,
            "the switch stays as it was"
        );
    }

    // --- the Finam check on the same scripted gateway ----------------------

    #[tokio::test]
    async fn a_finam_connection_checks_the_session_then_the_accounts() {
        let mut instance = Instance::new("finam-yes");
        let key = instance.key();
        // Finam's order on the wire: the session exchange answers with the
        // session token, then the accounts reading names what it sees.
        let broker = broker_answering(
            vec![
                body(200, r#"{"token":"invited-session-token"}"#),
                body(
                    200,
                    r#"{"account_ids":["invited-account-0","invited-account-1"]}"#,
                ),
            ],
            &instance,
        );

        let message = run_connect(
            &mut instance,
            &key,
            broker,
            "finam",
            Environment::Prod,
            false,
        )
        .await
        .expect("the Finam connection succeeds");

        assert!(
            message.contains("Finam connected: the token sees 2 accounts."),
            "{message}"
        );
        let broker_code = BrokerCode::parse("finam").expect("the code parses");
        assert!(
            instance
                .store
                .find_broker_access(instance._owner, &broker_code, "prod")
                .expect("the store reads")
                .is_some(),
            "the credential is stored"
        );
    }

    #[tokio::test]
    async fn a_finam_token_the_broker_refuses_stores_nothing() {
        let mut instance = Instance::new("finam-refused");
        instance
            .store
            .set_broker_egress_enabled(false)
            .expect("the switch starts off");
        let key = instance.key();
        let broker = broker_answering(vec![body(401, r#"{"code":"unauthorized"}"#)], &instance);

        let error = run_connect(
            &mut instance,
            &key,
            broker,
            "finam",
            Environment::Prod,
            false,
        )
        .await
        .expect_err("a refused Finam token is a refusal");

        let text = error.to_string();
        assert!(text.contains("refused the token"), "{text}");
        assert!(
            !text.contains(TOKEN),
            "the refusal never names the token: {text}"
        );
        assert!(
            !instance.store.broker_egress().unwrap().enabled,
            "the switch is as it was"
        );
    }

    #[tokio::test]
    async fn an_unknown_broker_name_is_refused_by_the_check_itself() {
        let instance = Instance::new("check-unknown");
        let no_broker = broker_answering(vec![], &instance);

        let failure = check_access(
            no_broker.gateway,
            "kraken",
            Environment::Prod,
            &Zeroizing::new(TOKEN.to_owned()),
        )
        .await;

        match failure {
            Err(CheckFailure::Other(message)) => {
                assert!(message.contains("unknown broker"), "{message}");
                assert!(!message.contains(TOKEN), "{message}");
            }
            other => panic!("an unknown broker is an Other refusal: {other:?}"),
        }
    }

    // --- the plain words of every check failure ----------------------------

    #[test]
    fn every_tinkoff_check_failure_maps_to_its_plain_words() {
        use iaam_broker::tinkoff::TinkoffError as Error;

        let wait = failure_from_tinkoff(Error::Unreachable {
            status: None,
            attempts: 3,
            retry_after: Duration::from_secs(90),
        });
        assert!(
            matches!(&wait, CheckFailure::Unreachable(text)
                if text.contains("3 attempts") && text.contains("1 minute")),
            "{wait:?}"
        );

        let through_the_gateway = failure_from_tinkoff(Error::Gateway(GatewayError::Exhausted {
            destination: Destination::TinkoffProd,
            status: None,
            attempts: 2,
            retry_after: Duration::from_secs(30),
        }));
        assert!(
            matches!(&through_the_gateway, CheckFailure::Unreachable(text)
                if text.contains("2 attempts")),
            "{through_the_gateway:?}"
        );

        let transport = failure_from_tinkoff(Error::Transport(HttpError::Network));
        assert!(
            matches!(&transport, CheckFailure::Other(text)
                if text.contains("the transport could not be built")),
            "{transport:?}"
        );

        let answered = failure_from_tinkoff(Error::UnexpectedStatus {
            status: 500,
            body: "invented".to_owned(),
        });
        assert!(
            matches!(&answered, CheckFailure::Other(text) if text.contains("status 500")),
            "{answered:?}"
        );

        let malformed = failure_from_tinkoff(Error::MalformedResponse);
        assert!(
            matches!(&malformed, CheckFailure::Other(text)
                if text.contains("was not the shape") && text.contains("environment")),
            "{malformed:?}"
        );

        let ceiling = failure_from_tinkoff(Error::RequestCeiling { ceiling: 7 });
        assert!(
            matches!(&ceiling, CheckFailure::Other(text) if text.contains("ceiling of 7")),
            "{ceiling:?}"
        );

        let built_fault = failure_from_tinkoff(Error::RequestSerialization);
        assert!(
            matches!(&built_fault, CheckFailure::Other(text)
                if text.contains("fault of this build")),
            "{built_fault:?}"
        );

        assert!(matches!(
            failure_from_tinkoff(Error::InvalidToken),
            CheckFailure::TokenRefused
        ));
    }

    #[test]
    fn every_finam_check_failure_maps_to_its_plain_words() {
        use iaam_broker::finam::FinamError as Error;

        assert!(matches!(
            failure_from_finam(Error::InvalidToken),
            CheckFailure::TokenRefused
        ));

        let unavailable = failure_from_finam(Error::Unavailable {
            status: Some(503),
            attempts: 2,
            retry_after: Duration::from_secs(45 * 60),
        });
        assert!(
            matches!(&unavailable, CheckFailure::Unreachable(text)
                if text.contains("2 attempts") && text.contains("45 minutes")),
            "{unavailable:?}"
        );

        let egress = failure_from_finam(Error::EgressRefused {
            reason: "the instance's day is spent".to_owned(),
            retry_after: None,
        });
        assert!(
            matches!(&egress, CheckFailure::Ceiling(text)
                if text.contains("the instance's day is spent")),
            "{egress:?}"
        );

        let gateway = failure_from_finam(Error::Gateway {
            reason: "the gateway refused".to_owned(),
        });
        assert!(
            matches!(&gateway, CheckFailure::Other(text)
                if text.contains("the gateway refused")),
            "{gateway:?}"
        );

        let answered = failure_from_finam(Error::UnexpectedStatus {
            status: 502,
            body: "invented".to_owned(),
        });
        assert!(
            matches!(&answered, CheckFailure::Other(text) if text.contains("status 502")),
            "{answered:?}"
        );

        let malformed = failure_from_finam(Error::MalformedResponse);
        assert!(
            matches!(&malformed, CheckFailure::Other(text)
                if text.contains("session answer was not the shape")),
            "{malformed:?}"
        );

        let transport = failure_from_finam(Error::TransportNotBuilt {
            reason: "no trust anchor".to_owned(),
        });
        assert!(
            matches!(&transport, CheckFailure::Other(text)
                if text.contains("the transport could not be built: no trust anchor")),
            "{transport:?}"
        );

        let ceiling = failure_from_finam(Error::RequestCeiling { ceiling: 4 });
        assert!(
            matches!(&ceiling, CheckFailure::Other(text) if text.contains("ceiling of 4")),
            "{ceiling:?}"
        );

        for built_fault in [Error::InvalidAccountId, Error::PartialResponse] {
            let mapped = failure_from_finam(built_fault);
            assert!(
                matches!(&mapped, CheckFailure::Other(text)
                    if text.contains("fault of this build")),
                "{mapped:?}"
            );
        }
    }

    #[test]
    fn every_gateway_refusal_maps_to_its_plain_words() {
        let paused = failure_from_gateway(&GatewayError::BrokerHostPaused {
            destination: Destination::FinamApi,
            host: "api.finam.ru",
            reopens_at: "soon".to_owned(),
            retry_after: Duration::from_secs(120),
            attempts: 3,
        });
        assert!(
            matches!(&paused, CheckFailure::Deferred(text)
                if text.contains("paused") && text.contains("reopens at soon")),
            "{paused:?}"
        );

        let closed = failure_from_gateway(&GatewayError::BrokerHostClosed {
            destination: Destination::FinamApi,
            host: "api.finam.ru",
            reason: "repeated refusals",
            reopens_at: "later".to_owned(),
            retry_after: Duration::from_secs(60),
            status: None,
            attempts: 4,
        });
        assert!(
            matches!(&closed, CheckFailure::Deferred(text)
                if text.contains("closed after repeated refusals")),
            "{closed:?}"
        );

        let owned = failure_from_gateway(&GatewayError::BrokerEndpointOwned {
            destination: Destination::FinamApi,
            endpoint: "api.finam.ru",
        });
        assert!(
            matches!(&owned, CheckFailure::Other(text)
                if text.contains("another iaam process owns the api.finam.ru endpoint")),
            "{owned:?}"
        );

        let breaker = failure_from_gateway(&GatewayError::CircuitOpen {
            destination: Destination::FinamApi,
            attempts: 2,
            retry_after: Duration::from_secs(30),
        });
        assert!(
            matches!(&breaker, CheckFailure::Deferred(text)
                if text.contains("refused for under a minute after repeated failures")),
            "{breaker:?}"
        );

        let exhausted = failure_from_gateway(&GatewayError::Exhausted {
            destination: Destination::FinamApi,
            status: None,
            attempts: 5,
            retry_after: Duration::from_secs(90),
        });
        assert!(
            matches!(&exhausted, CheckFailure::Unreachable(text)
                if text.contains("5 attempts") && text.contains("1 minute")),
            "{exhausted:?}"
        );

        let ceiling = failure_from_gateway(&GatewayError::DailyCeiling {
            destination: Destination::FinamApi,
            ceiling: 500,
            resets_at: "tomorrow".to_owned(),
            retry_after: Duration::from_secs(3_600),
        });
        assert!(
            matches!(&ceiling, CheckFailure::Ceiling(text)
                if text.contains("rolling-day ceiling") && text.contains("resets at tomorrow")),
            "{ceiling:?}"
        );

        let other = failure_from_gateway(&GatewayError::SecondGateway);
        assert!(
            matches!(&other, CheckFailure::Other(text) if text.contains("one gateway")),
            "{other:?}"
        );
    }

    #[test]
    fn a_401_or_403_is_a_refused_token_and_any_other_status_is_named() {
        for status in [401, 403] {
            assert!(
                matches!(unexpected_status(status), CheckFailure::TokenRefused),
                "status {status} is a refused token"
            );
        }
        let answered = unexpected_status(418);
        assert!(
            matches!(&answered, CheckFailure::Other(text)
                if text.contains("status 418") && text.contains("environment")),
            "{answered:?}"
        );
    }

    #[test]
    fn every_check_failure_explains_what_happened_to_the_stored_state() {
        let refused = explain_check_failure(
            "T-Invest",
            "tinkoff",
            Environment::Prod,
            &CheckFailure::TokenRefused,
        );
        assert!(
            refused.contains("refused the token")
                && refused.contains("Nothing was stored")
                && refused.contains("`iaam broker connect tinkoff` again"),
            "{refused}"
        );

        let sandbox = explain_check_failure(
            "T-Invest sandbox",
            "tinkoff",
            Environment::Sandbox,
            &CheckFailure::TokenRefused,
        );
        assert!(
            sandbox.contains("`iaam broker connect tinkoff --sandbox` again"),
            "{sandbox}"
        );

        let unreachable_check = explain_check_failure(
            "Finam",
            "finam",
            Environment::Prod,
            &CheckFailure::Unreachable("it stayed away".to_owned()),
        );
        assert!(
            unreachable_check.contains("could not be reached: it stayed away")
                && unreachable_check.contains("Nothing was stored"),
            "{unreachable_check}"
        );

        let deferred = explain_check_failure(
            "Finam",
            "finam",
            Environment::Prod,
            &CheckFailure::Deferred("it reopened late".to_owned()),
        );
        assert!(
            deferred.contains("is not taking requests now: it reopened late")
                && deferred.contains("Nothing was stored"),
            "{deferred}"
        );

        let ceiling = explain_check_failure(
            "Finam",
            "finam",
            Environment::Prod,
            &CheckFailure::Ceiling("the day is spent".to_owned()),
        );
        assert!(
            ceiling.contains("the instance's own broker allowance refused the check")
                && ceiling.contains("nothing was sent"),
            "{ceiling}"
        );

        let other = explain_check_failure(
            "Finam",
            "finam",
            Environment::Prod,
            &CheckFailure::Other("boom".to_owned()),
        );
        assert!(
            other.contains("the connection check failed: boom. Nothing was stored."),
            "{other}"
        );
    }

    #[test]
    fn a_wait_in_words_reads_as_moments_minutes_and_hours() {
        assert_eq!(human(Duration::from_secs(30)), "under a minute");
        assert_eq!(human(Duration::from_secs(60)), "1 minute");
        assert_eq!(human(Duration::from_secs(150)), "2 minutes");
        assert_eq!(human(Duration::from_secs(5_400)), "1 hour 30 minutes");
    }

    #[test]
    fn the_connect_key_is_read_when_it_exists_and_created_when_it_does_not() {
        use std::os::unix::fs::PermissionsExt;
        let instance = Instance::new("key-read");
        let place = crate::config::Place {
            path: instance.root.join("keys/broker.key"),
            source: crate::config::PlaceSource::Default,
        };

        let created = key_for_connect(&place).expect("a missing key is created");
        assert!(place.path.is_file(), "the key was written at the place");
        assert_eq!(
            place.path.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the key file is private"
        );

        // The second run reads the file the first wrote: what the created
        // key sealed, the reread key opens — the key `serve` later reads is
        // the one this connect used.
        let reread = key_for_connect(&place).expect("an existing key is read");
        let opened = open(&reread, &seal(&created, "round-trip"))
            .expect("the reread key opens what the created key sealed");
        assert_eq!(opened.expose(), "round-trip");
    }

    #[test]
    fn an_unreadable_key_file_is_refused_not_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let instance = Instance::new("key-unreadable");
        let place = crate::config::Place {
            path: instance.root.join("broker.key"),
            source: crate::config::PlaceSource::Default,
        };
        key_for_connect(&place).expect("the key is created for the test");
        std::fs::set_permissions(&place.path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let error = key_for_connect(&place).expect_err("an unreadable key is refused");
        let text = error.to_string();
        assert!(text.contains("exists"), "{text}");

        std::fs::set_permissions(&place.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    // --- the pasted token read ---------------------------------------------

    #[test]
    fn a_piped_paste_reads_one_line_from_a_non_terminal_standard_input() {
        // The suite runs with a non-terminal standard input (nextest, CI);
        // a caller on a real terminal gets the hidden read, which no unit
        // test can drive.
        assert!(
            !std::io::stdin().is_terminal(),
            "this read needs a non-terminal stdin; \
             run the suite under nextest or redirect standard input"
        );
        let token = read_token_hidden("T-Invest", Environment::Prod).expect("the pipe reads");
        assert_eq!(token.as_str(), "", "an empty pipe is an empty paste");
    }
}
