//! Source profiles: what a document says, described as data (decision 0019).
//!
//! A profile is one JSON file validated against
//! `crates/iaam-ingest/schema/source-profile-v1.json`. It names the column that
//! carries each cell and translates the source's own words into iaam's own
//! words, and it does nothing else — it is not a library, not a script and not
//! an expression language, and nothing here evaluates anything it contains.
//!
//! **A profile describes and the engine decides**, and that is a property of
//! the types rather than a rule anybody has to remember. What [`engine::read`]
//! produces is [`crate::observation::ObservedRow`] — the row as its source
//! stated it — which has no [`crate::operation::OperationKind`], no
//! classification, no category of the owner's and no arithmetic. A profile
//! therefore has nothing to reach for: there is no field on the output in which
//! a conclusion could be written.
//!
//! **One thing a profile transcribes never reaches that output at all**
//! (decision 0028): [`RowStatus`], the source's own word for whether it
//! completed the movement. A profile says which of the source's words mean
//! which of iaam's three and stops; the engine refuses a row that is not
//! `completed`, and nothing carries the word any further. That is deliberate —
//! a status in the journal would be a fact about the source's own workflow
//! sitting beside facts about the owner's money.
//!
//! Three modules, and the split is the decision's own:
//!
//! - [`load`] turns bytes into a [`SourceProfile`] or refuses them, and it is
//!   where the schema's three review invariants are enforced — closure, three
//!   leaf kinds, and no leniency vocabulary.
//! - [`engine`] reads a document through a loaded profile. Everything that can
//!   fail about a *cell* fails here, per row, naming field, expected and actual.
//! - [`catalogue`] is what an instance has installed, where each profile came
//!   from, and why any of them was refused.

pub mod catalogue;
pub mod engine;
pub mod load;

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::event::provenance::ParserVersion;

pub use catalogue::{Installed, Origin, ProfileCatalogue, Refused};
pub use engine::{DocumentReading, ReadContext, ReadOutcome, UnresolvedAccountName, read};
pub use load::ProfileError;

/// The schema version this build implements.
///
/// A file claiming any other version is refused rather than read with this
/// vocabulary: a profile validated against the wrong version is exactly the
/// silent acceptance decision 0019 exists to refuse.
pub const SCHEMA_VERSION: u32 = 1;

/// The first segment of every [`ParserVersion`] a profile produces.
///
/// Reserved, and no reader in the tree uses it — the versions here are
/// `ingest/csv/1`, `tinkoff-xlsx/…`, `finam-xls/…`, `tinkoff-api/…`,
/// `ingest/manual/1` and their neighbours — so the origin of a fact is readable
/// from the first segment alone.
pub const PARSER_VERSION_PREFIX: &str = "profile/";

/// One document type, printed by one institution, described as data.
///
/// Constructed only by [`load::from_bytes`]; the fields are readable and none
/// is writable, because a profile that could be adjusted after validation would
/// be a profile validated as something other than what read the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProfile {
    id: String,
    version: u32,
    issuer: String,
    document_label: Option<String>,
    document: DocumentShape,
    recognise: Vec<String>,
    row: RowShape,
    digest: String,
}

impl SourceProfile {
    /// The profile's name, unique within an instance.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The version of this profile's rules.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The institution that prints this document.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// What the issuer calls this document, where the profile says.
    #[must_use]
    pub fn document_label(&self) -> Option<&str> {
        self.document_label.as_deref()
    }

    /// How the bytes become a table.
    #[must_use]
    pub const fn document(&self) -> &DocumentShape {
        &self.document
    }

    /// Which cell of a row carries which field of an observation.
    #[must_use]
    pub const fn row(&self) -> &RowShape {
        &self.row
    }

    /// The header cells a document must all carry for this profile to
    /// recognise it.
    #[must_use]
    pub fn recognised_by(&self) -> &[String] {
        &self.recognise
    }

    /// SHA-256 of the file this profile was loaded from, in hexadecimal.
    ///
    /// A version is a name for a content, and this is the content it names
    /// (decision 0019 §5). It is recorded beside the profile rather than folded
    /// into [`Self::parser_version`] on purpose: a digest inside that string
    /// would demand a new `SettlementLagPolicy::with_profile` band for every
    /// byte changed, including changes that touch no date.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// What every fact this profile produced records as its reader.
    ///
    /// `profile/<id>/<version>`. The rows one version read are therefore a
    /// query rather than an archaeology, which is what makes a buggy profile's
    /// facts findable and retractable.
    #[must_use]
    pub fn parser_version(&self) -> ParserVersion {
        ParserVersion(format!(
            "{PARSER_VERSION_PREFIX}{id}/{version}",
            id = self.id,
            version = self.version
        ))
    }

    /// Every column heading this profile names, once each.
    ///
    /// What the engine checks the document's header row against: a heading the
    /// document does not have is a document this profile does not read, not a
    /// column that reads as empty.
    #[must_use]
    pub fn columns(&self) -> Vec<&str> {
        let row = &self.row;
        let mut named: Vec<&str> = Vec::new();
        if let AccountSource::Column { column } = &row.account {
            named.push(column);
        }
        for dated in [row.dates.trade.as_ref(), row.dates.cash_posted.as_ref()]
            .into_iter()
            .flatten()
        {
            named.push(&dated.column);
        }
        if let Some(TimeSource::Column { column, .. }) = &row.time {
            named.push(column);
        }
        match &row.amount.carried_by {
            AmountSource::SignedColumn { column } => named.push(column),
            AmountSource::DebitCredit {
                out_column,
                in_column,
            } => {
                named.push(out_column);
                named.push(in_column);
            }
        }
        if let CurrencySource::Column { column, .. } = &row.currency {
            named.push(column);
        }
        if let DirectionSource::Column { column, .. } = &row.direction {
            named.push(column);
        }
        if let Some(far_side) = &row.far_side {
            named.push(far_side.column());
        }
        if let Some(status) = &row.status {
            named.push(&status.column);
        }
        for column in [
            row.counterparty.as_deref(),
            row.description.as_deref(),
            row.source_kind.as_deref(),
            row.source_category.as_deref(),
            row.source_operation_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            named.push(column);
        }
        named.sort_unstable();
        named.dedup();
        named
    }
}

/// How the bytes are read into a table of cells.
///
/// One variant, and the schema has two. An `xlsx` profile is refused at load —
/// see [`load::from_bytes`] — because a workbook cell arrives already typed and
/// what a date format or a decimal shape should mean against a typed cell is
/// not settled by decision 0019.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentShape {
    Csv(CsvShape),
}

/// A delimiter-separated document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvShape {
    pub encoding: Encoding,
    pub delimiter: Delimiter,
    /// The one-based record that carries the headings. Records above it are a
    /// preamble and are not rows.
    pub header_row: u32,
}

/// How the document's bytes spell text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8, with no byte-order mark. A leading mark is **not** removed, so it
    /// lands in the first heading and the profile fails to recognise the
    /// document — which is the visible failure, not a silent one.
    Utf8,
    /// UTF-8 whose leading byte-order mark is removed where the document has
    /// one. Tolerating its absence is not leniency about a cell: the mark is
    /// invisible, an author cannot see whether his institution emits one, and
    /// the two spellings are one document.
    Utf8Bom,
    Windows1251,
}

/// The character between cells, named rather than given.
///
/// Named so that a profile cannot nominate a quote character, a newline or a
/// digit. Quoting is RFC 4180's and is not parameterised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    Comma,
    Semicolon,
    Tab,
}

impl Delimiter {
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Comma => b',',
            Self::Semicolon => b';',
            Self::Tab => b'\t',
        }
    }
}

/// Which cell of a row carries which field of an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowShape {
    pub account: AccountSource,
    pub dates: Dates,
    pub time: Option<TimeSource>,
    pub amount: Amount,
    pub currency: CurrencySource,
    pub direction: DirectionSource,
    pub far_side: Option<FarSideSource>,
    /// Whether the source says the row is a movement it completed. Absent, the
    /// engine reads every row as one — which is what a document that prints no
    /// such column has said.
    pub status: Option<StatusSource>,
    pub counterparty: Option<String>,
    pub description: Option<String>,
    pub source_kind: Option<String>,
    pub source_category: Option<String>,
    pub source_operation_id: Option<String>,
}

/// Whose statement the row is on.
///
/// There is no variant naming an account, and there is no key in the schema
/// that would take one: a profile is a shipped artefact and an account identity
/// belongs to an owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountSource {
    /// The document is one account's statement and does not print which; the
    /// caller declares it.
    Declaration,
    /// The export spans several accounts and prints the identity on every row.
    Column { column: String },
}

/// The day or days the source printed.
///
/// At least one, and where only one is named the engine records it as both:
/// a row with no posted day is a row no money-flow report can place in a month.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dates {
    pub trade: Option<DatedCell>,
    pub cash_posted: Option<DatedCell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatedCell {
    pub column: String,
    pub format: DateFormat,
}

/// A name for exactly one acceptance set, fixed by the engine.
///
/// Not a pattern. A `strptime`-style pattern is a small program whose
/// acceptance set cannot be reviewed by looking at it, and `%m/%d` against
/// `%d/%m` is indistinguishable on the first twelve days of every month — so
/// being wrong produces a wrong date rather than a rejected one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFormat {
    /// `YYYY-MM-DD`.
    IsoDate,
    /// `DD.MM.YYYY`.
    DayMonthYearDot,
    /// `DD/MM/YYYY`.
    DayMonthYearSlash,
    /// `MM/DD/YYYY`.
    MonthDayYearSlash,
}

/// Where the time of day is, when the source prints one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeSource {
    /// The named date cell carries a date and a time separated by a single
    /// space; the date's own format consumes the first half.
    DateCell {
        date_field: DateField,
        format: TimeFormat,
    },
    Column {
        column: String,
        format: TimeFormat,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    Trade,
    CashPosted,
}

/// Twenty-four hour, colon separated.
///
/// No offset and no timezone: converting one would change which day a row falls
/// on, and therefore which month a sum lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormat {
    HourMinute,
    HourMinuteSecond,
}

/// The sum, with the sign the source printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amount {
    pub decimal: DecimalShape,
    pub carried_by: AmountSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountSource {
    /// One column, whose sign is the source's own statement about direction.
    /// The engine transcribes it and never re-signs it from a direction word.
    SignedColumn { column: String },
    /// Two columns of magnitudes. Exactly one must be filled on a row; both
    /// filled, or neither, is a rejection rather than a guess about which the
    /// source meant. This is the one place the engine writes a sign the
    /// document did not print, and the choice was still the source's: it stated
    /// direction by choosing a column.
    DebitCredit {
        out_column: String,
        in_column: String,
    },
}

/// How the source writes a number.
///
/// The group separator can never equal the decimal separator: the schema splits
/// the two branches for exactly that reason, so the contradiction is refused at
/// load rather than discovered on a number that parses to the wrong value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalShape {
    pub decimal_separator: DecimalSeparator,
    pub group_separator: GroupSeparator,
    pub negative: NegativeForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalSeparator {
    Dot,
    Comma,
}

/// What sits between groups of digits, where anything does.
///
/// `Space` covers an ordinary space, a non-breaking space and a narrow
/// non-breaking space alike: which of the three an institution emits is not
/// knowledge a profile author has, and requiring it would make correctness
/// depend on a byte nobody can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSeparator {
    None,
    Space,
    Comma,
    Dot,
    Apostrophe,
}

/// How the source writes a negative number.
///
/// A leading plus is accepted by the engine in every profile and has no key:
/// making its acceptance a choice would be a profile changing what the engine
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegativeForm {
    LeadingMinus,
    TrailingMinus,
    Parentheses,
}

/// Which currency the sum is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrencySource {
    /// The document is single-currency and does not print the currency on a
    /// row. A claim about the document, and a separate branch a reviewer can
    /// see rather than a fallback for a missing column.
    Fixed { code: String },
    Column {
        column: String,
        /// Spelling only. It rewrites what the source printed into a code and
        /// decides nothing; the engine's own validation runs afterwards either
        /// way, so a spelling the map does not cover is still tried as a code
        /// and still refused if it is not one.
        spellings: BTreeMap<String, String>,
    },
}

/// Which way the source said the money went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectionSource {
    /// The sign is the source's own direction word. Available only because the
    /// engine reads the document, and unambiguous because a zero amount is
    /// already refused.
    AmountSign,
    /// A total map with no catch-all. A word the map does not carry rejects the
    /// row and names the word; mapping it to `unknown` was refused, because
    /// `unknown` asserts that the source said *nothing* about direction and
    /// here it said something the profile could not read.
    Column {
        column: String,
        tokens: BTreeMap<String, crate::observation::ObservedDirection>,
    },
}

/// What the source said about whose account is on the far side.
///
/// Absent from a profile, every row of the document is `unstated`, which is
/// what a source that does not make the claim said.
///
/// **Two variants, and the column decides which one a profile may write**
/// (`iaam-b0r0`). Which is not a matter of taste: a source that states the far
/// side in a column of its own prints a closed vocabulary there and
/// [`Self::Tokens`] maps all of it, while a source that states it inside a
/// free-text column prints one fixed sentence of its own beside arbitrary text
/// on every other row, and there is no closed vocabulary to be total over. The
/// second is the shape the first shipped profile needed and could not have: the
/// only branch was the total map, so the one column that carried the claim
/// could not be read, and every movement between the owner's own accounts
/// became a question the document had already answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FarSideSource {
    /// A total map with no catch-all, over a column whose vocabulary is closed.
    /// A word the map does not carry rejects the row and names the word.
    Tokens {
        column: String,
        tokens: BTreeMap<String, crate::classification::FarSide>,
    },
    /// The cells with which this source asserts the far side is the owner's,
    /// over a column whose vocabulary is not closed. A cell equal to one of
    /// them, after trimming, is `own_account`; every other cell is `unstated`.
    ///
    /// **Not the leniency decision 0019's third invariant refuses**, and the
    /// difference is exact: leniency says what becomes of a cell the engine
    /// could not read, and there is no such cell here. Every cell is read, and
    /// a cell that is not one of these words is a cell in which the source made
    /// no claim about the far side — which is what `unstated` means. There is
    /// deliberately no way to write a word meaning `unstated`, so nothing here
    /// can be absent in a way that conceals anything: a word misspelt or gone
    /// stale costs a question that gets asked, never one that stops being
    /// asked.
    ///
    /// Matched by equality after trimming and never as a substring. A substring
    /// test is a predicate, the second invariant admits none, and a
    /// counterparty whose printed name happened to contain the sentence would
    /// be filed as an account of the owner's.
    OwnAccountWords {
        column: String,
        words: BTreeSet<String>,
    },
}

impl FarSideSource {
    /// The column this source reads, whichever branch it is.
    #[must_use]
    pub fn column(&self) -> &str {
        match self {
            Self::Tokens { column, .. } | Self::OwnAccountWords { column, .. } => column,
        }
    }
}

/// Whether the source says the row is a movement it has completed.
///
/// A card export prints a column for it and files an authorisation it is still
/// holding, one it refused, and one that settled under three different words.
/// Without this block a profile cannot read that column, so a row the source
/// itself marks as not final is transcribed and committed like any other, and
/// the journal records money that did not move (`iaam-2hq0`).
///
/// The profile transcribes and stops. What follows a non-final word is the
/// engine's and is not parameterised: the row is refused by name, with its
/// column and its word, and the other rows of the document are read (§10.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSource {
    pub column: String,
    pub tokens: BTreeMap<String, RowStatus>,
}

/// iaam's own three words for what a source said about whether the movement
/// happened.
///
/// **The one profile vocabulary whose values never leave the engine.** No field
/// of an observation carries a status and nothing downstream can read one; the
/// three exist to be the vocabulary a refusal is written from. That is
/// deliberate — a status that survived into the journal would be a fact about
/// the source's own workflow sitting beside facts about the owner's money.
///
/// There is no fourth word, and in particular none for a reversal: a reversal
/// is a movement of its own with its own row and its own sign, not a status on
/// another movement, and a profile that filed a source's reversal word under
/// [`Self::Declined`] would say the money never moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// The source states it moved the money. The only word an observation is
    /// made from.
    Completed,
    /// The source has not moved the money yet and expects to. Refusing it now
    /// is what keeps the journal from holding one movement twice: the source
    /// prints the row again, completed, in a later export.
    Pending,
    /// The source states the money never moved at all.
    Declined,
}

impl RowStatus {
    /// What a refusal says this word meant, in the owner's own reading rather
    /// than in this vocabulary's (ADR 0027).
    #[must_use]
    pub const fn refusal(self) -> &'static str {
        match self {
            // Never reached: the engine returns before it asks. Named all the
            // same rather than joined to a catch-all, so that a fourth word
            // added to this vocabulary has to be given a sentence of its own
            // here instead of inheriting one that is untrue of it.
            Self::Completed => "a movement the source completed",
            Self::Pending => {
                "a payment the source has not taken yet. It prints this row again, \
                 completed, once it has, and reading it now would record the same \
                 money twice"
            }
            Self::Declined => "a payment the source refused. No money moved",
        }
    }
}
