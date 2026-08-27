//! Метаморфное свойство: повторная синхронизация ничего не меняет.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{
    AccountView, Clock, OutboundHttp, OutboundResponse, Principal, Scope,
};
use iaam_app::scenarios::reports::{ReturnsQuery, returns};
use iaam_app::scenarios::schedule::{SOURCE_ID, ScheduleSyncRequest, sync_schedule};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::FxSource;
use iaam_ingest::operation::{OperationDates, OperationKind};
use iaam_ingest::{SubmittedOperation, normalize};
use iaam_http::HttpRequest;
use iaam_store::SqliteStore;
use iaam_store::market::{Coverage, PriceRow, RunOutcome, SeriesKey};
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::reference::InstrumentRecord;
use time::Duration;
use time::macros::date;
use uuid::Uuid;
const WHOLE: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "maturity"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [
      ["2026-08-15", "2026-02-15", 34.41, 6.9, "RUB"],
      ["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]
    ]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

const EMPTY: &str = r#"{
  "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
  "coupons": {"columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
              "data": []},
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

struct Pages(Mutex<Vec<&'static str>>);

#[async_trait]
impl OutboundHttp for Pages {
    async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
        let mut bodies = self.0.lock().expect("страницы");
        let body = if bodies.is_empty() {
            EMPTY
        } else {
            bodies.remove(0)
        };
        Ok(OutboundResponse {
            status: 200,
            body: body.as_bytes().to_vec(),
            raw_hash: format!("hash-{}", body.len()),
        })
    }
}

fn store() -> (SqliteStore, InstrumentId) {
    let mut store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU46020RMFS2".to_owned(),
            title: "ОФЗ 46020".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
    store
        .extend_market_source_codes(
            SOURCE_ID,
            "профиль источника 2026-08-27",
            &[
                SourceCodeEntry {
                    domain: "currency".to_owned(),
                    source_code: "RUB".to_owned(),
                    meaning: "RUB".to_owned(),
                },
                SourceCodeEntry {
                    domain: "principal_repayment_kind".to_owned(),
                    source_code: "maturity".to_owned(),
                    meaning: "principal_return".to_owned(),
                },
            ],
        )
        .expect("словарь заселён");
    (store, instrument)
}

struct FixedClock(time::Date);

impl Clock for FixedClock {
    fn today(&self) -> time::Date {
        self.0
    }
}

fn fixture_services() -> (AppServices, OwnerId, AccountId, InstrumentId, ContourDefinition) {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().expect("база приложения"),
    ));
    let services = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        Arc::new(FixedClock(date!(2026 - 08 - 26))),
    );
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    (services, owner, account, instrument, contour)
}

async fn seed_report_position(
    services: &AppServices,
    owner: OwnerId,
    account: AccountId,
    instrument: InstrumentId,
    contour: &ContourDefinition,
) {
    services
        .store
        .upsert_account(
            owner,
            AccountView {
                id: account,
                title: "bond".to_owned(),
                institution: None,
            },
        )
        .await
        .expect("счёт");
    services
        .store
        .insert_contour_version(owner, contour.clone(), "bond".to_owned(), vec![account])
        .await
        .expect("контур");
    let operation = SubmittedOperation {
        account,
        kind: OperationKind::OpeningPosition {
            instrument,
            custody: iaam_core::ids::CustodyId::new_random(),
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
        idempotency_key: None,
        source_operation_id: None,
    };
    let event = normalize(
        &operation,
        iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
        },
    )
    .expect("нормализация")
    .event;
    services.store.append_events(vec![event]).await.expect("событие");
}

async fn seed_market_price(services: &AppServices, instrument: InstrumentId) {
    let from = date!(2026 - 08 - 01);
    let to = date!(2026 - 08 - 26);
    let series = SeriesKey {
        source_id: SOURCE_ID.to_owned(),
        dataset: "prices".to_owned(),
        series_key: format!("{}:TQOB", instrument.inner()),
    };
    let mut store = services.market_store.lock().await;
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU46020RMFS2".to_owned(),
            title: "ОФЗ 46020".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("рыночный инструмент");
    store
        .extend_market_source_codes(
            SOURCE_ID,
            "профиль источника",
            &[
                SourceCodeEntry {
                    domain: "currency".to_owned(),
                    source_code: "RUB".to_owned(),
                    meaning: "RUB".to_owned(),
                },
                SourceCodeEntry {
                    domain: "principal_repayment_kind".to_owned(),
                    source_code: "maturity".to_owned(),
                    meaning: "principal_return".to_owned(),
                },
            ],
        )
        .expect("словарь источника");
    let run = store
        .begin_run(
            series,
            from,
            to,
            time::OffsetDateTime::now_utc() + Duration::hours(1),
        )
        .expect("рыночный запуск");
    store
        .record_prices(
            &run,
            &"market-input-test".repeat(2),
            &[PriceRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQOB".to_owned(),
                session: 3,
                trade_date: "2026-08-03".to_owned(),
                kind: "legal_close".to_owned(),
                observed_at: "2026-08-26T09:00:00Z".to_owned(),
                price: "98.5".to_owned(),
                currency: "RUB".to_owned(),
                quotation_basis: "percent_of_remaining_face".to_owned(),
                basis_evidence: "test:market".to_owned(),
                executability: "executable".to_owned(),
            }],
        )
        .expect("рыночная цена");
    store
        .finish_run(&run, RunOutcome::Succeeded, Some(Coverage { from, to }))
        .expect("завершение рыночного запуска");
}

async fn sync_fixture_schedule(services: &AppServices, instrument: InstrumentId) {
    let mut store = services.market_store.lock().await;
    sync_schedule(
        &mut store,
        &Pages(Mutex::new(vec![WHOLE])),
        ScheduleSyncRequest {
            instrument,
            secid: "SU46020RMFS2".to_owned(),
        },
    )
    .await
    .expect("синхронизация графика");
}

fn principal(owner: OwnerId) -> Principal {
    Principal {
        token_id: Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    }
}

#[tokio::test]
async fn a_second_sync_of_an_unchanged_schedule_changes_nothing() {
    // Синхронизация — не событие: если источник прислал то же самое,
    // нового снимка быть не должно, и чтение на любую координату обязано
    // дать тот же ответ. Иначе ежедневный прогон раздувает ряд и делает
    // ось «когда мы узнали» бессмысленной.
    let (mut store, instrument) = store();
    let request = || ScheduleSyncRequest {
        instrument,
        secid: "SU46020RMFS2".to_owned(),
    };

    sync_schedule(&mut store, &Pages(Mutex::new(vec![WHOLE])), request())
        .await
        .expect("первый прогон");
    let after_first = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("снимок найден");

    sync_schedule(&mut store, &Pages(Mutex::new(vec![WHOLE])), request())
        .await
        .expect("второй прогон");
    let after_second = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("снимок найден");

    assert_eq!(after_first, after_second, "повтор изменил ответ");
}

#[tokio::test]
async fn resyncing_changes_no_bond_attribute_at_a_fixed_coordinate() {
    let (services, owner, account, instrument, contour) = fixture_services();
    seed_report_position(&services, owner, account, instrument, &contour).await;
    seed_market_price(&services, instrument).await;

    sync_fixture_schedule(&services, instrument).await;
    let query = ReturnsQuery {
        contour: contour.id(),
        contour_version: Some(contour.version()),
        as_of: Some(date!(2026 - 08 - 26)),
        report_currency: CurrencyCode::Rub,
        fx: iaam_core::valuation::FxTable::new(FxSource::OwnerSupplied),
        lot_rule: iaam_core::rules::LotRuleVersion(1),
    };
    let before = returns(&services, &principal(owner), &query)
        .await
        .expect("первый отчёт");
    sync_fixture_schedule(&services, instrument).await;
    let after = returns(&services, &principal(owner), &query)
        .await
        .expect("второй отчёт");

    assert_eq!(before.bond_attributes, after.bond_attributes);
    assert_eq!(before.bond_attributes.len(), 1);
    assert!(before.bond_attributes[0].accrued_interest.value().is_some());
}
