use iaam_core::event::provenance::ParserVersion;

// Re-export so `tinkoff::ChannelOperationKind` and
// `finam::ChannelOperationKind` continue to mean the same type:
// channel names remain familiar while the type behind them is shared.
pub use crate::operation_kind::ChannelOperationKind;
use iaam_core::ids::InstrumentId;
use iaam_core::money::{CalcMoney, CurrencyCode, PostedMinor, Quantity};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, Time, UtcOffset};

/// T-Invest response parser version, independent of the XLSX parser.
pub const TINKOFF_PARSER_VERSION: &str = "tinkoff-api/13";

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
    /// Exact broker-reported execution commission, when available.
    pub broker_commission: Option<CalcMoney>,
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
    /// Exact price of one unit, without rounding to currency minor units.
    pub price: Option<CalcMoney>,
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

/// Currency-specific broker reference data, without inferring identity from a ticker.
#[derive(Debug, Clone)]
pub struct CurrencyDescription {
    pub nominal: CalcMoney,
    pub settlement: CurrencyCode,
    pub lot: i64,
    pub blocked_tca_flag: bool,
}

/// Parse `CurrencyBy`; nominal carries the foreign currency and exact unit size.
pub fn parse_currency_description(body: &str) -> Result<CurrencyDescription, ParseError> {
    let response: Value =
        serde_json::from_str(body).map_err(|error| ParseError::Json(error.to_string()))?;
    let instrument = response.get("instrument").ok_or(ParseError::MissingField {
        field: "instrument",
    })?;
    let iso = instrument
        .get("isoCurrencyName")
        .and_then(Value::as_str)
        .ok_or(ParseError::MissingField {
            field: "isoCurrencyName",
        })?;
    let currency = parse_currency(iso)?;
    let nominal: RawMoneyValue = serde_json::from_value(
        instrument
            .get("nominal")
            .cloned()
            .ok_or(ParseError::MissingField { field: "nominal" })?,
    )
    .map_err(|error| ParseError::Json(error.to_string()))?;
    let nominal = parse_calc_money(&nominal, "nominal")?;
    if nominal.currency() != currency {
        return Err(ParseError::InvalidField {
            field: "nominal.currency",
            value: iso.to_owned(),
        });
    }
    let settlement = parse_currency(
        instrument
            .get("currency")
            .and_then(Value::as_str)
            .ok_or(ParseError::MissingField { field: "currency" })?,
    )?;
    let lot = instrument
        .get("lot")
        .and_then(Value::as_i64)
        .filter(|lot| *lot > 0)
        .ok_or(ParseError::MissingField { field: "lot" })?;
    Ok(CurrencyDescription {
        nominal,
        settlement,
        lot,
        blocked_tca_flag: blocked_tca_flag(instrument)?,
    })
}

/// Parse the reference data returned by `InstrumentsService/GetInstrumentBy`.
pub fn parse_instrument(body: &str) -> Result<crate::BrokerInstrumentDescription, ParseError> {
    use iaam_core::instrument::InstrumentKind;
    let response: Value = parse_json(body)?;
    let instrument = response.get("instrument").ok_or(ParseError::MissingField {
        field: "instrument",
    })?;
    let required = |field: &'static str| -> Result<String, ParseError> {
        instrument
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or(ParseError::MissingField { field })
    };
    let optional = |field: &str| {
        instrument
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
    };
    let kind = match optional("instrumentType").as_deref() {
        Some("share") => Some(InstrumentKind::Share),
        Some("bond") => Some(InstrumentKind::Bond),
        Some("etf") => Some(InstrumentKind::Etf),
        Some("currency") => Some(InstrumentKind::Currency),
        _ => None,
    };
    let lot = instrument
        .get("lot")
        .map(|value| {
            value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| ParseError::InvalidField {
                    field: "lot",
                    value: value.to_string(),
                })
        })
        .transpose()?;
    Ok(crate::BrokerInstrumentDescription {
        isin: optional("isin"),
        ticker: required("ticker")?,
        name: required("name")?,
        kind,
        currency: parse_currency(&required("currency")?)?,
        mic: None,
        board: optional("classCode"),
        lot,
        price_decimals: None,
        blocked_tca_flag: blocked_tca_flag(instrument)?,
    })
}

fn blocked_tca_flag(instrument: &Value) -> Result<bool, ParseError> {
    instrument.get("blockedTcaFlag").map_or(Ok(false), |value| {
        value.as_bool().ok_or_else(|| ParseError::InvalidField {
            field: "blockedTcaFlag",
            value: value.to_string(),
        })
    })
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

/// Rows and completeness metadata from a generated broker report page.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerReportPage {
    pub broker_report: Vec<Value>,
    pub items_count: usize,
    pub pages_count: i32,
    pub page: i32,
}

/// Read the task identifier returned when report generation starts.
pub fn parse_broker_report_task(body: &str) -> Result<String, ParseError> {
    let response: Value = parse_json(body)?;
    response
        .get("generateBrokerReportResponse")
        .and_then(|v| v.get("taskId"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or(ParseError::MissingField { field: "taskId" })
}

/// Parse a generated report page without interpreting trade cash.
pub fn parse_broker_report_page(body: &str) -> Result<BrokerReportPage, ParseError> {
    let response: Value = parse_json(body)?;
    serde_json::from_value(response.get("getBrokerReportResponse").cloned().ok_or(
        ParseError::MissingField {
            field: "getBrokerReportResponse",
        },
    )?)
    .map_err(|error| ParseError::Json(error.to_string()))
}

/// Only a trade payment rejected for subminor precision may use report cash.
pub fn needs_report_payment(operation: &ChannelOperation) -> bool {
    matches!(
        operation.rejection,
        Some(ParseError::NonRepresentableFraction {
            field: "payment",
            ..
        })
    ) && matches!(
        operation.source_kind.as_str(),
        "OPERATION_TYPE_BUY"
            | "OPERATION_TYPE_BUY_CARD"
            | "OPERATION_TYPE_BUY_MARGIN"
            | "OPERATION_TYPE_SELL"
            | "OPERATION_TYPE_SELL_CARD"
            | "OPERATION_TYPE_SELL_MARGIN"
    )
}

/// Replace unposted operation cash only with exact, settled report trade cash.
/// Reparse the complete operation so a second parse defect cannot be cleared.
pub fn resolve_report_payment(
    operation: &ChannelOperation,
    rows: &[Value],
) -> Result<ChannelOperation, String> {
    use iaam_core::money::Money;
    if !needs_report_payment(operation) || operation.trades.is_empty() {
        return Err("no exact source trade IDs for report payment resolution".to_owned());
    }
    let buy = operation.source_kind.contains("BUY");
    let mut amounts = Vec::new();
    let mut evidence = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for trade in &operation.trades {
        if !ids.insert(&trade.num) {
            return Err("duplicate source trade ID".to_owned());
        }
        let matches = rows
            .iter()
            .filter(|row| row.get("tradeId").and_then(Value::as_str) == Some(trade.num.as_str()))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(format!(
                "report trade {} has {} matches",
                trade.num,
                matches.len()
            ));
        }
        let row = matches[0];
        let figi = operation
            .figi
            .as_deref()
            .filter(|figi| !figi.is_empty())
            .ok_or_else(|| "operation has no FIGI".to_owned())?;
        if row.get("figi").and_then(Value::as_str) != Some(figi) {
            return Err(format!(
                "report trade {} instrument does not match operation",
                trade.num
            ));
        }
        let quantity = row
            .get("quantity")
            .and_then(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_i64().map(|n| n.to_string()))
            })
            .ok_or_else(|| format!("report trade {} has no quantity", trade.num))?;
        if parse_integer_quantity(&quantity, "quantity").map_err(|error| error.to_string())?
            != trade.quantity
        {
            return Err(format!("report trade {} quantity mismatch", trade.num));
        }
        let trade_date = row
            .get("tradeDatetime")
            .and_then(Value::as_str)
            .and_then(|date| OffsetDateTime::parse(date, &Rfc3339).ok());
        if trade_date.is_none_or(|date| date.date() != trade.at.date())
            || operation.date != Some(trade.at.date())
        {
            return Err(format!("report trade {} date mismatch", trade.num));
        }
        let settled = row
            .get("clearValueDate")
            .and_then(Value::as_str)
            .and_then(|date| OffsetDateTime::parse(date, &Rfc3339).ok());
        if settled.is_none_or(|date| date > OffsetDateTime::now_utc()) {
            return Err(format!(
                "report trade {} has no completed settlement date",
                trade.num
            ));
        }
        let raw_money = serde_json::from_value::<RawMoneyValue>(
            row.get("totalOrderAmount")
                .cloned()
                .ok_or_else(|| format!("report trade {} has no totalOrderAmount", trade.num))?,
        )
        .map_err(|error| error.to_string())?;
        let cash =
            parse_money(&raw_money, "totalOrderAmount").map_err(|error| error.to_string())?;
        let original_currency = operation
            .raw
            .get("payment")
            .and_then(|value| value.get("currency"))
            .and_then(Value::as_str);
        if original_currency.and_then(|currency| parse_currency(currency).ok())
            != Some(cash.currency)
            || cash.currency != trade.price.currency()
            || cash.amount.raw() <= 0
        {
            return Err(format!(
                "report trade {} has invalid settled cash",
                trade.num
            ));
        }
        amounts.push(Money::new(cash.amount, cash.currency));
        evidence.push(row.clone());
    }
    let currency = operation.trades[0].price.currency();
    let total = Money::sum(&amounts, currency).map_err(|error| error.to_string())?;
    let total = if buy {
        total.checked_negate().map_err(|error| error.to_string())?
    } else {
        total
    };
    let mut resolved_raw = operation.raw.clone();
    // Decimal is exact here: no calculated value is rounded into source cash.
    let decimal = total.to_calc_dec().inner();
    let units = decimal.trunc().to_string();
    fn same_type<T: std::str::FromStr>(_: T, text: &str) -> Result<T, String> {
        text.parse().map_err(|_| "invalid decimal scale".to_owned())
    }
    let scale = same_type(decimal, "1000000000")?;
    let nano = ((decimal - decimal.trunc()) * scale)
        .normalize()
        .to_string()
        .parse::<i32>()
        .map_err(|error| error.to_string())?;
    resolved_raw["payment"] =
        serde_json::json!({"currency": currency.code(), "units": units, "nano": nano});
    let parsed: RawOperation =
        serde_json::from_value(resolved_raw.clone()).map_err(|error| error.to_string())?;
    let mut resolved = parse_operation(parsed, resolved_raw);
    if let Some(error) = &resolved.rejection {
        return Err(error.to_string());
    }
    tracing::info!(
        operation_id = operation.operation_id,
        source = "broker_report",
        "trade payment sourced from broker report"
    );
    for row in &evidence {
        let payment = operation.raw.get("payment");
        let report = row.get("totalOrderAmount");
        tracing::debug!(operation_id = operation.operation_id, trade_id = ?row.get("tradeId").and_then(serde_json::Value::as_str),
            operations_currency = ?payment.and_then(|v| v.get("currency")).and_then(serde_json::Value::as_str),
            operations_units = ?payment.and_then(|v| v.get("units")).and_then(serde_json::Value::as_str),
            operations_nano = ?payment.and_then(|v| v.get("nano")).and_then(serde_json::Value::as_i64),
            report_currency = ?report.and_then(|v| v.get("currency")).and_then(serde_json::Value::as_str),
            report_units = ?report.and_then(|v| v.get("units")).and_then(serde_json::Value::as_str),
            report_nano = ?report.and_then(|v| v.get("nano")).and_then(serde_json::Value::as_i64), "broker cash evidence");
    }
    resolved.raw = operation.raw.clone();
    resolved.raw["brokerReportSettlement"] = Value::Array(evidence);
    Ok(resolved)
}

/// Attach exact per-execution broker commissions from unambiguous report rows.
/// Missing rows or absent commissions leave allocation to the order adapter.
/// The operation-level commission remains the independent order total.
pub fn resolve_report_commissions(
    operation: &ChannelOperation,
    rows: &[Value],
) -> Result<ChannelOperation, ParseError> {
    let mut resolved = operation.clone();
    let mut ids = std::collections::HashSet::new();
    let mut evidence = Vec::new();
    for trade in &mut resolved.trades {
        if trade.num.is_empty() || !ids.insert(&trade.num) {
            return Err(ParseError::InvalidField {
                field: "trade.num",
                value: trade.num.clone(),
            });
        }
        trade.broker_commission = None;
        let mut matches = rows
            .iter()
            .filter(|row| row.get("tradeId").and_then(Value::as_str) == Some(trade.num.as_str()));
        let Some(row) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            return Err(ParseError::InvalidField {
                field: "tradeId",
                value: trade.num.clone(),
            });
        }
        if let Some(figi) = row.get("figi") {
            let figi = figi.as_str().filter(|figi| !figi.is_empty());
            if figi.is_none() || figi != operation.figi.as_deref() {
                return Err(ParseError::InvalidField {
                    field: "figi",
                    value: row["figi"].to_string(),
                });
            }
        }
        let Some(raw) = row.get("brokerCommission") else {
            continue;
        };
        let raw: RawMoneyValue = serde_json::from_value(raw.clone())
            .map_err(|error| ParseError::Json(error.to_string()))?;
        // Report commissions are posted source cash, not calculated allocations.
        parse_money(&raw, "brokerCommission")?;
        let commission = parse_calc_money(&raw, "brokerCommission")?;
        if commission.value().inner().is_sign_negative()
            || commission.currency() != trade.price.currency()
            || operation
                .commission
                .is_some_and(|total| total.currency() != commission.currency())
        {
            return Err(ParseError::InvalidField {
                field: "brokerCommission",
                value: row["brokerCommission"].to_string(),
            });
        }
        trade.broker_commission = Some(commission);
        let mut selected = serde_json::json!({
            "tradeId": trade.num,
            "brokerCommission": row["brokerCommission"],
        });
        if let Some(figi) = row.get("figi") {
            selected["figi"] = figi.clone();
        }
        evidence.push(selected);
    }
    if !evidence.is_empty() {
        resolved.raw["brokerReportCommissions"] = Value::Array(evidence);
    }
    Ok(resolved)
}

/// A portfolio answer: the claims read from it, and the position rows that
/// could not become claims.
///
/// `refused` carries each unfit row with its reason and its original JSON,
/// exactly as [`parse_operations`] carries a rejected operation: the
/// readable rows still become claims, and the unfit rows reach the owner
/// instead of stopping the sync (iaam-vg8te.1.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPortfolio {
    pub claims: Vec<ControlClaim>,
    pub refused: Vec<RefusedPosition>,
}

/// One portfolio row the channel reported that could not become a claim.
#[derive(Debug, Clone, PartialEq)]
pub struct RefusedPosition {
    /// The row as the channel printed it, kept beside the reason so the
    /// owner can read what was set aside.
    pub raw: Value,
    pub reason: ParseError,
    /// The instrument type the row named (`currency` for a cash position),
    /// when the row was structured enough to say. The channel adapter uses
    /// it to pick the reconciliation dimension the refusal taints: a
    /// `currency` row is cash, a row with any other named type is a
    /// position, and a row that never got typed taints the conservative
    /// pair (`iaam-vg8te.1.2`).
    pub instrument_type: Option<String>,
}

/// The portfolio positions with everything the response stated about them,
/// and the rows that could not be read at all.
///
/// Both portfolio readers share one acceptance rule — a row is read whole or
/// refused whole — so the duplicate guard that compares this page's rows
/// against `parse_portfolio`'s claims compares like with like.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPortfolioPositions {
    pub positions: Vec<ChannelPortfolioPosition>,
    pub refused: Vec<RefusedPosition>,
}

/// Parse portfolio cash and positions into control claims.
///
/// A control claim can only say what `ControlClaim::PositionQuantity`
/// (`iaam_core::reconciliation::claim`) has a field for, which today is a
/// single quantity and nothing else — no encumbrance, no lot count. This
/// function therefore stays a lossy projection on purpose: whatever the
/// response said beyond that single number is not dropped, it is carried by
/// [`ChannelPortfolioPosition`] and reachable through
/// [`parse_portfolio_positions`], the boundary that keeps it available to a
/// caller that can eventually express it.
///
/// A position row that cannot be read whole is set aside with its reason and
/// its original JSON instead of refusing the answer: one instrument's
/// unreadable row withholds an opinion about that instrument, not about the
/// rest of the holdings (`iaam-vg8te.1.2`). A currency row that cannot be
/// read is refused the same way.
pub fn parse_portfolio(body: &str) -> Result<ParsedPortfolio, ParseError> {
    let (rows, mut refused) = read_position_rows(body)?;
    let mut claims = Vec::new();
    for row in rows {
        let claim = if row.position.instrument_type.as_deref() == Some("currency") {
            currency_claim(&row.position)
        } else {
            // The shared row helper keeps everything the response stated
            // about the position; the claim can still carry only the
            // quantity. Custody is gone from the claim because a position is
            // an account and an instrument (iaam-xep0), and this channel's
            // handle was never a place anyway.
            position_claim(&row.position)
        };
        match claim {
            Ok(claim) => claims.push(claim),
            Err(reason) => refused.push(RefusedPosition {
                raw: row.raw,
                reason,
                instrument_type: row.position.instrument_type.clone(),
            }),
        }
    }
    Ok(ParsedPortfolio { claims, refused })
}

/// One read position row: the row as printed, beside the typed shape both
/// portfolio readers project it into.
struct ReadPositionRow {
    raw: Value,
    position: RawPortfolioPosition,
}

/// The response's position rows, each either typed or refused with its
/// reason.
///
/// One deserialisation pass shared by [`parse_portfolio`] and
/// [`parse_portfolio_positions`], so the two readers of one body cannot
/// disagree about which rows it held.
fn read_position_rows(
    body: &str,
) -> Result<(Vec<ReadPositionRow>, Vec<RefusedPosition>), ParseError> {
    let response: RawPortfolioResponse = parse_json(body)?;
    let mut rows = Vec::new();
    let mut refused = Vec::new();
    for raw in response.positions.unwrap_or_default() {
        match serde_json::from_value::<RawPortfolioPosition>(raw.clone()) {
            Ok(position) => rows.push(ReadPositionRow { raw, position }),
            Err(error) => refused.push(RefusedPosition {
                raw,
                reason: ParseError::Json(error.to_string()),
                // The row never became a typed position, so it never said
                // whether it is a currency row or a security row. Leaving
                // the type empty sends the reader to the conservative pair
                // of dimensions. (`iaam-vg8te.1.2`)
                instrument_type: None,
            }),
        }
    }
    Ok((rows, refused))
}

/// A currency position row as the cash claim it is.
fn currency_claim(position: &RawPortfolioPosition) -> Result<ControlClaim, ParseError> {
    let quantity = position
        .quantity
        .as_ref()
        .ok_or(ParseError::MissingField { field: "quantity" })?;
    let currency = position_currency(position)?;
    let money = parse_money(
        &RawMoneyValue {
            units: quantity.units.clone(),
            nano: quantity.nano,
            currency: Some(currency.code().to_owned()),
        },
        "quantity",
    )?;
    Ok(ControlClaim::CashBalance {
        currency: money.currency,
        amount: money.amount,
        at: BalancePoint::Closing,
    })
}

/// A security position row as the single claim this channel can state about
/// it: the row is read whole — quantity, both identifiers, the stated lots —
/// or refused whole, so both portfolio readers accept exactly the same rows.
fn position_claim(position: &RawPortfolioPosition) -> Result<ControlClaim, ParseError> {
    let parsed = read_position(position)?;
    let instrument: InstrumentId =
        serde_json::from_value(Value::String(parsed.instrument_uid.clone())).map_err(|_| {
            ParseError::InvalidIdentifier {
                field: "instrumentUid",
                value: parsed.instrument_uid.clone(),
            }
        })?;
    Ok(ControlClaim::PositionQuantity {
        instrument,
        quantity: parsed.quantity,
        at: BalancePoint::Closing,
    })
}

/// One non-currency portfolio position as the response stated it, read
/// whole: the quantity, both identifiers and every stated lot must parse, or
/// the row is refused rather than carried half-read.
fn read_position(position: &RawPortfolioPosition) -> Result<ChannelPortfolioPosition, ParseError> {
    let parsed = parse_portfolio_position_row(position)?;
    serde_json::from_value::<InstrumentId>(Value::String(parsed.instrument_uid.clone())).map_err(
        |_| ParseError::InvalidIdentifier {
            field: "instrumentUid",
            value: parsed.instrument_uid.clone(),
        },
    )?;
    Ok(parsed)
}

/// One non-currency portfolio position as the response stated it, before it
/// is reduced to the single number a [`ControlClaim`] can carry.
///
/// The response reports an encumbrance (`blocked`, `blockedLots`) and a lot
/// count (`quantityLots`) on a position, and `RawPortfolioPosition` used to
/// declare no field for any of the three, so they vanished at
/// deserialisation — silently, and before anything got a chance to decide
/// they did not matter. This type is where they now land instead.
///
/// `ControlClaim::PositionQuantity` has no field to put an encumbrance in,
/// and changing that claim is out of scope here: a concurrent change to it
/// is in flight elsewhere in this codebase, and colliding with it would cost
/// more than the field is worth. So this struct is deliberately not folded
/// into a claim — it is the smallest honest shape that keeps the broker's
/// statement intact until a caller exists that can do something with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPortfolioPosition {
    /// `instrumentUid` from the response.
    pub instrument_uid: String,
    /// `positionUid` from the response.
    pub position_uid: String,
    /// `instrumentType`, if the response named one.
    pub instrument_type: Option<String>,
    /// Quantity in instrument units — the same value `parse_portfolio` turns
    /// into `ControlClaim::PositionQuantity::quantity`.
    pub quantity: Quantity,
    /// `quantityLots`: quantity expressed in lots rather than instrument
    /// units. It is **not** the same unit as `quantity`, and converting one
    /// to the other needs the instrument's lot size, which this response
    /// does not carry — so the two are kept apart rather than combined into
    /// a guess.
    pub quantity_lots: Option<Quantity>,
    /// `blocked`: whether the broker reports any part of this position as
    /// blocked.
    pub blocked: Option<bool>,
    /// `blockedLots`: the blocked quantity, in lots — the broker's own unit
    /// for this field, and subject to the same caveat as `quantity_lots`.
    pub blocked_lots: Option<Quantity>,
    /// `classCode`, carried without interpretation.
    pub class_code: Option<String>,
}

/// Parse portfolio positions with everything the response stated about them,
/// including the fields `parse_portfolio` cannot carry into a
/// [`ControlClaim`] — see [`ChannelPortfolioPosition`].
///
/// Currency positions are omitted: `parse_portfolio` turns them into
/// `CashBalance` claims rather than `PositionQuantity` ones, and an
/// encumbrance on cash is a different concept this parser has not been asked
/// to represent.
pub fn parse_portfolio_positions(body: &str) -> Result<ParsedPortfolioPositions, ParseError> {
    let (rows, mut refused) = read_position_rows(body)?;
    let mut positions = Vec::new();
    for row in rows {
        if row.position.instrument_type.as_deref() == Some("currency") {
            continue;
        }
        match read_position(&row.position) {
            Ok(position) => positions.push(position),
            Err(reason) => refused.push(RefusedPosition {
                raw: row.raw,
                reason,
                instrument_type: row.position.instrument_type.clone(),
            }),
        }
    }
    Ok(ParsedPortfolioPositions { positions, refused })
}

/// The account identifiers this access sees, from `UsersService/GetAccounts`
/// (`iaam-xzz5.3.2`).
///
/// The answer's `accounts` array is the whole of what this parser reads: an
/// entry is an object carrying a non-empty string `id`. An entry that does
/// not refuses the whole answer rather than shrinking the list — a silently
/// missing identifier is exactly the ambiguity a binding must not be recorded
/// over. Nothing else about an entry interests this system, and none of it is
/// validated: the id is opaque here the way it is opaque in the binding.
pub fn parse_account_ids(body: &str) -> Result<Vec<String>, ParseError> {
    let value: Value =
        serde_json::from_str(body).map_err(|error| ParseError::Json(error.to_string()))?;
    let accounts = value
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or(ParseError::MissingField { field: "accounts" })?;
    accounts
        .iter()
        .map(|account| {
            let id = account.get("id").and_then(Value::as_str).ok_or_else(|| {
                ParseError::InvalidField {
                    field: "accounts.id",
                    value: account.to_string(),
                }
            })?;
            if id.is_empty() {
                return Err(ParseError::InvalidField {
                    field: "accounts.id",
                    value: id.to_owned(),
                });
            }
            Ok(id.to_owned())
        })
        .collect()
}

fn parse_portfolio_position_row(
    position: &RawPortfolioPosition,
) -> Result<ChannelPortfolioPosition, ParseError> {
    let quantity = position
        .quantity
        .as_ref()
        .ok_or(ParseError::MissingField { field: "quantity" })
        .and_then(|value| parse_quantity(value, "quantity"))?;
    let instrument_uid = position
        .instrument_uid
        .clone()
        .ok_or(ParseError::MissingField {
            field: "instrumentUid",
        })?;
    let position_uid = position
        .position_uid
        .clone()
        .ok_or(ParseError::MissingField {
            field: "positionUid",
        })?;
    let quantity_lots = position
        .quantity_lots
        .as_ref()
        .map(|value| parse_quantity(value, "quantityLots"))
        .transpose()?;
    let blocked_lots = position
        .blocked_lots
        .as_ref()
        .map(|value| parse_quantity(value, "blockedLots"))
        .transpose()?;
    Ok(ChannelPortfolioPosition {
        instrument_uid,
        position_uid,
        instrument_type: position.instrument_type.clone(),
        quantity,
        quantity_lots,
        blocked: position.blocked,
        blocked_lots,
        class_code: position.class_code.clone(),
    })
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
    let _operation_span = tracing::debug_span!(
        "tinkoff_operation",
        operation_id,
        source_kind = operation_type
    )
    .entered();
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
    if let Some(ParseError::NonRepresentableFraction { field, .. }) = &rejection {
        let source = raw.get(*field);
        tracing::debug!(
            operation_id,
            source_kind = operation_type,
            reason = "subminor_payment_unrepresentable",
            field,
            source_instrument_type = raw
                .get("instrumentType")
                .and_then(serde_json::Value::as_str),
            source_instrument_kind = raw
                .get("instrumentKind")
                .and_then(serde_json::Value::as_str),
            source_ticker = raw.get("ticker").and_then(serde_json::Value::as_str),
            source_figi = raw.get("figi").and_then(serde_json::Value::as_str),
            source_instrument_uid = raw.get("instrumentUid").and_then(serde_json::Value::as_str),
            source_position_uid = raw.get("positionUid").and_then(serde_json::Value::as_str),
            source_quantity = raw.get("quantity").and_then(serde_json::Value::as_str),
            source_price_units = raw
                .get("price")
                .and_then(|price| price.get("units"))
                .and_then(serde_json::Value::as_str),
            source_price_nano = raw
                .get("price")
                .and_then(|price| price.get("nano"))
                .and_then(serde_json::Value::as_i64),
            source_price_currency = raw
                .get("price")
                .and_then(|price| price.get("currency"))
                .and_then(serde_json::Value::as_str),
            source_units = source
                .and_then(|v| v.get("units"))
                .and_then(serde_json::Value::as_str),
            source_nano = source
                .and_then(|v| v.get("nano"))
                .and_then(serde_json::Value::as_i64),
            source_currency = source
                .and_then(|v| v.get("currency"))
                .and_then(serde_json::Value::as_str),
            "T-Invest row has unrepresentable source cash"
        );
    }
    let price = keep_or_reject(
        parse_optional_calc_money(item.price.as_ref(), "price"),
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
    let remainder = i128::from(value.nano) % divisor;
    if remainder != 0 {
        tracing::debug!(
            reason = "subminor_payment_unrepresentable",
            field,
            source_units = value.units.as_deref().unwrap_or_default(),
            source_nano = value.nano,
            source_currency = value.currency.as_deref().unwrap_or_default(),
            currency = currency.code(),
            minor_units,
            nano_per_minor = %divisor,
            remainder_nano = %remainder,
            "T-Invest source cash cannot be represented without rounding"
        );
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
        broker_commission: None,
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

/// Response of `GetPortfolio`.
///
/// `virtualPositions` is part of the wire schema and is deliberately not
/// mapped here. In the T-Invest API it reports the broker's projected view
/// of the portfolio once trades pending settlement clear — a forward
/// position, not the settled one `positions` reports. Every claim this
/// parser builds is a statement about the account as it stands, at a named
/// `BalancePoint` (§10.3); "what the balance will become once T+n
/// settlement lands" is a different assertion this system has no concept
/// for today. Inventing one to consume this field would be a domain
/// decision smuggled into a parser fix, so it is declined here rather than
/// guessed at.
#[derive(Debug, Deserialize)]
struct RawPortfolioResponse {
    positions: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
struct RawPortfolioPosition {
    quantity: Option<RawQuotation>,
    #[serde(rename = "quantityLots")]
    quantity_lots: Option<RawQuotation>,
    // No longer read as a custody place (iaam-xep0): a position no longer keys
    // on custody, and this channel's own handle is not a place of custody at
    // all. Kept on the wire struct because a later change carries it as
    // `source_position_id` provenance, which is what it actually is.
    #[serde(rename = "positionUid")]
    #[allow(dead_code)]
    position_uid: Option<String>,
    #[serde(rename = "instrumentUid")]
    instrument_uid: Option<String>,
    #[serde(rename = "instrumentType")]
    instrument_type: Option<String>,
    #[serde(rename = "currentPrice")]
    current_price: Option<RawMoneyValue>,
    #[serde(rename = "averagePositionPrice")]
    average_position_price: Option<RawMoneyValue>,
    /// Whether the broker reports any part of the position blocked.
    blocked: Option<bool>,
    #[serde(rename = "blockedLots")]
    blocked_lots: Option<RawQuotation>,
    #[serde(rename = "classCode")]
    class_code: Option<String>,
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
    use super::{ChannelOrderState, ParseError, parse_account_ids, parse_operations as parse_page};

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
    fn report_task_and_pages_keep_completeness_metadata() {
        assert_eq!(
            super::parse_broker_report_task(
                r#"{"generateBrokerReportResponse":{"taskId":"task"}}"#
            )
            .unwrap(),
            "task"
        );
        assert!(super::parse_broker_report_task(r#"{"generateBrokerReportResponse":{}}"#).is_err());
        for page in 0..2 {
            let body = serde_json::json!({"getBrokerReportResponse": {"brokerReport": [{"tradeId": page.to_string()}],
                "itemsCount": 2, "pagesCount": 2, "page": page}});
            let parsed = super::parse_broker_report_page(&body.to_string()).unwrap();
            assert_eq!(parsed.page, page);
            assert_eq!(parsed.items_count, 2);
            assert_eq!(parsed.broker_report.len(), 1);
        }
        assert!(super::parse_broker_report_page(r#"{"getBrokerReportResponse":{}}"#).is_err());
    }

    #[test]
    fn report_cash_requires_matching_trade_evidence() {
        let body = serde_json::json!({"hasNext": false, "items": [{
            "cursor": "c", "brokerAccountId": "a", "id": "o",
            "date": "2026-08-20T10:00:00Z", "type": "OPERATION_TYPE_BUY",
            "state": "OPERATION_STATE_EXECUTED", "figi": "FIGI1", "quantity": "92",
            "payment": {"currency": "rub", "units": "-173", "nano": -926000000},
            "tradesInfo": {"trades": [{"num":"t1", "date":"2026-08-20T10:00:00Z",
                "quantity":"92", "price":{"currency":"rub", "units":"1", "nano":890500000}}]}
        }]});
        let operation = parse_operations(&body.to_string()).unwrap().remove(0);
        assert!(super::needs_report_payment(&operation));
        let row = serde_json::json!({"tradeId":"t1", "figi":"FIGI1", "quantity":"92",
            "tradeDatetime":"2026-08-20T10:00:00Z", "clearValueDate":"2026-08-21T00:00:00Z",
            "totalOrderAmount":{"currency":"rub", "units":"173", "nano":930000000}});
        let resolved =
            super::resolve_report_payment(&operation, std::slice::from_ref(&row)).unwrap();
        assert!(resolved.rejection.is_none());
        assert_eq!(resolved.payment.unwrap().amount.raw(), -17393);
        assert!(super::resolve_report_payment(&operation, &[]).is_err());
        assert!(super::resolve_report_payment(&operation, &[row.clone(), row.clone()]).is_err());
        for (key, value) in [
            ("figi", serde_json::json!("other")),
            ("quantity", serde_json::json!("91")),
            ("tradeDatetime", serde_json::json!("2026-08-19T10:00:00Z")),
            ("clearValueDate", serde_json::Value::Null),
            (
                "totalOrderAmount",
                serde_json::json!({"currency":"usd", "units":"173"}),
            ),
        ] {
            let mut invalid = row.clone();
            invalid[key] = value;
            assert!(
                super::resolve_report_payment(&operation, &[invalid]).is_err(),
                "{key}"
            );
        }
    }

    fn commission_operation() -> super::ChannelOperation {
        let body = serde_json::json!({"hasNext": false, "items": [{
            "cursor": "c", "brokerAccountId": "a", "id": "o",
            "date": "2026-08-20T10:00:00Z", "type": "OPERATION_TYPE_BUY",
            "state": "OPERATION_STATE_EXECUTED", "figi": "FIGI1",
            "commission": {"currency":"rub", "units":"0", "nano":90000000},
            "tradesInfo": {"trades": [
                {"num":"t1", "date":"2026-08-20T10:00:00Z", "quantity":"1",
                 "price":{"currency":"rub", "units":"10"}},
                {"num":"t2", "date":"2026-08-20T10:00:01Z", "quantity":"1",
                 "price":{"currency":"rub", "units":"20"}}
            ]}
        }]});
        let operation = parse_operations(&body.to_string()).unwrap().remove(0);
        assert!(operation.rejection.is_none());
        operation
    }

    #[test]
    fn report_commissions_follow_trade_ids_not_prices_or_row_order() {
        let operation = commission_operation();
        assert!(
            operation
                .trades
                .iter()
                .all(|trade| trade.broker_commission.is_none())
        );
        let rows = [
            serde_json::json!({"tradeId":"t2", "figi":"FIGI1",
                "brokerCommission":{"currency":"rub", "units":"0", "nano":20000000}}),
            serde_json::json!({"tradeId":"unrelated",
                "brokerCommission":{"currency":"usd", "units":"-1"}}),
            serde_json::json!({"tradeId":"t1",
                "brokerCommission":{"currency":"rub", "units":"0", "nano":60000000}}),
        ];
        let resolved = super::resolve_report_commissions(&operation, &rows).unwrap();
        let amounts = resolved
            .trades
            .iter()
            .map(|trade| trade.broker_commission.unwrap().value().inner().to_string())
            .collect::<Vec<_>>();
        assert_eq!(amounts, ["0.06", "0.02"]);
        assert_eq!(resolved.commission, operation.commission);
        assert_eq!(
            resolved.raw["brokerReportCommissions"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn report_commissions_leave_missing_evidence_for_fallback_but_keep_known_zero() {
        let operation = commission_operation();
        assert_eq!(
            super::resolve_report_commissions(&operation, &[]).unwrap(),
            operation
        );
        let row = serde_json::json!({"tradeId":"t1"});
        let resolved = super::resolve_report_commissions(&operation, &[row]).unwrap();
        assert!(
            resolved
                .trades
                .iter()
                .all(|trade| trade.broker_commission.is_none())
        );
        let row = serde_json::json!({"tradeId":"t2",
            "brokerCommission":{"currency":"rub", "units":"0"}});
        let resolved = super::resolve_report_commissions(&operation, &[row]).unwrap();
        assert!(resolved.trades[0].broker_commission.is_none());
        assert!(
            resolved.trades[1]
                .broker_commission
                .unwrap()
                .value()
                .is_zero()
        );
    }

    #[test]
    fn report_commissions_refuse_ambiguous_or_invalid_evidence() {
        let operation = commission_operation();
        let row = serde_json::json!({"tradeId":"t1", "figi":"FIGI1",
            "brokerCommission":{"currency":"rub", "units":"0", "nano":30000000}});
        assert!(
            super::resolve_report_commissions(&operation, &[row.clone(), row.clone()]).is_err()
        );
        let mut duplicate = operation.clone();
        duplicate.trades[1].num = "t1".to_owned();
        assert!(super::resolve_report_commissions(&duplicate, &[]).is_err());
        for invalid in [
            serde_json::json!(null),
            serde_json::json!({"currency":"rub"}),
            serde_json::json!({"currency":"rub", "units":"bad"}),
            serde_json::json!({"currency":"rub", "units":"0", "nano":1000000000}),
            serde_json::json!({"currency":"rub", "units":"0", "nano":-1}),
            serde_json::json!({"currency":"rub", "units":"0", "nano":20000001}),
            serde_json::json!({"currency":"usd", "units":"1"}),
        ] {
            let mut invalid_row = row.clone();
            invalid_row["brokerCommission"] = invalid;
            assert!(super::resolve_report_commissions(&operation, &[invalid_row]).is_err());
        }
        let mut invalid_row = row.clone();
        invalid_row["figi"] = serde_json::json!("another-instrument");
        assert!(super::resolve_report_commissions(&operation, &[invalid_row]).is_err());
        let mut invalid_total = operation.clone();
        invalid_total.commission = Some(super::CalcMoney::new(
            iaam_core::numeric::decimal::Dec::zero(),
            super::CurrencyCode::Usd,
        ));
        assert!(super::resolve_report_commissions(&invalid_total, &[row]).is_err());
    }

    #[test]
    fn the_accounts_listing_becomes_the_ids_it_names() {
        let ids = parse_account_ids(
            r#"{"accounts":[{"id":"first","type":"ACCOUNT_TYPE_INVEST"},{"id":"second"}]}"#,
        )
        .expect("the listing parses");
        assert_eq!(ids, ["first", "second"], "only the id is read, verbatim");
    }

    #[test]
    fn an_entry_without_an_id_refuses_the_whole_listing() {
        let refused =
            parse_account_ids(r#"{"accounts":[{"id":"first"},{"type":"ACCOUNT_TYPE_INVEST"}]}"#)
                .expect_err("a silently missing id is the ambiguity a binding cannot stand on");
        assert!(
            matches!(
                refused,
                ParseError::InvalidField {
                    field: "accounts.id",
                    ..
                }
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn an_answer_without_accounts_refuses() {
        let refused = parse_account_ids(r#"{"something":[]}"#).expect_err("no accounts, no ids");
        assert!(
            matches!(refused, ParseError::MissingField { field: "accounts" }),
            "{refused:?}"
        );
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
    fn refused_portfolio_rows_carry_the_instrument_type_they_named() {
        let parsed = super::parse_portfolio(
            r#"{
                "positions": [
                    {
                        "instrumentType": "currency",
                        "quantity": {"nano": 500000000},
                        "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c1",
                        "instrumentUid": "bbbbbbbb-0000-0000-0000-0000000000c2",
                        "currentPrice": {"units": "1", "nano": 0, "currency": "rub"}
                    },
                    {
                        "instrumentType": "share",
                        "quantity": {"nano": 500000000},
                        "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c3",
                        "instrumentUid": "dddddddd-0000-0000-0000-000000000042"
                    },
                    {
                        "quantity": "not-an-object",
                        "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c4",
                        "instrumentUid": "bbbbbbbb-0000-0000-0000-0000000000c5"
                    }
                ]
            }"#,
        )
        .expect("portfolio parses");

        // The row that never deserialised is refused first, in the shared
        // row pass, carrying no type; the rows that deserialised and then
        // failed their claim follow, each naming the type it owned.
        let types = parsed
            .refused
            .iter()
            .map(|row| row.instrument_type.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            types,
            [None, Some("currency".to_owned()), Some("share".to_owned()),]
        );
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
