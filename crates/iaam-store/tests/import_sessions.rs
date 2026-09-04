//! Import sessions: the store half of pre-journal state (iaam-3kru, iaam-6qsa).
//!
//! Nothing here is a fact, and the properties worth testing are the ones that
//! keep it that way: one session per declared import, a session that stops
//! accepting rows once it is closed, and an answer that reaches both the
//! question and the row it is about.
//!
//! Every account, label and identifier below is invented for this file.

use iaam_core::ids::{AccountId, ImportId, ImportQuestionId, ImportSessionId, OwnerId, SourceId};
use iaam_store::SqliteStore;
use iaam_store::import_session::{NewQuestion, SessionState};

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().expect("in-memory database")
}

fn asking() -> NewQuestion {
    NewQuestion {
        question: r#"{"question":"unresolved_direction"}"#.to_owned(),
        alternatives: r#"["paid","received"]"#.to_owned(),
        prompt: "Which way did the money go?".to_owned(),
    }
}

#[test]
fn a_declaration_reaches_one_session_and_an_undeclared_batch_its_own() {
    // Two sessions over one statement would split its questions across two
    // places, and the owner would answer one of them.
    let mut store = store();
    let owner = OwnerId::new_random();
    let source = SourceId::new_random();
    let import = ImportId::new_random();
    let account = AccountId::new_random();

    let first = store
        .open_import_session(owner, Some(account), Some(source), Some(import))
        .expect("session opens");
    let again = store
        .open_import_session(owner, Some(account), Some(source), Some(import))
        .expect("the same import reaches the same session");

    assert_eq!(first.id, again.id);
    assert_eq!(first.state, SessionState::Open);

    // A declaration naming a source and no label names no import, and it is
    // recognised anyway — by the source (iaam-zv54). Keying reuse on the label
    // alone recognised such a declaration by nothing and opened a fresh session
    // on every call, splitting one declaration's questions across as many
    // sessions as the caller made calls, which is the failure the reuse exists
    // to prevent. It is still a session of its own and not the one above: that
    // one names an import, and this declaration does not name it.
    let unlabelled = store
        .open_import_session(owner, Some(account), Some(source), None)
        .expect("session opens");
    assert_ne!(unlabelled.id, first.id);
    let unlabelled_again = store
        .open_import_session(owner, Some(account), Some(source), None)
        .expect("the same declaration reaches the same session");
    assert_eq!(unlabelled.id, unlabelled_again.id);

    // A batch that declared nothing at all is recognisable by nothing, so it
    // gets its own every time. That is the honest answer rather than an
    // oversight: joining two undeclared batches would join two unrelated
    // exports.
    let undeclared = store
        .open_import_session(owner, None, None, None)
        .expect("session opens");
    let undeclared_again = store
        .open_import_session(owner, None, None, None)
        .expect("session opens");
    assert_ne!(undeclared.id, undeclared_again.id);
}

/// The account a declaration named is stored, and comes back on every read
/// (iaam-tmvz).
///
/// Without it a declared session could not check the rows fed to it: `source`
/// and `import` are one-way hashes of the account, so nothing read back
/// afterwards recovers which account the caller declared, and a row for another
/// account was held and committed under this import's identity.
///
/// The three reads are asserted together because they are three different
/// queries over the same table, and a column added to one of them is exactly
/// the kind of omission that leaves the check silently off on the path that
/// forgot it.
#[test]
fn a_declared_session_remembers_the_account_it_was_declared_for() {
    let mut store = store();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let source = SourceId::new_random();
    let import = ImportId::new_random();

    let opened = store
        .open_import_session(owner, Some(account), Some(source), Some(import))
        .expect("session opens");
    assert_eq!(opened.account, Some(account));

    let loaded = store
        .load_import_session(owner, opened.id)
        .expect("session read")
        .expect("session found");
    assert_eq!(loaded.account, Some(account));

    let listed = store.list_import_sessions(owner).expect("sessions listed");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].account, Some(account));

    // Reuse returns the session that already exists, and it carries the account
    // it was opened with rather than a fresh reading of anything.
    let again = store
        .open_import_session(owner, Some(account), Some(source), Some(import))
        .expect("the same import reaches the same session");
    assert_eq!(again.id, opened.id);
    assert_eq!(again.account, Some(account));

    // A session opened without a declaration has no account, and that absence
    // is the state in which a session legitimately holds rows for several of
    // them.
    let free = store
        .open_import_session(owner, None, None, None)
        .expect("session opens");
    assert_eq!(free.account, None);
}

#[test]
fn a_row_resubmitted_under_its_own_key_occupies_the_row_it_already_had() {
    // Without this the same row opens a second question about the same money,
    // and the owner answers one of the two.
    let mut store = store();
    let owner = OwnerId::new_random();
    let session = store
        .open_import_session(owner, None, None, None)
        .expect("session opens")
        .id;

    let first = store
        .add_import_observation(owner, session, Some("idempotency/inner"), false, "{}")
        .expect("row added");
    let again = store
        .add_import_observation(owner, session, Some("idempotency/inner"), false, "{}")
        .expect("row added");

    assert_eq!(first.row, again.row);
    assert_eq!(
        store.list_import_observations(session).expect("rows").len(),
        1
    );

    // A row the caller gave nothing stable for is numbered afresh: `None` is
    // honest about what the caller supplied rather than convenient.
    store
        .add_import_observation(owner, session, None, true, "{}")
        .expect("row added");
    store
        .add_import_observation(owner, session, None, true, "{}")
        .expect("row added");
    assert_eq!(
        store.list_import_observations(session).expect("rows").len(),
        3
    );
}

#[test]
fn an_answer_reaches_both_the_question_and_the_row_it_is_about() {
    let mut store = store();
    let owner = OwnerId::new_random();
    let session = store
        .open_import_session(owner, None, None, None)
        .expect("session opens")
        .id;
    let row = store
        .add_import_observation(owner, session, Some("row/1"), false, "{}")
        .expect("row added")
        .row;
    let question = store
        .record_import_question(owner, session, row, &asking())
        .expect("question recorded");
    assert!(question.is_open());

    // One question per row: a row asked about twice would be answered twice,
    // and the two answers could differ.
    let again = store
        .record_import_question(owner, session, row, &asking())
        .expect("question recorded");
    assert_eq!(question.id, again.id);

    let answered = store
        .answer_import_question(
            owner,
            session,
            question.id,
            r#"{"answer":"paid"}"#,
            Some("rule-1"),
        )
        .expect("answer recorded");
    assert!(!answered.is_open());
    assert_eq!(answered.rule.as_deref(), Some("rule-1"));

    let rows = store.list_import_observations(session).expect("rows");
    assert_eq!(
        rows[0].answer.as_deref(),
        Some(r#"{"answer":"paid"}"#),
        "commit reads the row, so the answer has to be on it"
    );

    // Answering twice is refused rather than overwriting: the owner changing
    // their mind is a different act, carried by the rule the first answer made.
    assert!(
        store
            .answer_import_question(
                owner,
                session,
                question.id,
                r#"{"answer":"received"}"#,
                None
            )
            .is_err()
    );
}

#[test]
fn a_closed_session_takes_nothing_more_and_closes_once() {
    let mut store = store();
    let owner = OwnerId::new_random();
    let session = store
        .open_import_session(owner, None, None, None)
        .expect("session opens")
        .id;

    let abandoned = store
        .close_import_session(owner, session, SessionState::Abandoned)
        .expect("session closes");
    assert_eq!(abandoned.state, SessionState::Abandoned);
    assert!(abandoned.closed_at.is_some());

    // A row added after the session closed would sit somewhere that will never
    // be written.
    assert!(
        store
            .add_import_observation(owner, session, None, true, "{}")
            .is_err()
    );
    // And a session leaves `open` once: committing what was abandoned would
    // write facts the owner rejected.
    assert!(
        store
            .close_import_session(owner, session, SessionState::Committed)
            .is_err()
    );
}

#[test]
fn another_owners_session_is_neither_read_nor_written() {
    // A session identifier is not an access right (§14).
    let mut store = store();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let session = store
        .open_import_session(owner, None, None, None)
        .expect("session opens")
        .id;

    assert!(
        store
            .load_import_session(stranger, session)
            .expect("query runs")
            .is_none()
    );
    assert!(
        store
            .list_import_sessions(stranger)
            .expect("query runs")
            .is_empty()
    );
    assert!(
        store
            .add_import_observation(stranger, session, None, true, "{}")
            .is_err()
    );
    assert!(
        store
            .close_import_session(stranger, session, SessionState::Committed)
            .is_err()
    );
}

#[test]
fn a_payload_that_is_not_json_is_refused() {
    // A row the application cannot read must not be written silently: it would
    // become an unreadable line in an import that otherwise looks complete.
    let mut store = store();
    let owner = OwnerId::new_random();
    let session = store
        .open_import_session(owner, None, None, None)
        .expect("session opens")
        .id;

    assert!(
        store
            .add_import_observation(owner, session, None, false, "not json")
            .is_err()
    );
    assert!(
        store
            .record_import_question(
                owner,
                session,
                1,
                &NewQuestion {
                    question: "not json".to_owned(),
                    ..asking()
                }
            )
            .is_err()
    );
}

#[test]
fn a_question_that_does_not_exist_cannot_be_answered() {
    let mut store = store();
    let owner = OwnerId::new_random();
    let session = store
        .open_import_session(owner, None, None, None)
        .expect("session opens")
        .id;

    assert!(
        store
            .answer_import_question(
                owner,
                session,
                ImportQuestionId::new_random(),
                r#"{"answer":"paid"}"#,
                None
            )
            .is_err()
    );
}

#[test]
fn a_session_that_does_not_exist_is_not_found() {
    let store = store();
    let owner = OwnerId::new_random();
    assert!(
        store
            .load_import_session(owner, ImportSessionId::new_random())
            .expect("query runs")
            .is_none()
    );
}
