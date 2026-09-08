//! One operation's correction chain, entered from anywhere in it (spec §10.6).
//!
//! A correction is not an edit: correcting an event writes a reversal naming
//! it and a replacement naming it, so what the owner calls one operation is a
//! run of facts and not a row. [`SqliteStore::event_chain`] returns that run.
//!
//! Neither chain statement selects `payload` any more — there is no such
//! column. Each selects `id` and `recorded_at`, the two columns the walk
//! needs beyond the event body itself, and the body comes from
//! [`super::hydrate`] in one bulk call rather than from decoding a JSON
//! document per row.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use iaam_core::event::{Event, Relation};
use iaam_core::ids::{EventId, OwnerId};
use rusqlite::{OptionalExtension, params};

use super::{hydrate, parse_uuid};
use crate::{SqliteStore, StoreError};

/// One fact of a chain, with the moment this instance wrote it down.
///
/// The pair is a store row and not a domain type. [`Event`] is what the journal
/// holds; when *this* instance happened to write it down is a fact about this
/// database, true of no copy of the journal but this one, and so it is not part
/// of the fact that was written. Whoever reads a chain needs both — "what
/// changed" comes off the event, "when did it change" off the stamp — so the
/// two travel together, and no further.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedEvent {
    /// The fact exactly as the journal holds it.
    pub event: Event,
    /// When this instance wrote the fact down: RFC 3339, from its own clock at
    /// insert.
    ///
    /// **Not the effective date, and never published as one.** A correction
    /// written in March to a fact effective in January is a March act about a
    /// January fact, and a reader that confused the two would date the owner's
    /// change of mind to the day of the operation he changed it about.
    pub recorded_at: String,
}

/// The backward step of a chain walk: the fact a relation names, if it is his.
///
/// The owner is in the clause and not merely in the identifier the caller
/// supplied: an event identifier is a UUID, and a UUID confers no right to read
/// someone else's journal (§14). A target belonging to another owner therefore
/// reads as no target at all, which from here is what it is.
pub const CHAIN_STEP_BACK_SQL: &str =
    "SELECT id, recorded_at FROM events WHERE owner = ?1 AND id = ?2";

/// The forward step: every fact naming this one as what it reverses or replaces.
///
/// `relation` points only backwards, so this is the whole of how a chain is
/// followed to its tail, and migration `0030`'s `events_by_relation_target` is
/// the whole of what keeps it off the journal.
///
/// Both statements are published so that a test can prove the planner answers
/// **these** queries out of an index. A test carrying its own copy of the SQL
/// would prove only that its copy is indexed, and the walk could become a full
/// scan without a single test noticing.
pub const CHAIN_STEP_FORWARD_SQL: &str =
    "SELECT id, recorded_at FROM events WHERE owner = ?1 AND relation_target = ?2";

impl SqliteStore {
    /// Every fact of one operation's correction chain, oldest first.
    ///
    /// **Any identifier in it answers with the same run** — the original, the
    /// replacement standing now, or a reversal written along the way. The walk
    /// therefore goes backwards by `relation` to the head first and only then
    /// forwards to the tail: the identifier he most often holds is the one a
    /// correction just handed back to him, and entering by that must not answer
    /// a different question from entering by the one his import gave him.
    ///
    /// Empty where the owner has no such event, which is the same answer for an
    /// identifier of nothing and for an identifier of somebody else's fact —
    /// deliberately, since telling those apart is telling him that a stranger's
    /// event exists.
    pub fn event_chain(
        &self,
        owner: OwnerId,
        event: EventId,
    ) -> Result<Vec<RecordedEvent>, StoreError> {
        let Some(entered) = self.recorded_event(owner, event)? else {
            return Ok(Vec::new());
        };
        let head = self.head_of_chain(owner, entered)?;
        self.chain_from(owner, head)
    }

    /// One fact of the owner's, by identifier.
    fn recorded_event(
        &self,
        owner: OwnerId,
        event: EventId,
    ) -> Result<Option<RecordedEvent>, StoreError> {
        let Some((id, recorded_at)) = self
            .conn
            .query_row(
                CHAIN_STEP_BACK_SQL,
                params![owner.inner().to_string(), event.inner().to_string()],
                stamped_id_row,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let id = EventId(parse_uuid(&id, "event")?);
        self.recorded_from(id, recorded_at)
    }

    /// Follow `relation` back until nothing is named: the fact the chain began at.
    ///
    /// A target the owner's journal does not hold ends the walk at the fact that
    /// names it rather than failing it. Such a target is not a fact of his, so
    /// there is nothing further of his to show, and refusing the whole history
    /// over a link that leads out of it would hide the corrections he can see.
    fn head_of_chain(
        &self,
        owner: OwnerId,
        entered: RecordedEvent,
    ) -> Result<RecordedEvent, StoreError> {
        let mut seen = BTreeSet::new();
        let mut current = entered;
        loop {
            if !seen.insert(current.event.id) {
                return Err(cycle_at(current.event.id));
            }
            let target = match current.event.relation {
                Relation::None => return Ok(current),
                Relation::Reversal { target } | Relation::Replacement { target } => target,
            };
            match self.recorded_event(owner, target)? {
                Some(named) => current = named,
                None => return Ok(current),
            }
        }
    }

    /// Follow the index forward from the head, depth first.
    ///
    /// Depth first rather than by the recorded moment: the order the two facts
    /// of one correction were written in is the order the caller happened to
    /// send them, and a client is free to send the replacement first. The shape
    /// of the chain is not free, so it is what the order is read off.
    fn chain_from(
        &self,
        owner: OwnerId,
        head: RecordedEvent,
    ) -> Result<Vec<RecordedEvent>, StoreError> {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![head];
        while let Some(current) = pending.pop() {
            if !seen.insert(current.event.id) {
                return Err(cycle_at(current.event.id));
            }
            let mut naming = self.corrections_naming(owner, current.event.id)?;
            chain.push(current);
            // The stack pops what was pushed last, so the earliest act is
            // pushed last and comes back out first.
            naming.reverse();
            pending.extend(naming);
        }
        Ok(chain)
    }

    /// The facts that name this one as what they reverse or replace.
    ///
    /// The reversal comes before the replacement, because that is the order the
    /// act happened in: the fact stopped counting, and then another took its
    /// place. A pair written the other way round is still one act, and reading
    /// it in the order it was stored would describe the same act differently
    /// depending on how the request was assembled.
    ///
    /// The stamp and then the identifier break the tie between two facts of the
    /// same half — two reversals of one target — and exist so that the order is
    /// total and does not depend on what the query planner returned first. The
    /// stamp is compared as text, which is time order for any two stamps a whole
    /// second apart: each is written here at UTC in the one RFC 3339 form. Past
    /// the seconds it is not, because that form omits the fractional part when
    /// the nanoseconds are zero and trims its trailing zeros otherwise, so
    /// `…:00.4Z` sorts before `…:00Z` on `'.'` against `'Z'`. Two facts written
    /// inside one second can therefore come back in the wrong order, and the
    /// identifier settles only the pair whose stamps are identical. Both are
    /// arbitrary at that scale and both are stable, which is what this ordering
    /// is for: the tie is between facts that are equally the same half of the
    /// same act.
    fn corrections_naming(
        &self,
        owner: OwnerId,
        target: EventId,
    ) -> Result<Vec<RecordedEvent>, StoreError> {
        let mut statement = self.conn.prepare(CHAIN_STEP_FORWARD_SQL)?;
        let rows = statement.query_map(
            params![owner.inner().to_string(), target.inner().to_string()],
            stamped_id_row,
        )?;
        let mut stamps = Vec::new();
        for row in rows {
            let (id, recorded_at) = row?;
            stamps.push((EventId(parse_uuid(&id, "event")?), recorded_at));
        }
        let mut naming = self.recorded_many(stamps)?;
        naming.sort_by(|left, right| {
            relation_order(&left.event)
                .cmp(&relation_order(&right.event))
                .then_with(|| left.recorded_at.cmp(&right.recorded_at))
                .then_with(|| left.event.id.cmp(&right.event.id))
        });
        Ok(naming)
    }

    /// One id-and-stamp pair, hydrated into its full [`RecordedEvent`].
    fn recorded_from(
        &self,
        id: EventId,
        recorded_at: String,
    ) -> Result<Option<RecordedEvent>, StoreError> {
        Ok(hydrate(&self.conn, std::slice::from_ref(&id))?
            .into_iter()
            .next()
            .map(|event| RecordedEvent { event, recorded_at }))
    }

    /// Several id-and-stamp pairs, hydrated in one bulk call — the forward
    /// step can name more than one fact (a reversal and a replacement both
    /// naming the same target), and hydrating them one at a time would defeat
    /// the whole point of a fixed-statement bulk load.
    fn recorded_many(
        &self,
        stamps: Vec<(EventId, String)>,
    ) -> Result<Vec<RecordedEvent>, StoreError> {
        let ids: Vec<EventId> = stamps.iter().map(|(id, _)| *id).collect();
        let recorded_at: BTreeMap<EventId, String> = stamps.into_iter().collect();
        let events = hydrate(&self.conn, &ids)?;
        Ok(events
            .into_iter()
            .filter_map(|event| {
                recorded_at
                    .get(&event.id)
                    .cloned()
                    .map(|recorded_at| RecordedEvent { event, recorded_at })
            })
            .collect())
    }
}

/// Read the id-and-stamp pair both chain statements select, as the raw
/// strings SQLite gives back — parsing the id into an [`EventId`] happens
/// after collection, alongside the other `StoreError` conversions.
fn stamped_id_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String)> {
    Ok((row.get(0)?, row.get(1)?))
}

/// Where a fact sits within the act that named its target.
const fn relation_order(event: &Event) -> u8 {
    match event.relation {
        Relation::Reversal { .. } => 0,
        Relation::Replacement { .. } => 1,
        // Unreachable through the forward statement, which selects only rows
        // carrying a target. Last rather than first, so that a row the schema
        // should not hold cannot displace the facts that explain the act.
        Relation::None => 2,
    }
}

fn cycle_at(event: EventId) -> StoreError {
    StoreError::CorrectionChainCycle {
        event: event.inner().to_string(),
    }
}
