//! Properties and metamorphic checks for zero reinvestment (§15.3, §15.6).
//!
//! The generators here do not narrow the scope to convenient divisors: the distribution
//! remainder and event order are checked for arbitrary valid
//! quantities and monetary amounts.

use std::collections::BTreeMap;

use iaam_core::bond::{AccrualPeriod, BondSchedule, DefaultFlags, PrincipalReturn};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CalcMoney, CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::lots::{Cohort, LotKey};
use iaam_core::projection::{ProjectionContext, advance, project};
use iaam_core::returns::zero_reinvestment::{
    ZeroReinvestmentMetrics, lifetime_cohort_metrics, zero_reinvestment_metrics,
};
use iaam_core::returns::{Computed, ReturnsRequest, returns_report};
use iaam_core::rules::lot_disposal::{FifoV1, Lot, LotDisposalRule};
use iaam_core::rules::{
    CashflowPlan, CashflowProjection, CashflowProjectionV1, ExpectedPosting, LotRuleVersion,
    PostingKind, RuleRegistry,
};
use iaam_core::valuation::{FxSource, FxTable};
use proptest::prelude::*;
use rust_decimal::Decimal;
use time::macros::date;
use time::{Date, Duration};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn dec(value: i64) -> Dec {
    Dec::new(Decimal::new(value, 2))
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn calc_rub(minor: i64) -> CalcMoney {
    CalcMoney::new(dec(minor), CurrencyCode::Rub)
}

fn posting(day: Date, minor: i64) -> ExpectedPosting {
    ExpectedPosting {
        date: day,
        amount: calc_rub(minor),
        kind: PostingKind::Coupon,
    }
}

fn plan_with_one_posting(day: Date, minor: i64) -> CashflowPlan {
    CashflowPlan {
        postings: vec![posting(day, minor)],
        terminal_date: day,
        past: Vec::new(),
    }
}

fn postings_wealth(plan: &CashflowPlan) -> Dec {
    plan.postings
        .iter()
        .map(|posting| posting.amount.value())
        .try_fold(Dec::zero(), |sum, amount| sum.checked_add(amount))
        .expect("sum of valid payouts does not overflow")
}

proptest! {
    /// Scope: any positive cohort quantities and any positive
    /// payout. `lifetime_cohort_metrics` distributes each posting, and
    /// the last cohort receives the exact remainder. Repeated invocation on the same
    /// input establishes that the remainder is deterministic.
    #[test]
    fn every_payment_is_conserved_by_cohort_allocation(
        quantities in prop::collection::vec(1_i64..=1_000, 1..8),
        payment in 1_i64..=1_000_000,
    ) {
        let acquired = date!(2024 - 01 - 01);
        let cohorts: Vec<Cohort> = quantities
            .iter()
            .enumerate()
            .map(|(index, quantity)| Cohort {
                acquired: TradeDate(acquired + Duration::days(index as i64)),
                quantity: qty(*quantity),
                cost_basis: rub(100_000),
                acquisition_basis: Some(rub(100_000)),
                accrued_interest_paid: Some(rub(0)),
                received_to_date: Some(rub(0)),
            })
            .collect();
        let plan = plan_with_one_posting(date!(2026 - 01 - 01), payment);

        let first = lifetime_cohort_metrics(&cohorts, &plan);
        let second = lifetime_cohort_metrics(&cohorts, &plan);
        prop_assert_eq!(&first, &second, "distribution remainder must be deterministic");

        let metrics = match first {
            Computed::Value(metrics) => metrics,
            Computed::NotComputable { reason } => {
                prop_assert!(false, "valid distribution failed: {}", reason.code());
                return Ok(());
            }
        };
        let allocated = metrics
            .iter()
            .map(|cohort| match &cohort.metrics {
                Computed::Value(ZeroReinvestmentMetrics { postings, .. }) => postings[0].amount.value(),
                Computed::NotComputable { reason } => panic!("payout share was not computed: {}", reason.code()),
            })
            .try_fold(Dec::zero(), |sum, amount| sum.checked_add(amount))
            .expect("sum of shares does not overflow");
        let difference = allocated
            .checked_sub(dec(payment))
            .expect("difference between share sums does not overflow");
        prop_assert!(
            difference.inner().abs() <= Decimal::new(1, 2),
            "difference {} exceeds one minor unit",
            difference.inner()
        );
    }

    /// Scope: one positive lot, any partial disposal (including zero),
    /// and either known or unknown payout history. The public
    /// `DisposalResult` does not contain the disposed received_to_date, so
    /// we calculate the disposed share using the same deterministic rounding rule and
    /// check the invariant «remainder = original − disposed».
    #[test]
    fn fifo_split_conserves_received_to_date(
        (quantity, sold, received) in (1_i64..=1_000).prop_flat_map(|quantity| {
            (Just(quantity), 0_i64..quantity, prop::option::of(0_i64..=1_000_000))
        }),
    ) {
        let instrument = InstrumentId::new_random();
        let lot = Lot {
            id: iaam_core::rules::lot_disposal::LotId::new_random(),
            instrument,
            acquired: Some(TradeDate(date!(2024 - 01 - 01))),
            quantity: qty(quantity),
            cost_basis: rub(1_000_000),
            acquisition_basis: Some(rub(1_000_000)),
            accrued_interest_paid: None,
            received_to_date: received.map(rub),
        };
        let result = FifoV1
            .apply(&iaam_core::rules::lot_disposal::DisposalInput {
                lots: vec![lot],
                quantity: qty(sold),
            })
            .expect("disposed quantity is within the lot");

        match received {
            None => prop_assert_eq!(result.remaining[0].received_to_date, None),
            Some(original) => {
                let scaled = (Decimal::from(original) * Decimal::from(sold)) / Decimal::from(quantity);
                let taken = scaled
                    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointNearestEven)
                    .mantissa() as i64;
                let remaining = result.remaining[0]
                    .received_to_date
                    .expect("known value must remain known")
                    .amount()
                    .raw();
                prop_assert_eq!(taken + remaining, original);
            }
        }
    }

    /// Scope: positive C0 and nonnegative terminal wealth.
    /// For fixed C0, HPR = W_T / C0 − 1, so increasing W_T
    /// cannot decrease the result.
    #[test]
    fn hpr_is_monotone_in_terminal_wealth(
        c0_minor in 1_i64..=100_000_000,
        lower_wealth_minor in 0_i64..=100_000_000,
        added_wealth_minor in 0_i64..=100_000_000,
    ) {
        let coordinate = date!(2025 - 01 - 01);
        let terminal = date!(2026 - 01 - 01);
        let lower = zero_reinvestment_metrics(
            vec![posting(terminal, lower_wealth_minor)],
            calc_rub(c0_minor),
            coordinate,
            terminal,
        );
        let upper = zero_reinvestment_metrics(
            vec![posting(terminal, lower_wealth_minor + added_wealth_minor)],
            calc_rub(c0_minor),
            coordinate,
            terminal,
        );
        let lower_hpr = match lower {
            Computed::Value(metrics) => match metrics.hpr {
                Computed::Value(value) => value,
                Computed::NotComputable { reason } => panic!("lower HPR was not computed: {}", reason.code()),
            },
            Computed::NotComputable { reason } => panic!("lower metric was not computed: {}", reason.code()),
        };
        let upper_hpr = match upper {
            Computed::Value(metrics) => match metrics.hpr {
                Computed::Value(value) => value,
                Computed::NotComputable { reason } => panic!("upper HPR was not computed: {}", reason.code()),
            },
            Computed::NotComputable { reason } => panic!("upper metric was not computed: {}", reason.code()),
        };
        prop_assert!(lower_hpr <= upper_hpr);
    }

    /// Scope: two valid ruble-denominated schedules, where the second adds
    /// a known coupon to the right and moves part of the principal repayment to a new date.
    /// The total principal is preserved, and the new nonnegative coupon does not reduce
    /// terminal wealth.
    #[test]
    fn extending_schedule_to_the_right_does_not_reduce_terminal_wealth(
        coupon in 1_i64..=1_000_000,
        extension_days in 1_i64..=365,
    ) {
        let as_of = date!(2025 - 01 - 01);
        let first_date = as_of + Duration::days(30);
        let second_date = first_date + Duration::days(extension_days);
        let principal = PerUnitAmount::new(Dec::new(Decimal::from(1_000)), CurrencyCode::Rub);
        let first_coupon = PerUnitAmount::new(dec(coupon), CurrencyCode::Rub);
        let second_coupon = PerUnitAmount::new(dec(coupon), CurrencyCode::Rub);
        let common = |periods, principal_returns| BondSchedule {
            periods,
            principal_returns,
            completeness: iaam_core::bond::ScheduleCompleteness::Validated,
            default_flags: Some(DefaultFlags { declared: false, technical: false }),
            currency_roles: Some(CurrencyRoles::uniform(CurrencyCode::Rub)),
            initial_principal: Some(principal),
            ..BondSchedule::default()
        };
        let original = common(
            vec![AccrualPeriod {
                period_start: as_of,
                accrual_end: first_date,
                payment_date: first_date,
                record_date: None,
                coupon_per_unit: Some(first_coupon),
            }],
            vec![PrincipalReturn {
                repayment_date: first_date,
                share_percent: Dec::new(Decimal::from(100)),
            }],
        );
        let extended = common(
            vec![
                AccrualPeriod {
                    period_start: as_of,
                    accrual_end: first_date,
                    payment_date: first_date,
                    record_date: None,
                    coupon_per_unit: Some(first_coupon),
                },
                AccrualPeriod {
                    period_start: first_date,
                    accrual_end: second_date,
                    payment_date: second_date,
                    record_date: None,
                    coupon_per_unit: Some(second_coupon),
                },
            ],
            vec![
                PrincipalReturn {
                    repayment_date: first_date,
                    share_percent: Dec::new(Decimal::from(50)),
                },
                PrincipalReturn {
                    repayment_date: second_date,
                    share_percent: Dec::new(Decimal::from(50)),
                },
            ],
        );
        let hold = iaam_core::bond::offer::OfferChoice::HoldToMaturity;
        let original_plan = CashflowProjectionV1
            .future_postings(&iaam_core::rules::CashflowInput {
                schedule: &original,
                quantity: qty(1),
                choice: &hold,
                as_of,
                report_currency: CurrencyCode::Rub,
            })
            .expect("original schedule is valid");
        let extended_plan = CashflowProjectionV1
            .future_postings(&iaam_core::rules::CashflowInput {
                schedule: &extended,
                quantity: qty(1),
                choice: &hold,
                as_of,
                report_currency: CurrencyCode::Rub,
            })
            .expect("extended schedule is valid");
        prop_assert!(postings_wealth(&extended_plan) >= postings_wealth(&original_plan));
    }
}

/// Who records what and where is represented by a triple that remains unchanged for all scenario events.
///
/// Use a struct rather than three consecutive arguments: `OwnerId`, `SourceId`, and
/// `AccountId` are distinct types, but in a seven-item argument list they are easy
/// to swap, silently changing the meaning of the test.
#[derive(Debug, Clone, Copy)]
struct Party {
    owner: OwnerId,
    source: SourceId,
    account: AccountId,
}

fn event(party: Party, day: Date, sequence: u32, kind: EventKind, legs: Vec<Leg>) -> Event {
    let Party {
        owner,
        source,
        account,
    } = party;
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind,
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs,
        provenance: Provenance::new(
            source,
            RawHash::parse(&"a".repeat(64)).expect("hash"),
            ParserVersion("prop-zero/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn buy_event(
    party: Party,
    instrument: InstrumentId,
    custody: CustodyId,
    day: Date,
    sequence: u32,
    units: i64,
) -> Event {
    let mut event = event(
        party,
        day,
        sequence,
        EventKind::Trade {
            side: TradeSide::Buy,
            instrument,
            quantity: qty(units),
            gross: rub(units * 1_000),
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(party.account, rub(-(units * 1_000))),
            Leg::security(party.account, custody, instrument, qty(units)),
        ],
    );
    event.dates = EventDates::for_trade(TradeDate(day), None);
    event
}

fn income_event(
    party: Party,
    instrument: InstrumentId,
    day: Date,
    sequence: u32,
    amount: i64,
) -> Event {
    event(
        party,
        day,
        sequence,
        EventKind::Income {
            instrument: Some(instrument),
            gross: rub(amount),
            kind: Some(iaam_core::event::kind::IncomeKind::Coupon),
        },
        vec![Leg::cash(party.account, rub(amount))],
    )
}

proptest! {
    /// Metamorphic property: permuting incoming historical payouts does not change
    /// any cohort. The order of the two purchases is fixed, while the payout list
    /// is cyclically rotated before reimporting.
    #[test]
    fn permuting_import_order_of_past_payments_preserves_cohorts(
        first_quantity in 1_i64..=1_000,
        second_quantity in 1_i64..=1_000,
        payments in prop::collection::vec(1_i64..=1_000_000, 1..8),
        rotation in 0_usize..8,
    ) {
        let owner = OwnerId::new_random();
        let source = SourceId::new_random();
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let party = Party { owner, source, account };
        let first_buy = buy_event(
            party, instrument, CustodyId::new_random(),
            date!(2024 - 01 - 01), 1, first_quantity,
        );
        let second_buy = buy_event(
            party, instrument, CustodyId::new_random(),
            date!(2024 - 02 - 01), 2, second_quantity,
        );
        let income_events: Vec<Event> = payments
            .iter()
            .enumerate()
            .map(|(index, amount)| income_event(
                party, instrument,
                date!(2024 - 03 - 01) + Duration::days(index as i64),
                (index + 3) as u32,
                *amount,
            ))
            .collect();
        let mut rotated = income_events.clone();
        let rotation = rotation % rotated.len();
        rotated.rotate_left(rotation);

        let mut first_book = iaam_core::projection::lots::LotBook::new(LotRuleVersion(1));
        first_book.apply(&first_buy, &RuleRegistry::with_defaults()).unwrap();
        first_book.apply(&second_buy, &RuleRegistry::with_defaults()).unwrap();
        for income in &income_events {
            first_book.apply(income, &RuleRegistry::with_defaults()).unwrap();
        }
        let mut second_book = iaam_core::projection::lots::LotBook::new(LotRuleVersion(1));
        second_book.apply(&first_buy, &RuleRegistry::with_defaults()).unwrap();
        second_book.apply(&second_buy, &RuleRegistry::with_defaults()).unwrap();
        for income in &rotated {
            second_book.apply(income, &RuleRegistry::with_defaults()).unwrap();
        }
        let key = LotKey { account, instrument };
        prop_assert_eq!(
            first_book.entry(&key).unwrap().cohorts().unwrap(),
            second_book.entry(&key).unwrap().cohorts().unwrap(),
        );
    }
}

/// Metamorphic property: resynchronizing an unchanged journal via
/// `advance` does not change the reported values at the same knowledge coordinate.
#[test]
fn resyncing_the_same_issue_keeps_every_report_number() {
    let account = AccountId::new_random();
    let owner = OwnerId::new_random();
    let source = SourceId::new_random();
    let events = vec![event(
        Party {
            owner,
            source,
            account,
        },
        date!(2025 - 01 - 01),
        1,
        EventKind::CashIn {
            amount: rub(100_000),
        },
        vec![Leg::cash(account, rub(100_000))],
    )];
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let initial = project(&events, &context).expect("initial synchronization succeeds");
    let resynced = advance(initial.snapshot(), &events, &context)
        .expect("resynchronization of the unchanged journal succeeds");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let schedules = BTreeMap::new();
    let accrued = BTreeMap::new();
    let request = ReturnsRequest {
        contour: &contour,
        as_of: date!(2025 - 12 - 31),
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        ledger: &ledger,
        perimeter: &perimeter,
        market_prices: &[],
        bond_schedules: &schedules,
        accrued_observations: &accrued,
    };
    assert_eq!(
        returns_report(initial.state(), &request),
        returns_report(resynced.state(), &request),
        "an unchanged issue must not change any report figures",
    );
}
