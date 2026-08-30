//! Typed movement legs (§4.3).
//!
//! An event is split into legs rather than stored as a single total:
//! otherwise principal amortization, accrued interest, and fee allocation
//! cannot be recovered from the recorded fact.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, CustodyId, InstrumentId};
use crate::money::{Money, Quantity};

/// What exactly is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegKind {
    /// Movement of cash in an account.
    Cash,
    /// Movement of securities quantity.
    SecurityQuantity,
    /// Movement of outstanding principal (amortization).
    Principal,
    /// Fee.
    Fee,
    /// Tax.
    Tax,
}

/// One movement leg.
///
/// The sign sets the direction: positive — inflow to the specified account
/// or custody, negative — outflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leg {
    pub kind: LegKind,
    pub account: AccountId,
    pub custody: Option<CustodyId>,
    pub instrument: Option<InstrumentId>,
    pub money: Option<Money>,
    pub quantity: Option<Quantity>,
}

impl Leg {
    #[must_use]
    pub const fn cash(account: AccountId, money: Money) -> Self {
        Self {
            kind: LegKind::Cash,
            account,
            custody: None,
            instrument: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[must_use]
    pub const fn security(
        account: AccountId,
        custody: CustodyId,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Self {
        Self {
            kind: LegKind::SecurityQuantity,
            account,
            custody: Some(custody),
            instrument: Some(instrument),
            money: None,
            quantity: Some(quantity),
        }
    }

    #[must_use]
    pub const fn fee(account: AccountId, money: Money) -> Self {
        Self {
            kind: LegKind::Fee,
            account,
            custody: None,
            instrument: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[must_use]
    pub const fn tax(account: AccountId, money: Money) -> Self {
        Self {
            kind: LegKind::Tax,
            account,
            custody: None,
            instrument: None,
            money: Some(money),
            quantity: None,
        }
    }

    #[must_use]
    pub const fn principal(account: AccountId, instrument: InstrumentId, money: Money) -> Self {
        Self {
            kind: LegKind::Principal,
            account,
            custody: None,
            instrument: Some(instrument),
            money: Some(money),
            quantity: None,
        }
    }

    /// The leg's cash value, if any.
    /// Fees and taxes are also cash: they reduce the cash balance.
    #[must_use]
    pub const fn cash_effect(&self) -> Option<Money> {
        match self.kind {
            LegKind::Cash | LegKind::Fee | LegKind::Tax | LegKind::Principal => self.money,
            LegKind::SecurityQuantity => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    #[test]
    fn cash_leg_carries_money_and_no_instrument() {
        let account = AccountId::new_random();
        let leg = Leg::cash(account, rub(-5_000));
        assert_eq!(leg.kind, LegKind::Cash);
        assert_eq!(leg.account, account);
        assert_eq!(leg.money, Some(rub(-5_000)));
        assert_eq!(leg.custody, None);
        assert_eq!(leg.instrument, None);
        assert_eq!(leg.quantity, None);
    }

    #[test]
    fn security_leg_carries_custody_instrument_and_quantity() {
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let leg = Leg::security(account, custody, instrument, Quantity::zero());
        assert_eq!(leg.kind, LegKind::SecurityQuantity);
        assert_eq!(leg.account, account);
        assert_eq!(leg.custody, Some(custody));
        assert_eq!(leg.instrument, Some(instrument));
        assert_eq!(leg.quantity, Some(Quantity::zero()));
        assert_eq!(leg.money, None);
    }

    #[test]
    fn fee_leg_is_money_without_an_instrument() {
        let leg = Leg::fee(AccountId::new_random(), rub(-3_500));
        assert_eq!(leg.kind, LegKind::Fee);
        assert_eq!(leg.money, Some(rub(-3_500)));
        assert_eq!(leg.instrument, None);
    }

    #[test]
    fn tax_leg_is_money_without_an_instrument() {
        let leg = Leg::tax(AccountId::new_random(), rub(-1_300));
        assert_eq!(leg.kind, LegKind::Tax);
        assert_eq!(leg.money, Some(rub(-1_300)));
        assert_eq!(leg.instrument, None);
        assert_eq!(leg.custody, None);
    }

    #[test]
    fn principal_leg_names_the_instrument_being_amortised() {
        let instrument = InstrumentId::new_random();
        let leg = Leg::principal(AccountId::new_random(), instrument, rub(100_000));
        assert_eq!(leg.kind, LegKind::Principal);
        assert_eq!(leg.instrument, Some(instrument));
        assert_eq!(leg.money, Some(rub(100_000)));
        assert_eq!(leg.quantity, None);
    }

    #[test]
    fn security_leg_has_no_cash_effect() {
        let leg = Leg::security(
            AccountId::new_random(),
            CustodyId::new_random(),
            InstrumentId::new_random(),
            Quantity::zero(),
        );
        assert!(leg.cash_effect().is_none());
    }

    #[test]
    fn fee_leg_counts_as_cash_effect() {
        let m = Money::new(PostedMinor::new(-35), CurrencyCode::Rub);
        let leg = Leg::fee(AccountId::new_random(), m);
        assert_eq!(leg.cash_effect(), Some(m));
    }

    #[test]
    fn every_money_bearing_kind_counts_as_cash_effect() {
        // Tax and principal amortization also reduce or increase
        // the cash balance: skipping them means losing part of the movement.
        let account = AccountId::new_random();
        let m = rub(-1_300);
        assert_eq!(Leg::cash(account, m).cash_effect(), Some(m));
        assert_eq!(Leg::tax(account, m).cash_effect(), Some(m));
        assert_eq!(
            Leg::principal(account, InstrumentId::new_random(), m).cash_effect(),
            Some(m)
        );
    }
}
