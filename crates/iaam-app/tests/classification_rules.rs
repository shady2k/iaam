//! What a refused classification rule leaves behind (iaam-y6kt).
//!
//! `POST /v1/classification-rules` writes the rule and then recomputes the
//! history under the owner's whole active rule set. The bug this file was
//! filed about (iaam-y6kt) was an ordering one: the recomputation read every
//! stored rule with the classifier's own reader, and a rule it could not read
//! was an `AppError::Invalid` — a 422 at the transport — but that check ran
//! *after* the write. The caller was told its rule was refused by a store that
//! already held it, and the only reasonable answer to a refusal, sending it
//! again, added a second copy.
//!
//! **That regression can no longer be reconstructed, and this file no longer
//! tries to.** `ClassificationRuleStore::create_rule` takes `RuleMatcher` and
//! `Classification` (spec §4.6) rather than the JSON the store used to keep:
//! `Classification` is a closed six-variant enum, so there is no longer a
//! spelling of "a rule the classifier cannot read" to hand the port at all —
//! not a malformed one, not an unrecognised outcome word. The bug class the
//! ordering fix defended against is closed by the type system, one layer
//! before the ordering ever mattered. What remains here is the happy path,
//! kept so a future change to the write-then-recompute sequence still has a
//! test watching it.

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{Clock, Principal, Scope};
use iaam_app::scenarios::classification::{create_rule, list_rules};
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

/// The happy path the ordering fix must not have broken: a readable set still
/// admits a readable rule, is written, and answers with its recomputation
/// plan.
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
