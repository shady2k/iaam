//! ADR 0042 point 7: a migration killed inside its transaction leaves the
//! previous version, and the next `migrate` completes it.
//!
//! The invented migration is the test's own: the real list is read, never
//! changed, and version 2 below exists only in this file.

use std::path::PathBuf;

use iaam_store::fault;
use iaam_store::schema::{MIGRATIONS, migrate_with};
use rusqlite::Connection;
use uuid::Uuid;

const INVENTED_MIGRATION: &str = "CREATE TABLE fault_invented (value TEXT);";
const INVENTED_VERSION: u32 = 2;

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("iaam-store-fault-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temporary directory");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn migrations() -> [(u32, &'static str); 2] {
    [
        (MIGRATIONS[0].0, MIGRATIONS[0].1),
        (INVENTED_VERSION, INVENTED_MIGRATION),
    ]
}

fn user_version(conn: &Connection) -> u32 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema version")
}

fn invented_table_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'fault_invented')",
        [],
        |row| row.get(0),
    )
    .expect("schema lookup")
}

#[test]
fn killed_migration_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let database = std::env::var_os(fault::DATABASE_VARIABLE).expect("database configured");
    let conn = Connection::open(PathBuf::from(database)).expect("child opens the database");
    migrate_with(&conn, &migrations()).expect("the interrupted migration stops at the barrier");
}

#[test]
fn a_migration_killed_inside_its_transaction_leaves_the_previous_version_and_migrate_completes() {
    let directory = TempDirectory::new("migration");
    let database = directory.file("iaam.sqlite");

    let conn = Connection::open(&database).expect("create the database");
    migrate_with(&conn, &[migrations()[0]]).expect("the first migration applies");
    assert_eq!(user_version(&conn), 1, "the database starts at version 1");
    assert!(
        !invented_table_exists(&conn),
        "the invented table is absent"
    );
    drop(conn);

    let marker = directory.file("barrier-reached");
    fault::kill_child_at(
        "killed_migration_child",
        fault::MIGRATION_INSIDE_TRANSACTION,
        None,
        &marker,
        &database,
        &[],
    );
    assert!(marker.exists(), "the child was killed inside the migration");

    let conn = Connection::open(&database).expect("the killed database reopens");
    assert_eq!(
        user_version(&conn),
        1,
        "a kill inside the migration's transaction leaves the previous version"
    );
    assert!(
        !invented_table_exists(&conn),
        "no part of the interrupted migration is left"
    );

    migrate_with(&conn, &migrations()).expect("the next migrate completes the interrupted one");
    assert_eq!(user_version(&conn), INVENTED_VERSION);
    assert!(
        invented_table_exists(&conn),
        "the completed migration is there"
    );
}
