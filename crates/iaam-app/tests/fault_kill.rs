//! ADR 0042 point 7: a killed process leaves a store a retry completes.
//!
//! Each child test below is the closure the parent runs in another process:
//! it arms the barrier named by the parent, performs the work, and is killed
//! there. A closure cannot cross a process boundary, so the same binary is
//! re-executed with `--exact <test name>` and the test function is the body.

use std::ffi::OsStr;
use std::path::PathBuf;

use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Recorded, Store};
use iaam_app::storage::SqliteStore;
use iaam_core::dates::{EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_ingest::dedup::IdentityScope;
use iaam_store::fault::{self, KillAt};
use time::macros::date;
use uuid::Uuid;

const OWNER_VARIABLE: &str = "IAAM_FAULT_TEST_OWNER";
const ACCOUNT_VARIABLE: &str = "IAAM_FAULT_TEST_ACCOUNT";

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("iaam-app-fault-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temporary directory");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Three assertions of one batch. The identifiers are fresh on every call, so
/// a retry and a clean run never share them; the idempotency keys are the
/// batch's identity across processes.
fn batch(owner: OwnerId, account: AccountId) -> Vec<Event> {
    (1..=3)
        .map(|sequence| Event {
            id: EventId::new_random(),
            owner,
            account,
            kind: EventKind::ControlAssertion {
                period: AssertionPeriod::between(date!(2026 - 01 - 01), date!(2026 - 01 - 31))
                    .expect("valid period"),
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(100_000 * i64::from(sequence)),
                    at: BalancePoint::Closing,
                },
            },
            dates: EventDates::empty(),
            order: EffectiveOrder::new(date!(2026 - 01 - 31), sequence),
            legs: vec![],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&format!("{sequence}").repeat(64)[..64]).expect("invented hash"),
                ParserVersion("fault-kill-test/1".to_owned()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: Some(format!("fault-batch-{sequence}")),
        })
        .collect()
}

/// What a fact means, without the identifiers and stamps a new attempt
/// generates: the comparison the acceptance asks for.
#[derive(Debug, PartialEq, Eq)]
struct Meaning {
    account: AccountId,
    sequence: u32,
    idempotency_key: Option<String>,
    raw_hash: String,
}

fn meanings(events: &[Event]) -> Vec<Meaning> {
    let mut meanings: Vec<_> = events
        .iter()
        .map(|event| Meaning {
            account: event.account,
            sequence: event.order.sequence(),
            idempotency_key: event.idempotency_key.clone(),
            raw_hash: event.provenance.raw_hash().as_str().to_owned(),
        })
        .collect();
    meanings.sort_by_key(|meaning| meaning.sequence);
    meanings
}

async fn seed(database: &std::path::Path) -> (OwnerId, AccountId) {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    seed_same_account(database, owner, account).await;
    (owner, account)
}

/// Seeds `database` with the account the comparison names, so a clean run in
/// another file is compared against the same account rather than a new one.
async fn seed_same_account(database: &std::path::Path, owner: OwnerId, account: AccountId) {
    let adapter = SqliteAdapter::new(SqliteStore::open(database).expect("create database"));
    adapter
        .upsert_account(
            owner,
            AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Invented Bank".to_owned()),
            },
        )
        .await
        .expect("seed account");
}

fn child_parts() -> Option<(PathBuf, OwnerId, AccountId)> {
    let database = std::env::var_os(fault::DATABASE_VARIABLE)?;
    let owner = std::env::var(OWNER_VARIABLE).expect("owner configured");
    let account = std::env::var(ACCOUNT_VARIABLE).expect("account configured");
    Some((
        PathBuf::from(database),
        OwnerId(Uuid::parse_str(&owner).expect("owner identifier")),
        AccountId(Uuid::parse_str(&account).expect("account identifier")),
    ))
}

fn run_child_batch(database: &std::path::Path, owner: OwnerId, account: AccountId) {
    let adapter = SqliteAdapter::new(
        SqliteStore::open_existing(database).expect("the child opens the database"),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let _ = adapter
            .append_events(batch(owner, account), IdentityScope::Source)
            .await;
    });
}

#[test]
fn killed_before_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, owner, account) = child_parts().expect("child configuration");
    run_child_batch(&database, owner, account);
}

#[test]
fn killed_after_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, owner, account) = child_parts().expect("child configuration");
    run_child_batch(&database, owner, account);
}

fn kill(
    directory: &TempDirectory,
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    database: &std::path::Path,
    ids: (OwnerId, AccountId),
) -> KillAt {
    let (owner, account) = ids;
    let owner_value = owner.inner().to_string();
    let account_value = account.inner().to_string();
    fault::kill_at(
        child_test,
        point,
        after_write,
        &directory.file("barrier-reached"),
        database,
        &[
            (OWNER_VARIABLE, OsStr::new(&owner_value)),
            (ACCOUNT_VARIABLE, OsStr::new(&account_value)),
        ],
    )
}

#[tokio::test]
async fn a_kill_before_commit_leaves_nothing_and_the_retry_records_the_batch() {
    let directory = TempDirectory::new("before-commit");
    let database = directory.file("iaam.sqlite");
    let (owner, account) = seed(&database).await;

    let KillAt { store, child } = kill(
        &directory,
        "killed_before_commit_child",
        fault::BEFORE_COMMIT,
        Some(1),
        &database,
        (owner, account),
    );
    assert!(
        child.marker.exists(),
        "the child was killed at the before-commit barrier"
    );
    let store = SqliteAdapter::new(store);
    assert!(
        store
            .load_events_through(owner, time::Date::MAX)
            .await
            .expect("the killed database reopens")
            .is_empty(),
        "a kill inside the transaction leaves none of the three"
    );

    let retried = store
        .append_events(batch(owner, account), IdentityScope::Source)
        .await
        .expect("the retry records the batch");
    assert_eq!(retried.len(), 3);
    assert!(
        retried
            .iter()
            .all(|outcome| matches!(outcome, Recorded::Inserted { .. })),
        "nothing of the killed attempt survived, so every event inserts: {retried:?}"
    );
    let retried_journal = store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("read the retried journal");
    assert_eq!(retried_journal.len(), 3);

    let clean_database = directory.file("clean.sqlite");
    seed_same_account(&clean_database, owner, account).await;
    let clean = SqliteAdapter::new(
        SqliteStore::open_existing(&clean_database).expect("clean database reopens"),
    );
    clean
        .append_events(batch(owner, account), IdentityScope::Source)
        .await
        .expect("the clean run records the batch");
    let clean_journal = clean
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("read the clean journal");

    assert_eq!(
        meanings(&retried_journal),
        meanings(&clean_journal),
        "the retry records what a clean run records, by meaning"
    );
}

#[tokio::test]
async fn a_kill_after_commit_leaves_the_batch_and_the_retry_records_duplicates() {
    let directory = TempDirectory::new("after-commit");
    let database = directory.file("iaam.sqlite");
    let (owner, account) = seed(&database).await;

    let KillAt { store, child } = kill(
        &directory,
        "killed_after_commit_child",
        fault::AFTER_COMMIT,
        None,
        &database,
        (owner, account),
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the commit"
    );
    let store = SqliteAdapter::new(store);
    let committed = store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("the killed database reopens");
    assert_eq!(
        committed.len(),
        3,
        "a kill after COMMIT leaves the whole batch"
    );

    let retried = store
        .append_events(batch(owner, account), IdentityScope::Source)
        .await
        .expect("the retry is answered, not refused");
    assert_eq!(retried.len(), 3);
    assert!(
        retried
            .iter()
            .all(|outcome| matches!(outcome, Recorded::Duplicate { .. })),
        "the retry recognises every event it already committed: {retried:?}"
    );
    assert_eq!(
        store
            .load_events_through(owner, time::Date::MAX)
            .await
            .expect("read the journal again")
            .len(),
        3,
        "nothing is recorded twice"
    );
}
