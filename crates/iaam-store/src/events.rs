//! Fact log: recording and reading.

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::kind::{
    CASH_TRANSFER_KIND, CONTROL_ASSERTION_KIND, EventKind, FlowEndpoints, IMPORT_COVERAGE_GAP_KIND,
};
use iaam_core::event::{Event, Relation};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, EventId, ImportId, ImportSessionId, OwnerId, SourceId,
};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_core::reconciliation::evidence::IdentityScope;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::{Date, OffsetDateTime};

use crate::{SqliteStore, StoreError};

/// What happened while attempting to record.
///
/// A retry is not an error: a repeated call with the same key must return
/// the same result; otherwise, a client that did not receive a response cannot
/// safely retry the request (§10.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended {
    Inserted { id: EventId },
    Duplicate { existing: EventId },
}

/// Activity bounds for one account, excluding bookkeeping events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountActivityRecord {
    pub account: AccountId,
    pub has_business_fact: bool,
    pub first_effective_date: Option<Date>,
    pub last_effective_date: Option<Date>,
}

impl AccountActivityRecord {
    /// Widen the record to cover one more day this account had a fact on.
    ///
    /// The bounds are data-coverage bounds, so a fact reaching the account from
    /// the other side of a transfer moves them exactly as one recorded against
    /// it does: an account whose only day is the day money arrived is covered
    /// on that day and on no other.
    fn touch(&mut self, date: Date) {
        self.has_business_fact = true;
        self.first_effective_date = Some(
            self.first_effective_date
                .map_or(date, |first| first.min(date)),
        );
        self.last_effective_date =
            Some(self.last_effective_date.map_or(date, |last| last.max(date)));
    }
}

/// The state needed to match one control assertion without loading the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlAssertionRecord {
    pub account: AccountId,
    pub period: AssertionPeriod,
    pub point: Option<BalancePoint>,
    pub dimension: Dimension,
    pub reconstructed_opening: bool,
}

impl SqliteStore {
    /// Record an event with an already assigned sequence.
    ///
    /// Used where the sequence is defined externally and cannot be changed:
    /// importing an archived bundle and restoring from an archive.
    pub fn append_event(
        &self,
        event: &Event,
        identity_scope: IdentityScope,
    ) -> Result<Appended, StoreError> {
        if let Some(existing) = find_duplicate(&self.conn, event, identity_scope)? {
            return Ok(Appended::Duplicate { existing });
        }
        insert_event(&self.conn, event)?;
        Ok(Appended::Inserted { id: event.id })
    }

    /// Record an event while assigning its sequence number **in the same
    /// transaction**.
    ///
    /// Separating “get `MAX(sequence) + 1`” and “insert” is a race:
    /// two concurrent requests receive the same number, and the order
    /// within the day starts being determined by a random identifier instead of
    /// the declared semantics (§4.8). A transaction with immediate lock acquisition
    /// closes the race between processes as well, while the unique index
    /// `(owner, effective_date, sequence)` turns any remaining gap
    /// into an error instead of silently reordering entries.
    pub fn append_event_in_order(
        &mut self,
        event: &Event,
        identity_scope: IdentityScope,
    ) -> Result<Appended, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_duplicate(&transaction, event, identity_scope)? {
            return Ok(Appended::Duplicate { existing });
        }
        let day = event.order.date();
        let used: Option<u32> = transaction.query_row(
            "SELECT MAX(sequence) FROM events WHERE owner = ?1 AND effective_date = ?2",
            params![event.owner.inner().to_string(), day.to_string()],
            |row| row.get(0),
        )?;
        let stamped = Event {
            order: event.order.source_time().map_or_else(
                || EffectiveOrder::new(day, used.map_or(1, |value| value.saturating_add(1))),
                |source_time| {
                    EffectiveOrder::with_source_time(
                        day,
                        source_time,
                        used.map_or(1, |value| value.saturating_add(1)),
                    )
                },
            ),
            ..event.clone()
        };
        insert_event(&transaction, &stamped)?;
        transaction.commit()?;
        Ok(Appended::Inserted { id: stamped.id })
    }

    /// The owner's entire log in `EffectiveOrder`.
    ///
    /// The database defines the order, but the projection still sorts the slice itself:
    /// the core need not trust the order received from outside (§4.8). Known source times
    /// sort before untimed events, and raw hashes make equal source times reproducible.
    pub fn load_events(&self, owner: OwnerId) -> Result<Vec<Event>, StoreError> {
        self.query_events(
            "SELECT id, payload FROM events
             WHERE owner = ?1
             ORDER BY effective_date,
                      source_time IS NULL,
                      CASE WHEN source_time IS NULL THEN sequence ELSE 0 END,
                      source_time,
                      CASE WHEN source_time IS NULL THEN '' ELSE raw_hash END,
                      sequence,
                      id",
            params![owner.inner().to_string()],
        )
    }

    /// Owner ledger through and including the date. The report-date slice
    /// is assembled by the wrapper: the event core does not filter by date (§6.1).
    pub fn load_events_through(
        &self,
        owner: OwnerId,
        through: time::Date,
    ) -> Result<Vec<Event>, StoreError> {
        self.query_events(
            "SELECT id, payload FROM events
             WHERE owner = ?1 AND effective_date <= ?2
             ORDER BY effective_date,
                      source_time IS NULL,
                      CASE WHEN source_time IS NULL THEN sequence ELSE 0 END,
                      source_time,
                      CASE WHEN source_time IS NULL THEN '' ELSE raw_hash END,
                      sequence,
                      id",
            params![owner.inner().to_string(), through.to_string()],
        )
    }

    /// Summarise every owned account, counting **both** accounts a transfer
    /// touched.
    ///
    /// `events.account` is the account an event is *recorded against*, and a
    /// `CashTransfer` is recorded against one of the two it moves money
    /// between. Joining on that column alone therefore reports an account whose
    /// entire content arrived by internal transfer — savings fed from a current
    /// account, a deposit opened by moving money across — as having no business
    /// fact at all: the queue never asks it for a balance and offers it an
    /// import instead, while it holds money at month end (iaam-8axt).
    ///
    /// The second account lives only in the event payload, so it is read in
    /// Rust from [`EventKind::flow_endpoints`] rather than by `json_extract` in
    /// the SQL or from a denormalised column:
    ///
    /// - a JSON path in the projection is pinned to the serde shape of a core
    ///   type, and a renamed field would leave the query matching nothing
    ///   without breaking the build — the drift `CONTROL_ASSERTION_KIND` and
    ///   its neighbours exist to prevent;
    /// - a `counterparty_account` column would have to be backfilled, and the
    ///   journal is append-only *in the database*: `events_are_immutable`
    ///   aborts any `UPDATE`, so the backfill would mean dropping and
    ///   recreating the guard that makes the journal a journal.
    ///
    /// What it costs is reading the owner's transfer events — a subset of the
    /// journal, not the whole of it — on each call.
    pub fn list_account_activity(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountActivityRecord>, StoreError> {
        let mut activity = self.activity_recorded_against(owner)?;
        for (from, to, date) in self.transfer_endpoints(owner)? {
            for account in [from, to] {
                if let Some(record) = activity.iter_mut().find(|record| record.account == account) {
                    record.touch(date);
                }
            }
        }
        Ok(activity)
    }

    /// The bounds an account gets from the events recorded against it.
    fn activity_recorded_against(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountActivityRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT a.id, COUNT(e.id), MIN(e.effective_date), MAX(e.effective_date)
             FROM accounts AS a
             LEFT JOIN events AS e
               ON e.owner = a.owner
              AND e.account = a.id
              AND e.kind NOT IN (?2, ?3)
             WHERE a.owner = ?1
             GROUP BY a.id
             ORDER BY a.id",
        )?;
        let rows = statement.query_map(
            params![
                owner.inner().to_string(),
                CONTROL_ASSERTION_KIND,
                IMPORT_COVERAGE_GAP_KIND
            ],
            |row| {
                let id: String = row.get(0)?;
                let first: Option<String> = row.get(2)?;
                let last: Option<String> = row.get(3)?;
                Ok((id, row.get::<_, i64>(1)? > 0, first, last))
            },
        )?;
        let mut activity = Vec::new();
        for row in rows {
            let (id, has_business_fact, first, last) = row?;
            activity.push(AccountActivityRecord {
                account: AccountId(parse_uuid(&id, "account")?),
                has_business_fact,
                first_effective_date: first.as_deref().map(parse_date).transpose()?,
                last_effective_date: last.as_deref().map(parse_date).transpose()?,
            });
        }
        Ok(activity)
    }

    /// Both accounts of every transfer the owner's journal holds, with its day.
    ///
    /// The endpoints come from the core's own [`EventKind::flow_endpoints`],
    /// which is the single place that decides what a movement's endpoints are;
    /// a kind that later grows a second account is picked up here without this
    /// query being touched.
    fn transfer_endpoints(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<(AccountId, AccountId, Date)>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, payload FROM events
             WHERE owner = ?1 AND kind = ?2
             ORDER BY effective_date, sequence, id",
        )?;
        let rows = statement.query_map(
            params![owner.inner().to_string(), CASH_TRANSFER_KIND],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut endpoints = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            let event: Event = serde_json::from_str(&payload)
                .map_err(|source| StoreError::EventDecode { id, source })?;
            if let FlowEndpoints::BetweenAccounts { from, to } = event.kind.flow_endpoints() {
                endpoints.push((from, to, event.order.date()));
            }
        }
        Ok(endpoints)
    }

    /// List only an account's control assertions; payload matching stays in Rust.
    pub fn list_control_assertions(
        &self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<Vec<ControlAssertionRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, payload
             FROM events
             WHERE owner = ?1 AND account = ?2 AND kind IN (?3, ?4)
             ORDER BY effective_date, sequence, id",
        )?;
        let rows = statement.query_map(
            params![
                owner.inner().to_string(),
                account.inner().to_string(),
                CONTROL_ASSERTION_KIND,
                "opening_cash"
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut assertions = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            let event: Event = serde_json::from_str(&payload)
                .map_err(|source| StoreError::EventDecode { id, source })?;
            let (period, point, dimension, reconstructed_opening) = match event.kind {
                EventKind::ControlAssertion { period, claim } => {
                    let point = match &claim {
                        iaam_core::reconciliation::claim::ControlClaim::CashBalance {
                            at, ..
                        }
                        | iaam_core::reconciliation::claim::ControlClaim::PositionQuantity {
                            at,
                            ..
                        } => Some(*at),
                        _ => None,
                    };
                    (period, point, claim.dimension(), false)
                }
                EventKind::OpeningCash { .. } => (
                    AssertionPeriod::between(event.order.date(), event.order.date())
                        .expect("an opening event has a one-day assertion period"),
                    Some(BalancePoint::Opening),
                    Dimension::Cash,
                    true,
                ),
                _ => continue,
            };
            assertions.push(ControlAssertionRecord {
                account: event.account,
                period,
                point,
                dimension,
                reconstructed_opening,
            });
        }
        Ok(assertions)
    }

    fn query_events(
        &self,
        sql: &str,
        parameters: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<Event>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(parameters, |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            let event: Event = serde_json::from_str(&payload)
                .map_err(|source| StoreError::EventDecode { id, source })?;
            events.push(event);
        }
        Ok(events)
    }
}

fn parse_uuid(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}

fn parse_date(value: &str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::InvalidValue {
        field: "effective_date",
        value: value.to_owned(),
    })
}

/// Insert an event. The body is factored out of the public methods: both write paths
/// must insert the same data into the database, and a second copy of this SQL
/// would eventually drift from the first.
pub(crate) fn insert_event(conn: &Connection, event: &Event) -> Result<(), StoreError> {
    let payload = serde_json::to_string(event).map_err(StoreError::EventEncode)?;
    let (relation_kind, relation_target) = match event.relation {
        Relation::None => ("none", None),
        Relation::Reversal { target } => ("reversal", Some(target.inner().to_string())),
        Relation::Replacement { target } => ("replacement", Some(target.inner().to_string())),
    };
    let recorded_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));
    let source_time = event.order.source_time().map(format_source_time);

    conn.execute(
        "INSERT INTO events (
             id, schema_version, owner, account, kind, effective_date, sequence, source_time,
             relation_kind, relation_target, source, source_operation_id,
             idempotency_key, raw_hash, payload, recorded_at, import_session,
             settled_by_rule, settled_by_rule_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                   ?18, ?19)",
        params![
            event.id.inner().to_string(),
            event.schema_version,
            event.owner.inner().to_string(),
            event.account.inner().to_string(),
            event.kind.discriminant(),
            event.order.date().to_string(),
            event.order.sequence(),
            source_time,
            relation_kind,
            relation_target,
            event.provenance.source().inner().to_string(),
            event.provenance.source_operation_id(),
            event.idempotency_key.as_deref(),
            event.provenance.raw_hash().as_str(),
            payload,
            recorded_at,
            // Lifted out of the payload it is already in, so the journal can be
            // narrowed by it. Written from the event rather than from an
            // argument: a caller that could pass a different session than the
            // one the fact carries is a caller that can make the column
            // disagree with the provenance.
            event
                .provenance
                .import_session()
                .map(|session| session.inner().to_string()),
            // Lifted out of the payload for the same reason and written from
            // the event for the same one: a caller that could pass a rule other
            // than the one the fact carries is a caller that can make the
            // column disagree with the provenance. The version column is kept
            // as recorded provenance for compatibility with the schema, but it
            // is not a second journal filter: a rule identifier has one version.
            // Both columns stay NULL where the fact names no rule — including
            // where it says a reading found none, which the payload records and
            // the columns deliberately do not.
            event
                .provenance
                .settling_rule()
                .map(|(rule, _)| rule.inner().to_string()),
            event.provenance.settling_rule().map(|(_, version)| version),
        ],
    )?;
    Ok(())
}

fn format_source_time(time: time::Time) -> String {
    let (hour, minute, second, nanosecond) = time.as_hms_nano();
    format!("{hour:02}:{minute:02}:{second:02}.{nanosecond:09}")
}

/// Find a duplicate by keys from strongest to weakest (§10.6).
///
/// The natural key “account + date + amount” is intentionally absent here:
/// two identical purchases on the same day are a legitimate situation, and merging
/// them would mean losing the fact.
pub(crate) fn find_duplicate(
    conn: &Connection,
    event: &Event,
    identity_scope: IdentityScope,
) -> Result<Option<EventId>, StoreError> {
    if let Some(operation) = event.provenance.source_operation_id() {
        let found = match identity_scope {
            IdentityScope::Source => lookup(
                conn,
                "SELECT id FROM events WHERE owner = ?1 AND source = ?2 AND source_operation_id = ?3",
                params![
                    event.owner.inner().to_string(),
                    event.provenance.source().inner().to_string(),
                    operation
                ],
            )?,
            IdentityScope::Account => lookup(
                conn,
                "SELECT id FROM events
                 WHERE owner = ?1 AND source = ?2 AND account = ?3 AND source_operation_id = ?4",
                params![
                    event.owner.inner().to_string(),
                    event.provenance.source().inner().to_string(),
                    event.account.inner().to_string(),
                    operation
                ],
            )?,
        };
        if found.is_some() {
            return Ok(found);
        }
    }
    if let Some(key) = event.idempotency_key.as_deref() {
        let found = lookup(
            conn,
            "SELECT id FROM events WHERE owner = ?1 AND idempotency_key = ?2",
            params![event.owner.inner().to_string(), key],
        )?;
        if found.is_some() {
            return Ok(found);
        }
    }
    lookup(
        conn,
        "SELECT id FROM events WHERE id = ?1",
        params![event.id.inner().to_string()],
    )
}

fn lookup(
    conn: &Connection,
    sql: &str,
    parameters: &[&dyn rusqlite::ToSql],
) -> Result<Option<EventId>, StoreError> {
    let found: Option<String> = conn
        .query_row(sql, parameters, |row| row.get(0))
        .optional()?;
    Ok(found
        .and_then(|id| uuid::Uuid::parse_str(&id).ok())
        .map(EventId))
}

/// Position in the journal's total order.
///
/// `(owner, effective_date, sequence)` is unique by index, so this pair names
/// exactly one row and paging can resume from it without an offset. An offset
/// would shift under a concurrent write and silently skip or repeat a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalCursor {
    pub effective_date: Date,
    pub sequence: u32,
}

/// One narrowed page of the owner's journal.
///
/// Every handle is optional here, and the rule that a read must narrow by
/// *something* deliberately is not. That rule is a policy about what an API
/// caller may ask for, it lives in the application where the refusal is
/// written, and a second copy of it in the store would drift from the first.
#[derive(Debug, Clone, Default)]
pub struct JournalQuery {
    pub event: Option<EventId>,
    pub idempotency_key: Option<String>,
    pub account: Option<AccountId>,
    /// Only facts whose event account or one of their legs is this account.
    pub touching: Option<AccountId>,
    pub source: Option<SourceId>,
    /// Only facts committed out of this import session.
    pub import_session: Option<ImportSessionId>,
    /// Only facts carrying this declared import. Unlike `import_session`, the
    /// import survives across several sessions and is the key a retraction
    /// takes.
    pub import: Option<ImportId>,
    /// Only facts one of the owner's standing classification rules filed.
    ///
    /// A fact whose reading found no rule, and a fact recorded before the rule
    /// was recorded at all, are both outside every value of this filter: they
    /// name no rule, and this selects by a named one. What tells those two
    /// apart is the fact's own provenance, not this handle.
    pub settled_by_rule: Option<ClassificationRuleId>,
    /// Inclusive lower bound on the effective date.
    pub from: Option<Date>,
    /// Inclusive upper bound on the effective date.
    pub to: Option<Date>,
    /// Resume after this position, exclusive.
    pub after: Option<JournalCursor>,
    /// Maximum rows to return.
    pub limit: u32,
}
/// Filters for the distinct source-category values in the journal.
#[derive(Debug, Clone, Default)]
pub struct SourceCategoryQuery {
    pub account: Option<AccountId>,
    pub from: Option<Date>,
    pub to: Option<Date>,
}

impl SqliteStore {
    /// Read a narrowed page of the owner's journal in `(date, sequence)` order.
    ///
    /// The owner is part of every clause, not merely of the identifier the
    /// caller supplied: an event identifier is a UUID, and a UUID confers no
    /// right to read someone else's journal (§14).
    pub fn list_journal_events(
        &self,
        owner: OwnerId,
        query: &JournalQuery,
    ) -> Result<Vec<Event>, StoreError> {
        let (sql, parameters) = journal_sql(owner, query);
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            let event: Event = serde_json::from_str(&payload)
                .map_err(|source| StoreError::EventDecode { id, source })?;
            events.push(event);
        }
        Ok(events)
    }

    /// Distinct source-category evidence for the owner's journal scope.
    ///
    /// Facts written before schema version 14 used this storage slot for the
    /// source's operation word, so those rows are intentionally excluded.
    pub fn list_journal_source_categories(
        &self,
        owner: OwnerId,
        query: &SourceCategoryQuery,
    ) -> Result<Vec<String>, StoreError> {
        let (sql, parameters) = source_categories_sql(owner, query);
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// Assemble the narrowed query and its bound parameters.
fn source_categories_sql(
    owner: OwnerId,
    query: &SourceCategoryQuery,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut sql = String::from(
        "SELECT DISTINCT json_extract(payload, '$.provenance.source_category')
         FROM events
         WHERE owner = ?1
           AND json_extract(payload, '$.schema_version') >= 14
           AND json_extract(payload, '$.provenance.source_category') IS NOT NULL",
    );
    let mut parameters: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(owner.inner().to_string())];

    let mut bind = |clause: &str, value: Box<dyn rusqlite::ToSql>| {
        parameters.push(value);
        sql.push_str(&clause.replace('?', &format!("?{}", parameters.len())));
    };

    if let Some(account) = query.account {
        bind(" AND account = ?", Box::new(account.inner().to_string()));
    }
    if let Some(from) = query.from {
        bind(" AND effective_date >= ?", Box::new(from.to_string()));
    }
    if let Some(to) = query.to {
        bind(" AND effective_date <= ?", Box::new(to.to_string()));
    }
    sql.push_str(" ORDER BY 1");
    (sql, parameters)
}

///
/// The SQL is built rather than written out because the handles are
/// independent: spelling every combination would be sixteen statements that
/// must agree on the ordering, and one of them would eventually not.
/// Nothing from the caller is interpolated — only placeholder numbers are —
/// so a value can never become SQL.
fn journal_sql(owner: OwnerId, query: &JournalQuery) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut sql = String::from("SELECT id, payload FROM events WHERE owner = ?1");
    let mut parameters: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(owner.inner().to_string())];

    let mut bind = |sql: &mut String, clause: &str, value: Box<dyn rusqlite::ToSql>| {
        parameters.push(value);
        sql.push_str(&clause.replace('?', &format!("?{}", parameters.len())));
    };

    if let Some(event) = query.event {
        bind(&mut sql, " AND id = ?", Box::new(event.inner().to_string()));
    }
    if let Some(key) = query.idempotency_key.as_ref() {
        bind(&mut sql, " AND idempotency_key = ?", Box::new(key.clone()));
    }
    if let Some(account) = query.account {
        bind(
            &mut sql,
            " AND account = ?",
            Box::new(account.inner().to_string()),
        );
    }
    if let Some(touching) = query.touching {
        // The report fold reads event legs. Keep the event's own account in
        // this predicate too: ordinary events carry their account there, and
        // transfer events carry the receiving account only in a leg.
        bind(
            &mut sql,
            " AND (account = ?",
            Box::new(touching.inner().to_string()),
        );
        bind(
            &mut sql,
            " OR EXISTS (SELECT 1 FROM json_each(events.payload, '$.legs') AS leg \
             WHERE json_extract(leg.value, '$.account') = ?))",
            Box::new(touching.inner().to_string()),
        );
    }
    if let Some(source) = query.source {
        bind(
            &mut sql,
            " AND source = ?",
            Box::new(source.inner().to_string()),
        );
    }
    if let Some(import) = query.import {
        bind(
            &mut sql,
            " AND json_extract(payload, '$.provenance.import') = ?",
            Box::new(import.inner().to_string()),
        );
    }
    if let Some(session) = query.import_session {
        bind(
            &mut sql,
            " AND import_session = ?",
            Box::new(session.inner().to_string()),
        );
    }
    if let Some(rule) = query.settled_by_rule {
        bind(
            &mut sql,
            " AND settled_by_rule = ?",
            Box::new(rule.inner().to_string()),
        );
    }
    if let Some(from) = query.from {
        bind(
            &mut sql,
            " AND effective_date >= ?",
            Box::new(from.to_string()),
        );
    }
    if let Some(to) = query.to {
        bind(
            &mut sql,
            " AND effective_date <= ?",
            Box::new(to.to_string()),
        );
    }
    if let Some(after) = query.after {
        // Strictly after the cursor in the same order the rows come back in.
        // Comparing the pair, rather than the date alone, is what stops the
        // last row of a page from opening the next one.
        bind(
            &mut sql,
            " AND (effective_date > ?",
            Box::new(after.effective_date.to_string()),
        );
        bind(
            &mut sql,
            " OR (effective_date = ?",
            Box::new(after.effective_date.to_string()),
        );
        bind(&mut sql, " AND sequence > ?))", Box::new(after.sequence));
    }

    sql.push_str(" ORDER BY effective_date, sequence");
    bind(&mut sql, " LIMIT ?", Box::new(i64::from(query.limit)));
    (sql, parameters)
}

/// One fact of a chain, with the moment this instance wrote it down.
///
/// The pair is a store row and not a domain type. [`Event`] is what the journal
/// holds; when *this* instance happened to write it down is a fact about this
/// database, true of no copy of the journal but this one, and so it is not part
/// of the fact that was written. Whoever reads a chain needs both — "what
/// changed" comes off the event, "when did it change" off the stamp — so the
/// two travel together, and no further.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedEvent {
    /// The fact exactly as the journal holds it.
    pub event: Event,
    /// When this instance wrote the fact down: RFC 3339, from its own clock at
    /// insert.
    ///
    /// **Not the effective date, and never published as one.** A correction
    /// written in March to a fact effective in January is a March act about a
    /// January fact, and a reader that confused the two would date the owner's
    /// change of mind to the day of the operation he changed it about.
    pub recorded_at: String,
}

/// The backward step of a chain walk: the fact a relation names, if it is his.
///
/// The owner is in the clause and not merely in the identifier the caller
/// supplied: an event identifier is a UUID, and a UUID confers no right to read
/// someone else's journal (§14). A target belonging to another owner therefore
/// reads as no target at all, which from here is what it is.
pub const CHAIN_STEP_BACK_SQL: &str = "SELECT id, payload, recorded_at FROM events
     WHERE owner = ?1 AND id = ?2";

/// The forward step: every fact naming this one as what it reverses or replaces.
///
/// `relation` points only backwards, so this is the whole of how a chain is
/// followed to its tail, and migration `0030`'s `events_by_relation_target` is
/// the whole of what keeps it off the journal.
///
/// Both statements are published so that a test can prove the planner answers
/// **these** queries out of an index. A test carrying its own copy of the SQL
/// would prove only that its copy is indexed, and the walk could become a full
/// scan without a single test noticing.
pub const CHAIN_STEP_FORWARD_SQL: &str = "SELECT id, payload, recorded_at FROM events
     WHERE owner = ?1 AND relation_target = ?2";

impl SqliteStore {
    /// Every fact of one operation's correction chain, oldest first.
    ///
    /// A correction is not an edit: correcting an event writes a reversal
    /// naming it and a replacement naming it, so what the owner calls one
    /// operation is a run of facts and not a row. This returns the run.
    ///
    /// **Any identifier in it answers with the same run** — the original, the
    /// replacement standing now, or a reversal written along the way. The walk
    /// therefore goes backwards by `relation` to the head first and only then
    /// forwards to the tail: the identifier he most often holds is the one a
    /// correction just handed back to him, and entering by that must not answer
    /// a different question from entering by the one his import gave him.
    ///
    /// Empty where the owner has no such event, which is the same answer for an
    /// identifier of nothing and for an identifier of somebody else's fact —
    /// deliberately, since telling those apart is telling him that a stranger's
    /// event exists.
    pub fn event_chain(
        &self,
        owner: OwnerId,
        event: EventId,
    ) -> Result<Vec<RecordedEvent>, StoreError> {
        let Some(entered) = self.recorded_event(owner, event)? else {
            return Ok(Vec::new());
        };
        let head = self.head_of_chain(owner, entered)?;
        self.chain_from(owner, head)
    }

    /// One fact of the owner's, by identifier.
    fn recorded_event(
        &self,
        owner: OwnerId,
        event: EventId,
    ) -> Result<Option<RecordedEvent>, StoreError> {
        let row = self
            .conn
            .query_row(
                CHAIN_STEP_BACK_SQL,
                params![owner.inner().to_string(), event.inner().to_string()],
                recorded_row,
            )
            .optional()?;
        row.map(decode_recorded).transpose()
    }

    /// Follow `relation` back until nothing is named: the fact the chain began at.
    ///
    /// A target the owner's journal does not hold ends the walk at the fact that
    /// names it rather than failing it. Such a target is not a fact of his, so
    /// there is nothing further of his to show, and refusing the whole history
    /// over a link that leads out of it would hide the corrections he can see.
    fn head_of_chain(
        &self,
        owner: OwnerId,
        entered: RecordedEvent,
    ) -> Result<RecordedEvent, StoreError> {
        let mut seen = BTreeSet::new();
        let mut current = entered;
        loop {
            if !seen.insert(current.event.id) {
                return Err(cycle_at(current.event.id));
            }
            let target = match current.event.relation {
                Relation::None => return Ok(current),
                Relation::Reversal { target } | Relation::Replacement { target } => target,
            };
            match self.recorded_event(owner, target)? {
                Some(named) => current = named,
                None => return Ok(current),
            }
        }
    }

    /// Follow the index forward from the head, depth first.
    ///
    /// Depth first rather than by the recorded moment: the order the two facts
    /// of one correction were written in is the order the caller happened to
    /// send them, and a client is free to send the replacement first. The shape
    /// of the chain is not free, so it is what the order is read off.
    fn chain_from(
        &self,
        owner: OwnerId,
        head: RecordedEvent,
    ) -> Result<Vec<RecordedEvent>, StoreError> {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![head];
        while let Some(current) = pending.pop() {
            if !seen.insert(current.event.id) {
                return Err(cycle_at(current.event.id));
            }
            let mut naming = self.corrections_naming(owner, current.event.id)?;
            chain.push(current);
            // The stack pops what was pushed last, so the earliest act is
            // pushed last and comes back out first.
            naming.reverse();
            pending.extend(naming);
        }
        Ok(chain)
    }

    /// The facts that name this one as what they reverse or replace.
    ///
    /// The reversal comes before the replacement, because that is the order the
    /// act happened in: the fact stopped counting, and then another took its
    /// place. A pair written the other way round is still one act, and reading
    /// it in the order it was stored would describe the same act differently
    /// depending on how the request was assembled.
    ///
    /// The stamp and then the identifier break the tie between two facts of the
    /// same half — two reversals of one target — and exist so that the order is
    /// total and does not depend on what the query planner returned first. The
    /// stamp is compared as text, which is time order for any two stamps a whole
    /// second apart: each is written here at UTC in the one RFC 3339 form. Past
    /// the seconds it is not, because that form omits the fractional part when
    /// the nanoseconds are zero and trims its trailing zeros otherwise, so
    /// `…:00.4Z` sorts before `…:00Z` on `'.'` against `'Z'`. Two facts written
    /// inside one second can therefore come back in the wrong order, and the
    /// identifier settles only the pair whose stamps are identical. Both are
    /// arbitrary at that scale and both are stable, which is what this ordering
    /// is for: the tie is between facts that are equally the same half of the
    /// same act.
    fn corrections_naming(
        &self,
        owner: OwnerId,
        target: EventId,
    ) -> Result<Vec<RecordedEvent>, StoreError> {
        let mut statement = self.conn.prepare(CHAIN_STEP_FORWARD_SQL)?;
        let rows = statement.query_map(
            params![owner.inner().to_string(), target.inner().to_string()],
            recorded_row,
        )?;
        let mut naming = Vec::new();
        for row in rows {
            naming.push(decode_recorded(row?)?);
        }
        naming.sort_by(|left, right| {
            relation_order(&left.event)
                .cmp(&relation_order(&right.event))
                .then_with(|| left.recorded_at.cmp(&right.recorded_at))
                .then_with(|| left.event.id.cmp(&right.event.id))
        });
        Ok(naming)
    }
}

/// Read the three columns both chain statements select.
fn recorded_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

fn decode_recorded(
    (id, payload, recorded_at): (String, String, String),
) -> Result<RecordedEvent, StoreError> {
    let event: Event =
        serde_json::from_str(&payload).map_err(|source| StoreError::EventDecode { id, source })?;
    Ok(RecordedEvent { event, recorded_at })
}

/// Where a fact sits within the act that named its target.
const fn relation_order(event: &Event) -> u8 {
    match event.relation {
        Relation::Reversal { .. } => 0,
        Relation::Replacement { .. } => 1,
        // Unreachable through the forward statement, which selects only rows
        // carrying a target. Last rather than first, so that a row the schema
        // should not hold cannot displace the facts that explain the act.
        Relation::None => 2,
    }
}

fn cycle_at(event: EventId) -> StoreError {
    StoreError::CorrectionChainCycle {
        event: event.inner().to_string(),
    }
}
