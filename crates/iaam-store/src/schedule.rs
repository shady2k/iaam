//! Storage for payout schedule snapshots (§2.2 of the E3.4 spec).
//!
//! The storage does not know source formats: all values arrive as strings
//! and leave as strings. Domain type conversion happens at the application
//! boundary, as with market observations.

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// Snapshot header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSnapshotRow {
    pub instrument_id: String,
    pub source_id: String,
    pub observed_at: String,
    pub content_hash: String,
}

/// Coupon period row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouponPeriodRow {
    pub period_start: String,
    pub accrual_end: String,
    pub payment_date: String,
    pub record_date: Option<String>,
    pub amount_status: String,
    pub amount_per_unit: Option<String>,
    pub amount_currency: Option<String>,
    pub rate_percent: Option<String>,
    pub source_entry_id: Option<String>,
}

/// Principal repayment row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalRepaymentRow {
    pub repayment_date: String,
    pub share_percent: String,
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Offer window row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferWindowRow {
    pub execution_date: String,
    pub submission_start: Option<String>,
    pub submission_end: Option<String>,
    pub price_percent: Option<String>,
    pub agent: Option<String>,
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Snapshot write result.
///
/// `written = false` means that the contents matched the latest snapshot
/// and no new write was needed. This is not an error, and it must not be
/// silenced: “written” and “the same content was already present” are different events in the run trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotOutcome {
    pub snapshot_id: String,
    pub written: bool,
}

/// A snapshot read at a knowledge coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub snapshot_id: String,
    pub observed_at: String,
    pub coupon_periods: Vec<CouponPeriodRow>,
    pub principal_repayments: Vec<PrincipalRepaymentRow>,
    pub offer_windows: Vec<OfferWindowRow>,
}

/// Stored snapshot completeness verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleCompletenessRow {
    pub fetch_exhausted: bool,
    pub structurally_validated: bool,
    pub incomplete_reason: Option<String>,
}

/// Issue terms row. All values are strings, as everywhere in the
/// storage: it does not know source formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueTermsRow {
    pub instrument_id: String,
    pub source_id: String,
    pub observed_at: String,
    pub effective_from: Option<String>,
    pub maturity_date: Option<String>,
    pub initial_face_value: Option<String>,
    pub face_currency_code: Option<String>,
    pub coupon_periods_per_year: Option<i64>,
    pub day_count: Option<String>,
    pub calendar: Option<String>,
    pub default_declared: bool,
    pub default_technical: bool,
}

impl SqliteStore {
    /// Write the complete payout schedule snapshot.
    ///
    /// If the contents match the latest snapshot for the same series, no new
    /// write is created: a snapshot is not an observation if nothing was
    /// observed again.
    pub fn record_schedule_snapshot(
        &mut self,
        header: &ScheduleSnapshotRow,
        coupon_periods: &[CouponPeriodRow],
        principal_repayments: &[PrincipalRepaymentRow],
        offer_windows: &[OfferWindowRow],
    ) -> Result<SnapshotOutcome, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let latest: Option<(String, String)> = transaction
            .query_row(
                "SELECT id, content_hash FROM schedule_snapshots
                 WHERE instrument_id = ?1 AND source_id = ?2
                 ORDER BY observed_at DESC, id DESC
                 LIMIT 1",
                params![&header.instrument_id, &header.source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, hash)) = latest
            && hash == header.content_hash
        {
            transaction.commit()?;
            return Ok(SnapshotOutcome {
                snapshot_id: id,
                written: false,
            });
        }

        let id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO schedule_snapshots
                 (id, instrument_id, source_id, observed_at, content_hash, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &id,
                &header.instrument_id,
                &header.source_id,
                &header.observed_at,
                &header.content_hash,
                now(),
            ],
        )?;
        for row in coupon_periods {
            transaction.execute(
                "INSERT INTO schedule_coupon_periods
                     (snapshot_id, period_start, accrual_end, payment_date, record_date,
                      amount_status, amount_per_unit, amount_currency, rate_percent,
                      source_entry_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    &id,
                    &row.period_start,
                    &row.accrual_end,
                    &row.payment_date,
                    &row.record_date,
                    &row.amount_status,
                    &row.amount_per_unit,
                    &row.amount_currency,
                    &row.rate_percent,
                    &row.source_entry_id,
                ],
            )?;
        }
        for row in principal_repayments {
            transaction.execute(
                "INSERT INTO schedule_principal_repayments
                     (snapshot_id, repayment_date, share_percent, source_kind, source_entry_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    &id,
                    &row.repayment_date,
                    &row.share_percent,
                    &row.source_kind,
                    &row.source_entry_id,
                ],
            )?;
        }
        for row in offer_windows {
            transaction.execute(
                "INSERT INTO schedule_offer_windows
                     (snapshot_id, execution_date, submission_start, submission_end,
                      price_percent, agent, source_kind, source_entry_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &id,
                    &row.execution_date,
                    &row.submission_start,
                    &row.submission_end,
                    &row.price_percent,
                    &row.agent,
                    &row.source_kind,
                    &row.source_entry_id,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(SnapshotOutcome {
            snapshot_id: id,
            written: true,
        })
    }

    /// The last complete snapshot no later than the knowledge coordinate.
    ///
    /// As a whole, not row by row: rows from different snapshots must not be mixed—
    /// the schedule assembled from them would describe an issuance that did not
    /// exist at any point in time.
    pub fn schedule_at_or_before(
        &self,
        instrument_id: &str,
        source_id: &str,
        knowledge_as_of: &str,
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let header: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT id, observed_at FROM schedule_snapshots
                 WHERE instrument_id = ?1 AND source_id = ?2 AND observed_at <= ?3
                 ORDER BY observed_at DESC, id DESC
                 LIMIT 1",
                params![instrument_id, source_id, knowledge_as_of],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((snapshot_id, observed_at)) = header else {
            return Ok(None);
        };

        let mut coupons = self.conn.prepare(
            "SELECT period_start, accrual_end, payment_date, record_date, amount_status,
                    amount_per_unit, amount_currency, rate_percent, source_entry_id
             FROM schedule_coupon_periods WHERE snapshot_id = ?1 ORDER BY period_start",
        )?;
        let coupon_periods = coupons
            .query_map([&snapshot_id], |row| {
                Ok(CouponPeriodRow {
                    period_start: row.get(0)?,
                    accrual_end: row.get(1)?,
                    payment_date: row.get(2)?,
                    record_date: row.get(3)?,
                    amount_status: row.get(4)?,
                    amount_per_unit: row.get(5)?,
                    amount_currency: row.get(6)?,
                    rate_percent: row.get(7)?,
                    source_entry_id: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut repayments = self.conn.prepare(
            "SELECT repayment_date, share_percent, source_kind, source_entry_id
             FROM schedule_principal_repayments WHERE snapshot_id = ?1 ORDER BY repayment_date",
        )?;
        let principal_repayments = repayments
            .query_map([&snapshot_id], |row| {
                Ok(PrincipalRepaymentRow {
                    repayment_date: row.get(0)?,
                    share_percent: row.get(1)?,
                    source_kind: row.get(2)?,
                    source_entry_id: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut windows = self.conn.prepare(
            "SELECT execution_date, submission_start, submission_end, price_percent,
                    agent, source_kind, source_entry_id
             FROM schedule_offer_windows WHERE snapshot_id = ?1 ORDER BY execution_date",
        )?;
        let offer_windows = windows
            .query_map([&snapshot_id], |row| {
                Ok(OfferWindowRow {
                    execution_date: row.get(0)?,
                    submission_start: row.get(1)?,
                    submission_end: row.get(2)?,
                    price_percent: row.get(3)?,
                    agent: row.get(4)?,
                    source_kind: row.get(5)?,
                    source_entry_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(StoredSnapshot {
            snapshot_id,
            observed_at,
            coupon_periods,
            principal_repayments,
            offer_windows,
        }))
    }

    /// Read the saved completeness verdict for the entire snapshot.
    pub fn schedule_completeness(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<ScheduleCompletenessRow>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT fetch_exhausted, structurally_validated, incomplete_reason
                 FROM schedule_completeness
                 WHERE snapshot_id = ?1",
                [snapshot_id],
                |row| {
                    Ok(ScheduleCompletenessRow {
                        fetch_exhausted: row.get::<_, i64>(0)? != 0,
                        structurally_validated: row.get::<_, i64>(1)? != 0,
                        incomplete_reason: row.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Record three assertions about snapshot completeness.
    ///
    /// Three, not one: “the source was read to the end” and “the domain schedule
    /// is sufficient” are different assertions, and a source read completely
    /// with a gap inside would pass as complete.
    pub fn record_schedule_completeness(
        &mut self,
        snapshot_id: &str,
        fetch_exhausted: bool,
        structurally_validated: bool,
        incomplete_reason: Option<&str>,
        pages_seen: &[u32],
    ) -> Result<(), StoreError> {
        let pages = pages_seen
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.conn.execute(
            "INSERT INTO schedule_completeness
                 (snapshot_id, fetch_exhausted, structurally_validated,
                  incomplete_reason, pages_seen, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (snapshot_id) DO UPDATE SET
                 fetch_exhausted = excluded.fetch_exhausted,
                 structurally_validated = excluded.structurally_validated,
                 incomplete_reason = excluded.incomplete_reason,
                 pages_seen = excluded.pages_seen,
                 updated_at = excluded.updated_at",
            params![
                snapshot_id,
                i64::from(fetch_exhausted),
                i64::from(structurally_validated),
                incomplete_reason,
                format!("[{pages}]"),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Record an observation of the issuance conditions.
    ///
    /// `INSERT OR IGNORE`, not `UPSERT`: the observation is append-only, and
    /// recording the same `observed_at` again is the same observation,
    /// not a correction. A correction arrives with a new `observed_at`.
    pub fn record_issue_terms(&mut self, row: &IssueTermsRow) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO issue_terms
                 (instrument_id, source_id, observed_at, effective_from, maturity_date,
                  initial_face_value, face_currency_code, coupon_periods_per_year,
                  day_count, calendar, default_declared, default_technical, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &row.instrument_id,
                &row.source_id,
                &row.observed_at,
                &row.effective_from,
                &row.maturity_date,
                &row.initial_face_value,
                &row.face_currency_code,
                &row.coupon_periods_per_year,
                &row.day_count,
                &row.calendar,
                i64::from(row.default_declared),
                i64::from(row.default_technical),
                now(),
            ],
        )?;
        Ok(())
    }

    /// The latest observation of the conditions no later than the knowledge coordinate.
    pub fn issue_terms_at_or_before(
        &self,
        instrument_id: &str,
        source_id: &str,
        knowledge_as_of: &str,
    ) -> Result<Option<IssueTermsRow>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT instrument_id, source_id, observed_at, effective_from, maturity_date,
                        initial_face_value, face_currency_code, coupon_periods_per_year,
                        day_count, calendar, default_declared, default_technical
                 FROM issue_terms
                 WHERE instrument_id = ?1 AND source_id = ?2 AND observed_at <= ?3
                 ORDER BY observed_at DESC
                 LIMIT 1",
                params![instrument_id, source_id, knowledge_as_of],
                |row| {
                    Ok(IssueTermsRow {
                        instrument_id: row.get(0)?,
                        source_id: row.get(1)?,
                        observed_at: row.get(2)?,
                        effective_from: row.get(3)?,
                        maturity_date: row.get(4)?,
                        initial_face_value: row.get(5)?,
                        face_currency_code: row.get(6)?,
                        coupon_periods_per_year: row.get(7)?,
                        day_count: row.get(8)?,
                        calendar: row.get(9)?,
                        default_declared: row.get::<_, i64>(10)? != 0,
                        default_technical: row.get::<_, i64>(11)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}
