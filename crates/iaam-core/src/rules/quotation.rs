//! Пересчёт котировки в деньги за бумагу (§10.2).
//!
//! Отдельное версионированное правило, а не арифметика на месте:
//! умножение количества на цену живёт в двух местах —
//! `returns::position_value` и `returns::xirr::account_values`, — и два
//! независимых пересчёта неизбежно разъедутся (§10.4).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::money::{CurrencyCode, PerUnitAmount};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::valuation::QuotationBasis;

/// Версия правила пересчёта котировки в деньги.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct QuotationRuleVersion(pub u32);

/// Причина, по которой котировку нельзя пересчитать в деньги.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QuotationError {
    #[error("основание котировки неизвестно")]
    BasisUnknown,
    #[error("непогашенный номинал неизвестен")]
    PrincipalUnknown,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Пересчёт цены источника в деньги за одну бумагу.
pub trait QuotationRule: Send + Sync + std::fmt::Debug {
    /// Деньги за одну бумагу и валюта этих денег.
    fn money_per_unit(
        &self,
        basis: QuotationBasis,
        price: Dec,
        venue_currency: CurrencyCode,
        remaining_face: Option<PerUnitAmount>,
    ) -> Result<(Dec, CurrencyCode), QuotationError>;
}

/// Первая версия правила пересчёта котировки.
#[derive(Debug, Default)]
pub struct QuotationV1;

impl QuotationRule for QuotationV1 {
    fn money_per_unit(
        &self,
        basis: QuotationBasis,
        price: Dec,
        venue_currency: CurrencyCode,
        remaining_face: Option<PerUnitAmount>,
    ) -> Result<(Dec, CurrencyCode), QuotationError> {
        match basis {
            // Валюта наблюдения остаётся валютой числа, как и было.
            QuotationBasis::MoneyPerUnit => Ok((price, venue_currency)),
            QuotationBasis::PercentOfRemainingFace => {
                let face = remaining_face.ok_or(QuotationError::PrincipalUnknown)?;
                let fraction = price.checked_div(Dec::new(Decimal::ONE_HUNDRED))?;
                let money = fraction.checked_mul(face.value())?;
                // Денежная валюта приходит из номинала: само число
                // безразмерно, и валюта площадки к нему не относится.
                Ok((money, face.currency()))
            }
            // Наблюдение, происхождение которого не доказано, оценке
            // не подлежит: догадка занизила бы облигацию молча.
            QuotationBasis::Unknown => Err(QuotationError::BasisUnknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::CurrencyCode;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn per_unit(text: &str, currency: CurrencyCode) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), currency)
    }

    #[test]
    fn money_per_unit_passes_the_price_and_its_currency_through() {
        assert_eq!(
            QuotationV1
                .money_per_unit(
                    QuotationBasis::MoneyPerUnit,
                    dec("270.13"),
                    CurrencyCode::Rub,
                    None,
                )
                .unwrap(),
            (dec("270.13"), CurrencyCode::Rub)
        );
    }

    #[test]
    fn a_percent_quote_becomes_money_through_the_remaining_face() {
        // 98.5% от непогашенного номинала 1000 ₽ — это 985 ₽, а не 98.5 ₽.
        assert_eq!(
            QuotationV1
                .money_per_unit(
                    QuotationBasis::PercentOfRemainingFace,
                    dec("98.5"),
                    CurrencyCode::Rub,
                    Some(per_unit("1000", CurrencyCode::Rub)),
                )
                .unwrap(),
            (dec("985.000"), CurrencyCode::Rub)
        );
    }

    #[test]
    fn a_percent_quote_takes_its_currency_from_the_face_not_from_the_venue() {
        // Число 98.5 безразмерно: валютой результата становится валюта номинала.
        let (_, currency) = QuotationV1
            .money_per_unit(
                QuotationBasis::PercentOfRemainingFace,
                dec("98.5"),
                CurrencyCode::Rub,
                Some(per_unit("1000", CurrencyCode::Usd)),
            )
            .unwrap();
        assert_eq!(currency, CurrencyCode::Usd);
    }

    #[test]
    fn a_percent_quote_without_a_known_face_refuses_instead_of_guessing() {
        assert_eq!(
            QuotationV1
                .money_per_unit(
                    QuotationBasis::PercentOfRemainingFace,
                    dec("98.5"),
                    CurrencyCode::Rub,
                    None,
                )
                .unwrap_err(),
            QuotationError::PrincipalUnknown
        );
    }

    #[test]
    fn an_undecided_basis_refuses_rather_than_assuming_money() {
        assert_eq!(
            QuotationV1
                .money_per_unit(
                    QuotationBasis::Unknown,
                    dec("98.5"),
                    CurrencyCode::Rub,
                    None,
                )
                .unwrap_err(),
            QuotationError::BasisUnknown
        );
    }

    #[test]
    fn the_registry_resolves_the_default_quotation_rule() {
        let registry = RuleRegistry::with_defaults();
        assert_eq!(
            registry.latest_quotation_version(),
            Some(QuotationRuleVersion(1))
        );
        assert!(registry.quotation_rule(QuotationRuleVersion(1)).is_some());
        assert!(registry.quotation_rule(QuotationRuleVersion(999)).is_none());
    }
}
