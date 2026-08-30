//! Report boundaries: the report date is inclusive, and calculations must not use a snapshot
//! assembled for a different date.
//!
//! Both checks concern strict date comparisons. An off-by-one-day error here
//! does not look like an error: it produces a number, just not the right one.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{Projection, ProjectionContext, project};
use iaam_core::returns::xirr::{flow_series, terminal_value};
use iaam_core::returns::{NotComputable, ReturnsRequest};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn deposit(owner: OwnerId, account: AccountId, day: Date, sequence: u32, minor: i64) -> Event {
    let amount = rub(minor);
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::CashIn { amount },
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs: vec![Leg::cash(account, amount)],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"8".repeat(64)).expect("hash"),
            ParserVersion("boundary/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

struct Fixture {
    contour: ContourDefinition,
    projection: Projection,
}

fn project_days(days: &[(Date, i64)]) -> Fixture {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let events: Vec<Event> = days
        .iter()
        .enumerate()
        .map(|(i, (day, minor))| {
            deposit(
                owner,
                account,
                *day,
                u32::try_from(i).unwrap_or(u32::MAX) + 1,
                *minor,
            )
        })
        .collect();
    let projection = project(
        &events,
        &ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        },
    )
    .expect("projection succeeds");
    Fixture {
        contour,
        projection,
    }
}

#[test]
fn a_flow_on_the_report_date_is_included() {
    // The report date is inclusive. A strict «before» comparison would exclude a transaction
    // from the same day, and an «as of today» report would not include today's
    // deposit.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(date!(2025 - 06 - 01), 10_000_000), (as_of, 5_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Reconciliation and the perimeter are not involved in this test: it checks the calculation,
    // not data confirmation. An empty registry and assessment mean
    // «nothing confirmed», which is neutral for the calculation.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &fixture.contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
        market_prices: &[],
        bond_schedules: &std::collections::BTreeMap::new(),
        accrued_observations: &std::collections::BTreeMap::new(),
    };

    let series = flow_series(fixture.projection.state(), &request).expect("flow series");
    assert_eq!(
        series.flows.len(),
        2,
        "flow on the report date must be included"
    );
    assert_eq!(series.contributed, Dec::new(Decimal::from(150_000)));
}

#[test]
fn a_slice_containing_events_after_the_report_date_is_refused() {
    // The wrapper assembles the snapshot for the date. An event after the report date means
    // that the snapshot was assembled incorrectly, and calculating from it would produce a report
    // for a date that did not yet exist on that date.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(as_of, 10_000_000), (date!(2026 - 02 - 01), 1_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Reconciliation and the perimeter are not involved in this test: it checks the calculation,
    // not data confirmation. An empty registry and assessment mean
    // «nothing confirmed», which is neutral for the calculation.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &fixture.contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
        market_prices: &[],
        bond_schedules: &std::collections::BTreeMap::new(),
        accrued_observations: &std::collections::BTreeMap::new(),
    };

    assert!(matches!(
        flow_series(fixture.projection.state(), &request),
        Err(NotComputable::StateNewerThanReport { .. })
    ));
    assert!(matches!(
        terminal_value(fixture.projection.state(), &request),
        Err(NotComputable::StateNewerThanReport { .. })
    ));
}

#[test]
fn a_slice_ending_exactly_on_the_report_date_is_accepted() {
    // One-unit boundary: when the last event falls exactly on the report date,
    // this is a valid snapshot, not one assembled for the future.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(as_of, 10_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Reconciliation and the perimeter are not involved in this test: it checks the calculation,
    // not data confirmation. An empty registry and assessment mean
    // «nothing confirmed», which is neutral for the calculation.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &fixture.contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
        market_prices: &[],
        bond_schedules: &std::collections::BTreeMap::new(),
        accrued_observations: &std::collections::BTreeMap::new(),
    };
    assert!(flow_series(fixture.projection.state(), &request).is_ok());
    assert!(terminal_value(fixture.projection.state(), &request).is_ok());
}
