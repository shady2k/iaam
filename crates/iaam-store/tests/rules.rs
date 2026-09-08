//! Owner classification rules (§10.4, spec §4.6).
//!
//! The rule is never deleted: history has already been classified by it, and
//! after deletion there would be nothing to explain it.

use iaam_core::ids::{ClassificationRuleId, OwnerId};
use iaam_store::SqliteStore;
use iaam_store::rules::NewRule;

/// A rule matching on the description alone, classified as income of the
/// stated kind.
fn matching_description(text: &str, income_kind: &str) -> NewRule {
    NewRule {
        counterparty_account: None,
        description_contains: Some(text.to_owned()),
        source_kind: None,
        source_category: None,
        owner_category: None,
        source_code: None,
        movement: None,
        outcome_kind: "income".to_owned(),
        to_account: None,
        fee_origin: None,
        income_kind: Some(income_kind.to_owned()),
    }
}

#[test]
fn a_classification_rule_asking_nothing_is_refused_by_the_database() {
    // Plan Task 5, Step 1: the `asks_nothing` `CHECK` (spec §4.6) must reject a
    // rule with no condition at the database, not only in `RuleMatcher`'s own
    // `asks_nothing` method — the two are independent enforcements of the same
    // rule, and the database is the one this store cannot bypass.
    let store = SqliteStore::open_in_memory().expect("open");
    let outcome = store.connection().execute(
        "INSERT INTO classification_rules
             (id, owner, version, outcome_kind, created_at)
         VALUES ('r1', 'o1', 1, 'external_flow', '2026-01-01T00:00:00Z')",
        [],
    );
    assert!(
        outcome.is_err(),
        "a matcher with no condition must not be storable"
    );
}

#[test]
fn a_new_rule_is_active_from_the_moment_it_is_stored() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    let stored = store
        .insert_rule(owner, matching_description("Dividends", "dividend"))
        .unwrap();
    assert_eq!(stored.owner, owner);
    assert_eq!(stored.version, 1);
    assert_eq!(stored.retired_at, None);
    assert_eq!(stored.replaces, None);

    assert_eq!(store.list_active_rules(owner).unwrap(), vec![stored]);
}

#[test]
fn a_retired_rule_leaves_the_active_set_but_stays_in_history() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stored = store
        .insert_rule(owner, matching_description("Coupon", "coupon"))
        .unwrap();

    store.retire_rule(owner, stored.id).unwrap();

    assert_eq!(store.list_active_rules(owner).unwrap(), vec![]);
    let history = store.rule_history(owner).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, stored.id);
    assert!(
        history[0].retired_at.is_some(),
        "a retired rule remembers when it was retired"
    );
}

#[test]
fn an_amendment_adds_a_version_and_retires_the_one_it_replaces() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let first = store
        .insert_rule(owner, matching_description("Dividends", "dividend"))
        .unwrap();

    let second = store
        .amend_rule(
            owner,
            first.id,
            matching_description("Dividends for", "dividend"),
        )
        .unwrap();

    assert_eq!(second.version, 2);
    assert_eq!(second.replaces, Some(first.id));
    assert_eq!(
        store.list_active_rules(owner).unwrap(),
        vec![second.clone()],
        "only the new version is active"
    );
    let history = store.rule_history(owner).unwrap();
    assert_eq!(
        history.len(),
        2,
        "the previous version was not overwritten: {history:?}"
    );
    assert_eq!(history[0].id, first.id);
    assert_eq!(
        history[0].description_contains.as_deref(),
        Some("Dividends"),
        "the previous matcher remains unchanged"
    );
    assert!(history[0].retired_at.is_some());
}

#[test]
fn a_rule_is_retired_once_and_the_date_is_not_rewritten() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stored = store
        .insert_rule(owner, matching_description("Coupon", "coupon"))
        .unwrap();
    store.retire_rule(owner, stored.id).unwrap();
    let retired_at = store.rule_history(owner).unwrap()[0].retired_at.clone();

    assert!(
        store.retire_rule(owner, stored.id).is_err(),
        "repeated retirement is rejected, not a silent date update"
    );
    assert_eq!(store.rule_history(owner).unwrap()[0].retired_at, retired_at);
}

#[test]
fn versions_are_numbered_within_the_owner() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let busy = OwnerId::new_random();
    for _ in 0..3 {
        store
            .insert_rule(busy, matching_description("Coupon", "coupon"))
            .unwrap();
    }

    let newcomer = OwnerId::new_random();
    let first = store
        .insert_rule(newcomer, matching_description("Coupon", "coupon"))
        .unwrap();
    assert_eq!(
        first.version, 1,
        "the decision number does not leak between owners"
    );
}

#[test]
fn another_owners_rules_are_neither_active_nor_in_our_history() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let theirs = OwnerId::new_random();
    store
        .insert_rule(theirs, matching_description("Dividends", "dividend"))
        .unwrap();

    let stranger = OwnerId::new_random();
    assert_eq!(store.list_active_rules(stranger).unwrap(), vec![]);
    assert_eq!(store.rule_history(stranger).unwrap(), vec![]);
}

#[test]
fn another_owners_rule_is_neither_amended_nor_retired() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let theirs = OwnerId::new_random();
    let rule = store
        .insert_rule(theirs, matching_description("Dividends", "dividend"))
        .unwrap();

    let stranger = OwnerId::new_random();
    assert!(store.retire_rule(stranger, rule.id).is_err());
    assert!(
        store
            .amend_rule(
                stranger,
                rule.id,
                matching_description("Anything", "coupon")
            )
            .is_err()
    );
    assert_eq!(
        store.list_active_rules(theirs).unwrap().len(),
        1,
        "foreign rule remained active and unchanged"
    );
    assert_eq!(store.list_active_rules(theirs).unwrap()[0], rule);
}

#[test]
fn amending_a_rule_that_does_not_exist_is_refused() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    assert!(
        store
            .amend_rule(
                owner,
                ClassificationRuleId::new_random(),
                matching_description("Coupon", "coupon"),
            )
            .is_err()
    );
}

#[test]
fn an_outcome_kind_out_of_the_closed_vocabulary_is_refused() {
    // The database's `CHECK` is the enforcement now, not JSON syntax: the
    // vocabulary is closed to the six outcomes spec §4.6 fixes, and nothing
    // else can be written under any of them.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    let mut rule = matching_description("Coupon", "coupon");
    rule.outcome_kind = "reimbursement".to_owned();

    assert!(
        store.insert_rule(owner, rule).is_err(),
        "an outcome kind outside the six the schema allows must be refused"
    );
    assert_eq!(store.rule_history(owner).unwrap(), vec![]);
}
