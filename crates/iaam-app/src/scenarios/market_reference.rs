//! Справочные ряды рынка для транспорта и агента.
//!
//! Сценарий владеет чтением `MarketStore` и преобразованием его строк в
//! типы приложения. Сервер не знает ни SQLite, ни форматов источников.

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
    pub from: CurrencyCode,
    pub to: CurrencyCode,
    pub from_date: Date,
    pub to_date: Date,
    pub knowledge_as_of: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct MarketKeyRateQuery {
    pub from: Date,
    pub to: Date,
    pub knowledge_as_of: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketPriceView {
    pub instrument: InstrumentId,
    pub board: String,
    pub session: i64,
    pub kind: String,
    pub value: String,
    pub quotation_basis: QuotationBasis,
    pub basis_evidence: String,
    pub currency: String,
    pub date: Date,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
    pub complete_through: Option<Date>,
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
    pub complete_through: Option<Date>,
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
    pub complete_through: Option<Date>,
}

pub async fn list_market_prices(
    services: &AppServices,
    query: MarketPricesQuery,
) -> Result<Vec<MarketPriceView>, AppError> {
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
    rows.into_iter()
        .map(|row| price_view(row, complete_through))
        .collect()
}

pub async fn list_market_fx(
    services: &AppServices,
    query: MarketFxQuery,
) -> Result<Vec<MarketFxView>, AppError> {
    validate_range(query.from_date, query.to_date)?;
    let knowledge_as_of = format_timestamp(query.knowledge_as_of)?;
    let from_code = query.from.code().to_owned();
    let to_code = query.to.code().to_owned();
    let series = SeriesKey {
        source_id: "cbr".to_owned(),
        dataset: "fx".to_owned(),
        series_key: format!("{from_code}:{to_code}"),
    };
    let from = query.from_date.to_string();
    let to = query.to_date.to_string();
    let store = services.market_store.lock().await;
    let complete_through = store
        .complete_through_at_or_before(&series, &knowledge_as_of)
        .map_err(store_error)?;
    let rows = store
        .fx_between(
            &series,
            &from_code,
            &to_code,
            MarketWindow {
                from: &from,
                to: &to,
                knowledge_as_of: &knowledge_as_of,
            },
        )
        .map_err(store_error)?;
    rows.into_iter()
        .map(|row| fx_view(row, complete_through))
        .collect()
}

pub async fn list_market_key_rate(
    services: &AppServices,
    query: MarketKeyRateQuery,
) -> Result<Vec<MarketKeyRateView>, AppError> {
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
    derive_intervals(&observations)
        .into_iter()
        .filter(|interval| {
            interval.from <= query.to && interval.until.is_none_or(|until| until > query.from)
        })
        .map(|interval| key_rate_view(interval, &observations, complete_through))
        .collect()
}

fn price_view(row: PriceRow, complete_through: Option<Date>) -> Result<MarketPriceView, AppError> {
    let instrument = row
        .instrument_id
        .parse::<Uuid>()
        .map(InstrumentId)
        .map_err(|error| invalid_value("instrument_id", error.to_string()))?;
    let recorded_basis = QuotationBasis::from_code(&row.quotation_basis)
        .ok_or_else(|| invalid_value("quotation_basis", row.quotation_basis.clone()))?;
    let (quotation_basis, _) =
        reconcile_quotation_basis(recorded_basis, &row.basis_evidence);
    Ok(MarketPriceView {
        instrument,
        board: row.board,
        session: row.session,
        kind: row.kind,
        value: row.price,
        quotation_basis,
        basis_evidence: row.basis_evidence,
        currency: row.currency,
        date: parse_date(&row.trade_date)?,
        source: "moex-iss".to_owned(),
        observed_at: canonical_timestamp(&row.observed_at)?,
        quality: row.executability,
        complete_through,
    })
}

fn fx_view(row: FxRow, complete_through: Option<Date>) -> Result<MarketFxView, AppError> {
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
        complete_through,
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
    complete_through: Option<Date>,
) -> Result<MarketKeyRateView, AppError> {
    let observation = observations
        .iter()
        .filter(|observation| {
            observation.trade_date.0 == interval.from && observation.rate == interval.rate
        })
        .max_by_key(|observation| observation.observed_at)
        .ok_or_else(|| {
            invalid_value(
                "key_rate",
                "интервал не имеет исходного наблюдения".to_owned(),
            )
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
        complete_through,
    })
}

fn validate_range(from: Date, to: Date) -> Result<(), AppError> {
    if to < from {
        return Err(AppError::Invalid {
            field: "to".to_owned(),
            expected: "дата не раньше from".to_owned(),
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
        expected: "значение в формате источника".to_owned(),
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
            complete_through: None,
        };
        assert_eq!(view.source, "cbr");
        assert_eq!(view.quality, "official");
        assert!(!view.observed_at.is_empty());
    }

    #[test]
    fn мигрированная_строка_unknown_без_признака_остаётся_витриной() {
        let view = price_view(
            PriceRow {
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
            },
            None,
        )
        .expect("строка миграции допустима для витрины");

        assert_eq!(view.quotation_basis, QuotationBasis::Unknown);
        assert!(view.basis_evidence.is_empty());
    }
}
