//! A `BrokerChannel` port over the Finam Trade API channel.
//!
//! Response parsing stays in `iaam-broker`; this layer only requests the
//! body, quarantines rejected rows, and binds stable port types — the same
//! division as the T-Invest channel (`adapters/tinkoff.rs`), which is this
//! module's model.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use async_trait::async_trait;

use iaam_broker::finam::{
    ChannelMoney, ChannelOperation, ChannelOperationKind, FINAM_PARSER_VERSION, FinamClient,
    FinamError, ParseError, parse_asset, parse_operations, parse_portfolio,
};
use iaam_broker::operation_kind::OperationKindDictionary;
use iaam_core::event::kind::{FeeOrigin, IncomeKind};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, InstrumentId, SourceId};
use iaam_core::instrument::AliasNamespace;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_ingest::SubmittedOperation;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use time::Date;

use crate::error::AppError;
use crate::ports::{
    BrokerChannel, BrokerError, BrokerRequestContext, InstrumentDirectory, ParsedOperations,
    PortfolioAsOf, PortfolioSnapshot, Quarantined,
};

const BROKER: &str = "finam";

/// Broker channel implementation for Finam.
pub struct FinamChannel {
    client: FinamClient,
    source: SourceId,
    /// Dictionary of operation kinds for this channel. It arrives from storage
    /// ready to use: parsing in `iaam-broker` knows nothing about storage, and
    /// this adapter binds them — using the same approach already used
    /// for SQLite.
    dictionary: OperationKindDictionary,
    /// The instrument directory the symbols resolve against: the one lookup
    /// by external code the system already has (§4.7), asked by ISIN — never
    /// a registry of this channel's own.
    instruments: Arc<dyn InstrumentDirectory>,
    /// The asset answers already paid for: symbol → the ISIN Finam's
    /// description named, `None` when it named none. The channel is opened
    /// per synchronisation and dropped after it, so this map is the sync's
    /// scope: every distinct symbol of one sync costs one asset read, no
    /// matter how many rows name it or which fetch asked first. A channel
    /// reused past one sync keeps serving the answers it holds — they name
    /// instruments, and the instrument an ISIN names is asked per date; the
    /// response cache beside the database already bounds the wire reads of
    /// one asset answer to the hour.
    isins: Mutex<HashMap<String, Option<String>>>,
}

impl FinamChannel {
    /// Creates a channel with a preconfigured HTTP client, data source,
    /// instrument directory and operation kind dictionary.
    #[must_use]
    pub fn new(
        client: FinamClient,
        source: SourceId,
        dictionary: OperationKindDictionary,
        instruments: Arc<dyn InstrumentDirectory>,
    ) -> Self {
        Self {
            client,
            source,
            dictionary,
            instruments,
            isins: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl BrokerChannel for FinamChannel {
    async fn fetch_account_numbers(
        &self,
        context: BrokerRequestContext<'_>,
    ) -> Result<Vec<String>, BrokerError> {
        let BrokerRequestContext {
            deadline,
            allowance,
        } = context;
        bounded(
            deadline,
            "the Finam sessions request",
            self.client.get_account_ids(allowance),
        )
        .await
    }

    async fn fetch_operations(
        &self,
        account: AccountId,
        broker_account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, BrokerError> {
        let BrokerRequestContext {
            deadline,
            allowance,
        } = context;
        // The broker is asked for its own account number; the returned rows
        // are stamped with the owner's account in this system.
        let body = bounded(
            deadline,
            "the Finam transactions request",
            self.client
                .get_transactions(broker_account, from, to, allowance),
        )
        .await?;
        let operations = parse_operations(&body).map_err(parse_error)?;
        let symbols = Symbols {
            channel: self,
            context,
        };
        adapt_operations(account, operations, &self.dictionary, &symbols).await
    }

    async fn fetch_portfolio(
        &self,
        account: AccountId,
        broker_account: &str,
        at: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        let BrokerRequestContext {
            deadline,
            allowance,
        } = context;
        let _ = account;
        let body = bounded(
            deadline,
            "the Finam portfolio request",
            self.client.get_portfolio(broker_account, allowance),
        )
        .await?;
        adapt_portfolio(
            &body,
            at,
            &Symbols {
                channel: self,
                context,
            },
        )
        .await
    }

    fn channel(&self) -> SourceChannel {
        SourceChannel {
            source: self.source,
            parser_version: ParserVersion(FINAM_PARSER_VERSION.to_owned()),
            document: None,
        }
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Account
    }
}

/// Runs one client call under the sync's deadline.
///
/// The client's methods carry no deadline parameter — the `FinamClient`
/// signatures are frozen while their authentication changes — so the bound
/// is taken here, twice: a call whose deadline has already passed never
/// starts, and one still running is dropped the moment the deadline fires.
/// Dropping the future cancels the in-flight gateway call, so no wait for
/// Finam outlives the sync. Both cases answer `Unreachable`: the sync ran
/// out of its time, and asking again later is exactly the advice.
async fn bounded<T>(
    deadline: Option<Instant>,
    request: &'static str,
    call: impl Future<Output = Result<T, FinamError>>,
) -> Result<T, BrokerError> {
    let call = async { call.await.map_err(finam_error) };
    let Some(at) = deadline else {
        return call.await;
    };
    if Instant::now() >= at {
        return Err(deadline_refusal(request));
    }
    match tokio::time::timeout_at(tokio::time::Instant::from(at), call).await {
        Ok(result) => result,
        Err(_elapsed) => Err(deadline_refusal(request)),
    }
}

fn deadline_refusal(request: &'static str) -> BrokerError {
    BrokerError::Unreachable {
        broker: BROKER.to_owned(),
        detail: format!("{request} did not finish within the sync's deadline"),
        retry_after: None,
    }
}

/// The symbol resolution of one fetch: the channel's asset reads and
/// instrument directory, carried together through the row conversions.
struct Symbols<'a> {
    channel: &'a FinamChannel,
    context: BrokerRequestContext<'a>,
}

/// What one symbol became: an instrument, or the row's named refusal.
enum Resolution {
    Resolved(InstrumentId),
    Unresolved(String),
}

impl Symbols<'_> {
    /// The instrument an operation row's symbol names, resolved on the
    /// operation's own date. A row without a date has no day to resolve
    /// the ISIN on and is refused with that named.
    async fn required(&self, operation: &ChannelOperation) -> Result<InstrumentId, RowRefusal> {
        let symbol = operation
            .symbol
            .as_deref()
            .ok_or_else(|| row_unparsable("operation does not contain symbol"))?;
        let Some(on) = operation.date else {
            return Err(row_unparsable(
                "operation carries a symbol but no date to resolve it on",
            ));
        };
        match self.instrument(symbol, on).await {
            Ok(Resolution::Resolved(instrument)) => Ok(instrument),
            Ok(Resolution::Unresolved(reason)) => Err(row_unparsable(reason)),
            // An asset read that cannot name the symbol's ISIN is that row's
            // problem, not the page's: the symbol is invalid in Finam's own
            // path-segment rules, or the answer that names the ISIN cannot
            // be read. The row is set aside with what is missing and who
            // supplies it named; the rest of the page still imports.
            Err(BrokerError::Unparsable { detail, .. }) => Err(row_unparsable(format!(
                "symbol {symbol} is not resolved to an instrument: Finam's asset \
                     description (GET /v1/assets/{symbol}) that names its ISIN cannot \
                     be read — {detail} — record the instrument with the ISIN it \
                     actually carries once the answer can be read, then sync this \
                     range again"
            ))),
            // A genuine source-level failure of the asset read — the source
            // is down, said no, or spent the sync's request budget — is the
            // source's failure, not one row's: the sync stops with the
            // original error's own classification.
            Err(
                error @ (BrokerError::Unreachable { .. }
                | BrokerError::Refused { .. }
                | BrokerError::RequestCeiling { .. }
                | BrokerError::Adapter { .. }
                | BrokerError::NoAccess { .. }
                | BrokerError::ScopeNotReadOnly { .. }),
            ) => Err(RowRefusal::Fatal(error)),
        }
    }

    /// The same, where the row's instrument may legitimately be absent:
    /// a symbol the channel names resolves, an absent one stays absent.
    async fn optional(
        &self,
        operation: &ChannelOperation,
    ) -> Result<Option<InstrumentId>, RowRefusal> {
        match operation.symbol.as_deref() {
            None => Ok(None),
            Some(_) => Ok(Some(self.required(operation).await?)),
        }
    }

    /// The instrument one symbol names as of one date: the asset read
    /// names the ISIN, the directory names the instrument. A symbol whose
    /// description names no ISIN, and an ISIN no iaam instrument carries,
    /// are the standing refusals; a directory that cannot be asked at all
    /// fails the sync.
    async fn instrument(&self, symbol: &str, on: Date) -> Result<Resolution, BrokerError> {
        let isin = self.channel.isin_of(symbol, self.context).await?;
        let Some(isin) = isin else {
            return Ok(Resolution::Unresolved(unresolved_symbol_reason(
                symbol, None,
            )));
        };
        match self
            .channel
            .instruments
            .resolve(AliasNamespace::Isin.code(), &isin, on)
            .await
        {
            Ok(instrument) => Ok(Resolution::Resolved(instrument)),
            Err(AppError::NotFound { .. }) => Ok(Resolution::Unresolved(unresolved_symbol_reason(
                symbol,
                Some(&isin),
            ))),
            Err(error) => Err(BrokerError::Adapter {
                broker: BROKER.to_owned(),
                detail: format!("the instrument directory refused ISIN {isin} on {on}: {error}"),
            }),
        }
    }
}

/// The refusal a row carries when its symbol does not become an iaam
/// instrument: what is missing, what the asset description named, and who
/// supplies the missing instrument — the owner records the instrument
/// carrying that ISIN, and the same range syncs again.
fn unresolved_symbol_reason(symbol: &str, isin: Option<&str>) -> String {
    match isin {
        None => format!(
            "symbol {symbol} is not resolved to an instrument: Finam's asset \
             description (GET /v1/assets/{symbol}) names no ISIN — record the \
             instrument with the ISIN it actually carries, then sync this range again"
        ),
        Some(isin) => format!(
            "symbol {symbol} is not resolved to an instrument: it names ISIN {isin} \
             and no iaam instrument carries that ISIN — record the instrument \
             carrying ISIN {isin}, then sync this range again"
        ),
    }
}

impl FinamChannel {
    /// The ISIN Finam's asset description names for the symbol, one asset
    /// read per distinct symbol per sync: the first ask reads the wire and
    /// remembers, every later ask of the same symbol reuses the answer.
    /// The lock guards reads and stores only; the read itself is a network
    /// call and never holds it.
    async fn isin_of(
        &self,
        symbol: &str,
        context: BrokerRequestContext<'_>,
    ) -> Result<Option<String>, BrokerError> {
        if let Some(isin) = self.cached_isin(symbol) {
            return Ok(isin);
        }
        let body = bounded(
            context.deadline,
            "the Finam asset request",
            self.client.get_asset(symbol, context.allowance),
        )
        .await?;
        let isin = parse_asset(&body).map_err(parse_error)?;
        self.cache_isin(symbol, isin.clone());
        Ok(isin)
    }

    /// The remembered ISIN, taken even through a poison: every writer
    /// stores one whole answer, so the lock is recovered from rather than
    /// panicked on.
    fn cached_isin(&self, symbol: &str) -> Option<Option<String>> {
        self.isins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(symbol)
            .cloned()
    }

    fn cache_isin(&self, symbol: &str, isin: Option<String>) {
        self.isins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(symbol.to_owned(), isin);
    }
}

/// Turns the channel's operations into journal submissions and quarantine.
///
/// The dictionary decides what each row's code means. A row the parser
/// rejected, a code the dictionary does not know, and a row whose facts do
/// not assemble are refused with their reason beside the original JSON:
/// dropped and guessed are not options.
async fn adapt_operations(
    account: AccountId,
    operations: Vec<ChannelOperation>,
    dictionary: &OperationKindDictionary,
    symbols: &Symbols<'_>,
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
        let dimensions = dimensions_for_kind(&kind);
        let converted = if matches!(kind, ChannelOperationKind::Buy | ChannelOperationKind::Sell) {
            trade_operation(account, operation, kind, symbols).await
        } else {
            operation_to_submitted(account, operation, kind, symbols).await
        };
        match converted.map_err(|error| error.with_dimensions(dimensions)) {
            Ok(operation) => accepted.push(operation),
            Err(RowRefusal::Row { reason, dimensions }) => {
                quarantined.push(Quarantined {
                    raw,
                    reason,
                    dimensions,
                });
            }
            // A row was only ever refused by the arm above; everything
            // left here is a whole-sync failure — the original source
            // error, or a bug this adapter's branching excluded.
            Err(error) => {
                return Err(error
                    .into_original()
                    .expect("a refused row is set aside by the arm above, never a stop"));
            }
        }
    }
    Ok(ParsedOperations {
        accepted,
        quarantined,
    })
}

/// Turns the channel's portfolio answer into a snapshot.
///
/// Finam's account answer is its present holdings: the snapshot is recorded
/// as a current fact, whatever date was asked. Every position row the parser
/// kept whole has its symbol resolved here — the asset description names the
/// ISIN, the directory names the instrument — and a row the resolution does
/// not reach travels in `refused` with its reason and its original JSON, the
/// way a quarantined operation does: one instrument's unresolved row
/// withholds an opinion about that instrument, not about the rest of the
/// holdings (`iaam-vg8te.1.2`). Nor does one row's own unreadable asset
/// answer stop the sync: an invalid symbol or an answer that cannot be read
/// sets that row aside with its reason and payload, and the other holdings
/// still become claims. Only failures over the whole answer — an unparsable
/// body, a source down or refusing, a directory that cannot be asked — fail
/// the sync, each keeping its own classification (`iaam-vg8te.1.3`).
async fn adapt_portfolio(
    body: &str,
    at: Date,
    symbols: &Symbols<'_>,
) -> Result<PortfolioSnapshot, BrokerError> {
    let parsed = parse_portfolio(body).map_err(parse_error)?;
    let mut claims = parsed.claims;
    let mut refused: Vec<Quarantined> = parsed
        .refused
        .into_iter()
        .map(|row| Quarantined {
            raw: row.raw,
            // The same refusal shape the operations path prints: the
            // named variant for the machine, the sentence for the owner.
            reason: format!("{:?}: {}", row.reason, row.reason),
            dimensions: [Dimension::Positions].into_iter().collect(),
        })
        .collect();
    for row in parsed.unresolved {
        // A row whose symbol cannot become an instrument is one position's
        // problem, not the whole holdings': what cannot be read is that
        // row's own asset answer, and one instrument's unresolved row
        // withholds an opinion about that instrument rather than about the
        // rest of the answer. Only failures over the whole answer — an
        // unparsable body, a source down or refusing, a directory that
        // cannot be asked — stop the sync, and each keeps its own
        // classification.
        match symbols.instrument(&row.symbol, at).await {
            Ok(Resolution::Resolved(instrument)) => {
                claims.push(ControlClaim::PositionQuantity {
                    instrument,
                    quantity: row.quantity,
                    at: BalancePoint::Closing,
                });
            }
            Ok(Resolution::Unresolved(reason)) => refused.push(Quarantined {
                raw: row.raw,
                reason,
                dimensions: [Dimension::Positions].into_iter().collect(),
            }),
            // The symbol's own asset answer cannot be read — the symbol is
            // invalid in Finam's path-segment rules, or the description
            // that names the ISIN is unparsable: the row is set aside with
            // its reason and its original JSON, and the rest of the answer
            // still becomes claims.
            Err(BrokerError::Unparsable { detail, .. }) => {
                let reason = format!(
                    "position {} is not resolved to an instrument: Finam's asset \
                     description (GET /v1/assets/{}) that names its ISIN cannot \
                     be read — {detail} — record the instrument with the ISIN it \
                     actually carries once the answer can be read, then sync this \
                     range again",
                    row.symbol, row.symbol
                );
                refused.push(Quarantined {
                    raw: row.raw,
                    reason,
                    dimensions: [Dimension::Positions].into_iter().collect(),
                });
            }
            Err(
                error @ (BrokerError::Unreachable { .. }
                | BrokerError::Refused { .. }
                | BrokerError::RequestCeiling { .. }
                | BrokerError::Adapter { .. }
                | BrokerError::NoAccess { .. }
                | BrokerError::ScopeNotReadOnly { .. }),
            ) => return Err(error),
        }
    }
    Ok(PortfolioSnapshot {
        as_of: PortfolioAsOf::Current,
        claims,
        refused,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RowRefusal {
    /// A property of the row: it was read, and no fact can be built from it.
    Row {
        reason: String,
        dimensions: BTreeSet<Dimension>,
    },
    /// A genuine source-level failure behind one row's resolution — the
    /// source was down, said no, or spent the request budget — carries the
    /// original error whole, so the sync stops with the error's own
    /// classification and never with ours.
    Fatal(BrokerError),
    /// The adapter reached a state its own branching should have excluded.
    Adapter(String),
}

impl RowRefusal {
    fn with_dimensions(self, dimensions: BTreeSet<Dimension>) -> Self {
        match self {
            Self::Row { reason, .. } => Self::Row { reason, dimensions },
            Self::Fatal(error) => Self::Fatal(error),
            Self::Adapter(detail) => Self::Adapter(detail),
        }
    }

    /// The broker error this refusal is, when the sync is to stop for it:
    /// the fatal arm carries the original error with its classification
    /// whole, and a genuine adapter bug becomes the adapter error that is
    /// our side's failure. A refused row is set aside by the caller, never
    /// a stop, so its reason comes back on the `Err` side.
    fn into_original(self) -> Result<BrokerError, String> {
        match self {
            Self::Fatal(error) => Ok(error),
            Self::Adapter(detail) => Ok(BrokerError::Adapter {
                broker: BROKER.to_owned(),
                detail,
            }),
            Self::Row { reason, .. } => Err(reason),
        }
    }
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

fn finam_error(error: FinamError) -> BrokerError {
    let detail = error.to_string();
    match error {
        FinamError::Unavailable { retry_after, .. } => BrokerError::Unreachable {
            broker: BROKER.to_owned(),
            detail,
            retry_after: Some(retry_after),
        },
        FinamError::EgressRefused {
            retry_after,
            reason: _,
        } => BrokerError::Unreachable {
            broker: BROKER.to_owned(),
            detail,
            retry_after,
        },
        FinamError::RequestCeiling { ceiling } => BrokerError::RequestCeiling {
            broker: BROKER.to_owned(),
            ceiling,
        },
        FinamError::InvalidToken | FinamError::UnexpectedStatus { .. } => BrokerError::Refused {
            broker: BROKER.to_owned(),
            detail,
        },
        FinamError::InvalidAccountId
        | FinamError::InvalidSymbol
        | FinamError::PartialResponse
        | FinamError::MalformedResponse => unparsable(detail),
        // A method key without a budget, or a transport this build could not
        // set up, is this build's fault, not Finam's: retrying later meets
        // the same fault.
        FinamError::Gateway { .. } | FinamError::TransportNotBuilt { .. } => BrokerError::Adapter {
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

/// One Finam trade row is one execution: quantity, price and the money
/// change name the same single fill, so no per-trade expansion or commission
/// allocation applies — unlike the T-Invest channel, whose order carries a
/// list of fills.
async fn trade_operation(
    account: AccountId,
    operation: ChannelOperation,
    kind: ChannelOperationKind,
    symbols: &Symbols<'_>,
) -> Result<SubmittedOperation, RowRefusal> {
    let buy = matches!(kind, ChannelOperationKind::Buy);
    let quantity = required_quantity(&operation)?;
    // A trade that carries accrued interest cannot be recorded: Finam's
    // published contract names `change_original` — the money change in
    // the instrument's currency — and `trade.accrued_interest` side by
    // side, and says of neither that it holds the other, while the
    // recorded settlement is built as the gross plus the accrued
    // interest. Keeping both would guess the equation between them, so
    // the trade waits in quarantine until the relationship is
    // established. A zero interest carries nothing to relate, and a trade
    // without the field never did.
    if let Some(accrued) = operation
        .accrued_interest
        .filter(|accrued| !accrued.is_zero())
    {
        return Err(row_unparsable(format!(
            "trade carries accrued interest {}: whether Finam's change_original already \
             contains it is an unestablished relationship, and recording both could count \
             it twice — establish the relationship between change_original and accrued \
             interest before importing this trade",
            accrued.inner()
        )));
    }
    // The gross is the money Finam itself states for the trade:
    // `change_original`, the change in the instrument's own currency.
    // Computing a gross from the price would have to assume the price's
    // basis (a bond's price is a percentage of its face value), its
    // currency and the face value — none of which the transactions answer
    // carries — so a trade whose money Finam does not state is refused,
    // never computed by assumption. `change` folds every instrument into
    // rubles and is not the trade's money in its own terms.
    let money = operation
        .change_original
        .ok_or_else(|| row_unparsable("trade does not contain change_original"))?;
    let currency = money.currency;
    let gross_minor = money_amount(money, "change_original")?;
    // The instrument resolves only after the row's own facts stand: a row
    // this channel could not record anyway spends no asset read.
    let instrument = symbols.required(&operation).await?;
    // The commission arrives as its own COMMISSION/FEE row and becomes a Fee;
    // a fee inside the trade as well would charge the account twice. The
    // interest-bearing trades were refused above, so the recorded fact
    // carries no accrued interest of its own.
    let operation_kind = if buy {
        OperationKind::Buy {
            instrument,
            // No custody here: the channel names no place of storage.
            custody: None,
            quantity,
            gross_minor,
            fee_minor: None,
            basis_fee: None,
            accrued_interest_minor: None,
            currency,
        }
    } else {
        OperationKind::Sell {
            instrument,
            custody: None,
            quantity,
            gross_minor,
            fee_minor: None,
            basis_fee: None,
            accrued_interest_minor: None,
            currency,
        }
    };
    Ok(SubmittedOperation {
        account,
        kind: operation_kind,
        dates: OperationDates {
            trade: operation.date,
            ..OperationDates::default()
        },
        // Finam reports a calendar date per transaction, no moment.
        source_time: None,
        idempotency_key: Some(operation.deduplication_key),
        source_operation_id: Some(operation.operation_id),
        source_position_id: None,
        owner_category: None,
        source_code: None,
        // Two distinct source words: the category is what the operation WAS
        // (upper case is this channel's own wire form), the transaction's
        // grouping is what it was FOR — the slot a category rule matches,
        // so a grouping Finam never printed must not appear there.
        source_kind: Some(operation.source_kind),
        source_category: operation.transaction_category,
        description: operation.transaction_name,
        counterparty: None,
    })
}

/// A non-trade row becomes exactly the fact its kind names, or a refusal
/// saying what is missing.
async fn operation_to_submitted(
    account: AccountId,
    operation: ChannelOperation,
    kind: ChannelOperationKind,
    symbols: &Symbols<'_>,
) -> Result<SubmittedOperation, RowRefusal> {
    let kind = match kind {
        ChannelOperationKind::Buy | ChannelOperationKind::Sell => {
            return Err(RowRefusal::Adapter(
                "trading operations are converted from their trade fields".to_owned(),
            ));
        }
        // A coupon and a dividend must not be collapsed into a single receipt: the
        // journal stores the kind, and losing it here means losing it forever —
        // the event is immutable.
        kind @ (ChannelOperationKind::Dividend | ChannelOperationKind::Coupon) => {
            let (gross_minor, currency) = required_money(operation.payment, "change")?;
            let income_kind = match kind {
                ChannelOperationKind::Coupon => IncomeKind::Coupon,
                ChannelOperationKind::Dividend => IncomeKind::Dividend,
                // The outer pattern has already narrowed the possibilities. This
                // branch is unreachable and must fail loudly, rather than
                // substitute a dividend.
                other => {
                    return Err(RowRefusal::Adapter(format!(
                        "income kind mismatch: {other:?}"
                    )));
                }
            };
            OperationKind::Income {
                instrument: symbols.optional(&operation).await?,
                gross_minor,
                currency,
                kind: Some(income_kind),
            }
        }
        ChannelOperationKind::Commission => {
            let (amount_minor, currency) = required_money(operation.payment, "change")?;
            OperationKind::Fee {
                amount_minor,
                currency,
                origin: FeeOrigin::Brokerage,
            }
        }
        ChannelOperationKind::Deposit => {
            let (amount_minor, currency) = required_money(operation.payment, "change")?;
            OperationKind::Deposit {
                amount_minor,
                currency,
            }
        }
        ChannelOperationKind::Withdrawal => {
            let (amount_minor, currency) = required_money(operation.payment, "change")?;
            OperationKind::Withdrawal {
                amount_minor,
                currency,
            }
        }
        ChannelOperationKind::SecuritiesTransferIn
        | ChannelOperationKind::SecuritiesTransferOut => {
            return Err(row_unparsable(
                "securities moved without a cash movement: the transfer is not a cash fact",
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
        // (POST /v1/ingest/journal-events). The channel reports the payment
        // amount, but not the returned face value per unit or the custody
        // location; without them the fact cannot be constructed, and
        // substituting a guess would record something that never happened in
        // the append-only journal.
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
        source_time: None,
        idempotency_key: Some(operation.deduplication_key),
        source_operation_id: Some(operation.operation_id),
        source_position_id: None,
        owner_category: None,
        source_code: None,
        // Two distinct source words: the category is what the operation WAS
        // (upper case is this channel's own wire form), the transaction's
        // grouping is what it was FOR — the slot a category rule matches,
        // so a grouping Finam never printed must not appear there.
        source_kind: Some(operation.source_kind),
        source_category: operation.transaction_category,
        description: operation.transaction_name,
        counterparty: None,
    })
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
        .amount
        .raw()
        .checked_abs()
        .ok_or_else(|| row_unparsable(format!("field {field} does not have a positive magnitude")))
}

/// The quantity as the fact carries it: positive, the side being the
/// variant. The quantity comes from the contract's `trade.size`; a sign is
/// not an opinion this channel can read, so the magnitude is kept and zero
/// is refused — nothing else about the direction is guessed.
fn required_quantity(operation: &ChannelOperation) -> Result<Dec, RowRefusal> {
    let quantity = operation
        .quantity
        .ok_or_else(|| row_unparsable("trade does not contain trade.size"))?;
    if quantity.0.inner().is_zero() {
        return Err(row_unparsable("trade quantity is zero"));
    }
    Ok(Dec::new(quantity.0.inner().abs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::tinkoff::fake::{self, Answer};
    use iaam_broker::credentials::BrokerToken;
    use iaam_core::reconciliation::claim::ControlClaim;
    use iaam_http::{BrokerEgress, Gateway, Outbound};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use time::macros::date;

    fn broker_context(deadline: Option<Instant>) -> BrokerRequestContext<'static> {
        static ALLOWANCE: std::sync::LazyLock<iaam_http::RequestAllowance> =
            std::sync::LazyLock::new(|| iaam_http::RequestAllowance::new(u32::MAX));
        BrokerRequestContext {
            deadline,
            allowance: &ALLOWANCE,
        }
    }

    const TOKEN: &str = "invented-finam-token";
    const DIVIDEND_ID: &str = "3f2b8c5e-1a4d-4f6b-9c2e-5a7d8e1f4a3b";
    const BUY_ID: &str = "8d1c2f3a-4b5e-4c6d-9a0b-1c2d3e4f5a6b";
    const FEE_ID: &str = "c2d3e4f5-a6b7-4c8d-9e0f-1a2b3c4d5e6f";
    const ZERO_ACCRUED_ID: &str = "b7e3c9a1-5f8d-4b2e-8a6c-9d0e1f2a3b4c";
    /// The fixture rows' symbol, in Finam's own `TICKER@MIC` shape; it
    /// resolves through the scripted asset description in every test that
    /// consumes it.
    const SYMBOL: &str = "FIXT@MISX";

    fn token() -> BrokerToken {
        let key = iaam_broker::credentials::Key::from_bytes([9; 32]);
        iaam_broker::credentials::open(&key, &iaam_broker::credentials::seal(&key, TOKEN))
            .expect("token round trip")
    }

    /// An invented instance database; the gateway's egress place is derived
    /// from it, created beside it, and initialized with the fixture tally.
    fn broker_egress_database() -> std::path::PathBuf {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "iaam-app-finam-test-{}-{sequence}",
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

    fn dictionary() -> OperationKindDictionary {
        let (dictionary, unreadable) = OperationKindDictionary::build(
            iaam_broker::finam::dictionary_seed::FINAM_OPERATION_KINDS
                .iter()
                .copied(),
        );
        assert!(unreadable.is_empty(), "{unreadable:?}");
        dictionary
    }

    /// The real instrument directory over an invented in-memory store:
    /// the symbols resolve against the store's own lookup, never a fixture
    /// echo. Every ISIN given gets one invented instrument recorded under
    /// it, in order.
    async fn directory(isins: &[&str]) -> (Arc<dyn InstrumentDirectory>, Vec<InstrumentId>) {
        use crate::ports::{AliasUpsert, InstrumentUpsert};
        use iaam_core::instrument::AliasInterval;
        let adapter = crate::adapters::sqlite::SqliteAdapter::new(
            iaam_store::SqliteStore::open_in_memory().expect("in-memory store"),
        );
        let mut instruments = Vec::new();
        for isin in isins {
            let instrument = InstrumentId::new_random();
            adapter
                .record_instrument(InstrumentUpsert {
                    id: instrument,
                    kind: None,
                    symbol: "Fixture Share".to_owned(),
                    title: "Fixture Share".to_owned(),
                    currencies: iaam_core::instrument::CurrencyRoles {
                        denomination: CurrencyCode::Rub,
                        settlement: CurrencyCode::Rub,
                        quote: CurrencyCode::Rub,
                    },
                    lineage: None,
                })
                .await
                .expect("instrument recorded");
            adapter
                .record_alias(AliasUpsert {
                    namespace: AliasNamespace::Isin,
                    value: (*isin).to_owned(),
                    instrument,
                    interval: AliasInterval {
                        valid_from: date!(2000 - 01 - 01),
                        valid_to: None,
                    },
                    source: SourceId::new_random(),
                })
                .await
                .expect("alias recorded");
            instruments.push(instrument);
        }
        (Arc::new(adapter), instruments)
    }

    fn channel(
        gateway: Arc<dyn Outbound>,
        instruments: Arc<dyn InstrumentDirectory>,
    ) -> FinamChannel {
        FinamChannel::new(
            FinamClient::new(token(), gateway),
            SourceId::new_random(),
            dictionary(),
            instruments,
        )
    }

    fn account() -> AccountId {
        AccountId::new_random()
    }

    fn page(body: &str) -> Answer {
        Answer::status(200, body)
    }

    /// The session exchange's answer: Finam trades the access's secret for
    /// a session token (`POST /v1/sessions`, `{"token": "..."}`), which the
    /// first data call then carries as its bearer. The invented token never
    /// leaves this module. Every scripted data page is therefore preceded
    /// by one exchange answer.
    fn session_answer() -> Answer {
        Answer::status(200, r#"{"token":"invented-finam-session-token"}"#)
    }

    /// The ISIN the invented instrument carries, and the asset description
    /// that names it. One answer per distinct symbol serves a whole sync:
    /// a second wire read of the same symbol would exhaust the script.
    const ISIN: &str = "RU000AFIXTUR";

    fn asset_answer() -> Answer {
        Answer::status(200, r#"{"isin":"RU000AFIXTUR"}"#)
    }

    /// An invented June 2025 page: a dividend with an instrument, a purchase
    /// with the contract's own trade and money fields, a bare fee. No real
    /// account, instrument or amount.
    fn transactions_page() -> String {
        json!({
            "transactions": [
                {
                    "id": DIVIDEND_ID,
                    "timestamp": "2025-06-10T10:00:00Z",
                    "category": "DIVIDEND",
                    "symbol": SYMBOL,
                    "change": { "units": "12", "nanos": 500_000_000, "currencyCode": "rub" },
                    "transactionCategory": "ACCRUALS",
                    "transactionName": "Dividend",
                },
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "TRADE_BUY",
                    "symbol": SYMBOL,
                    "change": { "units": "-1005", "nanos": 0, "currencyCode": "rub" },
                    "changeOriginal": { "units": "-1005", "nanos": 0, "currencyCode": "rub" },
                    "trade": {
                        "size": { "value": "10" },
                        "price": { "value": "100.50" },
                    },
                    "transactionCategory": "TRADE",
                },
                {
                    "id": FEE_ID,
                    "timestamp": "2025-06-15T00:00:00Z",
                    "category": "FEE",
                    "change": { "units": "-299", "nanos": 0, "currencyCode": "rub" },
                },
            ],
        })
        .to_string()
    }

    /// The accounts listing is what a sync without a binding binds from, so
    /// the ids must be exactly what the broker named, in his order.
    #[tokio::test]
    async fn the_accounts_listing_becomes_the_ids_the_sync_binds() {
        let channel = channel(
            fake::gateway(
                vec![
                    session_answer(),
                    page(r#"{"account_ids":["invented-one","invented-two"]}"#),
                ],
                None,
            )
            .0,
            directory(&[]).await.0,
        );

        let ids = channel
            .fetch_account_numbers(broker_context(None))
            .await
            .expect("the listing parses");

        assert_eq!(ids, ["invented-one", "invented-two"]);
    }

    #[tokio::test]
    async fn a_finam_page_becomes_operations_through_the_channel_dictionary() {
        let (instruments, expected) = directory(&[ISIN]).await;
        let channel = channel(
            fake::gateway(
                vec![session_answer(), page(&transactions_page()), asset_answer()],
                None,
            )
            .0,
            instruments,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert!(
            parsed.quarantined.is_empty(),
            "every invented row is readable: {:?}",
            parsed.quarantined
        );
        assert_eq!(parsed.accepted.len(), 3, "{:?}", parsed.accepted);

        let OperationKind::Income {
            instrument,
            gross_minor,
            currency,
            kind,
        } = &parsed.accepted[0].kind
        else {
            panic!("first row is the dividend: {:?}", parsed.accepted[0].kind);
        };
        assert_eq!(*gross_minor, 1_250, "12.50 RUB in minor units");
        assert_eq!(
            parsed.accepted[0].dates.trade,
            Some(date!(2025 - 06 - 10)),
            "a non-trade row still carries the day Finam stamped on it"
        );
        assert_eq!(*currency, iaam_core::money::CurrencyCode::Rub);
        assert_eq!(*kind, Some(IncomeKind::Dividend));
        // The symbol resolved through the asset description to the ISIN,
        // and the ISIN to the instrument the directory holds.
        assert_eq!(instrument.as_ref(), expected.first());
        // The source's own words ride along verbatim: the category word as
        // what the operation was, the name as its description, and no
        // grouping Finam never printed.
        assert_eq!(parsed.accepted[0].source_kind.as_deref(), Some("DIVIDEND"));
        // The grouping the wire printed, kept for the category rules.
        assert_eq!(
            parsed.accepted[0].source_category.as_deref(),
            Some("ACCRUALS")
        );
        assert_eq!(parsed.accepted[0].description.as_deref(), Some("Dividend"));

        let OperationKind::Buy {
            quantity,
            gross_minor,
            currency,
            ..
        } = &parsed.accepted[1].kind
        else {
            panic!("second row is the purchase: {:?}", parsed.accepted[1].kind);
        };
        assert_eq!(quantity.inner().to_string(), "10");
        assert_eq!(
            *gross_minor, 100_500,
            "the money of change_original: 1_005.00 RUB"
        );
        assert_eq!(*currency, iaam_core::money::CurrencyCode::Rub);
        assert_eq!(parsed.accepted[1].dates.trade, Some(date!(2025 - 06 - 12)));
        assert_eq!(parsed.accepted[1].source_kind.as_deref(), Some("TRADE_BUY"));
        assert_eq!(parsed.accepted[1].source_category.as_deref(), Some("TRADE"));
        assert_eq!(
            parsed.accepted[1].source_operation_id.as_deref(),
            Some(BUY_ID)
        );

        let OperationKind::Fee {
            amount_minor,
            origin,
            ..
        } = &parsed.accepted[2].kind
        else {
            panic!("third row is the fee: {:?}", parsed.accepted[2].kind);
        };
        assert_eq!(*amount_minor, 29_900, "299.00 RUB in minor units");
        assert_eq!(*origin, FeeOrigin::Brokerage);
        // The fee row printed no grouping of its own: absence stays absent.
        assert_eq!(parsed.accepted[2].source_category, None);
        assert_eq!(parsed.accepted[2].source_kind.as_deref(), Some("FEE"));
    }

    #[tokio::test]
    async fn a_row_the_parser_cannot_read_is_quarantined_with_its_reason() {
        let body = json!({
            "transactions": [
                {
                    "timestamp": "2025-06-10T10:00:00Z",
                    "category": "DEPOSIT",
                    "change": { "units": "500", "nanos": 0, "currencyCode": "rub" },
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert!(parsed.accepted.is_empty(), "{:?}", parsed.accepted);
        assert_eq!(parsed.quarantined.len(), 1, "{:?}", parsed.quarantined);
        let refused = &parsed.quarantined[0];
        assert!(
            refused.reason.contains("id"),
            "the refusal names the field: {}",
            refused.reason
        );
        assert_eq!(refused.raw["category"], json!("DEPOSIT"), "{refused:?}");
    }

    #[tokio::test]
    async fn a_kind_absent_from_the_dictionary_is_quarantined_with_its_code() {
        let body = json!({
            "transactions": [
                {
                    "id": DIVIDEND_ID,
                    "timestamp": "2025-06-10T10:00:00Z",
                    "category": "MYSTERY",
                    "change": { "units": "1", "nanos": 0, "currencyCode": "usd" },
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert!(parsed.accepted.is_empty(), "{:?}", parsed.accepted);
        assert_eq!(parsed.quarantined.len(), 1, "{:?}", parsed.quarantined);
        let refused = &parsed.quarantined[0];
        assert_eq!(refused.reason, "unsupported operation kind: MYSTERY");
        assert_eq!(refused.raw["category"], json!("MYSTERY"), "{refused:?}");
        // Literals, not `all_dimensions()`: a mutated dimension set must fail
        // here, not agree with itself.
        assert_eq!(
            refused.dimensions,
            [
                Dimension::Cash,
                Dimension::Positions,
                Dimension::TaxBasis,
                Dimension::Income,
            ]
            .into_iter()
            .collect()
        );
    }

    /// A trade whose money Finam does not state has no gross this channel
    /// may record: `change` folds every instrument into rubles, and a gross
    /// computed from the price would have to assume the price basis and the
    /// currency. The row is refused with its reason, never computed.
    #[tokio::test]
    async fn a_trade_without_change_original_is_refused_not_computed() {
        let body = json!({
            "transactions": [
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "TRADE_BUY",
                    "symbol": SYMBOL,
                    "change": { "units": "-1005", "nanos": 0, "currencyCode": "rub" },
                    "trade": {
                        "size": { "value": "10" },
                        "price": { "value": "100.50" },
                    },
                },
                {
                    "id": FEE_ID,
                    "timestamp": "2025-06-15T00:00:00Z",
                    "category": "FEE",
                    "change": { "units": "-299", "nanos": 0, "currencyCode": "rub" },
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert_eq!(parsed.accepted.len(), 1, "{:?}", parsed.accepted);
        let OperationKind::Fee { amount_minor, .. } = &parsed.accepted[0].kind else {
            panic!(
                "the readable row still lands: {:?}",
                parsed.accepted[0].kind
            );
        };
        assert_eq!(*amount_minor, 29_900, "299.00 RUB in minor units");
        assert_eq!(parsed.quarantined.len(), 1, "{:?}", parsed.quarantined);
        let refused = &parsed.quarantined[0];
        assert!(
            refused.reason.contains("change_original"),
            "the refusal names the money Finam never stated: {}",
            refused.reason
        );
        assert_eq!(
            refused.dimensions,
            [Dimension::Cash, Dimension::Positions]
                .into_iter()
                .collect(),
            "{refused:?}"
        );
    }

    /// A bond's price is a percentage of its face value: multiplying it by
    /// the quantity would record a hundredth of the money that moved. The
    /// gross is the money Finam itself states in the instrument's currency.
    #[tokio::test]
    async fn a_bond_quoted_in_percent_of_face_records_the_money_finam_states() {
        let body = json!({
            "transactions": [
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "TRADE_BUY",
                    "symbol": SYMBOL,
                    "change": { "units": "-9850", "nanos": 0, "currencyCode": "rub" },
                    "changeOriginal": { "units": "-9850", "nanos": 0, "currencyCode": "rub" },
                    "trade": {
                        "size": { "value": "10" },
                        "price": { "value": "98.5" },
                    },
                    "transactionCategory": "TRADE",
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
        let OperationKind::Buy { gross_minor, .. } = &parsed.accepted[0].kind else {
            panic!("the bond purchase is a buy: {:?}", parsed.accepted[0].kind);
        };
        // 98.5% of the 1_000 face value, ten bonds: 9_850.00 RUB — not the
        // 985.00 a price-times-quantity computation would have assumed.
        assert_eq!(*gross_minor, 985_000);
    }

    /// A trade that carries accrued interest cannot be recorded: Finam's
    /// published contract names `change_original` — the money change in
    /// the instrument's currency — and `trade.accrued_interest` side by
    /// side, and says of neither that it holds the other, while the
    /// recorded settlement is built as the gross plus the accrued
    /// interest. Keeping both would guess the equation between them, so
    /// the trade waits in quarantine under a reason naming the
    /// unestablished relationship. A trade whose accrued interest is zero
    /// carries nothing to relate, and is recorded as now.
    #[tokio::test]
    async fn a_trade_with_accrued_interest_waits_until_the_relationship_is_established() {
        let body = json!({
            "transactions": [
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "TRADE_BUY",
                    "symbol": SYMBOL,
                    "change": { "units": "-9900", "nanos": 0, "currencyCode": "rub" },
                    "changeOriginal": { "units": "-9900", "nanos": 0, "currencyCode": "rub" },
                    "trade": {
                        "size": { "value": "10" },
                        "price": { "value": "98.5" },
                        "accruedInterest": { "value": "50" },
                    },
                    "transactionCategory": "TRADE",
                },
                {
                    "id": ZERO_ACCRUED_ID,
                    "timestamp": "2025-06-13T00:00:00Z",
                    "category": "TRADE_BUY",
                    "symbol": SYMBOL,
                    "change": { "units": "-9850", "nanos": 0, "currencyCode": "rub" },
                    "changeOriginal": { "units": "-9850", "nanos": 0, "currencyCode": "rub" },
                    "trade": {
                        "size": { "value": "10" },
                        "price": { "value": "98.5" },
                        "accruedInterest": { "value": "0" },
                    },
                    "transactionCategory": "TRADE",
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert_eq!(
            parsed.quarantined.len(),
            1,
            "only the interest-bearing trade waits: {:?}",
            parsed.quarantined
        );
        let refused = &parsed.quarantined[0];
        assert!(
            refused.reason.contains("change_original")
                && refused.reason.contains("accrued interest"),
            "the refusal names both sides of the relationship: {}",
            refused.reason
        );
        assert!(
            refused.reason.contains("unestablished"),
            "the refusal names the relationship as unestablished: {}",
            refused.reason
        );
        assert_eq!(
            refused.raw["trade"]["accruedInterest"],
            json!({ "value": "50" }),
            "{refused:?}"
        );
        assert_eq!(
            refused.dimensions,
            [Dimension::Cash, Dimension::Positions]
                .into_iter()
                .collect(),
            "{refused:?}"
        );

        assert_eq!(parsed.accepted.len(), 1, "{:?}", parsed.accepted);
        let OperationKind::Buy {
            gross_minor,
            accrued_interest_minor,
            ..
        } = &parsed.accepted[0].kind
        else {
            panic!(
                "the zero-interest trade is recorded as a buy: {:?}",
                parsed.accepted[0].kind
            );
        };
        assert_eq!(*gross_minor, 985_000, "9_850.00 RUB in minor units");
        assert_eq!(
            *accrued_interest_minor, None,
            "zero interest is no interest to record"
        );
    }

    /// A foreign-currency security's trade moves the instrument's own
    /// money: the ruble fold in `change` names neither the currency nor the
    /// amount the fact records.
    #[tokio::test]
    async fn a_foreign_currency_trade_records_the_instruments_own_money() {
        let body = json!({
            "transactions": [
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "TRADE_BUY",
                    "symbol": SYMBOL,
                    "change": { "units": "-95000", "nanos": 0, "currencyCode": "rub" },
                    "changeOriginal": { "units": "-1005", "nanos": 0, "currencyCode": "usd" },
                    "trade": {
                        "size": { "value": "10" },
                        "price": { "value": "100.50" },
                    },
                    "transactionCategory": "TRADE",
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
        let OperationKind::Buy {
            gross_minor,
            currency,
            ..
        } = &parsed.accepted[0].kind
        else {
            panic!("the purchase is a buy: {:?}", parsed.accepted[0].kind);
        };
        assert_eq!(*gross_minor, 100_500, "1_005.00 USD in minor units");
        assert_eq!(*currency, iaam_core::money::CurrencyCode::Usd);
    }

    /// Finam's own accounting for a sale decreases the position: `trade.size`
    /// arrives negative and `change` positive. The fact keeps positive
    /// quantities and amounts; the variant carries the side.
    #[tokio::test]
    async fn a_sale_is_recorded_positive_whatever_sign_finam_prints() {
        let body = json!({
            "transactions": [
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-18T00:00:00Z",
                    "category": "TRADE_SELL",
                    "symbol": SYMBOL,
                    "change": { "units": "1005", "nanos": 0, "currencyCode": "rub" },
                    "changeOriginal": { "units": "1005", "nanos": 0, "currencyCode": "rub" },
                    "trade": {
                        "size": { "value": "-10" },
                        "price": { "value": "100.50" },
                    },
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert!(
            parsed.quarantined.is_empty(),
            "a sale is an ordinary trade: {:?}",
            parsed.quarantined
        );
        assert_eq!(parsed.accepted.len(), 1, "{:?}", parsed.accepted);
        let OperationKind::Sell {
            quantity,
            gross_minor,
            ..
        } = &parsed.accepted[0].kind
        else {
            panic!("the sale is a sale: {:?}", parsed.accepted[0].kind);
        };
        assert_eq!(quantity.inner().to_string(), "10");
        assert_eq!(*gross_minor, 100_500, "10 x 100.50 RUB in minor units");
        assert_eq!(
            parsed.accepted[0].source_kind.as_deref(),
            Some("TRADE_SELL")
        );
        assert_eq!(parsed.accepted[0].source_category, None);
    }

    /// An empty page is an empty answer, not a misconfiguration: the
    /// empty-dictionary refusal names rows it cannot read, and there are
    /// none.
    #[tokio::test]
    async fn an_empty_page_over_an_empty_dictionary_is_an_empty_sync() {
        let body = json!({ "transactions": [] }).to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body)], None).0,
            directory(&[ISIN]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("an empty page is not refused");

        assert!(parsed.accepted.is_empty());
        assert!(parsed.quarantined.is_empty());
    }

    /// A securities transfer is refused by its own name, whatever the
    /// direction: securities moved without a cash movement, and the row says
    /// so rather than falling through to a generic refusal.
    #[tokio::test]
    async fn a_securities_transfer_is_refused_under_its_own_name() {
        let body = json!({
            "transactions": [
                {
                    "id": DIVIDEND_ID,
                    "timestamp": "2025-06-11T00:00:00Z",
                    "category": "SECURITIES_TRANSFER_IN",
                    "symbol": SYMBOL,
                    "changeQty": { "value": "3" },
                },
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "SECURITIES_TRANSFER_OUT",
                    "symbol": SYMBOL,
                    "changeQty": { "value": "-3" },
                },
            ],
        })
        .to_string();
        // The transfer vocabulary exists, but no Finam seed row names it: a
        // dictionary that maps the codes is what lets the channel refuse them
        // under their own names.
        let (dictionary, unreadable) = OperationKindDictionary::build([
            ("SECURITIES_TRANSFER_IN", "securities_transfer_in"),
            ("SECURITIES_TRANSFER_OUT", "securities_transfer_out"),
        ]);
        assert!(unreadable.is_empty(), "{unreadable:?}");
        let channel = FinamChannel::new(
            FinamClient::new(
                token(),
                fake::gateway(vec![session_answer(), page(&body)], None).0,
            ),
            SourceId::new_random(),
            dictionary,
            directory(&[]).await.0,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the page is parsed");

        assert!(parsed.accepted.is_empty(), "{:?}", parsed.accepted);
        assert_eq!(parsed.quarantined.len(), 2, "{:?}", parsed.quarantined);
        let reasons = parsed
            .quarantined
            .iter()
            .map(|refused| refused.reason.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            reasons,
            [
                "inbound securities transfer: securities moved without a cash movement",
                "outbound securities transfer: securities moved without a cash movement",
            ]
        );
        for refused in &parsed.quarantined {
            assert_eq!(
                refused.dimensions,
                [Dimension::Positions, Dimension::TaxBasis]
                    .into_iter()
                    .collect()
            );
        }
    }

    #[tokio::test]
    async fn the_channel_names_its_parser_version_and_account_scope() {
        let channel = channel(
            fake::gateway(Vec::new(), None).0,
            directory(&[ISIN]).await.0,
        );

        let provenance = channel.channel();
        assert_eq!(provenance.parser_version.0, FINAM_PARSER_VERSION);
        assert!(provenance.document.is_none());
        // Finam numbers its transactions inside one account, not across the
        // broker: the same account scope as the T-Invest channel's applies.
        assert_eq!(channel.identity_scope(), IdentityScope::Account);
    }

    #[tokio::test]
    async fn a_deadline_already_reached_starts_no_request() {
        let (gateway, log, _time) = fake::gateway(Vec::new(), Some(page("{}")));
        let channel = channel(gateway, directory(&[ISIN]).await.0);
        // The monotonic clock never runs backward, so any later read of
        // `now` — the pre-check in `bounded` reads it — is at or past this
        // instant. The deadline is reached without any arithmetic on it.
        let past = Instant::now();

        let error = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(Some(past)),
            )
            .await
            .expect_err("the deadline has passed");

        assert!(
            matches!(&error, BrokerError::Unreachable { broker, .. } if broker == BROKER),
            "{error}"
        );
        assert!(
            log.lock().expect("log").is_empty(),
            "no request may start past the deadline"
        );
    }

    /// A transport that never answers: the deadline, not the broker, must end
    /// the wait.
    struct Parked;

    impl iaam_http::gateway::Transport for Parked {
        async fn send(
            &self,
            _request: &iaam_http::HttpRequest,
        ) -> Result<iaam_http::HttpResponse, iaam_http::HttpError> {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_still_running_at_the_deadline_is_dropped() {
        let database = broker_egress_database();
        let gateway: Arc<dyn Outbound> = Arc::new(
            Gateway::new_for_database(Parked, BrokerEgress::On, &database)
                .expect("the budget table is valid"),
        );
        let channel = channel(gateway, directory(&[ISIN]).await.0);
        let deadline = Instant::now() + Duration::from_secs(15 * 60);

        let error = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                broker_context(Some(deadline)),
            )
            .await
            .expect_err("the deadline fires although Finam never answers");

        assert!(
            matches!(&error, BrokerError::Unreachable { broker, .. } if broker == BROKER),
            "{error}"
        );
    }

    #[tokio::test]
    async fn the_portfolio_answer_becomes_a_snapshot_of_claims() {
        let body = json!({
            "accountId": "d0c7aa0f-6f2e-4a5b-8c3d-9e0f1a2b3c4d",
            "cash": [ { "units": "15432", "nanos": 560_000_000, "currencyCode": "rub" } ],
            "positions": [ { "symbol": SYMBOL, "quantity": { "value": "7" } } ],
        })
        .to_string();
        let (instruments, expected) = directory(&[ISIN]).await;
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            instruments,
        );

        let snapshot = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the portfolio is parsed");

        // Finam's account answer describes its present holdings: the snapshot
        // is a current fact, whatever date was asked.
        assert_eq!(snapshot.as_of, PortfolioAsOf::Current);
        assert!(snapshot.refused.is_empty(), "{:?}", snapshot.refused);
        assert_eq!(snapshot.claims.len(), 2, "{:?}", snapshot.claims);
        assert!(snapshot.claims.iter().any(|claim| matches!(
            claim,
            ControlClaim::CashBalance {
                currency: iaam_core::money::CurrencyCode::Rub,
                amount,
                at: iaam_core::reconciliation::claim::BalancePoint::Closing,
            } if amount.raw() == 1_543_256
        )));
        assert!(snapshot.claims.iter().any(|claim| matches!(
            claim,
            ControlClaim::PositionQuantity { instrument, quantity, .. }
                if *instrument == expected[0]
                    && quantity.0.inner().to_string() == "7"
        )));
    }

    /// A symbol the asset description does not name an ISIN for stays set
    /// aside beside the claims: the reason names what is missing, what the
    /// asset read was, and who supplies the instrument (iaam-vg8te.1.2).
    #[tokio::test]
    async fn a_symbol_without_an_isin_is_set_aside_beside_the_claims() {
        let body = json!({
            "cash": [ { "units": "100", "nanos": 0, "currencyCode": "rub" } ],
            "positions": [
                { "symbol": "SBER@MISX", "quantity": { "value": "10" } }
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(
                vec![
                    session_answer(),
                    page(&body),
                    Answer::status(200, r#"{"name":"Fixture Share"}"#),
                ],
                None,
            )
            .0,
            directory(&[]).await.0,
        );

        let snapshot = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the portfolio is parsed");

        // The cash still becomes a claim; only the row whose symbol nothing
        // here can resolve is set aside, with its reason and its own JSON.
        assert_eq!(snapshot.claims.len(), 1, "{:?}", snapshot.claims);
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        let refused = &snapshot.refused[0];
        assert_eq!(refused.raw["symbol"], json!("SBER@MISX"), "{refused:?}");
        assert!(
            refused.reason.contains("not resolved to an instrument"),
            "{}",
            refused.reason
        );
        assert!(
            refused.reason.contains("names no ISIN"),
            "the refusal says what the asset read named: {}",
            refused.reason
        );
        assert_eq!(
            refused.dimensions,
            [Dimension::Positions].into_iter().collect(),
            "{refused:?}"
        );
    }

    /// An ISIN no iaam instrument carries is the second standing refusal:
    /// the reason names the ISIN so the owner can record the instrument
    /// carrying it and sync the same range again.
    #[tokio::test]
    async fn an_isin_no_instrument_carries_is_set_aside_with_the_isin_named() {
        let body = json!({
            "cash": [ { "units": "100", "nanos": 0, "currencyCode": "rub" } ],
            "positions": [
                { "symbol": "SBER@MISX", "quantity": { "value": "10" } }
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(vec![session_answer(), page(&body), asset_answer()], None).0,
            // The directory holds no instrument under the ISIN the asset
            // description names.
            directory(&[]).await.0,
        );

        let snapshot = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the portfolio is parsed");

        assert_eq!(snapshot.claims.len(), 1, "{:?}", snapshot.claims);
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        let refused = &snapshot.refused[0];
        assert_eq!(refused.raw["symbol"], json!("SBER@MISX"), "{refused:?}");
        assert!(
            refused.reason.contains(ISIN) && refused.reason.contains("record the instrument"),
            "{}",
            refused.reason
        );
    }

    /// One asset read per distinct symbol per sync: the operations fetch and
    /// the portfolio fetch of one channel share every answer, and a symbol
    /// read twice would exhaust the script and fail the test.
    #[tokio::test]
    async fn one_asset_read_per_distinct_symbol_serves_a_whole_sync() {
        const FIXT: &str = "FIXT@MISX";
        const OTHR: &str = "OTHR@MISX";
        const FIXT_ISIN: &str = "RU000AFIXTUR";
        const OTHR_ISIN: &str = "RU000BOTHER4";
        let operations = json!({
            "transactions": [
                {
                    "id": DIVIDEND_ID,
                    "timestamp": "2025-06-10T10:00:00Z",
                    "category": "DIVIDEND",
                    "symbol": FIXT,
                    "change": { "units": "12", "nanos": 500_000_000, "currencyCode": "rub" },
                },
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "TRADE_BUY",
                    "symbol": OTHR,
                    "change": { "units": "-1005", "nanos": 0, "currencyCode": "rub" },
                    "changeOriginal": { "units": "-1005", "nanos": 0, "currencyCode": "rub" },
                    "trade": { "size": { "value": "10" } },
                },
            ],
        })
        .to_string();
        let portfolio = json!({
            "positions": [
                { "symbol": FIXT, "quantity": { "value": "7" } },
                { "symbol": OTHR, "quantity": { "value": "3" } },
            ],
        })
        .to_string();
        let (instruments, expected) = directory(&[FIXT_ISIN, OTHR_ISIN]).await;
        let channel = channel(
            fake::gateway(
                vec![
                    session_answer(),
                    page(&operations),
                    Answer::status(200, format!(r#"{{"isin":"{FIXT_ISIN}"}}"#).as_str()),
                    Answer::status(200, format!(r#"{{"isin":"{OTHR_ISIN}"}}"#).as_str()),
                    page(&portfolio),
                ],
                None,
            )
            .0,
            instruments,
        );
        let context = broker_context(None);

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                context,
            )
            .await
            .expect("the operations are parsed");
        let snapshot = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                context,
            )
            .await
            .expect("the portfolio is parsed");

        assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
        assert!(snapshot.refused.is_empty(), "{:?}", snapshot.refused);
        // Both rows of both fetches resolved, each symbol at the cost of
        // exactly one asset read.
        assert!(matches!(
            &parsed.accepted[0].kind,
            OperationKind::Income { instrument: Some(id), .. } if *id == expected[0]
        ));
        assert!(matches!(
            &parsed.accepted[1].kind,
            OperationKind::Buy { instrument: id, .. } if *id == expected[1]
        ));
        // Both position claims stand, and each names its own instrument:
        // the fixture carries no cash, so the claims are exactly the two
        // positions, resolved to two distinct instruments.
        let mut instruments: Vec<_> = snapshot
            .claims
            .iter()
            .filter_map(|claim| match claim {
                ControlClaim::PositionQuantity { instrument, .. } => Some(*instrument),
                _ => None,
            })
            .collect();
        instruments.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        assert_eq!(instruments, wanted, "{:?}", snapshot.claims);
    }

    #[tokio::test]
    async fn an_invalid_finam_token_is_a_refusal_not_an_outage() {
        let channel = channel(
            fake::gateway(Vec::new(), Some(Answer::status(401, "unauthorized"))).0,
            directory(&[ISIN]).await.0,
        );

        let error = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect_err("a rejected token");

        assert!(
            matches!(&error, BrokerError::Refused { broker, .. } if broker == BROKER),
            "{error}"
        );
        assert!(!error.to_string().contains(TOKEN), "{error}");
    }

    /// One position whose symbol Finam will not accept as a path segment
    /// (it contains "/") is set aside, not an abort: the readable
    /// positions of the same answer still become claims, the operations of
    /// the same sync still import, and the bad row travels in `refused`
    /// with its reason and its own JSON (iaam-vg8te.1.3).
    #[tokio::test]
    async fn a_position_with_an_invalid_symbol_is_set_aside_not_an_abort() {
        const GOOD: &str = "GOOD@MISX";
        const BAD: &str = "BAD/ROW";
        const GOOD_ISIN: &str = "RU000AGOOD07";
        let operations = json!({
            "transactions": [
                {
                    "id": DIVIDEND_ID,
                    "timestamp": "2025-06-10T10:00:00Z",
                    "category": "DIVIDEND",
                    "symbol": GOOD,
                    "change": { "units": "12", "nanos": 500_000_000, "currencyCode": "rub" },
                },
            ],
        })
        .to_string();
        let portfolio = json!({
            "cash": [ { "units": "100", "nanos": 0, "currencyCode": "rub" } ],
            "positions": [
                { "symbol": GOOD, "quantity": { "value": "7" } },
                { "symbol": BAD, "quantity": { "value": "3" } },
            ],
        })
        .to_string();
        let (instruments, expected) = directory(&[GOOD_ISIN]).await;
        let channel = channel(
            fake::gateway(
                vec![
                    session_answer(),
                    page(&operations),
                    Answer::status(200, format!(r#"{{"isin":"{GOOD_ISIN}"}}"#).as_str()),
                    page(&portfolio),
                ],
                None,
            )
            .0,
            instruments,
        );
        let context = broker_context(None);

        // The operations of the same sync still import whole.
        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                context,
            )
            .await
            .expect("the operations are parsed");
        assert!(parsed.quarantined.is_empty(), "{:?}", parsed.quarantined);
        assert_eq!(parsed.accepted.len(), 1, "{:?}", parsed.accepted);
        assert!(matches!(
            &parsed.accepted[0].kind,
            OperationKind::Income { instrument: Some(id), .. } if *id == expected[0]
        ));

        // The portfolio no longer stops on the invalid row: the readable
        // position and the cash still become claims, the bad row is
        // refused with its reason and its own JSON.
        let snapshot = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                context,
            )
            .await
            .expect("the portfolio is parsed");
        assert_eq!(snapshot.claims.len(), 2, "{:?}", snapshot.claims);
        assert!(snapshot
            .claims
            .iter()
            .any(|claim| matches!(claim, ControlClaim::PositionQuantity { instrument, .. } if *instrument == expected[0])));
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        let refused = &snapshot.refused[0];
        assert_eq!(refused.raw["symbol"], json!(BAD), "{refused:?}");
        assert!(
            refused.reason.contains(BAD)
                && refused.reason.contains("not resolved to an instrument")
                && refused.reason.contains("record the instrument"),
            "{}",
            refused.reason
        );
        assert_eq!(
            refused.dimensions,
            [Dimension::Positions].into_iter().collect(),
            "{refused:?}"
        );
    }

    /// An operation whose symbol Finam will not accept as a path segment is
    /// quarantined beside the readable rows: one row's invalid symbol never
    /// stops the page (iaam-vg8te.1.3).
    #[tokio::test]
    async fn an_operation_with_an_invalid_symbol_is_quarantined_not_an_abort() {
        const GOOD: &str = "GOOD@MISX";
        const BAD: &str = "BAD/ROW";
        const GOOD_ISIN: &str = "RU000AGOOD07";
        let operations = json!({
            "transactions": [
                {
                    "id": DIVIDEND_ID,
                    "timestamp": "2025-06-10T10:00:00Z",
                    "category": "DIVIDEND",
                    "symbol": GOOD,
                    "change": { "units": "12", "nanos": 500_000_000, "currencyCode": "rub" },
                },
                {
                    "id": BUY_ID,
                    "timestamp": "2025-06-12T00:00:00Z",
                    "category": "DIVIDEND",
                    "symbol": BAD,
                    "change": { "units": "7", "nanos": 0, "currencyCode": "rub" },
                },
            ],
        })
        .to_string();
        let (instruments, expected) = directory(&[GOOD_ISIN]).await;
        let channel = channel(
            fake::gateway(
                vec![
                    session_answer(),
                    page(&operations),
                    Answer::status(200, format!(r#"{{"isin":"{GOOD_ISIN}"}}"#).as_str()),
                ],
                None,
            )
            .0,
            instruments,
        );

        let parsed = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the operations are parsed");

        assert_eq!(parsed.accepted.len(), 1, "{:?}", parsed.accepted);
        assert!(matches!(
            &parsed.accepted[0].kind,
            OperationKind::Income { instrument: Some(id), .. } if *id == expected[0]
        ));
        assert_eq!(parsed.quarantined.len(), 1, "{:?}", parsed.quarantined);
        let quarantined = &parsed.quarantined[0];
        assert_eq!(quarantined.raw["symbol"], json!(BAD), "{quarantined:?}");
        assert!(
            quarantined.reason.contains(BAD)
                && quarantined.reason.contains("not resolved to an instrument")
                && quarantined.reason.contains("record the instrument"),
            "{}",
            quarantined.reason
        );
    }

    /// The asset answer that should name a position's ISIN arrives
    /// unreadable: the row is set aside with its reason and its original
    /// JSON instead of stopping the sync, and the rest of the answer still
    /// becomes claims — never a store failure (iaam-vg8te.1.3).
    #[tokio::test]
    async fn an_asset_answer_that_cannot_be_read_is_a_refused_row_not_an_abort() {
        const GOOD: &str = "GOOD@MISX";
        const UNREADABLE: &str = "UNRD@MISX";
        const GOOD_ISIN: &str = "RU000AGOOD07";
        let portfolio = json!({
            "cash": [ { "units": "100", "nanos": 0, "currencyCode": "rub" } ],
            "positions": [
                { "symbol": GOOD, "quantity": { "value": "7" } },
                { "symbol": UNREADABLE, "quantity": { "value": "3" } },
            ],
        })
        .to_string();
        let (instruments, expected) = directory(&[GOOD_ISIN]).await;
        let channel = channel(
            fake::gateway(
                vec![
                    session_answer(),
                    page(&portfolio),
                    Answer::status(200, format!(r#"{{"isin":"{GOOD_ISIN}"}}"#).as_str()),
                    Answer::status(200, "not json at all"),
                ],
                None,
            )
            .0,
            instruments,
        );

        let snapshot = channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect("the portfolio is parsed: one row's asset answer never stops the sync");

        assert_eq!(snapshot.claims.len(), 2, "{:?}", snapshot.claims);
        assert!(snapshot
            .claims
            .iter()
            .any(|claim| matches!(claim, ControlClaim::PositionQuantity { instrument, .. } if *instrument == expected[0])));
        assert_eq!(snapshot.refused.len(), 1, "{:?}", snapshot.refused);
        let refused = &snapshot.refused[0];
        assert_eq!(refused.raw["symbol"], json!(UNREADABLE), "{refused:?}");
        assert!(
            refused.reason.contains(UNREADABLE) && refused.reason.contains("cannot be read"),
            "{}",
            refused.reason
        );
        assert_eq!(
            refused.dimensions,
            [Dimension::Positions].into_iter().collect(),
            "{refused:?}"
        );
    }

    /// A transactions or portfolio answer that is not JSON at all is the
    /// source's answer failing to read (`parse_error` → `Unparsable`), the
    /// classification the sync reports as source-unreadable, never our
    /// store's failure: the whole answer failing is a whole-sync failure,
    /// unlike one row's asset answer, and it keeps that classification
    /// (iaam-vg8te.1.3).
    #[tokio::test]
    async fn a_whole_answer_that_cannot_be_read_is_the_sources_error() {
        let portfolio_channel = channel(
            fake::gateway(vec![session_answer(), page("not json at all")], None).0,
            directory(&[]).await.0,
        );
        let error = portfolio_channel
            .fetch_portfolio(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect_err("the whole portfolio answer is unreadable");
        assert!(
            matches!(&error, BrokerError::Unparsable { broker, .. } if broker == BROKER),
            "{error}"
        );

        let operations_channel = channel(
            fake::gateway(vec![session_answer(), page("not json at all")], None).0,
            directory(&[]).await.0,
        );
        let error = operations_channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect_err("the whole transactions answer is unreadable");
        assert!(
            matches!(&error, BrokerError::Unparsable { broker, .. } if broker == BROKER),
            "{error}"
        );
    }

    /// The asset read behind one operation's symbol is refused by the
    /// access (403), and the refused operation would need the ISIN it names:
    /// the whole sync stops with the source's own refusal, never with our
    /// store's failure (iaam-vg8te.1.3).
    #[tokio::test]
    async fn an_asset_read_the_source_refuses_keeps_the_refusal() {
        let operations = json!({
            "transactions": [
                {
                    "id": DIVIDEND_ID,
                    "timestamp": "2025-06-10T10:00:00Z",
                    "category": "DIVIDEND",
                    "symbol": SYMBOL,
                    "change": { "units": "12", "nanos": 500_000_000, "currencyCode": "rub" },
                },
            ],
        })
        .to_string();
        let channel = channel(
            fake::gateway(
                vec![
                    session_answer(),
                    page(&operations),
                    Answer::status(403, "forbidden"),
                ],
                None,
            )
            .0,
            directory(&[]).await.0,
        );

        let error = channel
            .fetch_operations(
                account(),
                account().inner().to_string().as_str(),
                date!(2025 - 06 - 01),
                date!(2025 - 06 - 30),
                broker_context(None),
            )
            .await
            .expect_err("the asset read is refused");

        assert!(
            matches!(&error, BrokerError::Refused { broker, .. } if broker == BROKER),
            "{error}"
        );
    }
}
