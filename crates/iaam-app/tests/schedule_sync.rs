//! Синхронизация графика: пагинация, отказ на неизвестный код, дедуп.

use std::sync::Mutex;

use async_trait::async_trait;
use iaam_app::error::AppError;
use iaam_app::ports::{OutboundHttp, OutboundResponse};
use iaam_app::scenarios::schedule::{SOURCE_ID, ScheduleSyncRequest, sync_schedule};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_http::HttpRequest;
use iaam_market::schedule::completeness::Completeness;
use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::reference::InstrumentRecord;

/// Транспорт, отдающий заготовленные страницы **по порядку обращения**.
///
/// По порядку, а не по совпадению URL, намеренно: подделка, отвечающая
/// одним и тем же телом на любой запрос, пропустила бы отсутствие
/// пагинации — сценарий сходил бы один раз и выглядел бы исправным.
struct Pages {
    bodies: Mutex<Vec<&'static str>>,
    urls: Mutex<Vec<String>>,
}

impl Pages {
    fn new(bodies: &[&'static str]) -> Self {
        Self {
            bodies: Mutex::new(bodies.to_vec()),
            urls: Mutex::new(Vec::new()),
        }
    }

    fn urls(&self) -> Vec<String> {
        self.urls.lock().expect("журнал запросов").clone()
    }
}

#[async_trait]
impl OutboundHttp for Pages {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError> {
        self.urls
            .lock()
            .expect("журнал запросов")
            .push(request.url());
        let mut bodies = self.bodies.lock().expect("страницы");
        let body = if bodies.is_empty() {
            EMPTY_PAGE
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

const EMPTY_PAGE: &str = r#"{
  "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
  "coupons": {"columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
              "data": []},
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

/// Первая страница: амортизации и оферты уже кончились, купоны — нет.
/// Ровно та форма, на которой остановка по пустому блоку обрезает график.
const PAGE_ONE: &str = r#"{
  "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2026-08-15", "2026-02-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

const PAGE_TWO: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "maturity"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

const PAGE_WITH_UNKNOWN_KIND: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "досрочное погашение"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

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

fn request(instrument: InstrumentId) -> ScheduleSyncRequest {
    ScheduleSyncRequest {
        instrument,
        secid: "SU46020RMFS2".to_owned(),
    }
}

#[tokio::test]
async fn pagination_continues_while_any_block_still_returns_rows() {
    // Смещение общее на три блока: на второй странице амортизации и
    // оферты пусты, купоны продолжаются. Остановка по пустому блоку
    // обрезала бы график, и он выглядел бы замкнутым.
    let (mut store, instrument) = store();
    let transport = Pages::new(&[PAGE_ONE, PAGE_TWO]);
    let result = sync_schedule(&mut store, &transport, request(instrument))
        .await
        .expect("синхронизация");

    let urls = transport.urls();
    assert_eq!(
        urls.len(),
        3,
        "две страницы с данными и одна пустая: {urls:?}"
    );
    assert!(urls[0].contains("start=0"), "{urls:?}");
    assert!(urls[1].contains("start=100"), "{urls:?}");
    assert_eq!(result.pages_seen, vec![0, 100, 200]);

    let stored = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            SOURCE_ID,
            "2100-01-01T00:00:00Z",
        )
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(stored.coupon_periods.len(), 2, "купоны обеих страниц");
    assert_eq!(result.completeness, Completeness::Validated);
}

#[tokio::test]
async fn an_unknown_source_code_is_refused_by_name() {
    // Пропуск строки с незнакомым кодом молча укоротил бы график.
    // Отказ обязан назвать код, иначе владельцу нечего вносить в словарь.
    let (mut store, instrument) = store();
    let transport = Pages::new(&[PAGE_WITH_UNKNOWN_KIND]);
    let error = sync_schedule(&mut store, &transport, request(instrument))
        .await
        .expect_err("неизвестный код обязан быть отказом");
    assert!(
        error.to_string().contains("досрочное погашение"),
        "отказ обязан назвать код: {error}"
    );
}

#[tokio::test]
async fn a_second_run_over_an_unchanged_schedule_writes_no_new_snapshot() {
    let (mut store, instrument) = store();
    let first = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_ONE, PAGE_TWO]),
        request(instrument),
    )
    .await
    .expect("первый прогон");
    let second = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_ONE, PAGE_TWO]),
        request(instrument),
    )
    .await
    .expect("второй прогон");
    assert!(first.written);
    assert!(!second.written, "неизменный график писаться не должен");
    assert_eq!(first.snapshot_id, second.snapshot_id);
}

#[tokio::test]
async fn a_broken_invariant_does_not_cancel_the_snapshot() {
    // Снимок — то, что источник действительно прислал. Стереть его
    // значит потерять свидетельство. Отменяется пригодность к расчёту,
    // а не запись наблюдения.
    let (mut store, instrument) = store();
    let result = sync_schedule(&mut store, &Pages::new(&[PAGE_ONE]), request(instrument))
        .await
        .expect("синхронизация");
    assert!(matches!(result.completeness, Completeness::Unknown));
    assert!(result.written, "снимок обязан быть записан");
}
