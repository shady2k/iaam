//! Синхронизация одного брокерского канала с журналом фактов.
//!
//! Сценарий не считает остатки: он принимает операции и контрольные
//! утверждения, а сверка остаётся чистой функцией `iaam-core`.

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{BrokerChannel, Principal, Recorded};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::InstrumentId;
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_http::HttpRequest;
use iaam_ingest::dedup::{self, DedupDecision, DocumentContext, KnownRecord};
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Verdict, normalize};
use iaam_market::cbr::key_rate::key_rate_request;
use iaam_market::cbr::{daily_request, dynamic_request};
use iaam_market::moex::history_request;
use iaam_market::{FxObservation, KeyRateObservation, PriceKind, PriceObservation};
use iaam_store::market::{
    Coverage, FxRow, KeyRateRow, MarketStore, PriceRow, RunOutcome, SeriesKey,
};
use sha2::{Digest, Sha256};
use time::Date;
use time::OffsetDateTime;

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
/// Конкретный источник и серия ручного запуска.
#[derive(Debug, Clone)]
pub enum MarketSource {
    Moex {
        engine: String,
        market: String,
        board: String,
        secid: String,
        instrument: InstrumentId,
    },
    CbrDaily,
    CbrDynamic {
        cbr_currency_id: String,
        to: iaam_core::money::CurrencyCode,
    },
    CbrKeyRate,
}

/// Узкий запрос ручной синхронизации. Планировщик намеренно отсутствует:
/// расписание появится в следующей части эпика.
#[derive(Debug, Clone)]
pub struct MarketSyncRequest {
    pub source: MarketSource,
    pub from: Date,
    pub to: Date,
}

/// Наблюдаемое состояние запуска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketSyncResult {
    pub outcome: RunOutcome,
    pub rows: usize,
    pub covered: Option<Coverage>,
}

impl MarketSyncResult {
    #[must_use]
    pub const fn status(&self) -> &'static str {
        match self.outcome {
            RunOutcome::Succeeded => "succeeded",
            RunOutcome::Partial { .. } => "partial",
            RunOutcome::Failed { .. } => "failed",
        }
    }
}

/// Синхронизировать одну серию, не публикуя незавершённые строки.
///
/// Сценарий — единственная граница между `iaam-market` и `iaam-store`:
/// сначала разбираются доменные наблюдения источника, затем здесь они
/// превращаются в строковые строки таблиц.
pub async fn sync_market(
    store: &mut MarketStore,
    transport: &dyn crate::ports::MarketData,
    request: MarketSyncRequest,
) -> Result<MarketSyncResult, AppError> {
    if request.to < request.from {
        return Err(AppError::Invalid {
            field: "to".to_owned(),
            expected: "дата не раньше from".to_owned(),
            actual: request.to.to_string(),
        });
    }

    let series = series_for(&request.source);
    let handle = store
        .begin_run(
            series,
            request.from,
            request.to,
            OffsetDateTime::now_utc() + time::Duration::minutes(10),
        )
        .map_err(store_error)?;
    let observed_at = iaam_market::ObservedAt(OffsetDateTime::now_utc());
    let http_request = request_for(&request.source, request.from, request.to);

    let response = match transport.send(http_request).await {
        Ok(response) => response,
        Err(error) => return partial(store, &handle, error.to_string()),
    };
    if !(200..300).contains(&response.status) {
        return partial(
            store,
            &handle,
            format!("источник вернул HTTP {}", response.status),
        );
    }

    let parsed = match parse_response(&request.source, &response.body, observed_at) {
        Ok(parsed) => parsed,
        Err(error) => return partial(store, &handle, error.to_string()),
    };
    let coverage = Coverage {
        from: request.from,
        to: request.to,
    };
    let result = match parsed {
        ParsedObservations::Prices(observations) => {
            let rows = observations.iter().map(price_row).collect::<Vec<_>>();
            match store.record_prices(&handle, &response.raw_hash, &rows) {
                Ok(count) => count,
                Err(error) => return Err(fail_run(store, &handle, error)),
            }
        }
        ParsedObservations::Fx(observations) => {
            let rows = observations.iter().map(fx_row).collect::<Vec<_>>();
            match store.record_fx(&handle, &response.raw_hash, &rows) {
                Ok(count) => count,
                Err(error) => return Err(fail_run(store, &handle, error)),
            }
        }
        ParsedObservations::KeyRates(observations) => {
            let rows = observations.iter().map(key_rate_row).collect::<Vec<_>>();
            match store.record_key_rate(&handle, &response.raw_hash, &rows) {
                Ok(count) => count,
                Err(error) => return Err(fail_run(store, &handle, error)),
            }
        }
    };
    store
        .finish_run(&handle, RunOutcome::Succeeded, Some(coverage))
        .map_err(store_error)?;
    Ok(MarketSyncResult {
        outcome: RunOutcome::Succeeded,
        rows: result,
        covered: Some(coverage),
    })
}

/// Синхронизировать рынок через собранные зависимости приложения.
///
/// Сервер вызывает этот фасад, а не получает доступ к адаптеру хранилища:
/// оркестрация источника и записи остаётся в `iaam-app`.
pub async fn sync_market_with_services(
    services: &AppServices,
    request: MarketSyncRequest,
) -> Result<MarketSyncResult, AppError> {
    let mut store = services.market_store.lock().await;
    sync_market(&mut store, services.market.as_ref(), request).await
}

fn request_for(source: &MarketSource, from: Date, to: Date) -> HttpRequest {
    match source {
        MarketSource::Moex {
            engine,
            market,
            board,
            secid,
            ..
        } => history_request(engine, market, board, secid, from, to, 0),
        MarketSource::CbrDaily => daily_request(to),
        MarketSource::CbrDynamic {
            cbr_currency_id, ..
        } => dynamic_request(from, to, cbr_currency_id),
        MarketSource::CbrKeyRate => key_rate_request(from, to),
    }
}

enum ParsedObservations {
    Prices(Vec<PriceObservation>),
    Fx(Vec<FxObservation>),
    KeyRates(Vec<KeyRateObservation>),
}

fn parse_response(
    source: &MarketSource,
    body: &[u8],
    observed_at: iaam_market::ObservedAt,
) -> Result<ParsedObservations, AppError> {
    match source {
        MarketSource::Moex { instrument, .. } => {
            let body = core::str::from_utf8(body)
                .map_err(|error| AppError::Store(format!("ответ MOEX не UTF-8: {error}")))?;
            iaam_market::moex::parse::parse_history(body, *instrument, observed_at)
                .map(ParsedObservations::Prices)
                .map_err(|error| AppError::Store(error.to_string()))
        }
        MarketSource::CbrDaily => {
            let body = iaam_market::cbr::fx::decode_cp1251(body);
            iaam_market::cbr::fx::parse_daily(&body, observed_at)
                .map(ParsedObservations::Fx)
                .map_err(|error| AppError::Store(error.to_string()))
        }
        MarketSource::CbrDynamic { to, .. } => {
            let body = iaam_market::cbr::fx::decode_cp1251(body);
            iaam_market::cbr::fx::parse_dynamic(&body, *to, observed_at)
                .map(ParsedObservations::Fx)
                .map_err(|error| AppError::Store(error.to_string()))
        }
        MarketSource::CbrKeyRate => {
            let body = core::str::from_utf8(body)
                .map_err(|error| AppError::Store(format!("ответ ЦБ не UTF-8: {error}")))?;
            iaam_market::cbr::key_rate::parse_key_rate(body, observed_at)
                .map(ParsedObservations::KeyRates)
                .map_err(|error| AppError::Store(error.to_string()))
        }
    }
}

fn series_for(source: &MarketSource) -> SeriesKey {
    match source {
        MarketSource::Moex {
            instrument, board, ..
        } => SeriesKey {
            source_id: "moex-iss".to_owned(),
            dataset: "prices".to_owned(),
            series_key: format!("{}:{board}", instrument.inner()),
        },
        MarketSource::CbrDaily => SeriesKey {
            source_id: "cbr".to_owned(),
            dataset: "fx".to_owned(),
            series_key: "daily".to_owned(),
        },
        MarketSource::CbrDynamic {
            cbr_currency_id,
            to,
            ..
        } => SeriesKey {
            source_id: "cbr".to_owned(),
            dataset: "fx".to_owned(),
            series_key: format!("{cbr_currency_id}:{}", to.code()),
        },
        MarketSource::CbrKeyRate => SeriesKey {
            source_id: "cbr".to_owned(),
            dataset: "key_rate".to_owned(),
            series_key: "key_rate".to_owned(),
        },
    }
}

fn price_row(observation: &PriceObservation) -> PriceRow {
    PriceRow {
        instrument_id: observation.instrument.inner().to_string(),
        board: observation.venue.board.clone(),
        session: observation.venue.session,
        trade_date: observation.trade_date.0.to_string(),
        kind: price_kind(observation.kind).to_owned(),
        observed_at: observation
            .observed_at
            .0
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| observation.observed_at.0.to_string()),
        price: observation.price.inner().to_string(),
        currency: observation.currency.code().to_owned(),
        executability: executability(observation.executability).to_owned(),
    }
}

fn fx_row(observation: &FxObservation) -> FxRow {
    FxRow {
        from_code: observation.from.code().to_owned(),
        to_code: observation.to.code().to_owned(),
        trade_date: observation.trade_date.0.to_string(),
        observed_at: observation
            .observed_at
            .0
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| observation.observed_at.0.to_string()),
        nominal: observation.nominal,
        value: observation.value.inner().to_string(),
        unit_rate: observation.unit_rate.inner().to_string(),
    }
}

fn key_rate_row(observation: &KeyRateObservation) -> KeyRateRow {
    KeyRateRow {
        trade_date: observation.trade_date.0.to_string(),
        observed_at: observation
            .observed_at
            .0
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| observation.observed_at.0.to_string()),
        rate: observation.rate.inner().to_string(),
    }
}

fn price_kind(kind: PriceKind) -> &'static str {
    match kind {
        PriceKind::Close => "close",
        PriceKind::LegalClose => "legal_close",
        PriceKind::WeightedAverage => "weighted_average",
        PriceKind::MarketPrice2 => "market_price_2",
        PriceKind::MarketPrice3 => "market_price_3",
        PriceKind::AdmittedQuote => "admitted_quote",
    }
}

fn executability(value: iaam_market::Executability) -> &'static str {
    match value {
        iaam_market::Executability::Executable => "executable",
        iaam_market::Executability::IndicativePreviousClose => "indicative_previous_close",
        iaam_market::Executability::Stale => "stale",
    }
}

fn store_error(error: iaam_store::StoreError) -> AppError {
    AppError::Store(error.to_string())
}

fn fail_run(
    store: &mut MarketStore,
    handle: &iaam_store::market::RunHandle,
    error: iaam_store::StoreError,
) -> AppError {
    let app_error = store_error(error);
    let reason = app_error.to_string();
    let _ = store.finish_run(handle, RunOutcome::Failed { reason }, None);
    app_error
}

fn partial(
    store: &mut MarketStore,
    handle: &iaam_store::market::RunHandle,
    reason: String,
) -> Result<MarketSyncResult, AppError> {
    store
        .finish_run(
            handle,
            RunOutcome::Partial {
                reason: reason.clone(),
            },
            None,
        )
        .map_err(store_error)?;
    Ok(MarketSyncResult {
        outcome: RunOutcome::Partial { reason },
        rows: 0,
        covered: None,
    })
}

#[cfg(test)]
mod market_tests {
    use super::*;
    use crate::ports::{MarketData, MarketResponse};
    use async_trait::async_trait;
    use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
    use iaam_core::money::CurrencyCode;
    use iaam_store::reference::InstrumentRecord;
    use time::macros::date;

    struct FixtureTransport {
        body: Vec<u8>,
        status: u16,
    }

    #[async_trait]
    impl MarketData for FixtureTransport {
        async fn send(&self, _request: HttpRequest) -> Result<MarketResponse, AppError> {
            Ok(MarketResponse {
                status: self.status,
                raw_hash: "fixture-hash".to_owned(),
                body: self.body.clone(),
            })
        }
    }

    #[tokio::test]
    async fn a_fixture_transport_publishes_market_rows_without_network() {
        let mut store = MarketStore::open_in_memory().expect("хранилище");
        let instrument = InstrumentId::new_random();
        store
            .upsert_instrument(&InstrumentRecord {
                id: instrument,
                kind: Some(InstrumentKind::Share),
                symbol: "SBER".to_owned(),
                title: "Сбербанк".to_owned(),
                currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
                lineage: None,
            })
            .expect("инструмент");
        let body = include_bytes!("../../../../tests/fixtures/market/moex-iss-history-sber.json");
        let transport = FixtureTransport {
            body: body.to_vec(),
            status: 200,
        };
        let result = sync_market(
            &mut store,
            &transport,
            MarketSyncRequest {
                source: MarketSource::Moex {
                    engine: "stock".to_owned(),
                    market: "shares".to_owned(),
                    board: "TQBR".to_owned(),
                    secid: "SBER".to_owned(),
                    instrument,
                },
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 21),
            },
        )
        .await
        .expect("синхронизация");

        let first_observed_at: String = store
            .connection()
            .query_row(
                "SELECT observed_at FROM price_observations
                 ORDER BY observed_at ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("момент первого наблюдения");

        let first_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM price_observations", [], |row| {
                row.get(0)
            })
            .expect("число первых наблюдений");

        let repeated = sync_market(
            &mut store,
            &transport,
            MarketSyncRequest {
                source: MarketSource::Moex {
                    engine: "stock".to_owned(),
                    market: "shares".to_owned(),
                    board: "TQBR".to_owned(),
                    secid: "SBER".to_owned(),
                    instrument,
                },
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 21),
            },
        )
        .await
        .expect("повторная синхронизация");

        let second_observed_at: String = store
            .connection()
            .query_row(
                "SELECT observed_at FROM price_observations
                 ORDER BY observed_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("момент повторного наблюдения");
        let second_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM price_observations", [], |row| {
                row.get(0)
            })
            .expect("число повторных наблюдений");
        assert!(second_observed_at > first_observed_at);
        assert_eq!(
            second_count,
            first_count + i64::try_from(repeated.rows).unwrap()
        );

        assert_eq!(repeated.status(), "succeeded");
        assert_eq!(repeated.rows, result.rows);

        assert_eq!(result.status(), "succeeded");
        assert!(result.rows > 0);
        assert_eq!(
            store
                .complete_through(&SeriesKey {
                    source_id: "moex-iss".to_owned(),
                    dataset: "prices".to_owned(),
                    series_key: format!("{}:TQBR", instrument.inner()),
                })
                .expect("граница"),
            Some(date!(2026 - 08 - 21))
        );
    }

    #[tokio::test]
    async fn a_source_failure_is_partial_and_keeps_the_previous_boundary() {
        let mut store = MarketStore::open_in_memory().expect("хранилище");
        let transport = FixtureTransport {
            body: Vec::new(),
            status: 503,
        };
        let result = sync_market(
            &mut store,
            &transport,
            MarketSyncRequest {
                source: MarketSource::CbrKeyRate,
                from: date!(2026 - 02 - 01),
                to: date!(2026 - 02 - 28),
            },
        )
        .await
        .expect("частичный запуск");

        assert_eq!(result.status(), "partial");
        assert!(result.covered.is_none());
        assert_eq!(
            store
                .complete_through(&SeriesKey {
                    source_id: "cbr".to_owned(),
                    dataset: "key_rate".to_owned(),
                    series_key: "key_rate".to_owned(),
                })
                .expect("граница"),
            None
        );
    }
}
