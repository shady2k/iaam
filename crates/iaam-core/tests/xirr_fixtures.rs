//! Cross-checking the solver against an independent reference implementation (§15.4).

use std::collections::BTreeMap;

use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::numeric::xirr::{DayCount, SolverFlow, solve};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use time::macros::format_description;

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    flows: Vec<Flow>,
    expected_rate: String,
}

#[derive(Debug, Deserialize)]
struct Flow {
    date: String,
    amount: String,
}

fn parse_date(text: &str) -> Date {
    Date::parse(text, format_description!("[year]-[month]-[day]")).expect("fixture date")
}

#[test]
fn solver_matches_independent_decimal_oracle() {
    let raw = include_str!("../../../tests/fixtures/xirr_cases.json");
    let fixture: Fixture = serde_json::from_str(raw).expect("parse fixture");
    assert!(!fixture.cases.is_empty(), "an empty fixture tests nothing");

    let mut worst = BTreeMap::new();
    for case in &fixture.cases {
        let first = parse_date(&case.flows[0].date);
        let flows: Vec<SolverFlow> = case
            .flows
            .iter()
            .map(|f| SolverFlow {
                day_offset: (parse_date(&f.date) - first).whole_days(),
                amount: Dec::new(f.amount.parse::<Decimal>().expect("fixture amount")),
            })
            .collect();
        let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365)
            .unwrap_or_else(|e| panic!("{}: solver failed: {e}", case.name));
        let expected: f64 = case.expected_rate.parse().expect("fixture rate");
        let delta = (outcome.rate().value() - expected).abs();
        worst.insert(case.name.clone(), delta);
        assert!(
            delta < 1e-7,
            "{}: rate {} versus reference {} (difference {delta})",
            case.name,
            outcome.rate().value(),
            expected
        );
    }
    println!("{worst:#?}");
}
