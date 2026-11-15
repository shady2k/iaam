//! The fault harness's own guard.
//!
//! A barrier armed after more writes than its transaction makes must fail the
//! run instead of waiting at a point the work never reached: a kill there would
//! prove nothing about a batch, and a green run that never reached its
//! condition is worse than a red one.

use std::panic::AssertUnwindSafe;

use iaam_store::SqliteStore;
use iaam_store::StoreError;
use iaam_store::fault;

#[test]
fn a_barrier_armed_after_more_writes_than_the_work_makes_fails_instead_of_waiting() {
    let marker = std::env::temp_dir().join(format!(
        "iaam-fault-unreached-{}.marker",
        uuid::Uuid::new_v4()
    ));
    fault::arm_barrier(fault::BEFORE_COMMIT, &marker, Some(5));

    let mut store = SqliteStore::open_in_memory().expect("in-memory database");
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        store
            .with_immediate_transaction(
                |error| error,
                |_store| -> Result<(), StoreError> { Ok(()) },
            )
            .expect("never returns: the unreached barrier fails the run");
    }));

    let payload = outcome.expect_err("a run that never reached its barrier must fail");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .expect("the panic carries its message");
    assert!(message.contains("armed after write 5"), "{message}");
    assert!(
        !marker.exists(),
        "no marker is touched at a barrier the work never reached"
    );
}
