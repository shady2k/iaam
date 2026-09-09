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
//! dropped and the version wound back, which is byte for byte the shape an
//! older build left behind.

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
            "ALTER TABLE events DROP COLUMN source_counterparty; PRAGMA user_version = 1;",
        )
        .expect("winding the database back to version 1");
    assert!(
        !events_has_counterparty(&store),
        "the column must be gone for this test to be about anything"
    );

    migrate(store.connection()).expect("migrating the wound-back database");

    assert!(
        events_has_counterparty(&store),
        "an existing database must gain the column, not be assumed to have had it: a column \
         added to 0001_schema.sql would be skipped by every database already at version 1"
    );
    assert_eq!(
        user_version(&store),
        SCHEMA_VERSION,
        "and it must be left at this build's version, so the next migration is not reapplied"
    );
}
