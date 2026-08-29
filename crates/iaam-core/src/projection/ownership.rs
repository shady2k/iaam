//! Вывод владения из диапазона возможного количества.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::money::Quantity;
use crate::numeric::decimal::Dec;
use crate::settlement::{Applied, SettlementKnowledge};

/// Статус владения на календарную дату.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    NotOwned,
    Unknown,
}

/// Одно изменение количества и знание о дате его расчёта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct OwnershipChange {
    delta: Quantity,
    settlement: SettlementKnowledge,
}

/// История владения парой «счёт, инструмент».
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipHistory {
    changes: Vec<OwnershipChange>,
}

impl OwnershipHistory {
    /// Добавить изменение количества с известной степенью точности расчёта.
    pub fn observe(&mut self, delta: Quantity, settlement: SettlementKnowledge) {
        self.changes.push(OwnershipChange { delta, settlement });
    }

    /// Вывести владение из минимального и максимального возможного остатка.
    ///
    /// Ошибка возможна только в сторону лишнего `Unknown`: неопределённое
    /// событие расширяет диапазон, а не позволяет получить ложные `Owned`
    /// или `NotOwned`.
    #[must_use]
    pub fn ownership_at(&self, day: Date) -> Ownership {
        let mut minimum = Dec::zero();
        let mut maximum = Dec::zero();
        for change in &self.changes {
            match change.settlement.applied_before(day) {
                Applied::Yes => {
                    let Ok(next_minimum) = minimum.checked_add(change.delta.0) else {
                        return Ownership::Unknown;
                    };
                    let Ok(next_maximum) = maximum.checked_add(change.delta.0) else {
                        return Ownership::Unknown;
                    };
                    minimum = next_minimum;
                    maximum = next_maximum;
                }
                Applied::No => {}
                Applied::Maybe => {
                    if change.delta.0.is_negative() {
                        let Ok(next_minimum) = minimum.checked_add(change.delta.0) else {
                            return Ownership::Unknown;
                        };
                        minimum = next_minimum;
                    } else {
                        let Ok(next_maximum) = maximum.checked_add(change.delta.0) else {
                            return Ownership::Unknown;
                        };
                        maximum = next_maximum;
                    }
                }
            }
        }

        if minimum.is_positive() {
            Ownership::Owned
        } else if maximum.is_zero() {
            Ownership::NotOwned
        } else {
            Ownership::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use crate::settlement::SettlementKnowledge;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn quantity(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    #[test]
    fn overlapping_settlement_windows_make_ownership_unknown_without_crossing_zero_in_journal() {
        // Даже при журнальном переходе 1→2→1 продажа могла рассчитаться
        // первой, поэтому на 11 и 12 марта фактический остаток мог быть нулём.
        let mut history = OwnershipHistory::default();
        history.observe(
            quantity(1),
            SettlementKnowledge::Exact(date!(2026 - 03 - 01)),
        );
        history.observe(
            quantity(1),
            SettlementKnowledge::Bounded {
                earliest: date!(2026 - 03 - 10),
                latest: date!(2026 - 03 - 12),
            },
        );
        history.observe(
            quantity(-1),
            SettlementKnowledge::Bounded {
                earliest: date!(2026 - 03 - 11),
                latest: date!(2026 - 03 - 13),
            },
        );

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 05)),
            Ownership::Owned
        );
        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 11)),
            Ownership::Unknown
        );
        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 12)),
            Ownership::Unknown
        );
    }

    #[test]
    fn exact_settlement_has_no_false_boundary_at_the_settlement_date() {
        // На дате расчёта закрытый интервал даёт Maybe, а не придуманное
        // начало или конец внутридневного перехода прав.
        let date = date!(2026 - 03 - 10);
        let mut history = OwnershipHistory::default();
        history.observe(quantity(1), SettlementKnowledge::Exact(date));

        assert_eq!(
            history.ownership_at(date.previous_day().unwrap()),
            Ownership::NotOwned
        );
        assert_eq!(history.ownership_at(date), Ownership::Unknown);
        assert_eq!(
            history.ownership_at(date.next_day().unwrap()),
            Ownership::Owned
        );
    }

    #[test]
    fn exact_sale_is_owned_before_and_not_owned_after_its_settlement() {
        // Точная продажа закрывает владение только после даты её расчёта.
        let bought = date!(2026 - 03 - 01);
        let sold = date!(2026 - 03 - 10);
        let mut history = OwnershipHistory::default();
        history.observe(quantity(1), SettlementKnowledge::Exact(bought));
        history.observe(quantity(-1), SettlementKnowledge::Exact(sold));

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 09)),
            Ownership::Owned
        );
        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 11)),
            Ownership::NotOwned
        );
    }

    #[test]
    fn small_unbounded_sale_does_not_hide_a_large_exact_residual() {
        // Неограниченно датированная продажа 10 оставляет минимум 90
        // после точного приобретения 100, поэтому результат остаётся Owned.
        let mut history = OwnershipHistory::default();
        history.observe(
            quantity(100),
            SettlementKnowledge::Exact(date!(2026 - 03 - 01)),
        );
        history.observe(quantity(-10), SettlementKnowledge::Unbounded);

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 11)),
            Ownership::Owned
        );
    }

    #[test]
    fn quantity_bounds_use_decimal_checked_arithmetic() {
        // Переполнение нельзя превратить в правдоподобное владение: безопасный
        // отказ — Unknown, потому что точный диапазон больше не доказан.
        let mut history = OwnershipHistory::default();
        history.observe(
            Quantity(Dec::new(Decimal::MAX)),
            SettlementKnowledge::Exact(date!(2026 - 03 - 01)),
        );
        history.observe(
            quantity(1),
            SettlementKnowledge::Exact(date!(2026 - 03 - 02)),
        );

        assert_eq!(
            history.ownership_at(date!(2026 - 03 - 03)),
            Ownership::Unknown
        );
    }
}
