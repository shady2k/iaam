//! Checked arithmetic for diagnostic commission comparisons, not an allocation rule.
//!
//! Displayed allocations may be rounded by Decimal division. Exact comparisons
//! use cross-products instead and never infer a posting or change a fact.

use rust_decimal::Decimal;

/// Convert the broker's signed units/nano representation without rounding.
#[must_use]
pub fn money_decimal(units: i64, nano: i64) -> Option<Decimal> {
    if !(-999_999_999..=999_999_999).contains(&nano)
        || (units > 0 && nano < 0)
        || (units < 0 && nano > 0)
    {
        return None;
    }
    Decimal::from(units).checked_add(Decimal::new(nano, 9))
}

/// Total positive quantity and quantity-times-price weights for diagnostic fills.
#[must_use]
pub fn weights(fills: &[(Decimal, Decimal)]) -> Option<(Decimal, Decimal)> {
    if fills.is_empty() {
        return None;
    }
    let mut total_quantity = Decimal::ZERO;
    let mut total_value = Decimal::ZERO;
    for &(quantity, price) in fills {
        if quantity <= Decimal::ZERO || price <= Decimal::ZERO {
            return None;
        }
        total_quantity = total_quantity.checked_add(quantity)?;
        total_value = total_value.checked_add(quantity.checked_mul(price)?)?;
    }
    Some((total_quantity, total_value))
}

/// Compare both weighting hypotheses for one fill without rounded division.
#[must_use]
pub fn exact_matches(
    commission: Decimal,
    order_commission: Decimal,
    quantity: Decimal,
    price: Decimal,
    total_quantity: Decimal,
    total_value: Decimal,
) -> Option<(bool, bool)> {
    if quantity <= Decimal::ZERO
        || price <= Decimal::ZERO
        || total_quantity <= Decimal::ZERO
        || total_value <= Decimal::ZERO
    {
        return None;
    }
    let quantity_matches =
        commission.checked_mul(total_quantity)? == order_commission.checked_mul(quantity)?;
    let value_matches = commission.checked_mul(total_value)?
        == order_commission.checked_mul(quantity.checked_mul(price)?)?;
    Some((quantity_matches, value_matches))
}

/// Display-only allocations; a repeating quotient is a Decimal approximation.
#[must_use]
pub fn allocations(
    order_commission: Decimal,
    quantity: Decimal,
    price: Decimal,
    total_quantity: Decimal,
    total_value: Decimal,
) -> (Option<Decimal>, Option<Decimal>) {
    if quantity <= Decimal::ZERO
        || price <= Decimal::ZERO
        || total_quantity <= Decimal::ZERO
        || total_value <= Decimal::ZERO
    {
        return (None, None);
    }
    let by_quantity = order_commission
        .checked_mul(quantity)
        .and_then(|amount| amount.checked_div(total_quantity));
    let by_value = quantity
        .checked_mul(price)
        .and_then(|weight| order_commission.checked_mul(weight))
        .and_then(|amount| amount.checked_div(total_value));
    (by_quantity, by_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_money_boundaries_are_validated() {
        assert_eq!(money_decimal(0, -1), Some(Decimal::new(-1, 9)));
        assert_eq!(
            money_decimal(i64::MAX, 999_999_999),
            Decimal::from(i64::MAX).checked_add(Decimal::new(999_999_999, 9))
        );
        assert_eq!(money_decimal(1, -1), None);
        assert_eq!(money_decimal(-1, 1), None);
        assert_eq!(money_decimal(0, 1_000_000_000), None);
    }

    #[test]
    fn weights_and_exact_comparisons_distinguish_hypotheses() {
        let one = Decimal::ONE;
        let ten = Decimal::from(10);
        let twenty = Decimal::from(20);
        let (quantity, value) = weights(&[(one, ten), (one, twenty)]).unwrap();
        assert_eq!((quantity, value), (Decimal::from(2), Decimal::from(30)));
        assert_eq!(
            exact_matches(
                Decimal::from(3),
                Decimal::from(6),
                one,
                ten,
                quantity,
                value
            ),
            Some((true, false))
        );
        assert_eq!(
            exact_matches(
                Decimal::from(2),
                Decimal::from(6),
                one,
                ten,
                quantity,
                value
            ),
            Some((false, true))
        );
        assert_eq!(
            exact_matches(Decimal::ZERO, Decimal::ZERO, one, ten, quantity, value),
            Some((true, true))
        );
        assert_eq!(
            exact_matches(one, Decimal::from(6), one, ten, quantity, value),
            Some((false, false))
        );
    }

    #[test]
    fn zero_and_overflow_do_not_become_evidence() {
        assert_eq!(weights(&[]), None);
        assert_eq!(weights(&[(Decimal::ZERO, Decimal::ONE)]), None);
        assert_eq!(weights(&[(Decimal::MAX, Decimal::from(2))]), None);
        assert_eq!(
            exact_matches(
                Decimal::MAX,
                Decimal::ONE,
                Decimal::ONE,
                Decimal::ONE,
                Decimal::from(2),
                Decimal::from(2)
            ),
            None
        );
        assert_eq!(
            allocations(
                Decimal::ONE,
                Decimal::ONE,
                Decimal::ONE,
                Decimal::ZERO,
                Decimal::ZERO
            ),
            (None, None)
        );
        assert_eq!(
            allocations(
                Decimal::MAX,
                Decimal::from(2),
                Decimal::ONE,
                Decimal::from(2),
                Decimal::from(2)
            ),
            (None, None)
        );
    }

    #[test]
    fn rounded_display_does_not_establish_an_exact_match() {
        let (display, _) = allocations(
            Decimal::ONE,
            Decimal::ONE,
            Decimal::ONE,
            Decimal::from(3),
            Decimal::from(3),
        );
        let displayed = display.unwrap();
        assert_eq!(
            exact_matches(
                displayed,
                Decimal::ONE,
                Decimal::ONE,
                Decimal::ONE,
                Decimal::from(3),
                Decimal::from(3)
            ),
            Some((false, false))
        );
    }
}
