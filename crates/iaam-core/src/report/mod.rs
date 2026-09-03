//! Report answers and what they leave unsaid.
//!
//! A report is two statements: the figures, and how much of the owner's money
//! they are about. The second one is made **before** the fold — selection
//! happens first — so nothing computed downstream can see what was left out,
//! and each of a report's quality fields can be clean while the accounts chosen
//! for it were the wrong ones.
//!
//! [`confidence`] is the register that says both at once: one statement per
//! report of what would have to be true for its figures to be complete, and
//! which of those are not. It lives in the core, beside the numbers, because a
//! summary assembled by the transport can disagree with the report it
//! summarises.

pub mod assets;
pub mod balances;
pub mod confidence;
pub mod population;
