//! All samples here are synthetic and built from the official Finam REST API
//! documentation; the project has no live token or gateway snapshots. Tests
//! parse a fixed schema rather than asserting that a live service is unchanged.

use std::error::Error;

use iaam_broker::finam::{FINAM_PARSER_VERSION, ParseError, parse_operations, parse_portfolio};
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
    // The trade's quantity is read from `trade.size`, where the published
    // contract carries it; top-level `changeQty` is a securities-transfer
    // field the fixture does not invent.
    assert_eq!(buy.quantity_as_decimal(), Some("1".to_owned()));
    assert_eq!(
        buy.payment.as_ref().map(|money| money.amount),
        Some(PostedMinor::new(-27_013))
    );
    // The trade's own money arrives beside the ruble fold, named by the
    // contract's `change_original`.
    assert_eq!(
        buy.change_original.as_ref().map(|money| money.amount),
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
    let parsed = parse_portfolio(body)?;

    assert!(parsed.refused.is_empty(), "{:?}", parsed.refused);
    assert_eq!(parsed.claims.len(), 3);
    assert!(parsed.claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount,
            at: BalancePoint::Closing,
        } if *amount == PostedMinor::new(19_972_973)
    )));
    assert!(parsed.claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount,
            at: BalancePoint::Closing,
        } if *amount == PostedMinor::new(1_050)
    )));
    assert!(parsed.claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::PositionQuantity {
            quantity,
            at: BalancePoint::Closing,
            ..
        } if quantity.0.inner().to_string() == "1"
    )));
    Ok(())
}

/// Finam names an instrument by a symbol of the form `TICKER@MIC`, and this
/// parser cannot resolve a symbol to an instrument yet — the resolution
/// through Finam's own asset endpoint is a later task. Until then such a row
/// is set aside with that reason instead of refusing the whole answer: the
/// readable rows still become claims, and the unfit row reaches the owner
/// with its reason and its original JSON (iaam-vg8te.1.2).
#[test]
fn a_position_with_an_unresolved_symbol_is_set_aside_and_the_rest_imports() {
    let parsed = parse_portfolio(
        r#"{
            "cash": [{"currency_code": "RUB", "units": "100", "nanos": 0}],
            "positions": [
                {
                    "symbol": "01234567-89ab-cdef-0123-456789abcdef",
                    "quantity": {"value": "1"}
                },
                {
                    "symbol": "SBER@MISX",
                    "quantity": {"value": "10"}
                }
            ]
        }"#,
    )
    .expect("the answer parses: the unfit row is set aside, not fatal");

    assert_eq!(parsed.claims.len(), 2, "{:?}", parsed.claims);
    assert!(parsed.claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount,
            ..
        } if *amount == PostedMinor::new(10_000)
    )));
    assert!(parsed.claims.iter().any(|claim| matches!(
        claim,
        ControlClaim::PositionQuantity { quantity, .. }
            if quantity.0.inner().to_string() == "1"
    )));

    assert_eq!(parsed.refused.len(), 1, "{:?}", parsed.refused);
    let refused = &parsed.refused[0];
    assert_eq!(
        refused.raw["symbol"],
        serde_json::Value::String("SBER@MISX".to_owned())
    );
    assert!(matches!(
        &refused.reason,
        ParseError::UnresolvedSymbol { value } if value == "SBER@MISX"
    ));
    assert!(
        refused
            .reason
            .to_string()
            .contains("not resolved to an instrument"),
        "{}",
        refused.reason
    );
}

/// A row whose own fields do not assemble is set aside with the reason
/// naming the field, and its neighbours still become claims.
#[test]
fn a_position_without_a_readable_quantity_is_set_aside_with_the_field_named() {
    let parsed = parse_portfolio(
        r#"{
            "positions": [
                {"symbol": "01234567-89ab-cdef-0123-456789abcdef"},
                {
                    "symbol": "01234567-89ab-cdef-0123-456789abcdef",
                    "quantity": {"value": "3"}
                }
            ]
        }"#,
    )
    .expect("the answer parses: the unfit row is set aside, not fatal");

    assert_eq!(parsed.claims.len(), 1, "{:?}", parsed.claims);
    assert_eq!(parsed.refused.len(), 1, "{:?}", parsed.refused);
    assert!(matches!(
        &parsed.refused[0].reason,
        ParseError::MissingField { field: "quantity" }
    ));
}

/// The published response is a bare repeated list: no continuation fields
/// exist to be missing, and their absence is not an error.
#[test]
fn parses_the_bare_repeated_response_the_contract_publishes() {
    let operations = parse_operations(r#"{"transactions":[]}"#).expect("the bare answer parses");
    assert!(operations.is_empty());
}
