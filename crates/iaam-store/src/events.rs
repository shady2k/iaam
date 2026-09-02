//! Fact log: recording and reading.

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::kind::{CONTROL_ASSERTION_KIND, EventKind, IMPORT_COVERAGE_GAP_KIND};
use iaam_core::event::{Event, Relation};
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_core::reconciliation::evidence::IdentityScope;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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

/// The state needed to match one control assertion without loading the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlAssertionRecord {
    pub account: AccountId,
    pub period: AssertionPeriod,
    pub point: Option<BalancePoint>,
    pub dimension: Dimension,
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

    /// Summarise every owned account without loading its journal events.
    pub fn list_account_activity(
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

    /// List only an account's control assertions; payload matching stays in Rust.
    pub fn list_control_assertions(
        &self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<Vec<ControlAssertionRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, payload
             FROM events
             WHERE owner = ?1 AND account = ?2 AND kind = ?3
             ORDER BY effective_date, sequence, id",
        )?;
        let rows = statement.query_map(
            params![
                owner.inner().to_string(),
                account.inner().to_string(),
                CONTROL_ASSERTION_KIND
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut assertions = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            let event: Event = serde_json::from_str(&payload)
                .map_err(|source| StoreError::EventDecode { id, source })?;
            let EventKind::ControlAssertion { period, claim } = event.kind else {
                continue;
            };
            let point = match &claim {
                iaam_core::reconciliation::claim::ControlClaim::CashBalance { at, .. }
                | iaam_core::reconciliation::claim::ControlClaim::PositionQuantity { at, .. } => {
                    Some(*at)
                }
                _ => None,
            };
            assertions.push(ControlAssertionRecord {
                account: event.account,
                period,
                point,
                dimension: claim.dimension(),
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
             idempotency_key, raw_hash, payload, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
