//! Reading one document through one profile.
//!
//! What comes out is [`ObservedRow`] — the row as its source stated it — and
//! there is nothing else it could be: the type has no operation kind, no
//! classification, no category of the owner's and no arithmetic, so a profile
//! has nothing to reach for. What a row *turns out to be* is settled after this
//! function has finished, by the owner's directory, by one of his
//! classification rules, or by his answer to a question.
//!
//! **Every cell is validated, and one bad row is one bad row.** A row that
//! cannot be read is refused with [`Rejection`] — field, expected, actual — and
//! the remaining rows of the document are read (§10.1). No cell is guessed at,
//! and a cell the profile did not name is not read at all.
//!
//! That is also why there is no key for "the last two lines are totals". A
//! trailing totals line is read as a row, fails to be one, and is rejected by
//! name. A count of lines to drop is a claim that is true of one export and
//! false of the next, and when it is false it discards real movements in
//! silence.

use iaam_core::ids::AccountId;
use iaam_core::money::CurrencyCode;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use time::macros::format_description;
use time::{Date, Time};

use crate::classification::FarSide;
use crate::csv_source::AccountNames;
use crate::observation::{ObservedCounterparty, ObservedDirection, ObservedRow, RowIdentity};
use crate::operation::{OperationDates, to_minor_units};
use crate::verdict::Rejection;

use super::{
    AccountSource, AmountSource, CsvShape, CurrencySource, DateField, DateFormat, DatedCell,
    DecimalSeparator, DecimalShape, DirectionSource, DocumentShape, Encoding, GroupSeparator,
    NegativeForm, SourceProfile, TimeFormat, TimeSource,
};

/// Version of the derived row key's form.
///
/// Part of the key itself, as it is in the in-tree CSV reader's: keys have
/// already been deduplicated against, so a change of form must be visible in
/// the value rather than inferred from its shape.
const DERIVED_ROW_KEY_VERSION: u8 = 1;

/// What the engine made of one document.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentReading {
    /// SHA-256 of the document's bytes, in hexadecimal. Half of every derived
    /// row key, and the identity a re-read is answered `duplicate` under.
    pub digest: String,
    /// One outcome per record below the header row, in the order the document
    /// printed them.
    pub rows: Vec<ReadOutcome>,
}

impl DocumentReading {
    /// The rows that became observations.
    #[must_use]
    pub fn observed(&self) -> Vec<&ObservedRow> {
        self.rows
            .iter()
            .filter_map(|outcome| match outcome {
                ReadOutcome::Observed { row, .. } => Some(row.as_ref()),
                ReadOutcome::Rejected { .. } => None,
            })
            .collect()
    }
}

/// One record of the document, read or refused.
///
/// The locator is on both arms, and it is the **one-based line the record
/// begins at** — the number an operator counts to in his own file. A refused
/// row that could not say where it was would be a refusal nobody can act on.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadOutcome {
    Observed { locator: u64, row: Box<ObservedRow> },
    Rejected { locator: u64, rejection: Rejection },
}

impl ReadOutcome {
    #[must_use]
    pub const fn locator(&self) -> u64 {
        match self {
            Self::Observed { locator, .. } | Self::Rejected { locator, .. } => *locator,
        }
    }
}

/// What the engine needs that a profile may not carry.
///
/// Both fields are the owner's, and neither can be written into a shipped
/// artefact: the directory is his accounts, and the declaration is what he said
/// this document is a statement of.
#[derive(Debug, Clone, Copy)]
pub struct ReadContext<'a> {
    pub accounts: &'a AccountNames,
    /// The account the caller declared for this document, where it declared
    /// one. Required by a profile whose `row.account` is `declaration`.
    pub declared: Option<AccountId>,
}

/// The document's own header row, as this profile would read it.
///
/// Published because recognition needs it and recognition is the catalogue's:
/// a profile recognises a document when the document, read through *that
/// profile's* format, encoding, delimiter and header row, carries every header
/// cell the profile names. Asking any other way would mean one profile's answer
/// depending on another's idea of where the headings are.
pub fn header_of(bytes: &[u8], profile: &SourceProfile) -> Result<Vec<String>, Rejection> {
    let DocumentShape::Csv(shape) = profile.document();
    let text = decode(bytes, shape.encoding)?;
    let records = records(&text, shape)?;
    Ok(header(&records, shape)?.cells.clone())
}

/// The record that carries the headings, by the line the profile names.
fn header<'a>(records: &'a [Record], shape: &CsvShape) -> Result<&'a Record, Rejection> {
    let wanted = u64::from(shape.header_row);
    records
        .iter()
        .find(|record| record.locator == wanted)
        .ok_or_else(|| Rejection {
            field: "document".to_owned(),
            expected: format!(
                "a document whose line {wanted} carries the headings, as this profile says"
            ),
            actual: match records.last() {
                None => "an empty document".to_owned(),
                Some(last) => format!(
                    "no record begins at that line; the last begins at line {}",
                    last.locator
                ),
            },
        })
}

/// Whether this profile recognises the document.
///
/// Every header cell the profile names must be present in the document's header
/// row, compared after trimming and otherwise exactly. A document no profile
/// recognises is refused, and so is one two profiles recognise — see
/// [`super::catalogue::ProfileCatalogue::recognise`].
#[must_use]
pub fn recognises(bytes: &[u8], profile: &SourceProfile) -> bool {
    let Ok(header) = header_of(bytes, profile) else {
        return false;
    };
    let printed: Vec<&str> = header.iter().map(|cell| cell.trim()).collect();
    profile
        .recognised_by()
        .iter()
        .all(|wanted| printed.contains(&wanted.as_str()))
}

/// Read the document.
///
/// The outer `Err` is a refusal of the **document**: bytes that are not text in
/// the profile's encoding, a header row that is not there, a heading the
/// profile names and the document does not have. None of those is a property of
/// a row, and reporting one per row would print the same sentence a thousand
/// times while burying the one fact the operator needs.
pub fn read(
    bytes: &[u8],
    profile: &SourceProfile,
    context: &ReadContext<'_>,
) -> Result<DocumentReading, Rejection> {
    let DocumentShape::Csv(shape) = profile.document();
    let text = decode(bytes, shape.encoding)?;
    let records = records(&text, shape)?;
    let header = header(&records, shape)?;
    let header_line = header.locator;
    let columns = Columns::of(header, profile)?;
    // A profile that defers to the caller's declaration and finds none is a
    // refusal of the whole document, not of every row: the caller can supply
    // the declaration and try again, and a thousand identical rejections would
    // hide that.
    if matches!(profile.row().account, AccountSource::Declaration) && context.declared.is_none() {
        return Err(Rejection {
            field: "account".to_owned(),
            expected: "an account declared for this document: this profile says the \
                       document is one account's statement and does not print which"
                .to_owned(),
            actual: "no account declared".to_owned(),
        });
    }

    let digest = hex_digest(bytes);
    let mut rows = Vec::new();
    for record in records.iter().filter(|record| record.locator > header_line) {
        let locator = record.locator;
        rows.push(
            match row(record, &columns, profile, context, &digest, locator) {
                Ok(row) => ReadOutcome::Observed {
                    locator,
                    row: Box::new(row),
                },
                Err(rejection) => ReadOutcome::Rejected { locator, rejection },
            },
        );
    }
    Ok(DocumentReading { digest, rows })
}

/// The key under which one row of one document is the same submission twice.
///
/// **The document digest and the row's locator, and nothing else** — not the
/// profile, not its version, not the session. Re-reading the same document
/// under a new profile version therefore yields the same keys, so the second
/// import is answered `duplicate` and appends nothing until the first is
/// retracted. That is deliberate: putting the version into the key would let
/// both imports stand at once and double a month of movements while the owner
/// read a green response.
///
/// `profile` names the **form** of the key — a document read by this engine —
/// and never which profile read it. A content digest remains forbidden for ADR
/// 0017's reason: it merges two genuine identical payments and loses a movement
/// that really happened. Two identical rows sit at two locators and keep two
/// keys.
fn derived_row_key(document: &str, locator: u64) -> String {
    format!("profile:v{DERIVED_ROW_KEY_VERSION}:{document}:row:{locator}")
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One record of the document, with the line it was printed at.
struct Record {
    /// The one-based line the record begins at.
    locator: u64,
    cells: Vec<String>,
}

/// Bytes into text, in the encoding the profile names.
///
/// A byte sequence that is not text in that encoding is a refusal of the
/// document. It is not repaired and not read lossily: a replacement character
/// in a counterparty's name is a fact about somebody the owner never dealt
/// with.
fn decode(bytes: &[u8], encoding: Encoding) -> Result<String, Rejection> {
    let unreadable = |code: &str| Rejection {
        field: "document".to_owned(),
        expected: format!("bytes this document's profile can read as {code}"),
        actual: "bytes that are not text in that encoding".to_owned(),
    };
    // A byte-order mark is refused here rather than left to the reader, and the
    // reason is that the reader does not leave it: the `csv` crate strips a
    // leading mark whatever the profile said, so a profile describing a
    // document without one would silently read one that has it. The two spellings
    // would then be a distinction the schema offers and the engine cannot make.
    // A mark is not an unreadable byte — it is a different document from the one
    // this profile describes, and the refusal says so in those words.
    let marked = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    match encoding {
        Encoding::Utf8 if marked => Err(Rejection {
            field: "document".to_owned(),
            expected: "a document with no byte-order mark, which is what this profile \
                       describes; a profile for a document that carries one names the \
                       encoding utf-8-bom"
                .to_owned(),
            actual: "a leading byte-order mark".to_owned(),
        }),
        Encoding::Utf8 => std::str::from_utf8(bytes)
            .map(ToOwned::to_owned)
            .map_err(|_| unreadable("utf-8")),
        Encoding::Utf8Bom => {
            let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
            std::str::from_utf8(body)
                .map(ToOwned::to_owned)
                .map_err(|_| unreadable("utf-8"))
        }
        Encoding::Windows1251 => {
            let (text, _, malformed) = encoding_rs::WINDOWS_1251.decode(bytes);
            if malformed {
                return Err(unreadable("windows-1251"));
            }
            Ok(text.into_owned())
        }
    }
}

/// The document as records of cells.
///
/// **The locator is a line and not a count of records**, and the difference is
/// not cosmetic. A blank line yields no record at all — the reader discards it
/// before a record starts — so a preamble containing one would shift every
/// later record by one and put the header row inside the table, silently, and
/// differently for two exports of one format. A line is what the profile's
/// `header_row` names, it is what an operator counts to in his own file when a
/// refusal names a row, and it is half of the derived row key. Where a quoted
/// cell carries a newline the record spans two lines and the locator is the one
/// it began at, which is still the line to look at.
///
/// The line is counted here rather than taken from the reader's own
/// `Position::line`, and that is not distrust of the library: the position a
/// record carries is the one the reader stood at **before** parsing it, so a
/// blank line it then discards is not in the number. Counting from the byte
/// offset and stepping over the discarded terminators is the same arithmetic
/// done where the discarding is visible.
///
/// The reader is deliberately **flexible** about how many cells a record has,
/// because the alternative is worse: a strict reader abandons the whole
/// document at the first ragged line, and a ragged line is precisely the
/// trailing totals row this design wants rejected by name with its neighbours
/// intact. The arity is checked per row instead, in [`Columns::cell`].
fn records(text: &str, shape: &CsvShape) -> Result<Vec<Record>, Rejection> {
    let bytes = text.as_bytes();
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(shape.delimiter.byte())
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut records: Vec<Record> = Vec::new();
    // How far the line count has been carried, and the line at that point.
    let mut counted = 0_usize;
    let mut line = 1_u64;
    for record in reader.records() {
        let record = record.map_err(|error| Rejection {
            field: "document".to_owned(),
            expected: "a delimited document whose quoting this reader can follow".to_owned(),
            actual: error.to_string(),
        })?;
        let hint = record
            .position()
            .and_then(|position| usize::try_from(position.byte()).ok())
            .unwrap_or(counted);
        let mut start = hint.max(counted).min(bytes.len());
        while bytes
            .get(start)
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            start += 1;
        }
        while counted < start {
            if bytes[counted] == b'\n' {
                line += 1;
            }
            counted += 1;
        }
        records.push(Record {
            locator: line,
            cells: record.iter().map(ToOwned::to_owned).collect(),
        });
    }
    Ok(records)
}

/// Where each heading the profile names sits in a record.
struct Columns {
    /// Heading, and its zero-based position.
    positions: Vec<(String, usize)>,
    /// How many cells the header row had, which is what a row must have too.
    width: usize,
}

impl Columns {
    /// Locate every heading the profile names, or refuse the document.
    ///
    /// A heading the document does not have is a profile that does not read
    /// this document, not a column that reads as empty — the difference between
    /// a refusal an operator can act on and a month of rows with a field
    /// silently missing.
    ///
    /// A heading printed twice is refused for the neighbouring reason: choosing
    /// either occurrence would read a column nobody chose, and the two may hold
    /// different things.
    fn of(header: &Record, profile: &SourceProfile) -> Result<Self, Rejection> {
        let printed: Vec<&str> = header.cells.iter().map(|cell| cell.trim()).collect();
        let mut positions = Vec::new();
        for wanted in profile.columns() {
            let found: Vec<usize> = printed
                .iter()
                .enumerate()
                .filter(|(_, cell)| **cell == wanted)
                .map(|(index, _)| index)
                .collect();
            match found.as_slice() {
                [only] => positions.push((wanted.to_owned(), *only)),
                [] => {
                    return Err(Rejection {
                        field: "document".to_owned(),
                        expected: format!("a header row carrying the column «{wanted}»"),
                        actual: format!("a header row of: {}", printed.join(", ")),
                    });
                }
                several => {
                    return Err(Rejection {
                        field: "document".to_owned(),
                        expected: format!("the column «{wanted}» printed once"),
                        actual: format!("{} columns with that heading", several.len()),
                    });
                }
            }
        }
        Ok(Self {
            positions,
            width: header.cells.len(),
        })
    }

    /// The cell one heading names, verbatim.
    ///
    /// A record of the wrong width is refused before any cell is read: a row
    /// with fewer cells than the header has lost the alignment every column
    /// depends on, and reading it would put one column's value in another's
    /// field. This is the arm a trailing totals line falls down.
    fn cell<'a>(&self, record: &'a Record, column: &str) -> Result<&'a str, Rejection> {
        if record.cells.len() != self.width {
            return Err(Rejection {
                field: "row".to_owned(),
                expected: format!("a record of {} cells, as the header row has", self.width),
                actual: format!("{} cells", record.cells.len()),
            });
        }
        let position = self
            .positions
            .iter()
            .find(|(heading, _)| heading == column)
            .map(|(_, position)| *position)
            .ok_or_else(|| Rejection {
                field: "row".to_owned(),
                expected: format!("the column «{column}», which this profile names"),
                actual: "a column this reading did not locate".to_owned(),
            })?;
        record
            .cells
            .get(position)
            .map(String::as_str)
            .ok_or_else(|| Rejection {
                field: "row".to_owned(),
                expected: format!("a cell under «{column}»"),
                actual: "a record that ends before it".to_owned(),
            })
    }
}

/// One record into one observation.
fn row(
    record: &Record,
    columns: &Columns,
    profile: &SourceProfile,
    context: &ReadContext<'_>,
    digest: &str,
    locator: u64,
) -> Result<ObservedRow, Rejection> {
    let shape = profile.row();
    let account = match &shape.account {
        AccountSource::Declaration => context.declared.ok_or_else(|| Rejection {
            field: "account".to_owned(),
            expected: "an account declared for this document".to_owned(),
            actual: "no account declared".to_owned(),
        })?,
        AccountSource::Column { column } => {
            let printed = columns.cell(record, column)?;
            context
                .accounts
                .resolve(printed)
                .map_err(|unresolved| Rejection {
                    field: "account".to_owned(),
                    expected: unresolved.expected,
                    actual: format!("«{column}»: {}", unresolved.actual),
                })?
        }
    };

    let currency = currency(record, columns, &shape.currency)?;
    let amount_minor = amount(record, columns, shape, currency)?;
    let direction = direction(record, columns, &shape.direction, amount_minor)?;
    let far_side = match &shape.far_side {
        None => FarSide::Unstated,
        Some(source) => {
            let printed = columns.cell(record, &source.column)?;
            *source.tokens.get(printed.trim()).ok_or_else(|| Rejection {
                field: "far_side".to_owned(),
                expected: format!(
                    "one of the words this profile's far-side map carries: {}",
                    source.tokens.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
                actual: format!("«{}»: «{}»", source.column, printed.trim()),
            })?
        }
    };

    let (dates, source_time) = dates(record, columns, shape)?;

    let counterparty = match &shape.counterparty {
        None => ObservedCounterparty::Unknown,
        Some(column) => {
            let printed = columns.cell(record, column)?;
            // A cell that is empty or only whitespace is «the source named
            // nobody», which is a value and not a failure. A non-empty cell is
            // transcribed exactly, including whatever spacing the source
            // printed: the string is what the owner's rules match on.
            if printed.trim().is_empty() {
                ObservedCounterparty::Unknown
            } else {
                ObservedCounterparty::Named(printed.to_owned())
            }
        }
    };

    let source_operation_id = transcribed(record, columns, shape.source_operation_id.as_deref())?;
    let identity = RowIdentity {
        document: Some(digest.to_owned()),
        row: source_operation_id,
        // A row that names no identity of its own is given one derived from the
        // document and its locator. A row whose source printed an identifier
        // keeps that: it is the journal's duplicate test, scoped by source, and
        // it outranks anything this engine could derive.
        idempotency_key: None,
    };
    let identity = if identity.row.is_none() {
        RowIdentity {
            idempotency_key: Some(derived_row_key(digest, locator)),
            ..identity
        }
    } else {
        identity
    };

    Ok(ObservedRow {
        account,
        direction,
        amount_minor,
        currency,
        counterparty,
        far_side,
        source_kind: transcribed(record, columns, shape.source_kind.as_deref())?,
        source_category: transcribed(record, columns, shape.source_category.as_deref())?,
        description: transcribed(record, columns, shape.description.as_deref())?,
        dates,
        source_time,
        identity,
    })
}

/// A cell transcribed verbatim, or the statement that the source printed none.
///
/// An empty cell is `None` rather than `Some("")`: "the source said nothing"
/// and "the source said the empty string" are the same fact here, and only one
/// of the two spellings can be matched by a rule.
fn transcribed(
    record: &Record,
    columns: &Columns,
    column: Option<&str>,
) -> Result<Option<String>, Rejection> {
    let Some(column) = column else {
        return Ok(None);
    };
    let printed = columns.cell(record, column)?;
    Ok(if printed.trim().is_empty() {
        None
    } else {
        Some(printed.to_owned())
    })
}

fn currency(
    record: &Record,
    columns: &Columns,
    source: &CurrencySource,
) -> Result<CurrencyCode, Rejection> {
    let (column, printed, code) = match source {
        CurrencySource::Fixed { code } => (None, code.clone(), code.clone()),
        CurrencySource::Column { column, spellings } => {
            let printed = columns.cell(record, column)?.trim().to_owned();
            let code = spellings
                .get(&printed)
                .cloned()
                .unwrap_or_else(|| printed.clone());
            (Some(column.as_str()), printed, code)
        }
    };
    CurrencyCode::from_code(&code).ok_or_else(|| Rejection {
        field: "currency".to_owned(),
        expected: format!(
            "a currency this system accounts in: {}",
            CurrencyCode::ALL
                .iter()
                .map(|currency| currency.code())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        actual: match column {
            Some(column) => format!("«{column}»: «{printed}»"),
            None => format!("«{printed}», the currency this profile fixes for the document"),
        },
    })
}

/// The sum, with the sign the source printed.
fn amount(
    record: &Record,
    columns: &Columns,
    shape: &super::RowShape,
    currency: CurrencyCode,
) -> Result<i64, Rejection> {
    let value = match &shape.amount.carried_by {
        AmountSource::SignedColumn { column } => {
            let printed = columns.cell(record, column)?;
            decimal(printed, &shape.amount.decimal, column)?
        }
        AmountSource::DebitCredit {
            out_column,
            in_column,
        } => {
            let out_printed = columns.cell(record, out_column)?;
            let in_printed = columns.cell(record, in_column)?;
            match (out_printed.trim().is_empty(), in_printed.trim().is_empty()) {
                // The source stated direction by choosing a column, and an
                // observation carries one amount field, so the debit column's
                // magnitude is recorded negative. The choice is the source's;
                // the encoding is the engine's.
                (false, true) => -decimal(out_printed, &shape.amount.decimal, out_column)?,
                (true, false) => decimal(in_printed, &shape.amount.decimal, in_column)?,
                (false, false) => {
                    return Err(Rejection {
                        field: "amount".to_owned(),
                        expected: format!(
                            "a sum in exactly one of «{out_column}» and «{in_column}»"
                        ),
                        actual: "both filled".to_owned(),
                    });
                }
                (true, true) => {
                    return Err(Rejection {
                        field: "amount".to_owned(),
                        expected: format!(
                            "a sum in exactly one of «{out_column}» and «{in_column}»"
                        ),
                        actual: "neither filled".to_owned(),
                    });
                }
            }
        }
    };
    // Refused, not rounded: rounding an input is a silent alteration of the
    // fact. And refused at zero, because a row stating no movement is not a
    // movement of zero — it is also what makes the sign an unambiguous
    // statement of direction where a profile reads one from it.
    let minor = to_minor_units(value, currency, "amount")?;
    if minor == 0 {
        return Err(Rejection {
            field: "amount".to_owned(),
            expected: "a non-zero sum: a row stating no movement is not a movement of zero"
                .to_owned(),
            actual: value.to_string(),
        });
    }
    Ok(minor)
}

/// A printed number into a decimal, in the shape the profile describes.
///
/// Strict, and every refusal is a rejected row rather than a repaired one.
/// There is one accommodation and it is the engine's rather than a profile's: a
/// leading plus is accepted everywhere, because making its acceptance a choice
/// would be a profile changing what the engine reads.
///
/// The *position* of a group separator is not checked. What a profile author
/// knows is which character his institution groups with; whether it groups in
/// threes is neither knowable from a specification nor load-bearing, since
/// removing the separators leaves the digits unambiguous either way. What is
/// checked is that a separator never stands next to the decimal point or at
/// either end, where it would be a different number rather than the same one
/// spelled out.
fn decimal(printed: &str, shape: &DecimalShape, column: &str) -> Result<Decimal, Rejection> {
    let refuse = |actual: String| Rejection {
        field: "amount".to_owned(),
        expected: format!(
            "a number written as this profile says this source writes them: {} for the \
             decimal point, {} between groups of digits, {} for a negative",
            match shape.decimal_separator {
                DecimalSeparator::Dot => "a dot",
                DecimalSeparator::Comma => "a comma",
            },
            match shape.group_separator {
                GroupSeparator::None => "nothing",
                GroupSeparator::Space => "a space",
                GroupSeparator::Comma => "a comma",
                GroupSeparator::Dot => "a dot",
                GroupSeparator::Apostrophe => "an apostrophe",
            },
            match shape.negative {
                NegativeForm::LeadingMinus => "a leading minus",
                NegativeForm::TrailingMinus => "a trailing minus",
                NegativeForm::Parentheses => "parentheses",
            },
        ),
        actual: format!("«{column}»: {actual}"),
    };
    let body = printed.trim();
    if body.is_empty() {
        return Err(refuse("an empty cell".to_owned()));
    }
    let (body, negative) = match shape.negative {
        NegativeForm::LeadingMinus => match body.strip_prefix('-') {
            Some(rest) => (rest, true),
            None => (body, false),
        },
        NegativeForm::TrailingMinus => match body.strip_suffix('-') {
            Some(rest) => (rest, true),
            None => (body, false),
        },
        NegativeForm::Parentheses => match body
            .strip_prefix('(')
            .and_then(|rest| rest.strip_suffix(')'))
        {
            Some(rest) => (rest, true),
            None => (body, false),
        },
    };
    let body = body.trim();
    let body = body.strip_prefix('+').unwrap_or(body);
    let separator = match shape.decimal_separator {
        DecimalSeparator::Dot => '.',
        DecimalSeparator::Comma => ',',
    };
    let groupers: &[char] = match shape.group_separator {
        GroupSeparator::None => &[],
        // One token for three characters an institution may emit and nobody can
        // see the difference between.
        GroupSeparator::Space => &[' ', '\u{a0}', '\u{202f}'],
        GroupSeparator::Comma => &[','],
        GroupSeparator::Dot => &['.'],
        GroupSeparator::Apostrophe => &['\''],
    };

    let mut digits = String::with_capacity(body.len());
    let mut seen_separator = false;
    let characters: Vec<char> = body.chars().collect();
    for (index, character) in characters.iter().copied().enumerate() {
        if character == separator {
            if seen_separator {
                return Err(refuse(format!("«{printed}», with two decimal points")));
            }
            seen_separator = true;
            digits.push('.');
            continue;
        }
        if groupers.contains(&character) {
            let between_digits = index > 0
                && characters
                    .get(index - 1)
                    .copied()
                    .is_some_and(|before| before.is_ascii_digit())
                && characters
                    .get(index + 1)
                    .copied()
                    .is_some_and(|after| after.is_ascii_digit());
            if !between_digits {
                return Err(refuse(format!(
                    "«{printed}», with a group separator that is not between two digits"
                )));
            }
            continue;
        }
        if !character.is_ascii_digit() {
            return Err(refuse(format!("«{printed}»")));
        }
        digits.push(character);
    }
    if digits.is_empty() || digits.starts_with('.') || digits.ends_with('.') {
        return Err(refuse(format!("«{printed}»")));
    }
    let parsed: Decimal = digits.parse().map_err(|_| refuse(format!("«{printed}»")))?;
    Ok(if negative { -parsed } else { parsed })
}

fn direction(
    record: &Record,
    columns: &Columns,
    source: &DirectionSource,
    amount_minor: i64,
) -> Result<ObservedDirection, Rejection> {
    match source {
        // Zero is already refused, so the sign is a total statement.
        DirectionSource::AmountSign => Ok(if amount_minor < 0 {
            ObservedDirection::Out
        } else {
            ObservedDirection::In
        }),
        DirectionSource::Column { column, tokens } => {
            let printed = columns.cell(record, column)?;
            tokens
                .get(printed.trim())
                .copied()
                .ok_or_else(|| Rejection {
                    field: "direction".to_owned(),
                    expected: format!(
                        "one of the words this profile's direction map carries: {}. There is no \
                     catch-all: reading an unmapped word as «unknown» would say the source \
                     stated nothing, and it stated something this profile could not read",
                        tokens.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                    actual: format!("«{column}»: «{}»", printed.trim()),
                })
        }
    }
}

/// The day or days, and the time of day where the source printed one.
///
/// Where the profile names one date the engine records it as **both** the trade
/// date and the cash-posted date, as the in-tree CSV reader does: a row with no
/// posted day is a row no money-flow report can place in a month, and a profile
/// cannot ask for one without the other.
fn dates(
    record: &Record,
    columns: &Columns,
    shape: &super::RowShape,
) -> Result<(OperationDates, Option<Time>), Rejection> {
    let mut source_time = None;
    let mut read =
        |cell: Option<&DatedCell>, field: DateField| -> Result<Option<Date>, Rejection> {
            let Some(cell) = cell else {
                return Ok(None);
            };
            let printed = columns.cell(record, &cell.column)?.trim();
            let carries_time = matches!(
                &shape.time,
                Some(TimeSource::DateCell { date_field, .. }) if *date_field == field
            );
            let (day, rest) = if carries_time {
                match printed.split_once(' ') {
                    Some((day, rest)) => (day, Some(rest.trim())),
                    None => {
                        return Err(Rejection {
                            field: "time".to_owned(),
                            expected: format!(
                                "a date and a time separated by a space, as this profile says \
                             «{}» carries",
                                cell.column
                            ),
                            actual: format!("«{}»: «{printed}»", cell.column),
                        });
                    }
                }
            } else {
                (printed, None)
            };
            let parsed = date(day, cell.format, &cell.column)?;
            if let (Some(rest), Some(TimeSource::DateCell { format, .. })) = (rest, &shape.time) {
                source_time = Some(time_of_day(rest, *format, &cell.column)?);
            }
            Ok(Some(parsed))
        };
    let trade = read(shape.dates.trade.as_ref(), DateField::Trade)?;
    let cash_posted = read(shape.dates.cash_posted.as_ref(), DateField::CashPosted)?;
    if let Some(TimeSource::Column { column, format }) = &shape.time {
        let printed = columns.cell(record, column)?.trim();
        source_time = Some(time_of_day(printed, *format, column)?);
    }
    let day = trade.or(cash_posted);
    Ok((
        OperationDates {
            trade: trade.or(day),
            cash_posted: cash_posted.or(day),
            ..OperationDates::default()
        },
        source_time,
    ))
}

fn date(printed: &str, format: DateFormat, column: &str) -> Result<Date, Rejection> {
    // Parsed inside each arm rather than by looking a description up first:
    // `format_description!` produces a value whose type carries the pattern's
    // own length, so the four arms have four types and only the parsed date is
    // common to them.
    let (parsed, shape) = match format {
        DateFormat::IsoDate => (
            Date::parse(printed, format_description!("[year]-[month]-[day]")),
            "YYYY-MM-DD",
        ),
        DateFormat::DayMonthYearDot => (
            Date::parse(printed, format_description!("[day].[month].[year]")),
            "DD.MM.YYYY",
        ),
        DateFormat::DayMonthYearSlash => (
            Date::parse(printed, format_description!("[day]/[month]/[year]")),
            "DD/MM/YYYY",
        ),
        DateFormat::MonthDayYearSlash => (
            Date::parse(printed, format_description!("[month]/[day]/[year]")),
            "MM/DD/YYYY",
        ),
    };
    parsed.map_err(|_| Rejection {
        field: "date".to_owned(),
        expected: format!("a date written {shape}, the format this profile names for «{column}»"),
        actual: format!("«{column}»: «{printed}»"),
    })
}

fn time_of_day(printed: &str, format: TimeFormat, column: &str) -> Result<Time, Rejection> {
    let (parsed, shape) = match format {
        TimeFormat::HourMinute => (
            Time::parse(printed, format_description!("[hour]:[minute]")),
            "HH:MM",
        ),
        TimeFormat::HourMinuteSecond => (
            Time::parse(printed, format_description!("[hour]:[minute]:[second]")),
            "HH:MM:SS",
        ),
    };
    parsed.map_err(|_| Rejection {
        field: "time".to_owned(),
        expected: format!("a time written {shape}, the format this profile names for «{column}»"),
        actual: format!("«{column}»: «{printed}»"),
    })
}

#[cfg(test)]
mod tests {
    use iaam_core::ids::AccountId;
    use time::macros::{date, time};

    use crate::csv_source::{AccountEntry, AccountNames};
    use crate::observation::ObservedCounterparty;
    use crate::profile::load;

    use super::*;

    /// An invented profile over an invented document.
    ///
    /// Every heading, word and number in this module is made up. Nothing here
    /// is trimmed from anybody's export: a file derived from real rows carries
    /// real rows.
    fn profile(patch: serde_json::Value) -> SourceProfile {
        let mut value = serde_json::json!({
            "schema_version": 1,
            "id": "example-bank-statement",
            "version": 2,
            "issuer": "Example Bank",
            "document": {
                "format": "csv",
                "encoding": "utf-8",
                "delimiter": "semicolon",
                "header_row": 1
            },
            "recognise": { "header_cells": ["Posted", "Sum"] },
            "row": {
                "account": { "from": "declaration" },
                "dates": { "trade": { "column": "Posted", "format": "iso_date" } },
                "amount": {
                    "decimal": {
                        "decimal_separator": "dot",
                        "group_separator": "none",
                        "negative": "leading_minus"
                    },
                    "carried_by": { "from": "signed_column", "column": "Sum" }
                },
                "currency": { "from": "fixed", "code": "RUB" },
                "direction": { "from": "amount_sign" }
            }
        });
        merge(&mut value, patch);
        load::from_bytes(value.to_string().as_bytes()).expect("the test profile loads")
    }

    /// Deep-merge, so a test states only what it changes.
    fn merge(into: &mut serde_json::Value, patch: serde_json::Value) {
        match (into, patch) {
            (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
                for (key, value) in patch {
                    if value.is_null() {
                        target.remove(&key);
                        continue;
                    }
                    merge(target.entry(key).or_insert(serde_json::Value::Null), value);
                }
            }
            (target, patch) => *target = patch,
        }
    }

    fn account() -> (AccountId, AccountNames) {
        let id = AccountId::new_random();
        let names: AccountNames = [AccountEntry::titled("Everyday", id)].into_iter().collect();
        (id, names)
    }

    fn read_with(document: &str, profile: &SourceProfile) -> DocumentReading {
        let (declared, names) = account();
        read(
            document.as_bytes(),
            profile,
            &ReadContext {
                accounts: &names,
                declared: Some(declared),
            },
        )
        .expect("the document is readable")
    }

    fn observed(outcome: &ReadOutcome) -> &ObservedRow {
        match outcome {
            ReadOutcome::Observed { row, .. } => row,
            ReadOutcome::Rejected { rejection, .. } => {
                panic!("expected an observation, got {rejection:?}")
            }
        }
    }

    fn rejection(outcome: &ReadOutcome) -> &Rejection {
        match outcome {
            ReadOutcome::Rejected { rejection, .. } => rejection,
            ReadOutcome::Observed { row, .. } => panic!("expected a refusal, got {row:?}"),
        }
    }

    /// The sum keeps the sign the source printed, and the sign is the direction
    /// where the profile says so.
    #[test]
    fn a_row_is_transcribed_with_the_source_s_own_sign() {
        let reading = read_with(
            "Posted;Sum\n2026-08-05;-100.00\n",
            &profile(serde_json::json!({})),
        );
        let row = observed(&reading.rows[0]);
        assert_eq!(row.amount_minor, -10_000);
        assert_eq!(row.direction, ObservedDirection::Out);
        assert_eq!(row.dates.trade, Some(date!(2026 - 08 - 05)));
        // One printed day is recorded as both: a row with no posted day is a
        // row no money-flow report can place in a month.
        assert_eq!(row.dates.cash_posted, Some(date!(2026 - 08 - 05)));
    }

    /// **One bad row is one bad row.** The neighbours of an unreadable record
    /// are read, and the refusal names the field, what was admissible and what
    /// arrived.
    ///
    /// The third line here is the trailing totals line every export has, and it
    /// is why there is no profile key for "ignore the last two lines": a count
    /// of lines to drop is true of one export and false of the next, and when
    /// it is false it discards real movements in silence.
    #[test]
    fn one_unreadable_row_does_not_take_its_neighbours_with_it() {
        let reading = read_with(
            "Posted;Sum\n2026-08-05;-100.00\n2026-08-06;not a number\nTotal;-100.00\n",
            &profile(serde_json::json!({})),
        );
        assert_eq!(reading.rows.len(), 3);
        assert_eq!(observed(&reading.rows[0]).amount_minor, -10_000);
        assert_eq!(rejection(&reading.rows[1]).field, "amount");
        assert_eq!(rejection(&reading.rows[2]).field, "date");
        assert_eq!(reading.rows[2].locator(), 4);
    }

    /// A number with more precision than the currency's minor unit is refused,
    /// not rounded: rounding an input is a silent alteration of the fact.
    #[test]
    fn more_precision_than_the_minor_unit_is_refused() {
        let reading = read_with(
            "Posted;Sum\n2026-08-05;-100.005\n",
            &profile(serde_json::json!({})),
        );
        assert_eq!(rejection(&reading.rows[0]).field, "amount");
    }

    /// A row stating no movement is not a movement of zero.
    #[test]
    fn a_zero_sum_is_refused() {
        let reading = read_with(
            "Posted;Sum\n2026-08-05;0.00\n",
            &profile(serde_json::json!({})),
        );
        assert_eq!(rejection(&reading.rows[0]).field, "amount");
    }

    /// A currency this system does not account in is refused, whatever the
    /// profile says. Load time checks the shape; every row checks the code.
    #[test]
    fn an_unknown_currency_is_refused_per_row() {
        let reading = read_with(
            "Posted;Sum;Ccy\n2026-08-05;-100.00;GBP\n",
            &profile(serde_json::json!({
                "row": { "currency": { "from": "column", "column": "Ccy", "code": null } }
            })),
        );
        assert_eq!(rejection(&reading.rows[0]).field, "currency");
    }

    /// A spelling the map rewrites reaches the engine as a code, and the
    /// engine's own validation still runs.
    #[test]
    fn a_currency_spelling_is_rewritten_and_then_validated() {
        let reading = read_with(
            "Posted;Sum;Ccy\n2026-08-05;-100.00;RUR\n2026-08-06;-1.00;QQQ\n",
            &profile(serde_json::json!({
                "row": {
                    "currency": {
                        "from": "column",
                        "column": "Ccy",
                        "spellings": { "RUR": "RUB" },
                        "code": null
                    }
                }
            })),
        );
        assert_eq!(observed(&reading.rows[0]).currency, CurrencyCode::Rub);
        assert_eq!(rejection(&reading.rows[1]).field, "currency");
    }

    /// A direction word the map does not carry rejects the row and names the
    /// word. It is **not** read as `unknown`: `unknown` asserts the source said
    /// nothing, and here it said something the profile could not read.
    #[test]
    fn a_direction_word_outside_the_map_rejects_the_row_and_names_it() {
        let reading = read_with(
            "Posted;Sum;Operation\n2026-08-05;100.00;Debit\n2026-08-06;100.00;Reversal\n",
            &profile(serde_json::json!({
                "row": {
                    "direction": {
                        "from": "column",
                        "column": "Operation",
                        "tokens": { "Debit": "out", "Credit": "in" }
                    }
                }
            })),
        );
        // The sum keeps the source's own sign, and is never re-signed from the
        // direction word: the row says `+100` and `out` at once, and both are
        // what the source printed.
        assert_eq!(observed(&reading.rows[0]).direction, ObservedDirection::Out);
        assert_eq!(observed(&reading.rows[0]).amount_minor, 10_000);
        let refusal = rejection(&reading.rows[1]);
        assert_eq!(refusal.field, "direction");
        assert!(refusal.actual.contains("Reversal"), "{refusal:?}");
    }

    /// Two columns of magnitudes: exactly one filled, and the debit column's
    /// magnitude is recorded negative.
    #[test]
    fn a_debit_and_a_credit_column_state_direction_by_which_is_filled() {
        let profile = profile(serde_json::json!({
            "row": {
                "amount": {
                    "carried_by": {
                        "from": "debit_credit",
                        "out_column": "Paid",
                        "in_column": "Received",
                        "column": null
                    }
                }
            }
        }));
        let reading = read_with(
            "Posted;Paid;Received\n2026-08-05;100.00;\n2026-08-06;;42.00\n\
             2026-08-07;1.00;2.00\n2026-08-08;;\n",
            &profile,
        );
        assert_eq!(observed(&reading.rows[0]).amount_minor, -10_000);
        assert_eq!(observed(&reading.rows[0]).direction, ObservedDirection::Out);
        assert_eq!(observed(&reading.rows[1]).amount_minor, 4_200);
        assert_eq!(rejection(&reading.rows[2]).actual, "both filled");
        assert_eq!(rejection(&reading.rows[3]).actual, "neither filled");
    }

    /// A heading the profile names and the document does not have refuses the
    /// **document**, not every row of it.
    #[test]
    fn a_missing_column_refuses_the_document() {
        let (declared, names) = account();
        let refusal = read(
            b"Posted\n2026-08-05\n",
            &profile(serde_json::json!({})),
            &ReadContext {
                accounts: &names,
                declared: Some(declared),
            },
        )
        .expect_err("a document without the sum column is not this document");
        assert_eq!(refusal.field, "document");
        assert!(refusal.expected.contains("Sum"), "{refusal:?}");
    }

    /// A heading printed twice is refused rather than guessed between.
    #[test]
    fn a_duplicated_column_refuses_the_document() {
        let (declared, names) = account();
        let refusal = read(
            b"Posted;Sum;Sum\n2026-08-05;-1.00;-2.00\n",
            &profile(serde_json::json!({})),
            &ReadContext {
                accounts: &names,
                declared: Some(declared),
            },
        )
        .expect_err("two columns with one heading are an ambiguity");
        assert_eq!(refusal.field, "document");
    }

    /// A profile that defers to the caller's declaration and finds none refuses
    /// the document once, rather than every row identically.
    #[test]
    fn a_document_that_names_no_account_and_no_declaration_is_refused_once() {
        let (_, names) = account();
        let refusal = read(
            b"Posted;Sum\n2026-08-05;-1.00\n",
            &profile(serde_json::json!({})),
            &ReadContext {
                accounts: &names,
                declared: None,
            },
        )
        .expect_err("nothing says whose statement this is");
        assert_eq!(refusal.field, "account");
    }

    /// A printed account name that names none of the owner's accounts refuses
    /// its own row, by name, and the rest of the document is read.
    ///
    /// This is where a converter outside the server counts a row as "outside
    /// the contour" and drops it in silence. Here the row is refused, the
    /// refusal names the column and the string, and the operator can see that a
    /// month of one account went unread.
    #[test]
    fn an_unrecognised_account_refuses_its_own_row() {
        let profile = profile(serde_json::json!({
            "row": { "account": { "from": "column", "column": "Account" } }
        }));
        let (declared, names) = account();
        let reading = read(
            "Posted;Sum;Account\n2026-08-05;-1.00;Everyday\n2026-08-06;-2.00;Elsewhere\n"
                .as_bytes(),
            &profile,
            &ReadContext {
                accounts: &names,
                declared: Some(declared),
            },
        )
        .expect("the document is readable");
        assert_eq!(observed(&reading.rows[0]).account, declared);
        let refusal = rejection(&reading.rows[1]);
        assert_eq!(refusal.field, "account");
        assert!(refusal.actual.contains("Elsewhere"), "{refusal:?}");
    }

    /// A date and a time in one cell, and the time is recorded as printed.
    #[test]
    fn a_date_cell_carrying_a_time_is_split_at_the_space() {
        let profile = profile(serde_json::json!({
            "row": {
                "dates": { "trade": { "column": "Posted", "format": "day_month_year_dot" } },
                "time": { "from": "date_cell", "date_field": "trade", "format": "hour_minute_second" }
            }
        }));
        let reading = read_with("Posted;Sum\n05.08.2026 14:30:00;-1.00\n", &profile);
        let row = observed(&reading.rows[0]);
        assert_eq!(row.dates.trade, Some(date!(2026 - 08 - 05)));
        assert_eq!(row.source_time, Some(time!(14:30:00)));
    }

    /// A grouped number in the shape the profile describes, and one that is not.
    ///
    /// The three spaces are the three an institution may emit — ordinary,
    /// non-breaking, narrow non-breaking — because which of them a bank uses is
    /// not knowledge a profile author has.
    #[test]
    fn a_grouped_number_is_read_and_a_misgrouped_one_is_refused() {
        let profile = profile(serde_json::json!({
            "row": {
                "amount": {
                    "decimal": {
                        "decimal_separator": "comma",
                        "group_separator": "space",
                        "negative": "leading_minus"
                    }
                }
            }
        }));
        // The document is a fixture rather than a literal: a correctly grouped
        // amount is, by construction, shaped like one off a statement, and
        // `scripts/check-no-personal-data.sh` refuses that shape in a source
        // file whatever it means. Its own allowance is a fixture directory, and
        // a four-row document belongs in a file regardless.
        let reading = read_with(
            include_str!("../../tests/fixtures/profile/grouped-amounts.csv"),
            &profile,
        );
        assert_eq!(observed(&reading.rows[0]).amount_minor, -123_456_789);
        assert_eq!(observed(&reading.rows[1]).amount_minor, -123_450);
        assert_eq!(observed(&reading.rows[2]).amount_minor, -123_450);
        assert_eq!(rejection(&reading.rows[3]).field, "amount");
    }

    /// A leading plus is accepted in every profile and has no key, because
    /// making its acceptance a choice would be a profile changing what the
    /// engine reads.
    #[test]
    fn a_leading_plus_is_accepted_without_a_key_for_it() {
        let reading = read_with(
            "Posted;Sum\n2026-08-05;+100.00\n",
            &profile(serde_json::json!({})),
        );
        assert_eq!(observed(&reading.rows[0]).amount_minor, 10_000);
    }

    /// The negative form the profile names, and only that one.
    #[test]
    fn a_negative_written_another_way_is_refused() {
        let profile = profile(serde_json::json!({
            "row": { "amount": { "decimal": { "negative": "parentheses" } } }
        }));
        let reading = read_with(
            "Posted;Sum\n2026-08-05;(100.00)\n2026-08-06;-100.00\n",
            &profile,
        );
        assert_eq!(observed(&reading.rows[0]).amount_minor, -10_000);
        assert_eq!(rejection(&reading.rows[1]).field, "amount");
    }

    /// Two identical rows are two facts, and they keep two keys because they
    /// sit at two locators.
    ///
    /// A key over the row's **contents** would merge them and lose a movement
    /// that really happened, which ADR 0017 forbids. The key carries the
    /// document digest and the locator and nothing else — not the profile, not
    /// its version — so re-reading the same document under a corrected profile
    /// yields the same keys and appends nothing until the first import is
    /// retracted.
    #[test]
    fn two_identical_rows_keep_two_keys_and_a_re_read_keeps_the_same_ones() {
        let document = "Posted;Sum\n2026-08-05;-100.00\n2026-08-05;-100.00\n";
        let first = read_with(document, &profile(serde_json::json!({})));
        let keys: Vec<Option<String>> = first
            .rows
            .iter()
            .map(|outcome| observed(outcome).identity.idempotency_key.clone())
            .collect();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
        assert!(
            keys[0]
                .as_deref()
                .is_some_and(|key| key.starts_with("profile:v1:")),
            "{keys:?}"
        );
        // A later version of the same profile is a different reader and the
        // same document, so the keys do not move.
        let again = read_with(document, &profile(serde_json::json!({ "version": 9 })));
        let later: Vec<Option<String>> = again
            .rows
            .iter()
            .map(|outcome| observed(outcome).identity.idempotency_key.clone())
            .collect();
        assert_eq!(keys, later);
    }

    /// A source that prints its own row identifier keeps it, and no key is
    /// derived over it: the source's identifier is the journal's duplicate
    /// test, scoped by source, and it outranks anything this engine could
    /// derive.
    #[test]
    fn a_source_that_names_its_own_row_keeps_that_identity() {
        let profile = profile(serde_json::json!({
            "row": { "source_operation_id": { "column": "Reference" } }
        }));
        let reading = read_with("Posted;Sum;Reference\n2026-08-05;-1.00;OP-7\n", &profile);
        let row = observed(&reading.rows[0]);
        assert_eq!(row.identity.row.as_deref(), Some("OP-7"));
        assert_eq!(row.identity.idempotency_key, None);
    }

    /// The source's own operation word and its own category are transcribed to
    /// **separate** fields, and neither is mapped.
    ///
    /// The pair used to round-trip through one slot, so a category rule the
    /// owner wrote on a category never matched an observed row and one written
    /// on an operation word matched rows he was not describing (`iaam-p683`).
    #[test]
    fn the_source_s_word_and_the_source_s_category_go_to_their_own_fields() {
        let profile = profile(serde_json::json!({
            "row": {
                "source_kind": { "column": "Operation" },
                "source_category": { "column": "Category" },
                "counterparty": { "column": "Payee" },
                "description": { "column": "Purpose" }
            }
        }));
        let reading = read_with(
            "Posted;Sum;Operation;Category;Payee;Purpose\n\
             2026-08-05;-1.00;Card payment;Groceries;Shop One;card 1\n\
             2026-08-06;-2.00;Card payment;Groceries;   ;card 2\n",
            &profile,
        );
        let row = observed(&reading.rows[0]);
        assert_eq!(row.source_kind.as_deref(), Some("Card payment"));
        assert_eq!(row.source_category.as_deref(), Some("Groceries"));
        assert_eq!(
            row.counterparty,
            ObservedCounterparty::Named("Shop One".to_owned())
        );
        assert_eq!(row.description.as_deref(), Some("card 1"));
        // A cell of nothing but whitespace is «the source named nobody», which
        // is a value and not a failure.
        assert_eq!(
            observed(&reading.rows[1]).counterparty,
            ObservedCounterparty::Unknown
        );
    }

    /// A source that asserts the far side is the owner's in words, and one that
    /// prints a word the map does not carry.
    #[test]
    fn a_far_side_word_is_transcribed_and_an_unmapped_one_rejects_the_row() {
        let profile = profile(serde_json::json!({
            "row": {
                "far_side": {
                    "column": "Operation",
                    "tokens": { "Own transfer": "own_account", "Card payment": "unstated" }
                }
            }
        }));
        let reading = read_with(
            "Posted;Sum;Operation\n2026-08-05;-1.00;Own transfer\n\
             2026-08-06;-2.00;Card payment\n2026-08-07;-3.00;Refund\n",
            &profile,
        );
        assert_eq!(observed(&reading.rows[0]).far_side, FarSide::OwnAccount);
        assert_eq!(observed(&reading.rows[1]).far_side, FarSide::Unstated);
        assert_eq!(rejection(&reading.rows[2]).field, "far_side");
    }

    /// Bytes that are not text in the profile's encoding refuse the document,
    /// and are never read lossily: a replacement character in a counterparty's
    /// name is a fact about somebody the owner never dealt with.
    #[test]
    fn bytes_that_are_not_text_in_the_named_encoding_refuse_the_document() {
        let (declared, names) = account();
        let refusal = read(
            b"Posted;Sum\n2026-08-05;-1.00\xff\n",
            &profile(serde_json::json!({})),
            &ReadContext {
                accounts: &names,
                declared: Some(declared),
            },
        )
        .expect_err("these bytes are not utf-8");
        assert_eq!(refusal.field, "document");
    }

    /// `utf-8-bom` removes a leading mark, and `utf-8` does not — so a document
    /// with a mark read as plain utf-8 fails to be recognised rather than
    /// reading a heading nobody printed.
    #[test]
    fn a_byte_order_mark_is_removed_only_where_the_profile_says_so() {
        let mut document = vec![0xEF, 0xBB, 0xBF];
        document.extend_from_slice(b"Posted;Sum\n2026-08-05;-1.00\n");
        let strict = profile(serde_json::json!({}));
        let tolerant = profile(serde_json::json!({ "document": { "encoding": "utf-8-bom" } }));
        assert!(!recognises(&document, &strict));
        assert!(recognises(&document, &tolerant));

        let (declared, names) = account();
        let reading = read(
            &document,
            &tolerant,
            &ReadContext {
                accounts: &names,
                declared: Some(declared),
            },
        )
        .expect("the mark is removed");
        assert_eq!(observed(&reading.rows[0]).amount_minor, -100);
        // And a document without a mark is still read by the same profile: the
        // mark is invisible, and an author cannot see whether his institution
        // emits one.
        assert!(recognises(b"Posted;Sum\n2026-08-05;-1.00\n", &tolerant));
    }

    /// A profile recognises a document only when every header cell it names is
    /// printed, and a preamble above the header row is not a row.
    #[test]
    fn recognition_reads_the_header_row_this_profile_names() {
        let profile = profile(serde_json::json!({
            "document": { "header_row": 3 },
            "recognise": { "header_cells": ["Posted", "Sum"] }
        }));
        let document = "Statement of account\n\nPosted;Sum\n2026-08-05;-1.00\n";
        assert!(recognises(document.as_bytes(), &profile));
        let reading = read_with(document, &profile);
        assert_eq!(reading.rows.len(), 1);
        assert_eq!(reading.rows[0].locator(), 4);
        assert!(!recognises(b"Posted;Total\n", &profile));
    }
}
