//! Whether anything in the journal asserts the state a fold began from (§10.3).
//!
//! Every cash and position figure this system computes is a fold from zero over
//! the legs it has seen. Zero is a starting point only where something says the
//! account held nothing before its first recorded movement; otherwise the fold
//! is the movement over the imported interval and not a balance at all.
//!
//! **This rule lives here because two readers apply it and they must not
//! disagree.** The balances answer spends it on the spelling of a figure —
//! `CashFigure::Balance` or `CashFigure::Movement`, published as
//! `movement_since_unknown_start` — and reconciliation spends it on whether a
//! source's balance assertion can be compared at all. Until `iaam-d7hn` the two
//! read the same silence differently: the report refused to call the figure a
//! balance while reconciliation called it zero and told the owner his own
//! anchor was wrong. One rule, one place, is the only fix that stays fixed.
//!
//! **The question is whether anything states what the account held before the
//! first movement folded in.** Two things do, and both count:
//!
//! - an opening [`ControlClaim`] whose interval begins no later than that first
//!   movement — a source said so, and reconciliation then checks it;
//! - the first movement itself being a §10.7 reconstructed opening
//!   ([`EventKind::OpeningCash`], [`EventKind::OpeningPosition`]) — the owner
//!   said so, and it is recorded as a fact with provenance and legs.
//!
//! An ordinary transaction as the earliest record states nothing, and that is
//! the whole of the case this module exists for: a journal that simply begins
//! mid-history. Silence there was being read as «the account held zero».
//!
//! The assertion's *amount* is deliberately not consulted. Presence is what
//! makes the start compared rather than assumed; an anchor that disagrees with
//! the fold is a discrepancy, and a discrepancy is reported, not used to
//! withdraw the question. Nor does a reconstructed opening have to be right for
//! the fold to be a balance — being wrong is precisely what a source's later
//! balance assertion is able to catch once the fold means something.

use std::collections::BTreeMap;

use time::Date;

use super::claim::{BalancePoint, ControlClaim};
use crate::event::Event;
use crate::event::kind::EventKind;
use crate::event::leg::LegKind;
use crate::ids::{AccountId, CustodyId, InstrumentId};
use crate::money::CurrencyCode;

/// Whether anything asserts the state a fold began from.
///
/// Two values and not three: «no opening assertion» and «an opening assertion
/// that starts too late to cover the first movement» are the same answer to the
/// owner — the sum still rests on an unasserted start — and splitting them
/// would invite a reader to treat the second as half an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningAnchor {
    /// An opening assertion covers the state before the first movement folded
    /// in, so the fold's start was compared rather than assumed.
    Asserted,
    /// Nothing does: the fold began from an invented zero.
    Unasserted,
}

impl OpeningAnchor {
    /// Machine-readable code for the API (§13).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Asserted => "asserted",
            Self::Unasserted => "unasserted",
        }
    }
}

/// The anchor state of every account, currency and holding in one journal.
///
/// Built once from the effective set and queried per figure. It is an index
/// rather than a function over the journal because both callers ask it many
/// times over one journal — once per account and currency for the balances
/// answer, once per claim for reconciliation — and a rule re-derived per
/// question is a rule that can be given a different journal each time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpeningAnchors {
    /// The first date a cash leg moved, per account and currency.
    first_cash: BTreeMap<(AccountId, CurrencyCode), Date>,
    /// The first date a security leg moved, per account, instrument and custody.
    first_security: BTreeMap<(AccountId, InstrumentId, CustodyId), Date>,
    /// The first date a §10.7 reconstructed opening stated a starting amount,
    /// under the same keys. Compared against the key's first movement rather
    /// than assumed to be it: a reconstruction recorded *after* an ordinary
    /// transaction states the state before itself, not before the journal.
    reconstructed_cash: BTreeMap<(AccountId, CurrencyCode), Date>,
    reconstructed_position: BTreeMap<(AccountId, InstrumentId, CustodyId), Date>,
    /// The start of every interval an opening cash assertion speaks about.
    /// A `Vec` and not a map of minima: the earliest one wins, and taking the
    /// minimum here would hide from a reader that several may cover.
    cash_openings: Vec<(AccountId, CurrencyCode, Date)>,
    position_openings: Vec<(AccountId, InstrumentId, CustodyId, Date)>,
}

impl OpeningAnchors {
    /// Index the journal.
    ///
    /// `events` is the **already-resolved** effective set, and the `&[&Event]`
    /// shape is the reminder: a retracted movement is not this account's first
    /// one, and a retracted assertion anchors nothing.
    ///
    /// The logic is deliberately not in a constructor named `new`:
    /// `cargo-mutants` silently skips functions with that name (§15.7).
    #[must_use]
    pub fn of(events: &[&Event]) -> Self {
        let mut index = Self::default();
        for event in events {
            let Some(date) = event.dates.effective_date() else {
                continue;
            };
            index.record_movements(event, date);
            index.record_assertion(event);
        }
        index
    }

    /// Whether the cash fold for one account and currency began from an
    /// asserted state.
    ///
    /// An account and currency with no cash movement at all cannot be anchored
    /// by anything — there is no first movement for an assertion to reach back
    /// past — so it reads as unasserted. That is not the same as saying its
    /// balance is unknowable: a figure the fold never produced is not a fold,
    /// and the caller decides what to do about an absence separately.
    #[must_use]
    pub fn cash(&self, account: AccountId, currency: CurrencyCode) -> OpeningAnchor {
        let Some(first) = self.first_cash.get(&(account, currency)) else {
            return OpeningAnchor::Unasserted;
        };
        anchored(
            self.cash_openings.iter().filter_map(|(owner, code, from)| {
                (*owner == account && *code == currency).then_some(*from)
            }),
            self.reconstructed_cash.get(&(account, currency)).copied(),
            *first,
        )
    }

    /// The same question for one holding: an account's quantity of one
    /// instrument in one depository.
    ///
    /// Keyed by depository as well as instrument because that is what a
    /// position assertion names and what a position figure is about: the same
    /// quantity in another depository is a different position (§4.5).
    #[must_use]
    pub fn position(
        &self,
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
    ) -> OpeningAnchor {
        let Some(first) = self.first_security.get(&(account, instrument, custody)) else {
            return OpeningAnchor::Unasserted;
        };
        anchored(
            self.position_openings
                .iter()
                .filter_map(|(owner, held, where_held, from)| {
                    (*owner == account && *held == instrument && *where_held == custody)
                        .then_some(*from)
                }),
            self.reconstructed_position
                .get(&(account, instrument, custody))
                .copied(),
            *first,
        )
    }

    /// Legs are read by the **leg's** account, not the event's: a transfer
    /// between two accounts is one event and moves cash on both, and it is the
    /// leg that the projection accumulates.
    fn record_movements(&mut self, event: &Event, date: Date) {
        let reconstructed = matches!(
            event.kind,
            EventKind::OpeningCash { .. } | EventKind::OpeningPosition { .. }
        );
        for leg in &event.legs {
            if let Some(money) = leg.cash_effect() {
                let key = (leg.account, money.currency());
                earliest(&mut self.first_cash, key, date);
                if reconstructed {
                    earliest(&mut self.reconstructed_cash, key, date);
                }
            }
            if leg.kind == LegKind::SecurityQuantity
                && let (Some(instrument), Some(custody)) = (leg.instrument, leg.custody)
            {
                let key = (leg.account, instrument, custody);
                earliest(&mut self.first_security, key, date);
                if reconstructed {
                    earliest(&mut self.reconstructed_position, key, date);
                }
            }
        }
    }

    /// An assertion is read by the **event's** account: it has no legs to read,
    /// which is the point — it moves no money (§10.3).
    ///
    /// The date recorded is the start of the interval the assertion speaks
    /// about, not the date the document was filed. An opening balance states
    /// the state before the first event of its interval, so `period.from` is
    /// what has to reach back far enough.
    fn record_assertion(&mut self, event: &Event) {
        let EventKind::ControlAssertion { period, claim } = event.kind else {
            return;
        };
        match claim {
            ControlClaim::CashBalance {
                currency,
                at: BalancePoint::Opening,
                ..
            } => self
                .cash_openings
                .push((event.account, currency, period.from)),
            ControlClaim::PositionQuantity {
                instrument,
                custody,
                at: BalancePoint::Opening,
                ..
            } => self
                .position_openings
                .push((event.account, instrument, custody, period.from)),
            // A closing assertion states where the interval ended, which is the
            // claim side of the check and not a statement about where the fold
            // began. Interval totals state no state at all.
            _ => {}
        }
    }
}

/// Whether anything reaches back to the first movement.
///
/// An assertion that opens *after* the first movement leaves everything before
/// it unasserted, and the sum is still a running one — which is the case
/// `iaam-c6f0` is about and exactly why it is not treated as an anchor here. A
/// reconstructed opening recorded after the first movement is the same
/// situation: it states the state before itself, and the transactions already
/// folded in before it still came from nowhere.
fn anchored(
    mut asserted_from: impl Iterator<Item = Date>,
    reconstructed_at: Option<Date>,
    first_movement: Date,
) -> OpeningAnchor {
    if asserted_from.any(|from| from <= first_movement)
        || reconstructed_at.is_some_and(|at| at <= first_movement)
    {
        OpeningAnchor::Asserted
    } else {
        OpeningAnchor::Unasserted
    }
}

fn earliest<K: Ord>(into: &mut BTreeMap<K, Date>, key: K, date: Date) {
    into.entry(key)
        .and_modify(|known| {
            if date < *known {
                *known = date;
            }
        })
        .or_insert(date);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::money::{Money, PostedMinor};
    use crate::reconciliation::claim::AssertionPeriod;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn inflow(account: AccountId, day: time::Date) -> crate::event::Event {
        event_with(
            account,
            day,
            1,
            EventKind::CashIn {
                amount: rub(100_000),
            },
            vec![Leg::cash(account, rub(100_000))],
        )
    }

    /// An opening cash assertion whose interval starts on `from`.
    fn opening_from(account: AccountId, from: time::Date) -> crate::event::Event {
        event_with(
            account,
            from,
            0,
            EventKind::ControlAssertion {
                period: AssertionPeriod::between(from, date!(2026 - 12 - 31)).unwrap(),
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: BalancePoint::Opening,
                },
            },
            Vec::new(),
        )
    }

    fn anchors(events: &[crate::event::Event]) -> OpeningAnchors {
        OpeningAnchors::of(&events.iter().collect::<Vec<_>>())
    }

    #[test]
    fn an_opening_assertion_anchors_only_what_it_reaches_back_over() {
        // An opening assertion states the balance before the first event of its
        // own interval. It therefore anchors the accumulation exactly when that
        // interval starts no later than the first movement; one that opens after
        // the movement leaves everything before it unasserted, and the figure is
        // still a running sum. This is the whole of `iaam-c6f0`: a proven
        // balance in the middle of a history does not explain what precedes it.
        let account = AccountId::new_random();
        let first = date!(2026 - 08 - 05);
        let movement = inflow(account, first);

        let covering = anchors(&[
            movement.clone(),
            opening_from(account, date!(2026 - 08 - 01)),
        ]);
        assert_eq!(
            covering.cash(account, CurrencyCode::Rub),
            OpeningAnchor::Asserted
        );

        // The boundary: an interval opening on the day of the first movement
        // still speaks about the state before it.
        let on_the_day = anchors(&[movement.clone(), opening_from(account, first)]);
        assert_eq!(
            on_the_day.cash(account, CurrencyCode::Rub),
            OpeningAnchor::Asserted
        );

        let a_day_late = anchors(&[
            movement.clone(),
            opening_from(account, date!(2026 - 08 - 06)),
        ]);
        assert_eq!(
            a_day_late.cash(account, CurrencyCode::Rub),
            OpeningAnchor::Unasserted
        );

        assert_eq!(
            anchors(&[movement]).cash(account, CurrencyCode::Rub),
            OpeningAnchor::Unasserted
        );
    }

    #[test]
    fn an_assertion_is_shared_with_no_other_account_or_currency() {
        // An anchor is a statement about one account's one currency. Letting it
        // spread would anchor figures nobody has said anything about, which is
        // the failure this whole module exists to prevent, committed wholesale.
        let account = AccountId::new_random();
        let other = AccountId::new_random();
        let first = date!(2026 - 08 - 05);
        let dollars = Money::new(PostedMinor::new(100_000), CurrencyCode::Usd);
        let index = anchors(&[
            inflow(account, first),
            event_with(
                account,
                first,
                2,
                EventKind::CashIn { amount: dollars },
                vec![Leg::cash(account, dollars)],
            ),
            inflow(other, first),
            opening_from(account, date!(2026 - 08 - 01)),
        ]);

        assert_eq!(
            index.cash(account, CurrencyCode::Rub),
            OpeningAnchor::Asserted
        );
        assert_eq!(
            index.cash(other, CurrencyCode::Rub),
            OpeningAnchor::Unasserted,
            "an assertion is not shared between accounts"
        );
        assert_eq!(
            index.cash(account, CurrencyCode::Usd),
            OpeningAnchor::Unasserted,
            "nor between currencies of one account"
        );
    }

    #[test]
    fn a_transfer_is_read_by_the_leg_rather_than_by_the_event() {
        // One event, two accounts, and only one of them is the event's own. The
        // projection accumulates legs, so the first movement has to be found the
        // same way — otherwise the receiving account's fold would be anchored by
        // an assertion that reaches back past a movement the index never saw.
        let sender = AccountId::new_random();
        let receiver = AccountId::new_random();
        let day = date!(2026 - 08 - 05);
        let amount = rub(50_000);
        let transfer = event_with(
            sender,
            day,
            1,
            EventKind::CashTransfer {
                transfer_id: crate::ids::TransferId::new_random(),
                from: sender,
                to: receiver,
                amount,
            },
            vec![Leg::cash(sender, rub(-50_000)), Leg::cash(receiver, amount)],
        );
        let index = anchors(&[transfer, opening_from(receiver, date!(2026 - 08 - 10))]);
        assert_eq!(
            index.cash(receiver, CurrencyCode::Rub),
            OpeningAnchor::Unasserted,
            "the receiving leg is a movement, and an assertion after it anchors nothing"
        );
    }

    #[test]
    fn each_anchor_state_has_a_distinct_code() {
        // The two are published side by side with the balances answer's
        // `movement_since_unknown_start`, and one code for both would make the
        // distinction unreadable exactly where it matters.
        assert_eq!(OpeningAnchor::Asserted.code(), "asserted");
        assert_eq!(OpeningAnchor::Unasserted.code(), "unasserted");
        assert_ne!(
            OpeningAnchor::Asserted.code(),
            OpeningAnchor::Unasserted.code()
        );
    }
}
