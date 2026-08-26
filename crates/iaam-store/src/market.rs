//! Запись рыночных наблюдений и атомарная публикация серий.
//!
//! Строки сначала принадлежат запуску со статусом `running`. Чтение видит
//! только строки запусков со статусом `succeeded`, поэтому частичный или
//! оборванный запуск не может выдать неполный ряд за опубликованный.

use iaam_core::ids::InstrumentId;
use iaam_market::observation::{
    Executability, FxObservation, KeyRateObservation, PriceKind, PriceObservation, Venue,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::{Date, OffsetDateTime, UtcOffset};

use crate::{SqliteStore, StoreError, now};
use uuid::Uuid;

/// Единица полноты: источник, набор данных и конкретная серия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesKey {
    pub source_id: String,
    pub dataset: String,
    pub series_key: String,
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
        observations: &[PriceObservation],
    ) -> Result<usize, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_run(&transaction, run, "prices")?;
        for observation in observations {
            transaction.execute(
                "INSERT INTO price_observations
                     (instrument_id, board, session, trade_date, kind, source_id,
                      observed_at, price, currency, executability, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    observation.instrument.inner().to_string(),
                    &observation.venue.board,
                    observation.venue.session,
                    format_date(observation.trade_date.0),
                    price_kind_code(observation.kind),
                    &run.series.source_id,
                    format_datetime(observation.observed_at.0)?,
                    observation.price.inner().to_string(),
                    observation.currency.code(),
                    executability_code(observation.executability),
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
        observations: &[FxObservation],
    ) -> Result<usize, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_run(&transaction, run, "fx")?;
        for observation in observations {
            transaction.execute(
                "INSERT INTO fx_observations
                     (from_code, to_code, trade_date, source_id, observed_at,
                      nominal, value, unit_rate, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    observation.from.code(),
                    observation.to.code(),
                    format_date(observation.trade_date.0),
                    &run.series.source_id,
                    format_datetime(observation.observed_at.0)?,
                    i64::from(observation.nominal),
                    observation.value.inner().to_string(),
                    observation.unit_rate.inner().to_string(),
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
        observations: &[KeyRateObservation],
    ) -> Result<usize, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_run(&transaction, run, "key_rate")?;
        for observation in observations {
            transaction.execute(
                "INSERT INTO key_rate_observations
                     (trade_date, source_id, observed_at, rate, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    format_date(observation.trade_date.0),
                    &run.series.source_id,
                    format_datetime(observation.observed_at.0)?,
                    observation.rate.inner().to_string(),
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
        let (requested_from, requested_to) = ensure_run(&transaction, run, "")?;
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

    /// Вернуть последнее по знанию опубликованное наблюдение цены.
    pub fn prices_at_or_before(
        &self,
        instrument: InstrumentId,
        venue: &Venue,
        as_of: Date,
        knowledge_as_of: OffsetDateTime,
    ) -> Result<Option<PriceObservation>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT p.trade_date, p.observed_at, p.kind, p.price,
                        p.currency, p.executability
                 FROM price_observations AS p
                 JOIN sync_runs AS r ON r.id = p.sync_run_id
                 WHERE p.instrument_id = ?1
                   AND p.board = ?2
                   AND p.session = ?3
                   AND p.trade_date <= ?4
                   AND p.observed_at <= ?5
                   AND r.status = 'succeeded'
                 ORDER BY p.observed_at DESC
                 LIMIT 1",
                params![
                    instrument.inner().to_string(),
                    &venue.board,
                    venue.session,
                    format_date(as_of),
                    format_datetime(knowledge_as_of)?,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(trade_date, observed_at, kind, price, currency, executability)| {
                Ok(PriceObservation {
                    instrument,
                    venue: venue.clone(),
                    trade_date: crate_date(&trade_date)?,
                    observed_at: crate_observed_at(&observed_at)?,
                    kind: parse_price_kind(&kind)?,
                    price: parse_dec(&price, "price")?,
                    currency: parse_currency(&currency)?,
                    executability: parse_executability(&executability)?,
                })
            },
        )
        .transpose()
    }
}

fn ensure_run(
    transaction: &Transaction<'_>,
    run: &RunHandle,
    expected_dataset: &str,
) -> Result<(Date, Date), StoreError> {
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
        || (!expected_dataset.is_empty() && dataset != expected_dataset)
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

fn parse_date(value: &str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::InvalidValue {
        field: "date",
        value: value.to_owned(),
    })
}

fn crate_date(value: &str) -> Result<iaam_market::observation::TradeDate, StoreError> {
    Ok(iaam_market::observation::TradeDate(parse_date(value)?))
}

fn crate_observed_at(value: &str) -> Result<iaam_market::observation::ObservedAt, StoreError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map(iaam_market::observation::ObservedAt)
        .map_err(|_| StoreError::InvalidValue {
            field: "observed_at",
            value: value.to_owned(),
        })
}

fn parse_dec(
    value: &str,
    field: &'static str,
) -> Result<iaam_core::numeric::decimal::Dec, StoreError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).map_err(|_| {
        StoreError::InvalidValue {
            field,
            value: value.to_owned(),
        }
    })
}

fn format_datetime(value: OffsetDateTime) -> Result<String, StoreError> {
    value
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| StoreError::InvalidValue {
            field: "observed_at",
            value: value.to_string(),
        })
}

fn price_kind_code(kind: PriceKind) -> &'static str {
    match kind {
        PriceKind::Close => "close",
        PriceKind::LegalClose => "legal_close",
        PriceKind::WeightedAverage => "weighted_average",
        PriceKind::MarketPrice2 => "market_price_2",
        PriceKind::MarketPrice3 => "market_price_3",
        PriceKind::AdmittedQuote => "admitted_quote",
    }
}

fn parse_price_kind(value: &str) -> Result<PriceKind, StoreError> {
    match value {
        "close" => Ok(PriceKind::Close),
        "legal_close" => Ok(PriceKind::LegalClose),
        "weighted_average" => Ok(PriceKind::WeightedAverage),
        "market_price_2" => Ok(PriceKind::MarketPrice2),
        "market_price_3" => Ok(PriceKind::MarketPrice3),
        "admitted_quote" => Ok(PriceKind::AdmittedQuote),
        value => Err(StoreError::InvalidValue {
            field: "kind",
            value: value.to_owned(),
        }),
    }
}

fn executability_code(value: Executability) -> &'static str {
    match value {
        Executability::Executable => "executable",
        Executability::IndicativePreviousClose => "indicative_previous_close",
        Executability::Stale => "stale",
    }
}

fn parse_executability(value: &str) -> Result<Executability, StoreError> {
    match value {
        "executable" => Ok(Executability::Executable),
        "indicative_previous_close" => Ok(Executability::IndicativePreviousClose),
        "stale" => Ok(Executability::Stale),
        value => Err(StoreError::InvalidValue {
            field: "executability",
            value: value.to_owned(),
        }),
    }
}

fn parse_currency(value: &str) -> Result<iaam_core::money::CurrencyCode, StoreError> {
    iaam_core::money::CurrencyCode::from_code(value).ok_or_else(|| StoreError::InvalidValue {
        field: "currency",
        value: value.to_owned(),
    })
}
