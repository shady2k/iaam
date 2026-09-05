//! The content a profile version names, recorded so it outlives the process
//! (`iaam-mr25`, decision 0019 §5).
//!
//! Nothing here comes from any real profile: the ids, the versions and the
//! digests are invented (CLAUDE.md, "Conventions & Patterns").

use std::path::PathBuf;

use iaam_store::SqliteStore;
use iaam_store::source_profiles::ProfileBinding;
use uuid::Uuid;

/// Sixty-four hexadecimal characters, invented, shaped like SHA-256 because
/// that is what a profile's digest is.
fn digest(marker: char) -> String {
    std::iter::repeat_n(marker, 64).collect()
}

fn a_file() -> PathBuf {
    std::env::temp_dir().join(format!("iaam-profile-ledger-{}.db", Uuid::new_v4()))
}

/// The first content under a pair is recorded, and recorded is what it says.
#[test]
fn a_pair_nothing_stands_under_records_the_content() {
    let store = SqliteStore::open_in_memory().expect("in-memory database");

    let binding = store
        .bind_source_profile_version("example-bank-statement", 1, &digest('a'))
        .expect("the ledger answers");

    assert_eq!(binding, ProfileBinding::Recorded);
}

/// The same content under the same pair is the same binding, restart after
/// restart.
///
/// An instance that refused its own profile on the second start would refuse
/// every import from then on, which is the opposite of what the binding is for.
#[test]
fn the_same_content_binds_again_and_again() {
    let path = a_file();
    let recorded = digest('b');
    {
        let store = SqliteStore::open(&path).expect("file-backed database");
        assert_eq!(
            store
                .bind_source_profile_version("example-bank-statement", 1, &recorded)
                .expect("the ledger answers"),
            ProfileBinding::Recorded
        );
    }
    for _ in 0..2 {
        let store = SqliteStore::open(&path).expect("the same database again");
        assert_eq!(
            store
                .bind_source_profile_version("example-bank-statement", 1, &recorded)
                .expect("the ledger answers"),
            ProfileBinding::Unchanged
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// A second content under a recorded pair is reported as differing, and the
/// answer carries the content the pair already names.
///
/// It is the recorded digest that makes the report actionable: «refused» alone
/// sends the operator to compare files by hand.
#[test]
fn a_second_content_under_a_recorded_pair_is_reported_with_what_was_recorded() {
    let path = a_file();
    let first = digest('c');
    {
        let store = SqliteStore::open(&path).expect("file-backed database");
        store
            .bind_source_profile_version("example-bank-statement", 1, &first)
            .expect("the ledger answers");
    }
    // A restart: the process that recorded the binding is gone, and the file is
    // all that is left of it.
    let store = SqliteStore::open(&path).expect("the same database again");

    let binding = store
        .bind_source_profile_version("example-bank-statement", 1, &digest('d'))
        .expect("the ledger answers");

    assert_eq!(binding, ProfileBinding::Differs { recorded: first });
    let _ = std::fs::remove_file(&path);
}

/// A changed content under a new version is recorded on its own, and the older
/// pair keeps naming what it named.
///
/// Changing a reading is supported; changing it under the version it already
/// used is not. The ledger must not stand in the way of the first.
#[test]
fn a_new_version_records_its_own_content_and_leaves_the_old_one_standing() {
    let path = a_file();
    let first = digest('e');
    let second = digest('f');
    {
        let store = SqliteStore::open(&path).expect("file-backed database");
        store
            .bind_source_profile_version("example-bank-statement", 1, &first)
            .expect("the ledger answers");
    }
    let store = SqliteStore::open(&path).expect("the same database again");

    assert_eq!(
        store
            .bind_source_profile_version("example-bank-statement", 2, &second)
            .expect("the ledger answers"),
        ProfileBinding::Recorded
    );
    assert_eq!(
        store
            .bind_source_profile_version("example-bank-statement", 1, &first)
            .expect("the ledger answers"),
        ProfileBinding::Unchanged
    );
    let _ = std::fs::remove_file(&path);
}

/// Two profiles are two ledger entries: one id's history says nothing about
/// another's.
#[test]
fn two_ids_are_bound_apart() {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    store
        .bind_source_profile_version("example-bank-statement", 1, &digest('1'))
        .expect("the ledger answers");

    let binding = store
        .bind_source_profile_version("northline-card-export", 1, &digest('2'))
        .expect("the ledger answers");

    assert_eq!(binding, ProfileBinding::Recorded);
}
