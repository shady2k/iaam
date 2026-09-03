//! What would have to be true for a report's figures to be complete, and which
//! of those things are not.
//!
//! Every fact this module publishes is already published somewhere in the
//! report it summarises. `population.completeness`, `accounts[].cash[].opening`
//! and the rest are each correct and each in the right place; the difficulty
//! reported from a run through the whole flow was that a reader had to
//! reconstruct "how much of this answer do I believe" from four fields in three
//! shapes, and a reader who stopped at the numbers got a confident wrong
//! impression. So this is a **register**, published first, of the specific
//! things the report is silent or partial about.
//!
//! **There is no score.** No number, no percentage, no grade. A confidence
//! figure is an opinion the owner cannot check, which is exactly what
//! `PairingEvidenceDto` refused to publish and for the same reason: what is
//! published here is a list of caveats, each naming one thing and the field
//! that shows it.
//!
//! **This is never a second source of truth.** A [`Caveat`] carries a kind, the
//! subject it is about, and a pointer to the field of the same response that
//! states the fact in full. Its prose is a constant of its kind, not a sentence
//! assembled from figures: a summary that restated an amount could restate it
//! wrongly, and then the report would contradict itself.
//!
//! **It is computed here, beside the numbers.** A register assembled by the
//! transport can disagree with the report it summarises — the same failure the
//! architecture guard caught when a cash fold was written outside the core. The
//! derivations live in [`super::balances`], [`super::population`], and in the
//! two free functions below, all folding the values the report itself
//! publishes.

use crate::ids::{AccountId, InstrumentId};
use crate::money::CurrencyCode;
use crate::projection::money_flow::{MoneyFlow, MoneyFlowError};
use crate::returns::ReturnsReport;

use super::population::ReportPopulation;

/// What a report is for.
///
/// The four names are shared vocabulary: the outstanding-work queue grades its
/// items by the goal they are required for, and a report says which goal it
/// answers. The two join on this name, so a caller holding a report with
/// caveats can ask the queue what closes them.
///
/// Fixed at four. A fifth would have to mean a report nobody has written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReportGoal {
    /// What the owner holds, at a date: cash and positions by account.
    AssetSnapshot,
    /// Where money came from and went, over an interval.
    MoneyFlow,
    /// What the money earned.
    Returns,
    /// Whether the journal agrees with what the sources say.
    Reconciliation,
}

impl ReportGoal {
    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AssetSnapshot => "asset_snapshot",
            Self::MoneyFlow => "money_flow",
            Self::Returns => "returns",
            Self::Reconciliation => "reconciliation",
        }
    }
}

/// One thing a report's figures do not account for.
///
/// A closed set, and deliberately so: every kind is derived from a computation
/// the report already performs. Nothing here runs a fold of its own — a caveat
/// that computed its own evidence would be the second source of truth this
/// register exists not to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CaveatKind {
    /// One of the owner's accounts is outside this report and in no scope at
    /// all. Nobody has ruled on whether it belongs.
    AccountInNoScope,
    /// One of the owner's accounts is outside this report because he placed it
    /// in a scope of his own. The omission is a decision, and it is still an
    /// omission.
    AccountInAnotherScope,
    /// A cash figure accumulated from a start nothing asserts, so it is a
    /// running sum and not a balance.
    RunningCashSum,
    /// §11 refuses one account's period reports.
    PeriodReportsRefused,
    /// Cash movements the six flow quantities do not decompose.
    UndecomposedMovements,
    /// An account whose own cash change the six flow quantities do not explain.
    UnexplainedCashChange,
    /// A position no price covers, so it is absent from the portfolio value.
    UnpricedPosition,
    /// The portfolio value at the report date could not be computed.
    TerminalValueNotComputed,
    /// The rate of return could not be computed.
    ReturnNotComputed,
}

impl CaveatKind {
    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AccountInNoScope => "account_in_no_scope",
            Self::AccountInAnotherScope => "account_in_another_scope",
            Self::RunningCashSum => "running_cash_sum",
            Self::PeriodReportsRefused => "period_reports_refused",
            Self::UndecomposedMovements => "undecomposed_movements",
            Self::UnexplainedCashChange => "unexplained_cash_change",
            Self::UnpricedPosition => "unpriced_position",
            Self::TerminalValueNotComputed => "terminal_value_not_computed",
            Self::ReturnNotComputed => "return_not_computed",
        }
    }

    /// The field of the same response that states this fact in full.
    ///
    /// A path through the published answer, in the notation the report's own
    /// documentation already uses for one: `[]` stands for every element of an
    /// array, and the caveat's subject says which element. It is a property of
    /// the kind rather than a string a caller passes, so a caveat cannot be
    /// built pointing somewhere the fact is not.
    #[must_use]
    pub const fn see(self) -> &'static str {
        match self {
            Self::AccountInNoScope | Self::AccountInAnotherScope => "population.outside[]",
            Self::RunningCashSum => "accounts[].cash[].opening",
            Self::PeriodReportsRefused => "accounts[].period_reports",
            Self::UndecomposedMovements => "currencies[].not_decomposed.by_account[]",
            Self::UnexplainedCashChange => "unexplained[]",
            Self::UnpricedPosition => "data_quality.position_coverage.uncovered[]",
            Self::TerminalValueNotComputed => "terminal_value",
            Self::ReturnNotComputed => "xirr_pre_tax",
        }
    }

    /// What this kind means, in one sentence.
    ///
    /// A constant of the kind, with nothing interpolated into it. The register
    /// summarises; the amounts, dates and names live at [`Self::see`], and a
    /// sentence here that restated one could restate it wrongly.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::AccountInNoScope => {
                "This account is outside the report and in no scope at all: nobody has ruled on whether its money belongs in these figures, so its absence is an open question and not a decision."
            }
            Self::AccountInAnotherScope => {
                "This account is outside the report because it sits in another scope the owner drew. The figures are partial by decision, and still partial."
            }
            Self::RunningCashSum => {
                "Nothing asserts what this account held in this currency before its first recorded movement, so the figure is the movement since an unknown start and is not a balance."
            }
            Self::PeriodReportsRefused => {
                "The perimeter refuses this account's period reports (§11). Its observed cash and positions are stated, and the economics of the period behind them are not reconstructed."
            }
            Self::UndecomposedMovements => {
                "Cash moved on this account in this currency that the six quantities do not decompose, so the split by category does not account for all of it."
            }
            Self::UnexplainedCashChange => {
                "This account's own cash change over the interval is not explained by the six quantities: the interval's figures and the account's movement disagree."
            }
            Self::UnpricedPosition => {
                "No price covers this position at the report date, so it is absent from the portfolio value rather than valued at zero."
            }
            Self::TerminalValueNotComputed => {
                "The portfolio value at the report date could not be computed, so every figure derived from it is absent rather than approximate."
            }
            Self::ReturnNotComputed => {
                "The rate of return could not be computed. No figure was substituted for it."
            }
        }
    }
}

/// What one caveat is about.
///
/// Identifiers, not names: the field at [`CaveatKind::see`] carries the title,
/// the amount and the dates, and repeating them here would make the register a
/// second copy of what it points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CaveatSubject {
    /// The answer as a whole. Used where the fact is not about one account or
    /// one instrument — a figure the report declined to compute.
    Report,
    Account(AccountId),
    /// A cash figure is per account **and** currency, and so is an opening
    /// assertion; a caveat about one that named only the account would send the
    /// reader to the wrong row.
    AccountCurrency {
        account: AccountId,
        currency: CurrencyCode,
    },
    Instrument(InstrumentId),
}

/// One specific, checkable thing the report's figures do not account for.
///
/// Two fields, because that is all a caveat is: which kind of gap, and what it
/// is about. The sentence and the pointer are constants of the kind, so no
/// caller can publish a caveat whose prose says one thing and whose pointer
/// leads to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Caveat {
    kind: CaveatKind,
    subject: CaveatSubject,
}

impl Caveat {
    #[must_use]
    pub const fn new(kind: CaveatKind, subject: CaveatSubject) -> Self {
        Self { kind, subject }
    }

    #[must_use]
    pub const fn kind(&self) -> CaveatKind {
        self.kind
    }

    #[must_use]
    pub const fn subject(&self) -> CaveatSubject {
        self.subject
    }

    /// What this caveat means. See [`CaveatKind::detail`].
    #[must_use]
    pub const fn detail(&self) -> &'static str {
        self.kind.detail()
    }

    /// Where in the same response the fact is stated in full. See
    /// [`CaveatKind::see`].
    #[must_use]
    pub const fn see(&self) -> &'static str {
        self.kind.see()
    }
}

/// A report's own statement about how much of it is complete.
///
/// `complete` is **not a field**. It is `caveats.is_empty()`, and there is no
/// constructor that sets one without the other: a report that could be built
/// asserting completeness beside a register of its gaps is the failure this
/// whole block exists to prevent — a confident number over an incomplete
/// population.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportConfidence {
    goal: ReportGoal,
    caveats: Vec<Caveat>,
}

impl ReportConfidence {
    #[must_use]
    pub fn new(goal: ReportGoal, caveats: Vec<Caveat>) -> Self {
        Self { goal, caveats }
    }

    /// Which of the four goals this report answers.
    #[must_use]
    pub const fn goal(&self) -> ReportGoal {
        self.goal
    }

    /// Whether everything that would have to be true for the figures to be
    /// complete is true.
    ///
    /// Derived, never stored.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.caveats.is_empty()
    }

    /// The specific things that are not. Empty exactly when [`Self::complete`].
    #[must_use]
    pub fn caveats(&self) -> &[Caveat] {
        &self.caveats
    }
}

/// The flow report's register.
///
/// The population's caveats first: an account left out is a larger silence than
/// a row left undecomposed, and it is the one the figures themselves cannot
/// show. Then the two facts the fold already computed.
///
/// `not_decomposed_by_account` is read rather than
/// `not_decomposed_by_account_and_cause`: the published answer carries the
/// per-account totals and not the cause, and a caveat drawing a distinction the
/// reader cannot check at the field it points to would be an opinion again.
pub fn money_flow_confidence(
    population: &ReportPopulation,
    flow: &MoneyFlow,
) -> Result<ReportConfidence, MoneyFlowError> {
    let mut caveats = population.caveats();
    for currency in flow.currencies().collect::<Vec<_>>() {
        for (account, count, _amount) in flow.not_decomposed_by_account(currency)? {
            if count == 0 {
                continue;
            }
            caveats.push(Caveat::new(
                CaveatKind::UndecomposedMovements,
                CaveatSubject::AccountCurrency { account, currency },
            ));
        }
    }
    // Already filtered to the accounts that do not close: an account whose
    // residual is zero is not listed, and re-deriving that test here would give
    // one question two answers.
    for (account, money) in flow.residuals_by_account()? {
        caveats.push(Caveat::new(
            CaveatKind::UnexplainedCashChange,
            CaveatSubject::AccountCurrency {
                account,
                currency: money.currency(),
            },
        ));
    }
    Ok(ReportConfidence::new(ReportGoal::MoneyFlow, caveats))
}

/// The returns report's register.
///
/// The three report-specific kinds are read off `data_quality` and the two
/// `Computed` figures, which the report already carries. A position no price
/// covers is not valued at zero, and both figures are absent rather than
/// approximate when they refuse — which is correct, and invisible to a reader
/// who takes the presence of the block as the presence of an answer.
#[must_use]
pub fn returns_confidence(
    population: &ReportPopulation,
    report: &ReturnsReport,
) -> ReportConfidence {
    let mut caveats = population.caveats();
    for uncovered in &report.data_quality.position_coverage.uncovered {
        caveats.push(Caveat::new(
            CaveatKind::UnpricedPosition,
            CaveatSubject::Instrument(uncovered.instrument),
        ));
    }
    if report.terminal_value.value().is_none() {
        caveats.push(Caveat::new(
            CaveatKind::TerminalValueNotComputed,
            CaveatSubject::Report,
        ));
    }
    if report.xirr.value().is_none() {
        caveats.push(Caveat::new(
            CaveatKind::ReturnNotComputed,
            CaveatSubject::Report,
        ));
    }
    ReportConfidence::new(ReportGoal::Returns, caveats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completeness_is_the_register_being_empty() {
        let complete = ReportConfidence::new(ReportGoal::AssetSnapshot, Vec::new());
        assert!(complete.complete());
        assert!(complete.caveats().is_empty());

        let partial = ReportConfidence::new(
            ReportGoal::AssetSnapshot,
            vec![Caveat::new(
                CaveatKind::RunningCashSum,
                CaveatSubject::Report,
            )],
        );
        assert!(!partial.complete());
    }

    /// The four names are shared with the outstanding-work queue. A rename here
    /// silently breaks the join.
    #[test]
    fn the_four_goals_keep_their_agreed_names() {
        assert_eq!(ReportGoal::AssetSnapshot.code(), "asset_snapshot");
        assert_eq!(ReportGoal::MoneyFlow.code(), "money_flow");
        assert_eq!(ReportGoal::Returns.code(), "returns");
        assert_eq!(ReportGoal::Reconciliation.code(), "reconciliation");
    }

    /// Every kind must name a field, and no two kinds may share a name.
    #[test]
    fn every_kind_carries_a_pointer_and_a_sentence() {
        const KINDS: [CaveatKind; 9] = [
            CaveatKind::AccountInNoScope,
            CaveatKind::AccountInAnotherScope,
            CaveatKind::RunningCashSum,
            CaveatKind::PeriodReportsRefused,
            CaveatKind::UndecomposedMovements,
            CaveatKind::UnexplainedCashChange,
            CaveatKind::UnpricedPosition,
            CaveatKind::TerminalValueNotComputed,
            CaveatKind::ReturnNotComputed,
        ];
        let mut codes: Vec<&str> = KINDS.iter().map(|kind| kind.code()).collect();
        codes.sort_unstable();
        let unique = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), unique, "two caveat kinds share a code");
        for kind in KINDS {
            assert!(!kind.see().is_empty(), "{} names no field", kind.code());
            assert!(!kind.detail().is_empty(), "{} says nothing", kind.code());
        }
    }
}
