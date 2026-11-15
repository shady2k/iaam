//! ADR 0042 points 1, 2 and 4 for an answer to an import question: the answers
//! one request writes, the rule they mint, the question that names it and the
//! stamps on the rows it settled are one transaction, and a request repeated
//! after a lost response is answered from what the first one wrote
//! (`iaam-mkbfh.9.1`).
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
use iaam_app::scenarios::import_session::{
    AnswerReach, AnswerableQuestion, AnsweredQuestions, Generalisation, HeldRow, SessionContents,
    add_rows, answer_question, read_session,
};
use iaam_app::storage::SqliteStore;
use iaam_core::ids::{AccountId, ImportQuestionId, ImportSessionId, OwnerId};
use iaam_core::money::CurrencyCode;
use iaam_ingest::classification::{Answer, FarSide};
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
const QUESTION_VARIABLE: &str = "IAAM_FAULT_TEST_QUESTION";

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
            "iaam-app-import-answer-fault-{label}-{}",
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

/// One line the caller reported without concluding it, and which no rule of
/// his settles: it raises a question, and two of them raise the same one.
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

/// One line the caller concluded: it raises no question and is here so the
/// session is not only questions.
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

/// A session holding two alike questioned rows and one concluded row, and the
/// question the first of them raised.
///
/// Seeded through the intake route rather than by writing observations directly,
/// because what this file needs is a session the answering route recognises: a
/// row, the question it raised and the answer the request that wrote them gave.
async fn seed_session(
    services: &AppServices,
    owner: OwnerId,
    account: AccountId,
) -> (ImportSessionId, ImportQuestionId) {
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
    let rows = [
        questioned(account, "row-one", date!(2026 - 01 - 02)),
        questioned(account, "row-two", date!(2026 - 01 - 03)),
        concluded(account, "answer-fault-deposit", date!(2026 - 01 - 04)),
    ];
    let held = add_rows(services, &principal(owner, Uuid::new_v4()), session, &rows)
        .await
        .expect("the intake holds the rows and asks about the two alike");
    let asked: Vec<ImportQuestionId> = held
        .iter()
        .filter_map(|row| match row {
            HeldRow::Questioned { asked } => Some(asked.question),
            HeldRow::Held { .. } | HeldRow::Settled { .. } | HeldRow::Rejected { .. } => None,
        })
        .collect();
    assert_eq!(
        asked.len(),
        2,
        "the two alike rows are the questions this answer reaches"
    );
    (session, asked[0])
}

/// What one answer says, without the identifiers and stamps a new attempt
/// mints.
fn one(question: &AnswerableQuestion) -> String {
    let generalisation = match &question.generalisation {
        Generalisation::Unanswered => "unanswered",
        Generalisation::Recorded { .. } => "recorded",
        Generalisation::Available { .. } => "available",
        Generalisation::Impossible => "impossible",
        Generalisation::DoesNotGeneralise => "does_not_generalise",
    };
    format!(
        "row {} open {} answer {:?} rule {} generalisation {generalisation} settled {} accounts \
         {} alternatives {}",
        question.view.row,
        question.view.is_open(),
        question.view.answer,
        question.view.rule.is_some(),
        question.settled_without_answer.is_some(),
        question.accounts.len(),
        question.view.alternatives,
    )
}

fn answer_meaning(answered: &AnsweredQuestions) -> Vec<String> {
    let mut lines = vec![format!("asked: {}", one(&answered.asked))];
    for question in &answered.also_settled {
        lines.push(format!("also: {}", one(question)));
    }
    lines
}

struct ChildIds {
    owner: OwnerId,
    token_id: Uuid,
    session: ImportSessionId,
    question: ImportQuestionId,
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
            question: ImportQuestionId(
                Uuid::parse_str(&std::env::var(QUESTION_VARIABLE).expect("question")).unwrap(),
            ),
        },
    ))
}

fn run_child_answer(database: &Path, ids: &ChildIds) {
    let services = services_at(database, false);
    let principal = principal(ids.owner, ids.token_id);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let _ = answer_question(
            &services,
            &principal,
            ids.session,
            ids.question,
            Answer::Refund,
            AnswerReach::EveryLikeRowInThisSession,
        )
        .await;
    });
}

#[test]
fn killed_mid_answer_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, ids) = child_parts().expect("child configuration");
    run_child_answer(&database, &ids);
}

#[test]
fn killed_after_answer_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    let (database, ids) = child_parts().expect("child configuration");
    run_child_answer(&database, &ids);
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
            (
                QUESTION_VARIABLE,
                OsStr::new(&ids.question.inner().to_string()),
            ),
        ],
    )
}

fn reopened(store: SqliteStore) -> AppServices {
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

fn open_questions(contents: &SessionContents) -> usize {
    contents
        .questions
        .iter()
        .filter(|question| question.is_open())
        .count()
}

async fn rules(services: &AppServices, owner: OwnerId) -> usize {
    services
        .rules
        .list_rules(owner)
        .await
        .expect("the rules read back")
        .len()
}

#[tokio::test]
async fn a_kill_mid_answer_leaves_none_of_it_and_the_retry_matches_a_clean_run() {
    let directory = TempDirectory::new("mid-answer");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let (session, question) = seed_session(&services, owner, account).await;

    // The request's second write, not its first: the second answer it records.
    // A request that wrote each answer, the rule and each stamp on its own
    // would leave something behind and pass a threshold of one.
    let ids = ChildIds {
        owner,
        token_id,
        session,
        question,
    };
    let KillAt { store, child } = kill(
        &directory,
        "killed_mid_answer_child",
        fault::BEFORE_COMMIT,
        Some(2),
        &database,
        &ids,
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the request's second write, before COMMIT"
    );
    let reopened = reopened(store);
    let asking = principal(owner, token_id);
    let contents = read_session(&reopened, &asking, session)
        .await
        .expect("the killed database reopens with the session");
    assert_eq!(
        contents.session.state,
        ImportSessionState::Open,
        "a kill inside the answer leaves the session open"
    );
    assert_eq!(
        open_questions(&contents),
        2,
        "a kill before COMMIT leaves neither answer, although two writes completed"
    );
    assert_eq!(
        rules(&reopened, owner).await,
        0,
        "and the rule the answers would have minted is not there"
    );

    // The identical retry, and a clean run of the same answer on a database of
    // its own: state and answer must agree by meaning.
    let retried = answer_question(
        &reopened,
        &asking,
        session,
        question,
        Answer::Refund,
        AnswerReach::EveryLikeRowInThisSession,
    )
    .await
    .expect("the retried answer records the whole decision");
    let retried_contents = read_session(&reopened, &asking, session)
        .await
        .expect("the session reads back");

    let clean_database = directory.file("clean.sqlite");
    let clean_services = services_at(&clean_database, true);
    let (clean_session, clean_question) = seed_session(&clean_services, owner, account).await;
    let clean = answer_question(
        &clean_services,
        &principal(owner, token_id),
        clean_session,
        clean_question,
        Answer::Refund,
        AnswerReach::EveryLikeRowInThisSession,
    )
    .await
    .expect("the clean run records the same decision");
    let clean_contents = read_session(&clean_services, &principal(owner, token_id), clean_session)
        .await
        .expect("the clean session reads back");

    assert_eq!(
        answer_meaning(&retried),
        answer_meaning(&clean),
        "the retry answers what a clean run answers, by meaning"
    );
    assert_eq!(
        open_questions(&retried_contents),
        0,
        "the retry settles both questions the reach names"
    );
    assert_eq!(open_questions(&clean_contents), 0, "as the clean run does");
    assert_eq!(
        rules(&reopened, owner).await,
        1,
        "and exactly one standing rule stands, as in a clean run"
    );
    assert_eq!(rules(&clean_services, owner).await, 1);
    assert_eq!(
        retried.also_settled.len() + 1,
        clean.also_settled.len() + 1,
        "the reach settled the same number of questions"
    );
}

#[tokio::test]
async fn a_kill_after_answer_answers_the_identical_retry_with_the_first_answer() {
    let directory = TempDirectory::new("after-answer");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let (session, question) = seed_session(&services, owner, account).await;

    let ids = ChildIds {
        owner,
        token_id,
        session,
        question,
    };
    let KillAt { store, child } = kill(
        &directory,
        "killed_after_answer_child",
        fault::AFTER_COMMIT,
        None,
        &database,
        &ids,
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the commit and before its answer"
    );
    let reopened = reopened(store);
    let asking = principal(owner, token_id);
    let held = read_session(&reopened, &asking, session)
        .await
        .expect("the killed database reopens with the session");
    assert_eq!(
        open_questions(&held),
        0,
        "a kill after COMMIT leaves the whole decision"
    );
    assert_eq!(rules(&reopened, owner).await, 1);

    let retried = answer_question(
        &reopened,
        &asking,
        session,
        question,
        Answer::Refund,
        AnswerReach::EveryLikeRowInThisSession,
    )
    .await
    .expect("the identical retry is answered, not refused as «already answered»");
    let after_retry = read_session(&reopened, &asking, session)
        .await
        .expect("the session reads back");
    assert_eq!(
        rules(&reopened, owner).await,
        1,
        "the retry mints no second rule"
    );
    assert_eq!(
        after_retry
            .questions
            .iter()
            .map(|held| held.answer.clone())
            .collect::<Vec<_>>(),
        held.questions
            .iter()
            .map(|held| held.answer.clone())
            .collect::<Vec<_>>(),
        "and answers nothing a second time"
    );

    // The first answer, compared with a clean run's by meaning.
    let clean_database = directory.file("clean.sqlite");
    let clean_services = services_at(&clean_database, true);
    let (clean_session, clean_question) = seed_session(&clean_services, owner, account).await;
    let clean = answer_question(
        &clean_services,
        &principal(owner, token_id),
        clean_session,
        clean_question,
        Answer::Refund,
        AnswerReach::EveryLikeRowInThisSession,
    )
    .await
    .expect("the clean run records the same decision");
    assert_eq!(
        answer_meaning(&retried),
        answer_meaning(&clean),
        "the retry returns the first answer a clean run gives, by meaning"
    );
}

#[tokio::test]
async fn the_same_question_under_another_answer_is_refused() {
    let directory = TempDirectory::new("other-answer");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let token_id = Uuid::new_v4();
    let services = services_at(&database, true);
    let (session, question) = seed_session(&services, owner, account).await;
    let asking = principal(owner, token_id);

    answer_question(
        &services,
        &asking,
        session,
        question,
        Answer::Refund,
        AnswerReach::ThisRow,
    )
    .await
    .expect("the first answer stands");

    let refused = answer_question(
        &services,
        &asking,
        session,
        question,
        Answer::Refund,
        AnswerReach::EveryLikeRowInThisSession,
    )
    .await
    .expect_err("the same question under another request is refused");
    assert!(
        matches!(&refused, iaam_app::error::AppError::Conflict { .. }),
        "the refusal says the question was answered by another request, got {refused:?}"
    );
    let contents = read_session(&services, &asking, session)
        .await
        .expect("the session reads back");
    assert_eq!(
        open_questions(&contents),
        1,
        "the refused request settled nothing beyond the first answer"
    );
    assert_eq!(
        rules(&services, owner).await,
        1,
        "the refusal minted no rule of its own"
    );
}
