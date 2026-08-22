//! Миграции.
//!
//! Миграции нумерованы и применяются по одной в транзакции. Файл схемы
//! встроен в двоичный файл: база, открытая версией программы, обязана
//! соответствовать этой версии, а не тому, что лежит рядом на диске.

use rusqlite::Connection;

use crate::StoreError;

/// Версия схемы, которую понимает эта сборка.
pub const SCHEMA_VERSION: u32 = 1;

const MIGRATIONS: [(u32, &str); 1] = [(1, include_str!("../migrations/0001_initial.sql"))];

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
