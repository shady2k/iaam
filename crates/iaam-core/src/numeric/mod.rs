//! Three numerical modes (§6.6 of the specification).
//!
//! | Mode | Where | Type |
//! |---|---|---|
//! | Exact | result identity, basis allocation, reconciliation | [`exact::Exact`] |
//! | Monetary | amounts, prices, exchange rates, accrued interest | [`decimal::Dec`] |
//! | Approximate | XIRR, CAGR, DCF—powers, roots, iterations | [`approx`] |
//!
//! Approximate values **never** enter the monetary identity:
//! the identity checks amounts, not rates.

pub mod approx;
pub mod decimal;
pub mod exact;
pub mod xirr;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NumericError {
    #[error("denominator is zero")]
    ZeroDenominator,
    #[error("division by zero")]
    DivisionByZero,
    #[error("overflow during exact computation")]
    Overflow,
    #[error("scale {scale} exceeds the supported maximum {max}")]
    ScaleTooLarge { scale: u32, max: u32 },
}
