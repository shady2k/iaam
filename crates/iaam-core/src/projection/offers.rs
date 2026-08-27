//! Книга заявок по оферте (§3.5).
//!
//! Отдельная проекция, а не поля [`super::lots::LotBook`]: заявка живёт
//! своей цепочкой «подал — отозвал — рассчитались», её состояние не
//! является свойством партии приобретения, и снимок книги лотов
//! от появления оферты не меняется.
//!
//! Инвариант «исполнено плюс отозвано не больше поданного» — свойство
//! **цепочки**, а не одного факта, поэтому живёт здесь, а не
//! в структурной проверке события.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use crate::ids::InstrumentId;
use crate::money::Quantity;
use crate::numeric::NumericError;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OfferError {
    #[error("по заявке {submission:?} заявлено {claimed:?} при поданных {submitted:?}")]
    OverSettled {
        submission: OfferSubmissionId,
        submitted: Quantity,
        claimed: Quantity,
    },
    #[error("заявка {submission:?} не подавалась")]
    UnknownSubmission { submission: OfferSubmissionId },
    #[error("заявка {submission:?} подана повторно")]
    DuplicateSubmission { submission: OfferSubmissionId },
    #[error("по заявке {submission:?} количество не положительно")]
    NonPositiveQuantity { submission: OfferSubmissionId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Состояние одной заявки.
///
/// Хранятся три накопителя, а не один остаток: отозванное и
/// исполненное — разные исходы, и различать их придётся отчёту.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmissionState {
    pub window: OfferWindowId,
    pub instrument: InstrumentId,
    pub submitted: Quantity,
    pub cancelled: Quantity,
    pub settled: Quantity,
}

impl SubmissionState {
    /// Сколько по заявке ещё не решено.
    ///
    /// Инвариант `cancelled + settled <= submitted` держит [`OfferBook::apply`],
    /// поэтому разность не уходит ни в минус, ни в переполнение.
    /// `debug_assert` ловит нарушение инварианта в тестах; в релизе
    /// остаётся ноль — величина, которая не соблазнит вызывающего
    /// действовать по испорченному состоянию.
    #[must_use]
    pub fn outstanding(&self) -> Quantity {
        let claimed = self.cancelled.0.checked_add(self.settled.0);
        let left = claimed.and_then(|claimed| self.submitted.0.checked_sub(claimed));
        debug_assert!(
            left.is_ok_and(|left| !left.is_negative()),
            "инвариант заявки нарушен: отозвано плюс исполнено больше поданного"
        );
        left.map_or_else(|_| Quantity::zero(), Quantity)
    }
}

/// Книга заявок по оферте.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OfferBook {
    submissions: BTreeMap<OfferSubmissionId, SubmissionState>,
}

impl OfferBook {
    /// Применить факт оферты к книге.
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
                    // Повторная подача под тем же идентификатором —
                    // не вторая заявка, а потерянная первая.
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

    /// Сколько по заявке ещё не решено. У заявки, которой не подавали,
    /// непогашенного требования нет: `apply` не принимает фактов
    /// о неизвестной заявке, поэтому и требованию взяться неоткуда.
    #[must_use]
    pub fn outstanding(&self, submission: OfferSubmissionId) -> Quantity {
        self.submissions
            .get(&submission)
            .map_or_else(Quantity::zero, SubmissionState::outstanding)
    }

    /// Состояние заявки целиком.
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

/// Отозванное плюс исполненное не превосходит поданного.
///
/// Инвариант цепочки: отдельный факт расчёта сам по себе безупречен,
/// нарушение видно только вместе с предыдущими.
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
        // Отозвать больше, чем осталось непогашенным по заявке, нельзя:
        // такой факт противоречит цепочке.
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
        // Расчётов по одной заявке бывает несколько: агент рассчитывает
        // частями, и каждый расчёт — отдельный факт.
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&settled(submission, qty("3"))).unwrap();
        book.apply(&settled(submission, qty("3"))).unwrap();
        assert_eq!(book.outstanding(submission), qty("4"));
    }

    #[test]
    fn a_settlement_of_a_submission_nobody_filed_is_refused() {
        // Расчёт без заявки — не оферта, а неучтённое выбытие.
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
        // `apply` не принимает фактов о неизвестной заявке, поэтому
        // неизвестный идентификатор не несёт непогашенного требования.
        let book = OfferBook::default();
        assert_eq!(
            book.outstanding(OfferSubmissionId::new_random()),
            Quantity::zero()
        );
    }

    #[test]
    fn the_book_reports_the_whole_state_of_a_submission() {
        // Отозванное и исполненное — разные исходы, и различать их
        // придётся отчёту: одного остатка для этого мало.
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

        let state = book.submission(submission).expect("заявка известна");
        assert_eq!(state.window, window);
        assert_eq!(state.instrument, instrument);
        assert_eq!(state.submitted, qty("10"));
        assert_eq!(state.cancelled, qty("1"));
        assert_eq!(state.settled, qty("6"));
        assert_eq!(state.outstanding(), qty("3"));
    }

    #[test]
    fn a_submission_nobody_filed_has_no_state_at_all() {
        // «Нет заявки» и «по заявке ничего не осталось» — разные вещи.
        let book = OfferBook::default();
        assert!(book.submission(OfferSubmissionId::new_random()).is_none());
    }

    #[test]
    fn the_book_survives_a_json_round_trip() {
        // Книга попадает в снимок проекции наравне с остальными.
        let submission = OfferSubmissionId::new_random();
        let mut book = OfferBook::default();
        book.apply(&submitted(submission, qty("10"))).unwrap();
        book.apply(&settled(submission, qty("6"))).unwrap();
        let text = serde_json::to_string(&book).unwrap();
        assert_eq!(serde_json::from_str::<OfferBook>(&text).unwrap(), book);
    }
}
