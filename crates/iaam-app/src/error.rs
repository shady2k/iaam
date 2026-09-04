//! Scenario errors.
//!
//! Distinction under §15.2: incomplete data is not an error and goes
//! into the report as a quality block; an invariant violation aborts the report and goes
//! into the log with a correlation identifier.

use crate::actions::{InputAlternative, ResolutionOption};
use iaam_core::event::correction::CorrectionError;
use iaam_core::money::MoneyError;
use iaam_core::perimeter::PerimeterError;
use iaam_core::projection::ProjectionError;
use iaam_core::projection::active_instruments::ActiveInstrumentsError;
use iaam_core::projection::money_flow::MoneyFlowError;
use iaam_core::reconciliation::observed::ObserveError;
use iaam_core::report::assets::AssetSnapshotError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("store unavailable: {0}")]
    Store(String),
    #[error("category group {id} is retired")]
    CategoryGroupRetired { id: String },
    #[error("not found: {what} {id}")]
    NotFound { what: &'static str, id: String },
    #[error("request is invalid: field {field}, expected {expected}, received {actual}")]
    Invalid {
        field: String,
        expected: String,
        actual: String,
    },
    /// The same refusal as [`Self::Invalid`], for a field the server can say
    /// more about than that it was wrong: the closed set of values it admits,
    /// the call that produces an acceptable one, or both.
    ///
    /// A second variant rather than two more fields on `Invalid`, because
    /// almost every one of the ninety-odd places that raise `Invalid` has
    /// neither to offer, and putting the lists there would make all of them
    /// declare two empty vectors in order to say nothing. Both carry the same
    /// `invalid_request` code and the same status: the split records what the
    /// server knows about the field, not a difference in what happened.
    ///
    /// Boxed because the payload is four times the size of the largest other
    /// variant, and every `Result<_, AppError>` in the crate would carry that
    /// width on its successful path.
    #[error(
        "request is invalid: field {}, expected {}, received {}",
        .0.field, .0.expected, .0.actual
    )]
    InvalidField(Box<FieldRejection>),
    #[error("internal invariant violated, correlation identifier {correlation}")]
    Invariant {
        correlation: Uuid,
        #[source]
        source: ProjectionError,
    },
    /// Reference data invariant violated: a code resolves to more than one
    /// instrument, meaning that the trigger has fired
    /// `instrument_aliases_do_not_overlap`.
    /// Separate from `Invariant`, because that carries a source from the projection domain
    /// projection: putting any variant there merely to satisfy the signature would
    /// send an investigator to inspect snapshots instead of the reference data schema.
    #[error("reference data invariant violated, correlation identifier {correlation}: {detail}")]
    DirectoryInvariant { correlation: Uuid, detail: String },
    #[error("projection not built: {0}")]
    Projection(#[source] ProjectionError),
    #[error("money flow not built: {0}")]
    MoneyFlow(#[from] MoneyFlowError),
    /// A journal slice is unsuitable for reconciliation: an undated event,
    /// a balance overflow. Separate from `Projection`, because
    /// these are different causes for an external agent: one means an invalid
    /// slice, the other — an inability to verify the data.
    #[error("reconciliation not built: {0}")]
    Reconciliation(#[source] ObserveError),
    #[error("perimeter not assessed: {0}")]
    Perimeter(#[source] PerimeterError),
    /// The journal's corrections do not resolve: a reversal or replacement points at
    /// an event the slice does not contain, or one event is corrected twice. Separate
    /// from `Reconciliation`, because that names an inability to verify data, while
    /// this names a journal whose own links are inconsistent — and reporting it as
    /// «reconciliation not built» would send an investigator to the reconciliation
    /// register instead of the corrections in the journal.
    #[error("journal corrections do not resolve: {0}")]
    Correction(#[source] CorrectionError),
    /// The capability is not enabled by configuration, not broken: encryption
    /// of access to the broker without a key. Separate from `Store`, because
    /// these are different causes for an external agent: one is fixed by configuring
    /// the server, the other — by retrying the request.
    #[error("{what} is not configured")]
    NotConfigured { what: &'static str },
    /// The system source of randomness has failed. Separate from `Store`,
    /// because this is not a store failure and is not fixed by retrying the request,
    /// but by correcting the machine state; it is also separate because a secret,
    /// obtained by unknown means, must never be issued under any circumstances (§14).
    #[error("randomness source unavailable: {0}")]
    Random(String),
    /// The record already exists, and a second identical one would mean that it is unclear,
    /// which one is being used. Separate from `Store`, because it is fixed
    /// not by retrying the request, but by revoking the active record.
    #[error("{what}")]
    Conflict { what: String },
    /// There is no way to build the synchronisation schedule: deriving active securities
    /// from the journal failed numerically or because the journal's corrections were invalid.
    /// Separate from `Reconciliation`, because reconciliation is irrelevant here, while
    /// “reconciliation not built” would send an investigator to inspect the register instead
    /// of the quantity journal.
    #[error("synchronisation schedule not built: {0}")]
    Schedule(#[source] ActiveInstrumentsError),
    /// The asset snapshot did not fold: a currency mixed inside one total, or a
    /// decimal overflow while adding rows. Separate from `Projection`, because
    /// the projection succeeded — it is the summation over its rows that did
    /// not, and reporting it as a projection failure would send an investigator
    /// to the journal rather than to the totals.
    #[error("asset snapshot not built: {0}")]
    AssetSnapshot(#[source] AssetSnapshotError),
    /// An import batch did not total: adding one account's rows overflowed.
    /// Separate from `AssetSnapshot`, which names the same kind of failure over
    /// a different fold — reporting a batch that would not add up as a snapshot
    /// failure would send an investigator to the valuation report rather than to
    /// the rows a session is holding. Separate from `Invalid`, because nothing
    /// the caller sent is wrong: each row states an amount the currency admits,
    /// and it is their sum that does not fit.
    #[error("import batch not totalled: {0}")]
    BatchTotal(#[source] MoneyError),
}

/// A rejected request field, and everything the server can say about it.
///
/// The three prose members are exactly what [`AppError::Invalid`] carries, so a
/// caller reading a rejection reads one shape whichever variant raised it. What
/// this type adds is the two machine-readable halves, and both are deliberately
/// borrowed from the action queue rather than invented here: `alternatives` is
/// the [`InputAlternative`] a [`crate::actions::MissingInput`] publishes, and
/// `resolutions` is the [`ResolutionOption`] an action's target publishes. A
/// client that already reads the queue needs no second reader for a refusal,
/// and a field whose admissible values appear in both cannot describe them two
/// ways.
///
/// Both lists are allowed to be empty, and empty is the honest answer nearly
/// everywhere: most fields are not a closed choice, and almost no refusal has a
/// call that would fix it. A shape that promised either everywhere would have
/// to fabricate one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRejection {
    /// The field, in the dotted-and-indexed form a person reads:
    /// `event[3].schema_version`.
    pub field: String,
    /// What the field admits, in prose. Kept even where `alternatives` says the
    /// same thing in values: the sentence is what the error message quotes.
    pub expected: String,
    /// What arrived. Never more of the request than the named field.
    pub actual: String,
    /// The literal values this field admits, when it admits a closed set.
    ///
    /// Empty means the field is not a choice — a date, a title, an amount — and
    /// says nothing about what may be written there.
    pub alternatives: Vec<InputAlternative>,
    /// Calls that would produce a value this field accepts.
    ///
    /// Empty is the ordinary case. A rejection carries one only where the
    /// remedy genuinely is another request rather than another value — a commit
    /// refused for an unanswered question is the shape this exists for.
    pub resolutions: Vec<ResolutionOption>,
}

impl FieldRejection {
    #[must_use]
    pub fn new(
        field: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            expected: expected.into(),
            actual: actual.into(),
            alternatives: Vec::new(),
            resolutions: Vec::new(),
        }
    }

    /// Attach the closed set of values the field admits.
    #[must_use]
    pub fn admitting(mut self, alternatives: Vec<InputAlternative>) -> Self {
        self.alternatives = alternatives;
        self
    }

    /// Attach the closed set of values the field admits, where none of them
    /// requires anything further.
    ///
    /// The common case, and worth its own constructor: written through
    /// [`Self::admitting`] every plain vocabulary would repeat an empty
    /// `requires` per value, which reads as though the emptiness were a
    /// decision taken value by value rather than a property of the field.
    #[must_use]
    pub fn admitting_codes(self, codes: &[&str]) -> Self {
        self.admitting(
            codes
                .iter()
                .map(|code| InputAlternative {
                    value: (*code).to_owned(),
                    requires: Vec::new(),
                })
                .collect(),
        )
    }

    /// Attach the calls that would produce an acceptable value.
    #[must_use]
    pub fn resolved_by(mut self, resolutions: Vec<ResolutionOption>) -> Self {
        self.resolutions = resolutions;
        self
    }

    /// The JSON pointer to the rejected field.
    #[must_use]
    pub fn pointer(&self) -> String {
        json_pointer(&self.field)
    }
}

impl From<FieldRejection> for AppError {
    fn from(rejection: FieldRejection) -> Self {
        Self::InvalidField(Box::new(rejection))
    }
}

/// Transcribe a field path into an RFC 6901 JSON pointer.
///
/// `event[3].schema_version` becomes `/event/3/schema_version`. The dotted form
/// is what a person reads and what the error message quotes; the pointer is what
/// a client applies to the body it just sent, with no parsing of its own and no
/// guessing about whether `[3]` addresses an index or a key called `[3]`.
///
/// A transcription, not an interpretation: the pointer is exactly as accurate as
/// the field name it is derived from, and where a caller named something that is
/// not a body member — a token's scope, a query parameter — the pointer says the
/// same thing the field says. Deriving it here rather than writing it at each
/// refusal is the point: the two cannot come to disagree.
///
/// Escaping is RFC 6901 §3, and it is not decoration. A field whose name
/// contains `/` — a header, a media type — would otherwise transcribe into a
/// pointer addressing two members that do not exist.
#[must_use]
pub fn json_pointer(field: &str) -> String {
    let mut pointer = String::with_capacity(field.len() + 1);
    // Splitting on all three separators at once, then dropping what falls out
    // empty, is what makes `event[3]` and `event.3` transcribe alike: the
    // closing bracket leaves an empty token behind it, and an empty reference
    // token addresses a member named "", which is not what was meant.
    for token in field
        .split(['.', '[', ']'])
        .filter(|token| !token.is_empty())
    {
        pointer.push('/');
        for character in token.chars() {
            match character {
                '~' => pointer.push_str("~0"),
                '/' => pointer.push_str("~1"),
                _ => pointer.push(character),
            }
        }
    }
    pointer
}

impl From<ObserveError> for AppError {
    fn from(error: ObserveError) -> Self {
        Self::Reconciliation(error)
    }
}

impl From<PerimeterError> for AppError {
    fn from(error: PerimeterError) -> Self {
        Self::Perimeter(error)
    }
}

impl AppError {
    /// A projection is converted into an application error so that an invariant
    /// violation cannot be confused with an ordinary failure: the former
    /// gets a correlation identifier for the logs (§15.2).
    #[must_use]
    pub fn from_projection(error: ProjectionError) -> Self {
        if error.is_invariant_violation() {
            Self::Invariant {
                correlation: Uuid::new_v4(),
                source: error,
            }
        } else {
            Self::Projection(error)
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Store(_) => "store_unavailable",
            Self::CategoryGroupRetired { .. } => "category_group_retired",
            Self::NotFound { .. } => "not_found",
            Self::Invalid { .. } | Self::InvalidField(_) => "invalid_request",
            Self::Invariant { .. } => "invariant_violated",
            Self::DirectoryInvariant { .. } => "directory_invariant_violated",
            Self::Projection(_) => "projection_failed",
            Self::MoneyFlow(_) => "money_flow_failed",
            Self::Reconciliation(_) => "reconciliation_failed",
            Self::Schedule(_) => "schedule_not_built",
            Self::AssetSnapshot(_) => "asset_snapshot_not_built",
            Self::BatchTotal(_) => "batch_not_totalled",
            Self::Perimeter(_) => "perimeter_assessment_failed",
            Self::Correction(_) => "corrections_do_not_resolve",
            Self::NotConfigured { .. } => "not_configured",
            Self::Random(_) => "random_unavailable",
            Self::Conflict { .. } => "already_exists",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6901 §3: a pointer is a sequence of `/`-prefixed reference tokens,
    /// and inside a token only `~0` and `~1` may follow a tilde.
    fn is_rfc6901_pointer(pointer: &str) -> bool {
        if pointer.is_empty() {
            // The empty pointer addresses the whole document. Valid, and what a
            // rejection that named no field would produce.
            return true;
        }
        if !pointer.starts_with('/') {
            return false;
        }
        for token in pointer[1..].split('/') {
            let mut characters = token.chars();
            while let Some(character) = characters.next() {
                if character == '~' && !matches!(characters.next(), Some('0' | '1')) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn a_nested_indexed_field_transcribes_into_a_valid_json_pointer() {
        // The form the ingest scenario rejects a row of a batch with. A client
        // that wants to retry has the body it sent and no way to look
        // `event[3].schema_version` up in it; `/event/3/schema_version` it can
        // apply directly.
        let pointer = json_pointer("event[3].schema_version");
        assert_eq!(pointer, "/event/3/schema_version");
        assert!(
            is_rfc6901_pointer(&pointer),
            "a rejected field must transcribe into a pointer a client can apply: {pointer}"
        );
    }

    #[test]
    fn a_plain_field_becomes_a_single_reference_token() {
        assert_eq!(json_pointer("as_of"), "/as_of");
        assert_eq!(json_pointer("source.channel"), "/source/channel");
        assert_eq!(
            json_pointer("corrections[0].operation.date"),
            "/corrections/0/operation/date"
        );
    }

    #[test]
    fn a_field_name_with_a_pointer_character_is_escaped() {
        // Without the escape these would address members that do not exist:
        // `application/json` would read as two tokens, and a tilde would read as
        // the start of an escape sequence.
        assert_eq!(json_pointer("content~type"), "/content~0type");
        assert_eq!(json_pointer("application/json"), "/application~1json");
        assert!(is_rfc6901_pointer(&json_pointer("content~type")));
        assert!(is_rfc6901_pointer(&json_pointer("application/json")));
    }

    #[test]
    fn a_rejection_that_names_a_closed_set_publishes_its_values() {
        // The whole point of the variant: `expected` is a sentence and a client
        // that wants to retry would have to parse it. The values are the same
        // fact in a form nothing has to read.
        let rejection = FieldRejection::new("outcome", "one of the outcomes", "settled")
            .admitting_codes(&["internal_transfer", "external_flow", "refund", "income", "fee"]);
        assert_eq!(rejection.pointer(), "/outcome");
        assert_eq!(
            rejection
                .alternatives
                .iter()
                .map(|alternative| alternative.value.as_str())
                .collect::<Vec<_>>(),
            ["internal_transfer", "external_flow", "refund", "income", "fee"]
        );
        assert!(rejection.resolutions.is_empty());
        assert_eq!(AppError::from(rejection).code(), "invalid_request");
    }

    #[test]
    fn an_enriched_rejection_keeps_the_code_the_plain_one_has() {
        // Two variants, one contract: a client switching on `code` must not be
        // able to tell that the server had more to say.
        assert_eq!(
            AppError::from(FieldRejection::new("answer", "one of the shapes", "fee")).code(),
            AppError::Invalid {
                field: "answer".into(),
                expected: "one of the shapes".into(),
                actual: "fee".into(),
            }
            .code()
        );
    }

    #[test]
    fn every_app_error_has_a_machine_readable_code() {
        // The code goes in the response body: the external agent uses it to decide,
        // whether to retry the request. An empty string is indistinguishable from «no code»,
        // and a single code for all errors — from «something went wrong».
        assert_eq!(
            AppError::Store("no connection".into()).code(),
            "store_unavailable"
        );
        assert_eq!(
            AppError::NotFound {
                what: "environment",
                id: "does not exist".into(),
            }
            .code(),
            "not_found"
        );
        assert_eq!(
            AppError::Invalid {
                field: "as_of".into(),
                expected: "date in YYYY-MM-DD format".into(),
                actual: "yesterday".into(),
            }
            .code(),
            "invalid_request"
        );
        assert_eq!(
            AppError::Invariant {
                correlation: Uuid::new_v4(),
                source: ProjectionError::SnapshotFingerprintMismatch,
            }
            .code(),
            "invariant_violated"
        );
        assert_eq!(
            AppError::DirectoryInvariant {
                correlation: Uuid::new_v4(),
                detail: "code ticker:ABC on 2026-08-25 resolves to 2 instruments".into(),
            }
            .code(),
            "directory_invariant_violated"
        );
        assert_eq!(
            AppError::Projection(ProjectionError::SnapshotFingerprintMismatch).code(),
            "projection_failed"
        );
        assert_eq!(
            AppError::NotConfigured {
                what: "broker access encryption",
            }
            .code(),
            "not_configured"
        );
        assert_eq!(
            AppError::Random("source closed".into()).code(),
            "random_unavailable"
        );
        assert_eq!(
            AppError::Conflict {
                what: "access already configured".into(),
            }
            .code(),
            "already_exists"
        );
    }
}
