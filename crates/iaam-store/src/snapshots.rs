//! Projection snapshots.
//!
//! A snapshot is a **cache**: losing it does not mean losing data because
//! it can be restored by fully replaying the journal. Therefore, the format was chosen
//! based on whether it will survive the state schema, not on durability.
//!
//! The format is CBOR, not JSON. The state contains maps with composite
//! keys (account + currency, account + custody + instrument), which JSON cannot
//! represent: `serde_json` fails with “key must be a string”. Verified by running it.
//!

use iaam_core::contour::{ContourId, ContourVersion};
use iaam_core::ids::OwnerId;
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{SqliteStore, StoreError};

impl SqliteStore {
    /// Save a snapshot. The key is the projection, its version, and the rule version
    /// for debiting: a snapshot built with different rules is not
    /// a snapshot of the same calculation.
    pub fn save_snapshot(&self, owner: OwnerId, snapshot: &Snapshot) -> Result<(), StoreError> {
        let mut body = Vec::new();
        ciborium::into_writer(snapshot, &mut body)
            .map_err(|error| StoreError::SnapshotEncode(error.to_string()))?;
        let created_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));

        self.conn.execute(
            "INSERT INTO snapshots (
                 owner, contour, contour_version, lot_rule, projection_version,
                 through_date, through_sequence, fingerprint, body, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (owner, contour, contour_version, lot_rule) DO UPDATE SET
                 projection_version = excluded.projection_version,
                 through_date       = excluded.through_date,
                 through_sequence   = excluded.through_sequence,
                 fingerprint        = excluded.fingerprint,
                 body               = excluded.body,
                 created_at         = excluded.created_at",
            params![
                owner.inner().to_string(),
                snapshot.contour().0.to_string(),
                snapshot.contour_version().0,
                snapshot.lot_rule().0,
                snapshot.projection_version(),
                snapshot.through().map(|order| order.date().to_string()),
                snapshot.through().map(|order| order.sequence()),
                snapshot.fingerprint().to_string(),
                body,
                created_at,
            ],
        )?;
        Ok(())
    }

    /// Read a snapshot.
    ///
    /// A snapshot that cannot be read is not an operational error: the format may have
    /// changed along with the projection version. The caller receives `None`
    /// and replays the journal from scratch.
    pub fn load_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<Option<Snapshot>, StoreError> {
        let body: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT body FROM snapshots
                 WHERE owner = ?1 AND contour = ?2 AND contour_version = ?3 AND lot_rule = ?4",
                params![
                    owner.inner().to_string(),
                    contour.0.to_string(),
                    version.0,
                    lot_rule.0
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(body) = body else {
            return Ok(None);
        };
        Ok(ciborium::from_reader(body.as_slice()).ok())
    }

    /// Delete a snapshot. The only deletion operation in the store:
    /// the cache may be discarded; facts may not.
    pub fn drop_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM snapshots
             WHERE owner = ?1 AND contour = ?2 AND contour_version = ?3 AND lot_rule = ?4",
            params![
                owner.inner().to_string(),
                contour.0.to_string(),
                version.0,
                lot_rule.0
            ],
        )?;
        Ok(())
    }
}
