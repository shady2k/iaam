//! Row shapes for the journal tables (spec §4).
//!
//! One struct per table, and nothing else here: no SQL, no domain logic, no
//! reconstruction. `kind.rs` is the only reader and writer of these shapes.
//!
//! A value spec §4.5 lists as **reconstructed** — recovered from
//! `event_legs` rather than stored a second time — has **no field** on the
//! row that would otherwise carry it: `CashIn.amount`, `Fee.amount`,
//! `CashTransfer.amount`, the `CorporateAction` compensation in all three
//! variants, and the rest of §4.5's list. Adding one back "for symmetry"
//! reopens the two-places-that-can-disagree problem D5 closes.
//!
//! A corporate action's `compensation` is the sharpest case, so it is worth
//! saying why it has no field on [`CorporateActionRow`] rather than leaving a
//! reader to wonder. `validate_corporate_action` builds the leg it expects
//! **out of** the compensation — `principal_leg` for either redemption, the
//! optional `cash_leg` for a conversion — and `expect_legs` admits exactly the
//! legs it lists, so the leg is unique and the two determine each other in
//! both directions, absence included. The schema briefly carried
//! `compensation_amount` and `compensation_currency` anyway; they were removed
//! once that was checked, because a column holding what a leg already holds is
//! the duplication D5 exists to refuse.

/// One row of `events`: the envelope, the six optional dates, and
/// provenance (spec §4.1).
///
/// `recorded_at` is not part of the domain [`iaam_core::event::Event`] — it
/// is stamped by the write path at insert time — so [`super::kind::to_rows`]
/// leaves it empty and the writer overwrites it before the row is ever sent
/// to SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EventRow {
    pub id: String,
    pub owner: String,
    pub account: String,
    pub kind: String,
    pub effective_date: String,
    pub source_time: Option<String>,
    pub sequence: i64,
    pub confidence: String,
    pub relation_kind: String,
    pub relation_target: Option<String>,
    pub idempotency_key: Option<String>,
    pub recorded_at: String,

    // EventDates, all optional.
    pub date_trade: Option<String>,
    pub date_settled: Option<String>,
    pub date_cash_posted: Option<String>,
    pub date_entitlement: Option<String>,
    pub date_paid: Option<String>,
    pub tax_period_override: Option<i64>,

    // Provenance.
    pub source: String,
    pub raw_hash: String,
    pub parser_version: String,
    pub source_operation_id: Option<String>,
    pub source_category: Option<String>,
    pub source_kind: Option<String>,
    pub owner_category: Option<String>,
    pub source_code: Option<String>,
    pub source_description: Option<String>,
    pub import: Option<String>,
    pub import_session: Option<String>,
    pub declared_by: Option<String>,
    pub rule_settlement: Option<String>,
    pub settled_by_rule: Option<String>,
    pub settled_by_rule_version: Option<i64>,
    pub row_document: Option<String>,
    pub row_sheet: Option<String>,
    pub row_number: Option<i64>,
}

/// One row of `event_legs` (spec §4.2).
///
/// `ordinal` fixes a stable order for reconstruction and carries no other
/// meaning: [`super::kind::from_rows`] never looks a leg up by it, only by
/// kind and role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegRow {
    pub event: String,
    pub ordinal: i64,
    pub kind: String,
    pub account: String,
    pub custody: Option<String>,
    pub instrument: Option<String>,
    pub amount: Option<i64>,
    pub currency: Option<String>,
    pub quantity: Option<String>,
}

/// `event_trade`. `gross`, `fee`, `basis_fee`, `basis_fee_exact` and
/// `accrued_interest` are all **stored** (spec §4.5): `gross` cannot be
/// recovered from the settlement leg without unwinding the fee and the
/// accrued interest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TradeRow {
    pub event: String,
    pub side: String,
    pub instrument: String,
    pub quantity: String,
    pub gross_amount: i64,
    pub gross_currency: String,
    pub fee_amount: Option<i64>,
    pub fee_currency: Option<String>,
    pub basis_fee_amount: Option<i64>,
    pub basis_fee_currency: Option<String>,
    pub basis_fee_exact_value: Option<String>,
    pub basis_fee_exact_currency: Option<String>,
    pub accrued_interest_amount: Option<i64>,
    pub accrued_interest_currency: Option<String>,
}

/// `event_cash_transfer`. `amount` is reconstructed from the positive `to`
/// leg (spec §4.5) and is deliberately absent here; both endpoints are
/// stored because both legs are `LegKind::Cash` and carry no role, so
/// nothing else says which leg is which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CashTransferRow {
    pub event: String,
    pub transfer_id: String,
    pub from_account: String,
    pub to_account: String,
}

/// `event_unresolved_movement`. This variant posts no leg by design, so its
/// amount and currency are stored directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnresolvedMovementRow {
    pub event: String,
    pub amount: i64,
    pub currency: String,
}

/// `event_income`. `gross` is reconstructed from the cash leg (spec §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IncomeRow {
    pub event: String,
    pub instrument: Option<String>,
    pub income_kind: Option<String>,
}

/// `event_fee`. `amount` is reconstructed from the fee leg (spec §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeeRow {
    pub event: String,
    pub origin: String,
}

/// `event_tax`. `amount` is reconstructed from the tax leg (spec §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaxRow {
    pub event: String,
    pub origin: String,
}

/// `event_opening_position`. `quantity` here is the reconstructed position's
/// own value — a stored field, not the security leg's quantity restated —
/// and `OpeningAssertions` is nine scalar columns beside it (spec §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpeningPositionRow {
    pub event: String,
    pub instrument: String,
    pub quantity: String,
    pub cost_basis_amount: Option<i64>,
    pub cost_basis_currency: Option<String>,
    pub quantity_certainty: String,
    pub acquisition_date: Option<String>,
    pub acquisition_date_certainty: String,
    pub tax_basis_certainty: String,
    pub basis_currency: Option<String>,
    pub basis_rate: Option<String>,
    pub fees_included: String,
    pub ldv_eligibility: String,
    pub prior_corporate_actions: String,
}

/// `event_valuation`. Moves no money: the event has no legs, so every field
/// here is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValuationRow {
    pub event: String,
    pub instrument: String,
    pub price: String,
    pub currency: String,
    pub quality: String,
}

/// `event_control_assertion`. `ControlClaim`'s six variants share this one
/// family table with a discriminant and conditional columns (spec §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlAssertionRow {
    pub event: String,
    pub period_from: String,
    pub period_to: String,
    pub claim_kind: String,
    pub balance_point: Option<String>,
    pub currency: Option<String>,
    pub amount: Option<i64>,
    pub instrument: Option<String>,
    pub custody: Option<String>,
    pub quantity: Option<String>,
    pub debit: Option<i64>,
    pub credit: Option<i64>,
}

/// `event_coverage_gap`. `refused` and the top-level `dimensions` set are
/// **not** stored (spec §4.4): they are a count and a union over
/// [`CoverageGapSourceRow`]/[`CoverageGapDimensionRow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageGapRow {
    pub event: String,
    pub period_from: String,
    pub period_to: String,
}

/// One row of `event_coverage_gap_rows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageGapSourceRow {
    pub event: String,
    pub ordinal: i64,
    pub source: String,
    pub row_name_kind: String,
    pub row_name_value: String,
}

/// One row of `event_coverage_gap_row_dimensions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageGapDimensionRow {
    pub event: String,
    pub ordinal: i64,
    pub dimension: String,
}

/// `event_corporate_action`. `CorporateAction`'s three variants share this
/// family table with a discriminant and conditional columns (spec §4.3).
///
/// `compensation` is deliberately absent: spec §4.5 reconstructs it, in all
/// three variants, from the `Principal` leg (or the optional `Cash` leg, for
/// a conversion) — see the module comment above for the schema column this
/// leaves unfilled by this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CorporateActionRow {
    pub event: String,
    pub action_kind: String,
    pub instrument: Option<String>,
    pub predecessor: Option<String>,
    pub successor: Option<String>,
    pub custody: String,
    pub quantity: Option<String>,
    pub quantity_in: Option<String>,
    pub quantity_out: Option<String>,
    pub ratio: Option<String>,
    pub principal_returned_per_unit_value: Option<String>,
    pub principal_returned_per_unit_currency: Option<String>,
    pub fractional: Option<String>,
    pub basis_transfer: Option<String>,
    pub effective_date: String,
    pub record_date: Option<String>,
    pub grounds: Option<String>,
    pub allocation_kind: Option<String>,
    pub allocation_gap: Option<String>,
    pub allocation_share: Option<String>,
    pub allocation_inputs_hash: Option<String>,
    pub allocation_known_as_of: Option<String>,
    pub allocation_algorithm: Option<i64>,
}

/// `event_offer_exercise`. `OfferExerciseAction`'s three variants share this
/// one family table (spec §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OfferExerciseRow {
    pub event: String,
    pub action_kind: String,
    pub submission: String,
    pub window: Option<String>,
    pub instrument: Option<String>,
    pub custody: Option<String>,
    pub quantity: String,
    pub gross_amount: Option<i64>,
    pub gross_currency: Option<String>,
    pub fee_amount: Option<i64>,
    pub fee_currency: Option<String>,
    pub accrued_interest_amount: Option<i64>,
    pub accrued_interest_currency: Option<String>,
}

/// The whole family of a corporate action's coverage-gap sub-rows: the
/// header plus its two collections (spec §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageGapDetail {
    pub header: CoverageGapRow,
    pub sources: Vec<CoverageGapSourceRow>,
    pub dimensions: Vec<CoverageGapDimensionRow>,
}

/// Everything beyond the legs that one `EventKind` family carries (spec
/// §4.3). Five variants — `CashIn`, `CashOut`, `Refund`, `OpeningCash` and
/// `OwnAccountMovement` — carry nothing beyond `events.kind` and the legs,
/// and map to [`Self::None`].
// `CorporateActionRow` and `OpeningPositionRow` are boxed: both carry enough
// columns that leaving them inline would make every `DetailRows` value pay
// for the largest variant's size, most visibly in the `Vec<DetailRows>`
// Task 6's bulk hydration builds per page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetailRows {
    Trade(TradeRow),
    CashTransfer(CashTransferRow),
    UnresolvedMovement(UnresolvedMovementRow),
    Income(IncomeRow),
    Fee(FeeRow),
    Tax(TaxRow),
    OpeningPosition(Box<OpeningPositionRow>),
    Valuation(ValuationRow),
    ControlAssertion(ControlAssertionRow),
    CoverageGap(CoverageGapDetail),
    CorporateAction(Box<CorporateActionRow>),
    OfferExercise(OfferExerciseRow),
    None,
}
