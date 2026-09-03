//! Reconciliation of a source assertion against observed data (§10.3, §10.4).
//!
//! **No tolerance is allowed.** Both sides are posted amounts in minor
//! currency units, and a one-kopeck difference is a difference. A discrepancy
//! threshold exists where a calculated value is compared with a
//! posted value (deposit interest accruals, §8.3) — that is E3, and the threshold there
//! comes from the contract's rounding algorithm rather than being set here.

use super::anchor::OpeningAnchor;
use super::claim::ControlClaim;
use super::observed::{ObservedTotals, Turnover};
use crate::money::{CurrencyCode, PostedMinor, Quantity};
use crate::numeric::decimal::Dec;

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
            Some(_) if observed.cash_anchor(currency) == Some(OpeningAnchor::Unasserted) => {
                ClaimOutcome::NotComparable {
                    reason: NotComparable::OpeningNotAsserted,
                }
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
            opening_anchor(account),
            event_with(
                account,
                date!(2026 - 03 - 10),
                1,
                EventKind::CashIn { amount: rub(minor) },
                vec![Leg::cash(account, rub(minor))],
            ),
        ]
    }

    /// A source's opening assertion for March, reaching the start of the
    /// interval. No legs: an assertion moves no money (§10.3).
    fn opening_anchor(account: AccountId) -> crate::event::Event {
        event_with(
            account,
            date!(2026 - 03 - 01),
            0,
            EventKind::ControlAssertion {
                period: march(),
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
