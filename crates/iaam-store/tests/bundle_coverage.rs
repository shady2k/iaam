//! Exact table coverage guard for [`iaam_store::bundle::TABLE_DISPOSITIONS`].
//!
//! Follows `tests/schema_coverage.rs`'s own shape: parse the schema by
//! tracking the table a `CREATE TABLE <name> (` line opens, compare the
//! resulting exact set of names against a list this crate maintains by hand,
//! and fail on any difference in either direction. A table added later and
//! left unclassified fails the build; so does a name kept in the list after
//! its table is dropped.
//!
//! The bundle drifted to five sections out of fifty-nine precisely because no
//! such comparison existed (iaam-k3gh.9). Substring or table-family matching
//! could not have caught it — nothing about `event_category_assignments`
//! looks unlike the `event_*` family that already travelled — which is why
//! this, like the foreign-key guards next to it, compares exact sets.
//!
//! **`DROP TABLE` and `ALTER TABLE … RENAME TO` are read too, not only
//! `CREATE TABLE`.** `0003_event_stated_securities_value.sql` widens a
//! `CHECK` SQLite has no `ALTER TABLE … ADD CONSTRAINT` for by rebuilding
//! `events` under a scaffolding name and renaming it back — a `CREATE TABLE
//! events_v3` that a name-only scanner would have to classify beside `events`
//! itself, for a name that never exists in the schema this crate's own
//! `SqliteStore::open` ever produces. Reading the three statements in order
//! and folding them into one running set — `CREATE` adds, `DROP` removes,
//! `RENAME` moves — is what makes the comparison the exact final schema
//! rather than every name a migration's SQL text ever mentioned in passing.

use std::collections::BTreeSet;

use iaam_store::bundle::TABLE_DISPOSITIONS;
use iaam_store::schema::MIGRATIONS;

/// The exact set of tables the schema holds once every migration has run, by
/// replaying each one's `CREATE TABLE`, `DROP TABLE` and `ALTER TABLE …
/// RENAME TO` statements against a running set, in the order the schema
/// declares them.
///
/// Reads [`MIGRATIONS`] rather than a fixed pair of `include_str!`s: `0002`
/// exists today and more will, and this guard exists precisely so a future
/// migration cannot add a table that neither this test nor
/// `TABLE_DISPOSITIONS` ever sees.
fn every_table_name() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (_, sql) in MIGRATIONS {
        for line in sql.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("CREATE TABLE ") {
                let name = rest
                    .split_once('(')
                    .map_or(rest, |(name, _)| name)
                    .trim()
                    .to_owned();
                found.insert(name);
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("DROP TABLE ") {
                found.remove(rest.trim_end_matches(';').trim());
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("ALTER TABLE ")
                && let Some((old, new)) = rest.split_once(" RENAME TO ")
            {
                found.remove(old.trim());
                found.insert(new.trim_end_matches(';').trim().to_owned());
            }
        }
    }
    found
}

#[test]
fn every_table_is_classified_exactly_once() {
    let schema_tables = every_table_name();
    let mut classified = BTreeSet::new();
    for (table, _) in TABLE_DISPOSITIONS {
        assert!(
            classified.insert((*table).to_owned()),
            "{table} is listed twice in TABLE_DISPOSITIONS — one entry per table, or the \
             later one is silently doing nothing"
        );
    }

    assert_eq!(
        schema_tables, classified,
        "TABLE_DISPOSITIONS does not name exactly the tables the migrations declare — a \
         table is either new and unclassified, or classified and no longer exists. Every \
         CREATE TABLE must be carried by Bundle or named in TABLE_DISPOSITIONS with a reason"
    );
}

/// Proves the comparison above is not vacuous: an unclassified table — one
/// that is not `Carried`, not `Derived`, not a `Credential`, and not named as
/// `Pending` anywhere — must break the guard, or it is only checking that the
/// list is non-empty.
#[test]
fn the_guard_would_catch_an_unclassified_table() {
    let (_, first_migration) = MIGRATIONS[0];
    let sql_with_an_extra_table = format!(
        "{first_migration}\nCREATE TABLE a_table_nobody_classified (\n    id TEXT PRIMARY KEY\n) STRICT;\n"
    );
    assert_ne!(
        sql_with_an_extra_table, first_migration,
        "the injected table must actually be new text"
    );

    let mut found = BTreeSet::new();
    for line in sql_with_an_extra_table.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("CREATE TABLE ") {
            let name = rest.split_once('(').map_or(rest, |(name, _)| name).trim();
            found.insert(name.to_owned());
        }
    }
    assert!(
        found.contains("a_table_nobody_classified"),
        "the parser did not see the injected table at all: {found:?}"
    );

    let classified: BTreeSet<String> = TABLE_DISPOSITIONS
        .iter()
        .map(|(table, _)| (*table).to_owned())
        .collect();
    assert_ne!(
        found, classified,
        "an unclassified table must break the guard, or TABLE_DISPOSITIONS is not actually \
         being compared against the schema"
    );
}
