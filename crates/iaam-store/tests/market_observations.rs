//! Инварианты публикации и воспроизводимого чтения рыночных рядов.

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
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".to_owned(),
            title: "Сбербанк".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
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

/// Аренда, заведомо действующая на момент прогона.
///
/// Абсолютный момент здесь был бы бомбой замедленного действия:
/// `begin_run` отказывает при `lease_expires_at <= now_utc()`, и записанная
/// дата однажды наступает — весь файл падает с `LeaseExpired` без единой
/// правки кода. Так уже случилось (iaam-816).
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
        .expect("запуск курсов");
    let rows = [
        fx("2026-08-03T09:00:00Z", "80"),
        fx("2026-08-03T10:00:00Z", "81"),
    ];

    let inserted = store
        .record_fx(&run, "raw-fx", &rows)
        .expect("курсы записаны");

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
        .expect("запуск ключевой ставки");
    let rows = [
        key_rate("2026-08-03T09:00:00Z", "18"),
        key_rate("2026-08-03T10:00:00Z", "17.5"),
    ];

    let inserted = store
        .record_key_rate(&run, "raw-key-rate", &rows)
        .expect("ключевая ставка записана");

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
        .expect("свежая аренда");
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
        .expect("исходная аренда осталась действующей");
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
        .expect("запуск курсов");
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
        .expect("исходный запуск остался незавершённым");
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
        .expect("запуск курсов");
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
        .expect("исходный запуск остался незавершённым");
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
        .expect("активная аренда");
    store
        .connection()
        .execute(
            "UPDATE sync_runs SET lease_expires_at = '2026-08-01T00:00:00Z' WHERE id = ?1",
            [&run.id],
        )
        .expect("тестовая просрочка");

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
        .expect("активная аренда");
    store
        .connection()
        .execute(
            "UPDATE sync_runs SET lease_expires_at = NULL WHERE id = ?1",
            [&run.id],
        )
        .expect("тестовая аренда удалена");

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
        .expect("первый запуск");
    store
        .record_prices(
            &first,
            "raw-first",
            &[price(instrument, "2026-08-03T09:00:00Z", "100")],
        )
        .expect("первая цена");
    store
        .finish_run(
            &first,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("первый запуск опубликован");

    let correction = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("исправляющий запуск");
    store
        .record_prices(
            &correction,
            "raw-correction",
            &[price(instrument, "2026-08-03T10:00:00Z", "101")],
        )
        .expect("исправленная цена");
    store
        .finish_run(&correction, RunOutcome::Succeeded, None)
        .expect("исправляющий запуск опубликован");

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
        .expect("чтение до исправления")
        .expect("старая цена существует");
    let after_correction = store
        .prices_at_or_before(
            &instrument.inner().to_string(),
            &venue,
            "2026-08-03",
            "2026-08-03T11:00:00Z",
        )
        .expect("чтение после исправления")
        .expect("новая цена существует");

    assert_eq!(before_correction.price, "100");
    assert_eq!(after_correction.price, "101");
    let rows: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM price_observations", [], |row| {
            row.get(0)
        })
        .expect("число строк");
    assert_eq!(rows, 2, "исправление добавлено рядом со старым наблюдением");
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
        .expect("полный запуск");
    store
        .finish_run(
            &complete,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("полный запуск опубликован");

    let partial = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 04),
            date!(2026 - 08 - 10),
            lease(),
        )
        .expect("частичный запуск");
    store
        .finish_run(
            &partial,
            RunOutcome::Partial {
                reason: "страница 8 из 10 недоступна".to_owned(),
            },
            Some(Coverage {
                from: date!(2026 - 08 - 04),
                to: date!(2026 - 08 - 08),
            }),
        )
        .expect("частичный запуск зафиксирован");

    assert_eq!(
        store
            .complete_through(&series("SBER:TQBR:1"))
            .expect("граница полноты"),
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
        .expect("неудачная серия");
    store
        .finish_run(
            &failed,
            RunOutcome::Failed {
                reason: "MOEX недоступен".to_owned(),
            },
            None,
        )
        .expect("неудача записана");

    let other = store
        .begin_run(
            series("GAZP:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("другая серия не заблокирована");
    store
        .finish_run(
            &other,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("другая серия опубликована");

    assert_eq!(
        store
            .complete_through(&series("SBER:TQBR:1"))
            .expect("граница первой серии"),
        None
    );
    assert_eq!(
        store
            .complete_through(&series("GAZP:TQBR:1"))
            .expect("граница второй серии"),
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
        .expect("запуск");
    store
        .record_prices(
            &run,
            "raw",
            &[price(instrument, "2026-08-03T09:00:00Z", "100")],
        )
        .expect("незавершённая строка");

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
        .expect("чтение");
    assert!(found.is_none(), "running не публикуется чтением");
}

#[test]
fn accrued_interest_is_invisible_before_its_knowledge_coordinate() {
    // Наблюдение, записанное позже координаты, обязано быть невидимым:
    // иначе отчёт «на вчера» пересчитается от завтрашнего знания.
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
    store
        .finish_run(&run, RunOutcome::Succeeded, None)
        .unwrap();

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
        .expect("первая аренда");
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
        .expect("первый запуск");
    store
        .record_prices(
            &first,
            "raw-1",
            &[price(instrument, "2026-08-03T09:00:00Z", "100")],
        )
        .expect("первое наблюдение");
    store
        .finish_run(&first, RunOutcome::Succeeded, None)
        .expect("первое наблюдение опубликовано");

    let second = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("второй запуск");
    store
        .record_prices(
            &second,
            "raw-2",
            &[price(instrument, "2026-08-03T10:00:00Z", "101")],
        )
        .expect("исправленное наблюдение");
    store
        .finish_run(&second, RunOutcome::Succeeded, None)
        .expect("второе наблюдение опубликовано");

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
        .expect("чтение на ранний момент")
        .expect("ранняя цена");
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
        .expect("чтение на поздний момент")
        .expect("поздняя цена");
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
        .expect("активная аренда");
    store
        .connection()
        .execute(
            "UPDATE sync_runs SET lease_expires_at = '2026-08-01T00:00:00Z' WHERE id = ?1",
            [&old.id],
        )
        .expect("тестовая просрочка");
    let replacement = store
        .begin_run(
            series("SBER:TQBR:1"),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            OffsetDateTime::now_utc() + Duration::hours(1),
        )
        .expect("просроченная аренда освобождена");
    assert!(matches!(
        store.finish_run(&old, RunOutcome::Succeeded, None),
        Err(StoreError::RunNotFound)
    ));
    store
        .finish_run(&replacement, RunOutcome::Succeeded, None)
        .expect("новый запуск завершён");
}

#[test]
fn the_quotation_basis_survives_a_round_trip_through_every_read_path() {
    // Основание, потерянное на одном из путей чтения, обнаружится
    // не отказом, а заниженной в номинал/100 раз стоимостью позиции.
    let (mut store, instrument) = store_with_instrument();
    let series = series_with_dataset("moex", "SBER");
    let run = store
        .begin_run(
            series.clone(),
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease(),
        )
        .expect("запуск цен");
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
