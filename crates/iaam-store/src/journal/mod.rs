//! The relational journal (spec §4).
//!
//! This module is introduced by Task 4 of the relational-journal plan and
//! grows one file per task. Task 4 wrote the pure row shapes and the
//! `EventKind` mapper ([`rows`], [`kind`]); Task 6 added [`read`], the
//! two-phase read's bulk-hydration half. Task 7 adds [`write`] — the one
//! insert primitive — and the `SqliteStore` methods below, which route every
//! write path through it. `events.rs` still owns the *read* side
//! (`load_events`, `list_journal_events` and friends) and its own
//! `find_duplicate`/`Appended`, which Task 7 leaves in place: Task 8 is what
//! replaces `events.rs` wholesale.
//!
//! No `pub(crate) use` re-export lives here: `write` is reached from outside
//! this module (`bundle.rs`) through the module-qualified path
//! `crate::journal::write::insert_event_in`, the same way a sibling
//! submodule inside this one reaches another's private items directly
//! (`read.rs` does that for `kind::from_rows`).
mod kind;
mod read;
mod rows;
pub(crate) mod write;

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::Event;
use iaam_core::reconciliation::evidence::IdentityScope;
use rusqlite::{TransactionBehavior, params};

use crate::events::{Appended, find_duplicate};
use crate::{SqliteStore, StoreError};

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
}
