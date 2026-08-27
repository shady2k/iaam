//! Метаморфное свойство: повторная синхронизация ничего не меняет.

use std::sync::Mutex;

use async_trait::async_trait;
use iaam_app::error::AppError;
use iaam_app::ports::{OutboundHttp, OutboundResponse};
use iaam_app::scenarios::schedule::{SOURCE_ID, ScheduleSyncRequest, sync_schedule};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_http::HttpRequest;
use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::reference::InstrumentRecord;

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
