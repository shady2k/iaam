//! Reconciliation: account completeness status over an interval by dimension (§10.3).
//!
//! **The status is not assigned to a transaction.** A transaction is either recorded or
//! not; calling it “confirmed” is meaningless—the confirmation applies to
//! interval completeness: that all monetary activity for March has been accounted for, with nothing
//! extraneous. Therefore, the unit of status is an interval×dimension pair,
//! not an event, and an event has no “confidence level” field.

pub mod check;
pub mod claim;
pub mod evidence;
pub mod observed;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::Date;

use crate::event::correction::resolve;
use crate::event::{Event, kind::EventKind};
use crate::ids::AccountId;
use check::{ClaimOutcome, check_claim};
use claim::{AssertionPeriod, BalancePoint, ControlClaim};
pub use evidence::{Evidence, Ground, IdentityScope, SourceChannel};
use observed::{ObserveError, observe};

/// The dimension whose completeness is being asserted (§10.3).
///
/// This separation is mandatory: a confirmed balance covers monetary amounts and
/// quantities, but **does not confirm** tax value or
/// income classification. Using one dimension for everything would turn
/// “the balance reconciled” into “the taxes were calculated correctly”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Dimension {
    Cash,
    Positions,
    TaxBasis,
    Income,
}

impl Dimension {
    /// Machine-readable code for the API (§13).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Cash => "cash",
            Self::Positions => "positions",
            Self::TaxBasis => "tax_basis",
            Self::Income => "income",
        }
    }

    /// All dimensions in one list.
    ///
    /// Dimension iteration is written through this list rather than as a literal at the
    /// call site: a literal with a missing variant compiles, and
    /// the omitted dimension silently receives no status.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Cash, Self::Positions, Self::TaxBasis, Self::Income]
    }
}

/// Confidence level of an assertion (§10.3).
///
/// The order matters: comparison is used to raise the status.
/// There are three levels rather than two because transactions and control balances
/// are extracted by the same parser from the same document: a shared parsing error
/// will distort both sides of the check identically, and reconciliation will not detect it.
/// The middle level exists specifically for this case and calls it
/// exactly what it is—“reconciled within a single source”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ConfidenceLevel {
    Provisional,
    AcceptedInternal,
    AcceptedIndependent,
}

impl ConfidenceLevel {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::AcceptedInternal => "accepted_internal",
            Self::AcceptedIndependent => "accepted_independent",
        }
    }
}

/// Dimension status over an interval (§10.3).
///
/// The four values from the spec. `Discrepant` is not a level but an absorbing
/// state: a mismatched figure does not stop being mismatched merely because
/// another figure next to it matched.
///
/// **The variant order defines status strength** and is used through
/// `Ord`: `max` raises the status, while `min` takes the worst. Comparison is deliberately
/// delegated to the derived `Ord`—a hand-written `>` creates a branch
/// where replacing it with `>=` changes nothing (equal statuses
/// are identical), making that mutant impossible to kill with a test.
///
/// A discrepancy ranks **below** the absence of confirmation: “did not reconcile” is
/// a detected problem, while “not checked yet” is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DimensionStatus {
    Discrepant,
    Provisional,
    AcceptedInternal,
    AcceptedIndependent,
}

impl DimensionStatus {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::AcceptedInternal => "accepted_internal",
            Self::AcceptedIndependent => "accepted_independent",
            Self::Discrepant => "discrepant",
        }
    }

    const fn from_level(level: ConfidenceLevel) -> Self {
        match level {
            ConfidenceLevel::Provisional => Self::Provisional,
            ConfidenceLevel::AcceptedInternal => Self::AcceptedInternal,
            ConfidenceLevel::AcceptedIndependent => Self::AcceptedIndependent,
        }
    }
}

/// One checked assertion together with its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimCheck {
    pub claim: ControlClaim,
    pub outcome: ClaimOutcome,
}

/// Assertion of account completeness over an interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationStatus {
    account: AccountId,
    period: AssertionPeriod,
    dimensions: BTreeMap<Dimension, DimensionStatus>,
    evidence: Vec<Evidence>,
    outcomes: Vec<ClaimCheck>,
}

impl ReconciliationStatus {
    #[must_use]
    pub const fn account(&self) -> AccountId {
        self.account
    }

    #[must_use]
    pub const fn period(&self) -> AssertionPeriod {
        self.period
    }

    /// Dimension status.
    ///
    /// The absence of a record means `Provisional`: nothing is known about a dimension
    /// for which no assertion has been made.
    #[must_use]
    pub fn dimension(&self, dimension: Dimension) -> DimensionStatus {
        self.dimensions
            .get(&dimension)
            .copied()
            .unwrap_or(DimensionStatus::Provisional)
    }

    #[must_use]
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    #[must_use]
    pub fn outcomes(&self) -> &[ClaimCheck] {
        &self.outcomes
    }
}

/// A group of assertions from one document about one account over one interval.
///
/// Grouping uses a linear search rather than a map: channels have no meaningful
/// ordering, and an owner has only a handful of documents. A map would require
/// an order for order’s sake.
#[derive(Debug, Clone)]
struct StatementGroup {
    account: AccountId,
    period: AssertionPeriod,
    channel: SourceChannel,
    claims: Vec<ControlClaim>,
}

/// Status registry: a pure function of the journal (§3.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconciliationLedger {
    statuses: Vec<ReconciliationStatus>,
}

impl ReconciliationLedger {
    /// Build the registry from the journal without scope exceptions.
    ///
    /// This logic is deliberately kept out of a constructor named `new` (§15.7).
    pub fn build(events: &[Event]) -> Result<Self, ObserveError> {
        Self::build_with(events, &crate::perimeter::PerimeterExceptions::default())
    }

    /// Build the registry with scope exceptions (§11).
    ///
    /// A discrepancy covered by an exception becomes `Excepted`:
    /// the system knows why the figures do not reconcile and does not send
    /// the owner to fix something it does not support. Such an
    /// outcome is not confirmation—“we know the reason” is not the same as “it reconciled”.
    pub fn build_with(
        events: &[Event],
        exceptions: &crate::perimeter::PerimeterExceptions,
    ) -> Result<Self, ObserveError> {
        let effective_events = resolve(events)?;
        let groups = collect_groups(events);
        let gaps = collect_coverage_gaps(events);
        let tainted: Vec<BTreeSet<Dimension>> = groups
            .iter()
            .map(|group| tainted_dimensions(group, &gaps))
            .collect();

        // Step 1: reconcile each group against its projection.
        let mut checked: Vec<Vec<ClaimCheck>> = Vec::with_capacity(groups.len());
        for group in &groups {
            let observed = observe(&effective_events, group.account, group.period)?;
            checked.push(
                group
                    .claims
                    .iter()
                    .map(|claim| ClaimCheck {
                        claim: *claim,
                        outcome: apply_exceptions(
                            check_claim(claim, &observed),
                            group.account,
                            claim.dimension(),
                            exceptions,
                        ),
                    })
                    .collect(),
            );
        }

        // Step 2: evidence that the journal can generate itself.
        let mut evidence: Vec<(AccountId, AssertionPeriod, Evidence)> = Vec::new();
        for (index, outcomes) in checked.iter().enumerate() {
            let group = &groups[index];
            if let Some(item) = ground_five(group, outcomes, &tainted[index]) {
                evidence.push((group.account, group.period, item));
            }
            if let Some((period, item)) = ground_one(index, outcomes, &groups, &tainted) {
                evidence.push((group.account, period, item));
            }
        }
        evidence.extend(ground_two(&groups, &tainted));
        evidence.extend(ground_three(&groups, &checked, &tainted));

        // Step 3: statuses.
        let mut statuses: Vec<ReconciliationStatus> = Vec::new();
        for (index, outcomes) in checked.into_iter().enumerate() {
            merge_status(
                &mut statuses,
                build_status(&groups[index], outcomes, &evidence),
            );
        }
        Ok(Self { statuses })
    }

    /// Add evidence that the journal cannot yet generate:
    /// a depository statement, issue parameters, a tax-agent certificate,
    /// and payment-schedule confirmation (E3, E5, E7).
    #[must_use]
    pub fn with_external_evidence(
        mut self,
        items: Vec<(AccountId, AssertionPeriod, Evidence)>,
    ) -> Self {
        for (account, period, item) in items {
            let level = DimensionStatus::from_level(item.level());
            let dimensions = item.dimensions();
            if let Some(status) = self
                .statuses
                .iter_mut()
                .find(|status| status.account == account && status.period == period)
            {
                raise(&mut status.dimensions, &dimensions, level);
                status.evidence.push(item);
            } else {
                let mut map = BTreeMap::new();
                raise(&mut map, &dimensions, level);
                self.statuses.push(ReconciliationStatus {
                    account,
                    period,
                    dimensions: map,
                    evidence: vec![item],
                    outcomes: Vec::new(),
                });
            }
        }
        self
    }

    pub fn statuses(&self) -> impl Iterator<Item = &ReconciliationStatus> {
        self.statuses.iter()
    }

    /// Dimension status on a date.
    ///
    /// Take the **worst** status among intervals covering the date: two
    /// assertions about the same day, one of which did not reconcile, produce
    /// a discrepancy. Taking the best would allow an extra document
    /// to conceal the problem.
    #[must_use]
    pub fn status_for(
        &self,
        account: AccountId,
        date: Date,
        dimension: Dimension,
    ) -> DimensionStatus {
        let mut result: Option<DimensionStatus> = None;
        for status in &self.statuses {
            if status.account != account || !status.period.contains(date) {
                continue;
            }
            let candidate = status.dimension(dimension);
            // Worst among intervals covering the date: two assertions about the same day,
            // one of which did not reconcile, produce a discrepancy.
            result = Some(result.map_or(candidate, |current| current.min(candidate)));
        }
        result.unwrap_or(DimensionStatus::Provisional)
    }
}

/// Replace a discrepancy with a scope exception (§11).
///
/// Replace **only** a discrepancy: an exception does not explain
/// incomparability, and a match needs no explanation.
fn apply_exceptions(
    outcome: ClaimOutcome,
    account: AccountId,
    dimension: Dimension,
    exceptions: &crate::perimeter::PerimeterExceptions,
) -> ClaimOutcome {
    match (outcome, exceptions.covers(account, dimension)) {
        (ClaimOutcome::Discrepant(_), Some(exception)) => ClaimOutcome::Excepted { exception },
        (outcome, _) => outcome,
    }
}

fn collect_groups(events: &[Event]) -> Vec<StatementGroup> {
    let mut groups: Vec<StatementGroup> = Vec::new();
    for event in events {
        let EventKind::ControlAssertion { period, claim } = event.kind else {
            continue;
        };
        let channel = SourceChannel {
            source: event.provenance.source(),
            parser_version: event.provenance.parser_version().clone(),
            document: Some(event.provenance.raw_hash().clone()),
        };
        if let Some(group) = groups.iter_mut().find(|group| {
            group.account == event.account && group.period == period && group.channel == channel
        }) {
            group.claims.push(claim);
        } else {
            groups.push(StatementGroup {
                account: event.account,
                period,
                channel,
                claims: vec![claim],
            });
        }
    }
    groups
}

#[derive(Debug, Clone)]
struct CoverageGap {
    account: AccountId,
    period: AssertionPeriod,
    source: crate::ids::SourceId,
    parser_version: crate::event::provenance::ParserVersion,
    dimensions: BTreeSet<Dimension>,
}

fn collect_coverage_gaps(events: &[Event]) -> Vec<CoverageGap> {
    events
        .iter()
        .filter_map(|event| {
            let EventKind::ImportCoverageGap {
                period, dimensions, ..
            } = &event.kind
            else {
                return None;
            };
            Some(CoverageGap {
                account: event.account,
                period: *period,
                source: event.provenance.source(),
                parser_version: event.provenance.parser_version().clone(),
                dimensions: dimensions.clone(),
            })
        })
        .collect()
}

fn tainted_dimensions(group: &StatementGroup, gaps: &[CoverageGap]) -> BTreeSet<Dimension> {
    let mut tainted = BTreeSet::new();
    for gap in gaps {
        // Correlate an attempt by (account, period, source, parser version), deliberately
        // omitting document: each assertion claim can carry a distinct document hash.
        if gap.account == group.account
            && gap.period == group.period
            && gap.source == group.channel.source
            && gap.parser_version == group.channel.parser_version
        {
            tainted.extend(&gap.dimensions);
        }
    }
    tainted
}

/// Evidence 5: separate control sections of the same document reconciled
/// simultaneously.
///
/// Both the balance and the turnover amount are required: they are calculated differently,
/// and having both match provides an independent equation. A single reconciled
/// balance only confirms itself and is not evidence.
fn ground_five(
    group: &StatementGroup,
    outcomes: &[ClaimCheck],
    tainted: &BTreeSet<Dimension>,
) -> Option<Evidence> {
    if outcomes.is_empty() || !outcomes.iter().all(|check| check.outcome.confirms()) {
        return None;
    }
    let has_balance = group.claims.iter().any(|claim| {
        matches!(
            claim,
            ControlClaim::CashBalance { .. } | ControlClaim::PositionQuantity { .. }
        )
    });
    let has_flow = group.claims.iter().any(|claim| {
        matches!(
            claim,
            ControlClaim::CashTurnover { .. }
                | ControlClaim::FeesTotal { .. }
                | ControlClaim::IncomeTotal { .. }
        )
    });
    if !has_balance || !has_flow {
        return None;
    }
    let mut dimensions: BTreeSet<Dimension> =
        group.claims.iter().map(ControlClaim::dimension).collect();
    dimensions.retain(|dimension| !tainted.contains(dimension));
    Evidence::from_match(
        Ground::SeparateSectionsAgree,
        group.channel.clone(),
        group.channel.clone(),
        dimensions,
    )
}

/// Evidence 1: the opening balance of the next statement matched the
/// calculated balance of the previous period.
///
/// The **previous** period is raised: that is the period being confirmed. Raising
/// the current period would mean counting confirmation of data that it
/// does not yet contain.
fn ground_one(
    group_index: usize,
    outcomes: &[ClaimCheck],
    groups: &[StatementGroup],
    tainted: &[BTreeSet<Dimension>],
) -> Option<(AssertionPeriod, Evidence)> {
    let group = &groups[group_index];
    let mut opening_matched: BTreeSet<Dimension> = outcomes
        .iter()
        .filter(|check| {
            check.outcome.confirms()
                && matches!(
                    check.claim,
                    ControlClaim::CashBalance {
                        at: BalancePoint::Opening,
                        ..
                    } | ControlClaim::PositionQuantity {
                        at: BalancePoint::Opening,
                        ..
                    }
                )
        })
        .map(|check| check.claim.dimension())
        .collect();
    if opening_matched.is_empty() {
        return None;
    }
    let (prior_index, prior) = groups
        .iter()
        .enumerate()
        .filter(|(_, other)| other.account == group.account && other.period.to < group.period.from)
        .max_by_key(|(_, other)| other.period.to)?;
    opening_matched.retain(|dimension| {
        !tainted[group_index].contains(dimension) && !tainted[prior_index].contains(dimension)
    });
    let evidence = Evidence::from_match(
        Ground::OpeningMatchesPriorClosing,
        group.channel.clone(),
        prior.channel.clone(),
        opening_matched,
    )?;
    Some((prior.period, evidence))
}

/// Evidence 2: the closing balance of one statement matched the opening balance
/// of the next.
///
/// Two **source assertions** are compared, not an assertion with
/// a projection: this checks continuity between the documents themselves.
fn ground_two(
    groups: &[StatementGroup],
    tainted: &[BTreeSet<Dimension>],
) -> Vec<(AccountId, AssertionPeriod, Evidence)> {
    let mut found = Vec::new();
    for (earlier_index, earlier) in groups.iter().enumerate() {
        for (later_index, later) in groups.iter().enumerate() {
            if earlier.account != later.account || later.period.from <= earlier.period.to {
                continue;
            }
            let mut dimensions = BTreeSet::new();
            for closing in &earlier.claims {
                for opening in &later.claims {
                    if continuous(*closing, *opening) {
                        dimensions.insert(closing.dimension());
                    }
                }
            }
            dimensions.retain(|dimension| {
                !tainted
                    .get(earlier_index)
                    .is_some_and(|items| items.contains(dimension))
                    && !tainted
                        .get(later_index)
                        .is_some_and(|items| items.contains(dimension))
            });
            if let Some(evidence) = Evidence::from_match(
                Ground::ContinuityBetweenStatements,
                later.channel.clone(),
                earlier.channel.clone(),
                dimensions,
            ) {
                found.push((earlier.account, earlier.period, evidence));
            }
        }
    }
    found
}

/// Whether the closing assertion of one statement matches the opening assertion of another.
fn continuous(closing: ControlClaim, opening: ControlClaim) -> bool {
    match (closing, opening) {
        (
            ControlClaim::CashBalance {
                currency: left_currency,
                amount: left,
                at: BalancePoint::Closing,
            },
            ControlClaim::CashBalance {
                currency: right_currency,
                amount: right,
                at: BalancePoint::Opening,
            },
        ) => left_currency == right_currency && left == right,
        (
            ControlClaim::PositionQuantity {
                instrument: left_instrument,
                custody: left_custody,
                quantity: left,
                at: BalancePoint::Closing,
            },
            ControlClaim::PositionQuantity {
                instrument: right_instrument,
                custody: right_custody,
                quantity: right,
                at: BalancePoint::Opening,
            },
        ) => left_instrument == right_instrument && left_custody == right_custody && left == right,
        _ => false,
    }
}

/// Evidence 3: two independent channels for the same interval.
///
/// Each pair is taken once (`i < j`): the independence relation is symmetric,
/// and a second copy of the same evidence would double the list of proofs
/// without adding anything.
fn ground_three(
    groups: &[StatementGroup],
    checked: &[Vec<ClaimCheck>],
    tainted: &[BTreeSet<Dimension>],
) -> Vec<(AccountId, AssertionPeriod, Evidence)> {
    let mut found = Vec::new();
    for (left_index, left_outcomes) in checked.iter().enumerate() {
        for (offset, right_outcomes) in checked.iter().skip(left_index + 1).enumerate() {
            let right_index = left_index + 1 + offset;
            let left = &groups[left_index];
            let right = &groups[right_index];
            if left.account != right.account
                || left.period != right.period
                || !left.channel.is_independent_of(&right.channel)
            {
                continue;
            }
            let confirmed: BTreeSet<Dimension> =
                confirmed_dimensions(left_outcomes, &tainted[left_index])
                    .intersection(&confirmed_dimensions(right_outcomes, &tainted[right_index]))
                    .copied()
                    .collect();
            if let Some(evidence) = Evidence::from_match(
                Ground::BrokerApiAgreesWithStatement,
                right.channel.clone(),
                left.channel.clone(),
                confirmed,
            ) {
                found.push((left.account, left.period, evidence));
            }
        }
    }
    found
}

/// Dimensions for which at least something in the group reconciled and nothing
/// was discrepant.
fn confirmed_dimensions(
    outcomes: &[ClaimCheck],
    tainted: &BTreeSet<Dimension>,
) -> BTreeSet<Dimension> {
    let mut confirmed = BTreeSet::new();
    let mut broken = BTreeSet::new();
    for check in outcomes {
        let dimension = check.claim.dimension();
        match check.outcome {
            ClaimOutcome::Matched => {
                confirmed.insert(dimension);
            }
            ClaimOutcome::Discrepant(_) => {
                broken.insert(dimension);
            }
            // Incomparable and scope-excepted outcomes neither confirm
            // nor invalidate anything: they are silent.
            ClaimOutcome::NotComparable { .. } | ClaimOutcome::Excepted { .. } => {}
        }
    }
    confirmed.retain(|dimension| !broken.contains(dimension) && !tainted.contains(dimension));
    confirmed
}

fn build_status(
    group: &StatementGroup,
    outcomes: Vec<ClaimCheck>,
    evidence: &[(AccountId, AssertionPeriod, Evidence)],
) -> ReconciliationStatus {
    let mut dimensions: BTreeMap<Dimension, DimensionStatus> = BTreeMap::new();
    let mut own_evidence = Vec::new();
    for (account, period, item) in evidence {
        if *account == group.account && *period == group.period {
            raise(
                &mut dimensions,
                &item.dimensions(),
                DimensionStatus::from_level(item.level()),
            );
            own_evidence.push(item.clone());
        }
    }
    // A discrepancy absorbs everything: it is applied after status raises and is not cleared.
    for check in &outcomes {
        if matches!(check.outcome, ClaimOutcome::Discrepant(_)) {
            dimensions.insert(check.claim.dimension(), DimensionStatus::Discrepant);
        }
    }
    ReconciliationStatus {
        account: group.account,
        period: group.period,
        dimensions,
        evidence: own_evidence,
        outcomes,
    }
}

/// Raise dimension statuses to the evidence level. There is no downgrade:
/// evidence weaker than the level already reached changes nothing.
fn raise(
    dimensions: &mut BTreeMap<Dimension, DimensionStatus>,
    of: &BTreeSet<Dimension>,
    level: DimensionStatus,
) {
    for dimension in of {
        let slot = dimensions
            .entry(*dimension)
            .or_insert(DimensionStatus::Provisional);
        *slot = (*slot).max(level);
    }
}

/// Merge statuses for one account and interval received from different
/// documents: retain the best confirmation and all discrepancies.
fn merge_status(into: &mut Vec<ReconciliationStatus>, status: ReconciliationStatus) {
    let Some(existing) = into
        .iter_mut()
        .find(|item| item.account == status.account && item.period == status.period)
    else {
        into.push(status);
        return;
    };
    for (dimension, value) in &status.dimensions {
        let slot = existing
            .dimensions
            .entry(*dimension)
            .or_insert(DimensionStatus::Provisional);
        // A discrepancy absorbs a merge from either side: otherwise
        // confirmation from the second document would override an already detected
        // problem. In every other case, take the strongest status.
        *slot = if *value == DimensionStatus::Discrepant || *slot == DimensionStatus::Discrepant {
            DimensionStatus::Discrepant
        } else {
            (*slot).max(*value)
        };
    }
    existing.evidence.extend(status.evidence);
    existing.outcomes.extend(status.outcomes);
}

/// Tests for internal registry functions.
///
/// These live here rather than in integration tests because they check
/// decisions that are only indirectly visible externally: merging statuses
/// for one interval from different documents, assertion continuity,
/// and the rule that “raising does not lower”. The mutation barrier showed that
/// these branches cannot be reached through the public entry point (§15.7).
#[cfg(test)]
mod internals {
    use super::*;
    use crate::event::Relation;
    use crate::event::provenance::{ParserVersion, RawHash};
    use crate::event::test_support::sample_event_with;
    use crate::ids::{CustodyId, InstrumentId, SourceId};
    use crate::money::{CurrencyCode, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use time::macros::date;

    /// A channel with a document derived from the parser name.
    ///
    /// The document must differ along with the parser: the same hash
    /// for different channels would mean the same file, so there would be no independence
    /// under the rule in §10.3—which is the correct behavior,
    /// but not what this test checks.
    fn channel(parser: &str) -> SourceChannel {
        let mut hex: String = parser.bytes().map(|byte| format!("{byte:02x}")).collect();
        hex.truncate(64);
        while hex.len() < 64 {
            hex.push('0');
        }
        SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion(parser.to_owned()),
            document: Some(RawHash::parse(&hex).unwrap()),
        }
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    fn april() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30)).unwrap()
    }

    fn cash(amount: i64, at: BalancePoint) -> ControlClaim {
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(amount),
            at,
        }
    }

    fn group(period: AssertionPeriod, parser: &str, claims: Vec<ControlClaim>) -> StatementGroup {
        StatementGroup {
            account: AccountId::new_random(),
            period,
            channel: channel(parser),
            claims,
        }
    }

    #[test]
    fn a_correction_failure_is_reported_by_reconciliation_build() {
        let orphan = sample_event_with(
            0,
            Relation::Reversal {
                target: crate::ids::EventId::new_random(),
            },
        );

        assert!(matches!(
            ReconciliationLedger::build(&[orphan]),
            Err(ObserveError::Correction(
                crate::event::correction::CorrectionError::DanglingTarget { .. }
            ))
        ));
    }

    #[test]
    fn continuity_requires_the_same_currency_and_the_same_amount() {
        // Continuity means matching the closing balance of one
        // statement with the opening balance of the next. Relaxing either condition
        // would declare documents separated by a gap continuous.
        let closing = cash(100_000, BalancePoint::Closing);
        assert!(continuous(closing, cash(100_000, BalancePoint::Opening)));
        assert!(
            !continuous(closing, cash(99_999, BalancePoint::Opening)),
            "different amounts do not establish continuity"
        );
        assert!(
            !continuous(
                closing,
                ControlClaim::CashBalance {
                    currency: CurrencyCode::Usd,
                    amount: PostedMinor::new(100_000),
                    at: BalancePoint::Opening,
                }
            ),
            "different currencies do not establish continuity"
        );
        assert!(
            !continuous(closing, cash(100_000, BalancePoint::Closing)),
            "two closing balances are not continuity"
        );
        assert!(
            !continuous(
                cash(100_000, BalancePoint::Opening),
                cash(100_000, BalancePoint::Opening)
            ),
            "continuity runs from closing to opening, not the other way around"
        );
    }

    #[test]
    fn position_continuity_requires_the_same_instrument_and_custody() {
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let quantity = Quantity(Dec::one());
        let closing = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at: BalancePoint::Closing,
        };
        let opening = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at: BalancePoint::Opening,
        };
        assert!(continuous(closing, opening));

        let elsewhere = ControlClaim::PositionQuantity {
            instrument,
            custody: CustodyId::new_random(),
            quantity,
            at: BalancePoint::Opening,
        };
        assert!(
            !continuous(closing, elsewhere),
            "the same quantity in another depository is a different position"
        );

        let other_paper = ControlClaim::PositionQuantity {
            instrument: InstrumentId::new_random(),
            custody,
            quantity,
            at: BalancePoint::Opening,
        };
        assert!(!continuous(closing, other_paper));
    }

    #[test]
    fn a_claim_of_one_kind_is_never_continuous_with_another() {
        // Turnover and balance are not compared with each other: they have different
        // meanings, and declaring them continuous would present
        // a coincidental match between numbers as confirmation.
        let turnover = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(100_000),
            credit: PostedMinor::new(0),
        };
        assert!(!continuous(turnover, cash(100_000, BalancePoint::Opening)));
        assert!(!continuous(cash(100_000, BalancePoint::Closing), turnover));
    }

    #[test]
    fn continuity_holds_only_between_documents_that_do_not_overlap() {
        // Statements for overlapping periods are not continuous:
        // continuity is a junction, not an overlap.
        let account = AccountId::new_random();
        let mut earlier = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        let mut later = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        earlier.account = account;
        later.account = account;

        let found = ground_two(
            &[earlier.clone(), later.clone()],
            &[BTreeSet::new(), BTreeSet::new()],
        );
        assert_eq!(
            found.len(),
            1,
            "the junction of March and April provides evidence"
        );
        assert_eq!(found[0].1, march(), "the earlier period is confirmed");

        // The same document overlaid on itself provides no evidence.
        assert!(
            ground_two(
                &[earlier.clone(), earlier],
                &[BTreeSet::new(), BTreeSet::new()],
            )
            .is_empty()
        );
    }

    #[test]
    fn continuity_is_not_claimed_across_accounts() {
        let earlier = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        let later = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        assert!(
            ground_two(&[earlier, later], &[BTreeSet::new(), BTreeSet::new()]).is_empty(),
            "different accounts have no continuity"
        );
    }

    #[test]
    fn raising_never_lowers_an_already_reached_level() {
        // Raising a status takes the maximum, not the last value
        // recorded. Otherwise, weaker evidence arriving later would override
        // stronger evidence.
        let mut dimensions = BTreeMap::new();
        let only_cash: BTreeSet<Dimension> = [Dimension::Cash].into_iter().collect();

        raise(
            &mut dimensions,
            &only_cash,
            DimensionStatus::AcceptedIndependent,
        );
        raise(
            &mut dimensions,
            &only_cash,
            DimensionStatus::AcceptedInternal,
        );
        assert_eq!(
            dimensions.get(&Dimension::Cash),
            Some(&DimensionStatus::AcceptedIndependent),
            "weaker evidence does not lower the level already reached"
        );

        raise(
            &mut dimensions,
            &only_cash,
            DimensionStatus::AcceptedIndependent,
        );
        assert_eq!(
            dimensions.get(&Dimension::Cash),
            Some(&DimensionStatus::AcceptedIndependent),
            "repeating the same level changes nothing"
        );
    }

    fn status_with(
        account: AccountId,
        period: AssertionPeriod,
        dimension: Dimension,
        value: DimensionStatus,
    ) -> ReconciliationStatus {
        let mut dimensions = BTreeMap::new();
        dimensions.insert(dimension, value);
        ReconciliationStatus {
            account,
            period,
            dimensions,
            evidence: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    #[test]
    fn merging_takes_the_best_confirmation_of_the_same_period() {
        // Two documents for the same period: the strongest confirmation
        // remains. Otherwise, document read order would determine the level.
        let account = AccountId::new_random();
        let mut statuses = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::AcceptedInternal,
        )];
        merge_status(
            &mut statuses,
            status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::AcceptedIndependent,
            ),
        );
        assert_eq!(
            statuses.len(),
            1,
            "statuses for the same period were merged"
        );
        assert_eq!(
            statuses[0].dimension(Dimension::Cash),
            DimensionStatus::AcceptedIndependent
        );
    }

    #[test]
    fn merging_keeps_a_discrepancy_whichever_side_it_came_from() {
        // A discrepancy absorbs a merge in both directions: whether it
        // arrived second or first. A one-sided check
        // would miss half the cases.
        let account = AccountId::new_random();

        let mut first = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::AcceptedIndependent,
        )];
        merge_status(
            &mut first,
            status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::Discrepant,
            ),
        );
        assert_eq!(
            first[0].dimension(Dimension::Cash),
            DimensionStatus::Discrepant
        );

        let mut second = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::Discrepant,
        )];
        merge_status(
            &mut second,
            status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::AcceptedIndependent,
            ),
        );
        assert_eq!(
            second[0].dimension(Dimension::Cash),
            DimensionStatus::Discrepant,
            "confirmation does not override an already detected discrepancy"
        );
    }

    #[test]
    fn statuses_of_different_accounts_or_periods_do_not_merge() {
        let account = AccountId::new_random();
        let mut statuses = vec![status_with(
            account,
            march(),
            Dimension::Cash,
            DimensionStatus::AcceptedInternal,
        )];
        merge_status(
            &mut statuses,
            status_with(
                account,
                april(),
                Dimension::Cash,
                DimensionStatus::Discrepant,
            ),
        );
        assert_eq!(statuses.len(), 2, "different periods are not merged");

        merge_status(
            &mut statuses,
            status_with(
                AccountId::new_random(),
                march(),
                Dimension::Cash,
                DimensionStatus::Discrepant,
            ),
        );
        assert_eq!(statuses.len(), 3, "different accounts are not merged");
    }

    #[test]
    fn the_worst_status_wins_across_overlapping_periods() {
        // Two assertions cover the same day, and one did not reconcile.
        // Taking the best would allow an extra document to conceal
        // the problem.
        let account = AccountId::new_random();
        let year = AssertionPeriod::between(date!(2026 - 01 - 01), date!(2026 - 12 - 31)).unwrap();
        let ledger = ReconciliationLedger {
            statuses: vec![
                status_with(
                    account,
                    year,
                    Dimension::Cash,
                    DimensionStatus::AcceptedIndependent,
                ),
                status_with(
                    account,
                    march(),
                    Dimension::Cash,
                    DimensionStatus::Discrepant,
                ),
            ],
        };
        assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
            DimensionStatus::Discrepant
        );
        assert_eq!(
            ledger.status_for(account, date!(2026 - 07 - 15), Dimension::Cash),
            DimensionStatus::AcceptedIndependent,
            "the discrepancy does not apply outside the March interval"
        );
        assert_eq!(
            ledger.status_for(
                AccountId::new_random(),
                date!(2026 - 03 - 15),
                Dimension::Cash
            ),
            DimensionStatus::Provisional,
            "the registry makes no assertions about another account"
        );
    }

    #[test]
    fn external_evidence_lands_on_the_matching_period_and_creates_one_otherwise() {
        // Evidence 4, 6, 7, and 8 comes from outside. It must be applied
        // to an existing status, or create one if none exists:
        // otherwise the depository confirmation would simply disappear.
        let account = AccountId::new_random();
        let evidence = Evidence::from_match(
            Ground::DepositaryReportConfirms,
            channel("depositary/1"),
            channel("report/1"),
            [Dimension::Positions].into_iter().collect(),
        )
        .expect("evidence");

        let existing = ReconciliationLedger {
            statuses: vec![status_with(
                account,
                march(),
                Dimension::Cash,
                DimensionStatus::AcceptedInternal,
            )],
        }
        .with_external_evidence(vec![(account, march(), evidence.clone())]);
        assert_eq!(
            existing.statuses().count(),
            1,
            "the status was not duplicated"
        );
        assert_eq!(
            existing.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
            DimensionStatus::AcceptedIndependent
        );
        assert_eq!(
            existing.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
            DimensionStatus::AcceptedInternal,
            "another dimension was not changed"
        );

        let fresh = ReconciliationLedger::default().with_external_evidence(vec![(
            account,
            march(),
            evidence,
        )]);
        assert_eq!(fresh.statuses().count(), 1, "the status was created");
        assert_eq!(
            fresh.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
            DimensionStatus::AcceptedIndependent
        );
    }

    #[test]
    fn external_evidence_does_not_leak_between_periods_of_one_account() {
        // Evidence submitted for March must not raise
        // April. Weakening the lookup key to “account OR period” would assign
        // the depository confirmation to the first status encountered.
        let account = AccountId::new_random();
        let evidence = Evidence::from_match(
            Ground::DepositaryReportConfirms,
            channel("depositary/1"),
            channel("report/1"),
            [Dimension::Positions].into_iter().collect(),
        )
        .expect("evidence");

        let ledger = ReconciliationLedger {
            statuses: vec![
                status_with(
                    account,
                    april(),
                    Dimension::Positions,
                    DimensionStatus::Provisional,
                ),
                status_with(
                    account,
                    march(),
                    Dimension::Positions,
                    DimensionStatus::Provisional,
                ),
            ],
        }
        .with_external_evidence(vec![(account, march(), evidence)]);

        assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Positions),
            DimensionStatus::AcceptedIndependent,
            "March is confirmed"
        );
        assert_eq!(
            ledger.status_for(account, date!(2026 - 04 - 15), Dimension::Positions),
            DimensionStatus::Provisional,
            "April is not confirmed by March evidence"
        );
        assert_eq!(ledger.statuses().count(), 2, "the statuses were not merged");
    }

    #[test]
    fn ground_one_ignores_an_opening_claim_that_did_not_match() {
        // Evidence 1 requires the OPENING balance specifically to reconcile.
        // Neither a mismatched opening balance nor a matched closing balance provides it:
        // the former confirms nothing, while the latter concerns its own period.
        let account = AccountId::new_random();
        let mut current = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        current.account = account;
        let mut prior = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        prior.account = account;
        let groups = [prior, current.clone()];

        let unmatched_opening = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::Discrepant(check::Discrepancy {
                field: "amount",
                claimed: check::ClaimValue::Money {
                    amount: PostedMinor::new(100_000),
                    currency: CurrencyCode::Rub,
                },
                observed: check::ClaimValue::Money {
                    amount: PostedMinor::new(1),
                    currency: CurrencyCode::Rub,
                },
                delta: check::ClaimValue::Money {
                    amount: PostedMinor::new(99_999),
                    currency: CurrencyCode::Rub,
                },
            }),
        }];
        assert!(
            ground_one(
                1,
                &unmatched_opening,
                &groups,
                &[BTreeSet::new(), BTreeSet::new()],
            )
            .is_none(),
            "a mismatched opening balance confirms nothing"
        );

        let matched_closing = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Closing),
            outcome: ClaimOutcome::Matched,
        }];
        assert!(
            ground_one(
                1,
                &matched_closing,
                &groups,
                &[BTreeSet::new(), BTreeSet::new()],
            )
            .is_none(),
            "the closing balance concerns its own period, not the previous one"
        );
    }

    #[test]
    fn a_prior_statement_must_end_before_the_current_one_starts() {
        // A statement ending on the day the current one begins is not
        // previous: their periods touch, and the shared day would belong
        // to both. A period cannot be confirmed by its own day.
        let account = AccountId::new_random();
        let mut current = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        current.account = account;
        let touching = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 04 - 01))
            .expect("interval");
        let mut overlapping = group(
            touching,
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        overlapping.account = account;

        let outcomes = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::Matched,
        }];
        assert!(
            ground_one(
                1,
                &outcomes,
                &[overlapping, current.clone()],
                &[BTreeSet::new(), BTreeSet::new()],
            )
            .is_none(),
            "a touching statement is not considered previous"
        );
    }

    #[test]
    fn ground_three_needs_all_three_conditions_at_once() {
        // Evidence 3 requires all of the following: the same account, the same period,
        // and independent channels. Relaxing any condition would treat
        // a match of unrelated figures as independent confirmation.
        let account = AccountId::new_random();
        let matched = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Closing),
            outcome: ClaimOutcome::Matched,
        }];
        let no_taint = [BTreeSet::new(), BTreeSet::new()];

        let mut left = group(
            march(),
            "report/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        left.account = account;

        // The same account and an independent channel, but a DIFFERENT period.
        let mut other_period = group(april(), "api/1", vec![cash(100_000, BalancePoint::Closing)]);
        other_period.account = account;
        assert!(
            ground_three(
                &[left.clone(), other_period],
                &[matched.clone(), matched.clone()],
                &no_taint,
            )
            .is_empty(),
            "confirmation for another period is not evidence"
        );

        // The same period and an independent channel, but a DIFFERENT account.
        let other_account = group(march(), "api/1", vec![cash(100_000, BalancePoint::Closing)]);
        assert!(
            ground_three(
                &[left.clone(), other_account],
                &[matched.clone(), matched.clone()],
                &no_taint,
            )
            .is_empty(),
            "confirmation for another account is not evidence"
        );

        // The same account and period, but the channel is NOT independent.
        let same_channel = StatementGroup {
            account,
            period: march(),
            channel: left.channel.clone(),
            claims: vec![cash(100_000, BalancePoint::Closing)],
        };
        assert!(
            ground_three(
                &[left.clone(), same_channel],
                &[matched.clone(), matched.clone()],
                &no_taint,
            )
            .is_empty(),
            "the same channel does not provide independence"
        );

        // All three conditions are satisfied—evidence exists.
        let mut independent = group(march(), "api/1", vec![cash(100_000, BalancePoint::Closing)]);
        independent.account = account;
        let found = ground_three(&[left, independent], &[matched.clone(), matched], &no_taint);
        assert_eq!(found.len(), 1, "independent channel for the same period");
        assert_eq!(found[0].1, march());
    }

    #[test]
    fn ground_one_needs_a_strictly_earlier_statement() {
        // The previous period is confirmed. A statement overlapping
        // the current one is not previous, and using it would mean
        // confirming a period with its own data.
        let account = AccountId::new_random();
        let mut current = group(
            april(),
            "same/1",
            vec![cash(100_000, BalancePoint::Opening)],
        );
        current.account = account;
        let outcomes = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::Matched,
        }];

        assert!(
            ground_one(
                0,
                &outcomes,
                std::slice::from_ref(&current),
                &[BTreeSet::new()],
            )
            .is_none(),
            "an account cannot be its own previous statement"
        );

        let mut prior = group(
            march(),
            "same/1",
            vec![cash(100_000, BalancePoint::Closing)],
        );
        prior.account = account;
        let found = ground_one(
            1,
            &outcomes,
            &[prior, current.clone()],
            &[BTreeSet::new(), BTreeSet::new()],
        )
        .expect("previous statement found");
        assert_eq!(found.0, march(), "the previous period is confirmed");

        // A mismatched opening balance provides no evidence.
        let broken = vec![ClaimCheck {
            claim: cash(100_000, BalancePoint::Opening),
            outcome: ClaimOutcome::NotComparable {
                reason: check::NotComparable::NoJournalCoverage,
            },
        }];
        assert!(ground_one(0, &broken, &[current.clone()], &[BTreeSet::new()],).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dimension_has_a_distinct_machine_readable_code() {
        let codes: Vec<&str> = Dimension::all().iter().map(|d| d.code()).collect();
        assert_eq!(codes, vec!["cash", "positions", "tax_basis", "income"]);
    }

    #[test]
    fn every_confidence_level_has_a_distinct_machine_readable_code() {
        // The level is exposed as a code: an external agent uses it to decide
        // whether to display a warning. An empty string is indistinguishable from
        // “no level”, while one code for all three is indistinguishable from “some kind of data”.
        let all = [
            ConfidenceLevel::Provisional,
            ConfidenceLevel::AcceptedInternal,
            ConfidenceLevel::AcceptedIndependent,
        ];
        let codes: Vec<&str> = all.iter().map(|level| level.code()).collect();
        assert_eq!(
            codes,
            vec!["provisional", "accepted_internal", "accepted_independent"]
        );
    }

    #[test]
    fn confidence_levels_are_ordered_from_weakest_to_strongest() {
        // The order is used to raise the status. An incorrect
        // order would silently turn a raise into a downgrade.
        assert!(ConfidenceLevel::Provisional < ConfidenceLevel::AcceptedInternal);
        assert!(ConfidenceLevel::AcceptedInternal < ConfidenceLevel::AcceptedIndependent);
    }

    #[test]
    fn every_dimension_status_has_a_distinct_machine_readable_code() {
        let all = [
            DimensionStatus::Provisional,
            DimensionStatus::AcceptedInternal,
            DimensionStatus::AcceptedIndependent,
            DimensionStatus::Discrepant,
        ];
        let codes: Vec<&str> = all.iter().map(|status| status.code()).collect();
        assert_eq!(
            codes,
            vec![
                "provisional",
                "accepted_internal",
                "accepted_independent",
                "discrepant"
            ]
        );
    }

    #[test]
    fn a_discrepancy_ranks_below_an_unconfirmed_state() {
        // “Did not reconcile” is a detected problem; “not checked yet” is not.
        // If a discrepancy ranked higher, selecting the worst status among periods
        // would not select the discrepancy, and the problem would be hidden.
        assert!(DimensionStatus::Discrepant < DimensionStatus::Provisional);
        assert!(DimensionStatus::Provisional < DimensionStatus::AcceptedInternal);
        assert!(DimensionStatus::AcceptedInternal < DimensionStatus::AcceptedIndependent);
    }

    #[test]
    fn the_list_of_dimensions_covers_every_variant_once() {
        // The list is specified manually, so it must be checked: a forgotten
        // dimension receives no status and looks like “there is nothing
        // to confirm”, while a duplicate is counted twice.
        for dimension in Dimension::all() {
            let found = Dimension::all().iter().filter(|d| **d == dimension).count();
            assert_eq!(
                found, 1,
                "dimension {dimension:?} does not occur exactly once"
            );
        }
        assert_eq!(Dimension::all().len(), 4);
    }
}
