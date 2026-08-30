//! Storage for market observations and atomic publication of series.
//!
//! The store is unaware of source formats. It accepts its own string table
//! rows; source observation conversion is performed at the application boundary.
//! of the application.

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::{Date, OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// Unit of completeness: source, dataset, and specific series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesKey {
    pub source_id: String,
    pub dataset: String,
    pub series_key: String,
}

/// Price table row. All source values remain strings until the application
/// boundary, so the store does not depend on the source format crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceRow {
    pub instrument_id: String,
    pub board: String,
    pub session: i64,
    pub trade_date: String,
    pub kind: String,
    pub observed_at: String,
    pub price: String,
    pub currency: String,
    /// Quote basis code. Stored as a string, like all other source values,
    /// so the store does not depend on the source format crate.
    pub quotation_basis: String,
    /// Indicator specifying how the basis was derived.
    pub basis_evidence: String,
    pub executability: String,
}

/// Accrued coupon income observation row. Values are strings, as everywhere else in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccruedInterestRow {
    pub instrument_id: String,
    pub board: String,
    pub session: i64,
    pub trade_date: String,
    pub observed_at: String,
    /// Per security.
    pub per_unit: String,
    pub currency: String,
}

/// Venue and session used to select a price row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceVenue {
    pub board: String,
    pub session: i64,
}

/// Trading-series window and knowledge coordinate for reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketWindow<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub knowledge_as_of: &'a str,
}

/// Foreign-exchange rates table row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxRow {
    pub from_code: String,
    pub to_code: String,
    pub trade_date: String,
    pub observed_at: String,
    pub nominal: u32,
    pub value: String,
    pub unit_rate: String,
}

/// Key interest rate table row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRateRow {
    pub trade_date: String,
    pub observed_at: String,
    pub rate: String,
}

/// Run holding a lease on one series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunHandle {
    pub id: String,
    pub lease_token: String,
    pub series: SeriesKey,
}

/// Actual covered range reported by the completed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub from: Date,
    pub to: Date,
}

/// Name used by the application layer to designate the observations API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Succeeded,
    Partial { reason: String },
    Failed { reason: String },
}

/// Name used by the application layer for the observations API.
///
/// As with the other `iaam-store` domain modules, the implementation lives on
/// `SqliteStore`: this preserves one transaction and one connection.
pub type MarketStore = SqliteStore;

type RunRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

impl SqliteStore {
    /// Acquire a series lease and create an unfinished run.
    ///
    /// Expired `running` runs are moved to `failed` in the same
    /// transaction. They cannot be deleted: an external key references their observation
    /// rows, and the run history must be preserved.
    pub fn begin_run(
        &mut self,
        series: SeriesKey,
        requested_from: Date,
        requested_to: Date,
        lease_expires_at: OffsetDateTime,
    ) -> Result<RunHandle, StoreError> {
        if requested_to < requested_from {
            return Err(StoreError::InvalidValue {
                field: "requested_to",
                value: requested_to.to_string(),
            });
        }
        if lease_expires_at <= OffsetDateTime::now_utc() {
            return Err(StoreError::LeaseExpired);
        }

        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        transaction.execute(
            "UPDATE sync_runs
             SET status = 'failed', finished_at = ?1,
                 page_errors = '[\"lease_expired\"]',
                 lease_token = NULL, lease_expires_at = NULL
             WHERE status = 'running'
               AND lease_expires_at IS NOT NULL
               AND lease_expires_at <= ?1",
            [&now],
        )?;

        let active: Option<(String,)> = transaction
            .query_row(
                "SELECT id FROM sync_runs
                 WHERE source_id = ?1 AND dataset = ?2 AND series_key = ?3
                   AND status = 'running'",
                params![&series.source_id, &series.dataset, &series.series_key],
                |row| Ok((row.get(0)?,)),
            )
            .optional()?;
        if active.is_some() {
            return Err(StoreError::LeaseHeld {
                source_id: series.source_id,
                dataset: series.dataset,
                series_key: series.series_key,
            });
        }

        let run = RunHandle {
            id: Uuid::new_v4().to_string(),
            lease_token: Uuid::new_v4().to_string(),
            series,
        };
        let expires = format_datetime(lease_expires_at)?;
        transaction.execute(
            "INSERT INTO sync_runs
                 (id, source_id, dataset, series_key, status,
                  requested_from, requested_to, started_at,
                  lease_token, lease_expires_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?8, ?9)",
            params![
                &run.id,
                &run.series.source_id,
                &run.series.dataset,
                &run.series.series_key,
                format_date(requested_from),
                format_date(requested_to),
                &now,
                &run.lease_token,
                expires,
            ],
        )?;
        transaction.commit()?;
        Ok(run)
    }

    /// Write a price page to an unfinished run.
    pub fn record_prices(
        &mut self,
        run: &RunHandle,
        raw_hash: &str,
        observations: &[PriceRow],
    ) -> Result<usize, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_run(&transaction, run)?;
        for observation in observations {
            transaction.execute(
                "INSERT INTO price_observations
                     (instrument_id, board, session, trade_date, kind, source_id,
                      observed_at, price, currency, quotation_basis, basis_evidence,
                      executability, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    &observation.instrument_id,
                    &observation.board,
                    observation.session,
                    &observation.trade_date,
                    &observation.kind,
                    &run.series.source_id,
                    &observation.observed_at,
                    &observation.price,
                    &observation.currency,
                    &observation.quotation_basis,
                    &observation.basis_evidence,
                    &observation.executability,
                    raw_hash,
                    &run.id,
                ],
            )?;
        }
        let count = i64::try_from(observations.len()).map_err(|_| StoreError::InvalidValue {
            field: "rows",
            value: observations.len().to_string(),
        })?;
        transaction.execute(
            "UPDATE sync_runs SET pages = pages + 1, rows = rows + ?1 WHERE id = ?2",
            params![count, &run.id],
        )?;
        transaction.commit()?;
        Ok(observations.len())
    }

    /// Write an accrued-interest observation page to an unfinished run.
    pub fn record_accrued_interest(
        &mut self,
        run: &RunHandle,
        raw_hash: &str,
        observations: &[AccruedInterestRow],
    ) -> Result<usize, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_run(&transaction, run)?;
        for observation in observations {
            transaction.execute(
                "INSERT INTO accrued_interest_observations
                     (instrument_id, board, session, trade_date, source_id,
                      observed_at, per_unit, currency, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    &observation.instrument_id,
                    &observation.board,
                    observation.session,
                    &observation.trade_date,
                    &run.series.source_id,
                    &observation.observed_at,
                    &observation.per_unit,
                    &observation.currency,
                    raw_hash,
                    &run.id,
                ],
            )?;
        }
        let count = i64::try_from(observations.len()).map_err(|_| StoreError::InvalidValue {
            field: "rows",
            value: observations.len().to_string(),
        })?;
        transaction.execute(
            "UPDATE sync_runs SET pages = pages + 1, rows = rows + ?1 WHERE id = ?2",
            params![count, &run.id],
        )?;
        transaction.commit()?;
        Ok(observations.len())
    }

    /// Write an FX rate page to an unfinished run.
    pub fn record_fx(
        &mut self,
        run: &RunHandle,
        raw_hash: &str,
        observations: &[FxRow],
    ) -> Result<usize, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_run(&transaction, run)?;
        for observation in observations {
            transaction.execute(
                "INSERT INTO fx_observations
                     (from_code, to_code, trade_date, source_id, observed_at,
                      nominal, value, unit_rate, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    &observation.from_code,
                    &observation.to_code,
                    &observation.trade_date,
                    &run.series.source_id,
                    &observation.observed_at,
                    i64::from(observation.nominal),
                    &observation.value,
                    &observation.unit_rate,
                    raw_hash,
                    &run.id,
                ],
            )?;
        }
        let count = i64::try_from(observations.len()).map_err(|_| StoreError::InvalidValue {
            field: "rows",
            value: observations.len().to_string(),
        })?;
        transaction.execute(
            "UPDATE sync_runs SET pages = pages + 1, rows = rows + ?1 WHERE id = ?2",
            params![count, &run.id],
        )?;
        transaction.commit()?;
        Ok(observations.len())
    }

    /// Write a key rate page to an unfinished run.
    pub fn record_key_rate(
        &mut self,
        run: &RunHandle,
        raw_hash: &str,
        observations: &[KeyRateRow],
    ) -> Result<usize, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_run(&transaction, run)?;
        for observation in observations {
            transaction.execute(
                "INSERT INTO key_rate_observations
                     (trade_date, source_id, observed_at, rate, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &observation.trade_date,
                    &run.series.source_id,
                    &observation.observed_at,
                    &observation.rate,
                    raw_hash,
                    &run.id,
                ],
            )?;
        }
        let count = i64::try_from(observations.len()).map_err(|_| StoreError::InvalidValue {
            field: "rows",
            value: observations.len().to_string(),
        })?;
        transaction.execute(
            "UPDATE sync_runs SET pages = pages + 1, rows = rows + ?1 WHERE id = ?2",
            params![count, &run.id],
        )?;
        transaction.commit()?;
        Ok(observations.len())
    }

    /// Complete the run and, only on success, advance the completeness boundary.
    pub fn finish_run(
        &mut self,
        run: &RunHandle,
        outcome: RunOutcome,
        coverage: Option<Coverage>,
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (requested_from, requested_to) = ensure_run(&transaction, run)?;
        let coverage = coverage.unwrap_or(Coverage {
            from: requested_from,
            to: requested_to,
        });
        if coverage.to < coverage.from {
            return Err(StoreError::InvalidValue {
                field: "covered_to",
                value: coverage.to.to_string(),
            });
        }

        let (status, page_errors, publish) = match outcome {
            RunOutcome::Succeeded => ("succeeded", String::from("[]"), true),
            RunOutcome::Partial { reason } => (
                "partial",
                serde_json::to_string(&[reason]).map_err(StoreError::EventEncode)?,
                false,
            ),
            RunOutcome::Failed { reason } => (
                "failed",
                serde_json::to_string(&[reason]).map_err(StoreError::EventEncode)?,
                false,
            ),
        };
        let changed = transaction.execute(
            "UPDATE sync_runs
             SET status = ?1, covered_from = ?2, covered_to = ?3,
                 page_errors = ?4, finished_at = ?5,
                 lease_token = NULL, lease_expires_at = NULL
             WHERE id = ?6 AND status = 'running'",
            params![
                status,
                format_date(coverage.from),
                format_date(coverage.to),
                page_errors,
                now(),
                &run.id,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RunNotFound);
        }
        if publish {
            let complete = format_date(coverage.to);
            transaction.execute(
                "INSERT INTO series_completeness
                     (source_id, dataset, series_key, complete_through,
                      updated_at, last_successful_run)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (source_id, dataset, series_key) DO UPDATE SET
                     complete_through = CASE
                         WHEN series_completeness.complete_through IS NULL
                              OR excluded.complete_through > series_completeness.complete_through
                         THEN excluded.complete_through
                         ELSE series_completeness.complete_through
                     END,
                     updated_at = excluded.updated_at,
                     last_successful_run = excluded.last_successful_run",
                params![
                    &run.series.source_id,
                    &run.series.dataset,
                    &run.series.series_key,
                    complete,
                    now(),
                    &run.id,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Return the series' full-publication boundary.
    pub fn complete_through(&self, series: &SeriesKey) -> Result<Option<Date>, StoreError> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT complete_through FROM series_completeness
                 WHERE source_id = ?1 AND dataset = ?2 AND series_key = ?3",
                params![&series.source_id, &series.dataset, &series.series_key],
                |row| row.get(0),
            )
            .optional()?;
        value.map_or(Ok(None), |date| parse_date(&date).map(Some))
    }

    /// Return the boundary published no later than the knowledge time.
    pub fn complete_through_at_or_before(
        &self,
        series: &SeriesKey,
        knowledge_as_of: &str,
    ) -> Result<Option<Date>, StoreError> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT complete_through FROM series_completeness
                 WHERE source_id = ?1 AND dataset = ?2 AND series_key = ?3
                   AND updated_at <= ?4",
                params![
                    &series.source_id,
                    &series.dataset,
                    &series.series_key,
                    knowledge_as_of,
                ],
                |row| row.get(0),
            )
            .optional()?;
        value.map_or(Ok(None), |date| parse_date(&date).map(Some))
    }

    /// Return the latest published observation by trading date and knowledge time.
    pub fn prices_at_or_before(
        &self,
        instrument_id: &str,
        venue: &PriceVenue,
        as_of: &str,
        knowledge_as_of: &str,
    ) -> Result<Option<PriceRow>, StoreError> {
        self.conn
            .query_row(
                "SELECT p.instrument_id, p.board, p.session, p.trade_date,
                        p.kind, p.observed_at, p.price, p.currency,
                        p.quotation_basis, p.basis_evidence, p.executability
                 FROM price_observations AS p
                 JOIN sync_runs AS r ON r.id = p.sync_run_id
                 WHERE p.instrument_id = ?1
                   AND p.board = ?2
                   AND p.session = ?3
                   AND p.trade_date <= ?4
                   AND p.observed_at <= ?5
                   AND r.status = 'succeeded'
                 ORDER BY p.trade_date DESC, p.observed_at DESC
                 LIMIT 1",
                params![
                    instrument_id,
                    &venue.board,
                    venue.session,
                    as_of,
                    knowledge_as_of
                ],
                |row| {
                    Ok(PriceRow {
                        instrument_id: row.get(0)?,
                        board: row.get(1)?,
                        session: row.get(2)?,
                        trade_date: row.get(3)?,
                        kind: row.get(4)?,
                        observed_at: row.get(5)?,
                        price: row.get(6)?,
                        currency: row.get(7)?,
                        quotation_basis: row.get(8)?,
                        basis_evidence: row.get(9)?,
                        executability: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Return the latest published accrued-interest observation by trading date and knowledge time.
    pub fn accrued_interest_at_or_before(
        &self,
        instrument_id: &str,
        venue: &PriceVenue,
        as_of: &str,
        knowledge_as_of: &str,
    ) -> Result<Option<AccruedInterestRow>, StoreError> {
        self.conn
            .query_row(
                "SELECT a.instrument_id, a.board, a.session, a.trade_date,
                        a.observed_at, a.per_unit, a.currency
                 FROM accrued_interest_observations AS a
                 JOIN sync_runs AS r ON r.id = a.sync_run_id
                 WHERE a.instrument_id = ?1
                   AND a.board = ?2
                   AND a.session = ?3
                   AND a.trade_date <= ?4
                   AND a.observed_at <= ?5
                   AND r.status = 'succeeded'
                 ORDER BY a.trade_date DESC, a.observed_at DESC
                 LIMIT 1",
                params![
                    instrument_id,
                    &venue.board,
                    venue.session,
                    as_of,
                    knowledge_as_of,
                ],
                |row| {
                    Ok(AccruedInterestRow {
                        instrument_id: row.get(0)?,
                        board: row.get(1)?,
                        session: row.get(2)?,
                        trade_date: row.get(3)?,
                        observed_at: row.get(4)?,
                        per_unit: row.get(5)?,
                        currency: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }
    /// Return all published instrument price observations in the range
    /// as of the knowledge time, without selecting a venue in advance.
    ///
    /// Venue and session are not specified: which one to use is a decision
    /// of valuation policy (E3.3), not of the query. The source and set
    /// remain parameters: hard-coding them would mean creating a second source
    /// Silently make prices invisible.
    pub fn prices_for_instrument_between(
        &self,
        source_id: &str,
        dataset: &str,
        instrument_id: &str,
        window: MarketWindow<'_>,
    ) -> Result<Vec<PriceRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT p.instrument_id, p.board, p.session, p.trade_date,
                    p.kind, p.observed_at, p.price, p.currency,
                    p.quotation_basis, p.basis_evidence, p.executability
             FROM price_observations AS p
             JOIN sync_runs AS r ON r.id = p.sync_run_id
             WHERE p.source_id = ?1
               AND r.source_id = ?1
               AND r.dataset = ?2
               AND p.instrument_id = ?3
               AND p.trade_date BETWEEN ?4 AND ?5
               AND p.observed_at <= ?6
               AND r.status = 'succeeded'
             ORDER BY p.trade_date, p.observed_at, p.board, p.session, p.kind",
        )?;
        let rows = statement.query_map(
            params![
                source_id,
                dataset,
                instrument_id,
                window.from,
                window.to,
                window.knowledge_as_of,
            ],
            |row| {
                Ok(PriceRow {
                    instrument_id: row.get(0)?,
                    board: row.get(1)?,
                    session: row.get(2)?,
                    trade_date: row.get(3)?,
                    kind: row.get(4)?,
                    observed_at: row.get(5)?,
                    price: row.get(6)?,
                    currency: row.get(7)?,
                    quotation_basis: row.get(8)?,
                    basis_evidence: row.get(9)?,
                    executability: row.get(10)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Return published price observations within the range as of the time of knowledge.
    pub fn prices_between(
        &self,
        series: &SeriesKey,
        instrument_id: &str,
        venue: &PriceVenue,
        window: MarketWindow<'_>,
    ) -> Result<Vec<PriceRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT p.instrument_id, p.board, p.session, p.trade_date,
                    p.kind, p.observed_at, p.price, p.currency,
                    p.quotation_basis, p.basis_evidence, p.executability
             FROM price_observations AS p
             JOIN sync_runs AS r ON r.id = p.sync_run_id
             WHERE p.source_id = ?1
               AND r.source_id = ?1
               AND r.dataset = ?2
               AND r.series_key = ?3
               AND p.instrument_id = ?4
               AND p.board = ?5
               AND p.session = ?6
               AND p.trade_date BETWEEN ?7 AND ?8
               AND p.observed_at <= ?9
               AND r.status = 'succeeded'
             ORDER BY p.trade_date, p.observed_at, p.kind",
        )?;
        let rows = statement.query_map(
            params![
                &series.source_id,
                &series.dataset,
                &series.series_key,
                instrument_id,
                &venue.board,
                venue.session,
                window.from,
                window.to,
                window.knowledge_as_of,
            ],
            |row| {
                Ok(PriceRow {
                    instrument_id: row.get(0)?,
                    board: row.get(1)?,
                    session: row.get(2)?,
                    trade_date: row.get(3)?,
                    kind: row.get(4)?,
                    observed_at: row.get(5)?,
                    price: row.get(6)?,
                    currency: row.get(7)?,
                    quotation_basis: row.get(8)?,
                    basis_evidence: row.get(9)?,
                    executability: row.get(10)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Return published exchange-rate observations within the range as of the time of knowledge.
    pub fn fx_between(
        &self,
        series: &SeriesKey,
        from_code: &str,
        to_code: &str,
        window: MarketWindow<'_>,
    ) -> Result<Vec<FxRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT f.from_code, f.to_code, f.trade_date, f.observed_at,
                    f.nominal, f.value, f.unit_rate
             FROM fx_observations AS f
             JOIN sync_runs AS r ON r.id = f.sync_run_id
             WHERE f.source_id = ?1
               AND r.source_id = ?1
               AND r.dataset = ?2
               AND r.series_key = ?3
               AND f.from_code = ?4
               AND f.to_code = ?5
               AND f.trade_date BETWEEN ?6 AND ?7
               AND f.observed_at <= ?8
               AND r.status = 'succeeded'
             ORDER BY f.trade_date, f.observed_at",
        )?;
        let rows = statement.query_map(
            params![
                &series.source_id,
                &series.dataset,
                &series.series_key,
                from_code,
                to_code,
                window.from,
                window.to,
                window.knowledge_as_of,
            ],
            |row| {
                Ok(FxRow {
                    from_code: row.get(0)?,
                    to_code: row.get(1)?,
                    trade_date: row.get(2)?,
                    observed_at: row.get(3)?,
                    nominal: row.get(4)?,
                    value: row.get(5)?,
                    unit_rate: row.get(6)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Return published key-rate observations within the range.
    pub fn key_rates_through(
        &self,
        series: &SeriesKey,
        to: &str,
        knowledge_as_of: &str,
    ) -> Result<Vec<KeyRateRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT k.trade_date, k.observed_at, k.rate
             FROM key_rate_observations AS k
             JOIN sync_runs AS r ON r.id = k.sync_run_id
             WHERE k.source_id = ?1
               AND r.source_id = ?1
               AND r.dataset = ?2
               AND r.series_key = ?3
               AND k.trade_date <= ?4
               AND k.observed_at <= ?5
               AND r.status = 'succeeded'
             ORDER BY k.trade_date, k.observed_at",
        )?;
        let rows = statement.query_map(
            params![
                &series.source_id,
                &series.dataset,
                &series.series_key,
                to,
                knowledge_as_of,
            ],
            |row| {
                Ok(KeyRateRow {
                    trade_date: row.get(0)?,
                    observed_at: row.get(1)?,
                    rate: row.get(2)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

fn ensure_run(transaction: &Transaction<'_>, run: &RunHandle) -> Result<(Date, Date), StoreError> {
    let row: Option<RunRow> = transaction
        .query_row(
            "SELECT source_id, dataset, series_key, status,
                    requested_from, requested_to, lease_expires_at
             FROM sync_runs WHERE id = ?1 AND lease_token = ?2",
            params![&run.id, &run.lease_token],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((source_id, dataset, series_key, status, requested_from, requested_to, expires)) = row
    else {
        return Err(StoreError::RunNotFound);
    };
    if source_id != run.series.source_id
        || dataset != run.series.dataset
        || series_key != run.series.series_key
    {
        return Err(StoreError::RunNotFound);
    }
    if status != "running" {
        return Err(StoreError::RunNotFound);
    }
    let expires = expires.ok_or(StoreError::LeaseExpired)?;
    let expires =
        OffsetDateTime::parse(&expires, &Rfc3339).map_err(|_| StoreError::InvalidValue {
            field: "lease_expires_at",
            value: expires,
        })?;
    if expires <= OffsetDateTime::now_utc() {
        return Err(StoreError::LeaseExpired);
    }
    Ok((parse_date(&requested_from)?, parse_date(&requested_to)?))
}

fn format_date(date: Date) -> String {
    date.format(&Iso8601::DATE)
        .expect("ISO-8601 date formatting is infallible")
}

fn format_datetime(value: OffsetDateTime) -> Result<String, StoreError> {
    value
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| StoreError::InvalidValue {
            field: "lease_expires_at",
            value: value.to_string(),
        })
}

fn parse_date(value: &str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::InvalidValue {
        field: "date",
        value: value.to_owned(),
    })
}
