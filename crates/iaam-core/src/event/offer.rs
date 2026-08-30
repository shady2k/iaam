//! Offer execution (§4.7).
//!
//! An offer is **not** a corporate action: it is the holder’s right, not
//! the issuer’s decision. Treating the buyback as redemption would lose
//! both the cause of the disposal and the fact that the holder might not tender
//! the security at all — and the «tendered or held to maturity» scenario is precisely
//! why the offer is tracked.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::{CustodyId, InstrumentId};
use crate::money::{Money, Quantity};

/// A submitted request to tender a security under the offer.
///
/// Its own identity, not an event identifier: one request
/// links a chain of several facts — submission, withdrawal, and one or
/// more settlements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OfferSubmissionId(pub Uuid);

impl OfferSubmissionId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn inner(&self) -> Uuid {
        self.0
    }
}

/// Offer request submission window.
///
/// In Part 1, identity is opaque: there is no window registry, and the check
/// «the window exists, the request was submitted on time» is deferred to E3.4.6 **explicitly**.
/// Recording the identity now is cheaper than reconstructing the link later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OfferWindowId(pub Uuid);

impl OfferWindowId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn inner(&self) -> Uuid {
        self.0
    }
}

/// What happened to the offer request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OfferExerciseAction {
    /// Submitted request. It has no legs: it moves neither cash nor securities —
    /// like `ControlAssertion`. Having no legs is itself a form, and it
    /// is checked like all the others.
    Submitted {
        submission: OfferSubmissionId,
        window: OfferWindowId,
        instrument: InstrumentId,
        quantity: Quantity,
    },
    /// Full or partial withdrawal of the request.
    ///
    /// A third member, not the absence of settlement: §3.5 lists withdrawal alongside
    /// partial execution, and without it an outstanding request would remain
    /// forever, distorting the expected disposal.
    Cancelled {
        submission: OfferSubmissionId,
        quantity: Quantity,
    },
    /// Completed buyback. The legs are `Cash` and a negative
    /// `SecurityQuantity`; there is no `Principal` leg: the security is disposed of,
    /// not redeemed at face value. A single request may have
    /// multiple settlements.
    Settled {
        submission: OfferSubmissionId,
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        gross: Money,
        fee: Option<Money>,
        accrued_interest: Option<Money>,
    },
}

impl OfferExerciseAction {
    /// Member name for diagnostics and guardrails.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::Submitted { .. } => "offer_submitted",
            Self::Cancelled { .. } => "offer_cancelled",
            Self::Settled { .. } => "offer_settled",
        }
    }

    /// The request this fact belongs to: chain linkage available without
    /// parsing the family.
    #[must_use]
    pub const fn submission(&self) -> OfferSubmissionId {
        match self {
            Self::Submitted { submission, .. }
            | Self::Cancelled { submission, .. }
            | Self::Settled { submission, .. } => *submission,
        }
    }

    /// Quantity of securities affected by the fact.
    #[must_use]
    pub const fn quantity(&self) -> Quantity {
        match self {
            Self::Submitted { quantity, .. }
            | Self::Cancelled { quantity, .. }
            | Self::Settled { quantity, .. } => *quantity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(text: &str) -> Quantity {
        Quantity(Dec::new(Decimal::from_str_exact(text).unwrap()))
    }

    fn sample_submitted() -> OfferExerciseAction {
        OfferExerciseAction::Submitted {
            submission: OfferSubmissionId::new_random(),
            window: OfferWindowId::new_random(),
            instrument: InstrumentId::new_random(),
            quantity: qty("10"),
        }
    }

    fn sample_cancelled() -> OfferExerciseAction {
        OfferExerciseAction::Cancelled {
            submission: OfferSubmissionId::new_random(),
            quantity: qty("4"),
        }
    }

    fn sample_settled() -> OfferExerciseAction {
        OfferExerciseAction::Settled {
            submission: OfferSubmissionId::new_random(),
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: qty("6"),
            gross: rub(6_000_000),
            fee: Some(rub(-1_000)),
            accrued_interest: Some(rub(12_345)),
        }
    }

    #[test]
    fn every_offer_action_survives_a_json_round_trip() {
        for action in [sample_submitted(), sample_cancelled(), sample_settled()] {
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(
                serde_json::from_str::<OfferExerciseAction>(&text).unwrap(),
                action
            );
        }
    }

    #[test]
    fn every_offer_action_names_itself() {
        assert_eq!(sample_submitted().discriminant(), "offer_submitted");
        assert_eq!(sample_cancelled().discriminant(), "offer_cancelled");
        assert_eq!(sample_settled().discriminant(), "offer_settled");
    }

    #[test]
    fn every_offer_action_names_the_submission_it_belongs_to() {
        // The «submitted — withdrew — settled» chain is linked by the request,
        // and the link must be available without parsing the family.
        let submission = OfferSubmissionId::new_random();
        let action = OfferExerciseAction::Cancelled {
            submission,
            quantity: qty("1"),
        };
        assert_eq!(action.submission(), submission);
    }

    #[test]
    fn the_third_member_is_cancellation_not_an_afterthought() {
        // §3.5 lists withdrawal alongside partial execution: without it
        // an outstanding request would remain forever and distort the expected
        // disposal.
        assert!(matches!(
            sample_cancelled(),
            OfferExerciseAction::Cancelled { .. }
        ));
    }
}
