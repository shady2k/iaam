//! Corrections (§4.8): the owner's route from a request into the journal.
//!
//! The core already decides what a correction *means*.
//! [`iaam_core::event::correction::resolve`] excludes a reversed or replaced
//! event from the effective set, and refuses a target the journal does not
//! contain or a second replacement of the same event. Nothing here re-decides
//! any of that, and there is deliberately no second notion of an effective set:
//! this scenario turns a request into candidate events, asks `resolve` whether
//! the journal would still resolve **with those candidates in it**, and writes
//! them only then.
//!
//! The order matters because the journal is append-only. A correction `resolve`
//! would reject cannot be taken back once written: it would fail every later
//! read of the journal rather than only the request that introduced it, and
//! every report the owner has would stop being computable.

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::dates::EffectiveOrder;
use iaam_core::event::correction::{CorrectionError, resolve};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, ImportId, PrincipalId, SourceId};
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{SubmittedOperation, Verdict, normalize};
use sha2::{Digest, Sha256};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{Principal, Recorded};

/// Parser version stamped on every correction fact.
///
/// A correction is produced by this code, not by whatever parsed the row it
/// corrects, and provenance answers «which software wrote this fact» (§4.1).
pub const CORRECTION_PARSER_VERSION: &str = "correction/1";

/// Channel under which correction facts are recorded.
///
/// A correction did not arrive through the file that carried the fact it
/// corrects; it arrived because the owner said so. Recording it under the
/// corrected import's own source would also put a replacement inside the very
/// set that [`correct_import`] sweeps, so correcting an import twice would
/// retract the corrections made the first time.
pub const CORRECTION_CHANNEL: &str = "correction";

/// What the owner asks a correction to do to one event.
///
/// The replacement carries the operation that supersedes the target rather than
/// a patch of it: a fact is submitted whole, and a partial correction would make
/// the journal hold a value nobody ever stated.
#[derive(Debug, Clone, PartialEq)]
pub enum CorrectionRequest {
    /// Retract the target. It stays in the journal and stops being effective.
    Reversal { target: EventId },
    /// Supersede the target with the operation submitted beside it.
    ///
    /// Boxed because the operation is far larger than an identifier, and an
    /// enum sized for its largest variant would be copied on every reversal.
    Replacement {
        target: EventId,
        operation: Box<SubmittedOperation>,
    },
}

/// Which import a correction retracts.
///
/// Two variants rather than one identity, because the journal holds two kinds
/// of row. A submission that named its import stamped that name on every row
/// it carried, and is retracted by name. A submission that named none — every
/// row recorded before an import could be named, and every channel that
/// declares no source at all — left nothing finer than the source it arrived
/// through, so the only honest thing to retract is all of it. Saying that in
/// the type stops the second case from being reached by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportTarget {
    /// One named import, and nothing else that came through its source.
    Named { source: SourceId, import: ImportId },
    /// Everything that arrived through one source without naming an import.
    Unnamed { source: SourceId },
}

impl ImportTarget {
    /// The source the rows arrived through, whether or not they named an
    /// import. Reported back to the caller, never the thing matched on.
    #[must_use]
    pub const fn source(self) -> SourceId {
        match self {
            Self::Named { source, .. } | Self::Unnamed { source } => source,
        }
    }

    #[must_use]
    const fn import(self) -> Option<ImportId> {
        match self {
            Self::Named { import, .. } => Some(import),
            Self::Unnamed { .. } => None,
        }
    }

    /// Does this event belong to the import being retracted?
    ///
    /// A named target matches on the import alone: the import already implies
    /// its source, and a row cannot carry one import under two sources. An
    /// unnamed target matches rows that named no import **and** arrived through
    /// the named source — without the first half it would sweep every named
    /// import of that source as well, which is the defect this replaced.
    fn covers(self, event: &Event) -> bool {
        match self {
            Self::Named { import, .. } => event.provenance.import() == Some(import),
            Self::Unnamed { source } => {
                event.provenance.import().is_none() && event.provenance.source() == source
            }
        }
    }
}

/// Outcome of correcting one whole declared import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportCorrectionOutcome {
    /// The source identity the retracted rows arrived through.
    pub source: SourceId,
    /// The import identity the correction was keyed on, absent when the
    /// correction named the unnamed rows of a source.
    pub import: Option<ImportId>,
    /// Effective events this import still had in the journal.
    pub affected: usize,
    /// Reversed by an earlier correction: a repeat run reports these and
    /// writes nothing.
    pub already_reversed: usize,
    /// Reversal facts written by this run.
    pub written: usize,
}

/// Correct the events the owner names, one correction fact each.
///
/// All or nothing: unlike an import, whose rows are unknown to the caller and
/// therefore judged one at a time (§10.1), a correction batch is one deliberate
/// act. A request that names an event the journal does not hold is a mistake
/// about the journal, and applying the half of it that happened to resolve
/// would leave the owner unable to say which half.
pub async fn correct_events(
    services: &AppServices,
    principal: &Principal,
    acknowledge_retraction: bool,
    corrections: &[CorrectionRequest],
) -> Result<Vec<Verdict>, AppError> {
    may_correct(principal)?;
    acknowledged(acknowledge_retraction)?;
    if corrections.is_empty() {
        return Err(AppError::Invalid {
            field: "corrections".to_owned(),
            expected: "at least one correction".to_owned(),
            actual: "an empty list".to_owned(),
        });
    }

    let events = load_journal(services, principal).await?;

    let mut candidates = Vec::with_capacity(corrections.len());
    {
        let by_id: BTreeMap<EventId, &Event> =
            events.iter().map(|event| (event.id, event)).collect();
        for (index, correction) in corrections.iter().enumerate() {
            candidates.push(candidate_for(principal, &by_id, index, correction)?);
        }
    }

    let candidates = checked_against_resolve(events, candidates)?;
    let recorded =
        crate::scenarios::ingest::append_checked(services, candidates, IdentityScope::Source)
            .await?;
    Ok(recorded
        .into_iter()
        .map(|outcome| match outcome {
            Recorded::Inserted { id } => Verdict::Provisional { event: id },
            Recorded::Duplicate { existing } => Verdict::Duplicate { existing },
        })
        .collect())
}

/// Retract every effective event one declared import left in the journal.
///
/// The key is the declared import, because that is the handle a caller actually
/// holds after submitting a batch: the event identifiers were minted by the
/// server and the row identifiers belong to the file. Deriving that identity
/// from the account, the channel and the label belongs to the caller, which is
/// where the declaration was made; this takes the identity itself, so nothing
/// here can disagree with what ingestion wrote.
///
/// It is the import and not the source, because a source is not an import: two
/// months of one account exported the same way arrive through one source, and
/// keying on it would retract both when the caller named one.
///
/// One request, and one reversal **fact** per reversed event: a flag
/// saying «this import is retracted» would be a second, non-journal notion of
/// what is effective, and the append-only journal would no longer be the whole
/// account of what the owner knows.
///
/// **Who may call it.** The owner, for any import. An agent, for an import it
/// declared itself and nothing has been built on — see
/// [`undoes_only_its_own_declaration`] for the four conditions and for why they
/// are decided here rather than at the transport. That is narrower than
/// [`correct_events`] beside it, which stays owner-only, and the difference is
/// the doctrine rather than an inconsistency: taking back your own declaration
/// returns the journal to the state before you acted, and naming an event of the
/// owner's to reverse is a judgement about his history.
pub async fn correct_import(
    services: &AppServices,
    principal: &Principal,
    acknowledge_retraction: bool,
    target: ImportTarget,
) -> Result<ImportCorrectionOutcome, AppError> {
    may_retract_an_import(principal)?;
    acknowledged(acknowledge_retraction)?;

    let events = load_journal(services, principal).await?;

    let (targets, already_reversed) = {
        let effective = resolve(&events).map_err(AppError::Correction)?;
        let targets: Vec<Event> = effective
            .iter()
            .filter(|event| target.covers(event))
            .map(|event| (*event).clone())
            .collect();
        // Checked against the journal this correction is computed from, and not
        // in a step before it: see [`undoes_only_its_own_declaration`].
        if !principal.scope.may_administer() {
            undoes_only_its_own_declaration(principal, target, &events, &effective, &targets)?;
        }

        let by_id: BTreeMap<EventId, &Event> =
            events.iter().map(|event| (event.id, event)).collect();
        // A relation scan rather than an idempotency lookup: an occupied key
        // does not prove that the event holding it reverses this import. The
        // targets are a set because two reversals of one event are two facts
        // but one retraction.
        let reversed: BTreeSet<EventId> = events
            .iter()
            .filter_map(|event| match event.relation {
                Relation::Reversal { target } => Some(target),
                Relation::None | Relation::Replacement { .. } => None,
            })
            .collect();
        let already_reversed = reversed
            .iter()
            .filter(|target_event| {
                by_id
                    .get(target_event)
                    .is_some_and(|event| target.covers(event))
            })
            .count();
        (targets, already_reversed)
    };

    let affected = targets.len();
    if targets.is_empty() {
        return Ok(ImportCorrectionOutcome {
            source: target.source(),
            import: target.import(),
            affected,
            already_reversed,
            written: 0,
        });
    }

    let candidates: Vec<Event> = targets.iter().map(reversal_for).collect();
    let candidates = checked_against_resolve(events, candidates)?;
    let recorded =
        crate::scenarios::ingest::append_checked(services, candidates, IdentityScope::Source)
            .await?;

    let mut written = 0;
    let mut already_reversed = already_reversed;
    for outcome in recorded {
        match outcome {
            Recorded::Inserted { .. } => written += 1,
            // The key is derived from the target, so an occupied one means this
            // event was reversed before. `refuse_occupied_keys` has already
            // ruled out the case where something else holds it.
            Recorded::Duplicate { .. } => already_reversed += 1,
        }
    }

    Ok(ImportCorrectionOutcome {
        source: target.source(),
        import: target.import(),
        affected,
        already_reversed,
        written,
    })
}

/// Only the owner corrects the journal event by event.
///
/// A reversal rewrites what every downstream report says, and naming an
/// arbitrary event to reverse is a judgement about the owner's history: which
/// of the facts he holds should stop counting. Nothing about the caller's own
/// conduct bounds it, so nothing narrower than the owner's scope will do.
///
/// This is also why corrections do not ride the ingest transport, and that half
/// is unchanged by `iaam-rond`: [`crate::ports::Scope::may_submit`] admits an
/// agent, and a relation field on an ingest row would make every ingest handler
/// a retraction surface guarded by a per-row check that one input could forget.
/// A separate route with its own gate is the right shape; only the gate on
/// [`correct_import`] moved.
///
/// The reason that comment used to give — "the agent is an external client that
/// does not decide the portfolio's shape" — was already false when it was
/// written. Committing an import is open to the agent and rewrites every
/// downstream report, so the agent does decide the portfolio's shape by adding
/// to it; the gate closed only the safer direction, and left an agent that
/// finds by control total that it wrote nonsense with nothing to do but wake the
/// owner to undo the agent's own mistake.
fn may_correct(principal: &Principal) -> Result<(), AppError> {
    if principal.scope.may_administer() {
        Ok(())
    } else {
        Err(AppError::Invalid {
            field: "scope".to_owned(),
            expected: "owner permission to correct the journal".to_owned(),
            actual: principal.scope.code().to_owned(),
        })
    }
}

/// Who may ask to retract a whole declared import at all.
///
/// The floor, not the rule. It admits the owner and the agent and refuses a
/// read-only token, exactly as submission does; what an agent may retract is
/// then decided by [`undoes_only_its_own_declaration`] against the journal,
/// because that question cannot be answered from a scope alone.
fn may_retract_an_import(principal: &Principal) -> Result<(), AppError> {
    if principal.scope.may_submit() {
        Ok(())
    } else {
        Err(AppError::Invalid {
            field: "scope".to_owned(),
            expected: "permission to submit operations".to_owned(),
            actual: principal.scope.code().to_owned(),
        })
    }
}

/// The bound on an agent's retraction: it may take back its own declaration and
/// nothing else (`iaam-rond`).
///
/// **The doctrine.** Retracting an import you yourself declared returns the
/// journal to the state before you acted; no decision of the owner's is
/// reversed, because none was made. Retracting anything else is a judgement
/// about his history. The gate is therefore not "which route is this" but "what
/// does this act do to the owner's decisions", and the four conditions below are
/// that sentence made checkable.
///
/// 1. **The declaration named a label.** An unnamed target sweeps every row of
///    an account and channel that named no import — rows from before imports
///    could be named, and from channels that declare no source. That set is not
///    anybody's declaration, so no caller can claim it as its own.
/// 2. **Every event the target covers was submitted under this very credential.**
///    Not "under some agent token": a token is the finest identity there is (see
///    [`PrincipalId`]), and an import both the owner and the agent submitted
///    rows into is one the owner has a stake in. Absent provenance fails the
///    check rather than passing it — a fact recorded before anyone was written
///    down names no declarer, and reading that silence as "mine" would hand
///    every pre-existing import to whoever asked first.
/// 3. **Every event the target covers is still effective.** One already reversed
///    or replaced is one somebody has already ruled on: the transfer pairing
///    that superseded an outgoing leg, a correction the owner made by hand.
///    Sweeping the remainder would be finishing a judgement the agent did not
///    make. It also means a retraction is not idempotent for an agent: the
///    second call is refused, and the refusal says the rows are already
///    reversed, which is the true answer to "did my first call land".
/// 4. **Nothing has been reconciled against it.** A control assertion is a
///    journal fact (§10.3) that was compared with the projection over an
///    interval; retracting rows inside that interval changes what the comparison
///    was made against, and the reconciliation status the owner has already read
///    becomes a statement about a journal that no longer exists. An assertion
///    the target itself covers does not block: it is being retracted along with
///    the rows it was about.
///
/// **Why the check is here and not beside the caller.** Every one of these is a
/// predicate over the journal, and it is the same journal the reversal is
/// computed from — `load_journal` has already read it, `resolve` has already
/// folded it, and the relation scan below already walks it. Cost is therefore
/// one pass, not one query. The reason it must be here is stronger than cost: a
/// check made in a step before `correct_import` would run against its own read,
/// and between that read and the write the owner can record the very assertion
/// condition 4 exists to protect. The reversal would then be written against a
/// journal that no longer satisfies the precondition it was checked under. One
/// read, one decision.
fn undoes_only_its_own_declaration(
    principal: &Principal,
    target: ImportTarget,
    journal: &[Event],
    effective: &[&Event],
    effective_covered: &[Event],
) -> Result<(), AppError> {
    let Some(import) = target.import() else {
        return Err(AppError::Invalid {
            field: "source.label".to_owned(),
            expected: "the label this import was declared under: without one, the request \
                       reaches every row of the account and channel that named no \
                       import, which is nobody's declaration to take back"
                .to_owned(),
            actual: "absent".to_owned(),
        });
    };

    let declared_by = PrincipalId(principal.token_id);
    let covered: Vec<&Event> = journal
        .iter()
        .filter(|event| target.covers(event))
        .collect();
    let foreign = covered
        .iter()
        .filter(|event| event.provenance.declared_by() != Some(declared_by))
        .count();
    if foreign > 0 {
        return Err(AppError::Invalid {
            field: "source".to_owned(),
            expected: "an import every row of which this token declared".to_owned(),
            actual: format!(
                "import {} holds {foreign} of {} rows submitted under another \
                 credential, or under none recorded",
                import.inner(),
                covered.len()
            ),
        });
    }

    if covered.len() != effective_covered.len() {
        return Err(AppError::Invalid {
            field: "source".to_owned(),
            expected: "an import none of whose rows has been reversed or replaced".to_owned(),
            actual: format!(
                "{} of {} rows are no longer effective",
                covered.len() - effective_covered.len(),
                covered.len()
            ),
        });
    }

    let covered_ids: BTreeSet<EventId> = covered.iter().map(|event| event.id).collect();
    let accounts: BTreeSet<AccountId> = covered.iter().flat_map(|event| touched(event)).collect();
    let dates: BTreeSet<time::Date> = covered.iter().map(|event| event.order.date()).collect();
    for event in effective {
        let EventKind::ControlAssertion { period, .. } = &event.kind else {
            continue;
        };
        if covered_ids.contains(&event.id) || !accounts.contains(&event.account) {
            continue;
        }
        if dates.iter().any(|date| period.contains(*date)) {
            return Err(AppError::Invalid {
                field: "source".to_owned(),
                expected: "an import nothing has been reconciled against".to_owned(),
                actual: format!(
                    "control assertion {} covers {}..={} on an account these rows moved",
                    event.id.inner(),
                    period.from,
                    period.to
                ),
            });
        }
    }

    Ok(())
}

/// Every account an event moves, not only the one it is filed under.
///
/// A cash transfer is filed against one account and moves two, so an assertion
/// about the far side is reconciled against this row just as much. Widening the
/// set can only refuse more retractions, which is the direction a bound whose
/// evidence is incomplete should err in.
fn touched(event: &Event) -> BTreeSet<AccountId> {
    std::iter::once(event.account)
        .chain(event.legs.iter().map(|leg| leg.account))
        .collect()
}

/// A correction is stated, never implied.
///
/// Custody repair already demands an acknowledgement before retracting facts a
/// later synchronisation may not restore; the same is true of every correction,
/// and more besides: the retracted fact stops counting in reports the owner has
/// already read.
fn acknowledged(acknowledge_retraction: bool) -> Result<(), AppError> {
    if acknowledge_retraction {
        Ok(())
    } else {
        Err(AppError::Invalid {
            field: "acknowledge_retraction".to_owned(),
            expected: "true: a retracted fact stops counting in every report, and \
                       re-submitting the same rows does not bring it back"
                .to_owned(),
            actual: "false".to_owned(),
        })
    }
}

async fn load_journal(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<Event>, AppError> {
    services
        .store
        .load_events_through(principal.owner, time::Date::MAX)
        .await
}

/// Ask `resolve` whether the journal survives these candidates, then hand them
/// back for writing.
///
/// The journal is consumed rather than cloned: a correction of a large import
/// would otherwise copy every event the owner has in order to check a handful
/// of new ones.
fn checked_against_resolve(
    journal: Vec<Event>,
    candidates: Vec<Event>,
) -> Result<Vec<Event>, AppError> {
    let candidate_ids: Vec<EventId> = candidates.iter().map(|event| event.id).collect();
    let boundary = journal.len();
    let mut prospective = journal;
    prospective.extend(candidates);
    if let Err(error) = resolve(&prospective) {
        return Err(correction_refusal(&error, &candidate_ids));
    }
    let candidates = prospective.split_off(boundary);
    refuse_occupied_keys(&prospective, &candidates)?;
    Ok(candidates)
}

/// Name the correction `resolve` refused, or report a journal that did not
/// resolve before this request touched it.
///
/// The distinction is the whole point of this function. A request that would
/// break the journal is the caller's to fix and names the field they sent; a
/// journal that does not resolve on its own is our defect, carries no field
/// anyone could correct, and must not be reported as a bad request.
fn correction_refusal(error: &CorrectionError, candidates: &[EventId]) -> AppError {
    let (blamed, expected) = match error {
        CorrectionError::DanglingTarget { correction, target } => (
            Some((*correction, *target)),
            "an identifier of an event already in this owner's journal",
        ),
        CorrectionError::ConflictingReplacements {
            target,
            first,
            second,
        } => {
            let blamed = [*first, *second]
                .into_iter()
                .find(|id| candidates.contains(id));
            (
                blamed.map(|correction| (correction, *target)),
                "an event no other replacement already supersedes",
            )
        }
        // Two events with one identifier cannot come from a request: every
        // candidate is minted here with a fresh identifier.
        CorrectionError::DuplicateEvent { .. } => (None, ""),
    };

    let Some((correction, target)) = blamed else {
        return AppError::Correction(error.clone());
    };
    let Some(index) = candidates.iter().position(|id| *id == correction) else {
        return AppError::Correction(error.clone());
    };
    AppError::Invalid {
        field: format!("corrections[{index}].target"),
        expected: expected.to_owned(),
        actual: target.inner().to_string(),
    }
}

/// Refuse a correction whose idempotency key is held by an unrelated event.
///
/// Without this the store would answer «duplicate», which the caller reads as
/// «already corrected», while the correction was in fact swallowed by an
/// ordinary operation that happened to be submitted under that key. Custody
/// repair makes the same check for the same reason.
fn refuse_occupied_keys(journal: &[Event], candidates: &[Event]) -> Result<(), AppError> {
    let occupied: BTreeMap<&str, &Event> = journal
        .iter()
        .filter_map(|event| Some((event.idempotency_key.as_deref()?, event)))
        .collect();
    for candidate in candidates {
        let Some(key) = candidate.idempotency_key.as_deref() else {
            continue;
        };
        let Some(existing) = occupied.get(key) else {
            continue;
        };
        if existing.relation != candidate.relation {
            return Err(AppError::Conflict {
                what: format!(
                    "correction idempotency key {key} is held by event {:?}, which is not this correction",
                    existing.id
                ),
            });
        }
    }
    Ok(())
}

/// Build the event one correction writes.
fn candidate_for(
    principal: &Principal,
    by_id: &BTreeMap<EventId, &Event>,
    index: usize,
    correction: &CorrectionRequest,
) -> Result<Event, AppError> {
    match correction {
        CorrectionRequest::Reversal { target } => {
            let original = by_id
                .get(target)
                .ok_or_else(|| unknown_target(index, *target))?;
            if matches!(original.relation, Relation::Reversal { .. }) {
                return Err(AppError::Invalid {
                    field: format!("corrections[{index}].target"),
                    expected: "an event carrying a fact: a reversal is never part of the \
                               effective set, so reversing one changes nothing"
                        .to_owned(),
                    actual: target.inner().to_string(),
                });
            }
            Ok(reversal_for(original))
        }
        CorrectionRequest::Replacement { target, operation } => {
            // Looked up here rather than left to `resolve`, so a replacement of
            // an event that does not exist is refused before the submitted
            // operation is parsed: a rejection naming a field of the
            // replacement would send the caller to fix the wrong thing.
            if !by_id.contains_key(target) {
                return Err(unknown_target(index, *target));
            }
            let source = SourceId::declared(principal.owner, operation.account, CORRECTION_CHANNEL);
            let normalized = normalize(
                operation,
                NormalizationContext {
                    owner: principal.owner,
                    source,
                },
            )
            .map_err(|rejection| AppError::Invalid {
                field: format!("corrections[{index}].operation.{}", rejection.field),
                expected: rejection.expected,
                actual: rejection.actual,
            })?;
            Ok(Event {
                relation: Relation::Replacement { target: *target },
                ..normalized.event
            })
        }
    }
}

fn unknown_target(index: usize, target: EventId) -> AppError {
    AppError::Invalid {
        field: format!("corrections[{index}].target"),
        expected: "an identifier of an event in this owner's journal".to_owned(),
        actual: target.inner().to_string(),
    }
}

/// The reversal fact for one event.
///
/// Kind and legs are copied because the event must pass structural validation
/// before it is written, and a reversal that carried no legs would be a
/// malformed event in an append-only journal. They never post: `resolve` drops
/// every reversal from the effective set.
fn reversal_for(original: &Event) -> Event {
    let idempotency_key = format!("correction/reversal/{}", original.id.inner());
    let raw_hash = hash_of(&idempotency_key);
    // Sequence zero, like custody repair: the store assigns the real one within
    // the day in the same transaction as the insert (§4.8).
    let order = original.order.source_time().map_or_else(
        || EffectiveOrder::new(original.order.date(), 0),
        |source_time| EffectiveOrder::with_source_time(original.order.date(), source_time, 0),
    );

    Event {
        id: EventId::new_random(),
        // The version describes the software that wrote the fact, and this fact
        // is written now. Copying the original's would claim the reversal was
        // recorded by whatever understood the journal back then.
        schema_version: SCHEMA_VERSION,
        owner: original.owner,
        account: original.account,
        kind: original.kind.clone(),
        dates: original.dates,
        order,
        legs: original.legs.clone(),
        provenance: Provenance::new(
            SourceId::declared(original.owner, original.account, CORRECTION_CHANNEL),
            raw_hash,
            ParserVersion(CORRECTION_PARSER_VERSION.to_owned()),
        ),
        relation: Relation::Reversal {
            target: original.id,
        },
        confidence: original.confidence,
        idempotency_key: Some(idempotency_key),
    }
}

fn hash_of(key: &str) -> RawHash {
    let digest = Sha256::digest(key.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    RawHash::parse(&hex)
        .unwrap_or_else(|| unreachable!("SHA-256 output is always a valid raw hash"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::dates::{CashPostedDate, EventDates};
    use iaam_core::event::Confidence;
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::ids::{AccountId, OwnerId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use time::macros::date;

    fn owner() -> OwnerId {
        OwnerId(uuid::Uuid::from_u128(1))
    }

    fn account() -> AccountId {
        AccountId(uuid::Uuid::from_u128(2))
    }

    fn deposit(id: u128, source: SourceId) -> Event {
        let amount = Money::new(PostedMinor::new(1_000), CurrencyCode::Rub);
        Event {
            id: EventId(uuid::Uuid::from_u128(id)),
            schema_version: SCHEMA_VERSION,
            owner: owner(),
            account: account(),
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 01))),
            order: EffectiveOrder::new(date!(2026 - 03 - 01), 1),
            legs: vec![Leg::cash(account(), amount)],
            provenance: Provenance::new(
                source,
                RawHash::parse(&"a".repeat(64)).expect("hash"),
                ParserVersion("test".to_owned()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    /// A transfer is filed against one account and moves two, and an assertion
    /// about the far side is reconciled against the row just as much.
    ///
    /// Asserted at the helper rather than through the API because the API cannot
    /// reach it: the far account of a transfer is not what the request names, so
    /// a bound reading only `event.account` would pass every end-to-end test and
    /// still let an agent retract rows a reconciled balance was computed from.
    #[test]
    fn the_accounts_an_event_touches_include_the_far_side_of_a_transfer() {
        let far = AccountId(uuid::Uuid::from_u128(3));
        let amount = Money::new(PostedMinor::new(1_000), CurrencyCode::Rub);
        let mut event = deposit(9, SourceId::new_random());
        event.legs = vec![Leg::cash(account(), amount), Leg::cash(far, amount)];

        let touched = touched(&event);

        assert!(
            touched.contains(&account()),
            "the account it is filed under"
        );
        assert!(touched.contains(&far), "and the one its other leg moves");
    }

    #[test]
    fn a_reversal_keeps_the_facts_of_its_target_and_names_this_build() {
        let original = deposit(1, SourceId::new_random());
        let reversal = reversal_for(&original);

        assert_eq!(
            reversal.relation,
            Relation::Reversal {
                target: original.id
            }
        );
        assert_ne!(reversal.id, original.id, "a new fact, not the old one");
        assert_eq!(reversal.kind, original.kind);
        assert_eq!(reversal.legs, original.legs);
        assert_eq!(reversal.account, original.account);
        assert_eq!(reversal.schema_version, SCHEMA_VERSION);
        assert_eq!(
            reversal.provenance.parser_version(),
            &ParserVersion(CORRECTION_PARSER_VERSION.to_owned())
        );
        assert_eq!(
            reversal.provenance.source(),
            SourceId::declared(owner(), account(), CORRECTION_CHANNEL),
            "the correction arrived through the correction channel, not the import"
        );
        assert_eq!(
            reversal.idempotency_key.as_deref(),
            Some(format!("correction/reversal/{}", original.id.inner()).as_str())
        );
        assert!(
            reversal.validate_structure().is_ok(),
            "a reversal must be writable into an append-only journal"
        );
    }

    #[test]
    fn reversing_the_same_event_twice_produces_the_same_key() {
        let original = deposit(1, SourceId::new_random());
        assert_eq!(
            reversal_for(&original).idempotency_key,
            reversal_for(&original).idempotency_key,
            "a repeat correction must deduplicate rather than write a second fact"
        );
    }

    #[test]
    fn resolve_refuses_a_dangling_target_and_the_refusal_names_the_correction() {
        let missing = EventId(uuid::Uuid::from_u128(99));
        let mut orphan = deposit(1, SourceId::new_random());
        orphan.id = EventId(uuid::Uuid::from_u128(7));
        orphan.relation = Relation::Reversal { target: missing };

        let error = checked_against_resolve(Vec::new(), vec![orphan.clone()])
            .expect_err("a target that does not exist cannot be written");
        let AppError::Invalid {
            field,
            actual,
            expected,
        } = error
        else {
            panic!("a bad request, not a broken journal");
        };
        assert_eq!(field, "corrections[0].target");
        assert_eq!(actual, missing.inner().to_string());
        assert!(expected.contains("journal"), "{expected}");
    }

    #[test]
    fn a_journal_that_did_not_resolve_before_the_request_is_not_the_callers_fault() {
        // The dangling reversal is already in the journal; the request adds a
        // correction that is itself sound. Blaming the caller's field would send
        // them to fix an identifier they never sent.
        let mut broken = deposit(1, SourceId::new_random());
        broken.relation = Relation::Reversal {
            target: EventId(uuid::Uuid::from_u128(99)),
        };
        let sound = deposit(2, SourceId::new_random());
        let candidate = reversal_for(&sound);

        let error = checked_against_resolve(vec![broken, sound], vec![candidate])
            .expect_err("the journal does not resolve");
        assert!(
            matches!(error, AppError::Correction(_)),
            "expected a broken journal, got {error:?}"
        );
    }

    #[test]
    fn a_second_replacement_of_one_event_names_the_correction_that_conflicts() {
        let source = SourceId::new_random();
        let original = deposit(1, source);
        let mut first = deposit(2, source);
        first.relation = Relation::Replacement {
            target: original.id,
        };
        let mut second = deposit(3, source);
        second.relation = Relation::Replacement {
            target: original.id,
        };

        let error = checked_against_resolve(vec![original.clone(), first], vec![second])
            .expect_err("one event cannot be replaced twice");
        let AppError::Invalid { field, actual, .. } = error else {
            panic!("a bad request, not a broken journal");
        };
        assert_eq!(field, "corrections[0].target");
        assert_eq!(actual, original.id.inner().to_string());
    }

    #[test]
    fn a_correction_key_held_by_an_unrelated_event_is_a_conflict() {
        let original = deposit(1, SourceId::new_random());
        let candidate = reversal_for(&original);
        let mut squatter = deposit(2, SourceId::new_random());
        squatter.idempotency_key = candidate.idempotency_key.clone();

        let error = checked_against_resolve(vec![original, squatter], vec![candidate])
            .expect_err("the key is held by an ordinary operation");
        assert!(
            matches!(error, AppError::Conflict { .. }),
            "expected a conflict, got {error:?}"
        );
    }

    #[test]
    fn a_named_import_covers_its_own_rows_and_no_others() {
        let source = SourceId::declared(owner(), account(), "file");
        let january = ImportId::declared(owner(), account(), "file", "january");
        let february = ImportId::declared(owner(), account(), "file", "february");

        let mut in_january = deposit(1, source);
        in_january.provenance = in_january.provenance.clone().with_import(january);
        let mut in_february = deposit(2, source);
        in_february.provenance = in_february.provenance.clone().with_import(february);
        let unnamed = deposit(3, source);

        let target = ImportTarget::Named {
            source,
            import: february,
        };
        assert!(target.covers(&in_february));
        assert!(
            !target.covers(&in_january),
            "retracting one import must not reach another through the same source"
        );
        assert!(
            !target.covers(&unnamed),
            "rows that named no import are not part of a named one"
        );
        assert_eq!(target.source(), source);
        assert_eq!(target.import(), Some(february));
    }

    #[test]
    fn the_unnamed_target_takes_only_rows_that_named_no_import() {
        let source = SourceId::declared(owner(), account(), "file");
        let other_source = SourceId::declared(owner(), account(), "paste");
        let import = ImportId::declared(owner(), account(), "file", "january");

        let mut named = deposit(1, source);
        named.provenance = named.provenance.clone().with_import(import);
        let unnamed = deposit(2, source);
        let elsewhere = deposit(3, other_source);

        let target = ImportTarget::Unnamed { source };
        assert!(target.covers(&unnamed));
        assert!(
            !target.covers(&named),
            "an unlabelled retraction must not sweep the imports that were labelled"
        );
        assert!(!target.covers(&elsewhere));
        assert_eq!(target.import(), None);
    }

    #[test]
    fn an_acknowledgement_is_required_and_names_its_own_field() {
        let AppError::Invalid { field, actual, .. } =
            acknowledged(false).expect_err("a bare call is refused")
        else {
            panic!("expected an invalid request");
        };
        assert_eq!(field, "acknowledge_retraction");
        assert_eq!(actual, "false");
        assert!(acknowledged(true).is_ok());
    }

    #[test]
    fn only_the_owner_corrects_the_journal() {
        use crate::ports::Scope;

        for scope in [Scope::Agent, Scope::ReadOnly] {
            let principal = Principal {
                token_id: uuid::Uuid::from_u128(3),
                owner: owner(),
                scope,
            };
            let AppError::Invalid { field, actual, .. } =
                may_correct(&principal).expect_err("only the owner may correct")
            else {
                panic!("expected an invalid request");
            };
            assert_eq!(field, "scope");
            assert_eq!(actual, scope.code());
        }

        assert!(
            may_correct(&Principal {
                token_id: uuid::Uuid::from_u128(3),
                owner: owner(),
                scope: Scope::Owner,
            })
            .is_ok()
        );
    }
}
