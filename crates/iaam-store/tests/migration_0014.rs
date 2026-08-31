//! Data migration for securities-transfer operation kinds.

mod common;

use common::apply_migrations_through;
use rusqlite::{Connection, params};

fn database_at_version_thirteen() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory database");
    apply_migrations_through(&conn, 13);
    conn
}

fn insert_kind(conn: &Connection, source_kind: &str, kind: &str, origin: &str) {
    conn.execute(
        "INSERT INTO broker_operation_kinds (
             broker, source_kind, kind, origin, dictionary, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "tinkoff",
            source_kind,
            kind,
            origin,
            (origin == "contract").then_some("operations.proto@2026-08"),
            "2026-08-31T00:00:00Z",
        ],
    )
    .expect("pre-existing dictionary row");
}

fn read_kind(conn: &Connection, source_kind: &str) -> (String, String) {
    conn.query_row(
        "SELECT kind, origin FROM broker_operation_kinds
         WHERE broker = 'tinkoff' AND source_kind = ?1",
        [source_kind],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("dictionary row")
}

#[test]
fn securities_transfer_migration_rewrites_contract_and_owner_rows() {
    for origin in ["contract", "owner"] {
        let conn = database_at_version_thirteen();
        insert_kind(&conn, "OPERATION_TYPE_INPUT_SECURITIES", "deposit", origin);

        iaam_store::schema::migrate(&conn).expect("migration 0014");

        assert_eq!(
            read_kind(&conn, "OPERATION_TYPE_INPUT_SECURITIES"),
            ("securities_transfer_in".to_owned(), origin.to_owned())
        );
    }
}

#[test]
fn securities_transfer_migration_rewrites_output_regardless_of_origin() {
    let conn = database_at_version_thirteen();
    insert_kind(
        &conn,
        "OPERATION_TYPE_OUTPUT_SECURITIES",
        "withdrawal",
        "owner",
    );

    iaam_store::schema::migrate(&conn).expect("migration 0014");

    assert_eq!(
        read_kind(&conn, "OPERATION_TYPE_OUTPUT_SECURITIES"),
        ("securities_transfer_out".to_owned(), "owner".to_owned())
    );
}

#[test]
fn securities_transfer_migration_leaves_unrelated_owner_rows_untouched() {
    let conn = database_at_version_thirteen();
    insert_kind(&conn, "OPERATION_TYPE_INPUT_SECURITIES", "deposit", "owner");
    insert_kind(&conn, "OPERATION_TYPE_INPUT", "deposit", "owner");

    iaam_store::schema::migrate(&conn).expect("migration 0014");

    assert_eq!(
        read_kind(&conn, "OPERATION_TYPE_INPUT"),
        ("deposit".to_owned(), "owner".to_owned())
    );
}
