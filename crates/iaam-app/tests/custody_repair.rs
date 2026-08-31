use std::sync::Arc;

use async_trait::async_trait;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{BrokerAccessView, BrokerEnvironment, BrokerVault, Clock, Principal, Scope};
use iaam_app::scenarios::custody_repair::{CustodyRepairCase, repair_custody};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::observed::observe;
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use iaam_ingest::{SubmittedOperation, normalize};
use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;
use uuid::Uuid;
use zeroize::Zeroizing;

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> Date {
        date!(2026 - 03 - 31)
    }
}

struct FakeBrokerVault {
    accesses: Vec<BrokerAccessView>,
}

#[async_trait]
impl BrokerVault for FakeBrokerVault {
    async fn add_access(
        &self,
        _owner: OwnerId,
        _broker: String,
        _environment: BrokerEnvironment,
        _token: Zeroizing<String>,
    ) -> Result<BrokerAccessView, AppError> {
        Err(AppError::NotConfigured {
            what: "test broker access creation",
        })
    }

    async fn list_access(&self, _owner: OwnerId) -> Result<Vec<BrokerAccessView>, AppError> {
        Ok(self.accesses.clone())
    }

    async fn revoke_access(&self, _owner: OwnerId, _id: Uuid) -> Result<(), AppError> {
        Err(AppError::NotConfigured {
            what: "test broker access revocation",
        })
    }
}

fn principal(owner: OwnerId) -> Principal {
    Principal {
        token_id: Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    }
}

fn services(accesses: Vec<BrokerAccessView>) -> AppServices {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}")),
    ));
    AppServices::new(
        adapter.clone(),
        adapter.clone(),
        Arc::new(FakeBrokerVault { accesses }),
        adapter.clone(),
        Arc::new(FixedClock),
    )
}

fn live_access() -> BrokerAccessView {
    BrokerAccessView {
        id: Uuid::new_v4(),
        broker: "tinkoff".to_owned(),
        environment: "prod".to_owned(),
        scope: "read_only".to_owned(),
        created_at: "2026-03-01T00:00:00Z".to_owned(),
        revoked_at: None,
    }
}

fn revoked_access() -> BrokerAccessView {
    BrokerAccessView {
        revoked_at: Some("2026-03-01T00:00:00Z".to_owned()),
        ..live_access()
    }
}

fn affected_trade(owner: OwnerId, account: AccountId, instrument: InstrumentId) -> Event {
    let operation = SubmittedOperation {
        account,
        kind: OperationKind::Buy {
            instrument,
            custody: CustodyId(account.inner()),
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
        source_operation_id: Some(format!("old-{instrument:?}")),
    };
    normalize(
        &operation,
        iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
        },
    )
    .unwrap_or_else(|error| panic!("normalize affected trade: {error:?}"))
    .event
}

async fn append(services: &AppServices, events: Vec<Event>) {
    services
        .store
        .append_events(events, IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("append seed: {error}"));
}

async fn all_events(services: &AppServices, owner: OwnerId) -> Vec<Event> {
    services
        .store
        .load_events_through(owner, Date::MAX)
        .await
        .unwrap_or_else(|error| panic!("load events: {error}"))
}

fn corrected_trade(original: &Event, custody: CustodyId) -> Event {
    let mut corrected = original.clone();
    corrected.id = EventId::new_random();
    corrected.provenance = Provenance::new(
        SourceId::new_random(),
        RawHash::parse(&"d".repeat(64)).unwrap_or_else(|| panic!("corrected hash")),
        ParserVersion("tinkoff-api/4".to_owned()),
    );
    for leg in &mut corrected.legs {
        if leg.quantity.is_some() {
            leg.custody = Some(custody);
        }
    }
    corrected
}

/// What one position claim asserts. Grouped into a struct rather than passed as seven
/// arguments, which reads as a positional puzzle at the call site and trips
/// `clippy::too_many_arguments`.
struct PositionClaim<'a> {
    owner: OwnerId,
    account: AccountId,
    instrument: InstrumentId,
    custody: CustodyId,
    quantity: Quantity,
    source: SourceId,
    idempotency_key: &'a str,
}

fn position_assertion(claim: PositionClaim<'_>) -> Event {
    let PositionClaim {
        owner,
        account,
        instrument,
        custody,
        quantity,
        source,
        idempotency_key,
    } = claim;
    let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
        .unwrap_or_else(|| panic!("March period"));
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::ControlAssertion {
            period,
            claim: ControlClaim::PositionQuantity {
                instrument,
                custody,
                quantity,
                at: BalancePoint::Closing,
            },
        },
        dates: EventDates::for_cash(CashPostedDate(period.to)),
        order: EffectiveOrder::new(period.to, 0),
        legs: Vec::new(),
        provenance: Provenance::new(
            source,
            RawHash::parse(&"e".repeat(64)).unwrap_or_else(|| panic!("assertion hash")),
            ParserVersion("broker-report/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: Some(idempotency_key.to_owned()),
    }
}

#[tokio::test]
async fn repair_then_reimport_reconciles_without_doubling_the_position() {
    let services = services(vec![live_access()]);
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let original = affected_trade(owner, account, instrument);
    append(&services, vec![original.clone()]).await;

    let repaired = repair_custody(&services, &principal(owner), account, false)
        .await
        .unwrap_or_else(|error| panic!("repair: {error}"));
    assert_eq!(repaired.written, 1);

    let real_custody = CustodyId::new_random();
    let corrected = corrected_trade(&original, real_custody);
    iaam_app::scenarios::ingest::append_checked(&services, vec![corrected], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("re-import: {error}"));
    let assertion_source = SourceId::new_random();
    append(
        &services,
        vec![
            position_assertion(PositionClaim {
                owner,
                account,
                instrument,
                custody: real_custody,
                quantity: Quantity(Dec::one()),
                source: assertion_source,
                idempotency_key: "position-real",
            }),
            position_assertion(PositionClaim {
                owner,
                account,
                instrument,
                custody: CustodyId(account.inner()),
                quantity: Quantity(Dec::zero()),
                source: assertion_source,
                idempotency_key: "position-fabricated",
            }),
        ],
    )
    .await;

    let events = all_events(&services, owner).await;
    let period = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
        .unwrap_or_else(|| panic!("March period"));
    let effective = iaam_core::event::correction::resolve(&events)
        .unwrap_or_else(|error| panic!("resolve repaired journal: {error}"));
    let observed = observe(&effective, account, period)
        .unwrap_or_else(|error| panic!("observe repaired journal: {error}"));
    assert_eq!(
        observed.position_at(BalancePoint::Closing, instrument, real_custody),
        Some(Quantity(Dec::one()))
    );
    assert_eq!(
        observed.position_at(
            BalancePoint::Closing,
            instrument,
            CustodyId(account.inner())
        ),
        None
    );
    let ledger = ReconciliationLedger::build(&events)
        .unwrap_or_else(|error| panic!("reconciliation ledger: {error}"));
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 31), Dimension::Positions),
        DimensionStatus::Provisional
    );
}

#[tokio::test]
async fn repair_writes_reversals_with_fresh_provenance_and_is_idempotent() {
    let services = services(vec![live_access()]);
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let first = affected_trade(owner, account, InstrumentId::new_random());
    let second = affected_trade(owner, account, InstrumentId::new_random());
    append(&services, vec![first.clone(), second.clone()]).await;

    let outcome = repair_custody(&services, &principal(owner), account, false)
        .await
        .unwrap_or_else(|error| panic!("repair: {error}"));
    assert_eq!(outcome.case, CustodyRepairCase::AffectedWithLiveAccess);
    assert_eq!(outcome.affected_trades, 2);
    assert_eq!(outcome.already_reversed, 0);
    assert_eq!(outcome.written, 2);

    let events = all_events(&services, owner).await;
    assert_eq!(events.len(), 4);
    for original in [&first, &second] {
        let reversal = events
            .iter()
            .find(|event| {
                event.relation
                    == Relation::Reversal {
                        target: original.id,
                    }
            })
            .unwrap_or_else(|| panic!("reversal for {:?}", original.id));
        assert_eq!(reversal.kind, original.kind);
        assert_eq!(reversal.dates, original.dates);
        assert_eq!(reversal.legs, original.legs);
        assert_eq!(reversal.order.date(), original.order.date());
        assert_eq!(reversal.order.source_time(), original.order.source_time());
        assert_eq!(reversal.provenance.source(), original.provenance.source());
        assert_eq!(
            reversal.provenance.parser_version(),
            &ParserVersion("custody-repair/1".to_owned())
        );
        assert_eq!(reversal.provenance.source_operation_id(), None);
        assert_eq!(reversal.provenance.row(), None);
        assert_eq!(
            reversal.idempotency_key.as_deref(),
            Some(format!("custody-repair/{}/{}", account.inner(), original.id.inner()).as_str())
        );
        assert_ne!(reversal.id, original.id);
    }

    let second_outcome = repair_custody(&services, &principal(owner), account, false)
        .await
        .unwrap_or_else(|error| panic!("second repair: {error}"));
    assert_eq!(second_outcome.case, CustodyRepairCase::NothingAffected);
    assert_eq!(second_outcome.affected_trades, 0);
    assert_eq!(second_outcome.already_reversed, 2);
    assert_eq!(second_outcome.written, 0);
    assert_eq!(all_events(&services, owner).await.len(), 4);
}

#[tokio::test]
async fn unaffected_account_writes_nothing() {
    let services = services(Vec::new());
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut event = affected_trade(owner, account, InstrumentId::new_random());
    let real_custody = CustodyId::new_random();
    for leg in &mut event.legs {
        if leg.quantity.is_some() {
            leg.custody = Some(real_custody);
        }
    }
    append(&services, vec![event]).await;

    let outcome = repair_custody(&services, &principal(owner), account, false)
        .await
        .unwrap_or_else(|error| panic!("repair: {error}"));
    assert_eq!(outcome.case, CustodyRepairCase::NothingAffected);
    assert_eq!(outcome.affected_trades, 0);
    assert_eq!(outcome.written, 0);
    assert_eq!(all_events(&services, owner).await.len(), 1);
}

#[tokio::test]
async fn no_live_access_requires_acknowledgement_before_writing() {
    let services = services(vec![revoked_access()]);
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    append(
        &services,
        vec![affected_trade(owner, account, InstrumentId::new_random())],
    )
    .await;

    let refused = repair_custody(&services, &principal(owner), account, false)
        .await
        .unwrap_or_else(|error| panic!("repair: {error}"));
    assert_eq!(refused.case, CustodyRepairCase::AffectedWithoutLiveAccess);
    assert_eq!(refused.affected_trades, 1);
    assert_eq!(refused.written, 0);
    assert_eq!(all_events(&services, owner).await.len(), 1);

    let accepted = repair_custody(&services, &principal(owner), account, true)
        .await
        .unwrap_or_else(|error| panic!("acknowledged repair: {error}"));
    assert_eq!(accepted.case, CustodyRepairCase::AffectedWithoutLiveAccess);
    assert_eq!(accepted.written, 1);
    assert_eq!(all_events(&services, owner).await.len(), 2);
}

#[tokio::test]
async fn repair_requires_submit_scope() {
    let services = services(Vec::new());
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let mut read_only = principal(owner);
    read_only.scope = Scope::ReadOnly;

    let error = repair_custody(&services, &read_only, account, false)
        .await
        .expect_err("read-only repair must be refused");
    assert!(matches!(error, AppError::Invalid { field, .. } if field == "scope"));
}
