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
use iaam_core::instrument::AliasInterval;
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
    pub instruments: InstrumentAliases,
    /// Место хранения по умолчанию для счёта без указанного депозитария.
    pub default_custody: Option<CustodyId>,
}

/// Псевдонимы инструмента со своими интервалами действия.
///
/// Плоская карта «код → идентификатор» здесь неверна: один ISIN
/// в разные годы принадлежит разным выпускам, а один выпуск за свою
/// жизнь меняет ISIN корпоративным действием (§4.7). Разрешение идёт
/// на дату строки, а не на «сегодня».
pub type InstrumentAliases = BTreeMap<String, Vec<(AliasInterval, InstrumentId)>>;

/// Инструмент по коду на дату строки.
pub fn resolve_instrument(
    aliases: &InstrumentAliases,
    code: &str,
    on: Date,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код инструмента из справочника".into(),
            actual: code.to_owned(),
        });
    };
    let matching: Vec<InstrumentId> = candidates
        .iter()
        .filter(|(interval, _)| interval.covers(on))
        .map(|(_, id)| *id)
        .collect();
    match matching.as_slice() {
        [single] => Ok(*single),
        // Код известен, но не на эту дату. Отдельный текст, а не общий
        // отказ: это признак испорченной даты документа, а не новой
        // бумаги, и разбирающийся должен видеть разницу.
        [] => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код, действующий на дату операции".into(),
            actual: code.to_owned(),
        }),
        // Пересечение интервалов ловится триггером схемы; сюда попасть
        // можно только на справочнике, собранном мимо базы.
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "однозначный код инструмента".into(),
            actual: code.to_owned(),
        }),
    }
}

/// Инструмент для снимка без даты: допустим только единственный псевдоним.
///
/// Если отчёт не назвал дату снимка, несколько исторических вариантов
/// нельзя разрешить догадкой. Единственная запись сохраняет прежний
/// однозначный сценарий; история из двух и более записей даёт отказ.
pub fn resolve_instrument_without_date(
    aliases: &InstrumentAliases,
    code: &str,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код инструмента из справочника".into(),
            actual: code.to_owned(),
        });
    };
    match candidates.as_slice() {
        [(_, instrument)] => Ok(*instrument),
        [] => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код, действующий на дату операции".into(),
            actual: code.to_owned(),
        }),
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "дата снимка отчёта для выбора кода инструмента".into(),
            actual: code.to_owned(),
        }),
    }
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
    let kind = build_kind(row, directory, currency, date)?;

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
    date: Date,
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
        "buy" | "sell" => build_trade(row, directory, currency, date),
        "income" => Ok(OperationKind::Income {
            instrument: match row.instrument.as_deref() {
                None | Some("") => None,
                Some(symbol) => Some(resolve_instrument(&directory.instruments, symbol, date)?),
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
            instrument: resolve_instrument(
                &directory.instruments,
                row.instrument.as_deref().unwrap_or_default(),
                date,
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
    date: Date,
) -> Result<OperationKind, Rejection> {
    let instrument = resolve_instrument(
        &directory.instruments,
        row.instrument.as_deref().unwrap_or_default(),
        date,
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
    use time::macros::date;

    #[test]
    fn an_instrument_named_by_code_is_resolved_on_the_row_date() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let found = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01));

        assert_eq!(found.expect("код разрешён"), instrument);
    }

    #[test]
    fn an_instrument_code_is_rejected_on_the_first_day_after_its_interval() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: Some(date!(2024 - 03 - 01)),
                },
                instrument,
            )],
        );

        let refused = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("интервал уже закончился");

        assert_eq!(refused.field, "instrument");
    }

    #[test]
    fn an_unknown_code_is_refused_and_not_skipped() {
        let aliases = BTreeMap::new();

        let refused = resolve_instrument(&aliases, "NOPE", date!(2024 - 03 - 01))
            .expect_err("неизвестный код");

        assert_eq!(refused.field, "instrument");
    }

    #[test]
    fn a_code_outside_its_interval_is_told_apart_from_an_unknown_one() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                AliasInterval {
                    valid_from: date!(2025 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let unknown = resolve_instrument(&BTreeMap::new(), "SBER", date!(2024 - 03 - 01))
            .expect_err("код отсутствует");
        let too_early = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("код ещё не действовал");

        assert_ne!(
            unknown.expected, too_early.expected,
            "новая бумага и испорченная дата обязаны звучать по-разному"
        );
    }

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
