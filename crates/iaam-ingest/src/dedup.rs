//! Idempotency and deduplication (§10.6).
//!
//! Key hierarchy and the order in which keys are selected:
//!
//! | §10.6 | Key | When it applies |
//! |---|---|---|
//! | 1 | `SourceOperationId` | source provided a stable identifier |
//! | 2 | `IdempotencyKey` | client named the submission |
//! | 4 | `DocumentRow` | document **and** row locator are known |
//! | 3 | `NormalizedFingerprint` | document is known, but the locator is not |
//! | 5 | fingerprint hint | matches a record from another document |
//!
//! **The selection order is 1, 2, 4, 3, not 1, 2, 3, 4.** The spec numbers the
//! fingerprint third, but it also explicitly prohibits treating
//! two legitimate identical purchases on the same day as duplicates, while their fingerprints
//! match. One of the two must give way, and the numbering gives way:
//! **within a document, row identity is its locator, not its contents**.
//! The document itself is evidence that there were two operations:
//! the parser saw two rows.
//!
//! This also means that a fingerprint match within the **same**
//! document at another locator is `Fresh`, not even a hint: otherwise
//! a report with two identical purchases would arbitrarily bury its owner in
//! hints. Between different documents, the same
//! fingerprint is a level-five hint that deletes nothing.
//!
//! The natural key “account + date + amount” is not used anywhere: it
//! produces false matches and fails to catch duplicates after normalization.

use iaam_core::event::provenance::RawHash;
use iaam_core::ids::{AccountId, EventId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::journal_event::{JournalFact, SubmittedJournalEvent};
use crate::operation::{OperationDates, OperationKind, SubmittedOperation};

/// Version of the canonical fingerprint form.
///
/// It is part of the form itself: fingerprints have already been deduplicated, and changing
/// the form must be visible, not silent.
const CANONICAL_VERSION: u8 = 1;

/// Hierarchy level §10.6 at which the decision was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DedupLevel {
    SourceOperationId,
    IdempotencyKey,
    NormalizedFingerprint,
    DocumentRow,
    /// Probabilistic estimate. Shown to the owner; deletes nothing.
    Probabilistic,
}

impl DedupLevel {
    /// Level number in §10.6. Needed in the response to the owner: “why did the system
    /// decide this had already occurred?” — this references the specification level.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::SourceOperationId => 1,
            Self::IdempotencyKey => 2,
            Self::NormalizedFingerprint => 3,
            Self::DocumentRow => 4,
            Self::Probabilistic => 5,
        }
    }
}

/// Key by which a row is recognized as already seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupKey {
    SourceOperationId(String),
    IdempotencyKey(String),
    NormalizedFingerprint {
        document: RawHash,
        fingerprint: RawHash,
    },
    DocumentRow {
        document: RawHash,
        sheet: Option<String>,
        row: u64,
    },
}

impl DedupKey {
    #[must_use]
    pub const fn level(&self) -> DedupLevel {
        match self {
            Self::SourceOperationId(_) => DedupLevel::SourceOperationId,
            Self::IdempotencyKey(_) => DedupLevel::IdempotencyKey,
            Self::NormalizedFingerprint { .. } => DedupLevel::NormalizedFingerprint,
            Self::DocumentRow { .. } => DedupLevel::DocumentRow,
        }
    }

    /// Selection order: the lower the value, the stronger the key.
    ///
    /// Deliberately differs from the §10.6 level number — see the module
    /// header. Kept as a separate number so choosing the strongest key is
    /// verifiable rather than a consequence of branch order in `choose_key`.
    #[must_use]
    pub const fn precedence(&self) -> u8 {
        match self {
            Self::SourceOperationId(_) => 1,
            Self::IdempotencyKey(_) => 2,
            Self::DocumentRow { .. } => 3,
            Self::NormalizedFingerprint { .. } => 4,
        }
    }
}

pub use iaam_core::reconciliation::evidence::IdentityScope;

/// Where the row came from and which account supplied it.
///
/// `document: None` — a channel without a file: a broker API response is a stream,
/// not a document, and `None` means exactly that “there was no file.” The account
/// is still required because some channels scope their source identifiers to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentContext {
    pub account: AccountId,
    pub document: Option<RawHash>,
    pub sheet: Option<String>,
    pub row: Option<u64>,
    pub identity_scope: IdentityScope,
}

/// An already recorded fact against which comparison is made.
///
/// Constructed by the wrapper from the log: everything listed below is stored
/// in the event's `provenance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRecord {
    pub event: EventId,
    pub account: AccountId,
    pub source_operation_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub fingerprint: RawHash,
    pub document: Option<RawHash>,
    pub sheet: Option<String>,
    pub row: Option<u64>,
}

/// What to do with the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupDecision {
    /// Already recorded.
    Duplicate { key: DedupKey, existing: EventId },
    /// Not encountered.
    Fresh,
    /// Looks like a duplicate, but there is no evidence.
    ///
    /// **Never** leads to deletion: shown to the owner
    /// together with the recorded row (§10.6).
    PossibleDuplicate { of: EventId, level: DedupLevel },
}

impl DedupDecision {
    /// Whether the row is recorded.
    ///
    /// Exists so that “probabilistic duplicate is not
    /// discarded” is a verifiable property, rather than a promise
    /// in a comment.
    #[must_use]
    pub const fn records_the_row(&self) -> bool {
        match self {
            Self::Fresh | Self::PossibleDuplicate { .. } => true,
            Self::Duplicate { .. } => false,
        }
    }
}

/// Strongest available key. `None` — nothing identifies the row.
#[must_use]
pub fn choose_key(operation: &SubmittedOperation, context: &DocumentContext) -> Option<DedupKey> {
    let mut available: Vec<DedupKey> = Vec::new();
    // The insertion order intentionally does not match the hierarchy: it is defined by
    // `precedence`, not by the arbitrary order in which candidates appear.
    if let Some(document) = context.document.clone() {
        match context.row {
            Some(row) => available.push(DedupKey::DocumentRow {
                document,
                sheet: context.sheet.clone(),
                row,
            }),
            None => available.push(DedupKey::NormalizedFingerprint {
                fingerprint: fingerprint(operation),
                document,
            }),
        }
    }
    if let Some(id) = operation.source_operation_id.as_deref() {
        available.push(DedupKey::SourceOperationId(id.to_owned()));
    }
    if let Some(key) = operation.idempotency_key.as_deref() {
        available.push(DedupKey::IdempotencyKey(key.to_owned()));
    }
    available.sort_by_key(DedupKey::precedence);
    available.into_iter().next()
}

/// Decision for the row.
///
/// Order: an exact match of the selected key — duplicate; otherwise
/// a fingerprint match with a record from **another** document or a channel without
/// a file — hint; otherwise the row is new.
#[must_use]
pub fn assess(
    key: Option<&DedupKey>,
    fingerprint: &RawHash,
    context: &DocumentContext,
    known: &[KnownRecord],
) -> DedupDecision {
    if let Some(key) = key
        && let Some(existing) = known
            .iter()
            .find(|record| matches_key(record, key, context))
    {
        return DedupDecision::Duplicate {
            key: key.clone(),
            existing: existing.event,
        };
    }
    known
        .iter()
        .find(|record| &record.fingerprint == fingerprint && !same_document(record, context))
        .map_or(DedupDecision::Fresh, |record| {
            DedupDecision::PossibleDuplicate {
                of: record.event,
                level: DedupLevel::Probabilistic,
            }
        })
}

/// Whether it has been proven that the record and row came from the same document.
///
/// Only a proven match removes the hint: the document is evidence
/// that there were two operations. Two channels without a file
/// no such evidence is provided, and `None == None` here would mean
/// “both from nowhere, therefore from the same place” — silently allowing
/// a repeated export from the API.
fn same_document(record: &KnownRecord, context: &DocumentContext) -> bool {
    matches!(
        (record.document.as_ref(), context.document.as_ref()),
        (Some(known), Some(incoming)) if known == incoming
    )
}

/// Does the record match the key?
///
/// Exhaustive `match`: a new key type must break the build here,
/// rather than silently stopping duplicate detection.
fn matches_key(record: &KnownRecord, key: &DedupKey, context: &DocumentContext) -> bool {
    match key {
        DedupKey::SourceOperationId(id) => {
            record.source_operation_id.as_deref() == Some(id)
                && match context.identity_scope {
                    IdentityScope::Source => true,
                    IdentityScope::Account => record.account == context.account,
                }
        }
        DedupKey::IdempotencyKey(value) => record.idempotency_key.as_deref() == Some(value),
        DedupKey::NormalizedFingerprint {
            document,
            fingerprint,
        } => record.document.as_ref() == Some(document) && &record.fingerprint == fingerprint,
        DedupKey::DocumentRow {
            document,
            sheet,
            row,
        } => {
            record.document.as_ref() == Some(document)
                && record.sheet.as_deref() == sheet.as_deref()
                && record.row == Some(*row)
        }
    }
}

/// Canonical operation form: the basis for computing the fingerprint.
///
/// The idempotency key and source operation identifier are
/// **not included**: they identify the submission, not the operation. The same
/// operation sent with different keys must produce the same
/// fingerprint — otherwise the third level will catch nothing.
#[must_use]
pub fn canonical_form(operation: &SubmittedOperation) -> String {
    let canonical = Canonical {
        v: CANONICAL_VERSION,
        account: operation.account,
        kind: &operation.kind,
        dates: CanonicalDates::of(operation.dates),
    };
    serde_json::to_string(&canonical).unwrap_or_else(|_| unrepresentable_operation())
}

/// Fingerprint of a normalized record (§10.6, level three).
#[must_use]
pub fn fingerprint(operation: &SubmittedOperation) -> RawHash {
    let digest = Sha256::digest(canonical_form(operation).as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    // Length and alphabet are guaranteed by SHA-256, so parsing cannot
    // fail; but a placeholder must not be substituted on failure —
    // a fingerprint without a hash must not exist.
    RawHash::parse(&hex).unwrap_or_else(|| unreachable_hash())
}

/// Fingerprint of a journal fact (§10.6, level three).
///
/// A separate function rather than one shared with operations: the canonical forms
/// differ, and combining them into one `enum` would make the operation format
/// depend on the appearance of a second family. A fingerprint is a format, and it
/// must not change just because a new input was introduced alongside it.
#[must_use]
pub fn fingerprint_journal_event(submitted: &SubmittedJournalEvent) -> RawHash {
    let digest = Sha256::digest(canonical_journal_form(submitted).as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    RawHash::parse(&hex).unwrap_or_else(|| unreachable_hash())
}

/// Canonical form of a journal fact.
///
/// The idempotency key and the fact identifier in the source are not
/// **included** for the same reason as for operations: they identify
/// the submission, not the fact.
#[must_use]
pub fn canonical_journal_form(submitted: &SubmittedJournalEvent) -> String {
    let canonical = CanonicalJournalEvent {
        v: CANONICAL_VERSION,
        account: submitted.account,
        fact: &submitted.fact,
    };
    serde_json::to_string(&canonical).unwrap_or_else(|_| unrepresentable_operation())
}

/// Canonical form of a journal fact. Dates within the fact itself
/// are also serialized using its own representation: this is how the fact
/// is stored (`iaam-store/src/events.rs`), and a second record of
/// the same fact in another form would differ from the first.
#[derive(Serialize)]
struct CanonicalJournalEvent<'a> {
    v: u8,
    account: AccountId,
    fact: &'a JournalFact,
}

/// Canonical form. Fields are in declaration order—this order is the
/// format, so the structure is separate rather than borrowed from the DTO.
#[derive(Serialize)]
struct Canonical<'a> {
    v: u8,
    account: AccountId,
    kind: &'a OperationKind,
    dates: CanonicalDates,
}

/// Dates in canonical form are ISO 8601 strings.
///
/// The built-in serialization of `time::Date` produces an ordinal date
/// (`[2026, 91]`): it depends on the library's internal representation
/// and is unreadable to humans, while the fingerprint format must be both.
#[derive(Serialize)]
struct CanonicalDates {
    trade: Option<String>,
    settled: Option<String>,
    cash_posted: Option<String>,
    paid: Option<String>,
}

impl CanonicalDates {
    /// `Display` for `time::Date` is an ISO 8601 calendar date, and
    /// it cannot fail, unlike formatting by a format description.
    fn of(dates: OperationDates) -> Self {
        Self {
            trade: dates.trade.map(|day| day.to_string()),
            settled: dates.settled.map(|day| day.to_string()),
            cash_posted: dates.cash_posted.map(|day| day.to_string()),
            paid: dates.paid.map(|day| day.to_string()),
        }
    }
}

/// Separate functions instead of `unwrap`: `unwrap` here would read
/// as “what if?”, although both cases are impossible by construction.
fn unrepresentable_operation() -> ! {
    panic!("an operation consists of numbers, strings, and dates: JSON always represents it")
}

fn unreachable_hash() -> ! {
    panic!("SHA-256 always produces 64 hexadecimal digits")
}
