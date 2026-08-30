//! Reconciliation of a source assertion against observed data (§10.3, §10.4).
//!
//! **No tolerance is allowed.** Both sides are posted amounts in minor
//! currency units, and a one-kopeck difference is a difference. A discrepancy
//! threshold exists where a calculated value is compared with a
//! posted value (deposit interest accruals, §8.3) — that is E3, and the threshold there
//! comes from the contract's rounding algorithm rather than being set here.

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
}

impl NotComparable {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoJournalCoverage => "no_journal_coverage",
            Self::TaxFactsNotRecorded => "tax_facts_not_recorded",
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
        ControlClaim::CashBalance {
            currency,
            amount,
            at,
        } => compare_money(
            "amount",
            currency,
            amount,
            observed
                .cash_at(at, currency)
                .unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at,
        } => compare_quantity(
            quantity,
            observed
                .position_at(at, instrument, custody)
                .unwrap_or_else(Quantity::zero),
        ),
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
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::Money;
    use crate::reconciliation::claim::{AssertionPeriod, BalancePoint};
    use crate::reconciliation::observed::observe;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    fn journal_with_one_deposit(account: AccountId, minor: i64) -> Vec<crate::event::Event> {
        vec![event_with(
            account,
            date!(2026 - 03 - 10),
            1,
            EventKind::CashIn { amount: rub(minor) },
            vec![Leg::cash(account, rub(minor))],
        )]
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
