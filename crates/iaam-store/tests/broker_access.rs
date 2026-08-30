//! Broker access in the store (§14).

use std::fs;
use std::path::PathBuf;

use iaam_broker::credentials::{BrokerScope, Key, SealedToken, seal};
use iaam_broker::environment::Environment;
use iaam_core::ids::OwnerId;
use iaam_store::SqliteStore;
use iaam_store::broker_access::{BrokerAccess, BrokerAccessCiphertext, NewBrokerAccess, SoleOwner};
use iaam_store::documents::BrokerCode;
use uuid::Uuid;

const SECRET: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";

/// Directory for the file-based database. The file is needed literally: we check that
/// the token is not present in the bytes on disk, whereas an in-memory database cannot be inspected on disk.
struct TempDatabase {
    path: PathBuf,
}

impl TempDatabase {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("iaam-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("database directory");
        Self { path }
    }

    fn file(&self) -> PathBuf {
        self.path.join("iaam.sqlite3")
    }

    /// All database files, including the WAL: a secret that did not make it
    /// into the main file but settled in the log has leaked in exactly the same way.
    fn bytes(&self) -> Vec<u8> {
        let mut all = Vec::new();
        for entry in fs::read_dir(&self.path).expect("reading database directory") {
            let entry = entry.expect("database file");
            all.extend(fs::read(entry.path()).expect("database file bytes"));
        }
        all
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn access(owner: OwnerId, broker: &str, environment: Environment, key: &Key) -> NewBrokerAccess {
    let sealed = seal(key, SECRET);
    NewBrokerAccess {
        id: Uuid::new_v4(),
        owner,
        broker: BrokerCode::parse(broker).unwrap(),
        environment: environment.code().to_owned(),
        scope: BrokerScope::ReadOnly.code().to_owned(),
        nonce: sealed.nonce().to_vec(),
        ciphertext: sealed.ciphertext().to_vec(),
    }
}

#[test]
fn a_leaked_database_file_does_not_leak_the_token() {
    // §14 literally: leaking the database file must not provide access
    // to the broker account. We check not that “we called encryption,”
    // but the absence of the substring in the bytes on disk—the only form
    // of this check that cannot pass accidentally.
    let directory = TempDatabase::create("leak");
    let key = Key::from_bytes([9; 32]);
    let owner = OwnerId::new_random();
    let entry = access(owner, "tinkoff", Environment::Prod, &key);
    let ciphertext = entry.ciphertext.clone();
    {
        let mut store = SqliteStore::open(&directory.file()).unwrap();
        store.insert_broker_access(&entry).unwrap();
    }

    let bytes = directory.bytes();
    // First, verify that the test is actually looking at the written
    // data: a test searching for a secret in an empty file always passes.
    assert!(
        bytes
            .windows(ciphertext.len())
            .any(|window| window == ciphertext.as_slice()),
        "no ciphertext in the database file: the test is looking in the wrong place"
    );
    assert!(
        !bytes
            .windows(SECRET.len())
            .any(|window| window == SECRET.as_bytes()),
        "token found in database file"
    );
}

#[test]
fn an_archive_bundle_carries_no_broker_access() {
    // A portable archive with live access to the broker account is
    // a way to take access out of the system along with the archive.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([8; 32]);
    let owner = OwnerId::new_random();
    let entry = access(owner, "tinkoff", Environment::Prod, &key);
    let ciphertext = entry.ciphertext.clone();
    store.insert_broker_access(&entry).unwrap();

    let bundle = store.export_bundle(owner).unwrap();
    let serialised = serde_json::to_vec(&bundle).unwrap();

    assert!(
        !serialised
            .windows(ciphertext.len())
            .any(|window| window == ciphertext.as_slice()),
        "access ciphertext was included in the archive"
    );
}

#[test]
fn a_stored_access_opens_back_with_the_key() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([7; 32]);
    let owner = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    store
        .insert_broker_access(&access(owner, "tinkoff", Environment::Prod, &key))
        .unwrap();

    let found = store
        .find_broker_access(owner, &broker, Environment::Prod.code())
        .unwrap()
        .unwrap();
    assert_eq!(
        BrokerScope::parse(&found.scope),
        Some(BrokerScope::ReadOnly)
    );
    let (nonce, ciphertext) = found.sealed_parts();
    let sealed = SealedToken::of(nonce.to_vec(), ciphertext.to_vec());
    let opened = iaam_broker::credentials::open(&key, &sealed).unwrap();
    assert_eq!(opened.expose(), SECRET);
}

#[test]
fn broker_access_of_another_owner_is_not_found() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([6; 32]);
    let theirs = OwnerId::new_random();
    store
        .insert_broker_access(&access(theirs, "tinkoff", Environment::Prod, &key))
        .unwrap();

    let stranger = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    assert_eq!(
        store
            .find_broker_access(stranger, &broker, Environment::Prod.code())
            .unwrap(),
        None
    );
    assert_eq!(store.broker_access_history(stranger).unwrap(), vec![]);
}

#[test]
fn a_revoked_access_is_not_found_but_stays_in_history() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([5; 32]);
    let owner = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    let entry = access(owner, "tinkoff", Environment::Prod, &key);
    store.insert_broker_access(&entry).unwrap();

    store.revoke_broker_access(owner, entry.id).unwrap();

    assert_eq!(
        store
            .find_broker_access(owner, &broker, Environment::Prod.code())
            .unwrap(),
        None
    );
    let history = store.broker_access_history(owner).unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].revoked_at.is_some());
}

#[test]
fn a_second_active_access_for_one_broker_is_refused() {
    // Two active credentials in one environment mean it is unclear
    // which one the system uses to access the broker.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([4; 32]);
    let owner = OwnerId::new_random();
    store
        .insert_broker_access(&access(owner, "tinkoff", Environment::Prod, &key))
        .unwrap();

    assert!(
        store
            .insert_broker_access(&access(owner, "tinkoff", Environment::Prod, &key))
            .is_err(),
        "a second active credential for the same broker was registered"
    );
}

#[test]
fn a_revoked_access_makes_room_for_a_new_one() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([3; 32]);
    let owner = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    let first = access(owner, "tinkoff", Environment::Prod, &key);
    store.insert_broker_access(&first).unwrap();
    store.revoke_broker_access(owner, first.id).unwrap();

    let second = access(owner, "tinkoff", Environment::Prod, &key);
    store.insert_broker_access(&second).unwrap();

    let found: BrokerAccess = store
        .find_broker_access(owner, &broker, Environment::Prod.code())
        .unwrap()
        .unwrap();
    assert_eq!(found.id, second.id);
}

#[test]
fn the_two_environments_of_one_broker_live_side_by_side() {
    // Tokens differ between environments: a production token is not accepted by the sandbox,
    // and a sandbox token is not accepted in production. Therefore both credentials must exist
    // simultaneously; otherwise the live check and the production channel rule
    // each other out.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([2; 32]);
    let owner = OwnerId::new_random();
    let prod = access(owner, "tinkoff", Environment::Prod, &key);
    let sandbox = access(owner, "tinkoff", Environment::Sandbox, &key);

    store.insert_broker_access(&prod).unwrap();
    store.insert_broker_access(&sandbox).unwrap();

    let broker = BrokerCode::parse("tinkoff").unwrap();
    let found_prod = store
        .find_broker_access(owner, &broker, Environment::Prod.code())
        .unwrap()
        .unwrap();
    let found_sandbox = store
        .find_broker_access(owner, &broker, Environment::Sandbox.code())
        .unwrap()
        .unwrap();
    assert_eq!(found_prod.id, prod.id);
    assert_eq!(found_sandbox.id, sandbox.id);
    assert_eq!(found_prod.environment, "prod");
    assert_eq!(found_sandbox.environment, "sandbox");
}

#[test]
fn a_mid_rotation_failure_leaves_every_ciphertext_under_the_old_key() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let old_key = Key::from_bytes([21; 32]);
    let new_key = Key::from_bytes([22; 32]);
    let owner = OwnerId::new_random();
    let prod = access(owner, "tinkoff", Environment::Prod, &old_key);
    let sandbox = access(owner, "tinkoff", Environment::Sandbox, &old_key);
    store.insert_broker_access(&prod).unwrap();
    store.insert_broker_access(&sandbox).unwrap();
    store.revoke_broker_access(owner, sandbox.id).unwrap();

    let replacement = seal(&new_key, SECRET);
    let missing = seal(&new_key, SECRET);
    let result = store.rotate_broker_access_ciphertexts(&[
        BrokerAccessCiphertext {
            id: prod.id,
            nonce: replacement.nonce().to_vec(),
            ciphertext: replacement.ciphertext().to_vec(),
        },
        BrokerAccessCiphertext {
            id: Uuid::new_v4(),
            nonce: missing.nonce().to_vec(),
            ciphertext: missing.ciphertext().to_vec(),
        },
    ]);

    assert!(result.is_err());
    for entry in store.broker_access_history(owner).unwrap() {
        let sealed = SealedToken::of(entry.nonce.clone(), entry.ciphertext.clone());
        assert_eq!(
            iaam_broker::credentials::open(&old_key, &sealed)
                .unwrap()
                .expose(),
            SECRET
        );
        assert!(iaam_broker::credentials::open(&new_key, &sealed).is_err());
    }
}

#[test]
fn an_access_of_one_environment_is_not_found_in_the_other() {
    // Otherwise, the live check could silently access the sandbox with a production
    // token and receive an error that would not reveal the environment
    // it was using.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([1; 32]);
    let owner = OwnerId::new_random();
    store
        .insert_broker_access(&access(owner, "tinkoff", Environment::Prod, &key))
        .unwrap();

    let broker = BrokerCode::parse("tinkoff").unwrap();
    assert_eq!(
        store
            .find_broker_access(owner, &broker, Environment::Sandbox.code())
            .unwrap(),
        None
    );
}

#[test]
fn a_revoked_access_makes_room_only_in_its_own_environment() {
    // Revoking the sandbox credential must not free the production slot:
    // these are different records with different tokens.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([10; 32]);
    let owner = OwnerId::new_random();
    let prod = access(owner, "tinkoff", Environment::Prod, &key);
    let sandbox = access(owner, "tinkoff", Environment::Sandbox, &key);
    store.insert_broker_access(&prod).unwrap();
    store.insert_broker_access(&sandbox).unwrap();

    store.revoke_broker_access(owner, sandbox.id).unwrap();

    let broker = BrokerCode::parse("tinkoff").unwrap();
    assert!(
        store
            .find_broker_access(owner, &broker, Environment::Prod.code())
            .unwrap()
            .is_some(),
        "revoking the sandbox credential affected the production one"
    );
    assert!(
        store
            .insert_broker_access(&access(owner, "tinkoff", Environment::Prod, &key))
            .is_err(),
        "the production slot was freed by an unrelated revocation"
    );
}

// --- who counts as the owner when no owner was specified ---

#[test]
fn without_a_single_token_there_is_no_owner_to_assume() {
    let store = SqliteStore::open_in_memory().unwrap();
    assert_eq!(store.sole_token_owner().unwrap(), SoleOwner::None);
}

#[test]
fn one_owner_is_assumed_when_only_one_exists() {
    // The owner is not printed when the token is issued, and has no way to know their
    // identifier: the sole owner must be discoverable.
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    issue(&store, owner, "laptop");
    issue(&store, owner, "phone");

    assert_eq!(store.sole_token_owner().unwrap(), SoleOwner::Single(owner));
}

#[test]
fn several_owners_are_never_guessed_between() {
    // Choosing the owner on the user's behalf means registering a broker credential
    // not the right one — and detect it through someone else's trades in the portfolio.
    let store = SqliteStore::open_in_memory().unwrap();
    issue(&store, OwnerId::new_random(), "first");
    issue(&store, OwnerId::new_random(), "second");

    assert_eq!(store.sole_token_owner().unwrap(), SoleOwner::Several);
}

fn issue(store: &SqliteStore, owner: OwnerId, label: &str) {
    store
        .insert_token(
            &iaam_store::tokens::TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: label.to_owned(),
                scope: iaam_store::tokens::TokenScope::Owner,
                revoked: false,
            },
            &format!("hash-{label}-{}", owner.inner()),
        )
        .unwrap();
}
