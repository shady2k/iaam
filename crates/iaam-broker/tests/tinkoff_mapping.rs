//! Обезличенные образцы ответов T-Invest, снятые с песочницы.
//!
//! Ожидаемые значения выписаны вручную из образцов, а не вычисляются
//! тем же кодом, который проверяют тесты.

use std::error::Error;

use iaam_broker::tinkoff::{
    ChannelOperationKind, ParseError, TINKOFF_PARSER_VERSION, parse_operations, parse_portfolio,
};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};

#[test]
fn parses_operations_without_sharing_report_parser_code() -> Result<(), Box<dyn Error>> {
    let body = include_str!("../../../tests/fixtures/api/tinkoff-operations.json");
    let operations = parse_operations(body)?;
    assert_eq!(operations.len(), 4);

    let operation = operations
        .iter()
        .find(|operation| operation.operation_id == "06896b3e-038c-4970-85f2-fd5fc2dfb306")
        .ok_or("образец не содержит покупки SBER")?;
    assert_eq!(operation.kind, ChannelOperationKind::Buy);
    assert_eq!(
        operation.broker_account_id,
        "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4"
    );
    assert_eq!(
        operation.payment.as_ref().map(|money| money.amount),
        Some(PostedMinor::new(-27_013))
    );
    assert_eq!(
        operation.price.as_ref().map(|money| money.amount),
        Some(PostedMinor::new(27_013))
    );
    assert_eq!(operation.commission, None);
    assert_eq!(operation.quantity_as_decimal(), Some("1".to_owned()));
    assert_eq!(
        operation.parser_version,
        ParserVersion(TINKOFF_PARSER_VERSION.to_owned())
    );
    assert_eq!(
        operation.deduplication_key,
        "d87ca671-f5fd-4aa6-81f8-56aeaa2af6a4/06896b3e-038c-4970-85f2-fd5fc2dfb306"
    );
    assert!(matches!(
        operation.rejection.as_ref(),
        Some(ParseError::NonRepresentableFraction {
            field: "commission",
            currency: CurrencyCode::Rub,
        })
    ));

    let rejected = operations
        .iter()
        .find(|operation| operation.operation_id == "7aa1cf04-71c7-4b62-81c7-7f27ec4cfb8d")
        .ok_or("образец не содержит комиссии")?;
    assert!(matches!(
        rejected.rejection.as_ref(),
        Some(ParseError::NonRepresentableFraction {
            field: "payment",
            currency: CurrencyCode::Rub,
        })
    ));
    assert_eq!(
        rejected.raw["payment"]["units"],
        serde_json::Value::String("0".to_owned())
    );
    Ok(())
}

#[test]
fn keeps_a_rejected_row_and_its_source() -> Result<(), Box<dyn Error>> {
    let operations = parse_operations(
        r#"{
            "hasNext": false,
            "items": [{
                "id": "op",
                "payment": "not-a-money-object"
            }]
        }"#,
    )?;
    assert_eq!(operations.len(), 1);
    assert!(matches!(
        operations[0].rejection.as_ref(),
        Some(ParseError::Json(_))
    ));
    assert_eq!(
        operations[0].raw["id"],
        serde_json::Value::String("op".to_owned())
    );
    Ok(())
}

#[test]
fn rejects_a_missing_date_without_dropping_the_row() -> Result<(), Box<dyn Error>> {
    let operations = parse_operations(
        r#"{
            "hasNext": false,
            "items": [{
                "id": "op",
                "brokerAccountId": "account",
                "cursor": "cursor",
                "type": "OPERATION_TYPE_INPUT",
                "state": "OPERATION_STATE_EXECUTED"
            }]
        }"#,
    )?;
    assert!(matches!(
        operations[0].rejection.as_ref(),
        Some(ParseError::MissingField { field: "date" })
    ));
    Ok(())
}

#[test]
fn refuses_a_partial_operations_page() {
    let result = parse_operations(r#"{"hasNext":true,"items":[]}"#);
    assert!(matches!(result, Err(ParseError::PartialResponse)));
}

#[test]
fn parses_portfolio_into_cash_and_position_claims() -> Result<(), Box<dyn Error>> {
    let body = include_str!("../../../tests/fixtures/api/tinkoff-portfolio.json");
    let claims = parse_portfolio(body)?;

    assert_eq!(claims.len(), 2);
    assert!(claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount,
            at: BalancePoint::Closing,
        } if *amount == PostedMinor::new(19_972_973)
    )));
    assert!(claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::PositionQuantity {
            quantity,
            at: BalancePoint::Closing,
            ..
        } if quantity.0.inner().to_string() == "1"
    )));
    assert_eq!(
        claims
            .iter()
            .filter(|claim| matches!(claim, ControlClaim::PositionQuantity { .. }))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn maps_each_currency_position_without_aggregate_totals() -> Result<(), Box<dyn Error>> {
    let claims = parse_portfolio(
        r#"{
            "totalAmountCurrencies": {"currency": "usd", "units": "999", "nano": 0},
            "positions": [
                {
                    "instrumentType": "currency",
                    "quantity": {"units": "199729", "nano": 730000000},
                    "currentPrice": {"currency": "rub", "units": "1", "nano": 0}
                },
                {
                    "instrumentType": "currency",
                    "quantity": {"units": "10", "nano": 500000000},
                    "averagePositionPrice": {"currency": "usd", "units": "1", "nano": 0}
                }
            ]
        }"#,
    )?;

    assert_eq!(claims.len(), 2);
    assert!(claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount,
            at: BalancePoint::Closing,
        } if *amount == PostedMinor::new(19_972_973)
    )));
    assert!(claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount,
            at: BalancePoint::Closing,
        } if *amount == PostedMinor::new(1_050)
    )));
    assert!(
        !claims
            .iter()
            .any(|claim| matches!(claim, ControlClaim::PositionQuantity { .. }))
    );
    Ok(())
}

#[test]
fn refuses_money_that_cannot_be_represented_in_minor_units() -> Result<(), Box<dyn Error>> {
    let operations = parse_operations(
        r#"{
            "hasNext": false,
            "items": [{
                "cursor": "c",
                "brokerAccountId": "a",
                "id": "op",
                "date": "2026-08-20T10:11:12Z",
                "type": "OPERATION_TYPE_BUY",
                "state": "OPERATION_STATE_EXECUTED",
                "instrumentUid": "01234567-89ab-cdef-0123-456789abcdef",
                "quantity": "1",
                "payment": {"units": "1", "nano": 1, "currency": "rub"}
            }]
        }"#,
    )?;
    assert!(matches!(
        operations[0].rejection.as_ref(),
        Some(ParseError::NonRepresentableFraction {
            field: "payment",
            currency: CurrencyCode::Rub,
        })
    ));
    assert_eq!(
        operations[0].raw["id"],
        serde_json::Value::String("op".to_owned())
    );
    Ok(())
}

#[test]
fn refuses_a_position_without_uuid_identifiers() {
    let result = parse_portfolio(
        r#"{
            "positions": [{
                "quantity": {"units": "1", "nano": 0},
                "positionUid": "not-a-uuid",
                "instrumentUid": "also-not-a-uuid"
            }]
        }"#,
    );
    assert!(matches!(result, Err(ParseError::InvalidIdentifier { .. })));
}
