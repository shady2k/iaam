//! Which of the owner's accounts a report answered about.
//!
//! Selection happens **before** the fold, so nothing computed afterwards can
//! see what was left out. These types were written beside the balances
//! scenario; they live in the core because the caveat register in
//! [`crate::report::confidence`] is derived from them, and a summary computed
//! outside the core can disagree with the report it summarises.

use crate::contour::{ContourId, ContourVersion};
use crate::ids::AccountId;

use super::confidence::{Caveat, CaveatKind, CaveatSubject};

/// Where one of the owner's accounts stands with respect to a report's
/// population.
///
/// Selection happens **before** the fold, so nothing computed afterwards can
/// see what was left out: a report's quality fields all speak about defects
/// inside the calculation and are silent about whose money was calculated. The
/// standing is the second statement, made per account because that is the
/// granularity at which the owner decides.
///
/// The two outside variants are the distinction that makes the manifest worth
/// having: "four accounts are outside this report and nobody has decided
/// whether they belong" is a different sentence from "four accounts are outside
/// this report on purpose", and a manifest that could not tell them apart would
/// let the first be read as the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStanding {
    /// Inside the contour the report was folded over: this account's facts are
    /// in the answer.
    Covered,
    /// Outside this report, and the owner has placed the account in a contour
    /// of his own. Something has ruled on where it belongs.
    OutsidePlacedElsewhere,
    /// Outside this report and in no contour at all. Nobody has ruled on
    /// whether it belongs, so its absence is an open question and not a
    /// decision.
    OutsideUndecided,
}

impl AccountStanding {
    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::OutsidePlacedElsewhere => "outside_placed_elsewhere",
            Self::OutsideUndecided => "outside_undecided",
        }
    }

    /// Whether this standing keeps the account out of the answer.
    #[must_use]
    pub const fn is_outside(self) -> bool {
        !matches!(self, Self::Covered)
    }
}

/// One of the owner's accounts, and where it stands relative to a report.
///
/// The title travels with the identifier because the manifest exists to be
/// read: an owner asked to rule on an account cannot act on a bare UUID, and a
/// caller that had to fetch the names separately would be free to render the
/// manifest without them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopulationAccount {
    pub account: AccountId,
    pub title: String,
    pub standing: AccountStanding,
}

/// How much of what the system knows about one report answered about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopulationCompleteness {
    /// Every account the system knows of is inside the report.
    Whole,
    /// Accounts are outside the report, and each of them is placed in a contour
    /// the owner drew. The answer is partial by decision.
    Bounded,
    /// Accounts are outside the report that no contour claims. The answer is
    /// partial, and nothing says the omission was meant — this is the state the
    /// reader must not mistake for `Bounded`.
    Undecided,
}

impl PopulationCompleteness {
    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Whole => "whole",
            Self::Bounded => "bounded",
            Self::Undecided => "undecided",
        }
    }
}

/// The population a report answered about: the accounts it covered, and the
/// accounts the system knows of that it did not.
///
/// Held as **one** list rather than a covered list beside an outside list: an
/// account has exactly one standing, and two lists could disagree about which
/// it is or list an account twice. Callers that want one side ask for it.
///
/// This is built from the same `ContourDefinition` the fold was given, at the
/// point the population is chosen. Reconstructing it afterwards from the rows
/// of a result would produce a second, independently-derived answer to what the
/// report covered — and the two would drift on the first change to either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportPopulation {
    pub contour: ContourId,
    pub version: ContourVersion,
    pub accounts: Vec<PopulationAccount>,
}

impl ReportPopulation {
    /// The accounts inside the report's scope.
    pub fn covered(&self) -> impl Iterator<Item = &PopulationAccount> {
        self.accounts
            .iter()
            .filter(|entry| entry.standing == AccountStanding::Covered)
    }

    /// The known accounts outside it, on a decision or otherwise.
    pub fn outside(&self) -> impl Iterator<Item = &PopulationAccount> {
        self.accounts
            .iter()
            .filter(|entry| entry.standing.is_outside())
    }

    /// Those of them nobody has ruled on.
    pub fn undecided(&self) -> impl Iterator<Item = &PopulationAccount> {
        self.accounts
            .iter()
            .filter(|entry| entry.standing == AccountStanding::OutsideUndecided)
    }

    /// What the manifest says about the answer as a whole.
    ///
    /// `Undecided` outranks `Bounded`: one account nobody has ruled on is
    /// enough to make the report an answer about an undecided part of the
    /// owner's money, however many deliberate exclusions stand beside it.
    #[must_use]
    pub fn completeness(&self) -> PopulationCompleteness {
        if self.undecided().next().is_some() {
            return PopulationCompleteness::Undecided;
        }
        if self.outside().next().is_some() {
            return PopulationCompleteness::Bounded;
        }
        PopulationCompleteness::Whole
    }

    /// The manifest's contribution to a report's caveat register: one caveat
    /// per account the report did not cover.
    ///
    /// One per account rather than one saying "fifteen accounts are outside".
    /// The count is the shape of the reported difficulty, not its content: the
    /// owner acts per account, and a caveat he cannot attach to an account is
    /// one he cannot act on.
    ///
    /// The two outside standings keep their distinction here for the reason
    /// [`AccountStanding`] draws it — a deliberate omission and an undecided
    /// one are different sentences — and both are caveats, because both make
    /// the figures an answer about part of the owner's money.
    ///
    /// This is exactly the complement of [`Self::completeness`]: empty if and
    /// only if the population is [`PopulationCompleteness::Whole`], which is
    /// what keeps a report over a partial population from ever reading as
    /// complete.
    #[must_use]
    pub fn caveats(&self) -> Vec<Caveat> {
        self.outside()
            .map(|entry| {
                let kind = match entry.standing {
                    AccountStanding::OutsideUndecided => CaveatKind::AccountInNoScope,
                    _ => CaveatKind::AccountInAnotherScope,
                };
                Caveat::new(kind, CaveatSubject::Account(entry.account))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn population(standings: &[AccountStanding]) -> ReportPopulation {
        ReportPopulation {
            contour: ContourId(Uuid::from_u128(1)),
            version: ContourVersion(1),
            accounts: standings
                .iter()
                .enumerate()
                .map(|(index, standing)| PopulationAccount {
                    account: AccountId(Uuid::from_u128(index as u128 + 10)),
                    title: format!("Account {index}"),
                    standing: *standing,
                })
                .collect(),
        }
    }

    /// The register and the completeness verdict are one statement in two
    /// shapes. If they could disagree, a report could publish `undecided`
    /// beside an empty register and read as complete.
    #[test]
    fn the_register_is_empty_exactly_when_the_population_is_whole() {
        let cases = [
            (vec![AccountStanding::Covered], true),
            (vec![], true),
            (
                vec![AccountStanding::Covered, AccountStanding::OutsideUndecided],
                false,
            ),
            (
                vec![
                    AccountStanding::Covered,
                    AccountStanding::OutsidePlacedElsewhere,
                ],
                false,
            ),
        ];
        for (standings, expected_whole) in cases {
            let population = population(&standings);
            let whole = population.completeness() == PopulationCompleteness::Whole;
            assert_eq!(whole, expected_whole, "{standings:?}");
            assert_eq!(
                population.caveats().is_empty(),
                whole,
                "register disagrees with completeness for {standings:?}"
            );
        }
    }

    #[test]
    fn an_account_nobody_ruled_on_is_not_reported_as_a_decision() {
        let population = population(&[
            AccountStanding::OutsideUndecided,
            AccountStanding::OutsidePlacedElsewhere,
        ]);
        let kinds: Vec<_> = population
            .caveats()
            .iter()
            .map(|caveat| caveat.kind())
            .collect();
        assert_eq!(
            kinds,
            vec![
                CaveatKind::AccountInNoScope,
                CaveatKind::AccountInAnotherScope
            ]
        );
    }
}
