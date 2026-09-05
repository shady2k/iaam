//! Migration 0030 — the backward-pointing index and the minted-rule columns.

mod common;

use common::apply_migrations_through;
use rusqlite::{Connection, params};

const OWNER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ACCOUNT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const SOURCE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const HEAD: &str = "11111111-1111-4111-8111-111111111111";
const REPLACEMENT: &str = "22222222-2222-4222-8222-222222222222";

fn database_at_version_twenty_nine() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 29);

    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, 1, ?2, ?3, 'cash_in', '2026-02-01', 1,
                   'none', NULL, ?4, NULL, NULL, ?5, '{}', '2026-02-01T00:00:00Z')",
        params![HEAD, OWNER, ACCOUNT, SOURCE, "1".repeat(64)],
    )
    .expect("the head of the chain");
    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, 1, ?2, ?3, 'cash_in', '2026-02-02', 2,
                   'replaces', ?4, ?5, NULL, NULL, ?6, '{}', '2026-02-02T00:00:00Z')",
        params![REPLACEMENT, OWNER, ACCOUNT, HEAD, SOURCE, "2".repeat(64)],
    )
    .expect("a correction naming the head as its relation target");

    conn.execute(
        "INSERT INTO import_sessions (id, owner, state, source, import, opened_at, closed_at)
         VALUES (?1, ?2, 'open', NULL, NULL, '2026-02-01T00:00:00Z', NULL)",
        params!["dddddddd-dddd-4ddd-8ddd-dddddddddddd", OWNER],
    )
    .expect("an open session");
    conn.execute(
        "INSERT INTO import_observations (session, row, row_key, concluded, payload, answer)
         VALUES (?1, 1, NULL, 0, '{}', NULL)",
        params!["dddddddd-dddd-4ddd-8ddd-dddddddddddd"],
    )
    .expect("a pre-existing observation with no answer yet");

    conn
}

/// Applying 0030 to a database left by 0029 must not touch a single existing
/// row: the events stay exactly as recorded, including the relation that was
/// already there before the index existed to search it by.
#[test]
fn the_migration_preserves_preexisting_events_and_their_relation() {
    let conn = database_at_version_twenty_nine();

    iaam_store::schema::migrate(&conn).expect("migration 0030");

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);

    let count: u32 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("event count");
    assert_eq!(count, 2);

    let relation_target: Option<String> = conn
        .query_row(
            "SELECT relation_target FROM events WHERE id = ?1",
            [REPLACEMENT],
            |row| row.get(0),
        )
        .expect("the replacement row");
    assert_eq!(relation_target.as_deref(), Some(HEAD));
}

/// An observation recorded before an answer could mint a rule names none, and
/// the migration must not invent one: nothing was ever recorded about which
/// rule, if any, such an answer would have minted.
#[test]
fn the_migration_invents_no_minted_rule_for_an_observation_that_named_none() {
    let conn = database_at_version_twenty_nine();

    iaam_store::schema::migrate(&conn).expect("migration 0030");

    let (rule, rule_version): (Option<String>, Option<u32>) = conn
        .query_row(
            "SELECT answer_rule, answer_rule_version FROM import_observations
             WHERE session = ?1 AND row = 1",
            params!["dddddddd-dddd-4ddd-8ddd-dddddddddddd"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the pre-existing observation");
    assert_eq!(rule, None);
    assert_eq!(rule_version, None);
}

/// The chain walk (a later task) asks "which fact names this one" by
/// `relation_target`. Without a dedicated index that question is a full scan
/// of the journal; this proves the planner picks the index instead.
#[test]
fn a_lookup_by_relation_target_uses_the_index_rather_than_scanning() {
    let conn = database_at_version_twenty_nine();
    iaam_store::schema::migrate(&conn).expect("migration 0030");

    let plan: String = conn
        .query_row(
            "EXPLAIN QUERY PLAN
             SELECT id FROM events WHERE owner = ?1 AND relation_target = ?2",
            params![OWNER, HEAD],
            |row| row.get(3),
        )
        .expect("a query plan");

    assert!(
        plan.contains("events_by_relation_target"),
        "the chain walk must not scan the journal: {plan}"
    );
}
