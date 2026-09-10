//! An account row that should never have existed (`iaam-o0oj`).
//!
//! **A third axis, and neither of the other two is it.** A retirement
//! ([`crate::retirement`]) says a product existed and then ceased; a scope
//! exclusion says a product exists and the owner's reports leave it out on
//! purpose. Both statements presuppose an account: something real that either
//! stopped or was ruled out of a report. A row a probe created, or one an agent
//! rebuilding one instance from another minted by mistake, is neither — there
//! was no product to cease and no money to leave out of anything, because
//! there was never an account here at all.
//!
//! A third field session against a live instance found three such rows and
//! confirmed the owner had created none of them. Retiring them said "these
//! products existed and ended", which was false. Ruling them outside every
//! scope said "I have decided my reports do not want this money", which was
//! also false — he had decided nothing, because there was nothing of his to
//! decide about. Both acts left the row standing and both are indistinguishable,
//! a year later, from an owner who really did retire a product or really did
//! exclude one. That is the substitution this module exists to stop: it gives
//! "this row was a mistake" a sentence of its own, instead of borrowing one that
//! already means something else.
//!
//! **What it does that neither neighbour can.** A retirement and a scope
//! exclusion both *annotate* the account inside a report's population — see
//! [`crate::report::population::AccountStanding`] and
//! [`crate::report::population::PopulationAccount::retirement`]. A retraction
//! *removes* it: [`crate::report::population::ReportPopulation`] is built from
//! the accounts a caller hands it, and a retracted account is not among them.
//! Nothing was omitted, because there was never an account to leave out — which
//! is exactly the sentence a caveat cannot say about a row it is still being
//! asked to annotate.
//!
//! **Why that is refused while the account carries a business fact.** An
//! account money moved through is not an artefact to erase — the movements are
//! real, whatever the account row's own origin was. [`accept_retraction`] is
//! the rule, and it takes the one fact [`crate::retirement::accept_retirement`]
//! deliberately does not: retiring a non-empty account is allowed because the
//! fold already handles it (see that module's own reasoning); retracting one is
//! refused, full stop, because there is no fold that can un-happen a movement.
//! An account must be brought to nothing before it can be un-created.
//!
//! **Why it is withdrawable.** A wrong retraction is not "a queue item stayed
//! open a while longer" — it is an account quietly dropped from every report
//! and every reading of the outstanding-work queue. That is a bigger mistake
//! than the one it corrects, so the declaration follows the same doctrine every
//! other standing decision in this system does: it is a further statement, not
//! an erasure, and it can be taken back under the same key.

/// Why a declaration was refused.
///
/// A closed set rather than a message, for the reason
/// [`crate::retirement::RetirementRefusal`] is: the transport answers each one
/// in its own vocabulary, and a test pins which refusal happened rather than
/// the prose that reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetractionRefusal {
    /// A retraction already stands for this account.
    ///
    /// Refused rather than accepted a second time, for
    /// [`crate::retirement::RetirementRefusal::AlreadyRetired`]'s reason: every
    /// accepted call is a fact a reader can see happen, and a second
    /// declaration that changed nothing would be a fact that means nothing.
    AlreadyRetracted,
    /// Nothing is being withdrawn, because nothing stands.
    NotRetracted,
    /// The account carries a business fact.
    ///
    /// You cannot un-exist a thing money moved through. The account must be
    /// brought to nothing — a correction retracting the fact, or, where the
    /// fact is real and the product simply ceased, a retirement instead of a
    /// retraction — before this call can succeed.
    AccountNotEmpty,
}

/// Whether a retraction may be recorded.
///
/// `currently_retracted` is whether a retraction already stands for this
/// account; `has_business_fact` is whatever the journal holds against it,
/// read the same way [`crate::report::population`] and the outstanding-work
/// queue already do. Passed in rather than read, so the rule is testable
/// without a store — the same arrangement [`crate::retirement::accept_retirement`]
/// uses for its own two facts.
pub const fn accept_retraction(
    currently_retracted: bool,
    has_business_fact: bool,
) -> Result<(), RetractionRefusal> {
    if currently_retracted {
        return Err(RetractionRefusal::AlreadyRetracted);
    }
    if has_business_fact {
        return Err(RetractionRefusal::AccountNotEmpty);
    }
    Ok(())
}

/// Whether a retraction may be withdrawn.
///
/// The mirror of [`accept_retraction`], for
/// [`crate::retirement::accept_withdrawal`]'s reason: a retraction is
/// otherwise a statement that can be made in error and never taken back, and
/// the account it names would stay invisible to every report for good.
pub const fn accept_retraction_withdrawal(
    currently_retracted: bool,
) -> Result<(), RetractionRefusal> {
    if !currently_retracted {
        return Err(RetractionRefusal::NotRetracted);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_retraction_of_an_empty_account_is_accepted() {
        assert_eq!(accept_retraction(false, false), Ok(()));
    }

    /// You cannot un-exist a thing money moved through.
    #[test]
    fn an_account_carrying_a_business_fact_is_refused() {
        assert_eq!(
            accept_retraction(false, true),
            Err(RetractionRefusal::AccountNotEmpty)
        );
    }

    /// The standing declaration is not silently replaced: a caller must
    /// withdraw it first, so the change is a fact a reader can see.
    #[test]
    fn a_second_retraction_over_one_that_stands_is_refused() {
        assert_eq!(
            accept_retraction(true, false),
            Err(RetractionRefusal::AlreadyRetracted)
        );
        // Standing outranks emptiness: the state that already exists is
        // reported first, exactly as `accept_retirement`'s own test pins for
        // the standing-over-future-date case.
        assert_eq!(
            accept_retraction(true, true),
            Err(RetractionRefusal::AlreadyRetracted)
        );
    }

    #[test]
    fn a_withdrawal_needs_a_statement_to_withdraw() {
        assert_eq!(accept_retraction_withdrawal(true), Ok(()));
        assert_eq!(
            accept_retraction_withdrawal(false),
            Err(RetractionRefusal::NotRetracted)
        );
    }
}
