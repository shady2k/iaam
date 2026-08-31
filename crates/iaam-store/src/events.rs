//! Fact log: recording and reading.

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::{Event, Relation};
use iaam_core::ids::{EventId, OwnerId};
use iaam_core::reconciliation::evidence::IdentityScope;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

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
