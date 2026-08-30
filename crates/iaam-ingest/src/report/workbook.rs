//! Report workbook: sheets and cells.
//!
//! This type is self-owned rather than borrowed from `calamine`: parsers and tests
//! do not depend on the reader library's API, the workbook can be built in memory
//! without a file, and replacing the library does not affect any parser.
//!
//! **This is the I/O boundary.** XLSX stores numbers as binary floating-point
//! values, and dates as numbers with a date style. Parsing this
//! belongs in one place so domain values of type `f64` never see it.

use std::io::Cursor;

use calamine::{Data, Reader};
use iaam_core::numeric::decimal::Dec;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use thiserror::Error;
use time::Date;
use time::macros::date;

/// Excel date epoch. December 30, 1899, not December 31:
/// Excel treats 1900 as a leap year, shifting the epoch by one day so that
/// its error is compensated for.
const EXCEL_EPOCH: Date = date!(1899 - 12 - 30);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkbookError {
    #[error("workbook cannot be read: {detail}")]
    Unreadable { detail: String },
    #[error("sheet {sheet} cannot be read: {detail}")]
    UnreadableSheet { sheet: String, detail: String },
}

/// Workbook cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    Empty,
    Text(String),
    Number(Dec),
    Date(Date),
    Bool(bool),
    /// A cell with a calculation error (`#DIV/0!` and similar).
    ///
    /// Kept separate from text intentionally: `#Н/Д` in a text
    /// cell would be mistaken for a label by the parser.
    Error(String),
}

impl Cell {
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            Self::Empty | Self::Number(_) | Self::Date(_) | Self::Bool(_) | Self::Error(_) => None,
        }
    }

    #[must_use]
    pub const fn number(&self) -> Option<Dec> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Empty | Self::Text(_) | Self::Date(_) | Self::Bool(_) | Self::Error(_) => None,
        }
    }

    #[must_use]
    pub const fn date(&self) -> Option<Date> {
        match self {
            Self::Date(value) => Some(*value),
            Self::Empty | Self::Text(_) | Self::Number(_) | Self::Bool(_) | Self::Error(_) => None,
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// Workbook sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub name: String,
    pub rows: Vec<Vec<Cell>>,
}

impl Sheet {
    /// Cell at zero-based row and column indices.
    ///
    /// Beyond the sheet edge, it is empty rather than an error: reports arrive with ragged
    /// rows, and accessing a missing cell in them is normal.
    #[must_use]
    pub fn cell(&self, row: usize, column: usize) -> &Cell {
        self.rows
            .get(row)
            .and_then(|cells| cells.get(column))
            .unwrap_or(&Cell::Empty)
    }

    /// Does the sheet contain a text cell containing the substring.
    /// Case does not matter: report headers are written however people happen to write them.
    #[must_use]
    pub fn contains_text(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.rows.iter().flatten().any(|cell| {
            cell.text()
                .is_some_and(|text| text.to_lowercase().contains(&needle))
        })
    }
}

/// Open workbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workbook {
    sheets: Vec<Sheet>,
}

impl Workbook {
    /// A workbook from ready-made sheets. Needed by tests and parsers assembling
    /// the workbook differently: recognition is checked without binary fixtures.
    #[must_use]
    pub const fn of(sheets: Vec<Sheet>) -> Self {
        Self { sheets }
    }

    /// Reading a workbook from bytes. The format is determined by the contents.
    pub fn open(bytes: &[u8]) -> Result<Self, WorkbookError> {
        let mut book =
            calamine::open_workbook_auto_from_rs(Cursor::new(bytes.to_vec())).map_err(|error| {
                WorkbookError::Unreadable {
                    detail: error.to_string(),
                }
            })?;
        let names = book.sheet_names().to_vec();
        let mut sheets = Vec::with_capacity(names.len());
        for name in names {
            let range =
                book.worksheet_range(&name)
                    .map_err(|error| WorkbookError::UnreadableSheet {
                        sheet: name.clone(),
                        detail: error.to_string(),
                    })?;
            sheets.push(Sheet {
                name,
                rows: range
                    .rows()
                    .map(|row| row.iter().map(convert).collect())
                    .collect(),
            });
        }
        Ok(Self { sheets })
    }

    #[must_use]
    pub fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.sheets.iter().find(|sheet| sheet.name == name)
    }

    #[must_use]
    pub fn sheet_names(&self) -> Vec<&str> {
        self.sheets
            .iter()
            .map(|sheet| sheet.name.as_str())
            .collect()
    }

    #[must_use]
    pub fn sheets(&self) -> &[Sheet] {
        &self.sheets
    }
}

/// Conversion of a library cell to a workbook cell.
///
/// Exhaustive `match`: a new library data kind must break
/// the build here, rather than silently become an empty cell.
fn convert(data: &Data) -> Cell {
    match data {
        Data::Empty => Cell::Empty,
        Data::String(value) => Cell::Text(value.clone()),
        Data::Float(value) => decimal(*value),
        Data::Int(value) => Cell::Number(Dec::new(Decimal::from(*value))),
        Data::Bool(value) => Cell::Bool(*value),
        Data::DateTime(value) => excel_serial(value.as_f64()),
        // The date row and duration arrive already as text: there is nothing to parse
        // here—the report, not the workbook, defines the format.
        Data::DateTimeIso(value) | Data::DurationIso(value) => Cell::Text(value.clone()),
        Data::Error(error) => Cell::Error(format!("{error:?}")),
    }
}

/// Binary floating point to decimal.
///
/// Via the shortest reversible string representation: `1234.56`
/// in the workbook must become `1234.56`, not `1234.5599999999999`.
/// A number that the decimal type cannot represent remains text—
/// not zero or a lost cell (§4.9).
fn decimal(value: f64) -> Cell {
    let rendered = format!("{value}");
    rendered
        .parse::<Decimal>()
        .map_or(Cell::Text(rendered), |parsed| {
            Cell::Number(Dec::new(parsed))
        })
}

/// A number with a date style to a date.
///
/// The fractional part is discarded: the time of day in the broker report
/// is not the operation date; the operation date is the day.
fn excel_serial(serial: f64) -> Cell {
    // Conversion goes through the decimal type, not an `as` cast:
    // conversion outside the range silently produces the type limit, and the date would shift by centuries without any indication of an error.
    //
    let Some(days) = Decimal::from_f64_retain(serial)
        .map(|days| days.trunc())
        .and_then(|days| days.to_i64())
    else {
        return Cell::Text(format!("{serial}"));
    };
    EXCEL_EPOCH
        .checked_add(time::Duration::days(days))
        .map_or_else(|| Cell::Text(format!("{serial}")), Cell::Date)
}
