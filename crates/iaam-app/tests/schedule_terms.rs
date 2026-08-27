//! Синхронизация условий выпуска: незнание доходит до базы незнанием.

use std::sync::Mutex;

use async_trait::async_trait;
use iaam_app::error::AppError;
use iaam_app::ports::{OutboundHttp, OutboundResponse};
use iaam_app::scenarios::schedule::{SOURCE_ID, sync_issue_terms};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_http::HttpRequest;
use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::reference::InstrumentRecord;

const DESCRIPTION: &str = r#"{
  "description": {
    "columns": ["name", "title", "value"],
    "data": [
      ["MATDATE", "Дата погашения", "2036-02-06"],
      ["INITIALFACEVALUE", "Первоначальная номинальная стоимость", "1000"],
      ["FACEVALUE", "Номинальная стоимость", "375"],
      ["FACEUNIT", "Валюта номинала", "SUR"],
      ["COUPONFREQUENCY", "Периодичность выплаты купона в год", "2"],
      ["HASDEFAULT", "Допущен дефолт", "0"],
      ["HASTECHNICALDEFAULT", "Допущен технический дефолт", "1"]
    ]
  }
}"#;

struct Body(&'static str, Mutex<Vec<String>>);

#[async_trait]
impl OutboundHttp for Body {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError> {
        self.1.lock().expect("журнал").push(request.url());
        Ok(OutboundResponse {
            status: 200,
            body: self.0.as_bytes().to_vec(),
            raw_hash: "hash-terms".to_owned(),
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
            &[SourceCodeEntry {
                domain: "currency".to_owned(),
                source_code: "SUR".to_owned(),
                meaning: "RUB".to_owned(),
            }],
        )
        .expect("словарь заселён");
    (store, instrument)
}

#[tokio::test]
async fn unknown_day_count_reaches_the_database_as_null() {
    // Источник не даёт ни базы начисления дней, ни календаря. Значение
    // по умолчанию дало бы правдоподобно неверный НКД, которого не
    // покажет ни один тест на бумаге с целым числом периодов.
    let (mut store, instrument) = store();
    let transport = Body(DESCRIPTION, Mutex::new(Vec::new()));
    sync_issue_terms(&mut store, &transport, instrument, "SU46020RMFS2")
        .await
        .expect("синхронизация условий");

    let terms = store
        .issue_terms_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("условия найдены");
    assert_eq!(terms.day_count, None);
    assert_eq!(terms.calendar, None);
    assert_eq!(terms.effective_from, None);
}

#[tokio::test]
async fn the_source_currency_code_is_stored_verbatim() {
    // SUR здесь и RUB в графике — два кода одного источника на одну
    // валюту. Хранится код источника, переводит его словарь.
    let (mut store, instrument) = store();
    let transport = Body(DESCRIPTION, Mutex::new(Vec::new()));
    sync_issue_terms(&mut store, &transport, instrument, "SU46020RMFS2")
        .await
        .expect("синхронизация условий");
    let terms = store
        .issue_terms_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("условия найдены");
    assert_eq!(terms.face_currency_code.as_deref(), Some("SUR"));
}

#[tokio::test]
async fn both_default_flags_survive_the_trip() {
    // Объявленный дефолт делает будущий график недостоверным. Потерять
    // признак по дороге значит посчитать метрику так, будто выплаты
    // состоятся.
    let (mut store, instrument) = store();
    let transport = Body(DESCRIPTION, Mutex::new(Vec::new()));
    sync_issue_terms(&mut store, &transport, instrument, "SU46020RMFS2")
        .await
        .expect("синхронизация условий");
    let terms = store
        .issue_terms_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("условия найдены");
    assert!(!terms.default_declared);
    assert!(terms.default_technical);
}
