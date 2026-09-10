//! A database written before a column existed gains it, rather than being
//! expected to have had it all along.
//!
//! `0001_schema.sql` is the collapse of thirty-one migrations, and it was
//! collapsible only because the database was empty when it happened. It is not
//! empty now: the owner's journal is in it. So a column added to that file
//! would be applied to a database created afterwards and skipped entirely by
//! one already at `user_version = 1` — which is every database that holds
//! anything — and the first statement naming the column would fail at run time
//! on his own data.
//!
//! This test stands in for the owner's file. It cannot open a database written
//! by an older build, so it makes one: a current database with the column
//! dropped, the table `0003_event_stated_securities_value.sql` adds removed,
//! and the version wound back — the shape a version-1 database actually has.

use iaam_store::SqliteStore;
use iaam_store::schema::{SCHEMA_VERSION, migrate};

fn user_version(store: &SqliteStore) -> u32 {
    store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("reading the schema version")
}

fn events_has_counterparty(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT source_counterparty FROM events LIMIT 0")
        .is_ok()
}

fn has_stated_securities_value_table(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT event FROM event_stated_securities_value LIMIT 0")
        .is_ok()
}

fn accounts_has_declared_by(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT declared_by FROM accounts LIMIT 0")
        .is_ok()
}

#[test]
fn a_database_left_at_version_one_gains_the_counterparty_column() {
    let store = SqliteStore::open_in_memory().expect("open");
    assert_eq!(
        user_version(&store),
        SCHEMA_VERSION,
        "a database this build created is at this build's version"
    );

    store
        .connection()
        .execute_batch(
            "ALTER TABLE events DROP COLUMN source_counterparty; \
             DROP TABLE event_stated_securities_value; \
             ALTER TABLE accounts DROP COLUMN declared_by; \
             PRAGMA user_version = 1;",
        )
        .expect("winding the database back to version 1");
    assert!(
        !events_has_counterparty(&store),
        "the column must be gone for this test to be about anything"
    );
    assert!(
        !has_stated_securities_value_table(&store),
        "the table must be gone too, or migration 0003 is not actually being exercised"
    );
    assert!(
        !accounts_has_declared_by(&store),
        "and this column too, or migration 0004 is not actually being exercised"
    );

    migrate(store.connection()).expect("migrating the wound-back database");

    assert!(
        events_has_counterparty(&store),
        "an existing database must gain the column, not be assumed to have had it: a column \
         added to 0001_schema.sql would be skipped by every database already at version 1"
    );
    assert!(
        has_stated_securities_value_table(&store),
        "and it must gain the table 0003 adds, by rebuilding events under a widened CHECK \
         rather than failing to rebuild it a second time"
    );
    assert!(
        accounts_has_declared_by(&store),
        "and it must gain the column 0004 adds, for the same reason as source_counterparty"
    );
    assert_eq!(
        user_version(&store),
        SCHEMA_VERSION,
        "and it must be left at this build's version, so the next migration is not reapplied"
    );
}

/// The rebuild that widened `events.kind` copies a journal that has rows in
/// it, and leaves nothing pointing at a parent that is gone.
///
/// Migration 0003 rebuilds `events` — creates a replacement, copies every row
/// across, drops the original and renames — because SQLite cannot widen a
/// `CHECK`. Fifteen tables reference `events`, so it runs with
/// `PRAGMA foreign_keys` off; that is what makes the drop possible and also
/// what means nothing would notice a row left behind. Every other test of
/// this migration runs it against an empty journal, where a copy that dropped
/// every row would still pass.
///
/// So this one gives the database an event with a leg on it, winds the version
/// back, and migrates again: the copy has something to lose, and
/// `pragma_foreign_key_check` is asked whether it lost it.
#[test]
fn the_events_rebuild_carries_a_populated_journal_and_its_children() {
    let store = SqliteStore::open_in_memory().expect("open");
    let connection = store.connection();

    // Invented throughout: an owner, an account of his, one event and one leg
    // hanging off it. The values say nothing about anybody's money.
    connection
        .execute_batch(
            "INSERT INTO accounts (id, owner, title, institution, created_at)
             VALUES ('account-1', 'owner-1', 'Main', NULL, '2026-01-01T00:00:00Z');
             INSERT INTO events (
                 id, owner, account, kind, effective_date, sequence, confidence,
                 relation_kind, recorded_at, source, raw_hash, parser_version
             ) VALUES (
                 'event-1', 'owner-1', 'account-1', 'cash_in', '2026-01-02', 1, 'known',
                 'none', '2026-01-02T00:00:00Z', 'file', 'hash-1', 'v1'
             );
             INSERT INTO event_legs (event, ordinal, kind, account, amount, currency)
             VALUES ('event-1', 0, 'cash', 'account-1', 1000, 'RUB');",
        )
        .expect("seeding an invented journal");

    // Back to the version before the rebuild, so `migrate` performs it again —
    // this time over a journal that is not empty. `accounts.declared_by` is
    // dropped too: `migrate` still catches this database up to the current
    // `SCHEMA_VERSION` afterwards, and migration 0004's `ALTER TABLE ... ADD
    // COLUMN` would otherwise find the column already there from the initial
    // `open_in_memory` and fail on a name that already exists.
    connection
        .execute_batch(
            "DROP TABLE event_stated_securities_value; \
             ALTER TABLE accounts DROP COLUMN declared_by; \
             PRAGMA user_version = 2;",
        )
        .expect("winding the database back to version 2");

    migrate(connection).expect("migrating a populated database");

    let events: i64 = connection
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .expect("counting events");
    assert_eq!(events, 1, "the rebuild must carry every row it copied");

    let legs: i64 = connection
        .query_row("SELECT count(*) FROM event_legs", [], |row| row.get(0))
        .expect("counting legs");
    assert_eq!(legs, 1, "and leave the children that reference them alone");

    let orphans: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .expect("checking references");
    assert_eq!(
        orphans, 0,
        "a rebuild run with the foreign-key guard off must leave nothing pointing at a \
         parent that is not there"
    );
}
