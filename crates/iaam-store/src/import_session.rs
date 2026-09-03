//! Import sessions: observations accumulated before anything is committed.
//!
//! **Pre-journal state.** Nothing this module writes is a fact, and nothing it
//! writes reaches `events`. That is the line the whole design rests on: nothing
//! in the journal is provisional, and nothing provisional is in the journal.
//!
//! `payload`, `question` and `answer` are opaque JSON here, exactly as a
//! classification rule's matcher is (see [`crate::rules`]): the store keeps them
//! and the application reads them. It validates that they parse, because a
//! payload the application cannot read must not be written silently.

use iaam_core::ids::{ImportId, ImportQuestionId, ImportSessionId, OwnerId, SourceId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{SqliteStore, StoreError, now};

/// Where a session is in its life.
///
/// It leaves `Open` once and never returns: committing writes facts that cannot
/// be unwritten, and abandoning is the owner saying the rows were wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Open,
    Committed,
    Abandoned,
}

impl SessionState {
    /// Stored code. One place, so two tables cannot spell it differently.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Committed => "committed",
            Self::Abandoned => "abandoned",
        }
    }

    /// The code back, refusing anything that is not one of the three.
    ///
    /// An unknown code is an error rather than a default: a session silently
    /// read as `Open` because its state did not parse would accept observations
    /// after it was committed.
    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "open" => Ok(Self::Open),
            "committed" => Ok(Self::Committed),
            "abandoned" => Ok(Self::Abandoned),
            other => Err(StoreError::InvalidValue {
                field: "import session state",
                value: other.to_owned(),
            }),
        }
    }
}

/// A stored session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSession {
    pub id: ImportSessionId,
    pub owner: OwnerId,
    pub state: SessionState,
    pub source: Option<SourceId>,
    pub import: Option<ImportId>,
    pub opened_at: String,
    pub closed_at: Option<String>,
}

/// One submitted line held in a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObservation {
    pub row: u32,
    pub row_key: Option<String>,
    /// Whether the caller submitted a conclusion rather than an observation.
    pub concluded: bool,
    pub payload: String,
    pub answer: Option<String>,
}

/// A question about to be written: its typed form, its alternatives, its wording.
///
/// The three travel together because they are one question. Passed separately
/// they are three strings whose order a caller can get wrong, and two of them
/// are JSON while the third is prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewQuestion {
    pub question: String,
    pub alternatives: String,
    pub prompt: String,
}

/// One question put to the owner about one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredQuestion {
    pub id: ImportQuestionId,
    pub session: ImportSessionId,
    pub row: u32,
    pub question: String,
    pub alternatives: String,
    pub prompt: String,
    pub asked_at: String,
    pub answered_at: Option<String>,
    pub answer: Option<String>,
    pub rule: Option<String>,
}

impl StoredQuestion {
    /// Whether the owner has answered it.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.answered_at.is_none()
    }
}

/// The control section a source printed for one account and currency.
///
/// Opaque to this crate in the same way a payload is: the store keeps the four
/// figures and the interval, and the application decides what they mean and what
/// they are compared with. What the store does enforce is the key — one section
/// per account and currency in a session — because two sections for one account
/// would let the assessment compare against whichever it read first.
///
/// Every figure is separately nullable, and NULL means «the source did not print
/// it», never zero (§4.9).
///
/// One type for both directions: nothing here is minted or derived by the store.
/// `stated_at` is on the table and not on this struct, because it is the store's
/// own clock reading and nothing above reads it — it is there for the same
/// reason `asked_at` is, so that a session can be looked at afterwards and told
/// in what order it was assembled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredControlFigures {
    pub account: String,
    pub currency: String,
    pub period_from: String,
    pub period_to: String,
    pub opening: Option<i64>,
    pub closing: Option<i64>,
    pub debit_turnover: Option<i64>,
    pub credit_turnover: Option<i64>,
}

impl SqliteStore {
    /// Open a session, or return the open one this declaration already has.
    ///
    /// Reuse rather than a second session, because two sessions over one
    /// declared import would split that statement's questions across two places
    /// and the owner would answer one of them.
    ///
    /// Recognition is keyed on the **whole** declaration and not on the import
    /// alone (iaam-zv54). A batch that declared a source and no label names no
    /// import, so keying on the import recognised nothing and opened a fresh
    /// session on every call — splitting one declaration's questions across as
    /// many sessions as the caller made calls, which is the very failure the
    /// reuse exists to prevent. A batch that declared nothing at all still gets
    /// a session of its own every time, and must: there is nothing to recognise
    /// it by.
    ///
    /// **No unique index guards the source-only case**, unlike
    /// `import_sessions_by_import`. It cannot be added: this behaviour has
    /// already produced databases holding several open sessions for one source,
    /// and a migration creating the index over them would refuse to apply and
    /// leave the store unopenable. The race it would close is closed anyway —
    /// the lookup and the insert share one `Immediate` transaction, so two
    /// concurrent opens serialise — and across processes the worst outcome is
    /// the state every such database is already in.
    pub fn open_import_session(
        &mut self,
        owner: OwnerId,
        source: Option<SourceId>,
        import: Option<ImportId>,
    ) -> Result<StoredSession, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = standing_session(&transaction, owner, source, import)? {
            transaction.commit()?;
            return Ok(existing);
        }
        let session = StoredSession {
            id: ImportSessionId::new_random(),
            owner,
            state: SessionState::Open,
            source,
            import,
            opened_at: now(),
            closed_at: None,
        };
        transaction.execute(
            "INSERT INTO import_sessions (id, owner, state, source, import, opened_at, closed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                session.id.inner().to_string(),
                owner.inner().to_string(),
                session.state.code(),
                source.map(|id| id.inner().to_string()),
                import.map(|id| id.inner().to_string()),
                session.opened_at,
            ],
        )?;
        transaction.commit()?;
        Ok(session)
    }

    /// One session of the owner's, by identifier.
    ///
    /// The owner is in the query rather than checked afterwards: an identifier
    /// is not an access right, and a session read without its owner lets anyone
    /// holding the identifier read someone else's import (§14).
    pub fn load_import_session(
        &self,
        owner: OwnerId,
        id: ImportSessionId,
    ) -> Result<Option<StoredSession>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, state, source, import, opened_at, closed_at
                 FROM import_sessions WHERE owner = ?1 AND id = ?2",
                params![owner.inner().to_string(), id.inner().to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?
            .map(|row| session_from(owner, row))
            .transpose()
    }

    /// Every session of the owner's, newest first.
    ///
    /// This is what makes a question survive the response that carried it: a
    /// caller that lost the response finds the session here and the question in
    /// it.
    pub fn list_import_sessions(&self, owner: OwnerId) -> Result<Vec<StoredSession>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, state, source, import, opened_at, closed_at
             FROM import_sessions WHERE owner = ?1
             ORDER BY opened_at DESC, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(session_from(owner, row?)?);
        }
        Ok(sessions)
    }

    /// Add one submitted line, or return the row it already occupies.
    ///
    /// The row number is assigned in the same transaction as the insert, for the
    /// reason the journal's sequence is: separate "read the maximum" and "write"
    /// steps give two concurrent submissions one row number.
    ///
    /// A session that is no longer open refuses: an observation added after
    /// commit would sit in a session whose facts are already written and would
    /// never be written itself.
    pub fn add_import_observation(
        &mut self,
        owner: OwnerId,
        session: ImportSessionId,
        row_key: Option<&str>,
        concluded: bool,
        payload: &str,
    ) -> Result<StoredObservation, StoreError> {
        check_json(payload, "observation payload")?;
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_open(&transaction, owner, session)?;
        if let Some(key) = row_key
            && let Some(existing) = observation_by_key(&transaction, session, key)?
        {
            transaction.commit()?;
            return Ok(existing);
        }
        let used: Option<u32> = transaction.query_row(
            "SELECT MAX(row) FROM import_observations WHERE session = ?1",
            [session.inner().to_string()],
            |row| row.get(0),
        )?;
        let stored = StoredObservation {
            row: used.map_or(1, |value| value.saturating_add(1)),
            row_key: row_key.map(str::to_owned),
            concluded,
            payload: payload.to_owned(),
            answer: None,
        };
        transaction.execute(
            "INSERT INTO import_observations (session, row, row_key, concluded, payload, answer)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                session.inner().to_string(),
                stored.row,
                stored.row_key,
                i64::from(concluded),
                stored.payload,
            ],
        )?;
        transaction.commit()?;
        Ok(stored)
    }

    /// A session's lines in submission order.
    pub fn list_import_observations(
        &self,
        session: ImportSessionId,
    ) -> Result<Vec<StoredObservation>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT row, row_key, concluded, payload, answer
             FROM import_observations WHERE session = ?1 ORDER BY row",
        )?;
        let rows = statement.query_map([session.inner().to_string()], |row| {
            Ok(StoredObservation {
                row: row.get(0)?,
                row_key: row.get(1)?,
                concluded: row.get::<_, i64>(2)? != 0,
                payload: row.get(3)?,
                answer: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Record the question one row raises, or return the one it already raised.
    ///
    /// One question per row, enforced by the index rather than by the caller
    /// remembering: a row asked about twice would be answered twice, and the two
    /// answers could differ.
    pub fn record_import_question(
        &mut self,
        owner: OwnerId,
        session: ImportSessionId,
        row: u32,
        asking: &NewQuestion,
    ) -> Result<StoredQuestion, StoreError> {
        check_json(&asking.question, "question")?;
        check_json(&asking.alternatives, "question alternatives")?;
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_open(&transaction, owner, session)?;
        if let Some(existing) = question_for_row(&transaction, session, row)? {
            transaction.commit()?;
            return Ok(existing);
        }
        let stored = StoredQuestion {
            id: ImportQuestionId::new_random(),
            session,
            row,
            question: asking.question.clone(),
            alternatives: asking.alternatives.clone(),
            prompt: asking.prompt.clone(),
            asked_at: now(),
            answered_at: None,
            answer: None,
            rule: None,
        };
        transaction.execute(
            "INSERT INTO import_questions
                 (id, session, row, question, alternatives, prompt, asked_at,
                  answered_at, answer, rule)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL)",
            params![
                stored.id.inner().to_string(),
                session.inner().to_string(),
                row,
                stored.question,
                stored.alternatives,
                stored.prompt,
                stored.asked_at,
            ],
        )?;
        transaction.commit()?;
        Ok(stored)
    }

    /// A session's questions, in row order.
    pub fn list_import_questions(
        &self,
        session: ImportSessionId,
    ) -> Result<Vec<StoredQuestion>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, row, question, alternatives, prompt, asked_at, answered_at, answer, rule
             FROM import_questions WHERE session = ?1 ORDER BY row",
        )?;
        let rows = statement.query_map([session.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;
        let mut questions = Vec::new();
        for row in rows {
            let (id, row_number, question, alternatives, prompt, asked, answered, answer, rule) =
                row?;
            questions.push(StoredQuestion {
                id: ImportQuestionId(parse_uuid(&id, "import question")?),
                session,
                row: row_number,
                question,
                alternatives,
                prompt,
                asked_at: asked,
                answered_at: answered,
                answer,
                rule,
            });
        }
        Ok(questions)
    }

    /// Record the owner's answer on the question and on the row it is about.
    ///
    /// Both writes happen in one transaction: commit reads the observation and a
    /// question answered without its observation updated would be a session that
    /// looks settled and commits nothing.
    ///
    /// A question that is already answered is refused rather than overwritten.
    /// The owner changing their mind is a different act — the rule the first
    /// answer created is what carries it, and amending that rule replans the
    /// history it already classified.
    pub fn answer_import_question(
        &mut self,
        owner: OwnerId,
        session: ImportSessionId,
        question: ImportQuestionId,
        answer: &str,
        rule: Option<&str>,
    ) -> Result<StoredQuestion, StoreError> {
        check_json(answer, "answer")?;
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_open(&transaction, owner, session)?;
        let answered_at = now();
        let updated = transaction.execute(
            "UPDATE import_questions SET answered_at = ?3, answer = ?4, rule = ?5
             WHERE session = ?1 AND id = ?2 AND answered_at IS NULL",
            params![
                session.inner().to_string(),
                question.inner().to_string(),
                answered_at,
                answer,
                rule,
            ],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "an unanswered import question",
                id: question.inner().to_string(),
            });
        }
        transaction.execute(
            "UPDATE import_observations SET answer = ?3
             WHERE session = ?1
               AND row = (SELECT row FROM import_questions WHERE id = ?2)",
            params![
                session.inner().to_string(),
                question.inner().to_string(),
                answer,
            ],
        )?;
        let stored =
            question_by_id(&transaction, session, question)?.ok_or(StoreError::NotFound {
                what: "an import question",
                id: question.inner().to_string(),
            })?;
        transaction.commit()?;
        Ok(stored)
    }

    /// Record what a source printed about itself, replacing what it printed
    /// before.
    ///
    /// Replacement rather than a second row, and rather than a refusal: a
    /// section already stated and stated again is a transcription corrected, and
    /// the correction is the figure the owner now wants checked. The alternative
    /// — refusing the second — would leave a session pinned to a typo with no
    /// way out but abandoning every row in it.
    ///
    /// All of a call's sections are written in one transaction. A statement's
    /// control section is one thing; half of it written and half refused would
    /// be compared against the rows as though the source had printed only half.
    ///
    /// A session that is no longer open refuses, exactly as adding a row does:
    /// figures stated after the commit would be compared against nothing and
    /// written nowhere.
    pub fn state_import_control_figures(
        &mut self,
        owner: OwnerId,
        session: ImportSessionId,
        figures: &[StoredControlFigures],
    ) -> Result<Vec<StoredControlFigures>, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_open(&transaction, owner, session)?;
        let stated_at = now();
        for figure in figures {
            transaction.execute(
                "INSERT INTO import_control_figures
                     (session, account, currency, period_from, period_to,
                      opening, closing, debit_turnover, credit_turnover, stated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (session, account, currency) DO UPDATE SET
                     period_from = excluded.period_from,
                     period_to = excluded.period_to,
                     opening = excluded.opening,
                     closing = excluded.closing,
                     debit_turnover = excluded.debit_turnover,
                     credit_turnover = excluded.credit_turnover,
                     stated_at = excluded.stated_at",
                params![
                    session.inner().to_string(),
                    figure.account,
                    figure.currency,
                    figure.period_from,
                    figure.period_to,
                    figure.opening,
                    figure.closing,
                    figure.debit_turnover,
                    figure.credit_turnover,
                    stated_at,
                ],
            )?;
        }
        let stated = control_figures(&transaction, session)?;
        transaction.commit()?;
        Ok(stated)
    }

    /// A session's control sections, in account and currency order.
    pub fn list_import_control_figures(
        &self,
        session: ImportSessionId,
    ) -> Result<Vec<StoredControlFigures>, StoreError> {
        control_figures(&self.conn, session)
    }

    /// Close a session, committed or abandoned.
    ///
    /// Only an open session closes, and the check is part of the same statement
    /// rather than a read before it: two concurrent calls would otherwise both
    /// see `open` and both close it, one of them as committed and one as
    /// abandoned.
    ///
    /// Abandoning leaves the session's rows exactly where they are and marks it.
    /// It does not read or write the journal — that is the property being kept —
    /// and it does not delete the observations either: what the owner rejected is
    /// worth being able to look at.
    pub fn close_import_session(
        &mut self,
        owner: OwnerId,
        session: ImportSessionId,
        state: SessionState,
    ) -> Result<StoredSession, StoreError> {
        if state == SessionState::Open {
            return Err(StoreError::InvalidValue {
                field: "import session state",
                value: state.code().to_owned(),
            });
        }
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE import_sessions SET state = ?3, closed_at = ?4
             WHERE owner = ?1 AND id = ?2 AND state = 'open'",
            params![
                owner.inner().to_string(),
                session.inner().to_string(),
                state.code(),
                now(),
            ],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "an open import session",
                id: session.inner().to_string(),
            });
        }
        transaction.commit()?;
        self.load_import_session(owner, session)?
            .ok_or(StoreError::NotFound {
                what: "an import session",
                id: session.inner().to_string(),
            })
    }
}

fn control_figures(
    conn: &Connection,
    session: ImportSessionId,
) -> Result<Vec<StoredControlFigures>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT account, currency, period_from, period_to,
                opening, closing, debit_turnover, credit_turnover
         FROM import_control_figures WHERE session = ?1
         ORDER BY account, currency",
    )?;
    let rows = statement.query_map([session.inner().to_string()], |row| {
        Ok(StoredControlFigures {
            account: row.get(0)?,
            currency: row.get(1)?,
            period_from: row.get(2)?,
            period_to: row.get(3)?,
            opening: row.get(4)?,
            closing: row.get(5)?,
            debit_turnover: row.get(6)?,
            credit_turnover: row.get(7)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

/// The session an import already has open, when it has one.
fn open_session_for_import(
    conn: &Connection,
    owner: OwnerId,
    import: ImportId,
) -> Result<Option<StoredSession>, StoreError> {
    conn.query_row(
        "SELECT id, state, source, import, opened_at, closed_at
         FROM import_sessions
         WHERE owner = ?1 AND import = ?2 AND state = 'open'",
        params![owner.inner().to_string(), import.inner().to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    )
    .optional()?
    .map(|row| session_from(owner, row))
    .transpose()
}

/// The open session one declared source has while naming no import.
///
/// Ordered oldest first and taken one at a time, unlike the lookup above: the
/// import case is unique by index, while this one is not and this behaviour has
/// left databases holding several. The oldest is the one that has been open
/// longest and therefore the one holding whatever has already been answered, so
/// picking it is what the refusal above it means to describe.
fn open_session_for_source(
    conn: &Connection,
    owner: OwnerId,
    source: SourceId,
) -> Result<Option<StoredSession>, StoreError> {
    conn.query_row(
        "SELECT id, state, source, import, opened_at, closed_at
         FROM import_sessions
         WHERE owner = ?1 AND source = ?2 AND import IS NULL AND state = 'open'
         ORDER BY opened_at, id
         LIMIT 1",
        params![owner.inner().to_string(), source.inner().to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    )
    .optional()?
    .map(|row| session_from(owner, row))
    .transpose()
}

/// The open session a declaration already has, whatever it declared.
///
/// One place, so the store and the scenario that refuses a half-imported
/// session cannot disagree about which session a declaration reaches. A
/// declaration naming an import is recognised by it; one naming only a source
/// is recognised by that; one naming neither is recognised by nothing, which is
/// the honest answer and not an oversight.
pub(crate) fn standing_session(
    conn: &Connection,
    owner: OwnerId,
    source: Option<SourceId>,
    import: Option<ImportId>,
) -> Result<Option<StoredSession>, StoreError> {
    match (source, import) {
        (_, Some(import)) => open_session_for_import(conn, owner, import),
        (Some(source), None) => open_session_for_source(conn, owner, source),
        (None, None) => Ok(None),
    }
}

/// Refuse to touch a session that is no longer open.
fn require_open(
    conn: &Connection,
    owner: OwnerId,
    session: ImportSessionId,
) -> Result<(), StoreError> {
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM import_sessions WHERE owner = ?1 AND id = ?2",
            params![owner.inner().to_string(), session.inner().to_string()],
            |row| row.get(0),
        )
        .optional()?;
    match state.as_deref() {
        Some("open") => Ok(()),
        // A missing session and a closed one produce one error: different
        // answers would tell an outsider that such a session exists.
        _ => Err(StoreError::NotFound {
            what: "an open import session",
            id: session.inner().to_string(),
        }),
    }
}

fn observation_by_key(
    conn: &Connection,
    session: ImportSessionId,
    key: &str,
) -> Result<Option<StoredObservation>, StoreError> {
    conn.query_row(
        "SELECT row, row_key, concluded, payload, answer
         FROM import_observations WHERE session = ?1 AND row_key = ?2",
        params![session.inner().to_string(), key],
        |row| {
            Ok(StoredObservation {
                row: row.get(0)?,
                row_key: row.get(1)?,
                concluded: row.get::<_, i64>(2)? != 0,
                payload: row.get(3)?,
                answer: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(StoreError::from)
}

fn question_for_row(
    conn: &Connection,
    session: ImportSessionId,
    row: u32,
) -> Result<Option<StoredQuestion>, StoreError> {
    question_row(
        conn,
        session,
        "SELECT id, row, question, alternatives, prompt, asked_at, answered_at, answer, rule
         FROM import_questions WHERE session = ?1 AND row = ?2",
        params![session.inner().to_string(), row],
    )
}

fn question_by_id(
    conn: &Connection,
    session: ImportSessionId,
    id: ImportQuestionId,
) -> Result<Option<StoredQuestion>, StoreError> {
    question_row(
        conn,
        session,
        "SELECT id, row, question, alternatives, prompt, asked_at, answered_at, answer, rule
         FROM import_questions WHERE session = ?1 AND id = ?2",
        params![session.inner().to_string(), id.inner().to_string()],
    )
}

fn question_row(
    conn: &Connection,
    session: ImportSessionId,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Option<StoredQuestion>, StoreError> {
    let row = conn
        .query_row(sql, args, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })
        .optional()?;
    let Some((id, number, question, alternatives, prompt, asked, answered, answer, rule)) = row
    else {
        return Ok(None);
    };
    Ok(Some(StoredQuestion {
        id: ImportQuestionId(parse_uuid(&id, "import question")?),
        session,
        row: number,
        question,
        alternatives,
        prompt,
        asked_at: asked,
        answered_at: answered,
        answer,
        rule,
    }))
}

type SessionColumns = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

fn session_from(owner: OwnerId, row: SessionColumns) -> Result<StoredSession, StoreError> {
    let (id, state, source, import, opened_at, closed_at) = row;
    Ok(StoredSession {
        id: ImportSessionId(parse_uuid(&id, "import session")?),
        owner,
        state: SessionState::parse(&state)?,
        source: source
            .as_deref()
            .map(|value| parse_uuid(value, "source"))
            .transpose()?
            .map(SourceId),
        import: import
            .as_deref()
            .map(|value| parse_uuid(value, "import"))
            .transpose()?
            .map(ImportId),
        opened_at,
        closed_at,
    })
}

fn check_json(value: &str, field: &'static str) -> Result<(), StoreError> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|source| StoreError::RuleNotJson { field, source })
}

fn parse_uuid(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
