//! The relational journal (spec §4).
//!
//! This module is introduced by Task 4 of the relational-journal plan and
//! grows one file per task. Task 4 wrote the pure row shapes and the
//! `EventKind` mapper ([`rows`], [`kind`]); Task 6 adds [`read`], the
//! two-phase read's bulk-hydration half. Task 7 adds `write.rs` — the
//! insert primitive — and only then does `events.rs` get replaced.
//!
//! No `pub(crate) use` re-export lives here: nothing outside this module
//! calls into it yet, and a sibling submodule reaches another one's private
//! items directly through a module-qualified path (`read.rs` does exactly
//! that for `kind::from_rows`) without needing one. Tasks 7 and 8 add the
//! `SqliteStore` methods this module's doc line above still promises, and
//! those methods — defined here, in `mod.rs` — can call `read::hydrate(...)`
//! and `kind::to_rows(...)` the same way, so a re-export is not expected to
//! become necessary later either; if one turns out to be, that task adds it
//! together with its first user, not in advance of one.
//!
//! Two items are still unreachable from outside their own `#[cfg(test)]`
//! code and carry an individually named `#[allow(dead_code)]` rather than a
//! module-wide one:
//! - [`kind::to_rows`] — Task 7's write primitive is its production caller.
//! - [`read::hydrate`] and [`read::hydrate_one`] — Task 8's query builder
//!   is what selects the ids they are meant to be called with.
mod kind;
mod read;
mod rows;
