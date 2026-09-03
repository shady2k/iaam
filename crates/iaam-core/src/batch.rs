//! A batch of cash movements, totalled before any of it is a journal.
//!
//! An import publishes every row it would write, each with a signed amount and a
//! currency, and no sum of any of them. An operator checking a two-hundred-row
//! import against the one figure printed on his statement therefore has to add
//! two hundred decimal strings — and the client of this system is a language
//! model, in a system that deliberately keeps money arithmetic here.
//!
//! The fold is in the core because summing money is arithmetic, and
//! `scripts/check-architecture.sh` (§3.1, §13) refuses monetary arithmetic
//! outside it.
//!
//! **This is not [`crate::reconciliation`], and the difference is not
//! bookkeeping.** Reconciliation folds the journal — facts that were written,
//! corrections resolved, a period bounded by assertions — and asks whether the
//! journal agrees with what a source said afterwards. This folds a batch that is
//! not in the journal and may never be. Nothing here decides or refuses
//! anything: it lets a reader compare one number with one number.

use std::collections::BTreeMap;

use crate::ids::AccountId;
use crate::money::{CurrencyCode, Money, MoneyError, PostedMinor};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
