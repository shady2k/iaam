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

use iaam_core::event::kind::FeeOrigin;
use iaam_core::ids::{AccountId, ImportId, ImportQuestionId, ImportSessionId, OwnerId, SourceId};
use iaam_ingest::classification::{
    Answer, AnswerShape, Classification, ClassificationResult, ClassificationRule, Movement,
    Question, RuleMatcher, classify,
};
use iaam_ingest::observation::{Intake, ObservedRow};
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Rejection, SubmittedOperation, Verdict, normalize};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{
    AccountView, ImportObservationView, ImportQuestionView, ImportSessionState, ImportSessionView,
    NewImportQuestion, Principal,
};
use crate::scenarios::ingest::submit_candidates;

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
pub async fn commit_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
) -> Result<Vec<Verdict>, AppError> {
    require_submit(principal)?;
    let contents = read_session(services, principal, session).await?;
    if contents.session.state != ImportSessionState::Open {
        return Err(AppError::Invalid {
            field: "session".to_owned(),
            expected: "an open import session".to_owned(),
            actual: contents.session.state.code().to_owned(),
        });
    }
    if contents.has_open_questions() {
        let open = contents
            .questions
            .iter()
            .filter(|question| question.is_open())
            .count();
        return Err(AppError::Invalid {
            field: "session".to_owned(),
            expected: "every question answered before the import is committed".to_owned(),
            actual: format!("{open} unanswered"),
        });
    }

    let source = contents.session.source.unwrap_or_else(SourceId::new_random);
    let resolver = Resolver::load(services, principal.owner).await?;
    let mut candidates = Vec::with_capacity(contents.observations.len());
    for observation in &contents.observations {
        candidates.push(
            operation_of(observation, &resolver)
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
                }),
        );
    }
    let verdicts = submit_candidates(services, principal, "operation", candidates).await?;
    services
        .store
        .close_import_session(principal.owner, session, ImportSessionState::Committed)
        .await?;
    Ok(verdicts)
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
