//! Finam XLS report parser (§10.1, §10.3).
//!
//! Finam's format is parsed using dedicated functions and dedicated sheet markers.
//! Only result types are shared with other channels.

use iaam_core::event::kind::{FeeOrigin, IncomeKind};
use iaam_core::event::provenance::{ParserVersion, RawHash, RowLocator};
use iaam_core::ids::{CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use rust_decimal::Decimal;
use time::Date;
use time::macros::format_description;

use crate::csv_source::{
    Directory, ParsedRow, resolve_instrument, resolve_instrument_without_date,
    resolve_named_account, resolve_named_custody,
};
use crate::operation::{OperationDates, OperationKind, SubmittedOperation, to_minor_units};
use crate::report::sections::{
    CashSection, ControlSections, PositionSection, TotalSection, TurnoverSection,
};
use crate::report::workbook::{Cell, Sheet, Workbook};
use crate::report::{
    Broker, LocatedRow, ParsedReport, Quarantined, ReportFormat, ReportParser, UnsupportedReason,
};
use crate::verdict::Rejection;

const PARSER_VERSION: &str = "finam-xls/2";
const DOCUMENT_HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// Parser for a Finam broker report export.
#[derive(Debug, Clone, Copy, Default)]
pub struct FinamParser;

impl ReportParser for FinamParser {
    fn broker(&self) -> Broker {
        Broker::Finam
    }

    fn format(&self) -> ReportFormat {
        ReportFormat::Xls
    }

    fn version(&self) -> ParserVersion {
        ParserVersion(PARSER_VERSION.to_owned())
    }

    fn recognises(&self, workbook: &Workbook) -> bool {
        workbook
            .sheet("Сведения")
            .is_some_and(|sheet| sheet.contains_text("ОТЧЕТ БРОКЕРА ФИНАМ"))
            && workbook.sheet("Сделки").is_some_and(|sheet| {
                sheet.contains_text("Дата сделки") && sheet.contains_text("Код договора")
            })
            && workbook.sheet("Денежные движения").is_some()
    }

    fn parse(&self, workbook: &Workbook, directory: &Directory) -> ParsedReport {
        let mut report = ParsedReport {
            period: report_period(workbook.sheet("Сведения")),
            rows: Vec::new(),
            sections: ControlSections::default(),
            unsupported: Vec::new(),
        };

        if let Some(sheet) = workbook.sheet("Сделки") {
            parse_trades(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("Денежные движения") {
            parse_cash_movements(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("Списания комиссий") {
            parse_fees(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("Выплаты") {
            parse_income(sheet, directory, &mut report.rows);
        }
        if let Some(sheet) = workbook.sheet("РЕПО") {
            quarantine_repo(sheet, &mut report.unsupported);
        }

        parse_cash_balances(workbook.sheet("Денежные остатки"), &mut report.sections);
        parse_turnovers(
            workbook.sheet("Обороты денежных средств"),
            &mut report.sections,
        );
        parse_positions(
            workbook.sheet("Позиции"),
            directory,
            report.period.map(|period| period.to),
            &mut report.sections,
        );
        parse_totals(workbook.sheet("Сводные итоги"), &mut report.sections);
        report
    }
}

fn report_period(sheet: Option<&Sheet>) -> Option<AssertionPeriod> {
    let sheet = sheet?;
    for row in &sheet.rows {
        if !row.iter().any(|cell| {
            cell.text()
                .is_some_and(|text| text.contains("Отчетный период"))
        }) {
            continue;
        }
        let dates: Vec<Date> = row.iter().filter_map(cell_date).collect();
        if dates.len() >= 2 {
            return AssertionPeriod::between(dates[0], dates[1]);
        }
    }
    None
}

fn parse_trades(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(trade_date_col) = column(&headers, "дата сделки") else {
        return;
    };
    let settlement_date_col = column(&headers, "дата расчетов");
    let Some(account_col) = column(&headers, "код договора") else {
        return;
    };
    let Some(instrument_col) = column(&headers, "код инструмента") else {
        return;
    };
    let Some(side_col) = column(&headers, "направление") else {
        return;
    };
    let Some(quantity_col) = column(&headers, "объем") else {
        return;
    };
    let Some(gross_col) = column(&headers, "сумма сделки") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта расчетов") else {
        return;
    };
    let Some(custody_col) = column(&headers, "место учета") else {
        return;
    };
    let fee_col = column(&headers, "комиссия");
    let accrued_col = column(&headers, "нкд");
    let source_id_col = column(&headers, "идентификатор сделки");

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            let trade_date = date_value(cell(row, trade_date_col), "trade_date")?;
            let settlement_date = settlement_date_col
                .map(|column| date_value(cell(row, column), "settlement_date"))
                .transpose()?;
            let currency = currency_value(cell(row, currency_col))?;
            let account = account_value(directory, cell(row, account_col), "account")?;
            let instrument =
                instrument_value(directory, cell(row, instrument_col), Some(trade_date))?;
            let custody = custody_value(directory, cell(row, custody_col), "custody")?;
            let quantity = quantity_value(cell(row, quantity_col), "quantity")?;
            let gross_minor = money_value(cell(row, gross_col), "amount", currency)?;
            let fee_minor = optional_money(row, fee_col, "fee", currency)?;
            let accrued_interest_minor =
                optional_money(row, accrued_col, "accrued_interest", currency)?;
            let side = text_value(cell(row, side_col))?.to_lowercase();
            let kind = if side == "купля" || side == "покупка" {
                OperationKind::Buy {
                    instrument,
                    custody,
                    quantity,
                    gross_minor,
                    fee_minor,
                    basis_fee: None,
                    accrued_interest_minor,
                    currency,
                }
            } else if side == "продажа" || side == "продаж" {
                OperationKind::Sell {
                    instrument,
                    custody,
                    quantity,
                    gross_minor,
                    fee_minor,
                    basis_fee: None,
                    accrued_interest_minor,
                    currency,
                }
            } else {
                return Err(rejection("type", "buy or sell", &side));
            };
            Ok(operation(
                account,
                kind,
                trade_date,
                settlement_date,
                None,
                source_id_col.and_then(|column| optional_text(cell(row, column))),
            ))
        })();
        rows.push(located("Сделки", row_number, result));
    }
}

fn parse_cash_movements(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(date_col) = column(&headers, "дата проводки") else {
        return;
    };
    let Some(account_col) = column(&headers, "код договора") else {
        return;
    };
    let Some(kind_col) = column(&headers, "вид движения") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let Some(amount_col) = column(&headers, "сумма") else {
        return;
    };

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            let date = date_value(cell(row, date_col), "date")?;
            let currency = currency_value(cell(row, currency_col))?;
            let account = account_value(directory, cell(row, account_col), "account")?;
            let amount_minor = money_value(cell(row, amount_col), "amount", currency)?;
            let kind_text = text_value(cell(row, kind_col))?.to_lowercase();
            let kind = if kind_text == "ввод денежных средств" {
                OperationKind::Deposit {
                    amount_minor,
                    currency,
                }
            } else if kind_text == "вывод денежных средств" {
                OperationKind::Withdrawal {
                    amount_minor,
                    currency,
                }
            } else {
                return Err(rejection("type", "deposit or withdrawal", &kind_text));
            };
            Ok(operation(account, kind, date, None, Some(date), None))
        })();
        rows.push(located("Денежные движения", row_number, result));
    }
}

fn parse_fees(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(date_col) = column(&headers, "дата проводки") else {
        return;
    };
    let Some(account_col) = column(&headers, "код договора") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };
    let Some(amount_col) = column(&headers, "сумма комиссии") else {
        return;
    };

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            let date = date_value(cell(row, date_col), "date")?;
            let currency = currency_value(cell(row, currency_col))?;
            let account = account_value(directory, cell(row, account_col), "account")?;
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
                Some(date),
                None,
            ))
        })();
        rows.push(located("Списания комиссий", row_number, result));
    }
}

fn parse_income(sheet: &Sheet, directory: &Directory, rows: &mut Vec<LocatedRow>) {
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(date_col) = column(&headers, "дата выплаты") else {
        return;
    };
    let Some(account_col) = column(&headers, "код договора") else {
        return;
    };
    let Some(kind_col) = column(&headers, "событие") else {
        return;
    };
    let Some(instrument_col) = column(&headers, "код инструмента") else {
        return;
    };
    let Some(amount_col) = column(&headers, "сумма") else {
        return;
    };
    let Some(currency_col) = column(&headers, "валюта") else {
        return;
    };

    for (index, row) in data_rows(sheet).enumerate() {
        let row_number = index as u64 + 2;
        let result = (|| {
            // The report names the operation type with a word, and that word reaches
            // the operation: the «Выплаты» sheet accepts only coupon
            // and dividend, so saying «not stated» here would be
            // a loss, not honesty.
            let date = date_value(cell(row, date_col), "date")?;
            let currency = currency_value(cell(row, currency_col))?;
            let account = account_value(directory, cell(row, account_col), "account")?;
            let kind = text_value(cell(row, kind_col))?.to_lowercase();
            let income_kind = match kind.as_str() {
                "купон" => IncomeKind::Coupon,
                "дивиденд" => IncomeKind::Dividend,
                other => return Err(rejection("type", "coupon or dividend", other)),
            };
            let instrument = optional_text(cell(row, instrument_col))
                .map(|_| instrument_value(directory, cell(row, instrument_col), Some(date)))
                .transpose()?;
            let gross_minor = money_value(cell(row, amount_col), "amount", currency)?;
            Ok(operation(
                account,
                OperationKind::Income {
                    instrument,
                    gross_minor,
                    currency,
                    kind: Some(income_kind),
                },
                date,
                None,
                Some(date),
                None,
            ))
        })();
        rows.push(located("Выплаты", row_number, result));
    }
}

fn quarantine_repo(sheet: &Sheet, unsupported: &mut Vec<Quarantined>) {
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
    let Some(currency_col) = column(&headers, "валюта расчетов") else {
        return;
    };
    let opening_col = column(&headers, "на начало периода");
    let closing_col = column(&headers, "на конец периода");
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
    let Some(currency_col) = column(&headers, "валюта расчетов") else {
        return;
    };
    let Some(debit_col) = column(&headers, "списано") else {
        return;
    };
    let Some(credit_col) = column(&headers, "зачислено") else {
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

fn parse_positions(
    sheet: Option<&Sheet>,
    directory: &Directory,
    on: Option<Date>,
    sections: &mut ControlSections,
) {
    let Some(sheet) = sheet else { return };
    let Some(headers) = headers(sheet) else {
        return;
    };
    let Some(instrument_col) = column(&headers, "код инструмента") else {
        return;
    };
    let Some(custody_col) = column(&headers, "место учета") else {
        return;
    };
    let opening_col = column(&headers, "остаток на начало");
    let closing_col = column(&headers, "остаток на конец");
    for row in data_rows(sheet) {
        let Ok(instrument) = instrument_value(directory, cell(row, instrument_col), on) else {
            continue;
        };
        let Ok(custody) = custody_value(directory, cell(row, custody_col), "custody") else {
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
    headers.iter().position(|header| header == name)
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
    cell_date(cell).ok_or_else(|| rejection(field, "report date", &cell_description(cell)))
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
        value => Err(rejection("currency", "RUB, USD, EUR, CNY or XAU", value)),
    }
}

fn decimal_value(cell: &Cell, field: &'static str) -> Result<Decimal, Rejection> {
    match cell {
        Cell::Number(value) => Ok(value.inner()),
        Cell::Text(value) => value
            .replace(['\u{a0}', ' '], "")
            .replace(',', ".")
            .parse::<Decimal>()
            .map_err(|_| rejection(field, "decimal number", value)),
        _ => Err(rejection(field, "decimal number", &cell_description(cell))),
    }
}

fn money_value(cell: &Cell, field: &'static str, currency: CurrencyCode) -> Result<i64, Rejection> {
    to_minor_units(decimal_value(cell, field)?, currency, field)
}

fn optional_money(
    row: &[Cell],
    column: Option<usize>,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<Option<i64>, Rejection> {
    let Some(column) = column else {
        return Ok(None);
    };
    let value = cell(row, column);
    if value.is_empty() || value.text().is_some_and(|text| text.trim().is_empty()) {
        Ok(None)
    } else {
        money_value(value, field, currency).map(Some)
    }
}

fn quantity_value(cell: &Cell, field: &'static str) -> Result<Dec, Rejection> {
    decimal_value(cell, field).map(Dec::new)
}

fn text_value(cell: &Cell) -> Result<&str, Rejection> {
    cell.text()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| rejection("value", "non-empty text", &cell_description(cell)))
}

fn optional_text(cell: &Cell) -> Option<&str> {
    cell.text().filter(|value| !value.trim().is_empty())
}

fn account_value(
    directory: &Directory,
    cell: &Cell,
    field: &'static str,
) -> Result<iaam_core::ids::AccountId, Rejection> {
    let name = text_value(cell)?;
    resolve_named_account(name, directory, field)
}

fn instrument_value(
    directory: &Directory,
    cell: &Cell,
    on: Option<Date>,
) -> Result<InstrumentId, Rejection> {
    let name = text_value(cell)?;
    match on {
        Some(on) => resolve_instrument(&directory.instruments, name, on),
        None => resolve_instrument_without_date(&directory.instruments, name),
    }
}

fn custody_value(
    directory: &Directory,
    cell: &Cell,
    field: &'static str,
) -> Result<CustodyId, Rejection> {
    let name = text_value(cell)?;
    resolve_named_custody(name, directory, field)
}
fn operation(
    account: iaam_core::ids::AccountId,
    kind: OperationKind,
    trade_date: Date,
    // Settlement date, not the “money date”: rights transfer precisely upon settlement
    // (dates.rs:34). None means the report has no such column,
    // and the trade date must not be substituted: this is an assertion, not an omission.
    settlement_date: Option<Date>,
    cash_date: Option<Date>,
    source_id: Option<&str>,
) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(trade_date),
            settled: settlement_date,
            cash_posted: cash_date,
            ..OperationDates::default()
        },
        source_time: None,
        idempotency_key: None,
        source_operation_id: source_id.map(str::to_owned),
        source_category: None,
        owner_category: None,
        source_code: None,
        source_kind: None,
        description: None,
    }
}

fn located(sheet: &str, row: u64, result: Result<SubmittedOperation, Rejection>) -> LocatedRow {
    let outcome = match result {
        Ok(operation) => ParsedRow::Operation(Box::new(operation)),
        Err(rejection) => ParsedRow::Rejected(rejection),
    };
    LocatedRow {
        locator: locator(sheet, row),
        outcome,
    }
}

fn locator(sheet: &str, row: u64) -> RowLocator {
    let document = match RawHash::parse(DOCUMENT_HASH) {
        Some(document) => document,
        None => unreachable!("the parser document hash is a valid constant"),
    };
    RowLocator {
        document,
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
        Cell::Empty => "empty".to_owned(),
        Cell::Text(value) | Cell::Error(value) => value.clone(),
        Cell::Number(value) => value.inner().to_string(),
        Cell::Date(value) => value.to_string(),
        Cell::Bool(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::ids::AccountId;
    use time::macros::date;

    #[test]
    fn the_settlement_date_becomes_settled_and_not_cash_posted() {
        // Settlement date and money movement date are different facts: settlement
        // determines the transfer of rights and the reporting period, not account activity.
        let operation = operation(
            AccountId::new_random(),
            OperationKind::Deposit {
                amount_minor: 1,
                currency: CurrencyCode::Rub,
            },
            date!(2026 - 03 - 10),
            Some(date!(2026 - 03 - 12)),
            None,
            None,
        );
        assert_eq!(operation.dates.trade, Some(date!(2026 - 03 - 10)));
        assert_eq!(operation.dates.settled, Some(date!(2026 - 03 - 12)));
        assert_eq!(operation.dates.cash_posted, None);
    }

    #[test]
    fn a_cash_movement_date_becomes_cash_posted_and_not_settled() {
        // Money movement proves the crediting date, but not the trade's
        // settlement date: these are different facts in the journal.
        let operation = operation(
            AccountId::new_random(),
            OperationKind::Withdrawal {
                amount_minor: 1,
                currency: CurrencyCode::Rub,
            },
            date!(2026 - 03 - 10),
            None,
            Some(date!(2026 - 03 - 12)),
            None,
        );

        assert_eq!(operation.dates.cash_posted, Some(date!(2026 - 03 - 12)));
        assert_eq!(operation.dates.settled, None);
    }

    #[test]
    fn a_missing_settlement_column_stays_unknown() {
        // The absence of the column means the settlement date is unknown; the trade date
        // does not prove that settlement occurred on the same day.
        let operation = operation(
            AccountId::new_random(),
            OperationKind::Deposit {
                amount_minor: 1,
                currency: CurrencyCode::Rub,
            },
            date!(2026 - 03 - 10),
            None,
            None,
            None,
        );
        assert_eq!(operation.dates.settled, None);
    }
}
