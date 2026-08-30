//! Payout schedule snapshots: deduplication, reading at a knowledge date, disappearing rows.

use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_store::SqliteStore;
use iaam_store::reference::InstrumentRecord;
use iaam_store::schedule::{
    CouponPeriodRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};

// Pattern taken from `market_observations.rs`: the instrument is created through the public
// `upsert_instrument`, rather than raw SQL—the test should not know the schema better
// than the store itself does.
fn store() -> (SqliteStore, InstrumentId) {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
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
    (store, instrument)
}

fn coupon(period_start: &str, payment: &str) -> CouponPeriodRow {
    CouponPeriodRow {
        period_start: period_start.to_owned(),
        accrual_end: payment.to_owned(),
        payment_date: payment.to_owned(),
        record_date: None,
        amount_status: "undetermined".to_owned(),
        amount_per_unit: None,
        amount_currency: None,
        rate_percent: None,
        source_entry_id: None,
    }
}

fn repayment(date: &str, share: &str) -> PrincipalRepaymentRow {
    PrincipalRepaymentRow {
        repayment_date: date.to_owned(),
        share_percent: share.to_owned(),
        source_kind: "amortization".to_owned(),
        source_entry_id: None,
    }
}

fn snapshot(instrument: InstrumentId, observed_at: &str, hash: &str) -> ScheduleSnapshotRow {
    ScheduleSnapshotRow {
        instrument_id: instrument.inner().to_string(),
        source_id: "moex-iss".to_owned(),
        observed_at: observed_at.to_owned(),
        content_hash: hash.to_owned(),
    }
}

#[test]
fn an_unchanged_snapshot_is_not_written_twice() {
    // Otherwise, daily synchronization would write the unchanged schedule every
    // day, and the series would grow a hundredfold without a single new fact.
    let (mut store, instrument) = store();
    let rows = vec![coupon("2026-02-15", "2026-08-15")];
    let first = store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &rows,
            &[],
            &[],
        )
        .expect("first snapshot");
    let second = store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-1"),
            &rows,
            &[],
            &[],
        )
        .expect("repeat with the same contents");
    assert!(first.written, "the first snapshot must be written");
    assert!(!second.written, "an unchanged snapshot must not be written");
    assert_eq!(first.snapshot_id, second.snapshot_id);
}

#[test]
fn a_row_missing_from_the_next_snapshot_disappears() {
    // This is what the row-based model could not do: a cancelled amortization
    // must disappear rather than remain alongside the new schedule.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[repayment("2034-08-09", "25"), repayment("2035-02-07", "25")],
            &[],
        )
        .expect("snapshot with two repayments");
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-2"),
            &[],
            &[repayment("2035-02-07", "25")],
            &[],
        )
        .expect("snapshot with one repayment");

    let later = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-29T00:00:00Z",
        )
        .expect("read")
        .expect("snapshot found");
    assert_eq!(later.principal_repayments.len(), 1);
    assert_eq!(later.principal_repayments[0].repayment_date, "2035-02-07");
}

#[test]
fn a_later_snapshot_does_not_change_an_earlier_answer() {
    // Monotonicity along the knowledge axis: adding a later
    // Observations do not change the answer for an earlier knowledge_as_of.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[repayment("2034-08-09", "25"), repayment("2035-02-07", "25")],
            &[],
        )
        .expect("first snapshot");
    let before = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-27T23:59:59Z",
        )
        .expect("read")
        .expect("snapshot found");
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-2"),
            &[],
            &[repayment("2035-02-07", "25")],
            &[],
        )
        .expect("second snapshot");
    let again = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-27T23:59:59Z",
        )
        .expect("read")
        .expect("snapshot found");
    assert_eq!(before.principal_repayments, again.principal_repayments);
    assert_eq!(again.principal_repayments.len(), 2);
}

#[test]
fn an_offer_window_without_conditions_reads_back_as_absent_not_zero() {
    // An empty redemption price means the terms are unknown. Zero here would mean
    // free redemption, and the metric would be calculated plausibly incorrectly.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[],
            &[OfferWindowRow {
                execution_date: "2027-08-26".to_owned(),
                submission_start: None,
                submission_end: None,
                price_percent: None,
                agent: None,
                source_kind: "Оферта".to_owned(),
                source_entry_id: None,
            }],
        )
        .expect("snapshot with window");
    let stored = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-27T23:59:59Z",
        )
        .expect("read")
        .expect("snapshot found");
    assert_eq!(stored.offer_windows[0].price_percent, None);
}
