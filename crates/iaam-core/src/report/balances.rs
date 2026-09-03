//! The balances answer: cash, reconciliation, and positions by account.
//!
//! The rows are assembled by the application, which owns the store round-trips;
//! the shapes and everything derived from them live here, because
//! [`BalancesReport::confidence`] is derived from these fields and must not be
//! able to disagree with them.

use crate::ids::AccountId;
use crate::money::{Money, Quantity};
use crate::perimeter::NegativeCashSpan;
use crate::projection::balances::PositionKey;
use crate::reconciliation::ReconciliationStatus;

use super::confidence::{Caveat, CaveatKind, CaveatSubject, ReportConfidence, ReportGoal};
use super::population::ReportPopulation;

/// Whether anything asserts the state a cash figure was accumulated from.
///
/// The projection sums cash legs from zero. Zero is a starting point only when
/// something says the account held nothing before its first event; otherwise the
/// figure is the movement over the imported interval, not a balance.
///
/// This is carried by **every** cash figure, not only by figures that look
/// wrong. In the reported case one account showed an impossible negative and
/// the rest showed plausible positives — and from an unasserted start the
/// plausible ones were exactly as unfounded as the impossible one. They were
/// merely the ones that passed a plausibility check the reader happened to
/// have. A marker that appeared only on anomalies would confirm that mistake.
///
/// The distinction is per account-and-currency, because that is what an opening
/// assertion is about and what a cash figure is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CashOpening {
    /// An opening assertion covers the state before this account's first cash
    /// movement in this currency.
    Asserted,
    /// Nothing does: the figure accumulated from an unasserted start.
    Unasserted,
}

impl CashOpening {
    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Asserted => "asserted",
            Self::Unasserted => "unasserted",
        }
    }
}

/// One cash figure together with what is known about where it started.
///
/// The two travel as one value rather than in parallel collections: a caller
/// that carried them separately could render the amount and drop the marker,
/// which is the defect this type exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountCash {
    pub money: Money,
    pub opening: CashOpening,
}

/// Whether §11 lets the period's tax and financial reports be calculated for
/// one account.
///
/// The refusal is **per account**: §11 requires the remainder of the scope to
/// go on being calculated, so this is a field on a row rather than a state of
/// the answer. It is also not a refusal of the row: the account's observed cash
/// and positions are stated either way, because the perimeter retains an
/// observable cash effect and declines only to reconstruct financing economics
/// it does not support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeriodReports {
    /// Nothing in the assessment stops the period's reports for this account.
    Calculated,
    /// §11 refuses them, for the reasons carried here. Never empty: it is
    /// constructed only when the assessment says the account is blocked, and a
    /// refusal without its reason is the shape the owner cannot act on.
    Refused(Vec<NegativeCashSpan>),
}

impl PeriodReports {
    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Calculated => "calculated",
            Self::Refused(_) => "refused",
        }
    }

    /// The spans that refuse the period's reports; empty when none does.
    #[must_use]
    pub fn refusals(&self) -> &[NegativeCashSpan] {
        match self {
            Self::Calculated => &[],
            Self::Refused(spans) => spans,
        }
    }
}

/// Cash, reconciliation, and positions for one contour account.
#[derive(Debug, Clone)]
pub struct AccountBalanceRow {
    pub account: AccountId,
    pub cash: Vec<AccountCash>,
    pub reconciliation: Vec<ReconciliationStatus>,
    pub positions: Vec<(PositionKey, Quantity)>,
    /// What §11 says about this account's period reports (§11).
    pub period_reports: PeriodReports,
}

/// The balances answer: one row per contour account, plus what is true of the
/// answer rather than of a row.
///
/// `negative_cash` is on the answer because it is a fact about the set — which
/// accounts carry a liability — and a reader looking for it should not have to
/// scan every row's every currency to find out there is none. It is a warning,
/// not a prohibition: a technical overdraft on an ordinary account is real, and
/// a margin balance is a liability that belongs in NAV (§11). Nothing here
/// refuses, suppresses, or calls it an error; the answer states it and the
/// reader judges it, which is the stance `Balances::negative_cash` already
/// takes.
///
/// **How this composes with the perimeter.** `perimeter::NegativeCashSpan`
/// (`iaam-core/src/perimeter.rs`) is the richer statement of the same fact:
/// account, currency, `from`, `resolved`, and a `NegativeCashClassification`.
/// The key is the span's key, and at one `as_of` at most one span per account
/// and currency is still open, so each entry here **is** that open span — the
/// projection supplies the amount, which no span carries, and the span supplies
/// the dates and the classification. That is why wiring `assess` in was
/// additive: it added fields to these entries rather than introducing a second,
/// differently-keyed notion of negative cash to reconcile with them.
#[derive(Debug, Clone)]
pub struct BalancesReport {
    pub accounts: Vec<AccountBalanceRow>,
    pub negative_cash: Vec<NegativeCash>,
    /// The accounts this answer covered, and the known accounts it did not.
    ///
    /// On the answer rather than on a row for the reason `negative_cash` is: it
    /// is a fact about the set, and a row cannot state that another account was
    /// left out — there is no row for an account outside the contour, which is
    /// exactly the silence this field breaks.
    pub population: ReportPopulation,
}

/// What the owner expects a negative balance on one account to mean.
///
/// **An expectation, not a rule.** A first draft of `iaam-d41s` had the owner
/// record that an account *cannot be overdrawn*, and he corrected it: a
/// technical overdraft on a debit card is real and ordinary. What survived the
/// correction is a warning about a probable error, and nothing here refuses,
/// suppresses, or invalidates anything. A balance that contradicts the
/// expectation is stated with the expectation beside it, and the reader judges.
///
/// **It is a separate declaration, and it is never derived from the class of
/// cash the account holds.** «A savings account cannot be overdrawn, therefore
/// warn» is exactly the branch decision 0004 §3 forbids by name, and it is
/// wrong on the first ordinary technical overdraft. `CashAssetClass` is a
/// grouping label with one consumer — report headings — and this value is a
/// second value with a second consumer. Nothing reads one to produce the other.
///
/// **Absence is a value.** `Option::None` is «the owner has not said», which is
/// not the same as either variant, and it is never filled in by inference from
/// a title, a class or a transaction pattern. An account with nothing asserted
/// behaves exactly as every account did before this existed.
///
/// **It lives in the core because the core reads it**, which is the difference
/// from `CashAssetClass`: that label is stored in the storage adapter precisely
/// so no rule can reach it, while this value is read by one computation — the
/// balances answer's negative-cash entries, in this module. Its home says where
/// it may be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NegativeBalanceExpectation {
    /// A negative balance here would probably be an error: a missing opening
    /// assertion, an unimported month, a misfiled transfer.
    Unexpected,
    /// A negative balance here is ordinary — a credit line, a margin balance.
    /// Recorded so that silence is not read as a statement.
    Ordinary,
}

impl NegativeBalanceExpectation {
    /// All variants, for table-driven tests: a list written by hand would
    /// silently drift from the `enum`.
    pub const ALL: [Self; 2] = [Self::Unexpected, Self::Ordinary];

    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unexpected => "unexpected",
            Self::Ordinary => "ordinary",
        }
    }

    /// Parse a code. `None` rather than a default: an unrecognised code must
    /// reach the caller instead of becoming a statement the owner never made.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.code() == code)
    }
}

/// One account-and-currency whose cash balance is negative at the report date,
/// with the perimeter span it is the tail of.
///
/// The amount comes from the projection and the span from the assessment, and
/// the **projection decides which entries exist**. A figure is never withheld
/// for want of an explanation: if the assessment produced no open span for the
/// key, the entry is still here with the number on it and `span` is `None`.
/// Driving the list from the spans instead would let a disagreement between the
/// two folds silently drop a negative balance from the answer, which is the one
/// outcome §11 forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegativeCash {
    pub account: AccountId,
    pub money: Money,
    pub span: Option<NegativeCashSpan>,
    /// What the owner said a negative balance on this account would mean, if he
    /// said anything. `None` is «he has not said», and an entry carrying it is
    /// exactly the entry this report produced before the expectation existed.
    ///
    /// It rides on the entry rather than standing in a list of its own for the
    /// reason the span does: `iaam-sbht` required one notion of negative cash,
    /// not two keyed differently. The perimeter's classification is evidence
    /// about *why* the balance is negative; this is the owner's prior about
    /// whether it should be. They are layered, and both hang here.
    pub expectation: Option<NegativeBalanceExpectation>,
}

impl NegativeCash {
    /// Whether this figure contradicts what the owner expected of this account.
    ///
    /// True only where he said a negative balance would be unexpected. Silence
    /// is not a contradiction, and `Ordinary` is the opposite of one.
    ///
    /// Derived, never stored: a flag set beside the expectation could disagree
    /// with it, and then the answer would contradict itself about what the
    /// owner said.
    ///
    /// **A warning, and nothing more.** Nothing refuses a request, drops a row,
    /// suppresses a figure or fails an invariant on this. It is true or false
    /// beside a number the report states either way.
    #[must_use]
    pub const fn contradicts_expectation(&self) -> bool {
        matches!(
            self.expectation,
            Some(NegativeBalanceExpectation::Unexpected)
        )
    }
}

impl BalancesReport {
    /// What would have to be true for these figures to be a complete statement
    /// of what the owner holds, and which of those things are not.
    ///
    /// Derived on demand from the fields above rather than stored beside them.
    /// A stored register is a second copy that can fall behind the rows it
    /// summarises; this one is recomputed from the same values the response
    /// publishes, and cannot.
    ///
    /// **What is not here, and why.** `negative_cash` is not a caveat. A
    /// negative balance is a fact the answer states and the reader judges — a
    /// technical overdraft is real and a margin balance belongs in NAV — and
    /// nothing about it makes the figures incomplete. Listing it would put a
    /// caveat on almost every report for a reason that is not incompleteness,
    /// and a register that fires always is a register nobody reads. Where a
    /// negative balance *does* make the account's period incomplete, §11 says
    /// so through `period_reports`, and that is the caveat below.
    #[must_use]
    pub fn confidence(&self) -> ReportConfidence {
        // The population first: an account left out of the answer is a silence
        // no row can break, and it is the one the reported difficulty was
        // about.
        let mut caveats = self.population.caveats();
        for row in &self.accounts {
            for cash in &row.cash {
                if cash.opening == CashOpening::Unasserted {
                    caveats.push(Caveat::new(
                        CaveatKind::RunningCashSum,
                        CaveatSubject::AccountCurrency {
                            account: row.account,
                            currency: cash.money.currency(),
                        },
                    ));
                }
            }
            if matches!(row.period_reports, PeriodReports::Refused(_)) {
                caveats.push(Caveat::new(
                    CaveatKind::PeriodReportsRefused,
                    CaveatSubject::Account(row.account),
                ));
            }
        }
        ReportConfidence::new(ReportGoal::AssetSnapshot, caveats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourId, ContourVersion};
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::report::population::{AccountStanding, PopulationAccount, PopulationCompleteness};
    use uuid::Uuid;

    fn account(index: u128) -> AccountId {
        AccountId(Uuid::from_u128(index))
    }

    fn population(standings: &[AccountStanding]) -> ReportPopulation {
        ReportPopulation {
            contour: ContourId(Uuid::from_u128(1)),
            version: ContourVersion(1),
            accounts: standings
                .iter()
                .enumerate()
                .map(|(index, standing)| PopulationAccount {
                    account: account(index as u128 + 10),
                    title: format!("Account {index}"),
                    standing: *standing,
                })
                .collect(),
        }
    }

    fn row(opening: CashOpening) -> AccountBalanceRow {
        AccountBalanceRow {
            account: account(10),
            cash: vec![AccountCash {
                money: Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
                opening,
            }],
            reconciliation: Vec::new(),
            positions: Vec::new(),
            period_reports: PeriodReports::Calculated,
        }
    }

    fn report(standings: &[AccountStanding], opening: CashOpening) -> BalancesReport {
        BalancesReport {
            accounts: vec![row(opening)],
            negative_cash: Vec::new(),
            population: population(standings),
        }
    }

    /// The invariant the register exists for. A report over a population
    /// somebody has not ruled on, or one carrying a figure accumulated from an
    /// unknown start, is not a complete statement of what the owner holds —
    /// however ordinary its rows look. This is the failure being prevented: a
    /// confident number over an incomplete population.
    #[test]
    fn a_partial_population_or_a_running_sum_is_never_complete() {
        let partial_populations = [
            vec![AccountStanding::Covered, AccountStanding::OutsideUndecided],
            vec![
                AccountStanding::Covered,
                AccountStanding::OutsidePlacedElsewhere,
            ],
        ];
        for standings in partial_populations {
            for opening in [CashOpening::Asserted, CashOpening::Unasserted] {
                let report = report(&standings, opening);
                assert_ne!(
                    report.population.completeness(),
                    PopulationCompleteness::Whole
                );
                assert!(
                    !report.confidence().complete(),
                    "a report over a {:?} population read as complete",
                    report.population.completeness()
                );
            }
        }

        let whole = report(&[AccountStanding::Covered], CashOpening::Unasserted);
        assert_eq!(
            whole.population.completeness(),
            PopulationCompleteness::Whole
        );
        assert!(
            !whole.confidence().complete(),
            "a running sum over a whole population read as complete"
        );
    }

    /// The only shape that may read as complete, so that the assertion above is
    /// not passing because nothing ever does.
    #[test]
    fn a_whole_population_of_asserted_balances_is_complete() {
        let report = report(&[AccountStanding::Covered], CashOpening::Asserted);
        let confidence = report.confidence();
        assert!(confidence.complete());
        assert!(confidence.caveats().is_empty());
        assert_eq!(confidence.goal(), ReportGoal::AssetSnapshot);
    }

    /// Each caveat names the account and currency whose `opening` says the same
    /// thing, and points at that field. Without the currency the reader is sent
    /// to the wrong row of a multi-currency account.
    #[test]
    fn a_running_sum_caveat_names_the_account_and_currency_and_the_field() {
        let report = report(&[AccountStanding::Covered], CashOpening::Unasserted);
        let confidence = report.confidence();
        let caveat = confidence.caveats().first().expect("one caveat");
        assert_eq!(caveat.kind(), CaveatKind::RunningCashSum);
        assert_eq!(
            caveat.subject(),
            CaveatSubject::AccountCurrency {
                account: account(10),
                currency: CurrencyCode::Rub,
            }
        );
        assert_eq!(caveat.see(), "accounts[].cash[].opening");
    }

    /// §11 refusing one account's period reports is a caveat, and points at the
    /// field that says so.
    #[test]
    fn a_refused_period_is_a_caveat_naming_the_account() {
        let mut report = report(&[AccountStanding::Covered], CashOpening::Asserted);
        report.accounts[0].period_reports = PeriodReports::Refused(Vec::new());
        let confidence = report.confidence();
        let caveat = confidence.caveats().first().expect("one caveat");
        assert_eq!(caveat.kind(), CaveatKind::PeriodReportsRefused);
        assert_eq!(caveat.subject(), CaveatSubject::Account(account(10)));
        assert_eq!(caveat.see(), "accounts[].period_reports");
    }

    /// The owner's expectation is his statement about the account, and a
    /// negative balance that contradicts it is reported as contradicting it —
    /// as a warning, never as a refusal. An account he has said nothing about
    /// behaves exactly as every account did before this existed.
    #[test]
    fn a_negative_balance_contradicts_only_an_expectation_that_was_stated() {
        let cases = [
            (None, false),
            (Some(NegativeBalanceExpectation::Ordinary), false),
            (Some(NegativeBalanceExpectation::Unexpected), true),
        ];
        for (expectation, contradicts) in cases {
            let mut report = report(&[AccountStanding::Covered], CashOpening::Asserted);
            let entry = NegativeCash {
                account: account(10),
                money: Money::new(PostedMinor::new(-500), CurrencyCode::Rub),
                span: None,
                expectation,
            };
            report.negative_cash = vec![entry];
            assert_eq!(
                entry.contradicts_expectation(),
                contradicts,
                "{expectation:?}"
            );
            // The figure is stated either way, and the report stays whole: a
            // contradicted expectation is a fact the answer publishes, not a
            // refusal and not an incompleteness.
            assert_eq!(report.negative_cash[0].money.amount().raw(), -500);
            assert!(
                report.confidence().complete(),
                "a contradicted expectation is not a gap in the figures: {expectation:?}"
            );
        }
    }

    /// The expectation is the owner's, and it is round-tripped through its code
    /// rather than through a default. An unrecognised code is not a statement.
    #[test]
    fn an_expectation_survives_its_code_and_an_unknown_one_is_not_a_statement() {
        for value in NegativeBalanceExpectation::ALL {
            assert_eq!(
                NegativeBalanceExpectation::from_code(value.code()),
                Some(value)
            );
        }
        assert_eq!(NegativeBalanceExpectation::from_code("savings"), None);
        assert_eq!(NegativeBalanceExpectation::from_code(""), None);
    }
    /// A negative balance is a fact the answer states and the reader judges,
    /// not an incompleteness. A register that fired on every overdraft would
    /// fire on almost every report.
    #[test]
    fn a_negative_balance_alone_does_not_make_the_answer_incomplete() {
        let mut report = report(&[AccountStanding::Covered], CashOpening::Asserted);
        report.negative_cash = vec![NegativeCash {
            account: account(10),
            money: Money::new(PostedMinor::new(-500), CurrencyCode::Rub),
            span: None,
            expectation: None,
        }];
        assert!(report.confidence().complete());
    }
}
