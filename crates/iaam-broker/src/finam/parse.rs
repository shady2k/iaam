use iaam_core::event::provenance::ParserVersion;

// Re-export so `tinkoff::ChannelOperationKind` and
// `finam::ChannelOperationKind` continue to mean the same type:
// channel names remain familiar while the type behind them is shared.
pub use crate::operation_kind::ChannelOperationKind;
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime};

/// Finam Trade API response parser version.
pub const FINAM_PARSER_VERSION: &str = "finam-api/1";

/// Error while parsing a Finam response.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("Finam response is not valid JSON: {0}")]
    Json(String),
    #[error("Finam response is missing field {field}")]
    MissingField { field: &'static str },
    #[error("Finam response field {field} contains invalid value {value}")]
    InvalidField { field: &'static str, value: String },
    #[error("field {field} is not an RFC 3339 timestamp")]
    InvalidTimestamp { field: &'static str },
    #[error("field {field} is not a UUID: {value}")]
    InvalidIdentifier { field: &'static str, value: String },
    #[error("unsupported Finam currency: {value}")]
    UnsupportedCurrency { value: String },
    #[error("field {field} cannot be represented in currency minor units {currency:?}")]
    NonRepresentableFraction {
        field: &'static str,
        currency: CurrencyCode,
    },
    #[error("exact number overflow in field {field}")]
    NumericOverflow { field: &'static str },
    #[error("Finam paginated response is truncated: next-page token is missing")]
    PartialResponse,
}

/// An operation's monetary value in currency minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMoney {
    pub amount: PostedMinor,
    pub currency: CurrencyCode,
}

/// A transaction received from the Finam API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelOperation {
    pub date: Option<Date>,
    pub operation_id: String,
    /// The operation kind as named by the channel. The channel dictionary
    /// decides what it becomes, and that dictionary lives in data.
    pub source_kind: String,
    pub symbol: Option<String>,
    pub quantity: Option<Quantity>,
    pub payment: Option<ChannelMoney>,
    pub price: Option<Dec>,
    pub accrued_interest: Option<Dec>,
    pub transaction_category: String,
    pub transaction_name: Option<String>,
    pub deduplication_key: String,
    pub parser_version: ParserVersion,
    pub raw: Value,
    pub rejection: Option<ParseError>,
}

impl ChannelOperation {
    #[must_use]
    pub fn quantity_as_decimal(&self) -> Option<String> {
        self.quantity.map(|quantity| quantity.0.inner().to_string())
    }
}

/// Parse a transaction page without network access.
pub fn parse_operations(body: &str) -> Result<Vec<ChannelOperation>, ParseError> {
    let response: RawTransactionsResponse = parse_json(body)?;
    let has_more = response.has_more.unwrap_or(false);
    if has_more
        && response
            .next_page_token
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(ParseError::PartialResponse);
    }
    let transactions = response.transactions.ok_or(ParseError::MissingField {
        field: "transactions",
    })?;
    Ok(transactions
        .into_iter()
        .map(
            |raw| match serde_json::from_value::<RawTransaction>(raw.clone()) {
                Ok(item) => parse_operation(item, raw),
                Err(error) => rejected_operation(raw, ParseError::Json(error.to_string())),
            },
        )
        .collect())
}

/// Parse account cash and positions into source claims.
pub fn parse_portfolio(body: &str) -> Result<Vec<ControlClaim>, ParseError> {
    let response: RawPortfolioResponse = parse_json(body)?;
    let mut claims = Vec::new();
    for cash in response.cash.unwrap_or_default() {
        let money = parse_money(&cash, "cash")?;
        claims.push(ControlClaim::CashBalance {
            currency: money.currency,
            amount: money.amount,
            at: BalancePoint::Closing,
        });
    }

    let positions = response.positions.unwrap_or_default();
    if positions.is_empty() {
        return Ok(claims);
    }
    let account_id = response
        .account_id
        .as_deref()
        .ok_or(ParseError::MissingField {
            field: "account_id",
        })?;
    let custody = parse_identifier(account_id, "account_id")?;
    for position in positions {
        let symbol = position
            .symbol
            .as_deref()
            .ok_or(ParseError::MissingField { field: "symbol" })?;
        let quantity = position
            .quantity
            .as_ref()
            .ok_or(ParseError::MissingField { field: "quantity" })
            .and_then(|value| parse_quantity(value, "quantity"))?;
        claims.push(ControlClaim::PositionQuantity {
            instrument: parse_identifier(symbol, "symbol")?,
            custody,
            quantity,
            at: BalancePoint::Closing,
        });
    }
    Ok(claims)
}

fn parse_operation(item: RawTransaction, raw: Value) -> ChannelOperation {
    let mut rejection = None;
    let operation_id = required_or_reject(item.id, "id", &mut rejection);
    let timestamp = date_or_reject(item.timestamp, "timestamp", &mut rejection);
    let category = required_or_reject(item.category, "category", &mut rejection);
    let payment = keep_or_reject(
        item.change
            .as_ref()
            .map(|value| parse_money(value, "change"))
            .transpose(),
        &mut rejection,
    );
    let quantity = keep_or_reject(
        item.change_qty
            .as_ref()
            .map(|value| parse_quantity(value, "changeQty"))
            .transpose(),
        &mut rejection,
    );
    let price = keep_or_reject(
        item.trade
            .as_ref()
            .and_then(|trade| trade.price.as_ref())
            .map(|value| parse_decimal(value, "trade.price"))
            .transpose(),
        &mut rejection,
    );
    let accrued_interest = keep_or_reject(
        item.trade
            .as_ref()
            .and_then(|trade| trade.accrued_interest.as_ref())
            .map(|value| parse_decimal(value, "trade.accrued_interest"))
            .transpose(),
        &mut rejection,
    );
    ChannelOperation {
        date: timestamp,
        operation_id: operation_id.clone(),
        // Upper-casing and trimming belong to THIS channel, not the
        // dictionary: Finam writes the kind inconsistently, and a dictionary
        // that had to know about case would become code.
        source_kind: category.trim().to_ascii_uppercase(),
        symbol: nonempty(item.symbol),
        quantity,
        payment,
        price,
        accrued_interest,
        transaction_category: item.transaction_category.unwrap_or(category),
        transaction_name: nonempty(item.transaction_name),
        deduplication_key: operation_id.clone(),
        parser_version: ParserVersion(FINAM_PARSER_VERSION.to_owned()),
        raw,
        rejection,
    }
}

fn rejected_operation(raw: Value, reason: ParseError) -> ChannelOperation {
    ChannelOperation {
        date: None,
        operation_id: String::new(),
        source_kind: String::new(),
        symbol: None,
        quantity: None,
        payment: None,
        price: None,
        accrued_interest: None,
        transaction_category: String::new(),
        transaction_name: None,
        deduplication_key: String::new(),
        parser_version: ParserVersion(FINAM_PARSER_VERSION.to_owned()),
        raw,
        rejection: Some(reason),
    }
}

fn parse_money(value: &RawMoneyValue, field: &'static str) -> Result<ChannelMoney, ParseError> {
    let units = value
        .units
        .as_deref()
        .ok_or(ParseError::MissingField { field: "units" })?
        .parse::<i128>()
        .map_err(|_| ParseError::InvalidField {
            field: "units",
            value: value.units.clone().unwrap_or_default(),
        })?;
    let currency = parse_currency(value.currency_code.as_deref().ok_or(
        ParseError::MissingField {
            field: "currency_code",
        },
    )?)?;
    if !(-999_999_999..=999_999_999).contains(&value.nanos) {
        return Err(ParseError::InvalidField {
            field: "nanos",
            value: value.nanos.to_string(),
        });
    }
    let divisor = 10_i128
        .checked_pow(9 - currency.minor_units())
        .ok_or(ParseError::NumericOverflow { field })?;
    if i128::from(value.nanos) % divisor != 0 {
        return Err(ParseError::NonRepresentableFraction { field, currency });
    }
    let scale = 10_i128
        .checked_pow(currency.minor_units())
        .ok_or(ParseError::NumericOverflow { field })?;
    let amount = units
        .checked_mul(scale)
        .and_then(|whole| whole.checked_add(i128::from(value.nanos) / divisor))
        .and_then(|amount| i64::try_from(amount).ok())
        .ok_or(ParseError::NumericOverflow { field })?;
    Ok(ChannelMoney {
        amount: PostedMinor::new(amount),
        currency,
    })
}

fn parse_quantity(value: &RawQuotation, field: &'static str) -> Result<Quantity, ParseError> {
    Ok(Quantity(parse_decimal(value, field)?))
}

fn parse_decimal(value: &RawQuotation, field: &'static str) -> Result<Dec, ParseError> {
    let text = value
        .value
        .as_deref()
        .ok_or(ParseError::MissingField { field })?;
    serde_json::from_value(Value::String(text.to_owned())).map_err(|_| ParseError::InvalidField {
        field,
        value: text.to_owned(),
    })
}

fn parse_currency(value: &str) -> Result<CurrencyCode, ParseError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "rub" | "rur" => Ok(CurrencyCode::Rub),
        "usd" => Ok(CurrencyCode::Usd),
        "eur" => Ok(CurrencyCode::Eur),
        "cny" => Ok(CurrencyCode::Cny),
        "xau" => Ok(CurrencyCode::Xau),
        _ => Err(ParseError::UnsupportedCurrency {
            value: value.to_owned(),
        }),
    }
}

fn parse_identifier<T: DeserializeOwned>(
    value: &str,
    field: &'static str,
) -> Result<T, ParseError> {
    serde_json::from_value(Value::String(value.to_owned())).map_err(|_| {
        ParseError::InvalidIdentifier {
            field,
            value: value.to_owned(),
        }
    })
}

fn date_or_reject(
    value: Option<String>,
    field: &'static str,
    rejection: &mut Option<ParseError>,
) -> Option<Date> {
    keep_or_reject(
        value
            .as_deref()
            .map(|value| {
                OffsetDateTime::parse(value, &Rfc3339)
                    .map(|timestamp| timestamp.date())
                    .map_err(|_| ParseError::InvalidTimestamp { field })
            })
            .transpose(),
        rejection,
    )
}

fn required_or_reject(
    value: Option<String>,
    field: &'static str,
    rejection: &mut Option<ParseError>,
) -> String {
    keep_or_reject(
        value
            .filter(|value| !value.is_empty())
            .ok_or(ParseError::MissingField { field })
            .map(Some),
        rejection,
    )
    .unwrap_or_default()
}

fn keep_or_reject<T>(
    result: Result<Option<T>, ParseError>,
    rejection: &mut Option<ParseError>,
) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(error) => {
            if rejection.is_none() {
                *rejection = Some(error);
            }
            None
        }
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn parse_json<T: DeserializeOwned>(body: &str) -> Result<T, ParseError> {
    serde_json::from_str(body).map_err(|error| ParseError::Json(error.to_string()))
}

#[derive(Debug, Deserialize)]
struct RawTransactionsResponse {
    #[serde(rename = "hasMore", alias = "has_more")]
    has_more: Option<bool>,
    #[serde(rename = "nextPageToken", alias = "next_page_token")]
    next_page_token: Option<String>,
    transactions: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
struct RawTransaction {
    #[serde(alias = "transactionId")]
    id: Option<String>,
    category: Option<String>,
    #[serde(alias = "date")]
    timestamp: Option<String>,
    symbol: Option<String>,
    change: Option<RawMoneyValue>,
    trade: Option<RawTrade>,
    #[serde(alias = "transactionCategory")]
    transaction_category: Option<String>,
    #[serde(alias = "transactionName")]
    transaction_name: Option<String>,
    #[serde(alias = "changeQty")]
    change_qty: Option<RawQuotation>,
}

#[derive(Debug, Deserialize)]
struct RawTrade {
    price: Option<RawQuotation>,
    #[serde(alias = "accruedInterest")]
    accrued_interest: Option<RawQuotation>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawPortfolioResponse {
    #[serde(rename = "accountId", alias = "account_id")]
    account_id: Option<String>,
    cash: Option<Vec<RawMoneyValue>>,
    positions: Option<Vec<RawPortfolioPosition>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawPortfolioPosition {
    symbol: Option<String>,
    quantity: Option<RawQuotation>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMoneyValue {
    #[serde(rename = "currencyCode", alias = "currency_code", alias = "currency")]
    currency_code: Option<String>,
    units: Option<String>,
    #[serde(default, alias = "nano")]
    nanos: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct RawQuotation {
    value: Option<String>,
}
