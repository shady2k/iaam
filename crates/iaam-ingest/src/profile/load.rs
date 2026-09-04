//! Reading one profile file, whole or not at all.
//!
//! **A profile is accepted whole or refused whole**, and the unit of that rule
//! is one file: an unreadable profile must not take an instance's other formats
//! down with it, which is [`super::catalogue`]'s business, and a *half*-read
//! profile must never exist, which is this module's.
//!
//! The reader is written against the schema's three review invariants rather
//! than derived from a type, and that is deliberate. Two of the three are
//! properties a derive cannot state:
//!
//! 1. **Closure.** Every object is opened, its keys taken one at a time, and
//!    closed; [`Object::close`] refuses whatever is left, naming the key and its
//!    path. An unknown key is refused rather than ignored, because an ignored
//!    key is a rule its author believed was in force.
//! 2. **Three leaf kinds and no fourth.** Every leaf here is read by exactly one
//!    of [`text`], [`integer`] and [`token`] — a locator, a literal, or a word
//!    from a closed vocabulary this file enumerates. There is no reader for a
//!    number the engine computes with, for a regular expression, or for a
//!    format pattern, so there is nothing to write one into.
//! 3. **No leniency vocabulary.** There is no key here meaning accept,
//!    tolerate, ignore, skip, default, fallback, on-error, round or coerce.
//!    Every such key an author writes reaches [`Object::close`] and is refused
//!    by name.
//!
//! What is *not* checked here is anything about a cell: whether a number fits
//! the currency's minor unit, whether a date exists, whether a currency is
//! known. Those are asked of every row, by the engine, and a profile cannot
//! change the answer.

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::classification::FarSide;
use crate::observation::ObservedDirection;

use super::{
    AccountSource, Amount, AmountSource, CsvShape, CurrencySource, DateField, DateFormat,
    DatedCell, Dates, DecimalSeparator, DecimalShape, Delimiter, DirectionSource, DocumentShape,
    Encoding, FarSideSource, GroupSeparator, NegativeForm, RowShape, SCHEMA_VERSION, SourceProfile,
    TimeFormat, TimeSource,
};

/// Why a file is not a profile.
///
/// The same three parts a row's [`crate::verdict::Rejection`] carries, for the
/// same reason: an author is owed the place, what was admissible there, and what
/// he wrote. `at` is a dotted path into the file — `row.amount.carried_by` —
/// because "invalid profile" sends somebody to read four hundred lines of JSON.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{at}: expected {expected}, found {actual}")]
pub struct ProfileError {
    pub at: String,
    pub expected: String,
    pub actual: String,
}

impl ProfileError {
    fn new(at: impl Into<String>, expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            expected: expected.into(),
            actual: actual.into(),
        }
    }
}

/// Load one profile from the bytes of one file.
///
/// The digest is taken over the bytes as they arrived, before anything is
/// parsed: a version is a name for a content, and the content it names is the
/// file, not this build's reading of it.
pub fn from_bytes(bytes: &[u8]) -> Result<SourceProfile, ProfileError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| ProfileError::new("", "one JSON object", error.to_string()))?;
    let digest = hex_digest(bytes);
    let mut root = Object::open("", value)?;

    let claimed = integer(
        &root.path("schema_version"),
        root.require("schema_version")?,
        0,
        u64::from(u32::MAX),
    )?;
    if claimed != SCHEMA_VERSION {
        return Err(ProfileError::new(
            root.path("schema_version"),
            format!(
                "schema version {SCHEMA_VERSION}, the only vocabulary this build implements: \
                 a profile read with the wrong vocabulary is the silent acceptance this \
                 design exists to refuse"
            ),
            claimed.to_string(),
        ));
    }

    let id = profile_id(&root.path("id"), root.require("id")?)?;
    let version = integer(
        &root.path("version"),
        root.require("version")?,
        1,
        u64::from(u32::MAX),
    )?;
    let issuer = text(&root.path("issuer"), root.require("issuer")?, 1, 200)?;
    let document_label = match root.take("document_label") {
        None => None,
        Some(value) => Some(text(&root.path("document_label"), value, 1, 200)?),
    };
    let document = document(&root.path("document"), root.require("document")?)?;
    let recognise = recognition(&root.path("recognise"), root.require("recognise")?)?;
    let row = row_shape(&root.path("row"), root.require("row")?)?;
    root.close()?;

    Ok(SourceProfile {
        id,
        version,
        issuer,
        document_label,
        document,
        recognise,
        row,
        digest,
    })
}

/// SHA-256 of a profile file, in the hexadecimal form a `RawHash` takes.
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn document(at: &str, value: Value) -> Result<DocumentShape, ProfileError> {
    let mut object = Object::open(at, value)?;
    let format = text(&object.path("format"), object.require("format")?, 1, 32)?;
    let shape = match format.as_str() {
        "csv" => {
            let encoding = token(
                &object.path("encoding"),
                object.require("encoding")?,
                ENCODINGS,
            )?;
            let delimiter = token(
                &object.path("delimiter"),
                object.require("delimiter")?,
                DELIMITERS,
            )?;
            let header_row = integer(
                &object.path("header_row"),
                object.require("header_row")?,
                1,
                1000,
            )?;
            DocumentShape::Csv(CsvShape {
                encoding,
                delimiter,
                header_row,
            })
        }
        // Schema version 1 admits a workbook and this engine does not read one.
        // Refused here rather than accepted and half-read: a workbook cell
        // arrives already typed — calamine hands back a date or a number, not
        // the text a `date_format` or a `decimal` block describes — so what
        // those blocks mean against a typed cell is a question decision 0019
        // did not answer. A profile that loaded and then read dates by a rule
        // nobody chose is exactly the silent reading this design refuses; a
        // profile that does not load is published as refused, with this reason.
        "xlsx" => {
            return Err(ProfileError::new(
                object.path("format"),
                "csv, the one document shape this engine reads. A workbook cell arrives \
                 already typed, so a named date format and a decimal shape have nothing to \
                 act on, and what they should mean there is not settled",
                "xlsx",
            ));
        }
        other => {
            return Err(ProfileError::new(
                object.path("format"),
                "csv or xlsx",
                other,
            ));
        }
    };
    object.close()?;
    Ok(shape)
}

fn recognition(at: &str, value: Value) -> Result<Vec<String>, ProfileError> {
    let mut object = Object::open(at, value)?;
    let cells = object.require("header_cells")?;
    let path = object.path("header_cells");
    let Value::Array(items) = cells else {
        return Err(ProfileError::new(
            &path,
            "an array of header cells",
            kind_of(&cells),
        ));
    };
    if items.is_empty() || items.len() > 32 {
        return Err(ProfileError::new(
            &path,
            "between 1 and 32 header cells",
            items.len().to_string(),
        ));
    }
    let mut header_cells: Vec<String> = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let cell = literal(&format!("{path}[{index}]"), item)?;
        if header_cells.contains(&cell) {
            return Err(ProfileError::new(
                format!("{path}[{index}]"),
                "a header cell named once",
                format!("«{cell}», already named"),
            ));
        }
        header_cells.push(cell);
    }
    object.close()?;
    Ok(header_cells)
}

fn row_shape(at: &str, value: Value) -> Result<RowShape, ProfileError> {
    let mut object = Object::open(at, value)?;
    let account = account(&object.path("account"), object.require("account")?)?;
    let dates = dates(&object.path("dates"), object.require("dates")?)?;
    let time = match object.take("time") {
        None => None,
        Some(value) => Some(time_source(&object.path("time"), value, &dates)?),
    };
    let amount = amount(&object.path("amount"), object.require("amount")?)?;
    let currency = currency(&object.path("currency"), object.require("currency")?)?;
    let direction = direction(&object.path("direction"), object.require("direction")?)?;
    let far_side = match object.take("far_side") {
        None => None,
        Some(value) => Some(far_side(&object.path("far_side"), value)?),
    };
    let counterparty = column_block(&mut object, "counterparty")?;
    let description = column_block(&mut object, "description")?;
    let source_kind = column_block(&mut object, "source_kind")?;
    let source_category = column_block(&mut object, "source_category")?;
    let source_operation_id = column_block(&mut object, "source_operation_id")?;
    object.close()?;
    Ok(RowShape {
        account,
        dates,
        time,
        amount,
        currency,
        direction,
        far_side,
        counterparty,
        description,
        source_kind,
        source_category,
        source_operation_id,
    })
}

/// A block whose whole content is one column heading.
///
/// Five fields share this shape — counterparty, description, the source's own
/// operation word, its own category, its own row identifier — and every one of
/// them is transcribed verbatim into the field that field belongs in. One
/// reader, so a sixth cannot acquire an extra key by being written separately.
fn column_block(object: &mut Object, key: &str) -> Result<Option<String>, ProfileError> {
    let Some(value) = object.take(key) else {
        return Ok(None);
    };
    let at = object.path(key);
    let mut block = Object::open(&at, value)?;
    let column = column(&block.path("column"), block.require("column")?)?;
    block.close()?;
    Ok(Some(column))
}

fn account(at: &str, value: Value) -> Result<AccountSource, ProfileError> {
    let mut object = Object::open(at, value)?;
    let from = text(&object.path("from"), object.require("from")?, 1, 32)?;
    let source = match from.as_str() {
        "declaration" => AccountSource::Declaration,
        "column" => AccountSource::Column {
            column: column(&object.path("column"), object.require("column")?)?,
        },
        other => {
            return Err(ProfileError::new(
                object.path("from"),
                "declaration or column. There is no third arm, and in particular no arm \
                 taking an account of the owner's: a profile is a shipped artefact",
                other,
            ));
        }
    };
    object.close()?;
    Ok(source)
}

fn dates(at: &str, value: Value) -> Result<Dates, ProfileError> {
    let mut object = Object::open(at, value)?;
    let trade = match object.take("trade") {
        None => None,
        Some(value) => Some(dated_cell(&object.path("trade"), value)?),
    };
    let cash_posted = match object.take("cash_posted") {
        None => None,
        Some(value) => Some(dated_cell(&object.path("cash_posted"), value)?),
    };
    object.close()?;
    if trade.is_none() && cash_posted.is_none() {
        return Err(ProfileError::new(
            at,
            "at least one dated cell: a row with no day is a row no report can place \
             in a month",
            "neither trade nor cash_posted",
        ));
    }
    Ok(Dates { trade, cash_posted })
}

fn dated_cell(at: &str, value: Value) -> Result<DatedCell, ProfileError> {
    let mut object = Object::open(at, value)?;
    let column = column(&object.path("column"), object.require("column")?)?;
    let format = token(
        &object.path("format"),
        object.require("format")?,
        DATE_FORMATS,
    )?;
    object.close()?;
    Ok(DatedCell { column, format })
}

fn time_source(at: &str, value: Value, dates: &Dates) -> Result<TimeSource, ProfileError> {
    let mut object = Object::open(at, value)?;
    let from = text(&object.path("from"), object.require("from")?, 1, 32)?;
    let source = match from.as_str() {
        "date_cell" => {
            let date_field = token(
                &object.path("date_field"),
                object.require("date_field")?,
                DATE_FIELDS,
            )?;
            // A time taken out of a cell the profile did not name is a time
            // taken out of nothing. Refused at load rather than per row,
            // because it is a statement about the profile and it would
            // otherwise reject every row of every document.
            let named = match date_field {
                DateField::Trade => dates.trade.is_some(),
                DateField::CashPosted => dates.cash_posted.is_some(),
            };
            if !named {
                return Err(ProfileError::new(
                    object.path("date_field"),
                    "a date this profile's row block names",
                    match date_field {
                        DateField::Trade => "trade, which row.dates does not name",
                        DateField::CashPosted => "cash_posted, which row.dates does not name",
                    },
                ));
            }
            let format = token(
                &object.path("format"),
                object.require("format")?,
                TIME_FORMATS,
            )?;
            TimeSource::DateCell { date_field, format }
        }
        "column" => TimeSource::Column {
            column: column(&object.path("column"), object.require("column")?)?,
            format: token(
                &object.path("format"),
                object.require("format")?,
                TIME_FORMATS,
            )?,
        },
        other => {
            return Err(ProfileError::new(
                object.path("from"),
                "date_cell or column",
                other,
            ));
        }
    };
    object.close()?;
    Ok(source)
}

fn amount(at: &str, value: Value) -> Result<Amount, ProfileError> {
    let mut object = Object::open(at, value)?;
    let decimal = decimal(&object.path("decimal"), object.require("decimal")?)?;
    let carried_by = carried_by(&object.path("carried_by"), object.require("carried_by")?)?;
    object.close()?;
    Ok(Amount {
        decimal,
        carried_by,
    })
}

fn carried_by(at: &str, value: Value) -> Result<AmountSource, ProfileError> {
    let mut object = Object::open(at, value)?;
    let from = text(&object.path("from"), object.require("from")?, 1, 32)?;
    let source = match from.as_str() {
        "signed_column" => AmountSource::SignedColumn {
            column: column(&object.path("column"), object.require("column")?)?,
        },
        "debit_credit" => {
            let out_column = column(&object.path("out_column"), object.require("out_column")?)?;
            let in_column = column(&object.path("in_column"), object.require("in_column")?)?;
            if out_column == in_column {
                return Err(ProfileError::new(
                    object.path("in_column"),
                    "a column other than out_column: one column cannot state both \
                     directions, and a profile naming it twice would reject every row \
                     as having both filled",
                    in_column,
                ));
            }
            AmountSource::DebitCredit {
                out_column,
                in_column,
            }
        }
        other => {
            return Err(ProfileError::new(
                object.path("from"),
                "signed_column or debit_credit",
                other,
            ));
        }
    };
    object.close()?;
    Ok(source)
}

fn decimal(at: &str, value: Value) -> Result<DecimalShape, ProfileError> {
    let mut object = Object::open(at, value)?;
    let decimal_separator = token(
        &object.path("decimal_separator"),
        object.require("decimal_separator")?,
        DECIMAL_SEPARATORS,
    )?;
    // The group separator's vocabulary is chosen by the decimal separator, and
    // the two lists differ by exactly the character the decimal point has
    // taken. So a profile whose group separator is its decimal separator is
    // refused by name, rather than left to be discovered on a number that
    // parses to the wrong value.
    let group_path = object.path("group_separator");
    let group_value = object.require("group_separator")?;
    let group_separator = match decimal_separator {
        DecimalSeparator::Dot => token(&group_path, group_value, GROUPS_WITH_DOT_DECIMAL)?,
        DecimalSeparator::Comma => token(&group_path, group_value, GROUPS_WITH_COMMA_DECIMAL)?,
    };
    let negative = token(
        &object.path("negative"),
        object.require("negative")?,
        NEGATIVE_FORMS,
    )?;
    object.close()?;
    Ok(DecimalShape {
        decimal_separator,
        group_separator,
        negative,
    })
}

fn currency(at: &str, value: Value) -> Result<CurrencySource, ProfileError> {
    let mut object = Object::open(at, value)?;
    let from = text(&object.path("from"), object.require("from")?, 1, 32)?;
    let source = match from.as_str() {
        "fixed" => CurrencySource::Fixed {
            code: currency_code(&object.path("code"), object.require("code")?)?,
        },
        "column" => {
            let column = column(&object.path("column"), object.require("column")?)?;
            let spellings = match object.take("spellings") {
                None => BTreeMap::new(),
                Some(value) => {
                    let path = object.path("spellings");
                    let mut spellings = BTreeMap::new();
                    for (key, code) in literal_keyed(&path, value, 64)? {
                        spellings
                            .insert(key.clone(), currency_code(&format!("{path}.{key}"), code)?);
                    }
                    spellings
                }
            };
            CurrencySource::Column { column, spellings }
        }
        other => {
            return Err(ProfileError::new(
                object.path("from"),
                "fixed or column",
                other,
            ));
        }
    };
    object.close()?;
    Ok(source)
}

fn direction(at: &str, value: Value) -> Result<DirectionSource, ProfileError> {
    let mut object = Object::open(at, value)?;
    let from = text(&object.path("from"), object.require("from")?, 1, 32)?;
    let source = match from.as_str() {
        "amount_sign" => DirectionSource::AmountSign,
        "column" => {
            let column = column(&object.path("column"), object.require("column")?)?;
            let path = object.path("tokens");
            let tokens = token_map(&path, object.require("tokens")?, DIRECTIONS, 128)?;
            DirectionSource::Column { column, tokens }
        }
        other => {
            return Err(ProfileError::new(
                object.path("from"),
                "amount_sign or column",
                other,
            ));
        }
    };
    object.close()?;
    Ok(source)
}

fn far_side(at: &str, value: Value) -> Result<FarSideSource, ProfileError> {
    let mut object = Object::open(at, value)?;
    let column = column(&object.path("column"), object.require("column")?)?;
    let path = object.path("tokens");
    let tokens = token_map(&path, object.require("tokens")?, FAR_SIDES, 128)?;
    object.close()?;
    Ok(FarSideSource { column, tokens })
}

// ---------------------------------------------------------------------------
// The closed vocabularies. Every token a profile may write is in this section
// and nowhere else, so the second review invariant is one screen to check.
// ---------------------------------------------------------------------------

const ENCODINGS: &[(&str, Encoding)] = &[
    ("utf-8", Encoding::Utf8),
    ("utf-8-bom", Encoding::Utf8Bom),
    ("windows-1251", Encoding::Windows1251),
];

const DELIMITERS: &[(&str, Delimiter)] = &[
    ("comma", Delimiter::Comma),
    ("semicolon", Delimiter::Semicolon),
    ("tab", Delimiter::Tab),
];

const DATE_FORMATS: &[(&str, DateFormat)] = &[
    ("iso_date", DateFormat::IsoDate),
    ("day_month_year_dot", DateFormat::DayMonthYearDot),
    ("day_month_year_slash", DateFormat::DayMonthYearSlash),
    ("month_day_year_slash", DateFormat::MonthDayYearSlash),
];

const DATE_FIELDS: &[(&str, DateField)] = &[
    ("trade", DateField::Trade),
    ("cash_posted", DateField::CashPosted),
];

const TIME_FORMATS: &[(&str, TimeFormat)] = &[
    ("hour_minute", TimeFormat::HourMinute),
    ("hour_minute_second", TimeFormat::HourMinuteSecond),
];

const DECIMAL_SEPARATORS: &[(&str, DecimalSeparator)] = &[
    ("dot", DecimalSeparator::Dot),
    ("comma", DecimalSeparator::Comma),
];

const GROUPS_WITH_DOT_DECIMAL: &[(&str, GroupSeparator)] = &[
    ("none", GroupSeparator::None),
    ("space", GroupSeparator::Space),
    ("comma", GroupSeparator::Comma),
    ("apostrophe", GroupSeparator::Apostrophe),
];

const GROUPS_WITH_COMMA_DECIMAL: &[(&str, GroupSeparator)] = &[
    ("none", GroupSeparator::None),
    ("space", GroupSeparator::Space),
    ("dot", GroupSeparator::Dot),
    ("apostrophe", GroupSeparator::Apostrophe),
];

const NEGATIVE_FORMS: &[(&str, NegativeForm)] = &[
    ("leading_minus", NegativeForm::LeadingMinus),
    ("trailing_minus", NegativeForm::TrailingMinus),
    ("parentheses", NegativeForm::Parentheses),
];

const DIRECTIONS: &[(&str, ObservedDirection)] = &[
    ("in", ObservedDirection::In),
    ("out", ObservedDirection::Out),
    ("inner", ObservedDirection::Inner),
    ("unknown", ObservedDirection::Unknown),
];

const FAR_SIDES: &[(&str, FarSide)] = &[
    ("unstated", FarSide::Unstated),
    ("own_account", FarSide::OwnAccount),
];

// ---------------------------------------------------------------------------
// Leaves. Three kinds and no fourth: a locator, a token, a literal.
// ---------------------------------------------------------------------------

/// One object being read, and the keys of it nobody has claimed.
struct Object {
    at: String,
    map: serde_json::Map<String, Value>,
}

impl Object {
    fn open(at: &str, value: Value) -> Result<Self, ProfileError> {
        match value {
            Value::Object(map) => Ok(Self {
                at: at.to_owned(),
                map,
            }),
            other => Err(ProfileError::new(at, "an object", kind_of(&other))),
        }
    }

    fn path(&self, key: &str) -> String {
        if self.at.is_empty() {
            key.to_owned()
        } else {
            format!("{}.{key}", self.at)
        }
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        self.map.remove(key)
    }

    fn require(&mut self, key: &str) -> Result<Value, ProfileError> {
        let path = self.path(key);
        self.map
            .remove(key)
            .ok_or_else(|| ProfileError::new(path, "this key, which is required", "nothing"))
    }

    /// Refuse whatever was not read.
    ///
    /// The closure invariant, in one place. An unknown key is refused rather
    /// than ignored: an ignored key is a rule its author believed was in force,
    /// and the rule a profile author most wants to write — round this, tolerate
    /// that, default the other — is exactly the one that has no key.
    fn close(self) -> Result<(), ProfileError> {
        let Some(key) = self.map.keys().next().cloned() else {
            return Ok(());
        };
        Err(ProfileError::new(
            self.path(&key),
            "no key beyond the ones this schema defines: an unknown key is refused rather \
             than ignored, because an ignored key is a rule its author believed was in force",
            format!("«{key}»"),
        ))
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A string leaf, bounded as the schema bounds it.
fn text(at: &str, value: Value, min: usize, max: usize) -> Result<String, ProfileError> {
    let Value::String(text) = value else {
        return Err(ProfileError::new(at, "a string", kind_of(&value)));
    };
    let length = text.chars().count();
    if length < min || length > max {
        return Err(ProfileError::new(
            at,
            format!("between {min} and {max} characters"),
            length.to_string(),
        ));
    }
    Ok(text)
}

/// An integer leaf. Only ever a locator or a version — never a number the
/// engine computes with, of which the schema names none.
fn integer(at: &str, value: Value, min: u64, max: u64) -> Result<u32, ProfileError> {
    let Value::Number(number) = &value else {
        return Err(ProfileError::new(at, "a whole number", kind_of(&value)));
    };
    let Some(found) = number.as_u64() else {
        return Err(ProfileError::new(
            at,
            format!("a whole number between {min} and {max}"),
            number.to_string(),
        ));
    };
    if found < min || found > max {
        return Err(ProfileError::new(
            at,
            format!("a whole number between {min} and {max}"),
            found.to_string(),
        ));
    }
    u32::try_from(found).map_err(|_| {
        ProfileError::new(
            at,
            format!("a whole number between {min} and {max}"),
            found.to_string(),
        )
    })
}

/// A word from a closed vocabulary of iaam's own words.
///
/// The refusal lists the vocabulary, because an author who wrote a word this
/// build does not know needs to see the ones it does — and because a token is
/// how a source that needs a new date format asks for an engine release.
fn token<T: Copy>(at: &str, value: Value, table: &[(&str, T)]) -> Result<T, ProfileError> {
    let word = text(at, value, 1, 64)?;
    table
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, token)| *token)
        .ok_or_else(|| {
            ProfileError::new(
                at,
                table
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", "),
                word,
            )
        })
}

/// Text the source itself prints, quoted so a map can key on it.
///
/// Compared after trimming and otherwise exactly, so it is *stored* trimmed:
/// one representation, and no chance of a key that can never match because it
/// carries a space the author cannot see. Case folding was refused — it is
/// locale-dependent, and a key the author can read is better than one the
/// engine reinterprets.
fn literal(at: &str, value: Value) -> Result<String, ProfileError> {
    let text = text(at, value, 1, 200)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ProfileError::new(
            at,
            "text the source prints. A cell of nothing but whitespace is not a word a map \
             can key on",
            format!("«{text}»"),
        ));
    }
    Ok(trimmed.to_owned())
}

/// A heading in the document's header row.
fn column(at: &str, value: Value) -> Result<String, ProfileError> {
    literal(at, value)
}

fn profile_id(at: &str, value: Value) -> Result<String, ProfileError> {
    const SHAPE: &str = "lower-case words of letters and digits joined by single hyphens, \
                         3 to 64 characters: the id appears verbatim inside a ParserVersion \
                         and has to stay greppable in a journal";
    let id = text(at, value, 3, 64)?;
    let well_formed = !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if !well_formed {
        return Err(ProfileError::new(at, SHAPE, id));
    }
    Ok(id)
}

/// The shape of an ISO currency code, and only the shape.
///
/// Whether the engine knows the currency is the engine's question and it is
/// asked of every row; this catches a typo at load time and decides nothing.
fn currency_code(at: &str, value: Value) -> Result<String, ProfileError> {
    let code = text(at, value, 3, 3)?;
    if !code.chars().all(|character| character.is_ascii_uppercase()) {
        return Err(ProfileError::new(
            at,
            "three upper-case letters, the shape of an ISO 4217 code",
            code,
        ));
    }
    Ok(code)
}

/// A map keyed on the source's own printed words.
///
/// The one place `additionalProperties` is a value schema rather than a
/// closure, and the marking is the reason it is read by a function of its own:
/// a reviewer looking for a widening looks at the callers of this and at
/// nothing else.
///
/// **There is no catch-all and none can be smuggled in as a key.** A key is a
/// literal matched against a cell exactly after trimming, and the engine has no
/// wildcard — so `"*"` is a key that matches a cell printing an asterisk and
/// nothing else.
fn literal_keyed(at: &str, value: Value, max: usize) -> Result<Vec<(String, Value)>, ProfileError> {
    let Value::Object(map) = value else {
        return Err(ProfileError::new(at, "an object", kind_of(&value)));
    };
    if map.is_empty() || map.len() > max {
        return Err(ProfileError::new(
            at,
            format!("between 1 and {max} entries"),
            map.len().to_string(),
        ));
    }
    let mut entries: Vec<(String, Value)> = Vec::with_capacity(map.len());
    for (key, value) in map {
        let literal = literal(&format!("{at}.{key}"), Value::String(key.clone()))?;
        // Two keys that differ only in whitespace trim to one, and one of them
        // would then be a rule its author believed was in force.
        if entries.iter().any(|(existing, _)| *existing == literal) {
            return Err(ProfileError::new(
                format!("{at}.{key}"),
                "a word this map names once. Keys are compared after trimming, so two \
                 spellings that differ only in whitespace are one key and one of them \
                 would be silently lost",
                format!("«{literal}»"),
            ));
        }
        entries.push((literal, value));
    }
    Ok(entries)
}

fn token_map<T: Copy>(
    at: &str,
    value: Value,
    table: &[(&str, T)],
    max: usize,
) -> Result<BTreeMap<String, T>, ProfileError> {
    let mut tokens = BTreeMap::new();
    for (key, value) in literal_keyed(at, value, max)? {
        let token = token(&format!("{at}.{key}"), value, table)?;
        tokens.insert(key, token);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile that says everything schema version 1 lets a profile say.
    ///
    /// Invented end to end, with English column headings, so that a test which
    /// mutates it cannot accidentally publish a real export's shape.
    fn complete() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "id": "example-bank-statement",
            "version": 1,
            "issuer": "Example Bank",
            "document_label": "Account statement",
            "document": {
                "format": "csv",
                "encoding": "utf-8",
                "delimiter": "semicolon",
                "header_row": 1
            },
            "recognise": { "header_cells": ["Posted", "Operation", "Sum", "Ccy", "Payee"] },
            "row": {
                "account": { "from": "declaration" },
                "dates": {
                    "trade": { "column": "Operation date", "format": "day_month_year_dot" },
                    "cash_posted": { "column": "Posted", "format": "day_month_year_dot" }
                },
                "time": { "from": "date_cell", "date_field": "trade", "format": "hour_minute_second" },
                "amount": {
                    "decimal": {
                        "decimal_separator": "comma",
                        "group_separator": "space",
                        "negative": "leading_minus"
                    },
                    "carried_by": { "from": "signed_column", "column": "Sum" }
                },
                "currency": { "from": "column", "column": "Ccy", "spellings": { "RUR": "RUB" } },
                "direction": {
                    "from": "column",
                    "column": "Operation",
                    "tokens": { "Debit": "out", "Credit": "in", "Internal": "inner" }
                },
                "far_side": {
                    "column": "Operation",
                    "tokens": { "Debit": "unstated", "Credit": "unstated", "Internal": "own_account" }
                },
                "counterparty": { "column": "Payee" },
                "description": { "column": "Purpose" },
                "source_kind": { "column": "Operation" },
                "source_category": { "column": "Category" },
                "source_operation_id": { "column": "Reference" }
            }
        })
    }

    fn load(value: &serde_json::Value) -> Result<SourceProfile, ProfileError> {
        from_bytes(value.to_string().as_bytes())
    }

    /// The schema's own example is a profile this loader accepts.
    ///
    /// The schema is the authority and this file is the implementation of it;
    /// an example the implementation refuses means one of the two is wrong, and
    /// the test says so before an author copies the example.
    #[test]
    fn the_schema_s_own_example_loads() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schema/source-profile-v1.json"))
                .expect("the schema is JSON");
        let example = schema["examples"][0].clone();
        let profile = load(&example).expect("the schema's example is a profile");
        assert_eq!(profile.id(), "example-bank-statement");
        assert_eq!(
            profile.parser_version().0,
            "profile/example-bank-statement/1"
        );
    }

    /// Every widening a well-meaning author would write, and the place each is
    /// refused.
    ///
    /// This is decision 0019 §3's own list, frozen. Each entry is a profile
    /// somebody would sit down and write when a bank changes something, and the
    /// claim the design rests on is that not one of them loads. Freezing them
    /// is what keeps the claim true after somebody adds a fourth date format —
    /// the list is the reason the third review invariant survives its next
    /// edit.
    #[test]
    fn a_profile_cannot_widen_what_the_engine_accepts() {
        let refusals: Vec<(&str, serde_json::Value, &str)> = vec![
            (
                "a rounding flag",
                {
                    let mut profile = complete();
                    profile["row"]["amount"]["round_to_minor_unit"] = serde_json::json!(true);
                    profile
                },
                "row.amount.round_to_minor_unit",
            ),
            (
                "a strptime pattern in place of a format name",
                {
                    let mut profile = complete();
                    profile["row"]["dates"]["trade"]["format"] =
                        serde_json::json!("%d.%m.%Y %H:%M:%S");
                    profile
                },
                "row.dates.trade.format",
            ),
            (
                "allow_unknown beside a currency column",
                {
                    let mut profile = complete();
                    profile["row"]["currency"]["allow_unknown"] = serde_json::json!(true);
                    profile
                },
                "row.currency.allow_unknown",
            ),
            (
                "an on_unknown_token arm on a direction map",
                {
                    let mut profile = complete();
                    profile["row"]["direction"]["on_unknown_token"] = serde_json::json!("unknown");
                    profile
                },
                "row.direction.on_unknown_token",
            ),
            (
                "an account map under account",
                {
                    let mut profile = complete();
                    profile["row"]["account"] = serde_json::json!({
                        "from": "map",
                        "accounts": { "Everyday": "11111111-1111-4111-8111-111111111111" }
                    });
                    profile
                },
                "row.account.from",
            ),
            (
                "a group separator equal to the decimal separator",
                {
                    let mut profile = complete();
                    profile["row"]["amount"]["decimal"]["group_separator"] =
                        serde_json::json!("comma");
                    profile
                },
                "row.amount.decimal.group_separator",
            ),
            (
                "a category map under source_category",
                {
                    let mut profile = complete();
                    profile["row"]["source_category"] = serde_json::json!({
                        "column": "Category",
                        "map": { "Groceries": "food" }
                    });
                    profile
                },
                "row.source_category.map",
            ),
            (
                "an extract expression on the counterparty",
                {
                    let mut profile = complete();
                    profile["row"]["counterparty"] = serde_json::json!({
                        "column": "Purpose",
                        "extract": "^to (.+) for"
                    });
                    profile
                },
                "row.counterparty.extract",
            ),
            (
                "a count of trailing lines to ignore",
                {
                    let mut profile = complete();
                    profile["document"]["trailing_lines_to_ignore"] = serde_json::json!(2);
                    profile
                },
                "document.trailing_lines_to_ignore",
            ),
            (
                "a row block with no date at all",
                {
                    let mut profile = complete();
                    profile["row"]["dates"] = serde_json::json!({});
                    // The time block would otherwise fail first, for its own
                    // good reason; the case under test is the absent date.
                    profile["row"]
                        .as_object_mut()
                        .expect("row is an object")
                        .remove("time");
                    profile
                },
                "row.dates",
            ),
            (
                "version zero",
                {
                    let mut profile = complete();
                    profile["version"] = serde_json::json!(0);
                    profile
                },
                "version",
            ),
            (
                "a schema version this loader does not implement",
                {
                    let mut profile = complete();
                    profile["schema_version"] = serde_json::json!(2);
                    profile
                },
                "schema_version",
            ),
        ];
        for (what, profile, at) in refusals {
            let error = load(&profile).expect_err(&format!("«{what}» must not load"));
            assert_eq!(
                error.at, at,
                "«{what}» was refused in the wrong place: {error}"
            );
        }
    }

    /// The base the mutations above are made from is itself a profile.
    ///
    /// Without this, a mistake in `complete` would make every entry of that
    /// list pass for the wrong reason.
    #[test]
    fn the_unmutated_profile_loads() {
        let profile = load(&complete()).expect("the complete profile loads");
        assert_eq!(profile.version(), 1);
        assert_eq!(
            profile.parser_version().0,
            "profile/example-bank-statement/1"
        );
        assert!(profile.columns().contains(&"Payee"));
    }

    /// A key nobody defined is refused rather than ignored.
    ///
    /// The general form of half the list above: an ignored key is a rule its
    /// author believed was in force, and he will not find out until the month
    /// he checks.
    #[test]
    fn an_unknown_key_is_refused_and_named() {
        let mut profile = complete();
        profile["notes"] = serde_json::json!("read this file with care");
        let error = load(&profile).expect_err("an unknown key does not load");
        assert_eq!(error.at, "notes");
        assert!(error.actual.contains("notes"), "{error}");
    }

    /// A catch-all cannot be smuggled in as a map key.
    ///
    /// `"*"` is a literal, and it matches a cell printing an asterisk and
    /// nothing else. The test pins the reading rather than the refusal: there
    /// is nothing to refuse, because there is no wildcard for the key to be
    /// one of.
    #[test]
    fn a_star_is_a_word_the_source_prints_and_not_a_wildcard() {
        let mut profile = complete();
        profile["row"]["direction"]["tokens"] = serde_json::json!({ "*": "out" });
        let loaded = load(&profile).expect("a map keyed on an asterisk is a map");
        let super::DirectionSource::Column { tokens, .. } = &loaded.row().direction else {
            panic!("the direction is read from a column");
        };
        assert_eq!(tokens.len(), 1);
        assert!(tokens.contains_key("*"));
    }

    /// Two spellings of one key would be one key, and one of them would be lost.
    #[test]
    fn two_map_keys_that_differ_only_in_whitespace_are_refused() {
        let mut profile = complete();
        profile["row"]["direction"]["tokens"] =
            serde_json::json!({ "Debit": "out", " Debit": "in" });
        let error = load(&profile).expect_err("one key written twice does not load");
        assert!(error.at.starts_with("row.direction.tokens"), "{error}");
    }

    /// A workbook profile is refused, and the refusal says what is unsettled.
    ///
    /// Schema version 1 admits one and this engine reads csv only. Refused
    /// rather than accepted-and-half-read: a workbook cell arrives already
    /// typed, so a named date format has nothing to act on, and a profile that
    /// loaded would be read by a rule nobody decided.
    #[test]
    fn a_workbook_profile_does_not_load_and_says_why() {
        let mut profile = complete();
        profile["document"] = serde_json::json!({
            "format": "xlsx",
            "sheet": "Movements",
            "header_row": 3
        });
        let error = load(&profile).expect_err("this engine reads csv");
        assert_eq!(error.at, "document.format");
        assert_eq!(error.actual, "xlsx");
    }

    /// A time taken out of a date cell the profile does not name is refused at
    /// load, not once per row.
    #[test]
    fn a_time_read_from_a_date_the_profile_does_not_name_is_refused() {
        let mut profile = complete();
        profile["row"]["dates"]
            .as_object_mut()
            .expect("dates is an object")
            .remove("trade");
        let error = load(&profile).expect_err("there is no trade cell to take a time out of");
        assert_eq!(error.at, "row.time.date_field");
    }

    /// The digest is over the file's bytes, so two spellings of one profile are
    /// two contents.
    ///
    /// A version is a name for a content (§5). If the digest were taken over
    /// the parsed value, an author could change what a profile says about
    /// nothing — reordering keys is harmless — and also change what it says
    /// about something, under one digest.
    #[test]
    fn the_digest_names_the_file_and_not_the_reading() {
        let profile = complete();
        let compact = from_bytes(profile.to_string().as_bytes()).expect("loads");
        let spaced = from_bytes(
            serde_json::to_string_pretty(&profile)
                .expect("json")
                .as_bytes(),
        )
        .expect("loads");
        assert_eq!(compact.id(), spaced.id());
        assert_ne!(compact.digest(), spaced.digest());
        assert_eq!(compact.digest().len(), 64);
    }
}
