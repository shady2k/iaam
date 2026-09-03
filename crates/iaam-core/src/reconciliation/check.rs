//! Reconciliation of a source assertion against observed data (§10.3, §10.4).
//!
//! **No tolerance is allowed.** Both sides are posted amounts in minor
//! currency units, and a one-kopeck difference is a difference. A discrepancy
//! threshold exists where a calculated value is compared with a
//! posted value (deposit interest accruals, §8.3) — that is E3, and the threshold there
//! comes from the contract's rounding algorithm rather than being set here.

use super::anchor::OpeningAnchor;
use super::claim::{BalancePoint, ControlClaim};
use super::observed::{Baseline, FoldSpan, ObservedTotals, Turnover};
use crate::money::{CurrencyCode, PostedMinor, Quantity};
use crate::numeric::decimal::Dec;
use time::Date;

/// Value on one side of the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimValue {
    Money {
        amount: PostedMinor,
        currency: CurrencyCode,
    },
    Quantity(Quantity),
}

/// Discrepancy: what was asserted, what was observed, and the difference.
///
/// The difference is calculated as asserted minus observed: a positive value
/// means «the source sees more than we do».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discrepancy {
    /// The assertion field that did not match. For turnovers, identifies
    /// the side: `debit` or `credit`.
    pub field: &'static str,
    pub claimed: ClaimValue,
    pub observed: ClaimValue,
    pub delta: ClaimValue,
}

/// Why comparison is impossible.
///
/// Inability to compare is **not** a discrepancy. A discrepancy means
/// «the numbers do not match; investigate»; inability means «there is
/// nothing to compare against», and these are different answers to the owner (§10.4).
///
/// **Deliberately not [`crate::batch::NoCounterpart`]**, which answers the same
/// shape of question about the other fold and was re-examined for merging in
/// `iaam-tx3c`. The two were kept apart, and the sharpest way to see why is to
/// ask which side of the comparison is missing. Every reason here is a fact
/// about the **observed** side, produced from a ledger over events that were
/// recorded: the journal holds no events, no tax facts, or no asserted start.
/// `NoCounterpart` is a fact about the **claimed** side of a batch that may
/// never reach the journal at all: the source printed no opening balance, so the
/// document does not supply the term its own closing figure would be checked
/// against. One vocabulary would have to mean both, and then a client could read
/// «no journal coverage» off an import that has not touched the journal, or be
/// pointed at a missing document line while reconciling months of recorded
/// history. The remedies differ for the same reason — import the history or
/// assert an opening, against fetch a statement that prints its control section
/// in full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotComparable {
    /// The account has no events at all: there is nothing to confirm.
    NoJournalCoverage,
    /// The system does not yet record tax facts (E5).
    TaxFactsNotRecorded,
    /// The observed figure is a sum from a start nothing asserts, so it is
    /// movement over the recorded interval and not a balance.
    ///
    /// **This is the reason the owner's own anchor used to be called wrong.**
    /// An account whose journal begins in January was treated as having held
    /// exactly zero on the first of January, and a balance he could prove for
    /// the first of August was then compared against `0 + everything since` and
    /// reported `discrepant`. The claim was right and the baseline was invented,
    /// and the system had no vocabulary for saying so — while the balances
    /// answer, over the same silence, already refused to call the same fold a
    /// balance and published it as `movement_since_unknown_start` (`iaam-d7hn`).
    ///
    /// It is not a discrepancy for the reason `TaxFactsNotRecorded` is not one:
    /// «the numbers do not match» sends the owner to find an error, and there is
    /// none to find. It is also not a defect he is asked to repair by restating
    /// the balance — the repair is an opening assertion reaching back to the
    /// start of the recorded history, or the import of the history before it.
    OpeningNotAsserted,
}

impl NotComparable {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoJournalCoverage => "no_journal_coverage",
            Self::TaxFactsNotRecorded => "tax_facts_not_recorded",
            Self::OpeningNotAsserted => "opening_not_asserted",
        }
    }
}

/// A discrepancy explained by the perimeter boundary (§11).
///
/// Exists so that the owner is not assigned a task to «fix» something that
/// the system intentionally does not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationException {
    /// The quantities differ because the securities are encumbered under REPO.
    UnsupportedRepoEncumbrance,
    /// The period includes financing outside the perimeter (margin).
    UnsupportedFinancingPresent,
}

impl ReconciliationException {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedRepoEncumbrance => "unsupported_repo_encumbrance",
            Self::UnsupportedFinancingPresent => "unsupported_financing_present",
        }
    }
}

/// What the observed side of one comparison was: the fold it came out of, and
/// what that fold began from.
///
/// **A discrepancy used to state three numbers and nothing about where the
/// middle one came from.** An owner facing `discrepant` on a figure he had
/// confirmed had no way to ask the system what it had added up: he summed every
/// leg of the account by hand, over the whole period, and spent five iterations
/// guessing why the total did not match (`iaam-lg2t`). The system held the fold
/// the entire time.
///
/// It is carried by **every** outcome and not only by discrepancies. A `matched`
/// says what the confirmation covers — a balance folded over one imported month
/// is not the same evidence as one folded over four years — and a
/// `not_comparable` needs it most of all, because the fold's start is the whole
/// of its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationBasis {
    /// The events folded into the observed figure, and the dates they span.
    pub folded: FoldSpan,
    /// What the fold began from.
    pub start: ObservedStart,
    /// Which quantity was put against the claim.
    pub compared: Compared,
}

/// What was actually put against the claim.
///
/// A level and a change are different findings and must not be reported alike.
/// «Your closing balance matches» and «the movements since your August
/// statement account exactly for the distance to it» are both `matched`, and
/// only this field tells them apart — the second says nothing about the level,
/// which remains unknown while the fold's start is unasserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compared {
    /// The figure itself, against the fold.
    Level,
    /// The change since an earlier balance a source stated, carrying the date
    /// of that statement. Comparable over a fold nothing anchors, because the
    /// unknown start is in both figures and cancels out of their difference
    /// (`iaam-c6f0`).
    ChangeSince(Date),
}

impl Compared {
    /// Machine-readable code for the API (§13).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Level => "level",
            Self::ChangeSince(_) => "change_since_stated_balance",
        }
    }

    /// The date of the stated balance a change is measured from, where there is
    /// one.
    #[must_use]
    pub const fn since(self) -> Option<Date> {
        match self {
            Self::Level => None,
            Self::ChangeSince(date) => Some(date),
        }
    }
}

/// What the observed figure was accumulated from.
///
/// Four answers rather than an anchored/unanchored flag, because a flag would
/// have to lie twice: an interval total starts from no state at all, and a
/// currency the account has never moved has no sum whose start could have been
/// invented. Reporting either as «unasserted» would send a reader looking for a
/// missing opening assertion that would change nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedStart {
    /// The journal states what was held before the first movement folded in, so
    /// the figure is a balance.
    Asserted,
    /// Nothing states it. The figure is the movement over the recorded
    /// interval, and this is the reason a balance claim is not comparable.
    Unasserted,
    /// The journal has never moved this currency or this holding, so what the
    /// claim was compared against is the absence of any record rather than a
    /// sum. Zero here is the whole of what the journal says, not a placeholder
    /// for an unknown amount.
    NoRecordedMovement,
    /// The claim is an interval total — a turnover, a fee, income, withheld tax
    /// — and a total starts from no state. Asking after its opening is a
    /// category error, which is why it has its own answer instead of a null.
    NotABalance,
}

impl ObservedStart {
    /// Machine-readable code for the API (§13).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Asserted => "asserted",
            Self::Unasserted => "unasserted",
            Self::NoRecordedMovement => "no_recorded_movement",
            Self::NotABalance => "not_a_balance",
        }
    }
}

/// What one claim is compared against, stated so it can be published beside the
/// outcome.
///
/// Kept beside [`check_claim`] and reading the same two accessors, so that the
/// window named to the owner is the window the comparison used. Deriving it in
/// the transport from the interval's boundaries would have named a window the
/// figure does not come from: a March closing balance over a journal that begins
/// in February is folded from February.
#[must_use]
pub fn observation_basis(claim: &ControlClaim, observed: &ObservedTotals) -> ObservationBasis {
    match *claim {
        ControlClaim::CashBalance { currency, at, .. } => ObservationBasis {
            folded: folded_to(at, observed),
            start: start_of(observed.cash_anchor(currency)),
            compared: cash_compared(observed, at, currency),
        },
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            at,
            ..
        } => ObservationBasis {
            folded: folded_to(at, observed),
            start: start_of(observed.position_anchor(instrument, custody)),
            compared: Compared::Level,
        },
        ControlClaim::CashTurnover { .. }
        | ControlClaim::FeesTotal { .. }
        | ControlClaim::IncomeTotal { .. }
        | ControlClaim::TaxWithheldTotal { .. } => ObservationBasis {
            folded: observed.folded_within(),
            start: ObservedStart::NotABalance,
            compared: Compared::Level,
        },
    }
}

/// An opening figure comes out of what preceded the interval; a closing figure
/// out of that and the interval both.
fn folded_to(at: BalancePoint, observed: &ObservedTotals) -> FoldSpan {
    match at {
        BalancePoint::Opening => observed.folded_before(),
        BalancePoint::Closing => observed.folded_before().merge(observed.folded_within()),
    }
}

/// A level where the fold's start is asserted; the change since an earlier
/// stated balance where it is not and there is one.
///
/// The two are decided here and read by [`check_claim`] through
/// [`cash_baseline_for`], so what the outcome compared and what the basis says
/// it compared cannot come apart.
fn cash_compared(observed: &ObservedTotals, at: BalancePoint, currency: CurrencyCode) -> Compared {
    cash_baseline_for(observed, at, currency).map_or(Compared::Level, |baseline| {
        Compared::ChangeSince(baseline.at.date)
    })
}

/// The earlier stated balance this claim is measured from, where the level
/// cannot be compared and one exists.
///
/// `None` where the fold's start **is** asserted: there the level is the better
/// comparison, and falling back to a change would discard the stronger finding
/// for the weaker one.
fn cash_baseline_for(
    observed: &ObservedTotals,
    at: BalancePoint,
    currency: CurrencyCode,
) -> Option<Baseline> {
    if observed.cash_at(at, currency).is_none()
        || observed.cash_anchor(currency) != Some(OpeningAnchor::Unasserted)
    {
        return None;
    }
    observed.cash_baseline(at, currency)
}

const fn start_of(anchor: Option<OpeningAnchor>) -> ObservedStart {
    match anchor {
        Some(OpeningAnchor::Asserted) => ObservedStart::Asserted,
        Some(OpeningAnchor::Unasserted) => ObservedStart::Unasserted,
        None => ObservedStart::NoRecordedMovement,
    }
}

/// Result of reconciling one assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Matched,
    Discrepant(Discrepancy),
    NotComparable {
        reason: NotComparable,
    },
    /// The discrepancy is explained by the perimeter boundary and requires no action
    /// from the owner (§11). It does not justify elevating the status.
    Excepted {
        exception: ReconciliationException,
    },
}

impl ClaimOutcome {
    /// Does the outcome allow the measurement status to be elevated.
    ///
    /// A perimeter exception does not grant that right: «we know why it does not match» —
    /// does not mean «it matched».
    #[must_use]
    pub const fn confirms(&self) -> bool {
        match self {
            Self::Matched => true,
            Self::Discrepant(_) | Self::NotComparable { .. } | Self::Excepted { .. } => false,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::Discrepant(_) => "discrepant",
            Self::NotComparable { .. } => "not_comparable",
            Self::Excepted { .. } => "excepted",
        }
    }
}

/// Reconciliation of one assertion against observed values.
#[must_use]
pub fn check_claim(claim: &ControlClaim, observed: &ObservedTotals) -> ClaimOutcome {
    if observed.events_seen() == 0 {
        return ClaimOutcome::NotComparable {
            reason: NotComparable::NoJournalCoverage,
        };
    }
    match *claim {
        // A figure the fold produced can only be compared where the fold's
        // start is asserted. Where no fold produced a figure at all — the
        // account has never moved this currency — zero is not a placeholder for
        // an unknown amount but the whole of what the journal records, and the
        // comparison stands: see
        // `a_currency_without_movement_is_compared_as_zero_when_history_exists`.
        ControlClaim::CashBalance {
            currency,
            amount,
            at,
        } => match observed.cash_at(at, currency) {
            // The level rests on an invented start. The change since an earlier
            // stated balance does not — the same unknown start is in both folds
            // and cancels out of their difference — so where a source has
            // stated one, that is compared instead of nothing (`iaam-c6f0`).
            Some(seen) if observed.cash_anchor(currency) == Some(OpeningAnchor::Unasserted) => {
                cash_baseline_for(observed, at, currency).map_or(
                    ClaimOutcome::NotComparable {
                        reason: NotComparable::OpeningNotAsserted,
                    },
                    |baseline| compare_change(currency, amount, seen, baseline),
                )
            }
            seen => compare_money(
                "amount",
                currency,
                amount,
                seen.unwrap_or(PostedMinor::new(0)),
            ),
        },
        // The same, for the same reason. A quantity summed from an unasserted
        // start is the net of the trades that were imported, and a source
        // stating the holding is not contradicting it — there is nothing for it
        // to contradict. The two arms are written out rather than shared: the
        // anchor is keyed by currency for cash and by instrument-and-depository
        // for a holding, and a helper over both would have to invent a key that
        // is neither.
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at,
        } => match observed.position_at(at, instrument, custody) {
            Some(_)
                if observed.position_anchor(instrument, custody)
                    == Some(OpeningAnchor::Unasserted) =>
            {
                ClaimOutcome::NotComparable {
                    reason: NotComparable::OpeningNotAsserted,
                }
            }
            seen => compare_quantity(quantity, seen.unwrap_or_else(Quantity::zero)),
        },
        ControlClaim::CashTurnover {
            currency,
            debit,
            credit,
        } => {
            let Turnover {
                debit: seen_debit,
                credit: seen_credit,
            } = observed.turnover(currency).unwrap_or_default();
            match compare_money("debit", currency, debit, seen_debit) {
                ClaimOutcome::Matched => compare_money("credit", currency, credit, seen_credit),
                other => other,
            }
        }
        ControlClaim::FeesTotal { currency, amount } => compare_money(
            "amount",
            currency,
            amount,
            observed.fees(currency).unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::IncomeTotal { currency, amount } => compare_money(
            "amount",
            currency,
            amount,
            observed.income(currency).unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::TaxWithheldTotal { currency, amount } => {
            if observed.tax_facts_recorded() {
                compare_money(
                    "amount",
                    currency,
                    amount,
                    observed
                        .tax_withheld(currency)
                        .unwrap_or(PostedMinor::new(0)),
                )
            } else {
                ClaimOutcome::NotComparable {
                    reason: NotComparable::TaxFactsNotRecorded,
                }
            }
        }
    }
}

/// Comparison of the change since an earlier stated balance.
///
/// Both differences are taken the same way — later minus earlier — so the
/// unknown start the two folds share cancels out of the observed side exactly
/// as the source's own unstated history cancels out of the claimed side.
///
/// The field is named for what it is. Reporting `amount` here would print two
/// numbers that are neither side's balance under the name the level comparison
/// uses, and the reader would take a change for a holding.
///
/// An overflow in either difference is reported by saturation, for the reason
/// [`compare_money`] gives: a gap wider than the monetary type is a discrepancy
/// in any case, and a panic here would refuse the answer instead of stating it.
fn compare_change(
    currency: CurrencyCode,
    claimed: PostedMinor,
    observed: PostedMinor,
    baseline: Baseline,
) -> ClaimOutcome {
    compare_money(
        "change_since_stated_balance",
        currency,
        PostedMinor::new(claimed.raw().saturating_sub(baseline.claimed.raw())),
        PostedMinor::new(observed.raw().saturating_sub(baseline.observed.raw())),
    )
}

/// Comparison of posted amounts. Exact: no tolerance.
fn compare_money(
    field: &'static str,
    currency: CurrencyCode,
    claimed: PostedMinor,
    observed: PostedMinor,
) -> ClaimOutcome {
    if claimed == observed {
        return ClaimOutcome::Matched;
    }
    // Difference overflow means that the gap between the values
    // exceeds the range of the monetary type: it is a discrepancy in any case,
    // and is reported using saturation rather than a panic.
    let delta = claimed.raw().saturating_sub(observed.raw());
    ClaimOutcome::Discrepant(Discrepancy {
        field,
        claimed: ClaimValue::Money {
            amount: claimed,
            currency,
        },
        observed: ClaimValue::Money {
            amount: observed,
            currency,
        },
        delta: ClaimValue::Money {
            amount: PostedMinor::new(delta),
            currency,
        },
    })
}

fn compare_quantity(claimed: Quantity, observed: Quantity) -> ClaimOutcome {
    if claimed == observed {
        return ClaimOutcome::Matched;
    }
    // An uncomputable difference is still a discrepancy: the sides
    // have already been identified, and reporting an inability to compare would mean losing the
    // fact of the mismatch itself.
    let delta = claimed
        .0
        .checked_sub(observed.0)
        .unwrap_or_else(|_| Dec::zero());
    ClaimOutcome::Discrepant(Discrepancy {
        field: "quantity",
        claimed: ClaimValue::Quantity(claimed),
        observed: ClaimValue::Quantity(observed),
        delta: ClaimValue::Quantity(Quantity(delta)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::Money;
    use crate::reconciliation::claim::{AssertionPeriod, BalancePoint};
    use crate::reconciliation::observed::observe as observe_effective;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }
    fn observe(
        events: &[Event],
        account: AccountId,
        period: AssertionPeriod,
    ) -> Result<ObservedTotals, super::super::observed::ObserveError> {
        let effective: Vec<&Event> = events.iter().collect();
        observe_effective(&effective, account, period)
    }

    /// One March deposit on an account whose opening is anchored.
    ///
    /// The anchor is part of the fixture and not an extra in the tests that
    /// happen to need it. Without one, every figure below is a sum from a start
    /// nothing states, no balance can be compared at all, and the tests would be
    /// exercising `OpeningNotAsserted` while claiming to exercise the
    /// comparison. Its amount is zero because the account has no history before
    /// March; its *presence*, not its value, is what anchors (`iaam-d7hn`).
    fn journal_with_one_deposit(account: AccountId, minor: i64) -> Vec<crate::event::Event> {
        vec![
            opening_anchor(account, date!(2026 - 03 - 01)),
            event_with(
                account,
                date!(2026 - 03 - 10),
                1,
                EventKind::CashIn { amount: rub(minor) },
                vec![Leg::cash(account, rub(minor))],
            ),
        ]
    }

    /// A source's opening assertion reaching back to `from`. No legs: an
    /// assertion moves no money (§10.3).
    ///
    /// The date is a parameter because reaching back far enough is the whole of
    /// what makes an assertion an anchor: one that opens after the first
    /// movement leaves everything before it unasserted.
    fn opening_anchor(account: AccountId, from: time::Date) -> crate::event::Event {
        event_with(
            account,
            from,
            0,
            EventKind::ControlAssertion {
                period: AssertionPeriod::between(from, date!(2026 - 03 - 31)).unwrap(),
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: BalancePoint::Opening,
                },
            },
            Vec::new(),
        )
    }

    #[test]
    fn an_exact_match_is_accepted_and_one_kopeck_is_not() {
        // No tolerance. Both sides are posted amounts in minor
        // units; «off by only one kopeck» means a missing
        // kopeck, and a missing kopeck is a posting error
        // that accumulates over a long history.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();

        let exact = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&exact, &observed), ClaimOutcome::Matched);

        let off_by_one = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_001),
            at: BalancePoint::Closing,
        };
        let outcome = check_claim(&off_by_one, &observed);
        let ClaimOutcome::Discrepant(discrepancy) = outcome else {
            panic!("a one-kopeck difference must be a discrepancy: {outcome:?}");
        };
        assert_eq!(discrepancy.field, "amount");
        assert_eq!(
            discrepancy.delta,
            ClaimValue::Money {
                amount: PostedMinor::new(1),
                currency: CurrencyCode::Rub
            },
            "the difference is calculated as asserted minus observed"
        );
    }

    #[test]
    fn an_anchor_over_an_unanchored_history_is_not_a_discrepancy() {
        // The defect this outcome exists for (`iaam-d7hn`). The account's
        // journal begins in February with an ordinary inflow — nothing states
        // what was there before it — and the owner then states the balance he
        // can prove for the first of April. The fold before April starts from an
        // invented zero, so «zero plus everything since» is not a balance and
        // cannot contradict him. Calling it `discrepant` sent him to look for an
        // error the system had made itself, and told him the figure he had
        // confirmed against two sources was wrong.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 02 - 10),
                1,
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 20),
                1,
                EventKind::CashOut {
                    amount: rub(-30_000),
                },
                vec![Leg::cash(account, rub(-30_000))],
            ),
        ];
        let april = AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30)).unwrap();
        let observed = observe(&events, account, april).unwrap();

        let anchor = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(500_000),
            at: BalancePoint::Opening,
        };
        assert_eq!(
            check_claim(&anchor, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::OpeningNotAsserted
            },
            "the claim is right and the baseline is invented"
        );

        // Nor does the observed figure become right by agreeing with it: an
        // outcome of `matched` here would be a match against a number the
        // system has no grounds for, which is the same defect wearing the
        // opposite verdict.
        let agreeing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(70_000),
            at: BalancePoint::Opening,
        };
        assert_eq!(
            check_claim(&agreeing, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::OpeningNotAsserted
            }
        );
    }

    /// A journal that begins mid-history: one inflow in February, nothing
    /// stating what preceded it.
    fn unanchored_history(account: AccountId) -> Vec<crate::event::Event> {
        vec![
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
        ]
    }

    /// A source stating a cash balance at one point of one interval.
    fn stated_balance(
        account: AccountId,
        period: AssertionPeriod,
        at: BalancePoint,
        minor: i64,
    ) -> crate::event::Event {
        event_with(
            account,
            match at {
                BalancePoint::Opening => period.from,
                BalancePoint::Closing => period.to,
            },
            5,
            EventKind::ControlAssertion {
                period,
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(minor),
                    at,
                },
            },
            Vec::new(),
        )
    }

    #[test]
    fn a_stated_balance_becomes_the_baseline_for_what_follows_it() {
        // `iaam-c6f0`. Nothing anchors the start of this journal, so no level can
        // be compared — but the source states the balance at the start of March
        // as well as at its end, and the *change* between two stated balances is
        // comparable over any history: the unknown start is in both folds and
        // cancels out of their difference.
        //
        // Here March records one inflow of 500.00, and the source says the
        // balance rose from 9 000.00 to 9 500.00. The system knows neither
        // figure and can still say the movements account for the distance.
        let account = AccountId::new_random();
        let mut events = unanchored_history(account);
        events.push(stated_balance(
            account,
            march(),
            BalancePoint::Opening,
            900_000,
        ));
        let observed = observe(&events, account, march()).unwrap();

        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(950_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&closing, &observed), ClaimOutcome::Matched);

        let basis = observation_basis(&closing, &observed);
        assert_eq!(
            basis.compared,
            Compared::ChangeSince(date!(2026 - 03 - 01)),
            "the outcome says what it compared and from when"
        );
        assert_eq!(
            basis.start,
            ObservedStart::Unasserted,
            "and does not thereby claim the level is known"
        );

        // The stated opening itself is measured from nothing earlier, so it
        // stays incomparable: an anchor explains what follows it, never what
        // precedes it.
        let opening = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(900_000),
            at: BalancePoint::Opening,
        };
        assert_eq!(
            check_claim(&opening, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::OpeningNotAsserted
            }
        );
        assert_eq!(
            observation_basis(&opening, &observed).compared,
            Compared::Level
        );
    }

    #[test]
    fn a_balance_that_contradicts_an_earlier_one_is_a_discrepancy() {
        // The third question `iaam-c6f0` asks. Two stated balances a month
        // apart, and recorded movements of 500.00 between them; the source says
        // the distance is 700.00. Neither figure can be checked on its own and
        // the contradiction between them is certain, so it is reported as one —
        // not silently resolved by treating the later statement as a correction
        // of the earlier. A correction is an explicit act, and the journal has
        // `Relation` for saying so.
        let account = AccountId::new_random();
        let mut events = unanchored_history(account);
        events.push(stated_balance(
            account,
            march(),
            BalancePoint::Opening,
            900_000,
        ));
        let observed = observe(&events, account, march()).unwrap();

        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(970_000),
            at: BalancePoint::Closing,
        };
        let ClaimOutcome::Discrepant(discrepancy) = check_claim(&closing, &observed) else {
            panic!("two stated balances the movements do not join are a discrepancy");
        };
        assert_eq!(
            discrepancy.field, "change_since_stated_balance",
            "the field names the quantity, so a change is never read as a holding"
        );
        assert_eq!(
            discrepancy.delta,
            ClaimValue::Money {
                amount: PostedMinor::new(20_000),
                currency: CurrencyCode::Rub
            },
            "the source claims 200.00 more movement than the journal recorded"
        );
    }

    #[test]
    fn an_anchored_history_is_still_compared_at_the_level() {
        // The change is the weaker finding and is used only where the level
        // cannot be had. Falling back to it on an anchored history would discard
        // a confirmation of the holding for a confirmation of the movement.
        let account = AccountId::new_random();
        let mut events = journal_with_one_deposit(account, 100_000);
        events.push(stated_balance(
            account,
            march(),
            BalancePoint::Opening,
            900_000,
        ));
        let observed = observe(&events, account, march()).unwrap();
        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&closing, &observed), ClaimOutcome::Matched);
        assert_eq!(
            observation_basis(&closing, &observed).compared,
            Compared::Level
        );
    }

    #[test]
    fn two_sources_disagreeing_about_one_moment_are_no_baseline() {
        // A baseline has to be a fact the journal can lean on. Two statements
        // about the same moment with different figures are not one; picking
        // either would be arbitrary, and the disagreement is already reported
        // where each of them is checked.
        let account = AccountId::new_random();
        let mut events = unanchored_history(account);
        events.push(stated_balance(
            account,
            march(),
            BalancePoint::Opening,
            900_000,
        ));
        events.push(stated_balance(
            account,
            march(),
            BalancePoint::Opening,
            800_000,
        ));
        let observed = observe(&events, account, march()).unwrap();
        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(950_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(
            check_claim(&closing, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::OpeningNotAsserted
            }
        );
    }

    #[test]
    fn a_baseline_from_an_earlier_interval_reaches_across_statements() {
        // The owner's case: a balance he can prove for one date, and everything
        // after it measured from there. February's statement closes at 9 000.00;
        // March's closing figure is checked against it over March's movements,
        // although nothing anchors the journal's start.
        let account = AccountId::new_random();
        let february =
            AssertionPeriod::between(date!(2026 - 02 - 01), date!(2026 - 02 - 28)).unwrap();
        let mut events = unanchored_history(account);
        events.push(stated_balance(
            account,
            february,
            BalancePoint::Closing,
            900_000,
        ));
        let observed = observe(&events, account, march()).unwrap();

        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(950_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&closing, &observed), ClaimOutcome::Matched);
        assert_eq!(
            observation_basis(&closing, &observed).compared,
            Compared::ChangeSince(date!(2026 - 02 - 28))
        );
    }

    #[test]
    fn a_reconstructed_opening_anchors_the_fold_it_begins() {
        // §10.7's reconstructed opening states what the account held before the
        // journal began. It is a recorded fact with legs and provenance, not
        // silence, so the sum that follows it is a balance and a source's
        // closing figure is compared against it. Refusing to compare here would
        // leave an owner who reconstructed his opening unable to have it checked
        // by the very statement that could catch a wrong reconstruction.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 01),
                1,
                EventKind::OpeningCash {
                    amount: rub(200_000),
                },
                vec![Leg::cash(account, rub(200_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 10),
                1,
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
        ];
        let observed = observe(&events, account, march()).unwrap();
        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(300_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&closing, &observed), ClaimOutcome::Matched);
    }

    #[test]
    fn a_reconstructed_opening_recorded_late_anchors_nothing() {
        // The boundary of the rule above. A reconstruction entered after
        // transactions are already in the journal states the state before
        // *itself*; the transactions folded in ahead of it still came from
        // nowhere, and the sum is still a running one.
        let account = AccountId::new_random();
        let events = vec![
            event_with(
                account,
                date!(2026 - 03 - 05),
                1,
                EventKind::CashIn {
                    amount: rub(100_000),
                },
                vec![Leg::cash(account, rub(100_000))],
            ),
            event_with(
                account,
                date!(2026 - 03 - 20),
                1,
                EventKind::OpeningCash {
                    amount: rub(200_000),
                },
                vec![Leg::cash(account, rub(200_000))],
            ),
        ];
        let observed = observe(&events, account, march()).unwrap();
        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(300_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(
            check_claim(&closing, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::OpeningNotAsserted
            }
        );
    }

    #[test]
    fn a_position_summed_from_an_unasserted_start_is_not_compared() {
        // The same rule for a holding, keyed by instrument and depository. A
        // quantity summed from the trades that happen to have been imported is
        // not the position, and a source stating the holding is not
        // contradicting it.
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 11),
            1,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Buy,
                instrument,
                quantity: Quantity(Dec::new(Decimal::from(4))),
                gross: rub(-40_000),
                fee: None,
                basis_fee: None,
                basis_fee_exact: None,
                accrued_interest: None,
            },
            vec![Leg::security(
                account,
                custody,
                instrument,
                Quantity(Dec::new(Decimal::from(4))),
            )],
        )];
        let observed = observe(&events, account, march()).unwrap();
        let claim = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity: Quantity(Dec::new(Decimal::from(4))),
            at: BalancePoint::Closing,
        };
        assert_eq!(
            check_claim(&claim, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::OpeningNotAsserted
            }
        );
    }

    #[test]
    fn a_closing_outcome_names_the_window_it_was_folded_over() {
        // `iaam-lg2t`: the owner told `discrepant` had no way to ask what the
        // system had added up, so he added up the account by hand. The window
        // named is the one the figure comes from — February, where the journal
        // begins — and not the interval asked about.
        let account = AccountId::new_random();
        let events = vec![
            opening_anchor(account, date!(2026 - 02 - 01)),
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

        let closing = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(999),
            at: BalancePoint::Closing,
        };
        let basis = observation_basis(&closing, &observed);
        assert_eq!(
            basis.folded.events, 2,
            "the April inflow is after the interval and is in no fold"
        );
        assert_eq!(basis.folded.first, Some(date!(2026 - 02 - 20)));
        assert_eq!(basis.folded.last, Some(date!(2026 - 03 - 10)));
        assert_eq!(basis.start, ObservedStart::Asserted);

        // The opening figure of the same interval comes out of a narrower fold,
        // and saying so is the difference between «we compared your March
        // opening against February» and «we compared it against March».
        let opening = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(999),
            at: BalancePoint::Opening,
        };
        let basis = observation_basis(&opening, &observed);
        assert_eq!(basis.folded.events, 1);
        assert_eq!(basis.folded.first, Some(date!(2026 - 02 - 20)));
        assert_eq!(basis.folded.last, Some(date!(2026 - 02 - 20)));
    }

    #[test]
    fn an_interval_total_states_that_it_starts_from_no_balance() {
        // A turnover is a flow. Reporting it as accumulated from an unasserted
        // start would send the owner to record an opening assertion that would
        // change nothing about it.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();
        let turnover = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(100_000),
            credit: PostedMinor::new(0),
        };
        let basis = observation_basis(&turnover, &observed);
        assert_eq!(basis.start, ObservedStart::NotABalance);
        assert_eq!(basis.folded.events, 1, "only the interval's own events");
        assert_eq!(basis.folded.first, Some(date!(2026 - 03 - 10)));
    }

    #[test]
    fn an_unanchored_balance_says_so_in_its_basis_as_well_as_its_outcome() {
        // The outcome names the reason and the basis names the fold; the owner
        // needs both, because «not comparable» without the window does not say
        // which stretch of history has no anchor.
        let account = AccountId::new_random();
        let events = vec![event_with(
            account,
            date!(2026 - 02 - 20),
            1,
            EventKind::CashIn {
                amount: rub(100_000),
            },
            vec![Leg::cash(account, rub(100_000))],
        )];
        let observed = observe(&events, account, march()).unwrap();
        let opening = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Opening,
        };
        assert_eq!(
            check_claim(&opening, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::OpeningNotAsserted
            }
        );
        let basis = observation_basis(&opening, &observed);
        assert_eq!(basis.start, ObservedStart::Unasserted);
        assert_eq!(basis.folded.first, Some(date!(2026 - 02 - 20)));
    }

    #[test]
    fn a_key_the_journal_never_moved_is_not_called_unasserted() {
        // The account has an anchored rouble history and no dollar activity at
        // all. Zero dollars is the whole of what the journal says, not a sum
        // from an invented start, and calling it `unasserted` would send the
        // owner after an opening assertion that would confirm nothing.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();
        let dollars = ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount: PostedMinor::new(0),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&dollars, &observed), ClaimOutcome::Matched);
        assert_eq!(
            observation_basis(&dollars, &observed).start,
            ObservedStart::NoRecordedMovement
        );
    }

    #[test]
    fn every_kind_of_start_has_a_distinct_code() {
        let starts = [
            ObservedStart::Asserted,
            ObservedStart::Unasserted,
            ObservedStart::NoRecordedMovement,
            ObservedStart::NotABalance,
        ];
        let mut codes: Vec<&str> = starts.iter().map(|start| start.code()).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "start codes collided");
    }

    #[test]
    fn an_empty_journal_is_not_comparable_rather_than_wrong() {
        // The assertion «the account has 100 000» with an empty journal is not
        // a discrepancy of 100 000: there is nothing to compare against. A discrepancy here
        // would send the owner looking for an error where none exists,
        // when what they need is the needs_reconciliation verdict.
        let account = AccountId::new_random();
        let observed = observe(&[], account, march()).unwrap();
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(
            check_claim(&claim, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::NoJournalCoverage
            }
        );
    }

    #[test]
    fn a_currency_without_movement_is_compared_as_zero_when_history_exists() {
        // The account has history, but no dollar activity. The assertion
        // «the account has 0 USD» is confirmed, while «the account has 500 USD»
        // does not match. Returning NotComparable here would permanently
        // leave any currency with no activity unverifiable.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();

        let zero = ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount: PostedMinor::new(0),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&zero, &observed), ClaimOutcome::Matched);

        let nonzero = ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount: PostedMinor::new(50_000),
            at: BalancePoint::Closing,
        };
        assert!(matches!(
            check_claim(&nonzero, &observed),
            ClaimOutcome::Discrepant(_)
        ));
    }

    #[test]
    fn a_turnover_names_the_side_that_disagrees() {
        // «Turnovers do not match» without identifying the side forces the owner
        // to reconcile both columns manually — exactly the work that §10.2
        // refuses to shift onto them.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();

        let claim = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(100_000),
            credit: PostedMinor::new(700),
        };
        let ClaimOutcome::Discrepant(discrepancy) = check_claim(&claim, &observed) else {
            panic!("an outflow of 700 versus zero must be a discrepancy");
        };
        assert_eq!(discrepancy.field, "credit");
    }

    #[test]
    fn tax_without_tax_facts_is_not_comparable() {
        // No recording path produces tax facts before E5.
        // A zero on our side means «we do not calculate it», and calling
        // 1 300 withheld by the broker a discrepancy would be false.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();
        let claim = ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(130_000),
        };
        assert_eq!(
            check_claim(&claim, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::TaxFactsNotRecorded
            }
        );
    }

    #[test]
    fn a_position_quantity_is_compared_per_custody() {
        // The same quantity in another depository is a different position:
        // a transfer of securities between depositories within the same broker
        // is a real transaction (§4.5).
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(Dec::new(Decimal::from(10)));
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 11),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        )];
        let observed = observe(&events, account, march()).unwrap();

        let matching = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&matching, &observed), ClaimOutcome::Matched);

        let elsewhere = ControlClaim::PositionQuantity {
            instrument,
            custody: CustodyId::new_random(),
            quantity,
            at: BalancePoint::Closing,
        };
        assert!(matches!(
            check_claim(&elsewhere, &observed),
            ClaimOutcome::Discrepant(_)
        ));
    }

    #[test]
    fn each_reason_for_incomparability_has_a_distinct_code() {
        // «Nothing to reconcile» and «there are no tax facts» are different answers
        // to the owner: the first requires stating a balance, while the second requires
        // nothing before E5. Using one code for both would make them indistinguishable.
        assert_eq!(
            NotComparable::NoJournalCoverage.code(),
            "no_journal_coverage"
        );
        assert_eq!(
            NotComparable::TaxFactsNotRecorded.code(),
            "tax_facts_not_recorded"
        );
    }

    #[test]
    fn every_outcome_has_a_distinct_code() {
        let outcomes = [
            ClaimOutcome::Matched,
            ClaimOutcome::NotComparable {
                reason: NotComparable::NoJournalCoverage,
            },
            ClaimOutcome::Excepted {
                exception: ReconciliationException::UnsupportedRepoEncumbrance,
            },
        ];
        let mut codes: Vec<&str> = outcomes.iter().map(ClaimOutcome::code).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count);
        assert_eq!(
            ReconciliationException::UnsupportedFinancingPresent.code(),
            "unsupported_financing_present"
        );
    }

    #[test]
    fn an_exception_neither_confirms_nor_is_a_discrepancy() {
        // §11: «we know why it does not match» — does not mean «it matched».
        let excepted = ClaimOutcome::Excepted {
            exception: ReconciliationException::UnsupportedRepoEncumbrance,
        };
        assert!(!excepted.confirms());
        assert_eq!(excepted.code(), "excepted");
        assert_eq!(
            ReconciliationException::UnsupportedRepoEncumbrance.code(),
            "unsupported_repo_encumbrance"
        );
    }
}
