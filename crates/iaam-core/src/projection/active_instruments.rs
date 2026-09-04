//! Projection of nonzero positions from a journal slice.
//!
//! The shell needs the result only to select the instruments that should be
//! synchronized. This is a set of identifiers, not a reporting figure:
//! the function does not calculate or publish monetary or other totals.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::event::Event;
use crate::event::corporate_action::CorporateAction;
use crate::event::correction::{CorrectionError, resolve};
use crate::event::kind::{EventKind, TradeSide};
use crate::event::offer::OfferExerciseAction;
use crate::ids::InstrumentId;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActiveInstrumentsError {
    #[error(transparent)]
    Numeric(#[from] NumericError),
    #[error(transparent)]
    Correction(#[from] CorrectionError),
}

/// Negating the quantity. A negation failure is propagated to the caller because
/// “cannot compute” must not be replaced with the original quantity.
fn negated(quantity: Dec) -> Result<Dec, NumericError> {
    quantity.checked_neg()
}

/// Instruments whose resulting quantity is not zero.
///
/// This is a pure projection of core events: the shell uses it only to
/// select instruments for synchronization. Quantity accumulation overflow is
/// an explicit failure because continuing with the old value loses the delta and
/// produces an incorrect set of active instruments.
pub fn active_instruments(
    events: &[Event],
) -> Result<BTreeSet<InstrumentId>, ActiveInstrumentsError> {
    let effective = resolve(events)?;
    let mut quantities = BTreeMap::<InstrumentId, Dec>::new();
    for event in effective {
        // A list of pairs, not a single pair: replacement moves the quantity across
        // two securities at once, and reducing it to one would leave the
        // predecessor permanently active.
        let deltas: Vec<(InstrumentId, Dec)> = match &event.kind {
            EventKind::Trade {
                side,
                instrument,
                quantity,
                ..
            } => vec![(
                *instrument,
                match side {
                    TradeSide::Buy => quantity.0,
                    TradeSide::Sell => negated(quantity.0)?,
                },
            )],
            EventKind::OpeningPosition {
                instrument,
                quantity,
                ..
            } => vec![(*instrument, quantity.0)],
            EventKind::CorporateAction { action } => match action {
                // Amortisation pays out money but does not change the number
                // of securities (§6.5): a zero delta, not an omission.
                CorporateAction::PartialRedemption { instrument, .. } => {
                    vec![(*instrument, Dec::zero())]
                }
                CorporateAction::Redemption {
                    instrument,
                    quantity,
                    ..
                } => vec![(*instrument, negated(quantity.0)?)],
                CorporateAction::Conversion {
                    predecessor,
                    successor,
                    quantity_in,
                    quantity_out,
                    ..
                } => vec![
                    (*predecessor, negated(quantity_in.0)?),
                    (*successor, quantity_out.0),
                ],
            },
            EventKind::OfferExercise { action } => match action {
                // Placing and canceling an order do not move securities.
                OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => {
                    Vec::new()
                }
                OfferExerciseAction::Settled {
                    instrument,
                    quantity,
                    ..
                } => vec![(*instrument, negated(quantity.0)?)],
            },
            EventKind::CashIn { .. }
            | EventKind::Refund { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::OwnAccountMovement { .. }
            | EventKind::UnresolvedOwnAccountMovement { .. }
            | EventKind::Income { .. }
            | EventKind::Fee { .. }
            | EventKind::Tax { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. }
            | EventKind::ImportCoverageGap { .. } => Vec::new(),
        };
        for (instrument, delta) in deltas {
            let current = quantities.entry(instrument).or_insert_with(Dec::zero);
            *current = (*current).checked_add(delta)?;
        }
    }
    Ok(quantities
        .into_iter()
        .filter_map(|(instrument, quantity)| (!quantity.is_zero()).then_some(instrument))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use crate::event::kind::OpeningAssertions;
    use crate::event::provenance::{ParserVersion, Provenance, RawHash};
    use crate::event::{Confidence, Relation, SCHEMA_VERSION};
    use crate::ids::{AccountId, EventId, OwnerId, SourceId};
    use crate::money::Quantity;
    use crate::numeric::NumericError;
    use rust_decimal::Decimal;
    use time::macros::date;

    #[test]
    fn a_reversed_trade_does_not_leave_an_active_instrument() {
        let instrument = InstrumentId::new_random();
        let trade = opening(instrument, Dec::one());
        let mut reversal = trade.clone();
        reversal.id = EventId::new_random();
        reversal.relation = Relation::Reversal { target: trade.id };

        let active = active_instruments(&[trade, reversal]).unwrap();

        assert!(!active.contains(&instrument));
    }
    #[test]
    fn a_correction_failure_is_reported_by_active_instruments() {
        let mut orphan = opening(InstrumentId::new_random(), Dec::one());
        orphan.relation = Relation::Reversal {
            target: EventId::new_random(),
        };

        assert!(matches!(
            active_instruments(&[orphan]),
            Err(ActiveInstrumentsError::Correction(
                CorrectionError::DanglingTarget { .. }
            ))
        ));
    }

    fn opening(instrument: InstrumentId, quantity: Dec) -> Event {
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            kind: EventKind::OpeningPosition {
                instrument,
                quantity: Quantity(quantity),
                cost_basis: None,
                assertions: OpeningAssertions::default(),
            },
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 01 - 01))),
            order: EffectiveOrder::new(date!(2026 - 01 - 01), 0),
            legs: Vec::new(),
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"a".repeat(64)).expect("valid test hash"),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    #[test]
    fn reports_quantity_overflow_instead_of_returning_stale_set() {
        let instrument = InstrumentId::new_random();
        let events = [
            opening(instrument, Dec::new(Decimal::MAX)),
            opening(instrument, Dec::one()),
        ];

        assert_eq!(
            active_instruments(&events),
            Err(ActiveInstrumentsError::Numeric(NumericError::Overflow))
        );
    }
}
