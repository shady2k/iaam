//! Контрольные секции отчёта (§10.3).
//!
//! Отчёт содержит не только операции: остатки на начало и конец,
//! обороты, количества бумаг, суммы комиссий, купонов и дивидендов,
//! удержанный налог. Это факты источника, и они становятся
//! [`ControlClaim`] — утверждениями, с которыми потом сходится
//! посчитанное по журналу.
//!
//! **Секции, которой в документе нет, не существует.** Ноль здесь —
//! утверждение источника о том, что комиссий не было, а отсутствие
//! секции не утверждает ничего (§4.9). Поэтому каждое поле
//! необязательно, а собранный список утверждений короче на те секции,
//! которых в отчёте не оказалось.

use iaam_core::ids::{CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};

/// Остаток денег, заявленный секцией.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CashSection {
    pub currency: CurrencyCode,
    pub amount: PostedMinor,
    pub at: BalancePoint,
}

/// Обороты за интервал. Обе стороны — модули.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnoverSection {
    pub currency: CurrencyCode,
    pub debit: PostedMinor,
    pub credit: PostedMinor,
}

/// Итог за интервал: комиссии, доходы или удержанный налог.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TotalSection {
    pub currency: CurrencyCode,
    pub amount: PostedMinor,
}

/// Количество бумаг, заявленное секцией.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionSection {
    pub instrument: InstrumentId,
    pub custody: CustodyId,
    pub quantity: Quantity,
    pub at: BalancePoint,
}

/// Контрольные секции, найденные парсером в одном отчёте.
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
    /// Утверждения источника из найденных секций.
    ///
    /// Порядок устойчив: остатки, обороты, количества, затем итоги.
    /// Порядок, зависящий от того, в каком месте документа секция
    /// встретилась, сделал бы сравнение двух разборов одного файла
    /// невозможным.
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
