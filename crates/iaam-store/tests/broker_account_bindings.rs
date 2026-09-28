//! The binding between one of the owner's accounts and the broker's own
//! account number for it (`iaam-xzz5.3.2`).
//!
//! Every number here is invented: the numbers are the owner's data and live
//! in his database, never in the repository.

use iaam_core::ids::{AccountId, OwnerId};
use iaam_store::documents::BrokerCode;
use iaam_store::reference::AccountRecord;
use iaam_store::{SqliteStore, StoreError};

const BROKER: &str = "tinkoff";

fn broker() -> BrokerCode {
    BrokerCode::parse(BROKER).expect("broker code")
}

fn seed_account(store: &SqliteStore, owner: OwnerId) -> AccountId {
    let account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Main".into(),
            institution: None,
        })
        .expect("seed account");
    account
}

#[test]
fn a_binding_recorded_reads_back_its_number() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = seed_account(&store, owner);

    store
        .record_broker_account_binding(owner, account, &broker(), "invented-one")
        .unwrap();

    assert_eq!(
        store
            .broker_account_binding(owner, account, &broker())
            .unwrap(),
        Some("invented-one".to_owned()),
    );
}

#[test]
fn an_account_bound_at_one_broker_is_unbound_at_another() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = seed_account(&store, owner);
    let other_broker = BrokerCode::parse("finam").expect("broker code");

    store
        .record_broker_account_binding(owner, account, &broker(), "invented-one")
        .unwrap();

    assert_eq!(
        store
            .broker_account_binding(owner, account, &other_broker)
            .unwrap(),
        None,
        "the broker is part of the key: one broker's number never answers for another's",
    );
}

#[test]
fn one_broker_account_is_bound_to_at_most_one_iaam_account() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let first = seed_account(&store, owner);
    let second = seed_account(&store, owner);

    store
        .record_broker_account_binding(owner, first, &broker(), "invented-one")
        .unwrap();
    let refused = store
        .record_broker_account_binding(owner, second, &broker(), "invented-one")
        .expect_err("the number is taken");

    let StoreError::BrokerAccountBindingHeld { account } = refused else {
        panic!("expected a held-binding refusal, got {refused}");
    };
    assert_eq!(
        account,
        first.inner().to_string(),
        "the refusal names the account that already holds the number"
    );
}

#[test]
fn rebinding_replaces_the_number_and_frees_the_old_one() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let first = seed_account(&store, owner);
    let second = seed_account(&store, owner);

    store
        .record_broker_account_binding(owner, first, &broker(), "invented-one")
        .unwrap();
    store
        .record_broker_account_binding(owner, first, &broker(), "invented-two")
        .unwrap();
    assert_eq!(
        store
            .broker_account_binding(owner, first, &broker())
            .unwrap(),
        Some("invented-two".to_owned()),
        "the owner's correction is the word that stands",
    );

    store
        .record_broker_account_binding(owner, second, &broker(), "invented-one")
        .expect("the freed number binds again");
}

#[test]
fn a_binding_must_name_an_account_this_owner_holds() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let foreign_account = seed_account(&store, stranger);

    let refused = store
        .record_broker_account_binding(owner, foreign_account, &broker(), "invented-one")
        .expect_err("another owner's account is not a binding subject");

    assert!(
        matches!(
            refused,
            StoreError::NotFound {
                what: "account",
                ..
            }
        ),
        "expected a not-found refusal, got {refused}"
    );
}

#[test]
fn one_owners_binding_is_another_owners_silence() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let account = seed_account(&store, owner);

    store
        .record_broker_account_binding(owner, account, &broker(), "invented-one")
        .unwrap();

    assert_eq!(
        store
            .broker_account_binding(stranger, account, &broker())
            .unwrap(),
        None,
    );
}
