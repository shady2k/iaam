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
pub const TINKOFF_PARSER_VERSION: &str = "tinkoff-api/3";

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

/// State the channel reported for the order.
///
/// Typed rather than a string so that an unrecognised value is a named
/// variant carrying the raw text, and adding a member breaks compilation
/// wherever the state is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOrderState {
    /// `OPERATION_STATE_EXECUTED`: executed in part or in full.
    Executed,
    /// `OPERATION_STATE_CANCELED`.
    Cancelled,
    /// `OPERATION_STATE_PROGRESS`.
    InProgress,
    /// `OPERATION_STATE_UNSPECIFIED`: the channel did not name a state.
    Unspecified,
    /// A value absent from the contract, carrying the raw text.
    Unrecognised(String),
}

/// One execution of a trading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelTrade {
    pub num: String,
    pub at: OffsetDateTime,
    pub quantity: Quantity,
    /// Exact source price; it is not rounded until the gross is calculated.
    pub price: CalcMoney,
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
    pub state: ChannelOrderState,
    /// Position UID reported by the operation row.
    pub position_uid: Option<String>,
    /// Instrument UID, if the operation contains one.
    pub instrument_uid: Option<String>,
    /// FIGI, if the operation contains one.
    pub figi: Option<String>,
    /// Instrument quantity.
    pub quantity: Option<Quantity>,
    /// Executions reported for this order.
    pub trades: Vec<ChannelTrade>,
    /// Total quantity executed by the gateway, including an absent-as-zero value.
    pub quantity_done: Quantity,
    /// Quantity remaining in the order, including an absent-as-zero value.
    pub quantity_rest: Quantity,
    /// Gateway explanation for a cancelled order, if supplied.
    pub cancel_reason: Option<String>,
    /// Accrued interest reported for the whole order, if known.
    pub accrued_interest: Option<ChannelMoney>,
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

/// One parsed page of a `GetOperationsByCursor` response.
///
/// Pagination belongs to the caller because this parser has no transport
/// access and cannot decide whether another page should be requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationsPage {
    /// Operations in the response's order.
    pub operations: Vec<ChannelOperation>,
    /// Whether the gateway says another page follows.
    pub has_next: bool,
    /// Cursor to use for the next page, when one is available.
    pub next_cursor: Option<String>,
}

/// Parse one `GetOperationsByCursor` response page without network access.
pub fn parse_operations(body: &str) -> Result<OperationsPage, ParseError> {
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
    Ok(OperationsPage {
        operations: items
            .into_iter()
            .map(
                |raw| match serde_json::from_value::<RawOperation>(raw.clone()) {
                    Ok(item) => parse_operation(item, raw),
                    Err(error) => rejected_operation(raw, ParseError::Json(error.to_string())),
                },
            )
            .collect(),
        has_next,
        next_cursor: response.next_cursor,
    })
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
    let operation_type = required_or_reject(item.operation_type, "type", &mut rejection);
    let trades_info = item.trades_info;
    let trades = parse_trades(trades_info)
        .map_err(|error| {
            if rejection.is_none() {
                rejection = Some(error);
            }
        })
        .ok()
        .unwrap_or_default();
    let source_time = trades
        .first()
        .map(|trade| trade.at.to_offset(UtcOffset::UTC).time());
    let state = order_state_or_reject(item.state, &mut rejection);
    let quantity = keep_or_reject(
        item.quantity
            .as_deref()
            .map(|quantity| parse_integer_quantity(quantity, "quantity"))
            .transpose(),
        &mut rejection,
    );
    let quantity_done =
        integer_quantity_or_zero(item.quantity_done, "quantityDone", &mut rejection);
    let quantity_rest =
        integer_quantity_or_zero(item.quantity_rest, "quantityRest", &mut rejection);
    let cancel_reason = nonempty(item.cancel_reason);
    let accrued_interest = keep_or_reject(
        parse_optional_money(item.accrued_interest.as_ref(), "accruedInt"),
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
        position_uid: nonempty(item.position_uid),
        figi: nonempty(item.figi),
        quantity,
        trades,
        quantity_done,
        quantity_rest,
        cancel_reason,
        payment,
        accrued_interest,

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
        state: ChannelOrderState::Unspecified,
        position_uid: None,
        instrument_uid: None,
        figi: None,
        quantity: None,
        trades: Vec::new(),
        quantity_done: Quantity::zero(),
        quantity_rest: Quantity::zero(),
        cancel_reason: None,
        accrued_interest: None,

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

fn order_state_or_reject(
    value: Option<String>,
    rejection: &mut Option<ParseError>,
) -> ChannelOrderState {
    keep_or_reject(
        required(value, "state").map(|value| Some(parse_order_state(value))),
        rejection,
    )
    .unwrap_or(ChannelOrderState::Unspecified)
}

fn parse_order_state(raw: String) -> ChannelOrderState {
    match raw.as_str() {
        "OPERATION_STATE_EXECUTED" => ChannelOrderState::Executed,
        "OPERATION_STATE_CANCELED" => ChannelOrderState::Cancelled,
        "OPERATION_STATE_PROGRESS" => ChannelOrderState::InProgress,
        "OPERATION_STATE_UNSPECIFIED" => ChannelOrderState::Unspecified,
        _ => ChannelOrderState::Unrecognised(raw),
    }
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

fn parse_trades(trades_info: Option<RawTradesInfo>) -> Result<Vec<ChannelTrade>, ParseError> {
    let trades = trades_info.and_then(|info| info.trades).unwrap_or_default();
    // Cash operations carry a zero-valued placeholder whose empty currency
    // distinguishes it from an execution; its other fields are not required.
    trades
        .into_iter()
        .filter(|trade| {
            !matches!(
                trade
                    .price
                    .as_ref()
                    .and_then(|price| price.currency.as_deref()),
                Some("")
            )
        })
        .map(parse_trade)
        .collect()
}

fn parse_trade(trade: RawTrade) -> Result<ChannelTrade, ParseError> {
    let num = required(trade.num, "num")?;
    let at = required(trade.date, "date").and_then(|value| parse_timestamp(&value, "date"))?;
    let quantity = required(trade.quantity, "quantity")
        .and_then(|value| parse_integer_quantity(&value, "quantity"))?;
    let price = trade
        .price
        .as_ref()
        .ok_or(ParseError::MissingField { field: "price" })
        .and_then(|value| parse_calc_money(value, "price"))?;
    Ok(ChannelTrade {
        num,
        at,
        quantity,
        price,
    })
}

fn integer_quantity_or_zero(
    value: Option<String>,
    field: &'static str,
    rejection: &mut Option<ParseError>,
) -> Quantity {
    keep_or_reject(
        value
            .map(|value| parse_integer_quantity(&value, field))
            .transpose(),
        rejection,
    )
    .unwrap_or_else(Quantity::zero)
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
    #[serde(rename = "positionUid")]
    position_uid: Option<String>,
    figi: Option<String>,
    payment: Option<RawMoneyValue>,
    price: Option<RawMoneyValue>,
    commission: Option<RawMoneyValue>,
    #[serde(rename = "accruedInt")]
    accrued_interest: Option<RawMoneyValue>,
    quantity: Option<String>,
    #[serde(rename = "quantityRest")]
    quantity_rest: Option<String>,
    #[serde(rename = "quantityDone")]
    quantity_done: Option<String>,
    #[serde(rename = "cancelReason")]
    cancel_reason: Option<String>,
    #[serde(rename = "tradesInfo")]
    trades_info: Option<RawTradesInfo>,
}

#[derive(Debug, Deserialize)]
struct RawTradesInfo {
    trades: Option<Vec<RawTrade>>,
}

#[derive(Debug, Deserialize)]
struct RawTrade {
    num: Option<String>,
    date: Option<String>,
    quantity: Option<String>,
    price: Option<RawMoneyValue>,
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
#[cfg(test)]
mod tests {
    use super::{ChannelOrderState, ParseError, parse_operations as parse_page};

    fn parse_operations(body: &str) -> Result<Vec<super::ChannelOperation>, ParseError> {
        parse_page(body).map(|page| page.operations)
    }

    fn operation_json(state: Option<&str>) -> String {
        let state = state.map_or(String::new(), |state| format!(r#","state":"{state}""#));
        format!(
            r#"{{
                "hasNext": false,
                "items": [{{
                    "cursor": "cursor-1",
                    "brokerAccountId": "account-1",
                    "id": "operation-1",
                    "date": "2026-08-20T10:11:12Z",
                    "type": "OPERATION_TYPE_BUY"{state}
                }}]
            }}"#
        )
    }
    #[test]
    fn preserves_pagination_metadata_for_the_caller() {
        let page = parse_page(r#"{"hasNext":true,"nextCursor":"cursor-2","items":[]}"#)
            .expect("page parses");

        assert!(page.has_next);
        assert_eq!(page.next_cursor.as_deref(), Some("cursor-2"));
        assert!(page.operations.is_empty());
    }

    #[test]
    fn parses_each_contract_order_state() {
        let cases = [
            ("OPERATION_STATE_EXECUTED", ChannelOrderState::Executed),
            ("OPERATION_STATE_CANCELED", ChannelOrderState::Cancelled),
            ("OPERATION_STATE_PROGRESS", ChannelOrderState::InProgress),
            (
                "OPERATION_STATE_UNSPECIFIED",
                ChannelOrderState::Unspecified,
            ),
        ];

        for (wire, expected) in cases {
            let operation = parse_operations(&operation_json(Some(wire)))
                .expect("response parses")
                .pop()
                .expect("one operation");
            assert_eq!(operation.state, expected);
        }
    }

    #[test]
    fn preserves_unrecognised_order_state_verbatim() {
        let operation = parse_operations(&operation_json(Some("OPERATION_STATE_SOMETHING_NEW")))
            .expect("response parses")
            .pop()
            .expect("one operation");

        assert_eq!(
            operation.state,
            ChannelOrderState::Unrecognised("OPERATION_STATE_SOMETHING_NEW".to_owned())
        );
    }

    #[test]
    fn rejects_an_operation_without_order_state() {
        let operation = parse_operations(&operation_json(None))
            .expect("response parses")
            .pop()
            .expect("one operation");

        assert_eq!(
            operation.rejection,
            Some(ParseError::MissingField { field: "state" })
        );
        assert_eq!(operation.state, ChannelOrderState::Unspecified);
    }
    #[test]
    fn parses_each_trade_with_its_own_fields() {
        let operation = parse_operations(
            r#"{
                "hasNext": false,
                "items": [{
                    "cursor": "cursor-1",
                    "brokerAccountId": "account-1",
                    "id": "operation-1",
                    "date": "2026-08-20T10:11:12Z",
                    "type": "OPERATION_TYPE_BUY",
                    "state": "OPERATION_STATE_EXECUTED",
                    "positionUid": "f1a60ae6-3f1e-43c8-8d46-042df0fdc97a",
                    "quantityDone": "12",
                    "quantityRest": "3",
                    "cancelReason": "operator cancelled",
                    "tradesInfo": {"trades": [
                        {
                            "num": "trade-1",
                            "date": "2026-08-21T11:12:13+03:00",
                            "quantity": "10",
                            "price": {"units": "12", "nano": 345600000, "currency": "rub"}
                        },
                        {
                            "num": "trade-2",
                            "date": "2026-08-22T12:13:14Z",
                            "quantity": "2",
                            "price": {"units": "13", "nano": 450000000, "currency": "rub"}
                        }
                    ]}
                }]
            }"#,
        )
        .expect("response parses")
        .pop()
        .expect("one operation");
        assert_eq!(
            operation.position_uid.as_deref(),
            Some("f1a60ae6-3f1e-43c8-8d46-042df0fdc97a")
        );

        assert_eq!(operation.trades.len(), 2);
        assert_eq!(operation.trades[0].num, "trade-1");
        assert_eq!(operation.trades[0].quantity.0.inner().to_string(), "10");
        assert_eq!(
            operation.trades[0].price.value().inner().to_string(),
            "12.3456"
        );
        assert_eq!(
            operation.trades[0].at,
            time::macros::datetime!(2026-08-21 11:12:13 +03:00)
        );
        assert_eq!(operation.trades[1].num, "trade-2");
        assert_eq!(operation.trades[1].quantity.0.inner().to_string(), "2");
        assert_eq!(
            operation.trades[1].price.value().inner().to_string(),
            "13.45"
        );
        assert_eq!(
            operation.trades[1].at,
            time::macros::datetime!(2026-08-22 12:13:14 +00:00)
        );
        assert_eq!(operation.quantity_done.0.inner().to_string(), "12");
        assert_eq!(operation.quantity_rest.0.inner().to_string(), "3");
        assert_eq!(
            operation.cancel_reason.as_deref(),
            Some("operator cancelled")
        );
        assert_eq!(operation.source_time, Some(time::macros::time!(08:12:13)));
    }

    #[test]
    fn absent_or_empty_trades_info_means_no_trades() {
        for trades_info in ["", r#","tradesInfo": {"trades": []}"#] {
            let body = format!(
                r#"{{
                    "hasNext": false,
                    "items": [{{
                        "cursor": "cursor-1",
                        "brokerAccountId": "account-1",
                        "id": "operation-1",
                        "date": "2026-08-20T10:11:12Z",
                        "type": "OPERATION_TYPE_BUY",
                        "state": "OPERATION_STATE_CANCELED"{trades_info}
                    }}]
                }}"#
            );
            let operation = parse_operations(&body)
                .expect("response parses")
                .pop()
                .expect("one operation");
            assert!(operation.trades.is_empty());
            assert_eq!(operation.rejection, None);
        }
    }

    #[test]
    fn absent_quantity_done_is_present_as_zero() {
        let operation = parse_operations(&operation_json(Some("OPERATION_STATE_EXECUTED")))
            .expect("response parses")
            .pop()
            .expect("one operation");

        assert!(operation.quantity_done.0.is_zero());
        assert!(operation.quantity_rest.0.is_zero());
    }

    #[test]
    fn a_trade_missing_a_required_field_rejects_the_whole_row_and_keeps_raw_json() {
        let body = r#"{
            "hasNext": false,
            "items": [{
                "cursor": "cursor-1",
                "brokerAccountId": "account-1",
                "id": "operation-1",
                "date": "2026-08-20T10:11:12Z",
                "type": "OPERATION_TYPE_BUY",
                "state": "OPERATION_STATE_EXECUTED",
                "tradesInfo": {"trades": [{
                    "num": "trade-1",
                    "date": "2026-08-21T11:12:13Z",
                    "quantity": "10"
                }]}
            }]
        }"#;
        let operation = parse_operations(body)
            .expect("response parses")
            .pop()
            .expect("one operation");

        assert_eq!(
            operation.rejection,
            Some(ParseError::MissingField { field: "price" })
        );
        assert_eq!(operation.raw["tradesInfo"]["trades"][0]["quantity"], "10");
        assert!(operation.trades.is_empty());
    }
    #[test]
    fn each_missing_trade_field_rejects_the_whole_row() {
        let required_fields = ["num", "date", "quantity", "price"];
        for missing in required_fields {
            let mut trade = serde_json::json!({
                "num": "trade-1",
                "date": "2026-08-21T11:12:13Z",
                "quantity": "10",
                "price": {"units": "12", "nano": 0, "currency": "rub"}
            });
            trade.as_object_mut().expect("trade object").remove(missing);
            let body = serde_json::json!({
                "hasNext": false,
                "items": [{
                    "cursor": "cursor-1",
                    "brokerAccountId": "account-1",
                    "id": "operation-1",
                    "date": "2026-08-20T10:11:12Z",
                    "type": "OPERATION_TYPE_BUY",
                    "state": "OPERATION_STATE_EXECUTED",
                    "tradesInfo": {"trades": [trade]}
                }]
            })
            .to_string();
            let operation = parse_operations(&body)
                .expect("response parses")
                .pop()
                .expect("one operation");

            assert!(operation.rejection.is_some(), "missing {missing} accepted");
            assert_eq!(
                operation.raw["tradesInfo"]["trades"][0][missing],
                serde_json::Value::Null
            );
            assert!(operation.trades.is_empty());
        }
    }
    #[test]
    fn absent_accrued_interest_is_unknown() {
        let operation = parse_operations(&operation_json(Some("OPERATION_STATE_EXECUTED")))
            .expect("response parses")
            .pop()
            .expect("one operation");
        assert_eq!(operation.accrued_interest, None);
    }

    #[test]
    fn parses_accrued_interest_with_empty_currency_as_absent() {
        let body = r#"{
            "hasNext": false,
            "items": [{
                "cursor": "cursor-1",
                "brokerAccountId": "account-1",
                "id": "operation-1",
                "date": "2026-08-20T10:11:12Z",
                "type": "OPERATION_TYPE_BUY",
                "state": "OPERATION_STATE_EXECUTED",
                "accruedInt": {"units": "0", "nano": 0, "currency": ""}
            }]
        }"#;
        let operation = parse_operations(body)
            .expect("response parses")
            .pop()
            .expect("one operation");
        assert_eq!(operation.accrued_interest, None);
    }

    #[test]
    fn parses_accrued_interest_zero_with_a_real_currency() {
        let body = r#"{
            "hasNext": false,
            "items": [{
                "cursor": "cursor-1",
                "brokerAccountId": "account-1",
                "id": "operation-1",
                "date": "2026-08-20T10:11:12Z",
                "type": "OPERATION_TYPE_BUY",
                "state": "OPERATION_STATE_EXECUTED",
                "accruedInt": {"units": "0", "nano": 0, "currency": "rub"}
            }]
        }"#;
        let operation = parse_operations(body)
            .expect("response parses")
            .pop()
            .expect("one operation");
        let accrued = operation.accrued_interest.expect("known zero");
        assert_eq!(accrued.amount.raw(), 0);
        assert_eq!(accrued.currency, iaam_core::money::CurrencyCode::Rub);
    }
}
