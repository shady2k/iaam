//! Operation ingestion.

use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::{Event, SCHEMA_VERSION};
use iaam_core::ids::{ImportId, ImportSessionId, PrincipalId, SourceId};
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{
    JournalEventEnrichment, JournalFact, Rejection, SubmittedJournalEvent, SubmittedOperation,
    Verdict, normalize, normalize_journal_event,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::AppServices;
use crate::error::AppError;
use crate::market_candidate::MOEX_ISS_SOURCE_ID;
use crate::ports::{Principal, Recorded};

/// Where one row came from and which submission carried it.
///
/// The two travel together because they are one answer: the source says «where
/// do these rows come from» and is what deduplication is scoped by, the import
/// says «which submission carried this one» and is what a retraction is keyed
/// on. Both are derived from a declaration — see [`SourceId::declared`] and
/// [`ImportId::declared`] — and neither is ever minted at random, because a
/// caller holds no server-assigned handle after a submission and could then
/// never name its own rows again.
///
/// **Carried per row rather than hoisted over the batch.** A source is keyed on
/// one account, and a channel may name an account per row: the CSV format does,
/// so one file can carry two of the owner's accounts. Folding them into one
/// source would let a row of the first deduplicate against a row of the second.
/// A channel whose declaration names one account for the whole batch builds this
/// once and repeats it, which costs it nothing.
///
/// [`SourceId::declared`]: iaam_core::ids::SourceId::declared
/// [`ImportId::declared`]: iaam_core::ids::ImportId::declared
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowOrigin {
    pub source: SourceId,
    pub import: Option<ImportId>,
}

/// Submitting a batch of operations.
///
/// A verdict is issued **for each line**: one unrecognised operation
/// does not cancel the others (§10.1). Sequence numbers are assigned
/// by the store by date, so two operations on the same day do not merge
/// into a single ordering position.
///
/// Parsing lives here, writing is below, in [`submit_candidates`]: the input
/// for journal facts has its own parser, while the journal and its safeguards are shared.
///
/// Each row arrives with its own [`RowOrigin`]. The import in it is stamped
/// after normalisation rather than carried in [`NormalizationContext`], because
/// normalisation decides what a row *is* and the import decides nothing about
/// that: it is the handle a later retraction is keyed on, and nothing in the
/// shape of an event depends on it.
pub async fn submit_operations(
    services: &AppServices,
    principal: &Principal,
    operations: &[(RowOrigin, SubmittedOperation)],
) -> Result<Vec<Verdict>, AppError> {
    let candidates = operations
        .iter()
        .map(|(origin, operation)| {
            normalize(
                operation,
                NormalizationContext {
                    owner: principal.owner,
                    source: origin.source,
                },
            )
            .map(|normalized| {
                let mut event = normalized.event;
                if let Some(import) = origin.import {
                    event.provenance = event.provenance.with_import(import);
                }
                event
            })
        })
        .collect();
    // No session: this route writes straight to the journal, and a session
    // identifier stamped here would name an act that never happened.
    submit_candidates(services, principal, "operation", None, candidates).await
}

/// Ingestion of journal facts with schedule-based depreciation enrichment.
///
/// The application reads the schedule and the knowledge coordinate. The normaliser receives
/// only a prepared [`JournalEventEnrichment`], so it remains a pure
/// function and does not depend on reference data or storage.
pub async fn submit_journal_events(
    services: &AppServices,
    principal: &Principal,
    source: SourceId,
    events: &[SubmittedJournalEvent],
) -> Result<Vec<Verdict>, AppError> {
    let knowledge_as_of = OffsetDateTime::now_utc();
    let knowledge_as_of_wire = knowledge_as_of
        .format(&Rfc3339)
        .map_err(|error| AppError::Store(error.to_string()))?;
    let context = NormalizationContext {
        owner: principal.owner,
        source,
    };
    let mut candidates = Vec::with_capacity(events.len());

    for submitted in events {
        let enrichment = match &submitted.fact {
            JournalFact::CorporateAction(CorporateAction::PartialRedemption {
                instrument,
                principal_returned_per_unit,
                effective_date,
                ..
            }) => {
                let loaded_schedule = {
                    let store = services.market_store.lock().await;
                    let offer_kinds = store
                        .market_source_codes(MOEX_ISS_SOURCE_ID, "offer_kind")
                        .map_err(|error| AppError::Store(error.to_string()))?;
                    crate::market_candidate::schedule_from_store(
                        &store,
                        *instrument,
                        &knowledge_as_of_wire,
                        &offer_kinds,
                        None,
                    )?
                };
                let (schedule, snapshot_id) = match loaded_schedule {
                    Some((schedule, snapshot_id)) => (Some(schedule), snapshot_id),
                    None => (None, String::new()),
                };
                JournalEventEnrichment {
                    basis_allocation: iaam_core::rules::resolve_basis_allocation(
                        *principal_returned_per_unit,
                        *effective_date,
                        schedule.as_ref(),
                        &snapshot_id,
                        knowledge_as_of,
                    ),
                }
            }
            _ => JournalEventEnrichment::default(),
        };

        candidates.push(
            normalize_journal_event(submitted, &enrichment, context)
                .map(|normalized| normalized.event),
        );
    }

    submit_candidates(services, principal, "fact", None, candidates).await
}

/// Ingestion of prepared candidates: submission permission, form, writing, verdict.
///
/// The lower layer shared by all inputs. Each input has its own parsing —
/// operations, CSV, journal facts, — but submission permission, structural
/// core validation and writing to the append-only journal must be shared: three
/// copies of this code would silently diverge, precisely at the
/// point where an error can no longer be corrected.
///
/// The permission check is here, rather than at each input, for exactly this reason:
/// an input that forgets to call it cannot write anything.
///
/// `field` is the field name used to identify the rejection to the client: for operations it is
/// `operation`, while journal facts have their own. A rejection naming an unrelated
/// field sends the client to fix something they did not submit (§10.4).
///
/// The declaring principal is stamped here, in the same place and for the same
/// reason as the permission check: this is the one function every caller-driven
/// input passes through, so an input that forgets to stamp cannot write
/// anything. Stamping at each input instead would leave the field absent
/// wherever somebody forgot, and absent is indistinguishable from «recorded
/// before the field existed» — the one thing a retraction keyed on it must
/// never confuse (`iaam-rond`).
///
/// It is stamped on **every** submission and not only on those that name an
/// import, because the field answers who presented a fact and that question is
/// no less real for a batch that declared no label. Narrowing it to the case
/// the retraction rule needs would make the journal's answer depend on what one
/// rule happens to ask.
///
/// `session` names the import session this write is the commit of, and is
/// `None` for every route that writes without one. It is a **parameter** rather
/// than something each input stamps onto its own candidates, and that is the
/// difference between a fact somebody remembered to record and one the compiler
/// asked for: a fifth route added tomorrow cannot reach the journal without
/// saying which act it is performing. The import beside it is still stamped by
/// each input, because the import is decided while a row is being read and the
/// session is decided by the call — see [`crate::scenarios::import_session::commit_session`],
/// the only caller that passes `Some`.
pub async fn submit_candidates(
    services: &AppServices,
    principal: &Principal,
    field: &'static str,
    session: Option<ImportSessionId>,
    candidates: Vec<Result<Event, Rejection>>,
) -> Result<Vec<Verdict>, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "permission to submit operations".into(),
            actual: principal.scope.code().to_owned(),
        });
    }
    let declared_by = PrincipalId(principal.token_id);

    let mut verdicts = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        // The sequence number within a day is assigned by the store in the same
        // transaction as the insert: calling «get next» separately is
        // a race that gives two events the same number (§4.8).
        let mut event = match candidate {
            Ok(event) => event,
            Err(rejection) => {
                verdicts.push(Verdict::Rejected { rejection });
                continue;
            }
        };
        event.provenance = event.provenance.with_declared_by(declared_by);
        if let Some(session) = session {
            event.provenance = event.provenance.with_import_session(session);
        }
        verdicts.push(record_candidate(services, event, field).await?);
    }
    Ok(verdicts)
}

/// Convert a structural validation failure into the row rejection shared by all inputs.
pub fn structural_rejection(event: &Event, field: &'static str) -> Option<Rejection> {
    event.validate_structure().err().map(|error| Rejection {
        field: field.to_owned(),
        expected: "event shape matching its type".into(),
        actual: error.to_string(),
    })
}

/// Validate every event before making one append-only journal write.
pub async fn append_checked(
    services: &AppServices,
    events: Vec<Event>,
    scope: IdentityScope,
) -> Result<Vec<Recorded>, AppError> {
    for (index, event) in events.iter().enumerate() {
        // A newly written event may not claim a version other than the one this
        // build produces. Without this, the schema-aware allowance in
        // `validate_import_coverage_gap` becomes a way to write a gap that names
        // no rows and can never be lifted.
        if event.schema_version != SCHEMA_VERSION {
            return Err(AppError::Invalid {
                field: format!("event[{index}].schema_version"),
                expected: SCHEMA_VERSION.to_string(),
                actual: event.schema_version.to_string(),
            });
        }
        if let Some(rejection) = structural_rejection(event, "event") {
            return Err(AppError::Invalid {
                field: format!("event[{index}]"),
                expected: rejection.expected,
                actual: rejection.actual,
            });
        }
    }
    services.store.append_events(events, scope).await
}

/// Write one candidate and report the outcome.
///
/// Structural core validation happens before writing: the journal is append-only,
/// and a malformed event cannot then be removed from it (§4.8).
pub async fn record_candidate(
    services: &AppServices,
    event: Event,
    field: &'static str,
) -> Result<Verdict, AppError> {
    if let Some(rejection) = structural_rejection(&event, field) {
        return Ok(Verdict::Rejected { rejection });
    }

    let recorded = append_checked(services, vec![event], IdentityScope::Source).await?;
    Ok(match recorded.first() {
        Some(Recorded::Inserted { id }) => Verdict::Provisional { event: *id },
        Some(Recorded::Duplicate { existing }) => Verdict::Duplicate {
            existing: *existing,
        },
        None => Verdict::Rejected {
            rejection: Rejection {
                field: "storage".into(),
                expected: "event record".into(),
                actual: "store returned no result".into(),
            },
        },
    })
}
