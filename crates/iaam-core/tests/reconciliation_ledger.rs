//! Account completeness status over an interval, by dimension (§10.3).

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

fn deposit(channel: &TestChannel, owner: OwnerId, account: AccountId, minor: i64) -> Event {
    event_on(
        channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 10),
            sequence: 1,
        },
        EventKind::CashIn { amount: rub(minor) },
        vec![Leg::cash(account, rub(minor))],
    )
}

/// Control figures for a single document.
struct Sections {
    opening: i64,
    closing: i64,
    debit: i64,
    credit: i64,
}

/// Complete set of control sections for a single document: opening balance,
/// closing balance, and turnover. Exactly this set provides basis 5.
///
/// The assertion date is the end of the interval: the control section refers to
/// the period as a whole, so there is no need to provide it as a separate argument.
fn full_sections(
    channel: &TestChannel,
    owner: OwnerId,
    account: AccountId,
    period: AssertionPeriod,
    sections: Sections,
) -> Vec<Event> {
    [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(sections.opening),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(sections.closing),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(sections.debit),
            credit: PostedMinor::new(sections.credit),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, claim)| {
        event_on(
            channel,
            Posting {
                owner,
                account,
                day: period.to,
                sequence: u32::try_from(index).unwrap() + 10,
            },
            EventKind::ControlAssertion { period, claim },
            vec![],
        )
    })
    .collect()
}

#[test]
fn separate_sections_that_all_agree_raise_the_period_to_internal() {
    // Basis 5: independent equations, but a single document and a single
    // parser. By design, internal cannot be promoted any higher.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::TaxBasis),
        DimensionStatus::Provisional,
        "the tax basis is not confirmed by the cash balance"
    );
}

#[test]
fn one_agreeing_section_is_not_enough_for_ground_five() {
    // A single reconciled balance is not agreement between independent
    // equations: it confirms itself. Basis 5 requires both
    // the balance and turnover to reconcile—quantities calculated in different ways.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let events = vec![
        deposit(&march_channel, owner, account, 100_000),
        event_on(
            &march_channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: 10,
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
        ),
    ];

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Provisional
    );
}

#[test]
fn a_discrepancy_wins_over_any_amount_of_confirmation() {
    // Confirmation does not overwrite an unreconciled figure. Otherwise, it would be enough
    // to attach a second document for the discrepancy to disappear
    // from the screen while remaining in the data.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    // Turnover will reconcile, but the ending balance will not.
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 999_999,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Discrepant
    );
}

#[test]
fn two_independent_channels_over_the_same_period_reach_independent() {
    // Basis 3. The same period, the same figures, a different parser and a different
    // document—the independence requirement of §10.3 is met.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let apimarch_channel = TestChannel::new("tinkoff-api/1", "apimarch");
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));
    events.extend(full_sections(
        &apimarch_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn two_statements_of_the_same_parser_never_reach_independent() {
    // The literal wording of §10.3. Two different documents from the same broker,
    // parsed by the same parser, provide continuity, not
    // independence.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let copyone_channel = TestChannel::new("tinkoff-xlsx/1", "copyone");
    let copytwo_channel = TestChannel::new("tinkoff-xlsx/1", "copytwo");
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &copyone_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));
    events.extend(full_sections(
        &copytwo_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
}

#[test]
fn the_opening_of_the_next_statement_confirms_the_previous_period() {
    // Basis 1. The April report begins with the balance that
    // we calculated for March: MARCH is confirmed, not April—there is
    // nothing to confirm in April yet.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let april_channel = TestChannel::new("tinkoff-xlsx/1", "april");
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let april = AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30)).unwrap();

    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    // March: ending balance only, no turnover—does not qualify for basis 5.
    events.push(event_on(
        &march_channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 31),
            sequence: 10,
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
    // April: the opening balance matches the calculated March balance.
    events.push(event_on(
        &april_channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 04 - 30),
            sequence: 10,
        },
        EventKind::ControlAssertion {
            period: april,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(100_000),
                at: BalancePoint::Opening,
            },
        },
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal,
        "March confirmed"
    );
    assert_eq!(
        ledger.status_for(account, date!(2026 - 04 - 15), Dimension::Cash),
        DimensionStatus::Provisional,
        "April is not confirmed by its opening balance: there is nothing to confirm in April yet"
    );
}

#[test]
fn a_period_without_assertions_stays_provisional() {
    // The absence of assertions is the absence of confirmation, not
    // confirmation that there are no problems.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let none_channel = TestChannel::new("manual/1", "none");
    let events = vec![deposit(&none_channel, owner, account, 100_000)];
    let ledger = ReconciliationLedger::build(&events).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), dimension),
            DimensionStatus::Provisional
        );
    }
}

#[test]
fn the_ledger_is_a_pure_function_of_the_journal() {
    // The same journal means the same status. Otherwise, reproducing the figure shown
    // to the owner is impossible, and §3.1 specifically requires that.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let first = ReconciliationLedger::build(&events).unwrap();
    let second = ReconciliationLedger::build(&events).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            first.status_for(account, date!(2026 - 03 - 15), dimension),
            second.status_for(account, date!(2026 - 03 - 15), dimension)
        );
    }
}

#[test]
fn a_discrepancy_covered_by_a_perimeter_exception_is_excepted_not_discrepant() {
    // §11: the system knows why the figures do not reconcile and does not send
    // the owner to fix something it does not support. But this does not
    // constitute confirmation: the measurement cannot rise above provisional.
    use iaam_core::perimeter::PerimeterExceptions;
    use iaam_core::reconciliation::check::{ClaimOutcome, ReconciliationException};

    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");

    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 999_999,
            debit: 100_000,
            credit: 0,
        },
    ));

    let bare = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        bare.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Discrepant,
        "without an exception, this is a regular discrepancy"
    );

    let mut exceptions = PerimeterExceptions::default();
    exceptions.add(
        account,
        Dimension::Cash,
        ReconciliationException::UnsupportedFinancingPresent,
    );
    let excused = ReconciliationLedger::build_with(&events, &exceptions).unwrap();

    assert_eq!(
        excused.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Provisional,
        "an exception removes the requirement to fix it but does not confirm the data"
    );
    let status = excused
        .statuses()
        .find(|status| status.account() == account)
        .expect("March status");
    assert!(
        status
            .outcomes()
            .iter()
            .any(|check| matches!(check.outcome, ClaimOutcome::Excepted { .. })),
        "the outcome must be marked as an exception, not a discrepancy"
    );
    assert!(
        !status
            .outcomes()
            .iter()
            .any(|check| matches!(check.outcome, ClaimOutcome::Discrepant(_))),
        "a discrepancy covered by an exception does not remain a discrepancy"
    );
}

#[test]
fn a_status_carries_the_grounds_that_produced_it() {
    // The owner asks not only whether it can be trusted, but also why.
    // A status without a basis is a figure without an explanation, and §10.3 introduces
    // bases specifically so that the level can be verified.
    use iaam_core::reconciliation::evidence::Ground;

    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");

    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    let status = ledger
        .statuses()
        .find(|status| status.account() == account)
        .expect("March status");

    assert_eq!(status.period(), march());
    let grounds: Vec<Ground> = status
        .evidence()
        .iter()
        .map(iaam_core::reconciliation::evidence::Evidence::ground)
        .collect();
    assert_eq!(
        grounds,
        vec![Ground::SeparateSectionsAgree],
        "the status must state the basis on which it was obtained"
    );
    assert_eq!(
        status.outcomes().len(),
        3,
        "all three verified assertions remain visible"
    );
}
