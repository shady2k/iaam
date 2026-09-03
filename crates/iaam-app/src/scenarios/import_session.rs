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

use iaam_core::event::kind::FeeOrigin;
use iaam_core::ids::{AccountId, ImportId, ImportQuestionId, ImportSessionId, OwnerId, SourceId};
use iaam_core::money::CurrencyCode;
use iaam_ingest::classification::{
    Answer, AnswerShape, Classification, ClassificationResult, ClassificationRule, Movement,
    Question, RuleMatcher, classify,
};
use iaam_ingest::observation::{Intake, ObservedRow};
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Rejection, SubmittedOperation, Verdict, normalize};
use sha2::{Digest, Sha256};

use crate::AppServices;
use crate::actions::{AccountScope, account_scope};
use crate::error::AppError;
use crate::ports::{
    AccountScopeExclusionView, AccountView, ContourView, ImportObservationView, ImportQuestionView,
    ImportSessionState, ImportSessionView, NewImportQuestion, Principal,
};
use crate::scenarios::ingest::submit_candidates;
use crate::scenarios::transfer_pairing::{self, LegOrigin, Proposals, TransferLeg};

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
}

impl SessionContents {
    /// Whether anything is still waiting on the owner.
    #[must_use]
    pub fn has_open_questions(&self) -> bool {
        self.questions.iter().any(ImportQuestionView::is_open)
    }
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
                .open_import_session(principal.owner, Some(source), import)
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
    let mut recorded = submit_candidates(services, principal, "operation", candidates)
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
pub async fn open_session(
    services: &AppServices,
    principal: &Principal,
    source: Option<SourceId>,
    import: Option<ImportId>,
) -> Result<ImportSessionView, AppError> {
    require_submit(principal)?;
    services
        .store
        .open_import_session(principal.owner, source, import)
        .await
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
pub async fn add_rows(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    rows: &[Intake],
) -> Result<Vec<HeldRow>, AppError> {
    require_submit(principal)?;
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

/// Record the owner's answer to one question.
///
/// Three things happen, in this order and for these reasons:
///
/// 1. The answer is checked against what the question actually offered. An
///    answer the question does not admit is a different mistake from a wrong
///    answer, and only the first can be refused.
/// 2. The decision is written as a durable [`ClassificationRule`], so the next
///    import of a matching row resolves without asking. A row that offers
///    nothing to match on — no counterparty, no description, no word from the
///    source — gets **no** rule, because a matcher that asks nothing matches
///    nothing and an "everything" rule would silently reclassify the portfolio.
/// 3. The answer is recorded on the question and on the row.
///
/// The journal is not touched. The answer settles what the row is; commit is
/// what records it.
pub async fn answer_question(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    question: ImportQuestionId,
    answer: Answer,
) -> Result<ImportQuestionView, AppError> {
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
        return Err(AppError::Invalid {
            field: "answer".to_owned(),
            expected: asked
                .alternatives()
                .iter()
                .map(|shape| shape.code())
                .collect::<Vec<_>>()
                .join(", "),
            actual: answer.shape().code().to_owned(),
        });
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

    let rule = match matcher_for(&observed) {
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

    services
        .store
        .answer_import_question(
            principal.owner,
            session,
            question,
            json(&answer, "answer")?,
            rule,
        )
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
pub async fn commit_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    revision: Option<&SessionRevision>,
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
        return Err(AppError::Invalid {
            field: "session".to_owned(),
            expected: "every question answered before the import is committed".to_owned(),
            actual: format!("{open} unanswered"),
        });
    }

    let verdicts = submit_candidates(services, principal, "operation", planned.candidates).await?;
    services
        .store
        .close_import_session(principal.owner, session, ImportSessionState::Committed)
        .await?;
    Ok(CommitOutcome {
        revision: planned.plan.revision,
        verdicts,
    })
}

/// What committing wrote, and under which reading of the session.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitOutcome {
    /// The revision the commit was planned from. A caller that supplied one has
    /// it echoed; a caller that supplied none learns what it committed.
    pub revision: SessionRevision,
    /// A verdict per held row, in the order the rows were fed.
    pub verdicts: Vec<Verdict>,
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
/// The seven sections are not a summary of the rows. Each answers a question the
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
    pub readiness: Readiness,
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
}

impl Readiness {
    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked { .. } => "blocked",
            Self::RequiresOwnerDecision { .. } => "requires_owner_decision",
        }
    }
}

/// Read the session and say what committing it would do.
///
/// Everything [`commit_session`] needs is decided here, and nothing is written.
/// The whole of iaam-k1xa is that this is one function rather than two: a
/// preview written beside the import is a second implementation of it, and it
/// describes a different import from the one that runs.
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

    let source = contents.session.source.unwrap_or_else(SourceId::new_random);
    let mut candidates = Vec::with_capacity(contents.observations.len());
    let mut read_rows = Vec::with_capacity(contents.observations.len());
    for observation in &contents.observations {
        let intake = parse_intake(&observation.payload).ok();
        let candidate = operation_of(observation, &resolver)
            .and_then(|operation| {
                normalize(
                    &operation,
                    NormalizationContext {
                        owner: principal.owner,
                        source,
                    },
                )
            })
            .map(|normalized| {
                let mut event = normalized.event;
                if let Some(import) = contents.session.import {
                    event.provenance = event.provenance.with_import(import);
                }
                event
            });
        read_rows.push(ReadRow {
            row: observation.row,
            intake,
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
            Err(rejection) => retained.push(RetainedRow {
                row: read.row,
                reason: open_questions
                    .iter()
                    .find(|open| open.row == read.row)
                    .map_or_else(
                        || RetentionReason::Unreadable {
                            field: rejection.field.clone(),
                            expected: rejection.expected.clone(),
                            actual: rejection.actual.clone(),
                        },
                        |open| RetentionReason::Unanswered {
                            question: open.question,
                        },
                    ),
            }),
        }
    }

    let resolved: Vec<PlannedFact> = facts.iter().chain(duplicates.iter()).cloned().collect();
    // The same matching pass the journal-level proposal runs, over the rows this
    // session is about to write. One function, so an owner shown a candidate
    // here is shown the same candidate after the commit.
    let legs: Vec<TransferLeg> = read_rows
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

    let readiness = if contents.session.state == ImportSessionState::Open {
        if open_questions.is_empty() && cross_source_matching.candidates.is_empty() {
            Readiness::Ready
        } else {
            Readiness::RequiresOwnerDecision {
                unanswered_questions: open_questions.len(),
                transfer_candidates: cross_source_matching.candidates.len(),
            }
        }
    } else {
        Readiness::Blocked {
            reason: format!(
                "the session is {}, and a session leaves «open» once",
                contents.session.state.code()
            ),
        }
    };

    let commit_delta = CommitDelta {
        facts,
        duplicates,
        retained_unrecorded: retained,
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
        &readiness,
    );

    Ok(PlannedSession {
        plan: ImportPlan {
            session: contents.session,
            revision,
            source_inventory,
            account_resolution,
            scope_assessment,
            interpretation,
            cross_source_matching,
            commit_delta,
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
    candidate: Result<iaam_core::event::Event, Rejection>,
}

impl ReadRow {
    /// The account the row is on, as the caller stated it.
    fn account(&self) -> Option<AccountId> {
        match self.intake.as_ref()? {
            Intake::Observed { row } => Some(row.account),
            Intake::Concluded { operation } => Some(operation.account),
        }
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
            let known = resolver.accounts.iter().any(|held| held.id == account);
            let bucket = if known { &mut resolved } else { &mut missing };
            if !bucket.contains(&account) {
                bucket.push(account);
            }
        }
        if let Some(Intake::Observed { row }) = read.intake.as_ref()
            && let Some(name) = row.counterparty_name()
            && resolver.counterparty_matches(name) > 1
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
    let effect = event
        .legs
        .iter()
        .filter(|leg| leg.account == account)
        .filter_map(|leg| leg.cash_effect())
        .fold(None::<iaam_core::money::Money>, |sum, money| match sum {
            None => Some(money),
            Some(sum) => sum.try_add(money).ok(),
        });
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
    let _ = writeln!(rendered, "readiness {readiness:?}");

    let digest = Sha256::digest(rendered.as_bytes());
    SessionRevision(digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    }))
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

/// The owner's accounts and rules, loaded once per batch.
struct Resolver {
    accounts: Vec<AccountView>,
    rules: Vec<ClassificationRule>,
}

impl Resolver {
    async fn load(services: &AppServices, owner: OwnerId) -> Result<Self, AppError> {
        let accounts = services.store.list_accounts(owner).await?;
        let stored = services.rules.list_rules(owner).await?;
        let mut rules = Vec::with_capacity(stored.len());
        for rule in stored.into_iter().filter(|rule| rule.retired_at.is_none()) {
            rules.push(crate::scenarios::classification::rule_from_view(rule)?);
        }
        Ok(Self { accounts, rules })
    }

    /// The owner's own account a printed counterparty names, if it names one.
    ///
    /// This is the seam the derived internal transfer comes through: a
    /// counterparty recognised here reaches `classify` as
    /// `Counterparty::OwnAccount` and settles without a question. Recognition is
    /// by identifier or by exactly one account title, case-insensitively — a
    /// title shared by two accounts recognises neither, because picking one
    /// would be the guess this module exists to refuse.
    fn resolve_counterparty(&self, name: &str) -> Option<AccountId> {
        if let Ok(id) = uuid::Uuid::parse_str(name)
            && let Some(account) = self
                .accounts
                .iter()
                .find(|account| account.id.inner() == id)
        {
            return Some(account.id);
        }
        let wanted = name.trim().to_lowercase();
        let mut matched = self
            .accounts
            .iter()
            .filter(|account| account.title.trim().to_lowercase() == wanted);
        let first = matched.next()?;
        matched.next().is_none().then_some(first.id)
    }

    /// How many of the owner's accounts a printed counterparty could be.
    ///
    /// [`Self::resolve_counterparty`] answers `None` both for a name that
    /// matches nothing and for one that matches two accounts, and the two are
    /// not the same thing to report: the first is a stranger and the second is
    /// an ambiguity the owner can clear up by renaming an account.
    fn counterparty_matches(&self, name: &str) -> usize {
        if uuid::Uuid::parse_str(name)
            .is_ok_and(|id| self.accounts.iter().any(|account| account.id.inner() == id))
        {
            return 1;
        }
        let wanted = name.trim().to_lowercase();
        self.accounts
            .iter()
            .filter(|account| account.title.trim().to_lowercase() == wanted)
            .count()
    }

    fn title(&self, account: AccountId) -> String {
        self.accounts
            .iter()
            .find(|known| known.id == account)
            .map_or_else(|| account.inner().to_string(), |known| known.title.clone())
    }

    /// Settle the row, or name what has to be asked.
    ///
    /// Two things have to be true before a row can be recorded: **what** it was
    /// and **which way** the money went. `classify` answers the first. The
    /// second comes from the source when it stated a direction, and otherwise
    /// from the classification itself, which carries one for three of its four
    /// outcomes: a fee leaves, income arrives, and an internal transfer's
    /// direction is read from which side of it this account is on.
    ///
    /// `ExternalFlow` is the one outcome that carries no direction. A
    /// directionless row settled as an external flow is therefore still asked
    /// about — the alternative is picking `deposit`, which is the bug.
    fn assess(&self, row: &ObservedRow) -> Assessment {
        let resolved = row
            .counterparty_name()
            .and_then(|name| self.resolve_counterparty(name));
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
            Question::IsInflowIncome { .. } => format!(
                "Money arrived at {account} and the source named no counterparty. \
                 Was it income, or money coming back?"
            ),
            Question::UnresolvedDirection {
                stated,
                counterparty,
                ..
            } => {
                let word = stated
                    .as_deref()
                    .map_or_else(|| "no direction".to_owned(), |stated| format!("«{stated}»"));
                let other = counterparty.as_deref().map_or_else(
                    || "named no counterparty".to_owned(),
                    |name| format!("named «{name}» as the other side"),
                );
                format!(
                    "On {account}, the source stated {word} and {other}, \
                     so neither which way the money went nor the account on the \
                     other side can be read from the row. Which was it?"
                )
            }
        }
    }
}

/// Which way the money went, when anything says so.
fn movement_of(classification: Classification, row: &ObservedRow) -> Option<Movement> {
    if let Some(movement) = row.movement() {
        return Some(movement);
    }
    match classification {
        // The account a rule names is the far side of the movement: equal to
        // this row's account it means the money arrived, different from it that
        // the money left.
        Classification::InternalTransfer { to } => Some(if to == row.account {
            Movement::In
        } else {
            Movement::Out
        }),
        Classification::Fee { .. } => Some(Movement::Out),
        Classification::Income => Some(Movement::In),
        Classification::ExternalFlow => None,
    }
}

/// The account an answer names, when it names one.
const fn named_account(answer: Answer) -> Option<AccountId> {
    match answer {
        Answer::SentToOwnAccount { to } => Some(to),
        Answer::ReceivedFromOwnAccount { from } => Some(from),
        Answer::Paid | Answer::Received | Answer::Fee { .. } | Answer::Income => None,
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

fn matcher_json(matcher: &RuleMatcher) -> serde_json::Value {
    serde_json::json!({
        "counterparty_account": matcher.counterparty_account,
        "description_contains": matcher.description_contains,
        "kind": matcher.kind,
    })
}

/// A classification in the vocabulary the rule store keeps it in.
///
/// The inverse of the parser in [`crate::scenarios::classification`], and it must
/// stay so: a rule written in words that parser cannot read is a decision the
/// owner can never see again.
fn outcome_json(classification: Classification) -> serde_json::Value {
    match classification {
        Classification::InternalTransfer { to } => serde_json::json!({
            "kind": "internal_transfer",
            "to": to.inner().to_string(),
        }),
        Classification::ExternalFlow => serde_json::json!({ "kind": "external_flow" }),
        Classification::Income => serde_json::json!({ "kind": "income" }),
        Classification::Fee { origin } => serde_json::json!({
            "kind": "fee",
            "origin": match origin {
                FeeOrigin::Brokerage => "brokerage",
                FeeOrigin::Depositary => "depositary",
                FeeOrigin::AccountMaintenance => "account_maintenance",
                FeeOrigin::MarginInterest => "margin_interest",
                FeeOrigin::Other => "other",
            },
        }),
    }
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
