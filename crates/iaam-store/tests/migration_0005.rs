//! Data migration for 0005.
//!
//! A non-empty database is required for the test: on an empty database, the data
//! migration is trivially correct, while it breaks precisely on existing
//! rows.

use rusqlite::Connection;

/// The portion of the version 4 schema affected by migration 0005.
fn database_at_version_four() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.execute_batch(
        "CREATE TABLE instruments (
             id       TEXT PRIMARY KEY,
             symbol   TEXT NOT NULL,
             title    TEXT NOT NULL,
             currency TEXT NOT NULL
         ) STRICT;
         INSERT INTO instruments (id, symbol, title, currency)
         VALUES ('11111111-1111-4111-8111-111111111111', 'SBER', 'Sberbank', 'RUB');
         PRAGMA user_version = 4;",
    )
    .expect("version 4 schema");
    conn
}

fn apply_migration_0005(conn: &Connection) {
    let sql = include_str!("../migrations/0005_instrument_reference.sql");
    conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = 5; COMMIT;"))
        .expect("migration 0005");
}

#[test]
fn an_existing_instrument_keeps_its_currency_in_all_three_roles() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let (denomination, settlement, quote): (String, String, String) = conn
        .query_row(
            "SELECT denomination_currency, settlement_currency, quote_currency
             FROM instruments WHERE symbol = 'SBER'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("migrated instrument");

    assert_eq!(denomination, "RUB");
    assert_eq!(settlement, "RUB");
    assert_eq!(quote, "RUB");
}

#[test]
fn an_existing_instrument_has_no_kind_guessed_for_it() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM instruments WHERE symbol = 'SBER'",
            [],
            |row| row.get(0),
        )
        .expect("migrated instrument");

    assert_eq!(
        kind, None,
        "lineage is unknown and must not be substituted with a stock"
    );
}

#[test]
fn overlapping_alias_intervals_are_refused_by_the_database() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);
    conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2020-01-01', '2024-01-01', 'manual', '2026-08-25T00:00:00Z');",
    )
    .expect("first interval");

    let overlapping = conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2023-06-01', NULL, 'manual', '2026-08-25T00:00:00Z');",
    );

    assert!(
        overlapping.is_err(),
        "overlapping intervals make resolution ambiguous"
    );
}

#[test]
fn adjacent_alias_intervals_are_allowed() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let adjacent = conn.execute_batch(
        "INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2020-01-01', '2024-01-01', 'manual', '2026-08-25T00:00:00Z');
         INSERT INTO instrument_aliases
             (namespace, value, instrument, valid_from, valid_to, source, created_at)
         VALUES ('isin', 'RU000A0JX0J2', '11111111-1111-4111-8111-111111111111',
                 '2024-01-01', NULL, 'manual', '2026-08-25T00:00:00Z');",
    );

    assert!(
        adjacent.is_ok(),
        "adjacent intervals join without a gap: the half-open interval's end is exclusive"
    );
}

/// The table is created under the name `instruments_new` and renamed,
/// so its own foreign key on `lineage_parent` points to `instruments_new` at creation time.
/// One must not silently rely on `ALTER TABLE ... RENAME TO` rewriting this self-reference:
/// behavior depends on `legacy_alter_table`, and a broken key cannot be detected
/// behavior depends on `legacy_alter_table`, and a broken key in no way
/// will not surface until the first replacement bond appears —
/// that is, months after the migration.
#[test]
fn the_self_reference_survives_the_rename() {
    let conn = database_at_version_four();
    apply_migration_0005(&conn);

    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'instruments'",
            [],
            |row| row.get(0),
        )
        .expect("table definition");

    assert!(
        !ddl.contains("instruments_new"),
        "foreign key still points to the intermediate table name: {ddl}"
    );
}
