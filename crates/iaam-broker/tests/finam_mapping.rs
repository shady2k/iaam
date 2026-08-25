//! Все образцы здесь синтетические и построены по официальной документации
//! Finam REST API; живого токена и снимков шлюза у проекта нет. Тесты
//! проверяют разбор зафиксированной схемы, а не неизменность живого сервиса.

use std::error::Error;

use iaam_broker::finam::{
    ChannelOperationKind, FINAM_PARSER_VERSION, ParseError, parse_operations, parse_portfolio,
};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};

#[test]
fn parses_synthetic_transactions_and_keeps_rejected_rows() -> Result<(), Box<dyn Error>> {
    let body = include_str!("../../../tests/fixtures/api/finam-transactions.json");
    let operations = parse_operations(body)?;
    assert_eq!(operations.len(), 3);

    let buy = operations
        .iter()
        .find(|operation| operation.operation_id == "FINAM-TRADE-001")
        .ok_or("synthetic fixture does not contain the buy")?;
    assert_eq!(buy.kind, ChannelOperationKind::Buy);
    assert_eq!(buy.quantity_as_decimal(), Some("1".to_owned()));
    assert_eq!(
        buy.payment.as_ref().map(|money| money.amount),
        Some(PostedMinor::new(-27_013))
    );
    assert_eq!(
        buy.parser_version,
        ParserVersion(FINAM_PARSER_VERSION.to_owned())
    );

    let rejected = operations
        .iter()
        .find(|operation| operation.operation_id == "FINAM-FEE-001")
        .ok_or("synthetic fixture does not contain the rejected fee")?;
    assert!(matches!(
        rejected.rejection.as_ref(),
        Some(ParseError::NonRepresentableFraction {
            field: "change",
            currency: CurrencyCode::Rub,
        })
    ));
    assert_eq!(
        rejected.raw["change"]["nanos"],
        serde_json::Value::Number((-135065000_i64).into())
    );
    Ok(())
}

#[test]
fn parses_synthetic_portfolio_cash_and_positions() -> Result<(), Box<dyn Error>> {
    let body = include_str!("../../../tests/fixtures/api/finam-portfolio.json");
    let claims = parse_portfolio(body)?;

    assert_eq!(claims.len(), 3);
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
    assert!(claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::PositionQuantity {
            quantity,
            at: BalancePoint::Closing,
            ..
        } if quantity.0.inner().to_string() == "1"
    )));
    Ok(())
}

#[test]
fn refuses_a_transaction_page_with_missing_continuation_token() {
    let result = parse_operations(r#"{"hasMore":true,"transactions":[]}"#);
    assert!(matches!(result, Err(ParseError::PartialResponse)));
}
