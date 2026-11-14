//! ADR 0042 point 3: a panic inside the work rolls the transaction back.

use iaam_core::ids::{AccountId, OwnerId};
use iaam_store::reference::AccountRecord;
use iaam_store::{SqliteStore, StoreError};

#[test]
fn a_panic_inside_the_work_rolls_the_transaction_back_and_the_store_stays_usable() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory database");
    let owner = OwnerId::new_random();
    let account = AccountRecord {
        id: AccountId::new_random(),
        owner,
        title: "Main".to_owned(),
        institution: None,
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.with_immediate_transaction(
            |error| error,
            |store| -> Result<(), StoreError> {
                store.upsert_account(&account)?;
                panic!("interrupted after the write");
            },
        )
    }));
    assert!(
        outcome.is_err(),
        "the panic must unwind past the transaction"
    );

    assert!(
        store.list_accounts(owner).expect("read back").is_empty(),
        "the write before the panic must be rolled back"
    );
    store.upsert_account(&account).expect("the store is usable");
    assert_eq!(
        store.list_accounts(owner).expect("read back").len(),
        1,
        "what is written after the panic is written"
    );
}
