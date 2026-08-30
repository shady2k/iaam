//! Order book for the offer (§3.5).
//!
//! A separate projection rather than fields of [`super::lots::LotBook`]: a submission has
//! its own chain of “submitted — cancelled — settled”; its state is not
//! a property of the acquisition lot, and the lot book snapshot
//! does not change when the offer appears.
//!
//! The invariant “settled plus cancelled does not exceed submitted” is a property
//! of the **chain**, not of a single fact, so it belongs here rather than
//! in the event's structural validation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use crate::ids::InstrumentId;
use crate::money::Quantity;
use crate::numeric::NumericError;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OfferError {
    #[error("submission {submission:?} claims {claimed:?} with {submitted:?} submitted")]
    OverSettled {
        submission: OfferSubmissionId,
        submitted: Quantity,
        claimed: Quantity,
    },
    #[error("submission {submission:?} was not submitted")]
    UnknownSubmission { submission: OfferSubmissionId },
    #[error("submission {submission:?} was submitted more than once")]
    DuplicateSubmission { submission: OfferSubmissionId },
    #[error("quantity for submission {submission:?} is not positive")]
    NonPositiveQuantity { submission: OfferSubmissionId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// State of a single submission.
///
/// Three accumulators are stored rather than a single remainder: cancelled and
/// settled are different outcomes, and the report will need to distinguish them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmissionState {
    pub window: OfferWindowId,
    pub instrument: InstrumentId,
    pub submitted: Quantity,
    pub cancelled: Quantity,
    pub settled: Quantity,
}

impl SubmissionState {
    /// How much of the submission remains unresolved.
    ///
    /// The `cancelled + settled <= submitted` invariant is maintained by [`OfferBook::apply`],
    /// so the subtraction neither goes negative nor overflows.
    /// `debug_assert` catches invariant violations in tests; in release builds
    /// zero remains — a value that will not tempt the caller
    /// to act on corrupted state.
    #[must_use]
    pub fn outstanding(&self) -> Quantity {
        let claimed = self.cancelled.0.checked_add(self.settled.0);
        let left = claimed.and_then(|claimed| self.submitted.0.checked_sub(claimed));
        debug_assert!(
            left.is_ok_and(|left| !left.is_negative()),
            "submission invariant violated: cancelled plus settled exceeds submitted"
        );
        left.map_or_else(|_| Quantity::zero(), Quantity)
    }
}

/// Order book for the offer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OfferBook {
    submissions: BTreeMap<OfferSubmissionId, SubmissionState>,
}

impl OfferBook {
    /// Apply an offer fact to the book.
    pub fn apply(&mut self, action: &OfferExerciseAction) -> Result<(), OfferError> {
        let submission = action.submission();
        if !action.quantity().0.is_positive() {
            return Err(OfferError::NonPositiveQuantity { submission });
        }
        match action {
            OfferExerciseAction::Submitted {
                window,
                instrument,
                quantity,
                ..
            } => {
                if self.submissions.contains_key(&submission) {
                    // Repeated submission under the same identifier is
                    // not a second submission, but a lost first one.
                    return Err(OfferError::DuplicateSubmission { submission });
                }
                self.submissions.insert(
                    submission,
                    SubmissionState {
                        window: *window,
                        instrument: *instrument,
                        submitted: *quantity,
                        cancelled: Quantity::zero(),
                        settled: Quantity::zero(),
                    },
                );
                Ok(())
            }
            OfferExerciseAction::Cancelled { quantity, .. } => {
                let state = self.state_mut(submission)?;
                let cancelled = Quantity(state.cancelled.0.checked_add(quantity.0)?);
                check_claim(submission, state, cancelled, state.settled)?;
                state.cancelled = cancelled;
                Ok(())
            }
            OfferExerciseAction::Settled { quantity, .. } => {
                let state = self.state_mut(submission)?;
                let settled = Quantity(state.settled.0.checked_add(quantity.0)?);
                check_claim(submission, state, state.cancelled, settled)?;
                state.settled = settled;
                Ok(())
            }
        }
    }

    /// How much of the submission remains unresolved. A submission that was never made
    /// has no outstanding claim: `apply` does not accept facts
    /// about an unknown submission, so no claim can arise.
    #[must_use]
    pub fn outstanding(&self, submission: OfferSubmissionId) -> Quantity {
        self.submissions
            .get(&submission)
            .map_or_else(Quantity::zero, SubmissionState::outstanding)
    }

    /// The entire state of the submission.
    #[must_use]
    pub fn submission(&self, submission: OfferSubmissionId) -> Option<&SubmissionState> {
        self.submissions.get(&submission)
    }

    fn state_mut(
        &mut self,
        submission: OfferSubmissionId,
    ) -> Result<&mut SubmissionState, OfferError> {
        self.submissions
            .get_mut(&submission)
            .ok_or(OfferError::UnknownSubmission { submission })
    }
}

/// List submissions whose recorded windows are absent from the schedule registry.
///
/// Old journal facts are not rewritten or matched to windows by
/// a similar date: a submission has only its stored window identifier.
#[must_use]
pub fn unresolved_submissions(
    book: &OfferBook,
    schedule: &crate::bond::BondSchedule,
) -> Vec<OfferSubmissionId> {
    book.submissions
        .iter()
        .filter_map(|(submission, state)| {
            let known = schedule
                .offer_windows
                .iter()
                .any(|window| window.window == state.window);
            if known { None } else { Some(*submission) }
        })
        .collect()
}

/// Cancelled plus settled does not exceed submitted.
///
/// Chain invariant: an individual settlement fact is valid on its own;
/// the violation is visible only together with the preceding facts.
fn check_claim(
    submission: OfferSubmissionId,
    state: &SubmissionState,
    cancelled: Quantity,
    settled: Quantity,
) -> Result<(), OfferError> {
    let claimed = Quantity(cancelled.0.checked_add(settled.0)?);
    if claimed.0 > state.submitted.0 {
        return Err(OfferError::OverSettled {
            submission,
            submitted: state.submitted,
            claimed,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;

    fn qty(text: &str) -> Quantity {
        Quantity(Dec::new(Decimal::from_str_exact(text).unwrap()))
    }

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn submitted(submission: OfferSubmissionId, quantity: Quantity) -> OfferExerciseAction {
        OfferExerciseAction::Submitted {
            submission,
            window: OfferWindowId::new_random(),
            instrument: InstrumentId::new_random(),
            quantity,
        }
    }

    fn cancelled(submission: OfferSubmissionId, quantity: Quantity) -> OfferExerciseAction {
        OfferExerciseAction::Cancelled {
            submission,
            quantity,
        }
    }

    fn settled(submission: OfferSubmissionId, quantity: Quantity) -> OfferExerciseAction {
        OfferExerciseAction::Settled {
            submission,
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity,
            gross: rub(1_000_000),
            fee: None,
            accrued_interest: None,
        }
    }

    #[test]
    fn settlements_cannot_exceed_the_submitted_quantity() {
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&settled(submission, qty("6"))).unwrap();
        assert_eq!(
            book.apply(&settled(submission, qty("5"))).unwrap_err(),
            OfferError::OverSettled {
                submission,
                submitted: qty("10"),
                claimed: qty("11"),
            }
        );
    }

    #[test]
    fn a_partial_settlement_leaves_the_rest_outstanding() {
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&settled(submission, qty("6"))).unwrap();
        assert_eq!(book.outstanding(submission), qty("4"));
    }

    #[test]
    fn a_cancellation_frees_the_outstanding_quantity() {
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&cancelled(submission, qty("10"))).unwrap();
        assert_eq!(book.outstanding(submission), qty("0"));
    }

    #[test]
    fn a_cancellation_beyond_the_outstanding_quantity_is_refused() {
        // More than the amount remaining outstanding for the submission cannot be cancelled:
        // such a fact contradicts the chain.
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&settled(submission, qty("6"))).unwrap();
        assert_eq!(
            book.apply(&cancelled(submission, qty("5"))).unwrap_err(),
            OfferError::OverSettled {
                submission,
                submitted: qty("10"),
                claimed: qty("11"),
            }
        );
    }

    #[test]
    fn several_settlements_of_one_submission_accumulate() {
        // There may be several settlements for one submission: the agent settles
        // in parts, and each settlement is a separate fact.
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&settled(submission, qty("3"))).unwrap();
        book.apply(&settled(submission, qty("3"))).unwrap();
        assert_eq!(book.outstanding(submission), qty("4"));
    }

    #[test]
    fn a_settlement_of_a_submission_nobody_filed_is_refused() {
        // A settlement without a submission is not an offer, but an unrecorded disposal.
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        assert_eq!(
            book.apply(&settled(submission, qty("1"))).unwrap_err(),
            OfferError::UnknownSubmission { submission }
        );
    }

    #[test]
    fn the_same_submission_cannot_be_filed_twice() {
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        assert_eq!(
            book.apply(&submitted(submission, qty("10"))).unwrap_err(),
            OfferError::DuplicateSubmission { submission }
        );
    }

    #[test]
    fn a_non_positive_quantity_is_refused_everywhere() {
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        assert_eq!(
            book.apply(&submitted(submission, qty("0"))).unwrap_err(),
            OfferError::NonPositiveQuantity { submission }
        );
        book.apply(&submitted(submission, qty("10"))).unwrap();
        assert_eq!(
            book.apply(&settled(submission, qty("-1"))).unwrap_err(),
            OfferError::NonPositiveQuantity { submission }
        );
    }

    #[test]
    fn a_submission_nobody_filed_has_nothing_outstanding() {
        // `apply` does not accept facts about an unknown submission, so
        // an unknown identifier carries no outstanding claim.
        let book = OfferBook::default();
        assert_eq!(
            book.outstanding(OfferSubmissionId::new_random()),
            Quantity::zero()
        );
    }

    #[test]
    fn the_book_reports_the_whole_state_of_a_submission() {
        // Cancelled and settled are different outcomes, and the report
        // will need to distinguish them: a single remainder is not enough.
        let submission = OfferSubmissionId::new_random();
        let window = OfferWindowId::new_random();
        let instrument = InstrumentId::new_random();
        let mut book = OfferBook::default();
        book.apply(&OfferExerciseAction::Submitted {
            submission,
            window,
            instrument,
            quantity: qty("10"),
        })
        .unwrap();
        book.apply(&cancelled(submission, qty("1"))).unwrap();
        book.apply(&settled(submission, qty("6"))).unwrap();

        let state = book.submission(submission).expect("submission is known");
        assert_eq!(state.window, window);
        assert_eq!(state.instrument, instrument);
        assert_eq!(state.submitted, qty("10"));
        assert_eq!(state.cancelled, qty("1"));
        assert_eq!(state.settled, qty("6"));
        assert_eq!(state.outstanding(), qty("3"));
    }

    #[test]
    fn a_submission_nobody_filed_has_no_state_at_all() {
        // “No submission” and “nothing remains for the submission” are different things.
        let book = OfferBook::default();
        assert!(book.submission(OfferSubmissionId::new_random()).is_none());
    }

    #[test]
    fn the_book_survives_a_json_round_trip() {
        // The book is included in the projection snapshot alongside the others.
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&settled(submission, qty("6"))).unwrap();
        let text = serde_json::to_string(&book).unwrap();
        assert_eq!(serde_json::from_str::<OfferBook>(&text).unwrap(), book);
    }
    #[test]
    fn unresolved_submissions_names_only_windows_missing_from_schedule() {
        let known_window = OfferWindowId::new_random();
        let unknown_window = OfferWindowId::new_random();
        let known_submission = OfferSubmissionId::new_random();
        let unknown_submission = OfferSubmissionId::new_random();
        let instrument = InstrumentId::new_random();
        let mut book = OfferBook::default();
        book.apply(&OfferExerciseAction::Submitted {
            submission: known_submission,
            window: known_window,
            instrument,
            quantity: qty("1"),
        })
        .unwrap();
        book.apply(&OfferExerciseAction::Submitted {
            submission: unknown_submission,
            window: unknown_window,
            instrument,
            quantity: qty("1"),
        })
        .unwrap();
        let schedule = crate::bond::BondSchedule {
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window: known_window,
                right: crate::bond::OfferRight::HolderPut,
                execution_date: time::macros::date!(2026 - 12 - 01),
                submission_start: None,
                submission_end: None,
                price_percent: Some(Dec::one()),
            }],
            ..Default::default()
        };

        assert_eq!(
            unresolved_submissions(&book, &schedule),
            vec![unknown_submission]
        );
    }

    #[test]
    fn unresolved_submissions_does_not_guess_a_similar_window() {
        let recorded_window = OfferWindowId::new_random();
        let submitted_window = OfferWindowId::new_random();
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&OfferExerciseAction::Submitted {
            submission,
            window: submitted_window,
            instrument: InstrumentId::new_random(),
            quantity: qty("1"),
        })
        .unwrap();
        let schedule = crate::bond::BondSchedule {
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window: recorded_window,
                right: crate::bond::OfferRight::HolderPut,
                execution_date: time::macros::date!(2026 - 12 - 01),
                submission_start: None,
                submission_end: None,
                price_percent: Some(Dec::one()),
            }],
            ..Default::default()
        };

        assert_eq!(unresolved_submissions(&book, &schedule), vec![submission]);
    }
}
