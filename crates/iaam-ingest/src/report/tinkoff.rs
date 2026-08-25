//! Парсер XLSX-отчёта Т-Инвестиций (§10.1, §10.3).
//!
//! Формат распознаётся по содержимому книги. Операционные листы разбираются
//! построчно: ошибка одной строки становится `Rejected`, а РЕПО сохраняется
//! отдельно в карантине (§11).

use iaam_core::event::kind::FeeOrigin;
use iaam_core::event::provenance::{ParserVersion, RawHash, RowLocator};
use iaam_core::ids::{CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use rust_decimal::Decimal;
use time::Date;
use time::macros::format_description;

use crate::csv_source::{Directory, ParsedRow};
use crate::operation::{OperationDates, OperationKind, SubmittedOperation, to_minor_units};
use crate::report::sections::{
    CashSection, ControlSections, PositionSection, TotalSection, TurnoverSection,
};
use crate::report::workbook::{Cell, Sheet, Workbook};
use crate::report::{
    Broker, LocatedRow, ParsedReport, Quarantined, ReportFormat, ReportParser, UnsupportedReason,
};
use crate::verdict::Rejection;

const PARSER_VERSION: &str = "tinkoff-xlsx/1";
const DOCUMENT_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Парсер выгрузки брокерского отчёта Т-Инвестиций.
#[derive(Debug, Clone, Copy, Default)]
pub struct TinkoffParser;

impl ReportParser for TinkoffParser {
    fn broker(&self) -> Broker {
        Broker::Tinkoff
    }

    fn format(&self) -> ReportFormat {
        ReportFormat::Xlsx
    }

    fn version(&self) -> ParserVersion {
        ParserVersion(PARSER_VERSION.to_owned())
    }

    fn recognises(&self, workbook: &Workbook) -> bool {
        workbook
            .sheet("Общие сведения")
            .is_some_and(|sheet| sheet.contains_text("БРОКЕРСКИЙ ОТЧЕТ"))
            && workbook
                .sheet("Сделки")
                .is_some_and(|sheet| sheet.contains_text("Номер сделки"))
            && workbook.sheet("Денежные операции").is_some()
    }

    fn parse(&self, workbook: &Workbook, directory: &Directory) -> ParsedReport {
        let mut report = ParsedReport {
            period: report_period(workbook.sheet("Общие сведения")),
            rows: Vec::new(),
            sections: ControlSections::default(),
            unsupported: Vec::new(),
        };

        if let Some(sheet) = workbook.sheet("Сделки") {
            parse_trade_sheet(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("Денежные операции") {
            parse_cash_sheet(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("Комиссии") {
            parse_fee_sheet(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("Купоны и дивиденды") {
            parse_income_sheet(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("РЕПО") {
            quarantine_repo_sheet(sheet, &mut report.unsupported);
        }

        parse_cash_balances(
            workbook.sheet("Остатки денежных средств"),
            &mut report.sections,
        );
        parse_turnovers(workbook.sheet("Обороты"), &mut report.sections);
        parse_positions(
            workbook.sheet("Остатки ценных бумаг"),
            directory,
            &mut report.sections,
        );
        parse_totals(workbook.sheet("Итоги"), &mut report.sections);
        report
    }
}

fn report_period(sheet: Option<&Sheet>) -> Option<AssertionPeriod> {
    let sheet = sheet?;
    for row in &sheet.rows {
        if !row.iter().any(|cell| {
            cell.text()
                .is_some_and(|text| text.contains("Период отчета"))
        }) {
            continue;
        }
        let dates: Vec<Date> = row.iter().filter_map(cell_date).collect();
        if dates.len() < 2 {
            return None;
        }
        return AssertionPeriod::between(dates[0], dates[1]);
    }
    None
}

fn parse_trade_sheet(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(date_col) = column(&headers, "дата") else {
        return;
    };
    let Some(operation_col) = column(&headers, "операция") else {
        return;
    };
    let Some(account_col) = column(&headers, "счет") else {
        return;
    };
    let Some(instrument_col) = column(&headers, "тикер") else {
        return;
    };
    let Some(custody_col) = column(&headers, "место хранения") else {
        return;
    };
    let Some(quantity_col) = column(&headers, "количество") else {
        return;
    };
    let Some(gross_col) = column(&headers, "сумма сделки") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let fee_col = column(&headers, "комиссия");
    let accrued_col = column(&headers, "нкд");
    let source_id_col = column(&headers, "номер сделки");

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            let date = date_value(cell(row, date_col), "date")?;
            let currency = currency_value(cell(row, currency_col))?;
            let account =
                lookup_account(directory, text_value(cell(row, account_col))?, "account")?;
            let instrument = lookup_instrument(
                directory,
                text_value(cell(row, instrument_col))?,
                "instrument",
            )?;
            let custody =
                lookup_custody(directory, text_value(cell(row, custody_col))?, "custody")?;
            let quantity = quantity_value(cell(row, quantity_col), "quantity")?;
            let gross_minor = money_value(cell(row, gross_col), "amount", currency)?;
            let fee_minor =
                optional_money(cell(row, fee_col.unwrap_or(usize::MAX)), "fee", currency)?;
            let accrued_interest_minor = optional_money(
                cell(row, accrued_col.unwrap_or(usize::MAX)),
                "accrued_interest",
                currency,
            )?;
            let kind = match text_value(cell(row, operation_col))?
                .to_lowercase()
                .as_str()
            {
                value if value.contains("покуп") => OperationKind::Buy {
                    instrument,
                    custody,
                    quantity,
                    gross_minor,
                    fee_minor,
                    accrued_interest_minor,
                    currency,
                },
                value if value.contains("продаж") => OperationKind::Sell {
                    instrument,
                    custody,
                    quantity,
                    gross_minor,
                    fee_minor,
                    accrued_interest_minor,
                    currency,
                },
                other => return Err(rejection("type", "Покупка или Продажа", other)),
            };
            Ok(operation(
                account,
                kind,
                date,
                source_id_col.and_then(|column| optional_text(cell(row, column))),
            ))
        })();
        rows.push(located("Сделки", row_number, result));
    }
}

fn parse_cash_sheet(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(date_col) = column(&headers, "дата") else {
        return;
    };
    let Some(operation_col) = column(&headers, "операция") else {
        return;
    };
    let Some(amount_col) = column(&headers, "сумма") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let Some(account_col) = column(&headers, "счет") else {
        return;
    };

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            let date = date_value(cell(row, date_col), "date")?;
            let currency = currency_value(cell(row, currency_col))?;
            let account =
                lookup_account(directory, text_value(cell(row, account_col))?, "account")?;
            let amount_minor = money_value(cell(row, amount_col), "amount", currency)?;
            let operation_text = text_value(cell(row, operation_col))?.to_lowercase();
            let kind = if operation_text.contains("пополн") {
                OperationKind::Deposit {
                    amount_minor,
                    currency,
                }
            } else if operation_text.contains("вывод") || operation_text.contains("сняти")
            {
                OperationKind::Withdrawal {
                    amount_minor,
                    currency,
                }
            } else {
                return Err(rejection("type", "Пополнение или Вывод", &operation_text));
            };
            Ok(operation(account, kind, date, None))
        })();
        rows.push(located("Денежные операции", row_number, result));
    }
}

fn parse_fee_sheet(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(date_col) = column(&headers, "дата") else {
        return;
    };
    let Some(amount_col) = column(&headers, "сумма") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let Some(account_col) = column(&headers, "счет") else {
        return;
    };

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            let date = date_value(cell(row, date_col), "date")?;
            let currency = currency_value(cell(row, currency_col))?;
            let account =
                lookup_account(directory, text_value(cell(row, account_col))?, "account")?;
            let amount_minor = money_value(cell(row, amount_col), "amount", currency)?;
            Ok(operation(
                account,
                OperationKind::Fee {
                    amount_minor,
                    currency,
                    origin: FeeOrigin::Other,
                },
                date,
                None,
            ))
        })();
        rows.push(located("Комиссии", row_number, result));
    }
}

fn parse_income_sheet(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(date_col) = column(&headers, "дата") else {
        return;
    };
    let Some(instrument_col) = column(&headers, "тикер") else {
        return;
    };
    let Some(amount_col) = column(&headers, "сумма") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let Some(account_col) = column(&headers, "счет") else {
        return;
    };

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            let date = date_value(cell(row, date_col), "date")?;
            let currency = currency_value(cell(row, currency_col))?;
            let account =
                lookup_account(directory, text_value(cell(row, account_col))?, "account")?;
            let instrument = optional_text(cell(row, instrument_col))
                .map(|name| lookup_instrument(directory, name, "instrument"))
                .transpose()?;
            let gross_minor = money_value(cell(row, amount_col), "amount", currency)?;
            Ok(operation(
                account,
                OperationKind::Income {
                    instrument,
                    gross_minor,
                    currency,
                },
                date,
                None,
            ))
        })();
        rows.push(located("Купоны и дивиденды", row_number, result));
    }
}

fn quarantine_repo_sheet(sheet: &Sheet, unsupported: &mut Vec<Quarantined>) {
    for (index, row) in data_rows(sheet).enumerate() {
        if row.iter().all(Cell::is_empty) {
            continue;
        }
        unsupported.push(Quarantined {
            locator: locator("РЕПО", index as u64 + 2),
            reason: UnsupportedReason::Repo,
        });
    }
}

fn parse_cash_balances(sheet: Option<&Sheet>, sections: &mut ControlSections) {
    let Some(sheet) = sheet else { return };
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let opening_col = column(&headers, "остаток на начало");
    let closing_col = column(&headers, "остаток на конец");
    for row in data_rows(sheet) {
        let Ok(currency) = currency_value(cell(row, currency_col)) else {
            continue;
        };
        if let Some(column) = opening_col {
            if let Ok(amount) = money_value(cell(row, column), "opening", currency) {
                sections.cash_balances.push(CashSection {
                    currency,
                    amount: PostedMinor::new(amount),
                    at: BalancePoint::Opening,
                });
            }
        }
        if let Some(column) = closing_col {
            if let Ok(amount) = money_value(cell(row, column), "closing", currency) {
                sections.cash_balances.push(CashSection {
                    currency,
                    amount: PostedMinor::new(amount),
                    at: BalancePoint::Closing,
                });
            }
        }
    }
}

fn parse_turnovers(sheet: Option<&Sheet>, sections: &mut ControlSections) {
    let Some(sheet) = sheet else { return };
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let Some(debit_col) = column(&headers, "дебет") else {
        return;
    };
    let Some(credit_col) = column(&headers, "кредит") else {
        return;
    };
    for row in data_rows(sheet) {
        let Ok(currency) = currency_value(cell(row, currency_col)) else {
            continue;
        };
        let Ok(debit) = money_value(cell(row, debit_col), "debit", currency) else {
            continue;
        };
        let Ok(credit) = money_value(cell(row, credit_col), "credit", currency) else {
            continue;
        };
        sections.turnovers.push(TurnoverSection {
            currency,
            debit: PostedMinor::new(debit),
            credit: PostedMinor::new(credit),
        });
    }
}

fn parse_positions(sheet: Option<&Sheet>, directory: &Directory, sections: &mut ControlSections) {
    let Some(sheet) = sheet else { return };
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(instrument_col) = column(&headers, "тикер") else {
        return;
    };
    let Some(custody_col) = column(&headers, "место хранения") else {
        return;
    };
    let opening_col = column(&headers, "количество на начало");
    let closing_col = column(&headers, "количество на конец");
    for row in data_rows(sheet) {
        let Ok(instrument) = lookup_instrument(
            directory,
            text_value(cell(row, instrument_col)).unwrap_or_default(),
            "instrument",
        ) else {
            continue;
        };
        let Ok(custody) = lookup_custody(
            directory,
            text_value(cell(row, custody_col)).unwrap_or_default(),
            "custody",
        ) else {
            continue;
        };
        if let Some(column) = opening_col {
            if let Ok(quantity) = quantity_value(cell(row, column), "opening_quantity") {
                sections.positions.push(PositionSection {
                    instrument,
                    custody,
                    quantity: Quantity(quantity),
                    at: BalancePoint::Opening,
                });
            }
        }
        if let Some(column) = closing_col {
            if let Ok(quantity) = quantity_value(cell(row, column), "closing_quantity") {
                sections.positions.push(PositionSection {
                    instrument,
                    custody,
                    quantity: Quantity(quantity),
                    at: BalancePoint::Closing,
                });
            }
        }
    }
}

fn parse_totals(sheet: Option<&Sheet>, sections: &mut ControlSections) {
    let Some(sheet) = sheet else { return };
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(kind_col) = column(&headers, "показатель") else {
        return;
    };
    let Some(amount_col) = column(&headers, "сумма") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    for row in data_rows(sheet) {
        let Ok(kind) = text_value(cell(row, kind_col)) else {
            continue;
        };
        let Ok(currency) = currency_value(cell(row, currency_col)) else {
            continue;
        };
        let Ok(amount) = money_value(cell(row, amount_col), "amount", currency) else {
            continue;
        };
        let total = TotalSection {
            currency,
            amount: PostedMinor::new(amount),
        };
        let kind = kind.to_lowercase();
        if kind.contains("комис") {
            sections.fees = Some(total);
        } else if kind.contains("купон") || kind.contains("дивид") {
            sections.income = Some(total);
        } else if kind.contains("налог") {
            sections.tax_withheld = Some(total);
        }
    }
}

fn headers(sheet: &Sheet) -> Option<Vec<String>> {
    (!sheet.rows.is_empty()).then(|| {
        sheet.rows[0]
            .iter()
            .map(|cell| cell.text().unwrap_or_default().trim().to_lowercase())
            .collect()
    })
}

fn column(headers: &[String], name: &str) -> Option<usize> {
    headers.iter().position(|header| header.contains(name))
}

fn data_rows(sheet: &Sheet) -> impl Iterator<Item = &[Cell]> {
    sheet.rows.iter().skip(1).map(Vec::as_slice)
}

fn cell(row: &[Cell], index: usize) -> &Cell {
    row.get(index).unwrap_or(&Cell::Empty)
}

fn cell_date(cell: &Cell) -> Option<Date> {
    match cell {
        Cell::Date(value) => Some(*value),
        Cell::Text(value) => parse_date_text(value).ok(),
        Cell::Empty | Cell::Number(_) | Cell::Bool(_) | Cell::Error(_) => None,
    }
}

fn date_value(cell: &Cell, field: &'static str) -> Result<Date, Rejection> {
    cell_date(cell).ok_or_else(|| rejection(field, "дата отчёта", &cell_description(cell)))
}

fn parse_date_text(value: &str) -> Result<Date, ()> {
    Date::parse(value.trim(), format_description!("[year]-[month]-[day]"))
        .or_else(|_| Date::parse(value.trim(), format_description!("[day].[month].[year]")))
        .map_err(|_| ())
}

fn currency_value(cell: &Cell) -> Result<CurrencyCode, Rejection> {
    match text_value(cell)?.to_uppercase().as_str() {
        "RUB" | "₽" => Ok(CurrencyCode::Rub),
        "USD" | "$" => Ok(CurrencyCode::Usd),
        "EUR" | "€" => Ok(CurrencyCode::Eur),
        "CNY" => Ok(CurrencyCode::Cny),
        "XAU" => Ok(CurrencyCode::Xau),
        value => Err(rejection("currency", "RUB, USD, EUR, CNY или XAU", value)),
    }
}

fn decimal_value(cell: &Cell, field: &'static str) -> Result<Decimal, Rejection> {
    match cell {
        Cell::Number(value) => Ok(value.inner()),
        Cell::Text(value) => value
            .replace(['\u{a0}', ' '], "")
            .replace(',', ".")
            .parse::<Decimal>()
            .map_err(|_| rejection(field, "десятичное число", value)),
        _ => Err(rejection(
            field,
            "десятичное число",
            &cell_description(cell),
        )),
    }
}

fn money_value(cell: &Cell, field: &'static str, currency: CurrencyCode) -> Result<i64, Rejection> {
    to_minor_units(decimal_value(cell, field)?, currency, field)
}

fn optional_money(
    cell: &Cell,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<Option<i64>, Rejection> {
    if cell.is_empty() {
        Ok(None)
    } else {
        money_value(cell, field, currency).map(Some)
    }
}

fn quantity_value(cell: &Cell, field: &'static str) -> Result<Dec, Rejection> {
    decimal_value(cell, field).map(Dec::new)
}

fn text_value(cell: &Cell) -> Result<&str, Rejection> {
    cell.text()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| rejection("value", "непустой текст", &cell_description(cell)))
}

fn optional_text(cell: &Cell) -> Option<&str> {
    cell.text().filter(|value| !value.trim().is_empty())
}

fn lookup_account(
    directory: &Directory,
    name: &str,
    field: &'static str,
) -> Result<iaam_core::ids::AccountId, Rejection> {
    directory
        .accounts
        .get(name)
        .copied()
        .ok_or_else(|| rejection(field, "имя из справочника", name))
}

fn lookup_instrument(
    directory: &Directory,
    name: &str,
    field: &'static str,
) -> Result<InstrumentId, Rejection> {
    directory
        .instruments
        .get(name)
        .copied()
        .ok_or_else(|| rejection(field, "имя из справочника", name))
}

fn lookup_custody(
    directory: &Directory,
    name: &str,
    field: &'static str,
) -> Result<CustodyId, Rejection> {
    directory
        .custodies
        .get(name)
        .copied()
        .ok_or_else(|| rejection(field, "имя из справочника", name))
}

fn operation(
    account: iaam_core::ids::AccountId,
    kind: OperationKind,
    date: Date,
    source_id: Option<&str>,
) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(date),
            cash_posted: Some(date),
            ..OperationDates::default()
        },
        idempotency_key: None,
        source_operation_id: source_id.map(str::to_owned),
    }
}

fn located(sheet: &str, row: u64, result: Result<SubmittedOperation, Rejection>) -> LocatedRow {
    LocatedRow {
        locator: locator(sheet, row),
        outcome: result
            .map(|operation| ParsedRow::Operation(Box::new(operation)))
            .unwrap_or_else(ParsedRow::Rejected),
    }
}

fn locator(sheet: &str, row: u64) -> RowLocator {
    RowLocator {
        document: RawHash::parse(DOCUMENT_HASH)
            .unwrap_or_else(|| unreachable!("the parser document hash is a valid constant")),
        sheet: Some(sheet.to_owned()),
        row,
    }
}

fn rejection(field: &'static str, expected: &str, actual: &str) -> Rejection {
    Rejection {
        field: field.to_owned(),
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    }
}

fn cell_description(cell: &Cell) -> String {
    match cell {
        Cell::Empty => "пусто".to_owned(),
        Cell::Text(value) | Cell::Error(value) => value.clone(),
        Cell::Number(value) => value.inner().to_string(),
        Cell::Date(value) => value.to_string(),
        Cell::Bool(value) => value.to_string(),
    }
}
