//! Migration 0023 over a journal recorded before a session could be named.

mod common;

use common::apply_migrations_through;
use rusqlite::{Connection, params};

const EVENT: &str = "11111111-1111-4111-8111-111111111111";

fn database_at_version_twenty_two() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 22);

    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, 1, ?2, ?3, 'cash_in', '2026-02-01', 1,
                   'none', NULL, ?4, NULL, NULL, ?5, '{}', '2026-02-01T00:00:00Z')",
        params![
            EVENT,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "1".repeat(64),
        ],
    )
    .expect("pre-existing event");
    conn
}

/// A fact recorded before the column existed names no session, and must not be
/// made to name one.
///
/// The journal is append-only, so an event committed by a session before this
/// column existed cannot be told apart from one that passed through none. The
/// migration therefore back-fills nothing: an invented session identifier would
/// send a reader to a session that never wrote that row.
#[test]
fn the_migration_invents_no_session_for_an_event_that_named_none() {
    let conn = database_at_version_twenty_two();

    iaam_store::schema::migrate(&conn).expect("migration 0023");

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);

    let session: Option<String> = conn
        .query_row(
            "SELECT import_session FROM events WHERE id = ?1",
            [EVENT],
            |row| row.get(0),
        )
        .expect("pre-existing event row");
    assert_eq!(session, None);
}
