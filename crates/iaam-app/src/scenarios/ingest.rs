//! Приёмка операций.

use iaam_core::ids::SourceId;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{SubmittedOperation, Verdict, normalize};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{Principal, Recorded};

/// Отправка пачки операций.
///
/// Вердикт выдаётся **на каждую строку**: одна непонятая операция
/// не отменяет остальные (§10.1). Порядковые номера выдаются
/// хранилищем по дате, поэтому две операции одного дня не сливаются
/// в одну позицию порядка.
pub async fn submit_operations(
    services: &AppServices,
    principal: &Principal,
    source: SourceId,
    operations: &[SubmittedOperation],
) -> Result<Vec<Verdict>, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "право отправки операций".into(),
            actual: principal.scope.code().to_owned(),
        });
    }

    let mut verdicts = Vec::with_capacity(operations.len());
    for operation in operations {
        // Порядковый номер внутри дня назначает хранилище в той же
        // транзакции, что и вставку: «узнать следующий» отдельным
        // вызовом — гонка, дающая двум событиям один номер (§4.8).
        let normalized = match normalize(
            operation,
            NormalizationContext {
                owner: principal.owner,
                source,
            },
        ) {
            Ok(normalized) => normalized,
            Err(rejection) => {
                verdicts.push(Verdict::Rejected { rejection });
                continue;
            }
        };

        // Структурная проверка ядра до записи: журнал append-only,
        // и неверное по форме событие из него уже не убрать (§4.8).
        if let Err(error) = normalized.event.validate_structure() {
            verdicts.push(Verdict::Rejected {
                rejection: iaam_ingest::Rejection {
                    field: "operation".into(),
                    expected: "форма события, соответствующая его типу".into(),
                    actual: error.to_string(),
                },
            });
            continue;
        }

        let recorded = services.store.append_events(vec![normalized.event]).await?;
        verdicts.push(match recorded.first() {
            Some(Recorded::Inserted { id }) => Verdict::Provisional { event: *id },
            Some(Recorded::Duplicate { existing }) => Verdict::Duplicate {
                existing: *existing,
            },
            None => Verdict::Rejected {
                rejection: iaam_ingest::Rejection {
                    field: "storage".into(),
                    expected: "запись события".into(),
                    actual: "хранилище не вернуло результата".into(),
                },
            },
        });
    }
    Ok(verdicts)
}
