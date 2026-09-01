//! Dated income facts (§7.2).
//!
//! The fourth independent journal reader. It deliberately takes nothing
//! from lots: `received_to_date` answers a different question — how much
//! has been received over the lifetime — and is distributed proportionally across lots, while on
//! security replacement it is carried over to the new one. Reconciliation asks something else:
//! did a specific scheduled payment arrive, and when? Therefore the fact
//! lives separately from lot aggregates, with one row per
//! (account, instrument) pair.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::event::Event;
use crate::event::corporate_action::CorporateAction;
use crate::event::kind::{EventKind, IncomeKind};
use crate::event::offer::OfferExerciseAction;
use crate::ids::EventId;
use crate::money::Money;
use crate::projection::lots::LotKey;
use crate::rules::PostingKind;

/// An income fact with a date and kind.
///
/// Stores the `EventId`, not the entire event: the projection only needs a reference
/// to the journal plus the values used for matching. A copy
/// of the journal inside the snapshot would duplicate the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedPosting {
    pub event: EventId,
    pub date: Date,
    pub amount: Money,
    pub kind: PostingKind,
}

/// Why reconciliation for an (account, instrument) pair cannot be proven.
///
/// This is not a data defect, but an honest refusal to assert: silently saying
/// “the payment was not received” from incomplete input accuses the broker of something
/// the journal does not say (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncomeGap {
    /// There is a payment whose kind is unknown: it cannot be placed on the schedule.
    IncomeKindUnknown,
    /// There is a payment with neither a posting date nor a payment date.
    PaymentDateUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IncomeError {
    #[error(transparent)]
    Money(#[from] crate::money::MoneyError),
}

/// Dated income facts by (account, instrument) pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomeLedger {
    entries: BTreeMap<LotKey, Entry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    postings: Vec<ReceivedPosting>,
    gap: Option<IncomeGap>,
}

impl IncomeLedger {
    /// Facts for the pair in journal read order.
    ///
    /// An empty slice for a pair absent from the map and for a pair with no payments
    /// means the same thing: “there is nothing to confirm with.” They are distinguished not by the slice,
    /// but by [`IncomeLedger::gap`].
    #[must_use]
    pub fn postings(&self, key: &LotKey) -> &[ReceivedPosting] {
        self.entries
            .get(key)
            .map_or(&[][..], |entry| entry.postings.as_slice())
    }

    #[must_use]
    pub fn gap(&self, key: &LotKey) -> Option<IncomeGap> {
        self.entries.get(key).and_then(|entry| entry.gap)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The date the money was received.
    ///
    /// `cash_posted`, otherwise `paid`. The [`crate::dates::EventDates::effective_date`]
    /// chain is unsuitable here: it starts with `settled` and falls back to `trade`,
    /// but those are not dates when money was received — substituting one would silently move
    /// the fact to another day (§4.9), and the one-sided matching window
    /// would accept or reject it based on an unrelated date.
    fn payment_date(event: &Event) -> Option<Date> {
        event
            .dates
            .cash_posted
            .map(|posted| posted.0)
            .or_else(|| event.dates.paid.map(|paid| paid.0))
    }

    fn record(&mut self, key: LotKey, posting: ReceivedPosting) {
        self.entries.entry(key).or_default().postings.push(posting);
    }

    /// Mark a pair as unverifiable. The first reason wins: it
    /// occurred earlier in the journal, and overwriting it with a later one would make
    /// the diagnosis a function of read order rather than journal contents.
    fn mark(&mut self, key: LotKey, gap: IncomeGap) {
        let entry = self.entries.entry(key).or_default();
        if entry.gap.is_none() {
            entry.gap = Some(gap);
        }
    }

    /// All three sources of scheduled payments are now handled here: a coupon
    /// arrives as `Income`, principal repayment as `CorporateAction`,
    /// and offer settlement as `OfferExercise`.
    ///
    /// The match is intentionally exhaustive, and `_ =>` is forbidden here: a new
    /// [`EventKind`] variant must break the build and force the author
    /// to decide whether it is a payment. A silent `_` would answer
    /// “no” for them — and the missing fact would surface not as a build error, but as a false
    /// reconciliation alarm for the owner.
    pub fn apply(&mut self, event: &Event) -> Result<(), IncomeError> {
        match &event.kind {
            EventKind::Income {
                instrument: Some(instrument),
                gross,
                kind,
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                self.apply_income(event, key, *gross, *kind);
                Ok(())
            }
            // Without an instrument, there is nothing to reconcile against: the payment schedule
            // belongs to the security.
            EventKind::Income {
                instrument: None, ..
            } => Ok(()),
            // None of these events is a scheduled payment
            // on a bond.
            EventKind::Trade { .. }
            | EventKind::CashIn { .. }
            | EventKind::Refund { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Fee { .. }
            | EventKind::Tax { .. }
            | EventKind::OpeningPosition { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. }
            | EventKind::ImportCoverageGap { .. } => Ok(()),
            EventKind::CorporateAction { action } => {
                self.apply_corporate_action(event, action);
                Ok(())
            }
            EventKind::OfferExercise { action } => self.apply_offer_exercise(event, action),
        }
    }

    /// Principal repayment brings money in two ways: amortisation
    /// (the position remains) and redemption (the position goes away). Security replacement
    /// brings no money and creates no fact.
    ///
    /// `compensation` is recorded — the money actually received, not
    /// the declared principal repaid: they differ by the withheld
    /// tax, and reconciliation answers the question “did the money arrive?”
    ///
    /// The date comes from [`IncomeLedger::payment_date`], not the action's `effective_date`:
    /// the latter says when the issuer made the decision, not
    /// when the money reached the owner's account.
    fn apply_corporate_action(&mut self, event: &Event, action: &CorporateAction) {
        let (instrument, compensation) = match action {
            CorporateAction::PartialRedemption {
                instrument,
                compensation,
                ..
            }
            | CorporateAction::Redemption {
                instrument,
                compensation,
                ..
            } => (*instrument, *compensation),
            // Replacement exchanges one security for another: there is nothing to confirm with it,
            // and it does not create pairs in the map.
            CorporateAction::Conversion { .. } => return,
        };
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let Some(date) = Self::payment_date(event) else {
            self.mark(key, IncomeGap::PaymentDateUnknown);
            return;
        };
        self.record(
            key,
            ReceivedPosting {
                event: event.id,
                date,
                amount: compensation,
                kind: PostingKind::PrincipalReturn,
            },
        );
    }

    /// Offer settlement is the third and final way in which
    /// a scheduled payment arrives as cash (§3.3). The request and its
    /// withdrawal do not move money: the security remains with the owner until the buyback
    /// occurs, and their state is tracked by `super::offers::OfferBook`.
    ///
    /// The amount is calculated in the same order as the proceeds in the lot book
    /// (`lots.rs:746`) and the event's own cash leg
    /// (`event/mod.rs:732`): otherwise the same buyback would have three
    /// different amounts in three places. Neither the fee nor accrued interest is replaced
    /// with zero (§4.9) — a missing term is simply absent from the sum,
    /// while declared values are combined via `try_add`/`try_sub` so that
    /// overflow and a mismatched currency reach the caller as errors,
    /// rather than being silenced.
    ///
    /// The only one of the three handlers that may fail to add up: for a coupon
    /// and principal repayment, the amount is taken from the event as is, with nothing
    /// to calculate there.
    fn apply_offer_exercise(
        &mut self,
        event: &Event,
        action: &OfferExerciseAction,
    ) -> Result<(), IncomeError> {
        // Match variants instead of filtering with a single pattern: `let ... else`
        // would also treat a future member of the family as “there was no money,”
        // without a warning from the compiler.
        match action {
            OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => Ok(()),
            OfferExerciseAction::Settled {
                instrument,
                gross,
                fee,
                accrued_interest,
                ..
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                let Some(date) = Self::payment_date(event) else {
                    self.mark(key, IncomeGap::PaymentDateUnknown);
                    return Ok(());
                };
                let mut amount = *gross;
                if let Some(interest) = accrued_interest {
                    amount = amount.try_add(*interest)?;
                }
                if let Some(fee) = fee {
                    amount = amount.try_sub(*fee)?;
                }
                self.record(
                    key,
                    ReceivedPosting {
                        event: event.id,
                        date,
                        amount,
                        kind: PostingKind::OfferSettlement,
                    },
                );
                Ok(())
            }
        }
    }

    fn apply_income(
        &mut self,
        event: &Event,
        key: LotKey,
        amount: Money,
        kind: Option<IncomeKind>,
    ) {
        match kind {
            Some(IncomeKind::Coupon) => {
                let Some(date) = Self::payment_date(event) else {
                    self.mark(key, IncomeGap::PaymentDateUnknown);
                    return;
                };
                self.record(
                    key,
                    ReceivedPosting {
                        event: event.id,
                        date,
                        amount,
                        kind: PostingKind::Coupon,
                    },
                );
            }
            // Dividends, deposit interest and cashback do not appear in the
            // bond schedule: there is nothing to confirm with them.
            Some(IncomeKind::Dividend | IncomeKind::DepositInterest) => {}
            None => self.mark(key, IncomeGap::IncomeKindUnknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{CashPostedDate, EventDates, PaidDate};
    use crate::event::corporate_action::{BasisTransferRule, FractionalTreatment};
    use crate::event::kind::{EventKind, IncomeKind};
    use crate::event::leg::Leg;
    use crate::event::offer::{OfferSubmissionId, OfferWindowId};
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, MoneyError, PerUnitAmount, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::projection::lots::LotKey;
    use crate::rules::PostingKind;
    use rust_decimal::Decimal;
    use time::Date;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    /// An income event in the `test_support` envelope: it already sets
    /// `cash_posted` to the same day as the ordering date — exactly the date
    /// by which reconciliation must look up the fact.
    fn income(
        account: AccountId,
        instrument: Option<InstrumentId>,
        day: Date,
        kind: Option<IncomeKind>,
        minor: i64,
    ) -> Event {
        let amount = rub(minor);
        event_with(
            account,
            day,
            1,
            EventKind::Income {
                instrument,
                gross: amount,
                kind,
            },
            vec![Leg::cash(account, amount)],
        )
    }

    fn coupon(account: AccountId, instrument: InstrumentId, day: Date, minor: i64) -> Event {
        income(
            account,
            Some(instrument),
            day,
            Some(IncomeKind::Coupon),
            minor,
        )
    }

    /// A coupon without any date when the money was received. `validate_structure` for
    /// `Income` (`event/mod.rs:197`) requires only one positive
    /// cash leg and does not require dates at all, so such an event
    /// can come from a real import, not only from a test.
    fn coupon_without_payment_date(
        account: AccountId,
        instrument: InstrumentId,
        minor: i64,
    ) -> Event {
        let mut event = coupon(account, instrument, date!(2026 - 03 - 18), minor);
        event.dates = EventDates::empty();
        event
    }

    fn income_of_unknown_kind(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        minor: i64,
    ) -> Event {
        income(account, Some(instrument), day, None, minor)
    }

    fn dividend(account: AccountId, instrument: InstrumentId, day: Date, minor: i64) -> Event {
        income(
            account,
            Some(instrument),
            day,
            Some(IncomeKind::Dividend),
            minor,
        )
    }

    fn income_without_instrument(account: AccountId, day: Date, minor: i64) -> Event {
        income(account, None, day, Some(IncomeKind::Coupon), minor)
    }

    #[test]
    fn a_coupon_with_a_cash_posted_date_becomes_one_dated_fact() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon(account, instrument, date!(2026 - 03 - 18), 500))
            .expect("coupon with a posting date is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        let postings = ledger.postings(&key);
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].date, date!(2026 - 03 - 18));
        assert_eq!(postings[0].kind, PostingKind::Coupon);
        assert_eq!(postings[0].amount, rub(500));
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn a_coupon_falls_back_to_the_paid_date_but_never_to_settled_or_trade() {
        // The `EventDates::effective_date` chain starts with `settled`
        // and falls back to `trade` — those are not dates when money was received. Using them
        // would silently move the fact to another day (§4.9),
        // while the matching window in the rule is one-sided.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        let mut event = coupon(account, instrument, date!(2026 - 03 - 18), 500);
        event.dates = EventDates {
            settled: Some(crate::dates::SettledDate(date!(2026 - 03 - 10))),
            trade: Some(crate::dates::TradeDate(date!(2026 - 03 - 09))),
            paid: Some(PaidDate(date!(2026 - 03 - 20))),
            ..EventDates::empty()
        };
        ledger.apply(&event).expect("coupon is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.postings(&key)[0].date, date!(2026 - 03 - 20));
    }

    #[test]
    fn a_cash_posted_date_wins_over_the_paid_date() {
        // Money in the account is the fact of receipt; the issuer's “payment date”
        // only says when the issuer paid.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        let mut event = coupon(account, instrument, date!(2026 - 03 - 18), 500);
        event.dates = EventDates {
            cash_posted: Some(CashPostedDate(date!(2026 - 03 - 18))),
            paid: Some(PaidDate(date!(2026 - 03 - 16))),
            ..EventDates::empty()
        };
        ledger.apply(&event).expect("coupon is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.postings(&key)[0].date, date!(2026 - 03 - 18));
    }

    #[test]
    fn a_payment_without_a_cash_posted_or_paid_date_cannot_be_dated() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon_without_payment_date(account, instrument, 500))
            .expect("event is accepted but does not become a dated fact");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
    }

    #[test]
    fn an_income_of_unknown_kind_blocks_reconciliation_rather_than_being_guessed() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&income_of_unknown_kind(
                account,
                instrument,
                date!(2026 - 03 - 18),
                500,
            ))
            .expect("event is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), Some(IncomeGap::IncomeKindUnknown));
    }

    #[test]
    fn the_first_reason_a_pair_is_unverifiable_survives_a_later_one() {
        // The diagnosis must not depend on how many events were read
        // after the first one: overwriting it with a later reason would make
        // the answer a function of journal length rather than journal contents.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon_without_payment_date(account, instrument, 500))
            .expect("accepted");
        ledger
            .apply(&income_of_unknown_kind(
                account,
                instrument,
                date!(2026 - 03 - 19),
                700,
            ))
            .expect("accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
    }

    #[test]
    fn a_dividend_is_not_a_scheduled_bond_posting() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&dividend(account, instrument, date!(2026 - 03 - 18), 500))
            .expect("dividend is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn a_deposit_interest_is_not_a_scheduled_bond_posting() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&income(
                account,
                Some(instrument),
                date!(2026 - 03 - 18),
                Some(IncomeKind::DepositInterest),
                500,
            ))
            .expect("deposit interest is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn income_without_an_instrument_has_nothing_to_reconcile_against() {
        let account = AccountId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&income_without_instrument(
                account,
                date!(2026 - 03 - 18),
                500,
            ))
            .expect("accepted");

        assert!(ledger.is_empty());
    }

    #[test]
    fn two_coupons_on_one_pair_are_two_facts_in_journal_order() {
        // Reconciliation matches the plan to facts one-to-one, so
        // two coupons must remain two facts: merging them into one amount
        // is exactly the loss this reader exists to prevent.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&coupon(account, instrument, date!(2026 - 03 - 18), 500))
            .expect("accepted");
        ledger
            .apply(&coupon(account, instrument, date!(2026 - 09 - 18), 500))
            .expect("accepted");

        let key = LotKey {
            account,
            instrument,
        };
        let postings = ledger.postings(&key);
        assert_eq!(postings.len(), 2);
        assert!(!ledger.is_empty());
        assert_eq!(postings[0].date, date!(2026 - 03 - 18));
        assert_eq!(postings[1].date, date!(2026 - 09 - 18));
    }

    #[test]
    fn a_pair_the_journal_never_mentioned_has_neither_facts_nor_a_gap() {
        // “There were no payments” and “the instrument was never seen” are different answers;
        // an empty slice with no entry in the map distinguishes them for reconciliation.
        let ledger = IncomeLedger::default();
        let key = LotKey {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), None);
        assert!(ledger.is_empty());
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    /// Amortisation in the `test_support` envelope: `cash_posted` is set to the same
    /// day as the ordering date. `effective_date` intentionally differs from it —
    /// it is the date of the issuer's decision, not the day the money reached the account,
    /// and the fact must be dated by the latter, not the former.
    fn partial_redemption(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        minor: i64,
    ) -> Event {
        partial_redemption_with_withheld_tax(account, instrument, day, 3, minor)
    }

    fn partial_redemption_with_withheld_tax(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        principal_per_unit: i64,
        compensation_minor: i64,
    ) -> Event {
        let compensation = rub(compensation_minor);
        event_with(
            account,
            day,
            1,
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: qty(100),
                    principal_returned_per_unit: per_unit(&principal_per_unit.to_string()),
                    compensation,
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation: crate::event::allocation::BasisAllocation::default(),
                },
            },
            vec![Leg::principal(account, instrument, compensation)],
        )
    }

    /// Amortisation without any date when the money was received: the `test_support` envelope
    /// sets `cash_posted`, so the dates are explicitly removed.
    fn partial_redemption_without_payment_date(
        account: AccountId,
        instrument: InstrumentId,
        minor: i64,
    ) -> Event {
        let mut event = partial_redemption(account, instrument, date!(2026 - 06 - 18), minor);
        event.dates = EventDates::empty();
        event
    }

    fn redemption(account: AccountId, instrument: InstrumentId, day: Date, minor: i64) -> Event {
        let compensation = rub(minor);
        event_with(
            account,
            day,
            1,
            EventKind::CorporateAction {
                action: CorporateAction::Redemption {
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: qty(10),
                    principal_returned_per_unit: per_unit("100"),
                    compensation,
                    effective_date: date!(2026 - 09 - 15),
                    record_date: None,
                    grounds: None,
                },
            },
            vec![Leg::principal(account, instrument, compensation)],
        )
    }

    fn conversion(account: AccountId, day: Date) -> Event {
        event_with(
            account,
            day,
            1,
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: InstrumentId::new_random(),
                    successor: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    ratio: Dec::new(Decimal::from(1)),
                    quantity_in: qty(100),
                    quantity_out: qty(100),
                    fractional: FractionalTreatment::NotApplicable,
                    compensation: None,
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_transfer: BasisTransferRule::CarryOver,
                },
            },
            Vec::new(),
        )
    }

    #[test]
    fn an_amortisation_payment_is_a_dated_principal_return() {
        // Amortisation arrives as `CorporateAction`, not `Income`. Looking for it
        // among coupon facts would raise a false alarm for
        // every amortising bond.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&partial_redemption(
                account,
                instrument,
                date!(2026 - 06 - 18),
                300,
            ))
            .expect("amortisation is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        let postings = ledger.postings(&key);
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].kind, PostingKind::PrincipalReturn);
        assert_eq!(postings[0].date, date!(2026 - 06 - 18));
        assert_eq!(postings[0].amount, rub(300));
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn the_recorded_amount_is_the_money_received_not_the_principal_declared() {
        // `compensation` may be less than the principal repaid — by
        // the withheld tax, for example (`event/corporate_action.rs:37-40`).
        // Reconciliation answers the question “did the money arrive?”, so it uses
        // the money, not the declared principal.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&partial_redemption_with_withheld_tax(
                account,
                instrument,
                date!(2026 - 06 - 18),
                /* principal_per_unit */ 400,
                /* compensation */ 348,
            ))
            .expect("accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.postings(&key)[0].amount, rub(348));
    }

    #[test]
    fn a_full_redemption_is_a_dated_principal_return_too() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&redemption(
                account,
                instrument,
                date!(2026 - 09 - 20),
                1000,
            ))
            .expect("redemption is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        let postings = ledger.postings(&key);
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].kind, PostingKind::PrincipalReturn);
        assert_eq!(postings[0].date, date!(2026 - 09 - 20));
        assert_eq!(postings[0].amount, rub(1000));
    }

    #[test]
    fn a_conversion_brings_no_money_and_therefore_no_fact() {
        // Replacement exchanges one security for another: there is nothing to confirm with it,
        // and the absence of a fact here is not a gap, but the correct answer.
        let account = AccountId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&conversion(account, date!(2026 - 06 - 18)))
            .expect("replacement is accepted");

        assert!(ledger.is_empty());
    }

    #[test]
    fn a_corporate_action_without_a_payment_date_cannot_be_dated() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&partial_redemption_without_payment_date(
                account, instrument, 300,
            ))
            .expect("accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
    }

    /// An offer buyback in the `test_support` envelope. The legs are intentionally empty:
    /// [`IncomeLedger`] reads the event's kind, dates, and action values,
    /// while the leg structure is checked by `Event::validate_structure`
    /// (`event/mod.rs:472`) — building them here would mean repeating
    /// in the fixture exactly the arithmetic that the test checks.
    fn offer_settled(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        gross: Money,
        fee: Option<Money>,
        accrued_interest: Option<Money>,
    ) -> Event {
        event_with(
            account,
            day,
            1,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: qty(4),
                    gross,
                    fee,
                    accrued_interest,
                },
            },
            Vec::new(),
        )
    }

    fn offer_submitted(account: AccountId, instrument: InstrumentId, day: Date) -> Event {
        event_with(
            account,
            day,
            1,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Submitted {
                    submission: OfferSubmissionId::new_random(),
                    window: OfferWindowId::new_random(),
                    instrument,
                    quantity: qty(4),
                },
            },
            Vec::new(),
        )
    }

    fn offer_cancelled(account: AccountId, day: Date) -> Event {
        event_with(
            account,
            day,
            1,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Cancelled {
                    submission: OfferSubmissionId::new_random(),
                    quantity: qty(4),
                },
            },
            Vec::new(),
        )
    }

    #[test]
    fn an_offer_settlement_is_a_dated_fact() {
        // An offer buyback is the third way in which a scheduled
        // payment arrives as cash: it appears in the schedule
        // `PostingKind::OfferSettlement` (`rules/cashflow.rs:303`).
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&offer_settled(
                account,
                instrument,
                date!(2026 - 07 - 10),
                rub(1_000),
                Some(rub(30)),
                Some(rub(25)),
            ))
            .expect("buyback is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        let postings = ledger.postings(&key);
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].kind, PostingKind::OfferSettlement);
        assert_eq!(postings[0].date, date!(2026 - 07 - 10));
        // 1000 + 25 - 30
        assert_eq!(postings[0].amount, rub(995));
        assert_eq!(ledger.gap(&key), None);
    }

    #[test]
    fn an_unstated_fee_and_interest_are_absent_from_the_sum_rather_than_zero() {
        // A missing value is not replaced with zero (§4.9): it is simply
        // absent from the sum, and the result equals the declared `gross`.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&offer_settled(
                account,
                instrument,
                date!(2026 - 07 - 10),
                rub(1_000),
                None,
                None,
            ))
            .expect("buyback without a fee or accrued interest is accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert_eq!(ledger.postings(&key)[0].amount, rub(1_000));
    }

    #[test]
    fn a_zero_fee_in_another_currency_is_an_error_while_an_absent_one_is_not() {
        // Zero is a value like any other, and it carries a currency:
        // substituting it for a missing fee would mean
        // replacing a refusal to calculate with tacit agreement.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        let error = ledger
            .apply(&offer_settled(
                account,
                instrument,
                date!(2026 - 07 - 10),
                rub(1_000),
                Some(Money::new(PostedMinor::new(0), CurrencyCode::Usd)),
                None,
            ))
            .expect_err("a fee in a different currency cannot be added to rubles");

        assert!(matches!(
            error,
            IncomeError::Money(MoneyError::CurrencyMismatch { .. })
        ));
        assert!(
            ledger
                .postings(&LotKey {
                    account,
                    instrument
                })
                .is_empty()
        );
    }

    #[test]
    fn an_overflowing_settlement_is_an_error_rather_than_a_panic_or_a_silent_skip() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        let error = ledger
            .apply(&offer_settled(
                account,
                instrument,
                date!(2026 - 07 - 10),
                Money::new(PostedMinor::new(i64::MAX), CurrencyCode::Rub),
                None,
                Some(rub(1)),
            ))
            .expect_err("overflow must reach the caller");

        assert!(matches!(error, IncomeError::Money(MoneyError::Overflow)));
        assert!(
            ledger
                .postings(&LotKey {
                    account,
                    instrument
                })
                .is_empty()
        );
    }

    #[test]
    fn a_submitted_offer_moves_no_money_and_creates_no_fact() {
        // A request does not move money: the security remains with the owner until
        // the buyback occurs. Its state is tracked by `OfferBook`.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&offer_submitted(account, instrument, date!(2026 - 07 - 01)))
            .expect("request is accepted");

        assert!(ledger.is_empty());
    }

    #[test]
    fn a_cancelled_offer_moves_no_money_and_creates_no_fact() {
        let account = AccountId::new_random();
        let mut ledger = IncomeLedger::default();

        ledger
            .apply(&offer_cancelled(account, date!(2026 - 07 - 05)))
            .expect("withdrawal is accepted");

        assert!(ledger.is_empty());
    }

    #[test]
    fn an_offer_settlement_without_a_payment_date_cannot_be_dated() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut ledger = IncomeLedger::default();

        let mut event = offer_settled(
            account,
            instrument,
            date!(2026 - 07 - 10),
            rub(1_000),
            None,
            None,
        );
        event.dates = EventDates::empty();
        ledger.apply(&event).expect("accepted");

        let key = LotKey {
            account,
            instrument,
        };
        assert!(ledger.postings(&key).is_empty());
        assert_eq!(ledger.gap(&key), Some(IncomeGap::PaymentDateUnknown));
    }
}
