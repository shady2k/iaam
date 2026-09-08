//! What a refused classification rule leaves behind (iaam-y6kt).
//!
//! `POST /v1/classification-rules` writes the rule and then recomputes the
//! history under the owner's whole active rule set. The recomputation reads
//! every stored rule with the classifier's own reader, and a rule it cannot read
//! is an `AppError::Invalid` — a 422 at the transport. Run in that order the
//! caller was told its rule was refused by a store that already held it, and the
//! only reasonable answer to a refusal, sending it again, added a second copy.
//!
//! Every test here is about that ordering and not about the vocabulary: what
//! makes a stored rule unreadable is covered where the reader lives.

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{Clock, Principal, Scope};
use iaam_app::scenarios::classification::{create_rule, list_rules, retire_rule};
use iaam_core::ids::OwnerId;
use iaam_ingest::classification::{Classification, RuleMatcher};
use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;
use uuid::Uuid;

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> Date {
        date!(2026 - 03 - 31)
    }
}

struct Ctx {
    services: AppServices,
    principal: Principal,
}

fn harness() -> Ctx {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}")),
    ));
    let mut services = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        Arc::new(FixedClock),
    );
    // The rule store is one of the optional ports and defaults to the
    // unavailable stub. Every test here is about what the rule store holds, so
    // it is the one port that must be wired.
    services.rules = adapter;
    Ctx {
        services,
        principal: Principal {
            token_id: Uuid::new_v4(),
            owner: OwnerId::new_random(),
            scope: Scope::Owner,
        },
    }
}

/// An attempt to write a rule the classifier cannot read, made the only way
/// one still can be attempted.
///
/// Before the rule tables were typed (spec §4.6), the store kept a matcher and
/// an outcome as opaque text and validated only that it was JSON: it accepted
/// this, and the scenario's own encoder could never produce it — which is how
/// a journal could hold a rule written before the route was typed, or by
/// something other than it, and every later rule creation refused *and wrote*.
///
/// The typed columns close that gap: `outcome_kind` is a database `CHECK`
/// against the same six-word vocabulary the classifier itself accepts, so an
/// unknown word like `"reimbursement"` below is refused at the write, not
/// merely unreadable afterwards. The attempt is kept, and asserted to fail and
/// leave nothing behind, because that is now the guarantee in its place.
async fn attempt_an_unreadable_rule(ctx: &Ctx) -> AppError {
    ctx.services
        .rules
        .create_rule(
            ctx.principal.owner,
            r#"{"counterparty_account":null,"description_contains":null,"kind":null}"#.to_owned(),
            r#"{"kind":"reimbursement"}"#.to_owned(),
            None,
        )
        .await
        .expect_err("an outcome kind outside the classifier's six is refused, not stored")
}

fn proposal() -> (RuleMatcher, Classification) {
    (
        RuleMatcher {
            counterparty_account: None,
            description_contains: Some("Shop One".to_owned()),
            kind: None,
            source_category: None,
            owner_category: None,
            source_code: None,
            movement: None,
        },
        Classification::ExternalFlow,
    )
}

/// The database's `CHECK` refuses the write outright — the caller-facing
/// shape the transport still publishes as 422 — and leaves nothing behind to
/// explain later, unlike the write-then-refuse ordering `iaam-y6kt` was filed
/// about.
#[tokio::test]
async fn an_unreadable_rule_is_refused_and_leaves_nothing_behind() {
    let ctx = harness();
    let before = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("an empty rule history is readable");

    let error = attempt_an_unreadable_rule(&ctx).await;
    assert!(
        matches!(error, AppError::Invalid { .. } | AppError::InvalidField(_)),
        "the refusal is the caller-facing one the transport publishes as 422: {error:?}"
    );

    let after = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is still readable");
    assert_eq!(
        after.len(),
        before.len(),
        "a refused write left something behind: {after:?}"
    );
}

/// Retrying an attempt that was never written cannot accumulate copies of it:
/// there is nothing partial for a repeat to add to.
#[tokio::test]
async fn retrying_an_unreadable_rule_never_accumulates_copies() {
    let ctx = harness();

    for _ in 0..3 {
        attempt_an_unreadable_rule(&ctx).await;
    }

    let rules = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable");
    assert_eq!(rules.len(), 0, "no attempt left a row behind: {rules:?}");
}

/// A refused write must not disturb an unrelated existing rule, and a
/// retirement afterwards must still take effect on it normally: the refused
/// attempt left nothing behind for the retirement to trip over.
#[tokio::test]
async fn a_refused_write_does_not_disturb_an_existing_rule() {
    let ctx = harness();
    let (matcher, outcome) = proposal();
    let created = create_rule(&ctx.services, &ctx.principal, &matcher, outcome, None)
        .await
        .expect("the first rule is written against an empty, readable set");
    attempt_an_unreadable_rule(&ctx).await;

    let rules = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable");
    assert_eq!(
        rules.len(),
        1,
        "the refused write left a row behind: {rules:?}"
    );
    assert!(
        rules[0].retired_at.is_none(),
        "a refused write retired an unrelated rule: {:?}",
        rules[0]
    );

    retire_rule(&ctx.services, &ctx.principal, created.rule.id)
        .await
        .expect("retiring an existing rule does not depend on an attempt that wrote nothing");
    let rules = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable");
    assert!(
        rules[0].retired_at.is_some(),
        "the retirement did not take effect: {:?}",
        rules[0]
    );
}

/// The check refuses what the recomputation would refuse and nothing else.
///
/// A guard that also turned away the ordinary case would fix the ordering by
/// closing the route, which is the failure mode a pre-flight check invites.
#[tokio::test]
async fn an_ordinary_rule_is_still_written_and_still_answers_with_its_plan() {
    let ctx = harness();
    let (matcher, outcome) = proposal();
    let change = create_rule(&ctx.services, &ctx.principal, &matcher, outcome, None)
        .await
        .expect("a readable set admits a readable rule");

    assert!(
        change.plan.corrections.is_empty(),
        "an empty journal has nothing to correct: {:?}",
        change.plan
    );
    assert!(change.plan.preview.accounts.is_empty());
    let rules = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, change.rule.id);
}
