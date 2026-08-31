//! A `BrokerChannel` port over the parsed T-Invest channel.
//!
//! Response parsing stays in `iaam-broker`; this layer only requests
//! the body, quarantines rejected rows, and binds stable
//! port types.

use async_trait::async_trait;
use iaam_broker::operation_kind::OperationKindDictionary;
use iaam_broker::tinkoff::{
    ChannelMoney, ChannelOperation, ChannelOperationKind, GetOperationsByCursorRequest, ParseError,
    TINKOFF_PARSER_VERSION, TinkoffClient, TinkoffError, parse_operations, parse_portfolio,
};
use iaam_core::event::kind::{FeeOrigin, IncomeKind};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, SourceId};
use iaam_core::money::CurrencyCode;
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_ingest::SubmittedOperation;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use uuid::Uuid;

use crate::ports::{BrokerChannel, BrokerError, ParsedOperations, Quarantined};

const BROKER: &str = "tinkoff";

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
        request.to = Some(rfc3339_midnight(to));
        let body = self
            .client
            .get_operations_by_cursor(&request)
            .await
            .map_err(tinkoff_error)?;
        let operations = parse_operations(&body).map_err(parse_error)?;
        adapt_operations(account, operations, &self.dictionary)
    }

    async fn fetch_portfolio(
        &self,
        account: AccountId,
        _at: time::Date,
    ) -> Result<Vec<ControlClaim>, BrokerError> {
        let body = self
            .client
            .get_portfolio(&account.inner().to_string())
            .await
            .map_err(tinkoff_error)?;
        parse_portfolio(&body).map_err(parse_error)
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
    for operation in operations {
        if let Some(rejection) = operation.rejection.as_ref() {
            quarantined.push(Quarantined {
                raw: operation.raw,
                reason: format!("{rejection:?}: {rejection}"),
            });
        } else {
            let kind = dictionary.kind_of(&operation.source_kind);
            accepted.push(operation_to_submitted(account, operation, kind)?);
        }
    }
    Ok(ParsedOperations {
        accepted,
        quarantined,
    })
}

fn operation_to_submitted(
    account: AccountId,
    operation: ChannelOperation,
    kind: ChannelOperationKind,
) -> Result<SubmittedOperation, BrokerError> {
    if let Some(rejection) = operation.rejection.as_ref() {
        return Err(unparsable(format!("row rejected: {rejection}")));
    }
    let kind = match kind {
        ChannelOperationKind::Buy => trade_kind(account, &operation, true)?,
        ChannelOperationKind::Sell => trade_kind(account, &operation, false)?,
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
                    return Err(unparsable(format!("income kind mismatch: {other:?}")));
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
        ChannelOperationKind::Transfer => {
            return Err(unparsable("transfer does not contain a recipient account"));
        }
        // Amortisation and redemption are corporate actions, not
        // owner operations: they have their own representation and endpoint
        // (POST /v1/ingest/journal-events). The rejection here identifies
        // WHAT IS MISSING, rather than «unknown kind»: the channel reports the
        // payment amount, but not the returned face value per unit or the custody
        // location; without them the fact cannot be constructed, and substituting
        // a guess would record something that never happened in the append-only journal.
        ChannelOperationKind::BondAmortisation => {
            return Err(unparsable(
                "bond amortisation: the channel does not report the returned face value per unit \
                 or custody location — the fact is entered via the journal endpoint",
            ));
        }
        ChannelOperationKind::BondRedemption => {
            return Err(unparsable(
                "bond redemption: the channel does not report the returned face value per unit \
                 or custody location — the fact is entered via the journal endpoint",
            ));
        }
        ChannelOperationKind::Other(kind) => {
            return Err(unparsable(format!("unsupported operation kind: {kind}")));
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

fn trade_kind(
    account: AccountId,
    operation: &ChannelOperation,
    buy: bool,
) -> Result<OperationKind, BrokerError> {
    let (gross_minor, currency) = required_money(operation.payment, "payment")?;
    let quantity = operation
        .quantity
        .ok_or_else(|| unparsable("trading operation does not contain quantity"))?;
    let instrument = required_instrument(operation)?;
    let basis_fee = operation.commission;
    let custody = CustodyId(account.inner());
    Ok(if buy {
        OperationKind::Buy {
            instrument,
            custody,
            quantity: quantity.0,
            gross_minor,
            fee_minor: None,
            basis_fee,
            accrued_interest_minor: None,
            currency,
        }
    } else {
        OperationKind::Sell {
            instrument,
            custody,
            quantity: quantity.0,
            gross_minor,
            fee_minor: None,
            basis_fee,
            accrued_interest_minor: None,
            currency,
        }
    })
}

fn required_money(
    money: Option<ChannelMoney>,
    field: &'static str,
) -> Result<(i64, CurrencyCode), BrokerError> {
    let money = money.ok_or_else(|| unparsable(format!("operation does not contain {field}")))?;
    Ok((money_amount(money, field)?, money.currency))
}

fn money_amount(money: ChannelMoney, field: &'static str) -> Result<i64, BrokerError> {
    money
        .magnitude()
        .map(|amount| amount.raw())
        .ok_or_else(|| unparsable(format!("field {field} does not have a positive magnitude")))
}

fn required_instrument(operation: &ChannelOperation) -> Result<InstrumentId, BrokerError> {
    let value = operation
        .instrument_uid
        .as_deref()
        .ok_or_else(|| unparsable("trading operation does not contain instrumentUid"))?;
    parse_instrument(value)
}

fn optional_instrument(operation: &ChannelOperation) -> Result<Option<InstrumentId>, BrokerError> {
    operation
        .instrument_uid
        .as_deref()
        .map(parse_instrument)
        .transpose()
}

fn parse_instrument(value: &str) -> Result<InstrumentId, BrokerError> {
    Uuid::parse_str(value)
        .map(InstrumentId)
        .map_err(|_| unparsable(format!("instrumentUid is not a UUID: {value}")))
}

fn rfc3339_midnight(date: time::Date) -> String {
    format!("{date}T00:00:00Z")
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

#[cfg(test)]
mod tests {
    use iaam_broker::tinkoff::parse_operations;
    use iaam_core::ids::AccountId;
    use iaam_ingest::operation::OperationKind;
    use uuid::Uuid;

    use super::{adapt_operations, operation_to_submitted};
    use iaam_broker::operation_kind::OperationKindDictionary;

    use iaam_core::event::kind::IncomeKind;

    fn income_operation(operation_type: &str) -> String {
        format!(
            r#"{{
                "hasNext": false,
                "items": [{{
                    "cursor": "cursor-1",
                    "brokerAccountId": "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4",
                    "id": "06896b3e-038c-4970-85f2-fd5fc2dfb306",
                    "date": "2026-08-20T10:11:12Z",
                    "type": "{operation_type}",
                    "state": "OPERATION_STATE_EXECUTED",
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
                state: "OPERATION_STATE_EXECUTED".to_owned(),
                instrument_uid: None,
                figi: None,
                quantity: None,
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
            let text = error.to_string();
            assert!(
                text.contains("returned face value per unit"),
                "error does not identify what is missing: {text}"
            );
            assert!(
                text.contains("journal endpoint"),
                "error does not identify where the fact is entered: {text}"
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
            ("OPERATION_TYPE_OUTPUT", "withdrawal"),
        ]);
        assert!(unreadable.is_empty(), "{unreadable:?}");
        dictionary
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
    fn maps_a_parsed_buy_mechanically() -> Result<(), Box<dyn std::error::Error>> {
        let operations = parse_operations(
            r#"{
                "hasNext": false,
                "items": [{
                    "cursor": "cursor-1",
                    "brokerAccountId": "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4",
                    "id": "06896b3e-038c-4970-85f2-fd5fc2dfb306",
                    "date": "2026-08-20T10:11:12Z",
                    "type": "OPERATION_TYPE_BUY",
                    "state": "OPERATION_STATE_EXECUTED",
                    "instrumentUid": "01234567-89ab-cdef-0123-456789abcdef",
                    "quantity": "1",
                    "payment": {"units": "-270", "nano": -130000000, "currency": "rub"},
                    "commission": {"units": "0", "nano": -135065000, "currency": "rub"}
                }]
            }"#,
        )?;
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let operation = operations
            .into_iter()
            .next()
            .ok_or("fixture did not contain an operation")?;
        let kind = dictionary().kind_of(&operation.source_kind);
        let submitted = operation_to_submitted(account, operation, kind)?;

        assert_eq!(submitted.account, account);
        assert_eq!(
            submitted.source_operation_id.as_deref(),
            Some("06896b3e-038c-4970-85f2-fd5fc2dfb306")
        );
        assert_eq!(
            submitted.idempotency_key.as_deref(),
            Some("d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4/06896b3e-038c-4970-85f2-fd5fc2dfb306")
        );
        assert_eq!(
            submitted.dates.trade,
            Some(time::macros::date!(2026 - 08 - 20))
        );
        assert!(matches!(
            submitted.kind,
            OperationKind::Buy {
                gross_minor: 27013,
                quantity,
                basis_fee: Some(basis_fee),
                ..
            } if quantity.inner().to_string() == "1"
                && basis_fee.value().inner().to_string() == "-0.135065"
        ));
        Ok(())
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
            operation.source_operation_id.as_deref() == Some("06896b3e-038c-4970-85f2-fd5fc2dfb306")
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
        Ok(())
    }
}
