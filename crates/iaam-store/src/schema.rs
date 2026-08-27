//! Миграции.
//!
//! Миграции нумерованы и применяются по одной в транзакции. Файл схемы
//! встроен в двоичный файл: база, открытая версией программы, обязана
//! соответствовать этой версии, а не тому, что лежит рядом на диске.

use rusqlite::Connection;

use crate::StoreError;

/// Версия схемы, которую понимает эта сборка.
pub const SCHEMA_VERSION: u32 = 11;

const MIGRATIONS: [(u32, &str); 11] = [
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
];

/// Применение недостающих миграций.
///
/// База новее программы — отказ, а не попытка работать: неизвестная
/// колонка молча читается как отсутствующая, и это худший вид ошибки.
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
