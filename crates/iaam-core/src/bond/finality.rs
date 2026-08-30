//! Finality of principal returns (§6 of spec E3.4.4, bead iaam-d8b.4.3).
//!
//! One rule: a return is final when the accumulated share reaches 100%.
//! Source codes are not read — six of the fifty reviewed securities have no
//! maturity-code row at all.
//!
//! This is not recorded as an observation: it is a projection property
//! (ADR-0002). The completeness invariant in
//! `iaam_market::schedule::completeness` calculates the same total, but
//! belongs to the SOURCE PROFILE and answers a different question — whether
//! the export is intact.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::bond::PrincipalReturn;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Whether a principal return is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalReturnFinality {
    /// The accumulated share reached 100%: all principal was returned.
    Final,
    /// Part of the principal after which a remainder remains outstanding.
    Partial,
    /// Shares do not reach 100%: no row can be called final.
    Unknown,
}

/// Mark a sequence of principal returns with finality.
pub fn finality_of(
    returns: &[PrincipalReturn],
) -> Result<Vec<(PrincipalReturn, PrincipalReturnFinality)>, NumericError> {
    let shares = returns.iter().map(|r| r.share_percent).collect::<Vec<_>>();
    let total = Dec::sum(&shares)?;
    let hundred = Dec::new(Decimal::ONE_HUNDRED);
    if total != hundred {
        return Ok(returns
            .iter()
            .map(|r| (*r, PrincipalReturnFinality::Unknown))
            .collect());
    }

    // Source row order is not guaranteed, while accumulation depends on it
    // completely: without sorting, a random row would be marked final.
    let mut ordered = returns.to_vec();
    ordered.sort_by_key(|r| r.repayment_date);

    let mut accumulated = Dec::zero();
    let mut marked = Vec::with_capacity(ordered.len());
    for item in ordered {
        accumulated = accumulated.checked_add(item.share_percent)?;
        let finality = if accumulated == hundred {
            PrincipalReturnFinality::Final
        } else {
            PrincipalReturnFinality::Partial
        };
        marked.push((item, finality));
    }
    Ok(marked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::PrincipalReturn;
    use rust_decimal::Decimal;
    use time::{Date, macros::date};

    fn ret(day: Date, share: &str) -> PrincipalReturn {
        PrincipalReturn {
            repayment_date: day,
            share_percent: Dec::new(Decimal::from_str_exact(share).unwrap()),
        }
    }

    #[test]
    fn six_amortisations_without_a_maturity_code_still_end_finally() {
        // For six of fifty reviewed securities, the last return arrives as an
        // ordinary amortisation row without a maturity code.
        // Reading the source code would lose their finality.
        let returns = vec![
            ret(date!(2027 - 01 - 15), "10"),
            ret(date!(2028 - 01 - 15), "10"),
            ret(date!(2029 - 01 - 15), "10"),
            ret(date!(2030 - 01 - 15), "20"),
            ret(date!(2031 - 01 - 15), "20"),
            ret(date!(2032 - 01 - 15), "30"),
        ];
        let marked = finality_of(&returns).unwrap();
        assert_eq!(marked[5].1, PrincipalReturnFinality::Final);
        assert_eq!(marked[4].1, PrincipalReturnFinality::Partial);
    }

    #[test]
    fn shares_short_of_a_hundred_make_nobody_final() {
        // A truncated page gives a plausible but incomplete sequence.
        // Marking the last row final would close the security ten years early.
        let returns = vec![
            ret(date!(2027 - 01 - 15), "40"),
            ret(date!(2028 - 01 - 15), "35"),
        ];
        let marked = finality_of(&returns).unwrap();
        assert!(
            marked
                .iter()
                .all(|(_, finality)| *finality == PrincipalReturnFinality::Unknown)
        );
    }

    #[test]
    fn returns_are_walked_in_date_order_not_in_source_order() {
        // Source row order is not guaranteed, while share accumulation depends
        // on it completely.
        let returns = vec![
            ret(date!(2028 - 01 - 15), "60"),
            ret(date!(2027 - 01 - 15), "40"),
        ];
        let marked = finality_of(&returns).unwrap();
        let final_one = marked
            .iter()
            .find(|(_, finality)| *finality == PrincipalReturnFinality::Final)
            .unwrap();
        assert_eq!(final_one.0.repayment_date, date!(2028 - 01 - 15));
    }
}
