//! Synchronisation of a single broker channel with the fact journal.
//!
//! The scenario does not calculate balances: it accepts operations and control
//! assertions, while reconciliation remains a pure `iaam-core` function.

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{BrokerChannel, PortfolioAsOf, Principal, Recorded};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::InstrumentId;
use iaam_core::ids::{AccountId, CustodyId, EventId, OwnerId};
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_http::HttpRequest;
use iaam_ingest::dedup::{self, DedupDecision, DocumentContext, KnownRecord};
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{Verdict, normalize};
use iaam_market::cbr::key_rate::key_rate_request;
use iaam_market::cbr::{daily_request, dynamic_request};
use iaam_market::moex::{HistoryQuery, history_request};
use iaam_market::{
    AccruedInterestObservation, FxObservation, KeyRateObservation, PriceKind, PriceObservation,
};
use iaam_store::market::{
    AccruedInterestRow, Coverage, FxRow, KeyRateRow, MarketStore, PriceRow, RunOutcome, SeriesKey,
};
use sha2::{Digest, Sha256};
use time::Date;
use time::OffsetDateTime;

/// Why no control assertion was recorded, when none was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionsWithheld {
    /// The current portfolio does not describe any day in the requested interval.
    PortfolioDescribesAnotherDay { as_of: Date },
}

impl AssertionsWithheld {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::PortfolioDescribesAnotherDay { .. } => "portfolio_describes_another_day",
        }
    }
}

/// Result of synchronising one channel for one interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub recorded: Vec<Verdict>,
    pub duplicates: usize,
    pub possible_duplicates: usize,
    pub assertions: usize,
    pub assertions_withheld: Option<AssertionsWithheld>,
}

/// Retrieves the broker's operations and portfolio and records new facts.
///
/// Matching against the existing journal is performed before calling the store:
/// both gates apply the channel's declared identity scope to source operation identifiers.
/// Reconciliation of two independent channels must still recognise the same operation even
/// from different sources. A probable duplicate is not removed: it is only a hint
/// at the §10.6 level, so it enters the journal as a new fact.
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
            expected: "permission to synchronise the broker".to_owned(),
            actual: principal.scope.code().to_owned(),
        });
    }

    let parsed = broker
        .fetch_operations(account, from, to)
        .await
        .map_err(broker_error)?;
    let channel = broker.channel();
    // Deduplication belongs to the requested interval; it must not treat later facts
    // as part of this sync.
    let bounded_events = services
        .store
        .load_events_through(principal.owner, to)
        .await?;
    // The refusal predicate belongs to the account's entire history, including facts
    // outside the interval being imported.
    let all_events = services
        .store
        .load_events_through(principal.owner, Date::MAX)
        .await?;
    let affected = affected_trade_count(&all_events, account);
    if affected > 0 {
        return Err(AppError::Conflict {
            what: format!(
                "broker synchronisation refused for account {}: {affected} trade event(s) carry account-derived custody; run repair iaam-y3a2 before synchronising",
                account.inner()
            ),
        });
    }
    // A gateway may filter by order date while the fact uses its trade date.
    // Keep the fact, but do not assert completeness for an interval it falls outside.
    let has_out_of_interval_trade = parsed.accepted.iter().any(|operation| {
        operation
            .dates
            .trade
            .is_some_and(|trade| trade < from || trade > to)
    });
    let mut known = known_records(&bounded_events);
    let mut recorded = Vec::new();
    let mut duplicates = 0;
    let mut possible_duplicates = 0;

    for operation in parsed.accepted {
        let context = DocumentContext {
            account: operation.account,
            document: None,
            sheet: None,
            row: None,
            identity_scope: broker.identity_scope(),
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
        let possible_duplicate = match &decision {
            DedupDecision::PossibleDuplicate { of, level } => Some((*of, *level)),
            DedupDecision::Duplicate { .. } | DedupDecision::Fresh => None,
        };
        if let DedupDecision::Duplicate { existing, .. } = decision {
            duplicates += 1;
            recorded.push(Verdict::Duplicate { existing });
            continue;
        }

        let result = services
            .store
            .append_events(vec![event.clone()], broker.identity_scope())
            .await?;
        let verdict = verdict_from_recorded(&result, &mut duplicates);
        let verdict = match (possible_duplicate, event_id_from_verdict(&verdict)) {
            (Some((of, level)), Some(event)) => {
                possible_duplicates += 1;
                Verdict::PossibleDuplicate { event, of, level }
            }
            _ => verdict,
        };
        if let Some(event_id) = event_id_from_verdict(&verdict) {
            known.push(known_record(&event, event_id));
        }
        recorded.push(verdict);
    }

    // A rejected row proves that the response is not a complete export.
    // The operations above are still saved, but the control balance cannot be
    // recorded alongside an incomplete interval.
    if !parsed.quarantined.is_empty() || has_out_of_interval_trade {
        return Ok(SyncOutcome {
            recorded,
            duplicates,
            possible_duplicates,
            assertions: 0,
            assertions_withheld: None,
        });
    }

    let snapshot = broker
        .fetch_portfolio(account, to)
        .await
        .map_err(broker_error)?;
    let assertions_withheld = match snapshot.as_of {
        PortfolioAsOf::Requested => None,
        PortfolioAsOf::Current => {
            let as_of = services.clock.today();
            (!((from..=to).contains(&as_of)))
                .then_some(AssertionsWithheld::PortfolioDescribesAnotherDay { as_of })
        }
    };
    if assertions_withheld.is_some() {
        return Ok(SyncOutcome {
            recorded,
            duplicates,
            possible_duplicates,
            assertions: 0,
            assertions_withheld,
        });
    }

    let mut assertions = 0;
    for (index, claim) in snapshot.claims.into_iter().enumerate() {
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
        let result = services
            .store
            .append_events(vec![event.clone()], broker.identity_scope())
            .await?;
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
        possible_duplicates,
        assertions,
        assertions_withheld: None,
    })
}

fn broker_error(error: crate::ports::BrokerError) -> AppError {
    AppError::Store(format!("broker synchronisation: {error}"))
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
fn affected_trade_count(events: &[Event], account: AccountId) -> usize {
    let account_custody = CustodyId(account.inner());
    events
        .iter()
        .filter(|event| {
            event.account == account
                && matches!(&event.kind, EventKind::Trade { .. })
                && event.legs.iter().any(|leg| {
                    leg.account == account
                        && leg.quantity.is_some()
                        && leg.custody == Some(account_custody)
                })
        })
        .count()
}

fn known_record(event: &Event, event_id: EventId) -> KnownRecord {
    let row = event.provenance.row();
    KnownRecord {
        event: event_id,
        account: event.account,
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
                expected: "event recording result".to_owned(),
                actual: "storage returned no result".to_owned(),
            },
        },
    }
}

fn event_id_from_verdict(verdict: &Verdict) -> Option<EventId> {
    match verdict {
        Verdict::Provisional { event }
        | Verdict::Accepted { event }
        | Verdict::Discrepancy { event, .. }
        | Verdict::PossibleDuplicate { event, .. } => Some(*event),
        Verdict::Duplicate { .. }
        | Verdict::NeedsReconciliation { .. }
        | Verdict::NeedsClassification { .. }
        | Verdict::Unsupported { .. }
        | Verdict::Rejected { .. } => None,
    }
}

/// Whose assertion and for which interval.
///
/// A separate type rather than four consecutive parameters: the owner and account are
/// different things of the same kind, while the two interval dates can be swapped
/// unnoticed, after which reconciliation will never match anything.
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
        .unwrap_or_else(|| unreachable!("hexadecimal SHA-256 is always a valid RawHash"));
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
/// A specific source and manual run series.
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

/// A narrow manual synchronisation request. The scheduler is deliberately absent:
/// scheduling will be added in the next part of the epic.
#[derive(Debug, Clone)]
pub struct MarketSyncRequest {
    pub source: MarketSource,
    pub from: Date,
    pub to: Date,
}

/// Observable run state.
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

/// Synchronise one series without publishing incomplete rows.
///
/// The scenario is the only boundary between `iaam-market` and `iaam-store`:
/// first the source's domain observations are parsed, then here they
/// are converted into textual table rows.
pub async fn sync_market(
    store: &mut MarketStore,
    transport: &dyn crate::ports::OutboundHttp,
    request: MarketSyncRequest,
) -> Result<MarketSyncResult, AppError> {
    if request.to < request.from {
        return Err(AppError::Invalid {
            field: "to".to_owned(),
            expected: "date no earlier than from".to_owned(),
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
            format!("source returned HTTP {}", response.status),
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
        ParsedObservations::Prices { prices, accrued } => {
            let rows = prices.iter().map(price_row).collect::<Vec<_>>();
            let written = match store.record_prices(&handle, &response.raw_hash, &rows) {
                Ok(count) => count,
                Err(error) => return Err(fail_run(store, &handle, error)),
            };
            let accrued_rows = accrued.iter().map(accrued_interest_row).collect::<Vec<_>>();
            match store.record_accrued_interest(&handle, &response.raw_hash, &accrued_rows) {
                Ok(count) => written + count,
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

/// Synchronise the market through the application's assembled dependencies.
///
/// The server calls this façade rather than accessing the storage adapter:
/// source and recording orchestration remains in `iaam-app`.
pub async fn sync_market_with_services(
    services: &AppServices,
    request: MarketSyncRequest,
) -> Result<MarketSyncResult, AppError> {
    let mut store = services.market_store.lock().await;
    sync_market(&mut store, services.http.as_ref(), request).await
}

fn request_for(source: &MarketSource, from: Date, to: Date) -> HttpRequest {
    match source {
        MarketSource::Moex {
            engine,
            market,
            board,
            secid,
            ..
        } => history_request(HistoryQuery {
            engine,
            market,
            board,
            secid,
            from,
            till: to,
            start: 0,
        }),
        MarketSource::CbrDaily => daily_request(to),
        MarketSource::CbrDynamic {
            cbr_currency_id, ..
        } => dynamic_request(from, to, cbr_currency_id),
        MarketSource::CbrKeyRate => key_rate_request(from, to),
    }
}

enum ParsedObservations {
    Prices {
        prices: Vec<PriceObservation>,
        accrued: Vec<AccruedInterestObservation>,
    },
    Fx(Vec<FxObservation>),
    KeyRates(Vec<KeyRateObservation>),
}

fn parse_response(
    source: &MarketSource,
    body: &[u8],
    observed_at: iaam_market::ObservedAt,
) -> Result<ParsedObservations, AppError> {
    match source {
        MarketSource::Moex {
            instrument,
            engine,
            market,
            ..
        } => {
            let body = core::str::from_utf8(body)
                .map_err(|error| AppError::Store(format!("MOEX response is not UTF-8: {error}")))?;
            let prices = iaam_market::moex::parse::parse_history(
                body,
                *instrument,
                observed_at,
                iaam_market::moex::parse::MarketSegment { engine, market },
            )
            .map_err(|error| AppError::Store(error.to_string()))?;
            // The same response, the same knowledge coordinate: accrued interest is in the same
            // row as the price and requires no second request.
            let accrued =
                iaam_market::moex::parse::parse_accrued_interest(body, *instrument, observed_at)
                    .map_err(|error| AppError::Store(error.to_string()))?;
            Ok(ParsedObservations::Prices { prices, accrued })
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
            let body = core::str::from_utf8(body).map_err(|error| {
                AppError::Store(format!("Central Bank response is not UTF-8: {error}"))
            })?;
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
        quotation_basis: observation.basis.code().to_owned(),
        basis_evidence: observation.basis_evidence.clone(),
        executability: executability(observation.executability).to_owned(),
    }
}

fn accrued_interest_row(observation: &AccruedInterestObservation) -> AccruedInterestRow {
    AccruedInterestRow {
        instrument_id: observation.instrument.inner().to_string(),
        board: observation.venue.board.clone(),
        session: observation.venue.session,
        trade_date: observation.trade_date.0.to_string(),
        observed_at: observation
            .observed_at
            .0
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| observation.observed_at.0.to_string()),
        per_unit: observation.per_unit.value().inner().to_string(),
        currency: observation.per_unit.currency().code().to_owned(),
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
    use crate::ports::{OutboundHttp, OutboundResponse};
    use async_trait::async_trait;
    use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
    use iaam_core::money::CurrencyCode;
    use iaam_store::reference::InstrumentRecord;
    use time::macros::{date, datetime};

    #[test]
    fn one_bond_response_yields_both_prices_and_accrued_interest() {
        // ACCINT arrives in the same row as CLOSE. A second request
        // for it would be an unnecessary call to the source and a second
        // knowledge coordinate for the same row.
        let body = std::fs::read("../../tests/fixtures/market/moex-iss-history-ofz.json").unwrap();
        let parsed = parse_response(
            &MarketSource::Moex {
                instrument: iaam_core::ids::InstrumentId::new_random(),
                engine: "stock".to_owned(),
                market: "bonds".to_owned(),
                board: "TQOB".to_owned(),
                secid: "SU26238RMFS4".to_owned(),
            },
            &body,
            iaam_market::ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .unwrap();
        let ParsedObservations::Prices { prices, accrued } = parsed else {
            panic!("bond response must yield both types of observation");
        };
        assert!(!prices.is_empty());
        assert!(!accrued.is_empty());
    }

    struct FixtureTransport {
        body: Vec<u8>,
        status: u16,
    }

    #[async_trait]
    impl OutboundHttp for FixtureTransport {
        async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
            Ok(OutboundResponse {
                status: self.status,
                raw_hash: "fixture-hash".to_owned(),
                body: self.body.clone(),
            })
        }
    }

    #[tokio::test]
    async fn a_fixture_transport_publishes_market_rows_without_network() {
        let mut store = MarketStore::open_in_memory().expect("storage");
        let instrument = InstrumentId::new_random();
        store
            .upsert_instrument(&InstrumentRecord {
                id: instrument,
                kind: Some(InstrumentKind::Share),
                symbol: "SBER".to_owned(),
                title: "Sberbank".to_owned(),
                currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
                lineage: None,
            })
            .expect("instrument");
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
        .expect("synchronisation");

        let first_observed_at: String = store
            .connection()
            .query_row(
                "SELECT observed_at FROM price_observations
                 ORDER BY observed_at ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("time of first observation");

        let first_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM price_observations", [], |row| {
                row.get(0)
            })
            .expect("number of first observations");

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
        .expect("repeat synchronisation");

        let second_observed_at: String = store
            .connection()
            .query_row(
                "SELECT observed_at FROM price_observations
                 ORDER BY observed_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("time of repeat observation");
        let second_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM price_observations", [], |row| {
                row.get(0)
            })
            .expect("number of repeat observations");
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
                .expect("boundary"),
            Some(date!(2026 - 08 - 21))
        );
    }

    #[tokio::test]
    async fn a_source_failure_is_partial_and_keeps_the_previous_boundary() {
        let mut store = MarketStore::open_in_memory().expect("store");
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
        .expect("partial run");

        assert_eq!(result.status(), "partial");
        assert!(result.covered.is_none());
        assert_eq!(
            store
                .complete_through(&SeriesKey {
                    source_id: "cbr".to_owned(),
                    dataset: "key_rate".to_owned(),
                    series_key: "key_rate".to_owned(),
                })
                .expect("boundary"),
            None
        );
    }
}
