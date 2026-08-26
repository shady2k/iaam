//! Порт `BrokerChannel` поверх разобранного канала T-Invest.
//!
//! Разбор ответа остаётся в `iaam-broker`; этот слой только запрашивает
//! тело, сохраняет отвергнутые строки в карантине и связывает устойчивые
//! типы портов.

use async_trait::async_trait;
use iaam_broker::tinkoff::{
    ChannelMoney, ChannelOperation, ChannelOperationKind, GetOperationsByCursorRequest, ParseError,
    TINKOFF_PARSER_VERSION, TinkoffClient, TinkoffError, parse_operations, parse_portfolio,
};
use iaam_core::event::kind::FeeOrigin;
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, SourceId};
use iaam_core::money::CurrencyCode;
use iaam_core::reconciliation::claim::ControlClaim;
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_ingest::SubmittedOperation;
use iaam_ingest::operation::{OperationDates, OperationKind};
use uuid::Uuid;

use crate::ports::{BrokerChannel, BrokerError, ParsedOperations, Quarantined};

const BROKER: &str = "tinkoff";

/// Реализация канала брокера для T-Invest.
pub struct TinkoffChannel {
    client: TinkoffClient,
    source: SourceId,
}

impl TinkoffChannel {
    /// Создаёт канал с уже настроенным HTTP-клиентом и источником данных.
    #[must_use]
    pub fn new(client: TinkoffClient, source: SourceId) -> Self {
        Self { client, source }
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
        adapt_operations(account, operations)
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
}

fn adapt_operations(
    account: AccountId,
    operations: Vec<ChannelOperation>,
) -> Result<ParsedOperations, BrokerError> {
    let mut accepted = Vec::new();
    let mut quarantined = Vec::new();
    for operation in operations {
        if let Some(rejection) = operation.rejection.as_ref() {
            quarantined.push(Quarantined {
                raw: operation.raw,
                reason: format!("{rejection:?}: {rejection}"),
            });
        } else {
            accepted.push(operation_to_submitted(account, operation)?);
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
) -> Result<SubmittedOperation, BrokerError> {
    if let Some(rejection) = operation.rejection.as_ref() {
        return Err(unparsable(format!("строка отклонена: {rejection}")));
    }
    let kind = match operation.kind.clone() {
        ChannelOperationKind::Buy => trade_kind(account, &operation, true)?,
        ChannelOperationKind::Sell => trade_kind(account, &operation, false)?,
        ChannelOperationKind::Dividend | ChannelOperationKind::Coupon => {
            let (gross_minor, currency) = required_money(operation.payment, "payment")?;
            OperationKind::Income {
                instrument: optional_instrument(&operation)?,
                gross_minor,
                currency,
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
            return Err(unparsable("перевод не содержит счёт получателя"));
        }
        ChannelOperationKind::Other(kind) => {
            return Err(unparsable(format!("неподдержанный вид операции: {kind}")));
        }
    };

    Ok(SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: operation.date,
            ..OperationDates::default()
        },
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
        .ok_or_else(|| unparsable("торговая операция не содержит quantity"))?;
    let instrument = required_instrument(operation)?;
    let fee_minor = operation
        .commission
        .map(|money| money_amount(money, "commission"))
        .transpose()?;
    let custody = CustodyId(account.inner());
    Ok(if buy {
        OperationKind::Buy {
            instrument,
            custody,
            quantity: quantity.0,
            gross_minor,
            fee_minor,
            accrued_interest_minor: None,
            currency,
        }
    } else {
        OperationKind::Sell {
            instrument,
            custody,
            quantity: quantity.0,
            gross_minor,
            fee_minor,
            accrued_interest_minor: None,
            currency,
        }
    })
}

fn required_money(
    money: Option<ChannelMoney>,
    field: &'static str,
) -> Result<(i64, CurrencyCode), BrokerError> {
    let money = money.ok_or_else(|| unparsable(format!("операция не содержит {field}")))?;
    Ok((money_amount(money, field)?, money.currency))
}

fn money_amount(money: ChannelMoney, field: &'static str) -> Result<i64, BrokerError> {
    money
        .magnitude()
        .map(|amount| amount.raw())
        .ok_or_else(|| unparsable(format!("поле {field} не имеет положительного модуля")))
}

fn required_instrument(operation: &ChannelOperation) -> Result<InstrumentId, BrokerError> {
    let value = operation
        .instrument_uid
        .as_deref()
        .ok_or_else(|| unparsable("торговая операция не содержит instrumentUid"))?;
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
        .map_err(|_| unparsable(format!("instrumentUid не является UUID: {value}")))
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
                    "payment": {"units": "-270", "nano": -130000000, "currency": "rub"}
                }]
            }"#,
        )?;
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let operation = operations
            .into_iter()
            .next()
            .ok_or("fixture did not contain an operation")?;
        let submitted = operation_to_submitted(account, operation)?;

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
                ..
            } if quantity.inner().to_string() == "1"
        ));
        Ok(())
    }

    #[test]
    fn preserves_rejected_fixture_rows_in_quarantine() -> Result<(), Box<dyn std::error::Error>> {
        let operations = parse_operations(include_str!(
            "../../../../tests/fixtures/api/tinkoff-operations.json"
        ))?;
        let account = AccountId(Uuid::parse_str("d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4")?);
        let parsed = adapt_operations(account, operations)?;

        assert_eq!(parsed.accepted.len(), 2);
        assert_eq!(parsed.quarantined.len(), 2);
        assert!(!parsed.accepted.iter().any(|operation| {
            operation.source_operation_id.as_deref() == Some("7aa1cf04-71c7-4b62-81c7-7f27ec4cfb8d")
        }));
        let rejected = parsed
            .quarantined
            .iter()
            .find(|row| row.raw["id"] == "7aa1cf04-71c7-4b62-81c7-7f27ec4cfb8d")
            .ok_or("отказанная комиссия исчезла из карантина")?;
        assert!(rejected.reason.contains("NonRepresentableFraction"));
        assert_eq!(rejected.raw["payment"]["nano"], -135065000);
        Ok(())
    }
}
