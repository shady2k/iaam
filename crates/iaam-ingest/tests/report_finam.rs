//! Приёмка синтетического отчёта Финама.
//!
//! Ожидаемые числа ниже выписаны вручную из `finam-synthetic.xls`.
//! Тест намеренно не получает эталон из вывода парсера (§15.5).

use iaam_core::event::kind::FeeOrigin;
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_ingest::csv_source::{Directory, ParsedRow};
use iaam_ingest::operation::OperationKind;
use iaam_ingest::report::finam::FinamParser;
use iaam_ingest::report::sections::ControlSections;
use iaam_ingest::report::tinkoff::TinkoffParser;
use iaam_ingest::report::workbook::{Cell, Sheet, Workbook};
use iaam_ingest::report::{ReportFormat, ReportParser};
use rust_decimal::Decimal;
use time::macros::date;

const REPORT: &[u8] = include_bytes!("../../../tests/fixtures/reports/finam-synthetic.xls");
const EXPECTED_ROW_OUTCOMES: usize = 8;
const EXPECTED_OPERATIONS: usize = 7;
const EXPECTED_REJECTIONS: usize = 1;
const EXPECTED_DEPOSIT_MINOR: i64 = 6_000_000;
const EXPECTED_WITHDRAWAL_MINOR: i64 = 800_000;
const EXPECTED_BUY_GROSS_MINOR: i64 = 2_400_000;
const EXPECTED_BUY_FEE_MINOR: i64 = 2_400;
const EXPECTED_BUY_ACCRUED_MINOR: i64 = 15_000;
const EXPECTED_SELL_GROSS_MINOR: i64 = 1_750_000;
const EXPECTED_SELL_FEE_MINOR: i64 = 1_750;
const EXPECTED_FEE_MINOR: i64 = 3_000;
const EXPECTED_COUPON_MINOR: i64 = 50_000;
const EXPECTED_DIVIDEND_MINOR: i64 = 25_000;
const EXPECTED_CASH_OPENING_MINOR: i64 = 1_000_000;
const EXPECTED_CASH_CLOSING_MINOR: i64 = 5_619_600;
const EXPECTED_TURNOVER_DEBIT_MINOR: i64 = 3_205_400;
const EXPECTED_TURNOVER_CREDIT_MINOR: i64 = 7_825_000;
const EXPECTED_POSITION_OPENING: i64 = 3;
const EXPECTED_POSITION_CLOSING: i64 = 8;
const EXPECTED_FEES_TOTAL_MINOR: i64 = 71_500;
const EXPECTED_INCOME_TOTAL_MINOR: i64 = 75_000;
const EXPECTED_TAX_TOTAL_MINOR: i64 = 10_000;

fn directory(account: AccountId, custody: CustodyId, instrument: InstrumentId) -> Directory {
    Directory {
        accounts: [("INVEST-001".to_owned(), account)].into_iter().collect(),
        custodies: [("НРД".to_owned(), custody)].into_iter().collect(),
        instruments: [("FIN-BOND".to_owned(), instrument)].into_iter().collect(),
        default_custody: None,
    }
}

fn dec(value: i64) -> Dec {
    Dec::new(Decimal::from(value))
}

#[test]
fn finam_fixture_is_recognised_by_contents_and_has_its_own_version() {
    let workbook = Workbook::open(REPORT).unwrap();
    let parser = FinamParser;

    assert!(parser.recognises(&workbook));
    assert_eq!(parser.broker().code(), "finam");
    assert_eq!(parser.format(), ReportFormat::Xls);
    assert_eq!(parser.version(), ParserVersion("finam-xls/1".to_owned()));
}

#[test]
fn report_formats_reject_each_other() {
    let finam = Workbook::open(REPORT).unwrap();
    let tinkoff = Workbook::open(include_bytes!(
        "../../../tests/fixtures/reports/tinkoff-synthetic.xlsx"
    ))
    .unwrap();

    assert!(!FinamParser.recognises(&tinkoff));
    assert!(!TinkoffParser.recognises(&finam));
}

#[test]
fn finam_report_preserves_rows_operations_period_controls_and_repo_quarantine() {
    let workbook = Workbook::open(REPORT).unwrap();
    let parser = FinamParser;
    let account = AccountId::new_random();
    let custody = CustodyId::new_random();
    let instrument = InstrumentId::new_random();
    let report = parser.parse(&workbook, &directory(account, custody, instrument));

    assert_eq!(
        report.period,
        AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30))
    );
    assert_eq!(report.rows.len(), EXPECTED_ROW_OUTCOMES);
    assert_eq!(
        report
            .rows
            .iter()
            .filter(|row| matches!(row.outcome, ParsedRow::Operation(_)))
            .count(),
        EXPECTED_OPERATIONS
    );
    assert_eq!(
        report
            .rows
            .iter()
            .filter(|row| matches!(row.outcome, ParsedRow::Rejected(_)))
            .count(),
        EXPECTED_REJECTIONS
    );

    let mut deposits = 0;
    let mut withdrawals = 0;
    let mut buys = 0;
    let mut sells = 0;
    let mut fees = 0;
    let mut incomes = Vec::new();
    for row in &report.rows {
        let ParsedRow::Operation(operation) = &row.outcome else {
            continue;
        };
        match &operation.kind {
            OperationKind::Deposit {
                amount_minor,
                currency,
            } => {
                assert_eq!(
                    (*amount_minor, *currency),
                    (EXPECTED_DEPOSIT_MINOR, CurrencyCode::Rub)
                );
                deposits += 1;
            }
            OperationKind::Withdrawal {
                amount_minor,
                currency,
            } => {
                assert_eq!(
                    (*amount_minor, *currency),
                    (EXPECTED_WITHDRAWAL_MINOR, CurrencyCode::Rub)
                );
                withdrawals += 1;
            }
            OperationKind::Buy {
                instrument: actual_instrument,
                custody: actual_custody,
                quantity,
                gross_minor,
                fee_minor,
                accrued_interest_minor,
                currency,
            } => {
                assert_eq!((*actual_instrument, *actual_custody), (instrument, custody));
                assert_eq!(
                    (
                        *quantity,
                        *gross_minor,
                        *fee_minor,
                        *accrued_interest_minor,
                        *currency
                    ),
                    (
                        dec(12),
                        EXPECTED_BUY_GROSS_MINOR,
                        Some(EXPECTED_BUY_FEE_MINOR),
                        Some(EXPECTED_BUY_ACCRUED_MINOR),
                        CurrencyCode::Rub
                    )
                );
                buys += 1;
            }
            OperationKind::Sell {
                instrument: actual_instrument,
                custody: actual_custody,
                quantity,
                gross_minor,
                fee_minor,
                accrued_interest_minor,
                currency,
            } => {
                assert_eq!((*actual_instrument, *actual_custody), (instrument, custody));
                assert_eq!(
                    (
                        *quantity,
                        *gross_minor,
                        *fee_minor,
                        *accrued_interest_minor,
                        *currency
                    ),
                    (
                        dec(7),
                        EXPECTED_SELL_GROSS_MINOR,
                        Some(EXPECTED_SELL_FEE_MINOR),
                        None,
                        CurrencyCode::Rub
                    )
                );
                sells += 1;
            }
            OperationKind::Fee {
                amount_minor,
                currency,
                origin,
            } => {
                assert_eq!(
                    (*amount_minor, *currency, *origin),
                    (EXPECTED_FEE_MINOR, CurrencyCode::Rub, FeeOrigin::Other)
                );
                fees += 1;
            }
            OperationKind::Income {
                instrument: actual_instrument,
                gross_minor,
                currency,
            } => {
                assert_eq!(*currency, CurrencyCode::Rub);
                incomes.push((*actual_instrument, *gross_minor));
            }
            other => panic!("неожиданный вид операции: {other:?}"),
        }
    }
    assert_eq!((deposits, withdrawals, buys, sells, fees), (1, 1, 1, 1, 1));
    assert_eq!(
        incomes,
        vec![
            (Some(instrument), EXPECTED_COUPON_MINOR),
            (None, EXPECTED_DIVIDEND_MINOR)
        ]
    );

    let expected_claims = vec![
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(EXPECTED_CASH_OPENING_MINOR),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(EXPECTED_CASH_CLOSING_MINOR),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(EXPECTED_TURNOVER_DEBIT_MINOR),
            credit: PostedMinor::new(EXPECTED_TURNOVER_CREDIT_MINOR),
        },
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity: Quantity(dec(EXPECTED_POSITION_OPENING)),
            at: BalancePoint::Opening,
        },
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity: Quantity(dec(EXPECTED_POSITION_CLOSING)),
            at: BalancePoint::Closing,
        },
        ControlClaim::FeesTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(EXPECTED_FEES_TOTAL_MINOR),
        },
        ControlClaim::IncomeTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(EXPECTED_INCOME_TOTAL_MINOR),
        },
        ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(EXPECTED_TAX_TOTAL_MINOR),
        },
    ];
    assert_eq!(report.sections.claims(), expected_claims);

    assert_eq!(report.unsupported.len(), 1);
    assert_eq!(
        report.unsupported[0].reason,
        iaam_ingest::report::UnsupportedReason::Repo
    );
    assert_eq!(report.unsupported[0].locator.sheet.as_deref(), Some("РЕПО"));
    assert_eq!(report.unsupported[0].locator.row, 2);
    assert!(
        report
            .rows
            .iter()
            .all(|row| row.locator.sheet.as_deref() != Some("РЕПО"))
    );
}

#[test]
fn non_representable_money_is_rejected_without_rounding() {
    let workbook = Workbook::of(vec![
        Sheet {
            name: "Сведения".to_owned(),
            rows: vec![vec![
                Cell::Text("ОТЧЕТ БРОКЕРА ФИНАМ".to_owned()),
                Cell::Empty,
                Cell::Empty,
            ]],
        },
        Sheet {
            name: "Сделки".to_owned(),
            rows: vec![
                vec![
                    Cell::Text("Дата сделки".to_owned()),
                    Cell::Text("Дата расчетов".to_owned()),
                    Cell::Text("Код договора".to_owned()),
                    Cell::Text("Код инструмента".to_owned()),
                    Cell::Text("Направление".to_owned()),
                    Cell::Text("Объем".to_owned()),
                    Cell::Text("Сумма сделки".to_owned()),
                    Cell::Text("Комиссия".to_owned()),
                    Cell::Text("НКД".to_owned()),
                    Cell::Text("Валюта расчетов".to_owned()),
                    Cell::Text("Место учета".to_owned()),
                    Cell::Text("Идентификатор сделки".to_owned()),
                ],
                vec![
                    Cell::Text("2026-04-03".to_owned()),
                    Cell::Text("2026-04-06".to_owned()),
                    Cell::Text("INVEST-001".to_owned()),
                    Cell::Text("FIN-BOND".to_owned()),
                    Cell::Text("Купля".to_owned()),
                    Cell::Number(dec(1)),
                    Cell::Number(dec(100)),
                    Cell::Number(Dec::new(Decimal::new(1, 3))),
                    Cell::Empty,
                    Cell::Text("RUB".to_owned()),
                    Cell::Text("НРД".to_owned()),
                    Cell::Text("bad-money".to_owned()),
                ],
            ],
        },
    ]);
    let account = AccountId::new_random();
    let custody = CustodyId::new_random();
    let instrument = InstrumentId::new_random();
    let report = FinamParser.parse(&workbook, &directory(account, custody, instrument));

    assert!(matches!(report.rows[0].outcome, ParsedRow::Rejected(_)));
}

#[test]
fn absent_control_sections_remain_absent() {
    let sections = ControlSections::default();
    assert!(sections.claims().is_empty());
}
