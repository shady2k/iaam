//! Object-safe ports. The only place where they exist (§3.2).

use async_trait::async_trait;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId};
use iaam_core::projection::Snapshot;
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_core::rules::LotRuleVersion;
use iaam_http::HttpRequest;
use iaam_ingest::SubmittedOperation;
use iaam_ingest::dedup::IdentityScope;
use iaam_store::documents::BrokerCode;
use serde_json::Value;
use std::sync::Arc;
use time::Date;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::AppError;

/// Result of writing an event. The type belongs to the port, not the store:
/// otherwise the transport would learn about SQLite through the return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    Inserted { id: iaam_core::ids::EventId },
    Duplicate { existing: iaam_core::ids::EventId },
}

/// Token permissions at the application level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Owner,
    Agent,
    ReadOnly,
}

impl Scope {
    #[must_use]
    pub const fn may_submit(self) -> bool {
        match self {
            Self::Owner | Self::Agent => true,
            Self::ReadOnly => false,
        }
    }

    #[must_use]
    pub const fn may_administer(self) -> bool {
        match self {
            Self::Owner => true,
            Self::Agent | Self::ReadOnly => false,
        }
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Agent => "agent",
            Self::ReadOnly => "read_only",
        }
    }
}

/// Identified token bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub token_id: Uuid,
    pub owner: OwnerId,
    pub scope: Scope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    pub id: AccountId,
    pub title: String,
    pub institution: Option<String>,
}
/// Instrument as seen by the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentView {
    pub id: InstrumentId,
    /// `None` — the kind is not set. The valuation of such an instrument is incomplete,
    /// and defaulting it to a share is forbidden (§4.9).
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
}

/// Active instrument alias.
///
/// The `source` field is intentionally absent here: the reference data is global and read
/// by everyone, while `SourceId` points to a specific owner's document.
/// Exposing it would reveal the existence of someone else's
/// upload (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasView {
    pub namespace: String,
    pub value: String,
    pub instrument: InstrumentId,
    pub valid_from: Date,
    pub valid_to: Option<Date>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyView {
    pub id: CustodyId,
    pub title: String,
    pub institution: Option<String>,
}

/// Instrument data from an authorised write source.
///
/// The caller assigns the identifier: synchronisation first resolves
/// an existing external code, but creates an identifier for a new security and
/// then associates it with aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentUpsert {
    pub id: InstrumentId,
    pub kind: Option<iaam_core::instrument::InstrumentKind>,
    pub symbol: String,
    pub title: String,
    pub currencies: iaam_core::instrument::CurrencyRoles,
    pub lineage: Option<iaam_core::instrument::Lineage>,
}

/// Instrument alias from an authorised write source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasUpsert {
    pub namespace: iaam_core::instrument::AliasNamespace,
    pub value: String,
    pub instrument: InstrumentId,
    pub interval: iaam_core::instrument::AliasInterval,
    pub source: iaam_core::ids::SourceId,
}

/// Instrument reference data (§4.5, §4.7).
#[async_trait]
pub trait InstrumentDirectory: Send + Sync {
    /// Instrument by external code as of a date.
    ///
    /// The date is required and does not default to «today»: an ISIN changes
    /// through a corporate action, so there is no «current» answer to the question
    /// «which instrument is behind this code» (§4.7).
    async fn resolve(
        &self,
        namespace: &str,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, AppError>;

    async fn instrument(&self, id: InstrumentId) -> Result<Option<InstrumentView>, AppError>;

    async fn list_instruments(&self) -> Result<Vec<InstrumentView>, AppError>;

    /// All aliases with their validity intervals.
    ///
    /// They are returned in full in a single query: otherwise parsing a document would access
    /// the database for every row.
    async fn list_aliases(&self) -> Result<Vec<AliasView>, AppError>;

    /// Create or update an instrument and return its identifier.
    ///
    /// Writing is needed by source synchronisation and the administrator; an agent
    /// token is not given access to this method through an HTTP route (§7, §14).
    async fn record_instrument(&self, record: InstrumentUpsert) -> Result<InstrumentId, AppError>;

    /// Record an instrument's external code with its validity interval.
    async fn record_alias(&self, alias: AliasUpsert) -> Result<(), AppError>;

    async fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyView>, AppError>;
}

/// Store for facts and reference data.
#[async_trait]
pub trait Store: Send + Sync {
    /// Write events, assigning their order within the day.
    ///
    /// The store assigns the order in the same transaction as the insertion:
    /// separate «get the next number» and «insert» operations create a race (§4.8).
    async fn append_events(
        &self,
        events: Vec<Event>,
        identity_scope: IdentityScope,
    ) -> Result<Vec<Recorded>, AppError>;
    async fn load_events_through(
        &self,
        owner: OwnerId,
        through: Date,
    ) -> Result<Vec<Event>, AppError>;

    /// The owner is included in every reference-data and scope query.
    /// A scope identifier is a UUID, but a UUID does not confer
    /// access rights: without the owner in the query, anyone who knows the identifier
    /// can read someone else's portfolio (§14).
    async fn load_contour(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
    ) -> Result<Option<ContourDefinition>, AppError>;
    async fn latest_contour_version(
        &self,
        owner: OwnerId,
        contour: ContourId,
    ) -> Result<Option<ContourVersion>, AppError>;
    async fn insert_contour_version(
        &self,
        owner: OwnerId,
        definition: ContourDefinition,
        title: String,
        accounts: Vec<AccountId>,
    ) -> Result<(), AppError>;

    async fn upsert_account(&self, owner: OwnerId, account: AccountView) -> Result<(), AppError>;
    async fn list_accounts(&self, owner: OwnerId) -> Result<Vec<AccountView>, AppError>;

    async fn save_snapshot(&self, owner: OwnerId, snapshot: Snapshot) -> Result<(), AppError>;
    async fn load_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<Option<Snapshot>, AppError>;

    async fn find_principal(&self, token_hash: String) -> Result<Option<Principal>, AppError>;
    async fn record_token_use(
        &self,
        token_hash: String,
        route: String,
        outcome: String,
    ) -> Result<(), AppError>;
}
/// Source response after applying the transport policy.
///
/// The transport returns the body without parsing it: the character encoding and format are known by
/// `iaam-market`, while the hash links observation rows to the original response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub raw_hash: String,
}

/// Outgoing HTTP.
///
/// The port is generic, not «market-specific»: `HttpRequest` itself specifies the request host
/// through its `Destination`, and the market was merely the first to use this port.
/// Naming it after its first user would force every
/// the next one either to lie in the failure message, or to introduce another identical
/// port.
///
/// The port allows scenarios to be tested against frozen responses without network access.
/// `HttpRequest` describes a request, rather than an action; only the
/// `iaam-app` adapter sends it.
#[async_trait]
pub trait OutboundHttp: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError>;
}
/// Explicit failure on manual invocation without a configured HTTP adapter.
pub struct UnavailableOutboundHttp;

#[async_trait]
impl OutboundHttp for UnavailableOutboundHttp {
    async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
        Err(AppError::NotConfigured {
            what: "outbound HTTP transport",
        })
    }
}

/// Dictionary of channel operation types.
///
/// A separate port, rather than a `Store` method: the dictionary is read and populated
/// by entirely different scenarios from those that use the event log, and putting them
/// in one trait would mean granting access to the log together with access
/// to the reference data.
#[async_trait]
pub trait BrokerDictionary: Send + Sync {
    /// The complete channel dictionary: source code -> type name.
    async fn operation_kinds(
        &self,
        broker: &BrokerCode,
    ) -> Result<std::collections::BTreeMap<String, String>, AppError>;
}

/// Failure when the dictionary is not wired in.
pub struct UnavailableBrokerDictionary;

#[async_trait]
impl BrokerDictionary for UnavailableBrokerDictionary {
    async fn operation_kinds(
        &self,
        _broker: &BrokerCode,
    ) -> Result<std::collections::BTreeMap<String, String>, AppError> {
        Err(AppError::NotConfigured {
            what: "channel operation type dictionary",
        })
    }
}

/// A clock. A port, rather than `OffsetDateTime::now_utc()` inside a scenario:
/// otherwise a report «for today» cannot be reproduced in a test.
pub trait Clock: Send + Sync {
    fn today(&self) -> Date;
}

/// Provisioned access as shown to its owner.
///
/// There is no token or ciphertext here, nor can there be: what the port
/// does not return, the transport cannot expose externally through either a response or a log (§14).
/// The creation and revocation times are storage strings: there is one clock for
/// the whole storage crate, and reconstructing them as a type at the boundary would mean
/// introducing a second clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerAccessView {
    pub id: Uuid,
    pub broker: String,
    /// Broker environment: production or sandbox. It is deliberately part of the response —
    /// the access list should make clear where the system connects.
    pub environment: String,
    pub scope: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// A stored rule in a form the transport can return.
///
/// The JSON matcher/outcome values remain opaque to the store and
/// are returned without reinterpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationRuleView {
    pub id: uuid::Uuid,
    pub version: u32,
    pub matcher: String,
    pub outcome: String,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<uuid::Uuid>,
}

/// Port for historical classification rules.
#[async_trait]
pub trait ClassificationRuleStore: Send + Sync {
    async fn list_rules(&self, owner: OwnerId) -> Result<Vec<ClassificationRuleView>, AppError>;
    async fn create_rule(
        &self,
        owner: OwnerId,
        matcher: String,
        outcome: String,
        replaces: Option<uuid::Uuid>,
    ) -> Result<ClassificationRuleView, AppError>;
    async fn retire_rule(&self, owner: OwnerId, id: uuid::Uuid) -> Result<(), AppError>;
}

/// Broker environment in the port's vocabulary.
///
/// A separate enum, rather than an `iaam-broker` type: the transport calls the port
/// and must not know about the adapter — as already done for scopes
/// (`Scope` in the port versus `BrokerScope` in the broker). The gateway address
/// is not included here: it is a property of the adapter that connects to this environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerEnvironment {
    Prod,
    Sandbox,
}

impl BrokerEnvironment {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Sandbox => "sandbox",
        }
    }
}

/// Broker access store.
///
/// A separate port, rather than a `Store` method: provisioning access requires an encryption key
/// that the fact store does not and must not have.
///
/// The token is accepted as a `Zeroizing<String>`, rather than a `String`: in plaintext
/// it exists only until encryption and is zeroed on drop. An ordinary
/// string would leave it in freed process memory.
#[async_trait]
pub trait BrokerVault: Send + Sync {
    /// Provision access. Returns the record identifier — the access is
    /// revoked using it. The token itself is not returned: what the caller has not
    /// received, they cannot expose externally.
    async fn add_access(
        &self,
        owner: OwnerId,
        broker: String,
        environment: BrokerEnvironment,
        token: Zeroizing<String>,
    ) -> Result<BrokerAccessView, AppError>;

    /// All access records belonging to the owner, including revoked ones: «when the system
    /// stopped contacting the broker» is a question that
    /// needs an answer.
    async fn list_access(&self, owner: OwnerId) -> Result<Vec<BrokerAccessView>, AppError>;

    /// Revoke access. Not deletion: a revoked token remains part of the history.
    async fn revoke_access(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError>;
}

/// Who to treat as the owner when none was named explicitly.
///
/// The owner identifier is never printed — when a token is issued,
/// only the token itself is sent out, — and the user has nowhere to obtain it from.
/// Therefore the system identifies the sole owner itself. The type belongs to
/// the port, not the store: otherwise the transport would learn about SQLite through
/// the return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoleOwner {
    /// No token has been issued yet: the instance is unassigned.
    None,
    Single(OwnerId),
    /// There are multiple owners. For a single-user system, these are
    /// signs of corruption, not a state: the system must not choose for the user.
    Several,
}

/// A freshly issued token.
///
/// The token itself is an **ordinary** `String` here, not `Zeroizing`: it is issued
/// to be placed in the response body, and the path to the socket still
/// passes through serialisation buffers that cannot be zeroed.
/// A zeroing wrapper would promise a guarantee that this path cannot provide;
/// the broker token is different, as it is never returned externally.
/// Shown **once**: only the hash remains in the database.
#[derive(Clone)]
pub struct IssuedToken {
    pub id: Uuid,
    pub token: String,
    pub label: String,
    pub scope: Scope,
}

/// Manual `Debug`: a derived implementation would expose the token in the very first log,
/// and the log is what outlives the process.
impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedToken")
            .field("id", &self.id)
            .field("token", &"<hidden>")
            .field("label", &self.label)
            .field("scope", &self.scope)
            .finish()
    }
}

/// An issued token in the form shown to its owner.
///
/// Neither the token nor its hash is present here or can be: the hash is all that
/// needs to be inserted into a lookup query for the system to recognise
/// the bearer as its own. What the port did not return, the transport cannot
/// expose externally in either a response or a log (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenView {
    pub id: Uuid,
    pub label: String,
    pub scope: Scope,
    pub created_at: String,
    /// Time of revocation. `None` — the token is active.
    pub revoked_at: Option<String>,
}

/// Token management.
///
/// A separate port, not methods on `Store`: `Store` reads and writes portfolio facts,
/// while this port grants and revokes rights to them. Combining them
/// would mean that anyone allowed to read the ledger would also gain
/// the ability to issue themselves a second token.
///
/// **Token issuance exists only here.** It used to live in the composition root,
/// but the route would need its own; two implementations of «random
/// bytes, hash into the database, token out» silently diverge and produce tokens
/// of different strengths, while the weak one looks exactly like the strong one.
#[async_trait]
pub trait TokenAdmin: Send + Sync {
    /// The owner, if there is only one in the system.
    ///
    /// Needed both for assigning the instance (an owner already exists — there is
    /// nothing to assign) and for issuing a token from the console (a token must not
    /// be issued to a second owner).
    async fn sole_owner(&self) -> Result<SoleOwner, AppError>;

    /// Issue a token. Returns it in the clear **once**: the database
    /// retains the hash, and there is nowhere to retrieve the token from a second time.
    async fn issue_token(
        &self,
        owner: OwnerId,
        label: String,
        scope: Scope,
    ) -> Result<IssuedToken, AppError>;

    /// All of the owner's tokens, including revoked ones: «when the token stopped
    /// granting access» is a question that needs an answer.
    async fn list_tokens(&self, owner: OwnerId) -> Result<Vec<TokenView>, AppError>;

    /// Revoke a token. Not deletion: a revoked token remains part of the history.
    /// Someone else's and a non-existent one deliberately produce the same denial (§14).
    async fn revoke_token(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError>;
}

/// Why the broker request failed.
///
/// The type belongs to the port, not the adapter: otherwise the use case would learn about
/// HTTP and the specific broker through the return value.
///
/// No variant contains a token: the error message is what will
/// definitely be written to the log (§14).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrokerError {
    #[error("access to broker {broker} has not been configured")]
    NoAccess { broker: String },
    #[error("access to broker {broker} has been configured with permissions other than read-only")]
    ScopeNotReadOnly { broker: String },
    #[error("broker {broker} rejected the request: {detail}")]
    Refused { broker: String, detail: String },
    #[error("broker {broker} is unavailable: {detail}")]
    Unreachable { broker: String, detail: String },
    #[error("response from broker {broker} could not be parsed: {detail}")]
    Unparsable { broker: String, detail: String },
}

/// A channel operation that cannot be accepted into the journal.
///
/// The original JSON is preserved without conversion to another domain type:
/// the caller must be able to explain the discrepancy using the fields
/// in the broker's response.
#[derive(Debug, Clone, PartialEq)]
pub struct Quarantined {
    pub raw: Value,
    pub reason: String,
}

/// Result of retrieving a page of broker-channel operations.
///
/// Rejected rows are not lost: they are separated from accepted operations, but
/// reach the caller together with the reason and original JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOperations {
    pub accepted: Vec<SubmittedOperation>,
    pub quarantined: Vec<Quarantined>,
}

/// Broker channel: a second way to obtain the same data.
///
/// It exists to ensure independence (§10.3): a match between the parsed
/// report and the API response is basis 3, and only this gives
/// `accepted_independent` for real data. Therefore, **the implementation
/// of this port does not share parsing code with report parsers**: a shared
/// normalisation function would distort both sides with the same error, and
/// the reconciliation would not detect it.
///
/// Only read access is requested from the broker. There is no method here
/// that sends anything to the broker, nor will there ever be one (§14).
#[async_trait]
pub trait BrokerChannel: Send + Sync {
    /// Account operations for an interval: accepted and sent to quarantine.
    async fn fetch_operations(
        &self,
        account: AccountId,
        from: Date,
        to: Date,
    ) -> Result<ParsedOperations, BrokerError>;

    /// Control values as at a date: balances and quantities.
    ///
    /// Returns the source's assertions, not a calculation: the values calculated
    /// from the journal are subsequently reconciled against them.
    async fn fetch_portfolio(
        &self,
        account: AccountId,
        at: Date,
    ) -> Result<Vec<ControlClaim>, BrokerError>;

    /// Exactly how the data was obtained. The parser version and absence
    /// of a document are what the channel's independence is derived from.
    /// Scope guaranteed for source operation identifiers from this channel.
    fn identity_scope(&self) -> IdentityScope;
    fn channel(&self) -> SourceChannel;
}

/// Channel factory that hides access storage and decryption from the use case.
///
/// The secret crosses the boundary only within the adapter implementation and is never
/// returned to the application or transport.
#[async_trait]
pub trait BrokerChannelFactory: Send + Sync {
    async fn open(&self, owner: OwnerId, broker: &str) -> Result<Arc<dyn BrokerChannel>, AppError>;
}

/// Explicit stub for the composition point when no adapter is configured.
pub struct UnavailableBrokerChannelFactory;

#[async_trait]
impl BrokerChannelFactory for UnavailableBrokerChannelFactory {
    async fn open(
        &self,
        _owner: OwnerId,
        _broker: &str,
    ) -> Result<Arc<dyn BrokerChannel>, AppError> {
        Err(AppError::NotConfigured {
            what: "broker channel",
        })
    }
}

/// Explicit rules-port stub for test builds without a rules store.
pub struct UnavailableClassificationRuleStore;

#[async_trait]
impl ClassificationRuleStore for UnavailableClassificationRuleStore {
    async fn list_rules(&self, _owner: OwnerId) -> Result<Vec<ClassificationRuleView>, AppError> {
        Err(AppError::NotConfigured {
            what: "classification rules",
        })
    }

    async fn create_rule(
        &self,
        _owner: OwnerId,
        _matcher: String,
        _outcome: String,
        _replaces: Option<uuid::Uuid>,
    ) -> Result<ClassificationRuleView, AppError> {
        Err(AppError::NotConfigured {
            what: "classification rules",
        })
    }

    async fn retire_rule(&self, _owner: OwnerId, _id: uuid::Uuid) -> Result<(), AppError> {
        Err(AppError::NotConfigured {
            what: "classification rules",
        })
    }
}

/// System clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn today(&self) -> Date {
        time::OffsetDateTime::now_utc().date()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_only_scope_may_neither_submit_nor_administer() {
        // The token scope is a safety barrier, not a hint.
        // Conflated values produce either a reader that writes to the journal,
        // or the owner, who cannot do anything.
        assert!(Scope::Owner.may_submit());
        assert!(Scope::Agent.may_submit());
        assert!(!Scope::ReadOnly.may_submit());

        assert!(Scope::Owner.may_administer());
        assert!(
            !Scope::Agent.may_administer(),
            "the agent submits operations, but does not manage tokens (§14)"
        );
        assert!(!Scope::ReadOnly.may_administer());
    }

    #[test]
    fn every_scope_has_a_distinct_machine_readable_code() {
        assert_eq!(Scope::Owner.code(), "owner");
        assert_eq!(Scope::Agent.code(), "agent");
        assert_eq!(Scope::ReadOnly.code(), "read_only");
    }
    /// The port must be object-safe: the composition root holds
    /// adapters behind `Arc<dyn ...>`, and adapter selection must not be
    /// reflected in types at compile time (§3.2).
    #[test]
    fn the_instrument_directory_port_is_object_safe() {
        fn accepts(_: &dyn InstrumentDirectory) {}
        let _: fn(&dyn InstrumentDirectory) = accepts;
    }
}
