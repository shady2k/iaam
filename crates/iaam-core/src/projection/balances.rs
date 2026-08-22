//! Денежные остатки и позиции (§3.1).
//!
//! Считаются **по ногам события**, единообразно для всех типов. Лоты
//! (`super::lots`) считаются по типу события и правилу списания. Две
//! независимые дороги к одному количеству — то, что делает инвариант
//! «сумма лотов равна позиции» проверкой, а не тавтологией (§15.4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::Event;
use crate::ids::{AccountId, CustodyId, EventId, InstrumentId};
use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
use crate::numeric::NumericError;

/// Позиция определяется тройкой: счёт, место хранения, инструмент.
/// Перевод бумаг между депозитариями внутри одного брокера — реальная
/// операция, поэтому custody входит в ключ (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PositionKey {
    pub account: AccountId,
    pub custody: Option<CustodyId>,
    pub instrument: InstrumentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BalanceError {
    #[error("переполнение денежного остатка на счёте {account:?} в {currency:?}")]
    CashOverflow {
        account: AccountId,
        currency: CurrencyCode,
    },
    #[error("нога события {event:?} несёт количество без инструмента")]
    QuantityWithoutInstrument { event: EventId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Остатки денег и бумаг.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balances {
    cash: BTreeMap<(AccountId, CurrencyCode), PostedMinor>,
    positions: BTreeMap<PositionKey, Quantity>,
}

impl Balances {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Применение одного события. Тело вынесено из цикла проекции,
    /// чтобы порядок обхода ног был виден и проверяем.
    pub fn apply(&mut self, event: &Event) -> Result<(), BalanceError> {
        for leg in &event.legs {
            if let Some(money) = leg.cash_effect() {
                let slot = self
                    .cash
                    .entry((leg.account, money.currency()))
                    .or_insert_with(|| PostedMinor::new(0));
                *slot = slot
                    .checked_add(money.amount())
                    .ok_or(BalanceError::CashOverflow {
                        account: leg.account,
                        currency: money.currency(),
                    })?;
            }
            if let Some(quantity) = leg.quantity {
                let instrument = leg
                    .instrument
                    .ok_or(BalanceError::QuantityWithoutInstrument { event: event.id })?;
                let key = PositionKey {
                    account: leg.account,
                    custody: leg.custody,
                    instrument,
                };
                let slot = self.positions.entry(key).or_insert_with(Quantity::zero);
                *slot = Quantity(slot.0.checked_add(quantity.0)?);
            }
        }
        Ok(())
    }

    /// Остаток счёта в валюте. `None` означает «движений не было»,
    /// а не «ноль»: разница видна в отчёте о полноте данных (§10.7).
    #[must_use]
    pub fn cash(&self, account: AccountId, currency: CurrencyCode) -> Option<Money> {
        self.cash
            .get(&(account, currency))
            .map(|amount| Money::new(*amount, currency))
    }

    pub fn iter_cash(&self) -> impl Iterator<Item = (AccountId, Money)> {
        self.cash
            .iter()
            .map(|((account, currency), amount)| (*account, Money::new(*amount, *currency)))
    }

    #[must_use]
    pub fn position(&self, key: &PositionKey) -> Option<Quantity> {
        self.positions.get(key).copied()
    }

    pub fn iter_positions(&self) -> impl Iterator<Item = (&PositionKey, Quantity)> {
        self.positions.iter().map(|(key, qty)| (key, *qty))
    }

    /// Суммарное количество инструмента на счёте по всем местам хранения.
    /// Именно это сравнивается с суммой лотов: лоты не различают custody.
    pub fn quantity_of(
        &self,
        account: AccountId,
        instrument: InstrumentId,
    ) -> Result<Quantity, NumericError> {
        self.positions
            .iter()
            .filter(|(key, _)| key.account == account && key.instrument == instrument)
            .try_fold(Quantity::zero().0, |acc, (_, qty)| acc.checked_add(qty.0))
            .map(Quantity)
    }

    /// Счета с отрицательным денежным остатком (§15.9).
    /// На этапе 1 это не ошибка: маржинальный минус — обязательство,
    /// которое обязано попасть в NAV, а не исчезнуть.
    pub fn negative_cash(&self) -> impl Iterator<Item = (AccountId, Money)> {
        self.iter_cash()
            .filter(|(_, money)| money.amount().raw() < 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::provenance::{ParserVersion, Provenance, RawHash};
    use crate::event::{Confidence, Event, Relation, SCHEMA_VERSION};
    use crate::ids::{CustodyId, EventId, OwnerId, SourceId};
    use crate::money::PostedMinor;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn cash_event(account: AccountId, amount: Money) -> Event {
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 01 - 10))),
            order: EffectiveOrder::new(date!(2026 - 01 - 10), 1),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"c".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    #[test]
    fn cash_legs_accumulate_per_account_and_currency() {
        let account = AccountId::new_random();
        let mut balances = Balances::new();
        balances.apply(&cash_event(account, rub(10_000))).unwrap();
        balances.apply(&cash_event(account, rub(2_500))).unwrap();
        assert_eq!(balances.cash(account, CurrencyCode::Rub), Some(rub(12_500)));
    }

    #[test]
    fn an_account_without_movements_is_not_a_zero_balance() {
        // Разница между «движений не было» и «остаток ноль» видна
        // в отчёте о полноте данных (§10.7), поэтому она в типе.
        let balances = Balances::new();
        assert_eq!(
            balances.cash(AccountId::new_random(), CurrencyCode::Rub),
            None
        );
    }

    #[test]
    fn negative_cash_is_reported_not_hidden() {
        let account = AccountId::new_random();
        let mut balances = Balances::new();
        balances.apply(&cash_event(account, rub(-5_000))).unwrap();
        let negative: Vec<_> = balances.negative_cash().collect();
        assert_eq!(negative, vec![(account, rub(-5_000))]);
    }

    #[test]
    fn quantity_sums_across_custodies_of_the_same_account() {
        // Лоты не различают место хранения, поэтому сравнивать с ними
        // надо сумму по всем custody, а не отдельную строку позиции.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut balances = Balances::new();
        for _ in 0..2 {
            let custody = CustodyId::new_random();
            let mut event = cash_event(account, rub(1));
            event.legs = vec![Leg::security(
                account,
                custody,
                instrument,
                Quantity(crate::numeric::decimal::Dec::new(10.into())),
            )];
            balances.apply(&event).unwrap();
        }
        assert_eq!(
            balances.quantity_of(account, instrument).unwrap(),
            Quantity(crate::numeric::decimal::Dec::new(20.into()))
        );
    }

    #[test]
    fn quantity_of_sums_neither_a_foreign_account_nor_a_foreign_instrument() {
        // Обе половины условия отбора обязаны действовать: с одной
        // из них сумма молча вбирала бы чужую позицию.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other_account = AccountId::new_random();
        let other_instrument = InstrumentId::new_random();

        let mut balances = Balances::new();
        let mut put = |acct: AccountId, inst: InstrumentId, qty: i64| {
            let mut event = cash_event(acct, rub(1));
            event.legs = vec![Leg::security(
                acct,
                CustodyId::new_random(),
                inst,
                Quantity(crate::numeric::decimal::Dec::new(qty.into())),
            )];
            balances.apply(&event).unwrap();
        };
        put(account, instrument, 3);
        put(account, other_instrument, 40);
        put(other_account, instrument, 500);

        assert_eq!(
            balances.quantity_of(account, instrument).unwrap(),
            Quantity(crate::numeric::decimal::Dec::new(3.into()))
        );
    }

    #[test]
    fn a_quantity_leg_without_an_instrument_is_an_error() {
        let account = AccountId::new_random();
        let mut event = cash_event(account, rub(1));
        event.legs = vec![Leg {
            kind: crate::event::leg::LegKind::SecurityQuantity,
            account,
            custody: None,
            instrument: None,
            money: None,
            quantity: Some(Quantity::zero()),
        }];
        let mut balances = Balances::new();
        assert!(matches!(
            balances.apply(&event),
            Err(BalanceError::QuantityWithoutInstrument { .. })
        ));
    }
    #[test]
    fn a_position_is_addressed_by_account_custody_and_instrument() {
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let mut balances = Balances::new();
        let mut event = cash_event(account, rub(1));
        event.legs = vec![Leg::security(
            account,
            custody,
            instrument,
            Quantity(crate::numeric::decimal::Dec::new(7.into())),
        )];
        balances.apply(&event).unwrap();

        let key = PositionKey {
            account,
            custody: Some(custody),
            instrument,
        };
        assert_eq!(
            balances.position(&key),
            Some(Quantity(crate::numeric::decimal::Dec::new(7.into())))
        );
        // Другое место хранения — другая позиция, а не та же.
        assert_eq!(
            balances.position(&PositionKey {
                custody: Some(CustodyId::new_random()),
                ..key
            }),
            None
        );
        assert_eq!(balances.iter_positions().count(), 1);
    }

    #[test]
    fn a_zero_balance_is_not_a_negative_one() {
        // Граница: ноль обязательством не является, и в блок качества
        // данных попадать не должен.
        let account = AccountId::new_random();
        let mut balances = Balances::new();
        balances.apply(&cash_event(account, rub(5_000))).unwrap();
        balances.apply(&cash_event(account, rub(-5_000))).unwrap();
        assert_eq!(balances.cash(account, CurrencyCode::Rub), Some(rub(0)));
        assert_eq!(balances.negative_cash().count(), 0);
    }
}
