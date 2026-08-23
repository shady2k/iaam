//! Книга отчёта: листы и ячейки.
//!
//! Тип собственный, а не заимствованный у `calamine`: парсеры и тесты
//! не зависят от API библиотеки чтения, книгу можно собрать в памяти
//! без файла, а замена библиотеки не трогает ни один парсер.
//!
//! **Здесь проходит граница ввода-вывода.** XLSX хранит числа двоичной
//! плавающей точкой, а даты — числами со стилем даты. Разбор этого
//! живёт в одном месте, чтобы доменные величины `f64` не видели.

use std::io::Cursor;

use calamine::{Data, Reader};
use iaam_core::numeric::decimal::Dec;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use thiserror::Error;
use time::Date;
use time::macros::date;

/// Эпоха дат Excel. Тридцатое декабря 1899 года, а не тридцать первое:
/// Excel считает 1900 год високосным, и эпоха сдвинута на день, чтобы
/// его ошибка компенсировалась.
const EXCEL_EPOCH: Date = date!(1899 - 12 - 30);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkbookError {
    #[error("книга не читается: {detail}")]
    Unreadable { detail: String },
    #[error("лист {sheet} не читается: {detail}")]
    UnreadableSheet { sheet: String, detail: String },
}

/// Ячейка книги.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    Empty,
    Text(String),
    Number(Dec),
    Date(Date),
    Bool(bool),
    /// Ячейка с ошибкой вычисления (`#DIV/0!` и подобные).
    ///
    /// Отдельно от текста намеренно: `#Н/Д`, попавшее в текстовую
    /// ячейку, парсер принял бы за подпись.
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

/// Лист книги.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub name: String,
    pub rows: Vec<Vec<Cell>>,
}

impl Sheet {
    /// Ячейка по нулевым индексам строки и колонки.
    ///
    /// За краем листа — пусто, а не отказ: отчёты приходят с рваными
    /// строками, и обращение к отсутствующей ячейке в них нормально.
    #[must_use]
    pub fn cell(&self, row: usize, column: usize) -> &Cell {
        self.rows
            .get(row)
            .and_then(|cells| cells.get(column))
            .unwrap_or(&Cell::Empty)
    }

    /// Есть ли в листе текстовая ячейка, содержащая подстроку.
    /// Регистр не важен: заголовки в отчётах пишут как придётся.
    #[must_use]
    pub fn contains_text(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.rows.iter().flatten().any(|cell| {
            cell.text()
                .is_some_and(|text| text.to_lowercase().contains(&needle))
        })
    }
}

/// Открытая книга.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workbook {
    sheets: Vec<Sheet>,
}

impl Workbook {
    /// Книга из готовых листов. Нужна тестам и парсерам, собирающим
    /// книгу иначе: опознание проверяется без двоичных фикстур.
    #[must_use]
    pub const fn of(sheets: Vec<Sheet>) -> Self {
        Self { sheets }
    }

    /// Чтение книги из байтов. Формат определяется содержимым.
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

/// Перевод ячейки библиотеки в ячейку книги.
///
/// Исчерпывающий `match`: новый вид данных библиотеки обязан сломать
/// сборку здесь, а не молча стать пустой ячейкой.
fn convert(data: &Data) -> Cell {
    match data {
        Data::Empty => Cell::Empty,
        Data::String(value) => Cell::Text(value.clone()),
        Data::Float(value) => decimal(*value),
        Data::Int(value) => Cell::Number(Dec::new(Decimal::from(*value))),
        Data::Bool(value) => Cell::Bool(*value),
        Data::DateTime(value) => excel_serial(value.as_f64()),
        // Строка даты и длительность приходят уже текстом: разбирать их
        // здесь нечем — формат задаёт отчёт, а не книга.
        Data::DateTimeIso(value) | Data::DurationIso(value) => Cell::Text(value.clone()),
        Data::Error(error) => Cell::Error(format!("{error:?}")),
    }
}

/// Двоичная плавающая точка в десятичную.
///
/// Через кратчайшее обратимое строковое представление: `1234.56`
/// в книге обязано стать `1234.56`, а не `1234.5599999999999`.
/// Число, которого десятичный тип не представляет, остаётся текстом —
/// не нулём и не потерянной ячейкой (§4.9).
fn decimal(value: f64) -> Cell {
    let rendered = format!("{value}");
    rendered
        .parse::<Decimal>()
        .map_or(Cell::Text(rendered), |parsed| {
            Cell::Number(Dec::new(parsed))
        })
}

/// Число со стилем даты в дату.
///
/// Дробная часть отбрасывается: время суток в брокерском отчёте не
/// является датой операции, а датой операции является день.
fn excel_serial(serial: f64) -> Cell {
    // Перевод идёт через десятичный тип, а не приведением `as`:
    // приведение вне диапазона молча даёт край типа, и дата уехала бы
    // на столетия без единого признака ошибки.
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
