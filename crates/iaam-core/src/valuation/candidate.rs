//! Shared valuation candidate and sampling port (E3.3, design section 3).
//!
//! The two price channels — an exchange observation and a statement by the owner or
//! a document — arrive here as the same type. Executability in the candidate
//! belongs to the source; everything inferred by the valuation policy lives in
//! [`SelectedPrice`] and is excluded from the candidate by construction.

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

use crate::ids::{InstrumentId, SourceId};
use crate::money::CurrencyCode;
use crate::numeric::decimal::Dec;

use super::PriceQuality;
/// Trading mode that is part of the identity of a market observation.
///
/// The session number distinguishes regular trading from the evening session: the instrument
/// code and board alone are insufficient to associate a price with an accrued-interest observation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Venue {
    pub board: String,
    pub session: i64,
}

/// MOEX market-price columns distinguished by the valuation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PriceKind {
    Close,
    LegalClose,
    WeightedAverage,
    MarketPrice2,
    MarketPrice3,
    AdmittedQuote,
}

impl PriceKind {
    /// Canonical column name in the wire format.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::LegalClose => "legal_close",
            Self::WeightedAverage => "weighted_average",
            Self::MarketPrice2 => "market_price_2",
            Self::MarketPrice3 => "market_price_3",
            Self::AdmittedQuote => "admitted_quote",
        }
    }
}

/// Where the candidate came from. Not inferred: the channel is known at the point of construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceOrigin {
    /// An observation from a market source.
    Market { venue: Venue, kind: PriceKind },
    /// A price parsed from a report or another document.
    ReportParsed { source: SourceId },
    /// A price asserted by the owner.
    OwnerAsserted,
}

/// Executability as asserted by the source.
///
/// `Unknown` is required: when entering a price for an illiquid asset, the owner asserts
/// neither that the position can be exited at that price nor that it is a closing price.
/// Without this variant, the manual channel would be forced to lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceExecutability {
    /// The source asserts a price at which the position can be exited.
    Executable,
    /// The source asserts the closing price of the previous trading session.
    IndicativePreviousClose,
    /// The source makes no assertion about the price's executability.
    Unknown,
}

/// The unit in which the source quoted the price (§10.2).
///
/// The third axis alongside completeness and executability (ADR-0002), and, like them,
/// **an attribute of the source observation**, not an inference by the policy: the basis
/// is determined by the market and trading mode from which the adapter took the row.
/// Inferring it later by rule would be the same conflation of axes that
/// decision 0002 prohibits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QuotationBasis {
    /// Money per security. The number's currency is the observation currency.
    MoneyPerUnit,
    /// Percentage of the outstanding face value. The number itself is **dimensionless**:
    /// the monetary currency comes from the face-value currency, not from here.
    PercentOfRemainingFace,
    /// The source did not establish the basis. Reject during valuation rather than guess.
    #[default]
    Unknown,
}

impl QuotationBasis {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MoneyPerUnit => "money_per_unit",
            Self::PercentOfRemainingFace => "percent_of_remaining_face",
            Self::Unknown => "unknown",
        }
    }

    /// Parses a code from storage. `None`, not `Unknown`: an unknown code indicates
    /// a corrupted row, and passing it off as an unsubstantiated observation would
    /// hide the corruption.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::MoneyPerUnit,
            Self::PercentOfRemainingFace,
            Self::Unknown,
        ]
        .into_iter()
        .find(|basis| basis.code() == code)
    }
}

/// Selection method — why the observation date differed from the valuation date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSelection {
    /// The observation is from exactly the valuation date.
    AsObserved,
    /// The observation was carried forward from an earlier date.
    CarriedForward { observed_on: Date, days: u16 },
    /// The value was inherited from a legacy rule and is not revalued.
    LegacyDerived { quality: PriceQuality },
}

/// Freshness is a separate axis: a price can be both carried forward and stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceFreshness {
    /// The price age does not exceed the freshness threshold.
    Fresh,
    /// The price age exceeds the normal threshold, but the price is still selected.
    Stale { days: u16 },
}

/// Why the position remained unpriced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncoveredReason {
    /// There are no observations for the instrument.
    NoObservation,
    /// All observations exceed the maximum age.
    TooOld,
    /// The venue cannot be determined unambiguously.
    AmbiguousVenue,
    /// Multiple candidates remained after filtering.
    AmbiguousCandidate,
}

/// Plan-compatible name for the reason coverage is missing.
pub type Uncovered = UncoveredReason;

/// Shared valuation candidate.
///
/// Executability belongs to the source. There is intentionally no
/// [`PriceSelection`] here: carry-forward and staleness are policy outputs,
/// not observation attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceCandidate {
    pub instrument: InstrumentId,
    pub price: Dec,
    pub currency: CurrencyCode,
    /// Price unit. `#[serde(default)]` is unnecessary: `PriceCandidate`
    /// is not serialized; it is constructed for every calculation.
    pub basis: QuotationBasis,
    /// Evidence from which the basis was inferred. Stored alongside it rather than
    /// reconstructed from the basis: without it, the record cannot be substantiated
    /// when parsing the audit trail (§10.2).
    pub basis_evidence: String,
    /// The evidence contradicts the recorded basis. The effective basis
    /// in such a candidate is already `Unknown`, but the rejection reason must reach
    /// the position valuation separately from the lack of evidence.
    pub basis_evidence_contradicts: bool,
    pub trade_date: Date,
    /// The time when the source learned of the observation. For journal-based
    /// valuation, this time is not recorded and remains `None`.
    pub observed_at: Option<OffsetDateTime>,
    pub origin: PriceOrigin,
    pub executability: SourceExecutability,
}

/// Basis for the policy decision: all versions and thresholds capable of
/// changing its interpretation (§6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceProvenance {
    pub price_kind: Option<String>,
    pub origin: PriceOrigin,
    pub venue: Option<String>,
    /// The unit in which the source quoted the price. Without it, the audit trail
    /// does not explain where the position's monetary value came from.
    pub quotation_basis: QuotationBasis,
    /// Evidence from which the basis was inferred.
    pub basis_evidence: String,
    /// The time when the source learned of the observation, if recorded by the log.
    pub observed_at: Option<OffsetDateTime>,
    pub valuation_policy_version: u32,
    pub source_priority_version: u32,
    pub carry_forward_limit: u16,
    pub price_max_age: u16,
}

/// The selected candidate with independent policy determinations and rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPrice {
    pub candidate: PriceCandidate,
    pub selection: PriceSelection,
    pub freshness: PriceFreshness,
    pub provenance: PriceProvenance,
}

/// A request to select a price for a valuation date and knowledge coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceQuery {
    pub instrument: InstrumentId,
    pub as_of: Date,
    pub knowledge_as_of: OffsetDateTime,
}

/// The result of parsing legacy price quality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyValuationOutcome {
    /// A legacy event can be represented as a price source.
    Candidate(PriceCandidate),
    /// A legacy event contains a precomputed policy result.
    LegacyDerived(PriceQuality),
}

impl LegacyValuationOutcome {
    /// Returns the candidate if the legacy quality permits reevaluation.
    #[must_use]
    pub const fn candidate(&self) -> Option<&PriceCandidate> {
        match self {
            Self::Candidate(candidate) => Some(candidate),
            Self::LegacyDerived(_) => None,
        }
    }

    /// Returns the inherited quality if it is terminal.
    #[must_use]
    pub const fn legacy(&self) -> Option<PriceQuality> {
        match self {
            Self::Candidate(_) => None,
            Self::LegacyDerived(quality) => Some(*quality),
        }
    }

    /// Extracts the candidate, if present.
    #[must_use]
    pub fn into_candidate(self) -> Option<PriceCandidate> {
        match self {
            Self::Candidate(candidate) => Some(candidate),
            Self::LegacyDerived(_) => None,
        }
    }
}

/// Parses legacy price quality into provenance and executability.
///
/// `Executable`, `PreviousClose`, and `OwnerEstimate` become
/// candidates again. `CarriedForward` and `Stale` do not become candidates:
/// the legacy event stores the date to which the price was assigned, but not the original
/// observation date, so re-selection would launder an old determination as fresh.
#[must_use]
pub fn candidate_from_legacy_valuation(
    quality: PriceQuality,
    mut candidate: PriceCandidate,
) -> LegacyValuationOutcome {
    match quality {
        PriceQuality::Executable => {
            candidate.executability = SourceExecutability::Executable;
            LegacyValuationOutcome::Candidate(candidate)
        }
        PriceQuality::PreviousClose => {
            candidate.executability = SourceExecutability::IndicativePreviousClose;
            LegacyValuationOutcome::Candidate(candidate)
        }
        PriceQuality::OwnerEstimate => {
            candidate.origin = PriceOrigin::OwnerAsserted;
            candidate.executability = SourceExecutability::Unknown;
            LegacyValuationOutcome::Candidate(candidate)
        }
        PriceQuality::CarriedForward | PriceQuality::Stale => {
            LegacyValuationOutcome::LegacyDerived(quality)
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    use super::*;

    #[test]
    fn an_undecided_quotation_basis_is_unknown_not_money_per_unit() {
        // A row written before the rationale existed cannot be proven.
        // `MoneyPerUnit` would declare it proven by default (§4.9).
        assert_eq!(QuotationBasis::default(), QuotationBasis::Unknown);
    }

    #[test]
    fn every_quotation_basis_names_itself() {
        assert_eq!(QuotationBasis::MoneyPerUnit.code(), "money_per_unit");
        assert_eq!(
            QuotationBasis::PercentOfRemainingFace.code(),
            "percent_of_remaining_face"
        );
        assert_eq!(QuotationBasis::Unknown.code(), "unknown");
    }

    #[test]
    fn a_quotation_basis_survives_a_round_trip_through_its_code() {
        for basis in [
            QuotationBasis::MoneyPerUnit,
            QuotationBasis::PercentOfRemainingFace,
            QuotationBasis::Unknown,
        ] {
            assert_eq!(QuotationBasis::from_code(basis.code()), Some(basis));
        }
    }

    #[test]
    fn an_unrecognised_code_does_not_fall_back_to_a_basis() {
        // An unknown code from the database is corruption, not `Unknown`: `Unknown`
        // means «the source did not prove it», not «the row could not be read».
        assert_eq!(QuotationBasis::from_code("percent"), None);
    }

    fn price() -> PriceCandidate {
        PriceCandidate {
            instrument: InstrumentId::new_random(),
            price: Dec::new(Decimal::from(281)),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::Unknown,
            basis_evidence: String::new(),
            basis_evidence_contradicts: false,
            trade_date: date!(2026 - 08 - 03),
            observed_at: Some(datetime!(2026 - 08 - 03 18:00 UTC)),
            origin: PriceOrigin::ReportParsed {
                source: SourceId::new_random(),
            },
            executability: SourceExecutability::Executable,
        }
    }

    #[test]
    fn a_legacy_owner_estimate_becomes_an_owner_asserted_candidate() {
        let outcome = candidate_from_legacy_valuation(PriceQuality::OwnerEstimate, price());
        let candidate = outcome.candidate().expect("owner estimate is a candidate");
        assert_eq!(candidate.origin, PriceOrigin::OwnerAsserted);
        assert_eq!(candidate.executability, SourceExecutability::Unknown);
    }

    #[test]
    fn a_legacy_carried_forward_price_is_never_re_derived() {
        let outcome = candidate_from_legacy_valuation(PriceQuality::CarriedForward, price());
        assert!(outcome.candidate().is_none());
        assert_eq!(
            outcome.legacy(),
            Some(PriceQuality::CarriedForward),
            "original observation date was lost: reevaluation would pass off a carry-forward as an observation"
        );
    }

    #[test]
    fn legacy_stale_price_is_never_re_derived() {
        let outcome = candidate_from_legacy_valuation(PriceQuality::Stale, price());
        assert!(outcome.candidate().is_none());
        assert_eq!(outcome.legacy(), Some(PriceQuality::Stale));
    }

    #[test]
    fn carried_forward_and_stale_are_independent_facts() {
        let selected = SelectedPrice {
            candidate: price(),
            selection: PriceSelection::CarriedForward {
                observed_on: date!(2026 - 07 - 01),
                days: 40,
            },
            freshness: PriceFreshness::Stale { days: 40 },
            provenance: PriceProvenance {
                price_kind: None,
                origin: PriceOrigin::ReportParsed {
                    source: SourceId::new_random(),
                },
                venue: None,
                quotation_basis: QuotationBasis::Unknown,
                basis_evidence: String::new(),
                observed_at: Some(datetime!(2026 - 07 - 01 18:00 UTC)),
                valuation_policy_version: 1,
                source_priority_version: 1,
                carry_forward_limit: 10,
                price_max_age: 30,
            },
        };
        assert!(matches!(
            selected.selection,
            PriceSelection::CarriedForward { .. }
        ));
        assert!(matches!(selected.freshness, PriceFreshness::Stale { .. }));
    }

    #[test]
    fn source_quality_maps_to_source_executability() {
        let executable = candidate_from_legacy_valuation(PriceQuality::Executable, price());
        assert_eq!(
            executable
                .candidate()
                .map(|candidate| candidate.executability),
            Some(SourceExecutability::Executable)
        );

        let previous_close = candidate_from_legacy_valuation(PriceQuality::PreviousClose, price());
        assert_eq!(
            previous_close
                .candidate()
                .map(|candidate| candidate.executability),
            Some(SourceExecutability::IndicativePreviousClose)
        );
    }

    #[test]
    fn into_candidate_extracts_only_revaluable_candidates() {
        let candidate = price();
        assert_eq!(
            LegacyValuationOutcome::Candidate(candidate.clone()).into_candidate(),
            Some(candidate)
        );
        assert_eq!(
            LegacyValuationOutcome::LegacyDerived(PriceQuality::Stale).into_candidate(),
            None
        );
    }
    #[test]
    fn price_query_keeps_evaluation_and_knowledge_coordinates() {
        let query = PriceQuery {
            instrument: InstrumentId::new_random(),
            as_of: date!(2026 - 08 - 26),
            knowledge_as_of: datetime!(2026 - 08 - 26 12:00 UTC),
        };
        assert_eq!(query.as_of, date!(2026 - 08 - 26));
        assert_eq!(query.knowledge_as_of, datetime!(2026 - 08 - 26 12:00 UTC));
    }
}
