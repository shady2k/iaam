//! The relational journal (spec §4).
//!
//! This module is introduced by Task 4 of the relational-journal plan and
//! carries only the pure row shapes and the `EventKind` mapper for now:
//! [`rows`] and [`kind`]. It is not wired into any read or write path yet —
//! Tasks 6 and 7 add `read.rs`, `write.rs` and `query.rs`, and only then does
//! `events.rs` get replaced.
//!
//! Everything below is exercised today only by `kind`'s own round-trip
//! tests, so the ordinary build sees dead code and unused re-exports until
//! Tasks 6 and 7 call `to_rows`/`from_rows` from the read and write paths.
//! The allow is on the module rather than deleting the re-exports, so this
//! module's `pub(crate)` shape does not have to change again once that
//! wiring lands.
#![allow(dead_code, unused_imports)]

mod kind;
mod rows;

pub(crate) use kind::{from_rows, to_rows};
pub(crate) use rows::{
    CashTransferRow, ControlAssertionRow, CorporateActionRow, CoverageGapDetail,
    CoverageGapDimensionRow, CoverageGapRow, CoverageGapSourceRow, DetailRows, EventRow, FeeRow,
    IncomeRow, LegRow, OfferExerciseRow, OpeningPositionRow, TaxRow, TradeRow,
    UnresolvedMovementRow, ValuationRow,
};
