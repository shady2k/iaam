use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Clock, InstrumentUpsert, Principal, Scope};
use iaam_app::scenarios::reports::{ReturnsQuery, returns};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::returns::{NotComputable, UncoveredReason};
use iaam_core::valuation::{FxSource, FxTable};
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use iaam_ingest::{SubmittedOperation, normalize};
use iaam_store::SqliteStore;
use iaam_store::market::{Coverage, PriceRow, RunOutcome, SeriesKey};
use iaam_store::reference::InstrumentRecord;
use time::macros::date;
use time::{Date, Duration};
use uuid::Uuid;

struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
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
        Arc::new(FixedClock(date!(2026 - 08 - 26))),
    )
}

fn principal(owner: OwnerId) -> Principal {
    Principal {
        token_id: Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    }
}

async fn seed_position(
    services: &AppServices,
    owner: OwnerId,
    account: AccountId,
    instrument: InstrumentId,
    custody: CustodyId,
) {
    let operation = SubmittedOperation {
        account,
        kind: OperationKind::OpeningPosition {
            instrument,
            custody,
            quantity: Dec::one(),
            cost_basis_minor: None,
            currency: CurrencyCode::Rub,
            assertions: None,
        },
        dates: OperationDates {
            trade: Some(date!(2026 - 08 - 01)),
            settled: Some(date!(2026 - 08 - 01)),
            cash_posted: Some(date!(2026 - 08 - 01)),
            paid: None,
        },
        source_time: None,
        idempotency_key: None,
        source_operation_id: None,
        source_category: None,
        description: None,
    };
    let event = normalize(
        &operation,
        iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
        },
    )
    .unwrap_or_else(|error| panic!("normalize position: {error:?}"))
    .event;
    services
        .store
        .append_events(vec![event], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("append position: {error}"));
}

async fn seed_market_price(services: &AppServices, instrument: InstrumentId) {
    seed_market_price_with(
        services,
        instrument,
        "281.39",
        "money_per_unit",
        "iss:engines/stock/markets/shares",
    )
    .await;
}

async fn seed_market_price_with(
    services: &AppServices,
    instrument: InstrumentId,
    price: &str,
    quotation_basis: &str,
    basis_evidence: &str,
) {
    let from = date!(2026 - 08 - 01);
    let to = date!(2026 - 08 - 26);
    let series = SeriesKey {
        source_id: "moex-iss".to_owned(),
        dataset: "prices".to_owned(),
        series_key: format!("{}:TQBR", instrument.inner()),
    };
    let mut store = services.market_store.lock().await;
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".to_owned(),
            title: "Sberbank".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .unwrap_or_else(|error| panic!("insert market instrument: {error}"));
    let run = store
        .begin_run(
            series,
            from,
            to,
            time::OffsetDateTime::now_utc() + Duration::hours(1),
        )
        .unwrap_or_else(|error| panic!("begin market run: {error}"));
    store
        .record_prices(
            &run,
            &"market-input-test".repeat(2),
            &[PriceRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQBR".to_owned(),
                session: 3,
                trade_date: "2026-08-03".to_owned(),
                kind: "legal_close".to_owned(),
                observed_at: "2026-08-26T09:00:00Z".to_owned(),
                price: price.to_owned(),
                currency: "RUB".to_owned(),
                quotation_basis: quotation_basis.to_owned(),
                basis_evidence: basis_evidence.to_owned(),
                executability: "indicative_previous_close".to_owned(),
            }],
        )
        .unwrap_or_else(|error| panic!("record market price: {error}"));
    store
        .finish_run(&run, RunOutcome::Succeeded, Some(Coverage { from, to }))
        .unwrap_or_else(|error| panic!("finish market run: {error}"));
}

#[tokio::test]
async fn report_values_position_from_market_observation() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    services
        .store
        .upsert_account(
            owner,
            AccountView {
                id: account,
                title: "market".to_owned(),
                institution: None,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("insert account: {error}"));

    services
        .store
        .insert_contour_version(owner, contour.clone(), "market".to_owned(), vec![account])
        .await
        .unwrap_or_else(|error| panic!("insert contour: {error}"));
    services
        .directory
        .record_instrument(InstrumentUpsert {
            id: instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".to_owned(),
            title: "Sberbank".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .await
        .unwrap_or_else(|error| panic!("insert instrument: {error}"));
    seed_position(&services, owner, account, instrument, custody).await;
    seed_market_price(&services, instrument).await;

    let report = returns(
        &services,
        &principal(owner),
        &ReturnsQuery {
            contour: contour.id(),
            contour_version: Some(contour.version()),
            as_of: Some(date!(2026 - 08 - 26)),
            report_currency: CurrencyCode::Rub,
            fx: FxTable::new(FxSource::OwnerSupplied),
            lot_rule: iaam_core::rules::LotRuleVersion(1),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("report: {error}"))
    .report;

    let selected = report
        .data_quality
        .position_coverage
        .selected
        .first()
        .unwrap_or_else(|| panic!("market position was not evaluated"));
    assert_eq!(
        selected.price.candidate.price,
        Dec::new(rust_decimal::Decimal::new(28139, 2))
    );
    assert!(matches!(
        selected.price.candidate.origin,
        iaam_core::valuation::PriceOrigin::Market { .. }
    ));
    assert_eq!(report.data_quality.position_coverage.evaluated_positions, 1);
    assert_eq!(report.data_quality.position_coverage.total_positions, 1);
}

#[tokio::test]
async fn contradictory_price_leaves_only_its_position_uncovered() {
    let services = services();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let healthy = InstrumentId::new_random();
    let contradictory = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    services
        .store
        .upsert_account(
            owner,
            AccountView {
                id: account,
                title: "market".to_owned(),
                institution: None,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("insert account: {error}"));
    services
        .store
        .insert_contour_version(owner, contour.clone(), "market".to_owned(), vec![account])
        .await
        .unwrap_or_else(|error| panic!("insert contour: {error}"));
    for instrument in [healthy, contradictory] {
        services
            .directory
            .record_instrument(InstrumentUpsert {
                id: instrument,
                kind: Some(InstrumentKind::Share),
                symbol: "SBER".to_owned(),
                title: "Sberbank".to_owned(),
                currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
                lineage: None,
            })
            .await
            .unwrap_or_else(|error| panic!("insert instrument: {error}"));
        seed_position(&services, owner, account, instrument, custody).await;
    }
    seed_market_price_with(
        &services,
        healthy,
        "281.39",
        "money_per_unit",
        "iss:engines/stock/markets/shares",
    )
    .await;
    seed_market_price_with(
        &services,
        contradictory,
        "281.39",
        "money_per_unit",
        "iss:engines/stock/markets/bonds",
    )
    .await;

    let report = returns(
        &services,
        &principal(owner),
        &ReturnsQuery {
            contour: contour.id(),
            contour_version: Some(contour.version()),
            as_of: Some(date!(2026 - 08 - 26)),
            report_currency: CurrencyCode::Rub,
            fx: FxTable::new(FxSource::OwnerSupplied),
            lot_rule: iaam_core::rules::LotRuleVersion(1),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("report: {error}"))
    .report;

    let coverage = &report.data_quality.position_coverage;
    assert_eq!(coverage.total_positions, 2);
    assert_eq!(coverage.evaluated_positions, 1);
    assert_eq!(coverage.selected[0].instrument, healthy);
    assert!(coverage.uncovered.iter().any(|position| {
        position.instrument == contradictory
            && position.reason
                == (UncoveredReason::NotComputable {
                    reason: NotComputable::QuotationBasisContradictsEvidence {
                        instrument: contradictory,
                    },
                })
    }));
}
