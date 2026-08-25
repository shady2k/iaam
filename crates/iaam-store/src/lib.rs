//! Хранилище: SQLite как полное рабочее состояние (§3.3).
//!
//! Крейта синхронная и блокирующая. Асинхронность живёт в `iaam-app`,
//! которая зовёт хранилище через выделенный блокирующий исполнитель:
//! `rusqlite` блокирует поток, и вызов его прямо из обработчика axum
//! останавливает исполнитель (§3.2).

pub mod broker_access;
pub mod bundle;
pub mod documents;
pub mod events;
pub mod reference;
pub mod rules;
pub mod schema;
pub mod snapshots;
pub mod tokens;

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Момент записи в RFC 3339, UTC.
///
/// Одна на всю крейту: раньше эта функция существовала копией в
/// `reference.rs` и в `tokens.rs`, и две копии одного форматирования
/// расходятся молча — колонки `created_at` разных таблиц начали бы
/// отличаться форматом, а сравнить их было бы уже нечем.
///
/// Отказ форматирования не паникует и не даёт пустой строки: пустое
/// `created_at` неотличимо от «поля нет», а эпоха хотя бы заведомо
/// неправдоподобна и потому заметна.
pub(crate) fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

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
    #[error("сохранённый документ {id} не читается: {detail}")]
    DocumentDecode { id: String, detail: String },
    #[error("номер строки {row} не помещается в хранилище")]
    RowNumberOutOfRange { row: u64 },
    /// Действующая запись уже есть. Отдельно от `Sqlite`, потому что
    /// это ответ на вопрос владельца, а не сбой: текст «UNIQUE
    /// constraint failed» отправляет искать поломку там, где её нет,
    /// да ещё и рассказывает наружу устройство схемы.
    #[error("{what} уже заведён: сначала отзовите действующий")]
    AlreadyExists { what: &'static str },
    #[error("поле {field} правила классификации не является JSON: {source}")]
    RuleNotJson {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },
}
/// Почему инструмент не разрешился по внешнему коду.
///
/// Три случая различаются намеренно. Слить их в один `NotFound`
/// означало бы отдать разбирающемуся сообщение, по которому нельзя
/// отличить новую бумагу от испорченной даты документа.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("код {value} в пространстве {namespace} неизвестен")]
    Unknown {
        namespace: &'static str,
        value: String,
    },
    #[error(
        "код {value} в пространстве {namespace} известен, но не на {on}: \
         действует с {known_from} по {known_to}"
    )]
    NotOnDate {
        namespace: &'static str,
        value: String,
        on: String,
        known_from: String,
        known_to: String,
    },
    /// Триггер `instrument_aliases_do_not_overlap` пробит: это дефект
    /// схемы, а не данных, и молчать о нём нельзя.
    #[error(
        "код {value} в пространстве {namespace} на {on} разрешается в {candidates} инструментов"
    )]
    Ambiguous {
        namespace: &'static str,
        value: String,
        on: String,
        candidates: usize,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_moment_is_a_parsable_utc_timestamp() {
        // Пустая строка и произвольный текст в `created_at` неотличимы
        // от «поля нет»: момент обязан разбираться обратно.
        let stamp = now();
        let parsed = OffsetDateTime::parse(&stamp, &Rfc3339).expect("момент разбирается обратно");
        assert!(stamp.ends_with('Z'), "момент записывается в UTC: {stamp}");
        assert!(parsed.year() >= 2025, "момент не из прошлого века: {stamp}");
    }
}
