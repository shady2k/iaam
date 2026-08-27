use iaam_core::event::provenance::ParserVersion;

// Реэкспорт, чтобы `tinkoff::ChannelOperationKind` и
// `finam::ChannelOperationKind` продолжали означать один и тот же тип:
// имя у каналов привычное, а тип за ним теперь общий.
pub use crate::operation_kind::ChannelOperationKind;
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime};

/// Версия разбора ответов T-Invest, независимая от XLSX-парсера.
pub const TINKOFF_PARSER_VERSION: &str = "tinkoff-api/1";

/// Ошибка разбора ответа канала T-Invest.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// Тело не соответствует JSON-схеме ответа.
    #[error("ответ T-Invest не разобран как JSON: {0}")]
    Json(String),
    /// В ответе отсутствует обязательное поле.
    #[error("в ответе T-Invest отсутствует поле {field}")]
    MissingField { field: &'static str },
    /// Поле содержит значение, которого транспорт не принимает.
    #[error("поле {field} ответа T-Invest содержит недопустимое значение {value}")]
    InvalidField { field: &'static str, value: String },
    /// Дата операции не является RFC 3339 timestamp.
    #[error("поле {field} не является timestamp RFC 3339")]
    InvalidTimestamp { field: &'static str },
    /// Внешний идентификатор нельзя связать с типизированным ID ядра.
    #[error("поле {field} не является UUID: {value}")]
    InvalidIdentifier { field: &'static str, value: String },
    /// Валюта не входит в исчерпывающий список ядра.
    #[error("неизвестная валюта T-Invest: {value}")]
    UnsupportedCurrency { value: String },
    /// Дробная часть не представима минимальной единицей валюты.
    #[error("поле {field} нельзя представить минимальной единицей валюты {currency:?}")]
    NonRepresentableFraction {
        field: &'static str,
        currency: CurrencyCode,
    },
    /// Число вышло за диапазон точного типа ядра.
    #[error("переполнение точного числа в поле {field}")]
    NumericOverflow { field: &'static str },
    /// В ответе есть следующая страница, но нет курсора для неё.
    #[error("ответ с операциями оборван: отсутствует курсор следующей страницы")]
    PartialResponse,
}

/// Деньги операции в точной минимальной единице валюты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMoney {
    /// Сумма в минимальных единицах, со знаком шлюза.
    pub amount: PostedMinor,
    /// Валюта суммы.
    pub currency: CurrencyCode,
}
impl ChannelMoney {
    /// Возвращает сумму без знака шлюза для доменных операций.
    ///
    /// `OperationKind` хранит положительную величину, а знак движения
    /// кодирует сам вариант операции. Значение `i64::MIN` не имеет
    /// представимого модуля и потому явно отказывается.
    #[must_use]
    pub fn magnitude(self) -> Option<PostedMinor> {
        self.amount.raw().checked_abs().map(PostedMinor::new)
    }
}

/// Операция, полученная из REST-канала T-Invest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelOperation {
    /// Дата поручения из timestamp шлюза.
    pub date: Option<Date>,
    /// Счёт, который назвал шлюз.
    pub broker_account_id: String,
    /// Идентификатор операции источника.
    pub operation_id: String,
    /// Идентификатор родительской операции.
    pub parent_operation_id: Option<String>,
    /// Курсор, которым шлюз обозначил строку.
    pub cursor: String,
    /// Классифицированный вид операции.
    pub kind: ChannelOperationKind,
    /// Исходное состояние поручения.
    pub state: String,
    /// UID инструмента, если операция его содержит.
    pub instrument_uid: Option<String>,
    /// FIGI, если операция его содержит.
    pub figi: Option<String>,
    /// Количество инструмента.
    pub quantity: Option<Quantity>,
    /// Денежный эффект операции.
    pub payment: Option<ChannelMoney>,
    /// Цена одной единицы.
    pub price: Option<ChannelMoney>,
    /// Комиссия операции.
    pub commission: Option<ChannelMoney>,
    /// Стабильный ключ первой ступени дедупликации.
    pub deduplication_key: String,
    /// Версия именно этого кода разбора.
    pub parser_version: ParserVersion,
    /// Исходный JSON-объект строки, сохраняемый и при отказе.
    pub raw: Value,
    /// Причина, по которой эта строка не стала принятой операцией.
    pub rejection: Option<ParseError>,
}

impl ChannelOperation {
    /// Отдаёт количество как десятичный текст для транспортных тестов и логов.
    #[must_use]
    pub fn quantity_as_decimal(&self) -> Option<String> {
        self.quantity.map(|quantity| quantity.0.inner().to_string())
    }
}

/// Разбирает полный ответ `GetOperationsByCursor` без сетевых обращений.
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

/// Разбирает денежный остаток и позиции портфеля в контрольные утверждения.
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
        parse_optional_money(item.commission.as_ref(), "commission"),
        &mut rejection,
    );
    ChannelOperation {
        date,
        broker_account_id: broker_account_id.clone(),
        operation_id: operation_id.clone(),
        parent_operation_id: nonempty(item.parent_operation_id),
        cursor,
        kind: operation_kind(&operation_type),
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
        broker_account_id: String::new(),
        operation_id: String::new(),
        parent_operation_id: None,
        cursor: String::new(),
        kind: ChannelOperationKind::Other("unparsed".to_owned()),
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

fn parse_quantity(value: &RawQuotation, field: &'static str) -> Result<Quantity, ParseError> {
    let text = decimal_text(value, field)?;
    serde_json::from_value(Value::String(text)).map_err(|_| ParseError::InvalidField {
        field,
        value: "десятичное количество".to_owned(),
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

fn parse_date(value: &str, field: &'static str) -> Result<Date, ParseError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|timestamp| timestamp.date())
        .map_err(|_| ParseError::InvalidTimestamp { field })
}

fn operation_kind(value: &str) -> ChannelOperationKind {
    match value {
        "OPERATION_TYPE_BUY"
        | "OPERATION_TYPE_BUY_CARD"
        | "OPERATION_TYPE_BUY_MARGIN"
        | "OPERATION_TYPE_DELIVERY_BUY" => ChannelOperationKind::Buy,
        "OPERATION_TYPE_SELL"
        | "OPERATION_TYPE_SELL_CARD"
        | "OPERATION_TYPE_SELL_MARGIN"
        | "OPERATION_TYPE_DELIVERY_SELL" => ChannelOperationKind::Sell,
        "OPERATION_TYPE_DIVIDEND" | "OPERATION_TYPE_DIV_EXT" => ChannelOperationKind::Dividend,
        "OPERATION_TYPE_COUPON" => ChannelOperationKind::Coupon,
        "OPERATION_TYPE_BROKER_FEE"
        | "OPERATION_TYPE_SERVICE_FEE"
        | "OPERATION_TYPE_MARGIN_FEE"
        | "OPERATION_TYPE_SUCCESS_FEE"
        | "OPERATION_TYPE_TRACK_MFEE"
        | "OPERATION_TYPE_TRACK_PFEE"
        | "OPERATION_TYPE_CASH_FEE"
        | "OPERATION_TYPE_OUT_FEE"
        | "OPERATION_TYPE_OUT_STAMP_DUTY"
        | "OPERATION_TYPE_OUTPUT_PENALTY"
        | "OPERATION_TYPE_ADVICE_FEE"
        | "OPERATION_TYPE_OVER_COM" => ChannelOperationKind::Commission,
        "OPERATION_TYPE_INPUT"
        | "OPERATION_TYPE_INPUT_SECURITIES"
        | "OPERATION_TYPE_INPUT_SWIFT"
        | "OPERATION_TYPE_INPUT_ACQUIRING"
        | "OPERATION_TYPE_INP_MULTI" => ChannelOperationKind::Deposit,
        "OPERATION_TYPE_OUTPUT"
        | "OPERATION_TYPE_OUTPUT_SECURITIES"
        | "OPERATION_TYPE_OUTPUT_SWIFT"
        | "OPERATION_TYPE_OUTPUT_ACQUIRING"
        | "OPERATION_TYPE_OUT_MULTI" => ChannelOperationKind::Withdrawal,
        "OPERATION_TYPE_TRANS_IIS_BS" | "OPERATION_TYPE_TRANS_BS_BS" => {
            ChannelOperationKind::Transfer
        }
        _ => ChannelOperationKind::Other(value.to_owned()),
    }
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
