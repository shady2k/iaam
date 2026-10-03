//! Sync refusals: the rows a broker sync set aside, kept for the owner.
//!
//! The journal's coverage-gap event keeps the attempt statement — the
//! dimensions it withheld and the count — while this table keeps what the
//! owner can actually act on: each refused row's stable key, its original
//! payload, the reason and the remedy (iaam-vg8te.1.2). It is deliberately
//! NOT a journal event: it is the sync's own reading, pre-journal, and a
//! re-sync of the same unchanged row updates the record rather than minting
//! a second fact.
//!
//! The store treats the row content as opaque text (payload, reason,
//! dimensions, dates): interpreting them is the app's. Only the owner scope
//! is typed, so nothing of one owner's refusals ever answers another's.

use iaam_core::ids::{AccountId, OwnerId};
use rusqlite::params;
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// One refused row of one sync, as the owner should see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRefusalRecord {
    pub id: Uuid,
    pub owner: OwnerId,
    pub account: AccountId,
    /// The channel's source identity, as the app printed it.
    pub source: String,
    /// The row's stable key across re-imports (the fingerprint or given id).
    pub row_key: String,
    /// The interval the sync covered, ISO dates, inclusive ends.
    pub range_from: String,
    pub range_to: String,
    /// The reconciliation dimensions the refusal withheld, serialized by
    /// the app (codes joined by `|`).
    pub dimensions: String,
    /// The reason, naming what is missing and who supplies it.
    pub reason: String,
    /// The refused row's original wire JSON.
    pub payload: String,
    /// Whether a later sync showed the row no longer refused.
    pub settled: bool,
}

impl SqliteStore {
    /// Record or refresh one refused row. A re-sync of the same owner row
    /// updates the record and reopens it: the refusal stays as the account
    /// of that attempt, and the row that is refused again is asked about
    /// again. A record that stopped recurring is settled by
    /// [`SqliteStore::settle_sync_refusals_besides`].
    pub fn upsert_sync_refusal(&self, record: &SyncRefusalRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO sync_refusals
                 (id, owner, account, source, row_key, range_from, range_to,
                  dimensions, reason, payload, settled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (owner, account, source, row_key) DO UPDATE SET
                 range_from = excluded.range_from,
                 range_to = excluded.range_to,
                 dimensions = excluded.dimensions,
                 reason = excluded.reason,
                 payload = excluded.payload,
                 settled = 0
             WHERE sync_refusals.owner = excluded.owner",
            params![
                record.id.to_string(),
                record.owner.inner().to_string(),
                record.account.inner().to_string(),
                record.source,
                record.row_key,
                record.range_from,
                record.range_to,
                record.dimensions,
                record.reason,
                record.payload,
                i64::from(record.settled),
                now(),
            ],
        )?;
        Ok(())
    }

    /// The open refusals of one owner account and channel whose interval
    /// overlaps `[from, to]`, newest first.
    pub fn list_open_sync_refusals(
        &self,
        owner: OwnerId,
        account: AccountId,
        source: &str,
        from: &str,
        to: &str,
    ) -> Result<Vec<SyncRefusalRecord>, StoreError> {
        // Ordered by the row key only: `created_at` has second precision and
        // the upsert does not refresh it, so it cannot say which refusal is
        // newer. The key is the stable identity the owner recognises.
        let mut statement = self.conn.prepare(
            "SELECT id, owner, account, source, row_key, range_from, range_to,
                    dimensions, reason, payload, settled
             FROM sync_refusals
             WHERE settled = 0 AND owner = ?1 AND account = ?2 AND source = ?3
               AND range_from <= ?4 AND range_to >= ?5
             ORDER BY row_key",
        )?;
        let rows = statement.query_map(
            params![
                owner.inner().to_string(),
                account.inner().to_string(),
                source,
                to,
                from,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )?;
        let mut records = Vec::new();
        for row in rows {
            let (
                id,
                owner,
                account,
                source,
                row_key,
                range_from,
                range_to,
                dimensions,
                reason,
                payload,
                settled,
            ) = row?;
            records.push(SyncRefusalRecord {
                id: Uuid::parse_str(&id).map_err(|_| decode_error("sync refusal id", &id))?,
                owner: OwnerId(
                    Uuid::parse_str(&owner)
                        .map_err(|_| decode_error("sync refusal owner", &owner))?,
                ),
                account: AccountId(
                    Uuid::parse_str(&account)
                        .map_err(|_| decode_error("sync refusal account", &account))?,
                ),
                source,
                row_key,
                range_from,
                range_to,
                dimensions,
                reason,
                payload,
                settled: settled != 0,
            });
        }
        Ok(records)
    }

    /// Settle every open refusal of the channel that a later sync no longer
    /// lists, among those whose interval overlaps `[from, to]`. A row that
    /// this sync refused again stays open, however `row_keys` is ordered.
    pub fn settle_sync_refusals_besides(
        &self,
        owner: OwnerId,
        account: AccountId,
        source: &str,
        from: &str,
        to: &str,
        row_keys: &[String],
    ) -> Result<(), StoreError> {
        // An empty list means the sync refused nothing: everything open and
        // overlapping is settled.
        if row_keys.is_empty() {
            self.conn.execute(
                "UPDATE sync_refusals SET settled = 1
                 WHERE settled = 0 AND owner = ?1 AND account = ?2 AND source = ?3
                   AND range_from <= ?4 AND range_to >= ?5",
                params![
                    owner.inner().to_string(),
                    account.inner().to_string(),
                    source,
                    to,
                    from,
                ],
            )?;
            return Ok(());
        }
        let mut sql = String::from(
            "UPDATE sync_refusals SET settled = 1
             WHERE settled = 0 AND owner = ?1 AND account = ?2 AND source = ?3
               AND range_from <= ?4 AND range_to >= ?5
               AND row_key NOT IN (",
        );
        let mut values: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::Text(owner.inner().to_string()),
            rusqlite::types::Value::Text(account.inner().to_string()),
            rusqlite::types::Value::Text(source.to_owned()),
            rusqlite::types::Value::Text(to.to_owned()),
            rusqlite::types::Value::Text(from.to_owned()),
        ];
        for (index, key) in row_keys.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
            sql.push_str(&(values.len() + 1).to_string());
            values.push(rusqlite::types::Value::Text(key.clone()));
        }
        sql.push(')');
        self.conn
            .execute(&sql, rusqlite::params_from_iter(values))?;
        Ok(())
    }
}

fn decode_error(what: &'static str, id: &str) -> StoreError {
    StoreError::NotFound {
        what,
        id: id.to_owned(),
    }
}
