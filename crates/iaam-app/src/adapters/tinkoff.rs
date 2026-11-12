//! A `BrokerChannel` port over the parsed T-Invest channel.
//!
//! Response parsing stays in `iaam-broker`; this layer only requests
//! the body, quarantines rejected rows, and binds stable
//! port types.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use iaam_broker::BrokerInstrumentDescription;
use iaam_broker::operation_kind::OperationKindDictionary;
use iaam_broker::tinkoff::{
    ChannelMoney, ChannelOperation, ChannelOperationKind, ChannelOrderState,
    ChannelPortfolioPosition, GetOperationsByCursorRequest, InstrumentIdType, ParseError,
    TINKOFF_PARSER_VERSION, TinkoffClient, TinkoffError, parse_account_ids, parse_operations,
    parse_portfolio, parse_portfolio_positions,
};
use iaam_core::event::kind::{FeeOrigin, IncomeKind, TaxOrigin};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, InstrumentId, SourceId};
use iaam_core::money::{CalcMoney, CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::{Dimension, evidence::SourceChannel};
use iaam_core::rules::trade_allocation::{
    allocate_gross_minor, allocate_minor as core_allocate_minor,
};
use iaam_ingest::SubmittedOperation;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use rust_decimal::Decimal;
use uuid::Uuid;

use time::{Date, OffsetDateTime};

use crate::ports::{
    BrokerChannel, BrokerError, BrokerRequestContext, InstrumentDirectory, ParsedOperations,
    PortfolioAsOf, PortfolioSnapshot, Quarantined,
};

#[path = "tinkoff_report.rs"]
mod report;

#[path = "tinkoff_diagnostics.rs"]
mod diagnostics;

#[path = "tinkoff_fund_redemption.rs"]
mod fund_redemption;

#[path = "tinkoff_questions_timestamps.rs"]
mod questions_timestamps;

const BROKER: &str = "tinkoff";
const OPERATIONS_PAGE_LIMIT: i32 = 1_000;

#[derive(Clone, Copy)]
enum DescriptionMethod {
    Instrument,
    Currency,
}

type DescriptionParser<T> = fn(&str) -> Result<T, String>;

#[derive(Clone)]
struct CachedDescription<T = BrokerInstrumentDescription> {
    read_at: Instant,
    description: Result<T, String>,
    uid_not_found: bool,
    probes: HashSet<String>,
    broker_instrument_type: Option<String>,
    request_handle: String,
}

/// Broker channel implementation for T-Invest.
pub struct TinkoffChannel {
    client: TinkoffClient,
    source: SourceId,
    /// Dictionary of operation kinds for this channel. It arrives from storage
    /// ready to use: parsing in `iaam-broker` knows nothing about storage, and
    /// this adapter binds them — using the same approach already used
    /// for SQLite.
    dictionary: OperationKindDictionary,
    instruments: Arc<dyn InstrumentDirectory>,
    descriptions: tokio::sync::Mutex<HashMap<String, CachedDescription>>,
    diagnostic_operations: tokio::sync::Mutex<Vec<diagnostics::ReceivedOperation>>,
}

impl TinkoffChannel {
    /// Creates a channel with a preconfigured HTTP client, data source
    /// and operation kind dictionary.
    #[must_use]
    pub fn new(
        client: TinkoffClient,
        source: SourceId,
        dictionary: OperationKindDictionary,
        instruments: Arc<dyn InstrumentDirectory>,
    ) -> Self {
        Self {
            client,
            source,
            dictionary,
            instruments,
            descriptions: tokio::sync::Mutex::new(HashMap::new()),
            diagnostic_operations: tokio::sync::Mutex::new(Vec::new()),
        }
    }
    // FindInstrument results are diagnostic only; typed lookups resolve identity.
    async fn probe_instrument(
        &self,
        identifier: &str,
        id_type: Option<InstrumentIdType>,
        raw: &serde_json::Value,
        context: BrokerRequestContext<'_>,
    ) {
        let response = match id_type {
            Some(id_type) => {
                self.client
                    .get_instrument(identifier, id_type, context.deadline, context.allowance)
                    .await
            }
            None => {
                self.client
                    .find_instrument(identifier, context.deadline, context.allowance)
                    .await
            }
        };
        trace_instrument_lookup(raw, identifier, id_type, &response);
    }

    async fn request_description(
        &self,
        id: &str,
        id_type: InstrumentIdType,
        method: DescriptionMethod,
        context: BrokerRequestContext<'_>,
    ) -> Result<String, TinkoffError> {
        match method {
            DescriptionMethod::Instrument => {
                self.client
                    .get_instrument(id, id_type, context.deadline, context.allowance)
                    .await
            }
            DescriptionMethod::Currency => {
                self.client
                    .get_currency(id, id_type, context.deadline, context.allowance)
                    .await
            }
        }
    }

    async fn resolve_description<T: Clone>(
        &self,
        (id, id_type): (&str, InstrumentIdType),
        raw: &serde_json::Value,
        context: BrokerRequestContext<'_>,
        descriptions: &mut HashMap<String, CachedDescription<T>>,
        (method, parse): (DescriptionMethod, DescriptionParser<T>),
    ) -> Result<CachedDescription<T>, BrokerError> {
        let key = description_handle(id, id_type);
        let cache_hit = descriptions
            .get(&key)
            .is_some_and(|cached| cached.read_at.elapsed() < std::time::Duration::from_secs(3600));
        if !cache_hit {
            tracing::info!(
                phase = "describe_instruments",
                status = "start",
                instruments = 1,
                "reading broker instrument description"
            );
            let response = self.request_description(id, id_type, method, context).await;
            trace_description_lookup(raw, id, Some(id_type), method, &response);
            let broker_instrument_type = response.as_ref().ok().and_then(|body| {
                let value: serde_json::Value = serde_json::from_str(body).ok()?;
                value
                    .get("instrument")?
                    .get("instrumentType")?
                    .as_str()
                    .map(str::to_owned)
            });
            let uid_not_found = id_type == InstrumentIdType::Uid
                && matches!(
                    &response,
                    Err(TinkoffError::UnexpectedStatus { status: 404, .. })
                );
            let description = match response.map_err(tinkoff_error) {
                Ok(body) => parse(&body),
                Err(
                    BrokerError::Refused { detail, .. } | BrokerError::Unparsable { detail, .. },
                ) => Err(detail),
                Err(error) => return Err(error),
            };
            tracing::info!(
                phase = "describe_instruments",
                status = "complete",
                instruments = 1,
                described = usize::from(description.is_ok()),
                "broker instrument description read"
            );
            descriptions.insert(
                key.clone(),
                CachedDescription {
                    read_at: Instant::now(),
                    description,
                    uid_not_found,
                    probes: HashSet::new(),
                    broker_instrument_type,
                    request_handle: key.clone(),
                },
            );
        }
        let cached = descriptions.get_mut(&key).expect("description inserted");
        if cached.uid_not_found {
            let field = |name: &str| {
                raw.get(name)
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
            };
            for (identifier, fallback_type) in [
                (field("positionUid"), InstrumentIdType::PositionUid),
                (field("figi"), InstrumentIdType::Figi),
            ] {
                let Some(identifier) = identifier else {
                    continue;
                };
                let probe_key = format!("{fallback_type:?}:{identifier}");
                if !cached.probes.insert(probe_key) {
                    continue;
                }
                let response = self
                    .request_description(identifier, fallback_type, method, context)
                    .await;
                trace_description_lookup(raw, identifier, Some(fallback_type), method, &response);
                let not_found = matches!(
                    &response,
                    Err(TinkoffError::UnexpectedStatus { status: 404, .. })
                );
                match response.map_err(tinkoff_error) {
                    Ok(body) => {
                        cached.broker_instrument_type =
                            serde_json::from_str::<serde_json::Value>(&body)
                                .ok()
                                .and_then(|value| {
                                    value
                                        .get("instrument")?
                                        .get("instrumentType")?
                                        .as_str()
                                        .map(str::to_owned)
                                });
                        cached.request_handle = description_handle(identifier, fallback_type);
                        cached.description = parse(&body);
                        cached.uid_not_found = false;
                        tracing::info!(
                            phase = "resolve_instruments",
                            lookup_identifier = identifier,
                            lookup_type = ?fallback_type,
                            instrument_uid = id,
                            "broker instrument fallback answered"
                        );
                        break;
                    }
                    Err(BrokerError::Refused { .. }) if not_found => {}
                    Err(
                        BrokerError::Refused { detail, .. }
                        | BrokerError::Unparsable { detail, .. },
                    ) => {
                        cached.description = Err(detail);
                        cached.uid_not_found = false;
                        break;
                    }
                    Err(error) => {
                        // A budget or transient failure must remain retryable.
                        cached
                            .probes
                            .remove(&format!("{fallback_type:?}:{identifier}"));
                        return Err(error);
                    }
                }
            }
            if cached.uid_not_found && matches!(method, DescriptionMethod::Instrument) {
                for identifier in [Some(id), field("figi")].into_iter().flatten() {
                    if cached.probes.insert(format!("search:{identifier}")) {
                        self.probe_instrument(identifier, None, raw, context).await;
                    }
                }
            }
        }
        tracing::debug!(
            phase = "describe_instruments",
            request_handle = cached.request_handle,
            cache_hit,
            "broker description resolved"
        );
        Ok(cached.clone())
    }

    async fn resolve_instrument(
        &self,
        uid: Option<&str>,
        raw: &serde_json::Value,
        on: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<Result<InstrumentId, String>, BrokerError> {
        Ok(self
            .resolve_instrument_with_flag(uid, raw, on, context)
            .await?
            .map(|(instrument, _)| instrument))
    }

    async fn resolve_instrument_with_flag(
        &self,
        uid: Option<&str>,
        raw: &serde_json::Value,
        on: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<Result<(InstrumentId, bool), String>, BrokerError> {
        let (id, id_type) = match uid.filter(|value| !value.is_empty()) {
            Some(uid) => (uid, InstrumentIdType::Uid),
            None => match raw.get("figi").and_then(serde_json::Value::as_str).filter(|value| !value.is_empty()) {
                Some(figi) => (figi, InstrumentIdType::Figi),
                None => match raw.get("positionUid").and_then(serde_json::Value::as_str).filter(|value| !value.is_empty()) {
                    Some(position) => (position, InstrumentIdType::PositionUid),
                    None => return Ok(Err("instrument description cannot be requested: the row supplies neither instrument UID, FIGI nor position UID".to_owned())),
                },
            },
        };
        let mut descriptions = self.descriptions.lock().await;
        let cached = self
            .resolve_description(
                (id, id_type),
                raw,
                context,
                &mut descriptions,
                (DescriptionMethod::Instrument, |body| {
                    iaam_broker::tinkoff::parse_instrument(body).map_err(|error| error.to_string())
                }),
            )
            .await?;
        let request_handle = cached.request_handle;
        let description = cached.description;
        let uid_not_found = cached.uid_not_found;
        let broker_instrument_type = cached.broker_instrument_type;
        drop(descriptions);
        let description = match description {
            Ok(description) => description,
            Err(reason) => {
                if uid_not_found {
                    trace_instrument_miss(raw, id, id_type, "uid_not_found", None, None);
                }
                return Ok(Err(format!(
                    "instrument {id} description refused or unreadable: {reason}"
                )));
            }
        };
        tracing::debug!(
            phase = "describe_instruments",
            lookup_identifier = id,
            blocked_tca_flag = description.blocked_tca_flag,
            "broker instrument description parsed"
        );
        if description
            .isin
            .as_deref()
            .is_none_or(|isin| isin.trim().is_empty())
        {
            trace_instrument_miss(
                raw,
                id,
                id_type,
                "no_isin",
                broker_instrument_type.as_deref(),
                Some(&description),
            );
            return Ok(Err(format!("instrument {id} description names no ISIN")));
        }
        let instrument = super::catalogue::resolve_or_record(
            self.instruments.as_ref(),
            &description,
            self.source,
            on,
        )
        .await
        .map_err(|error| BrokerError::Adapter {
            broker: BROKER.to_owned(),
            detail: error.to_string(),
        })?;
        self.instruments
            .update_blocked_tca_flag(instrument, &request_handle, description.blocked_tca_flag)
            .await
            .map_err(|error| BrokerError::Adapter {
                broker: BROKER.to_owned(),
                detail: error.to_string(),
            })?;
        tracing::debug!(
            phase = "describe_instruments",
            instrument = %instrument.inner(),
            request_handle,
            instrument_uid = raw.get("instrumentUid").and_then(serde_json::Value::as_str),
            figi = raw.get("figi").and_then(serde_json::Value::as_str),
            position_uid = raw.get("positionUid").and_then(serde_json::Value::as_str),
            blocked_tca_flag = description.blocked_tca_flag,
            "broker instrument blocking flag refreshed"
        );
        Ok(Ok((instrument, description.blocked_tca_flag)))
    }
}

fn description_handle(id: &str, id_type: InstrumentIdType) -> String {
    let (kind, identifier) = match id_type {
        InstrumentIdType::Uid | InstrumentIdType::PositionUid => (
            if id_type == InstrumentIdType::Uid {
                "uid"
            } else {
                "position_uid"
            },
            Uuid::parse_str(id).map_or_else(|_| id.to_owned(), |uid| uid.to_string()),
        ),
        InstrumentIdType::Figi => ("figi", id.to_owned()),
    };
    format!("tinkoff:{kind}:{identifier}")
}

fn trace_instrument_lookup(
    raw: &serde_json::Value,
    identifier: &str,
    id_type: Option<InstrumentIdType>,
    response: &Result<String, TinkoffError>,
) {
    trace_description_lookup(
        raw,
        identifier,
        id_type,
        DescriptionMethod::Instrument,
        response,
    );
}

fn trace_description_lookup(
    raw: &serde_json::Value,
    identifier: &str,
    id_type: Option<InstrumentIdType>,
    method: DescriptionMethod,
    response: &Result<String, TinkoffError>,
) {
    let field = |name: &str| raw.get(name).and_then(serde_json::Value::as_str);
    let status = match response {
        Ok(_) => "200",
        Err(TinkoffError::UnexpectedStatus { status: 404, .. }) => "404",
        Err(_) => "other",
    };
    let broker_instrument_type = response.as_ref().ok().and_then(|body| {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        value
            .get("instrument")?
            .get("instrumentType")?
            .as_str()
            .map(str::to_owned)
    });
    macro_rules! trace_lookup {
        ($level:ident) => {
            tracing::$level!(
        phase = "describe_instruments",
        broker_instrument_type = broker_instrument_type.as_deref(),
        operation_id = field("id").or_else(|| field("operationId")),
        instrument_type = field("instrumentType"),
        instrument_kind = field("instrumentKind"),
        instrument_uid = field("instrumentUid"),
        position_uid = field("positionUid"),
        figi = field("figi"),
        isin = field("isin"),
        ticker = field("ticker"),
        lookup_method = match (method, id_type) {
            (DescriptionMethod::Currency, _) => "CurrencyBy",
            (DescriptionMethod::Instrument, Some(_)) => "GetInstrumentBy",
            (DescriptionMethod::Instrument, None) => "FindInstrument",
        },
        lookup_identifier = identifier,
        lookup_type = ?id_type,
        lookup_status = status,
        "broker instrument lookup"
    );
        };
    }
    if response.is_ok() {
        trace_lookup!(debug);
    } else {
        trace_lookup!(warn);
    }
}

fn trace_instrument_miss(
    raw: &serde_json::Value,
    lookup_identifier: &str,
    lookup_type: InstrumentIdType,
    reason: &str,
    broker_instrument_type: Option<&str>,
    description: Option<&BrokerInstrumentDescription>,
) {
    let field = |name: &str| raw.get(name).and_then(serde_json::Value::as_str);
    tracing::warn!(
        phase = "resolve_instruments",
        reason,
        operation_id = field("id").or_else(|| field("operationId")),
        parent_operation_id = field("parentOperationId"),
        operation_type = field("type").or_else(|| field("operationType")),
        instrument_type = field("instrumentType"),
        instrument_kind = field("instrumentKind"),
        figi = field("figi"),
        instrument_uid = field("instrumentUid"),
        position_uid = field("positionUid"),
        ticker = field("ticker").or_else(|| description.map(|description| description.ticker.as_str())),
        name = field("name").or_else(|| description.map(|description| description.name.as_str())),
        lookup_identifier,
        lookup_type = ?lookup_type,
        broker_instrument_type,
        "broker instrument resolution miss"
    );
}

fn received_by_type(operations: &[ChannelOperation]) -> std::collections::BTreeMap<&str, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for operation in operations {
        let kind = operation
            .raw
            .get("type")
            .or_else(|| operation.raw.get("operationType"))
            .and_then(serde_json::Value::as_str)
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| {
                if operation.source_kind.is_empty() {
                    "<missing>"
                } else {
                    &operation.source_kind
                }
            });
        *counts.entry(kind).or_default() += 1;
    }
    counts
}

#[async_trait]
impl BrokerChannel for TinkoffChannel {
    async fn diagnose_instruments(
        &self,
        instruments: &[InstrumentId],
        broker_account: &str,
        period: (Date, Date),
        context: BrokerRequestContext<'_>,
    ) {
        self.trace_instrument_history(instruments, broker_account, period, context)
            .await;
    }

    async fn diagnose_missing_fund_units(
        &self,
        instruments: &[InstrumentId],
        broker_account: &str,
        period: (Date, Date),
        context: BrokerRequestContext<'_>,
    ) {
        self.trace_missing_fund_units(instruments, broker_account, period, context)
            .await;
    }

    async fn fetch_account_numbers(
        &self,
        context: BrokerRequestContext<'_>,
    ) -> Result<Vec<String>, BrokerError> {
        let BrokerRequestContext {
            deadline,
            allowance,
        } = context;
        // The accounts listing has no deadline parameter on the client — its
        // frozen signature predates the sync's deadline — so the bound is
        // taken here, the way the Finam channel bounds every call.
        let body = bounded(deadline, self.client.get_accounts(allowance)).await?;
        parse_account_ids(&body).map_err(parse_error)
    }

    async fn fetch_operations(
        &self,
        account: AccountId,
        broker_account: &str,
        from: time::Date,
        to: time::Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, BrokerError> {
        let BrokerRequestContext {
            deadline,
            allowance,
        } = context;
        // The broker is asked for its own account number; the returned rows
        // are stamped with the owner's account in this system.
        let mut request = GetOperationsByCursorRequest::new(broker_account.to_owned());
        request.from = Some(rfc3339_midnight(from));
        request.to = Some(rfc3339_operation_end(to));
        request.limit = Some(OPERATIONS_PAGE_LIMIT);
        let mut operations = fetch_operation_pages(request, |request| async move {
            self.client
                .get_operations_by_cursor(&request, deadline, allowance)
                .await
        })
        .await?;
        tracing::info!(
            phase = "fetch_operations",
            received_by_type = ?received_by_type(&operations),
            "received T-Invest operation types"
        );
        questions_timestamps::trace_timestamps(&operations, from, to);
        let mut diagnostic_operations = diagnostics::capture(&operations);
        self.resolve_report_payments(&mut operations, broker_account, from, to, context)
            .await?;
        let received = operations.len();
        tracing::info!(
            phase = "resolve_instruments",
            status = "start",
            rows = operations.len(),
            strategy = "broker_description_isin",
            "broker sync phase"
        );
        let mut checked = Vec::new();
        let mut refused = Vec::new();
        let mut currency_cache = HashMap::new();
        let mut currency_rows = HashMap::new();
        let mut blocking_flags = HashMap::new();
        for (operation_index, mut operation) in operations.into_iter().enumerate() {
            let has_identity = operation.instrument_uid.is_some()
                || ["figi", "ticker", "isin", "positionUid"]
                    .into_iter()
                    .any(|key| {
                        operation
                            .raw
                            .get(key)
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|value| !value.is_empty())
                    });
            let kind = self.dictionary.kind_of(&operation.source_kind);
            let is_currency = operation
                .raw
                .get("instrumentKind")
                .and_then(serde_json::Value::as_str)
                == Some("INSTRUMENT_TYPE_CURRENCY")
                || operation
                    .raw
                    .get("instrumentType")
                    .and_then(serde_json::Value::as_str)
                    == Some("currency");
            let currency_trade = is_currency
                && matches!(kind, ChannelOperationKind::Buy | ChannelOperationKind::Sell);
            if currency_trade && operation.rejection.is_none() {
                let identity = operation
                    .instrument_uid
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .map(|id| (id, InstrumentIdType::Uid))
                    .or_else(|| {
                        operation
                            .figi
                            .as_deref()
                            .filter(|id| !id.is_empty())
                            .map(|id| (id, InstrumentIdType::Figi))
                    });
                let result = if let Some((id, id_type)) = identity {
                    self.resolve_description(
                        (id, id_type),
                        &operation.raw,
                        context,
                        &mut currency_cache,
                        (DescriptionMethod::Currency, |body| {
                            iaam_broker::tinkoff::parse::parse_currency_description(body)
                                .map_err(|error| error.to_string())
                        }),
                    )
                    .await?
                    .description
                } else {
                    Err("currency description requires instrument UID or FIGI".to_owned())
                };
                match result {
                    Ok(description) => {
                        tracing::debug!(phase = "describe_currency", iso_currency = description.nominal.currency().code(), settlement = description.settlement.code(), lot = description.lot, blocked_tca_flag = description.blocked_tca_flag, nominal = ?description.nominal.value(), "currency description read");
                        currency_rows.insert(operation.deduplication_key.clone(), description);
                    }
                    Err(reason) => {
                        refused.push(Quarantined {
                            reason: format!("currency description refused: {reason}"),
                            dimensions: [Dimension::Cash].into_iter().collect(),
                            raw: operation.raw,
                        });
                        continue;
                    }
                }
            } else if has_identity
                && operation.rejection.is_none()
                && !(is_currency && matches!(kind, ChannelOperationKind::Commission))
            {
                match self
                    .resolve_instrument_with_flag(
                        operation.instrument_uid.as_deref(),
                        &operation.raw,
                        operation.date.unwrap_or(from),
                        context,
                    )
                    .await?
                {
                    Ok((instrument, blocked_tca_flag)) => {
                        blocking_flags.insert(operation.operation_id.clone(), blocked_tca_flag);
                        if let Some(row) = diagnostic_operations.get_mut(operation_index) {
                            row.instrument = Some(instrument);
                        }
                        operation.instrument_uid = Some(instrument.inner().to_string())
                    }
                    Err(reason) => {
                        trace_quarantined_operation(
                            &operation.operation_id,
                            &operation.source_kind,
                            "instrument_resolution",
                        );
                        tracing::debug!(
                            phase = "resolve_instruments",
                            reason,
                            "operation set aside"
                        );
                        refused.push(Quarantined {
                            reason,
                            dimensions: dimensions_for_kind(
                                &self.dictionary.kind_of(&operation.source_kind),
                            ),
                            raw: operation.raw,
                        });
                        continue;
                    }
                }
            }
            checked.push(operation);
        }
        tracing::info!(
            phase = "resolve_instruments",
            status = "complete",
            accepted = checked.len(),
            quarantined = refused.len(),
            strategy = "broker_description_isin",
            "broker sync phase complete"
        );
        *self.diagnostic_operations.lock().await = diagnostic_operations;
        let (mut parsed, accounting) =
            adapt_operations_with_currencies(account, checked, &self.dictionary, &currency_rows)?;
        for pending in &mut parsed.pending_redemptions {
            pending.blocked_tca_flag = pending
                .withdrawal_operation_id
                .as_ref()
                .and_then(|id| blocking_flags.get(id))
                .copied()
                .unwrap_or(false);
        }
        let quarantined_instrument_resolution = refused.len();
        parsed.quarantined.extend(refused);
        tracing::info!(
            phase = "fetch_operations",
            status = "complete",
            received,
            source_rows_accepted = accounting.source_rows_accepted,
            quarantined = parsed.quarantined.len(),
            quarantined_instrument_resolution,
            quarantined_parse = accounting.quarantined_parse,
            quarantined_kind_not_in_dictionary = accounting.quarantined_kind_not_in_dictionary,
            quarantined_securities_transfer = accounting.quarantined_securities_transfer,
            quarantined_order_state = accounting.quarantined_order_state,
            quarantined_trade = accounting.quarantined_trade,
            quarantined_conversion = accounting.quarantined_conversion,
            reduced_no_execution_trades = accounting.reduced_no_execution_trades,
            reduced_paired_fund_withdrawals = accounting.reduced_paired_fund_withdrawals,
            emitted_operations =
                parsed.accepted.len() + parsed.journal.len() + parsed.pending_redemptions.len(),
            "T-Invest operations accounted"
        );
        Ok(parsed)
    }

    async fn fetch_portfolio(
        &self,
        account: AccountId,
        broker_account: &str,
        _at: time::Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        let BrokerRequestContext {
            deadline,
            allowance,
        } = context;
        let _ = account;
        let (body, observed_at) = self
            .client
            .get_portfolio(broker_account, deadline, allowance)
            .await
            .map_err(tinkoff_error)?;
        // The answer's own moment becomes its observation date: the UTC day
        // the transport first saw this answer, live or from the response
        // cache. A `Current` portfolio fetched yesterday and served today
        // stays dated yesterday (`iaam-vg8te.1.1`).
        let observed_on = observed_at.map(|moment| OffsetDateTime::from(moment).date());
        let mut raw: serde_json::Value =
            serde_json::from_str(&body).map_err(|error| unparsable(error.to_string()))?;
        let mut refused = Vec::new();
        let mut accepted_positions = Vec::new();
        let mut originals: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        let positions = raw
            .get_mut("positions")
            .and_then(serde_json::Value::as_array_mut);
        if let Some(positions) = positions {
            tracing::info!(
                phase = "resolve_instruments",
                status = "start",
                rows = positions.len(),
                strategy = "broker_description_isin",
                "resolving portfolio instruments"
            );
            for mut position in std::mem::take(positions) {
                if position
                    .get("instrumentType")
                    .and_then(serde_json::Value::as_str)
                    != Some("currency")
                {
                    let uid = position
                        .get("instrumentUid")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    match self
                        .resolve_instrument(uid.as_deref(), &position, _at, context)
                        .await?
                    {
                        Ok(instrument) => {
                            originals
                                .entry(instrument.inner().to_string())
                                .or_default()
                                .push(position.clone());
                            position["instrumentUid"] =
                                serde_json::json!(instrument.inner().to_string());
                        }
                        Err(reason) => {
                            tracing::debug!(
                                phase = "resolve_instruments",
                                reason,
                                "portfolio row set aside"
                            );
                            let dimensions = refusal_dimensions(
                                position
                                    .get("instrumentType")
                                    .and_then(serde_json::Value::as_str),
                            );
                            refused.push(Quarantined {
                                reason,
                                raw: position,
                                dimensions,
                            });
                            continue;
                        }
                    }
                }
                accepted_positions.push(position);
            }
            tracing::info!(
                phase = "resolve_instruments",
                status = "complete",
                accepted = accepted_positions.len(),
                quarantined = refused.len(),
                "portfolio instruments resolved"
            );
            *positions = accepted_positions;
        }
        let mut snapshot = adapt_portfolio(&raw.to_string(), observed_on)?;
        for refusal in &mut snapshot.refused {
            if let Some(uid) = refusal
                .raw
                .get("instrumentUid")
                .and_then(serde_json::Value::as_str)
            {
                if let Some(rows) = originals.get(uid) {
                    refusal.raw = if rows.len() == 1 {
                        rows[0].clone()
                    } else {
                        serde_json::json!({"positions": rows})
                    };
                }
            }
            tracing::debug!(
                phase = "map_portfolio",
                reason = refusal.reason,
                "portfolio row set aside"
            );
        }
        snapshot.refused.extend(refused);
        Ok(snapshot)
    }

    fn channel(&self) -> SourceChannel {
        SourceChannel {
            source: self.source,
            parser_version: ParserVersion(TINKOFF_PARSER_VERSION.to_owned()),
            document: None,
        }
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Account
    }
}

/// Turns the channel's portfolio answer into claims, refusing any instrument
/// two rows disagree about instead of guessing which one is right.
///
/// A row the parser could not read whole travels in `refused` beside the
/// duplicate refusals, each with its reason and its original JSON — one
/// instrument's unreadable row withholds an opinion about that instrument,
/// not about the rest of the holdings (`iaam-vg8te.1.2`).
///
/// `parse_portfolio` still does the lossy projection (§`parse_portfolio`'s
/// own doc comment): one `ControlClaim::PositionQuantity` per row, with no
/// way for either of two rows for one instrument to say why there are two.
/// `parse_portfolio_positions` carries the fields that would answer that —
/// `blocked`, `blockedLots`, `quantityLots` — so this function groups by
/// instrument there, drops every claim whose instrument has more than one
/// row, and reports the dropped rows as `refused` instead of silently
/// discarding an opinion the channel never gave.
fn adapt_portfolio(
    body: &str,
    observed_on: Option<Date>,
) -> Result<PortfolioSnapshot, BrokerError> {
    let parsed = parse_portfolio(body).map_err(parse_error)?;
    let positions = parse_portfolio_positions(body).map_err(parse_error)?;

    let mut by_instrument: HashMap<&str, Vec<&ChannelPortfolioPosition>> = HashMap::new();
    for position in &positions.positions {
        by_instrument
            .entry(position.instrument_uid.as_str())
            .or_default()
            .push(position);
    }
    let duplicated: HashSet<&str> = by_instrument
        .iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|(instrument, _)| *instrument)
        .collect();
    // Rows the parser set aside come first, in the channel's own order; the
    // duplicate refusals follow them.
    let mut refused: Vec<Quarantined> = parsed
        .refused
        .iter()
        .map(|row| Quarantined {
            raw: row.raw.clone(),
            // The same refusal shape the operations path prints: the named
            // variant for the machine, the sentence for the owner.
            reason: format!("{:?}: {}", row.reason, row.reason),
            dimensions: refusal_dimensions(row.instrument_type.as_deref()),
        })
        .collect();
    refused.extend(
        duplicated
            .iter()
            .map(|instrument| duplicate_position_refusal(instrument, &by_instrument[instrument])),
    );

    let claims = parsed
        .claims
        .into_iter()
        .filter(|claim| match claim {
            ControlClaim::PositionQuantity { instrument, .. } => {
                !duplicated.contains(instrument.inner().to_string().as_str())
            }
            ControlClaim::CashBalance { .. }
            | ControlClaim::CashTurnover { .. }
            | ControlClaim::FeesTotal { .. }
            | ControlClaim::IncomeTotal { .. }
            | ControlClaim::TaxWithheldTotal { .. } => true,
        })
        .collect();

    Ok(PortfolioSnapshot {
        as_of: PortfolioAsOf::Current,
        claims,
        refused,
        observed_on,
    })
}

/// The dimension a set-aside row taints, from the instrument type the parser
/// read off the row: a currency position is cash, a security is a position,
/// and a row broken enough that its type never got read taints the
/// conservative pair — neither dimension may be declared confirmed while one
/// of its rows is invisible (`iaam-vg8te.1.2`).
fn refusal_dimensions(instrument_type: Option<&str>) -> BTreeSet<Dimension> {
    match instrument_type {
        Some("currency") => [Dimension::Cash].into_iter().collect(),
        Some(_) => [Dimension::Positions].into_iter().collect(),
        None => [Dimension::Cash, Dimension::Positions]
            .into_iter()
            .collect(),
    }
}

/// Two or more portfolio rows the channel reported for one instrument,
/// refused as a pair rather than summed.
///
/// Summing is only valid if the rows are disjoint additive components, and
/// nothing here can establish that — a total and one of its own subsets
/// looks identical to two peers once `blocked`, `blockedLots` and
/// `quantityLots` are gone, which is exactly what plain
/// `ControlClaim::PositionQuantity` values are (`iaam-xep0`, T4). The
/// refusal names both rows by their own `positionUid` rather than reporting
/// two opaque duplicates.
fn duplicate_position_refusal(
    instrument_uid: &str,
    rows: &[&ChannelPortfolioPosition],
) -> Quarantined {
    let position_uids = rows
        .iter()
        .map(|row| row.position_uid.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let raw = serde_json::json!({
        "instrumentUid": instrument_uid,
        "positions": rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "positionUid": row.position_uid,
                    "quantity": row.quantity.0.inner().to_string(),
                    "quantityLots": row.quantity_lots.map(|value| value.0.inner().to_string()),
                    "blocked": row.blocked,
                    "blockedLots": row.blocked_lots.map(|value| value.0.inner().to_string()),
                    "classCode": row.class_code,
                })
            })
            .collect::<Vec<_>>(),
    });
    Quarantined {
        raw,
        reason: format!(
            "instrument {instrument_uid} carries {} portfolio rows (positionUid {position_uids}); \
             refused as a pair rather than summed — nothing establishes they are disjoint",
            rows.len()
        ),
        dimensions: [Dimension::Positions].into_iter().collect(),
    }
}

/// Runs one client call under the sync's deadline.
///
/// The accounts listing carries no deadline on the client, so the bound is
/// taken here, twice, exactly as the Finam channel takes it: a call whose
/// deadline has already passed never starts, and one still running is
/// dropped the moment the deadline fires — which cancels the in-flight
/// gateway call, so no wait outlives the sync.
async fn bounded(
    deadline: Option<Instant>,
    call: impl Future<Output = Result<String, TinkoffError>>,
) -> Result<String, BrokerError> {
    let call = async { call.await.map_err(tinkoff_error) };
    let Some(at) = deadline else {
        return call.await;
    };
    if Instant::now() >= at {
        return Err(unreachable_after_deadline());
    }
    match tokio::time::timeout_at(tokio::time::Instant::from(at), call).await {
        Ok(result) => result,
        Err(_elapsed) => Err(unreachable_after_deadline()),
    }
}

fn unreachable_after_deadline() -> BrokerError {
    BrokerError::Unreachable {
        broker: BROKER.to_owned(),
        detail: "the accounts request did not finish within the sync's deadline".to_owned(),
        retry_after: None,
    }
}

async fn fetch_operation_pages<F, Fut>(
    mut request: GetOperationsByCursorRequest,
    mut fetch: F,
) -> Result<Vec<ChannelOperation>, BrokerError>
where
    F: FnMut(GetOperationsByCursorRequest) -> Fut,
    Fut: Future<Output = Result<String, TinkoffError>>,
{
    let mut operations = Vec::new();
    let mut seen_cursors = HashSet::new();

    let mut fetched = 0;
    loop {
        // Every page is refetched on the next try, so the refusal says how
        // far this one got rather than leaving a slow sync looking like a
        // dead one.
        let body = fetch(request.clone())
            .await
            .map_err(|error| after_pages(tinkoff_error(error), fetched))?;
        fetched += 1;
        let page = parse_operations(&body).map_err(parse_error)?;
        tracing::debug!(
            phase = "fetch_operation_pages",
            page = fetched,
            received = page.operations.len(),
            "T-Invest operation page received"
        );
        operations.extend(page.operations);
        if !page.has_next {
            tracing::info!(
                phase = "fetch_operation_pages",
                status = "complete",
                pages_fetched = fetched,
                received = operations.len(),
                "T-Invest operation pages fetched"
            );
            return Ok(operations);
        }
        let Some(cursor) = page.next_cursor else {
            return Err(unparsable(
                "operations response is truncated: next-page cursor is missing",
            ));
        };
        if !seen_cursors.insert(cursor.clone()) {
            return Err(unparsable(format!(
                "operations response repeated cursor {cursor:?}"
            )));
        }
        request.cursor = Some(cursor);
    }
}

/// Names on an "unreachable, retry later" how many operation pages had
/// arrived before it. Another refusal is about the request, not the progress.
fn after_pages(error: BrokerError, fetched: usize) -> BrokerError {
    match error {
        BrokerError::Unreachable {
            broker,
            detail,
            retry_after,
        } => BrokerError::Unreachable {
            broker,
            detail: format!("{detail}; operation pages fetched before it: {fetched}"),
            retry_after,
        },
        other => other,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RowRefusal {
    /// A property of the row: it was read, and no fact can be built from it.
    Row {
        reason: String,
        dimensions: BTreeSet<Dimension>,
    },
    /// The adapter reached a state its own branching should have excluded.
    Adapter(String),
}

impl RowRefusal {
    fn with_dimensions(self, dimensions: BTreeSet<Dimension>) -> Self {
        match self {
            Self::Row { reason, .. } => Self::Row { reason, dimensions },
            Self::Adapter(detail) => Self::Adapter(detail),
        }
    }

    fn into_broker_error(self) -> BrokerError {
        match self {
            Self::Row { reason, .. } => unparsable(reason),
            Self::Adapter(detail) => BrokerError::Adapter {
                broker: BROKER.to_owned(),
                detail,
            },
        }
    }
}

#[derive(Debug, Default)]
struct OperationAccounting {
    source_rows_accepted: usize,
    reduced_no_execution_trades: usize,
    reduced_paired_fund_withdrawals: usize,
    quarantined_parse: usize,
    quarantined_kind_not_in_dictionary: usize,
    quarantined_securities_transfer: usize,
    quarantined_order_state: usize,
    quarantined_trade: usize,
    quarantined_conversion: usize,
}

#[cfg(test)]
fn adapt_operations(
    account: AccountId,
    operations: Vec<ChannelOperation>,
    dictionary: &OperationKindDictionary,
) -> Result<ParsedOperations, BrokerError> {
    adapt_operations_accounted(account, operations, dictionary).map(|(parsed, _)| parsed)
}

#[cfg(test)]
fn adapt_operations_accounted(
    account: AccountId,
    operations: Vec<ChannelOperation>,
    dictionary: &OperationKindDictionary,
) -> Result<(ParsedOperations, OperationAccounting), BrokerError> {
    adapt_operations_with_currencies(account, operations, dictionary, &HashMap::new())
}

fn adapt_operations_with_currencies(
    account: AccountId,
    operations: Vec<ChannelOperation>,
    dictionary: &OperationKindDictionary,
    currency_rows: &HashMap<String, iaam_broker::tinkoff::parse::CurrencyDescription>,
) -> Result<(ParsedOperations, OperationAccounting), BrokerError> {
    // An empty dictionary means an unconfigured channel, not an unknown broker.
    // Without this check, the owner would receive a rejection for every code
    // separately and investigate the broker instead of the configuration.
    if dictionary.is_empty() && !operations.is_empty() {
        return Err(unparsable(
            "the channel's operation kind dictionary is empty: there is nothing to parse the export with",
        ));
    }
    let mut accepted = Vec::new();
    let mut journal = Vec::new();
    let mut pending_redemptions = Vec::new();
    let mut quarantined = Vec::new();
    let mut accounting = OperationAccounting::default();
    let source_rows = operations.len();
    let mut operations = operations;
    let pairing = fund_redemption::prepare(&mut operations, dictionary);
    for (operation_index, mut operation) in operations.into_iter().enumerate() {
        let operation_id = if operation.operation_id.is_empty() {
            operation
                .raw
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        } else {
            operation.operation_id.clone()
        };
        let source_kind = if operation.source_kind.is_empty() {
            operation
                .raw
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        } else {
            operation.source_kind.clone()
        };
        // Take the payload before moving the operation into conversion; no later stage reads it.
        let currency_description = currency_rows.get(&operation.deduplication_key);
        let raw = std::mem::take(&mut operation.raw);
        let kind = dictionary.kind_of(&operation.source_kind);
        if let Some(rejection) = operation.rejection.as_ref() {
            accounting.quarantined_parse += 1;
            trace_quarantined_operation(&operation_id, &source_kind, "parse");
            let dimensions = if operation.source_kind.is_empty() {
                all_dimensions()
            } else {
                dimensions_for_kind(&kind)
            };
            quarantined.push(Quarantined {
                raw,
                reason: format!("{rejection:?}: {rejection}"),
                dimensions,
            });
            continue;
        }

        if let Some(reason) = pairing.refusals.get(&operation_index) {
            quarantined.push(Quarantined {
                raw,
                reason: reason.clone(),
                dimensions: cash_positions(),
            });
            continue;
        }
        if pairing.withdrawals.contains(&operation_index) {
            accounting.reduced_paired_fund_withdrawals += 1;
            continue;
        }
        if matches!(kind, ChannelOperationKind::Other(_)) {
            accounting.quarantined_kind_not_in_dictionary += 1;
            let dictionary = iaam_broker::tinkoff::dictionary_seed::TINKOFF_SEED_NAME;
            tracing::debug!(
                phase = "map_operations",
                operation_id,
                source_kind,
                dictionary,
                reason = "kind_not_in_dictionary",
                "T-Invest operation quarantined"
            );
            quarantined.push(Quarantined {
                raw,
                reason: format!(
                    "kind_not_in_dictionary: {source_kind} is absent from {dictionary}"
                ),
                dimensions: dimensions_for_kind(&kind),
            });
            continue;
        }
        if matches!(kind, ChannelOperationKind::SecuritiesTransferOut) {
            tracing::debug!(
                phase = "map_operations",
                operation_id,
                description = ?raw.get("description"),
                child_operation_ids = ?raw.get("childOperations").and_then(serde_json::Value::as_array).map(|children| children.iter().filter_map(|child| child.get("id").and_then(serde_json::Value::as_str)).collect::<Vec<_>>()),
                child_descriptions = ?raw.get("childOperations").and_then(serde_json::Value::as_array).map(|children| children.iter().filter_map(|child| child.get("description").and_then(serde_json::Value::as_str)).collect::<Vec<_>>()),
                destination_account = ?raw.get("destinationAccountId"),
                recipient_account = ?raw.get("recipientAccountId"),
                destination = "unpaired",
                "T-Invest outbound securities evidence; no receiving account inferred"
            );
        }
        if let Some(reason) = securities_transfer_reason(&kind) {
            accounting.quarantined_securities_transfer += 1;
            trace_quarantined_operation(&operation_id, &source_kind, "securities_transfer");
            quarantined.push(Quarantined {
                raw,
                reason: reason.to_owned(),
                dimensions: dimensions_for_kind(&kind),
            });
            continue;
        }
        if let Some(reason) = order_state_reason(&kind, &operation.state) {
            accounting.quarantined_order_state += 1;
            trace_quarantined_operation(&operation_id, &source_kind, "order_state");
            quarantined.push(Quarantined {
                raw,
                reason,
                dimensions: if matches!(
                    kind,
                    ChannelOperationKind::Buy | ChannelOperationKind::Sell
                ) {
                    cash_positions()
                } else {
                    dimensions_for_kind(&kind)
                },
            });
            continue;
        }
        if matches!(kind, ChannelOperationKind::Buy | ChannelOperationKind::Sell)
            && operation.trades.is_empty()
            && (currency_description.is_none()
                || (operation.quantity_done == iaam_core::money::Quantity::zero()
                    && matches!(
                        operation.state,
                        ChannelOrderState::Cancelled | ChannelOrderState::InProgress
                    )))
        {
            accounting.reduced_no_execution_trades += 1;
            tracing::debug!(
                phase = "map_operations",
                operation_id,
                source_kind,
                reason = "no_execution_trades",
                "T-Invest operation reduced"
            );
            continue;
        }
        if matches!(kind, ChannelOperationKind::BondRedemption) {
            if operation
                .quantity
                .is_some_and(|quantity| quantity == iaam_core::money::Quantity::zero())
            {
                match pending_redemption(account, &operation, raw.to_string()) {
                    Ok(mut pending) => {
                        if let Some((quantity, operation_id, withdrawal_date)) =
                            pairing.redemptions.get(&operation_index)
                        {
                            pending.withdrawn_quantity = Some(*quantity);
                            pending.withdrawal_operation_id = Some(operation_id.clone());
                            pending.withdrawal_date = Some(*withdrawal_date);
                        }
                        pending_redemptions.push(pending);
                    }
                    Err(error) => {
                        trace_quarantined_operation(&operation_id, &source_kind, "conversion");
                        quarantined.push(Quarantined {
                            raw,
                            reason: error.to_string(),
                            dimensions: cash_positions(),
                        });
                    }
                }
                continue;
            }
            match redemption_to_journal(account, operation) {
                Ok(event) => journal.push(event),
                Err(RowRefusal::Row { reason, .. }) => {
                    trace_quarantined_operation(&operation_id, &source_kind, "conversion");
                    quarantined.push(Quarantined {
                        raw,
                        reason,
                        dimensions: cash_positions(),
                    });
                }
                Err(error @ RowRefusal::Adapter(_)) => {
                    return Err(error.into_broker_error());
                }
            }
        } else if let Some(description) = currency_description {
            match currency_exchange_operations(account, operation, kind, description, &raw) {
                Ok(exchange) => accepted.extend(exchange),
                Err(error) => quarantined.push(Quarantined {
                    raw,
                    reason: format!("currency exchange: {error:?}"),
                    dimensions: [Dimension::Cash].into_iter().collect(),
                }),
            }
        } else if matches!(kind, ChannelOperationKind::Buy | ChannelOperationKind::Sell) {
            if let Some(reason) = trade_row_reason(&operation, &kind) {
                accounting.quarantined_trade += 1;
                trace_quarantined_operation(&operation_id, &source_kind, "trade");
                quarantined.push(Quarantined {
                    raw,
                    reason,
                    dimensions: cash_positions(),
                });
                continue;
            }
            let dimensions = dimensions_for_kind(&kind);
            match trade_operations(account, operation, kind)
                .map_err(|error| error.with_dimensions(dimensions))
            {
                Ok(operations) => accepted.extend(operations),
                Err(RowRefusal::Row { reason, dimensions }) => {
                    trace_quarantined_operation(&operation_id, &source_kind, "conversion");
                    quarantined.push(Quarantined {
                        raw,
                        reason,
                        dimensions,
                    });
                }
                Err(error @ RowRefusal::Adapter(_)) => {
                    return Err(error.into_broker_error());
                }
            }
        } else {
            let dimensions = dimensions_for_kind(&kind);
            match operation_to_submitted(account, operation, kind)
                .map_err(|error| error.with_dimensions(dimensions))
            {
                Ok(operation) => {
                    tracing::debug!(
                        phase = "map_operations",
                        operation_id,
                        source_kind,
                        mapped_kind = match &operation.kind {
                            OperationKind::Tax { .. } => "tax",
                            OperationKind::Fee { .. } => "fee",
                            _ => "operation",
                        },
                        "T-Invest operation mapped"
                    );
                    accepted.push(operation);
                }
                Err(RowRefusal::Row { reason, dimensions }) => {
                    trace_quarantined_operation(&operation_id, &source_kind, "conversion");
                    quarantined.push(Quarantined {
                        raw,
                        reason,
                        dimensions,
                    });
                }
                Err(error @ RowRefusal::Adapter(_)) => {
                    return Err(error.into_broker_error());
                }
            }
        }
    }
    accounting.source_rows_accepted = source_rows
        - quarantined.len()
        - accounting.reduced_no_execution_trades
        - accounting.reduced_paired_fund_withdrawals;
    accounting.quarantined_conversion = quarantined.len()
        - accounting.quarantined_parse
        - accounting.quarantined_kind_not_in_dictionary
        - accounting.quarantined_securities_transfer
        - accounting.quarantined_order_state
        - accounting.quarantined_trade;
    Ok((
        ParsedOperations {
            accepted,
            journal,
            pending_redemptions,
            quarantined,
        },
        accounting,
    ))
}

fn trace_quarantined_operation(operation_id: &str, source_kind: &str, reason: &'static str) {
    tracing::debug!(
        phase = "map_operations",
        operation_id,
        source_kind,
        reason,
        "T-Invest operation quarantined"
    );
}

fn all_dimensions() -> BTreeSet<Dimension> {
    Dimension::all().into_iter().collect()
}

fn cash_positions() -> BTreeSet<Dimension> {
    [Dimension::Cash, Dimension::Positions]
        .into_iter()
        .collect()
}

fn dimensions_for_kind(kind: &ChannelOperationKind) -> BTreeSet<Dimension> {
    match kind {
        ChannelOperationKind::Buy | ChannelOperationKind::Sell => cash_positions(),
        ChannelOperationKind::Dividend | ChannelOperationKind::Coupon => {
            [Dimension::Cash, Dimension::Income].into_iter().collect()
        }
        ChannelOperationKind::Commission
        | ChannelOperationKind::Tax
        | ChannelOperationKind::Deposit
        | ChannelOperationKind::Withdrawal
        | ChannelOperationKind::Transfer
        | ChannelOperationKind::BondAmortisation => [Dimension::Cash].into_iter().collect(),
        ChannelOperationKind::BondRedemption => cash_positions(),
        ChannelOperationKind::SecuritiesTransferIn
        | ChannelOperationKind::SecuritiesTransferOut => {
            [Dimension::Positions, Dimension::TaxBasis]
                .into_iter()
                .collect()
        }
        ChannelOperationKind::Other(_) => all_dimensions(),
    }
}

fn securities_transfer_reason(kind: &ChannelOperationKind) -> Option<&'static str> {
    match kind {
        ChannelOperationKind::SecuritiesTransferIn => {
            Some("inbound securities transfer: securities moved without a cash movement")
        }
        ChannelOperationKind::SecuritiesTransferOut => None,
        _ => None,
    }
}

fn order_state_reason(kind: &ChannelOperationKind, state: &ChannelOrderState) -> Option<String> {
    let state_name = match state {
        ChannelOrderState::Executed => return None,
        ChannelOrderState::Cancelled => "OPERATION_STATE_CANCELED".to_owned(),
        ChannelOrderState::InProgress => "OPERATION_STATE_PROGRESS".to_owned(),
        ChannelOrderState::Unspecified => "OPERATION_STATE_UNSPECIFIED".to_owned(),
        ChannelOrderState::Unrecognised(value) => format!("{value:?}"),
    };
    let is_trade = matches!(kind, ChannelOperationKind::Buy | ChannelOperationKind::Sell);
    if is_trade
        && matches!(
            state,
            ChannelOrderState::Cancelled | ChannelOrderState::InProgress
        )
    {
        return None;
    }
    let evidence = if is_trade {
        "the channel did not provide a usable order state"
    } else {
        "non-trade operation is refused for lack of evidence that money moved"
    };
    Some(format!("order state {state_name}: {evidence}"))
}
fn pending_redemption(
    account: AccountId,
    operation: &ChannelOperation,
    raw: String,
) -> Result<crate::ports::PendingFullRedemption, BrokerError> {
    let instrument = optional_instrument(operation)
        .map_err(RowRefusal::into_broker_error)?
        .ok_or_else(|| unparsable("bond redemption does not contain instrumentUid"))?;
    let effective_date = operation
        .date
        .ok_or_else(|| unparsable("bond redemption does not contain date"))?;
    let payment = operation
        .payment
        .filter(|payment| payment.amount.raw() > 0)
        .ok_or_else(|| unparsable("bond redemption requires positive payment"))?;
    Ok(crate::ports::PendingFullRedemption {
        account,
        instrument,
        effective_date,
        compensation: iaam_core::money::Money::new(payment.amount, payment.currency),
        idempotency_key: operation.deduplication_key.clone(),
        source_operation_id: operation.operation_id.clone(),
        withdrawn_quantity: None,
        blocked_tca_flag: false,
        withdrawal_operation_id: None,
        withdrawal_date: None,
        raw,
    })
}

fn redemption_to_journal(
    account: AccountId,
    operation: ChannelOperation,
) -> Result<iaam_ingest::journal_event::SubmittedJournalEvent, RowRefusal> {
    use iaam_core::event::corporate_action::CorporateAction;
    use iaam_core::money::Money;
    use iaam_ingest::journal_event::{JournalFact, SubmittedJournalEvent};

    let instrument = optional_instrument(&operation)?
        .ok_or_else(|| row_unparsable("bond redemption does not contain instrumentUid"))?;
    let effective_date = operation
        .date
        .ok_or_else(|| row_unparsable("bond redemption does not contain date"))?;
    let quantity = operation
        .quantity
        .filter(|quantity| quantity.0 > Dec::zero())
        .ok_or_else(|| row_unparsable("bond redemption requires positive quantity"))?;
    let payment = operation
        .payment
        .filter(|payment| payment.amount.raw() > 0)
        .ok_or_else(|| row_unparsable("bond redemption requires positive payment"))?;
    let compensation = Money::new(payment.amount, payment.currency);
    Ok(SubmittedJournalEvent {
        account,
        fact: JournalFact::CorporateAction(CorporateAction::Redemption {
            instrument,
            custody: None,
            quantity,
            principal_returned_per_unit: iaam_core::money::ReturnedPrincipal::Posted(compensation),
            compensation,
            effective_date,
            record_date: None,
            grounds: None,
        }),
        idempotency_key: Some(operation.deduplication_key),
        source_operation_id: Some(operation.operation_id),
    })
}

fn currency_exchange_operations(
    account: AccountId,
    operation: ChannelOperation,
    kind: ChannelOperationKind,
    description: &iaam_broker::tinkoff::parse::CurrencyDescription,
    raw: &serde_json::Value,
) -> Result<Vec<SubmittedOperation>, RowRefusal> {
    let date = operation
        .date
        .ok_or_else(|| row_unparsable("currency exchange lacks date"))?;
    let quantity = operation
        .quantity
        .ok_or_else(|| row_unparsable("currency exchange lacks quantity"))?;
    let foreign = iaam_core::money::currency_amount_from_nominal(description.nominal, quantity)
        .map_err(|error| row_unparsable(format!("currency quantity: {error}")))?;
    let payment = operation
        .payment
        .ok_or_else(|| row_unparsable("currency exchange lacks payment"))?;
    if payment.currency != description.settlement || payment.currency == foreign.currency() {
        return Err(row_unparsable(
            "currency exchange payment does not match distinct settlement currency",
        ));
    }
    let cash = iaam_core::money::Money::new(
        payment
            .magnitude()
            .ok_or_else(|| row_unparsable("invalid payment"))?,
        payment.currency,
    );
    let (sold, bought) = match kind {
        ChannelOperationKind::Buy if payment.amount.raw() < 0 => (cash, foreign),
        ChannelOperationKind::Sell if payment.amount.raw() > 0 => (foreign, cash),
        _ => {
            tracing::debug!(
                phase = "currency_exchange",
                operation_id = operation.operation_id,
                operation_type = operation.source_kind,
                state = ?raw.get("state"),
                description = ?raw.get("description"),
                parent_operation_id = ?operation.parent_operation_id,
                cancel_reason = ?operation.cancel_reason,
                cancel_date_time = ?raw.get("cancelDateTime"),
                trades_count = operation.trades.len(),
                trades_quantities = ?operation.trades.iter().map(|trade| trade.quantity.0.inner().to_string()).collect::<Vec<_>>(),
                date = ?raw.get("date"),
                instrument_type = ?raw.get("instrumentType"),
                instrument_kind = ?raw.get("instrumentKind"),
                ticker = ?raw.get("ticker"),
                instrument_uid = ?operation.instrument_uid,
                payment_units = ?raw.pointer("/payment/units"),
                payment_nano = ?raw.pointer("/payment/nano"),
                payment_currency = payment.currency.code(),
                quantity = %quantity.0.inner(),
                quantity_sign = if quantity.0 > Dec::zero() { "positive" } else if quantity.0 < Dec::zero() { "negative" } else { "zero" },
                payment_sign = if payment.amount.raw() < 0 { "negative" } else if payment.amount.raw() > 0 { "positive" } else { "zero" },
                expected_payment_sign = match kind { ChannelOperationKind::Buy => "negative", ChannelOperationKind::Sell => "positive", _ => "unsupported operation" },
                "currency exchange payment sign disagreement"
            );
            return Err(row_unparsable(
                "currency exchange payment sign disagrees with operation",
            ));
        }
    };
    let fee = if let Some(commission) = operation
        .commission
        .filter(|value| value.value() != Dec::zero())
    {
        let magnitude = CalcMoney::new(
            Dec::new(commission.value().inner().abs()),
            commission.currency(),
        );
        let amount = iaam_core::money::currency_amount_from_nominal(
            magnitude,
            iaam_core::money::Quantity(Dec::new(Decimal::ONE)),
        )
        .map_err(|error| row_unparsable(format!("currency exchange commission: {error}")))?;
        Some(amount)
    } else {
        None
    };
    let submitted = |kind, key: String, source_id: String| SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(date),
            ..OperationDates::default()
        },
        source_time: operation.source_time,
        idempotency_key: Some(key),
        source_operation_id: Some(source_id),
        source_position_id: None,
        source_category: None,
        owner_category: None,
        source_code: None,
        source_kind: None,
        description: None,
        counterparty: None,
    };
    let mut result = vec![submitted(
        OperationKind::CurrencyExchange { sold, bought },
        operation.deduplication_key.clone(),
        operation.operation_id.clone(),
    )];
    if let Some(fee) = fee {
        result.push(submitted(
            OperationKind::Fee {
                amount_minor: fee.amount().raw(),
                currency: fee.currency(),
                origin: FeeOrigin::Brokerage,
            },
            composite_identity(
                &operation.broker_account_id,
                &operation.operation_id,
                "currency-exchange-fee",
            ),
            format!(
                "{}#currency-exchange-fee",
                escape_component(&operation.operation_id)
            ),
        ));
    }
    Ok(result)
}

fn operation_to_submitted(
    account: AccountId,
    operation: ChannelOperation,
    kind: ChannelOperationKind,
) -> Result<SubmittedOperation, RowRefusal> {
    if let Some(rejection) = operation.rejection.as_ref() {
        return Err(row_unparsable(format!("row rejected: {rejection}")));
    }
    let kind = match kind {
        ChannelOperationKind::Buy | ChannelOperationKind::Sell => {
            return Err(RowRefusal::Adapter(
                "trading operations must be expanded from their execution trades".to_owned(),
            ));
        }
        // A coupon and a dividend must not be collapsed into a single receipt: the journal
        // stores the kind, and losing it here means losing it forever —
        // the event is immutable.
        kind @ (ChannelOperationKind::Dividend | ChannelOperationKind::Coupon) => {
            let (gross_minor, currency) = required_money(operation.payment, "payment")?;
            let income_kind = match kind {
                ChannelOperationKind::Coupon => IncomeKind::Coupon,
                ChannelOperationKind::Dividend => IncomeKind::Dividend,
                // The outer pattern has already narrowed the possibilities. This branch is unreachable
                // and must fail loudly, rather than substitute a dividend.
                other => {
                    return Err(RowRefusal::Adapter(format!(
                        "income kind mismatch: {other:?}"
                    )));
                }
            };
            OperationKind::Income {
                instrument: optional_instrument(&operation)?,
                gross_minor,
                currency,
                kind: Some(income_kind),
            }
        }
        ChannelOperationKind::Tax => {
            let payment = operation
                .payment
                .ok_or_else(|| row_unparsable("operation does not contain payment"))?;
            let amount_minor = payment
                .amount
                .checked_neg()
                .ok_or_else(|| row_unparsable("tax payment opposite sign is not representable"))?
                .raw();
            OperationKind::Tax {
                amount_minor,
                currency: payment.currency,
                origin: TaxOrigin::WithheldAtSource,
                instrument: optional_instrument(&operation)?,
            }
        }
        ChannelOperationKind::Commission => {
            let (amount_minor, currency) = required_money(operation.payment, "payment")?;
            OperationKind::Fee {
                amount_minor,
                currency,
                origin: FeeOrigin::Brokerage,
            }
        }
        ChannelOperationKind::Deposit => {
            let (amount_minor, currency) = required_money(operation.payment, "payment")?;
            OperationKind::Deposit {
                amount_minor,
                currency,
            }
        }
        ChannelOperationKind::Withdrawal => {
            let (amount_minor, currency) = required_money(operation.payment, "payment")?;
            OperationKind::Withdrawal {
                amount_minor,
                currency,
            }
        }
        ChannelOperationKind::SecuritiesTransferIn => {
            return Err(row_unparsable(
                "inbound securities transfer: securities moved without a cash movement",
            ));
        }
        ChannelOperationKind::SecuritiesTransferOut => {
            let instrument = optional_instrument(&operation)?.ok_or_else(|| {
                row_unparsable("outbound securities transfer does not contain instrumentUid")
            })?;
            let quantity = operation
                .quantity
                .filter(|quantity| quantity.0 > Dec::zero())
                .ok_or_else(|| {
                    row_unparsable("outbound securities transfer requires positive quantity")
                })?;
            if operation.date.is_none() {
                return Err(row_unparsable(
                    "outbound securities transfer does not contain date",
                ));
            }
            OperationKind::SecuritiesTransferOut {
                instrument,
                quantity: quantity.0,
            }
        }
        ChannelOperationKind::Transfer => {
            return Err(RowRefusal::Row {
                reason: "transfer does not contain a recipient account".to_owned(),
                dimensions: BTreeSet::new(),
            });
        }
        // Partial repayment still needs its own journal mapping. Final
        // redemption is routed to the journal before this conversion.
        ChannelOperationKind::BondAmortisation => {
            return Err(row_unparsable(
                "bond amortisation: the channel does not report the returned face value per unit \
                 or custody location — the fact is entered via the journal endpoint",
            ));
        }
        ChannelOperationKind::BondRedemption => {
            return Err(RowRefusal::Adapter(
                "bond redemption must be submitted through the journal path".to_owned(),
            ));
        }
        ChannelOperationKind::Other(kind) => {
            return Err(row_unparsable(format!(
                "unsupported operation kind: {kind}"
            )));
        }
    };

    Ok(SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: operation.date,
            ..OperationDates::default()
        },
        source_time: operation.source_time,
        idempotency_key: Some(operation.deduplication_key),
        source_operation_id: Some(operation.operation_id),
        // Not a trade: no position handle applies here.
        source_position_id: None,
        source_category: None,
        owner_category: None,
        source_code: None,
        source_kind: None,
        description: None,
        // The gateway names no counterparty on a broker/exchange operation:
        // this channel has never had one to carry.
        counterparty: None,
    })
}

fn trade_row_reason(operation: &ChannelOperation, kind: &ChannelOperationKind) -> Option<String> {
    if !operation.trades.is_empty() && operation.position_uid.is_none() {
        return Some("trading operation does not contain positionUid".to_owned());
    }
    for (index, trade) in operation.trades.iter().enumerate() {
        if operation.trades[..index]
            .iter()
            .any(|previous| previous.num == trade.num)
        {
            let count = operation
                .trades
                .iter()
                .filter(|candidate| candidate.num == trade.num)
                .count();
            return Some(format!(
                "duplicate trade num {:?} appears {count} times in one order",
                trade.num
            ));
        }
    }

    let trade_quantities = operation
        .trades
        .iter()
        .map(|trade| trade.quantity)
        .collect::<Vec<_>>();
    let Ok(total_quantity) = iaam_core::money::sum_quantities(&trade_quantities) else {
        return Some("trade quantity total is not representable".to_owned());
    };
    if total_quantity != operation.quantity_done {
        return Some(format!(
            "trade quantity total {} does not equal quantity_done {}",
            total_quantity.0.inner(),
            operation.quantity_done.0.inner()
        ));
    }
    let first_trade = operation.trades.first()?;
    let currency = first_trade.price.currency();
    if operation
        .trades
        .iter()
        .any(|trade| trade.price.currency() != currency)
    {
        return Some("trades report more than one currency".to_owned());
    }
    let Some(payment) = operation.payment else {
        return Some("trading order does not report payment".to_owned());
    };
    if payment.currency != currency {
        return Some(format!(
            "payment currency {:?} does not match trade currency {:?}",
            payment.currency, currency
        ));
    }
    if let Some(commission) = operation.commission {
        if commission.currency() != currency {
            return Some(format!(
                "commission currency {:?} does not match trade currency {:?}",
                commission.currency(),
                currency
            ));
        }
    }
    let accrued = match operation.accrued_interest {
        None => 0,
        Some(value) if value.currency != currency => {
            return Some(format!(
                "accrued interest currency {:?} does not match trade currency {:?}",
                value.currency, currency
            ));
        }
        Some(value) if value.amount.raw() < 0 => {
            return Some(format!(
                "accrued interest must not be negative: {}",
                value.amount.raw()
            ));
        }
        Some(value) => value.amount.raw(),
    };
    if accrued > 0 {
        let mut dates = operation
            .trades
            .iter()
            .map(|trade| trade.at.to_offset(time::UtcOffset::UTC).date().to_string())
            .collect::<Vec<_>>();
        dates.sort();
        dates.dedup();
        if dates.len() > 1 {
            return Some(format!(
                "trade fills span multiple UTC dates: {}",
                dates.join(", ")
            ));
        }
    }
    match kind {
        ChannelOperationKind::Buy if payment.amount.raw() >= 0 => {
            return Some(format!(
                "Buy payment must be negative, got {}",
                payment.amount.raw()
            ));
        }
        ChannelOperationKind::Sell if payment.amount.raw() <= 0 => {
            return Some(format!(
                "Sell payment must be positive, got {}",
                payment.amount.raw()
            ));
        }
        _ => {}
    }
    trade_gross_allocations(payment, accrued, &operation.trades, currency).err()
}

fn trade_operations(
    account: AccountId,
    operation: ChannelOperation,
    kind: ChannelOperationKind,
) -> Result<Vec<SubmittedOperation>, RowRefusal> {
    if !matches!(kind, ChannelOperationKind::Buy | ChannelOperationKind::Sell) {
        return Err(RowRefusal::Adapter(
            "trading conversion received a non-trade operation kind".to_owned(),
        ));
    }
    if operation.trades.is_empty() {
        return Ok(Vec::new());
    }
    let buy = matches!(kind, ChannelOperationKind::Buy);
    let instrument = required_instrument(&operation)?;
    let mut trades = operation.trades;
    trades.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.num.as_bytes().cmp(right.num.as_bytes()))
    });
    let currency = trades[0].price.currency();
    let payment = operation
        .payment
        .ok_or_else(|| row_unparsable("trading order does not report payment"))?;
    let accrued = operation
        .accrued_interest
        .map_or(0, |value| value.amount.raw());
    let gross_allocation =
        trade_gross_allocations(payment, accrued, &trades, currency).map_err(row_unparsable)?;
    if let Some(reconciliation) = gross_allocation.reconciliation {
        tracing::debug!(
            phase = "map_operations",
            reason = "cash_authoritative",
            operation_id = operation.operation_id,
            payment_minor = reconciliation.payment.raw(),
            trade_total_minor = reconciliation.trade_total.raw(),
            delta_minor = reconciliation.delta.raw(),
            "broker payment overrides calculated trade total"
        );
    }
    let commission_minor = posted_commission(operation.commission, currency)?;
    let commission_allocations = commission_minor
        .map(|total| allocate_minor(total, &trades))
        .transpose()?
        .unwrap_or_else(|| vec![0; trades.len()]);
    let accrued_known = operation.accrued_interest.is_some();
    let accrued_allocations = operation
        .accrued_interest
        .map(|value| allocate_minor(value.amount.raw(), &trades))
        .transpose()?
        .unwrap_or_else(|| vec![0; trades.len()]);

    trades
        .into_iter()
        .zip(commission_allocations)
        .zip(accrued_allocations)
        .zip(gross_allocation.amounts)
        .map(|(((trade, commission), accrued), gross)| {
            let gross_minor = gross.raw();
            let basis_fee = calc_money_from_minor(commission, currency);
            let accrued_interest_minor = accrued_known.then_some(accrued);
            // No custody here: the channel names no place of storage for a
            // trade, only an opaque `positionUid` handle, carried below as
            // `source_position_id` — where it is what it is (`iaam-xep0`,
            // T4).
            let operation_kind = if buy {
                OperationKind::Buy {
                    instrument,
                    custody: None,
                    quantity: trade.quantity.0,
                    gross_minor,
                    fee_minor: None,
                    basis_fee,
                    accrued_interest_minor,
                    currency: trade.price.currency(),
                }
            } else {
                OperationKind::Sell {
                    instrument,
                    custody: None,
                    quantity: trade.quantity.0,
                    gross_minor,
                    fee_minor: None,
                    basis_fee,
                    accrued_interest_minor,
                    currency: trade.price.currency(),
                }
            };
            let at = trade.at.to_offset(time::UtcOffset::UTC);
            Ok(SubmittedOperation {
                account,
                kind: operation_kind,
                dates: OperationDates {
                    trade: Some(at.date()),
                    ..OperationDates::default()
                },
                source_time: Some(at.time()),
                idempotency_key: Some(composite_identity(
                    &operation.broker_account_id,
                    &operation.operation_id,
                    &trade.num,
                )),
                source_operation_id: Some(format!(
                    "{}#{}",
                    escape_component(&operation.operation_id),
                    escape_component(&trade.num)
                )),
                // Verbatim: `trade_row_reason` has already refused every
                // trade whose order carries no `positionUid`, so this is
                // always present here.
                source_position_id: operation.position_uid.clone(),
                source_category: None,
                owner_category: None,
                source_code: None,
                source_kind: None,
                description: None,
                // A trade fill, not a cash statement row: the gateway names
                // no counterparty here either.
                counterparty: None,
            })
        })
        .collect()
}

fn trade_gross_allocations(
    payment: ChannelMoney,
    accrued: i64,
    trades: &[iaam_broker::tinkoff::ChannelTrade],
    currency: CurrencyCode,
) -> Result<iaam_core::rules::trade_allocation::GrossAllocation, String> {
    let payment = payment
        .magnitude()
        .ok_or_else(|| "payment magnitude is not representable".to_owned())?;
    let mut order = (0..trades.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        trades[*left]
            .num
            .as_bytes()
            .cmp(trades[*right].num.as_bytes())
    });
    let grosses = order
        .iter()
        .map(|index| {
            iaam_core::money::gross_for_fill(trades[*index].price, trades[*index].quantity)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("trade gross total is not representable: {error}"))?;
    let mut allocation =
        allocate_gross_minor(payment, PostedMinor::new(accrued), &grosses, currency)
            .map_err(|error| error.to_string())?;
    let mut amounts = vec![PostedMinor::new(0); trades.len()];
    for (position, index) in order.into_iter().enumerate() {
        amounts[index] = allocation.amounts[position];
    }
    allocation.amounts = amounts;
    Ok(allocation)
}

fn posted_commission(
    commission: Option<CalcMoney>,
    currency: CurrencyCode,
) -> Result<Option<i64>, RowRefusal> {
    let Some(commission) = commission else {
        return Ok(None);
    };
    let magnitude = CalcMoney::new(Dec::new(commission.value().inner().abs()), currency)
        .rounded_minor()
        .map_err(|error| row_unparsable(format!("commission cannot be rounded: {error}")))?
        .raw();
    magnitude
        .checked_abs()
        .map(Some)
        .ok_or_else(|| row_unparsable("commission magnitude is not representable"))
}

fn allocate_minor(
    total: i64,
    trades: &[iaam_broker::tinkoff::ChannelTrade],
) -> Result<Vec<i64>, RowRefusal> {
    let mut order: Vec<usize> = (0..trades.len()).collect();
    order.sort_by(|left, right| {
        trades[*left]
            .num
            .as_bytes()
            .cmp(trades[*right].num.as_bytes())
    });
    let weights = order
        .iter()
        .map(|index| trades[*index].quantity)
        .collect::<Vec<_>>();
    let allocations = core_allocate_minor(PostedMinor::new(total), &weights)
        .map_err(|error| row_unparsable(error.to_string()))?;
    let mut result = vec![0; trades.len()];
    for (position, index) in order.into_iter().enumerate() {
        result[index] = allocations[position].raw();
    }
    Ok(result)
}

fn calc_money_from_minor(value: i64, currency: CurrencyCode) -> Option<CalcMoney> {
    (value > 0).then(|| {
        CalcMoney::new(
            Dec::new(Decimal::new(value, currency.minor_units())),
            currency,
        )
    })
}

fn composite_identity(account: &str, operation_id: &str, trade_num: &str) -> String {
    format!(
        "{}/{}#{}",
        escape_component(account),
        escape_component(operation_id),
        escape_component(trade_num)
    )
}

fn escape_component(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('#', "%23")
        .replace('/', "%2F")
}

fn required_money(
    money: Option<ChannelMoney>,
    field: &'static str,
) -> Result<(i64, CurrencyCode), RowRefusal> {
    let money =
        money.ok_or_else(|| row_unparsable(format!("operation does not contain {field}")))?;
    Ok((money_amount(money, field)?, money.currency))
}

fn money_amount(money: ChannelMoney, field: &'static str) -> Result<i64, RowRefusal> {
    money
        .magnitude()
        .map(|amount| amount.raw())
        .ok_or_else(|| row_unparsable(format!("field {field} does not have a positive magnitude")))
}

fn required_instrument(operation: &ChannelOperation) -> Result<InstrumentId, RowRefusal> {
    let value = operation
        .instrument_uid
        .as_deref()
        .ok_or_else(|| row_unparsable("trading operation does not contain instrumentUid"))?;
    parse_instrument(value)
}

fn optional_instrument(operation: &ChannelOperation) -> Result<Option<InstrumentId>, RowRefusal> {
    operation
        .instrument_uid
        .as_deref()
        .map(parse_instrument)
        .transpose()
}

fn parse_instrument(value: &str) -> Result<InstrumentId, RowRefusal> {
    Uuid::parse_str(value)
        .map(InstrumentId)
        .map_err(|_| row_unparsable(format!("instrumentUid is not a UUID: {value}")))
}

fn rfc3339_midnight(date: time::Date) -> String {
    format!("{date}T00:00:00Z")
}

fn rfc3339_operation_end(date: time::Date) -> String {
    format!("{date}T23:59:59.999999999Z")
}

/// A transient failure the gateway gave up on is "unreachable, retry later",
/// with when in its detail; only T-Invest's permanent answer is a refusal.
fn tinkoff_error(error: TinkoffError) -> BrokerError {
    let detail = error.to_string();
    match error {
        TinkoffError::Unreachable { retry_after, .. } => BrokerError::Unreachable {
            broker: BROKER.to_owned(),
            detail,
            retry_after: Some(retry_after),
        },
        TinkoffError::Gateway(error) if error.is_broker_egress_refusal() => {
            BrokerError::Unreachable {
                broker: BROKER.to_owned(),
                detail,
                retry_after: error.retry_after(),
            }
        }
        TinkoffError::RequestCeiling { ceiling } => BrokerError::RequestCeiling {
            broker: BROKER.to_owned(),
            ceiling,
        },
        TinkoffError::InvalidToken
        | TinkoffError::MethodUnavailable { .. }
        | TinkoffError::UnexpectedStatus { .. } => BrokerError::Refused {
            broker: BROKER.to_owned(),
            detail,
        },
        TinkoffError::PartialResponse
        | TinkoffError::MalformedResponse
        | TinkoffError::RequestSerialization => unparsable(detail),
        // A method key without a budget, or a client or trust root that could
        // not be built, is this build's fault, not the broker's: retrying
        // later meets the same fault. Operational egress/tally refusals were
        // separated above so they remain actionable at the server boundary.
        TinkoffError::Gateway(_) | TinkoffError::Transport(_) => BrokerError::Adapter {
            broker: BROKER.to_owned(),
            detail,
        },
    }
}

fn parse_error(error: ParseError) -> BrokerError {
    unparsable(error.to_string())
}

fn unparsable(detail: impl Into<String>) -> BrokerError {
    BrokerError::Unparsable {
        broker: BROKER.to_owned(),
        detail: detail.into(),
    }
}

fn row_unparsable(detail: impl Into<String>) -> RowRefusal {
    RowRefusal::Row {
        reason: detail.into(),
        dimensions: BTreeSet::new(),
    }
}

/// A T-Invest on a fake clock, behind a real gateway: the adapter's tests and
/// the channel factory's share it.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    use iaam_http::gateway::{BUDGETS, Clock, Sleeper, Transport};
    use iaam_http::{Gateway, HttpError, HttpRequest, HttpResponse};

    /// A clock that moves only when something sleeps on it.
    pub(crate) struct FakeTime {
        now: Mutex<Instant>,
        slept: Mutex<Vec<Duration>>,
        wall: Mutex<SystemTime>,
    }

    impl FakeTime {
        pub(crate) fn slept(&self) -> Vec<Duration> {
            self.slept.lock().expect("sleeps").clone()
        }
    }

    impl Clock for FakeTime {
        fn now(&self) -> Instant {
            *self.now.lock().expect("clock")
        }

        fn now_boot(&self) -> Result<iaam_http::gateway::BootTime, String> {
            let elapsed = self
                .wall
                .lock()
                .expect("wall clock")
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|error| error.to_string())?;
            Ok(iaam_http::gateway::BootTime::new("test-boot", elapsed))
        }
    }

    impl Sleeper for FakeTime {
        fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.slept.lock().expect("sleeps").push(delay);
            *self.now.lock().expect("clock") += delay;
            *self.wall.lock().expect("wall clock") += delay;
            Box::pin(async {})
        }
    }

    /// One scripted answer. `reset` is what T-Invest puts in its
    /// `x-ratelimit-reset` header; like the real transport, the fake reads
    /// it only when the request declared that header.
    pub(crate) struct Answer {
        pub(crate) status: u16,
        pub(crate) body: String,
        pub(crate) reset: Option<Duration>,
        pub(crate) remaining: Option<u64>,
    }

    impl Answer {
        pub(crate) fn status(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                reset: None,
                remaining: None,
            }
        }
    }

    /// What the fake was asked: the request bodies, in order.
    pub(crate) type Log = Arc<Mutex<Vec<String>>>;

    pub(crate) struct FakeTinvest {
        script: Mutex<VecDeque<Answer>>,
        /// Answered once the script has run out.
        otherwise: Option<Answer>,
        log: Log,
        scripted_instruments: bool,
    }

    impl Transport for FakeTinvest {
        async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            let body = match request.body() {
                Some(iaam_http::RequestBody::Json(body)) => body.clone(),
                _ => String::new(),
            };
            if !self.scripted_instruments
                && request
                    .path()
                    .ends_with("InstrumentsService/GetInstrumentBy")
            {
                let value: serde_json::Value =
                    serde_json::from_str(&body).expect("instrument request");
                let id = value["id"].as_str().expect("instrument ID");
                return Ok(HttpResponse { status: 200, body: serde_json::json!({"instrument": {
                    "isin": if id == "eeeeeeee-0000-0000-0000-000000000001" { None } else { Some(format!("TEST-{id}")) }, "ticker": "TEST", "name": "Invented instrument",
                    "instrumentType": "share", "currency": "rub", "uid": id
                }}).to_string().into_bytes(), ..Default::default() });
            }
            self.log.lock().expect("log").push(body);
            let scripted = self.script.lock().expect("script").pop_front();
            let answer = scripted
                .as_ref()
                .or(self.otherwise.as_ref())
                .expect("the script ran out: more requests than expected");
            Ok(HttpResponse {
                status: answer.status,
                body: answer.body.as_bytes().to_vec(),
                rate_remaining: answer.remaining,
                rate_reset: answer.reset.map(|reset| reset.as_secs()),
                retry_after: answer
                    .reset
                    .filter(|_| request.reset_header() == Some("x-ratelimit-reset")),
                ..Default::default()
            })
        }
    }

    /// An invented instance database; the gateway's egress place is derived
    /// from it, created beside it, and initialized with the fixture tally.
    fn broker_egress_database() -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "iaam-app-tinkoff-test-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).expect("instance directory created");
        let database = directory.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database file written");
        // The fixture tally is initialized in the place derived from the
        // database, and records a boot of its own, so no test starts inside
        // the conservative empty-pair state.
        let place = iaam_http::egress_directory_for(&database).expect("place derived");
        std::fs::create_dir(&place).expect("egress place created");
        std::fs::write(
            place.join("outbound-tally"),
            "iaam-outbound-tally-v4\ngeneration\t0\nboot\tfixture-boot\nhigh-water\t0\n",
        )
        .expect("initialized tally created");
        std::fs::write(place.join("outbound-tally-generation"), "0\n")
            .expect("initialized generation created");
        database
    }

    /// A gateway over a fake T-Invest answering `script`, then `otherwise`.
    pub(crate) fn gateway(
        script: Vec<Answer>,
        otherwise: Option<Answer>,
    ) -> (Arc<Gateway<FakeTinvest>>, Log, Arc<FakeTime>) {
        gateway_with_instruments(script, otherwise, false)
    }

    pub(crate) fn gateway_with_instruments(
        script: Vec<Answer>,
        otherwise: Option<Answer>,
        scripted_instruments: bool,
    ) -> (Arc<Gateway<FakeTinvest>>, Log, Arc<FakeTime>) {
        let time = Arc::new(FakeTime {
            now: Mutex::new(Instant::now()),
            slept: Mutex::new(Vec::new()),
            wall: Mutex::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
        });
        let log = Log::default();
        let database = broker_egress_database();
        let gateway = Gateway::with_parts_for_database(
            FakeTinvest {
                script: Mutex::new(script.into()),
                otherwise,
                log: Arc::clone(&log),
                scripted_instruments,
            },
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
            iaam_http::BrokerEgress::On,
            &database,
        )
        .expect("the documented table is valid");
        (Arc::new(gateway), log, time)
    }
}

#[cfg(test)]
mod tests {
    use iaam_broker::tinkoff::{
        ChannelOrderState, ParseError, parse_operations as parse_operations_page, parse_portfolio,
    };
    use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
    use iaam_core::event::kind::{EventKind, IncomeKind};
    use iaam_core::event::provenance::ParserVersion;
    use iaam_core::ids::{AccountId, EventId, InstrumentId, OwnerId, SourceId};
    use iaam_core::projection::{ProjectionContext, project};
    use iaam_core::reconciliation::{Dimension, claim::ControlClaim};
    use iaam_core::rules::{LotRuleVersion, RuleRegistry};

    fn parse_operations(
        body: &str,
    ) -> Result<Vec<iaam_broker::tinkoff::ChannelOperation>, ParseError> {
        parse_operations_page(body).map(|page| page.operations)
    }
    use iaam_ingest::operation::{NormalizationContext, OperationKind, PARSER_VERSION, normalize};
    use uuid::Uuid;

    use super::{
        BrokerError, GetOperationsByCursorRequest, PortfolioAsOf, Quarantined, RowRefusal,
        adapt_operations, adapt_portfolio, bounded, fetch_operation_pages, operation_to_submitted,
        order_state_reason, rfc3339_operation_end, trade_operations,
    };
    use iaam_broker::operation_kind::OperationKindDictionary;

    #[tokio::test]
    async fn currency_descriptions_produce_cash_exchanges_and_separate_fee() {
        use iaam_core::money::CurrencyCode;
        for (
            code,
            operation_type,
            payment,
            nominal_units,
            nominal_nano,
            foreign_minor,
            rub_minor,
        ) in [
            ("usd", "OPERATION_TYPE_BUY", "-100", "1", 0, 200, -10000),
            ("usd", "OPERATION_TYPE_SELL", "100", "1", 0, -200, 10000),
            (
                "xau",
                "OPERATION_TYPE_BUY",
                "-100",
                "0",
                500000000,
                10000,
                -10000,
            ),
            (
                "xau",
                "OPERATION_TYPE_SELL",
                "100",
                "0",
                500000000,
                -10000,
                10000,
            ),
        ] {
            let row = serde_json::json!({"hasNext":false,"items":[{
                "cursor":"invented", "brokerAccountId":"invented", "id":"currency-trade",
                "date":"2026-08-20T10:11:12Z", "type":operation_type,
                "state":"OPERATION_STATE_EXECUTED", "instrumentKind":"INSTRUMENT_TYPE_CURRENCY",
                "instrumentUid":"aaaaaaaa-0000-0000-0000-000000000001", "quantity":"2",
                "payment":{"units":payment,"nano":0,"currency":"rub"},
                "commission":{"units":"1","nano":0,"currency":"rub"}
            }]});
            let description = serde_json::json!({"instrument":{
                "isoCurrencyName":code,"currency":"rub","lot":1,
                "nominal":{"currency":code,"units":nominal_units,"nano":nominal_nano}
            }});
            let (gateway, requests, _) = fake::gateway_with_instruments(
                vec![
                    Answer::status(200, &row.to_string()),
                    Answer::status(200, &description.to_string()),
                ],
                None,
                true,
            );
            let parsed = fetch(&channel(gateway))
                .await
                .expect("fetch currency trade");
            assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
            assert_eq!(parsed.accepted.len(), 2);
            assert_eq!(requests.lock().expect("requests").len(), 2);
            let context = NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
                parser_version: ParserVersion(PARSER_VERSION.to_owned()),
            };
            let events: Vec<_> = parsed
                .accepted
                .iter()
                .map(|item| normalize(item, &context).expect("normalize").event)
                .collect();
            assert!(matches!(events[0].kind, EventKind::CurrencyExchange { .. }));
            assert!(matches!(events[1].kind, EventKind::Fee { .. }));
            let foreign = if code == "usd" {
                CurrencyCode::Usd
            } else {
                CurrencyCode::Xau
            };
            assert_eq!(
                events[0]
                    .cash_effect(foreign)
                    .expect("foreign")
                    .amount()
                    .raw(),
                foreign_minor
            );
            assert_eq!(
                events[0]
                    .cash_effect(CurrencyCode::Rub)
                    .expect("rub")
                    .amount()
                    .raw(),
                rub_minor
            );
            assert_eq!(
                events[1]
                    .cash_effect(CurrencyCode::Rub)
                    .expect("fee")
                    .amount()
                    .raw(),
                -100
            );
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [AccountId(Uuid::from_u128(1))],
            );
            let rules = RuleRegistry::with_defaults();
            let snapshot = project(
                &events,
                &ProjectionContext {
                    contour: &contour,
                    rules: &rules,
                    lot_rule: LotRuleVersion(1),
                },
            )
            .expect("project");
            assert_eq!(snapshot.state().book().iter().count(), 0);
        }
    }

    #[tokio::test]
    async fn currency_sign_disagreement_keeps_refusal_and_logs_source_fields() {
        let row = serde_json::json!({"hasNext":false,"items":[{
            "cursor":"invented", "brokerAccountId":"invented", "id":"sign-proof",
            "date":"2026-08-20T10:11:12Z", "type":"OPERATION_TYPE_BUY",
            "state":"OPERATION_STATE_EXECUTED", "instrumentType":"currency",
            "instrumentUid":"invented-usd-uid", "ticker":"INVENTED-USD", "quantity":"2",
            "description":"Invented correction evidence", "parentOperationId":"invented-parent",
            "cancelReason":"Invented cancellation explanation", "cancelDateTime":"2026-08-21T09:00:00Z",
            "tradesInfo":{"trades":[{"num":"invented-trade", "date":"2026-08-20T10:11:12Z", "quantity":"2", "price":{"units":"50","nano":0,"currency":"rub"}}]},
            "payment":{"units":"100","nano":120000000,"currency":"rub"}
        }]});
        let description = serde_json::json!({"instrument":{
            "isoCurrencyName":"usd","currency":"rub","lot":1,
            "nominal":{"currency":"usd","units":"1","nano":0}
        }});
        let (gateway, _, _) = fake::gateway_with_instruments(
            vec![
                Answer::status(200, &row.to_string()),
                Answer::status(200, &description.to_string()),
            ],
            None,
            true,
        );
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let parsed = fetch(&channel(gateway))
            .await
            .expect("fetch sign disagreement");
        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(
            parsed.quarantined[0]
                .reason
                .contains("payment sign disagrees")
        );
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        for evidence in [
            "sign-proof",
            "INVENTED-USD",
            "invented-usd-uid",
            "payment_units",
            "payment_nano",
            "120000000",
            "payment_currency",
            "quantity_sign",
            "expected_payment_sign",
            "negative",
            "OPERATION_STATE_EXECUTED",
            "Invented correction evidence",
            "invented-parent",
            "Invented cancellation explanation",
            "2026-08-21T09:00:00Z",
            "trades_count=1",
            "trades_quantities=[\"2\"]",
            "2026-08-20T10:11:12Z",
        ] {
            assert!(log.contains(evidence), "missing {evidence} in {log}");
        }
    }

    #[test]
    fn tax_codes_map_to_signed_tax_facts_with_optional_instruments() {
        let (dictionary, unreadable) = OperationKindDictionary::build(
            iaam_broker::tinkoff::dictionary_seed::TINKOFF_OPERATION_KINDS
                .iter()
                .copied(),
        );
        assert!(unreadable.is_empty());
        let instrument = InstrumentId(Uuid::from_u128(71));
        for (source_kind, units, expected_cash, named) in [
            (
                "OPERATION_TYPE_DIVIDEND_TAX",
                "-11",
                -1100,
                Some(instrument),
            ),
            ("OPERATION_TYPE_TAX", "-8", -800, None),
            ("OPERATION_TYPE_TAX_CORRECTION", "-3", -300, None),
            ("OPERATION_TYPE_TAX_CORRECTION", "3", 300, Some(instrument)),
        ] {
            let body = serde_json::json!({
                "hasNext": false,
                "items": [{
                    "cursor": "invented-tax-cursor",
                    "brokerAccountId": "invented-tax-account",
                    "id": "invented-tax-row",
                    "date": "2026-08-20T10:11:12Z",
                    "type": source_kind,
                    "state": "OPERATION_STATE_EXECUTED",
                    "instrumentUid": named.map(|id| id.inner().to_string()),
                    "payment": {"units": units, "nano": 0, "currency": "rub"}
                }]
            });
            let parsed = adapt_operations(
                AccountId::new_random(),
                parse_operations(&body.to_string()).expect("parse"),
                &dictionary,
            )
            .expect("adapt");
            assert!(parsed.quarantined.is_empty(), "{source_kind}");
            assert_eq!(parsed.accepted.len(), 1);
            let context = NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
                parser_version: ParserVersion(super::TINKOFF_PARSER_VERSION.to_owned()),
            };
            let event = normalize(&parsed.accepted[0], &context)
                .expect("normalize tax")
                .event;
            event.validate_structure().expect("tax structure");
            assert!(
                matches!(&event.kind, EventKind::Tax { instrument, origin: iaam_core::event::kind::TaxOrigin::WithheldAtSource, .. } if *instrument == named)
            );
            assert_eq!(
                event
                    .cash_effect(iaam_core::money::CurrencyCode::Rub)
                    .expect("cash")
                    .amount()
                    .raw(),
                expected_cash
            );
        }
    }

    fn broker_context(
        deadline: Option<std::time::Instant>,
    ) -> crate::ports::BrokerRequestContext<'static> {
        static ALLOWANCE: std::sync::LazyLock<iaam_http::RequestAllowance> =
            std::sync::LazyLock::new(|| iaam_http::RequestAllowance::new(u32::MAX));
        crate::ports::BrokerRequestContext {
            deadline,
            allowance: &ALLOWANCE,
        }
    }

    #[derive(Clone, Default)]
    struct InstrumentLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for InstrumentLog {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("instrument log")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn report_operation() -> iaam_broker::tinkoff::ChannelOperation {
        let body = serde_json::json!({"hasNext": false, "items": [{
            "cursor":"c", "brokerAccountId":"account", "id":"operation", "figi":"FIGI1",
            "date":"2026-08-20T10:00:00Z", "type":"OPERATION_TYPE_BUY", "state":"OPERATION_STATE_EXECUTED",
            "quantity":"92", "payment":{"currency":"rub", "units":"-173", "nano":-926000000},
            "tradesInfo":{"trades":[{"num":"trade-1", "date":"2026-08-20T10:00:00Z", "quantity":"92",
                "price":{"currency":"rub", "units":"1", "nano":890500000}}]}
        }]});
        parse_operations(&body.to_string()).unwrap().remove(0)
    }

    #[tokio::test(start_paused = true)]
    async fn position_refusal_diagnostics_skip_near_and_elapsed_deadlines_without_requests() {
        let (gateway, requests, _) = fake::gateway(Vec::new(), None);
        let channel = channel(gateway);
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let started = tokio::time::Instant::now();
        for deadline in [
            started.into_std() - std::time::Duration::from_secs(1),
            started.into_std(),
            started.into_std() + crate::ports::BROKER_DIAGNOSTIC_RESERVE,
        ] {
            channel
                .trace_instrument_history(
                    &[InstrumentId::new_random()],
                    "account",
                    (
                        time::macros::date!(2026 - 08 - 01),
                        time::macros::date!(2026 - 08 - 31),
                    ),
                    broker_context(Some(deadline)),
                )
                .await;
        }
        assert!(requests.lock().unwrap().is_empty());
        assert_eq!(tokio::time::Instant::now(), started);
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert_eq!(log.matches("status=\"skipped\"").count(), 3, "{log}");
        assert!(log.contains("phase=\"position_refusal_history\""), "{log}");
        assert!(
            log.contains("reason=\"sync_deadline_near_or_elapsed\""),
            "{log}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn position_refusal_diagnostics_cancel_a_local_mutex_wait_at_the_sync_deadline() {
        let (gateway, requests, _) = fake::gateway(vec![Answer::status(200, "{}")], None);
        let channel = channel(gateway);
        // A normal sync has already made required calls before diagnostics.
        channel
            .client
            .generate_broker_report(
                "invented",
                "2026-08-01T00:00:00Z",
                "2026-08-31T00:00:00Z",
                None,
                broker_context(None).allowance,
            )
            .await
            .unwrap();
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let _held = channel.diagnostic_operations.lock().await;
        let started = tokio::time::Instant::now();
        let bound = std::time::Duration::from_secs(5);
        channel
            .trace_instrument_history(
                &[InstrumentId::new_random()],
                "account",
                (
                    time::macros::date!(2026 - 08 - 01),
                    time::macros::date!(2026 - 08 - 31),
                ),
                broker_context(Some((started + bound).into_std())),
            )
            .await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= bound,
            "diagnostic did not wait for its deadline: {elapsed:?}"
        );
        assert!(
            elapsed <= bound + std::time::Duration::from_millis(1),
            "diagnostic outlived its deadline: {elapsed:?}"
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("phase=\"position_refusal_history\""), "{log}");
        assert!(log.contains("status=\"deadline_elapsed\""), "{log}");
    }

    #[test]
    fn received_type_counts_include_unmapped_and_refused_operations() {
        let mut input = report_operation();
        input.source_kind = "OPERATION_TYPE_INPUT_SECURITIES".to_owned();
        input.raw = serde_json::json!({"type":"OPERATION_TYPE_INPUT_SECURITIES"});
        let mut unknown = report_operation();
        unknown.source_kind.clear();
        unknown.raw = serde_json::json!({"type":"OPERATION_TYPE_NOT_IN_DICTIONARY"});
        unknown.rejection = Some(iaam_broker::tinkoff::ParseError::PartialResponse);
        assert_eq!(
            super::received_by_type(&[input.clone(), unknown, input]),
            std::collections::BTreeMap::from([
                ("OPERATION_TYPE_INPUT_SECURITIES", 2),
                ("OPERATION_TYPE_NOT_IN_DICTIONARY", 1),
            ])
        );
    }

    #[tokio::test]
    async fn position_refusal_diagnostics_skip_low_remaining_without_report_requests() {
        let (gateway, requests, _) = fake::gateway(
            vec![Answer {
                status: 200,
                body: "{\"hasNext\":false,\"items\":[]}".to_owned(),
                reset: Some(Duration::from_secs(30)),
                remaining: Some(5),
            }],
            None,
        );
        let channel = channel(gateway);
        channel
            .client
            .generate_broker_report(
                "invented",
                "2026-08-01T00:00:00Z",
                "2026-08-31T00:00:00Z",
                None,
                broker_context(None).allowance,
            )
            .await
            .unwrap();
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        channel
            .trace_instrument_history(
                &[InstrumentId::new_random()],
                "invented",
                (
                    time::macros::date!(2026 - 08 - 01),
                    time::macros::date!(2026 - 08 - 31),
                ),
                broker_context(None),
            )
            .await;
        assert_eq!(requests.lock().unwrap().len(), 1);
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("low_rate_capacity_or_host_pause"), "{log}");
    }

    #[tokio::test]
    async fn missing_fund_units_cross_checks_only_formation_windows_without_recording() {
        let legacy = serde_json::json!({"operations":[
            {"id":"invented-arrival", "operationType":"OPERATION_TYPE_INPUT_SECURITIES", "figi":"OLD_FIGI", "isin":"INVENTED_TWIN_ISIN", "quantity":"3", "date":"2023-11-01T12:00:00Z"},
            {"id":"unrelated-legacy", "operationType":"OPERATION_TYPE_OUTPUT", "figi":"UNRELATED"}
        ]});
        let report = serde_json::json!({"getBrokerReportResponse":{
            "brokerReport":[{"figi":"OLD_FIGI", "name":"invented historical fund", "quantity":"3"}, {"figi":"UNRELATED", "name":"unrelated report name"}],
            "itemsCount":2,"pagesCount":1,"page":0
        }});
        let mut answers = vec![Answer::status(200, "{}")];
        for _ in 0..3 {
            answers.push(Answer::status(200, &legacy.to_string()));
        }
        for _ in 0..3 {
            answers.push(Answer::status(
                200,
                r#"{"generateBrokerReportResponse":{"taskId":"invented-formation-task"}}"#,
            ));
            answers.push(Answer::status(200, &report.to_string()));
        }
        let (gateway, requests, _) = fake::gateway(answers, None);
        let channel = channel(gateway);
        channel
            .client
            .generate_broker_report(
                "invented",
                "2026-08-01T00:00:00Z",
                "2026-08-31T00:00:00Z",
                None,
                broker_context(None).allowance,
            )
            .await
            .unwrap();
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let affected = InstrumentId::new_random();
        let mut operation = report_operation();
        operation.raw = serde_json::json!({"id":"invented-withdrawal", "figi":"NEW_FIGI", "isin":"INVENTED_TWIN_ISIN", "date":"2026-08-20T00:00:00Z"});
        let mut rows = super::diagnostics::capture(&[operation]);
        rows[0].instrument = Some(affected);
        *channel.diagnostic_operations.lock().await = rows;
        channel
            .trace_missing_fund_units(
                &[affected],
                "invented",
                (
                    time::macros::date!(2015 - 01 - 01),
                    time::macros::date!(2026 - 11 - 12),
                ),
                broker_context(None),
            )
            .await;
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 10);
        let legacy_requests: Vec<serde_json::Value> = sent[1..4]
            .iter()
            .map(|body| serde_json::from_str(body).unwrap())
            .collect();
        assert_eq!(legacy_requests[0]["from"], "2023-10-01T00:00:00Z");
        assert_eq!(legacy_requests[2]["to"], "2023-12-31T23:59:59.999999999Z");
        for request in &legacy_requests {
            let from = time::OffsetDateTime::parse(
                request["from"].as_str().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            let to = time::OffsetDateTime::parse(
                request["to"].as_str().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            assert!(to - from <= time::Duration::days(31));
        }
        for (request, legacy) in sent[4..].iter().step_by(2).zip(&legacy_requests) {
            let request: serde_json::Value = serde_json::from_str(request).unwrap();
            assert_eq!(
                request["generateBrokerReportRequest"]["from"],
                legacy["from"]
            );
            assert_eq!(request["generateBrokerReportRequest"]["to"], legacy["to"]);
        }
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("invented-arrival"), "{log}");
        assert!(log.contains("invented historical fund"), "{log}");
        assert!(!log.contains("unrelated-legacy"), "{log}");
        assert!(!log.contains("unrelated report name"), "{log}");
    }

    #[tokio::test]
    async fn missing_fund_units_logs_local_inputs_even_when_network_capacity_is_low() {
        let (gateway, requests, _) = fake::gateway(
            vec![Answer {
                status: 200,
                body: "{\"hasNext\":false,\"items\":[]}".to_owned(),
                reset: Some(Duration::from_secs(30)),
                remaining: Some(5),
            }],
            None,
        );
        let channel = channel(gateway);
        channel
            .client
            .generate_broker_report(
                "invented",
                "2026-08-01T00:00:00Z",
                "2026-08-31T00:00:00Z",
                None,
                broker_context(None).allowance,
            )
            .await
            .unwrap();
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let mut operation = report_operation();
        operation.raw = serde_json::json!({"id":"invented-unrelated-input", "type":"OPERATION_TYPE_INPUT_SECURITIES", "figi":"UNRELATED_FIGI", "date":"2023-11-01T00:00:00Z", "quantity":"3"});
        *channel.diagnostic_operations.lock().await = super::diagnostics::capture(&[operation]);
        channel
            .trace_missing_fund_units(
                &[InstrumentId::new_random()],
                "invented",
                (
                    time::macros::date!(2026 - 08 - 01),
                    time::macros::date!(2026 - 08 - 31),
                ),
                broker_context(None),
            )
            .await;
        assert_eq!(requests.lock().unwrap().len(), 1);
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("low_rate_capacity_or_host_pause"), "{log}");
        assert!(log.contains("invented-unrelated-input"), "{log}");
        assert!(log.contains("cursor_received"), "{log}");
    }

    #[tokio::test]
    async fn position_refusal_diagnostics_read_report_and_log_only_related_selected_rows() {
        let affected = InstrumentId::new_random();
        let page = serde_json::json!({"getBrokerReportResponse":{"brokerReport":[
            {"figi":"FIGI1", "ticker":"INVENTED", "direction":"Buy", "quantity":"7", "tradeDatetime":"2026-08-20T10:00:00Z", "party":"hidden-report-party"},
            {"figi":"UNRELATED", "ticker":"OTHER", "direction":"hidden-direction"}
        ], "itemsCount":2,"pagesCount":1,"page":0}});
        let (gateway, requests, _) = fake::gateway(
            vec![
                Answer::status(200, "{}"),
                Answer::status(
                    200,
                    r#"{"generateBrokerReportResponse":{"taskId":"diagnostic-task"}}"#,
                ),
                Answer::status(200, &page.to_string()),
            ],
            None,
        );
        let channel = channel(gateway);
        // A normal sync has already made required calls before diagnostics.
        channel
            .client
            .generate_broker_report(
                "invented",
                "2026-08-01T00:00:00Z",
                "2026-08-31T00:00:00Z",
                None,
                broker_context(None).allowance,
            )
            .await
            .unwrap();
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let mut operation = report_operation();
        operation.raw = serde_json::json!({"id":"source-proof", "date":"2026-08-20T10:00:00Z", "figi":"FIGI1", "description":"invented-history", "payment":{"units":"-1","nano":0}, "brokerAccountId":"hidden-account"});
        let mut rows = super::diagnostics::capture(&[operation]);
        rows[0].instrument = Some(affected);
        *channel.diagnostic_operations.lock().await = rows;
        channel
            .trace_instrument_history(
                &[affected],
                "account",
                (
                    time::macros::date!(2015 - 01 - 01),
                    time::macros::date!(2026 - 11 - 12),
                ),
                broker_context(None),
            )
            .await;
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 3);
        let request: serde_json::Value = serde_json::from_str(&sent[1]).unwrap();
        let period = &request["generateBrokerReportRequest"];
        let from = time::OffsetDateTime::parse(
            period["from"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let to = time::OffsetDateTime::parse(
            period["to"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(to - from <= time::Duration::days(31));
        assert!(from.date() <= time::macros::date!(2026 - 08 - 20));
        assert!(to.date() >= time::macros::date!(2026 - 08 - 20));
        drop(sent);
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("invented-history"));
        assert!(log.contains("FIGI1"));
        assert!(log.contains("matching_rows=1"));
        for excluded in ["hidden-report-party", "hidden-direction", "hidden-account"] {
            assert!(
                !log.contains(excluded),
                "unselected evidence leaked: {excluded}"
            );
        }
    }

    #[tokio::test]
    async fn report_generation_polls_pending_then_reads_every_page() {
        let report_row = serde_json::json!({"tradeId":"trade-1", "figi":"FIGI1", "quantity":"92",
            "tradeDatetime":"2026-08-20T10:00:00Z", "clearValueDate":"2026-08-21T00:00:00Z",
            "totalOrderAmount":{"currency":"rub", "units":"173", "nano":930000000}});
        let page0 = serde_json::json!({"getBrokerReportResponse":{"brokerReport":[{"tradeId":"unrelated"}], "itemsCount":2,"pagesCount":2,"page":0}}).to_string();
        let page1 = serde_json::json!({"getBrokerReportResponse":{"brokerReport":[report_row.clone()], "itemsCount":2,"pagesCount":2,"page":1}}).to_string();
        let (gateway, requests, time) = fake::gateway(
            vec![
                Answer::status(
                    200,
                    r#"{"generateBrokerReportResponse":{"taskId":"task-1"}}"#,
                ),
                Answer::status(400, r#"{"code":30058}"#),
                Answer::status(400, r#"{"code":30058}"#),
                Answer::status(200, &page0),
                Answer::status(200, &page1),
            ],
            None,
        );
        let channel = channel(gateway);
        let mut operations = vec![report_operation()];
        assert!(iaam_broker::tinkoff::needs_report_payment(&operations[0]));
        iaam_broker::tinkoff::resolve_report_payment(&operations[0], &[report_row])
            .expect("fixture report resolves");
        channel
            .resolve_report_payments(
                &mut operations,
                "account",
                time::macros::date!(2026 - 08 - 01),
                time::macros::date!(2026 - 08 - 31),
                broker_context(None),
            )
            .await
            .unwrap();
        assert!(
            operations[0].rejection.is_none(),
            "remaining refusal: {:?}",
            operations[0].rejection
        );
        assert_eq!(operations[0].payment.unwrap().amount.raw(), -17393);
        let requests = requests.lock().unwrap();
        let calls: Vec<serde_json::Value> = requests
            .iter()
            .map(|v| serde_json::from_str(v).unwrap())
            .collect();
        assert_eq!(calls.len(), 5);
        assert_eq!(
            calls[0]["generateBrokerReportRequest"]["accountId"],
            "account"
        );
        for call in &calls[1..4] {
            assert_eq!(call["getBrokerReportRequest"]["taskId"], "task-1");
            assert_eq!(call["getBrokerReportRequest"]["page"], 0);
        }
        assert_eq!(calls[4]["getBrokerReportRequest"]["page"], 1);
        assert!(!time.slept().is_empty(), "gateway spaces report polls");
    }

    #[tokio::test]
    async fn incomplete_report_keeps_subminor_operation_refused() {
        let (gateway, _, _) = fake::gateway(
            vec![
                Answer::status(200, r#"{"generateBrokerReportResponse":{"taskId":"task"}}"#),
                Answer::status(
                    200,
                    r#"{"getBrokerReportResponse":{"brokerReport":[],"itemsCount":1,"pagesCount":2,"page":0}}"#,
                ),
            ],
            None,
        );
        let channel = channel(gateway);
        let mut operations = vec![report_operation()];
        channel
            .resolve_report_payments(
                &mut operations,
                "account",
                time::macros::date!(2026 - 08 - 01),
                time::macros::date!(2026 - 08 - 31),
                broker_context(None),
            )
            .await
            .unwrap();
        assert!(iaam_broker::tinkoff::needs_report_payment(&operations[0]));
    }

    #[tokio::test]
    async fn currency_lookup_uses_first_success_and_caches_stale_uid_fallbacks() {
        let description = serde_json::json!({"instrument":{
            "isoCurrencyName":"usd", "currency":"rub", "lot":1,
            "nominal":{"currency":"usd","units":"1","nano":0}
        }})
        .to_string();
        for missing in 0..=2 {
            let mut row = serde_json::json!({
                "cursor":"invented", "brokerAccountId":"invented", "id":"currency-trade",
                "date":"2026-08-20T10:11:12Z", "type":"OPERATION_TYPE_BUY",
                "state":"OPERATION_STATE_EXECUTED", "instrumentKind":"INSTRUMENT_TYPE_CURRENCY",
                "instrumentUid":"invented-currency-uid", "positionUid":"invented-currency-position",
                "figi":"INVENTEDFIGI", "quantity":"2",
                "payment":{"units":"-100","nano":0,"currency":"rub"}
            });
            let mut rows = Vec::new();
            for index in 0..14 {
                row["id"] = format!("currency-trade-{index}").into();
                row["cursor"] = format!("cursor-{index}").into();
                rows.push(row.clone());
            }
            let page = serde_json::json!({"hasNext":false,"items":rows});
            let mut answers = vec![Answer::status(200, &page.to_string())];
            for _ in 0..missing {
                answers.push(Answer::status(404, "{}"));
            }
            answers.push(Answer::status(200, &description));
            let (gateway, requests, _) = fake::gateway_with_instruments(answers, None, true);
            let writer = InstrumentLog::default();
            let captured = writer.clone();
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish();
            let _subscriber = tracing::subscriber::set_default(subscriber);
            let parsed = fetch(&channel(gateway))
                .await
                .expect("currency fallback fetch");
            assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
            assert_eq!(parsed.accepted.len(), 14);
            let requests = requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                missing + 2,
                "first 200 stops; repeated rows use cache"
            );
            let expected = [
                ("invented-currency-uid", "INSTRUMENT_ID_TYPE_UID"),
                (
                    "invented-currency-position",
                    "INSTRUMENT_ID_TYPE_POSITION_UID",
                ),
                ("INVENTEDFIGI", "INSTRUMENT_ID_TYPE_FIGI"),
            ];
            for (request, (id, id_type)) in requests[1..].iter().zip(expected) {
                let call: serde_json::Value = serde_json::from_str(request).unwrap();
                assert_eq!(call["id"], id);
                assert_eq!(call["idType"], id_type);
            }
            let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            if missing > 0 {
                let miss = logs
                    .lines()
                    .find(|line| line.contains("lookup_status=\"404\""))
                    .expect("404 diagnostic");
                assert!(miss.contains("lookup_method=\"CurrencyBy\""), "{miss}");
                assert!(
                    miss.contains("lookup_identifier=\"invented-currency-uid\""),
                    "{miss}"
                );
                assert!(miss.contains("Uid"), "{miss}");
            }
        }
    }

    #[tokio::test]
    async fn currency_lookup_does_not_fall_back_after_non_404_or_unreadable_200() {
        for statuses in [
            vec![400],
            vec![404, 400],
            vec![200],
            vec![404, 200],
            vec![404, 404, 404],
        ] {
            let answers = statuses
                .iter()
                .map(|status| Answer::status(*status, "{}"))
                .collect();
            let (gateway, requests, _) = fake::gateway_with_instruments(answers, None, true);
            let channel = channel(gateway);
            let mut cache = std::collections::HashMap::new();
            let row = serde_json::json!({"positionUid":"invented-position", "figi":"INVENTEDFIGI"});
            for _ in 0..2 {
                let result = channel
                    .resolve_description(
                        ("invented-uid", iaam_broker::tinkoff::InstrumentIdType::Uid),
                        &row,
                        broker_context(None),
                        &mut cache,
                        (super::DescriptionMethod::Currency, |body| {
                            iaam_broker::tinkoff::parse::parse_currency_description(body)
                                .map_err(|error| error.to_string())
                        }),
                    )
                    .await
                    .expect("row-local refusal");
                assert!(result.description.is_err());
            }
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), statuses.len());
            for request in requests.iter() {
                let call: serde_json::Value = serde_json::from_str(request).unwrap();
                assert!(
                    call.get("query").is_none(),
                    "currency must not use FindInstrument"
                );
            }
        }
    }

    #[tokio::test]
    async fn paired_withdrawal_keeps_its_handle_flag_when_isin_is_shared() {
        for position_handle in [false, true] {
            for first_blocked in [false, true] {
                let page = |blocked: bool| {
                    let identity = |blocked| {
                        let handle = if blocked {
                            "BLOCKED_HANDLE"
                        } else {
                            "OPEN_HANDLE"
                        };
                        if position_handle {
                            serde_json::json!({"positionUid":handle})
                        } else {
                            serde_json::json!({"figi":handle})
                        }
                    };
                    let mut withdrawal = serde_json::json!({
                        "id":"withdrawal", "cursor":"out", "brokerAccountId":"invented-account",
                        "date":"2026-08-01T10:00:00Z", "type":"OPERATION_TYPE_OUTPUT_SECURITIES",
                        "state":"OPERATION_STATE_EXECUTED", "quantity":"12",
                        "payment":{"currency":"rub","units":"0","nano":0},
                        "description":"Вывод 12.75 лотов фонда Invented Fund в другой депозитарий"
                    });
                    let mut redemption = serde_json::json!({
                        "id":"redemption", "cursor":"paid", "brokerAccountId":"invented-account",
                        "date":"2026-08-02T10:00:00Z", "type":"OPERATION_TYPE_BOND_REPAYMENT_FULL",
                        "state":"OPERATION_STATE_EXECUTED", "quantity":"0",
                        "payment":{"currency":"rub","units":"71","nano":0}
                    });
                    withdrawal
                        .as_object_mut()
                        .unwrap()
                        .extend(identity(blocked).as_object().unwrap().clone());
                    redemption
                        .as_object_mut()
                        .unwrap()
                        .extend(identity(!blocked).as_object().unwrap().clone());
                    serde_json::json!({"hasNext":false,"items":[withdrawal, redemption]})
                        .to_string()
                };
                let description = |blocked| {
                    serde_json::json!({"instrument":{
                        "isin":"XX0000000042", "ticker":"TEST", "name":"Invented fund",
                        "instrumentType":"etf", "currency":"rub", "blockedTcaFlag":blocked
                    }})
                    .to_string()
                };
                let answers = vec![
                    Answer::status(200, &page(first_blocked)),
                    Answer::status(200, &description(first_blocked)),
                    Answer::status(200, &description(!first_blocked)),
                    Answer::status(200, &page(!first_blocked)),
                ];
                let (gateway, requests, _) = fake::gateway_with_instruments(answers, None, true);
                let mut channel = channel(gateway);
                let (dictionary, unreadable) = OperationKindDictionary::build(
                    iaam_broker::tinkoff::dictionary_seed::TINKOFF_OPERATION_KINDS
                        .iter()
                        .copied(),
                );
                assert!(unreadable.is_empty());
                channel.dictionary = dictionary;
                let mut canonical = None;
                for expected in [first_blocked, !first_blocked] {
                    let parsed = fetch(&channel).await.unwrap();
                    assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
                    assert_eq!(parsed.pending_redemptions.len(), 1);
                    let pending = &parsed.pending_redemptions[0];
                    assert_eq!(
                        pending.withdrawal_operation_id.as_deref(),
                        Some("withdrawal")
                    );
                    assert_eq!(pending.blocked_tca_flag, expected);
                    assert_eq!(
                        *canonical.get_or_insert(pending.instrument),
                        pending.instrument
                    );
                    let records = channel.instruments.list_instruments().await.unwrap();
                    assert!(
                        records
                            .iter()
                            .find(|record| record.id == pending.instrument)
                            .unwrap()
                            .blocked_tca_flag
                    );
                }
                assert_eq!(
                    requests.lock().unwrap().len(),
                    4,
                    "second fetch uses cached descriptions"
                );
            }
        }
    }

    #[tokio::test]
    async fn reread_description_refreshes_blocking_flag_in_both_directions() {
        let answers = [true, false, true].map(|blocked| {
            Answer::status(
                200,
                &serde_json::json!({"instrument": {
                    "isin":"XX0000000042", "ticker":"TEST", "name":"Invented share",
                    "instrumentType":"share", "currency":"rub", "blockedTcaFlag":blocked
                }})
                .to_string(),
            )
        });
        let (gateway, requests, _) = fake::gateway_with_instruments(answers.into(), None, true);
        let channel = channel(gateway);
        let mut identity = None;
        for expected in [true, false, true] {
            channel.descriptions.lock().await.clear();
            let instrument = channel
                .resolve_instrument(
                    Some("invented-uid"),
                    &serde_json::json!({}),
                    time::macros::date!(2026 - 01 - 01),
                    broker_context(None),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*identity.get_or_insert(instrument), instrument);
            let records = channel.instruments.list_instruments().await.unwrap();
            let record = records
                .iter()
                .find(|record| record.id == instrument)
                .unwrap();
            assert_eq!(record.blocked_tca_flag, expected);
        }
        assert_eq!(requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn uid_miss_uses_the_first_typed_fallback_and_caches_it() {
        let body = r#"{"instrument":{"isin":"XX0000000042","ticker":"FALLBACK","name":"Invented fallback","instrumentType":"share","currency":"rub"}}"#;
        for position_found in [true, false] {
            let mut answers = vec![Answer::status(404, "{}")];
            if !position_found {
                answers.push(Answer::status(404, "{}"));
            }
            answers.push(Answer::status(200, body));
            let (gateway, requests, _) = fake::gateway_with_instruments(answers, None, true);
            let channel = channel(gateway);
            let row = serde_json::json!({
                "id":"invented-operation", "instrumentUid":"invented-uid",
                "positionUid":"invented-position", "figi":"TESTFIGI"
            });
            let writer = InstrumentLog::default();
            let captured = writer.clone();
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish();
            let _subscriber = tracing::subscriber::set_default(subscriber);
            for _ in 0..2 {
                let resolved = channel
                    .resolve_instrument(
                        Some("invented-uid"),
                        &row,
                        time::macros::date!(2026 - 01 - 01),
                        broker_context(None),
                    )
                    .await
                    .expect("fallback request")
                    .expect("fallback resolves");
                assert_eq!(
                    resolved,
                    channel
                        .instruments
                        .resolve("isin", "XX0000000042", time::macros::date!(2026 - 01 - 01),)
                        .await
                        .expect("ISIN identity recorded")
                );
            }
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), if position_found { 2 } else { 3 });
            let calls: Vec<serde_json::Value> = requests
                .iter()
                .map(|request| serde_json::from_str(request).unwrap())
                .collect();
            assert_eq!(calls[0]["idType"], "INSTRUMENT_ID_TYPE_UID");
            assert_eq!(calls[1]["idType"], "INSTRUMENT_ID_TYPE_POSITION_UID");
            assert_eq!(calls[1]["id"], "invented-position");
            if !position_found {
                assert_eq!(calls[2]["idType"], "INSTRUMENT_ID_TYPE_FIGI");
                assert_eq!(calls[2]["id"], "TESTFIGI");
            }
            let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            assert!(log.contains("broker instrument fallback answered"), "{log}");
            assert!(
                log.contains(if position_found {
                    "PositionUid"
                } else {
                    "Figi"
                }),
                "{log}"
            );
        }
    }

    #[tokio::test]
    async fn all_typed_lookups_missing_stays_set_aside_and_searches_are_diagnostic() {
        let body = r#"{"instruments":[{"isin":"XX0000000042","ticker":"SEARCH"}]}"#;
        let (gateway, requests, _) = fake::gateway_with_instruments(
            vec![
                Answer::status(404, "{}"),
                Answer::status(404, "{}"),
                Answer::status(404, "{}"),
                Answer::status(200, body),
                Answer::status(200, body),
            ],
            None,
            true,
        );
        let channel = channel(gateway);
        let row = serde_json::json!({"positionUid":"invented-position", "figi":"TESTFIGI"});
        for _ in 0..2 {
            let result = channel
                .resolve_instrument(
                    Some("invented-uid"),
                    &row,
                    time::macros::date!(2026 - 01 - 01),
                    broker_context(None),
                )
                .await
                .expect("404 is local");
            assert!(result.unwrap_err().contains("404"));
        }
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 5);
            let calls: Vec<serde_json::Value> = requests
                .iter()
                .map(|request| serde_json::from_str(request).unwrap())
                .collect();
            assert_eq!(calls[2]["idType"], "INSTRUMENT_ID_TYPE_FIGI");
            assert_eq!(calls[3]["query"], "invented-uid");
            assert_eq!(calls[4]["query"], "TESTFIGI");
        }
        assert!(
            channel
                .instruments
                .resolve("isin", "XX0000000042", time::macros::date!(2026 - 01 - 01))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cached_uid_miss_tries_figi_once_when_a_later_row_supplies_it() {
        let (gateway, requests, _) = fake::gateway_with_instruments(
            vec![
                Answer::status(404, "{}"),
                Answer::status(404, "{}"),
                Answer::status(404, "{}"),
                Answer::status(404, "{}"),
            ],
            None,
            true,
        );
        let channel = channel(gateway);
        for row in [
            serde_json::json!({}),
            serde_json::json!({"figi":"TESTFIGI"}),
            serde_json::json!({"figi":"TESTFIGI"}),
        ] {
            let result = channel
                .resolve_instrument(
                    Some("invented-uid"),
                    &row,
                    time::macros::date!(2026 - 01 - 01),
                    broker_context(None),
                )
                .await
                .unwrap();
            assert!(result.unwrap_err().contains("404"));
        }
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            4,
            "UID lookup and search, then later FIGI lookup and search"
        );
        let uid: serde_json::Value = serde_json::from_str(&requests[0]).unwrap();
        let figi: serde_json::Value = serde_json::from_str(&requests[2]).unwrap();
        assert_eq!(uid["idType"], "INSTRUMENT_ID_TYPE_UID");
        assert_eq!(figi["idType"], "INSTRUMENT_ID_TYPE_FIGI");
    }

    #[tokio::test]
    async fn uid_404_figi_fallback_uses_the_same_request_allowance() {
        let (gateway, requests, _) =
            fake::gateway_with_instruments(vec![Answer::status(404, "{}")], None, true);
        let channel = channel(gateway);
        let allowance = iaam_http::RequestAllowance::new(1);
        let context = crate::ports::BrokerRequestContext {
            deadline: None,
            allowance: &allowance,
        };
        let result = channel
            .resolve_instrument(
                Some("invented-uid"),
                &serde_json::json!({"figi":"TESTFIGI"}),
                time::macros::date!(2026 - 01 - 01),
                context,
            )
            .await;
        assert!(matches!(
            result,
            Err(BrokerError::RequestCeiling { ceiling: 1, .. })
        ));
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "probe cannot exceed sync budget"
        );
        let next = channel
            .resolve_instrument(
                Some("another-invented-uid"),
                &serde_json::json!({}),
                time::macros::date!(2026 - 01 - 01),
                context,
            )
            .await;
        assert!(matches!(
            next,
            Err(BrokerError::RequestCeiling { ceiling: 1, .. })
        ));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn description_without_isin_logs_metadata_without_amounts_and_stays_quarantined() {
        let (gateway, requests, _) = fake::gateway_with_instruments(
            vec![Answer::status(
                200,
                r#"{"instrument":{"ticker":"NOISIN","name":"Invented derivative","instrumentType":"futures","currency":"rub"}}"#,
            )],
            None,
            true,
        );
        let channel = channel(gateway);
        let writer = InstrumentLog::default();
        let captured = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let row = serde_json::json!({"id":"no-isin-operation", "instrumentUid":"no-isin-uid", "quantity":"123456789"});
        let result = channel
            .resolve_instrument(
                Some("no-isin-uid"),
                &row,
                time::macros::date!(2026 - 01 - 01),
                broker_context(None),
            )
            .await
            .unwrap();
        assert!(result.unwrap_err().contains("names no ISIN"));
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "no FIGI probe for no-ISIN descriptions"
        );
        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        for expected in [
            "no_isin",
            "NOISIN",
            "Invented derivative",
            "no-isin-operation",
            "no-isin-uid",
            "futures",
        ] {
            assert!(log.contains(expected), "missing {expected}: {log}");
        }
        assert!(!log.contains("123456789"));
    }

    /// A future that marks itself the first time it is polled and then never
    /// finishes: the probe for the deadline branch that must never even start
    /// the call.
    struct NeverReady(Arc<AtomicBool>);

    impl Future for NeverReady {
        type Output = Result<String, TinkoffError>;

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.0.store(true, Ordering::SeqCst);
            Poll::Pending
        }
    }

    /// The accounts listing is what a sync without a binding binds from, so
    /// the ids must be exactly what the broker named, in his order.
    #[tokio::test]
    async fn the_accounts_listing_becomes_the_ids_the_sync_binds() {
        let (gateway, log, _) = fake::gateway(
            vec![Answer::status(
                200,
                r#"{"accounts":[{"id":"first"},{"id":"second"}]}"#,
            )],
            None,
        );

        let ids = channel(gateway)
            .fetch_account_numbers(broker_context(None))
            .await
            .expect("the listing parses");

        assert_eq!(ids, ["first", "second"]);
        assert_eq!(log.lock().expect("log").len(), 1);
    }

    #[tokio::test]
    async fn bounded_passes_the_answer_through_when_no_deadline_is_set() {
        let body = bounded(None, async { Ok::<_, TinkoffError>("the-body".to_owned()) })
            .await
            .expect("no deadline, no bound");
        assert_eq!(body, "the-body");
    }

    #[tokio::test]
    async fn an_expired_deadline_never_starts_the_call() {
        let polled = Arc::new(AtomicBool::new(false));

        let error = bounded(
            Some(Instant::now() - Duration::from_secs(1)),
            NeverReady(Arc::clone(&polled)),
        )
        .await
        .expect_err("an expired deadline refuses");

        assert!(
            matches!(error, BrokerError::Unreachable { .. }),
            "{error:?}"
        );
        assert!(
            !polled.load(Ordering::SeqCst),
            "the call whose deadline has passed is never started"
        );
    }

    #[tokio::test]
    async fn a_call_outliving_its_deadline_is_dropped_and_refused() {
        let error = bounded(
            Some(Instant::now() + Duration::from_millis(20)),
            NeverReady(Arc::new(AtomicBool::new(false))),
        )
        .await
        .expect_err("the deadline fires");

        assert!(
            matches!(error, BrokerError::Unreachable { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn an_executed_state_has_no_quarantine_reason() {
        let kind = iaam_broker::operation_kind::ChannelOperationKind::Buy;

        assert_eq!(
            order_state_reason(&kind, &ChannelOrderState::Executed),
            None
        );
    }

    fn income_operation(operation_type: &str) -> String {
        income_operation_with_state(operation_type, "OPERATION_STATE_EXECUTED")
    }

    fn income_operation_with_state(operation_type: &str, state: &str) -> String {
        format!(
            r#"{{
                "hasNext": false,
                "items": [{{
                    "cursor": "cursor-1",
                    "brokerAccountId": "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4",
                    "id": "06896b3e-038c-4970-85f2-fd5fc2dfb306",
                    "date": "2026-08-20T10:11:12Z",
                    "type": "{operation_type}",
                    "state": "{state}",
                    "instrumentUid": "01234567-89ab-cdef-0123-456789abcdef",
                    "quantity": "1",
                    "payment": {{"units": "270", "nano": 130000000, "currency": "rub"}}
                }}]
            }}"#
        )
    }

    /// Partial repayment remains refused until its journal mapping exists. The rejection must identify WHAT IS MISSING:
    /// «unsupported kind» would send the owner looking for support
    /// for a kind that is supported, rather than for the data that is missing.
    #[test]
    fn a_partial_bond_repayment_is_refused_by_naming_what_the_channel_does_not_report() {
        use iaam_broker::operation_kind::ChannelOperationKind;
        use iaam_broker::tinkoff::ChannelOperation;

        let kind = ChannelOperationKind::BondAmortisation;
        let operation = ChannelOperation {
            date: None,
            source_time: None,
            broker_account_id: "account".to_owned(),
            operation_id: "1".to_owned(),
            parent_operation_id: None,
            cursor: "c".to_owned(),
            source_kind: "irrelevant: the kind is passed separately".to_owned(),
            state: ChannelOrderState::Executed,
            position_uid: None,
            instrument_uid: None,
            figi: None,
            quantity: None,
            trades: Vec::new(),
            quantity_done: iaam_core::money::Quantity::zero(),
            quantity_rest: iaam_core::money::Quantity::zero(),
            cancel_reason: None,
            accrued_interest: None,
            payment: None,
            price: None,
            commission: None,
            deduplication_key: "k".to_owned(),
            parser_version: iaam_core::event::provenance::ParserVersion("test".to_owned()),
            raw: serde_json::Value::Null,
            rejection: None,
        };
        let account = AccountId(Uuid::from_u128(1));
        let error = operation_to_submitted(account, operation, kind.clone())
            .expect_err("a corporate action cannot be constructed through the channel");
        let RowRefusal::Row { reason, .. } = error else {
            panic!("corporate action should be a row refusal");
        };
        assert!(
            reason.contains("returned face value per unit"),
            "error does not identify what is missing: {reason}"
        );
        assert!(
            reason.contains("journal endpoint"),
            "error does not identify where the fact is entered: {reason}"
        );
    }

    /// The channel dictionary is defined explicitly in tests: classification is data,
    /// and a test relying on a hard-coded list would be testing a list
    /// that no longer exists.
    fn dictionary() -> OperationKindDictionary {
        let (dictionary, unreadable) = OperationKindDictionary::build([
            ("OPERATION_TYPE_BUY", "buy"),
            ("OPERATION_TYPE_SELL", "sell"),
            ("OPERATION_TYPE_COUPON", "coupon"),
            ("OPERATION_TYPE_DIVIDEND", "dividend"),
            ("OPERATION_TYPE_DIV_EXT", "dividend"),
            ("OPERATION_TYPE_BROKER_FEE", "commission"),
            ("OPERATION_TYPE_INPUT", "deposit"),
            ("OPERATION_TYPE_INPUT_SECURITIES", "securities_transfer_in"),
            (
                "OPERATION_TYPE_OUTPUT_SECURITIES",
                "securities_transfer_out",
            ),
            ("OPERATION_TYPE_OUTPUT", "withdrawal"),
            ("OPERATION_TYPE_TRANSFER", "transfer"),
            ("OPERATION_TYPE_BOND_AMORTISATION", "bond_amortisation"),
        ]);
        assert!(unreadable.is_empty(), "{unreadable:?}");
        dictionary
    }

    #[test]
    fn a_missing_dictionary_kind_has_its_own_quarantine_accounting_and_names_the_dictionary() {
        let operations = parse_operations(&income_operation("OPERATION_TYPE_UNKNOWN")).unwrap();
        let (parsed, accounting) = super::adapt_operations_accounted(
            AccountId(Uuid::from_u128(1)),
            operations,
            &dictionary(),
        )
        .unwrap();
        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert_eq!(accounting.quarantined_kind_not_in_dictionary, 1);
        assert_eq!(accounting.quarantined_conversion, 0);
        assert_eq!(
            parsed.quarantined[0].reason,
            "kind_not_in_dictionary: OPERATION_TYPE_UNKNOWN is absent from embedded T-Invest dictionary"
        );
        assert_eq!(parsed.quarantined[0].dimensions, super::all_dimensions());
    }

    #[test]
    fn a_row_refusal_is_quarantined_without_aborting_other_operations() {
        for source_kind in [
            "OPERATION_TYPE_TRANSFER",
            "OPERATION_TYPE_BOND_AMORTISATION",
            "OPERATION_TYPE_UNKNOWN",
        ] {
            let mut operations =
                parse_operations(&income_operation("OPERATION_TYPE_INPUT")).expect("parsing");
            operations.extend(parse_operations(&income_operation(source_kind)).expect("parsing"));
            let parsed = adapt_operations(AccountId(Uuid::from_u128(1)), operations, &dictionary())
                .expect("row refusal must not abort the batch");

            assert_eq!(parsed.accepted.len(), 1);
            assert_eq!(parsed.quarantined.len(), 1);
            assert!(
                parsed.quarantined[0].reason.contains(match source_kind {
                    "OPERATION_TYPE_TRANSFER" => "transfer does not contain a recipient account",
                    "OPERATION_TYPE_BOND_AMORTISATION" => "returned face value per unit",
                    _ => "kind_not_in_dictionary: OPERATION_TYPE_UNKNOWN is absent from embedded T-Invest dictionary",
                }),
                "row refusal reason: {}",
                parsed.quarantined[0].reason
            );
            let expected = match source_kind {
                "OPERATION_TYPE_TRANSFER" | "OPERATION_TYPE_BOND_AMORTISATION" => {
                    [Dimension::Cash].into_iter().collect()
                }
                _ => Dimension::all().into_iter().collect(),
            };
            assert_eq!(parsed.quarantined[0].dimensions, expected);
        }
    }

    #[test]
    fn an_adapter_defect_stays_loud_instead_of_becoming_a_quarantine_row() {
        let operation = parse_operations(&income_operation("OPERATION_TYPE_BUY"))
            .expect("parsing")
            .into_iter()
            .next()
            .expect("one operation");
        let error = operation_to_submitted(
            AccountId(Uuid::from_u128(1)),
            operation.clone(),
            iaam_broker::operation_kind::ChannelOperationKind::Buy,
        )
        .expect_err("the direct conversion must reject the impossible branch");
        let RowRefusal::Adapter(detail) = error else {
            panic!("the impossible branch must be typed as an adapter defect");
        };
        assert!(matches!(
            RowRefusal::Adapter(detail).into_broker_error(),
            BrokerError::Adapter { broker, detail }
                if broker == "tinkoff" && detail.contains("trading operations")
        ));
        let error = trade_operations(
            AccountId(Uuid::from_u128(1)),
            operation,
            iaam_broker::operation_kind::ChannelOperationKind::Deposit,
        )
        .expect_err("the trade conversion must reject the impossible branch");
        assert!(matches!(error, RowRefusal::Adapter(_)));
    }
    fn income_kind_of(operation_type: &str) -> Option<IncomeKind> {
        let operations = parse_operations(&income_operation(operation_type)).expect("parsing");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let operation = operations.into_iter().next().expect("one operation");
        let kind = dictionary().kind_of(&operation.source_kind);
        let submitted =
            operation_to_submitted(account, operation, kind).expect("operation accepted");
        match submitted.kind {
            OperationKind::Income { kind, .. } => kind,
            other => panic!("expected an income receipt, got {other:?}"),
        }
    }

    #[test]
    fn a_coupon_reaches_the_journal_as_a_coupon() {
        assert_eq!(
            income_kind_of("OPERATION_TYPE_COUPON"),
            Some(IncomeKind::Coupon)
        );
    }

    #[test]
    fn a_dividend_does_not_become_a_coupon() {
        // Collapsing two kinds into a single receipt lost the kind forever:
        // the journal event is immutable.
        assert_eq!(
            income_kind_of("OPERATION_TYPE_DIVIDEND"),
            Some(IncomeKind::Dividend)
        );
        assert_eq!(
            income_kind_of("OPERATION_TYPE_DIV_EXT"),
            Some(IncomeKind::Dividend)
        );
    }

    #[test]
    fn an_unknown_operation_kind_is_still_refused() {
        // Silently turning an unknown kind into a cash receipt
        // is worse than rejection: rejection is visible, fabrication is not.
        let operations =
            parse_operations(&income_operation("OPERATION_TYPE_SOMETHING_NEW")).expect("parsing");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let operation = operations.into_iter().next().expect("one operation");
        let kind = dictionary().kind_of(&operation.source_kind);
        assert!(operation_to_submitted(account, operation, kind).is_err());
    }

    #[test]
    fn preserves_rejected_fixture_rows_in_quarantine() -> Result<(), Box<dyn std::error::Error>> {
        let operations = parse_operations(include_str!(
            "../../../../tests/fixtures/api/tinkoff-operations.json"
        ))?;
        let account = AccountId(Uuid::parse_str("d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4")?);
        let parsed = adapt_operations(account, operations, &dictionary())?;

        assert_eq!(parsed.accepted.len(), 3);
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(parsed.accepted.iter().any(|operation| {
            operation.source_operation_id.as_deref()
                == Some("06896b3e-038c-4970-85f2-fd5fc2dfb306#06896b3e-038c-4970-85f2-fd5fc2dfb306")
        }));
        assert!(!parsed.accepted.iter().any(|operation| {
            operation.source_operation_id.as_deref() == Some("7aa1cf04-71c7-4b62-81c7-7f27ec4cfb8d")
        }));
        let rejected = parsed
            .quarantined
            .iter()
            .find(|row| row.raw["id"] == "7aa1cf04-71c7-4b62-81c7-7f27ec4cfb8d")
            .ok_or("rejected commission disappeared from quarantine")?;
        assert!(rejected.reason.contains("NonRepresentableFraction"));
        assert_eq!(rejected.raw["payment"]["nano"], -135065000);
        assert_eq!(rejected.dimensions, [Dimension::Cash].into_iter().collect());
        Ok(())
    }

    #[test]
    fn a_whole_row_parse_refusal_taints_every_dimension() {
        let operations = parse_operations(r#"{"hasNext":false,"items":[null]}"#)
            .expect("whole-row parse refusal");
        assert!(operations[0].source_kind.is_empty());
        let parsed = adapt_operations(AccountId(Uuid::from_u128(1)), operations, &dictionary())
            .expect("whole-row refusal is quarantined");

        assert_eq!(parsed.quarantined.len(), 1);
        assert_eq!(
            parsed.quarantined[0].dimensions,
            Dimension::all().into_iter().collect()
        );
    }
    #[test]
    fn a_trade_carries_the_recorded_position_uid_as_provenance_and_no_custody() {
        let operations = parse_operations(include_str!(
            "../../../../tests/fixtures/api/tinkoff-operations.json"
        ))
        .expect("parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");
        let expected = "f1a60ae6-3f1e-43c8-8d46-042df0fdc97a";

        let buy = parsed
            .accepted
            .iter()
            .find(|operation| matches!(operation.kind, OperationKind::Buy { .. }))
            .expect("a recorded buy");
        assert_eq!(buy.source_position_id.as_deref(), Some(expected));
        assert!(matches!(buy.kind, OperationKind::Buy { custody: None, .. }));
    }

    #[test]
    fn a_trade_without_a_position_uid_is_quarantined_without_account_fallback() {
        for position_field in [String::new(), r#""positionUid": "", "#.to_owned()] {
            let body = format!(
                r#"{{
                    "hasNext": false,
                    "items": [{{
                        "cursor": "cursor-1",
                        "brokerAccountId": "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4",
                        "id": "missing-position",
                        "date": "2026-08-20T10:11:12Z",
                        "type": "OPERATION_TYPE_BUY",
                        "state": "OPERATION_STATE_EXECUTED",
                        {position_field}
                        "instrumentUid": "01234567-89ab-cdef-0123-456789abcdef",
                        "quantityDone": "1",
                        "payment": {{"units":"-12","nano":0,"currency":"rub"}},
                        "tradesInfo": {{"trades": [{trade}]}}
                    }}]
                }}"#,
                trade = trade("trade-1", "2026-08-20T10:11:12Z", "1", "12", 0),
            );
            let parsed = adapt_operations(
                trading_account(),
                parse_operations(&body).expect("parsing"),
                &dictionary(),
            )
            .expect("adaptation");

            assert!(parsed.accepted.is_empty());
            assert_eq!(parsed.quarantined.len(), 1);
            assert!(parsed.quarantined[0].reason.contains("positionUid"));
            assert!(!parsed.quarantined[0].reason.contains("account"));
            assert_eq!(
                parsed.quarantined[0].dimensions,
                [Dimension::Cash, Dimension::Positions]
                    .into_iter()
                    .collect()
            );
        }
    }

    #[test]
    fn an_executed_transfer_is_quarantined_with_its_existing_reason() {
        let operations = parse_operations(&income_operation_with_state(
            "OPERATION_TYPE_TRANSFER",
            "OPERATION_STATE_EXECUTED",
        ))
        .expect("parsing");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));

        let parsed =
            adapt_operations(account, operations, &dictionary()).expect("transfer is quarantined");
        assert_eq!(parsed.accepted.len(), 0);
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(
            parsed.quarantined[0]
                .reason
                .contains("transfer does not contain a recipient account")
        );
        assert_eq!(
            parsed.quarantined[0].dimensions,
            [Dimension::Cash].into_iter().collect()
        );
    }

    #[test]
    fn a_securities_transfer_in_is_quarantined_as_an_inbound_securities_transfer() {
        let operations = parse_operations(&income_operation("OPERATION_TYPE_INPUT_SECURITIES"))
            .expect("parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(parsed.quarantined[0].reason.contains("securities"));
        assert!(parsed.quarantined[0].reason.contains("inbound"));
        assert_eq!(
            parsed.quarantined[0].dimensions,
            [Dimension::Positions, Dimension::TaxBasis]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn an_outbound_securities_transfer_records_quantity_without_cash_or_destination() {
        let mut body: serde_json::Value =
            serde_json::from_str(&income_operation("OPERATION_TYPE_OUTPUT_SECURITIES")).unwrap();
        body["items"][0].as_object_mut().unwrap().remove("payment");
        body["items"][0]["description"] = serde_json::json!("Transfer to another depository");
        body["items"][0]["childOperations"] = serde_json::json!([]);
        let operations = parse_operations(&body.to_string()).expect("parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");
        assert!(parsed.quarantined.is_empty());
        assert_eq!(parsed.accepted.len(), 1);
        assert!(
            matches!(parsed.accepted[0].kind, OperationKind::SecuritiesTransferOut { quantity, .. } if quantity == iaam_core::numeric::decimal::Dec::one())
        );
        assert_eq!(
            parsed.accepted[0].dates.trade,
            Some(time::macros::date!(2026 - 08 - 20))
        );
        assert!(parsed.accepted[0].source_operation_id.is_some());
    }

    #[test]
    fn a_quarantined_securities_transfer_does_not_abort_an_accepted_row() {
        let accepted = parse_operations(&income_operation("OPERATION_TYPE_INPUT"))
            .expect("parsing")
            .pop()
            .expect("one accepted operation");
        let rejected = parse_operations(&income_operation("OPERATION_TYPE_INPUT_SECURITIES"))
            .expect("parsing")
            .pop()
            .expect("one securities transfer");
        let parsed = adapt_operations(trading_account(), vec![accepted, rejected], &dictionary())
            .expect("adaptation");

        assert_eq!(parsed.accepted.len(), 1);
        assert!(matches!(
            parsed.accepted[0].kind,
            OperationKind::Deposit { .. }
        ));
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(parsed.quarantined[0].reason.contains("securities"));
        assert_eq!(
            parsed.quarantined[0].dimensions,
            [Dimension::Positions, Dimension::TaxBasis]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn a_cancelled_buy_without_trades_produces_no_fact_or_quarantine() {
        let operations = parse_operations(&income_operation_with_state(
            "OPERATION_TYPE_BUY",
            "OPERATION_STATE_CANCELED",
        ))
        .expect("parsing");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let parsed = adapt_operations(account, operations, &dictionary()).expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert!(parsed.quarantined.is_empty());
    }

    #[test]
    fn cancelled_currency_and_security_buys_share_no_execution_accounting() {
        use super::adapt_operations_with_currencies;
        use std::collections::HashMap;
        for is_currency in [false, true] {
            let body = serde_json::json!({"hasNext": false, "items": [{
                "cursor": "invented-cancelled-cursor",
                "brokerAccountId": "invented-account",
                "id": "invented-cancelled-buy",
                "date": "2026-08-20T10:11:12Z",
                "type": "OPERATION_TYPE_BUY",
                "state": "OPERATION_STATE_CANCELED",
                "instrumentType": if is_currency { "currency" } else { "share" },
                "instrumentUid": "aaaaaaaa-0000-0000-0000-000000000001",
                "quantity": "2",
                "quantityDone": "0",
                "payment": {"units": "100", "nano": 0, "currency": "rub"},
                "tradesInfo": {"trades": []}
            }]});
            let operations = parse_operations(&body.to_string()).expect("parse invented buy");
            let mut currency_rows = HashMap::new();
            if is_currency {
                let description = serde_json::json!({"instrument": {
                    "isoCurrencyName": "usd", "currency": "rub", "lot": 1,
                    "nominal": {"currency": "usd", "units": "1", "nano": 0}
                }});
                currency_rows.insert(
                    operations[0].deduplication_key.clone(),
                    iaam_broker::tinkoff::parse::parse_currency_description(
                        &description.to_string(),
                    )
                    .expect("parse invented currency description"),
                );
            }
            let (parsed, accounting) = adapt_operations_with_currencies(
                trading_account(),
                operations,
                &dictionary(),
                &currency_rows,
            )
            .expect("adapt cancelled buy");

            assert!(parsed.accepted.is_empty());
            assert!(parsed.journal.is_empty());
            assert!(parsed.pending_redemptions.is_empty());
            assert!(parsed.quarantined.is_empty());
            assert_eq!(accounting.reduced_no_execution_trades, 1);
            assert_eq!(accounting.source_rows_accepted, 0);
            assert_eq!(accounting.quarantined_conversion, 0);
        }
    }

    #[test]
    fn a_cancelled_coupon_is_quarantined_without_a_fact() {
        let operations = parse_operations(&income_operation_with_state(
            "OPERATION_TYPE_COUPON",
            "OPERATION_STATE_CANCELED",
        ))
        .expect("parsing");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let parsed = adapt_operations(account, operations, &dictionary()).expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(
            parsed.quarantined[0]
                .reason
                .contains("OPERATION_STATE_CANCELED")
        );
        assert_eq!(
            parsed.quarantined[0].dimensions,
            [Dimension::Cash, Dimension::Income].into_iter().collect()
        );
    }
    #[test]
    fn a_cancelled_dividend_taints_cash_and_income() {
        let operations = parse_operations(&income_operation_with_state(
            "OPERATION_TYPE_DIVIDEND",
            "OPERATION_STATE_CANCELED",
        ))
        .expect("parsing");
        let parsed = adapt_operations(
            AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888)),
            operations,
            &dictionary(),
        )
        .expect("adaptation");

        assert_eq!(parsed.quarantined.len(), 1);
        assert_eq!(
            parsed.quarantined[0].dimensions,
            [Dimension::Cash, Dimension::Income].into_iter().collect()
        );
    }

    #[test]
    fn a_cancelled_unknown_kind_taints_all_dimensions() {
        let operations = parse_operations(&income_operation_with_state(
            "OPERATION_TYPE_UNKNOWN",
            "OPERATION_STATE_CANCELED",
        ))
        .expect("parsing");
        let parsed = adapt_operations(
            AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888)),
            operations,
            &dictionary(),
        )
        .expect("adaptation");

        assert_eq!(parsed.quarantined.len(), 1);
        assert_eq!(
            parsed.quarantined[0].dimensions,
            Dimension::all().into_iter().collect()
        );
    }

    #[test]
    fn a_deposit_placeholder_trade_is_ignored_and_becomes_a_fact() {
        let body = r#"{
            "hasNext": false,
            "items": [{
                "cursor": "cursor-1",
                "brokerAccountId": "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4",
                "id": "deposit-1",
                "date": "2026-08-20T10:11:12Z",
                "type": "OPERATION_TYPE_INPUT",
                "state": "OPERATION_STATE_EXECUTED",
                "quantityDone": "0",
                "tradesInfo": {"trades": [{
                    "price": {"units": "0", "nano": 0, "currency": ""}
                }]},
                "payment": {"units": "100", "nano": 0, "currency": "rub"}
            }]
        }"#;
        let operation = parse_operations(body)
            .expect("parsing")
            .pop()
            .expect("one operation");
        assert!(operation.rejection.is_none());
        assert!(operation.trades.is_empty());

        let parsed = adapt_operations(trading_account(), vec![operation], &dictionary())
            .expect("adaptation");

        assert!(parsed.quarantined.is_empty());
        assert_eq!(parsed.accepted.len(), 1);
        assert!(matches!(
            &parsed.accepted[0].kind,
            OperationKind::Deposit { .. }
        ));
    }

    #[test]
    fn an_unrecognised_coupon_state_is_quarantined_with_the_value_quoted() {
        let operations = parse_operations(&income_operation_with_state(
            "OPERATION_TYPE_COUPON",
            "OPERATION_STATE_SOMETHING_NEW",
        ))
        .expect("parsing");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let parsed = adapt_operations(account, operations, &dictionary()).expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(
            parsed.quarantined[0]
                .reason
                .contains("\"OPERATION_STATE_SOMETHING_NEW\"")
        );
    }

    #[test]
    fn a_quarantined_row_does_not_abort_an_accepted_row_in_the_same_response() {
        let accepted_body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "executed-buy",
            quantity_done: "1",
            payment: r#"{"units":"-270","nano":-130000000,"currency":"rub"}"#,
            fields: "",
            trades: &trade("trade-1", "2026-08-20T10:11:12Z", "1", "270", 130_000_000),
        });
        let rejected_body = trading_operation(
            "OPERATION_TYPE_BUY",
            "OPERATION_STATE_CANCELED",
            "cancelled-buy",
            "2",
            &trade("trade-2", "2026-08-20T10:11:12Z", "1", "270", 130_000_000),
        );
        let accepted = parse_operations(&accepted_body)
            .expect("parsing")
            .pop()
            .expect("one operation");
        let rejected = parse_operations(&rejected_body)
            .expect("parsing")
            .pop()
            .expect("one operation");
        let parsed = adapt_operations(trading_account(), vec![accepted, rejected], &dictionary())
            .expect("adaptation");

        assert_eq!(parsed.accepted.len(), 1);
        assert_eq!(parsed.quarantined.len(), 1);
    }
    fn trading_operation(
        operation_type: &str,
        state: &str,
        operation_id: &str,
        quantity_done: &str,
        trades: &str,
    ) -> String {
        let amount = quantity_done.parse::<i64>().expect("integer quantity") * 12;
        let amount = if operation_type == "OPERATION_TYPE_SELL" {
            amount
        } else {
            -amount
        };
        let payment = format!(r#"{{"units":"{amount}","nano":0,"currency":"rub"}}"#);
        trading_operation_with_fields(TradingOperationFields {
            operation_type,
            state,
            operation_id,
            quantity_done,
            payment: &payment,
            fields: "",
            trades,
        })
    }

    fn trade(num: &str, date: &str, quantity: &str, units: &str, nano: i64) -> String {
        format!(
            r#"{{"num":"{num}","date":"{date}","quantity":"{quantity}","price":{{"units":"{units}","nano":{nano},"currency":"rub"}}}}"#
        )
    }

    fn trading_account() -> AccountId {
        AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888))
    }

    #[test]
    fn one_order_becomes_one_fact_per_trade_with_trade_dates_and_quantities() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "operation-1",
            quantity_done: "12",
            payment: r#"{"units":"-150","nano":-360000000,"currency":"rub"}"#,
            fields: "",
            trades: &format!(
                "{},{}",
                trade("trade-1", "2026-08-21T11:12:13Z", "10", "12", 345_600_000),
                trade("trade-2", "2026-08-22T12:13:14Z", "2", "13", 450_000_000)
            ),
        });
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(&body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");

        assert_eq!(parsed.quarantined.len(), 0);
        assert_eq!(parsed.accepted.len(), 2);
        assert_eq!(
            parsed.accepted[0].dates.trade,
            Some(time::macros::date!(2026 - 08 - 21))
        );
        assert_eq!(
            parsed.accepted[1].dates.trade,
            Some(time::macros::date!(2026 - 08 - 22))
        );
        assert_eq!(
            parsed.accepted[0].source_time,
            Some(time::macros::time!(11:12:13))
        );
        assert_eq!(
            parsed.accepted[1].source_time,
            Some(time::macros::time!(12:13:14))
        );
        assert_eq!(
            parsed.accepted[0].source_operation_id.as_deref(),
            Some("operation-1#trade-1")
        );
        assert_eq!(
            parsed.accepted[1].source_operation_id.as_deref(),
            Some("operation-1#trade-2")
        );
        assert!(matches!(
            parsed.accepted[0].kind,
            OperationKind::Buy { quantity, gross_minor: 12_346, .. }
                if quantity.inner().to_string() == "10"
        ));
        assert!(matches!(
            parsed.accepted[1].kind,
            OperationKind::Buy { quantity, gross_minor: 2_690, .. }
                if quantity.inner().to_string() == "2"
        ));
    }

    #[test]
    fn an_order_without_trades_produces_no_fact_or_quarantine() {
        let body = trading_operation(
            "OPERATION_TYPE_BUY",
            "OPERATION_STATE_CANCELED",
            "operation-1",
            "0",
            "",
        );
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(&body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert!(parsed.quarantined.is_empty());
    }

    #[test]
    fn a_cancelled_order_with_trades_produces_facts() {
        let body = trading_operation(
            "OPERATION_TYPE_SELL",
            "OPERATION_STATE_CANCELED",
            "operation-1",
            "2",
            &trade("trade-1", "2026-08-21T11:12:13Z", "2", "12", 0),
        );
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(&body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");

        assert_eq!(parsed.accepted.len(), 1);
        assert!(parsed.quarantined.is_empty());
    }

    #[test]
    fn mismatched_trade_quantity_quarantines_with_both_totals() {
        let body = trading_operation(
            "OPERATION_TYPE_BUY",
            "OPERATION_STATE_EXECUTED",
            "operation-1",
            "3",
            &trade("trade-1", "2026-08-21T11:12:13Z", "2", "12", 0),
        );
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(&body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(parsed.quarantined[0].reason.contains("2"));
        assert!(parsed.quarantined[0].reason.contains("3"));
    }

    #[test]
    fn duplicate_trade_numbers_quarantine_with_the_value_quoted() {
        let body = trading_operation(
            "OPERATION_TYPE_BUY",
            "OPERATION_STATE_EXECUTED",
            "operation-1",
            "2",
            &format!(
                "{},{}",
                trade("same-num", "2026-08-21T11:12:13Z", "1", "12", 0),
                trade("same-num", "2026-08-21T11:12:14Z", "1", "12", 0)
            ),
        );
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(&body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(parsed.quarantined[0].reason.contains("\"same-num\""));
    }

    #[test]
    fn composite_identity_escapes_each_component_before_joining() {
        let body = trading_operation(
            "OPERATION_TYPE_BUY",
            "OPERATION_STATE_EXECUTED",
            "operation#/%23",
            "1",
            &trade("trade/#%", "2026-08-21T11:12:13Z", "1", "12", 0),
        );
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(&body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");

        assert_eq!(parsed.accepted.len(), 1);
        assert_eq!(
            parsed.accepted[0].source_operation_id.as_deref(),
            Some("operation%23%2F%2523#trade%2F%23%25")
        );
        assert_eq!(
            parsed.accepted[0].idempotency_key.as_deref(),
            Some("d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4/operation%23%2F%2523#trade%2F%23%25")
        );
    }

    #[test]
    fn reversed_response_order_emits_the_same_sorted_sequence() {
        let first = format!(
            "{},{}",
            trade("trade-b", "2026-08-21T11:12:13Z", "1", "12", 0),
            trade("trade-a", "2026-08-21T11:12:13Z", "1", "12", 0)
        );
        let second = format!(
            "{},{}",
            trade("trade-a", "2026-08-21T11:12:13Z", "1", "12", 0),
            trade("trade-b", "2026-08-21T11:12:13Z", "1", "12", 0)
        );
        let adapt = |trades: &str| {
            adapt_operations(
                trading_account(),
                parse_operations(&trading_operation(
                    "OPERATION_TYPE_BUY",
                    "OPERATION_STATE_EXECUTED",
                    "operation-1",
                    "2",
                    trades,
                ))
                .expect("parsing"),
                &dictionary(),
            )
            .expect("adaptation")
            .accepted
            .into_iter()
            .map(|operation| operation.source_operation_id)
            .collect::<Vec<_>>()
        };

        assert_eq!(adapt(&first), adapt(&second));
        assert_eq!(
            adapt(&first),
            vec![
                Some("operation-1#trade-a".to_owned()),
                Some("operation-1#trade-b".to_owned())
            ]
        );
    }

    #[test]
    fn sorted_buy_fills_are_the_lots_fifo_later_sales_consume() {
        let check = |trades: &str| {
            let buy = adapt_operations(
                trading_account(),
                parse_operations(&trading_operation(
                    "OPERATION_TYPE_BUY",
                    "OPERATION_STATE_EXECUTED",
                    "buy-order",
                    "2",
                    trades,
                ))
                .expect("parsing"),
                &dictionary(),
            )
            .expect("buy adaptation")
            .accepted;
            let sale = adapt_operations(
                trading_account(),
                parse_operations(&trading_operation(
                    "OPERATION_TYPE_SELL",
                    "OPERATION_STATE_EXECUTED",
                    "sell-order",
                    "1",
                    &trade("sale", "2026-08-22T11:00:00Z", "1", "12", 0),
                ))
                .expect("parsing"),
                &dictionary(),
            )
            .expect("sale adaptation")
            .accepted;
            let context = NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
                parser_version: ParserVersion(PARSER_VERSION.to_owned()),
            };
            let events = buy
                .iter()
                .chain(sale.iter())
                .enumerate()
                .map(|(index, operation)| {
                    let mut event = normalize(operation, &context).expect("normalization").event;
                    event.id = EventId(Uuid::from_u128((index + 1) as u128));
                    event
                })
                .collect::<Vec<_>>();
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [trading_account()],
            );
            let rules = RuleRegistry::with_defaults();
            let projection = project(
                &events,
                &ProjectionContext {
                    contour: &contour,
                    rules: &rules,
                    lot_rule: LotRuleVersion(1),
                },
            )
            .expect("projection");
            let key = iaam_core::projection::lots::LotKey {
                account: trading_account(),
                instrument: InstrumentId(
                    Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef")
                        .expect("instrument UUID"),
                ),
            };
            let remaining = projection
                .state()
                .book()
                .entry(&key)
                .expect("remaining lot entry")
                .lots()
                .first()
                .expect("one remaining lot")
                .id
                .0;
            (events[1].id.inner(), remaining)
        };
        let forward = format!(
            "{},{}",
            trade("trade-a", "2026-08-21T10:00:00Z", "1", "12", 0),
            trade("trade-b", "2026-08-21T11:00:00Z", "1", "12", 0)
        );
        let reversed = format!(
            "{},{}",
            trade("trade-b", "2026-08-21T11:00:00Z", "1", "12", 0),
            trade("trade-a", "2026-08-21T10:00:00Z", "1", "12", 0)
        );

        let (forward_second, forward_consumed) = check(&forward);
        let (reversed_second, reversed_consumed) = check(&reversed);
        assert_eq!(forward_second, forward_consumed);
        assert_eq!(reversed_second, reversed_consumed);
        assert_eq!(
            (forward_second, forward_consumed),
            (reversed_second, reversed_consumed)
        );
    }
    #[test]
    fn a_fine_trade_price_is_multiplied_before_rounding_and_sales_are_positive() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_SELL",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "operation-1",
            quantity_done: "10",
            payment: r#"{"units":"123","nano":460000000,"currency":"rub"}"#,
            fields: "",
            trades: &trade("trade-1", "2026-08-21T11:12:13Z", "10", "12", 345_600_000),
        });
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(&body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");

        assert!(matches!(
            parsed.accepted[0].kind,
            OperationKind::Sell {
                gross_minor: 12_346,
                quantity,
                basis_fee: None,
                ..
            } if quantity.inner().to_string() == "10"
        ));
    }

    struct TradingOperationFields<'a> {
        operation_type: &'a str,
        state: &'a str,
        operation_id: &'a str,
        quantity_done: &'a str,
        payment: &'a str,
        fields: &'a str,
        trades: &'a str,
    }

    fn trading_operation_with_fields(fields: TradingOperationFields<'_>) -> String {
        let TradingOperationFields {
            operation_type,
            state,
            operation_id,
            quantity_done,
            payment,
            fields: extra_fields,
            trades,
        } = fields;
        format!(
            r#"{{
                "hasNext": false,
                "items": [{{
                    "cursor": "cursor-1",
                    "brokerAccountId": "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4",
                    "id": "{operation_id}",
                    "date": "2026-08-20T10:11:12Z",
                    "type": "{operation_type}",
                    "state": "{state}",
                    "positionUid": "f1a60ae6-3f1e-43c8-8d46-042df0fdc97a",
                    "instrumentUid": "01234567-89ab-cdef-0123-456789abcdef",
                    "quantityDone": "{quantity_done}",
                    "quantityRest": "0",
                    "payment": {payment}{extra_fields},
                    "tradesInfo": {{"trades": [{trades}]}}
                }}]
            }}"#
        )
    }

    fn accepted_trade_operations(
        body: &str,
    ) -> (Vec<iaam_ingest::SubmittedOperation>, Vec<Quarantined>) {
        let parsed = adapt_operations(
            trading_account(),
            parse_operations(body).expect("parsing"),
            &dictionary(),
        )
        .expect("adaptation");
        (parsed.accepted, parsed.quarantined)
    }

    #[test]
    fn recorded_sber_purchase_reconciles_and_has_unknown_accrued_interest() {
        let operations = parse_operations(include_str!(
            "../../../../tests/fixtures/api/tinkoff-operations.json"
        ))
        .expect("parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");

        assert_eq!(parsed.quarantined.len(), 1);
        for operation in parsed.accepted {
            if matches!(
                operation.kind,
                OperationKind::Buy { .. } | OperationKind::Sell { .. }
            ) {
                assert!(matches!(
                    operation.kind,
                    OperationKind::Buy {
                        fee_minor: None,
                        accrued_interest_minor: None,
                        ..
                    } | OperationKind::Sell {
                        fee_minor: None,
                        accrued_interest_minor: None,
                        ..
                    }
                ));
            }
        }
    }
    #[test]
    fn t_invest_portfolio_answers_with_current_date_semantics() {
        let snapshot = adapt_portfolio(
            include_str!("../../../../tests/fixtures/api/tinkoff-portfolio.json"),
            None,
        )
        .expect("portfolio adaptation");

        assert_eq!(snapshot.as_of, PortfolioAsOf::Current);
        assert!(snapshot.refused.is_empty());
    }

    /// Two rows naming one instrument are refused as a pair — naming both by
    /// their own `positionUid` — while the instrument only one row names
    /// still becomes a claim.
    #[test]
    fn two_portfolio_rows_for_one_instrument_are_refused_as_a_pair() {
        let body = r#"{
            "positions": [
                {
                    "instrumentType": "share",
                    "quantity": {"units": "10", "nano": 0},
                    "positionUid": "aaaaaaaa-0000-0000-0000-000000000001",
                    "instrumentUid": "cccccccc-0000-0000-0000-000000000099",
                    "blocked": false
                },
                {
                    "instrumentType": "share",
                    "quantity": {"units": "5", "nano": 0},
                    "positionUid": "aaaaaaaa-0000-0000-0000-000000000002",
                    "instrumentUid": "cccccccc-0000-0000-0000-000000000099",
                    "blocked": true,
                    "blockedLots": {"units": "5", "nano": 0}
                },
                {
                    "instrumentType": "share",
                    "quantity": {"units": "3", "nano": 0},
                    "positionUid": "bbbbbbbb-0000-0000-0000-000000000001",
                    "instrumentUid": "dddddddd-0000-0000-0000-000000000042",
                    "blocked": false
                }
            ]
        }"#;

        let snapshot = adapt_portfolio(body, None).expect("portfolio adaptation");

        let disputed = InstrumentId(
            Uuid::parse_str("cccccccc-0000-0000-0000-000000000099").expect("instrument UID"),
        );
        let undisputed = InstrumentId(
            Uuid::parse_str("dddddddd-0000-0000-0000-000000000042").expect("instrument UID"),
        );
        assert!(
            !snapshot.claims.iter().any(|claim| matches!(
                claim,
                ControlClaim::PositionQuantity { instrument, .. } if *instrument == disputed
            )),
            "the disputed instrument must not become a claim"
        );
        assert!(
            snapshot.claims.iter().any(|claim| matches!(
                claim,
                ControlClaim::PositionQuantity { instrument, .. } if *instrument == undisputed
            )),
            "the rest of the snapshot must still import"
        );

        assert_eq!(snapshot.refused.len(), 1);
        let refusal = &snapshot.refused[0];
        assert!(
            refusal
                .reason
                .contains("aaaaaaaa-0000-0000-0000-000000000001")
        );
        assert!(
            refusal
                .reason
                .contains("aaaaaaaa-0000-0000-0000-000000000002")
        );
        assert!(
            refusal
                .reason
                .contains("cccccccc-0000-0000-0000-000000000099")
        );
        assert_eq!(
            refusal.dimensions,
            [Dimension::Positions].into_iter().collect()
        );
    }

    /// A row whose identifier is not a UUID is set aside beside the claims,
    /// with its reason and its own JSON — the sync must not stop on it, and
    /// the readable rows must still import (iaam-vg8te.1.2).
    #[test]
    fn a_position_with_an_unreadable_identifier_is_set_aside_beside_the_claims() {
        let body = r#"{
            "positions": [
                {
                    "instrumentType": "share",
                    "quantity": {"units": "3", "nano": 0},
                    "positionUid": "bbbbbbbb-0000-0000-0000-000000000001",
                    "instrumentUid": "dddddddd-0000-0000-0000-000000000042",
                    "blocked": false
                },
                {
                    "instrumentType": "share",
                    "quantity": {"units": "1", "nano": 0},
                    "positionUid": "not-a-uuid",
                    "instrumentUid": "also-not-a-uuid"
                }
            ]
        }"#;

        let snapshot = adapt_portfolio(body, None).expect("portfolio adaptation");

        assert_eq!(snapshot.claims.len(), 1, "{:?}", snapshot.claims);
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        let refusal = &snapshot.refused[0];
        assert!(refusal.reason.contains("not a UUID"), "{}", refusal.reason);
        assert!(
            refusal.reason.contains("also-not-a-uuid"),
            "{}",
            refusal.reason
        );
        assert_eq!(
            refusal.dimensions,
            [Dimension::Positions].into_iter().collect()
        );
    }

    /// A refused RUB currency row is cash, never a position: the row names
    /// `instrumentType: currency`, so the refusal taints Cash — the readable
    /// RUB row beside it still becomes a cash claim (iaam-vg8te.1.2).
    #[test]
    fn a_refused_rub_currency_row_taints_cash_not_positions() {
        let body = r#"{
            "positions": [
                {
                    "instrumentType": "currency",
                    "quantity": {"units": "0", "nano": 0},
                    "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c1",
                    "instrumentUid": "bbbbbbbb-0000-0000-0000-0000000000c2",
                    "currentPrice": {"units": "1", "nano": 0, "currency": "rub"},
                    "averagePositionPrice": {"units": "1", "nano": 0, "currency": "rub"}
                },
                {
                    "instrumentType": "currency",
                    "quantity": {"nano": 500000000},
                    "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c3",
                    "instrumentUid": "bbbbbbbb-0000-0000-0000-0000000000c4",
                    "currentPrice": {"units": "1", "nano": 0, "currency": "rub"}
                }
            ]
        }"#;

        let snapshot = adapt_portfolio(body, None).expect("portfolio adaptation");

        assert_eq!(
            snapshot
                .claims
                .iter()
                .filter(|claim| { matches!(claim, ControlClaim::CashBalance { .. }) })
                .count(),
            1,
            "{:?}",
            snapshot.claims
        );
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        let refusal = &snapshot.refused[0];
        assert!(
            refusal.reason.contains("units"),
            "the refusal names the quantity field the row withheld: {}",
            refusal.reason
        );
        assert_eq!(refusal.dimensions, [Dimension::Cash].into_iter().collect());
    }

    /// A refused security row is a position: the row names a real instrument
    /// type, so the refusal taints Positions while the readable security
    /// beside it still becomes a position claim (iaam-vg8te.1.2).
    #[test]
    fn a_refused_security_row_taints_positions() {
        let body = r#"{
            "positions": [
                {
                    "instrumentType": "share",
                    "quantity": {"units": "0", "nano": 0},
                    "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c5",
                    "instrumentUid": "dddddddd-0000-0000-0000-000000000042",
                    "blocked": false
                },
                {
                    "instrumentType": "share",
                    "quantity": {"nano": 500000000},
                    "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c6",
                    "instrumentUid": "dddddddd-0000-0000-0000-000000000098",
                    "blocked": false
                }
            ]
        }"#;

        let snapshot = adapt_portfolio(body, None).expect("portfolio adaptation");

        assert_eq!(
            snapshot
                .claims
                .iter()
                .filter(|claim| { matches!(claim, ControlClaim::PositionQuantity { .. }) })
                .count(),
            1,
            "{:?}",
            snapshot.claims
        );
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        let refusal = &snapshot.refused[0];
        assert!(refusal.reason.contains("quantity"), "{}", refusal.reason);
        assert_eq!(
            refusal.dimensions,
            [Dimension::Positions].into_iter().collect()
        );
    }

    /// A row broken enough that the parser never typed it cannot say whether
    /// it is cash or a holding: the refusal taints the conservative pair, so
    /// neither dimension is declared confirmed while the row is invisible
    /// (iaam-vg8te.1.2).
    #[test]
    fn a_row_too_broken_to_name_its_type_taints_the_conservative_pair() {
        let body = r#"{
            "positions": [
                {
                    "quantity": "not-an-object",
                    "positionUid": "aaaaaaaa-0000-0000-0000-0000000000c7",
                    "instrumentUid": "bbbbbbbb-0000-0000-0000-0000000000c8"
                }
            ]
        }"#;

        let snapshot = adapt_portfolio(body, None).expect("portfolio adaptation");

        assert!(snapshot.claims.is_empty(), "{:?}", snapshot.claims);
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        assert_eq!(
            snapshot.refused[0].dimensions,
            [Dimension::Cash, Dimension::Positions]
                .into_iter()
                .collect()
        );
    }

    /// An opening position assertion of nothing held, for the instrument and
    /// depository the portfolio claim names.
    ///
    /// Built by rewriting an already normalised event rather than assembling an
    /// envelope by hand: a hand-written envelope can diverge from the real one,
    /// and the test would then be testing the fixture.
    fn position_opening_anchor(
        event: &iaam_core::event::Event,
        claim: &ControlClaim,
        period: iaam_core::reconciliation::claim::AssertionPeriod,
    ) -> iaam_core::event::Event {
        let ControlClaim::PositionQuantity { instrument, .. } = *claim else {
            panic!("a position claim");
        };
        let mut anchor = event.clone();
        anchor.kind = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::PositionQuantity {
                instrument,
                quantity: iaam_core::money::Quantity::zero(),
                at: iaam_core::reconciliation::claim::BalancePoint::Opening,
            },
        };
        anchor.legs = Vec::new();
        anchor
    }

    #[test]
    fn recorded_buy_matches_the_non_zero_portfolio_position() {
        let operations = parse_operations(include_str!(
            "../../../../tests/fixtures/api/tinkoff-operations.json"
        ))
        .expect("operations parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");
        let operation = parsed
            .accepted
            .into_iter()
            .find(|operation| matches!(operation.kind, OperationKind::Buy { .. }))
            .expect("recorded SBER buy");
        let claim = parse_portfolio(include_str!(
            "../../../../tests/fixtures/api/tinkoff-portfolio.json"
        ))
        .expect("portfolio parsing")
        .claims
        .into_iter()
        .find(|claim| matches!(claim, ControlClaim::PositionQuantity { .. }))
        .expect("SBER position claim");
        let period = iaam_core::reconciliation::claim::AssertionPeriod::between(
            time::macros::date!(2026 - 08 - 01),
            time::macros::date!(2026 - 08 - 31),
        )
        .expect("August period");
        let event = normalize(
            &operation,
            &NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
                parser_version: ParserVersion(PARSER_VERSION.to_owned()),
            },
        )
        .expect("trade normalization")
        .event;
        // The holding's fold needs a stated start, or the quantity observed is
        // the net of the trades imported and not the position, and nothing can
        // be compared (`iaam-d7hn`). What this test is about is the adapter's
        // mapping, so the anchor is supplied: an opening assertion of nothing
        // held, reaching the start of the period.
        let anchor = position_opening_anchor(&event, &claim, period);
        let observed = iaam_core::reconciliation::observed::observe(
            &[&anchor, &event],
            trading_account(),
            period,
        )
        .expect("observation");

        assert_eq!(
            iaam_core::reconciliation::check::check_claim(&claim, &observed),
            iaam_core::reconciliation::check::ClaimOutcome::Matched
        );
    }

    #[test]
    fn mismatched_order_money_uses_broker_cash_instead_of_refusing() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "money-mismatch",
            quantity_done: "2",
            payment: r#"{"units":"-25","nano":0,"currency":"rub"}"#,
            fields: "",
            trades: &format!(
                "{},{}",
                trade("a", "2026-08-21T10:00:00Z", "1", "12", 0),
                trade("b", "2026-08-21T10:00:01Z", "1", "12", 0)
            ),
        });
        let (accepted, quarantined) = accepted_trade_operations(&body);

        assert!(quarantined.is_empty());
        assert_eq!(accepted.len(), 2);
        for operation in accepted {
            assert!(matches!(
                operation.kind,
                OperationKind::Buy {
                    gross_minor: 1_250,
                    ..
                }
            ));
        }
    }

    #[test]
    fn accrued_interest_is_removed_from_authoritative_order_cash() {
        let trade = trade("bond-fill", "2026-08-21T10:00:00Z", "1", "120", 0);
        let matching = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "bond-matching",
            quantity_done: "1",
            payment: r#"{"units":"-123","nano":0,"currency":"rub"}"#,
            fields: r#","accruedInt":{"units":"3","nano":0,"currency":"rub"}"#,
            trades: &trade,
        });
        let mismatching = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "bond-mismatching",
            quantity_done: "1",
            payment: r#"{"units":"-120","nano":0,"currency":"rub"}"#,
            fields: r#","accruedInt":{"units":"3","nano":0,"currency":"rub"}"#,
            trades: &trade,
        });

        let (accepted, quarantined) = accepted_trade_operations(&matching);
        assert_eq!(accepted.len(), 1);
        assert!(quarantined.is_empty());
        let (accepted, quarantined) = accepted_trade_operations(&mismatching);
        assert!(quarantined.is_empty());
        assert!(matches!(
            accepted[0].kind,
            OperationKind::Buy {
                gross_minor: 11_700,
                accrued_interest_minor: Some(300),
                ..
            }
        ));
    }

    #[test]
    fn mismatched_cash_is_allocated_by_exact_gross_and_reconciles_after_accrued() {
        for operation_type in ["OPERATION_TYPE_BUY", "OPERATION_TYPE_SELL"] {
            let payment = if operation_type == "OPERATION_TYPE_BUY" {
                r#"{"units":"-13","nano":-10000000,"currency":"rub"}"#
            } else {
                r#"{"units":"13","nano":10000000,"currency":"rub"}"#
            };
            let fills = [
                trade("b", "2026-08-21T10:00:00Z", "1", "20", 2_000_000),
                trade("a", "2026-08-21T10:00:00Z", "1", "10", 1_000_000),
            ];
            let adapt = |trades: &str| {
                let body = trading_operation_with_fields(TradingOperationFields {
                    operation_type,
                    state: "OPERATION_STATE_EXECUTED",
                    operation_id: "cash-authoritative",
                    quantity_done: "2",
                    payment,
                    fields: r#","accruedInt":{"units":"3","nano":0,"currency":"rub"}"#,
                    trades,
                });
                let (accepted, quarantined) = accepted_trade_operations(&body);
                assert!(quarantined.is_empty());
                accepted
                    .into_iter()
                    .map(|operation| match operation.kind {
                        OperationKind::Buy {
                            gross_minor,
                            accrued_interest_minor,
                            ..
                        }
                        | OperationKind::Sell {
                            gross_minor,
                            accrued_interest_minor,
                            ..
                        } => (gross_minor, accrued_interest_minor.unwrap()),
                        other => panic!("expected trade, got {other:?}"),
                    })
                    .collect::<Vec<_>>()
            };
            let amounts = adapt(&fills.join(","));
            assert_eq!(amounts, vec![(334, 150), (667, 150)]);
            assert_eq!(
                amounts
                    .iter()
                    .map(|(gross, accrued)| gross + accrued)
                    .sum::<i64>(),
                1_301
            );
            assert_eq!(
                amounts,
                adapt(&[fills[1].clone(), fills[0].clone()].join(","))
            );
        }
    }

    #[test]
    fn payment_sign_must_match_trade_family() {
        for (operation_type, payment, family) in [
            (
                "OPERATION_TYPE_BUY",
                r#"{"units":"1","nano":0,"currency":"rub"}"#,
                "Buy",
            ),
            (
                "OPERATION_TYPE_SELL",
                r#"{"units":"-1","nano":0,"currency":"rub"}"#,
                "Sell",
            ),
        ] {
            let body = trading_operation_with_fields(TradingOperationFields {
                operation_type,
                state: "OPERATION_STATE_EXECUTED",
                operation_id: "wrong-sign",
                quantity_done: "1",
                payment,
                fields: "",
                trades: &trade("fill", "2026-08-21T10:00:00Z", "1", "1", 0),
            });
            let (accepted, quarantined) = accepted_trade_operations(&body);
            assert!(accepted.is_empty());
            assert_eq!(quarantined.len(), 1);
            assert!(quarantined[0].reason.contains(family));
        }
    }

    #[test]
    fn commission_is_allocated_in_minor_units_by_quantity_and_trade_num() {
        let fields = r#","commission":{"units":"1","nano":0,"currency":"rub"}"#;
        let first = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "commission-a",
            quantity_done: "3",
            payment: r#"{"units":"-3","nano":0,"currency":"rub"}"#,
            fields,
            trades: &format!(
                "{},{},{}",
                trade("c", "2026-08-21T10:00:00Z", "1", "1", 0),
                trade("a", "2026-08-21T10:00:00Z", "1", "1", 0),
                trade("b", "2026-08-21T10:00:00Z", "1", "1", 0)
            ),
        });
        let second = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "commission-b",
            quantity_done: "3",
            payment: r#"{"units":"-3","nano":0,"currency":"rub"}"#,
            fields,
            trades: &format!(
                "{},{},{}",
                trade("b", "2026-08-21T10:00:00Z", "1", "1", 0),
                trade("c", "2026-08-21T10:00:00Z", "1", "1", 0),
                trade("a", "2026-08-21T10:00:00Z", "1", "1", 0)
            ),
        });

        let (first, first_quarantine) = accepted_trade_operations(&first);
        let (second, second_quarantine) = accepted_trade_operations(&second);
        assert!(first_quarantine.is_empty());
        assert!(second_quarantine.is_empty());
        let fees = |operations: &[iaam_ingest::SubmittedOperation]| {
            operations
                .iter()
                .map(|operation| match &operation.kind {
                    OperationKind::Buy { basis_fee, .. } => {
                        basis_fee.map(|fee| fee.value().inner().to_string())
                    }
                    _ => panic!("expected buy"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            fees(&first),
            vec![
                Some("0.34".into()),
                Some("0.33".into()),
                Some("0.33".into())
            ]
        );
        assert_eq!(fees(&first), fees(&second));
        for operation in &first {
            let event = normalize(
                operation,
                &NormalizationContext {
                    owner: OwnerId::new_random(),
                    source: SourceId::new_random(),
                    parser_version: ParserVersion(PARSER_VERSION.to_owned()),
                },
            )
            .expect("commission allocation normalizes");
            match event.event.kind {
                EventKind::Trade {
                    basis_fee: Some(basis_fee),
                    basis_fee_exact: Some(exact),
                    ..
                } => assert_eq!(basis_fee.to_calc_dec(), exact.value()),
                other => panic!("expected trade with basis fee, got {other:?}"),
            }
        }
    }

    #[test]
    fn zero_commission_shares_are_absent_without_quarantine() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "small-commission",
            quantity_done: "3",
            payment: r#"{"units":"-3","nano":0,"currency":"rub"}"#,
            fields: r#","commission":{"units":"0","nano":10000000,"currency":"rub"}"#,
            trades: &format!(
                "{},{},{}",
                trade("a", "2026-08-21T10:00:00Z", "1", "1", 0),
                trade("b", "2026-08-21T10:00:00Z", "1", "1", 0),
                trade("c", "2026-08-21T10:00:00Z", "1", "1", 0)
            ),
        });
        let (accepted, quarantined) = accepted_trade_operations(&body);

        assert!(quarantined.is_empty());
        assert_eq!(accepted.len(), 3);
        let fees = accepted
            .iter()
            .map(|operation| match &operation.kind {
                OperationKind::Buy { basis_fee, .. } => basis_fee,
                _ => panic!("expected buy"),
            })
            .collect::<Vec<_>>();
        assert_eq!(fees.iter().filter(|fee| fee.is_none()).count(), 2);
        assert_eq!(fees.iter().filter(|fee| fee.is_some()).count(), 1);
    }

    #[test]
    fn accrued_interest_splits_by_quantity_on_one_date() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "accrued-split",
            quantity_done: "4",
            payment: r#"{"units":"-41","nano":0,"currency":"rub"}"#,
            fields: r#","accruedInt":{"units":"1","nano":0,"currency":"rub"}"#,
            trades: &format!(
                "{},{}",
                trade("a", "2026-08-21T10:00:00Z", "1", "10", 0),
                trade("b", "2026-08-21T10:00:01Z", "3", "10", 0)
            ),
        });
        let (accepted, quarantined) = accepted_trade_operations(&body);

        assert!(quarantined.is_empty());
        let accrued = accepted
            .iter()
            .map(|operation| match &operation.kind {
                OperationKind::Buy {
                    accrued_interest_minor,
                    ..
                } => *accrued_interest_minor,
                _ => panic!("expected buy"),
            })
            .collect::<Vec<_>>();
        assert_eq!(accrued, vec![Some(25), Some(75)]);
    }

    #[test]
    fn real_currency_zero_accrued_interest_reaches_the_event_as_known_zero() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "accrued-zero",
            quantity_done: "1",
            payment: r#"{"units":"-10","nano":0,"currency":"rub"}"#,
            fields: r#","accruedInt":{"units":"0","nano":0,"currency":"rub"}"#,
            trades: &trade("a", "2026-08-21T10:00:00Z", "1", "10", 0),
        });
        let (accepted, quarantined) = accepted_trade_operations(&body);

        assert!(quarantined.is_empty());
        let normalized = normalize(
            &accepted[0],
            &NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
                parser_version: ParserVersion(PARSER_VERSION.to_owned()),
            },
        )
        .expect("known zero is valid");
        match normalized.event.kind {
            EventKind::Trade {
                accrued_interest: Some(value),
                ..
            } => assert_eq!(value.amount().raw(), 0),
            other => panic!("expected trade with known accrued interest, got {other:?}"),
        }
    }

    #[test]
    fn non_utc_fill_offsets_use_recorded_utc_dates_for_accrued_interest_quarantine() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "accrued-utc-dates",
            quantity_done: "2",
            payment: r#"{"units":"-21","nano":0,"currency":"rub"}"#,
            fields: r#","accruedInt":{"units":"1","nano":0,"currency":"rub"}"#,
            trades: &format!(
                "{},{}",
                trade("a", "2026-08-21T02:00:00+03:00", "1", "10", 0),
                trade("b", "2026-08-21T04:00:00+03:00", "1", "10", 0)
            ),
        });
        let (accepted, quarantined) = accepted_trade_operations(&body);

        assert!(accepted.is_empty());
        assert_eq!(quarantined.len(), 1);
        assert!(quarantined[0].reason.contains("2026-08-20"));
        assert!(quarantined[0].reason.contains("2026-08-21"));
    }

    #[test]
    fn multi_day_accrued_interest_is_quarantined_with_both_dates() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "accrued-multi-day",
            quantity_done: "2",
            payment: r#"{"units":"-21","nano":0,"currency":"rub"}"#,
            fields: r#","accruedInt":{"units":"1","nano":0,"currency":"rub"}"#,
            trades: &format!(
                "{},{}",
                trade("a", "2026-08-21T10:00:00Z", "1", "10", 0),
                trade("b", "2026-08-22T10:00:00Z", "1", "10", 0)
            ),
        });
        let (accepted, quarantined) = accepted_trade_operations(&body);

        assert!(accepted.is_empty());
        assert_eq!(quarantined.len(), 1);
        assert!(quarantined[0].reason.contains("2026-08-21"));
        assert!(quarantined[0].reason.contains("2026-08-22"));
    }

    #[test]
    fn accrued_interest_currency_mismatch_is_quarantined() {
        let body = trading_operation_with_fields(TradingOperationFields {
            operation_type: "OPERATION_TYPE_BUY",
            state: "OPERATION_STATE_EXECUTED",
            operation_id: "accrued-currency",
            quantity_done: "1",
            payment: r#"{"units":"-11","nano":0,"currency":"rub"}"#,
            fields: r#","accruedInt":{"units":"1","nano":0,"currency":"usd"}"#,
            trades: &trade("a", "2026-08-21T10:00:00Z", "1", "10", 0),
        });
        let (accepted, quarantined) = accepted_trade_operations(&body);

        assert!(accepted.is_empty());
        assert_eq!(quarantined.len(), 1);
        assert!(quarantined[0].reason.contains("currency"));
    }
    #[test]
    fn formats_the_inclusive_interval_end_as_the_last_nanosecond() {
        assert_eq!(
            rfc3339_operation_end(time::macros::date!(2026 - 08 - 31)),
            "2026-08-31T23:59:59.999999999Z"
        );
    }
    #[tokio::test]
    async fn follows_pages_and_repeats_the_interval_request_fields() {
        let mut request = GetOperationsByCursorRequest::new("account");
        request.from = Some("2026-08-01T00:00:00Z".to_owned());
        request.to = Some("2026-08-31T23:59:59.999999999Z".to_owned());
        request.limit = Some(1000);
        let mut requests = Vec::new();
        let mut responses = vec![
            page_json(true, Some("cursor-2"), Some("operation-1")),
            page_json(false, None, Some("operation-2")),
        ];

        let operations = fetch_operation_pages(request, |request| {
            requests.push(request);
            let body = responses.remove(0);
            async move { Ok(body) }
        })
        .await
        .expect("both pages parse");

        assert_eq!(
            operations
                .iter()
                .map(|operation| operation.operation_id.as_str())
                .collect::<Vec<_>>(),
            ["operation-1", "operation-2"]
        );
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].from, requests[0].from);
        assert_eq!(requests[1].to, requests[0].to);
        assert_eq!(requests[1].limit, Some(1000));
        assert_eq!(requests[1].cursor.as_deref(), Some("cursor-2"));
    }

    #[tokio::test]
    async fn refuses_an_a_b_a_cursor_cycle_without_returning_partial_operations() {
        let request = GetOperationsByCursorRequest::new("account");
        let mut responses = vec![
            page_json(true, Some("cursor-a"), Some("operation-1")),
            page_json(true, Some("cursor-b"), Some("operation-2")),
            page_json(true, Some("cursor-a"), Some("operation-3")),
        ];
        let mut requests = Vec::new();

        let error = fetch_operation_pages(request, |request| {
            requests.push(request);
            let body = responses.remove(0);
            async move { Ok(body) }
        })
        .await
        .expect_err("repeated cursor must refuse");

        assert!(error.to_string().contains("cursor-a"));
        assert_eq!(requests.len(), 3);
    }

    #[tokio::test]
    async fn continues_pagination_after_one_hundred_pages() {
        let request = GetOperationsByCursorRequest::new("account");
        let mut responses = (1..=101)
            .map(|number| {
                page_json(
                    number != 101,
                    (number != 101)
                        .then(|| format!("cursor-{number}"))
                        .as_deref(),
                    Some(&format!("operation-{number}")),
                )
            })
            .collect::<Vec<_>>();
        let mut requests = Vec::new();
        let result = fetch_operation_pages(request, |request| {
            requests.push(request);
            let body = responses.remove(0);
            async move { Ok(body) }
        })
        .await;
        let Ok(operations) = result else {
            panic!("pagination stopped at the removed page cap");
        };

        assert_eq!(operations.len(), 101);
        assert_eq!(requests.len(), 101);
    }

    #[tokio::test]
    async fn a_missing_cursor_stays_a_refusal() {
        let request = GetOperationsByCursorRequest::new("account");
        let error = fetch_operation_pages(request, |_request| async {
            Ok(r#"{"hasNext":true,"items":[]}"#.to_owned())
        })
        .await
        .expect_err("hasNext without a cursor must refuse");

        assert!(error.to_string().contains("cursor"));
    }

    fn page_json(has_next: bool, next_cursor: Option<&str>, operation_id: Option<&str>) -> String {
        let next_cursor = next_cursor.map_or(String::new(), |cursor| {
            format!(r#","nextCursor":"{cursor}""#)
        });
        let item = operation_id.map_or_else(String::new, |operation_id| {
            format!(
                r#"{{"cursor":"row-{operation_id}","brokerAccountId":"account","id":"{operation_id}","date":"2026-08-20T10:11:12Z","type":"OPERATION_TYPE_INPUT","state":"OPERATION_STATE_EXECUTED"}}"#
            )
        });
        format!(r#"{{"hasNext":{has_next}{next_cursor},"items":[{item}]}}"#)
    }

    // --- through the gateway ------------------------------------------------

    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    use iaam_broker::credentials::{Key, open, seal};
    use iaam_broker::environment::Environment;
    use iaam_broker::tinkoff::{TinkoffClient, TinkoffError};
    use iaam_http::gateway::{ATTEMPTS, Clock, FIRST_BACKOFF};
    use iaam_http::resilience::{Outcome, RetryPolicy};
    use iaam_http::{Outbound, RequestAllowance};

    use super::fake::{self, Answer};
    use crate::ports::{BrokerChannel, BrokerRequestContext};

    const TOKEN: &str = "secret-token-42";

    fn channel(gateway: Arc<dyn Outbound>) -> super::TinkoffChannel {
        let key = Key::from_bytes([5; 32]);
        let token = open(&key, &seal(&key, TOKEN)).expect("token opens");
        let store = iaam_store::SqliteStore::open_in_memory().expect("instrument directory");
        for uid in [
            "01234567-89ab-cdef-0123-456789abcdef",
            "cccccccc-0000-0000-0000-000000000099",
            "dddddddd-0000-0000-0000-000000000042",
            "bbbbbbbb-0000-0000-0000-0000000000c2",
            "bbbbbbbb-0000-0000-0000-0000000000c4",
            "dddddddd-0000-0000-0000-000000000098",
            "bbbbbbbb-0000-0000-0000-0000000000c8",
        ] {
            store
                .upsert_instrument(&iaam_store::reference::InstrumentRecord {
                    id: InstrumentId(Uuid::parse_str(uid).expect("invented UID")),
                    kind: None,
                    symbol: "TEST".to_owned(),
                    title: "Invented instrument".to_owned(),
                    currencies: iaam_core::instrument::CurrencyRoles::uniform(
                        iaam_core::money::CurrencyCode::Rub,
                    ),
                    lineage: None,
                    metadata: None,
                    blocked_tca_flag: false,
                })
                .expect("seed instrument");
            store
                .record_alias(&iaam_store::reference::AliasRecord {
                    namespace: iaam_core::instrument::AliasNamespace::Isin,
                    value: format!("TEST-{uid}"),
                    instrument: InstrumentId(Uuid::parse_str(uid).expect("invented UID")),
                    interval: iaam_core::instrument::AliasInterval {
                        valid_from: time::macros::date!(0001 - 01 - 01),
                        valid_to: None,
                    },
                    source: SourceId(Uuid::from_u128(9)),
                })
                .expect("seed ISIN");
        }
        super::TinkoffChannel::new(
            TinkoffClient::new(Environment::Prod, token, gateway),
            SourceId(Uuid::from_u128(9)),
            dictionary(),
            Arc::new(crate::adapters::sqlite::SqliteAdapter::new(store)),
        )
    }

    async fn fetch(
        channel: &super::TinkoffChannel,
    ) -> Result<super::ParsedOperations, BrokerError> {
        channel
            .fetch_operations(
                AccountId(Uuid::from_u128(1)),
                "00000000-0000-0000-0000-000000000001",
                time::macros::date!(2026 - 08 - 01),
                time::macros::date!(2026 - 08 - 31),
                broker_context(None),
            )
            .await
    }

    #[tokio::test]
    async fn descriptions_without_isin_are_refused_before_any_operation_kind_is_mapped() {
        let missing = "eeeeeeee-0000-0000-0000-000000000001";
        for kind in [
            "OPERATION_TYPE_BUY",
            "OPERATION_TYPE_SELL",
            "OPERATION_TYPE_COUPON",
            "OPERATION_TYPE_DIVIDEND",
            "OPERATION_TYPE_BOND_REPAYMENT_FULL",
            "OPERATION_TYPE_BOND_REPAYMENT",
        ] {
            let body =
                income_operation(kind).replace("01234567-89ab-cdef-0123-456789abcdef", missing);
            let (gateway, _, _) = fake::gateway(vec![Answer::status(200, &body)], None);
            let parsed = fetch(&channel(gateway))
                .await
                .expect("unknown instrument is a row refusal");
            assert!(parsed.accepted.is_empty());
            assert_eq!(parsed.quarantined.len(), 1);
            let reason = &parsed.quarantined[0].reason;
            assert!(reason.contains(missing), "{reason}");
            assert!(reason.contains("names no ISIN"), "{reason}");
        }
    }

    #[tokio::test]
    async fn endless_operation_pages_stop_before_request_three_hundred_and_one() {
        let pages = (1..=300)
            .map(|number| {
                Answer::status(
                    200,
                    &page_json(
                        true,
                        Some(&format!("cursor-{number}")),
                        Some(&format!("operation-{number}")),
                    ),
                )
            })
            .collect();
        let (gateway, log, _) = fake::gateway(
            pages,
            Some(Answer::status(
                200,
                &page_json(false, None, Some("operation-301")),
            )),
        );
        let allowance = RequestAllowance::new(300);
        let adapter = channel(gateway);

        let error = adapter
            .fetch_operations(
                AccountId(Uuid::from_u128(1)),
                "00000000-0000-0000-0000-000000000001",
                time::macros::date!(2026 - 08 - 01),
                time::macros::date!(2026 - 08 - 31),
                BrokerRequestContext {
                    deadline: None,
                    allowance: &allowance,
                },
            )
            .await
            .expect_err("the next page exceeds this sync's request allowance");

        assert!(matches!(
            error,
            BrokerError::RequestCeiling {
                ref broker,
                ceiling: 300
            } if broker == "tinkoff"
        ));
        assert_eq!(log.lock().expect("log").len(), 300);
        let message = error.to_string();
        assert!(message.contains("300"), "{message}");
        assert!(message.contains("narrow"), "{message}");
    }

    /// T-Invest limits a caller with 429 and names a reset shorter than the
    /// mandatory pause. Once the first-send safety wait has elapsed, the fetch
    /// stops on the response and exposes the full minute before another call.
    #[tokio::test]
    async fn a_multi_page_fetch_stops_at_a_limit_and_exposes_the_host_pause() {
        let (gateway, log, time) = fake::gateway(
            vec![
                Answer {
                    status: 429,
                    body: "{}".to_owned(),
                    reset: Some(Duration::from_secs(7)),
                    remaining: None,
                },
                Answer::status(200, &page_json(false, None, Some("operation-1"))),
            ],
            None,
        );

        let error = fetch(&channel(gateway))
            .await
            .expect_err("the broker host is paused");

        assert!(
            matches!(
                &error,
                BrokerError::Unreachable {
                    detail,
                    retry_after: Some(retry_after),
                    ..
                } if detail.contains("paused after status 429")
                    && *retry_after == Duration::from_secs(60)
            ),
            "{error:?}"
        );
        assert_eq!(time.slept(), [Duration::from_secs(60)]);
        assert_eq!(log.lock().expect("log").len(), 1);
    }

    #[tokio::test]
    async fn a_rejected_token_is_a_refusal_sent_once_and_carrying_no_token() {
        let (gateway, log, time) = fake::gateway(
            vec![Answer::status(401, &format!(r#"{{"message":"{TOKEN}"}}"#))],
            None,
        );

        let error = fetch(&channel(gateway)).await.expect_err("401 refuses");

        assert!(matches!(error, BrokerError::Refused { .. }), "{error:?}");
        assert_eq!(log.lock().expect("log").len(), 1);
        assert_eq!(time.slept(), [Duration::from_secs(60)]);
        assert!(!error.to_string().contains(TOKEN));
        assert!(!format!("{error:?}").contains(TOKEN));
    }

    #[tokio::test]
    async fn another_client_error_stays_a_refusal() {
        let (gateway, log, _) = fake::gateway(vec![Answer::status(400, "{}")], None);

        let error = fetch(&channel(gateway)).await.expect_err("400 refuses");

        assert!(matches!(error, BrokerError::Refused { .. }), "{error:?}");
        assert_eq!(log.lock().expect("log").len(), 1);
    }

    /// A server error is retried; one that outlasts the retries is
    /// "unreachable, try later" and says when, not a refusal.
    #[tokio::test]
    async fn a_lasting_server_error_is_unreachable_and_says_when() {
        let (gateway, log, _) = fake::gateway(Vec::new(), Some(Answer::status(500, "{}")));

        let error = channel(gateway)
            .fetch_portfolio(
                AccountId(Uuid::from_u128(1)),
                "00000000-0000-0000-0000-000000000001",
                time::macros::date!(2026 - 08 - 31),
                broker_context(None),
            )
            .await
            .expect_err("every attempt failed");

        assert_eq!(log.lock().expect("log").len(), ATTEMPTS as usize);
        let when = RetryPolicy::new(ATTEMPTS, FIRST_BACKOFF).delay(ATTEMPTS, &Outcome::status(500));
        assert!(
            matches!(&error, BrokerError::Unreachable { detail, .. }
                if detail.contains(&format!("retry after {when:?}"))),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_server_error_that_passes_on_retry_is_no_error() {
        let (gateway, log, _) = fake::gateway(
            vec![
                Answer::status(500, "{}"),
                Answer::status(200, &page_json(false, None, None)),
            ],
            None,
        );

        fetch(&channel(gateway)).await.expect("the retry passes");

        assert_eq!(log.lock().expect("log").len(), 2);
    }

    /// A 429 stops a multi-page fetch without waiting in the request and says
    /// how many pages had arrived before the persisted host pause.
    #[tokio::test]
    async fn a_fetch_limited_after_two_pages_is_unreachable_and_names_the_pages_fetched() {
        let (gateway, log, time) = fake::gateway(
            vec![
                Answer::status(200, &page_json(true, Some("cursor-2"), Some("operation-1"))),
                Answer::status(200, &page_json(true, Some("cursor-3"), Some("operation-2"))),
                Answer {
                    status: 429,
                    body: "{}".to_owned(),
                    reset: Some(Duration::from_secs(60)),
                    remaining: None,
                },
            ],
            None,
        );
        let deadline = time.now() + Duration::from_secs(90);

        let error = channel(gateway)
            .fetch_operations(
                AccountId(Uuid::from_u128(1)),
                "00000000-0000-0000-0000-000000000001",
                time::macros::date!(2026 - 08 - 01),
                time::macros::date!(2026 - 08 - 31),
                broker_context(Some(deadline)),
            )
            .await
            .expect_err("the named wait crosses the deadline");

        assert_eq!(
            log.lock().expect("log").len(),
            3,
            "error={error:?}; sleeps={:?}",
            time.slept()
        );
        assert_eq!(
            time.slept(),
            [
                Duration::from_secs(60),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ],
            "first-send safety and host spacing were waited before the three sends"
        );
        assert!(
            matches!(
                &error,
                BrokerError::Unreachable {
                    detail,
                    retry_after: Some(retry_after),
                    ..
                } if detail.contains("reopens at")
                    && detail.ends_with("operation pages fetched before it: 2")
                    && *retry_after == Duration::from_secs(60)
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_portfolio_asked_at_its_deadline_sends_nothing() {
        let (gateway, log, time) = fake::gateway(Vec::new(), None);

        let error = channel(gateway)
            .fetch_portfolio(
                AccountId(Uuid::from_u128(1)),
                "00000000-0000-0000-0000-000000000001",
                time::macros::date!(2026 - 08 - 31),
                broker_context(Some(time.now())),
            )
            .await
            .expect_err("the deadline is now");

        assert!(log.lock().expect("log").is_empty());
        assert!(
            matches!(error, BrokerError::Unreachable { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_refusal_other_than_unreachable_does_not_name_the_pages() {
        let refused = BrokerError::Refused {
            broker: "tinkoff".to_owned(),
            detail: "token".to_owned(),
        };
        assert_eq!(super::after_pages(refused.clone(), 3), refused);
    }

    #[test]
    fn a_gateway_that_refuses_before_sending_is_an_adapter_fault() {
        let error = super::tinkoff_error(super::TinkoffError::Gateway(
            iaam_http::GatewayError::UnknownBudget {
                destination: iaam_http::Destination::TinkoffProd,
                path: "/Nowhere".to_owned(),
            },
        ));
        assert!(matches!(error, BrokerError::Adapter { .. }), "{error:?}");
    }

    #[test]
    fn egress_and_tally_refusals_remain_actionable_for_the_sync_surface() {
        for refusal in [
            iaam_http::GatewayError::BrokerEgressOff,
            iaam_http::GatewayError::TallyCorrupt {
                path: std::path::PathBuf::from("/run/iaam/outbound-tally"),
                reason: "missing header".to_owned(),
            },
        ] {
            let error = super::tinkoff_error(super::TinkoffError::Gateway(refusal));
            let BrokerError::Unreachable {
                detail,
                retry_after,
                ..
            } = error
            else {
                panic!("operational refusal became an adapter fault: {error:?}");
            };
            assert!(retry_after.is_none());
            assert!(
                detail.contains("IAAM_BROKER_EGRESS") || detail.contains("outbound-tally"),
                "{detail}"
            );
        }
    }

    /// A client or trust root this build could not construct is our fault
    /// (500), not "the broker is unreachable, retry later": retrying meets the
    /// same fault.
    #[test]
    fn a_transport_that_cannot_be_built_is_an_adapter_fault() {
        for fault in [
            iaam_http::HttpError::ClientNotBuilt("no".to_owned()),
            iaam_http::HttpError::TrustAnchorNotParsed("no".to_owned()),
        ] {
            let error = super::tinkoff_error(super::TinkoffError::Transport(fault));
            assert!(matches!(error, BrokerError::Adapter { .. }), "{error:?}");
        }
    }
}

#[cfg(test)]
mod full_redemption_tests {
    use super::*;

    // Invented end to end, including the zero execution counters that belong
    // to the channel's non-trading repayment rows.
    fn full_repayment_row() -> serde_json::Value {
        serde_json::json!({
            "cursor": "invented-redemption-cursor",
            "brokerAccountId": "invented-broker-account",
            "id": "invented-redemption",
            "parentOperationId": "",
            "date": "2026-08-20T10:11:12Z",
            "type": "OPERATION_TYPE_BOND_REPAYMENT_FULL",
            "state": "OPERATION_STATE_EXECUTED",
            "instrumentUid": "01234567-89ab-cdef-0123-456789abcdef",
            "positionUid": "11234567-89ab-cdef-0123-456789abcdef",
            "figi": "TESTBOND0001",
            "instrumentType": "bond",
            "instrumentKind": "INSTRUMENT_TYPE_BOND",
            "ticker": "TESTBOND",
            "classCode": "TQCB",
            "name": "Invented bond",
            "description": "Full repayment of an invented bond",
            "quantity": "3",
            "quantityRest": "0",
            "quantityDone": "0",
            "payment": {"units": "2400", "nano": 0, "currency": "rub"},
            "price": {"units": "800", "nano": 0, "currency": "rub"},
            "commission": {"units": "0", "nano": 0, "currency": ""},
            "accruedInt": {"units": "0", "nano": 0, "currency": ""},
            "cancelReason": "",
            "tradesInfo": {"trades": []},
            "childOperations": []
        })
    }

    fn adapt_row(row: serde_json::Value) -> ParsedOperations {
        let body = serde_json::json!({"hasNext": false, "items": [row]}).to_string();
        let operations = parse_operations(&body)
            .expect("invented repayment parses")
            .operations;
        let (dictionary, unreadable) = OperationKindDictionary::build(
            iaam_broker::tinkoff::dictionary_seed::TINKOFF_OPERATION_KINDS
                .iter()
                .copied(),
        );
        assert!(unreadable.is_empty());
        adapt_operations(AccountId(Uuid::from_u128(1)), operations, &dictionary)
            .expect("row mapping")
    }

    #[test]
    fn full_repayment_becomes_principal_return_and_retires_the_reported_quantity() {
        use iaam_core::event::corporate_action::CorporateAction;
        use iaam_ingest::journal_event::JournalFact;

        let mut row = full_repayment_row();
        // Principal comes from the payment, not a quote or execution counter.
        row["price"]["units"] = serde_json::json!("999");
        let parsed = adapt_row(row);
        assert!(
            parsed.accepted.is_empty(),
            "redemption is not income or a sale"
        );
        assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
        assert_eq!(parsed.journal.len(), 1);
        let submitted = &parsed.journal[0];
        assert_eq!(submitted.account, AccountId(Uuid::from_u128(1)));
        assert_eq!(
            submitted.source_operation_id.as_deref(),
            Some("invented-redemption")
        );
        assert_eq!(
            submitted.idempotency_key.as_deref(),
            Some("invented-broker-account/invented-redemption")
        );
        let JournalFact::CorporateAction(CorporateAction::Redemption {
            instrument,
            custody,
            quantity,
            principal_returned_per_unit,
            compensation,
            effective_date,
            record_date,
            grounds,
        }) = &submitted.fact
        else {
            panic!("full repayment must be a journal redemption");
        };
        assert_eq!(
            *instrument,
            InstrumentId(
                Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").expect("invented UID")
            )
        );
        assert_eq!(*custody, None, "positionUid is not a custody place");
        assert_eq!(quantity.0.inner().to_string(), "3");
        assert_eq!(
            principal_returned_per_unit
                .per_unit_exact(*quantity)
                .unwrap(),
            iaam_core::numeric::exact::Exact::from_int(800)
        );
        assert_eq!(principal_returned_per_unit.currency(), CurrencyCode::Rub);
        assert_eq!(compensation.amount().raw(), 240_000);
        assert_eq!(compensation.currency(), CurrencyCode::Rub);
        assert_eq!(*effective_date, time::macros::date!(2026 - 08 - 20));
        assert_eq!(*record_date, None);
        assert_eq!(*grounds, None);
        let normalized = iaam_ingest::journal_event::normalize_journal_event(
            submitted,
            &iaam_ingest::journal_event::JournalEventEnrichment::default(),
            &iaam_ingest::operation::NormalizationContext {
                owner: iaam_core::ids::OwnerId(Uuid::from_u128(2)),
                source: SourceId(Uuid::from_u128(3)),
                parser_version: ParserVersion(TINKOFF_PARSER_VERSION.to_owned()),
            },
        )
        .expect("redemption normalizes into journal legs");
        normalized
            .event
            .validate_structure()
            .expect("valid redemption");
        assert_eq!(normalized.event.legs.len(), 2);
        let principal = normalized
            .event
            .legs
            .iter()
            .find(|leg| leg.kind == iaam_core::event::leg::LegKind::Principal)
            .expect("principal cash return");
        assert_eq!(
            principal.cash_effect().expect("cash").amount().raw(),
            240_000
        );
        let security = normalized
            .event
            .legs
            .iter()
            .find(|leg| leg.kind == iaam_core::event::leg::LegKind::SecurityQuantity)
            .expect("retired quantity");
        assert_eq!(
            security.quantity.expect("quantity").0.inner().to_string(),
            "-3"
        );
        assert_eq!(security.custody, None);
    }

    #[test]
    fn full_repayment_keeps_one_ruble_for_three_securities_exactly() {
        use iaam_core::event::corporate_action::CorporateAction;
        use iaam_core::money::ReturnedPrincipal;
        use iaam_core::numeric::exact::Exact;
        use iaam_ingest::journal_event::JournalFact;

        let mut row = full_repayment_row();
        row["payment"] = serde_json::json!({"units": "1", "nano": 0, "currency": "rub"});
        let parsed = adapt_row(row);
        assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
        let JournalFact::CorporateAction(CorporateAction::Redemption {
            quantity,
            principal_returned_per_unit,
            compensation,
            ..
        }) = &parsed.journal[0].fact
        else {
            panic!("expected a redemption");
        };
        assert!(
            matches!(principal_returned_per_unit, ReturnedPrincipal::Posted(amount) if amount.amount().raw() == 100)
        );
        assert_eq!(compensation.amount().raw(), 100);
        assert_eq!(
            principal_returned_per_unit
                .per_unit_exact(*quantity)
                .unwrap(),
            Exact::new(1, 3).unwrap()
        );
    }

    #[test]
    fn full_repayment_refuses_missing_or_nonpositive_principal_and_invalid_quantity() {
        for (field, value, reason) in [
            ("payment", serde_json::Value::Null, "positive payment"),
            (
                "payment",
                serde_json::json!({"units": "0", "nano": 0, "currency": "rub"}),
                "positive payment",
            ),
            (
                "payment",
                serde_json::json!({"units": "-1", "nano": 0, "currency": "rub"}),
                "positive payment",
            ),
            ("quantity", serde_json::Value::Null, "positive quantity"),
            ("quantity", serde_json::json!("-1"), "positive quantity"),
            ("instrumentUid", serde_json::Value::Null, "instrumentUid"),
            (
                "instrumentUid",
                serde_json::json!("not-a-uuid"),
                "instrumentUid",
            ),
        ] {
            let mut row = full_repayment_row();
            row[field] = value;
            let parsed = adapt_row(row.clone());
            assert!(parsed.accepted.is_empty());
            assert!(parsed.journal.is_empty());
            assert_eq!(parsed.quarantined.len(), 1);
            let refusal = &parsed.quarantined[0];
            assert!(
                refusal.reason.contains(reason),
                "{field}: {}",
                refusal.reason
            );
            assert_eq!(refusal.dimensions, cash_positions());
            assert_eq!(refusal.raw, row);
        }
    }

    #[test]
    fn zero_quantity_full_repayment_waits_for_journal_holdings() {
        let mut row = full_repayment_row();
        row["quantity"] = serde_json::json!("0");
        row["price"] = serde_json::json!({"units": "0", "nano": 0, "currency": "rub"});
        let parsed = adapt_row(row.clone());
        assert!(parsed.accepted.is_empty());
        assert!(parsed.journal.is_empty());
        assert!(parsed.quarantined.is_empty());
        assert_eq!(parsed.pending_redemptions.len(), 1);
        assert_eq!(
            parsed.pending_redemptions[0].compensation.amount().raw(),
            240_000
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&parsed.pending_redemptions[0].raw).unwrap(),
            row
        );
    }

    #[test]
    fn unexecuted_full_repayment_does_not_create_a_journal_fact() {
        for state in ["OPERATION_STATE_CANCELED", "OPERATION_STATE_PROGRESS"] {
            let mut row = full_repayment_row();
            row["state"] = serde_json::json!(state);
            let parsed = adapt_row(row);
            assert!(parsed.accepted.is_empty());
            assert!(parsed.journal.is_empty());
            assert_eq!(parsed.quarantined.len(), 1);
            assert!(parsed.quarantined[0].reason.contains(state));
            assert_eq!(parsed.quarantined[0].dimensions, cash_positions());
        }
    }
}
