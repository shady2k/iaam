use crate::{SqliteStore, StoreError};
use iaam_core::ids::{OwnerId, PrincipalId};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

/// One reversible standing decision recorded by this instance.
///
/// The decision table is a read index for the typed records. The actor remains
/// on those records; this row lets the owner read all decision families as one
/// bounded list.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDecision {
    pub operation: String,
    pub declared_by: Option<PrincipalId>,
    pub actor_scope: Option<String>,
    pub subject: String,
    pub decision: Value,
    pub undo: String,
    pub recorded_at: String,
}

impl SqliteStore {
    #[allow(clippy::too_many_arguments)]
    pub fn record_decision(
        &mut self,
        owner: OwnerId,
        declared_by: Option<PrincipalId>,
        operation: &str,
        subject: &str,
        decision: &Value,
        undo: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO decision_history
                 (owner, declared_by, operation, subject, decision, undo, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                owner.inner().to_string(),
                declared_by.map(|id| id.inner().to_string()),
                operation,
                subject,
                serde_json::to_string(decision).map_err(StoreError::EventEncode)?,
                undo,
                crate::now(),
            ],
        )?;
        Ok(())
    }

    pub fn list_decisions(
        &self,
        owner: OwnerId,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<StoredDecision>, StoreError> {
        let mut rows = Vec::new();
        let mut statement = self.conn.prepare(
            "SELECT operation, declared_by, subject, decision, undo, recorded_at
             FROM decision_history
             WHERE owner = ?1
               AND (?2 IS NULL OR substr(recorded_at, 1, 10) >= ?2)
               AND (?3 IS NULL OR substr(recorded_at, 1, 10) <= ?3)
             ORDER BY recorded_at, id",
        )?;
        let decisions =
            statement.query_map(params![owner.inner().to_string(), from, to], |row| {
                let declared_by: Option<String> = row.get(1)?;
                let decision: String = row.get(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    declared_by,
                    row.get::<_, String>(2)?,
                    decision,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
        for row in decisions {
            let (operation, declared_by, subject, decision, undo, recorded_at) = row?;
            let actor = parse_principal(declared_by.as_deref())?;
            rows.push(StoredDecision {
                operation,
                actor_scope: self.scope_for(owner, declared_by.as_deref())?,
                declared_by: actor,
                subject,
                decision: serde_json::from_str(&decision).map_err(StoreError::EventEncode)?,
                undo,
                recorded_at,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT id, payload, recorded_at,
                    json_extract(payload, '$.provenance.declared_by')
             FROM events
             WHERE owner = ?1
               AND json_extract(payload, '$.provenance.declared_by') IS NOT NULL
               AND (?2 IS NULL OR substr(recorded_at, 1, 10) >= ?2)
               AND (?3 IS NULL OR substr(recorded_at, 1, 10) <= ?3)
             ORDER BY recorded_at, id",
        )?;
        let events = statement.query_map(params![owner.inner().to_string(), from, to], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in events {
            let (id, payload, recorded_at, declared_by) = row?;
            let actor = parse_principal(declared_by.as_deref())?;
            rows.push(StoredDecision {
                operation: "journal_event".to_owned(),
                declared_by: actor,
                actor_scope: self.scope_for(owner, declared_by.as_deref())?,
                subject: id.clone(),
                decision: serde_json::from_str(&payload)
                    .map_err(|source| StoreError::EventDecode { id, source })?,
                undo: "submit_corrections".to_owned(),
                recorded_at,
            });
        }
        rows.sort_by(|left, right| {
            left.recorded_at
                .cmp(&right.recorded_at)
                .then_with(|| left.subject.cmp(&right.subject))
        });
        Ok(rows)
    }

    fn scope_for(&self, owner: OwnerId, token: Option<&str>) -> Result<Option<String>, StoreError> {
        let Some(token) = token else { return Ok(None) };
        self.conn
            .query_row(
                "SELECT scope FROM api_tokens WHERE owner = ?1 AND id = ?2",
                params![owner.inner().to_string(), token],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)
    }
}

fn parse_principal(value: Option<&str>) -> Result<Option<PrincipalId>, StoreError> {
    value
        .map(|id| {
            uuid::Uuid::parse_str(id)
                .map(PrincipalId)
                .map_err(|_| StoreError::NotFound {
                    what: "token",
                    id: id.to_owned(),
                })
        })
        .transpose()
}
