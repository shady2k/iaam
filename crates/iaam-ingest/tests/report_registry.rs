//! Report parser registry and control sections (§10.1, §10.3).

use iaam_core::event::provenance::{ParserVersion, RawHash, RowLocator};
use iaam_core::ids::{CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use iaam_ingest::Rejection;
use iaam_ingest::csv_source::{Directory, ParsedRow};
use iaam_ingest::report::sections::{
    CashSection, ControlSections, PositionSection, TotalSection, TurnoverSection,
};
use iaam_ingest::report::workbook::{Cell, Sheet, Workbook, WorkbookError};
use iaam_ingest::report::{
    Broker, DetectError, LocatedRow, ParsedReport, ParserRegistry, Quarantined, ReportFormat,
    ReportParser, UnsupportedReason,
};
use rust_decimal::Decimal;
use time::macros::date;

const MINIMAL_WORKBOOK: &[u8] = include_bytes!("fixtures/minimal_workbook.xlsx");

fn text_sheet(name: &str, rows: &[&[&str]]) -> Sheet {
    Sheet {
        name: name.to_owned(),
        rows: rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|value| Cell::Text((*value).to_owned()))
                    .collect()
            })
            .collect(),
    }
}

/// A parser that identifies a workbook by sheet name. The real ones arrive
/// in tasks 15 and 16; this only needs to identify and report.
struct MarkerParser {
    broker: Broker,
    marker: &'static str,
    version: &'static str,
}

impl ReportParser for MarkerParser {
    fn broker(&self) -> Broker {
        self.broker
    }

    fn format(&self) -> ReportFormat {
        ReportFormat::Xlsx
    }

    fn version(&self) -> ParserVersion {
        ParserVersion(self.version.to_owned())
    }

    fn recognises(&self, workbook: &Workbook) -> bool {
        workbook.sheet(self.marker).is_some()
    }

    fn parse(&self, _workbook: &Workbook, _directory: &Directory) -> ParsedReport {
        ParsedReport::empty()
    }
}

fn registry(parsers: Vec<Box<dyn ReportParser>>) -> ParserRegistry {
    ParserRegistry::of(parsers)
}

fn tinkoff_like() -> Box<dyn ReportParser> {
    Box::new(MarkerParser {
        broker: Broker::Tinkoff,
        marker: "Сделки",
        version: "tinkoff-xlsx/1",
    })
}

fn finam_like() -> Box<dyn ReportParser> {
    Box::new(MarkerParser {
        broker: Broker::Finam,
        marker: "Операции",
        version: "finam-xlsx/1",
    })
}

#[test]
fn a_workbook_is_recognised_by_what_is_inside_it() {
    // The filename is not part of identification at all: `detect` does not have it.
    let book = Workbook::of(vec![text_sheet("Сделки", &[&["Дата", "Тип"]])]);
    let registry = registry(vec![tinkoff_like(), finam_like()]);

    let parser = registry.detect(&book).unwrap();
    assert_eq!(parser.broker(), Broker::Tinkoff);
}

#[test]
fn an_unrecognised_workbook_is_an_error_not_a_guess() {
    let book = Workbook::of(vec![text_sheet("Лист1", &[&["что-то"]])]);
    let registry = registry(vec![tinkoff_like(), finam_like()]);

    // `map` before erasure: the error can only be compared on a result
    // that can be printed; a trait object cannot be printed.
    assert_eq!(
        registry
            .detect(&book)
            .map(ReportParser::broker)
            .unwrap_err(),
        DetectError::Unrecognised
    );
}

#[test]
fn two_parsers_claiming_one_workbook_is_an_error() {
    // Two parsers for one file mean that the identification criterion is too
    // weak. Choosing any one would record facts using another parser.
    let book = Workbook::of(vec![
        text_sheet("Сделки", &[&["Дата"]]),
        text_sheet("Операции", &[&["Дата"]]),
    ]);
    let registry = registry(vec![tinkoff_like(), finam_like()]);

    assert_eq!(
        registry
            .detect(&book)
            .map(ReportParser::broker)
            .unwrap_err(),
        DetectError::Ambiguous {
            first: Broker::Tinkoff,
            second: Broker::Finam,
        }
    );
}

#[test]
fn the_builtin_registry_recognises_nothing_until_parsers_arrive() {
    // More honest than an empty registry is a registry with parsers. The parsers
    // arrive in tasks 15 and 16; until then identification fails,
    // rather than choosing at random.
    let book = Workbook::of(vec![text_sheet("Сделки", &[&["Дата"]])]);
    assert_eq!(
        ParserRegistry::builtin()
            .detect(&book)
            .map(ReportParser::broker)
            .unwrap_err(),
        DetectError::Unrecognised
    );
}

#[test]
fn a_real_xlsx_opens_into_sheets_and_cells() {
    let book = Workbook::open(MINIMAL_WORKBOOK).unwrap();

    assert_eq!(book.sheet_names(), vec!["Сделки"]);
    let sheet = book.sheet("Сделки").unwrap();
    assert_eq!(sheet.cell(0, 1).text(), Some("Тип"));
    assert_eq!(
        sheet.cell(1, 2).number(),
        Some(Dec::new(Decimal::new(123_456, 2))),
        "the number is read as decimal, not as a binary floating-point value"
    );
    assert!(
        sheet.cell(9, 9).is_empty(),
        "cells beyond the sheet boundary are empty"
    );
}

#[test]
fn a_date_cell_arrives_as_a_date_and_not_as_a_number() {
    // In XLSX, a date is a number with a style. Without reading the style, 46154 would remain
    // a number, and the parser would interpret it as an amount.
    let book = Workbook::open(MINIMAL_WORKBOOK).unwrap();
    let sheet = book.sheet("Сделки").unwrap();

    assert_eq!(sheet.cell(1, 0).date(), Some(date!(2026 - 05 - 12)));
}

#[test]
fn an_unreadable_stream_is_a_typed_error() {
    // It is not the book that is rejected with a reason, rather than a panic midway through import.
    assert!(matches!(
        Workbook::open(b"\x00\x01 not a workbook"),
        Err(WorkbookError::Unreadable { .. })
    ));
}

#[test]
fn an_unparsed_row_does_not_cancel_the_document() {
    // §10.1: a row that could not be understood receives an issue and remains
    // a row—the document continues to be parsed.
    let mut report = ParsedReport::empty();
    report.rows.push(LocatedRow {
        locator: locator("Сделки", 12),
        outcome: ParsedRow::Rejected(Rejection {
            field: "type".into(),
            expected: "known operation kind".into(),
            actual: "unrecognized word".into(),
        }),
    });

    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].locator.row, 12);
    assert_eq!(report.rows[0].locator.sheet.as_deref(), Some("Сделки"));
}

#[test]
fn a_quarantined_row_does_not_cancel_the_document() {
    // §11: an operation outside the scope is sent to quarantine with a reason, rather than
    // invalidating the report and fabricating its economics.
    let mut report = ParsedReport::empty();
    report.unsupported.push(Quarantined {
        locator: locator("Сделки", 13),
        reason: UnsupportedReason::Repo,
    });

    assert!(report.rows.is_empty());
    assert_eq!(report.unsupported[0].reason, UnsupportedReason::Repo);
    assert_eq!(report.unsupported[0].locator.row, 13);
}

fn locator(sheet: &str, row: u64) -> RowLocator {
    RowLocator {
        document: RawHash::parse(&"4".repeat(64)).unwrap(),
        sheet: Some(sheet.to_owned()),
        row,
    }
}

#[test]
fn an_absent_control_section_never_becomes_a_zero() {
    // A section that is absent from the document does not exist. Zero here is
    // the source's assertion that there were no fees (§4.9).
    let sections = ControlSections::default();
    assert_eq!(sections.claims(), vec![]);
}

#[test]
fn present_control_sections_become_claims_of_the_right_dimension() {
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let sections = ControlSections {
        cash_balances: vec![CashSection {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(-12_500),
            at: BalancePoint::Closing,
        }],
        turnovers: vec![TurnoverSection {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(100_000),
            credit: PostedMinor::new(87_500),
        }],
        fees: Some(TotalSection {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(350),
        }),
        income: None,
        tax_withheld: Some(TotalSection {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1_300),
        }),
        positions: vec![PositionSection {
            instrument,
            custody,
            quantity: Quantity(Dec::new(Decimal::from(10))),
            at: BalancePoint::Opening,
        }],
    };

    let claims = sections.claims();
    assert_eq!(
        claims.len(),
        5,
        "sections without data do not turn into zero"
    );
    assert!(claims.contains(&ControlClaim::CashBalance {
        currency: CurrencyCode::Rub,
        amount: PostedMinor::new(-12_500),
        at: BalancePoint::Closing,
    }));
    assert!(claims.contains(&ControlClaim::FeesTotal {
        currency: CurrencyCode::Rub,
        amount: PostedMinor::new(350),
    }));
    let dimensions: Vec<Dimension> = claims.iter().map(ControlClaim::dimension).collect();
    assert!(dimensions.contains(&Dimension::Positions));
    assert!(dimensions.contains(&Dimension::TaxBasis));
    assert!(
        !dimensions.contains(&Dimension::Income),
        "there was no income section"
    );
}

#[test]
fn each_parser_carries_its_own_version() {
    // The parser version is part of its contract: without it, it is impossible to distinguish
    // a source error from a parsing error fixed later.
    assert_eq!(
        tinkoff_like().version(),
        ParserVersion("tinkoff-xlsx/1".to_owned())
    );
    assert_ne!(tinkoff_like().version(), finam_like().version());
}
