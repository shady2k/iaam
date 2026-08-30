//! Invariants for publishing and reproducibly reading market time series.

use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_store::market::{
    AccruedInterestRow, Coverage, FxRow, KeyRateRow, PriceRow, PriceVenue, RunOutcome, SeriesKey,
};
use iaam_store::reference::InstrumentRecord;
use iaam_store::{SqliteStore, StoreError};
use time::macros::{date, datetime};
use time::{Duration, OffsetDateTime};

fn store_with_instrument() -> (SqliteStore, InstrumentId) {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".to_owned(),
            title: "Sberbank".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("instrument created");
    (store, instrument)
}

fn series(name: &str) -> SeriesKey {
    series_with_dataset("prices", name)
}

fn series_with_dataset(dataset: &str, name: &str) -> SeriesKey {
    SeriesKey {
        source_id: "moex-iss".to_owned(),
        dataset: dataset.to_owned(),
        series_key: name.to_owned(),
    }
}

fn price(instrument: InstrumentId, observed_at: &str, value: &str) -> PriceRow {
    PriceRow {
        instrument_id: instrument.inner().to_string(),
        board: "TQBR".to_owned(),
        session: 1,
        trade_date: "2026-08-03".to_owned(),
        kind: "close".to_owned(),
        observed_at: observed_at.to_owned(),
        price: value.to_owned(),
        currency: "RUB".to_owned(),
        executability: "executable".to_owned(),
        quotation_basis: "unknown".to_owned(),
        basis_evidence: String::new(),
    }
}

fn bond_price(
    instrument: InstrumentId,
    observed_at: &str,
    value: &str,
    quotation_basis: &str,
    basis_evidence: &str,
) -> PriceRow {
    PriceRow {
        instrument_id: instrument.inner().to_string(),
        board: "TQBR".to_owned(),
        session: 1,
        trade_date: "2026-08-03".to_owned(),
        kind: "close".to_owned(),
        observed_at: observed_at.to_owned(),
        price: value.to_owned(),
        currency: "RUB".to_owned(),
        quotation_basis: quotation_basis.to_owned(),
        basis_evidence: basis_evidence.to_owned(),
        executability: "executable".to_owned(),
    }
}

fn fx(observed_at: &str, value: &str) -> FxRow {
    FxRow {
        from_code: "USD".to_owned(),
        to_code: "RUB".to_owned(),
        trade_date: "2026-08-03".to_owned(),
        observed_at: observed_at.to_owned(),
        nominal: 1,
        value: value.to_owned(),
        unit_rate: value.to_owned(),
    }
}

fn key_rate(observed_at: &str, rate: &str) -> KeyRateRow {
    KeyRateRow {
        trade_date: "2026-08-03".to_owned(),
        observed_at: observed_at.to_owned(),
        rate: rate.to_owned(),
    }
}

/// A lease that is guaranteed to be valid at the time of the run.
///
/// An absolute point in time here would be a ticking time bomb:
/// `begin_run` refuses when `lease_expires_at <= now_utc()`, and the recorded
/// date eventually arrives—the entire file fails with `LeaseExpired` without a single
/// code change. This has already happened (iaam-816).
fn lease() -> OffsetDateTime {
    OffsetDateTime::now_utc() + Duration::hours(1)
}

#[test]
fn record_fx_reports_exact_number_of_rows_inserted() {
    let (mut store, _) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("fx", "USD/RUB"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("rates run started");
    let rows = [
        fx("2026-08-03T09:00:00Z", "80"),
        fx("2026-08-03T10:00:00Z", "81"),
    ];

    let inserted = store
        .record_fx(&run, "raw-fx", &rows)
        .expect("rates recorded");

    assert_eq!(inserted, 2);
}

#[test]
fn record_key_rate_reports_exact_number_of_rows_inserted() {
    let (mut store, _) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("key-rate", "CBR"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("key rate run started");
    let rows = [
        key_rate("2026-08-03T09:00:00Z", "18"),
        key_rate("2026-08-03T10:00:00Z", "17.5"),
    ];

    let inserted = store
        .record_key_rate(&run, "raw-key-rate", &rows)
        .expect("key rate recorded");

    assert_eq!(inserted, 2);
}

#[test]
fn a_foreign_lease_token_is_refused_for_a_fresh_run() {
    let (mut store, _) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("fx", "USD/RUB"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("fresh lease");
    let mut foreign = run.clone();
    foreign.lease_token = "foreign-token".to_owned();

    let result = store.record_fx(&foreign, "raw-foreign", &[fx("2026-08-03T09:00:00Z", "80")]);

    assert!(matches!(result, Err(StoreError::RunNotFound)));
    store
        .finish_run(
            &run,
            RunOutcome::Failed {
                reason: "cleanup".to_owned(),
            },
            None,
        )
        .expect("original lease remained active");
}

#[test]
fn a_dataset_mismatch_is_refused_even_when_the_other_run_identity_fields_match() {
    let (mut store, _) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("fx", "USD/RUB"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("rates run started");
    let mut wrong_dataset = run.clone();
    wrong_dataset.series.dataset = "other-dataset".to_owned();

    let result = store.record_fx(
        &wrong_dataset,
        "raw-wrong-dataset",
        &[fx("2026-08-03T09:00:00Z", "80")],
    );

    assert!(matches!(result, Err(StoreError::RunNotFound)));
    store
        .finish_run(
            &run,
            RunOutcome::Failed {
                reason: "cleanup".to_owned(),
            },
            None,
        )
        .expect("original run remained unfinished");
}

#[test]
fn a_series_key_mismatch_is_refused_even_when_the_other_run_identity_fields_match() {
    let (mut store, _) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("fx", "USD/RUB"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("rates run started");
    let mut wrong_series = run.clone();
    wrong_series.series.series_key = "EUR/RUB".to_owned();

    let result = store.record_fx(
        &wrong_series,
        "raw-wrong-series",
        &[fx("2026-08-03T09:00:00Z", "80")],
    );

    assert!(matches!(result, Err(StoreError::RunNotFound)));
    store
        .finish_run(
            &run,
            RunOutcome::Failed {
                reason: "cleanup".to_owned(),
            },
            None,
        )
        .expect("original run remained unfinished");
}

#[test]
fn an_expired_lease_is_refused_by_recording() {
    let (mut store, _) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("fx", "USD/RUB"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            OffsetDateTime::now_utc() + Duration::hours(1),
        )
        .expect("active lease");
    store
        .connection()
        .execute(
            "UPDATE sync_runs SET lease_expires_at = '2026-08-01T00:00:00Z' WHERE id = ?1",
            [&run.id],
        )
        .expect("test overdue");

    let result = store.record_fx(&run, "raw-expired", &[fx("2026-08-03T09:00:00Z", "80")]);

    assert!(matches!(result, Err(StoreError::LeaseExpired)));
}

#[test]
fn a_missing_lease_is_refused_by_recording() {
    let (mut store, _) = store_with_instrument();
    let run = store
        .begin_run(
            series_with_dataset("fx", "USD/RUB"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            OffsetDateTime::now_utc() + Duration::hours(1),
        )
        .expect("active rental");
    store
        .connection()
        .execute(
            "UPDATE sync_runs SET lease_expires_at = NULL WHERE id = ?1",
            [&run.id],
        )
        .expect("test rental deleted");

    let result = store.record_fx(&run, "raw-missing-lease", &[]);

    assert!(matches!(result, Err(StoreError::LeaseExpired)));
}

#[test]
fn a_corrected_price_lands_beside_the_old_one_not_over_it() {
    let (mut store, instrument) = store_with_instrument();
    let first = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("first run");
    store
        .record_prices(
            &first,
            "raw-first",
            &[price(instrument, "2026-08-03T09:00:00Z", "100")],
        )
        .expect("first price");
    store
        .finish_run(
            &first,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("first run published");

    let correction = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("corrective run");
    store
        .record_prices(
            &correction,
            "raw-correction",
            &[price(instrument, "2026-08-03T10:00:00Z", "101")],
        )
        .expect("corrected price");
    store
        .finish_run(&correction, RunOutcome::Succeeded, None)
        .expect("corrective run published");

    let venue = PriceVenue {
        board: "TQBR".to_owned(),
        session: 1,
    };
    let before_correction = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &venue,
            "2026-08-03",
            "2026-08-03T09:30:00Z",
        )
        .expect("read before correction")
        .expect("old price exists");
    let after_correction = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &venue,
            "2026-08-03",
            "2026-08-03T11:00:00Z",
        )
        .expect("read after correction")
        .expect("new price exists");

    assert_eq!(before_correction.price, "100");
    assert_eq!(after_correction.price, "101");
    let rows: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM price_observations", [], |row| {
            row.get(0)
        })
        .expect("row count");
    assert_eq!(rows, 2, "correction added alongside the old observation");
}

#[test]
fn a_partial_run_does_not_advance_the_completeness_boundary() {
    let (mut store, _) = store_with_instrument();
    let complete = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("full run");
    store
        .finish_run(
            &complete,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("full run published");

    let partial = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 04),
            date!(2026 - 08 - 10),
            lease(),
        )
        .expect("partial run");
    store
        .finish_run(
            &partial,
            RunOutcome::Partial {
                reason: "page 8 of 10 is unavailable".to_owned(),
            },
            Some(Coverage {
                from: date!(2026 - 08 - 04),
                to: date!(2026 - 08 - 08),
            }),
        )
        .expect("partial run recorded");

    assert_eq!(
        store
            .complete_through(&series("SBER:TQBR:1"))
            .expect("completeness boundary"),
        Some(date!(2026 - 08 - 03))
    );
}

#[test]
fn a_failed_series_does_not_hold_back_other_series() {
    let (mut store, _) = store_with_instrument();
    let failed = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("failed series");
    store
        .finish_run(
            &failed,
            RunOutcome::Failed {
                reason: "MOEX unavailable".to_owned(),
            },
            None,
        )
        .expect("failure recorded");

    let other = store
        .begin_run(
            series("GAZP:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("other series not blocked");
    store
        .finish_run(
            &other,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("other series published");

    assert_eq!(
        store
            .complete_through(&series("SBER:TQBR:1"))
            .expect("first series boundary"),
        None
    );
    assert_eq!(
        store
            .complete_through(&series("GAZP:TQBR:1"))
            .expect("second series boundary"),
        Some(date!(2026 - 08 - 03))
    );
}

#[test]
fn rows_of_an_unfinished_run_are_invisible_to_reads() {
    let (mut store, instrument) = store_with_instrument();
    let run = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("start");
    store
        .record_prices(
            &run,
            "raw",
            &[price(instrument, "2026-08-03T09:00:00Z", "100")],
        )
        .expect("unfinished row");

    let found = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &PriceVenue {
                board: "TQBR".to_owned(),
                session: 1,
            },
            "2026-08-03",
            "2026-08-03T12:00:00Z",
        )
        .expect("read");
    assert!(found.is_none(), "running is not published on read");
}

#[test]
fn accrued_interest_is_invisible_before_its_knowledge_coordinate() {
    // An observation recorded after the coordinate must be invisible:
    // otherwise the “as of yesterday” report will be recalculated from tomorrow’s knowledge.
    let (mut store, instrument) = store_with_instrument();
    let run = store
        .begin_run(
            series("SBER:TQOB:3"),
            date!(2026 - 08 - 20),
            date!(2026 - 08 - 20),
            lease(),
        )
        .unwrap();
    store
        .record_accrued_interest(
            &run,
            "hash",
            &[AccruedInterestRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQOB".to_owned(),
                session: 3,
                trade_date: "2026-08-20".to_owned(),
                observed_at: "2026-08-27T12:00:00Z".to_owned(),
                per_unit: "15.17".to_owned(),
                currency: "RUB".to_owned(),
            }],
        )
        .unwrap();
    store.finish_run(&run, RunOutcome::Succeeded, None).unwrap();

    let venue = PriceVenue {
        board: "TQOB".to_owned(),
        session: 3,
    };
    assert!(
        store
            .accrued_interest_at_or_before(
                &instrument.inner().to_string(),
                &venue,
                "2026-08-20",
                "2026-08-26T00:00:00Z",
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .accrued_interest_at_or_before(
                &instrument.inner().to_string(),
                &venue,
                "2026-08-20",
                "2026-08-27T12:00:00Z",
            )
            .unwrap()
            .map(|row| row.per_unit),
        Some("15.17".to_owned())
    );
}

#[test]
fn a_second_run_on_the_same_series_is_refused_while_the_lease_holds() {
    let (mut store, _) = store_with_instrument();
    let _first = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("first lease");
    let refused = store.begin_run(
        series("SBER:TQBR:1"),
        date!(2026 - 08 - 03),
        date!(2026 - 08 - 03),
        lease(),
    );

    assert!(matches!(refused, Err(StoreError::LeaseHeld { .. })));
}

#[test]
fn a_read_at_an_earlier_knowledge_time_returns_the_earlier_value() {
    let (mut store, instrument) = store_with_instrument();
    let first = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("first run");
    store
        .record_prices(
            &first,
            "raw-1",
            &[price(instrument, "2026-08-03T09:00:00Z", "100")],
        )
        .expect("first observation");
    store
        .finish_run(&first, RunOutcome::Succeeded, None)
        .expect("first observation published");

    let second = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("second run");
    store
        .record_prices(
            &second,
            "raw-2",
            &[price(instrument, "2026-08-03T10:00:00Z", "101")],
        )
        .expect("corrected observation");
    store
        .finish_run(&second, RunOutcome::Succeeded, None)
        .expect("second observation published");

    let value = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &PriceVenue {
                board: "TQBR".to_owned(),
                session: 1,
            },
            "2026-08-03",
            "2026-08-03T09:30:00Z",
        )
        .expect("reading at the early moment")
        .expect("early price");
    assert_eq!(value.price, "100");

    let later = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &PriceVenue {
                board: "TQBR".to_owned(),
                session: 1,
            },
            "2026-08-03",
            "2026-08-03T11:00:00Z",
        )
        .expect("reading at the late moment")
        .expect("late price");
    assert_eq!(later.price, "101");
}

#[test]
fn an_expired_lease_is_replaced_and_old_token_cannot_finish() {
    let (mut store, _) = store_with_instrument();
    let expired = store.begin_run(
        series("SBER:TQBR:1"),
        date!(2026 - 08 - 03),
        date!(2026 - 08 - 03),
        datetime!(2026-08-01 00:00:00 UTC),
    );
    assert!(matches!(expired, Err(StoreError::LeaseExpired)));

    let old = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            OffsetDateTime::now_utc() + Duration::hours(1),
        )
        .expect("active lease");
    store
        .connection()
        .execute(
            "UPDATE sync_runs SET lease_expires_at = '2026-08-01T00:00:00Z' WHERE id = ?1",
            [&old.id],
        )
        .expect("test expiration");
    let replacement = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            OffsetDateTime::now_utc() + Duration::hours(1),
        )
        .expect("expired lease released");
    assert!(matches!(
        store.finish_run(&old, RunOutcome::Succeeded, None),
        Err(StoreError::RunNotFound)
    ));
    store
        .finish_run(&replacement, RunOutcome::Succeeded, None)
        .expect("new run completed");
}

#[test]
fn price_uses_latest_trade_date_then_knowledge() {
    let (mut store, instrument) = store_with_instrument();
    let old_day = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 20),
            date!(2026 - 08 - 20),
            lease(),
        )
        .expect("starting the old trading day");
    store
        .record_prices(
            &old_day,
            "raw-old-day",
            &[PriceRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQBR".to_owned(),
                session: 1,
                trade_date: "2026-08-20".to_owned(),
                kind: "close".to_owned(),
                observed_at: "2026-08-28T09:00:00Z".to_owned(),
                price: "100".to_owned(),
                currency: "RUB".to_owned(),
                quotation_basis: "unknown".to_owned(),
                basis_evidence: String::new(),
                executability: "executable".to_owned(),
            }],
        )
        .expect("old price");
    store
        .finish_run(&old_day, RunOutcome::Succeeded, None)
        .expect("old day published");

    let recent_day = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 26),
            date!(2026 - 08 - 26),
            lease(),
        )
        .expect("starting a fresh trading day");
    store
        .record_prices(
            &recent_day,
            "raw-recent-day",
            &[PriceRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQBR".to_owned(),
                session: 1,
                trade_date: "2026-08-26".to_owned(),
                kind: "close".to_owned(),
                observed_at: "2026-08-27T09:00:00Z".to_owned(),
                price: "101".to_owned(),
                currency: "RUB".to_owned(),
                quotation_basis: "unknown".to_owned(),
                basis_evidence: String::new(),
                executability: "executable".to_owned(),
            }],
        )
        .expect("fresh price");
    store
        .finish_run(&recent_day, RunOutcome::Succeeded, None)
        .expect("fresh day published");

    let value = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &PriceVenue {
                board: "TQBR".to_owned(),
                session: 1,
            },
            "2026-08-26",
            "2026-08-29T00:00:00Z",
        )
        .expect("reading price")
        .expect("price found");
    assert_eq!(value.trade_date, "2026-08-26");
    assert_eq!(value.price, "101");
}

#[test]
fn accrued_interest_uses_latest_trade_date_then_knowledge() {
    let (mut store, instrument) = store_with_instrument();
    let old_day = store
        .begin_run(
            series("SBER:TQOB:3"),
            date!(2026 - 08 - 20),
            date!(2026 - 08 - 20),
            lease(),
        )
        .expect("starting the old trading day");
    store
        .record_accrued_interest(
            &old_day,
            "raw-old-day",
            &[AccruedInterestRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQOB".to_owned(),
                session: 3,
                trade_date: "2026-08-20".to_owned(),
                observed_at: "2026-08-28T09:00:00Z".to_owned(),
                per_unit: "15.00".to_owned(),
                currency: "RUB".to_owned(),
            }],
        )
        .expect("old accrued interest");
    store
        .finish_run(&old_day, RunOutcome::Succeeded, None)
        .expect("old accrued interest published");

    let recent_day = store
        .begin_run(
            series("SBER:TQOB:3"),
            date!(2026 - 08 - 26),
            date!(2026 - 08 - 26),
            lease(),
        )
        .expect("start a fresh trading day");
    store
        .record_accrued_interest(
            &recent_day,
            "raw-recent-day",
            &[AccruedInterestRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQOB".to_owned(),
                session: 3,
                trade_date: "2026-08-26".to_owned(),
                observed_at: "2026-08-27T09:00:00Z".to_owned(),
                per_unit: "16.00".to_owned(),
                currency: "RUB".to_owned(),
            }],
        )
        .expect("fresh accrued interest");
    store
        .finish_run(&recent_day, RunOutcome::Succeeded, None)
        .expect("fresh accrued interest published");

    let value = store
        .accrued_interest_at_or_before(
            &instrument.inner().to_string(),
            &PriceVenue {
                board: "TQOB".to_owned(),
                session: 3,
            },
            "2026-08-26",
            "2026-08-29T00:00:00Z",
        )
        .expect("read accrued interest")
        .expect("accrued interest found");
    assert_eq!(value.trade_date, "2026-08-26");
    assert_eq!(value.per_unit, "16.00");
}

#[test]
fn same_day_observation_wins_for_price_and_accrued_interest() {
    let (mut store, instrument) = store_with_instrument();
    let first_price = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 26),
            date!(2026 - 08 - 26),
            lease(),
        )
        .expect("first price run");
    store
        .record_prices(
            &first_price,
            "raw-price-1",
            &[PriceRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQBR".to_owned(),
                session: 1,
                trade_date: "2026-08-26".to_owned(),
                kind: "close".to_owned(),
                observed_at: "2026-08-27T09:00:00Z".to_owned(),
                price: "101".to_owned(),
                currency: "RUB".to_owned(),
                quotation_basis: "unknown".to_owned(),
                basis_evidence: String::new(),
                executability: "executable".to_owned(),
            }],
        )
        .expect("first price");
    store
        .finish_run(&first_price, RunOutcome::Succeeded, None)
        .unwrap();

    let second_price = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 26),
            date!(2026 - 08 - 26),
            lease(),
        )
        .expect("second price run");
    store
        .record_prices(
            &second_price,
            "raw-price-2",
            &[PriceRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQBR".to_owned(),
                session: 1,
                trade_date: "2026-08-26".to_owned(),
                kind: "close".to_owned(),
                observed_at: "2026-08-28T09:00:00Z".to_owned(),
                price: "102".to_owned(),
                currency: "RUB".to_owned(),
                quotation_basis: "unknown".to_owned(),
                basis_evidence: String::new(),
                executability: "executable".to_owned(),
            }],
        )
        .expect("price refinement");
    store
        .finish_run(&second_price, RunOutcome::Succeeded, None)
        .unwrap();

    let first_interest = store
        .begin_run(
            series("SBER:TQOB:3"),
            date!(2026 - 08 - 26),
            date!(2026 - 08 - 26),
            lease(),
        )
        .expect("first accrued interest run");
    store
        .record_accrued_interest(
            &first_interest,
            "raw-interest-1",
            &[AccruedInterestRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQOB".to_owned(),
                session: 3,
                trade_date: "2026-08-26".to_owned(),
                observed_at: "2026-08-27T09:00:00Z".to_owned(),
                per_unit: "16.00".to_owned(),
                currency: "RUB".to_owned(),
            }],
        )
        .expect("first accrued interest");
    store
        .finish_run(&first_interest, RunOutcome::Succeeded, None)
        .unwrap();

    let second_interest = store
        .begin_run(
            series("SBER:TQOB:3"),
            date!(2026 - 08 - 26),
            date!(2026 - 08 - 26),
            lease(),
        )
        .expect("second accrued interest run");
    store
        .record_accrued_interest(
            &second_interest,
            "raw-interest-2",
            &[AccruedInterestRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQOB".to_owned(),
                session: 3,
                trade_date: "2026-08-26".to_owned(),
                observed_at: "2026-08-28T09:00:00Z".to_owned(),
                per_unit: "17.00".to_owned(),
                currency: "RUB".to_owned(),
            }],
        )
        .expect("accrued interest refinement");
    store
        .finish_run(&second_interest, RunOutcome::Succeeded, None)
        .unwrap();

    let venue = PriceVenue {
        board: "TQBR".to_owned(),
        session: 1,
    };
    let price = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &venue,
            "2026-08-26",
            "2026-08-29T00:00:00Z",
        )
        .unwrap()
        .unwrap();
    assert_eq!(price.price, "102");

    let interest = store
        .accrued_interest_at_or_before(
            &instrument.inner().to_string(),
            &PriceVenue {
                board: "TQOB".to_owned(),
                session: 3,
            },
            "2026-08-26",
            "2026-08-29T00:00:00Z",
        )
        .unwrap()
        .unwrap();
    assert_eq!(interest.per_unit, "17.00");
}

#[test]
fn observation_after_knowledge_coordinate_is_hidden() {
    let (mut store, instrument) = store_with_instrument();
    let run = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 26),
            date!(2026 - 08 - 26),
            lease(),
        )
        .expect("start");
    store
        .record_prices(
            &run,
            "raw-future",
            &[PriceRow {
                instrument_id: instrument.inner().to_string(),
                board: "TQBR".to_owned(),
                session: 1,
                trade_date: "2026-08-26".to_owned(),
                kind: "close".to_owned(),
                observed_at: "2026-08-29T09:00:00Z".to_owned(),
                price: "103".to_owned(),
                currency: "RUB".to_owned(),
                quotation_basis: "unknown".to_owned(),
                basis_evidence: String::new(),
                executability: "executable".to_owned(),
            }],
        )
        .expect("future price");
    store.finish_run(&run, RunOutcome::Succeeded, None).unwrap();

    assert!(
        store
            .prices_at_or_before(
                &instrument.inner().to_string(),
                &PriceVenue {
                    board: "TQBR".to_owned(),
                    session: 1,
                },
                "2026-08-26",
                "2026-08-28T00:00:00Z",
            )
            .unwrap()
            .is_none(),
        "knowledge from the future is not published"
    );
}

#[test]
fn the_quotation_basis_survives_a_round_trip_through_every_read_path() {
    // A basis lost on one of the read paths is detected
    // not by rejection, but by the position value being understated by a factor of par/100.
    let (mut store, instrument) = store_with_instrument();
    let series = series_with_dataset("moex", "SBER");
    let run = store
        .begin_run(
            series.clone(),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("start prices");
    let row = bond_price(
        instrument,
        "2026-08-03T19:00:00Z",
        "98.5",
        "percent_of_remaining_face",
        "iss:engines/stock/markets/bonds",
    );
    store.record_prices(&run, "raw-basis", &[row]).unwrap();
    store.finish_run(&run, RunOutcome::Succeeded, None).unwrap();

    let venue = PriceVenue {
        board: "TQBR".to_owned(),
        session: 1,
    };
    let window = iaam_store::market::MarketWindow {
        from: "2026-08-03",
        to: "2026-08-03",
        knowledge_as_of: "2026-08-04T00:00:00Z",
    };
    let instrument_id = instrument.inner().to_string();
    let assert_basis = |row: &PriceRow| {
        assert_eq!(row.quotation_basis, "percent_of_remaining_face");
        assert_eq!(row.basis_evidence, "iss:engines/stock/markets/bonds");
    };

    let at_or_before = store
        .prices_at_or_before(&instrument_id, &venue, "2026-08-03", "2026-08-04T00:00:00Z")
        .unwrap()
        .unwrap();
    assert_basis(&at_or_before);

    let by_instrument = store
        .prices_for_instrument_between("moex-iss", "moex", &instrument_id, window)
        .unwrap();
    assert_eq!(by_instrument.len(), 1);
    assert_basis(&by_instrument[0]);

    let by_series = store
        .prices_between(&series, &instrument_id, &venue, window)
        .unwrap();
    assert_eq!(by_series.len(), 1);
    assert_basis(&by_series[0]);
}

#[test]
fn completeness_boundary_is_available_at_knowledge_time_and_not_earlier() {
    let (mut store, _) = store_with_instrument();
    let series = series("SBER:TQBR:1");
    let run = store
        .begin_run(
            series.clone(),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("start");
    store
        .finish_run(
            &run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("boundary published");
    store
        .connection()
        .execute(
            "UPDATE series_completeness
             SET updated_at = '2026-08-04T12:00:00Z'
             WHERE source_id = 'moex-iss'
               AND dataset = 'prices'
               AND series_key = 'SBER:TQBR:1'",
            [],
        )
        .expect("knowledge moment recorded");

    assert_eq!(
        store
            .complete_through_at_or_before(&series, "2026-08-04T12:00:00Z")
            .expect("read at boundary"),
        Some(date!(2026 - 08 - 03))
    );
    assert_eq!(
        store
            .complete_through_at_or_before(&series, "2026-08-04T11:59:59Z")
            .expect("read before boundary"),
        None
    );
}

#[test]
fn fx_rates_are_returned_only_within_window_and_at_knowledge_time() {
    let (mut store, _) = store_with_instrument();
    let series = series_with_dataset("fx", "USD/RUB");
    let run = store
        .begin_run(
            series.clone(),
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 04),
            lease(),
        )
        .expect("rates started");
    let rows = [
        FxRow {
            from_code: "USD".to_owned(),
            to_code: "RUB".to_owned(),
            trade_date: "2026-08-01".to_owned(),
            observed_at: "2026-08-02T09:00:00Z".to_owned(),
            nominal: 1,
            value: "80".to_owned(),
            unit_rate: "80".to_owned(),
        },
        FxRow {
            from_code: "USD".to_owned(),
            to_code: "RUB".to_owned(),
            trade_date: "2026-08-03".to_owned(),
            observed_at: "2026-08-04T12:00:00Z".to_owned(),
            nominal: 1,
            value: "81".to_owned(),
            unit_rate: "81".to_owned(),
        },
        FxRow {
            from_code: "USD".to_owned(),
            to_code: "RUB".to_owned(),
            trade_date: "2026-08-04".to_owned(),
            observed_at: "2026-08-04T13:00:00Z".to_owned(),
            nominal: 1,
            value: "82".to_owned(),
            unit_rate: "82".to_owned(),
        },
    ];
    store
        .record_fx(&run, "raw-fx", &rows)
        .expect("rates recorded");
    store
        .finish_run(&run, RunOutcome::Succeeded, None)
        .expect("rates published");

    let found = store
        .fx_between(
            &series,
            "USD",
            "RUB",
            iaam_store::market::MarketWindow {
                from: "2026-08-01",
                to: "2026-08-03",
                knowledge_as_of: "2026-08-04T12:00:00Z",
            },
        )
        .expect("read rates");

    assert_eq!(found, rows[..2].to_vec());
}

#[test]
fn key_rates_are_returned_only_through_trade_and_knowledge_boundaries() {
    let (mut store, _) = store_with_instrument();
    let series = series_with_dataset("key-rate", "CBR");
    let run = store
        .begin_run(
            series.clone(),
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 04),
            lease(),
        )
        .expect("key rate started");
    let rows = [
        KeyRateRow {
            trade_date: "2026-08-01".to_owned(),
            observed_at: "2026-08-02T09:00:00Z".to_owned(),
            rate: "18".to_owned(),
        },
        KeyRateRow {
            trade_date: "2026-08-03".to_owned(),
            observed_at: "2026-08-04T12:00:00Z".to_owned(),
            rate: "17.5".to_owned(),
        },
        KeyRateRow {
            trade_date: "2026-08-04".to_owned(),
            observed_at: "2026-08-04T13:00:00Z".to_owned(),
            rate: "17".to_owned(),
        },
    ];
    store
        .record_key_rate(&run, "raw-key-rate", &rows)
        .expect("rates recorded");
    store
        .finish_run(&run, RunOutcome::Succeeded, None)
        .expect("rates published");

    let found = store
        .key_rates_through(&series, "2026-08-03", "2026-08-04T12:00:00Z")
        .expect("read key rates");

    assert_eq!(found, rows[..2].to_vec());
}
