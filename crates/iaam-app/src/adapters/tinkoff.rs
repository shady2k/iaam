//! A `BrokerChannel` port over the parsed T-Invest channel.
//!
//! Response parsing stays in `iaam-broker`; this layer only requests
//! the body, quarantines rejected rows, and binds stable
//! port types.

use std::collections::{BTreeSet, HashSet};
use std::future::Future;

use async_trait::async_trait;
use iaam_broker::operation_kind::OperationKindDictionary;
use iaam_broker::tinkoff::{
    ChannelMoney, ChannelOperation, ChannelOperationKind, ChannelOrderState,
    GetOperationsByCursorRequest, ParseError, TINKOFF_PARSER_VERSION, TinkoffClient, TinkoffError,
    parse_operations, parse_portfolio,
};
use iaam_core::event::kind::{FeeOrigin, IncomeKind};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, SourceId};
use iaam_core::money::{CalcMoney, CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::{Dimension, evidence::SourceChannel};
use iaam_core::rules::trade_allocation::{
    allocate_minor as core_allocate_minor, check_order_completeness,
};
use iaam_ingest::SubmittedOperation;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::ports::{
    BrokerChannel, BrokerError, ParsedOperations, PortfolioAsOf, PortfolioSnapshot, Quarantined,
};

const BROKER: &str = "tinkoff";
const OPERATIONS_PAGE_LIMIT: i32 = 1_000;
const MAX_OPERATION_PAGES: usize = 100;

/// Broker channel implementation for T-Invest.
pub struct TinkoffChannel {
    client: TinkoffClient,
    source: SourceId,
    /// Dictionary of operation kinds for this channel. It arrives from storage
    /// ready to use: parsing in `iaam-broker` knows nothing about storage, and
    /// this adapter binds them — using the same approach already used
    /// for SQLite.
    dictionary: OperationKindDictionary,
}

impl TinkoffChannel {
    /// Creates a channel with a preconfigured HTTP client, data source
    /// and operation kind dictionary.
    #[must_use]
    pub fn new(
        client: TinkoffClient,
        source: SourceId,
        dictionary: OperationKindDictionary,
    ) -> Self {
        Self {
            client,
            source,
            dictionary,
        }
    }
}

#[async_trait]
impl BrokerChannel for TinkoffChannel {
    async fn fetch_operations(
        &self,
        account: AccountId,
        from: time::Date,
        to: time::Date,
    ) -> Result<ParsedOperations, BrokerError> {
        let mut request = GetOperationsByCursorRequest::new(account.inner().to_string());
        request.from = Some(rfc3339_midnight(from));
        request.to = Some(rfc3339_operation_end(to));
        request.limit = Some(OPERATIONS_PAGE_LIMIT);
        let operations = fetch_operation_pages(request, |request| async move {
            self.client.get_operations_by_cursor(&request).await
        })
        .await?;
        adapt_operations(account, operations, &self.dictionary)
    }

    async fn fetch_portfolio(
        &self,
        account: AccountId,
        _at: time::Date,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        let body = self
            .client
            .get_portfolio(&account.inner().to_string())
            .await
            .map_err(tinkoff_error)?;
        adapt_portfolio(&body)
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

fn adapt_portfolio(body: &str) -> Result<PortfolioSnapshot, BrokerError> {
    parse_portfolio(body)
        .map(|claims| PortfolioSnapshot {
            as_of: PortfolioAsOf::Current,
            claims,
        })
        .map_err(parse_error)
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

    for _ in 1..=MAX_OPERATION_PAGES {
        let body = fetch(request.clone()).await.map_err(tinkoff_error)?;
        let page = parse_operations(&body).map_err(parse_error)?;
        operations.extend(page.operations);
        if !page.has_next {
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
    Err(unparsable(format!(
        "operations response exceeded page cap: fetched {MAX_OPERATION_PAGES} pages while hasNext remained true"
    )))
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

fn adapt_operations(
    account: AccountId,
    operations: Vec<ChannelOperation>,
    dictionary: &OperationKindDictionary,
) -> Result<ParsedOperations, BrokerError> {
    // An empty dictionary means an unconfigured channel, not an unknown broker.
    // Without this check, the owner would receive a rejection for every code
    // separately and investigate the broker instead of the configuration.
    if dictionary.is_empty() && !operations.is_empty() {
        return Err(unparsable(
            "the channel's operation kind dictionary is empty: there is nothing to parse the export with",
        ));
    }
    let mut accepted = Vec::new();
    let mut quarantined = Vec::new();
    for mut operation in operations {
        // Take the payload before moving the operation into conversion; no later stage reads it.
        let raw = std::mem::take(&mut operation.raw);
        let kind = dictionary.kind_of(&operation.source_kind);
        if let Some(rejection) = operation.rejection.as_ref() {
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

        if let Some(reason) = securities_transfer_reason(&kind) {
            quarantined.push(Quarantined {
                raw,
                reason: reason.to_owned(),
                dimensions: dimensions_for_kind(&kind),
            });
            continue;
        }
        if let Some(reason) = order_state_reason(&kind, &operation.state) {
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
        if matches!(kind, ChannelOperationKind::Buy | ChannelOperationKind::Sell) {
            if let Some(reason) = trade_row_reason(&operation, &kind) {
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
                Err(RowRefusal::Row { reason, dimensions }) => quarantined.push(Quarantined {
                    raw,
                    reason,
                    dimensions,
                }),
                Err(error @ RowRefusal::Adapter(_)) => {
                    return Err(error.into_broker_error());
                }
            }
        } else {
            let dimensions = dimensions_for_kind(&kind);
            match operation_to_submitted(account, operation, kind)
                .map_err(|error| error.with_dimensions(dimensions))
            {
                Ok(operation) => accepted.push(operation),
                Err(RowRefusal::Row { reason, dimensions }) => quarantined.push(Quarantined {
                    raw,
                    reason,
                    dimensions,
                }),
                Err(error @ RowRefusal::Adapter(_)) => {
                    return Err(error.into_broker_error());
                }
            }
        }
    }
    Ok(ParsedOperations {
        accepted,
        quarantined,
    })
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
        ChannelOperationKind::SecuritiesTransferOut => {
            Some("outbound securities transfer: securities moved without a cash movement")
        }
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
            return Err(row_unparsable(
                "outbound securities transfer: securities moved without a cash movement",
            ));
        }
        ChannelOperationKind::Transfer => {
            return Err(RowRefusal::Row {
                reason: "transfer does not contain a recipient account".to_owned(),
                dimensions: BTreeSet::new(),
            });
        }
        // Amortisation and redemption are corporate actions, not
        // owner operations: they have their own representation and endpoint
        // (POST /v1/ingest/journal-events). The rejection here identifies
        // WHAT IS MISSING, rather than «unknown kind»: the channel reports the
        // payment amount, but not the returned face value per unit or the custody
        // location; without them the fact cannot be constructed, and substituting
        // a guess would record something that never happened in the append-only journal.
        ChannelOperationKind::BondAmortisation => {
            return Err(row_unparsable(
                "bond amortisation: the channel does not report the returned face value per unit \
                 or custody location — the fact is entered via the journal endpoint",
            ));
        }
        ChannelOperationKind::BondRedemption => {
            return Err(row_unparsable(
                "bond redemption: the channel does not report the returned face value per unit \
                 or custody location — the fact is entered via the journal endpoint",
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
    let grosses = operation
        .trades
        .iter()
        .map(|trade| iaam_core::money::gross_for_fill(trade.price, trade.quantity))
        .collect::<Result<Vec<_>, _>>();
    let Ok(grosses) = grosses else {
        return Some("trade gross total is not representable".to_owned());
    };
    let Some(payment_minor) = payment.magnitude() else {
        return Some("payment magnitude is not representable".to_owned());
    };
    let accrued_money = Some(CalcMoney::new(
        Dec::new(Decimal::new(accrued, currency.minor_units())),
        currency,
    ));
    match check_order_completeness(&grosses, accrued_money, payment_minor, currency) {
        Ok(None) => None,
        Ok(Some(mismatch)) => Some(format!(
            "order payment {} does not equal trade total {}",
            mismatch.reported.raw(),
            mismatch.expected.raw()
        )),
        Err(_) => Some("trade money total is not representable".to_owned()),
    }
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
    let custody = required_custody(&operation)?;
    let buy = matches!(kind, ChannelOperationKind::Buy);
    let instrument = required_instrument(&operation)?;
    let mut trades = operation.trades;
    trades.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.num.as_bytes().cmp(right.num.as_bytes()))
    });
    let currency = trades[0].price.currency();
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
        .map(|((trade, commission), accrued)| {
            let gross_exact = iaam_core::money::gross_for_fill(trade.price, trade.quantity)
                .map_err(|error| {
                    row_unparsable(format!("trade {} gross overflow: {error}", trade.num))
                })?;
            let gross = CalcMoney::new(
                Dec::new(gross_exact.value().inner().abs()),
                trade.price.currency(),
            );
            let gross_minor = gross
                .rounded_minor()
                .map_err(|error| {
                    row_unparsable(format!(
                        "trade {} gross cannot be rounded: {error}",
                        trade.num
                    ))
                })?
                .raw();
            let basis_fee = calc_money_from_minor(commission, currency);
            let accrued_interest_minor = accrued_known.then_some(accrued);
            let operation_kind = if buy {
                OperationKind::Buy {
                    instrument,
                    custody,
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
                    custody,
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
            })
        })
        .collect()
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

fn required_custody(operation: &ChannelOperation) -> Result<CustodyId, RowRefusal> {
    let value = operation
        .position_uid
        .as_deref()
        .ok_or_else(|| row_unparsable("trading operation does not contain positionUid"))?;
    Uuid::parse_str(value)
        .map(CustodyId)
        .map_err(|_| row_unparsable(format!("positionUid is not a UUID: {value}")))
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

fn tinkoff_error(error: TinkoffError) -> BrokerError {
    let detail = error.to_string();
    match error {
        TinkoffError::Network | TinkoffError::RateLimited | TinkoffError::Transport(_) => {
            BrokerError::Unreachable {
                broker: BROKER.to_owned(),
                detail,
            }
        }
        TinkoffError::InvalidToken
        | TinkoffError::MethodUnavailable { .. }
        | TinkoffError::UnexpectedStatus { .. } => BrokerError::Refused {
            broker: BROKER.to_owned(),
            detail,
        },
        TinkoffError::PartialResponse
        | TinkoffError::MalformedResponse
        | TinkoffError::RequestSerialization => unparsable(detail),
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

#[cfg(test)]
mod tests {
    use iaam_broker::tinkoff::{
        ChannelOrderState, ParseError, parse_operations as parse_operations_page, parse_portfolio,
    };
    use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
    use iaam_core::event::kind::{EventKind, IncomeKind};
    use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
    use iaam_core::projection::{ProjectionContext, project};
    use iaam_core::reconciliation::{Dimension, claim::ControlClaim};
    use iaam_core::rules::{LotRuleVersion, RuleRegistry};

    fn parse_operations(
        body: &str,
    ) -> Result<Vec<iaam_broker::tinkoff::ChannelOperation>, ParseError> {
        parse_operations_page(body).map(|page| page.operations)
    }
    use iaam_ingest::operation::{NormalizationContext, OperationKind, normalize};
    use uuid::Uuid;

    use super::{
        BrokerError, GetOperationsByCursorRequest, PortfolioAsOf, Quarantined, RowRefusal,
        adapt_operations, adapt_portfolio, fetch_operation_pages, operation_to_submitted,
        order_state_reason, rfc3339_operation_end, trade_operations,
    };
    use iaam_broker::operation_kind::OperationKindDictionary;

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

    /// Amortisation and redemption are corporate actions, and the channel
    /// does not provide the data they require. The rejection must identify WHAT IS MISSING:
    /// «unsupported kind» would send the owner looking for support
    /// for a kind that is supported, rather than for the data that is missing.
    #[test]
    fn a_bond_repayment_is_refused_by_naming_what_the_channel_does_not_report() {
        use iaam_broker::operation_kind::ChannelOperationKind;
        use iaam_broker::tinkoff::ChannelOperation;

        for kind in [
            ChannelOperationKind::BondAmortisation,
            ChannelOperationKind::BondRedemption,
        ] {
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
                    _ => "unsupported operation kind: OPERATION_TYPE_UNKNOWN",
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
    fn a_trade_uses_the_recorded_position_uid_as_custody() {
        let operations = parse_operations(include_str!(
            "../../../../tests/fixtures/api/tinkoff-operations.json"
        ))
        .expect("parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");
        let expected = CustodyId(
            Uuid::parse_str("f1a60ae6-3f1e-43c8-8d46-042df0fdc97a").expect("position UID"),
        );

        assert!(parsed.accepted.iter().any(|operation| {
            matches!(
                operation.kind,
                OperationKind::Buy { custody, .. } if custody == expected
            )
        }));
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
    fn a_securities_transfer_out_is_quarantined_as_an_outbound_securities_transfer() {
        let operations = parse_operations(&income_operation("OPERATION_TYPE_OUTPUT_SECURITIES"))
            .expect("parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");

        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.quarantined.len(), 1);
        assert!(parsed.quarantined[0].reason.contains("securities"));
        assert!(parsed.quarantined[0].reason.contains("outbound"));
        assert_eq!(
            parsed.quarantined[0].dimensions,
            [Dimension::Positions, Dimension::TaxBasis]
                .into_iter()
                .collect()
        );
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
            };
            let events = buy
                .iter()
                .chain(sale.iter())
                .enumerate()
                .map(|(index, operation)| {
                    let mut event = normalize(operation, context).expect("normalization").event;
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
    fn a_trade_and_portfolio_claim_for_sber_share_the_same_custody() {
        let operations = parse_operations(include_str!(
            "../../../../tests/fixtures/api/tinkoff-operations.json"
        ))
        .expect("operations parsing");
        let parsed =
            adapt_operations(trading_account(), operations, &dictionary()).expect("adaptation");
        let instrument = InstrumentId(
            Uuid::parse_str("1c004240-d18d-46e1-8ac1-2aa05ebfdb38").expect("instrument UID"),
        );
        let trade_custody = parsed
            .accepted
            .iter()
            .find_map(|operation| match &operation.kind {
                OperationKind::Buy {
                    instrument: found,
                    custody,
                    ..
                } if *found == instrument => Some(*custody),
                _ => None,
            });
        let claim_custody = parse_portfolio(include_str!(
            "../../../../tests/fixtures/api/tinkoff-portfolio.json"
        ))
        .expect("portfolio parsing")
        .into_iter()
        .find_map(|claim| match claim {
            ControlClaim::PositionQuantity {
                instrument: found,
                custody,
                ..
            } if found == instrument => Some(custody),
            _ => None,
        });

        assert_eq!(trade_custody, claim_custody);
    }

    #[test]
    fn t_invest_portfolio_answers_with_current_date_semantics() {
        let snapshot = adapt_portfolio(include_str!(
            "../../../../tests/fixtures/api/tinkoff-portfolio.json"
        ))
        .expect("portfolio adaptation");

        assert_eq!(snapshot.as_of, PortfolioAsOf::Current);
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
            NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
            },
        )
        .expect("trade normalization")
        .event;
        let observed =
            iaam_core::reconciliation::observed::observe(&[event], trading_account(), period)
                .expect("observation");

        assert_eq!(
            iaam_core::reconciliation::check::check_claim(&claim, &observed),
            iaam_core::reconciliation::check::ClaimOutcome::Matched
        );

        let mut old_operation = operation;
        if let OperationKind::Buy { custody, .. } = &mut old_operation.kind {
            *custody = CustodyId(trading_account().inner());
        } else {
            panic!("expected buy");
        }
        let old_event = normalize(
            &old_operation,
            NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
            },
        )
        .expect("old trade normalization")
        .event;
        let old_observed =
            iaam_core::reconciliation::observed::observe(&[old_event], trading_account(), period)
                .expect("old observation");
        assert!(matches!(
            iaam_core::reconciliation::check::check_claim(&claim, &old_observed),
            iaam_core::reconciliation::check::ClaimOutcome::Discrepant(_)
        ));
    }

    #[test]
    fn mismatched_order_money_quarantines_with_both_amounts() {
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

        assert!(accepted.is_empty());
        assert_eq!(quarantined.len(), 1);
        assert!(quarantined[0].reason.contains("25"));
        assert!(quarantined[0].reason.contains("24"));
    }

    #[test]
    fn accrued_interest_is_part_of_order_completeness() {
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
        assert!(accepted.is_empty());
        assert_eq!(quarantined.len(), 1);
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
                NormalizationContext {
                    owner: OwnerId::new_random(),
                    source: SourceId::new_random(),
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
            NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
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
    async fn refuses_after_one_hundred_pages_only_when_the_next_page_is_needed() {
        let request = GetOperationsByCursorRequest::new("account");
        let mut responses = (1..=100)
            .map(|number| page_json(true, Some(&format!("cursor-{number}")), None))
            .collect::<Vec<_>>();
        let mut requests = Vec::new();
        let error = fetch_operation_pages(request, |request| {
            requests.push(request);
            let body = responses.remove(0);
            async move { Ok(body) }
        })
        .await
        .expect_err("the hundredth page still requests a continuation");

        assert!(error.to_string().contains("100"));
        assert_eq!(requests.len(), 100);

        let request = GetOperationsByCursorRequest::new("account");
        let mut responses = (1..=100)
            .map(|number| {
                page_json(
                    number != 100,
                    (number != 100)
                        .then(|| format!("cursor-{number}"))
                        .as_deref(),
                    Some(&format!("operation-{number}")),
                )
            })
            .collect::<Vec<_>>();
        let mut requests = Vec::new();
        let operations = fetch_operation_pages(request, |request| {
            requests.push(request);
            let body = responses.remove(0);
            async move { Ok(body) }
        })
        .await
        .expect("the hundredth page is accepted when complete");

        assert_eq!(operations.len(), 100);
        assert_eq!(requests.len(), 100);
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
}
