//! Data migration for source-time ordering.

mod common;

use common::apply_migrations_through;
use rusqlite::{Connection, params};

fn database_at_version_twelve() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 12);

    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, 1, ?2, ?3, 'cash_in', '2026-02-01', 1,
                   'none', NULL, ?4, NULL, NULL, ?5, '{}', '2026-02-01T00:00:00Z')",
        params![
            "11111111-1111-4111-8111-111111111111",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "1".repeat(64),
        ],
    )
    .expect("pre-existing event");
    conn
}

#[test]
fn source_time_migration_preserves_preexisting_rows() {
    let conn = database_at_version_twelve();

    iaam_store::schema::migrate(&conn).expect("migration 0013");

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);
    let row: (String, Option<String>) = conn
        .query_row(
            "SELECT id, source_time FROM events WHERE id = ?1",
            ["11111111-1111-4111-8111-111111111111"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("pre-existing event row");
    assert_eq!(row.0, "11111111-1111-4111-8111-111111111111");
    assert_eq!(row.1, None);
}
