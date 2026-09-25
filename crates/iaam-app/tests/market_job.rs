//! The daily market job keeps its pace when its source fails: the day's
//! attempt was made, and the next one is tomorrow, not the scheduler's next
//! minute.
//!
//! The source is a fake port, so nothing here sleeps or touches the network.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::jobs::MarketSyncJob;
use iaam_app::ports::{Clock, OutboundHttp, OutboundResponse};
use iaam_app::sync::MarketSource;
use iaam_http::HttpRequest;
use iaam_store::SqliteStore;
use time::macros::{date, datetime};

struct FixedClock(time::Date);

impl Clock for FixedClock {
    fn today(&self) -> time::Date {
        self.0
    }
}

/// A source that answers every request with the same refusal and counts
/// the requests it was sent.
struct FailingSource {
    refusal: fn() -> AppError,
    sent: Arc<AtomicUsize>,
}

#[async_trait]
impl OutboundHttp for FailingSource {
    async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        Err((self.refusal)())
    }
}

fn unreachable() -> AppError {
    AppError::SourceUnreachable {
        origin: "cbr".to_owned(),
        detail: "failed 5 attempts".to_owned(),
        retry_after: Some(Duration::from_secs(30)),
    }
}

fn refused() -> AppError {
    AppError::SourceRefused {
        origin: "cbr".to_owned(),
        detail: "status 404".to_owned(),
    }
}

fn job(refusal: fn() -> AppError) -> (MarketSyncJob, Arc<AtomicUsize>) {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().expect("application database"),
    ));
    let mut services = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        Arc::new(FixedClock(date!(2026 - 08 - 26))),
    );
    let sent = Arc::new(AtomicUsize::new(0));
    services.http = Arc::new(FailingSource {
        refusal,
        sent: Arc::clone(&sent),
    });
    let job = MarketSyncJob::new(
        Arc::new(services),
        MarketSource::CbrKeyRate,
        date!(2026 - 01 - 15),
    );
    (job, sent)
}

async fn keeps_its_daily_pace(refusal: fn() -> AppError) {
    let (job, sent) = job(refusal);

    let first = job.run_if_due(datetime!(2026-08-26 19:00 UTC)).await;
    let a_minute_later = job.run_if_due(datetime!(2026-08-26 19:01 UTC)).await;
    let late_that_day = job.run_if_due(datetime!(2026-08-26 23:59 UTC)).await;

    assert!(
        first.is_err(),
        "the failure reaches the scheduler: {first:?}"
    );
    assert!(
        matches!(a_minute_later, Ok(None)),
        "the job ran again the same day: {a_minute_later:?}"
    );
    assert!(matches!(late_that_day, Ok(None)), "{late_that_day:?}");
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    assert_eq!(job.state().last_run(), Some(date!(2026 - 08 - 26)));

    let next_day = job.run_if_due(datetime!(2026-08-27 19:00 UTC)).await;

    assert!(next_day.is_err(), "{next_day:?}");
    assert_eq!(sent.load(Ordering::SeqCst), 2, "the next day tries again");
}

#[tokio::test]
async fn an_unreachable_source_is_tried_again_tomorrow_not_next_minute() {
    keeps_its_daily_pace(unreachable).await;
}

#[tokio::test]
async fn a_refusing_source_is_tried_again_tomorrow_not_next_minute() {
    keeps_its_daily_pace(refused).await;
}
