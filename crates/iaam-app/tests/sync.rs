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
use iaam_ingest::SubmittedOperation;
use iaam_ingest::operation::{OperationDates, OperationKind};
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
            accrued_interest_minor: None,
            currency: CurrencyCode::Rub,
        },
        dates: OperationDates {
            trade: Some(date!(2026 - 03 - 15)),
            settled: Some(date!(2026 - 03 - 17)),
            cash_posted: Some(date!(2026 - 03 - 17)),
            paid: None,
        },
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

#[tokio::test]
async fn api_and_report_trade_is_one_fact_and_independent_cash_is_accepted() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let operation = trade(account, InstrumentId::new_random(), CustodyId::new_random());
    let report_source = SourceId::new_random();
    services
        .store
        .append_events(vec![
            report_trade_event(owner, &operation, report_source),
            report_cash_assertion(owner, account, report_source),
        ])
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
            reason: "неполная выгрузка".to_owned(),
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
