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

    /// Сложение с проверкой переполнения. Штатный `+` у `Decimal` паникует
    /// при выходе за диапазон; тихая паника в расчёте доходности хуже
    /// типизированного отказа.
    pub fn checked_add(self, other: Self) -> Result<Self, NumericError> {
        self.0
            .checked_add(other.0)
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
}
