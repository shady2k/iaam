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

use std::collections::{BTreeMap, BTreeSet};

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
    pub accounts: AccountNames,
    pub custodies: CustodyNames,
    pub instruments: InstrumentAliases,
    /// Место хранения по умолчанию для счёта без указанного депозитария.
    pub default_custody: Option<CustodyId>,
}

/// Названия счетов с сохранением всех совпадений.
pub type AccountNames = BTreeMap<String, Vec<AccountId>>;

/// Названия мест хранения с сохранением всех совпадений.
pub type CustodyNames = BTreeMap<String, Vec<CustodyId>>;

/// Псевдонимы инструмента со своими интервалами действия.
///
/// Плоская карта «код → идентификатор» здесь неверна: один ISIN
/// в разные годы принадлежит разным выпускам, а один выпуск за свою
/// жизнь меняет ISIN корпоративным действием (§4.7). Разрешение идёт
/// на дату строки, а не на «сегодня».
pub type InstrumentAliases = BTreeMap<String, Vec<(String, AliasInterval, InstrumentId)>>;

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
    let matching: BTreeSet<InstrumentId> = candidates
        .iter()
        .filter(|(_, interval, _)| interval.covers(on))
        .map(|(_, _, id)| *id)
        .collect();
    match matching.len() {
        1 => Ok(*matching.first().expect("непустое множество")),
        // Код известен, но не на эту дату. Отдельный текст, а не общий
        // отказ: это признак испорченной даты документа, а не новой
        // бумаги, и разбирающийся должен видеть разницу.
        0 => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код, действующий на дату операции".into(),
            actual: code.to_owned(),
        }),
        // Несколько пространств могут содержать один и тот же код. Это
        // безопасно, если они указывают на один выпуск; разные выпуски
        // требуют явного исправления входных данных.
        _ => {
            let namespaces: BTreeSet<&str> = candidates
                .iter()
                .filter(|(_, interval, _)| interval.covers(on))
                .map(|(namespace, _, _)| namespace.as_str())
                .collect();
            Err(Rejection {
                field: "instrument".to_owned(),
                expected: "один инструмент среди действующих пространств имён".into(),
                actual: format!(
                    "{code}: неоднозначность между пространствами имён {}",
                    namespaces.into_iter().collect::<Vec<_>>().join(", ")
                ),
            })
        }
    }
}

/// Инструмент по коду из названного пространства имён на дату.
///
/// Явное пространство не допускает fallback в другой namespace: отчёт,
/// назвавший тикер, не должен случайно подобрать `broker_code`.
pub(crate) fn resolve_instrument_in_namespace(
    aliases: &InstrumentAliases,
    namespace: &str,
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
    if !candidates
        .iter()
        .any(|(candidate_namespace, _, _)| candidate_namespace == namespace)
    {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код инструмента из справочника".into(),
            actual: code.to_owned(),
        });
    }
    let matching: BTreeSet<InstrumentId> = candidates
        .iter()
        .filter(|(candidate_namespace, interval, _)| {
            candidate_namespace == namespace && interval.covers(on)
        })
        .map(|(_, _, id)| *id)
        .collect();
    match matching.len() {
        1 => Ok(*matching.first().expect("непустое множество")),
        0 => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код, действующий на дату операции".into(),
            actual: code.to_owned(),
        }),
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "один инструмент среди действующих пространств имён".into(),
            actual: format!("{code}: неоднозначность в пространстве имён {namespace}"),
        }),
    }
}

/// Инструмент по коду из названного пространства без даты снимка.
pub(crate) fn resolve_instrument_in_namespace_without_date(
    aliases: &InstrumentAliases,
    namespace: &str,
    code: &str,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код инструмента из справочника".into(),
            actual: code.to_owned(),
        });
    };
    let namespaced: Vec<_> = candidates
        .iter()
        .filter(|(candidate_namespace, _, _)| candidate_namespace == namespace)
        .collect();
    match namespaced.as_slice() {
        [(_, _, instrument)] => Ok(*instrument),
        [] => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "код инструмента из справочника".into(),
            actual: code.to_owned(),
        }),
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "дата снимка отчёта для выбора кода инструмента".into(),
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
        [(_, _, instrument)] => Ok(*instrument),
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
    let account = lookup(&directory.accounts, &row.account, "account", "счёта")?;
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
                "счёта",
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
            // У строки CSV колонки вида дохода нет: источник его
            // не называл, и подставить вид было бы выдумкой (§4.9).
            kind: None,
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
        Some(name) if !name.is_empty() => resolve_named_custody(name, directory, "custody"),
        _ => directory.default_custody.ok_or_else(|| Rejection {
            field: "custody".into(),
            expected: "известное место хранения или значение по умолчанию".into(),
            actual: "не указано".into(),
        }),
    }
}

pub(crate) fn resolve_named_account(
    name: &str,
    directory: &Directory,
    field: &'static str,
) -> Result<AccountId, Rejection> {
    lookup(&directory.accounts, name, field, "счёта")
}

pub(crate) fn resolve_named_custody(
    name: &str,
    directory: &Directory,
    field: &'static str,
) -> Result<CustodyId, Rejection> {
    lookup(&directory.custodies, name, field, "места хранения")
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
    table: &BTreeMap<String, Vec<T>>,
    name: &str,
    field: &'static str,
    entity: &'static str,
) -> Result<T, Rejection> {
    let Some(candidates) = table.get(name) else {
        return Err(Rejection {
            field: field.to_owned(),
            expected: "имя из справочника".into(),
            actual: name.to_owned(),
        });
    };
    match candidates.as_slice() {
        [single] => Ok(*single),
        [] => Err(Rejection {
            field: field.to_owned(),
            expected: "имя из справочника".into(),
            actual: name.to_owned(),
        }),
        _ => Err(Rejection {
            field: field.to_owned(),
            expected: "однозначное имя из справочника".into(),
            actual: format!(
                "{name}: название {entity} неоднозначно: {} {entity}",
                candidates.len()
            ),
        }),
    }
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
                "ticker".to_owned(),
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
    fn an_explicit_namespace_does_not_fall_back_to_other_namespaces() {
        let broker_instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "broker_code".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                broker_instrument,
            )],
        );

        let refused =
            resolve_instrument_in_namespace(&aliases, "ticker", "SBER", date!(2024 - 03 - 01))
                .expect_err("тикер не должен искать в broker_code");

        assert_eq!(refused.field, "instrument");
        assert_eq!(refused.expected, "код инструмента из справочника");
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn an_instrument_known_in_multiple_namespaces_resolves_to_the_same_id() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
                (
                    "broker_code".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
            ],
        );

        let found = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01));

        assert_eq!(
            found.expect("коды разных пространств разрешены"),
            instrument
        );
    }

    #[test]
    fn an_instrument_code_is_rejected_when_namespaces_point_to_different_ids() {
        let ticker_instrument = InstrumentId::new_random();
        let broker_instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    ticker_instrument,
                ),
                (
                    "broker_code".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    broker_instrument,
                ),
            ],
        );

        let refused = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("одинаковое значение в разных пространствах неоднозначно");

        assert_eq!(refused.field, "instrument");
        assert!(refused.actual.contains("SBER"));
        assert!(refused.actual.contains("ticker"));
        assert!(refused.actual.contains("broker_code"));
    }

    #[test]
    fn an_instrument_with_multiple_namespaces_still_requires_a_date() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
                (
                    "broker_code".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
            ],
        );

        let refused = resolve_instrument_without_date(&aliases, "SBER")
            .expect_err("без даты нельзя выбрать пространство имён");

        assert_eq!(
            refused.expected,
            "дата снимка отчёта для выбора кода инструмента"
        );
    }

    #[test]
    fn пространство_без_даты_принимает_единственный_псевдоним() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        assert_eq!(
            resolve_instrument_in_namespace_without_date(&aliases, "ticker", "SBER")
                .expect("единственный псевдоним разрешён"),
            instrument
        );
    }

    #[test]
    fn пространство_без_даты_отвергает_код_из_другого_пространства() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "broker_code".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let refused = resolve_instrument_in_namespace_without_date(&aliases, "ticker", "SBER")
            .expect_err("чужое пространство не должно разрешаться");

        assert_eq!(refused.field, "instrument");
        assert_eq!(refused.expected, "код инструмента из справочника");
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn пространство_без_даты_отвергает_несколько_псевдонимов_с_причиной() {
        let first = InstrumentId::new_random();
        let second = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    first,
                ),
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2024 - 01 - 01),
                        valid_to: None,
                    },
                    second,
                ),
            ],
        );

        let refused = resolve_instrument_in_namespace_without_date(&aliases, "ticker", "SBER")
            .expect_err("несколько псевдонимов требуют дату снимка");

        assert_eq!(
            refused.expected,
            "дата снимка отчёта для выбора кода инструмента"
        );
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn пространство_с_датой_отличает_неработающий_код_от_неоднозначного() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2025 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let refused =
            resolve_instrument_in_namespace(&aliases, "ticker", "SBER", date!(2024 - 03 - 01))
                .expect_err("код вне интервала должен быть отвергнут");

        assert_eq!(refused.field, "instrument");
        assert_eq!(refused.expected, "код, действующий на дату операции");
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn a_duplicate_custody_title_is_rejected_as_ambiguous() {
        let first = CustodyId::new_random();
        let second = CustodyId::new_random();
        let mut directory = Directory::default();
        directory
            .custodies
            .insert("НРД".into(), vec![first, second]);

        let refused = resolve_custody(Some("НРД"), &directory)
            .expect_err("одинаковое название места хранения неоднозначно");

        assert_eq!(refused.field, "custody");
        assert!(refused.actual.contains("НРД"));
        assert!(refused.actual.contains("неоднозначно"));
    }

    #[test]
    fn an_instrument_code_is_rejected_on_the_first_day_after_its_interval() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
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
                "ticker".to_owned(),
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
        directory.custodies.insert("НРД".into(), vec![named]);

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
    #[test]
    fn a_known_code_outside_its_interval_reports_a_date_specific_rejection() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2025 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let refused = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("код известен, но не действует на дату");

        assert_eq!(refused.field, "instrument");
        assert_eq!(
            refused.expected, "код, действующий на дату операции",
            "код, известный справочнику, должен отличаться от неизвестного"
        );
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn an_instrument_without_date_returns_the_only_candidate() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        assert_eq!(
            resolve_instrument_without_date(&aliases, "SBER").expect("единственный код"),
            instrument
        );
    }

    #[test]
    fn an_empty_instrument_history_without_date_has_the_date_specific_rejection() {
        let mut aliases = BTreeMap::new();
        aliases.insert("SBER".to_owned(), Vec::new());

        let refused = resolve_instrument_without_date(&aliases, "SBER")
            .expect_err("пустая история инструмента");

        assert_eq!(refused.field, "instrument");
        assert_eq!(refused.expected, "код, действующий на дату операции");
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn an_empty_custody_history_is_rejected_as_an_unknown_name() {
        let mut directory = Directory::default();
        directory.custodies.insert("НРД".into(), Vec::new());

        let refused = resolve_named_custody("НРД", &directory, "custody")
            .expect_err("пустая история места хранения");

        assert_eq!(refused.field, "custody");
        assert_eq!(refused.expected, "имя из справочника");
        assert_eq!(refused.actual, "НРД");
    }
}
