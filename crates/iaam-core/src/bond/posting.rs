//! Nearest payment for a security (§5 of spec E3.4.4).

use time::Date;

use crate::bond::{AccrualPeriod, PrincipalReturn};

/// Date of the nearest payment of ANY kind not earlier than `as_of`.
///
/// Coupons use `payment_date`: moving a weekend shifts the payment,
/// not accrual, and `accrual_end` would promise money before its due date.
///
/// An offer window from the schedule is NOT included — it is a right, not a
/// payment (E3.4.6). Settlement for an already submitted application is
/// included: it comes from the application projection, not the source schedule.
#[must_use]
pub fn next_posting_date(
    periods: &[AccrualPeriod],
    returns: &[PrincipalReturn],
    settled_offers: &[Date],
    as_of: Date,
) -> Option<Date> {
    periods
        .iter()
        .map(|period| period.payment_date)
        .chain(returns.iter().map(|item| item.repayment_date))
        .chain(settled_offers.iter().copied())
        .filter(|date| *date >= as_of)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::{AccrualPeriod, PrincipalReturn};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    #[test]
    fn a_coupon_is_taken_by_its_payment_date_not_by_its_accrual_end() {
        // Moving a weekend shifts payment to December 3, while accrual stays
        // on the 2nd. Using accrual_end would promise money a day early.
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 03),
            record_date: None,
            coupon_per_unit: None,
        }];
        assert_eq!(
            next_posting_date(&periods, &[], &[], date!(2026 - 08 - 20)),
            Some(date!(2026 - 12 - 03))
        );
    }

    #[test]
    fn an_amortisation_competes_with_the_coupon_on_equal_terms() {
        // Looking only at the coupon schedule would be incomplete: for an
        // amortising security, the nearest cash is a principal return.
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            record_date: None,
            coupon_per_unit: None,
        }];
        let returns = vec![PrincipalReturn {
            repayment_date: date!(2026 - 09 - 15),
            share_percent: Dec::new(Decimal::from(25)),
        }];
        assert_eq!(
            next_posting_date(&periods, &returns, &[], date!(2026 - 08 - 20)),
            Some(date!(2026 - 09 - 15))
        );
    }

    #[test]
    fn a_submitted_offer_settlement_competes_too() {
        // An offer window from the schedule is a right, not a payment (E3.4.6).
        // An already SUBMITTED application is a payment, and comes from the
        // projection.
        assert_eq!(
            next_posting_date(&[], &[], &[date!(2026 - 09 - 01)], date!(2026 - 08 - 20)),
            Some(date!(2026 - 09 - 01))
        );
    }

    #[test]
    fn nothing_ahead_is_none_not_a_far_future_guess() {
        assert_eq!(
            next_posting_date(&[], &[], &[], date!(2026 - 08 - 20)),
            None
        );
    }
}
