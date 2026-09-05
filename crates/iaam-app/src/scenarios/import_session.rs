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

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::batch::{
    self, BatchMovement, BatchTotal, ControlCheck, ControlComparison, ControlSection,
};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::correction::resolve;
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RuleSettlement};
use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
use iaam_core::event::{
    Confidence, Event, Relation, SCHEMA_VERSION, SOURCE_CATEGORY_IS_A_CATEGORY_FROM,
};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, EventId, ImportId, ImportQuestionId, ImportSessionId, OwnerId,
    SourceId,
};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_ingest::classification::{
    Answer, AnswerShape, Basis, Classification, ClassificationResult, ClassificationRule,
    ClassificationSubject, Counterparty, Movement, Question, QuestionSubject, RuleMatcher,
    classification_of, classify,
};
use iaam_ingest::csv_source::{AccountEntry, AccountNames, UnresolvedAccount};
use iaam_ingest::mirror::{MirrorSide, Unpaired, mirrored};
use iaam_ingest::observation::{Intake, ObservedDirection, ObservedRow};
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Rejection, SubmittedOperation, Verdict, normalize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppServices;
use crate::actions::{
    AccountCandidate, AccountScope, OperationKey, OwnerQuestion, RequestPlan, ResolutionOption,
    account_scope, answer_account_candidates, answer_input,
};
use crate::error::{AppError, FieldRejection};
use iaam_ingest::dedup::IdentityScope;

use crate::ports::{
    AccountDetailView, AccountScopeExclusionView, AccountTransferStatementView, ContourView,
    ImportObservationView, ImportQuestionView, ImportSessionState, ImportSessionSummaryView,
    ImportSessionView, NewImportQuestion, Principal, Recorded, UnresolvedAccountView,
};
use crate::scenarios::classification::{
    ClassifiedAs, classified_as, matcher_json, outcome_json, subject,
};
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
    /// Held, and it will produce no fact at all — which is correct for it.
    ///
    /// Told here rather than only in the plan, because the caller is entitled
    /// to know at the moment it feeds the row: read as [`Self::Held`] it would
    /// promise a fact the commit will not write, and the caller would have to
    /// discover the difference by comparing counts.
    Settled { row: u32, reason: NoFactReason },
    /// Not held: the row could not be read, so there was nothing to hold.
    Rejected { row: u32, rejection: Rejection },
}

impl HeldRow {
    /// The row's position in the session, whatever became of it.
    #[must_use]
    pub const fn row(&self) -> u32 {
        match self {
            Self::Held { row } | Self::Rejected { row, .. } | Self::Settled { row, .. } => *row,
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

// `SessionContents::has_open_questions` was here, and it is gone (`iaam-m2oi`).
// It answered from `ImportQuestionView::is_open` alone, which is «the owner has
// not answered it» and not «it is still waiting on him»: a standing rule he
// wrote after the question was recorded settles the row without touching the
// question, and the stored question then says the session is blocked by a
// decision nothing needs. Nothing that holds only `SessionContents` can answer
// the real question, because the answer is a property of the session's
// **reading** and not of its rows — see [`QuestionSettlements`], which is where
// every reader of a question's openness now goes. A method that can only give
// the wrong answer is worse than no method: it is the wrong answer with the
// right name on it.

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
    /// What settled the row without the owner ever answering this question.
    ///
    /// **`None` is the ordinary state and means two different things on
    /// purpose** (`iaam-m2oi`): the question is still waiting on him, or he
    /// answered it. The second is stated by the question itself —
    /// `answered_at` — and this field says nothing about it, because «he
    /// answered» and «something else settled it» are the two ways a question
    /// stops waiting and only the second needs explaining.
    ///
    /// **Published on the question rather than left to be inferred.** A caller
    /// holding a question whose `answered_at` is null used to have exactly one
    /// reading of it — the owner must answer this — and after a standing rule
    /// of his settles the row that reading is wrong in the direction that costs
    /// him an evening: it puts a decision to him that his own earlier decision
    /// already made. The word is [`QuestionSettlement`]'s and is the same word
    /// the assessment puts on the row, so a caller that reads both reads one
    /// determination twice rather than two that can differ.
    pub settled_without_answer: Option<QuestionSettlement>,
}

impl AnswerableQuestion {
    /// Whether this question is still the owner's to answer.
    ///
    /// The one predicate a caller should branch on, and the reason the
    /// settlement is published beside the question rather than computed by
    /// each caller out of `answered_at` — which answers a different question
    /// and answered it wrongly for every row a rule settled.
    #[must_use]
    pub const fn awaits_answer(&self) -> bool {
        self.view.is_open() && self.settled_without_answer.is_none()
    }
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
/// questions must not pay for it.
///
/// **It reads the session, and that is new** (`iaam-m2oi`). Whether a question
/// is still waiting on the owner is not written in the questions table — a
/// standing rule of his settles the row and touches no question — so a view of
/// a question that did not read the session could only publish `answered_at`
/// and let its caller draw the wrong conclusion. What that costs is his
/// directory, his transfer statements and his rules, once for the whole list;
/// what it buys is that a caller relaying these questions to him relays the ones
/// that are genuinely his to answer. The commit planner reads the same session
/// the same way and reaches the same set, because both go through
/// [`QuestionSettlements`].
pub async fn answerable_questions(
    services: &AppServices,
    principal: &Principal,
    contents: &SessionContents,
    questions: &[ImportQuestionView],
) -> Result<Vec<AnswerableQuestion>, AppError> {
    // One reading of the session, so that what a published question says about
    // itself and what the assessment says about the same row are one
    // determination (`iaam-m2oi`). It costs the owner's accounts, his transfer
    // statements and his rules — not his journal, which is what
    // [`SessionReading`] deliberately stops short of.
    let settlements = SessionReading::of(services, principal, contents)
        .await?
        .settlements();
    let asked: Vec<Option<Question>> = questions
        .iter()
        .map(|question| {
            question
                .is_open()
                .then(|| stored_question(question))
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
            // Still offered for a question a rule settled, and that is the
            // decision rather than an oversight. His standing rule classifies
            // the row; his word about *this* row overrules it, because
            // `resolution_of` reads the stored answer before it consults the
            // rules at all. So the call stays open to him and the answer keeps
            // meaning what it meant — what he is no longer told is that he must
            // make it.
            accounts: asked.map_or_else(Vec::new, |asked| {
                answer_account_candidates(&asked, &accounts)
            }),
            generalisation: generalisation_of(&contents.observations, question),
            settled_without_answer: settlements.settlement_of(question).cloned(),
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
    // The settlement travels beside the operation rather than inside it, for
    // `RowResolution::Fact`'s reason: the operation is what will be written, and
    // this is why it was written that way. It is carried this far rather than
    // read off the assessment again at the end, because by then the row is a
    // normalised event and the reading that settled it is gone.
    let mut settled: Vec<Option<(Result<SubmittedOperation, Rejection>, RuleSettlement)>> =
        Vec::new();
    let mut pending: Vec<Option<(&ObservedRow, Question)>> = Vec::new();
    // The third outcome, kept in its own list rather than folded into either of
    // the two above: such a row opens no session, because there is nothing to
    // ask, and produces no candidate, because there is nothing to write.
    let mut no_fact: Vec<Option<NoFactReason>> = Vec::new();
    for intake in rows {
        match intake {
            Intake::Concluded { operation } => {
                // A caller that concluded settled the row itself, so no rule of
                // his filed it. That is a statement, not a silence: it is the
                // reading this route performed, and the fact records it.
                settled.push(Some((
                    Ok((**operation).clone()),
                    FactBasis::Concluded.rule_settlement(),
                )));
                pending.push(None);
                no_fact.push(None);
            }
            Intake::Observed { row, .. } => match resolver.assess(row) {
                Assessment::Settled {
                    classification,
                    movement,
                    basis,
                } => {
                    settled.push(Some((
                        row.resolve(classification, movement),
                        FactBasis::of(&basis).rule_settlement(),
                    )));
                    pending.push(None);
                    no_fact.push(None);
                }
                Assessment::NoFact { reason } => {
                    settled.push(None);
                    pending.push(None);
                    no_fact.push(Some(reason));
                }
                Assessment::Ambiguous { question } => {
                    settled.push(None);
                    pending.push(Some((row.as_ref(), question)));
                    no_fact.push(None);
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
        .zip(rows)
        .filter_map(|(settled, intake)| {
            settled
                .as_ref()
                .map(|(operation, settlement)| (operation, *settlement, intake))
        })
        .map(|(operation, settlement, intake)| {
            operation.clone().and_then(|operation| {
                normalize(
                    &operation,
                    &NormalizationContext {
                        owner: principal.owner,
                        source,
                        // What read the row, and not what submitted it. A
                        // caller that stated the row itself is its reader and
                        // records `ingest/manual/1`; a reader inside this
                        // product — today, the source-profile engine — names
                        // itself on the intake, and the fact records that
                        // instead. The field has no default, so a reader that
                        // forgets does not compile (`iaam-h69n`).
                        parser_version: reader_of(intake),
                    },
                )
                .map(|normalized| {
                    let mut event = normalized.event;
                    if let Some(import) = import {
                        event.provenance = event.provenance.with_import(import);
                    }
                    event.provenance = event.provenance.with_rule_settlement(settlement);
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
        // Before the question, because a row settled without a fact raised
        // none: it is answered here and nothing is parked.
        if let Some(reason) = no_fact[index] {
            outcomes.push(IntakeOutcome {
                verdict: Verdict::NoFact {
                    reason: reason.code().to_owned(),
                },
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
    // The observation the question was raised from. The wording needs it: a
    // question alone does not say which row of a statement it is about, and
    // four rows of one export produced four identical sentences before it did
    // (`iaam-3ewp`) — see [`Resolver::render`].
    //
    // Matched here rather than taken as a seventh argument, and the arm is
    // written out rather than assumed. Only an observation can be unsettled — a
    // conclusion is recorded, not questioned — so this is unreachable, and an
    // invariant refused in one line is one a later caller cannot break by
    // handing the wrong half of the pair along.
    let Intake::Observed { row, .. } = intake else {
        return Err(AppError::Store(
            "a concluded row reached the question path".to_owned(),
        ));
    };
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
    let prompt = resolver.render(question, row);
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
/// **Only a declaration reaches this refusal**, and an undeclared open reaches
/// nothing: `standing_session` answers `None` for it, so two free sessions
/// can hold one statement at once. That is deliberate and its reasoning is on
/// that function, together with where the condition is caught instead.
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
///
/// # A declaration naming neither is found by nothing, and stays that way
///
/// `(None, None)` answering `None` means an undeclared open bypasses
/// [`half_imported_refusal`] entirely: two undeclared sessions can hold one
/// statement at the same time, and each will commit it (iaam-56vq). Weighed and
/// kept, and the reasons are worth writing down because the arm looks like an
/// oversight.
///
/// **There is no import for it to be half-way through.** The refusal above is
/// about one *declared import*: the store hands the same session back for the
/// same declaration, so a second open would mix a second file's rows into a
/// statement somebody is part-way through answering questions about. An
/// undeclared open declares no import and the store opens a fresh session every
/// time, exactly as it documents — so nothing is mixed, and the harm the
/// refusal prevents does not arise here. What can go wrong is a different
/// thing: two sessions, two commits, one statement in the journal twice.
///
/// **The refusal could not be written truthfully.** An undeclared session does
/// have a stable identity — [`session_origin`] derives it — but not at the
/// moment this runs: the identity is keyed on an account, the rows have not
/// arrived, and the declaration is what would have named one. So the only
/// refusal available is «you have another undeclared session open, holding
/// rows», which names nothing the two have in common. It would fire between an
/// export of one institution and an export of another, and a free session is
/// opened without a declaration *precisely* so that an institution-wide export
/// is one session and not four. A refusal that is wrong every time the owner
/// does the thing the mechanism exists for is worse than the hole it closes.
///
/// **What catches it instead is the assessment, at the moment it becomes
/// true.** Two open sessions holding one statement is not yet a defect;
/// committing both is. After the first commit the second session's rows are
/// measured against a journal that now holds them: recognised by key as
/// [`CommitDelta::duplicates`] — including across the two sessions, since
/// [`session_origin`] derives one source per account and the source operation
/// identifier is scoped by it — and, where the rows carry no key at all, named
/// in [`CommitDelta::resembles_recorded`] with the readiness word to match.
/// That is later than a refusal and it is the first point at which anything
/// true can be said.
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
        // The counts the listing carries are not read here: this dispatch asks
        // which session a declaration reaches, and what that session holds is
        // the refusal's business rather than the recognition's.
        .map(|summary| summary.session)
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
    // What the session is **waiting on**, and not what its questions table says
    // was never answered (`iaam-m2oi`). A refusal that offered a question a
    // standing rule of his had already settled sent the owner to answer work
    // that was done, and its count named a wall that was not there.
    //
    // A reading that fails leaves the refusal counting stored questions, which
    // is what it counted before and can only over-state. This is the tolerance
    // the neighbouring refusal already applies to the accounts it names: what
    // was wrong with the request does not change because the extra detail could
    // not be computed.
    let settlements = SessionReading::of(services, principal, contents)
        .await
        .map(|reading| reading.settlements())
        .unwrap_or_default();
    let unanswered = settlements.awaiting(&contents.questions);
    // Singular where there is one. The field is read out to whoever is being
    // told why the import will not start, and «1 questions» is a sentence
    // nobody wrote on purpose.
    let waiting = if unanswered == 1 {
        "1 question still waiting on you".to_owned()
    } else {
        format!("{unanswered} questions still waiting on you")
    };
    let rejection = FieldRejection::new(
        "source.label",
        "a label naming an import with no session open, or one of the calls that          ends the session this label already has",
        format!(
            "session {session} has been open since {opened}, holding {rows} rows and \
             {waiting}",
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
        .filter(|question| settlements.awaits_answer(question))
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

/// Every session of the owner's, newest first, with how much each holds.
///
/// The counts travel with the headers, and that is the whole of this
/// function's shape. A list of headers alone answers «which sessions exist»
/// and leaves «which of them is waiting on me» to one request per session — a
/// cost a caller pays by not paying it, concluding from a list it did not walk
/// that nothing is outstanding. Both numbers are read in the same store
/// statement as the headers, so the honest answer costs what the incomplete
/// one did.
pub async fn list_sessions(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<ImportSessionSummaryView>, AppError> {
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
        let Intake::Observed { row, .. } = intake else {
            outcomes.push(HeldRow::Held {
                row: observation.row,
            });
            continue;
        };
        match resolver.assess(row) {
            Assessment::Settled { .. } => outcomes.push(HeldRow::Held {
                row: observation.row,
            }),
            Assessment::NoFact { reason } => outcomes.push(HeldRow::Settled {
                row: observation.row,
                reason,
            }),
            Assessment::Ambiguous { question } => {
                let prompt = resolver.render(&question, row);
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
/// Five states, and only four of them can be true of an answered question.
///
/// **Why states and not an `Option<rule>`.** The rule identifier alone cannot say
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
    /// description, no word from the source and no category the source filed it
    /// under generalises into nothing, and there is no call the owner could make
    /// that would change that.
    Impossible,
    /// The row could have grounded a rule and the **answer** is not one to
    /// generalise (`iaam-axrf`).
    ///
    /// Kept apart from [`Self::Impossible`] because the two absences have
    /// nothing in common except being absences, and `impossible` makes a claim
    /// about the **row** — that it prints nothing a matcher could ask about —
    /// which is false here and which a client can act on wrongly: it is the one
    /// state that says no call of anybody's will ever produce a rule for this
    /// line, and the row here may well ground one for a different answer given
    /// on a later import.
    ///
    /// It is not [`Self::Available`] either, and that is the load-bearing half.
    /// `available` publishes the rule for the owner to adopt and the queue
    /// offers him the act; this state has nothing to offer, because there is
    /// nothing here anybody should make stand. Which answers these are and why
    /// is `AnswerShape::generalises`, and it is asked of the answer rather than
    /// restated here.
    DoesNotGeneralise,
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
            Self::DoesNotGeneralise => "does_not_generalise",
        }
    }
}

/// What answering will decide beyond this session, before it is answered.
///
/// [`Generalisation`] is the same question asked afterwards, and it cannot
/// answer this one: an unanswered question is [`Generalisation::Unanswered`] by
/// construction. Two things are known before the answer and they are the whole
/// of it — whether the row can ground a matcher at all, and whether the caller
/// may generalise (`iaam-hnod`).
///
/// **One derivation, because two sentences.** The queue's item and the
/// assessment's group proposal both have to say this. They said opposite things
/// — the queue that the answer is written as a rule, the group that no standing
/// decision is kept — while each was right about one case and neither was
/// conditional, so a reader had no way to tell which case he was in. A caller
/// that read both had to choose, and choosing wrong is how an owner came to be
/// told a rule would not exist that would exist.
///
/// **The sentences differ and the value does not.** A surface reporting *about*
/// the owner and a surface speaking *to* him are two registers, and one string
/// serving both would be wrong in one of them; so the two spellings live here,
/// side by side, off one value. Nothing downstream re-derives which of the three
/// it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralisationProspect {
    /// The answer is written as a rule, and a row matching it settles by itself
    /// next time.
    WillStand,
    /// The answer settles this row and writes no rule, because the answerer may
    /// not generalise. The rule is the owner's to make stand.
    NeedsHisAdoption,
    /// No rule can be built from this row under any token: a matcher that asks
    /// nothing matches nothing.
    NoneFromThisRow,
}

impl GeneralisationProspect {
    /// The sentence for a surface reporting about the owner.
    #[must_use]
    pub const fn reported(self) -> &'static str {
        match self {
            Self::WillStand => {
                "The answer is written as a rule, so a row matching it settles by itself next time."
            }
            Self::NeedsHisAdoption => {
                "The answer settles this row and writes no rule: the rule it would have been is \
                 published with the answer, and making it stand is the owner's own act."
            }
            Self::NoneFromThisRow => {
                "The answer settles this row and nothing else: this row carries nothing a rule \
                 could match on, so no later row settles by itself because of it."
            }
        }
    }

    /// The same fact in the second person, for a sentence the owner is read.
    ///
    /// **Not the sentence above with the pronouns changed.** Decision 0027's
    /// register holds here and is enforced mechanically — «rule» is this
    /// system's word for what he calls a standing decision, and the guard on
    /// [`decision_group_question`] refuses it along with every other word that
    /// exists only because of how this is built.
    #[must_use]
    pub const fn addressed(self) -> &'static str {
        match self {
            Self::WillStand => {
                "Your answer is also kept as a standing decision, so a line matching it in a \
                 later statement is settled without asking you again."
            }
            Self::NeedsHisAdoption => {
                "Your answer keeps no standing decision, because only you may make one: the \
                 standing decision it would have been is published beside the answer, and one \
                 call of your own makes it stand."
            }
            Self::NoneFromThisRow => {
                "Your answer keeps no standing decision, because these lines carry nothing a \
                 later line could be matched against: no line of a later statement is settled \
                 by it."
            }
        }
    }
}

/// What answering one question will decide beyond this session.
///
/// The two facts it reads are the two that exist before an answer does. The
/// subject is `None` where this build cannot read the row, and a row it can read
/// still grounds no rule when it prints nothing a matcher could ask about — both
/// are «no rule can be built from this», which is the same pair
/// [`generalisation_of`] folds into [`Generalisation::Impossible`]. Groundedness
/// is asked of [`matcher_from`] and not restated here, because a second spelling
/// of that field policy is a second answer to what a rule can be built from.
#[must_use]
pub fn generalisation_ahead(
    subject: Option<&ClassificationSubject>,
    may_generalise: bool,
) -> GeneralisationProspect {
    match subject.and_then(matcher_from) {
        None => GeneralisationProspect::NoneFromThisRow,
        Some(_) if may_generalise => GeneralisationProspect::WillStand,
        Some(_) => GeneralisationProspect::NeedsHisAdoption,
    }
}

/// What one question's answer did, or could still do, to the standing rules.
///
/// The order of the four tests is the decision. A written rule settles it
/// whatever the row says, because that rule exists and the row is no longer the
/// evidence. Then a question he has not answered, which has no answer to
/// generalise. Then the answer itself, because one of the eight is not a claim
/// about every row like this one and is therefore never made into a rule
/// (`AnswerShape::generalises`) — asked here rather than after the row, so that
/// a row this build cannot read cannot turn that answer into `Impossible`.
/// Only then is the row consulted, and a row this build cannot read falls to
/// `Impossible` beside a row that asks nothing — which is not a fudge: «no rule
/// can be built from this» is true of both, and the assessment is where an
/// unreadable row is reported as unreadable.
///
/// **`is_open` is the right test here and is not the one `iaam-m2oi` replaced.**
/// This asks what the owner's *answer* generalised into, and a question he never
/// answered has no answer to generalise however his standing rules have since
/// settled the row. What that bead replaced is «is anything still waiting on
/// him», which is a different question with a different answer — and a row a
/// rule settled reports `unanswered` here truthfully, because there is nothing
/// of his to turn into a rule and a rule already covers the row.
///
/// Public, and taking the session's observations rather than the whole
/// [`SessionContents`], because the action queue derives the same state from the
/// same three facts. It reads the session's questions and observations out of
/// the store directly — it never loads a session — and a second derivation there
/// would be a second answer to «what did this answer generalise into», which is
/// the one thing this type exists to have only one of.
pub fn generalisation_of(
    observations: &[ImportObservationView],
    question: &ImportQuestionView,
) -> Generalisation {
    if let Some(rule) = question.rule.clone() {
        return Generalisation::Recorded { rule };
    }
    if question.is_open() {
        return Generalisation::Unanswered;
    }
    let answer: Option<Answer> = question
        .answer
        .as_deref()
        .and_then(|stored| serde_json::from_str(stored).ok());
    // Asked of the answer alone, and before the row is read. Whether an answer
    // is one to generalise is a property of the word he said and of nothing
    // else, so a row this build cannot read does not turn the truthful
    // `does_not_generalise` into the false `impossible`.
    if answer.is_some_and(|answer| !answer.shape().generalises()) {
        return Generalisation::DoesNotGeneralise;
    }
    observed_row(observations, question.row)
        .ok()
        .and_then(|observed| {
            let matcher = matcher_for(&observed)?;
            Some(Generalisation::Available {
                matcher,
                outcome: answer?.classification(),
            })
        })
        .unwrap_or(Generalisation::Impossible)
}

/// The row one question is about, as the classifier asks about it.
///
/// `None` where this build cannot read the row, which is the same absence
/// [`generalisation_of`] turns into [`Generalisation::Impossible`]: a row nothing
/// can read is a row no rule can be tested against.
///
/// **Nothing is resolved into it.** [`ObservedRow::subject`] takes the account
/// the owner's directory made of the printed counterparty, and this passes
/// `None` — so the counterparty stays the name the source printed. That is not a
/// shortcut around a directory read: it is the only reading under which a
/// standing rule can be tested at all, because [`RuleMatcher::matches`] compares
/// `counterparty_account` against a printed name and never against one of the
/// owner's accounts, and [`matcher_for`] proposes the printed name for the same
/// reason. A subject built with the resolution would be tested against rules
/// written without it, and would answer «no rule matches» for every rule that
/// does.
#[must_use]
pub fn subject_of(
    observations: &[ImportObservationView],
    question: &ImportQuestionView,
) -> Option<ClassificationSubject> {
    observed_row(observations, question.row)
        .ok()
        .map(|observed| observed.subject(None))
}

/// What was published as answerable when this question was asked.
///
/// **One reader, because there are four publishers** (`iaam-ulib`). The ingest
/// verdict, the held row, the question route and — since decision 0029 — the
/// session's assessment all say what may be said to one question, and four
/// readings of one stored string is four chances for a surface to offer a word
/// the stored question does not admit. `answer_question` refuses such a word, so
/// the drift would show up as a caller being told to send something the server
/// then rejects.
///
/// **Stored and not recomputed.** `Question::alternatives` answers for the
/// question *this build* would ask; the stored list is what the owner was
/// offered. They differ exactly when the vocabulary has changed under a session
/// that was already open, which is the moment publishing the recomputed list
/// would offer a word the answer path still measures against the stored one.
///
/// A stored list this build cannot read yields none, which is what the callers
/// that read it have always done with it: a question published with an empty
/// list says «nothing may be said to this», which is visibly wrong and sends the
/// reader to the question route, where the same absence is visible.
#[must_use]
pub fn stored_alternatives(question: &ImportQuestionView) -> Vec<AnswerShape> {
    serde_json::from_str(&question.alternatives).unwrap_or_default()
}

/// The question as it was asked, where this build can still read it.
///
/// Lenient, and every caller here is a reader rather than a writer: a stored
/// question this build cannot parse is a question that can be published, shown
/// and answered by its stored prompt and alternatives, and only the grouping and
/// the account candidates are lost. `answer_question` reads the same string
/// strictly, because writing an answer against a question nobody can read is a
/// different act.
#[must_use]
pub fn stored_question(question: &ImportQuestionView) -> Option<Question> {
    serde_json::from_str(&question.question).ok()
}

/// The decision one open question puts, where both halves can be read.
///
/// `None` for a question this build cannot parse and for one whose row it cannot
/// read. Such a question is grouped with nothing and is published alone —
/// which is the honest answer for a question nobody can compare, and never
/// «this one is unlike every other», which the absence would otherwise be
/// mistaken for.
fn subject_asked(
    observations: &[ImportObservationView],
    question: &ImportQuestionView,
) -> Option<QuestionSubject> {
    let row = observed_row(observations, question.row).ok()?;
    decision_of_read_row(question, &row)
}

/// The same decision, for a caller that has already read the row.
///
/// Named for what it reads rather than for what it returns, because
/// `subject_of` was already taken by the *classification* subject a rule is
/// tested against — a different thing under a name that would have fitted both
/// (`iaam-93lz`'s reader against this one). Two functions sharing a name is the
/// drift this module argues against everywhere else.
///
/// Split out of [`subject_asked`] rather than copied into the one caller that
/// reads the rows itself: what makes two questions one decision is a rule, and a
/// second spelling of it beside the first is two rules that both look right —
/// which is the argument `QuestionSubject` is written with and the reason
/// [`stored_alternatives`] is one function.
fn decision_of_read_row(
    question: &ImportQuestionView,
    row: &ObservedRow,
) -> Option<QuestionSubject> {
    Some(stored_question(question)?.about(row.movement()))
}

/// Record the owner's answer to one question.
///
/// Three things happen, in this order and for these reasons:
///
/// 1. The answer is checked against what the question actually offered. An
///    answer the question does not admit is a different mistake from a wrong
///    answer, and only the first can be refused.
/// 2. The answer is recorded on the question and on the row.
/// 3. If the answerer may generalise, the decision is **then** written as a
///    durable [`ClassificationRule`] and named on the question, so the next
///    import of a matching row resolves without asking. See [`may_generalise`]:
///    settling this row is import mechanics, and standing rules are the owner's
///    judgement. A row that offers nothing to match on — no counterparty, no
///    description, no operation word and no category of the source's own —
///    gets **no** rule either way, because a matcher that asks nothing matches
///    nothing and an "everything" rule would silently reclassify the portfolio.
///
/// Steps 2 and 3 were the other way round until `iaam-77hk`, and the swap is the
/// whole of that bead: the answer is the owner's fact and the rule is derived
/// from it, so the derived one is written second and a failure costs the
/// derivation rather than the fact. The reasoning is at the call, together with
/// what remains possible now that it cannot be made one transaction.
///
/// **`reach` decides how many rows this settles and never whether a rule is
/// written** (`iaam-q5og`, decision 0029). [`AnswerReach::ThisRow`] is the
/// default and is what this call has always done.
/// [`AnswerReach::EveryLikeRowInThisSession`] records the same answer against
/// every question still open in this session that is the same decision — a
/// question equal to this one, about a row the source stated the same direction
/// for. Those rows are checked before anything is written and the call is
/// refused whole if one of them cannot take the answer; step 3 above is
/// unchanged by any of it, and at most one rule is ever written, from the row
/// the caller addressed.
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
    reach: AnswerReach,
) -> Result<AnsweredQuestions, AppError> {
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
    // An answer naming an account must name one of the owner's. That is the
    // whole of this check, and the comment used to claim a second half — «and
    // it must not name the account the row is already on» — beside code that
    // performs no such test. The rule is real and is enforced twice, in
    // `ObservedRow::resolve` below and again in `Event::validate_transfer`; it
    // is not enforced here, and a reader who trusted the old wording would go
    // looking for the missing check rather than for the two that exist.
    if let Some(account) = named_account(answer) {
        let accounts = services.store.list_accounts(principal.owner).await?;
        if !accounts.iter().any(|known| known.id == account) {
            return Err(AppError::NotFound {
                what: "an account of the owner's",
                id: account.inner().to_string(),
            });
        }
    }

    let observed = observed_row(&contents.observations, stored.row)?;
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

    // The other rows this answer reaches, chosen and checked **before** anything
    // is written.
    //
    // Checked, because a caller saying «and every other row like this one» is
    // making a claim about those rows, and a claim that is false of one of them
    // is a wrong request rather than a row to skip. Settling the rest and
    // silently dropping the one is the failure this module refuses everywhere
    // else: an import that files what it can and says nothing about what it
    // could not. The one this catches in practice is an answer naming an own
    // account which some other row is itself on — `resolve` refuses a transfer
    // to itself, and that row is not the same decision however alike its
    // question looks.
    //
    // The subject is recomputed here rather than read off the assessment: the
    // assessment is a rendering the caller may be holding from before the last
    // row was fed, and what this call settles must be decided from the session
    // as it now stands.
    let targets: Vec<&ImportQuestionView> = match reach {
        AnswerReach::ThisRow => Vec::new(),
        AnswerReach::EveryLikeRowInThisSession => {
            let Some(subject) = subject_asked(&contents.observations, stored) else {
                return Err(FieldRejection::new(
                    "settles",
                    "a question this build can still read, for an answer that reaches beyond \
                     its own row",
                    "a stored question that could not be read",
                )
                .into());
            };
            // Over the questions this session is **waiting on**, which is the
            // set the assessment published to the caller as
            // [`OpenQuestion::alike`] (`iaam-m2oi`). Read off `is_open` alone
            // the reach was wider than the list he was shown: it wrote his
            // answer onto rows a standing rule of his had already settled and
            // that no response had named to him as open, so «and every other
            // row like this one» settled rows he had never seen. The reading is
            // taken here rather than passed in, because what this call settles
            // must be decided from the session as it now stands — the same
            // reason the subject is recomputed rather than read off the
            // assessment.
            let settlements = SessionReading::of(services, principal, &contents)
                .await?
                .settlements();
            contents
                .questions
                .iter()
                .filter(|candidate| {
                    candidate.id != question
                        && settlements.awaits_answer(candidate)
                        && subject_asked(&contents.observations, candidate)
                            .is_some_and(|of_candidate| of_candidate == subject)
                })
                .collect()
        }
    };
    for target in &targets {
        let row = observed_row(&contents.observations, target.row)?;
        row.resolve_with(answer)
            .map_err(|rejection| AppError::Invalid {
                field: format!("settles/{}", target.row),
                expected: rejection.expected,
                actual: rejection.actual,
            })?;
    }

    // **The answer is written before the rule, and this is iaam-77hk.** It used
    // to be the other way round: the rule was created, and its identifier was
    // passed to the write that recorded the answer. A failure of that second
    // write left the owner holding a standing rule for an answer no session
    // shows — a rule whose origin he never sees, whose question still reads as
    // open, and which he cannot reach from the row that made it.
    //
    // The two writes cannot be one. `services.store` and `services.rules` are
    // separate ports; neither signature carries a transaction handle and the
    // rule store need not even be the same database, so a promise of atomicity
    // here would be a promise nothing keeps. What is left is the order, and the
    // order is decided by which fact is the owner's: the answer is what he said
    // about the row, and the rule is derived from it. The derived one goes
    // second.
    //
    // What remains possible is stated rather than hidden. A failure after this
    // point leaves the row settled and no rule recorded — which is exactly
    // `Generalisation::Available`, a state the action queue offers an act for,
    // so the owner is one call from the rule rather than back at the row. A
    // caller that sees this call fail must therefore re-read the session rather
    // than repeat the call: the answer may already stand, and answering twice is
    // refused.
    //
    // The reach makes one addition to that reasoning and no change to it. The
    // rows this answer also settles are answered **before** the row it was asked
    // about, and that order is chosen for recovery rather than for precedence:
    // every one of them is the owner's own fact, so decision 0027's «his fact
    // before the derived one» is satisfied whichever comes first among them,
    // while answering a question twice is refused — so a call that failed
    // half-way must be repeatable, and it is repeatable only if the question the
    // caller addresses is the last one written. On a repeat the session is read
    // again, the rows already settled are no longer open, and they are simply
    // not among the targets.
    let payload = json(&answer, "answer")?;
    let mut also_settled = Vec::with_capacity(targets.len());
    for target in &targets {
        also_settled.push(
            services
                .store
                .answer_import_question(principal.owner, session, target.id, payload.clone())
                .await?,
        );
    }
    let answered = services
        .store
        .answer_import_question(principal.owner, session, question, payload)
        .await?;
    // One rule at most, whatever the reach, and it is minted from the row the
    // caller addressed. A rule per settled row would be one decision recorded
    // many times — and `matcher_for` builds them all from the same field of the
    // same subject, so they would be the same rule written over and over.
    // Two filters and they refuse different things. `may_generalise` is about
    // the **answerer**: an agent settles the row and the standing decision stays
    // the owner's, and the rule it would have written is published as
    // [`Generalisation::Available`] for him to adopt. `generalises` is about the
    // **answer**: «it was between accounts of mine and I cannot say which» is a
    // fact about what this document did not contain, and a rule made of it would
    // file every later row of the same shape as unplaceable — including the ones
    // whose far half is in the export and which the pairing would settle whole.
    // So there is nothing here for anybody to adopt, and the question reports
    // [`Generalisation::DoesNotGeneralise`] rather than a proposal.
    let answered = match matcher_for(&observed)
        .filter(|_| may_generalise(principal))
        .filter(|_| answer.shape().generalises())
    {
        Some(matcher) => {
            let rule = services
                .rules
                .create_rule(
                    principal.owner,
                    json(&matcher_json(&matcher), "matcher")?,
                    json(&outcome_json(answer.classification()), "outcome")?,
                    None,
                )
                .await?;
            services
                .store
                .attach_import_question_rule(
                    principal.owner,
                    session,
                    question,
                    rule.id.to_string(),
                )
                .await?
        }
        None => answered,
    };
    // Through the same pairing every other reading of a question goes through,
    // rather than an empty list and a generalisation written out here. The
    // answered question offers no candidates *because it is answered*, and what
    // its answer generalised into is one derivation; stated in two places they
    // are rules that can come to disagree with themselves.
    //
    // `contents` was read before the answer was written, and that is harmless:
    // what the generalisation consults it for is the observed row, which an
    // answer does not change.
    let mut paired = answerable_questions(
        services,
        principal,
        &contents,
        &also_settled
            .iter()
            .cloned()
            .chain(std::iter::once(answered.clone()))
            .collect::<Vec<_>>(),
    )
    .await?;
    let asked = paired.pop().unwrap_or(AnswerableQuestion {
        view: answered,
        accounts: Vec::new(),
        generalisation: Generalisation::Unanswered,
        // The question this call just answered, so nothing else settled it.
        settled_without_answer: None,
    });
    Ok(AnsweredQuestions {
        asked,
        also_settled: paired,
    })
}

/// How far one answer reaches (`iaam-q5og`, decision 0029).
///
/// **Both members settle rows and neither writes a rule.** That line is the
/// whole of this type: `may_generalise` is unchanged, a standing rule is still
/// the owner's, and the wider reach claims nothing about a statement nobody has
/// imported yet. What it settles is rows already fed to one session, which the
/// owner reads in that session's assessment before the commit writes anything
/// and can abandon whole, leaving the journal exactly as it was.
///
/// **Stated by the caller, never assumed.** Making the wider reach automatic was
/// weighed and refused: it would turn a call that settled one row into a call
/// that settles fifty without the caller choosing, and «a mistake is made many
/// times» is the third of the three complaints this bead was filed on. An
/// explicit reach makes the fan-out a decision, and the assessment publishes
/// [`OpenQuestion::alike`] so the decision is an informed one rather than a
/// guess about how many rows are alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnswerReach {
    /// Only the row the question names. The behaviour before decision 0029, and
    /// the default, so a caller that says nothing settles exactly what it
    /// addressed.
    #[default]
    ThisRow,
    /// Every question this session is still waiting on that raises the same
    /// decision.
    ///
    /// «Still waiting on» and not «unanswered» (`iaam-m2oi`): a question a
    /// standing rule of his has already settled is not reached, because it is
    /// not in the [`OpenQuestion::alike`] list the assessment showed him and a
    /// reach wider than the list he read is a reach he did not choose.
    ///
    /// «The same decision» is
    /// [`QuestionSubject`](iaam_ingest::classification::QuestionSubject), which
    /// is the question paired with the direction the source stated for the row.
    /// The pairing is what keeps a counterparty named on a row that arrived and
    /// on a row that left two decisions rather than one.
    EveryLikeRowInThisSession,
}

impl AnswerReach {
    /// The wire word for this reach.
    ///
    /// One place, so that what a group publishes as the reach that settles it
    /// and what the answering call parses cannot come to be spelt differently.
    /// Before `iaam-cixz` the two words existed only inside the transport's
    /// parser, where nothing else could name them.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ThisRow => "this_row",
            Self::EveryLikeRowInThisSession => "every_like_row_in_this_session",
        }
    }
}

/// What one call to [`answer_question`] settled.
///
/// Two lists rather than one, and the split is the caller's own act: `asked` is
/// the question it addressed, and `also_settled` is what its reach carried the
/// answer to. Folded into one list they would be indistinguishable, and the
/// caller could not tell the owner which row he decided and which rows were
/// decided with it — which is the thing the reach must never hide.
///
/// `also_settled` is empty for [`AnswerReach::ThisRow`], and it is empty for the
/// wider reach when no other open question of the session was the same decision.
/// Those are the same fact about the session and are not distinguished here: in
/// both, this answer settled one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsweredQuestions {
    pub asked: AnswerableQuestion,
    pub also_settled: Vec<AnswerableQuestion>,
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

    // One verdict per **held row**, in the order the rows were fed. The
    // candidates are only the rows that become facts, so the settled ones are
    // put back at their own positions here: a caller reads this list by
    // position, and a list that renumbered itself around a settled row would
    // report every later row's outcome against the wrong row.
    let written = submit_candidates(
        services,
        principal,
        "operation",
        Some(session),
        planned.candidates,
    )
    .await?;
    let mut written = written.into_iter();
    let mut verdicts = Vec::with_capacity(planned.dispositions.len());
    for disposition in &planned.dispositions {
        match disposition {
            Disposition::NoFact(reason) => verdicts.push(Verdict::NoFact {
                reason: reason.code().to_owned(),
            }),
            Disposition::Candidate => {
                let verdict = written.next().ok_or_else(|| {
                    AppError::Store(
                        "the commit produced fewer verdicts than it had candidates".to_owned(),
                    )
                })?;
                verdicts.push(verdict);
            }
        }
    }
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
    /// One entry per held row, in the order the rows were fed, saying whether
    /// that row is among `candidates` or produced nothing.
    ///
    /// Private, and it exists so that [`CommitOutcome::verdicts`] stays a list
    /// a caller reads **by row position**. `candidates` holds only the rows
    /// that become facts, so without this the verdict list would silently
    /// renumber every row after a settled one — the failure mode where every
    /// verdict is positive and the rows they describe are not the rows the
    /// caller sent.
    dispositions: Vec<Disposition>,
    /// The events the commit would **append**, in row order: exactly the
    /// candidates that became `commit_delta.facts`.
    ///
    /// Collected in the loop that makes that very split, so this list and the
    /// published `facts` cannot come to describe different rows. The duplicates
    /// are deliberately absent: their idempotency key is already in the
    /// journal, so appending them adds nothing, and folding one into a
    /// projection beside the journal would count that money twice.
    ///
    /// Read through [`PlannedSession::would_append`], which says what these may
    /// and may not be used for.
    appended: Vec<iaam_core::event::Event>,
}

impl PlannedSession {
    /// The events this commit would append, for a reader that folds them.
    ///
    /// Handed out so that a report asked to answer over the journal **plus**
    /// this session's held rows folds the same facts the commit would write,
    /// planned once by [`plan_session`] and never by a second pass over the
    /// stored observations. That is iaam-k1xa read from the reporting side: a
    /// preview written beside the import describes a different import from the
    /// one that runs.
    ///
    /// **What these are not.** They carry freshly minted identifiers that name
    /// nothing in the journal and differ on the next call, so nothing may
    /// publish one, address one, or persist a projection folded over one — see
    /// decision 0018 §5 for the snapshot that must not be saved. They are
    /// values to fold and then discard.
    #[must_use]
    pub fn would_append(&self) -> &[iaam_core::event::Event] {
        &self.appended
    }
}

/// Where one held row's verdict comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// The row is the next element of `candidates`.
    Candidate,
    /// The row produced nothing, and this is why.
    NoFact(NoFactReason),
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
    /// Account names a document of this session printed that the owner's
    /// directory resolves to no single account.
    ///
    /// **A fourth field and not a widening of `missing`** (decision 0024). The
    /// three above it are answers about a row this session holds: `missing`
    /// names accounts a row named **by identifier** and the directory does not
    /// hold, and it is a list of identifiers because that is what such a row
    /// carried. A printed string that matched nothing has no identifier at all —
    /// there is nothing to put in that list, and putting the string there under
    /// a union type would be two facts sharing one slot, which is exactly how
    /// `iaam-p683` happened one field over.
    ///
    /// It is also quantified differently, and that is the second reason it is
    /// its own field: these names come from records that **were refused**, so
    /// this session holds no row for any of them. Every other section here is a
    /// statement about rows the commit would write; this one is a statement
    /// about rows that were never held, and reading it as the first would say
    /// the import is about to record something it will not.
    ///
    /// In the order the documents printed them, deduplicated, and filtered
    /// through the directory as it now stands: a name the owner has since
    /// created an account for is not listed, because it is no longer true.
    pub unrecognised: Vec<String>,
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
    /// Standing decisions this session's own rows offer, before the owner is
    /// asked about them one at a time.
    ///
    /// **This is the first import problem** (`iaam-qn6d`). A first import has no
    /// rules, so every row carrying a party name becomes a question, and the
    /// answer is the same for most of them. The one field in the document that
    /// says what a row was **for** — the category the source filed it under — is
    /// transcribed by the profile, matchable by a rule since decision 0026, and
    /// read by nothing on a first import, because a standing rule comes only
    /// from answering a question and there are hundreds of those.
    ///
    /// So the session says what it has: the words its own unanswered rows were
    /// filed under, how many rows each accounts for, and the condition a rule on
    /// that word would ask. It offers **conditions and never outcomes** — see
    /// [`OfferedRule`] — which is decision 0019 §6's line held one level up: the
    /// source's word is evidence, what the rows *are* is the owner's, and
    /// nothing here concludes on his behalf.
    pub offered_rules: Vec<OfferedRule>,
    /// The words this session's own rows were filed under that no rule is
    /// offered on, and what each of them turned out to hold.
    ///
    /// **This is `iaam-xchm`, and it is a list rather than a flag on
    /// [`OfferedRule`].** A word an institution files by its own purposes can
    /// cover things the owner would decide differently — a transfer word covers
    /// every transfer, inward and outward, to his own accounts and to other
    /// people's — and one rule on such a word is a confident recommendation to
    /// make one wrong standing decision instead of many right ones. The offer
    /// exists to stop the owner answering the same thing many times; adopted on
    /// a group that is not one thing it causes the failure it exists to prevent.
    ///
    /// **Two lists and not one list with a marker**, because a marker is a field
    /// a caller can ignore, and the thing being marked is what makes the entry
    /// dangerous. A caller that walks [`Self::offered_rules`] and relays every
    /// entry cannot relay a bad one; a caller that ignored a `mixed` flag would
    /// relay exactly the offers that must not be relayed. Nothing is hidden by
    /// the split: every row named here is in [`Self::open_questions`] as well,
    /// so what such a caller loses is a shortcut and never a row.
    ///
    /// An offer withheld with a reason is a fact about the document. It is the
    /// answer to «why is there no offer on the word that covers half my
    /// statement», which silence answers wrongly by implying there is nothing
    /// there.
    pub withheld_offers: Vec<WithheldOffer>,
    /// The owner's accounts an answer that names one may name.
    ///
    /// **This is `iaam-7iyg`.** Two of the four questions are answered by naming
    /// one of his accounts, [`OpenQuestion::alternatives`] says so per word
    /// through `AnswerShape::needs_account`, and until now the assessment said
    /// only *that* an account is required and never which accounts exist. That
    /// is the state `MissingInput::candidates` was given a field to end: an item
    /// that only said an account was needed left the caller to find out
    /// elsewhere which ones are eligible.
    ///
    /// **Once per assessment and not once per question**, which is the opposite
    /// of `MissingInput::candidates`' choice and is not a disagreement with it.
    /// A queue item is read alone — it is one field of one call, and there is no
    /// second item in the response to hang a shared list on. An assessment is
    /// one response holding every open question of the session, and a first
    /// import holds hundreds; repeating the directory under each of them would
    /// publish the owner's whole account list hundreds of times to say something
    /// identical every time. Decision 0029 declined to add the list per question
    /// for that reason and pointed the caller at the per-question route instead;
    /// that costs one call per question, which is the same arithmetic in another
    /// currency.
    ///
    /// **The exclusion the per-question list makes is derivable here.**
    /// `answer_account_candidates` drops the account the row is already on,
    /// because an account is not the far side of itself. This list cannot drop
    /// it, because it is not about one row — so [`PrintedRow::account`] says
    /// which account each question is on, and the caller filters. That is one
    /// comparison against a field published beside the question, not a lookup
    /// somewhere else, and it is the reason the two beads are answered together.
    ///
    /// Empty when no open question offers an answer that names an account, and
    /// when the owner holds no accounts at all — both true statements about his
    /// directory rather than a lookup nobody made.
    pub answer_accounts: Vec<AccountCandidate>,
    /// The sets these open rows form, each with what its members have in common
    /// and the one answer that settles the whole of it.
    ///
    /// **This is `iaam-cixz`.** [`OpenQuestion::alike`] and
    /// [`OpenQuestion::pair`] publish the relation from each row to the others;
    /// nothing published the set. A caller asked what a set of rows actually was
    /// therefore listed every member or invented a summary, and the one that
    /// happened was neither — it read the owner's raw statement file. See
    /// [`RowGroup`].
    ///
    /// **Once for the assessment, not once per question**, on
    /// [`Self::answer_accounts`]' grounds: this response holds every open
    /// question of the session, a group of twenty rows hung on each of its
    /// members would be published twenty times, and the field that relates a
    /// question to its group is already on the question. A caller going the
    /// other way compares the row against [`RowGroup::rows`], which is one
    /// comparison against a list published beside it.
    ///
    /// Largest first, then by the first row, so the group worth putting to him
    /// first is first and the list does not reorder itself between two readings
    /// of one session. Empty means every open question of this session stands
    /// alone, which is a true statement about the document and not a fold nobody
    /// made.
    pub groups: Vec<RowGroup>,
}

/// One question the session is still waiting on.
///
/// **The row's own facts are published beside the sentence** (`iaam-pm4w`). The
/// sentence is what a person reads; the fields are what a caller groups, sorts
/// and totals by, and before they were here a caller that had to show the owner
/// what a group of questions contained extracted the date, the amount, the
/// direction and the party out of [`Self::prompt`] with regular expressions.
///
/// That is the act `docs/import-boundary.md` refuses one level down. A caller
/// may not interpret a document's rows — that is what a profile is for — and a
/// caller re-deriving structure from prose this engine wrote is the same act
/// with the same failure: an expression that is right today and silently wrong
/// when the wording changes. The wording is being actively rewritten, and every
/// one of those values was a typed value here while the question was built.
///
/// **This is not a second reading of the row.** The figures a session's rows are
/// read *by* — what each will move in the journal — are [`PlannedFact`]s,
/// computed by planning the commit, and `Resolver::render` refused published
/// fields partly on the ground that a second rendering of one row is a pair of
/// readings that can disagree. There is no such pair here: a row published here
/// produces no planned fact at all, so the two lists are disjoint by
/// construction. What is published here is what the **source** printed, signed
/// as it printed it, and never what the commit would record — see
/// [`PrintedRow::amount_minor`].
///
/// **The disjointness is a property of the reading and not of the questions
/// table** (`iaam-m2oi`, decision 0038). It used to be written as «a row with an
/// open question produces no planned fact», which was read off
/// `ImportQuestionView::is_open` and was false: a row whose stored question the
/// owner never answered still produces a fact once a standing rule of his
/// classifies it, and the assessment then named that row in
/// [`Interpretation::resolved`] and here at once — which is exactly what
/// decision 0032 §1 says cannot happen. What is published here is now decided by
/// [`QuestionSettlements`], which is the one fold over what the reading made of
/// each row, so the two lists are disjoint because they are two halves of one
/// determination rather than two tests that agree by habit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenQuestion {
    /// The row's position **in this session**, which is what the answering call
    /// takes.
    ///
    /// **Not the line of any file** (`iaam-f6y4`). It is the position the row
    /// occupies among what this session was handed, in submission order — see
    /// [`crate::ports::Store::add_import_observation`], which assigns it — and a
    /// record the reader refused occupies a line of the document and takes no
    /// row here, so from the first refusal the two numbers differ by however
    /// many records have been refused above. A caller that matched these
    /// numbers against the lines of the owner's file agreed for a while and then
    /// silently did not.
    ///
    /// The line is the `locator` published for every record when a document is
    /// read, on the same object as the row it became; nothing here turns a row
    /// back into one, and decision 0035 §4 says why the question does not carry
    /// it.
    pub row: u32,
    pub question: ImportQuestionId,
    pub prompt: String,
    /// The row this question is about, as the source printed it.
    ///
    /// **One absence and not six.** The values travel together because they are
    /// one row, and the one thing that can go wrong with them is the same thing:
    /// a session holding an observation this build can no longer parse. Six
    /// optional fields would let a reader think a row could state its amount and
    /// not its account, which no row can — and it is the shape `NewImportQuestion`
    /// already argues for the three halves of a question.
    ///
    /// `None` is therefore «this build cannot read the stored row», the same
    /// tolerance [`Self::alike`] already applies: such a question keeps its
    /// stored sentence and its stored alternatives, is answerable, and is
    /// comparable with nothing.
    pub printed: Option<PrintedRow>,
    /// What may be said in answer to it, each carrying what it decides.
    ///
    /// **This is `iaam-ulib`.** The question was published in four places and
    /// this one omitted the words that answer it, so an agent reading the
    /// assessment to work a session went hunting across routes — guessing at one
    /// that does not exist — for the list every other publisher already carried.
    /// A question published without its answers is not a question a reader can
    /// put to anybody.
    ///
    /// Read from what was stored when the question was asked, through
    /// [`stored_alternatives`], which is the one function every publisher reads
    /// them with. Recomputing them here from the question would publish what
    /// *this build* would offer for a question asked by an older one, and the
    /// owner would be shown a word the stored question does not admit.
    pub alternatives: Vec<AnswerShape>,
    /// The other open rows of this session raising the same decision, in row
    /// order and excluding this one.
    ///
    /// **This is the half of `iaam-q5og` that is not about answering.** The
    /// assessment published questions in row order, and grouping them was work
    /// the caller had to invent — so the owner was read a question he had
    /// already answered, which is the state decision 0016 and the whole
    /// visibility line were filed to end. Empty means this row's decision is
    /// asked once.
    ///
    /// «The same decision» is `iaam_ingest::classification::QuestionSubject`
    /// and not the question alone: a counterparty named on a row that arrived
    /// and on a row that left raises the *same* question and is two decisions,
    /// because an answer carries a direction of its own.
    pub alike: Vec<u32>,
    /// The other leg of one movement, where this row is one of the two
    /// (`iaam-3qsq`, decision 0031).
    ///
    /// **This is not [`Self::alike`] and the two must never be read as degrees
    /// of one relation.** Alike rows raise the *same decision* about different
    /// money — twenty card payments to one merchant — and answering one says
    /// nothing about the others until a caller asks for it. A pair is *one
    /// movement*: the document printed a departure on one account and the
    /// arrival on the other, and there is one fact between them. Answering
    /// either as a movement to or from the other's account settles both, and
    /// the pair records **one** transfer, from the sending side; the other row
    /// then comes back under `settled_without_fact` rather than as a fact of
    /// its own.
    ///
    /// **A pair is a hypothesis and this field is where it stays one.** Two
    /// unrelated payments of one amount on one day have the same shape, and the
    /// answer that says so is any answer that does not name the other row's
    /// account — «this was a payment to a shop» leaves both rows standing as
    /// two rows, with two questions, exactly as they are today. Nothing is
    /// recorded, suppressed or refused on the strength of this field: what it
    /// does is let one decision be put once instead of twice.
    ///
    /// `None` is the ordinary case and means this row's question stands alone.
    /// The identifier is derived from the session and the two row numbers, so
    /// two readings of an unchanged session publish the same one — a random
    /// value here would move the session's revision stamp under a session
    /// nobody touched.
    ///
    /// It carries the other row as well as the identifier ([`MirroredPair`],
    /// `iaam-6jsj`): a caller that can say «rows 4 and 9 are the two sides of
    /// one movement» can put the decision once, and one holding only a shared
    /// uuid had to scan the list to find out what to say.
    pub pair: Option<MirroredPair>,
}

/// The row one open question is about, as the source printed it.
///
/// **Every value here was a typed value while the question was being built, and
/// was published only joined into a sentence** (`iaam-pm4w`). An agent that had
/// to show the owner what a group of questions contained recovered them with
/// regular expressions over the prose, which is `docs/import-boundary.md`'s
/// refusal — a caller may not interpret a document's rows — committed against
/// this engine's own output.
///
/// **Nothing is normalised, and that is the difference from [`PlannedFact`].** A
/// planned fact says what the commit would post; every field here says what the
/// source said, including where it said nothing. The two never describe one row:
/// a row with an open question is not planned, and a planned row has no open
/// question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintedRow {
    /// The account whose statement the row is on.
    ///
    /// Not the far side, which is the thing being asked about. It is also what
    /// makes [`Interpretation::answer_accounts`] usable: the one account that
    /// list must not offer for this question is this one.
    pub account: AccountId,
    /// What the owner calls that account, where his directory holds it
    /// (`iaam-6jsj`, decision 0035).
    ///
    /// **The identifier was the whole of it, and a caller working a real import
    /// printed the identifier.** A question is published in order to be put to a
    /// person, and an account he is asked about and cannot name is an account he
    /// cannot rule on — which is `docs/api/conventions.md` §3 in the one place
    /// on this path that had not kept it. The title never replaces
    /// [`Self::account`] and could not: the identifier is what the answering
    /// call takes, and §3.2 refuses a name as input.
    ///
    /// **It is not recoverable from [`Interpretation::answer_accounts`], which
    /// is why it is a field and not a join.** That list is published only where
    /// some open question admits an answer that names an account, so a session
    /// whose questions are all about a fee publishes none of it; and it is the
    /// owner's directory rather than this session's accounts, so it says nothing
    /// about a row on an account the directory does not hold.
    ///
    /// `None` is that account and nothing else: one a row named by identifier
    /// which the directory does not hold, published by
    /// [`AccountResolution::missing`] and exactly the case this section must
    /// still be able to ask a question about. It is never the identifier
    /// rendered as a name — [`AccountNames::title`] falls back that way for a
    /// refusal an operator reads, and a fallback that prints a uuid where a
    /// title belongs is the defect this field was added for.
    pub title: Option<String>,
    /// The institution he said holds that account, when he said.
    ///
    /// Beside the title for §3.1's reason and not by symmetry: two accounts he
    /// calls `Savings`, at two banks, are one word apart in a list and are not
    /// the same question. `None` is «he has not said», never «it is held
    /// nowhere», and never a guess — an invented institution would tell two
    /// accounts apart by a fiction.
    pub institution: Option<String>,
    /// The amount **with the sign the source printed**.
    ///
    /// [`ObservedRow::amount_minor`] unchanged. Not made positive and not
    /// normalised into what the journal would post: the sign is the source's own
    /// statement about direction, it is the evidence the owner is matching
    /// against the line in front of him, and for the one question where the
    /// direction is genuinely open it is evidence of nothing — which is why that
    /// question exists and why the value is passed through rather than read.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    /// The day the row states, when it states one.
    ///
    /// `None` is a row the commit will refuse for want of a date, and it is
    /// published as an absence rather than filled in: a date invented here would
    /// be the first invented value in a section whose whole point is that
    /// nothing in it is invented.
    pub date: Option<time::Date>,
    /// Which way the source said the money went, when it said.
    ///
    /// [`ObservedRow::movement`], which reads the source's own direction word
    /// and never the sign on the amount. `None` means the source stated no
    /// direction — a statement about the row, and the condition
    /// `Question::UnresolvedDirection` is asked under — and never «unknown to
    /// us».
    pub movement: Option<Movement>,
    /// The party the source named, exactly as it printed it, when it named one.
    ///
    /// The string a rule would match on, so a caller that groups by it groups by
    /// what a standing decision would later fire on. `None` for the two
    /// questions asked precisely because no party was named.
    ///
    /// It is already in the question's sentence wherever there is one, so
    /// publishing it as a field discloses nothing the prose did not — which is
    /// the test this field had to pass and the description does not: the
    /// description is the row's whole text, of unbounded length and written by
    /// the source, and it stays out of both (see [`row_mark`]).
    pub counterparty: Option<String>,
    /// The word the source filed the row under, verbatim, when it printed one.
    ///
    /// The field [`OfferedRule`] groups by, published on the question so that a
    /// caller reading a withheld offer can see which word each open row belongs
    /// to without joining two lists by row number.
    pub source_category: Option<String>,
    /// The word the **owner himself** filed the row under, at the source,
    /// verbatim, when the source printed one.
    ///
    /// Beside [`Self::source_category`] for its reason and never instead of it:
    /// both ground a group, so a caller reading an offer or a withheld entry
    /// keyed on his own word needs the same join. It is a decision he already
    /// took, so publishing it discloses nothing he did not himself write down —
    /// and it is the one field here he will recognise at a glance.
    pub owner_category: Option<String>,
}

/// One standing decision the session's own rows offer, stated as a condition.
///
/// **The condition, and never the outcome.** What the rows have in common is a
/// fact about the document: this many of them were filed by the source under
/// this word. What they *are* — a purchase, a fee, money of his coming back — is
/// the owner's, and a profile mapping a source's category to one of his
/// classifications is exactly what decision 0019 §6 refuses, on the ground that
/// a map baked into a profile is frozen into every fact at import while a rule
/// of his is editable and re-runnable over rows already recorded. An offer that
/// filled in the outcome would be that map, written at the session instead of in
/// the profile, and no better for being written later.
///
/// **One field per condition, which is decision 0008's number**, and here it is
/// the category rather than the counterparty [`matcher_for`] would pick. That is
/// the point of the offer: a counterparty condition is one decision per shop and
/// there are hundreds of shops, while the categories one statement prints are a
/// handful and they cover the same rows. The two are not rivals — a counterparty
/// rule the owner adopts still wins nothing over this one, because
/// [`classify`](iaam_ingest::classification::classify) takes the highest version
/// among matching rules and both are his.
///
/// **Only rows with an open question are counted.** A row a rule of his already
/// settles is not evidence that he wants another rule, and counting it would
/// make an offer grow every month while settling nothing new.
///
/// **Offered only where the group is one thing** (`iaam-xchm`). [`Self::contains`]
/// is one [`RowShape`] and cannot hold two, so an offer whose rows disagree
/// about what the source said they were is not representable: it becomes a
/// [`WithheldOffer`] instead. That is the invariant the type states and no
/// caller has to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedRule {
    /// The condition, in the shape `POST /v1/classification-rules` takes.
    pub matcher: RuleMatcher,
    /// What is asked of the owner, and what his answer changes.
    ///
    /// [`OwnerQuestion`] and not a second string of this module's own, because
    /// decision 0027 settled the shape of a question put to a person: two
    /// values, so that the half saying what turns on the answer cannot be folded
    /// away into a sentence that already reads as finished. This is the first
    /// owner-facing text written since that decision, so it is written in its
    /// type rather than in the older one-string shape of [`OpenQuestion::prompt`]
    /// beside it.
    pub question: OwnerQuestion,
    /// The rows of this session, in order, whose open question this condition
    /// would settle.
    pub covers: Vec<u32>,
    /// What those rows are, as far as the source said — one shape, always.
    ///
    /// Published so that a caller can show the owner what he is deciding about
    /// without expanding the group by hand, and so that the claim this offer
    /// makes is checkable by its reader rather than asserted. `contains.rows`
    /// is [`Self::covers`]; it is repeated inside the shape because a
    /// [`WithheldOffer`] holds several shapes and each of them must name its
    /// own rows, and one type for both is what keeps the two lists comparable.
    pub contains: RowShape,
}

/// Whose filing a group of rows is keyed by.
///
/// **Two vocabularies and never one**, which is the whole reason this exists.
/// An institution files by its own purposes; the owner files by his, in the
/// institution's app, and the export prints both back. A group keyed by one of
/// them is a group keyed by a decision one party made, and which party it was
/// decides what may truthfully be said about the group and which field of a
/// [`RuleMatcher`] a condition on it goes in.
///
/// Ordered so that a listing does not reorder itself between two readings, and
/// the institution's word comes first for no better reason than that it is the
/// one every profile has always transcribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FiledBy {
    /// The institution's own word for what the movement was for.
    Source,
    /// The owner's own word, taken in the institution's app and printed back.
    ///
    /// **His decision, already made.** It is the strongest evidence a statement
    /// carries about what a row was, and it is still only evidence: it is his
    /// decision in his *bank's* vocabulary, and what it is called here is the
    /// question the offer puts — once for the word, never once per row.
    Owner,
}

impl FiledBy {
    /// The word for the wire, one place so the transport cannot spell it
    /// differently. It is the field of a condition on this ground, which is
    /// what makes an offer and a withheld entry joinable.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Source => "source_category",
            Self::Owner => "owner_category",
        }
    }
}

/// A word rows were filed under whose rows are not one thing, and the
/// rule that is therefore not offered on it.
///
/// **This is `iaam-xchm`.** On a real export one word the institution files by
/// covered a large share of the document and held at least four incompatible
/// things — movements between the owner's own accounts, payments to a person,
/// payments to a company, and others. One rule on that word would have been
/// wrong for most of what it matched, and the only way to find that out was to
/// expand the group by hand.
///
/// **The word is not the defect.** An institution files by its own purposes, and
/// a transfer word covering every transfer — inward and outward, internal and
/// not — is that vocabulary working correctly. What was missing is that the
/// offer said nothing about whether the group was one thing.
///
/// # Why the group is not narrowed instead
///
/// The tempting repair is to key the group by the word **and** the direction, as
/// `QuestionSubject` keys a decision, and offer a rule per key. It cannot be
/// done, and the reason is older and stronger than this bead:
/// [`Classification`] carries no direction on purpose — «a rule fires on rows
/// the owner has never seen; a direction carried over from the row he wrote it
/// on would be asserted about all of them» — and [`RuleMatcher`] therefore has
/// no field that could express one. A group narrowed by direction would publish
/// a `covers` list of the outgoing rows beside a matcher that matches the
/// incoming ones too, so the offer would claim something the rule does not do
/// and `preview_category_rule_route`'s answer to «what would this match» would
/// contradict it. The condition and the group have to be the same question,
/// which means the group is the word.
///
/// So the honest move is the other one: keep the group the word, say what it
/// holds, and offer no rule where what it holds is not one thing.
///
/// # What «one thing» can and cannot see
///
/// [`RowShape`] is made of what the **source** stated, which is all this side
/// has. It separates the case that actually occurred — one word covering rows
/// that ran both ways and rows that named nobody — and it cannot separate a
/// payment to a person from a payment to a company, because no field of the row
/// says which. A single shape is therefore a statement that the source
/// contradicted itself nowhere, not a guarantee that the owner would answer
/// alike; it is the strongest claim the document supports, and the offer's
/// wording is the owner's protection for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithheldOffer {
    /// Whose filing the word is — the institution's or the owner's.
    ///
    /// **Published because the sentence beside it would otherwise be false in
    /// one of the two cases.** An offer withheld on a word of the owner's own is
    /// withheld on a decision he took, and telling him «your statement files
    /// these under …» would hand his own filing back to him as the bank's.
    /// [`OfferedRule`] needs no such field: its condition names the field it
    /// asks about, so its ground is already readable off the matcher.
    pub filed_by: FiledBy,
    /// The word these rows were filed under, verbatim, by whoever
    /// [`Self::filed_by`] says filed them.
    pub filed_under: String,
    /// The rows of this session, in order, that no offer covers because of it.
    /// The union of [`Self::contains`]'s rows.
    pub covers: Vec<u32>,
    /// What the word turned out to hold, largest share first — two shapes at
    /// least, or this would be an [`OfferedRule`].
    ///
    /// This is the part a caller shows. Working the group by shape is what is
    /// left when no standing decision can be offered on it, and it is not
    /// nothing: `OpenQuestion::alike` and the answering call's reach already
    /// settle many rows from one answer without any rule being written.
    pub contains: Vec<RowShape>,
    /// Why no rule is offered, in one sentence.
    ///
    /// **A statement, and deliberately not an [`OwnerQuestion`].** Decision 0027
    /// governs what is *put* to a person, and its two halves are what is being
    /// asked and what his answer changes. Nothing is being asked here and no
    /// answer of his changes anything, so a `consequence` would have to be
    /// invented, and an invented one is the sentence that reads as finished
    /// which 0027 exists to prevent. It is written in his register regardless —
    /// no field name and no word that exists only because of how this is built —
    /// because a caller that shows it will show it to him.
    pub reason: String,
}

/// What a group of rows is, as far as the source said.
///
/// **Two facts, and they are the two that decide the question a row raises.**
/// `question_for` builds its four variants out of exactly this pair — a named
/// party with a stated direction asks whether the far side is the owner's, an
/// unnamed outflow asks whether it was a fee, an unnamed inflow asks what
/// arrived, and no direction at all asks which way it ran — so two rows with the
/// same shape raised the same question and two with different shapes did not. A
/// third field naming the question would be that same fact written twice, in a
/// place where the two spellings could drift.
///
/// Read from the row and not from the stored question, because the group is a
/// statement about the document: a question this build can no longer parse still
/// has a row that says which way the money went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowShape {
    /// Which way the source said the money went, `None` where it said nothing.
    /// Not a wildcard: rows the source stated no direction for are their own
    /// shape, because that is what they have in common.
    pub movement: Option<Movement>,
    /// Whether the source named a party at all. Not the party's name: two shops
    /// are the same shape, and grouping by the name would be one group per shop,
    /// which is the count the whole offer exists to reduce.
    pub counterparty_named: bool,
    /// The rows of this shape, in order.
    pub rows: Vec<u32>,
}

/// A set of this session's open rows put to a person as **one** thing.
///
/// **This is `iaam-cixz`, and it is the wall wave Y left standing.** Every
/// grouping this module publishes names its members and none of them publishes
/// the group: [`OpenQuestion::alike`] names the other rows raising one decision,
/// [`OfferedRule::covers`] names the rows one word covers, [`OpenQuestion::pair`]
/// names the other leg. So a caller asked what a set of rows actually **was**
/// had two moves and both are failures. It could read every member out to the
/// owner — the wall those fields were added to end — or it could invent a
/// summary of them, which is interpreting a document with this engine's own
/// output as the document (`docs/import-boundary.md`). The owner watched a
/// caller do neither and go to his raw statement file instead, which is this
/// project's own boundary crossed for want of a view.
///
/// **His statement of what he wanted is the measure.** He does not need every
/// record: show one of the group and ask what it was, because most of them share
/// every attribute except the day, the time and the amount. So a group publishes
/// what its members agree on, how many there are, how far the ones that differ
/// run — and, because a group nobody can answer as one is the same wall in
/// better clothes, the one sentence to put to him and the reach one answer must
/// state to settle the whole of it.
///
/// **The count is [`Self::rows`]'s length and is not a field of its own.** A
/// count beside the list of the things counted is one fact in two places, which
/// this module refuses everywhere else, and the list is what a caller needs
/// anyway: it is how it finds the members among
/// [`Interpretation::open_questions`].
///
/// **No representative row, and it was the obvious carrier.** A real member is
/// recognisable — the owner is matching it against a line in front of him — but
/// it is also a particular: its day and its amount are true of it and of nothing
/// else in the group, and a caller that shows it *as* the group shows him one
/// line and takes an answer about twenty. Two things make the field unnecessary
/// as well as unsafe. Every member is published in full beside this, keyed by
/// row, so a caller that wants a line takes one out of [`Self::rows`]; and
/// [`Self::question`] is written here out of the shared attributes, so the
/// sentence put to him describes the group instead of standing in for it. A
/// `representative` field would be `rows[0]` under a second name with a licence
/// attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowGroup {
    /// What makes these rows one group.
    pub basis: GroupBasis,
    /// The members, in row order. Never fewer than two.
    ///
    /// **Never a set of one**, which decision 0033 §2 settled one surface over
    /// and which holds here for its reason: one row is one question already, a
    /// group of one would make a caller take a group apart to find what it had,
    /// and it would put a sentence about several lines to the owner about one.
    pub rows: Vec<u32>,
    /// What every member states alike.
    pub common: SharedRow,
    /// The days the members run between, over the members that state one.
    ///
    /// `None` where no member states a day. A member that states none is still
    /// in [`Self::rows`] and outside this span: a date invented for it would be
    /// the first invented value in a section whose whole point is that nothing
    /// in it is invented, and that row's own [`PrintedRow::date`] says so.
    pub days: Option<DaySpan>,
    /// The smallest and largest amount among the members, with the signs the
    /// source printed.
    ///
    /// Published only where [`SharedRow::currency`] is, and that is an invariant
    /// rather than an accident: a range taken over two currencies is a pair of
    /// numbers with no unit, and «from twelve to four hundred» read across two of
    /// them tells a person something false.
    pub amounts: Option<AmountSpan>,
    /// The reach one answer must state to settle the whole group.
    ///
    /// **The half of this bead that is not about publishing.** `iaam-q5og` gave
    /// the answering call a stated reach and made a wider answer refuse whole;
    /// what was missing is that nothing said which reach settles which group, so
    /// a caller reading a group still had to work out whether one call could
    /// answer it.
    ///
    /// [`AnswerReach::EveryLikeRowInThisSession`] for
    /// [`GroupBasis::OneDecision`], whose members are exactly one
    /// `QuestionSubject` — which is what that reach is defined over, so the
    /// group and the reach cannot disagree about who is in it.
    /// [`AnswerReach::ThisRow`] for [`GroupBasis::OneMovement`], and that is not
    /// the weaker answer: the two legs are one movement, so an answer naming the
    /// other row's account settles both from either side (decision 0031), and a
    /// wider reach would be claiming something about rows that are not this
    /// movement.
    pub settles: AnswerReach,
    /// The one sentence to put to a person about the whole group, and what
    /// answering it once decides.
    ///
    /// **Written here and not by the caller**, which is decision 0027's finding:
    /// a surface that publishes typed fields and leaves the sentence to whoever
    /// relays them gets a sentence composed out of field names. The members'
    /// own sentences are on the members, one per row, and not one of them is
    /// about the group — which is exactly what sent a caller to a file.
    ///
    /// It names what the source says about all of them and asks the decision
    /// their questions raise. It does **not** quote the description, even where
    /// [`SharedRow::description`] publishes one: [`row_mark`] argues at length
    /// that a source's whole text has no place in a sentence a person is read,
    /// and that argument is about sentences and is untouched by this.
    pub question: OwnerQuestion,
}

/// What makes a set of this session's open rows one group.
///
/// **Two members and one shape, which is the answer to «which groupings get
/// this».** Three groupings exist — the decision [`OpenQuestion::alike`] names,
/// the movement [`OpenQuestion::pair`] names, and the word [`OfferedRule`]
/// groups by — and three shapes for the three would be exactly the drift this
/// module refuses everywhere else. Two of them are here under one shape, and the
/// third is deliberately not, for a reason and not for want of effort.
///
/// **The word the source filed rows under is not a group of this kind, because
/// nothing answers it as one.** It is a grouping for a *condition*: decision
/// 0032 fixed the group as the word precisely because the condition and the
/// group have to be the same question, and a word covering a single [`RowShape`]
/// still covers a hundred parties — so its rows raise a hundred decisions and
/// there is no one answer to put to him about them. The only call that acts on
/// the word whole is the rule route, which decides rows nobody has looked at and
/// leaves every question of this session where it was. Publishing the word in
/// this shape would be publishing a group with no answer, which is the thing
/// this bead exists to stop. What it gets instead is what it already had —
/// [`OfferedRule`] and [`WithheldOffer`] — and the join costs one comparison: a
/// group whose members agree on the word publishes it as
/// [`SharedRow::source_category`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBasis {
    /// Every member raises the same decision — `QuestionSubject` equality, which
    /// is the relation [`OpenQuestion::alike`] publishes per question. Twenty
    /// card payments to one party are one of these.
    OneDecision,
    /// The two legs of one movement the document printed twice, sharing
    /// [`OpenQuestion::pair`]. Exactly two members, and a hypothesis until an
    /// answer names the other side (decision 0031).
    OneMovement,
}

impl GroupBasis {
    /// Wire code. One place, so two publishers cannot spell it differently.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::OneDecision => "one_decision",
            Self::OneMovement => "one_movement",
        }
    }
}

/// What every member of a group states alike.
///
/// **Read off the members and never derived from what made them a group.** A
/// decision group agrees about its account, the party the source named and the
/// direction the source stated because `QuestionSubject` equality says so;
/// whether it also agrees about the currency, the word it was filed under or its
/// description is a fact about the document that has to be looked at. One fold
/// answers for both, and that is what lets one shape carry a grouping whose
/// members agree about nearly everything and one whose members agree about
/// nearly nothing.
///
/// An absence here is «they do not all state the same thing». It is never «this
/// system could not tell»: every field is a value the source printed, and every
/// member publishes its own beside this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedRow {
    /// The account every member's row is on, with the title the owner reads
    /// beside the identifier a call takes.
    ///
    /// [`AccountCandidate`] and not a bare identifier, and not a second shape of
    /// this module's own: an account published for a person to read is one shape
    /// in this API (conventions §3.3), and a group that named an account by
    /// identifier alone would be a group he cannot recognise.
    ///
    /// `None` where the members are on different accounts — which is what a
    /// [`GroupBasis::OneMovement`] group is — and where this instance's
    /// directory no longer holds the account they share, because a group named
    /// by an identifier nobody can read is not a group anybody can be asked
    /// about.
    pub account: Option<AccountCandidate>,
    /// The currency every member is in. `None` where they are not all in one,
    /// which is also what makes [`RowGroup::amounts`] unpublishable.
    pub currency: Option<CurrencyCode>,
    /// What every member says about which way the money went.
    pub movement: Option<SharedMovement>,
    /// The party every member named, exactly as the source printed it.
    ///
    /// `None` where they did not all name one — **including** where none of them
    /// named anybody, which is a real thing to have in common and is not spelt
    /// as a third state here. A direction is a closed vocabulary of two words, so
    /// [`SharedMovement`] can afford a third; a party is an open string, and any
    /// sentinel put in this field would be a name somebody could have. What the
    /// absence of a party means for the group is carried where it is decidable:
    /// [`RowGroup::question`] is written from the fold that knows, and the
    /// members' own [`PrintedRow::counterparty`] says it row by row.
    pub counterparty: Option<String>,
    /// The word the source filed every member under, verbatim.
    ///
    /// The join to [`OfferedRule`] and [`WithheldOffer`], which group by this
    /// word and are not published in this shape — see [`GroupBasis`].
    pub source_category: Option<String>,
    /// The description every member carries, verbatim, where the source printed
    /// one and printed the same one on all of them.
    ///
    /// **This is decision 0032's exclusion revisited here rather than
    /// reversed.** That decision kept the description off [`PrintedRow`] on
    /// [`row_mark`]'s grounds — the row's whole text, of unbounded length,
    /// written by the source — and added a second ground of its own: every other
    /// field it published was already inside some question's sentence, so those
    /// fields disclosed nothing the prose did not. Both grounds are about **a
    /// field beside every one of hundreds of questions**, both still hold there,
    /// and nothing here puts one on a row.
    ///
    /// A description shared by every member of a group is a different object. It
    /// is one string for a set rather than one per row; it is published only
    /// where the source itself said the same thing about every member, so it is a
    /// property of the group and not the text of any line in it; and a group is
    /// never a set of one, so there is no group whose description is one row's
    /// text under another name.
    ///
    /// **And the exclusion cost more disclosure than the inclusion does.** Asked
    /// what a set of rows actually was, a caller holding no field that says could
    /// not answer out of this API and read the owner's raw statement instead —
    /// every description of every row, unbounded, read outside the system
    /// altogether. This is the one field that answers «what were these», and
    /// withholding it is what produced the larger reading.
    ///
    /// `None` where the source printed none and where the members' descriptions
    /// differ in any character. It is a field and never a clause of
    /// [`RowGroup::question`]: `row_mark`'s rule is about sentences a person is
    /// read, and it stands.
    pub description: Option<String>,
}

/// What every member of a group says about which way the money went.
///
/// **Three states and not `Option<Movement>`**, because a group can agree that
/// the source stated no direction and a group can fail to agree at all, and
/// those are different facts. `Option<Movement>` already means «the source stated
/// none» everywhere on this path — [`PrintedRow::movement`] says so and
/// [`RowShape::movement`] says it is not a wildcard — so spelling «they disagree»
/// with the same `None` would collide the two on the one field that decides which
/// sentence is put to him. The vocabulary is closed at two words, so a third
/// costs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedMovement {
    /// Every member states this direction.
    Stated(Movement),
    /// Every member states none — the condition `Question::UnresolvedDirection`
    /// is asked under, and the whole identity of the group that raises it.
    NoneStated,
}

/// The days a group's members run between.
///
/// **Two endpoints and not a list of days, and not the earliest alone.** The
/// owner is placing a group on a statement he is looking at, and «between these
/// two» tells him which page to open. `earliest == latest` is a group that
/// happened on one day, and says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaySpan {
    /// The earliest day any member states.
    pub earliest: time::Date,
    /// The latest day any member states.
    pub latest: time::Date,
}

/// The amounts a group's members run between, with the signs the source printed.
///
/// **Not made positive and not totalled.** The sign is the source's own statement
/// about direction, exactly as on [`PrintedRow::amount_minor`], and a total would
/// be a figure this section computed out of rows the commit has not planned —
/// which is the second reading [`OpenQuestion`] argues at length it is not
/// making.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmountSpan {
    /// The smallest signed amount among the members.
    ///
    /// For a group of rows that left an account this is the **largest** sum that
    /// left, because the source printed those negative. That is what «with the
    /// signs the source printed» costs, and it is the cost of the alternative
    /// that matters: a span made of absolute values would agree with no line on
    /// his statement.
    pub smallest_minor: i64,
    /// The largest signed amount among the members.
    pub largest_minor: i64,
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
    ///
    /// What the fact **is**, and never why it may be written: those are two
    /// questions and [`Self::settled_by`] beside it answers the second.
    pub records_as: &'static str,
    /// On whose word this row was settled (`iaam-rdya`).
    ///
    /// Published beside `records_as` because the two together are the only way
    /// to catch a reading that settled too much. A row a profile asserted a far
    /// side for and a row one of the owner's rules matched used to reach a
    /// reader as one word — the event kind — and the difference between them is
    /// exactly the difference between a question that was answered and a
    /// question that was never asked.
    pub settled_by: FactBasis,
    /// The cash this row moves on its own account, signed as the journal will
    /// record it. Zero where the fact moves no cash on that account.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    pub date: Option<time::Date>,
    pub idempotency_key: Option<String>,
}

/// A row the journal can prove nothing about, and which looks like a fact it
/// already holds.
///
/// **Not a duplicate, and never folded into one.** [`CommitDelta::duplicates`]
/// is the hard finding: the journal holds this row's key, the commit answers
/// `duplicate`, nothing is appended. This is the soft one — §10.6's level five,
/// [`DedupLevel::Probabilistic`], «looks like a duplicate, but there is no
/// evidence» — and the commit **will** append it. The two oblige a reader
/// differently, so they are two lists.
///
/// # Why this and not a key derived for the row
///
/// A row fed to a session that names no idempotency key and no source operation
/// identifier could never be recognised as a duplicate at all (iaam-1k9t), and
/// three ways of giving it a key were weighed:
///
/// - **A key over the row's contents.** Two genuine payments of one amount on
///   one day to one place are an ordinary thing, and §10.6 forbids merging
///   them. Such a key would merge them, silently, in the direction that loses a
///   movement that really happened — a wrong answer nobody can see, which is
///   worse than the duplicate it prevents.
/// - **A key over where the row sits in this session.** The session identifier
///   is different in every session, so two sessions holding one statement would
///   derive two different keys and the check would answer «fresh» to exactly
///   the case that motivated it. It protects against re-feeding one session,
///   which the store's own `row_key` already does, and against nothing else.
/// - **A key over the document and the locator**, as `csv_source::parse`
///   stamps. That one is sound and is already had: a row stating a locator
///   carries it as `source_operation_id`, which is level **one** and is now
///   compared. What is left over is the row that states no locator either, and
///   for it the document is not a digest of anything — it is a name the caller
///   typed, and two months of statements saved under one name would collide at
///   every row.
///
/// What is left is to say what was seen and let the owner decide, which is what
/// [`DedupDecision::PossibleDuplicate`] has meant since §10.6 was written and
/// what no caller had ever been shown.
///
/// [`DedupLevel::Probabilistic`]: iaam_ingest::dedup::DedupLevel::Probabilistic
/// [`DedupDecision::PossibleDuplicate`]: iaam_ingest::dedup::DedupDecision::PossibleDuplicate
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResemblingRow {
    /// The fact this row would append — and does append: nothing here stops it.
    pub fact: PlannedFact,
    /// The recorded event it looks like: the earliest, where several share the
    /// shape, so that the plan does not depend on the journal's insertion
    /// order.
    pub resembles: EventId,
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
    /// Rows the owner's journal already holds under a key of theirs. They
    /// commit to a `duplicate` verdict and add nothing — which is correct, and
    /// is also exactly what «every verdict was positive and half the rows are
    /// not in the report» looked like from the outside.
    ///
    /// **Both key levels the store consults, in the store's order**
    /// (iaam-1k9t). `find_duplicate` matches the source's own operation
    /// identifier first, scoped by the source, and the idempotency key second,
    /// scoped by the owner. This list was computed from the second alone, so a
    /// row carrying the source's identifier and no key was published here as a
    /// **fact** and then answered `duplicate` by the very commit this plan is
    /// the description of. See `RecordedIdentities`.
    pub duplicates: Vec<PlannedFact>,
    /// Rows the journal holds no key for, and whose shape it already holds.
    ///
    /// The list `duplicates` cannot contain and must never grow to absorb:
    /// those rows append nothing, and these ones will. They are counted in
    /// `facts` and in `fact_totals` for that reason — the totals say what the
    /// journal gains, and it gains these.
    ///
    /// Separate for the reason `facts`, `duplicates`, `retained_unrecorded` and
    /// `settled_without_fact` are separate: a reader acts differently on each.
    /// Separate above all because folding a guess into a finding makes the
    /// finding a guess, and `duplicates` is the one list here whose meaning is
    /// arithmetic the owner need not check.
    ///
    /// Empty is the ordinary case, and a non-empty one is not an accusation.
    /// See [`ResemblingRow`].
    pub resembles_recorded: Vec<ResemblingRow>,
    /// Rows the session keeps and the journal will not receive.
    pub retained_unrecorded: Vec<RetainedRow>,
    /// Rows the commit read and deliberately recorded nothing for.
    ///
    /// Published beside the facts because a plan that showed a row neither as a
    /// fact nor as retained would show a row that vanished. Empty is the
    /// ordinary case.
    pub settled_without_fact: Vec<SettledRow>,
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

/// A row the commit read, understood, and deliberately recorded nothing for.
///
/// Beside [`RetainedRow`] and deliberately not one of its reasons: a retained
/// row is one the commit owes the journal and could not give it, which is why a
/// coverage gap names the interval it fell in. This row owes nothing. Told
/// apart at the type and not by a variant, so a reader folding the retained
/// rows into a gap cannot pick this one up by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledRow {
    pub row: u32,
    pub reason: NoFactReason,
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
    ///
    /// The third counter is the same argument about a heavier consequence. A
    /// row whose shape the journal already holds under no key of its own
    /// (`commit_delta.resembles_recorded`) commits by default too, and what it
    /// writes may be one movement recorded twice. It does not refuse, for the
    /// reason it is only a count and not a verdict: two genuine payments of one
    /// amount on one day have this exact shape, and a refusal that fired on
    /// them would refuse honest imports and teach its reader to wave the flag
    /// through. What the word does is make «I committed without looking» a
    /// thing the owner cannot say afterwards.
    RequiresOwnerDecision {
        unanswered_questions: usize,
        transfer_candidates: usize,
        /// Rows in `commit_delta.resembles_recorded`.
        rows_resembling_recorded: usize,
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

/// One reading of a whole session: the directory and the standing rules it was
/// read against, what each of its rows became, and which of its rows are two
/// sights of one movement.
///
/// **Extracted from [`plan_session`] because it is what «is this question still
/// open» is decided from** (`iaam-m2oi`). The refusals, the answering call and
/// the published question all have to give the same answer as the assessment,
/// and the only way for one answer to serve them is for there to be one
/// function that reads the session. A second, cheaper reading written for the
/// refusal path would be a second answer to what a row settles as, and the two
/// would differ exactly where it matters — over a rule the owner wrote between
/// the two readings.
///
/// It stops short of the journal on purpose. Deduplication, the control
/// figures, the transfer candidates and the coverage gaps all need
/// `load_events_through`, which is the expensive read on this path; what a row
/// settles as needs none of it. So a caller that only wants to know what is
/// still waiting on the owner pays for his accounts, his statements and his
/// rules, and not for his whole journal.
struct SessionReading {
    resolver: Resolver,
    rows: Vec<ReadRow>,
    mirrors: MirroredRows,
}

impl SessionReading {
    /// Read every row of the session once, against the owner's directory and
    /// rules as they stand now.
    async fn of(
        services: &AppServices,
        principal: &Principal,
        contents: &SessionContents,
    ) -> Result<Self, AppError> {
        let resolver = Resolver::load(services, principal.owner).await?;
        let mut read_rows = Vec::with_capacity(contents.observations.len());
        for observation in &contents.observations {
            let intake = parse_intake(&observation.payload).ok();
            let resolution = resolution_of(observation, &resolver);
            // A settled row that produces nothing leaves the fact pipeline here and
            // is carried by `settled` instead. It is not folded into `operation`
            // with a rejection, because it is not one: everything downstream of a
            // rejection — the coverage gap, the retention reason, the refusal to
            // commit — is about a row something is owed for.
            let settled = match &resolution {
                Ok(RowResolution::NoFact(reason)) => Some(*reason),
                Ok(RowResolution::Fact { .. }) | Err(_) => None,
            };
            let basis = match &resolution {
                Ok(RowResolution::Fact { basis, .. }) => Some(basis.clone()),
                Ok(RowResolution::NoFact(_)) | Err(_) => None,
            };
            let operation = match resolution {
                Ok(RowResolution::Fact { operation, .. }) => Ok(*operation),
                Ok(RowResolution::NoFact(_)) => Err(Rejection {
                    field: "row".to_owned(),
                    expected: "a row that becomes a fact".to_owned(),
                    actual: "a row settled without one".to_owned(),
                }),
                Err(rejection) => Err(rejection),
            };
            // The origin is derived from the session and the account this row names,
            // so that this function called twice — which is exactly what committing
            // does — plans the same provenance both times. See [`session_origin`].
            // The operation itself is kept beside the candidate because a row this
            // commit declines is named in the coverage gap by what it would have
            // moved (iaam-bufs), and a candidate that failed to normalise no longer
            // says.
            // What read this row, as its provenance will record it. Read off the
            // intake rather than assumed, exactly as on the intake path above: a
            // row the source-profile engine produced records
            // `profile/<id>/<version>`, so the rows one profile version wrote are a
            // query rather than an archaeology (decision 0019 §5). A row whose
            // payload this build cannot parse at all has no reader to name and
            // falls to the submitted version, which is what it would have carried.
            let reader = intake.as_ref().map_or_else(
                || ParserVersion(SUBMITTED_PARSER_VERSION.to_owned()),
                reader_of,
            );
            let candidate = operation.clone().and_then(|operation| {
                let origin = session_origin(principal.owner, &contents.session, operation.account);
                normalize(
                    &operation,
                    &NormalizationContext {
                        owner: principal.owner,
                        source: origin.source,
                        parser_version: reader.clone(),
                    },
                )
                .map(|normalized| {
                    let mut event = normalized.event;
                    if let Some(import) = origin.import {
                        event.provenance = event.provenance.with_import(import);
                    }
                    // What filed the row, recorded on the fact itself. The
                    // reading already established it — it is what the plan
                    // publishes as `settled_by` — and until now it stopped at
                    // the plan, so the journal could be asked which import wrote
                    // a row and never which decision of his did.
                    if let Some(basis) = &basis {
                        event.provenance = event
                            .provenance
                            .with_rule_settlement(basis.rule_settlement());
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
                candidate: if settled.is_some() {
                    None
                } else {
                    Some(candidate)
                },
                settled,
                basis,
            });
        }

        // One movement this document printed on both of its accounts is one fact
        // (decision 0031). Run **here**, over the whole session and after every row
        // has been read: the pair is invisible to a reading of either row on its
        // own, which is why the two legs used to reach the journal as two complete
        // transfers, each carrying a leg on each account.
        let mirrors = mirrored_rows(contents.session.id, &read_rows, &contents.questions);
        settle_mirrored(&mut read_rows, &mirrors);
        Ok(Self {
            resolver,
            rows: read_rows,
            mirrors,
        })
    }

    /// What this reading settled, in the shape every reader of a question's
    /// openness asks.
    fn settlements(&self) -> QuestionSettlements {
        QuestionSettlements::of(&self.rows, &self.mirrors)
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
///
/// # What it does say, and why that is not the same thing
///
/// `commit_delta.resembles_recorded` looks like the mark refused above and is
/// its opposite in the one way that decides it: **it is a comparison.** The
/// magnitude mark compared a row against a distribution nobody asserted. This
/// compares a candidate against a fact the journal holds, by the canonical
/// fingerprint `normalize` already stamps, and states the event it matched. It
/// says what it compared, it assumes nothing, and the reader can go and look at
/// both.
///
/// It is still a guess about what the owner should do, which is why it lives in
/// a list of its own and never inside `duplicates`, why it refuses nothing, and
/// why the level it belongs to — §10.6's fifth, the probabilistic one — is
/// published beside every row of it.
pub async fn plan_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
) -> Result<PlannedSession, AppError> {
    let contents = read_session(services, principal, session).await?;
    let contours = services.store.list_contours(principal.owner).await?;
    let exclusions = services
        .store
        .list_account_scope_exclusions(principal.owner)
        .await?;
    // What the journal already holds, in the terms the write path recognises:
    // so the plan can say which rows will commit to `duplicate` rather than to
    // a fact, and which carry no identity it can decide on at all. Read here
    // rather than at commit for the reason the whole split exists: what commit
    // will do is what the owner is entitled to read before it does it.
    let recorded = RecordedIdentities::of(
        &services
            .store
            .load_events_through(principal.owner, time::Date::MAX)
            .await?,
    );

    let reading = SessionReading::of(services, principal, &contents).await?;
    // What this reading settled without a word from the owner, folded once and
    // read by everything below that asks whether a question is still his to
    // answer. See [`QuestionSettlements`].
    let settlements = reading.settlements();
    let SessionReading {
        resolver,
        rows: read_rows,
        ..
    } = reading;

    let mut candidates = Vec::with_capacity(read_rows.len());
    let mut dispositions = Vec::with_capacity(read_rows.len());
    for read in &read_rows {
        match (read.settled, &read.candidate) {
            (Some(reason), _) => dispositions.push(Disposition::NoFact(reason)),
            (None, Some(candidate)) => {
                dispositions.push(Disposition::Candidate);
                candidates.push(candidate.clone());
            }
            // Unreachable while a row is either settled or a candidate, and
            // written out rather than assumed: a build that produced a third
            // state would otherwise silently drop the row from both lists, and
            // the commit reads them positionally.
            (None, None) => {
                return Err(AppError::Store(format!(
                    "row {} of the session is neither a candidate nor settled",
                    read.row
                )));
            }
        }
    }

    let source_inventory = inventory(&contents, &read_rows);
    // What the readings of this session's documents could not place. Narrowed to
    // this session's documents and not left at the owner's whole set: the
    // assessment answers for this import, and a name another import's document
    // printed is that import's question.
    //
    // Two tests, because neither alone is the set. A reading records the session
    // it was read into, which is the ordinary tie — and it is the only tie there
    // is when *every* record of the document was refused, because then the
    // session holds no row naming that document. And a record is kept per
    // document, so the remedy of §5 — retract, then read the same bytes into a
    // fresh session — moves the record to the newer session while this one still
    // holds rows out of that document; the inventory's own digests are what keep
    // this assessment answering for them.
    let recorded_names: Vec<UnresolvedAccountView> = services
        .store
        .list_unresolved_accounts(principal.owner)
        .await?
        .into_iter()
        .filter(|name| {
            name.session == session || source_inventory.documents.contains(&name.document_hash)
        })
        .collect();
    let account_resolution = account_resolution(&resolver, &read_rows, &recorded_names);
    let scope_assessment = scope_assessment(&source_inventory.accounts, &contours, &exclusions);
    // The whole reading runs before the questions are published, so a question
    // the reading itself settled is not published at all: the other leg already
    // records the movement (0031), or a standing rule of his classifies the row
    // (`iaam-m2oi`). The offers below are folded over what is genuinely still
    // open — over neither half of one movement, and over no row a rule he has
    // already written covers, which is what keeps an offer from growing every
    // month while settling nothing new.
    let open_questions = open_questions(
        &resolver.directory,
        &contents.observations,
        &contents.questions,
        &settlements,
    );
    let offers = offers(&contents.observations, &open_questions);
    // The directory the resolution already read, in the shape an answer names an
    // account by. No second read of the store: `Resolver::load` holds the very
    // accounts every row of this plan was resolved against, and the list is
    // built only where an open question admits an answer that names one — a
    // session whose questions are all about a fee that no account is the other
    // side of publishes none, which is what it means.
    let answer_accounts = answer_accounts(&resolver.directory, &open_questions);
    // The sets those questions form, folded over the relations the questions
    // above already publish rather than over a second reading of what makes two
    // questions one decision. The directory is the same one every row of this
    // plan was resolved against, because a group names its account the way the
    // owner reads it.
    let groups = row_groups(
        &contents.observations,
        &open_questions,
        &resolver.directory,
        may_generalise(principal),
    );

    let mut facts = Vec::new();
    let mut duplicates = Vec::new();
    // Rows the journal holds no key for and whose shape it already holds. They
    // are in `facts` as well, because the commit appends them; this list is
    // what the owner reads before deciding whether it should. See
    // [`ResemblingRow`].
    let mut resembling: Vec<ResemblingRow> = Vec::new();
    let mut retained = Vec::new();
    // The events behind `facts`, kept so a reader that folds this session
    // beside the journal folds what the commit would write. See
    // [`PlannedSession::would_append`].
    let mut appended: Vec<iaam_core::event::Event> = Vec::new();
    // Rows this commit was handed and will not take, in the shape a coverage
    // gap names them (iaam-bufs). Collected in the same pass as everything
    // else, for the reason the whole planner is one function: a second walk
    // over the rows would describe a different import from the one that runs.
    let mut declined: Vec<DeclinedRow> = Vec::new();
    let mut settled_without_fact: Vec<SettledRow> = Vec::new();
    for read in &read_rows {
        let Some(candidate) = &read.candidate else {
            // Settled, and settled is not retained: nothing is owed, nothing is
            // declined, and no coverage gap is written. A gap here would say
            // this attempt could not confirm the dimensions this row moves,
            // when the row moves none.
            if let Some(reason) = read.settled {
                settled_without_fact.push(SettledRow {
                    row: read.row,
                    reason,
                });
            }
            continue;
        };
        match candidate {
            Ok(event) => {
                let fact = planned_fact(read, event);
                if recorded.holds(event) {
                    duplicates.push(fact);
                } else {
                    // Level five is asked only of a row the two key levels
                    // above said nothing about — `dedup::assess`'s own order,
                    // so a row already known to be recorded is never also
                    // reported as merely resembling one.
                    if let Some(existing) = recorded.resembling(event) {
                        resembling.push(ResemblingRow {
                            fact: fact.clone(),
                            resembles: existing,
                        });
                    }
                    facts.push(fact);
                    // The same event, taken here rather than recovered later by
                    // matching row numbers against `candidates`: the split into
                    // facts and duplicates happens on this line and nowhere
                    // else, so the list of events to append is made where the
                    // decision is made.
                    appended.push(event.clone());
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
            let event = read.candidate.as_ref()?.as_ref().ok()?;
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
        resembles_recorded: resembling,
        retained_unrecorded: retained,
        settled_without_fact,
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
    // what the journal *relates*, not what it holds — and beside them the rows
    // whose shape the journal already holds under no key of theirs, which
    // commit by default too.
    //
    // The resemblance is last and not first although it is the one thing here
    // that can put a movement in the journal twice. It is level five of §10.6:
    // it proves nothing, it is true of two genuine payments of one amount on
    // one day, and it is right about neither of them. A word that let a guess
    // speak over an unanswered question or a figure that disagrees would teach
    // its reader to skip the word.
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
            rows_resembling_recorded: commit_delta.resembles_recorded.len(),
        }
    } else if mismatched_figures > 0 || misplaced_rows > 0 {
        Readiness::DoesNotReconcile {
            mismatched_figures,
            misplaced_rows,
        }
    } else if !cross_source_matching.candidates.is_empty()
        || !commit_delta.resembles_recorded.is_empty()
    {
        Readiness::RequiresOwnerDecision {
            unanswered_questions: 0,
            transfer_candidates: cross_source_matching.candidates.len(),
            rows_resembling_recorded: commit_delta.resembles_recorded.len(),
        }
    } else {
        Readiness::Ready
    };
    let interpretation = Interpretation {
        resolved,
        open_questions,
        offered_rules: offers.offered,
        withheld_offers: offers.withheld,
        answer_accounts,
        groups,
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
        dispositions,
        appended,
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

/// What the owner's journal already holds, in the terms the plan must answer in.
///
/// Built once per plan, out of the journal load the plan already makes, and
/// asked about every candidate. Three fields, and the line between the first
/// two and the third is the line between a duplicate and a disclosure.
///
/// **`keys` and `operations` are the write path's own test, not a second one.**
/// The store's `find_duplicate` looks for the source operation identifier
/// first, scoped by the source, and only then for the idempotency key, scoped
/// by the owner. This plan asked the second alone (iaam-1k9t), so a row
/// carrying the source's own identifier and no key was published as a fact and
/// then answered `duplicate` by the very commit the plan is the description of
/// — the assessment and the commit disagreeing about the one thing the
/// assessment exists to settle. Both levels are asked here, in that order and
/// with that scoping, because two answers to «is this row already recorded» is
/// how they come to differ.
///
/// The scoping is what makes the answer travel between sessions. A source is
/// [`SourceId::declared`] on the owner, the account and the channel, and
/// [`session_origin`] derives an undeclared session's from those three and
/// nothing else — so two sessions holding one statement for one account derive
/// **one** source, and a row identified by its source's operation identifier is
/// recognised across both. That is the half of «the same statement must not
/// enter twice» that a key can carry.
///
/// **`fingerprints` is not a key and decides nothing.** It is §10.6's level
/// five — [`DedupLevel::Probabilistic`], «looks like a duplicate, but there is
/// no evidence» — and it is the only thing that can be said about a row whose
/// caller named no identity at all. It is keyed on
/// [`Provenance::raw_hash`], which is what `normalize` stamps and what
/// [`iaam_ingest::dedup::fingerprint`] computes: the canonical form of the
/// account, the kind and the dates, deliberately excluding both submission
/// identifiers so that one operation sent under two different keys has one
/// fingerprint. Level five has been specified and implemented since §10.6 was
/// written and had reached no caller; this is where it reaches one, as a list
/// the owner reads and never as a decision taken for him.
///
/// The earliest event wins where several share a fingerprint. Two genuine
/// identical purchases on one day have one fingerprint between them — which is
/// exactly why level five is a hint — and naming the later one would make the
/// plan depend on the journal's insertion order, so the revision stamp would
/// move under a session nobody touched.
///
/// [`DedupLevel::Probabilistic`]: iaam_ingest::dedup::DedupLevel::Probabilistic
/// [`Provenance::raw_hash`]: iaam_core::event::provenance::Provenance::raw_hash
/// [`SourceId::declared`]: iaam_core::ids::SourceId::declared
struct RecordedIdentities {
    /// Every idempotency key in the journal. Scoped to the owner, as the
    /// store's lookup on this column is.
    keys: BTreeSet<String>,
    /// Every source operation identifier, beside the source it is unique
    /// within. The pair and not the identifier, because an identifier is unique
    /// **within a source**: comparing two across sources suppresses a
    /// legitimate fact (§10.6).
    operations: BTreeSet<(SourceId, String)>,
    /// The earliest event carrying each canonical fingerprint.
    fingerprints: BTreeMap<String, EventId>,
}

impl RecordedIdentities {
    fn of(events: &[iaam_core::event::Event]) -> Self {
        let mut keys = BTreeSet::new();
        let mut operations = BTreeSet::new();
        let mut fingerprints: BTreeMap<String, EventId> = BTreeMap::new();
        for event in events {
            if let Some(key) = &event.idempotency_key {
                keys.insert(key.clone());
            }
            if let Some(operation) = event.provenance.source_operation_id() {
                operations.insert((event.provenance.source(), operation.to_owned()));
            }
            // `load_events_through` answers in effective order, so the first
            // entry for a fingerprint is the earliest event that carries it.
            fingerprints
                .entry(event.provenance.raw_hash().as_str().to_owned())
                .or_insert(event.id);
        }
        Self {
            keys,
            operations,
            fingerprints,
        }
    }

    /// Whether the journal already holds this candidate under a key of its own.
    ///
    /// The store's own two levels, in the store's own order. A third level
    /// there — the event identifier — is not asked, because the identifier is
    /// minted for this candidate and can name nothing already written.
    fn holds(&self, event: &iaam_core::event::Event) -> bool {
        if let Some(operation) = event.provenance.source_operation_id()
            && self
                .operations
                .contains(&(event.provenance.source(), operation.to_owned()))
        {
            return true;
        }
        event
            .idempotency_key
            .as_deref()
            .is_some_and(|key| self.keys.contains(key))
    }

    /// The recorded event this candidate looks like, on the evidence of nothing
    /// but its own shape.
    ///
    /// Asked only of a candidate [`Self::holds`] answered `false` about, which
    /// is [`iaam_ingest::dedup::assess`]'s own order: a row already known to be
    /// recorded is never also reported as merely resembling something.
    fn resembling(&self, event: &iaam_core::event::Event) -> Option<EventId> {
        self.fingerprints
            .get(event.provenance.raw_hash().as_str())
            .copied()
    }
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
    /// The event the row would become, or `None` where the row is settled and
    /// deliberately becomes none.
    ///
    /// The `Option` is the third outcome and not a convenience: `Err` already
    /// means «this row should have become a fact and could not», and everything
    /// downstream of it — the coverage gap, the retention reason, the refusal
    /// to commit — says that something is owed. Nothing is owed here.
    candidate: Option<Result<iaam_core::event::Event, Rejection>>,
    /// Why the row produced nothing, where that is what happened.
    settled: Option<NoFactReason>,
    /// On whose word the row was settled, where it was settled into a fact.
    ///
    /// `None` for a row that produces no fact — unreadable, unanswered, or
    /// settled without one — because there is no fact for it to be the basis
    /// of. It is not the absence of a basis: a row nobody settled has none.
    basis: Option<FactBasis>,
}

impl ReadRow {
    /// What the reading made of this row, where the reading settled it at all.
    ///
    /// **The one definition of «settled», and it is on the row** (`iaam-m2oi`).
    /// Two things ask it and they ask at two different moments:
    /// [`QuestionSettlements::of`] asks after the mirror pass, to decide which
    /// questions are still the owner's, and [`mirrored_rows`] asks before it, to
    /// decide which rows are two sights of one movement and which of the two
    /// sides is already a fact. A second spelling for the second caller was what
    /// the mirror pass used to have — it read `ImportQuestionView::is_open`, so a
    /// row his directory or a rule of his had settled into a complete transfer
    /// counted as *not* settled while its stored question stayed open, and the
    /// pair came out as «neither side settled». Held together with the commit's
    /// new refusal that is a movement recorded twice, which is the very defect
    /// decision 0031 exists to prevent.
    ///
    /// `None` covers the two states nothing has settled: a row still ambiguous,
    /// and a row this build cannot read.
    fn settlement(&self) -> Option<QuestionSettlement> {
        match (self.settled, &self.candidate, &self.basis) {
            (Some(reason), _, _) => Some(QuestionSettlement::NoFact { reason }),
            (None, Some(Ok(_)), Some(basis)) => Some(QuestionSettlement::Fact {
                basis: basis.clone(),
            }),
            _ => None,
        }
    }

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

    /// The day the row states, read from the row itself rather than from the
    /// event it would become.
    fn stated_day(&self) -> Option<time::Date> {
        match self.intake.as_ref()? {
            Intake::Observed { row, .. } => row.dates.effective_date(),
            Intake::Concluded { operation } => operation.dates.effective_date(),
        }
    }

    /// The document the row names, when it names one.
    fn document(&self) -> Option<&str> {
        match self.intake.as_ref()? {
            Intake::Observed { row, .. } => row.identity.document.as_deref(),
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
        // A settled row has no event to be dated by, and it is still a row the
        // document printed: taking its day from what the row states keeps the
        // period from stopping short of the statement it came from.
        let day = match (&read.candidate, read.settled) {
            (Some(Ok(event)), _) => Some(event.order.date()),
            (None, Some(_)) => read.stated_day(),
            _ => None,
        };
        if let Some(day) = day {
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

/// What the rows' accounts resolved to, and what the documents asked for.
///
/// `recorded` is what the readings of this session's documents could not place,
/// as the instance kept it (decision 0024). It is read here rather than
/// recomputed because the records it came from are not in the session: a record
/// whose account name resolved to nothing was refused and never became a row,
/// so no fold over `rows` can see it, however carefully it is written.
///
/// Every one of those names is asked again, against the directory this
/// assessment was built with. The stored fact is a transcription — this document
/// printed this string — and stays true; the verdict on it is not stored, and is
/// this call.
fn account_resolution(
    resolver: &Resolver,
    rows: &[ReadRow],
    recorded: &[UnresolvedAccountView],
) -> AccountResolution {
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
        if let Some(Intake::Observed { row, .. }) = read.intake.as_ref()
            && let Some(name) = row.counterparty_name()
            && resolver.counterparty_matches(name, row.dates.effective_date()) > 1
            && !conflicting.iter().any(|seen| seen == name)
        {
            conflicting.push(name.to_owned());
        }
    }
    let known = resolver.directory.names();
    let mut unrecognised: Vec<String> = Vec::new();
    for name in recorded {
        if known.resolve(&name.printed).is_err()
            && !unrecognised.iter().any(|seen| seen == &name.printed)
        {
            unrecognised.push(name.printed.clone());
        }
    }
    AccountResolution {
        resolved,
        missing,
        conflicting,
        unrecognised,
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
        // A row that reached a candidate event was settled by something, so the
        // fallback is never taken on the path this is called from. It is
        // `Concluded` rather than a panic because a plan that could not say how
        // one row was settled is still the plan the owner is entitled to read.
        settled_by: read.basis.clone().unwrap_or(FactBasis::Concluded),
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
    // The fourth section of the account resolution, on its own line rather than
    // appended to the one above: the stamp must change when it changes — an
    // account created between the reading and the commit removes a name from it
    // — and a line per field is what keeps two sections from colliding into one
    // rendering.
    let _ = writeln!(
        rendered,
        "accounts unrecognised {:?}",
        accounts.unrecognised
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
        // The pair is on the line because it is published and can change under
        // an unchanged set of questions: a row fed later can make a pairing
        // ambiguous, and a stamp that did not move would say the assessment the
        // caller is holding is still the assessment.
        let _ = writeln!(
            rendered,
            "open {} {} {} {:?}",
            open.row,
            open.question.inner(),
            open.prompt,
            open.pair
        );
    }
    // The accounts an answer may name, and the only section of the
    // interpretation that is stamped besides the questions themselves
    // (`iaam-7iyg`). The line is drawn by what a section is derived *from*:
    // `offered_rules`, `withheld_offers` and every field of an open question
    // come out of the questions and observations this session already holds,
    // which cannot change while the questions stamped above do not. This list
    // comes out of the owner's directory, which he can change without touching
    // the session at all — so leaving it out would let an assessment read before
    // he created an account and one read after it carry one stamp.
    // Whole, and not the identifier alone: a rename changes what this section
    // publishes, and the stamp's contract is that it covers everything the plan
    // says. It is folded into a digest, so nothing the owner reads is retained.
    for candidate in &interpretation.answer_accounts {
        let _ = writeln!(rendered, "answer account {candidate:?}");
    }
    // The groups, for the same reason and no other (`iaam-cixz`): the members
    // and the relation between them come out of the questions stamped above, but
    // a group names its account with the title the owner reads, and he can rename
    // an account without touching this session. Whole, like the line above it:
    // the stamp's contract is that it covers everything the plan says, and the
    // sentence put to him is part of what it says.
    for group in &interpretation.groups {
        let _ = writeln!(rendered, "group {group:?}");
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
    for resembling in &delta.resembles_recorded {
        let _ = writeln!(rendered, "resembles {resembling:?}");
    }
    for retained in &delta.retained_unrecorded {
        let _ = writeln!(rendered, "retained {retained:?}");
    }
    for settled in &delta.settled_without_fact {
        let _ = writeln!(rendered, "settled {settled:?}");
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

/// What read the rows a session holds: the caller that submitted them.
///
/// An alias for [`iaam_ingest::operation::PARSER_VERSION`] rather than a value
/// of its own, and deliberately so — a session's rows arrive as JSON, and
/// whether the caller concluded what a row was or left that to the resolver
/// says nothing about what *read* the document. Naming it here is what makes
/// the choice visible at the two commit sites instead of inherited from a
/// default: every row committed out of a session used to record
/// `ingest/manual/1` because `normalize` stamped it, whatever had produced the
/// rows (`iaam-h69n`). When a reader in this product produces a session's rows,
/// it supplies its own version there and this constant stays what it says: the
/// version for a row nothing here read.
const SUBMITTED_PARSER_VERSION: &str = iaam_ingest::operation::PARSER_VERSION;

/// What read one row, as its provenance will record it.
///
/// [`SUBMITTED_PARSER_VERSION`] unless the intake names a reader inside this
/// product, which only a reader here can do: the DTO conversion never fills
/// that field, so a caller cannot claim a source profile's version for rows it
/// typed by hand. Written once rather than at each of the two `normalize`
/// sites, because those two are the assessment and the commit and they must
/// plan the same provenance — a difference between them would be invisible in
/// the response and permanent in the journal.
fn reader_of(intake: &Intake) -> ParserVersion {
    intake
        .reader()
        .cloned()
        .unwrap_or_else(|| ParserVersion(SUBMITTED_PARSER_VERSION.to_owned()))
}

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
#[derive(Debug)]
enum Assessment {
    Settled {
        classification: Classification,
        /// `None` where the source stated no direction and the classification
        /// survives that, which exactly one of them does. See
        /// [`ObservedRow::resolve`].
        movement: Option<Movement>,
        /// On whose word the classification was reached.
        ///
        /// Kept rather than dropped, which is the whole of `iaam-rdya`: this
        /// function used to take the classification out of
        /// [`ClassificationResult::Resolved`] and throw the basis away one line
        /// later, so a row a source asserted its own far side for and a row one
        /// of the owner's standing rules settled reached the plan as the same
        /// thing. The damage that does is measured by what was **not** asked,
        /// which appears in no list of open questions — so a profile that
        /// over-asserts is invisible exactly where the plan is read.
        basis: Basis,
    },
    /// The row is understood and deliberately becomes no journal fact.
    ///
    /// Not a failure and not an [`Self::Ambiguous`] awaiting an answer: this is
    /// a settled reading of the row that happens to have nothing to record.
    NoFact {
        reason: NoFactReason,
    },
    Ambiguous {
        question: Question,
    },
}

/// What one stored row settles as.
///
/// The distinction the session could not draw before `iaam-tb5o`: a row that
/// produced nothing was describable only as unreadable or unanswered, and both
/// hold the commit. `Verdict::Quarantined` does not help — it is the answer to
/// **a write**, computed once in the response that wrote the batch, and what is
/// wanted here is a durable disposition of a row that the plan can publish
/// before anything is written and the commit can then honour.
#[derive(Debug, Clone, PartialEq)]
pub enum RowResolution {
    /// The row becomes this operation, and the commit writes the fact.
    Fact {
        operation: Box<SubmittedOperation>,
        /// On whose word the row was settled (`iaam-rdya`).
        ///
        /// Beside the operation and not inside it: a [`SubmittedOperation`] is
        /// what will be written, and this is why it may be. The two travel
        /// together from here to [`PlannedFact::settled_by`], which is the one
        /// place a reader can compare what a row records against what settled
        /// it.
        basis: FactBasis,
    },
    /// The row is settled and produces no journal fact at all.
    NoFact(NoFactReason),
}

/// On whose word a row is settled.
///
/// **This is not [`Basis`] and it is not a superset of it** (`iaam-rdya`).
/// [`Basis`] answers for the one step [`classify`] takes; this answers for a
/// row, and a row can be settled without that step running at all — by the
/// caller having concluded, or by the owner having answered. So the two
/// vocabularies overlap in three members and neither contains the other, and
/// this one is the one a plan publishes.
///
/// **Why it is published at all.** [`PlannedFact::records_as`] names the
/// journal's event kind, which is what the fact *is*, and says nothing about
/// what made it so. Asserting a far side is the cheapest way to make questions
/// disappear — one column in a profile turns a document full of questions into
/// a document full of settled rows — and with the two spelt alike there was
/// nothing a reader of the plan could catch that with. The damage such a
/// profile does is measured by what was **never asked**, and what was never
/// asked appears in no list of open questions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactBasis {
    /// The caller stated the operation itself, and nothing here classified it.
    Concluded,
    /// The owner's directory recognised the counterparty the source printed as
    /// one of his own accounts, which named the far side and settled the row.
    Directory,
    /// The source asserted the far side is one of the owner's accounts, and
    /// nothing here checked that or could.
    ///
    /// The value this type was split for. It settles a row exactly as
    /// [`Self::Directory`] does and rests on nothing but the document's word
    /// about itself.
    SourceAsserted,
    /// One of the owner's standing classification rules.
    ///
    /// The rule is the identifier and never its text (`iaam-r0qk`). Held as
    /// text, this arm could be given a value naming a rule that cannot be read
    /// back, and [`Self::rule_settlement`] then had to answer something about
    /// it — and every word it could say was false. «No rule» is a reading that
    /// found none of his rules, which is not what happened; «nothing recorded»
    /// is the absence of the value and not a word this vocabulary can say. The
    /// state has no truthful answer, so it is made unrepresentable instead.
    Rule {
        rule: ClassificationRuleId,
        version: u32,
    },
    /// The owner answered the question this row raised.
    Answered,
}

impl FactBasis {
    /// The basis of a row the resolver settled, in this vocabulary.
    fn of(basis: &Basis) -> Self {
        match basis {
            Basis::Derived => Self::Directory,
            Basis::Asserted => Self::SourceAsserted,
            Basis::Rule { rule, version } => Self::Rule {
                rule: *rule,
                version: *version,
            },
        }
    }

    /// What the fact records about the rule that filed the row (`iaam-k4qu`).
    ///
    /// A narrower vocabulary than this one and deliberately so: the journal is
    /// asked «which rule filed this», and the four bases that name no rule
    /// answer that question identically. Which of the four it was belongs to the
    /// import assessment, which publishes this whole enumeration; putting it on
    /// the fact as well would be a second answer to «what settled this row» in a
    /// second place, and the two would come to disagree in front of the owner.
    ///
    /// Every arm answers something, and none answers «nothing recorded». That
    /// state is reserved for facts no reading of this kind produced — one
    /// written before the field existed, a correction, a broker
    /// synchronisation — and it is the absence of the whole value rather than a
    /// member of this vocabulary.
    ///
    /// Total, and it has no failure to fall back from (`iaam-r0qk`): a basis
    /// that names a rule names it as an identifier, so «a rule filed this» can
    /// never come out here as «a reading ran and no rule of his matched».
    #[must_use]
    pub const fn rule_settlement(&self) -> RuleSettlement {
        match self {
            Self::Rule { rule, version } => RuleSettlement::Rule {
                rule: *rule,
                version: *version,
            },
            Self::Concluded | Self::Directory | Self::SourceAsserted | Self::Answered => {
                RuleSettlement::NoRule
            }
        }
    }

    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Concluded => "concluded",
            Self::Directory => "directory",
            Self::SourceAsserted => "source_asserted",
            Self::Rule { .. } => "rule",
            Self::Answered => "answered",
        }
    }

    /// The same determination in words, for a reader of the plan.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Concluded => {
                "the caller submitted a finished operation, so nothing here classified the row"
            }
            Self::Directory => {
                "your account directory recognised the counterparty the source printed as one \
                 of your own accounts"
            }
            Self::SourceAsserted => {
                "the source said the other side is an account of yours and named no account; \
                 nothing here checked that, and no question was asked about the row"
            }
            Self::Rule { .. } => "a standing rule of yours matched the row",
            Self::Answered => "you answered the question this row raised",
        }
    }
}

/// Why a question is no longer waiting on the owner, when he never answered it.
///
/// **This is `iaam-m2oi`, and it is deliberately not a vocabulary of its own.**
/// The two arms are the two vocabularies this module already uses to say what
/// became of a row — [`FactBasis`] for a row that settled into a fact and
/// [`NoFactReason`] for one that correctly settled into none — and every word
/// either of them can say is already published on [`PlannedFact::settled_by`]
/// and [`SettledRow::reason`]. A third enumeration listing «rule», «mirror»,
/// «directory» again would be a second answer to «what settled this row», and
/// the two would come to disagree in front of the owner about the same row.
/// Decision 0031 needed such a word once and extended `NoFactReason` with
/// `second_leg_of_one_movement` rather than inventing one; this extends the
/// same two, by naming them together.
///
/// **What it is not is «answered».** A question a standing rule of his settled
/// is not a question he answered, and the difference is the whole of what he is
/// entitled to see: he made one decision about a condition, and this row is one
/// of the rows that decision reached. `FactBasis::Answered` is reachable here
/// and means the ordinary thing — the row carries his answer — but a question
/// carrying his answer is not open in the first place, so nothing reads this to
/// find out whether he spoke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionSettlement {
    /// The reading settled the row into a fact, on evidence that was not the
    /// owner's answer to this question.
    Fact { basis: FactBasis },
    /// The reading settled the row into no fact at all.
    NoFact { reason: NoFactReason },
}

impl QuestionSettlement {
    /// Wire code. Delegated to the two vocabularies rather than spelled again,
    /// so a caller matching on a row's disposition and a caller matching on a
    /// question's settlement match on the same words.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Fact { basis } => basis.code(),
            Self::NoFact { reason } => reason.code(),
        }
    }

    /// The same determination in words, for the owner reading it.
    ///
    /// Decision 0035: what is published to be read out to him carries what he
    /// can read beside the identifier, and the identifier here is a code no
    /// person says out loud.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Fact { basis } => basis.describe(),
            Self::NoFact { reason } => reason.describe(),
        }
    }
}

/// Why a row correctly produces nothing.
///
/// A closed enumeration, and the closure is the point rather than an accident
/// of how many cases have been found. «This row produced nothing»
/// must be a determination somebody can audit, tied to the evidence that
/// supports it; an importer that merely came up empty must never be able to
/// present itself as one of these. That is the same rule
/// `iaam_core::event::source_row` was built under for a refused row's identity,
/// and it is why the reason is a value here rather than a free string.
///
/// **Each member names the evidence it rests on, and no two rest on the same.**
/// The first is what the owner's directory establishes on its own; the second
/// is what the rest of *this session's rows* establish, which is a different
/// reading and needs a different word. An owner-declared no-fact is still
/// absent, and deliberately: his word that a row should record nothing would be
/// a third determination on a third kind of evidence, and it would need an
/// answer shape this does not add. Naming it as absent is the honest state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoFactReason {
    /// The identifier the source printed for the far side resolves to the very
    /// account the row is on, so the two sides are two payment instruments over
    /// one account.
    ///
    /// The honest financial record is nothing. The account's balance does not
    /// change, no asset class changes, and there will never be a second
    /// statement to pair with, because there is no second leg. A zero-net pair
    /// of facts would invent two movements the model says did not happen, and
    /// `CashTransfer { from: X, to: X }` is refused by the core itself
    /// (`EventValidationError::TransferToSelf`) for that same reason.
    ///
    /// **The mapping this rests on already exists** and is decision 0004's:
    /// two cards over one underlying account are one account with two aliases,
    /// each with a validity interval, so the far side's identifier resolves
    /// through the same tiering as any other. Nothing about the account model
    /// changes here; what changes is that the resolution stops being thrown
    /// away as a rejected row.
    ///
    /// A direction is not needed and is deliberately not consulted: whichever
    /// way the money ran between two instruments over one account, the account
    /// moved by nothing. That is what lets this settle a row no question could
    /// have settled.
    OneAccountTwoInstruments { account: AccountId },
    /// Another row of this session already records the movement, and a movement
    /// recorded twice moves both of its accounts twice (`iaam-3qsq`).
    ///
    /// **The second reading a document gives one movement.** A statement
    /// covering two of the owner's own accounts prints a movement between them
    /// on each of them: a departure and an arrival, same day, same amount,
    /// opposite signs. The journal shape for such a movement carries a leg on
    /// **each** account, so the two rows do not become two halves — they each
    /// become the whole thing, and the account balances move twice.
    ///
    /// **Why this is a settled row and not an unreadable or an unanswered
    /// one.** It was read; it is real; it is not waiting on anybody. A
    /// rejection would say a fact is owed and open a coverage gap for a row
    /// nothing is owed for, and a retention would hold the commit for a row
    /// there is nothing to decide about. What it produces is nothing, and
    /// `records` names the row that produces the movement instead — so the
    /// determination is auditable in the one way this enumeration insists on:
    /// the reader can go and look at the fact that was kept.
    ///
    /// It is deliberately **not** reached for two rows the source settled as
    /// `own_account_movement`. Those post one signed leg each and count
    /// nothing twice; relating them is a pairing the owner confirms
    /// (`iaam-9ck1`), and collapsing one into the other here would destroy a
    /// leg the journal correctly holds.
    SecondLegOfOneMovement {
        /// The row of this session whose fact carries the movement.
        records: u32,
    },
}

impl NoFactReason {
    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::OneAccountTwoInstruments { .. } => "one_account_two_instruments",
            Self::SecondLegOfOneMovement { .. } => "second_leg_of_one_movement",
        }
    }

    /// The reason in words, for the owner reading the plan.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::OneAccountTwoInstruments { .. } => {
                "the source names the far side as this same account, so the row moves \
                 money between two payment instruments over one account and changes \
                 no balance"
            }
            Self::SecondLegOfOneMovement { .. } => {
                "this document printed one movement between two of your own accounts on \
                 both of them, and the other row records it with a leg on each account; \
                 recording this one as well would move both accounts twice"
            }
        }
    }
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
/// Split out of [`Resolver`] so that the tiering has exactly one
/// implementation. A row's counterparty and a batch's declared account are the
/// same question asked at two moments — «which of the owner's accounts does
/// this printed string name» — and two implementations of it could come to
/// disagree, which is how a batch gets declared against one account while its
/// rows resolve against another.
///
/// **The tiering itself lives one crate down**, in
/// [`iaam_ingest::csv_source::AccountNames`], and this type holds one and asks
/// it. It moved there when a document's `account` column had to ask the same
/// question (iaam-w49n): a CSV is parsed in `iaam-ingest`, which cannot see this
/// crate, so leaving the tiering here would have meant a second copy of it in
/// the parser — and the copy that already existed answered in a different
/// vocabulary and refused in words that named none. The views stay here because
/// a declaration is answered with one, and `AccountDetailView` is this crate's
/// type; what goes down is the question, not the store.
pub struct AccountDirectory {
    accounts: Vec<AccountDetailView>,
    /// The same accounts, in the shape the tiering searches.
    ///
    /// Held rather than rebuilt per row: a batch of two hundred rows asks the
    /// same question two hundred times, and rebuilding the table each time
    /// would allocate the owner's whole directory per row.
    names: AccountNames,
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
            directory: AccountDirectory::from_accounts(accounts),
            statements,
            rules,
        })
    }
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
    pub fn from_accounts(accounts: Vec<AccountDetailView>) -> Self {
        let names = accounts.iter().map(entry_for).collect();
        Self { accounts, names }
    }

    /// The same accounts, as a document's parser reads them.
    ///
    /// A parser is handed this rather than being handed the account views and
    /// left to build the table itself, because the translation from a view to a
    /// vocabulary — which field is an identity, which is a title, which carries
    /// an interval — is a decision, and a second copy of it in the server layer
    /// is a place the two answers could drift apart.
    #[must_use]
    pub fn names(&self) -> AccountNames {
        self.names.clone()
    }

    /// Every account a printed counterparty could be, from the strongest kind of
    /// evidence that recognised anything.
    ///
    /// The tiering is [`AccountNames::candidates`], one crate down, and this is
    /// the whole of what happens here: a row's counterparty, a batch's
    /// declaration and a document's `account` column all reach that one
    /// function, so none of them can come to answer differently.
    fn candidates(&self, name: &str, on: Option<time::Date>) -> Vec<AccountId> {
        self.names.candidates(name, on)
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
        self.resolve(printed)
            .map(|account| account.id)
            .map_err(|refusal| refusal.into_rejection("account"))
    }

    /// The one account a printed identifier names, or why it names none.
    ///
    /// The refusal is worded in [`AccountNames::resolve`] and nowhere else, so
    /// that a declaration, a row of a JSON batch and the `account` column of a
    /// document refuse an identifier in the same words under three different
    /// field names. Two wordings would eventually describe two different rules,
    /// and one of them did: the document's column refused with `directory name`
    /// and accepted only a title, while this one named two identifiers
    /// (iaam-w49n).
    fn resolve(&self, printed: &str) -> Result<AccountDetailView, UnresolvedAccount> {
        let only = self.names.resolve(printed)?;
        Ok(self
            .accounts
            .iter()
            .find(|account| account.id == only)
            .cloned()
            .expect("a candidate comes from this directory"))
    }

    fn title(&self, account: AccountId) -> String {
        self.names.title(account)
    }

    /// The owner's own words for one account, where this directory holds it.
    ///
    /// **Not [`Self::title`], and the difference is the whole point**
    /// (`iaam-6jsj`). That function falls back to the identifier so that a
    /// refusal an operator reads is never empty; a *published* title that can
    /// come out as a uuid is the defect decision 0035 was written against, so
    /// this one says `None` instead and lets the reader publish the absence.
    ///
    /// `None` is an account this directory does not hold. That is not an error
    /// here: a row may name an account by identifier that the owner's directory
    /// has never held, which is what [`AccountResolution::missing`] is for.
    fn held(&self, account: AccountId) -> Option<&AccountDetailView> {
        self.accounts.iter().find(|known| known.id == account)
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
        // Read **before** the classifier, because the classifier has no word
        // for it: `Counterparty::OwnAccount(row.account)` becomes
        // `InternalTransfer { to: row.account }`, which `ObservedRow::resolve`
        // then refuses as a transfer to itself — so the row used to come out
        // unreadable, and the one thing the directory actually established
        // about it was thrown away with the rejection.
        //
        // Nothing is hidden by settling it here: the plan publishes the row and
        // this reason before the commit writes anything, so an owner whose bank
        // prints the near side in the counterparty column sees a row named as
        // producing nothing rather than a row silently dropped.
        if resolved == Some(row.account) {
            return Assessment::NoFact {
                reason: NoFactReason::OneAccountTwoInstruments {
                    account: row.account,
                },
            };
        }
        let subject = row.subject(resolved);
        let (classification, basis) = match classify(&subject, &self.rules) {
            ClassificationResult::Resolved {
                classification,
                basis,
            } => (classification, basis),
            ClassificationResult::Ambiguous { question } => {
                return Assessment::Ambiguous { question };
            }
        };
        match (classification, movement_of(classification, row)) {
            (classification, Some(movement)) => Assessment::Settled {
                classification,
                movement: Some(movement),
                basis,
            },
            // The one outcome the journal can record without a direction, so
            // the one outcome that does not become a question. Everything else
            // still does, and must: «the source printed a word for it» is not
            // «the source said which way it went», and the four rows that
            // opened this bead were four questions about exactly that.
            (Classification::OwnAccountMovement, None) => Assessment::Settled {
                classification,
                movement: None,
                basis,
            },
            (_, None) => Assessment::Ambiguous {
                question: Question::UnresolvedDirection {
                    account: row.account,
                    stated: row.source_kind.clone(),
                    counterparty: row.counterparty_name().map(str::to_owned),
                },
            },
        }
    }

    /// The question in words, with account titles rather than identifiers, and
    /// with the row named the way a person can find it on a statement.
    ///
    /// The rendering is here rather than in `iaam-ingest` because the pure
    /// function has no directory, and a sentence containing a UUID is not a
    /// specific question.
    ///
    /// **Why the row is handed in as well as the question (`iaam-3ewp`).** A
    /// [`Question`] carries what the row left open — the account, the word the
    /// source printed, the party it named — and none of that distinguishes one
    /// row of a statement from another. One export raised four
    /// `UnresolvedDirection` questions whose four sentences were identical to
    /// the character, because all four rows carried the same word and named
    /// nobody. The owner matched question to row by counting down the list, got
    /// the offset wrong, and answered for rows he had not read — and a wrong
    /// answer is *accepted*: it settles the row, may be generalised into a
    /// standing rule, and nothing ever asks again.
    ///
    /// This is **not** the identifier problem earlier waves fixed. The row
    /// number is a perfectly good identifier, it is published beside the
    /// question, and it is what the answering call takes. What was missing is
    /// what a *human* recognises a row by, and on a bank statement that is the
    /// date and the amount.
    ///
    /// **Why they go in the sentence rather than into two new published
    /// fields.** The prompt is the one carrier both surfaces share: the same
    /// string is the ingest verdict's `question`, the session's `prompt`, and
    /// the action queue's `reason` ([`crate::actions`]). Fields on
    /// `ImportQuestionDto` would leave the queue publishing four items nothing
    /// tells apart. And a published amount invites a client to compute with it,
    /// while the figures a session's rows are read by are published by the
    /// assessment route, computed by planning the commit — a second rendering
    /// of the same rows built from the stored observation is exactly the pair of
    /// readings that can disagree which `ImportSessionContentsDto.row_count`
    /// documents refusing.
    ///
    /// **[`OpenQuestion`] now publishes those values as fields as well, and this
    /// paragraph still holds** (`iaam-pm4w`). What it argued against is a second
    /// *reading*, and there is none: a row with an open question produces no
    /// planned fact, so nothing else in the assessment states a figure for it,
    /// and the fields carry the source's own printed amount rather than what the
    /// commit would post. What it argued *for* — that the sentence must name the
    /// row, because the queue and the ingest verdict carry the sentence and
    /// nothing else — is untouched: the sentence keeps the date and the amount,
    /// and the fields are for the caller that must group and total rather than
    /// read one out.
    fn render(&self, question: &Question, row: &ObservedRow) -> String {
        let account = self.title(question.account());
        let mark = row_mark(row);
        // One clause, identical on all four, saying that the alternatives carry
        // their own consequences. The consequences themselves are **not** here:
        // seven of them in one sentence would be a mapping from a word to its
        // effect encoded as prose, which `docs/api/conventions.md` §5 refuses,
        // and they are published attached to the words they belong to — see
        // [`AnswerShape::consequence`].
        let stakes = "What you answer decides which figure the row moves in your money-flow \
                      report; each alternative published with this question says which.";
        match question {
            Question::IsTransferInternal { counterparty, .. } => format!(
                "On {account}, {mark}: the source named «{counterparty}» as the other side. \
                 Is that one of your own accounts, and if so which one? {stakes}"
            ),
            Question::IsOutflowAFee { .. } => format!(
                "On {account}, {mark}: money left the account and the source named no \
                 counterparty. Was it a fee, or a payment out? {stakes}"
            ),
            // Three alternatives and therefore three clauses. The middle one is
            // new wording as well as a new answer: the question used to read
            // «income, or money coming back?», where «money coming back» was the
            // sentence for `received` — money arriving from outside — and read
            // to a human, and to an agent relaying it, as the refund the
            // vocabulary could not express (`iaam-7l7v`).
            Question::IsInflowIncome { .. } => format!(
                "On {account}, {mark}: money arrived and the source named no counterparty. \
                 Was it income the capital earned, money a counterparty returned \
                 on something you paid for, or money coming in from outside? {stakes}"
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
                format!(
                    "On {account}, {mark}: the source stated {word} and {rest}. \
                     Which was it? {stakes}"
                )
            }
        }
    }
}

/// The row as a person finds it on the statement in front of him: its date and
/// the amount with the sign the source printed.
///
/// **Two facts and no more.** The description would narrow it further, and it is
/// deliberately left out: it is the row's whole text, of unbounded length and
/// written by the source, and pasting it into a sentence the owner reads is how
/// a statement's own words end up quoted in a queue item, a log line and an
/// agent transcript. A date and an amount identify a line on a month's statement
/// well enough to point at, and stop there.
///
/// **The sign is kept.** [`ObservedRow::amount_minor`] holds the amount as the
/// source printed it, and this is a recognition aid: the owner is matching it
/// against a line he is looking at, not against a normalised figure. For the one
/// question where the direction is genuinely open the sign is not evidence of
/// anything — that is why the question exists — and printing it as the source
/// did says exactly what the source said and no more.
///
/// **An undated row says so.** The formatting drops nothing silently: a row with
/// no date at all is a row the commit will refuse for want of one, and a
/// sentence that just omitted the date would read as a row dated nowhere in
/// particular.
///
/// [`decimal`] and not arithmetic of its own: the same value in another
/// representation, which is the one transition §3.4 allows, and the one this
/// module already prints a control figure with.
fn row_mark(row: &ObservedRow) -> String {
    let amount = decimal(PostedMinor::new(row.amount_minor), row.currency);
    let code = row.currency.code();
    row.dates.effective_date().map_or_else(
        || format!("the row for {amount} {code}, which the source left undated"),
        |date| format!("the row dated {date} for {amount} {code}"),
    )
}

/// One account as the tiering reads it.
///
/// The one translation from a stored view to a vocabulary, and the reason
/// [`AccountDirectory::names`] hands out a built table rather than the views: a
/// second place deciding that `provider_account_id` is an identity and `title`
/// is a name is a second place that could decide otherwise.
///
/// `provider_account_id` carries no interval because the owner stated it for the
/// account, not for a stretch of its history; an alias carries its own, which is
/// how two cards over one underlying account stay one account.
fn entry_for(account: &AccountDetailView) -> AccountEntry {
    AccountEntry {
        id: account.id,
        printed: account
            .provider_account_id
            .iter()
            .map(|value| (value.clone(), None))
            .chain(account.aliases.iter().map(|alias| {
                (
                    alias.value.clone(),
                    Some(iaam_core::instrument::AliasInterval {
                        valid_from: alias.valid_from,
                        valid_to: alias.valid_to,
                    }),
                )
            }))
            .collect(),
        title: account.title.clone(),
    }
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
        | Answer::Refund
        // The one own-account answer that names none, which is the whole of it.
        | Answer::BetweenOwnAccounts => None,
    }
}

/// The namespace the identifier of one mirrored pair is derived in.
///
/// A constant so the derivation is a function of the session and the two rows
/// and of nothing else: two readings of an unchanged session must publish one
/// identifier, or the revision stamp moves under a session nobody touched.
const MIRRORED_PAIR_NAMESPACE: uuid::Uuid = uuid::uuid!("4a6d27e5-0dc6-43ba-8069-dfee3c007f89");

/// What one session's rows say about each other: which of them are two sights
/// of one movement (`iaam-3qsq`, decision 0031).
///
/// **Derived and never stored**, for [`Generalisation`]'s reason and with the
/// same force. Every input already lives in the session — the observations, the
/// answers, the readings each of them yields — and a stored copy would be a
/// second place recording one determination, able to disagree with the first in
/// silence. Derived, the pairing is also the pairing that holds **now**: a row
/// fed after the pair was found, or an answer that named a third account,
/// changes it on the next reading, which is exactly what should happen to a
/// hypothesis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MirroredRows {
    /// Rows whose movement another row of this session records, and that row.
    ///
    /// The map a reading acts on: its keys record nothing, and the fact each of
    /// them would have written is already in the plan under the value.
    settled: BTreeMap<u32, u32>,
    /// Pairs whose questions are both still open, and the identifier that says
    /// the two are one decision.
    ///
    /// Nothing is settled by these and nothing is suppressed: they are
    /// published on [`OpenQuestion::pair`] so a caller can put one decision to
    /// the owner instead of two, and his answer is what turns the hypothesis
    /// into a fact or refuses it.
    open: Vec<(u32, u32, Uuid)>,
}

/// The other leg of one movement, and the identifier the two questions share.
///
/// **Both, because either alone is half of what a caller has to say**
/// (`iaam-6jsj`). The identifier states that two questions are one decision and
/// does not state *which* two; the row states which row and cannot state that
/// the relation is a pair rather than [`OpenQuestion::alike`]. A caller holding
/// only the identifier had to scan every other open question for a match before
/// it could name the other row to anybody — and [`OpenQuestion::alike`], the
/// larger relation, has published its rows outright since it existed. The
/// smaller one published our correlation key instead, which is decision 0035's
/// third rule: an identifier this system minted for its own bookkeeping is not
/// a fact about the owner's money and does not stand in for one.
///
/// Neither half is derived from the other. The identifier is derived from the
/// session and both row numbers, so it is stable across readings of an unchanged
/// session; the row is read off the same tuple, so the two cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirroredPair {
    /// The identifier both questions of this pair carry, and no other question
    /// of the session does.
    pub id: Uuid,
    /// The other row of the pair. A row of this session, so it addresses the
    /// answering call exactly as [`OpenQuestion::row`] does.
    pub row: u32,
}

/// One movement a document printed on both of its accounts, with both of its
/// rows named (`iaam-lkvb`).
///
/// **[`MirroredPair`] read from outside one question, and that is the whole
/// difference.** A caller walking a session's open questions one at a time holds
/// the row it is on and asks what the other one is; a caller that publishes
/// **one** piece of work for the two holds neither yet and has to name both — so
/// it needs the two legs together, not the relation from one side.
///
/// Named by what each source line printed, because that is how a row is named to
/// the owner. A row number is enough for a call and is not enough for a person:
/// several lines of one month can be identical in everything the number does not
/// say, and «row 4» tells nobody which line of the statement in front of him
/// this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirroredMovement {
    /// The identifier both rows carry, and no other row of the session does.
    ///
    /// [`pair_identity`]'s, so it is the same identifier the assessment
    /// publishes on the two questions and is a function of the session and the
    /// two rows alone — a caller that deduplicates by it sees nothing move
    /// between two readings of an unchanged session.
    pub id: Uuid,
    /// The row on the account the money left.
    ///
    /// The one of the two a caller acts on, wherever it must pick one: a
    /// transfer is recorded from its sending side everywhere else in this
    /// system, and `mirrored_rows` keeps the departure's fact for the same
    /// reason.
    pub departure: MovementLeg,
    /// The row on the account the money reached.
    pub arrival: MovementLeg,
}

impl MirroredMovement {
    /// This row's own leg.
    ///
    /// Beside [`Self::far_of`] rather than left to the caller to pick by
    /// comparing row numbers: a caller that publishes the two together names
    /// both, and the two lookups are one question — which of the two is this —
    /// answered once here.
    #[must_use]
    pub const fn leg_of(&self, row: u32) -> Option<MovementLeg> {
        if self.departure.row == row {
            Some(self.departure)
        } else if self.arrival.row == row {
            Some(self.arrival)
        } else {
            None
        }
    }

    /// The leg that is not this row's.
    ///
    /// `None` for a row this movement does not hold, which is the same answer
    /// [`MirroredRows::pair_of`] gives such a row.
    #[must_use]
    pub const fn far_of(&self, row: u32) -> Option<MovementLeg> {
        if self.departure.row == row {
            Some(self.arrival)
        } else if self.arrival.row == row {
            Some(self.departure)
        } else {
            None
        }
    }
}

/// One leg of such a movement, as the source printed the row.
///
/// Nothing here is normalised, for [`PrintedRow`]'s reason: every field says
/// what the document said, so that the owner can match it against the line in
/// front of him.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovementLeg {
    /// The row's position in its session, which is what the answering call
    /// takes.
    pub row: u32,
    /// The account whose statement printed it.
    pub account: AccountId,
    /// The day the source dated the row.
    ///
    /// Not an `Option`, unlike [`PrintedRow::date`]: a row the source left
    /// undated is not a side of anything — [`mirrored`] pairs on an exact day —
    /// so a leg with no day cannot exist.
    pub date: time::Date,
    /// The amount **with the sign the source printed**, for
    /// [`PrintedRow::amount_minor`]'s reason.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
}

/// What one row of a session is to a movement its document printed twice.
///
/// Two states and not one, because a caller publishing work about the row owes a
/// different sentence for each: two rows that are one question, and a row that
/// is no longer a question at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneMovement {
    /// This row and one other are the two legs, and both are still the owner's
    /// to answer. One answer settles both, so they are one decision and belong
    /// in one piece of work.
    Waiting(MirroredMovement),
    /// The other leg's answer already records the movement, so this row has
    /// nothing of its own left to record.
    ///
    /// [`NoFactReason::SecondLegOfOneMovement`] as a caller that has not read
    /// the whole session can see it, and the row it names is the row that
    /// records the movement — which is what makes it a statement rather than a
    /// disappearance.
    Recorded { by: u32 },
    /// This row could be one leg, and this document holds no other half for it
    /// (`iaam-0evk`).
    ///
    /// **A third state and not the absence of the first two.** A row half of
    /// nothing this reading can see publishes no [`OneMovement`] at all, and
    /// that is the ordinary row: a card payment is not the near half of
    /// anything and has nothing to be told about. This is the row that *is*
    /// leg-shaped — the owner may answer it «money I moved to my own account»,
    /// because that is one of the words the question admits — and whose other
    /// half no row of this document is. Answered as an ordinary row it produces
    /// one of two wrong facts: a transfer whose far leg is not there, or a
    /// movement between his own accounts filed as spending.
    NoCounterpart(NoCounterpart),
}

/// Why this row has no other half here, said as narrowly as it is known.
///
/// **«In this document» and never «nowhere».** The far half may be in a
/// statement the owner has not brought yet, or on an account he did not put in
/// his group — the second is the ordinary case and it looks exactly like the
/// first from here. Nothing published from this value says the movement had no
/// other half, only that this document does not hold it.
///
/// Where the document covered one account the reason is known and is worth
/// saying: a movement between two accounts prints its halves on two accounts, so
/// a document holding one of them never held the other. That is a different
/// conversation from a bare «no counterpart», because it tells him what to do —
/// bring the other account's statement, or name the account he has not declared
/// — rather than leaving him to search a file that cannot contain the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoCounterpart {
    /// Every row this session holds is on one account, so no row of it could
    /// have been the far half of a movement between two.
    ///
    /// The narrower thing, and it is arithmetic over the rows rather than a
    /// conclusion: the accounts their own statements printed them on, counted.
    OneAccount,
    /// The document covered more than one account and no row of it is
    /// **available** to be this row's other half.
    ///
    /// The bare fact, said where nothing explains it. It is deliberately not
    /// dressed up as the first: several accounts *could* have held the far half
    /// and none of them did, which says nothing about why.
    ///
    /// **«Available» and not «there is none»** (`iaam-y5ww`). This is
    /// [`Unpaired::NoCounterpart`] relayed, and that value covers two
    /// situations: nothing in the document mirrors the row at all, or everything
    /// that does is already the other half of another movement — one arrival
    /// cannot be the far half of two departures, so the leftover departure of
    /// several identical ones lands here with a row of the document that *is*
    /// its opposite half, spent on an earlier pairing. A sentence saying no row
    /// of the document is that half would deny a row the document printed.
    SeveralAccounts,
}

impl NoCounterpart {
    /// The sentence for a surface reporting about the owner.
    ///
    /// **Beside the value rather than at the surface**, which is
    /// [`GeneralisationProspect::reported`]'s reason held here: a wording
    /// written where an item is built is a second answer to a question this
    /// enum already answers, and the two disagree the moment one of them is
    /// edited. What a publisher does is relay it.
    ///
    /// Both sentences say **in this document**, and neither says the far half
    /// does not exist. That is not a nicety of tone: a movement between two
    /// accounts prints its halves on two accounts, this reading sees one
    /// document, and the half it cannot see is ordinarily on an account the
    /// owner has not declared. Asserting its absence would be the fabrication
    /// [`iaam_ingest::mirror`] refuses in the other direction.
    ///
    /// Both also say what the two ordinary answers would do, because that is
    /// what makes this a different question rather than a warning label. The
    /// question the row raises is where the other half went; «name the far
    /// account» and «this left the perimeter» are answers to a question nobody
    /// asked of it.
    ///
    /// **And both then name the answer that is** (`iaam-axrf`). Until there was
    /// one, this paragraph told him what his two words would do to the row and
    /// left him to pick one of them anyway — a door pointed at and not built,
    /// which was the one criterion the wave that wrote it could not meet. The
    /// third answer is offered by exactly the questions this paragraph is
    /// published on, because both gates are `AnswerShape::needs_account` over
    /// what the question admits, so the sentence cannot name a word its own
    /// question would refuse.
    #[must_use]
    pub const fn reported(self) -> &'static str {
        match self {
            Self::OneAccount => {
                "This document holds no counterpart for it, and why is known: every row of this \
                 session is on one account, and a movement between two accounts prints one half \
                 on each — so a statement of one of them never held the other. That is «not in \
                 this document» and not «nowhere»: if this row was money moved between the \
                 owner's own accounts, the far half is on a statement not yet handed over, or on \
                 an account that is not in his directory. Naming a far account in the answer \
                 records the whole movement from this row alone, and answering that the money \
                 left the perimeter files it as spending — so what this row needs settled is \
                 where the other half went, and not which of the ordinary alternatives fits. There is an answer for exactly \
                 that, published among this question's alternatives: that the money moved \
                 between accounts of his and that he cannot say which account the other side \
                 is. It records the movement without inventing a half the document does not \
                 hold and without filing it as spending, and it keeps no standing decision, \
                 because it says what this statement did not contain rather than what the line \
                 was."
            }
            Self::SeveralAccounts => {
                "This document holds no counterpart for it: it covers more than one account, and \
                 no row on any of them is available to be the opposite half of the same amount \
                 on the same day — either no row of it mirrors this one, or the one that does is \
                 already the other half of another movement. That is «not in this document» and \
                 not «nowhere»: if this row was money moved between the owner's own accounts, \
                 the far half is on a statement not yet handed \
                 over, or on an account that is not in his directory. Naming a far account in \
                 the answer records the whole movement from this row alone, and answering that \
                 the money left the perimeter files it as spending — so what this row needs \
                 settled is where the other half went, and not which of the ordinary \
                 alternatives fits. There is an answer for exactly \
                 that, published among this question's alternatives: that the money moved \
                 between accounts of his and that he cannot say which account the other side \
                 is. It records the movement without inventing a half the document does not \
                 hold and without filing it as spending, and it keeps no standing decision, \
                 because it says what this statement did not contain rather than what the line \
                 was."
            }
        }
    }
}

/// What a session's questions are to the movements its document printed twice
/// (`iaam-lkvb`).
///
/// **The queue's reader for the pairing, and not a second derivation of it.**
/// The action queue publishes an item per open question and never consulted the
/// pairing at all, so one movement a document printed twice reached the owner as
/// two independent items — and an agent working them answered both legs
/// separately, which records the movement twice or leaves one leg an orphan. The
/// fix could have been a pairing written beside the queue; two derivations of
/// one fact on two surfaces is the defect one level up, so this goes through
/// [`mirrored`], [`unanswered_side`] and [`pair_identity`] — the three the
/// assessment's own pass is built from — and splits the result the three ways
/// [`mirrored_rows`] splits it.
///
/// **It sees less than [`mirrored_rows`] and says so.** That pass runs over rows
/// already read against the owner's directory and his standing rules, so a leg
/// his directory recognised is a named side of it. This has the session's
/// observations and its stored questions and nothing else — which is exactly
/// what [`crate::actions`] holds, and it holds them without loading a session —
/// so the far sides it can name are the ones a question states on its own: a row
/// he answered as a movement to or from one of his own accounts names the far
/// side in the answer itself, and nothing else here names one at all.
///
/// **It asks [`mirrored`] about every readable row of the session all the same,
/// and not only about the questioned ones** (`iaam-y5ww`). What stood here
/// before was the claim that a pair this finds is a pair the deep pass finds,
/// «because both ask [`mirrored`] the same question about the same two rows»,
/// and it was false: [`mirrored`]'s ambiguity refusal is a function of the whole
/// side set and not of two rows, and the two passes were handed different sets.
/// An open departure, an open arrival, and a third arrival of the same amount on
/// the same day that a rule or the directory had already settled: this pass saw
/// two sides and paired them, the deep pass saw three, found two candidate
/// counterparts and refused — so the queue published a pairing the assessment
/// denied and suppressed the arrival's own open question behind it. That is this
/// wave's own defect one level up, and the fix is to weigh the same rows.
///
/// A row carrying no question was settled by something this pass cannot see — a
/// rule of his, his directory, the source's own word — so it is read as its
/// source printed it, with no far side named. That is the widest reading of it,
/// and widening can only add candidate counterparts, which can only make
/// [`mirrored`] refuse. What remains of the difference therefore runs one way:
/// this pass refuses pairs the deep pass makes, and a row it refuses about is
/// published as it always has been, which is where this started rather than
/// something it breaks.
///
/// **A pair whose far half carries no question publishes nothing**, for the same
/// reason. `Recorded` names the row that records the movement, and this pass
/// cannot know what a row it never questioned was settled as — a fee, income,
/// money that left the perimeter. Naming it as the row that records this
/// movement would be the fabrication the whole module is written against, so the
/// near row is published as the ordinary row it was published as before.
#[must_use]
pub fn mirrored_movements_of(
    session: ImportSessionId,
    observations: &[ImportObservationView],
    questions: &[ImportQuestionView],
) -> BTreeMap<u32, OneMovement> {
    let rows: BTreeMap<u32, ObservedRow> = observations
        .iter()
        .filter_map(|observation| match parse_intake(&observation.payload) {
            // A row the caller concluded is not an observation and is no side
            // of anything: it states what it was, and this pass has nothing to
            // add to it.
            Ok(Intake::Observed { row, .. }) => Some((observation.row, *row)),
            Ok(Intake::Concluded { .. }) | Err(_) => None,
        })
        .collect();
    let asked: BTreeMap<u32, &ImportQuestionView> = questions
        .iter()
        .map(|question| (question.row, question))
        .collect();
    let mut open: BTreeSet<u32> = BTreeSet::new();
    let mut sides: Vec<MirrorSide> = Vec::new();
    for (row, observed) in &rows {
        match asked.get(row) {
            Some(question) if question.is_open() => {
                open.insert(*row);
                sides.extend(unanswered_side(*row, observed));
            }
            Some(question) => sides.extend(answered_side(*row, observed, question)),
            // Settled by something this pass cannot see, and read at its widest
            // for the reason above.
            None => sides.extend(unanswered_side(*row, observed)),
        }
    }
    let mut movements = BTreeMap::new();
    let reading = mirrored(&sides);
    for mirror in reading.pairs {
        // The three outcomes [`mirrored_rows`] decides, decided the same way and
        // on the same question: how many of the two sides are already answered.
        // A pair with one side answered is one movement already recorded, and
        // the other side has nothing of its own to add.
        match (
            open.contains(&mirror.outgoing),
            open.contains(&mirror.incoming),
        ) {
            (true, true) => {
                let (Some(departure), Some(arrival)) = (
                    rows.get(&mirror.outgoing)
                        .and_then(|observed| movement_leg(mirror.outgoing, observed)),
                    rows.get(&mirror.incoming)
                        .and_then(|observed| movement_leg(mirror.incoming, observed)),
                ) else {
                    continue;
                };
                let movement = OneMovement::Waiting(MirroredMovement {
                    id: pair_identity(session, mirror.outgoing, mirror.incoming),
                    departure,
                    arrival,
                });
                movements.insert(mirror.outgoing, movement);
                movements.insert(mirror.incoming, movement);
            }
            // The far half is settled, and `Recorded` names it as the row that
            // records the movement. Only a row this pass questioned may be
            // named that way: a row it never questioned was settled by
            // something it cannot see, and «that row records this movement» is
            // then a claim about a fact nobody here has read.
            (true, false) if asked.contains_key(&mirror.incoming) => {
                movements.insert(
                    mirror.outgoing,
                    OneMovement::Recorded {
                        by: mirror.incoming,
                    },
                );
            }
            (false, true) if asked.contains_key(&mirror.outgoing) => {
                movements.insert(
                    mirror.incoming,
                    OneMovement::Recorded {
                        by: mirror.outgoing,
                    },
                );
            }
            (true, false) | (false, true) => {}
            // Both answered. Each states the movement in the owner's own words
            // and the deep pass decides which of the two records it; nothing
            // here is published about either, because neither is work.
            (false, false) => {}
        }
    }
    // The sides no pair holds (`iaam-0evk`). Absence from the pairs published
    // nothing, so a leg whose other half this document does not hold reached the
    // owner among the ordinary rows with the ordinary alternatives — and both of
    // the ordinary answers write a wrong fact for it.
    let one_account = covers_one_account(observations);
    for unpaired in reading.unpaired {
        // Only the two the reason names, and only the first of them. An
        // ambiguous row is **not** a row with no counterpart — this document
        // holds more than one row that could be its other half and states
        // nothing that chooses — so it is left as it was rather than told the
        // opposite of the truth. Saying that to him is its own item and its own
        // sentence, and it is not this one.
        if unpaired.reason != Unpaired::NoCounterpart {
            continue;
        }
        // Still his to answer. A row he has already answered is not work, and
        // this says nothing a settled row needs.
        if !open.contains(&unpaired.row) {
            continue;
        }
        // And only where «money I moved to my own account» is a word the
        // question admits. That is the whole harm: an answer naming a far
        // account records a movement whose other half is not in the document,
        // and the alternatives are what say whether he can give one. A question
        // that offers no such word — an outflow that is either a fee or a
        // payment, an inflow that is either income or a receipt — cannot produce
        // the fact this warns about, and a clause on it would be a sentence
        // about something that is not the case.
        let Some(question) = questions
            .iter()
            .find(|question| question.row == unpaired.row)
        else {
            continue;
        };
        if !stored_alternatives(question)
            .iter()
            .any(|shape| shape.needs_account())
        {
            continue;
        }
        // And only where the **source** left the row leg-shaped. The
        // alternatives alone were the gate until `iaam-y5ww` and they are far
        // too wide: `Question::IsTransferInternal` admits an own-account answer
        // and is raised for any row with a named counterparty and a stated
        // direction, a card payment to a merchant the directory does not
        // recognise included. On a one-account import nothing pairs, so nearly
        // every open row was handed the paragraph — which contradicts what
        // [`OneMovement::NoCounterpart`] says about itself.
        let Some(observed) = rows.get(&unpaired.row) else {
            continue;
        };
        if !leg_shaped(observed) {
            continue;
        }
        movements.insert(
            unpaired.row,
            OneMovement::NoCounterpart(if one_account {
                NoCounterpart::OneAccount
            } else {
                NoCounterpart::SeveralAccounts
            }),
        );
    }
    movements
}

/// Whether the source's own statement leaves this row the near half of a
/// movement between two accounts (`iaam-y5ww`).
///
/// **Read off the source and not off the alternatives.** What a question admits
/// says whether the owner *could* answer «money I moved to my own account»; it
/// does not say the row looks like a leg. Three things a source can say do, and
/// they are three separate fields of its own statement:
///
/// - it asserted the far side is one of the owner's own accounts;
/// - it named no counterparty at all, so nothing on the row says the money went
///   to anybody outside;
/// - it used its own word for a movement internal to itself, which is what
///   [`ObservedDirection::Inner`] transcribes.
///
/// A merchant printed by name beside a stated direction is **not** any of the
/// three. It is the ordinary row, it is half of nothing this reading can see,
/// and it has nothing to be told about.
///
/// The first arm fires on no row today, and it is written rather than left out:
/// `classify` settles a row whose source asserted an own-account far side into
/// `Classification::OwnAccountMovement` before any question is raised about it,
/// so such a row reaches this function only if that ever stops being true. It is
/// the strongest of the three and would be wrong to answer `false` for.
fn leg_shaped(observed: &ObservedRow) -> bool {
    observed.far_side.is_own_account()
        || observed.counterparty_name().is_none()
        || observed.direction == ObservedDirection::Inner
}

/// Whether every row of this session was printed on one account.
///
/// **Arithmetic over the rows and never a conclusion**, which is what lets the
/// sentence built from it be narrow: counting the accounts the rows themselves
/// name says how many statements this session was handed, and one is the case
/// where the missing half is explained rather than merely reported.
///
/// **A row this build cannot read is an account, not nothing** (`iaam-y5ww`).
/// Dropping it used to be called the safe direction, and it is the opposite:
/// dropping rows *shrinks* the set, and a smaller set is what makes «every row
/// of this session is on one account» come out — the **narrower** of the two
/// sentences, and false the moment the row that was dropped was on a second
/// account. So an unreadable row counts as an account this reading cannot name,
/// and a session holding one is never a session of one account.
fn covers_one_account(observations: &[ImportObservationView]) -> bool {
    let mut accounts: BTreeSet<AccountId> = BTreeSet::new();
    for observation in observations {
        let Ok(intake) = parse_intake(&observation.payload) else {
            return false;
        };
        accounts.insert(intake.account());
    }
    accounts.len() == 1
}

/// A row the owner has answered as a movement between two of his own accounts,
/// as the mirror test reads it.
///
/// **The far side comes from his answer, which is the only named far side a
/// caller without the session's reading has.** [`mirror_side`]'s first branch
/// reads it off the `CashTransfer` such an answer resolves into, and the two
/// cannot disagree: the transfer's far account *is* the account the answer
/// named — `Answer::SentToOwnAccount`'s and `ReceivedFromOwnAccount`'s whole
/// content.
///
/// Every other answer yields no side, which is [`mirror_side`]'s rule word for
/// word: money that left the perimeter, income, a fee or a refund is a row his
/// answer already said what it was, and a shape it happens to share with another
/// row does not overrule it.
///
/// **`BetweenOwnAccounts` is among the others and is the interesting one**: it
/// says the far side is his and does *not* name it, so there is no account to
/// put here. Deriving one — pairing it with whichever row of the session
/// mirrors it — would be the matcher decision 0013 §5 declined to build, and
/// building it in this function would build it for one row at a time and call it
/// his answer.
fn answered_side(
    row: u32,
    observed: &ObservedRow,
    question: &ImportQuestionView,
) -> Option<MirrorSide> {
    let answer: Answer = serde_json::from_str(question.answer.as_deref()?).ok()?;
    let (direction, far_side) = match answer {
        Answer::SentToOwnAccount { to } => (Movement::Out, to),
        Answer::ReceivedFromOwnAccount { from } => (Movement::In, from),
        Answer::Paid
        | Answer::Received
        | Answer::Fee { .. }
        | Answer::Income { .. }
        | Answer::Refund
        | Answer::BetweenOwnAccounts => return None,
    };
    Some(MirrorSide {
        row,
        account: observed.account,
        direction,
        amount_minor: observed.amount_minor.checked_abs().filter(|it| *it > 0)?,
        currency: observed.currency,
        date: observed.dates.effective_date()?,
        far_side: Some(far_side),
    })
}

/// One leg named by what its line printed.
fn movement_leg(row: u32, observed: &ObservedRow) -> Option<MovementLeg> {
    Some(MovementLeg {
        row,
        account: observed.account,
        date: observed.dates.effective_date()?,
        amount_minor: observed.amount_minor,
        currency: observed.currency,
    })
}

impl MirroredRows {
    /// The pair this row's question belongs to, if it belongs to one.
    fn pair_of(&self, row: u32) -> Option<MirroredPair> {
        self.open
            .iter()
            .find(|(outgoing, incoming, _)| *outgoing == row || *incoming == row)
            .map(|(outgoing, incoming, id)| MirroredPair {
                id: *id,
                // The *other* row, which is why the tuple is read rather than
                // the identifier alone: read from the side the caller asked
                // about, each of the two questions names the one it is not.
                row: if *outgoing == row {
                    *incoming
                } else {
                    *outgoing
                },
            })
    }
}

/// What this session's own reading has already settled, and what it has paired.
///
/// **The one place that decides whether a question is still waiting on the
/// owner** (`iaam-m2oi`). Before this existed, every reader asked
/// [`ImportQuestionView::is_open`], which reads one column of the questions
/// table and therefore answers a different question: «has he answered it», not
/// «does anything still need him to». The two came apart the moment he adopted
/// an offered rule — the rule settles the rows, the stored questions do not
/// move, and the assessment then named one row in `resolved` and in
/// `open_questions` at once while the commit went on refusing over questions
/// nothing needed answered. The offer's own sentence promised the opposite.
///
/// **Computed from the rows, never stored.** [`MirroredRows`] argues this for
/// the pairing and the argument is the same one, with more force: a settlement
/// recorded in the questions table would be a verdict about the owner's rules
/// and his directory, and both move. He creates an account and a row that was a
/// question resolves itself; he retires the rule and the row goes back to being
/// one. A stored verdict is right at the moment it is written and stale
/// afterwards, and the queue that publishes work already done is the one he
/// learns to ignore — which is `account_named_by_document_completion`'s
/// argument, held here. Retiring the rule then needs no compensating write in a
/// table another port owns and no transaction spanning the two, which is the
/// shape `iaam-77hk` was filed on: the next reading simply finds no rule, the
/// row assesses as ambiguous again, and the question is waiting again.
///
/// **Folded over the rows as they were finally read**, mirror pass included, so
/// there is exactly one computation and not one per reader. A row settles into
/// a fact or into a deliberate no-fact; either way nothing is owed and the
/// question about it is answered by the reading. A row that reached neither is
/// a row whose question is genuinely still his to answer — including a row this
/// build cannot read, whose question stays open because no rule can settle a
/// row nothing can test a rule against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionSettlements {
    /// Row number to what settled it. A row absent here is a row the reading
    /// could not settle.
    settled: BTreeMap<u32, QuestionSettlement>,
    /// The other leg, for the rows whose pair is still open on both sides.
    ///
    /// Carried here rather than beside it because a caller reading a question
    /// reads both — whether it is still waiting, and which other question is
    /// the same decision — and two arguments threaded through the same readers
    /// are two things that can be passed from two different readings.
    pairs: BTreeMap<u32, MirroredPair>,
}

impl QuestionSettlements {
    /// Fold one reading of a session into what it settled.
    fn of(rows: &[ReadRow], mirrors: &MirroredRows) -> Self {
        let mut settled = BTreeMap::new();
        for read in rows {
            // Through [`ReadRow::settlement`], which is also what the mirror
            // pass asks one step earlier. A row that became neither a fact nor a
            // deliberate no-fact — unreadable, or read and still ambiguous —
            // leaves its question where it was.
            if let Some(settlement) = read.settlement() {
                settled.insert(read.row, settlement);
            }
        }
        let mut pairs = BTreeMap::new();
        for read in rows {
            if let Some(pair) = mirrors.pair_of(read.row) {
                pairs.insert(read.row, pair);
            }
        }
        Self { settled, pairs }
    }

    /// Whether this question is still his to answer.
    ///
    /// Both halves are necessary and neither is sufficient. `is_open` alone is
    /// the defect this type exists for. The settlement alone would read an
    /// answer the row cannot express — a stored answer whose `resolve_with`
    /// rejects — as a question still waiting, which would refuse the commit
    /// forever with no call that could clear it.
    #[must_use]
    pub fn awaits_answer(&self, question: &ImportQuestionView) -> bool {
        question.is_open() && !self.settled.contains_key(&question.row)
    }

    /// How many of them are.
    #[must_use]
    pub fn awaiting(&self, questions: &[ImportQuestionView]) -> usize {
        questions
            .iter()
            .filter(|question| self.awaits_answer(question))
            .count()
    }

    /// What settles this line, where this reading settles it at all.
    ///
    /// Asked by line rather than by question, and it does not hold back the
    /// line he answered: this says what settles a line today, not why a
    /// question stopped waiting, so «he answered it» is one of the answers
    /// rather than an omission ([`Self::settlement_of`] is the other reading
    /// and holds it back for its own reason). `None` is a line this reading
    /// settles no way at all — still his to answer, or unreadable.
    fn settlement_for(&self, row: u32) -> Option<&QuestionSettlement> {
        self.settled.get(&row)
    }

    /// What settled this question, where the reading settled it and he did not.
    ///
    /// `None` for a question still waiting **and** for one he answered: this
    /// says why a question stopped waiting without him, and «he answered it» is
    /// the other reason, which the question states itself through
    /// `answered_at`.
    #[must_use]
    pub fn settlement_of(&self, question: &ImportQuestionView) -> Option<&QuestionSettlement> {
        question
            .is_open()
            .then(|| self.settled.get(&question.row))
            .flatten()
    }

    /// The pair this row's question belongs to, if it belongs to one.
    fn pair_of(&self, row: u32) -> Option<MirroredPair> {
        self.pairs.get(&row).copied()
    }
}

/// Take the rows the mirror pass settled out of the fact pipeline.
///
/// Split out of [`plan_session`] so that a reading built for any other reason
/// is the same reading — the questions a session is waiting on are decided from
/// rows the mirror pass has already run over, and a caller that skipped it
/// would publish both legs of one movement as two open questions after decision
/// 0031 said they are one.
fn settle_mirrored(rows: &mut [ReadRow], mirrors: &MirroredRows) {
    for read in rows.iter_mut() {
        let Some(records) = mirrors.settled.get(&read.row).copied() else {
            continue;
        };
        // Not a rejection and not a retention: the row was read, it is real,
        // and the movement it states is already in the plan under another row.
        // See [`NoFactReason::SecondLegOfOneMovement`].
        read.candidate = None;
        read.settled = Some(NoFactReason::SecondLegOfOneMovement { records });
        read.basis = None;
    }
}

/// Pair the rows of this session that are one movement printed twice.
///
/// The whole of the reasoning is in [`iaam_ingest::mirror`]; this is the part
/// that cannot be pure, because it needs to know what each row was **read as**
/// and which questions are still open.
///
/// Three outcomes per pair, and they are decided by how many of the two sides
/// are already settled into a fact:
///
/// - **Both.** Each row would write a complete `CashTransfer` naming both
///   accounts — the case that put one movement in the journal twice, whether it
///   got there by the owner answering both legs or by his directory recognising
///   the far side printed on each of them. The row on the **sending** account
///   keeps its fact, because a transfer is recorded from its sending side
///   everywhere else in this system, and the other row records nothing.
/// - **One.** The settled row's fact already carries a leg on the open row's
///   account, so the open row has nothing left to add: it is settled by that
///   answer rather than by one of its own. This is what lets a session commit
///   after the owner has answered **one** of the two questions.
/// - **Neither.** Nothing is settled and nothing is suppressed. The pair is
///   published on the two questions so that one decision can be put once.
///
/// A row settled as anything else — money that left the perimeter, income, a
/// fee, a movement whose far side the source asserted and named nobody for — is
/// not a side at all. His answer said what that row was, and a shape it happens
/// to share with another row does not overrule it.
fn mirrored_rows(
    session: ImportSessionId,
    rows: &[ReadRow],
    questions: &[ImportQuestionView],
) -> MirroredRows {
    // A row is open here when its question is the owner's **and** this reading
    // settled nothing for it. Both halves, and the second is `iaam-m2oi`: the
    // set used to be read off `is_open` alone, so a row whose counterparty his
    // directory has since recognised — or that a standing rule of his now
    // classifies — counted as unsettled while carrying a complete transfer, and
    // two such rows paired as «neither side settled» instead of one recording
    // the movement for both. The stale question used to hide that behind a
    // refused commit; it does not any more.
    let open: BTreeSet<u32> = questions
        .iter()
        .filter(|question| question.is_open())
        .map(|question| question.row)
        .filter(|row| {
            rows.iter()
                .any(|read| read.row == *row && read.settlement().is_none())
        })
        .collect();
    let sides: Vec<MirrorSide> = rows
        .iter()
        .filter_map(|read| mirror_side(read, open.contains(&read.row)))
        .collect();
    let mut paired = MirroredRows::default();
    for mirror in mirrored(&sides).pairs {
        let outgoing_settled = !open.contains(&mirror.outgoing);
        let incoming_settled = !open.contains(&mirror.incoming);
        match (outgoing_settled, incoming_settled) {
            (true, true) | (true, false) => {
                paired.settled.insert(mirror.incoming, mirror.outgoing);
            }
            (false, true) => {
                paired.settled.insert(mirror.outgoing, mirror.incoming);
            }
            (false, false) => paired.open.push((
                mirror.outgoing,
                mirror.incoming,
                pair_identity(session, mirror.outgoing, mirror.incoming),
            )),
        }
    }
    paired
}

/// The identifier two questions of one movement share.
fn pair_identity(session: ImportSessionId, outgoing: u32, incoming: u32) -> Uuid {
    Uuid::new_v5(
        &MIRRORED_PAIR_NAMESPACE,
        format!("{}/{outgoing}/{incoming}", session.inner()).as_bytes(),
    )
}

/// One row as the mirror test reads it, or nothing where the row is not a side
/// of anything.
///
/// Two kinds of row are sides and no others:
///
/// - a row already read into a `CashTransfer`, which names both accounts and a
///   direction, so its far side is a fact rather than a guess;
/// - a row whose question is still open, whose far side is by definition
///   unnamed.
///
/// Everything else is excluded on purpose. A row settled as a fee, as income,
/// as money that left the perimeter or as a movement whose far side the source
/// asserted without naming is a row something already answered for, and the
/// mirror test has no business overruling that answer with a coincidence of
/// day and amount.
///
/// The direction of an open row comes from the source's own direction word, and
/// from the **sign it printed** where it used no word — which is the one place
/// in this system that reads the sign as evidence, and it is allowed to because
/// of what it can and cannot cause. A sign read wrongly can only fail to pair
/// two rows or offer a pair the owner then refuses; it can settle nothing on
/// its own, because settling needs the *other* side to be a named fact.
fn mirror_side(read: &ReadRow, questioned: bool) -> Option<MirrorSide> {
    let account = read.account()?;
    let date = read.stated_day()?;
    match read
        .candidate
        .as_ref()
        .and_then(|candidate| candidate.as_ref().ok())
    {
        Some(event) => {
            let EventKind::CashTransfer {
                from, to, amount, ..
            } = &event.kind
            else {
                return None;
            };
            let (direction, far_side) = if *from == account {
                (Movement::Out, *to)
            } else if *to == account {
                (Movement::In, *from)
            } else {
                return None;
            };
            Some(MirrorSide {
                row: read.row,
                account,
                direction,
                amount_minor: amount.amount().raw().checked_abs().filter(|it| *it > 0)?,
                currency: amount.currency(),
                date,
                far_side: Some(far_side),
            })
        }
        None if questioned => {
            let Intake::Observed { row, .. } = read.intake.as_ref()? else {
                return None;
            };
            // The account and the day above are `ReadRow`'s readings of this
            // same intake — `Intake::account` is `row.account` and
            // `ReadRow::stated_day` is `row.dates.effective_date()` — so nothing
            // is lost by letting the shared function read them off the row.
            unanswered_side(read.row, row)
        }
        None => None,
    }
}

/// One row still waiting on the owner, as the mirror test reads it.
///
/// **Shared with [`mirrored_movements_of`] rather than spelled twice**
/// (`iaam-lkvb`). The action queue pairs the same rows from the observations
/// alone, and the one thing that must not differ between the two readings is
/// what a row still waiting on him *is* — the sign read as a direction included.
///
/// The direction comes from the source's own direction word, and from the **sign
/// it printed** where it used no word, which is the one place in this system
/// that reads the sign as evidence. See [`mirror_side`] for why it is allowed
/// to: a sign read wrongly can only fail to pair two rows or offer a pair the
/// owner then refuses.
fn unanswered_side(row: u32, observed: &ObservedRow) -> Option<MirrorSide> {
    let direction = match observed.movement() {
        Some(movement) => movement,
        None => match observed.amount_minor.signum() {
            1 => Movement::In,
            -1 => Movement::Out,
            _ => return None,
        },
    };
    Some(MirrorSide {
        row,
        account: observed.account,
        direction,
        amount_minor: observed.amount_minor.checked_abs().filter(|it| *it > 0)?,
        currency: observed.currency,
        date: observed.dates.effective_date()?,
        far_side: None,
    })
}

/// The questions still waiting, each carrying what may be said to it and which
/// other rows are the same decision.
///
/// **One pass over one list**, because «alike» is a relation among these
/// questions: a second walk that recomputed the subjects would be a second
/// answer to what makes two of them the same, and the two could disagree while
/// both looked right (decision 0029).
///
/// The relation is symmetric and each question is excluded from its own list, so
/// a caller reading two of them reads each naming the other. It is deliberately
/// **not** collapsed into groups here: the assessment's unit is a row, decision
/// 0012 has a question name its row, and publishing groups instead would take
/// away the one identifier the answering call takes.
/// The directory is taken rather than read here, and taken as an argument rather
/// than reached for: the account named beside a question must be the account the
/// row was resolved against, and a second reading of the store could name one
/// account two ways in one response (§3.4). It is the same reading
/// [`answer_accounts`] is folded over, one line below the call.
fn open_questions(
    directory: &AccountDirectory,
    observations: &[ImportObservationView],
    questions: &[ImportQuestionView],
    settlements: &QuestionSettlements,
) -> Vec<OpenQuestion> {
    let open: Vec<&ImportQuestionView> = questions
        .iter()
        // A question this session's own reading has already settled is not
        // open, however the questions table still reads. Two things settle one
        // without a word from the owner and they are one test here rather than
        // two filters, because they are one fact about the row: the other leg's
        // answer already records the movement (`iaam-3qsq`), or a standing rule
        // of his classifies it (`iaam-m2oi`). Publishing either as open is the
        // same defect — the commit refused over a question nothing needed
        // answered, and one row named in `resolved` and here at once. See
        // [`QuestionSettlements`].
        .filter(|question| settlements.awaits_answer(question))
        .collect();
    // The stored rows, parsed once for the whole list. What each question is a
    // decision *about* and what its row printed are two answers over one
    // reading, and the grouping used to make a reading of its own per question —
    // a session of two hundred open rows parsed each of them twice to answer two
    // questions about the same bytes. `subject_of` is still the one place that
    // decides what makes two questions one decision.
    let rows: Vec<Option<ObservedRow>> = open
        .iter()
        .map(|question| observed_row(observations, question.row).ok())
        .collect();
    let subjects: Vec<Option<QuestionSubject>> = open
        .iter()
        .zip(&rows)
        .map(|(question, row)| decision_of_read_row(question, row.as_ref()?))
        .collect();
    open.iter()
        .zip(&subjects)
        .zip(&rows)
        .map(|((question, subject), row)| OpenQuestion {
            row: question.row,
            question: question.id,
            prompt: question.prompt.clone(),
            printed: row.as_ref().map(|row| printed_row(directory, row)),
            alternatives: stored_alternatives(question),
            alike: subject.as_ref().map_or_else(Vec::new, |subject| {
                open.iter()
                    .zip(&subjects)
                    .filter(|(other, of_other)| {
                        other.id != question.id && of_other.as_ref() == Some(subject)
                    })
                    .map(|(other, _)| other.row)
                    .collect()
            }),
            pair: settlements.pair_of(question.row),
        })
        .collect()
}

/// The row behind one open question, in the shape the assessment publishes.
///
/// A copy and never a computation: every field is read off the stored
/// observation as the source stated it, so that the one transition this makes —
/// a minor amount into the decimal string the transport prints — happens at the
/// wire and not here.
///
/// The one thing not read off the observation is the account's title, which no
/// observation carries: it is read out of the directory this plan already
/// loaded, so the name printed beside a question comes from the same reading the
/// row was resolved against. A second read of the store here could name one
/// account two ways in one response, which is `docs/api/conventions.md` §3.4.
fn printed_row(directory: &AccountDirectory, row: &ObservedRow) -> PrintedRow {
    let held = directory.held(row.account);
    PrintedRow {
        account: row.account,
        title: held.map(|account| account.title.clone()),
        institution: held.and_then(|account| account.institution.clone()),
        amount_minor: row.amount_minor,
        currency: row.currency,
        date: row.dates.effective_date(),
        movement: row.movement(),
        counterparty: row.counterparty_name().map(str::to_owned),
        source_category: row.source_category.clone(),
        owner_category: row.owner_category.clone(),
    }
}

/// The owner's accounts an answer to one of these questions may name, once for
/// the whole assessment.
///
/// **Read from the directory the plan already loaded** (`iaam-7iyg`). The
/// planner holds `AccountDirectory`, which is the one reading of the owner's
/// accounts this plan makes and the reading every row of it was resolved
/// against; a second read to reach `AccountView`, which is the shape
/// `answer_account_candidates` takes, would be two readings of one directory in
/// one plan — the thing [`AccountDirectory::from_accounts`] exists to prevent,
/// and it could differ from the first by an account created in between.
///
/// The mapping and the order are that function's: `id` to send back, `title` and
/// `institution` to read by (conventions §3.3), ordered by identifier so the
/// list does not move between two readings of one session. What is deliberately
/// **not** done here is that function's other half — dropping the account the
/// row is on — because this list is not about one row. See
/// [`Interpretation::answer_accounts`]: the exclusion is one comparison against
/// [`PrintedRow::account`], published beside every question.
///
/// Empty where no open question offers an answer that names an account, which
/// is read from the stored alternatives and not from the questions this build
/// would ask.
fn answer_accounts(directory: &AccountDirectory, open: &[OpenQuestion]) -> Vec<AccountCandidate> {
    if !open.iter().any(|question| {
        question
            .alternatives
            .iter()
            .copied()
            .any(AnswerShape::needs_account)
    }) {
        return Vec::new();
    }
    let mut candidates: Vec<AccountCandidate> = directory
        .accounts
        .iter()
        .map(|account| AccountCandidate {
            id: account.id,
            title: account.title.clone(),
            institution: account.institution.clone(),
        })
        .collect();
    candidates.sort_by_key(|candidate| candidate.id.inner());
    candidates
}

/// The sets this session's open rows form, each with what its members agree on
/// and the one answer that settles the whole of it.
///
/// **Folded over the relations the questions already publish, and not over a
/// second reading of them** (`iaam-cixz`). A decision group is
/// `{row} ∪ alike` for a question whose [`OpenQuestion::alike`] is not empty; a
/// movement group is the rows sharing one [`OpenQuestion::pair`]. Recomputing
/// `QuestionSubject` here to find the same sets would be a second answer to
/// «what makes two questions one decision», in a module whose whole argument for
/// [`decision_of_read_row`] being one function is that two spellings of that rule
/// both look right. Read this way, a group and the `alike` list beside it cannot
/// disagree.
///
/// **A set of one is not a group**, which is decision 0033 §2 one surface down:
/// a question already stands alone, and «here is a group of one» would make a
/// caller take it apart to find that out. A pair whose other leg an answer has
/// already settled is exactly that case — the settled leg is not among the open
/// questions at all (decision 0031) — and it is published as one question again.
///
/// **The stored rows are read again, and they are read for one field.** Every
/// other value a group publishes is on [`OpenQuestion::printed`] already; the
/// description is on neither that nor the question, deliberately (decision 0032),
/// and it is the one field that answers what a set of rows was. Reading it here
/// rather than widening [`PrintedRow`] is what keeps it a property of the group:
/// it is read for a set and published only where the set agrees on it. A group
/// none of whose rows this build can read is published not at all, because a
/// group is a claim about what its members have in common and there is nothing to
/// make the claim out of.
///
/// **`may_generalise` is a parameter and not a read** (`iaam-sh6m`). A decision
/// group's sentence has to say what answering it keeps, and that depends on who
/// is answering; the caller holds the principal and hands the one bit down,
/// rather than this fold acquiring an authority of its own.
fn row_groups(
    observations: &[ImportObservationView],
    open: &[OpenQuestion],
    directory: &AccountDirectory,
    may_generalise: bool,
) -> Vec<RowGroup> {
    let mut sets: Vec<(GroupBasis, Vec<u32>)> = Vec::new();
    let mut seen: BTreeSet<Vec<u32>> = BTreeSet::new();
    for question in open {
        if question.alike.is_empty() {
            continue;
        }
        let mut rows = question.alike.clone();
        rows.push(question.row);
        rows.sort_unstable();
        rows.dedup();
        // Every member of one decision group publishes the same set, so the
        // first of them names it and the rest are the same set arriving again.
        if seen.insert(rows.clone()) {
            sets.push((GroupBasis::OneDecision, rows));
        }
    }
    let mut pairs: BTreeMap<Uuid, Vec<u32>> = BTreeMap::new();
    for question in open {
        if let Some(pair) = &question.pair {
            // Keyed by the pair's own identifier and not by the pair: the other
            // row it names differs between the two questions of one pair, which
            // is the point of publishing it, and would put each leg in a group
            // of its own.
            pairs.entry(pair.id).or_default().push(question.row);
        }
    }
    for mut rows in pairs.into_values() {
        // In row order like every other member list here, rather than in the
        // order the questions happened to arrive: a group's members are read
        // beside the rows they name, and two readings of one session must list
        // them the same way.
        rows.sort_unstable();
        if rows.len() > 1 {
            sets.push((GroupBasis::OneMovement, rows));
        }
    }
    let mut groups: Vec<RowGroup> = Vec::new();
    for (basis, rows) in sets {
        let Some(members) = rows
            .iter()
            .map(|row| observed_row(observations, *row).ok())
            .collect::<Option<Vec<ObservedRow>>>()
        else {
            continue;
        };
        let common = shared_row(&members, directory);
        let days = day_span(&members);
        // The invariant `AmountSpan` states: a span over two currencies is a pair
        // of numbers with no unit.
        let amounts = common.currency.and_then(|_| amount_span(&members));
        let question = match basis {
            GroupBasis::OneDecision => {
                // The group's prospect is the weakest of its members'. One
                // answer covers every one of them, so a single member no rule
                // could be built from is a member the answer settles and
                // nothing more — and a sentence promising a standing rule to
                // the set would be false of that one. Where they all ground a
                // rule they ground the same kind of one, because what makes
                // them a group is the question their rows raise.
                let subjects: Vec<ClassificationSubject> =
                    members.iter().map(|member| member.subject(None)).collect();
                let ground = if subjects
                    .iter()
                    .all(|subject| matcher_from(subject).is_some())
                {
                    subjects.first()
                } else {
                    None
                };
                decision_group_question(
                    rows.len(),
                    &common,
                    days.as_ref(),
                    amounts.as_ref(),
                    generalisation_ahead(ground, may_generalise),
                )
            }
            GroupBasis::OneMovement => movement_group_question(&members, directory),
        };
        groups.push(RowGroup {
            basis,
            rows,
            common,
            days,
            amounts,
            settles: match basis {
                GroupBasis::OneDecision => AnswerReach::EveryLikeRowInThisSession,
                GroupBasis::OneMovement => AnswerReach::ThisRow,
            },
            question,
        });
    }
    // Largest first, and by the rows themselves where two are equal — the group
    // worth putting to him first is first, and the order does not move between
    // two readings of one session. Both this and the offers beside it are
    // ordered that way, because a caller reads them together.
    groups.sort_by(|left, right| {
        right
            .rows
            .len()
            .cmp(&left.rows.len())
            .then_with(|| left.rows.cmp(&right.rows))
    });
    groups
}

/// What every member of a set states alike.
///
/// One fold for both groupings, which is what «one shape» costs and what it buys:
/// nothing here knows why these rows are together, so a grouping whose members
/// agree about their account and their party and one whose members agree about
/// neither are described by the same code.
fn shared_row(members: &[ObservedRow], directory: &AccountDirectory) -> SharedRow {
    let Some(first) = members.first() else {
        return SharedRow {
            account: None,
            currency: None,
            movement: None,
            counterparty: None,
            source_category: None,
            description: None,
        };
    };
    let account = members
        .iter()
        .all(|row| row.account == first.account)
        .then(|| group_account(directory, first.account))
        .flatten();
    let currency = members
        .iter()
        .all(|row| row.currency == first.currency)
        .then_some(first.currency);
    let movement = members
        .iter()
        .all(|row| row.movement() == first.movement())
        .then(|| {
            first
                .movement()
                .map_or(SharedMovement::NoneStated, SharedMovement::Stated)
        });
    let counterparty = first.counterparty_name().and_then(|named| {
        members
            .iter()
            .all(|row| row.counterparty_name() == Some(named))
            .then(|| named.to_owned())
    });
    let source_category = first.source_category.as_deref().and_then(|category| {
        members
            .iter()
            .all(|row| row.source_category.as_deref() == Some(category))
            .then(|| category.to_owned())
    });
    let description = first.description.as_deref().and_then(|described| {
        members
            .iter()
            .all(|row| row.description.as_deref() == Some(described))
            .then(|| described.to_owned())
    });
    SharedRow {
        account,
        currency,
        movement,
        counterparty,
        source_category,
        description,
    }
}

/// One of the owner's accounts in the shape a person reads it by.
///
/// `None` for an account this instance's directory does not hold, and the group
/// then names none: a title cannot be invented for it, and an identifier alone is
/// not something anybody can be asked about. The row is still in the group and
/// still publishes its own account.
fn group_account(directory: &AccountDirectory, account: AccountId) -> Option<AccountCandidate> {
    directory
        .accounts
        .iter()
        .find(|held| held.id == account)
        .map(|held| AccountCandidate {
            id: held.id,
            title: held.title.clone(),
            institution: held.institution.clone(),
        })
}

/// The days the members that state one run between.
fn day_span(members: &[ObservedRow]) -> Option<DaySpan> {
    let mut stated = members.iter().filter_map(|row| row.dates.effective_date());
    let first = stated.next()?;
    let (earliest, latest) = stated.fold((first, first), |(earliest, latest), day| {
        (earliest.min(day), latest.max(day))
    });
    Some(DaySpan { earliest, latest })
}

/// The amounts the members run between, signed as the source printed them.
fn amount_span(members: &[ObservedRow]) -> Option<AmountSpan> {
    let first = members.first()?.amount_minor;
    let (smallest_minor, largest_minor) =
        members
            .iter()
            .fold((first, first), |(smallest, largest), row| {
                (
                    smallest.min(row.amount_minor),
                    largest.max(row.amount_minor),
                )
            });
    Some(AmountSpan {
        smallest_minor,
        largest_minor,
    })
}

/// What the source says about the whole group, as a person finds it on the
/// statement in front of him.
///
/// **[`row_mark`] one level up, and it keeps that function's rule.** A day and an
/// amount identify a line well enough to point at; a span of days and a span of
/// amounts identify a set of them, and the description stays out of the sentence
/// for the reason it stays out of `row_mark`'s — it is the source's whole text,
/// of unbounded length, and a sentence carrying it is how a statement's words end
/// up in a queue item, a log line and an agent transcript. Where the group shares
/// one, [`SharedRow::description`] publishes it beside this.
///
/// Only what the members agree on is said. A clause for a field they disagree
/// about would be a claim about the group that is false of some of it, which is
/// the whole failure this shape exists to prevent.
fn group_mark(
    count: usize,
    common: &SharedRow,
    days: Option<&DaySpan>,
    amounts: Option<&AmountSpan>,
) -> String {
    let mut clauses = vec![common.account.as_ref().map_or_else(
        || format!("{count} lines of this import"),
        |account| format!("{count} lines of this import on «{}»", account.title),
    )];
    match common.movement {
        Some(SharedMovement::Stated(Movement::Out)) => {
            clauses.push("all money that left".to_owned());
        }
        Some(SharedMovement::Stated(Movement::In)) => {
            clauses.push("all money that arrived".to_owned());
        }
        Some(SharedMovement::NoneStated) => {
            clauses.push("none of them saying which way the money went".to_owned());
        }
        None => {}
    }
    if let Some(counterparty) = &common.counterparty {
        clauses.push(format!("all naming «{counterparty}» on the other side"));
    }
    if let Some(category) = &common.source_category {
        clauses.push(format!("all filed under «{category}»"));
    }
    if let Some(days) = days {
        clauses.push(if days.earliest == days.latest {
            format!("all dated {}", days.earliest)
        } else {
            format!("dated between {} and {}", days.earliest, days.latest)
        });
    }
    if let (Some(amounts), Some(currency)) = (amounts, common.currency) {
        let code = currency.code();
        let smallest = decimal(PostedMinor::new(amounts.smallest_minor), currency);
        if amounts.smallest_minor == amounts.largest_minor {
            clauses.push(format!("each for {smallest} {code}"));
        } else {
            let largest = decimal(PostedMinor::new(amounts.largest_minor), currency);
            clauses.push(format!("for amounts from {smallest} to {largest} {code}"));
        }
    }
    clauses.join(", ")
}

/// The one sentence to put to a person about a set of rows raising one decision.
///
/// **The discriminating clause is read off what the members share, and that is
/// decision 0032's own rule rather than a shortcut.** The question a row raises is
/// determined by exactly two facts the source stated — which way the money went
/// and whether a party was named — so a third field naming the question would be
/// one fact written twice in a place where the two spellings could drift. The
/// four branches here are `question_for`'s four, reached from the same pair.
///
/// **The words that answer it are not repeated here.** They are on every member,
/// read from what was stored when the question was asked, and `iaam-ulib` is the
/// bead about a question published without them; a group that carried its own
/// copy would be a fifth publisher of one stored list, which is the thing that
/// bead's one reader exists to prevent.
///
/// **The persistence half is not written here either** (`iaam-sh6m`). How many
/// of this session's lines one answer settles is the group's own fact and is
/// stated below. Whether the answer also becomes a standing rule is not: it
/// depends on the row and on who is answering, this sentence used to assert one
/// of the three cases flatly — «no standing decision is kept» — and the queue
/// asserted a different one about the same act. It is now
/// [`GeneralisationProspect`], derived once and spoken here in the second
/// person.
fn decision_group_question(
    count: usize,
    common: &SharedRow,
    days: Option<&DaySpan>,
    amounts: Option<&AmountSpan>,
    prospect: GeneralisationProspect,
) -> OwnerQuestion {
    let mark = group_mark(count, common, days, amounts);
    let ask = match (common.movement, common.counterparty.as_deref()) {
        (Some(SharedMovement::Stated(_)), Some(named)) => format!(
            "{mark}. Is «{named}» one of your own accounts — and if so which one — or was this \
             money moving between you and somebody who is not you?"
        ),
        (Some(SharedMovement::Stated(Movement::Out)), None) => format!(
            "{mark}. Your statement named nobody on the other side of any of them. Were these \
             charges the institution made, or money you paid to somebody?"
        ),
        (Some(SharedMovement::Stated(Movement::In)), None) => format!(
            "{mark}. Your statement named nobody on the other side of any of them. Was this \
             money your capital earned, money somebody returned on something you had paid for, \
             or money arriving from outside?"
        ),
        (Some(SharedMovement::NoneStated), _) => format!(
            "{mark}. Your statement did not say which way any of them ran. Which way did this \
             money go, and what was it?"
        ),
        // Unreachable for a group these rows raise one decision about, and
        // written rather than asserted: a sentence is what this function owes its
        // reader, and a panic here would take the whole assessment down over a
        // clause.
        (None, _) => format!("{mark}. What were they?"),
    };
    OwnerQuestion {
        ask,
        consequence: format!(
            "One answer here decides all {count} of these lines together instead of one at a \
             time. {kept} Your statement says the same thing about every one of them, but only \
             you know whether they were the same thing — answered as a group, a line that was \
             something else is decided wrongly along with the rest, and the way to keep it out \
             is to answer that one on its own first. Which figure of your money-flow report each \
             line moves depends on the word you choose, and every one of these lines is \
             published with the words that answer it and what each of them decides. Nothing is \
             written until you commit this import.",
            kept = prospect.addressed(),
        ),
    }
}

/// The one sentence to put to a person about two rows that look like one
/// movement.
///
/// **A group whose members agree about almost nothing, and that is what makes it
/// one.** A pair is a departure on one account and the arrival on the other, so
/// the account and the direction — the two things a decision group agrees on —
/// are exactly what these two differ in, and the sentence is therefore built from
/// the members rather than from what they share. It is the same shape carrying
/// the opposite content, which is the argument for there being one shape.
///
/// **It asks and it does not conclude** (decision 0031). Two unrelated payments of
/// one sum on one day have this shape, and the answer that says so is any answer
/// that does not name the other row's account.
fn movement_group_question(members: &[ObservedRow], directory: &AccountDirectory) -> OwnerQuestion {
    let legs: Vec<String> = members
        .iter()
        .map(|row| {
            let amount = decimal(PostedMinor::new(row.amount_minor), row.currency);
            let code = row.currency.code();
            let title = directory.title(row.account);
            let side = match row.movement() {
                Some(Movement::Out) => format!("{amount} {code} left «{title}»"),
                Some(Movement::In) => format!("{amount} {code} arrived on «{title}»"),
                None => format!("{amount} {code} on «{title}»"),
            };
            row.dates.effective_date().map_or_else(
                || format!("{side}, which the source left undated"),
                |date| format!("{side} on {date}"),
            )
        })
        .collect();
    let printed = legs.join(", and ");
    OwnerQuestion {
        ask: format!(
            "Two lines of this import look like one movement your statement printed twice: \
             {printed}. Was this your own money moving between two accounts of yours?"
        ),
        consequence: "If it was, answering either of the two as a movement to or from the other \
                      account records one movement and settles both, and the second line is left \
                      with nothing of its own to record — so the same money is not counted twice \
                      in your money-flow report. If it was not, two unrelated lines of one sum on \
                      one day look exactly like this: any other answer leaves them as two lines \
                      with two questions, and nothing about this pairing is kept. Nothing is \
                      written until you commit this import."
            .to_owned(),
    }
}

/// The standing decisions this session's unanswered rows offer, one per word the
/// source filed them under.
///
/// **Why the source's category and not the counterparty [`matcher_for`] would
/// pick** (`iaam-qn6d`). Both are conditions the owner could adopt, and they
/// differ in how many decisions they cost him. A first import of one card
/// statement raises a question per row, most of them repeats of a handful of
/// merchants and all of them merchants; one decision per merchant is hundreds of
/// decisions, and the merchants are the part that changes next month. The words
/// the institution files its own rows under are a closed list it controls, they
/// are printed on every row, and there are a dozen of them. `matcher_for` is
/// right to prefer the counterparty for the rule minted from **one** answer —
/// that rule is a claim about the party the owner just decided about — and this
/// is the other question: which single condition would settle the most of what
/// is still open.
///
/// **Only rows with an open question.** A row already settled by a rule of his is
/// not evidence that he wants another one, and counting it would make an offer
/// grow every month while settling nothing new. A document whose profile
/// transcribes no category offers nothing here, and the list is empty — which is
/// the truthful answer and not a failure: decision 0019 §6 has a profile name the
/// column and stop, so a source that prints no such column leaves nothing to
/// offer.
///
/// **Offered only where the word covers one thing** (`iaam-xchm`). A word an
/// institution files by is its vocabulary and not a description of the owner's
/// money: a transfer word covers every transfer, inward and outward, to his own
/// accounts and to other people's, and one rule on such a word is a confident
/// recommendation to make one wrong standing decision instead of many right
/// ones. Where the group is not one thing this offers nothing and says so — see
/// [`WithheldOffer`], which also argues why the group is not narrowed instead.
///
/// Ordered by how many open rows each would settle, most first, and by the word
/// itself where two are equal — so the offer worth reading first is first, and
/// the list does not reorder itself between two readings of one session. Both
/// lists are ordered that way, because a caller reads them together.
fn offers(observations: &[ImportObservationView], open: &[OpenQuestion]) -> Offers {
    let mut by_category: BTreeMap<(FiledBy, String), Vec<(u32, RowShapeKey)>> = BTreeMap::new();
    for question in open {
        let Ok(row) = observed_row(observations, question.row) else {
            continue;
        };
        // Both words, and a row carrying both is counted under both. They are
        // two conditions over two vocabularies, either of which he may prefer,
        // and each offer covers exactly the open rows its own condition matches
        // — so two offers overlapping is two ways to settle one set and not a
        // claim made twice.
        //
        // **Adopting both is not free where they overlap** (`iaam-y5ww`).
        // `classify` takes `max_by_key(version)` among matching rules and a
        // version is per-owner increasing, so on a row both conditions match the
        // rule he adopted **later** wins — silently, whichever of the two he
        // meant to hold there. That costs nothing only while he answers the two
        // the same way. A caller publishing both therefore owes the reader that
        // sentence rather than presenting them as independent, and a caller that
        // wants the overlap settled by a condition it can name adopts one of
        // them and leaves the other.
        for (filed_by, word) in [
            (FiledBy::Source, row.source_category.clone()),
            (FiledBy::Owner, row.owner_category.clone()),
        ] {
            let Some(word) = word else {
                continue;
            };
            by_category
                .entry((filed_by, word))
                .or_default()
                .push((question.row, shape_key(&row)));
        }
    }
    let mut offered: Vec<OfferedRule> = Vec::new();
    let mut withheld: Vec<WithheldOffer> = Vec::new();
    for ((filed_by, category), mut rows) in by_category {
        rows.sort_unstable();
        let covers: Vec<u32> = rows.iter().map(|(row, _)| *row).collect();
        let mut shapes = shapes_of(&rows);
        // Most-covering first inside the group too, and by the shape itself
        // where two are equal: a caller showing a mixed word shows the largest
        // share first, and the order does not move between two readings.
        shapes.sort_by(|left, right| {
            right
                .rows
                .len()
                .cmp(&left.rows.len())
                .then_with(|| shape_key_of(left).cmp(&shape_key_of(right)))
        });
        // One shape or many, and the branch is the whole bead. `OfferedRule`
        // holds one `RowShape` and cannot hold two, so a group that is not one
        // thing is not representable as an offer at all.
        let sole = if shapes.len() == 1 {
            shapes.pop()
        } else {
            None
        };
        if let Some(contains) = sole {
            // The word goes in the field that asks about the party who filed
            // it. A condition carrying the owner's own word in the source's
            // field would fire on rows the institution filed under it and he
            // did not, which is the defect decision 0020 §2 took two words out
            // of one slot to end.
            let (source_category, owner_category) = match filed_by {
                FiledBy::Source => (Some(category.clone()), None),
                FiledBy::Owner => (None, Some(category.clone())),
            };
            offered.push(OfferedRule {
                question: offered_rule_question(filed_by, &category, covers.len(), &contains),
                matcher: RuleMatcher {
                    counterparty_account: None,
                    description_contains: None,
                    kind: None,
                    source_category,
                    owner_category,
                    source_code: None,
                },
                covers,
                contains,
            });
        } else {
            withheld.push(WithheldOffer {
                reason: withheld_offer_reason(filed_by, &category, &shapes),
                filed_by,
                filed_under: category,
                covers,
                contains: shapes,
            });
        }
    }
    offered.sort_by(|left, right| {
        right
            .covers
            .len()
            .cmp(&left.covers.len())
            .then_with(|| offered_ground(left).cmp(&offered_ground(right)))
    });
    withheld.sort_by(|left, right| {
        right.covers.len().cmp(&left.covers.len()).then_with(|| {
            (left.filed_by, &left.filed_under).cmp(&(right.filed_by, &right.filed_under))
        })
    });
    Offers { offered, withheld }
}

/// The ground an offer is keyed by, read off the condition it publishes.
///
/// One reader, so the order two offers are listed in and the field a caller
/// reads the word out of cannot come apart. [`OfferedRule`] keeps no ground of
/// its own for exactly this reason: the matcher already says which word it asks
/// about, and a second statement of it is a second answer that can disagree.
fn offered_ground(offer: &OfferedRule) -> (FiledBy, Option<&String>) {
    offer.matcher.owner_category.as_ref().map_or(
        (FiledBy::Source, offer.matcher.source_category.as_ref()),
        |word| (FiledBy::Owner, Some(word)),
    )
}

/// What one word the source files by turned out to be worth, both ways.
///
/// **One function and two lists, because they are one decision.** Whether a word
/// is offered as a rule and whether it is published as a word no rule fits are
/// the same judgement read twice, and two functions computing it would be two
/// answers to «is this group one thing» that can differ while both look right.
struct Offers {
    offered: Vec<OfferedRule>,
    withheld: Vec<WithheldOffer>,
}

/// What a row's shape is, in a form that can be compared and ordered.
///
/// The pair [`RowShape`] publishes, without the rows — so that grouping is a map
/// keyed by it and the shape's own row list is built from the group rather than
/// merged into by hand.
type RowShapeKey = (Option<Movement>, bool);

fn shape_key(row: &ObservedRow) -> RowShapeKey {
    (row.movement(), row.counterparty_name().is_some())
}

fn shape_key_of(shape: &RowShape) -> RowShapeKey {
    (shape.movement, shape.counterparty_named)
}

/// The shapes among one word's open rows, each carrying its own rows in order.
fn shapes_of(rows: &[(u32, RowShapeKey)]) -> Vec<RowShape> {
    let mut by_shape: BTreeMap<RowShapeKey, Vec<u32>> = BTreeMap::new();
    for (row, key) in rows {
        by_shape.entry(*key).or_default().push(*row);
    }
    by_shape
        .into_iter()
        .map(|((movement, counterparty_named), rows)| RowShape {
            movement,
            counterparty_named,
            rows,
        })
        .collect()
}

/// Why no rule is offered on a word, in one sentence for the owner.
///
/// **A statement and not a question**, so it has one part and not two: nothing
/// is being asked and nothing turns on an answer he is not being invited to
/// give. Decision 0027's other two obligations still bind, because a caller that
/// shows this shows it to him — no field name, no word that exists only because
/// of how this is built, and the word he can see on his own statement quoted so
/// he knows which one is meant.
///
/// It says what the group holds in the terms the group is divided by, and stops:
/// the rows themselves are published beside it, and a sentence that listed them
/// would be a structure encoded as prose, which `docs/api/conventions.md` §5
/// refuses and which is the defect one bead over (`iaam-pm4w`).
fn withheld_offer_reason(filed_by: FiledBy, category: &str, contains: &[RowShape]) -> String {
    let shapes = contains.len();
    let covers: usize = contains.iter().map(|shape| shape.rows.len()).sum();
    // Whose filing it was, said plainly. The word is a decision one party took,
    // and telling him his statement filed rows he filed himself would hand his
    // own decision back to him as the institution's.
    let files = match filed_by {
        FiledBy::Source => "Your statement files",
        FiledBy::Owner => "You file",
    };
    format!(
        "{files} {covers} of the lines still waiting on you under «{category}», and \
         they are not all the same thing: they fall into {shapes} groups by which way the money \
         went and whether the statement named anyone on the other side. One answer for the whole \
         word would be wrong for some of them and you would not be asked about those again, so \
         nothing is offered on it — the lines are put to you in their groups instead."
    )
}

/// The offer put to the owner, in his words.
///
/// **Decision 0027's register, and the third obligation is the one this had to
/// earn.** What is asked is what a line the institution files under one of its
/// own words *is*; what turns on it is that one answer stands for every such
/// line, this month's and every later one, which is the whole reason the offer
/// exists and is also the whole of its risk. Saying only «this saves you
/// questions» would be the shape of consequence the decision refuses — true of
/// the offer rather than of his choice between one answer and another.
///
/// The word is quoted and nothing else of the row is: it is a value out of a
/// vocabulary the institution controls, printed identically on every row it
/// covers, and it is the thing he is being asked about. No counterparty, no
/// amount and no description reaches this sentence — [`row_mark`] argues that at
/// length for the per-row prompt, and an offer covering many rows has no single
/// row to point at anyway.
///
/// # The sentence had to become true, and three parts of it were not
///
/// **«Settles all N of them at once» was false, and `iaam-m2oi` is that it was.**
/// Adopting the offer wrote the rule and recomputed the journal and touched no
/// import question, so the rows were re-read as facts while their stored
/// questions stayed open: the assessment named one row as resolved and as still
/// waiting in the same response, and the commit went on refusing over questions
/// nothing needed answered. The wall he was promised relief from was still
/// there, and he had made a standing decision about every row like these on the
/// strength of a sentence that was false — which is worse than never offering
/// the rule. [`QuestionSettlements`] is what makes the clause true; this comment
/// exists so that a later change to either is checked against the other.
///
/// **«The same institution» was false too.** Decision 0026 §4 refuses to scope a
/// category condition to a source and says why at length: every handle this
/// journal holds scopes the wrong thing, and `SourceId` is an account and a
/// channel rather than an institution. So the rule fires on any row any source
/// files under exactly that word, and a sentence that said «the same
/// institution» understated the reach in the direction that costs him — it
/// described a narrower decision than the one he was making. The clause now says
/// what the rule does.
///
/// **An answer that does not fit settles nothing, and he is told so.** The rows
/// this covers are one [`RowShape`], so an outcome either fits all of them or
/// none: `ObservedRow::resolve` refuses a fee that arrived and income that left,
/// and a row that refuses is a row still waiting. That is the honest shape of
/// the risk — a wrong answer here costs him the offer and not the rows — and it
/// is stated as a property of the answer rather than as a table of which
/// outcomes suit which lines, which would be a structure encoded as prose
/// (`docs/api/conventions.md` §5).
///
/// The direction clause is the one exception, and it is one fact rather than a
/// mapping: where the source stated no direction, four of the five outcomes
/// carry one themselves and `Classification::ExternalFlow` carries none, so a
/// rule stating it leaves every row of the group at
/// `Question::UnresolvedDirection`. Naming it is the difference between an offer
/// that keeps its promise and one that keeps four fifths of it in silence.
fn offered_rule_question(
    filed_by: FiledBy,
    category: &str,
    covers: usize,
    contains: &RowShape,
) -> OwnerQuestion {
    // The one thing about these rows the promise depends on. Every other
    // attribute they share is the same for all of them by construction, and this
    // one decides whether an answer finishes them or only classifies them.
    let undecided_direction = contains.movement.is_none();
    let direction = if undecided_direction {
        " These lines do not say which way the money went. Four of the five answers decide that \
         by themselves; «money you spent» does not, so it would record what the lines were and \
         leave them still waiting to be told which way."
    } else {
        ""
    };
    // Whose word it is, and it changes both halves. A word of the owner's own is
    // a decision he already took, so the question is not «what did your bank
    // mean by this» but «what you call this, what is it here» — and the
    // sentence must not tell him his statement filed what he filed himself.
    let (files, later) = match filed_by {
        FiledBy::Source => (
            "Your statement files",
            "any later line filed under exactly «{category}», by whoever sends it, is settled the \
             same way",
        ),
        FiledBy::Owner => (
            "You file",
            "any later line you file under exactly «{category}», at whichever institution, is \
             settled the same way",
        ),
    };
    let later = later.replace("{category}", category);
    OwnerQuestion {
        ask: format!(
            "{files} {covers} of the lines still waiting on you under «{category}». \
             What is a line filed that way — money you spent, a charge the institution made, \
             money someone gave back, something your money earned, or money moving between \
             accounts of your own?"
        ),
        consequence: format!(
            "One answer here settles all {covers} of them at once: they stop waiting on you and \
             are recorded as what you say they are, without being put to you one at a time. It \
             does not stop at this statement, and it does not stop at this institution — {later} \
             without being put to you again. That is the risk as well as the saving: a \
             word that also covers lines you would have decided differently files those wrongly, \
             and you will not be asked about them. Withdrawing the decision afterwards puts back \
             what it decided — lines still waiting in an import go back to being put to you, and \
             lines already recorded come back as a correction for you to approve — so being \
             wrong costs a correction rather than a line nobody ever looks at again. An answer \
             that does not fit these lines settles none of them and takes nothing away: they \
             stay exactly as they are.{direction}"
        ),
    }
}

/// The rule condition a row can be recognised by later, if any.
///
/// `None` where the row offers nothing to match on. A matcher that asks nothing
/// matches nothing by construction, and writing one would record a decision that
/// never applies while looking like one that does.
///
/// **One field, not every field the row offers (`iaam-g7yc`).** This used to
/// fill every field at once, and [`RuleMatcher::matches`] joins present fields
/// with «and»: the rule then demanded the counterparty exactly *and* the
/// source's word exactly *and* the description as a substring — and the
/// description it was given is the row's **whole** description, so as a
/// substring test it recognises essentially that row and nothing else. The rule
/// was correct and empty: a standing decision that settles one line the owner
/// had already settled by hand. `docs/import-boundary.md` §7 recorded the choice
/// as open; decision 0008 takes it, and this is what it decided.
///
/// **Why one field is the right number, and where that is read from.** Every
/// classification rule written by hand anywhere in this workspace asks about
/// exactly one thing — a counterparty, a source word, or a fragment of a
/// description — and never two. Those are the rules people wrote when they meant
/// something, and a proposal the owner is asked to adopt should have the shape
/// of a rule somebody would write.
///
/// **Which one, and why in this order.** Decision 0008 fixed the first three of
/// these; the third of them is now the fourth, and the reason for the insertion
/// is under step 3.
///
/// 1. The **counterparty** where the row names one. The classification is a
///    claim about who the money moved with — «anything with this counterparty is
///    a fee», «this name is my own account at another bank» — and the printed
///    name is the field that identifies him. It is matched exactly, so it is
///    also the narrowest of the three that still generalises.
/// 2. Otherwise the **source's own word** for the operation. It is matched
///    exactly against a vocabulary one source controls, and it is what a row
///    with no counterparty has instead of one: the bank's word for a movement
///    internal to itself is the whole evidence such a row carries.
/// 3. Otherwise the **category the source filed the row under**, added by
///    `iaam-93lz` and matched exactly against a vocabulary one source controls,
///    exactly as the word above it is. It comes third rather than second
///    because it says what the movement was *for* and the question being
///    generalised is what the movement *was*; it comes before the description
///    because it is a value out of a closed vocabulary and a description is
///    prose. The first profile that ships prints no operation-type word at all,
///    so for its rows this is what step 2 would have been.
/// 4. Otherwise the **description**, which is last because it is the only one
///    matched as a substring and the only one taken whole. A whole description
///    is close to unique to its row, so this is barely a generalisation — but it
///    is the difference between a rule and none, and the row would otherwise
///    read as [`Generalisation::Impossible`], which claims that no rule can be
///    built from it under any token. That would be false.
///
/// **The trade-off is real and is not resolved in this direction by accident.**
/// A matcher on one field settles more rows than a matcher on several, and one
/// of them can be settled wrongly: a source word like the one a bank prints on
/// every transfer would carry a classification onto rows that do not deserve it.
/// Two things bound that. The proposal is only ever *offered* — it is published
/// as the body of `POST /v1/classification-rules` for the owner to read, narrow
/// and send, and a rule he adopts is one he can retire, which replans the
/// history it classified. And the rows this is computed for are rows the
/// classifier could **not** settle, so the field it proposes is one that had no
/// standing rule on it.
fn matcher_for(row: &ObservedRow) -> Option<RuleMatcher> {
    // Through the subject, because the subject is the shape the whole field
    // policy below is written about and it is what `generalisation_ahead` holds.
    // `ObservedRow::subject(None)` is the reading a rule is tested under — the
    // counterparty stays the name the source printed — which is the same reading
    // `subject_of` publishes and the same one this function has always taken.
    matcher_from(&row.subject(None))
}

/// The same policy, asked of the row as the classifier sees it.
///
/// Split out so that «can a rule be built from this row at all» has one answer.
/// [`generalisation_ahead`] needs exactly that question and holds a
/// [`ClassificationSubject`] rather than an [`ObservedRow`]; a predicate of its
/// own beside this would be a second statement of which fields ground a rule,
/// and the two would drift the first time a fifth field is admitted.
fn matcher_from(subject: &ClassificationSubject) -> Option<RuleMatcher> {
    let matcher = if let Counterparty::Named(counterparty) = &subject.counterparty {
        RuleMatcher {
            counterparty_account: Some(counterparty.clone()),
            description_contains: None,
            kind: None,
            source_category: None,
            owner_category: None,
            source_code: None,
        }
    } else if let Some(kind) = subject.source_kind.clone() {
        RuleMatcher {
            counterparty_account: None,
            description_contains: None,
            kind: Some(kind),
            source_category: None,
            owner_category: None,
            source_code: None,
        }
    } else if let Some(category) = subject.source_category.clone() {
        RuleMatcher {
            counterparty_account: None,
            description_contains: None,
            kind: None,
            source_category: Some(category),
            owner_category: None,
            source_code: None,
        }
    } else {
        RuleMatcher {
            counterparty_account: None,
            description_contains: subject.description.clone(),
            kind: None,
            source_category: None,
            owner_category: None,
            source_code: None,
        }
    };
    // Still the last word, and deliberately not replaced by one more branch
    // returning `None`: «a matcher that asks nothing is no matcher» is one rule,
    // and it is stated once whatever the field policy above becomes. It is what
    // answers for the row that prints none of the four.
    (!matcher.asks_nothing()).then_some(matcher)
}

/// The observed row one question is about.
fn observed_row(observations: &[ImportObservationView], row: u32) -> Result<ObservedRow, AppError> {
    let observation = observations
        .iter()
        .find(|candidate| candidate.row == row)
        .ok_or(AppError::NotFound {
            what: "an import observation",
            id: row.to_string(),
        })?;
    match parse_intake(&observation.payload)? {
        Intake::Observed { row, .. } => Ok(*row),
        Intake::Concluded { .. } => Err(AppError::Invalid {
            field: "question".to_owned(),
            expected: "a question about a row whose source stated no conclusion".to_owned(),
            actual: "a row the caller concluded".to_owned(),
        }),
    }
}

/// What one stored row settles as.
///
/// Three outcomes and not two, which is `iaam-tb5o`: a fact, no fact and a
/// reason, or a rejection. A row that deliberately produces nothing used to
/// have to borrow the rejection, and a rejection retains the row and refuses
/// nothing else — so the one disposition the importer could establish on its
/// own looked exactly like a row it had failed to read.
fn resolution_of(
    observation: &ImportObservationView,
    resolver: &Resolver,
) -> Result<RowResolution, Rejection> {
    let intake = parse_intake(&observation.payload).map_err(|error| Rejection {
        field: "observation".to_owned(),
        expected: "a row this build can read".to_owned(),
        actual: error.to_string(),
    })?;
    let row = match intake {
        // A caller that concluded has said there is a fact, so there is one to
        // write or a rejection to report. Nothing here second-guesses it into
        // producing nothing.
        Intake::Concluded { operation } => {
            return Ok(RowResolution::Fact {
                operation,
                basis: FactBasis::Concluded,
            });
        }
        Intake::Observed { row, .. } => *row,
    };
    if let Some(answer) = &observation.answer {
        let answer: Answer = serde_json::from_str(answer).map_err(|error| Rejection {
            field: "answer".to_owned(),
            expected: "an answer this build can read".to_owned(),
            actual: error.to_string(),
        })?;
        return row
            .resolve_with(answer)
            .map(|operation| RowResolution::Fact {
                operation: Box::new(operation),
                basis: FactBasis::Answered,
            });
    }
    match resolver.assess(&row) {
        Assessment::Settled {
            classification,
            movement,
            basis,
        } => row
            .resolve(classification, movement)
            .map(|operation| RowResolution::Fact {
                operation: Box::new(operation),
                basis: FactBasis::of(&basis),
            }),
        Assessment::NoFact { reason } => Ok(RowResolution::NoFact(reason)),
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
///
/// **Public since `iaam-sh6m`,** because the queue must say what a caller's
/// answer will keep and the authority is the caller's fact to supply: the route
/// asks this one question of the principal it already holds and hands the answer
/// to `frontier` as a `bool`. Exporting the predicate rather than letting a
/// second surface read `may_administer` itself is what keeps the gate stated
/// once.
#[must_use]
pub fn may_generalise(principal: &Principal) -> bool {
    principal.scope.may_administer()
}

/// The standing decision one answer would keep, as its condition and its
/// outcome.
///
/// The pair [`Generalisation::Available`] already publishes after the fact,
/// under a name that says the tense: this is the same object asked about
/// **before** the answer is given, so nothing here has been written and nothing
/// has been offered for adoption yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedRule {
    /// The condition, in the shape `POST /v1/classification-rules` takes.
    pub matcher: RuleMatcher,
    /// What the rows it matches would be settled as.
    pub outcome: Classification,
}

/// What answering one question this way would do to the owner's standing
/// decisions, before it is answered.
///
/// **Four states, and they are [`Generalisation`]'s four minus the one that
/// cannot be true yet.** `recorded` is what a written rule reports afterwards
/// and has no forward tense; the other three are here, with `available` split
/// in two — because before the act the difference between «this call writes it»
/// and «it is published for you to adopt» is a fact the caller can act on, and
/// after the act only the second is ever reported.
///
/// **Why the two absences stay apart.** [`Self::NotFromThisAnswer`] is a claim
/// about the **answer** — one of the eight is not a claim about every row like
/// this one (`AnswerShape::generalises`) — and [`Self::NotFromThisRow`] is a
/// claim about the **row**, that it prints nothing a condition could ask about.
/// Folded together they would tell the owner that no answer of his could ever
/// keep a standing decision about these lines, which is false of the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WouldStand {
    /// Answering writes it, because the answerer may generalise.
    Written(ProposedRule),
    /// Answering writes nothing and publishes it, because only the owner may
    /// make a standing decision (`iaam-hnod`). One call of his own makes it
    /// stand, and this forecast does not make it.
    ForHisAdoption(ProposedRule),
    /// This answer is not one to generalise, so no standing decision comes of
    /// it under any token (`AnswerShape::generalises`).
    NotFromThisAnswer,
    /// This row grounds no condition, so no standing decision comes of it under
    /// any token: a condition that asks about nothing matches nothing.
    NotFromThisRow,
}

impl WouldStand {
    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Written(_) => "written",
            Self::ForHisAdoption(_) => "for_his_adoption",
            Self::NotFromThisAnswer => "not_from_this_answer",
            Self::NotFromThisRow => "not_from_this_row",
        }
    }

    /// The standing decision itself, where there would be one.
    #[must_use]
    pub const fn proposed(&self) -> Option<&ProposedRule> {
        match self {
            Self::Written(rule) | Self::ForHisAdoption(rule) => Some(rule),
            Self::NotFromThisAnswer | Self::NotFromThisRow => None,
        }
    }
}

/// One line of this import the condition reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachedRow {
    /// The line's position among what this import took, in submission order.
    /// Not the document's line, which is `locator` (decision 0035 §4).
    pub row: u32,
    /// What the source printed on it, in the same shape a question publishes.
    pub printed: PrintedRow,
    /// Whether this import is still waiting on an answer for it.
    ///
    /// Read from the same reading `answer_question` reads its own reach from,
    /// so «what is still waiting» is answered once. A line that raised no
    /// question, and a line something already settled, both answer `false` —
    /// which is why it is not read alone: `false` here is three different
    /// situations, and [`Self::now`] is the field that says which
    /// (`iaam-r0qk`).
    pub awaiting_answer: bool,
    /// What settles this line today, where anything does.
    ///
    /// [`ReachedFact::now`]'s counterpart for a line, and it exists for the
    /// same reason: what a decision would do to something is only readable
    /// beside what is true of it now. `None` is a line nothing settles — his to
    /// answer, and the state the forecast's own question is asked from.
    pub now: Option<QuestionSettlement>,
}

impl ReachedRow {
    /// Whether this line is settled whatever this decision does
    /// (`iaam-r0qk`).
    ///
    /// **Not a second model of what wins, but a reading of the one order the
    /// commit already runs in.** Two things settle a line before any standing
    /// decision of his is consulted, and a decision made now displaces
    /// neither: his own answer, which `resolution_of` takes before it reaches
    /// the classifier at all, and his account directory recognising the far
    /// side, which `classify` answers before it looks at his decisions. A line
    /// that settles into no fact is not settled by a decision either.
    ///
    /// The two that a decision made now **does** displace are the other side of
    /// the same order: another standing decision of his, because a new one is
    /// written above the highest version he holds and the highest wins, and the
    /// source's own assertion about the far side, which `classify` consults
    /// after his decisions and not before.
    #[must_use]
    pub const fn settled_regardless(&self) -> bool {
        match &self.now {
            None => false,
            Some(QuestionSettlement::NoFact { .. }) => true,
            Some(QuestionSettlement::Fact { basis }) => match basis {
                FactBasis::Answered | FactBasis::Directory | FactBasis::Concluded => true,
                FactBasis::Rule { .. } | FactBasis::SourceAsserted => false,
            },
        }
    }
}

/// One movement already recorded that the condition reaches.
///
/// **What it is, and not what it would become.** Which of the owner's standing
/// decisions wins over a fact is settled by the version the store assigns, and
/// modelling that here would be a second model of the store's own behaviour —
/// which `refuse_unreadable_rules` argues against at the one place a plan is
/// actually computed. So this says the condition reaches the movement and says
/// what the journal records it as today, and the two together are what let the
/// owner find the one that was something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachedFact {
    /// The fact, as a correction addresses it.
    pub event: EventId,
    /// The account it is recorded on.
    pub account: AccountId,
    /// What the owner calls that account, where his directory holds it
    /// (decision 0035 §1). Never the identifier rendered as a name.
    pub title: Option<String>,
    /// The day it is effective on, where it has one.
    pub date: Option<time::Date>,
    /// The amount as the journal holds it, with its sign.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    /// What the journal records it as today, in the words a standing decision
    /// is written in. `None` for a fact no standing decision classifies.
    pub now: Option<ClassifiedAs>,
}

/// Something the condition could not be tested against, and why.
///
/// **Published rather than omitted, and that is the whole of this type.** A
/// forecast that dropped these would read as «nothing else is affected», which
/// is the one false thing it must not say — and it would say it silently, in
/// the response the owner is about to answer from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undecided {
    /// A line of this import whose stored text this build cannot read, so
    /// nothing can be tested against it.
    UnreadableRow { row: u32 },
    /// A movement recorded before the journal kept the source's two words
    /// apart (decision 0020 §3), against a condition that asks about one of
    /// them.
    ///
    /// The word is not in the field the condition asks about and may be sitting
    /// in the other one, so «it does not match» would be a false negative and
    /// «it matches» would be a guess.
    FactWithoutTheWord {
        event: EventId,
        account: AccountId,
        title: Option<String>,
        date: Option<time::Date>,
    },
    /// Everything already recorded, because the whole of it could not be folded
    /// into what is currently in force.
    ///
    /// A correction reverses or replaces a fact, so what a condition would reach
    /// is the set that survives those — which is what the recomputation reads,
    /// and which a journal with a dangling or doubled correction has no answer
    /// for. **Reported and not raised**: a fold that fails is a fact about the
    /// journal and not about the answer he is about to give, and refusing the
    /// whole forecast over it would take the half that works away with it.
    RecordedMovementsWouldNotFold,
}

impl Undecided {
    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnreadableRow { .. } => "unreadable_row",
            Self::FactWithoutTheWord { .. } => "fact_without_the_word",
            Self::RecordedMovementsWouldNotFold => "recorded_movements_would_not_fold",
        }
    }

    /// Why it could not be judged, in the register the owner reads
    /// (decision 0035 §3).
    #[must_use]
    pub const fn why(&self) -> &'static str {
        match self {
            Self::UnreadableRow { .. } => {
                "This line of your statement cannot be read here at all, so nothing can be said \
                 about whether this standing decision would cover it."
            }
            Self::FactWithoutTheWord { .. } => {
                "This movement was recorded before what your institution called an operation and \
                 what it filed the operation under were kept in separate places, so the word this \
                 standing decision asks about cannot be told from the other one. It is neither \
                 included nor excluded."
            }
            Self::RecordedMovementsWouldNotFold => {
                "What you have already recorded could not be read here as a whole, because \
                 something you put right earlier no longer lines up with what it was putting \
                 right. Nothing of it was judged, and none of it is included or excluded."
            }
        }
    }
}

/// What the standing decision one answer would keep would settle, before it
/// stands.
///
/// **This is `preview_category_rule`'s promise for the other kind of standing
/// decision, and deliberately the same promise**: read-only, writing nothing and
/// standing nothing, saying what the condition would move rather than moving it.
/// The owner asked it in his own words — a decision was made from one answer, it
/// covered a group automatically, and one line of the group is wrong — and the
/// two halves after the fact already exist. This is the half before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerRuleForecast {
    /// What would stand, and whose act would make it stand.
    pub stands: WouldStand,
    /// The lines of this import the condition reaches, in submission order,
    /// including the line asked about.
    ///
    /// Empty where [`Self::stands`] says no standing decision comes of this,
    /// and that state is what says why — never an empty list on its own.
    pub in_this_import: Vec<ReachedRow>,
    /// The movements already recorded that the condition reaches, in the order
    /// the journal holds them.
    pub already_recorded: Vec<ReachedFact>,
    /// What could not be judged either way, each saying why.
    pub undecided: Vec<Undecided>,
    /// The whole of the above in one statement, in the owner's register
    /// (decision 0035).
    pub notice: String,
}

/// What answering this way would keep, asked of the answer before the row.
///
/// The order is [`generalisation_of`]'s and for its reason: whether an answer is
/// one to generalise is a property of the word he said and of nothing else, so a
/// row that grounds no condition does not turn the truthful «this answer keeps
/// none» into the false «this line grounds none».
fn would_stand(observed: &ObservedRow, answer: Answer, may_generalise: bool) -> WouldStand {
    if !answer.shape().generalises() {
        return WouldStand::NotFromThisAnswer;
    }
    let Some(matcher) = matcher_for(observed) else {
        return WouldStand::NotFromThisRow;
    };
    let proposed = ProposedRule {
        matcher,
        outcome: answer.classification(),
    };
    if may_generalise {
        WouldStand::Written(proposed)
    } else {
        WouldStand::ForHisAdoption(proposed)
    }
}

/// One session as a forecast reads it: its lines, the questions they raised,
/// and what the reading has already settled.
///
/// The three travel together because a forecast asks one thing of them — which
/// lines a condition covers and which of those this import is still waiting on —
/// and passing them separately is three arguments a caller can pair wrongly,
/// with the settlements taken from one reading and the questions from another.
struct SessionAsRead<'a> {
    observations: &'a [ImportObservationView],
    questions: &'a [ImportQuestionView],
    settlements: &'a QuestionSettlements,
}

/// The lines of one import a condition reaches, and the ones it cannot be
/// tested against.
fn reach_over_session(
    session: &SessionAsRead<'_>,
    matcher: &RuleMatcher,
    directory: &AccountDirectory,
) -> (Vec<ReachedRow>, Vec<Undecided>) {
    let mut reached = Vec::new();
    let mut undecided = Vec::new();
    for observation in session.observations {
        // Three dispositions and not two. A line the caller concluded is a line
        // no standing decision of his is ever tested against — it arrived with
        // its answer — so it is left out and the leaving out is decided, not
        // failed. A line this build cannot read is neither reached nor ruled
        // out, and it is declared.
        match parse_intake(&observation.payload) {
            Err(_) => undecided.push(Undecided::UnreadableRow {
                row: observation.row,
            }),
            Ok(Intake::Concluded { .. }) => {}
            Ok(Intake::Observed { row, .. }) => {
                // The reading a standing decision is tested under, and the same
                // one `matcher_for` proposes from: the counterparty stays the
                // name the source printed, because that is what a condition
                // compares against.
                if matcher.matches(&row.subject(None)) {
                    reached.push(ReachedRow {
                        row: observation.row,
                        printed: printed_row(directory, &row),
                        awaiting_answer: session
                            .questions
                            .iter()
                            .filter(|question| question.row == observation.row)
                            .any(|question| session.settlements.awaits_answer(question)),
                        // From the same reading the line above is, so what is
                        // still waiting and what already settles it cannot be
                        // taken from two readings of one session.
                        now: session.settlements.settlement_for(observation.row).cloned(),
                    });
                }
            }
        }
    }
    (reached, undecided)
}

/// The movements already recorded that a condition reaches, and the ones it
/// cannot be tested against.
fn reach_over_journal(
    events: &[Event],
    matcher: &RuleMatcher,
    directory: &AccountDirectory,
) -> (Vec<ReachedFact>, Vec<Undecided>) {
    let mut reached = Vec::new();
    let mut undecided = Vec::new();
    // What is in force, which is what the recomputation reads — a fact a
    // correction reversed or replaced is not something a standing decision would
    // settle, and counting it would name a movement he has already put right as
    // one he still has to look at.
    let Ok(in_force) = resolve(events) else {
        return (Vec::new(), vec![Undecided::RecordedMovementsWouldNotFold]);
    };
    for event in in_force {
        // Through the one reader of a fact as a condition's subject, which is
        // the reader the recomputation replays history with. A second one here
        // would answer «this condition reaches that movement» differently from
        // the code that would actually classify it.
        let Some(read) = subject(event) else {
            continue;
        };
        let title = directory
            .held(event.account)
            .map(|account| account.title.clone());
        if unvouched_word(matcher, event, &read) {
            undecided.push(Undecided::FactWithoutTheWord {
                event: event.id,
                account: event.account,
                title,
                date: event.dates.effective_date(),
            });
            continue;
        }
        if !matcher.matches(&read) {
            continue;
        }
        let Some(amount) = cash_amount(&event.kind) else {
            continue;
        };
        reached.push(ReachedFact {
            event: event.id,
            account: event.account,
            title,
            date: event.dates.effective_date(),
            amount_minor: amount.amount().raw(),
            currency: amount.currency(),
            now: classification_of(event).map(classified_as),
        });
    }
    (reached, undecided)
}

/// Whether this condition asks about a word this fact's journal entry cannot
/// vouch for.
///
/// **Decision 0020 §3, read forwards.** A fact below
/// [`SOURCE_CATEGORY_IS_A_CATEGORY_FROM`] carries no operation word at all and
/// may carry one in the category's slot, and the two cannot be told apart
/// afterwards — so `subject` blanks the category and the operation word is
/// absent, and a condition asking about either gets a **false negative** off it.
/// The recomputation takes that false negative deliberately, because a rule that
/// does not fire leaves a fact exactly as the owner already accepted it. A
/// forecast cannot: the same silence there reads as «nothing else is affected»,
/// which is the one thing it must not say.
///
/// The other clauses are asked first, and that is what keeps the declaration
/// small and true. A condition's fields join with «and», so one clause the fact
/// plainly fails settles it as a non-match whatever the unreadable word says;
/// only a fact the rest of the condition holds for is genuinely undecided. A
/// condition made of nothing **but** the doubtful words has no rest, and every
/// other clause is then vacuously true.
fn unvouched_word(matcher: &RuleMatcher, event: &Event, read: &ClassificationSubject) -> bool {
    if event.schema_version >= SOURCE_CATEGORY_IS_A_CATEGORY_FROM {
        return false;
    }
    if matcher.kind.is_none() && matcher.source_category.is_none() {
        return false;
    }
    let rest = RuleMatcher {
        kind: None,
        source_category: None,
        ..matcher.clone()
    };
    rest.asks_nothing() || rest.matches(read)
}

/// The cash a fact moved, for the seven kinds a standing decision classifies.
///
/// `None` for a fact that carries no single cash amount, which is every kind
/// [`subject`] already returns nothing for — asked again here rather than
/// assumed, because a kind admitted there and not here must fail to be listed
/// rather than be listed with an invented amount.
const fn cash_amount(kind: &EventKind) -> Option<Money> {
    match kind {
        EventKind::CashIn { amount }
        | EventKind::CashOut { amount }
        | EventKind::Refund { amount }
        | EventKind::CashTransfer { amount, .. }
        | EventKind::OwnAccountMovement { amount }
        | EventKind::UnresolvedOwnAccountMovement { amount }
        | EventKind::Fee { amount, .. } => Some(*amount),
        EventKind::Income { gross, .. } => Some(*gross),
        _ => None,
    }
}

/// The forecast in one statement, in the register the owner reads.
fn forecast_notice(
    stands: &WouldStand,
    asked_row: u32,
    in_this_import: &[ReachedRow],
    already_recorded: &[ReachedFact],
    undecided: &[Undecided],
) -> String {
    let kept = match stands {
        WouldStand::Written(_) => GeneralisationProspect::WillStand.addressed(),
        WouldStand::ForHisAdoption(_) => GeneralisationProspect::NeedsHisAdoption.addressed(),
        WouldStand::NotFromThisRow => {
            return GeneralisationProspect::NoneFromThisRow
                .addressed()
                .to_owned();
        }
        WouldStand::NotFromThisAnswer => {
            return "Your answer keeps no standing decision, and it is your answer rather than \
                    these lines that keeps none: what you would be saying is what this statement \
                    did not contain, not what every line like these was, so there is nothing here \
                    for a later line to be matched against."
                .to_owned();
        }
    };
    // Two counts out of one list, and the split is `iaam-r0qk`. The list is
    // everything the condition covers, because a line left out of it reads as
    // «not affected»; the count beside «it would settle» is what answering
    // would actually decide, because a line his own answer or his own directory
    // already settled is one this decision never reaches. Counted together they
    // told him to go and look for the wrong one among lines the decision does
    // not touch.
    let (settled_anyway, others): (Vec<_>, Vec<_>) = in_this_import
        .iter()
        .filter(|line| line.row != asked_row)
        .partition(|line| line.settled_regardless());
    let others = others.len();
    let already = match settled_anyway.len() {
        0 => String::new(),
        1 => " One other line of this import is covered by the same words and is already \
              settled by something this would not displace — your own answer, or the other \
              side being recognised as an account of yours — so answering here leaves it as \
              it stands."
            .to_owned(),
        count => format!(
            " {count} other lines of this import are covered by the same words and are \
             already settled by something this would not displace — your own answer, or the \
             other side being recognised as an account of yours — so answering here leaves \
             them as they stand."
        ),
    };
    // The count of what is already recorded is not published where none of it
    // could be folded: zero would then read as «nothing of yours is affected»,
    // which is the one sentence a forecast must never compose out of a failure.
    let recorded = if undecided
        .iter()
        .any(|entry| matches!(entry, Undecided::RecordedMovementsWouldNotFold))
    {
        "none of what you have already recorded, because none of it could be read here".to_owned()
    } else {
        format!(
            "{} you have already recorded",
            counted(
                already_recorded.len(),
                "no movement",
                "1 movement",
                "movements"
            )
        )
    };
    format!(
        "{kept} Before it stands, this is everything it would cover. Besides the line you were \
         asked about it would settle {others} of this import, and {recorded}.{already} \
         {left_out} Read them \
         before you answer: your statement says the same thing about every one of them, but only \
         you know whether they were the same thing — one that was something else is decided \
         wrongly along with the rest, and the way to keep it out is to answer that one on its own \
         first. Nothing is written by asking this: no standing decision is made, nothing you have \
         already recorded is changed, and putting one of those movements right stays an act of \
         its own, separate from changing the standing decision behind it.",
        others = counted(others, "no other line", "1 other line", "other lines"),
        left_out = left_out(undecided),
    )
}

/// What the two lists above could not decide, said rather than omitted.
fn left_out(undecided: &[Undecided]) -> String {
    let unreadable = undecided
        .iter()
        .filter(|entry| matches!(entry, Undecided::UnreadableRow { .. }))
        .count();
    let would_not_fold = undecided
        .iter()
        .any(|entry| matches!(entry, Undecided::RecordedMovementsWouldNotFold));
    let unvouched = undecided.len() - unreadable - usize::from(would_not_fold);
    if undecided.is_empty() {
        return "Nothing was left out of those two counts.".to_owned();
    }
    let mut clauses = Vec::new();
    if would_not_fold {
        clauses.push(
            "what you have already recorded could not be read as a whole, because something you \
             put right earlier no longer lines up with what it was putting right"
                .to_owned(),
        );
    }
    if unreadable > 0 {
        clauses.push(format!(
            "{} of this import cannot be read here",
            counted(unreadable, "", "1 line", "lines")
        ));
    }
    if unvouched > 0 {
        clauses.push(format!(
            "{} you have already recorded cannot be judged either way, because it was written \
             before what your institution called an operation and what it filed the operation \
             under were kept in separate places",
            counted(unvouched, "", "1 movement", "movements")
        ));
    }
    format!(
        "{} — each is named beside this with its reason, and none of them is included or \
         excluded.",
        clauses.join(", and ")
    )
}

/// A count in words, so that none and one do not read as a defect.
fn counted(count: usize, none: &str, one: &str, many: &str) -> String {
    match count {
        0 => none.to_owned(),
        1 => one.to_owned(),
        _ => format!("{count} {many}"),
    }
}

/// The whole forecast, out of what a caller has already read.
///
/// Pure, and the store reading is [`preview_answer_rule`]'s: what this call must
/// not do is write, and the way to hold that is for the deciding half to have
/// nothing to write with.
fn forecast(
    stands: WouldStand,
    asked_row: u32,
    session: &SessionAsRead<'_>,
    events: &[Event],
    directory: &AccountDirectory,
) -> AnswerRuleForecast {
    let (in_this_import, already_recorded, mut undecided) = match stands.proposed() {
        None => (Vec::new(), Vec::new(), Vec::new()),
        Some(proposed) => {
            let (rows, unreadable) = reach_over_session(session, &proposed.matcher, directory);
            let (facts, unvouched) = reach_over_journal(events, &proposed.matcher, directory);
            (rows, facts, [unreadable, unvouched].concat())
        }
    };
    undecided.sort_by_key(|entry| match entry {
        Undecided::UnreadableRow { row } => (0, u64::from(*row), EventId(Uuid::nil())),
        Undecided::FactWithoutTheWord { event, .. } => (1, 0, *event),
        Undecided::RecordedMovementsWouldNotFold => (2, 0, EventId(Uuid::nil())),
    });
    let notice = forecast_notice(
        &stands,
        asked_row,
        &in_this_import,
        &already_recorded,
        &undecided,
    );
    AnswerRuleForecast {
        stands,
        in_this_import,
        already_recorded,
        undecided,
        notice,
    }
}

/// What the standing decision an answer would keep would settle, before it
/// stands.
///
/// **Read-only, and it stands the decision no sooner.** Nothing is written,
/// nothing is retired and nothing is adopted: the store is read three times —
/// the session, the owner's directory, the journal — and the deciding half is a
/// pure fold over what those reads returned. That is the same promise
/// `preview_category_rule` makes for a category rule, kept the same way.
///
/// **It does not merge two acts into one.** Answering is still `answer_question`
/// and correcting a recorded movement is still a correction; this call performs
/// neither, and its whole purpose is that the second is an informed choice
/// rather than a discovery next month.
///
/// The answer is checked against what the question admits, exactly as answering
/// checks it: a forecast for an answer that cannot be given describes an act
/// nobody can perform.
pub async fn preview_answer_rule(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    question: ImportQuestionId,
    answer: Answer,
) -> Result<AnswerRuleForecast, AppError> {
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
        .into());
    }
    let observed = observed_row(&contents.observations, stored.row)?;
    let stands = would_stand(&observed, answer, may_generalise(principal));
    // Nothing is read that nothing would be said about. A forecast with no
    // standing decision in it has an empty reach whatever the journal holds, and
    // the state says why it is empty — so the three reads below are skipped
    // rather than performed and discarded.
    if stands.proposed().is_none() {
        return Ok(forecast(
            stands,
            stored.row,
            &SessionAsRead {
                observations: &contents.observations,
                questions: &contents.questions,
                settlements: &QuestionSettlements::default(),
            },
            &[],
            &AccountDirectory::from_accounts(Vec::new()),
        ));
    }
    let directory = AccountDirectory::load(services, principal.owner).await?;
    // The same reading `answer_question` takes to decide what its own reach
    // settles, rather than `is_open` alone: a line a standing decision of his
    // already settles is not one this import is waiting on, and saying it is
    // would be the reading `iaam-m2oi` replaced.
    let settlements = SessionReading::of(services, principal, &contents)
        .await?
        .settlements();
    let events = services
        .store
        .load_events_through(principal.owner, time::Date::MAX)
        .await?;
    Ok(forecast(
        stands,
        stored.row,
        &SessionAsRead {
            observations: &contents.observations,
            questions: &contents.questions,
            settlements: &settlements,
        },
        &events,
        &directory,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::AccountAliasView;
    use iaam_core::event::kind::FeeOrigin;
    use iaam_ingest::classification::FarSide;
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

    /// A directory holding nothing, for a test that asks about something else.
    ///
    /// An empty directory is a real state and not a stub: a row may name an
    /// account by identifier that the owner's directory has never held, which is
    /// what `AccountResolution::missing` publishes. A question about such a row
    /// publishes no title, and the tests that assert that say so by name.
    fn no_accounts() -> AccountDirectory {
        AccountDirectory::from_accounts(Vec::new())
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

    // --- The names a document asked for and the directory does not hold ------
    //
    // `missing` is a list of identifiers, and a printed string that matched
    // nothing has none — so the section that answers «which accounts did these
    // rows name» could not answer it in the one case where the answer decides
    // what the owner does next (iaam-x9ls, decision 0024 §2).

    fn recorded(session: ImportSessionId, printed: &str, records: u32) -> UnresolvedAccountView {
        UnresolvedAccountView {
            session,
            document_hash: "f".repeat(64),
            printed: printed.to_owned(),
            records,
        }
    }

    /// The assessment names an account no row of it could carry.
    ///
    /// The records that printed these strings were refused when the document was
    /// read, so this session holds no row for any of them and no fold over its
    /// rows can see them. Read off what the reading recorded instead.
    #[test]
    fn the_assessment_names_the_accounts_the_documents_asked_for() {
        let session = ImportSessionId::new_random();
        let resolution = account_resolution(
            &resolver(vec![detail(account(1), "Main")]),
            &[],
            &[
                recorded(session, "Shop One", 3),
                recorded(session, "Shop Two", 1),
            ],
        );

        assert_eq!(
            resolution.unrecognised,
            vec!["Shop One".to_owned(), "Shop Two".to_owned()]
        );
        // And nowhere else: the three lists above it are about rows this session
        // holds, and it holds none of these.
        assert!(resolution.missing.is_empty());
        assert!(resolution.resolved.is_empty());
        assert!(resolution.conflicting.is_empty());
    }

    /// A name the directory now places is not listed.
    ///
    /// The stored fact is a transcription — this document printed this string —
    /// and the verdict on it is recomputed here, against the directory this
    /// assessment was built with. So an account created after the reading drops
    /// out without the document being read again, and it drops out through the
    /// identity tier rather than the title tier.
    #[test]
    fn an_account_created_since_the_reading_is_not_listed() {
        let session = ImportSessionId::new_random();
        let resolution = account_resolution(
            &resolver(vec![with_identity(detail(account(1), "Main"), "Shop One")]),
            &[],
            &[
                recorded(session, "Shop One", 3),
                recorded(session, "Shop Two", 1),
            ],
        );
        assert_eq!(resolution.unrecognised, vec!["Shop Two".to_owned()]);
    }

    /// One name, however many documents printed it.
    #[test]
    fn a_name_two_documents_printed_is_listed_once() {
        let session = ImportSessionId::new_random();
        let mut second = recorded(session, "Shop One", 2);
        second.document_hash = "a".repeat(64);
        let resolution = account_resolution(
            &resolver(vec![detail(account(1), "Main")]),
            &[],
            &[recorded(session, "Shop One", 3), second],
        );
        assert_eq!(resolution.unrecognised, vec!["Shop One".to_owned()]);
    }

    fn resolver(accounts: Vec<AccountDetailView>) -> Resolver {
        Resolver {
            directory: AccountDirectory::from_accounts(accounts),
            statements: Vec::new(),
            rules: Vec::new(),
        }
    }

    /// A resolver holding rules the owner has already written.
    fn ruled(accounts: Vec<AccountDetailView>, rules: Vec<ClassificationRule>) -> Resolver {
        Resolver {
            directory: AccountDirectory::from_accounts(accounts),
            statements: Vec::new(),
            rules,
        }
    }

    fn stating(
        accounts: Vec<AccountDetailView>,
        statements: Vec<(AccountId, Vec<AccountId>)>,
    ) -> Resolver {
        Resolver {
            directory: AccountDirectory::from_accounts(accounts),
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
            far_side: FarSide::Unstated,
            source_kind: Some("transfer".to_owned()),
            source_category: None,
            owner_category: None,
            source_code: None,
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

    /// The same row with the source naming nobody, at the amount it printed.
    ///
    /// The shape `iaam-3ewp` was found in: a word the bank uses for a movement
    /// internal to itself, an amount, a date, and no counterparty at all. It is
    /// the absence of the counterparty that makes several such rows of one
    /// statement read alike, so it is what the fixture has to reproduce.
    fn anonymous(row: ObservedRow, amount_minor: i64) -> ObservedRow {
        ObservedRow {
            counterparty: ObservedCounterparty::Unknown,
            amount_minor,
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
        let directory = AccountDirectory::from_accounts(vec![
            with_identity(detail(main, "Main"), "ACC-1"),
            detail(account(2), "Savings"),
        ]);
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
        let directory = AccountDirectory::from_accounts(vec![with_alias(
            detail(main, "Main"),
            "CARD-1",
            date!(2026 - 01 - 01),
            Some(date!(2026 - 03 - 01)),
        )]);
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
        let directory = AccountDirectory::from_accounts(vec![
            with_identity(detail(account(1), "Main"), "ACC-1"),
            with_alias(
                detail(account(2), "Savings"),
                "ACC-1",
                date!(2026 - 01 - 01),
                None,
            ),
        ]);
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
        let directory = AccountDirectory::from_accounts(vec![detail(account(1), "Main")]);
        let error = directory
            .resolve_declared("ACC-9")
            .expect_err("no account answers to it");
        assert_eq!(error.code(), "invalid_request");
        assert!(
            error.to_string().contains("ACC-9"),
            "the refusal repeats what was sent: {error}"
        );
    }

    // --- two payment instruments over one account (iaam-tb5o) --------------

    #[test]
    fn a_far_side_that_resolves_to_this_very_account_settles_without_a_fact() {
        // Decision 0004's alias, doing the work it was designed for: two cards
        // over one underlying account are one account with two aliases, so the
        // identifier the source printed for the far side resolves to the very
        // account the row is on. The honest record is nothing — the balance
        // does not change and there is no second leg to wait for — and no
        // question is asked, which is the owner's actual requirement.
        let main = account(1);
        let resolver = resolver(vec![with_alias(
            detail(main, "Main"),
            "card-two",
            time::macros::date!(2024 - 01 - 01),
            None,
        )]);
        match resolver.assess(&row(main, "card-two", None)) {
            Assessment::NoFact { reason } => assert_eq!(
                reason,
                NoFactReason::OneAccountTwoInstruments { account: main }
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn it_settles_whether_or_not_the_source_stated_a_direction() {
        // The determination does not consult one, and could not be helped by
        // one: whichever way the money ran between two instruments over one
        // account, the account moved by nothing. That is what lets it settle a
        // row no question could have settled.
        let main = account(1);
        let resolver = resolver(vec![with_identity(detail(main, "Main"), "acct-1")]);
        match resolver.assess(&directionless(row(main, "acct-1", None))) {
            Assessment::NoFact { reason } => assert_eq!(
                reason,
                NoFactReason::OneAccountTwoInstruments { account: main }
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_far_side_that_is_a_different_account_is_still_a_transfer() {
        // The falsification: if any resolved own account settled without a
        // fact, every internal transfer would vanish from the journal.
        let main = account(1);
        let savings = account(2);
        let resolver = resolver(vec![detail(main, "Main"), detail(savings, "Savings")]);
        match resolver.assess(&row(main, "Savings", None)) {
            Assessment::Settled { classification, .. } => assert_eq!(
                classification,
                Classification::InternalTransfer { to: savings }
            ),
            other => panic!("{other:?}"),
        }
    }

    // --- a source that names whose account the far side is (iaam-cp94) -----

    #[test]
    fn a_source_asserting_its_own_accounts_is_recorded_rather_than_asked_about() {
        // The four rows of the run this wave came from: a date, an amount, no
        // direction, no counterparty, and the source's own word for a movement
        // between the owner's accounts. Each of them used to raise a question
        // and hold the commit.
        let main = account(1);
        let resolver = resolver(vec![detail(main, "Main")]);
        let mut asserted = directionless(row(main, "Somebody", None));
        asserted.counterparty = ObservedCounterparty::Unknown;
        asserted.far_side = FarSide::OwnAccount;
        match resolver.assess(&asserted) {
            Assessment::Settled {
                classification,
                movement,
                ..
            } => {
                assert_eq!(classification, Classification::OwnAccountMovement);
                assert_eq!(movement, None, "and nothing invented a direction");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_same_row_without_the_assertion_is_still_a_question() {
        let main = account(1);
        let resolver = resolver(vec![detail(main, "Main")]);
        let mut plain = directionless(row(main, "Somebody", None));
        plain.counterparty = ObservedCounterparty::Unknown;
        assert!(matches!(
            resolver.assess(&plain),
            Assessment::Ambiguous {
                question: Question::UnresolvedDirection { .. }
            }
        ));
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
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
            }
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
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
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
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
            }
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
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
            }
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
                ..
            } => panic!(
                "the source stated no direction, so there is none to settle                  with: {classification:?} {movement:?}"
            ),
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
            }
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
                source_category: None,
                owner_category: None,
                source_code: None,
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
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
            }
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
                ..
            } => {
                assert_eq!(
                    classification,
                    Classification::InternalTransfer { to: checking }
                );
                assert_eq!(movement, Some(Movement::Out), "the source printed «out»");
            }
            Assessment::Ambiguous { question } => {
                panic!("the source stated the direction: {question:?}")
            }
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
            }
        }

        match resolver.assess(&incoming(row(main, "Checking", None))) {
            Assessment::Settled {
                classification,
                movement,
                ..
            } => {
                assert_eq!(
                    classification,
                    Classification::InternalTransfer { to: checking },
                    "the far side is the same account whichever way the row ran"
                );
                assert_eq!(movement, Some(Movement::In), "and the source printed «in»");
            }
            Assessment::Ambiguous { question } => {
                panic!("the source stated the direction: {question:?}")
            }
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
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
                    source_category: None,
                    owner_category: None,
                    source_code: None,
                },
                outcome: Classification::Fee {
                    origin: FeeOrigin::AccountMaintenance,
                },
            }],
        );

        match fee.assess(&directionless(row(main, "Somebody", None))) {
            Assessment::Settled { movement, .. } => assert_eq!(movement, Some(Movement::Out)),
            Assessment::Ambiguous { question } => {
                panic!("a fee leaves the account, and that is not a guess: {question:?}")
            }
            Assessment::NoFact { reason } => {
                panic!("nothing here resolves the far side to this row's own account: {reason:?}")
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
    // The same decision asked many times (iaam-q5og, decision 0029)
    // -----------------------------------------------------------------------

    /// One open question about a row, stored as the session stores it.
    fn stored_question_about(row: u32, asked: &Question) -> ImportQuestionView {
        ImportQuestionView {
            id: ImportQuestionId::new_random(),
            session: ImportSessionId::new_random(),
            row,
            question: serde_json::to_string(asked).expect("a question"),
            alternatives: serde_json::to_string(&asked.alternatives()).expect("alternatives"),
            prompt: String::new(),
            asked_at: "2026-03-01T00:00:00Z".to_owned(),
            answered_at: None,
            answer: None,
            rule: None,
        }
    }

    fn arriving(mut observed: ObservedRow) -> ObservedRow {
        observed.direction = ObservedDirection::In;
        observed.amount_minor = 1_000;
        observed
    }

    /// The repeats are published as repeats, from both sides.
    ///
    /// The complaint: the assessment listed questions in row order, two thirds
    /// of them the same decision, and nothing said so — so grouping was work
    /// every caller had to invent and the owner was read a question he had
    /// already answered.
    #[test]
    fn two_rows_naming_one_counterparty_the_same_way_each_name_the_other() {
        let main = account(1);
        let asked = Question::IsTransferInternal {
            account: main,
            counterparty: "Shop One".to_owned(),
        };
        let observations = vec![
            stored_row(1, &row(main, "Shop One", None)),
            stored_row(2, &row(main, "Shop One", None)),
        ];
        let questions = vec![
            stored_question_about(1, &asked),
            stored_question_about(2, &asked),
        ];
        let open = open_questions(
            &no_accounts(),
            &observations,
            &questions,
            &QuestionSettlements::default(),
        );
        assert_eq!(open[0].alike, vec![2]);
        assert_eq!(open[1].alike, vec![1]);
        assert!(
            !open[0].alternatives.is_empty(),
            "and the words that answer it travel with it (iaam-ulib)"
        );
    }

    /// One counterparty, two directions, two decisions.
    ///
    /// This is the whole reason the subject is a pair. The stored questions here
    /// are byte-identical — `question_for` builds `IsTransferInternal` for a
    /// named party whichever way the row ran — and an answer states a direction
    /// of its own that `resolve_with` records, so carrying one across both would
    /// file money that arrived as money that left.
    #[test]
    fn one_counterparty_the_source_ran_two_ways_is_not_one_decision() {
        let main = account(1);
        let asked = Question::IsTransferInternal {
            account: main,
            counterparty: "Shop One".to_owned(),
        };
        let observations = vec![
            stored_row(1, &row(main, "Shop One", None)),
            stored_row(2, &arriving(row(main, "Shop One", None))),
        ];
        let questions = vec![
            stored_question_about(1, &asked),
            stored_question_about(2, &asked),
        ];
        assert_eq!(questions[0].question, questions[1].question);
        let open = open_questions(
            &no_accounts(),
            &observations,
            &questions,
            &QuestionSettlements::default(),
        );
        assert!(open[0].alike.is_empty(), "{open:?}");
        assert!(open[1].alike.is_empty(), "{open:?}");
    }

    /// Two counterparties are two decisions.
    #[test]
    fn two_counterparties_are_never_alike() {
        let main = account(1);
        let observations = vec![
            stored_row(1, &row(main, "Shop One", None)),
            stored_row(2, &row(main, "Shop Two", None)),
        ];
        let questions = vec![
            stored_question_about(
                1,
                &Question::IsTransferInternal {
                    account: main,
                    counterparty: "Shop One".to_owned(),
                },
            ),
            stored_question_about(
                2,
                &Question::IsTransferInternal {
                    account: main,
                    counterparty: "Shop Two".to_owned(),
                },
            ),
        ];
        let open = open_questions(
            &no_accounts(),
            &observations,
            &questions,
            &QuestionSettlements::default(),
        );
        assert!(open[0].alike.is_empty());
        assert!(open[1].alike.is_empty());
    }

    /// An answered question is neither published nor counted as a repeat.
    #[test]
    fn a_question_already_answered_is_no_longer_one_of_the_repeats() {
        let main = account(1);
        let asked = Question::IsTransferInternal {
            account: main,
            counterparty: "Shop One".to_owned(),
        };
        let observations = vec![
            stored_row(1, &row(main, "Shop One", None)),
            stored_row(2, &row(main, "Shop One", None)),
        ];
        let mut questions = vec![
            stored_question_about(1, &asked),
            stored_question_about(2, &asked),
        ];
        questions[0].answered_at = Some("2026-03-02T00:00:00Z".to_owned());
        questions[0].answer = Some(serde_json::to_string(&Answer::Paid).expect("an answer"));
        let open = open_questions(
            &no_accounts(),
            &observations,
            &questions,
            &QuestionSettlements::default(),
        );
        assert_eq!(open.len(), 1, "{open:?}");
        assert!(open[0].alike.is_empty(), "{open:?}");
    }

    /// A question this build cannot read is published alone, and says nothing.
    ///
    /// Grouping needs both halves — the question and the row it was asked about
    /// — and a stored question that will not parse has neither. Publishing it
    /// with an empty list is the honest answer; the mistake the absence must not
    /// be read as is «this one is unlike every other», which is a claim nobody
    /// made.
    #[test]
    fn a_stored_question_this_build_cannot_read_is_alike_to_nothing() {
        let main = account(1);
        let asked = Question::IsTransferInternal {
            account: main,
            counterparty: "Shop One".to_owned(),
        };
        let observations = vec![
            stored_row(1, &row(main, "Shop One", None)),
            stored_row(2, &row(main, "Shop One", None)),
        ];
        let mut questions = vec![
            stored_question_about(1, &asked),
            stored_question_about(2, &asked),
        ];
        questions[0].question = "{\"question\":\"a word this build has never had\"}".to_owned();
        let open = open_questions(
            &no_accounts(),
            &observations,
            &questions,
            &QuestionSettlements::default(),
        );
        assert!(open[0].alike.is_empty(), "{open:?}");
        assert!(
            open[1].alike.is_empty(),
            "and the readable one does not claim it as a repeat either: {open:?}"
        );
    }

    // -----------------------------------------------------------------------
    // One movement a document printed twice (iaam-3qsq, decision 0031)
    // -----------------------------------------------------------------------

    /// One row of a session, read as the plan reads it.
    ///
    /// `answer` is what the owner said about it, and `None` is a row whose
    /// question is still open — which the plan carries as a rejection, because
    /// that is what an unanswered row resolves to.
    fn read_row(row: u32, observed: &ObservedRow, answer: Option<Answer>) -> ReadRow {
        let candidate = answer.map_or_else(
            || {
                Err(Rejection {
                    field: "row".to_owned(),
                    expected: "a row whose classification is settled".to_owned(),
                    actual: "unanswered".to_owned(),
                })
            },
            |answer| {
                let operation = observed.resolve_with(answer).expect("the row resolves");
                Ok(normalize(
                    &operation,
                    &NormalizationContext {
                        owner: OwnerId(uuid::Uuid::from_bytes([9; 16])),
                        source: SourceId(uuid::Uuid::from_bytes([9; 16])),
                        parser_version: ParserVersion("ingest/manual/1".to_owned()),
                    },
                )
                .expect("it normalises")
                .event)
            },
        );
        ReadRow {
            row,
            intake: Some(Intake::Observed {
                row: Box::new(observed.clone()),
                reader: None,
            }),
            operation: None,
            row_key: None,
            payload: String::new(),
            candidate: Some(candidate),
            settled: None,
            basis: answer.map(|_| FactBasis::Answered),
        }
    }

    /// The settlements one reading of these rows yields, exactly as
    /// [`SessionReading`] computes them.
    ///
    /// The mirror pass is applied to the rows first and the fold comes after,
    /// in that order and through the same two functions the plan uses: a test
    /// that folded before the pass would assert against a reading no caller can
    /// ever get.
    fn settlements(rows: &mut [ReadRow], mirrors: &MirroredRows) -> QuestionSettlements {
        settle_mirrored(rows, mirrors);
        QuestionSettlements::of(rows, mirrors)
    }

    /// The two rows one movement between two of the owner's accounts prints.
    fn two_legs(main: AccountId, savings: AccountId) -> (ObservedRow, ObservedRow) {
        let day = time::macros::date!(2025 - 04 - 10);
        let mut departure = row(main, "Anything", Some(day));
        departure.counterparty = ObservedCounterparty::Unknown;
        let mut arrival = arriving(row(savings, "Anything", Some(day)));
        arrival.counterparty = ObservedCounterparty::Unknown;
        (departure, arrival)
    }

    /// Answering both legs records the movement once.
    ///
    /// The state before decision 0031: both answers reach
    /// `Classification::InternalTransfer`, both rows resolve to a transfer
    /// carrying a leg on **each** account, the two rows have different keys so
    /// deduplication sees nothing, and every account moves twice.
    #[test]
    fn both_legs_of_one_movement_answered_record_one_transfer_from_the_sending_side() {
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let rows = vec![
            read_row(
                1,
                &departure,
                Some(Answer::SentToOwnAccount { to: savings }),
            ),
            read_row(
                2,
                &arrival,
                Some(Answer::ReceivedFromOwnAccount { from: main }),
            ),
        ];
        let mirrors = mirrored_rows(ImportSessionId::new_random(), &rows, &[]);
        assert_eq!(mirrors.settled.get(&2), Some(&1));
        assert!(
            !mirrors.settled.contains_key(&1),
            "the sending row is the one that records it"
        );
    }

    /// Answering one leg settles the other, so the session can commit.
    ///
    /// The second half of the bead: answering only one leg used to leave the
    /// mirror row's question open, the commit refused, and answering that one
    /// too recorded the movement twice. There was no third option.
    #[test]
    fn answering_one_leg_settles_the_other_and_leaves_no_question_open() {
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let mut rows = vec![
            read_row(
                1,
                &departure,
                Some(Answer::SentToOwnAccount { to: savings }),
            ),
            read_row(2, &arrival, None),
        ];
        let questions = vec![stored_question_about(
            2,
            &Question::IsInflowIncome { account: savings },
        )];
        let mirrors = mirrored_rows(ImportSessionId::new_random(), &rows, &questions);
        assert_eq!(mirrors.settled.get(&2), Some(&1));
        let settled = settlements(&mut rows, &mirrors);
        let observations = vec![stored_row(2, &arrival)];
        assert!(
            open_questions(&no_accounts(), &observations, &questions, &settled).is_empty(),
            "the answer to the other leg is the answer to this row"
        );
    }

    /// Neither leg answered: one decision, published twice and settled by
    /// neither.
    #[test]
    fn two_unanswered_legs_are_published_as_one_decision_and_settle_nothing() {
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let mut rows = vec![read_row(1, &departure, None), read_row(2, &arrival, None)];
        let questions = vec![
            stored_question_about(1, &Question::IsOutflowAFee { account: main }),
            stored_question_about(2, &Question::IsInflowIncome { account: savings }),
        ];
        let mirrors = mirrored_rows(ImportSessionId::new_random(), &rows, &questions);
        assert!(
            mirrors.settled.is_empty(),
            "a shape is not an answer, so nothing is recorded and nothing is suppressed"
        );
        let settled = settlements(&mut rows, &mirrors);
        let observations = vec![stored_row(1, &departure), stored_row(2, &arrival)];
        let open = open_questions(&no_accounts(), &observations, &questions, &settled);
        assert_eq!(open.len(), 2);
        let (first, second) = (
            open[0].pair.expect("the departure is one side of a pair"),
            open[1].pair.expect("the arrival is the other"),
        );
        assert_eq!(
            first.id, second.id,
            "and the two carry one identifier, so the decision can be put once"
        );
        assert_eq!(
            (first.row, second.row),
            (open[1].row, open[0].row),
            "each naming the row it is not, so the decision can be put in words"
        );
    }

    /// The pair is a hypothesis, and the answer is how it is refused.
    #[test]
    fn an_answer_that_names_no_own_account_leaves_the_two_rows_as_two_rows() {
        // «No, these are two different things»: one of them was a payment out,
        // and the arrival beside it is somebody else's money.
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let mut rows = vec![
            read_row(1, &departure, Some(Answer::Paid)),
            read_row(2, &arrival, None),
        ];
        let questions = vec![stored_question_about(
            2,
            &Question::IsInflowIncome { account: savings },
        )];
        let mirrors = mirrored_rows(ImportSessionId::new_random(), &rows, &questions);
        assert!(mirrors.settled.is_empty(), "{mirrors:?}");
        let settled = settlements(&mut rows, &mirrors);
        let observations = vec![stored_row(2, &arrival)];
        assert_eq!(
            open_questions(&no_accounts(), &observations, &questions, &settled).len(),
            1,
            "and the arrival still has to be answered on its own"
        );
    }

    /// The same pair, read by a caller that has not loaded the session.
    ///
    /// This is what the action queue holds: the session's observations and its
    /// stored questions, and no reading of the owner's directory or his rules.
    /// The two legs must come out as one decision there too, or the queue
    /// publishes the two rows as two items and an agent answers both — which is
    /// the movement recorded twice that decision 0031 exists to prevent.
    #[test]
    fn the_two_legs_are_one_decision_to_a_caller_holding_only_the_questions() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let observations = vec![stored_row(1, &departure), stored_row(2, &arrival)];
        let questions = vec![
            stored_question_about(1, &Question::IsOutflowAFee { account: main }),
            stored_question_about(2, &Question::IsInflowIncome { account: savings }),
        ];

        let movements = mirrored_movements_of(session, &observations, &questions);
        let OneMovement::Waiting(pair) = movements[&1] else {
            panic!("the departure is one leg of a movement still waiting: {movements:?}")
        };
        assert_eq!(movements[&2], OneMovement::Waiting(pair));
        assert_eq!(
            pair.id,
            pair_identity(session, 1, 2),
            "and under the identifier the assessment publishes, so the two \
             surfaces name one decision"
        );
        assert_eq!((pair.departure.row, pair.arrival.row), (1, 2));
        assert_eq!(
            (pair.departure.amount_minor, pair.arrival.amount_minor),
            (departure.amount_minor, arrival.amount_minor),
            "each leg carries the amount its own line printed, sign included"
        );
    }

    /// One leg answered leaves the other with nothing of its own to record.
    ///
    /// The far side comes out of the answer rather than out of a reading: it is
    /// the account he named, and it is what makes the queue stop asking about
    /// the second row instead of asking him to record the movement again.
    #[test]
    fn an_answered_leg_leaves_the_other_recording_nothing_of_its_own() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let observations = vec![stored_row(1, &departure), stored_row(2, &arrival)];
        let mut answered = stored_question_about(1, &Question::IsOutflowAFee { account: main });
        answered.answered_at = Some("2026-03-02T00:00:00Z".to_owned());
        answered.answer = Some(
            serde_json::to_string(&Answer::SentToOwnAccount { to: savings }).expect("an answer"),
        );
        let questions = vec![
            answered,
            stored_question_about(2, &Question::IsInflowIncome { account: savings }),
        ];

        let movements = mirrored_movements_of(session, &observations, &questions);
        assert_eq!(movements.get(&2), Some(&OneMovement::Recorded { by: 1 }));
        assert!(
            !movements.contains_key(&1),
            "the row he answered records the movement and is not published as \
             half of anything: {movements:?}"
        );
    }

    /// «No, these are two different things» leaves two rows and two questions.
    ///
    /// The refusal `iaam_ingest::mirror` insists on, at this surface: an answer
    /// naming no account of his own says the row was something else, and a shape
    /// two rows happen to share does not overrule it.
    #[test]
    fn an_answer_naming_no_own_account_pairs_nothing_for_the_queue() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let observations = vec![stored_row(1, &departure), stored_row(2, &arrival)];
        let mut answered = stored_question_about(1, &Question::IsOutflowAFee { account: main });
        answered.answered_at = Some("2026-03-02T00:00:00Z".to_owned());
        answered.answer = Some(serde_json::to_string(&Answer::Paid).expect("an answer"));
        let questions = vec![
            answered,
            stored_question_about(2, &Question::IsInflowIncome { account: savings }),
        ];

        // The property the emptiness rests on, stated rather than relied on.
        // `is_empty` holds for any reason at all, and the reason here is meant
        // to be «no pair»: the arrival's own question offers no own-account
        // word, so the other thing this function publishes about a lone row —
        // the sentence that this document holds no counterpart for it — is not
        // due about it either, and the map is empty for the one reason the test
        // is named for.
        assert!(
            !Question::IsInflowIncome { account: savings }
                .alternatives()
                .iter()
                .any(|shape| shape.needs_account()),
            "the fixture's arrival admits an own-account answer, so an empty map \
             would no longer mean «no pair»"
        );
        assert!(
            mirrored_movements_of(session, &observations, &questions).is_empty(),
            "the arrival is still a row with a question of its own"
        );
    }

    /// A merchant beside a direction is not the near half of anything
    /// (`iaam-y5ww`).
    ///
    /// The gate used to be «the question admits an own-account answer», and
    /// [`Question::IsTransferInternal`] admits one — it is raised for any row
    /// with a named counterparty and a stated direction, a card payment
    /// included. On a one-account import nothing pairs, so nearly every open
    /// row was handed the paragraph about a counterpart the document does not
    /// hold. A card payment is not the near half of anything and has nothing to
    /// be told about.
    #[test]
    fn a_merchant_row_the_source_stated_a_direction_for_is_not_a_leg() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let observations = vec![stored_row(
            1,
            &row(main, "Shop One", Some(date!(2025 - 04 - 10))),
        )];
        let questions = vec![stored_question_about(
            1,
            &Question::IsTransferInternal {
                account: main,
                counterparty: "Shop One".to_owned(),
            },
        )];

        assert!(
            mirrored_movements_of(session, &observations, &questions).is_empty(),
            "an ordinary payment to a named merchant is told this document holds \
             no counterpart for it"
        );
    }

    /// The three shapes that *are* a leg still say so.
    ///
    /// The companion to the test above, and it is what keeps that one from
    /// passing by the whole publication being deleted: each of the three things
    /// a source can say that leaves a row leg-shaped is a row that gets the
    /// sentence.
    #[test]
    fn what_the_source_says_about_a_leg_is_what_publishes_the_sentence() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let day = date!(2025 - 04 - 10);
        // Named nobody at all.
        let mut nameless = row(main, "Shop One", Some(day));
        nameless.counterparty = ObservedCounterparty::Unknown;
        // The source's own word for a movement internal to itself.
        let inner = directionless(row(main, "Shop One", Some(day)));
        // The source asserting the far side is one of his own accounts.
        let mut asserted = row(main, "Shop One", Some(day));
        asserted.far_side = FarSide::OwnAccount;

        for (what, observed) in [
            ("a row the source named nobody on", nameless),
            ("a row the source called internal to itself", inner),
            ("a row the source said runs to his own account", asserted),
        ] {
            let observations = vec![stored_row(1, &observed)];
            let questions = vec![stored_question_about(
                1,
                &Question::UnresolvedDirection {
                    account: main,
                    stated: observed.source_kind.clone(),
                    counterparty: observed.counterparty_name().map(str::to_owned),
                },
            )];
            assert_eq!(
                mirrored_movements_of(session, &observations, &questions).get(&1),
                Some(&OneMovement::NoCounterpart(NoCounterpart::OneAccount)),
                "{what} is not told this document holds no counterpart for it"
            );
        }
    }

    /// The paragraph points at an answer that exists (`iaam-axrf`).
    ///
    /// The wave that added it was right about the row and had nowhere to send
    /// him: it says that naming a far account records a movement whose other
    /// half this document does not hold, and that «the money left the
    /// perimeter» files an internal move as spending, and then stops. Both
    /// halves are asserted here — that the question carrying the paragraph
    /// admits the third answer, and that the paragraph says so — because either
    /// alone is the defect: an answer nothing mentions is one he never reaches,
    /// and a sentence naming an answer the question refuses is worse than
    /// silence.
    #[test]
    fn the_no_counterpart_paragraph_names_an_answer_the_question_admits() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let observed = directionless(row(main, "Shop One", Some(date!(2025 - 04 - 10))));
        let asked = Question::UnresolvedDirection {
            account: main,
            stated: observed.source_kind.clone(),
            counterparty: observed.counterparty_name().map(str::to_owned),
        };
        let observations = vec![stored_row(1, &observed)];
        let questions = vec![stored_question_about(1, &asked)];

        let movements = mirrored_movements_of(session, &observations, &questions);
        let Some(&OneMovement::NoCounterpart(reason)) = movements.get(&1) else {
            panic!("the row this paragraph is written for: {movements:?}");
        };
        assert!(
            asked
                .alternatives()
                .contains(&AnswerShape::BetweenOwnAccounts),
            "the paragraph is published only on a question that offers the answer it points at"
        );
        assert!(
            reason.reported().contains("cannot say which"),
            "the paragraph names the answer rather than leaving him with the two \
             it has just told him are wrong: {}",
            reason.reported()
        );
    }

    /// The queue weighs every row of the session, not only the questioned ones
    /// (`iaam-y5ww`).
    ///
    /// `mirrored`'s ambiguity refusal is a function of the whole side set, so
    /// two passes handed different sets disagree about the same two rows. An
    /// open departure, an open arrival, and a third arrival of the same amount
    /// on the same day that something already settled: the deep pass saw three
    /// sides, found two candidate counterparts and refused; the queue saw two,
    /// paired them, published one item saying the other row raises none of its
    /// own, and suppressed it. That is a pairing one surface asserts and the
    /// other denies, with an open question hidden behind it.
    #[test]
    fn a_settled_third_side_makes_the_queue_refuse_what_the_assessment_refuses() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let savings = account(2);
        let reserve = account(3);
        let (departure, arrival) = two_legs(main, savings);
        // The same arrival on a third account, settled by something the queue
        // cannot see — here the owner's own answer on a row that raised no
        // question of its own, which is what a rule or his directory does.
        let mut elsewhere = arrival.clone();
        elsewhere.account = reserve;
        let observations = vec![
            stored_row(1, &departure),
            stored_row(2, &arrival),
            stored_row(3, &elsewhere),
        ];
        let questions = vec![
            stored_question_about(1, &Question::IsOutflowAFee { account: main }),
            stored_question_about(2, &Question::IsInflowIncome { account: savings }),
        ];

        let rows = vec![
            read_row(1, &departure, None),
            read_row(2, &arrival, None),
            read_row(
                3,
                &elsewhere,
                Some(Answer::ReceivedFromOwnAccount { from: main }),
            ),
        ];
        let mirrors = mirrored_rows(session, &rows, &questions);
        assert!(
            mirrors.open.is_empty() && mirrors.settled.is_empty(),
            "the fixture is meant to be one the assessment refuses to pair: {mirrors:?}"
        );

        assert!(
            mirrored_movements_of(session, &observations, &questions).is_empty(),
            "the queue pairs two rows the assessment refuses to pair, and hides \
             the arrival's own open question behind the pairing"
        );
    }

    /// A row this build cannot read is an account, not nothing (`iaam-y5ww`).
    ///
    /// Dropping it shrinks the set of accounts the session covers, which makes
    /// «every row of this session is on one account» *more* likely — the
    /// narrower of the two sentences, and false wherever the row it dropped was
    /// on a second account.
    #[test]
    fn a_row_this_build_cannot_read_is_not_a_session_of_one_account() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let savings = account(2);
        let (departure, _) = two_legs(main, savings);
        let unreadable = ImportObservationView {
            row: 2,
            row_key: None,
            concluded: false,
            payload: "{".to_owned(),
            answer: None,
        };
        let observations = vec![stored_row(1, &departure), unreadable];
        let questions = vec![stored_question_about(
            1,
            &Question::UnresolvedDirection {
                account: main,
                stated: departure.source_kind.clone(),
                counterparty: None,
            },
        )];

        assert_eq!(
            mirrored_movements_of(session, &observations, &questions).get(&1),
            Some(&OneMovement::NoCounterpart(NoCounterpart::SeveralAccounts)),
            "a session holding a row nobody can read is told every row of it is \
             on one account"
        );
    }

    /// The sentence does not deny a row an earlier pairing spent (`iaam-y5ww`).
    ///
    /// [`why_unpaired`] classifies the leftover of several identical sides as
    /// [`Unpaired::NoCounterpart`], and in that case a row of the document
    /// **is** the opposite half of this one — it was spent on an earlier
    /// pairing. A sentence saying no row of the document is that half denies a
    /// row the document printed.
    #[test]
    fn the_no_counterpart_sentence_does_not_deny_a_row_an_earlier_pairing_spent() {
        let session = ImportSessionId::new_random();
        let main = account(1);
        let savings = account(2);
        let day = date!(2025 - 04 - 10);
        // Two identical departures on one account and one arrival on the other,
        // each printed under the source's own word for a movement internal to
        // itself and naming nobody. The pairing spends the arrival on the first
        // departure; the second is the leftover this sentence is about.
        let departure = anonymous(directionless(row(main, "Anything", Some(day))), -1_000);
        let arrival = anonymous(directionless(row(savings, "Anything", Some(day))), 1_000);
        let observations = vec![
            stored_row(1, &departure),
            stored_row(2, &departure),
            stored_row(3, &arrival),
        ];
        let asked = |account| Question::UnresolvedDirection {
            account,
            stated: Some("INNER".to_owned()),
            counterparty: None,
        };
        let questions = vec![
            stored_question_about(1, &asked(main)),
            stored_question_about(2, &asked(main)),
            stored_question_about(3, &asked(savings)),
        ];
        let movements = mirrored_movements_of(session, &observations, &questions);
        assert_eq!(
            movements.get(&2),
            Some(&OneMovement::NoCounterpart(NoCounterpart::SeveralAccounts)),
            "the second departure is the leftover of two identical sides: {movements:?}"
        );

        let said = NoCounterpart::SeveralAccounts.reported();
        assert!(
            !said.contains("is the opposite half of the same amount on the same day"),
            "the arrival on the other account *is* the opposite half of this row; \
             what the document does not hold is a second one still free to be \
             it: {said}"
        );
        assert!(
            said.contains("available"),
            "the sentence says what `Unpaired::NoCounterpart` means — no row of \
             this document is available to be its other half: {said}"
        );
    }

    /// A pair identifier is a function of the session and the two rows.
    ///
    /// Not a minted value: the assessment's revision stamp is computed over the
    /// interpretation, so a random identifier here would move it under a
    /// session nobody had touched.
    #[test]
    fn the_identifier_of_a_pair_is_the_same_on_two_readings_of_one_session() {
        let session = ImportSessionId::new_random();
        assert_eq!(pair_identity(session, 1, 2), pair_identity(session, 1, 2));
        assert_ne!(pair_identity(session, 1, 2), pair_identity(session, 1, 3));
        assert_ne!(
            pair_identity(session, 1, 2),
            pair_identity(ImportSessionId::new_random(), 1, 2)
        );
    }

    /// A movement the source settled without naming a far side is not a side.
    ///
    /// Those post one signed leg each and count nothing twice. Collapsing one
    /// into the other would destroy a leg the journal correctly holds; relating
    /// them is `iaam-9ck1`'s pairing, which the owner confirms.
    #[test]
    fn two_own_account_movements_are_not_one_movement_printed_twice() {
        let main = account(1);
        let savings = account(2);
        let (mut departure, mut arrival) = two_legs(main, savings);
        departure.far_side = FarSide::OwnAccount;
        arrival.far_side = FarSide::OwnAccount;
        let resolver = resolver(vec![detail(main, "Main"), detail(savings, "Savings")]);
        let rows: Vec<ReadRow> = [(1, &departure), (2, &arrival)]
            .into_iter()
            .map(|(number, observed)| {
                let Assessment::Settled {
                    classification,
                    movement,
                    ..
                } = resolver.assess(observed)
                else {
                    panic!("the source asserted the far side, so nothing is asked");
                };
                let operation = observed
                    .resolve(classification, movement)
                    .expect("it resolves");
                let event = normalize(
                    &operation,
                    &NormalizationContext {
                        owner: OwnerId(uuid::Uuid::from_bytes([9; 16])),
                        source: SourceId(uuid::Uuid::from_bytes([9; 16])),
                        parser_version: ParserVersion("ingest/manual/1".to_owned()),
                    },
                )
                .expect("it normalises")
                .event;
                ReadRow {
                    row: number,
                    intake: Some(Intake::Observed {
                        row: Box::new(observed.clone()),
                        reader: None,
                    }),
                    operation: None,
                    row_key: None,
                    payload: String::new(),
                    candidate: Some(Ok(event)),
                    settled: None,
                    basis: Some(FactBasis::SourceAsserted),
                }
            })
            .collect();
        let mirrors = mirrored_rows(ImportSessionId::new_random(), &rows, &[]);
        assert!(mirrors.settled.is_empty(), "{mirrors:?}");
        assert!(mirrors.open.is_empty());
    }

    /// A source that asserts its own far side is not spelt like a rule.
    #[test]
    fn a_fact_says_whether_a_source_asserted_it_or_a_rule_settled_it() {
        let main = account(1);
        let mut asserted = directionless(row(main, "Anything", None));
        asserted.counterparty = ObservedCounterparty::Unknown;
        asserted.far_side = FarSide::OwnAccount;
        let resolver = resolver(vec![detail(main, "Main")]);
        let Assessment::Settled { basis, .. } = resolver.assess(&asserted) else {
            panic!("the source settled it");
        };
        assert_eq!(FactBasis::of(&basis), FactBasis::SourceAsserted);
        assert_eq!(FactBasis::SourceAsserted.code(), "source_asserted");
        assert_ne!(FactBasis::SourceAsserted, FactBasis::Directory);
    }

    // -----------------------------------------------------------------------
    // The standing decisions a first import offers (iaam-qn6d, decision 0029)
    // -----------------------------------------------------------------------

    /// One row of an import, as the session stored it.
    fn stored_row(row: u32, observed: &ObservedRow) -> ImportObservationView {
        ImportObservationView {
            row,
            row_key: None,
            concluded: false,
            payload: serde_json::to_string(&Intake::Observed {
                row: Box::new(observed.clone()),
                reader: None,
            })
            .expect("an intake"),
            answer: None,
        }
    }

    /// One open question about that row, in the shape the assessment reads.
    fn open_about(row: u32) -> OpenQuestion {
        OpenQuestion {
            row,
            question: ImportQuestionId::new_random(),
            prompt: String::new(),
            printed: None,
            alternatives: Vec::new(),
            alike: Vec::new(),
            pair: None,
        }
    }

    fn filed_under(mut observed: ObservedRow, category: &str) -> ObservedRow {
        observed.source_category = Some(category.to_owned());
        observed
    }

    /// The same row, with the word the **owner himself** filed it under there.
    fn owner_filed_under(mut observed: ObservedRow, category: &str) -> ObservedRow {
        observed.owner_category = Some(category.to_owned());
        observed
    }

    /// The word the owner himself filed rows under grounds an offer of its own,
    /// asked once for the word rather than once for each row carrying it.
    ///
    /// **This is the worst question this system asks, and this is its end.** The
    /// category is his decision, already made and recorded at his institution;
    /// the export prints it back on every row he took it on; and until the
    /// profile read the column he was asked, once per row, for what he had
    /// already told his bank. One question per distinct value is the reach one
    /// answer already has over a set — a handful of questions where there were
    /// as many as there are rows.
    ///
    /// **The offer is on his word and the bank's word both**, because they are
    /// two different conditions over two different vocabularies and either may
    /// be the one he wants. Each offer covers exactly the open rows its own
    /// condition matches, which is what makes the two comparable rather than
    /// rivalrous.
    #[test]
    fn the_word_the_owner_filed_rows_under_is_offered_once_for_the_word() {
        let main = account(1);
        let observations = vec![
            stored_row(
                1,
                &owner_filed_under(
                    filed_under(row(main, "Shop One", None), "Groceries"),
                    "Mine",
                ),
            ),
            stored_row(
                2,
                &owner_filed_under(
                    filed_under(row(main, "Shop Two", None), "Groceries"),
                    "Mine",
                ),
            ),
            stored_row(
                3,
                &owner_filed_under(
                    filed_under(row(main, "Shop Three", None), "Groceries"),
                    "Ours",
                ),
            ),
        ];
        let open: Vec<OpenQuestion> = (1..=3).map(open_about).collect();
        let offered = offers(&observations, &open).offered;

        // Three rows, one word of the bank's and two of his: three offers and
        // never three questions per row.
        let his: Vec<&OfferedRule> = offered
            .iter()
            .filter(|offer| offer.matcher.owner_category.is_some())
            .collect();
        assert_eq!(his.len(), 2, "one offer per word of his: {offered:?}");
        assert_eq!(his[0].matcher.owner_category.as_deref(), Some("Mine"));
        assert_eq!(his[0].covers, vec![1, 2]);
        assert_eq!(
            his[0].matcher.source_category, None,
            "his word is not the bank's word and the condition says which it asks about"
        );
        assert_eq!(his[1].matcher.owner_category.as_deref(), Some("Ours"));
        assert_eq!(his[1].covers, vec![3]);

        // And the bank's own word still grounds its own offer beside them.
        let theirs: Vec<&OfferedRule> = offered
            .iter()
            .filter(|offer| offer.matcher.source_category.is_some())
            .collect();
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].covers, vec![1, 2, 3]);

        // The question is put in his register and quotes his own word, because
        // it is his: asking him what his bank called it would be asking him
        // about a word he did not choose.
        assert!(his[0].question.ask.contains("«Mine»"), "{:?}", his[0]);
    }

    /// A word of his whose rows are not one thing is withheld, and the withheld
    /// entry says whose word it was.
    ///
    /// Without that, a caller shows him «your statement files these under
    /// «Mine»» — which is false twice over: the statement did not file them, he
    /// did, and the sentence would hand his own decision back to him as the
    /// bank's.
    #[test]
    fn a_word_of_his_whose_rows_disagree_is_withheld_and_named_as_his() {
        let main = account(1);
        let mut arrived = row(main, "Someone", None);
        arrived.direction = ObservedDirection::In;
        arrived.amount_minor = 1_000;
        let observations = vec![
            stored_row(1, &owner_filed_under(row(main, "Shop One", None), "Mine")),
            stored_row(2, &owner_filed_under(arrived, "Mine")),
        ];
        let offers = offers(&observations, &[open_about(1), open_about(2)]);
        assert!(offers.offered.is_empty(), "{:?}", offers.offered);
        assert_eq!(offers.withheld.len(), 1);
        let withheld = &offers.withheld[0];
        assert_eq!(withheld.filed_under, "Mine");
        assert_eq!(withheld.filed_by, FiledBy::Owner);
        assert_eq!(withheld.covers, vec![1, 2]);
        assert!(
            withheld.reason.contains("You file"),
            "the sentence says it was his own filing: {}",
            withheld.reason
        );
    }

    /// A statement's own categories are offered once each, whatever the parties.
    ///
    /// The complaint this answers: a first import asks about every row, two
    /// thirds of them literal repeats, and the field that would settle them is
    /// transcribed and read by nothing. Three shops filed under one word are one
    /// decision, and the offer says so before any of the three is asked about.
    #[test]
    fn one_word_the_source_filed_rows_under_is_offered_once_however_many_shops_it_covers() {
        let main = account(1);
        let observations = vec![
            stored_row(1, &filed_under(row(main, "Shop One", None), "Groceries")),
            stored_row(2, &filed_under(row(main, "Shop Two", None), "Groceries")),
            stored_row(3, &filed_under(row(main, "Shop One", None), "Groceries")),
            stored_row(4, &filed_under(row(main, "Shop Three", None), "Travel")),
        ];
        let open: Vec<OpenQuestion> = (1..=4).map(open_about).collect();
        let offered = offers(&observations, &open).offered;
        assert_eq!(offered.len(), 2, "two words, two decisions: {offered:?}");
        assert_eq!(
            offered[0].matcher.source_category.as_deref(),
            Some("Groceries"),
            "the offer that settles the most open rows is read first"
        );
        assert_eq!(offered[0].covers, vec![1, 2, 3]);
        assert_eq!(offered[1].covers, vec![4]);
    }

    /// An offer states the condition and never the outcome.
    ///
    /// Decision 0019 §6 refuses a map from a source's category to one of the
    /// owner's classifications, on the ground that such a map is frozen into
    /// every fact at import. An offer that filled in the outcome would be that
    /// map written a step later, so the only thing this may publish is what the
    /// rows have in common — and one field of it, which is decision 0008's
    /// number.
    #[test]
    fn an_offer_says_what_the_rows_have_in_common_and_never_what_they_are() {
        let main = account(1);
        let observations = vec![stored_row(
            1,
            &filed_under(row(main, "Shop One", None), "Groceries"),
        )];
        let offered = offers(&observations, &[open_about(1)]).offered;
        let matcher = &offered[0].matcher;
        assert_eq!(matcher.source_category.as_deref(), Some("Groceries"));
        assert_eq!(matcher.counterparty_account, None);
        assert_eq!(matcher.kind, None);
        assert_eq!(matcher.description_contains, None);
    }

    /// A document that prints no category of its own offers nothing.
    ///
    /// The falsification for an offer built out of whatever field is to hand: a
    /// row here names a counterparty and a source word, and neither is a word
    /// the institution filed the row under. Offering a condition on one of them
    /// would be one decision per shop, which is the count this exists to reduce.
    #[test]
    fn a_document_that_files_its_rows_under_nothing_offers_nothing() {
        let main = account(1);
        let observations = vec![stored_row(1, &row(main, "Shop One", None))];
        let offers = offers(&observations, &[open_about(1)]);
        assert!(offers.offered.is_empty());
        assert!(
            offers.withheld.is_empty(),
            "and no offer is withheld either: there is no word to withhold one on"
        );
    }

    /// A row nobody is still being asked about is not evidence of a decision.
    ///
    /// Only open questions are counted. Counting settled rows would make an
    /// offer grow every month while settling nothing new, and would tell the
    /// owner that a word he has already decided about is still outstanding.
    #[test]
    fn a_row_no_question_is_open_about_is_not_counted_towards_an_offer() {
        let main = account(1);
        let observations = vec![
            stored_row(1, &filed_under(row(main, "Shop One", None), "Groceries")),
            stored_row(2, &filed_under(row(main, "Shop Two", None), "Groceries")),
        ];
        let offered = offers(&observations, &[open_about(2)]).offered;
        assert_eq!(offered[0].covers, vec![2], "{offered:?}");
    }

    /// One shape a word's rows can all have, for a test about the sentence.
    ///
    /// The rows themselves are not the subject here — the sentence is — so the
    /// list is empty and the two attributes that decide the wording are stated.
    fn shape_of(movement: Option<Movement>, counterparty_named: bool) -> RowShape {
        RowShape {
            movement,
            counterparty_named,
            rows: Vec::new(),
        }
    }

    /// The offer is put to the owner in his words, and says what turns on it.
    ///
    /// Decision 0027's register, checked the mechanical way it is checked in the
    /// queue: no field name, no word that exists only because of how this is
    /// built, and a consequence that says what differs between answering one way
    /// and another rather than that the decision is his.
    #[test]
    fn an_offer_is_worded_for_a_person_and_says_what_his_answer_changes() {
        let question = offered_rule_question(
            FiledBy::Source,
            "Groceries",
            3,
            &shape_of(Some(Movement::Out), true),
        );
        for internal in [
            "source_category",
            "matcher",
            "classification",
            "session",
            "row",
            "rule",
        ] {
            assert!(
                !question.ask.to_lowercase().contains(internal),
                "«{internal}» is our word, not his: {}",
                question.ask
            );
        }
        assert!(
            question.ask.contains("Groceries"),
            "he is asked about the word he can see on his statement: {}",
            question.ask
        );
        assert!(
            question.consequence.contains('3'),
            "what turns on the answer is how many lines it decides: {}",
            question.consequence
        );
        assert!(
            question.consequence.contains("wrongly"),
            "and what it costs to be wrong, which is the half that gets dropped: {}",
            question.consequence
        );
    }

    /// The offer does not confine the decision to the institution that sent
    /// these lines.
    ///
    /// Decision 0026 §4 refuses to scope a category condition to a source and
    /// argues it at length: the rule fires on any row any source files under
    /// exactly that word. The sentence used to say «the same institution», which
    /// described a narrower standing decision than the one he was making — and
    /// the whole purpose of the consequence clause is that he reads what he is
    /// actually deciding.
    #[test]
    fn an_offer_says_the_decision_is_not_held_to_the_institution_that_sent_the_lines() {
        let question = offered_rule_question(
            FiledBy::Source,
            "Groceries",
            3,
            &shape_of(Some(Movement::Out), true),
        );
        assert!(
            !question.consequence.contains("the same institution"),
            "which is what it used to promise and is not what the rule does: {}",
            question.consequence
        );
        assert!(
            question
                .consequence
                .contains("does not stop at this institution"),
            "and it says so, because he is deciding for every source: {}",
            question.consequence
        );
    }

    /// Where the lines say which way the money went, every answer finishes them.
    ///
    /// So there is no caveat, and its absence is asserted: a sentence that
    /// warned about direction on rows that state one would be teaching him to
    /// skip the warning that matters.
    #[test]
    fn an_offer_on_lines_that_state_a_direction_carries_no_caveat_about_direction() {
        let question = offered_rule_question(
            FiledBy::Source,
            "Groceries",
            3,
            &shape_of(Some(Movement::Out), true),
        );
        assert!(
            !question.consequence.contains("which way the money went"),
            "these lines say which way, so nothing about direction is open: {}",
            question.consequence
        );
    }

    /// Where they do not, the one answer that leaves them waiting is named.
    ///
    /// `Classification::ExternalFlow` carries no direction of its own and the
    /// rows carry none either, so a rule stating it settles the classification
    /// and leaves every row at `Question::UnresolvedDirection` — still waiting,
    /// after the one act that was supposed to end the waiting. The other four
    /// outcomes the offer names decide a direction themselves.
    #[test]
    fn an_offer_on_lines_that_state_no_direction_says_which_answer_does_not_finish_them() {
        let question =
            offered_rule_question(FiledBy::Source, "Groceries", 3, &shape_of(None, false));
        assert!(
            question.consequence.contains("which way the money went"),
            "the lines do not say, and that decides whether the offer keeps its \
             promise: {}",
            question.consequence
        );
        assert!(
            question.consequence.contains("«money you spent»"),
            "and the answer that would leave them waiting is named, in the words \
             the question itself put to him: {}",
            question.consequence
        );
    }

    /// An answer that does not fit the lines settles none of them and costs him
    /// nothing.
    ///
    /// One [`RowShape`] means one outcome for all of them: `ObservedRow::resolve`
    /// refuses a fee that arrived and income that left, so a mismatched rule
    /// leaves every row of the group exactly where it was. That is the honest
    /// shape of the risk and it is stated, because «settles all of them at once»
    /// read alone invites him to expect the rows to move whatever he says.
    #[test]
    fn an_offer_says_a_wrong_answer_settles_none_of_the_lines_rather_than_some() {
        let question = offered_rule_question(
            FiledBy::Source,
            "Groceries",
            3,
            &shape_of(Some(Movement::In), false),
        );
        assert!(
            question.consequence.contains("settles none of them"),
            "all or none, never some: {}",
            question.consequence
        );
    }

    // -----------------------------------------------------------------------
    // A word that holds more than one thing (iaam-xchm, decision 0032)
    // -----------------------------------------------------------------------

    /// A word covering rows that ran both ways offers no rule at all.
    ///
    /// The complaint: one word of a real export covered a large share of the
    /// document and held at least four incompatible things, and the offer said
    /// nothing about it. One rule there would have been wrong for most of what it
    /// matched — a confident recommendation to make one wrong standing decision
    /// instead of many right ones, which is the failure the offer exists to
    /// prevent.
    #[test]
    fn a_word_covering_money_that_arrived_and_money_that_left_is_offered_as_no_rule() {
        let main = account(1);
        let observations = vec![
            stored_row(1, &filed_under(row(main, "Shop One", None), "Transfer")),
            stored_row(2, &filed_under(row(main, "Shop Two", None), "Transfer")),
            stored_row(
                3,
                &filed_under(arriving(row(main, "Shop Three", None)), "Transfer"),
            ),
        ];
        let open: Vec<OpenQuestion> = (1..=3).map(open_about).collect();
        let offers = offers(&observations, &open);
        assert!(
            offers.offered.is_empty(),
            "one rule on this word would file the arrival as a departure: {:?}",
            offers.offered
        );
        let withheld = &offers.withheld[0];
        assert_eq!(withheld.filed_under, "Transfer");
        assert_eq!(withheld.filed_by, FiledBy::Source);
        assert_eq!(withheld.covers, vec![1, 2, 3]);
        assert_eq!(withheld.contains.len(), 2, "{:?}", withheld.contains);
        assert_eq!(
            withheld.contains[0].rows,
            vec![1, 2],
            "the largest share first, so a caller shows the biggest group first"
        );
        assert_eq!(withheld.contains[1].rows, vec![3]);
    }

    /// The direction is not the only thing that splits a word.
    ///
    /// A statement that names a party on some of its lines and nobody on others
    /// is asking two different questions about them — whether the far side is one
    /// of his accounts, and whether the money that left was a fee — and one rule
    /// cannot answer both.
    #[test]
    fn a_word_covering_rows_that_named_a_party_and_rows_that_named_nobody_is_two_things() {
        let main = account(1);
        let observations = vec![
            stored_row(1, &filed_under(row(main, "Shop One", None), "Transfer")),
            stored_row(
                2,
                &filed_under(anonymous(row(main, "Shop One", None), -1_000), "Transfer"),
            ),
        ];
        let open: Vec<OpenQuestion> = (1..=2).map(open_about).collect();
        let offers = offers(&observations, &open);
        assert!(offers.offered.is_empty());
        assert_eq!(offers.withheld[0].contains.len(), 2);
        assert!(
            offers.withheld[0]
                .contains
                .iter()
                .any(|shape| shape.counterparty_named)
        );
        assert!(
            offers.withheld[0]
                .contains
                .iter()
                .any(|shape| !shape.counterparty_named)
        );
    }

    /// A word that is one thing is still offered, and says so.
    ///
    /// The falsification for withholding everything. Two shops the statement
    /// files under one word, both outward, both named, are one decision — and the
    /// offer both makes it and publishes the shape it is claiming, so its reader
    /// can check the claim instead of taking it.
    #[test]
    fn a_word_whose_rows_agree_is_offered_and_publishes_the_shape_it_claims() {
        let main = account(1);
        let observations = vec![
            stored_row(1, &filed_under(row(main, "Shop One", None), "Groceries")),
            stored_row(2, &filed_under(row(main, "Shop Two", None), "Groceries")),
        ];
        let open: Vec<OpenQuestion> = (1..=2).map(open_about).collect();
        let offers = offers(&observations, &open);
        assert!(offers.withheld.is_empty());
        let offered = &offers.offered[0];
        assert_eq!(offered.covers, vec![1, 2]);
        assert_eq!(offered.contains.rows, offered.covers);
        assert_eq!(offered.contains.movement, Some(Movement::Out));
        assert!(offered.contains.counterparty_named);
    }

    /// Every row an offer covers is a row the offer's own condition matches.
    ///
    /// The invariant that forbids narrowing the group by direction. A matcher
    /// carries no direction — deliberately, because a rule fires on rows nobody
    /// has looked at — so a group narrowed to one direction would publish a
    /// `covers` list the condition beside it does not agree with, and «what would
    /// this rule match» would answer something else.
    #[test]
    fn an_offer_covers_exactly_the_open_rows_its_own_condition_matches() {
        let main = account(1);
        let rows = [
            filed_under(row(main, "Shop One", None), "Groceries"),
            filed_under(arriving(row(main, "Shop Two", None)), "Travel"),
            filed_under(row(main, "Shop Three", None), "Groceries"),
        ];
        let observations: Vec<ImportObservationView> = rows
            .iter()
            .enumerate()
            .map(|(index, observed)| {
                stored_row(u32::try_from(index).expect("a row number") + 1, observed)
            })
            .collect();
        let open: Vec<OpenQuestion> = (1..=3).map(open_about).collect();
        for offered in offers(&observations, &open).offered {
            let matched: Vec<u32> = rows
                .iter()
                .enumerate()
                .filter(|(_, observed)| offered.matcher.matches(&observed.subject(None)))
                .map(|(index, _)| u32::try_from(index).expect("a row number") + 1)
                .collect();
            assert_eq!(
                offered.covers, matched,
                "the offer claims rows its own condition does not match: {offered:?}"
            );
        }
    }

    /// The withheld offer says why, in words a person can read.
    ///
    /// It is a statement and not a question — nothing is being asked, so there is
    /// no consequence to state — but decision 0027's other two obligations hold,
    /// because a caller that shows it shows it to him.
    #[test]
    fn an_offer_withheld_says_why_without_a_word_of_ours() {
        let reason = withheld_offer_reason(
            FiledBy::Source,
            "Transfer",
            &[
                RowShape {
                    movement: Some(Movement::Out),
                    counterparty_named: true,
                    rows: vec![1, 2],
                },
                RowShape {
                    movement: Some(Movement::In),
                    counterparty_named: true,
                    rows: vec![3],
                },
            ],
        );
        for internal in ["source_category", "matcher", "shape", "row", "session"] {
            assert!(
                !reason.to_lowercase().contains(internal),
                "«{internal}» is our word, not his: {reason}"
            );
        }
        assert!(
            reason.contains("Transfer") && reason.contains('3'),
            "he is told which word and how many of his lines it covers: {reason}"
        );
        assert!(
            reason.contains("not all the same thing"),
            "and why nothing is offered on it: {reason}"
        );
    }

    // -----------------------------------------------------------------------
    // A question published with the row it is about (iaam-pm4w, decision 0032)
    // -----------------------------------------------------------------------

    /// The values the sentence was built from travel beside the sentence.
    ///
    /// The complaint: an agent that had to show the owner what a group of
    /// questions contained pulled the date, the amount, the direction and the
    /// party back out of the prose with regular expressions — the act
    /// `docs/import-boundary.md` refuses one level down, committed against this
    /// engine's own output.
    #[test]
    fn a_question_publishes_the_row_it_is_about_and_not_only_the_sentence() {
        let main = account(1);
        let day = date!(2026 - 03 - 04);
        let asked = Question::IsTransferInternal {
            account: main,
            counterparty: "Shop One".to_owned(),
        };
        let observations = vec![stored_row(
            1,
            &owner_filed_under(
                filed_under(row(main, "Shop One", Some(day)), "Transfer"),
                "Mine",
            ),
        )];
        let questions = vec![stored_question_about(1, &asked)];
        let open = open_questions(
            &no_accounts(),
            &observations,
            &questions,
            &QuestionSettlements::default(),
        );
        let printed = open[0].printed.as_ref().expect("the row it is about");
        assert_eq!(printed.account, main);
        assert_eq!(printed.date, Some(day));
        assert_eq!(
            printed.amount_minor, -1_000,
            "with the sign the source printed, and never the one the journal would post"
        );
        assert_eq!(printed.currency, CurrencyCode::Rub);
        assert_eq!(printed.movement, Some(Movement::Out));
        assert_eq!(printed.counterparty.as_deref(), Some("Shop One"));
        assert_eq!(printed.source_category.as_deref(), Some("Transfer"));
        // Both words, because both ground a group: a caller reading an offer or
        // a withheld entry keyed on the owner's own word can see which open row
        // carries it without joining two lists by row number, exactly as it can
        // for the institution's.
        assert_eq!(printed.owner_category.as_deref(), Some("Mine"));
    }

    /// A row the source stated no direction for says so, and does not guess.
    ///
    /// The falsification for reading the direction off the sign: this row's
    /// amount is positive and the source stated nothing, which is exactly the
    /// condition `UnresolvedDirection` is asked under. A published direction here
    /// would answer the question the question exists to ask.
    #[test]
    fn a_row_the_source_gave_no_direction_publishes_none_whatever_its_sign() {
        let main = account(1);
        let asked = Question::UnresolvedDirection {
            account: main,
            stated: Some("INNER".to_owned()),
            counterparty: None,
        };
        let observed = directionless(anonymous(row(main, "Shop One", None), -1_000));
        assert!(observed.amount_minor > 0);
        let open = open_questions(
            &no_accounts(),
            &[stored_row(1, &observed)],
            &[stored_question_about(1, &asked)],
            &QuestionSettlements::default(),
        );
        let printed = open[0].printed.as_ref().expect("the row it is about");
        assert_eq!(printed.movement, None);
        assert!(
            printed.amount_minor > 0,
            "and the sign is still what it was"
        );
        assert_eq!(printed.counterparty, None);
        assert_eq!(printed.date, None, "an undated row is published as undated");
    }

    /// A question whose row this build cannot read is still a question.
    ///
    /// One absence and not six. The stored sentence and the stored alternatives
    /// are what such a question is answered by, and they are unaffected — the
    /// same tolerance `alike` already applies.
    #[test]
    fn a_question_whose_row_cannot_be_read_publishes_no_row_and_stays_answerable() {
        let main = account(1);
        let asked = Question::IsOutflowAFee { account: main };
        let mut stored = stored_row(1, &row(main, "Shop One", None));
        stored.payload = "{\"not\":\"an intake\"}".to_owned();
        let open = open_questions(
            &no_accounts(),
            &[stored],
            &[stored_question_about(1, &asked)],
            &QuestionSettlements::default(),
        );
        assert_eq!(open[0].printed, None);
        assert!(
            !open[0].alternatives.is_empty(),
            "and the words that answer it are still there"
        );
    }

    // -----------------------------------------------------------------------
    // Which accounts an answer may name (iaam-7iyg, decision 0032)
    // -----------------------------------------------------------------------

    /// The directory is published once, not once per question.
    ///
    /// The complaint: the assessment said an answer must name an account and
    /// never which accounts exist, so a caller working a session of hundreds of
    /// questions made one extra call per question to learn a list that is
    /// identical every time.
    #[test]
    fn the_accounts_an_answer_may_name_are_published_once_for_the_whole_assessment() {
        let main = account(1);
        let savings = account(2);
        let directory =
            AccountDirectory::from_accounts(vec![detail(main, "Main"), detail(savings, "Savings")]);
        let asked = Question::IsTransferInternal {
            account: main,
            counterparty: "Shop One".to_owned(),
        };
        let observations = vec![
            stored_row(1, &row(main, "Shop One", None)),
            stored_row(2, &row(main, "Shop Two", None)),
        ];
        let questions = vec![
            stored_question_about(1, &asked),
            stored_question_about(2, &asked),
        ];
        let open = open_questions(
            &directory,
            &observations,
            &questions,
            &QuestionSettlements::default(),
        );
        let accounts = answer_accounts(&directory, &open);
        assert_eq!(accounts.len(), 2, "{accounts:?}");
        assert!(
            accounts.iter().any(|candidate| candidate.id == main),
            "including the one the rows are on: the list is not about one row, \
             and every question says which account it is on"
        );
        assert_eq!(
            open[0].printed.as_ref().expect("the row").account,
            main,
            "which is what makes the exclusion one comparison rather than a lookup"
        );
    }

    /// A session no answer names an account for publishes no directory.
    ///
    /// The falsification for publishing the owner's accounts unconditionally.
    /// Neither answer to «was it a fee, or a payment out?» names an account, so
    /// there is nothing for the list to be for.
    #[test]
    fn a_session_whose_questions_name_no_account_publishes_no_accounts() {
        let main = account(1);
        let directory = AccountDirectory::from_accounts(vec![
            detail(main, "Main"),
            detail(account(2), "Savings"),
        ]);
        let asked = Question::IsOutflowAFee { account: main };
        let open = open_questions(
            &directory,
            &[stored_row(
                1,
                &anonymous(row(main, "Shop One", None), -1_000),
            )],
            &[stored_question_about(1, &asked)],
            &QuestionSettlements::default(),
        );
        assert!(
            open[0]
                .alternatives
                .iter()
                .copied()
                .all(|shape| !shape.needs_account()),
            "the premise: no word this question admits names an account"
        );
        assert!(answer_accounts(&directory, &open).is_empty());
    }

    // -----------------------------------------------------------------------
    // A question names its account in the owner's words (iaam-6jsj, 0035)
    // -----------------------------------------------------------------------

    /// The account a question is about is published as he calls it.
    ///
    /// The complaint: an agent working a real import read account identifiers
    /// out to him, because the identifier was the only thing beside the
    /// question — `answer_accounts` deliberately does not narrow to the account
    /// the row is on, so the title of the account a question is about could not
    /// be got from the assessment at all.
    #[test]
    fn a_question_names_the_account_it_is_about_in_the_owners_own_words() {
        let main = account(1);
        let mut held = detail(main, "Main");
        held.institution = Some("Bank One".to_owned());
        let directory = AccountDirectory::from_accounts(vec![held]);
        let asked = Question::IsOutflowAFee { account: main };
        let open = open_questions(
            &directory,
            &[stored_row(1, &row(main, "Shop One", None))],
            &[stored_question_about(1, &asked)],
            &QuestionSettlements::default(),
        );
        let printed = open[0].printed.as_ref().expect("the row it is about");
        assert_eq!(
            printed.account, main,
            "the identifier stays, because it is what the answering call takes"
        );
        assert_eq!(printed.title.as_deref(), Some("Main"));
        assert_eq!(
            printed.institution.as_deref(),
            Some("Bank One"),
            "two accounts he calls one word apart are not one question"
        );
    }

    /// An account his directory does not hold publishes no name, and not a uuid.
    ///
    /// The falsification. `AccountNames::title` answers this case with the
    /// identifier as a string, which is legible in a refusal and is exactly the
    /// defect here — a title a reader would read out that is not a name at all.
    /// A row may name an account by identifier that the directory has never
    /// held; `AccountResolution::missing` publishes it, and a question about it
    /// is still a question.
    #[test]
    fn a_row_on_an_account_the_directory_does_not_hold_publishes_no_title() {
        let main = account(1);
        let asked = Question::IsOutflowAFee { account: main };
        let open = open_questions(
            &no_accounts(),
            &[stored_row(1, &row(main, "Shop One", None))],
            &[stored_question_about(1, &asked)],
            &QuestionSettlements::default(),
        );
        let printed = open[0].printed.as_ref().expect("the row it is about");
        assert_eq!(printed.account, main, "which is still addressable");
        assert_eq!(printed.title, None);
        assert_ne!(
            printed.title.as_deref(),
            Some(main.inner().to_string().as_str()),
            "an absence, never the identifier rendered where a name belongs"
        );
        assert_eq!(printed.institution, None);
    }

    /// A pair says which two rows it is, and not only that it is a pair.
    ///
    /// The complaint: the identifier states that two questions are one decision
    /// and states nothing about which two, so a caller that wanted to put the
    /// decision once had to scan every other open question for a matching uuid
    /// before it could name the other row to anybody. `alike`, the larger
    /// relation, has published its rows outright since it existed.
    #[test]
    fn each_leg_of_one_movement_names_the_other_row_and_not_only_the_shared_identifier() {
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let mut rows = vec![read_row(1, &departure, None), read_row(2, &arrival, None)];
        let questions = vec![
            stored_question_about(1, &Question::IsOutflowAFee { account: main }),
            stored_question_about(2, &Question::IsInflowIncome { account: savings }),
        ];
        let mirrors = mirrored_rows(ImportSessionId::new_random(), &rows, &questions);
        let settled = settlements(&mut rows, &mirrors);
        let observations = vec![stored_row(1, &departure), stored_row(2, &arrival)];
        let open = open_questions(&no_accounts(), &observations, &questions, &settled);
        let (first, second) = (
            open[0].pair.expect("the departure is one side"),
            open[1].pair.expect("the arrival is the other"),
        );
        assert_eq!(first.id, second.id, "one decision, so one identifier");
        assert_eq!(
            first.row, open[1].row,
            "and each names the row it is not, so the decision can be put in words"
        );
        assert_eq!(second.row, open[0].row);
        assert_ne!(
            first.row, open[0].row,
            "never its own row, which would say a row pairs with itself"
        );
    }

    // -----------------------------------------------------------------------
    // A question a standing rule settles stops being open (iaam-m2oi)
    // -----------------------------------------------------------------------

    /// One row as [`SessionReading`] reads it: through [`resolution_of`],
    /// against the owner's directory and rules as they stand now.
    ///
    /// The one helper these tests share, because the whole claim under test is
    /// that «still waiting on him» is decided by the reading and by nothing
    /// else. A helper that set `settled` or `basis` by hand would assert
    /// against a state no reading produces.
    fn read_against(row: u32, observed: &ObservedRow, resolver: &Resolver) -> ReadRow {
        read_observation(stored_row(row, observed), observed, resolver)
    }

    /// The same reading, over a stored line the caller has already shaped —
    /// one carrying his answer, say, which `stored_row` deliberately does not.
    fn read_observation(
        observation: ImportObservationView,
        observed: &ObservedRow,
        resolver: &Resolver,
    ) -> ReadRow {
        let row = observation.row;
        let resolution = resolution_of(&observation, resolver);
        let settled = match &resolution {
            Ok(RowResolution::NoFact(reason)) => Some(*reason),
            Ok(RowResolution::Fact { .. }) | Err(_) => None,
        };
        let basis = match &resolution {
            Ok(RowResolution::Fact { basis, .. }) => Some(basis.clone()),
            Ok(RowResolution::NoFact(_)) | Err(_) => None,
        };
        let candidate = match resolution {
            Ok(RowResolution::Fact { operation, .. }) => Some(
                normalize(
                    &operation,
                    &NormalizationContext {
                        owner: OwnerId(uuid::Uuid::from_bytes([9; 16])),
                        source: SourceId(uuid::Uuid::from_bytes([9; 16])),
                        parser_version: ParserVersion("ingest/manual/1".to_owned()),
                    },
                )
                .map(|normalized| normalized.event),
            ),
            Ok(RowResolution::NoFact(_)) => None,
            Err(rejection) => Some(Err(rejection)),
        };
        ReadRow {
            row,
            intake: Some(Intake::Observed {
                row: Box::new(observed.clone()),
                reader: None,
            }),
            operation: None,
            row_key: None,
            payload: observation.payload,
            candidate,
            settled,
            basis,
        }
    }

    /// The standing decision the owner adopts when he takes the offer: anything
    /// this source filed under one of its own words is money that left the
    /// perimeter.
    fn rule_on_category(category: &str) -> ClassificationRule {
        ClassificationRule {
            id: iaam_core::ids::ClassificationRuleId::new_random(),
            version: 1,
            matcher: RuleMatcher {
                counterparty_account: None,
                description_contains: None,
                kind: None,
                source_category: Some(category.to_owned()),
                owner_category: None,
                source_code: None,
            },
            outcome: Classification::ExternalFlow,
        }
    }

    /// Adopting the rule the session offered stops the questions it covers from
    /// waiting on him.
    ///
    /// **The bead.** Questions are recorded at intake and the rule is written
    /// afterwards, so nothing about the stored question changes: `answered_at`
    /// stays empty for ever, because he never answered it. Read off that column
    /// the session was as blocked after the one act that was meant to replace
    /// hundreds of answers as it was before it, and the offer's own sentence
    /// told him the opposite.
    #[test]
    fn a_question_a_standing_rule_settles_no_longer_waits_on_the_owner() {
        let main = account(1);
        let observed = filed_under(
            row(main, "Shop One", Some(date!(2025 - 04 - 10))),
            "Groceries",
        );
        let questions = vec![stored_question_about(
            1,
            &Question::IsTransferInternal {
                account: main,
                counterparty: "Shop One".to_owned(),
            },
        )];
        let observations = vec![stored_row(1, &observed)];

        let before = ruled(vec![detail(main, "Main")], Vec::new());
        let mut rows = vec![read_against(1, &observed, &before)];
        let settled = settlements(&mut rows, &MirroredRows::default());
        assert_eq!(
            settled.awaiting(&questions),
            1,
            "with no rule the row is his to decide"
        );

        let after = ruled(
            vec![detail(main, "Main")],
            vec![rule_on_category("Groceries")],
        );
        let mut rows = vec![read_against(1, &observed, &after)];
        let settled = settlements(&mut rows, &MirroredRows::default());
        assert_eq!(
            settled.awaiting(&questions),
            0,
            "and with it the question is answered by his own standing decision"
        );
        assert!(
            open_questions(&no_accounts(), &observations, &questions, &settled).is_empty(),
            "so it is not published as one he still has to answer"
        );
    }

    /// The row is in one list and not in both.
    ///
    /// Decision 0032 §1 says the resolved rows and the open questions are
    /// disjoint by construction, and they were not: the row produced a planned
    /// fact and its stored question was published beside it, so a caller
    /// totalling the assessment counted one row as decided and as outstanding
    /// at once.
    #[test]
    fn a_row_a_rule_settles_produces_a_fact_and_no_open_question() {
        let main = account(1);
        let observed = filed_under(
            row(main, "Shop One", Some(date!(2025 - 04 - 10))),
            "Groceries",
        );
        let questions = vec![stored_question_about(
            1,
            &Question::IsTransferInternal {
                account: main,
                counterparty: "Shop One".to_owned(),
            },
        )];
        let resolver = ruled(
            vec![detail(main, "Main")],
            vec![rule_on_category("Groceries")],
        );
        let mut rows = vec![read_against(1, &observed, &resolver)];
        assert!(
            matches!(rows[0].candidate, Some(Ok(_))),
            "the row becomes a fact: {:?}",
            rows[0].candidate
        );
        let settled = settlements(&mut rows, &MirroredRows::default());
        assert!(
            open_questions(
                &no_accounts(),
                &[stored_row(1, &observed)],
                &questions,
                &settled
            )
            .is_empty(),
            "and is therefore in neither the other list"
        );
    }

    /// What settled it is the word the assessment puts on the same row.
    ///
    /// One vocabulary and not two: a caller that reads the question and the
    /// planned fact reads one determination twice.
    #[test]
    fn a_settled_question_names_the_rule_in_the_words_the_fact_uses() {
        let main = account(1);
        let observed = filed_under(
            row(main, "Shop One", Some(date!(2025 - 04 - 10))),
            "Groceries",
        );
        let question = stored_question_about(
            1,
            &Question::IsTransferInternal {
                account: main,
                counterparty: "Shop One".to_owned(),
            },
        );
        let resolver = ruled(
            vec![detail(main, "Main")],
            vec![rule_on_category("Groceries")],
        );
        let mut rows = vec![read_against(1, &observed, &resolver)];
        let settled = settlements(&mut rows, &MirroredRows::default());
        let settlement = settled
            .settlement_of(&question)
            .expect("the reading settled it");
        assert_eq!(settlement.code(), "rule");
        assert_eq!(
            settlement.code(),
            rows[0]
                .basis
                .as_ref()
                .expect("the row was settled into a fact")
                .code(),
            "the question and the fact say the same word about the same row"
        );
        assert!(
            question.answered_at.is_none(),
            "and it is still not a question he answered, which is the distinction"
        );
    }

    /// The mirror pass reaches the same readers through the same fold.
    ///
    /// Decision 0031's settlement was the first «settled by something other
    /// than its own answer», and it was filtered out of the published questions
    /// by a test of its own that no other reader shared. Both now go through
    /// [`QuestionSettlements`], so a reader that learns one learns the other.
    #[test]
    fn the_other_leg_of_one_movement_settles_a_question_through_the_same_fold() {
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let mut rows = vec![
            read_row(
                1,
                &departure,
                Some(Answer::SentToOwnAccount { to: savings }),
            ),
            read_row(2, &arrival, None),
        ];
        let question = stored_question_about(2, &Question::IsInflowIncome { account: savings });
        let mirrors = mirrored_rows(
            ImportSessionId::new_random(),
            &rows,
            std::slice::from_ref(&question),
        );
        let settled = settlements(&mut rows, &mirrors);
        assert_eq!(
            settled
                .settlement_of(&question)
                .expect("the other leg settled it")
                .code(),
            "second_leg_of_one_movement",
            "the word decision 0031 already had, and not a second one for questions"
        );
        assert_eq!(settled.awaiting(std::slice::from_ref(&question)), 0);
    }

    /// A movement his directory now recognises on both rows is still one fact.
    ///
    /// **The regression this decision could have caused, caught here.** Both
    /// rows carry a question recorded at intake, when the directory could not
    /// place the name the source printed. He then creates the account — or gives
    /// one of his an alias — and both rows resolve into complete transfers, each
    /// carrying a leg on each account, while their stored questions keep their
    /// empty `answered_at` for ever.
    ///
    /// The mirror pass used to read those questions to decide which side of a
    /// pair was already a fact, and answered «neither», so nothing was
    /// suppressed and both rows would have committed — the movement twice, which
    /// is what decision 0031 exists to prevent. It was hidden by the commit
    /// refusing over the two stale questions. It does not refuse over them any
    /// more, so the pass asks the reading instead, through the one predicate
    /// every other reader of «settled» asks.
    #[test]
    fn two_legs_the_directory_now_places_are_one_fact_although_both_questions_stand() {
        let main = account(1);
        let savings = account(2);
        let day = date!(2025 - 04 - 10);
        let departure = row(main, "Savings", Some(day));
        let arrival = arriving(row(savings, "Main", Some(day)));
        // The directory as it stands now: both names place, which is what makes
        // each row a complete transfer without a word from him.
        let directory = resolver(vec![detail(main, "Main"), detail(savings, "Savings")]);
        let rows = vec![
            read_against(1, &departure, &directory),
            read_against(2, &arrival, &directory),
        ];
        // The questions recorded before either name placed, and never answered.
        let questions = vec![
            stored_question_about(
                1,
                &Question::IsTransferInternal {
                    account: main,
                    counterparty: "Savings".to_owned(),
                },
            ),
            stored_question_about(
                2,
                &Question::IsTransferInternal {
                    account: savings,
                    counterparty: "Main".to_owned(),
                },
            ),
        ];

        let mirrors = mirrored_rows(ImportSessionId::new_random(), &rows, &questions);
        assert_eq!(
            mirrors.settled.get(&2),
            Some(&1),
            "the arrival records nothing: the departure already carries a leg on \
             each account"
        );
        assert!(
            mirrors.open.is_empty(),
            "and nothing is published as a decision he still has to make: {mirrors:?}"
        );

        let mut rows = rows;
        let settled = settlements(&mut rows, &mirrors);
        assert_eq!(
            settled.awaiting(&questions),
            0,
            "so the commit is not refused over two questions nothing needs"
        );
        assert_eq!(
            settled
                .settlement_of(&questions[1])
                .expect("the other leg settled it")
                .code(),
            "second_leg_of_one_movement"
        );
    }

    /// Retiring the rule puts the question back.
    ///
    /// **Why this is the right behaviour and not a regression.** The row was
    /// never decided by him; it was decided by a standing rule of his, and
    /// withdrawing the rule withdraws the decision. Something has to happen to
    /// the row, and the two candidates are «it comes back as a question» and
    /// «it stays settled by a rule that no longer exists» — the second is a
    /// decision nobody made still classifying his money.
    ///
    /// **And it is free, which is the argument for deciding this at read time.**
    /// Nothing compensates, nothing is un-retired, and `retire_rule` — which
    /// lives in another scenario and knows nothing about import sessions — needs
    /// no knowledge of them: the next reading finds no rule, the row assesses as
    /// ambiguous again, and the question is waiting again. A settlement recorded
    /// in the questions table would need a second write across a port boundary
    /// with no transaction spanning the two, which is the shape `iaam-77hk` was
    /// filed on.
    ///
    /// A row already committed is not this case and does not come back: it is a
    /// fact in the journal, and retiring the rule replans it as a correction the
    /// owner approves. The session it came from is closed and does not reopen.
    #[test]
    fn retiring_the_rule_that_settled_a_question_puts_the_question_back() {
        let main = account(1);
        let observed = filed_under(
            row(main, "Shop One", Some(date!(2025 - 04 - 10))),
            "Groceries",
        );
        let question = stored_question_about(
            1,
            &Question::IsTransferInternal {
                account: main,
                counterparty: "Shop One".to_owned(),
            },
        );
        // `Resolver::load` holds only the rules that are not retired, so a
        // retired rule is a rule that is not here.
        let retired = ruled(vec![detail(main, "Main")], Vec::new());
        let mut rows = vec![read_against(1, &observed, &retired)];
        let settled = settlements(&mut rows, &MirroredRows::default());
        assert_eq!(
            settled.awaiting(std::slice::from_ref(&question)),
            1,
            "the decision was withdrawn, so the row is his to decide again"
        );
        assert!(
            settled.settlement_of(&question).is_none(),
            "and nothing claims to have settled it"
        );
    }

    /// An answer he gave outranks a rule, and the question stays answered.
    ///
    /// `resolution_of` reads the stored answer before it consults the rules at
    /// all, so a row he spoke about is settled by his word whatever his rules
    /// say about it. The published settlement must not overwrite that: what he
    /// said is on the question, and this field is for the questions he never
    /// answered.
    #[test]
    fn a_question_he_answered_is_not_reported_as_settled_by_something_else() {
        let main = account(1);
        let observed = filed_under(
            row(main, "Shop One", Some(date!(2025 - 04 - 10))),
            "Groceries",
        );
        let mut question = stored_question_about(
            1,
            &Question::IsTransferInternal {
                account: main,
                counterparty: "Shop One".to_owned(),
            },
        );
        question.answered_at = Some("2026-03-02T00:00:00Z".to_owned());
        question.answer = Some(serde_json::to_string(&Answer::Paid).expect("an answer"));
        let resolver = ruled(
            vec![detail(main, "Main")],
            vec![rule_on_category("Groceries")],
        );
        let mut rows = vec![read_against(1, &observed, &resolver)];
        let settled = settlements(&mut rows, &MirroredRows::default());
        assert!(
            settled.settlement_of(&question).is_none(),
            "he answered it; nothing else has to explain why it stopped waiting"
        );
        assert_eq!(settled.awaiting(std::slice::from_ref(&question)), 0);
    }

    /// What a fact records about the rule that filed it (`iaam-k4qu`).
    ///
    /// Only one of the five bases names a rule, and it is the one whose rows the
    /// owner reviews as a group. The other four are not a gap: each of them is a
    /// reading that ran and found no rule of his, which is a statement the fact
    /// records rather than a silence. The silence is reserved for the facts no
    /// reading here produced at all.
    #[test]
    fn a_fact_records_the_rule_that_filed_it_and_says_so_when_none_did() {
        let rule = iaam_core::ids::ClassificationRuleId::new_random();
        assert_eq!(
            FactBasis::Rule { rule, version: 4 }.rule_settlement(),
            RuleSettlement::Rule { rule, version: 4 }
        );
        for basis in [
            FactBasis::Concluded,
            FactBasis::Directory,
            FactBasis::SourceAsserted,
            FactBasis::Answered,
        ] {
            assert_eq!(
                basis.rule_settlement(),
                RuleSettlement::NoRule,
                "a reading that found no rule says so: {basis:?}"
            );
        }
    }

    /// A basis that names a rule never records «no rule» (`iaam-r0qk`).
    ///
    /// «No rule» is a reading that ran and found none of his rules, and a basis
    /// naming a rule is a reading that ran and found one. Answering the first
    /// for the second tells him a fact one of his own standing decisions filed
    /// was decided by hand: it drops out of the group he asked to see, and a
    /// correction on it reports no standing decision behind the row.
    ///
    /// The basis used to carry the rule as text, and an identifier that would
    /// not parse was recorded as «no rule» — the one place this wave said the
    /// second of its three states when the first was true. There is no third
    /// answer to give instead: «nothing recorded» is the absence of the whole
    /// value and not a word this vocabulary can say. So the state is gone
    /// rather than handled, and this test is what says it is gone: the version
    /// of it that named an unreadable rule no longer compiles, because there is
    /// no unreadable rule to name.
    #[test]
    fn a_basis_naming_a_rule_records_that_rule_and_never_no_rule() {
        let rule = iaam_core::ids::ClassificationRuleId::new_random();
        let settlement = FactBasis::Rule { rule, version: 1 }.rule_settlement();
        assert_ne!(settlement, RuleSettlement::NoRule);
        assert_eq!(settlement, RuleSettlement::Rule { rule, version: 1 });
        assert_eq!(settlement.rule(), Some((rule, 1)));
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
            reader: None,
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
        row.source_category = None;
        row.description = None;
        row
    }

    #[test]
    fn an_open_question_has_generalised_nothing_yet() {
        let contents = session_with(row(account(1), "Savings", None), None, None);
        assert_eq!(
            generalisation_of(&contents.observations, &contents.questions[0]),
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
            generalisation_of(&contents.observations, &contents.questions[0]),
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
            generalisation_of(&contents.observations, &contents.questions[0])
        else {
            panic!("a row naming a counterparty generalises");
        };
        assert_eq!(
            matcher.counterparty_account.as_deref(),
            Some("Savings"),
            "the matcher asks what the row printed"
        );
        assert_eq!(
            outcome,
            Classification::ExternalFlow,
            "the outcome is the classification the answer settled the row as"
        );
    }

    /// The one answer that is not a claim about every row like it (iaam-axrf).
    ///
    /// The other seven say what a thing **was** and generalise. This one says
    /// what the document did not contain, which is a fact about one row: a
    /// standing decision made of it would file every later row of the same shape
    /// as an unplaceable movement, including the ones whose other half is in the
    /// export and which the pairing would have settled completely.
    ///
    /// The row here is the one that **could** ground a matcher — it prints a
    /// counterparty — so the state is not `Impossible`, and asserting that is
    /// the point: `Impossible` claims no rule can be built from the row under
    /// any token, and here the row is fine and the answer is the reason.
    #[test]
    fn an_answer_that_says_what_the_document_did_not_contain_generalises_into_nothing() {
        let contents = session_with(
            row(account(1), "Savings", None),
            Some(Answer::BetweenOwnAccounts),
            None,
        );
        let described = generalisation_of(&contents.observations, &contents.questions[0]);
        assert_eq!(described, Generalisation::DoesNotGeneralise);
        assert_eq!(described.code(), "does_not_generalise");
        assert_ne!(
            described,
            Generalisation::Impossible,
            "the row prints a counterparty, so a rule could have been built from it"
        );
    }

    // --- what a question says about the row (iaam-3ewp, iaam-pzm9) --------

    /// The defect, in the shape it was found in: one statement, several rows
    /// the source described with the same word and no counterparty, and four
    /// questions whose sentences were identical to the character.
    ///
    /// The owner matched question to row by counting down the list, got the
    /// offset wrong, and answered for rows he had not read. Nothing catches
    /// that afterwards: an answer the question admits is accepted, it settles
    /// the row, it may become a standing rule, and no later call asks again.
    ///
    /// Two rows here rather than four, because two is what the assertion needs:
    /// they agree on the account, the word, the currency and the absence of a
    /// counterparty, and differ only in the two things a person reads a
    /// statement by.
    #[test]
    fn two_rows_differing_only_in_date_and_amount_get_questions_that_tell_them_apart() {
        let main = account(1);
        let resolver = resolver(vec![detail(main, "Main")]);
        let first = anonymous(
            directionless(row(main, "", Some(date!(2026 - 03 - 04)))),
            1_000,
        );
        let second = anonymous(
            directionless(row(main, "", Some(date!(2026 - 03 - 19)))),
            4_250,
        );

        let question = Question::UnresolvedDirection {
            account: main,
            stated: Some("INNER".to_owned()),
            counterparty: None,
        };
        let one = resolver.render(&question, &first);
        let other = resolver.render(&question, &second);

        assert_ne!(
            one, other,
            "the two rows raised the same question, and the question is all the \
             owner is shown: if the sentences match he can only count"
        );
        assert!(
            one.contains("2026-03-04") && one.contains("10.00"),
            "the row is named by what a person finds it on the statement by: {one}"
        );
        assert!(
            other.contains("2026-03-19") && other.contains("42.50"),
            "and so is the other one: {other}"
        );
    }

    /// The amount is printed with the sign the source printed.
    ///
    /// Not normalised to a magnitude. This is a recognition aid — the owner is
    /// matching it against a line he is looking at — and
    /// [`ObservedRow::amount_minor`] exists precisely to keep what the source
    /// said. Where the question is about a direction the sign is not evidence
    /// of one, which is why the question is being asked at all; printing it as
    /// the source did says what the source said and stops.
    #[test]
    fn the_question_prints_the_amount_with_the_sign_the_source_printed() {
        let main = account(1);
        let resolver = resolver(vec![detail(main, "Main")]);
        let outgoing = anonymous(row(main, "", Some(date!(2026 - 03 - 04))), -1_000);
        let question = Question::IsOutflowAFee { account: main };

        let prompt = resolver.render(&question, &outgoing);
        assert!(prompt.contains("-10.00"), "{prompt}");
    }

    /// A row with no date says so instead of quietly losing the clause.
    ///
    /// Such a row exists — the commit refuses it for want of a date — and a
    /// sentence that simply omitted the date would read as a row dated nowhere
    /// in particular rather than as one the source dated nowhere at all.
    #[test]
    fn a_row_the_source_left_undated_says_so_rather_than_dropping_the_clause() {
        let main = account(1);
        let resolver = resolver(vec![detail(main, "Main")]);
        let undated = anonymous(directionless(row(main, "", None)), 1_000);
        let question = Question::UnresolvedDirection {
            account: main,
            stated: Some("INNER".to_owned()),
            counterparty: None,
        };

        let prompt = resolver.render(&question, &undated);
        assert!(
            prompt.contains("undated") && prompt.contains("10.00"),
            "the absence is stated and the amount still identifies the row: {prompt}"
        );
    }

    /// All four questions say that the answer decides something, not just that
    /// the row is unclear (iaam-pzm9).
    ///
    /// The clause is one sentence and identical on all four, and that is the
    /// decision: the consequences themselves are seven, one per alternative,
    /// and they are published attached to the words they belong to rather than
    /// gathered into the prompt — see [`AnswerShape::consequence`].
    #[test]
    fn every_question_says_that_the_answer_decides_a_figure_in_the_report() {
        let main = account(1);
        let resolver = resolver(vec![detail(main, "Main")]);
        let subject = anonymous(
            directionless(row(main, "", Some(date!(2026 - 03 - 04)))),
            1_000,
        );

        for question in [
            Question::IsTransferInternal {
                account: main,
                counterparty: "Shop One".to_owned(),
            },
            Question::IsOutflowAFee { account: main },
            Question::IsInflowIncome { account: main },
            Question::UnresolvedDirection {
                account: main,
                stated: Some("INNER".to_owned()),
                counterparty: None,
            },
        ] {
            let prompt = resolver.render(&question, &subject);
            assert!(
                prompt.contains("money-flow report"),
                "a question that does not say what turns on the answer asks the \
                 owner to choose blind: {prompt}"
            );
            assert!(
                prompt.contains("2026-03-04"),
                "and every one of the four names its row: {prompt}"
            );
        }
    }

    // --- what a proposed rule asks about (iaam-g7yc) -----------------------

    /// The defect: the proposal used to ask about every field the row offered,
    /// joined with «and», so the rule the owner adopted recognised the row he
    /// had just settled and practically nothing else.
    ///
    /// The row here prints a counterparty **and** a source word; the proposal
    /// takes the counterparty and leaves the word alone.
    #[test]
    fn a_proposed_rule_asks_about_the_counterparty_and_nothing_else() {
        let proposed = matcher_for(&row(account(1), "Shop One", None)).expect("a matcher");
        assert_eq!(proposed.counterparty_account.as_deref(), Some("Shop One"));
        assert_eq!(
            proposed.kind, None,
            "the source's word for the operation is not part of who the money \
             moved with"
        );
        assert_eq!(proposed.description_contains, None);
    }

    /// The point of the change, stated as behaviour rather than as fields: a
    /// second row from the same counterparty, which the source described
    /// differently, is recognised.
    ///
    /// Under the old matcher it was not — the description alone would have
    /// failed — and the rule the owner adopted was therefore a standing decision
    /// that settled one row he had already settled by hand.
    #[test]
    fn a_proposed_rule_recognises_the_next_row_from_the_same_counterparty() {
        let main = account(1);
        let mut settled = row(main, "Shop One", None);
        settled.description = Some("card purchase 0001".to_owned());
        let proposed = matcher_for(&settled).expect("a matcher");

        let mut later = row(main, "Shop One", None);
        later.description = Some("card purchase 0002".to_owned());
        later.source_kind = Some("card".to_owned());
        assert!(
            proposed.matches(&later.subject(None)),
            "same counterparty, different words: the rule is about who, not \
             about how the source spelled it"
        );
    }

    /// A row with no counterparty falls to the source's own word, which is the
    /// whole of what such a row carries.
    #[test]
    fn a_row_naming_nobody_generalises_on_the_word_the_source_used() {
        let mut anonymous = row(account(1), "Savings", None);
        anonymous.counterparty = ObservedCounterparty::Unknown;
        anonymous.description = Some("internal movement".to_owned());
        let proposed = matcher_for(&anonymous).expect("a matcher");
        assert_eq!(proposed.kind.as_deref(), Some("transfer"));
        assert_eq!(proposed.counterparty_account, None);
        assert_eq!(
            proposed.description_contains, None,
            "the description is the last resort, not an addition to the word"
        );
    }

    /// The last resort. It barely generalises — a whole description is close to
    /// unique to its row — and it is still a rule, where the alternative would
    /// be to tell the owner that no rule can be built from the row at all.
    #[test]
    fn a_row_carrying_only_a_description_generalises_on_it() {
        let mut described = row(account(1), "Savings", None);
        described.counterparty = ObservedCounterparty::Unknown;
        described.source_kind = None;
        described.description = Some("standing order".to_owned());
        let proposed = matcher_for(&described).expect("a matcher");
        assert_eq!(
            proposed.description_contains.as_deref(),
            Some("standing order")
        );
        assert_eq!(proposed.counterparty_account, None);
        assert_eq!(proposed.kind, None);
    }

    /// What an answer will keep, before anyone has answered (`iaam-sh6m`).
    ///
    /// The three cases the queue and the assessment used each to assert one of,
    /// flatly, in two sentences that contradicted each other. The last is the
    /// one no authority changes, and the one a test of the enum alone would miss:
    /// a row this build reads perfectly well and that still grounds nothing.
    #[test]
    fn what_an_answer_will_keep_is_read_off_the_row_and_the_authority() {
        let grounded = row(account(1), "Shop One", None).subject(None);
        assert_eq!(
            generalisation_ahead(Some(&grounded), true),
            GeneralisationProspect::WillStand
        );
        assert_eq!(
            generalisation_ahead(Some(&grounded), false),
            GeneralisationProspect::NeedsHisAdoption,
            "the row would ground one; the answerer may not write it"
        );

        // Read, and still nothing to build a standing decision from — so no
        // token makes this one stand.
        let bare = unmatchable(account(1)).subject(None);
        for authority in [true, false] {
            assert_eq!(
                generalisation_ahead(Some(&bare), authority),
                GeneralisationProspect::NoneFromThisRow,
                "a row that asks nothing grounds nothing under any token"
            );
        }

        // And the row this build cannot read at all, which is the absence
        // `subject_of` publishes.
        assert_eq!(
            generalisation_ahead(None, true),
            GeneralisationProspect::NoneFromThisRow
        );
    }

    /// The one row that still generalises into nothing, unchanged by the field
    /// policy above: a matcher that asks nothing matches nothing.
    #[test]
    fn a_row_printing_none_of_the_four_proposes_no_matcher() {
        assert_eq!(matcher_for(&unmatchable(account(1))), None);
    }

    /// A row whose source named no counterparty and no operation word falls to
    /// the category it was filed under, ahead of the description.
    ///
    /// This is the shape the first profile produces on every row, and under the
    /// three-field chain it proposed the **whole description** — a condition
    /// close to unique to the row it was learned from, which is the emptiness
    /// decision 0008 was written to end.
    #[test]
    fn a_row_naming_only_a_category_generalises_on_the_category() {
        let mut filed = unmatchable(account(1));
        filed.source_category = Some("Bank interest".to_owned());
        filed.description = Some("statement line 0001".to_owned());

        let proposed = matcher_for(&filed).expect("a matcher");

        assert_eq!(proposed.source_category.as_deref(), Some("Bank interest"));
        assert_eq!(
            proposed.description_contains, None,
            "the whole description is the last resort, not an addition to the \
             category"
        );
        assert_eq!(proposed.counterparty_account, None);
        assert_eq!(proposed.kind, None);
    }

    /// And it recognises the next row filed the same way, which is the only
    /// thing that makes adopting the proposal worth a call.
    #[test]
    fn a_category_proposal_recognises_the_next_row_filed_the_same_way() {
        let main = account(1);
        let mut settled = unmatchable(main);
        settled.source_category = Some("Bank interest".to_owned());
        settled.description = Some("statement line 0001".to_owned());
        let proposed = matcher_for(&settled).expect("a matcher");

        let mut later = unmatchable(main);
        later.source_category = Some("Bank interest".to_owned());
        later.description = Some("statement line 0002".to_owned());
        assert!(
            proposed.matches(&later.subject(None)),
            "same category, different line: the rule is about how the source \
             filed it, not about how it spelled the line"
        );
    }

    /// The order between the two words the source contributes.
    ///
    /// The word for what the operation **was** comes first, because that is the
    /// question being generalised; the category says what it was **for** and is
    /// one axis over. A row printing both must propose the word.
    #[test]
    fn a_row_printing_both_of_the_sources_words_proposes_the_operation_word() {
        let mut both = unmatchable(account(1));
        both.source_kind = Some("credit".to_owned());
        both.source_category = Some("Bank interest".to_owned());

        let proposed = matcher_for(&both).expect("a matcher");

        assert_eq!(proposed.kind.as_deref(), Some("credit"));
        assert_eq!(proposed.source_category, None);
    }

    #[test]
    fn an_answer_on_a_row_with_nothing_to_match_on_can_never_generalise() {
        // The other half of the absence, and the one no call of the owner's can
        // change: a matcher that asks nothing matches nothing, so there is no
        // rule to offer him.
        let contents = session_with(unmatchable(account(1)), Some(Answer::Paid), None);
        assert_eq!(
            generalisation_of(&contents.observations, &contents.questions[0]),
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
                    source_category: None,
                    owner_category: None,
                    source_code: None,
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
        let described = generalisation_of(&contents.observations, &contents.questions[0]);
        assert_eq!(described.code(), "available");
    }

    // -----------------------------------------------------------------------
    // A group publishes what its members have in common (iaam-cixz, 0034)
    //
    // Every account, title, party, word, sum and day below is invented
    // (CLAUDE.md).
    // -----------------------------------------------------------------------

    /// The owner's directory, in the one shape the plan resolves rows against.
    fn held(accounts: Vec<AccountDetailView>) -> AccountDirectory {
        AccountDirectory::from_accounts(accounts)
    }

    /// One open question whose decision the named rows also raise.
    fn open_alike(row: u32, alike: Vec<u32>) -> OpenQuestion {
        OpenQuestion {
            alike,
            ..open_about(row)
        }
    }

    /// One open question that is a leg of the movement `pair` names.
    ///
    /// `other` is the row on the far side, which a real pair always names and
    /// which the grouping deliberately does not key on.
    fn open_paired(row: u32, pair: Uuid, other: u32) -> OpenQuestion {
        OpenQuestion {
            pair: Some(MirroredPair {
                id: pair,
                row: other,
            }),
            ..open_about(row)
        }
    }

    /// The same row at another day and another sum — which is what the owner
    /// said the members of a group differ in and nothing else.
    fn on_day(observed: ObservedRow, amount_minor: i64, day: time::Date) -> ObservedRow {
        ObservedRow {
            amount_minor,
            dates: OperationDates {
                cash_posted: Some(day),
                ..OperationDates::default()
            },
            ..observed
        }
    }

    /// Three lines one party was paid over one month, as one group.
    fn one_party_three_times(main: AccountId) -> Vec<ImportObservationView> {
        let shop = filed_under(
            row(main, "Shop One", Some(date!(2025 - 01 - 03))),
            "Groceries",
        );
        vec![
            stored_row(1, &shop),
            stored_row(2, &on_day(shop.clone(), -4_500, date!(2025 - 01 - 19))),
            stored_row(3, &on_day(shop, -250, date!(2025 - 01 - 28))),
        ]
    }

    fn three_alike() -> Vec<OpenQuestion> {
        vec![
            open_alike(1, vec![2, 3]),
            open_alike(2, vec![1, 3]),
            open_alike(3, vec![1, 2]),
        ]
    }

    /// A group says what its members agree on and how far the rest of them run.
    ///
    /// The complaint, in the owner's own words: he does not need every record —
    /// show one of the group and ask what it was, because most of them share
    /// every attribute except the day, the time and the amount. Wave Y published
    /// the relation from each row to the others and never the set, so a caller
    /// asked what a set of rows was either read every member out or invented a
    /// summary of them. The one that happened was neither: it read his raw
    /// statement file.
    #[test]
    fn a_group_publishes_what_its_members_agree_on_and_the_spread_of_what_they_do_not() {
        let main = account(1);
        let groups = row_groups(
            &one_party_three_times(main),
            &three_alike(),
            &held(vec![detail(main, "Main")]),
            true,
        );
        assert_eq!(groups.len(), 1, "one decision is one group: {groups:?}");
        let group = &groups[0];
        assert_eq!(group.basis, GroupBasis::OneDecision);
        assert_eq!(
            group.rows,
            vec![1, 2, 3],
            "the members are the list, and the count is its length"
        );
        assert_eq!(
            group
                .common
                .account
                .as_ref()
                .map(|held| held.title.as_str()),
            Some("Main")
        );
        assert_eq!(group.common.counterparty.as_deref(), Some("Shop One"));
        assert_eq!(group.common.source_category.as_deref(), Some("Groceries"));
        assert_eq!(
            group.common.movement,
            Some(SharedMovement::Stated(Movement::Out))
        );
        assert_eq!(group.common.currency, Some(CurrencyCode::Rub));
        assert_eq!(
            group.days,
            Some(DaySpan {
                earliest: date!(2025 - 01 - 03),
                latest: date!(2025 - 01 - 28)
            }),
            "the day is one of the two things they differ in, and a span says \
             more than either endpoint"
        );
        assert_eq!(
            group.amounts,
            Some(AmountSpan {
                smallest_minor: -4_500,
                largest_minor: -250
            }),
            "and the amount is the other, signed as the source printed it"
        );
    }

    /// The description belongs to the group where every member carries it.
    ///
    /// Decision 0032 kept it off the row and this does not put it back: it is
    /// published for a set, only where the source said the same thing about
    /// every member of that set, and never inside the sentence — `row_mark`'s
    /// rule about what a person is read is untouched.
    #[test]
    fn a_description_every_member_carries_is_the_groups_and_one_that_differs_is_nobodys() {
        let main = account(1);
        let printed = "What the statement printed on this line";
        let mut one = row(main, "Shop One", Some(date!(2025 - 01 - 03)));
        one.description = Some(printed.to_owned());
        let mut two = row(main, "Shop One", Some(date!(2025 - 01 - 09)));
        two.description = Some(printed.to_owned());
        let open = vec![open_alike(1, vec![2]), open_alike(2, vec![1])];
        let directory = held(vec![detail(main, "Main")]);

        let agreeing = vec![stored_row(1, &one), stored_row(2, &two)];
        let groups = row_groups(&agreeing, &open, &directory, true);
        assert_eq!(
            groups[0].common.description.as_deref(),
            Some(printed),
            "one string for the set is the field that answers what these were"
        );
        assert!(
            !groups[0].question.ask.contains(printed),
            "and it is not read into the sentence: {}",
            groups[0].question.ask
        );

        two.description = Some("Something else the statement printed".to_owned());
        let differing = vec![stored_row(1, &one), stored_row(2, &two)];
        let groups = row_groups(&differing, &open, &directory, true);
        assert_eq!(
            groups[0].common.description, None,
            "a text one member does not carry is that member's and not the group's"
        );
    }

    /// A set of one is not a group.
    ///
    /// Decision 0033 §2 one surface down: a question already stands alone, and
    /// «here is a group of one» would make a caller take a group apart to find
    /// that out — and would put a sentence about several lines to him about one.
    #[test]
    fn a_set_of_one_is_no_group_because_one_row_is_one_question_already() {
        let main = account(1);
        let observations = vec![stored_row(1, &row(main, "Shop One", None))];
        let directory = held(vec![detail(main, "Main")]);
        assert!(
            row_groups(&observations, &[open_about(1)], &directory, true).is_empty(),
            "a question nothing else is alike to is published as itself"
        );
        assert!(
            row_groups(
                &observations,
                &[open_paired(1, Uuid::from_bytes([7; 16]), 2)],
                &directory,
                true
            )
            .is_empty(),
            "and a movement whose other leg an answer already settled is one \
             question again, not a group of one"
        );
    }

    /// A group says which single call settles it, and the two do not settle
    /// alike.
    ///
    /// `iaam-q5og` gave the answering call a stated reach; nothing said which
    /// reach settles which set, so a group published as a set was still a set a
    /// caller had to work out how to answer. A group with no answer is the wall
    /// in better clothes.
    #[test]
    fn a_group_publishes_the_reach_that_settles_it_and_a_pair_settles_from_either_side() {
        let main = account(1);
        let savings = account(2);
        let (departure, arrival) = two_legs(main, savings);
        let shop = row(main, "Shop One", Some(date!(2025 - 01 - 03)));
        let observations = vec![
            stored_row(1, &shop),
            stored_row(2, &on_day(shop, -250, date!(2025 - 01 - 04))),
            stored_row(3, &departure),
            stored_row(4, &arrival),
        ];
        let pair = Uuid::from_bytes([9; 16]);
        let open = vec![
            open_alike(1, vec![2]),
            open_alike(2, vec![1]),
            open_paired(3, pair, 4),
            open_paired(4, pair, 3),
        ];
        let groups = row_groups(
            &observations,
            &open,
            &held(vec![detail(main, "Main"), detail(savings, "Savings")]),
            true,
        );
        assert_eq!(groups.len(), 2, "{groups:?}");
        let decision = groups
            .iter()
            .find(|group| group.basis == GroupBasis::OneDecision)
            .expect("the two alike rows are a decision");
        assert_eq!(decision.settles, AnswerReach::EveryLikeRowInThisSession);
        let movement = groups
            .iter()
            .find(|group| group.basis == GroupBasis::OneMovement)
            .expect("the two legs are a movement");
        assert_eq!(
            movement.settles,
            AnswerReach::ThisRow,
            "the legs are one movement, so either of them settles it and a \
             wider reach would claim rows that are not this movement"
        );
        assert_eq!(movement.rows, vec![3, 4]);
        assert_eq!(
            movement.common.account, None,
            "the account is what the two legs differ in"
        );
        assert_eq!(movement.common.movement, None, "and so is the direction");
        assert_eq!(
            movement.common.currency,
            Some(CurrencyCode::Rub),
            "what they do share is what makes the two sums comparable at all"
        );
        assert_eq!(
            movement.amounts,
            Some(AmountSpan {
                smallest_minor: -1_000,
                largest_minor: 1_000
            }),
            "and the span is the two signs the source printed, not one sum twice"
        );
    }

    /// The group is put to him in his words and says what one answer decides.
    ///
    /// Decision 0027's register, checked the mechanical way the offer beside it
    /// is checked: no field name, no word that exists only because of how this
    /// is built, and a consequence that says what one answer costs when it is
    /// wrong rather than that the decision is his.
    #[test]
    fn a_group_is_asked_in_his_words_and_says_what_answering_it_once_decides() {
        let main = account(1);
        let groups = row_groups(
            &one_party_three_times(main),
            &three_alike(),
            &held(vec![detail(main, "Main")]),
            true,
        );
        let question = &groups[0].question;
        for internal in [
            "source_category",
            "matcher",
            "classification",
            "session",
            "row",
            "rule",
            "alike",
            "subject",
            "reach",
        ] {
            assert!(
                !question.ask.to_lowercase().contains(internal),
                "«{internal}» is our word, not his: {}",
                question.ask
            );
            assert!(
                !question.consequence.to_lowercase().contains(internal),
                "«{internal}» is our word, not his: {}",
                question.consequence
            );
        }
        assert!(
            question.ask.contains("Shop One") && question.ask.contains("Main"),
            "he is shown what his statement says about all of them: {}",
            question.ask
        );
        assert!(
            question.consequence.contains('3'),
            "what turns on the answer is how many lines it decides at once: {}",
            question.consequence
        );
        assert!(
            question.consequence.contains("wrongly"),
            "and what it costs to be wrong, which is the half that gets dropped: {}",
            question.consequence
        );
    }

    /// A range taken over two currencies is a pair of numbers with no unit.
    #[test]
    fn a_group_whose_members_are_in_two_currencies_publishes_no_amount_range() {
        let main = account(1);
        let one = row(main, "Shop One", Some(date!(2025 - 01 - 03)));
        let mut two = row(main, "Shop One", Some(date!(2025 - 01 - 09)));
        two.currency = CurrencyCode::Usd;
        let observations = vec![stored_row(1, &one), stored_row(2, &two)];
        let open = vec![open_alike(1, vec![2]), open_alike(2, vec![1])];
        let groups = row_groups(
            &observations,
            &open,
            &held(vec![detail(main, "Main")]),
            true,
        );
        assert_eq!(groups[0].common.currency, None);
        assert_eq!(
            groups[0].amounts, None,
            "«from twelve to four hundred» across two currencies tells him \
             something false"
        );
        assert!(
            groups[0].days.is_some(),
            "the days are still days, and only the amounts lose their unit"
        );
    }

    /// An account a group names carries the title he reads beside the
    /// identifier a call takes.
    ///
    /// One shape for an account published for a person, and not a second one
    /// invented here.
    #[test]
    fn a_group_names_its_account_the_way_the_owner_reads_it() {
        let main = account(1);
        let mut known = detail(main, "Main");
        known.institution = Some("Institution One".to_owned());
        let observations = one_party_three_times(main);
        let open = three_alike();
        let groups = row_groups(&observations, &open, &held(vec![known]), true);
        let named = groups[0]
            .common
            .account
            .as_ref()
            .expect("the members are all on one account");
        assert_eq!(named.id, main);
        assert_eq!(named.title, "Main");
        assert_eq!(named.institution.as_deref(), Some("Institution One"));

        let stranger = row_groups(&observations, &open, &held(Vec::new()), true);
        assert_eq!(
            stranger[0].common.account, None,
            "an identifier this instance cannot put a title on is not something \
             he can be asked about"
        );
        assert!(
            !stranger[0].question.ask.contains("«»"),
            "and the sentence drops the clause rather than quoting an empty \
             name: {}",
            stranger[0].question.ask
        );
    }

    /// The word the source files by is offered as a condition and is no group.
    ///
    /// The stated reason the third grouping does not take this shape. One word
    /// covering one row shape still covers as many decisions as it covers
    /// parties, so there is no one answer to put to him about it, and the only
    /// call that acts on the word whole decides rows nobody has looked at.
    #[test]
    fn a_word_the_source_filed_rows_under_is_offered_as_a_condition_and_is_no_group() {
        let main = account(1);
        let observations = vec![
            stored_row(
                1,
                &filed_under(
                    row(main, "Shop One", Some(date!(2025 - 01 - 03))),
                    "Groceries",
                ),
            ),
            stored_row(
                2,
                &filed_under(
                    row(main, "Shop Two", Some(date!(2025 - 01 - 09))),
                    "Groceries",
                ),
            ),
        ];
        // Two parties, so two decisions and nothing alike between them.
        let open = vec![open_about(1), open_about(2)];
        assert_eq!(
            offers(&observations, &open).offered.len(),
            1,
            "the word is still worth one condition"
        );
        assert!(
            row_groups(
                &observations,
                &open,
                &held(vec![detail(main, "Main")]),
                true
            )
            .is_empty(),
            "and it is not a group: no one answer settles two parties"
        );
    }

    /// The group worth putting to him first is first.
    #[test]
    fn groups_are_published_largest_first_so_the_one_that_settles_most_is_read_first() {
        let main = account(1);
        let two_more = row(main, "Shop Two", Some(date!(2025 - 02 - 02)));
        let mut observations = one_party_three_times(main);
        observations.push(stored_row(4, &two_more));
        observations.push(stored_row(
            5,
            &on_day(two_more, -700, date!(2025 - 02 - 06)),
        ));
        let mut open = three_alike();
        open.push(open_alike(4, vec![5]));
        open.push(open_alike(5, vec![4]));
        let groups = row_groups(
            &observations,
            &open,
            &held(vec![detail(main, "Main")]),
            true,
        );
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!(groups[0].rows, vec![1, 2, 3]);
        assert_eq!(groups[1].rows, vec![4, 5]);
    }

    /// A group nothing can be said about is not published as one.
    ///
    /// A group is a claim about what its members have in common, and a member
    /// this build cannot read leaves nothing to make the claim out of. It is not
    /// published as a group with the claim silently taken over the rest, which
    /// would be a set that says it is complete and is not.
    #[test]
    fn a_group_whose_rows_this_build_cannot_read_is_not_published_at_all() {
        let main = account(1);
        let observations = vec![stored_row(1, &row(main, "Shop One", None))];
        let open = vec![open_alike(1, vec![2]), open_alike(2, vec![1])];
        assert!(
            row_groups(
                &observations,
                &open,
                &held(vec![detail(main, "Main")]),
                true
            )
            .is_empty()
        );
    }

    // --- What one answer's standing decision would settle, before it stands --
    //
    // The owner's question, in his own words: «а если все операции верные,
    // кроме одной?» — a decision made from one answer covered a group and one
    // line of the group was wrong. Every account, word, amount and date below is
    // invented (CLAUDE.md).

    /// A line whose direction the source did not state, filed under one word,
    /// with nobody named on the other side.
    fn inner_line(on: AccountId, amount_minor: i64) -> ObservedRow {
        anonymous(
            directionless(row(on, "Anything", Some(date!(2026 - 03 - 01)))),
            amount_minor,
        )
    }

    /// A line of the same import whose stored text this build cannot read.
    fn unreadable_line(number: u32) -> ImportObservationView {
        ImportObservationView {
            row: number,
            row_key: None,
            concluded: false,
            payload: "{".to_owned(),
            answer: None,
        }
    }

    /// The same line, stored with the owner's answer already on it.
    ///
    /// `stored_row` deliberately carries none, and the difference is the whole
    /// of what one half of `iaam-r0qk` is about: a line he has already answered
    /// is settled before any standing decision of his is consulted.
    fn answered_row(number: u32, line: &ObservedRow, answer: Answer) -> ImportObservationView {
        ImportObservationView {
            answer: Some(serde_json::to_string(&answer).expect("an answer")),
            ..stored_row(number, line)
        }
    }

    /// A line of the same shape whose far side the owner's directory
    /// recognises as another account of his.
    ///
    /// It carries a direction because a directionless line naming one of his
    /// own accounts is still a question — the far side is known and the way the
    /// money went is not.
    fn recognised_line(on: AccountId, named: &str, amount_minor: i64) -> ObservedRow {
        ObservedRow {
            counterparty: ObservedCounterparty::Named(named.to_owned()),
            direction: ObservedDirection::Out,
            amount_minor: -amount_minor,
            ..directionless(row(on, named, Some(date!(2026 - 03 - 01))))
        }
    }

    fn asked_about(number: u32, line: &ObservedRow, on: AccountId) -> ImportQuestionView {
        stored_question_about(
            number,
            &Question::UnresolvedDirection {
                account: on,
                stated: line.source_kind.clone(),
                counterparty: None,
            },
        )
    }

    /// One movement already recorded, as the journal holds it.
    fn movement(
        on: AccountId,
        source_kind: Option<&str>,
        source_category: Option<&str>,
        amount_minor: i64,
    ) -> Event {
        let operation = SubmittedOperation {
            account: on,
            kind: iaam_ingest::operation::OperationKind::Withdrawal {
                amount_minor,
                currency: CurrencyCode::Rub,
            },
            dates: OperationDates {
                cash_posted: Some(date!(2026 - 02 - 04)),
                ..OperationDates::default()
            },
            source_time: None,
            idempotency_key: None,
            source_operation_id: None,
            source_category: source_category.map(str::to_owned),
            owner_category: None,
            source_code: None,
            source_kind: source_kind.map(str::to_owned),
            description: None,
        };
        normalize(
            &operation,
            &NormalizationContext {
                owner: OwnerId(uuid::Uuid::from_bytes([9; 16])),
                source: SourceId(uuid::Uuid::from_bytes([9; 16])),
                parser_version: ParserVersion(SUBMITTED_PARSER_VERSION.to_owned()),
            },
        )
        .expect("a movement this test states completely")
        .event
    }

    /// The forecast names the other lines of the import and the movements
    /// already recorded that the same decision would reach.
    ///
    /// The half he had was the answer's own row. What was missing is everything
    /// else the decision covers, which is the only thing that makes «all of them
    /// are right except one» answerable before the decision stands rather than a
    /// month after it.
    #[test]
    fn a_standing_decision_from_one_answer_names_what_it_would_settle() {
        let main = account(1);
        let asked = inner_line(main, 1_000);
        let alike = inner_line(main, 2_000);
        let mut filed_otherwise = inner_line(main, 3_000);
        filed_otherwise.source_kind = Some("OUTER".to_owned());

        let observations = vec![
            stored_row(1, &asked),
            stored_row(2, &alike),
            stored_row(3, &filed_otherwise),
            unreadable_line(4),
        ];
        let questions = vec![
            asked_about(1, &asked, main),
            asked_about(2, &alike, main),
            asked_about(3, &filed_otherwise, main),
        ];
        let journal = vec![
            movement(main, Some("INNER"), None, 4_000),
            movement(main, Some("OUTER"), None, 5_000),
        ];

        let forecast = forecast(
            would_stand(&asked, Answer::Paid, true),
            1,
            &SessionAsRead {
                observations: &observations,
                questions: &questions,
                settlements: &QuestionSettlements::default(),
            },
            &journal,
            &held(vec![detail(main, "Main")]),
        );

        assert_eq!(forecast.stands.code(), "written");
        assert_eq!(
            forecast
                .stands
                .proposed()
                .expect("a standing decision")
                .matcher
                .kind
                .as_deref(),
            Some("INNER"),
            "the condition is the one the answering call would write"
        );

        assert_eq!(
            forecast
                .in_this_import
                .iter()
                .map(|line| line.row)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "the line asked about and the one like it, and not the one filed \
             under another word"
        );
        assert!(
            forecast
                .in_this_import
                .iter()
                .all(|line| line.awaiting_answer),
            "both are still waiting on him"
        );
        assert_eq!(forecast.in_this_import[1].printed.amount_minor, 2_000);
        assert_eq!(
            forecast.in_this_import[1].printed.title.as_deref(),
            Some("Main"),
            "an account he is shown carries what he calls it"
        );

        assert_eq!(
            forecast
                .already_recorded
                .iter()
                .map(|fact| fact.event)
                .collect::<Vec<_>>(),
            vec![journal[0].id],
            "the recorded movement the same condition reaches, and only it"
        );
        assert_eq!(forecast.already_recorded[0].title.as_deref(), Some("Main"));
        assert_eq!(
            forecast.already_recorded[0]
                .now
                .as_ref()
                .map(|now| now.kind),
            Some("external_flow"),
            "what the journal records it as today is what tells him it is wrong"
        );

        assert_eq!(
            forecast.undecided,
            vec![Undecided::UnreadableRow { row: 4 }],
            "a line nothing can read is declared, never passed over"
        );
    }

    /// A line something already settles is not one this decision would settle
    /// (`iaam-r0qk`).
    ///
    /// Two things settle a line before any standing decision of his is
    /// consulted: his own earlier answer, which the reading takes first, and his
    /// account directory recognising the far side, which is read before his
    /// decisions are. A forecast that counts those among what it would settle
    /// tells him, of a line his own answer already decided, that answering here
    /// decides it too — and then invites him to go and put right whichever of
    /// them was something else. Both halves are false about such a line.
    ///
    /// The count and the list are kept apart for that reason: the list is
    /// everything the condition covers, because a line left out of it reads as
    /// «not affected», and the count is what the decision would actually
    /// settle. Each line says which of the two it is.
    #[test]
    fn a_line_already_settled_is_not_counted_as_one_the_decision_would_settle() {
        let main = account(1);
        let savings = account(2);
        let asked = inner_line(main, 1_000);
        let answered = inner_line(main, 2_000);
        let recognised = recognised_line(main, "Savings", 3_000);

        let accounts = vec![detail(main, "Main"), detail(savings, "Savings")];
        let observations = vec![
            stored_row(1, &asked),
            answered_row(2, &answered, Answer::Paid),
            stored_row(3, &recognised),
        ];
        let questions = vec![
            asked_about(1, &asked, main),
            asked_about(2, &answered, main),
        ];
        let resolver = ruled(accounts.clone(), Vec::new());
        let mut read = vec![
            read_against(1, &asked, &resolver),
            read_observation(observations[1].clone(), &answered, &resolver),
            read_against(3, &recognised, &resolver),
        ];
        let settled = settlements(&mut read, &MirroredRows::default());

        let forecast = forecast(
            would_stand(&asked, Answer::Paid, true),
            1,
            &SessionAsRead {
                observations: &observations,
                questions: &questions,
                settlements: &settled,
            },
            &[],
            &held(accounts),
        );

        assert_eq!(
            forecast
                .in_this_import
                .iter()
                .map(|line| line.row)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "all three are covered by the condition, and none is dropped out \
             of sight"
        );
        assert!(
            forecast.notice.contains("no other line"),
            "and none of them is one it would settle, which is what he is \
             read: {}",
            forecast.notice
        );
        assert!(
            forecast.notice.contains("2 other lines"),
            "the two it leaves as they are are declared and not dropped: {}",
            forecast.notice
        );

        // «Still waiting» answers `false` for three different situations, so
        // each line says which of them it is rather than leaving the reader to
        // guess from the one word.
        let words = forecast
            .in_this_import
            .iter()
            .map(|line| {
                (
                    line.awaiting_answer,
                    line.now.as_ref().map(QuestionSettlement::code),
                    line.settled_regardless(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            words,
            vec![
                (true, None, false),
                (false, Some("answered"), true),
                (false, Some("directory"), true),
            ],
            "the one he is being asked about is his to answer; the second he \
             answered himself; the third his own accounts settled"
        );
    }

    /// A movement recorded before the source's two words were kept apart is
    /// declared rather than dropped.
    ///
    /// This is the falsification, and it is the specimen that matters: such a
    /// fact answers «no» to a condition asking about the operation word, because
    /// the word it carries is in the other slot and the reader blanks it below
    /// schema version 14. Counted as a non-match it disappears — and a forecast
    /// that drops it tells him nothing else is affected, which is the one false
    /// thing it must not say.
    #[test]
    fn a_movement_recorded_before_the_two_words_were_kept_apart_is_declared() {
        let main = account(1);
        let asked = inner_line(main, 1_000);
        let mut older = movement(main, None, Some("INNER"), 6_000);
        older.schema_version = SOURCE_CATEGORY_IS_A_CATEGORY_FROM - 1;
        let elsewhere = movement(main, Some("OUTER"), None, 7_000);

        let forecast = forecast(
            would_stand(&asked, Answer::Paid, true),
            1,
            &SessionAsRead {
                observations: &[stored_row(1, &asked)],
                questions: &[asked_about(1, &asked, main)],
                settlements: &QuestionSettlements::default(),
            },
            &[older.clone(), elsewhere],
            &held(vec![detail(main, "Main")]),
        );

        assert!(
            forecast.already_recorded.is_empty(),
            "nothing is claimed about a word the journal cannot vouch for"
        );
        assert_eq!(
            forecast.undecided,
            vec![Undecided::FactWithoutTheWord {
                event: older.id,
                account: main,
                title: Some("Main".to_owned()),
                date: Some(date!(2026 - 02 - 04)),
            }],
            "and the movement it could not judge is named, with the one it \
             could judge left out"
        );
        assert!(
            forecast.notice.contains('1'),
            "and what could not be judged is in the sentence he is read: {}",
            forecast.notice
        );
    }

    /// A movement he has already put right is not one the decision still
    /// settles.
    ///
    /// What a condition would reach is what is **in force**, which is what the
    /// recomputation reads. A fold over the raw record instead would hand him a
    /// movement to look at that he had already corrected, in the one list whose
    /// whole purpose is to be short enough to read.
    #[test]
    fn a_movement_already_put_right_is_not_one_the_decision_still_settles() {
        let main = account(1);
        let asked = inner_line(main, 1_000);
        let filed = movement(main, Some("INNER"), None, 4_000);
        let reversal = Event {
            id: EventId::new_random(),
            relation: Relation::Reversal { target: filed.id },
            ..filed.clone()
        };

        let forecast = forecast(
            would_stand(&asked, Answer::Paid, true),
            1,
            &SessionAsRead {
                observations: &[stored_row(1, &asked)],
                questions: &[asked_about(1, &asked, main)],
                settlements: &QuestionSettlements::default(),
            },
            &[filed, reversal],
            &held(vec![detail(main, "Main")]),
        );

        assert!(
            forecast.already_recorded.is_empty(),
            "a movement he has already put right is not left behind by anything"
        );
        assert!(forecast.undecided.is_empty());
    }

    /// A record that will not fold is declared, and the count is not published
    /// as none.
    ///
    /// The failure mode this refuses is the quiet one: a fold that gives up
    /// leaves an empty list, and an empty list beside «no movement you have
    /// already recorded» tells him nothing of his is affected — which is a
    /// sentence composed out of a failure.
    #[test]
    fn a_record_that_will_not_fold_is_declared_rather_than_counted_as_none() {
        let main = account(1);
        let asked = inner_line(main, 1_000);
        let filed = movement(main, Some("INNER"), None, 4_000);
        let dangling = Event {
            id: EventId::new_random(),
            relation: Relation::Reversal {
                target: EventId::new_random(),
            },
            ..filed.clone()
        };

        let forecast = forecast(
            would_stand(&asked, Answer::Paid, true),
            1,
            &SessionAsRead {
                observations: &[stored_row(1, &asked)],
                questions: &[asked_about(1, &asked, main)],
                settlements: &QuestionSettlements::default(),
            },
            &[filed, dangling],
            &held(vec![detail(main, "Main")]),
        );

        assert!(forecast.already_recorded.is_empty());
        assert_eq!(
            forecast.undecided,
            vec![Undecided::RecordedMovementsWouldNotFold]
        );
        assert!(
            !forecast.notice.contains("no movement"),
            "nothing of his is counted as unaffected on the strength of a \
             failure: {}",
            forecast.notice
        );
        assert!(
            forecast.notice.contains("could not be read"),
            "and he is told why the count is missing: {}",
            forecast.notice
        );
    }

    /// The three ways no standing decision comes of an answer, each saying
    /// which one it is.
    ///
    /// An empty reach is published by all three, so the state is the only thing
    /// that says why it is empty — and «this answer keeps none» and «this line
    /// grounds none» are different facts with different remedies.
    #[test]
    fn an_answer_that_keeps_no_standing_decision_says_which_of_the_reasons_it_is() {
        let main = account(1);
        let asked = inner_line(main, 1_000);

        assert_eq!(
            would_stand(&asked, Answer::BetweenOwnAccounts, true).code(),
            "not_from_this_answer",
            "the one answer that is not a claim about every line like this one"
        );
        assert_eq!(
            would_stand(&unmatchable(main), Answer::Paid, true).code(),
            "not_from_this_row",
            "a line that prints nothing a later line could be matched against"
        );
        assert_eq!(
            would_stand(&asked, Answer::Paid, false).code(),
            "for_his_adoption",
            "an answerer who may not make a standing decision is told what it \
             would be, not that there is none"
        );

        let forecast = forecast(
            would_stand(&asked, Answer::BetweenOwnAccounts, true),
            1,
            &SessionAsRead {
                observations: &[stored_row(1, &asked)],
                questions: &[asked_about(1, &asked, main)],
                settlements: &QuestionSettlements::default(),
            },
            &[movement(main, Some("INNER"), None, 4_000)],
            &held(vec![detail(main, "Main")]),
        );
        assert!(forecast.in_this_import.is_empty());
        assert!(forecast.already_recorded.is_empty());
        assert!(
            !forecast.notice.is_empty(),
            "the empty lists are explained rather than left to be read as «nothing else is affected»"
        );
    }

    /// The forecast is read out to a person, so it is in his words.
    ///
    /// Checked the mechanical way the group's question beside it is checked: no
    /// field name, and no word that exists only because of how this is built.
    #[test]
    fn what_the_forecast_reads_out_is_in_his_words() {
        let main = account(1);
        let asked = inner_line(main, 1_000);
        let alike = inner_line(main, 2_000);
        let forecast = forecast(
            would_stand(&asked, Answer::Paid, true),
            1,
            &SessionAsRead {
                observations: &[stored_row(1, &asked), stored_row(2, &alike)],
                questions: &[asked_about(1, &asked, main), asked_about(2, &alike, main)],
                settlements: &QuestionSettlements::default(),
            },
            &[movement(main, Some("INNER"), None, 4_000)],
            &held(vec![detail(main, "Main")]),
        );

        // Every sentence this forecast can compose, and not only the ones this
        // one happens to hold: a reason published for a state no fixture
        // reaches is exactly the sentence nobody rereads.
        let sentences = std::iter::once(forecast.notice.clone())
            .chain(
                [
                    Undecided::UnreadableRow { row: 1 },
                    Undecided::FactWithoutTheWord {
                        event: EventId::new_random(),
                        account: main,
                        title: None,
                        date: None,
                    },
                    Undecided::RecordedMovementsWouldNotFold,
                ]
                .iter()
                .map(|entry| entry.why().to_owned()),
            )
            .chain(
                [
                    would_stand(&asked, Answer::Paid, false),
                    would_stand(&asked, Answer::BetweenOwnAccounts, true),
                    would_stand(&unmatchable(main), Answer::Paid, true),
                ]
                .iter()
                .map(|stands| forecast_notice(stands, 1, &[], &[], &[])),
            )
            .collect::<Vec<_>>();
        for sentence in &sentences {
            for internal in [
                "source_category",
                "matcher",
                "classification",
                "session",
                "row",
                "rule",
                "alike",
                "subject",
                "reach",
                "journal",
            ] {
                assert!(
                    !sentence.to_lowercase().contains(internal),
                    "«{internal}» is our word, not his: {sentence}"
                );
            }
        }
        assert!(
            forecast.notice.contains('1'),
            "he is told how many other lines of this import one answer covers: {}",
            forecast.notice
        );
    }
}

/// The identity index the plan asks about every candidate (iaam-1k9t).
///
/// Every account, amount, date and identifier below is invented (CLAUDE.md).
#[cfg(test)]
mod recorded_identities {
    use super::*;
    use iaam_ingest::operation::{OperationDates, OperationKind};
    use time::macros::date;

    fn owner() -> OwnerId {
        OwnerId(uuid::Uuid::from_bytes([1; 16]))
    }

    fn account() -> AccountId {
        AccountId(uuid::Uuid::from_bytes([2; 16]))
    }

    fn source(byte: u8) -> SourceId {
        SourceId(uuid::Uuid::from_bytes([byte; 16]))
    }

    /// One recorded movement.
    fn recorded(
        source: SourceId,
        source_operation_id: Option<&str>,
        idempotency_key: Option<&str>,
        amount_minor: i64,
    ) -> iaam_core::event::Event {
        let operation = SubmittedOperation {
            account: account(),
            kind: OperationKind::Deposit {
                amount_minor,
                currency: CurrencyCode::Rub,
            },
            dates: OperationDates {
                cash_posted: Some(date!(2025 - 03 - 02)),
                ..OperationDates::default()
            },
            source_time: None,
            idempotency_key: idempotency_key.map(str::to_owned),
            source_operation_id: source_operation_id.map(str::to_owned),
            source_category: None,
            owner_category: None,
            source_code: None,
            source_kind: None,
            description: None,
        };
        normalize(
            &operation,
            &NormalizationContext {
                owner: owner(),
                source,
                parser_version: ParserVersion(SUBMITTED_PARSER_VERSION.to_owned()),
            },
        )
        .expect("a movement this test states completely")
        .event
    }

    #[test]
    fn a_row_the_source_identified_is_held_although_it_names_no_key() {
        let journal = [recorded(source(3), Some("row-7"), None, 100)];
        let identities = RecordedIdentities::of(&journal);
        assert!(
            identities.holds(&recorded(source(3), Some("row-7"), None, 100)),
            "the store matches this identifier first, and the plan must match it too"
        );
    }

    #[test]
    fn one_operation_identifier_under_two_sources_is_two_facts() {
        // §10.6: an operation identifier is unique **within** a source, so
        // comparing two across sources suppresses a legitimate fact. This is
        // the property that makes the pair the key and not the string.
        let journal = [recorded(source(3), Some("row-7"), None, 100)];
        let identities = RecordedIdentities::of(&journal);
        assert!(!identities.holds(&recorded(source(4), Some("row-7"), None, 100)));
    }

    #[test]
    fn a_key_the_journal_holds_is_held_whatever_the_source() {
        // The other scoping, and it is deliberately different: the idempotency
        // key is the owner's namespace, not the source's.
        let journal = [recorded(source(3), None, Some("statement-1"), 100)];
        let identities = RecordedIdentities::of(&journal);
        assert!(identities.holds(&recorded(source(4), None, Some("statement-1"), 100)));
    }

    #[test]
    fn a_row_naming_neither_is_never_held() {
        let journal = [recorded(source(3), None, None, 100)];
        let identities = RecordedIdentities::of(&journal);
        assert!(
            !identities.holds(&recorded(source(3), None, None, 100)),
            "nothing about this row is a key, so nothing about it may be a duplicate"
        );
    }

    #[test]
    fn a_row_naming_neither_still_resembles_what_the_journal_holds() {
        // The whole of the disclosure: the shape is all there is, and the shape
        // is enough to tell the owner about and never enough to decide on.
        let journal = [recorded(source(3), None, None, 100)];
        let identities = RecordedIdentities::of(&journal);
        assert_eq!(
            identities.resembling(&recorded(source(3), None, None, 100)),
            Some(journal[0].id)
        );
    }

    #[test]
    fn a_row_of_another_amount_resembles_nothing() {
        let journal = [recorded(source(3), None, None, 100)];
        let identities = RecordedIdentities::of(&journal);
        assert_eq!(
            identities.resembling(&recorded(source(3), None, None, 101)),
            None
        );
    }

    #[test]
    fn the_key_a_row_carries_does_not_stop_it_resembling_another_fact() {
        // A file re-imported after one row was corrected carries new derived
        // keys for every row, so the unchanged ones are fresh by key and are
        // about to be written twice. The resemblance is the only thing that
        // says so.
        let journal = [recorded(source(3), None, Some("first-import-row-1"), 100)];
        let identities = RecordedIdentities::of(&journal);
        let again = recorded(source(3), None, Some("second-import-row-1"), 100);
        assert!(!identities.holds(&again));
        assert_eq!(identities.resembling(&again), Some(journal[0].id));
    }

    #[test]
    fn the_earliest_event_sharing_a_shape_is_the_one_named() {
        // Two genuine identical payments have one fingerprint between them.
        // Naming the later one would make the plan — and therefore the revision
        // stamp — depend on the order the journal happened to be read in.
        let journal = [
            recorded(source(3), None, Some("one"), 100),
            recorded(source(3), None, Some("two"), 100),
        ];
        let identities = RecordedIdentities::of(&journal);
        assert_eq!(
            identities.resembling(&recorded(source(3), None, Some("three"), 100)),
            Some(journal[0].id)
        );
    }
}
