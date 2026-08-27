//! Приёмка синтетического отчёта Т-Инвестиций (§10.1, §10.3, §11).
//!
//! Ожидаемые числа ниже выписаны вручную из `tinkoff-synthetic.xlsx`.
//! Тест намеренно не получает эталон из вывода парсера (§15.5).

use iaam_core::event::kind::{FeeOrigin, TradeSide};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::instrument::AliasInterval;
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_ingest::csv_source::{Directory, ParsedRow};
use iaam_ingest::operation::OperationKind;
use iaam_ingest::report::ReportParser;
use iaam_ingest::report::sections::ControlSections;
use iaam_ingest::report::tinkoff::TinkoffParser;
use iaam_ingest::report::workbook::Workbook;
use rust_decimal::Decimal;
use time::macros::date;

const REPORT: &[u8] = include_bytes!("../../../tests/fixtures/reports/tinkoff-synthetic.xlsx");
const EXPECTED_ROW_OUTCOMES: usize = 8;
const EXPECTED_OPERATIONS: usize = 7;
const EXPECTED_REJECTIONS: usize = 1;
const EXPECTED_DEPOSIT_MINOR: i64 = 5_000_000;
const EXPECTED_WITHDRAWAL_MINOR: i64 = 700_000;
const EXPECTED_BUY_GROSS_MINOR: i64 = 1_000_000;
const EXPECTED_BUY_FEE_MINOR: i64 = 1_000;
const EXPECTED_BUY_ACCRUED_MINOR: i64 = 12_050;
const EXPECTED_SELL_GROSS_MINOR: i64 = 1_000_000;
const EXPECTED_SELL_FEE_MINOR: i64 = 1_500;
const EXPECTED_FEE_MINOR: i64 = 2_500;
const EXPECTED_COUPON_MINOR: i64 = 45_000;
const EXPECTED_DIVIDEND_MINOR: i64 = 30_000;
const EXPECTED_CASH_OPENING_MINOR: i64 = 1_200_000;
const EXPECTED_CASH_CLOSING_MINOR: i64 = 5_557_950;
const EXPECTED_TURNOVER_DEBIT_MINOR: i64 = 1_715_550;
const EXPECTED_TURNOVER_CREDIT_MINOR: i64 = 6_073_500;
const EXPECTED_POSITION_CLOSING: i64 = 5;
const EXPECTED_FEES_TOTAL_MINOR: i64 = 2_500;
const EXPECTED_INCOME_TOTAL_MINOR: i64 = 75_000;
const EXPECTED_TAX_TOTAL_MINOR: i64 = 7_500;

fn directory(account: AccountId, custody: CustodyId, instrument: InstrumentId) -> Directory {
    Directory {
        accounts: [("INVEST-001".to_owned(), account)].into_iter().collect(),
        custodies: [("НРД".to_owned(), vec![custody])].into_iter().collect(),
        instruments: [(
            "BOND-X".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(1900 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        )]
        .into_iter()
        .collect(),
        default_custody: None,
    }
}

#[test]
fn tinkoff_ticker_column_does_not_resolve_through_broker_code() {
    let workbook = Workbook::open(REPORT).unwrap();
    let account = AccountId::new_random();
    let custody = CustodyId::new_random();
    let ticker_instrument = InstrumentId::new_random();
    let broker_instrument = InstrumentId::new_random();
    let mut directory = directory(account, custody, ticker_instrument);
    directory.instruments.get_mut("BOND-X").unwrap().push((
        "broker_code".to_owned(),
        AliasInterval {
            valid_from: date!(1900 - 01 - 01),
            valid_to: None,
        },
        broker_instrument,
    ));

    let report = TinkoffParser.parse(&workbook, &directory);
    let instruments: Vec<_> = report
        .rows
        .iter()
        .filter_map(|row| match &row.outcome {
            ParsedRow::Operation(operation) => match &operation.kind {
                OperationKind::Buy { instrument, .. } | OperationKind::Sell { instrument, .. } => {
                    Some(instrument)
                }
                _ => None,
            },
            ParsedRow::Rejected(_) => None,
        })
        .collect();

    assert_eq!(instruments, vec![&ticker_instrument, &ticker_instrument]);
}

fn directory_with_historical_instrument(
    account: AccountId,
    custody: CustodyId,
    first: InstrumentId,
    second: InstrumentId,
) -> Directory {
    Directory {
        accounts: [("INVEST-001".to_owned(), account)].into_iter().collect(),
        custodies: [("НРД".to_owned(), vec![custody])].into_iter().collect(),
        instruments: [(
            "BOND-X".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(1900 - 01 - 01),
                        valid_to: Some(date!(2026 - 03 - 01)),
                    },
                    first,
                ),
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2026 - 03 - 01),
                        valid_to: None,
                    },
                    second,
                ),
            ],
        )]
        .into_iter()
        .collect(),
        default_custody: None,
    }
}

#[test]
fn tinkoff_report_resolves_historical_instrument_on_each_report_date() {
    let workbook = Workbook::open(REPORT).unwrap();
    let parser = TinkoffParser;
    let account = AccountId::new_random();
    let custody = CustodyId::new_random();
    let first = InstrumentId::new_random();
    let second = InstrumentId::new_random();
    let report = parser.parse(
        &workbook,
        &directory_with_historical_instrument(account, custody, first, second),
    );

    let mut instrument_rows = 0;
    for row in &report.rows {
        let ParsedRow::Operation(operation) = &row.outcome else {
            continue;
        };
        match &operation.kind {
            OperationKind::Buy { instrument, .. } | OperationKind::Sell { instrument, .. } => {
                assert_eq!(*instrument, second);
                instrument_rows += 1;
            }
            OperationKind::Income {
                instrument: Some(instrument),
                ..
            } => {
                assert_eq!(*instrument, second);
                instrument_rows += 1;
            }
            _ => {}
        }
    }
    assert_eq!(instrument_rows, 3);
    assert!(!report.sections.positions.is_empty());
    assert!(
        report
            .sections
            .positions
            .iter()
            .all(|position| position.instrument == second)
    );
}

fn dec(value: i64) -> Dec {
    Dec::new(Decimal::from(value))
}

#[test]
fn tinkoff_fixture_is_recognised_by_contents_and_has_its_own_version() {
    let workbook = Workbook::open(REPORT).unwrap();
    let parser = TinkoffParser;

    assert!(parser.recognises(&workbook));
    assert_eq!(parser.version(), ParserVersion("tinkoff-xlsx/1".to_owned()));
}

#[test]
fn tinkoff_report_preserves_rows_operations_period_controls_and_repo_quarantine() {
    let workbook = Workbook::open(REPORT).unwrap();
    let parser = TinkoffParser;
    let account = AccountId::new_random();
    let custody = CustodyId::new_random();
    let instrument = InstrumentId::new_random();
    let report = parser.parse(&workbook, &directory(account, custody, instrument));

    assert_eq!(
        report.period,
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
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
                        dec(10),
                        EXPECTED_BUY_GROSS_MINOR,
                        Some(EXPECTED_BUY_FEE_MINOR),
                        Some(EXPECTED_BUY_ACCRUED_MINOR),
                        CurrencyCode::Rub,
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
                        dec(5),
                        EXPECTED_SELL_GROSS_MINOR,
                        Some(EXPECTED_SELL_FEE_MINOR),
                        None,
                        CurrencyCode::Rub,
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
                kind,
            } => {
                assert_eq!(*currency, CurrencyCode::Rub);
                // Лист «Купоны и дивиденды» смешивает оба вида и вида
                // построчно не называет: «не утверждалось» здесь честно,
                // а угадывание по типу бумаги — нет.
                assert_eq!(*kind, None);
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
            quantity: Quantity(dec(0)),
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
    assert_eq!(report.sections.claims().len(), 8);

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
fn control_sections_keep_absent_claims_absent() {
    let sections = ControlSections::default();
    assert!(sections.claims().is_empty());
}

#[test]
fn trade_rows_keep_their_side_in_the_operation_kind() {
    let workbook = Workbook::open(REPORT).unwrap();
    let parser = TinkoffParser;
    let report = parser.parse(
        &workbook,
        &directory(
            AccountId::new_random(),
            CustodyId::new_random(),
            InstrumentId::new_random(),
        ),
    );

    let sides: Vec<_> = report
        .rows
        .iter()
        .filter_map(|row| match &row.outcome {
            ParsedRow::Operation(operation) => match operation.kind {
                OperationKind::Buy { .. } => Some(TradeSide::Buy),
                OperationKind::Sell { .. } => Some(TradeSide::Sell),
                _ => None,
            },
            ParsedRow::Rejected(_) => None,
        })
        .collect();
    assert_eq!(sides, vec![TradeSide::Buy, TradeSide::Sell]);
}
