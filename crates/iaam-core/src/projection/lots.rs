//! Книга лотов (§4.12).
//!
//! Лоты строятся **по типу события**: покупка добавляет партию, продажа
//! списывает её версионированным правилом из реестра. Количество бумаг
//! при этом считается независимо — по ногам события (`super::balances`).
//!
//! Восстановленная позиция без документированной стоимости (§10.7)
//! **не превращается в лот с нулевой стоимостью**: она хранится отдельным
//! количеством, списывается первой и делает реализованный результат
//! невычислимым. Нулевая заглушка здесь означала бы выдуманную прибыль,
//! равную всей выручке.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::Event;
use crate::event::kind::{EventKind, TradeSide};
use crate::ids::{AccountId, EventId, InstrumentId};
use crate::money::{Money, MoneyError, Quantity};
use crate::numeric::NumericError;
use crate::rules::lot_disposal::{
    DisposalError, DisposalInput, DisposalResult, Lot, LotId, PrincipalState, RuleId,
};
use crate::rules::{LotRuleVersion, RuleRegistry};

/// Лоты не различают место хранения: перевод бумаги между депозитариями
/// не является приобретением и не создаёт новой партии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LotKey {
    pub account: AccountId,
    pub instrument: InstrumentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LotError {
    #[error("в реестре нет правила списания версии {version:?}")]
    UnknownRule { version: LotRuleVersion },
    #[error("продажа {event:?} без предшествующей позиции по инструменту {instrument:?}")]
    SaleWithoutPosition {
        event: EventId,
        instrument: InstrumentId,
    },
    #[error("книга лотов ещё не применяет факт {kind} события {event:?}")]
    NotYetApplied { event: EventId, kind: &'static str },
    #[error(transparent)]
    Disposal(#[from] DisposalError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Почему реализованный результат по инструменту не вычисляется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisGap {
    /// Позиция восстановлена без документированной стоимости (§10.7).
    RestoredWithoutBasis,
}

impl BasisGap {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RestoredWithoutBasis => "restored_without_basis",
        }
    }
}

/// Лоты одного инструмента на одном счёте.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentLots {
    /// Количество, восстановленное без стоимости. Списывается первым:
    /// оно приобретено раньше всего, что система видела.
    unpriced: Quantity,
    /// Партии в порядке приобретения.
    lots: Vec<Lot>,
    /// Реализованный результат до налога. `None`, если хотя бы одно
    /// выбытие затронуло количество без стоимости.
    realized: Option<Money>,
    /// Суммарная стоимость всех приобретений с документированной ценой.
    acquired_basis: Option<Money>,
    /// Суммарная стоимость, списанная при выбытиях.
    released_basis: Option<Money>,
    gap: Option<BasisGap>,
}

/// Пустая книга по инструменту. Пишется вручную, потому что `Quantity`
/// намеренно не реализует `Default`: нулевое количество должно возникать
/// осознанно, а не как значение по умолчанию неизвестного поля (§4.9).
impl Default for InstrumentLots {
    fn default() -> Self {
        Self {
            unpriced: Quantity::zero(),
            lots: Vec::new(),
            realized: None,
            acquired_basis: None,
            released_basis: None,
            gap: None,
        }
    }
}

impl InstrumentLots {
    #[must_use]
    pub fn lots(&self) -> &[Lot] {
        &self.lots
    }

    #[must_use]
    pub const fn unpriced(&self) -> Quantity {
        self.unpriced
    }

    #[must_use]
    pub const fn realized(&self) -> Option<Money> {
        self.realized
    }

    #[must_use]
    pub const fn gap(&self) -> Option<BasisGap> {
        self.gap
    }

    /// Стоимость приобретений. Вместе с [`Self::released_basis`] образует
    /// проверяемое тождество: приобретено = осталось + списано.
    #[must_use]
    pub const fn acquired_basis(&self) -> Option<Money> {
        self.acquired_basis
    }

    #[must_use]
    pub const fn released_basis(&self) -> Option<Money> {
        self.released_basis
    }

    /// Стоимость непроданных партий.
    pub fn remaining_basis(&self) -> Result<Option<Money>, MoneyError> {
        let Some(first) = self.lots.first() else {
            return Ok(None);
        };
        let amounts: Vec<Money> = self.lots.iter().map(|lot| lot.cost_basis).collect();
        Money::sum(&amounts, first.cost_basis.currency()).map(Some)
    }

    /// Суммарное количество: партии плюс восстановленный остаток.
    pub fn quantity(&self) -> Result<Quantity, NumericError> {
        self.lots
            .iter()
            .try_fold(self.unpriced.0, |acc, lot| acc.checked_add(lot.quantity.0))
            .map(Quantity)
    }
}

/// Факты сделки, нужные книге лотов. Отдельная структура, а не восемь
/// аргументов: порог `too-many-arguments-threshold = 6` в `clippy.toml`
/// действует, а подавлять линт запрещено (§15.7).
#[derive(Debug, Clone, Copy, PartialEq)]
struct TradeFacts {
    side: TradeSide,
    instrument: InstrumentId,
    quantity: Quantity,
    gross: Money,
    fee: Option<Money>,
}

/// Книга лотов и применённое правило.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LotBook {
    entries: BTreeMap<LotKey, InstrumentLots>,
    rule_version: LotRuleVersion,
    applied_rule: Option<RuleId>,
}

impl LotBook {
    #[must_use]
    pub fn new(rule_version: LotRuleVersion) -> Self {
        Self {
            entries: BTreeMap::new(),
            rule_version,
            applied_rule: None,
        }
    }

    #[must_use]
    pub const fn rule_version(&self) -> LotRuleVersion {
        self.rule_version
    }

    /// Идентификатор правила, которым фактически списывались лоты.
    /// Входит в отчёт и в след аудита: без него цифру не воспроизвести (§3.2).
    #[must_use]
    pub const fn applied_rule(&self) -> Option<&RuleId> {
        self.applied_rule.as_ref()
    }

    #[must_use]
    pub fn entry(&self, key: &LotKey) -> Option<&InstrumentLots> {
        self.entries.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&LotKey, &InstrumentLots)> {
        self.entries.iter()
    }

    /// Применение события к книге лотов.
    ///
    /// Диспетчер исчерпывающий: новый тип события обязан сломать сборку
    /// здесь, а не молча не создать лот.
    pub fn apply(&mut self, event: &Event, rules: &RuleRegistry) -> Result<(), LotError> {
        match &event.kind {
            EventKind::Trade {
                side,
                instrument,
                quantity,
                gross,
                fee,
                // НКД не участвует в стоимости приобретения — см. `apply_trade`.
                accrued_interest: _,
            } => self.apply_trade(
                event,
                TradeFacts {
                    side: *side,
                    instrument: *instrument,
                    quantity: *quantity,
                    gross: *gross,
                    fee: *fee,
                },
                rules,
            ),
            // Утверждения восстановленного начала (§10.7) книгу лотов
            // не меняют: они описывают, насколько можно верить
            // количеству и стоимости, а не сами величины. Читает их
            // отчёт о качестве данных.
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis,
                assertions: _,
            } => self.restore(event, *instrument, *quantity, *cost_basis),
            EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Income { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. } => Ok(()),
            // Промежуточное состояние: типы факта уже включены
            // в `EventKind`, применение появляется в E3.4.1.T9.
            // Отказ, а не `Ok(())`: принять факт и молча его не
            // применить — ровно то, чего этот эпик не допускает.
            EventKind::CorporateAction { .. } | EventKind::OfferExercise { .. } => {
                Err(LotError::NotYetApplied {
                    event: event.id,
                    kind: event.kind.discriminant(),
                })
            }
        }
    }

    /// Стоимость приобретения включает комиссию и **не включает НКД**:
    /// накопленный купонный доход возвращается купоном, а не продажей,
    /// поэтому он не является стоимостью бумаги (§7.2). Налоговая
    /// стоимость по ст. 214.1 считается иначе и появится в E5 —
    /// поэтому она и версионирована правилом.
    fn apply_trade(
        &mut self,
        event: &Event,
        trade: TradeFacts,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let TradeFacts {
            side,
            instrument,
            quantity,
            gross,
            fee,
        } = trade;
        let key = LotKey {
            account: event.account,
            instrument,
        };
        match side {
            TradeSide::Buy => {
                let basis = match fee {
                    Some(f) => gross.try_add(f)?,
                    None => gross,
                };
                let entry = self.entries.entry(key).or_default();
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.lots.push(Lot {
                    // Идентификатор лота выводится из события приобретения:
                    // ядро чисто, случайных идентификаторов в нём быть не может,
                    // иначе повторная проекция того же журнала дала бы другой
                    // результат (§3.1, §15.3).
                    id: LotId(event.id.inner()),
                    instrument,
                    acquired: event.dates.trade,
                    quantity,
                    cost_basis: basis,
                    // Номинал сюда придёт из справочника в E3.4;
                    // подставлять ноль запрещено (§4.9).
                    principal: PrincipalState::Unknown,
                });
                Ok(())
            }
            TradeSide::Sell => {
                let proceeds = match fee {
                    Some(f) => gross.try_sub(f)?,
                    None => gross,
                };
                self.dispose(event, key, quantity, proceeds, rules)
            }
        }
    }

    fn restore(
        &mut self,
        event: &Event,
        instrument: InstrumentId,
        quantity: Quantity,
        cost_basis: Option<Money>,
    ) -> Result<(), LotError> {
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let entry = self.entries.entry(key).or_default();
        match cost_basis {
            // Восстановленная партия старше всего, что система видела,
            // поэтому встаёт в голову очереди FIFO, а не в хвост.
            Some(basis) => {
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.lots.insert(
                    0,
                    Lot {
                        id: LotId(event.id.inner()),
                        instrument,
                        acquired: event.dates.trade,
                        quantity,
                        cost_basis: basis,
                        principal: PrincipalState::Unknown,
                    },
                );
            }
            None => {
                entry.unpriced = Quantity(entry.unpriced.0.checked_add(quantity.0)?);
                entry.gap = Some(BasisGap::RestoredWithoutBasis);
            }
        }
        Ok(())
    }

    fn dispose(
        &mut self,
        event: &Event,
        key: LotKey,
        quantity: Quantity,
        proceeds: Money,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let rule = rules
            .disposal_rule(self.rule_version)
            .ok_or(LotError::UnknownRule {
                version: self.rule_version,
            })?;
        let entry = self
            .entries
            .get_mut(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: key.instrument,
            })?;

        // Восстановленное количество списывается первым: оно приобретено
        // раньше всего, что система наблюдала. Стоимости у него нет,
        // поэтому реализованный результат по инструменту становится
        // невычислимым — но количество списывается честно.
        let from_unpriced = entry.unpriced.0.min(quantity.0);
        if !from_unpriced.is_zero() {
            entry.unpriced = Quantity(entry.unpriced.0.checked_sub(from_unpriced)?);
            entry.realized = None;
            entry.gap = Some(BasisGap::RestoredWithoutBasis);
        }
        let left = quantity.0.checked_sub(from_unpriced)?;
        if left.is_zero() {
            return Ok(());
        }

        let result: DisposalResult = rule.apply(&DisposalInput {
            lots: entry.lots.clone(),
            quantity: Quantity(left),
        })?;
        entry.lots = result.remaining.clone();
        entry.released_basis = Some(match entry.released_basis {
            Some(previous) => previous.try_add(result.basis_released)?,
            None => result.basis_released,
        });
        self.applied_rule = Some(result.rule.clone());

        // Реализованный результат до налога: выручка минус списанная
        // стоимость. Он не суммируется с невычислимым: один разрыв делает
        // невычислимым весь инструмент, а не «почти всё».
        if entry.gap.is_none() {
            let realized = proceeds.try_sub(result.basis_released)?;
            entry.realized = Some(match entry.realized {
                Some(previous) => previous.try_add(realized)?,
                None => realized,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::CustodyId;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;
    use time::Date;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    struct Trade {
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        units: i64,
        gross: i64,
    }

    fn buy(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(-(trade.gross + 10_000));
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(trade.units),
                ),
            ],
        )
    }

    fn sell(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(trade.gross - 10_000);
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(-trade.units),
                ),
            ],
        )
    }

    fn key(trade: &Trade) -> LotKey {
        LotKey {
            account: trade.account,
            instrument: trade.instrument,
        }
    }

    fn sample_trade() -> Trade {
        Trade {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
            day: date!(2025 - 03 - 01),
            units: 100,
            gross: 1_000_000,
        }
    }

    #[test]
    fn a_purchase_creates_a_lot_including_the_fee() {
        // Комиссия входит в стоимость приобретения; НКД — нет (§7.2).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots().len(), 1);
        assert_eq!(entry.lots()[0].cost_basis, rub(1_010_000));
        assert_eq!(entry.quantity().unwrap(), qty(100));
    }

    #[test]
    fn lot_identity_comes_from_the_acquisition_event_not_from_randomness() {
        // Ядро чисто: повторная проекция того же журнала обязана дать
        // те же идентификаторы лотов, иначе снимки несравнимы (§3.1).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let event = buy(&trade, 1);
        let mut first = LotBook::new(LotRuleVersion(1));
        let mut second = LotBook::new(LotRuleVersion(1));
        first.apply(&event, &rules).unwrap();
        second.apply(&event, &rules).unwrap();
        assert_eq!(
            first.entry(&key(&trade)).unwrap().lots()[0].id,
            second.entry(&key(&trade)).unwrap().lots()[0].id
        );
    }

    #[test]
    fn a_partial_sale_releases_basis_and_records_realized_result() {
        // Куплено 100 за 1 010 000, продано 40 за 500 000 минус комиссия.
        // Списанная стоимость: 1 010 000 * 40 / 100 = 404 000.
        // Реализовано: 490 000 − 404 000 = 86 000.
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let partial = Trade {
            units: 40,
            gross: 500_000,
            day: date!(2025 - 06 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(60));
        assert_eq!(entry.released_basis(), Some(rub(404_000)));
        assert_eq!(entry.realized(), Some(rub(86_000)));
        assert_eq!(
            book.applied_rule().map(|r| r.0.as_str()),
            Some("fifo/214.1/v1")
        );
    }

    #[test]
    fn remaining_basis_completes_the_identity_acquired_equals_remaining_plus_released() {
        // Тождество §6.3 в его денежной части. Ожидания посчитаны вручную:
        // куплено 100 за 1 010 000, продано 40 — списано 404 000,
        // значит на непроданных 60 бумагах осталось 606 000.
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        assert_eq!(
            book.entry(&key(&trade)).and_then(|entry| entry
                .remaining_basis()
                .expect("пустая книга не считает стоимость")),
            None
        );

        book.apply(&buy(&trade, 1), &rules).unwrap();
        let partial = Trade {
            units: 40,
            gross: 500_000,
            day: date!(2025 - 06 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(606_000)));
        assert_eq!(
            entry.acquired_basis(),
            Some(rub(1_010_000)),
            "приобретено = осталось + списано"
        );
        assert_eq!(entry.released_basis(), Some(rub(404_000)));
    }

    #[test]
    fn a_restored_position_without_basis_does_not_become_a_zero_cost_lot() {
        // Нулевая стоимость означала бы прибыль, равную всей выручке (§4.9).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        book.apply(&restored, &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert!(entry.lots().is_empty());
        assert_eq!(entry.unpriced(), qty(50));
        assert_eq!(entry.gap(), Some(BasisGap::RestoredWithoutBasis));

        // Продажа из восстановленного количества уменьшает позицию,
        // но реализованный результат остаётся невычислимым.
        let partial = Trade {
            units: 20,
            gross: 300_000,
            day: date!(2025 - 02 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.unpriced(), qty(30));
        assert_eq!(entry.realized(), None);
    }

    #[test]
    fn selling_an_instrument_never_held_is_an_error() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        assert!(matches!(
            book.apply(&sell(&trade, 1), &rules),
            Err(LotError::SaleWithoutPosition { .. })
        ));
    }

    #[test]
    fn an_unknown_rule_version_is_an_error_not_a_silent_fallback() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(99));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        assert!(matches!(
            book.apply(&sell(&trade, 2), &rules),
            Err(LotError::UnknownRule { .. })
        ));
    }
    #[test]
    fn the_book_exposes_its_entries_and_the_cost_of_acquisitions() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();

        assert_eq!(book.iter().count(), 1, "книга обязана отдавать записи");
        let (found_key, entry) = book.iter().next().unwrap();
        assert_eq!(*found_key, key(&trade));
        // Приобретено = тело + комиссия; вместе со списанным образует
        // проверяемое тождество сохранения стоимости.
        assert_eq!(entry.acquired_basis(), Some(rub(1_010_000)));
        assert_eq!(entry.released_basis(), None);
        assert_eq!(book.rule_version(), LotRuleVersion(1));
    }

    #[test]
    fn the_basis_gap_has_a_machine_readable_code() {
        // Код уходит в API: агент разбирает его, а не текст.
        assert_eq!(
            BasisGap::RestoredWithoutBasis.code(),
            "restored_without_basis"
        );
    }
}
