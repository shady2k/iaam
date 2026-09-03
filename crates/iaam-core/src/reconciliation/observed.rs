//! The same quantities, calculated from the journal (§10.3).
//!
//! This is the **second side** of the reconciliation. The first is what the source reported
//! ([`super::claim`]). The sides must be calculated independently: a shared
//! helper between them would turn the check into a tautology, and
//! a compensating parsing error would no longer be caught — exactly
//! why §10.3 introduces three levels of confidence, not two.
//!
//! Balances are taken from [`Balances`] — an already verified projection. This does not
//! compromise independence: `Balances` calculates from the journal, not from the
//! document's control section, and it shares no code with report parsing.
//!

use std::collections::BTreeMap;

use thiserror::Error;
use time::Date;

use super::anchor::{OpeningAnchor, OpeningAnchors};
use super::claim::{AssertionPeriod, BalancePoint};
use crate::event::Event;
use crate::event::correction::CorrectionError;
use crate::event::kind::EventKind;
use crate::event::leg::LegKind;
use crate::ids::{AccountId, CustodyId, EventId, InstrumentId};
use crate::money::{CurrencyCode, PostedMinor, Quantity};
use crate::numeric::NumericError;
use crate::projection::balances::{BalanceError, Balances, PositionKey};

/// Account turnover over an interval.
///
/// Both sides are **absolute values**. `debit` is inflow, `credit` is outflow;
/// the parser establishes how they map to the columns of a specific report,
/// not this structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turnover {
    pub debit: PostedMinor,
    pub credit: PostedMinor,
}

impl Default for Turnover {
    /// Zero turnover is written explicitly here rather than derived from
    /// `Default` for [`PostedMinor`]: the monetary type deliberately has no
    /// default, because using a zero placeholder instead of an unknown amount
    /// is exactly what §4.9 prohibits. For turnover, zero is meaningful:
    /// it means «there were no movements», and the accumulator starts from it
    /// only where the account is already known to exist.
    fn default() -> Self {
        Self {
            debit: PostedMinor::new(0),
            credit: PostedMinor::new(0),
        }
    }
}

/// The events that went into one fold, and the dates they span.
///
/// This exists because a discrepancy that states only asserted, observed and
/// their difference does not say what it compared, and an owner facing one had
/// to reconstruct the answer by summing the account's legs by hand
/// (`iaam-lg2t`). The system holds the fold; it can say how wide it was.
///
/// It carries a **count and a span, not the events themselves**. The identities
/// would be an unbounded list on every outcome — a balance folded over years of
/// history names every event of those years — and they are already answerable
/// for exactly this window from the operations listing. What cannot be
/// recovered without this is the window: which dates the figure covered, and
/// that it covered any events at all.
///
/// `first` and `last` are the dates actually folded, not the interval's
/// boundaries. A March closing balance folded from a journal that begins in
/// February spans February to March, and saying «March» would name a window the
/// figure does not come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FoldSpan {
    /// How many of the account's events were folded in.
    pub events: u64,
    /// The effective date of the earliest one; `None` when none was folded.
    pub first: Option<Date>,
    /// The effective date of the latest one; `None` when none was folded.
    pub last: Option<Date>,
}

impl FoldSpan {
    fn include(&mut self, date: Date) {
        self.events += 1;
        self.first = Some(self.first.map_or(date, |known| known.min(date)));
        self.last = Some(self.last.map_or(date, |known| known.max(date)));
    }

    /// The span of two folds taken together, as a closing balance is: everything
    /// before the interval, then everything within it.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            events: self.events.saturating_add(other.events),
            first: match (self.first, other.first) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (value, None) | (None, value) => value,
            },
            last: match (self.last, other.last) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (value, None) | (None, value) => value,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ObserveError {
    #[error("event {event:?} has no date and falls within no period")]
    EventWithoutDate { event: EventId },
    #[error("overflow while calculating quantity {field}")]
    Overflow { field: &'static str },
    #[error(transparent)]
    Balance(#[from] BalanceError),
    #[error(transparent)]
    Correction(#[from] CorrectionError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Observed quantities over an interval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedTotals {
    cash_opening: BTreeMap<CurrencyCode, PostedMinor>,
    cash_closing: BTreeMap<CurrencyCode, PostedMinor>,
    positions_opening: BTreeMap<(InstrumentId, CustodyId), Quantity>,
    positions_closing: BTreeMap<(InstrumentId, CustodyId), Quantity>,
    turnover: BTreeMap<CurrencyCode, Turnover>,
    fees: BTreeMap<CurrencyCode, PostedMinor>,
    income: BTreeMap<CurrencyCode, PostedMinor>,
    tax_withheld: BTreeMap<CurrencyCode, PostedMinor>,
    tax_facts_recorded: bool,
    /// Whether anything asserts the state each fold began from, for exactly the
    /// keys a fold produced a figure for. A key absent here has no figure, and
    /// the question does not arise: there is no sum whose start could have been
    /// invented.
    cash_anchor: BTreeMap<CurrencyCode, OpeningAnchor>,
    position_anchor: BTreeMap<(InstrumentId, CustodyId), OpeningAnchor>,
    before: FoldSpan,
    within: FoldSpan,
}

impl ObservedTotals {
    #[must_use]
    pub fn cash_at(&self, at: BalancePoint, currency: CurrencyCode) -> Option<PostedMinor> {
        match at {
            BalancePoint::Opening => self.cash_opening.get(&currency).copied(),
            BalancePoint::Closing => self.cash_closing.get(&currency).copied(),
        }
    }

    #[must_use]
    pub fn position_at(
        &self,
        at: BalancePoint,
        instrument: InstrumentId,
        custody: CustodyId,
    ) -> Option<Quantity> {
        match at {
            BalancePoint::Opening => self.positions_opening.get(&(instrument, custody)).copied(),
            BalancePoint::Closing => self.positions_closing.get(&(instrument, custody)).copied(),
        }
    }

    #[must_use]
    pub fn turnover(&self, currency: CurrencyCode) -> Option<Turnover> {
        self.turnover.get(&currency).copied()
    }

    #[must_use]
    pub fn fees(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.fees.get(&currency).copied()
    }

    #[must_use]
    pub fn income(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.income.get(&currency).copied()
    }

    #[must_use]
    pub fn tax_withheld(&self, currency: CurrencyCode) -> Option<PostedMinor> {
        self.tax_withheld.get(&currency).copied()
    }

    /// Whether at least one withheld-tax fact has been recorded in the journal.
    ///
    /// False means «there is nothing to compare against», not «the tax is zero».
    /// Tax facts appear in E5; until then, the report's claim
    /// about withheld tax is not a discrepancy.
    #[must_use]
    pub const fn tax_facts_recorded(&self) -> bool {
        self.tax_facts_recorded
    }

    /// Whether the opening the cash fold began from is asserted.
    ///
    /// `None` means the fold produced no figure in this currency at all — the
    /// account has never moved it — so there is no invented start to report.
    #[must_use]
    pub fn cash_anchor(&self, currency: CurrencyCode) -> Option<OpeningAnchor> {
        self.cash_anchor.get(&currency).copied()
    }

    /// The same for one holding. `None` under the same condition: no leg of
    /// this instrument in this depository has ever touched the account.
    #[must_use]
    pub fn position_anchor(
        &self,
        instrument: InstrumentId,
        custody: CustodyId,
    ) -> Option<OpeningAnchor> {
        self.position_anchor.get(&(instrument, custody)).copied()
    }

    /// The account's events dated before the interval — the fold an opening
    /// figure came out of.
    #[must_use]
    pub const fn folded_before(&self) -> FoldSpan {
        self.before
    }

    /// The account's events dated within the interval — the fold every interval
    /// total came out of, and the second half of a closing figure's fold.
    #[must_use]
    pub const fn folded_within(&self) -> FoldSpan {
        self.within
    }

    /// How many account events the journal saw during and before the interval.
    /// Zero means there is nothing to verify: no history exists.
    #[must_use]
    pub const fn events_seen(&self) -> u64 {
        self.before.events + self.within.events
    }
}

/// Calculation of observed quantities over an interval.
///
/// The logic was deliberately moved out of a constructor named `new`:
/// `cargo-mutants` silently skips functions with that name (§15.7).
///
/// `events` is the **already-resolved** effective set, and the `&[&Event]`
/// shape is the reminder: a raw slice would apply a reversal alongside the
/// event it reverses and double it. `ReconciliationLedger::build_with`
/// resolves once per build rather than once per group.
pub fn observe(
    events: &[&Event],
    account: AccountId,
    period: AssertionPeriod,
) -> Result<ObservedTotals, ObserveError> {
    let mut opening = Balances::new();
    let mut closing = Balances::new();
    let mut totals = ObservedTotals::default();

    for event in events {
        let date = event
            .dates
            .effective_date()
            .ok_or(ObserveError::EventWithoutDate { event: event.id })?;

        let touches_us = event.legs.iter().any(|leg| leg.account == account);
        if date < period.from {
            opening.apply(event)?;
            closing.apply(event)?;
            if touches_us {
                totals.before.include(date);
            }
        } else if period.contains(date) {
            closing.apply(event)?;
            if touches_us {
                totals.within.include(date);
                accumulate(&mut totals, event, account)?;
            }
        }
        // Events after the end of the interval do not apply to anything:
        // the end-of-March balance knows nothing about April.
    }

    snapshot_cash(&opening, account, &mut totals.cash_opening);
    snapshot_cash(&closing, account, &mut totals.cash_closing);
    snapshot_positions(&opening, account, &mut totals.positions_opening);
    snapshot_positions(&closing, account, &mut totals.positions_closing);
    // The anchor is asked of the whole journal, not of the interval: what
    // asserts the state before an account's first movement is a fact about the
    // account, and asking it per interval would make a March figure anchored
    // and the same account's April figure not.
    record_anchors(&OpeningAnchors::of(events), account, &mut totals);
    Ok(totals)
}

/// Record, for every key a fold produced a figure for, whether its start is
/// asserted.
///
/// Only for those keys. A currency the account has never moved has no fold, and
/// stamping it «unasserted» would say a sum rests on an invented start when
/// there is no sum — the caller compares such a claim against the absence of a
/// record, which is a different question and is answered elsewhere.
fn record_anchors(anchors: &OpeningAnchors, account: AccountId, totals: &mut ObservedTotals) {
    for currency in totals
        .cash_opening
        .keys()
        .chain(totals.cash_closing.keys())
        .copied()
        .collect::<Vec<_>>()
    {
        totals
            .cash_anchor
            .insert(currency, anchors.cash(account, currency));
    }
    for (instrument, custody) in totals
        .positions_opening
        .keys()
        .chain(totals.positions_closing.keys())
        .copied()
        .collect::<Vec<_>>()
    {
        totals.position_anchor.insert(
            (instrument, custody),
            anchors.position(account, instrument, custody),
        );
    }
}

fn snapshot_cash(
    balances: &Balances,
    account: AccountId,
    into: &mut BTreeMap<CurrencyCode, PostedMinor>,
) {
    for (owner, money) in balances.iter_cash() {
        if owner == account {
            into.insert(money.currency(), money.amount());
        }
    }
}

fn snapshot_positions(
    balances: &Balances,
    account: AccountId,
    into: &mut BTreeMap<(InstrumentId, CustodyId), Quantity>,
) {
    for (key, quantity) in balances.iter_positions() {
        let PositionKey {
            account: owner,
            custody,
            instrument,
        } = key;
        if *owner != account {
            continue;
        }
        // The report's claim always names a depository, so
        // a position recorded without one is not eligible for reconciliation and is
        // not included in the snapshot: there would be nothing to compare it against.
        if let Some(custody) = custody {
            into.insert((*instrument, *custody), quantity);
        }
    }
}

/// Accumulation of interval quantities from the legs of **our** account.
fn accumulate(
    totals: &mut ObservedTotals,
    event: &Event,
    account: AccountId,
) -> Result<(), ObserveError> {
    let is_income = matches!(event.kind, EventKind::Income { .. });
    for leg in &event.legs {
        if leg.account != account {
            continue;
        }
        let Some(money) = leg.cash_effect() else {
            continue;
        };
        let currency = money.currency();
        let raw = money.amount().raw();

        let turnover = totals.turnover.entry(currency).or_default();
        if raw >= 0 {
            turnover.debit = turnover
                .debit
                .checked_add(PostedMinor::new(raw))
                .ok_or(ObserveError::Overflow { field: "debit" })?;
        } else {
            let magnitude = raw
                .checked_neg()
                .ok_or(ObserveError::Overflow { field: "credit" })?;
            turnover.credit = turnover
                .credit
                .checked_add(PostedMinor::new(magnitude))
                .ok_or(ObserveError::Overflow { field: "credit" })?;
        }

        match leg.kind {
            LegKind::Fee => add_magnitude(&mut totals.fees, currency, raw, "fees")?,
            LegKind::Tax => {
                totals.tax_facts_recorded = true;
                add_magnitude(&mut totals.tax_withheld, currency, raw, "tax_withheld")?;
            }
            LegKind::Cash => {
                if is_income {
                    add_magnitude(&mut totals.income, currency, raw, "income")?;
                }
            }
            LegKind::SecurityQuantity | LegKind::Principal => {}
        }
    }
    Ok(())
}

/// Adding the absolute value of a quantity: the report's control totals are absolute values,
/// and the sign is conveyed by the column name, not the number.
fn add_magnitude(
    into: &mut BTreeMap<CurrencyCode, PostedMinor>,
    currency: CurrencyCode,
    raw: i64,
    field: &'static str,
) -> Result<(), ObserveError> {
    let magnitude = raw.checked_abs().ok_or(ObserveError::Overflow { field })?;
    let slot = into.entry(currency).or_insert_with(|| PostedMinor::new(0));
    *slot = slot
        .checked_add(PostedMinor::new(magnitude))
        .ok_or(ObserveError::Overflow { field })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Relation;
    use crate::event::kind::{FeeOrigin, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::money::Money;
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn observe(
        events: &[Event],
        account: AccountId,
        period: AssertionPeriod,
    ) -> Result<ObservedTotals, ObserveError> {
        let effective: Vec<&Event> = events.iter().collect();
        super::observe(&effective, account, period)
    }

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    fn per_unit(text: &str) -> crate::money::PerUnitAmount {
        crate::money::PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    /// Bond position at the beginning of March plus a corporate action.
    ///
    /// The opening position is needed so that the snapshot has something to show:
    /// amortisation does not change the quantity, and without it there is nothing to compare.
    fn bond_events(
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
        action: crate::event::corporate_action::CorporateAction,
        action_legs: Vec<Leg>,
    ) -> Vec<Event> {
        vec![
            event_with(
                account,
                date!(2026 - 02 - 10),
                1,
                EventKind::OpeningPosition {
                    instrument,
                    quantity: qty(10),
                    cost_basis: Some(rub(1_000_000)),
                    assertions: crate::event::kind::OpeningAssertions::default(),
                },
                vec![Leg::security(account, custody, instrument, qty(10))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 15),
                1,
                EventKind::CorporateAction { action },
                action_legs,
            ),
        ]
    }

    #[test]
    fn amortisation_moves_cash_but_not_the_position_count() {
        // §6.5: there is a payment, but no disposal. Changing the quantity here
        // would create a spurious discrepancy with the broker report.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let events = bond_events(
            account,
            instrument,
            custody,
            crate::event::corporate_action::CorporateAction::PartialRedemption {
                instrument,
                custody,
                quantity: qty(10),
                principal_returned_per_unit: per_unit("200"),
                compensation: rub(200_000),
                effective_date: date!(2026 - 03 - 15),
                record_date: None,
                grounds: None,
                basis_allocation: crate::event::allocation::BasisAllocation::default(),
            },
            vec![Leg::principal(account, instrument, rub(200_000))],
        );

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.turnover(CurrencyCode::Rub).map(|t| t.debit),
            Some(PostedMinor::new(200_000)),
            "the Principal leg must be included in turnover: it is already monetary"
        );
        assert_eq!(
            observed.position_at(BalancePoint::Closing, instrument, custody),
            Some(qty(10)),
            "amortisation does not remove the security from the position"
        );
    }

    #[test]
    fn amortisation_is_not_counted_as_income() {
        // Return of capital is not income (§6.5).
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let events = bond_events(
            account,
            instrument,
            custody,
            crate::event::corporate_action::CorporateAction::PartialRedemption {
                instrument,
                custody,
                quantity: qty(10),
                principal_returned_per_unit: per_unit("200"),
                compensation: rub(200_000),
                effective_date: date!(2026 - 03 - 15),
                record_date: None,
                grounds: None,
                basis_allocation: crate::event::allocation::BasisAllocation::default(),
            },
            vec![Leg::principal(account, instrument, rub(200_000))],
        );

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(observed.income(CurrencyCode::Rub), None);
    }

    #[test]
    fn a_redemption_moves_both_the_cash_and_the_position() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let events = bond_events(
            account,
            instrument,
            custody,
            crate::event::corporate_action::CorporateAction::Redemption {
                instrument,
                custody,
                quantity: qty(10),
                principal_returned_per_unit: per_unit("1000"),
                compensation: rub(1_000_000),
                effective_date: date!(2026 - 03 - 15),
                record_date: None,
                grounds: None,
            },
            vec![
                Leg::principal(account, instrument, rub(1_000_000)),
                Leg::security(account, custody, instrument, qty(-10)),
            ],
        );

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.turnover(CurrencyCode::Rub).map(|t| t.debit),
            Some(PostedMinor::new(1_000_000))
        );
        assert_eq!(
            observed.position_at(BalancePoint::Closing, instrument, custody),
            Some(qty(0)),
            "a redeemed security does not remain in the position"
        );
    }

    #[test]
    fn opening_excludes_the_period_and_closing_includes_it() {
        // The opening balance for March is the state before the first March
        // event. Including March in «opening» means reconciling the report against
        // itself: both sides would shift identically, and the discrepancy would disappear.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 02 - 20),
                1,
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 10),
                1,
                EventKind::CashIn {
                    amount: rub(50_000),
                },
                vec![Leg::cash(account, rub(50_000))],
            ),
            event_with(
                account,
                date!(2026 - 04 - 05),
                1,
                EventKind::CashIn { amount: rub(7) },
                vec![Leg::cash(account, rub(7))],
            ),
        ];

        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.cash_at(BalancePoint::Opening, CurrencyCode::Rub),
            Some(PostedMinor::new(100_000))
        );
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            Some(PostedMinor::new(150_000)),
            "an April event must not be included in the end-of-March balance"
        );
    }

    #[test]
    fn turnover_counts_every_cash_leg_including_fees() {
        // Account turnover is every movement of money, not just legs
        // of type Cash. A fee charged to the same account is present in the
        // broker report's turnover, and failing to include it would
        // create a spurious discrepancy.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 02),
                1,
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 03),
                1,
                EventKind::Fee {
                    amount: rub(-350),
                    origin: FeeOrigin::Brokerage,
                },
                vec![Leg::fee(account, rub(-350))],
            ),
        ];

        let observed = observe(&events, account, march()).unwrap();
        let turnover = observed.turnover(CurrencyCode::Rub).unwrap();
        assert_eq!(turnover.debit, PostedMinor::new(100_000), "inflow");
        assert_eq!(
            turnover.credit,
            PostedMinor::new(350),
            "outflow as an absolute value"
        );
    }

    #[test]
    fn fees_are_collected_from_trades_too() {
        // A fee within a trade is still a fee. The report's control section
        // totals all of them, and collecting only standalone Fee events
        // means undercounting by exactly the trade fees.
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(Dec::new(Decimal::from(10)));
        let trade = event_with(
            account,
            date!(2026 - 03 - 04),
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(-50_000),
                fee: Some(rub(-120)),
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, rub(-50_000)),
                Leg::fee(account, rub(-120)),
                Leg::security(account, custody, instrument, quantity),
            ],
        );
        let standalone = event_with(
            account,
            date!(2026 - 03 - 05),
            1,
            EventKind::Fee {
                amount: rub(-80),
                origin: FeeOrigin::Depositary,
            },
            vec![Leg::fee(account, rub(-80))],
        );

        let observed = observe(&[trade, standalone], account, march()).unwrap();
        assert_eq!(
            observed.fees(CurrencyCode::Rub),
            Some(PostedMinor::new(200)),
            "120 within the trade plus 80 as a separate event, as an absolute value"
        );
    }

    #[test]
    fn an_event_on_the_first_day_belongs_to_the_period_not_before_it() {
        // The interval boundary is inclusive: an event on March 1 belongs to
        // March, not «before March». Shifting the boundary by one day would move
        // the transaction into the opening balance, and both sides of the reconciliation would shift
        // identically — making the error invisible.
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 01),
            1,
            EventKind::CashIn {
                amount: rub(100_000),
            },
            vec![Leg::cash(account, rub(100_000))],
        )];
        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.cash_at(BalancePoint::Opening, CurrencyCode::Rub),
            None,
            "March 1 is not yet included in the opening balance for March"
        );
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            Some(PostedMinor::new(100_000))
        );
        assert_eq!(
            observed.turnover(CurrencyCode::Rub).unwrap().debit,
            PostedMinor::new(100_000),
            "the March 1 transaction is included in March turnover"
        );
    }

    #[test]
    fn every_touching_event_is_counted_once() {
        // The counter determines whether the account has any history at all: it lets
        // the reconciliation distinguish «a mismatch» from «nothing to reconcile».
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 02 - 20),
                1,
                EventKind::CashIn { amount: rub(1) },
                vec![Leg::cash(account, rub(1))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 02),
                1,
                EventKind::CashIn { amount: rub(1) },
                vec![Leg::cash(account, rub(1))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 03),
                1,
                EventKind::CashIn { amount: rub(1) },
                vec![Leg::cash(account, rub(1))],
            ),
        ];
        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.events_seen(),
            3,
            "events both before the interval and within it are counted"
        );
    }

    #[test]
    fn absence_of_movement_is_not_zero() {
        // `None` and `Some(0)` are different claims. The first means
        // «there is no data», the second «there is data, and the balance is zero».
        // Collapsing them would present the absence of history as
        // a confirmed zero (§4.9, §10.7).
        let account = AccountId::new_random();
        let observed = observe(&[], account, march()).unwrap();
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            None
        );
        assert_eq!(observed.turnover(CurrencyCode::Rub), None);
        assert_eq!(observed.events_seen(), 0);
    }

    #[test]
    fn tax_is_not_comparable_until_a_tax_leg_exists() {
        // No write path produces tax legs: taxes are E5.
        // Until they exist, there is nothing to compare withheld tax against, and zero
        // on our side means «we do not count it», not «the broker withheld nothing».
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 02),
            1,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        )];
        let observed = observe(&events, account, march()).unwrap();
        assert!(!observed.tax_facts_recorded());
        assert_eq!(observed.tax_withheld(CurrencyCode::Rub), None);
    }

    #[test]
    fn a_tax_leg_makes_the_dimension_comparable() {
        // The converse of the previous test: as soon as a tax fact
        // appears, comparison becomes possible on its own — without
        // changing the reconciliation. This verifies that the flag is calculated
        // from the journal, rather than hard-coded as «there are no taxes in E2».
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 07),
            1,
            EventKind::Income {
                instrument: None,
                gross: rub(10_000),
                kind: None,
            },
            vec![
                Leg::cash(account, rub(10_000)),
                Leg::tax(account, rub(-1_300)),
            ],
        )];
        let observed = observe(&events, account, march()).unwrap();
        assert!(observed.tax_facts_recorded());
        assert_eq!(
            observed.tax_withheld(CurrencyCode::Rub),
            Some(PostedMinor::new(1_300)),
            "withheld tax is accumulated as an absolute value"
        );
    }

    #[test]
    fn income_is_summed_from_income_events_only() {
        // Cash received and income are different things. An owner's account contribution
        // is cash, but not income, and must not be included in the control total
        // for coupons and dividends.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 06),
                1,
                EventKind::CashIn {
                    amount: rub(500_000),
                },
                vec![Leg::cash(account, rub(500_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 07),
                1,
                EventKind::Income {
                    instrument: None,
                    gross: rub(4_000),
                    kind: None,
                },
                vec![Leg::cash(account, rub(4_000))],
            ),
        ];
        let observed = observe(&events, account, march()).unwrap();
        assert_eq!(
            observed.income(CurrencyCode::Rub),
            Some(PostedMinor::new(4_000))
        );
    }

    #[test]
    fn another_account_does_not_leak_into_the_totals() {
        // The claim is about the account. Legs from another account in the turnover are
        // confirmation obtained using someone else's money.
        let ours = AccountId::new_random();
        let theirs = AccountId::new_random();
        let events = vec![event_with(
            theirs,
            date!(2026 - 03 - 08),
            1,
            EventKind::CashIn { amount: rub(999) },
            vec![Leg::cash(theirs, rub(999))],
        )];
        let observed = observe(&events, ours, march()).unwrap();
        assert_eq!(observed.turnover(CurrencyCode::Rub), None);
        assert_eq!(observed.events_seen(), 0);
    }

    #[test]
    fn an_event_without_a_date_is_a_typed_error() {
        // An event without a date falls within no period. Silently skipping it
        // means calculating the reconciliation over an incomplete snapshot and reporting
        // a discrepancy where none exists.
        let account = AccountId::new_random();
        let mut event = event_with(
            account,
            date!(2026 - 03 - 09),
            1,
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        );
        event.dates = crate::dates::EventDates::empty();
        assert!(matches!(
            observe(&[event], account, march()),
            Err(ObserveError::EventWithoutDate { .. })
        ));
    }
    #[test]
    fn a_reversed_trade_contributes_nothing_to_observed_totals() {
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = qty(10);
        let trade = event_with(
            account,
            date!(2026 - 03 - 10),
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(-50_000),
                fee: Some(rub(-120)),
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, rub(-50_000)),
                Leg::fee(account, rub(-120)),
                Leg::security(account, custody, instrument, quantity),
            ],
        );
        let mut reversal = trade.clone();
        reversal.id = crate::ids::EventId::new_random();
        reversal.relation = Relation::Reversal { target: trade.id };

        let raw = [trade, reversal];
        let effective = crate::event::correction::resolve(&raw).unwrap();
        let observed = super::observe(&effective, account, march()).unwrap();

        assert_eq!(
            observed.cash_at(BalancePoint::Opening, CurrencyCode::Rub),
            None
        );
        assert_eq!(
            observed.cash_at(BalancePoint::Closing, CurrencyCode::Rub),
            None
        );
        assert_eq!(observed.turnover(CurrencyCode::Rub), None);
        assert_eq!(observed.fees(CurrencyCode::Rub), None);
    }
}
