//! Проекция ненулевых позиций из среза журнала.
//!
//! Результат нужен оболочке только для выбора инструментов, которые следует
//! синхронизировать. Это множество идентификаторов, а не отчётное число:
//! функция не рассчитывает и не публикует денежные или иные итоговые суммы.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::Event;
use crate::event::corporate_action::CorporateAction;
use crate::event::kind::{EventKind, TradeSide};
use crate::event::offer::OfferExerciseAction;
use crate::ids::InstrumentId;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Смена знака количества. Отказ отрицания передаётся вызывающему, потому
/// что «невозможно вычислить» нельзя подменить исходным количеством.
fn negated(quantity: Dec) -> Result<Dec, NumericError> {
    quantity.checked_neg()
}

/// Инструменты, у которых итоговое количество не равно нулю.
///
/// Это чистая проекция событий ядра: оболочка использует её только для
/// выбора инструментов синхронизации. Переполнение накопления количества —
/// явный отказ, потому что продолжение со старым значением теряет дельту и
/// создаёт неверное множество активных инструментов.
pub fn active_instruments(events: &[Event]) -> Result<BTreeSet<InstrumentId>, NumericError> {
    let mut quantities = BTreeMap::<InstrumentId, Dec>::new();
    for event in events {
        // Список пар, а не одна пара: замещение двигает количество сразу
        // по двум бумагам, и свести его к одной значило бы оставить
        // предшественника вечно активным.
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
                // Амортизация выплачивает деньги, но количество бумаг
                // не меняет (§6.5): нулевая дельта, а не пропуск.
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
                // Подача и отзыв заявки бумаг не двигают.
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
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Income { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. } => Vec::new(),
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

        assert_eq!(active_instruments(&events), Err(NumericError::Overflow),);
    }
}
