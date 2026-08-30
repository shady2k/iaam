//! Доменные типы графика выплат, нужные расчёту (§7 плана E3.4.4).
//!
//! Это НЕ зеркало `iaam_market::schedule`. Ядро не зависит от крейт
//! воркспейса (§3.2), а правило НКД — политика и обязано жить здесь,
//! рядом с `ValuationPolicyV1`. Перевод снимка источника в эти типы
//! делает `iaam-app` и делает **структурно**: любое условие в нём —
//! признак, что правило утекло из ядра.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::instrument::CurrencyRoles;
use crate::money::PerUnitAmount;
use crate::numeric::decimal::Dec;

pub mod finality;
pub mod offer;
pub mod posting;
pub mod principal;
pub use offer::{
    OfferRight, OfferWindowError, OfferWindowId, OfferWindowTerms, ScheduleCompleteness,
};
pub use principal::{RemainingPrincipalError, remaining_principal};

/// Объявленный дефолт по выпуску.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultFlags {
    pub declared: bool,
    pub technical: bool,
}

/// Купонный период: начисление и платёж — разные даты.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccrualPeriod {
    pub period_start: Date,
    /// Конец начисления. По нему считается НКД.
    pub accrual_end: Date,
    /// Дата платежа. Двигается переносом с выходного.
    pub payment_date: Date,
    /// Дата фиксации реестра — она решает, КОМУ платят.
    ///
    /// `None` означает «источник не сообщил». Подставлять вместо неё
    /// дату платежа запрещено: зазор между ними непостоянен (0–5 дней
    /// по фикстурам), и в 157 случаях из 275 он равен одному дню —
    /// ровно тем дням, когда сделка меняет ответ.
    pub record_date: Option<Date>,
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

/// График выплат облигации на координату знания.
///
/// Это компактный доменный вход ядра, а не зеркало структуры источника:
/// перевод снимка в него выполняет слой приложения.
///
/// `Default` нужен для тестовых литералов через `..Default::default()`.
/// В рабочем коде `BondSchedule::default()` вызывать нельзя: пустой график
/// с полнотой `Unknown` означает неизвестный источник, а не отсутствие графика.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BondSchedule {
    pub periods: Vec<AccrualPeriod>,
    pub principal_returns: Vec<PrincipalReturn>,
    /// Первоначальный номинал на одну бумагу.
    ///
    /// `None` — источник не сообщил либо бумага не долговая. Ноль
    /// подставлять запрещено (§4.9): «номинал ноль» и «номинал
    /// неизвестен» требуют от владельца разных действий.
    ///
    /// Текущий номинал здесь отсутствует намеренно: остаток выводится
    /// из первоначального и ряда возвратов, и второй источник истины
    /// разошёлся бы с первым молча.
    #[serde(default)]
    pub initial_principal: Option<PerUnitAmount>,
    #[serde(default)]
    pub offer_windows: Vec<offer::OfferWindowTerms>,
    #[serde(default)]
    pub completeness: offer::ScheduleCompleteness,
    #[serde(default)]
    pub default_flags: Option<DefaultFlags>,
    #[serde(default)]
    pub currency_roles: Option<CurrencyRoles>,
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
            record_date: None,
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
            record_date: None,
            coupon_per_unit: None,
        };
        assert!(period.coupon_per_unit.is_none());
    }

    #[test]
    fn bond_schedule_carries_typed_offer_and_quality_inputs() {
        let instrument = crate::ids::InstrumentId::new_random();
        let window = offer::OfferWindowId::derive(instrument, date!(2026 - 12 - 01));
        let schedule = BondSchedule {
            periods: Vec::new(),
            principal_returns: Vec::new(),
            initial_principal: None,
            offer_windows: vec![offer::OfferWindowTerms {
                window,
                right: offer::OfferRight::HolderPut,
                execution_date: date!(2026 - 12 - 01),
                submission_start: None,
                submission_end: None,
                price_percent: None,
            }],
            completeness: offer::ScheduleCompleteness::Validated,
            default_flags: Some(DefaultFlags {
                declared: false,
                technical: false,
            }),
            currency_roles: Some(CurrencyRoles::uniform(crate::money::CurrencyCode::Rub)),
        };

        assert_eq!(schedule.offer_windows[0].window, window);
        assert_eq!(
            schedule.completeness,
            offer::ScheduleCompleteness::Validated
        );
        assert!(schedule.currency_roles.is_some());
        assert!(BondSchedule::default().default_flags.is_none());
    }
}
