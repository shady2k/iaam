//! Internal rate of return solver (§6.1, §6.6).
//!
//! The second and final core file where binary floating point is allowed:
//! the rate requires exponentiation to a fractional power, which `rust_decimal`
//! does not support. The solver result **never** enters the monetary
//! identity — it is derived from amounts, not one of their components (§6.6).
//!
//! **Root uniqueness is proven by the rule of signs, not by scanning.**
//! The substitution `x = 1/(1 + r)` transforms `NPV` into a generalized polynomial
//! `Σ aᵢ·x^tᵢ` with positive exponents, for which the number
//! of positive roots does not exceed the number of sign changes in the
//! chronologically ordered sequence of amounts. One sign change —
//! at most one root; combined with an interval whose endpoints
//! have opposite signs, this means exactly one root. Grid scanning
//! serves only to find such an interval: it cannot be used to count roots —
//! it misses roots of even multiplicity and pairs of roots within a single step.

use thiserror::Error;

use super::approx::{ApproxValue, SolverPolicy, dec_to_f64};
use super::decimal::Dec;

/// Day-count basis. Recorded in the result: without it, the rate
/// cannot be reproduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayCount {
    /// Actual days, 365-day year. XIRR convention.
    Act365,
}

impl DayCount {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Act365 => "act/365",
        }
    }

    const fn year_length(self) -> f64 {
        match self {
            Self::Act365 => 365.0,
        }
    }
}

/// Cash flow for the solver: offset in days from the first cash flow and amount
/// in the reporting currency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolverFlow {
    pub day_offset: i64,
    pub amount: Dec,
}

/// Solver failure. Failure is a result, not an exception: the NPV equation
/// for cash flows with alternating signs may have no roots, have
/// multiple roots, or not permit uniqueness to be proven, and an arbitrarily
/// chosen number is worse than an honest failure (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SolverRefusal {
    #[error("fewer than two cash flows: rate is undefined")]
    TooFewFlows,
    #[error("all cash flows have the same sign: the NPV equation has no root")]
    NoSignChange,
    #[error("root was not bracketed within the specified rate range")]
    RootNotBracketed,
    #[error("number of sign-change intervals found in the rate range: {count}; root is not unique")]
    MultipleRoots { count: u32 },
    #[error(
        "the cash-flow sign changes {sign_changes} times: root uniqueness cannot be proven, \
         and choosing one of the possible roots is not permitted"
    )]
    UniquenessNotProven { sign_changes: u32 },
    #[error("method did not converge within {iterations} iterations")]
    NotConverged { iterations: u32 },
    #[error("cash-flow amount cannot be converted to approximate mode or is not a number")]
    NotRepresentable,
    #[error("invalid bracketing range: lower bound is not less than upper bound")]
    BadBracket,
    #[error("all cash flows are zero: rate is undefined")]
    AllZero,
}

impl SolverRefusal {
    /// Machine-readable failure code. Required by the API: the text is intended for humans,
    /// while an external agent parses the code (§13).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooFewFlows => "too_few_flows",
            Self::NoSignChange => "no_sign_change",
            Self::RootNotBracketed => "root_not_bracketed",
            Self::MultipleRoots { .. } => "multiple_roots",
            Self::UniquenessNotProven { .. } => "uniqueness_not_proven",
            Self::NotConverged { .. } => "not_converged",
            Self::NotRepresentable => "not_representable",
            Self::BadBracket => "bad_bracket",
            Self::AllZero => "all_zero",
        }
    }
}

/// The solved rate together with the policy used to find it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateOutcome {
    rate: ApproxValue,
    policy: SolverPolicy,
    day_count: DayCount,
}

impl RateOutcome {
    /// Construct an exact boundary rate for which the numerical solver
    /// is inapplicable (for example, total loss of capital yields exactly −100 %).
    #[must_use]
    pub const fn exact(value: f64, policy: SolverPolicy, day_count: DayCount) -> Self {
        Self {
            rate: ApproxValue::new(value, 0.0, 0),
            policy,
            day_count,
        }
    }
    #[must_use]
    pub const fn rate(&self) -> ApproxValue {
        self.rate
    }

    #[must_use]
    pub const fn policy(&self) -> SolverPolicy {
        self.policy
    }

    #[must_use]
    pub const fn day_count(&self) -> DayCount {
        self.day_count
    }
}

/// Number of points used to scan the rate range.
///
/// With the default range (−99,99 %…+10 000 %), the step is approximately
/// 0,1 — that is, about ten percentage points. This is
/// sufficient to find a sign-change interval for a cash-flow series
/// with one sign change, and **insufficient** for drawing conclusions
/// about the number of roots: those conclusions are provided by the rule of signs.
const SCAN_POINTS: u32 = 1_000;

/// Internal cash-flow series in approximate mode.
struct Series {
    /// Pairs of «year fraction since the first cash flow, amount», in chronological order.
    terms: Vec<(f64, f64)>,
}

impl Series {
    fn build(flows: &[SolverFlow], day_count: DayCount) -> Result<Self, SolverRefusal> {
        if flows.len() < 2 {
            return Err(SolverRefusal::TooFewFlows);
        }
        let mut terms = Vec::with_capacity(flows.len());
        // Sum of absolute values: needed for exactly one conclusion — a series
        // consisting only of zeros has no rate. It is not stored in the structure
        // because it is not used anywhere else.
        let mut magnitude = 0.0_f64;
        for flow in flows {
            let amount = dec_to_f64(&flow.amount).ok_or(SolverRefusal::NotRepresentable)?;
            if !amount.is_finite() {
                return Err(SolverRefusal::NotRepresentable);
            }
            let years = flow.day_offset as f64 / day_count.year_length();
            if !years.is_finite() {
                return Err(SolverRefusal::NotRepresentable);
            }
            magnitude += amount.abs();
            terms.push((years, amount));
        }
        // The sum of absolute values is strictly positive for a non-empty, nonzero series.
        // It is checked exactly this way, rather than as «equal to zero»: a negative
        // value would indicate an error in the accumulation itself, and it must not
        // be silently ignored.
        if !magnitude.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if magnitude <= 0.0 {
            return Err(SolverRefusal::AllZero);
        }
        Ok(Self { terms })
    }

    /// Number of sign changes in the chronologically ordered sequence
    /// of amounts. Zero cash flows are skipped: zero has no sign.
    fn sign_changes(&self) -> u32 {
        let mut changes = 0;
        let mut previous = 0.0_f64;
        for (_, amount) in &self.terms {
            if *amount == 0.0 {
                continue;
            }
            if previous != 0.0 && previous.signum() != amount.signum() {
                changes += 1;
            }
            previous = *amount;
        }
        changes
    }

    fn npv(&self, rate: f64) -> f64 {
        self.terms
            .iter()
            .map(|(years, amount)| amount / (1.0 + rate).powf(*years))
            .sum()
    }
}

/// Scan intervals whose endpoints have NPV values of opposite signs.
///
/// Non-numeric NPV values terminate the search with a failure: `NaN` does not compare
/// equal to itself, and a naive sign check would turn it into a spurious root.
fn brackets(series: &Series, policy: SolverPolicy) -> Result<Vec<(f64, f64)>, SolverRefusal> {
    // The bounds must be comparable and ordered: NaN in the policy
    // means that the range is invalid, not «any range».
    if !policy.bracket_low.is_finite() || !policy.bracket_high.is_finite() {
        return Err(SolverRefusal::BadBracket);
    }
    if policy.bracket_low >= policy.bracket_high {
        return Err(SolverRefusal::BadBracket);
    }
    // A rate of −100 % makes the base of the exponent zero; below that, it becomes
    // negative, and a fractional power of a negative number is undefined.
    // The range must start strictly above it: a condition on the upper bound
    // is unnecessary here and would only create a second, untestable branch.
    if policy.bracket_low <= -1.0 {
        return Err(SolverRefusal::BadBracket);
    }
    let step = (policy.bracket_high - policy.bracket_low) / f64::from(SCAN_POINTS);
    let mut found = Vec::new();
    let mut previous_rate = policy.bracket_low;
    let mut previous_value = series.npv(previous_rate);
    if !previous_value.is_finite() {
        return Err(SolverRefusal::NotRepresentable);
    }
    for i in 1..=SCAN_POINTS {
        let rate = policy.bracket_low + step * f64::from(i);
        let value = series.npv(rate);
        if !value.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if value == 0.0 {
            found.push((rate, rate));
        } else if previous_value != 0.0 && previous_value.signum() != value.signum() {
            found.push((previous_rate, rate));
        }
        previous_rate = rate;
        previous_value = value;
    }
    Ok(found)
}

/// Root refinement using the Illinois method (§6.1).
///
/// This is a modified false-position method: it **never** loses the
/// bracketing interval — the two endpoints always yield values of opposite signs, —
/// while converging superlinearly because when one endpoint stalls,
/// its value is halved and the next secant jumps
/// to the other side.
///
/// Why not Newton's method with a bisection fallback, as in the first revision:
/// when a Newton step lands close to the root, it barely moves the far endpoint
/// of the interval, while the stated error is calculated from the interval itself.
/// The «did not halve — bisect» safeguard triggered almost every time,
/// and the method degenerated into pure bisection: thirty-seven iterations where
/// only a few were sufficient. Confirmed by execution.
///
/// The stopping criterion is based on **interval width**, not residual magnitude:
/// near a flat root, the residual is small even when the rate error is large.
/// A separate residual check is unnecessary: the root lies within the interval
/// by construction, so half the width is a proven bound,
/// not an estimate.
fn refine(
    series: &Series,
    bracket: (f64, f64),
    policy: SolverPolicy,
) -> Result<ApproxValue, SolverRefusal> {
    // The interval endpoints are scan points, and their values have already been checked
    // for finiteness in `brackets`: an interval is returned only when
    // both values are finite and have opposite signs. Rechecking
    // here would be a dead branch, and a dead check creates the false
    // impression that the case is handled.
    let (mut low, mut high) = bracket;
    let mut low_value = series.npv(low);
    let mut high_value = series.npv(high);
    if high - low <= policy.rate_tolerance {
        return Ok(finish(low, high, 0));
    }

    for iteration in 1..=policy.max_iterations {
        let denominator = high_value - low_value;
        let secant = high - high_value * (high - low) / denominator;
        let guess = if secant_is_inside(secant, low, high) {
            secant
        } else {
            (low + high) / 2.0
        };

        let value = series.npv(guess);
        if !value.is_finite() {
            return Err(SolverRefusal::NotRepresentable);
        }
        if value == 0.0 {
            return Ok(finish(guess, guess, iteration));
        }

        if value.signum() == high_value.signum() {
            high = guess;
            high_value = value;
            // Illinois adjustment: the stalled endpoint is «weakened», and the next
            // secant jumps to the other side of the root.
            low_value /= 2.0;
        } else {
            low = high;
            low_value = high_value;
            high = guess;
            high_value = value;
        }

        let (left, right) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        if right - left <= policy.rate_tolerance {
            return Ok(finish(left, right, iteration));
        }
    }
    Err(SolverRefusal::NotConverged {
        iterations: policy.max_iterations,
    })
}

/// Secant acceptance criterion: strictly inside the bracketing interval.
///
/// A single comparison covers everything that can go wrong: `NaN` is neither greater
/// nor less than anything, infinity (from a zero denominator) is not less than
/// the upper bound, and a secant outside the interval fails by definition.
///
/// Extracted into a separate function not for readability, but for testability:
/// inside the loop this branch is unreachable — for a pair of values with opposite signs,
/// the secant mathematically lies between the endpoints, — and the mutation barrier
/// rightly called its conditions equivalent. A separate function
/// is tested directly, and the guard is no longer untested.
const fn secant_is_inside(secant: f64, low: f64, high: f64) -> bool {
    secant > low && secant < high
}

/// The midpoint of the interval as the value, half its width as the proven
/// error bound.
fn finish(low: f64, high: f64, iterations: u32) -> ApproxValue {
    ApproxValue::new((low + high) / 2.0, (high - low).abs() / 2.0, iterations)
}

/// The rate at which the present value of the cash flows equals zero.
pub fn solve(
    flows: &[SolverFlow],
    policy: SolverPolicy,
    day_count: DayCount,
) -> Result<RateOutcome, SolverRefusal> {
    let series = Series::build(flows, day_count)?;
    let sign_changes = series.sign_changes();
    if sign_changes == 0 {
        return Err(SolverRefusal::NoSignChange);
    }

    let found = brackets(&series, policy)?;
    let bracket = match found.len() {
        0 => return Err(SolverRefusal::RootNotBracketed),
        1 => found[0],
        n => {
            return Err(SolverRefusal::MultipleRoots {
                count: u32::try_from(n).unwrap_or(u32::MAX),
            });
        }
    };

    // The sole bracket found proves that the root is unique
    // only when the cash flows have a single sign change. With more sign changes,
    // the grid could miss an even-multiplicity root or a pair of roots
    // within one step—and return one of several values as the answer.
    if sign_changes > 1 {
        return Err(SolverRefusal::UniquenessNotProven { sign_changes });
    }

    let rate = refine(&series, bracket, policy)?;
    Ok(RateOutcome {
        rate,
        policy,
        day_count,
    })
}
#[cfg(test)]
mod tests {
    use super::secant_is_inside;

    /// The secant safeguard is tested directly: it is unreachable inside the loop,
    /// and an unreachable check is one for which it is unknown
    /// whether it works.
    #[test]
    fn only_a_strictly_interior_secant_is_accepted() {
        assert!(secant_is_inside(0.5, 0.0, 1.0));
        // The endpoints are unsuitable: accepting an endpoint would cause the method to stop
        // shrinking the interval and loop forever.
        assert!(!secant_is_inside(0.0, 0.0, 1.0));
        assert!(!secant_is_inside(1.0, 0.0, 1.0));
        // Leaving the interval means losing localization, that is, losing
        // the proven error bound.
        assert!(!secant_is_inside(-0.1, 0.0, 1.0));
        assert!(!secant_is_inside(1.1, 0.0, 1.0));
        // NaN and infinities caused by a zero denominator.
        assert!(!secant_is_inside(f64::NAN, 0.0, 1.0));
        assert!(!secant_is_inside(f64::INFINITY, 0.0, 1.0));
        assert!(!secant_is_inside(f64::NEG_INFINITY, 0.0, 1.0));
    }
}
