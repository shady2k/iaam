//! Деньги и количества.
//!
//! Разделяются две категории величин (§3.4):
//! - **проведённые суммы** — целые в минимальных единицах, в опубликованной
//!   источником точности; это факты, их нельзя пересчитывать;
//! - **расчётные величины** — [`crate::numeric::decimal::Dec`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::numeric::exact::Exact;

/// Валюта. Исчерпаемый `enum`, а не строка (§15.1): добавление валюты
/// обязано сломать сборку везде, где её не обработали.
/// Атрибут `#[non_exhaustive]` намеренно **не** применяется: он запретил бы
/// исчерпывающий `match` внешним крейтам и тем самым отменил бы гарантию
/// «добавление валюты ломает сборку везде, где её не обработали» (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CurrencyCode {
    Rub,
    Usd,
    Eur,
    Cny,
    /// Золото в граммах — металлический счёт (§9.5).
    Xau,
}

impl CurrencyCode {
    /// Число знаков после запятой в минимальной единице.
    #[must_use]
    pub const fn minor_units(self) -> u32 {
        match self {
            Self::Rub | Self::Usd | Self::Eur | Self::Cny => 2,
            Self::Xau => 4,
        }
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Rub => "RUB",
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Cny => "CNY",
            Self::Xau => "XAU",
        }
    }
}

/// Проведённая сумма в минимальных единицах валюты.
///
/// Обёртка, а не голый `i64`: смешать её с количеством бумаг или
/// с расчётной величиной невозможно.
/// Поле **приватное**: публичное `pub i64` делало бы тривиальным обход
/// запрета на смешение валют — достаточно было бы сложить сырые `i64`.
/// Доступ к сырому значению даётся только точному арифметическому слою
/// и сериализации.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PostedMinor(i64);

impl PostedMinor {
    /// Тривиальная упаковка поля: логики, которую стоило бы вынести
    /// в отдельную функцию ради мутационного заслона, здесь нет.
    /// Слепота `cargo-mutants` к имени `new` тут ничего не скрывает.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Сырое значение. Предназначено для сериализации, форматирования
    /// и перевода в точный режим — не для арифметики над деньгами.
    #[must_use]
    pub const fn raw(self) -> i64 {
        self.0
    }

    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    #[must_use]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    #[must_use]
    pub fn checked_neg(self) -> Option<Self> {
        self.0.checked_neg().map(Self)
    }
}

/// Количество бумаг. Дробное — крипта и дробные остатки после сплитов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Quantity(pub Dec);

impl Quantity {
    #[must_use]
    pub fn zero() -> Self {
        Self(Dec::zero())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MoneyError {
    #[error("нельзя смешивать валюты: {left:?} и {right:?}")]
    CurrencyMismatch {
        left: CurrencyCode,
        right: CurrencyCode,
    },
    #[error("переполнение при сложении сумм")]
    Overflow,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Денежная сумма с валютой.
///
/// **Намеренно не реализует `std::ops::Add`.** Сложить можно только через
/// [`Money::try_add`], который обязывает обработать несовпадение валют.
/// Это компенсация за рантайм-тег валюты вместо фантомного типа —
/// обоснование в описании задачи 8 плана.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    amount: PostedMinor,
    currency: CurrencyCode,
}

impl Money {
    /// Тривиальная упаковка двух полей: проверять при сборке нечего,
    /// валюта и сумма независимы. Выносить в приватную функцию ради
    /// мутационного заслона нечего — тела, которое он мог бы проверить,
    /// не существует.
    #[must_use]
    pub const fn new(amount: PostedMinor, currency: CurrencyCode) -> Self {
        Self { amount, currency }
    }

    #[must_use]
    pub const fn zero(currency: CurrencyCode) -> Self {
        Self {
            amount: PostedMinor(0),
            currency,
        }
    }

    #[must_use]
    pub const fn amount(&self) -> PostedMinor {
        self.amount
    }

    #[must_use]
    pub const fn currency(&self) -> CurrencyCode {
        self.currency
    }

    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.amount.raw() == 0
    }

    fn require_same_currency(self, other: Self) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch {
                left: self.currency,
                right: other.currency,
            })
        }
    }

    pub fn try_add(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        let amount = self
            .amount
            .checked_add(other.amount)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self {
            amount,
            currency: self.currency,
        })
    }

    /// Вычитание через `checked_sub`, а **не** через отрицание:
    /// `-i64::MIN` не представим, поэтому реализация через `negate`
    /// делала бы метод паническим при обещанном в сигнатуре `Result`.
    pub fn try_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        let amount = self
            .amount
            .checked_sub(other.amount)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self {
            amount,
            currency: self.currency,
        })
    }

    pub fn checked_negate(self) -> Result<Self, MoneyError> {
        let amount = self.amount.checked_neg().ok_or(MoneyError::Overflow)?;
        Ok(Self {
            amount,
            currency: self.currency,
        })
    }

    /// Сумма списка. Валюта задаётся явно, чтобы пустой список давал
    /// осмысленный ноль, а не паниковал и не угадывал.
    pub fn sum(items: &[Self], currency: CurrencyCode) -> Result<Self, MoneyError> {
        items
            .iter()
            .try_fold(Self::zero(currency), |acc, item| acc.try_add(*item))
    }

    /// Переход в денежный режим: сумма как десятичная дробь.
    ///
    /// Единственная разрешённая точка перехода «проведённая сумма →
    /// расчётная величина» (§3.4). Обратного перехода нет намеренно:
    /// расчётная величина становится проведённой суммой только через
    /// факт источника, а не через округление.
    #[must_use]
    pub fn to_calc_dec(&self) -> Dec {
        Dec::new(Decimal::new(self.amount.raw(), self.currency.minor_units()))
    }

    /// Точное представление: `amount / 10^minor_units`.
    ///
    /// Отказать здесь нечему: `minor_units()` не превышает 4, поэтому
    /// `10^minor_units` не переполняет `i128` и всегда положителен,
    /// а числитель — образ `i64`. `Result` сохранён в сигнатуре ради
    /// устойчивости к появлению валюты с большей точностью.
    pub fn to_exact(&self) -> Result<Exact, MoneyError> {
        let den = 10_i128
            .checked_pow(self.currency.minor_units())
            .ok_or(NumericError::Overflow)?;
        Ok(Exact::new(i128::from(self.amount.raw()), den)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Валюта ---

    #[test]
    fn minor_units_follow_the_currency() {
        assert_eq!(CurrencyCode::Rub.minor_units(), 2);
        assert_eq!(CurrencyCode::Usd.minor_units(), 2);
        assert_eq!(CurrencyCode::Eur.minor_units(), 2);
        assert_eq!(CurrencyCode::Cny.minor_units(), 2);
        // Металлический счёт (§9.5) ведётся с большей точностью.
        assert_eq!(CurrencyCode::Xau.minor_units(), 4);
    }

    #[test]
    fn every_currency_reports_its_iso_code() {
        assert_eq!(CurrencyCode::Rub.code(), "RUB");
        assert_eq!(CurrencyCode::Usd.code(), "USD");
        assert_eq!(CurrencyCode::Eur.code(), "EUR");
        assert_eq!(CurrencyCode::Cny.code(), "CNY");
        assert_eq!(CurrencyCode::Xau.code(), "XAU");
    }

    // --- Проведённая сумма ---

    #[test]
    fn posted_minor_keeps_the_value_it_was_given() {
        assert_eq!(PostedMinor::new(35_050).raw(), 35_050);
        assert_eq!(PostedMinor::new(-7).raw(), -7);
    }

    #[test]
    fn posted_minor_arithmetic_is_checked() {
        let a = PostedMinor::new(10);
        let b = PostedMinor::new(4);
        assert_eq!(a.checked_add(b), Some(PostedMinor::new(14)));
        assert_eq!(a.checked_sub(b), Some(PostedMinor::new(6)));
        assert_eq!(a.checked_neg(), Some(PostedMinor::new(-10)));
    }

    #[test]
    fn posted_minor_reports_overflow_instead_of_wrapping() {
        assert_eq!(
            PostedMinor::new(i64::MAX).checked_add(PostedMinor::new(1)),
            None
        );
        assert_eq!(
            PostedMinor::new(i64::MIN).checked_sub(PostedMinor::new(1)),
            None
        );
        assert_eq!(PostedMinor::new(i64::MIN).checked_neg(), None);
    }

    // --- Количество ---

    #[test]
    fn quantity_zero_is_the_decimal_zero() {
        assert_eq!(Quantity::zero(), Quantity(Dec::zero()));
        assert!(Quantity::zero().0.to_exact().unwrap().is_zero());
    }

    // --- Деньги ---

    #[test]
    fn same_currency_adds() {
        let a = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let b = Money::new(PostedMinor::new(25_050), CurrencyCode::Rub);
        assert_eq!(
            a.try_add(b).unwrap(),
            Money::new(PostedMinor::new(35_050), CurrencyCode::Rub)
        );
    }

    #[test]
    fn different_currencies_refuse_to_add() {
        let rub = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let usd = Money::new(PostedMinor::new(10_000), CurrencyCode::Usd);
        assert!(matches!(
            rub.try_add(usd),
            Err(MoneyError::CurrencyMismatch {
                left: CurrencyCode::Rub,
                right: CurrencyCode::Usd
            })
        ));
    }

    #[test]
    fn addition_reports_overflow_instead_of_wrapping() {
        let max = Money::new(PostedMinor::new(i64::MAX), CurrencyCode::Rub);
        let one = Money::new(PostedMinor::new(1), CurrencyCode::Rub);
        assert!(matches!(max.try_add(one), Err(MoneyError::Overflow)));
    }

    #[test]
    fn subtraction_is_not_addition() {
        let a = Money::new(PostedMinor::new(1_000), CurrencyCode::Rub);
        let b = Money::new(PostedMinor::new(250), CurrencyCode::Rub);
        assert_eq!(
            a.try_sub(b).unwrap(),
            Money::new(PostedMinor::new(750), CurrencyCode::Rub)
        );
    }

    #[test]
    fn different_currencies_refuse_to_subtract() {
        let rub = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let eur = Money::new(PostedMinor::new(1), CurrencyCode::Eur);
        assert!(matches!(
            rub.try_sub(eur),
            Err(MoneyError::CurrencyMismatch {
                left: CurrencyCode::Rub,
                right: CurrencyCode::Eur
            })
        ));
    }

    #[test]
    fn subtracting_from_the_minimum_does_not_panic() {
        // `-i64::MIN` не представим. Реализация через отрицание сделала бы
        // try_sub паническим при обещанном Result.
        let min = Money::new(PostedMinor::new(i64::MIN), CurrencyCode::Rub);
        let one = Money::new(PostedMinor::new(1), CurrencyCode::Rub);
        assert!(matches!(min.try_sub(one), Err(MoneyError::Overflow)));
    }

    #[test]
    fn negate_flips_sign_and_keeps_currency() {
        let m = Money::new(PostedMinor::new(500), CurrencyCode::Rub);
        let n = m.checked_negate().unwrap();
        assert_eq!(n.amount().raw(), -500);
        assert_eq!(n.currency(), CurrencyCode::Rub);
    }

    #[test]
    fn negating_the_minimum_reports_overflow() {
        let min = Money::new(PostedMinor::new(i64::MIN), CurrencyCode::Rub);
        assert!(matches!(min.checked_negate(), Err(MoneyError::Overflow)));
    }

    #[test]
    fn accessors_expose_amount_and_currency() {
        let m = Money::new(PostedMinor::new(-42), CurrencyCode::Usd);
        assert_eq!(m.amount(), PostedMinor::new(-42));
        assert_eq!(m.currency(), CurrencyCode::Usd);
    }

    #[test]
    fn zero_carries_the_requested_currency() {
        let z = Money::zero(CurrencyCode::Cny);
        assert_eq!(z.amount().raw(), 0);
        assert_eq!(z.currency(), CurrencyCode::Cny);
    }

    #[test]
    fn is_zero_holds_only_for_the_zero_amount() {
        assert!(Money::zero(CurrencyCode::Eur).is_zero());
        assert!(!Money::new(PostedMinor::new(1), CurrencyCode::Eur).is_zero());
        assert!(!Money::new(PostedMinor::new(-1), CurrencyCode::Eur).is_zero());
    }

    // --- Суммирование списка ---

    #[test]
    fn sum_of_empty_is_zero_in_requested_currency() {
        let z = Money::sum(&[], CurrencyCode::Rub).unwrap();
        assert_eq!(z, Money::zero(CurrencyCode::Rub));
    }

    #[test]
    fn sum_accumulates_every_item() {
        // Слагаемые различны и знакопеременны: потеря любого из них
        // или остановка после первого меняет результат.
        let items = [
            Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
            Money::new(PostedMinor::new(250), CurrencyCode::Rub),
            Money::new(PostedMinor::new(-325), CurrencyCode::Rub),
        ];
        assert_eq!(
            Money::sum(&items, CurrencyCode::Rub).unwrap(),
            Money::new(PostedMinor::new(925), CurrencyCode::Rub)
        );
    }

    #[test]
    fn sum_rejects_mixed_currencies() {
        let items = [
            Money::new(PostedMinor::new(1), CurrencyCode::Rub),
            Money::new(PostedMinor::new(1), CurrencyCode::Usd),
        ];
        assert!(Money::sum(&items, CurrencyCode::Rub).is_err());
    }

    #[test]
    fn sum_rejects_items_of_a_currency_other_than_requested() {
        // Валюта задана явно: однородный список в другой валюте — тоже ошибка,
        // а не «вывели валюту из первого элемента».
        let items = [Money::new(PostedMinor::new(1), CurrencyCode::Usd)];
        assert!(matches!(
            Money::sum(&items, CurrencyCode::Rub),
            Err(MoneyError::CurrencyMismatch {
                left: CurrencyCode::Rub,
                right: CurrencyCode::Usd
            })
        ));
    }

    #[test]
    fn sum_reports_overflow_instead_of_wrapping() {
        let items = [
            Money::new(PostedMinor::new(i64::MAX), CurrencyCode::Rub),
            Money::new(PostedMinor::new(1), CurrencyCode::Rub),
        ];
        assert!(matches!(
            Money::sum(&items, CurrencyCode::Rub),
            Err(MoneyError::Overflow)
        ));
    }

    // --- Перевод в точный режим ---

    #[test]
    fn to_exact_is_scaled_by_minor_units() {
        // 350,50 ₽ == 35050/100
        let m = Money::new(PostedMinor::new(35_050), CurrencyCode::Rub);
        let e = m.to_exact().unwrap();
        assert_eq!(e, crate::numeric::exact::Exact::new(35_050, 100).unwrap());
    }

    #[test]
    fn to_exact_uses_the_scale_of_its_own_currency() {
        // У XAU четыре знака: 12345 минимальных единиц — это 1,2345 г.
        let m = Money::new(PostedMinor::new(12_345), CurrencyCode::Xau);
        assert_eq!(m.to_exact().unwrap(), Exact::new(12_345, 10_000).unwrap());
    }

    #[test]
    fn to_exact_keeps_the_sign() {
        let m = Money::new(PostedMinor::new(-1), CurrencyCode::Usd);
        assert_eq!(m.to_exact().unwrap(), Exact::new(-1, 100).unwrap());
    }

    #[test]
    fn to_exact_survives_the_most_extreme_posted_amount() {
        // i64::MIN = -2^63, минимальная единица XAU = 10^-4.
        // -2^63 / 10^4 в несократимом виде: НОД(2^63, 10^4) = 2^4 = 16,
        // откуда -576460752303423488/625. Знаменатель 10^4 не переполняет
        // i128, а числитель — образ i64, поэтому Exact::new здесь
        // не может отказать.
        let m = Money::new(PostedMinor::new(i64::MIN), CurrencyCode::Xau);
        let e = m.to_exact().unwrap();
        assert_eq!(e.numerator(), -576_460_752_303_423_488);
        assert_eq!(e.denominator(), 625);
    }
}
