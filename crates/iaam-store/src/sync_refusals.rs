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

use std::collections::BTreeSet;

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
        self.write_sync_refusal(record)
    }

    /// Record or refresh one refused row inside the caller's transaction.
    ///
    /// The same work as [`Self::upsert_sync_refusal`] and no transaction of its
    /// own: a broker sync records the rows it set aside with the facts and the
    /// gap of the same attempt, so a kill leaves no question it cannot
    /// answer (ADR 0042 point 1). Called with no transaction open it refuses,
    /// naming itself (`StoreError::NoTransaction`).
    pub fn upsert_sync_refusal_within(&self, record: &SyncRefusalRecord) -> Result<(), StoreError> {
        self.require_transaction("upsert_sync_refusal_within")?;
        self.write_sync_refusal(record)?;
        #[cfg(any(test, feature = "test-support"))]
        crate::fault::write_completed();
        Ok(())
    }

    fn write_sync_refusal(&self, record: &SyncRefusalRecord) -> Result<(), StoreError> {
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
    /// lists, among those whose interval the later sync demonstrably covers.
    ///
    /// Overlap is not enough: a refusal recorded for a March 1–31 sync (a
    /// March 5 operation refused) must not be settled by a later sync of
    /// March 31 only — that sync's silence about the row proves nothing
    /// about March 5. A record settles only when the new sync's own interval
    /// contains the record's (`from <= range_from AND range_to <= to`); a
    /// sync that covers the whole record has re-read the row and can say it
    /// is no longer refused. A row that this sync refused again stays open,
    /// however `row_keys` is ordered.
    pub fn settle_sync_refusals_besides(
        &self,
        filter: &SyncRefusalFilter,
        row_keys: &[String],
    ) -> Result<(), StoreError> {
        // The transaction here is what keeps the selection and the updates
        // under one write lock; see [`Self::settle_covered_refusals`].
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = self.settle_covered_refusals(filter, row_keys);
        match outcome {
            Ok(_) => self.conn.execute_batch("COMMIT").map_err(Into::into),
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Settle the refusals inside the caller's transaction (ADR 0042 point 1).
    ///
    /// The same work as [`Self::settle_sync_refusals_besides`] and no
    /// transaction of its own: the questions a sync settles belong to the
    /// attempt that re-read the rows, so they land with it or not at all.
    /// Returns how many records were settled. Called with no transaction open
    /// it refuses, naming itself (`StoreError::NoTransaction`).
    pub fn settle_sync_refusals_within(
        &self,
        filter: &SyncRefusalFilter,
        row_keys: &[String],
    ) -> Result<usize, StoreError> {
        self.require_transaction("settle_sync_refusals_within")?;
        let settled = self.settle_covered_refusals(filter, row_keys)?;
        #[cfg(any(test, feature = "test-support"))]
        for _ in 0..settled {
            crate::fault::write_completed();
        }
        Ok(settled)
    }

    /// The covered records are selected, their keys compared in memory,
    /// and the absent ones settled one bounded statement each — all on
    /// whichever transaction is open, the caller's or
    /// [`Self::settle_sync_refusals_besides`]'s. An unbounded NOT IN list
    /// would trip SQLite's variable limit on a refusal set of more than
    /// ~32,761 rows, and chunking the list could settle a key present in
    /// another chunk. Selecting BEFORE the begin would let a concurrent upsert
    /// widen a record's interval between the selection and the settle, and
    /// this sync does not cover the widened record — so the begin comes first
    /// and the selection lives under the write lock with the updates.
    fn settle_covered_refusals(
        &self,
        filter: &SyncRefusalFilter,
        row_keys: &[String],
    ) -> Result<usize, StoreError> {
        let SyncRefusalFilter {
            owner,
            account,
            source,
            from,
            to,
        } = filter;
        {
            let mut statement = self.conn.prepare(
                "SELECT row_key FROM sync_refusals
                 WHERE settled = 0 AND owner = ?1 AND account = ?2 AND source = ?3
                   AND ?5 <= range_from AND range_to <= ?4",
            )?;
            let covered: Vec<String> = statement
                .query_map(
                    params![
                        owner.inner().to_string(),
                        account.inner().to_string(),
                        source,
                        to,
                        from,
                    ],
                    |row| row.get(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let kept: BTreeSet<&str> = row_keys.iter().map(String::as_str).collect();
            let mut settled = 0;
            for key in &covered {
                if kept.contains(key.as_str()) {
                    continue;
                }
                let updated = self.conn.execute(
                    "UPDATE sync_refusals SET settled = 1
                     WHERE owner = ?1 AND account = ?2 AND source = ?3 AND row_key = ?4
                       AND settled = 0",
                    params![
                        owner.inner().to_string(),
                        account.inner().to_string(),
                        source,
                        key,
                    ],
                )?;
                settled += updated;
            }
            Ok(settled)
        }
    }
}

/// Whose refusals, and over which interval: the filter a settle applies.
///
/// Folded from six positional parameters so no lint allow is needed
/// (`iaam-pygg`): the owner, account and channel name the records, the
/// interval is the sync's own (ISO dates, inclusive ends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRefusalFilter {
    pub owner: OwnerId,
    pub account: AccountId,
    pub source: String,
    pub from: String,
    pub to: String,
}

fn decode_error(what: &'static str, id: &str) -> StoreError {
    StoreError::NotFound {
        what,
        id: id.to_owned(),
    }
}
