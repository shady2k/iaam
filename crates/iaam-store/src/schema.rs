//! Migrations.
//!
//! Migrations are numbered and applied one at a time in a transaction. The schema file
//! is embedded in the binary: a database opened by this program version must match this version, not whatever is on disk beside it.
//! correspond to this version, rather than what is nearby on disk.

use rusqlite::Connection;

use crate::StoreError;

/// Schema version understood by this build.
pub const SCHEMA_VERSION: u32 = 18;

const MIGRATIONS: [(u32, &str); 18] = [
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
    (18, include_str!("../migrations/0018_import_sessions.sql")),
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
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"
        ))?;
    }
    Ok(())
}
