//! ADR 0042 points 1, 2 and 4: a correction batch is one transaction, and a
//! retry of it is recognised rather than repeated.
//!
//! Each child test is the work the parent runs in another process and kills at
//! a named store barrier.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{AccountView, Clock, Principal};
use iaam_app::scenarios::correction::{CorrectionOutcome, CorrectionRequest, correct_events};
use iaam_app::storage::SqliteStore;
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::correction::resolve;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::projection::balances::Balances;
use iaam_ingest::Verdict;
use iaam_ingest::classification::Classification;
use iaam_ingest::dedup::IdentityScope;
use iaam_store::fault::{self, KillAt};
use time::macros::date;
use uuid::Uuid;

const OWNER_VARIABLE: &str = "IAAM_FAULT_TEST_OWNER";
const TOKEN_VARIABLE: &str = "IAAM_FAULT_TEST_TOKEN";
const TARGET_VARIABLE: &str = "IAAM_FAULT_TEST_TARGET";
const SENDER_VARIABLE: &str = "IAAM_FAULT_TEST_SENDER";

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> time::Date {
        date!(2026 - 08 - 31)
    }
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "iaam-app-correction-fault-{label}-{}",
            Uuid::new_v4()
        ));
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

fn services_at(database: &Path, create: bool) -> AppServices {
    let store = if create {
        SqliteStore::open(database).expect("create database")
    } else {
        SqliteStore::open_existing(database).expect("open database")
    };
    let adapter = Arc::new(SqliteAdapter::new(store));
    AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        Arc::new(FixedClock),
    )
}

fn principal(owner: OwnerId, token_id: Uuid) -> Principal {
    Principal {
        token_id,
        owner,
        scope: iaam_app::ports::Scope::Owner,
    }
}

fn deposit(owner: OwnerId, account: AccountId, idempotency_key: &str) -> Event {
    let money = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
    Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::CashIn { amount: money },
        dates: EventDates::for_cash(CashPostedDate(date!(2026 - 08 - 01))),
        order: EffectiveOrder::new(date!(2026 - 08 - 01), 1),
        legs: vec![Leg::cash(account, money)],
        provenance: Provenance::new(
            SourceId::declared(owner, account, "file"),
            RawHash::parse(&"d".repeat(64)).expect("invented hash"),
            ParserVersion("correction-fault/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: Some(idempotency_key.to_owned()),
    }
}

fn reclassify(target: EventId, sender: AccountId) -> CorrectionRequest {
    CorrectionRequest::Reclassification {
        target,
        classification: Classification::InternalTransfer { to: sender },
    }
}

async fn seed(
    services: &AppServices,
    owner: OwnerId,
    account: AccountId,
    sender: AccountId,
    idempotency_key: &str,
) -> Event {
    for (id, title) in [(account, "Main"), (sender, "Savings")] {
        services
            .store
            .upsert_account(
                owner,
                AccountView {
                    id,
                    title: title.to_owned(),
                    institution: Some("Invented Bank".to_owned()),
                },
            )
            .await
            .expect("seed account");
    }
    let event = deposit(owner, account, idempotency_key);
    services
        .store
        .append_events(vec![event.clone()], IdentityScope::Source)
        .await
        .expect("seed the corrected fact");
    event
}

fn balance(events: &[Event], account: AccountId) -> Option<Money> {
    let effective = resolve(events).expect("the journal resolves");
    let mut balances = Balances::new();
    for event in effective {
        balances.apply(event).expect("balance folds");
    }
    balances.cash(account, CurrencyCode::Rub)
}

fn correction_rows(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| !matches!(event.relation, Relation::None))
        .count()
}

/// What a fact means, without the identifiers a fresh attempt generates — the
/// event's own, the one its relation points at, and a transfer's link.
fn meaning(event: &Event, by_id: &BTreeMap<EventId, &Event>) -> String {
    let named = |target: EventId| {
        by_id.get(&target).map_or_else(
            || format!("{target:?}"),
            |target| {
                target
                    .idempotency_key
                    .clone()
                    .unwrap_or_else(|| format!("{target:?}"))
            },
        )
    };
    let (relation, target) = match event.relation {
        Relation::None => ("fact".to_owned(), None),
        Relation::Reversal { target } => (format!("reversal of {}", named(target)), Some(target)),
        Relation::Replacement { target } => {
            (format!("replacement of {}", named(target)), Some(target))
        }
    };
    // A transfer's link identifier is generated per attempt; what the kind
    // states is the two accounts and the amount.
    let kind = match &event.kind {
        EventKind::CashTransfer {
            from, to, amount, ..
        } => format!("transfer {from:?} to {to:?} {amount:?}"),
        other => format!("{other:?}"),
    };
    // A correction's own key names the event it corrects, so it carries that
    // event's identifier; the identifier is what a fresh run of the same
    // correction generates again, and the key in its place is what the two runs
    // hold in common.
    let mut key = event.idempotency_key.clone().unwrap_or_default();
    if let Some(target) = target {
        key = key.replace(&target.inner().to_string(), &named(target));
    }
    // The reversal's raw hash is the digest of its own key, which is compared
    // above after the corrected event's identifier has been named; the
    // replacement's is the digest of the operation, whose every stated value
    // `kind` and `dates` carry.
    format!(
        "{relation} | {kind} | {key} | {:?} | {:?} | {:?}",
        event.dates,
        event.order.date(),
        event.order.source_time(),
    )
}

fn meanings(events: &[Event]) -> Vec<String> {
    let by_id: BTreeMap<EventId, &Event> = events.iter().map(|event| (event.id, event)).collect();
    let mut meanings: Vec<String> = events
        .iter()
        .map(|event| meaning(event, &by_id))
        .collect();
    meanings.sort();
    meanings
}

/// What the request answered, by the meaning of the fact it names.
fn answers(outcomes: &[CorrectionOutcome], events: &[Event]) -> Vec<String> {
    let by_id: BTreeMap<EventId, &Event> = events.iter().map(|event| (event.id, event)).collect();
    outcomes
        .iter()
        .map(|outcome| {
            let (verdict, event) = match &outcome.verdict {
                Verdict::Provisional { event } => ("provisional".to_owned(), Some(*event)),
                Verdict::Duplicate { existing } => ("duplicate".to_owned(), Some(*existing)),
                other => (format!("{other:?}"), None),
            };
            let fact = event.map_or_else(String::new, |event| {
                by_id
                    .get(&event)
                    .map_or_else(|| format!("{event:?}"), |fact| meaning(fact, &by_id))
            });
            format!(
                "{verdict} {fact} | standing rule {:?}",
                outcome.standing_rule.map(|standing| (standing.version, standing.still_filed))
            )
        })
        .collect()
}

struct ChildParts {
    database: PathBuf,
    owner: OwnerId,
    token_id: Uuid,
    target: EventId,
    sender: AccountId,
}

fn child_parts() -> Option<ChildParts> {
    let database = std::env::var_os(fault::DATABASE_VARIABLE)?;
    Some(ChildParts {
        database: PathBuf::from(database),
        owner: OwnerId(Uuid::parse_str(&std::env::var(OWNER_VARIABLE).expect("owner")).unwrap()),
        token_id: Uuid::parse_str(&std::env::var(TOKEN_VARIABLE).expect("token")).unwrap(),
        target: EventId(Uuid::parse_str(&std::env::var(TARGET_VARIABLE).expect("target")).unwrap()),
        sender: AccountId(
            Uuid::parse_str(&std::env::var(SENDER_VARIABLE).expect("sender")).unwrap(),
        ),
    })
}

fn run_child_reclassification(parts: ChildParts) {
    let services = services_at(&parts.database, false);
    let principal = principal(parts.owner, parts.token_id);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let _ = correct_events(
            &services,
            &principal,
            true,
            &[reclassify(parts.target, parts.sender)],
        )
        .await;
    });
}

/// The child is killed after the batch's second write — the reversal and the
/// replacement of a reclassification are both written, and neither is
/// committed. A threshold of one would pass just as well against a store that
/// committed each fact on its own.
#[test]
fn killed_mid_batch_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    run_child_reclassification(child_parts().expect("child configuration"));
}

#[test]
fn killed_after_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    run_child_reclassification(child_parts().expect("child configuration"));
}

/// The identifiers a child needs to rebuild the same correction request.
struct CorrectionIds {
    owner: OwnerId,
    token_id: Uuid,
    target: EventId,
    sender: AccountId,
}

fn kill(
    directory: &TempDirectory,
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    database: &Path,
    ids: &CorrectionIds,
) -> KillAt {
    fault::kill_at(
        child_test,
        point,
        after_write,
        &directory.file("barrier-reached"),
        database,
        &[
            (OWNER_VARIABLE, OsStr::new(&ids.owner.inner().to_string())),
            (TOKEN_VARIABLE, OsStr::new(&ids.token_id.to_string())),
            (TARGET_VARIABLE, OsStr::new(&ids.target.inner().to_string())),
            (SENDER_VARIABLE, OsStr::new(&ids.sender.inner().to_string())),
        ],
    )
}

#[tokio::test]
async fn a_kill_mid_batch_leaves_neither_correction_fact_and_the_retry_matches_a_clean_run() {
    let directory = TempDirectory::new("mid-batch");
    let database = directory.file("iaam.sqlite");
    let services = services_at(&database, true);
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let sender = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let target = seed(&services, owner, account, sender, "correction-fault-target")
        .await
        .id;

    // The second write of the batch, not the first: a store that committed each
    // fact on its own would leave the reversal behind and pass a threshold of
    // one.
    let KillAt { store, child } = kill(
        &directory,
        "killed_mid_batch_child",
        fault::BEFORE_COMMIT,
        Some(2),
        &database,
        &CorrectionIds {
            owner,
            token_id,
            target,
            sender,
        },
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the batch's second write, before COMMIT"
    );
    let adapter = Arc::new(SqliteAdapter::new(store));
    let reopened = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        Arc::new(FixedClock),
    );
    let journal = reopened
        .store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("the killed database reopens");
    assert_eq!(
        correction_rows(&journal),
        0,
        "a kill before COMMIT leaves neither the reversal nor the replacement, \
         although both were written"
    );
    assert_eq!(journal.len(), 1, "the corrected fact alone remains");

    // The identical request, repeated after reopening, and a clean run of it on
    // a database of its own: the two must leave the same facts and answer the
    // same thing. The balance alone would not do — a reclassification to a
    // transfer leaves the corrected account's balance as it was either way.
    let retried = correct_events(
        &reopened,
        &principal(owner, token_id),
        true,
        &[reclassify(target, sender)],
    )
    .await
    .expect("the retry records the correction");
    let retried_journal = reopened
        .store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("read the retried journal");

    let clean_database = directory.file("clean.sqlite");
    let clean_services = services_at(&clean_database, true);
    let clean_target = seed(
        &clean_services,
        owner,
        account,
        sender,
        "correction-fault-target",
    )
    .await
    .id;
    let clean = correct_events(
        &clean_services,
        &principal(owner, token_id),
        true,
        &[reclassify(clean_target, sender)],
    )
    .await
    .expect("the clean correction commits");
    let clean_journal = clean_services
        .store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("read the clean journal");

    assert_eq!(
        meanings(&retried_journal),
        meanings(&clean_journal),
        "the retry records what a clean run records, by meaning"
    );
    assert_eq!(
        answers(&retried, &retried_journal),
        answers(&clean, &clean_journal),
        "the retry answers what a clean run answers, by meaning"
    );
    assert_eq!(
        balance(&retried_journal, account),
        balance(&clean_journal, account),
        "the account balance equals a clean run's"
    );
    assert!(
        balance(&retried_journal, account).is_some(),
        "the comparison is not two absent balances"
    );
}

#[tokio::test]
async fn a_kill_after_commit_keeps_both_facts_and_the_identical_retry_repeats_the_answer() {
    let directory = TempDirectory::new("after-commit");
    let database = directory.file("iaam.sqlite");
    let services = services_at(&database, true);
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let sender = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let target = seed(&services, owner, account, sender, "correction-fault-target")
        .await
        .id;

    let KillAt { store, child } = kill(
        &directory,
        "killed_after_commit_child",
        fault::AFTER_COMMIT,
        None,
        &database,
        &CorrectionIds {
            owner,
            token_id,
            target,
            sender,
        },
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the batch committed"
    );
    let services = {
        let adapter = Arc::new(SqliteAdapter::new(store));
        AppServices::new(
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            Arc::new(FixedClock),
        )
    };
    let journal = services
        .store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("the killed database reopens");
    assert_eq!(journal.len(), 3, "the whole batch survived the kill");
    let replacement = journal
        .iter()
        .find(|event| matches!(event.relation, Relation::Replacement { .. }))
        .expect("the replacement is present")
        .id;

    let principal = principal(owner, token_id);
    let retried = correct_events(&services, &principal, true, &[reclassify(target, sender)])
        .await
        .expect("the identical retry is answered, not refused");
    assert_eq!(retried.len(), 1);
    assert_eq!(
        retried[0].verdict,
        iaam_ingest::Verdict::Provisional { event: replacement },
        "the retry returns the answer the first call returned, reconstructed"
    );
    assert_eq!(
        services
            .store
            .load_events_through(owner, time::Date::MAX)
            .await
            .expect("read the journal again")
            .len(),
        3,
        "the retry writes nothing"
    );

    let different = correct_events(
        &services,
        &principal,
        true,
        &[CorrectionRequest::Reclassification {
            target,
            classification: Classification::OwnAccountMovement,
        }],
    )
    .await
    .expect_err("a different payload under the same key is refused");
    assert!(
        matches!(different, AppError::Conflict { .. }),
        "expected a conflict, got {different:?}"
    );
    assert_eq!(
        services
            .store
            .load_events_through(owner, time::Date::MAX)
            .await
            .expect("read the journal after the refusal")
            .len(),
        3,
        "the refusal writes nothing"
    );
}
