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

/// Cash figures explaining why the broker payment overrides calculated fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CashReconciliation {
    pub payment: PostedMinor,
    pub trade_total: PostedMinor,
    pub delta: PostedMinor,
}

/// Posted fill grosses and, when needed, their cash-authoritative explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrossAllocation {
    pub amounts: Vec<PostedMinor>,
    pub reconciliation: Option<CashReconciliation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GrossAllocationError {
    #[error("accrued interest must be nonnegative and must not exceed payment")]
    InvalidAccrued,
    #[error("nonzero payment cannot be allocated across zero trade gross")]
    ZeroGross,
    #[error(transparent)]
    Money(#[from] crate::money::MoneyError),
    #[error(transparent)]
    Allocation(#[from] AllocationError),
}

/// Allocate the broker's posted payment less accrued interest by exact fill gross.
///
/// Prices remain calculated values: no per-unit or per-fill rounding supplies the
/// weights. The caller orders fills to break equal largest remainders stably.
pub fn allocate_gross_minor(
    payment: PostedMinor,
    accrued: PostedMinor,
    grosses: &[CalcMoney],
    currency: CurrencyCode,
) -> Result<GrossAllocation, GrossAllocationError> {
    if accrued.raw() < 0 || accrued.raw() > payment.raw() {
        return Err(GrossAllocationError::InvalidAccrued);
    }
    let total = payment
        .checked_sub(accrued)
        .ok_or(crate::money::MoneyError::Overflow)?;
    let accrued_money = CalcMoney::new(
        Dec::new(rust_decimal::Decimal::new(
            accrued.raw(),
            currency.minor_units(),
        )),
        currency,
    );
    let reconciliation = check_order_completeness(grosses, Some(accrued_money), payment, currency)?
        .map(|mismatch| {
            Ok::<_, crate::money::MoneyError>(CashReconciliation {
                payment: mismatch.reported,
                trade_total: mismatch.expected,
                delta: mismatch
                    .reported
                    .checked_sub(mismatch.expected)
                    .ok_or(crate::money::MoneyError::Overflow)?,
            })
        })
        .transpose()?;
    let weights = grosses
        .iter()
        .map(|gross| Quantity(Dec::new(gross.value().inner().abs())))
        .collect::<Vec<_>>();
    let amounts = if weights.iter().all(|weight| weight.0.is_zero()) {
        if total.raw() != 0 {
            return Err(GrossAllocationError::ZeroGross);
        }
        vec![PostedMinor::new(0); weights.len()]
    } else {
        allocate_minor(total, &weights)?
    };
    Ok(GrossAllocation {
        amounts,
        reconciliation,
    })
}

/// Failure while preserving reported fill commissions and allocating the remainder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CommissionAllocationError {
    #[error("commission grosses and reports must have equal lengths")]
    LengthMismatch,
    #[error("commission amounts must not be negative")]
    NegativeAmount,
    #[error("reported fill commissions do not reconcile with the order total")]
    ReportedTotalMismatch,
    #[error(transparent)]
    Allocation(#[from] AllocationError),
}

/// Preserve reported commissions and allocate only the unreported remainder.
///
/// Exact gross magnitudes supply the weights, without rounding prices or fills.
/// The caller sorts fills by trade identifier so equal largest remainders follow
/// that stable order. Reported commissions are authoritative, even when their
/// proportions differ from the gross weights.
pub fn allocate_commission_minor(
    total: PostedMinor,
    grosses: &[CalcMoney],
    reported: &[Option<PostedMinor>],
) -> Result<Vec<PostedMinor>, CommissionAllocationError> {
    if grosses.len() != reported.len() {
        return Err(CommissionAllocationError::LengthMismatch);
    }
    if total.raw() < 0 || reported.iter().flatten().any(|amount| amount.raw() < 0) {
        return Err(CommissionAllocationError::NegativeAmount);
    }
    let mut remaining = total;
    for amount in reported.iter().flatten() {
        if amount.raw() > remaining.raw() {
            return Err(CommissionAllocationError::ReportedTotalMismatch);
        }
        remaining = remaining
            .checked_sub(*amount)
            .ok_or(AllocationError::Overflow)?;
    }
    let weights = grosses
        .iter()
        .zip(reported)
        .filter(|(_, amount)| amount.is_none())
        .map(|(gross, _)| Quantity(Dec::new(gross.value().inner().abs())))
        .collect::<Vec<_>>();
    if weights.is_empty() && remaining.raw() != 0 {
        return Err(CommissionAllocationError::ReportedTotalMismatch);
    }
    let allocated = if remaining.raw() == 0 {
        vec![PostedMinor::new(0); weights.len()]
    } else {
        allocate_minor(remaining, &weights)?
    };
    let mut allocated = allocated.into_iter();
    Ok(reported
        .iter()
        .map(|amount| {
            amount.unwrap_or_else(|| allocated.next().expect("one share per missing fill"))
        })
        .collect())
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
    fn gross_allocation_preserves_cash_and_exact_price_weights() {
        let grosses = [
            CalcMoney::new(dec("0.001"), CurrencyCode::Rub),
            CalcMoney::new(dec("0.002"), CurrencyCode::Rub),
        ];
        let allocation = allocate_gross_minor(
            PostedMinor::new(13),
            PostedMinor::new(3),
            &grosses,
            CurrencyCode::Rub,
        )
        .unwrap();
        assert_eq!(
            allocation.amounts,
            vec![PostedMinor::new(3), PostedMinor::new(7)]
        );
        assert_eq!(
            allocation.reconciliation,
            Some(CashReconciliation {
                payment: PostedMinor::new(13),
                trade_total: PostedMinor::new(3),
                delta: PostedMinor::new(10),
            })
        );
    }

    #[test]
    fn gross_allocation_rejects_cash_without_weights_and_accrued_above_payment() {
        let zero = [CalcMoney::new(dec("0"), CurrencyCode::Rub)];
        assert_eq!(
            allocate_gross_minor(
                PostedMinor::new(1),
                PostedMinor::new(0),
                &zero,
                CurrencyCode::Rub
            ),
            Err(GrossAllocationError::ZeroGross),
        );
        assert_eq!(
            allocate_gross_minor(
                PostedMinor::new(1),
                PostedMinor::new(2),
                &zero,
                CurrencyCode::Rub
            ),
            Err(GrossAllocationError::InvalidAccrued),
        );
    }

    #[test]
    fn commission_preserves_reports_and_allocates_exact_gross_remainder() {
        let grosses = ["12.12345", "0.001", "0.002"]
            .map(|value| CalcMoney::new(dec(value), CurrencyCode::Rub));
        let result = allocate_commission_minor(
            PostedMinor::new(14),
            &grosses,
            &[Some(PostedMinor::new(4)), None, None],
        )
        .unwrap();
        assert_eq!(
            result,
            vec![
                PostedMinor::new(4),
                PostedMinor::new(3),
                PostedMinor::new(7)
            ]
        );
        assert_eq!(result.iter().map(|amount| amount.raw()).sum::<i64>(), 14);
    }

    #[test]
    fn commission_breaks_ties_in_missing_fill_order() {
        let grosses = [CalcMoney::new(dec("0.0001"), CurrencyCode::Rub); 3];
        assert_eq!(
            allocate_commission_minor(
                PostedMinor::new(6),
                &grosses,
                &[None, Some(PostedMinor::new(5)), None],
            )
            .unwrap(),
            vec![
                PostedMinor::new(1),
                PostedMinor::new(5),
                PostedMinor::new(0)
            ]
        );
    }

    #[test]
    fn commission_accepts_matching_reports_and_zero_without_weights() {
        let zero = [CalcMoney::new(dec("0"), CurrencyCode::Rub); 2];
        assert_eq!(
            allocate_commission_minor(
                PostedMinor::new(7),
                &zero,
                &[Some(PostedMinor::new(2)), Some(PostedMinor::new(5))],
            )
            .unwrap(),
            vec![PostedMinor::new(2), PostedMinor::new(5)]
        );
        assert_eq!(
            allocate_commission_minor(PostedMinor::new(0), &zero, &[None, None]).unwrap(),
            vec![PostedMinor::new(0); 2]
        );
        assert_eq!(
            allocate_commission_minor(PostedMinor::new(0), &[], &[]).unwrap(),
            vec![]
        );
    }

    #[test]
    fn commission_refuses_inconsistent_or_negative_reports() {
        let gross = [CalcMoney::new(dec("1"), CurrencyCode::Rub)];
        for (total, reports, expected) in [
            (1, vec![], CommissionAllocationError::LengthMismatch),
            (-1, vec![None], CommissionAllocationError::NegativeAmount),
            (
                1,
                vec![Some(PostedMinor::new(-1))],
                CommissionAllocationError::NegativeAmount,
            ),
            (
                1,
                vec![Some(PostedMinor::new(2))],
                CommissionAllocationError::ReportedTotalMismatch,
            ),
            (
                2,
                vec![Some(PostedMinor::new(1))],
                CommissionAllocationError::ReportedTotalMismatch,
            ),
        ] {
            assert_eq!(
                allocate_commission_minor(PostedMinor::new(total), &gross, &reports),
                Err(expected)
            );
        }
        assert_eq!(
            allocate_commission_minor(
                PostedMinor::new(1),
                &[CalcMoney::new(dec("0"), CurrencyCode::Rub)],
                &[None],
            ),
            Err(CommissionAllocationError::Allocation(
                AllocationError::ZeroQuantity
            ))
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
