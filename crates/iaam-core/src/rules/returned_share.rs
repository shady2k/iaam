//! Share of outstanding principal returned by one event (§6.5).
//!
//! A dimensionless value, not an amount: basis allocation reduces amounts,
//! and storing the share keeps the fact independent of what the reference
//! data will know about principal tomorrow.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::numeric::decimal::Dec;

/// Return share from the outstanding remainder **before** the event.
///
/// One is valid: the final amortisation returns the entire remainder, and
/// legal disposal of the security is a separate fact, not a consequence
/// (`event/corporate_action.rs`, `PartialRedemption` versus `Redemption`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ReturnedShare(Dec);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReturnedShareError {
    #[error("return share is not positive: the event returned nothing")]
    NotPositive,
    #[error("return share exceeds one: more than the remainder cannot be returned")]
    AboveOne,
}

impl ReturnedShare {
    /// A constructor, not a public field: manually assembled values would
    /// bypass both checks.
    pub fn new(value: Dec) -> Result<Self, ReturnedShareError> {
        if !value.is_positive() {
            return Err(ReturnedShareError::NotPositive);
        }
        if value > Dec::one() {
            return Err(ReturnedShareError::AboveOne);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn inner(self) -> Dec {
        self.0
    }
}

impl TryFrom<Dec> for ReturnedShare {
    type Error = ReturnedShareError;

    fn try_from(value: Dec) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

// `#[derive(Deserialize)]` would assemble the newtype without the constructor,
// so deserialisation goes through `TryFrom`.
impl<'de> Deserialize<'de> for ReturnedShare {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Dec::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn dec(text: &str) -> Dec {
        Dec::new(text.parse::<Decimal>().expect("decimal number"))
    }

    #[test]
    fn a_share_of_one_is_accepted_because_a_last_amortisation_returns_everything() {
        assert!(ReturnedShare::new(dec("1")).is_ok());
    }

    #[test]
    fn a_zero_share_is_rejected_because_nothing_was_returned() {
        assert_eq!(
            ReturnedShare::new(dec("0")).unwrap_err(),
            ReturnedShareError::NotPositive
        );
    }

    #[test]
    fn a_negative_share_is_rejected() {
        assert_eq!(
            ReturnedShare::new(dec("-0.1")).unwrap_err(),
            ReturnedShareError::NotPositive
        );
    }

    #[test]
    fn a_share_above_one_is_rejected_because_more_than_the_remainder_cannot_return() {
        assert_eq!(
            ReturnedShare::new(dec("1.0001")).unwrap_err(),
            ReturnedShareError::AboveOne
        );
    }

    #[test]
    fn json_deserialisation_does_not_bypass_the_invariant() {
        let error = serde_json::from_str::<ReturnedShare>("\"1.5\"")
            .expect_err("invalid share must fail to deserialize");
        assert!(error.to_string().contains("exceeds one"), "{error}");
    }

    #[test]
    fn cbor_deserialisation_does_not_bypass_the_invariant() {
        let mut body = Vec::new();
        ciborium::into_writer(&dec("2"), &mut body).expect("write");
        assert!(ciborium::from_reader::<ReturnedShare, _>(body.as_slice()).is_err());
    }
}
