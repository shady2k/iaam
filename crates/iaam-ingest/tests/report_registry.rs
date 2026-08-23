//! Реестр парсеров отчётов и контрольные секции (§10.1, §10.3).

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

/// Парсер, опознающий книгу по имени листа. Настоящие приходят
/// задачами 15 и 16; этому достаточно уметь опознавать и отчитываться.
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
    // Имя файла в опознание не входит вовсе: у `detect` его нет.
    let book = Workbook::of(vec![text_sheet("Сделки", &[&["Дата", "Тип"]])]);
    let registry = registry(vec![tinkoff_like(), finam_like()]);

    let parser = registry.detect(&book).unwrap();
    assert_eq!(parser.broker(), Broker::Tinkoff);
}

#[test]
fn an_unrecognised_workbook_is_an_error_not_a_guess() {
    let book = Workbook::of(vec![text_sheet("Лист1", &[&["что-то"]])]);
    let registry = registry(vec![tinkoff_like(), finam_like()]);

    // `map` до брокера: сравнивать ошибку можно только у результата,
    // который умеет печататься, а трейт-объект печататься не умеет.
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
    // Два парсера на один файл означают, что признак опознания слишком
    // слаб. Взять любой значит записать факты чужим разбором.
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
    // Честнее пустого реестра только реестр с парсерами. Парсеры
    // приходят задачами 15 и 16; до тех пор опознание отказывает,
    // а не выбирает наугад.
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
        "число читается десятичным, а не двоичной плавающей точкой"
    );
    assert!(sheet.cell(9, 9).is_empty(), "ячейки за краем листа пусты");
}

#[test]
fn a_date_cell_arrives_as_a_date_and_not_as_a_number() {
    // В XLSX дата — число со стилем. Без чтения стиля 46154 осталось бы
    // числом, и парсер разобрал бы его как сумму.
    let book = Workbook::open(MINIMAL_WORKBOOK).unwrap();
    let sheet = book.sheet("Сделки").unwrap();

    assert_eq!(sheet.cell(1, 0).date(), Some(date!(2026 - 05 - 12)));
}

#[test]
fn an_unreadable_stream_is_a_typed_error() {
    // Не книга — это отказ с причиной, а не паника посреди импорта.
    assert!(matches!(
        Workbook::open(b"\x00\x01 not a workbook"),
        Err(WorkbookError::Unreadable { .. })
    ));
}

#[test]
fn an_unparsed_row_does_not_cancel_the_document() {
    // §10.1: строка, которую не поняли, получает исход и остаётся
    // строкой — документ продолжает разбираться.
    let mut report = ParsedReport::empty();
    report.rows.push(LocatedRow {
        locator: locator("Сделки", 12),
        outcome: ParsedRow::Rejected(Rejection {
            field: "type".into(),
            expected: "известный вид операции".into(),
            actual: "непонятное слово".into(),
        }),
    });

    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].locator.row, 12);
    assert_eq!(report.rows[0].locator.sheet.as_deref(), Some("Сделки"));
}

#[test]
fn a_quarantined_row_does_not_cancel_the_document() {
    // §11: операция вне периметра уходит в карантин с причиной, а не
    // отменяет отчёт и не достраивает экономику.
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
    // Секции, которой в документе нет, не существует. Ноль здесь —
    // утверждение источника о том, что комиссий не было (§4.9).
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
    assert_eq!(claims.len(), 5, "секции без данных не превращаются в ноль");
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
        "секции дохода не было"
    );
}

#[test]
fn each_parser_carries_its_own_version() {
    // Версия парсера — часть его контракта: без неё нельзя отличить
    // ошибку источника от ошибки разбора, исправленной позже.
    assert_eq!(
        tinkoff_like().version(),
        ParserVersion("tinkoff-xlsx/1".to_owned())
    );
    assert_ne!(tinkoff_like().version(), finam_like().version());
}
