//! Cash balances and positions (§3.1).
//!
//! Calculated **by event legs**, uniformly for all types. Lots
//! (`super::lots`) are calculated by event type and disposal rule. Two
//! independent paths to the same quantity are what make the invariant
//! “the sum of lots equals the position” a check rather than a tautology (§15.4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::Event;
use crate::ids::{AccountId, EventId, InstrumentId};
use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
use crate::numeric::NumericError;

/// A position is defined by a pair: account and instrument.
///
/// An account already says which broker and which account there. A place of
/// custody adds nothing to that identity — the owner keeps positions at the
/// level of the broker and of the account, not below it, and that is a
/// settled product question, not an engineering preference. Custody stays
/// elsewhere, as description (`Leg.custody` and the like); it is not part of
/// this key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PositionKey {
    pub account: AccountId,
    pub instrument: InstrumentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BalanceError {
    #[error("cash balance overflow for account {account:?} in {currency:?}")]
    CashOverflow {
        account: AccountId,
        currency: CurrencyCode,
    },
    #[error("event leg {event:?} carries a quantity without an instrument")]
    QuantityWithoutInstrument { event: EventId },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Cash and securities balances.
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

    /// Applies a single event. The body is extracted from the projection loop
    /// so that the leg traversal order is visible and verifiable.
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
                    instrument,
                };
                let slot = self.positions.entry(key).or_insert_with(Quantity::zero);
                *slot = Quantity(slot.0.checked_add(quantity.0)?);
            }
        }
        Ok(())
    }

    /// Account balance in the currency. `None` means “there were no movements”,
    /// not “zero”: the distinction is visible in the data completeness report (§10.7).
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

    /// The quantity held under one key, account and instrument.
    ///
    /// A single lookup and not a sum: since custody left the key, there is at
    /// most one entry per account and instrument, so summing across custody
    /// (what this method used to do, under the name `quantity_of`) and
    /// looking the position up directly are the same operation. Two names for
    /// one answer is worse than one, so only one remains.
    #[must_use]
    pub fn position(&self, key: &PositionKey) -> Option<Quantity> {
        self.positions.get(key).copied()
    }

    pub fn iter_positions(&self) -> impl Iterator<Item = (&PositionKey, Quantity)> {
        self.positions.iter().map(|(key, qty)| (key, *qty))
    }

    /// Accounts with a negative cash balance (§15.9).
    /// At stage 1 this is not an error: a negative margin balance is a liability
    /// that must be included in NAV rather than disappear.
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
    use crate::event::{Confidence, Event, Relation};
    use crate::ids::{CustodyId, EventId, OwnerId, SourceId};
    use crate::money::PostedMinor;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn cash_event(account: AccountId, amount: Money) -> Event {
        Event {
            id: EventId::new_random(),
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
        // The distinction between “there were no movements” and “the balance is zero” is visible
        // in the data completeness report (§10.7), so it is represented in the type.
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
    fn two_trades_with_different_custody_are_one_position() {
        // The owner keeps positions at the level of the broker and of the
        // account, not below it: custody is not part of the key, so two legs
        // for the same account and instrument land in one row regardless of
        // which depository each named.
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
        let key = PositionKey {
            account,
            instrument,
        };
        assert_eq!(
            balances.position(&key),
            Some(Quantity(crate::numeric::decimal::Dec::new(20.into())))
        );
        assert_eq!(balances.iter_positions().count(), 1);
    }

    #[test]
    fn a_position_is_addressed_by_neither_a_foreign_account_nor_a_foreign_instrument() {
        // Both halves of the key must apply: without either of them, a
        // lookup would silently return an unrelated position.
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
            balances.position(&PositionKey {
                account,
                instrument
            }),
            Some(Quantity(crate::numeric::decimal::Dec::new(3.into())))
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
    fn a_position_recorded_with_no_custody_is_still_a_position() {
        // A position an API sync produced can carry no custody at all: the
        // key no longer needs one to exist.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut balances = Balances::new();
        let mut event = cash_event(account, rub(1));
        event.legs = vec![Leg {
            kind: crate::event::leg::LegKind::SecurityQuantity,
            account,
            custody: None,
            instrument: Some(instrument),
            money: None,
            quantity: Some(Quantity(crate::numeric::decimal::Dec::new(7.into()))),
        }];
        balances.apply(&event).unwrap();

        let key = PositionKey {
            account,
            instrument,
        };
        assert_eq!(
            balances.position(&key),
            Some(Quantity(crate::numeric::decimal::Dec::new(7.into())))
        );
        assert_eq!(balances.iter_positions().count(), 1);
    }

    #[test]
    fn a_zero_balance_is_not_a_negative_one() {
        // Boundary: zero is not a liability and must not be included
        // in the data quality block.
        let account = AccountId::new_random();
        let mut balances = Balances::new();
        balances.apply(&cash_event(account, rub(5_000))).unwrap();
        balances.apply(&cash_event(account, rub(-5_000))).unwrap();
        assert_eq!(balances.cash(account, CurrencyCode::Rub), Some(rub(0)));
        assert_eq!(balances.negative_cash().count(), 0);
    }
}
