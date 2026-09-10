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
pub mod categories;
pub mod decisions;
pub mod documents;
pub mod import_session;
pub mod journal;
pub mod reference;
pub mod rules;
pub mod schedule;
pub mod schema;
pub mod snapshots;
pub mod source_profiles;
pub mod tokens;

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

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
    #[error("failed to serialize event: {0}")]
    EventEncode(#[source] serde_json::Error),
    #[error("failed to parse snapshot: {0}")]
    SnapshotDecode(String),
    #[error("failed to serialize snapshot: {0}")]
    SnapshotEncode(String),
    #[error("database schema version {found} is newer than supported version {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    /// The archive's `schema_version` was written under a different schema
    /// numbering than this build implements (iaam-k3gh.9.5): the collapse
    /// that produced `0001_schema.sql` reset the counter once already, so the
    /// same bare integer can name two different schemas depending which
    /// numbering wrote it. Comparing `found` against `current` here the way
    /// [`StoreError::SchemaTooNew`] compares versions would be exactly the
    /// false acceptance this variant exists to refuse instead: two equal
    /// generation numbers mean the versions beside them are comparable, and
    /// two unequal ones mean they are not, regardless of which is larger.
    #[error(
        "archive schema numbering (generation {found}) does not match this build's schema \
         numbering (generation {current}): its schema_version was written under a different \
         numbering than this build's counter, so the two are not comparable and the archive \
         cannot be safely restored"
    )]
    SchemaGenerationMismatch { found: u32, current: u32 },
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
    #[error("category group {id} is retired")]
    CategoryGroupRetired { id: String },
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
    /// A correction chain that comes back to a fact it already holds.
    ///
    /// `iaam_core::event::correction::resolve` refuses to write one, so a
    /// database holding one is corrupt. It is an error rather than a shorter
    /// chain because the two are indistinguishable to whoever reads the answer:
    /// a fragment of a history returned as though it were the whole of it is a
    /// wrong answer given confidently, and the walk would keep giving it.
    #[error(
        "the correction chain returns to event {event}: the journal holds a cycle, \
         and no whole history can be read out of it"
    )]
    CorrectionChainCycle { event: String },
    /// The insert primitive's own postcondition (spec §D6, §5): reading the
    /// event back inside the same transaction, right before commit, found it
    /// missing or found it reconstructed unequal to what was given to write.
    ///
    /// Not a tamper check and not a hash. A foreign key proves a child row
    /// has its parent; nothing in SQLite proves a parent has all its required
    /// children, and there is no cross-table `ASSERTION`. This is the runtime
    /// postcondition standing in for that: the one thing that catches a
    /// mapper that returned `Ok` having simply forgotten a leg, a detail row,
    /// or an optional field. The transaction rolls back, so no row of the
    /// event survives in any table.
    #[error(
        "event {event} could not be read back as written inside its own write transaction: \
         the write was incomplete"
    )]
    IncompleteWrite { event: String },
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

/// How long a connection waits for a lock another connection holds.
///
/// Also the budget for [`enable_wal`], so that one opening waits once, not twice.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Pause between attempts to switch the journal mode. Long enough that a refused connection
/// is not spinning, short enough that the usual case—the other connection is a moment away
/// from finishing its own switch—costs one pause rather than a visible delay.
const RETRY_PAUSE: Duration = Duration::from_millis(10);

/// Switch the file to WAL, waiting out a connection that is switching it at the same moment.
///
/// Changing the journal mode needs a brief exclusive lock, and this is the one place where
/// SQLite reports SQLITE_BUSY without consulting `busy_timeout`—so the wait has to be written
/// out here. Two processes opening a fresh file at the same instant both find it in the
/// default mode and both attempt the switch; the one that is refused retries, and by then the
/// file is in WAL already and the pragma is a no-op. The mode is a property of the file, not
/// of the connection, so there is nothing else to reconcile.
fn enable_wal(conn: &Connection) -> Result<(), StoreError> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => return Ok(()),
            Err(error) => {
                let busy = matches!(
                    error,
                    rusqlite::Error::SqliteFailure(inner, _)
                        if inner.code == rusqlite::ErrorCode::DatabaseBusy
                            || inner.code == rusqlite::ErrorCode::DatabaseLocked
                );
                if !busy || Instant::now() >= deadline {
                    return Err(error.into());
                }
                thread::sleep(RETRY_PAUSE);
            }
        }
    }
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
        // Set first, before anything touches the file: by default a locked database
        // returns SQLITE_BUSY at once, so the server starting and a CLI command run a
        // moment later turn a wait of milliseconds into a hard failure. Five seconds is
        // far longer than any write this program makes—the longest is migrating an empty
        // file—and short enough that a holder that is genuinely stuck is reported rather
        // than waited on forever.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // foreign_keys are disabled by default in SQLite: without this line
        // declared foreign keys are not checked at all.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // WAL: a reader does not block a writer. For a single user,
        // this is not about load, but about preventing a report from failing during a write.
        enable_wal(&conn)?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let store = Self { conn };
        schema::migrate(&store.conn)?;
        Ok(store)
    }

    /// The raw connection, unmediated by any typed method.
    ///
    /// Behind a test-only surface on purpose (spec §D6): append-only is a
    /// property of what this crate's API publishes, not of the file, and a
    /// production caller with the raw connection can write, update or delete
    /// any row this type's methods would otherwise refuse to. No in-process
    /// production code needs that — the one caller that used to reach for
    /// `connection_mut()` for transaction control (`iaam-app`'s owner-claim
    /// race guard) now uses [`Self::with_immediate_transaction`] instead,
    /// which cannot be used to bypass a write method's own checks. What
    /// remains is tests: setting up fixtures the public API has no
    /// constructor for, and probing invariants (row counts, fault injection)
    /// the public API does not report.
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub const fn connection(&self) -> &Connection {
        &self.conn
    }

    /// See [`Self::connection`]: same reasoning, mutable access.
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub const fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Runs `work` inside one `BEGIN IMMEDIATE` transaction, committing on
    /// success and rolling back otherwise.
    ///
    /// For a caller outside this crate that needs to demarcate a transaction
    /// around several of this store's own typed calls — `iaam-app`'s
    /// owner-claim race guard is the one today (ADR-0003) — without reaching
    /// for the raw connection. `work` receives `&mut Self` rather than a
    /// borrowed `rusqlite::Transaction`: the guard's own work is itself a
    /// sequence of calls back into typed `SqliteStore` methods, and a
    /// `Transaction<'_>` borrows the connection in a way that would make that
    /// impossible. The transaction is therefore demarcated with explicit
    /// `BEGIN`/`COMMIT`/`ROLLBACK` statements rather than the `Transaction`
    /// type — the same trade `in_immediate_transaction` made before this
    /// method existed to replace it.
    ///
    /// `convert` turns this call's own transaction-control failure (opening,
    /// committing) into the caller's error type: this crate does not know
    /// `iaam-app`'s `AppError`, or any other caller's error type, so it
    /// cannot implement `From<StoreError>` for it.
    pub fn with_immediate_transaction<T, E>(
        &mut self,
        convert: impl Fn(StoreError) -> E,
        work: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| convert(error.into()))?;
        match work(self) {
            Ok(value) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(value),
                Err(error) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(convert(error.into()))
                }
            },
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Sets the connection's busy-wait budget.
    ///
    /// Exists for the same caller as [`Self::with_immediate_transaction`]:
    /// the owner-claim race guard raises the wait budget just before entering
    /// the transaction, so the loser of the race waits for the winner rather
    /// than failing at once with `SQLITE_BUSY`.
    pub fn set_busy_timeout(&mut self, timeout: Duration) -> Result<(), StoreError> {
        self.conn.busy_timeout(timeout).map_err(Into::into)
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
