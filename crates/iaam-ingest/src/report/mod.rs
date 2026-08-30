//! Parsing broker reports (§10.1).
//!
//! The parser is selected **by the contents of the workbook**, not by the file name:
//! the name guarantees nothing, and a Finam report saved under the name
//! of a T-Investments report must be parsed as a Finam report.
//!
//! **A row is the unit of parsing.** An unrecognized row receives its raw input
//! and does not invalidate the document (§10.1); a row outside the perimeter goes
//! to quarantine with a reason and likewise does not invalidate the document (§11).
//!
//! **This code is not reused by the API channel.** A shared function
//! for normalization between report parsing and broker-response parsing
//! would destroy the independence for which the second channel is introduced:
//! a shared error would distort both sides, and reconciliation would not detect it
//! (§10.3). Task 21 puts a barrier against this.

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

/// The broker whose report is being parsed.
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

/// Report file format.
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

/// Why the row was assigned to the uncovered perimeter (§11).
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

/// A report row with the location from which it was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct LocatedRow {
    pub locator: RowLocator,
    pub outcome: ParsedRow,
}

/// A row outside the perimeter.
///
/// The monetary effect of such an operation is stored separately; the economics
/// are not reconstructed, and a discrepancy for this reason is an exception,
/// not “fix it” (§11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    pub locator: RowLocator,
    pub reason: UnsupportedReason,
}

/// Result of parsing a single report.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedReport {
    /// The interval described by the report. `None` means the period is not
    /// named in the document; it must not be inferred from operation dates, as
    /// that would be a guess about completeness (§10.3).
    pub period: Option<AssertionPeriod>,
    pub rows: Vec<LocatedRow>,
    pub sections: ControlSections,
    pub unsupported: Vec<Quarantined>,
}

impl ParsedReport {
    /// An empty result. There is no logic here, hence the name `empty`,
    /// rather than `new`: the constructor hides no checks.
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

/// Report parser contract.
pub trait ReportParser {
    fn broker(&self) -> Broker;
    fn format(&self) -> ReportFormat;
    /// Parsing version. Part of the contract, not an implementation detail: without it, one cannot
    /// distinguish a source error from a parsing error fixed
    /// later (§4.1). It is included in each row's provenance.
    fn version(&self) -> ParserVersion;
    /// Whether the parser recognizes this workbook. It examines the contents: sheet
    /// names and header anchor cells.
    fn recognises(&self, workbook: &Workbook) -> bool;
    fn parse(&self, workbook: &Workbook, directory: &Directory) -> ParsedReport;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DetectError {
    #[error("the workbook was not recognized by any parser")]
    Unrecognised,
    #[error("the workbook was recognized by two parsers: {} and {}", first.code(), second.code())]
    Ambiguous { first: Broker, second: Broker },
}

/// Parser registry.
#[derive(Default)]
pub struct ParserRegistry {
    parsers: Vec<Box<dyn ReportParser>>,
}

impl ParserRegistry {
    /// Built-in parsers.
    ///
    /// Empty until the parsers are written (tasks 15 and 16). An empty registry
    /// refuses to recognize anything—it is more honest than a registry
    /// pretending that it can read reports.
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

    /// Parser for this workbook.
    ///
    /// Two matches are an error, not the first winner: two parsers for
    /// one file mean the recognition criterion is too weak, and silently
    /// choosing either would record facts from the wrong parser.
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
