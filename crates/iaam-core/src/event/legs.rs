//! Expectations for event legs (§15.2).
//!
//! Existing helpers `expect_single_cash` and `validate_trade` compare
//! the leg kind, amount and sign — but not the account, security or custody location.
//! A guard that accepts a leg for another security is decorative: the event
//! with an unrelated movement is not the event it claims to be.

use super::leg::{Leg, LegKind};
use super::{Event, EventValidationError};
use crate::ids::{AccountId, CustodyId, InstrumentId};
use crate::money::{Money, Quantity};

/// Expectation for one leg.
///
/// An unset field is not checked — a set one must match.
/// Kind and account are required: without them, a leg is not described at all.
#[derive(Debug, Clone, PartialEq)]
pub struct LegExpectation {
    pub kind: LegKind,
    pub account: AccountId,
    pub instrument: Option<InstrumentId>,
    pub custody: Option<CustodyId>,
    pub money: Option<Money>,
    pub quantity: Option<Quantity>,
}

impl Event {
    /// **Exactly** the listed legs, in any order.
    ///
    /// An extra leg is as much an error as a missing one: an event
    /// with an unrelated movement is not the event it
    /// claims to be. Leg order is not checked: the source may record
    /// them however it likes.
    pub fn expect_legs(
        &self,
        name: &'static str,
        expected: &[LegExpectation],
    ) -> Result<(), EventValidationError> {
        let mut taken = vec![false; self.legs.len()];
        if self.legs.len() == expected.len() && assign(&self.legs, expected, &mut taken) {
            return Ok(());
        }
        Err(self.diagnose(name, expected))
    }

    /// Why the matching failed. Diagnostic precision matters: «wrong
    /// number of legs» tells the reader of an import rejection nothing.
    fn diagnose(&self, name: &'static str, expected: &[LegExpectation]) -> EventValidationError {
        let found = self.legs.len();
        let want = expected.len();
        // An expectation that no leg matches is the most precise
        // complaint: it names the field, not the count.
        for expectation in expected {
            if self.legs.iter().any(|leg| matches(leg, expectation)) {
                continue;
            }
            // Since no leg matched, every leg of the same kind
            // has a mismatched field — and its name is the diagnosis.
            let mismatch = self
                .legs
                .iter()
                .filter(|leg| leg.kind == expectation.kind)
                .find_map(|leg| first_difference(leg, expectation));
            return match mismatch {
                Some(field) => EventValidationError::LegMismatch {
                    event: name,
                    kind: expectation.kind,
                    field,
                },
                None => EventValidationError::MissingLeg {
                    event: name,
                    kind: expectation.kind,
                    expected: want,
                    found,
                },
            };
        }
        // Each expectation can be met separately — so the problem
        // is the number of legs: either an extra leg remains, or two expectations
        // claim the same one.
        if found < want {
            return EventValidationError::MissingLeg {
                event: name,
                kind: expected[want - 1].kind,
                expected: want,
                found,
            };
        }
        EventValidationError::UnexpectedLeg {
            event: name,
            expected: want,
            found,
        }
    }
}

/// Exhaustive backtracking, not greedy matching.
///
/// A greedy algorithm would match the unset expectation to the first matching leg
/// and declare the event invalid, although a matching exists.
/// Events have only a few legs, so the search cost is negligible.
fn assign(legs: &[Leg], expected: &[LegExpectation], taken: &mut [bool]) -> bool {
    let Some((first, rest)) = expected.split_first() else {
        return true;
    };
    for (index, leg) in legs.iter().enumerate() {
        if taken[index] || !matches(leg, first) {
            continue;
        }
        taken[index] = true;
        if assign(legs, rest, taken) {
            return true;
        }
        taken[index] = false;
    }
    false
}

fn matches(leg: &Leg, expectation: &LegExpectation) -> bool {
    first_difference(leg, expectation).is_none()
}

/// The name of the first mismatched field, if any. Field order
/// is fixed: the diagnosis must be reproducible, otherwise the same
/// defect is explained differently each time.
fn first_difference(leg: &Leg, expectation: &LegExpectation) -> Option<&'static str> {
    if leg.kind != expectation.kind {
        return Some("kind");
    }
    if leg.account != expectation.account {
        return Some("account");
    }
    if differs(leg.instrument, expectation.instrument) {
        return Some("instrument");
    }
    if differs(leg.custody, expectation.custody) {
        return Some("custody");
    }
    if differs(leg.money, expectation.money) {
        return Some("money");
    }
    if differs(leg.quantity, expectation.quantity) {
        return Some("quantity");
    }
    None
}

/// An unset expectation is not checked; a set one must match,
/// including an empty leg field.
fn differs<T: PartialEq>(actual: Option<T>, wanted: Option<T>) -> bool {
    wanted.is_some_and(|wanted| actual != Some(wanted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Leg;
    use crate::event::kind::EventKind;
    use crate::event::test_support::event_with;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(text: &str) -> Quantity {
        Quantity(Dec::new(Decimal::from_str_exact(text).unwrap()))
    }

    fn with_legs(account: AccountId, legs: Vec<Leg>) -> Event {
        event_with(
            account,
            date!(2026 - 03 - 01),
            0,
            EventKind::CashIn { amount: rub(1) },
            legs,
        )
    }

    fn principal_expectation(
        account: AccountId,
        instrument: InstrumentId,
        money: Money,
    ) -> LegExpectation {
        LegExpectation {
            kind: LegKind::Principal,
            account,
            instrument: Some(instrument),
            custody: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[test]
    fn a_leg_naming_another_instrument_is_refused() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();
        let event = with_legs(account, vec![Leg::principal(account, other, rub(100_000))]);

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::Principal,
                field: "instrument",
            })
        );
    }

    #[test]
    fn an_extra_leg_is_refused_like_a_missing_one() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![
                Leg::principal(account, instrument, rub(100_000)),
                Leg::cash(account, rub(1)),
            ],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::UnexpectedLeg {
                event: "x",
                expected: 1,
                found: 2,
            })
        );
    }

    #[test]
    fn a_leg_of_a_kind_that_is_not_there_at_all_is_reported_as_missing() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(account, vec![Leg::cash(account, rub(100_000))]);

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::MissingLeg {
                event: "x",
                kind: LegKind::Principal,
                expected: 1,
                found: 1,
            })
        );
    }

    #[test]
    fn a_leg_held_in_another_custody_is_refused() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let other = CustodyId::new_random();
        let event = with_legs(
            account,
            vec![Leg::security(account, other, instrument, qty("10"))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[LegExpectation {
                    kind: LegKind::SecurityQuantity,
                    account,
                    instrument: Some(instrument),
                    custody: Some(custody),
                    money: None,
                    quantity: Some(qty("10")),
                }]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::SecurityQuantity,
                field: "custody",
            })
        );
    }

    #[test]
    fn a_quantity_of_the_wrong_sign_is_refused() {
        // An outflow is recorded as a negative quantity: the same magnitude
        // with the opposite sign is the opposite movement, not a typo.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let event = with_legs(
            account,
            vec![Leg::security(account, custody, instrument, qty("10"))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[LegExpectation {
                    kind: LegKind::SecurityQuantity,
                    account,
                    instrument: Some(instrument),
                    custody: Some(custody),
                    money: None,
                    quantity: Some(qty("-10")),
                }]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::SecurityQuantity,
                field: "quantity",
            })
        );
    }

    #[test]
    fn a_leg_booked_to_another_account_is_refused() {
        let account = AccountId::new_random();
        let other = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![Leg::principal(other, instrument, rub(100_000))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::Principal,
                field: "account",
            })
        );
    }

    #[test]
    fn a_leg_carrying_another_amount_is_refused() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![Leg::principal(account, instrument, rub(99_999))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[principal_expectation(account, instrument, rub(100_000))]
            ),
            Err(EventValidationError::LegMismatch {
                event: "x",
                kind: LegKind::Principal,
                field: "money",
            })
        );
    }

    #[test]
    fn matching_legs_pass_regardless_of_the_order_they_were_written_in() {
        // Leg order within an event has no meaning: the source may record
        // them however it likes, and an order-dependent guard would be checking order.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![
                Leg::cash(account, rub(1_000)),
                Leg::principal(account, instrument, rub(100_000)),
            ],
        );

        let expectations = [
            principal_expectation(account, instrument, rub(100_000)),
            LegExpectation {
                kind: LegKind::Cash,
                account,
                instrument: None,
                custody: None,
                money: Some(rub(1_000)),
                quantity: None,
            },
        ];

        assert_eq!(event.expect_legs("x", &expectations), Ok(()));
    }

    #[test]
    fn two_expectations_never_settle_on_the_same_leg() {
        // Greedy matching would match both legs to one expectation
        // and declare the event valid, losing the second leg.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![
                Leg::principal(account, instrument, rub(100_000)),
                Leg::principal(account, instrument, rub(200_000)),
            ],
        );

        let loose = LegExpectation {
            kind: LegKind::Principal,
            account,
            instrument: Some(instrument),
            custody: None,
            money: None,
            quantity: None,
        };

        // An unset expectation matches both legs; the set one —
        // only one. The matching must find it, not give up.
        assert_eq!(
            event.expect_legs(
                "x",
                &[
                    loose.clone(),
                    principal_expectation(account, instrument, rub(200_000))
                ]
            ),
            Ok(())
        );
    }

    #[test]
    fn two_expectations_and_one_matching_leg_report_a_missing_leg() {
        // Each expectation can be met separately, but together —
        // they cannot: one leg cannot satisfy two expected legs.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![Leg::principal(account, instrument, rub(100_000))],
        );
        let expectation = principal_expectation(account, instrument, rub(100_000));

        assert_eq!(
            event.expect_legs("x", &[expectation.clone(), expectation]),
            Err(EventValidationError::MissingLeg {
                event: "x",
                kind: LegKind::Principal,
                expected: 2,
                found: 1,
            })
        );
    }

    #[test]
    fn equal_counts_that_do_not_pair_up_report_an_unexpected_leg() {
        // There are as many legs as expectations, but no matching exists: both
        // claim the same leg, while the second remains extra.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![
                Leg::principal(account, instrument, rub(100_000)),
                Leg::cash(account, rub(1)),
            ],
        );
        let expectation = principal_expectation(account, instrument, rub(100_000));

        assert_eq!(
            event.expect_legs("x", &[expectation.clone(), expectation]),
            Err(EventValidationError::UnexpectedLeg {
                event: "x",
                expected: 2,
                found: 2,
            })
        );
    }

    #[test]
    fn an_unfilled_expectation_field_is_not_checked() {
        // An unset field — «not checked», not «must be empty».
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let event = with_legs(
            account,
            vec![Leg::principal(account, instrument, rub(100_000))],
        );

        assert_eq!(
            event.expect_legs(
                "x",
                &[LegExpectation {
                    kind: LegKind::Principal,
                    account,
                    instrument: None,
                    custody: None,
                    money: None,
                    quantity: None,
                }]
            ),
            Ok(())
        );
    }

    #[test]
    fn an_event_with_no_legs_meets_an_empty_expectation() {
        // A submitted tender offer order has no legs: it moves no cash, nor does it move
        // securities. The guard must confirm this, not reject it.
        let account = AccountId::new_random();
        let event = with_legs(account, Vec::new());
        assert_eq!(event.expect_legs("x", &[]), Ok(()));
    }

    #[test]
    fn a_leg_where_none_was_expected_is_refused() {
        let account = AccountId::new_random();
        let event = with_legs(account, vec![Leg::cash(account, rub(1))]);
        assert_eq!(
            event.expect_legs("x", &[]),
            Err(EventValidationError::UnexpectedLeg {
                event: "x",
                expected: 0,
                found: 1,
            })
        );
    }
}
