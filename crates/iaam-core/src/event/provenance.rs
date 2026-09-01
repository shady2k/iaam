//! Origin of the fact (§4.1).
//!
//! These data cannot be recovered later, so they are mandatory
//! from the first commit (§16.1).

use serde::{Deserialize, Serialize};

use crate::ids::SourceId;

/// Hash of the raw source record. A hexadecimal SHA-256 string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RawHash(String);

impl RawHash {
    /// Accepts only a valid hexadecimal SHA-256.
    ///
    /// Validation logic lives here, not in a constructor named `new`:
    /// `cargo-mutants` silently skips functions with this name, so validation
    /// of the hash format would remain invisible to the mutation gate.
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| Self(hex.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Version of the parser that produced the fact. Without it, a source
/// error cannot be distinguished from a parsing error fixed in a later version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ParserVersion(pub String);

/// Reference to a specific line in the source document.
///
/// The document is identified by its hash, not its filename: the name is not
/// its identity — the same report saved under another name would no longer
/// be deduplicated (§10.6, level 4). The human-readable document name
/// is stored alongside the raw data and resolved by this hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowLocator {
    pub document: RawHash,
    pub sheet: Option<String>,
    pub row: u64,
}

/// Provenance. Cannot be constructed without a raw data hash and parser version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    source: SourceId,
    raw_hash: RawHash,
    parser_version: ParserVersion,
    source_operation_id: Option<String>,
    /// The category the source itself assigned to the row.
    ///
    /// Retained separately from any owner category and never rewritten. It is
    /// evidence about what the source said, not a decision: a bank calling a
    /// subscription "Развлечения" is a hint the owner may map or override, and
    /// storing it as the owner's own category would let the bank decide what
    /// his spending was.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_category: Option<String>,
    row: Option<RowLocator>,
}

impl Provenance {
    /// Trivial field packaging: there is nothing to validate during construction; the required
    /// hash and parser version are enforced by the signature itself, not the body. There is no logic
    /// worth moving out of `cargo-mutants`' blind spot around the name `new`,
    /// here (cf. [`crate::money::Money::new`]).
    #[must_use]
    pub fn new(source: SourceId, raw_hash: RawHash, parser_version: ParserVersion) -> Self {
        Self {
            source,
            raw_hash,
            parser_version,
            source_operation_id: None,
            source_category: None,
            row: None,
        }
    }

    #[must_use]
    pub fn with_source_operation_id(mut self, id: impl Into<String>) -> Self {
        self.source_operation_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_source_category(mut self, category: impl Into<String>) -> Self {
        self.source_category = Some(category.into());
        self
    }

    #[must_use]
    pub fn with_row(mut self, row: RowLocator) -> Self {
        self.row = Some(row);
        self
    }

    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.source
    }

    #[must_use]
    pub const fn raw_hash(&self) -> &RawHash {
        &self.raw_hash
    }

    /// Parser version. Readable alongside the hash: provenance from which
    /// the parser version cannot be retrieved does not answer «what parsed this».
    #[must_use]
    pub const fn parser_version(&self) -> &ParserVersion {
        &self.parser_version
    }

    #[must_use]
    pub fn source_operation_id(&self) -> Option<&str> {
        self.source_operation_id.as_deref()
    }

    #[must_use]
    pub fn source_category(&self) -> Option<&str> {
        self.source_category.as_deref()
    }

    #[must_use]
    pub const fn row(&self) -> Option<&RowLocator> {
        self.row.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(seed: &str) -> RawHash {
        RawHash::parse(&seed.repeat(64)).unwrap()
    }

    #[test]
    fn raw_hash_rejects_malformed_input() {
        assert!(RawHash::parse("не хеш").is_none());
        assert!(RawHash::parse("abc").is_none());
        assert!(RawHash::parse(&"a".repeat(64)).is_some());
    }

    #[test]
    fn raw_hash_rejects_the_right_length_with_a_wrong_character() {
        // 64 characters, but not hexadecimal: length alone is not enough.
        let mut s = "a".repeat(63);
        s.push('z');
        assert_eq!(s.len(), 64);
        assert!(RawHash::parse(&s).is_none());
    }

    #[test]
    fn raw_hash_rejects_a_hash_that_is_one_character_too_long() {
        assert!(RawHash::parse(&"a".repeat(65)).is_none());
    }

    #[test]
    fn raw_hash_is_normalised_to_lowercase() {
        let h = RawHash::parse(&"A".repeat(64)).unwrap();
        assert_eq!(h.as_str(), "a".repeat(64));
    }

    #[test]
    fn provenance_keeps_the_source_hash_and_parser_version() {
        let source = SourceId::new_random();
        let p = Provenance::new(
            source,
            hash("a"),
            ParserVersion("tinkoff-xlsx/3".to_owned()),
        );
        assert_eq!(p.source(), source);
        assert_eq!(p.raw_hash(), &hash("a"));
        assert_eq!(
            p.parser_version(),
            &ParserVersion("tinkoff-xlsx/3".to_owned())
        );
    }

    #[test]
    fn optional_provenance_details_are_absent_until_set() {
        // Unknown is None, not an empty string (§4.9).
        let p = Provenance::new(
            SourceId::new_random(),
            hash("b"),
            ParserVersion("manual/1".to_owned()),
        );
        assert_eq!(p.source_operation_id(), None);
        assert_eq!(p.row(), None);
    }

    #[test]
    fn source_operation_id_is_recorded_when_given() {
        let p = Provenance::new(
            SourceId::new_random(),
            hash("c"),
            ParserVersion("manual/1".to_owned()),
        )
        .with_source_operation_id("OP-4417");
        assert_eq!(p.source_operation_id(), Some("OP-4417"));
    }

    #[test]
    fn row_locator_points_at_the_exact_line_of_the_document() {
        let row = RowLocator {
            document: hash("e"),
            sheet: Some("Сделки".to_owned()),
            row: 118,
        };
        let p = Provenance::new(
            SourceId::new_random(),
            hash("d"),
            ParserVersion("manual/1".to_owned()),
        )
        .with_row(row.clone());
        assert_eq!(p.row(), Some(&row));
    }
}
