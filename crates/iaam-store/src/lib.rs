//! Store: SQLite as the complete working state (§3.3).
//!
//! The crate is synchronous and blocking. Asynchrony lives in `iaam-app`,
//! which calls the store through a dedicated blocking executor:
//! `rusqlite` blocks the thread, and calling it directly from an axum handler
//! stops the executor (§3.2).

pub mod broker_access;
pub mod broker_operation_kinds;
pub mod market;
pub mod market_source_codes;

pub mod bundle;
pub mod documents;
pub mod events;
pub mod reference;
pub mod rules;
pub mod schedule;
pub mod schema;
pub mod snapshots;
pub mod tokens;

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Write timestamp in RFC 3339, UTC.
///
/// One for the entire crate: previously, this function existed as a copy in
/// `reference.rs` and `tokens.rs`, and the two copies of the same formatting
/// could silently diverge — the `created_at` columns in different tables would
/// start using different formats, leaving nothing to compare them with.
///
/// A formatting failure neither panics nor returns an empty string: an empty
/// `created_at` is indistinguishable from “field missing”, while an epoch is at
/// least clearly implausible and therefore noticeable.
pub(crate) fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("failed to parse saved event {id}: {source}")]
    EventDecode {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize event: {0}")]
    EventEncode(#[source] serde_json::Error),
    #[error("failed to parse snapshot: {0}")]
    SnapshotDecode(String),
    #[error("failed to serialize snapshot: {0}")]
    SnapshotEncode(String),
    #[error("database schema version {found} is newer than supported version {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("record {what} {id} not found in database")]
    NotFound { what: &'static str, id: String },
    #[error("active alias {namespace}:{value} not found for instrument {instrument}")]
    AliasNotFoundForInstrument {
        namespace: &'static str,
        value: String,
        instrument: String,
    },
    #[error("archived bundle is corrupted: {detail}")]
    BundleCorrupted { detail: String },
    #[error("saved document {id} cannot be read: {detail}")]
    DocumentDecode { id: String, detail: String },
    #[error("row number {row} cannot be stored")]
    RowNumberOutOfRange { row: u64 },
    /// An active record already exists. Separate from `Sqlite` because
    /// this is the owner's response, not a failure: the text “UNIQUE
    #[error("{what} already exists: revoke the active one first")]
    AlreadyExists { what: &'static str },
    #[error("invalid value for {field}: {value}")]
    InvalidValue { field: &'static str, value: String },
    #[error("a synchronization run is already in progress for {source_id}/{dataset}/{series_key}")]
    LeaseHeld {
        source_id: String,
        dataset: String,
        series_key: String,
    },
    #[error("synchronization run lease has expired")]
    LeaseExpired,
    #[error("synchronization run not found or lease token is invalid")]
    RunNotFound,
    #[error("classification rule field {field} is not JSON: {source}")]
    RuleNotJson {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },
}
/// Why the instrument could not be resolved by external code.
///
/// The three cases are intentionally distinguished. Merging them into one `NotFound`
/// would give the caller a message that cannot
/// distinguish a new security from a corrupted document date.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("code {value} in namespace {namespace} is unknown")]
    Unknown {
        namespace: &'static str,
        value: String,
    },
    #[error(
        "code {value} in namespace {namespace} is known, but not on {on}: \
         active from {known_from} through {known_to}"
    )]
    NotOnDate {
        namespace: &'static str,
        value: String,
        on: String,
        known_from: String,
        known_to: String,
    },
    /// The `instrument_aliases_do_not_overlap` trigger fired: this is a defect
    /// of the schema, not the data, and must not be ignored.
    #[error("code {value} in namespace {namespace} on {on} resolves to {candidates} instruments")]
    Ambiguous {
        namespace: &'static str,
        value: String,
        on: String,
        candidates: usize,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Database connection.
///
/// Owns the connection exclusively: `rusqlite::Connection` is not `Sync`,
/// and a pool is unnecessary in stage 1—the writer is single-threaded, while reads run on the same
/// blocking executor.
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Opens the database file and applies migrations.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    /// In-memory database. Needed by tests: a file-based database leaves
    /// residue in the test and makes tests dependent on one another.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::prepare(conn)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
        // foreign_keys are disabled by default in SQLite: without this line
        // declared foreign keys are not checked at all.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // WAL: a reader does not block a writer. For a single user,
        // this is not about load, but about preventing a report from failing during a write.
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
        // An empty string and arbitrary text in `created_at` are indistinguishable
        // from “field is missing”: the timestamp must be parsed back.
        let stamp = now();
        let parsed = OffsetDateTime::parse(&stamp, &Rfc3339).expect("timestamp parses back");
        assert!(stamp.ends_with('Z'), "timestamp is written in UTC: {stamp}");
        assert!(
            parsed.year() >= 2025,
            "timestamp is not from a previous century: {stamp}"
        );
    }
}
