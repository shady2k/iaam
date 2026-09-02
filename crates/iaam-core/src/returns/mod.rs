//! Return report (§6.1, §10.5, §16.3).
//!
//! An honest description of the stage 1 result: **XIRR before tax** for
//! simple long-only securities. Taxes appear in E5, and until then no
//! no field in this report pretends to be a return after tax.
//!
//! **Report period — the entire account history.** XIRR over an arbitrary interval
//! requires an opening NAV valuation as a terminal cash flow,
//! and stage 1 valuation exists only as of the report date. Calculating
//! the interval by substituting cost basis for the opening value,
//! would mean presenting as return a value that does not correspond to
//! any transaction.

pub mod xirr;
pub mod zero_reinvestment;

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Date, OffsetDateTime, UtcOffset};

use crate::bond::{
    BondSchedule,
    finality::{PrincipalReturnFinality, finality_of},
    offer::{OfferChoice, available_choices},
    posting::next_posting_date,
};
use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::ids::{AccountId, InstrumentId, SourceId};
use crate::money::PerUnitAmount;
use crate::money::{CalcMoney, CurrencyCode};
use crate::numeric::approx::SolverPolicy;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverRefusal};
use crate::perimeter::{PerimeterAssessment, PerimeterPolicy};
use crate::projection::lots::{BasisGap, LotKey};
use crate::projection::offers::{OfferBook, unresolved_submissions};
use crate::projection::ownership::Ownership;
use crate::projection::state::LedgerState;
use crate::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use crate::rules::lot_disposal::RuleId;
use crate::rules::quotation::{QuotationError, QuotationRule, QuotationRuleVersion, QuotationV1};
use crate::rules::{
    AccruedInterestError, AccruedInterestRule, AccruedInterestRuleVersion, AccruedInterestV1,
    CashflowInput, CashflowPlan, CashflowProjectionVersion, PostingKind, PostingMatchV2,
    PostingMatchVersion, SourcePriorityVersion, ValuationPolicyV1, ValuationRule, Verdict,
    historical_schedule_postings,
};
use crate::valuation::{
    FxSource, FxTable, LegacyValuationOutcome, PriceCandidate, PriceQuality, PriceQuery,
    QuotationBasis, SelectedPrice, SourceExecutability, UncoveredReason as PolicyUncoveredReason,
    ValuationError, Venue, candidate_from_legacy_valuation,
};

/// A value the system may refuse to compute.
///
/// Failure — part of the contract, not an exceptional condition: an unknown
/// price, a missing exchange rate, and an equation with no unique root
/// occur during normal operation (§5.4, §6.1, §10.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Computed<T> {
    Value(T),
    NotComputable { reason: NotComputable },
}

impl<T> Computed<T> {
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Value(v) => Some(v),
            Self::NotComputable { .. } => None,
        }
    }

    #[must_use]
    pub const fn reason(&self) -> Option<&NotComputable> {
        match self {
            Self::Value(_) => None,
            Self::NotComputable { reason } => Some(reason),
        }
    }
}

/// Why the value was not computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotComputable {
    /// No instrument price: the position value is unknown.
    MissingPrice { instrument: InstrumentId },
    /// No exchange rate for the date.
    MissingFxRate {
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
    },
    /// The quote basis is not substantiated by the source.
    QuotationBasisUnknown { instrument: InstrumentId },
    /// The recorded basis conflicts with the source evidence.
    QuotationBasisContradictsEvidence { instrument: InstrumentId },
    /// The bond's face value is unknown.
    RemainingFaceUnknown { instrument: InstrumentId },
    /// No face value was provided for quote conversion.
    PrincipalUnknown,
    /// The solver failed: no root, multiple roots, or no convergence.
    SolverRefused { refusal: SolverRefusal },
    /// No flows cross the perimeter boundary.
    NoExternalFlows,
    /// The log slice contains events after the report date: it was assembled incorrectly.
    ///
    /// A defect report, not a refusal the owner can act on: the answer is
    /// reported as a fault in the system, never paraphrased to him as a gap in
    /// his data. Every other variant here translates into something to tell
    /// him; this one and `Numeric` do not.
    StateNewerThanReport { last_event: Date, as_of: Date },
    /// Arithmetic impossibility: overflow, division by zero.
    ///
    /// A defect report, like `StateNewerThanReport`: the arithmetic could not
    /// be performed, which is a fault in the system rather than something
    /// missing from the owner's data.
    Numeric { code: &'static str },
    /// The account has funding outside the perimeter: the system does not reconstruct the economics.
    UnsupportedFinancing { account: AccountId },
    /// No issue schedule snapshot exists at the knowledge coordinate.
    ScheduleMissing { instrument: InstrumentId },
    /// No accrued interest observation exists for the exit date.
    AccruedObservationMissing { instrument: InstrumentId },
    /// The current period coupon amount is unknown.
    CouponUndetermined { instrument: InstrumentId },
    /// The report date is outside the schedule coverage.
    OutsideScheduleCoverage { instrument: InstrumentId },
    /// The report date is covered by multiple schedule periods.
    OverlappingScheduleCoverage { instrument: InstrumentId },
    /// No feasible exit: accrued interest cannot be realized today.
    ExitNotExecutable,
    /// The horizon end date is no later than the metric coordinate.
    NonPositiveDuration {
        coordinate: Date,
        terminal_date: Date,
    },
    /// The initial value is not positive.
    NonPositiveInitialCapital,
    /// Terminal wealth is negative.
    NegativeTerminalWealth,
    /// The cohort's historical acquisition cost is unknown.
    AcquisitionBasisUnknown,
    /// Accrued interest paid on acquisition is unknown.
    AccruedInterestAtAcquisitionUnknown,
    /// The received payment history was aggregated in an unknown way.
    HistoricalReceiptsUnknown,
    /// The cohort cannot be constructed.
    CohortGap {
        gap: crate::projection::lots::CohortGap,
    },
    /// Monetary amounts have different currencies.
    CurrencyMismatch {
        expected: CurrencyCode,
        actual: CurrencyCode,
    },
    /// The expense is unknown and has no upper bound.
    ExpenseUnknown,
}

/// The refusal vocabulary: every variant, its wire code, and what the code means.
///
/// This is the single source for both. `NotComputable::code` below is expanded
/// from it, and so is the enumerated, described `not_computable` schema the API
/// publishes: pass the name of a macro that accepts
/// `Variant => "code": "meaning",` arms and it will be called with the whole
/// list. A refusal therefore reaches the caller with a sentence saying why,
/// and the sentence cannot drift away from the code, because neither is
/// written twice.
///
/// The meaning explains the code, not the instance: which instrument had no
/// price is a property of one refusal and travels in `detail`.
#[macro_export]
macro_rules! not_computable_vocabulary {
    ($receiver:path) => {
        $receiver! {
            MissingPrice => "missing_price":
                "There is no price for the instrument, so the position cannot be valued: a valuation as of the report date is needed.",
            MissingFxRate => "missing_fx_rate":
                "There is no exchange rate for the pair on that date, so the amount cannot be expressed in the report currency.",
            QuotationBasisContradictsEvidence => "quotation_basis_contradicts_evidence":
                "The recorded quotation basis contradicts the source evidence, so the quote is not converted into money.",
            QuotationBasisUnknown => "quotation_basis_unknown":
                "The source does not substantiate what the quote is expressed in, so it is not converted into money.",
            RemainingFaceUnknown => "remaining_face_unknown":
                "The remaining face value of the bond is unknown, so a percentage quote cannot be turned into an amount.",
            PrincipalUnknown => "principal_unknown":
                "No face value was supplied for converting the quote.",
            SolverRefused => "solver_refused":
                "The return equation has no unique root, or the solver did not converge: the return is not defined for this sequence of flows.",
            NoExternalFlows => "no_external_flows":
                "No flow crosses the perimeter boundary: nothing was contributed, so there is no return to compute.",
            StateNewerThanReport => "state_newer_than_report":
                "The journal slice contains events later than the report date: it was assembled incorrectly. Report this as a defect in the system, not to the owner as a gap in his data.",
            Numeric => "numeric":
                "Arithmetic was impossible — an overflow or a division by zero. Report this as a defect in the system, not to the owner as a gap in his data.",
            UnsupportedFinancing => "unsupported_financing":
                "The account carries funding from outside the perimeter, so the economics of the period are not reconstructed.",
            ScheduleMissing => "schedule_missing":
                "No issue schedule exists at the knowledge coordinate, so the payments cannot be derived.",
            AccruedObservationMissing => "accrued_observation_missing":
                "There is no accrued interest observation for the exit date.",
            CouponUndetermined => "coupon_undetermined":
                "The coupon amount of the current period is unknown.",
            OutsideScheduleCoverage => "outside_schedule_coverage":
                "The report date lies outside the coverage of the issue schedule.",
            OverlappingScheduleCoverage => "overlapping_schedule_coverage":
                "The report date is covered by several schedule periods, so no single period can be chosen.",
            ExitNotExecutable => "exit_not_executable":
                "There is no feasible exit: the accrued interest cannot be realised today.",
            NonPositiveDuration => "non_positive_duration":
                "The end of the horizon is no later than the metric coordinate, so there is no period to annualise over.",
            NonPositiveInitialCapital => "non_positive_initial_capital":
                "The initial value is not positive, so a return on it means nothing.",
            NegativeTerminalWealth => "negative_terminal_wealth":
                "Terminal wealth is negative, so the growth rate has no real root.",
            AcquisitionBasisUnknown => "acquisition_basis_unknown":
                "The historical acquisition cost of the cohort is unknown, so the realised result cannot be derived.",
            AccruedInterestAtAcquisitionUnknown => "accrued_interest_at_acquisition_unknown":
                "The accrued interest paid on acquisition is unknown.",
            HistoricalReceiptsUnknown => "historical_receipts_unknown":
                "The history of received payments was aggregated in an unknown way, so it cannot be attributed to the cohort.",
            CohortGap => "cohort_gap":
                "The cohort cannot be assembled: the history of lots has a gap.",
            CurrencyMismatch => "currency_mismatch":
                "The amounts are in different currencies and were not converted.",
            ExpenseUnknown => "expense_unknown":
                "The expense is unknown and has no upper bound, so not even a bounded estimate can be given.",
        }
    };
}

macro_rules! define_not_computable_code {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        impl NotComputable {
            /// Machine-readable code for the API (§13). The external agent parses the code,
            /// and the text is intended for humans.
            #[must_use]
            pub const fn code(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $code,)+
                }
            }
        }
    };
}

not_computable_vocabulary!(define_not_computable_code);

impl From<ValuationError> for NotComputable {
    fn from(error: ValuationError) -> Self {
        match error {
            ValuationError::MissingPrice { instrument } => Self::MissingPrice { instrument },
            ValuationError::MissingFxRate { from, to, date } => {
                Self::MissingFxRate { from, to, date }
            }
            ValuationError::Numeric(_) => Self::Numeric { code: "numeric" },
        }
    }
}

/// Data quality state (§10.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataQualityStatus {
    /// All data has been confirmed. Unattainable at stage 1: no reconciliation.
    Clean,
    /// Some data has not been independently confirmed.
    Mixed,
    /// Not enough data for a complete answer.
    Incomplete,
}

/// The data quality vocabulary: every status, its wire code, and what it means.
///
/// The same single source as `not_computable_vocabulary`: `DataQualityStatus::code`
/// and the published schema are both expanded from these arms.
#[macro_export]
macro_rules! data_quality_status_vocabulary {
    ($receiver:path) => {
        $receiver! {
            Clean => "clean":
                "There is no material issue, and the provisional and discrepant shares of value are both zero.",
            Mixed => "mixed":
                "Part of the value has no independent confirmation yet. This is a normal state, not an error: an account without an export is confirmed by the owner alone.",
            Incomplete => "incomplete":
                "A material issue affects the answer: read `material_issues` and pass it on to the owner.",
        }
    };
}

macro_rules! define_data_quality_status_code {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        impl DataQualityStatus {
            /// Machine-readable code for the API (§13).
            #[must_use]
            pub const fn code(self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $code,)+
                }
            }
        }
    };
}

data_quality_status_vocabulary!(define_data_quality_status_code);

/// Material data quality issue. Shown to the owner
/// only when it affects the result (§10.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialIssue {
    /// The position was reconstructed without documented cost (§10.7).
    RestoredWithoutBasis { account: AccountId },
    /// The amortization allocation share was not derived, so the returned
    /// position value and realized result are not calculated
    /// (§4.9). Fixed with a verified issuance schedule.
    AmortisationAllocationUnknown {
        account: AccountId,
        instrument: InstrumentId,
    },
    /// A negative cash balance is a liability in NAV (§15.9).
    NegativeCash {
        account: AccountId,
        currency: CurrencyCode,
    },
    /// No data exists before this date; anything earlier was excluded from the calculation.
    HistoryStartsAt { date: Date },
    /// There is no independent confirmation for the account (§10.5).
    NoIndependentSource {
        account: AccountId,
        dimension: Dimension,
    },
    /// Account reconciliation fails.
    Discrepancy {
        account: AccountId,
        dimension: Dimension,
    },
    /// The account includes funding outside the perimeter (§11).
    UnsupportedFinancing { account: AccountId },
    /// The submitted order references a window absent from the schedule.
    OfferWindowUnresolved {
        submission: crate::event::offer::OfferSubmissionId,
    },
    /// The scheduled payment is not confirmed by a dated fact
    /// of income.
    ///
    /// Account is required: otherwise one security in two accounts yields two
    /// indistinguishable issues. Payment type is required: «the coupon did not arrive»
    /// and «the principal repayment did not arrive» require different
    /// actions and are sought in different log events.
    ScheduledPostingNotReceived {
        account: AccountId,
        instrument: InstrumentId,
        date: Date,
        kind: PostingKind,
    },
    /// There is no basis for reconciling one scheduled payment.
    ///
    /// The date and type pinpoint the issue: inability to prove one payment
    /// must not hide a provable omission of another payment for the same pair.
    ScheduledPostingUnverifiable {
        account: AccountId,
        instrument: InstrumentId,
        date: Date,
        kind: PostingKind,
        reason: UnverifiableReason,
    },
    /// Several scheduled payments are unprovable for the same reason.
    ///
    /// An aggregation, not a list of individual issues: a source-level cause
    /// can be fixed with one action, and ten identical rows carry no
    /// ten pieces of information. The number and date boundaries are retained,
    /// because «a bad date somewhere in the history» is just as useless,
    /// as ten repetitions.
    ScheduledPostingsUnverifiable {
        account: AccountId,
        instrument: InstrumentId,
        kind: PostingKind,
        reason: UnverifiableReason,
        count: u32,
        first_date: Date,
        last_date: Date,
    },
    /// Calculated and observed accrued interest differ by more than the tolerance.
    AccruedInterestMismatch {
        instrument: InstrumentId,
        computed: Dec,
        computed_currency: CurrencyCode,
        observed: Dec,
        observed_currency: CurrencyCode,
        quantity: crate::money::Quantity,
        date: Date,
    },
}

impl MaterialIssue {
    /// Whether the issue makes the answer **incomplete**.
    ///
    /// Two issues do not constitute incompleteness and therefore do not change the status
    /// in `Incomplete`:
    ///
    /// - the start of history is a fact about the period, not a defect (§10.7);
    /// - the absence of an independent source is a normal state
    ///   data: §10.5 explicitly requires counting such records in reports on
    ///   by default, otherwise the system is useless specifically for banks without
    ///   exports and manual input. This must be shown, but the answer must not be declared
    ///   incomplete data — this must not be done, otherwise `Incomplete` will cease to
    ///   mean anything, because it will be present almost always.
    ///
    /// `navCoverage` indicates how large the unconfirmed share is,
    /// rather than a status.
    #[must_use]
    pub const fn is_defect(&self) -> bool {
        match self {
            Self::HistoryStartsAt { .. } | Self::NoIndependentSource { .. } => false,
            // The journal horizon mirrors `HistoryStartsAt`: this is a fact about
            // the period, not a defect. An owner whose journal starts
            // later than the security's issuance would otherwise get a permanent `Incomplete`
            // Other causes can be fixed by loading more facts and are therefore
            // are defects.
            Self::ScheduledPostingUnverifiable { reason, .. }
            | Self::ScheduledPostingsUnverifiable { reason, .. } => {
                !matches!(reason, UnverifiableReason::HistoryStartsAfterSchedule)
            }
            Self::AccruedInterestMismatch { .. }
            | Self::RestoredWithoutBasis { .. }
            | Self::AmortisationAllocationUnknown { .. }
            | Self::NegativeCash { .. }
            | Self::Discrepancy { .. }
            | Self::UnsupportedFinancing { .. }
            | Self::OfferWindowUnresolved { .. }
            | Self::ScheduledPostingNotReceived { .. } => true,
        }
    }
}

/// Why there is nothing to reconcile scheduled payments against (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnverifiableReason {
    /// The ownership boundary cannot be established: the lot has no acquisition date
    /// or there is a quantity reconstructed without cost basis.
    AcquisitionDateUnknown,
    /// Ownership on the payment record date cannot be proven.
    OwnershipUnknown,
    /// The source did not report the date on which payment entitlement is determined.
    EntitlementDateUnknown,
    /// The pair has a payment of unknown type: it cannot be mapped to the schedule
    /// not include it.
    IncomeKindUnknown,
    /// The pair has a payment with neither a credit date nor a payment date.
    PaymentDateUnknown,
    /// The payment crossed the ownership boundary, but its date precedes the first
    /// log event: there are no facts for it and cannot be any.
    HistoryStartsAfterSchedule,
    /// The schedule cannot serve as evidence of past payments.
    ScheduleNotTrusted,
}
impl UnverifiableReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AcquisitionDateUnknown => "acquisition_date_unknown",
            Self::OwnershipUnknown => "ownership_unknown",
            Self::EntitlementDateUnknown => "entitlement_date_unknown",
            Self::IncomeKindUnknown => "income_kind_unknown",
            Self::PaymentDateUnknown => "payment_date_unknown",
            Self::HistoryStartsAfterSchedule => "history_starts_after_schedule",
            Self::ScheduleNotTrusted => "schedule_not_trusted",
        }
    }
}

/// Reason the position was excluded from the monetary value.
///
/// The first variants represent the price-selection policy outcome. The variant
/// `NotComputable` retains the specific reason for recalculation failure, so that
/// coverage does not mark a position as valued without a monetary contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UncoveredReason {
    /// There are no observations for the instrument.
    NoObservation,
    /// All observations exceed the maximum age.
    TooOld,
    /// The venue cannot be determined unambiguously.
    AmbiguousVenue,
    /// Several candidates remain after filtering.
    AmbiguousCandidate,
    /// The selected observation cannot be converted to cash.
    NotComputable { reason: NotComputable },
}

/// A position with no selected candidate and the reason it is not covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncoveredPosition {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub reason: UncoveredReason,
}

/// A position left at the value computed by the old rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyDerivedPosition {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub quality: PriceQuality,
}

/// A position with a selected candidate and the complete policy decision rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedPosition {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub quantity: crate::money::Quantity,
    pub price: SelectedPrice,
}
/// Bond position attributes (§5.1: attributes, not a valuation basis).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondPositionAttributes {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    /// Income accrued on the position as of the date: accrued interest per bond × quantity.
    pub accrued_interest: Computed<Dec>,
    /// Amount actually realizable today (§4.2). Not contractual.
    pub accrued_interest_payable_on_termination: Computed<Dec>,
    /// Nearest payment of any kind.
    pub next_posting_date: Option<Date>,
    /// Whether the nearest principal repayment is final, if any.
    pub next_principal_return_finality: Option<PrincipalReturnFinality>,
}
/// Metrics for all scenarios of one bond position.
#[derive(Debug, Clone, PartialEq)]
pub struct BondPositionMetrics {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub scenarios: Vec<crate::returns::zero_reinvestment::BondScenarioResult>,
}

/// Price coverage: position count only, with no fabricated monetary
/// denominator for positions for which no price was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionCoverage {
    pub evaluated_positions: u32,
    pub total_positions: u32,
    pub selected: Vec<EvaluatedPosition>,
    pub uncovered: Vec<UncoveredPosition>,
    pub legacy_derived: Vec<LegacyDerivedPosition>,
}

/// Executable fractions of the value of **valued positions**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutabilityShares {
    pub evaluated_positions_value: Dec,
    pub executable: Dec,
    pub indicative_previous_close: Dec,
    pub unknown: Dec,
}

/// A monetary amount for which lack of knowledge cannot be replaced with zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountQualification {
    Known(Dec),
    Unknown,
}

/// Valuation before exit costs and tax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidationEstimate {
    pub value_before_exit_costs_and_tax: Computed<Dec>,
    pub executability: ExecutabilityShares,
    pub exit_costs: AmountQualification,
    pub tax: AmountQualification,
    /// Accrued interest realizable today across all bond positions.
    ///
    /// `NotComputable` makes this specific estimate incomplete,
    /// but does not propagate uncertainty into `terminal_value` (§4.2).
    pub accrued_interest_payable_on_termination: Computed<Dec>,
}

/// Portfolio value coverage by confidence level (§10.5).
///
/// Fractions are calculated using the **absolute value** of account value: an account with negative
/// the remainder is also either covered or not covered by reconciliation, and discarding it would
/// mean calculating the share of an incomplete portfolio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavCoverage {
    pub accepted_independent: Dec,
    pub accepted_internal: Dec,
    pub provisional: Dec,
    /// Share of value for which reconciliation fails.
    ///
    /// §10.5 shows three shares in the example. The fourth was added
    /// intentionally: without it, an unreconciled account would fall into `provisional`
    /// and looked like «merely not yet confirmed» — that is, the issue
    /// would be hidden in the very figure that exists to
    /// show it.
    pub discrepant: Dec,
}

/// Data quality block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataQuality {
    pub status: DataQualityStatus,
    /// Reconciliation of cash and position measurements by account.
    pub nav_coverage: NavCoverage,
    /// Price coverage and reasons for uncovered positions.
    pub position_coverage: PositionCoverage,
    /// Feasibility shares by value of priced positions.
    pub executability: ExecutabilityShares,
    pub material_issues: Vec<MaterialIssue>,
}
/// The exact inputs used in the calculation. Without this, the figure cannot be reproduced
/// (§3.2, §6.1).
///
/// `Eq` is not derived: the solver policy includes a tolerance in binary
/// floating-point arithmetic, and equality for such values is not reflexive.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedRules {
    pub contour: ContourId,
    pub contour_version: ContourVersion,
    pub lot_rule: Option<RuleId>,
    pub fx_source: FxSource,
    pub day_count: DayCount,
    /// Version of the rule for constructing a bond's future cash flow.
    pub cashflow_projection: CashflowProjectionVersion,
    /// Version of the expense accounting policy.
    pub expense_policy: zero_reinvestment::ExpensePolicyVersion,
    pub solver_policy: SolverPolicy,
    /// Threshold used to classify the negative balance (§11).
    /// A threshold-dependent figure must carry its threshold alongside it.
    pub perimeter_policy: PerimeterPolicy,
    /// Version of the unified quote-to-cash conversion rule.
    pub quotation_rule: QuotationRuleVersion,
    /// Version of the accrued interest calculation rule.
    pub accrued_interest_rule: AccruedInterestRuleVersion,
    /// Version of the rule for reconciling scheduled payments with the journal.
    pub posting_match: PostingMatchVersion,
}

/// Knowledge coordinate recorded by the report (§4).
///
/// This is a tuple of versions and a knowledge timestamp, not a list of identifiers
/// for observations: the append-only log and deterministic selection reconstruct
/// the input set at the same coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeCoordinate {
    pub knowledge_as_of: OffsetDateTime,
    pub source_priority_version: u32,
    pub valuation_policy_version: u32,
}

impl Default for KnowledgeCoordinate {
    fn default() -> Self {
        Self {
            knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
            source_priority_version: 1,
            valuation_policy_version: 1,
        }
    }
}

/// Report request.
#[derive(Debug, Clone, Copy)]
pub struct ReturnsRequest<'a> {
    pub contour: &'a ContourDefinition,
    pub as_of: Date,
    pub report_currency: CurrencyCode,
    pub fx: &'a FxTable,
    pub solver_policy: SolverPolicy,
    /// Coordinate of the observation set used in the calculation.
    pub coordinate: KnowledgeCoordinate,
    /// Reconciliation registry: without it, the confirmed share is unknown (§10.5).
    pub ledger: &'a ReconciliationLedger,
    /// Perimeter assessment: without it, the report does not know where to opt out
    /// calculate (§11).
    pub perimeter: &'a PerimeterAssessment,
    /// Candidates from the market store.
    ///
    /// Provided as separate input rather than journal events, — via the same path,
    /// for which official exchange rates are already available (E3.3, design 2.1). An empty
    /// slice means «no market observations», not an error: the decision about
    /// coverage is decided by policy.
    pub market_prices: &'a [PriceCandidate],
    /// Payment schedule at the knowledge coordinate, by instrument.
    pub bond_schedules: &'a BTreeMap<InstrumentId, BondSchedule>,
    /// Observed accrued interest per bond, tied to the venue and trade date.
    pub accrued_observations: &'a BTreeMap<(InstrumentId, Venue, Date), PerUnitAmount>,
}

/// Answers to the three questions in phase 1.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnsReport {
    pub as_of: Date,
    pub history_starts: Option<Date>,
    pub report_currency: CurrencyCode,
    /// Coordinate at which the report input set was selected.
    pub coordinate: KnowledgeCoordinate,
    /// SHA-256 of the canonical input selection.
    pub inputs_hash: String,
    /// Added to the perimeter over the entire history.
    pub contributed: Computed<Dec>,
    /// Removed from the perimeter over the entire history.
    pub withdrawn: Computed<Dec>,
    /// Portfolio value as of the report date: cash plus positions valued at their prices.
    pub terminal_value: Computed<Dec>,
    /// Bond position metrics for each available scenario.
    pub bond_metrics: Vec<BondPositionMetrics>,
    /// Value before exit costs and before tax.
    pub liquidation_value_before_exit_costs_and_tax: LiquidationEstimate,
    /// Internal rate of return **before tax**.
    pub xirr: Computed<RateOutcome>,
    pub applied_rules: AppliedRules,
    /// Bond position attributes (§4 of the E3.4.4 spec).
    pub bond_attributes: Vec<BondPositionAttributes>,
    pub data_quality: DataQuality,
}

impl ReturnsReport {
    /// Result label. Exists so that no API consumer
    /// calls this figure «return» without qualification (§16.3).
    pub const XIRR_LABEL: &'static str = "xirr_pre_tax";
    /// Valuation excluding hypothetical exit costs and tax (§6.2).
    pub const LIQUIDATION_LABEL: &'static str = "liquidation_value_before_exit_costs_and_tax";
}

#[derive(Serialize)]
struct SelectedPosition {
    account: AccountId,
    custody: Option<crate::ids::CustodyId>,
    instrument: InstrumentId,
    quantity: crate::money::Quantity,
    valuation: PositionValuation,
}

#[derive(Serialize)]
enum PositionValuation {
    /// Boxed observation: with the quote basis, the variant outweighed
    /// the others by a factor of four, and the enum grew to the size of the
    /// large on every position (clippy::large_enum_variant). The same
    /// the same technique has already been applied to `PositionAssessmentKind::Selected`.
    Selected(Box<SelectedObservation>),
    LegacyDerived {
        quality: PriceQuality,
        price: Option<crate::valuation::InstrumentPrice>,
    },
    Uncovered {
        reason: &'static str,
    },
}

#[derive(Serialize)]
struct SelectedObservation {
    instrument: InstrumentId,
    price: Dec,
    currency: CurrencyCode,
    #[serde(default)]
    quotation_basis: &'static str,
    #[serde(default)]
    basis_evidence: String,
    trade_date: Date,
    observed_at: Option<OffsetDateTime>,
    executability: &'static str,
    selection: SelectedSelection,
    freshness: SelectedFreshness,
    provenance: SelectedProvenance,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SelectedSelection {
    AsObserved,
    CarriedForward { observed_on: Date, days: u16 },
    LegacyDerived { quality: PriceQuality },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SelectedFreshness {
    Fresh,
    Stale { days: u16 },
}

#[derive(Serialize)]
struct SelectedProvenance {
    price_kind: Option<String>,
    origin: SelectedOrigin,
    venue: Option<String>,
    observed_at: Option<OffsetDateTime>,
    valuation_policy_version: u32,
    source_priority_version: u32,
    carry_forward_limit: u16,
    price_max_age: u16,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SelectedOrigin {
    Market { venue: String, price_kind: String },
    ReportParsed { source: SourceId },
    OwnerAsserted,
}

fn selected_observation(price: &SelectedPrice) -> SelectedObservation {
    let selection = match price.selection {
        crate::valuation::PriceSelection::AsObserved => SelectedSelection::AsObserved,
        crate::valuation::PriceSelection::CarriedForward { observed_on, days } => {
            SelectedSelection::CarriedForward { observed_on, days }
        }
        crate::valuation::PriceSelection::LegacyDerived { quality } => {
            SelectedSelection::LegacyDerived { quality }
        }
    };
    let freshness = match price.freshness {
        crate::valuation::PriceFreshness::Fresh => SelectedFreshness::Fresh,
        crate::valuation::PriceFreshness::Stale { days } => SelectedFreshness::Stale { days },
    };
    let origin = match &price.provenance.origin {
        crate::valuation::PriceOrigin::Market { venue, kind } => SelectedOrigin::Market {
            venue: venue.board.clone(),
            price_kind: match kind {
                crate::valuation::PriceKind::Close => "close",
                crate::valuation::PriceKind::LegalClose => "legal_close",
                crate::valuation::PriceKind::WeightedAverage => "weighted_average",
                crate::valuation::PriceKind::MarketPrice2 => "market_price_2",
                crate::valuation::PriceKind::MarketPrice3 => "market_price_3",
                crate::valuation::PriceKind::AdmittedQuote => "admitted_quote",
            }
            .to_owned(),
        },
        crate::valuation::PriceOrigin::ReportParsed { source } => {
            SelectedOrigin::ReportParsed { source: *source }
        }
        crate::valuation::PriceOrigin::OwnerAsserted => SelectedOrigin::OwnerAsserted,
    };
    SelectedObservation {
        quotation_basis: price.candidate.basis.code(),
        basis_evidence: price.candidate.basis_evidence.clone(),
        instrument: price.candidate.instrument,
        price: price.candidate.price,
        currency: price.candidate.currency,
        trade_date: price.candidate.trade_date,
        observed_at: price.candidate.observed_at,
        executability: match price.candidate.executability {
            SourceExecutability::Executable => "executable",
            SourceExecutability::IndicativePreviousClose => "indicative_previous_close",
            SourceExecutability::Unknown => "unknown",
        },
        selection,
        freshness,
        provenance: SelectedProvenance {
            price_kind: price.provenance.price_kind.clone(),
            origin,
            venue: price.provenance.venue.clone(),
            observed_at: price.provenance.observed_at,
            valuation_policy_version: price.provenance.valuation_policy_version,
            source_priority_version: price.provenance.source_priority_version,
            carry_forward_limit: price.provenance.carry_forward_limit,
            price_max_age: price.provenance.price_max_age,
        },
    }
}

fn uncovered_reason_code(reason: &UncoveredReason) -> &'static str {
    match reason {
        UncoveredReason::NoObservation => "no_observation",
        UncoveredReason::TooOld => "too_old",
        UncoveredReason::AmbiguousVenue => "ambiguous_venue",
        UncoveredReason::AmbiguousCandidate => "ambiguous_candidate",
        UncoveredReason::NotComputable { reason } => reason.code(),
    }
}

fn policy_uncovered_reason(reason: PolicyUncoveredReason) -> UncoveredReason {
    match reason {
        PolicyUncoveredReason::NoObservation => UncoveredReason::NoObservation,
        PolicyUncoveredReason::TooOld => UncoveredReason::TooOld,
        PolicyUncoveredReason::AmbiguousVenue => UncoveredReason::AmbiguousVenue,
        PolicyUncoveredReason::AmbiguousCandidate => UncoveredReason::AmbiguousCandidate,
    }
}

#[derive(Serialize)]
struct SelectedFx<'a> {
    source: &'a FxSource,
    rates: Vec<(CurrencyCode, CurrencyCode, Date, Option<Dec>)>,
}

#[derive(Serialize)]
struct SelectedCoordinate {
    knowledge_as_of: OffsetDateTime,
    source_priority_version: u32,
    valuation_policy_version: u32,
}

#[derive(Serialize)]
struct SelectedInputs<'a> {
    coordinate: SelectedCoordinate,
    as_of: Date,
    contour: &'a ContourDefinition,
    report_currency: CurrencyCode,
    flows: Vec<crate::projection::flows::ExternalFlow>,
    cash: Vec<(AccountId, crate::money::Money)>,
    positions: Vec<SelectedPosition>,
    fx: SelectedFx<'a>,
    bond_schedules: &'a BTreeMap<InstrumentId, BondSchedule>,
    accrued_observations: &'a BTreeMap<(InstrumentId, Venue, Date), PerUnitAmount>,
    accrued_interest_rule: AccruedInterestRuleVersion,
    offer_book: &'a OfferBook,
    cashflow_projection: CashflowProjectionVersion,
    expense_policy: u32,
    /// Reconciliation rule version: changing the report input must change
    /// fingerprint, otherwise two incomparable answers would look
    /// by reproducing one.
    posting_match: PostingMatchVersion,
}

#[cfg(test)]
fn inputs_hash_with_assessments(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
    assessments: Vec<PositionAssessment>,
) -> String {
    let offer_book = OfferBook::default();
    inputs_hash_with_bond_inputs(state, request, assessments, &offer_book)
}

fn inputs_hash_with_bond_inputs(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
    assessments: Vec<PositionAssessment>,
    offer_book: &OfferBook,
) -> String {
    let mut flows: Vec<_> = state
        .flows()
        .external()
        .iter()
        .filter(|flow| {
            flow.date <= request.as_of
                && flow.contour == request.contour.id()
                && flow.version == request.contour.version()
        })
        .copied()
        .collect();
    flows.sort_by_key(|flow| (flow.date, flow.event));

    let mut cash: Vec<_> = state
        .balances()
        .iter_cash()
        .filter(|(account, _)| request.contour.contains(*account))
        .collect();
    cash.sort_by_key(|(account, money)| (*account, money.currency()));

    let positions: Vec<_> = assessments
        .into_iter()
        .map(|assessment| {
            let PositionAssessment {
                account,
                custody,
                instrument,
                quantity,
                raw_price,
                kind,
                remaining_face: _,
            } = assessment;
            let valuation = match kind {
                PositionAssessmentKind::Selected(selected) => {
                    PositionValuation::Selected(Box::new(selected_observation(&selected)))
                }
                PositionAssessmentKind::LegacyDerived(quality) => {
                    PositionValuation::LegacyDerived {
                        quality,
                        price: raw_price,
                    }
                }
                PositionAssessmentKind::Uncovered(reason) => PositionValuation::Uncovered {
                    reason: uncovered_reason_code(&reason),
                },
            };
            SelectedPosition {
                account,
                custody,
                instrument,
                quantity,
                valuation,
            }
        })
        .collect();

    let mut fx_keys = std::collections::BTreeSet::new();
    for flow in &flows {
        fx_keys.insert((flow.amount.currency(), flow.date));
    }
    for (_, money) in &cash {
        fx_keys.insert((money.currency(), request.as_of));
    }
    for position in &positions {
        let currency = match &position.valuation {
            PositionValuation::Selected(observation) => Some(observation.currency),
            PositionValuation::LegacyDerived { price, .. } => price.map(|price| price.currency),
            PositionValuation::Uncovered { .. } => None,
        };
        if let Some(currency) = currency {
            fx_keys.insert((currency, request.as_of));
        }
    }
    let rates = fx_keys
        .into_iter()
        .map(|(from, date)| {
            (
                from,
                request.report_currency,
                date,
                request.fx.rate(from, request.report_currency, date),
            )
        })
        .collect();

    let selected = SelectedInputs {
        coordinate: SelectedCoordinate {
            knowledge_as_of: request.coordinate.knowledge_as_of.to_offset(UtcOffset::UTC),
            source_priority_version: request.coordinate.source_priority_version,
            valuation_policy_version: request.coordinate.valuation_policy_version,
        },
        as_of: request.as_of,
        contour: request.contour,
        report_currency: request.report_currency,
        flows,
        cash,
        positions,
        fx: SelectedFx {
            source: request.fx.source(),
            rates,
        },
        bond_schedules: request.bond_schedules,
        accrued_observations: request.accrued_observations,
        accrued_interest_rule: accrued_interest_rule().0,
        offer_book,
        cashflow_projection: cashflow_projection_rule().0,
        expense_policy: expense_policy_rule().0,
        posting_match: posting_match_rule().0,
    };
    hash_selected(&selected)
}

/// Fingerprint of the selected inputs.
///
/// Extracted from [`inputs_hash_with_bond_inputs`], so that the test can change
/// change one input field, and verify that the fingerprint responds to it:
/// building `SelectedInputs` manually is cheaper than recreating an entire
/// state for a single rule version.
fn hash_selected(selected: &SelectedInputs<'_>) -> String {
    let mut encoded = Vec::new();
    if ciborium::into_writer(selected, &mut encoded).is_err() {
        encoded.extend_from_slice(b"serialization_error");
    }

    let mut hasher = Sha256::new();
    hasher.update(b"iaam/returns-inputs/v2");
    hasher.update(encoded);
    let digest = hasher.finalize();
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        result.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        result.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    result
}
/// Quote conversion rule used by the report.
///
/// One helper is needed so that position valuation and XIRR do not receive
/// divergent conversion implementations.
pub(crate) const fn quotation_rule() -> (QuotationRuleVersion, QuotationV1) {
    (QuotationRuleVersion(1), QuotationV1)
}

/// Scenario cash flow construction rule version.
pub(crate) const fn cashflow_projection_rule() -> (
    CashflowProjectionVersion,
    crate::rules::CashflowProjectionV2,
) {
    (
        CashflowProjectionVersion(2),
        crate::rules::CashflowProjectionV2,
    )
}

/// Rule for reconciling scheduled payments against dated facts.
pub(crate) const fn posting_match_rule() -> (PostingMatchVersion, PostingMatchV2) {
    (PostingMatchVersion(2), PostingMatchV2::new())
}

/// Expense policy version used before the tax scope is introduced.
pub(crate) const fn expense_policy_rule() -> zero_reinvestment::ExpensePolicyVersion {
    zero_reinvestment::ExpensePolicyVersion(1)
}

/// Accrued interest calculation rule used by the report.
pub(crate) const fn accrued_interest_rule() -> (AccruedInterestRuleVersion, AccruedInterestV1) {
    (AccruedInterestRuleVersion(1), AccruedInterestV1)
}

fn accrued_error(error: AccruedInterestError, instrument: InstrumentId) -> NotComputable {
    match error {
        AccruedInterestError::OutsideCoverage => {
            NotComputable::OutsideScheduleCoverage { instrument }
        }
        AccruedInterestError::OverlappingCoverage => {
            NotComputable::OverlappingScheduleCoverage { instrument }
        }
        AccruedInterestError::CouponUndetermined => {
            NotComputable::CouponUndetermined { instrument }
        }
        AccruedInterestError::Numeric(_) => NotComputable::Numeric { code: "numeric" },
    }
}

fn accrued_per_unit(
    request: &ReturnsRequest<'_>,
    rule: &dyn AccruedInterestRule,
    instrument: InstrumentId,
) -> Result<PerUnitAmount, NotComputable> {
    let schedule = request
        .bond_schedules
        .get(&instrument)
        .ok_or(NotComputable::ScheduleMissing { instrument })?;
    rule.accrued_per_unit(&schedule.periods, request.as_of)
        .map_err(|error| accrued_error(error, instrument))
}

fn position_amount(
    per_unit: PerUnitAmount,
    quantity: crate::money::Quantity,
    request: &ReturnsRequest<'_>,
) -> Computed<Dec> {
    let local = match per_unit.checked_mul_quantity(quantity) {
        Ok(value) => value,
        Err(_) => {
            return Computed::NotComputable {
                reason: NotComputable::Numeric { code: "numeric" },
            };
        }
    };
    let Some(rate) = request
        .fx
        .rate(per_unit.currency(), request.report_currency, request.as_of)
    else {
        return Computed::NotComputable {
            reason: NotComputable::MissingFxRate {
                from: per_unit.currency(),
                to: request.report_currency,
                date: request.as_of,
            },
        };
    };
    match local.checked_mul(rate) {
        Ok(value) => Computed::Value(value),
        Err(_) => Computed::NotComputable {
            reason: NotComputable::Numeric { code: "numeric" },
        },
    }
}

fn selected_executability(assessment: &PositionAssessment) -> Option<SourceExecutability> {
    match &assessment.kind {
        PositionAssessmentKind::Selected(selected) => Some(selected.candidate.executability),
        PositionAssessmentKind::LegacyDerived(_) | PositionAssessmentKind::Uncovered(_) => None,
    }
}
fn selected_accrued_observation<'a>(
    assessment: &PositionAssessment,
    request: &'a ReturnsRequest<'_>,
) -> Option<&'a PerUnitAmount> {
    let PositionAssessmentKind::Selected(selected) = &assessment.kind else {
        return None;
    };
    if selected.candidate.trade_date != request.as_of {
        return None;
    }
    let crate::valuation::PriceOrigin::Market { venue, .. } = &selected.candidate.origin else {
        return None;
    };
    request
        .accrued_observations
        .get(&(assessment.instrument, venue.clone(), request.as_of))
}

fn bond_position_attributes(
    positions: &[PositionValue],
    request: &ReturnsRequest<'_>,
    rule: &dyn AccruedInterestRule,
) -> Vec<BondPositionAttributes> {
    positions
        .iter()
        .filter(|position| {
            matches!(
                &position.assessment.kind,
                PositionAssessmentKind::Selected(selected)
                    if selected.candidate.basis == QuotationBasis::PercentOfRemainingFace
            )
        })
        .map(|position| {
            let assessment = &position.assessment;
            let accrued = match accrued_per_unit(request, rule, assessment.instrument) {
                Ok(per_unit) => position_amount(per_unit, assessment.quantity, request),
                Err(reason) => Computed::NotComputable { reason },
            };
            let payable_observation = selected_accrued_observation(assessment, request)
                .map(|per_unit| position_amount(*per_unit, assessment.quantity, request))
                .unwrap_or_else(|| Computed::NotComputable {
                    reason: NotComputable::AccruedObservationMissing {
                        instrument: assessment.instrument,
                    },
                });
            let payable = payable_on_termination(
                &payable_observation,
                selected_executability(assessment).unwrap_or(SourceExecutability::Unknown),
            );
            let (next_posting_date, next_principal_return_finality) = request
                .bond_schedules
                .get(&assessment.instrument)
                .map(|schedule| {
                    let next = next_posting_date(
                        &schedule.periods,
                        &schedule.principal_returns,
                        &[],
                        request.as_of,
                    );
                    let finality = next.and_then(|date| {
                        finality_of(&schedule.principal_returns)
                            .ok()?
                            .into_iter()
                            .find(|(item, _)| item.repayment_date == date)
                            .map(|(_, finality)| finality)
                    });
                    (next, finality)
                })
                .unwrap_or((None, None));
            BondPositionAttributes {
                account: assessment.account,
                custody: assessment.custody,
                instrument: assessment.instrument,
                accrued_interest: accrued,
                accrued_interest_payable_on_termination: payable,
                next_posting_date,
                next_principal_return_finality,
            }
        })
        .collect()
}

/// Whether the discrepancy between calculated accrued interest and the observation is material.
///
/// Tolerance is one minor currency unit: a discrepancy of one kopeck
/// is due to rounding, not a rule error.
fn accrued_mismatch_is_material(computed: Dec, observed: Dec, currency: CurrencyCode) -> bool {
    let Ok(difference) = computed.checked_sub(observed) else {
        return true;
    };
    let tolerance = Dec::new(Decimal::new(1, currency.minor_units()));
    difference.inner().abs() > tolerance.inner()
}

fn accrued_mismatch_issues(
    positions: &[PositionValue],
    request: &ReturnsRequest,
    rule: &dyn AccruedInterestRule,
) -> Vec<MaterialIssue> {
    let mut quantities = BTreeMap::new();
    for position in positions {
        let assessment = &position.assessment;
        let total = quantities
            .entry(assessment.instrument)
            .or_insert_with(Dec::zero);
        if let Ok(sum) = total.checked_add(assessment.quantity.0) {
            *total = sum;
        }
    }
    quantities
        .into_iter()
        .filter_map(|(instrument, quantity)| {
            let computed = accrued_per_unit(request, rule, instrument).ok()?;
            let observed = positions
                .iter()
                .find(|position| position.assessment.instrument == instrument)
                .and_then(|position| selected_accrued_observation(&position.assessment, request))?;
            let material = computed.currency() != observed.currency()
                || accrued_mismatch_is_material(
                    computed.value(),
                    observed.value(),
                    observed.currency(),
                );
            if !material {
                return None;
            }
            let quantity = crate::money::Quantity(quantity);
            Some(MaterialIssue::AccruedInterestMismatch {
                instrument,
                computed: computed.checked_mul_quantity(quantity).ok()?,
                computed_currency: computed.currency(),
                observed: observed.checked_mul_quantity(quantity).ok()?,
                observed_currency: observed.currency(),
                quantity,
                date: request.as_of,
            })
        })
        .collect()
}

/// Amount realizable on exit (§4.2).
///
/// Accrued interest becomes realizable only at a price the source has declared
/// executable. An indicative close and the absence of a selected price — failures,
/// rather than a zero result or a guarantee of liquidity.
fn payable_on_termination(
    accrued: &Computed<Dec>,
    executability: SourceExecutability,
) -> Computed<Dec> {
    match executability {
        SourceExecutability::Executable => accrued.clone(),
        SourceExecutability::IndicativePreviousClose | SourceExecutability::Unknown => {
            Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable,
            }
        }
    }
}

fn aggregate_payable_on_termination(attributes: &[BondPositionAttributes]) -> Computed<Dec> {
    let mut total = Dec::zero();
    for attribute in attributes {
        let Computed::Value(value) = &attribute.accrued_interest_payable_on_termination else {
            return attribute.accrued_interest_payable_on_termination.clone();
        };
        total = match total.checked_add(*value) {
            Ok(value) => value,
            Err(_) => {
                return Computed::NotComputable {
                    reason: NotComputable::Numeric { code: "numeric" },
                };
            }
        };
    }
    Computed::Value(total)
}

/// Report calculation without a separate order book.
#[must_use]
pub fn returns_report(state: &LedgerState, request: &ReturnsRequest) -> ReturnsReport {
    let offer_book = OfferBook::default();
    returns_report_with_bond_inputs(state, request, &offer_book)
}

/// Report calculation with inputs specific to bond
/// scenarios. The order book is built from the log by the application shell.
///
/// The core does not fetch data: prices and exchange rates are supplied ready to use, and the boundaries
/// of the perimeter are explicit. Everything, if missing, becomes a failure
/// with a stated reason, not a substituted value.
#[must_use]
pub fn returns_report_with_bond_inputs(
    state: &LedgerState,
    request: &ReturnsRequest,
    offer_book: &OfferBook,
) -> ReturnsReport {
    let (quotation_rule_version, _) = quotation_rule();
    let (accrued_interest_rule_version, accrued_interest_rule) = accrued_interest_rule();
    let (cashflow_projection_version, _) = cashflow_projection_rule();
    let (posting_match_version, _) = posting_match_rule();
    let expense_policy_version = expense_policy_rule();
    let positions = position_values(state, request);
    let series = xirr::flow_series(state, request);
    let terminal = xirr::terminal_value_from_position_values(state, request, &positions);
    let (contributed, withdrawn) = match &series {
        Ok(series) => (
            Computed::Value(series.contributed),
            Computed::Value(series.withdrawn),
        ),
        Err(reason) => (
            Computed::NotComputable {
                reason: reason.clone(),
            },
            Computed::NotComputable {
                reason: reason.clone(),
            },
        ),
    };
    let terminal_value = match &terminal {
        Ok(value) => Computed::Value(*value),
        Err(reason) => Computed::NotComputable {
            reason: reason.clone(),
        },
    };
    let rate = xirr::rate(&series, &terminal, request);
    let bond_attributes = bond_position_attributes(&positions, request, &accrued_interest_rule);
    let mut data_quality = data_quality(state, request, &positions);
    let bond_metrics = bond_position_metrics(state, request, &positions, offer_book);
    for issue in unresolved_offer_issues(request, offer_book) {
        data_quality.material_issues.push(issue);
    }
    data_quality
        .material_issues
        .extend(historical_reconciliation_issues(state, request));
    if data_quality
        .material_issues
        .iter()
        .any(MaterialIssue::is_defect)
    {
        data_quality.status = DataQualityStatus::Incomplete;
    }
    let liquidation_value_before_exit_costs_and_tax = LiquidationEstimate {
        value_before_exit_costs_and_tax: terminal_value.clone(),
        executability: data_quality.executability,
        exit_costs: AmountQualification::Unknown,
        tax: AmountQualification::Unknown,
        accrued_interest_payable_on_termination: aggregate_payable_on_termination(&bond_attributes),
    };

    ReturnsReport {
        as_of: request.as_of,
        history_starts: state.coverage().first_event(),
        report_currency: request.report_currency,
        coordinate: request.coordinate,
        inputs_hash: inputs_hash_with_bond_inputs(
            state,
            request,
            position_assessments(state, request),
            offer_book,
        ),
        contributed,
        withdrawn,
        terminal_value,
        bond_metrics,
        liquidation_value_before_exit_costs_and_tax,
        xirr: rate,
        applied_rules: AppliedRules {
            contour: request.contour.id(),
            contour_version: request.contour.version(),
            lot_rule: state.book().applied_rule().cloned(),
            fx_source: request.fx.source().clone(),
            day_count: DayCount::Act365,
            solver_policy: request.solver_policy,
            perimeter_policy: request.perimeter.policy(),
            quotation_rule: quotation_rule_version,
            accrued_interest_rule: accrued_interest_rule_version,
            cashflow_projection: cashflow_projection_version,
            expense_policy: expense_policy_version,
            posting_match: posting_match_version,
        },
        bond_attributes,
        data_quality,
    }
}

fn cashflow_reason(error: crate::rules::CashflowError, instrument: InstrumentId) -> NotComputable {
    match error {
        crate::rules::CashflowError::CouponUndetermined { .. } => {
            NotComputable::CouponUndetermined { instrument }
        }
        crate::rules::CashflowError::PrincipalUnknown => NotComputable::PrincipalUnknown,
        crate::rules::CashflowError::ScheduleIncomplete { .. }
        | crate::rules::CashflowError::ScheduleCompletenessUnknown
        | crate::rules::CashflowError::IssueTermsUnknown => {
            NotComputable::ScheduleMissing { instrument }
        }
        crate::rules::CashflowError::IssuerDefaultDeclared
        | crate::rules::CashflowError::IssuerTechnicalDefault
        | crate::rules::CashflowError::SharesDoNotSumToWhole { .. }
        | crate::rules::CashflowError::CurrencyFormulaUnknown { .. }
        | crate::rules::CashflowError::OfferWindowNotExercisable { .. }
        | crate::rules::CashflowError::Numeric(_) => NotComputable::Numeric { code: "cashflow" },
    }
}

fn bond_c0(
    assessment: &PositionAssessment,
    request: &ReturnsRequest<'_>,
    accrued_rule: &dyn AccruedInterestRule,
) -> Computed<CalcMoney> {
    let accrued =
        match accrued_per_unit(request, accrued_rule, assessment.instrument).and_then(|value| {
            value
                .checked_mul_quantity(assessment.quantity)
                .map(|total| CalcMoney::new(total, value.currency()))
                .map_err(|_| NotComputable::Numeric {
                    code: "accrued_total",
                })
        }) {
            Ok(value) => value,
            Err(reason) => return Computed::NotComputable { reason },
        };
    let selected = match &assessment.kind {
        PositionAssessmentKind::Selected(selected) => selected,
        PositionAssessmentKind::LegacyDerived(_)
        | PositionAssessmentKind::Uncovered(UncoveredReason::NoObservation)
        | PositionAssessmentKind::Uncovered(UncoveredReason::TooOld)
        | PositionAssessmentKind::Uncovered(UncoveredReason::AmbiguousVenue)
        | PositionAssessmentKind::Uncovered(UncoveredReason::AmbiguousCandidate) => {
            return Computed::NotComputable {
                reason: NotComputable::MissingPrice {
                    instrument: assessment.instrument,
                },
            };
        }
        PositionAssessmentKind::Uncovered(UncoveredReason::NotComputable { reason }) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    let remaining_face = match &assessment.remaining_face {
        Ok(value) => *value,
        Err(reason) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    zero_reinvestment::prospective_c0(
        assessment.quantity,
        selected.candidate.basis,
        selected.candidate.price,
        selected.candidate.currency,
        remaining_face,
        accrued,
    )
}

fn unavailable_prospective(
    as_of: Date,
    terminal_date: Date,
    c0: Computed<CalcMoney>,
    choice: OfferChoice,
    reason: NotComputable,
) -> zero_reinvestment::ProspectiveMetric {
    let irr_label = match choice {
        OfferChoice::HoldToMaturity => zero_reinvestment::IrrLabel::YieldToMaturity,
        OfferChoice::ExerciseAtOffer { .. } => zero_reinvestment::IrrLabel::YieldToOffer,
    };
    zero_reinvestment::ProspectiveMetric {
        as_of,
        terminal_date,
        c0,
        metrics: Computed::NotComputable {
            reason: reason.clone(),
        },
        irr: Computed::NotComputable { reason },
        irr_label,
    }
}

/// Inputs for one bond position, shared by all its scenarios.
///
/// A struct rather than seven arguments: there is one set per position, and only
/// the scenario changes between calls, and swapping two references
/// of the same type in a list of seven is all too easy.
struct BondScenarioInputs<'a> {
    assessment: &'a PositionAssessment,
    request: &'a ReturnsRequest<'a>,
    schedule: &'a BondSchedule,
    lots: Option<&'a crate::projection::lots::InstrumentLots>,
    cashflow: &'a dyn crate::rules::CashflowProjection,
    accrued_rule: &'a dyn AccruedInterestRule,
}

/// Flow for a single scenario. Extracted from [`bond_scenario`], because
/// historical reconciliation constructs it separately and must use the same
/// rule: otherwise `past` would diverge between the reconciliation and the scenario.
fn scenario_plan(
    inputs: &BondScenarioInputs<'_>,
    choice: &OfferChoice,
) -> Result<CashflowPlan, NotComputable> {
    let BondScenarioInputs {
        assessment,
        request,
        schedule,
        lots: _,
        cashflow,
        accrued_rule: _,
    } = *inputs;
    cashflow
        .future_postings(&CashflowInput {
            schedule,
            quantity: assessment.quantity,
            choice,
            as_of: request.as_of,
            report_currency: request.report_currency,
        })
        .map_err(|error| cashflow_reason(error, assessment.instrument))
}

fn bond_scenario(
    inputs: &BondScenarioInputs<'_>,
    choice: OfferChoice,
) -> zero_reinvestment::BondScenarioResult {
    let BondScenarioInputs {
        assessment,
        request,
        schedule,
        lots,
        cashflow: _,
        accrued_rule,
    } = *inputs;
    let c0 = bond_c0(assessment, request, accrued_rule);
    let plan = scenario_plan(inputs, &choice);
    match plan {
        Ok(plan) => {
            let prospective =
                zero_reinvestment::prospective_metric(request.as_of, &plan, c0, &choice);
            let lifetime = lots.map_or_else(
                || Computed::NotComputable {
                    reason: NotComputable::CohortGap {
                        gap: crate::projection::lots::CohortGap::AcquisitionDateUnknown,
                    },
                },
                |lots| zero_reinvestment::lifetime_metrics_from_lots(lots, &plan),
            );
            zero_reinvestment::BondScenarioResult {
                choice,
                prospective,
                lifetime,
            }
        }
        Err(reason) => {
            let terminal_date = match choice {
                OfferChoice::HoldToMaturity => schedule
                    .principal_returns
                    .iter()
                    .map(|item| item.repayment_date)
                    .max()
                    .unwrap_or(request.as_of),
                OfferChoice::ExerciseAtOffer { window } => schedule
                    .offer_windows
                    .iter()
                    .find(|terms| terms.window == window)
                    .map_or(request.as_of, |terms| terms.execution_date),
            };
            zero_reinvestment::BondScenarioResult {
                choice: choice.clone(),
                prospective: unavailable_prospective(
                    request.as_of,
                    terminal_date,
                    c0,
                    choice,
                    reason.clone(),
                ),
                lifetime: Computed::NotComputable { reason },
            }
        }
    }
}

/// Coalesces repeated unprovability reports after the entire reconciliation is complete.
///
/// The issue source can be fixed with one action, so repeating the same reason
/// should not consume a report entry for every individual payment. At the same time
/// all other issues pass through unfiltered: the omission finding remains
/// specific, and distinct causes are not conflated.
fn collapse_scheduled_posting_unverifiable(issues: Vec<MaterialIssue>) -> Vec<MaterialIssue> {
    type Key = (AccountId, InstrumentId, PostingKind, UnverifiableReason);

    let mut groups: BTreeMap<Key, (u32, Date, Date)> = BTreeMap::new();
    for issue in &issues {
        let MaterialIssue::ScheduledPostingUnverifiable {
            account,
            instrument,
            date,
            kind,
            reason,
        } = issue
        else {
            continue;
        };
        // The source profile is intentionally excluded from the key: the lot book does not
        // tracks event provenance, while the quantity and period already
        // provide the owner with the required corrective action.
        let key = (*account, *instrument, *kind, *reason);
        groups
            .entry(key)
            .and_modify(|(count, first_date, last_date)| {
                *count = count.checked_add(1).expect("too many payments");
                *first_date = (*first_date).min(*date);
                *last_date = (*last_date).max(*date);
            })
            .or_insert((1, *date, *date));
    }

    let mut emitted = BTreeSet::new();
    let mut collapsed = Vec::with_capacity(issues.len());
    for issue in issues {
        let MaterialIssue::ScheduledPostingUnverifiable {
            account,
            instrument,
            date,
            kind,
            reason,
        } = issue
        else {
            collapsed.push(issue);
            continue;
        };
        let key = (account, instrument, kind, reason);
        if !emitted.insert(key) {
            continue;
        }
        let (count, first_date, last_date) =
            groups.remove(&key).expect("unprovability group was lost");
        if count == 1 {
            collapsed.push(MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date,
                kind,
                reason,
            });
        } else {
            collapsed.push(MaterialIssue::ScheduledPostingsUnverifiable {
                account,
                instrument,
                kind,
                reason,
                count,
                first_date,
                last_date,
            });
        }
    }
    collapsed
}

/// Historical reconciliation against the lot book, independently of current positions.
///
/// A fully sold security remains in the lot book, but disappears from
/// the positions, along with its storage location. Therefore the pass must iterate over
/// [`LotKey`]: only issues are collected here, while metrics still
/// is computed in a separate pass over the positions.
fn historical_reconciliation_issues(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
) -> Vec<MaterialIssue> {
    let (_, posting_match) = posting_match_rule();
    let mut issues = Vec::new();

    for (key, lots) in state.book().iter() {
        if !request.contour.contains(key.account) {
            continue;
        }
        let Some(schedule) = request.bond_schedules.get(&key.instrument) else {
            continue;
        };
        let postings = match historical_schedule_postings(schedule, request.as_of) {
            Ok(postings) => postings,
            Err(_) => {
                // The trust error applies to the entire pair: the date and kind of
                // the payment cannot be obtained, but the problem shape must be
                // populated, so we honestly use the report date here
                // and the neutral Coupon kind.
                issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                    account: key.account,
                    instrument: key.instrument,
                    date: request.as_of,
                    kind: PostingKind::Coupon,
                    reason: UnverifiableReason::ScheduleNotTrusted,
                });
                continue;
            }
        };

        let gap = state.income().gap(key);
        let history_starts = state.coverage().first_event_for(key.account);
        let mut judged = Vec::new();
        for posting in postings
            .into_iter()
            .filter(|posting| posting_match.is_due(posting, request.as_of))
        {
            if let Some(gap) = gap {
                let reason = match gap {
                    crate::projection::income::IncomeGap::IncomeKindUnknown => {
                        UnverifiableReason::IncomeKindUnknown
                    }
                    crate::projection::income::IncomeGap::PaymentDateUnknown => {
                        UnverifiableReason::PaymentDateUnknown
                    }
                };
                issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                    account: key.account,
                    instrument: key.instrument,
                    date: posting.date,
                    kind: posting.kind,
                    reason,
                });
                continue;
            }
            if history_starts.is_some_and(|start| posting.date < start) {
                issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                    account: key.account,
                    instrument: key.instrument,
                    date: posting.date,
                    kind: posting.kind,
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                });
                continue;
            }
            let ownership = match posting.entitlement {
                Some(entitlement) => lots.ownership_at(entitlement),
                None => Ownership::Unknown,
            };
            judged.push((posting, ownership));
        }

        // All payments that reach the third step are passed in a single call:
        // otherwise an unverifiable payment could cover an adjacent one and hide
        // the actual omission.
        for ((posting, _), verdict) in judged
            .iter()
            .zip(posting_match.judge_all(&judged, state.income().postings(key)))
        {
            match verdict {
                Verdict::NotReceived => {
                    issues.push(MaterialIssue::ScheduledPostingNotReceived {
                        account: key.account,
                        instrument: key.instrument,
                        date: posting.date,
                        kind: posting.kind,
                    });
                }
                Verdict::Unverifiable(reason) => {
                    issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                        account: key.account,
                        instrument: key.instrument,
                        date: posting.date,
                        kind: posting.kind,
                        reason,
                    });
                }
                Verdict::Silent => {}
            }
        }
    }

    collapse_scheduled_posting_unverifiable(issues)
}

fn bond_position_metrics(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
    positions: &[PositionValue],
    offer_book: &OfferBook,
) -> Vec<BondPositionMetrics> {
    let (_, cashflow) = cashflow_projection_rule();
    let (_, accrued_rule) = accrued_interest_rule();
    positions
        .iter()
        .filter_map(|position| {
            let assessment = &position.assessment;
            let schedule = request.bond_schedules.get(&assessment.instrument)?;
            if !request.contour.contains(assessment.account) || assessment.quantity.0.is_zero() {
                return None;
            }
            let key = LotKey {
                account: assessment.account,
                instrument: assessment.instrument,
            };
            let lots = state.book().entry(&key);
            let unresolved: std::collections::BTreeSet<_> =
                unresolved_submissions(offer_book, schedule)
                    .into_iter()
                    .filter(|submission| {
                        offer_book
                            .submission(*submission)
                            .is_some_and(|state| state.instrument == assessment.instrument)
                    })
                    .collect();
            let inputs = BondScenarioInputs {
                assessment,
                request,
                schedule,
                lots,
                cashflow: &cashflow,
                accrued_rule: &accrued_rule,
            };
            let scenarios = available_choices(schedule, request.as_of)
                .into_iter()
                .filter(|choice| match choice {
                    OfferChoice::HoldToMaturity => true,
                    OfferChoice::ExerciseAtOffer { .. } => unresolved.is_empty(),
                })
                .map(|choice| bond_scenario(&inputs, choice))
                .collect();
            Some(BondPositionMetrics {
                account: assessment.account,
                custody: assessment.custody,
                instrument: assessment.instrument,
                scenarios,
            })
        })
        .collect()
}

fn unresolved_offer_issues(
    request: &ReturnsRequest<'_>,
    offer_book: &OfferBook,
) -> Vec<MaterialIssue> {
    let submissions: std::collections::BTreeSet<_> = request
        .bond_schedules
        .values()
        .flat_map(|schedule| unresolved_submissions(offer_book, schedule))
        .collect();
    submissions
        .into_iter()
        .map(|submission| MaterialIssue::OfferWindowUnresolved { submission })
        .collect()
}

#[derive(Debug)]
enum PositionAssessmentKind {
    /// Boxed candidate: without it, the enum grows to the size of
    /// largest variant on every position (clippy::large_enum_variant).
    Selected(Box<SelectedPrice>),
    LegacyDerived(PriceQuality),
    Uncovered(UncoveredReason),
}

#[derive(Debug)]
struct PositionAssessment {
    account: AccountId,
    custody: Option<crate::ids::CustodyId>,
    instrument: InstrumentId,
    quantity: crate::money::Quantity,
    raw_price: Option<crate::valuation::InstrumentPrice>,
    kind: PositionAssessmentKind,
    remaining_face: Result<Option<PerUnitAmount>, NotComputable>,
}

fn position_assessments(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
) -> Vec<PositionAssessment> {
    let defaults = ValuationPolicyV1::default();
    let policy = ValuationPolicyV1 {
        carry_forward_limit: defaults.carry_forward_limit,
        price_max_age: defaults.price_max_age,
        source_priority_version: SourcePriorityVersion(request.coordinate.source_priority_version),
    };
    let source = SourceId(uuid::Uuid::nil());
    state
        .balances()
        .iter_positions()
        .filter(|(key, quantity)| request.contour.contains(key.account) && !quantity.0.is_zero())
        .map(|(key, quantity)| {
            let observations: Vec<_> = state
                .prices()
                .observations_at_or_before(key.instrument, request.as_of)
                .copied()
                .collect();
            let raw_price = observations.first().copied();
            let mut candidates = Vec::new();
            let mut legacy_quality = None;
            for price in &observations {
                let candidate = PriceCandidate {
                    instrument: price.instrument,
                    price: price.price,
                    currency: price.currency,
                    // §10.3: owner price — money per unit
                    // by definition, not guesswork. Entering a percentage
                    // of face value via `EventKind::Valuation` is prohibited.
                    basis: crate::valuation::QuotationBasis::MoneyPerUnit,
                    basis_evidence: "journal:valuation".to_owned(),
                    basis_evidence_contradicts: false,
                    trade_date: price.as_of,
                    observed_at: None,
                    origin: crate::valuation::PriceOrigin::ReportParsed { source },
                    executability: SourceExecutability::Unknown,
                };
                match candidate_from_legacy_valuation(price.quality, candidate) {
                    LegacyValuationOutcome::Candidate(candidate) => candidates.push(candidate),
                    LegacyValuationOutcome::LegacyDerived(quality) => {
                        legacy_quality.get_or_insert(quality);
                    }
                }
            }
            candidates.extend(
                request
                    .market_prices
                    .iter()
                    .filter(|candidate| {
                        candidate.instrument == key.instrument
                            && candidate.trade_date <= request.as_of
                    })
                    .cloned(),
            );
            let kind = if candidates.is_empty() {
                match legacy_quality {
                    Some(quality) => PositionAssessmentKind::LegacyDerived(quality),
                    None => PositionAssessmentKind::Uncovered(UncoveredReason::NoObservation),
                }
            } else {
                let result = policy.select(
                    &PriceQuery {
                        instrument: key.instrument,
                        as_of: request.as_of,
                        knowledge_as_of: request.coordinate.knowledge_as_of,
                    },
                    &candidates,
                );
                match result.selected() {
                    Some(selected) => PositionAssessmentKind::Selected(Box::new(selected.clone())),
                    None => PositionAssessmentKind::Uncovered(policy_uncovered_reason(
                        result
                            .uncovered_reason()
                            .unwrap_or(PolicyUncoveredReason::NoObservation),
                    )),
                }
            };
            PositionAssessment {
                account: key.account,
                custody: key.custody,
                instrument: key.instrument,
                quantity,
                raw_price,
                remaining_face: request
                    .bond_schedules
                    .get(&key.instrument)
                    .map(|schedule| {
                        crate::bond::remaining_principal(schedule, request.as_of)
                            .map_err(|error| remaining_principal_reason(error, key.instrument))
                    })
                    .transpose(),
                kind,
            }
        })
        .collect()
}

fn position_values(state: &LedgerState, request: &ReturnsRequest<'_>) -> Vec<PositionValue> {
    position_values_from_assessments(position_assessments(state, request), request)
}

struct PositionValue {
    assessment: PositionAssessment,
    value: Result<Dec, NotComputable>,
}

fn position_values_from_assessments(
    assessments: Vec<PositionAssessment>,
    request: &ReturnsRequest<'_>,
) -> Vec<PositionValue> {
    let (_, rule) = quotation_rule();
    assessments
        .into_iter()
        .map(|assessment| {
            let value = match &assessment.kind {
                PositionAssessmentKind::Selected(selected) => position_value(
                    &assessment,
                    PositionQuotation {
                        price: selected.candidate.price,
                        basis: selected.candidate.basis,
                        venue_currency: selected.candidate.currency,
                        remaining_face: assessment.remaining_face.clone(),
                        rule: &rule,
                    },
                    request,
                ),
                PositionAssessmentKind::LegacyDerived(_) => {
                    if let Some(price) = assessment.raw_price {
                        position_value(
                            &assessment,
                            PositionQuotation {
                                price: price.price,
                                basis: QuotationBasis::MoneyPerUnit,
                                venue_currency: price.currency,
                                remaining_face: Ok(None),
                                rule: &rule,
                            },
                            request,
                        )
                    } else {
                        Err(NotComputable::MissingPrice {
                            instrument: assessment.instrument,
                        })
                    }
                }
                PositionAssessmentKind::Uncovered(reason) => Err(match reason {
                    UncoveredReason::NotComputable { reason } => reason.clone(),
                    UncoveredReason::NoObservation
                    | UncoveredReason::TooOld
                    | UncoveredReason::AmbiguousVenue
                    | UncoveredReason::AmbiguousCandidate => NotComputable::MissingPrice {
                        instrument: assessment.instrument,
                    },
                }),
            };
            PositionValue { assessment, value }
        })
        .collect()
}

struct PositionQuotation<'a> {
    price: Dec,
    basis: QuotationBasis,
    venue_currency: CurrencyCode,
    remaining_face: Result<Option<PerUnitAmount>, NotComputable>,
    rule: &'a dyn QuotationRule,
}

/// Why the remaining face value was not derived from the schedule.
///
/// A schedule trust failure is represented separately from an unknown
/// face value: in the first case, the holder needs a verified snapshot
/// of the issue, while in the second — the face value itself.
fn remaining_principal_reason(
    error: crate::bond::RemainingPrincipalError,
    instrument: InstrumentId,
) -> NotComputable {
    match error {
        crate::bond::RemainingPrincipalError::Unknown => {
            NotComputable::RemainingFaceUnknown { instrument }
        }
        crate::bond::RemainingPrincipalError::ScheduleNotValidated => {
            NotComputable::ScheduleMissing { instrument }
        }
        crate::bond::RemainingPrincipalError::ShareNotPositive
        | crate::bond::RemainingPrincipalError::PrefixAboveHundred => {
            NotComputable::ScheduleMissing { instrument }
        }
        crate::bond::RemainingPrincipalError::Numeric(_) => NotComputable::Numeric {
            code: "remaining_principal",
        },
    }
}

fn quotation_error(error: QuotationError, instrument: InstrumentId) -> NotComputable {
    match error {
        QuotationError::BasisUnknown => NotComputable::QuotationBasisUnknown { instrument },
        QuotationError::PrincipalUnknown => NotComputable::RemainingFaceUnknown { instrument },
        QuotationError::Numeric(_) => NotComputable::Numeric { code: "numeric" },
    }
}

fn position_value(
    assessment: &PositionAssessment,
    quotation: PositionQuotation<'_>,
    request: &ReturnsRequest<'_>,
) -> Result<Dec, NotComputable> {
    if let PositionAssessmentKind::Selected(selected) = &assessment.kind {
        if selected.candidate.basis_evidence_contradicts {
            return Err(NotComputable::QuotationBasisContradictsEvidence {
                instrument: assessment.instrument,
            });
        }
    }
    let remaining_face = match quotation.basis {
        QuotationBasis::PercentOfRemainingFace => quotation.remaining_face?,
        QuotationBasis::MoneyPerUnit | QuotationBasis::Unknown => None,
    };
    let (money_per_unit, currency) = quotation
        .rule
        .money_per_unit(
            quotation.basis,
            quotation.price,
            quotation.venue_currency,
            remaining_face,
        )
        .map_err(|error| quotation_error(error, assessment.instrument))?;
    let local = assessment
        .quantity
        .0
        .checked_mul(money_per_unit)
        .map_err(|_| NotComputable::Numeric { code: "numeric" })?;
    let rate = request
        .fx
        .rate(currency, request.report_currency, request.as_of)
        .ok_or(NotComputable::MissingFxRate {
            from: currency,
            to: request.report_currency,
            date: request.as_of,
        })?;
    local
        .checked_mul(rate)
        .map_err(|_| NotComputable::Numeric { code: "numeric" })
}

/// The data quality block is built from state, the reconciliation registry and valuation
/// the perimeter, not a desire to show a green status.
fn data_quality(
    state: &LedgerState,
    request: &ReturnsRequest,
    positions: &[PositionValue],
) -> DataQuality {
    let mut issues = Vec::new();
    for account in state.coverage().restored_accounts() {
        issues.push(MaterialIssue::RestoredWithoutBasis { account: *account });
    }
    for (key, lots) in state.book().iter() {
        if matches!(lots.gap(), Some(BasisGap::AmortisationAllocationUnknown)) {
            issues.push(MaterialIssue::AmortisationAllocationUnknown {
                account: key.account,
                instrument: key.instrument,
            });
        }
    }
    for (account, money) in state.balances().negative_cash() {
        issues.push(MaterialIssue::NegativeCash {
            account,
            currency: money.currency(),
        });
    }
    if let Some(date) = state.coverage().first_event() {
        issues.push(MaterialIssue::HistoryStartsAt { date });
    }

    let (_, accrued_rule) = accrued_interest_rule();
    issues.extend(accrued_mismatch_issues(positions, request, &accrued_rule));
    let mut position_coverage = PositionCoverage {
        evaluated_positions: 0,
        total_positions: positions.len() as u32,
        selected: Vec::new(),
        uncovered: Vec::new(),
        legacy_derived: Vec::new(),
    };
    let mut executability = ExecutabilityAccumulator::default();
    for position in positions {
        let assessment = &position.assessment;
        match (&assessment.kind, &position.value) {
            (PositionAssessmentKind::Selected(selected), Ok(value)) => {
                position_coverage.evaluated_positions += 1;
                position_coverage.selected.push(EvaluatedPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    quantity: assessment.quantity,
                    price: (**selected).clone(),
                });
                executability.add(selected.candidate.executability, *value);
            }
            (PositionAssessmentKind::Selected(_), Err(reason)) => {
                position_coverage.uncovered.push(UncoveredPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    reason: UncoveredReason::NotComputable {
                        reason: reason.clone(),
                    },
                });
            }
            (PositionAssessmentKind::LegacyDerived(quality), Ok(value)) => {
                position_coverage.evaluated_positions += 1;
                position_coverage
                    .legacy_derived
                    .push(LegacyDerivedPosition {
                        account: assessment.account,
                        custody: assessment.custody,
                        instrument: assessment.instrument,
                        quality: *quality,
                    });
                executability.add(SourceExecutability::Unknown, *value);
            }
            (PositionAssessmentKind::LegacyDerived(_), Err(reason)) => {
                position_coverage.uncovered.push(UncoveredPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    reason: UncoveredReason::NotComputable {
                        reason: reason.clone(),
                    },
                });
            }
            (PositionAssessmentKind::Uncovered(reason), _) => {
                position_coverage.uncovered.push(UncoveredPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    reason: reason.clone(),
                });
            }
        }
    }

    // The account value may be impossible to calculate — for example, without a price.
    // Then there is nothing to weight coverage by, and it honestly remains
    // unknown rather than being presented as complete.
    let values =
        xirr::account_values_from_position_values(state, request, positions).unwrap_or_default();
    let mut shares = Shares::default();
    for (account, value) in &values {
        if request.perimeter.financing_present(*account) {
            issues.push(MaterialIssue::UnsupportedFinancing { account: *account });
        }
        // Cash is confirmed by the `cash` measurement, securities — by the
        // `positions`. These are different assertions about different parts of the account,
        // and weighting them under a single status would either understate
        // cash confirmation because of unconfirmed securities, or
        // vice versa.
        for (part, dimension) in [
            (value.cash, Dimension::Cash),
            (value.positions, Dimension::Positions),
        ] {
            if part.is_zero() {
                // A measurement in which the account has nothing has nothing to
                // confirm: reporting it as unconfirmed is
                // noise, not a problem.
                continue;
            }
            let status = request
                .ledger
                .status_for(*account, request.as_of, dimension);
            match status {
                DimensionStatus::Discrepant => issues.push(MaterialIssue::Discrepancy {
                    account: *account,
                    dimension,
                }),
                DimensionStatus::Provisional => {
                    issues.push(MaterialIssue::NoIndependentSource {
                        account: *account,
                        dimension,
                    });
                }
                DimensionStatus::AcceptedInternal | DimensionStatus::AcceptedIndependent => {}
            }
            shares.add(status, part.inner().abs());
        }
    }
    let nav_coverage = shares.finish();

    let material =
        !position_coverage.uncovered.is_empty() || issues.iter().any(MaterialIssue::is_defect);
    let status = if material {
        DataQualityStatus::Incomplete
    } else if nav_coverage.provisional.is_zero() && nav_coverage.discrepant.is_zero() {
        DataQualityStatus::Clean
    } else {
        DataQualityStatus::Mixed
    };
    DataQuality {
        status,
        nav_coverage,
        position_coverage,
        executability: executability.finish(),
        material_issues: issues,
    }
}

#[derive(Debug, Default)]
struct ExecutabilityAccumulator {
    evaluated_positions_value: rust_decimal::Decimal,
    executable: rust_decimal::Decimal,
    indicative_previous_close: rust_decimal::Decimal,
    unknown: rust_decimal::Decimal,
}

impl ExecutabilityAccumulator {
    fn add(&mut self, executability: SourceExecutability, value: Dec) {
        let value = value.inner().abs();
        self.evaluated_positions_value += value;
        match executability {
            SourceExecutability::Executable => self.executable += value,
            SourceExecutability::IndicativePreviousClose => self.indicative_previous_close += value,
            SourceExecutability::Unknown => self.unknown += value,
        }
    }

    fn finish(self) -> ExecutabilityShares {
        let total = self.evaluated_positions_value;
        if total.is_zero() {
            return ExecutabilityShares {
                evaluated_positions_value: Dec::zero(),
                executable: Dec::zero(),
                indicative_previous_close: Dec::zero(),
                unknown: Dec::one(),
            };
        }
        let executable = self.executable / total;
        let indicative_previous_close = self.indicative_previous_close / total;
        let unknown = self.unknown / total;
        ExecutabilityShares {
            evaluated_positions_value: Dec::new(total),
            executable: Dec::new(executable),
            indicative_previous_close: Dec::new(indicative_previous_close),
            unknown: Dec::new(unknown),
        }
    }
}

/// Fraction accumulator.
///
/// Computes using `rust_decimal`, because the share is a calculated value,
/// rather than the settled amount (§3.4).
#[derive(Debug, Default)]
struct Shares {
    independent: rust_decimal::Decimal,
    internal: rust_decimal::Decimal,
    provisional: rust_decimal::Decimal,
    discrepant: rust_decimal::Decimal,
}

impl Shares {
    fn add(&mut self, level: DimensionStatus, weight: rust_decimal::Decimal) {
        let slot = match level {
            DimensionStatus::AcceptedIndependent => &mut self.independent,
            DimensionStatus::AcceptedInternal => &mut self.internal,
            DimensionStatus::Provisional => &mut self.provisional,
            DimensionStatus::Discrepant => &mut self.discrepant,
        };
        *slot += weight;
    }

    /// Shares of the sum of weights.
    ///
    /// A zero total means an empty portfolio or an uncalculated value
    /// value: the shares are indeterminate, and the honest answer is «nothing to
    /// confirmed», rather than division by zero or a fabricated unit
    /// in independent confirmation.
    fn finish(self) -> NavCoverage {
        let total = self.independent + self.internal + self.provisional + self.discrepant;
        if total.is_zero() {
            return NavCoverage {
                accepted_independent: Dec::zero(),
                accepted_internal: Dec::zero(),
                provisional: Dec::one(),
                discrepant: Dec::zero(),
            };
        }
        NavCoverage {
            accepted_independent: Dec::new(self.independent / total),
            accepted_internal: Dec::new(self.internal / total),
            provisional: Dec::new(self.provisional / total),
            discrepant: Dec::new(self.discrepant / total),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::xirr::SolverRefusal;

    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{Money, PerUnitAmount, PostedMinor, Quantity};
    use crate::projection::lots::LotBook;
    use crate::projection::{ProjectionContext, project};
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use crate::valuation::{PriceKind as CorePriceKind, PriceOrigin, PriceQuality};
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    static EMPTY_BOND_SCHEDULES: BTreeMap<InstrumentId, BondSchedule> = BTreeMap::new();
    static EMPTY_ACCRUED_OBSERVATIONS: BTreeMap<(InstrumentId, Venue, Date), PerUnitAmount> =
        BTreeMap::new();

    fn report_for(state: &LedgerState, coordinate: KnowledgeCoordinate) -> ReturnsReport {
        let contour = ContourDefinition::new(
            ContourId(uuid::Uuid::nil()),
            ContourVersion(1),
            Vec::<AccountId>::new(),
        );
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate,
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        returns_report(state, &request)
    }

    fn report_for_schedules(
        state: &LedgerState,
        schedules: &BTreeMap<InstrumentId, BondSchedule>,
    ) -> ReturnsReport {
        let contour = ContourDefinition::new(
            ContourId(uuid::Uuid::nil()),
            ContourVersion(1),
            Vec::<AccountId>::new(),
        );
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        returns_report(
            state,
            &ReturnsRequest {
                contour: &contour,
                as_of: date!(2026 - 08 - 26),
                report_currency: CurrencyCode::Rub,
                fx: &fx,
                solver_policy: SolverPolicy::returns_default(),
                coordinate: KnowledgeCoordinate::default(),
                ledger: &ledger,
                perimeter: &perimeter,
                market_prices: &[],
                bond_schedules: schedules,
                accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            },
        )
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn position_assessment(
        account: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> PositionAssessment {
        PositionAssessment {
            account,
            custody: None,
            instrument,
            quantity,
            raw_price: None,
            remaining_face: Ok(None),
            kind: PositionAssessmentKind::Uncovered(UncoveredReason::NoObservation),
        }
    }
    fn legacy_position_assessment(
        account: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
        raw_price: Option<crate::valuation::InstrumentPrice>,
    ) -> PositionAssessment {
        PositionAssessment {
            account,
            custody: None,
            instrument,
            quantity,
            raw_price,
            remaining_face: Ok(None),
            kind: PositionAssessmentKind::LegacyDerived(PriceQuality::CarriedForward),
        }
    }

    fn legacy_price(instrument: InstrumentId, price: &str) -> crate::valuation::InstrumentPrice {
        crate::valuation::InstrumentPrice {
            instrument,
            price: dec(price),
            currency: CurrencyCode::Rub,
            quality: PriceQuality::CarriedForward,
            as_of: date!(2026 - 08 - 25),
        }
    }

    fn position_values_for_tests(assessments: Vec<PositionAssessment>) -> Vec<PositionValue> {
        assessments
            .into_iter()
            .map(|assessment| PositionValue {
                assessment,
                value: Ok(Dec::zero()),
            })
            .collect()
    }

    fn selected_market_position_assessment(
        account: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
        venue: Venue,
        trade_date: Date,
    ) -> PositionAssessment {
        let candidate = PriceCandidate {
            instrument,
            price: dec("100"),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::Unknown,
            basis_evidence: String::new(),
            basis_evidence_contradicts: false,
            trade_date,
            observed_at: Some(datetime!(2026 - 08 - 26 09:00:00 UTC)),
            origin: PriceOrigin::Market {
                venue: venue.clone(),
                kind: CorePriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        };
        let selected = SelectedPrice {
            candidate: candidate.clone(),
            selection: crate::valuation::PriceSelection::AsObserved,
            freshness: crate::valuation::PriceFreshness::Fresh,
            provenance: crate::valuation::PriceProvenance {
                price_kind: Some("legal_close".to_owned()),
                origin: candidate.origin,
                venue: Some(venue.board.clone()),
                quotation_basis: QuotationBasis::Unknown,
                basis_evidence: String::new(),
                observed_at: candidate.observed_at,
                valuation_policy_version: 1,
                source_priority_version: 1,
                carry_forward_limit: 10,
                price_max_age: 30,
            },
        };
        PositionAssessment {
            account,
            custody: None,
            instrument,
            quantity,
            raw_price: None,
            remaining_face: Ok(None),
            kind: PositionAssessmentKind::Selected(Box::new(selected)),
        }
    }

    fn position_request<'a>(
        contour: &'a ContourDefinition,
        fx: &'a FxTable,
        ledger: &'a ReconciliationLedger,
        perimeter: &'a PerimeterAssessment,
    ) -> ReturnsRequest<'a> {
        ReturnsRequest {
            contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger,
            perimeter,
            market_prices: &[],
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        }
    }
    fn report_with_market_basis(basis: QuotationBasis) -> (ReturnsReport, InstrumentId) {
        report_with_market_basis_and_schedule(basis, None)
    }

    fn report_with_market_basis_and_schedule(
        basis: QuotationBasis,
        schedule: Option<BondSchedule>,
    ) -> (ReturnsReport, InstrumentId) {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let quantity = Quantity(dec("10"));
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        );
        let state = project(&[opening], &context)
            .expect("position projection")
            .snapshot()
            .state()
            .clone();
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let candidate = PriceCandidate {
            instrument,
            price: dec("98.5"),
            currency: CurrencyCode::Rub,
            basis,
            basis_evidence: "test:market".to_owned(),
            basis_evidence_contradicts: false,
            trade_date: date!(2026 - 08 - 03),
            observed_at: Some(datetime!(2026 - 08 - 26 09:00:00 UTC)),
            origin: PriceOrigin::Market {
                venue: crate::valuation::Venue {
                    board: "TQOB".to_owned(),
                    session: 0,
                },
                kind: CorePriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        };
        let bond_schedules = schedule
            .map(|schedule| BTreeMap::from([(instrument, schedule)]))
            .unwrap_or_default();
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate {
                knowledge_as_of: datetime!(2026 - 08 - 26 12:00:00 UTC),
                source_priority_version: 1,
                valuation_policy_version: 1,
            },
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &bond_schedules,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        (returns_report(&state, &request), instrument)
    }
    #[test]
    fn legacy_valuation_enters_terminal_value_as_money_per_unit() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let without_legacy_positions = position_values_from_assessments(Vec::new(), &request);
        let legacy_positions = position_values_from_assessments(
            vec![legacy_position_assessment(
                account,
                instrument,
                Quantity(dec("10")),
                Some(legacy_price(instrument, "12")),
            )],
            &request,
        );

        let without_legacy_value =
            xirr::terminal_value_from_position_values(&state, &request, &without_legacy_positions)
                .unwrap();
        let legacy_value =
            xirr::terminal_value_from_position_values(&state, &request, &legacy_positions).unwrap();
        let quality = data_quality(&state, &request, &legacy_positions);
        assert_eq!(quality.position_coverage.evaluated_positions, 1);
        assert_eq!(quality.position_coverage.legacy_derived.len(), 1);

        assert_eq!(without_legacy_value, Dec::zero());
        assert_eq!(
            legacy_value.checked_sub(without_legacy_value).unwrap(),
            dec("120")
        );
    }

    #[test]
    fn legacy_valuation_without_raw_price_becomes_uncovered_with_missing_price() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = legacy_position_assessment(account, instrument, Quantity(dec("10")), None);

        let positions = position_values_from_assessments(vec![assessment], &request);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let quality = data_quality(&state, &request, &positions);

        assert_eq!(
            positions[0].value,
            Err(NotComputable::MissingPrice { instrument })
        );
        assert_eq!(quality.position_coverage.evaluated_positions, 0);
        assert_eq!(quality.position_coverage.uncovered.len(), 1);
        assert_eq!(
            quality.position_coverage.uncovered[0].reason,
            UncoveredReason::NotComputable {
                reason: NotComputable::MissingPrice { instrument }
            }
        );
    }

    #[test]
    fn uncovered_position_reports_all_reasons_in_not_computable() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let not_computable_instrument = InstrumentId::new_random();
        let cases = [
            (InstrumentId::new_random(), UncoveredReason::NoObservation),
            (InstrumentId::new_random(), UncoveredReason::TooOld),
            (InstrumentId::new_random(), UncoveredReason::AmbiguousVenue),
            (
                InstrumentId::new_random(),
                UncoveredReason::AmbiguousCandidate,
            ),
            (
                not_computable_instrument,
                UncoveredReason::NotComputable {
                    reason: NotComputable::QuotationBasisUnknown {
                        instrument: not_computable_instrument,
                    },
                },
            ),
        ];
        let assessments = cases
            .iter()
            .map(|(instrument, reason)| {
                let mut assessment = position_assessment(account, *instrument, Quantity(dec("1")));
                assessment.kind = PositionAssessmentKind::Uncovered(reason.clone());
                assessment
            })
            .collect();

        let positions = position_values_from_assessments(assessments, &request);

        for ((instrument, reason), position) in cases.iter().zip(&positions) {
            let expected = match reason {
                UncoveredReason::NotComputable { reason } => reason.clone(),
                UncoveredReason::NoObservation
                | UncoveredReason::TooOld
                | UncoveredReason::AmbiguousVenue
                | UncoveredReason::AmbiguousCandidate => NotComputable::MissingPrice {
                    instrument: *instrument,
                },
            };
            assert_eq!(position.value, Err(expected));
        }
    }

    #[test]
    fn uncovered_reason_code_has_codes_for_all_branches() {
        let instrument = InstrumentId::new_random();
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::NoObservation),
            "no_observation"
        );
        assert_eq!(uncovered_reason_code(&UncoveredReason::TooOld), "too_old");
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::AmbiguousVenue),
            "ambiguous_venue"
        );
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::AmbiguousCandidate),
            "ambiguous_candidate"
        );
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::NotComputable {
                reason: NotComputable::MissingPrice { instrument }
            }),
            "missing_price"
        );
    }

    #[test]
    fn policy_uncovered_reason_displays_all_four_variants() {
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::NoObservation),
            UncoveredReason::NoObservation
        );
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::TooOld),
            UncoveredReason::TooOld
        );
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::AmbiguousVenue),
            UncoveredReason::AmbiguousVenue
        );
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::AmbiguousCandidate),
            UncoveredReason::AmbiguousCandidate
        );
    }

    #[test]
    fn exchange_valuation_without_journal_valuation_enters_contour_value() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::MoneyPerUnit);

        assert_eq!(report.terminal_value, Computed::Value(dec("985")));
        assert_eq!(report.data_quality.position_coverage.evaluated_positions, 1);
        assert!(
            report
                .data_quality
                .position_coverage
                .uncovered
                .iter()
                .all(|position| position.instrument != instrument)
        );
    }
    #[test]
    fn unknown_basis_makes_position_uncovered_with_recalculation_reason() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::Unknown);
        let coverage = &report.data_quality.position_coverage;

        assert_eq!(coverage.evaluated_positions, 0);
        assert_eq!(coverage.uncovered.len(), 1);
        assert_eq!(
            coverage.uncovered[0].reason,
            UncoveredReason::NotComputable {
                reason: NotComputable::QuotationBasisUnknown { instrument },
            }
        );
        assert_eq!(
            report.terminal_value.reason(),
            Some(&NotComputable::QuotationBasisUnknown { instrument })
        );
        assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
    }

    #[test]
    fn uncovered_position_alone_makes_quality_incomplete() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let positions = position_values_for_tests(vec![position_assessment(
            account,
            instrument,
            Quantity(dec("1")),
        )]);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));

        let quality = data_quality(&state, &request, &positions);

        assert_eq!(quality.position_coverage.uncovered.len(), 1);
        assert_eq!(quality.status, DataQualityStatus::Incomplete);
    }

    #[test]
    fn basis_contradiction_has_separate_position_reason() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let mut assessment = selected_market_position_assessment(
            account,
            instrument,
            Quantity(dec("10")),
            Venue {
                board: "TQBR".to_owned(),
                session: 3,
            },
            date!(2026 - 08 - 26),
        );
        if let PositionAssessmentKind::Selected(selected) = &mut assessment.kind {
            selected.candidate.basis = QuotationBasis::Unknown;
            selected.candidate.basis_evidence_contradicts = true;
        }

        let positions = position_values_from_assessments(vec![assessment], &request);

        assert_eq!(
            positions[0].value,
            Err(NotComputable::QuotationBasisContradictsEvidence { instrument })
        );
    }

    #[test]
    fn coverage_counts_each_position_exactly_once() {
        let (report, _) = report_with_market_basis(QuotationBasis::MoneyPerUnit);
        let coverage = &report.data_quality.position_coverage;

        assert_eq!(
            coverage.evaluated_positions as usize + coverage.uncovered.len(),
            coverage.total_positions as usize
        );
        assert_eq!(
            coverage.evaluated_positions as usize,
            coverage.selected.len() + coverage.legacy_derived.len()
        );
        assert!(report.terminal_value.value().is_some());
    }

    #[test]
    fn a_share_does_not_get_bond_position_attributes() {
        let (report, _) = report_with_market_basis(QuotationBasis::MoneyPerUnit);

        assert!(report.bond_attributes.is_empty());
    }

    #[test]
    fn a_bond_without_a_schedule_keeps_a_schedule_missing_attribute() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::PercentOfRemainingFace);

        let attributes = report
            .bond_attributes
            .first()
            .expect("bond must have attributes");
        assert!(matches!(
            attributes.accrued_interest,
            Computed::NotComputable {
                reason: NotComputable::ScheduleMissing { instrument: actual }
            } if actual == instrument
        ));
    }

    #[test]
    fn a_coupon_as_the_next_posting_has_no_principal_return_finality() {
        let (report, instrument) = report_with_market_basis_and_schedule(
            QuotationBasis::PercentOfRemainingFace,
            Some(BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![crate::bond::PrincipalReturn {
                    repayment_date: date!(2026 - 10 - 01),
                    share_percent: dec("100"),
                }],
                ..Default::default()
            }),
        );
        let attributes = report
            .bond_attributes
            .iter()
            .find(|attributes| attributes.instrument == instrument)
            .expect("coupon bond must have attributes");

        assert_eq!(attributes.next_posting_date, Some(date!(2026 - 09 - 01)));
        assert_eq!(attributes.next_principal_return_finality, None);
    }

    #[test]
    fn principal_return_finality_comes_from_the_return_on_the_next_date() {
        let (report, instrument) = report_with_market_basis_and_schedule(
            QuotationBasis::PercentOfRemainingFace,
            Some(BondSchedule {
                periods: vec![],
                principal_returns: vec![
                    crate::bond::PrincipalReturn {
                        repayment_date: date!(2026 - 09 - 15),
                        share_percent: dec("40"),
                    },
                    crate::bond::PrincipalReturn {
                        repayment_date: date!(2026 - 10 - 15),
                        share_percent: dec("60"),
                    },
                ],
                ..Default::default()
            }),
        );
        let attributes = report
            .bond_attributes
            .iter()
            .find(|attributes| attributes.instrument == instrument)
            .expect("coupon bond must have attributes");

        assert_eq!(attributes.next_posting_date, Some(date!(2026 - 09 - 15)));
        assert_eq!(
            attributes.next_principal_return_finality,
            Some(PrincipalReturnFinality::Partial)
        );
    }

    #[test]
    fn a_bond_quoted_in_percent_is_valued_through_its_remaining_face() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        let value = position_value(
            &assessment,
            PositionQuotation {
                price: dec("98.5"),
                basis: QuotationBasis::PercentOfRemainingFace,
                venue_currency: CurrencyCode::Rub,
                remaining_face: Ok(Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub))),
                rule: &rule,
            },
            &request,
        );

        assert_eq!(value, Ok(dec("9850.0")));
    }

    #[test]
    fn percentage_quote_enters_nav_through_remaining_principal() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let venue = Venue {
            board: "TQOB".to_owned(),
            session: 0,
        };
        let mut assessment = selected_market_position_assessment(
            account,
            instrument,
            Quantity(dec("10")),
            venue,
            date!(2026 - 08 - 26),
        );
        if let PositionAssessmentKind::Selected(selected) = &mut assessment.kind {
            selected.candidate.price = dec("98.5");
            selected.candidate.basis = QuotationBasis::PercentOfRemainingFace;
            selected.provenance.quotation_basis = QuotationBasis::PercentOfRemainingFace;
        }
        assessment.remaining_face = Ok(Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub)));
        let positions = position_values_from_assessments(vec![assessment], &request);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));

        assert_eq!(
            xirr::terminal_value_from_position_values(&state, &request, &positions),
            Ok(dec("9850.0"))
        );
    }

    #[test]
    fn a_bond_without_a_known_face_is_not_computable_with_its_own_reason() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        assert_eq!(
            position_value(
                &assessment,
                PositionQuotation {
                    price: dec("98.5"),
                    basis: QuotationBasis::PercentOfRemainingFace,
                    venue_currency: CurrencyCode::Rub,
                    remaining_face: Ok(None),
                    rule: &rule,
                },
                &request,
            ),
            Err(NotComputable::RemainingFaceUnknown { instrument }),
        );
    }

    #[test]
    fn an_undecided_basis_is_not_computable_rather_than_valued_as_money() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        assert_eq!(
            position_value(
                &assessment,
                PositionQuotation {
                    price: dec("98.5"),
                    basis: QuotationBasis::Unknown,
                    venue_currency: CurrencyCode::Rub,
                    remaining_face: Ok(None),
                    rule: &rule,
                },
                &request,
            ),
            Err(NotComputable::QuotationBasisUnknown { instrument }),
        );
    }

    #[test]
    fn a_share_quoted_in_money_is_valued_exactly_as_before() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        let value = position_value(
            &assessment,
            PositionQuotation {
                price: dec("270.13"),
                basis: QuotationBasis::MoneyPerUnit,
                venue_currency: CurrencyCode::Rub,
                remaining_face: Ok(None),
                rule: &rule,
            },
            &request,
        );

        assert_eq!(value, Ok(dec("2701.30")));
    }

    #[test]
    fn the_report_names_the_quotation_rule_it_applied() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let report = report_for(&state, KnowledgeCoordinate::default());
        assert_eq!(report.applied_rules.quotation_rule, QuotationRuleVersion(1));
    }

    #[test]
    fn the_same_coordinate_yields_the_same_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let coordinate = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };

        let first = report_for(&state, coordinate);
        let second = report_for(&state, coordinate);
        assert_eq!(first.coordinate, coordinate);
        assert_eq!(second.coordinate, coordinate);
        assert_eq!(first.inputs_hash, second.inputs_hash);
        assert_eq!(first.inputs_hash.len(), 64);
        let equivalent_coordinate = KnowledgeCoordinate {
            knowledge_as_of: coordinate
                .knowledge_as_of
                .to_offset(UtcOffset::from_hms(3, 0, 0).unwrap()),
            ..coordinate
        };
        assert_eq!(
            first.inputs_hash,
            report_for(&state, equivalent_coordinate).inputs_hash
        );
    }

    #[test]
    fn a_different_knowledge_time_yields_a_different_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let first = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };
        let second = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-27 09:00:00 UTC),
            ..first
        };

        let first_report = report_for(&state, first);
        let second_report = report_for(&state, second);
        assert_eq!(first_report.coordinate, first);
        assert_eq!(second_report.coordinate, second);
        assert_ne!(first_report.inputs_hash, second_report.inputs_hash);
    }

    #[test]
    fn a_source_correction_inside_the_window_changes_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let coordinate = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate,
            ledger: &ledger,
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            perimeter: &perimeter,
            market_prices: &[],
        };

        let hash_for_venue = |venue: &str| {
            let origin = crate::valuation::PriceOrigin::Market {
                venue: crate::valuation::Venue {
                    board: venue.to_owned(),
                    session: 0,
                },
                kind: crate::valuation::PriceKind::LegalClose,
            };
            let selected = SelectedPrice {
                candidate: PriceCandidate {
                    instrument,
                    price: Dec::new(rust_decimal::Decimal::from(100)),
                    currency: CurrencyCode::Rub,
                    basis: crate::valuation::QuotationBasis::Unknown,
                    basis_evidence: String::new(),
                    basis_evidence_contradicts: false,
                    trade_date: date!(2026 - 08 - 26),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    origin: origin.clone(),
                    executability: SourceExecutability::Executable,
                },
                selection: crate::valuation::PriceSelection::AsObserved,
                freshness: crate::valuation::PriceFreshness::Fresh,
                provenance: crate::valuation::PriceProvenance {
                    price_kind: Some("legal_close".to_owned()),
                    origin,
                    venue: Some(venue.to_owned()),
                    quotation_basis: crate::valuation::QuotationBasis::Unknown,
                    basis_evidence: String::new(),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    valuation_policy_version: coordinate.valuation_policy_version,
                    source_priority_version: coordinate.source_priority_version,
                    carry_forward_limit: 10,
                    price_max_age: 30,
                },
            };
            inputs_hash_with_assessments(
                &state,
                &request,
                vec![PositionAssessment {
                    account,
                    custody: None,
                    instrument,
                    quantity: crate::money::Quantity(Dec::one()),
                    raw_price: None,
                    remaining_face: Ok(None),
                    kind: PositionAssessmentKind::Selected(Box::new(selected)),
                }],
            )
        };

        assert_ne!(
            hash_for_venue("moex"),
            hash_for_venue("corrected-source"),
            "correcting the provenance of the selected observation within the window must change the hash"
        );
    }
    #[test]
    fn a_quotation_basis_change_inside_the_window_changes_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let coordinate = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate,
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &BTreeMap::new(),
            accrued_observations: &BTreeMap::new(),
        };

        let hash_for_basis = |basis: QuotationBasis| {
            let origin = crate::valuation::PriceOrigin::Market {
                venue: crate::valuation::Venue {
                    board: "moex".to_owned(),
                    session: 0,
                },
                kind: crate::valuation::PriceKind::LegalClose,
            };
            let selected = SelectedPrice {
                candidate: PriceCandidate {
                    instrument,
                    price: Dec::new(rust_decimal::Decimal::from(98)),
                    currency: CurrencyCode::Rub,
                    basis,
                    basis_evidence: "iss:engines/stock/markets/bonds".to_owned(),
                    basis_evidence_contradicts: false,
                    trade_date: date!(2026 - 08 - 26),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    origin: origin.clone(),
                    executability: SourceExecutability::Executable,
                },
                selection: crate::valuation::PriceSelection::AsObserved,
                freshness: crate::valuation::PriceFreshness::Fresh,
                provenance: crate::valuation::PriceProvenance {
                    price_kind: Some("legal_close".to_owned()),
                    origin,
                    venue: Some("moex".to_owned()),
                    quotation_basis: basis,
                    basis_evidence: "iss:engines/stock/markets/bonds".to_owned(),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    valuation_policy_version: coordinate.valuation_policy_version,
                    source_priority_version: coordinate.source_priority_version,
                    carry_forward_limit: 10,
                    price_max_age: 30,
                },
            };
            inputs_hash_with_assessments(
                &state,
                &request,
                vec![PositionAssessment {
                    account,
                    custody: None,
                    instrument,
                    quantity: crate::money::Quantity(Dec::one()),
                    raw_price: None,
                    remaining_face: Ok(None),
                    kind: PositionAssessmentKind::Selected(Box::new(selected)),
                }],
            )
        };

        assert_ne!(
            hash_for_basis(QuotationBasis::MoneyPerUnit),
            hash_for_basis(QuotationBasis::PercentOfRemainingFace),
            "changing the proven basis must change the input hash"
        );
    }

    #[test]
    fn an_owner_valuation_is_money_per_unit_by_contract_not_by_guess() {
        // §10.3: the owner's journal price — money per unit
        // by definition, not as a percentage of face value.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let quantity = Quantity(Dec::new(rust_decimal::Decimal::from(10)));
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let opening = event_with(
            account,
            date!(2026 - 08 - 02),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        );
        let valuation = event_with(
            account,
            date!(2026 - 08 - 03),
            2,
            EventKind::Valuation {
                instrument,
                price: Dec::new(rust_decimal::Decimal::from(98)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::OwnerEstimate,
            },
            vec![],
        );
        let state = project(&[opening, valuation], &context)
            .expect("owner valuation projection")
            .snapshot()
            .state()
            .clone();
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 03),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            perimeter: &perimeter,
            market_prices: &[],
        };
        let assessments = position_assessments(&state, &request);
        let PositionAssessmentKind::Selected(selected) = &assessments[0].kind else {
            panic!("owner valuation must be selected");
        };
        assert_eq!(
            selected.candidate.basis,
            crate::valuation::QuotationBasis::MoneyPerUnit
        );
        assert_eq!(selected.candidate.basis_evidence, "journal:valuation");
        assert_eq!(selected.candidate.observed_at, None);
        assert_eq!(selected.provenance.observed_at, None);
    }

    #[test]
    fn future_or_foreign_inputs_do_not_change_the_inputs_hash() {
        let base = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let account = AccountId::new_random();
        let foreign_contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &foreign_contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let future_event = event_with(
            account,
            date!(2026 - 09 - 01),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let future_state = project(&[future_event], &context)
            .expect("future state")
            .snapshot()
            .state()
            .clone();
        let coordinate = KnowledgeCoordinate::default();

        assert_eq!(
            report_for(&base, coordinate).inputs_hash,
            report_for(&future_state, coordinate).inputs_hash
        );
    }

    #[test]
    fn every_data_quality_status_has_a_machine_readable_code() {
        // The external agent parses the code, not the text. An empty string instead of
        // is indistinguishable in code from «no status».
        assert_eq!(DataQualityStatus::Clean.code(), "clean");
        assert_eq!(DataQualityStatus::Mixed.code(), "mixed");
        assert_eq!(DataQualityStatus::Incomplete.code(), "incomplete");
    }
    #[test]
    fn overlapping_accrual_is_a_distinct_not_computable_reason() {
        let instrument = InstrumentId::new_random();
        let reason = accrued_error(AccruedInterestError::OverlappingCoverage, instrument);
        assert_eq!(
            reason,
            NotComputable::OverlappingScheduleCoverage { instrument }
        );
        assert_eq!(reason.code(), "overlapping_schedule_coverage");
    }
    #[test]
    fn shares_add_accumulates_weights_before_normalizing() {
        let mut shares = Shares::default();
        shares.add(
            DimensionStatus::AcceptedIndependent,
            rust_decimal::Decimal::new(2, 0),
        );
        shares.add(
            DimensionStatus::Provisional,
            rust_decimal::Decimal::new(1, 0),
        );
        assert_eq!(shares.independent, rust_decimal::Decimal::new(2, 0));
        assert_eq!(shares.provisional, rust_decimal::Decimal::new(1, 0));

        let coverage = shares.finish();
        assert_eq!(
            coverage.accepted_independent,
            Dec::new(rust_decimal::Decimal::new(2, 0) / rust_decimal::Decimal::new(3, 0))
        );
        assert_eq!(
            coverage.provisional,
            Dec::new(rust_decimal::Decimal::new(1, 0) / rust_decimal::Decimal::new(3, 0))
        );
    }

    /// Builds state from one deposit and one valuation of the specified
    /// quality. Nothing else contributes to the data quality block.
    fn quality_of(price_quality: PriceQuality) -> DataQuality {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let events = vec![
            event_with(
                account,
                date!(2025 - 01 - 01),
                1,
                EventKind::CashIn { amount },
                vec![Leg::cash(account, amount)],
            ),
            event_with(
                account,
                date!(2025 - 02 - 01),
                2,
                EventKind::Valuation {
                    instrument,
                    price: Dec::one(),
                    currency: CurrencyCode::Rub,
                    quality: price_quality,
                },
                vec![],
            ),
        ];
        let projection = project(&events, &ctx).expect("projection");
        // The reconciliation registry is empty, and the scope estimate is empty: this helper
        // checks exactly the material state issues, while coverage
        // and the scope are covered by separate tests.
        let ledger = crate::reconciliation::ReconciliationLedger::default();
        let perimeter = crate::perimeter::PerimeterAssessment::empty(PerimeterPolicy::default());
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let request = ReturnsRequest {
            contour: &contour,
            coordinate: KnowledgeCoordinate::default(),
            as_of: date!(2025 - 03 - 01),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            perimeter: &perimeter,
            market_prices: &[],
        };
        let positions = position_values(projection.snapshot().state(), &request);
        data_quality(projection.snapshot().state(), &request, &positions)
    }

    #[test]
    fn the_start_of_history_is_a_fact_about_the_period_not_a_defect() {
        // Dirty price and no other problems: only the marker remains
        // “there is no data before this date.” It is always reported, but
        // does not constitute incompleteness — otherwise the `Incomplete` status would cease to
        // mean anything, because it would always be present.
        let quality = quality_of(PriceQuality::Executable);
        assert_eq!(quality.status, DataQualityStatus::Mixed);
        assert!(
            quality
                .material_issues
                .iter()
                .any(|issue| matches!(issue, MaterialIssue::HistoryStartsAt { .. })),
            "the history start must be specified"
        );
    }

    #[test]
    fn executability_shares_are_weighted_by_evaluated_position_value() {
        let mut shares = ExecutabilityAccumulator::default();
        shares.add(
            SourceExecutability::Executable,
            Dec::new(rust_decimal::Decimal::new(2, 0)),
        );
        shares.add(
            SourceExecutability::IndicativePreviousClose,
            Dec::new(rust_decimal::Decimal::new(1, 0)),
        );
        shares.add(
            SourceExecutability::Unknown,
            Dec::new(rust_decimal::Decimal::new(1, 0)),
        );

        let shares = shares.finish();
        assert_eq!(
            shares.evaluated_positions_value,
            Dec::new(rust_decimal::Decimal::new(4, 0))
        );
        assert_eq!(
            shares.executable,
            Dec::new(rust_decimal::Decimal::new(50, 2))
        );
        assert_eq!(
            shares.indicative_previous_close,
            Dec::new(rust_decimal::Decimal::new(25, 2))
        );
        assert_eq!(
            shares.executable.inner()
                + shares.indicative_previous_close.inner()
                + shares.unknown.inner(),
            rust_decimal::Decimal::ONE
        );
    }

    #[test]
    fn uncovered_positions_have_reasons_but_no_cost_percentage() {
        let instrument = InstrumentId::new_random();
        let coverage = PositionCoverage {
            evaluated_positions: 1,
            total_positions: 2,
            selected: Vec::new(),
            uncovered: vec![UncoveredPosition {
                account: AccountId::new_random(),
                custody: None,
                instrument,
                reason: UncoveredReason::TooOld,
            }],
            legacy_derived: Vec::new(),
        };

        assert_eq!(coverage.evaluated_positions, 1);
        assert_eq!(coverage.total_positions, 2);
        assert_eq!(coverage.uncovered[0].reason, UncoveredReason::TooOld);
    }

    #[test]
    fn liquidation_estimate_keeps_unknown_costs_and_tax_typed() {
        let estimate = LiquidationEstimate {
            value_before_exit_costs_and_tax: Computed::Value(Dec::new(rust_decimal::Decimal::new(
                100, 0,
            ))),
            executability: ExecutabilityShares {
                evaluated_positions_value: Dec::new(rust_decimal::Decimal::new(100, 0)),
                executable: Dec::zero(),
                indicative_previous_close: Dec::one(),
                unknown: Dec::zero(),
            },
            exit_costs: AmountQualification::Unknown,
            tax: AmountQualification::Unknown,
            accrued_interest_payable_on_termination: Computed::Value(Dec::zero()),
        };

        assert!(matches!(estimate.exit_costs, AmountQualification::Unknown));
        assert!(matches!(estimate.tax, AmountQualification::Unknown));
        assert_eq!(
            ReturnsReport::LIQUIDATION_LABEL,
            "liquidation_value_before_exit_costs_and_tax"
        );
    }

    #[test]
    fn indicative_previous_close_has_zero_executable_share_without_error() {
        let mut shares = ExecutabilityAccumulator::default();
        shares.add(
            SourceExecutability::IndicativePreviousClose,
            Dec::new(rust_decimal::Decimal::new(100, 0)),
        );
        let shares = shares.finish();
        assert!(shares.executable.is_zero());
        assert_eq!(shares.indicative_previous_close, Dec::one());
        assert_eq!(
            shares.executable.inner()
                + shares.indicative_previous_close.inner()
                + shares.unknown.inner(),
            rust_decimal::Decimal::ONE
        );
    }

    #[test]
    fn a_kopeck_of_disagreement_with_the_exchange_is_rounding_not_an_issue() {
        assert!(!accrued_mismatch_is_material(
            dec("15.17"),
            dec("15.18"),
            CurrencyCode::Rub
        ));
    }

    #[test]
    fn a_real_disagreement_names_both_numbers() {
        assert!(accrued_mismatch_is_material(
            dec("15.17"),
            dec("22.40"),
            CurrencyCode::Rub
        ));
    }
    #[test]
    fn accrued_mismatch_amounts_are_position_totals_and_duplicate_accounts_are_merged() {
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), []);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let mut schedules = BTreeMap::new();
        schedules.insert(
            instrument,
            BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![],
                ..Default::default()
            },
        );
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let mut observed = BTreeMap::new();
        observed.insert(
            (instrument, venue.clone(), date!(2026 - 08 - 26)),
            PerUnitAmount::new(dec("22.40"), CurrencyCode::Rub),
        );
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &schedules,
            accrued_observations: &observed,
        };
        let assessments = vec![
            selected_market_position_assessment(
                AccountId::new_random(),
                instrument,
                Quantity(dec("60")),
                venue.clone(),
                date!(2026 - 08 - 26),
            ),
            selected_market_position_assessment(
                AccountId::new_random(),
                instrument,
                Quantity(dec("40")),
                venue,
                date!(2026 - 08 - 26),
            ),
        ];

        let issues = accrued_mismatch_issues(
            &position_values_for_tests(assessments),
            &request,
            &AccruedInterestV1,
        );
        assert_eq!(issues.len(), 1);
        let MaterialIssue::AccruedInterestMismatch {
            computed,
            computed_currency,
            observed,
            observed_currency,
            quantity,
            ..
        } = &issues[0]
        else {
            panic!("expected an accrued-interest discrepancy");
        };
        assert_eq!(*computed, dec("2500.00"));
        assert_eq!(*computed_currency, CurrencyCode::Rub);
        assert_eq!(*observed, dec("2240.00"));
        assert_eq!(*observed_currency, CurrencyCode::Rub);
        assert_eq!(*quantity, Quantity(dec("100")));
    }
    #[test]
    fn accrued_mismatch_marks_close_values_in_different_currencies_as_material() {
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), []);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let schedules = BTreeMap::from([(
            instrument,
            BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![],
                ..Default::default()
            },
        )]);
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let observed = BTreeMap::from([(
            (instrument, venue.clone(), date!(2026 - 08 - 26)),
            PerUnitAmount::new(dec("25.01"), CurrencyCode::Usd),
        )]);
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &schedules,
            accrued_observations: &observed,
        };
        let issues = accrued_mismatch_issues(
            &position_values_for_tests(vec![selected_market_position_assessment(
                AccountId::new_random(),
                instrument,
                Quantity(dec("1")),
                venue,
                date!(2026 - 08 - 26),
            )]),
            &request,
            &AccruedInterestV1,
        );
        let MaterialIssue::AccruedInterestMismatch {
            computed,
            computed_currency,
            observed,
            observed_currency,
            ..
        } = &issues[0]
        else {
            panic!("expected an accrued-interest discrepancy");
        };
        assert_eq!(*computed_currency, CurrencyCode::Rub);
        assert_eq!(*computed, dec("25.00"));
        assert_eq!(*observed, dec("25.01"));
        assert_eq!(*observed_currency, CurrencyCode::Usd);
    }
    #[test]
    fn an_older_trade_date_does_not_supply_the_current_day_observation() {
        let instrument = InstrumentId::new_random();
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), []);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let observed = BTreeMap::from([(
            (instrument, venue.clone(), date!(2026 - 08 - 25)),
            PerUnitAmount::new(dec("22.40"), CurrencyCode::Rub),
        )]);
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &observed,
        };
        let assessment = selected_market_position_assessment(
            account,
            instrument,
            Quantity(dec("1")),
            venue,
            date!(2026 - 08 - 25),
        );

        assert!(
            accrued_mismatch_issues(
                &position_values_for_tests(vec![assessment]),
                &request,
                &AccruedInterestV1,
            )
            .is_empty()
        );
    }

    #[test]
    fn an_accrued_interest_mismatch_is_a_defect() {
        let issue = MaterialIssue::AccruedInterestMismatch {
            instrument: InstrumentId::new_random(),
            computed: dec("15.17"),
            computed_currency: CurrencyCode::Rub,
            observed: dec("22.40"),
            observed_currency: CurrencyCode::Rub,
            quantity: Quantity(dec("1")),
            date: date!(2026 - 08 - 26),
        };
        assert!(issue.is_defect());
    }
    #[test]
    fn a_report_with_an_accrued_mismatch_is_not_clean() {
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(dec("10"));
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: Some(Money::new(PostedMinor::new(100_000), CurrencyCode::Rub)),
                assertions: Default::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        );
        let valuation = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::Valuation {
                instrument,
                price: dec("100"),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::OwnerEstimate,
            },
            vec![],
        );
        let period = crate::reconciliation::claim::AssertionPeriod::between(
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 31),
        )
        .unwrap();
        let assertion = |sequence, claim| {
            event_with(
                account,
                date!(2026 - 08 - 26),
                sequence,
                EventKind::ControlAssertion { period, claim },
                vec![],
            )
        };
        let events = vec![
            opening,
            valuation,
            assertion(
                3,
                crate::reconciliation::claim::ControlClaim::PositionQuantity {
                    instrument,
                    custody,
                    quantity,
                    at: crate::reconciliation::claim::BalancePoint::Closing,
                },
            ),
            assertion(
                4,
                crate::reconciliation::claim::ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: crate::reconciliation::claim::BalancePoint::Closing,
                },
            ),
        ];
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let state = project(&events, &context)
            .unwrap()
            .snapshot()
            .state()
            .clone();
        let ledger = ReconciliationLedger::build(&events).unwrap();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let mut schedules = BTreeMap::new();
        schedules.insert(
            instrument,
            BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![],
                ..Default::default()
            },
        );
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let mut accrued = BTreeMap::new();
        accrued.insert(
            (instrument, venue.clone(), date!(2026 - 08 - 26)),
            PerUnitAmount::new(dec("22.40"), CurrencyCode::Rub),
        );
        let market_prices = [
            match selected_market_position_assessment(
                account,
                instrument,
                Quantity(dec("100")),
                venue,
                date!(2026 - 08 - 26),
            )
            .kind
            {
                PositionAssessmentKind::Selected(selected) => selected.candidate,
                _ => unreachable!(),
            },
        ];
        let report = returns_report(
            &state,
            &ReturnsRequest {
                contour: &contour,
                as_of: date!(2026 - 08 - 26),
                report_currency: CurrencyCode::Rub,
                fx: &fx,
                solver_policy: SolverPolicy::returns_default(),
                coordinate: KnowledgeCoordinate {
                    knowledge_as_of: datetime!(2026 - 08 - 26 12:00:00 UTC),
                    ..KnowledgeCoordinate::default()
                },
                ledger: &ledger,
                perimeter: &perimeter,
                market_prices: &market_prices,
                bond_schedules: &schedules,
                accrued_observations: &accrued,
            },
        );
        assert_ne!(report.data_quality.status, DataQualityStatus::Clean);
        assert!(
            report
                .data_quality
                .material_issues
                .iter()
                .any(|issue| matches!(issue, MaterialIssue::AccruedInterestMismatch { .. }))
        );
    }
    #[test]
    fn termination_value_without_an_executable_exit_is_unknown_not_the_accrual() {
        let value = payable_on_termination(
            &Computed::Value(dec("15.17")),
            SourceExecutability::IndicativePreviousClose,
        );
        assert!(matches!(
            value,
            Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable
            }
        ));
    }

    #[test]
    fn unknown_termination_values_make_only_their_aggregate_unknown() {
        let attributes = vec![BondPositionAttributes {
            account: AccountId::new_random(),
            custody: None,
            instrument: InstrumentId::new_random(),
            accrued_interest: Computed::Value(dec("15.17")),
            accrued_interest_payable_on_termination: Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable,
            },
            next_posting_date: None,
            next_principal_return_finality: None,
        }];
        assert!(matches!(
            aggregate_payable_on_termination(&attributes),
            Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable
            }
        ));
    }

    #[test]
    fn every_refusal_has_a_machine_readable_code() {
        assert_eq!(NotComputable::NoExternalFlows.code(), "no_external_flows");
        assert_eq!(
            NotComputable::SolverRefused {
                refusal: SolverRefusal::NoSignChange
            }
            .code(),
            "solver_refused"
        );
        assert_eq!(
            NotComputable::MissingPrice {
                instrument: crate::ids::InstrumentId::new_random()
            }
            .code(),
            "missing_price"
        );
    }

    #[test]
    fn a_not_computable_value_carries_no_number() {
        // The type prevents reading a number where none exists:
        // “zero with a warning” cannot be constructed (§15.2).
        let value: Computed<Dec> = Computed::NotComputable {
            reason: NotComputable::NoExternalFlows,
        };
        assert!(value.value().is_none());
        assert_eq!(
            value.reason().map(NotComputable::code),
            Some("no_external_flows")
        );
    }
    #[test]
    fn a_missing_accrued_observation_has_its_own_reason() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::PercentOfRemainingFace);

        let attributes = report
            .bond_attributes
            .first()
            .expect("bond must have attributes");
        assert!(matches!(
            attributes.accrued_interest_payable_on_termination,
            Computed::NotComputable {
                reason: NotComputable::AccruedObservationMissing { instrument: actual }
            } if actual == instrument
        ));
    }

    fn purchase_with_known_cost(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        sequence: u32,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let mut event = event_with(
            account,
            day,
            sequence,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: Some(Money::new(PostedMinor::new(100_000), CurrencyCode::Rub)),
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                quantity,
            )],
        );
        // The helper models a source that reports the ownership transfer date.
        event.dates.settled = Some(crate::dates::SettledDate(day));
        event
    }

    fn percentage_price_report_from_purchases(
        lot_count: usize,
        schedule: Option<BondSchedule>,
    ) -> ReturnsReport {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let events: Vec<_> = (0..lot_count)
            .map(|index| {
                purchase_with_known_cost(
                    account,
                    instrument,
                    date!(2026 - 08 - 01) + time::Duration::days(index as i64),
                    u32::try_from(index + 1).expect("purchase number"),
                )
            })
            .collect();
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let state = project(&events, &context)
            .expect("purchase projection")
            .snapshot()
            .state()
            .clone();
        let mut candidate = market_price(instrument, date!(2026 - 08 - 25));
        candidate.price = dec("98.5");
        candidate.basis = QuotationBasis::PercentOfRemainingFace;
        let bond_schedules = schedule
            .map(|schedule| BTreeMap::from([(instrument, schedule)]))
            .unwrap_or_default();
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &bond_schedules,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        returns_report(&state, &request)
    }

    #[test]
    fn a_percent_quote_uses_the_issue_remainder_for_every_lot() {
        // The balance belongs to the issue: two lots purchased at different
        // days are evaluated against the same outstanding face value.
        let mut schedule = coupon_schedule(&[date!(2026 - 06 - 01)], date!(2026 - 08 - 15));
        schedule.principal_returns[0].share_percent = dec("30");
        let report = percentage_price_report_from_purchases(2, Some(schedule));

        // 20 securities × 700 balance × 98.5% = 13_790.
        assert_eq!(report.terminal_value, Computed::Value(dec("13790")));
    }

    #[test]
    fn a_position_with_unpriced_quantity_still_projects_a_flow_but_has_no_lifetime_metrics() {
        // Face value belongs to the issue, so an unknown valuation
        // a partial position does not prevent constructing a flow for the entire quantity.
        // Lifetime metrics remain incalculable.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let custody = CustodyId::new_random();
        let mut priced = purchase_with_known_cost(account, instrument, date!(2026 - 08 - 01), 1);
        for leg in &mut priced.legs {
            if leg.quantity.is_some() {
                leg.custody = Some(custody);
            }
        }
        let mut unpriced = purchase_with_known_cost(account, instrument, date!(2026 - 08 - 02), 2);
        for leg in &mut unpriced.legs {
            if leg.quantity.is_some() {
                leg.custody = Some(custody);
            }
        }
        let EventKind::OpeningPosition { cost_basis, .. } = &mut unpriced.kind else {
            panic!("expected an open position");
        };
        *cost_basis = None;
        let events = vec![priced, unpriced];
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let state = project(&events, &context)
            .expect("mixed-position projection")
            .snapshot()
            .state()
            .clone();
        let schedule = coupon_schedule(&[date!(2026 - 09 - 01)], date!(2027 - 08 - 15));
        let candidate = market_price(instrument, date!(2026 - 08 - 25));
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let schedules = BTreeMap::from([(instrument, schedule)]);
        let report = returns_report(
            &state,
            &ReturnsRequest {
                contour: &contour,
                as_of: date!(2026 - 08 - 26),
                report_currency: CurrencyCode::Rub,
                fx: &fx,
                solver_policy: SolverPolicy::returns_default(),
                coordinate: KnowledgeCoordinate::default(),
                ledger: &ledger,
                perimeter: &perimeter,
                market_prices: std::slice::from_ref(&candidate),
                bond_schedules: &schedules,
                accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            },
        );
        assert_eq!(report.bond_metrics.len(), 1);
        let scenario = &report.bond_metrics[0].scenarios[0];
        assert!(matches!(scenario.prospective.metrics, Computed::Value(_)));
        assert!(matches!(scenario.lifetime, Computed::NotComputable { .. }));
    }

    #[test]
    fn a_percent_quote_without_a_schedule_is_not_computable_with_a_named_reason() {
        let report = percentage_price_report_from_purchases(1, None);

        assert!(matches!(
            report.terminal_value,
            Computed::NotComputable {
                reason: NotComputable::RemainingFaceUnknown { .. }
            }
        ));
    }
    fn state_from_event(contour: &ContourDefinition, event: &crate::event::Event) -> LedgerState {
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        project(std::slice::from_ref(event), &context)
            .expect("test event projection")
            .snapshot()
            .state()
            .clone()
    }

    fn report_hash(state: &LedgerState, contour: &ContourDefinition) -> String {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(contour, &fx, &ledger, &perimeter);
        returns_report(state, &request).inputs_hash
    }

    fn market_price(instrument: InstrumentId, trade_date: Date) -> PriceCandidate {
        PriceCandidate {
            instrument,
            price: dec("100"),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::MoneyPerUnit,
            basis_evidence: "test:market".to_owned(),
            basis_evidence_contradicts: false,
            trade_date,
            observed_at: None,
            origin: PriceOrigin::Market {
                venue: Venue {
                    board: "TQBR".to_owned(),
                    session: 0,
                },
                kind: CorePriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        }
    }

    fn report_with_market_price(
        state: &LedgerState,
        contour: &ContourDefinition,
        candidate: PriceCandidate,
    ) -> ReturnsReport {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        returns_report(state, &request)
    }

    #[test]
    fn post_report_date_flow_is_excluded_from_public_fingerprint() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let event = event_with(
            account,
            date!(2026 - 08 - 26),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let mut future_event = event.clone();
        future_event.dates =
            crate::dates::EventDates::for_cash(crate::dates::CashPostedDate(date!(2026 - 08 - 27)));
        future_event.order = crate::dates::EffectiveOrder::new(date!(2026 - 08 - 27), 1);
        let included = state_from_event(&contour, &event);
        let excluded = state_from_event(&contour, &future_event);
        let baseline_event = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::OpeningCash { amount },
            vec![Leg::cash(account, amount)],
        );
        let baseline = state_from_event(&contour, &baseline_event);

        assert_ne!(
            report_hash(&included, &contour),
            report_hash(&baseline, &contour)
        );
        assert_eq!(
            report_hash(&excluded, &contour),
            report_hash(&baseline, &contour)
        );
    }

    #[test]
    fn foreign_contour_flow_is_excluded_from_public_fingerprint() {
        let account = AccountId::new_random();
        let id = ContourId::new_random();
        let contour = ContourDefinition::new(id, ContourVersion(1), [account]);
        let foreign = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let event = event_with(
            account,
            date!(2026 - 08 - 26),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let included = state_from_event(&contour, &event);
        let excluded = state_from_event(&foreign, &event);
        let baseline_event = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::OpeningCash { amount },
            vec![Leg::cash(account, amount)],
        );
        let baseline = state_from_event(&contour, &baseline_event);

        assert_ne!(
            report_hash(&included, &contour),
            report_hash(&baseline, &contour)
        );
        assert_eq!(
            report_hash(&excluded, &contour),
            report_hash(&baseline, &contour)
        );
    }

    #[test]
    fn old_contour_version_flow_is_excluded_from_public_fingerprint() {
        let account = AccountId::new_random();
        let id = ContourId::new_random();
        let contour = ContourDefinition::new(id, ContourVersion(2), [account]);
        let foreign = ContourDefinition::new(id, ContourVersion(1), [account]);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let event = event_with(
            account,
            date!(2026 - 08 - 26),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let included = state_from_event(&contour, &event);
        let excluded = state_from_event(&foreign, &event);
        let baseline_event = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::OpeningCash { amount },
            vec![Leg::cash(account, amount)],
        );
        let baseline = state_from_event(&contour, &baseline_event);

        assert_ne!(
            report_hash(&included, &contour),
            report_hash(&baseline, &contour)
        );
        assert_eq!(
            report_hash(&excluded, &contour),
            report_hash(&baseline, &contour)
        );
    }

    fn position_state_with_inherited_valuation(
        contour: &ContourDefinition,
        instrument: InstrumentId,
        account: AccountId,
    ) -> LedgerState {
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity(dec("10")),
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                Quantity(dec("10")),
            )],
        );
        let valuation = event_with(
            account,
            date!(2026 - 08 - 25),
            2,
            EventKind::Valuation {
                instrument,
                price: dec("12"),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::CarriedForward,
            },
            vec![],
        );
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        project(&[opening, valuation], &context)
            .expect("position projection with inherited valuation")
            .snapshot()
            .state()
            .clone()
    }

    #[test]
    fn future_market_candidate_does_not_override_inherited_valuation() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let state = position_state_with_inherited_valuation(&contour, instrument, account);
        let report = report_with_market_price(
            &state,
            &contour,
            market_price(instrument, date!(2026 - 08 - 27)),
        );

        assert_eq!(
            report.data_quality.position_coverage.legacy_derived.len(),
            1
        );
        assert!(report.data_quality.position_coverage.uncovered.is_empty());
    }

    #[test]
    fn candidate_for_foreign_instrument_does_not_override_inherited_valuation() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let state = position_state_with_inherited_valuation(&contour, instrument, account);
        let report = report_with_market_price(
            &state,
            &contour,
            market_price(InstrumentId::new_random(), date!(2026 - 08 - 25)),
        );

        assert_eq!(
            report.data_quality.position_coverage.legacy_derived.len(),
            1
        );
        assert!(report.data_quality.position_coverage.uncovered.is_empty());
    }

    #[test]
    fn market_price_for_foreign_instrument_does_not_cover_position() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity(dec("10")),
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                Quantity(dec("10")),
            )],
        );
        let state = state_from_event(&contour, &opening);
        let report = report_with_market_price(
            &state,
            &contour,
            market_price(InstrumentId::new_random(), date!(2026 - 08 - 25)),
        );

        assert!(matches!(
            report.data_quality.position_coverage.uncovered.first(),
            Some(UncoveredPosition {
                reason: UncoveredReason::NoObservation,
                ..
            })
        ));
    }

    #[test]
    fn future_market_price_does_not_cover_position() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity(dec("10")),
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                Quantity(dec("10")),
            )],
        );

        let state = state_from_event(&contour, &opening);
        let report = report_with_market_price(
            &state,
            &contour,
            market_price(instrument, date!(2026 - 08 - 27)),
        );

        assert!(matches!(
            report.data_quality.position_coverage.uncovered.first(),
            Some(UncoveredPosition {
                reason: UncoveredReason::NoObservation,
                ..
            })
        ));
    }

    #[test]
    fn report_shows_accumulated_unknown_share_for_two_positions() {
        let mut shares = ExecutabilityAccumulator::default();
        shares.add(SourceExecutability::Executable, dec("2"));
        shares.add(SourceExecutability::IndicativePreviousClose, dec("1"));
        shares.add(SourceExecutability::Unknown, dec("1"));

        let shares = shares.finish();

        assert_eq!(shares.evaluated_positions_value, dec("4"));
        assert_eq!(shares.executable, dec("0.5"));
        assert_eq!(shares.indicative_previous_close, dec("0.25"));
        assert_eq!(shares.unknown, dec("0.25"));
    }
    #[test]
    fn bond_positions_receive_scenario_metrics_and_rule_versions() {
        let schedule = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                period_start: date!(2026 - 08 - 01),
                accrual_end: date!(2026 - 12 - 01),
                payment_date: date!(2026 - 12 - 02),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("5"), CurrencyCode::Rub)),
            }],
            principal_returns: vec![crate::bond::PrincipalReturn {
                repayment_date: date!(2026 - 12 - 02),
                share_percent: dec("100"),
            }],
            initial_principal: Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub)),
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window: crate::bond::OfferWindowId::new_random(),
                right: crate::bond::OfferRight::HolderPut,
                execution_date: date!(2026 - 09 - 15),
                submission_start: None,
                submission_end: None,
                price_percent: Some(dec("100")),
            }],
            completeness: crate::bond::ScheduleCompleteness::Validated,
            default_flags: Some(crate::bond::DefaultFlags {
                declared: false,
                technical: false,
            }),
            currency_roles: Some(crate::instrument::CurrencyRoles::uniform(CurrencyCode::Rub)),
        };
        let (report, _) =
            report_with_market_basis_and_schedule(QuotationBasis::MoneyPerUnit, Some(schedule));

        assert_eq!(report.bond_metrics.len(), 1);
        assert_eq!(report.bond_metrics[0].scenarios.len(), 2);
        assert_ne!(
            report.bond_metrics[0].scenarios[0]
                .prospective
                .terminal_date,
            report.bond_metrics[0].scenarios[1]
                .prospective
                .terminal_date
        );
        let (share_report, _) = report_with_market_basis(QuotationBasis::MoneyPerUnit);
        assert!(share_report.bond_metrics.is_empty());
        assert_eq!(
            report.applied_rules.cashflow_projection,
            crate::rules::CashflowProjectionVersion(2)
        );
        assert_eq!(
            report.applied_rules.expense_policy,
            crate::returns::zero_reinvestment::ExpensePolicyVersion(1)
        );
    }

    #[test]
    fn failed_offer_scenario_keeps_matching_execution_date() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let execution_date = date!(2026 - 09 - 15);
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let schedule = BondSchedule {
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window: crate::bond::OfferWindowId::new_random(),
                right: crate::bond::OfferRight::HolderPut,
                execution_date,
                submission_start: None,
                submission_end: None,
                price_percent: Some(dec("100")),
            }],
            ..BondSchedule::default()
        };
        let choice = OfferChoice::ExerciseAtOffer {
            window: schedule.offer_windows[0].window,
        };
        let assessment = position_assessment(account, instrument, Quantity(dec("1")));
        let (_, cashflow) = cashflow_projection_rule();
        let (_, accrued_rule) = accrued_interest_rule();

        let result = bond_scenario(
            &BondScenarioInputs {
                assessment: &assessment,
                request: &request,
                schedule: &schedule,
                lots: None,
                cashflow: &cashflow,
                accrued_rule: &accrued_rule,
            },
            choice.clone(),
        );

        assert_eq!(result.choice, choice);
        assert_eq!(result.prospective.terminal_date, execution_date);
        assert!(matches!(
            &result.prospective.metrics,
            Computed::NotComputable {
                reason: NotComputable::ScheduleMissing { instrument: actual }
            } if *actual == instrument
        ));
    }

    #[test]
    fn zero_quantity_bond_position_is_not_reported() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let mut request = position_request(&contour, &fx, &ledger, &perimeter);
        let schedules = BTreeMap::from([(instrument, BondSchedule::default())]);
        request.bond_schedules = &schedules;
        let positions = vec![PositionValue {
            assessment: position_assessment(account, instrument, Quantity::zero()),
            value: Ok(Dec::zero()),
        }];
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));

        let metrics = bond_position_metrics(&state, &request, &positions, &OfferBook::default());

        assert!(metrics.is_empty());
        // A zero position does not create a `LotKey` entry in an empty book, so
        // a separate pass through the book also finds no pair to reconcile.
        assert!(historical_reconciliation_issues(&state, &request).is_empty());
    }

    #[test]
    fn unresolved_offer_submission_only_removes_its_instrument_offer_scenarios() {
        let account = AccountId::new_random();
        let first_instrument = InstrumentId::new_random();
        let second_instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let quantity = Quantity(dec("10"));
        let first_opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: first_instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                first_instrument,
                quantity,
            )],
        );
        let second_opening = event_with(
            account,
            date!(2026 - 08 - 02),
            1,
            EventKind::OpeningPosition {
                instrument: second_instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                second_instrument,
                quantity,
            )],
        );
        let state = project(&[first_opening, second_opening], &context)
            .expect("two-position projection")
            .snapshot()
            .state()
            .clone();
        let first_window = crate::bond::OfferWindowId::new_random();
        let second_window = crate::bond::OfferWindowId::new_random();
        let unknown_window = crate::bond::OfferWindowId::new_random();
        let schedule = |window| BondSchedule {
            principal_returns: vec![crate::bond::PrincipalReturn {
                repayment_date: date!(2026 - 12 - 02),
                share_percent: dec("100"),
            }],
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window,
                right: crate::bond::OfferRight::HolderPut,
                execution_date: date!(2026 - 09 - 15),
                submission_start: None,
                submission_end: None,
                price_percent: Some(dec("100")),
            }],
            completeness: crate::bond::ScheduleCompleteness::Validated,
            currency_roles: Some(crate::instrument::CurrencyRoles::uniform(CurrencyCode::Rub)),
            ..Default::default()
        };
        let schedules = BTreeMap::from([
            (first_instrument, schedule(first_window)),
            (second_instrument, schedule(second_window)),
        ]);
        let submission = crate::event::offer::OfferSubmissionId::new_random();
        let mut offer_book = OfferBook::default();
        offer_book
            .apply(&crate::event::offer::OfferExerciseAction::Submitted {
                submission,
                window: unknown_window,
                instrument: first_instrument,
                quantity,
            })
            .expect("order submission");
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &schedules,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        let report = returns_report_with_bond_inputs(&state, &request, &offer_book);

        assert!(report.data_quality.material_issues.iter().any(|issue| {
            matches!(
                issue,
                MaterialIssue::OfferWindowUnresolved { submission: id } if *id == submission
            )
        }));
        let first_metrics = report
            .bond_metrics
            .iter()
            .find(|metrics| metrics.instrument == first_instrument)
            .expect("first security metrics");
        assert_eq!(first_metrics.scenarios.len(), 1);
        assert!(matches!(
            first_metrics.scenarios[0].choice,
            OfferChoice::HoldToMaturity
        ));
        let second_metrics = report
            .bond_metrics
            .iter()
            .find(|metrics| metrics.instrument == second_instrument)
            .expect("second security metrics");
        assert_eq!(second_metrics.scenarios.len(), 2);
    }
    #[test]
    fn a_record_date_change_changes_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let instrument = InstrumentId::new_random();
        let first = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                period_start: date!(2026 - 01 - 01),
                accrual_end: date!(2026 - 06 - 30),
                payment_date: date!(2026 - 07 - 01),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("5"), CurrencyCode::Rub)),
            }],
            ..Default::default()
        };
        let second = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                record_date: Some(date!(2026 - 06 - 30)),
                ..first.periods[0]
            }],
            ..first.clone()
        };
        let first_hash =
            report_for_schedules(&state, &BTreeMap::from([(instrument, first)])).inputs_hash;
        let second_hash =
            report_for_schedules(&state, &BTreeMap::from([(instrument, second)])).inputs_hash;
        assert_ne!(first_hash, second_hash);
    }

    #[test]
    fn resyncing_unchanged_schedule_keeps_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let instrument = InstrumentId::new_random();
        let schedule = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                period_start: date!(2026 - 01 - 01),
                accrual_end: date!(2026 - 06 - 30),
                payment_date: date!(2026 - 07 - 01),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("5"), CurrencyCode::Rub)),
            }],
            ..Default::default()
        };
        let inputs = BTreeMap::from([(instrument, schedule)]);
        let first_hash = report_for_schedules(&state, &inputs).inputs_hash;
        let second_hash = report_for_schedules(&state, &inputs).inputs_hash;
        assert_eq!(first_hash, second_hash);
    }

    /// Bond schedule with listed coupon dates and one
    /// redemption.
    ///
    /// Completeness, default flags, and currency roles are explicit: without them
    /// if the cash flow rule refuses to build a plan, `past` does not appear
    /// at all, and the test would verify construction failure rather than reconciliation.
    fn coupon_schedule(coupons: &[Date], repayment: Date) -> BondSchedule {
        schedule_with_offers(coupons, repayment, &[])
    }

    fn schedule_with_offers(coupons: &[Date], repayment: Date, offers: &[Date]) -> BondSchedule {
        BondSchedule {
            // The period starts with the previous payment: a closed chain
            // is needed not for reconciliation but for accrued interest — without coverage through the report date, the failure
            // comes from the accrued-interest calculation, and the test would stop distinguishing
            // reconciliation silence caused by a scenario that did not occur.
            periods: coupons
                .iter()
                .enumerate()
                .map(|(index, payment_date)| crate::bond::AccrualPeriod {
                    period_start: if index == 0 {
                        payment_date.saturating_sub(time::Duration::days(180))
                    } else {
                        coupons[index - 1]
                    },
                    accrual_end: *payment_date,
                    payment_date: *payment_date,
                    // Reconciliation tests verify ownership on the entitlement date, so
                    // the record date in this valid schedule is known.
                    record_date: Some(*payment_date),
                    coupon_per_unit: Some(PerUnitAmount::new(dec("50"), CurrencyCode::Rub)),
                })
                .collect(),
            principal_returns: vec![crate::bond::PrincipalReturn {
                repayment_date: repayment,
                share_percent: dec("100"),
            }],
            initial_principal: Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub)),
            offer_windows: offers
                .iter()
                .map(|execution_date| crate::bond::OfferWindowTerms {
                    window: crate::bond::OfferWindowId::derive(
                        InstrumentId::new_random(),
                        *execution_date,
                    ),
                    right: crate::bond::OfferRight::HolderPut,
                    execution_date: *execution_date,
                    submission_start: None,
                    submission_end: None,
                    price_percent: Some(dec("100")),
                })
                .collect(),
            completeness: crate::bond::ScheduleCompleteness::Validated,
            default_flags: Some(crate::bond::DefaultFlags {
                declared: false,
                technical: false,
            }),
            currency_roles: Some(crate::instrument::CurrencyRoles::uniform(CurrencyCode::Rub)),
        }
    }

    fn cash_in(account: AccountId, day: Date) -> crate::event::Event {
        let amount = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Rub);
        event_with(
            account,
            day,
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        )
    }

    /// Bond purchase. The trade date is specified in a separate field, because
    /// that its lot book is what writes to `Lot.acquired`, while the envelope
    /// `event_with` populates only the money credit date. `None`
    /// reproduces a regular purchase without a trade date: the schema
    /// permits (§4.9), and the account is not thereby considered reconstructed.
    fn bond_purchase(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        trade: Option<Date>,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let gross = Money::new(PostedMinor::new(100_000), CurrencyCode::Rub);
        let settlement = Money::new(PostedMinor::new(-100_000), CurrencyCode::Rub);
        let mut event = event_with(
            account,
            day,
            2,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Buy,
                instrument,
                quantity,
                gross,
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, settlement),
                Leg::security(account, CustodyId::new_random(), instrument, quantity),
            ],
        );
        event.dates.trade = trade.map(crate::dates::TradeDate);
        // The helper models a source that reports the settlement date; with
        // when the trade date is absent the source also does not report settlements.
        event.dates.settled = trade.map(crate::dates::SettledDate);
        event
    }

    /// Reconstructed position with a stated five-year-old trade date
    /// age. The credit date remains the entry date, so the log
    /// starts after the security was purchased — exactly the case
    /// which must be distinct from a missed payment.
    fn restored_position(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        trade: Date,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let mut event = event_with(
            account,
            day,
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: Some(Money::new(PostedMinor::new(100_000), CurrencyCode::Rub)),
                assertions: crate::event::kind::OpeningAssertions {
                    acquisition_date: Some(trade),
                    acquisition_date_certainty: crate::event::kind::DateCertainty::Known,
                    ..crate::event::kind::OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                quantity,
            )],
        );
        event.dates.trade = Some(crate::dates::TradeDate(trade));
        // Recovery models a source with known settlement on the day
        // records, while the historical trade date remains a separate fact.
        event.dates.settled = Some(crate::dates::SettledDate(day));
        event
    }

    /// Sale of the bond in full as one lot. The trade date is specified
    /// separately from the cash credit date for the same reason as
    /// and for the purchase.
    fn bond_sale(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        trade: Option<Date>,
        custody: CustodyId,
        sequence: u32,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let gross = Money::new(PostedMinor::new(100_000), CurrencyCode::Rub);
        let mut event = event_with(
            account,
            day,
            sequence,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Sell,
                instrument,
                quantity,
                gross,
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, gross),
                Leg::security(account, custody, instrument, Quantity(dec("-10"))),
            ],
        );
        event.dates.trade = trade.map(crate::dates::TradeDate);
        // The helper models a source that reports the settlement date; it
        // matches the trade date, if known.
        event.dates.settled = trade.map(crate::dates::SettledDate);
        event
    }

    /// Purchase into the specified custody location. A separate helper, because
    /// that `bond_purchase` fabricates a depository on every call,
    /// and here it matters that the security was booked specifically to this one.
    fn bond_purchase_in_custody(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        custody: CustodyId,
        sequence: u32,
    ) -> crate::event::Event {
        let mut event = bond_purchase(account, instrument, day, Some(day));
        event.order = crate::dates::EffectiveOrder::new(day, sequence);
        for leg in &mut event.legs {
            if leg.quantity.is_some() {
                leg.custody = Some(custody);
            }
        }
        event
    }

    fn coupon_fact(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        sequence: u32,
    ) -> crate::event::Event {
        let amount = Money::new(PostedMinor::new(50_000), CurrencyCode::Rub);
        event_with(
            account,
            day,
            sequence,
            EventKind::Income {
                instrument: Some(instrument),
                gross: amount,
                kind: Some(crate::event::kind::IncomeKind::Coupon),
            },
            vec![Leg::cash(account, amount)],
        )
    }

    /// Bond journal report.
    ///
    /// The position receives an exchange price because the uncovered position
    /// by itself makes the report incomplete and would hide the contribution of reconciliation to the status.
    fn reconciliation_report(
        accounts: &[AccountId],
        instrument: InstrumentId,
        events: &[crate::event::Event],
        schedule: &BondSchedule,
    ) -> ReturnsReport {
        reconciliation_report_at(
            accounts,
            instrument,
            events,
            schedule,
            date!(2026 - 08 - 26),
        )
    }

    /// The same report with an explicit report date. A separate parameter, because
    /// that the grace period before an alert is measured from it: without variation
    /// the waiting-period boundary cannot be checked at `as_of`.
    fn reconciliation_report_at(
        accounts: &[AccountId],
        instrument: InstrumentId,
        events: &[crate::event::Event],
        schedule: &BondSchedule,
        as_of: Date,
    ) -> ReturnsReport {
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            accounts.to_vec(),
        );
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let state = project(events, &context)
            .expect("bond journal projection")
            .snapshot()
            .state()
            .clone();
        // The price was observed the day before the report: the selection policy does not use a price from
        // the future, and the position would remain uncovered.
        let candidate = market_price(instrument, as_of.saturating_sub(time::Duration::days(1)));
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let schedules = BTreeMap::from([(instrument, schedule.clone())]);
        let request = ReturnsRequest {
            contour: &contour,
            as_of,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &schedules,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        returns_report(&state, &request)
    }

    fn contains(report: &ReturnsReport, predicate: impl Fn(&MaterialIssue) -> bool) -> bool {
        report.data_quality.material_issues.iter().any(predicate)
    }

    fn missing_postings(report: &ReturnsReport) -> Vec<&MaterialIssue> {
        report
            .data_quality
            .material_issues
            .iter()
            .filter(|issue| matches!(issue, MaterialIssue::ScheduledPostingNotReceived { .. }))
            .collect()
    }

    /// Journal with one missed payment: the coupon on 15.03 is confirmed
    /// by the fact dated 16.03, the coupon dated 15.06 is not supported by any evidence.
    fn journal_with_missing_coupon(
        account: AccountId,
        instrument: InstrumentId,
    ) -> Vec<crate::event::Event> {
        vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
            coupon_fact(account, instrument, date!(2026 - 03 - 16), 3),
        ]
    }

    #[test]
    fn a_missing_coupon_is_named_with_its_account_instrument_date_and_kind() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let report = reconciliation_report(
            &[account],
            instrument,
            &journal_with_missing_coupon(account, instrument),
            &schedule,
        );

        let issues = missing_postings(&report);
        assert_eq!(issues.len(), 1, "issues: {issues:?}");
        assert!(matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                account: issue_account,
                instrument: issue_instrument,
                date,
                kind: PostingKind::Coupon,
            } if *issue_account == account
                && *issue_instrument == instrument
                && *date == date!(2026 - 06 - 15)
        ));
    }

    #[test]
    fn a_coupon_before_the_history_horizon_is_unverifiable_even_before_purchase() {
        // Ownership before purchase is proven as `NotOwned`, but the rule ordering
        // must first report the lack of historical coverage.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![
            cash_in(account, date!(2026 - 07 - 01)),
            bond_purchase(
                account,
                instrument,
                date!(2026 - 07 - 05),
                Some(date!(2026 - 07 - 05)),
            ),
        ];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        // Both coupons predate the account's first event: they cannot be blamed
        // the issuer, even if the book already knows that the purchase was later.
        let reasons: Vec<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingUnverifiable {
                    date,
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                    ..
                } => Some((1, *date, *date)),
                MaterialIssue::ScheduledPostingsUnverifiable {
                    first_date,
                    last_date,
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                    count,
                    ..
                } => Some((*count, *first_date, *last_date)),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasons,
            vec![(2, date!(2026 - 03 - 15), date!(2026 - 06 - 15))]
        );
        assert!(missing_postings(&report).is_empty());
    }

    #[test]
    fn a_restored_history_reports_that_it_cannot_verify_rather_than_crying_wolf() {
        // A declared trade date five years in the past establishes the boundary
        // ownership before the journal began: the 2021–2025 coupons pass through it,
        // but there are no facts for them in the journal and there cannot be.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2021 - 06 - 15),
                date!(2022 - 06 - 15),
                date!(2023 - 06 - 15),
                date!(2024 - 06 - 15),
                date!(2025 - 06 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![restored_position(
            account,
            instrument,
            date!(2026 - 01 - 01),
            date!(2021 - 05 - 01),
        )];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        // June 2026 is already covered by the journal, so its absence is now
        // is tested separately; five 2021–2025 coupons are marked unverifiable.
        assert_eq!(missing_postings(&report).len(), 1);
        assert!(contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::HistoryStartsAfterSchedule,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::HistoryStartsAfterSchedule,
                ..
            }
        )));
    }
    #[test]
    fn a_posting_before_the_journal_does_not_silence_a_provable_miss_after_it() {
        // A position recovered in 2021, with a journal starting on 01.01.2026,
        // the coupons dated 15.09.2025, 15.12.2025 and 15.03.2026, the March one did not arrive.
        // Earlier returns were deemed unverifiable individually, while the March
        // the omission must not disappear during deduplication.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2025 - 09 - 15),
                date!(2025 - 12 - 15),
                date!(2026 - 03 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![restored_position(
            account,
            instrument,
            date!(2026 - 01 - 01),
            date!(2021 - 05 - 01),
        )];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);
        // Two identical unprovabilities must merge only after
        // the full traversal, without swallowing the separate finding below.
        assert!(
            report
                .data_quality
                .material_issues
                .iter()
                .any(|issue| matches!(
                    issue,
                    MaterialIssue::ScheduledPostingsUnverifiable {
                        reason: UnverifiableReason::HistoryStartsAfterSchedule,
                        count: 2,
                        first_date,
                        last_date,
                        ..
                    } if *first_date == date!(2025 - 09 - 15)
                        && *last_date == date!(2025 - 12 - 15)
                ))
        );

        let dates: Vec<_> = missing_postings(&report)
            .into_iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingNotReceived { date, .. } => Some(*date),
                _ => None,
            })
            .collect();
        assert!(
            dates.contains(&date!(2026 - 03 - 15)),
            "a gap within coverage must be identified: {dates:?}"
        );
        assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
    }

    #[test]
    fn a_posting_on_the_first_journal_day_is_covered_but_settlement_is_unknown() {
        // The journal start boundary is half-open: the day of the first event
        // is covered, so unverifiability must not be masked
        // by the `HistoryStartsAfterSchedule` reason.
        // But the entitlement date exactly matches the settlement date, and the closed
        // the ownership boundary intentionally yields `OwnershipUnknown`.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let first_day = date!(2026 - 01 - 01);
        let schedule = coupon_schedule(&[first_day, date!(2026 - 06 - 15)], date!(2026 - 12 - 15));
        // The declared trade date predates the journal: the coupon's ownership boundary
        // passes the first-day check, and its fate is determined precisely by the boundary
        // history rather than ownership-based selection.
        let mut restored = restored_position(account, instrument, first_day, date!(2025 - 06 - 01));
        // We specifically test the `Exact` boundary: the assertion falls on the same day as
        // the journal opening, so ownership on that day remains ambiguous.
        let EventKind::OpeningPosition { assertions, .. } = &mut restored.kind else {
            unreachable!("the helper creates opening_position");
        };
        assertions.acquisition_date = Some(first_day);
        assertions.acquisition_date_certainty = crate::event::kind::DateCertainty::Known;
        let events = vec![restored];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(
            !contains(&report, |issue| matches!(
                issue,
                MaterialIssue::ScheduledPostingUnverifiable {
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                    ..
                }
            )),
            "the first event day is covered by the journal: there is no unprovability here"
        );
        assert!(contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                date,
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } if *date == first_day
        )));
        // The June coupon is correctly flagged: ownership on its record date
        // is established by the settlement date of the reconstructed position, but that fact is absent.
        // The previous expectation of silence came from the era when a single
        // an unverifiable payment suppressed the entire pair (iaam-d8b.21).
        let missing_dates: Vec<_> = missing_postings(&report)
            .into_iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingNotReceived { date, .. } => Some(*date),
                _ => None,
            })
            .collect();
        assert_eq!(missing_dates, vec![date!(2026 - 06 - 15)]);
    }

    #[test]
    fn a_purchase_without_a_trade_date_leaves_the_ownership_bound_undrawable() {
        // `Lot.acquired` — `Option<TradeDate>`, and in the schema the absence of a date
        // permits this (§4.9). Here the source also does not report settlements,
        // so ownership on the record date remains unverifiable.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase(account, instrument, date!(2026 - 01 - 10), None),
        ];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            }
        )));
        assert!(!contains(&report, |issue| matches!(
            issue,
            MaterialIssue::RestoredWithoutBasis { .. }
        )));
        assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
        // The status is incomplete specifically because of reconciliation: there are no other defects in the report
        // there are none, otherwise the test would pass without the new variant.
        let others: Vec<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter(|issue| {
                issue.is_defect()
                    && !matches!(
                        issue,
                        MaterialIssue::ScheduledPostingUnverifiable { .. }
                            | MaterialIssue::ScheduledPostingsUnverifiable { .. }
                    )
            })
            .collect();
        assert!(others.is_empty(), "extraneous defects: {others:?}");
    }

    #[test]
    fn an_event_without_settlement_date_reports_unknown_ownership() {
        // A source that does not report the ownership transfer date makes ownership
        // unverifiable: the system must admit this rather than guess the trade date
        // trade date or report a finding.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
        let mut purchase = bond_purchase(
            account,
            instrument,
            date!(2026 - 01 - 10),
            Some(date!(2026 - 01 - 10)),
        );
        purchase.dates.settled = None;
        let events = vec![cash_in(account, date!(2026 - 01 - 05)), purchase];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(missing_postings(&report).is_empty());
        assert!(contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            }
        )));
    }

    #[test]
    fn the_history_horizon_is_reported_but_does_not_make_the_answer_incomplete() {
        // Mirrors `HistoryStartsAt`: a fact about the period, not a defect.
        assert!(
            !MaterialIssue::ScheduledPostingUnverifiable {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                date: date!(2026 - 06 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::HistoryStartsAfterSchedule,
            }
            .is_defect()
        );
    }

    #[test]
    fn the_other_unverifiable_reasons_are_defects_because_loading_facts_fixes_them() {
        for reason in [
            UnverifiableReason::AcquisitionDateUnknown,
            UnverifiableReason::OwnershipUnknown,
            UnverifiableReason::EntitlementDateUnknown,
            UnverifiableReason::IncomeKindUnknown,
            UnverifiableReason::PaymentDateUnknown,
        ] {
            assert!(
                MaterialIssue::ScheduledPostingUnverifiable {
                    account: AccountId::new_random(),
                    instrument: InstrumentId::new_random(),
                    date: date!(2026 - 06 - 15),
                    kind: PostingKind::Coupon,
                    reason,
                }
                .is_defect(),
                "{reason:?} can be fixed by loading more facts and is therefore a defect"
            );
        }
    }

    #[test]
    fn a_scheduled_posting_not_received_is_a_defect() {
        assert!(
            MaterialIssue::ScheduledPostingNotReceived {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                date: date!(2026 - 06 - 15),
                kind: PostingKind::Coupon,
            }
            .is_defect()
        );
    }

    #[test]
    fn every_unverifiable_reason_has_a_machine_readable_code() {
        let codes: std::collections::BTreeSet<_> = [
            UnverifiableReason::AcquisitionDateUnknown,
            UnverifiableReason::OwnershipUnknown,
            UnverifiableReason::EntitlementDateUnknown,
            UnverifiableReason::IncomeKindUnknown,
            UnverifiableReason::PaymentDateUnknown,
            UnverifiableReason::HistoryStartsAfterSchedule,
            UnverifiableReason::ScheduleNotTrusted,
        ]
        .into_iter()
        .map(UnverifiableReason::code)
        .collect();

        // All seven variants must remain distinct: each defect
        // requires its own data backfill and cannot be merged into a single code.
        assert_eq!(codes.len(), 7);
    }

    #[test]
    fn an_income_of_unknown_kind_is_reported_before_the_ownership_bound() {
        // An unknown kind breaks the facts side: there is nothing to reconcile with any
        // at the ownership boundary, so this is precisely the reason rather than the absence of
        // the trade date, which is absent here.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let amount = Money::new(PostedMinor::new(50_000), CurrencyCode::Rub);
        let events = vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
            event_with(
                account,
                date!(2026 - 03 - 16),
                3,
                EventKind::Income {
                    instrument: Some(instrument),
                    gross: amount,
                    kind: None,
                },
                vec![Leg::cash(account, amount)],
            ),
        ];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(missing_postings(&report).is_empty());
        assert!(contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::IncomeKindUnknown,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::IncomeKindUnknown,
                ..
            }
        )));
    }

    #[test]
    fn a_bond_with_several_offer_windows_reports_one_miss_once() {
        // `bond_scenario` is called once per scenario, while the history of
        // All scenarios share this: reconciliation within a scenario would duplicate the issue.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = schedule_with_offers(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
            &[date!(2026 - 09 - 15), date!(2026 - 10 - 15)],
        );
        let report = reconciliation_report(
            &[account],
            instrument,
            &journal_with_missing_coupon(account, instrument),
            &schedule,
        );

        assert_eq!(
            report.bond_metrics[0].scenarios.len(),
            3,
            "there must be more than one scenario, otherwise the test proves nothing"
        );
        assert_eq!(missing_postings(&report).len(), 1);
    }

    /// Journal for a healthy bond: two purchases and sale of the early lot,
    /// all past coupons are supported by facts.
    fn journal_with_early_lot_sold(
        account: AccountId,
        instrument: InstrumentId,
        fact_dates: &[Date],
    ) -> Vec<crate::event::Event> {
        // One storage location for the entire journal: otherwise a sale would have removed
        // the security from a depository where it was never deposited, and the number of positions would
        // be three — the test would check duplication rather than the ownership boundary.
        let custody = CustodyId::new_random();
        let mut events = vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase_in_custody(account, instrument, date!(2026 - 01 - 10), custody, 2),
            bond_purchase_in_custody(account, instrument, date!(2026 - 04 - 10), custody, 3),
            bond_sale(
                account,
                instrument,
                date!(2026 - 07 - 10),
                Some(date!(2026 - 07 - 10)),
                custody,
                4,
            ),
        ];
        events.extend(fact_dates.iter().enumerate().map(|(index, day)| {
            coupon_fact(
                account,
                instrument,
                *day,
                5 + u32::try_from(index).expect("fact number"),
            )
        }));
        events
    }

    #[test]
    fn a_coupon_whose_waiting_window_is_still_running_is_not_reported_as_missing() {
        // Money travels through the depository chain for up to three weeks, and all of that
        // during that time the absence of a fact proves nothing. Boundary: +20
        // days — silence, +21 — alert.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = journal_with_missing_coupon(account, instrument);

        let still_pending = reconciliation_report_at(
            &[account],
            instrument,
            &events,
            &schedule,
            date!(2026 - 07 - 05),
        );
        // The flow was built, so reconciliation reached the plan and stayed silent
        // on the merits, not because construction failed.
        assert!(matches!(
            still_pending.bond_metrics[0].scenarios[0]
                .prospective
                .metrics,
            Computed::Value(_)
        ));
        assert!(missing_postings(&still_pending).is_empty());
        assert!(!contains(&still_pending, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));

        let expired = reconciliation_report_at(
            &[account],
            instrument,
            &events,
            &schedule,
            date!(2026 - 07 - 06),
        );
        assert_eq!(
            missing_postings(&expired).len(),
            1,
            "problems: {:?}",
            missing_postings(&expired)
        );
    }

    #[test]
    fn a_payment_whose_waiting_window_is_still_running_is_not_a_reason_to_call_the_report_unverifiable()
     {
        // A payment whose period is still ongoing is excluded from the check
        // entirely: it is neither grounds for an alert, nor a reason for
        // unverifiability. Here its date precedes the first journal event
        // — previously this yielded `HistoryStartsAfterSchedule`.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[date!(2026 - 08 - 20), date!(2026 - 12 - 15)],
            date!(2026 - 12 - 15),
        );
        let events = vec![restored_position(
            account,
            instrument,
            date!(2026 - 08 - 22),
            date!(2021 - 05 - 01),
        )];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(missing_postings(&report).is_empty());
        assert!(!contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));
    }

    #[test]
    fn a_fully_sold_bond_is_still_reconciled_from_the_lot_book() {
        // A security sold down to zero disappears from positions, but its `LotKey` record
        // remains: the March coupon from the ownership period must not be lost
        // together with the current quantity.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let schedule = coupon_schedule(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
        let events = vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase_in_custody(account, instrument, date!(2026 - 01 - 10), custody, 2),
            bond_sale(
                account,
                instrument,
                date!(2026 - 05 - 10),
                Some(date!(2026 - 05 - 10)),
                custody,
                3,
            ),
        ];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(report.bond_metrics.is_empty());
        let issues = missing_postings(&report);
        assert_eq!(issues.len(), 1, "issues: {issues:?}");
        assert!(matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        ));
    }

    #[test]
    fn reconciliation_survives_an_unknown_nominal_when_scenario_cannot_be_built() {
        // Face value is needed by the scenario for monetary amounts, but not reconciliation dates: the failure
        // the scenario must not disable the missing-coupon check.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        schedule.initial_principal = None;
        let events = vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
        ];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(matches!(
            report.bond_metrics[0].scenarios[0].prospective.metrics,
            Computed::NotComputable {
                reason: NotComputable::PrincipalUnknown
            }
        ));
        assert!(missing_postings(&report).iter().any(|issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        )));
    }

    #[test]
    fn an_untrusted_schedule_reports_one_pair_level_refusal() {
        // Neither a gap, nor an absence can be attributed to an incomplete schedule
        // entitlement: the owner must see a separate reason to trust the schedule.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        schedule.completeness = crate::bond::ScheduleCompleteness::Unknown;
        let events = vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
        ];
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        let refusals: Vec<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter(|issue| {
                matches!(
                    issue,
                    MaterialIssue::ScheduledPostingUnverifiable {
                        reason: UnverifiableReason::ScheduleNotTrusted,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(refusals.len(), 1, "issues: {refusals:?}");
        assert!(missing_postings(&report).is_empty());
    }

    #[test]
    fn a_missing_coupon_is_still_named_after_the_earliest_lot_was_sold() {
        // The ownership boundary is the earliest acquisition date ever
        // observed for the pair. Otherwise selling the January lot would raise
        // extended the boundary through April and hid the March omission.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = journal_with_early_lot_sold(account, instrument, &[date!(2026 - 06 - 16)]);
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        let issues = missing_postings(&report);
        assert_eq!(issues.len(), 1, "issues: {issues:?}");
        assert!(matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        ));
    }

    #[test]
    fn a_healthy_history_with_a_sold_early_lot_raises_no_alarm() {
        // The converse of the same boundary: a complete history of received payments
        // coupons produce no alerts.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = journal_with_early_lot_sold(
            account,
            instrument,
            &[date!(2026 - 03 - 16), date!(2026 - 06 - 16)],
        );
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert!(missing_postings(&report).is_empty());
        assert!(!contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));
    }

    #[test]
    fn a_bond_kept_in_two_custodies_reports_one_miss_once() {
        // Position iteration uses `PositionKey` with the storage location,
        // while `LotKey` is reconciled without it: the same problem would otherwise
        // is emitted once per depository.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events =
            journal_for_bond_in_two_custodies(account, instrument, &[date!(2026 - 03 - 16)]);
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert_eq!(
            report.bond_metrics.len(),
            2,
            "there must be two positions, otherwise the test proves nothing"
        );
        let issues = missing_postings(&report);
        assert_eq!(issues.len(), 1, "issues: {issues:?}");
    }

    #[test]
    fn moving_a_bond_between_custodies_raises_no_false_alarm() {
        // There is no separate event kind for transferring a security between
        // There are no transfers between depositories in the model: a transfer is visible only through state —
        // one `LotKey` under two `PositionKey` entries. That is exactly what is being tested:
        // a complete coupon history produces no alerts.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = journal_for_bond_in_two_custodies(
            account,
            instrument,
            &[date!(2026 - 03 - 16), date!(2026 - 06 - 16)],
        );
        let report = reconciliation_report(&[account], instrument, &events, &schedule);

        assert_eq!(report.bond_metrics.len(), 2);
        assert!(missing_postings(&report).is_empty());
        assert!(!contains(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));
    }

    /// One security in one account at two custody locations.
    fn journal_for_bond_in_two_custodies(
        account: AccountId,
        instrument: InstrumentId,
        fact_dates: &[Date],
    ) -> Vec<crate::event::Event> {
        let mut events = vec![
            cash_in(account, date!(2026 - 01 - 05)),
            bond_purchase_in_custody(
                account,
                instrument,
                date!(2026 - 01 - 10),
                CustodyId::new_random(),
                2,
            ),
            bond_purchase_in_custody(
                account,
                instrument,
                date!(2026 - 01 - 11),
                CustodyId::new_random(),
                3,
            ),
        ];
        events.extend(fact_dates.iter().enumerate().map(|(index, day)| {
            coupon_fact(
                account,
                instrument,
                *day,
                4 + u32::try_from(index).expect("fact number"),
            )
        }));
        events
    }

    #[test]
    fn two_accounts_holding_the_same_bond_give_two_distinguishable_issues() {
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = coupon_schedule(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let mut events = journal_with_missing_coupon(first, instrument);
        events.extend(journal_with_missing_coupon(second, instrument));
        let report = reconciliation_report(&[first, second], instrument, &events, &schedule);

        let accounts: std::collections::BTreeSet<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingNotReceived { account, .. } => Some(*account),
                _ => None,
            })
            .collect();
        assert_eq!(accounts, std::collections::BTreeSet::from([first, second]));
    }

    #[test]
    fn the_report_names_the_applied_rule_versions() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let report = report_for(&state, KnowledgeCoordinate::default());

        assert_eq!(
            report.applied_rules.cashflow_projection,
            crate::rules::CashflowProjectionVersion(2)
        );
        assert_eq!(
            report.applied_rules.posting_match,
            crate::rules::PostingMatchVersion(2)
        );
    }

    #[test]
    fn the_posting_match_version_reaches_the_inputs_hash() {
        // The rule version is a report input: changing it must change
        // a fingerprint, otherwise two incomparable answers would appear
        // by reproducing a single one.
        let contour = ContourDefinition::new(
            ContourId(uuid::Uuid::nil()),
            ContourVersion(1),
            Vec::<AccountId>::new(),
        );
        let fx_source = FxSource::OwnerSupplied;
        let offer_book = OfferBook::default();
        let inputs = |posting_match| SelectedInputs {
            coordinate: SelectedCoordinate {
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
                source_priority_version: 1,
                valuation_policy_version: 1,
            },
            as_of: date!(2026 - 08 - 26),
            contour: &contour,
            report_currency: CurrencyCode::Rub,
            flows: Vec::new(),
            cash: Vec::new(),
            positions: Vec::new(),
            fx: SelectedFx {
                source: &fx_source,
                rates: Vec::new(),
            },
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            accrued_interest_rule: accrued_interest_rule().0,
            offer_book: &offer_book,
            cashflow_projection: cashflow_projection_rule().0,
            expense_policy: expense_policy_rule().0,
            posting_match,
        };

        assert_ne!(
            hash_selected(&inputs(crate::rules::PostingMatchVersion(1))),
            hash_selected(&inputs(crate::rules::PostingMatchVersion(2)))
        );
    }
    #[test]
    fn five_unverifiable_postings_with_one_reason_become_one_issue() {
        // A single source-level cause is fixed by one action, so
        // five targeted lines must report one issue with the full
        // with the quantity and period, rather than repeating the same instruction five times.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let issues = (0..5)
            .map(|offset| MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 01 - 15)
                    .saturating_add(time::Duration::days(i64::from(offset) * 30)),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::EntitlementDateUnknown,
            })
            .collect();

        let collapsed = collapse_scheduled_posting_unverifiable(issues);

        assert!(matches!(
            collapsed.as_slice(),
            [MaterialIssue::ScheduledPostingsUnverifiable {
                count: 5,
                first_date,
                last_date,
                ..
            }] if *first_date == date!(2026 - 01 - 15)
                && *last_date == date!(2026 - 05 - 15)
        ));
    }

    #[test]
    fn one_unverifiable_posting_stays_an_addressed_issue() {
        // Deduplication is needed only for duplicates: a single payment must preserve
        // the old address-specific form, to avoid creating false group semantics.
        let issue = MaterialIssue::ScheduledPostingUnverifiable {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
            date: date!(2026 - 01 - 15),
            kind: PostingKind::Coupon,
            reason: UnverifiableReason::OwnershipUnknown,
        };

        let collapsed = collapse_scheduled_posting_unverifiable(vec![issue.clone()]);

        assert_eq!(collapsed, vec![issue]);
    }

    #[test]
    fn collapsing_unverifiable_postings_keeps_an_interleaved_missing_posting() {
        // Flagging a specific missing payment is fixed by finding that payment,
        // so it must survive global deduplication of adjacent reasons.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let issues = vec![
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 01 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::PaymentDateUnknown,
            },
            MaterialIssue::ScheduledPostingNotReceived {
                account,
                instrument,
                date: date!(2026 - 03 - 15),
                kind: PostingKind::Coupon,
            },
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 05 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::PaymentDateUnknown,
            },
        ];

        let collapsed = collapse_scheduled_posting_unverifiable(issues);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            collapsed[0],
            MaterialIssue::ScheduledPostingsUnverifiable {
                count: 2,
                first_date,
                last_date,
                ..
            } if first_date == date!(2026 - 01 - 15)
                && last_date == date!(2026 - 05 - 15)
        ));
        assert!(matches!(
            collapsed[1],
            MaterialIssue::ScheduledPostingNotReceived {
                date,
                kind: PostingKind::Coupon,
                ..
            } if date == date!(2026 - 03 - 15)
        ));
    }

    #[test]
    fn different_unverifiable_reasons_do_not_merge() {
        // Different reasons require different corrective actions, so
        // the same pair and payment type do not justify losing the distinction.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let issues = vec![
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 01 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::OwnershipUnknown,
            },
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 05 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::PaymentDateUnknown,
            },
        ];

        let collapsed = collapse_scheduled_posting_unverifiable(issues);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            collapsed[0],
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            }
        ));
        assert!(matches!(
            collapsed[1],
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::PaymentDateUnknown,
                ..
            }
        ));
    }
}
