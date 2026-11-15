//! ADR 0042 points 1, 2 and 4 for an import session's intake: the rows of one
//! request, their questions and the answer the request gave are one
//! transaction, and a request repeated after a lost response is answered from
//! what the first one wrote (`iaam-mkbfh.9.1`).
//!
//! Each child test below is the work the parent runs in another process and
//! kills at a named store barrier. Every account, label and identifier here is
//! invented for this file.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Clock, ImportSessionState, Principal, Scope};
use iaam_app::scenarios::import_session::{HeldRow, add_rows, read_session};
use iaam_app::storage::SqliteStore;
use iaam_core::ids::{AccountId, ImportSessionId, OwnerId};
use iaam_core::money::CurrencyCode;
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
const ROWS_VARIABLE: &str = "IAAM_FAULT_TEST_ROWS";

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
            "iaam-app-import-intake-fault-{label}-{}",
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

/// `AppServices` over one database, with the rules port wired: a resolver reads
/// the owner's classification rules, and the default leaves that port
/// unavailable.
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
fn concluded(account: AccountId, key: &str, day: time::Date) -> Intake {
    Intake::Concluded {
        operation: Box::new(SubmittedOperation {
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
        }),
    }
}

/// One line the caller reported without concluding it, and which no rule of
/// his settles: the row raises a question, which is the second write of the
/// request.
fn questioned(account: AccountId, key: &str, day: time::Date) -> Intake {
    Intake::Observed {
        row: Box::new(ObservedRow {
            account,
            direction: ObservedDirection::In,
            amount_minor: 125_000,
            currency: CurrencyCode::Rub,
            counterparty: ObservedCounterparty::Named("Shop One".to_owned()),
            far_side: FarSide::Unstated,
            source_kind: Some("RETURN".to_owned()),
            source_category: None,
            owner_category: None,
            source_code: None,
            description: Some("an invented line no rule of his settles".to_owned()),
            dates: OperationDates {
                cash_posted: Some(day),
                ..OperationDates::default()
            },
            source_time: None,
            identity: RowIdentity {
                document: Some("invented-statement".to_owned()),
                row: Some(key.to_owned()),
                idempotency_key: None,
            },
        }),
        reader: None,
    }
}

/// The request both the child and the clean run submit: two concluded rows and
/// one that raises a question, so the request's second write is well inside it.
fn request(account: AccountId) -> Vec<Intake> {
    vec![
        concluded(account, "intake-fault-one", date!(2026 - 01 - 02)),
        concluded(account, "intake-fault-two", date!(2026 - 01 - 03)),
        questioned(account, "row-three", date!(2026 - 01 - 04)),
    ]
}

/// The account the session is declared for, and the session itself.
async fn seed_session(
    services: &AppServices,
    owner: OwnerId,
    account: AccountId,
) -> ImportSessionId {
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
    services
        .store
        .open_import_session(owner, Some(account), None, None)
        .await
        .expect("open session")
        .id
}

/// What one row's answer says, without the identifiers a new attempt mints.
fn meaning(held: &HeldRow) -> String {
    match held {
        HeldRow::Held { row } => format!("held row {row}"),
        HeldRow::Settled { row, reason } => format!("settled row {row} {}", reason.code()),
        HeldRow::Questioned { asked } => format!(
            "questioned row {} with {} alternatives: {}",
            asked.row,
            asked.alternatives.len(),
            asked.prompt
        ),
        HeldRow::Rejected { row, rejection } => format!("rejected row {row} {}", rejection.field),
    }
}

fn meanings(held: &[HeldRow]) -> Vec<String> {
    held.iter().map(meaning).collect()
}

/// Everything the child needs to repeat one intake request.
struct ChildIds {
    owner: OwnerId,
    token_id: Uuid,
    session: ImportSessionId,
    rows: Vec<Intake>,
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
            rows: serde_json::from_str(&std::env::var(ROWS_VARIABLE).expect("rows"))
                .expect("rows parse"),
        },
    ))
}

fn run_child_intake(database: &Path, ids: &ChildIds) {
    let services = services_at(database, false);
    let principal = principal(ids.owner, ids.token_id);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let _ = add_rows(&services, &principal, ids.session, &ids.rows).await;
    });
}

#[test]
fn killed_mid_intake_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, ids) = child_parts().expect("child configuration");
    run_child_intake(&database, &ids);
}

#[test]
fn killed_after_intake_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, ids) = child_parts().expect("child configuration");
    run_child_intake(&database, &ids);
}

fn kill(
    directory: &TempDirectory,
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    database: &Path,
    ids: &ChildIds,
) -> KillAt {
    let rows = serde_json::to_string(&ids.rows).expect("rows serialise");
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
            (ROWS_VARIABLE, OsStr::new(&rows)),
        ],
    )
}

#[tokio::test]
async fn a_kill_mid_intake_leaves_nothing_and_the_retry_matches_a_clean_run() {
    let directory = TempDirectory::new("mid-intake");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let session = seed_session(&services, owner, account).await;
    let rows = request(account);

    // The request's second write, not its first: a request that wrote each
    // row, each question and each answer on its own would leave something
    // behind and pass a threshold of one.
    let ids = ChildIds {
        owner,
        token_id,
        session,
        rows: rows.clone(),
    };
    let KillAt { store, child } = kill(
        &directory,
        "killed_mid_intake_child",
        fault::BEFORE_COMMIT,
        Some(2),
        &database,
        &ids,
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the request's second write, before COMMIT"
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
        "a kill inside the intake leaves the session open"
    );
    assert!(
        contents.observations.is_empty(),
        "a kill before COMMIT leaves none of the request's rows, although two writes completed; \
         found {}",
        contents.observations.len()
    );
    assert!(contents.questions.is_empty(), "and none of its questions");

    // The identical retry, and a clean run of the same request on a database of
    // its own: state and answer must agree by meaning.
    let retried = add_rows(&reopened, &asking, session, &rows)
        .await
        .expect("the retried request is completed");
    let retried_contents = read_session(&reopened, &asking, session)
        .await
        .expect("the session reads back");

    let clean_database = directory.file("clean.sqlite");
    let clean_services = services_at(&clean_database, true);
    let clean_session = seed_session(&clean_services, owner, account).await;
    let clean = add_rows(
        &clean_services,
        &principal(owner, token_id),
        clean_session,
        &rows,
    )
    .await
    .expect("the clean run holds the same rows");
    let clean_contents = read_session(&clean_services, &principal(owner, token_id), clean_session)
        .await
        .expect("the clean session reads back");

    assert_eq!(
        meanings(&retried),
        meanings(&clean),
        "the retry answers what a clean run answers, by meaning"
    );
    assert_eq!(
        retried_contents.observations.len(),
        clean_contents.observations.len(),
        "and the session holds what a clean run holds"
    );
    assert_eq!(
        retried_contents.questions.len(),
        clean_contents.questions.len()
    );
    assert_eq!(
        retried_contents
            .observations
            .iter()
            .map(|row| (row.row, row.payload.clone()))
            .collect::<Vec<_>>(),
        clean_contents
            .observations
            .iter()
            .map(|row| (row.row, row.payload.clone()))
            .collect::<Vec<_>>(),
        "the rows are the same rows, in the same positions"
    );
    assert!(
        retried
            .iter()
            .any(|held| matches!(held, HeldRow::Questioned { .. })),
        "the request holds a question, so its second write is inside the request"
    );
}

#[tokio::test]
async fn a_kill_after_intake_answers_the_identical_retry_with_the_first_answer() {
    let directory = TempDirectory::new("after-intake");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let session = seed_session(&services, owner, account).await;
    let rows = request(account);

    let ids = ChildIds {
        owner,
        token_id,
        session,
        rows: rows.clone(),
    };
    let KillAt { store, child } = kill(
        &directory,
        "killed_after_intake_child",
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
    let held = read_session(&reopened, &asking, session)
        .await
        .expect("the killed database reopens with the session");
    assert_eq!(
        held.observations.len(),
        rows.len(),
        "a kill after COMMIT leaves the whole request"
    );
    assert_eq!(held.questions.len(), 1, "and the question it raised");

    let retried = add_rows(&reopened, &asking, session, &rows)
        .await
        .expect("the identical retry is answered, not refused as «session not open»");
    let after_retry = read_session(&reopened, &asking, session)
        .await
        .expect("the session reads back");
    assert_eq!(
        after_retry
            .observations
            .iter()
            .map(|row| (row.row, row.payload.clone()))
            .collect::<Vec<_>>(),
        held.observations
            .iter()
            .map(|row| (row.row, row.payload.clone()))
            .collect::<Vec<_>>(),
        "the retry holds nothing a second time"
    );
    assert_eq!(
        after_retry.questions.len(),
        held.questions.len(),
        "and asks nothing a second time"
    );

    // The first answer, compared with a clean run's by meaning.
    let clean_database = directory.file("clean.sqlite");
    let clean_services = services_at(&clean_database, true);
    let clean_session = seed_session(&clean_services, owner, account).await;
    let clean = add_rows(
        &clean_services,
        &principal(owner, token_id),
        clean_session,
        &rows,
    )
    .await
    .expect("the clean run holds the same rows");
    assert_eq!(
        meanings(&retried),
        meanings(&clean),
        "the retry returns the first answer a clean run gives, by meaning"
    );
}

#[tokio::test]
async fn a_row_already_held_under_its_key_with_another_payload_is_refused() {
    let directory = TempDirectory::new("other-payload");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let session = seed_session(&services, owner, account).await;
    let asking = principal(owner, token_id);

    add_rows(&services, &asking, session, &request(account))
        .await
        .expect("the first request is held");

    // The same key, a different amount: a different row wearing the same name.
    // Answering it with the row already held would record one line twice under
    // one identity, so it is refused rather than answered.
    let mut changed = request(account);
    if let Intake::Concluded { operation } = &mut changed[0] {
        operation.kind = OperationKind::Deposit {
            amount_minor: 20_000,
            currency: CurrencyCode::Rub,
        };
    }
    let refused = add_rows(&services, &asking, session, &changed)
        .await
        .expect_err("another payload under a held key is refused");
    assert!(
        matches!(&refused, iaam_app::error::AppError::Conflict { .. }),
        "the refusal says the key is taken by another row, got {refused:?}"
    );
    let contents = read_session(&services, &asking, session)
        .await
        .expect("the session reads back");
    assert_eq!(
        contents.observations.len(),
        request(account).len(),
        "and the refusal wrote nothing"
    );
}

/// A request repeated after its session was committed is answered from what the
/// request wrote (ADR 0042 point 4).
///
/// The row's key is recognised before the session's state is consulted, so a
/// caller whose response was lost and who repeats the identical request is given
/// the first answer rather than «session not open». A row the session does not
/// hold is still refused: a line added after commit would never be written.
#[tokio::test]
async fn a_request_repeated_after_the_session_was_committed_is_answered() {
    let directory = TempDirectory::new("committed");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let session = seed_session(&services, owner, account).await;
    let asking = principal(owner, token_id);
    let rows = request(account);

    let first = add_rows(&services, &asking, session, &rows)
        .await
        .expect("the request is held");
    services
        .store
        .close_import_session(owner, session, ImportSessionState::Committed)
        .await
        .expect("the session is committed by the route that records it");

    let repeated = add_rows(&services, &asking, session, &rows)
        .await
        .expect("the identical request is answered, not refused as «session not open»");
    assert_eq!(
        meanings(&repeated),
        meanings(&first),
        "the repeat is answered as the first call was"
    );
    let contents = read_session(&services, &asking, session)
        .await
        .expect("the committed session reads back");
    assert_eq!(
        contents.observations.len(),
        rows.len(),
        "and it wrote nothing"
    );

    let mut added = request(account);
    added.push(concluded(
        account,
        "intake-fault-three",
        date!(2026 - 01 - 05),
    ));
    assert!(
        add_rows(&services, &asking, session, &added).await.is_err(),
        "a row the committed session does not hold is refused"
    );
}
