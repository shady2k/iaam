//! Schedule synchronisation: pagination, failure on an unknown code, deduplication.

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

/// Transport returning predefined pages **in request order**.
///
/// Request order, rather than URL matching, is deliberate: a fake returning
/// the same body for every request would mask missing
/// pagination — the scenario would make one request and appear correct.
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
        self.urls.lock().expect("request log").clone()
    }
}

#[async_trait]
impl OutboundHttp for Pages {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError> {
        self.urls.lock().expect("request log").push(request.url());
        let mut bodies = self.bodies.lock().expect("pages");
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

/// Transport whose pages never end.
///
/// Needed for the `MAX_PAGES` safeguard: a source returning rows
/// indefinitely must fail, rather than silently truncate the schedule.
struct Endless;

#[async_trait]
impl OutboundHttp for Endless {
    async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
        Ok(OutboundResponse {
            status: 200,
            body: PAGE_ONE.as_bytes().to_vec(),
            raw_hash: "hash-endless".to_owned(),
        })
    }
}

const EMPTY_PAGE: &str = r#"{
  "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
  "coupons": {"columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
              "data": []},
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

/// First page: amortisations and offers have already ended, but coupons have not.
/// Exactly the shape where stopping at an empty block truncates the schedule.
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

const PAGE_WITH_OFFER: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "maturity"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {
    "columns": ["offerdate", "offertype"],
    "data": [["2026-11-20", "Оферта"]]
  }
}"#;

const PAGE_WITH_UNKNOWN_OFFER_KIND: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "maturity"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2027-02-15", "2026-08-15", 34.41, 6.9, "RUB"]]
  },
  "offers": {
    "columns": ["offerdate", "offertype"],
    "data": [["2026-11-20", "Early redemption"]]
  }
}"#;

/// A schedule differing from `PAGE_ONE`/`PAGE_TWO`: the issuer cancelled one
/// coupon. Used to verify that a changed schedule **is written**.
const PAGE_CHANGED: &str = r#"{
  "amortizations": {
    "columns": ["amortdate", "valueprc", "data_source"],
    "data": [["2027-02-15", 100, "maturity"]]
  },
  "coupons": {
    "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
    "data": [["2027-02-15", "2026-02-15", 68.82, 6.9, "RUB"]]
  },
  "offers": {"columns": ["offerdate", "offertype"], "data": []}
}"#;

fn store() -> (SqliteStore, InstrumentId) {
    let mut store = SqliteStore::open_in_memory().expect("in-memory database");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU46020RMFS2".to_owned(),
            title: "OFZ 46020".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("instrument created");
    store
        .extend_market_source_codes(
            SOURCE_ID,
            "source profile 2026-08-27",
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
                SourceCodeEntry {
                    domain: "offer_kind".to_owned(),
                    source_code: "Оферта".to_owned(),
                    meaning: "put_option".to_owned(),
                },
            ],
        )
        .expect("dictionary populated");
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
    // The offset is shared across all three blocks: on the second page, amortisations and
    // offers are empty, while coupons continue. Stopping at an empty block
    // would truncate the schedule, making it appear complete.
    let (mut store, instrument) = store();
    let transport = Pages::new(&[PAGE_ONE, PAGE_TWO]);
    let result = sync_schedule(&mut store, &transport, request(instrument))
        .await
        .expect("synchronisation");

    let urls = transport.urls();
    assert_eq!(
        urls.len(),
        3,
        "two pages with data and one empty page: {urls:?}"
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
        .expect("read")
        .expect("snapshot found");
    assert_eq!(stored.coupon_periods.len(), 2, "coupons from both pages");
    assert_eq!(result.completeness, Completeness::Validated);
}

#[tokio::test]
async fn an_unknown_source_code_is_refused_by_name() {
    // Silently skipping a row with an unknown code would shorten the schedule.
    // The error must name the code; otherwise the owner would not know what to add to the dictionary.
    let (mut store, instrument) = store();
    let transport = Pages::new(&[PAGE_WITH_UNKNOWN_KIND]);
    let error = sync_schedule(&mut store, &transport, request(instrument))
        .await
        .expect_err("an unknown code must cause an error");
    assert!(
        error.to_string().contains("досрочное погашение"),
        "error must name the code: {error}"
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
    .expect("first run");
    let second = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_ONE, PAGE_TWO]),
        request(instrument),
    )
    .await
    .expect("second run");
    assert!(first.written);
    assert!(!second.written, "an unchanged schedule must not be written");
    assert_eq!(first.snapshot_id, second.snapshot_id);
}

#[tokio::test]
async fn a_broken_invariant_does_not_cancel_the_snapshot() {
    // The snapshot is what the source actually sent. Deleting it
    // would mean losing the evidence. Its suitability for calculation is revoked,
    // not the observation record.
    let (mut store, instrument) = store();
    let result = sync_schedule(&mut store, &Pages::new(&[PAGE_ONE]), request(instrument))
        .await
        .expect("synchronisation");
    assert!(matches!(result.completeness, Completeness::Unknown));
    assert!(result.written, "the snapshot must be written");
}

#[tokio::test]
async fn an_offer_kind_known_to_the_dictionary_passes_and_an_unknown_one_does_not() {
    // Offer type validation must reject the UNKNOWN, not the known.
    // The inverted condition would let an unknown code through and trip over
    // with a known one — both silently alter the schedule composition.
    let (mut accepting, known) = store();
    let accepted = sync_schedule(
        &mut accepting,
        &Pages::new(&[PAGE_WITH_OFFER]),
        request(known),
    )
    .await
    .expect("an offer type recognised by the dictionary must be accepted");
    assert!(accepted.written);

    let (mut fresh, other) = store();
    let error = sync_schedule(
        &mut fresh,
        &Pages::new(&[PAGE_WITH_UNKNOWN_OFFER_KIND]),
        request(other),
    )
    .await
    .expect_err("an unknown offer type must be rejected");
    assert!(
        error.to_string().contains("Early redemption"),
        "the rejection must name the code: {error}"
    );
}

#[tokio::test]
async fn a_source_that_never_runs_out_of_pages_is_refused_not_truncated() {
    // The page-count guard exists for a source that
    // returns rows indefinitely. Silently returning at the limit would be the same
    // truncation, only by our own hand — so this must be an error.
    let (mut store, instrument) = store();
    let error = sync_schedule(&mut store, &Endless, request(instrument))
        .await
        .expect_err("an infinite source must produce an error");
    assert!(
        error.to_string().contains("100"),
        "the error must name the limit: {error}"
    );
}

#[tokio::test]
async fn a_changed_schedule_is_written_as_a_new_snapshot() {
    // The other side of deduplication: if the content has changed, a new snapshot
    // must be created. A constant hash would pass the duplicate check and
    // silently bury every issuer amendment.
    let (mut store, instrument) = store();
    let first = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_ONE, PAGE_TWO]),
        request(instrument),
    )
    .await
    .expect("first run");
    let second = sync_schedule(
        &mut store,
        &Pages::new(&[PAGE_CHANGED]),
        request(instrument),
    )
    .await
    .expect("run with a changed schedule");
    assert!(second.written, "a changed schedule must be written");
    assert_ne!(first.snapshot_id, second.snapshot_id);
}
