//! Money and quantities.
//!
//! Two categories of values are kept separate (§3.4):
//! - **posted amounts**—integers in minor units, at the precision published by
//!   the source; these are facts and must not be recalculated;
//! - **calculated values**—[`crate::numeric::decimal::Dec`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::numeric::exact::Exact;

/// Currency. An exhaustive `enum`, not a string (§15.1): adding a currency
/// must break compilation everywhere it is not handled.
/// `#[non_exhaustive]` is deliberately **not** used: it would prevent an
/// exhaustive `match` in external crates and cancel that guarantee (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CurrencyCode {
    Rub,
    Usd,
    Eur,
    Cny,
    /// Gold in grams—a metal account (§9.5).
    Xau,
}

impl CurrencyCode {
    /// Number of decimal places in the minor unit.
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

    /// Parse an ISO code without choosing a default currency.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        [Self::Rub, Self::Usd, Self::Eur, Self::Cny, Self::Xau]
            .into_iter()
            .find(|currency| currency.code() == code)
    }
}

/// Posted amount in the currency's minor units.
///
/// A wrapper, not a bare `i64`: it cannot be mixed with a security quantity or
/// a calculated value.
/// The field is **private**: a public `pub i64` would make the currency-mixing
/// prohibition trivial to bypass by adding raw `i64` values.
/// Raw access is available only to exact arithmetic and serialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PostedMinor(i64);

impl PostedMinor {
    /// Trivial field packing: there is no logic here worth extracting into a
    /// separate function for the mutation guard.
    /// `cargo-mutants`' blindness to the name `new` hides nothing here.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Raw value. Intended for serialisation, formatting, and conversion to
    /// exact mode—not for arithmetic on money.
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

/// Security quantity. Fractional values support crypto and fractional
/// remainders after splits.
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
    #[error("currencies cannot be mixed: {left:?} and {right:?}")]
    CurrencyMismatch {
        left: CurrencyCode,
        right: CurrencyCode,
    },
    #[error("amount addition overflow")]
    Overflow,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Monetary amount with a currency.
///
/// **Deliberately does not implement `std::ops::Add`.** Addition is available
/// only through [`Money::try_add`], which requires handling a currency mismatch.
/// This compensates for a runtime currency tag instead of a phantom type—the
/// rationale is in task 8 of the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    amount: PostedMinor,
    currency: CurrencyCode,
}

impl Money {
    /// Trivial packing of two fields: the amount and currency are independent.
    /// There is no body to extract for the mutation guard.
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

    /// Subtract through `checked_sub`, **not** negation:
    /// `-i64::MIN` is unrepresentable, so implementing this through `negate`
    /// would make a method panic despite its `Result` contract.
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

    /// Sum a list. The currency is explicit so an empty list has a meaningful
    /// zero rather than panicking or guessing.
    pub fn sum(items: &[Self], currency: CurrencyCode) -> Result<Self, MoneyError> {
        items
            .iter()
            .try_fold(Self::zero(currency), |acc, item| acc.try_add(*item))
    }

    /// Convert to calculated money: represent the amount as a decimal.
    ///
    /// This is the only permitted transition from a “posted amount” to a
    /// “calculated value” (§3.4). There is deliberately no reverse transition:
    /// a calculated value becomes a posted amount only through a source fact,
    /// not by rounding.
    #[must_use]
    pub fn to_calc_dec(&self) -> Dec {
        Dec::new(Decimal::new(self.amount.raw(), self.currency.minor_units()))
    }

    /// Exact representation: `amount / 10^minor_units`.
    ///
    /// Nothing can fail here: `minor_units()` is at most 4, so
    /// `10^minor_units` cannot overflow `i128` and is always positive, while
    /// the numerator is an `i64` value. `Result` remains in the signature to
    /// stay robust if a higher-precision currency is added.
    pub fn to_exact(&self) -> Result<Exact, MoneyError> {
        let den = 10_i128
            .checked_pow(self.currency.minor_units())
            .ok_or(NumericError::Overflow)?;
        Ok(Exact::new(i128::from(self.amount.raw()), den)?)
    }
}

/// Calculated monetary value with a currency.
///
/// Unlike [`Money`], which stores a posted amount in integer minor units,
/// `CalcMoney` stores the exact [`Dec`] value calculated from issue terms or a
/// schedule. Fractions of a minor unit are therefore not rounded during
/// calculation.
///
/// There is deliberately no transition back to [`Money`]: a calculated value
/// becomes a posted amount only through a confirmed source fact, not by
/// rounding the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalcMoney {
    value: Dec,
    currency: CurrencyCode,
}

impl CalcMoney {
    /// Trivial packing of two independent fields; there is nothing to validate
    /// during construction, just as with [`Money::new`].
    #[must_use]
    pub const fn new(value: Dec, currency: CurrencyCode) -> Self {
        Self { value, currency }
    }

    #[must_use]
    pub const fn value(&self) -> Dec {
        self.value
    }

    #[must_use]
    pub const fn currency(&self) -> CurrencyCode {
        self.currency
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

    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        Ok(Self::new(
            self.value.checked_add(other.value)?,
            self.currency,
        ))
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        Ok(Self::new(
            self.value.checked_sub(other.value)?,
            self.currency,
        ))
    }

    pub fn checked_mul(self, factor: Dec) -> Result<Self, MoneyError> {
        Ok(Self::new(self.value.checked_mul(factor)?, self.currency))
    }
}

/// Calculated **per-unit** monetary value, not a posted amount.
///
/// Issue face value and coupon per security are contractual, not debits from
/// the account: [`Money`] stores minor units, so face value 333.3333 would lose
/// two decimal places. A separate type prevents adding a calculated value to a
/// posted amount—under §3.4 they are different things.
///
/// It intentionally has no internal currency invariant: it represents one
/// value, so there is nothing to mix. Currency reconciliation belongs where two
/// values meet—in lot principal state and in the amortisation rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerUnitAmount {
    value: Dec,
    currency: CurrencyCode,
}

impl PerUnitAmount {
    /// Trivial packing of two independent fields; there is nothing to validate
    /// during construction, just as with [`Money::new`].
    #[must_use]
    pub const fn new(value: Dec, currency: CurrencyCode) -> Self {
        Self { value, currency }
    }

    #[must_use]
    pub const fn value(&self) -> Dec {
        self.value
    }

    #[must_use]
    pub const fn currency(&self) -> CurrencyCode {
        self.currency
    }

    /// Total for the position. Returns [`Dec`], not [`Money`]: the result
    /// remains calculated until posted to the account (§3.4).
    pub fn checked_mul_quantity(&self, quantity: Quantity) -> Result<Dec, NumericError> {
        self.value.checked_mul(quantity.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    // --- Calculated monetary value ---

    #[test]
    fn calc_money_keeps_value_and_currency() {
        let amount = CalcMoney::new(dec("333.333"), CurrencyCode::Rub);

        assert_eq!(amount.value(), dec("333.333"));
        assert_eq!(amount.currency(), CurrencyCode::Rub);
    }

    #[test]
    fn calc_money_arithmetic_is_checked() {
        let amount = CalcMoney::new(dec("10.25"), CurrencyCode::Rub);
        let other = CalcMoney::new(dec("2.75"), CurrencyCode::Rub);

        assert_eq!(
            amount.checked_add(other).unwrap(),
            CalcMoney::new(dec("13.00"), CurrencyCode::Rub)
        );
        assert_eq!(
            amount.checked_sub(other).unwrap(),
            CalcMoney::new(dec("7.50"), CurrencyCode::Rub)
        );
        assert_eq!(
            amount.checked_mul(dec("3")).unwrap(),
            CalcMoney::new(dec("30.75"), CurrencyCode::Rub)
        );
    }

    #[test]
    fn calc_money_rejects_different_currencies() {
        let rubles = CalcMoney::new(dec("10"), CurrencyCode::Rub);
        let dollars = CalcMoney::new(dec("10"), CurrencyCode::Usd);

        assert!(matches!(
            rubles.checked_add(dollars),
            Err(MoneyError::CurrencyMismatch {
                left: CurrencyCode::Rub,
                right: CurrencyCode::Usd
            })
        ));
        assert!(matches!(
            rubles.checked_sub(dollars),
            Err(MoneyError::CurrencyMismatch {
                left: CurrencyCode::Rub,
                right: CurrencyCode::Usd
            })
        ));
    }

    #[test]
    fn calc_money_multiplication_reports_numeric_overflow() {
        let amount = CalcMoney::new(dec("79228162514264337593543950335"), CurrencyCode::Rub);

        assert!(matches!(
            amount.checked_mul(dec("2")),
            Err(MoneyError::Numeric(NumericError::Overflow))
        ));
    }

    // --- Per-unit value ---

    #[test]
    fn per_unit_amount_multiplied_by_quantity_stays_a_calculated_value() {
        let nominal = PerUnitAmount::new(dec("1000.0000"), CurrencyCode::Rub);
        assert_eq!(
            nominal.checked_mul_quantity(Quantity(dec("3"))).unwrap(),
            dec("3000.0000")
        );
    }

    #[test]
    fn per_unit_amount_keeps_precision_finer_than_a_minor_unit() {
        // The rouble minor unit is a kopeck; face value 333.3333 in `Money`
        // would lose two decimal places (§3.4).
        assert_eq!(
            PerUnitAmount::new(dec("333.3333"), CurrencyCode::Rub).value(),
            dec("333.3333")
        );
    }

    #[test]
    fn per_unit_amount_carries_its_own_currency() {
        assert_eq!(
            PerUnitAmount::new(dec("1000"), CurrencyCode::Usd).currency(),
            CurrencyCode::Usd
        );
    }

    #[test]
    fn per_unit_amount_survives_a_json_round_trip() {
        let nominal = PerUnitAmount::new(dec("1000.0000"), CurrencyCode::Rub);
        let text = serde_json::to_string(&nominal).unwrap();
        assert_eq!(
            serde_json::from_str::<PerUnitAmount>(&text).unwrap(),
            nominal
        );
    }

    // --- Currency ---

    #[test]
    fn minor_units_follow_the_currency() {
        assert_eq!(CurrencyCode::Rub.minor_units(), 2);
        assert_eq!(CurrencyCode::Usd.minor_units(), 2);
        assert_eq!(CurrencyCode::Eur.minor_units(), 2);
        assert_eq!(CurrencyCode::Cny.minor_units(), 2);
        // The metal account (§9.5) uses finer precision.
        assert_eq!(CurrencyCode::Xau.minor_units(), 4);
    }

    #[test]
    fn every_currency_survives_a_round_trip_through_its_iso_code() {
        for currency in [
            CurrencyCode::Rub,
            CurrencyCode::Usd,
            CurrencyCode::Eur,
            CurrencyCode::Cny,
            CurrencyCode::Xau,
        ] {
            assert_eq!(CurrencyCode::from_code(currency.code()), Some(currency));
        }
    }
    #[test]
    fn every_currency_reports_its_iso_code() {
        assert_eq!(CurrencyCode::Rub.code(), "RUB");
        assert_eq!(CurrencyCode::Usd.code(), "USD");
        assert_eq!(CurrencyCode::Eur.code(), "EUR");
        assert_eq!(CurrencyCode::Cny.code(), "CNY");
        assert_eq!(CurrencyCode::Xau.code(), "XAU");
    }

    // --- Posted amount ---

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

    // --- Quantity ---

    #[test]
    fn quantity_zero_is_the_decimal_zero() {
        assert_eq!(Quantity::zero(), Quantity(Dec::zero()));
        assert!(Quantity::zero().0.to_exact().unwrap().is_zero());
    }

    // --- Money ---

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
        // `-i64::MIN` is unrepresentable. Implementing subtraction through
        // negation would make try_sub panic despite its Result contract.
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

    // --- List summation ---

    #[test]
    fn sum_of_empty_is_zero_in_requested_currency() {
        let z = Money::sum(&[], CurrencyCode::Rub).unwrap();
        assert_eq!(z, Money::zero(CurrencyCode::Rub));
    }

    #[test]
    fn sum_accumulates_every_item() {
        // Terms are distinct and alternate in sign: dropping any term or
        // stopping after the first changes the result.
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
        // The currency is explicit: a uniform list in another currency is also
        // an error, rather than deriving the currency from the first item.
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

    // --- Conversion to exact mode ---

    #[test]
    fn to_exact_is_scaled_by_minor_units() {
        // 350.50 ₽ == 35050/100
        let m = Money::new(PostedMinor::new(35_050), CurrencyCode::Rub);
        let e = m.to_exact().unwrap();
        assert_eq!(e, crate::numeric::exact::Exact::new(35_050, 100).unwrap());
    }

    #[test]
    fn to_exact_uses_the_scale_of_its_own_currency() {
        // XAU has four decimal places: 12345 minor units equal 1.2345 g.
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
        // i64::MIN = -2^63; the XAU minor unit is 10^-4.
        // In reduced form, -2^63 / 10^4 = -576460752303423488/625 because
        // gcd(2^63, 10^4) = 2^4 = 16. The denominator fits in i128 and the
        // numerator is an i64 value, so Exact::new cannot fail here.
        let m = Money::new(PostedMinor::new(i64::MIN), CurrencyCode::Xau);
        let e = m.to_exact().unwrap();
        assert_eq!(e.numerator(), -576_460_752_303_423_488);
        assert_eq!(e.denominator(), 625);
    }
}
