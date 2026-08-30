//! Structural schedule-completeness invariants (§2.10, §2.11).
//!
//! The source provides neither a cursor nor a record count, so there is no
//! count to compare. Completeness is proved structurally; all three invariants
//! were checked against a live sample of 50 TQOB and TQCB securities—50/50 for
//! each.
//!
//! The invariants belong to the **source profile**, not the domain, and have an
//! explicit applicability range: zero-coupon, perpetual, and legally unusual
//! issues were absent from the sample.

use rust_decimal::Decimal;
use time::Date;

use iaam_core::numeric::decimal::Dec;

// `CouponAmount` and `Knowledge` are not needed here: invariants inspect dates
// and shares, not amounts. Tests need them and import them in the test module.
use crate::schedule::{CouponPeriod, PrincipalRepayment};

/// Result of the structural check.
///
/// `Incomplete` rather than `complete_prefix` is intentional: a downloaded
/// but truncated schedule looks closed and plausible, while “complete prefix”
/// sounds like “almost everything is fine”.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Validated,
    Incomplete {
        reason: String,
    },
    /// Issue outside the profile's applicability range.
    Unknown,
}

/// Check the three invariants of the MOEX profile.
#[must_use]
pub fn validate_moex_profile(
    coupons: &[CouponPeriod],
    repayments: &[PrincipalRepayment],
) -> Completeness {
    if coupons.is_empty() || repayments.is_empty() {
        // The sample contained neither zero-coupon nor perpetual issues.
        // Rejecting their valid schedules is as wrong as accepting a truncated
        // one, so this is unknown rather than a refusal.
        return Completeness::Unknown;
    }

    // Invariant 1: the coupon-period chain is closed.
    for pair in coupons.windows(2) {
        if pair[0].accrual_end != pair[1].period_start {
            return Completeness::Incomplete {
                reason: format!(
                    "period chain gap: period ends {}, next starts {}",
                    pair[0].accrual_end, pair[1].period_start
                ),
            };
        }
    }

    // Invariant 2: the tail agrees with the last principal return.
    // This catches a truncated page: stopping after a whole period leaves the
    // chain closed, and nothing else notices.
    let last_accrual = coupons
        .iter()
        .map(|period| period.accrual_end)
        .max()
        .unwrap_or(Date::MIN);
    let last_return = repayments
        .iter()
        .map(|repayment| repayment.repayment_date)
        .max()
        .unwrap_or(Date::MIN);
    if last_accrual != last_return {
        return Completeness::Incomplete {
            reason: format!("schedule tail {last_accrual} does not meet last return {last_return}"),
        };
    }

    // Invariant 3: return shares sum to exactly 100%.
    // Sum through Dec, not raw Decimal: overflow and loss of precision here
    // are refusals, not silently wrong totals.
    let shares = repayments
        .iter()
        .map(|repayment| repayment.share_percent)
        .collect::<Vec<_>>();
    let total = match Dec::sum(&shares) {
        Ok(total) => total,
        Err(error) => {
            return Completeness::Incomplete {
                reason: format!("principal-return shares do not sum: {error}"),
            };
        }
    };
    if total != Dec::new(Decimal::from(100)) {
        return Completeness::Incomplete {
            reason: format!("principal-return shares total {}, not 100", total.inner()),
        };
    }

    Completeness::Validated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{CouponAmount, Knowledge};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn coupon(start: Date, end: Date) -> CouponPeriod {
        CouponPeriod {
            period_start: start,
            accrual_end: end,
            payment_date: end,
            record_date: Knowledge::Unknown,
            amount: CouponAmount::Undetermined,
            source_entry_id: None,
        }
    }

    fn repayment(date: Date, share: i64) -> PrincipalRepayment {
        PrincipalRepayment {
            repayment_date: date,
            share_percent: Dec::new(Decimal::from(share)),
            source_kind: "amortization".to_owned(),
            source_entry_id: None,
        }
    }

    #[test]
    fn a_whole_schedule_validates() {
        let coupons = vec![
            coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15)),
            coupon(date!(2026 - 08 - 15), date!(2027 - 02 - 15)),
        ];
        let repayments = vec![repayment(date!(2027 - 02 - 15), 100)];
        assert_eq!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Validated
        );
    }

    #[test]
    fn a_truncated_page_is_caught_by_the_tail_not_by_the_chain() {
        // This is the main trap: a truncated page stops after a whole period,
        // leaving the chain closed and the schedule apparently complete. Only
        // matching the tail to the last return catches it.
        let coupons = vec![coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15))];
        let repayments = vec![repayment(date!(2036 - 02 - 06), 100)];
        assert!(matches!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Incomplete { .. }
        ));
    }

    #[test]
    fn a_broken_chain_is_named_as_such() {
        let coupons = vec![
            coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15)),
            coupon(date!(2026 - 09 - 15), date!(2027 - 02 - 15)),
        ];
        let repayments = vec![repayment(date!(2027 - 02 - 15), 100)];
        let outcome = validate_moex_profile(&coupons, &repayments);
        let Completeness::Incomplete { reason } = outcome else {
            panic!("period chain gap must be detected: {outcome:?}");
        };
        assert!(
            reason.contains("2026-09-15"),
            "reason must name the location: {reason}"
        );
    }

    #[test]
    fn shares_that_do_not_sum_to_a_hundred_are_incomplete() {
        let coupons = vec![coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15))];
        let repayments = vec![repayment(date!(2026 - 08 - 15), 75)];
        assert!(matches!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Incomplete { .. }
        ));
    }

    #[test]
    fn an_issue_outside_the_profile_is_unknown_not_rejected() {
        // Invariants were checked on coupon issues with redemption. Zero-coupon
        // and perpetual issues were absent from the sample, and rejecting a
        // valid schedule is as wrong as accepting a truncated one.
        assert_eq!(validate_moex_profile(&[], &[]), Completeness::Unknown);
    }

    #[test]
    fn coupons_without_any_repayment_are_unknown_not_incomplete() {
        // Applicability is a coupon issue WITH redemption. A coupon series
        // without a principal return is outside it; declaring it incomplete
        // would reject a valid perpetual issue. Unknown is not a violation.
        let coupons = vec![coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15))];
        assert_eq!(validate_moex_profile(&coupons, &[]), Completeness::Unknown);
    }

    #[test]
    fn repayments_without_any_coupon_are_unknown_too() {
        // A zero-coupon issue has a return but no coupons: the same profile
        // boundary from the other side.
        let repayments = vec![repayment(date!(2026 - 08 - 15), 100)];
        assert_eq!(
            validate_moex_profile(&[], &repayments),
            Completeness::Unknown
        );
    }
}
