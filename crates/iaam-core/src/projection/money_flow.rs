//! The flow of money across and inside the contour (spec §2).
//!
//! This module answers a household question — what came in, what went out,
//! what the capital earned, what moved into assets — and it deliberately does
//! **not** reclassify any event to do so. `flows.rs` answers a different
//! question, about contributions and withdrawals for the returns path, and
//! `EventKind::Income` is `WithinAccount` there for a correct reason: a coupon
//! is not a new contribution of capital. Moving it would corrupt XIRR.
//!
//! So the two projections read the same journal from two angles. The quantity
//! that makes this honest is `residual`: the difference between the cash the
//! contour actually gained and the six quantities that claim to explain it. A
//! non-zero residual is reported, never absorbed.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::contour::{ContourDefinition, FlowClass, classify};
use crate::event::Event;
use crate::event::kind::EventKind;
use crate::event::leg::LegKind;
use crate::ids::{AccountId, EventId};
use crate::money::{CurrencyCode, Money, PostedMinor};

/// The interval a report covers, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateWindow {
    pub from: Date,
    pub to: Date,
}

impl DateWindow {
    /// Inclusive at both ends: a report for August includes the 1st and the 31st.
    #[must_use]
    pub fn covers(&self, on: Date) -> bool {
        self.from <= on && on <= self.to
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MoneyFlowError {
    #[error("event {event:?} moves money inside the window but has no date")]
    MovementWithoutDate { event: EventId },
    #[error("overflow while summing {quantity} for event {event:?}")]
    Overflow {
        quantity: &'static str,
        event: EventId,
    },
}

/// Seven quantities and the cash they claim to explain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyFlow {
    came_in: Ledger,
    went_out: Ledger,
    earned_by_capital: Ledger,
    moved_into_assets: Ledger,
    fees: Ledger,
    taxes: Ledger,
    internal_transfers: Ledger,
    cash_delta: Ledger,
}

/// Amounts kept per account **and** per currency.
///
/// Per currency, because currencies are never silently added. Per account,
/// because §2 requires the residual to name the account it belongs to: a
/// contour-wide zero built from one account short and another long is the
/// worst possible report — it looks correct and is wrong twice.
type Ledger = BTreeMap<(AccountId, CurrencyCode), PostedMinor>;

impl MoneyFlow {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one event.
    ///
    /// `cash_delta` is accumulated from the legs, the same way `Balances` does
    /// it, so the two can never drift apart. The explanatory quantities are
    /// accumulated from the event kind and the leg kind. Their disagreement is
    /// what `residual` reports.
    pub fn apply(
        &mut self,
        event: &Event,
        contour: &ContourDefinition,
        window: DateWindow,
    ) -> Result<(), MoneyFlowError> {
        let flow_class = classify(contour, event);
        if !self.event_belongs(event, contour, window, flow_class)? {
            return Ok(());
        }

        for leg in &event.legs {
            if !contour.contains(leg.account) {
                continue;
            }
            let Some(money) = leg.cash_effect() else {
                continue;
            };
            add(&mut self.cash_delta, leg.account, money, "cash_delta", event.id)?;

            match leg.kind {
                LegKind::Fee => {
                    let amount = negated(money, "fees", event.id)?;
                    add(&mut self.fees, leg.account, amount, "fees", event.id)?;
                }
                LegKind::Tax => {
                    let amount = negated(money, "taxes", event.id)?;
                    add(&mut self.taxes, leg.account, amount, "taxes", event.id)?;
                }
                LegKind::Cash => match (&event.kind, flow_class) {
                    (EventKind::Trade { .. }, _) => {
                        let amount = negated(money, "moved_into_assets", event.id)?;
                        add(
                            &mut self.moved_into_assets,
                            leg.account,
                            amount,
                            "moved_into_assets",
                            event.id,
                        )?;
                    }
                    (EventKind::Income { .. }, _) => {
                        add(
                            &mut self.earned_by_capital,
                            leg.account,
                            money,
                            "earned_by_capital",
                            event.id,
                        )?;
                    }
                    (EventKind::CashIn { .. }, _) => {
                        add(&mut self.came_in, leg.account, money, "came_in", event.id)?;
                    }
                    (EventKind::CashOut { .. }, _) => {
                        let amount = negated(money, "went_out", event.id)?;
                        add(&mut self.went_out, leg.account, amount, "went_out", event.id)?;
                    }
                    (EventKind::CashTransfer { .. }, FlowClass::Internal) => {
                        if money.amount().raw() > 0 {
                            add(
                                &mut self.internal_transfers,
                                leg.account,
                                money,
                                "internal_transfers",
                                event.id,
                            )?;
                        }
                    }
                    (EventKind::CashTransfer { .. }, FlowClass::ExternalIn { .. }) => {
                        add(&mut self.came_in, leg.account, money, "came_in", event.id)?;
                    }
                    (EventKind::CashTransfer { .. }, FlowClass::ExternalOut { .. }) => {
                        let amount = negated(money, "went_out", event.id)?;
                        add(&mut self.went_out, leg.account, amount, "went_out", event.id)?;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    }

    fn event_belongs(
        &self,
        event: &Event,
        contour: &ContourDefinition,
        window: DateWindow,
        flow_class: FlowClass,
    ) -> Result<bool, MoneyFlowError> {
        if matches!(flow_class, FlowClass::Irrelevant) {
            return Ok(false);
        }
        let Some(on) = event.dates.effective_date() else {
            if has_contour_cash_leg(event, contour) {
                return Err(MoneyFlowError::MovementWithoutDate { event: event.id });
            }
            return Ok(false);
        };
        Ok(window.covers(on))
    }

    #[must_use]
    pub fn came_in(&self, currency: CurrencyCode) -> Money {
        total(&self.came_in, currency)
    }

    #[must_use]
    pub fn went_out(&self, currency: CurrencyCode) -> Money {
        total(&self.went_out, currency)
    }

    #[must_use]
    pub fn earned_by_capital(&self, currency: CurrencyCode) -> Money {
        total(&self.earned_by_capital, currency)
    }

    #[must_use]
    pub fn moved_into_assets(&self, currency: CurrencyCode) -> Money {
        total(&self.moved_into_assets, currency)
    }

    #[must_use]
    pub fn fees(&self, currency: CurrencyCode) -> Money {
        total(&self.fees, currency)
    }

    #[must_use]
    pub fn taxes(&self, currency: CurrencyCode) -> Money {
        total(&self.taxes, currency)
    }

    #[must_use]
    pub fn internal_transfers(&self, currency: CurrencyCode) -> Money {
        total(&self.internal_transfers, currency)
    }

    #[must_use]
    pub fn cash_delta(&self, currency: CurrencyCode) -> Money {
        total(&self.cash_delta, currency)
    }

    /// The currencies present in any quantity or cash delta.
    #[must_use]
    pub fn currencies(&self) -> impl Iterator<Item = CurrencyCode> + '_ {
        let mut currencies = BTreeSet::new();
        for ledger in self.ledgers() {
            currencies.extend(ledger.keys().map(|(_, currency)| *currency));
        }
        currencies.into_iter()
    }

    /// The cash the six quantities fail to explain, for one account.
    ///
    /// Zero means the report closes there. Non-zero is a defect and is shown as
    /// one: a report that quietly absorbs its residual is how
    /// `Saved <redacted>` came to mean nothing.
    #[must_use]
    fn residual_of(&self, account: AccountId, currency: CurrencyCode) -> i64 {
        let at = |ledger: &Ledger| {
            ledger
                .get(&(account, currency))
                .map_or(0, |amount| amount.raw())
        };
        let explained = at(&self.came_in)
            .checked_sub(at(&self.went_out))
            .and_then(|amount| amount.checked_add(at(&self.earned_by_capital)))
            .and_then(|amount| amount.checked_sub(at(&self.moved_into_assets)))
            .and_then(|amount| amount.checked_sub(at(&self.fees)))
            .and_then(|amount| amount.checked_sub(at(&self.taxes)))
            .expect("money-flow residual arithmetic overflow");
        at(&self.cash_delta)
            .checked_sub(explained)
            .expect("money-flow residual arithmetic overflow")
    }

    /// The contour-wide residual in a currency.
    #[must_use]
    pub fn residual(&self, currency: CurrencyCode) -> Money {
        let total = self.accounts().into_iter().fold(0i64, |total, account| {
            total
                .checked_add(self.residual_of(account, currency))
                .expect("money-flow residual arithmetic overflow")
        });
        Money::new(PostedMinor::new(total), currency)
    }

    /// Every account that does not close, with what it owes.
    ///
    /// Reported separately from `residual` on purpose. Two accounts wrong in
    /// opposite directions sum to zero, and a report that showed only the total
    /// would call that success while being wrong twice.
    #[must_use]
    pub fn residuals_by_account(&self) -> Vec<(AccountId, Money)> {
        let mut rows = Vec::new();
        for account in self.accounts() {
            for currency in self.currencies() {
                let residual = self.residual_of(account, currency);
                if residual != 0 {
                    rows.push((account, Money::new(PostedMinor::new(residual), currency)));
                }
            }
        }
        rows
    }

    fn accounts(&self) -> BTreeSet<AccountId> {
        self.ledgers()
            .into_iter()
            .flat_map(|ledger| ledger.keys().map(|(account, _)| *account))
            .collect()
    }

    fn ledgers(&self) -> [&Ledger; 8] {
        [
            &self.came_in,
            &self.went_out,
            &self.earned_by_capital,
            &self.moved_into_assets,
            &self.fees,
            &self.taxes,
            &self.internal_transfers,
            &self.cash_delta,
        ]
    }
}

fn has_contour_cash_leg(event: &Event, contour: &ContourDefinition) -> bool {
    event
        .legs
        .iter()
        .any(|leg| contour.contains(leg.account) && leg.cash_effect().is_some())
}

fn add(
    ledger: &mut Ledger,
    account: AccountId,
    money: Money,
    quantity: &'static str,
    event: EventId,
) -> Result<(), MoneyFlowError> {
    let slot = ledger
        .entry((account, money.currency()))
        .or_insert_with(|| PostedMinor::new(0));
    *slot = slot
        .checked_add(money.amount())
        .ok_or(MoneyFlowError::Overflow { quantity, event })?;
    Ok(())
}

fn negated(
    money: Money,
    quantity: &'static str,
    event: EventId,
) -> Result<Money, MoneyFlowError> {
    let amount = money
        .amount()
        .checked_neg()
        .ok_or(MoneyFlowError::Overflow { quantity, event })?;
    Ok(Money::new(amount, money.currency()))
}

fn total(ledger: &Ledger, currency: CurrencyCode) -> Money {
    let amount = ledger
        .iter()
        .filter(|((_, item_currency), _)| *item_currency == currency)
        .fold(PostedMinor::new(0), |total, (_, amount)| {
            total
                .checked_add(*amount)
                .expect("money-flow total arithmetic overflow")
        });
    Money::new(amount, currency)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourDefinition, ContourId, ContourVersion};
    use crate::event::Event;
    use crate::event::kind::{EventKind, TaxOrigin, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, InstrumentId, TransferId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use time::{Date, macros::date};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn august() -> DateWindow {
        DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        }
    }

    /// Builds an event dated inside August with the given kind and legs.
    ///
    /// Wraps the crate's existing constructor rather than rewriting the event
    /// envelope: a hand-written envelope silently diverges from the real one,
    /// and the test then tests the fixture instead of the code — the reason
    /// `test_support` exists at all.
    fn event(kind: EventKind, legs: Vec<Leg>, on: Date) -> Event {
        let account = legs.first().expect("at least one leg").account;
        event_with(account, on, 1, kind, legs)
    }

    #[test]
    fn an_internal_transfer_is_neither_income_nor_expense() {
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![card, deposit],
        );
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashTransfer {
                    transfer_id: TransferId::new_random(),
                    from: card,
                    to: deposit,
                    amount: rub(480_000),
                },
                vec![Leg::cash(card, rub(-480_000)), Leg::cash(deposit, rub(480_000))],
                date!(2026 - 08 - 10),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.went_out(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.internal_transfers(CurrencyCode::Rub), rub(480_000));
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn a_coupon_is_earned_by_the_capital_and_not_an_inflow() {
        let broker = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Income {
                    instrument: Some(InstrumentId::new_random()),
                    gross: rub(31_000),
                    kind: None,
                },
                vec![Leg::cash(broker, rub(31_000))],
                date!(2026 - 08 - 15),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.earned_by_capital(CurrencyCode::Rub), rub(31_000));
        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(31_000));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn a_purchase_moves_money_into_assets_and_its_fee_is_counted_once() {
        let broker = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: Quantity::zero(),
                    gross: rub(-100_000),
                    fee: Some(rub(-350)),
                    basis_fee: None,
                    basis_fee_exact: None,
                    accrued_interest: None,
                },
                vec![Leg::cash(broker, rub(-100_000)), Leg::fee(broker, rub(-350))],
                date!(2026 - 08 - 20),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.moved_into_assets(CurrencyCode::Rub), rub(100_000));
        assert_eq!(flow.fees(CurrencyCode::Rub), rub(350));
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(-100_350));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn a_self_paid_tax_is_not_ordinary_spending() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Tax {
                    amount: rub(-13_000),
                    origin: TaxOrigin::SelfPaid,
                },
                vec![Leg::tax(card, rub(-13_000))],
                date!(2026 - 08 - 25),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.taxes(CurrencyCode::Rub), rub(13_000));
        assert_eq!(flow.went_out(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn salary_in_and_spending_out_close_the_identity() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        for (kind, legs, on) in [
            (
                EventKind::CashIn { amount: rub(300_000) },
                vec![Leg::cash(card, rub(300_000))],
                date!(2026 - 08 - 05),
            ),
            (
                EventKind::CashOut { amount: rub(-120_000) },
                vec![Leg::cash(card, rub(-120_000))],
                date!(2026 - 08 - 12),
            ),
        ] {
            flow.apply(&event(kind, legs, on), &contour, august())
                .expect("applies");
        }

        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(300_000));
        assert_eq!(flow.went_out(CurrencyCode::Rub), rub(120_000));
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(180_000));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn an_event_outside_the_window_is_ignored() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashIn { amount: rub(1_000) },
                vec![Leg::cash(card, rub(1_000))],
                date!(2026 - 07 - 31),
            ),
            &contour,
            august(),
        )
        .expect("applies");
        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.currencies().count(), 0);
    }

    #[test]
    fn two_accounts_wrong_in_opposite_directions_are_both_named() {
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![card, deposit],
        );
        let mut flow = MoneyFlow::new();
        for (account, amount) in [(card, rub(-5_000)), (deposit, rub(5_000))] {
            flow.apply(
                &event(
                    EventKind::OpeningCash { amount },
                    vec![Leg::cash(account, amount)],
                    date!(2026 - 08 - 18),
                ),
                &contour,
                august(),
            )
            .expect("applies");
        }

        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
        let named = flow.residuals_by_account();
        assert_eq!(named.len(), 2, "both accounts must be named: {named:?}");
    }

    #[test]
    fn two_currencies_never_mix() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let usd = Money::new(PostedMinor::new(50_000), CurrencyCode::Usd);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashIn { amount: rub(300_000) },
                vec![Leg::cash(card, rub(300_000))],
                date!(2026 - 08 - 05),
            ),
            &contour,
            august(),
        )
        .expect("applies");
        flow.apply(
            &event(
                EventKind::CashIn { amount: usd },
                vec![Leg::cash(card, usd)],
                date!(2026 - 08 - 06),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(300_000));
        assert_eq!(flow.came_in(CurrencyCode::Usd), usd);
        assert_eq!(flow.currencies().count(), 2);
    }
}
