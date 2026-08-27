//! Миграция 0008: основание котировки в наблюдении цены.

use iaam_store::SqliteStore;
use rusqlite::Connection;

fn database_at_version_seven() -> Connection {
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

    for (version, sql) in [
        (
            6,
            include_str!("../migrations/0006_market_observations.sql"),
        ),
        (
            7,
            include_str!("../migrations/0007_executability_without_stale.sql"),
        ),
    ] {
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"
        ))
        .expect("применение предыдущей миграции");
    }
    conn.execute(
        "INSERT INTO sync_runs
             (id, source_id, dataset, series_key, status,
              requested_from, requested_to, started_at, lease_token)
         VALUES ('run-1', 'moex-iss', 'prices', 'instrument-1:TQBR:1', 'succeeded',
                 '2026-08-01', '2026-08-01', '2026-08-02T00:00:00Z', 'lease-1')",
        [],
    )
    .expect("запуск синхронизации");
    conn.execute(
        "INSERT INTO price_observations
             (instrument_id, board, session, trade_date, kind, source_id,
              observed_at, price, currency, executability, raw_hash, sync_run_id)
         VALUES ('instrument-1', 'TQBR', 1, '2026-08-01', 'close', 'moex-iss',
                 '2026-08-02T09:00:00Z', '100.00', 'RUB', 'executable',
                 'hash-1', 'run-1')",
        [],
    )
    .expect("старое наблюдение цены");
    conn
}

#[test]
fn an_existing_observation_migrates_to_an_undecided_basis() {
    // Подставить старой строке `money_per_unit` значило бы объявить
    // доказанным то, чего никто не доказывал: облигационных строк
    // в ней могло и не быть, а могло и быть (§10.4).
    let conn = database_at_version_seven();
    iaam_store::schema::migrate(&conn).expect("миграция 0008");

    let basis: String = conn
        .query_row(
            "SELECT quotation_basis FROM price_observations LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("основание старой строки");
    assert_eq!(basis, "unknown");
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("версия схемы");
    // Сверяется с константой, а не с цифрой 8: этот тест про то, что
    // миграция 0008 делает со старой строкой, а не про то, сколько
    // миграций в проекте всего. Вшитая цифра краснела бы у каждой
    // следующей схемы, ничего о ней не проверяя.
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);
}

#[test]
fn an_unknown_basis_code_is_refused_by_the_table() {
    let store = SqliteStore::open_in_memory().expect("актуальная схема");
    let refused = store.connection().execute(
        "INSERT INTO price_observations (
             instrument_id, board, session, trade_date, kind, source_id,
             observed_at, price, currency, quotation_basis, basis_evidence,
             executability, raw_hash, sync_run_id
         ) VALUES ('i','TQBR',3,'2026-08-03','close','s','2026-08-03T19:00:00Z',
                   '100','RUB','percent','x','executable','h','r')",
        [],
    );
    assert!(
        refused.is_err(),
        "неизвестный код основания обязан быть отвергнут"
    );
}

#[test]
fn the_append_only_triggers_survive_the_migration() {
    // Правка таблицы не имеет права снять заслон: пересоздание через
    // `_new` уносит триггеры вместе со старой таблицей.
    let store = SqliteStore::open_in_memory().expect("актуальная схема");
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name = 'price_observations'",
            [],
            |row| row.get(0),
        )
        .expect("триггеры наблюдений");
    assert_eq!(count, 2, "оба триггера append-only обязаны существовать");
}
