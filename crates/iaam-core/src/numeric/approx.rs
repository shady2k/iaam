//! Approximate mode (§6.6): the only place in the core where
//! binary floating-point arithmetic is permitted.
//!
//! Used only where powers, roots, and iterations are required:
//! XIRR, CAGR, discounting. Results from this module **never**
//! enter the monetary identity in §6.3—the identity checks amounts,
//! not rates.

use rust_decimal::prelude::ToPrimitive;

use super::decimal::Dec;

/// Numerical-method policy. Every solver must declare it,
/// and it is included in the result alongside the number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolverPolicy {
    /// The allowed width of the root-bracketing interval—in units of the
    /// **rate**. It also determines the declared error bound of the result.
    ///
    /// There is exactly one tolerance, and it is in rate units. A tolerance on the
    /// residual magnitude is unnecessary and harmful here: near a flat root, the residual
    /// is small even when the rate error is large, while an absolute residual tolerance
    /// would also depend on the scale of the amounts—the same series, multiplied
    /// by a thousand, would stop at a different point even though the rate
    /// must be scale-invariant. The root is enclosed
    /// in the interval by construction, so half the width is a proven
    /// bound, not an estimate.
    pub rate_tolerance: f64,
    /// Maximum number of iterations before failure.
    pub max_iterations: u32,
    /// Lower bound of the root-bracketing interval.
    pub bracket_low: f64,
    /// Upper bound of the root-bracketing interval.
    pub bracket_high: f64,
}

impl SolverPolicy {
    /// Default policy for calculating rates of return.
    ///
    /// Bracketing from −99.99% to +10,000% annually covers any
    /// realistic result, including a total loss of capital.
    #[must_use]
    pub const fn returns_default() -> Self {
        Self {
            rate_tolerance: 1e-10,
            max_iterations: 200,
            bracket_low: -0.9999,
            bracket_high: 100.0,
        }
    }
}

/// An approximate value together with an error estimate.
///
/// It cannot be constructed without an error bound: a value
/// whose accuracy is unknown is useless for reporting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ApproxValue {
    value: f64,
    error_bound: f64,
    iterations: u32,
}

impl ApproxValue {
    #[must_use]
    pub const fn new(value: f64, error_bound: f64, iterations: u32) -> Self {
        Self {
            value,
            error_bound,
            iterations,
        }
    }

    #[must_use]
    pub const fn value(&self) -> f64 {
        self.value
    }

    #[must_use]
    pub const fn error_bound(&self) -> f64 {
        self.error_bound
    }

    #[must_use]
    pub const fn iterations(&self) -> u32 {
        self.iterations
    }
}

/// Explicit transition from monetary mode to approximate mode.
/// The only permitted point for such a transition.
#[must_use]
pub fn dec_to_f64(d: &Dec) -> Option<f64> {
    d.inner().to_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;
    use rust_decimal::Decimal;

    #[test]
    fn approx_value_carries_error_bound() {
        let v = ApproxValue::new(0.1234, 1e-9, 12);
        assert!(v.error_bound() > 0.0);
        assert_eq!(v.iterations(), 12);
    }

    #[test]
    fn approx_value_reports_the_value_it_was_built_from() {
        let v = ApproxValue::new(0.1234, 1e-9, 12);
        assert!((v.value() - 0.1234).abs() < f64::EPSILON);
    }

    #[test]
    fn returns_policy_brackets_total_loss_and_extreme_gain() {
        let p = SolverPolicy::returns_default();
        assert!(p.bracket_low < -0.99, "must cover a total loss of capital");
        assert!(p.bracket_high > 10.0, "must cover extreme growth");
    }

    #[test]
    fn returns_policy_stops_on_tolerance_and_iteration_budget() {
        // There is one tolerance, and it is in rate units: 1e-10 is one
        // ten-billionth of a percentage point, which is certainly
        // finer than any meaningful presentation precision.
        let p = SolverPolicy::returns_default();
        assert!(p.rate_tolerance > 0.0 && p.rate_tolerance < 1e-6);
        assert_eq!(p.max_iterations, 200);
    }

    #[test]
    fn dec_to_f64_is_the_only_crossing_point() {
        let d = Dec::new(Decimal::from_str("2.5").unwrap());
        assert_eq!(dec_to_f64(&d), Some(2.5));
    }
}
