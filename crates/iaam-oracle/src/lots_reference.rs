//! Reference implementation of lot disposal (§15.4).
//!
//! **A deliberately different algorithm.** Production uses an iterative pass
//! with a mutable remainder and `Decimal`. The reference uses recursion with
//! an accumulator and integer arithmetic. No code is shared, so the same bug
//! cannot occur in both implementations.
//!
//! Quantities here are integers: the reference covers exchange-traded
//! securities, where fractional quantities do not exist. Fractional cases
//! (crypto) are tested by fixtures.

use core::cmp::Ordering;

/// A lot in the reference representation: quantity and cost in minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefLot {
    pub quantity: i64,
    pub basis_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefDisposal {
    pub basis_released_minor: i64,
    pub remaining: Vec<RefLot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefError {
    InsufficientQuantity,
}

/// Disposal in acquisition-time order (FIFO).
///
/// Implemented recursively: each step processes the head of the list and
/// passes the tail onward. The accumulator carries the released cost.
pub fn dispose_fifo_rational(lots: &[RefLot], quantity: i64) -> Result<RefDisposal, RefError> {
    fn go(lots: &[RefLot], left: i64, released: i64) -> Result<RefDisposal, RefError> {
        match lots.split_first() {
            None if left == 0 => Ok(RefDisposal {
                basis_released_minor: released,
                remaining: vec![],
            }),
            None => Err(RefError::InsufficientQuantity),
            Some((head, tail)) if left == 0 => {
                let mut remaining = vec![*head];
                remaining.extend_from_slice(tail);
                Ok(RefDisposal {
                    basis_released_minor: released,
                    remaining,
                })
            }
            Some((head, tail)) if head.quantity <= left => {
                go(tail, left - head.quantity, released + head.basis_minor)
            }
            Some((head, tail)) => {
                // Proportional allocation through integer arithmetic
                // with ties-to-even rounding, as in production,
                // but expressed differently.
                let taken = round_half_to_even(head.basis_minor, left, head.quantity);
                let kept = head.basis_minor - taken;
                let mut remaining = vec![RefLot {
                    quantity: head.quantity - left,
                    basis_minor: kept,
                }];
                remaining.extend_from_slice(tail);
                Ok(RefDisposal {
                    basis_released_minor: released + taken,
                    remaining,
                })
            }
        }
    }
    go(lots, quantity, 0)
}

/// `total * num / den` with ties-to-even rounding, without floating point.
fn round_half_to_even(total: i64, num: i64, den: i64) -> i64 {
    debug_assert!(den > 0);
    let product = i128::from(total) * i128::from(num);
    let den = i128::from(den);
    let quotient = product.div_euclid(den);
    let remainder = product.rem_euclid(den);
    let twice = remainder * 2;
    // Three arms instead of an `if` chain: two would produce the same value,
    // and the chain would not compile (`clippy::if_same_then_else`).
    // The solution is to compute a “round up” flag, not the value itself.
    let round_up = match twice.cmp(&den) {
        Ordering::Greater => true,
        Ordering::Less => false,
        // Tie: round to even. Raise an odd quotient, leave an even one.
        Ordering::Equal => quotient % 2 != 0,
    };
    let result = if round_up { quotient + 1 } else { quotient };
    i64::try_from(result).expect("lot cost fits in i64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_to_even_rounds_ties_to_even() {
        // 5 * 1 / 2 = 2.5 → 2 (even)
        assert_eq!(round_half_to_even(5, 1, 2), 2);
        // 7 * 1 / 2 = 3.5 → 4 (even)
        assert_eq!(round_half_to_even(7, 1, 2), 4);
    }

    #[test]
    fn selling_more_than_held_is_an_error() {
        // The reference's InsufficientQuantity branch had no coverage: the
        // parity fixture has no oversell case, and the reference had no
        // tests of its own. The mutation guard exposed this.
        let lots = [RefLot {
            quantity: 10,
            basis_minor: 100_000,
        }];
        assert_eq!(
            dispose_fifo_rational(&lots, 11),
            Err(RefError::InsufficientQuantity)
        );
    }

    #[test]
    fn selling_nothing_consumes_no_lot_even_if_it_is_empty() {
        // A zero-quantity lot with nonzero cost distinguishes “sell nothing”
        // from “dispose the whole lot”: if execution falls through to the
        // next branch, `0 <= 0` is true and the cost would be released.
        // Degenerate input, but exactly what separates the two intentions.
        let lots = [
            RefLot {
                quantity: 0,
                basis_minor: 500,
            },
            RefLot {
                quantity: 10,
                basis_minor: 100_000,
            },
        ];
        let out = dispose_fifo_rational(&lots, 0).unwrap();
        assert_eq!(
            out.basis_released_minor, 0,
            "nothing sold means nothing released"
        );
        assert_eq!(out.remaining.len(), 2, "both lots remain untouched");
    }

    #[test]
    fn taking_first_lot_whole() {
        let lots = [RefLot {
            quantity: 10,
            basis_minor: 100_000,
        }];
        let out = dispose_fifo_rational(&lots, 10).unwrap();
        assert_eq!(out.basis_released_minor, 100_000);
        assert!(out.remaining.is_empty());
    }
}
