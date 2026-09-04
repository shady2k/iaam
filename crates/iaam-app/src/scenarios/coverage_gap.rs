//! The one place an [`EventKind::ImportCoverageGap`] is assembled.
//!
//! **What a gap says, and what it must never say.** It is a statement about one
//! import attempt: *this attempt was handed rows it did not take, so it cannot
//! confirm on its own the dimensions those rows would have moved.* It is not a
//! statement about the interval — the same operations may already be in the
//! journal from another channel — and it is not a statement about the document,
//! which this system never sees.
//!
//! That last line is why the module exists rather than a second copy of the
//! construction beside each writer. Two paths write gaps now: the broker sync,
//! which made the fetch and holds both sides, and an import session's commit
//! (iaam-bufs), which holds the rows a client sent and the ones it declined.
//! They differ in how they name an attempt and what provenance they stand
//! under, and they must not differ in what a gap **is**: that its dimensions
//! are derived from its rows rather than passed in beside them, that its
//! `refused` count is the number of rows it lists, and that a gap taints
//! nothing unless it names something. Those three are the core's structural
//! invariants for the variant, and a second construction site is how one path
//! comes to satisfy them while the other stops.

use std::collections::BTreeSet;

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::Provenance;
use iaam_core::event::source_row::RefusedRow;
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::AssertionPeriod;
use iaam_ingest::operation::OperationKind;

/// The account and interval one gap is about.
///
/// A struct rather than four arguments, for [`crate::scenarios::sync`]'s reason
/// on its own target: they travel together everywhere and three of the four are
/// the same shape, so an argument list is a place to transpose two of them.
pub(crate) struct GapTarget {
    pub owner: OwnerId,
    pub account: AccountId,
    pub period: AssertionPeriod,
}

/// What refusing an operation of this kind leaves unconfirmable.
///
/// Shared by both writers deliberately. The mapping is a claim about what a
/// kind of operation moves, and the same claim read two ways would let one
/// path taint `Income` for a refused coupon while the other tainted only
/// `Cash` — and then a status could reach `accepted_internal` depending on
/// which route had dropped the row.
///
/// A tax payment moves cash without changing any position's basis, so it is
/// `Cash` and not `TaxBasis`. A valuation changes no control dimension at all,
/// so refusing one taints nothing — which is why this returns a set that may be
/// empty, and why [`gap_event`] refuses to build a gap out of nothing but
/// those.
pub(crate) fn operation_dimensions(kind: &OperationKind) -> BTreeSet<Dimension> {
    match kind {
        OperationKind::Buy { .. } | OperationKind::Sell { .. } => {
            [Dimension::Cash, Dimension::Positions]
                .into_iter()
                .collect()
        }
        OperationKind::Income { .. } => [Dimension::Cash, Dimension::Income].into_iter().collect(),
        // Tax payments move cash without changing any position's basis; use Cash, not TaxBasis.
        OperationKind::Deposit { .. }
        | OperationKind::Withdrawal { .. }
        | OperationKind::Refund { .. }
        | OperationKind::Transfer { .. }
        // Cash, and only cash, whichever way it turns out to have run: the
        // dimension a row could have confirmed does not depend on a direction,
        // and an unresolved own-account movement moves the same dimension as a
        // resolved one.
        | OperationKind::OwnAccountMovement { .. }
        | OperationKind::Fee { .. }
        | OperationKind::Tax { .. }
        | OperationKind::OpeningCash { .. } => [Dimension::Cash].into_iter().collect(),
        OperationKind::OpeningPosition { .. } => [Dimension::Positions].into_iter().collect(),
        // Valuation changes no control dimension, so refusing it cannot taint
        // cash, positions, income, or tax-basis assertions.
        OperationKind::Valuation { .. } => BTreeSet::new(),
    }
}

/// The gap one attempt's refusals are, or nothing where they are not a fact.
///
/// `dimensions` and `refused` are **derived here and never passed in**. The
/// core's structural validation requires the union of the rows' dimensions to
/// equal `dimensions` and the count to equal `refused`, and two sources of
/// truth for one number eventually disagree — at which point the journal
/// refuses an append on a path that had already decided to write.
///
/// `None` on two conditions, and both are «this is not a fact»: no rows at all,
/// and rows that between them taint nothing. The second is the real one — an
/// attempt that refused only valuations has withheld no confirmation from
/// anybody, and a gap saying so would be a permanent record that nothing
/// happened. The caller decides what to do with `None`, which is always
/// nothing.
///
/// The provenance and the idempotency key are the caller's, because they are
/// the two things the two writers genuinely differ on: a sync attempt stands
/// under the broker channel it fetched through, an import commit under the
/// parser version its control assertions carry, and each names an attempt in
/// its own terms. What they may not differ on is everything above.
pub(crate) fn gap_event(
    target: GapTarget,
    rows: Vec<RefusedRow>,
    provenance: Provenance,
    idempotency_key: String,
) -> Option<Event> {
    let dimensions: BTreeSet<Dimension> = rows
        .iter()
        .flat_map(|row| row.dimensions.iter().copied())
        .collect();
    if rows.is_empty() || dimensions.is_empty() {
        return None;
    }
    let refused = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let GapTarget {
        owner,
        account,
        period,
    } = target;
    Some(Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::ImportCoverageGap {
            period,
            dimensions,
            refused,
            rows,
        },
        // Dated at the end of the interval it speaks about, as the assertions
        // it qualifies are: a gap that fell outside the period it taints would
        // be read by `reconciliation::observe` as an event belonging to no
        // period at all.
        dates: EventDates::for_cash(CashPostedDate(period.to)),
        // The sequence within the day is assigned by the store; this is the
        // temporary number every ingestion path passes.
        order: EffectiveOrder::new(period.to, 0),
        // No legs: a gap is a statement about an attempt and moves no money.
        legs: Vec::new(),
        provenance,
        relation: Relation::None,
        // `Confidence` describes the value, not its verification (§4.9): that
        // rows were refused is known.
        confidence: Confidence::Known,
        idempotency_key: Some(idempotency_key),
    })
}
