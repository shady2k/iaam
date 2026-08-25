//! Разбор брокерских отчётов (§10.1).
//!
//! Парсер выбирается **по содержимому книги**, а не по имени файла:
//! имя не гарантирует ничего, и отчёт Финама, сохранённый под именем
//! отчёта Т-Инвестиций, обязан разбираться как отчёт Финама.
//!
//! **Строка — единица разбора.** Непонятая строка получает исход
//! и не отменяет документ (§10.1); строка вне периметра уходит
//! в карантин с причиной и тоже не отменяет документ (§11).
//!
//! **Этот код не переиспользуется каналом API.** Общая функция
//! нормализации между разбором отчёта и разбором ответа брокера
//! уничтожила бы независимость, ради которой второй канал заводится:
//! общая ошибка исказила бы обе стороны, и сверка её не заметила бы
//! (§10.3). Заслон на это ставится задачей 21.

pub mod finam;
pub mod sections;
pub mod tinkoff;
pub mod workbook;

use iaam_core::event::provenance::{ParserVersion, RowLocator};
use iaam_core::reconciliation::claim::AssertionPeriod;
use thiserror::Error;

use crate::csv_source::{Directory, ParsedRow};
use sections::ControlSections;
use workbook::Workbook;

/// Брокер, чей отчёт разбирается.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Broker {
    Tinkoff,
    Finam,
}

impl Broker {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Tinkoff => "tinkoff",
            Self::Finam => "finam",
        }
    }
}

/// Формат файла отчёта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReportFormat {
    Xlsx,
    Xls,
}

impl ReportFormat {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Xlsx => "xlsx",
            Self::Xls => "xls",
        }
    }
}

/// Почему строка отнесена к непокрытому периметру (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsupportedReason {
    Repo,
    Margin,
    Derivative,
    Short,
}

impl UnsupportedReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Margin => "margin",
            Self::Derivative => "derivative",
            Self::Short => "short",
        }
    }
}

/// Строка отчёта с местом, откуда она взята.
#[derive(Debug, Clone, PartialEq)]
pub struct LocatedRow {
    pub locator: RowLocator,
    pub outcome: ParsedRow,
}

/// Строка вне периметра.
///
/// Денежный эффект такой операции сохраняется отдельно; экономика
/// не достраивается, и расхождение по этой причине — исключение,
/// а не «почини это» (§11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    pub locator: RowLocator,
    pub reason: UnsupportedReason,
}

/// Результат разбора одного отчёта.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedReport {
    /// Интервал, о котором говорит отчёт. `None` — период в документе
    /// не назван; выводить его из дат операций нельзя, это была бы
    /// догадка о полноте (§10.3).
    pub period: Option<AssertionPeriod>,
    pub rows: Vec<LocatedRow>,
    pub sections: ControlSections,
    pub unsupported: Vec<Quarantined>,
}

impl ParsedReport {
    /// Пустой результат. Логики здесь нет, поэтому имя `empty`,
    /// а не `new`: конструктор не скрывает ни одной проверки.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            period: None,
            rows: Vec::new(),
            sections: ControlSections::default(),
            unsupported: Vec::new(),
        }
    }
}

/// Контракт парсера отчёта.
pub trait ReportParser {
    fn broker(&self) -> Broker;
    fn format(&self) -> ReportFormat;
    /// Версия разбора. Часть контракта, а не деталь: без неё нельзя
    /// отличить ошибку источника от ошибки разбора, исправленной
    /// позже (§4.1). Попадает в provenance каждой строки.
    fn version(&self) -> ParserVersion;
    /// Опознаёт ли парсер эту книгу. Смотрит на содержимое: имена
    /// листов и опорные ячейки заголовка.
    fn recognises(&self, workbook: &Workbook) -> bool;
    fn parse(&self, workbook: &Workbook, directory: &Directory) -> ParsedReport;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DetectError {
    #[error("книга не опознана ни одним парсером")]
    Unrecognised,
    #[error("книгу опознали два парсера: {} и {}", first.code(), second.code())]
    Ambiguous { first: Broker, second: Broker },
}

/// Реестр парсеров.
#[derive(Default)]
pub struct ParserRegistry {
    parsers: Vec<Box<dyn ReportParser>>,
}

impl ParserRegistry {
    /// Встроенные парсеры.
    ///
    /// Пуст, пока парсеры не написаны (задачи 15 и 16). Пустой реестр
    /// отказывает опознать что угодно — это честнее, чем реестр,
    /// делающий вид, что умеет читать отчёты.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            parsers: Vec::new(),
        }
    }

    #[must_use]
    pub const fn of(parsers: Vec<Box<dyn ReportParser>>) -> Self {
        Self { parsers }
    }

    /// Парсер для этой книги.
    ///
    /// Опознали двое — ошибка, а не первый выигравший: два парсера на
    /// один файл означают, что признак опознания слишком слаб, и молча
    /// взять любой значит записать факты чужим разбором.
    pub fn detect(&self, workbook: &Workbook) -> Result<&dyn ReportParser, DetectError> {
        let mut found: Option<&dyn ReportParser> = None;
        for parser in &self.parsers {
            if !parser.recognises(workbook) {
                continue;
            }
            if let Some(first) = found {
                return Err(DetectError::Ambiguous {
                    first: first.broker(),
                    second: parser.broker(),
                });
            }
            found = Some(parser.as_ref());
        }
        found.ok_or(DetectError::Unrecognised)
    }
}
