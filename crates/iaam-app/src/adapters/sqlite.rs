//! Storage port over `iaam-store`.
//!
//! The async/blocking boundary is crossed here and only here (§3.2).
//! `rusqlite` blocks the thread; calling it directly from an `axum` handler
//! stalls the executor, so every operation is sent to
//! `spawn_blocking`.

use std::sync::{Arc, Mutex};

use crate::error::AppError;
use crate::ports::{
    AccountActivityView, AccountAliasView, AccountCreated, AccountDetailView,
    AccountScopeExclusionView, AccountTransferStatementView, AccountView, AliasUpsert, AliasView,
    BrokerAccessView, BrokerChannel, BrokerChannelFactory, BrokerEnvironment, BrokerVault,
    CategoryGroupView, CategoryRuleUpsert, CategoryRuleView, CategoryStore, CategoryView,
    ClassificationRuleStore, ClassificationRuleView, ContourView, ControlAssertionView,
    CustodyView, DocumentToKeep, ImportObservationView, ImportQuestionView, ImportSessionState,
    ImportSessionView, InstrumentDirectory, InstrumentUpsert, InstrumentView, IssuedToken,
    JournalQuery, NewImportQuestion, Principal, Recorded, Scope, SoleOwner, Store, TokenAdmin,
    TokenView,
};
use crate::tokens::{hash_token, secret_hex};
use async_trait::async_trait;
use iaam_broker::credentials::{BrokerScope, Key, SealedToken, open, seal};
use iaam_broker::environment::Environment;
use iaam_broker::operation_kind::OperationKindDictionary;
use iaam_broker::tinkoff::TinkoffClient;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::event::provenance::RawHash;
use iaam_core::ids::{
    AccountId, CategoryGroupId, CategoryId, CategoryRuleId, ClassificationRuleId, ImportId,
    ImportQuestionId, ImportSessionId, InstrumentId, OwnerId, SourceId,
};
use iaam_core::instrument::{AliasInterval, AliasNamespace};
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use iaam_ingest::dedup::IdentityScope;
use iaam_store::SqliteStore;
use iaam_store::broker_access::{NewBrokerAccess, SoleOwner as StoredSoleOwner};
use iaam_store::broker_operation_kinds::BrokerOperationKind;
use iaam_store::categories::NewCategoryRule;
use iaam_store::documents::{
    BrokerCode, DocumentStored, NewDocument as StoredNewDocument,
    ReportFormat as StoredReportFormat,
};
use iaam_store::events::{
    AccountActivityRecord, Appended, ControlAssertionRecord, JournalCursor as StoredJournalCursor,
    JournalQuery as StoredJournalQuery,
};
use iaam_store::import_session::{
    NewQuestion as StoredNewQuestion, SessionState as StoredSessionState, StoredObservation,
    StoredQuestion, StoredSession,
};
use iaam_store::reference::{
    AccountAliasRecord, AccountCreation, AccountDetailRecord, AccountIdentity, AccountRecord,
    AccountScopeExclusionRecord, AccountTransferStatementRecord, AliasRecord, ContourRecord,
    InstrumentRecord,
};
use iaam_store::tokens::{TokenRecord, TokenScope};
use time::Date;
use uuid::Uuid;
use zeroize::Zeroizing;

/// Connection behind a mutex: `rusqlite::Connection` is not `Sync`, and a
/// single-user database has only one writer. A pool will be added when there is
/// a second writer, not before.
pub struct SqliteAdapter {
    store: Arc<Mutex<SqliteStore>>,
    /// The encryption key for broker credentials. It lives outside the database and therefore
    /// is supplied externally, not by the database. `None` — no key is configured,
    /// so broker credentials are neither created nor read: creating
    /// them «without encryption for now» would mean storing someone else's token
    /// in plaintext and discovering this through a database leak (§14).
    broker_key: Option<Key>,
}

impl SqliteAdapter {
    #[must_use]
    pub fn new(store: SqliteStore) -> Self {
        Self::with_broker_key(store, None)
    }

    /// The same adapter with an encryption key for broker credentials.
    #[must_use]
    pub fn with_broker_key(store: SqliteStore, key: Option<Key>) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            broker_key: key,
        }
    }

    /// The key or an error.
    ///
    /// A separate error variant, not `Store`: a missing key is
    /// incomplete server configuration, not a storage failure, and
    /// retrying the request will not fix it.
    fn key(&self) -> Result<&Key, AppError> {
        self.broker_key.as_ref().ok_or(AppError::NotConfigured {
            what: "broker access encryption",
        })
    }

    /// Runs a blocking operation.
    ///
    /// Mutex poisoning is recovered from rather than causing a panic:
    /// a panic in one request must not bring down the entire service,
    /// and the state of `SqliteStore` — a connection that a panic
    /// in the previous call does not corrupt.
    async fn blocking<T, F>(&self, work: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut SqliteStore) -> Result<T, AppError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut guard = match store.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            work(&mut guard)
        })
        .await
        .map_err(|error| AppError::Store(format!("blocking task failed: {error}")))?
    }
}

/// Session state: from the port to the store, and back.
///
/// Exhaustive in both directions, for the reason `scope_from_store` is: a fourth
/// state must break the build rather than silently read as `Open`, which would
/// let a committed session accept more observations.
const fn session_state_to_store(state: ImportSessionState) -> StoredSessionState {
    match state {
        ImportSessionState::Open => StoredSessionState::Open,
        ImportSessionState::Committed => StoredSessionState::Committed,
        ImportSessionState::Abandoned => StoredSessionState::Abandoned,
    }
}

const fn session_state_from_store(state: StoredSessionState) -> ImportSessionState {
    match state {
        StoredSessionState::Open => ImportSessionState::Open,
        StoredSessionState::Committed => ImportSessionState::Committed,
        StoredSessionState::Abandoned => ImportSessionState::Abandoned,
    }
}

fn import_session_view(session: StoredSession) -> ImportSessionView {
    ImportSessionView {
        id: session.id,
        state: session_state_from_store(session.state),
        source: session.source,
        import: session.import,
        opened_at: session.opened_at,
        closed_at: session.closed_at,
    }
}

fn import_observation_view(observation: StoredObservation) -> ImportObservationView {
    ImportObservationView {
        row: observation.row,
        row_key: observation.row_key,
        concluded: observation.concluded,
        payload: observation.payload,
        answer: observation.answer,
    }
}

fn import_question_view(question: StoredQuestion) -> ImportQuestionView {
    ImportQuestionView {
        id: question.id,
        session: question.session,
        row: question.row,
        question: question.question,
        alternatives: question.alternatives,
        prompt: question.prompt,
        asked_at: question.asked_at,
        answered_at: question.answered_at,
        answer: question.answer,
        rule: question.rule,
    }
}

/// A session failure the caller can act on, kept apart from a store failure.
///
/// A closed session and a missing one are one `NotFound` here as they are in the
/// store: distinguishing them would tell a caller holding a stranger's
/// identifier that the session exists.
fn import_session_error(error: iaam_store::StoreError) -> AppError {
    match error {
        iaam_store::StoreError::NotFound { what, id } => AppError::NotFound { what, id },
        iaam_store::StoreError::InvalidValue { field, value } => AppError::Invalid {
            field: field.to_owned(),
            expected: "a value the import session accepts".to_owned(),
            actual: value,
        },
        other => store_error(other),
    }
}

fn store_error(error: iaam_store::StoreError) -> AppError {
    AppError::Store(error.to_string())
}

/// The three resolution cases must remain distinguishable on this side
/// of the port too: merged into one `NotFound`, they no longer answer the question
/// «is this a new security or an incorrect date» (E3.1, §5.1 of the task specification).
fn resolve_error(error: iaam_store::ResolveError) -> AppError {
    match error {
        iaam_store::ResolveError::Unknown { namespace, value } => AppError::NotFound {
            what: "instrument by code",
            id: format!("{namespace}:{value}"),
        },
        iaam_store::ResolveError::NotOnDate {
            namespace,
            value,
            on,
            known_from,
            known_to,
        } => AppError::Invalid {
            field: "on".to_owned(),
            expected: format!("date within code validity interval {known_from}..{known_to}"),
            actual: format!("{namespace}:{value} on {on}"),
        },
        iaam_store::ResolveError::Ambiguous {
            namespace,
            value,
            on,
            candidates,
        } => AppError::DirectoryInvariant {
            correlation: Uuid::new_v4(),
            detail: format!(
                "code {namespace}:{value} on {on} resolves to {candidates} instruments: \
                 the instrument_aliases_do_not_overlap trigger has been breached"
            ),
        },
        iaam_store::ResolveError::Store(error) => store_error(error),
    }
}

fn instrument_view(record: iaam_store::reference::InstrumentRecord) -> InstrumentView {
    InstrumentView {
        id: record.id,
        kind: record.kind.map(|kind| kind.code().to_owned()),
        symbol: record.symbol,
        title: record.title,
        denomination_currency: record.currencies.denomination.code().to_owned(),
        settlement_currency: record.currencies.settlement.code().to_owned(),
        quote_currency: record.currencies.quote.code().to_owned(),
    }
}

fn alias_view(record: iaam_store::reference::AliasRecord) -> AliasView {
    AliasView {
        namespace: record.namespace.code().to_owned(),
        value: record.value,
        instrument: record.instrument,
        valid_from: record.interval.valid_from,
        valid_to: record.interval.valid_to,
    }
}

fn custody_view(record: iaam_store::reference::CustodyRecord) -> CustodyView {
    CustodyView {
        id: record.id,
        title: record.title,
        institution: record.institution,
    }
}

/// Token permissions: from the store to the port.
///
/// Conversion in both directions uses an exhaustive `match`, not a code string:
/// a new permission must break the build here, rather than silently turn
/// into a «reader» when an unknown code is parsed (§15.1).
const fn scope_from_store(scope: TokenScope) -> Scope {
    match scope {
        TokenScope::Owner => Scope::Owner,
        TokenScope::Agent => Scope::Agent,
        TokenScope::ReadOnly => Scope::ReadOnly,
    }
}

const fn scope_to_store(scope: Scope) -> TokenScope {
    match scope {
        Scope::Owner => TokenScope::Owner,
        Scope::Agent => TokenScope::Agent,
        Scope::ReadOnly => TokenScope::ReadOnly,
    }
}

#[async_trait]
impl Store for SqliteAdapter {
    async fn append_events(
        &self,
        events: Vec<Event>,
        identity_scope: IdentityScope,
    ) -> Result<Vec<Recorded>, AppError> {
        self.blocking(move |store| {
            let mut recorded = Vec::with_capacity(events.len());
            for event in &events {
                let outcome = store
                    .append_event_in_order(event, identity_scope)
                    .map_err(store_error)?;
                recorded.push(match outcome {
                    Appended::Inserted { id } => Recorded::Inserted { id },
                    Appended::Duplicate { existing } => Recorded::Duplicate { existing },
                });
            }
            Ok(recorded)
        })
        .await
    }

    async fn load_events_through(
        &self,
        owner: OwnerId,
        through: Date,
    ) -> Result<Vec<Event>, AppError> {
        self.blocking(move |store| {
            store
                .load_events_through(owner, through)
                .map_err(store_error)
        })
        .await
    }

    async fn list_journal_events(
        &self,
        owner: OwnerId,
        query: JournalQuery,
    ) -> Result<Vec<Event>, AppError> {
        let query = StoredJournalQuery {
            event: query.event,
            idempotency_key: query.idempotency_key,
            account: query.account,
            source: query.source,
            from: query.from,
            to: query.to,
            after: query.after.map(|cursor| StoredJournalCursor {
                effective_date: cursor.effective_date,
                sequence: cursor.sequence,
            }),
            limit: query.limit,
        };
        self.blocking(move |store| {
            store
                .list_journal_events(owner, &query)
                .map_err(store_error)
        })
        .await
    }

    async fn load_contour(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
    ) -> Result<Option<ContourDefinition>, AppError> {
        self.blocking(move |store| {
            store
                .load_contour(owner, contour, version)
                .map_err(store_error)
        })
        .await
    }

    async fn latest_contour_version(
        &self,
        owner: OwnerId,
        contour: ContourId,
    ) -> Result<Option<ContourVersion>, AppError> {
        self.blocking(move |store| {
            store
                .latest_contour_version(owner, contour)
                .map_err(store_error)
        })
        .await
    }

    async fn insert_contour_version(
        &self,
        owner: OwnerId,
        definition: ContourDefinition,
        title: String,
        accounts: Vec<AccountId>,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .insert_contour_version(owner, &definition, &title, &accounts)
                .map_err(store_error)
        })
        .await
    }

    async fn upsert_account(&self, owner: OwnerId, account: AccountView) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .upsert_account(&AccountRecord {
                    id: account.id,
                    owner,
                    title: account.title,
                    institution: account.institution,
                })
                .map_err(store_error)
        })
        .await
    }

    async fn create_account(
        &self,
        owner: OwnerId,
        account: AccountDetailView,
    ) -> Result<AccountCreated, AppError> {
        self.blocking(move |store| {
            let record = account_detail_record(owner, account);
            match store.create_account(&record).map_err(store_error)? {
                AccountCreation::Created(stored) => {
                    Ok(AccountCreated::Created(account_detail_view(stored)))
                }
                AccountCreation::Existing(stored) => {
                    Ok(AccountCreated::Existing(account_detail_view(stored)))
                }
            }
        })
        .await
    }

    async fn list_account_details(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountDetailView>, AppError> {
        self.blocking(move |store| {
            let accounts = store.list_account_details(owner).map_err(store_error)?;
            Ok(accounts.into_iter().map(account_detail_view).collect())
        })
        .await
    }

    async fn replace_account_aliases(
        &self,
        owner: OwnerId,
        account: AccountId,
        aliases: Vec<AccountAliasView>,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            let aliases: Vec<AccountAliasRecord> = aliases
                .into_iter()
                .map(|alias| AccountAliasRecord {
                    value: alias.value,
                    interval: AliasInterval {
                        valid_from: alias.valid_from,
                        valid_to: alias.valid_to,
                    },
                })
                .collect();
            store
                .replace_account_aliases(owner, account, &aliases)
                .map_err(store_error)
        })
        .await
    }

    async fn list_contours(&self, owner: OwnerId) -> Result<Vec<ContourView>, AppError> {
        self.blocking(move |store| {
            let contours = store.list_contours(owner).map_err(store_error)?;
            let mut views = Vec::with_capacity(contours.len());
            for record in contours {
                let record: ContourRecord = record;
                // `load_contour` returns `None` for a version with no members
                // and for a version that does not exist; the identity came from
                // the listing, so here only the first reading is possible and an
                // empty composition is the honest answer.
                let accounts = store
                    .load_contour(owner, record.id, record.version)
                    .map_err(store_error)?
                    .map(|definition| definition.accounts().collect())
                    .unwrap_or_default();
                views.push(ContourView {
                    id: record.id,
                    version: record.version,
                    title: record.title,
                    accounts,
                });
            }
            Ok(views)
        })
        .await
    }

    async fn list_accounts(&self, owner: OwnerId) -> Result<Vec<AccountView>, AppError> {
        self.blocking(move |store| {
            let accounts = store.list_accounts(owner).map_err(store_error)?;
            Ok(accounts
                .into_iter()
                .map(|record| AccountView {
                    id: record.id,
                    title: record.title,
                    institution: record.institution,
                })
                .collect())
        })
        .await
    }

    async fn list_account_activity(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountActivityView>, AppError> {
        self.blocking(move |store| {
            let activity = store.list_account_activity(owner).map_err(store_error)?;
            Ok(activity
                .into_iter()
                .map(|record: AccountActivityRecord| AccountActivityView {
                    account: record.account,
                    has_business_fact: record.has_business_fact,
                    first_effective_date: record.first_effective_date,
                    last_effective_date: record.last_effective_date,
                })
                .collect())
        })
        .await
    }

    async fn list_control_assertions(
        &self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<Vec<ControlAssertionView>, AppError> {
        self.blocking(move |store| {
            let assertions = store
                .list_control_assertions(owner, account)
                .map_err(store_error)?;
            Ok(assertions
                .into_iter()
                .map(|record: ControlAssertionRecord| ControlAssertionView {
                    account: record.account,
                    period: record.period,
                    point: record.point,
                    dimension: record.dimension,
                })
                .collect())
        })
        .await
    }

    async fn list_account_scope_exclusions(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountScopeExclusionView>, AppError> {
        self.blocking(move |store| {
            let exclusions = store
                .list_account_scope_exclusions(owner)
                .map_err(store_error)?;
            Ok(exclusions
                .into_iter()
                .map(
                    |record: AccountScopeExclusionRecord| AccountScopeExclusionView {
                        account: record.account,
                        reason: record.reason,
                    },
                )
                .collect())
        })
        .await
    }

    async fn record_account_scope_exclusion(
        &self,
        owner: OwnerId,
        exclusion: AccountScopeExclusionView,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .record_account_scope_exclusion(
                    owner,
                    &AccountScopeExclusionRecord {
                        account: exclusion.account,
                        reason: exclusion.reason,
                    },
                )
                .map_err(store_error)
        })
        .await
    }

    async fn clear_account_scope_exclusion(
        &self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .clear_account_scope_exclusion(owner, account)
                .map_err(store_error)
        })
        .await
    }

    async fn list_account_transfer_statements(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountTransferStatementView>, AppError> {
        self.blocking(move |store| {
            let statements = store
                .list_account_transfer_statements(owner)
                .map_err(store_error)?;
            Ok(statements
                .into_iter()
                .map(
                    |record: AccountTransferStatementRecord| AccountTransferStatementView {
                        account: record.account,
                        partners: record.partners,
                    },
                )
                .collect())
        })
        .await
    }

    async fn record_account_transfer_statement(
        &self,
        owner: OwnerId,
        statement: AccountTransferStatementView,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .record_account_transfer_statement(owner, statement.account, &statement.partners)
                .map_err(store_error)
        })
        .await
    }

    async fn clear_account_transfer_statement(
        &self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .clear_account_transfer_statement(owner, account)
                .map_err(store_error)
        })
        .await
    }

    async fn save_snapshot(&self, owner: OwnerId, snapshot: Snapshot) -> Result<(), AppError> {
        self.blocking(move |store| store.save_snapshot(owner, &snapshot).map_err(store_error))
            .await
    }

    async fn load_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<Option<Snapshot>, AppError> {
        self.blocking(move |store| {
            store
                .load_snapshot(owner, contour, version, lot_rule)
                .map_err(store_error)
        })
        .await
    }

    async fn keep_document(&self, document: DocumentToKeep) -> Result<SourceId, AppError> {
        let broker = BrokerCode::parse(&document.broker).ok_or_else(|| AppError::Invalid {
            field: "broker".to_owned(),
            expected: "the code the parser registry identifies the broker by".to_owned(),
            actual: document.broker.clone(),
        })?;
        let format =
            StoredReportFormat::parse(&document.format).ok_or_else(|| AppError::Invalid {
                field: "format".to_owned(),
                expected: "the code the parser registry identifies the format by".to_owned(),
                actual: document.format.clone(),
            })?;
        let stored = StoredNewDocument {
            id: document.id,
            owner: document.owner,
            broker,
            format,
            parser_version: document.parser_version,
            document_hash: document.document_hash,
            body: document.body,
        };
        self.blocking(move |store| {
            store
                .insert_document(&stored)
                .map(|outcome| match outcome {
                    DocumentStored::Inserted { id }
                    | DocumentStored::AlreadyPresent { existing: id } => id,
                })
                .map_err(store_error)
        })
        .await
    }

    async fn load_document_body(
        &self,
        owner: OwnerId,
        document_hash: RawHash,
    ) -> Result<Option<Vec<u8>>, AppError> {
        self.blocking(move |store| {
            store
                .load_document_by_hash(owner, &document_hash)
                .map(|found| found.map(|document| document.body))
                .map_err(store_error)
        })
        .await
    }

    async fn find_principal(&self, token_hash: String) -> Result<Option<Principal>, AppError> {
        self.blocking(move |store| {
            let found = store.find_token(&token_hash).map_err(store_error)?;
            Ok(found.map(|record| Principal {
                token_id: record.id,
                owner: record.owner,
                scope: scope_from_store(record.scope),
            }))
        })
        .await
    }

    async fn record_token_use(
        &self,
        token_hash: String,
        route: String,
        outcome: String,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .record_token_use(&token_hash, &route, &outcome)
                .map_err(store_error)
        })
        .await
    }

    async fn open_import_session(
        &self,
        owner: OwnerId,
        source: Option<SourceId>,
        import: Option<ImportId>,
    ) -> Result<ImportSessionView, AppError> {
        self.blocking(move |store| {
            store
                .open_import_session(owner, source, import)
                .map(import_session_view)
                .map_err(store_error)
        })
        .await
    }

    async fn load_import_session(
        &self,
        owner: OwnerId,
        session: ImportSessionId,
    ) -> Result<Option<ImportSessionView>, AppError> {
        self.blocking(move |store| {
            store
                .load_import_session(owner, session)
                .map(|found| found.map(import_session_view))
                .map_err(store_error)
        })
        .await
    }

    async fn list_import_sessions(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<ImportSessionView>, AppError> {
        self.blocking(move |store| {
            store
                .list_import_sessions(owner)
                .map(|sessions| sessions.into_iter().map(import_session_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn add_import_observation(
        &self,
        owner: OwnerId,
        session: ImportSessionId,
        row_key: Option<String>,
        concluded: bool,
        payload: String,
    ) -> Result<ImportObservationView, AppError> {
        self.blocking(move |store| {
            store
                .add_import_observation(owner, session, row_key.as_deref(), concluded, &payload)
                .map(import_observation_view)
                .map_err(import_session_error)
        })
        .await
    }

    async fn list_import_observations(
        &self,
        owner: OwnerId,
        session: ImportSessionId,
    ) -> Result<Vec<ImportObservationView>, AppError> {
        self.blocking(move |store| {
            // The owner is checked before the rows are read: a session
            // identifier is not an access right (§14), and the rows themselves
            // carry no owner to filter on.
            store
                .load_import_session(owner, session)
                .map_err(store_error)?
                .ok_or(AppError::NotFound {
                    what: "an import session",
                    id: session.inner().to_string(),
                })?;
            store
                .list_import_observations(session)
                .map(|rows| rows.into_iter().map(import_observation_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn record_import_question(
        &self,
        owner: OwnerId,
        session: ImportSessionId,
        row: u32,
        asking: NewImportQuestion,
    ) -> Result<ImportQuestionView, AppError> {
        self.blocking(move |store| {
            let asking = StoredNewQuestion {
                question: asking.question,
                alternatives: asking.alternatives,
                prompt: asking.prompt,
            };
            store
                .record_import_question(owner, session, row, &asking)
                .map(import_question_view)
                .map_err(import_session_error)
        })
        .await
    }

    async fn list_import_questions(
        &self,
        owner: OwnerId,
        session: ImportSessionId,
    ) -> Result<Vec<ImportQuestionView>, AppError> {
        self.blocking(move |store| {
            store
                .load_import_session(owner, session)
                .map_err(store_error)?
                .ok_or(AppError::NotFound {
                    what: "an import session",
                    id: session.inner().to_string(),
                })?;
            store
                .list_import_questions(session)
                .map(|rows| rows.into_iter().map(import_question_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn answer_import_question(
        &self,
        owner: OwnerId,
        session: ImportSessionId,
        question: ImportQuestionId,
        answer: String,
        rule: Option<String>,
    ) -> Result<ImportQuestionView, AppError> {
        self.blocking(move |store| {
            store
                .answer_import_question(owner, session, question, &answer, rule.as_deref())
                .map(import_question_view)
                .map_err(import_session_error)
        })
        .await
    }

    async fn close_import_session(
        &self,
        owner: OwnerId,
        session: ImportSessionId,
        state: ImportSessionState,
    ) -> Result<ImportSessionView, AppError> {
        self.blocking(move |store| {
            store
                .close_import_session(owner, session, session_state_to_store(state))
                .map(import_session_view)
                .map_err(import_session_error)
        })
        .await
    }
}

#[async_trait]
impl InstrumentDirectory for SqliteAdapter {
    async fn record_instrument(&self, record: InstrumentUpsert) -> Result<InstrumentId, AppError> {
        let InstrumentUpsert {
            id,
            kind,
            symbol,
            title,
            currencies,
            lineage,
        } = record;
        self.blocking(move |store| {
            store
                .upsert_instrument(&InstrumentRecord {
                    id,
                    kind,
                    symbol,
                    title,
                    currencies,
                    lineage,
                })
                .map_err(store_error)?;
            Ok(id)
        })
        .await
    }

    async fn record_alias(&self, alias: AliasUpsert) -> Result<(), AppError> {
        let AliasUpsert {
            namespace,
            value,
            instrument,
            interval,
            source,
        } = alias;
        self.blocking(move |store| {
            store
                .record_alias(&AliasRecord {
                    namespace,
                    value,
                    instrument,
                    interval,
                    source,
                })
                .map_err(store_error)
        })
        .await
    }
    async fn resolve(
        &self,
        namespace: &str,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, AppError> {
        let Some(namespace) = AliasNamespace::from_code(namespace) else {
            return Err(AppError::Invalid {
                field: "namespace".to_owned(),
                expected: "isin, moex_secid, ticker, figi or broker_code".to_owned(),
                actual: namespace.to_owned(),
            });
        };
        let value = value.to_owned();
        self.blocking(move |store| {
            store
                .resolve_instrument(namespace, &value, on)
                .map_err(resolve_error)
        })
        .await
    }

    async fn instrument(&self, id: InstrumentId) -> Result<Option<InstrumentView>, AppError> {
        self.blocking(move |store| {
            store
                .instrument(id)
                .map(|found| found.map(instrument_view))
                .map_err(store_error)
        })
        .await
    }

    async fn list_instruments(&self) -> Result<Vec<InstrumentView>, AppError> {
        self.blocking(|store| {
            store
                .list_instruments()
                .map(|rows| rows.into_iter().map(instrument_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn list_aliases(&self) -> Result<Vec<AliasView>, AppError> {
        self.blocking(|store| {
            store
                .list_aliases()
                .map(|rows| rows.into_iter().map(alias_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyView>, AppError> {
        self.blocking(move |store| {
            store
                .list_custody_places(owner)
                .map(|rows| rows.into_iter().map(custody_view).collect())
                .map_err(store_error)
        })
        .await
    }
}

#[async_trait]
impl BrokerVault for SqliteAdapter {
    async fn add_access(
        &self,
        owner: OwnerId,
        broker: String,
        environment: BrokerEnvironment,
        token: Zeroizing<String>,
    ) -> Result<BrokerAccessView, AppError> {
        let key = self.key()?;
        let code = BrokerCode::parse(&broker).ok_or_else(|| AppError::Invalid {
            field: "broker".to_owned(),
            expected: "non-empty broker code".to_owned(),
            actual: broker.clone(),
        })?;
        // Leading or trailing whitespace is not part of the token: the broker will reject a header with an extra
        // space, and give a vague reason.
        let token = Zeroizing::new(token.trim().to_owned());
        if token.is_empty() {
            return Err(AppError::Invalid {
                field: "token".to_owned(),
                expected: "non-empty token".to_owned(),
                // The value is not named even here: the error text —
                // exactly what is certain to end up in the log (§14).
                actual: "empty string".to_owned(),
            });
        }

        // This step requires no network access: the contract lists the codes, but
        // does not say what they map to in our system — the dictionary must be populated
        // from our own knowledge (`dictionary_seed`).
        // The contract cross-check is separate and invoked explicitly.
        let Some((dictionary, entries)) = iaam_broker::operation_kind::seed_for(code.as_str())
        else {
            return Err(AppError::Invalid {
                field: "broker".to_owned(),
                expected: "a broker with a known operation-type dictionary".to_owned(),
                actual: code.as_str().to_owned(),
            });
        };
        let entries: Vec<BrokerOperationKind> = entries
            .iter()
            .map(|(source_kind, kind)| BrokerOperationKind {
                source_kind: (*source_kind).to_owned(),
                kind: (*kind).to_owned(),
            })
            .collect();

        // Encrypt before handing off to the blocking task: the plaintext token
        // does not cross a thread boundary and is not copied into the closure.
        let sealed = seal(key, &token);
        let access = NewBrokerAccess {
            id: Uuid::new_v4(),
            owner,
            broker: code,
            // The environment is supplied externally, and this is the only place where it is
            // specified: thereafter it is taken from the record. During setup,
            // the person knows which token they hold. The mapping
            // from the port's vocabulary to the broker's happens here: the adapter is
            // the only component that knows both.
            environment: broker_environment(environment).code().to_owned(),
            // The permission scope is set here rather than supplied externally:
            // trading permissions are never requested under any circumstances (§14).
            scope: BrokerScope::ReadOnly.code().to_owned(),
            nonce: sealed.nonce().to_vec(),
            ciphertext: sealed.ciphertext().to_vec(),
        };
        // Writing and reading back are done in a single blocking task. The
        // setup time is assigned by the store, and constructing the representation here
        // would mean showing the owner a fabricated time; nor can it be read in a second
        // call — the access could be revoked between the two calls.
        let owner_of_access = access.owner;
        let broker_of_access = access.broker.clone();
        let environment_of_access = access.environment.clone();
        self.blocking(move |store| {
            store
                .insert_broker_access_with_operation_kinds(&access, dictionary, &entries)
                .map_err(|error| match error {
                    iaam_store::StoreError::AlreadyExists { what } => AppError::Conflict {
                        what: format!("{what} is already set up: revoke the active one first"),
                    },
                    other => store_error(other),
                })?;

            let stored = store
                .find_broker_access(owner_of_access, &broker_of_access, &environment_of_access)
                .map_err(store_error)?
                .ok_or(AppError::Store(
                    "access was set up but not read back".to_owned(),
                ))?;
            Ok(BrokerAccessView {
                id: stored.id,
                broker: stored.broker.as_str().to_owned(),
                environment: stored.environment,
                scope: stored.scope,
                created_at: stored.created_at,
                revoked_at: stored.revoked_at,
            })
        })
        .await
    }

    async fn list_access(&self, owner: OwnerId) -> Result<Vec<BrokerAccessView>, AppError> {
        // The key is also required when listing: without it, the listed access
        // would promise something unusable, and the issue would have to be diagnosed
        // from an empty broker response rather than the configuration.
        self.key()?;
        self.blocking(move |store| {
            let history = store.broker_access_history(owner).map_err(store_error)?;
            Ok(history
                .into_iter()
                .map(|access| BrokerAccessView {
                    id: access.id,
                    broker: access.broker.as_str().to_owned(),
                    environment: access.environment,
                    scope: access.scope,
                    created_at: access.created_at,
                    revoked_at: access.revoked_at,
                })
                .collect())
        })
        .await
    }

    async fn revoke_access(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError> {
        self.key()?;
        self.blocking(move |store| {
            store.revoke_broker_access(owner, id).map_err(|error| {
                // Missing access is a request error, not a failure of the
                // store: retrying will not fix it, and a `500` would send
                // the owner looking for a fault where none exists. Someone else's access
                // deliberately returns the same response — otherwise it would reveal
                // to an outsider that such a record exists (§14).
                match error {
                    iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                        what: "broker access",
                        id: id.to_string(),
                    },
                    other => store_error(other),
                }
            })
        })
        .await
    }
}

struct ChannelAccess {
    id: Uuid,
    environment: String,
    scope: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

#[async_trait]
impl crate::ports::BrokerDictionary for SqliteAdapter {
    async fn operation_kinds(
        &self,
        broker: &BrokerCode,
    ) -> Result<std::collections::BTreeMap<String, String>, AppError> {
        let broker = broker.clone();
        self.blocking(move |store| store.broker_operation_kinds(&broker).map_err(store_error))
            .await
    }
}

#[async_trait]
impl BrokerChannelFactory for SqliteAdapter {
    async fn open(&self, owner: OwnerId, broker: &str) -> Result<Arc<dyn BrokerChannel>, AppError> {
        let code = BrokerCode::parse(broker).ok_or_else(|| AppError::Invalid {
            field: "broker".to_owned(),
            expected: "a supported broker code".to_owned(),
            actual: broker.to_owned(),
        })?;
        if code.as_str() != "tinkoff" {
            return Err(AppError::Invalid {
                field: "broker".to_owned(),
                expected: "tinkoff".to_owned(),
                actual: broker.to_owned(),
            });
        }

        let key = self.key()?.clone();
        let broker = broker.to_owned();
        let access = self
            .blocking(move |store| {
                let mut active = store
                    .broker_access_history(owner)
                    .map_err(store_error)?
                    .into_iter()
                    .filter(|access| {
                        access.broker.as_str() == broker && access.revoked_at.is_none()
                    });
                let Some(first) = active.next() else {
                    return Err(AppError::NotConfigured {
                        what: "broker access",
                    });
                };
                if active.next().is_some() {
                    return Err(AppError::Invalid {
                        field: "broker".to_owned(),
                        expected: "exactly one active access".to_owned(),
                        actual: broker,
                    });
                }
                Ok(ChannelAccess {
                    id: first.id,
                    environment: first.environment,
                    scope: first.scope,
                    nonce: first.nonce,
                    ciphertext: first.ciphertext,
                })
            })
            .await?;

        if BrokerScope::parse(&access.scope) != Some(BrokerScope::ReadOnly) {
            return Err(AppError::Invalid {
                field: "scope".to_owned(),
                expected: BrokerScope::ReadOnly.code().to_owned(),
                actual: access.scope,
            });
        }
        let environment =
            Environment::parse(&access.environment).ok_or_else(|| AppError::Invalid {
                field: "environment".to_owned(),
                expected: "prod or sandbox".to_owned(),
                actual: access.environment.clone(),
            })?;
        let token =
            open(&key, &SealedToken::of(access.nonce, access.ciphertext)).map_err(|_| {
                AppError::NotConfigured {
                    what: "broker access",
                }
            })?;
        let client = TinkoffClient::new(environment, token)
            .map_err(|error| AppError::Store(format!("failed to create broker client: {error}")))?;
        // The dictionary is read here, not during parsing: `iaam-broker` intentionally
        // knows nothing about storage (see its `lib.rs`), and the adapter links
        // them — using the same approach already used for SQLite.
        let code = code.clone();
        let rows = self
            .blocking(move |store| store.broker_operation_kinds(&code).map_err(store_error))
            .await?;
        let (dictionary, unreadable) = OperationKindDictionary::build(rows);
        // A dictionary row not understood by this build means that the
        // database is newer than the code. Silently discarding it would turn
        // a known broker code into an unknown one — that is, it would reject
        // the import without its message indicating the mismatch.
        if let Some(first) = unreadable.first() {
            return Err(AppError::Invalid {
                field: "broker_operation_kinds".to_owned(),
                expected: "an operation type known to this build".to_owned(),
                actual: format!("{} -> {}", first.source_kind, first.kind),
            });
        }
        Ok(Arc::new(crate::adapters::tinkoff::TinkoffChannel::new(
            client,
            SourceId(access.id),
            dictionary,
        )))
    }
}

fn classification_rule_view(rule: iaam_store::rules::StoredRule) -> ClassificationRuleView {
    ClassificationRuleView {
        id: rule.id.inner(),
        version: rule.version,
        matcher: rule.matcher,
        outcome: rule.outcome,
        created_at: rule.created_at,
        retired_at: rule.retired_at,
        replaces: rule.replaces.map(|id| id.inner()),
    }
}

#[async_trait]
impl ClassificationRuleStore for SqliteAdapter {
    async fn list_rules(&self, owner: OwnerId) -> Result<Vec<ClassificationRuleView>, AppError> {
        self.blocking(move |store| {
            store
                .rule_history(owner)
                .map(|rules| rules.into_iter().map(classification_rule_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn create_rule(
        &self,
        owner: OwnerId,
        matcher: String,
        outcome: String,
        replaces: Option<Uuid>,
    ) -> Result<ClassificationRuleView, AppError> {
        self.blocking(move |store| {
            let rule = match replaces {
                Some(previous) => {
                    store.amend_rule(owner, ClassificationRuleId(previous), &matcher, &outcome)
                }
                None => store.insert_rule(owner, &matcher, &outcome),
            }
            .map_err(|error| match error {
                iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                    what: "an active classification rule",
                    id: replaces.map_or_else(String::new, |id| id.to_string()),
                },
                iaam_store::StoreError::AlreadyExists { what } => AppError::Conflict {
                    what: what.to_owned(),
                },
                other => store_error(other),
            })?;
            Ok(classification_rule_view(rule))
        })
        .await
    }

    async fn retire_rule(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .retire_rule(owner, ClassificationRuleId(id))
                .map_err(|error| match error {
                    iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                        what: "an active classification rule",
                        id: id.to_string(),
                    },
                    other => store_error(other),
                })
        })
        .await
    }
}

fn category_store_error(error: iaam_store::StoreError) -> AppError {
    match error {
        iaam_store::StoreError::CategoryGroupRetired { id } => {
            AppError::CategoryGroupRetired { id }
        }
        iaam_store::StoreError::NotFound { what, id } => AppError::NotFound { what, id },
        iaam_store::StoreError::AlreadyExists { what } => AppError::Conflict {
            what: what.to_owned(),
        },
        other => store_error(other),
    }
}

fn category_group_view(
    id: Uuid,
    title: String,
    retired_at: Option<String>,
    is_income: bool,
) -> CategoryGroupView {
    CategoryGroupView {
        id: CategoryGroupId(id),
        title,
        retired_at,
        is_income,
    }
}

fn category_view(row: iaam_store::categories::CategoryRow) -> CategoryView {
    CategoryView {
        id: CategoryId(row.id),
        group: CategoryGroupId(row.group_id),
        title: row.title,
        retired_at: row.retired_at,
    }
}

fn category_rule_view(row: iaam_store::categories::CategoryRuleRow) -> CategoryRuleView {
    CategoryRuleView {
        id: row.id,
        version: row.version,
        matcher: row.matcher_json,
        category: row.category,
        valid_from: row.valid_from,
        valid_to: row.valid_to,
        created_at: row.created_at,
        retired_at: row.retired_at,
        replaces: row.replaces,
    }
}

#[async_trait]
impl CategoryStore for SqliteAdapter {
    async fn create_group(
        &self,
        owner: OwnerId,
        title: String,
        is_income: bool,
    ) -> Result<CategoryGroupView, AppError> {
        self.blocking(move |store| {
            let id = store
                .insert_category_group_of_kind(owner, &title, is_income)
                .map_err(category_store_error)?;
            Ok(category_group_view(id, title, None, is_income))
        })
        .await
    }
    async fn list_groups(&self, owner: OwnerId) -> Result<Vec<CategoryGroupView>, AppError> {
        self.blocking(move |store| {
            store
                .list_groups(owner)
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| {
                            category_group_view(row.id, row.title, row.retired_at, row.is_income)
                        })
                        .collect()
                })
                .map_err(category_store_error)
        })
        .await
    }

    async fn retire_group(&self, owner: OwnerId, id: CategoryGroupId) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .retire_category_group(owner, id.inner())
                .map_err(category_store_error)
        })
        .await
    }

    async fn list_categories(&self, owner: OwnerId) -> Result<Vec<CategoryView>, AppError> {
        self.blocking(move |store| {
            store
                .list_categories(owner)
                .map(|rows| rows.into_iter().map(category_view).collect())
                .map_err(category_store_error)
        })
        .await
    }

    async fn create_category(
        &self,
        owner: OwnerId,
        group: CategoryGroupId,
        title: String,
    ) -> Result<CategoryView, AppError> {
        self.blocking(move |store| {
            let id = store
                .insert_category(owner, group.inner(), &title)
                .map_err(category_store_error)?;
            Ok(CategoryView {
                id: CategoryId(id),
                group,
                title,
                retired_at: None,
            })
        })
        .await
    }

    async fn retire_category(&self, owner: OwnerId, id: CategoryId) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .retire_category(owner, id.inner())
                .map_err(category_store_error)
        })
        .await
    }

    async fn list_category_rules(&self, owner: OwnerId) -> Result<Vec<CategoryRuleView>, AppError> {
        self.blocking(move |store| {
            store
                .list_category_rules(owner)
                .map(|rows| rows.into_iter().map(category_rule_view).collect())
                .map_err(category_store_error)
        })
        .await
    }

    async fn create_category_rule(
        &self,
        owner: OwnerId,
        rule: CategoryRuleUpsert,
        replaces: Option<CategoryRuleId>,
    ) -> Result<CategoryRuleView, AppError> {
        self.blocking(move |store| {
            let store_rule = NewCategoryRule {
                matcher_json: rule.matcher,
                category: rule.category.inner(),
                valid_from: rule.valid_from,
                valid_to: rule.valid_to,
            };
            let row = match replaces {
                Some(previous) => store.amend_category_rule(owner, previous, store_rule),
                None => store.insert_category_rule(owner, store_rule, None),
            }
            .map_err(category_store_error)?;
            Ok(category_rule_view(row))
        })
        .await
    }

    async fn retire_category_rule(
        &self,
        owner: OwnerId,
        id: CategoryRuleId,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .retire_category_rule(owner, id)
                .map_err(category_store_error)
        })
        .await
    }
}

/// Convert the port environment to the broker environment.
///
/// A conversion, not a shared type: the transport invokes the port and knows nothing about `iaam-broker`
/// — the architecture guard checks this.
const fn broker_environment(environment: BrokerEnvironment) -> Environment {
    match environment {
        BrokerEnvironment::Prod => Environment::Prod,
        BrokerEnvironment::Sandbox => Environment::Sandbox,
    }
}

/// A credential the moment it is minted: the record the database keeps, the
/// hash stored beside it, and the secret handed out exactly once.
///
/// **This is the only place that builds a token record.** Issuance used to be
/// written twice — once in `issue_token` and once in the CLI's claim path,
/// which needed its decision and its insert inside one transaction and so
/// assembled a record of its own. The second copy was the one that minted the
/// **owner's** token, and a change here — a longer secret, a different hash,
/// another field worth recording — would have left it silently on the old
/// behaviour with nothing failing to say so.
struct MintedToken {
    record: TokenRecord,
    hash: String,
    /// The token in the clear. Never logged, never stored: only `issued`
    /// lets it out, and only once (§14).
    secret: String,
}

impl MintedToken {
    /// 32 bytes from the system source, not a «long enough» string: a token is
    /// the key to someone else's money, and its strength is set here once for
    /// the whole system.
    ///
    /// Minting happens before any database work: a failure of the randomness
    /// source must not look like a store failure, and a secret obtained by
    /// unknown means must never be issued at all.
    fn mint(owner: OwnerId, label: String, scope: Scope) -> Result<Self, AppError> {
        let secret = secret_hex(32)?;
        let hash = hash_token(&secret);
        Ok(Self {
            record: TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label,
                scope: scope_to_store(scope),
                revoked: false,
            },
            hash,
            secret,
        })
    }

    /// Writes the record and the hash. The secret is not among them, so a leak
    /// of the database file does not grant access to the API (§14).
    fn store(&self, store: &SqliteStore) -> Result<(), AppError> {
        store
            .insert_token(&self.record, &self.hash)
            .map_err(store_error)
    }

    /// The token in the clear, released once. Consumes the mint: there is no
    /// second call, and nowhere to read the secret from afterwards.
    fn issued(self) -> IssuedToken {
        IssuedToken {
            id: self.record.id,
            token: self.secret,
            label: self.record.label,
            scope: scope_from_store(self.record.scope),
        }
    }
}

/// Runs `work` inside one `BEGIN IMMEDIATE` write transaction.
///
/// `IMMEDIATE`, not the default deferred begin: a deferred transaction takes
/// its write lock at the first write, which is after the read that decided to
/// write, and that gap is the race. `rusqlite::Transaction` is not used because
/// it borrows the connection, and `work` needs the store itself.
fn in_immediate_transaction<T>(
    store: &mut SqliteStore,
    work: impl FnOnce(&mut SqliteStore) -> Result<T, AppError>,
) -> Result<T, AppError> {
    store
        .connection_mut()
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(sqlite_error)?;
    match work(store) {
        Ok(value) => match store.connection_mut().execute_batch("COMMIT") {
            Ok(()) => Ok(value),
            Err(error) => {
                let _ = store.connection_mut().execute_batch("ROLLBACK");
                Err(sqlite_error(error))
            }
        },
        Err(error) => {
            let _ = store.connection_mut().execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// The driver's own error, reported as a store failure. Generic over `Into`
/// so that the driver type is not named here: `iaam-app` depends on
/// `iaam-store`, not on `rusqlite`.
fn sqlite_error(error: impl Into<iaam_store::StoreError>) -> AppError {
    store_error(error.into())
}

#[async_trait]
impl TokenAdmin for SqliteAdapter {
    async fn sole_owner(&self) -> Result<SoleOwner, AppError> {
        self.blocking(move |store| {
            let found = store.sole_token_owner().map_err(store_error)?;
            Ok(match found {
                StoredSoleOwner::None => SoleOwner::None,
                StoredSoleOwner::Single(owner) => SoleOwner::Single(owner),
                StoredSoleOwner::Several => SoleOwner::Several,
            })
        })
        .await
    }

    /// Claiming: the instance is checked to be unclaimed and its owner token is
    /// created under one `BEGIN IMMEDIATE` write transaction, held for the whole
    /// blocking call.
    ///
    /// Two console processes starting at the same instant used to be able to
    /// both observe an empty token table and create owners with unrelated
    /// portfolios. Nothing in the schema forbids a second owner, so the
    /// transaction is the only thing that does (ADR-0003).
    ///
    /// The mint happens outside the transaction on purpose — see
    /// `MintedToken::mint`. A secret minted for a claim that is then refused is
    /// simply dropped: it was never written, printed, or logged.
    async fn claim_owner(&self, label: String) -> Result<IssuedToken, AppError> {
        let minted = MintedToken::mint(OwnerId::new_random(), label, Scope::Owner)?;
        self.blocking(move |store| {
            // The loser of the race must wait for the winner's transaction
            // rather than fail at once as «database is locked».
            store
                .connection_mut()
                .busy_timeout(std::time::Duration::from_secs(5))
                .map_err(sqlite_error)?;
            in_immediate_transaction(store, |store| {
                if !matches!(
                    store.sole_token_owner().map_err(store_error)?,
                    StoredSoleOwner::None
                ) {
                    // Not a store failure: a second owner token is issued by
                    // `issue_token`, and retrying the claim will never work.
                    return Err(AppError::Conflict {
                        what: "instance is already claimed".to_owned(),
                    });
                }
                minted.store(store)
            })?;
            Ok(minted.issued())
        })
        .await
    }

    /// Token issuance: 32 random bytes, the hash goes into the database, the token itself goes out.
    ///
    /// The token is returned in plaintext exactly once — only the hash remains
    /// in the database, so a leak of the database file does not grant access to
    /// the API (§14). The record, the secret and the hash all come from
    /// `MintedToken`, which is what `claim_owner` mints too.
    async fn issue_token(
        &self,
        owner: OwnerId,
        label: String,
        scope: Scope,
    ) -> Result<IssuedToken, AppError> {
        let minted = MintedToken::mint(owner, label, scope)?;
        self.blocking(move |store| {
            minted.store(store)?;
            Ok(minted.issued())
        })
        .await
    }

    async fn list_tokens(&self, owner: OwnerId) -> Result<Vec<TokenView>, AppError> {
        self.blocking(move |store| {
            let tokens = store.list_tokens(owner).map_err(store_error)?;
            Ok(tokens
                .into_iter()
                .map(|token| TokenView {
                    id: token.id,
                    label: token.label,
                    scope: scope_from_store(token.scope),
                    created_at: token.created_at,
                    revoked_at: token.revoked_at,
                })
                .collect())
        })
        .await
    }

    async fn revoke_token(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError> {
        self.blocking(move |store| {
            store.revoke_token(owner, id).map_err(|error| {
                // A missing token is a request error, not a storage
                // failure: retrying will not fix it. A token belonging to someone else deliberately produces
                // the same response — otherwise it would tell
                // an outsider that such a record exists (§14).
                match error {
                    iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                        what: "token",
                        id: id.to_string(),
                    },
                    other => store_error(other),
                }
            })
        })
        .await
    }
}

/// The port's account, as the store's row.
///
/// Both halves of the identity or neither: one half alone would be stored as no
/// identity at all, and the transport refuses that shape before it reaches here.
fn account_detail_record(owner: OwnerId, account: AccountDetailView) -> AccountDetailRecord {
    AccountDetailRecord {
        id: account.id,
        owner,
        title: account.title,
        institution: account.institution,
        identity: account.provider.zip(account.provider_account_id).map(
            |(provider, provider_account_id)| AccountIdentity {
                provider,
                provider_account_id,
            },
        ),
        cash_class: account.cash_class,
        aliases: account
            .aliases
            .into_iter()
            .map(|alias| AccountAliasRecord {
                value: alias.value,
                interval: AliasInterval {
                    valid_from: alias.valid_from,
                    valid_to: alias.valid_to,
                },
            })
            .collect(),
    }
}

/// The store's row, as the port's account.
fn account_detail_view(record: AccountDetailRecord) -> AccountDetailView {
    let (provider, provider_account_id) = match record.identity {
        Some(identity) => (Some(identity.provider), Some(identity.provider_account_id)),
        None => (None, None),
    };
    AccountDetailView {
        id: record.id,
        title: record.title,
        institution: record.institution,
        provider,
        provider_account_id,
        cash_class: record.cash_class,
        aliases: record
            .aliases
            .into_iter()
            .map(|alias| AccountAliasView {
                value: alias.value,
                valid_from: alias.interval.valid_from,
                valid_to: alias.interval.valid_to,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_error_preserves_unknown_date_and_ambiguous_distinctions() {
        let unknown = resolve_error(iaam_store::ResolveError::Unknown {
            namespace: "isin",
            value: "RU000A".to_owned(),
        });
        let not_on_date = resolve_error(iaam_store::ResolveError::NotOnDate {
            namespace: "isin",
            value: "RU000A".to_owned(),
            on: "2026-08-25".to_owned(),
            known_from: "2020-01-01".to_owned(),
            known_to: "2025-12-31".to_owned(),
        });
        let ambiguous = resolve_error(iaam_store::ResolveError::Ambiguous {
            namespace: "ticker",
            value: "ABC".to_owned(),
            on: "2026-08-25".to_owned(),
            candidates: 2,
        });

        assert_eq!(unknown.code(), "not_found");
        assert_eq!(not_on_date.code(), "invalid_request");
        assert_eq!(ambiguous.code(), "directory_invariant_violated");
        assert!(matches!(
            &unknown,
            AppError::NotFound {
                what: "instrument by code",
                id,
            } if id == "isin:RU000A"
        ));
        assert!(matches!(
            &not_on_date,
            AppError::Invalid {
                field,
                expected,
                actual,
            } if field == "on"
                && expected == "date within code validity interval 2020-01-01..2025-12-31"
                && actual == "isin:RU000A on 2026-08-25"
        ));
        assert!(matches!(
            &ambiguous,
            AppError::DirectoryInvariant { detail, .. }
                if detail
                    == "code ticker:ABC on 2026-08-25 resolves to 2 instruments: \
                       the instrument_aliases_do_not_overlap trigger has been breached"
        ));
        let message = ambiguous.to_string();
        assert!(message.contains("reference data invariant"));
        assert!(message.contains("ticker:ABC"));
        assert!(message.contains("2026-08-25"));
        assert!(message.contains("2"));
    }
}
