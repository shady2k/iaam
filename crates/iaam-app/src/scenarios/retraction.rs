//! The owner, or the agent that created it, declares that an account should
//! never have existed (`iaam-o0oj`).
//!
//! **Why this is not a spelling of `retirement`.** Read
//! [`iaam_core::retraction`]'s own doc comment first; this module is the act,
//! and the rule it enforces lives there so it can be tested without a
//! database — the same split [`crate::scenarios::retirement`] uses for its own
//! rule.
//!
//! **Authority is attribution, not a flat scope (`iaam-7ffl`).** The owner may
//! always retract; an agent may retract only an account it declared itself.
//! `accounts.declared_by` is what makes that checkable rather than merely
//! asserted, and the shape of the check — read the account's own attribution
//! against the caller's token, inside the same call rather than in a step
//! before it — is `docs/api/conventions.md` §4.5-§4.7's doctrine for retracting
//! a declared import, carried over to retracting a declared account.

use std::collections::BTreeSet;

use iaam_core::ids::{AccountId, PrincipalId};
use iaam_core::retraction::{RetractionRefusal, accept_retraction, accept_retraction_withdrawal};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{AccountActivityView, Principal};

/// What the owner's, or an agent's, declaration stands at after the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountRetractionOutcome {
    pub account: AccountId,
    pub retracted: bool,
}

/// Record that an account should never have existed.
///
/// Refused when the account carries a business fact — see
/// [`iaam_core::retraction::accept_retraction`] — and when a caller other than
/// the owner did not declare the account itself.
///
/// The account is **not** checked for existence here, for
/// [`crate::scenarios::retirement::record_account_retirement`]'s own reason:
/// the transport has already resolved it against the owner's directory, and
/// the store's foreign key refuses what that misses.
pub async fn record_account_retraction(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<AccountRetractionOutcome, AppError> {
    may_retract(services, principal, account).await?;
    let currently_retracted = is_retracted(services, principal, account).await?;
    let has_business_fact = has_business_fact(services, principal, account).await?;
    accept_retraction(currently_retracted, has_business_fact).map_err(refusal)?;
    services
        .store
        .record_account_retraction(principal.owner, account)
        .await?;
    Ok(AccountRetractionOutcome {
        account,
        retracted: true,
    })
}

/// Withdraw the statement, returning the account to standing.
pub async fn withdraw_account_retraction(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<AccountRetractionOutcome, AppError> {
    may_retract(services, principal, account).await?;
    let currently_retracted = is_retracted(services, principal, account).await?;
    accept_retraction_withdrawal(currently_retracted).map_err(refusal)?;
    services
        .store
        .withdraw_account_retraction(principal.owner, account)
        .await?;
    Ok(AccountRetractionOutcome {
        account,
        retracted: false,
    })
}

/// What the owner has said about one account, for a caller that is reading
/// rather than writing.
pub async fn account_retraction(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<AccountRetractionOutcome, AppError> {
    Ok(AccountRetractionOutcome {
        account,
        retracted: is_retracted(services, principal, account).await?,
    })
}

async fn is_retracted(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<bool, AppError> {
    Ok(services
        .store
        .list_account_retractions(principal.owner)
        .await?
        .contains(&account))
}

async fn has_business_fact(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<bool, AppError> {
    Ok(services
        .store
        .list_account_activity(principal.owner)
        .await?
        .into_iter()
        .find(|activity| activity.account == account)
        .is_some_and(|activity| activity.has_business_fact))
}

/// Who may retract this particular account.
///
/// The owner may always retract. An agent may retract only an account it
/// declared itself — attribution is what `iaam-7ffl` bought, and it is the
/// difference between a reviewable withdrawal of one's own artefact and an
/// unauditable deletion of a row somebody may have meant to create. Absent
/// attribution refuses rather than assumes, the same fail-closed reading
/// `docs/api/conventions.md` §4.6 gives a fact recorded before anyone was
/// written down: it names no declarer, and reading that silence as "this
/// caller's own" would hand every pre-existing account to whoever asked
/// first.
async fn may_retract(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<(), AppError> {
    if principal.scope.may_administer() {
        return Ok(());
    }
    let declared_by = services
        .store
        .list_account_details(principal.owner)
        .await?
        .into_iter()
        .find(|held| held.id == account)
        .and_then(|held| held.declared_by);
    if declared_by == Some(PrincipalId(principal.token_id)) {
        Ok(())
    } else {
        Err(AppError::Invalid {
            field: "account".to_owned(),
            expected: "an account this credential declared itself, or the owner's own \
                       credential"
                .to_owned(),
            actual: "an account declared under a different credential, or recorded before \
                     attribution existed, which reads as no declarer rather than as this \
                     caller's own"
                .to_owned(),
        })
    }
}

/// The core's refusal in the transport's vocabulary.
///
/// Both are `Conflict`, for [`crate::scenarios::retirement::refusal`]'s
/// reason: nothing about the request was wrong, the same body would have been
/// accepted a moment earlier or after the opposite call.
fn refusal(refused: RetractionRefusal) -> AppError {
    match refused {
        RetractionRefusal::AlreadyRetracted => AppError::Conflict {
            what: "this account is already retracted: withdraw that statement before \
                   recording another, so that the change is a revision a reader can see"
                .to_owned(),
        },
        RetractionRefusal::NotRetracted => AppError::Conflict {
            what: "this account is not retracted, so there is nothing to withdraw".to_owned(),
        },
        RetractionRefusal::AccountNotEmpty => AppError::Conflict {
            what: "this account carries a business fact and cannot be retracted: you cannot \
                   un-exist a thing money moved through. If a fact on it should never have \
                   counted, rule it out with a correction (POST /v1/corrections) first; if the \
                   product genuinely existed and then ceased, record its retirement instead \
                   (POST /v1/accounts/{id}/retirement). Once the account carries no business \
                   fact, retract it."
                .to_owned(),
        },
    }
}

/// The retractions that still hold: an account the owner or its declaring agent
/// said should never have existed, **and which still carries no business fact**.
///
/// **A retraction may not hide money, and this is what enforces it.**
/// [`accept_retraction`] refuses an account that carries a fact, so the state
/// this guards against cannot be reached by retracting — it is reached by a
/// fact arriving *afterwards*, on an account the journal is perfectly willing
/// to accept one for. Without this, that fact would be written and then read by
/// nobody: [`crate::actions::frontier`] and
/// [`crate::scenarios::reports::report_population`] both drop a retracted
/// account before anything downstream sees it, so the money would sit in the
/// journal and appear in no report at all. `docs/api/conventions.md` §6.4 says
/// a retirement never hides money; a retraction is a stronger claim than a
/// retirement and must clear the same bar.
///
/// So the filter is a conjunction and not a lookup. A fact on a retracted
/// account brings the account back — into the reports, into the queue, under
/// its own name — and the contradiction is then visible to the owner instead of
/// being resolved silently in favour of the claim that turned out to be wrong.
/// Withdrawing the retraction is how he agrees with what the journal already
/// says; correcting the fact is how he disagrees with it.
#[must_use]
pub fn retractions_that_hold(
    retracted: &[AccountId],
    activity: &[AccountActivityView],
) -> BTreeSet<AccountId> {
    retracted
        .iter()
        .copied()
        .filter(|account| {
            !activity
                .iter()
                .any(|row| row.account == *account && row.has_business_fact)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(account: AccountId, has_business_fact: bool) -> AccountActivityView {
        AccountActivityView {
            account,
            has_business_fact,
            first_effective_date: None,
            last_effective_date: None,
        }
    }

    #[test]
    fn a_retraction_over_an_empty_account_holds() {
        let account = AccountId::new_random();
        assert!(
            retractions_that_hold(&[account], &[activity(account, false)]).contains(&account),
            "an account with no business fact stays retracted"
        );
    }

    #[test]
    fn a_fact_arriving_afterwards_brings_the_account_back() {
        // The state `accept_retraction` cannot produce and the journal can: the
        // retraction was accepted while the account was empty, and a fact
        // landed on it later. The account must be visible again, or the money
        // on it is in the journal and in no report.
        let account = AccountId::new_random();
        assert!(
            !retractions_that_hold(&[account], &[activity(account, true)]).contains(&account),
            "a retraction must not hide a business fact that arrived after it"
        );
    }

    #[test]
    fn an_account_with_no_activity_row_at_all_stays_retracted() {
        // No row is not a row saying `false`: an account nothing was ever
        // recorded against has no activity entry, and that is the ordinary
        // state of the accounts this act exists for.
        let account = AccountId::new_random();
        assert!(
            retractions_that_hold(&[account], &[]).contains(&account),
            "an account the journal has never heard of stays retracted"
        );
    }
}
