use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::adapters::tinkoff::TinkoffChannel;
use iaam_app::error::AppError;
use iaam_app::ports::{
    BrokerChannel, BrokerError, BrokerRequestContext, Clock, CustodyUpsert, ParsedOperations,
    PortfolioAsOf, PortfolioSnapshot, Principal, Scope,
};
use iaam_app::scenarios::ingest::append_checked;
use iaam_app::sync::{AssertionsWithheld, SYNC_DEADLINE};
use iaam_broker::credentials::{Key, open, seal};
use iaam_broker::environment::Environment;
use iaam_broker::operation_kind::{OperationKindDictionary, seed_for};
use iaam_broker::tinkoff::TinkoffClient;
use iaam_core::custody::CustodyOrigin;
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_http::gateway::{BUDGETS, Clock as GatewayClock, Sleeper, Transport};
use iaam_http::{Gateway, HttpError, HttpRequest, HttpResponse, RequestAllowance};
use iaam_ingest::dedup::DedupLevel;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind, PARSER_VERSION};
use iaam_ingest::{SubmittedOperation, Verdict};
use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;
use tokio::sync::Notify;

struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

struct FakeBroker {
    source: SourceChannel,
    identity_scope: IdentityScope,
    operations: Result<ParsedOperations, BrokerError>,
    portfolio: Result<PortfolioSnapshot, BrokerError>,
    /// The account numbers the access sees. Empty is empty: the access sees
    /// none, and a sync that asks is refused rather than left to guess.
    accounts: Vec<String>,
    /// The broker numbers the double was actually asked for, in order.
    requested_numbers: Mutex<Vec<String>>,
    /// How many times the access was asked what it sees.
    accounts_requests: AtomicUsize,
}

#[async_trait]
impl BrokerChannel for FakeBroker {
    async fn fetch_account_numbers(
        &self,
        _context: BrokerRequestContext<'_>,
    ) -> Result<Vec<String>, BrokerError> {
        self.accounts_requests.fetch_add(1, Ordering::SeqCst);
        Ok(self.accounts.clone())
    }

    async fn fetch_operations(
        &self,
        _account: AccountId,
        broker_account: &str,
        _from: Date,
        _to: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, BrokerError> {
        self.requested_numbers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(broker_account.to_owned());
        self.operations.clone()
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        broker_account: &str,
        _at: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        self.requested_numbers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(broker_account.to_owned());
        self.portfolio.clone()
    }

    fn channel(&self) -> SourceChannel {
        self.source.clone()
    }

    fn identity_scope(&self) -> IdentityScope {
        self.identity_scope
    }
}

fn broker_code() -> iaam_store::documents::BrokerCode {
    iaam_store::documents::BrokerCode::parse("tinkoff").expect("broker code")
}

/// The old one-account shape every existing test was written against, kept
/// as a harness helper: the binding the scenario now reads is pre-recorded
/// (seeding the account row it needs), so these tests go on exercising the
/// fetch path and nothing else. The binding behaviours themselves are tested
/// against [`iaam_app::sync::sync_broker`] directly, below.
async fn sync_broker(
    services: &AppServices,
    principal: &Principal,
    broker: &impl BrokerChannel,
    account: AccountId,
    from: Date,
    to: Date,
) -> Result<iaam_app::sync::SyncOutcome, AppError> {
    let code = broker_code();
    if services
        .store
        .broker_account_binding(principal.owner, account, &code)
        .await
        .unwrap_or_else(|error| panic!("binding read: {error}"))
        .is_none()
    {
        services
            .store
            .upsert_account(
                principal.owner,
                iaam_app::ports::AccountView {
                    id: account,
                    title: "Main".to_owned(),
                    institution: Some("Test Bank".to_owned()),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("seed account: {error}"));
        services
            .store
            .record_broker_account_binding(
                principal.owner,
                account,
                &code,
                // Per-account, because one broker number is bound to at most
                // one iaam account and a helper shared by tests syncing two
                // accounts must not bind the same number twice. The value is
                // invented either way: the doubles never read it.
                account.inner().to_string(),
            )
            .await
            .unwrap_or_else(|error| panic!("seed binding: {error}"));
    }
    let request = iaam_app::sync::BrokerSyncRequest {
        broker_code: code,
        account,
        from,
        to,
    };
    iaam_app::sync::sync_broker(services, principal, broker, request).await
}

fn principal(owner: OwnerId) -> Principal {
    Principal {
        token_id: uuid::Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    }
}

fn services() -> AppServices {
    services_at(date!(2026 - 03 - 31))
}

fn services_at(today: Date) -> AppServices {
    services_with(today, |_store| {})
}

/// Builds the store, lets `setup` seed reference rows on it, then wraps it
/// into `SqliteAdapter`.
///
/// The write path now checks that every account, custody place and
/// instrument an event or its detail row names exists (and, for accounts and
/// custody places, belongs to the event's owner) — real foreign keys in the
/// relational journal. Instruments are still seeded on the raw store here,
/// before the wrap; custody places no longer are — `seed_custody` goes
/// through `InstrumentDirectory::record_custody_place` on the returned
/// `AppServices` instead.
fn services_with(today: Date, setup: impl FnOnce(&SqliteStore)) -> AppServices {
    let store =
        SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}"));
    setup(&store);
    let adapter = Arc::new(SqliteAdapter::new(store));
    AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        Arc::new(FixedClock(today)),
    )
}

fn seed_account(store: &SqliteStore, owner: OwnerId, account: AccountId, title: &str) {
    store
        .upsert_account(&iaam_store::reference::AccountRecord {
            id: account,
            owner,
            title: title.to_owned(),
            institution: Some("Test Bank".to_owned()),
        })
        .unwrap_or_else(|error| panic!("seed account: {error}"));
}

async fn seed_custody(services: &AppServices, owner: OwnerId, custody: CustodyId, title: &str) {
    services
        .directory
        .record_custody_place(
            owner,
            CustodyUpsert {
                id: custody,
                title: title.to_owned(),
                institution: None,
                origin: CustodyOrigin::Declared,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("seed custody place: {error}"));
}

fn seed_instrument(store: &SqliteStore, instrument: InstrumentId, symbol: &str) {
    store
        .upsert_instrument(&iaam_store::reference::InstrumentRecord {
            id: instrument,
            kind: None,
            symbol: symbol.to_owned(),
            title: symbol.to_owned(),
            currencies: iaam_core::instrument::CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .unwrap_or_else(|error| panic!("seed instrument: {error}"));
}

fn trade(account: AccountId, instrument: InstrumentId, custody: CustodyId) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind: OperationKind::Buy {
            instrument,
            custody: Some(custody),
            quantity: Dec::one(),
            gross_minor: 10_000,
            fee_minor: None,
            basis_fee: None,
            accrued_interest_minor: None,
            currency: CurrencyCode::Rub,
        },
        dates: OperationDates {
            trade: Some(date!(2026 - 03 - 15)),
            settled: Some(date!(2026 - 03 - 17)),
            cash_posted: Some(date!(2026 - 03 - 17)),
            paid: None,
        },
        source_time: None,
        idempotency_key: None,
        source_operation_id: Some("TRADE-MARCH-1".to_owned()),
        source_position_id: None,
        source_category: None,
        owner_category: None,
        source_code: None,
        source_kind: None,
        description: None,
        counterparty: None,
    }
}

/// A trade as the API broker channel actually produces one: no custody
/// stated at all, since the channel names none — only its own opaque
/// position handle, which the caller sets on `source_position_id`.
fn trade_without_custody(account: AccountId, instrument: InstrumentId) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind: OperationKind::Buy {
            instrument,
            custody: None,
            quantity: Dec::one(),
            gross_minor: 10_000,
            fee_minor: None,
            basis_fee: None,
            accrued_interest_minor: None,
            currency: CurrencyCode::Rub,
        },
        dates: OperationDates {
            trade: Some(date!(2026 - 03 - 15)),
            settled: Some(date!(2026 - 03 - 17)),
            cash_posted: Some(date!(2026 - 03 - 17)),
            paid: None,
        },
        source_time: None,
        idempotency_key: None,
        source_operation_id: Some("TRADE-MARCH-1".to_owned()),
        source_position_id: None,
        source_category: None,
        owner_category: None,
        source_code: None,
        source_kind: None,
        description: None,
        counterparty: None,
    }
}

fn report_trade_event(
    owner: OwnerId,
    operation: &SubmittedOperation,
    report_source: SourceId,
) -> Event {
    let normalized = iaam_ingest::normalize(
        operation,
        &iaam_ingest::operation::NormalizationContext {
            owner,
            source: report_source,
            parser_version: ParserVersion(PARSER_VERSION.to_owned()),
        },
    )
    .unwrap_or_else(|error| panic!("report trade: {error:?}"));
    Event {
        provenance: Provenance::new(
            report_source,
            normalized.event.provenance.raw_hash().clone(),
            ParserVersion("finam-xlsx/1".to_owned()),
        )
        .with_source_operation_id("TRADE-MARCH-1"),
        ..normalized.event
    }
}

/// The opening half of a control section, for cash and for the holding.
///
/// Present wherever a closing figure is expected to be compared at all: without
/// an opening the source reaches back over, the closing figure is a sum from a
/// start nothing states and the outcome is `OpeningNotAsserted` rather than a
/// match (`iaam-d7hn`). Both are zero, because each account here is opened by
/// the March trade itself.
fn cash_opening_claim() -> ControlClaim {
    ControlClaim::CashBalance {
        currency: CurrencyCode::Rub,
        amount: PostedMinor::new(0),
        at: BalancePoint::Opening,
    }
}

fn position_opening_claim(instrument: InstrumentId) -> ControlClaim {
    ControlClaim::PositionQuantity {
        instrument,
        quantity: iaam_core::money::Quantity::zero(),
        at: BalancePoint::Opening,
    }
}

/// The same opening claims, as the report channel's own events.
fn report_opening_assertions(
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
    holding: Option<InstrumentId>,
) -> Vec<Event> {
    let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
        .unwrap_or_else(|| panic!("March period"));
    let mut claims = vec![cash_opening_claim()];
    claims.extend(holding.map(position_opening_claim));
    claims
        .into_iter()
        .enumerate()
        .map(|(index, claim)| Event {
            id: EventId::new_random(),
            owner,
            account,
            kind: EventKind::ControlAssertion { period, claim },
            dates: EventDates::for_cash(CashPostedDate(period.from)),
            order: EffectiveOrder::new(period.from, u32::try_from(index).unwrap()),
            legs: Vec::new(),
            provenance: Provenance::new(
                source,
                iaam_core::event::provenance::RawHash::parse(&"a".repeat(64))
                    .unwrap_or_else(|| panic!("report hash")),
                ParserVersion("finam-xlsx/1".to_owned()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: Some(format!("report-opening-march-{index}")),
        })
        .collect()
}

fn report_cash_assertion(owner: OwnerId, account: AccountId, source: SourceId) -> Event {
    let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
        .unwrap_or_else(|| panic!("March period"));
    Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::ControlAssertion {
            period,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-10_000),
                at: BalancePoint::Closing,
            },
        },
        dates: EventDates::for_cash(CashPostedDate(period.to)),
        order: EffectiveOrder::new(period.to, 2),
        legs: Vec::new(),
        provenance: Provenance::new(
            source,
            iaam_core::event::provenance::RawHash::parse(&"a".repeat(64))
                .unwrap_or_else(|| panic!("report hash")),
            ParserVersion("finam-xlsx/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: Some("report-cash-march".to_owned()),
    }
}
fn report_position_assertion(
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
    instrument: InstrumentId,
) -> Event {
    let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
        .unwrap_or_else(|| panic!("March period"));
    Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::ControlAssertion {
            period,
            claim: ControlClaim::PositionQuantity {
                instrument,
                quantity: iaam_core::money::Quantity(Dec::one()),
                at: BalancePoint::Closing,
            },
        },
        dates: EventDates::for_cash(CashPostedDate(period.to)),
        order: EffectiveOrder::new(period.to, 3),
        legs: Vec::new(),
        provenance: Provenance::new(
            source,
            iaam_core::event::provenance::RawHash::parse(&"b".repeat(64))
                .unwrap_or_else(|| panic!("report hash")),
            ParserVersion("finam-xlsx/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: Some("report-position-march".to_owned()),
    }
}

fn api(_account: AccountId, source: SourceId, operation: SubmittedOperation) -> FakeBroker {
    api_with_claims(
        source,
        operation,
        vec![ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(-10_000),
            at: BalancePoint::Closing,
        }],
    )
}

fn api_with_claims(
    source: SourceId,
    operation: SubmittedOperation,
    claims: Vec<ControlClaim>,
) -> FakeBroker {
    FakeBroker {
        accounts: vec!["invented-one".to_owned()],
        requested_numbers: Mutex::new(Vec::new()),
        accounts_requests: AtomicUsize::new(0),
        source: SourceChannel {
            source,
            parser_version: ParserVersion("finam-api/1".to_owned()),
            document: None,
        },
        identity_scope: IdentityScope::Source,
        operations: Ok(ParsedOperations {
            accepted: vec![operation],
            quarantined: Vec::new(),
        }),
        portfolio: Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Current,
            claims,
            refused: Vec::new(),
        }),
    }
}
fn dimensions(values: &[Dimension]) -> BTreeSet<Dimension> {
    values.iter().copied().collect()
}

async fn load(services: &AppServices, owner: OwnerId) -> Vec<Event> {
    services
        .store
        .load_events_through(owner, date!(2026 - 04 - 01))
        .await
        .unwrap_or_else(|error| panic!("load events: {error}"))
}

async fn load_all(services: &AppServices, owner: OwnerId) -> Vec<Event> {
    services
        .store
        .load_events_through(owner, Date::MAX)
        .await
        .unwrap_or_else(|error| panic!("load all events: {error}"))
}

/// **The operations loop writes `positionUid` as provenance, not as a
/// custody place, and mints nothing.** A position is an account and an
/// instrument (`iaam-xep0`); the channel's own handle is carried on
/// `Provenance::source_position_id`, and the leg it appends names no
/// custody at all — there is nothing left to register.
#[tokio::test]
async fn a_sync_writes_the_position_handle_as_provenance_and_registers_no_custody() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let mut operation = trade_without_custody(account, instrument);
    operation.source_position_id = Some("f1a60ae6-3f1e-43c8-8d46-042df0fdc97a".to_owned());
    let broker = api(account, SourceId::new_random(), operation);
    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("sync with no custody stated: {error}"));

    assert!(matches!(
        outcome.recorded.first(),
        Some(Verdict::Provisional { .. })
    ));
    let events = load_all(&services, owner).await;
    let trade_event = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::Trade { .. }))
        .expect("trade event recorded");
    assert!(
        trade_event.legs.iter().all(|leg| leg.custody.is_none()),
        "a broker sync must not mint a leg's custody"
    );
    assert_eq!(
        trade_event.provenance.source_position_id(),
        Some("f1a60ae6-3f1e-43c8-8d46-042df0fdc97a")
    );
    let places = services
        .directory
        .list_custody_places(owner)
        .await
        .expect("list custody places");
    assert!(places.is_empty(), "no sync may create a custody_places row");
}

/// **The portfolio loop is the second, independent append path, and it
/// mints nothing either.** A holding the broker reports but that saw no
/// trade in the interval reaches this loop with no operation behind it at
/// all — and, since a position claim carries no custody field any more,
/// there is nothing here to register in the first place.
#[tokio::test]
async fn a_position_with_no_trade_in_the_interval_registers_no_custody() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let broker = FakeBroker {
        accounts: vec!["invented-one".to_owned()],
        requested_numbers: Mutex::new(Vec::new()),
        accounts_requests: AtomicUsize::new(0),
        source: SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("finam-api/1".to_owned()),
            document: None,
        },
        identity_scope: IdentityScope::Source,
        operations: Ok(ParsedOperations {
            accepted: Vec::new(),
            quarantined: Vec::new(),
        }),
        portfolio: Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Current,
            claims: vec![ControlClaim::PositionQuantity {
                instrument,
                quantity: iaam_core::money::Quantity(Dec::one()),
                at: BalancePoint::Closing,
            }],
            refused: Vec::new(),
        }),
    };

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("portfolio-only sync with no custody stated: {error}"));

    assert_eq!(outcome.assertions, 1);
    let places = services
        .directory
        .list_custody_places(owner)
        .await
        .expect("list custody places");
    assert!(places.is_empty(), "no sync may create a custody_places row");
}

#[tokio::test]
async fn account_scope_sync_records_same_source_identifier_for_two_accounts() {
    let owner = OwnerId::new_random();
    let source = SourceId::new_random();
    let instrument = InstrumentId::new_random();
    let first_account = AccountId::new_random();
    let second_account = AccountId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, first_account, "Main");
        seed_account(store, owner, second_account, "Savings");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let first_operation = trade_without_custody(first_account, instrument);
    let second_operation = trade_without_custody(second_account, instrument);
    let make_broker = |operation| FakeBroker {
        accounts: vec!["invented-one".to_owned()],
        requested_numbers: Mutex::new(Vec::new()),
        accounts_requests: AtomicUsize::new(0),
        source: SourceChannel {
            source,
            parser_version: ParserVersion("account-scoped/1".to_owned()),
            document: None,
        },
        identity_scope: IdentityScope::Account,
        operations: Ok(ParsedOperations {
            accepted: vec![operation],
            quarantined: Vec::new(),
        }),
        portfolio: Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Current,
            claims: Vec::new(),
            refused: Vec::new(),
        }),
    };

    let first = sync_broker(
        &services,
        &principal(owner),
        &make_broker(first_operation),
        first_account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("first account sync: {error}"));
    let second = sync_broker(
        &services,
        &principal(owner),
        &make_broker(second_operation),
        second_account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("second account sync: {error}"));

    assert_eq!(first.duplicates, 0);
    assert_eq!(second.duplicates, 0);
    assert_eq!(
        load(&services, owner)
            .await
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Trade { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn api_and_report_trade_is_one_fact_and_independent_cash_is_accepted() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });
    // Declared: this place backs a fact appended directly below, outside
    // the sync under test, so the sync's own registration never reaches it.
    seed_custody(&services, owner, custody, "Test Custody").await;

    let operation = trade(account, instrument, custody);
    let report_source = SourceId::new_random();
    let existing = report_trade_event(owner, &operation, report_source);
    services
        .store
        .append_events(
            [
                vec![
                    existing.clone(),
                    report_cash_assertion(owner, account, report_source),
                ],
                report_opening_assertions(owner, account, report_source, None),
            ]
            .concat(),
            IdentityScope::Source,
        )
        .await
        .unwrap_or_else(|error| panic!("seed report: {error}"));

    let broker = api_with_claims(
        SourceId::new_random(),
        operation,
        vec![
            cash_opening_claim(),
            ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-10_000),
                at: BalancePoint::Closing,
            },
        ],
    );
    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("sync: {error}"));

    assert_eq!(outcome.duplicates, 1);
    assert_eq!(outcome.possible_duplicates, 0);
    assert!(matches!(
        outcome.recorded.first(),
        Some(Verdict::Duplicate { existing: id }) if *id == existing.id
    ));
    // Two: the opening balance the source states as well as the closing one.
    assert_eq!(outcome.assertions, 2);
    assert_eq!(outcome.assertions_withheld, None);
    let events = load(&services, owner).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Trade { .. }))
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ImportCoverageGap { .. }))
    );

    let ledger = iaam_core::reconciliation::ReconciliationLedger::build(&events)
        .unwrap_or_else(|error| panic!("ledger: {error}"));
    assert_eq!(
        ledger.status_for(
            account,
            date!(2026 - 03 - 15),
            iaam_core::reconciliation::Dimension::Cash,
        ),
        iaam_core::reconciliation::DimensionStatus::AcceptedIndependent,
    );
}

#[tokio::test]
async fn a_refused_commission_records_a_cash_gap_but_preserves_position_evidence() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });
    // Declared: this place backs a fact appended directly below, outside
    // the sync under test, so the sync's own registration never reaches it.
    seed_custody(&services, owner, custody, "Test Custody").await;

    let operation = trade(account, instrument, custody);
    let report_source = SourceId::new_random();
    let existing = report_trade_event(owner, &operation, report_source);
    services
        .store
        .append_events(
            [
                vec![
                    existing,
                    report_cash_assertion(owner, account, report_source),
                    report_position_assertion(owner, account, report_source, instrument),
                ],
                report_opening_assertions(owner, account, report_source, Some(instrument)),
            ]
            .concat(),
            IdentityScope::Source,
        )
        .await
        .unwrap_or_else(|error| panic!("seed report: {error}"));

    let api_source = SourceId::new_random();
    let mut broker = api_with_claims(
        api_source,
        operation,
        vec![
            cash_opening_claim(),
            position_opening_claim(instrument),
            ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-10_000),
                at: BalancePoint::Closing,
            },
            ControlClaim::PositionQuantity {
                instrument,
                quantity: iaam_core::money::Quantity(Dec::one()),
                at: BalancePoint::Closing,
            },
        ],
    );
    broker.operations = Ok(ParsedOperations {
        accepted: broker
            .operations
            .as_ref()
            .expect("operations")
            .accepted
            .clone(),
        quarantined: vec![iaam_app::ports::Quarantined {
            raw: serde_json::json!({"commission": "too precise"}),
            reason: "commission cannot be represented".to_owned(),
            dimensions: dimensions(&[Dimension::Cash]),
        }],
    });

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("sync: {error}"));

    // Four: an opening and a closing figure for cash and for the holding.
    assert_eq!(outcome.assertions, 4);
    let events = load_all(&services, owner).await;
    let gap = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ImportCoverageGap {
                period,
                dimensions,
                refused,
                ..
            } => Some((event, period, dimensions, refused)),
            _ => None,
        })
        .expect("coverage gap");
    assert_eq!(gap.1.from, date!(2026 - 03 - 01));
    assert_eq!(gap.1.to, date!(2026 - 03 - 31));
    assert_eq!(gap.2, &dimensions(&[Dimension::Cash]));
    assert_eq!(*gap.3, 1);
    assert_eq!(gap.0.provenance.source(), api_source);
    assert_eq!(gap.0.provenance.parser_version().0, "finam-api/1");

    let ledger = iaam_core::reconciliation::ReconciliationLedger::build(&events)
        .unwrap_or_else(|error| panic!("ledger: {error}"));
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Positions,),
        iaam_core::reconciliation::DimensionStatus::AcceptedIndependent
    );
    assert_ne!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        iaam_core::reconciliation::DimensionStatus::AcceptedIndependent
    );
}

#[tokio::test]
async fn repeating_refused_sync_appends_one_coverage_gap() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let operation = trade_without_custody(account, instrument);
    let mut broker = api(account, SourceId::new_random(), operation);
    broker.operations = Ok(ParsedOperations {
        accepted: broker
            .operations
            .as_ref()
            .expect("operations")
            .accepted
            .clone(),
        quarantined: vec![iaam_app::ports::Quarantined {
            raw: serde_json::Value::Null,
            reason: "commission cannot be represented".to_owned(),
            dimensions: dimensions(&[Dimension::Cash]),
        }],
    });

    let first = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("first refused sync: {error}"));
    let second = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("second refused sync: {error}"));

    assert_eq!(first.assertions, 1);
    assert_eq!(second.assertions, 0);
    assert_eq!(
        load_all(&services, owner)
            .await
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ImportCoverageGap { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_later_refusal_widens_the_existing_gap_for_reconciliation() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });
    // Declared: this place backs a fact appended directly below, outside
    // the sync under test, so the sync's own registration never reaches it.
    seed_custody(&services, owner, custody, "Test Custody").await;

    let operation = trade(account, instrument, custody);
    let report_source = SourceId::new_random();
    services
        .store
        .append_events(
            [
                vec![
                    report_trade_event(owner, &operation, report_source),
                    report_cash_assertion(owner, account, report_source),
                    report_position_assertion(owner, account, report_source, instrument),
                ],
                report_opening_assertions(owner, account, report_source, Some(instrument)),
            ]
            .concat(),
            IdentityScope::Source,
        )
        .await
        .unwrap_or_else(|error| panic!("seed report: {error}"));

    let api_source = SourceId::new_random();
    let mut broker = api_with_claims(
        api_source,
        operation,
        vec![
            cash_opening_claim(),
            position_opening_claim(instrument),
            ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-10_000),
                at: BalancePoint::Closing,
            },
            ControlClaim::PositionQuantity {
                instrument,
                quantity: iaam_core::money::Quantity(Dec::one()),
                at: BalancePoint::Closing,
            },
        ],
    );
    broker.operations = Ok(ParsedOperations {
        accepted: broker
            .operations
            .as_ref()
            .expect("operations")
            .accepted
            .clone(),
        quarantined: vec![iaam_app::ports::Quarantined {
            raw: serde_json::Value::Null,
            reason: "commission cannot be represented".to_owned(),
            dimensions: dimensions(&[Dimension::Cash]),
        }],
    });

    sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("first sync: {error}"));
    let first_events = load_all(&services, owner).await;
    let first_ledger = iaam_core::reconciliation::ReconciliationLedger::build(&first_events)
        .unwrap_or_else(|error| panic!("first ledger: {error}"));
    assert_eq!(
        first_ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
        iaam_core::reconciliation::DimensionStatus::AcceptedIndependent
    );

    broker.operations = Ok(ParsedOperations {
        accepted: broker
            .operations
            .as_ref()
            .expect("operations")
            .accepted
            .clone(),
        quarantined: vec![
            iaam_app::ports::Quarantined {
                raw: serde_json::Value::Null,
                reason: "commission cannot be represented".to_owned(),
                dimensions: dimensions(&[Dimension::Cash]),
            },
            iaam_app::ports::Quarantined {
                raw: serde_json::Value::Null,
                reason: "inbound securities transfer".to_owned(),
                dimensions: dimensions(&[Dimension::Positions, Dimension::TaxBasis]),
            },
        ],
    });
    sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("widening sync: {error}"));

    let events = load_all(&services, owner).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ImportCoverageGap { .. }))
            .count(),
        2
    );
    let ledger = iaam_core::reconciliation::ReconciliationLedger::build(&events)
        .unwrap_or_else(|error| panic!("widened ledger: {error}"));
    assert_ne!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
        iaam_core::reconciliation::DimensionStatus::AcceptedIndependent
    );
}

#[tokio::test]
async fn a_fingerprint_match_is_recorded_as_a_possible_duplicate() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });
    // Declared: this place backs a fact appended directly below, outside
    // the sync under test, so the sync's own registration never reaches it.
    seed_custody(&services, owner, custody, "Test Custody").await;

    let mut operation = trade(account, instrument, custody);
    operation.source_operation_id = None;
    let report_source = SourceId::new_random();
    let existing = report_trade_event(owner, &operation, report_source);
    services
        .store
        .append_events(vec![existing.clone()], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("seed report: {error}"));

    let broker = api(account, SourceId::new_random(), operation);
    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("sync: {error}"));

    assert_eq!(outcome.possible_duplicates, 1);
    let Verdict::PossibleDuplicate { event, of, level } = &outcome.recorded[0] else {
        panic!(
            "expected possible duplicate verdict: {:?}",
            outcome.recorded[0]
        );
    };
    assert_ne!(*event, existing.id);
    assert_eq!(*of, existing.id);
    assert_eq!(*level, DedupLevel::Probabilistic);
    assert!(
        load(&services, owner)
            .await
            .iter()
            .any(|record| record.id == *event),
        "possible duplicate event must be recorded"
    );
}

#[tokio::test]
async fn a_renumbered_operation_is_recorded_as_a_possible_duplicate() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });
    // Declared: this place backs a fact appended directly below, outside
    // the sync under test, so the sync's own registration never reaches it.
    seed_custody(&services, owner, custody, "Test Custody").await;

    let mut operation = trade(account, instrument, custody);
    operation.source_operation_id = Some("REN_NUMBERED-2".to_owned());
    let report_source = SourceId::new_random();
    let existing = report_trade_event(owner, &operation, report_source);
    services
        .store
        .append_events(vec![existing.clone()], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("seed report: {error}"));
    assert_ne!(
        operation.source_operation_id.as_deref(),
        existing.provenance.source_operation_id()
    );

    let broker = api(account, SourceId::new_random(), operation);
    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("sync: {error}"));

    assert_eq!(outcome.possible_duplicates, 1);
    let Verdict::PossibleDuplicate { event, of, level } = &outcome.recorded[0] else {
        panic!(
            "expected possible duplicate verdict: {:?}",
            outcome.recorded[0]
        );
    };
    assert_ne!(*event, existing.id);
    assert_eq!(*of, existing.id);
    assert_eq!(*level, DedupLevel::Probabilistic);
    assert!(
        load(&services, owner)
            .await
            .iter()
            .any(|record| record.id == *event),
        "possible duplicate event must be recorded"
    );
}

#[tokio::test]
async fn mixed_sync_counts_possible_duplicates_separately_from_duplicates() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let fresh_instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
        seed_instrument(store, fresh_instrument, "FRESHSHARE");
    });
    // Declared: this place backs a fact appended directly below, outside
    // the sync under test, so the sync's own registration never reaches it.
    seed_custody(&services, owner, custody, "Test Custody").await;

    let duplicate = trade(account, instrument, custody);
    let existing = report_trade_event(owner, &duplicate, SourceId::new_random());
    services
        .store
        .append_events(vec![existing.clone()], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("seed report: {error}"));

    let mut possible = duplicate.clone();
    possible.source_operation_id = None;
    let mut fresh = trade_without_custody(account, fresh_instrument);
    fresh.source_operation_id = Some("FRESH-1".to_owned());
    let mut broker = api(account, SourceId::new_random(), possible.clone());
    broker.operations = Ok(ParsedOperations {
        accepted: vec![possible, duplicate, fresh],
        quarantined: Vec::new(),
    });

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("sync: {error}"));

    assert_eq!(outcome.possible_duplicates, 1);
    assert_eq!(outcome.duplicates, 1);
    assert!(matches!(
        outcome.recorded.first(),
        Some(Verdict::PossibleDuplicate { of, .. }) if *of == existing.id
    ));
    assert!(matches!(
        outcome.recorded.get(1),
        Some(Verdict::Duplicate { existing: id }) if *id == existing.id
    ));
    assert!(matches!(
        outcome.recorded.get(2),
        Some(Verdict::Provisional { .. })
    ));
    assert_eq!(load(&services, owner).await.len(), 4);
}

#[tokio::test]
async fn corrected_parser_records_new_assertion_while_document_hash_stays_parser_independent() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let operation = trade_without_custody(account, instrument);
    let mut broker = api(account, SourceId::new_random(), operation);

    let first = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("first sync: {error}"));

    broker.source.parser_version = ParserVersion("finam-api/2".to_owned());
    let second = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("corrected parser sync: {error}"));

    assert_eq!(first.assertions, 1);
    assert_eq!(second.assertions, 1);
    assert_eq!(second.duplicates, 1);
    assert!(matches!(
        second.recorded.last(),
        Some(Verdict::Provisional { .. })
    ));

    let events = load_all(&services, owner).await;
    let assertions: Vec<&Event> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ControlAssertion { .. }))
        .collect();
    assert_eq!(assertions.len(), 2);
    assert_eq!(
        assertions[0].provenance.raw_hash(),
        assertions[1].provenance.raw_hash(),
        "reparsing changes the parser version, not the source document"
    );
}

#[tokio::test]
async fn repeating_sync_is_idempotent_for_operations_and_assertions() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let operation = trade_without_custody(account, instrument);
    let broker = api(account, SourceId::new_random(), operation);
    let first = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("first sync: {error}"));
    let second = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("second sync: {error}"));

    assert_eq!(first.assertions, 1);
    assert_eq!(second.duplicates, 2);
    assert_eq!(second.assertions, 0);
    assert_eq!(load(&services, owner).await.len(), 2);
}

#[tokio::test]
async fn partial_operations_record_control_assertion_and_coverage_gap() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let operation = trade_without_custody(account, instrument);
    let mut broker = api(account, SourceId::new_random(), operation);
    broker.operations = Ok(ParsedOperations {
        accepted: broker
            .operations
            .as_ref()
            .unwrap_or_else(|error| panic!("operations: {error}"))
            .accepted
            .clone(),
        quarantined: vec![iaam_app::ports::Quarantined {
            raw: serde_json::json!({"row": "bad"}),
            reason: "incomplete export".to_owned(),
            dimensions: dimensions(&[Dimension::Cash]),
        }],
    });

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("partial sync: {error}"));

    assert_eq!(outcome.assertions, 1);
    assert_eq!(load(&services, owner).await.len(), 3);
}

#[tokio::test]
async fn a_transfer_refusal_becomes_a_quarantined_verdict_without_losing_other_rows() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let first_instrument = InstrumentId::new_random();
    let second_instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, first_instrument, "SHAREONE");
        seed_instrument(store, second_instrument, "SHARETWO");
    });

    let first = trade_without_custody(account, first_instrument);
    let mut second = trade_without_custody(account, second_instrument);
    second.source_operation_id = Some("TRADE-MARCH-2".to_owned());
    let mut broker = api(account, SourceId::new_random(), first);
    broker.operations = Ok(ParsedOperations {
        accepted: vec![
            broker.operations.as_ref().expect("operations").accepted[0].clone(),
            second,
        ],
        quarantined: vec![iaam_app::ports::Quarantined {
            raw: serde_json::Value::Null,
            reason: "transfer does not contain a recipient account".to_owned(),
            dimensions: dimensions(&[Dimension::Cash]),
        }],
    });

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("partial transfer sync: {error}"));

    assert_eq!(outcome.recorded.len(), 5);
    assert!(outcome.recorded.iter().any(|verdict| {
        matches!(
            verdict,
            Verdict::Quarantined { reason }
                if reason == "transfer does not contain a recipient account"
        )
    }));
    assert_eq!(load_all(&services, owner).await.len(), 4);
}

#[tokio::test]
async fn bond_amortisation_and_unknown_rows_become_quarantined_verdicts() {
    for reason in [
        "bond amortisation: the channel does not report the returned face value per unit",
        "unsupported operation kind: OPERATION_TYPE_UNKNOWN",
    ] {
        let owner = OwnerId::new_random();
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let services = services_with(date!(2026 - 03 - 31), |store| {
            seed_account(store, owner, account, "Main");
            seed_instrument(store, instrument, "TESTSHARE");
        });

        let broker_operation = trade_without_custody(account, instrument);
        let mut broker = api(account, SourceId::new_random(), broker_operation);
        broker.operations = Ok(ParsedOperations {
            accepted: broker
                .operations
                .as_ref()
                .expect("operations")
                .accepted
                .clone(),
            quarantined: vec![iaam_app::ports::Quarantined {
                raw: serde_json::Value::Null,
                reason: reason.to_owned(),
                dimensions: if reason.starts_with("bond amortisation") {
                    dimensions(&[Dimension::Cash])
                } else {
                    dimensions(&Dimension::all())
                },
            }],
        });

        let outcome = sync_broker(
            &services,
            &principal(owner),
            &broker,
            account,
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        )
        .await
        .unwrap_or_else(|error| panic!("partial corporate action sync: {error}"));

        assert!(outcome.recorded.iter().any(|verdict| {
            matches!(verdict, Verdict::Quarantined { reason: actual } if actual == reason)
        }));
        assert_eq!(load_all(&services, owner).await.len(), 3);
    }
}

#[tokio::test]
async fn a_normalisation_rejection_stops_one_row_and_records_the_other_rows() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let valid = trade_without_custody(account, instrument);
    let mut invalid = trade_without_custody(account, InstrumentId::new_random());
    invalid.dates = OperationDates::default();
    invalid.source_operation_id = Some("INVALID-MARCH-1".to_owned());
    let mut broker = api(account, SourceId::new_random(), valid.clone());
    broker.operations = Ok(ParsedOperations {
        accepted: vec![valid, invalid],
        quarantined: Vec::new(),
    });

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("normalisation rejection sync: {error}"));

    assert!(outcome.recorded.iter().any(|verdict| {
        matches!(
            verdict,
            Verdict::Rejected { rejection } if rejection.field == "dates"
        )
    }));
    assert_eq!(outcome.assertions, 1);
    let gap_dimensions = load_all(&services, owner)
        .await
        .into_iter()
        .find_map(|event| match event.kind {
            EventKind::ImportCoverageGap { dimensions, .. } => Some(dimensions),
            _ => None,
        })
        .expect("normalisation rejection coverage gap");
    assert_eq!(
        gap_dimensions,
        dimensions(&[Dimension::Cash, Dimension::Positions])
    );
    assert_eq!(load_all(&services, owner).await.len(), 3);
}

#[tokio::test]
async fn a_structural_rejection_stops_one_operation_and_records_its_dimensions() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
    });
    let mut invalid = trade(account, InstrumentId::new_random(), CustodyId::new_random());
    let OperationKind::Buy { quantity, .. } = &mut invalid.kind else {
        panic!("trade fixture must be a buy");
    };
    *quantity = Dec::zero();
    let mut broker = api(
        account,
        SourceId::new_random(),
        trade(account, InstrumentId::new_random(), CustodyId::new_random()),
    );
    broker.operations = Ok(ParsedOperations {
        accepted: vec![invalid],
        quarantined: Vec::new(),
    });

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("structural rejection sync: {error}"));

    assert!(outcome.recorded.iter().any(|verdict| {
        matches!(
            verdict,
            Verdict::Rejected { rejection }
                if rejection.field == "operation"
                    && rejection.expected == "event shape matching its type"
        )
    }));
    let events = load_all(&services, owner).await;
    let gap_dimensions = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ImportCoverageGap { dimensions, .. } => Some(dimensions),
            _ => None,
        })
        .expect("structural rejection coverage gap");
    assert_eq!(
        gap_dimensions,
        &dimensions(&[Dimension::Cash, Dimension::Positions])
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Trade { .. }))
            .count(),
        0
    );
}

#[tokio::test]
async fn append_checked_rejects_a_batch_before_any_event_is_written() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let mut invalid_operation = trade(account, instrument, custody);
    let OperationKind::Buy { quantity, .. } = &mut invalid_operation.kind else {
        panic!("trade fixture must be a buy");
    };
    *quantity = Dec::zero();
    let invalid = iaam_ingest::normalize(
        &invalid_operation,
        &iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
            parser_version: ParserVersion(PARSER_VERSION.to_owned()),
        },
    )
    .unwrap_or_else(|error| panic!("invalid fixture normalisation: {error:?}"))
    .event;
    let valid = iaam_ingest::normalize(
        &trade(account, instrument, custody),
        &iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
            parser_version: ParserVersion(PARSER_VERSION.to_owned()),
        },
    )
    .unwrap_or_else(|error| panic!("valid fixture normalisation: {error:?}"))
    .event;

    let error = append_checked(&services, vec![invalid, valid], IdentityScope::Source)
        .await
        .expect_err("invalid batch must be refused");
    assert!(matches!(
        error,
        AppError::Invalid {
            field,
            expected,
            actual,
        } if field == "event[0]"
            && expected == "event shape matching its type"
            && actual.contains("quantity")
    ));
    assert!(load_all(&services, owner).await.is_empty());
}

#[tokio::test]
async fn existing_quarantine_reasons_reach_the_owner_as_row_verdicts() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let mut broker = api(
        account,
        SourceId::new_random(),
        trade_without_custody(account, instrument),
    );
    broker.operations = Ok(ParsedOperations {
        accepted: broker
            .operations
            .as_ref()
            .expect("operations")
            .accepted
            .clone(),
        quarantined: [
            "order state OPERATION_STATE_CANCELED: non-trade operation is refused for lack of evidence that money moved",
            "inbound securities transfer: securities moved without a cash movement",
            "trading operation does not contain positionUid",
        ]
        .into_iter()
        .map(|reason| iaam_app::ports::Quarantined {
            raw: serde_json::Value::Null,
            reason: reason.to_owned(),
            dimensions: if reason.starts_with("order state")
                || reason.starts_with("trading operation")
            {
                dimensions(&[Dimension::Cash, Dimension::Positions])
            } else {
                dimensions(&[Dimension::Positions, Dimension::TaxBasis])
            },
        })
        .collect(),
    });

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("existing quarantine sync: {error}"));

    let reasons: Vec<&str> = outcome
        .recorded
        .iter()
        .filter_map(|verdict| match verdict {
            Verdict::Quarantined { reason } => Some(reason.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasons,
        vec![
            "order state OPERATION_STATE_CANCELED: non-trade operation is refused for lack of evidence that money moved",
            "inbound securities transfer: securities moved without a cash movement",
            "trading operation does not contain positionUid",
        ]
    );
}

#[tokio::test]
async fn one_broker_failure_does_not_poison_another_sync() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let failed = FakeBroker {
        accounts: vec!["invented-one".to_owned()],
        requested_numbers: Mutex::new(Vec::new()),
        accounts_requests: AtomicUsize::new(0),
        source: SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("finam-api/1".to_owned()),
            document: None,
        },
        identity_scope: IdentityScope::Source,
        operations: Err(BrokerError::Unreachable {
            broker: "finam".to_owned(),
            detail: "offline".to_owned(),
            retry_after: None,
        }),
        portfolio: Err(BrokerError::Unreachable {
            broker: "finam".to_owned(),
            detail: "offline".to_owned(),
            retry_after: None,
        }),
    };
    let error = sync_broker(
        &services,
        &principal(owner),
        &failed,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await;
    assert!(error.is_err());

    let good = api(
        account,
        SourceId::new_random(),
        trade_without_custody(account, instrument),
    );
    let outcome = sync_broker(
        &services,
        &principal(owner),
        &good,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("good sync: {error}"));
    assert_eq!(outcome.assertions, 1);
    assert_eq!(load(&services, owner).await.len(), 2);
}

#[tokio::test]
async fn out_of_interval_trade_fact_is_recorded_without_a_control_assertion() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let mut operation = trade_without_custody(account, instrument);
    operation.dates.trade = Some(date!(2026 - 04 - 02));
    let broker = api(account, SourceId::new_random(), operation);

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect("the fact remains recordable");

    assert_eq!(outcome.assertions, 0);
    let events = load_all(&services, owner).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::Trade { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ControlAssertion { .. }))
    );
}

#[tokio::test]
async fn a_current_portfolio_is_withheld_when_interval_ends_before_clock_date() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 04 - 01), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let broker = api(
        account,
        SourceId::new_random(),
        trade_without_custody(account, instrument),
    );

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("past sync: {error}"));

    assert_eq!(outcome.assertions, 0);
    assert_eq!(
        outcome.assertions_withheld,
        Some(AssertionsWithheld::PortfolioDescribesAnotherDay {
            as_of: date!(2026 - 04 - 01)
        })
    );
    assert_eq!(
        outcome.assertions_withheld.map(|withheld| withheld.code()),
        Some("portfolio_describes_another_day"),
    );
    let events = load_all(&services, owner).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::Trade { .. })),
        "operation facts remain recordable"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ControlAssertion { .. })),
        "a portfolio from another day must not become an assertion"
    );
}
#[tokio::test]
async fn a_current_portfolio_is_withheld_when_interval_contains_today_but_ends_later() {
    let today = date!(2026 - 04 - 01);
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(today, |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let mut operation = trade_without_custody(account, instrument);
    operation.dates.trade = Some(today);
    let broker = api(account, SourceId::new_random(), operation);

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 22),
        date!(2026 - 04 - 11),
    )
    .await
    .unwrap_or_else(|error| panic!("future-ending sync: {error}"));

    assert_eq!(outcome.assertions, 0);
    assert_eq!(
        outcome.assertions_withheld,
        Some(AssertionsWithheld::PortfolioDescribesAnotherDay { as_of: today })
    );
    let events = load_all(&services, owner).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ControlAssertion { .. })),
        "a current portfolio cannot be recorded as a future closing assertion"
    );
}

#[tokio::test]
async fn a_requested_portfolio_is_recorded_for_its_requested_interval() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 04 - 01), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });

    let mut broker = api(
        account,
        SourceId::new_random(),
        trade_without_custody(account, instrument),
    );
    broker.portfolio.as_mut().expect("portfolio").as_of = PortfolioAsOf::Requested;

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("historical sync: {error}"));

    assert_eq!(outcome.assertions, 1);
    assert_eq!(outcome.assertions_withheld, None);
    let events = load_all(&services, owner).await;
    let assertion_period = events.iter().find_map(|event| match event.kind {
        EventKind::ControlAssertion { period, .. } => Some(period),
        _ => None,
    });
    assert_eq!(
        assertion_period,
        Some(
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
                .expect("requested interval"),
        )
    );
}

// --- one sync per account, and a deadline on the whole sync -----------------

/// An empty sync source: no operations, and a portfolio with nothing in it.
fn empty_operations() -> ParsedOperations {
    ParsedOperations {
        accepted: Vec::new(),
        quarantined: Vec::new(),
    }
}

fn empty_portfolio() -> PortfolioSnapshot {
    PortfolioSnapshot {
        as_of: PortfolioAsOf::Requested,
        claims: Vec::new(),
        refused: Vec::new(),
    }
}

fn held_channel() -> SourceChannel {
    SourceChannel {
        source: SourceId::new_random(),
        parser_version: ParserVersion("held-api/1".to_owned()),
        document: None,
    }
}

/// A broker that holds its operations request open until it is released,
/// and counts the requests it was sent.
struct HeldBroker {
    source: SourceChannel,
    calls: AtomicUsize,
    entered: Notify,
    release: Notify,
}

impl HeldBroker {
    fn new() -> Self {
        Self {
            source: held_channel(),
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }

    /// A broker that answers at once: its release is granted in advance.
    fn released() -> Self {
        let broker = Self::new();
        broker.release.notify_one();
        broker
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BrokerChannel for HeldBroker {
    async fn fetch_account_numbers(
        &self,
        _context: BrokerRequestContext<'_>,
    ) -> Result<Vec<String>, BrokerError> {
        Ok(Vec::new())
    }

    async fn fetch_operations(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _from: Date,
        _to: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, BrokerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(empty_operations())
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _at: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        Ok(empty_portfolio())
    }

    fn channel(&self) -> SourceChannel {
        self.source.clone()
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Account
    }
}

/// Captures the sync-owned allowance while holding both account syncs at the
/// same operation boundary, proving they are live together.
struct AllowanceBroker {
    source: SourceChannel,
    allowances: Mutex<Vec<RequestAllowance>>,
    entered: AtomicUsize,
    both_entered: Notify,
}

#[async_trait]
impl BrokerChannel for AllowanceBroker {
    async fn fetch_account_numbers(
        &self,
        _context: BrokerRequestContext<'_>,
    ) -> Result<Vec<String>, BrokerError> {
        Ok(Vec::new())
    }

    async fn fetch_operations(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _from: Date,
        _to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, BrokerError> {
        let allowance = context.allowance;
        self.allowances
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(allowance.clone());
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.both_entered.notify_waiters();
        loop {
            let notified = self.both_entered.notified();
            if self.entered.load(Ordering::SeqCst) == 2 {
                break;
            }
            notified.await;
        }
        Ok(empty_operations())
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _at: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        Ok(empty_portfolio())
    }

    fn channel(&self) -> SourceChannel {
        self.source.clone()
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Account
    }
}

struct CountingTransport(Arc<AtomicUsize>);

impl Transport for CountingTransport {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(HttpResponse {
            status: 200,
            body: Vec::new(),
            retry_after: None,
        })
    }
}

#[tokio::test]
async fn a_second_sync_of_one_account_is_refused_at_once_and_sends_nothing() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let held = HeldBroker::new();
    let second_broker = HeldBroker::released();

    let principal = principal(owner);
    let first = sync_broker(
        &services,
        &principal,
        &held,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    );
    let second = async {
        held.entered.notified().await;
        let second = sync_broker(
            &services,
            &principal,
            &second_broker,
            account,
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        )
        .await;
        held.release.notify_one();
        second
    };
    let (first, second) = tokio::join!(first, second);

    first.unwrap_or_else(|error| panic!("the first sync proceeds: {error}"));
    match second {
        Err(AppError::Conflict { what }) => assert!(
            what.contains("sync of this account is already running"),
            "{what}"
        ),
        other => panic!("the second sync must be refused as a conflict: {other:?}"),
    }
    assert_eq!(second_broker.calls(), 0, "the refused sync sent a request");
}

#[tokio::test]
async fn syncs_of_two_accounts_do_not_block_each_other() {
    let services = services();
    let owner = OwnerId::new_random();
    let held = HeldBroker::new();
    let other = HeldBroker::released();

    let principal = principal(owner);
    let first = sync_broker(
        &services,
        &principal,
        &held,
        AccountId::new_random(),
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    );
    let second = async {
        held.entered.notified().await;
        let second = sync_broker(
            &services,
            &principal,
            &other,
            AccountId::new_random(),
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        )
        .await;
        held.release.notify_one();
        second
    };
    let (first, second) = tokio::join!(first, second);

    first.unwrap_or_else(|error| panic!("the first account syncs: {error}"));
    second.unwrap_or_else(|error| panic!("the second account syncs: {error}"));
    assert_eq!(other.calls(), 1);
}

#[tokio::test]
async fn concurrent_account_syncs_own_separate_three_hundred_attempt_allowances() {
    let services = services();
    let owner = OwnerId::new_random();
    let first_account = AccountId::new_random();
    let second_account = AccountId::new_random();
    seed_bound_account(&services, owner, first_account, "first").await;
    seed_bound_account(&services, owner, second_account, "second").await;
    let before = load_all(&services, owner).await;
    let broker = AllowanceBroker {
        source: held_channel(),
        allowances: Mutex::new(Vec::new()),
        entered: AtomicUsize::new(0),
        both_entered: Notify::new(),
    };
    let principal = principal(owner);

    let (first, second) = tokio::join!(
        sync_broker(
            &services,
            &principal,
            &broker,
            first_account,
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        ),
        sync_broker(
            &services,
            &principal,
            &broker,
            second_account,
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        )
    );
    if let Err(error) = first {
        panic!("the first account did not sync: {error}");
    }
    if let Err(error) = second {
        panic!("the second account did not sync: {error}");
    }
    let allowances = broker
        .allowances
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert_eq!(allowances.len(), 2);
    assert!(
        allowances
            .iter()
            .all(|allowance| allowance.ceiling() == 300)
    );

    let sent = Arc::new(AtomicUsize::new(0));
    let time = Arc::new(FakeTime {
        now: Mutex::new(Instant::now()),
        wall: Mutex::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
    });
    let gateway = Gateway::with_parts(
        CountingTransport(Arc::clone(&sent)),
        BUDGETS,
        Arc::clone(&time) as Arc<dyn GatewayClock>,
        time as Arc<dyn Sleeper>,
        broker_egress(),
    );
    let Ok(gateway) = gateway else {
        panic!("the documented budget table was invalid");
    };
    for allowance in allowances {
        let request = HttpRequest::post(
            iaam_http::Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.OperationsService/GetOperations",
            iaam_http::RequestBody::Json("{}".to_owned()),
        )
        .idempotent()
        .with_request_allowance(allowance);
        for attempt in 1..=300 {
            if let Err(error) = gateway.send("OperationsService", &request, None).await {
                panic!("sync allowance ended at attempt {attempt}: {error}");
            }
        }
        let result = gateway.send("OperationsService", &request, None).await;
        assert!(matches!(
            result,
            Err(iaam_http::GatewayError::RequestCeiling { ceiling: 300, .. })
        ));
    }
    assert_eq!(sent.load(Ordering::SeqCst), 600);
    assert_eq!(load_all(&services, owner).await, before);
}

#[tokio::test]
async fn a_failed_sync_releases_its_account() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let failing = FakeBroker {
        accounts: vec!["invented-one".to_owned()],
        requested_numbers: Mutex::new(Vec::new()),
        accounts_requests: AtomicUsize::new(0),
        source: held_channel(),
        identity_scope: IdentityScope::Account,
        operations: Err(BrokerError::Unreachable {
            broker: "test".to_owned(),
            detail: "offline".to_owned(),
            retry_after: None,
        }),
        portfolio: Ok(empty_portfolio()),
    };
    sync_broker(
        &services,
        &principal(owner),
        &failing,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect_err("the broker is offline");

    let after = HeldBroker::released();
    sync_broker(
        &services,
        &principal(owner),
        &after,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("the sync after a failure proceeds: {error}"));
    assert_eq!(after.calls(), 1);
}

/// A broker whose operations request panics.
struct PanickingBroker(SourceChannel);

#[async_trait]
impl BrokerChannel for PanickingBroker {
    async fn fetch_account_numbers(
        &self,
        _context: BrokerRequestContext<'_>,
    ) -> Result<Vec<String>, BrokerError> {
        Ok(Vec::new())
    }

    async fn fetch_operations(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _from: Date,
        _to: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, BrokerError> {
        panic!("the broker adapter panicked");
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _at: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        Ok(empty_portfolio())
    }

    fn channel(&self) -> SourceChannel {
        self.0.clone()
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Account
    }
}

#[tokio::test]
async fn a_panicked_sync_releases_its_account() {
    let services = Arc::new(services());
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    let panicking = Arc::clone(&services);
    let joined = tokio::spawn(async move {
        sync_broker(
            &panicking,
            &principal(owner),
            &PanickingBroker(held_channel()),
            account,
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        )
        .await
    })
    .await;
    assert!(joined.is_err_and(|error| error.is_panic()));

    let after = HeldBroker::released();
    sync_broker(
        &services,
        &principal(owner),
        &after,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("the sync after a panic proceeds: {error}"));
}

/// A clock that moves only when something sleeps on it, or when the slow
/// T-Invest below takes its time to answer.
struct FakeTime {
    now: Mutex<Instant>,
    wall: Mutex<SystemTime>,
}

impl FakeTime {
    fn advance(&self, by: Duration) {
        *self.now.lock().expect("clock") += by;
        *self.wall.lock().expect("wall clock") += by;
    }
}

impl GatewayClock for FakeTime {
    fn now(&self) -> Instant {
        *self.now.lock().expect("clock")
    }

    fn now_utc(&self) -> SystemTime {
        *self.wall.lock().expect("wall clock")
    }
}

impl Sleeper for FakeTime {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.advance(delay);
        Box::pin(async {})
    }
}

/// Time on tokio's paused timer: the gateway's clock and sleeps and the slow
/// T-Invest below all run on it, so a request still in flight at the deadline
/// loses a real race against it rather than a clock moved by hand.
struct PausedTime {
    start: Instant,
}

impl GatewayClock for PausedTime {
    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }

    fn now_utc(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(1_800_000_000)
            + self.now().duration_since(self.start)
    }
}

impl Sleeper for PausedTime {
    fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(delay))
    }
}

fn broker_egress() -> iaam_http::BrokerEgress {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tally = std::env::temp_dir().join(format!(
        "iaam-app-sync-test-{}-{sequence}",
        std::process::id()
    ));
    std::fs::write(&tally, "").expect("empty tally created");
    iaam_http::BrokerEgress::On { tally }
}

/// A T-Invest that answers every operations page, always with one more to
/// come, `step` after it was asked.
struct SlowTinvest {
    step: Duration,
    asked: Arc<AtomicUsize>,
}

impl Transport for SlowTinvest {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let page = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::time::sleep(self.step).await;
        let body = format!(
            r#"{{"hasNext":true,"nextCursor":"page-{page}","items":[{{"cursor":"row-{page}","brokerAccountId":"account","id":"op-{page}","date":"2026-03-10T10:11:12Z","type":"OPERATION_TYPE_INPUT","state":"OPERATION_STATE_EXECUTED"}}]}}"#
        );
        Ok(HttpResponse {
            status: 200,
            body: body.into_bytes(),
            retry_after: None,
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_sync_that_outlasts_its_deadline_is_refused_by_it_naming_the_pages_and_writes_nothing() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
    });
    let asked = Arc::new(AtomicUsize::new(0));
    // Four minutes a page: pages are asked for at 0, 4, 8 and 12 minutes;
    // the fourth would answer at 16, past the sync's fifteen.
    let time = Arc::new(PausedTime {
        start: tokio::time::Instant::now().into_std(),
    });
    let gateway = Gateway::with_parts(
        SlowTinvest {
            step: Duration::from_secs(4 * 60),
            asked: Arc::clone(&asked),
        },
        BUDGETS,
        Arc::clone(&time) as Arc<dyn GatewayClock>,
        time as Arc<dyn Sleeper>,
        broker_egress(),
    )
    .expect("the documented table is valid");
    let key = Key::from_bytes([5; 32]);
    let token = open(&key, &seal(&key, "invented-token")).expect("token opens");
    let client = TinkoffClient::new(Environment::Prod, token, Arc::new(gateway));
    let (seed_name, seed) = seed_for("tinkoff").expect("a T-Invest seed");
    let (dictionary, unreadable) = OperationKindDictionary::build(seed.iter().copied());
    assert!(unreadable.is_empty(), "{seed_name}: {unreadable:?}");
    let channel = TinkoffChannel::new(client, SourceId::new_random(), dictionary);
    let before = load_all(&services, owner).await;
    let started = tokio::time::Instant::now();

    let refused = sync_broker(
        &services,
        &principal(owner),
        &channel,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect_err("the deadline cuts the sync");

    // The sync takes its deadline from the wall clock while the pages run on
    // the paused one, which the wall clock leads by the real time the test has
    // taken; a second covers that and is still minutes short of the overrun a
    // fourth page waited out to its end would show.
    let took = started.elapsed();
    assert!(
        took <= SYNC_DEADLINE + Duration::from_secs(1),
        "the sync returned {took:?} after it started, past its deadline"
    );
    assert!(took >= Duration::from_secs(12 * 60), "{took:?}");
    assert_eq!(asked.load(Ordering::SeqCst), 4, "the fourth page was asked");
    assert!(
        matches!(
            refused,
            AppError::SourceUnreachable {
                retry_after: Some(_),
                ..
            }
        ),
        "{refused:?}"
    );
    let message = refused.to_string();
    assert!(message.contains("unreachable"), "{message}");
    assert!(message.contains("retry after"), "{message}");
    assert!(
        message.contains("operation pages fetched before it: 3"),
        "{message}"
    );
    assert_eq!(load_all(&services, owner).await, before);
}

/// A T-Invest that answers every call with 429 and a reset delay of `wait`.
struct ThrottlingTinvest {
    wait: Duration,
}

impl Transport for ThrottlingTinvest {
    async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        Ok(HttpResponse {
            status: 429,
            body: b"{}".to_vec(),
            retry_after: Some(self.wait),
        })
    }
}

fn channel_over<T: Transport + 'static>(transport: T) -> TinkoffChannel {
    let time = Arc::new(FakeTime {
        now: Mutex::new(Instant::now()),
        wall: Mutex::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
    });
    let gateway = Gateway::with_parts(
        transport,
        BUDGETS,
        Arc::clone(&time) as Arc<dyn GatewayClock>,
        time as Arc<dyn Sleeper>,
        broker_egress(),
    )
    .expect("the documented table is valid");
    let key = Key::from_bytes([5; 32]);
    let token = open(&key, &seal(&key, "invented-token")).expect("token opens");
    let client = TinkoffClient::new(Environment::Prod, token, Arc::new(gateway));
    let (seed_name, seed) = seed_for("tinkoff").expect("a T-Invest seed");
    let (dictionary, unreadable) = OperationKindDictionary::build(seed.iter().copied());
    assert!(unreadable.is_empty(), "{seed_name}: {unreadable:?}");
    TinkoffChannel::new(client, SourceId::new_random(), dictionary)
}

#[tokio::test]
async fn a_broker_that_stays_throttled_is_unreachable_for_the_minimum_pause() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
    });
    let channel = channel_over(ThrottlingTinvest {
        wait: Duration::from_secs(7),
    });

    let refused = sync_broker(
        &services,
        &principal(owner),
        &channel,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect_err("T-Invest never lets the call through");

    match refused {
        AppError::SourceUnreachable {
            origin,
            retry_after,
            ..
        } => {
            assert_eq!(origin, "tinkoff");
            assert_eq!(retry_after, Some(Duration::from_secs(60)));
        }
        other => panic!("expected an unreachable broker, got {other:?}"),
    }
}

/// A broker whose operations request always fails with `error`.
fn failing_broker(error: BrokerError) -> FakeBroker {
    FakeBroker {
        accounts: vec!["invented-one".to_owned()],
        requested_numbers: Mutex::new(Vec::new()),
        accounts_requests: AtomicUsize::new(0),
        source: held_channel(),
        identity_scope: IdentityScope::Account,
        operations: Err(error),
        portfolio: Ok(empty_portfolio()),
    }
}

async fn sync_error(broker: &FakeBroker) -> AppError {
    let services = services();
    sync_broker(
        &services,
        &principal(OwnerId::new_random()),
        broker,
        AccountId::new_random(),
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect_err("the broker fails")
}

#[tokio::test]
async fn an_unreachable_broker_keeps_its_wait_through_the_scenario() {
    let error = sync_error(&failing_broker(BrokerError::Unreachable {
        broker: "test".to_owned(),
        detail: "offline".to_owned(),
        retry_after: Some(Duration::from_millis(1_500)),
    }))
    .await;
    match error {
        AppError::SourceUnreachable {
            origin,
            detail,
            retry_after,
        } => {
            assert_eq!(origin, "test");
            assert_eq!(detail, "offline");
            assert_eq!(retry_after, Some(Duration::from_millis(1_500)));
        }
        other => panic!("expected an unreachable broker, got {other:?}"),
    }
}

#[tokio::test]
async fn a_broker_refusal_is_not_a_store_failure() {
    let error = sync_error(&failing_broker(BrokerError::Refused {
        broker: "test".to_owned(),
        detail: "token is invalid".to_owned(),
    }))
    .await;
    match error {
        AppError::SourceRefused { origin, detail } => {
            assert_eq!(origin, "test");
            assert!(detail.starts_with("token is invalid"), "{detail}");
            // A source's refusal is general; which access to fix is the
            // broker's own advice, carried in the detail.
            assert!(detail.contains("check the broker access"), "{detail}");
        }
        other => panic!("expected a broker refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unparsable_broker_answer_is_still_our_failure() {
    let error = sync_error(&failing_broker(BrokerError::Unparsable {
        broker: "test".to_owned(),
        detail: "not json".to_owned(),
    }))
    .await;
    assert!(matches!(error, AppError::Store(_)), "{error:?}");
}

/// **Nothing is written until both of the broker's answers are in.** A sync
/// that recorded the operations and then lost the portfolio would leave a
/// journal the caller was told had not changed; fetching both first keeps the
/// refusal honest.
#[tokio::test]
async fn a_broker_whose_portfolio_fails_leaves_the_journal_unchanged() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let services = services_with(date!(2026 - 03 - 31), |store| {
        seed_account(store, owner, account, "Main");
        seed_instrument(store, instrument, "TESTSHARE");
    });
    let mut broker = api(
        account,
        SourceId::new_random(),
        trade_without_custody(account, instrument),
    );
    broker.portfolio = Err(BrokerError::Unreachable {
        broker: "test".to_owned(),
        detail: "offline".to_owned(),
        retry_after: None,
    });

    let error = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect_err("the portfolio request fails");

    assert!(
        matches!(error, AppError::SourceUnreachable { .. }),
        "{error:?}"
    );
    assert!(load_all(&services, owner).await.is_empty());
}

// --- iaam-xzz5.3.2: a sync reaches the broker's own account --------------

/// A channel with nothing to say but an access that sees `accounts`.
fn quiet_broker(accounts: Vec<String>) -> FakeBroker {
    FakeBroker {
        source: held_channel(),
        identity_scope: IdentityScope::Account,
        operations: Ok(empty_operations()),
        portfolio: Ok(empty_portfolio()),
        accounts,
        requested_numbers: Mutex::new(Vec::new()),
        accounts_requests: AtomicUsize::new(0),
    }
}

async fn seed_bound_account(
    services: &AppServices,
    owner: OwnerId,
    account: AccountId,
    number: &str,
) {
    services
        .store
        .upsert_account(
            owner,
            iaam_app::ports::AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Test Bank".to_owned()),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("seed account: {error}"));
    services
        .store
        .record_broker_account_binding(owner, account, &broker_code(), number.to_owned())
        .await
        .unwrap_or_else(|error| panic!("seed binding: {error}"));
}

#[tokio::test]
async fn a_bound_account_syncs_with_the_brokers_own_number() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    seed_bound_account(&services, owner, account, "bound-7").await;
    let broker = quiet_broker(vec!["seen-1".to_owned(), "seen-2".to_owned()]);

    let outcome = iaam_app::sync::sync_broker(
        &services,
        &principal(owner),
        &broker,
        iaam_app::sync::BrokerSyncRequest {
            broker_code: broker_code(),
            account,
            from: date!(2026 - 03 - 01),
            to: date!(2026 - 03 - 31),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("sync: {error}"));

    assert_eq!(
        broker
            .requested_numbers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_slice(),
        ["bound-7", "bound-7"],
        "both requests carry the bound number, and only it"
    );
    assert_eq!(
        broker.accounts_requests.load(Ordering::SeqCst),
        0,
        "a stored binding means the access is never asked what it sees"
    );
    assert!(!outcome.binding_recorded, "the binding already stood");
}

#[tokio::test]
async fn an_unbound_account_is_bound_to_the_one_account_the_access_sees() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    services
        .store
        .upsert_account(
            owner,
            iaam_app::ports::AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Test Bank".to_owned()),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("seed account: {error}"));
    let broker = quiet_broker(vec!["solo".to_owned()]);

    let outcome = iaam_app::sync::sync_broker(
        &services,
        &principal(owner),
        &broker,
        iaam_app::sync::BrokerSyncRequest {
            broker_code: broker_code(),
            account,
            from: date!(2026 - 03 - 01),
            to: date!(2026 - 03 - 31),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("sync: {error}"));

    assert!(
        outcome.binding_recorded,
        "the answer says the sync bound it"
    );
    assert_eq!(
        services
            .store
            .broker_account_binding(owner, account, &broker_code())
            .await
            .unwrap_or_else(|error| panic!("binding read: {error}")),
        Some("solo".to_owned()),
    );
    assert_eq!(
        broker
            .requested_numbers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_slice(),
        ["solo", "solo"],
    );
}
/// **A sync that fails after discovering the account leaves no binding.**
/// The binding is written only once both of the broker's answers are in:
/// a number whose operations or portfolio never arrived has proven
/// nothing, and a binding recorded anyway would make the next sync skip
/// the discovery and trust a number nothing confirmed.
#[tokio::test]
async fn a_sync_that_fails_after_discovery_leaves_no_binding() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    services
        .store
        .upsert_account(
            owner,
            iaam_app::ports::AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Test Bank".to_owned()),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("seed account: {error}"));
    let mut broker = quiet_broker(vec!["solo".to_owned()]);
    broker.operations = Err(BrokerError::Unreachable {
        broker: "test".to_owned(),
        detail: "offline".to_owned(),
        retry_after: None,
    });

    let error = iaam_app::sync::sync_broker(
        &services,
        &principal(owner),
        &broker,
        iaam_app::sync::BrokerSyncRequest {
            broker_code: broker_code(),
            account,
            from: date!(2026 - 03 - 01),
            to: date!(2026 - 03 - 31),
        },
    )
    .await
    .expect_err("the operations request fails");

    assert!(
        matches!(error, AppError::SourceUnreachable { .. }),
        "{error:?}"
    );
    assert!(
        load_all(&services, owner).await.is_empty(),
        "the journal is unchanged"
    );
    assert_eq!(
        services
            .store
            .broker_account_binding(owner, account, &broker_code())
            .await
            .unwrap_or_else(|error| panic!("binding read: {error}")),
        None,
        "no binding stands behind a failed sync"
    );

    // The next sync asks the access again and binds on its own success.
    broker.operations = Ok(empty_operations());
    let outcome = iaam_app::sync::sync_broker(
        &services,
        &principal(owner),
        &broker,
        iaam_app::sync::BrokerSyncRequest {
            broker_code: broker_code(),
            account,
            from: date!(2026 - 03 - 01),
            to: date!(2026 - 03 - 31),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("the retry: {error}"));

    assert!(outcome.binding_recorded, "the retry binds it");
    assert_eq!(
        services
            .store
            .broker_account_binding(owner, account, &broker_code())
            .await
            .unwrap_or_else(|error| panic!("binding read: {error}")),
        Some("solo".to_owned()),
    );
    assert_eq!(
        broker.accounts_requests.load(Ordering::SeqCst),
        2,
        "the retry discovered the account again"
    );
}

#[tokio::test]
async fn several_candidates_refuse_the_sync_before_anything_is_fetched() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    services
        .store
        .upsert_account(
            owner,
            iaam_app::ports::AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Test Bank".to_owned()),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("seed account: {error}"));
    let broker = quiet_broker(vec!["one".to_owned(), "two".to_owned(), "three".to_owned()]);

    let error = iaam_app::sync::sync_broker(
        &services,
        &principal(owner),
        &broker,
        iaam_app::sync::BrokerSyncRequest {
            broker_code: broker_code(),
            account,
            from: date!(2026 - 03 - 01),
            to: date!(2026 - 03 - 31),
        },
    )
    .await
    .expect_err("the sync must refuse, not guess");

    let AppError::BrokerAccountAmbiguous {
        broker: code,
        candidates,
    } = error
    else {
        panic!("expected the ambiguity refusal, got {error:?}");
    };
    assert_eq!(code, "tinkoff");
    assert_eq!(
        candidates,
        ["one", "two", "three"],
        "the refusal names them"
    );
    assert_eq!(
        broker.accounts_requests.load(Ordering::SeqCst),
        1,
        "the access was asked once, to learn the candidates"
    );
    assert!(
        broker
            .requested_numbers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty(),
        "nothing was fetched for the interval"
    );
    assert!(
        load_all(&services, owner).await.is_empty(),
        "and nothing was written"
    );
}

#[tokio::test]
async fn an_access_that_sees_no_account_refuses_the_sync() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    services
        .store
        .upsert_account(
            owner,
            iaam_app::ports::AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Test Bank".to_owned()),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("seed account: {error}"));
    let broker = quiet_broker(Vec::new());

    let error = iaam_app::sync::sync_broker(
        &services,
        &principal(owner),
        &broker,
        iaam_app::sync::BrokerSyncRequest {
            broker_code: broker_code(),
            account,
            from: date!(2026 - 03 - 01),
            to: date!(2026 - 03 - 31),
        },
    )
    .await
    .expect_err("the sync must refuse");

    assert!(
        matches!(error, AppError::BrokerAccountUnseen { ref broker } if broker == "tinkoff"),
        "expected the unseen refusal, got {error:?}"
    );
    assert!(load_all(&services, owner).await.is_empty());
}
