//! The retirement history: an append-only second axis (`iaam-gua5`).
//!
//! Nothing here is derived from any real export: the accounts, the dates and
//! the owner are invented (CLAUDE.md, "Conventions & Patterns").

mod common;

use common::apply_migrations_through;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::retirement::{AccountRetirement, RetirementRevision};
use iaam_store::SqliteStore;
use iaam_store::reference::{AccountRecord, AccountRetirementsRecord};
use rusqlite::{Connection, params};
use time::macros::date;

const OWNER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ACCOUNT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

fn database_at_version_twenty_four() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 24);
    conn.execute(
        "INSERT INTO accounts (id, owner, title, institution, created_at)
         VALUES (?1, ?2, 'Term', 'Northline', '2026-01-01T00:00:00Z')",
        params![ACCOUNT, OWNER],
    )
    .expect("pre-existing account");
    conn
}

/// An account that existed before the declaration did carries no retirement,
/// and the migration invents none.
///
/// The absence of a row is «he has not said», the state every account starts
/// in. A back-filled date would be the system asserting on his behalf that a
/// product had ceased — and it would suppress the account's row in every asset
/// snapshot from that day, which is exactly the silent change the revision
/// coordinate exists to prevent.
#[test]
fn the_migration_retires_nothing_that_existed_before_it() {
    let conn = database_at_version_twenty_four();

    iaam_store::schema::migrate(&conn).expect("migration 0025");

    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);

    let declared: u32 = conn
        .query_row("SELECT COUNT(*) FROM account_retirements", [], |row| {
            row.get(0)
        })
        .expect("retirement count");
    assert_eq!(declared, 0);
}

/// A retirement is written once and never edited or deleted.
///
/// The triggers are the schema's half of the promise the revision coordinate
/// makes. A ban on `UPDATE` alone would catch an edited row and let
/// `DELETE` + `INSERT` through, and the result is the same: a revision an
/// already-published report named now says something else.
#[test]
fn a_recorded_retirement_can_be_neither_edited_nor_deleted() {
    let conn = database_at_version_twenty_four();
    iaam_store::schema::migrate(&conn).expect("migration 0025");
    conn.execute(
        "INSERT INTO account_retirements (owner, revision, account, effective_on, recorded_at)
         VALUES (?1, 1, ?2, '2026-03-10', '2026-03-20T00:00:00Z')",
        params![OWNER, ACCOUNT],
    )
    .expect("first retirement");

    assert!(
        conn.execute(
            "UPDATE account_retirements SET effective_on = '2026-01-01' WHERE revision = 1",
            [],
        )
        .is_err(),
        "a retirement was edited in place"
    );
    assert!(
        conn.execute("DELETE FROM account_retirements WHERE revision = 1", [])
            .is_err(),
        "a retirement was deleted"
    );
}

fn owned(store: &mut SqliteStore, owner: OwnerId, title: &str) -> AccountId {
    let account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: title.into(),
            institution: None,
        })
        .expect("account");
    account
}

/// The whole life of the declaration through the store: declare, read back,
/// withdraw, declare again — each of them a revision, and the history kept.
#[test]
fn a_retirement_is_a_revision_and_a_withdrawal_is_another() {
    let mut store = SqliteStore::open_in_memory().expect("store");
    let owner = OwnerId::new_random();
    let other_owner = OwnerId::new_random();
    let term = owned(&mut store, owner, "Term");
    let savings = owned(&mut store, owner, "Savings");
    let foreign = owned(&mut store, other_owner, "Main");

    // An owner who has declared nothing is at revision zero, which is a real
    // coordinate and not a missing one.
    assert_eq!(
        store.list_account_retirements(owner).expect("read"),
        AccountRetirementsRecord {
            revision: RetirementRevision::NONE,
            statements: Vec::new(),
        }
    );

    let first = store
        .record_account_retirement(
            owner,
            &AccountRetirement {
                account: term,
                effective_on: date!(2026 - 03 - 10),
            },
        )
        .expect("first retirement");
    assert_eq!(first, RetirementRevision(1));
    assert_eq!(
        store.list_account_retirements(owner).expect("read"),
        AccountRetirementsRecord {
            revision: RetirementRevision(1),
            statements: vec![AccountRetirement {
                account: term,
                effective_on: date!(2026 - 03 - 10),
            }],
        }
    );

    // A withdrawal is a further row, so the account leaves the set in force
    // while the row that declared it stays where it was.
    let withdrawn = store
        .withdraw_account_retirement(owner, term)
        .expect("withdrawal");
    assert_eq!(withdrawn, RetirementRevision(2));
    assert_eq!(
        store.list_account_retirements(owner).expect("read"),
        AccountRetirementsRecord {
            revision: RetirementRevision(2),
            statements: Vec::new(),
        }
    );

    // And it can be declared again, on a different date. Filtering the history
    // on «has an effective date» alone would have resurrected the first one.
    let again = store
        .record_account_retirement(
            owner,
            &AccountRetirement {
                account: term,
                effective_on: date!(2026 - 04 - 01),
            },
        )
        .expect("second retirement");
    assert_eq!(again, RetirementRevision(3));
    store
        .record_account_retirement(
            owner,
            &AccountRetirement {
                account: savings,
                effective_on: date!(2026 - 02 - 01),
            },
        )
        .expect("a second account retired");
    let held = store.list_account_retirements(owner).expect("read");
    assert_eq!(held.revision, RetirementRevision(4));
    assert_eq!(
        held.statements.len(),
        2,
        "one statement per account in force: {held:?}"
    );
    assert!(
        held.statements.contains(&AccountRetirement {
            account: term,
            effective_on: date!(2026 - 04 - 01),
        }),
        "the newest statement is the one in force: {held:?}"
    );

    // The history is still there, four rows deep, and nothing rewrote it.
    let rows: u32 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM account_retirements", [], |row| {
            row.get(0)
        })
        .expect("history");
    assert_eq!(rows, 4);

    // Another owner sees none of it, and the foreign key refuses a statement
    // about an account this owner does not hold: an identifier is not an access
    // right.
    assert_eq!(
        store
            .list_account_retirements(other_owner)
            .expect("read")
            .statements,
        Vec::new()
    );
    assert!(
        store
            .record_account_retirement(
                owner,
                &AccountRetirement {
                    account: foreign,
                    effective_on: date!(2026 - 03 - 10),
                },
            )
            .is_err()
    );
}
