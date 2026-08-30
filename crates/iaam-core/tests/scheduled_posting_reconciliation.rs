//! Regression guards for scheduled payment reconciliation (§7.2, §15.3).
//!
//! Targeted tests of the matching rule, income-fact projection, and
//! ownership boundary were written by their authors next to the code. This file tests
//! the **integration point**: the entire event log passes through the projection, cash-flow rule,
//! matching rule, and report construction, while assertions concern only
//! what the owner sees — `MaterialIssue` entries in `data_quality`.
//! A unit test does not catch this: it calls the rule directly and stays silent
//! when reconciliation no longer reaches the rule.
//!
//! This is intentionally an integration-test file: it builds the log and reads the report
//! exclusively through the crate's public interface. A regression that
//! hides reconciliation behind a private helper will fail here.
//!
//! ## About face value
//!
//! Face value is no longer a property of a lot: it comes from the issue schedule.
//! The old helper patched it directly into the CBOR state snapshot — a technique
//! that for years tested the entire E3.4 path against data that production
//! code never sees. The helper was removed together with `Lot.principal` (T8).
//!
//! The five-year bond schedule and all other scenarios provide face value through
//! the normal path. The separate test `without_the_face_value_the_reconciliation_still_runs`
//! intentionally leaves `initial_principal` unknown: the past must still
//! be reconciled directly from the schedule.

use std::collections::BTreeMap;

use iaam_core::bond::offer::ScheduleCompleteness;
use iaam_core::bond::{AccrualPeriod, BondSchedule, DefaultFlags, PrincipalReturn};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, SettledDate, TradeDate};
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::{EventKind, IncomeKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterAssessment, PerimeterPolicy};
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::ReconciliationLedger;
use iaam_core::returns::{
    Computed, KnowledgeCoordinate, MaterialIssue, ReturnsReport, ReturnsRequest,
    UnverifiableReason, returns_report,
};
use iaam_core::rules::{LotRuleVersion, PostingKind, RuleRegistry};
use iaam_core::valuation::{
    PriceCandidate, PriceKind, PriceOrigin, QuotationBasis, SourceExecutability, Venue,
};
use rust_decimal::Decimal;
use time::macros::date;
use time::{Date, Duration};
use uuid::Uuid;

// Identities are specified numerically rather than with `new_random`: the log-order
// property compares the verdicts from two runs, and a random `EventId`
// would make any discrepancy unreproducible (§15.3).
const OWNER: OwnerId = OwnerId(Uuid::from_u128(1));
const ACCOUNT: AccountId = AccountId(Uuid::from_u128(2));
const INSTRUMENT: InstrumentId = InstrumentId(Uuid::from_u128(3));
const CUSTODY: CustodyId = CustodyId(Uuid::from_u128(4));
const OTHER_CUSTODY: CustodyId = CustodyId(Uuid::from_u128(5));
const SOURCE: SourceId = SourceId(Uuid::from_u128(6));

/// Number of securities in one purchase.
const PURCHASE_QUANTITY_TEXT: &str = "10";
/// Default report date.
const REPORT_DATE: Date = date!(2026 - 08 - 26);

fn dec(text: &str) -> Dec {
    Dec::new(Decimal::from_str_exact(text).expect("decimal constant"))
}

fn rubles(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn per_unit(text: &str) -> PerUnitAmount {
    PerUnitAmount::new(dec(text), CurrencyCode::Rub)
}

/// Event envelope. The identifier is derived from the event's log number, which also
/// determines the order, so two events with the same number are a test error,
/// not a reason to introduce randomness.
fn event(date: Date, number: u32, kind: EventKind, legs: Vec<Leg>) -> Event {
    Event {
        id: EventId(Uuid::from_u128(u128::from(number))),
        schema_version: SCHEMA_VERSION,
        owner: OWNER,
        account: ACCOUNT,
        kind,
        dates: EventDates::for_cash(CashPostedDate(date)),
        order: EffectiveOrder::new(date, number),
        legs,
        provenance: Provenance::new(
            SOURCE,
            RawHash::parse(&"a".repeat(64)).expect("hexadecimal hash"),
            ParserVersion("test/scheduled-posting/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn cash_in(date: Date, number: u32) -> Event {
    let amount = rubles(10_000_000);
    event(
        date,
        number,
        EventKind::CashIn { amount },
        vec![Leg::cash(ACCOUNT, amount)],
    )
}

/// Bond purchase with a specified trade date.
///
/// The trade date is required: the lot book stores it in `Lot.acquired`,
/// and the lower ownership bound is derived from it. A purchase without a trade date
/// makes reconciliation unprovable — that is a separate case, covered
/// by a core unit test.
fn purchase(custody: CustodyId, date: Date, number: u32) -> Event {
    let quantity = Quantity(dec(PURCHASE_QUANTITY_TEXT));
    let mut event = event(
        date,
        number,
        EventKind::Trade {
            side: TradeSide::Buy,
            instrument: INSTRUMENT,
            quantity,
            gross: rubles(1_000_000),
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(ACCOUNT, rubles(-1_000_000)),
            Leg::security(ACCOUNT, custody, INSTRUMENT, quantity),
        ],
    );
    // The test models the Finam source, which reports the settlement date;
    // here it matches the trade date because settlement is not
    // tested separately.
    event.dates.settled = Some(SettledDate(date));
    event.dates.trade = Some(TradeDate(date));
    event
}
/// Purchase from a source that does not report the settlement date.
///
/// The absence of `settled` intentionally leaves ownership unprovable:
/// the source does not entitle the system to guess when ownership was transferred.
fn purchase_without_settlement_date(custody: CustodyId, date: Date, number: u32) -> Event {
    let mut event = purchase(custody, date, number);
    event.dates.settled = None;
    event
}

/// Sale of the entire lot from the named depository.
fn sale(custody: CustodyId, date: Date, number: u32) -> Event {
    let quantity = Quantity(dec(PURCHASE_QUANTITY_TEXT));
    let mut event = event(
        date,
        number,
        EventKind::Trade {
            side: TradeSide::Sell,
            instrument: INSTRUMENT,
            quantity,
            gross: rubles(1_000_000),
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(ACCOUNT, rubles(1_000_000)),
            Leg::security(ACCOUNT, custody, INSTRUMENT, Quantity(dec("-10"))),
        ],
    );
    // The test models the Finam source, which reports the settlement date;
    // here it matches the trade date because settlement is not
    // tested separately.
    event.dates.settled = Some(SettledDate(date));
    event.dates.trade = Some(TradeDate(date));
    event
}

/// Coupon receipt: the date the funds are credited is the fact date.
fn coupon(date: Date, number: u32) -> Event {
    let amount = rubles(50_000);
    event(
        date,
        number,
        EventKind::Income {
            instrument: Some(INSTRUMENT),
            gross: amount,
            kind: Some(IncomeKind::Coupon),
        },
        vec![Leg::cash(ACCOUNT, amount)],
    )
}

/// Amortization payment: half the face value in cash, while the number of securities
/// remains unchanged (§6.5). The only source of a fact of kind `PrincipalReturn`
/// for a bond that has not yet been redeemed.
fn partial_redemption(date: Date, number: u32) -> Event {
    let compensation = rubles(500_000);
    event(
        date,
        number,
        EventKind::CorporateAction {
            action: CorporateAction::PartialRedemption {
                instrument: INSTRUMENT,
                custody: CUSTODY,
                quantity: Quantity(dec(PURCHASE_QUANTITY_TEXT)),
                principal_returned_per_unit: per_unit("500"),
                compensation,
                effective_date: date,
                record_date: None,
                grounds: None,
                basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
            },
        },
        vec![Leg::principal(ACCOUNT, INSTRUMENT, compensation)],
    )
}

/// Issue schedule: coupon dates and face-value repayment fractions.
///
/// Completeness, default flags, and currency roles are specified explicitly — without them the cash-flow
/// rule refuses to build a plan, `past` never appears at all, and the test
/// would be checking a construction failure rather than reconciliation. The period starts
/// with the previous payment: a contiguous chain is required to calculate accrued coupon interest, otherwise the failure
/// originates there.
fn schedule(coupon_dates: &[Date], returns: &[(Date, &str)]) -> BondSchedule {
    BondSchedule {
        // The test schedule models a source that reports the
        // record date; the record date matches the payment date.
        periods: coupon_dates
            .iter()
            .enumerate()
            .map(|(index, date)| AccrualPeriod {
                period_start: if index == 0 {
                    date.saturating_sub(Duration::days(180))
                } else {
                    coupon_dates[index - 1]
                },
                accrual_end: *date,
                payment_date: *date,
                record_date: Some(*date),
                coupon_per_unit: Some(per_unit("50")),
            })
            .collect(),
        principal_returns: returns
            .iter()
            .map(|(date, share)| PrincipalReturn {
                repayment_date: *date,
                share_percent: dec(share),
            })
            .collect(),
        initial_principal: None,
        offer_windows: Vec::new(),
        completeness: ScheduleCompleteness::Validated,
        default_flags: Some(DefaultFlags {
            declared: false,
            technical: false,
        }),
        currency_roles: Some(CurrencyRoles::uniform(CurrencyCode::Rub)),
    }
}
/// Schedule with face value supplied by the issue reference data.
fn schedule_with_face_value(coupon_dates: &[Date], returns: &[(Date, &str)]) -> BondSchedule {
    let mut schedule = schedule(coupon_dates, returns);
    schedule.initial_principal = Some(per_unit("1000"));
    schedule
}

/// Exchange price on the day before the report: an unpriced position would itself make the report
/// incomplete and mask reconciliation's contribution to the quality status.
fn price(report_date: Date) -> PriceCandidate {
    PriceCandidate {
        instrument: INSTRUMENT,
        price: dec("1000"),
        currency: CurrencyCode::Rub,
        basis: QuotationBasis::MoneyPerUnit,
        basis_evidence: "test:market".to_owned(),
        basis_evidence_contradicts: false,
        trade_date: report_date.saturating_sub(Duration::days(1)),
        observed_at: None,
        origin: PriceOrigin::Market {
            venue: Venue {
                board: "TQBR".to_owned(),
                session: 0,
            },
            kind: PriceKind::LegalClose,
        },
        executability: SourceExecutability::Executable,
    }
}

/// Inputs supplied to the report.
///
/// A struct rather than five positional arguments: swapping the log and schedule is easy,
/// but spotting that from a failing test — not so much.
struct Scenario<'a> {
    events: &'a [Event],
    schedule: &'a BondSchedule,
    report_date: Date,
}

fn build_report(scenario: &Scenario<'_>) -> ReturnsReport {
    let contour =
        ContourDefinition::new(ContourId(Uuid::from_u128(7)), ContourVersion(1), [ACCOUNT]);
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let state = project(scenario.events, &context)
        .expect("bond log projection")
        .state()
        .clone();
    let fx = iaam_core::valuation::FxTable::new(iaam_core::valuation::FxSource::OwnerSupplied);
    let ledger = ReconciliationLedger::default();
    let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
    let candidate = price(scenario.report_date);
    let schedules = BTreeMap::from([(INSTRUMENT, scenario.schedule.clone())]);
    returns_report(
        &state,
        &ReturnsRequest {
            contour: &contour,
            as_of: scenario.report_date,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &schedules,
            accrued_observations: &BTreeMap::new(),
        },
    )
}

fn missing_postings(report: &ReturnsReport) -> Vec<&MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| matches!(issue, MaterialIssue::ScheduledPostingNotReceived { .. }))
        .collect()
}

fn unverifiable_postings(report: &ReturnsReport) -> Vec<&MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| {
            matches!(
                issue,
                MaterialIssue::ScheduledPostingUnverifiable { .. }
                    | MaterialIssue::ScheduledPostingsUnverifiable { .. }
            )
        })
        .collect()
}

/// Reconciliation verdict: both issues it emits, in report order.
fn verdict(report: &ReturnsReport) -> Vec<MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| {
            matches!(
                issue,
                MaterialIssue::ScheduledPostingNotReceived { .. }
                    | MaterialIssue::ScheduledPostingUnverifiable { .. }
                    | MaterialIssue::ScheduledPostingsUnverifiable { .. }
            )
        })
        .cloned()
        .collect()
}

/// The cash flow was built, so reconciliation reached the plan.
///
/// Without this check, silence from reconciliation cannot be distinguished from a scenario
/// that never ran: a construction failure skips reconciliation entirely.
fn assert_flow_built(report: &ReturnsReport) {
    assert!(
        !report.bond_metrics.is_empty(),
        "bond position was not included in the report: there is nothing to check"
    );
    for position in &report.bond_metrics {
        assert!(
            matches!(
                position.scenarios[0].prospective.metrics,
                Computed::Value(_)
            ),
            "cash flow was not built: {:?}",
            position.scenarios[0].prospective.metrics
        );
    }
}

/// Coupon dates that have already passed by the report date: five years
/// of semiannual payments.
const PAST_COUPON_DATES: [Date; 10] = [
    date!(2021 - 09 - 15),
    date!(2022 - 03 - 15),
    date!(2022 - 09 - 15),
    date!(2023 - 03 - 15),
    date!(2023 - 09 - 15),
    date!(2024 - 03 - 15),
    date!(2024 - 09 - 15),
    date!(2025 - 03 - 15),
    date!(2025 - 09 - 15),
    date!(2026 - 03 - 15),
];

/// Five-year bond schedule: coupons and redemption without face value.
fn five_year_bond_schedule_without_face_value() -> BondSchedule {
    let mut coupon_dates = PAST_COUPON_DATES.to_vec();
    coupon_dates.push(date!(2026 - 09 - 15));
    coupon_dates.push(date!(2027 - 03 - 15));
    schedule(&coupon_dates, &[(date!(2027 - 03 - 15), "100")])
}

/// Five-year bond schedule with face value from the issue reference data.
fn five_year_bond_schedule() -> BondSchedule {
    let mut schedule = five_year_bond_schedule_without_face_value();
    schedule.initial_principal = Some(per_unit("1000"));
    schedule
}

/// Five-year history: a purchase and coupons received after delays
/// in the depository chain.
///
/// The offset of the actual date from the scheduled date spans the entire allowed range of
/// 1–7 days: actual payments do not arrive on the exact scheduled date, and the test must
/// pass on exactly this kind of journal, not on an imaginary ideal one.
fn five_year_journal(missing_date: Option<Date>) -> Vec<Event> {
    let mut events = vec![
        cash_in(date!(2021 - 07 - 25), 1),
        purchase(CUSTODY, date!(2021 - 08 - 01), 2),
    ];
    let mut number = 10;
    for (index, date) in PAST_COUPON_DATES.iter().enumerate() {
        if Some(*date) == missing_date {
            continue;
        }
        let offset = i64::try_from(index % 7).expect("coupon number") + 1;
        events.push(coupon(date.saturating_add(Duration::days(offset)), number));
        number += 1;
    }
    events
}

#[test]
fn five_years_of_coupons_received_late_but_received_raise_no_alarm() {
    // The epic's primary criterion: a healthy security produces no alerts. A payment delay
    // of 1–7 days is normal in the depository chain, not a defect, and if
    // a journal like this raises even one alert, reconciliation is useless: the owner
    // will stop reading warnings in the second week.
    let report = build_report(&Scenario {
        events: &five_year_journal(None),
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    assert!(
        verdict(&report).is_empty(),
        "verdict: {:?}",
        verdict(&report)
    );
}

#[test]
fn an_amortised_bond_closes_its_principal_returns_with_partial_redemptions() {
    // A principal repayment is confirmed by a corporate action, while a coupon is confirmed by
    // income. For coupon periods, the schedule provides the record date, but
    // `PrincipalReturn` does not yet carry such a field. Therefore both branches
    // must specifically refrain from an accusation: without an entitlement date, a
    // repayment cannot be declared missed, even if the repayment fact has arrived.
    let schedule = schedule_with_face_value(
        &[date!(2026 - 03 - 15), date!(2026 - 09 - 15)],
        &[(date!(2026 - 06 - 15), "50"), (date!(2026 - 09 - 15), "50")],
    );
    let healthy_events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
        coupon(date!(2026 - 03 - 17), 3),
        partial_redemption(date!(2026 - 06 - 17), 4),
    ];
    let assert_unverifiable = |report: &ReturnsReport| {
        assert_flow_built(report);
        assert!(
            missing_postings(report).is_empty(),
            "a repayment cannot be declared missed without an entitlement date: {:?}",
            verdict(report)
        );
        let issues = unverifiable_postings(report);
        assert_eq!(issues.len(), 1, "issues: {issues:?}");
        assert!(
            matches!(
                issues[0],
                MaterialIssue::ScheduledPostingUnverifiable {
                    date,
                    kind: PostingKind::PrincipalReturn,
                    reason: UnverifiableReason::EntitlementDateUnknown,
                    ..
                } if *date == date!(2026 - 06 - 15)
            ),
            "issue: {:?}",
            issues[0]
        );
    };
    let report = build_report(&Scenario {
        events: &healthy_events,
        schedule: &schedule,
        report_date: REPORT_DATE,
    });
    assert_unverifiable(&report);

    // The same journal without an amortization payment: until the model has an entitlement
    // date for `PrincipalReturn`, the absence of a fact cannot be called
    // a miss either—the result remains indeterminate rather than accusatory.
    let without_amortisation: Vec<Event> = healthy_events[..3].to_vec();
    let report = build_report(&Scenario {
        events: &without_amortisation,
        schedule: &schedule,
        report_date: REPORT_DATE,
    });
    assert_unverifiable(&report);
}

#[test]
fn a_single_gap_in_the_middle_of_the_history_is_named_once_and_exactly() {
    // A gap in the middle of the series is precisely why matching
    // was made one-to-one: greedy matching where «any fact satisfies
    // any payment» would mask the gap with neighboring coupons and remain silent.
    let missing_date = date!(2023 - 09 - 15);
    let report = build_report(&Scenario {
        events: &five_year_journal(Some(missing_date)),
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "issues: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                account,
                instrument,
                date,
                kind: PostingKind::Coupon,
            } if *account == ACCOUNT && *instrument == INSTRUMENT && *date == missing_date
        ),
        "issue: {:?}",
        issues[0]
    );
    assert!(unverifiable_postings(&report).is_empty());

    // The same check across the entire series: removing any of the ten coupons
    // must produce exactly one issue—its own. Without this pass,
    // reconciliation could check only two or three coupons out of ten and
    // silently skip the rest, while the first test in the file would still pass.
    for missing_date in PAST_COUPON_DATES {
        let report = build_report(&Scenario {
            events: &five_year_journal(Some(missing_date)),
            schedule: &five_year_bond_schedule(),
            report_date: REPORT_DATE,
        });
        let issues = missing_postings(&report);
        assert_eq!(issues.len(), 1, "coupon {missing_date}: {issues:?}");
        assert!(
            matches!(
                issues[0],
                MaterialIssue::ScheduledPostingNotReceived { date, .. }
                    if *date == missing_date
            ),
            "coupon {missing_date}: {:?}",
            issues[0]
        );
    }
}

#[test]
fn the_waiting_window_expires_exactly_twenty_one_days_after_the_scheduled_date() {
    // `is_due` is `date + 21 <= as_of`. Thus, on day twenty,
    // the grace period is still running and there is no alert, while on day twenty-one it has expired and
    // an alert is required. The boundary is checked at three points because
    // shifting it by one day in either direction means either a false alert on
    // a healthy security or silence on a missed payment.
    let scheduled_date = date!(2026 - 03 - 15);
    let schedule = schedule_with_face_value(
        &[scheduled_date, date!(2026 - 09 - 15)],
        &[(date!(2026 - 09 - 15), "100")],
    );
    let events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
    ];
    let as_of = |offset: i64| {
        build_report(&Scenario {
            events: &events,
            schedule: &schedule,
            report_date: scheduled_date.saturating_add(Duration::days(offset)),
        })
    };

    let still_pending = as_of(20);
    assert_flow_built(&still_pending);
    assert!(
        verdict(&still_pending).is_empty(),
        "the grace period is still running on day twenty: {:?}",
        verdict(&still_pending)
    );

    let expired = as_of(21);
    assert_flow_built(&expired);
    assert_eq!(
        missing_postings(&expired).len(),
        1,
        "issues: {:?}",
        missing_postings(&expired)
    );

    let long_expired = as_of(22);
    assert_flow_built(&long_expired);
    assert_eq!(
        missing_postings(&long_expired).len(),
        1,
        "issues: {:?}",
        missing_postings(&long_expired)
    );
}

/// Two purchases and a sale of the earlier lot from the same depository.
///
/// A single depository is used for the entire journal: otherwise the sale would remove the security
/// from a place where it was never deposited, and there would be three positions—the test would check
/// duplication rather than the ownership boundary.
fn journal_with_early_lot_sold(fact_dates: &[Date]) -> Vec<Event> {
    let mut events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
        purchase(CUSTODY, date!(2026 - 04 - 10), 3),
        sale(CUSTODY, date!(2026 - 07 - 10), 4),
    ];
    for (index, date) in fact_dates.iter().enumerate() {
        events.push(coupon(
            *date,
            10 + u32::try_from(index).expect("fact number"),
        ));
    }
    events
}

/// A security schedule with two past coupons and redemption in December.
fn two_coupon_schedule() -> BondSchedule {
    schedule_with_face_value(
        &[
            date!(2026 - 03 - 15),
            date!(2026 - 06 - 15),
            date!(2026 - 12 - 15),
        ],
        &[(date!(2026 - 12 - 15), "100")],
    )
}

#[test]
fn a_coupon_missed_while_the_early_lot_was_held_is_named_after_it_was_sold() {
    // The ownership boundary is the earliest acquisition date ever
    // observed for the pair, not the date of the oldest open lot. Otherwise,
    // selling the January lot would move the boundary to April and hide
    // the missed March payment: the owner would lose money exactly where
    // reconciliation is required to warn them.
    let report = build_report(&Scenario {
        events: &journal_with_early_lot_sold(&[date!(2026 - 06 - 16)]),
        schedule: &two_coupon_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "issues: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        ),
        "issue: {:?}",
        issues[0]
    );
}

#[test]
fn two_purchases_with_a_complete_history_raise_no_alarm() {
    // The flip side of the same boundary. Without this test, the boundary
    // could be «fixed» by declaring everything missed.
    let report = build_report(&Scenario {
        events: &journal_with_early_lot_sold(&[date!(2026 - 03 - 16), date!(2026 - 06 - 16)]),
        schedule: &two_coupon_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    assert!(
        verdict(&report).is_empty(),
        "verdict: {:?}",
        verdict(&report)
    );
}

#[test]
fn one_bond_in_two_custodies_reports_a_single_missing_coupon() {
    // Positions are traversed by custody location, while reconciliation uses the
    // (account, security) pair without it: otherwise the same payment would be
    // reported once per depository, and the owner would look for two missed payments
    // instead of one.
    let events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
        purchase(OTHER_CUSTODY, date!(2026 - 01 - 11), 3),
        coupon(date!(2026 - 03 - 16), 4),
    ];
    let report = build_report(&Scenario {
        events: &events,
        schedule: &two_coupon_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    assert_eq!(
        report.bond_metrics.len(),
        2,
        "there must be two positions, otherwise the test proves nothing"
    );
    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "issues: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 06 - 15)
        ),
        "issue: {:?}",
        issues[0]
    );
}

/// Permuting a journal with an explicitly specified seed.
///
/// Fisher—Yates shuffle using a linear congruential generator: the core is
/// deterministic, and using `rand` from the environment would make a property failure
/// irreproducible. The constants are the well-known multiplier and increment of the
/// LCG (Knuth); their quality is irrelevant here, only
/// reproducibility from run to run matters.
fn shuffled(events: &[Event], seed: u64) -> Vec<Event> {
    let mut order = events.to_vec();
    let mut state = seed;
    for index in (1..order.len()).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let choice = usize::try_from(state >> 33).expect("high-order seed bits") % (index + 1);
        order.swap(index, choice);
    }
    order
}

#[test]
fn the_verdict_does_not_depend_on_the_order_of_the_journal() {
    // §15.3. The matching rule sorts its inputs and is therefore
    // independent of order—this is verified by its own tests.
    // This tests the integration point: the projection must turn the journal
    // into an effective set ordered by `EffectiveOrder` before
    // reconciliation sees anything. The journal has an omission: the property
    // «always empty» would also hold for a broken reconciliation.
    //
    // The entire journal is permuted, and that is valid specifically here: no
    // event refers to another (`Relation::None`), while the sequence number in the
    // journal is unique to each one, so `EffectiveOrder` defines a total
    // order, and the effective set is independent of the permutation by
    // construction. A journal with corrections must not be permuted this way.
    let events = five_year_journal(Some(date!(2023 - 09 - 15)));
    let baseline_verdict = verdict(&build_report(&Scenario {
        events: &events,
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    }));
    assert_eq!(
        baseline_verdict.len(),
        1,
        "baseline verdict: {baseline_verdict:?}"
    );

    // The shuffle must actually permute something: a property tested on
    // the identity permutation tests nothing.
    let mut order_changed = false;
    for seed in 1..=32_u64 {
        let shuffled_events = shuffled(&events, seed);
        order_changed |= shuffled_events != events;
        let shuffled_verdict = verdict(&build_report(&Scenario {
            events: &shuffled_events,
            schedule: &five_year_bond_schedule(),
            report_date: REPORT_DATE,
        }));
        assert_eq!(
            shuffled_verdict, baseline_verdict,
            "permutation with seed {seed} changed the verdict"
        );
    }

    assert!(order_changed, "the shuffle did not reorder any journal");

    // Reverse order is not a random permutation, but the most likely
    // way to read a broker export backwards.
    let mut reversed_events = events.clone();
    reversed_events.reverse();
    assert_eq!(
        verdict(&build_report(&Scenario {
            events: &reversed_events,
            schedule: &five_year_bond_schedule(),
            report_date: REPORT_DATE,
        })),
        baseline_verdict,
        "reverse order changed the verdict"
    );
}

#[test]
fn projecting_the_same_journal_twice_gives_the_same_verdict() {
    // §15.3: projecting the same journal again must produce the same
    // state and the same report. The state fingerprint, the
    // fingerprint of the report inputs, and the verdict itself are all checked: reconciliation reads the state,
    // not the journal, and they may diverge independently.
    let events = five_year_journal(Some(date!(2024 - 03 - 15)));
    let scenario = Scenario {
        events: &events,
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    };

    let first_report = build_report(&scenario);
    let second_report = build_report(&scenario);

    assert_eq!(
        verdict(&first_report).len(),
        1,
        "verdict: {:?}",
        verdict(&first_report)
    );
    assert_eq!(verdict(&first_report), verdict(&second_report));
    assert_eq!(first_report.inputs_hash, second_report.inputs_hash);
}

#[test]
fn without_the_face_value_the_reconciliation_still_runs() {
    // The previous behavior was a defect: face value did not make it into the lots
    // (`iaam-d8b.15`), causing reconciliation to remain silent on all real-world data.
    // The historical record is now built directly from the schedule and must
    // report the missed coupon even when the face value is unknown.
    let events = five_year_journal(Some(date!(2023 - 09 - 15)));
    let report = build_report(&Scenario {
        events: &events,
        schedule: &five_year_bond_schedule_without_face_value(),
        report_date: REPORT_DATE,
    });

    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "issues: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                date,
                kind: PostingKind::Coupon,
                ..
            } if *date == date!(2023 - 09 - 15)
        ),
        "issue: {:?}",
        issues[0]
    );
    assert!(
        unverifiable_postings(&report).is_empty(),
        "known ownership must not become indeterminate: {:?}",
        verdict(&report)
    );
}

#[test]
fn a_source_without_settlement_dates_cannot_accuse_anyone() {
    // A source that does not report title transfer dates makes ownership
    // indeterminate. The system must admit that rather than guess: an accusation
    // requires proof; an admission of ignorance does not.
    let events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase_without_settlement_date(CUSTODY, date!(2026 - 01 - 10), 2),
    ];
    let schedule = schedule(
        &[date!(2026 - 03 - 15), date!(2026 - 12 - 15)],
        &[(date!(2026 - 12 - 15), "100")],
    );
    let report = build_report(&Scenario {
        events: &events,
        schedule: &schedule,
        report_date: REPORT_DATE,
    });

    let issues = unverifiable_postings(&report);
    assert_eq!(issues.len(), 1, "issues: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingUnverifiable {
                date,
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } if *date == date!(2026 - 03 - 15)
        ),
        "issue: {:?}",
        issues[0]
    );
    assert!(
        missing_postings(&report).is_empty(),
        "cannot report an omission without proven ownership: {:?}",
        verdict(&report)
    );
}
