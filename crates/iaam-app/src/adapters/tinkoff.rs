//! Порт `BrokerChannel` поверх разобранного канала T-Invest.
//!
//! Разбор ответа остаётся в `iaam-broker`; этот слой только запрашивает
//! тело, сохраняет отвергнутые строки в карантине и связывает устойчивые
//! типы портов.

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
use iaam_ingest::operation::{OperationDates, OperationKind};
use uuid::Uuid;

use crate::ports::{BrokerChannel, BrokerError, ParsedOperations, Quarantined};

const BROKER: &str = "tinkoff";

/// Реализация канала брокера для T-Invest.
pub struct TinkoffChannel {
    client: TinkoffClient,
    source: SourceId,
    /// Словарь видов операций этого канала. Приезжает из хранилища
    /// готовым: разбор в `iaam-broker` про хранилище не знает, и
    /// связывает их этот адаптер — тем же приёмом, что уже сделан
    /// для SQLite.
    dictionary: OperationKindDictionary,
}

impl TinkoffChannel {
    /// Создаёт канал с уже настроенным HTTP-клиентом, источником данных
    /// и словарём видов операций.
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
}

fn adapt_operations(
    account: AccountId,
    operations: Vec<ChannelOperation>,
    dictionary: &OperationKindDictionary,
) -> Result<ParsedOperations, BrokerError> {
    // Пустой словарь — это ненастроенный канал, а не непонятный брокер.
    // Без этой проверки владелец получил бы отказ про каждый код
    // по отдельности и пошёл бы разбираться с брокером вместо настройки.
    if dictionary.is_empty() && !operations.is_empty() {
        return Err(unparsable(
            "словарь видов операций канала пуст: разбирать выгрузку нечем",
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
        return Err(unparsable(format!("строка отклонена: {rejection}")));
    }
    let kind = match kind {
        ChannelOperationKind::Buy => trade_kind(account, &operation, true)?,
        ChannelOperationKind::Sell => trade_kind(account, &operation, false)?,
        // Схлопывать купон и дивиденд в один приход нельзя: журнал
        // хранит вид, и потерять его здесь значит потерять навсегда —
        // событие неизменяемо.
        kind @ (ChannelOperationKind::Dividend | ChannelOperationKind::Coupon) => {
            let (gross_minor, currency) = required_money(operation.payment, "payment")?;
            let income_kind = match kind {
                ChannelOperationKind::Coupon => IncomeKind::Coupon,
                ChannelOperationKind::Dividend => IncomeKind::Dividend,
                // Внешний образец уже сузил варианты. Ветвь недостижима
                // и обязана быть шумной, а не подставлять дивиденд.
                other => {
                    return Err(unparsable(format!("вид дохода разъехался: {other:?}")));
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
            return Err(unparsable("перевод не содержит счёт получателя"));
        }
        // Амортизация и погашение — корпоративные действия, а не
        // операции владельца: у них своя форма и свой вход
        // (POST /v1/ingest/journal-events). Отказ здесь называет
        // НЕДОСТАЮЩЕЕ, а не «непонятный вид»: канал сообщает сумму
        // выплаты, но не возвращённый номинал на единицу и не место
        // хранения, а без них факт не построить, и подстановка
        // догадки записала бы в append-only журнал то, чего не было.
        ChannelOperationKind::BondAmortisation => {
            return Err(unparsable(
                "амортизация облигации: канал не сообщает возвращённый номинал на единицу \
                 и место хранения — факт вводится через журнальный вход",
            ));
        }
        ChannelOperationKind::BondRedemption => {
            return Err(unparsable(
                "погашение облигации: канал не сообщает возвращённый номинал на единицу \
                 и место хранения — факт вводится через журнальный вход",
            ));
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

    /// Амортизация и погашение — корпоративные действия, и канал
    /// данных для них не даёт. Отказ обязан называть НЕДОСТАЮЩЕЕ:
    /// «неподдержанный вид» отправил бы владельца искать поддержку
    /// вида, которая есть, вместо данных, которых нет.
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
                broker_account_id: "счёт".to_owned(),
                operation_id: "1".to_owned(),
                parent_operation_id: None,
                cursor: "c".to_owned(),
                source_kind: "не важно: вид передаётся отдельно".to_owned(),
                state: "OPERATION_STATE_EXECUTED".to_owned(),
                instrument_uid: None,
                figi: None,
                quantity: None,
                payment: None,
                price: None,
                commission: None,
                deduplication_key: "k".to_owned(),
                parser_version: iaam_core::event::provenance::ParserVersion("тест".to_owned()),
                raw: serde_json::Value::Null,
                rejection: None,
            };
            let account = AccountId(Uuid::from_u128(1));
            let error = operation_to_submitted(account, operation, kind.clone())
                .expect_err("корпоративное действие каналом не строится");
            let text = error.to_string();
            assert!(
                text.contains("номинал на единицу"),
                "отказ не называет недостающее: {text}"
            );
            assert!(
                text.contains("журнальный вход"),
                "отказ не называет, куда факт вводится: {text}"
            );
        }
    }

    /// Словарь канала в тестах заводится явно: классификация — данные,
    /// и тест, полагающийся на вшитый список, проверял бы список,
    /// которого больше нет.
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
        let operations = parse_operations(&income_operation(operation_type)).expect("разбор");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let operation = operations.into_iter().next().expect("одна операция");
        let kind = dictionary().kind_of(&operation.source_kind);
        let submitted = operation_to_submitted(account, operation, kind).expect("операция принята");
        match submitted.kind {
            OperationKind::Income { kind, .. } => kind,
            other => panic!("ожидался приход дохода, получено {other:?}"),
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
        // Схлопывание двух видов в один приход теряло вид навсегда:
        // событие журнала неизменяемо.
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
        // Молчаливое превращение неизвестного вида в приход денег
        // хуже отказа: отказ виден, выдумка — нет.
        let operations =
            parse_operations(&income_operation("OPERATION_TYPE_SOMETHING_NEW")).expect("разбор");
        let account = AccountId(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888));
        let operation = operations.into_iter().next().expect("одна операция");
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
                    "payment": {"units": "-270", "nano": -130000000, "currency": "rub"}
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
        let parsed = adapt_operations(account, operations, &dictionary())?;

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
