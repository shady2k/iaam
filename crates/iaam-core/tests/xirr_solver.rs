//! Rate solver: refusals, root uniqueness, scale invariance.
//!
//! The tests were deliberately moved from `src/numeric/xirr.rs` into a separate file:
//! the architecture guard limits the size of approximate-mode files
//! to prevent a shadow calculation layer from taking root in them, while test code,
//! as it grew, would consume that limit and force it to be raised.

use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::numeric::xirr::{DayCount, SolverFlow, SolverRefusal, solve};
use rust_decimal::Decimal;

fn flow(day_offset: i64, amount: i64) -> SolverFlow {
    SolverFlow {
        day_offset,
        amount: Dec::new(Decimal::from(amount)),
    }
}

#[test]
fn a_single_year_of_ten_percent_is_ten_percent() {
    // Invested 1000, received 1100 after 365 days. The rate is known
    // from the problem statement, not inferred from the program's output (§15.5).
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!((outcome.rate().value() - 0.1).abs() < 1e-9);
    assert_eq!(outcome.day_count(), DayCount::Act365);
    // The error is half the width of the bracketing interval,
    // which is a proven bound, not the difference between approximations.
    assert!(outcome.rate().error_bound() <= SolverPolicy::returns_default().rate_tolerance);
}

#[test]
fn the_rate_does_not_depend_on_the_scale_of_the_flows() {
    // Scale invariance (§15.3): an absolute residual tolerance
    // would violate it because it would depend on the magnitude of the amounts.
    let small = [flow(0, -1_000), flow(365, 1_100)];
    let large = [flow(0, -1_000_000_000), flow(365, 1_100_000_000)];
    let policy = SolverPolicy::returns_default();
    let left = solve(&small, policy, DayCount::Act365).unwrap();
    let right = solve(&large, policy, DayCount::Act365).unwrap();
    assert!((left.rate().value() - right.rate().value()).abs() < 1e-9);
}

#[test]
fn flows_of_one_sign_have_no_rate() {
    let flows = [flow(0, -1_000), flow(365, -1_100)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::NoSignChange)
    );
}

#[test]
fn fewer_than_two_flows_have_no_rate() {
    let flows = [flow(0, -1_000)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::TooFewFlows)
    );
}

#[test]
fn all_zero_flows_have_no_rate() {
    let flows = [flow(0, 0), flow(365, 0)];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::AllZero)
    );
}

#[test]
fn two_sign_changes_are_refused_even_when_the_grid_finds_one_bracket() {
    // A classic sign-alternating series. The grid can find one
    // sign-change interval and «prove» uniqueness—but it
    // misses roots of even multiplicity and pairs of roots within a single step.
    // Refusal is mandatory even when the number looks plausible.
    let flows = [flow(0, -1_000), flow(365, 2_500), flow(730, -1_540)];
    let refusal = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap_err();
    assert!(
        matches!(
            refusal,
            SolverRefusal::MultipleRoots { .. } | SolverRefusal::UniquenessNotProven { .. }
        ),
        "got {refusal:?}"
    );
}

#[test]
fn a_coupon_series_with_one_sign_change_is_solved() {
    // Coupons between investment and redemption do not change sign: there is
    // one sign change and one root, so there must be no refusal.
    let flows = [
        flow(0, -98_000),
        flow(182, 4_500),
        flow(365, 4_500),
        flow(547, 4_500),
        flow(731, 104_500),
    ];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(outcome.rate().value() > 0.0);
}

#[test]
fn an_inverted_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 1.0,
        bracket_high: -1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_bracket_reaching_minus_one_hundred_percent_is_refused() {
    // At a rate of −100 %, the base of the power is zero: NPV is undefined.
    let policy = SolverPolicy {
        bracket_low: -1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_root_outside_the_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 0.0,
        bracket_high: 0.01,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::RootNotBracketed)
    );
}

#[test]
fn every_refusal_has_a_machine_readable_code() {
    assert_eq!(SolverRefusal::TooFewFlows.code(), "too_few_flows");
    assert_eq!(SolverRefusal::NoSignChange.code(), "no_sign_change");
    assert_eq!(SolverRefusal::RootNotBracketed.code(), "root_not_bracketed");
    assert_eq!(
        SolverRefusal::MultipleRoots { count: 2 }.code(),
        "multiple_roots"
    );
    assert_eq!(
        SolverRefusal::UniquenessNotProven { sign_changes: 3 }.code(),
        "uniqueness_not_proven"
    );
    assert_eq!(
        SolverRefusal::NotConverged { iterations: 1 }.code(),
        "not_converged"
    );
    assert_eq!(SolverRefusal::NotRepresentable.code(), "not_representable");
    assert_eq!(SolverRefusal::BadBracket.code(), "bad_bracket");
    assert_eq!(SolverRefusal::AllZero.code(), "all_zero");
}

#[test]
fn the_day_count_has_a_stable_code() {
    // The code is included in the report and snapshot: without it, the rate is not reproducible.
    assert_eq!(DayCount::Act365.code(), "act/365");
}

#[test]
fn the_solver_converges_superlinearly_not_by_halving() {
    // The Illinois method must converge appreciably faster than bisection: on an
    // interval about 0.1 wide, pure bisection to a tolerance of 1e-10
    // takes about thirty steps. Checking the iteration count is
    // the only way to detect that the Illinois technique is broken: the answer
    // remains correct but takes twice as long to obtain.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(
        outcome.rate().iterations() <= 20,
        "iterations {}: method degenerates into bisection",
        outcome.rate().iterations()
    );
}

#[test]
fn a_degenerate_bracket_is_refused() {
    let policy = SolverPolicy {
        bracket_low: 0.5,
        bracket_high: 0.5,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    assert_eq!(
        solve(&flows, policy, DayCount::Act365),
        Err(SolverRefusal::BadBracket)
    );
}

#[test]
fn a_non_numeric_bracket_is_refused() {
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    for policy in [
        SolverPolicy {
            bracket_low: f64::NAN,
            ..SolverPolicy::returns_default()
        },
        SolverPolicy {
            bracket_high: f64::INFINITY,
            ..SolverPolicy::returns_default()
        },
    ] {
        assert_eq!(
            solve(&flows, policy, DayCount::Act365),
            Err(SolverRefusal::BadBracket)
        );
    }
}

#[test]
fn any_bracket_reaching_minus_one_hundred_percent_is_refused() {
    // A rate of −100 % makes the base of the power zero, while a lower rate makes it
    // negative. The refusal must come from the lower bound regardless
    // of the upper bound: both when the range crosses −1 and when it lies entirely below it.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    for (low, high) in [(-1.0, 100.0), (-2.0, -1.0), (-3.0, -2.0)] {
        let policy = SolverPolicy {
            bracket_low: low,
            bracket_high: high,
            ..SolverPolicy::returns_default()
        };
        assert_eq!(
            solve(&flows, policy, DayCount::Act365),
            Err(SolverRefusal::BadBracket),
            "range [{low}, {high}]"
        );
    }
}

#[test]
fn the_scan_step_covers_exactly_the_requested_range() {
    // The step is (high − low) / points. A symmetric range catches
    // subtraction being replaced with addition: the sum of the bounds is zero there, the step
    // becomes zero, and scanning stops advancing.
    let policy = SolverPolicy {
        bracket_low: -0.5,
        bracket_high: 0.5,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert!((outcome.rate().value() - 0.1).abs() < 1e-9);
}

#[test]
fn a_bracket_already_within_tolerance_needs_no_iterations() {
    // If the interval found is already within tolerance, there is nothing to refine,
    // and the reported error is exactly half its width.
    //
    // The width here is known exactly: it is the scanning step, that is
    // (100 − (−0,9999)) / 1000 ≈ 0,10100. Half is about 0,05050.
    // The check is deliberately tied to the number of scanning points: without
    // the exact expectation, «half the width» is indistinguishable from «the width»
    // and from «twice the width».
    let policy = SolverPolicy {
        rate_tolerance: 1.0,
        ..SolverPolicy::returns_default()
    };
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert_eq!(outcome.rate().iterations(), 0);
    let bound = outcome.rate().error_bound();
    assert!(
        (0.0504..0.0506).contains(&bound),
        "error bound {bound}: this is not half the interval width"
    );
}

#[test]
fn a_series_with_three_sign_changes_is_refused_by_the_sign_rule() {
    // Here the grid finds exactly one sign-change interval—that is, it
    // would have «proved» uniqueness. The rule of signs rules out
    // uniqueness, and it alone does: without it, the system would return one of the
    // possible values as the answer.
    let flows = [
        flow(0, -1_000),
        flow(365, 2_000),
        flow(730, -1_000),
        flow(1_095, 400),
    ];
    assert_eq!(
        solve(&flows, SolverPolicy::returns_default(), DayCount::Act365),
        Err(SolverRefusal::UniquenessNotProven { sign_changes: 3 })
    );
}

#[test]
fn the_error_bound_shrinks_with_the_requested_tolerance() {
    // The error is half the width of the bracketing interval, not
    // an arbitrary number: tightening the tolerance must reduce it.
    let flows = [flow(0, -1_000), flow(365, 1_100)];
    let loose = solve(
        &flows,
        SolverPolicy {
            rate_tolerance: 1e-4,
            ..SolverPolicy::returns_default()
        },
        DayCount::Act365,
    )
    .unwrap();
    let tight = solve(&flows, SolverPolicy::returns_default(), DayCount::Act365).unwrap();
    assert!(loose.rate().error_bound() > tight.rate().error_bound());
    // The bound is HALF the interval width, not the width: the tolerance specifies
    // the width, so the reported error is half of it.
    assert!(loose.rate().error_bound() <= 1e-4 / 2.0 + f64::EPSILON);
}

#[test]
fn the_stopping_test_is_the_width_of_the_bracket_not_the_sum_of_its_ends() {
    // Refinement boundary case: the interval found by scanning is already
    // narrower than the tolerance—there is nothing to refine, and the result must be returned after zero
    // iterations. The stopping condition is the interval WIDTH, that is, the difference
    // between the endpoints; their sum is neither the width nor anything else relevant.
    // The root near +500 % per year was chosen deliberately: there, the difference between the endpoints
    // is small while their sum is large, making a substitution of one for the other visible.
    let flows = [flow(0, -1_000), flow(365, 6_000)];
    let policy = SolverPolicy {
        rate_tolerance: 0.5,
        max_iterations: 200,
        bracket_low: 4.0,
        bracket_high: 6.0,
    };
    let outcome = solve(&flows, policy, DayCount::Act365).unwrap();
    assert_eq!(
        outcome.rate().iterations(),
        0,
        "interval is already within tolerance—nothing to refine"
    );
    assert!((outcome.rate().value() - 5.0).abs() < 0.01);
    // The error is half the scanning-cell width: 2.0 / 1000 / 2.
    assert!(outcome.rate().error_bound() <= 0.001);
}
