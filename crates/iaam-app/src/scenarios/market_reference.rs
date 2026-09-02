//! Market reference series for transport and the agent.
//!
//! The scenario owns the reading of `MarketStore` and conversion of its rows into
//! application types. The server knows neither SQLite nor the source formats.

use crate::AppServices;
use crate::error::AppError;
use iaam_core::ids::InstrumentId;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::QuotationBasis;
use iaam_market::cbr::key_rate::{Boundary, derive_intervals};
use iaam_market::moex::parse::reconcile_quotation_basis;
use iaam_market::{KeyRateObservation, ObservedAt, TradeDate};
use iaam_store::market::{FxRow, KeyRateRow, MarketWindow, PriceRow, PriceVenue, SeriesKey};
use rust_decimal::Decimal;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct MarketPricesQuery {
    pub instrument: InstrumentId,
    pub board: String,
    pub session: i64,
    pub from: Date,
    pub to: Date,
    pub knowledge_as_of: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MarketFxQuery {
    /// The currency being priced.
    pub base: CurrencyCode,
    /// The currency the rate is expressed in.
    pub quote: CurrencyCode,
    /// Inclusive start of the interval.
    pub from: Date,
    /// Inclusive end of the interval.
    pub to: Date,
    pub knowledge_as_of: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MarketKeyRateQuery {
    pub from: Date,
    pub to: Date,
    pub knowledge_as_of: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotationBasisStatus {
    Proven,
    Contradicts,
    NotProven,
}

/// A reference series together with the boundary its data is complete through.
///
/// The boundary belongs to the answer, not to a row: it is one value for the
/// whole series, and it must survive an answer that holds no rows at all —
/// otherwise "this instance holds nothing for that period" and "there is no
/// value in this interval" look alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketSeries<T> {
    pub rows: Vec<T>,
    pub complete_through: Option<Date>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketPriceView {
    pub instrument: InstrumentId,
    pub board: String,
    pub session: i64,
    pub kind: String,
    pub value: String,
    pub quotation_basis: QuotationBasis,
    pub recorded_quotation_basis: String,
    pub quotation_basis_status: QuotationBasisStatus,
    pub basis_evidence: String,
    pub currency: String,
    pub date: Date,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketFxView {
    pub from: CurrencyCode,
    pub to: CurrencyCode,
    pub nominal: u32,
    pub value: String,
    pub unit_rate: String,
    pub date: Date,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketKeyRateView {
    pub value: String,
    pub from: Date,
    pub until: Option<Date>,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
    pub boundary: String,
}

pub async fn list_market_prices(
    services: &AppServices,
    query: MarketPricesQuery,
) -> Result<MarketSeries<MarketPriceView>, AppError> {
    validate_range(query.from, query.to)?;
    let knowledge_as_of = format_timestamp(query.knowledge_as_of)?;
    let series = SeriesKey {
        source_id: "moex-iss".to_owned(),
        dataset: "prices".to_owned(),
        series_key: format!(
            "{}:{}:{}",
            query.instrument.inner(),
            query.board,
            query.session
        ),
    };
    let from = query.from.to_string();
    let to = query.to.to_string();
    let store = services.market_store.lock().await;
    let complete_through = store
        .complete_through_at_or_before(&series, &knowledge_as_of)
        .map_err(store_error)?;
    let rows = store
        .prices_between(
            &series,
            &query.instrument.inner().to_string(),
            &PriceVenue {
                board: query.board,
                session: query.session,
            },
            MarketWindow {
                from: &from,
                to: &to,
                knowledge_as_of: &knowledge_as_of,
            },
        )
        .map_err(store_error)?;
    Ok(MarketSeries {
        rows: rows.into_iter().map(price_view).collect::<Result<_, _>>()?,
        complete_through,
    })
}

pub async fn list_market_fx(
    services: &AppServices,
    query: MarketFxQuery,
) -> Result<MarketSeries<MarketFxView>, AppError> {
    validate_range(query.from, query.to)?;
    let knowledge_as_of = format_timestamp(query.knowledge_as_of)?;
    let base_code = query.base.code().to_owned();
    let quote_code = query.quote.code().to_owned();
    let series = SeriesKey {
        source_id: "cbr".to_owned(),
        dataset: "fx".to_owned(),
        series_key: format!("{base_code}:{quote_code}"),
    };
    let from = query.from.to_string();
    let to = query.to.to_string();
    let store = services.market_store.lock().await;
    let complete_through = store
        .complete_through_at_or_before(&series, &knowledge_as_of)
        .map_err(store_error)?;
    let rows = store
        .fx_between(
            &series,
            &base_code,
            &quote_code,
            MarketWindow {
                from: &from,
                to: &to,
                knowledge_as_of: &knowledge_as_of,
            },
        )
        .map_err(store_error)?;
    Ok(MarketSeries {
        rows: rows.into_iter().map(fx_view).collect::<Result<_, _>>()?,
        complete_through,
    })
}

pub async fn list_market_key_rate(
    services: &AppServices,
    query: MarketKeyRateQuery,
) -> Result<MarketSeries<MarketKeyRateView>, AppError> {
    validate_range(query.from, query.to)?;
    let knowledge_as_of = format_timestamp(query.knowledge_as_of)?;
    let series = SeriesKey {
        source_id: "cbr".to_owned(),
        dataset: "key_rate".to_owned(),
        series_key: "key_rate".to_owned(),
    };
    let to = query.to.to_string();
    let store = services.market_store.lock().await;
    let complete_through = store
        .complete_through_at_or_before(&series, &knowledge_as_of)
        .map_err(store_error)?;
    let rows = store
        .key_rates_through(&series, &to, &knowledge_as_of)
        .map_err(store_error)?;
    let observations = rows
        .iter()
        .map(key_rate_observation)
        .collect::<Result<Vec<_>, _>>()?;
    let rows = derive_intervals(&observations)
        .into_iter()
        .filter(|interval| {
            interval.from <= query.to && interval.until.is_none_or(|until| until > query.from)
        })
        .map(|interval| key_rate_view(interval, &observations))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MarketSeries {
        rows,
        complete_through,
    })
}

fn price_view(row: PriceRow) -> Result<MarketPriceView, AppError> {
    let instrument = row
        .instrument_id
        .parse::<Uuid>()
        .map(InstrumentId)
        .map_err(|error| invalid_value("instrument_id", error.to_string()))?;
    let recorded_quotation_basis = row.quotation_basis.clone();
    let recorded_basis = QuotationBasis::from_code(&recorded_quotation_basis)
        .ok_or_else(|| invalid_value("quotation_basis", recorded_quotation_basis.clone()))?;
    let (quotation_basis, contradicts) =
        reconcile_quotation_basis(recorded_basis, &row.basis_evidence);
    let quotation_basis_status = if contradicts {
        QuotationBasisStatus::Contradicts
    } else if quotation_basis == QuotationBasis::Unknown {
        QuotationBasisStatus::NotProven
    } else {
        QuotationBasisStatus::Proven
    };
    Ok(MarketPriceView {
        instrument,
        board: row.board,
        session: row.session,
        kind: row.kind,
        value: row.price,
        quotation_basis,
        recorded_quotation_basis,
        quotation_basis_status,
        basis_evidence: row.basis_evidence,
        currency: row.currency,
        date: parse_date(&row.trade_date)?,
        source: "moex-iss".to_owned(),
        observed_at: canonical_timestamp(&row.observed_at)?,
        quality: row.executability,
    })
}

fn fx_view(row: FxRow) -> Result<MarketFxView, AppError> {
    let from = CurrencyCode::from_code(&row.from_code)
        .ok_or_else(|| invalid_value("from", row.from_code.clone()))?;
    let to = CurrencyCode::from_code(&row.to_code)
        .ok_or_else(|| invalid_value("to", row.to_code.clone()))?;
    Ok(MarketFxView {
        from,
        to,
        nominal: row.nominal,
        value: row.value,
        unit_rate: row.unit_rate,
        date: parse_date(&row.trade_date)?,
        source: "cbr".to_owned(),
        observed_at: canonical_timestamp(&row.observed_at)?,
        quality: "official".to_owned(),
    })
}

fn key_rate_observation(row: &KeyRateRow) -> Result<KeyRateObservation, AppError> {
    let rate = row
        .rate
        .parse::<Decimal>()
        .map_err(|error| invalid_value("rate", error.to_string()))?;
    Ok(KeyRateObservation {
        trade_date: TradeDate(parse_date(&row.trade_date)?),
        observed_at: ObservedAt(parse_timestamp(&row.observed_at)?),
        rate: Dec::new(rate),
    })
}

fn key_rate_view(
    interval: iaam_market::cbr::key_rate::RateInterval,
    observations: &[KeyRateObservation],
) -> Result<MarketKeyRateView, AppError> {
    let observation = observations
        .iter()
        .filter(|observation| {
            observation.trade_date.0 == interval.from && observation.rate == interval.rate
        })
        .max_by_key(|observation| observation.observed_at)
        .ok_or_else(|| {
            invalid_value("key_rate", "interval has no source observation".to_owned())
        })?;
    let inferred = matches!(interval.boundary, Boundary::InferredAcrossNonTradingDays);
    Ok(MarketKeyRateView {
        value: interval.rate.inner().to_string(),
        from: interval.from,
        until: interval.until,
        source: "cbr".to_owned(),
        observed_at: format_timestamp(observation.observed_at.0)?,
        quality: if inferred { "inferred" } else { "observed" }.to_owned(),
        boundary: if inferred {
            "inferred_across_non_trading_days"
        } else {
            "observed"
        }
        .to_owned(),
    })
}

fn validate_range(from: Date, to: Date) -> Result<(), AppError> {
    if to < from {
        return Err(AppError::Invalid {
            field: "to".to_owned(),
            expected: "date no earlier than from".to_owned(),
            actual: to.to_string(),
        });
    }
    Ok(())
}

fn parse_date(value: &str) -> Result<Date, AppError> {
    Date::parse(
        value,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|error| invalid_value("trade_date", error.to_string()))
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, AppError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|error| invalid_value("observed_at", error.to_string()))
}

fn canonical_timestamp(value: &str) -> Result<String, AppError> {
    format_timestamp(parse_timestamp(value)?)
}

fn format_timestamp(value: OffsetDateTime) -> Result<String, AppError> {
    value
        .format(&Rfc3339)
        .map_err(|error| invalid_value("knowledge_as_of", error.to_string()))
}

fn invalid_value(field: &'static str, actual: String) -> AppError {
    AppError::Invalid {
        field: field.to_owned(),
        expected: "value in source format".to_owned(),
        actual,
    }
}

fn store_error(error: iaam_store::StoreError) -> AppError {
    AppError::Store(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_reference_views_keep_provenance_fields() {
        let view = MarketFxView {
            from: CurrencyCode::Usd,
            to: CurrencyCode::Rub,
            nominal: 1,
            value: "80".into(),
            unit_rate: "80".into(),
            date: Date::MIN,
            source: "cbr".into(),
            observed_at: "2026-08-20T00:00:00Z".into(),
            quality: "official".into(),
        };
        assert_eq!(view.source, "cbr");
        assert_eq!(view.quality, "official");
        assert!(!view.observed_at.is_empty());
    }

    #[test]
    fn migrated_row_without_basis_remains_a_view() {
        let view = price_view(PriceRow {
            instrument_id: Uuid::nil().to_string(),
            board: "TQBR".to_owned(),
            session: 3,
            trade_date: "2026-08-03".to_owned(),
            kind: "close".to_owned(),
            observed_at: "2026-08-26T09:00:00Z".to_owned(),
            price: "281.39".to_owned(),
            currency: "RUB".to_owned(),
            quotation_basis: "unknown".to_owned(),
            basis_evidence: String::new(),
            executability: "indicative_previous_close".to_owned(),
        })
        .expect("migration row is valid for the view");

        assert_eq!(view.quotation_basis, QuotationBasis::Unknown);
        assert_eq!(view.quotation_basis_status, QuotationBasisStatus::NotProven);
        assert_eq!(view.recorded_quotation_basis, "unknown");
        assert!(view.basis_evidence.is_empty());
    }
    #[test]
    fn matching_basis_has_proven_status() {
        let view = price_view(price_row(
            "percent_of_remaining_face",
            "iss:engines/stock/markets/bonds",
        ))
        .expect("price row");

        assert_eq!(view.quotation_basis, QuotationBasis::PercentOfRemainingFace);
        assert_eq!(view.quotation_basis_status, QuotationBasisStatus::Proven);
        assert_eq!(view.recorded_quotation_basis, "percent_of_remaining_face");
    }

    #[test]
    fn contradictory_basis_has_contradicts_status() {
        let view = price_view(price_row(
            "money_per_unit",
            "iss:engines/stock/markets/bonds",
        ))
        .expect("price row");

        assert_eq!(view.quotation_basis, QuotationBasis::Unknown);
        assert_eq!(
            view.quotation_basis_status,
            QuotationBasisStatus::Contradicts
        );
        assert_eq!(view.recorded_quotation_basis, "money_per_unit");
    }

    #[test]
    fn missing_evidence_has_not_proven_status() {
        let view = price_view(price_row("money_per_unit", "test:market")).expect("price row");

        assert_eq!(view.quotation_basis, QuotationBasis::Unknown);
        assert_eq!(view.quotation_basis_status, QuotationBasisStatus::NotProven);
        assert_eq!(view.recorded_quotation_basis, "money_per_unit");
    }

    fn price_row(quotation_basis: &str, basis_evidence: &str) -> PriceRow {
        PriceRow {
            instrument_id: Uuid::nil().to_string(),
            board: "TQBR".to_owned(),
            session: 3,
            trade_date: "2026-08-03".to_owned(),
            kind: "close".to_owned(),
            observed_at: "2026-08-26T09:00:00Z".to_owned(),
            price: "281.39".to_owned(),
            currency: "RUB".to_owned(),
            quotation_basis: quotation_basis.to_owned(),
            basis_evidence: basis_evidence.to_owned(),
            executability: "indicative_previous_close".to_owned(),
        }
    }
}
