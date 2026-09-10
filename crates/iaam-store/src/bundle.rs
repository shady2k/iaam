//! Archive bundle (§14).
//!
//! **A copy of the database file is not a complete backup**, and exporting only
//! events is even less so: it would produce different projections because the
//! set of scopes and reference data would remain outside. The bundle carries everything
//! needed to repeat the calculation: events, accounts, scope versions,
//! the reference data an event's legs point at, and the schema version under
//! which all this is recorded.
//!
//! `event_legs.custody` and `event_legs.instrument` are real, undeferred
//! foreign keys, so a bundle that carried only events, accounts and contours
//! could not be restored into an empty database at all — the first security
//! leg would be refused. `custody_places` and `instruments` close that: an
//! archive is needed exactly when the outside world is not available, and one
//! that needs a market sync before it will restore is not an archive.
//!
//! The owner decided what a bundle is for (iaam-k3gh.9, 2026-09-10): the
//! transferable state of an instance, not a journal archive, and it carries
//! everything — not only events and the reference data their legs point at,
//! but his standing decisions (iaam-k3gh.9.2: classification rules, category
//! rules, account declarations, the decision-history audit trail) and the
//! evidence he acquired that a re-sync cannot reliably reproduce
//! (iaam-k3gh.9.3: prices, rates, uploaded statements, import sessions still
//! in progress). [`TABLE_DISPOSITIONS`] is the complete, schema-checked
//! account of what does and does not travel, and why.
//!
//! Market observations, instrument aliases and issue terms are scoped to the
//! same instrument closure [`REFERENCE_CLOSURE_SQL`] computes for
//! [`InstrumentSection`] — not the whole market directory, which a sync fills
//! with an exchange's entire universe and an archive of one owner's affairs
//! has no business carrying. `sync_runs`, `fx_observations`,
//! `key_rate_observations` and `series_completeness` are not instrument-scoped
//! at all and travel wholesale: this is architecturally a single-owner
//! instance (`BundleCliError::AmbiguousOwner`'s own doc comment in
//! `iaam-bootstrap` says so directly), so there is no second owner's data
//! these global tables could be leaking.

use std::collections::HashSet;

use iaam_core::custody::CustodyOrigin;
use iaam_core::event::Event;
use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::journal::find_duplicate;
use crate::journal::write::insert_event_in;
use crate::{SqliteStore, StoreError};

/// Bundle format version.
///
/// A bundle with a newer version is not read: silently skipping an unknown
/// section would result in data loss. Version 2 adds the reference data an
/// event's legs point at — without it a bundle holding one security leg
/// could not be restored into an empty database at all.
///
/// Version 3 (iaam-k3gh.9.2) carries the owner's standing decisions, settled
/// 2026-09-10: a bundle is the transferable state of an instance, not an
/// archive of the journal, and it carries everything. Before this a restore
/// held his facts and not one of his judgements about them — classification
/// rules, category rules and groups, the category-assignment projection,
/// account scope exclusions, transfer partners, retirements, aliases,
/// declined account names, and the decision-history audit trail. See
/// [`TABLE_DISPOSITIONS`] for what still does not travel, and why.
///
/// Version 4 (iaam-k3gh.9.5) adds [`Bundle::schema_generation`]: a bundle
/// claiming `schema_version` alone could not be told apart from one written
/// under the numbering the schema collapse discarded, because both use the
/// same bare integers. This field is what lets `import_bundle` tell them
/// apart, so it does not itself change what a bundle carries — no new
/// section, nothing new to restore — and old archives that never wrote it
/// are read as belonging to this build's own generation (see the field's own
/// doc comment).
///
/// Version 5 (iaam-k3gh.9.3) carries the evidence the owner acquired and
/// cannot fetch again: the instrument closure's aliases and issue terms; the
/// documents he uploaded, body and all, and the raw rows read from them; the
/// import sessions built while reading them, still-open ones included, and
/// everything asked and answered inside one; the source-profile version
/// ledger and the broker and market source-code dictionaries; and the market
/// data family — synchronization runs, prices, FX rates, the key rate,
/// series completeness markers and accrued-interest readings. Three tables
/// that hang off `schedule_snapshots` by a real foreign key
/// (`schedule_coupon_periods`, `schedule_principal_repayments`,
/// `schedule_offer_windows`) do **not** travel: their header is `Derived`
/// (iaam-k3gh.9.1), and carrying the detail rows without it would either
/// dangle the foreign key on restore or reopen that decision — see
/// [`TABLE_DISPOSITIONS`]'s own comment on the three for the full reasoning.
///
/// Version 6 (`iaam-7ffl`) adds [`AccountSection::declared_by`]: the
/// credential that created the account, in `Provenance::declared_by`'s own
/// vocabulary. The field is optional and contributes no bytes when absent, so
/// an archive written under version 6 reads back correctly on a build that
/// only knows version 5 — but that older build would then silently drop the
/// attribution on its own next export, rather than fail on a fact it does not
/// know how to carry. That is exactly the data loss this module's own doc
/// comment says a version bump exists to refuse rather than commit quietly,
/// so the version is bumped for it even though nothing about reading an older
/// archive under this build needed one.
pub const BUNDLE_VERSION: u32 = 6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContourSection {
    pub contour: uuid::Uuid,
    pub version: u32,
    pub title: String,
    pub accounts: Vec<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSection {
    pub id: uuid::Uuid,
    pub title: String,
    pub institution: Option<String>,
    /// The credential the account was created under, in `accounts.declared_by`'s
    /// own vocabulary — a reference to a token, not the token itself, which
    /// does not travel at all (see the `Credential` disposition on
    /// `api_tokens` in [`TABLE_DISPOSITIONS`]). Added at bundle version 6
    /// (`iaam-7ffl`).
    ///
    /// `#[serde(default)]` is what lets an archive written before this field
    /// existed still deserialize; `skip_serializing_if` is what keeps its
    /// checksum unchanged once it does — every account in such an archive
    /// reads back `None`, and a section nobody wrote contributes no bytes.
    /// `None` is «not recorded», never «the owner's own account»: it is the
    /// state of every account restored from an archive this old, and a
    /// restore must not read it as agreement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<uuid::Uuid>,
}

/// A place of custody, carried the way `custody_places` stores it.
///
/// `origin` is the raw code (`"declared"` or `"minted"`), not
/// [`CustodyOrigin`] itself: a bundle is a wire format, and the schema's
/// `CHECK` constraint — not this type — is what refuses a third value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustodyPlaceSection {
    pub id: uuid::Uuid,
    pub title: String,
    pub institution: Option<String>,
    pub origin: String,
}

/// An instrument, carried the way `instruments` stores it.
///
/// `kind` is the raw code, for the same reason `origin` above is: the
/// schema, not this type, is where an invalid code is refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentSection {
    pub id: uuid::Uuid,
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
    pub lineage_parent: Option<uuid::Uuid>,
    pub lineage_reason: Option<String>,
}

/// An owner classification rule, carried the way `classification_rules`
/// stores it (§10.4).
///
/// `retired_at` and `replaces` travel exactly as stored: a restore that
/// dropped either would silently turn a retired rule back into a live one, or
/// lose the decision chain an edit recorded (iaam-k3gh.9.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationRuleSection {
    pub id: uuid::Uuid,
    pub version: u32,
    pub counterparty_account: Option<String>,
    pub description_contains: Option<String>,
    pub source_kind: Option<String>,
    pub source_category: Option<String>,
    pub owner_category: Option<String>,
    pub source_code: Option<String>,
    pub movement: Option<String>,
    pub outcome_kind: String,
    pub to_account: Option<uuid::Uuid>,
    pub fee_origin: Option<String>,
    pub income_kind: Option<String>,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<uuid::Uuid>,
}

/// A category group, carried the way `category_groups` stores it.
///
/// `retired_at` is carried for the reason it is on the rules above:
/// retirement preserves the names historical reports used, and dropping it
/// would resurrect a group the owner closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryGroupSection {
    pub id: uuid::Uuid,
    pub title: String,
    pub retired_at: Option<String>,
    pub is_income: bool,
}

/// A category belonging to one group, carried the way `categories` stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategorySection {
    pub id: uuid::Uuid,
    pub group_id: uuid::Uuid,
    pub title: String,
    pub retired_at: Option<String>,
}

/// A category assignment rule, carried the way `category_rules` stores it.
///
/// `matcher_kind`, `value`, `text` and `description_mode` are `CategoryMatcher`'s
/// four variants as the schema's own raw columns, not the domain enum — the
/// same reason [`CustodyPlaceSection::origin`] carries a raw code: the
/// schema's `CHECK`, not this type, is where an invalid combination is
/// refused. `retired_at` and `replaces` carry the same weight they do on
/// [`ClassificationRuleSection`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryRuleSection {
    pub id: uuid::Uuid,
    pub version: u32,
    pub matcher_kind: String,
    pub value: Option<String>,
    pub text: Option<String>,
    pub description_mode: Option<String>,
    pub category: uuid::Uuid,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<uuid::Uuid>,
}

/// One event's category assignment, carried the way
/// `event_category_assignments` stores it.
///
/// That table's own doc comment calls it a read model, rebuilt rather than
/// repaired — and the owner's decision (iaam-k3gh.9) is to carry it anyway,
/// because a rebuild on the far side needs `classification_rules` and
/// `category_rules` to already be there in exactly the state that produced
/// this projection, and nothing guarantees a later instance runs the same
/// rebuild before anyone reads a report. Carrying the projection removes that
/// dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventCategoryAssignmentSection {
    pub event: uuid::Uuid,
    pub category: uuid::Uuid,
    pub rule: uuid::Uuid,
    pub basis: String,
    pub rules_revision: i64,
}

/// The owner's statement that an account sits outside every contour, carried
/// the way `account_scope_exclusions` stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountScopeExclusionSection {
    pub account: uuid::Uuid,
    pub reason: String,
    pub recorded_at: String,
}

/// The owner's statement about one account's transfer partners: one row of
/// `account_transfer_statements` plus the `account_transfer_partners` it
/// names.
///
/// `partners` may be empty, and that is a real answer — "money moves between
/// this account and none of my others" — not "he has not said": the absence
/// of a section entirely is what the second means. This is why the two
/// tables are one section rather than two: an empty partner list carried
/// without its statement would be indistinguishable from silence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountTransferSection {
    pub account: uuid::Uuid,
    pub recorded_at: String,
    pub partners: Vec<uuid::Uuid>,
}

/// One row of `account_retirements`, exactly as stored.
///
/// This table is an APPEND-ONLY HISTORY (see its own doc comment), and the
/// whole of it travels rather than only the state currently in force: a
/// report published under an earlier revision names that revision, and only
/// the raw rows — in revision order — let a restored instance keep answering
/// to it. `effective_on` is `None` exactly on the row that withdraws a
/// previous retirement; losing that against a `Some` is exactly what turns a
/// withdrawn retirement back into a live one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountRetirementSection {
    pub revision: u32,
    pub account: uuid::Uuid,
    pub effective_on: Option<String>,
    pub recorded_at: String,
}

/// A further identifier reaching one account, carried the way
/// `account_aliases` stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountAliasSection {
    pub account: uuid::Uuid,
    pub value: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
}

/// A printed name the owner has said is not one of his accounts, carried the
/// way `declined_account_names` stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclinedAccountNameSection {
    pub printed: String,
    pub reason: String,
    pub recorded_at: String,
}

/// One row of the reversible-decision audit trail, carried the way
/// `decision_history` stores it.
///
/// `declared_by` travels as the bare identifier this table already stores it
/// as — a reference to a token, not the token itself, which does not travel
/// at all (see the `Credential` disposition on `api_tokens` in
/// [`TABLE_DISPOSITIONS`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionHistorySection {
    pub declared_by: Option<uuid::Uuid>,
    pub operation: String,
    pub subject: String,
    pub decision: String,
    pub undo: String,
    pub recorded_at: String,
}

/// An instrument alias, carried the way `instrument_aliases` stores it.
///
/// Scoped to the same instrument closure [`InstrumentSection`] is (see
/// [`REFERENCE_CLOSURE_SQL`]'s own doc comment), for the identical reason:
/// `instrument` is a real foreign key, so an alias naming an instrument this
/// bundle does not also carry would refuse on import, and one naming an
/// instrument nobody's events reach has no business in one owner's archive
/// regardless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentAliasSection {
    pub namespace: String,
    pub value: String,
    pub instrument: uuid::Uuid,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub source: String,
    pub created_at: String,
}

/// One row of `issue_terms`: a bond's terms as observed at a source, at a
/// point in time.
///
/// Carried, not derived (iaam-k3gh.9.3): `default_declared` and
/// `default_technical` are what a source said on the day it was asked, and a
/// later reading of the same instrument can say something else — an issuer
/// that was current on `observed_at` and later defaults does not rewrite the
/// row that already recorded it wasn't. Scoped to the instrument closure for
/// the same reason [`InstrumentAliasSection`] is: `instrument_id` is a real
/// foreign key into [`InstrumentSection`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueTermsSection {
    pub instrument_id: uuid::Uuid,
    pub source_id: String,
    pub observed_at: String,
    pub effective_from: Option<String>,
    pub maturity_date: Option<String>,
    pub initial_face_value: Option<String>,
    pub face_currency_code: Option<String>,
    pub coupon_periods_per_year: Option<i64>,
    pub day_count: Option<String>,
    pub calendar: Option<String>,
    pub default_declared: bool,
    pub default_technical: bool,
    pub recorded_at: String,
}

/// A loaded document, carried the way `source_documents` stores it, body and
/// all (iaam-k3gh.9.3).
///
/// **The body travels.** A statement the owner uploaded is exactly the kind
/// of evidence this task's whole framing is about: the broker may no longer
/// serve last year's report the way it served it then, or at all, and the
/// parser version recorded beside it exists specifically so a corrected
/// parser can replay the same bytes (see this crate's `documents` module).
/// Losing the body on restore would leave that provenance pointing at
/// nothing. `body` is base64 inside the JSON wire format (see
/// `body_encoding` below) rather than serde's default byte-array
/// representation, which would inflate a multi-megabyte statement several
/// times over — exactly the "an export nobody can move" failure iaam-k3gh.9.3
/// asks not to ship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceDocumentSection {
    pub id: uuid::Uuid,
    pub broker: String,
    pub format: String,
    pub parser_version: String,
    pub document_hash: String,
    pub uploaded_at: String,
    #[serde(with = "body_encoding")]
    pub body: Vec<u8>,
}

/// Base64 for a document body, so the JSON wire format stores it as one
/// compact string instead of serde's default array-of-integers rendering of
/// `Vec<u8>` (roughly four bytes of JSON per byte of document, before this).
mod body_encoding {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// One row of `raw_rows`: a document's row exactly as its source printed it,
/// before any parsing decision.
///
/// Carried alongside [`SourceDocumentSection`] rather than folded into it:
/// the schema keeps them as two tables (`documents.rs`'s own module doc
/// explains why — the row is a fact about parsing a document, not a second
/// copy of the document), and the bundle keeps the same shape so a restore
/// writes exactly the rows the source produced, `status` included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRowSection {
    pub document: uuid::Uuid,
    pub sheet: Option<String>,
    pub row: i64,
    pub payload: String,
    pub status: String,
}

/// One row of `document_unresolved_accounts`: a name a reading of a document
/// could not place, still open the day the owner exported.
///
/// Carried (iaam-k3gh.9.3) because it is not recomputable from anything a
/// restore has: whether `printed` resolves depends on the account directory
/// as it stood when the document was read, and that moment does not survive
/// as a fact anywhere else once the row is gone. `import_session` is a real
/// foreign key into [`ImportSessionSection`], which is why import order
/// carries sessions first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentUnresolvedAccountSection {
    pub document_hash: String,
    pub printed: String,
    pub ordinal: i64,
    pub records: i64,
    pub import_session: uuid::Uuid,
    pub recorded_at: String,
}

/// A pre-journal import session, carried the way `import_sessions` stores it
/// (iaam-k3gh.9.3).
///
/// The module doc comment on `iaam-store::import_session` calls a session
/// **pre-journal state**: nothing in it is a fact, and that is exactly why it
/// must travel rather than be treated as disposable scratch space — an open
/// session the owner has half-answered is work he would have to redo from
/// the statement again, and an abandoned or committed one is history of a
/// decision he already made about a document. `source` and `import` are the
/// same opaque hash-derived identifiers `iaam_core::ids` mints them as
/// everywhere else, not a foreign key into anything this bundle carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportSessionSection {
    pub id: uuid::Uuid,
    pub state: String,
    pub source: Option<uuid::Uuid>,
    pub import: Option<uuid::Uuid>,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub account: Option<uuid::Uuid>,
}

/// One row of `import_observations`: one submitted line's provisional state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportObservationSection {
    pub session: uuid::Uuid,
    pub row: i64,
    pub row_key: Option<String>,
    pub concluded: bool,
    pub payload: String,
    pub answer: Option<String>,
    pub answer_rule: Option<uuid::Uuid>,
    pub answer_rule_version: Option<i64>,
}

/// One row of `import_questions`: one thing the engine could not decide on
/// its own, and whatever the owner has answered so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportQuestionSection {
    pub id: uuid::Uuid,
    pub session: uuid::Uuid,
    pub row: i64,
    pub question: String,
    pub alternatives: String,
    pub prompt: String,
    pub asked_at: String,
    pub answered_at: Option<String>,
    pub answer: Option<String>,
    pub rule: Option<uuid::Uuid>,
}

/// One row of `import_control_figures`: the control section a statement
/// prints about itself, deliberately not a foreign key into `accounts` — see
/// that table's own comment, carried verbatim here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportControlFigureSection {
    pub session: uuid::Uuid,
    pub account: String,
    pub currency: String,
    pub period_from: String,
    pub period_to: String,
    pub opening: Option<i64>,
    pub closing: Option<i64>,
    pub debit_turnover: Option<i64>,
    pub credit_turnover: Option<i64>,
    pub stated_at: String,
}

/// One binding of `source_profile_versions`: which content a profile's
/// `(id, version)` already names.
///
/// Instance-wide, not owner-scoped — that module's own doc comment says why —
/// so this section is not filtered to the owner at all; a single-owner
/// instance's whole ledger is small and is exactly the thing that makes «this
/// id and version always mean this content» checkable after a restore.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceProfileVersionSection {
    pub id: String,
    pub version: u32,
    pub digest: String,
    pub first_loaded_at: String,
}

/// One row of `broker_operation_kinds`: the dictionary mapping one broker's
/// own code for an operation to iaam's vocabulary for it.
///
/// Instance-wide dictionary data, not owner-scoped, carried wholesale for the
/// same reason [`SourceProfileVersionSection`] is. `origin` distinguishes a
/// row seeded from the broker's contract from one the owner corrected by
/// hand — losing that on restore would make an owner's override
/// indistinguishable from the seed it overrode, and a later reseed could
/// silently take it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerOperationKindSection {
    pub broker: String,
    pub source_kind: String,
    pub kind: String,
    pub origin: String,
    pub dictionary: Option<String>,
    pub recorded_at: String,
}

/// One row of `market_source_codes`: the same dictionary shape as
/// [`BrokerOperationKindSection`], for a market data source's own codes
/// instead of a broker's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketSourceCodeSection {
    pub source_id: String,
    pub domain: String,
    pub source_code: String,
    pub meaning: String,
    pub origin: String,
    pub dictionary: Option<String>,
    pub recorded_at: String,
}

/// One row of `sync_runs`: a durable record of one attempt to fetch a market
/// data series, carried wholesale (iaam-k3gh.9.3).
///
/// Not owner-scoped — a run is not the owner's fact, it is this instance's
/// own history of asking an external source something — and every
/// observation section below names one by `sync_run_id`, a real foreign key,
/// which is why runs are inserted before any of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRunSection {
    pub id: uuid::Uuid,
    pub source_id: String,
    pub dataset: String,
    pub series_key: String,
    pub status: String,
    pub requested_from: String,
    pub requested_to: String,
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    pub pages: i64,
    pub rows: i64,
    pub page_errors: String,
    pub rate_limit_hits: i64,
    pub raw_hash: Option<String>,
    pub lease_token: Option<String>,
    pub lease_expires_at: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// One row of `price_observations`: a historical quote, exactly as observed.
///
/// The clearest case iaam-k3gh.9.3 is about: MOEX does not promise to keep
/// serving last month's closing prices forever, and a database that lost this
/// table lost that history for good. Scoped to the instrument closure like
/// [`InstrumentAliasSection`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceObservationSection {
    pub instrument_id: uuid::Uuid,
    pub board: String,
    pub session: i64,
    pub trade_date: String,
    pub kind: String,
    pub source_id: String,
    pub observed_at: String,
    pub price: String,
    pub currency: String,
    pub quotation_basis: String,
    pub basis_evidence: String,
    pub executability: String,
    pub raw_hash: String,
    pub sync_run_id: uuid::Uuid,
}

/// One row of `fx_observations`: a historical exchange rate. Not scoped to
/// any instrument — currency pairs are not instrument-specific — so, like
/// [`SyncRunSection`], every row travels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxObservationSection {
    pub from_code: String,
    pub to_code: String,
    pub trade_date: String,
    pub source_id: String,
    pub observed_at: String,
    pub nominal: i64,
    pub value: String,
    pub unit_rate: String,
    pub raw_hash: String,
    pub sync_run_id: uuid::Uuid,
}

/// One row of `key_rate_observations`: a historical central bank key rate,
/// carried wholesale for the same reason [`FxObservationSection`] is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyRateObservationSection {
    pub trade_date: String,
    pub source_id: String,
    pub observed_at: String,
    pub rate: String,
    pub raw_hash: String,
    pub sync_run_id: uuid::Uuid,
}

/// One row of `series_completeness`: how far one series is known to reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesCompletenessSection {
    pub source_id: String,
    pub dataset: String,
    pub series_key: String,
    pub complete_through: Option<String>,
    pub updated_at: String,
    pub last_successful_run: Option<uuid::Uuid>,
}

/// One row of `accrued_interest_observations`: a historical accrued-coupon
/// reading. The schema does not declare `instrument_id` as a foreign key on
/// this table (unlike [`PriceObservationSection::instrument_id`]), but this
/// section is scoped to the instrument closure anyway: an accrued-interest
/// reading for an instrument nowhere else in the archive is not this owner's
/// affair either, and scoping it any differently from the rest of the market
/// data family would be an inconsistency nobody asked for. `id` is a bare
/// autoincrement surrogate referenced by nothing else, so it does not travel
/// — a fresh one is assigned on insert, the same way [`ContourSection`]
/// leaves out no key of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccruedInterestObservationSection {
    pub instrument_id: uuid::Uuid,
    pub board: String,
    pub session: i64,
    pub trade_date: String,
    pub source_id: String,
    pub observed_at: String,
    pub per_unit: String,
    pub currency: String,
    pub raw_hash: String,
    pub sync_run_id: uuid::Uuid,
}

/// The complete bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_version: u32,
    pub schema_version: u32,
    /// The schema numbering `schema_version` was written under
    /// (iaam-k3gh.9.5).
    ///
    /// `None` on an archive written before this field existed, and **absence
    /// is not agreement**. The archive that made this bead necessary was
    /// written by this codebase's own export, under the numbering the collapse
    /// later discarded, and it carries no marker precisely because nobody was
    /// writing one yet: it cannot say it differs, and reading its silence as
    /// «this build's generation» is the false acceptance the marker exists to
    /// prevent, not a case the marker happens not to cover.
    ///
    /// What an unmarked archive can still be judged by is its own
    /// `schema_version` against
    /// [`crate::schema::LAST_UNMARKED_SCHEMA_VERSION`] — see
    /// [`Self::refuse_unreadable_numbering`], which is where the rule lives.
    #[serde(default)]
    pub schema_generation: Option<u32>,
    pub exported_at: String,
    pub owner: OwnerId,
    pub events: Vec<Event>,
    pub accounts: Vec<AccountSection>,
    pub contours: Vec<ContourSection>,
    /// Every `declared` custody place of the owner's, and the `minted` ones
    /// the exported events reach. `#[serde(default)]` is what lets an
    /// archive written before this field existed still deserialize.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custody_places: Vec<CustodyPlaceSection>,
    /// The instrument closure the exported events reach, plus `lineage_parent`
    /// transitively. See the note above `custody_places`: the same
    /// `#[serde(default)]` reasoning applies here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruments: Vec<InstrumentSection>,
    /// The owner's classification rules (§10.4), including retired ones.
    /// Added at version 3 (iaam-k3gh.9.2); the `#[serde(default)]` reasoning
    /// above `custody_places` applies to every field from here down.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classification_rules: Vec<ClassificationRuleSection>,
    /// The owner's category groups, including retired ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category_groups: Vec<CategoryGroupSection>,
    /// The owner's categories, including retired ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<CategorySection>,
    /// The owner's category assignment rules, including retired ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category_rules: Vec<CategoryRuleSection>,
    /// The category-assignment projection over the exported events.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_category_assignments: Vec<EventCategoryAssignmentSection>,
    /// The owner's declarations that an account sits outside every contour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_scope_exclusions: Vec<AccountScopeExclusionSection>,
    /// The owner's statements about which accounts transfer money between
    /// each other.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_transfers: Vec<AccountTransferSection>,
    /// The full `account_retirements` history, every revision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_retirements: Vec<AccountRetirementSection>,
    /// Every account alias.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_aliases: Vec<AccountAliasSection>,
    /// Printed names the owner has said are not his accounts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declined_account_names: Vec<DeclinedAccountNameSection>,
    /// The reversible-decision audit trail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_history: Vec<DecisionHistorySection>,
    /// Evidence the owner acquired and cannot fetch again (iaam-k3gh.9.3).
    /// Added at version 5; the `#[serde(default)]` reasoning above
    /// `custody_places` applies to every field from here down.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instrument_aliases: Vec<InstrumentAliasSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_terms: Vec<IssueTermsSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_documents: Vec<SourceDocumentSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_rows: Vec<RawRowSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_unresolved_accounts: Vec<DocumentUnresolvedAccountSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_sessions: Vec<ImportSessionSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_observations: Vec<ImportObservationSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_questions: Vec<ImportQuestionSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_control_figures: Vec<ImportControlFigureSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_profile_versions: Vec<SourceProfileVersionSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broker_operation_kinds: Vec<BrokerOperationKindSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub market_source_codes: Vec<MarketSourceCodeSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync_runs: Vec<SyncRunSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub price_observations: Vec<PriceObservationSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fx_observations: Vec<FxObservationSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_rate_observations: Vec<KeyRateObservationSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub series_completeness: Vec<SeriesCompletenessSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accrued_interest_observations: Vec<AccruedInterestObservationSection>,
    /// Content checksum. Computed from the canonical
    /// representation of all sections except the checksum itself.
    pub checksum: String,
}

/// Bundle contents without metadata fields. Exists for the checksum:
/// it must be computed over everything the bundle carries, and only that.
///
/// **Every field added to [`Bundle`] must be added here too, with the same
/// `#[serde(default, skip_serializing_if = "Vec::is_empty")]` pair on the
/// `Vec` fields.** ciborium writes a struct as a definite-length, named-key
/// map, and serde-derive computes that length after skipping — so a section
/// nobody wrote emits no bytes, shifts no neighbour, and does not move the
/// map header. `#[serde(default)]` on `Bundle` alone fixes reading an old
/// archive and leaves this checksum wrong; `skip_serializing_if` here alone
/// fixes the checksum and leaves old archives undeserialisable. Both structs,
/// or neither claim holds.
#[derive(Debug, Serialize)]
struct BundleContent<'a> {
    bundle_version: u32,
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_generation: Option<u32>,
    owner: OwnerId,
    events: &'a [Event],
    accounts: &'a [AccountSection],
    contours: &'a [ContourSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    custody_places: &'a [CustodyPlaceSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    instruments: &'a [InstrumentSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    classification_rules: &'a [ClassificationRuleSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    category_groups: &'a [CategoryGroupSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    categories: &'a [CategorySection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    category_rules: &'a [CategoryRuleSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    event_category_assignments: &'a [EventCategoryAssignmentSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    account_scope_exclusions: &'a [AccountScopeExclusionSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    account_transfers: &'a [AccountTransferSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    account_retirements: &'a [AccountRetirementSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    account_aliases: &'a [AccountAliasSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    declined_account_names: &'a [DeclinedAccountNameSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    decision_history: &'a [DecisionHistorySection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    instrument_aliases: &'a [InstrumentAliasSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    issue_terms: &'a [IssueTermsSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    source_documents: &'a [SourceDocumentSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    raw_rows: &'a [RawRowSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    document_unresolved_accounts: &'a [DocumentUnresolvedAccountSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    import_sessions: &'a [ImportSessionSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    import_observations: &'a [ImportObservationSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    import_questions: &'a [ImportQuestionSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    import_control_figures: &'a [ImportControlFigureSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    source_profile_versions: &'a [SourceProfileVersionSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    broker_operation_kinds: &'a [BrokerOperationKindSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    market_source_codes: &'a [MarketSourceCodeSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    sync_runs: &'a [SyncRunSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    price_observations: &'a [PriceObservationSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    fx_observations: &'a [FxObservationSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    key_rate_observations: &'a [KeyRateObservationSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    series_completeness: &'a [SeriesCompletenessSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    accrued_interest_observations: &'a [AccruedInterestObservationSection],
}

impl Bundle {
    /// The generation this archive's `schema_version` was written under, when
    /// the archive says.
    ///
    /// `None` is not «this build's generation». It is «nobody wrote it down»,
    /// and the two must not be conflated: the archive iaam-k3gh.9.5 is about
    /// carries no marker and claims `schema_version` 2 under the numbering the
    /// collapse discarded, so reading its silence as agreement is precisely the
    /// false acceptance the marker exists to prevent. What an unmarked archive
    /// can still be judged by is
    /// [`crate::schema::LAST_UNMARKED_SCHEMA_VERSION`] — see
    /// [`Self::refuse_unreadable_numbering`].
    #[must_use]
    pub const fn schema_generation(&self) -> Option<u32> {
        self.schema_generation
    }

    /// Refuse an archive whose `schema_version` this build cannot read as a
    /// number of its own numbering.
    ///
    /// Two cases, and they are one rule: an archive that names a generation
    /// other than this build's is refused by name, and an archive that names
    /// none is refused as soon as its `schema_version` exceeds what any
    /// unmarked build ever wrote. Both say the same thing — the counter that
    /// produced this number is not the counter this build compares against —
    /// and both are stated as such rather than as two integers that happen not
    /// to collide.
    fn refuse_unreadable_numbering(&self) -> Result<(), StoreError> {
        match self.schema_generation {
            Some(generation) if generation == crate::schema::SCHEMA_GENERATION => Ok(()),
            Some(generation) => Err(StoreError::SchemaGenerationMismatch {
                found: generation,
                current: crate::schema::SCHEMA_GENERATION,
            }),
            None if self.schema_version > crate::schema::LAST_UNMARKED_SCHEMA_VERSION => {
                Err(StoreError::BundleCorrupted {
                    detail: format!(
                        "archive names no schema generation and claims schema_version {},                          which no build writing no generation ever wrote: it belongs to the                          numbering the schema collapse discarded, and its version cannot be                          compared with this build's. Export it again from the instance that                          holds it",
                        self.schema_version
                    ),
                })
            }
            None => Ok(()),
        }
    }

    /// Content checksum.
    ///
    /// Computed from the **canonical serialization of all contents**.
    /// The first revision hashed only event identifiers and hashes
    /// of the raw data—at such a checksum, a substituted monetary value passed
    /// validation, and a corrupted archive appeared intact. This is exactly the
    /// failure the checksum exists to prevent (§14).
    ///
    /// The export date is not included in the checksum: it describes the export,
    /// not the facts being transferred, and changing it corrupts nothing.
    #[must_use]
    pub fn compute_checksum(&self) -> String {
        let content = BundleContent {
            bundle_version: self.bundle_version,
            schema_version: self.schema_version,
            schema_generation: self.schema_generation,
            owner: self.owner,
            events: &self.events,
            accounts: &self.accounts,
            contours: &self.contours,
            custody_places: &self.custody_places,
            instruments: &self.instruments,
            classification_rules: &self.classification_rules,
            category_groups: &self.category_groups,
            categories: &self.categories,
            category_rules: &self.category_rules,
            event_category_assignments: &self.event_category_assignments,
            account_scope_exclusions: &self.account_scope_exclusions,
            account_transfers: &self.account_transfers,
            account_retirements: &self.account_retirements,
            account_aliases: &self.account_aliases,
            declined_account_names: &self.declined_account_names,
            decision_history: &self.decision_history,
            instrument_aliases: &self.instrument_aliases,
            issue_terms: &self.issue_terms,
            source_documents: &self.source_documents,
            raw_rows: &self.raw_rows,
            document_unresolved_accounts: &self.document_unresolved_accounts,
            import_sessions: &self.import_sessions,
            import_observations: &self.import_observations,
            import_questions: &self.import_questions,
            import_control_figures: &self.import_control_figures,
            source_profile_versions: &self.source_profile_versions,
            broker_operation_kinds: &self.broker_operation_kinds,
            market_source_codes: &self.market_source_codes,
            sync_runs: &self.sync_runs,
            price_observations: &self.price_observations,
            fx_observations: &self.fx_observations,
            key_rate_observations: &self.key_rate_observations,
            series_completeness: &self.series_completeness,
            accrued_interest_observations: &self.accrued_interest_observations,
        };
        let mut body = Vec::new();
        ciborium::into_writer(&content, &mut body)
            .unwrap_or_else(|error| panic!("bundle cannot be serialized: {error}"));
        let digest = Sha256::digest(&body);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Every `(table, column)` pair in `0001_schema.sql` where a table whose
/// name starts with `event` declares `REFERENCES instruments`.
///
/// This is the ground truth [`REFERENCE_CLOSURE_SQL`] promises to cover, the
/// way `journal::CUSTODY_REFERENCE_COLUMNS` is the ground truth
/// `Event::referenced_custodies` promises to cover. Re-exported so that
/// `tests/schema_coverage.rs` — an integration test, outside this crate's
/// own boundary — can hold the schema to it: growing this list without
/// growing the query is exactly the drift that test exists to catch. Ten
/// pairs across eight tables today; `event_corporate_action` alone carries
/// three.
pub const INSTRUMENT_REFERENCE_COLUMNS: &[(&str, &str)] = &[
    ("event_control_assertion", "instrument"),
    ("event_corporate_action", "instrument"),
    ("event_corporate_action", "predecessor"),
    ("event_corporate_action", "successor"),
    ("event_income", "instrument"),
    ("event_legs", "instrument"),
    ("event_offer_exercise", "instrument"),
    ("event_opening_position", "instrument"),
    ("event_trade", "instrument"),
    ("event_valuation", "instrument"),
];

/// The instruments an owner's events reach.
///
/// Public so `tests/schema_coverage.rs` can check it against the schema: the
/// query is a list of tables, and a list is what goes stale when a ninth
/// table gains an instrument column. Every `(table, column)` pair here is one
/// listed in [`INSTRUMENT_REFERENCE_COLUMNS`]; the two are kept in sync by
/// that test, not by construction, the same way the custody guard in that
/// file holds `journal::CUSTODY_REFERENCE_COLUMNS` to the schema without
/// re-deriving `Event::referenced_custodies` from it.
///
/// `lineage_parent` is walked transitively in the final `UNION`, because an
/// instrument's row alone does not explain itself without its parent — and
/// aliases do not travel: nothing here reaches `instrument_aliases`, because
/// the events carry resolved `InstrumentId`s, not codes.
pub const REFERENCE_CLOSURE_SQL: &str = "
    WITH RECURSIVE reached(id) AS (
        SELECT l.instrument FROM event_legs l
          JOIN events e ON e.id = l.event
         WHERE e.owner = ?1 AND l.instrument IS NOT NULL
        UNION SELECT t.instrument FROM event_trade t
          JOIN events e ON e.id = t.event WHERE e.owner = ?1
        UNION SELECT i.instrument FROM event_income i
          JOIN events e ON e.id = i.event WHERE e.owner = ?1 AND i.instrument IS NOT NULL
        UNION SELECT p.instrument FROM event_opening_position p
          JOIN events e ON e.id = p.event WHERE e.owner = ?1
        UNION SELECT v.instrument FROM event_valuation v
          JOIN events e ON e.id = v.event WHERE e.owner = ?1
        UNION SELECT c.instrument FROM event_control_assertion c
          JOIN events e ON e.id = c.event WHERE e.owner = ?1 AND c.instrument IS NOT NULL
        UNION SELECT a.instrument FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.instrument IS NOT NULL
        UNION SELECT a.predecessor FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.predecessor IS NOT NULL
        UNION SELECT a.successor FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.successor IS NOT NULL
        UNION SELECT o.instrument FROM event_offer_exercise o
          JOIN events e ON e.id = o.event WHERE e.owner = ?1 AND o.instrument IS NOT NULL
        UNION SELECT n.lineage_parent FROM instruments n
          JOIN reached r ON n.id = r.id WHERE n.lineage_parent IS NOT NULL
    )
    SELECT n.id, n.kind, n.symbol, n.title,
           n.denomination_currency, n.settlement_currency, n.quote_currency,
           n.lineage_parent, n.lineage_reason
      FROM instruments n JOIN reached r ON n.id = r.id
     ORDER BY n.symbol, n.id
";

/// Why a table in the schema does, or does not yet, travel in a bundle.
///
/// A closed set, not free text, so that "not yet done" cannot masquerade as
/// "deliberately excluded" (iaam-k3gh.9.1). [`TABLE_DISPOSITIONS`] pairs every
/// table the schema declares with exactly one of these, and
/// `tests/bundle_coverage.rs` holds that pairing to the schema the way
/// `tests/schema_coverage.rs` holds [`INSTRUMENT_REFERENCE_COLUMNS`] to it: a
/// table added later and left off the list fails the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableDisposition {
    /// Recomputable from the journal and the owner's other standing decisions
    /// on restore: a cache whose loss changes nothing a reader can observe,
    /// because the projection is rebuilt from first principles rather than
    /// repaired. Snapshots, schedule snapshots and schedule completeness.
    Derived,
    /// A secret that must not travel in a file the owner copies between
    /// machines: `api_tokens`, `token_usage`, `broker_access`. A bundle is a
    /// portable archive, and a token inside it is a credential copied along
    /// with it — this is the owner's call to overturn, not a flag to add.
    Credential,
    /// Carried by [`Bundle`] today.
    Carried,
    /// Classified — not a credential, not derived, not left off by
    /// oversight — but not yet carried. Names the bead that will carry it, so
    /// the remaining work stays visible in code rather than only in a
    /// tracker.
    Pending(&'static str),
}

/// Every table the schema declares, classified by [`TableDisposition`].
///
/// This is the device iaam-k3gh.9 asked for in place of prose: a bundle
/// drifted to five sections out of fifty-nine because nothing failed when a
/// table was added and not exported. `tests/bundle_coverage.rs` asserts this
/// list's key set equals the exact set of `CREATE TABLE` names across every
/// migration in [`crate::schema::MIGRATIONS`] — so a table missing from
/// either side fails the build, and there is no substring or table-family
/// shortcut for a reviewer to be fooled by.
///
/// The four groups below are the ones the owner's decision (iaam-k3gh.9,
/// 2026-09-10) recorded, not re-derived here: the sections already in
/// [`Bundle`] before this list existed; the owner's standing decisions
/// (iaam-k3gh.9.2); evidence he acquired and cannot re-fetch (iaam-k3gh.9.3);
/// data derived from the journal; and credentials.
pub const TABLE_DISPOSITIONS: &[(&str, TableDisposition)] = &[
    // --- Already carried, via `events` and its per-kind detail tables. ---
    ("events", TableDisposition::Carried),
    ("event_legs", TableDisposition::Carried),
    ("event_trade", TableDisposition::Carried),
    ("event_cash_transfer", TableDisposition::Carried),
    ("event_unresolved_movement", TableDisposition::Carried),
    ("event_income", TableDisposition::Carried),
    ("event_fee", TableDisposition::Carried),
    ("event_tax", TableDisposition::Carried),
    ("event_opening_position", TableDisposition::Carried),
    ("event_stated_securities_value", TableDisposition::Carried),
    ("event_valuation", TableDisposition::Carried),
    ("event_control_assertion", TableDisposition::Carried),
    ("event_coverage_gap", TableDisposition::Carried),
    ("event_corporate_action", TableDisposition::Carried),
    ("event_offer_exercise", TableDisposition::Carried),
    ("event_coverage_gap_rows", TableDisposition::Carried),
    (
        "event_coverage_gap_row_dimensions",
        TableDisposition::Carried,
    ),
    // --- Already carried, via `accounts`, `contours`, `custody_places`,
    // `instruments`. ---
    ("accounts", TableDisposition::Carried),
    ("contour_versions", TableDisposition::Carried),
    ("contour_accounts", TableDisposition::Carried),
    ("custody_places", TableDisposition::Carried),
    ("instruments", TableDisposition::Carried),
    // --- The owner's standing decisions (iaam-k3gh.9.2). Carried as of that
    // task: `classification_rules`, `category_rules`, `event_category_assignments`
    // and the account declarations below, each with its own bundle section. ---
    ("classification_rules", TableDisposition::Carried),
    ("category_groups", TableDisposition::Carried),
    ("categories", TableDisposition::Carried),
    ("category_rules", TableDisposition::Carried),
    ("event_category_assignments", TableDisposition::Carried),
    ("account_scope_exclusions", TableDisposition::Carried),
    ("account_transfer_statements", TableDisposition::Carried),
    ("account_transfer_partners", TableDisposition::Carried),
    ("account_retirements", TableDisposition::Carried),
    ("account_aliases", TableDisposition::Carried),
    ("declined_account_names", TableDisposition::Carried),
    ("decision_history", TableDisposition::Carried),
    // --- Evidence the owner acquired and cannot re-fetch (iaam-k3gh.9.3).
    // Carried as of that task, each for its own stated reason — see the
    // section type's own doc comment for the reason behind each entry
    // below; this list only records the verdict. ---
    ("instrument_aliases", TableDisposition::Carried),
    ("source_documents", TableDisposition::Carried),
    ("raw_rows", TableDisposition::Carried),
    ("document_unresolved_accounts", TableDisposition::Carried),
    ("source_profile_versions", TableDisposition::Carried),
    // A durable synchronization run, the same operational-evidence shape as
    // the observation tables it leases for below: not a decision, not
    // recomputable from the journal, not a secret.
    ("sync_runs", TableDisposition::Carried),
    ("price_observations", TableDisposition::Carried),
    ("fx_observations", TableDisposition::Carried),
    ("key_rate_observations", TableDisposition::Carried),
    ("series_completeness", TableDisposition::Carried),
    ("accrued_interest_observations", TableDisposition::Carried),
    ("broker_operation_kinds", TableDisposition::Carried),
    ("issue_terms", TableDisposition::Carried),
    ("market_source_codes", TableDisposition::Carried),
    ("import_sessions", TableDisposition::Carried),
    ("import_observations", TableDisposition::Carried),
    ("import_questions", TableDisposition::Carried),
    ("import_control_figures", TableDisposition::Carried),
    // These three are the one deliberate exclusion this task's own
    // acceptance criteria asked for: each has a real foreign key into
    // `schedule_snapshots.id`, and that table is `Derived` — recomputed by a
    // schedule re-sync on restore, not carried — so carrying the detail rows
    // without their header would either orphan the foreign key on restore or
    // require reversing the landed decision that the header itself does not
    // travel (iaam-k3gh.9.1). A schedule re-sync recreates header and detail
    // rows together; they are exactly as re-fetchable as the header they
    // hang off, which is the test iaam-k3gh.9.3 asks this group to pass.
    ("schedule_coupon_periods", TableDisposition::Derived),
    ("schedule_principal_repayments", TableDisposition::Derived),
    ("schedule_offer_windows", TableDisposition::Derived),
    // --- Derived from the journal: recomputed on restore, never carried. ---
    ("snapshots", TableDisposition::Derived),
    ("schedule_snapshots", TableDisposition::Derived),
    ("schedule_completeness", TableDisposition::Derived),
    // --- Credentials: must not travel in a file the owner copies between
    // machines (see the module doc above `Bundle`). ---
    ("api_tokens", TableDisposition::Credential),
    ("token_usage", TableDisposition::Credential),
    ("broker_access", TableDisposition::Credential),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// How many events were added and how many already existed.
    Applied { inserted: usize, duplicates: usize },
}

impl SqliteStore {
    /// Bundle export.
    pub fn export_bundle(&self, owner: OwnerId) -> Result<Bundle, StoreError> {
        let events = self.load_events(owner)?;
        // `declared_by` is read directly here rather than through
        // `list_accounts`: that call returns the deliberately reduced summary
        // `AccountRecord` (id, title, institution) that most of this crate's
        // callers read, the same way `AccountView` withholds `cash_class` from
        // its own readers — see that type's own doc comment. A bundle export
        // is not one of those callers; it needs the column the summary leaves
        // out, so it reads the table itself instead of widening a view every
        // other caller would then also see.
        let mut statement = self.conn.prepare(
            "SELECT id, title, institution, declared_by FROM accounts
             WHERE owner = ?1 ORDER BY title, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut accounts = Vec::new();
        for row in rows {
            let (id, title, institution, declared_by) = row?;
            accounts.push(AccountSection {
                id: parse(&id, "account")?,
                title,
                institution,
                declared_by: declared_by
                    .map(|value| parse(&value, "account.declared_by"))
                    .transpose()?,
            });
        }
        drop(statement);

        let mut statement = self.conn.prepare(
            "SELECT v.contour, v.version, v.title, a.account
             FROM contour_versions v
             LEFT JOIN contour_accounts a
               ON a.owner = v.owner AND a.contour = v.contour AND a.version = v.version
             WHERE v.owner = ?1
             ORDER BY v.contour, v.version, a.account",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut contours: Vec<ContourSection> = Vec::new();
        for row in rows {
            let (contour, version, title, account) = row?;
            let contour = parse(&contour, "contour")?;
            let account = account
                .map(|value| parse(&value, "contour_account"))
                .transpose()?;
            match contours
                .last_mut()
                .filter(|section| section.contour == contour && section.version == version)
            {
                Some(section) => section.accounts.extend(account),
                None => contours.push(ContourSection {
                    contour,
                    version,
                    title,
                    accounts: account.into_iter().collect(),
                }),
            }
        }

        // A minted row is as load bearing for the foreign key as a declared
        // one, so every one the exported events reach travels alongside
        // every declared row of the owner's. Scoping the minted half to
        // reachability also keeps a failed synchronization's unreferenced
        // handles out of the archive — the write order (T6) registers a
        // handle before it appends, so an append that failed leaves one
        // behind with nothing pointing at it.
        let reached_custodies: HashSet<uuid::Uuid> = events
            .iter()
            .flat_map(Event::referenced_custodies)
            .map(|id| id.inner())
            .collect();
        let custody_places = self
            .list_custody_places(owner)?
            .into_iter()
            .filter(|place| {
                place.origin == CustodyOrigin::Declared
                    || reached_custodies.contains(&place.id.inner())
            })
            .map(|place| CustodyPlaceSection {
                id: place.id.inner(),
                title: place.title,
                institution: place.institution,
                origin: place.origin.code().to_owned(),
            })
            .collect();

        // The closure reached from the exported events, plus `lineage_parent`
        // transitively — not the whole directory, which a market sync fills
        // with an exchange's entire universe and an archive of one owner's
        // affairs has no business carrying.
        let mut statement = self.conn.prepare(REFERENCE_CLOSURE_SQL)?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;
        let mut instruments = Vec::new();
        for row in rows {
            let (
                id,
                kind,
                symbol,
                title,
                denomination,
                settlement,
                quote,
                lineage_parent,
                lineage_reason,
            ) = row?;
            let lineage_parent = lineage_parent
                .map(|value| parse(&value, "instrument"))
                .transpose()?;
            instruments.push(InstrumentSection {
                id: parse(&id, "instrument")?,
                kind,
                symbol,
                title,
                denomination_currency: denomination,
                settlement_currency: settlement,
                quote_currency: quote,
                lineage_parent,
                lineage_reason,
            });
        }

        // The instrument closure `instruments` above already computed, kept
        // as a set for the evidence sections below (iaam-k3gh.9.3) that are
        // scoped to it: instrument aliases, issue terms, price observations
        // and accrued-interest observations all name an instrument, and
        // carrying one this bundle does not also carry `instruments` for
        // would either dangle a real foreign key on restore or reintroduce
        // "the exchange's entire universe" the closure exists to keep out.
        let reached_instruments: HashSet<uuid::Uuid> =
            instruments.iter().map(|section| section.id).collect();

        // The owner's standing decisions (iaam-k3gh.9.2). `rule_history` and
        // `list_category_rules`/`list_groups`/`list_categories` already
        // return every row including retired ones, in decision order — the
        // history a restore needs is the history these calls already answer.
        let classification_rules = self
            .rule_history(owner)?
            .into_iter()
            .map(|rule| ClassificationRuleSection {
                id: rule.id.inner(),
                version: rule.version,
                counterparty_account: rule.counterparty_account,
                description_contains: rule.description_contains,
                source_kind: rule.source_kind,
                source_category: rule.source_category,
                owner_category: rule.owner_category,
                source_code: rule.source_code,
                movement: rule.movement,
                outcome_kind: rule.outcome_kind,
                to_account: rule.to_account.map(|account| account.inner()),
                fee_origin: rule.fee_origin,
                income_kind: rule.income_kind,
                created_at: rule.created_at,
                retired_at: rule.retired_at,
                replaces: rule.replaces.map(|id| id.inner()),
            })
            .collect();

        let category_groups = self
            .list_groups(owner)?
            .into_iter()
            .map(|group| CategoryGroupSection {
                id: group.id,
                title: group.title,
                retired_at: group.retired_at,
                is_income: group.is_income,
            })
            .collect();

        let categories = self
            .list_categories(owner)?
            .into_iter()
            .map(|category| CategorySection {
                id: category.id,
                group_id: category.group_id,
                title: category.title,
                retired_at: category.retired_at,
            })
            .collect();

        // Raw columns, not `CategoryMatcher`: as with `CustodyPlaceSection`,
        // the wire format carries what the schema stores, not the domain
        // type — `list_category_rules` decodes into the enum for its own
        // callers, which is exactly the step this must not depend on.
        let mut statement = self.conn.prepare(
            "SELECT id, version, matcher_kind, value, text, description_mode,
                    category, valid_from, valid_to, created_at, retired_at, replaces
             FROM category_rules
             WHERE owner = ?1
             ORDER BY version",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?;
        let mut category_rules = Vec::new();
        for row in rows {
            let (
                id,
                version,
                matcher_kind,
                value,
                text,
                description_mode,
                category,
                valid_from,
                valid_to,
                created_at,
                retired_at,
                replaces,
            ) = row?;
            category_rules.push(CategoryRuleSection {
                id: parse(&id, "category_rule")?,
                version,
                matcher_kind,
                value,
                text,
                description_mode,
                category: parse(&category, "category")?,
                valid_from,
                valid_to,
                created_at,
                retired_at,
                replaces: replaces
                    .map(|value| parse(&value, "category_rule"))
                    .transpose()?,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT event, category, rule, basis, rules_revision
             FROM event_category_assignments
             WHERE owner = ?1
             ORDER BY event",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut event_category_assignments = Vec::new();
        for row in rows {
            let (event, category, rule, basis, rules_revision) = row?;
            event_category_assignments.push(EventCategoryAssignmentSection {
                event: parse(&event, "event")?,
                category: parse(&category, "category")?,
                rule: parse(&rule, "category_rule")?,
                basis,
                rules_revision,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT account, reason, recorded_at
             FROM account_scope_exclusions
             WHERE owner = ?1
             ORDER BY account",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut account_scope_exclusions = Vec::new();
        for row in rows {
            let (account, reason, recorded_at) = row?;
            account_scope_exclusions.push(AccountScopeExclusionSection {
                account: parse(&account, "account")?,
                reason,
                recorded_at,
            });
        }

        // One statement row per account, each carrying the partners named
        // for it — the same shape `list_account_transfer_statements` builds,
        // rebuilt here so `recorded_at` (which that call does not select)
        // travels too.
        let mut statement = self.conn.prepare(
            "SELECT account, partner FROM account_transfer_partners
             WHERE owner = ?1
             ORDER BY account, partner",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut partners_by_account: std::collections::BTreeMap<uuid::Uuid, Vec<uuid::Uuid>> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (account, partner) = row?;
            partners_by_account
                .entry(parse(&account, "account")?)
                .or_default()
                .push(parse(&partner, "account")?);
        }
        let mut statement = self.conn.prepare(
            "SELECT account, recorded_at FROM account_transfer_statements
             WHERE owner = ?1
             ORDER BY account",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut account_transfers = Vec::new();
        for row in rows {
            let (account, recorded_at) = row?;
            let account = parse(&account, "account")?;
            account_transfers.push(AccountTransferSection {
                account,
                recorded_at,
                partners: partners_by_account.remove(&account).unwrap_or_default(),
            });
        }

        // The whole append-only history, not only the state in force: see
        // the doc comment on `AccountRetirementSection`.
        let mut statement = self.conn.prepare(
            "SELECT revision, account, effective_on, recorded_at
             FROM account_retirements
             WHERE owner = ?1
             ORDER BY revision",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut account_retirements = Vec::new();
        for row in rows {
            let (revision, account, effective_on, recorded_at) = row?;
            account_retirements.push(AccountRetirementSection {
                revision,
                account: parse(&account, "account")?,
                effective_on,
                recorded_at,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT account, value, valid_from, valid_to
             FROM account_aliases
             WHERE owner = ?1
             ORDER BY account, valid_from, value",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut account_aliases = Vec::new();
        for row in rows {
            let (account, value, valid_from, valid_to) = row?;
            account_aliases.push(AccountAliasSection {
                account: parse(&account, "account")?,
                value,
                valid_from,
                valid_to,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT printed, reason, recorded_at
             FROM declined_account_names
             WHERE owner = ?1
             ORDER BY recorded_at, printed",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut declined_account_names = Vec::new();
        for row in rows {
            let (printed, reason, recorded_at) = row?;
            declined_account_names.push(DeclinedAccountNameSection {
                printed,
                reason,
                recorded_at,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT declared_by, operation, subject, decision, undo, recorded_at
             FROM decision_history
             WHERE owner = ?1
             ORDER BY recorded_at, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut decision_history = Vec::new();
        for row in rows {
            let (declared_by, operation, subject, decision, undo, recorded_at) = row?;
            decision_history.push(DecisionHistorySection {
                declared_by: declared_by
                    .map(|value| parse(&value, "token"))
                    .transpose()?,
                operation,
                subject,
                decision,
                undo,
                recorded_at,
            });
        }

        // --- Evidence the owner acquired and cannot re-fetch (iaam-k3gh.9.3) ---

        let instrument_ids: Vec<String> = reached_instruments
            .iter()
            .map(|id| id.to_string())
            .collect();

        let mut instrument_aliases = Vec::new();
        if !instrument_ids.is_empty() {
            let sql = format!(
                "SELECT namespace, value, instrument, valid_from, valid_to, source, created_at
                 FROM instrument_aliases
                 WHERE instrument IN ({})
                 ORDER BY namespace, value, valid_from",
                placeholders(instrument_ids.len())
            );
            let mut statement = self.conn.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(instrument_ids.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?;
            for row in rows {
                let (namespace, value, instrument, valid_from, valid_to, source, created_at) = row?;
                instrument_aliases.push(InstrumentAliasSection {
                    namespace,
                    value,
                    instrument: parse(&instrument, "instrument")?,
                    valid_from,
                    valid_to,
                    source,
                    created_at,
                });
            }
        }

        let mut issue_terms = Vec::new();
        if !instrument_ids.is_empty() {
            let sql = format!(
                "SELECT instrument_id, source_id, observed_at, effective_from, maturity_date,
                        initial_face_value, face_currency_code, coupon_periods_per_year,
                        day_count, calendar, default_declared, default_technical, recorded_at
                 FROM issue_terms
                 WHERE instrument_id IN ({})
                 ORDER BY instrument_id, source_id, observed_at",
                placeholders(instrument_ids.len())
            );
            let mut statement = self.conn.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(instrument_ids.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, String>(12)?,
                ))
            })?;
            for row in rows {
                let (
                    instrument_id,
                    source_id,
                    observed_at,
                    effective_from,
                    maturity_date,
                    initial_face_value,
                    face_currency_code,
                    coupon_periods_per_year,
                    day_count,
                    calendar,
                    default_declared,
                    default_technical,
                    recorded_at,
                ) = row?;
                issue_terms.push(IssueTermsSection {
                    instrument_id: parse(&instrument_id, "instrument")?,
                    source_id,
                    observed_at,
                    effective_from,
                    maturity_date,
                    initial_face_value,
                    face_currency_code,
                    coupon_periods_per_year,
                    day_count,
                    calendar,
                    default_declared: default_declared != 0,
                    default_technical: default_technical != 0,
                    recorded_at,
                });
            }
        }

        let mut statement = self.conn.prepare(
            "SELECT id, broker, format, parser_version, document_hash, uploaded_at, body
             FROM source_documents
             WHERE owner = ?1
             ORDER BY uploaded_at, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Vec<u8>>(6)?,
            ))
        })?;
        let mut source_documents = Vec::new();
        for row in rows {
            let (id, broker, format, parser_version, document_hash, uploaded_at, body) = row?;
            source_documents.push(SourceDocumentSection {
                id: parse(&id, "document")?,
                broker,
                format,
                parser_version,
                document_hash,
                uploaded_at,
                body,
            });
        }

        // Joined to `source_documents` for the owner scope: `raw_rows` has
        // no `owner` column of its own.
        let mut statement = self.conn.prepare(
            "SELECT r.document, r.sheet, r.row, r.payload, r.status
             FROM raw_rows r
             JOIN source_documents d ON d.id = r.document
             WHERE d.owner = ?1
             ORDER BY r.document, r.sheet, r.row",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut raw_rows = Vec::new();
        for row in rows {
            let (document, sheet, row_number, payload, status) = row?;
            raw_rows.push(RawRowSection {
                document: parse(&document, "document")?,
                sheet,
                row: row_number,
                payload,
                status,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT document_hash, printed, ordinal, records, import_session, recorded_at
             FROM document_unresolved_accounts
             WHERE owner = ?1
             ORDER BY document_hash, ordinal",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut document_unresolved_accounts = Vec::new();
        for row in rows {
            let (document_hash, printed, ordinal, records, import_session, recorded_at) = row?;
            document_unresolved_accounts.push(DocumentUnresolvedAccountSection {
                document_hash,
                printed,
                ordinal,
                records,
                import_session: parse(&import_session, "import session")?,
                recorded_at,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT id, state, source, import, opened_at, closed_at, account
             FROM import_sessions
             WHERE owner = ?1
             ORDER BY opened_at, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut import_sessions = Vec::new();
        for row in rows {
            let (id, state, source, import, opened_at, closed_at, account) = row?;
            import_sessions.push(ImportSessionSection {
                id: parse(&id, "import session")?,
                state,
                source: source.map(|value| parse(&value, "source")).transpose()?,
                import: import.map(|value| parse(&value, "import")).transpose()?,
                opened_at,
                closed_at,
                account: account.map(|value| parse(&value, "account")).transpose()?,
            });
        }

        // Joined to `import_sessions` for the owner scope, exactly as
        // `raw_rows` is joined to `source_documents`: none of the three
        // tables below carries an `owner` column of its own.
        let mut statement = self.conn.prepare(
            "SELECT o.session, o.row, o.row_key, o.concluded, o.payload, o.answer,
                    o.answer_rule, o.answer_rule_version
             FROM import_observations o
             JOIN import_sessions s ON s.id = o.session
             WHERE s.owner = ?1
             ORDER BY o.session, o.row",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })?;
        let mut import_observations = Vec::new();
        for row in rows {
            let (
                session,
                row_number,
                row_key,
                concluded,
                payload,
                answer,
                answer_rule,
                answer_rule_version,
            ) = row?;
            import_observations.push(ImportObservationSection {
                session: parse(&session, "import session")?,
                row: row_number,
                row_key,
                concluded: concluded != 0,
                payload,
                answer,
                answer_rule: answer_rule
                    .map(|value| parse(&value, "classification rule"))
                    .transpose()?,
                answer_rule_version,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT q.id, q.session, q.row, q.question, q.alternatives, q.prompt,
                    q.asked_at, q.answered_at, q.answer, q.rule
             FROM import_questions q
             JOIN import_sessions s ON s.id = q.session
             WHERE s.owner = ?1
             ORDER BY q.session, q.row",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?;
        let mut import_questions = Vec::new();
        for row in rows {
            let (
                id,
                session,
                row_number,
                question,
                alternatives,
                prompt,
                asked_at,
                answered_at,
                answer,
                rule,
            ) = row?;
            import_questions.push(ImportQuestionSection {
                id: parse(&id, "import question")?,
                session: parse(&session, "import session")?,
                row: row_number,
                question,
                alternatives,
                prompt,
                asked_at,
                answered_at,
                answer,
                rule: rule
                    .map(|value| parse(&value, "classification rule"))
                    .transpose()?,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT f.session, f.account, f.currency, f.period_from, f.period_to,
                    f.opening, f.closing, f.debit_turnover, f.credit_turnover, f.stated_at
             FROM import_control_figures f
             JOIN import_sessions s ON s.id = f.session
             WHERE s.owner = ?1
             ORDER BY f.session, f.account, f.currency",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?;
        let mut import_control_figures = Vec::new();
        for row in rows {
            let (
                session,
                account,
                currency,
                period_from,
                period_to,
                opening,
                closing,
                debit_turnover,
                credit_turnover,
                stated_at,
            ) = row?;
            import_control_figures.push(ImportControlFigureSection {
                session: parse(&session, "import session")?,
                account,
                currency,
                period_from,
                period_to,
                opening,
                closing,
                debit_turnover,
                credit_turnover,
                stated_at,
            });
        }

        // Everything from here down is instance-wide, not owner-scoped: see
        // the module doc comment for why a single-owner instance carries
        // these wholesale rather than filtering them.
        let mut statement = self.conn.prepare(
            "SELECT id, version, digest, first_loaded_at
             FROM source_profile_versions
             ORDER BY id, version",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut source_profile_versions = Vec::new();
        for row in rows {
            let (id, version, digest, first_loaded_at) = row?;
            source_profile_versions.push(SourceProfileVersionSection {
                id,
                version,
                digest,
                first_loaded_at,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT broker, source_kind, kind, origin, dictionary, recorded_at
             FROM broker_operation_kinds
             ORDER BY broker, source_kind",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut broker_operation_kinds = Vec::new();
        for row in rows {
            let (broker, source_kind, kind, origin, dictionary, recorded_at) = row?;
            broker_operation_kinds.push(BrokerOperationKindSection {
                broker,
                source_kind,
                kind,
                origin,
                dictionary,
                recorded_at,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT source_id, domain, source_code, meaning, origin, dictionary, recorded_at
             FROM market_source_codes
             ORDER BY source_id, domain, source_code",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut market_source_codes = Vec::new();
        for row in rows {
            let (source_id, domain, source_code, meaning, origin, dictionary, recorded_at) = row?;
            market_source_codes.push(MarketSourceCodeSection {
                source_id,
                domain,
                source_code,
                meaning,
                origin,
                dictionary,
                recorded_at,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT id, source_id, dataset, series_key, status, requested_from, requested_to,
                    covered_from, covered_to, pages, rows, page_errors, rate_limit_hits,
                    raw_hash, lease_token, lease_expires_at, started_at, finished_at
             FROM sync_runs
             ORDER BY started_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, String>(16)?,
                row.get::<_, Option<String>>(17)?,
            ))
        })?;
        let mut sync_runs = Vec::new();
        for row in rows {
            let (
                id,
                source_id,
                dataset,
                series_key,
                status,
                requested_from,
                requested_to,
                covered_from,
                covered_to,
                pages,
                run_rows,
                page_errors,
                rate_limit_hits,
                raw_hash,
                lease_token,
                lease_expires_at,
                started_at,
                finished_at,
            ) = row?;
            sync_runs.push(SyncRunSection {
                id: parse(&id, "sync run")?,
                source_id,
                dataset,
                series_key,
                status,
                requested_from,
                requested_to,
                covered_from,
                covered_to,
                pages,
                rows: run_rows,
                page_errors,
                rate_limit_hits,
                raw_hash,
                lease_token,
                lease_expires_at,
                started_at,
                finished_at,
            });
        }

        let mut price_observations = Vec::new();
        if !instrument_ids.is_empty() {
            let sql = format!(
                "SELECT instrument_id, board, session, trade_date, kind, source_id, observed_at,
                        price, currency, quotation_basis, basis_evidence, executability,
                        raw_hash, sync_run_id
                 FROM price_observations
                 WHERE instrument_id IN ({})
                 ORDER BY instrument_id, board, session, trade_date, source_id, observed_at",
                placeholders(instrument_ids.len())
            );
            let mut statement = self.conn.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(instrument_ids.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                ))
            })?;
            for row in rows {
                let (
                    instrument_id,
                    board,
                    session,
                    trade_date,
                    kind,
                    source_id,
                    observed_at,
                    price,
                    currency,
                    quotation_basis,
                    basis_evidence,
                    executability,
                    raw_hash,
                    sync_run_id,
                ) = row?;
                price_observations.push(PriceObservationSection {
                    instrument_id: parse(&instrument_id, "instrument")?,
                    board,
                    session,
                    trade_date,
                    kind,
                    source_id,
                    observed_at,
                    price,
                    currency,
                    quotation_basis,
                    basis_evidence,
                    executability,
                    raw_hash,
                    sync_run_id: parse(&sync_run_id, "sync run")?,
                });
            }
        }

        let mut statement = self.conn.prepare(
            "SELECT from_code, to_code, trade_date, source_id, observed_at,
                    nominal, value, unit_rate, raw_hash, sync_run_id
             FROM fx_observations
             ORDER BY from_code, to_code, trade_date, source_id, observed_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?;
        let mut fx_observations = Vec::new();
        for row in rows {
            let (
                from_code,
                to_code,
                trade_date,
                source_id,
                observed_at,
                nominal,
                value,
                unit_rate,
                raw_hash,
                sync_run_id,
            ) = row?;
            fx_observations.push(FxObservationSection {
                from_code,
                to_code,
                trade_date,
                source_id,
                observed_at,
                nominal,
                value,
                unit_rate,
                raw_hash,
                sync_run_id: parse(&sync_run_id, "sync run")?,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT trade_date, source_id, observed_at, rate, raw_hash, sync_run_id
             FROM key_rate_observations
             ORDER BY trade_date, source_id, observed_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut key_rate_observations = Vec::new();
        for row in rows {
            let (trade_date, source_id, observed_at, rate, raw_hash, sync_run_id) = row?;
            key_rate_observations.push(KeyRateObservationSection {
                trade_date,
                source_id,
                observed_at,
                rate,
                raw_hash,
                sync_run_id: parse(&sync_run_id, "sync run")?,
            });
        }

        let mut statement = self.conn.prepare(
            "SELECT source_id, dataset, series_key, complete_through, updated_at, last_successful_run
             FROM series_completeness
             ORDER BY source_id, dataset, series_key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut series_completeness = Vec::new();
        for row in rows {
            let (source_id, dataset, series_key, complete_through, updated_at, last_successful_run) =
                row?;
            series_completeness.push(SeriesCompletenessSection {
                source_id,
                dataset,
                series_key,
                complete_through,
                updated_at,
                last_successful_run: last_successful_run
                    .map(|value| parse(&value, "sync run"))
                    .transpose()?,
            });
        }

        let mut accrued_interest_observations = Vec::new();
        if !instrument_ids.is_empty() {
            let sql = format!(
                "SELECT instrument_id, board, session, trade_date, source_id, observed_at,
                        per_unit, currency, raw_hash, sync_run_id
                 FROM accrued_interest_observations
                 WHERE instrument_id IN ({})
                 ORDER BY instrument_id, board, session, trade_date, source_id, observed_at",
                placeholders(instrument_ids.len())
            );
            let mut statement = self.conn.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(instrument_ids.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })?;
            for row in rows {
                let (
                    instrument_id,
                    board,
                    session,
                    trade_date,
                    source_id,
                    observed_at,
                    per_unit,
                    currency,
                    raw_hash,
                    sync_run_id,
                ) = row?;
                accrued_interest_observations.push(AccruedInterestObservationSection {
                    instrument_id: parse(&instrument_id, "instrument")?,
                    board,
                    session,
                    trade_date,
                    source_id,
                    observed_at,
                    per_unit,
                    currency,
                    raw_hash,
                    sync_run_id: parse(&sync_run_id, "sync run")?,
                });
            }
        }

        let mut bundle = Bundle {
            bundle_version: BUNDLE_VERSION,
            schema_version: crate::schema::SCHEMA_VERSION,
            schema_generation: Some(crate::schema::SCHEMA_GENERATION),
            exported_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z")),
            owner,
            events,
            accounts,
            contours,
            custody_places,
            instruments,
            classification_rules,
            category_groups,
            categories,
            category_rules,
            event_category_assignments,
            account_scope_exclusions,
            account_transfers,
            account_retirements,
            account_aliases,
            declined_account_names,
            decision_history,
            instrument_aliases,
            issue_terms,
            source_documents,
            raw_rows,
            document_unresolved_accounts,
            import_sessions,
            import_observations,
            import_questions,
            import_control_figures,
            source_profile_versions,
            broker_operation_kinds,
            market_source_codes,
            sync_runs,
            price_observations,
            fx_observations,
            key_rate_observations,
            series_completeness,
            accrued_interest_observations,
            checksum: String::new(),
        };
        bundle.checksum = bundle.compute_checksum();
        Ok(bundle)
    }

    /// Bundle import.
    ///
    /// Idempotent: events with known keys are not created again.
    /// Runs in **a single transaction**: a partially imported archive
    /// is a state that never existed, and dealing with it is worse
    /// than dealing with a failed import.
    ///
    /// Rejects a bundle that is: newer than the supported format; written
    /// with a schema newer than supported; inconsistent with the checksum;
    /// contains events belonging to another owner.
    pub fn import_bundle(&mut self, bundle: &Bundle) -> Result<ImportOutcome, StoreError> {
        if bundle.bundle_version > BUNDLE_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: bundle.bundle_version,
                supported: BUNDLE_VERSION,
            });
        }
        // The generation check comes first and is not a version comparison:
        // an archive whose generation does not match this build's own is not
        // "older" or "newer" in `schema_version` terms at all, because the
        // counter that produced it was reset. Comparing the versions
        // regardless — as this code once did — is exactly the false
        // acceptance iaam-k3gh.9.5 found: an archive written under the
        // numbering the collapse discarded can claim the same bare integer a
        // genuinely current archive would, and the two are not the same
        // schema.
        bundle.refuse_unreadable_numbering()?;
        if bundle.schema_version > crate::schema::SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: bundle.schema_version,
                supported: crate::schema::SCHEMA_VERSION,
            });
        }
        if bundle.checksum != bundle.compute_checksum() {
            return Err(StoreError::BundleCorrupted {
                detail: "checksum does not match contents".into(),
            });
        }
        // There is one owner in a bundle. An event belonging to another owner means either
        // that two archives were joined or that a substitution occurred; either would make
        // the ownership boundary fictitious (§14).
        if let Some(foreign) = bundle
            .events
            .iter()
            .find(|event| event.owner != bundle.owner)
        {
            return Err(StoreError::BundleCorrupted {
                detail: format!("event {} belongs to another owner", foreign.id.inner()),
            });
        }

        let owner = bundle.owner;
        let created_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));
        let transaction = self.conn.transaction()?;

        for account in &bundle.accounts {
            // `declared_by` is written on the `INSERT` branch only and named in
            // neither `ON CONFLICT` `SET` clause, alongside `title` and
            // `institution`: an account already present is not re-attributed by
            // a later restore, for the same reason [`SqliteStore::create_account`]
            // never rewrites the column once set — it is who created the
            // account, not a restatement a later archive gets to correct.
            transaction.execute(
                "INSERT INTO accounts (id, owner, title, institution, created_at, declared_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET
                     title = excluded.title,
                     institution = excluded.institution
                 WHERE accounts.owner = excluded.owner",
                params![
                    account.id.to_string(),
                    owner.inner().to_string(),
                    account.title,
                    account.institution,
                    created_at,
                    account.declared_by.map(|id| id.to_string()),
                ],
            )?;
        }

        // The owner's declarations about his accounts (iaam-k3gh.9.2): every
        // one of these references `accounts (owner, id)` and nothing else,
        // so any order among themselves is fine as long as they follow the
        // accounts loop above. Each conflict resolves to `DO NOTHING`: an
        // archive supplies what is missing and never overwrites what is
        // there, the same rule `instruments` follows below — and
        // `account_retirements` in particular has a trigger that would abort
        // an `UPDATE`, so `DO NOTHING` is not merely a style choice there.
        for alias in &bundle.account_aliases {
            transaction.execute(
                "INSERT INTO account_aliases (owner, account, value, valid_from, valid_to)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (owner, account, value, valid_from) DO NOTHING",
                params![
                    owner.inner().to_string(),
                    alias.account.to_string(),
                    alias.value,
                    alias.valid_from,
                    alias.valid_to,
                ],
            )?;
        }

        for exclusion in &bundle.account_scope_exclusions {
            transaction.execute(
                "INSERT INTO account_scope_exclusions (owner, account, reason, recorded_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (owner, account) DO NOTHING",
                params![
                    owner.inner().to_string(),
                    exclusion.account.to_string(),
                    exclusion.reason,
                    exclusion.recorded_at,
                ],
            )?;
        }

        for transfer in &bundle.account_transfers {
            transaction.execute(
                "INSERT INTO account_transfer_statements (owner, account, recorded_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (owner, account) DO NOTHING",
                params![
                    owner.inner().to_string(),
                    transfer.account.to_string(),
                    transfer.recorded_at,
                ],
            )?;
            for partner in &transfer.partners {
                transaction.execute(
                    "INSERT INTO account_transfer_partners (owner, account, partner)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT (owner, account, partner) DO NOTHING",
                    params![
                        owner.inner().to_string(),
                        transfer.account.to_string(),
                        partner.to_string(),
                    ],
                )?;
            }
        }

        // The whole append-only history, in the revision order the section
        // already carries it: a fresh `INSERT` with the original revision
        // number, never `record_account_retirement`'s own MAX+1 allocation,
        // because that would renumber every row and disconnect a published
        // report from the revision it named.
        for retirement in &bundle.account_retirements {
            transaction.execute(
                "INSERT INTO account_retirements (owner, revision, account, effective_on, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (owner, revision) DO NOTHING",
                params![
                    owner.inner().to_string(),
                    retirement.revision,
                    retirement.account.to_string(),
                    retirement.effective_on,
                    retirement.recorded_at,
                ],
            )?;
        }

        for declined in &bundle.declined_account_names {
            transaction.execute(
                "INSERT INTO declined_account_names (owner, printed, reason, recorded_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (owner, printed) DO NOTHING",
                params![
                    owner.inner().to_string(),
                    declined.printed,
                    declined.reason,
                    declined.recorded_at,
                ],
            )?;
        }

        // No natural key beyond the autoincrement `id`, which this section
        // does not carry — re-importing the same bundle must still change
        // nothing, so a row is inserted only when an identical one is not
        // already there. `IS` rather than `=` compares `declared_by`
        // NULL-safely.
        for decision in &bundle.decision_history {
            let known: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM decision_history
                     WHERE owner = ?1 AND declared_by IS ?2 AND operation = ?3
                       AND subject = ?4 AND decision = ?5 AND undo = ?6 AND recorded_at = ?7",
                    params![
                        owner.inner().to_string(),
                        decision.declared_by.map(|id| id.to_string()),
                        decision.operation,
                        decision.subject,
                        decision.decision,
                        decision.undo,
                        decision.recorded_at,
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                continue;
            }
            transaction.execute(
                "INSERT INTO decision_history
                     (owner, declared_by, operation, subject, decision, undo, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    owner.inner().to_string(),
                    decision.declared_by.map(|id| id.to_string()),
                    decision.operation,
                    decision.subject,
                    decision.decision,
                    decision.undo,
                    decision.recorded_at,
                ],
            )?;
        }

        // Before the events loop: `PRAGMA foreign_keys` is on
        // (`store/src/lib.rs:239`) and no constraint in this schema is
        // deferred, so SQLite checks at the statement that violates it, not
        // at COMMIT. Order here is a requirement, not a habit.
        for place in &bundle.custody_places {
            transaction.execute(
                "INSERT INTO custody_places (id, owner, title, institution, origin, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET
                     title = excluded.title,
                     institution = excluded.institution
                 WHERE custody_places.owner = excluded.owner",
                params![
                    place.id.to_string(),
                    owner.inner().to_string(),
                    place.title,
                    place.institution,
                    place.origin,
                    created_at,
                ],
            )?;
        }

        // An archive supplies what is missing and never overwrites what is
        // there. Instruments are global reference data shared with whatever
        // else this database holds, so a restore must not silently rewrite a
        // live directory with a snapshot of an older one. Accounts and
        // custody places are the owner's own and do update, above.
        //
        // `instruments.lineage_parent` is a self-referencing foreign key, so
        // an instrument whose parent is also in this section must be
        // inserted after it. The closure query includes every such parent
        // transitively, but does not order by lineage, so this inserts in
        // passes: each pass inserts every remaining instrument whose parent
        // is not itself still pending, and stops once a pass inserts
        // nothing — which happens exactly when every instrument is in.
        let mut pending: Vec<&InstrumentSection> = bundle.instruments.iter().collect();
        let mut placed: HashSet<uuid::Uuid> = HashSet::new();
        while !pending.is_empty() {
            let mut still_pending = Vec::new();
            let mut progressed = false;
            for instrument in pending {
                let parent_still_pending = instrument.lineage_parent.is_some_and(|parent| {
                    bundle
                        .instruments
                        .iter()
                        .any(|candidate| candidate.id == parent)
                        && !placed.contains(&parent)
                });
                if parent_still_pending {
                    still_pending.push(instrument);
                    continue;
                }
                transaction.execute(
                    "INSERT INTO instruments
                         (id, kind, symbol, title,
                          denomination_currency, settlement_currency, quote_currency,
                          lineage_parent, lineage_reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT (id) DO NOTHING",
                    params![
                        instrument.id.to_string(),
                        instrument.kind,
                        instrument.symbol,
                        instrument.title,
                        instrument.denomination_currency,
                        instrument.settlement_currency,
                        instrument.quote_currency,
                        instrument.lineage_parent.map(|id| id.to_string()),
                        instrument.lineage_reason,
                        created_at,
                    ],
                )?;
                placed.insert(instrument.id);
                progressed = true;
            }
            if !progressed {
                // Every instrument left names a parent also left, in this
                // section: a cycle, which nothing in this codebase writes.
                // Refusing beats looping forever on an archive that cannot
                // exist honestly.
                return Err(StoreError::BundleCorrupted {
                    detail: "instrument lineage_parent forms a cycle within one bundle section"
                        .into(),
                });
            }
            pending = still_pending;
        }

        // After the instruments loop above: both name `instrument` as a real
        // foreign key. `DO NOTHING` throughout — this evidence is append-only
        // by the schema's own triggers (`instrument_aliases` has none, but
        // `issue_terms` behaves the same way in spirit: a later observation
        // at a new `observed_at` is a new row, never a correction in place).
        // Not a bare `ON CONFLICT (namespace, value, valid_from) DO NOTHING`:
        // `instrument_aliases_do_not_overlap` is a `BEFORE INSERT` trigger,
        // and SQLite runs it before it ever decides whether the row would
        // conflict — so re-importing the very same row the trigger sees as
        // "overlapping itself" and aborts, rather than the no-op a repeat
        // import needs to be. The existence check below is what
        // `ON CONFLICT` would have done, made explicit ahead of the insert
        // instead of left to the constraint machinery this table's own
        // trigger gets to first.
        for alias in &bundle.instrument_aliases {
            let known: Option<i64> = transaction
                .query_row(
                    "SELECT rowid FROM instrument_aliases
                     WHERE namespace = ?1 AND value = ?2 AND valid_from = ?3",
                    params![alias.namespace, alias.value, alias.valid_from],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                continue;
            }
            transaction.execute(
                "INSERT INTO instrument_aliases
                     (namespace, value, instrument, valid_from, valid_to, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    alias.namespace,
                    alias.value,
                    alias.instrument.to_string(),
                    alias.valid_from,
                    alias.valid_to,
                    alias.source,
                    alias.created_at,
                ],
            )?;
        }

        for terms in &bundle.issue_terms {
            transaction.execute(
                "INSERT INTO issue_terms (
                     instrument_id, source_id, observed_at, effective_from, maturity_date,
                     initial_face_value, face_currency_code, coupon_periods_per_year,
                     day_count, calendar, default_declared, default_technical, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT (instrument_id, source_id, observed_at) DO NOTHING",
                params![
                    terms.instrument_id.to_string(),
                    terms.source_id,
                    terms.observed_at,
                    terms.effective_from,
                    terms.maturity_date,
                    terms.initial_face_value,
                    terms.face_currency_code,
                    terms.coupon_periods_per_year,
                    terms.day_count,
                    terms.calendar,
                    i64::from(terms.default_declared),
                    i64::from(terms.default_technical),
                    terms.recorded_at,
                ],
            )?;
        }

        for contour in &bundle.contours {
            // The scope version is immutable: an existing one is skipped,
            // rather than overwritten.
            let known: Option<u32> = transaction
                .query_row(
                    "SELECT version FROM contour_versions
                     WHERE owner = ?1 AND contour = ?2 AND version = ?3",
                    params![
                        owner.inner().to_string(),
                        contour.contour.to_string(),
                        contour.version
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                continue;
            }
            transaction.execute(
                "INSERT INTO contour_versions (owner, contour, version, title, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    owner.inner().to_string(),
                    contour.contour.to_string(),
                    contour.version,
                    contour.title,
                    created_at,
                ],
            )?;
            for account in &contour.accounts {
                transaction.execute(
                    "INSERT INTO contour_accounts (owner, contour, version, account)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        owner.inner().to_string(),
                        contour.contour.to_string(),
                        contour.version,
                        account.to_string(),
                    ],
                )?;
            }
        }

        for group in &bundle.category_groups {
            transaction.execute(
                "INSERT INTO category_groups (id, owner, title, created_at, retired_at, is_income)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    group.id.to_string(),
                    owner.inner().to_string(),
                    group.title,
                    created_at,
                    group.retired_at,
                    i64::from(group.is_income),
                ],
            )?;
        }

        for category in &bundle.categories {
            transaction.execute(
                "INSERT INTO categories (id, owner, group_id, title, created_at, retired_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    category.id.to_string(),
                    owner.inner().to_string(),
                    category.group_id.to_string(),
                    category.title,
                    created_at,
                    category.retired_at,
                ],
            )?;
        }

        // `replaces` is a self-referencing foreign key naming a strictly
        // earlier decision, so inserting in ascending `version` order —
        // `rule_history`'s own order, re-sorted here rather than trusted, the
        // way `instruments.lineage_parent` is not trusted to arrive
        // pre-ordered either — guarantees a rule's `replaces` target is
        // already in before it is needed.
        let mut ordered_classification_rules: Vec<&ClassificationRuleSection> =
            bundle.classification_rules.iter().collect();
        ordered_classification_rules.sort_by_key(|rule| rule.version);
        for rule in ordered_classification_rules {
            transaction.execute(
                "INSERT INTO classification_rules (
                     id, owner, version,
                     counterparty_account, description_contains, source_kind,
                     source_category, owner_category, source_code, movement,
                     outcome_kind, to_account, fee_origin, income_kind,
                     created_at, retired_at, replaces
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    rule.id.to_string(),
                    owner.inner().to_string(),
                    rule.version,
                    rule.counterparty_account,
                    rule.description_contains,
                    rule.source_kind,
                    rule.source_category,
                    rule.owner_category,
                    rule.source_code,
                    rule.movement,
                    rule.outcome_kind,
                    rule.to_account.map(|id| id.to_string()),
                    rule.fee_origin,
                    rule.income_kind,
                    rule.created_at,
                    rule.retired_at,
                    rule.replaces.map(|id| id.to_string()),
                ],
            )?;
        }

        let mut ordered_category_rules: Vec<&CategoryRuleSection> =
            bundle.category_rules.iter().collect();
        ordered_category_rules.sort_by_key(|rule| rule.version);
        for rule in ordered_category_rules {
            transaction.execute(
                "INSERT INTO category_rules (
                     id, owner, version, matcher_kind, value, text, description_mode,
                     category, valid_from, valid_to, created_at, retired_at, replaces
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    rule.id.to_string(),
                    owner.inner().to_string(),
                    rule.version,
                    rule.matcher_kind,
                    rule.value,
                    rule.text,
                    rule.description_mode,
                    rule.category.to_string(),
                    rule.valid_from,
                    rule.valid_to,
                    rule.created_at,
                    rule.retired_at,
                    rule.replaces.map(|id| id.to_string()),
                ],
            )?;
        }

        let mut inserted = 0;
        let mut duplicates = 0;
        for event in &bundle.events {
            if find_duplicate(
                &transaction,
                event,
                iaam_core::reconciliation::evidence::IdentityScope::Source,
            )?
            .is_some()
            {
                duplicates += 1;
                continue;
            }
            insert_event_in(&transaction, event)?;
            inserted += 1;
        }

        // After the events loop: this projection references `events (id)`.
        for assignment in &bundle.event_category_assignments {
            transaction.execute(
                "INSERT INTO event_category_assignments
                     (owner, event, category, rule, basis, rules_revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (owner, event) DO NOTHING",
                params![
                    owner.inner().to_string(),
                    assignment.event.to_string(),
                    assignment.category.to_string(),
                    assignment.rule.to_string(),
                    assignment.basis,
                    assignment.rules_revision,
                ],
            )?;
        }

        // --- Evidence the owner acquired and cannot re-fetch (iaam-k3gh.9.3) ---
        //
        // `source_documents` before `raw_rows` (a real foreign key);
        // `import_sessions` before its four dependents (also real foreign
        // keys, `document_unresolved_accounts` included); `sync_runs` before
        // every observation table naming a `sync_run_id` — all four
        // orderings below follow directly from the schema. None of these
        // tables is self-referential, so — unlike `instruments`,
        // `classification_rules` and `category_rules` above — a single pass
        // in bundle order is enough for each.

        for document in &bundle.source_documents {
            transaction.execute(
                "INSERT INTO source_documents
                     (id, owner, broker, format, parser_version, document_hash, uploaded_at, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    document.id.to_string(),
                    owner.inner().to_string(),
                    document.broker,
                    document.format,
                    document.parser_version,
                    document.document_hash,
                    document.uploaded_at,
                    document.body,
                ],
            )?;
        }

        // `raw_rows` has no primary key of its own — only the unique index
        // `raw_rows_by_locator` on `(document, ifnull(sheet, ''), row)` — so
        // idempotency is a manual existence check, the same shape
        // `decision_history` below uses for the same reason: a natural key
        // exists, but it is not one `ON CONFLICT` can target directly against
        // a bare `INSERT`.
        for row in &bundle.raw_rows {
            let known: Option<i64> = transaction
                .query_row(
                    "SELECT rowid FROM raw_rows
                     WHERE document = ?1 AND sheet IS ?2 AND row = ?3",
                    params![row.document.to_string(), row.sheet, row.row],
                    |sql_row| sql_row.get(0),
                )
                .optional()?;
            if known.is_some() {
                continue;
            }
            transaction.execute(
                "INSERT INTO raw_rows (document, sheet, row, payload, status)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    row.document.to_string(),
                    row.sheet,
                    row.row,
                    row.payload,
                    row.status,
                ],
            )?;
        }

        for session in &bundle.import_sessions {
            transaction.execute(
                "INSERT INTO import_sessions (id, owner, state, source, import, opened_at, closed_at, account)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    session.id.to_string(),
                    owner.inner().to_string(),
                    session.state,
                    session.source.map(|id| id.to_string()),
                    session.import.map(|id| id.to_string()),
                    session.opened_at,
                    session.closed_at,
                    session.account.map(|id| id.to_string()),
                ],
            )?;
        }

        for unresolved in &bundle.document_unresolved_accounts {
            transaction.execute(
                "INSERT INTO document_unresolved_accounts
                     (owner, document_hash, printed, ordinal, records, import_session, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (owner, document_hash, printed) DO NOTHING",
                params![
                    owner.inner().to_string(),
                    unresolved.document_hash,
                    unresolved.printed,
                    unresolved.ordinal,
                    unresolved.records,
                    unresolved.import_session.to_string(),
                    unresolved.recorded_at,
                ],
            )?;
        }

        for observation in &bundle.import_observations {
            transaction.execute(
                "INSERT INTO import_observations
                     (session, row, row_key, concluded, payload, answer, answer_rule, answer_rule_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (session, row) DO NOTHING",
                params![
                    observation.session.to_string(),
                    observation.row,
                    observation.row_key,
                    i64::from(observation.concluded),
                    observation.payload,
                    observation.answer,
                    observation.answer_rule.map(|id| id.to_string()),
                    observation.answer_rule_version,
                ],
            )?;
        }

        for question in &bundle.import_questions {
            transaction.execute(
                "INSERT INTO import_questions
                     (id, session, row, question, alternatives, prompt, asked_at,
                      answered_at, answer, rule)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    question.id.to_string(),
                    question.session.to_string(),
                    question.row,
                    question.question,
                    question.alternatives,
                    question.prompt,
                    question.asked_at,
                    question.answered_at,
                    question.answer,
                    question.rule.map(|id| id.to_string()),
                ],
            )?;
        }

        for figure in &bundle.import_control_figures {
            transaction.execute(
                "INSERT INTO import_control_figures
                     (session, account, currency, period_from, period_to,
                      opening, closing, debit_turnover, credit_turnover, stated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (session, account, currency) DO NOTHING",
                params![
                    figure.session.to_string(),
                    figure.account,
                    figure.currency,
                    figure.period_from,
                    figure.period_to,
                    figure.opening,
                    figure.closing,
                    figure.debit_turnover,
                    figure.credit_turnover,
                    figure.stated_at,
                ],
            )?;
        }

        // Instance-wide dictionaries and ledgers, not owner-scoped — see the
        // module doc comment.
        for version in &bundle.source_profile_versions {
            transaction.execute(
                "INSERT INTO source_profile_versions (id, version, digest, first_loaded_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id, version) DO NOTHING",
                params![
                    version.id,
                    version.version,
                    version.digest,
                    version.first_loaded_at
                ],
            )?;
        }

        for kind in &bundle.broker_operation_kinds {
            transaction.execute(
                "INSERT INTO broker_operation_kinds (broker, source_kind, kind, origin, dictionary, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (broker, source_kind) DO NOTHING",
                params![
                    kind.broker,
                    kind.source_kind,
                    kind.kind,
                    kind.origin,
                    kind.dictionary,
                    kind.recorded_at,
                ],
            )?;
        }

        for code in &bundle.market_source_codes {
            transaction.execute(
                "INSERT INTO market_source_codes (source_id, domain, source_code, meaning, origin, dictionary, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (source_id, domain, source_code) DO NOTHING",
                params![
                    code.source_id,
                    code.domain,
                    code.source_code,
                    code.meaning,
                    code.origin,
                    code.dictionary,
                    code.recorded_at,
                ],
            )?;
        }

        // `sync_runs` before every observation table below that names one by
        // `sync_run_id` — a real foreign key in every case.
        for run in &bundle.sync_runs {
            transaction.execute(
                "INSERT INTO sync_runs (
                     id, source_id, dataset, series_key, status,
                     requested_from, requested_to, covered_from, covered_to,
                     pages, rows, page_errors, rate_limit_hits, raw_hash,
                     lease_token, lease_expires_at, started_at, finished_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    run.id.to_string(),
                    run.source_id,
                    run.dataset,
                    run.series_key,
                    run.status,
                    run.requested_from,
                    run.requested_to,
                    run.covered_from,
                    run.covered_to,
                    run.pages,
                    run.rows,
                    run.page_errors,
                    run.rate_limit_hits,
                    run.raw_hash,
                    run.lease_token,
                    run.lease_expires_at,
                    run.started_at,
                    run.finished_at,
                ],
            )?;
        }

        for price in &bundle.price_observations {
            transaction.execute(
                "INSERT INTO price_observations (
                     instrument_id, board, session, trade_date, kind, source_id, observed_at,
                     price, currency, quotation_basis, basis_evidence, executability,
                     raw_hash, sync_run_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT (instrument_id, board, session, trade_date, kind, source_id, observed_at)
                 DO NOTHING",
                params![
                    price.instrument_id.to_string(),
                    price.board,
                    price.session,
                    price.trade_date,
                    price.kind,
                    price.source_id,
                    price.observed_at,
                    price.price,
                    price.currency,
                    price.quotation_basis,
                    price.basis_evidence,
                    price.executability,
                    price.raw_hash,
                    price.sync_run_id.to_string(),
                ],
            )?;
        }

        for fx in &bundle.fx_observations {
            transaction.execute(
                "INSERT INTO fx_observations (
                     from_code, to_code, trade_date, source_id, observed_at,
                     nominal, value, unit_rate, raw_hash, sync_run_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (from_code, to_code, trade_date, source_id, observed_at) DO NOTHING",
                params![
                    fx.from_code,
                    fx.to_code,
                    fx.trade_date,
                    fx.source_id,
                    fx.observed_at,
                    fx.nominal,
                    fx.value,
                    fx.unit_rate,
                    fx.raw_hash,
                    fx.sync_run_id.to_string(),
                ],
            )?;
        }

        for key_rate in &bundle.key_rate_observations {
            transaction.execute(
                "INSERT INTO key_rate_observations (trade_date, source_id, observed_at, rate, raw_hash, sync_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (trade_date, source_id, observed_at) DO NOTHING",
                params![
                    key_rate.trade_date,
                    key_rate.source_id,
                    key_rate.observed_at,
                    key_rate.rate,
                    key_rate.raw_hash,
                    key_rate.sync_run_id.to_string(),
                ],
            )?;
        }

        for completeness in &bundle.series_completeness {
            transaction.execute(
                "INSERT INTO series_completeness
                     (source_id, dataset, series_key, complete_through, updated_at, last_successful_run)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (source_id, dataset, series_key) DO NOTHING",
                params![
                    completeness.source_id,
                    completeness.dataset,
                    completeness.series_key,
                    completeness.complete_through,
                    completeness.updated_at,
                    completeness.last_successful_run.map(|id| id.to_string()),
                ],
            )?;
        }

        // No natural key besides the bare autoincrement `id`, which this
        // section does not carry (see the section type's own doc comment):
        // a manual existence check, the same shape `decision_history` and
        // `raw_rows` above use for the same reason.
        for interest in &bundle.accrued_interest_observations {
            let known: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM accrued_interest_observations
                     WHERE instrument_id = ?1 AND board = ?2 AND session = ?3
                       AND trade_date = ?4 AND source_id = ?5 AND observed_at = ?6",
                    params![
                        interest.instrument_id.to_string(),
                        interest.board,
                        interest.session,
                        interest.trade_date,
                        interest.source_id,
                        interest.observed_at,
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                continue;
            }
            transaction.execute(
                "INSERT INTO accrued_interest_observations (
                     instrument_id, board, session, trade_date, source_id, observed_at,
                     per_unit, currency, raw_hash, sync_run_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    interest.instrument_id.to_string(),
                    interest.board,
                    interest.session,
                    interest.trade_date,
                    interest.source_id,
                    interest.observed_at,
                    interest.per_unit,
                    interest.currency,
                    interest.raw_hash,
                    interest.sync_run_id.to_string(),
                ],
            )?;
        }

        transaction.commit()?;
        Ok(ImportOutcome::Applied {
            inserted,
            duplicates,
        })
    }
}

fn parse(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}

/// `?,?,...` for `count` placeholders, for an `IN (...)` clause whose arity
/// (the size of the instrument closure) is not known until the export runs.
/// The same idiom `journal::read::placeholders` already uses, duplicated
/// rather than shared across a crate-private boundary neither module has a
/// reason to cross otherwise.
fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}
