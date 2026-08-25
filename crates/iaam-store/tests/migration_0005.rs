//! Перенос данных при миграции 0005.
//!
//! Проверка на непустой базе обязательна: на пустой базе перенос
//! данных верен тривиально, а ломается он ровно на существующих
//! строках.

use rusqlite::Connection;

/// Схема версии 4 в объёме, который затрагивает миграция 0005.
fn database_at_version_four() -> Connection {
    let conn = Connection::open_in_memory().expect("база в памяти");
    conn.execute_batch(
        "CREATE TABLE instruments (
             id       TEXT PRIMARY KEY,
             symbol   TEXT NOT NULL,
             title    TEXT NOT NULL,
             currency TEXT NOT NULL
         ) STRICT;
         INSERT INTO instruments (id, symbol, title, currency)
         VALUES ('11111111-1111-4111-8111-111111111111', 'SBER', 'Сбербанк', 'RUB');
         PRAGMA user_version = 4;",
    )
    .expect("схема версии 4");
    conn
}

fn apply_migration_0005(conn: &Connection) {
    let sql = include_str!("../migrations/0005_instrument_reference.sql");
    conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = 5; COMMIT;"))
        .expect("миграция 0005");
}

#[test]
fn an_existing_instrument_keeps_its_currency_in_all_three_roles() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let (denomination, settlement, quote): (String, String, String) = conn
        .query_row(
            "SELECT denomination_currency, settlement_currency, quote_currency
             FROM instruments WHERE symbol = 'SBER'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("перенесённый инструмент");

    assert_eq!(denomination, "RUB");
    assert_eq!(settlement, "RUB");
    assert_eq!(quote, "RUB");
}

#[test]
fn an_existing_instrument_has_no_kind_guessed_for_it() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM instruments WHERE symbol = 'SBER'",
            [],
            |row| row.get(0),
        )
        .expect("перенесённый инструмент");

    assert_eq!(
        kind, None,
        "род не известен и не должен подставляться акцией"
    );
}

#[test]
fn overlapping_alias_intervals_are_refused_by_the_database() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);
    conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2020-01-01', '2024-01-01', 'manual', '2026-08-25T00:00:00Z');",
    )
    .expect("первый интервал");

    let overlapping = conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2023-06-01', NULL, 'manual', '2026-08-25T00:00:00Z');",
    );

    assert!(
        overlapping.is_err(),
        "пересечение интервалов делает резолвинг неоднозначным"
    );
}

#[test]
fn adjacent_alias_intervals_are_allowed() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let adjacent = conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2020-01-01', '2024-01-01', 'manual', '2026-08-25T00:00:00Z');
         INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2024-01-01', NULL, 'manual', '2026-08-25T00:00:00Z');",
    );

    assert!(
        adjacent.is_ok(),
        "смежные интервалы стыкуются без зазора: конец полуинтервала исключителен"
    );
}
