//! Raw source materials: documents and rows (§10.1).
//!
//! The parser version is written to `provenance` to allow reprocessing, but
//! reprocessing without the raw source is impossible: a corrected parser would be
//! useless for a report that has already been loaded. Therefore, the document body and
//! each of its rows are stored in full and remain immutable.
//!
//! The storage does not know which brokers or formats exist: the closed set
//! lives in the parser registry, and here the broker code is the name under which
//! the registry identifies it. There is nothing to parse here, nor any reason to.

use iaam_core::event::provenance::{ParserVersion, RawHash};
use iaam_core::ids::{OwnerId, SourceId};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{SqliteStore, StoreError, now};

/// The broker code under which the parser registry identifies it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrokerCode(String);

/// The report format under which the parser registry identifies it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReportFormat(String);

macro_rules! named_code {
    ($name:ident, $what:literal) => {
        impl $name {
            /// Accepts only a non-empty name.
            ///
            /// The check lives here rather than in the constructor named `new`:
            /// `cargo-mutants` silently skips functions with this name.
            ///
            #[doc = concat!("An empty string in column `", $what, "` is indistinguishable from “we don't know",)]
            /// ”, while an unknown value is an `Option`, not a placeholder (§4.9).
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

/// A loadable document: what came from the owner.
///
/// There is no loading timestamp here: the storage sets it, because there is
/// one clock for the entire crate, while a timestamp supplied by the client is a moment
/// that cannot be trusted.
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

/// A stored document.
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

/// What happened during loading.
///
/// Resubmitting the same file is neither an error nor a second document:
/// a client that did not receive a response must be able to retry (§10.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentStored {
    Inserted { id: SourceId },
    AlreadyPresent { existing: SourceId },
}

/// What happened to the row during parsing.
///
/// There are two outcomes because they are exactly what the storage sees: the row
/// became a journal fact or it did not. Finer-grained outcomes—a duplicate,
/// an operation outside the scope—belong in the acceptance verdict, not in the raw input:
/// the raw input describes the document, not the decision about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowStatus {
    /// The row was parsed and became a fact.
    Parsed,
    /// The row was not parsed. This does not invalidate the document (§10.1).
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

/// Document row with a locator.
///
/// The document itself is not in the row: there is one document for the entire batch, and it is passed
/// separately. A second copy of this field would diverge from the first, and
/// a row from one document could end up being written to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// Sheet. `None` means there was no sheet (CSV), not “the sheet was not parsed.”
    pub sheet: Option<String>,
    pub row: u64,
    pub payload: String,
    pub status: RowStatus,
}

impl SqliteStore {
    /// Saving the entire document.
    ///
    /// Checking whether “this file already exists” and inserting it are performed in one immediate
    /// transaction: separately, they create a race in which two
    /// simultaneous requests receive two documents with the same hash.
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
                existing: SourceId(parse_uuid(&existing, "document")?),
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

    /// Read the owner's document.
    ///
    /// The owner is included in the query rather than checked after reading: a foreign
    /// document must not reach the caller even for an instant.
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
            what: "document",
            id: id.inner().to_string(),
        })
    }

    /// Read the owner's document by the hash that names it.
    ///
    /// A read separate from [`SqliteStore::load_document`] because the hash, not
    /// the upload identifier, is what `provenance` carries: a caller holding a
    /// fact knows the document under this name and no other. The owner is in the
    /// query for the same reason it is there — a foreign document must not reach
    /// the caller even for an instant.
    ///
    /// Absence is `None` rather than [`StoreError::NotFound`]: a document that
    /// was never stored has an answer for the caller — send its bytes — while a
    /// failed read has none, and one return value for both would hide the
    /// difference.
    pub fn load_document_by_hash(
        &self,
        owner: OwnerId,
        document_hash: &RawHash,
    ) -> Result<Option<DocumentRecord>, StoreError> {
        Ok(self
            .query_documents(
                "SELECT id, broker, format, parser_version, document_hash, uploaded_at, body
                 FROM source_documents WHERE owner = ?1 AND document_hash = ?2",
                params![owner.inner().to_string(), document_hash.as_str()],
                owner,
            )?
            .pop())
    }

    /// Documents parsed by a different parser version.
    ///
    /// This is a list of candidates for re-parsing, not a list of
    /// unfinished work: the document row is immutable, so no
    /// “re-parsed” marker is added to it. The fact that parsing occurred
    /// is shown by the events' `provenance`, not here.
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
                id: SourceId(parse_uuid(&id, "document")?),
                owner,
                broker: BrokerCode::parse(&broker).ok_or_else(|| StoreError::DocumentDecode {
                    id: id.clone(),
                    detail: "broker code is empty".to_owned(),
                })?,
                format: ReportFormat::parse(&format).ok_or_else(|| StoreError::DocumentDecode {
                    id: id.clone(),
                    detail: "report format is empty".to_owned(),
                })?,
                parser_version: ParserVersion(parser_version),
                document_hash: RawHash::parse(&document_hash).ok_or_else(|| {
                    StoreError::DocumentDecode {
                        id: id.clone(),
                        detail: "document hash is not SHA-256".to_owned(),
                    }
                })?,
                uploaded_at,
                body,
            });
        }
        Ok(documents)
    }

    /// Write a batch of document rows.
    ///
    /// The batch is written in a single transaction: half the raw data is worse than none—
    /// re-parsing an incomplete set of rows will silently
    /// produce an incomplete result.
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

    /// Document rows ordered by locator.
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
                        detail: format!("unknown row status: {status}"),
                    }
                })?,
            });
        }
        Ok(raw)
    }
}

/// Verify that the document belongs to the owner.
///
/// Missing and foreign ownership intentionally produce the same error: different
/// responses would reveal to an outsider that such a document exists.
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
        what: "document",
        id: document.inner().to_string(),
    })
}

/// A SQLite row number is signed. A number that does not fit becomes
/// an error rather than being silently truncated.
fn row_number_to_sql(row: u64) -> Result<i64, StoreError> {
    i64::try_from(row).map_err(|_| StoreError::RowNumberOutOfRange { row })
}

fn row_number_from_sql(row: i64, document: SourceId) -> Result<u64, StoreError> {
    u64::try_from(row).map_err(|_| StoreError::DocumentDecode {
        id: document.inner().to_string(),
        detail: format!("negative row number: {row}"),
    })
}

fn parse_uuid(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}

/// One account name a reading of a document could not place, as it is stored.
///
/// A transcription and a count, and nothing else. Whether the owner's directory
/// still fails to resolve `printed` is decided by whoever reads this back,
/// against the directory as it then stands — see the migration for why a stored
/// verdict would go quietly stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedAccountRecord {
    pub document_hash: RawHash,
    pub import_session: uuid::Uuid,
    /// The cell as the document printed it.
    pub printed: String,
    /// Records of that document that printed it.
    pub records: u32,
}

impl SqliteStore {
    /// Record what one reading of one document could not place.
    ///
    /// **The document's whole set is replaced, in one transaction.** A reading
    /// is an answer to «what does this document ask for, against the directory
    /// as it now stands», and a second reading — after the owner created two of
    /// the accounts, say — answers it again and answers it better. Adding to the
    /// set rather than replacing it would leave the two answers side by side
    /// with nothing saying which is current, and would count one document's
    /// records twice.
    ///
    /// An empty `names` is therefore a statement and not a no-op: it says this
    /// reading placed every account the document named, and it is what clears
    /// the rows an earlier reading left.
    pub fn record_unresolved_accounts(
        &mut self,
        owner: OwnerId,
        document_hash: &RawHash,
        import_session: uuid::Uuid,
        names: &[(String, u32)],
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM document_unresolved_accounts
             WHERE owner = ?1 AND document_hash = ?2",
            params![owner.inner().to_string(), document_hash.as_str()],
        )?;
        let recorded_at = now();
        for (ordinal, (printed, records)) in names.iter().enumerate() {
            transaction.execute(
                "INSERT INTO document_unresolved_accounts (
                     owner, document_hash, printed, ordinal, records,
                     import_session, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    owner.inner().to_string(),
                    document_hash.as_str(),
                    printed.as_str(),
                    i64::try_from(ordinal).unwrap_or(i64::MAX),
                    i64::from(*records),
                    import_session.to_string(),
                    recorded_at,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Every such name this instance holds for the owner.
    ///
    /// Ordered by document and then by the position the document first printed
    /// the name at, so a caller sees each document's names in the order its own
    /// file prints them. The owner is in the query rather than checked after
    /// reading, for the reason every read here gives.
    pub fn list_unresolved_accounts(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<UnresolvedAccountRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT document_hash, import_session, printed, records
             FROM document_unresolved_accounts
             WHERE owner = ?1
             ORDER BY document_hash, ordinal",
        )?;
        let rows = statement.query_map(params![owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut records = Vec::new();
        for entry in rows {
            let (document_hash, session, printed, count) = entry?;
            records.push(UnresolvedAccountRecord {
                document_hash: RawHash::parse(&document_hash).ok_or_else(|| {
                    StoreError::DocumentDecode {
                        id: document_hash.clone(),
                        detail: "document hash is not SHA-256".to_owned(),
                    }
                })?,
                import_session: parse_uuid(&session, "import session")?,
                printed,
                records: u32::try_from(count).unwrap_or(u32::MAX),
            });
        }
        Ok(records)
    }
}
