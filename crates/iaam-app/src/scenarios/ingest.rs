//! Приёмка операций.

use iaam_core::bond::BondSchedule;
use iaam_core::event::Event;
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::ids::SourceId;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{
    JournalEventEnrichment, JournalFact, Rejection, SubmittedJournalEvent, SubmittedOperation,
    Verdict, normalize, normalize_journal_event,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{Principal, Recorded};
use crate::scenarios::reports::MOEX_ISS_SOURCE_ID;

/// Отправка пачки операций.
///
/// Вердикт выдаётся **на каждую строку**: одна непонятая операция
/// не отменяет остальные (§10.1). Порядковые номера выдаются
/// хранилищем по дате, поэтому две операции одного дня не сливаются
/// в одну позицию порядка.
///
/// Разбор живёт здесь, запись — ниже, в [`submit_candidates`]: у входа
/// для журнальных фактов разбор свой, а журнал и его заслоны те же.
pub async fn submit_operations(
    services: &AppServices,
    principal: &Principal,
    source: SourceId,
    operations: &[SubmittedOperation],
) -> Result<Vec<Verdict>, AppError> {
    let candidates = operations
        .iter()
        .map(|operation| {
            normalize(
                operation,
                NormalizationContext {
                    owner: principal.owner,
                    source,
                },
            )
            .map(|normalized| normalized.event)
        })
        .collect();
    submit_candidates(services, principal, "operation", candidates).await
}

/// Приёмка журнальных фактов с обогащением амортизации графиком.
///
/// График и координату знания читает приложение. Нормализатор получает
/// только готовое [`JournalEventEnrichment`], поэтому остаётся чистой
/// функцией и не зависит от справочника или хранилища.
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
                    match store
                        .schedule_at_or_before(
                            &instrument.inner().to_string(),
                            MOEX_ISS_SOURCE_ID,
                            &knowledge_as_of_wire,
                        )
                        .map_err(|error| AppError::Store(error.to_string()))?
                    {
                        None => None,
                        Some(snapshot) => {
                            let completeness_row = store
                                .schedule_completeness(&snapshot.snapshot_id)
                                .map_err(|error| AppError::Store(error.to_string()))?;
                            let terms = store
                                .issue_terms_at_or_before(
                                    &instrument.inner().to_string(),
                                    MOEX_ISS_SOURCE_ID,
                                    &knowledge_as_of_wire,
                                )
                                .map_err(|error| AppError::Store(error.to_string()))?;
                            let offer_kinds = store
                                .market_source_codes(MOEX_ISS_SOURCE_ID, "offer_kind")
                                .map_err(|error| AppError::Store(error.to_string()))?;
                            let schedule = BondSchedule {
                                periods: crate::market_candidate::accrual_periods_from_snapshot(
                                    &snapshot,
                                )?,
                                principal_returns:
                                    crate::market_candidate::principal_returns_from_snapshot(
                                        &snapshot,
                                    )?,
                                initial_principal:
                                    crate::market_candidate::initial_principal_from_terms(
                                        terms.as_ref(),
                                    ),
                                offer_windows:
                                    crate::market_candidate::offer_windows_from_snapshot(
                                        &snapshot,
                                        *instrument,
                                        &offer_kinds,
                                    )?,
                                completeness:
                                    crate::market_candidate::schedule_completeness_from_row(
                                        completeness_row.as_ref(),
                                    ),
                                default_flags: crate::market_candidate::default_flags_from_terms(
                                    terms.as_ref(),
                                ),
                                currency_roles: None,
                            };
                            Some((schedule, snapshot.snapshot_id.clone()))
                        }
                    }
                };
                let (schedule, snapshot_id) = match loaded_schedule {
                    Some((schedule, snapshot_id)) => (Some(schedule), snapshot_id),
                    None => (None, String::new()),
                };
                JournalEventEnrichment {
                    basis_allocation: crate::allocation::resolve_basis_allocation(
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

    submit_candidates(services, principal, "fact", candidates).await
}

/// Приёмка готовых кандидатов: право отправки, форма, запись, вердикт.
///
/// Нижний уровень, общий для всех входов. Разбор у каждого входа свой —
/// операции, CSV, журнальные факты, — а вот право отправки, структурная
/// проверка ядра и запись в append-only журнал обязаны быть одни: три
/// копии этого куска разошлись бы молча, и разошлись бы именно в том
/// месте, где ошибка уже неисправима.
///
/// Проверка права стоит здесь, а не у каждого входа, ровно поэтому:
/// вход, который забудет её позвать, записать ничего не сможет.
///
/// `field` — имя поля, которым отказ называется клиенту: у операций это
/// `operation`, у журнальных фактов — своё. Отказ, называющий чужое имя
/// поля, отправляет клиента чинить то, чего он не отправлял (§10.4).
pub async fn submit_candidates(
    services: &AppServices,
    principal: &Principal,
    field: &'static str,
    candidates: Vec<Result<Event, Rejection>>,
) -> Result<Vec<Verdict>, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "право отправки операций".into(),
            actual: principal.scope.code().to_owned(),
        });
    }

    let mut verdicts = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        // Порядковый номер внутри дня назначает хранилище в той же
        // транзакции, что и вставку: «узнать следующий» отдельным
        // вызовом — гонка, дающая двум событиям один номер (§4.8).
        let event = match candidate {
            Ok(event) => event,
            Err(rejection) => {
                verdicts.push(Verdict::Rejected { rejection });
                continue;
            }
        };
        verdicts.push(record_candidate(services, event, field).await?);
    }
    Ok(verdicts)
}

/// Записать одного кандидата и назвать исход.
///
/// Структурная проверка ядра идёт до записи: журнал append-only,
/// и неверное по форме событие из него уже не убрать (§4.8).
pub async fn record_candidate(
    services: &AppServices,
    event: Event,
    field: &'static str,
) -> Result<Verdict, AppError> {
    if let Err(error) = event.validate_structure() {
        return Ok(Verdict::Rejected {
            rejection: Rejection {
                field: field.to_owned(),
                expected: "форма события, соответствующая его типу".into(),
                actual: error.to_string(),
            },
        });
    }

    let recorded = services.store.append_events(vec![event]).await?;
    Ok(match recorded.first() {
        Some(Recorded::Inserted { id }) => Verdict::Provisional { event: *id },
        Some(Recorded::Duplicate { existing }) => Verdict::Duplicate {
            existing: *existing,
        },
        None => Verdict::Rejected {
            rejection: Rejection {
                field: "storage".into(),
                expected: "запись события".into(),
                actual: "хранилище не вернуло результата".into(),
            },
        },
    })
}
