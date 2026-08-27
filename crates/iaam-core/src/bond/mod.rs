//! Доменные типы графика выплат, нужные расчёту (§7 плана E3.4.4).
//!
//! Это НЕ зеркало `iaam_market::schedule`. Ядро не зависит от крейт
//! воркспейса (§3.2), а правило НКД — политика и обязано жить здесь,
//! рядом с `ValuationPolicyV1`. Перевод снимка источника в эти типы
//! делает `iaam-app` и делает **структурно**: любое условие в нём —
//! признак, что правило утекло из ядра.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::money::PerUnitAmount;
use crate::numeric::decimal::Dec;

pub mod finality;

/// Купонный период: начисление и платёж — разные даты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccrualPeriod {
    pub period_start: Date,
    /// Конец начисления. По нему считается НКД.
    pub accrual_end: Date,
    /// Дата платежа. Двигается переносом с выходного.
    pub payment_date: Date,
    /// Сумма купона за период на одну бумагу.
    ///
    /// `None` — сумма не определена (флоатер, будущий период). Ноль
    /// означал бы бумагу, которая ничего не платит.
    pub coupon_per_unit: Option<PerUnitAmount>,
}

/// Возврат части номинала.
///
/// Доля, а не сумма: сумма зависит от остатка, а остаток выводится
/// из первоначального номинала и ряда возвратов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalReturn {
    pub repayment_date: Date,
    /// Доля ПЕРВОНАЧАЛЬНОГО номинала, в процентах.
    pub share_percent: Dec,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn an_accrual_period_keeps_accrual_end_and_payment_date_apart() {
        // НКД считается по accrual_end, ближайшая выплата — по
        // payment_date. Перенос с выходного двигает второе, но не первое.
        let period = AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 03),
            coupon_per_unit: None,
        };
        assert_ne!(period.accrual_end, period.payment_date);
    }

    #[test]
    fn an_undetermined_coupon_is_absent_not_zero() {
        // Ноль купона означал бы бумагу, которая ничего не платит,
        // и занизил бы и НКД, и все метрики §7.1.
        let period = AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            coupon_per_unit: None,
        };
        assert!(period.coupon_per_unit.is_none());
    }
}
