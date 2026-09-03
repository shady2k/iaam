//! Account completeness status over an interval, by dimension (§10.3).
use std::collections::BTreeSet;

use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
use iaam_core::event::{Event, EventValidationError, Relation};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::reconciliation::check::ClaimOutcome;
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

#[derive(Clone, Copy)]
struct AssertionScope {
    owner: OwnerId,
    account: AccountId,
    period: AssertionPeriod,
}

fn channel_with_document(channel: &TestChannel, document: &str) -> TestChannel {
    TestChannel {
        source: channel.source,
        parser: channel.parser.clone(),
        document: support::document_hash(document),
    }
}

fn channel_with_parser(channel: &TestChannel, parser: &str) -> TestChannel {
    TestChannel {
        source: channel.source,
        parser: iaam_core::event::provenance::ParserVersion(parser.to_owned()),
        document: channel.document.clone(),
    }
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

fn position_section(
    channel: &TestChannel,
    owner: OwnerId,
    account: AccountId,
    period: AssertionPeriod,
    instrument: InstrumentId,
    custody: CustodyId,
) -> Event {
    event_on(
        channel,
        Posting {
            owner,
            account,
            day: period.to,
            sequence: 20,
        },
        EventKind::ControlAssertion {
            period,
            claim: ControlClaim::PositionQuantity {
                instrument,
                custody,
                quantity: Quantity::zero(),
                at: BalancePoint::Closing,
            },
        },
        vec![],
    )
}

fn full_sections_with_position(
    channel: &TestChannel,
    scope: AssertionScope,
    sections: Sections,
    instrument: InstrumentId,
    custody: CustodyId,
) -> Vec<Event> {
    let mut events = full_sections(channel, scope.owner, scope.account, scope.period, sections);
    events.push(position_section(
        channel,
        scope.owner,
        scope.account,
        scope.period,
        instrument,
        custody,
    ));
    events
}

fn coverage_gap(
    channel: &TestChannel,
    scope: AssertionScope,
    dimensions: impl IntoIterator<Item = Dimension>,
    refused: u32,
    legs: Vec<Leg>,
) -> Event {
    let dimensions: BTreeSet<Dimension> = dimensions.into_iter().collect();
    event_on(
        channel,
        Posting {
            owner: scope.owner,
            account: scope.account,
            day: scope.period.to,
            sequence: 30,
        },
        EventKind::ImportCoverageGap {
            period: scope.period,
            dimensions: dimensions.clone(),
            refused,
            rows: (0..refused)
                .map(|index| RefusedRow {
                    key: SourceRowKey {
                        source: channel.source,
                        row: RowName::Given(format!("row-{index}")),
                    },
                    dimensions: dimensions.clone(),
                })
                .collect(),
        },
        legs,
    )
}

fn seeded_journal() -> (
    OwnerId,
    AccountId,
    InstrumentId,
    CustodyId,
    TestChannel,
    Vec<Event>,
) {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let first_channel = TestChannel::new("tinkoff-api/1", "first");
    let second_channel = TestChannel::new("tinkoff-xlsx/1", "second");
    let mut events = vec![deposit(&first_channel, owner, account, 100_000)];
    for channel in [&first_channel, &second_channel] {
        events.extend(full_sections_with_position(
            channel,
            AssertionScope {
                owner,
                account,
                period: march(),
            },
            Sections {
                opening: 0,
                closing: 100_000,
                debit: 100_000,
                credit: 0,
            },
            instrument,
            custody,
        ));
    }
    (owner, account, instrument, custody, first_channel, events)
}

#[test]
fn a_matching_coverage_gap_withholds_cash_independent_confirmation() {
    let (owner, account, _instrument, _custody, first_channel, mut events) = seeded_journal();
    events.push(coverage_gap(
        &channel_with_document(&first_channel, "gap"),
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_ne!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn a_coverage_gap_does_not_withhold_a_dimension_it_does_not_name() {
    let (owner, account, _instrument, _custody, first_channel, mut events) = seeded_journal();
    events.push(coverage_gap(
        &channel_with_document(&first_channel, "gap"),
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn a_gap_from_another_source_or_parser_leaves_the_group_intact() {
    let (owner, account, _instrument, _custody, first_channel, mut events) = seeded_journal();
    let different_source = TestChannel::new("tinkoff-api/1", "different-source");
    let different_parser = channel_with_parser(&first_channel, "other-parser/1");
    events.push(coverage_gap(
        &different_source,
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    ));
    events.push(coverage_gap(
        &different_parser,
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn a_later_group_without_a_gap_can_restore_independent_confirmation() {
    let (owner, account, instrument, custody, first_channel, mut events) = seeded_journal();
    let later_channel = TestChannel::new("later-parser/1", "later");
    events.extend(full_sections_with_position(
        &later_channel,
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
        instrument,
        custody,
    ));
    events.push(coverage_gap(
        &channel_with_document(&first_channel, "gap"),
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn a_gap_does_not_change_matched_or_discrepant_claim_outcomes() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-api/1", "claims");
    let mut events = vec![deposit(&channel, owner, account, 100_000)];
    events.extend(full_sections(
        &channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 99_999,
            debit: 100_000,
            credit: 0,
        },
    ));
    events.push(coverage_gap(
        &channel_with_document(&channel, "gap"),
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    let status = ledger
        .statuses()
        .next()
        .expect("the assertion group has a status");
    assert!(
        status
            .outcomes()
            .iter()
            .any(|check| matches!(check.outcome, ClaimOutcome::Matched)),
        "the matched outcome remains matched"
    );
    assert!(
        status
            .outcomes()
            .iter()
            .any(|check| matches!(check.outcome, ClaimOutcome::Discrepant(_))),
        "the discrepant outcome remains discrepant"
    );
}

#[test]
fn a_coverage_gap_requires_dimensions_and_no_legs() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-api/1", "invalid-gap");

    let empty = coverage_gap(
        &channel,
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [],
        1,
        vec![],
    );
    assert!(matches!(
        empty.validate_structure(),
        Err(EventValidationError::EmptySet {
            kind: "import_coverage_gap",
            field: "dimensions",
        })
    ));

    let with_leg = coverage_gap(
        &channel,
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![Leg::cash(account, rub(1))],
    );
    assert!(matches!(
        with_leg.validate_structure(),
        Err(EventValidationError::LegCount {
            kind: "import_coverage_gap",
            expected: "no legs",
            found: 1
        })
    ));

    let inverted = coverage_gap(
        &channel,
        AssertionScope {
            owner,
            account,
            period: AssertionPeriod {
                from: date!(2026 - 03 - 31),
                to: date!(2026 - 03 - 01),
            },
        },
        [Dimension::Cash],
        1,
        vec![],
    );
    assert!(matches!(
        inverted.validate_structure(),
        Err(EventValidationError::NonPositive {
            kind: "import_coverage_gap",
            field: "period",
            ..
        })
    ));
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

/// The same event, retracted.
///
/// A reversal carries the kind and the legs of what it withdraws (§4.8), which
/// is why a reversed control assertion is not merely still present in the raw
/// journal but present twice.
fn reversal_of(event: &Event) -> Event {
    let mut reversal = event.clone();
    reversal.id = EventId::new_random();
    reversal.relation = Relation::Reversal { target: event.id };
    reversal
}

#[test]
fn a_reversed_control_assertion_no_longer_confirms() {
    let (_owner, account, _instrument, _custody, _channel, mut events) = seeded_journal();
    assert_eq!(
        ReconciliationLedger::build(&events).unwrap().status_for(
            account,
            date!(2026 - 03 - 15),
            Dimension::Cash
        ),
        DimensionStatus::AcceptedIndependent,
        "the unretracted journal confirms; without this the test proves nothing"
    );

    let reversals: Vec<Event> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ControlAssertion { .. }))
        .map(reversal_of)
        .collect();
    events.extend(reversals);

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Provisional,
        "a retracted assertion must withdraw the confirmation it produced"
    );
}

#[test]
fn a_reversed_coverage_gap_stops_withholding_confirmation() {
    let (owner, account, _instrument, _custody, first_channel, mut events) = seeded_journal();
    let gap = coverage_gap(
        &channel_with_document(&first_channel, "gap"),
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    );
    events.push(reversal_of(&gap));
    events.push(gap);

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent,
        "a retracted gap withholds nothing: it is not part of the journal that counts"
    );
}

/// A gap that correlated with a group must reach the status it tainted, and carry
/// the rows it refused: a reader holding one status must not have to re-correlate
/// the journal to say what was missed. The same fixture is asserted clean first,
/// or "no taints" would prove only that the fixture built nothing.
#[test]
fn a_tainted_status_carries_the_gaps_rows_and_an_untainted_status_carries_none() {
    let (owner, account, _instrument, _custody, first_channel, mut events) = seeded_journal();

    for status in ReconciliationLedger::build(&events).unwrap().statuses() {
        assert!(
            status.taints().is_empty(),
            "no gap in the journal, so no status is tainted"
        );
    }

    events.push(coverage_gap(
        &channel_with_document(&first_channel, "gap"),
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        2,
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    let status = ledger
        .statuses()
        .find(|status| status.account() == account && status.period() == march())
        .expect("status for the asserted interval");
    assert_eq!(status.taints().len(), 1, "{:?}", status.taints());
    let taint = &status.taints()[0];
    assert_eq!(taint.dimensions, BTreeSet::from([Dimension::Cash]));
    assert_eq!(taint.refused, 2);
    assert_eq!(
        taint
            .rows
            .iter()
            .map(|row| row.key.row.clone())
            .collect::<Vec<_>>(),
        vec![
            RowName::Given("row-0".to_owned()),
            RowName::Given("row-1".to_owned()),
        ]
    );
}

/// The sync path writes the gap and can then return before recording any
/// assertion, so a gap exists that no group correlates with and no status
/// mentions. The ledger's own list is the only place it can be seen.
#[test]
fn ledger_gaps_hold_a_gap_that_correlated_with_no_group() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-api/1", "gap-only");

    let ledger = ReconciliationLedger::build(&[coverage_gap(
        &channel,
        AssertionScope {
            owner,
            account,
            period: march(),
        },
        [Dimension::Cash],
        1,
        vec![],
    )])
    .unwrap();

    assert_eq!(
        ledger.statuses().count(),
        0,
        "a coverage gap forms no assertion group"
    );
    assert_eq!(ledger.gaps().len(), 1);
    assert_eq!(ledger.gaps()[0].account, account);
    assert_eq!(
        ledger.gaps()[0].dimensions,
        BTreeSet::from([Dimension::Cash])
    );
    assert_eq!(ledger.gaps()[0].refused, 1);
    assert_eq!(ledger.gaps()[0].rows.len(), 1);
}

#[test]
fn an_anchor_over_an_unanchored_history_leaves_the_dimension_provisional() {
    // The owner's case in `iaam-d7hn`, end to end. The journal begins in
    // January with an ordinary inflow, so nothing states what the account held
    // before it. In April he records a statement whose control section states
    // an opening balance he has confirmed against two independent sources.
    //
    // The observed opening is «zero plus everything since January», which is
    // movement and not a balance. `Discrepant` would say the figure he proved
    // is wrong; the truth is that the system has no baseline to hold it against,
    // and `Provisional` — «not checked yet» — is what that means. The registry
    // keeps discrepancies as an absorbing state precisely so that a real one is
    // never softened, and this test guards the other side of that line.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("statement/1", "april");
    let april = AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30)).unwrap();

    let mut events = vec![event_on(
        &channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 01 - 15),
            sequence: 1,
        },
        EventKind::CashIn {
            amount: rub(100_000),
        },
        vec![Leg::cash(account, rub(100_000))],
    )];
    events.extend(full_sections(
        &channel,
        owner,
        account,
        april,
        Sections {
            opening: 500_000,
            closing: 500_000,
            debit: 0,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 04 - 15), Dimension::Cash),
        DimensionStatus::Provisional,
        "an invented baseline must not be published as the owner's error"
    );

    let outcomes: Vec<&ClaimOutcome> = ledger
        .statuses()
        .flat_map(|status| status.outcomes())
        .map(|check| &check.outcome)
        .collect();
    assert!(
        outcomes
            .iter()
            .all(|outcome| !matches!(outcome, ClaimOutcome::Discrepant(_))),
        "no claim here disagrees with anything: {outcomes:?}"
    );
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            ClaimOutcome::NotComparable {
                reason: iaam_core::reconciliation::check::NotComparable::OpeningNotAsserted
            }
        )),
        "and the reason is named rather than left to be guessed: {outcomes:?}"
    );
}
