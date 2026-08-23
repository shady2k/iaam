//! Правила классификации владельца (§10.4).
//!
//! Правило не удаляется: по нему уже классифицирована история, и
//! объяснять её после удаления будет нечем.

use iaam_core::ids::{ClassificationRuleId, OwnerId};
use iaam_store::SqliteStore;

fn matcher(text: &str) -> String {
    format!(r#"{{"description_contains":"{text}"}}"#)
}

const DIVIDEND: &str = r#"{"kind":"dividend"}"#;
const COUPON: &str = r#"{"kind":"coupon"}"#;

#[test]
fn a_new_rule_is_active_from_the_moment_it_is_stored() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    let stored = store
        .insert_rule(owner, &matcher("Дивиденды"), DIVIDEND)
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
    let stored = store.insert_rule(owner, &matcher("Купон"), COUPON).unwrap();

    store.retire_rule(owner, stored.id).unwrap();

    assert_eq!(store.list_active_rules(owner).unwrap(), vec![]);
    let history = store.rule_history(owner).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, stored.id);
    assert!(
        history[0].retired_at.is_some(),
        "выведенное из обращения правило помнит, когда это случилось"
    );
}

#[test]
fn an_amendment_adds_a_version_and_retires_the_one_it_replaces() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let first = store
        .insert_rule(owner, &matcher("Дивиденды"), DIVIDEND)
        .unwrap();

    let second = store
        .amend_rule(owner, first.id, &matcher("Дивиденды за"), DIVIDEND)
        .unwrap();

    assert_eq!(second.version, 2);
    assert_eq!(second.replaces, Some(first.id));
    assert_eq!(
        store.list_active_rules(owner).unwrap(),
        vec![second.clone()],
        "действует только новая версия"
    );
    let history = store.rule_history(owner).unwrap();
    assert_eq!(history.len(), 2, "прежняя версия не затёрта: {history:?}");
    assert_eq!(history[0].id, first.id);
    assert_eq!(
        history[0].matcher,
        matcher("Дивиденды"),
        "прежний матчер остался прежним"
    );
    assert!(history[0].retired_at.is_some());
}

#[test]
fn a_rule_is_retired_once_and_the_date_is_not_rewritten() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stored = store.insert_rule(owner, &matcher("Купон"), COUPON).unwrap();
    store.retire_rule(owner, stored.id).unwrap();
    let retired_at = store.rule_history(owner).unwrap()[0].retired_at.clone();

    assert!(
        store.retire_rule(owner, stored.id).is_err(),
        "повторный вывод из обращения — отказ, а не тихое обновление даты"
    );
    assert_eq!(store.rule_history(owner).unwrap()[0].retired_at, retired_at);
}

#[test]
fn versions_are_numbered_within_the_owner() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let busy = OwnerId::new_random();
    for _ in 0..3 {
        store.insert_rule(busy, &matcher("Купон"), COUPON).unwrap();
    }

    let newcomer = OwnerId::new_random();
    let first = store
        .insert_rule(newcomer, &matcher("Купон"), COUPON)
        .unwrap();
    assert_eq!(first.version, 1, "номер решения не течёт между владельцами");
}

#[test]
fn another_owners_rules_are_neither_active_nor_in_our_history() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let theirs = OwnerId::new_random();
    store
        .insert_rule(theirs, &matcher("Дивиденды"), DIVIDEND)
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
        .insert_rule(theirs, &matcher("Дивиденды"), DIVIDEND)
        .unwrap();

    let stranger = OwnerId::new_random();
    assert!(store.retire_rule(stranger, rule.id).is_err());
    assert!(
        store
            .amend_rule(stranger, rule.id, &matcher("Что угодно"), COUPON)
            .is_err()
    );
    assert_eq!(
        store.list_active_rules(theirs).unwrap().len(),
        1,
        "чужое правило осталось действующим и неизменённым"
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
                &matcher("Купон"),
                COUPON,
            )
            .is_err()
    );
}

#[test]
fn a_matcher_or_outcome_that_is_not_json_is_refused() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();

    // Правило, которое не сможет прочитать классификатор, не должно
    // ложиться в базу молча: чинить его будет уже нечем.
    assert!(store.insert_rule(owner, "не json", DIVIDEND).is_err());
    assert!(
        store
            .insert_rule(owner, &matcher("Купон"), "тоже не json")
            .is_err()
    );
    assert_eq!(store.rule_history(owner).unwrap(), vec![]);
}
