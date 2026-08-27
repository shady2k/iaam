//! Приёмка операций.

use iaam_core::event::Event;
use iaam_core::ids::SourceId;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Rejection, SubmittedOperation, Verdict, normalize};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{Principal, Recorded};

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
