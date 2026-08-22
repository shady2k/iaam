//! Инварианты как исполняемый код (§15.2).
//!
//! Нарушение инварианта — **не** то же самое, что неполные данные.
//! Неполнота даёт нормальный результат плюс блок качества данных;
//! нарушение инварианта отменяет отчёт целиком: возвращать число
//! с предупреждением после доказанного нарушения тождества нельзя.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::lots::LotKey;
use super::state::LedgerState;
use crate::event::{Event, EventValidationError};
use crate::ids::EventId;
use crate::money::Money;
use crate::numeric::NumericError;

/// Проверенный инвариант. Отчёт показывает, что именно было проверено:
/// «инварианты выполнены» без перечисления неотличимо от «не проверялось».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckedInvariant {
    /// Структура каждого события соответствует его типу.
    EventStructure { events: usize },
    /// Сумма лотов равна позиции по каждому инструменту.
    LotsMatchPositions { pairs: usize },
    /// Приобретено = осталось + списано, в минимальных единицах, точно.
    BasisConserved { pairs: usize },
    /// Ни один внешний поток не имеет нулевой суммы.
    FlowsNonZero { flows: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvariantViolation {
    #[error("событие {event:?} не проходит структурную проверку: {source}")]
    EventStructure {
        event: EventId,
        #[source]
        source: EventValidationError,
    },
    #[error(
        "сумма лотов по {key:?} равна {lots}, позиция по ногам событий — {position}; \
         две независимые дороги к одному количеству разошлись"
    )]
    LotsDoNotMatchPosition {
        key: LotKey,
        lots: String,
        position: String,
    },
    #[error(
        "стоимость по {key:?} не сохраняется: приобретено {acquired}, \
         осталось {remaining}, списано {released}"
    )]
    BasisNotConserved {
        key: LotKey,
        acquired: i64,
        remaining: i64,
        released: i64,
    },
    #[error("внешний поток события {event:?} имеет нулевую сумму")]
    ZeroExternalFlow { event: EventId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

impl InvariantViolation {
    /// Машиночитаемый код. Нарушение инварианта попадает в лог
    /// с идентификатором корреляции, а наружу уходит `not_computable` (§15.2).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EventStructure { .. } => "event_structure",
            Self::LotsDoNotMatchPosition { .. } => "lots_do_not_match_position",
            Self::BasisNotConserved { .. } => "basis_not_conserved",
            Self::ZeroExternalFlow { .. } => "zero_external_flow",
            Self::Numeric(_) => "numeric",
        }
    }
}

/// Отчёт о проверенных инвариантах.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantReport {
    checked: Vec<CheckedInvariant>,
}

impl InvariantReport {
    #[must_use]
    pub fn checked(&self) -> &[CheckedInvariant] {
        &self.checked
    }
}

/// Проверка всех инвариантов состояния.
///
/// Ядро не доверяет входу: события уже проверялись при записи, но
/// проекция строится и по данным, пришедшим из хранилища, а хранилище
/// могли наполнить в обход приёмки.
pub fn check(
    state: &LedgerState,
    events: &[&Event],
) -> Result<InvariantReport, InvariantViolation> {
    let mut checked = Vec::new();

    for event in events {
        event
            .validate_structure()
            .map_err(|source| InvariantViolation::EventStructure {
                event: event.id,
                source,
            })?;
    }
    checked.push(CheckedInvariant::EventStructure {
        events: events.len(),
    });

    let mut pairs = 0;
    let mut basis_pairs = 0;
    for (key, entry) in state.book().iter() {
        let lots = entry.quantity()?;
        let position = state.balances().quantity_of(key.account, key.instrument)?;
        if lots != position {
            return Err(InvariantViolation::LotsDoNotMatchPosition {
                key: *key,
                lots: format!("{:?}", lots.0.inner()),
                position: format!("{:?}", position.0.inner()),
            });
        }
        pairs += 1;

        if let Some(acquired) = entry.acquired_basis() {
            let remaining = entry
                .remaining_basis()
                .map_err(|_| InvariantViolation::BasisNotConserved {
                    key: *key,
                    acquired: acquired.amount().raw(),
                    remaining: 0,
                    released: 0,
                })?
                .unwrap_or_else(|| Money::zero(acquired.currency()));
            let released = entry
                .released_basis()
                .unwrap_or_else(|| Money::zero(acquired.currency()));
            let sum = remaining.amount().raw() + released.amount().raw();
            if sum != acquired.amount().raw() {
                return Err(InvariantViolation::BasisNotConserved {
                    key: *key,
                    acquired: acquired.amount().raw(),
                    remaining: remaining.amount().raw(),
                    released: released.amount().raw(),
                });
            }
            basis_pairs += 1;
        }
    }
    checked.push(CheckedInvariant::LotsMatchPositions { pairs });
    checked.push(CheckedInvariant::BasisConserved { pairs: basis_pairs });

    for flow in state.flows().external() {
        if flow.amount.is_zero() {
            return Err(InvariantViolation::ZeroExternalFlow { event: flow.event });
        }
    }
    checked.push(CheckedInvariant::FlowsNonZero {
        flows: state.flows().external().len(),
    });

    Ok(InvariantReport { checked })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourDefinition, ContourId, ContourVersion};
    use crate::event::kind::{EventKind, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::projection::{ProjectionContext, project};
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    /// Отчёт обязан перечислять, ЧТО проверено, и с какими количествами:
    /// «инварианты выполнены» без чисел неотличимо от «не проверялось».
    #[test]
    fn the_report_names_what_was_checked_and_how_much() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = vec![
            event_with(
                account,
                date!(2025 - 01 - 01),
                1,
                EventKind::CashIn {
                    amount: rub(10_000_000),
                },
                vec![Leg::cash(account, rub(10_000_000))],
            ),
            event_with(
                account,
                date!(2025 - 02 - 01),
                2,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross: rub(900_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(account, rub(-900_000)),
                    Leg::security(account, CustodyId::new_random(), instrument, qty(100)),
                ],
            ),
        ];

        let projection = project(&events, &ctx).unwrap();
        let report = projection.invariants();
        assert!(!report.checked().is_empty());
        assert_eq!(
            report.checked(),
            &[
                CheckedInvariant::EventStructure { events: 2 },
                CheckedInvariant::LotsMatchPositions { pairs: 1 },
                CheckedInvariant::BasisConserved { pairs: 1 },
                CheckedInvariant::FlowsNonZero { flows: 1 },
            ]
        );
    }

    #[test]
    fn basis_is_conserved_across_a_partial_sale_and_not_merely_when_nothing_is_sold() {
        // Тождество «приобретено = осталось + списано» проверяется там,
        // где обе части ненулевые. Пока ничего не продано, списанная
        // стоимость равна нулю, и сумма неотличима от разности:
        // испорченный знак прошёл бы незамеченным.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let custody = CustodyId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2025 - 01 - 01),
                1,
                EventKind::CashIn {
                    amount: rub(10_000_000),
                },
                vec![Leg::cash(account, rub(10_000_000))],
            ),
            event_with(
                account,
                date!(2025 - 02 - 01),
                2,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross: rub(1_000_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(account, rub(-1_000_000)),
                    Leg::security(account, custody, instrument, qty(100)),
                ],
            ),
            event_with(
                account,
                date!(2025 - 03 - 01),
                3,
                EventKind::Trade {
                    side: TradeSide::Sell,
                    instrument,
                    quantity: qty(40),
                    gross: rub(500_000),
                    fee: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(account, rub(500_000)),
                    Leg::security(account, custody, instrument, qty(-40)),
                ],
            ),
        ];

        let projection = project(&events, &ctx).unwrap();
        // Обе части ненулевые и различны: приобретено 1 000 000,
        // списано 400 000, осталось 600 000. Числа посчитаны вручную.
        let entry = projection
            .snapshot
            .state()
            .book()
            .entry(&crate::projection::lots::LotKey {
                account,
                instrument,
            })
            .unwrap();
        assert_eq!(entry.acquired_basis(), Some(rub(1_000_000)));
        assert_eq!(entry.released_basis(), Some(rub(400_000)));
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(600_000)));
        assert!(
            projection
                .invariants()
                .checked()
                .contains(&CheckedInvariant::BasisConserved { pairs: 1 })
        );
    }

    #[test]
    fn an_empty_journal_still_reports_what_it_checked() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let projection = project(
            &[],
            &ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        )
        .unwrap();
        assert_eq!(
            projection.invariants().checked(),
            &[
                CheckedInvariant::EventStructure { events: 0 },
                CheckedInvariant::LotsMatchPositions { pairs: 0 },
                CheckedInvariant::BasisConserved { pairs: 0 },
                CheckedInvariant::FlowsNonZero { flows: 0 },
            ]
        );
    }

    #[test]
    fn every_violation_has_a_machine_readable_code() {
        let key = super::LotKey {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
        };
        assert_eq!(
            InvariantViolation::LotsDoNotMatchPosition {
                key,
                lots: "1".into(),
                position: "2".into(),
            }
            .code(),
            "lots_do_not_match_position"
        );
        assert_eq!(
            InvariantViolation::BasisNotConserved {
                key,
                acquired: 1,
                remaining: 0,
                released: 0,
            }
            .code(),
            "basis_not_conserved"
        );
        assert_eq!(
            InvariantViolation::ZeroExternalFlow {
                event: crate::ids::EventId::new_random(),
            }
            .code(),
            "zero_external_flow"
        );
        assert_eq!(
            InvariantViolation::Numeric(crate::numeric::NumericError::Overflow).code(),
            "numeric"
        );
    }
}
