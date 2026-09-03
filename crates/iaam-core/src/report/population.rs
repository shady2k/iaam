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
/// The three outside variants are the distinction that makes the manifest worth
/// having: "four accounts are outside this report and nobody has decided
/// whether they belong" is a different sentence from "four accounts are outside
/// this report on purpose", and a manifest that could not tell them apart would
/// let the first be read as the second.
///
/// They are declared strongest ruling first, and they grade **what was said**
/// rather than how far outside the account is. The middle one is why there are
/// three and not two: for one wave, membership in another contour was the only
/// evidence of a ruling this type could read, so it was published as the
/// deliberate omission — and an owner who had ruled in as many words, with a
/// reason, was told nobody had. Reading the disposition splits the two apart,
/// and what is left in the middle claims exactly what it can prove: he decided
/// where the account belongs; he did not say it does not belong here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStanding {
    /// Inside the contour the report was folded over: this account's facts are
    /// in the answer.
    Covered,
    /// Outside this report because the owner ruled the account outside every
    /// contour of his and said why.
    ///
    /// The authoritative disposition, recorded per account and per owner
    /// because it is a statement about the account that no single contour owns.
    /// Nothing here is inferred, and there is nothing further to ask him: the
    /// absence is answered, not open.
    OutsideByDecision,
    /// Outside this report, and some other contour of the owner's names the
    /// account.
    ///
    /// He has ruled on where it belongs. He has **not** ruled that it does not
    /// belong in this report, and this variant claims no more than that — which
    /// is the whole of the difference between it and
    /// [`Self::OutsideByDecision`].
    OutsidePlacedElsewhere,
    /// Outside this report, in no contour at all, and carrying no disposition.
    /// Nobody has ruled on whether it belongs, so its absence is an open
    /// question and not a decision.
    OutsideUndecided,
}

impl AccountStanding {
    /// The machine-readable name carried to a caller.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::OutsideByDecision => "outside_by_decision",
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
/// manifest without them. `docs/api/conventions.md` §3 is that sentence made a
/// rule for the whole API, and it cites this type as where the rule was first
/// written down.
///
/// The institution follows for the case the title alone cannot settle: two
/// accounts the owner calls the same word, at two banks, are one line apart in
/// an `outside` list and are not the same question. `None` is "he has not said
/// where it is held" and is never filled in — an invented institution would
/// tell two accounts apart by a fiction, which is worse than not telling them
/// apart at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopulationAccount {
    pub account: AccountId,
    pub title: String,
    pub institution: Option<String>,
    pub standing: AccountStanding,
}

/// How much of what the system knows about one report answered about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopulationCompleteness {
    /// Every account the system knows of is inside the report.
    Whole,
    /// Accounts are outside the report, and the owner has ruled on every one of
    /// them — each is either placed in a contour of his own or ruled outside
    /// every contour with a reason. The answer is partial by decision.
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

    /// The known accounts outside it, on a ruling or otherwise.
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
    ///
    /// An account the owner ruled outside is `Bounded` and **not** `Whole`, and
    /// the temptation to make it `Whole` is worth answering here. `Whole` is a
    /// statement about the figures — every account the system knows of is in
    /// them — and not a grade of the owner's housekeeping. Money he has ruled
    /// out of every contour is still money the system knows he has, so a report
    /// answering `whole` over it would tell a reader the figures cover
    /// everything when they cover everything *he chose*. What his decision
    /// changes is the sentence the reader is given — the standing, and the kind
    /// of caveat — never whether he is given one.
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
    /// The three outside standings keep their distinction here for the reason
    /// [`AccountStanding`] draws it — a ruling, a placement and an open
    /// question are three different sentences — and all three are caveats,
    /// because all three make the figures an answer about part of the owner's
    /// money.
    ///
    /// Written as a `filter_map` over every account rather than a `map` over
    /// [`Self::outside`] so that the match is exhaustive over the standings: a
    /// fifth standing does not compile until somebody has said which line of
    /// the register it produces, and the alternative — a catch-all arm — is
    /// exactly how [`AccountStanding::OutsideByDecision`] would have been
    /// silently reported as a placement elsewhere.
    ///
    /// This is exactly the complement of [`Self::completeness`]: empty if and
    /// only if the population is [`PopulationCompleteness::Whole`], which is
    /// what keeps a report over a partial population from ever reading as
    /// complete.
    #[must_use]
    pub fn caveats(&self) -> Vec<Caveat> {
        self.accounts
            .iter()
            .filter_map(|entry| {
                let kind = match entry.standing {
                    AccountStanding::Covered => return None,
                    AccountStanding::OutsideByDecision => CaveatKind::AccountRuledOutside,
                    AccountStanding::OutsidePlacedElsewhere => CaveatKind::AccountInAnotherScope,
                    AccountStanding::OutsideUndecided => CaveatKind::AccountInNoScope,
                };
                Some(Caveat::new(kind, CaveatSubject::Account(entry.account)))
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
                    institution: None,
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
