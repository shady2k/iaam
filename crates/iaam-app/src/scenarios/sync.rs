//! Синхронизация одного брокерского канала с журналом фактов.
//!
//! Сценарий не считает остатки: он принимает операции и контрольные
//! утверждения, а сверка остаётся чистой функцией `iaam-core`.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_ingest::dedup::{self, DedupDecision, DocumentContext, KnownRecord};
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Verdict, normalize};
use sha2::{Digest, Sha256};
use time::Date;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{BrokerChannel, Principal, Recorded};

/// Результат синхронизации одного канала за один интервал.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub recorded: Vec<Verdict>,
    pub duplicates: usize,
    pub assertions: usize,
}

/// Получает операции и портфель брокера и записывает новые факты.
///
/// Сопоставление с уже записанным журналом выполняется до вызова store:
/// слой хранилища знает только источник вместе с `source_operation_id`, а
/// сверка двух независимых каналов обязана видеть одинаковую операцию и при
/// разных источниках. Вероятный дубликат не удаляется: это лишь подсказка
/// уровня §10.6, поэтому в журнал он попадает как новый факт.
pub async fn sync_broker(
    services: &AppServices,
    principal: &Principal,
    broker: &dyn BrokerChannel,
    account: AccountId,
    from: Date,
    to: Date,
) -> Result<SyncOutcome, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".to_owned(),
            expected: "право синхронизации брокера".to_owned(),
            actual: principal.scope.code().to_owned(),
        });
    }

    let parsed = broker
        .fetch_operations(account, from, to)
        .await
        .map_err(broker_error)?;
    let channel = broker.channel();
    let mut known = known_records(
        &services
            .store
            .load_events_through(principal.owner, to)
            .await?,
    );
    let mut recorded = Vec::new();
    let mut duplicates = 0;

    for operation in parsed.accepted {
        let context = DocumentContext {
            document: None,
            sheet: None,
            row: None,
        };
        let key = dedup::choose_key(&operation, &context);
        let normalized = normalize(
            &operation,
            NormalizationContext {
                owner: principal.owner,
                source: channel.source,
            },
        )
        .map_err(|rejection| AppError::Invalid {
            field: rejection.field,
            expected: rejection.expected,
            actual: rejection.actual,
        })?;
        let event = with_channel_provenance(normalized.event, &channel);
        let decision = dedup::assess(key.as_ref(), event.provenance.raw_hash(), &context, &known);
        if let DedupDecision::Duplicate { existing, .. } = decision {
            duplicates += 1;
            recorded.push(Verdict::Duplicate { existing });
            continue;
        }

        let result = services.store.append_events(vec![event.clone()]).await?;
        let verdict = verdict_from_recorded(&result, &mut duplicates);
        if let Some(event_id) = event_id_from_verdict(&verdict) {
            known.push(known_record(&event, event_id));
        }
        recorded.push(verdict);
    }

    // Отказанная строка доказывает, что ответ не является полной выгрузкой.
    // Операции выше всё равно сохраняются, но контрольный остаток нельзя
    // записать рядом с неполным интервалом.
    if !parsed.quarantined.is_empty() {
        return Ok(SyncOutcome {
            recorded,
            duplicates,
            assertions: 0,
        });
    }

    let claims = broker
        .fetch_portfolio(account, to)
        .await
        .map_err(broker_error)?;
    let mut assertions = 0;
    for (index, claim) in claims.into_iter().enumerate() {
        let event = assertion_event(
            AssertionTarget {
                owner: principal.owner,
                account,
                from,
                to,
            },
            claim,
            &channel,
            index as u32 + 1,
        );
        let key = event.idempotency_key.clone();
        if let Some(existing) = known.iter().find_map(|record| {
            (record.idempotency_key.as_deref() == key.as_deref()).then_some(record.event)
        }) {
            duplicates += 1;
            recorded.push(Verdict::Duplicate { existing });
            continue;
        }
        let result = services.store.append_events(vec![event.clone()]).await?;
        let verdict = verdict_from_recorded(&result, &mut duplicates);
        if matches!(verdict, Verdict::Provisional { .. }) {
            assertions += 1;
        }
        if let Some(event_id) = event_id_from_verdict(&verdict) {
            known.push(known_record(&event, event_id));
        }
        recorded.push(verdict);
    }

    Ok(SyncOutcome {
        recorded,
        duplicates,
        assertions,
    })
}

fn broker_error(error: crate::ports::BrokerError) -> AppError {
    AppError::Store(format!("синхронизация брокера: {error}"))
}

fn with_channel_provenance(mut event: Event, channel: &SourceChannel) -> Event {
    let mut provenance = Provenance::new(
        channel.source,
        event.provenance.raw_hash().clone(),
        channel.parser_version.clone(),
    );
    if let Some(source_operation_id) = event.provenance.source_operation_id() {
        provenance = provenance.with_source_operation_id(source_operation_id);
    }
    event.provenance = provenance;
    event
}

fn known_records(events: &[Event]) -> Vec<KnownRecord> {
    events
        .iter()
        .map(|event| known_record(event, event.id))
        .collect()
}

fn known_record(event: &Event, event_id: EventId) -> KnownRecord {
    let row = event.provenance.row();
    KnownRecord {
        event: event_id,
        source_operation_id: event.provenance.source_operation_id().map(str::to_owned),
        idempotency_key: event.idempotency_key.clone(),
        fingerprint: event.provenance.raw_hash().clone(),
        document: row.map(|locator| locator.document.clone()),
        sheet: row.and_then(|locator| locator.sheet.clone()),
        row: row.map(|locator| locator.row),
    }
}

fn verdict_from_recorded(recorded: &[Recorded], duplicates: &mut usize) -> Verdict {
    match recorded.first() {
        Some(Recorded::Inserted { id }) => Verdict::Provisional { event: *id },
        Some(Recorded::Duplicate { existing }) => {
            *duplicates += 1;
            Verdict::Duplicate {
                existing: *existing,
            }
        }
        None => Verdict::Rejected {
            rejection: iaam_ingest::Rejection {
                field: "storage".to_owned(),
                expected: "результат записи события".to_owned(),
                actual: "хранилище не вернуло результата".to_owned(),
            },
        },
    }
}

fn event_id_from_verdict(verdict: &Verdict) -> Option<EventId> {
    match verdict {
        Verdict::Provisional { event }
        | Verdict::Accepted { event }
        | Verdict::Discrepancy { event, .. } => Some(*event),
        Verdict::Duplicate { .. }
        | Verdict::NeedsReconciliation { .. }
        | Verdict::NeedsClassification { .. }
        | Verdict::Unsupported { .. }
        | Verdict::Rejected { .. } => None,
    }
}

/// Чьё утверждение и за какой интервал.
///
/// Отдельный тип, а не четыре параметра подряд: владелец и счёт —
/// разные вещи одного вида, а две даты интервала переставляются местами
/// незаметно, и сверка после этого никогда ни с чем не сходится.
struct AssertionTarget {
    owner: OwnerId,
    account: AccountId,
    from: Date,
    to: Date,
}

fn assertion_event(
    target: AssertionTarget,
    claim: ControlClaim,
    channel: &SourceChannel,
    order: u32,
) -> Event {
    let AssertionTarget {
        owner,
        account,
        from,
        to,
    } = target;
    let identity = format!(
        "sync-assertion/{account:?}/{from}/{to}/{:?}/{:?}",
        channel.source, claim
    );
    let digest = Sha256::digest(identity.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let raw_hash = RawHash::parse(&hex)
        .unwrap_or_else(|| unreachable!("шестнадцатеричный SHA-256 — всегда годный RawHash"));
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: iaam_core::event::kind::EventKind::ControlAssertion {
            period: iaam_core::reconciliation::claim::AssertionPeriod { from, to },
            claim,
        },
        dates: EventDates::for_cash(CashPostedDate(to)),
        order: EffectiveOrder::new(to, order),
        legs: Vec::new(),
        provenance: Provenance::new(
            channel.source,
            raw_hash,
            ParserVersion(channel.parser_version.0.clone()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: Some(identity),
    }
}
