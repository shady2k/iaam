//! Денежный режим (§6.6): суммы, цены, курсы, НКД.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::NumericError;
use super::exact::Exact;

/// Максимальный масштаб, который умеет представить `Exact` без переполнения
/// при типичных величинах портфеля.
const MAX_SCALE: u32 = 18;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Dec(Decimal);

impl Dec {
    #[must_use]
    pub const fn new(value: Decimal) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn inner(&self) -> Decimal {
        self.0
    }

    #[must_use]
    pub const fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    /// Наибольший масштаб, переводимый в точный режим без потерь.
    #[must_use]
    pub const fn max_scale() -> u32 {
        MAX_SCALE
    }
    /// Округление до знака после запятой, половина — от нуля.
    ///
    /// Отдельный метод, а не `Decimal::round_dp` на месте: правило НКД
    /// округляет до минорной единицы валюты, и стратегия округления —
    /// часть версионированного правила, а не вкус вызывающего.
    pub fn checked_round_to_scale(self, scale: u32) -> Result<Self, NumericError> {
        if scale > Self::max_scale() {
            return Err(NumericError::ScaleTooLarge {
                scale,
                max: Self::max_scale(),
            });
        }
        Ok(Self::new(self.0.round_dp_with_strategy(
            scale,
            rust_decimal::RoundingStrategy::MidpointAwayFromZero,
        )))
    }

    #[must_use]
    pub const fn one() -> Self {
        Self(Decimal::ONE)
    }

    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    #[must_use]
    pub const fn is_positive(&self) -> bool {
        self.0.is_sign_positive() && !self.0.is_zero()
    }

    #[must_use]
    pub const fn is_negative(&self) -> bool {
        self.0.is_sign_negative() && !self.0.is_zero()
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, NumericError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(NumericError::Overflow)
    }

    pub fn checked_mul(self, other: Self) -> Result<Self, NumericError> {
        self.0
            .checked_mul(other.0)
            .map(Self)
            .ok_or(NumericError::Overflow)
    }

    pub fn checked_neg(self) -> Result<Self, NumericError> {
        Self::zero().checked_sub(self)
    }

    /// Сумма списка. Вынесена отдельно по той же причине, что и у `Exact`:
    /// суммирование компонентов отчёта обязано отказывать явно.
    pub fn sum(items: &[Self]) -> Result<Self, NumericError> {
        items
            .iter()
            .try_fold(Self::zero(), |acc, x| acc.checked_add(*x))
    }

    /// Сложение с проверкой переполнения. Штатный `+` у `Decimal` паникует
    /// при выходе за диапазон; тихая паника в расчёте доходности хуже
    /// типизированного отказа.
    pub fn checked_add(self, other: Self) -> Result<Self, NumericError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(NumericError::Overflow)
    }

    /// Деление с отказом на нуле. У `Decimal` штатное деление на ноль
    /// паникует, а `checked_div` возвращает `None` и на нуле, и на
    /// переполнении — два разных отказа под одним ответом. Ноль
    /// отделён явно: «делили на ноль» и «результат не представим» —
    /// разные диагнозы для того, кто читает отказ расчёта.
    pub fn checked_div(self, other: Self) -> Result<Self, NumericError> {
        if other.0.is_zero() {
            return Err(NumericError::DivisionByZero);
        }
        self.0
            .checked_div(other.0)
            .map(Self)
            .ok_or(NumericError::Overflow)
    }

    /// Перевод в точный режим. Возможен без потерь: десятичная дробь
    /// с масштабом `s` — это рациональное число со знаменателем `10^s`.
    pub fn to_exact(&self) -> Result<Exact, NumericError> {
        let scale = self.0.scale();
        if scale > MAX_SCALE {
            return Err(NumericError::ScaleTooLarge {
                scale,
                max: MAX_SCALE,
            });
        }
        let mantissa = self.0.mantissa();
        let den = 10_i128.checked_pow(scale).ok_or(NumericError::Overflow)?;
        Exact::new(mantissa, den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    fn dec(s: &str) -> Dec {
        Dec::new(Decimal::from_str(s).unwrap())
    }

    #[test]
    fn dividing_by_zero_is_an_error_not_a_panic() {
        assert_eq!(
            dec("1").checked_div(dec("0")),
            Err(NumericError::DivisionByZero)
        );
    }

    #[test]
    fn division_is_the_inverse_of_multiplication_where_it_is_exact() {
        assert_eq!(dec("200").checked_div(dec("1000")).unwrap(), dec("0.2"));
    }

    #[test]
    fn is_zero_holds_only_at_zero_and_ignores_the_sign_of_zero() {
        assert!(Dec::zero().is_zero());
        assert!(dec("0.000").is_zero());
        assert!(dec("-0").is_zero());
        assert!(!dec("0.0000000001").is_zero());
        assert!(!dec("-0.0000000001").is_zero());
    }

    #[test]
    fn one_is_the_multiplicative_unit_and_not_zero() {
        assert_eq!(Dec::one(), dec("1"));
        assert!(!Dec::one().is_zero());
    }

    #[test]
    fn sign_predicates_split_the_line_in_three_and_leave_zero_to_neither() {
        // Ноль не положителен и не отрицателен — обе проверки обязаны
        // отказать на нём, иначе «есть цена» и «цена ниже нуля»
        // начинают пересекаться.
        assert!(dec("0.01").is_positive());
        assert!(!dec("0.01").is_negative());
        assert!(dec("-0.01").is_negative());
        assert!(!dec("-0.01").is_positive());
        assert!(!Dec::zero().is_positive());
        assert!(!Dec::zero().is_negative());
        // Отрицательный ноль остаётся нулём: знак у него есть,
        // а величины нет.
        assert!(!dec("-0").is_negative());
    }

    #[test]
    fn checked_sub_and_neg_are_exact_and_signed() {
        assert_eq!(dec("0.3").checked_sub(dec("0.1")).unwrap(), dec("0.2"));
        assert_eq!(dec("1").checked_sub(dec("3")).unwrap(), dec("-2"));
        assert_eq!(dec("2.5").checked_neg().unwrap(), dec("-2.5"));
        assert_eq!(dec("-2.5").checked_neg().unwrap(), dec("2.5"));
        assert_eq!(Dec::zero().checked_neg().unwrap(), Dec::zero());
    }

    #[test]
    fn checked_mul_multiplies_and_refuses_overflow() {
        assert_eq!(dec("1.5").checked_mul(dec("4")).unwrap(), dec("6.0"));
        let max = Dec::new(Decimal::MAX);
        assert_eq!(max.checked_mul(max), Err(NumericError::Overflow));
    }

    #[test]
    fn checked_sub_refuses_overflow_instead_of_panicking() {
        let min = Dec::new(Decimal::MIN);
        assert_eq!(
            min.checked_sub(Dec::new(Decimal::MAX)),
            Err(NumericError::Overflow)
        );
    }

    #[test]
    fn sum_of_an_empty_list_is_zero_and_of_a_list_is_its_total() {
        assert_eq!(Dec::sum(&[]).unwrap(), Dec::zero());
        assert_eq!(
            Dec::sum(&[dec("0.1"), dec("0.2"), dec("-0.05")]).unwrap(),
            dec("0.25")
        );
    }

    #[test]
    fn sum_refuses_when_the_running_total_overflows() {
        let max = Dec::new(Decimal::MAX);
        assert_eq!(Dec::sum(&[max, max]), Err(NumericError::Overflow));
    }

    #[test]
    fn checked_add_sums_without_binary_rounding() {
        // Ожидание посчитано вручную, а не снято с вывода (§15.5).
        assert_eq!(dec("0.1").checked_add(dec("0.2")).unwrap(), dec("0.3"));
    }

    #[test]
    fn checked_add_is_not_either_operand() {
        // Отдельно от предыдущего: сумма разных величин обязана отличаться
        // от каждого слагаемого, иначе «сложение», возвращающее один из
        // операндов, прошло бы незамеченным.
        let sum = dec("2.5").checked_add(dec("7.5")).unwrap();
        assert_ne!(sum, dec("2.5"));
        assert_ne!(sum, dec("7.5"));
        assert_eq!(sum, dec("10"));
    }

    #[test]
    fn checked_add_refuses_overflow_instead_of_panicking() {
        let max = Dec::new(Decimal::MAX);
        assert_eq!(max.checked_add(max), Err(NumericError::Overflow));
    }

    #[test]
    fn decimal_to_exact_is_lossless() {
        let d = dec("123.456");
        let e = d.to_exact().unwrap();
        assert_eq!(e, Exact::new(123_456, 1_000).unwrap());
    }

    #[test]
    fn tenths_and_hundredths_are_exact_after_conversion() {
        let a = dec("0.1").to_exact().unwrap();
        let b = dec("0.2").to_exact().unwrap();
        let c = dec("0.3").to_exact().unwrap();
        assert_eq!(a.add(&b), c);
    }

    #[test]
    fn negative_values_keep_their_sign() {
        let e = dec("-2.5").to_exact().unwrap();
        assert_eq!(e, Exact::new(-5, 2).unwrap());
    }

    #[test]
    fn integer_scale_zero_converts_to_whole_number() {
        let e = dec("42").to_exact().unwrap();
        assert_eq!(e, Exact::from_int(42));
    }

    #[test]
    fn maximum_supported_scale_still_converts() {
        let d = dec("0.000000000000000001");
        assert_eq!(d.inner().scale(), Dec::max_scale());
        let e = d.to_exact().unwrap();
        assert_eq!(e, Exact::new(1, 1_000_000_000_000_000_000).unwrap());
    }

    #[test]
    fn scale_beyond_the_supported_maximum_is_rejected() {
        let d = dec("0.0000000000000000001");
        assert_eq!(
            d.to_exact(),
            Err(NumericError::ScaleTooLarge {
                scale: 19,
                max: MAX_SCALE
            })
        );
    }

    #[test]
    fn zero_is_the_decimal_zero() {
        assert_eq!(Dec::zero().inner(), Decimal::ZERO);
        assert!(Dec::zero().to_exact().unwrap().is_zero());
    }

    #[test]
    fn ordering_follows_the_underlying_decimal() {
        assert!(dec("1.10") < dec("1.20"));
        assert!(dec("-1.0") < dec("0"));
    }
    #[test]
    fn rounding_to_a_scale_matches_the_kopeck_of_the_source() {
        // Числа взяты из живой сверки с MOEX: линейный расчёт даёт
        // 0.70571 и 17.99571, источник печатает 0.71 и 18.00.
        let value = Dec::new(Decimal::from_str_exact("0.70571").unwrap());
        assert_eq!(
            value.checked_round_to_scale(2).unwrap(),
            Dec::new(Decimal::from_str_exact("0.71").unwrap())
        );
        let value = Dec::new(Decimal::from_str_exact("17.99571").unwrap());
        assert_eq!(
            value.checked_round_to_scale(2).unwrap(),
            Dec::new(Decimal::from_str_exact("18.00").unwrap())
        );
    }

    #[test]
    fn a_scale_beyond_the_limit_is_refused_not_truncated() {
        // Молчаливое усечение до max_scale дало бы число, о котором
        // вызывающий думает, что оно точнее, чем есть.
        let value = Dec::new(Decimal::from_str_exact("1.5").unwrap());
        assert!(value.checked_round_to_scale(Dec::max_scale() + 1).is_err());
    }

    #[test]
    fn the_limit_itself_is_allowed_not_refused() {
        // Граница включительная: max_scale — предельный, а не первый
        // запрещённый знак. Без этого утверждения мутант `>` -> `>=`
        // выживает, а с ним валюта с максимальной точностью перестала бы
        // округляться вовсе — и правило НКД отказывало бы на ровном месте.
        let value = Dec::new(Decimal::from_str_exact("1.5").unwrap());
        assert!(value.checked_round_to_scale(Dec::max_scale()).is_ok());
    }
}
