//! Accrued coupon income (§3.2 of spec E3.4.4).
//!
//! A versioned rule, not inline arithmetic: period-boundary inclusion and
//! rounding strategy change the amount even with the same `inputs_hash`
//! (§2.7 of the main spec E3.4).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::bond::AccrualPeriod;
use crate::money::PerUnitAmount;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Accrued-interest rule version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccruedInterestRuleVersion(pub u32);

/// Why accrued interest cannot be calculated.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccruedInterestError {
    #[error("date is outside schedule coverage")]
    OutsideCoverage,
    #[error("date is covered by multiple periods")]
    OverlappingCoverage,
    #[error("period coupon amount is undetermined")]
    CouponUndetermined,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Coupon income accrued per security as of a date.
pub trait AccruedInterestRule: Send + Sync + std::fmt::Debug {
    fn accrued_per_unit(
        &self,
        periods: &[AccrualPeriod],
        as_of: Date,
    ) -> Result<PerUnitAmount, AccruedInterestError>;
}

/// Linear accrual within a period.
///
/// The rule does NOT require a day-count basis: the period fraction
/// self-normalises. This matters — MOEX provides no basis at all (§2.11 of
/// the main spec), and an invented basis produces plausibly wrong accrued
/// interest.
///
/// ACT/365 equivalence was checked live on 6814 observations for five
/// securities, including an irregular 175-day period: zero discrepancies.
#[derive(Debug, Default)]
pub struct AccruedInterestV1;

impl AccruedInterestRule for AccruedInterestV1 {
    fn accrued_per_unit(
        &self,
        periods: &[AccrualPeriod],
        as_of: Date,
    ) -> Result<PerUnitAmount, AccruedInterestError> {
        // Half-open boundary: [period_start, accrual_end). At accrual_end the
        // coupon is fully accrued and belongs to the elapsed period, while the
        // next period starts at zero — the closed-chain invariant
        // (completeness.rs) guarantees this.
        let covering: Vec<_> = periods
            .iter()
            .filter(|period| period.period_start <= as_of && as_of < period.accrual_end)
            .collect();
        let period = covering
            .first()
            .ok_or(AccruedInterestError::OutsideCoverage)?;
        if covering.len() > 1 {
            return Err(AccruedInterestError::OverlappingCoverage);
        }
        let coupon = period
            .coupon_per_unit
            .as_ref()
            .ok_or(AccruedInterestError::CouponUndetermined)?;

        let elapsed = (as_of - period.period_start).whole_days();
        let whole = (period.accrual_end - period.period_start).whole_days();
        // A zero-length period cannot be divided; a schedule with one is
        // structurally invalid, and a silent zero would hide it.
        if whole <= 0 {
            return Err(AccruedInterestError::OutsideCoverage);
        }
        let fraction =
            Dec::new(Decimal::from(elapsed)).checked_div(Dec::new(Decimal::from(whole)))?;
        let accrued = coupon.value().checked_mul(fraction)?;
        let rounded = accrued.checked_round_to_scale(coupon.currency().minor_units())?;
        Ok(PerUnitAmount::new(rounded, coupon.currency()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::AccrualPeriod;
    use crate::money::{CurrencyCode, PerUnitAmount};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    /// Coupon period for OFZ SU26238RMFS4, checked live on 2026-08-27:
    /// 2026-06-03 → 2026-12-02, coupon 35.40 ₽ per security.
    fn ofz_periods() -> Vec<AccrualPeriod> {
        vec![
            AccrualPeriod {
                period_start: date!(2026 - 06 - 03),
                accrual_end: date!(2026 - 12 - 02),
                payment_date: date!(2026 - 12 - 02),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("35.40"), CurrencyCode::Rub)),
            },
            AccrualPeriod {
                period_start: date!(2026 - 12 - 02),
                accrual_end: date!(2027 - 06 - 02),
                payment_date: date!(2027 - 06 - 02),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("35.40"), CurrencyCode::Rub)),
            },
        ]
    }

    #[test]
    fn the_rule_reproduces_the_kopeck_the_exchange_published() {
        // Three points captured from live ISS calls: 15.17, 15.37, and 15.95.
        // This is a reference against a specific source, not an abstract
        // property: if the rule drifts from the exchange, it will drift here.
        let rule = AccruedInterestV1;
        let periods = ofz_periods();
        for (day, expected) in [
            (date!(2026 - 08 - 20), "15.17"),
            (date!(2026 - 08 - 21), "15.37"),
            (date!(2026 - 08 - 24), "15.95"),
        ] {
            assert_eq!(
                rule.accrued_per_unit(&periods, day).unwrap().value(),
                dec(expected),
                "discrepancy on {day}"
            );
        }
    }

    #[test]
    fn on_the_accrual_end_the_next_period_starts_at_zero() {
        // The main half-open-boundary trap: at accrual_end the coupon is
        // already fully accrued and belongs to the ELAPSED period.
        // An inclusive boundary would show a whole coupon instead of zero.
        let rule = AccruedInterestV1;
        assert_eq!(
            rule.accrued_per_unit(&ofz_periods(), date!(2026 - 12 - 02))
                .unwrap()
                .value(),
            Dec::zero()
        );
    }

    #[test]
    fn a_date_outside_the_schedule_is_refused_not_zeroed() {
        // Zero is indistinguishable from unknown here and would silently
        // understate NAV.
        let rule = AccruedInterestV1;
        assert!(matches!(
            rule.accrued_per_unit(&ofz_periods(), date!(2026 - 01 - 01)),
            Err(AccruedInterestError::OutsideCoverage)
        ));
    }
    #[test]
    fn overlapping_periods_are_refused_instead_of_ordered_by_input() {
        let periods = vec![
            AccrualPeriod {
                period_start: date!(2026 - 01 - 01),
                accrual_end: date!(2026 - 03 - 01),
                payment_date: date!(2026 - 03 - 01),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("10"), CurrencyCode::Rub)),
            },
            AccrualPeriod {
                period_start: date!(2026 - 02 - 01),
                accrual_end: date!(2026 - 04 - 01),
                payment_date: date!(2026 - 04 - 01),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("20"), CurrencyCode::Rub)),
            },
        ];
        assert!(matches!(
            AccruedInterestV1.accrued_per_unit(&periods, date!(2026 - 02 - 15)),
            Err(AccruedInterestError::OverlappingCoverage)
        ));
    }

    #[test]
    fn an_undetermined_coupon_is_refused_not_zeroed() {
        // A floater with no stated amount: the correct answer is “unknown”.
        let rule = AccruedInterestV1;
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            record_date: None,
            coupon_per_unit: None,
        }];
        assert!(matches!(
            rule.accrued_per_unit(&periods, date!(2026 - 08 - 20)),
            Err(AccruedInterestError::CouponUndetermined)
        ));
    }
}
