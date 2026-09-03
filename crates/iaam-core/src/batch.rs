//! A batch of cash movements, totalled before any of it is a journal.
//!
//! An import is read one row at a time and committed all at once, and between
//! those two moments there is a question nobody was in a position to ask: **do
//! these rows add up to what the source says they add up to?** Answering it
//! needs two numbers — the batch's own total, and the figure the source printed
//! in its own control section — and this module produces the first and compares
//! it with the second.
//!
//! The fold is here rather than in the caller because summing money is
//! arithmetic, and `scripts/check-architecture.sh` (§3.1, §13) refuses monetary
//! arithmetic outside the core. The rule earns its keep here: a client asked to
//! add two hundred decimal strings to check one figure will get it wrong, and
//! the client of this system is a language model.
//!
//! **This is not [`crate::reconciliation`], and the difference is not
//! bookkeeping.** Reconciliation folds the journal — facts that were written,
//! corrections resolved, a period bounded by assertions — and asks whether the
//! journal agrees with what a source said afterwards. This folds a batch that is
//! not in the journal and may never be, and its only counterpart is the control
//! section printed on the very same document the rows came from. The two sides
//! therefore share a parser and a document, so agreement here is never
//! independent confirmation of anything; it is the arithmetic check that a
//! transcription is faithful, and no more.

use std::collections::BTreeMap;

use crate::ids::AccountId;
use crate::money::{CurrencyCode, Money, MoneyError, PostedMinor};
use crate::reconciliation::claim::AssertionPeriod;

/// One account and currency's share of a batch.
///
/// The pair is the unit because money in two currencies does not add, and one
/// import routinely carries rows for several accounts. A total over «the batch»
/// would be a single number that no control section anywhere states.
///
/// `debit` and `credit` are **absolute values**, as [`Turnover`] and
/// [`ControlClaim::CashTurnover`] are: the side carries the sign, not the
/// number. `net` is the one signed figure, and it is signed because a batch can
/// legitimately take an account down.
///
/// [`Turnover`]: crate::reconciliation::observed::Turnover
/// [`ControlClaim::CashTurnover`]: crate::reconciliation::claim::ControlClaim::CashTurnover
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchTotal {
    pub account: AccountId,
    pub currency: CurrencyCode,
    /// How many of the batch's movements this total folded.
    pub rows: usize,
    /// What arrived, as a positive number.
    pub debit: PostedMinor,
    /// What left, as a positive number.
    pub credit: PostedMinor,
    /// `debit` minus `credit`: what the batch does to the account.
    pub net: PostedMinor,
}

/// Total a batch of signed movements per account and currency.
///
/// The input is a movement per entry, signed the way the journal will record
/// it, and the caller decides what counts as a movement. Nothing is filtered
/// here: a fold that dropped entries would answer a different question from the
/// one its caller asked, and the caller is the only one who knows whether a row
/// that moved no cash on the account is an omission or a security trade.
///
/// The order is by account and then currency, which is `BTreeMap`'s and not an
/// accident: two calls over the same batch must produce the same list, because
/// the assessment this feeds is fingerprinted and a reordering would refuse a
/// commit that changed nothing.
///
/// Overflow is an error rather than a wrap. A batch whose sum does not fit is a
/// batch nobody can check, and reporting a wrapped total would be reporting a
/// figure that agrees with nothing.
pub fn total(movements: &[(AccountId, Money)]) -> Result<Vec<BatchTotal>, MoneyError> {
    let mut folded: BTreeMap<(AccountId, CurrencyCode), (usize, Money, Money)> = BTreeMap::new();
    for (account, movement) in movements {
        let currency = movement.currency();
        let bucket = folded.entry((*account, currency)).or_insert((
            0,
            Money::zero(currency),
            Money::zero(currency),
        ));
        bucket.0 += 1;
        if movement.amount().raw() >= 0 {
            bucket.1 = bucket.1.try_add(*movement)?;
        } else {
            // Subtracted from zero rather than negated: `-i64::MIN` is
            // unrepresentable, and `Money::try_sub` says so instead of
            // panicking (see its own note).
            bucket.2 = bucket.2.try_sub(*movement)?;
        }
    }
    folded
        .into_iter()
        .map(|((account, currency), (rows, debit, credit))| {
            Ok(BatchTotal {
                account,
                currency,
                rows,
                debit: debit.amount(),
                credit: credit.amount(),
                net: debit.try_sub(credit)?.amount(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The control section a source printed for itself (iaam-jc3y)
// ---------------------------------------------------------------------------

/// The control figures a source printed for one account and currency.
///
/// A statement prints its own arithmetic: what the account held at the start,
/// what it held at the end, and how much crossed it each way in between. The
/// journal has had the vocabulary for exactly these since §10.3
/// ([`ControlClaim`]), and has only ever been able to receive them **after** the
/// rows were written. This is that same section held before the rows are
/// written, so that the one moment a mismatch is cheap is also the moment
/// something knows what right looks like.
///
/// Every figure is optional and separately so, because a source prints what it
/// prints: a card statement gives two balances and no turnover, a broker report
/// gives turnover and no opening balance. An absent figure is not zero — that
/// distinction is §4.9's, and a zero written in for a figure nobody stated would
/// manufacture a mismatch out of silence.
///
/// [`ControlClaim`]: crate::reconciliation::claim::ControlClaim
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlSection {
    pub account: AccountId,
    pub currency: CurrencyCode,
    /// The interval the source printed the section for, inclusive at both ends.
    ///
    /// [`compare`] never reads it, and it is on the type anyway: the interval is
    /// part of what the source printed, and it is what the assertion written at
    /// commit is dated and scoped by. Kept on a second, parallel structure it
    /// would be a period that could come to belong to a different section from
    /// the one it was read beside.
    pub period: AssertionPeriod,
    /// What the account held before the first row of the period.
    pub opening: Option<PostedMinor>,
    /// What it held after the last one.
    pub closing: Option<PostedMinor>,
    /// Everything that arrived over the period, as a positive number.
    pub debit_turnover: Option<PostedMinor>,
    /// Everything that left, as a positive number.
    pub credit_turnover: Option<PostedMinor>,
}

impl ControlSection {
    /// Whether the source printed anything at all here.
    ///
    /// A section stating nothing is not a section: it compares against nothing
    /// and would publish a row of four absences. Callers use this to refuse one
    /// on the way in, where the mistake is still the caller's to fix.
    #[must_use]
    pub const fn states_nothing(&self) -> bool {
        self.opening.is_none()
            && self.closing.is_none()
            && self.debit_turnover.is_none()
            && self.credit_turnover.is_none()
    }
}

/// The figure of a control section one check is about.
///
/// The opening balance is deliberately not among them. A batch cannot produce an
/// opening balance — it holds movements, not a starting position — so there is
/// nothing to compare one with. It is not ignored either: it is the term the
/// closing check is built from, and a section that states it buys the check that
/// catches an import off by three orders of magnitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFigure {
    ClosingBalance,
    DebitTurnover,
    CreditTurnover,
}

impl ControlFigure {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ClosingBalance => "closing_balance",
            Self::DebitTurnover => "debit_turnover",
            Self::CreditTurnover => "credit_turnover",
        }
    }
}

/// Why a figure the source stated has no counterpart in the batch.
///
/// Deliberately **not** [`NotComparable`], which answers the same shape of
/// question about a different fold: that one says a journal holds no coverage
/// for an interval, and it is reached from a ledger built over recorded events.
/// This one says a batch cannot derive a figure from what it holds, whatever the
/// journal contains. Sharing a vocabulary between them would let a client read
/// «no journal coverage» off an import that has not touched the journal.
///
/// The two were put side by side again in `iaam-tx3c` and kept apart, on the
/// test of which side of the comparison is missing: every `NotComparable` reason
/// is a fact about the observed side, and this one is a fact about the claimed
/// side — the document did not print the term. [`NotComparable`] carries the
/// same argument from its end.
///
/// [`NotComparable`]: crate::reconciliation::check::NotComparable
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoCounterpart {
    /// A closing balance is checked as «the opening balance plus what the batch
    /// moves», and the source printed no opening balance. The journal's own
    /// balance is deliberately not substituted: the batch would then be checked
    /// against facts that are not in it, and this check exists to say whether
    /// the batch is a faithful reading of its document.
    OpeningBalanceNotStated,
}

impl NoCounterpart {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::OpeningBalanceNotStated => "opening_balance_not_stated",
        }
    }
}

/// One figure the source stated, checked against the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCheck {
    Matched {
        figure: ControlFigure,
        claimed: PostedMinor,
        /// Equal to `claimed`, and printed anyway: a check that reported only
        /// «matched» would not say what it had compared, and the whole value of
        /// this section is that both numbers are on the page.
        observed: PostedMinor,
    },
    Mismatched {
        figure: ControlFigure,
        claimed: PostedMinor,
        observed: PostedMinor,
        /// `claimed` minus `observed`: positive where the source sees more than
        /// the batch does, as [`Discrepancy`] signs it.
        ///
        /// [`Discrepancy`]: crate::reconciliation::check::Discrepancy
        delta: PostedMinor,
    },
    NotChecked {
        figure: ControlFigure,
        claimed: PostedMinor,
        reason: NoCounterpart,
    },
}

impl ControlCheck {
    #[must_use]
    pub const fn figure(&self) -> ControlFigure {
        match self {
            Self::Matched { figure, .. }
            | Self::Mismatched { figure, .. }
            | Self::NotChecked { figure, .. } => *figure,
        }
    }

    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Matched { .. } => "matched",
            Self::Mismatched { .. } => "mismatched",
            Self::NotChecked { .. } => "not_checked",
        }
    }

    /// Whether the two numbers disagree.
    ///
    /// A figure that could not be checked is **not** a mismatch, exactly as
    /// §10.4 keeps «nothing to compare against» apart from «the numbers do not
    /// match». Reporting the first as the second would refuse an import for the
    /// source's having printed less than another source prints.
    #[must_use]
    pub const fn is_mismatch(&self) -> bool {
        matches!(self, Self::Mismatched { .. })
    }
}

/// One account and currency: what the source said, what the batch says, and
/// where the two are put beside each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlComparison {
    pub account: AccountId,
    pub currency: CurrencyCode,
    /// The section the source printed, where it printed one. `None` means the
    /// batch moved money here and nothing was stated about it — which refuses
    /// nothing, and is worth publishing because it is the difference between «it
    /// agreed» and «it was never checked».
    pub stated: Option<ControlSection>,
    /// What the batch's rows come to.
    ///
    /// Zero over zero rows where the source stated figures for an account and
    /// currency no row touched. That is not an absence but the batch's answer —
    /// «nothing arrived here» — and it is the answer that catches a statement
    /// whose second page never reached the importer.
    pub observed: BatchTotal,
    /// One entry per figure the source stated. Empty where it stated none.
    pub checks: Vec<ControlCheck>,
}

/// Check every stated control section against the batch's totals.
///
/// One comparison per account and currency named by either side, in the order
/// [`total`] produces, so that the same session read twice compares the same.
///
/// **The journal is not consulted, and that is the design rather than a
/// simplification.** What this answers is «did the rows get read correctly»,
/// and the only evidence bearing on it is the document the rows and the control
/// section were both printed on. Folding the journal's prior balance in would
/// answer a different and later question — «does the journal agree with the
/// bank» — which reconciliation already answers, after the facts are written and
/// with a status that says how much the agreement is worth. It would also make
/// the very first import of an empty journal uncheckable, which is the import
/// most worth checking.
pub fn compare(
    sections: &[ControlSection],
    totals: &[BatchTotal],
) -> Result<Vec<ControlComparison>, MoneyError> {
    let mut keys: BTreeMap<(AccountId, CurrencyCode), ()> = BTreeMap::new();
    for total in totals {
        keys.insert((total.account, total.currency), ());
    }
    for section in sections {
        keys.insert((section.account, section.currency), ());
    }
    keys.into_keys()
        .map(|(account, currency)| {
            let stated = sections
                .iter()
                .find(|section| section.account == account && section.currency == currency)
                .copied();
            let observed = totals
                .iter()
                .find(|total| total.account == account && total.currency == currency)
                .copied()
                .unwrap_or(BatchTotal {
                    account,
                    currency,
                    rows: 0,
                    debit: PostedMinor::new(0),
                    credit: PostedMinor::new(0),
                    net: PostedMinor::new(0),
                });
            let checks = match stated {
                Some(section) => checks_for(&section, &observed)?,
                None => Vec::new(),
            };
            Ok(ControlComparison {
                account,
                currency,
                stated,
                observed,
                checks,
            })
        })
        .collect()
}

/// The checks one stated section buys against one total.
fn checks_for(
    section: &ControlSection,
    observed: &BatchTotal,
) -> Result<Vec<ControlCheck>, MoneyError> {
    let currency = section.currency;
    let mut checks = Vec::new();
    if let Some(claimed) = section.debit_turnover {
        checks.push(checked(
            ControlFigure::DebitTurnover,
            claimed,
            observed.debit,
            currency,
        )?);
    }
    if let Some(claimed) = section.credit_turnover {
        checks.push(checked(
            ControlFigure::CreditTurnover,
            claimed,
            observed.credit,
            currency,
        )?);
    }
    if let Some(claimed) = section.closing {
        match section.opening {
            Some(opening) => {
                let derived = Money::new(opening, currency)
                    .try_add(Money::new(observed.net, currency))?
                    .amount();
                checks.push(checked(
                    ControlFigure::ClosingBalance,
                    claimed,
                    derived,
                    currency,
                )?);
            }
            None => checks.push(ControlCheck::NotChecked {
                figure: ControlFigure::ClosingBalance,
                claimed,
                reason: NoCounterpart::OpeningBalanceNotStated,
            }),
        }
    }
    Ok(checks)
}

/// Two figures, compared. **No tolerance**, for [`check_claim`]'s reason: both
/// sides are posted amounts in minor units, and a one-kopeck difference is a
/// difference.
///
/// [`check_claim`]: crate::reconciliation::check::check_claim
fn checked(
    figure: ControlFigure,
    claimed: PostedMinor,
    observed: PostedMinor,
    currency: CurrencyCode,
) -> Result<ControlCheck, MoneyError> {
    if claimed == observed {
        return Ok(ControlCheck::Matched {
            figure,
            claimed,
            observed,
        });
    }
    let delta = Money::new(claimed, currency)
        .try_sub(Money::new(observed, currency))?
        .amount();
    Ok(ControlCheck::Mismatched {
        figure,
        claimed,
        observed,
        delta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn account(byte: u8) -> AccountId {
        AccountId(uuid::Uuid::from_bytes([byte; 16]))
    }

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn usd(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Usd)
    }

    #[test]
    fn an_empty_batch_totals_nothing() {
        // Not «zero on every account»: a batch that moved nothing on an account
        // has nothing to say about that account, and inventing a zero row would
        // publish a comparison nobody made.
        assert_eq!(total(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn the_two_sides_are_kept_apart_and_both_are_positive() {
        let main = account(1);
        let totals = total(&[(main, rub(3_000)), (main, rub(-1_200))]).unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].debit, PostedMinor::new(3_000));
        assert_eq!(
            totals[0].credit,
            PostedMinor::new(1_200),
            "an outflow is reported as an absolute value, as a turnover section prints it"
        );
        assert_eq!(totals[0].net, PostedMinor::new(1_800));
        assert_eq!(totals[0].rows, 2);
    }

    #[test]
    fn a_batch_that_takes_an_account_down_reports_a_negative_net() {
        // The one signed figure, and it must stay signed: an account that spent
        // more than it received over the period has a negative net, and a
        // closing balance below its opening one is the ordinary case.
        let main = account(1);
        let totals = total(&[(main, rub(100)), (main, rub(-900))]).unwrap();
        assert_eq!(totals[0].net, PostedMinor::new(-800));
    }

    #[test]
    fn two_currencies_on_one_account_are_two_totals() {
        // Not a `CurrencyMismatch`: the pair is the key precisely so that a
        // multi-currency account totals rather than refuses.
        let main = account(1);
        let totals = total(&[(main, rub(500)), (main, usd(700))]).unwrap();
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0].account, main);
        assert_eq!(totals[1].account, main);
        assert_ne!(totals[0].currency, totals[1].currency);
    }

    #[test]
    fn two_accounts_are_two_totals() {
        let main = account(1);
        let savings = account(2);
        let totals = total(&[(main, rub(500)), (savings, rub(500))]).unwrap();
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0].account, main);
        assert_eq!(totals[1].account, savings);
    }

    #[test]
    fn the_order_does_not_depend_on_the_order_the_rows_arrived_in() {
        // The assessment carrying these totals is fingerprinted, and a commit is
        // refused when the fingerprint moved. A fold whose output order
        // depended on input order would refuse commits that changed nothing.
        let main = account(1);
        let savings = account(2);
        let forwards = total(&[(main, rub(1)), (savings, usd(2)), (savings, rub(3))]).unwrap();
        let backwards = total(&[(savings, rub(3)), (savings, usd(2)), (main, rub(1))]).unwrap();
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn a_zero_movement_is_folded_rather_than_dropped() {
        // The caller decides what a movement is. A zero here counts as a row and
        // adds nothing to either side, which is what lets a caller that means
        // «this row moved no cash» drop it deliberately rather than discover it
        // was dropped for it.
        let main = account(1);
        let totals = total(&[(main, rub(0))]).unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].rows, 1);
        assert_eq!(totals[0].debit, PostedMinor::new(0));
        assert_eq!(totals[0].credit, PostedMinor::new(0));
    }

    #[test]
    fn a_sum_that_does_not_fit_is_refused_rather_than_wrapped() {
        let main = account(1);
        let overflow = total(&[(main, rub(i64::MAX)), (main, rub(1))]);
        assert_eq!(overflow, Err(MoneyError::Overflow));
    }

    // --- the source's own control section ---------------------------------

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    /// A section stating nothing, to be filled in by the test that needs it.
    fn section(account: AccountId) -> ControlSection {
        ControlSection {
            account,
            currency: CurrencyCode::Rub,
            period: march(),
            opening: None,
            closing: None,
            debit_turnover: None,
            credit_turnover: None,
        }
    }

    fn minor(value: i64) -> PostedMinor {
        PostedMinor::new(value)
    }

    #[test]
    fn a_section_stating_nothing_is_recognised_as_stating_nothing() {
        let main = account(1);
        assert!(section(main).states_nothing());
        assert!(
            !ControlSection {
                closing: Some(minor(0)),
                ..section(main)
            }
            .states_nothing(),
            "a stated zero is a figure, and only an absence is silence (§4.9)"
        );
    }

    #[test]
    fn a_turnover_the_rows_exceed_is_a_mismatch_that_says_by_how_much() {
        // The mirrored-transfer failure in miniature: the source printed what
        // arrived, and the rows say more arrived than that.
        let main = account(1);
        let stated = ControlSection {
            debit_turnover: Some(minor(1_000)),
            credit_turnover: Some(minor(500)),
            ..section(main)
        };
        let totals = total(&[(main, rub(1_000)), (main, rub(300)), (main, rub(-500))]).unwrap();
        let compared = compare(&[stated], &totals).unwrap();
        assert_eq!(compared.len(), 1);
        assert_eq!(
            compared[0].checks[0],
            ControlCheck::Mismatched {
                figure: ControlFigure::DebitTurnover,
                claimed: minor(1_000),
                observed: minor(1_300),
                delta: minor(-300),
            }
        );
        assert_eq!(
            compared[0].checks[1],
            ControlCheck::Matched {
                figure: ControlFigure::CreditTurnover,
                claimed: minor(500),
                observed: minor(500),
            },
            "the side the mistake did not touch still agrees, and says with what"
        );
    }

    #[test]
    fn a_closing_balance_is_the_opening_one_plus_what_the_batch_moves() {
        // The wrong-units failure in miniature: every row a hundred times too
        // large, every row well formed, and the only thing that knew otherwise
        // was the figure at the bottom of the statement.
        let main = account(1);
        let stated = ControlSection {
            opening: Some(minor(0)),
            closing: Some(minor(150_000)),
            ..section(main)
        };
        let totals = total(&[(main, rub(15_000_000))]).unwrap();
        let compared = compare(&[stated], &totals).unwrap();
        assert_eq!(
            compared[0].checks[0],
            ControlCheck::Mismatched {
                figure: ControlFigure::ClosingBalance,
                claimed: minor(150_000),
                observed: minor(15_000_000),
                delta: minor(-14_850_000),
            }
        );
    }

    #[test]
    fn a_closing_balance_without_an_opening_one_is_not_checked_and_is_not_a_mismatch() {
        // «Nothing to compare against» is not «the numbers do not match» (§10.4).
        // The journal's own balance is deliberately not substituted: the batch
        // would then be checked against facts that are not in it.
        let main = account(1);
        let stated = ControlSection {
            closing: Some(minor(500)),
            ..section(main)
        };
        let compared = compare(&[stated], &total(&[(main, rub(100))]).unwrap()).unwrap();
        assert_eq!(
            compared[0].checks[0],
            ControlCheck::NotChecked {
                figure: ControlFigure::ClosingBalance,
                claimed: minor(500),
                reason: NoCounterpart::OpeningBalanceNotStated,
            }
        );
        assert!(!compared[0].checks[0].is_mismatch());
    }

    #[test]
    fn a_section_no_row_touched_is_compared_against_zero_rather_than_skipped() {
        // The statement's second page never reached the importer. The batch's
        // answer is «nothing arrived», which is an answer and not an absence.
        let main = account(1);
        let stated = ControlSection {
            debit_turnover: Some(minor(700)),
            ..section(main)
        };
        let compared = compare(&[stated], &[]).unwrap();
        assert_eq!(compared.len(), 1);
        assert_eq!(compared[0].observed.rows, 0);
        assert_eq!(
            compared[0].checks[0],
            ControlCheck::Mismatched {
                figure: ControlFigure::DebitTurnover,
                claimed: minor(700),
                observed: minor(0),
                delta: minor(700),
            }
        );
    }

    #[test]
    fn rows_the_source_stated_nothing_about_are_published_and_check_nothing() {
        // «It agreed» and «it was never checked» are different answers, and an
        // import that could not be checked must not read like one that passed.
        let main = account(1);
        let compared = compare(&[], &total(&[(main, rub(100))]).unwrap()).unwrap();
        assert_eq!(compared.len(), 1);
        assert!(compared[0].stated.is_none());
        assert!(compared[0].checks.is_empty());
        assert_eq!(compared[0].observed.debit, minor(100));
    }

    #[test]
    fn one_account_in_two_currencies_is_two_comparisons() {
        let main = account(1);
        let roubles = ControlSection {
            debit_turnover: Some(minor(100)),
            ..section(main)
        };
        let dollars = ControlSection {
            currency: CurrencyCode::Usd,
            debit_turnover: Some(minor(100)),
            ..section(main)
        };
        let totals = total(&[(main, rub(100)), (main, usd(200))]).unwrap();
        let compared = compare(&[roubles, dollars], &totals).unwrap();
        assert_eq!(compared.len(), 2);
        assert!(
            compared[0].checks[0]
                == ControlCheck::Matched {
                    figure: ControlFigure::DebitTurnover,
                    claimed: minor(100),
                    observed: minor(100),
                }
        );
        assert!(
            compared[1].checks[0].is_mismatch(),
            "a currency that agrees does not vouch for the one beside it"
        );
    }

    #[test]
    fn a_single_kopeck_is_a_difference() {
        // No tolerance, for `check_claim`'s reason: both sides are posted
        // amounts in minor units.
        let main = account(1);
        let stated = ControlSection {
            debit_turnover: Some(minor(1_001)),
            ..section(main)
        };
        let compared = compare(&[stated], &total(&[(main, rub(1_000))]).unwrap()).unwrap();
        assert!(compared[0].checks[0].is_mismatch());
    }
}
