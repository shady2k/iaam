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
use crate::numeric::decimal::Dec;

/// Смена знака количества. `i64::MIN`-подобного случая у `Dec` нет,
/// но отказ отрицания молча оставил бы количество прежним, поэтому
/// он вынесен в одно место.
fn negated(quantity: Dec) -> Dec {
    quantity.checked_neg().unwrap_or(quantity)
}

/// Инструменты, у которых итоговое количество не равно нулю.
///
/// Это чистая проекция событий ядра: оболочка использует её только для
/// выбора инструментов синхронизации. Результат — множество идентификаторов,
/// а не отчётное число, поэтому функция не создаёт второго источника чисел.
#[must_use]
pub fn active_instruments(events: &[Event]) -> BTreeSet<InstrumentId> {
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
                    TradeSide::Sell => negated(quantity.0),
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
                } => vec![(*instrument, negated(quantity.0))],
                CorporateAction::Conversion {
                    predecessor,
                    successor,
                    quantity_in,
                    quantity_out,
                    ..
                } => vec![
                    (*predecessor, negated(quantity_in.0)),
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
                } => vec![(*instrument, negated(quantity.0))],
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
            *current = current.checked_add(delta).unwrap_or(*current);
        }
    }
    quantities
        .into_iter()
        .filter_map(|(instrument, quantity)| (!quantity.is_zero()).then_some(instrument))
        .collect()
}
