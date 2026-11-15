//! ADR 0042 points 1, 2 and 4 for an import commit: the rows, the assertions,
//! the coverage gaps and the session's close are one transaction, and a commit
//! repeated after a lost response is answered from what the first one wrote.
//!
//! Each child test below is the work the parent runs in another process and
//! kills at a named store barrier.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{AccountView, Clock, ImportSessionState, Principal, Scope};
use iaam_app::scenarios::import_session::{
    CommitOutcome, SessionRevision, commit_session, plan_session, read_session,
};
use iaam_app::storage::SqliteStore;
use iaam_core::batch::ControlSection;
use iaam_core::event::kind::EventKind;
use iaam_core::event::{Event, Relation};
use iaam_core::ids::{AccountId, EventId, ImportSessionId, OwnerId};
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::reconciliation::claim::AssertionPeriod;
use iaam_ingest::Verdict;
use iaam_ingest::classification::FarSide;
use iaam_ingest::observation::{
    Intake, ObservedCounterparty, ObservedDirection, ObservedRow, RowIdentity,
};
use iaam_ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use iaam_store::fault::{self, KillAt};
use time::macros::date;
use uuid::Uuid;

const OWNER_VARIABLE: &str = "IAAM_FAULT_TEST_OWNER";
const TOKEN_VARIABLE: &str = "IAAM_FAULT_TEST_TOKEN";
const SESSION_VARIABLE: &str = "IAAM_FAULT_TEST_SESSION";
const REVISION_VARIABLE: &str = "IAAM_FAULT_TEST_REVISION";

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> time::Date {
        date!(2026 - 02 - 01)
    }
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "iaam-app-import-commit-fault-{label}-{}",
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

/// `AppServices` over one database, with the rules port wired: a plan reads the
/// owner's classification rules, and the default leaves that port unavailable.
fn services_at(database: &Path, create: bool) -> AppServices {
    let store = if create {
        SqliteStore::open(database).expect("create database")
    } else {
        SqliteStore::open_existing(database).expect("open database")
    };
    let adapter = Arc::new(SqliteAdapter::new(store));
    let mut services = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        Arc::new(FixedClock),
    );
    services.rules = adapter;
    services
}

fn principal(owner: OwnerId, token_id: Uuid) -> Principal {
    Principal {
        token_id,
        owner,
        scope: Scope::Owner,
    }
}

/// One line the caller has already concluded: a deposit onto `account`.
///
/// Concluded rather than observed so that the five rows become five facts
/// without a question, which is what this test is about; the unreadable row
/// beside them is what a coverage gap is written for.
fn deposit(account: AccountId, key: &str, day: time::Date) -> String {
    let operation = SubmittedOperation {
        account,
        kind: OperationKind::Deposit {
            amount_minor: 10_000,
            currency: CurrencyCode::Rub,
        },
        dates: OperationDates {
            cash_posted: Some(day),
            ..OperationDates::default()
        },
        source_time: None,
        idempotency_key: Some(key.to_owned()),
        source_operation_id: None,
        source_position_id: None,
        source_category: None,
        owner_category: None,
        source_code: None,
        source_kind: None,
        description: None,
        counterparty: None,
    };
    serde_json::to_string(&Intake::Concluded {
        operation: Box::new(operation),
    })
    .expect("a row payload this test states completely")
}

/// The control section the invented source prints for itself: five deposits of
/// 100.00 over the period, so the rows agree with it and the assertions are
/// written.
fn section(account: AccountId) -> ControlSection {
    ControlSection {
        account,
        currency: CurrencyCode::Rub,
        period: AssertionPeriod::between(date!(2026 - 01 - 01), date!(2026 - 01 - 31))
            .expect("valid period"),
        opening: Some(PostedMinor::new(0)),
        closing: Some(PostedMinor::new(50_000)),
        debit_turnover: Some(PostedMinor::new(50_000)),
        credit_turnover: None,
    }
}

const ROWS: [i64; 5] = [1, 2, 3, 4, 5];

/// One line the caller reported without concluding it, and which no rule of his
/// makes into a fact: what a commit declines, and therefore what a coverage gap
/// is written for.
///
/// Observed rather than unreadable on purpose: the row names its account, and a
/// gap is dated and attributed by the account of the rows it refused. A row
/// nobody could read names no account and so produces no gap at all.
fn unclassified(account: AccountId) -> String {
    serde_json::to_string(&Intake::Observed {
        row: Box::new(ObservedRow {
            account,
            direction: ObservedDirection::In,
            amount_minor: 25_000,
            currency: CurrencyCode::Rub,
            counterparty: ObservedCounterparty::Unknown,
            far_side: FarSide::Unstated,
            source_kind: Some("Deposit".to_owned()),
            source_category: None,
            owner_category: None,
            source_code: None,
            description: Some("an invented line no rule of his settles".to_owned()),
            dates: OperationDates {
                cash_posted: Some(date!(2026 - 01 - 06)),
                ..OperationDates::default()
            },
            source_time: None,
            identity: RowIdentity {
                document: Some("invented-statement".to_owned()),
                row: Some("row-unclassified".to_owned()),
                idempotency_key: None,
            },
        }),
        reader: None,
    })
    .expect("an observed row this test states completely")
}

/// A session holding five concluded rows and one this build cannot read, and the
/// control section its source printed; and the revision a caller reads before
/// committing it.
async fn seed_session(
    services: &AppServices,
    owner: OwnerId,
    account: AccountId,
    token_id: Uuid,
) -> (ImportSessionId, SessionRevision) {
    services
        .store
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
    let session = services
        .store
        .open_import_session(owner, Some(account), None, None)
        .await
        .expect("open session")
        .id;
    for row in ROWS {
        services
            .store
            .add_import_observation(
                owner,
                session,
                Some(format!("row-{row}")),
                true,
                deposit(
                    account,
                    &format!("import-commit-fault-{row}"),
                    date!(2026 - 01 - 01) + time::Duration::days(row),
                ),
            )
            .await
            .expect("an observation fits the session");
    }
    // A row no rule of his settles: the commit declines it, and a declined row
    // beside a stated control section is what a coverage gap is written for.
    services
        .store
        .add_import_observation(
            owner,
            session,
            Some("row-unclassified".to_owned()),
            false,
            unclassified(account),
        )
        .await
        .expect("an observed row is still a row");
    services
        .store
        .state_import_control_figures(owner, session, vec![section(account)])
        .await
        .expect("the source's control section");

    let planned = plan_session(services, &principal(owner, token_id), session)
        .await
        .expect("the session plans");
    assert_eq!(planned.plan.session.state, ImportSessionState::Open);
    assert!(
        planned.plan.interpretation.open_questions.is_empty(),
        "the seeded rows raise no question"
    );
    assert_eq!(
        planned.plan.control_reconciliation.comparisons.len(),
        1,
        "the stated control section is compared with the rows"
    );
    assert!(
        planned
            .plan
            .control_reconciliation
            .comparisons
            .iter()
            .all(|comparison| comparison.stated.is_some()),
        "and the section is carried into the plan, which is what the commit writes \
         its assertions from"
    );
    assert_eq!(
        planned.plan.control_reconciliation.mismatches(),
        0,
        "the rows agree with the source's own figures"
    );
    assert_eq!(
        planned.plan.commit_delta.retained_unrecorded.len(),
        1,
        "the unclassified row is declined, which is what the coverage gap names"
    );
    (session, planned.plan.revision)
}

/// What a fact means, without the identifiers and stamps a new attempt
/// generates.
fn meaning(event: &Event) -> String {
    let kind = match &event.kind {
        EventKind::ControlAssertion { period, claim } => {
            format!("assertion {period:?} {claim:?}")
        }
        EventKind::ImportCoverageGap { .. } => "gap".to_owned(),
        other => format!("{other:?}"),
    };
    format!(
        "{kind} | key {:?} | raw {} | {:?}",
        event.idempotency_key,
        event.provenance.raw_hash().as_str(),
        Relation::None == event.relation
    )
}

fn meanings(events: &[Event]) -> Vec<String> {
    let mut meanings: Vec<String> = events.iter().map(meaning).collect();
    meanings.sort();
    meanings
}

fn corrections(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ImportCoverageGap { .. }))
        .count()
}

fn assertions(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ControlAssertion { .. }))
        .count()
}

async fn journal(services: &AppServices, owner: OwnerId) -> Vec<Event> {
    services
        .store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("the journal reads back")
}

/// What a commit answered, by meaning: the row verdicts in the order the rows
/// were fed, with every event identifier named by the fact it points at.
fn answer(outcome: &CommitOutcome, events: &[Event]) -> Vec<String> {
    let by_id: BTreeMap<EventId, &Event> = events.iter().map(|event| (event.id, event)).collect();
    let named = |id: EventId| {
        by_id
            .get(&id)
            .and_then(|event| event.idempotency_key.clone())
            .unwrap_or_else(|| format!("{id:?}"))
    };
    let verdicts: Vec<String> = outcome
        .verdicts
        .iter()
        .map(|verdict| match verdict {
            Verdict::Provisional { event } => format!("provisional {}", named(*event)),
            Verdict::Duplicate { existing } => format!("duplicate {}", named(*existing)),
            Verdict::Rejected { rejection } => format!("rejected {}", rejection.field),
            Verdict::NoFact { reason } => format!("no fact {reason}"),
            other => format!("{other:?}"),
        })
        .collect();
    let mut lines = verdicts;
    lines.push(format!("assertions {}", outcome.control_assertions.len()));
    lines.push(format!("gaps {}", outcome.coverage_gaps.len()));
    lines
}

/// The identifiers a child needs to repeat one commit.
struct ChildIds {
    owner: OwnerId,
    token_id: Uuid,
    session: ImportSessionId,
    revision: SessionRevision,
}

fn child_parts() -> Option<(PathBuf, ChildIds)> {
    let database = std::env::var_os(fault::DATABASE_VARIABLE)?;
    Some((
        PathBuf::from(database),
        ChildIds {
            owner: OwnerId(
                Uuid::parse_str(&std::env::var(OWNER_VARIABLE).expect("owner")).unwrap(),
            ),
            token_id: Uuid::parse_str(&std::env::var(TOKEN_VARIABLE).expect("token")).unwrap(),
            session: ImportSessionId(
                Uuid::parse_str(&std::env::var(SESSION_VARIABLE).expect("session")).unwrap(),
            ),
            revision: SessionRevision(std::env::var(REVISION_VARIABLE).expect("revision")),
        },
    ))
}

fn run_child_commit(database: &Path, ids: &ChildIds) {
    let services = services_at(database, false);
    let principal = principal(ids.owner, ids.token_id);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let _ = commit_session(
            &services,
            &principal,
            ids.session,
            Some(&ids.revision),
            false,
        )
        .await;
    });
}

#[test]
fn killed_mid_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, ids) = child_parts().expect("child configuration");
    run_child_commit(&database, &ids);
}

#[test]
fn killed_after_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, ids) = child_parts().expect("child configuration");
    run_child_commit(&database, &ids);
}

fn kill(
    directory: &TempDirectory,
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    database: &Path,
    ids: &ChildIds,
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
            (
                SESSION_VARIABLE,
                OsStr::new(&ids.session.inner().to_string()),
            ),
            (REVISION_VARIABLE, OsStr::new(&ids.revision.0)),
        ],
    )
}

#[tokio::test]
async fn a_kill_mid_commit_leaves_the_session_open_and_the_retry_matches_a_clean_run() {
    let directory = TempDirectory::new("mid-commit");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let (session, revision) = seed_session(&services, owner, account, token_id).await;

    // The commit's second write, not its first: a commit that wrote each row in
    // a transaction of its own would leave one behind and pass a threshold of
    // one.
    let ids = ChildIds {
        owner,
        token_id,
        session,
        revision: revision.clone(),
    };
    let KillAt { store, child } = kill(
        &directory,
        "killed_mid_commit_child",
        fault::BEFORE_COMMIT,
        Some(2),
        &database,
        &ids,
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the commit's second write, before COMMIT"
    );
    let reopened = {
        let adapter = Arc::new(SqliteAdapter::new(store));
        let mut services = AppServices::new(
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            Arc::new(FixedClock),
        );
        services.rules = adapter;
        services
    };
    let asking = principal(owner, token_id);
    let contents = read_session(&reopened, &asking, session)
        .await
        .expect("the killed database reopens with the session");
    assert_eq!(
        contents.session.state,
        ImportSessionState::Open,
        "a kill inside the commit leaves the session open"
    );
    assert_eq!(
        contents.session.closed_at, None,
        "and the session was not closed on the way out"
    );
    let surviving = journal(&reopened, owner).await;
    assert!(
        surviving.is_empty(),
        "a kill before COMMIT leaves nothing of the commit, although two rows were written; \
         found {} event(s)",
        surviving.len()
    );
    assert_eq!(
        (assertions(&surviving), corrections(&surviving)),
        (0, 0),
        "no assertion stands without the coverage gap written with it"
    );

    // The identical recommit, and a clean commit of the same session content on
    // a database of its own: state and answer must agree by meaning.
    let retried = commit_session(&reopened, &asking, session, Some(&revision), false)
        .await
        .expect("the recommit records the whole commit");
    let retried_journal = journal(&reopened, owner).await;

    let clean_database = directory.file("clean.sqlite");
    let clean_services = services_at(&clean_database, true);
    let (clean_session, clean_revision) =
        seed_session(&clean_services, owner, account, token_id).await;
    let clean = commit_session(
        &clean_services,
        &principal(owner, token_id),
        clean_session,
        Some(&clean_revision),
        false,
    )
    .await
    .expect("the clean commit records the same rows");
    let clean_journal = journal(&clean_services, owner).await;

    assert_eq!(
        ROWS.len() + 1,
        retried.verdicts.len(),
        "one verdict per held row"
    );
    assert_eq!(
        meanings(&retried_journal),
        meanings(&clean_journal),
        "the retry leaves what a clean commit leaves, by meaning"
    );
    assert_eq!(
        answer(&retried, &retried_journal),
        answer(&clean, &clean_journal),
        "the retry answers what a clean commit answers, by meaning"
    );
    assert_eq!(
        (assertions(&retried_journal), corrections(&retried_journal)),
        (assertions(&clean_journal), corrections(&clean_journal)),
        "the assertions and the gaps land in the same numbers"
    );
    assert!(
        assertions(&retried_journal) > 0 && corrections(&retried_journal) > 0,
        "the comparison is not two empty journals"
    );
    let closed = read_session(&reopened, &asking, session)
        .await
        .expect("the committed session reads back");
    assert_eq!(
        closed.session.state,
        ImportSessionState::Committed,
        "the session is closed by the commit that wrote the rows"
    );
}

#[tokio::test]
async fn a_kill_after_commit_answers_the_identical_retry_with_the_first_answer() {
    let directory = TempDirectory::new("after-commit");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let (session, revision) = seed_session(&services, owner, account, token_id).await;

    let ids = ChildIds {
        owner,
        token_id,
        session,
        revision: revision.clone(),
    };
    let KillAt { store, child } = kill(
        &directory,
        "killed_after_commit_child",
        fault::AFTER_COMMIT,
        None,
        &database,
        &ids,
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the commit and before its answer"
    );
    let reopened = {
        let adapter = Arc::new(SqliteAdapter::new(store));
        let mut services = AppServices::new(
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            Arc::new(FixedClock),
        );
        services.rules = adapter;
        services
    };
    let asking = principal(owner, token_id);
    let committed = journal(&reopened, owner).await;
    assert!(
        !committed.is_empty(),
        "a kill after COMMIT leaves the whole commit"
    );
    let contents = read_session(&reopened, &asking, session)
        .await
        .expect("the committed session reads back");
    assert_eq!(contents.session.state, ImportSessionState::Committed);

    let retried = commit_session(&reopened, &asking, session, Some(&revision), false)
        .await
        .expect("the identical retry is answered, not refused as «session not open»");
    assert_eq!(
        retried.revision, revision,
        "the retry answers under the revision the first call committed"
    );
    let after_retry = journal(&reopened, owner).await;
    assert_eq!(
        meanings(&after_retry),
        meanings(&committed),
        "the retry writes nothing"
    );

    // The first answer, compared with a clean commit's by meaning.
    let clean_database = directory.file("clean.sqlite");
    let clean_services = services_at(&clean_database, true);
    let (clean_session, clean_revision) =
        seed_session(&clean_services, owner, account, token_id).await;
    let clean = commit_session(
        &clean_services,
        &principal(owner, token_id),
        clean_session,
        Some(&clean_revision),
        false,
    )
    .await
    .expect("the clean commit records the same rows");
    let clean_journal = journal(&clean_services, owner).await;
    assert_eq!(
        answer(&retried, &after_retry),
        answer(&clean, &clean_journal),
        "the retry returns the first answer a clean commit gives, by meaning"
    );
    assert_eq!(
        meanings(&committed),
        meanings(&clean_journal),
        "the killed commit left what a clean commit leaves, by meaning"
    );
}

#[tokio::test]
async fn a_session_edited_between_the_plan_and_the_commit_is_refused_inside_the_transaction() {
    let directory = TempDirectory::new("edited");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let (session, revision) = seed_session(&services, owner, account, token_id).await;
    let asking = principal(owner, token_id);

    // A row the caller's approved plan did not see: the commit must refuse
    // rather than write a plan its caller never read.
    services
        .store
        .add_import_observation(
            owner,
            session,
            Some("row-late".to_owned()),
            true,
            deposit(account, "import-commit-fault-late", date!(2026 - 01 - 06)),
        )
        .await
        .expect("a row added after the plan was read");

    let refused = commit_session(&services, &asking, session, Some(&revision), false)
        .await
        .expect_err("a session changed since the plan is refused");
    assert!(
        matches!(&refused, AppError::Invalid { field, .. } if field == "revision"),
        "the refusal names the revision the caller read, got {refused:?}"
    );
    assert!(
        journal(&services, owner).await.is_empty(),
        "the refusal writes nothing"
    );
    let contents = read_session(&services, &asking, session)
        .await
        .expect("the session reads back");
    assert_eq!(
        contents.session.state,
        ImportSessionState::Open,
        "and the session stays open"
    );
}
