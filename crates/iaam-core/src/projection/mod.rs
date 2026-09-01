//! Snapshot-based log projections (§3.1).
//!
//! “The entire log in memory” is the default, not an architectural invariant.
//! Therefore, the public interface supports snapshots from the outset:
//! [`project`] builds one from scratch, [`advance`] advances an existing one,
//! and full recomputation remains the reference for the incremental path.
//!
//! Snapshots and the cache are stored by the **wrapper**: the core remains stateless.

pub mod active_instruments;
pub mod balances;
pub mod flows;
pub mod money_flow;
pub mod income;
pub mod invariants;
pub mod lots;
pub mod offers;
pub mod ownership;
pub mod state;

pub use active_instruments::active_instruments;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::dates::EffectiveOrder;
use crate::event::Event;
use crate::event::correction::{CorrectionError, resolve};
use crate::event::kind::EventKind;
use crate::rules::{LotRuleVersion, RuleRegistry};
use crate::valuation::InstrumentPrice;
use balances::BalanceError;
use flows::FlowError;
use income::IncomeError;
use invariants::{InvariantReport, InvariantViolation};
use lots::{LotBook, LotError};
use state::{LedgerState, StateHash};

/// Projection format version. A snapshot built by another version
/// cannot be advanced: the meaning of its fields may have changed.
///
/// Version 7: face value was removed from the lot, and the prefix fingerprint covers
/// the event contents (`prefix_digest/v2`). Version 8 incorporates source-time
/// ordering; older snapshots are incompatible and trigger a full recomputation.
pub const PROJECTION_VERSION: u32 = 8;

/// Immutable projection input: scope boundaries and rule versions.
///
/// `Debug` is not derived: `RuleRegistry` stores strategy trait objects
/// that do not and cannot have a debug representation.
#[derive(Clone, Copy)]
pub struct ProjectionContext<'a> {
    pub contour: &'a ContourDefinition,
    pub rules: &'a RuleRegistry,
    pub lot_rule: LotRuleVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectionError {
    #[error("snapshot was built by projection version {found}, current version is {expected}")]
    SnapshotVersionMismatch { expected: u32, found: u32 },
    #[error(
        "snapshot fingerprint does not match its state: snapshot was assembled without the core"
    )]
    SnapshotFingerprintMismatch,
    #[error(
        "snapshot was built for scope {snapshot_contour:?} version {snapshot_version:?}, \
         requested {requested_contour:?} version {requested_version:?}"
    )]
    SnapshotContourMismatch {
        snapshot_contour: ContourId,
        snapshot_version: ContourVersion,
        requested_contour: ContourId,
        requested_version: ContourVersion,
    },
    #[error("snapshot was built with disposal rule {snapshot:?}, requested {requested:?}")]
    SnapshotRuleMismatch {
        snapshot: LotRuleVersion,
        requested: LotRuleVersion,
    },
    #[error(
        "the active log up to the snapshot boundary has changed: the snapshot cannot be advanced, \
         a full recomputation is required"
    )]
    PrefixChanged {
        expected: StateHash,
        found: StateHash,
    },
    #[error(transparent)]
    Correction(#[from] CorrectionError),
    #[error(transparent)]
    Balance(#[from] BalanceError),
    #[error(transparent)]
    Lot(#[from] LotError),
    #[error(transparent)]
    Flow(#[from] FlowError),
    #[error(transparent)]
    Income(#[from] IncomeError),
    #[error(transparent)]
    Invariant(#[from] InvariantViolation),
}

impl ProjectionError {
    /// Machine-readable code for APIs and logs.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SnapshotVersionMismatch { .. } => "snapshot_version_mismatch",
            Self::SnapshotFingerprintMismatch => "snapshot_fingerprint_mismatch",
            Self::SnapshotContourMismatch { .. } => "snapshot_contour_mismatch",
            Self::SnapshotRuleMismatch { .. } => "snapshot_rule_mismatch",
            Self::PrefixChanged { .. } => "prefix_changed",
            Self::Correction(_) => "correction",
            Self::Balance(_) => "balance",
            Self::Lot(_) => "lot",
            Self::Flow(_) => "flow",
            Self::Income(_) => "income",
            Self::Invariant(_) => "invariant",
        }
    }

    /// Distinguishes an invariant violation from incomplete input (§15.2).
    #[must_use]
    pub const fn is_invariant_violation(&self) -> bool {
        matches!(self, Self::Invariant(_))
    }
}

/// State snapshot at the `through` boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    projection_version: u32,
    contour: ContourId,
    contour_version: ContourVersion,
    lot_rule: LotRuleVersion,
    through: Option<EffectiveOrder>,
    state: LedgerState,
    fingerprint: StateHash,
    /// Fingerprint of the active log folded into this snapshot.
    prefix_digest: StateHash,
}

impl Snapshot {
    #[must_use]
    pub const fn projection_version(&self) -> u32 {
        self.projection_version
    }

    #[must_use]
    pub const fn contour(&self) -> ContourId {
        self.contour
    }

    #[must_use]
    pub const fn contour_version(&self) -> ContourVersion {
        self.contour_version
    }

    #[must_use]
    pub const fn lot_rule(&self) -> LotRuleVersion {
        self.lot_rule
    }

    #[must_use]
    pub const fn through(&self) -> Option<EffectiveOrder> {
        self.through
    }

    #[must_use]
    pub const fn state(&self) -> &LedgerState {
        &self.state
    }

    #[must_use]
    pub const fn fingerprint(&self) -> StateHash {
        self.fingerprint
    }

    /// Fingerprint of the folded log prefix. Distinguishes
    /// “the log is unchanged” from “the log changed before the snapshot boundary”.
    #[must_use]
    pub const fn prefix_digest(&self) -> StateHash {
        self.prefix_digest
    }
}

/// Decomposed snapshot.
///
/// Exists for storage: the snapshot is stored in the database in parts and
/// assembled again. The core verifies a snapshot assembled this way
/// using its fingerprint—the wrapper may have assembled it incorrectly or incompletely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotParts {
    pub projection_version: u32,
    pub contour: ContourId,
    pub contour_version: ContourVersion,
    pub lot_rule: LotRuleVersion,
    pub through: Option<EffectiveOrder>,
    pub state: LedgerState,
    pub fingerprint: StateHash,
    pub prefix_digest: StateHash,
}

impl Snapshot {
    /// Assembles a snapshot from stored parts. The fingerprint is **not** recomputed:
    /// the point of the check in `advance` is precisely to compare the declared
    /// fingerprint against the actual state.
    #[must_use]
    pub fn restore(parts: SnapshotParts) -> Self {
        Self {
            projection_version: parts.projection_version,
            contour: parts.contour,
            contour_version: parts.contour_version,
            lot_rule: parts.lot_rule,
            through: parts.through,
            state: parts.state,
            fingerprint: parts.fingerprint,
            prefix_digest: parts.prefix_digest,
        }
    }

    #[must_use]
    pub fn into_parts(self) -> SnapshotParts {
        SnapshotParts {
            projection_version: self.projection_version,
            contour: self.contour,
            contour_version: self.contour_version,
            lot_rule: self.lot_rule,
            through: self.through,
            state: self.state,
            fingerprint: self.fingerprint,
            prefix_digest: self.prefix_digest,
        }
    }
}

/// Projection result: a snapshot plus the list of verified invariants.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    snapshot: Snapshot,
    invariants: InvariantReport,
}

impl Projection {
    #[must_use]
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    #[must_use]
    pub const fn state(&self) -> &LedgerState {
        self.snapshot.state()
    }

    #[must_use]
    pub const fn invariants(&self) -> &InvariantReport {
        &self.invariants
    }

    #[must_use]
    pub fn into_snapshot(self) -> Snapshot {
        self.snapshot
    }
}

/// Full recomputation from scratch. The reference for [`advance`].
pub fn project(events: &[Event], ctx: &ProjectionContext) -> Result<Projection, ProjectionError> {
    let state = LedgerState::new(LotBook::new(ctx.lot_rule));
    let effective = resolve(events)?;
    fold(state, &[], &effective, ctx)
}

/// Advances a snapshot using a **full log slice**.
///
/// Takes the same slice as [`project`], not a “batch of new events”.
/// This is not a convenience but a correctness requirement: an event added
/// retroactively before the snapshot boundary changes neither the boundary nor the state
/// of the snapshot. A caller that selects “everything after the boundary” will silently
/// miss such an event—and obtain plausible but incorrect
/// balances, lots, and returns. Code review confirmed this: this exact bug was
/// present in the first revision of this module.
///
/// Therefore, the core determines whether the snapshot is applicable: it folds
/// the active set, compares the prefix fingerprint, and advances the state
/// only with events beyond the boundary. A prefix mismatch is not an operational error,
/// but a signal that “a full recomputation is required”; reversing an event within the snapshot
/// appears exactly this way because it removes the event from the active set.
pub fn advance(
    previous: &Snapshot,
    events: &[Event],
    ctx: &ProjectionContext,
) -> Result<Projection, ProjectionError> {
    if previous.projection_version != PROJECTION_VERSION {
        return Err(ProjectionError::SnapshotVersionMismatch {
            expected: PROJECTION_VERSION,
            found: previous.projection_version,
        });
    }
    if previous.contour != ctx.contour.id() || previous.contour_version != ctx.contour.version() {
        return Err(ProjectionError::SnapshotContourMismatch {
            snapshot_contour: previous.contour,
            snapshot_version: previous.contour_version,
            requested_contour: ctx.contour.id(),
            requested_version: ctx.contour.version(),
        });
    }
    if previous.lot_rule != ctx.lot_rule {
        return Err(ProjectionError::SnapshotRuleMismatch {
            snapshot: previous.lot_rule,
            requested: ctx.lot_rule,
        });
    }
    if previous.state.fingerprint() != previous.fingerprint {
        return Err(ProjectionError::SnapshotFingerprintMismatch);
    }

    let effective = resolve(events)?;
    let split = match previous.through {
        None => 0,
        Some(through) => effective.partition_point(|event| event.order <= through),
    };
    let (prefix, suffix) = effective.split_at(split);

    let found = state::prefix_digest(prefix);
    if found != previous.prefix_digest {
        return Err(ProjectionError::PrefixChanged {
            expected: previous.prefix_digest,
            found,
        });
    }

    fold(previous.state.clone(), prefix, suffix, ctx)
}

/// Applies the active event set to the state.
///
/// Three independent log readers—balances, lots, and flows—are invoked
/// in sequence for each event. They intentionally share no helper functions:
/// the invariant “the sum of lots equals the position” holds precisely
/// because of this independence (§15.4).
fn fold(
    mut state: LedgerState,
    already_applied: &[&Event],
    effective: &[&Event],
    ctx: &ProjectionContext,
) -> Result<Projection, ProjectionError> {
    let mut through = already_applied.last().map(|event| event.order);
    for event in effective {
        {
            let (balances, book, flows) = state.parts_mut();
            balances.apply(event)?;
            book.apply(event, ctx.rules)?;
            flows.apply(event, ctx.contour)?;
        }
        state.income_mut().apply(event)?;
        if let EventKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } = &event.kind
        {
            if let Some(as_of) = event.dates.effective_date() {
                state.prices_mut().record(InstrumentPrice {
                    instrument: *instrument,
                    price: *price,
                    currency: *currency,
                    quality: *quality,
                    as_of,
                });
            }
        }
        state.observe(event);
        through = Some(event.order);
    }

    // Invariants are checked against the entire active set, not only
    // the advanced portion: the state is shared, and the violation may have come
    // from a snapshot that the core is not required to trust (§15.2).
    let all: Vec<&Event> = already_applied
        .iter()
        .chain(effective.iter())
        .copied()
        .collect();
    let invariants = invariants::check(&state, &all)?;
    let fingerprint = state.fingerprint();
    let prefix_digest = state::prefix_digest(&all);
    Ok(Projection {
        snapshot: Snapshot {
            projection_version: PROJECTION_VERSION,
            contour: ctx.contour.id(),
            contour_version: ctx.contour.version(),
            lot_rule: ctx.lot_rule,
            through,
            state,
            fingerprint,
            prefix_digest,
        },
        invariants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourId, ContourVersion};
    use crate::event::Relation;
    use crate::event::kind::{EventKind, IncomeKind, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;
    use time::macros::{date, time};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    fn contour_of(account: AccountId) -> ContourDefinition {
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account])
    }

    fn deposits(account: AccountId) -> Vec<Event> {
        (1..=4)
            .map(|i| {
                let amount = rub(i64::from(i) * 10_000);
                event_with(
                    account,
                    date!(2025 - 01 - 01) + time::Duration::days(i64::from(i)),
                    i,
                    EventKind::CashIn { amount },
                    vec![Leg::cash(account, amount)],
                )
            })
            .collect()
    }

    #[test]
    fn a_snapshot_reports_its_version_and_its_boundary() {
        // The projection version and snapshot boundary form the storage contract: based on them
        // the wrapper decides whether the snapshot is usable at all. A silent zero
        // instead of the version would make an unusable snapshot usable.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };

        let empty = project(&[], &ctx).unwrap();
        assert_eq!(empty.snapshot.projection_version(), PROJECTION_VERSION);
        assert_eq!(
            empty.snapshot.through(),
            None,
            "an empty log has no boundary"
        );

        let events = deposits(account);
        let full = project(&events, &ctx).unwrap();
        assert_eq!(full.snapshot.projection_version(), PROJECTION_VERSION);
        assert_eq!(
            full.snapshot.through(),
            Some(events[3].order),
            "the boundary is the order of the last active event"
        );

        // A snapshot loaded from storage carries ITS OWN version, not the version
        // of the current code: rejection with
        // `SnapshotVersionMismatch` depends on exactly this distinction. An accessor returning a constant
        // would make a snapshot from another version usable.
        let foreign = Snapshot::restore(SnapshotParts {
            projection_version: PROJECTION_VERSION + 41,
            ..full.snapshot.into_parts()
        });
        assert_eq!(foreign.projection_version(), PROJECTION_VERSION + 41);
        assert!(matches!(
            advance(&foreign, &events, &ctx),
            Err(ProjectionError::SnapshotVersionMismatch { .. })
        ));
    }

    #[test]
    fn only_an_invariant_violation_is_reported_as_one() {
        // §15.2 requires distinguishing an invariant violation from incomplete input:
        // the former invalidates the report, while the latter marks it as uncomputable.
        let mismatched = ProjectionError::SnapshotRuleMismatch {
            snapshot: LotRuleVersion(1),
            requested: LotRuleVersion(2),
        };
        assert!(!mismatched.is_invariant_violation());
        assert!(!ProjectionError::SnapshotFingerprintMismatch.is_invariant_violation());
        assert!(
            ProjectionError::Invariant(InvariantViolation::LotsDoNotMatchPosition {
                key: crate::projection::lots::LotKey {
                    account: AccountId::new_random(),
                    instrument: InstrumentId::new_random(),
                },
                lots: "1".into(),
                position: "2".into(),
            })
            .is_invariant_violation()
        );
    }

    #[test]
    fn advancing_a_snapshot_equals_a_full_recompute() {
        // The incremental path must match the reference path: a snapshot is
        // an optimization, not a different model (§3.1).
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);

        let full = project(&events, &ctx).unwrap();
        let head = project(&events[..2], &ctx).unwrap();
        // The full slice is passed in: the core decides what has already been folded.
        let advanced = advance(head.snapshot(), &events, &ctx).unwrap();

        assert_eq!(
            full.snapshot().fingerprint(),
            advanced.snapshot().fingerprint()
        );
        assert_eq!(full.snapshot().through(), advanced.snapshot().through());
        assert_eq!(
            full.snapshot().prefix_digest(),
            advanced.snapshot().prefix_digest()
        );
    }

    #[test]
    fn import_order_does_not_change_the_projection() {
        // Property §15.3: the projection depends on EffectiveOrder, not on
        // the order in which the files were loaded.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let mut shuffled = events.clone();
        shuffled.reverse();

        assert_eq!(
            project(&events, &ctx).unwrap().snapshot().fingerprint(),
            project(&shuffled, &ctx).unwrap().snapshot().fingerprint()
        );
    }

    #[test]
    fn a_tampered_snapshot_is_rejected() {
        // The wrapper stores the snapshot; the core is not required to trust it.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events, &ctx).unwrap().into_snapshot();

        // The wrapper assembled the snapshot from parts and substituted an unrelated state,
        // while retaining the old fingerprint.
        let other = project(&events[..2], &ctx).unwrap().into_snapshot();
        let mut parts = snapshot.into_parts();
        parts.state = other.into_parts().state;
        let tampered = Snapshot::restore(parts);

        assert!(matches!(
            advance(&tampered, &events, &ctx),
            Err(ProjectionError::SnapshotFingerprintMismatch)
        ));
    }

    #[test]
    fn an_event_inserted_before_the_snapshot_boundary_forces_a_full_recompute() {
        // The most dangerous case: an event arrived retroactively and was inserted
        // BEFORE the snapshot boundary. It changes neither the boundary nor the state
        // of the snapshot, so a naive “take everything after the boundary” would silently
        // miss it—and produce plausible but incorrect
        // balances. The core must detect this.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events, &ctx).unwrap().into_snapshot();

        // A forgotten deposit dated in the middle of the already folded period.
        let forgotten = event_with(
            account,
            date!(2025 - 01 - 02),
            99,
            EventKind::CashIn { amount: rub(777) },
            vec![Leg::cash(account, rub(777))],
        );
        let mut with_backdated = events.clone();
        with_backdated.push(forgotten);

        let error = advance(&snapshot, &with_backdated, &ctx).unwrap_err();
        assert!(
            matches!(error, ProjectionError::PrefixChanged { .. }),
            "expected PrefixChanged, got {error}"
        );

        // A full recomputation sees the forgotten event.
        let recomputed = project(&with_backdated, &ctx).unwrap();
        assert_eq!(
            recomputed
                .state()
                .balances()
                .cash(account, CurrencyCode::Rub),
            Some(rub(10_000 + 20_000 + 30_000 + 40_000 + 777))
        );
    }

    #[test]
    fn reversing_an_event_inside_the_snapshot_forces_a_full_recompute() {
        // A reversal removes an event from the active set, which means it
        // changes the already folded prefix. It cannot be subtracted from the aggregate,
        // and pretending otherwise means silently losing the correction.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events[..2], &ctx).unwrap().into_snapshot();

        let mut with_reversal = events.clone();
        with_reversal[3].relation = Relation::Reversal {
            target: events[0].id,
        };

        assert!(matches!(
            advance(&snapshot, &with_reversal, &ctx),
            Err(ProjectionError::PrefixChanged { .. })
        ));
    }

    #[test]
    fn a_snapshot_of_another_contour_is_rejected() {
        let account = AccountId::new_random();
        let rules = RuleRegistry::with_defaults();
        let first = contour_of(account);
        let second = contour_of(account);
        let events = deposits(account);
        let snapshot = project(
            &events,
            &ProjectionContext {
                contour: &first,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        )
        .unwrap()
        .into_snapshot();

        assert!(matches!(
            advance(
                &snapshot,
                &events,
                &ProjectionContext {
                    contour: &second,
                    rules: &rules,
                    lot_rule: LotRuleVersion(1),
                }
            ),
            Err(ProjectionError::SnapshotContourMismatch { .. })
        ));
    }

    #[test]
    fn a_leg_contradicting_the_event_never_reaches_the_projection() {
        // An event whose leg contradicts its event type is rejected
        // by the input gate—before it enters the append-only log.
        // The invariant “the sum of lots equals the position” remains the second line of defense:
        // it catches the same mismatch if it comes from storage
        // populated by bypassing ingestion (§15.2).
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let event = event_with(
            account,
            date!(2025 - 04 - 01),
            1,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(1_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, rub(-1_000_000)),
                // The leg says 90 securities, while the event type says 100.
                Leg::security(account, CustodyId::new_random(), instrument, qty(90)),
            ],
        );

        // The event-shape gate rejects the contradiction on its own.
        assert!(matches!(
            event.validate_structure(),
            Err(crate::event::EventValidationError::LegDoesNotMatchEvent { .. })
        ));

        // Nor is a projection built from such an event: it rechecks the shape
        // because it is not required to trust what is stored.
        let error = project(&[event], &ctx).unwrap_err();
        assert!(error.is_invariant_violation(), "{error}");
        assert_eq!(error.code(), "invariant");
    }
    #[test]
    fn mixed_day_projection_is_reproducible() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let day = date!(2026 - 03 - 01);
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };

        let mut buy = event_with(
            account,
            day,
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(1),
                gross: rub(100),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, rub(-100)),
                Leg::security(account, custody, instrument, qty(1)),
            ],
        );
        buy.order = EffectiveOrder::with_source_time(day, time!(09:00:00), 1);

        let mut sell = event_with(
            account,
            day,
            2,
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(1),
                gross: rub(120),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, rub(120)),
                Leg::security(account, custody, instrument, qty(-1)),
            ],
        );
        sell.order = EffectiveOrder::with_source_time(day, time!(10:00:00), 2);

        let mut coupon = event_with(
            account,
            day,
            3,
            EventKind::Income {
                instrument: Some(instrument),
                gross: rub(10),
                kind: Some(IncomeKind::Coupon),
            },
            vec![Leg::cash(account, rub(10))],
        );
        coupon.order = EffectiveOrder::new(day, 3);

        let events = vec![buy, sell, coupon];
        let mut reversed = events.clone();
        reversed.reverse();
        assert_eq!(
            project(&events, &ctx).unwrap().snapshot().fingerprint(),
            project(&reversed, &ctx).unwrap().snapshot().fingerprint()
        );
    }
    #[test]
    fn an_old_event_without_source_time_still_projects() {
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let event = deposits(account).remove(0);
        let mut value = serde_json::to_value(&event).unwrap();
        value
            .get_mut("order")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("source_time");
        let restored = serde_json::from_value(value).unwrap();

        let projection = project(&[restored], &ctx).unwrap();
        assert_eq!(
            projection
                .state()
                .balances()
                .cash(account, CurrencyCode::Rub),
            Some(rub(10_000))
        );
    }
}
