//! Хранилище: SQLite как полное рабочее состояние (§3.3).
//!
//! Крейта синхронная и блокирующая. Асинхронность живёт в `iaam-app`,
//! которая зовёт хранилище через выделенный блокирующий исполнитель:
//! `rusqlite` блокирует поток, и вызов его прямо из обработчика axum
//! останавливает исполнитель (§3.2).

pub mod bundle;
pub mod events;
pub mod reference;
pub mod schema;
pub mod snapshots;
pub mod tokens;

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("ошибка SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("не удалось разобрать сохранённое событие {id}: {source}")]
    EventDecode {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("не удалось сериализовать событие: {0}")]
    EventEncode(#[source] serde_json::Error),
    #[error("не удалось разобрать снимок: {0}")]
    SnapshotDecode(String),
    #[error("не удалось сериализовать снимок: {0}")]
    SnapshotEncode(String),
    #[error("схема базы версии {found} новее поддерживаемой {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("в базе нет записи {what} {id}")]
    NotFound { what: &'static str, id: String },
    #[error("архивный бандл повреждён: {detail}")]
    BundleCorrupted { detail: String },
}

/// Подключение к базе.
///
/// Владеет соединением монопольно: `rusqlite::Connection` не `Sync`,
/// и пул на этапе 1 не нужен — писатель один, а чтение идёт под тем же
/// блокирующим исполнителем.
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Открытие файла базы с применением миграций.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    /// База в памяти. Нужна тестам: файловая база в тесте оставляет
    /// мусор и делает тесты зависимыми друг от друга.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::prepare(conn)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
        // foreign_keys выключены в SQLite по умолчанию: без этой строки
        // объявленные внешние ключи не проверяются вообще.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // WAL: читатель не блокирует писателя. Для одного пользователя
        // это не про нагрузку, а про то, чтобы отчёт не падал во время записи.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let store = Self { conn };
        schema::migrate(&store.conn)?;
        Ok(store)
    }

    #[must_use]
    pub const fn connection(&self) -> &Connection {
        &self.conn
    }

    #[must_use]
    pub const fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}
