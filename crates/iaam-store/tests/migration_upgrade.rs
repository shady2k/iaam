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

use iaam_core::ids::{AccountId, OwnerId};
use iaam_store::SqliteStore;
use iaam_store::schema::{SCHEMA_VERSION, migrate};
use iaam_store::sync_refusals::{SyncRefusalFilter, SyncRefusalRecord};
use uuid::Uuid;

fn rewind_tax_and_redemption_columns(conn: &rusqlite::Connection) {
    conn.execute_batch("DROP TABLE event_securities_transfer_out;")
        .expect("remove version-15 detail table");
    conn.execute_batch("ALTER TABLE event_tax DROP COLUMN instrument;")
        .expect("remove the version-13 tax instrument");
    let legacy = include_str!("../migrations/0001_schema.sql");
    let start = legacy
        .find("CREATE TABLE event_corporate_action (")
        .unwrap();
    let end = start + legacy[start..].find(") STRICT;").unwrap() + ") STRICT;".len();
    let create = legacy[start..end].replace(
        "CREATE TABLE event_corporate_action (",
        "CREATE TABLE event_corporate_action_old (",
    );
    conn.execute_batch(&create)
        .expect("restore the pre-version-11 corporate-action shape");
    conn.execute_batch(
        "INSERT INTO event_corporate_action_old (
             event, action_kind, instrument, predecessor, successor, custody,
             quantity, quantity_in, quantity_out, ratio,
             principal_returned_per_unit_value, principal_returned_per_unit_currency,
             fractional, basis_transfer, effective_date, record_date, grounds,
             allocation_kind, allocation_gap, allocation_share,
             allocation_inputs_hash, allocation_known_as_of, allocation_algorithm
         ) SELECT
             event, action_kind, instrument, predecessor, successor, custody,
             quantity, quantity_in, quantity_out, ratio,
             principal_returned_per_unit_value, principal_returned_per_unit_currency,
             fractional, basis_transfer, effective_date, record_date, grounds,
             allocation_kind, allocation_gap, allocation_share,
             allocation_inputs_hash, allocation_known_as_of, allocation_algorithm
         FROM event_corporate_action;
         DROP TABLE event_corporate_action;
         ALTER TABLE event_corporate_action_old RENAME TO event_corporate_action;",
    )
    .expect("remove the version-14 posted-principal columns");
}

#[test]
fn version_fourteen_upgrade_preserves_journal_and_foreign_keys() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for (version, sql) in iaam_store::schema::MIGRATIONS {
        if version <= 14 {
            conn.execute_batch(sql).unwrap();
        }
    }
    conn.execute_batch(
        "PRAGMA user_version = 14;
         PRAGMA foreign_keys = ON;
         INSERT INTO accounts (id, owner, title, created_at)
         VALUES ('a', 'o', 'Account', '2026-01-01T00:00:00Z');
         INSERT INTO events (id, owner, account, kind, effective_date, sequence, confidence, relation_kind, recorded_at, source, raw_hash, parser_version)
         VALUES ('e', 'o', 'a', 'cash_in', '2026-01-02', 1, 'known', 'none', '2026-01-02T00:00:00Z', 'file', 'hash', 'v1');
         INSERT INTO event_legs (event, ordinal, kind, account, amount, currency)
         VALUES ('e', 0, 'cash', 'a', 100, 'RUB');"
    ).unwrap();
    migrate(&conn).expect("upgrade populated version fourteen");
    let amount: i64 = conn
        .query_row(
            "SELECT amount FROM event_legs WHERE event = 'e'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(amount, 100);
    let violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

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

fn has_account_retractions_table(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT account FROM account_retractions LIMIT 0")
        .is_ok()
}

fn has_contour_report_defaults_table(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT contour FROM contour_report_defaults LIMIT 0")
        .is_ok()
}

fn has_broker_account_bindings_table(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT broker_account FROM broker_account_bindings LIMIT 0")
        .is_ok()
}

fn has_broker_egress_table(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT setting FROM broker_egress LIMIT 0")
        .is_ok()
}

fn has_sync_refusals_table(store: &SqliteStore) -> bool {
    store
        .connection()
        .prepare("SELECT row_key FROM sync_refusals LIMIT 0")
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

    rewind_tax_and_redemption_columns(store.connection());
    store
        .connection()
        .execute_batch(
            "ALTER TABLE events DROP COLUMN source_counterparty; \
             DROP TABLE event_stated_securities_value; \
             ALTER TABLE accounts DROP COLUMN declared_by; \
             DROP TABLE account_retractions; \
             DROP TABLE contour_report_defaults; \
             DROP TABLE broker_account_bindings; \
             DROP TABLE broker_egress; \
             DROP TABLE sync_refusals; \
             ALTER TABLE instruments DROP COLUMN metadata; \
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
    assert!(
        !has_account_retractions_table(&store),
        "and this table too, or migration 0005 is not actually being exercised"
    );
    assert!(
        !has_contour_report_defaults_table(&store),
        "and this table too, or migration 0006 is not actually being exercised"
    );
    assert!(
        !has_broker_account_bindings_table(&store),
        "and this table too, or migration 0007 is not actually being exercised"
    );
    assert!(
        !has_broker_egress_table(&store),
        "and this table too, or migration 0008 is not actually being exercised"
    );
    assert!(
        !has_sync_refusals_table(&store),
        "and this table too, or migration 0009 is not actually being exercised"
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
    assert!(
        has_account_retractions_table(&store),
        "and it must gain the table 0005 adds, for the same reason"
    );
    assert!(
        has_contour_report_defaults_table(&store),
        "and the table 0006 adds, for the same reason"
    );
    assert!(
        has_broker_account_bindings_table(&store),
        "and the table 0007 adds, for the same reason"
    );
    assert!(
        has_broker_egress_table(&store),
        "and the table 0008 adds, for the same reason"
    );
    assert!(
        has_sync_refusals_table(&store),
        "and the table 0009 adds, for the same reason"
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
    // this time over a journal that is not empty. `accounts.declared_by`,
    // `account_retractions`, `contour_report_defaults`,
    // `broker_account_bindings`, `broker_egress` and `sync_refusals` are
    // dropped too: `migrate` still catches this database up to the current
    // `SCHEMA_VERSION` afterwards, and migrations 0004 to 0009 would otherwise
    // find the column and the tables already there from the initial
    // `open_in_memory` and fail on a name that already exists.
    rewind_tax_and_redemption_columns(connection);
    connection
        .execute_batch(
            "DROP TABLE event_stated_securities_value; \
             ALTER TABLE accounts DROP COLUMN declared_by; \
             DROP TABLE account_retractions; \
             DROP TABLE contour_report_defaults; \
             DROP TABLE broker_account_bindings; \
             DROP TABLE broker_egress; \
             DROP TABLE sync_refusals; \
             ALTER TABLE instruments DROP COLUMN metadata; \
             PRAGMA user_version = 2;",
        )
        .expect("winding the database back to version 2");
    assert!(
        !has_sync_refusals_table(&store),
        "the sync_refusals table must be gone too, or migration 0009 is not actually being exercised"
    );

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

/// A database built at version 8 — before `sync_refusals` existed — gains the
/// table and its methods work on it.
///
/// The sync-refusals table was first appended to `0001_schema.sql`, which
/// every database already at version 1 skips: an existing instance would
/// never re-apply it, and the first `settle_sync_refusals_besides` would
/// fail with "no such table" even when nothing was refused
/// (iaam-vg8te.1.2). A database that build never sees again is produced the
/// way every migration test here produces one: current database, the new
/// migration's table dropped, the version wound back to where that build
/// left it.
#[test]
fn a_version_eight_database_gains_the_sync_refusals_table() {
    let store = SqliteStore::open_in_memory().expect("open");
    assert!(
        has_sync_refusals_table(&store),
        "a database this build creates has the table, or the test is not about a fresh one"
    );

    // The shape a version-8 build actually leaves behind: everything of
    // migrations 1 to 8, nothing of 0009, and the version number it wrote.
    rewind_tax_and_redemption_columns(store.connection());
    store
        .connection()
        .execute_batch(
            "DROP TABLE sync_refusals; \
             ALTER TABLE instruments DROP COLUMN metadata; \
             PRAGMA user_version = 8;",
        )
        .expect("winding the database back to version 8");
    assert!(
        !has_sync_refusals_table(&store),
        "the table must be gone, or the upgrade is not actually being exercised"
    );
    assert_eq!(
        user_version(&store),
        8,
        "the wound-back database must claim exactly the version of the last migration it has"
    );

    migrate(store.connection()).expect("migrating the version-8 database");

    assert!(
        has_sync_refusals_table(&store),
        "an existing database must gain the table, not be expected to have had it all along"
    );
    assert_eq!(
        user_version(&store),
        SCHEMA_VERSION,
        "it must be left at this build's version, so the next migration is not reapplied"
    );

    // The store's own methods work over the migrated table: a refusal is
    // recorded, listed as the owner's question, and settled again by a later
    // sync that covers its whole interval and does not list it.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let record = SyncRefusalRecord {
        id: Uuid::new_v4(),
        owner,
        account,
        source: "finam".to_owned(),
        row_key: "row-1".to_owned(),
        range_from: "2026-03-01".to_owned(),
        range_to: "2026-03-31".to_owned(),
        dimensions: "positions".to_owned(),
        reason: "no instrument carries ISIN RU000AFIXTUR".to_owned(),
        payload: r#"{"symbol":"FIXT@MISX"}"#.to_owned(),
        settled: false,
    };
    store
        .upsert_sync_refusal(&record)
        .expect("the migrated table records a refusal");
    let listed = store
        .list_open_sync_refusals(owner, account, "finam", "2026-03-01", "2026-03-31")
        .expect("the migrated table lists open refusals");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].reason, "no instrument carries ISIN RU000AFIXTUR");

    store
        .settle_sync_refusals_besides(
            &SyncRefusalFilter {
                owner,
                account,
                source: "finam".to_owned(),
                from: "2026-03-01".to_owned(),
                to: "2026-03-31".to_owned(),
            },
            &[],
        )
        .expect("the migrated table settles covered refusals");
    assert!(
        store
            .list_open_sync_refusals(owner, account, "finam", "2026-03-01", "2026-03-31")
            .expect("list after settling")
            .is_empty(),
        "the refusal settled through the migrated table"
    );
}

#[test]
fn version_ten_redemption_rows_survive_optional_custody_migration() {
    let store = SqliteStore::open_in_memory().expect("open");
    let conn = store.connection();
    rewind_tax_and_redemption_columns(conn);
    conn.execute_batch(
        "INSERT INTO accounts (id, owner, title, created_at) VALUES ('a', 'o', 'Account', '2026-01-01T00:00:00Z');
         INSERT INTO instruments (id, kind, symbol, title, denomination_currency, settlement_currency, quote_currency, created_at) VALUES ('i', 'bond', 'BOND', 'Bond', 'RUB', 'RUB', 'RUB', '2026-01-01T00:00:00Z');
         INSERT INTO custody_places (id, owner, title, origin, created_at) VALUES ('c', 'o', 'Custody', 'declared', '2026-01-01T00:00:00Z');
         INSERT INTO events (id, owner, account, kind, effective_date, sequence, confidence, relation_kind, recorded_at, source, raw_hash, parser_version)
         VALUES ('e', 'o', 'a', 'corporate_action', '2026-01-02', 1, 'known', 'none', '2026-01-02T00:00:00Z', 'file', 'hash', 'v1');
         INSERT INTO event_corporate_action (event, action_kind, instrument, custody, quantity, principal_returned_per_unit_value, principal_returned_per_unit_currency, effective_date)
         VALUES ('e', 'redemption', 'i', 'c', '10', '1000', 'RUB', '2026-01-02');
         PRAGMA user_version = 10;"
    ).unwrap();
    migrate(conn).expect("migrate populated version ten");
    let custody: Option<String> = conn
        .query_row(
            "SELECT custody FROM event_corporate_action WHERE event = 'e'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(custody.as_deref(), Some("c"));
    conn.execute(
        "UPDATE event_corporate_action SET custody = NULL WHERE event = 'e'",
        [],
    )
    .expect("redemption accepts no custody");
    assert!(
        conn.execute(
            "UPDATE event_corporate_action SET action_kind = 'conversion' WHERE event = 'e'",
            []
        )
        .is_err()
    );
    assert!(conn.execute(
        "UPDATE event_corporate_action SET principal_returned_per_unit_value = NULL, principal_returned_per_unit_currency = NULL WHERE event = 'e'", [],
    ).is_err(), "neither principal representation is invalid");
    assert!(conn.execute(
        "UPDATE event_corporate_action SET posted_principal_minor = 100, posted_principal_currency = 'RUB' WHERE event = 'e'", [],
    ).is_err(), "both principal representations are invalid");
    conn.execute(
        "UPDATE event_corporate_action SET quantity = '3', principal_returned_per_unit_value = NULL, principal_returned_per_unit_currency = NULL, posted_principal_minor = 100, posted_principal_currency = 'RUB' WHERE event = 'e'", [],
    ).expect("posted principal is preserved without a rounded quotient");
    let posted: i64 = conn
        .query_row(
            "SELECT posted_principal_minor FROM event_corporate_action WHERE event = 'e'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(posted, 100);
    assert_eq!(user_version(&store), SCHEMA_VERSION);
}

#[test]
fn version_eleven_income_rows_and_rule_history_survive_the_widened_checks() {
    use rusqlite::{Connection, types::Value};

    fn rows(conn: &Connection, table: &str) -> Vec<Vec<Value>> {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    let conn = Connection::open_in_memory().unwrap();
    for (version, sql) in iaam_store::schema::MIGRATIONS {
        if version <= 11 {
            conn.execute_batch(sql).unwrap();
        }
    }
    conn.execute_batch(
        "PRAGMA user_version = 11;
         PRAGMA foreign_keys = ON;
         INSERT INTO accounts (id, owner, title, created_at)
         VALUES ('a', 'o', 'Account', '2026-01-01T00:00:00Z');
         INSERT INTO instruments (id, kind, symbol, title, denomination_currency, settlement_currency, quote_currency, created_at)
         VALUES ('i', 'bond', 'BOND', 'Bond', 'RUB', 'RUB', 'RUB', '2026-01-01T00:00:00Z');"
    ).unwrap();
    for (index, kind) in [
        None,
        Some("coupon"),
        Some("dividend"),
        Some("deposit_interest"),
    ]
    .into_iter()
    .enumerate()
    {
        let event = format!("e{index}");
        conn.execute(
            "INSERT INTO events (id, owner, account, kind, effective_date, sequence, confidence, relation_kind, recorded_at, source, raw_hash, parser_version)
             VALUES (?1, 'o', 'a', 'income', '2026-01-02', ?2, 'known', 'none', '2026-01-02T00:00:00Z', 'file', ?1, 'v1')",
            rusqlite::params![event, i64::try_from(index).unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO event_income (event, instrument, income_kind) VALUES (?1, 'i', ?2)",
            rusqlite::params![event, kind],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO classification_rules (id, owner, version, description_contains, outcome_kind, income_kind, created_at, retired_at, replaces)
             VALUES (?1, 'o', ?2, 'Income', 'income', ?3, '2026-01-01T00:00:00Z', ?4, ?5)",
            rusqlite::params![format!("r{index}"), i64::try_from(index + 1).unwrap(), kind,
                (index < 3).then_some("2026-01-02T00:00:00Z"),
                index.checked_sub(1).map(|previous| format!("r{previous}"))],
        ).unwrap();
    }
    let income_before = rows(&conn, "event_income");
    let rules_before = rows(&conn, "classification_rules");
    assert!(
        conn.execute(
            "UPDATE event_income SET income_kind = 'securities_lending_income'",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE classification_rules SET income_kind = 'securities_lending_income'",
            []
        )
        .is_err()
    );

    migrate(&conn).expect("migrate populated version eleven");

    assert_eq!(rows(&conn, "event_income"), income_before);
    assert_eq!(rows(&conn, "classification_rules"), rules_before);
    conn.execute(
        "UPDATE event_income SET income_kind = 'securities_lending_income' WHERE event = 'e0'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE classification_rules SET income_kind = 'securities_lending_income' WHERE id = 'r3'",
        [],
    )
    .unwrap();
    for table in ["event_income", "classification_rules"] {
        assert!(
            conn.execute(
                &format!("UPDATE {table} SET income_kind = 'unknown_kind'"),
                []
            )
            .is_err()
        );
    }
    assert!(
        conn.execute(
            "UPDATE classification_rules SET outcome_kind = 'external_flow' WHERE id = 'r3'",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE classification_rules SET description_contains = NULL WHERE id = 'r3'",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE classification_rules SET version = 1 WHERE id = 'r3'",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE event_income SET instrument = 'missing' WHERE event = 'e0'",
            []
        )
        .is_err()
    );
    let indexes: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN ('classification_rules_by_version', 'classification_rules_by_owner')", [], |row| row.get(0)).unwrap();
    assert_eq!(indexes, 2);
    let violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}
