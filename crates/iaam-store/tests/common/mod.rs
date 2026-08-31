//! Shared setup for migration integration tests.

use rusqlite::Connection;

/// Build a database by applying the production migrations through `through`.
pub fn apply_migrations_through(conn: &Connection, through: u32) {
    let migrations = [
        include_str!("../../migrations/0001_initial.sql"),
        include_str!("../../migrations/0002_sources_and_rules.sql"),
        include_str!("../../migrations/0003_broker_access.sql"),
        include_str!("../../migrations/0004_broker_environment.sql"),
        include_str!("../../migrations/0005_instrument_reference.sql"),
        include_str!("../../migrations/0006_market_observations.sql"),
        include_str!("../../migrations/0007_executability_without_stale.sql"),
        include_str!("../../migrations/0008_quotation_basis.sql"),
        include_str!("../../migrations/0009_broker_operation_kinds.sql"),
        include_str!("../../migrations/0010_bond_schedule.sql"),
        include_str!("../../migrations/0011_accrued_interest.sql"),
        include_str!("../../migrations/0012_account_scoped_source_operation.sql"),
        include_str!("../../migrations/0013_event_source_time.sql"),
        include_str!("../../migrations/0014_securities_transfer_kinds.sql"),
    ];
    assert!(
        through <= migrations.len() as u32,
        "unsupported migration version"
    );

    for (version, sql) in migrations.into_iter().enumerate() {
        let version = version + 1;
        if version as u32 > through {
            break;
        }
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"
        ))
        .expect("applying previous migration");
    }
}
