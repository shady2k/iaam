//! Allocation of tax basis on amortisation as an event fact.
//!
//! The share is stored in the fact itself, not derived later: if the
//! reference data is corrected, there would be no way to derive it.
//! The same argument explains why `Conversion` stores `basis_transfer`:
//! the terms live in the issuer's decision, not in reference data.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::rules::ReturnedShare;

/// Why the basis allocation was not computed.
///
/// Projections need only one “unknown”, but the owner needs to know what to
/// load next, and the audit needs to know what went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationGap {
    /// The event predates the field or enrichment was not run.
    NotComputed,
    /// No issuance schedule exists.
    ScheduleMissing,
    /// The schedule exists but was not validated.
    ScheduleNotValidated,
    /// No repayment occurs on the event date.
    NoRepaymentOnDate,
    /// The event amount does not match the scheduled share.
    AmountMismatch,
    /// The repayment currency does not match the principal currency.
    CurrencyMismatch,
    /// Multiple repayments occur on the date and could not be matched
    /// to events.
    AmbiguousSameDateRepayments,
    /// Returns through the date exceed 100%.
    InvalidPrefix,
}

impl AllocationGap {
    /// All variants. A guard against forgetting a code for a new family
    /// member: the test walks this array, while the compiler checks its length.
    pub const ALL: [Self; 8] = [
        Self::NotComputed,
        Self::ScheduleMissing,
        Self::ScheduleNotValidated,
        Self::NoRepaymentOnDate,
        Self::AmountMismatch,
        Self::CurrencyMismatch,
        Self::AmbiguousSameDateRepayments,
        Self::InvalidPrefix,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotComputed => "not_computed",
            Self::ScheduleMissing => "schedule_missing",
            Self::ScheduleNotValidated => "schedule_not_validated",
            Self::NoRepaymentOnDate => "no_repayment_on_date",
            Self::AmountMismatch => "amount_mismatch",
            Self::CurrencyMismatch => "currency_mismatch",
            Self::AmbiguousSameDateRepayments => "ambiguous_same_date_repayments",
            Self::InvalidPrefix => "invalid_prefix",
        }
    }
}

/// Allocation-algorithm version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AllocationAlgorithmVersion(pub u16);

/// Fingerprint of the canonical reference-input selection used for allocation.
///
/// Covers everything the share depends on: principal and currency, returns
/// included in the remainder before the event, returns on the event date,
/// source-snapshot identity, and the rule version for grouping equal dates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AllocationInputsHash(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("input fingerprint is not 64 hexadecimal characters")]
pub struct AllocationInputsHashError;

impl AllocationInputsHash {
    pub fn new(value: impl Into<String>) -> Result<Self, AllocationInputsHashError> {
        let value = value.into();
        if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AllocationInputsHashError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Additional inputs from which the application derived the share.
///
/// Separate from `Provenance`: that answers “where did the raw fact come from?”,
/// while this answers “from what was the derived field computed?”.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationEvidence {
    pub inputs_hash: AllocationInputsHash,
    pub knowledge_as_of: OffsetDateTime,
    pub algorithm_version: AllocationAlgorithmVersion,
}

/// Basis allocation with evidence of its computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BasisAllocation {
    Unknown(AllocationGap),
    Known {
        share: ReturnedShare,
        evidence: AllocationEvidence,
    },
}

impl Default for BasisAllocation {
    /// Honest default: an event written before the field existed asserted
    /// nothing, and assigning it a share would claim that someone computed
    /// what nobody computed.
    fn default() -> Self {
        Self::Unknown(AllocationGap::NotComputed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use crate::rules::ReturnedShare;
    use rust_decimal::Decimal;
    use time::OffsetDateTime;

    fn known() -> BasisAllocation {
        BasisAllocation::Known {
            share: ReturnedShare::new(Dec::new(Decimal::new(2, 1))).expect("share 0.2"),
            evidence: AllocationEvidence {
                inputs_hash: AllocationInputsHash::new("a".repeat(64)).expect("hex"),
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
                algorithm_version: AllocationAlgorithmVersion(1),
            },
        }
    }

    #[test]
    fn the_default_allocation_is_unknown_because_the_field_was_never_filled() {
        assert_eq!(
            BasisAllocation::default(),
            BasisAllocation::Unknown(AllocationGap::NotComputed)
        );
    }

    #[test]
    fn a_known_allocation_survives_a_json_round_trip() {
        let text = serde_json::to_string(&known()).expect("write");
        assert_eq!(
            serde_json::from_str::<BasisAllocation>(&text).expect("read"),
            known()
        );
    }

    #[test]
    fn a_known_allocation_survives_a_cbor_round_trip() {
        let mut body = Vec::new();
        ciborium::into_writer(&known(), &mut body).expect("write");
        assert_eq!(
            ciborium::from_reader::<BasisAllocation, _>(body.as_slice()).expect("read"),
            known()
        );
    }

    #[test]
    fn every_gap_names_its_reason() {
        for gap in AllocationGap::ALL {
            assert!(!gap.code().is_empty());
        }
    }

    #[test]
    fn a_hash_that_is_not_sixty_four_hex_digits_is_rejected() {
        assert!(AllocationInputsHash::new("abc").is_err());
        assert!(AllocationInputsHash::new("z".repeat(64)).is_err());
        assert!(AllocationInputsHash::new("A".repeat(64)).is_ok());
    }
}
