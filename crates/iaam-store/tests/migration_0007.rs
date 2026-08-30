//! Migration 0007: executability — source attribute only.

use rusqlite::Connection;

type PriceRow = (
    String,
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn database_at_version_six() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.execute_batch(
        "CREATE TABLE instruments (
             id       TEXT PRIMARY KEY,
             symbol   TEXT NOT NULL,
             title    TEXT NOT NULL,
             currency TEXT NOT NULL
         ) STRICT;
         INSERT INTO instruments (id, symbol, title, currency)
         VALUES ('instrument-1', 'SBER', 'Sberbank', 'RUB');
         PRAGMA user_version = 5;",
    )
    .expect("schema version 5");

    let sql = include_str!("../migrations/0006_market_observations.sql");
    conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = 6; COMMIT;"))
        .expect("migration 0006");
    conn
}

fn apply_migration_0007(conn: &Connection) {
    let sql = include_str!("../migrations/0007_executability_without_stale.sql");
    conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = 7; COMMIT;"))
        .expect("migration 0007");
}

fn insert_sync_run(conn: &Connection) {
    conn.execute(
        "INSERT INTO sync_runs
             (id, source_id, dataset, series_key, status,
              requested_from, requested_to, started_at, lease_token)
         VALUES ('run-1', 'moex-iss', 'prices', 'instrument-1:TQBR:1', 'succeeded',
                 '2026-08-01', '2026-08-01', '2026-08-02T00:00:00Z', 'lease-1')",
        [],
    )
    .expect("synchronization run");
}

fn insert_price(conn: &Connection, observed_at: &str, price: &str, executability: &str) {
    conn.execute(
        "INSERT INTO price_observations
             (instrument_id, board, session, trade_date, kind, source_id,
              observed_at, price, currency, executability, raw_hash, sync_run_id)
         VALUES ('instrument-1', 'TQBR', 1, '2026-08-01', 'close', 'moex-iss',
                 ?1, ?2, 'RUB', ?3, ?4, 'run-1')",
        (
            observed_at,
            price,
            executability,
            format!("hash-{observed_at}"),
        ),
    )
    .expect("price observation");
}

fn price_rows(conn: &Connection) -> Vec<PriceRow> {
    let mut statement = conn
        .prepare(
            "SELECT instrument_id, board, session, trade_date, kind, source_id,
                    observed_at, price, currency, executability, raw_hash, sync_run_id
             FROM price_observations
             ORDER BY observed_at",
        )
        .expect("query observations");
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            ))
        })
        .expect("reading observations")
        .collect::<Result<Vec<_>, _>>()
        .expect("observation rows")
}

#[test]
fn migration_rejects_stale_and_preserves_rows_index_and_triggers() {
    let conn = database_at_version_six();
    insert_sync_run(&conn);
    insert_price(&conn, "2026-08-02T09:00:00Z", "100.00", "executable");
    insert_price(
        &conn,
        "2026-08-03T09:00:00Z",
        "101.00",
        "indicative_previous_close",
    );
    let before = price_rows(&conn);

    apply_migration_0007(&conn);

    assert_eq!(price_rows(&conn), before, "history preserved unchanged");
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, 7);

    let index_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'price_observations_by_series'",
            [],
            |row| row.get(0),
        )
        .expect("observation index");
    assert_eq!(index_count, 1, "index preserved");

    for trigger in [
        "price_observations_are_immutable",
        "price_observations_are_not_deletable",
    ] {
        let trigger_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'trigger' AND name = ?1",
                [trigger],
                |row| row.get(0),
            )
            .expect("observation trigger");
        assert_eq!(trigger_count, 1, "trigger {trigger} preserved");
    }

    let err = conn
        .execute(
            "INSERT INTO price_observations
                 (instrument_id, board, session, trade_date, kind, source_id,
                  observed_at, price, currency, executability, raw_hash, sync_run_id)
             VALUES ('instrument-1', 'TQBR', 1, '2026-08-01', 'close', 'moex-iss',
                     '2026-08-04T09:00:00Z', '102.00', 'RUB', 'stale', 'hash-stale', 'run-1')",
            [],
        )
        .expect_err("stale must be rejected");
    assert!(err.to_string().contains("CHECK"), "CHECK error: {err}");

    let err = conn
        .execute(
            "UPDATE price_observations SET price = '999.00'
             WHERE observed_at = '2026-08-02T09:00:00Z'",
            [],
        )
        .expect_err("observation modification must be forbidden");
    assert!(
        err.to_string().contains("append-only"),
        "trigger error: {err}"
    );

    let err = conn
        .execute(
            "DELETE FROM price_observations
             WHERE observed_at = '2026-08-02T09:00:00Z'",
            [],
        )
        .expect_err("deleting an observation should be forbidden");
    assert!(
        err.to_string().contains("append-only"),
        "trigger error: {err}"
    );

    assert_eq!(price_rows(&conn), before, "triggers did not alter history");
}
