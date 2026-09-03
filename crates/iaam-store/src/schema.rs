//! Migrations.
//!
//! Migrations are numbered and applied one at a time in a transaction. The schema file
//! is embedded in the binary: a database opened by this program version must match this version, not whatever is on disk beside it.
//! correspond to this version, rather than what is nearby on disk.

use rusqlite::Connection;

use crate::StoreError;

/// Schema version understood by this build.
pub const SCHEMA_VERSION: u32 = 20;

const MIGRATIONS: [(u32, &str); 20] = [
    (1, include_str!("../migrations/0001_initial.sql")),
    (2, include_str!("../migrations/0002_sources_and_rules.sql")),
    (3, include_str!("../migrations/0003_broker_access.sql")),
    (4, include_str!("../migrations/0004_broker_environment.sql")),
    (
        5,
        include_str!("../migrations/0005_instrument_reference.sql"),
    ),
    (
        6,
        include_str!("../migrations/0006_market_observations.sql"),
    ),
    (
        7,
        include_str!("../migrations/0007_executability_without_stale.sql"),
    ),
    (8, include_str!("../migrations/0008_quotation_basis.sql")),
    (
        9,
        include_str!("../migrations/0009_broker_operation_kinds.sql"),
    ),
    (10, include_str!("../migrations/0010_bond_schedule.sql")),
    (11, include_str!("../migrations/0011_accrued_interest.sql")),
    (
        12,
        include_str!("../migrations/0012_account_scoped_source_operation.sql"),
    ),
    (13, include_str!("../migrations/0013_event_source_time.sql")),
    (
        14,
        include_str!("../migrations/0014_securities_transfer_kinds.sql"),
    ),
    (15, include_str!("../migrations/0015_categories.sql")),
    (
        16,
        include_str!("../migrations/0016_category_group_is_income.sql"),
    ),
    (
        17,
        include_str!("../migrations/0017_account_scope_dispositions.sql"),
    ),
    (
        18,
        include_str!("../migrations/0018_account_transfer_partners.sql"),
    ),
    (19, include_str!("../migrations/0019_import_sessions.sql")),
    (
        20,
        include_str!("../migrations/0020_account_external_identity.sql"),
    ),
];

/// Apply missing migrations.
///
/// The database is newer than the program—reject it rather than attempt to operate: an unknown
/// column is silently read as absent, which is the worst kind of error.
pub fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    for (version, sql) in MIGRATIONS {
        if version <= current {
            continue;
        }
        apply(conn, version, sql)?;
    }
    Ok(())
}

/// Apply one migration, unless another connection got there first.
///
/// The version read at the top of `migrate` is only a hint: between that read and this
/// call another process—the server starting, a CLI command run a moment later—may have
/// applied the very same migration. So the transaction opens with `BEGIN IMMEDIATE`,
/// taking the write lock at once rather than at the first write, and the version is read
/// again inside it. The loser of the race then sees the work already done and commits
/// nothing, instead of failing halfway through with `table … already exists`.
fn apply(conn: &Connection, version: u32, sql: &str) -> Result<(), StoreError> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let outcome = apply_inside_transaction(conn, version, sql);
    match outcome {
        Ok(true) => conn.execute_batch("COMMIT;")?,
        Ok(false) | Err(_) => {
            // Rolling back a transaction SQLite has already unwound itself is harmless,
            // and leaving one open would poison every later statement on this connection.
            let _ = conn.execute_batch("ROLLBACK;");
            outcome?;
        }
    }
    Ok(())
}

/// Runs the migration and reports whether anything was written.
fn apply_inside_transaction(
    conn: &Connection,
    version: u32,
    sql: &str,
) -> Result<bool, StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current >= version {
        return Ok(false);
    }
    conn.execute_batch(&format!("{sql} PRAGMA user_version = {version};"))?;
    Ok(true)
}
