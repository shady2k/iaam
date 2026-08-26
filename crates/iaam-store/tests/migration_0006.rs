//! Миграция 0006: битемпоральные наблюдения рынка.
//!
//! Наблюдение за тем же торговым днём не является перезаписью: источник
//! может прислать исправленное значение, и обе версии должны остаться в базе.

use rusqlite::Connection;

fn database_at_version_five() -> Connection {
    let conn = Connection::open_in_memory().expect("база в памяти");
    conn.execute_batch(
        "CREATE TABLE instruments (
             id       TEXT PRIMARY KEY,
             symbol   TEXT NOT NULL,
             title    TEXT NOT NULL,
             currency TEXT NOT NULL
         ) STRICT;
         INSERT INTO instruments (id, symbol, title, currency)
         VALUES ('instrument-1', 'SBER', 'Сбербанк', 'RUB');
         PRAGMA user_version = 5;",
    )
    .expect("схема версии 5");
    conn
}

fn apply_migration_0006(conn: &Connection) {
    let sql = include_str!("../migrations/0006_market_observations.sql");
    conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = 6; COMMIT;"))
        .expect("миграция 0006");
}

fn insert_sync_run(conn: &Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO sync_runs
             (id, source_id, dataset, series_key, status,
              requested_from, requested_to, started_at, lease_token)
         VALUES (?1, 'moex-iss', 'prices', 'instrument-1:TQBR:1', ?2,
                 '2026-08-01', '2026-08-01',
                 '2026-08-02T00:00:00Z', ?3)",
        (id, status, format!("lease-{id}")),
    )
    .expect("запуск синхронизации");
}

fn insert_price(conn: &Connection, sync_run_id: &str, observed_at: &str, price: &str) {
    conn.execute(
        "INSERT INTO price_observations
             (instrument_id, board, session, trade_date, kind, source_id,
              observed_at, price, currency, executability, raw_hash, sync_run_id)
         VALUES ('instrument-1', 'TQBR', 1, '2026-08-01', 'close', 'moex-iss',
                 ?1, ?2, 'RUB', 'executable', ?3, ?4)",
        (
            observed_at,
            price,
            format!("hash-{observed_at}"),
            sync_run_id,
        ),
    )
    .expect("наблюдение цены");
}

#[test]
fn migration_creates_all_observation_tables_and_preserves_history() {
    let conn = database_at_version_five();
    apply_migration_0006(&conn);

    for table in [
        "price_observations",
        "fx_observations",
        "key_rate_observations",
        "sync_runs",
        "series_completeness",
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type = 'table' AND name = ?1
                 )",
                [table],
                |row| row.get(0),
            )
            .expect("проверка таблицы");
        assert!(exists, "таблица {table} создана");
    }

    let instrument: String = conn
        .query_row(
            "SELECT symbol FROM instruments WHERE id = 'instrument-1'",
            [],
            |row| row.get(0),
        )
        .expect("существующий инструмент сохранён");
    assert_eq!(instrument, "SBER");

    insert_sync_run(&conn, "run-1", "succeeded");
    insert_sync_run(&conn, "run-2", "succeeded");
    insert_price(&conn, "run-1", "2026-08-02T09:00:00Z", "100.00");
    insert_price(&conn, "run-2", "2026-08-03T09:00:00Z", "101.00");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM price_observations
             WHERE instrument_id = 'instrument-1'
               AND trade_date = '2026-08-01'
               AND source_id = 'moex-iss'",
            [],
            |row| row.get(0),
        )
        .expect("количество наблюдений");
    assert_eq!(count, 2, "исправление добавлено, а не затёрло старую цену");

    let latest_as_known: String = conn
        .query_row(
            "SELECT price FROM price_observations
             WHERE instrument_id = 'instrument-1'
               AND board = 'TQBR'
               AND session = 1
               AND trade_date = '2026-08-01'
               AND kind = 'close'
               AND source_id = 'moex-iss'
               AND observed_at <= '2026-08-03T23:59:59Z'
             ORDER BY observed_at DESC
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("последнее известное наблюдение");
    assert_eq!(latest_as_known, "101.00");
}

#[test]
fn observation_rows_are_immutable_and_running_series_has_a_lease() {
    let conn = database_at_version_five();
    apply_migration_0006(&conn);

    insert_sync_run(&conn, "run-running", "running");
    let second_running = conn.execute(
        "INSERT INTO sync_runs
             (id, source_id, dataset, series_key, status,
              requested_from, requested_to, started_at, lease_token)
         VALUES ('run-running-2', 'moex-iss', 'prices', 'instrument-1:TQBR:1',
                 'running', '2026-08-01', '2026-08-01',
                 '2026-08-02T00:00:00Z', 'lease-2')",
        [],
    );
    assert!(
        second_running.is_err(),
        "две синхронизации одной серии не стартуют"
    );

    insert_sync_run(&conn, "run-done", "succeeded");
    insert_price(&conn, "run-done", "2026-08-02T09:00:00Z", "100.00");

    assert!(
        conn.execute(
            "UPDATE price_observations SET price = '999.00' WHERE sync_run_id = 'run-done'",
            [],
        )
        .is_err(),
        "наблюдения нельзя исправить UPDATE"
    );
    assert!(
        conn.execute(
            "DELETE FROM price_observations WHERE sync_run_id = 'run-done'",
            [],
        )
        .is_err(),
        "наблюдения нельзя удалить"
    );
}

#[test]
fn completeness_key_allows_an_unknown_boundary_but_stays_unique() {
    let conn = database_at_version_five();
    apply_migration_0006(&conn);

    conn.execute(
        "INSERT INTO series_completeness
             (source_id, dataset, series_key, complete_through, updated_at)
         VALUES ('moex-iss', 'prices', 'instrument-1:TQBR:1', NULL,
                 '2026-08-02T00:00:00Z')",
        [],
    )
    .expect("граница полноты может быть неизвестной");
    assert!(
        conn.execute(
            "INSERT INTO series_completeness
                 (source_id, dataset, series_key, complete_through, updated_at)
             VALUES ('moex-iss', 'prices', 'instrument-1:TQBR:1', NULL,
                     '2026-08-03T00:00:00Z')",
            [],
        )
        .is_err(),
        "единица полноты уникальна даже с NULL-границей"
    );
}
