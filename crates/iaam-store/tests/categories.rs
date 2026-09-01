//! Owner category reference data.
//!
//! Categories are never deleted: history has already been decomposed by them,
//! and after deletion there would be nothing to explain it.

use iaam_core::ids::{CategoryRuleId, OwnerId};
use iaam_store::SqliteStore;
use iaam_store::categories::NewCategoryRule;
use time::macros::date;

#[test]
fn a_retired_category_is_still_listed_and_still_flagged() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");
    store.retire_category(owner, food).expect("retired");

    let listed = store.list_categories(owner).expect("listed");
    let row = listed
        .iter()
        .find(|row| row.id == food)
        .expect("still listed");
    // Present and marked, not gone: a past report decomposed spending into this
    // category, and dropping the row would make that report unreadable.
    assert!(row.retired_at.is_some());
}

#[test]
fn categories_are_scoped_to_one_owner_and_one_group() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let other_owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let other_group = store
        .insert_category_group(other_owner, "Usual Expenses")
        .expect("other group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");

    assert_eq!(store.list_categories(owner).unwrap()[0].group_id, group);
    assert!(store.list_categories(other_owner).unwrap().is_empty());
    assert!(
        store
            .insert_category(owner, other_group, "Foreign")
            .is_err(),
        "a category cannot be attached to another owner's group"
    );
    assert_ne!(food, other_group);
}

#[test]
fn duplicate_titles_follow_the_owner_and_group_indexes() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let other_owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    assert!(
        store
            .insert_category_group(owner, "Usual Expenses")
            .is_err(),
        "group titles are unique per owner"
    );
    store
        .insert_category_group(other_owner, "Usual Expenses")
        .expect("other owners may use the same title");

    store
        .insert_category(owner, group, "Food")
        .expect("category");
    assert!(
        store.insert_category(owner, group, "Food").is_err(),
        "category titles are unique within an owner and group"
    );
}

#[test]
fn retirement_is_owner_scoped_and_happens_only_once() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let other_owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");

    assert!(store.retire_category(other_owner, food).is_err());
    assert!(
        store
            .list_categories(owner)
            .unwrap()
            .iter()
            .find(|row| row.id == food)
            .unwrap()
            .retired_at
            .is_none()
    );

    store.retire_category(owner, food).expect("retired");
    let retired_at = store
        .list_categories(owner)
        .unwrap()
        .iter()
        .find(|row| row.id == food)
        .unwrap()
        .retired_at
        .clone();
    assert!(store.retire_category(owner, food).is_err());
    assert_eq!(
        store
            .list_categories(owner)
            .unwrap()
            .iter()
            .find(|row| row.id == food)
            .unwrap()
            .retired_at,
        retired_at
    );
}

#[test]
fn migration_is_a_no_op_after_category_data_exists() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let category = store
        .insert_category(owner, group, "Food")
        .expect("category");
    let before = store.list_categories(owner).expect("before migration");

    iaam_store::schema::migrate(store.connection()).expect("second migration");

    let after = store.list_categories(owner).expect("after migration");
    assert_eq!(after, before);
    assert_eq!(after[0].id, category);
}

#[test]
fn two_rules_cannot_share_a_version_number() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");

    let first = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"SourceCategory":{"value":"Супермаркеты"}}"#.to_owned(),
                category: food,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect("first");
    let second = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"DescriptionContains":{"text":"ЛАВКА"}}"#.to_owned(),
                category: food,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect("second");
    // Without a unique version per owner, two concurrent requests take the same
    // number and the order of rules stops being an order.
    assert_eq!(first.version, 1);
    assert_eq!(second.version, 2);
}

#[test]
fn an_amended_rule_retires_the_old_row_and_points_at_it() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");
    let first = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"SourceCategory":{"value":"Супермаркеты"}}"#.to_owned(),
                category: food,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect("first");

    let second = store
        .amend_category_rule(
            owner,
            first.id,
            NewCategoryRule {
                matcher_json: r#"{"SourceCategory":{"value":"Супермаркет"}}"#.to_owned(),
                category: food,
                valid_from: Some(date!(2026 - 01 - 01)),
                valid_to: None,
            },
        )
        .expect("second");
    assert_eq!(second.replaces, Some(first.id));

    let listed = store.list_category_rules(owner).expect("rules");
    assert_eq!(listed.len(), 2);
    assert!(listed[0].retired_at.is_some());
    assert!(listed[1].retired_at.is_none());
}

#[test]
fn an_amendment_rolls_back_retirement_if_the_new_rule_cannot_be_written() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");
    let first = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"Row":{"key":"row-1"}}"#.to_owned(),
                category: food,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect("first");

    assert!(
        store
            .amend_category_rule(
                owner,
                first.id,
                NewCategoryRule {
                    matcher_json: "{not-json".to_owned(),
                    category: food,
                    valid_from: None,
                    valid_to: None,
                },
            )
            .is_err()
    );

    let listed = store.list_category_rules(owner).expect("rules");
    assert_eq!(listed.len(), 1);
    assert!(listed[0].retired_at.is_none());
}

#[test]
fn category_rule_intervals_and_open_ends_round_trip() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");

    store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"DescriptionContains": {"text": "ЛАВКА"}}"#.to_owned(),
                category: food,
                valid_from: Some(date!(2024 - 01 - 01)),
                valid_to: None,
            },
            None,
        )
        .expect("open-ended rule");
    store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"Row":{"key":"row-2"}}"#.to_owned(),
                category: food,
                valid_from: None,
                valid_to: Some(date!(2025 - 12 - 31)),
            },
            None,
        )
        .expect("open-start rule");

    let listed = store.list_category_rules(owner).expect("rules");
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed[0].matcher_json,
        r#"{"DescriptionContains": {"text": "ЛАВКА"}}"#
    );
    assert_eq!(listed[0].valid_from, Some(date!(2024 - 01 - 01)));
    assert_eq!(listed[0].valid_to, None);
    assert_eq!(listed[1].valid_from, None);
    assert_eq!(listed[1].valid_to, Some(date!(2025 - 12 - 31)));
}

#[test]
fn a_retired_category_rule_is_still_listed_and_flagged() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let food = store
        .insert_category(owner, group, "Food")
        .expect("category");
    let rule = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"Row":{"key":"row-3"}}"#.to_owned(),
                category: food,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect("rule");

    store.retire_category_rule(owner, rule.id).expect("retired");
    let listed = store.list_category_rules(owner).expect("rules");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, CategoryRuleId(rule.id.inner()));
    assert!(listed[0].retired_at.is_some());
}

#[test]
fn retiring_a_group_requires_an_existing_active_owned_group() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let other_owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");

    let error = store
        .retire_category_group(other_owner, group)
        .expect_err("another owner cannot retire the group");
    assert!(matches!(
        error,
        iaam_store::StoreError::NotFound {
            what: "active category group",
            ..
        }
    ));
    store.retire_category_group(owner, group).expect("retired");
    let error = store
        .retire_category_group(owner, group)
        .expect_err("retirement is one-shot");
    assert!(matches!(
        error,
        iaam_store::StoreError::NotFound {
            what: "active category group",
            ..
        }
    ));
}

#[test]
fn a_category_cannot_be_added_to_a_retired_group() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    store.retire_category_group(owner, group).expect("retired");

    let error = store
        .insert_category(owner, group, "Food")
        .expect_err("retired group must refuse new categories");
    assert!(matches!(
        error,
        iaam_store::StoreError::CategoryGroupRetired { id } if id == group.to_string()
    ));
}

#[test]
fn a_rule_must_reference_an_owned_category_and_retirement_is_owner_scoped() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let other_owner = OwnerId::new_random();
    let missing_category = uuid::Uuid::new_v4();
    let error = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"Row":{"key":"row-1"}}"#.to_owned(),
                category: missing_category,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect_err("foreign category must be refused");
    assert!(matches!(
        error,
        iaam_store::StoreError::NotFound { what: "category", id } if id == missing_category.to_string()
    ));

    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let category = store
        .insert_category(owner, group, "Food")
        .expect("category");
    let rule = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"Row":{"key":"row-2"}}"#.to_owned(),
                category,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect("rule");

    let error = store
        .retire_category_rule(other_owner, rule.id)
        .expect_err("another owner cannot retire this rule");
    assert!(matches!(
        error,
        iaam_store::StoreError::NotFound {
            what: "active category rule",
            ..
        }
    ));
    store.retire_category_rule(owner, rule.id).expect("retired");
    let error = store
        .retire_category_rule(owner, rule.id)
        .expect_err("retirement is one-shot");
    assert!(matches!(
        error,
        iaam_store::StoreError::NotFound {
            what: "active category rule",
            ..
        }
    ));
}

#[test]
fn malformed_category_rule_storage_is_rejected_with_the_bad_field() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let group = store
        .insert_category_group(owner, "Usual Expenses")
        .expect("group");
    let category = store
        .insert_category(owner, group, "Food")
        .expect("category");
    store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher_json: r#"{"Row":{"key":"row-3"}}"#.to_owned(),
                category,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .expect("rule");

    store
        .connection()
        .execute(
            "UPDATE category_rules SET valid_from = 'not-a-date'",
            [],
        )
        .expect("corrupt date");
    let error = store
        .list_category_rules(owner)
        .expect_err("invalid date must be rejected");
    assert!(matches!(
        error,
        iaam_store::StoreError::InvalidValue {
            field: "valid_from",
            value
        } if value == "not-a-date"
    ));

    store
        .connection()
        .execute(
            "UPDATE category_rules SET valid_from = NULL, id = 'not-a-uuid'",
            [],
        )
        .expect("corrupt identifier");
    let error = store
        .list_category_rules(owner)
        .expect_err("invalid identifier must be rejected");
    assert!(matches!(
        error,
        iaam_store::StoreError::NotFound {
            what: "category rule",
            id
        } if id == "not-a-uuid"
    ));
}
