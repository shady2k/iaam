//! Снимки проекций.
//!
//! Снимок — **кэш**: его потеря не является потерей данных, потому что
//! он восстановим полным пересчётом журнала. Поэтому формат выбран
//! из соображений «переживёт ли он состав состояния», а не долговечности.
//!
//! Формат — CBOR, а не JSON. Состояние содержит карты с составными
//! ключами (счёт + валюта, счёт + место хранения + инструмент), которые
//! JSON представить не может: `serde_json` отказывает с «key must be
//! a string». Проверено исполнением.

use iaam_core::contour::{ContourId, ContourVersion};
use iaam_core::ids::OwnerId;
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{SqliteStore, StoreError};

impl SqliteStore {
    /// Сохранение снимка. Ключ — контур, его версия и версия правила
    /// списания: снимок, построенный другими правилами, не является
    /// снимком того же расчёта.
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

    /// Чтение снимка.
    ///
    /// Снимок, который не читается, — не ошибка работы: формат мог
    /// измениться вместе с версией проекции. Вызывающий получает `None`
    /// и пересчитывает журнал с нуля.
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

    /// Удаление снимка. Единственная операция удаления в хранилище:
    /// кэш выбрасывать можно, факты — нет.
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
