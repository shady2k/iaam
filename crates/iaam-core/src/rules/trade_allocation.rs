//! Trade-order arithmetic that must be evaluated by the core.

use thiserror::Error;

use crate::money::{CalcMoney, CurrencyCode, PostedMinor, Quantity};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// The two posted magnitudes used to explain an order-completeness mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderMoneyMismatch {
    pub reported: PostedMinor,
    pub expected: PostedMinor,
}

/// Compare an order's reported payment with its exact trade total.
///
/// Grosses and accrued interest are compared as magnitudes. Rounding happens
/// once, after the order total has been assembled, so a sub-minor exact value
/// cannot alter the source-fact rounding path.
pub fn check_order_completeness(
    grosses: &[CalcMoney],
    accrued_interest: Option<CalcMoney>,
    reported_payment: PostedMinor,
    currency: CurrencyCode,
) -> Result<Option<OrderMoneyMismatch>, crate::money::MoneyError> {
    let gross_total =
        grosses
            .iter()
            .try_fold(CalcMoney::new(Dec::zero(), currency), |total, gross| {
                total.checked_add(CalcMoney::new(
                    Dec::new(gross.value().inner().abs()),
                    gross.currency(),
                ))
            })?;
    let accrued = accrued_interest
        .map(|value| CalcMoney::new(Dec::new(value.value().inner().abs()), value.currency()))
        .unwrap_or_else(|| CalcMoney::new(Dec::zero(), currency));
    let expected = gross_total.checked_add(accrued)?.rounded_minor()?;
    if expected == reported_payment {
        Ok(None)
    } else {
        Ok(Some(OrderMoneyMismatch {
            reported: reported_payment,
            expected,
        }))
    }
}

/// Failure while distributing a posted order amount across fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AllocationError {
    #[error("minor allocation total must not be negative")]
    NegativeTotal,
    #[error("cannot allocate minor units across zero quantity")]
    ZeroQuantity,
    #[error("minor allocation floor is not representable")]
    FloorOverflow,
    #[error("minor allocation total overflow")]
    TotalOverflow,
    #[error("minor allocation remainder overflow")]
    RemainderOverflow,
    #[error("minor allocation overflow")]
    Overflow,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Allocate a posted amount by quantity with largest remainders.
///
/// The caller supplies its deterministic order. Equal fractional remainders
/// therefore follow the caller's order, without making the core depend on a
/// channel's trade identifier or response type.
pub fn allocate_minor(
    total: PostedMinor,
    weights: &[Quantity],
) -> Result<Vec<PostedMinor>, AllocationError> {
    if total.raw() < 0 {
        return Err(AllocationError::NegativeTotal);
    }
    let quantity = crate::money::sum_quantities(weights)?;
    if quantity.0.is_zero() {
        return Err(AllocationError::ZeroQuantity);
    }

    let total_exact = Dec::new(rust_decimal::Decimal::from(total.raw()));
    let mut parts = Vec::with_capacity(weights.len());
    for weight in weights {
        let exact = total_exact.checked_mul(weight.0)?.checked_div(quantity.0)?;
        let floor = exact.inner().trunc();
        let base = i64::try_from(floor.mantissa()).map_err(|_| AllocationError::FloorOverflow)?;
        let remainder = exact
            .checked_sub(Dec::new(floor))
            .map_err(|_| AllocationError::RemainderOverflow)?;
        parts.push((base, remainder));
    }

    let allocated = parts.iter().try_fold(0_i64, |sum, (base, _)| {
        sum.checked_add(*base).ok_or(AllocationError::TotalOverflow)
    })?;
    let remaining = i128::from(total.raw())
        .checked_sub(i128::from(allocated))
        .ok_or(AllocationError::RemainderOverflow)?;
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|left, right| {
        parts[*right]
            .1
            .cmp(&parts[*left].1)
            .then_with(|| left.cmp(right))
    });
    let mut result = parts.into_iter().map(|(base, _)| base).collect::<Vec<_>>();
    for index in order
        .into_iter()
        .take(usize::try_from(remaining).unwrap_or(0))
    {
        result[index] = result[index]
            .checked_add(1)
            .ok_or(AllocationError::Overflow)?;
    }
    Ok(result.into_iter().map(PostedMinor::new).collect())
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    #[test]
    fn completeness_rounds_the_total_once_and_matches() {
        let grosses = [CalcMoney::new(dec("10.125"), CurrencyCode::Rub)];
        let accrued = Some(CalcMoney::new(dec("0.004"), CurrencyCode::Rub));
        assert_eq!(
            check_order_completeness(
                &grosses,
                accrued,
                PostedMinor::new(1_013),
                CurrencyCode::Rub,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn completeness_returns_both_posted_amounts_on_mismatch() {
        let grosses = [CalcMoney::new(dec("10"), CurrencyCode::Rub)];
        let mismatch =
            check_order_completeness(&grosses, None, PostedMinor::new(999), CurrencyCode::Rub)
                .unwrap();
        assert_eq!(
            mismatch,
            Some(OrderMoneyMismatch {
                reported: PostedMinor::new(999),
                expected: PostedMinor::new(1_000),
            })
        );
    }

    #[test]
    fn completeness_compares_magnitudes() {
        let grosses = [CalcMoney::new(dec("-10"), CurrencyCode::Rub)];
        let accrued = Some(CalcMoney::new(dec("-1"), CurrencyCode::Rub));
        assert_eq!(
            check_order_completeness(
                &grosses,
                accrued,
                PostedMinor::new(1_100),
                CurrencyCode::Rub,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn completeness_refuses_exact_decimal_overflow() {
        let grosses = [
            CalcMoney::new(Dec::new(Decimal::MAX), CurrencyCode::Rub),
            CalcMoney::new(Dec::new(Decimal::MAX), CurrencyCode::Rub),
        ];
        assert_eq!(
            check_order_completeness(&grosses, None, PostedMinor::new(0), CurrencyCode::Rub,),
            Err(crate::money::MoneyError::Numeric(NumericError::Overflow))
        );
    }

    #[test]
    fn allocation_gives_remainder_to_largest_fraction() {
        let weights = [Quantity(dec("1")), Quantity(dec("2")), Quantity(dec("3"))];
        assert_eq!(
            allocate_minor(PostedMinor::new(10), &weights).unwrap(),
            vec![
                PostedMinor::new(2),
                PostedMinor::new(3),
                PostedMinor::new(5)
            ]
        );
    }

    #[test]
    fn allocation_handles_fewer_units_than_weights() {
        let weights = [Quantity(dec("1")), Quantity(dec("1")), Quantity(dec("1"))];
        assert_eq!(
            allocate_minor(PostedMinor::new(2), &weights).unwrap(),
            vec![
                PostedMinor::new(1),
                PostedMinor::new(1),
                PostedMinor::new(0)
            ]
        );
    }

    #[test]
    fn allocation_refuses_quantity_sum_overflow() {
        let weights = [
            Quantity(Dec::new(Decimal::MAX)),
            Quantity(Dec::new(Decimal::MAX)),
        ];
        assert_eq!(
            allocate_minor(PostedMinor::new(1), &weights),
            Err(AllocationError::Numeric(NumericError::Overflow))
        );
    }
}
