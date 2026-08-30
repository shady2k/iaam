//! Source reconciliation assertions (§10.3).
//!
//! A broker report contains not only transactions but also reconciliation sections:
//! opening and closing balances, Dt/Kt turnover, security quantities,
//! totals for fees, coupons and dividends, and tax withheld. These are **source
//! facts**, not calculations, so they are recorded in the journal alongside
//! transactions — with provenance, parser version, and line locator.
//!
//! An assertion does not move money: the event has no legs, just like `Valuation`.
//! A leg here would mean that the reconciliation section was included in the balance
//! a second time.

use serde::{Deserialize, Serialize};
use time::Date;

use super::Dimension;
use crate::ids::{CustodyId, InstrumentId};
use crate::money::{CurrencyCode, PostedMinor, Quantity};

/// The interval covered by the assertion. Both boundaries are inclusive:
/// a report for March covers both the first and the thirty-first of March.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssertionPeriod {
    pub from: Date,
    pub to: Date,
}

impl AssertionPeriod {
    /// An interval whose start is later than its end cannot be created.
    ///
    /// Such an interval is not an «empty period», but an incorrectly parsed
    /// document: swapped dates produce a reconciliation that will never
    /// match anything and therefore remains a discrepancy forever.
    ///
    /// The check is not in `new`: `cargo-mutants` silently skips
    /// functions with that name (§15.7).
    #[must_use]
    pub fn between(from: Date, to: Date) -> Option<Self> {
        (from <= to).then_some(Self { from, to })
    }

    /// Whether the interval is valid.
    ///
    /// This is needed separately from the constructor: the event can also come from JSON, where
    /// the constructor was not called, and shape validation must check
    /// the state rather than assume that someone assembled it correctly.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.from <= self.to
    }

    #[must_use]
    pub fn contains(&self, date: Date) -> bool {
        self.from <= date && date <= self.to
    }
}

/// The point in the interval for which the balance assertion was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BalancePoint {
    /// Opening balance: the state **before** the first event in the interval.
    Opening,
    /// Closing balance: the state including the last event in the interval.
    Closing,
}

impl BalancePoint {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Closing => "closing",
        }
    }
}

/// What exactly the reconciliation section asserts.
///
/// Turnover and total values are **absolute values**: the side
/// (debit/credit) and field semantics carry the sign, not the number itself. A cash balance is
/// the exception: it may be negative, and that is a valid
/// state (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlClaim {
    /// Cash balance at the start or end of the interval.
    CashBalance {
        currency: CurrencyCode,
        amount: PostedMinor,
        at: BalancePoint,
    },
    /// Security quantity at the start or end of the interval.
    PositionQuantity {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        at: BalancePoint,
    },
    /// Account turnover over the interval, with both sides as absolute values.
    CashTurnover {
        currency: CurrencyCode,
        debit: PostedMinor,
        credit: PostedMinor,
    },
    /// Total fees over the interval.
    FeesTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
    /// Total coupons and dividends over the interval.
    IncomeTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
    /// Tax withheld by the tax agent over the interval.
    TaxWithheldTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
}

impl ControlClaim {
    /// Which dimension this assertion constrains (§10.3).
    ///
    /// Fees are assigned to money rather than income: a fee is a
    /// cash outflow, and it reconciles with the cash projection.
    /// Withheld tax is the only item that says anything about `TaxBasis`,
    /// and it does so only for the aggregate (rationale 8).
    #[must_use]
    pub const fn dimension(&self) -> Dimension {
        match self {
            Self::CashBalance { .. } | Self::CashTurnover { .. } | Self::FeesTotal { .. } => {
                Dimension::Cash
            }
            Self::PositionQuantity { .. } => Dimension::Positions,
            Self::IncomeTotal { .. } => Dimension::Income,
            Self::TaxWithheldTotal { .. } => Dimension::TaxBasis,
        }
    }

    /// Machine-readable name of the assertion kind.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::CashBalance { .. } => "cash_balance",
            Self::PositionQuantity { .. } => "position_quantity",
            Self::CashTurnover { .. } => "cash_turnover",
            Self::FeesTotal { .. } => "fees_total",
            Self::IncomeTotal { .. } => "income_total",
            Self::TaxWithheldTotal { .. } => "tax_withheld_total",
        }
    }

    /// The value that must be non-negative, and the name of its field.
    ///
    /// `None` means «a negative value is valid»: the cash
    /// balance (§11), and quantity, which is checked separately as a perimeter
    /// value rather than as the sign of a total.
    #[must_use]
    pub const fn non_negative_field(&self) -> Option<(&'static str, i64)> {
        match self {
            Self::CashBalance { .. } | Self::PositionQuantity { .. } => None,
            Self::CashTurnover { debit, credit, .. } => {
                // The smaller of the two sides is checked: if the smaller is non-negative,
                // both are non-negative. Taking the first one encountered
                // would allow a negative credit to pass
                // when the debit is positive.
                let smaller = if debit.raw() <= credit.raw() {
                    debit.raw()
                } else {
                    credit.raw()
                };
                Some(("turnover", smaller))
            }
            Self::FeesTotal { amount, .. }
            | Self::IncomeTotal { amount, .. }
            | Self::TaxWithheldTotal { amount, .. } => Some(("amount", amount.raw())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(amount: i64) -> PostedMinor {
        PostedMinor::new(amount)
    }

    #[test]
    fn an_inverted_period_is_not_constructed() {
        assert!(AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).is_some());
        assert!(AssertionPeriod::between(date!(2026 - 03 - 31), date!(2026 - 03 - 01)).is_none());
    }

    #[test]
    fn a_single_day_period_is_valid() {
        // A one-day report is a valid document, not a degenerate case.
        let day = date!(2026 - 03 - 15);
        let period = AssertionPeriod::between(day, day).unwrap();
        assert!(period.is_well_formed());
        assert!(period.contains(day));
    }

    #[test]
    fn period_boundaries_are_inclusive_on_both_ends() {
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        assert!(period.contains(date!(2026 - 03 - 01)));
        assert!(period.contains(date!(2026 - 03 - 31)));
        assert!(!period.contains(date!(2026 - 02 - 28)));
        assert!(!period.contains(date!(2026 - 04 - 01)));
    }

    #[test]
    fn a_period_built_around_the_constructor_is_recognised_as_malformed() {
        // This is exactly why the check is separate from
        // the constructor: the struct was assembled field by field, bypassing `between`.
        let inverted = AssertionPeriod {
            from: date!(2026 - 03 - 31),
            to: date!(2026 - 03 - 01),
        };
        assert!(!inverted.is_well_formed());
    }

    #[test]
    fn each_claim_constrains_exactly_one_dimension() {
        // The dimension is derived from the assertion kind rather than assigned
        // by the caller: an assignable dimension would allow a reconciled
        // balance to be declared confirmation of the tax basis.
        let cash = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: rub(100),
            at: BalancePoint::Closing,
        };
        let fees = ControlClaim::FeesTotal {
            currency: CurrencyCode::Rub,
            amount: rub(100),
        };
        let position = ControlClaim::PositionQuantity {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: Quantity(Dec::new(Decimal::from(10))),
            at: BalancePoint::Closing,
        };
        let income = ControlClaim::IncomeTotal {
            currency: CurrencyCode::Rub,
            amount: rub(100),
        };
        let tax = ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: rub(13),
        };

        assert_eq!(cash.dimension(), Dimension::Cash);
        assert_eq!(fees.dimension(), Dimension::Cash);
        assert_eq!(position.dimension(), Dimension::Positions);
        assert_eq!(income.dimension(), Dimension::Income);
        assert_eq!(tax.dimension(), Dimension::TaxBasis);
    }

    #[test]
    fn every_claim_kind_has_a_distinct_discriminant() {
        let claims = [
            ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: rub(1),
                at: BalancePoint::Opening,
            },
            ControlClaim::PositionQuantity {
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: Quantity(Dec::one()),
                at: BalancePoint::Opening,
            },
            ControlClaim::CashTurnover {
                currency: CurrencyCode::Rub,
                debit: rub(1),
                credit: rub(1),
            },
            ControlClaim::FeesTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
            ControlClaim::IncomeTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
            ControlClaim::TaxWithheldTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
        ];
        let mut names: Vec<&str> = claims.iter().map(ControlClaim::discriminant).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "assertion kind names collided");
    }

    #[test]
    fn a_turnover_reports_the_smaller_side_for_the_sign_check() {
        // The smaller of the two sides is checked regardless of which
        // one is negative: otherwise a negative credit with a
        // positive debit would pass validation.
        let claim = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: rub(500),
            credit: rub(-1),
        };
        assert_eq!(claim.non_negative_field(), Some(("turnover", -1)));

        let mirrored = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: rub(-1),
            credit: rub(500),
        };
        assert_eq!(mirrored.non_negative_field(), Some(("turnover", -1)));
    }

    #[test]
    fn each_balance_point_has_a_distinct_machine_readable_code() {
        // The balance point is exposed as a code: «at start» and «at end» are
        // distinct assertions, and using the same code would turn one into the other.
        assert_eq!(BalancePoint::Opening.code(), "opening");
        assert_eq!(BalancePoint::Closing.code(), "closing");
        assert_ne!(BalancePoint::Opening.code(), BalancePoint::Closing.code());
    }

    #[test]
    fn a_negative_cash_balance_is_not_a_sign_violation() {
        // §11: technical overdrafts and settlement timing can produce a negative balance,
        // and it must be included in NAV as a liability rather than rejected.
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: rub(-5_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(claim.non_negative_field(), None);
    }
}
