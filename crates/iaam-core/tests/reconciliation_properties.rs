//! Cross-checking properties with their domains of applicability (§15.3).
//!
//! The properties are formulated from the rules in §10.3, not inferred by running
//! the program (§15.5). Each includes a qualification specifying where it
//! applies: a property without a domain causes false failures, which are
//! most easily addressed by weakening the generator into a tautology.

use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use proptest::prelude::*;
use time::macros::date;

mod support;
use support::{Posting, TestChannel, event_on};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

/// A log containing several documents parsed by **the same parser**.
///
/// The documents have different names, arbitrary amounts, and control sections
/// consistent with the operation — so the cross-check will succeed. This input
/// specifically verifies that agreement within a single parsing implementation does not
/// elevate the status to independent.
fn one_parser_journal(deposits: &[i64]) -> (AccountId, Vec<Event>) {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let total: i64 = deposits.iter().sum();
    let mut events = Vec::new();

    for (index, amount) in deposits.iter().enumerate() {
        let channel = TestChannel::new("same/1", &format!("doc{index}"));
        events.push(event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 10),
                sequence: u32::try_from(index).unwrap() + 1,
            },
            EventKind::CashIn {
                amount: rub(*amount),
            },
            vec![Leg::cash(account, rub(*amount))],
        ));
    }
    // The last document's control sections agree with the result. The opening
    // balance is among them, as it is in a real control section: without it the
    // closing figure is a sum from a start nothing asserts and is not compared
    // at all (`iaam-d7hn`), which would make every property below vacuous.
    let channel = TestChannel::new("same/1", "control");
    for (index, claim) in [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(0),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(total),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(total),
            credit: PostedMinor::new(0),
        },
    ]
    .into_iter()
    .enumerate()
    {
        events.push(event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: u32::try_from(index).unwrap() + 100,
            },
            EventKind::ControlAssertion {
                period: march(),
                claim,
            },
            vec![],
        ));
    }
    (account, events)
}

proptest! {
        /// **Domain:** logs whose channels all share the same parser version.
    ///
        /// Rule §10.3: corroborating data must not pass through
        /// the same parsing implementation. As long as there is only one parser, there is no
        /// independence regardless of whether the figures agree — no matter how many documents agree.
    #[test]
    fn one_parser_never_reaches_independent(
        deposits in prop::collection::vec(1_i64..=1_000_000, 1..=5)
    ) {
        let (account, events) = one_parser_journal(&deposits);
        let ledger = ReconciliationLedger::build(&events).unwrap();
        for dimension in Dimension::all() {
            let status = ledger.status_for(account, date!(2026 - 03 - 15), dimension);
            prop_assert_ne!(
                status,
                DimensionStatus::AcceptedIndependent,
                    "measurement {:?} was declared independently corroborated using a single parser",
                dimension
            );
        }
    }

        /// **Domain:** logs in which exactly one assertion is known to
        /// disagree while all others agree.
    ///
        /// A mismatch is absorbing: corroboration does not overwrite the mismatched
        /// figure, no matter how many agreeing assertions accompany it.
    #[test]
    fn a_single_discrepancy_absorbs_any_number_of_confirmations(
        deposits in prop::collection::vec(1_i64..=1_000_000, 1..=5),
        skew in 1_i64..=999_999,
    ) {
        let (account, mut events) = one_parser_journal(&deposits);
        let total: i64 = deposits.iter().sum();
        let owner = OwnerId::new_random();
        let channel = TestChannel::new("same/1", "broken");
        events.push(event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: 200,
            },
            EventKind::ControlAssertion {
                period: march(),
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(total + skew),
                    at: BalancePoint::Closing,
                },
            },
            vec![],
        ));

        let ledger = ReconciliationLedger::build(&events).unwrap();
        prop_assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
            DimensionStatus::Discrepant
        );
    }

        /// **Domain:** any log of control assertions.
    ///
        /// The registry is a pure function: the same input produces the same output. Without
        /// this, the figure shown to the owner cannot be reproduced (§3.1).
    #[test]
    fn the_ledger_is_deterministic(
        deposits in prop::collection::vec(1_i64..=1_000_000, 1..=5)
    ) {
        let (account, events) = one_parser_journal(&deposits);
        let first = ReconciliationLedger::build(&events).unwrap();
        let second = ReconciliationLedger::build(&events).unwrap();
        for dimension in Dimension::all() {
            prop_assert_eq!(
                first.status_for(account, date!(2026 - 03 - 15), dimension),
                second.status_for(account, date!(2026 - 03 - 15), dimension)
            );
        }
    }
}
