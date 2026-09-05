//! Migration 0029 over a journal recorded before a rule could be named.

mod common;

use common::apply_migrations_through;
use rusqlite::{Connection, params};

const EVENT: &str = "11111111-1111-4111-8111-111111111111";

fn database_at_version_twenty_eight() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 28);

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

/// A fact recorded before the column existed names no rule, and must not be
/// made to name one.
///
/// The journal is append-only and the rule that settled such a row was never
/// recorded anywhere, so there is nothing to back-fill from. An invented rule
/// identifier would tell the owner that a decision of his reached a row it never
/// touched, and a `no_rule` written here would tell him he settled the row
/// himself. The migration therefore writes neither.
#[test]
fn the_migration_invents_no_rule_for_an_event_that_named_none() {
    let conn = database_at_version_twenty_eight();

    iaam_store::schema::migrate(&conn).expect("migration 0029");

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);

    let (rule, rule_version): (Option<String>, Option<u32>) = conn
        .query_row(
            "SELECT settled_by_rule, settled_by_rule_version FROM events WHERE id = ?1",
            [EVENT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("pre-existing event row");
    assert_eq!(rule, None);
    assert_eq!(rule_version, None);
}
