//! Allocating returned basis on amortisation (§6.5).
//!
//! A separate trait and version, not an extension of
//! [`super::lot_disposal::LotDisposalRule`]: lot disposal is the owner's
//! choice (FIFO versus others), while amortisation is an issue event.
//! A shared version number would couple two independent decisions, and changing
//! the disposal method retroactively would rewrite amortisation history.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::lot_disposal::{DisposalError, Lot, split_basis};
use crate::money::Money;
use crate::numeric::NumericError;
use crate::rules::ReturnedShare;

/// Amortisation rule version. Separate from lot disposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AmortisationRuleVersion(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AmortisationError {
    #[error(transparent)]
    Numeric(#[from] NumericError),
    #[error(transparent)]
    Disposal(#[from] DisposalError),
}

/// How much of a lot's tax basis is returned with the principal.
pub trait AmortisationRule: Send + Sync + std::fmt::Debug {
    /// The argument is dimensionless: amounts cancel in the formula, so the
    /// rule need not know either initial principal or the remainder. The
    /// application has already computed the share and stored it in the fact.
    fn basis_returned(&self, lot: &Lot, share: ReturnedShare) -> Result<Money, AmortisationError>;
}

/// Share of the value proportional to the share of principal repaid.
#[derive(Debug, Default)]
pub struct ProRataV1;

impl AmortisationRule for ProRataV1 {
    fn basis_returned(&self, lot: &Lot, share: ReturnedShare) -> Result<Money, AmortisationError> {
        // Rounding and truncation live in `split_basis`: it solves exactly the
        // “share of an amount” problem with the “half to even” convention, and
        // a separate convention inside one core would produce two answers to
        // one question. The denominator is one: the share is already from the
        // pre-event remainder.
        Ok(split_basis(
            lot.cost_basis,
            share.inner().inner(),
            rust_decimal::Decimal::ONE,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::TradeDate;
    use crate::ids::InstrumentId;
    use crate::money::{CurrencyCode, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::rules::ReturnedShare;
    use crate::rules::lot_disposal::LotId;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn share(text: &str) -> ReturnedShare {
        ReturnedShare::new(dec(text)).expect("share is within the invariant")
    }

    fn lot(cost_basis: Money) -> Lot {
        Lot {
            id: LotId::new_random(),
            instrument: InstrumentId::new_random(),
            acquired: Some(TradeDate(date!(2026 - 01 - 10))),
            quantity: Quantity(dec("100")),
            cost_basis,
            acquisition_basis: None,
            accrued_interest_paid: None,
            received_to_date: None,
        }
    }

    #[test]
    fn a_fifth_of_the_remaining_principal_returns_a_fifth_of_the_basis() {
        let lot = lot(rub(100_000));
        assert_eq!(
            ProRataV1.basis_returned(&lot, share("0.2")).unwrap(),
            rub(20_000)
        );
    }

    #[test]
    fn the_whole_basis_comes_back_when_the_whole_remainder_does() {
        // The final amortisation returns the entire principal remainder.
        // The security remains in the position: its disposal is a separate
        // fact, not a consequence of the cash return.
        let lot = lot(rub(100_000));
        assert_eq!(
            ProRataV1.basis_returned(&lot, share("1")).unwrap(),
            rub(100_000)
        );
    }

    #[test]
    fn rounding_follows_the_half_to_even_convention_of_split_basis() {
        // 101 kopecks split in half is 50, not 51: the “half to even”
        // convention lives in `split_basis` and remains the only one
        // in the core.
        let lot = lot(rub(101));
        assert_eq!(
            ProRataV1.basis_returned(&lot, share("0.5")).unwrap(),
            rub(50)
        );
    }
}
