//! Report control sections (§10.3).
//!
//! The report contains more than just transactions: opening and closing balances,
//! turnover, security quantities, commission, coupon, and dividend amounts,
//! and tax withheld. These are source facts, and they become
//! [`ControlClaim`]s—claims against which the journal-based calculation is later reconciled.
//! calculated from the journal.
//!
//! **A section absent from the document does not exist.** Zero here is
//! the source's assertion that there were no commissions, while the absence
//! of a section asserts nothing (§4.9). Therefore, every field
//! is optional, and the collected list of claims is shorter when
//! those sections are absent from the report.

use iaam_core::ids::{CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};

/// Cash balance reported by the section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CashSection {
    pub currency: CurrencyCode,
    pub amount: PostedMinor,
    pub at: BalancePoint,
}

/// Turnover for the interval. Both sides are magnitudes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnoverSection {
    pub currency: CurrencyCode,
    pub debit: PostedMinor,
    pub credit: PostedMinor,
}

/// Total for the interval: commissions, income, or tax withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TotalSection {
    pub currency: CurrencyCode,
    pub amount: PostedMinor,
}

/// Security quantity reported by the section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionSection {
    pub instrument: InstrumentId,
    pub custody: CustodyId,
    pub quantity: Quantity,
    pub at: BalancePoint,
}

/// Control sections found by the parser in a single report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlSections {
    pub cash_balances: Vec<CashSection>,
    pub turnovers: Vec<TurnoverSection>,
    pub fees: Option<TotalSection>,
    pub income: Option<TotalSection>,
    pub tax_withheld: Option<TotalSection>,
    pub positions: Vec<PositionSection>,
}

impl ControlSections {
    /// Source assertions from the sections found.
    ///
    /// The order is stable: balances, turnover, quantities, then totals.
    /// An order dependent on where the section
    /// appeared in the document would make comparing two parses of the same file
    /// impossible.
    #[must_use]
    pub fn claims(&self) -> Vec<ControlClaim> {
        let mut claims = Vec::new();
        for balance in &self.cash_balances {
            claims.push(ControlClaim::CashBalance {
                currency: balance.currency,
                amount: balance.amount,
                at: balance.at,
            });
        }
        for turnover in &self.turnovers {
            claims.push(ControlClaim::CashTurnover {
                currency: turnover.currency,
                debit: turnover.debit,
                credit: turnover.credit,
            });
        }
        for position in &self.positions {
            claims.push(ControlClaim::PositionQuantity {
                instrument: position.instrument,
                custody: position.custody,
                quantity: position.quantity,
                at: position.at,
            });
        }
        if let Some(fees) = self.fees {
            claims.push(ControlClaim::FeesTotal {
                currency: fees.currency,
                amount: fees.amount,
            });
        }
        if let Some(income) = self.income {
            claims.push(ControlClaim::IncomeTotal {
                currency: income.currency,
                amount: income.amount,
            });
        }
        if let Some(tax) = self.tax_withheld {
            claims.push(ControlClaim::TaxWithheldTotal {
                currency: tax.currency,
                amount: tax.amount,
            });
        }
        claims
    }
}
