//! Grounds for automatic status promotion (§10.3).
//!
//! Eight grounds from the spec plus a ninth — the owner-stated balance
//! (§10.4). None of the first eight requires human involvement.
//!
//! **The level is determined by channel independence, not by the type
//! of ground.** This is the module's main rule: the ground only sets
//! the ceiling, while the actual level is obtained by lowering the ceiling to
//! `internal` if independence has not been proven.

use std::collections::BTreeSet;

use super::{ConfidenceLevel, Dimension};
use crate::event::provenance::{ParserVersion, RawHash};
use crate::ids::SourceId;

/// The channel through which the data was obtained.
///
/// A document is the hash of the file from which the data was parsed. An API response
/// has no document: it is a stream, not a file, and `None` here specifically means
/// “there was no file”, not “the hash was not computed”.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceChannel {
    pub source: SourceId,
    pub parser_version: ParserVersion,
    pub document: Option<RawHash>,
}

impl SourceChannel {
    /// Whether this channel is independent of another (§10.3).
    ///
    /// The spec's criterion: confirming data must not pass through
    /// **the same parsing code** and **the same document**. Both conditions
    /// are required, so this is a conjunction:
    ///
    /// - same parser, different document — the next report from the same
    ///   broker: continuity, but not independence;
    /// - different parser, same document — reparsing with a new
    ///   version: corrected parsing, but the source is the same.
    ///
    /// The source identifier is **not part of** the criterion: two sources
    /// may share parsing code, in which case a common error will corrupt both
    /// sides, no matter how many different identifiers they have.
    #[must_use]
    pub fn is_independent_of(&self, other: &Self) -> bool {
        self.parser_version != other.parser_version && self.document != other.document
    }
}

/// Ground for status promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ground {
    /// 1. The opening balance of the next report matched the computed
    ///    balance of the previous period.
    OpeningMatchesPriorClosing,
    /// 2. The closing balance of one report matched the opening balance of the next.
    ContinuityBetweenStatements,
    /// 3. The broker API matched the parsed report.
    BrokerApiAgreesWithStatement,
    /// 4. The depository report confirmed the quantities and custody location.
    DepositaryReportConfirms,
    /// 5. Separate control sections of the same document reconciled
    ///    simultaneously.
    SeparateSectionsAgree,
    /// 6. The actual payment confirmed the schedule from the preceding period.
    PayoutConfirmsSchedule,
    /// 7. Quantities after the corporate action matched the issue
    ///    parameters.
    CorporateActionMatchesIssueTerms,
    /// 8. The tax agent certificate confirmed the aggregates.
    TaxAgentCertificate,
    /// Owner-stated balance (§10.4).
    ///
    /// Not one of the eight automatic grounds: it requires human
    /// involvement. The level is deliberately capped at `internal` — the owner may have
    /// read the same figure in the same report that we parsed,
    /// and independence has not been proven here, while §10.3 specifically requires
    /// proof, not a type of ground.
    OwnerStatedBalance,
}

impl Ground {
    /// The maximum level that the ground can provide in principle.
    ///
    /// Grounds 1, 2, and 5 are capped at `internal` **by design**: they
    /// compare data that passed through the same parser. This restriction
    /// cannot be removed in favor of relying on the independence check:
    /// grounds 1 and 2 use different documents, and the check would let them pass
    /// if the parser version happened to differ as well.
    #[must_use]
    pub const fn ceiling(self) -> ConfidenceLevel {
        match self {
            Self::OpeningMatchesPriorClosing
            | Self::ContinuityBetweenStatements
            | Self::SeparateSectionsAgree
            | Self::OwnerStatedBalance => ConfidenceLevel::AcceptedInternal,
            Self::BrokerApiAgreesWithStatement
            | Self::DepositaryReportConfirms
            | Self::PayoutConfirmsSchedule
            | Self::CorporateActionMatchesIssueTerms
            | Self::TaxAgentCertificate => ConfidenceLevel::AcceptedIndependent,
        }
    }

    /// Which dimensions the ground may promote.
    ///
    /// This restriction matters: a depository says nothing about money,
    /// a tax agent certificate covers only aggregates, and an owner-stated
    /// balance covers only a snapshot (§10.4).
    #[must_use]
    pub fn dimensions(self) -> BTreeSet<Dimension> {
        let list: &[Dimension] = match self {
            Self::OpeningMatchesPriorClosing
            | Self::ContinuityBetweenStatements
            | Self::BrokerApiAgreesWithStatement
            | Self::OwnerStatedBalance => &[Dimension::Cash, Dimension::Positions],
            Self::DepositaryReportConfirms | Self::CorporateActionMatchesIssueTerms => {
                &[Dimension::Positions]
            }
            Self::SeparateSectionsAgree => &[
                Dimension::Cash,
                Dimension::Positions,
                Dimension::Income,
                Dimension::TaxBasis,
            ],
            Self::PayoutConfirmsSchedule => &[Dimension::Income],
            Self::TaxAgentCertificate => &[Dimension::Income, Dimension::TaxBasis],
        };
        list.iter().copied().collect()
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::OpeningMatchesPriorClosing => "opening_matches_prior_closing",
            Self::ContinuityBetweenStatements => "continuity_between_statements",
            Self::BrokerApiAgreesWithStatement => "broker_api_agrees_with_statement",
            Self::DepositaryReportConfirms => "depositary_report_confirms",
            Self::SeparateSectionsAgree => "separate_sections_agree",
            Self::PayoutConfirmsSchedule => "payout_confirms_schedule",
            Self::CorporateActionMatchesIssueTerms => "corporate_action_matches_issue_terms",
            Self::TaxAgentCertificate => "tax_agent_certificate",
            Self::OwnerStatedBalance => "owner_stated_balance",
        }
    }

    /// All grounds in a single list — for iteration and completeness checks.
    #[must_use]
    pub const fn all() -> [Self; 9] {
        [
            Self::OpeningMatchesPriorClosing,
            Self::ContinuityBetweenStatements,
            Self::BrokerApiAgreesWithStatement,
            Self::DepositaryReportConfirms,
            Self::SeparateSectionsAgree,
            Self::PayoutConfirmsSchedule,
            Self::CorporateActionMatchesIssueTerms,
            Self::TaxAgentCertificate,
            Self::OwnerStatedBalance,
        ]
    }
}

/// A successful confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    ground: Ground,
    confirming: SourceChannel,
    confirmed: SourceChannel,
    dimensions: BTreeSet<Dimension>,
}

impl Evidence {
    /// Constructs a ground from a successful match.
    ///
    /// Returns `None` when the ground confirms none of the
    /// matching dimensions: a ground that confirms nothing
    /// is not an empty ground but the absence of a ground, and adding
    /// one to the evidence list would create the appearance of verification.
    ///
    /// The logic does not live in `new`: `cargo-mutants` skips this name.
    #[must_use]
    pub fn from_match(
        ground: Ground,
        confirming: SourceChannel,
        confirmed: SourceChannel,
        matched_dimensions: BTreeSet<Dimension>,
    ) -> Option<Self> {
        let dimensions: BTreeSet<Dimension> = ground
            .dimensions()
            .intersection(&matched_dimensions)
            .copied()
            .collect();
        (!dimensions.is_empty()).then_some(Self {
            ground,
            confirming,
            confirmed,
            dimensions,
        })
    }

    /// The level granted by this ground.
    ///
    /// The ground's ceiling is lowered to `internal` if channel independence
    /// has not been proven. There is no reverse path: a ground capped at
    /// `internal` by design is always capped there — the channel check
    /// does not promote it.
    #[must_use]
    pub fn level(&self) -> ConfidenceLevel {
        let ceiling = self.ground.ceiling();
        if ceiling == ConfidenceLevel::AcceptedIndependent
            && !self.confirming.is_independent_of(&self.confirmed)
        {
            return ConfidenceLevel::AcceptedInternal;
        }
        ceiling
    }

    #[must_use]
    pub fn dimensions(&self) -> BTreeSet<Dimension> {
        self.dimensions.clone()
    }

    #[must_use]
    pub const fn ground(&self) -> Ground {
        self.ground
    }

    #[must_use]
    pub const fn confirming(&self) -> &SourceChannel {
        &self.confirming
    }

    #[must_use]
    pub const fn confirmed(&self) -> &SourceChannel {
        &self.confirmed
    }
}
