//! Owner category reference data.
//!
//! Categories are never deleted: history has already been decomposed by them,
//! and after deletion there would be nothing to explain it.

use iaam_core::ids::OwnerId;
use iaam_store::SqliteStore;

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
