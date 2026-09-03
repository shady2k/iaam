//! Migration 0020 over accounts that all carry no external identity.

mod common;

use common::apply_migrations_through;
use rusqlite::{Connection, params};

const OWNER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const MAIN: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const SAVINGS: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

fn database_at_version_nineteen() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 19);
    for (id, title) in [(MAIN, "Main"), (SAVINGS, "Savings")] {
        conn.execute(
            "INSERT INTO accounts (id, owner, title, institution, created_at)
             VALUES (?1, ?2, ?3, NULL, '2026-02-01T00:00:00Z')",
            params![id, OWNER, title],
        )
        .expect("pre-existing account");
    }
    conn
}

#[test]
fn the_migration_invents_no_identity_for_an_account_that_had_none() {
    let conn = database_at_version_nineteen();

    iaam_store::schema::migrate(&conn).expect("migration 0020");

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);

    let identified: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM accounts
             WHERE provider IS NOT NULL OR provider_account_id IS NOT NULL
                OR cash_class IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("identified accounts");
    assert_eq!(
        identified, 0,
        "an account that carried no identity must not be given one"
    );

    let kept: u32 = conn
        .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
        .expect("account count");
    assert_eq!(kept, 2, "the migration must keep every pre-existing row");
}

#[test]
fn the_uniqueness_constraint_tolerates_absence() {
    // Two accounts carrying no identity are two accounts. If the index bound
    // rows with NULL columns, the second insert would be refused and every
    // database written before decision 0004 would fail to migrate.
    let conn = database_at_version_nineteen();
    iaam_store::schema::migrate(&conn).expect("migration 0020");

    conn.execute(
        "INSERT INTO accounts (id, owner, title, institution, created_at)
         VALUES ('dddddddd-dddd-4ddd-8ddd-dddddddddddd', ?1, 'Wallet', NULL,
                 '2026-02-01T00:00:00Z')",
        params![OWNER],
    )
    .expect("a third account without an identity");
}

#[test]
fn one_identity_cannot_be_written_twice_for_one_owner() {
    let conn = database_at_version_nineteen();
    iaam_store::schema::migrate(&conn).expect("migration 0020");

    conn.execute(
        "UPDATE accounts SET provider = 'bank-one', provider_account_id = 'opaque-1'
         WHERE id = ?1",
        params![MAIN],
    )
    .expect("first identity");

    let refused = conn.execute(
        "UPDATE accounts SET provider = 'bank-one', provider_account_id = 'opaque-1'
         WHERE id = ?1",
        params![SAVINGS],
    );

    assert!(
        refused.is_err(),
        "(owner, provider, provider_account_id) must be unique"
    );
}
