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
    #[error("instance has no owner: run `iaam claim --label <label>` first")]
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

    if replace {
        provision::replace_broker_access(store, key, broker, environment, &token)?;
    } else {
        provision::add_broker_access(store, key, broker, environment, &token)?;
    }
    store.set_broker_egress_enabled(true)?;
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

/// Read the token from standard input, hidden on a terminal.
///
/// On a terminal the echo is switched off for the read and restored
/// afterwards, whatever the read returned; the pasted line is not shown and
/// does not land in the terminal's scrollback. From a pipe or a file — how
/// the tests and the documentation runs feed the token — the line is read
/// plainly. In both modes the token is one line, returned in zeroizing
/// memory.
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
        return read_line(&stdin);
    }
    let fd: BorrowedFd<'_> = rustix::stdio::stdin();
    let original: Termios =
        tcgetattr(fd).map_err(|error| hidden_input_refused("read the terminal state", error))?;
    let mut concealed = original.clone();
    concealed.local_modes.remove(LocalModes::ECHO);
    tcsetattr(fd, OptionalActions::Drain, &concealed)
        .map_err(|error| hidden_input_refused("hide the input", error))?;
    let read = read_line(&stdin);
    // Restored on every path: the terminal must not keep a hidden echo
    // because the paste was empty or the pipe closed.
    tcsetattr(fd, OptionalActions::Drain, &original)
        .map_err(|error| hidden_input_refused("restore the terminal echo", error))?;
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

/// The broker lines of `iaam status`: whether the stored switch is on and
/// which brokers are connected, read from the instance's database.
///
/// `Ok(None)` when the database does not exist yet: there is no instance, so
/// no stored word and no connected broker anywhere — `status` stays safe to
/// run before anything exists. A database that exists but cannot be read is
/// an error, never a silent "off".
///
/// # Errors
/// The database could not be opened or read.
pub fn broker_status_lines(database: &Place) -> Result<Option<String>, iaam_store::StoreError> {
    if !database.path.is_file() {
        return Ok(None);
    }
    let store = SqliteStore::open(&database.path)?;
    let setting = store.broker_egress()?;
    let connected = store
        .active_broker_environments()?
        .into_iter()
        .map(|(broker, environment)| format!("{broker} ({environment})"))
        .collect::<Vec<_>>();
    Ok(Some(format!(
        "broker requests: {}\nconnected brokers: {}\n",
        if setting.enabled { "on" } else { "off" },
        if connected.is_empty() {
            "none".to_owned()
        } else {
            connected.join(", ")
        }
    )))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use iaam_broker::credentials::Key;
    use iaam_broker::environment::Environment;
    use iaam_core::ids::OwnerId;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::sync::PoisonError;

    use iaam_http::egress_directory_for;
    use iaam_http::gateway::Transport;
    use iaam_http::{BrokerEgress, Gateway, HttpRequest, HttpResponse};
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
            let root = std::env::temp_dir().join(format!(
                "iaam-connect-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).expect("scratch root created");
            let database = root.join("iaam.db");
            let (store, _owner) = seeded_store(&database);
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

    fn seeded_store(database: &Path) -> (SqliteStore, OwnerId) {
        let store = SqliteStore::open(database).expect("the instance's store opens");
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
                "hash-invented",
            )
            .expect("the sole owner is seeded");
        (store, owner)
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
    fn status_lines_read_the_database_and_absent_database_reads_none() {
        let mut instance = Instance::new("status");
        let database = crate::config::Place {
            path: instance.database.clone(),
            source: crate::config::PlaceSource::Default,
        };
        assert!(
            broker_status_lines(&crate::config::Place {
                path: instance.root.join("absent.db"),
                source: crate::config::PlaceSource::Default,
            })
            .expect("absent reads")
            .is_none(),
            "no database, no stored word"
        );

        let lines = broker_status_lines(&database)
            .expect("the lines read")
            .expect("a database gives lines");
        assert!(lines.contains("broker requests: off"), "{lines}");
        assert!(lines.contains("connected brokers: none"), "{lines}");

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

        let lines = broker_status_lines(&database)
            .expect("the lines read")
            .expect("a database gives lines");
        assert!(lines.contains("broker requests: on"), "{lines}");
        assert!(
            lines.contains("connected brokers: tinkoff (prod)"),
            "{lines}"
        );
    }
}
