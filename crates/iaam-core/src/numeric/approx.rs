//! Приближённый режим (§6.6): единственное место в ядре, где разрешена
//! двоичная плавающая точка.
//!
//! Применяется только там, где требуются степени, корни и итерации:
//! XIRR, CAGR, дисконтирование. Результаты этого модуля **никогда**
//! не входят в денежное тождество §6.3 — тождество проверяет суммы,
//! а не ставки.

use rust_decimal::prelude::ToPrimitive;

use super::decimal::Dec;

/// Политика численного метода. Каждый решатель обязан её объявить,
/// и она попадает в результат рядом с числом.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolverPolicy {
    /// Допустимая ширина интервала, локализующего корень, — в единицах
    /// **ставки**. Она же определяет объявленную погрешность результата.
    ///
    /// Допуск ровно один, и он в единицах ставки. Допуск по величине
    /// невязки здесь не нужен и вреден: возле пологого корня невязка
    /// мала при большой ошибке ставки, а абсолютный допуск по невязке
    /// ещё и зависел бы от масштаба денег — та же серия, умноженная
    /// на тысячу, останавливалась бы в другой точке, хотя ставка
    /// обязана быть масштабно-инвариантной. Корень заключён
    /// в интервале по построению, поэтому половина ширины — доказанная
    /// граница, а не оценка.
    pub rate_tolerance: f64,
    /// Максимум итераций до отказа.
    pub max_iterations: u32,
    /// Нижняя граница локализации корня.
    pub bracket_low: f64,
    /// Верхняя граница локализации корня.
    pub bracket_high: f64,
}

impl SolverPolicy {
    /// Политика по умолчанию для расчёта ставок доходности.
    ///
    /// Локализация от −99,99 % до +10 000 % годовых покрывает любой
    /// реалистичный результат, включая полную потерю капитала.
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

/// Приближённое значение вместе с оценкой погрешности.
///
/// Сконструировать без границы погрешности невозможно: значение,
/// про которое неизвестно, насколько оно точно, бесполезно для отчёта.
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

/// Явный переход из денежного режима в приближённый.
/// Единственная разрешённая точка такого перехода.
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
        assert!(
            p.bracket_low < -0.99,
            "должна покрывать полную потерю капитала"
        );
        assert!(p.bracket_high > 10.0, "должна покрывать экстремальный рост");
    }

    #[test]
    fn returns_policy_stops_on_tolerance_and_iteration_budget() {
        // Допуск один и он в единицах ставки: 1e-10 — это одна
        // десятимиллиардная процентного пункта, то есть заведомо
        // тоньше любой осмысленной подачи результата.
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
