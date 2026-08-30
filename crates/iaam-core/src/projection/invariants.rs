//! Invariants as executable code (§15.2).
//!
//! An invariant violation is **not** the same as incomplete data.
//! Incompleteness produces a normal result plus a data-quality block;
//! an invariant violation invalidates the entire report: returning a number
//! with a warning after a proven identity violation is not allowed.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::lots::LotKey;
use super::state::LedgerState;
use crate::event::{Event, EventValidationError};
use crate::ids::EventId;
use crate::money::{Money, Quantity};
use crate::numeric::NumericError;

/// A verified invariant. The report shows exactly what was checked:
/// “invariants satisfied” without a list is indistinguishable from “not checked”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckedInvariant {
    /// The structure of each event matches its type.
    EventStructure { events: usize },
    /// The total lot quantity equals the position for each instrument.
    LotsMatchPositions { pairs: usize },
    /// Acquired = remaining + released, exactly, in minor units.
    BasisConserved { pairs: usize },
    /// No external flow has a zero amount.
    FlowsNonZero { flows: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvariantViolation {
    #[error("event {event:?} fails structural validation: {source}")]
    EventStructure {
        event: EventId,
        #[source]
        source: EventValidationError,
    },
    #[error(
        "the total lot quantity for {key:?} is {lots}, while the position from event legs is {position}; \
         two independent paths to the same quantity diverged"
    )]
    LotsDoNotMatchPosition {
        key: LotKey,
        lots: String,
        position: String,
    },
    #[error(
        "value is not conserved for {key:?}: acquired {acquired}, \
         remaining {remaining}, released {released}"
    )]
    BasisNotConserved {
        key: LotKey,
        acquired: i64,
        remaining: i64,
        released: i64,
    },
    #[error("external flow for event {event:?} has a zero amount")]
    ZeroExternalFlow { event: EventId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

impl InvariantViolation {
    /// Machine-readable code. An invariant violation is logged
    /// with a correlation identifier, while `not_computable` is returned externally (§15.2).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EventStructure { .. } => "event_structure",
            Self::LotsDoNotMatchPosition { .. } => "lots_do_not_match_position",
            Self::BasisNotConserved { .. } => "basis_not_conserved",
            Self::ZeroExternalFlow { .. } => "zero_external_flow",
            Self::Numeric(_) => "numeric",
        }
    }
}

/// Report on verified invariants.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantReport {
    checked: Vec<CheckedInvariant>,
}

impl InvariantReport {
    #[must_use]
    pub fn checked(&self) -> &[CheckedInvariant] {
        &self.checked
    }
}

/// Validation of all state invariants.
///
/// The core does not trust its input: events were already validated when written, but
/// the projection is also built from data retrieved from storage, and the storage
/// could have been populated by bypassing ingestion.
pub fn check(
    state: &LedgerState,
    events: &[&Event],
) -> Result<InvariantReport, InvariantViolation> {
    let mut checked = Vec::new();

    for event in events {
        event
            .validate_structure()
            .map_err(|source| InvariantViolation::EventStructure {
                event: event.id,
                source,
            })?;
    }
    checked.push(CheckedInvariant::EventStructure {
        events: events.len(),
    });

    let mut pairs = 0;
    let mut basis_pairs = 0;
    let keys: BTreeSet<LotKey> = state
        .book()
        .iter()
        .map(|(key, _)| *key)
        .chain(state.balances().iter_positions().map(|(key, _)| LotKey {
            account: key.account,
            instrument: key.instrument,
        }))
        .collect();
    for key in keys {
        let entry = state.book().entry(&key);
        let lots = entry
            .map(|entry| entry.quantity())
            .transpose()?
            .unwrap_or_else(Quantity::zero);
        let position = state.balances().quantity_of(key.account, key.instrument)?;
        if lots != position {
            return Err(InvariantViolation::LotsDoNotMatchPosition {
                key,
                lots: format!("{:?}", lots.0.inner()),
                position: format!("{:?}", position.0.inner()),
            });
        }
        pairs += 1;

        if let Some(entry) = entry {
            if let Some(acquired) = entry.acquired_basis() {
                let remaining = entry
                    .remaining_basis()
                    .map_err(|_| InvariantViolation::BasisNotConserved {
                        key,
                        acquired: acquired.amount().raw(),
                        remaining: 0,
                        released: 0,
                    })?
                    .unwrap_or_else(|| Money::zero(acquired.currency()));
                let released = entry
                    .released_basis()
                    .unwrap_or_else(|| Money::zero(acquired.currency()));
                let sum = remaining.amount().raw() + released.amount().raw();
                if sum != acquired.amount().raw() {
                    return Err(InvariantViolation::BasisNotConserved {
                        key,
                        acquired: acquired.amount().raw(),
                        remaining: remaining.amount().raw(),
                        released: released.amount().raw(),
                    });
                }
                basis_pairs += 1;
            }
        }
    }
    checked.push(CheckedInvariant::LotsMatchPositions { pairs });
    checked.push(CheckedInvariant::BasisConserved { pairs: basis_pairs });

    for flow in state.flows().external() {
        if flow.amount.is_zero() {
            return Err(InvariantViolation::ZeroExternalFlow { event: flow.event });
        }
    }
    checked.push(CheckedInvariant::FlowsNonZero {
        flows: state.flows().external().len(),
    });

    Ok(InvariantReport { checked })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourDefinition, ContourId, ContourVersion};
    use crate::event::kind::{EventKind, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::projection::lots::LotBook;
    use crate::projection::state::LedgerState;
    use crate::projection::{ProjectionContext, project};
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    /// The report must list WHAT was checked and with what quantities:
    /// “invariants satisfied” without numbers is indistinguishable from “not checked”.
    #[test]
    fn the_report_names_what_was_checked_and_how_much() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = vec![
            event_with(
                account,
                date!(2025 - 01 - 01),
                1,
                EventKind::CashIn {
                    amount: rub(10_000_000),
                },
                vec![Leg::cash(account, rub(10_000_000))],
            ),
            event_with(
                account,
                date!(2025 - 02 - 01),
                2,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross: rub(900_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(account, rub(-900_000)),
                    Leg::security(account, CustodyId::new_random(), instrument, qty(100)),
                ],
            ),
        ];

        let projection = project(&events, &ctx).unwrap();
        let report = projection.invariants();
        assert!(!report.checked().is_empty());
        assert_eq!(
            report.checked(),
            &[
                CheckedInvariant::EventStructure { events: 2 },
                CheckedInvariant::LotsMatchPositions { pairs: 1 },
                CheckedInvariant::BasisConserved { pairs: 1 },
                CheckedInvariant::FlowsNonZero { flows: 1 },
            ]
        );
    }

    #[test]
    fn basis_is_conserved_across_a_partial_sale_and_not_merely_when_nothing_is_sold() {
        // The identity “acquired = remaining + released” is checked where
        // both parts are nonzero. Until anything has been sold, the released
        // value is zero, and addition is indistinguishable from subtraction:
        // an incorrect sign would go unnoticed.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let custody = CustodyId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2025 - 01 - 01),
                1,
                EventKind::CashIn {
                    amount: rub(10_000_000),
                },
                vec![Leg::cash(account, rub(10_000_000))],
            ),
            event_with(
                account,
                date!(2025 - 02 - 01),
                2,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross: rub(1_000_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(account, rub(-1_000_000)),
                    Leg::security(account, custody, instrument, qty(100)),
                ],
            ),
            event_with(
                account,
                date!(2025 - 03 - 01),
                3,
                EventKind::Trade {
                    side: TradeSide::Sell,
                    instrument,
                    quantity: qty(40),
                    gross: rub(500_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(account, rub(500_000)),
                    Leg::security(account, custody, instrument, qty(-40)),
                ],
            ),
        ];

        let projection = project(&events, &ctx).unwrap();
        // Both parts are nonzero and differ: acquired 1 000 000,
        // released 400 000, remaining 600 000. The numbers were calculated manually.
        let entry = projection
            .snapshot
            .state()
            .book()
            .entry(&crate::projection::lots::LotKey {
                account,
                instrument,
            })
            .unwrap();
        assert_eq!(entry.acquired_basis(), Some(rub(1_000_000)));
        assert_eq!(entry.released_basis(), Some(rub(400_000)));
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(600_000)));
        assert!(
            projection
                .invariants()
                .checked()
                .contains(&CheckedInvariant::BasisConserved { pairs: 1 })
        );
    }

    #[test]
    fn an_empty_journal_still_reports_what_it_checked() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let projection = project(
            &[],
            &ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        )
        .unwrap();
        assert_eq!(
            projection.invariants().checked(),
            &[
                CheckedInvariant::EventStructure { events: 0 },
                CheckedInvariant::LotsMatchPositions { pairs: 0 },
                CheckedInvariant::BasisConserved { pairs: 0 },
                CheckedInvariant::FlowsNonZero { flows: 0 },
            ]
        );
    }

    #[test]
    fn lots_disagreeing_with_positions_abort_the_projection() {
        // §15.2: a violated invariant invalidates the entire report rather than
        // marking it with a warning. Two independent paths to the same
        // quantity—the position from event legs and the total lot quantity based on
        // the event type and disposal rule—must agree; a mismatch
        // means that one of them is wrong, and it is unknown which one.
        //
        // The mismatch is constructed manually because it can no longer be produced
        // through the journal: structural validation compares the leg with
        // the event and does not allow the contradiction into the journal at all. This
        // is correct and makes the invariant a second line of defense, not the first—
        // but a second line of defense that nobody checks is not a line of defense.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let mut state = LedgerState::new(LotBook::new(LotRuleVersion(1)));

        // A real purchase: it is recorded in both the balances and the lot book.
        let rules = RuleRegistry::with_defaults();
        let purchase = event_with(
            account,
            date!(2025 - 04 - 01),
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(10),
                gross: rub(1_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-1_000_000)),
                Leg::security(account, custody, instrument, qty(10)),
            ],
        );
        {
            let (balances, book, _) = state.parts_mut();
            balances.apply(&purchase).expect("balances");
            book.apply(&purchase, &rules).expect("lots");
        }

        // Now the security has “appeared” in the account outside the lot book—exactly
        // what a duplicated leg or a missing sale record would cause.
        let phantom = event_with(
            account,
            date!(2025 - 04 - 02),
            2,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::security(account, custody, instrument, qty(5))],
        );
        {
            let (balances, _, _) = state.parts_mut();
            balances.apply(&phantom).expect("leg applies");
        }

        let verdict = check(&state, &[]);
        let violation = verdict.expect_err("mismatch must invalidate the projection");
        assert!(matches!(
            violation,
            InvariantViolation::LotsDoNotMatchPosition { .. }
        ));
        assert_eq!(violation.code(), "lots_do_not_match_position");

        // This is propagated upward specifically as an invariant violation, not as
        // incomplete data: the former invalidates the report, while the latter marks
        // the value as not computable.
        assert!(crate::projection::ProjectionError::from(violation).is_invariant_violation());
    }

    #[test]
    fn a_position_without_a_single_lot_is_caught() {
        // This is a separate case: traversing only the lot book physically
        // cannot see a position for which there is no book entry at all. The test
        // serves as a regression guard for traversal over the union of keys.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let key = LotKey {
            account,
            instrument,
        };
        let mut state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let phantom = event_with(
            account,
            date!(2025 - 04 - 02),
            1,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::security(account, custody, instrument, qty(5))],
        );
        {
            let (balances, _, _) = state.parts_mut();
            balances.apply(&phantom).expect("leg applies");
        }

        assert!(state.book().entry(&key).is_none());
        let verdict = check(&state, &[]);
        let violation =
            verdict.expect_err("a position without a lot must invalidate the projection");
        assert!(matches!(
            violation,
            InvariantViolation::LotsDoNotMatchPosition { .. }
        ));
        assert_eq!(violation.code(), "lots_do_not_match_position");
    }

    #[test]
    fn every_violation_has_a_machine_readable_code() {
        let key = super::LotKey {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
        };
        assert_eq!(
            InvariantViolation::LotsDoNotMatchPosition {
                key,
                lots: "1".into(),
                position: "2".into(),
            }
            .code(),
            "lots_do_not_match_position"
        );
        assert_eq!(
            InvariantViolation::BasisNotConserved {
                key,
                acquired: 1,
                remaining: 0,
                released: 0,
            }
            .code(),
            "basis_not_conserved"
        );
        assert_eq!(
            InvariantViolation::ZeroExternalFlow {
                event: crate::ids::EventId::new_random(),
            }
            .code(),
            "zero_external_flow"
        );
        assert_eq!(
            InvariantViolation::Numeric(crate::numeric::NumericError::Overflow).code(),
            "numeric"
        );
    }
}
