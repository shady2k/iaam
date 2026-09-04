//! The owner declares that one of his products ceased to exist (`iaam-gua5`).
//!
//! **The second axis.** A contour says which accounts a calculation folds over;
//! a retirement says which products still exist. The case that needed both at
//! once is a term deposit that was closed and its balance returned to another
//! account of the owner's: the interest it paid must go on counting as an
//! earning, the movement that emptied it must go on being internal, and the
//! account must stop appearing in asset reports as a zero-balance shell. Only
//! the first two survive if the account is dropped from a later contour
//! version, and none of the three needs classification to change — which is why
//! nothing here touches [`iaam_core::contour::classify`], now or ever.
//!
//! What this module owns is the act: read what the owner has already said, ask
//! the core whether the new statement may stand, and write one revision. The
//! rule itself is [`iaam_core::retirement`], where it can be tested without a
//! database and without a clock.

use iaam_core::ids::AccountId;
use iaam_core::retirement::{
    AccountRetirement, RetirementRefusal, RetirementRevision, accept_retirement, accept_withdrawal,
};
use time::Date;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::Principal;

/// What the owner's declarations stand at after the call, and what the account
/// now says.
///
/// The revision travels back with the statement because the caller publishes
/// both: a client that had to read the revision again would be reading a state
/// a further call may already have moved past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRetirementOutcome {
    pub account: AccountId,
    /// The date the product ceased, or `None` where the call withdrew the
    /// statement.
    pub effective_on: Option<Date>,
    pub revision: RetirementRevision,
}

/// Record that one product ceased to exist on a date.
///
/// The three refusals and why each is a refusal are
/// [`iaam_core::retirement::RetirementRefusal`]; the fourth thing a reader
/// expects to see refused — retiring an account that still holds money — is
/// deliberately allowed, and [`accept_retirement`] carries that argument.
///
/// The account is **not** checked for existence here. The transport has already
/// resolved it against the owner's directory in order to print its title beside
/// the answer, and the store's foreign key refuses what that misses; a third
/// check would be a third answer to one question.
pub async fn record_account_retirement(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
    effective_on: Date,
) -> Result<AccountRetirementOutcome, AppError> {
    let held = services
        .store
        .list_account_retirements(principal.owner)
        .await?;
    accept_retirement(
        held.effective_on(account),
        effective_on,
        services.clock.today(),
    )
    .map_err(refusal)?;
    let revision = services
        .store
        .record_account_retirement(
            principal.owner,
            AccountRetirement {
                account,
                effective_on,
            },
        )
        .await?;
    Ok(AccountRetirementOutcome {
        account,
        effective_on: Some(effective_on),
        revision,
    })
}

/// Withdraw the statement, returning the account to «he has not said».
///
/// Not an erasure: the withdrawal is itself a revision, so a snapshot published
/// while the retirement stood still names the state it was computed under.
pub async fn withdraw_account_retirement(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<AccountRetirementOutcome, AppError> {
    let held = services
        .store
        .list_account_retirements(principal.owner)
        .await?;
    accept_withdrawal(held.effective_on(account)).map_err(refusal)?;
    let revision = services
        .store
        .withdraw_account_retirement(principal.owner, account)
        .await?;
    Ok(AccountRetirementOutcome {
        account,
        effective_on: None,
        revision,
    })
}

/// What the owner has said about one account, for a caller that is reading
/// rather than writing.
pub async fn account_retirement(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
) -> Result<AccountRetirementOutcome, AppError> {
    let held = services
        .store
        .list_account_retirements(principal.owner)
        .await?;
    Ok(AccountRetirementOutcome {
        account,
        effective_on: held.effective_on(account),
        revision: held.revision,
    })
}

/// The core's refusal in the transport's vocabulary.
///
/// The two conflicts are `Conflict` and not `Invalid`, because nothing about
/// the request was wrong: the same body would have been accepted a moment
/// earlier, or after the opposite call. The future date is `Invalid`, because
/// the field is.
fn refusal(refused: RetirementRefusal) -> AppError {
    match refused {
        RetirementRefusal::AlreadyRetired { effective_on } => AppError::Conflict {
            what: format!(
                "this account is already retired, effective {effective_on}: withdraw that \
                 statement before recording another, so that the change is a revision a \
                 reader can see"
            ),
        },
        RetirementRefusal::NotRetired => AppError::Conflict {
            what: "this account is not retired, so there is nothing to withdraw".to_owned(),
        },
        RetirementRefusal::NotYetCeased {
            effective_on,
            today,
        } => AppError::Invalid {
            field: "effective_on".into(),
            expected: format!("a date no later than {today}"),
            actual: effective_on.to_string(),
        },
    }
}
