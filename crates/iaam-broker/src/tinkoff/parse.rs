use iaam_core::event::provenance::ParserVersion;

// Re-export so `tinkoff::ChannelOperationKind` and
// `finam::ChannelOperationKind` continue to mean the same type:
// channel names remain familiar while the type behind them is shared.
pub use crate::operation_kind::ChannelOperationKind;
use iaam_core::money::{CalcMoney, CurrencyCode, PostedMinor, Quantity};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, Time, UtcOffset};

/// T-Invest response parser version, independent of the XLSX parser.
pub const TINKOFF_PARSER_VERSION: &str = "tinkoff-api/2";

/// Error while parsing a T-Invest channel response.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// The body does not match the response JSON schema.
    #[error("T-Invest response is not valid JSON: {0}")]
    Json(String),
    /// A required field is absent from the response.
    #[error("T-Invest response is missing field {field}")]
    MissingField { field: &'static str },
    /// The field contains a value rejected by transport.
    #[error("response field {field} contains invalid value {value}")]
    InvalidField { field: &'static str, value: String },
    /// The operation date is not an RFC 3339 timestamp.
    #[error("field {field} is not an RFC 3339 timestamp")]
    InvalidTimestamp { field: &'static str },
    /// An external identifier cannot be connected to a typed core ID.
    #[error("field {field} is not a UUID: {value}")]
    InvalidIdentifier { field: &'static str, value: String },
    /// The currency is absent from the core's exhaustive list.
    #[error("unknown T-Invest currency: {value}")]
    UnsupportedCurrency { value: String },
    /// The fractional part cannot be represented in currency minor units.
    #[error("field {field} cannot be represented in currency minor units {currency:?}")]
    NonRepresentableFraction {
        field: &'static str,
        currency: CurrencyCode,
    },
    /// The number exceeds the exact core type's range.
    #[error("exact number overflow in field {field}")]
    NumericOverflow { field: &'static str },
    /// The response has another page but no cursor for it.
    #[error("operations response is truncated: next-page cursor is missing")]
    PartialResponse,
}

/// Operation money in exact currency minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMoney {
    /// Amount in minor units, including the gateway's sign.
    pub amount: PostedMinor,
    /// Currency of the amount.
    pub currency: CurrencyCode,
}
impl ChannelMoney {
    /// Return the unsigned amount for domain operations.
    ///
    /// `OperationKind` stores a positive magnitude, while the operation
    /// variant itself encodes movement direction. `i64::MIN` has no
    /// representable magnitude, so it is explicitly refused.
    #[must_use]
    pub fn magnitude(self) -> Option<PostedMinor> {
        self.amount.raw().checked_abs().map(PostedMinor::new)
    }
}

/// Operation received from the T-Invest REST channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelOperation {
    /// Order date from the gateway timestamp.
    pub date: Option<Date>,
    /// Time from the first source trade, if the order carries trade details.
    pub source_time: Option<Time>,
    /// Account named by the gateway.
    pub broker_account_id: String,
    /// Source operation identifier.
    pub operation_id: String,
    /// Parent operation identifier.
    pub parent_operation_id: Option<String>,
    /// Cursor with which the gateway labelled the row.
    pub cursor: String,
    /// Operation kind named by the channel, such as `OPERATION_TYPE_COUPON`.
    /// The set is open and belongs to the broker, so this is a string rather
    /// than an enum: the channel dictionary (`OperationKindDictionary`), which
    /// lives in data, decides what it becomes.
    pub source_kind: String,
    /// Original order state.
    pub state: String,
    /// Instrument UID, if the operation contains one.
    pub instrument_uid: Option<String>,
    /// FIGI, if the operation contains one.
    pub figi: Option<String>,
    /// Instrument quantity.
    pub quantity: Option<Quantity>,
    /// Monetary effect of the operation.
    pub payment: Option<ChannelMoney>,
    /// Price of one unit.
    pub price: Option<ChannelMoney>,
    /// Operation commission.
    pub commission: Option<CalcMoney>,
    /// Stable key for the first deduplication stage.
    pub deduplication_key: String,
    /// Version of this exact parser code.
    pub parser_version: ParserVersion,
    /// Original row JSON object, retained even on refusal.
    pub raw: Value,
    /// Why this row did not become an accepted operation.
    pub rejection: Option<ParseError>,
}

impl ChannelOperation {
    /// Return quantity as decimal text for transport tests and logs.
    #[must_use]
    pub fn quantity_as_decimal(&self) -> Option<String> {
        self.quantity.map(|quantity| quantity.0.inner().to_string())
    }
}

/// Parse a complete `GetOperationsByCursor` response without network access.
pub fn parse_operations(body: &str) -> Result<Vec<ChannelOperation>, ParseError> {
    let response: RawOperationsResponse = parse_json(body)?;
    let has_next = response
        .has_next
        .ok_or(ParseError::MissingField { field: "hasNext" })?;
    let items = response
        .items
        .ok_or(ParseError::MissingField { field: "items" })?;
    if has_next && response.next_cursor.as_deref().is_none_or(str::is_empty) {
        return Err(ParseError::PartialResponse);
    }
    Ok(items
        .into_iter()
        .map(
            |raw| match serde_json::from_value::<RawOperation>(raw.clone()) {
                Ok(item) => parse_operation(item, raw),
                Err(error) => rejected_operation(raw, ParseError::Json(error.to_string())),
            },
        )
        .collect())
}

/// Parse portfolio cash and positions into control claims.
pub fn parse_portfolio(body: &str) -> Result<Vec<ControlClaim>, ParseError> {
    let response: RawPortfolioResponse = parse_json(body)?;
    let mut claims = Vec::new();
    for position in response.positions.unwrap_or_default() {
        if position.instrument_type.as_deref() == Some("currency") {
            let quantity = position
                .quantity
                .as_ref()
                .ok_or(ParseError::MissingField { field: "quantity" })?;
            let currency = position_currency(&position)?;
            let money = parse_money(
                &RawMoneyValue {
                    units: quantity.units.clone(),
                    nano: quantity.nano,
                    currency: Some(currency.code().to_owned()),
                },
                "quantity",
            )?;
            claims.push(ControlClaim::CashBalance {
                currency: money.currency,
                amount: money.amount,
                at: BalancePoint::Closing,
            });
            continue;
        }

        let quantity = position
            .quantity
            .as_ref()
            .ok_or(ParseError::MissingField { field: "quantity" })
            .and_then(|value| parse_quantity(value, "quantity"))?;
        let instrument_uid =
            position
                .instrument_uid
                .as_deref()
                .ok_or(ParseError::MissingField {
                    field: "instrumentUid",
                })?;
        let position_uid = position
            .position_uid
            .as_deref()
            .ok_or(ParseError::MissingField {
                field: "positionUid",
            })?;
        claims.push(ControlClaim::PositionQuantity {
            instrument: parse_identifier(instrument_uid, "instrumentUid")?,
            custody: parse_identifier(position_uid, "positionUid")?,
            quantity,
            at: BalancePoint::Closing,
        });
    }
    Ok(claims)
}

fn position_currency(position: &RawPortfolioPosition) -> Result<CurrencyCode, ParseError> {
    let currency = position
        .current_price
        .as_ref()
        .and_then(|price| price.currency.as_deref())
        .or_else(|| {
            position
                .average_position_price
                .as_ref()
                .and_then(|price| price.currency.as_deref())
        })
        .ok_or(ParseError::MissingField {
            field: "currentPrice.currency",
        })?;
    parse_currency(currency)
}

fn parse_operation(item: RawOperation, raw: Value) -> ChannelOperation {
    let mut rejection = None;
    let operation_id = required_or_reject(item.id, "id", &mut rejection);
    let broker_account_id =
        required_or_reject(item.broker_account_id, "brokerAccountId", &mut rejection);
    let cursor = required_or_reject(item.cursor, "cursor", &mut rejection);
    let date = date_or_reject(item.date, "date", &mut rejection);
    let source_time = source_time_or_reject(item.trades_info, &mut rejection);
    let operation_type = required_or_reject(item.operation_type, "type", &mut rejection);
    let state = required_or_reject(item.state, "state", &mut rejection);
    let quantity = keep_or_reject(
        item.quantity
            .as_deref()
            .map(|quantity| parse_integer_quantity(quantity, "quantity"))
            .transpose(),
        &mut rejection,
    );
    let payment = keep_or_reject(
        parse_optional_money(item.payment.as_ref(), "payment"),
        &mut rejection,
    );
    let price = keep_or_reject(
        parse_optional_money(item.price.as_ref(), "price"),
        &mut rejection,
    );
    let commission = keep_or_reject(
        parse_optional_calc_money(item.commission.as_ref(), "commission"),
        &mut rejection,
    );
    ChannelOperation {
        date,
        source_time,
        broker_account_id: broker_account_id.clone(),
        operation_id: operation_id.clone(),
        parent_operation_id: nonempty(item.parent_operation_id),
        cursor,
        source_kind: operation_type.clone(),
        state,
        instrument_uid: nonempty(item.instrument_uid),
        figi: nonempty(item.figi),
        quantity,
        payment,
        price,
        commission,
        deduplication_key: format!("{broker_account_id}/{operation_id}"),
        parser_version: ParserVersion(TINKOFF_PARSER_VERSION.to_owned()),
        raw,
        rejection,
    }
}

fn rejected_operation(raw: Value, reason: ParseError) -> ChannelOperation {
    ChannelOperation {
        date: None,
        source_time: None,
        broker_account_id: String::new(),
        operation_id: String::new(),
        parent_operation_id: None,
        cursor: String::new(),
        source_kind: String::new(),
        state: String::new(),
        instrument_uid: None,
        figi: None,
        quantity: None,
        payment: None,
        price: None,
        commission: None,
        deduplication_key: String::new(),
        parser_version: ParserVersion(TINKOFF_PARSER_VERSION.to_owned()),
        raw,
        rejection: Some(reason),
    }
}

fn required_or_reject(
    value: Option<String>,
    field: &'static str,
    rejection: &mut Option<ParseError>,
) -> String {
    keep_or_reject(required(value, field).map(Some), rejection).unwrap_or_default()
}

fn date_or_reject(
    value: Option<String>,
    field: &'static str,
    rejection: &mut Option<ParseError>,
) -> Option<Date> {
    keep_or_reject(
        required(value, field).and_then(|value| parse_date(&value, field).map(Some)),
        rejection,
    )
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
    let currency = parse_currency(
        value
            .currency
            .as_deref()
            .ok_or(ParseError::MissingField { field: "currency" })?,
    )?;
    if !(-999_999_999..=999_999_999).contains(&value.nano) {
        return Err(ParseError::InvalidField {
            field: "nano",
            value: value.nano.to_string(),
        });
    }
    let minor_units = currency.minor_units();
    let divisor = 10_i128
        .checked_pow(9 - minor_units)
        .ok_or(ParseError::NumericOverflow { field })?;
    if i128::from(value.nano) % divisor != 0 {
        return Err(ParseError::NonRepresentableFraction { field, currency });
    }
    let scale = 10_i128
        .checked_pow(minor_units)
        .ok_or(ParseError::NumericOverflow { field })?;
    let amount = units
        .checked_mul(scale)
        .and_then(|whole| whole.checked_add(i128::from(value.nano) / divisor))
        .and_then(|amount| i64::try_from(amount).ok())
        .ok_or(ParseError::NumericOverflow { field })?;
    Ok(ChannelMoney {
        amount: PostedMinor::new(amount),
        currency,
    })
}
fn parse_optional_money(
    value: Option<&RawMoneyValue>,
    field: &'static str,
) -> Result<Option<ChannelMoney>, ParseError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.currency.as_deref() == Some("")
        && value.units.as_deref() == Some("0")
        && value.nano == 0
    {
        return Ok(None);
    }
    parse_money(value, field).map(Some)
}

fn parse_optional_calc_money(
    value: Option<&RawMoneyValue>,
    field: &'static str,
) -> Result<Option<CalcMoney>, ParseError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.currency.as_deref() == Some("")
        && value.units.as_deref() == Some("0")
        && value.nano == 0
    {
        return Ok(None);
    }
    parse_calc_money(value, field).map(Some)
}

fn parse_calc_money(value: &RawMoneyValue, field: &'static str) -> Result<CalcMoney, ParseError> {
    let currency = parse_currency(
        value
            .currency
            .as_deref()
            .ok_or(ParseError::MissingField { field: "currency" })?,
    )?;
    let text = decimal_text(
        &RawQuotation {
            units: value.units.clone(),
            nano: value.nano,
        },
        field,
    )?;
    let decimal = serde_json::from_value(Value::String(text))
        .map_err(|_| ParseError::NumericOverflow { field })?;
    Ok(CalcMoney::new(decimal, currency))
}

fn parse_quantity(value: &RawQuotation, field: &'static str) -> Result<Quantity, ParseError> {
    let text = decimal_text(value, field)?;
    serde_json::from_value(Value::String(text)).map_err(|_| ParseError::InvalidField {
        field,
        value: "decimal quantity".to_owned(),
    })
}
fn parse_integer_quantity(value: &str, field: &'static str) -> Result<Quantity, ParseError> {
    parse_quantity(
        &RawQuotation {
            units: Some(value.to_owned()),
            nano: 0,
        },
        field,
    )
}

fn decimal_text(value: &RawQuotation, field: &'static str) -> Result<String, ParseError> {
    let units = value
        .units
        .as_deref()
        .ok_or(ParseError::MissingField { field })?
        .parse::<i128>()
        .map_err(|_| ParseError::InvalidField {
            field,
            value: value.units.clone().unwrap_or_default(),
        })?;
    if !(-999_999_999..=999_999_999).contains(&value.nano) {
        return Err(ParseError::InvalidField {
            field,
            value: value.nano.to_string(),
        });
    }
    let scaled = units
        .checked_mul(1_000_000_000)
        .and_then(|whole| whole.checked_add(i128::from(value.nano)))
        .ok_or(ParseError::NumericOverflow { field })?;
    let negative = scaled < 0;
    let absolute = scaled
        .checked_abs()
        .ok_or(ParseError::NumericOverflow { field })?;
    let whole = absolute / 1_000_000_000;
    let fraction = absolute % 1_000_000_000;
    if fraction == 0 {
        return Ok(if negative {
            format!("-{whole}")
        } else {
            whole.to_string()
        });
    }
    let mut fraction_text = format!("{fraction:09}");
    while fraction_text.ends_with('0') {
        fraction_text.pop();
    }
    let sign = if negative { "-" } else { "" };
    Ok(format!("{sign}{whole}.{fraction_text}"))
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

fn parse_currency(value: &str) -> Result<CurrencyCode, ParseError> {
    match value.to_ascii_lowercase().as_str() {
        "rub" => Ok(CurrencyCode::Rub),
        "usd" => Ok(CurrencyCode::Usd),
        "eur" => Ok(CurrencyCode::Eur),
        "cny" => Ok(CurrencyCode::Cny),
        "xau" => Ok(CurrencyCode::Xau),
        _ => Err(ParseError::UnsupportedCurrency {
            value: value.to_owned(),
        }),
    }
}

fn parse_timestamp(value: &str, field: &'static str) -> Result<OffsetDateTime, ParseError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ParseError::InvalidTimestamp { field })
}

fn parse_date(value: &str, field: &'static str) -> Result<Date, ParseError> {
    parse_timestamp(value, field).map(|timestamp| timestamp.date())
}

fn source_time_or_reject(
    trades_info: Option<RawTradesInfo>,
    rejection: &mut Option<ParseError>,
) -> Option<Time> {
    let trade = trades_info
        .and_then(|info| info.trades)
        .and_then(|trades| trades.into_iter().next())?;
    keep_or_reject(
        required(trade.date, "tradesInfo.trades[0].date").and_then(|value| {
            parse_timestamp(&value, "tradesInfo.trades[0].date")
                .map(|timestamp| Some(timestamp.to_offset(UtcOffset::UTC).time()))
        }),
        rejection,
    )
}

fn required(value: Option<String>, field: &'static str) -> Result<String, ParseError> {
    value
        .filter(|value| !value.is_empty())
        .ok_or(ParseError::MissingField { field })
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn parse_json<T: DeserializeOwned>(body: &str) -> Result<T, ParseError> {
    serde_json::from_str(body).map_err(|error| ParseError::Json(error.to_string()))
}

#[derive(Debug, Deserialize)]
struct RawOperationsResponse {
    #[serde(rename = "hasNext")]
    has_next: Option<bool>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
    items: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
struct RawOperation {
    cursor: Option<String>,
    #[serde(rename = "brokerAccountId")]
    broker_account_id: Option<String>,
    id: Option<String>,
    #[serde(rename = "parentOperationId")]
    parent_operation_id: Option<String>,
    date: Option<String>,
    #[serde(rename = "type")]
    operation_type: Option<String>,
    state: Option<String>,
    #[serde(rename = "instrumentUid")]
    instrument_uid: Option<String>,
    figi: Option<String>,
    payment: Option<RawMoneyValue>,
    price: Option<RawMoneyValue>,
    commission: Option<RawMoneyValue>,
    quantity: Option<String>,
    #[serde(rename = "tradesInfo")]
    trades_info: Option<RawTradesInfo>,
}

#[derive(Debug, Deserialize)]
struct RawTradesInfo {
    trades: Option<Vec<RawTrade>>,
}

#[derive(Debug, Deserialize)]
struct RawTrade {
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPortfolioResponse {
    positions: Option<Vec<RawPortfolioPosition>>,
}

#[derive(Debug, Deserialize)]
struct RawPortfolioPosition {
    quantity: Option<RawQuotation>,
    #[serde(rename = "positionUid")]
    position_uid: Option<String>,
    #[serde(rename = "instrumentUid")]
    instrument_uid: Option<String>,
    #[serde(rename = "instrumentType")]
    instrument_type: Option<String>,
    #[serde(rename = "currentPrice")]
    current_price: Option<RawMoneyValue>,
    #[serde(rename = "averagePositionPrice")]
    average_position_price: Option<RawMoneyValue>,
}

#[derive(Debug, Deserialize)]
struct RawMoneyValue {
    units: Option<String>,
    #[serde(default)]
    nano: i64,
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawQuotation {
    units: Option<String>,
    #[serde(default)]
    nano: i64,
}
