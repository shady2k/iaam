//! Deriving ownership from a range of possible quantities.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::money::Quantity;
use crate::numeric::decimal::Dec;
use crate::settlement::{Applied, SettlementKnowledge};

/// Ownership status on a calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    NotOwned,
    Unknown,
}

/// A single quantity change and knowledge of its settlement date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct OwnershipChange {
    delta: Quantity,
    settlement: SettlementKnowledge,
}

/// Ownership history for an «account, instrument» pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipHistory {
    changes: Vec<OwnershipChange>,
}

impl OwnershipHistory {
    /// Add a quantity change with a known degree of settlement-date precision.
    pub fn observe(&mut self, delta: Quantity, settlement: SettlementKnowledge) {
        self.changes.push(OwnershipChange { delta, settlement });
    }

    /// Derive ownership from the minimum and maximum possible balances.
    ///
    /// The only possible error is an extra `Unknown`: an indeterminate
    /// event widens the range rather than producing a false `Owned`
    /// or `NotOwned`.
    #[must_use]
    pub fn ownership_at(&self, day: Date) -> Ownership {
        let mut minimum = Dec::zero();
        let mut maximum = Dec::zero();
        for change in &self.changes {
            match change.settlement.applied_before(day) {
                Applied::Yes => {
                    let Ok(next_minimum) = minimum.checked_add(change.delta.0) else {
                        return Ownership::Unknown;
                    };
                    let Ok(next_maximum) = maximum.checked_add(change.delta.0) else {
                        return Ownership::Unknown;
                    };
                    minimum = next_minimum;
                    maximum = next_maximum;
                }
                Applied::No => {}
                Applied::Maybe => {
                    if change.delta.0.is_negative() {
                        let Ok(next_minimum) = minimum.checked_add(change.delta.0) else {
                            return Ownership::Unknown;
                        };
                        minimum = next_minimum;
                    } else {
                        let Ok(next_maximum) = maximum.checked_add(change.delta.0) else {
                            return Ownership::Unknown;
                        };
                        maximum = next_maximum;
                    }
                }
            }
        }

        if minimum.is_positive() {
            Ownership::Owned
        } else if maximum.is_zero() {
            Ownership::NotOwned
        } else {
            Ownership::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use crate::settlement::SettlementKnowledge;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn quantity(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    #[test]
    fn overlapping_settlement_windows_make_ownership_unknown_without_crossing_zero_in_journal() {
        // Even with the journal transition 1→2→1, the sale could have settled
        // first, so the actual balance could have been zero on March 11 and 12.
        let mut history = OwnershipHistory::default();
        history.observe(
            quantity(1),
            SettlementKnowledge::Exact(date!(2026 - 03 - 01)),
        );
        history.observe(
            quantity(1),
            SettlementKnowledge::Bounded {
                earliest: date!(2026 - 03 - 10),
                latest: date!(2026 - 03 - 12),
            },
        );
        history.observe(
            quantity(-1),
            SettlementKnowledge::Bounded {
                earliest: date!(2026 - 03 - 11),
                latest: date!(2026 - 03 - 13),
            },
        );

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 05)),
            Ownership::Owned
        );
        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 11)),
            Ownership::Unknown
        );
        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 12)),
            Ownership::Unknown
        );
    }

    #[test]
    fn exact_settlement_has_no_false_boundary_at_the_settlement_date() {
        // On the settlement date, the closed interval yields Maybe, rather than an invented
        // start or end of the intraday transfer of rights.
        let date = date!(2026 - 03 - 10);
        let mut history = OwnershipHistory::default();
        history.observe(quantity(1), SettlementKnowledge::Exact(date));

        assert_eq!(
            history.ownership_at(date.previous_day().unwrap()),
            Ownership::NotOwned
        );
        assert_eq!(history.ownership_at(date), Ownership::Unknown);
        assert_eq!(
            history.ownership_at(date.next_day().unwrap()),
            Ownership::Owned
        );
    }

    #[test]
    fn exact_sale_is_owned_before_and_not_owned_after_its_settlement() {
        // A sale with an exact settlement date ends ownership only after that date.
        let bought = date!(2026 - 03 - 01);
        let sold = date!(2026 - 03 - 10);
        let mut history = OwnershipHistory::default();
        history.observe(quantity(1), SettlementKnowledge::Exact(bought));
        history.observe(quantity(-1), SettlementKnowledge::Exact(sold));

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 09)),
            Ownership::Owned
        );
        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 11)),
            Ownership::NotOwned
        );
    }

    #[test]
    fn an_unbounded_sale_from_a_bad_source_preserves_a_proven_positive_minimum() {
        // An unreliable source does not make the entire balance unknown: a sale of 10
        // without a date after an exact acquisition of 100 leaves a minimum of 90,
        // so proven ownership must remain Owned.
        let mut history = OwnershipHistory::default();
        history.observe(
            quantity(100),
            SettlementKnowledge::Exact(date!(2026 - 03 - 01)),
        );
        history.observe(quantity(-10), SettlementKnowledge::Unbounded);

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 11)),
            Ownership::Owned
        );
    }

    #[test]
    fn quantity_bounds_use_decimal_checked_arithmetic() {
        // Overflow must not be turned into plausible ownership: the safe
        // fallback is Unknown, because the exact range is no longer proven.
        let mut history = OwnershipHistory::default();
        history.observe(
            Quantity(Dec::new(Decimal::MAX)),
            SettlementKnowledge::Exact(date!(2026 - 03 - 01)),
        );
        history.observe(
            quantity(1),
            SettlementKnowledge::Exact(date!(2026 - 03 - 02)),
        );

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 03)),
            Ownership::Unknown
        );
    }
}
