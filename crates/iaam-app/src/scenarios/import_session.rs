//! An import session: observations accumulate, are questioned, and commit as one
//! plan.
//!
//! **Not a database transaction, and the difference is the point.** An import
//! needs the owner to answer questions between steps, and that takes hours or
//! days. A connection held open across that blocks every other writer and does
//! not survive a restart. What is durable here is application state.
//!
//! The line the whole module keeps: **nothing in the journal is provisional, and
//! nothing provisional is in the journal.** A session is pre-journal state. It
//! never appends an event; [`commit_session`] does, once, out of what the
//! session already holds, and [`abandon_session`] does not read or write the
//! journal at all.

use std::collections::BTreeSet;

use iaam_core::batch::{
    self, BatchMovement, BatchTotal, ControlCheck, ControlComparison, ControlSection,
};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
use iaam_core::event::{Confidence, Relation, SCHEMA_VERSION};
use iaam_core::ids::{
    AccountId, EventId, ImportId, ImportQuestionId, ImportSessionId, OwnerId, SourceId,
};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_ingest::classification::{
    Answer, AnswerShape, Classification, ClassificationResult, ClassificationRule, Movement,
    Question, RuleMatcher, classify,
};
use iaam_ingest::observation::{Intake, ObservedRow};
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Rejection, SubmittedOperation, Verdict, normalize};
use sha2::{Digest, Sha256};

use crate::AppServices;
use crate::actions::{
    AccountCandidate, AccountScope, OperationKey, RequestPlan, ResolutionOption, account_scope,
    answer_account_candidates, answer_input,
};
use crate::error::{AppError, FieldRejection};
use iaam_ingest::dedup::IdentityScope;

use crate::ports::{
    AccountDetailView, AccountScopeExclusionView, AccountTransferStatementView, ContourView,
    ImportObservationView, ImportQuestionView, ImportSessionState, ImportSessionView,
    NewImportQuestion, Principal, Recorded,
};
use crate::scenarios::classification::{matcher_json, outcome_json};
use crate::scenarios::coverage_gap;
use crate::scenarios::ingest::{RowOrigin, submit_candidates};
use crate::scenarios::transfer_pairing::{self, CashLeg, LegOrigin, Proposals};

/// The durable question one row raised.
///
/// Returned beside the verdict rather than folded into it: the published verdict
/// vocabulary carries the sentence, and the identifiers are what a caller needs
/// to answer it after the response is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskedQuestion {
    pub session: ImportSessionId,
    pub question: ImportQuestionId,
    pub row: u32,
    pub prompt: String,
    pub alternatives: Vec<AnswerShape>,
}

/// One row's outcome inside a session.
///
/// Deliberately **not** a [`Verdict`]. A verdict answers "what was recorded",
/// and the answer for every row a session holds is "nothing, yet" — which is not
/// `quarantined`, whose published meaning is that no fact *could* be written
/// from the row. A held row will be written, at commit and at no other moment,
/// and saying so needs a word the verdict vocabulary does not have.
#[derive(Debug, Clone, PartialEq)]
pub enum HeldRow {
    /// Held, and nothing about it is in doubt.
    Held { row: u32 },
    /// Held, and waiting on the owner.
    Questioned { asked: AskedQuestion },
    /// Not held: the row could not be read, so there was nothing to hold.
    Rejected { row: u32, rejection: Rejection },
}

impl HeldRow {
    /// The row's position in the session, whatever became of it.
    #[must_use]
    pub const fn row(&self) -> u32 {
        match self {
            Self::Held { row } | Self::Rejected { row, .. } => *row,
            Self::Questioned { asked } => asked.row,
        }
    }
}

/// One row's outcome at intake.
#[derive(Debug, Clone, PartialEq)]
pub struct IntakeOutcome {
    pub verdict: Verdict,
    /// Present exactly when the verdict is `needs_classification`.
    pub asked: Option<AskedQuestion>,
}

/// What a session holds, for the owner to read before committing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContents {
    pub session: ImportSessionView,
    pub observations: Vec<ImportObservationView>,
    pub questions: Vec<ImportQuestionView>,
    /// The control figures the source printed about itself, where the caller
    /// transcribed them. Empty is the ordinary state of a session fed by a
    /// converter that reads only the rows.
    pub control_figures: Vec<ControlSection>,
}

impl SessionContents {
    /// Whether anything is still waiting on the owner.
    #[must_use]
    pub fn has_open_questions(&self) -> bool {
        self.questions.iter().any(ImportQuestionView::is_open)
    }
}

/// A question, and the accounts an answer to it may name.
///
/// **Why the pair exists (iaam-boj4).** Two of the answers —
/// `sent_to_own_account` and `received_from_own_account` — name one of the
/// owner's own accounts, and the question published only `needs_account: true`.
/// A client holding the question therefore had to call `GET /v1/accounts` and
/// join, which is the one identifier left on the import path that a client had
/// to fetch rather than copy out of the response it was answering. Everything
/// else was removed: a session is declared by what the source prints, and a
/// row's account is copied out of the open response.
///
/// **Why it is a pair and not a field on the store's view.** The candidates are
/// built where the answer is built, out of the account list this read of the
/// store returned, under `docs/api/conventions.md` §3.4. Joined on by the
/// transport they would come from a second reading, and one response could then
/// name one account two ways. The store's `ImportQuestionView` cannot carry them
/// either: it is what the questions table holds, and the questions table knows
/// nothing about accounts.
///
/// **Why the answer still sends an identifier.** §3.2: a name is not an
/// identity, and a request that resolved an account by title would address the
/// wrong account and succeed. So the candidate carries `title` and
/// `institution` to be read by, and `id` to be sent back — which makes the
/// answer a copy of something the client was given rather than a value it
/// composed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerableQuestion {
    pub view: ImportQuestionView,
    /// Empty in three cases a client tells apart from the rest of the answer:
    /// the question is answered (`answered_at` is set, and an answered question
    /// cannot be answered again); no alternative it offers names an account
    /// (every `needs_account` is false); or the owner has no account other than
    /// the one the row is already on, which is a true statement about his
    /// directory and not a missing lookup.
    pub accounts: Vec<AccountCandidate>,
    /// What became of the chance to turn this answer into a standing rule.
    ///
    /// Paired here rather than published beside the question for the reason the
    /// account list is paired here: a caller reading a question reads one
    /// object, and two objects assembled by two functions are two readings of
    /// one answer that can come to disagree.
    pub generalisation: Generalisation,
}

/// Pair each question with the accounts an answer to it may name.
///
/// The account list is read once, here, and only when some open question
/// actually offers an answer that names one — a session whose questions are all
/// answered, or all about a fee that no account is the other side of, costs no
/// query at all.
///
/// The candidates come from [`answer_account_candidates`], which is the function
/// the action queue's `/account` field and the answering route's own refusal
/// already use. One builder, so what the question offers, what the queue offers
/// and what the route accepts cannot drift apart — and a caller that copies an
/// id out of any of the three is copying the same list.
///
/// A question whose stored JSON cannot be read gets no candidates rather than
/// failing the read. The same tolerance the transport already applies to a
/// question's stored alternatives: what the session holds does not become
/// unreadable because one row of it is.
///
/// A free function over what [`read_session`] already returned rather than a
/// field on [`SessionContents`], because it decides nothing the session holds:
/// it is a second view of the same rows, and the callers that need only the
/// questions — the refusals, the commit planner — must not pay for it.
pub async fn answerable_questions(
    services: &AppServices,
    principal: &Principal,
    contents: &SessionContents,
    questions: &[ImportQuestionView],
) -> Result<Vec<AnswerableQuestion>, AppError> {
    let asked: Vec<Option<Question>> = questions
        .iter()
        .map(|question| {
            question
                .is_open()
                .then(|| serde_json::from_str::<Question>(&question.question).ok())
                .flatten()
                .filter(|asked| {
                    asked
                        .alternatives()
                        .into_iter()
                        .any(AnswerShape::needs_account)
                })
        })
        .collect();
    let accounts = if asked.iter().any(Option::is_some) {
        services.store.list_accounts(principal.owner).await?
    } else {
        Vec::new()
    };
    Ok(questions
        .iter()
        .zip(asked)
        .map(|(question, asked)| AnswerableQuestion {
            view: question.clone(),
            accounts: asked.map_or_else(Vec::new, |asked| {
                answer_account_candidates(&asked, &accounts)
            }),
            generalisation: generalisation_of(contents, question),
        })
        .collect())
}

/// Submit rows without naming a session.
///
/// This is the conclusive route's path, and it keeps working exactly as it did
/// for every shape it already accepted: a conclusion is normalised and recorded,
/// and the verdict is the one it always was.
///
/// What is new is the other arm. An observation the owner's rules and directory
/// settle is resolved and recorded like any other row — the caller gains a shape
/// it can state, and loses nothing. An observation they do **not** settle is
/// parked: a session addressed by this batch's declared source and import is
/// opened (or reused), the observation and its question are written there, and
/// the row comes back needing an answer with **nothing in the journal**.
///
/// Parking it is what makes the question durable. A question that existed only
/// in this response would die with it, and the owner would answer nothing.
pub async fn submit_intake(
    services: &AppServices,
    principal: &Principal,
    account: Option<AccountId>,
    source: SourceId,
    import: Option<ImportId>,
    rows: &[Intake],
) -> Result<Vec<IntakeOutcome>, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "permission to submit operations".into(),
            actual: principal.scope.code().to_owned(),
        });
    }
    let resolver = Resolver::load(services, principal.owner).await?;

    // Settle what can be settled first, so a batch with no ambiguity opens no
    // session at all: a session per import is state the owner has to look at,
    // and creating one for a batch that raised no question would fill the list
    // with nothing.
    let mut settled: Vec<Option<Result<SubmittedOperation, Rejection>>> = Vec::new();
    let mut pending: Vec<Option<(&ObservedRow, Question)>> = Vec::new();
    for intake in rows {
        match intake {
            Intake::Concluded { operation } => {
                settled.push(Some(Ok((**operation).clone())));
                pending.push(None);
            }
            Intake::Observed { row } => match resolver.assess(row) {
                Assessment::Settled {
                    classification,
                    movement,
                } => {
                    settled.push(Some(row.resolve(classification, movement)));
                    pending.push(None);
                }
                Assessment::Ambiguous { question } => {
                    settled.push(None);
                    pending.push(Some((row.as_ref(), question)));
                }
            },
        }
    }

    let session = if pending.iter().any(Option::is_some) {
        Some(
            services
                .store
                // The declared account travels into the session with the source
                // and the import. This route has already checked every row
                // against it, but the session it opens outlives the request:
                // the caller is handed its identifier and may feed it further
                // rows, and those go through [`add_rows`], which can only check
                // what the session recorded.
                .open_import_session(principal.owner, account, Some(source), import)
                .await?,
        )
    } else {
        None
    };

    let candidates: Vec<Result<iaam_core::event::Event, Rejection>> = settled
        .iter()
        .flatten()
        .map(|operation| {
            operation.clone().and_then(|operation| {
                normalize(
                    &operation,
                    NormalizationContext {
                        owner: principal.owner,
                        source,
                    },
                )
                .map(|normalized| {
                    let mut event = normalized.event;
                    if let Some(import) = import {
                        event.provenance = event.provenance.with_import(import);
                    }
                    event
                })
            })
        })
        .collect();
    // No session, even where one was opened above. A session was opened only to
    // hold the rows this batch could **not** settle; these are the ones it
    // could, and they reach the journal without ever having been in it.
    // Stamping the session on them would say the commit wrote what the
    // conclusive route did.
    let mut recorded = submit_candidates(services, principal, "operation", None, candidates)
        .await?
        .into_iter();

    let mut outcomes = Vec::with_capacity(rows.len());
    for (index, intake) in rows.iter().enumerate() {
        if settled[index].is_some() {
            let verdict = recorded.next().ok_or_else(|| {
                AppError::Store("ingestion returned fewer verdicts than rows".to_owned())
            })?;
            outcomes.push(IntakeOutcome {
                verdict,
                asked: None,
            });
            continue;
        }
        let (_, question) = pending[index].clone().ok_or_else(|| {
            AppError::Store("a row was neither settled nor questioned".to_owned())
        })?;
        let session = session.as_ref().ok_or_else(|| {
            AppError::Store("a question was raised with no session to hold it".to_owned())
        })?;
        let asked = park(
            services,
            principal.owner,
            session.id,
            intake,
            &question,
            &resolver,
        )
        .await?;
        outcomes.push(IntakeOutcome {
            verdict: Verdict::NeedsClassification {
                question: asked.prompt.clone(),
            },
            asked: Some(asked),
        });
    }
    Ok(outcomes)
}

/// Write one unsettled row and its question into the session.
async fn park(
    services: &AppServices,
    owner: OwnerId,
    session: ImportSessionId,
    intake: &Intake,
    question: &Question,
    resolver: &Resolver,
) -> Result<AskedQuestion, AppError> {
    let observation = services
        .store
        .add_import_observation(
            owner,
            session,
            intake.row_key(),
            false,
            serde_json::to_string(intake).map_err(|error| {
                AppError::Store(format!("import observation could not be written: {error}"))
            })?,
        )
        .await?;
    let prompt = resolver.render(question);
    let alternatives = question.alternatives();
    let stored = services
        .store
        .record_import_question(
            owner,
            session,
            observation.row,
            NewImportQuestion {
                question: json(question, "question")?,
                alternatives: json(&alternatives, "question alternatives")?,
                prompt: prompt.clone(),
            },
        )
        .await?;
    Ok(AskedQuestion {
        session,
        question: stored.id,
        row: stored.row,
        prompt: stored.prompt.clone(),
        alternatives,
    })
}

/// Open a session the caller will feed itself.
/// The account a batch declares, named the way its source names it.
///
/// The declaration used to take iaam's own account identifier and nothing else,
/// which cost every caller a directory read and a join before it could open a
/// session — four of them for an export holding four accounts, before a single
/// row was sent. What a statement actually prints is an account number or a
/// card, and the account already stores both (decision 0004), so this is the
/// identifier the caller already has in front of it.
///
/// The tiering is [`AccountDirectory::resolve_declared`], which is the tiering
/// the rows go through. Only the accounts are loaded: the statements and rules a
/// [`Resolver`] also carries decide nothing about which account a printed string
/// names, and loading them here would put a second, heavier read in front of
/// every declaration for no answer.
pub async fn resolve_declared_account(
    services: &AppServices,
    principal: &Principal,
    printed: &str,
) -> Result<AccountDetailView, AppError> {
    AccountDirectory::load(services, principal.owner)
        .await?
        .resolve_declared(printed)
}

/// Open a session for a declared import, or refuse because one is under way.
///
/// The store reuses rather than opening a second session for one import, and
/// that is right: two sessions over one statement would split its questions
/// across two places and the owner would answer one of them. What was wrong was
/// answering **`201 Created` with a session that already existed** — a caller
/// that reused a label for a different file was silently handed the earlier
/// session, its rows joined the ones already there, and the commit was then
/// refused over questions belonging to rows it had never sent. The refusal was
/// truthful and useless: nothing in it said which import the questions came
/// from, and nothing said that the session could be thrown away. The owner found
/// `abandon` by experiment.
///
/// So a found session is handed back only while it holds nothing. That case is a
/// caller retrying the open call — it lost the response, or it opens before
/// every batch — and nothing can be mixed into a session with no rows in it.
/// A found session that holds rows is a statement half imported, and only the
/// caller can say whether this is the same statement or another one. It is
/// refused, and the refusal carries the session, what it holds, and the two
/// calls that end it.
///
/// **Why the refusal and not a queue item.** A stale session is invisible to
/// every report: nothing it holds is in the journal, so no figure is computed
/// differently because it exists, and the queue's items are the things standing
/// between the owner and a report. The part of a stale session that *does*
/// change what a figure means — a row nobody has classified — is already an item
/// per open question, graded for exactly that reason, and a second item saying
/// «and a session holds them» would name one piece of work twice. A session held
/// open deliberately, waiting for the second bank's file so both legs of a
/// transfer can be committed together, is the documented use of the mechanism,
/// and an item telling the owner to finish it would be wrong every time he was
/// doing the thing sessions exist for. There is also nothing to attach such an
/// item to: the queue's items name an account, and a session records a source
/// and an import, both of them one-way derivations of the account, so
/// attributing one would mean reading every open session's rows on every call
/// to `/v1/actions`. The moment the owner needs to know is the moment he tries
/// to import against that import again, and that is this refusal.
///
/// The check is not serialised against the open that follows it, and does not
/// need to be: the store's unique index still admits one open session per
/// import, so the worst a race produces is the old answer — a session handed
/// back that acquired its first row in between.
///
/// `account` is what the declaration resolved to, and it is stored on the
/// session rather than left in this request. `source` and `import` are both
/// one-way hashes of it, so a session that kept only those could not say
/// afterwards which account it was declared for, and [`add_rows`] therefore
/// could not refuse a row for another one (iaam-tmvz). `None` opens a free
/// session, which holds rows for as many accounts as its export covers.
pub async fn open_session(
    services: &AppServices,
    principal: &Principal,
    account: Option<AccountId>,
    source: Option<SourceId>,
    import: Option<ImportId>,
) -> Result<ImportSessionView, AppError> {
    require_submit(principal)?;
    if let Some(standing) = standing_session(services, principal, source, import).await? {
        let contents = read_session(services, principal, standing.id).await?;
        if !contents.observations.is_empty() {
            return Err(half_imported_refusal(services, principal, &contents).await);
        }
    }
    services
        .store
        .open_import_session(principal.owner, account, source, import)
        .await
}

/// Channel a session that declared no source records its rows under.
///
/// A session the caller opened without saying where the rows came from is still
/// a way rows reached the journal, and this names it. It is distinct from
/// `file`, `paste`, `manual` and `correction` on purpose: those are things the
/// caller declares about a source outside the system, and this one is a fact
/// about how the rows got in.
pub const SESSION_CHANNEL: &str = "session";

/// The identity one session's rows arrive under, for the account a row names.
///
/// A declared session is stamped with what it declared, unchanged. An
/// **undeclared** one used to be stamped with `SourceId::new_random()`
/// (iaam-zv54), and three things followed from that:
///
/// - `POST /v1/corrections/imports` is keyed on a declaration a caller can
///   re-derive, so an undeclared session's rows were reachable only one event
///   at a time, while every other channel's are retractable as a group;
/// - deduplication is scoped by the source, so it had nothing stable to scope
///   against;
/// - and worst, because it is the one that would be hardest to debug:
///   [`plan_session`] runs a second time inside [`commit_session`], so the
///   assessment the owner read and the commit planned from it minted
///   **different** source identities. Invisible today only because
///   `PlannedFactDto` carries no source — but «what the assessment said» and
///   «what the commit wrote» differed in provenance, and the assessment is what
///   the commit is supposed to be planned from.
///
/// The derivation answers all three because it is a function of what the caller
/// already holds. The source is keyed on the account, as every source is; the
/// **import is keyed on the session identifier**, which is the label in the
/// [`ImportId::declared`] sense — the thing that names one import within an
/// account and channel. A session commits once, so one session is one import,
/// and the caller holds its identifier from the moment it opened it. Retracting
/// it is therefore the ordinary call: that account, channel `session`, label
/// the session identifier.
///
/// Per row rather than once for the session, for the reason [`RowOrigin`]
/// gives: a session may hold rows for two accounts, and a source is keyed on
/// one.
///
/// [`ImportId::declared`]: iaam_core::ids::ImportId::declared
fn session_origin(owner: OwnerId, session: &ImportSessionView, account: AccountId) -> RowOrigin {
    match session.source {
        Some(source) => RowOrigin {
            source,
            import: session.import,
        },
        None => RowOrigin {
            source: SourceId::declared(owner, account, SESSION_CHANNEL),
            import: Some(ImportId::declared(
                owner,
                account,
                SESSION_CHANNEL,
                &session.id.inner().to_string(),
            )),
        },
    }
}

/// The open session this declaration already has, if it has one.
///
/// Keyed on the whole declaration, exactly as the store's own recognition is: a
/// declaration naming an import is found by it, one naming only a source is
/// found by that, and one naming neither is found by nothing. Two layers
/// disagreeing about which session a declaration reaches would refuse one
/// import while feeding another.
///
/// Sorted oldest first rather than read in the store's listing order, which is
/// newest first: a source may have several open sessions, because the defect
/// this fixes produced them, and the store recognises the oldest.
async fn standing_session(
    services: &AppServices,
    principal: &Principal,
    source: Option<SourceId>,
    import: Option<ImportId>,
) -> Result<Option<ImportSessionView>, AppError> {
    let mut open: Vec<ImportSessionView> = services
        .store
        .list_import_sessions(principal.owner)
        .await?
        .into_iter()
        .filter(|session| session.state == ImportSessionState::Open)
        .collect();
    open.sort_by(|left, right| {
        left.opened_at
            .cmp(&right.opened_at)
            .then_with(|| left.id.inner().cmp(&right.id.inner()))
    });
    Ok(match (source, import) {
        (_, Some(import)) => open
            .into_iter()
            .find(|session| session.import == Some(import)),
        (Some(source), None) => open
            .into_iter()
            .find(|session| session.source == Some(source) && session.import.is_none()),
        (None, None) => None,
    })
}

/// The refusal that names the import already under way, and how to end it.
///
/// The field is `source.label`, and it is the honest one: the import is keyed on
/// the account, the channel and the label together, the first two are what the
/// caller means to import into, and the label is the part it varies per
/// statement. A caller importing a different file changes the label and the
/// refusal goes away; a caller importing the same file was never going to be
/// helped by a different value in any field, which is why the two calls that
/// end the session are published beside it.
///
/// Two resolutions, ordered, and the first depends on what the session is
/// waiting on. With every question answered the session can be committed, so
/// committing is offered first. With a question open it cannot — the commit
/// route refuses — so answering is offered first and the answering call is built
/// exactly as [`unanswered_refusal`] builds it. Abandoning is second in both
/// cases and never first: it is the way out, not the way on, and a refusal that
/// led with «throw this away» would invite a caller to discard rows the owner
/// spent an evening answering questions about.
async fn half_imported_refusal(
    services: &AppServices,
    principal: &Principal,
    contents: &SessionContents,
) -> AppError {
    let session = contents.session.id;
    let unanswered = contents
        .questions
        .iter()
        .filter(|question| question.is_open())
        .count();
    let rejection = FieldRejection::new(
        "source.label",
        "a label naming an import with no session open, or one of the calls that          ends the session this label already has",
        format!(
            "session {session} has been open since {opened}, holding {rows} rows and              {unanswered} unanswered questions",
            session = session.inner(),
            opened = contents.session.opened_at,
            rows = contents.observations.len(),
        ),
    );

    let mut preset = std::collections::BTreeMap::new();
    preset.insert("session".to_owned(), session.inner().to_string().into());
    let end_it = |operation| ResolutionOption {
        operation,
        request: RequestPlan {
            preset: preset.clone(),
            missing: Vec::new(),
        },
    };

    let first = match contents
        .questions
        .iter()
        .filter(|question| question.is_open())
        .min_by_key(|question| (question.row, question.id.inner()))
    {
        Some(open) => answer_resolution(services, principal, session, open.id).await,
        None => None,
    };
    rejection
        .resolved_by(vec![
            first.unwrap_or_else(|| end_it(OperationKey::CommitImportSession)),
            end_it(OperationKey::AbandonImportSession),
        ])
        .into()
}

/// Everything the session holds.
pub async fn read_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
) -> Result<SessionContents, AppError> {
    let view = services
        .store
        .load_import_session(principal.owner, session)
        .await?
        .ok_or(AppError::NotFound {
            what: "an import session",
            id: session.inner().to_string(),
        })?;
    Ok(SessionContents {
        session: view,
        observations: services
            .store
            .list_import_observations(principal.owner, session)
            .await?,
        questions: services
            .store
            .list_import_questions(principal.owner, session)
            .await?,
        control_figures: services
            .store
            .list_import_control_figures(principal.owner, session)
            .await?,
    })
}

/// Every session of the owner's, newest first.
pub async fn list_sessions(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<ImportSessionView>, AppError> {
    services.store.list_import_sessions(principal.owner).await
}

/// Feed rows into a session the caller named.
///
/// Nothing is written to the journal here, whatever the rows say — including a
/// row the caller concluded. That is the difference between this and
/// [`submit_intake`]: a session defers **everything** to commit, which is what
/// lets both legs of one transfer sit in it before either is recorded.
///
/// **A declared session takes rows for the account it declared and no other**
/// (iaam-tmvz). The batch route has always checked this because its declaration
/// and its rows arrive together; a session could not, because it stored only
/// `source` and `import`, which are one-way hashes of the account, and by the
/// time the rows arrived the account was gone. It stores the account now, and
/// this is the check that account was stored for: a row for another account
/// would be held and then committed under **this** import's identity —
/// recorded against one account while carrying the import identity of another,
/// so that retracting either import takes the wrong rows.
///
/// The check is against the account the declaration **resolved** to, not the
/// text it was written as: a caller may declare by the number its bank prints
/// while its rows name the account by its iaam identifier, which is the one
/// thing both sides can state.
///
/// It applies only where a declaration was made, and that is not a gap. A free
/// session is opened without one precisely so that an institution-wide export
/// is one session rather than four, and its rows legitimately name several
/// accounts; there is nothing for them to disagree with. A session opened
/// before the account was recorded is in the same position and is left there:
/// giving it the account of whoever feeds it next would be inventing the
/// declaration it never made.
///
/// The whole call is refused rather than the row, exactly as on the batch
/// route, and for its reason: an unreadable row is one row the caller got
/// wrong, while a row for another account contradicts the declaration the
/// session was opened under, and holding the agreeing half would leave a
/// half-import staged under an identity that names the wrong account. It is
/// refused **before** the first observation is written, so a refused call
/// leaves the session exactly as it found it.
///
/// The refusal names no request index. The rows this function receives are the
/// ones the transport could read, so a position here is a position in that
/// list and not in the caller's body; naming the offending row in prose says
/// what is true instead of an index that is off by however many rows above it
/// were unreadable.
pub async fn add_rows(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    rows: &[Intake],
) -> Result<Vec<HeldRow>, AppError> {
    require_submit(principal)?;
    // A session that cannot be found declares nothing, and the refusal for that
    // is `add_import_observation`'s own: reporting it here as well would give
    // one mistake two answers.
    let declared = services
        .store
        .load_import_session(principal.owner, session)
        .await?
        .and_then(|view| view.account);
    if let Some(declared) = declared {
        for (index, intake) in rows.iter().enumerate() {
            let named = intake.account();
            if named != declared {
                return Err(AppError::Invalid {
                    field: "operations".to_owned(),
                    expected: format!(
                        "rows for account {}, which this session was declared for",
                        declared.inner()
                    ),
                    actual: format!(
                        "row {} of this batch names account {}",
                        index + 1,
                        named.inner()
                    ),
                });
            }
        }
    }
    let resolver = Resolver::load(services, principal.owner).await?;
    let mut outcomes: Vec<HeldRow> = Vec::with_capacity(rows.len());
    for intake in rows {
        let observation = services
            .store
            .add_import_observation(
                principal.owner,
                session,
                intake.row_key(),
                intake.is_concluded(),
                serde_json::to_string(intake).map_err(|error| {
                    AppError::Store(format!("import observation could not be written: {error}"))
                })?,
            )
            .await?;
        let Intake::Observed { row } = intake else {
            outcomes.push(HeldRow::Held {
                row: observation.row,
            });
            continue;
        };
        match resolver.assess(row) {
            Assessment::Settled { .. } => outcomes.push(HeldRow::Held {
                row: observation.row,
            }),
            Assessment::Ambiguous { question } => {
                let prompt = resolver.render(&question);
                let alternatives = question.alternatives();
                let stored = services
                    .store
                    .record_import_question(
                        principal.owner,
                        session,
                        observation.row,
                        NewImportQuestion {
                            question: json(&question, "question")?,
                            alternatives: json(&alternatives, "question alternatives")?,
                            prompt,
                        },
                    )
                    .await?;
                outcomes.push(HeldRow::Questioned {
                    asked: AskedQuestion {
                        session,
                        question: stored.id,
                        row: stored.row,
                        prompt: stored.prompt,
                        alternatives,
                    },
                });
            }
        }
    }
    Ok(outcomes)
}

/// What became of the chance to turn one answer into a standing rule.
///
/// Four states, and only three of them can be true of an answered question.
///
/// **Why four and not an `Option<rule>`.** The rule identifier alone cannot say
/// why it is absent, and since the answering scope narrowed (`iaam-hnod`) it has
/// been absent for two unrelated reasons: the row offered nothing a matcher
/// could match on, or the answer arrived under a token that may not generalise.
/// A client can tell those apart, because it knows what token it holds. The
/// owner reading the session back cannot — he sees a question answered and no
/// rule — and he is the one for whom the difference is actionable.
///
/// **Why this and not a column on `import_questions`.** A column would record
/// the reason at answer time, and the reason is not what the owner needs: he
/// needs the rule. Recording «the answerer could not generalise» tells him a
/// rule is missing and leaves him to reconstruct it from the row — read the
/// observation, work out what a matcher would have asked, restate the
/// classification the answer implies. That is the expensive path, and an agent
/// that settles rows correctly while leaving it as the only way to keep those
/// settlements has made the honest path the losing one. So the state carries the
/// rule itself, and the reason falls out of which state it is.
///
/// **Why it is derived and not stored.** Every input already lives in the
/// session: the observed row is the observation's payload and the classification
/// is the stored answer, and [`matcher_for`] is the same function
/// [`answer_question`] built the written rule with. A stored copy would be a
/// second place recording one decision, and the two can disagree — the stored
/// one silently, because nothing would ever compare them. Derived, the proposal
/// is also the rule that would be created **now**, which is what a caller about
/// to post it needs; a copy frozen at answer time would offer a matcher this
/// build no longer writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Generalisation {
    /// The question is still waiting on an answer, so there is nothing to
    /// generalise yet.
    Unanswered,
    /// The answer created a standing rule, and this is its identifier.
    Recorded { rule: String },
    /// A rule was possible and none was written, because the answerer may not
    /// generalise (`iaam-hnod`). This is the rule it would have been, and the
    /// owner makes it stand with one call under his own token.
    ///
    /// It says what **this answer** wrote, not what rules now exist, and it goes
    /// on saying `available` after the owner adopts the proposal. That is
    /// deliberate: the rule he creates is his own act, recorded in his rule
    /// listing where he reads, edits and retires it, and claiming the question
    /// wrote it would attribute his decision to the import. The cost is that a
    /// second adoption writes a second identical rule, which classifies
    /// identically; the alternative — reading his rules here to see whether one
    /// now covers the row — answers a different question, since a row a rule
    /// matches is never asked about in the first place.
    Available {
        matcher: RuleMatcher,
        outcome: Classification,
    },
    /// No rule can be built from this row, under any token. A matcher that asks
    /// nothing matches nothing, and an "everything" rule would silently
    /// reclassify the portfolio — so a row carrying no counterparty, no
    /// description and no word from the source generalises into nothing, and
    /// there is no call the owner could make that would change that.
    Impossible,
}

impl Generalisation {
    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unanswered => "unanswered",
            Self::Recorded { .. } => "recorded",
            Self::Available { .. } => "available",
            Self::Impossible => "impossible",
        }
    }
}

/// What one question's answer did, or could still do, to the standing rules.
///
/// The order of the three tests is the decision. A written rule settles it
/// whatever the row says, because that rule exists and the row is no longer the
/// evidence. Then an open question, which has no answer to generalise. Only then
/// is the row consulted, and a row this build cannot read falls to `Impossible`
/// beside a row that asks nothing — which is not a fudge: «no rule can be built
/// from this» is true of both, and the assessment is where an unreadable row is
/// reported as unreadable.
fn generalisation_of(contents: &SessionContents, question: &ImportQuestionView) -> Generalisation {
    if let Some(rule) = question.rule.clone() {
        return Generalisation::Recorded { rule };
    }
    if question.is_open() {
        return Generalisation::Unanswered;
    }
    observed_row(contents, question.row)
        .ok()
        .and_then(|observed| {
            let matcher = matcher_for(&observed)?;
            let answer: Answer = question
                .answer
                .as_deref()
                .and_then(|stored| serde_json::from_str(stored).ok())?;
            Some(Generalisation::Available {
                matcher,
                outcome: answer.classification(),
            })
        })
        .unwrap_or(Generalisation::Impossible)
}

/// Record the owner's answer to one question.
///
/// Three things happen, in this order and for these reasons:
///
/// 1. The answer is checked against what the question actually offered. An
///    answer the question does not admit is a different mistake from a wrong
///    answer, and only the first can be refused.
/// 2. If the answerer may generalise, the decision is also written as a durable
///    [`ClassificationRule`], so the next import of a matching row resolves
///    without asking. See [`may_generalise`]: settling this row is import
///    mechanics, and standing rules are the owner's judgement. A row that offers
///    nothing to match on — no counterparty, no description, no word from the
///    source — gets **no** rule either way, because a matcher that asks nothing
///    matches nothing and an "everything" rule would silently reclassify the
///    portfolio.
/// 3. The answer is recorded on the question and on the row.
///
/// The journal is not touched. The answer settles what the row is; commit is
/// what records it.
///
/// What comes back is an [`AnswerableQuestion`] rather than the stored question,
/// and the difference is the whole of `iaam-ngwn`: an answerer that could not
/// write a rule is told, in the same response, what rule its answer would have
/// made. Otherwise the agent's own reply is the one place that knows a
/// generalisation was possible, and it is the place that cannot act on it.
pub async fn answer_question(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    question: ImportQuestionId,
    answer: Answer,
) -> Result<AnswerableQuestion, AppError> {
    require_submit(principal)?;
    let contents = read_session(services, principal, session).await?;
    let stored = contents
        .questions
        .iter()
        .find(|candidate| candidate.id == question)
        .ok_or(AppError::NotFound {
            what: "an import question",
            id: question.inner().to_string(),
        })?;
    let asked: Question = serde_json::from_str(&stored.question).map_err(|error| {
        AppError::Store(format!("stored import question could not be read: {error}"))
    })?;
    if !asked.admits(&answer) {
        // The shapes this question admits, as values rather than as a sentence
        // the caller would have to split on commas. They are built by the same
        // function the action queue publishes `/answer` with, so what a refusal
        // offers and what the queue offers cannot drift apart.
        //
        // Reading the owner's accounts here costs one query on a path that is
        // already refusing, and it buys the two shapes that name an account the
        // list of accounts they may name. A read that fails leaves the refusal
        // as prose rather than turning a rejected answer into a store failure:
        // what was wrong with the request does not change because the extra
        // detail could not be fetched.
        let alternatives = match services.store.list_accounts(principal.owner).await {
            Ok(accounts) => answer_input(&asked, &accounts).alternatives,
            Err(_) => Vec::new(),
        };
        return Err(FieldRejection::new(
            "answer",
            asked
                .alternatives()
                .iter()
                .map(|shape| shape.code())
                .collect::<Vec<_>>()
                .join(", "),
            answer.shape().code(),
        )
        .admitting(alternatives)
        .into());
    }
    // An answer naming an account must name one of the owner's, and it must not
    // name the account the row is already on: a transfer to itself is not a
    // movement, and the far side of an internal transfer is what a rule matches.
    if let Some(account) = named_account(answer) {
        let accounts = services.store.list_accounts(principal.owner).await?;
        if !accounts.iter().any(|known| known.id == account) {
            return Err(AppError::NotFound {
                what: "an account of the owner's",
                id: account.inner().to_string(),
            });
        }
    }

    let observed = observed_row(&contents, stored.row)?;
    // Refused before the rule is written rather than at commit: a rule created
    // for an answer the row cannot express would outlive the mistake and
    // reclassify later imports by it.
    observed
        .resolve_with(answer)
        .map_err(|rejection| AppError::Invalid {
            field: rejection.field,
            expected: rejection.expected,
            actual: rejection.actual,
        })?;

    let rule = match matcher_for(&observed).filter(|_| may_generalise(principal)) {
        Some(matcher) => Some(
            services
                .rules
                .create_rule(
                    principal.owner,
                    json(&matcher_json(&matcher), "matcher")?,
                    json(&outcome_json(answer.classification()), "outcome")?,
                    None,
                )
                .await?
                .id
                .to_string(),
        ),
        None => None,
    };

    let answered = services
        .store
        .answer_import_question(
            principal.owner,
            session,
            question,
            json(&answer, "answer")?,
            rule,
        )
        .await?;
    // Through the same pairing every other reading of a question goes through,
    // rather than an empty list and a generalisation written out here. The
    // answered question offers no candidates *because it is answered*, and what
    // its answer generalised into is one derivation; stated in two places they
    // are rules that can come to disagree with themselves.
    //
    // `contents` was read before the answer was written, and that is harmless:
    // what the generalisation consults it for is the observed row, which an
    // answer does not change.
    Ok(answerable_questions(
        services,
        principal,
        &contents,
        std::slice::from_ref(&answered),
    )
    .await?
    .pop()
    .unwrap_or(AnswerableQuestion {
        view: answered,
        accounts: Vec::new(),
        generalisation: Generalisation::Unanswered,
    }))
}

/// Record what the source printed about itself in its own control section.
///
/// **This is the answer to «why are we allowed to commit an import that is
/// knowably wrong».** Because at commit nothing knew what right looked like —
/// while the source had printed it on the same page as the rows. A statement's
/// control section is opening balance, closing balance and turnover each way,
/// and the journal has had the vocabulary for exactly those since §10.3. It has
/// only ever been able to receive them *after* the rows were written, through
/// the owner-only reconciliation route, against a journal that already held
/// whatever the import got wrong.
///
/// Four things are refused here rather than at commit, because each is a mistake
/// in the transcription and the transcriber is the only one who can fix it:
///
/// 1. A section stating nothing. It would compare against nothing and publish a
///    row of four absences, which reads as «checked» to anybody skimming.
/// 2. A section stating a figure twice for one account and currency in one call.
///    The store's upsert would silently keep whichever came last, and the caller
///    would never learn that its two readings disagreed.
/// 3. A negative turnover. Both sides are absolute values, as
///    [`ControlClaim::CashTurnover`] carries them and as a statement prints them;
///    a negative one is a sign convention misread, and letting it through would
///    turn every subsequent comparison into nonsense. A **balance** may be
///    negative and is not checked: §11 says an overdrawn account is a valid
///    state.
/// 4. An inverted interval, which [`AssertionPeriod::between`] refuses for the
///    reason it always has: it reconciles with nothing and stays a discrepancy
///    forever.
///
/// What is **not** refused is a section naming an account the owner's directory
/// does not hold. That is a finding, not a malformed request, and the assessment
/// already has a place to report it: a source printing a control section for an
/// account nobody has described is exactly the case `account_resolution.missing`
/// exists for, and refusing it here would hide it there.
///
/// Restating a section replaces it, per account and currency. A transcription
/// corrected is a correction, and the alternative — refusing the second — pins a
/// session to a typo with no way out but abandoning every row in it.
///
/// [`ControlClaim::CashTurnover`]: iaam_core::reconciliation::claim::ControlClaim::CashTurnover
pub async fn state_control_figures(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    stated: Vec<ControlSection>,
) -> Result<Vec<ControlSection>, AppError> {
    require_submit(principal)?;
    let mut seen: BTreeSet<(AccountId, CurrencyCode)> = BTreeSet::new();
    for (index, section) in stated.iter().enumerate() {
        if section.states_nothing() {
            return Err(AppError::Invalid {
                field: format!("figures[{index}]"),
                expected: "at least one of opening, closing, debit_turnover or \
                           credit_turnover"
                    .to_owned(),
                actual: "a section stating no figure".to_owned(),
            });
        }
        if !seen.insert((section.account, section.currency)) {
            return Err(AppError::Invalid {
                field: format!("figures[{index}]"),
                expected: "one section per account and currency".to_owned(),
                actual: format!(
                    "a second section for account {} in {}",
                    section.account.inner(),
                    section.currency.code()
                ),
            });
        }
        for (field, side) in [
            ("debit_turnover", section.debit_turnover),
            ("credit_turnover", section.credit_turnover),
        ] {
            if let Some(amount) = side
                && amount.raw() < 0
            {
                return Err(AppError::Invalid {
                    field: format!("figures[{index}].{field}"),
                    expected: "an absolute value: a turnover carries no sign, the side does"
                        .to_owned(),
                    actual: amount.raw().to_string(),
                });
            }
        }
    }
    services
        .store
        .state_import_control_figures(principal.owner, session, stated)
        .await
}

/// Write everything the session holds into the journal, once.
///
/// Refused while any question is unanswered. That refusal is the session's
/// purpose: committing with a question open means recording the guess the
/// question exists to prevent.
///
/// **This function does not decide what is written.** [`plan_session`] does, and
/// commit is that same function carried one step further, into
/// `submit_candidates`. The split is the whole of iaam-k1xa: an assessment
/// produced beside the import rather than by it describes a different import
/// from the one that runs, and the two drift. There is no second pass over the
/// rows here, and there must never be one.
///
/// `revision` is the stamp the plan the caller read carried. Supplied, it is
/// checked against the plan this commit just produced and a difference refuses:
/// the rows, the answers, the owner's accounts or their classification rules
/// changed between the reading and the writing, so the assessment the caller
/// approved is not what would be recorded. Omitted, the commit proceeds and the
/// outcome carries the revision it wrote under, so a caller that committed blind
/// is at least told what it committed.
///
/// `accept_control_mismatch` is how a batch that does not agree with its own
/// source's control section is committed anyway. Two disagreements pass through
/// it: a figure the rows do not come to, and — since iaam-mnv0 — rows the
/// interval that section states does not cover. One flag for both, because the
/// remedy is the same and because the second is the one that would be waved
/// through: a figure that disagrees announces itself, while a comparison folded
/// over rows the figures are not about comes out matched.
///
/// **Refusing outright was rejected.** A source's control section can itself be
/// wrong — a misprinted statement, a bank's own correction issued a week later,
/// a section covering a period the export does not — and a system that could not
/// record what happened because a bank's arithmetic was wrong is a system that
/// cannot record what happened. **Committing by default was rejected too**, and
/// that is the whole bead: the two failures this exists for, both real, were a
/// converter that mirrored every internal transfer and one that emitted minor
/// units where the rest emitted major, and each of them committed silently
/// because nothing had ever compared anything.
///
/// So the mismatch is neither a refusal nor a shrug: it is a flag whose absence
/// refuses and whose presence is a sentence the caller had to write. The refusal
/// names every figure that disagrees, with both numbers and the difference, so
/// the flag is set by somebody who has read them.
///
/// The deliberation is not stored, and does not need to be. Committing writes
/// the control figures into the journal as the assertions they are, beside the
/// rows they disagree with; the disagreement becomes a permanent, readable fact
/// that reconciliation will report as `discrepant` for as long as it stands. A
/// boolean recorded in a session table would say less and be read by nobody.
pub async fn commit_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    revision: Option<&SessionRevision>,
    accept_control_mismatch: bool,
) -> Result<CommitOutcome, AppError> {
    require_submit(principal)?;
    let planned = plan_session(services, principal, session).await?;
    if planned.plan.session.state != ImportSessionState::Open {
        return Err(AppError::Invalid {
            field: "session".to_owned(),
            expected: "an open import session".to_owned(),
            actual: planned.plan.session.state.code().to_owned(),
        });
    }
    if let Some(stated) = revision
        && *stated != planned.plan.revision
    {
        return Err(AppError::Invalid {
            field: "revision".to_owned(),
            expected: planned.plan.revision.0.clone(),
            actual: stated.0.clone(),
        });
    }
    let open = planned.plan.interpretation.open_questions.len();
    if open > 0 {
        return Err(unanswered_refusal(
            services,
            principal,
            session,
            &planned.plan.interpretation.open_questions,
        )
        .await);
    }

    if !accept_control_mismatch
        && let Some(refusal) = control_mismatch_refusal(&planned.plan.control_reconciliation)
    {
        return Err(refusal);
    }

    let verdicts = submit_candidates(
        services,
        principal,
        "operation",
        Some(session),
        planned.candidates,
    )
    .await?;
    // The assertions go in **after** the rows, and the order is not arbitrary:
    // an assertion is a statement about a period, and one written over a journal
    // that does not yet hold the period's rows is a discrepancy against an empty
    // interval. A failure between the two leaves the session open, so committing
    // again re-submits the rows — which their idempotency keys answer with
    // `duplicate` — and retries the assertions, whose own keys do the same.
    //
    // The coverage gaps go in **with** the assertions and not after them, in one
    // call. A gap says what this commit was handed and declined, and an
    // assertion recorded without the gap that qualifies it is the one
    // intermediate state that misleads: for as long as it stands alone, the
    // figures look confirmable against a journal that is short the very rows
    // this attempt dropped.
    let assertions = control_assertions(principal.owner, &planned.plan);
    let stated = assertions.len();
    let mut writing = assertions;
    writing.extend(coverage_gaps(
        principal.owner,
        &planned.plan,
        &planned.declined,
    ));
    let mut recorded = if writing.is_empty() {
        Vec::new()
    } else {
        crate::scenarios::ingest::append_checked(services, writing, IdentityScope::Source).await?
    }
    .into_iter();
    let control_assertions: Vec<Recorded> = recorded.by_ref().take(stated).collect();
    let coverage_gaps: Vec<Recorded> = recorded.collect();
    services
        .store
        .close_import_session(principal.owner, session, ImportSessionState::Committed)
        .await?;
    Ok(CommitOutcome {
        revision: planned.plan.revision,
        verdicts,
        control_assertions,
        coverage_gaps,
    })
}

/// The refusal a mismatching batch earns, or nothing.
///
/// Every disagreeing figure is named with both numbers and the difference. One
/// figure would have been shorter, and would have made the flag a guess: the
/// caller sets it to say «I have read these and I still want them recorded»,
/// and it cannot mean that about a list it was not shown. This is the opposite
/// choice from [`unanswered_refusal`], which offers one question of many — and
/// for the opposite reason: there the remedy is a separate call per question and
/// a hundred of them would be a hundred requests, while here the remedy is one
/// flag on this same call, and the list is what the flag is about.
fn control_mismatch_refusal(control: &ControlReconciliation) -> Option<AppError> {
    let mut disagreements: Vec<String> = control
        .comparisons
        .iter()
        .flat_map(|comparison| {
            comparison
                .checks
                .iter()
                .filter_map(move |check| match check {
                    ControlCheck::Mismatched {
                        figure,
                        claimed,
                        observed,
                        delta,
                    } => Some(format!(
                        "{} on account {} in {}: the source states {}, the rows come to {} \
                     (difference {})",
                        figure.code(),
                        comparison.account.inner(),
                        comparison.currency.code(),
                        decimal(*claimed, comparison.currency),
                        decimal(*observed, comparison.currency),
                        decimal(*delta, comparison.currency),
                    )),
                    ControlCheck::Matched { .. } | ControlCheck::NotChecked { .. } => None,
                })
        })
        .collect();
    // Beside the figures, and in the same list, the rows the stated interval
    // does not cover (iaam-mnv0). It belongs here rather than in a refusal of
    // its own because the remedy is the same flag on the same call, and a
    // caller shown one list and refused over another would set the flag without
    // having read what it was about. The sentence names both numbers for the
    // same reason the figures do: the flag says «I have read these».
    disagreements.extend(control.comparisons.iter().filter_map(|comparison| {
        let (fit, stated) = (comparison.fit?, comparison.stated?);
        if fit.fits() {
            return None;
        }
        let mut how = Vec::new();
        if let Some((from, to)) = fit.span {
            how.push(format!("{} dated {from} to {to}", fit.outside));
        }
        if fit.undated > 0 {
            how.push(format!("{} carrying no date", fit.undated));
        }
        Some(format!(
            "account {} in {}: the source states {} to {}, and {} of the rows folded into \
             that comparison are not covered by it — {}",
            comparison.account.inner(),
            comparison.currency.code(),
            stated.period.from,
            stated.period.to,
            fit.misplaced(),
            how.join(", "),
        ))
    }));
    if disagreements.is_empty() {
        return None;
    }
    Some(AppError::Invalid {
        field: "accept_control_mismatch".to_owned(),
        expected: "rows that add up to the control figures the source printed and fall \
                       inside the interval it states, or accept_control_mismatch: true to \
                       record them as they are"
            .to_owned(),
        actual: format!(
            "{} disagreement(s) with the source's own control section: {}",
            disagreements.len(),
            disagreements.join("; ")
        ),
    })
}

/// An amount in minor units as a decimal string.
///
/// [`Money::to_calc_dec`] and not arithmetic: the same value in a different
/// representation, which is the one transition §3.4 allows. A refusal that
/// printed raw minor units would be asking the reader to divide by a hundred
/// before deciding whether to override a check.
fn decimal(amount: PostedMinor, currency: CurrencyCode) -> String {
    Money::new(amount, currency)
        .to_calc_dec()
        .inner()
        .to_string()
}

/// The commit refusal, carrying the call that lifts it.
///
/// The one rejection in this crate whose remedy genuinely is another request
/// rather than another value: nothing may be written in the `session` field
/// that makes an unanswered question answered. So the refusal publishes the
/// answering call itself — the operation, the two path segments already known,
/// and the `/answer` field with the shapes that one question admits — in the
/// shape the action queue publishes a resolution with.
///
/// **One call, not one per open question.** The caller must answer all of them
/// before committing, and it discovers the next by committing again or by
/// reading the queue, which is the place that lists outstanding work. A refusal
/// that reprinted the whole backlog would be a report, and a session holding a
/// hundred questions would answer a rejected commit with a hundred requests.
/// The first by row order is the one offered, so two identical refusals name the
/// same question.
///
/// Every read here is on a path that has already decided to refuse. A failing
/// read costs the caller the resolution, not the refusal: the commit was
/// invalid before the read was attempted, and reporting a store failure instead
/// would send the caller to retry a request that cannot succeed.
async fn unanswered_refusal(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    open_questions: &[OpenQuestion],
) -> AppError {
    let rejection = FieldRejection::new(
        "session",
        "every question answered before the import is committed",
        format!("{} unanswered", open_questions.len()),
    );
    let Some(first) = open_questions
        .iter()
        .min_by_key(|question| (question.row, question.question.inner()))
    else {
        return rejection.into();
    };
    let Some(resolution) = answer_resolution(services, principal, session, first.question).await
    else {
        return rejection.into();
    };
    rejection.resolved_by(vec![resolution]).into()
}

/// The call that answers one question, addressed and pre-filled.
///
/// Shared by the two refusals that offer it — a commit refused for an unanswered
/// question, and an import refused because the session standing in its way is
/// waiting on one. Written once because the two must publish the same call: a
/// caller that met both would otherwise be told to answer the same question in
/// two shapes, and one of them would be the stale one.
///
/// `None` rather than an error on every failure below. The only callers are
/// refusals that are already decided, and a session that cannot be read or a
/// question whose stored JSON will not parse costs the refusal its next call and
/// nothing else.
async fn answer_resolution(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    question: ImportQuestionId,
) -> Option<ResolutionOption> {
    let asked = read_asked_question(services, principal, session, question).await?;
    let accounts = services
        .store
        .list_accounts(principal.owner)
        .await
        .unwrap_or_default();

    let mut preset = std::collections::BTreeMap::new();
    // Both are path segments of the answering route, and both are known here:
    // the session is the one in hand and the question is the one being named.
    preset.insert("session".to_owned(), session.inner().to_string().into());
    preset.insert("question".to_owned(), question.inner().to_string().into());

    Some(ResolutionOption {
        operation: OperationKey::AnswerImportQuestion,
        request: RequestPlan {
            preset,
            missing: vec![answer_input(&asked, &accounts)],
        },
    })
}

/// The typed question behind one identifier, or nothing.
///
/// `Option` rather than `Result` on purpose: the only caller is a refusal that
/// is already decided, and every way this can fail — the session unreadable, the
/// question gone, its stored JSON unparseable — costs that refusal its extra
/// detail and nothing else.
async fn read_asked_question(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    question: ImportQuestionId,
) -> Option<Question> {
    let contents = read_session(services, principal, session).await.ok()?;
    let stored = contents
        .questions
        .iter()
        .find(|candidate| candidate.id == question)?;
    serde_json::from_str(&stored.question).ok()
}

/// What committing wrote, and under which reading of the session.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitOutcome {
    /// The revision the commit was planned from. A caller that supplied one has
    /// it echoed; a caller that supplied none learns what it committed.
    pub revision: SessionRevision,
    /// A verdict per held row, in the order the rows were fed.
    pub verdicts: Vec<Verdict>,
    /// The control assertions written out of the source's own control section.
    ///
    /// Reported apart from `verdicts` rather than appended to it, because
    /// `verdicts` is a verdict **per row** and a caller reads it by position. An
    /// assertion is not any row's outcome, and putting one at the end of that
    /// list would give the last row of every import a neighbour that is not a
    /// row.
    pub control_assertions: Vec<Recorded>,
    /// The gaps recording what this commit was handed and did not take
    /// (iaam-bufs).
    ///
    /// Empty on the ordinary commit, which declines nothing, and empty on a
    /// commit whose source printed no control section — see [`coverage_gaps`]
    /// for why a gap needs a stated interval to be dated by.
    ///
    /// Reported apart from `control_assertions` because they are opposite
    /// statements written together: an assertion is what the source claims, a
    /// gap is what this attempt could not stand behind, and the whole point of
    /// writing both is that a reader of the journal later sees the second
    /// beside the first.
    pub coverage_gaps: Vec<Recorded>,
}

// ---------------------------------------------------------------------------
// The assessment (iaam-k1xa)
// ---------------------------------------------------------------------------

/// A reading of the session, stamped by the plan that produced it.
///
/// Opaque on purpose: it is a fingerprint of the plan, and a caller that parsed
/// it would be depending on how the plan is rendered rather than on what it
/// says.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionRevision(pub String);

/// What an import will and will not record, and what it is still waiting on.
///
/// Produced by [`plan_session`], which [`commit_session`] then carries one step
/// further. The defect this answers is an import that committed before anything
/// said what it would record: the reporter's rows arrived with positive verdicts
/// and part of them absent from the report he was shown, and nothing in between
/// had ever said so.
///
/// The eight sections are not a summary of the rows. Each answers a question the
/// owner had no way to ask before committing, and they are separate because the
/// answers can disagree — a row can be interpretable and outside the contour, or
/// resolved and a duplicate.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPlan {
    pub session: ImportSessionView,
    /// The stamp [`commit_session`] checks. See [`SessionRevision`].
    pub revision: SessionRevision,
    pub source_inventory: SourceInventory,
    pub account_resolution: AccountResolution,
    pub scope_assessment: ScopeAssessment,
    pub interpretation: Interpretation,
    pub cross_source_matching: Proposals,
    pub commit_delta: CommitDelta,
    /// The batch checked against the control figures its own source printed.
    pub control_reconciliation: ControlReconciliation,
    pub readiness: Readiness,
}

/// What the source said about itself, and whether the rows agree.
///
/// The eighth section, and the only one that compares two numbers rather than
/// describing one. Every other section reports what the import *is*; this one
/// reports whether it *adds up*, against a figure the source printed itself.
///
/// Empty comparisons is the ordinary state of a session whose converter reads
/// only rows, and it says exactly that: nothing was compared. That is worth
/// publishing — «agreed» and «never checked» are different answers, and an
/// import that could not be checked should not read like one that passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlReconciliation {
    /// One entry per account and currency named by a control section or moved by
    /// a row, in account and currency order.
    pub comparisons: Vec<ControlComparison>,
}

impl ControlReconciliation {
    /// How many stated figures disagree with the rows.
    ///
    /// A figure that could not be checked is not counted, for §10.4's reason:
    /// «nothing to compare against» is not «the numbers do not match», and
    /// counting it would refuse an import because its source printed less than
    /// another source prints.
    #[must_use]
    pub fn mismatches(&self) -> usize {
        self.comparisons
            .iter()
            .flat_map(|comparison| comparison.checks.iter())
            .filter(|check| check.is_mismatch())
            .count()
    }

    /// How many folded rows the stated intervals do not place inside themselves
    /// (iaam-mnv0).
    ///
    /// Counted apart from [`Self::mismatches`] because it is a different fault,
    /// and reported beside it because it reaches the same door. A mismatch says
    /// two numbers disagree; this says the numbers were computed over rows the
    /// figures are not about — and a comparison folded over the wrong set of
    /// rows is worse than one that disagrees, because it comes out clean.
    ///
    /// A comparison the source stated nothing for contributes nothing: with no
    /// interval there is nothing for a row to be outside of, exactly as a
    /// figure nobody stated is not a figure that failed.
    #[must_use]
    pub fn misplaced_rows(&self) -> usize {
        self.comparisons
            .iter()
            .filter_map(|comparison| comparison.fit)
            .map(|fit| fit.misplaced())
            .sum()
    }
}

/// The plan, and the events it would append.
///
/// The candidates are not published: they carry freshly minted identifiers that
/// mean nothing until they are written, and a caller holding them would believe
/// it knew what the journal was about to contain.
#[derive(Debug)]
pub struct PlannedSession {
    pub plan: ImportPlan,
    candidates: Vec<Result<iaam_core::event::Event, Rejection>>,
    /// The rows the commit will decline, named as a coverage gap names them.
    ///
    /// Private for `candidates`' reason and one more of its own. The names
    /// carry the session's source, which a caller holding them would read as
    /// an identity it can address; and what these rows *are* is already
    /// published, per row and with the reason each was declined, as
    /// `commit_delta.retained_unrecorded`. This is that same fact in the shape
    /// the journal records it, not a second finding.
    declined: Vec<DeclinedRow>,
}

// No source is held here. It was, while a session that declared none had one
// minted for the occasion and three writers minting it separately would have
// given one commit three sources. `session_origin` derives it instead, from the
// owner, the session and the account a row names (`iaam-zv54`), so the rows, the
// control assertions and the coverage gaps reach the same identity without
// anybody carrying it between them — and a refused row is named under the
// identity it would itself have been written under.

/// One row a commit was handed and will not take.
///
/// The account is beside the row rather than inside it because a gap is an
/// event, and an event names an account: the rows are grouped by it. `None`
/// where the stored payload does not parse, which is the one case in which a
/// row names nothing this build can read.
#[derive(Debug, Clone)]
struct DeclinedRow {
    account: Option<AccountId>,
    row: RefusedRow,
}

/// What the session's own source named.
///
/// Not what the source *is* — that is the declaration, and it is above — but
/// what its rows turned out to name once read. A statement that declares one
/// account and carries rows for another is the failure the acceptance of this
/// import must show before it happens rather than after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInventory {
    pub source: Option<SourceId>,
    pub import: Option<ImportId>,
    /// Documents the rows name, deduplicated, in first-seen order.
    pub documents: Vec<String>,
    pub rows: usize,
    /// The earliest and latest day any row states, when any row states one.
    pub period: Option<(time::Date, time::Date)>,
    /// Accounts the rows are on, deduplicated.
    pub accounts: Vec<AccountId>,
}

/// What the rows' accounts resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResolution {
    /// Named by a row and held by the owner's directory.
    pub resolved: Vec<AccountId>,
    /// Named by a row and **not** in the owner's directory. Every fact built on
    /// one of these is a fact about an account the owner has never described.
    pub missing: Vec<AccountId>,
    /// Counterparty strings that name more than one of the owner's accounts.
    /// Reported rather than resolved: picking one is the guess the whole module
    /// refuses.
    pub conflicting: Vec<String>,
}

/// Where each account the rows name stands relative to the reporting perimeter.
///
/// The three dispositions are [`crate::actions::AccountScope`]'s, read through
/// it rather than recomputed: «inside» is derived from contour composition and
/// «outside» is a statement the owner recorded, and a second implementation of
/// that pair would let this assessment and the queue disagree about the same
/// account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeAssessment {
    /// Named by the latest version of at least one contour.
    pub in_contour: Vec<AccountId>,
    /// The owner has ruled it outside every contour, and said why.
    pub explicitly_outside: Vec<AccountId>,
    /// Neither, and the state a newly created account is in. Rows on such an
    /// account are recorded and then appear in no contour's report — which is
    /// exactly the shape of the reporter's complaint.
    pub awaiting_disposition: Vec<AccountId>,
}

/// What each row was read as, and what is still unread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interpretation {
    pub resolved: Vec<PlannedFact>,
    pub open_questions: Vec<OpenQuestion>,
}

/// One question the session is still waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenQuestion {
    pub row: u32,
    pub question: ImportQuestionId,
    pub prompt: String,
}

/// One fact the commit would write, described without writing it.
///
/// The event identifier is deliberately absent: it is minted at commit, and a
/// plan that carried one would be naming a fact that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFact {
    pub row: u32,
    /// The account the row is on — not necessarily the event's own account: a
    /// transfer that arrived is submitted from the sending side, and the row is
    /// still the receiving statement's row.
    pub account: AccountId,
    /// The event kind, in the journal's own vocabulary.
    pub records_as: &'static str,
    /// The cash this row moves on its own account, signed as the journal will
    /// record it. Zero where the fact moves no cash on that account.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    pub date: Option<time::Date>,
    pub idempotency_key: Option<String>,
}

/// What the journal gains, and what it does not.
///
/// Every list here is a list of rows, and beside two of them is what those rows
/// come to. The totals answer the question the lists cannot: an operator
/// checking a two-hundred-row import against the figure printed on his statement
/// has one number on the statement and two hundred decimal strings here, and
/// adding them up is arithmetic — which belongs in the core (§3.1, §13) and not
/// in a client that is a language model.
///
/// They decide nothing and refuse nothing: they let a reader compare one number
/// with one number, and what he does about a difference is his business. A
/// source that printed no control section at all still leaves him a figure to
/// check by hand against the statement in front of him.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDelta {
    /// Facts that would be appended.
    pub facts: Vec<PlannedFact>,
    /// Rows whose idempotency key the owner's journal already holds. They commit
    /// to a `duplicate` verdict and add nothing — which is correct, and is also
    /// exactly what «every verdict was positive and half the rows are not in the
    /// report» looked like from the outside.
    pub duplicates: Vec<PlannedFact>,
    /// Rows the session keeps and the journal will not receive.
    pub retained_unrecorded: Vec<RetainedRow>,
    /// The provenance each account's `facts` will be written under.
    ///
    /// **Per account, because that is where the answer is true.** The origin is
    /// `session_origin`'s, derived from the owner, the session and the account
    /// a row names, and a session that declared nothing may hold rows for
    /// several accounts — which is what a free session is for, since an export
    /// covering a whole institution is one session and not one per account. A
    /// single source at the top of the plan would therefore be a lie for
    /// exactly the sessions that need it most. The grouping is the one
    /// [`CommitDelta::fact_totals`] already uses, minus the currency, which
    /// nothing about a source depends on.
    ///
    /// **Facts only, not duplicates.** A duplicate appends nothing, so there is
    /// no provenance it will be written under; the row it collides with was
    /// written by some earlier submission, quite possibly under another import
    /// entirely, and publishing this session's identity for it would name a
    /// group a retraction here would not take.
    ///
    /// It is published because a plan that says what will be written and not
    /// what it will be written under cannot be checked against the one question
    /// a pre-commit review exists to settle: if this turns out to be wrong,
    /// what does taking it back have to name. The information was there all
    /// along — the commit derives it from the very same call — and only the
    /// publishing was missing.
    pub fact_origins: Vec<PlannedOrigin>,
    /// What `facts` come to, per account and currency.
    pub fact_totals: Vec<BatchTotal>,
    /// What `duplicates` come to, per account and currency.
    ///
    /// Totalled separately rather than folded in with the facts, because the two
    /// answer different questions. The facts' total is what the journal gains;
    /// this one is what the source stated and the journal already holds, and it
    /// is the figure that explains a statement whose turnover exceeds what the
    /// import adds. Summed together they would be neither.
    pub duplicate_totals: Vec<BatchTotal>,
}

/// The identity one account's planned facts will carry into the journal.
///
/// Both halves travel together for [`RowOrigin`]'s reason, and they answer two
/// different questions: the source says which channel of which account these
/// rows come from and is what deduplication is scoped by, and the import says
/// which submission carried them and is what a retraction is keyed on. A plan
/// that published only the first would let a reader work out the channel and
/// still not know whether taking this import back would take one statement or
/// every statement that account ever arrived by.
///
/// `import` is `None` where the session declared a source without a label:
/// those rows are retracted together with every other unlabelled row of the
/// same account and channel, and that is what naming no label means, not an
/// identity still to be assigned. A session that declared nothing at all always
/// has one, because `session_origin` derives it from the session identifier —
/// which the caller has held since it opened the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedOrigin {
    pub account: AccountId,
    pub source: SourceId,
    pub import: Option<ImportId>,
}

/// A row that stays in the session and becomes no fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedRow {
    pub row: u32,
    pub reason: RetentionReason,
}

/// Why a row records nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionReason {
    /// The row could not be read into a fact at all.
    Unreadable {
        field: String,
        expected: String,
        actual: String,
    },
    /// The row raised a question the owner has not answered.
    Unanswered { question: ImportQuestionId },
}

/// Whether this import can proceed, and on whose word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// Every row is settled and nothing is waiting on anybody.
    Ready,
    /// The session cannot commit at all: it is already committed or abandoned.
    Blocked { reason: String },
    /// It could commit, and something the owner alone can settle would change
    /// what it writes. Unanswered questions refuse the commit; unconfirmed
    /// transfer candidates do not — leaving them unconfirmed records the two
    /// legs separately, which is the state the journal is in today and is not a
    /// fabrication. It is still reported, because committing without looking is
    /// how the two legs came to be unrelated in the first place.
    RequiresOwnerDecision {
        unanswered_questions: usize,
        transfer_candidates: usize,
    },
    /// Every row is readable and answered, and the batch does not agree with the
    /// control figures its own source printed.
    ///
    /// A third reason beside the other two because it is a third kind of thing.
    /// `blocked` is a session that cannot commit at all; `requires_owner_decision`
    /// is a question only the owner can settle. This is neither: every row was
    /// read, nothing is waiting on anybody, and the arithmetic does not come out.
    /// Reported as `requires_owner_decision` it would send the owner looking for
    /// a question to answer, and there is none — what there is, is a batch that
    /// does not add up, and either the reading of the document or the document
    /// itself is wrong.
    ///
    /// It does **not** refuse the commit, and that is deliberate: a source's own
    /// control section can itself be wrong, and a system that could not record a
    /// statement its bank misprinted would be a system that cannot record what
    /// happened. What it does is make committing a stated act — see
    /// [`commit_session`].
    ///
    /// **Two disagreements, one word** (iaam-mnv0). A figure that disagrees and
    /// a row the stated interval does not cover are reported together because
    /// they reach the same remedy: one flag on the commit call, set by somebody
    /// who has read the list. Splitting them would mean a caller lifting one and
    /// being refused for the other, and the second is exactly the kind that gets
    /// waved through. `misplaced_rows` is not the lesser of the two: a figure
    /// that disagrees announces itself, while a comparison folded over rows the
    /// figures are not about comes out **clean** — the turnover matches because
    /// the same rows were folded on both sides of nothing.
    DoesNotReconcile {
        mismatched_figures: usize,
        /// Rows folded into a comparison that the interval its source stated
        /// does not place inside itself: dated outside it, or carrying no date
        /// to be placed by.
        misplaced_rows: usize,
    },
}

impl Readiness {
    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked { .. } => "blocked",
            Self::RequiresOwnerDecision { .. } => "requires_owner_decision",
            Self::DoesNotReconcile { .. } => "does_not_reconcile",
        }
    }
}

/// Read the session and say what committing it would do.
///
/// Everything [`commit_session`] needs is decided here, and nothing is written.
/// The whole of iaam-k1xa is that this is one function rather than two: a
/// preview written beside the import is a second implementation of it, and it
/// describes a different import from the one that runs.
///
/// # What the assessment deliberately does not say: that an amount looks wrong
///
/// Proposed and refused (iaam-p3az), and recorded here because this is the
/// function somebody would add it to.
///
/// The occasion was real. One branch of a converter emitted minor units where
/// every other branch emitted major, the journal then held figures orders of
/// magnitude apart on one account, and nothing in this plan remarked on it; it
/// was found by a person reading the numbers. The proposal was to mark a row
/// whose magnitude is out of keeping with its neighbours — per account and
/// currency, over the rows of one session — never refusing, never rescaling,
/// only marking and saying why.
///
/// The case for it is the assessment's own case, and it is not weak: this is the
/// one place whose entire purpose is *here is what is about to be written, look
/// at it before it is*, and a statement about the shape of a batch is the kind
/// of statement the sections above already make.
///
/// It is refused because a magnitude outlier is not a comparison. It compares a
/// row against a distribution nobody asserted, and this system's rule is that it
/// says what it compared and states nothing it merely assumed. Three
/// consequences follow, and the third is what decides it:
///
/// - **It is wrong on ordinary data.** A month's rent beside a month's card
///   purchases is an outlier by any measure and is not a defect; so is one
///   transfer between the owner's own banks, which the pairing section already
///   publishes as a candidate for its own reasons. A mark that fires on those
///   teaches its reader to skip the section it lives in, and the sections it
///   lives beside are the ones that are never a guess.
/// - **No threshold makes it honest.** Anything loose enough to stay quiet on
///   the case above is also quiet on a hundredfold scale error inside a small
///   row — which is the error it was proposed for. The signal is tuned by what
///   it must not fire on, and that is the wrong end.
/// - **The same defect is caught by comparison instead.** A session that carries
///   the statement's own control figures (iaam-jc3y) checks the planned facts
///   against the source's arithmetic, and a converter emitting minor units fails
///   that check by exactly the factor of its bug, with the source's own number
///   to name in the refusal. A heuristic beside a check is a second and weaker
///   answer to a question already answered, and the two can disagree in front of
///   the owner.
///
/// What would reopen it is a source that prints no control section at all,
/// together with an import of one that went wrong this way. Even then the move
/// is probably not a verdict: every planned fact is already published here with
/// its amount and its currency, so what such a source lacks is a reader, not a
/// suspicion computed on its behalf.
pub async fn plan_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
) -> Result<PlannedSession, AppError> {
    let contents = read_session(services, principal, session).await?;
    let resolver = Resolver::load(services, principal.owner).await?;
    let contours = services.store.list_contours(principal.owner).await?;
    let exclusions = services
        .store
        .list_account_scope_exclusions(principal.owner)
        .await?;
    // The keys the journal already holds, so the plan can say which rows will
    // commit to `duplicate` rather than to a fact. Read here rather than at
    // commit for the reason the whole split exists: what commit will do is what
    // the owner is entitled to read before it does it.
    let recorded_keys: BTreeSet<String> = services
        .store
        .load_events_through(principal.owner, time::Date::MAX)
        .await?
        .into_iter()
        .filter_map(|event| event.idempotency_key)
        .collect();

    let mut candidates = Vec::with_capacity(contents.observations.len());
    let mut read_rows = Vec::with_capacity(contents.observations.len());
    for observation in &contents.observations {
        let intake = parse_intake(&observation.payload).ok();
        let operation = operation_of(observation, &resolver);
        // The origin is derived from the session and the account this row names,
        // so that this function called twice — which is exactly what committing
        // does — plans the same provenance both times. See [`session_origin`].
        // The operation itself is kept beside the candidate because a row this
        // commit declines is named in the coverage gap by what it would have
        // moved (iaam-bufs), and a candidate that failed to normalise no longer
        // says.
        let candidate = operation.clone().and_then(|operation| {
            let origin = session_origin(principal.owner, &contents.session, operation.account);
            normalize(
                &operation,
                NormalizationContext {
                    owner: principal.owner,
                    source: origin.source,
                },
            )
            .map(|normalized| {
                let mut event = normalized.event;
                if let Some(import) = origin.import {
                    event.provenance = event.provenance.with_import(import);
                }
                event
            })
        });
        read_rows.push(ReadRow {
            row: observation.row,
            intake,
            operation: operation.ok(),
            row_key: observation.row_key.clone(),
            payload: observation.payload.clone(),
            candidate: candidate.clone(),
        });
        candidates.push(candidate);
    }

    let source_inventory = inventory(&contents, &read_rows);
    let account_resolution = account_resolution(&resolver, &read_rows);
    let scope_assessment = scope_assessment(&source_inventory.accounts, &contours, &exclusions);
    let open_questions: Vec<OpenQuestion> = contents
        .questions
        .iter()
        .filter(|question| question.is_open())
        .map(|question| OpenQuestion {
            row: question.row,
            question: question.id,
            prompt: question.prompt.clone(),
        })
        .collect();

    let mut facts = Vec::new();
    let mut duplicates = Vec::new();
    let mut retained = Vec::new();
    // Rows this commit was handed and will not take, in the shape a coverage
    // gap names them (iaam-bufs). Collected in the same pass as everything
    // else, for the reason the whole planner is one function: a second walk
    // over the rows would describe a different import from the one that runs.
    let mut declined: Vec<DeclinedRow> = Vec::new();
    for read in &read_rows {
        match &read.candidate {
            Ok(event) => {
                let fact = planned_fact(read, event);
                if fact
                    .idempotency_key
                    .as_deref()
                    .is_some_and(|key| recorded_keys.contains(key))
                {
                    duplicates.push(fact);
                } else {
                    facts.push(fact);
                }
            }
            Err(rejection) => {
                let open = open_questions.iter().find(|open| open.row == read.row);
                let reason = match open {
                    Some(open) => RetentionReason::Unanswered {
                        question: open.question,
                    },
                    None => {
                        // Only an unreadable row is declined. An unanswered one
                        // refuses the commit outright, so no commit ever
                        // declines it — recording a gap for it would be a
                        // statement about an attempt that did not happen.
                        declined.push(declined_row(principal.owner, &contents.session, read));
                        RetentionReason::Unreadable {
                            field: rejection.field.clone(),
                            expected: rejection.expected.clone(),
                            actual: rejection.actual.clone(),
                        }
                    }
                };
                retained.push(RetainedRow {
                    row: read.row,
                    reason,
                });
            }
        }
    }

    let resolved: Vec<PlannedFact> = facts.iter().chain(duplicates.iter()).cloned().collect();
    // The same matching pass the journal-level proposal runs, over the rows this
    // session is about to write. One function, so an owner shown a candidate
    // here is shown the same candidate after the commit.
    let legs: Vec<CashLeg> = read_rows
        .iter()
        .filter_map(|read| {
            let event = read.candidate.as_ref().ok()?;
            let mut leg = transfer_pairing::leg_of_event(event)?;
            leg.origin = LegOrigin::Observed {
                session,
                row: read.row,
            };
            Some(leg)
        })
        .collect();
    let cross_source_matching = transfer_pairing::propose(&legs);

    let commit_delta = CommitDelta {
        fact_origins: fact_origins(principal.owner, &contents.session, &facts),
        fact_totals: batch_totals(&facts)?,
        duplicate_totals: batch_totals(&duplicates)?,
        facts,
        duplicates,
        retained_unrecorded: retained,
    };
    // Compared against the facts **and** the duplicates. A statement's turnover
    // covers every row it printed, including the ones this journal already
    // holds under their key; checking the facts alone would report a shortfall
    // on every re-import of a statement that overlaps the last one, which is the
    // ordinary shape of a monthly export.
    let stated: Vec<ControlSection> = contents.control_figures.clone();
    let control_reconciliation = ControlReconciliation {
        comparisons: batch::compare(
            &stated,
            &movements(
                &commit_delta
                    .facts
                    .iter()
                    .chain(commit_delta.duplicates.iter())
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
        )
        .map_err(AppError::BatchTotal)?,
    };

    // The order is the decision, and it is by how firmly the door is shut.
    //
    // `blocked` first: a session that has left «open» commits nothing, whatever
    // else is true of it. Then unanswered questions, which **refuse** the
    // commit — and which are also a complete explanation of a shortfall, since
    // an unread row is a row missing from every total; naming a mismatch here
    // would send the owner to check arithmetic when what is missing is an
    // answer. Then the mismatch, which commits only deliberately — and beside
    // it, on the same word, a row the stated interval does not cover: it is the
    // same finding wearing another shape, that the figures and the rows are not
    // about the same thing, and it reaches the same flag on the same call. Last
    // the unconfirmed transfer candidates, which commit by default and change
    // what the journal *relates*, not what it holds.
    //
    // Nothing is hidden by the ordering: every section is published whatever the
    // readiness says, and `control_reconciliation` states both numbers of every
    // comparison it made. What the ordering decides is the one word.
    let mismatched_figures = control_reconciliation.mismatches();
    let misplaced_rows = control_reconciliation.misplaced_rows();
    let readiness = if contents.session.state != ImportSessionState::Open {
        Readiness::Blocked {
            reason: format!(
                "the session is {}, and a session leaves «open» once",
                contents.session.state.code()
            ),
        }
    } else if !open_questions.is_empty() {
        Readiness::RequiresOwnerDecision {
            unanswered_questions: open_questions.len(),
            transfer_candidates: cross_source_matching.candidates.len(),
        }
    } else if mismatched_figures > 0 || misplaced_rows > 0 {
        Readiness::DoesNotReconcile {
            mismatched_figures,
            misplaced_rows,
        }
    } else if !cross_source_matching.candidates.is_empty() {
        Readiness::RequiresOwnerDecision {
            unanswered_questions: 0,
            transfer_candidates: cross_source_matching.candidates.len(),
        }
    } else {
        Readiness::Ready
    };
    let interpretation = Interpretation {
        resolved,
        open_questions,
    };
    let revision = fingerprint(
        &contents.session,
        &source_inventory,
        &account_resolution,
        &scope_assessment,
        &interpretation,
        &cross_source_matching,
        &commit_delta,
        &control_reconciliation,
        &readiness,
    );

    Ok(PlannedSession {
        declined,
        plan: ImportPlan {
            session: contents.session,
            revision,
            source_inventory,
            account_resolution,
            scope_assessment,
            interpretation,
            cross_source_matching,
            commit_delta,
            control_reconciliation,
            readiness,
        },
        candidates,
    })
}

/// One row, read once and used by every section.
struct ReadRow {
    row: u32,
    /// `None` when the stored payload cannot be parsed by this build.
    intake: Option<Intake>,
    /// The operation the row read as, where it read as one at all.
    ///
    /// Kept beside `candidate` rather than recovered from it, because the two
    /// fail at different steps and the difference is what a coverage gap can
    /// say: a row that reached an operation and then failed normalisation
    /// states what it would have moved, and a row that never reached one does
    /// not (iaam-bufs).
    operation: Option<SubmittedOperation>,
    /// The stable key the caller's row identity yielded, when it yielded one.
    row_key: Option<String>,
    /// The stored payload, so a row the caller named nothing by can still be
    /// named by a fingerprint of what it sent — as the sync path fingerprints
    /// a row its source did not identify.
    payload: String,
    candidate: Result<iaam_core::event::Event, Rejection>,
}

impl ReadRow {
    /// The account the row is on, as the caller stated it.
    ///
    /// `None` only where the stored payload does not parse: a row this build
    /// cannot read names nothing it can be trusted about. The answer for every
    /// row it can read is [`Intake::account`]'s, so that the account this
    /// assessment reports and the account [`add_rows`] checked are read the
    /// same way.
    fn account(&self) -> Option<AccountId> {
        Some(self.intake.as_ref()?.account())
    }

    /// The document the row names, when it names one.
    fn document(&self) -> Option<&str> {
        match self.intake.as_ref()? {
            Intake::Observed { row } => row.identity.document.as_deref(),
            Intake::Concluded { .. } => None,
        }
    }
}

fn inventory(contents: &SessionContents, rows: &[ReadRow]) -> SourceInventory {
    let mut documents: Vec<String> = Vec::new();
    let mut accounts: Vec<AccountId> = Vec::new();
    let mut period: Option<(time::Date, time::Date)> = None;
    for read in rows {
        if let Some(document) = read.document()
            && !documents.iter().any(|seen| seen == document)
        {
            documents.push(document.to_owned());
        }
        if let Some(account) = read.account()
            && !accounts.contains(&account)
        {
            accounts.push(account);
        }
        if let Ok(event) = &read.candidate {
            let day = event.order.date();
            period = Some(match period {
                None => (day, day),
                Some((from, to)) => (from.min(day), to.max(day)),
            });
        }
    }
    SourceInventory {
        source: contents.session.source,
        import: contents.session.import,
        documents,
        rows: contents.observations.len(),
        period,
        accounts,
    }
}

fn account_resolution(resolver: &Resolver, rows: &[ReadRow]) -> AccountResolution {
    let mut resolved: Vec<AccountId> = Vec::new();
    let mut missing: Vec<AccountId> = Vec::new();
    let mut conflicting: Vec<String> = Vec::new();
    for read in rows {
        if let Some(account) = read.account() {
            let known = resolver
                .directory
                .accounts
                .iter()
                .any(|held| held.id == account);
            let bucket = if known { &mut resolved } else { &mut missing };
            if !bucket.contains(&account) {
                bucket.push(account);
            }
        }
        if let Some(Intake::Observed { row }) = read.intake.as_ref()
            && let Some(name) = row.counterparty_name()
            && resolver.counterparty_matches(name, row.dates.effective_date()) > 1
            && !conflicting.iter().any(|seen| seen == name)
        {
            conflicting.push(name.to_owned());
        }
    }
    AccountResolution {
        resolved,
        missing,
        conflicting,
    }
}

fn scope_assessment(
    accounts: &[AccountId],
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
) -> ScopeAssessment {
    let mut assessment = ScopeAssessment {
        in_contour: Vec::new(),
        explicitly_outside: Vec::new(),
        awaiting_disposition: Vec::new(),
    };
    for account in accounts {
        match account_scope(*account, contours, exclusions) {
            AccountScope::Inside => assessment.in_contour.push(*account),
            AccountScope::Outside => assessment.explicitly_outside.push(*account),
            AccountScope::Undecided => assessment.awaiting_disposition.push(*account),
        }
    }
    assessment
}

fn planned_fact(read: &ReadRow, event: &iaam_core::event::Event) -> PlannedFact {
    let account = read.account().unwrap_or(event.account);
    // `cash_effect_on` rather than a fold here: summing money is arithmetic and
    // belongs to the core, which the architecture guard enforces (§3.1, §13).
    // A mixture of currencies on one account is an error there, where the old
    // fold turned it into `None` and then into a zero labelled RUB.
    let effect = event.cash_effect_on(account).ok().flatten();
    PlannedFact {
        row: read.row,
        account,
        records_as: event.kind.discriminant(),
        amount_minor: effect.map_or(0, |money| money.amount().raw()),
        currency: effect.map_or(CurrencyCode::Rub, |money| money.currency()),
        date: event.dates.cash_posted.map(|posted| posted.0),
        idempotency_key: event.idempotency_key.clone(),
    }
}

/// The provenance each account's planned facts will be written under.
///
/// Derived here from [`session_origin`] rather than carried out of the
/// normalisation above, and that is deliberate: the same function decides what
/// the commit stamps, so the plan cannot describe an origin the commit does not
/// use — which is the whole reason a session's origin is derived and not stored
/// (iaam-zv54). Reading it back off the built candidates would be a second
/// answer to a question that already has one owner.
///
/// Keyed on [`PlannedFact::account`] — the account the published fact names —
/// so that a reader can join the two by the value in front of him rather than
/// by one this function chose and did not print.
///
/// Sorted and deduplicated by account, so that two plans of an unchanged session
/// are the same plan: the row order the facts arrive in is the session's, and a
/// list that inherited it would make the revision stamp depend on it.
fn fact_origins(
    owner: OwnerId,
    session: &ImportSessionView,
    facts: &[PlannedFact],
) -> Vec<PlannedOrigin> {
    let accounts: BTreeSet<AccountId> = facts.iter().map(|fact| fact.account).collect();
    accounts
        .into_iter()
        .map(|account| {
            let origin = session_origin(owner, session, account);
            PlannedOrigin {
                account,
                source: origin.source,
                import: origin.import,
            }
        })
        .collect()
}

/// What a list of planned facts comes to, per account and currency.
///
/// The fold itself is [`iaam_core::batch::total`]: summing money is arithmetic,
/// and the architecture guard refuses it here (§3.1, §13). What this function
/// decides is what counts as a movement, which the core deliberately leaves to
/// its caller.
///
/// A fact whose cash effect on its own account is zero is left out. Its
/// `currency` is a placeholder — [`planned_fact`] labels a zero `RUB` where the
/// event moves no cash on that account, because the published shape has no room
/// for «no cash» — and folding it would open a rouble total on an account that
/// has never held roubles, and count a row against a currency it never named.
fn batch_totals(facts: &[PlannedFact]) -> Result<Vec<BatchTotal>, AppError> {
    batch::total(&movements(facts)).map_err(AppError::BatchTotal)
}

/// The cash movements a list of planned facts is, in the core's own vocabulary.
///
/// Written once and used by both the totals and the control comparison, because
/// the two must be about the same rows: the comparison's `observed` is folded
/// from exactly what is passed here, and a second selection beside this one
/// could come to fold a row this one drops.
///
/// The date travels with the movement, and that is iaam-mnv0: it is what lets
/// [`batch::compare`] ask whether the rows it folded are the rows the source's
/// figures are about. It is [`PlannedFact::date`], the day the fact will be
/// posted on, because that is the day a statement's period is printed in terms
/// of; `None` where the row states none, which no interval places either way.
fn movements(facts: &[PlannedFact]) -> Vec<BatchMovement> {
    facts
        .iter()
        .filter(|fact| fact.amount_minor != 0)
        .map(|fact| BatchMovement {
            account: fact.account,
            amount: Money::new(PostedMinor::new(fact.amount_minor), fact.currency),
            date: fact.date,
        })
        .collect()
}

/// The stamp a plan carries, and commit refuses when it no longer matches.
///
/// A digest of everything the plan says, and of nothing else. That is what makes
/// the refusal meaningful in both directions: **anything** that would change what
/// commit writes — a row added, an answer given, an account created, a
/// classification rule written or retired, a fact appended that now holds one of
/// these rows' idempotency keys, the session itself closing — changes some
/// section of the plan and therefore the stamp; and a change that alters nothing
/// the plan says does not refuse a commit for no reason.
///
/// It is a hash rather than a counter for the same reason: a counter says the
/// session was touched, and a plan is stale when what it describes changed, not
/// when somebody looked at it.
#[allow(clippy::too_many_arguments)]
fn fingerprint(
    session: &ImportSessionView,
    inventory: &SourceInventory,
    accounts: &AccountResolution,
    scope: &ScopeAssessment,
    interpretation: &Interpretation,
    matching: &Proposals,
    delta: &CommitDelta,
    control: &ControlReconciliation,
    readiness: &Readiness,
) -> SessionRevision {
    use std::fmt::Write as _;

    let mut rendered = String::new();
    let _ = writeln!(
        rendered,
        "session {} {}",
        session.id.inner(),
        session.state.code()
    );
    let _ = writeln!(
        rendered,
        "inventory {:?} {:?} {:?} {} {:?} {:?}",
        inventory.source.map(|id| id.inner()),
        inventory.import.map(|id| id.inner()),
        inventory.documents,
        inventory.rows,
        inventory.period,
        inventory
            .accounts
            .iter()
            .map(|id| id.inner())
            .collect::<Vec<_>>()
    );
    let _ = writeln!(
        rendered,
        "accounts {:?} {:?} {:?}",
        accounts
            .resolved
            .iter()
            .map(|id| id.inner())
            .collect::<Vec<_>>(),
        accounts
            .missing
            .iter()
            .map(|id| id.inner())
            .collect::<Vec<_>>(),
        accounts.conflicting
    );
    let _ = writeln!(
        rendered,
        "scope {:?} {:?} {:?}",
        scope
            .in_contour
            .iter()
            .map(|id| id.inner())
            .collect::<Vec<_>>(),
        scope
            .explicitly_outside
            .iter()
            .map(|id| id.inner())
            .collect::<Vec<_>>(),
        scope
            .awaiting_disposition
            .iter()
            .map(|id| id.inner())
            .collect::<Vec<_>>()
    );
    for fact in &interpretation.resolved {
        let _ = writeln!(rendered, "resolved {fact:?}");
    }
    for open in &interpretation.open_questions {
        let _ = writeln!(
            rendered,
            "open {} {} {}",
            open.row,
            open.question.inner(),
            open.prompt
        );
    }
    for candidate in &matching.candidates {
        let _ = writeln!(
            rendered,
            "candidate {:?} {:?} {:?}",
            candidate.outgoing.origin, candidate.incoming.origin, candidate.evidence
        );
    }
    for leg in &matching.unmatched {
        let _ = writeln!(rendered, "unmatched {:?}", leg.origin);
    }
    for fact in &delta.facts {
        let _ = writeln!(rendered, "fact {fact:?}");
    }
    for fact in &delta.duplicates {
        let _ = writeln!(rendered, "duplicate {fact:?}");
    }
    for retained in &delta.retained_unrecorded {
        let _ = writeln!(rendered, "retained {retained:?}");
    }
    // Derived from the lists above, and stamped anyway: the digest's contract is
    // that it covers everything the plan says, and a section left out because it
    // «cannot disagree» is the section that will, the day someone changes how it
    // is folded.
    for origin in &delta.fact_origins {
        let _ = writeln!(rendered, "fact origin {origin:?}");
    }
    for total in &delta.fact_totals {
        let _ = writeln!(rendered, "fact total {total:?}");
    }
    for total in &delta.duplicate_totals {
        let _ = writeln!(rendered, "duplicate total {total:?}");
    }
    for comparison in &control.comparisons {
        let _ = writeln!(rendered, "control {comparison:?}");
    }
    let _ = writeln!(rendered, "readiness {readiness:?}");

    let digest = Sha256::digest(rendered.as_bytes());
    SessionRevision(digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    }))
}

// ---------------------------------------------------------------------------
// The source's control section, written as the assertions it is (iaam-jc3y)
// ---------------------------------------------------------------------------

/// The parser version every control assertion out of an import session carries.
///
/// **This is the whole of how a source's word is kept apart from the owner's.**
/// `record_owner_balance` stamps `owner-stated/1`, and §10.4 caps anything
/// resting on it at `accepted_internal` through
/// [`Ground::OwnerStatedBalance`] — because the owner may well have read the
/// figure in the very report the system parsed. A control section transcribed
/// out of a statement is not that claim: it is what the document says, stated by
/// whoever fed the document in, and the owner may never have looked at it.
///
/// Recording it under `owner-stated/1` would have been the easy path and would
/// have been a forgery: an agent, which may open and commit a session, would be
/// writing the owner's word about his own balance. Recording it under the row
/// parser's version would have been the other forgery — the rows and the control
/// section would then look like one parse of one document, which they nearly are
/// but are not, and [`SourceChannel::is_independent_of`] would be answering
/// about a distinction that had been erased.
///
/// It stamps the coverage gaps a commit writes as well as its assertions
/// (iaam-bufs), and that is not a widening of what it means but the whole of
/// what makes a gap work. Reconciliation correlates a gap with an assertion
/// group by account, period, source **and parser version**; a gap written under
/// any other version would taint nothing it was about, and the assertions this
/// same commit wrote would go on to confirm a batch that had dropped rows.
///
/// [`Ground::OwnerStatedBalance`]: iaam_core::reconciliation::evidence::Ground::OwnerStatedBalance
/// [`SourceChannel::is_independent_of`]: iaam_core::reconciliation::evidence::SourceChannel::is_independent_of
const CONTROL_PARSER_VERSION: &str = "import-control/1";

/// Version of the key one transcribed control assertion is written under.
///
/// Part of the key, as [`OWNER_BALANCE_KEY_VERSION`] is part of its own: keys
/// have already been deduplicated against, so a change of form must be visible
/// in the value rather than inferred from its shape.
///
/// [`OWNER_BALANCE_KEY_VERSION`]: crate::scenarios::reconciliation
const CONTROL_KEY_VERSION: u8 = 1;

/// The key under which one transcribed control figure is the same fact twice.
///
/// Deliberately the same shape as the owner-stated key — account, period,
/// [`ControlClaim::subject_key`] — in its own namespace. The shape, because the
/// subject is the question and not the answer: the same statement imported
/// twice states one fact, and the second import must not append a second copy of
/// it. The separate namespace, because these two facts are not the same fact:
/// an agent transcribing what a bank printed cannot supersede, or be superseded
/// by, the owner saying what he holds. Sharing a namespace would let either
/// silently deduplicate the other away.
///
/// The session is **not** in the key, and that is the point of keying on the
/// claim: two sessions importing the same statement — the ordinary shape of a
/// retried import — state one fact and write it once.
///
/// [`ControlClaim::subject_key`]: iaam_core::reconciliation::claim::ControlClaim::subject_key
fn control_assertion_key(
    account: AccountId,
    period: AssertionPeriod,
    claim: &ControlClaim,
) -> String {
    format!(
        "source-control:v{CONTROL_KEY_VERSION}:{}:{}:{}:{}",
        account.inner(),
        period.from,
        period.to,
        claim.subject_key()
    )
}

/// The events one session's control sections become.
///
/// Built from the plan the commit is about to write, so what is asserted is
/// exactly what was compared: a second read of the session here would be the
/// drift iaam-k1xa exists to prevent, one section further along.
///
/// Every stated figure becomes a claim in the journal's own §10.3 vocabulary,
/// and the mapping is fixed: the two balances are [`ControlClaim::CashBalance`]
/// at their respective points, and the two turnover sides are one
/// [`ControlClaim::CashTurnover`] — one claim, because a statement asserts a
/// pair, and because the type carries both sides in one value. A section that
/// prints only one side asserts the other as zero, which is what a statement
/// with an empty column means and is checkable; splitting it into a claim the
/// type cannot express is not an option the vocabulary offers.
///
/// The provenance is the session's own source with
/// [`CONTROL_PARSER_VERSION`], and the document hash is a digest of the figures
/// themselves. It is a digest rather than the file's hash because a session
/// never sees the file — it receives rows and figures over HTTP — and a
/// fabricated file hash would be a claim about a document nobody read.
///
/// The session is stamped on the assertion as it is on the rows, and it names
/// the session that **wrote** it. [`control_assertion_key`] deliberately keeps
/// the session out of the key, so a second import of the same statement states
/// the same fact and deduplicates against the first; the assertion in the
/// journal therefore names the session that first recorded the figure, which is
/// the act that put it there.
///
/// [`ControlClaim::CashBalance`]: iaam_core::reconciliation::claim::ControlClaim::CashBalance
/// [`ControlClaim::CashTurnover`]: iaam_core::reconciliation::claim::ControlClaim::CashTurnover
fn control_assertions(owner: OwnerId, plan: &ImportPlan) -> Vec<iaam_core::event::Event> {
    let mut events = Vec::new();
    for section in plan
        .control_reconciliation
        .comparisons
        .iter()
        .filter_map(|comparison| comparison.stated.as_ref())
    {
        let mut claims: Vec<ControlClaim> = Vec::new();
        for (at, amount) in [
            (BalancePoint::Opening, section.opening),
            (BalancePoint::Closing, section.closing),
        ] {
            if let Some(amount) = amount {
                claims.push(ControlClaim::CashBalance {
                    currency: section.currency,
                    amount,
                    at,
                });
            }
        }
        if section.debit_turnover.is_some() || section.credit_turnover.is_some() {
            claims.push(ControlClaim::CashTurnover {
                currency: section.currency,
                debit: section.debit_turnover.unwrap_or(PostedMinor::new(0)),
                credit: section.credit_turnover.unwrap_or(PostedMinor::new(0)),
            });
        }
        // The same derivation the rows of this session get, for the account
        // this section is about: an assertion written under an identity nobody
        // can name is as unreachable as a row written under one, and a second
        // random source here would put the section and its own rows in two
        // `StatementGroup`s.
        let provenance = Provenance::new(
            session_origin(owner, &plan.session, section.account).source,
            section_hash(section),
            ParserVersion(CONTROL_PARSER_VERSION.to_owned()),
        )
        .with_import_session(plan.session.id);
        for claim in claims {
            events.push(iaam_core::event::Event {
                id: EventId::new_random(),
                schema_version: SCHEMA_VERSION,
                owner,
                account: section.account,
                kind: EventKind::ControlAssertion {
                    period: section.period,
                    claim,
                },
                // Dated at the end of the interval it speaks about, as the
                // owner-stated path dates its own and as the sync path dates the
                // assertions it parses out of a report. An undated assertion
                // makes every reconciliation, balances and returns report fail
                // to build: `reconciliation::observe` refuses a journal holding
                // an event that falls within no period.
                dates: EventDates::for_cash(CashPostedDate(section.period.to)),
                // The sequence within the day is assigned by the store; this is
                // the temporary number every ingestion path passes.
                order: EffectiveOrder::new(section.period.to, 1),
                // No legs, as `Valuation` has none: an assertion does not move
                // money, and a leg here would put the control section into the
                // balance a second time.
                legs: Vec::new(),
                provenance: provenance.clone(),
                relation: Relation::None,
                // `Confidence` describes the value, not its verification (§4.9):
                // the figure is the one the source printed.
                confidence: Confidence::Known,
                idempotency_key: Some(control_assertion_key(
                    section.account,
                    section.period,
                    &claim,
                )),
            });
        }
    }
    events
}

/// One declined row, named the way a coverage gap names rows.
///
/// The naming is the sync path's, deliberately: the identifier the caller gave
/// the row where it gave one, and a fingerprint of what it actually sent where
/// it did not. A later import of the same unchanged row reproduces the same
/// name, which is what lets a gap be lifted by the row that repaired it —
/// nothing lifts gaps today (iaam-dvki), and a name that could not survive to
/// that point would have to be migrated when something does.
///
/// **The dimensions are what the system knows, and stop exactly there.** A row
/// that reached an operation before being refused says what that operation
/// would have moved. A row that never reached one is an observation, and the
/// only thing an observation certainly is, is a cash line — it carries an
/// account, an amount, a currency and a direction, and every classification of
/// such a row moves cash. It is **not** widened to «cash and possibly income»:
/// a row nobody could classify has not been shown to be income, and tainting
/// `Income` on the strength of what a row might have been would make the taint
/// mean less on every row that genuinely is one. The cost is the honest one —
/// an unclassifiable coupon taints `Cash` and not `Income` — and it is the cost
/// of not asserting what was never read.
///
/// A row whose stored payload this build cannot parse taints nothing: it is
/// still named, so that a gap holding others reports it as refused, but nothing
/// can be said about what it would have moved. Where such rows are the only
/// ones, [`coverage_gap::gap_event`] writes nothing at all.
fn declined_row(owner: OwnerId, session: &ImportSessionView, read: &ReadRow) -> DeclinedRow {
    let row = match read.row_key.as_deref() {
        // An empty identifier is not an identifier, as the sync path also says.
        Some(key) if !key.is_empty() => RowName::Given(key.to_owned()),
        _ => RowName::Fingerprint(digest_hex(&read.payload)),
    };
    let dimensions: BTreeSet<Dimension> = match (&read.operation, &read.intake) {
        (Some(operation), _) => coverage_gap::operation_dimensions(&operation.kind),
        (None, Some(Intake::Observed { .. })) => [Dimension::Cash].into_iter().collect(),
        (None, _) => BTreeSet::new(),
    };
    // Named under the identity the row itself would have been written under, by
    // the one derivation every other writer of this session uses. A row nobody
    // could read names no account, and then the session's own source is the
    // most this can honestly say — it is the identity the gap holding the row
    // is written under either way.
    let source = read
        .account()
        .map(|account| session_origin(owner, session, account).source)
        .or(session.source)
        .unwrap_or_else(SourceId::new_random);
    DeclinedRow {
        account: read.account(),
        row: RefusedRow {
            key: SourceRowKey { source, row },
            dimensions,
        },
    }
}

/// Version of the key one import coverage gap is written under.
///
/// Part of the key for [`CONTROL_KEY_VERSION`]'s reason: keys have already been
/// deduplicated against, so a change of form must be visible in the value
/// rather than inferred from its shape.
const IMPORT_GAP_KEY_VERSION: u8 = 1;

/// The key under which one import's coverage gap is the same fact twice.
///
/// **Keyed on the rows it refused, not on how many there were.** That is the
/// defect iaam-lg4q names on the sync path, which builds its identity from the
/// dimension union and a count: refuse row A, repair it, later refuse a
/// different row B in the same dimension, and the second gap collides with the
/// first one's key and is dropped as a duplicate — leaving the journal holding
/// a gap for a row that is now present and none for the row that is missing.
/// A path written after that was found has no excuse for reproducing it, so
/// this key digests the rows themselves.
///
/// Rendered field by field and sorted, never through `Debug`: a digest over a
/// derived rendering is a digest that can change with the compiler, and this
/// string is compared against keys already in the journal.
///
/// The session is **not** in the key, exactly as it is not in
/// [`control_assertion_key`]: two commits of the same statement refusing the
/// same rows state one fact and must write it once. What that costs is the
/// counterpart of iaam-dvki — a later clean import through the same channel
/// writes no gap, and so records nothing that could lift the old one — and it
/// is the epic's to fix, not this key's.
fn coverage_gap_key(account: AccountId, period: AssertionPeriod, rows: &[RefusedRow]) -> String {
    let mut named: Vec<String> = rows
        .iter()
        .map(|refused| {
            let (kind, value) = match &refused.key.row {
                RowName::Given(id) => ("given", id.as_str()),
                RowName::Fingerprint(hex) => ("fingerprint", hex.as_str()),
            };
            format!(
                "{}:{kind}:{value}:{}",
                refused.key.source.inner(),
                refused
                    .dimensions
                    .iter()
                    .map(|dimension| dimension.code())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect();
    // Sorted, so that the same refusals fed in another order are one identity:
    // the rows a commit declines are a set, and their order is the order the
    // caller happened to send them in.
    named.sort();
    format!(
        "import-coverage-gap:v{IMPORT_GAP_KEY_VERSION}:{}:{}:{}:{}",
        account.inner(),
        period.from,
        period.to,
        digest_hex(&named.join("\n"))
    )
}

/// The coverage gaps this commit writes, one per account and stated interval.
///
/// **What this records, and the line it does not cross.** A commit must not
/// record what the *document* contained: this system never sees the document,
/// it receives the rows a client chose to send, and a field saying «the
/// statement held forty rows» would republish the client's word as the
/// system's knowledge. What it may record is what it **was handed and
/// declined**, which is a fact it owns entirely — the rows are in its own
/// session table and the refusal is its own. That is the whole of iaam-bufs,
/// and the line runs exactly between those two sentences.
///
/// So the rows are `commit_delta.retained_unrecorded`, and only the unreadable
/// ones. Three neighbouring lists were considered and each is excluded for the
/// same reason — **the rows are recorded**:
///
/// - `duplicates` commit to a `duplicate` verdict because the journal already
///   holds them under their key. A gap naming them would taint an interval
///   whose rows are present, which is the false taint iaam-lg4q was filed
///   about arriving by another door.
/// - `account_resolution.missing` names accounts the owner has never
///   described. Its rows are written all the same, against an account nobody
///   has described — a directory problem, published as one, and not a coverage
///   problem.
/// - `scope_assessment.awaiting_disposition` names accounts in no contour.
///   Those rows are in the journal and appear in no report, which is a
///   perimeter question the queue already raises. A gap says «this attempt
///   could not confirm», and this attempt recorded them.
///
/// **One gap per account and stated interval, and no section means no gap.**
/// A gap is dated and scoped by an [`AssertionPeriod`], and the only interval
/// this system has been *told* about is the one the source printed in its
/// control section. Deriving one from the rows it managed to read would place a
/// claim on an interval nobody asserted, computed from the rows that are not
/// the problem. It also happens to be where the gap does its work: correlation
/// is by account, period, source and parser version, so a gap carrying
/// [`CONTROL_PARSER_VERSION`] taints exactly the control assertions this same
/// commit is writing — which is what stops a batch that dropped rows from
/// later *confirming* the figures it was allowed to commit against.
///
/// That leaves a session whose source printed no control section writing no
/// gap. It is a real gap in the coverage, it is iaam-hj1o's, and it needs an
/// interval this system can honestly state rather than a change here.
///
/// Where an account has two stated intervals, both are tainted by the same
/// declined rows. Which of the two a declined row belonged to is precisely what
/// could not be read, and «this attempt cannot confirm either» is the true
/// statement.
fn coverage_gaps(
    owner: OwnerId,
    plan: &ImportPlan,
    declined: &[DeclinedRow],
) -> Vec<iaam_core::event::Event> {
    let mut written: BTreeSet<(AccountId, time::Date, time::Date)> = BTreeSet::new();
    let mut events = Vec::new();
    for section in plan
        .control_reconciliation
        .comparisons
        .iter()
        .filter_map(|comparison| comparison.stated.as_ref())
    {
        if !written.insert((section.account, section.period.from, section.period.to)) {
            continue;
        }
        let rows: Vec<RefusedRow> = declined
            .iter()
            .filter(|declined| declined.account == Some(section.account))
            .map(|declined| declined.row.clone())
            .collect();
        let key = coverage_gap_key(section.account, section.period, &rows);
        // The same per-account derivation the rows and the assertions of this
        // session get: a gap written under an identity nobody can name is as
        // unreachable as a row written under one.
        let provenance = Provenance::new(
            session_origin(owner, &plan.session, section.account).source,
            RawHash::parse(&digest_hex(&key))
                .expect("a SHA-256 digest is 64 hexadecimal characters"),
            ParserVersion(CONTROL_PARSER_VERSION.to_owned()),
        );
        if let Some(event) = coverage_gap::gap_event(
            coverage_gap::GapTarget {
                owner,
                account: section.account,
                period: section.period,
            },
            rows,
            provenance,
            key,
        ) {
            events.push(event);
        }
    }
    events
}

/// A SHA-256 of some text, in lower-case hexadecimal.
fn digest_hex(input: &str) -> String {
    use std::fmt::Write as _;

    Sha256::digest(input.as_bytes())
        .iter()
        .fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// The digest one control section stands under.
///
/// Every figure and the interval, rendered in a fixed order. Two transcriptions
/// of the same section therefore carry one hash, and a corrected transcription
/// carries a different one — which is what a `raw_hash` is for: it says which
/// reading of the source a fact came from.
fn section_hash(section: &ControlSection) -> RawHash {
    use std::fmt::Write as _;

    let rendered = format!(
        "control:{}:{}:{}:{}:{:?}:{:?}:{:?}:{:?}",
        section.account.inner(),
        section.currency.code(),
        section.period.from,
        section.period.to,
        section.opening.map(PostedMinor::raw),
        section.closing.map(PostedMinor::raw),
        section.debit_turnover.map(PostedMinor::raw),
        section.credit_turnover.map(PostedMinor::raw),
    );
    let digest = Sha256::digest(rendered.as_bytes());
    let hex = digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    });
    RawHash::parse(&hex).expect("a SHA-256 digest is 64 hexadecimal characters")
}

/// Abandon the session.
///
/// The journal is neither read nor written here, and that is the whole
/// behaviour: what the session held was never a fact, so there is nothing to
/// retract. The session and its rows stay, marked abandoned — what the owner
/// rejected is worth being able to look at, and it is not in the journal either
/// way.
pub async fn abandon_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
) -> Result<ImportSessionView, AppError> {
    require_submit(principal)?;
    services
        .store
        .close_import_session(principal.owner, session, ImportSessionState::Abandoned)
        .await
}

// ---------------------------------------------------------------------------
// Settling a row
// ---------------------------------------------------------------------------

/// What the owner's directory and rules make of one observed row.
enum Assessment {
    Settled {
        classification: Classification,
        movement: Movement,
    },
    Ambiguous {
        question: Question,
    },
}

/// The owner's accounts, and the one place a printed identifier is turned into
/// one of them.
///
/// The detailed view rather than the summary because resolution reads the
/// identity a source prints and the aliases that reach the same account
/// (decision 0004), and neither is on the summary view. `cash_class` travels
/// with them and **nothing here reads it**: it is a grouping label for reports,
/// and a classifier that branched on it is the failure decision 0004 asks a
/// reviewer to check for.
///
/// Split out of [`Resolver`] so that the tiering below has exactly one
/// implementation. A row's counterparty and a batch's declared account are the
/// same question asked at two moments — «which of the owner's accounts does
/// this printed string name» — and two implementations of it could come to
/// disagree, which is how a batch gets declared against one account while its
/// rows resolve against another.
pub struct AccountDirectory {
    accounts: Vec<AccountDetailView>,
}

/// The owner's accounts, his statements about them, and his rules, loaded once
/// per batch.
struct Resolver {
    directory: AccountDirectory,
    /// What the owner said about which of his accounts money moves between.
    ///
    /// Three states, and the absence of a view is one of them — see
    /// [`AccountTransferStatementView`]. Read by [`Self::denies`] and by nothing
    /// else, because that is the only thing a statement is allowed to do here.
    statements: Vec<AccountTransferStatementView>,
    rules: Vec<ClassificationRule>,
}

impl Resolver {
    async fn load(services: &AppServices, owner: OwnerId) -> Result<Self, AppError> {
        let accounts = services.store.list_account_details(owner).await?;
        let statements = services
            .store
            .list_account_transfer_statements(owner)
            .await?;
        let stored = services.rules.list_rules(owner).await?;
        let mut rules = Vec::with_capacity(stored.len());
        for rule in stored.into_iter().filter(|rule| rule.retired_at.is_none()) {
            rules.push(crate::scenarios::classification::rule_from_view(rule)?);
        }
        Ok(Self {
            directory: AccountDirectory { accounts },
            statements,
            rules,
        })
    }
}

/// Why a printed identifier named no single account, before a field name is put
/// on it.
///
/// The two halves of a refusal that do not depend on where the identifier was
/// written: what the field admits and what arrived. The field itself is added by
/// the caller, because the same failure is `source.account` on a declaration and
/// `account` on a row.
struct UnresolvedAccount {
    expected: String,
    actual: String,
}

impl AccountDirectory {
    /// The owner's accounts, read once.
    ///
    /// Public because a route that judges rows one by one needs the same
    /// directory the declaration is resolved against, and needs it **once**: a
    /// batch of two hundred rows that each loaded the directory would read the
    /// store two hundred times to answer the same question.
    pub async fn load(services: &AppServices, owner: OwnerId) -> Result<Self, AppError> {
        Ok(Self::from_accounts(
            services.store.list_account_details(owner).await?,
        ))
    }

    /// A directory over accounts already in hand.
    ///
    /// [`Self::load`] is how a route gets one. This is for a caller that has the
    /// accounts already and must not read them a second time — the resolution
    /// must be over one reading, or a batch could be declared against the
    /// directory as it was and have its rows judged against the directory as it
    /// became.
    #[must_use]
    pub const fn from_accounts(accounts: Vec<AccountDetailView>) -> Self {
        Self { accounts }
    }

    /// Every account a printed counterparty could be, from the strongest kind of
    /// evidence that recognised anything.
    ///
    /// Three tiers, tried in order, and the search stops at the first that
    /// matches at all rather than pooling them:
    ///
    /// 1. **iaam's own account identifier**, printed verbatim.
    /// 2. **The identity the source prints** for the account, and the aliases
    ///    that reach the same account — a card among them (decision 0004). An
    ///    alias is read against the day of the row where the row carries one.
    /// 3. **The account's title**, trimmed and case-insensitively.
    ///
    /// The order is the decision. A title is what the owner reads and may
    /// rename at any moment; an identity is what a source repeats. So an
    /// identity must not merely tie with a title — a rename would otherwise
    /// silently re-point a resolution, which is the defect decision 0004 was
    /// written about. Stopping at the first tier that matched is what makes it
    /// beat rather than tie: an identity naming one account is not diluted by
    /// another account whose title happens to agree, and — the other way round —
    /// a title shared by two accounts is not settled by an identity somewhere
    /// below it, because nothing below is consulted.
    ///
    /// **The title tier stays**, deliberately. Every account that existed before
    /// decision 0004 states no identity and has no aliases, and dropping the
    /// tier would stop recognising their transfers until each is back-filled —
    /// a silent behaviour change bought for no correctness. Its one failure mode
    /// is a collision, and a collision is refused here rather than guessed at.
    fn candidates(&self, name: &str, on: Option<time::Date>) -> Vec<AccountId> {
        let printed = name.trim();

        if let Ok(id) = uuid::Uuid::parse_str(printed) {
            let own: Vec<AccountId> = self
                .accounts
                .iter()
                .filter(|account| account.id.inner() == id)
                .map(|account| account.id)
                .collect();
            if !own.is_empty() {
                return own;
            }
        }

        let identified: Vec<AccountId> = self
            .accounts
            .iter()
            .filter(|account| identifies(account, printed, on))
            .map(|account| account.id)
            .collect();
        if !identified.is_empty() {
            return identified;
        }

        let wanted = printed.to_lowercase();
        self.accounts
            .iter()
            .filter(|account| account.title.trim().to_lowercase() == wanted)
            .map(|account| account.id)
            .collect()
    }

    /// The account a declaration names, or a refusal saying why it names none.
    ///
    /// The same tiering a row's counterparty goes through, and deliberately the
    /// same call: a batch declared against the identifier a source prints must
    /// land on the account its own rows land on, and the only way to guarantee
    /// that is for one function to answer both.
    ///
    /// **No date is asked for, and an alias therefore matches over its whole
    /// life.** A declaration is about a file, not about a day: the rows inside
    /// it may span a card replacement, so requiring the identifier to be valid
    /// on some particular date would refuse the very statement that shows the
    /// change. The interval is still read where it can decide something — on
    /// the rows, each against its own date.
    ///
    /// Two accounts is refused rather than picked between, exactly as
    /// [`Resolver::resolve_counterparty`] refuses. The refusal names both the
    /// identifier and the accounts it reached: an ambiguity the owner cannot
    /// see is one he cannot clear.
    pub fn resolve_declared(&self, printed: &str) -> Result<AccountDetailView, AppError> {
        self.resolve(printed).map_err(|refusal| AppError::Invalid {
            // The pointer the caller sent it under. Every route reading a
            // declaration spells it this way, beside `source.channel` and
            // `source.label`, which are refused from the same object.
            field: "source.account".to_owned(),
            expected: refusal.expected,
            actual: refusal.actual,
        })
    }

    /// The account **one row** names, refused in the vocabulary a row is judged
    /// in.
    ///
    /// The same resolution the declaration goes through, and deliberately the
    /// same call: `account` on a row and `source.account` on the batch are the
    /// one question «which of the owner's accounts is this», and two answers to
    /// it are how a row is recorded against an account its own batch was not
    /// declared for.
    ///
    /// What differs is only what a refusal is. A declaration is one statement
    /// about the whole request, so a declaration that names no account refuses
    /// the request. A row is judged on its own — §10.1 — so a row that names no
    /// account is one rejected row beside the ones that were read, and a
    /// [`Rejection`] is the shape that says so.
    ///
    /// **No date is asked for here either.** A row carries one, and an alias
    /// interval could be read against it — but the row's account is the account
    /// whose statement the row is *on*, not a counterparty guessed from a
    /// printed string, and refusing a row because the card it was declared under
    /// had been replaced by the time the statement was exported would refuse the
    /// statement that shows the replacement. The interval still decides where it
    /// can decide something: on a row's counterparty, in
    /// [`Resolver::resolve_counterparty`].
    pub fn resolve_row(&self, printed: &str) -> Result<AccountId, Rejection> {
        match self.resolve(printed) {
            Ok(account) => Ok(account.id),
            Err(refusal) => Err(Rejection {
                field: "account".to_owned(),
                expected: refusal.expected,
                actual: refusal.actual,
            }),
        }
    }

    /// The one account a printed identifier names, or why it names none.
    ///
    /// The refusal is built here and worded once, so the declaration and the row
    /// refuse an identifier in the same words under two different field names.
    /// Two wordings would eventually describe two different rules.
    fn resolve(&self, printed: &str) -> Result<AccountDetailView, UnresolvedAccount> {
        let matched = self.candidates(printed, None);
        let [only] = matched[..] else {
            return Err(UnresolvedAccount {
                expected: if matched.is_empty() {
                    "an account of the owner's, named by its iaam identifier or \
                     by the identifier its source prints for it"
                        .to_owned()
                } else {
                    "an identifier naming exactly one account: name one of them \
                     by its iaam identifier, or give exactly one of them the \
                     identifier this source prints (provider, provider_account_id)"
                        .to_owned()
                },
                actual: if matched.is_empty() {
                    format!("«{printed}» names none of the owner's accounts")
                } else {
                    format!(
                        "«{printed}» names {} of the owner's accounts: {}",
                        matched.len(),
                        matched
                            .iter()
                            .map(|id| format!("{} ({})", self.title(*id), id.inner()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
            });
        };
        Ok(self
            .accounts
            .iter()
            .find(|account| account.id == only)
            .cloned()
            .expect("a candidate comes from this directory"))
    }

    fn title(&self, account: AccountId) -> String {
        self.accounts
            .iter()
            .find(|known| known.id == account)
            .map_or_else(|| account.inner().to_string(), |known| known.title.clone())
    }
}

impl Resolver {
    fn candidates(&self, name: &str, on: Option<time::Date>) -> Vec<AccountId> {
        self.directory.candidates(name, on)
    }

    fn title(&self, account: AccountId) -> String {
        self.directory.title(account)
    }

    /// The owner's own account a printed counterparty names, if it names one.
    ///
    /// This is the seam the derived internal transfer comes through: a
    /// counterparty recognised here reaches `classify` as
    /// `Counterparty::OwnAccount` and settles without a question. Exactly one
    /// candidate is a recognition; two are not, and picking between them would
    /// be the guess this module exists to refuse. The refusal is per tier —
    /// two accounts sharing an alias recognise neither, just as two sharing a
    /// title do.
    fn resolve_counterparty(&self, name: &str, on: Option<time::Date>) -> Option<AccountId> {
        let mut candidates = self.candidates(name, on).into_iter();
        let first = candidates.next()?;
        candidates.next().is_none().then_some(first)
    }

    /// How many of the owner's accounts a printed counterparty could be.
    ///
    /// [`Self::resolve_counterparty`] answers `None` both for a name that
    /// matches nothing and for one that matches two accounts, and the two are
    /// not the same thing to report: the first is a stranger and the second is
    /// an ambiguity the owner can clear up — by renaming an account, or now by
    /// giving one of them the identifier its source prints.
    fn counterparty_matches(&self, name: &str, on: Option<time::Date>) -> usize {
        self.candidates(name, on).len()
    }

    /// Whether the owner's own words say money does not move between these two.
    ///
    /// This is the only thing his transfer statement is allowed to do here, and
    /// the restriction is the decision rather than an unfinished implementation.
    /// A statement is a general claim about which pairs of his accounts money
    /// moves between; it says nothing about which account any printed string
    /// names, and it carries no direction. So it can never **make** a
    /// resolution, never break a tie between candidates, and never settle a row
    /// whose direction is open. It can only **withdraw** a resolution reached
    /// on other grounds, which is a restriction and not a conclusion.
    ///
    /// A pair is declared when either side's statement names the other: both
    /// sides are asked separately ([`crate::actions`]), and the pair is the same
    /// pair whichever side he was asked about. It is denied when neither names
    /// it and at least one of the two statements exists — a statement is «these,
    /// and no others», so an account it does not name is one he excluded. Where
    /// neither account has a statement he has not spoken, and silence denies
    /// nothing: that is the state every new account starts in.
    fn denies(&self, account: AccountId, partner: AccountId) -> bool {
        if account == partner {
            return false;
        }
        let names = |near: AccountId, far: AccountId| {
            self.statements
                .iter()
                .find(|statement| statement.account == near)
                .map(|statement| statement.partners.contains(&far))
        };
        match (names(account, partner), names(partner, account)) {
            (Some(true), _) | (_, Some(true)) | (None, None) => false,
            (Some(false), _) | (_, Some(false)) => true,
        }
    }

    /// Settle the row, or name what has to be asked.
    ///
    /// Two things have to be true before a row can be recorded: **what** it was
    /// and **which way** the money went. `classify` answers the first. The
    /// second comes from the source when it stated a direction, and otherwise
    /// from the classification — but only from the two outcomes that *are* a
    /// direction: a fee leaves and income arrives.
    ///
    /// The other two carry none, and a directionless row settled as either is
    /// therefore still asked about. `ExternalFlow` was always the obvious case —
    /// the alternative is picking `deposit`, which is the bug the observation
    /// shape was introduced for. `InternalTransfer` is the same case wearing an
    /// account: it names the far side, not a destination, and reading a
    /// direction out of it was iaam-xf49. Both now reach
    /// [`Question::UnresolvedDirection`], which is the question that names a
    /// direction and a classification in one answer.
    fn assess(&self, row: &ObservedRow) -> Assessment {
        // The owner's transfer statement reaches classification here, and only
        // here. It is applied to the resolution rather than to the candidates
        // that produced it — see [`Self::denies`] — so it can take a derived
        // internal transfer back to being a question, and can do nothing else.
        // A resolution his own words deny is one the system would otherwise
        // record without asking him about a movement he said does not happen.
        let resolved = row
            .counterparty_name()
            .and_then(|name| self.resolve_counterparty(name, row.dates.effective_date()))
            .filter(|resolved| !self.denies(row.account, *resolved));
        let subject = row.subject(resolved);
        let classification = match classify(&subject, &self.rules) {
            ClassificationResult::Resolved { classification, .. } => classification,
            ClassificationResult::Ambiguous { question } => {
                return Assessment::Ambiguous { question };
            }
        };
        match movement_of(classification, row) {
            Some(movement) => Assessment::Settled {
                classification,
                movement,
            },
            None => Assessment::Ambiguous {
                question: Question::UnresolvedDirection {
                    account: row.account,
                    stated: row.source_kind.clone(),
                    counterparty: row.counterparty_name().map(str::to_owned),
                },
            },
        }
    }

    /// The question in words, with account titles rather than identifiers.
    ///
    /// The rendering is here rather than in `iaam-ingest` because the pure
    /// function has no directory, and a sentence containing a UUID is not a
    /// specific question.
    fn render(&self, question: &Question) -> String {
        let account = self.title(question.account());
        match question {
            Question::IsTransferInternal { counterparty, .. } => format!(
                "On {account}, the source named «{counterparty}» as the other side. \
                 Is that one of your own accounts, and if so which one?"
            ),
            Question::IsOutflowAFee { .. } => format!(
                "Money left {account} and the source named no counterparty. \
                 Was it a fee, or a payment out?"
            ),
            // Three alternatives and therefore three clauses. The middle one is
            // new wording as well as a new answer: the question used to read
            // «income, or money coming back?», where «money coming back» was the
            // sentence for `received` — money arriving from outside — and read
            // to a human, and to an agent relaying it, as the refund the
            // vocabulary could not express (`iaam-7l7v`).
            Question::IsInflowIncome { .. } => format!(
                "Money arrived at {account} and the source named no counterparty. \
                 Was it income the capital earned, money a counterparty returned \
                 on something you paid for, or money coming in from outside?"
            ),
            Question::UnresolvedDirection {
                stated,
                counterparty,
                ..
            } => {
                let word = stated
                    .as_deref()
                    .map_or_else(|| "no direction".to_owned(), |stated| format!("«{stated}»"));
                // What the row leaves open depends on whether it named anybody.
                // A row that named a counterparty leaves only the direction open
                // — and since iaam-xf49 that includes one the directory
                // recognised, which settles what the row is and not which way it
                // ran. Saying the other side cannot be read would then be false.
                let rest = counterparty.as_deref().map_or_else(
                    || {
                        "named no counterparty, so neither which way the money \
                         went nor who was on the other side can be read from \
                         the row"
                            .to_owned()
                    },
                    |name| {
                        format!(
                            "named «{name}» as the other side, so which way the \
                             money went cannot be read from the row"
                        )
                    },
                );
                format!("On {account}, the source stated {word} and {rest}. Which was it?")
            }
        }
    }
}

/// Whether an account is the one a source printed this identifier for.
///
/// The identity and the aliases are compared **verbatim**: decision 0004 defines
/// `provider_account_id` as opaque to iaam, and equality is the whole contract.
/// Case-folding it would be a claim about what the value means, and the first
/// rule that depended on that claim would be depending on a parse.
///
/// An alias is read against the day of the row where there is one, so a card
/// whose interval has closed stops recognising rows posted after it. Where the
/// row carries no date the interval is not consulted: refusing the alias anyway
/// would be a conclusion drawn from a field the row does not have.
fn identifies(account: &AccountDetailView, printed: &str, on: Option<time::Date>) -> bool {
    if account.provider_account_id.as_deref() == Some(printed) {
        return true;
    }
    account.aliases.iter().any(|alias| {
        alias.value == printed
            && on.is_none_or(|day| {
                day >= alias.valid_from && alias.valid_to.is_none_or(|until| day < until)
            })
    })
}

/// Which way the money went, when anything says so.
///
/// Two things may say so and there is no third. The source stated a direction,
/// or the classification is one — a fee leaves and income arrives, which
/// [`Classification::implied_movement`] decides beside the type, so that a fifth
/// outcome has to answer the question rather than inherit an answer.
///
/// An internal transfer is not one of those, and this function used to treat it
/// as one: it compared the account the classification names with the row's own
/// and called the difference a direction. The account it names is the **far
/// side**, so that comparison always said "out" — including for
/// `Answer::ReceivedFromOwnAccount { from }`, which records the far side in the
/// same place for money that arrived (iaam-xf49). Nothing derives a direction
/// from an internal transfer now; a directionless one is asked about.
fn movement_of(classification: Classification, row: &ObservedRow) -> Option<Movement> {
    row.movement().or_else(|| classification.implied_movement())
}

/// The account an answer names, when it names one.
const fn named_account(answer: Answer) -> Option<AccountId> {
    match answer {
        Answer::SentToOwnAccount { to } => Some(to),
        Answer::ReceivedFromOwnAccount { from } => Some(from),
        Answer::Paid
        | Answer::Received
        | Answer::Fee { .. }
        | Answer::Income { .. }
        | Answer::Refund => None,
    }
}

/// The rule condition a row can be recognised by later, if any.
///
/// `None` where the row offers nothing to match on. A matcher that asks nothing
/// matches nothing by construction, and writing one would record a decision that
/// never applies while looking like one that does.
fn matcher_for(row: &ObservedRow) -> Option<RuleMatcher> {
    let matcher = RuleMatcher {
        counterparty_account: row.counterparty_name().map(str::to_owned),
        description_contains: row.description.clone(),
        kind: row.source_kind.clone(),
    };
    (!matcher.asks_nothing()).then_some(matcher)
}

/// The observed row one question is about.
fn observed_row(contents: &SessionContents, row: u32) -> Result<ObservedRow, AppError> {
    let observation = contents
        .observations
        .iter()
        .find(|candidate| candidate.row == row)
        .ok_or(AppError::NotFound {
            what: "an import observation",
            id: row.to_string(),
        })?;
    match parse_intake(&observation.payload)? {
        Intake::Observed { row } => Ok(*row),
        Intake::Concluded { .. } => Err(AppError::Invalid {
            field: "question".to_owned(),
            expected: "a question about a row whose source stated no conclusion".to_owned(),
            actual: "a row the caller concluded".to_owned(),
        }),
    }
}

/// The operation one stored row commits as.
fn operation_of(
    observation: &ImportObservationView,
    resolver: &Resolver,
) -> Result<SubmittedOperation, Rejection> {
    let intake = parse_intake(&observation.payload).map_err(|error| Rejection {
        field: "observation".to_owned(),
        expected: "a row this build can read".to_owned(),
        actual: error.to_string(),
    })?;
    let row = match intake {
        Intake::Concluded { operation } => return Ok(*operation),
        Intake::Observed { row } => *row,
    };
    if let Some(answer) = &observation.answer {
        let answer: Answer = serde_json::from_str(answer).map_err(|error| Rejection {
            field: "answer".to_owned(),
            expected: "an answer this build can read".to_owned(),
            actual: error.to_string(),
        })?;
        return row.resolve_with(answer);
    }
    match resolver.assess(&row) {
        Assessment::Settled {
            classification,
            movement,
        } => row.resolve(classification, movement),
        // Commit refuses while a question is open, so this is reached only for a
        // row that raised none and still cannot be settled — which the assessment
        // above rules out. Refusing rather than guessing keeps that true even if
        // it stops being.
        Assessment::Ambiguous { .. } => Err(Rejection {
            field: "row".to_owned(),
            expected: "a row whose classification is settled".to_owned(),
            actual: "unanswered".to_owned(),
        }),
    }
}

fn parse_intake(payload: &str) -> Result<Intake, AppError> {
    serde_json::from_str(payload)
        .map_err(|error| AppError::Store(format!("stored import row could not be read: {error}")))
}

fn json<T>(value: &T, what: &str) -> Result<String, AppError>
where
    T: ?Sized + serde::Serialize,
{
    serde_json::to_string(value)
        .map_err(|error| AppError::Store(format!("{what} could not be written: {error}")))
}

fn require_submit(principal: &Principal) -> Result<(), AppError> {
    if principal.scope.may_submit() {
        Ok(())
    } else {
        Err(AppError::Invalid {
            field: "scope".into(),
            expected: "permission to submit operations".into(),
            actual: principal.scope.code().to_owned(),
        })
    }
}

/// May this answerer's decision be turned into a standing rule (`iaam-hnod`)?
///
/// Two acts hide inside one call to `/answer`, and they are gated differently
/// because they do different things to the owner's decisions. Settling this row
/// is import mechanics: it disposes of one line the caller already submitted,
/// and nothing else in the portfolio changes. Writing a
/// [`ClassificationRule`] out of it generalises the settlement into a standing
/// decision that will classify rows nobody has looked at yet — including rows
/// from months the caller has not imported, and including rows it will never
/// see, because a matched row is never asked about.
///
/// The second act is the same one `POST /v1/classification-rules` performs, and
/// that route is owner-only. Admitting the agent here and refusing it there
/// would leave the harder gate protecting nothing: the agent would simply make
/// the decision through the route whose name does not mention rules. So the
/// answer is refused a rule rather than the whole call — the row still settles,
/// and the import still finishes without waking the owner twice.
///
/// The cost is real and is the point: an agent relaying the owner's answers is
/// asked about the same counterparty again next month, because nobody recorded
/// that the answer generalises. Turning it into a rule is one call the owner
/// makes with his own token, and it is a decision he can then read back, edit
/// and retire.
///
/// It reads [`crate::ports::Scope::may_administer`] and not a gate of its own:
/// this **is** the administer decision, arriving by another door, and a second
/// predicate beside it would be a second place for the two to drift apart.
fn may_generalise(principal: &Principal) -> bool {
    principal.scope.may_administer()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::AccountAliasView;
    use iaam_core::event::kind::FeeOrigin;
    use iaam_ingest::observation::{ObservedCounterparty, ObservedDirection, RowIdentity};
    use iaam_ingest::operation::OperationDates;
    use time::macros::date;

    fn account(byte: u8) -> AccountId {
        AccountId(uuid::Uuid::from_bytes([byte; 16]))
    }

    fn detail(id: AccountId, title: &str) -> AccountDetailView {
        AccountDetailView {
            id,
            title: title.to_owned(),
            institution: None,
            provider: None,
            provider_account_id: None,
            cash_class: None,
            // Resolution reads identity, aliases and title. It has never read
            // the owner's expectation about a minus, and iaam-d41s forbids
            // anything but the balances report from reading it.
            negative_balance_expectation: None,
            aliases: Vec::new(),
        }
    }

    fn with_identity(mut view: AccountDetailView, printed: &str) -> AccountDetailView {
        view.provider = Some("bank-one".to_owned());
        view.provider_account_id = Some(printed.to_owned());
        view
    }

    fn with_alias(
        mut view: AccountDetailView,
        value: &str,
        valid_from: time::Date,
        valid_to: Option<time::Date>,
    ) -> AccountDetailView {
        view.aliases.push(AccountAliasView {
            value: value.to_owned(),
            valid_from,
            valid_to,
        });
        view
    }

    fn resolver(accounts: Vec<AccountDetailView>) -> Resolver {
        Resolver {
            directory: AccountDirectory { accounts },
            statements: Vec::new(),
            rules: Vec::new(),
        }
    }

    /// A resolver holding rules the owner has already written.
    fn ruled(accounts: Vec<AccountDetailView>, rules: Vec<ClassificationRule>) -> Resolver {
        Resolver {
            directory: AccountDirectory { accounts },
            statements: Vec::new(),
            rules,
        }
    }

    fn stating(
        accounts: Vec<AccountDetailView>,
        statements: Vec<(AccountId, Vec<AccountId>)>,
    ) -> Resolver {
        Resolver {
            directory: AccountDirectory { accounts },
            statements: statements
                .into_iter()
                .map(|(account, partners)| AccountTransferStatementView { account, partners })
                .collect(),
            rules: Vec::new(),
        }
    }

    /// An outgoing row on `on`, naming `counterparty`, posted on `posted`.
    fn row(on: AccountId, counterparty: &str, posted: Option<time::Date>) -> ObservedRow {
        ObservedRow {
            account: on,
            direction: ObservedDirection::Out,
            amount_minor: -1_000,
            currency: CurrencyCode::Rub,
            counterparty: ObservedCounterparty::Named(counterparty.to_owned()),
            source_kind: Some("transfer".to_owned()),
            description: None,
            dates: OperationDates {
                cash_posted: posted,
                ..OperationDates::default()
            },
            source_time: None,
            identity: RowIdentity::default(),
        }
    }

    /// The same row with the source stating no direction.
    ///
    /// A bank printing the word it uses for a movement internal to itself, with
    /// an amount beside it: not which side of it this account was on, and — the
    /// sign notwithstanding — nothing a direction can be read out of.
    fn directionless(row: ObservedRow) -> ObservedRow {
        ObservedRow {
            direction: ObservedDirection::Inner,
            amount_minor: 1_000,
            source_kind: Some("INNER".to_owned()),
            ..row
        }
    }

    /// The same row with the source stating that the money arrived.
    fn incoming(row: ObservedRow) -> ObservedRow {
        ObservedRow {
            direction: ObservedDirection::In,
            amount_minor: 1_000,
            ..row
        }
    }

    // --- resolution -------------------------------------------------------

    #[test]
    fn the_identity_the_source_prints_resolves_a_counterparty() {
        let main = account(1);
        let resolver = resolver(vec![with_identity(detail(main, "Main"), "ACC-1")]);
        assert_eq!(
            resolver.resolve_counterparty("ACC-1", None),
            Some(main),
            "the counterparty is the identity the source prints for Main"
        );
    }

    #[test]
    fn an_alias_resolves_a_counterparty_no_title_matches() {
        let main = account(1);
        let resolver = resolver(vec![with_alias(
            detail(main, "Main"),
            "CARD-1",
            date!(2026 - 01 - 01),
            None,
        )]);
        assert_eq!(
            resolver.resolve_counterparty("CARD-1", Some(date!(2026 - 06 - 01))),
            Some(main),
            "an alias is a further identifier for the same account"
        );
    }

    #[test]
    fn an_alias_whose_interval_closed_before_the_row_resolves_nothing() {
        let main = account(1);
        let resolver = resolver(vec![with_alias(
            detail(main, "Main"),
            "CARD-1",
            date!(2026 - 01 - 01),
            Some(date!(2026 - 03 - 01)),
        )]);
        assert_eq!(
            resolver.resolve_counterparty("CARD-1", Some(date!(2026 - 06 - 01))),
            None,
            "the alias had stopped reaching the account by the day of the row"
        );
        assert_eq!(
            resolver.resolve_counterparty("CARD-1", Some(date!(2026 - 02 - 01))),
            Some(main),
            "and it did reach it while the interval was open"
        );
    }

    #[test]
    fn an_alias_resolves_a_row_that_carries_no_date() {
        let main = account(1);
        let resolver = resolver(vec![with_alias(
            detail(main, "Main"),
            "CARD-1",
            date!(2026 - 01 - 01),
            Some(date!(2026 - 03 - 01)),
        )]);
        assert_eq!(
            resolver.resolve_counterparty("CARD-1", None),
            Some(main),
            "there is no date to refuse the alias with, and refusing it anyway \
             would be a conclusion drawn from a field the row does not carry"
        );
    }

    #[test]
    fn an_identity_match_beats_a_title_match() {
        let by_title = account(1);
        let by_identity = account(2);
        let resolver = resolver(vec![
            detail(by_title, "ACC-2"),
            with_identity(detail(by_identity, "Savings"), "ACC-2"),
        ]);
        assert_eq!(
            resolver.resolve_counterparty("ACC-2", None),
            Some(by_identity),
            "a title is a display name and an identity is what a source repeats"
        );
    }

    #[test]
    fn an_alias_match_beats_a_title_match() {
        let by_title = account(1);
        let by_alias = account(2);
        let resolver = resolver(vec![
            detail(by_title, "CARD-1"),
            with_alias(
                detail(by_alias, "Savings"),
                "CARD-1",
                date!(2026 - 01 - 01),
                None,
            ),
        ]);
        assert_eq!(
            resolver.resolve_counterparty("CARD-1", Some(date!(2026 - 06 - 01))),
            Some(by_alias),
            "an alias is an identifier and a title is not"
        );
    }

    #[test]
    fn two_accounts_sharing_a_title_resolve_neither() {
        let resolver = resolver(vec![
            detail(account(1), "Savings"),
            detail(account(2), "Savings"),
        ]);
        assert_eq!(resolver.resolve_counterparty("Savings", None), None);
        assert_eq!(resolver.counterparty_matches("Savings", None), 2);
    }

    #[test]
    fn two_accounts_sharing_an_identifier_resolve_neither() {
        let resolver = resolver(vec![
            with_identity(detail(account(1), "Main"), "ACC-1"),
            with_alias(
                detail(account(2), "Savings"),
                "ACC-1",
                date!(2026 - 01 - 01),
                None,
            ),
        ]);
        assert_eq!(
            resolver.resolve_counterparty("ACC-1", None),
            None,
            "picking between them is the guess the resolver exists to refuse, \
             and it is refused at every tier and not only at the title"
        );
        assert_eq!(resolver.counterparty_matches("ACC-1", None), 2);
    }

    #[test]
    fn a_title_still_resolves_an_account_that_states_no_identity() {
        let main = account(1);
        let resolver = resolver(vec![detail(main, "Savings")]);
        assert_eq!(resolver.resolve_counterparty(" savings ", None), Some(main));
    }

    // --- what a batch's declaration resolves to ---------------------------

    #[test]
    fn a_declaration_is_read_by_the_identifier_its_source_prints() {
        let main = account(1);
        let directory = AccountDirectory {
            accounts: vec![
                with_identity(detail(main, "Main"), "ACC-1"),
                detail(account(2), "Savings"),
            ],
        };
        assert_eq!(
            directory
                .resolve_declared("ACC-1")
                .expect("the identity names one account")
                .id,
            main,
        );
        assert_eq!(
            directory
                .resolve_declared(&main.inner().to_string())
                .expect("iaam's own identifier still names it")
                .id,
            main,
            "the shape every existing caller sends keeps working unchanged"
        );
    }

    #[test]
    fn a_declaration_is_read_by_an_alias_whatever_its_interval() {
        // A statement spans a card replacement, so the declaration cannot be
        // tied to a day: the interval decides on the rows, each against its own
        // date, and refusing the file would refuse the very export that shows
        // the change.
        let main = account(1);
        let directory = AccountDirectory {
            accounts: vec![with_alias(
                detail(main, "Main"),
                "CARD-1",
                date!(2026 - 01 - 01),
                Some(date!(2026 - 03 - 01)),
            )],
        };
        assert_eq!(
            directory
                .resolve_declared("CARD-1")
                .expect("the alias names one account")
                .id,
            main,
        );
    }

    #[test]
    fn a_declaration_naming_two_accounts_is_refused_and_says_which() {
        let directory = AccountDirectory {
            accounts: vec![
                with_identity(detail(account(1), "Main"), "ACC-1"),
                with_alias(
                    detail(account(2), "Savings"),
                    "ACC-1",
                    date!(2026 - 01 - 01),
                    None,
                ),
            ],
        };
        let error = directory
            .resolve_declared("ACC-1")
            .expect_err("two accounts answer to it");
        let AppError::Invalid {
            field,
            expected,
            actual,
        } = &error
        else {
            panic!("an ambiguous declaration is an invalid request: {error}");
        };
        assert_eq!(field, "source.account");
        assert!(
            actual.contains("Main") && actual.contains("Savings"),
            "an ambiguity the owner cannot see is one he cannot clear: {actual}"
        );
        assert!(
            actual.contains(&account(1).inner().to_string()),
            "and the identifiers are what he answers with: {actual}"
        );
        assert!(
            expected.contains("provider_account_id"),
            "the refusal has to say what would settle it: {expected}"
        );
    }

    #[test]
    fn a_declaration_naming_nothing_is_refused_as_a_stranger() {
        let directory = AccountDirectory {
            accounts: vec![detail(account(1), "Main")],
        };
        let error = directory
            .resolve_declared("ACC-9")
            .expect_err("no account answers to it");
        assert_eq!(error.code(), "invalid_request");
        assert!(
            error.to_string().contains("ACC-9"),
            "the refusal repeats what was sent: {error}"
        );
    }

    // --- what the owner's transfer statement does -------------------------

    #[test]
    fn a_resolution_the_owners_statement_denies_is_withdrawn() {
        let main = account(1);
        let checking = account(2);
        let savings = account(3);
        let resolver = stating(
            vec![
                detail(main, "Main"),
                detail(checking, "Checking"),
                detail(savings, "Savings"),
            ],
            vec![(main, vec![savings])],
        );
        match resolver.assess(&row(main, "Checking", None)) {
            Assessment::Ambiguous { question } => assert_eq!(
                question,
                Question::IsTransferInternal {
                    account: main,
                    counterparty: "Checking".to_owned(),
                },
                "the row goes back to being a question about a named counterparty"
            ),
            Assessment::Settled { classification, .. } => panic!(
                "the owner said money does not move between these two, so this \
                 must be asked and not derived: {classification:?}"
            ),
        }
    }

    #[test]
    fn a_resolution_the_owners_statement_names_stands() {
        let main = account(1);
        let checking = account(2);
        let resolver = stating(
            vec![detail(main, "Main"), detail(checking, "Checking")],
            vec![(main, vec![checking])],
        );
        match resolver.assess(&row(main, "Checking", None)) {
            Assessment::Settled { classification, .. } => assert_eq!(
                classification,
                Classification::InternalTransfer { to: checking }
            ),
            Assessment::Ambiguous { question } => {
                panic!("nothing here withdraws a declared pair: {question:?}")
            }
        }
    }

    #[test]
    fn a_statement_on_the_far_side_naming_this_account_leaves_the_resolution_standing() {
        let main = account(1);
        let checking = account(2);
        let savings = account(3);
        let resolver = stating(
            vec![
                detail(main, "Main"),
                detail(checking, "Checking"),
                detail(savings, "Savings"),
            ],
            vec![(main, vec![savings]), (checking, vec![main])],
        );
        match resolver.assess(&row(main, "Checking", None)) {
            Assessment::Settled { classification, .. } => assert_eq!(
                classification,
                Classification::InternalTransfer { to: checking },
                "he declared the pair from the other side, and the pair is the \
                 same pair whichever side he was asked about"
            ),
            Assessment::Ambiguous { question } => panic!("{question:?}"),
        }
    }

    #[test]
    fn an_account_the_owner_has_not_spoken_about_withdraws_nothing() {
        let main = account(1);
        let checking = account(2);
        let resolver = stating(
            vec![detail(main, "Main"), detail(checking, "Checking")],
            Vec::new(),
        );
        match resolver.assess(&row(main, "Checking", None)) {
            Assessment::Settled { classification, .. } => assert_eq!(
                classification,
                Classification::InternalTransfer { to: checking },
                "silence is not a denial, and it is the state a new account is in"
            ),
            Assessment::Ambiguous { question } => panic!("{question:?}"),
        }
    }

    #[test]
    fn a_statement_naming_no_partners_denies_every_own_account() {
        let main = account(1);
        let checking = account(2);
        let resolver = stating(
            vec![detail(main, "Main"), detail(checking, "Checking")],
            vec![(main, Vec::new())],
        );
        assert!(
            matches!(
                resolver.assess(&row(main, "Checking", None)),
                Assessment::Ambiguous { .. }
            ),
            "«none of my others» is an answer, and it answers this"
        );
    }

    // --- which way the money went ----------------------------------------

    #[test]
    fn a_directionless_internal_transfer_is_asked_and_not_derived() {
        // The row the source gave an amount and no direction for, whose
        // counterparty the directory does recognise. Recognising it settles
        // **what** the row is; it does not settle which way it ran. Deriving
        // `Out` from "the far side is not this account" is the guess
        // `question_for` refuses one function away — the answer would be
        // recorded and the guess made anyway, one step further along.
        let main = account(1);
        let checking = account(2);
        let resolver = resolver(vec![detail(main, "Main"), detail(checking, "Checking")]);

        match resolver.assess(&directionless(row(main, "Checking", None))) {
            Assessment::Ambiguous { question } => assert_eq!(
                question,
                Question::UnresolvedDirection {
                    account: main,
                    stated: Some("INNER".to_owned()),
                    counterparty: Some("Checking".to_owned()),
                },
                "the direction is what is open, and it is what is asked"
            ),
            Assessment::Settled {
                classification,
                movement,
            } => panic!(
                "the source stated no direction, so there is none to settle                  with: {classification:?} {movement:?}"
            ),
        }
    }

    #[test]
    fn a_rule_learned_from_an_incoming_transfer_does_not_make_a_row_outgoing() {
        // The other half of the same defect, and the one that shows the
        // derivation was wrong in **both** directions.
        // `Answer::ReceivedFromOwnAccount { from }` records the far side in the
        // rule, exactly as the outgoing answer does. A later directionless row
        // the rule matches would then derive `Out` for money that arrived.
        let main = account(1);
        let savings = account(2);
        let learned = Answer::ReceivedFromOwnAccount { from: savings };
        let rule = ClassificationRule {
            id: iaam_core::ids::ClassificationRuleId::new_random(),
            version: 1,
            matcher: RuleMatcher {
                counterparty_account: Some("Somebody".to_owned()),
                description_contains: None,
                kind: None,
            },
            outcome: learned.classification(),
        };
        let resolver = ruled(
            vec![detail(main, "Main"), detail(savings, "Savings")],
            vec![rule],
        );

        match resolver.assess(&directionless(row(main, "Somebody", None))) {
            Assessment::Ambiguous { question } => assert!(
                matches!(question, Question::UnresolvedDirection { .. }),
                "{question:?}"
            ),
            Assessment::Settled { movement, .. } => panic!(
                "the rule names the far side and no direction; the owner                  answered «received» when he wrote it: {movement:?}"
            ),
        }
    }

    #[test]
    fn a_row_whose_source_stated_a_direction_settles_without_a_question() {
        // The ordinary case, and the one that must not start asking: broadening
        // the question to every internal transfer would be a worse defect than
        // the one being fixed. The source said which way the money went, and
        // the directory said what the row is — nothing is open.
        let main = account(1);
        let checking = account(2);
        let resolver = resolver(vec![detail(main, "Main"), detail(checking, "Checking")]);

        match resolver.assess(&row(main, "Checking", None)) {
            Assessment::Settled {
                classification,
                movement,
            } => {
                assert_eq!(
                    classification,
                    Classification::InternalTransfer { to: checking }
                );
                assert_eq!(movement, Movement::Out, "the source printed «out»");
            }
            Assessment::Ambiguous { question } => {
                panic!("the source stated the direction: {question:?}")
            }
        }

        match resolver.assess(&incoming(row(main, "Checking", None))) {
            Assessment::Settled {
                classification,
                movement,
            } => {
                assert_eq!(
                    classification,
                    Classification::InternalTransfer { to: checking },
                    "the far side is the same account whichever way the row ran"
                );
                assert_eq!(movement, Movement::In, "and the source printed «in»");
            }
            Assessment::Ambiguous { question } => {
                panic!("the source stated the direction: {question:?}")
            }
        }
    }

    #[test]
    fn a_fee_and_income_still_settle_a_directionless_row() {
        // A fee and income are each a direction of their own: a fee leaves
        // the account and income arrives at it. Nothing about them is a guess,
        // and the fix must not take them away — a row settled as a fee has
        // never needed to be asked.
        let main = account(1);
        let fee = ruled(
            vec![detail(main, "Main")],
            vec![ClassificationRule {
                id: iaam_core::ids::ClassificationRuleId::new_random(),
                version: 1,
                matcher: RuleMatcher {
                    counterparty_account: Some("Somebody".to_owned()),
                    description_contains: None,
                    kind: None,
                },
                outcome: Classification::Fee {
                    origin: FeeOrigin::AccountMaintenance,
                },
            }],
        );

        match fee.assess(&directionless(row(main, "Somebody", None))) {
            Assessment::Settled { movement, .. } => assert_eq!(movement, Movement::Out),
            Assessment::Ambiguous { question } => {
                panic!("a fee leaves the account, and that is not a guess: {question:?}")
            }
        }
    }

    #[test]
    fn a_statement_does_not_break_a_tie_between_two_accounts_of_one_title() {
        let main = account(1);
        let declared = account(2);
        let other = account(3);
        let resolver = stating(
            vec![
                detail(main, "Main"),
                detail(declared, "Savings"),
                detail(other, "Savings"),
            ],
            vec![(main, vec![declared])],
        );
        assert_eq!(
            resolver.resolve_counterparty("Savings", None),
            None,
            "the statement says which pairs move money; it does not say which \
             account the printed string names, and assembling one conclusion \
             out of the two would be a fact about this row that neither states"
        );
        assert!(matches!(
            resolver.assess(&row(main, "Savings", None)),
            Assessment::Ambiguous { .. }
        ));
    }

    // -----------------------------------------------------------------------
    // What an answered question generalised into (iaam-ngwn)
    // -----------------------------------------------------------------------

    /// A session holding exactly one row and one question about it.
    ///
    /// Built by hand rather than through the store, because what is under test
    /// is a pure reading of what a session holds: the store's part is fetching
    /// these three strings, and a fake of it would only be able to hand back the
    /// same ones.
    fn session_with(
        observed: ObservedRow,
        answer: Option<Answer>,
        rule: Option<&str>,
    ) -> SessionContents {
        let session = ImportSessionId::new_random();
        let intake = Intake::Observed {
            row: Box::new(observed),
        };
        let answered = answer.map(|answer| serde_json::to_string(&answer).expect("an answer"));
        SessionContents {
            session: ImportSessionView {
                id: session,
                state: ImportSessionState::Open,
                account: None,
                source: None,
                import: None,
                opened_at: "2026-03-01T00:00:00Z".to_owned(),
                closed_at: None,
            },
            observations: vec![ImportObservationView {
                row: 1,
                row_key: None,
                concluded: false,
                payload: serde_json::to_string(&intake).expect("an intake"),
                answer: answered.clone(),
            }],
            questions: vec![ImportQuestionView {
                id: ImportQuestionId::new_random(),
                session,
                row: 1,
                question: "{}".to_owned(),
                alternatives: "[]".to_owned(),
                prompt: "Which of your accounts is Savings?".to_owned(),
                asked_at: "2026-03-01T00:00:00Z".to_owned(),
                answered_at: answered
                    .is_some()
                    .then(|| "2026-03-02T00:00:00Z".to_owned()),
                answer: answered,
                rule: rule.map(str::to_owned),
            }],
            control_figures: Vec::new(),
        }
    }

    /// A row the source printed nothing matchable on: no counterparty, no
    /// description, no word of its own.
    fn unmatchable(on: AccountId) -> ObservedRow {
        let mut row = row(on, "Savings", None);
        row.counterparty = ObservedCounterparty::Unknown;
        row.source_kind = None;
        row.description = None;
        row
    }

    #[test]
    fn an_open_question_has_generalised_nothing_yet() {
        let contents = session_with(row(account(1), "Savings", None), None, None);
        assert_eq!(
            generalisation_of(&contents, &contents.questions[0]),
            Generalisation::Unanswered
        );
    }

    #[test]
    fn an_answer_that_created_a_rule_names_it() {
        let contents = session_with(
            row(account(1), "Savings", None),
            Some(Answer::Paid),
            Some("11111111-1111-4111-8111-111111111111"),
        );
        assert_eq!(
            generalisation_of(&contents, &contents.questions[0]),
            Generalisation::Recorded {
                rule: "11111111-1111-4111-8111-111111111111".to_owned()
            }
        );
    }

    /// The defect this bead is about: an answered question with no rule, on a
    /// row that could perfectly well have made one.
    ///
    /// Before this, that state and «this row can never make a rule» were one
    /// absent field. Here it carries the rule itself, so the owner posts it
    /// rather than reconstructing it from the row.
    #[test]
    fn an_answer_the_answerer_could_not_generalise_carries_the_rule_it_would_have_made() {
        let contents = session_with(row(account(1), "Savings", None), Some(Answer::Paid), None);
        let Generalisation::Available { matcher, outcome } =
            generalisation_of(&contents, &contents.questions[0])
        else {
            panic!("a row naming a counterparty generalises");
        };
        assert_eq!(
            matcher.counterparty_account.as_deref(),
            Some("Savings"),
            "the matcher asks what the row printed"
        );
        assert_eq!(matcher.kind.as_deref(), Some("transfer"));
        assert_eq!(
            outcome,
            Classification::ExternalFlow,
            "the outcome is the classification the answer settled the row as"
        );
    }

    #[test]
    fn an_answer_on_a_row_with_nothing_to_match_on_can_never_generalise() {
        // The other half of the absence, and the one no call of the owner's can
        // change: a matcher that asks nothing matches nothing, so there is no
        // rule to offer him.
        let contents = session_with(unmatchable(account(1)), Some(Answer::Paid), None);
        assert_eq!(
            generalisation_of(&contents, &contents.questions[0]),
            Generalisation::Impossible
        );
    }

    #[test]
    fn every_generalisation_state_has_its_own_word() {
        // The four words are what a client branches on, and two of them
        // colliding would put the owner back where the absent field left him.
        let words: BTreeSet<&str> = [
            Generalisation::Unanswered.code(),
            Generalisation::Recorded {
                rule: String::new(),
            }
            .code(),
            Generalisation::Available {
                matcher: RuleMatcher {
                    counterparty_account: None,
                    description_contains: None,
                    kind: None,
                },
                outcome: Classification::ExternalFlow,
            }
            .code(),
            Generalisation::Impossible.code(),
        ]
        .into_iter()
        .collect();
        assert_eq!(words.len(), 4);
    }

    #[test]
    fn describing_a_session_answers_for_every_question_it_holds() {
        let contents = session_with(row(account(1), "Savings", None), Some(Answer::Paid), None);
        let described = generalisation_of(&contents, &contents.questions[0]);
        assert_eq!(described.code(), "available");
    }
}
