//! Data migration for the account-scoped source operation index.

mod common;

use common::apply_migrations_through;
use rusqlite::{Connection, params};

fn database_at_version_eleven() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 11);

    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, 1, ?2, ?3, 'cash_in', '2026-02-01', ?4,
                   'none', NULL, ?5, ?6, NULL, ?7, '{}', '2026-02-01T00:00:00Z')",
        params![
            "11111111-1111-4111-8111-111111111111",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            1_u32,
            "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "OP-1",
            "1".repeat(64),
        ],
    )
    .expect("first pre-existing event");
    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, 1, ?2, ?3, 'cash_in', '2026-02-01', ?4,
                   'none', NULL, ?5, ?6, NULL, ?7, '{}', '2026-02-01T00:00:00Z')",
        params![
            "22222222-2222-4222-8222-222222222222",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
            2_u32,
            "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "OP-2",
            "2".repeat(64),
        ],
    )
    .expect("second pre-existing event");
    conn
}

#[test]
fn account_scope_migration_preserves_preexisting_rows() {
    let conn = database_at_version_eleven();

    iaam_store::schema::migrate(&conn).expect("migration 0012");

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);
    let count: u32 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("event count");
    assert_eq!(count, 2);
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM events ORDER BY id")
        .expect("event ids")
        .query_map([], |row| row.get(0))
        .expect("event rows")
        .collect::<Result<_, _>>()
        .expect("event ids collected");
    assert_eq!(
        ids,
        vec![
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        ]
    );
}
