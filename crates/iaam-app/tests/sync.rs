use std::sync::Arc;

use async_trait::async_trait;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{BrokerChannel, BrokerError, Clock, ParsedOperations, Principal, Scope};
use iaam_app::sync::sync_broker;
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_ingest::dedup::DedupLevel;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use iaam_ingest::{SubmittedOperation, Verdict};
use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;

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
    portfolio: Result<Vec<ControlClaim>, BrokerError>,
}

#[async_trait]
impl BrokerChannel for FakeBroker {
    async fn fetch_operations(
        &self,
        _account: AccountId,
        _from: Date,
        _to: Date,
    ) -> Result<ParsedOperations, BrokerError> {
        self.operations.clone()
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _at: Date,
    ) -> Result<Vec<ControlClaim>, BrokerError> {
        self.portfolio.clone()
    }

    fn channel(&self) -> SourceChannel {
        self.source.clone()
    }

    fn identity_scope(&self) -> IdentityScope {
        self.identity_scope
    }
}

fn principal(owner: OwnerId) -> Principal {
    Principal {
        token_id: uuid::Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    }
}

fn services() -> AppServices {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}")),
    ));
    AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        Arc::new(FixedClock(date!(2026 - 04 - 01))),
    )
}

fn trade(account: AccountId, instrument: InstrumentId, custody: CustodyId) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind: OperationKind::Buy {
            instrument,
            custody,
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
    }
}

fn report_trade_event(
    owner: OwnerId,
    operation: &SubmittedOperation,
    report_source: SourceId,
) -> Event {
    let normalized = iaam_ingest::normalize(
        operation,
        iaam_ingest::operation::NormalizationContext {
            owner,
            source: report_source,
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

fn report_cash_assertion(owner: OwnerId, account: AccountId, source: SourceId) -> Event {
    let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
        .unwrap_or_else(|| panic!("March period"));
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
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

fn api(_account: AccountId, source: SourceId, operation: SubmittedOperation) -> FakeBroker {
    FakeBroker {
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
        portfolio: Ok(vec![ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(-10_000),
            at: BalancePoint::Closing,
        }]),
    }
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

fn seeded_trade(
    owner: OwnerId,
    account: AccountId,
    instrument: InstrumentId,
    custody: CustodyId,
    day: Date,
) -> Event {
    let mut operation = trade(account, instrument, custody);
    operation.dates.trade = Some(day);
    operation.dates.settled = Some(day);
    operation.dates.cash_posted = Some(day);
    iaam_ingest::normalize(
        &operation,
        iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
        },
    )
    .unwrap_or_else(|error| panic!("seed trade: {error:?}"))
    .event
}

#[tokio::test]
async fn account_scope_sync_records_same_source_identifier_for_two_accounts() {
    let services = services();
    let owner = OwnerId::new_random();
    let source = SourceId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let first_account = AccountId::new_random();
    let second_account = AccountId::new_random();
    let first_operation = trade(first_account, instrument, custody);
    let second_operation = trade(second_account, instrument, custody);
    let make_broker = |operation| FakeBroker {
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
        portfolio: Ok(Vec::new()),
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
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let operation = trade(account, InstrumentId::new_random(), CustodyId::new_random());
    let report_source = SourceId::new_random();
    let existing = report_trade_event(owner, &operation, report_source);
    services
        .store
        .append_events(
            vec![
                existing.clone(),
                report_cash_assertion(owner, account, report_source),
            ],
            IdentityScope::Source,
        )
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

    assert_eq!(outcome.duplicates, 1);
    assert_eq!(outcome.possible_duplicates, 0);
    assert!(matches!(
        outcome.recorded.first(),
        Some(Verdict::Duplicate { existing: id }) if *id == existing.id
    ));
    assert_eq!(outcome.assertions, 1);
    let events = load(&services, owner).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Trade { .. }))
            .count(),
        1
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
async fn a_fingerprint_match_is_recorded_as_a_possible_duplicate() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut operation = trade(account, InstrumentId::new_random(), CustodyId::new_random());
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
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut operation = trade(account, InstrumentId::new_random(), CustodyId::new_random());
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
    let services = services();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let duplicate = trade(account, instrument, custody);
    let existing = report_trade_event(owner, &duplicate, SourceId::new_random());
    services
        .store
        .append_events(vec![existing.clone()], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("seed report: {error}"));

    let mut possible = duplicate.clone();
    possible.source_operation_id = None;
    let mut fresh = trade(account, InstrumentId::new_random(), CustodyId::new_random());
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
async fn repeating_sync_is_idempotent_for_operations_and_assertions() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let operation = trade(account, InstrumentId::new_random(), CustodyId::new_random());
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
async fn partial_operations_do_not_create_control_assertions() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let operation = trade(account, InstrumentId::new_random(), CustodyId::new_random());
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

    assert_eq!(outcome.assertions, 0);
    assert_eq!(load(&services, owner).await.len(), 1);
}

#[tokio::test]
async fn one_broker_failure_does_not_poison_another_sync() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let failed = FakeBroker {
        source: SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("finam-api/1".to_owned()),
            document: None,
        },
        identity_scope: IdentityScope::Source,
        operations: Err(BrokerError::Unreachable {
            broker: "finam".to_owned(),
            detail: "offline".to_owned(),
        }),
        portfolio: Err(BrokerError::Unreachable {
            broker: "finam".to_owned(),
            detail: "offline".to_owned(),
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
        trade(account, InstrumentId::new_random(), CustodyId::new_random()),
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
async fn sync_refuses_account_with_account_derived_trade_custody() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let old = seeded_trade(
        owner,
        account,
        instrument,
        CustodyId(account.inner()),
        date!(2026 - 03 - 15),
    );
    services
        .store
        .append_events(vec![old], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("seed old trade: {error}"));
    let broker = api(
        account,
        SourceId::new_random(),
        trade(account, instrument, CustodyId::new_random()),
    );

    let error = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect_err("affected account must be refused");

    let text = error.to_string();
    assert!(text.contains("1"), "refusal must name the count: {text}");
    assert!(
        text.contains("iaam-y3a2"),
        "refusal must name the repair task: {text}"
    );
    assert_eq!(load_all(&services, owner).await.len(), 1);
}

#[tokio::test]
async fn sync_allows_account_with_only_position_derived_trade_custody() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let broker = api(
        account,
        SourceId::new_random(),
        trade(account, InstrumentId::new_random(), CustodyId::new_random()),
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
    .unwrap_or_else(|error| panic!("position-derived sync: {error}"));

    assert_eq!(outcome.assertions, 1);
    assert_eq!(load_all(&services, owner).await.len(), 2);
}

#[tokio::test]
async fn sync_refusal_is_scoped_to_the_affected_account() {
    let services = services();
    let owner = OwnerId::new_random();
    let affected = AccountId::new_random();
    let unaffected = AccountId::new_random();
    let old = seeded_trade(
        owner,
        affected,
        InstrumentId::new_random(),
        CustodyId(affected.inner()),
        date!(2026 - 03 - 15),
    );
    services
        .store
        .append_events(vec![old], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("seed affected trade: {error}"));
    let mut unaffected_operation = trade(
        unaffected,
        InstrumentId::new_random(),
        CustodyId::new_random(),
    );
    unaffected_operation.source_operation_id = Some("UNAFF-MARCH-1".to_owned());
    let broker = api(unaffected, SourceId::new_random(), unaffected_operation);

    let outcome = sync_broker(
        &services,
        &principal(owner),
        &broker,
        unaffected,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("unaffected account sync: {error}"));

    assert_eq!(outcome.assertions, 1);
    assert_eq!(load_all(&services, owner).await.len(), 3);
}

#[tokio::test]
async fn sync_refuses_account_derived_trade_after_requested_interval() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let old = seeded_trade(
        owner,
        account,
        InstrumentId::new_random(),
        CustodyId(account.inner()),
        date!(2026 - 04 - 15),
    );
    services
        .store
        .append_events(vec![old], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("seed later trade: {error}"));
    let broker = api(
        account,
        SourceId::new_random(),
        trade(account, InstrumentId::new_random(), CustodyId::new_random()),
    );

    let error = sync_broker(
        &services,
        &principal(owner),
        &broker,
        account,
        date!(2026 - 03 - 01),
        date!(2026 - 03 - 31),
    )
    .await
    .expect_err("later affected trade must still refuse sync");

    assert!(error.to_string().contains("1"));
    assert_eq!(load_all(&services, owner).await.len(), 1);
}

#[tokio::test]
async fn out_of_interval_trade_fact_is_recorded_without_a_control_assertion() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut operation = trade(account, InstrumentId::new_random(), CustodyId::new_random());
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
