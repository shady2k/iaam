//! Снимки графика выплат: дедуп, чтение на координату знания, исчезновение строк.

use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_store::SqliteStore;
use iaam_store::reference::InstrumentRecord;
use iaam_store::schedule::{
    CouponPeriodRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};

// Приём взят из `market_observations.rs`: инструмент заводится публичным
// `upsert_instrument`, а не сырым SQL — тест не должен знать схему лучше,
// чем её знает хранилище.
fn store() -> (SqliteStore, InstrumentId) {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
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
    // Иначе ежедневная синхронизация писала бы неизменный график каждый
    // день, и ряд рос бы в сотни раз без единого нового факта.
    let (mut store, instrument) = store();
    let rows = vec![coupon("2026-02-15", "2026-08-15")];
    let first = store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &rows,
            &[],
            &[],
        )
        .expect("первый снимок");
    let second = store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-1"),
            &rows,
            &[],
            &[],
        )
        .expect("повтор с тем же содержимым");
    assert!(first.written, "первый снимок обязан записаться");
    assert!(!second.written, "неизменный снимок писаться не должен");
    assert_eq!(first.snapshot_id, second.snapshot_id);
}

#[test]
fn a_row_missing_from_the_next_snapshot_disappears() {
    // Это то, чего построчная модель не умела: отменённая амортизация
    // обязана исчезнуть, а не остаться рядом с новым графиком.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[repayment("2034-08-09", "25"), repayment("2035-02-07", "25")],
            &[],
        )
        .expect("снимок с двумя возвратами");
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-2"),
            &[],
            &[repayment("2035-02-07", "25")],
            &[],
        )
        .expect("снимок с одним возвратом");

    let later = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-29T00:00:00Z",
        )
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(later.principal_repayments.len(), 1);
    assert_eq!(later.principal_repayments[0].repayment_date, "2035-02-07");
}

#[test]
fn a_later_snapshot_does_not_change_an_earlier_answer() {
    // Свойство монотонности по оси знания: добавление более позднего
    // наблюдения не меняет ответ на меньший knowledge_as_of.
    let (mut store, instrument) = store();
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-27T12:00:00Z", "hash-1"),
            &[],
            &[repayment("2034-08-09", "25"), repayment("2035-02-07", "25")],
            &[],
        )
        .expect("первый снимок");
    let before = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-27T23:59:59Z",
        )
        .expect("чтение")
        .expect("снимок найден");
    store
        .record_schedule_snapshot(
            &snapshot(instrument, "2026-08-28T12:00:00Z", "hash-2"),
            &[],
            &[repayment("2035-02-07", "25")],
            &[],
        )
        .expect("второй снимок");
    let again = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-27T23:59:59Z",
        )
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(before.principal_repayments, again.principal_repayments);
    assert_eq!(again.principal_repayments.len(), 2);
}

#[test]
fn an_offer_window_without_conditions_reads_back_as_absent_not_zero() {
    // Пустая цена выкупа — незнание условий. Ноль здесь означал бы
    // выкуп даром, и метрика посчиталась бы правдоподобно неверно.
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
        .expect("снимок с окном");
    let stored = store
        .schedule_at_or_before(
            &instrument.inner().to_string(),
            "moex-iss",
            "2026-08-27T23:59:59Z",
        )
        .expect("чтение")
        .expect("снимок найден");
    assert_eq!(stored.offer_windows[0].price_percent, None);
}
