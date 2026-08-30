//! Migration 0008: quote basis in a price observation.

use iaam_store::SqliteStore;
use rusqlite::Connection;

fn database_at_version_seven() -> Connection {
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
        .expect("applying previous migration");
    }
    conn.execute(
        "INSERT INTO sync_runs
             (id, source_id, dataset, series_key, status,
              requested_from, requested_to, started_at, lease_token)
         VALUES ('run-1', 'moex-iss', 'prices', 'instrument-1:TQBR:1', 'succeeded',
                 '2026-08-01', '2026-08-01', '2026-08-02T00:00:00Z', 'lease-1')",
        [],
    )
    .expect("running synchronization");
    conn.execute(
        "INSERT INTO price_observations
             (instrument_id, board, session, trade_date, kind, source_id,
              observed_at, price, currency, executability, raw_hash, sync_run_id)
         VALUES ('instrument-1', 'TQBR', 1, '2026-08-01', 'close', 'moex-iss',
                 '2026-08-02T09:00:00Z', '100.00', 'RUB', 'executable',
                 'hash-1', 'run-1')",
        [],
    )
    .expect("old price observation");
    conn
}

#[test]
fn an_existing_observation_migrates_to_an_undecided_basis() {
    // Filling the old row's `money_per_unit` would mean declaring
    // proven something no one proved: there may have been bond rows
    // in it, or there may not have been (§10.4).
    let conn = database_at_version_seven();
    iaam_store::schema::migrate(&conn).expect("migration 0008");

    let basis: String = conn
        .query_row(
            "SELECT quotation_basis FROM price_observations LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("basis of old row");
    assert_eq!(basis, "unknown");
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    // Compare against the constant, not the digit 8: this test is about what
    // migration 0008 does to the old row, not how many
    // migrations there are in the project overall. A hardcoded digit would turn red with every
    // subsequent schema, without checking anything about it.
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);
}

#[test]
fn an_unknown_basis_code_is_refused_by_the_table() {
    let store = SqliteStore::open_in_memory().expect("current schema");
    let refused = store.connection().execute(
        "INSERT INTO price_observations (
             instrument_id, board, session, trade_date, kind, source_id,
             observed_at, price, currency, quotation_basis, basis_evidence,
             executability, raw_hash, sync_run_id
         ) VALUES ('i','TQBR',3,'2026-08-03','close','s','2026-08-03T19:00:00Z',
                   '100','RUB','percent','x','executable','h','r')",
        [],
    );
    assert!(refused.is_err(), "an unknown basis code must be rejected");
}

#[test]
fn the_append_only_triggers_survive_the_migration() {
    // Table changes must not be allowed to remove the guard: recreating via
    // `_new` carries the triggers along with the old table.
    let store = SqliteStore::open_in_memory().expect("up-to-date schema");
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name = 'price_observations'",
            [],
            |row| row.get(0),
        )
        .expect("observation triggers");
    assert_eq!(count, 2, "both append-only triggers must exist");
}
