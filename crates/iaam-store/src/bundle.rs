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
//! What is not yet included in the stage 1 bundle, and why: market data and
//! rates beyond the instrument catalogue itself (to be added in E3), tax
//! context (E5), and classification rules (E2). Each of these sections will
//! be added to the bundle with its own epic, and that is what the format
//! version is for.

use std::collections::HashSet;

use iaam_core::custody::CustodyOrigin;
use iaam_core::event::Event;
use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params};
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
pub const BUNDLE_VERSION: u32 = 3;

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

/// The complete bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_version: u32,
    pub schema_version: u32,
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
}

impl Bundle {
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
    // --- Evidence the owner acquired and cannot re-fetch (iaam-k3gh.9.3). ---
    (
        "instrument_aliases",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "source_documents",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    ("raw_rows", TableDisposition::Pending("iaam-k3gh.9.3")),
    (
        "document_unresolved_accounts",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "source_profile_versions",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    // A durable synchronization run, the same operational-evidence shape as
    // the observation tables it leases for below: not a decision, not
    // recomputable from the journal, not a secret.
    ("sync_runs", TableDisposition::Pending("iaam-k3gh.9.3")),
    (
        "price_observations",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "fx_observations",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "key_rate_observations",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "series_completeness",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "accrued_interest_observations",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "broker_operation_kinds",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "schedule_coupon_periods",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "schedule_principal_repayments",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "schedule_offer_windows",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    ("issue_terms", TableDisposition::Pending("iaam-k3gh.9.3")),
    (
        "market_source_codes",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "import_sessions",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "import_observations",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "import_questions",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
    (
        "import_control_figures",
        TableDisposition::Pending("iaam-k3gh.9.3"),
    ),
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
        let accounts = self
            .list_accounts(owner)?
            .into_iter()
            .map(|record| AccountSection {
                id: record.id.inner(),
                title: record.title,
                institution: record.institution,
            })
            .collect();

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

        let mut bundle = Bundle {
            bundle_version: BUNDLE_VERSION,
            schema_version: crate::schema::SCHEMA_VERSION,
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
            transaction.execute(
                "INSERT INTO accounts (id, owner, title, institution, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
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
