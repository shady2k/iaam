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

/// A rule the classifier cannot read, written the only way one can be.
///
/// The store keeps a matcher and an outcome as opaque text on purpose — it must
/// not know the classifier's vocabulary — so it accepts this, and the scenario's
/// own encoder can never produce it. That is exactly the state a journal reaches
/// by holding a rule written before the route was typed, and it is the state in
/// which every later rule creation refused *and wrote*.
async fn store_an_unreadable_rule(ctx: &Ctx) {
    ctx.services
        .rules
        .create_rule(
            ctx.principal.owner,
            r#"{"counterparty_account":null,"description_contains":null,"kind":null}"#.to_owned(),
            r#"{"kind":"reimbursement"}"#.to_owned(),
            None,
        )
        .await
        .expect("the store keeps rule text opaque and writes what it is given");
}

fn proposal() -> (RuleMatcher, Classification) {
    (
        RuleMatcher {
            counterparty_account: None,
            description_contains: Some("Shop One".to_owned()),
            kind: None,
        },
        Classification::ExternalFlow,
    )
}

#[tokio::test]
async fn a_refused_rule_is_not_written() {
    let ctx = harness();
    store_an_unreadable_rule(&ctx).await;
    let before = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable even when a rule in it is not");

    let (matcher, outcome) = proposal();
    let error = create_rule(&ctx.services, &ctx.principal, &matcher, outcome, None)
        .await
        .expect_err("the recomputation cannot read the stored set, so the call is refused");
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
        "a refused rule left something behind: {after:?}"
    );
}

/// The retry is the part that made it a defect rather than an untidiness.
///
/// A caller that is told 422 sends the corrected request again. With the write
/// happening first, each attempt added another copy of a rule the caller
/// believed had never been stored — and a standing rule decides rows nobody has
/// looked at, so the duplicates are not inert.
#[tokio::test]
async fn retrying_a_refused_rule_does_not_accumulate_copies() {
    let ctx = harness();
    store_an_unreadable_rule(&ctx).await;

    for _ in 0..3 {
        let (matcher, outcome) = proposal();
        create_rule(&ctx.services, &ctx.principal, &matcher, outcome, None)
            .await
            .expect_err("every attempt is refused for the same reason");
    }

    let rules = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable");
    assert_eq!(rules.len(), 1, "only the unreadable rule stands: {rules:?}");
}

/// Retirement had the same shape, and its second call was more confusing still:
/// the rule was retired by the refused attempt, so retrying answered `404`, and
/// a caller reading the two responses would conclude it had never retired
/// anything.
#[tokio::test]
async fn a_refused_retirement_leaves_the_rule_active() {
    let ctx = harness();
    let (matcher, outcome) = proposal();
    let created = create_rule(&ctx.services, &ctx.principal, &matcher, outcome, None)
        .await
        .expect("the first rule is written against an empty, readable set");
    store_an_unreadable_rule(&ctx).await;

    retire_rule(&ctx.services, &ctx.principal, created.rule.id)
        .await
        .expect_err("the recomputation cannot read the stored set, so the call is refused");

    let rules = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable");
    let target = rules
        .iter()
        .find(|rule| rule.id == created.rule.id)
        .expect("the rule the retirement named is still in the history");
    assert!(
        target.retired_at.is_none(),
        "a refused retirement retired the rule anyway: {target:?}"
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
        change.plan.is_empty(),
        "an empty journal has nothing to correct: {:?}",
        change.plan
    );
    let rules = list_rules(&ctx.services, &ctx.principal)
        .await
        .expect("the rule history is readable");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, change.rule.id);
}
