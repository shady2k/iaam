//! What the owner says an account's securities are worth, with no
//! composition (`iaam-k3gh.11`).
//!
//! **Why this is not [`super::balances::Balances`].** That fold sums legs, and
//! [`crate::event::kind::EventKind::StatedSecuritiesValue`] posts none — it
//! names no instrument, so there is nothing to key a security leg on. This
//! module is the fold this fact actually needs: not a sum, but a set of
//! dated assertions per account, read back by "the one at or before a date",
//! exactly as [`crate::valuation::PriceBoard`] is read for an instrument's
//! price. That is deliberate and not a borrowed shape: a second
//! `stated_securities_value` on one account is the owner correcting or
//! updating the figure, not a second fact to add to the first, and reading
//! "the latest at or before" is what makes the second one **replace** the
//! first instead of accumulating with it — see the event kind's own doc
//! comment for why that is the opposite of how `OpeningCash` behaves on a
//! second submission.
//!
//! **Why this is not wired into [`super::state::LedgerState`].** `PriceBoard`
//! is, because two independent readers need one answer to "what did the
//! journal say this was worth" — the returns report, folded through
//! [`super::advance`], and the asset snapshot, folded directly. This fact has
//! one reader today: the asset snapshot, which already folds
//! [`super::balances::Balances`] and `PriceBoard` directly from a journal
//! slice rather than through a whole projection, because it needs a state
//! without needing lots, flows or income. Adding this ledger to
//! `LedgerState` for a reader that does not exist yet would cost every stored
//! snapshot its fingerprint for nothing a report reads. If a second reader
//! arrives, wiring this into `LedgerState` the way `PriceBoard` is wired is
//! the change to make then, not a guess made now.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::Date;

use crate::event::Event;
use crate::event::kind::EventKind;
use crate::ids::AccountId;
use crate::money::Money;

/// One dated assertion of an account's securities value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatedSecuritiesValue {
    pub amount: Money,
    pub as_of: Date,
}

/// Every stated-securities assertion the journal holds, by account and date.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatedSecuritiesLedger {
    values: BTreeMap<AccountId, BTreeMap<Date, Money>>,
}

impl StatedSecuritiesLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one assertion. Multiple assertions for one account remain
    /// under their own dates — an earlier one must not disappear when a later
    /// one arrives, on the same grounds [`crate::valuation::PriceBoard::record`]
    /// gives: [`Self::value_at_or_before`] is what turns "several dated
    /// assertions" into "the one that currently applies", and it needs every
    /// one of them in order to answer that for any date asked of it, not only
    /// the latest.
    pub fn record(&mut self, account: AccountId, as_of: Date, amount: Money) {
        self.values
            .entry(account)
            .or_default()
            .insert(as_of, amount);
    }

    /// Record whatever a journal event states about an account's stated
    /// securities value, and nothing when it states none.
    ///
    /// The single definition of "a journal event that carries a stated
    /// securities value", on the same grounds
    /// [`crate::valuation::PriceBoard::observe`] gives for a price: written
    /// twice, the two would drift.
    ///
    /// An event with no effective date records nothing: the assertion is a
    /// fact about a day, and a fact without one cannot be looked up at or
    /// before anything.
    pub fn observe(&mut self, event: &Event) {
        let EventKind::StatedSecuritiesValue { amount } = &event.kind else {
            return;
        };
        let Some(as_of) = event.dates.effective_date() else {
            return;
        };
        self.record(event.account, as_of, *amount);
    }

    /// The account's stated securities value at or before a date — its
    /// latest assertion no later than `as_of` — or `None` where the owner
    /// has asserted nothing yet.
    ///
    /// **This is the whole of how a second assertion supersedes the first.**
    /// There is no replacement relation and no correction: both events stand
    /// in the journal, and this method reads only the one whose date is
    /// closest to `as_of` without exceeding it — exactly
    /// [`crate::valuation::PriceBoard::price_at_or_before`]'s own answer for
    /// a price, and for the same reason: a report at an earlier date must
    /// still see what was asserted as of *that* date, not a figure from the
    /// owner's future.
    ///
    /// **Whether this figure is still read at all — as against merely being
    /// the latest of its own kind — is decided by the caller, not here.** A
    /// full position synchronisation supersedes this fact once the account
    /// carries a real position, and this method has no way to know that: it
    /// answers only "what was last asserted", and
    /// `iaam_core::report::assets::fold_positions` is where the presence of
    /// a real position is checked before this answer is read into a total.
    #[must_use]
    pub fn value_at_or_before(
        &self,
        account: AccountId,
        as_of: Date,
    ) -> Option<StatedSecuritiesValue> {
        let (&as_of, &amount) = self.values.get(&account)?.range(..=as_of).next_back()?;
        Some(StatedSecuritiesValue { amount, as_of })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use crate::event::provenance::{ParserVersion, Provenance, RawHash};
    use crate::event::{Confidence, Relation};
    use crate::ids::{EventId, OwnerId, SourceId};
    use crate::money::{CurrencyCode, PostedMinor};
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn event(account: AccountId, day: time::Date, amount: Money) -> Event {
        Event {
            id: EventId::new_random(),
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::StatedSecuritiesValue { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, 1),
            legs: Vec::new(),
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"a".repeat(64)).expect("raw hash"),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    #[test]
    fn an_unasserted_account_answers_none() {
        let ledger = StatedSecuritiesLedger::new();
        assert_eq!(
            ledger.value_at_or_before(AccountId::new_random(), date!(2026 - 01 - 31)),
            None
        );
    }

    #[test]
    fn a_report_before_the_assertion_date_sees_nothing() {
        let account = AccountId::new_random();
        let mut ledger = StatedSecuritiesLedger::new();
        ledger.observe(&event(account, date!(2026 - 03 - 01), rub(500_000)));
        assert_eq!(
            ledger.value_at_or_before(account, date!(2026 - 02 - 01)),
            None
        );
    }

    /// A second assertion replaces the first: the report reads the one at or
    /// before the date, never their sum.
    #[test]
    fn a_later_assertion_supersedes_an_earlier_one() {
        let account = AccountId::new_random();
        let mut ledger = StatedSecuritiesLedger::new();
        ledger.observe(&event(account, date!(2026 - 01 - 31), rub(500_000)));
        ledger.observe(&event(account, date!(2026 - 02 - 28), rub(620_000)));

        assert_eq!(
            ledger.value_at_or_before(account, date!(2026 - 01 - 31)),
            Some(StatedSecuritiesValue {
                amount: rub(500_000),
                as_of: date!(2026 - 01 - 31)
            })
        );
        assert_eq!(
            ledger.value_at_or_before(account, date!(2026 - 03 - 31)),
            Some(StatedSecuritiesValue {
                amount: rub(620_000),
                as_of: date!(2026 - 02 - 28)
            }),
            "the later assertion replaces the earlier one, never adds to it"
        );
    }

    #[test]
    fn an_event_of_a_different_kind_is_ignored() {
        let account = AccountId::new_random();
        let mut ledger = StatedSecuritiesLedger::new();
        let mut cash_in = event(account, date!(2026 - 01 - 10), rub(1));
        cash_in.kind = EventKind::CashIn { amount: rub(1) };
        ledger.observe(&cash_in);
        assert_eq!(
            ledger.value_at_or_before(account, date!(2026 - 12 - 31)),
            None
        );
    }
}
