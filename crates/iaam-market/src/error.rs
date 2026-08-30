//! Parsing refusals.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MarketError {
    #[error("source response could not be parsed: {0}")]
    Malformed(String),
    /// Unknown currency code. A separate variant rather than `Malformed`:
    /// MOEX's `SUR` means the rouble, and silently turning an unfamiliar code
    /// into a parse error would hide the cause.
    #[error("unknown source currency code: {0}")]
    UnknownCurrency(String),
    /// A paginated response is truncated.
    ///
    /// A separate refusal rather than “accept whatever arrived”: treating an
    /// incomplete page as complete creates a gap that later cannot be
    /// distinguished from a non-trading day.
    #[error("page is incomplete: received {got} of {total}")]
    Truncated { got: usize, total: usize },
}
