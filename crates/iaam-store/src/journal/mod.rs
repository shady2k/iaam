//! The relational journal (spec §4).
//!
//! Task 4 wrote the pure row shapes and the `EventKind` mapper ([`rows`],
//! [`kind`]). Task 6 added [`read`], the two-phase read's bulk-hydration
//! half. Task 7 added [`write`] — the one insert primitive — and the
//! `SqliteStore` write methods below, which route every write path through
//! it. Task 8 replaces `events.rs` wholesale: [`query`] is the paginated
//! journal listing and the source-category projection, [`reads`] is the
//! remaining whole-journal and narrowed reads (`load_events`, account
//! activity, control assertions), and [`chain`] is the correction-chain
//! walk. None of the three reach into `payload` — that column is gone — and
//! none of them extract or iterate a JSON path: every predicate that used
//! to reach into the JSON document is now a predicate over a column, per
//! spec §6.
//!
//! No `pub(crate) use` re-export lives here: `write` is reached from outside
//! this module (`bundle.rs`) through the module-qualified path
//! `crate::journal::write::insert_event_in`, the same way a sibling
//! submodule inside this one reaches another's private items directly
//! (`read.rs` does that for `kind::from_rows`). [`category_index`], the
//! `event_category_assignments` projection, is reached from `write` the same
//! way, and [`SqliteStore::rebuild_category_index`] below is the public door
//! the application layer rebuilds it through.
//!
//! `find_duplicate` and [`Appended`] live here rather than in one of the
//! read modules because both write paths below need them and neither read
//! module does.
mod category_index;
mod chain;
mod kind;
pub mod query;
mod read;
pub mod reads;
mod rows;
pub(crate) mod write;

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::Event;
use iaam_core::ids::{EventId, OwnerId};
use iaam_core::reconciliation::evidence::IdentityScope;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub use chain::{CHAIN_STEP_BACK_SQL, CHAIN_STEP_FORWARD_SQL, RecordedEvent};
pub use query::{JournalCursor, JournalQuery, SourceCategoryQuery};
pub(crate) use read::hydrate;
pub use reads::{AccountActivityRecord, ControlAssertionRecord};
// `CUSTODY_REFERENCE_COLUMNS` alone, not the rest of `write`'s API: the
// schema-coverage test (`tests/schema_coverage.rs`) needs the ground-truth
// list to check the schema against, and nothing else in `write` is meant to
// be reachable from outside this crate (see the module doc above).
pub use write::CUSTODY_REFERENCE_COLUMNS;

use crate::{SqliteStore, StoreError};

/// What happened while attempting to record.
///
/// A retry is not an error: a repeated call with the same key must return
/// the same result; otherwise, a client that did not receive a response cannot
/// safely retry the request (§10.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended {
    Inserted { id: EventId },
    Duplicate { existing: EventId },
}

/// Find a duplicate by keys from strongest to weakest (§10.6).
///
/// The natural key “account + date + amount” is intentionally absent here:
/// two identical purchases on the same day are a legitimate situation, and merging
/// them would mean losing the fact.
pub(crate) fn find_duplicate(
    conn: &Connection,
    event: &Event,
    identity_scope: IdentityScope,
) -> Result<Option<EventId>, StoreError> {
    if let Some(operation) = event.provenance.source_operation_id() {
        let found = match identity_scope {
            IdentityScope::Source => lookup(
                conn,
                "SELECT id FROM events WHERE owner = ?1 AND source = ?2 AND source_operation_id = ?3",
                params![
                    event.owner.inner().to_string(),
                    event.provenance.source().inner().to_string(),
                    operation
                ],
            )?,
            IdentityScope::Account => lookup(
                conn,
                "SELECT id FROM events
                 WHERE owner = ?1 AND source = ?2 AND account = ?3 AND source_operation_id = ?4",
                params![
                    event.owner.inner().to_string(),
                    event.provenance.source().inner().to_string(),
                    event.account.inner().to_string(),
                    operation
                ],
            )?,
        };
        if found.is_some() {
            return Ok(found);
        }
    }
    if let Some(key) = event.idempotency_key.as_deref() {
        let found = lookup(
            conn,
            "SELECT id FROM events WHERE owner = ?1 AND idempotency_key = ?2",
            params![event.owner.inner().to_string(), key],
        )?;
        if found.is_some() {
            return Ok(found);
        }
    }
    lookup(
        conn,
        "SELECT id FROM events WHERE id = ?1",
        params![event.id.inner().to_string()],
    )
}

fn lookup(
    conn: &Connection,
    sql: &str,
    parameters: &[&dyn rusqlite::ToSql],
) -> Result<Option<EventId>, StoreError> {
    let found: Option<String> = conn
        .query_row(sql, parameters, |row| row.get(0))
        .optional()?;
    Ok(found
        .and_then(|id| uuid::Uuid::parse_str(&id).ok())
        .map(EventId))
}

/// Parse a UUID column that names a row this store itself just wrote — an
/// unparseable value there is a corrupt database, not a caller error, so the
/// column name travels with the failure.
pub(crate) fn parse_uuid(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}

impl SqliteStore {
    /// Record an event with an already assigned sequence.
    ///
    /// Used where the sequence is defined externally and cannot be changed:
    /// importing an archived bundle and restoring from an archive.
    ///
    /// Transactional: the duplicate check and the insert happen inside one
    /// transaction, so the write primitive's own postcondition (spec §D6) is
    /// checked, and a failure of either leaves no row behind.
    pub fn append_event(
        &mut self,
        event: &Event,
        identity_scope: IdentityScope,
    ) -> Result<Appended, StoreError> {
        let transaction = self.conn.transaction()?;
        if let Some(existing) = find_duplicate(&transaction, event, identity_scope)? {
            return Ok(Appended::Duplicate { existing });
        }
        write::insert_event_in(&transaction, event)?;
        transaction.commit()?;
        Ok(Appended::Inserted { id: event.id })
    }

    /// Record an event while assigning its sequence number **in the same
    /// transaction**.
    ///
    /// Separating “get `MAX(sequence) + 1`” and “insert” is a race:
    /// two concurrent requests receive the same number, and the order
    /// within the day starts being determined by a random identifier instead of
    /// the declared semantics (§4.8). A transaction with immediate lock acquisition
    /// closes the race between processes as well, while the unique index
    /// `(owner, effective_date, sequence)` turns any remaining gap
    /// into an error instead of silently reordering entries.
    pub fn append_event_in_order(
        &mut self,
        event: &Event,
        identity_scope: IdentityScope,
    ) -> Result<Appended, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_duplicate(&transaction, event, identity_scope)? {
            return Ok(Appended::Duplicate { existing });
        }
        let day = event.order.date();
        let used: Option<u32> = transaction.query_row(
            "SELECT MAX(sequence) FROM events WHERE owner = ?1 AND effective_date = ?2",
            params![event.owner.inner().to_string(), day.to_string()],
            |row| row.get(0),
        )?;
        let stamped = Event {
            order: event.order.source_time().map_or_else(
                || EffectiveOrder::new(day, used.map_or(1, |value| value.saturating_add(1))),
                |source_time| {
                    EffectiveOrder::with_source_time(
                        day,
                        source_time,
                        used.map_or(1, |value| value.saturating_add(1)),
                    )
                },
            ),
            ..event.clone()
        };
        write::insert_event_in(&transaction, &stamped)?;
        transaction.commit()?;
        Ok(Appended::Inserted { id: stamped.id })
    }

    /// Recomputes the owner's whole `event_category_assignments` projection
    /// from the journal and the currently active category rules.
    ///
    /// This is the door the application layer calls after a category rule is
    /// created, edited (retire-then-create) or retired (spec §4.7): a rule
    /// change is the one event this crate cannot fold into an incremental
    /// `assign_for` on append, since it can change what every past event in
    /// the journal decomposes to, not just a newly appended one. Returns how
    /// many of the owner's events came out decomposed.
    pub fn rebuild_category_index(&mut self, owner: OwnerId) -> Result<u32, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let decomposed = category_index::rebuild(&transaction, owner)?;
        transaction.commit()?;
        Ok(decomposed)
    }

    /// The revision a stored `event_category_assignments` row must match to
    /// still be current: the highest active `category_rules.version` the
    /// owner has right now, or `0` with none (spec §4.7).
    pub fn category_index_revision(&self, owner: OwnerId) -> Result<i64, StoreError> {
        category_index::revision(&self.conn, owner)
    }
}
