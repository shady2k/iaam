//! Exact mode (§6.6): rational arithmetic with no loss of precision.
//!
//! Used where the residual must be exactly zero: identity
//! of the result (§6.3), tax-basis allocation, reconciliation.

use core::cmp::Ordering;

use ethnum::I256;

use super::NumericError;

/// A rational number: always in lowest terms, with a positive denominator.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Exact {
    num: i128,
    den: i128,
}

impl Exact {
    /// Constructor. The body is moved to [`Exact::from_ratio`]: `cargo-mutants`
    /// 27.1.0 does not generate mutants for functions named `new`, so
    /// normalization left here would not be covered by the mutation
    /// gate at all — “no survivors” would mean “not tested”.
    pub fn new(num: i128, den: i128) -> Result<Self, NumericError> {
        Self::from_ratio(num, den)
    }

    /// Reduces the fraction to canonical form: the denominator is positive,
    /// and the fraction is in lowest terms.
    fn from_ratio(num: i128, den: i128) -> Result<Self, NumericError> {
        if den == 0 {
            return Err(NumericError::ZeroDenominator);
        }
        // checked_neg rather than unary minus: `i128::MIN` has no positive
        // representation, and `-i128::MIN` panics in debug and wraps
        // in release. Silent wrapping is unacceptable in financial arithmetic.
        let (num, den) = if den.is_negative() {
            (
                num.checked_neg().ok_or(NumericError::Overflow)?,
                den.checked_neg().ok_or(NumericError::Overflow)?,
            )
        } else {
            (num, den)
        };
        // g != 0: den != 0, so the GCD is positive
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()) as i128;
        Ok(Self {
            num: num / g,
            den: den / g,
        })
    }

    pub const fn from_int(v: i128) -> Self {
        Self { num: v, den: 1 }
    }

    pub const fn zero() -> Self {
        Self { num: 0, den: 1 }
    }

    pub const fn is_zero(&self) -> bool {
        self.num == 0
    }

    pub const fn numerator(&self) -> i128 {
        self.num
    }

    pub const fn denominator(&self) -> i128 {
        self.den
    }

    /// Addition. Panics on `i128` overflow — this is a domain
    /// error (amounts of this magnitude are impossible), not a normal path.
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        let num = self
            .num
            .checked_mul(other.den)
            .and_then(|a| {
                other
                    .num
                    .checked_mul(self.den)
                    .and_then(|b| a.checked_add(b))
            })
            .expect("i128 overflow in exact arithmetic");
        let den = self
            .den
            .checked_mul(other.den)
            .expect("i128 overflow in denominator");
        Self::new(num, den).expect("denominator cannot become zero")
    }

    pub fn neg(&self) -> Result<Self, NumericError> {
        Ok(Self {
            num: self.num.checked_neg().ok_or(NumericError::Overflow)?,
            den: self.den,
        })
    }

    pub fn sub(&self, other: &Self) -> Result<Self, NumericError> {
        Ok(self.add(&other.neg()?))
    }

    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        let num = self
            .num
            .checked_mul(other.num)
            .expect("i128 overflow in multiplication");
        let den = self
            .den
            .checked_mul(other.den)
            .expect("i128 overflow in multiplication");
        Self::new(num, den).expect("denominator cannot become zero")
    }

    pub fn div(&self, other: &Self) -> Result<Self, NumericError> {
        if other.is_zero() {
            return Err(NumericError::DivisionByZero);
        }
        let num = self
            .num
            .checked_mul(other.den)
            .ok_or(NumericError::Overflow)?;
        let den = self
            .den
            .checked_mul(other.num)
            .ok_or(NumericError::Overflow)?;
        Self::new(num, den)
    }

    /// Sums a list. Kept separate because the identity check (§6.3)
    /// sums the components and must produce an exact zero.
    #[must_use]
    pub fn sum(items: &[Self]) -> Self {
        items.iter().fold(Self::zero(), |acc, x| acc.add(x))
    }
}

impl PartialOrd for Exact {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Exact {
    fn cmp(&self, other: &Self) -> Ordering {
        // a/b <=> c/d for b,d > 0 is equivalent to a*d <=> c*b.
        // The products are computed using 256-bit arithmetic, so there is no overflow
        // and the comparison remains exact for all valid values.
        let lhs = I256::from(self.num) * I256::from(other.den);
        let rhs = I256::from(other.num) * I256::from(self.den);
        lhs.cmp(&rhs)
    }
}

/// Greatest common divisor. Called only with denominator `den != 0`,
/// so the result is always positive and division by it is safe.
const fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirds_sum_to_exactly_one() {
        let third = Exact::new(1, 3).unwrap();
        let sum = third.add(&third).add(&third);
        assert_eq!(sum, Exact::from_int(1));
    }

    #[test]
    fn decimal_tenths_sum_exactly() {
        // 0.1 + 0.2 == 0.3 — unlike binary floating-point arithmetic
        let a = Exact::new(1, 10).unwrap();
        let b = Exact::new(2, 10).unwrap();
        assert_eq!(a.add(&b), Exact::new(3, 10).unwrap());
    }

    #[test]
    fn zero_denominator_rejected() {
        assert!(matches!(
            Exact::new(1, 0),
            Err(NumericError::ZeroDenominator)
        ));
    }

    #[test]
    fn fraction_is_reduced_to_lowest_terms() {
        let e = Exact::new(6, 4).unwrap();
        assert_eq!(e.numerator(), 3);
        assert_eq!(e.denominator(), 2);
    }

    #[test]
    fn reduction_needs_the_full_euclidean_loop() {
        // GCD(1_071, 462) = 21 takes several steps of the algorithm:
        // dividing only once by the first remainder would yield a different fraction.
        let e = Exact::new(1_071, 462).unwrap();
        assert_eq!(e.numerator(), 51);
        assert_eq!(e.denominator(), 22);
    }

    #[test]
    fn negative_denominator_moves_sign_to_numerator() {
        let e = Exact::new(3, -4).unwrap();
        assert_eq!(e.numerator(), -3);
        assert_eq!(e.denominator(), 4);
    }

    #[test]
    fn positive_denominator_keeps_its_sign() {
        let e = Exact::new(-3, 4).unwrap();
        assert_eq!(e.numerator(), -3);
        assert_eq!(e.denominator(), 4);
    }

    #[test]
    fn negation_of_i128_min_reports_overflow_instead_of_panicking() {
        assert!(matches!(
            Exact::from_int(i128::MIN).neg(),
            Err(NumericError::Overflow)
        ));
    }

    #[test]
    fn negative_denominator_at_i128_min_reports_overflow() {
        assert!(matches!(
            Exact::new(1, i128::MIN),
            Err(NumericError::Overflow)
        ));
        assert!(matches!(
            Exact::new(i128::MIN, -1),
            Err(NumericError::Overflow)
        ));
    }

    #[test]
    fn from_int_and_zero_are_canonical() {
        assert_eq!(Exact::from_int(7).numerator(), 7);
        assert_eq!(Exact::from_int(7).denominator(), 1);
        assert_eq!(Exact::zero().numerator(), 0);
        assert_eq!(Exact::zero().denominator(), 1);
    }

    #[test]
    fn is_zero_distinguishes_zero_from_small_values() {
        assert!(Exact::zero().is_zero());
        assert!(!Exact::new(1, 1_000_000).unwrap().is_zero());
    }

    #[test]
    fn addition_of_unlike_denominators() {
        let a = Exact::new(1, 2).unwrap();
        let b = Exact::new(1, 3).unwrap();
        assert_eq!(a.add(&b), Exact::new(5, 6).unwrap());
    }

    #[test]
    fn addition_with_opposite_signs_cancels() {
        let a = Exact::new(2, 7).unwrap();
        let b = Exact::new(-2, 7).unwrap();
        assert!(a.add(&b).is_zero());
    }

    #[test]
    fn negation_flips_sign_and_keeps_denominator() {
        let e = Exact::new(5, 8).unwrap().neg().unwrap();
        assert_eq!(e.numerator(), -5);
        assert_eq!(e.denominator(), 8);
    }

    #[test]
    fn subtraction_is_not_addition() {
        let a = Exact::new(3, 4).unwrap();
        let b = Exact::new(1, 6).unwrap();
        assert_eq!(a.sub(&b).unwrap(), Exact::new(7, 12).unwrap());
    }

    #[test]
    fn subtraction_reports_overflow_on_i128_min() {
        let a = Exact::from_int(1);
        let b = Exact::from_int(i128::MIN);
        assert!(matches!(a.sub(&b), Err(NumericError::Overflow)));
    }

    #[test]
    fn multiplication_reduces_the_result() {
        let a = Exact::new(2, 3).unwrap();
        let b = Exact::new(3, 4).unwrap();
        assert_eq!(a.mul(&b), Exact::new(1, 2).unwrap());
    }

    #[test]
    fn division_is_multiplication_by_the_reciprocal() {
        let a = Exact::new(2, 3).unwrap();
        let b = Exact::new(4, 5).unwrap();
        assert_eq!(a.div(&b).unwrap(), Exact::new(5, 6).unwrap());
    }

    #[test]
    fn division_by_zero_is_rejected() {
        let a = Exact::new(2, 3).unwrap();
        assert!(matches!(
            a.div(&Exact::zero()),
            Err(NumericError::DivisionByZero)
        ));
    }

    #[test]
    fn division_by_negative_keeps_denominator_positive() {
        let a = Exact::new(1, 2).unwrap();
        let b = Exact::new(-1, 3).unwrap();
        let q = a.div(&b).unwrap();
        assert_eq!(q.numerator(), -3);
        assert_eq!(q.denominator(), 2);
    }

    #[test]
    fn division_reports_overflow_instead_of_wrapping() {
        let a = Exact::new(i128::MAX, 1).unwrap();
        let b = Exact::new(1, i128::MAX).unwrap();
        assert!(matches!(a.div(&b), Err(NumericError::Overflow)));
    }

    #[test]
    fn division_reports_overflow_in_the_denominator() {
        let a = Exact::new(1, i128::MAX).unwrap();
        let b = Exact::new(i128::MAX, 1).unwrap();
        assert!(matches!(a.div(&b), Err(NumericError::Overflow)));
    }

    #[test]
    fn sum_of_empty_slice_is_zero() {
        assert_eq!(Exact::sum(&[]), Exact::zero());
    }

    #[test]
    fn sum_accumulates_every_item() {
        let items = [
            Exact::new(1, 6).unwrap(),
            Exact::new(1, 3).unwrap(),
            Exact::new(1, 2).unwrap(),
        ];
        assert_eq!(Exact::sum(&items), Exact::from_int(1));
    }

    #[test]
    fn sum_of_identity_components_is_strictly_zero() {
        let items = [
            Exact::new(7, 3).unwrap(),
            Exact::new(-4, 3).unwrap(),
            Exact::from_int(-1),
        ];
        assert!(Exact::sum(&items).is_zero());
    }

    #[test]
    fn ordering_compares_by_cross_multiplication() {
        let three_quarters = Exact::new(3, 4).unwrap();
        let two_thirds = Exact::new(2, 3).unwrap();
        assert_eq!(three_quarters.cmp(&two_thirds), Ordering::Greater);
        assert_eq!(two_thirds.cmp(&three_quarters), Ordering::Less);
        assert_eq!(two_thirds.cmp(&two_thirds), Ordering::Equal);
    }

    #[test]
    fn ordering_is_not_a_difference_of_terms() {
        // 5/2 < 3/1, even though 5−1 > 3−2: the comparison must use cross-products.
        let a = Exact::new(5, 2).unwrap();
        let b = Exact::from_int(3);
        assert_eq!(a.cmp(&b), Ordering::Less);
    }

    #[test]
    fn ordering_survives_operands_that_would_overflow_i128() {
        // The cross-products here do not fit in i128 —
        // the comparison must be computed at a wider width.
        let big = Exact::new(i128::MAX, 2).unwrap();
        let bigger = Exact::new(i128::MAX, 1).unwrap();
        assert_eq!(big.cmp(&bigger), Ordering::Less);
        assert_eq!(bigger.cmp(&big), Ordering::Greater);
    }

    #[test]
    fn ordering_handles_negative_values() {
        let neg = Exact::new(-1, 3).unwrap();
        let pos = Exact::new(1, 300).unwrap();
        assert!(neg < pos);
        assert!(pos > neg);
    }

    #[test]
    fn partial_ord_agrees_with_ord() {
        let a = Exact::new(1, 3).unwrap();
        let b = Exact::new(1, 2).unwrap();
        assert_eq!(a.partial_cmp(&b), Some(Ordering::Less));
        assert_eq!(b.partial_cmp(&a), Some(Ordering::Greater));
        assert_eq!(a.partial_cmp(&a), Some(Ordering::Equal));
    }

    #[test]
    fn gcd_of_coprime_values_is_one() {
        let e = Exact::new(7, 9).unwrap();
        assert_eq!(e.numerator(), 7);
        assert_eq!(e.denominator(), 9);
    }

    #[test]
    fn zero_numerator_normalizes_to_canonical_zero() {
        let e = Exact::new(0, 5).unwrap();
        assert_eq!(e, Exact::zero());
        assert_eq!(e.denominator(), 1);
    }
}
