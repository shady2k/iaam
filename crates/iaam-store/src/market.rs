//! Запись рыночных наблюдений и атомарная публикация серий.
//!
//! Хранилище не знает форматы источников. Оно принимает собственные строковые
//! строки таблиц; преобразование наблюдений источника выполняется на границе
//! приложения.

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::{Date, OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// Единица полноты: источник, набор данных и конкретная серия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesKey {
    pub source_id: String,
    pub dataset: String,
    pub series_key: String,
}

/// Строка таблицы цен. Все значения источника остаются строками до границы
/// приложения, чтобы хранилище не зависело от крейты формата источника.
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
    /// Код основания котировки. Строкой, как и остальные значения
    /// источника: хранилище не зависит от крейта формата.
    pub quotation_basis: String,
    /// Признак, по которому основание выведено.
    pub basis_evidence: String,
    pub executability: String,
}

/// Строка наблюдения НКД. Значения строками, как и везде в хранилище.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccruedInterestRow {
    pub instrument_id: String,
    pub board: String,
    pub session: i64,
    pub trade_date: String,
    pub observed_at: String,
    /// На одну бумагу.
    pub per_unit: String,
    pub currency: String,
}

/// Площадка и сессия для выборки строки цены.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceVenue {
    pub board: String,
    pub session: i64,
}

/// Окно торгового ряда и координата знания для чтения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketWindow<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub knowledge_as_of: &'a str,
}

/// Строка таблицы курсов валют.
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

/// Строка таблицы ключевой ставки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRateRow {
    pub trade_date: String,
    pub observed_at: String,
    pub rate: String,
}

/// Запуск, удерживающий аренду одной серии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunHandle {
    pub id: String,
    pub lease_token: String,
    pub series: SeriesKey,
}

/// Фактически покрытый диапазон, сообщаемый завершённым запуском.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub from: Date,
    pub to: Date,
}

/// Итог запуска синхронизации.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Succeeded,
    Partial { reason: String },
    Failed { reason: String },
}

/// Имя, которым слой приложения обозначает API наблюдений.
///
/// Как и остальные доменные модули `iaam-store`, реализация живёт на
/// `SqliteStore`: это сохраняет одну транзакцию и одно соединение.
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
    /// Захватить аренду серии и создать незавершённый запуск.
    ///
    /// Просроченные `running`-запуски переводятся в `failed` в той же
    /// транзакции. Удалять их нельзя: на их строки наблюдений ссылается
    /// внешний ключ, а история запуска должна сохраниться.
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

    /// Записать страницу цен в незавершённый запуск.
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

    /// Записать страницу наблюдений НКД в незавершённый запуск.
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

    /// Записать страницу курсов валют в незавершённый запуск.
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

    /// Записать страницу ключевой ставки в незавершённый запуск.
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

    /// Завершить запуск и, только при успехе, продвинуть границу полноты.
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

    /// Вернуть границу полной публикации серии.
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

    /// Вернуть границу, опубликованную не позже момента знания.
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

    /// Вернуть последнее опубликованное наблюдение по торговой дате и знанию.
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

    /// Вернуть последнее опубликованное наблюдение НКД по торговой дате и знанию.
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
    /// Вернуть все опубликованные наблюдения цены инструмента в диапазоне
    /// на момент знания, не выбирая площадку заранее.
    ///
    /// Площадка и сессия не задаются: какую из них применить — решение
    /// политики оценки (E3.3), а не запроса. Источник и набор при этом
    /// остаются параметрами: зашить их значило бы сделать второй источник
    /// цен невидимым молча.
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

    /// Вернуть опубликованные наблюдения цен в диапазоне на момент знания.
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

    /// Вернуть опубликованные наблюдения курса в диапазоне на момент знания.
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

    /// Вернуть опубликованные наблюдения ключевой ставки в диапазоне.
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
