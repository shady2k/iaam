//! Repair T4 trades whose custody was fabricated from the account identifier.

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::correction::resolve;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId};
use iaam_ingest::dedup::IdentityScope;
use sha2::{Digest, Sha256};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{Principal, Recorded};

/// The preflight case found for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyRepairCase {
    /// Affected trades exist and an unrevoked broker access can restore them.
    AffectedWithLiveAccess,
    /// Affected trades exist, but no unrevoked broker access can restore them.
    AffectedWithoutLiveAccess,
    /// No effective affected trades remain to repair.
    NothingAffected,
}

/// Result of repairing one account's account-derived custody facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustodyRepairOutcome {
    pub case: CustodyRepairCase,
    pub affected_trades: usize,
    pub already_reversed: usize,
    pub written: usize,
}

/// Retract affected trades so the account can be synchronised again.
///
/// Reversals are deliberately not followed by synchronisation: the route owns that
/// separate, externally observable operation. When no live access exists, the caller
/// must explicitly acknowledge that the re-import may not be possible.
pub async fn repair_custody(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
    acknowledge_without_live_access: bool,
) -> Result<CustodyRepairOutcome, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".to_owned(),
            expected: "permission to repair account custody".to_owned(),
            actual: principal.scope.code().to_owned(),
        });
    }

    let events = services
        .store
        .load_events_through(principal.owner, time::Date::MAX)
        .await?;
    let effective = resolve(&events).map_err(AppError::Correction)?;
    let targets: Vec<&Event> = effective
        .into_iter()
        .filter(|event| crate::sync::is_affected_trade(event, account))
        .collect();

    let by_id: BTreeMap<EventId, &Event> = events.iter().map(|event| (event.id, event)).collect();
    let reversal_targets: BTreeSet<EventId> = events
        .iter()
        .filter_map(|event| match event.relation {
            Relation::Reversal { target } => Some(target),
            Relation::None | Relation::Replacement { .. } => None,
        })
        .collect();
    // This is intentionally a relation scan, not a duplicate lookup: an idempotency
    // hit does not prove that the existing event reverses the intended target.
    let mut already_reversed = reversal_targets
        .iter()
        .filter(|target| {
            by_id
                .get(target)
                .is_some_and(|event| crate::sync::is_affected_trade(event, account))
        })
        .count();

    if targets.is_empty() {
        return Ok(CustodyRepairOutcome {
            case: CustodyRepairCase::NothingAffected,
            affected_trades: 0,
            already_reversed,
            written: 0,
        });
    }

    // This coarse check asks whether the owner has any live broker access. It cannot
    // match a revoked access's SourceId to a fact's interval, so it is not a guarantee
    // that a particular access can restore a particular trade.
    let has_live_access = services
        .broker
        .list_access(principal.owner)
        .await?
        .iter()
        .any(|access| access.revoked_at.is_none());
    let case = if has_live_access {
        CustodyRepairCase::AffectedWithLiveAccess
    } else {
        CustodyRepairCase::AffectedWithoutLiveAccess
    };
    if !has_live_access && !acknowledge_without_live_access {
        return Ok(CustodyRepairOutcome {
            case,
            affected_trades: targets.len(),
            already_reversed,
            written: 0,
        });
    }

    let affected_trades = targets.len();
    let mut written = 0;
    for original in targets {
        let reversal = reversal_for(original);
        let target = original.id;
        let recorded = crate::scenarios::ingest::append_checked(
            services,
            vec![reversal],
            IdentityScope::Source,
        )
        .await?;
        match recorded.first() {
            Some(Recorded::Inserted { .. }) => written += 1,
            Some(Recorded::Duplicate { existing }) => {
                let current = services
                    .store
                    .load_events_through(principal.owner, time::Date::MAX)
                    .await?;
                let is_matching_reversal = current.iter().any(|event| {
                    event.id == *existing && event.relation == (Relation::Reversal { target })
                });
                if !is_matching_reversal {
                    return Err(AppError::Conflict {
                        what: format!(
                            "custody repair idempotency key for target {target:?} is occupied by an unrelated event"
                        ),
                    });
                }
                already_reversed += 1;
            }
            None => {
                return Err(AppError::Store(
                    "custody repair append returned no record".to_owned(),
                ));
            }
        }
    }

    Ok(CustodyRepairOutcome {
        case,
        affected_trades,
        already_reversed,
        written,
    })
}

fn reversal_for(original: &Event) -> Event {
    let idempotency_key = format!(
        "custody-repair/{}/{}",
        original.account.inner(),
        original.id.inner()
    );
    let digest = Sha256::digest(idempotency_key.as_bytes());
    let raw_hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let raw_hash = RawHash::parse(&raw_hash)
        .unwrap_or_else(|| unreachable!("SHA-256 output is always a valid raw hash"));
    let order = original.order.source_time().map_or_else(
        || EffectiveOrder::new(original.order.date(), 0),
        |source_time| EffectiveOrder::with_source_time(original.order.date(), source_time, 0),
    );

    Event {
        id: EventId::new_random(),
        // The version describes the software that wrote the fact, and this fact is
        // written now. Copying the original's would claim the reversal was recorded
        // by whatever understood the journal back then.
        schema_version: SCHEMA_VERSION,
        owner: original.owner,
        account: original.account,
        kind: original.kind.clone(),
        dates: original.dates,
        order,
        legs: original.legs.clone(),
        provenance: Provenance::new(
            original.provenance.source(),
            raw_hash,
            ParserVersion("custody-repair/1".to_owned()),
        ),
        relation: Relation::Reversal {
            target: original.id,
        },
        confidence: original.confidence,
        idempotency_key: Some(idempotency_key),
    }
}
