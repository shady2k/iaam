//! ADR 0042 point 1 for market sync: the lease is taken before the network,
//! and prices, accrued interest and the run's completion are published in one
//! transaction after it.
//!
//! Each child test below is the closure the parent runs in another process: it
//! arms the barrier named by the parent, performs the sync, and is killed
//! there. A closure cannot cross a process boundary, so the same binary is
//! re-executed with `--exact <test name>` and the test function is the body.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{OutboundHttp, OutboundResponse};
use iaam_app::sync::{MarketSource, MarketSyncRequest, MarketSyncResult, sync_market};
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_http::HttpRequest;
use iaam_store::SqliteStore;
use iaam_store::fault::{self, KillAt};
use iaam_store::market::SeriesKey;
use iaam_store::reference::InstrumentRecord;
use time::macros::date;
use uuid::Uuid;

const INSTRUMENT_VARIABLE: &str = "IAAM_FAULT_TEST_INSTRUMENT";

/// The invented MOEX page every run below answers with. One row carries both a
/// price and the accrued coupon interest, which is what makes the publication
/// two writes in one transaction.
const BODY: &[u8] = include_bytes!("../../../tests/fixtures/market/moex-iss-history-ofz.json");
/// The knowledge coordinate of the invented page, opaque to the store.
const RAW_HASH: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("iaam-app-market-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temporary directory");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The source answers with the same invented page and refuses nothing, so a
/// run's only variable is where it is killed.
struct FixturePage;

#[async_trait]
impl OutboundHttp for FixturePage {
    async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
        Ok(OutboundResponse {
            status: 200,
            raw_hash: RAW_HASH.to_owned(),
            body: BODY.to_vec(),
        })
    }
}

fn request(instrument: InstrumentId) -> MarketSyncRequest {
    MarketSyncRequest {
        source: MarketSource::Moex {
            engine: "stock".to_owned(),
            market: "bonds".to_owned(),
            board: "TQOB".to_owned(),
            secid: "SU26238RMFS4".to_owned(),
            instrument,
        },
        from: date!(2026 - 08 - 20),
        to: date!(2026 - 08 - 26),
    }
}

fn series(instrument: InstrumentId) -> SeriesKey {
    SeriesKey {
        source_id: "moex-iss".to_owned(),
        dataset: "prices".to_owned(),
        series_key: format!("{}:TQOB", instrument.inner()),
    }
}

/// The instrument the rows name, which the foreign key requires.
fn seed(database: &Path, instrument: InstrumentId) {
    SqliteStore::open(database)
        .expect("create database")
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "SU26238RMFS4".to_owned(),
            title: "Invented bond".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
            metadata: None,
            blocked_tca_flag: false,
        })
        .expect("seed instrument");
}

fn child_parts() -> Option<(PathBuf, InstrumentId)> {
    let database = std::env::var_os(fault::DATABASE_VARIABLE)?;
    let instrument = std::env::var(INSTRUMENT_VARIABLE).expect("instrument configured");
    Some((
        PathBuf::from(database),
        InstrumentId(Uuid::parse_str(&instrument).expect("instrument identifier")),
    ))
}

fn run_child_sync(database: &Path, instrument: InstrumentId) {
    let store = SqliteAdapter::new(
        SqliteStore::open_existing(database).expect("the child opens the database"),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let _ = sync_market(&store, &FixturePage, request(instrument)).await;
    });
}

#[test]
fn killed_before_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, instrument) = child_parts().expect("child configuration");
    run_child_sync(&database, instrument);
}

#[test]
fn killed_after_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, instrument) = child_parts().expect("child configuration");
    run_child_sync(&database, instrument);
}

fn kill(
    directory: &TempDirectory,
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    database: &Path,
    instrument: InstrumentId,
) -> KillAt {
    let value = instrument.inner().to_string();
    fault::kill_at(
        child_test,
        point,
        after_write,
        &directory.file("barrier-reached"),
        database,
        &[(INSTRUMENT_VARIABLE, OsStr::new(&value))],
    )
}

/// What a published series means, without the run that carried it, the moment
/// it was observed, or the knowledge coordinate: the comparison the acceptance
/// asks for.
#[derive(Debug, PartialEq, Eq)]
struct Published {
    prices: Vec<String>,
    accrued: Vec<String>,
    completed: Vec<String>,
    boundary: Option<String>,
}

fn published(database: &Path, instrument: InstrumentId) -> Published {
    let store = SqliteStore::open_existing(database).expect("the database reopens");
    let connection = store.connection();
    let mut prices = Vec::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT instrument_id, board, session, trade_date, kind, price, currency,
                        quotation_basis, basis_evidence, executability
                 FROM price_observations
                 ORDER BY trade_date, kind, board, session",
            )
            .expect("price query");
        let rows = statement
            .query_map([], |row| {
                Ok(format!(
                    "{} {} {} {} {} {} {} {} {} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .expect("price rows");
        prices.extend(rows.map(|row| row.expect("price row")));
    }
    let mut accrued = Vec::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT instrument_id, board, session, trade_date, per_unit, currency
                 FROM accrued_interest_observations
                 ORDER BY trade_date, board, session",
            )
            .expect("accrued query");
        let rows = statement
            .query_map([], |row| {
                Ok(format!(
                    "{} {} {} {} {} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .expect("accrued rows");
        accrued.extend(rows.map(|row| row.expect("accrued row")));
    }
    let mut statement = connection
        .prepare(
            "SELECT status, covered_from, covered_to FROM sync_runs
             WHERE status = 'succeeded'
             ORDER BY covered_from, covered_to",
        )
        .expect("run query");
    let completed = statement
        .query_map([], |row| {
            Ok(format!(
                "{} {} {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .expect("run rows")
        .map(|row| row.expect("run row"))
        .collect();
    Published {
        prices,
        accrued,
        completed,
        boundary: store
            .complete_through(&series(instrument))
            .expect("boundary")
            .map(|day| day.to_string()),
    }
}

/// The figures a reader sees: the latest published observation per identity,
/// which is what a later repeat sync must leave as it was.
fn figures(database: &Path, instrument: InstrumentId) -> Vec<String> {
    let store = SqliteStore::open_existing(database).expect("the database reopens");
    let mut figures = Vec::new();
    for (table, cells) in [
        (
            "price_observations",
            "board || ' ' || session || ' ' || trade_date || ' ' || kind || ' ' || price || ' ' || \
             currency || ' ' || quotation_basis || ' ' || executability",
        ),
        (
            "accrued_interest_observations",
            "board || ' ' || session || ' ' || trade_date || ' ' || per_unit || ' ' || currency",
        ),
    ] {
        let sql = format!(
            "SELECT {cells} FROM {table} AS o
             JOIN sync_runs AS r ON r.id = o.sync_run_id
             WHERE r.status = 'succeeded'
               AND o.observed_at = (
                   SELECT MAX(i.observed_at) FROM {table} AS i
                   JOIN sync_runs AS s ON s.id = i.sync_run_id
                   WHERE s.status = 'succeeded'
                     AND i.instrument_id = o.instrument_id
                     AND i.board = o.board
                     AND i.session = o.session
                     AND i.trade_date = o.trade_date)
             ORDER BY o.trade_date, o.board, o.session"
        );
        let mut statement = store.connection().prepare(&sql).expect("figure query");
        figures.extend(
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .expect("figure rows")
                .map(|row| format!("{table} {}", row.expect("figure row"))),
        );
    }
    figures.push(format!(
        "boundary {:?}",
        store
            .complete_through(&series(instrument))
            .expect("boundary")
    ));
    figures
}

async fn sync(database: &Path, instrument: InstrumentId) -> Result<MarketSyncResult, AppError> {
    let store =
        SqliteAdapter::new(SqliteStore::open_existing(database).expect("the database reopens"));
    sync_market(&store, &FixturePage, request(instrument)).await
}

/// A clean run of the same request on its own database: what the interrupted
/// attempt is compared against.
async fn clean_run(directory: &TempDirectory, instrument: InstrumentId) -> Published {
    let database = directory.file("clean.sqlite");
    seed(&database, instrument);
    sync(&database, instrument)
        .await
        .expect("the clean run publishes");
    published(&database, instrument)
}

#[tokio::test]
async fn a_kill_inside_publication_leaves_nothing_and_a_rerun_publishes_everything() {
    let directory = TempDirectory::new("before-commit");
    let database = directory.file("iaam.sqlite");
    let instrument = InstrumentId::new_random();
    seed(&database, instrument);

    // The publication's second write, not its first: the page carries one
    // price row and one accrued-interest row, and a store that committed each
    // write on its own would leave the first behind.
    let KillAt { child, .. } = kill(
        &directory,
        "killed_before_commit_child",
        fault::BEFORE_COMMIT,
        Some(2),
        &database,
        instrument,
    );
    assert!(
        child.marker.exists(),
        "the child was killed inside the publication transaction, after its second write"
    );
    let interrupted = published(&database, instrument);
    assert_eq!(
        interrupted.prices,
        Vec::<String>::new(),
        "a kill inside the transaction leaves no price, although one was written"
    );
    assert_eq!(
        interrupted.accrued,
        Vec::<String>::new(),
        "and no accrued interest"
    );
    assert_eq!(
        interrupted.completed,
        Vec::<String>::new(),
        "and no completed run"
    );
    assert_eq!(interrupted.boundary, None, "and no completeness boundary");

    // The interrupted run still holds the series' lease, which is the owner's
    // decision: the lease is the one write before the network. It is released
    // by expiry, and until then the series is not synced again — and the
    // refusal says so in terms of when, not of a store that failed.
    let refused = sync(&database, instrument).await;
    match refused {
        Err(AppError::SyncInProgress {
            series,
            retry_after,
        }) => {
            assert_eq!(
                series,
                format!("moex-iss/prices/{}:TQOB", instrument.inner()),
                "the refusal names the series whose lease is held"
            );
            assert!(
                retry_after > std::time::Duration::ZERO
                    && retry_after <= std::time::Duration::from_secs(600),
                "the wait is what is left of the ten-minute lease: {retry_after:?}"
            );
        }
        other => panic!("expected the lease refusal, got {other:?}"),
    }

    // Expire the lease, as the next start of the day would.
    {
        let store = SqliteStore::open_existing(&database).expect("the database reopens");
        store
            .connection()
            .execute(
                "UPDATE sync_runs SET lease_expires_at = '2000-01-01T00:00:00Z'
                 WHERE status = 'running'",
                [],
            )
            .expect("expire the lease");
    }

    let retried = sync(&database, instrument)
        .await
        .expect("the retry publishes");
    assert_eq!(retried.status(), "succeeded");
    assert_eq!(
        published(&database, instrument),
        clean_run(&directory, instrument).await,
        "the retry publishes what a clean run publishes, by meaning"
    );
}

#[tokio::test]
async fn a_kill_after_commit_leaves_the_publication_and_a_rerun_changes_nothing() {
    let directory = TempDirectory::new("after-commit");
    let database = directory.file("iaam.sqlite");
    let instrument = InstrumentId::new_random();
    seed(&database, instrument);

    let KillAt { child, .. } = kill(
        &directory,
        "killed_after_commit_child",
        fault::AFTER_COMMIT,
        None,
        &database,
        instrument,
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the commit"
    );
    let committed = published(&database, instrument);
    assert!(
        !committed.prices.is_empty(),
        "a kill after COMMIT leaves the prices"
    );
    assert!(!committed.accrued.is_empty(), "and the accrued interest");
    assert_eq!(
        committed.completed.len(),
        1,
        "and the completed run: {committed:?}"
    );
    assert_eq!(committed.boundary.as_deref(), Some("2026-08-26"));
    assert_eq!(
        committed,
        clean_run(&directory, instrument).await,
        "the killed run leaves what a clean run leaves, by meaning"
    );

    let figures_before = figures(&database, instrument);
    let repeated = sync(&database, instrument)
        .await
        .expect("the repeat publishes");
    assert_eq!(repeated.status(), "succeeded");
    assert_eq!(
        figures(&database, instrument),
        figures_before,
        "the repeat leaves the figures and the boundary as they were"
    );
    assert_eq!(
        published(&database, instrument).boundary,
        committed.boundary,
        "the completed run's boundary stands"
    );
}
