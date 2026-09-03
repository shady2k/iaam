//! Investment accounting core.
//!
//! Pure synchronous functions over a loaded data slice.
//! No I/O, `async`, `Mutex`, or dependencies on other workspace crates.
//! See §3.1 of the specification.

pub mod bond;
pub mod category;
pub mod contour;
pub mod dates;
pub mod event;
pub mod ids;
pub mod instrument;
pub mod money;
pub mod numeric;
pub mod perimeter;
pub mod projection;
pub mod reconciliation;
pub mod report;
pub mod returns;
pub mod rules;
pub mod settlement;
pub mod valuation;
