//! Exact `(table, column)` coverage guards over `0001_schema.sql`.
//!
//! Each guard here answers one question: does an exhaustive traversal in
//! the domain still see every foreign key the schema declares? Substring or
//! table-name matching cannot answer that honestly — a table already named
//! can grow a second such column and the guard would not notice — so
//! [`columns_referencing`] returns exact `(table, column)` pairs, and every
//! test in this file compares exact sets.
//!
//! Custody is checked below. A later task adds the same shape of guard for
//! `REFERENCES instruments`, sharing [`columns_referencing`] with a
//! different `target` and a different covered list; nothing here is written
//! to make that harder.

use std::collections::BTreeSet;

use iaam_store::journal::CUSTODY_REFERENCE_COLUMNS;

const SCHEMA: &str = include_str!("../migrations/0001_schema.sql");

/// Every `(table, column)` pair, among tables whose name starts with
/// `event`, whose column declaration contains `REFERENCES <target>`.
///
/// Parses by tracking the table a `CREATE TABLE <name> (` line opens and the
/// `) STRICT;` line that closes it — the schema's own consistent style
/// (`crates/iaam-store/migrations/0001_schema.sql`, every `CREATE TABLE`
/// closes exactly that way) — rather than a general SQL parser, because
/// nothing here needs one.
fn columns_referencing(sql: &str, target: &str) -> BTreeSet<(String, String)> {
    let reference = format!("REFERENCES {target}");
    let mut found = BTreeSet::new();
    let mut current_table: Option<String> = None;

    for line in sql.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("CREATE TABLE ") {
            let name = rest
                .split_once('(')
                .map_or(rest, |(name, _)| name)
                .trim()
                .to_owned();
            current_table = Some(name);
            continue;
        }
        if trimmed.starts_with(") STRICT;") {
            current_table = None;
            continue;
        }
        let Some(table) = &current_table else {
            continue;
        };
        if !table.starts_with("event") || !trimmed.contains(&reference) {
            continue;
        }
        let Some(column) = trimmed.split_whitespace().next() else {
            continue;
        };
        found.insert((table.clone(), column.to_owned()));
    }
    found
}

#[test]
fn event_tables_referencing_custody_places_match_the_covered_list() {
    let schema_pairs = columns_referencing(SCHEMA, "custody_places");
    let covered: BTreeSet<(String, String)> = CUSTODY_REFERENCE_COLUMNS
        .iter()
        .map(|(table, column)| ((*table).to_owned(), (*column).to_owned()))
        .collect();

    assert_eq!(
        schema_pairs, covered,
        "0001_schema.sql declares a REFERENCES custody_places column on an \
         event* table that CUSTODY_REFERENCE_COLUMNS does not list (or vice \
         versa) — Event::referenced_custodies must cover exactly this set"
    );
}

/// Proves the comparison above is not vacuous: exact-pair matching catches
/// a second `REFERENCES custody_places` column landing on a table the
/// covered list already names, which a substring or table-name check would
/// have missed.
#[test]
fn the_guard_would_catch_an_uncovered_custody_column() {
    let sql_with_an_extra_column = SCHEMA.replace(
        "CREATE TABLE event_control_assertion (",
        "CREATE TABLE event_control_assertion (\n    \
         second_custody TEXT REFERENCES custody_places (id),",
    );
    assert_ne!(
        sql_with_an_extra_column, SCHEMA,
        "the replacement above must actually have matched something"
    );

    let schema_pairs = columns_referencing(&sql_with_an_extra_column, "custody_places");
    let covered: BTreeSet<(String, String)> = CUSTODY_REFERENCE_COLUMNS
        .iter()
        .map(|(table, column)| ((*table).to_owned(), (*column).to_owned()))
        .collect();

    assert!(
        schema_pairs.contains(&(
            "event_control_assertion".to_owned(),
            "second_custody".to_owned()
        )),
        "the parser did not see the injected column at all: {schema_pairs:?}"
    );
    assert_ne!(
        schema_pairs, covered,
        "an uncovered custody column on an already-listed table must break \
         the guard, or the guard is only checking table names"
    );
}
