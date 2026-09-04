//! Which of the owner's products still exist (`iaam-gua5`).
//!
//! **The perimeter a calculation folds over and the inventory of products the
//! owner still has are two axes, and this is the second one.** A
//! [`crate::contour::ContourDefinition`] answers "whose money is in these
//! figures"; a retirement answers "does this product exist any more". They were
//! one axis for as long as the system had only the first, and the case that
//! showed the cost is a term deposit that was closed and its balance returned
//! to another account of the owner's: keeping it inside the contour reports the
//! interest as an earning and the closing movement as internal, and leaves a
//! zero-balance shell in every asset report for ever; dropping it from a later
//! contour version removes the shell and destroys both of the others, because
//! the closing transfer then crosses the boundary and the interest stops being
//! folded at all.
//!
//! So the retirement is a declaration of its own, and three properties are what
//! make it safe:
//!
//! **It never reaches [`crate::contour::classify`].** A retired account stays a
//! contour member; the closing transfer stays internal and the interest stays
//! an earning, whenever the report is run and however long after the product
//! ceased. A retirement that changed classification would be exactly the
//! retroactive rewriting of history that contour versions exist to prevent,
//! arriving through another door — and a report run a year later would answer a
//! different question from the one it answered when it was published.
//!
//! **It never removes an account from a report's population.** The population
//! states which of the owner's known accounts the report answered about, and a
//! calculation that still folds an account must still name it. What retirement
//! adds there is a date beside the entry, never an absence:
//! [`crate::report::population::PopulationAccount::retirement`].
//!
//! **It is not derived from anything.** Not from a balance that reached zero,
//! not from an account that stopped moving, not from a label, and — when
//! deposit contracts arrive — not from a contract's scheduled end. Each of
//! those is wrong on the first deposit closed early, and a rule reading a
//! declared label is what `iaam-store`'s `CashAssetClass` doctrine refuses by
//! name. It is the owner's statement of a fact, on a date.
//!
//! **The rule for the contract that has not been built yet.** A deposit
//! contract (epic E3.5) will carry a *scheduled* end date; this carries the
//! *actual* one. Two things saying "this deposit ended" is how they come to
//! disagree, and the rule that settles it is already in force one domain over:
//! a bond has a payment schedule and actual payments, and the posting match
//! compares one against the other with a verdict for a payment that was due and
//! did not arrive. **The schedule predicts; the journal records; a plan never
//! overrides a fact.** A deposit closed early is that shape exactly — the
//! contract's end date is a prediction the retirement falsifies, and nothing
//! may read the contract to conclude that a product ceased.

use time::Date;

use crate::ids::AccountId;

/// Which state of the owner's retirement declarations a figure was computed
/// under.
///
/// **A coordinate, for the reason [`crate::contour::ContourVersion`] is one.**
/// A retirement changes what an asset snapshot prints, so an unversioned flag
/// would change what an already-published snapshot says — the failure the
/// contour tables' immutability triggers exist to prevent, on a second axis.
/// A report states the revision it read, so "the same report at the same
/// contour version and the same retirement revision" is the same answer for
/// ever.
///
/// It is **per owner and not per account**: one monotone sequence over every
/// retirement he has declared, so a single number identifies the whole state of
/// this axis, exactly as one contour version identifies a whole membership. A
/// per-account counter would need one number per account to say the same thing,
/// and a report could then state a coordinate no reader could compare.
///
/// `0` is "he has declared none", which is where every owner starts and where
/// most stay. It is a real revision and not a missing one: a report over an
/// owner who has retired nothing states `0`, and a reader comparing it against
/// a later `3` learns that something on this axis moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetirementRevision(pub u32);

impl RetirementRevision {
    /// The revision of an owner who has declared nothing.
    pub const NONE: Self = Self(0);

    /// The revision a further declaration mints.
    ///
    /// Named `successor` rather than `next`: this is not an iterator, and a
    /// method named for one invites a reader to expect a sequence that ends.
    ///
    /// Saturating rather than wrapping: at `u32::MAX` the honest failure is a
    /// revision that stops advancing, and the alternative is one that returns
    /// to a coordinate a published report already used. Neither is reachable by
    /// an owner declaring retirements by hand, and only one of them is silent.
    #[must_use]
    pub const fn successor(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// The owner's statement that one product ceased to exist, on a date.
///
/// Two fields and no more, and each absence is deliberate:
///
/// - **No reason.** A scope exclusion carries one because "outside every
///   contour" is a judgement a year later cannot reconstruct; "this product no
///   longer exists" is a fact, and the date is the whole of it.
/// - **No kind, no term, no rate.** Those are a deposit contract's (E3.5), and
///   a contract states what a product *is* while this states that it *ended*.
///   A design that finds itself wanting one of them here has crossed into that
///   epic.
/// - **No moment of recording.** The revision carries when, in the only sense a
///   report can compare; `effective_on` carries the date in the owner's own
///   history, which is the one every report asks about.
///
/// `effective_on` is a date and not a timestamp because every report that
/// consults it asks a question about a day: an asset snapshot is taken `as_of`
/// a date, and "did this product exist then" is answered by the same
/// granularity or it is answered by a fiction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountRetirement {
    pub account: AccountId,
    pub effective_on: Date,
}

impl AccountRetirement {
    /// Whether the product had ceased by the date a report is taken at.
    ///
    /// Inclusive of the effective date itself: the owner names the day the
    /// product ceased, and a snapshot taken that evening is taken after it
    /// ceased. The alternative — a strict comparison — would make the boundary
    /// day the one day a reader could not predict, and the closing movement is
    /// dated on it in every statement that prints one.
    #[must_use]
    pub fn in_force_on(&self, as_of: Date) -> bool {
        self.effective_on <= as_of
    }
}

/// Why a declaration was refused.
///
/// A closed set rather than a message, so the transport can answer each one in
/// its own vocabulary and a test can pin which refusal happened rather than the
/// prose that reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementRefusal {
    /// A retirement already stands for this account.
    ///
    /// Refused rather than replaced, and this is the one refusal worth arguing
    /// for. A second statement with a different date would silently move the
    /// boundary under every snapshot already published between the two dates,
    /// which is the retroactivity this whole design exists to avoid. Restating
    /// it is therefore two acts — withdraw, then declare — and the withdrawal
    /// is a revision of its own that a reader can see.
    AlreadyRetired { effective_on: Date },
    /// Nothing is being withdrawn, because nothing stands.
    ///
    /// A no-op would be friendlier and is refused all the same: every accepted
    /// call mints a revision, and a revision that changed nothing is a
    /// coordinate that means nothing — an owner comparing two reports at
    /// revisions 6 and 7 would find no difference and no explanation. Refusing
    /// keeps the sequence's one promise: **every revision changed something.**
    NotRetired,
    /// The date is after today.
    ///
    /// A product that has not ceased yet has not ceased. Accepting the
    /// statement would arm a suppression that begins on a day nobody revisits,
    /// and the system would start printing a different asset snapshot without
    /// anybody acting — which is the one way a declaration can change a report
    /// with no act behind the change. A prediction is not a fact (§4.9), and
    /// this is the only place a date could smuggle one in.
    NotYetCeased { effective_on: Date, today: Date },
}

/// Whether a retirement may be recorded.
///
/// `current` is the effective date of the statement that stands for this
/// account, if one does. `today` is passed in rather than read, so the rule can
/// be tested without a clock: this is the same arrangement, for the same
/// reason, as the reports scenario's own date comparison.
///
/// **Nothing here consults the journal.** Retiring an account that still holds
/// money is deliberately *not* refused, and it is worth saying why, because the
/// refusal is the first one a reader reaches for:
///
/// 1. The fold already handles it, and unconditionally. A retired account's row
///    is suppressed only where every one of its figures is zero, so an account
///    that still holds something keeps its row and earns a caveat. A refusal
///    here would buy nothing the snapshot does not already guarantee.
/// 2. What a refusal could read is "what the journal holds today", and that is
///    not the question a retirement answers. The two disagree exactly where the
///    system's knowledge is short — a deposit whose principal predates the
///    imported interval sums to a figure that is movement from an unknown start
///    and is not a balance at all — so the refusal would block the owner from
///    stating a true fact because an import has not happened yet.
/// 3. It is his statement about his product. The system records what he said
///    and reports the disagreement; refusing his word because a fold disagrees
///    with it inverts the discipline every other declaration here follows.
pub fn accept_retirement(
    current: Option<Date>,
    effective_on: Date,
    today: Date,
) -> Result<(), RetirementRefusal> {
    if let Some(effective_on) = current {
        return Err(RetirementRefusal::AlreadyRetired { effective_on });
    }
    if effective_on > today {
        return Err(RetirementRefusal::NotYetCeased {
            effective_on,
            today,
        });
    }
    Ok(())
}

/// Whether a retirement may be withdrawn.
///
/// The mirror of [`accept_retirement`], and the reason withdrawal exists at
/// all: a retirement is otherwise a statement the owner can make by a typo and
/// never take back, and one of the two remedies the asset report names for a
/// retired account that still holds money is "the retirement was premature".
///
/// A withdrawal does not erase anything. The declaration is an append-only
/// history and the withdrawal is a further row in it, so a report at an earlier
/// revision still sees the retirement exactly as it stood.
pub fn accept_withdrawal(current: Option<Date>) -> Result<(), RetirementRefusal> {
    if current.is_none() {
        return Err(RetirementRefusal::NotRetired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;
    use uuid::Uuid;

    const TODAY: Date = date!(2026 - 03 - 20);

    fn retirement(effective_on: Date) -> AccountRetirement {
        AccountRetirement {
            account: AccountId(Uuid::from_u128(1)),
            effective_on,
        }
    }

    /// The boundary day belongs to the report taken on it.
    ///
    /// The closing movement of a product is dated on the day it ceased in every
    /// statement that prints one, so a strict comparison would leave exactly
    /// one day on which the account is neither in use nor retired.
    #[test]
    fn a_retirement_is_in_force_on_its_own_effective_date() {
        let ceased = retirement(date!(2026 - 03 - 10));
        assert!(!ceased.in_force_on(date!(2026 - 03 - 09)));
        assert!(ceased.in_force_on(date!(2026 - 03 - 10)));
        assert!(ceased.in_force_on(date!(2026 - 03 - 11)));
    }

    #[test]
    fn a_first_retirement_on_a_past_date_is_accepted() {
        assert_eq!(
            accept_retirement(None, date!(2026 - 03 - 10), TODAY),
            Ok(())
        );
    }

    #[test]
    fn a_retirement_dated_today_is_accepted() {
        assert_eq!(accept_retirement(None, TODAY, TODAY), Ok(()));
    }

    /// A date in the future would arm a suppression that begins with no act
    /// behind it: the same request, replayed a month later, would answer
    /// differently because a day passed.
    #[test]
    fn a_retirement_dated_after_today_is_refused() {
        assert_eq!(
            accept_retirement(None, date!(2026 - 03 - 21), TODAY),
            Err(RetirementRefusal::NotYetCeased {
                effective_on: date!(2026 - 03 - 21),
                today: TODAY,
            })
        );
    }

    /// The second statement is refused rather than replacing the first, because
    /// replacing it moves the boundary under every snapshot already published
    /// between the two dates.
    #[test]
    fn a_second_retirement_does_not_silently_move_the_boundary() {
        let standing = date!(2026 - 03 - 10);
        assert_eq!(
            accept_retirement(Some(standing), date!(2026 - 03 - 01), TODAY),
            Err(RetirementRefusal::AlreadyRetired {
                effective_on: standing
            })
        );
        // Even restating the very same date is refused: it would mint a
        // revision that changed nothing, which is the coordinate this axis
        // promises never to publish.
        assert_eq!(
            accept_retirement(Some(standing), standing, TODAY),
            Err(RetirementRefusal::AlreadyRetired {
                effective_on: standing
            })
        );
    }

    /// The future-date rule is checked *after* the standing statement, so a
    /// second declaration is reported as the conflict it is rather than as a
    /// bad date.
    #[test]
    fn a_standing_retirement_outranks_a_future_date() {
        let standing = date!(2026 - 03 - 10);
        assert_eq!(
            accept_retirement(Some(standing), date!(2027 - 01 - 01), TODAY),
            Err(RetirementRefusal::AlreadyRetired {
                effective_on: standing
            })
        );
    }

    #[test]
    fn a_withdrawal_needs_a_statement_to_withdraw() {
        assert_eq!(accept_withdrawal(Some(date!(2026 - 03 - 10))), Ok(()));
        assert_eq!(accept_withdrawal(None), Err(RetirementRefusal::NotRetired));
    }

    /// Every accepted call moves the coordinate, so a reader comparing two
    /// revisions is comparing two different states.
    #[test]
    fn the_revision_advances_from_the_state_of_having_declared_nothing() {
        assert_eq!(RetirementRevision::NONE, RetirementRevision(0));
        assert_eq!(RetirementRevision::NONE.successor(), RetirementRevision(1));
        assert_eq!(RetirementRevision(7).successor(), RetirementRevision(8));
    }

    /// The saturating step is the deliberate end of the sequence: it stops
    /// advancing rather than returning to a coordinate a published report used.
    #[test]
    fn the_revision_does_not_return_to_a_coordinate_already_published() {
        assert_eq!(
            RetirementRevision(u32::MAX).successor(),
            RetirementRevision(u32::MAX)
        );
    }
}
