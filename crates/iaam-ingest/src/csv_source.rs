//! Разбор CSV (§10.1).
//!
//! Строка — единица разбора: непонятая строка получает вердикт, а
//! документ продолжает разбираться. Счёт и инструмент указываются
//! человеческими именами и разрешаются справочником: идентификаторы
//! UUID в файле, который заполняет человек, — способ гарантировать
//! ошибки.
//!
//! Суммы записываются как десятичные числа. Число с большей точностью,
//! чем минимальная единица валюты, **отклоняется**, а не округляется:
//! округление входных данных — это тихое изменение факта.

use std::collections::BTreeMap;

use iaam_core::event::kind::FeeOrigin;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use time::macros::format_description;

use crate::operation::{OperationDates, OperationKind, SubmittedOperation, to_minor_units};
use crate::verdict::Rejection;

/// Справочник имён. Заполняется оболочкой из таблиц счетов и инструментов.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub accounts: BTreeMap<String, AccountId>,
    pub custodies: BTreeMap<String, CustodyId>,
    pub instruments: BTreeMap<String, InstrumentId>,
    /// Место хранения по умолчанию для счёта без указанного депозитария.
    pub default_custody: Option<CustodyId>,
}

/// Одна строка файла в сыром виде.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub date: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub account: String,
    #[serde(default)]
    pub counterparty_account: Option<String>,
    #[serde(default)]
    pub instrument: Option<String>,
    #[serde(default)]
    pub custody: Option<String>,
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub fee: Option<String>,
    #[serde(default)]
    pub accrued_interest: Option<String>,
    pub currency: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// Результат разбора одной строки.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedRow {
    Operation(Box<SubmittedOperation>),
    Rejected(Rejection),
}

/// Разбор всего документа. Возвращает по элементу на строку, включая
/// отклонённые: номер строки — это индекс в результате плюс единица.
#[must_use]
pub fn parse(content: &str, directory: &Directory) -> Vec<ParsedRow> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    let mut parsed = Vec::new();
    for record in reader.deserialize::<Row>() {
        parsed.push(match record {
            Ok(row) => match row_to_operation(&row, directory) {
                Ok(operation) => ParsedRow::Operation(Box::new(operation)),
                Err(rejection) => ParsedRow::Rejected(rejection),
            },
            Err(error) => ParsedRow::Rejected(Rejection {
                field: "row".into(),
                expected: "строка в формате заголовка файла".into(),
                actual: error.to_string(),
            }),
        });
    }
    parsed
}

fn row_to_operation(row: &Row, directory: &Directory) -> Result<SubmittedOperation, Rejection> {
    let date = parse_date(&row.date)?;
    let currency = parse_currency(&row.currency)?;
    let account = lookup(&directory.accounts, &row.account, "account")?;
    let kind = build_kind(row, directory, currency)?;

    Ok(SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(date),
            cash_posted: Some(date),
            ..OperationDates::default()
        },
        idempotency_key: row.idempotency_key.clone(),
        source_operation_id: None,
    })
}

fn build_kind(
    row: &Row,
    directory: &Directory,
    currency: CurrencyCode,
) -> Result<OperationKind, Rejection> {
    match row.kind.as_str() {
        "deposit" => Ok(OperationKind::Deposit {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "withdrawal" => Ok(OperationKind::Withdrawal {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "transfer" => Ok(OperationKind::Transfer {
            to: lookup(
                &directory.accounts,
                row.counterparty_account.as_deref().unwrap_or_default(),
                "counterparty_account",
            )?,
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "buy" | "sell" => build_trade(row, directory, currency),
        "income" => Ok(OperationKind::Income {
            instrument: match row.instrument.as_deref() {
                None | Some("") => None,
                Some(symbol) => Some(lookup(&directory.instruments, symbol, "instrument")?),
            },
            gross_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "fee" => Ok(OperationKind::Fee {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
            origin: FeeOrigin::Other,
        }),
        "valuation" => Ok(OperationKind::Valuation {
            instrument: lookup(
                &directory.instruments,
                row.instrument.as_deref().unwrap_or_default(),
                "instrument",
            )?,
            price: Dec::new(decimal(row.amount.as_deref(), "amount")?),
            currency,
            // Цена, названная владельцем, не является исполнимой (§5.4).
            quality: PriceQuality::OwnerEstimate,
        }),
        other => Err(Rejection {
            field: "type".into(),
            expected: "deposit, withdrawal, transfer, buy, sell, income, fee или valuation".into(),
            actual: other.to_owned(),
        }),
    }
}

/// Место хранения: названное разрешается справочником, неназванное
/// берётся из умолчания.
///
/// Вынесено отдельной функцией **ради проверяемости, а не читаемости**.
/// Изнутри `parse` пустую строку получить нельзя: разбор CSV отдаёт для
/// пустой ячейки `None`, а не `Some("")`. Ветка с проверкой на пустоту
/// оказалась бы недостижимой, и мутационный заслон назвал бы её
/// эквивалентной. Недостижимая проверка — это проверка, про которую
/// неизвестно, работает ли она; отдельную функцию можно вызвать
/// напрямую и на пустой строке тоже.
fn resolve_custody(name: Option<&str>, directory: &Directory) -> Result<CustodyId, Rejection> {
    match name {
        Some(name) if !name.is_empty() => lookup(&directory.custodies, name, "custody"),
        _ => directory.default_custody.ok_or_else(|| Rejection {
            field: "custody".into(),
            expected: "известное место хранения или значение по умолчанию".into(),
            actual: "не указано".into(),
        }),
    }
}

fn build_trade(
    row: &Row,
    directory: &Directory,
    currency: CurrencyCode,
) -> Result<OperationKind, Rejection> {
    let instrument = lookup(
        &directory.instruments,
        row.instrument.as_deref().unwrap_or_default(),
        "instrument",
    )?;
    let custody = resolve_custody(row.custody.as_deref(), directory)?;
    let quantity = Dec::new(decimal(row.quantity.as_deref(), "quantity")?);
    let gross_minor = minor(row.amount.as_deref(), "amount", currency)?;
    let fee_minor = optional_minor(row.fee.as_deref(), "fee", currency)?;
    let accrued_interest_minor = optional_minor(
        row.accrued_interest.as_deref(),
        "accrued_interest",
        currency,
    )?;

    if row.kind == "buy" {
        Ok(OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        })
    } else {
        Ok(OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        })
    }
}

fn lookup<T: Copy>(
    table: &BTreeMap<String, T>,
    name: &str,
    field: &'static str,
) -> Result<T, Rejection> {
    table.get(name).copied().ok_or_else(|| Rejection {
        field: field.to_owned(),
        expected: "имя из справочника".into(),
        actual: name.to_owned(),
    })
}

fn parse_date(value: &str) -> Result<Date, Rejection> {
    Date::parse(value, format_description!("[year]-[month]-[day]")).map_err(|_| Rejection {
        field: "date".into(),
        expected: "дата в формате ГГГГ-ММ-ДД".into(),
        actual: value.to_owned(),
    })
}

fn parse_currency(value: &str) -> Result<CurrencyCode, Rejection> {
    match value {
        "RUB" => Ok(CurrencyCode::Rub),
        "USD" => Ok(CurrencyCode::Usd),
        "EUR" => Ok(CurrencyCode::Eur),
        "CNY" => Ok(CurrencyCode::Cny),
        "XAU" => Ok(CurrencyCode::Xau),
        other => Err(Rejection {
            field: "currency".into(),
            expected: "RUB, USD, EUR, CNY или XAU".into(),
            actual: other.to_owned(),
        }),
    }
}

fn decimal(value: Option<&str>, field: &'static str) -> Result<Decimal, Rejection> {
    let raw = value.unwrap_or_default();
    raw.parse::<Decimal>().map_err(|_| Rejection {
        field: field.to_owned(),
        expected: "десятичное число".into(),
        actual: raw.to_owned(),
    })
}

fn minor(
    value: Option<&str>,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<i64, Rejection> {
    to_minor_units(decimal(value, field)?, currency, field)
}

fn optional_minor(
    value: Option<&str>,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<Option<i64>, Rejection> {
    match value {
        None | Some("") => Ok(None),
        Some(raw) => minor(Some(raw), field, currency).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unnamed_custody_falls_back_to_the_default_and_a_named_one_is_looked_up() {
        // Три состояния имени: не задано, задано пустым, задано.
        // Пустая строка означает то же, что и отсутствие: места хранения
        // не назвали. Изнутри разбора CSV пустую строку не получить,
        // поэтому проверка вызывается здесь напрямую.
        let default = CustodyId::new_random();
        let named = CustodyId::new_random();
        let mut directory = Directory {
            default_custody: Some(default),
            ..Directory::default()
        };
        directory.custodies.insert("НРД".into(), named);

        assert_eq!(resolve_custody(None, &directory).unwrap(), default);
        assert_eq!(resolve_custody(Some(""), &directory).unwrap(), default);
        assert_eq!(resolve_custody(Some("НРД"), &directory).unwrap(), named);
        assert_eq!(
            resolve_custody(Some("Неизвестный"), &directory)
                .unwrap_err()
                .field,
            "custody"
        );
    }

    #[test]
    fn without_a_default_an_unnamed_custody_is_a_refusal_not_a_guess() {
        let directory = Directory::default();
        let rejection = resolve_custody(None, &directory).unwrap_err();
        assert_eq!(rejection.field, "custody");
        assert_eq!(rejection.actual, "не указано");
    }
}
