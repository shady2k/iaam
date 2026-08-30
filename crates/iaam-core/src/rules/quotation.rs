//! Converting a quotation into money per security (§10.2).
//!
//! A separate versioned rule, not inline arithmetic:
//! multiplying quantity by price lives in two places —
//! `returns::position_value` and `returns::xirr::account_values` — and two
//! independent recalculations inevitably drift (§10.4).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::money::{CurrencyCode, PerUnitAmount};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::valuation::QuotationBasis;

/// Quotation-to-money rule version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct QuotationRuleVersion(pub u32);

/// Why a quotation cannot be converted into money.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QuotationError {
    #[error("quotation basis is unknown")]
    BasisUnknown,
    #[error("outstanding principal is unknown")]
    PrincipalUnknown,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Convert the source price into money per security.
pub trait QuotationRule: Send + Sync + std::fmt::Debug {
    /// Money per security and its currency.
    fn money_per_unit(
        &self,
        basis: QuotationBasis,
        price: Dec,
        venue_currency: CurrencyCode,
        remaining_face: Option<PerUnitAmount>,
    ) -> Result<(Dec, CurrencyCode), QuotationError>;
}

/// First version of the quotation conversion rule.
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
            // The observation's currency remains the number's currency.
            QuotationBasis::MoneyPerUnit => Ok((price, venue_currency)),
            QuotationBasis::PercentOfRemainingFace => {
                let face = remaining_face.ok_or(QuotationError::PrincipalUnknown)?;
                let fraction = price.checked_div(Dec::new(Decimal::ONE_HUNDRED))?;
                let money = fraction.checked_mul(face.value())?;
                // The money currency comes from principal: the number is
                // dimensionless, so the venue currency does not apply.
                Ok((money, face.currency()))
            }
            // An observation whose provenance is unproven cannot be valued:
            // a guess would silently understate the bond.
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
        // 98.5% of 1,000 ₽ outstanding principal is 985 ₽, not 98.5 ₽.
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
        // The number 98.5 is dimensionless: the result takes the principal's currency.
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
