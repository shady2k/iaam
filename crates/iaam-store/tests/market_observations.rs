//! Инварианты публикации и воспроизводимого чтения рыночных рядов.
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_market::observation::{
    Executability, ObservedAt, PriceKind, PriceObservation, TradeDate, Venue,
};
use iaam_store::market::{Coverage, RunOutcome, SeriesKey};
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
    SeriesKey {
        source_id: "moex-iss".to_owned(),
        dataset: "prices".to_owned(),
        series_key: name.to_owned(),
    }
}

fn price(instrument: InstrumentId, observed_at: OffsetDateTime, value: &str) -> PriceObservation {
    PriceObservation {
        instrument,
        venue: Venue {
            board: "TQBR".to_owned(),
            session: 1,
        },
        trade_date: TradeDate(date!(2026 - 08 - 03)),
        observed_at: ObservedAt(observed_at),
        kind: PriceKind::Close,
        price: dec(value),
        currency: CurrencyCode::Rub,
        executability: Executability::Executable,
    }
}

fn dec(value: &str) -> Dec {
    serde_json::from_str(&format!("\"{value}\"")).expect("десятичное число")
}

fn lease() -> OffsetDateTime {
    datetime!(2026-08-27 00:00:00 UTC)
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
            &[price(instrument, datetime!(2026-08-03 09:00:00 UTC), "100")],
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
            &[price(instrument, datetime!(2026-08-03 10:00:00 UTC), "101")],
        )
        .expect("исправленная цена");
    store
        .finish_run(&correction, RunOutcome::Succeeded, None)
        .expect("исправляющий запуск опубликован");

    let venue = Venue {
        board: "TQBR".to_owned(),
        session: 1,
    };
    let before_correction = store
        .prices_at_or_before(
            instrument,
            &venue,
            date!(2026 - 08 - 03),
            datetime!(2026-08-03 09:30:00 UTC),
        )
        .expect("чтение до исправления")
        .expect("старая цена существует");
    let after_correction = store
        .prices_at_or_before(
            instrument,
            &venue,
            date!(2026 - 08 - 03),
            datetime!(2026-08-03 11:00:00 UTC),
        )
        .expect("чтение после исправления")
        .expect("новая цена существует");

    assert_eq!(before_correction.price, dec("100"));
    assert_eq!(after_correction.price, dec("101"));
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
            &[price(instrument, datetime!(2026-08-03 09:00:00 UTC), "100")],
        )
        .expect("незавершённая строка");

    let found = store
        .prices_at_or_before(
            instrument,
            &Venue {
                board: "TQBR".to_owned(),
                session: 1,
            },
            date!(2026 - 08 - 03),
            datetime!(2026-08-03 12:00:00 UTC),
        )
        .expect("чтение");
    assert!(found.is_none(), "running не публикуется чтением");
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
            &[price(instrument, datetime!(2026-08-03 09:00:00 UTC), "100")],
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
            &[price(instrument, datetime!(2026-08-03 10:00:00 UTC), "101")],
        )
        .expect("исправленное наблюдение");
    store
        .finish_run(&second, RunOutcome::Succeeded, None)
        .expect("второе наблюдение опубликовано");

    let value = store
        .prices_at_or_before(
            instrument,
            &Venue {
                board: "TQBR".to_owned(),
                session: 1,
            },
            date!(2026 - 08 - 03),
            datetime!(2026-08-03 09:30:00 UTC),
        )
        .expect("чтение на ранний момент")
        .expect("ранняя цена");
    assert_eq!(value.price, dec("100"));

    let later = store
        .prices_at_or_before(
            instrument,
            &Venue {
                board: "TQBR".to_owned(),
                session: 1,
            },
            date!(2026 - 08 - 03),
            datetime!(2026-08-03 11:00:00 UTC),
        )
        .expect("чтение на поздний момент")
        .expect("поздняя цена");
    assert_eq!(later.price, dec("101"));
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
