//! Накопленный купонный доход (§3.2 спеки E3.4.4).
//!
//! Версионированное правило, а не арифметика на месте: включительность
//! границы периода и стратегия округления меняют сумму при одинаковом
//! `inputs_hash` (§2.7 основной спеки E3.4).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::bond::AccrualPeriod;
use crate::money::PerUnitAmount;
use crate::numeric::decimal::Dec;
use crate::numeric::NumericError;

/// Версия правила НКД.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccruedInterestRuleVersion(pub u32);

/// Причина, по которой НКД не считается.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccruedInterestError {
    #[error("дата вне покрытия графика")]
    OutsideCoverage,
    #[error("сумма купона периода не определена")]
    CouponUndetermined,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Начисленный на дату купонный доход на одну бумагу.
pub trait AccruedInterestRule: Send + Sync + std::fmt::Debug {
    fn accrued_per_unit(
        &self,
        periods: &[AccrualPeriod],
        as_of: Date,
    ) -> Result<PerUnitAmount, AccruedInterestError>;
}

/// Линейное начисление внутри периода.
///
/// Базы начисления дней правило НЕ требует: доля периода
/// самонормируется. Это существенно — MOEX базы не даёт вовсе (§2.11
/// основной спеки), а подставленная база даёт правдоподобно неверный НКД.
///
/// Эквивалентность ACT/365 проверена живьём на 6814 наблюдениях по пяти
/// бумагам, включая нерегулярный период в 175 дней: ноль расхождений.
#[derive(Debug, Default)]
pub struct AccruedInterestV1;

impl AccruedInterestRule for AccruedInterestV1 {
    fn accrued_per_unit(
        &self,
        periods: &[AccrualPeriod],
        as_of: Date,
    ) -> Result<PerUnitAmount, AccruedInterestError> {
        // Граница полуоткрыта: [period_start, accrual_end). На accrual_end
        // купон начислен целиком и принадлежит прошедшему периоду, а
        // следующий период стартует с нуля — инвариант замкнутой цепи
        // (completeness.rs) это гарантирует.
        let period = periods
            .iter()
            .find(|period| period.period_start <= as_of && as_of < period.accrual_end)
            .ok_or(AccruedInterestError::OutsideCoverage)?;
        let coupon = period
            .coupon_per_unit
            .as_ref()
            .ok_or(AccruedInterestError::CouponUndetermined)?;

        let elapsed = (as_of - period.period_start).whole_days();
        let whole = (period.accrual_end - period.period_start).whole_days();
        // Период нулевой длины разделить нельзя; график с таким периодом
        // структурно неверен, и молчаливый ноль его бы спрятал.
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

    /// Купонный период ОФЗ SU26238RMFS4, проверенный живьём 2026-08-27:
    /// 2026-06-03 → 2026-12-02, купон 35.40 ₽ на бумагу.
    fn ofz_periods() -> Vec<AccrualPeriod> {
        vec![
            AccrualPeriod {
                period_start: date!(2026 - 06 - 03),
                accrual_end: date!(2026 - 12 - 02),
                payment_date: date!(2026 - 12 - 02),
                coupon_per_unit: Some(PerUnitAmount::new(dec("35.40"), CurrencyCode::Rub)),
            },
            AccrualPeriod {
                period_start: date!(2026 - 12 - 02),
                accrual_end: date!(2027 - 06 - 02),
                payment_date: date!(2027 - 06 - 02),
                coupon_per_unit: Some(PerUnitAmount::new(dec("35.40"), CurrencyCode::Rub)),
            },
        ]
    }

    #[test]
    fn the_rule_reproduces_the_kopeck_the_exchange_published() {
        // Три точки сняты живым вызовом ISS: 15.17, 15.37 и 15.95.
        // Это эталон против конкретного источника, а не абстрактное
        // свойство: если правило разъедется с биржей, разъедется тут.
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
                "расхождение на {day}"
            );
        }
    }

    #[test]
    fn on_the_accrual_end_the_next_period_starts_at_zero() {
        // Главная ловушка полуоткрытой границы: на accrual_end купон
        // уже начислен целиком и относится к ПРОШЕДШЕМУ периоду.
        // Включительная граница показала бы целый купон вместо нуля.
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
        // Ноль здесь неотличим от незнания и молча занизил бы NAV.
        let rule = AccruedInterestV1;
        assert!(matches!(
            rule.accrued_per_unit(&ofz_periods(), date!(2026 - 01 - 01)),
            Err(AccruedInterestError::OutsideCoverage)
        ));
    }

    #[test]
    fn an_undetermined_coupon_is_refused_not_zeroed() {
        // Флоатер с неназванной суммой: правильный ответ — «не знаем».
        let rule = AccruedInterestV1;
        let periods = vec![AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            coupon_per_unit: None,
        }];
        assert!(matches!(
            rule.accrued_per_unit(&periods, date!(2026 - 08 - 20)),
            Err(AccruedInterestError::CouponUndetermined)
        ));
    }
}
