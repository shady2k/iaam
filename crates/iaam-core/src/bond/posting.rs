//! Ближайшая выплата по бумаге (§5 спеки E3.4.4).

use time::Date;

use crate::bond::{AccrualPeriod, PrincipalReturn};

/// Дата ближайшей ЛЮБОЙ выплаты не раньше `as_of`.
///
/// Купон берётся по `payment_date`: перенос с выходного двигает платёж,
/// но не начисление, и `accrual_end` обещал бы деньги раньше срока.
///
/// Окно оферты из графика сюда НЕ входит — это право, а не платёж
/// (E3.4.6). Входит расчёт по уже поданной заявке: она приходит из
/// проекции заявок, а не из графика источника.
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
        // Перенос с выходного двигает платёж на 3 декабря, начисление
        // остаётся на 2-е. Взять accrual_end значит обещать деньги
        // на день раньше, чем они придут.
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 03),
            coupon_per_unit: None,
        }];
        assert_eq!(
            next_posting_date(&periods, &[], &[], date!(2026 - 08 - 20)),
            Some(date!(2026 - 12 - 03))
        );
    }

    #[test]
    fn an_amortisation_competes_with_the_coupon_on_equal_terms() {
        // Выбор только из купонного графика был бы неполон: на
        // амортизируемой бумаге ближайшие деньги — возврат номинала.
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
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
        // Окно оферты из графика — право, а не платёж (E3.4.6).
        // Уже ПОДАННАЯ заявка — платёж, и она приходит из проекции.
        assert_eq!(
            next_posting_date(&[], &[], &[date!(2026 - 09 - 01)], date!(2026 - 08 - 20)),
            Some(date!(2026 - 09 - 01))
        );
    }

    #[test]
    fn nothing_ahead_is_none_not_a_far_future_guess() {
        assert_eq!(next_posting_date(&[], &[], &[], date!(2026 - 08 - 20)), None);
    }
}
