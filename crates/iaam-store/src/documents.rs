//! Сырьё источников: документы и строки (§10.1).
//!
//! Версия парсера пишется в `provenance` ради повторного разбора, а
//! повторить разбор без сырья нельзя: исправленный парсер оказался бы
//! бесполезен для уже загруженного отчёта. Поэтому тело документа и
//! каждая его строка хранятся целиком и неизменяемо.
//!
//! Хранилище не знает, какие бывают брокеры и форматы: закрытый набор
//! живёт в реестре парсеров, а здесь код брокера — имя, под которым
//! реестр себя назвал. Разбирать его тут не на что и незачем.

use iaam_core::event::provenance::{ParserVersion, RawHash};
use iaam_core::ids::{OwnerId, SourceId};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{SqliteStore, StoreError, now};

/// Код брокера, под которым его знает реестр парсеров.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrokerCode(String);

/// Формат отчёта, под которым его знает реестр парсеров.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReportFormat(String);

macro_rules! named_code {
    ($name:ident, $what:literal) => {
        impl $name {
            /// Принимает только непустое имя.
            ///
            /// Проверка живёт здесь, а не в конструкторе с именем `new`:
            /// `cargo-mutants` молча пропускает функции с этим именем.
            ///
            #[doc = concat!("Пустая строка в колонке `", $what, "` неотличима от «не знаем",)]
            /// », а неизвестное значение — это `Option`, а не заглушка (§4.9).
            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

named_code!(BrokerCode, "broker");
named_code!(ReportFormat, "format");

/// Загружаемый документ: то, что пришло от владельца.
///
/// Момента загрузки здесь нет: его ставит хранилище, потому что часы
/// одни на всю крейту, а присланный клиентом момент — это момент,
/// которому нечем верить.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDocument {
    pub id: SourceId,
    pub owner: OwnerId,
    pub broker: BrokerCode,
    pub format: ReportFormat,
    pub parser_version: ParserVersion,
    pub document_hash: RawHash,
    pub body: Vec<u8>,
}

/// Сохранённый документ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRecord {
    pub id: SourceId,
    pub owner: OwnerId,
    pub broker: BrokerCode,
    pub format: ReportFormat,
    pub parser_version: ParserVersion,
    pub document_hash: RawHash,
    pub uploaded_at: String,
    pub body: Vec<u8>,
}

/// Что произошло при загрузке.
///
/// Повторная отправка того же файла — не ошибка и не второй документ:
/// клиент, не получивший ответа, обязан иметь право повторить (§10.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentStored {
    Inserted { id: SourceId },
    AlreadyPresent { existing: SourceId },
}

/// Что стало со строкой при разборе.
///
/// Различаются два исхода, потому что ровно их видит хранилище: строка
/// стала фактом журнала или не стала. Более тонкие исходы — повтор,
/// операция вне периметра — живут в вердикте приёмки, а не в сырье:
/// сырьё описывает документ, а не решение по нему.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowStatus {
    /// Строка разобрана и стала фактом.
    Parsed,
    /// Строку не разобрали. Документ этим не отменяется (§10.1).
    Unparsed,
}

impl RowStatus {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::Unparsed => "unparsed",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "parsed" => Some(Self::Parsed),
            "unparsed" => Some(Self::Unparsed),
            _ => None,
        }
    }
}

/// Строка документа с локатором.
///
/// Документа в самой строке нет: он один на всю пачку и передаётся
/// отдельно. Второй экземпляр этого поля разошёлся бы с первым, и
/// строка одного документа оказалась бы записана в другой.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// Лист. `None` — листа не было (CSV), а не «лист не разобрали».
    pub sheet: Option<String>,
    pub row: u64,
    pub payload: String,
    pub status: RowStatus,
}

impl SqliteStore {
    /// Сохранение документа целиком.
    ///
    /// Проверка «этот файл уже есть» и вставка идут одной немедленной
    /// транзакцией: раздельно они образуют гонку, в которой два
    /// одновременных запроса получают два документа с одним хешом.
    pub fn insert_document(
        &mut self,
        document: &NewDocument,
    ) -> Result<DocumentStored, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT id FROM source_documents WHERE owner = ?1 AND document_hash = ?2",
                params![
                    document.owner.inner().to_string(),
                    document.document_hash.as_str()
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            return Ok(DocumentStored::AlreadyPresent {
                existing: SourceId(parse_uuid(&existing, "документ")?),
            });
        }
        transaction.execute(
            "INSERT INTO source_documents (
                 id, owner, broker, format, parser_version, document_hash, uploaded_at, body
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                document.id.inner().to_string(),
                document.owner.inner().to_string(),
                document.broker.as_str(),
                document.format.as_str(),
                document.parser_version.0,
                document.document_hash.as_str(),
                now(),
                document.body,
            ],
        )?;
        transaction.commit()?;
        Ok(DocumentStored::Inserted { id: document.id })
    }

    /// Чтение документа владельца.
    ///
    /// Владелец входит в запрос, а не проверяется после чтения: чужой
    /// документ не должен доезжать до вызывающего даже на мгновение.
    pub fn load_document(
        &self,
        owner: OwnerId,
        id: SourceId,
    ) -> Result<DocumentRecord, StoreError> {
        self.query_documents(
            "SELECT id, broker, format, parser_version, document_hash, uploaded_at, body
             FROM source_documents WHERE owner = ?1 AND id = ?2",
            params![owner.inner().to_string(), id.inner().to_string()],
            owner,
        )?
        .pop()
        .ok_or_else(|| StoreError::NotFound {
            what: "документ",
            id: id.inner().to_string(),
        })
    }

    /// Документы, разобранные не этой версией парсера.
    ///
    /// Это список кандидатов на повторный разбор, а не список
    /// невыполненной работы: строка документа неизменяема, и отметки
    /// «переразобрано» в ней не появляется. Что разбор состоялся,
    /// видно по `provenance` событий, а не отсюда.
    pub fn documents_needing_reparse(
        &self,
        owner: OwnerId,
        parser_version: &ParserVersion,
    ) -> Result<Vec<DocumentRecord>, StoreError> {
        self.query_documents(
            "SELECT id, broker, format, parser_version, document_hash, uploaded_at, body
             FROM source_documents
             WHERE owner = ?1 AND parser_version <> ?2
             ORDER BY uploaded_at, id",
            params![owner.inner().to_string(), parser_version.0],
            owner,
        )
    }

    fn query_documents(
        &self,
        sql: &str,
        parameters: &[&dyn rusqlite::ToSql],
        owner: OwnerId,
    ) -> Result<Vec<DocumentRecord>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(parameters, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Vec<u8>>(6)?,
            ))
        })?;
        let mut documents = Vec::new();
        for row in rows {
            let (id, broker, format, parser_version, document_hash, uploaded_at, body) = row?;
            documents.push(DocumentRecord {
                id: SourceId(parse_uuid(&id, "документ")?),
                owner,
                broker: BrokerCode::parse(&broker).ok_or_else(|| StoreError::DocumentDecode {
                    id: id.clone(),
                    detail: "код брокера пуст".to_owned(),
                })?,
                format: ReportFormat::parse(&format).ok_or_else(|| StoreError::DocumentDecode {
                    id: id.clone(),
                    detail: "формат отчёта пуст".to_owned(),
                })?,
                parser_version: ParserVersion(parser_version),
                document_hash: RawHash::parse(&document_hash).ok_or_else(|| {
                    StoreError::DocumentDecode {
                        id: id.clone(),
                        detail: "хеш документа не является SHA-256".to_owned(),
                    }
                })?,
                uploaded_at,
                body,
            });
        }
        Ok(documents)
    }

    /// Запись пачки строк документа.
    ///
    /// Пачка кладётся одной транзакцией: половина сырья хуже, чем его
    /// отсутствие — по неполному набору строк повторный разбор молча
    /// даст неполный результат.
    pub fn insert_rows(
        &mut self,
        owner: OwnerId,
        document: SourceId,
        rows: &[RawRow],
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        owned_document(&transaction, owner, document)?;
        for row in rows {
            transaction.execute(
                "INSERT INTO raw_rows (document, sheet, row, payload, status)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    document.inner().to_string(),
                    row.sheet,
                    row_number_to_sql(row.row)?,
                    row.payload,
                    row.status.code(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Строки документа в порядке локатора.
    pub fn rows_of_document(
        &self,
        owner: OwnerId,
        document: SourceId,
    ) -> Result<Vec<RawRow>, StoreError> {
        owned_document(&self.conn, owner, document)?;
        let mut statement = self.conn.prepare(
            "SELECT sheet, row, payload, status FROM raw_rows
             WHERE document = ?1
             ORDER BY sheet, row",
        )?;
        let rows = statement.query_map([document.inner().to_string()], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut raw = Vec::new();
        for entry in rows {
            let (sheet, number, payload, status) = entry?;
            raw.push(RawRow {
                sheet,
                row: row_number_from_sql(number, document)?,
                payload,
                status: RowStatus::from_code(&status).ok_or_else(|| {
                    StoreError::DocumentDecode {
                        id: document.inner().to_string(),
                        detail: format!("неизвестный статус строки: {status}"),
                    }
                })?,
            });
        }
        Ok(raw)
    }
}

/// Проверка, что документ принадлежит владельцу.
///
/// Отсутствие и чужое владение дают одну ошибку намеренно: разные
/// ответы сообщили бы постороннему, что такой документ существует.
fn owned_document(
    conn: &rusqlite::Connection,
    owner: OwnerId,
    document: SourceId,
) -> Result<(), StoreError> {
    let found: Option<String> = conn
        .query_row(
            "SELECT id FROM source_documents WHERE owner = ?1 AND id = ?2",
            params![owner.inner().to_string(), document.inner().to_string()],
            |row| row.get(0),
        )
        .optional()?;
    found.map(|_| ()).ok_or(StoreError::NotFound {
        what: "документ",
        id: document.inner().to_string(),
    })
}

/// Номер строки в SQLite — знаковый. Номер, который туда не влезает,
/// становится ошибкой, а не молча урезанным числом.
fn row_number_to_sql(row: u64) -> Result<i64, StoreError> {
    i64::try_from(row).map_err(|_| StoreError::RowNumberOutOfRange { row })
}

fn row_number_from_sql(row: i64, document: SourceId) -> Result<u64, StoreError> {
    u64::try_from(row).map_err(|_| StoreError::DocumentDecode {
        id: document.inner().to_string(),
        detail: format!("отрицательный номер строки: {row}"),
    })
}

fn parse_uuid(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
