//! Metamorphic reconciliation tests (§15.6).
//!
//! A metamorphic relation checks behavior under a known input transformation,
//! not a specific number. That is the key here: reconciliation must
//! **not detect** a compensating parsing error—and must not
//! present as independent confirmation something that is not independent.

use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use time::macros::date;

mod support;
use support::{Posting, TestChannel, event_on};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

/// A single-document journal: an operation and a complete set of control sections
/// consistent with one another.
///
/// `shift` shifts **both** sides at once—this is the compensating
/// parser error: the same parsing error affected both the operation
/// and the control section.
fn statement(owner: OwnerId, account: AccountId, channel: &TestChannel, shift: i64) -> Vec<Event> {
    let deposit = 100_000 + shift;
    let mut events = vec![event_on(
        channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 10),
            sequence: 1,
        },
        EventKind::CashIn {
            amount: rub(deposit),
        },
        vec![Leg::cash(account, rub(deposit))],
    )];
    for (index, claim) in [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(0),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(deposit),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(deposit),
            credit: PostedMinor::new(0),
        },
    ]
    .into_iter()
    .enumerate()
    {
        events.push(event_on(
            channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: u32::try_from(index).unwrap() + 10,
            },
            EventKind::ControlAssertion {
                period: march(),
                claim,
            },
            vec![],
        ));
    }
    events
}

#[test]
fn a_compensating_parser_error_never_reaches_independent() {
    // The epic acceptance criterion. The parser made the same seven-kopeck error
    // in the operation and the control section: both sides shifted, reconciliation
    // still matched, and this is exactly the case for which §10.3 introduces
    // a third level instead of two. The status must remain internal.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    let honest = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        0,
    ))
    .unwrap();
    let skewed = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        7,
    ))
    .unwrap();

    let honest_status = honest.status_for(account, date!(2026 - 03 - 15), Dimension::Cash);
    let skewed_status = skewed.status_for(account, date!(2026 - 03 - 15), Dimension::Cash);

    assert_eq!(
        honest_status, skewed_status,
        "reconciliation within a single document cannot distinguish a correct parse from \
         an incorrect parse with compensating errors—and that is exactly why it must not \
         call it independent"
    );
    assert_eq!(skewed_status, DimensionStatus::AcceptedInternal);
    assert_ne!(skewed_status, DimensionStatus::AcceptedIndependent);
}

#[test]
fn a_second_channel_catches_what_one_document_cannot() {
    // Conversely, an error that is invisible within the document becomes
    // visible as soon as an independent channel is introduced. If this
    // were not the case, the second channel would be pointless.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    let mut events = statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        7,
    );
    // The second channel sees the correct amount—and disagrees with the journal,
    // which contains the error from the first channel.
    let api = TestChannel::new("tinkoff-api/1", "apimarch");
    events.push(event_on(
        &api,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 31),
            sequence: 20,
        },
        EventKind::ControlAssertion {
            period: march(),
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(100_000),
                at: BalancePoint::Closing,
            },
        },
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Discrepant,
        "an independent channel must catch the error that the document \
         concealed from itself"
    );
}

#[test]
fn reordering_the_journal_does_not_change_any_status() {
    // The projection is determined by the journal, not by its read order.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let events = statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        0,
    );
    let mut reversed = events.clone();
    reversed.reverse();

    let straight = ReconciliationLedger::build(&events).unwrap();
    let backwards = ReconciliationLedger::build(&reversed).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            straight.status_for(account, date!(2026 - 03 - 15), dimension),
            backwards.status_for(account, date!(2026 - 03 - 15), dimension),
            "journal read order changed the status of dimension {dimension:?}"
        );
    }
}

#[test]
fn scaling_every_amount_keeps_the_status() {
    // Multiplying all amounts by the same number is a transformation under which
    // reconciliation must behave identically: it compares the two sides,
    // rather than assessing the scale.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let small = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        0,
    ))
    .unwrap();
    let large = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        900_000,
    ))
    .unwrap();
    assert_eq!(
        small.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        large.status_for(account, date!(2026 - 03 - 15), Dimension::Cash)
    );
}
