//! Migration 0010: bond payment schedule snapshots.

mod common;

use common::apply_migrations_through;
use rusqlite::Connection;

// The version-nine database must include every production table, index, and constraint from
// migrations one through nine so migration 0010 runs against the schema it will encounter in
// production.
fn database_at_version_nine() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 9);
    conn.execute(
        "INSERT INTO instruments (
             id, kind, symbol, title, denomination_currency,
             settlement_currency, quote_currency, created_at
         ) VALUES ('instrument-1', 'bond', 'SU46020RMFS2', 'OFZ 46020', 'RUB', 'RUB', 'RUB',
                   '1970-01-01T00:00:00Z')",
        [],
    )
    .expect("instrument");
    conn
}

#[test]
fn a_snapshot_row_cannot_be_rewritten() {
    // A snapshot is an observation: correcting the source is recorded as a new snapshot,
    // not by editing the old one. Otherwise, reproducing the report at its previous
    // point in the knowledge timeline is lost irreversibly.
    let conn = database_at_version_nine();
    iaam_store::schema::migrate(&conn).expect("migrated to 10");
    conn.execute_batch(
        "INSERT INTO schedule_snapshots
             (id, instrument_id, source_id, observed_at, content_hash, recorded_at)
         VALUES ('snap-1', 'instrument-1', 'moex-iss',
                 '2026-08-27T12:00:00Z', 'hash-1', '2026-08-27T12:00:00Z');",
    )
    .expect("snapshot saved");

    let rewritten = conn.execute(
        "UPDATE schedule_snapshots SET content_hash = 'hash-2' WHERE id = 'snap-1'",
        [],
    );
    assert!(rewritten.is_err(), "editing a snapshot must be forbidden");

    let deleted = conn.execute("DELETE FROM schedule_snapshots WHERE id = 'snap-1'", []);
    assert!(deleted.is_err(), "deleting a snapshot must be forbidden");
}

#[test]
fn a_coupon_row_belongs_to_a_snapshot_and_has_no_own_knowledge_axis() {
    // The knowledge axis belongs to the snapshot. An observed_at column on the row would
    // produce a row-level model that cannot express a row disappearing.
    let conn = database_at_version_nine();
    iaam_store::schema::migrate(&conn).expect("migrated to 10");
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info('schedule_coupon_periods')")
        .expect("table description");
    let columns: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("reading columns")
        .collect::<Result<_, _>>()
        .expect("columns");
    assert!(columns.iter().any(|c| c == "snapshot_id"));
    assert!(
        !columns.iter().any(|c| c == "observed_at"),
        "a chart row must not have its own knowledge axis: {columns:?}"
    );
}
