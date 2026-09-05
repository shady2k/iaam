//! Origin of the fact (§4.1).
//!
//! These data cannot be recovered later, so they are mandatory
//! from the first commit (§16.1).

use serde::{Deserialize, Serialize};

use crate::ids::{ClassificationRuleId, ImportId, ImportSessionId, PrincipalId, SourceId};

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

/// What one of the owner's standing classification rules did to this row.
///
/// **Two values, and the absence of the whole thing is a third state**
/// ([`Provenance::rule_settlement`]). A row an owner reviews is one of a group
/// a single decision of his reached, and the group is defined by the rule — so
/// the fact has to say which rule, or the group can only be found by reading a
/// whole import by eye.
///
/// [`Self::NoRule`] and «nothing recorded» are different claims and are never
/// merged: the first says a reading ran and no rule of his matched, the second
/// says nothing at all. A reader that treats the second as the first tells him
/// a row he never touched was decided by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "settled_by", rename_all = "snake_case")]
pub enum RuleSettlement {
    /// The row was read against the owner's standing rules and none settled it.
    ///
    /// It covers every other way a row can come to be settled — his own answer,
    /// his account directory recognising the far side, the source asserting it,
    /// and a caller that submitted a finished operation. What those have in
    /// common is the only thing this field claims: no rule of his filed the row.
    /// Which of them it was is the reading's own vocabulary and is published by
    /// the import assessment, not here.
    NoRule,
    /// This rule, at this version, settled the row.
    ///
    /// The version is recorded beside the identifier because a rule can be
    /// edited, and «the rows rule R filed» and «the rows version 3 of R filed»
    /// are different questions. Recording only the identifier would make the
    /// second unanswerable, and the second is the one asked after an edit.
    Rule {
        rule: ClassificationRuleId,
        version: u32,
    },
}

impl RuleSettlement {
    /// Wire code. One place, so two publishers cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoRule => "no_rule",
            Self::Rule { .. } => "rule",
        }
    }

    /// The rule and the version, where a rule settled the row.
    #[must_use]
    pub const fn rule(&self) -> Option<(ClassificationRuleId, u32)> {
        match self {
            Self::NoRule => None,
            Self::Rule { rule, version } => Some((*rule, *version)),
        }
    }
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
    /// The source's own word for **what the operation was**.
    ///
    /// A different fact from [`Self::source_category`] beside it, and the two
    /// are never written through one slot. This one is the word a bank prints
    /// in its operation-type column — "transfer", "card payment" — and it says
    /// what happened; the category says what the money was *for*. A rule the
    /// owner writes on one must not fire on the other, which is exactly what
    /// happened while the observation path carried this word in the category's
    /// field (`iaam-p683`).
    ///
    /// Evidence, like every field around it, and never rewritten: it is what
    /// the direction the row was read with was read *from*, so a wrong reading
    /// stays visible against the word it was made from.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field. `None` therefore means "not
    /// recorded", which covers both a source that printed no such word and
    /// every fact written before schema version 14 — including the observed
    /// rows whose operation word went into `source_category`. Nothing rewrites
    /// those; what they carry is what the observation path meant at the time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_kind: Option<String>,
    /// The category the **owner himself** filed the row under, at the source.
    ///
    /// A third word beside [`Self::source_category`] and [`Self::source_kind`],
    /// and a different fact from both: those two are the institution's, this
    /// one is a decision the owner took in the institution's own app which the
    /// export prints back. It is the answer he was being asked for once per
    /// row, and it is retained here so that a rule of his written on it goes on
    /// matching after the fact is recorded — evidence dropped at commit makes
    /// recomputation reconsider the row as one whose source said nothing.
    ///
    /// Evidence, like every field around it, and never rewritten. Nothing maps
    /// it into one of the owner's categories here: it is his decision in his
    /// bank's vocabulary, and what it is called here is a question of its own.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field. `None` means "not recorded",
    /// which covers a source that printed no such column and every fact written
    /// before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_category: Option<String>,
    /// The standardised code the source printed for the row.
    ///
    /// The one word among these that is not one institution's private
    /// vocabulary: it is assigned by the payment network, so a rule written on
    /// it holds across institutions. Retained as text and never as a number —
    /// it is an identifier printed with leading zeros.
    ///
    /// `#[serde(default)]` for [`Self::owner_category`]'s reason, and `None`
    /// also means the source assigned the row no code, which it does on rows
    /// that are not a purchase from a merchant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_code: Option<String>,
    /// The description or counterparty the source printed on the row.
    ///
    /// Evidence about what the source said, exactly like `source_category`
    /// beside it, and never rewritten. It is what a description rule matches
    /// when the source's own category is too coarse to separate two different
    /// meanings — a bank filing both a transfer to one's own account and a
    /// utility payment under one word.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// The import this row arrived in, when the caller named one.
    ///
    /// Beside the source rather than inside it. The source is what
    /// deduplication is scoped by — a source operation identifier is unique
    /// within a source (§10.6) — so narrowing the source to one submission
    /// would stop the same bank's identifiers being compared across two of its
    /// own exports. Retraction needs the narrower handle, and it is this one.
    ///
    /// `None` means the submission named no import: rows ingested before this
    /// field existed, and rows from channels that declare no source at all.
    /// They are retracted as one unnamed group, which is what they are.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import: Option<ImportId>,
    /// The credential the fact was submitted under.
    ///
    /// Provenance already answers «which software wrote this» through
    /// [`ParserVersion`]; this answers «which credential presented it», which is
    /// the same axis and the one thing about a submission that cannot be
    /// recovered afterwards. A token's scope is checked at the door and then
    /// forgotten, so without this field the journal can say what an act was and
    /// never who performed it.
    ///
    /// It is here rather than in a table beside the journal for the reason
    /// every other origin field is: a second place recording where a fact came
    /// from is a second place that can disagree with the fact.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field. `None` therefore means «not
    /// recorded», never «submitted by nobody», and every rule that reads it
    /// must refuse on `None` rather than assume — the whole point of the field
    /// is that a missing declaration is not evidence of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    declared_by: Option<PrincipalId>,
    /// The import session this fact was committed out of.
    ///
    /// Beside [`Provenance::import`] rather than derived from it, because the
    /// import does not name a session and cannot be made to. The import is
    /// derived from the declaration — owner, account, channel, label — while a
    /// session identifier is minted per session, and the store admits one
    /// **open** session per import rather than one session per import: a label
    /// committed and then declared again opens a second session, so an import
    /// names as many sessions as it was ever imported under. Rows carrying an
    /// import need not have passed through a session at all — `/v1/ingest/operations`
    /// stamps the import and opens nothing — and a session opened without a
    /// declared import commits rows that carry no import whatsoever. There is
    /// no half of the question the import answers.
    ///
    /// What it buys is the step [`crate::event::Event::provenance`] could not
    /// take before: a figure in a report leads back to the rows it was folded
    /// from, the rows lead back to the source that printed them, and this leads
    /// back to the act that admitted them — the assessment that was read, the
    /// questions that were answered, the control figures that were compared.
    /// Those are stored beside the session and were reachable from nothing the
    /// journal held.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field. `None` therefore means «no
    /// session is recorded», which covers both a fact that came through no
    /// session and a fact committed by one before this field existed. The two
    /// are not separable and a rule must not try: as with [`Self::declared_by`],
    /// a missing session is not evidence of its absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import_session: Option<ImportSessionId>,
    /// Which of the owner's standing classification rules settled this row.
    ///
    /// The one thing about a fact that nothing else here can answer: every
    /// other field says what the source printed or which act admitted the row,
    /// and none of them says **why it was filed the way it was**. A rule made
    /// from one answer applies to a group of rows the owner never saw one by
    /// one, and reviewing that group means finding it — which, without this
    /// field, means reading a whole import by eye.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field. `None` therefore means «not
    /// recorded» and never «no rule settled it» — the second is
    /// [`RuleSettlement::NoRule`], which is a statement a reading made. Absence
    /// covers every fact written before this field existed and every path that
    /// writes without reading a row against the rules at all: a correction
    /// minted by this code, a broker synchronisation. A caller that reads the
    /// absence as «he decided this one himself» is stating something the
    /// journal never said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rule_settlement: Option<RuleSettlement>,
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
            source_kind: None,
            owner_category: None,
            source_code: None,
            description: None,
            import: None,
            declared_by: None,
            import_session: None,
            rule_settlement: None,
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
    pub fn with_source_kind(mut self, kind: impl Into<String>) -> Self {
        self.source_kind = Some(kind.into());
        self
    }

    #[must_use]
    pub fn with_owner_category(mut self, category: impl Into<String>) -> Self {
        self.owner_category = Some(category.into());
        self
    }

    #[must_use]
    pub fn with_source_code(mut self, code: impl Into<String>) -> Self {
        self.source_code = Some(code.into());
        self
    }

    /// Replace the raw hash with the digest of the document the row came off.
    ///
    /// The one legitimate caller is the document channel. `normalize` has no
    /// document, so it fingerprints the row's own canonical form; a report row
    /// is identified by the document and its locator instead (§10.6, level 4),
    /// and that digest is only known to the caller that opened the file.
    ///
    /// It replaces the hash and nothing else, which is the whole reason it
    /// exists: rebuilding the provenance to change one field silently drops
    /// every other field the normaliser had already filled.
    #[must_use]
    pub fn with_raw_hash(mut self, raw_hash: RawHash) -> Self {
        self.raw_hash = raw_hash;
        self
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn with_import(mut self, import: ImportId) -> Self {
        self.import = Some(import);
        self
    }

    /// Stamp the credential this fact was submitted under.
    ///
    /// Applied after normalisation rather than carried in the normalisation
    /// context, for the reason the import is: normalisation decides what a row
    /// *is*, and who presented it decides nothing about that.
    #[must_use]
    pub const fn with_declared_by(mut self, principal: PrincipalId) -> Self {
        self.declared_by = Some(principal);
        self
    }

    /// Stamp the import session this fact is being committed out of.
    ///
    /// Applied after normalisation for the reason the import and the declaring
    /// principal are: normalisation decides what a row *is*, and the act that
    /// admitted it decides nothing about that.
    #[must_use]
    pub const fn with_import_session(mut self, session: ImportSessionId) -> Self {
        self.import_session = Some(session);
        self
    }

    /// Stamp what the owner's standing rules made of this row.
    ///
    /// Applied after normalisation, like the import session and the declaring
    /// principal, and for a reason of its own: normalisation turns a settled
    /// operation into a fact, and by then the reading that settled it has
    /// already happened somewhere the normaliser cannot see. Only the caller
    /// that ran the reading knows whether a rule was involved, so only it may
    /// say — and a path that never read the row against the rules says nothing,
    /// which is what the absence means.
    #[must_use]
    pub const fn with_rule_settlement(mut self, settlement: RuleSettlement) -> Self {
        self.rule_settlement = Some(settlement);
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

    /// The import this row arrived in, or `None` when the submission named
    /// none. The key an import correction is decided by.
    #[must_use]
    pub const fn import(&self) -> Option<ImportId> {
        self.import
    }

    /// The credential this fact was submitted under, when one was recorded.
    ///
    /// `None` for everything written before the field existed and for every
    /// path that appends without a caller behind it — a broker synchronisation,
    /// a correction minted by this code. A caller that reads `None` as «mine»
    /// would hand every unattributed fact to whoever asked first.
    #[must_use]
    pub const fn declared_by(&self) -> Option<PrincipalId> {
        self.declared_by
    }

    /// The import session this fact was committed out of, when one is
    /// recorded.
    ///
    /// `None` for every path that appends without a session behind it — a
    /// direct operation submission, a broker synchronisation, a correction
    /// minted by this code — and for everything written before the field
    /// existed. A caller that read `None` as «no session exists» would be
    /// stating something the journal never said.
    #[must_use]
    pub const fn import_session(&self) -> Option<ImportSessionId> {
        self.import_session
    }

    /// What the owner's standing rules made of this row, when a reading said.
    ///
    /// `None` is «not recorded» and must never be read as «no rule settled it»:
    /// that one is [`RuleSettlement::NoRule`], and the difference is the whole
    /// point of the field being an option over an enumeration rather than an
    /// option over a rule identifier.
    #[must_use]
    pub const fn rule_settlement(&self) -> Option<&RuleSettlement> {
        self.rule_settlement.as_ref()
    }

    /// The rule and version that settled this row, where one did.
    ///
    /// Answers `None` for both of the other two states, so a caller that only
    /// wants to know «which rule» need not distinguish them — and a caller that
    /// must distinguish them reads [`Self::rule_settlement`] instead.
    #[must_use]
    pub const fn settling_rule(&self) -> Option<(ClassificationRuleId, u32)> {
        match self.rule_settlement {
            Some(settlement) => settlement.rule(),
            None => None,
        }
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

    /// The source's own word for what the operation was, when it printed one.
    ///
    /// Read by classification, never by the category rules: those read
    /// [`Self::source_category`], and the two answer different questions.
    #[must_use]
    pub fn source_kind(&self) -> Option<&str> {
        self.source_kind.as_deref()
    }

    /// The category the owner himself filed the row under at the source, when
    /// the source printed one.
    ///
    /// Read by both rule vocabularies and by neither as the other's field: a
    /// word of his is not a word of the institution's, and the two are compared
    /// separately or a rule fires on rows he was not describing.
    #[must_use]
    pub fn owner_category(&self) -> Option<&str> {
        self.owner_category.as_deref()
    }

    /// The standardised code the source printed, when it printed one.
    #[must_use]
    pub fn source_code(&self) -> Option<&str> {
        self.source_code.as_deref()
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub const fn row(&self) -> Option<&RowLocator> {
        self.row.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AccountId, ClassificationRuleId, OwnerId};

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
    #[test]
    fn a_description_is_kept_and_read_back() {
        let provenance = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_description("Corner Shop");

        assert_eq!(provenance.description(), Some("Corner Shop"));
    }

    #[test]
    fn the_declaring_principal_is_kept_and_read_back() {
        let principal = PrincipalId::new_random();
        let provenance = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_declared_by(principal);

        assert_eq!(provenance.declared_by(), Some(principal));
    }

    #[test]
    fn provenance_written_before_a_declarer_was_recorded_names_none() {
        // The load-bearing half of the field: a fact that names no declarer must
        // read as «not recorded», so a rule of the form «you may undo what you
        // declared» refuses it instead of handing it to whoever asks first.
        let stored = r#"{"source":"00000000-0000-0000-0000-000000000000",
        "raw_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "parser_version":"test"}"#;
        let provenance: Provenance = serde_json::from_str(stored).expect("older provenance");

        assert_eq!(provenance.declared_by(), None);
    }

    #[test]
    fn the_committing_import_session_is_kept_and_read_back() {
        let session = ImportSessionId::new_random();
        let provenance = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_import_session(session);

        assert_eq!(provenance.import_session(), Some(session));
    }

    #[test]
    fn an_import_session_is_recorded_apart_from_the_import() {
        // The two answer different questions and neither implies the other: an
        // import may be imported under several sessions, and a session need not
        // declare an import at all. A fact carrying one and not the other is
        // therefore an ordinary fact, not a half-written one.
        let import = ImportId::declared(
            OwnerId::new_random(),
            AccountId::new_random(),
            "file",
            "january",
        );
        let with_import_only = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_import(import);
        assert_eq!(with_import_only.import(), Some(import));
        assert_eq!(with_import_only.import_session(), None);

        let session = ImportSessionId::new_random();
        let with_session_only = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_import_session(session);
        assert_eq!(with_session_only.import(), None);
        assert_eq!(with_session_only.import_session(), Some(session));
    }

    #[test]
    fn provenance_written_before_an_import_session_was_recorded_names_none() {
        // The load-bearing half: an event from before the field existed must
        // read as «no session recorded», so a caller asking what produced a
        // figure is told nothing rather than told the wrong session.
        let stored = r#"{"source":"00000000-0000-0000-0000-000000000000",
        "raw_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "parser_version":"test"}"#;
        let provenance: Provenance = serde_json::from_str(stored).expect("older provenance");

        assert_eq!(provenance.import_session(), None);
    }

    #[test]
    fn the_rule_that_settled_the_row_is_kept_and_read_back() {
        let rule = ClassificationRuleId::new_random();
        let provenance = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_rule_settlement(RuleSettlement::Rule { rule, version: 3 });

        assert_eq!(
            provenance.rule_settlement(),
            Some(&RuleSettlement::Rule { rule, version: 3 })
        );
        assert_eq!(provenance.settling_rule(), Some((rule, 3)));
    }

    #[test]
    fn a_row_no_rule_settled_is_not_a_row_whose_rule_was_never_recorded() {
        // The three states this field exists for. A fact that says «no rule» was
        // read by a build that looks for one and found none; a fact that says
        // nothing was written by a path that never asked, or before the field
        // existed. Reading the second as the first would tell the owner that a
        // row a rule of his filed was decided by hand.
        let no_rule = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_rule_settlement(RuleSettlement::NoRule);
        let unrecorded = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        );

        assert_eq!(no_rule.rule_settlement(), Some(&RuleSettlement::NoRule));
        assert_eq!(no_rule.settling_rule(), None);
        assert_eq!(unrecorded.rule_settlement(), None);
        assert_eq!(unrecorded.settling_rule(), None);
    }

    #[test]
    fn provenance_written_before_a_rule_settlement_was_recorded_names_nothing() {
        // The load-bearing half: an event from before the field existed must read
        // as «not recorded», never as «no rule settled it». The journal is
        // append-only and nothing rewrites such a fact, so the absence is the
        // whole of what it can say.
        let stored = r#"{"source":"00000000-0000-0000-0000-000000000000",
        "raw_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "parser_version":"test"}"#;
        let provenance: Provenance = serde_json::from_str(stored).expect("older provenance");

        assert_eq!(provenance.rule_settlement(), None);
    }

    #[test]
    fn a_recorded_rule_settlement_survives_a_round_trip() {
        let rule = ClassificationRuleId::new_random();
        let provenance = Provenance::new(
            SourceId::new_random(),
            hash("a"),
            ParserVersion("test".to_owned()),
        )
        .with_rule_settlement(RuleSettlement::Rule { rule, version: 7 });

        let stored = serde_json::to_string(&provenance).expect("provenance encodes");
        let read: Provenance = serde_json::from_str(&stored).expect("provenance decodes");

        assert_eq!(read.settling_rule(), Some((rule, 7)));
    }

    #[test]
    fn provenance_recorded_before_the_description_existed_still_reads() {
        // The journal is append-only: a fact written under an older schema must
        // stay readable, or the field cannot be added at all.
        let stored = r#"{"source":"00000000-0000-0000-0000-000000000000",
        "raw_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "parser_version":"test"}"#;
        let provenance: Provenance = serde_json::from_str(stored).expect("older provenance");

        assert_eq!(provenance.description(), None);
    }
}
