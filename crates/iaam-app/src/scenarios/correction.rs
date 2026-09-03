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
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{EventId, ImportId, SourceId};
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
pub async fn correct_import(
    services: &AppServices,
    principal: &Principal,
    acknowledge_retraction: bool,
    target: ImportTarget,
) -> Result<ImportCorrectionOutcome, AppError> {
    may_correct(principal)?;
    acknowledged(acknowledge_retraction)?;

    let events = load_journal(services, principal).await?;

    let (targets, already_reversed) = {
        let effective = resolve(&events).map_err(AppError::Correction)?;
        let targets: Vec<Event> = effective
            .into_iter()
            .filter(|event| target.covers(event))
            .cloned()
            .collect();

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

/// Only the owner corrects the journal.
///
/// A reversal rewrites what every downstream report says, and the agent is an
/// external client that does not decide the portfolio's shape. This is why
/// corrections do not ride the ingest transport: `Scope::may_submit` admits an
/// agent, and a relation field on an ingest row would make every ingest handler
/// a retraction surface guarded by a per-row check that one input could forget.
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
