//! All samples here are synthetic and built from the official Finam REST API
//! documentation; the project has no live token or gateway snapshots. Tests
//! parse a fixed schema rather than asserting that a live service is unchanged.

use std::error::Error;

use iaam_broker::finam::{
    FINAM_PARSER_VERSION, ParseError, parse_asset, parse_operations, parse_portfolio,
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
    // The cash entries become claims here; a position row becomes one only
    // after its symbol resolves through Finam's asset description, which is
    // the adapter's work — the parser keeps the row whole instead.
    assert_eq!(parsed.claims.len(), 2, "{:?}", parsed.claims);
    assert_eq!(parsed.unresolved.len(), 1, "{:?}", parsed.unresolved);
    assert_eq!(
        parsed.unresolved[0].symbol,
        "01234567-89ab-cdef-0123-456789abcdef"
    );
    assert_eq!(parsed.unresolved[0].quantity.0.inner().to_string(), "1");
    assert!(parsed.claims.iter().all(|claim| matches!(
        claim,
        ControlClaim::CashBalance {
            at: BalancePoint::Closing,
            ..
        }
    )));
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
    Ok(())
}

/// Finam names an instrument by a symbol of the form `TICKER@MIC`, and the
/// resolution to an iaam instrument runs through Finam's own asset
/// description — the adapter's work, not the parser's. The parser keeps
/// every readable position row whole: its symbol, its quantity and its
/// original JSON travel together, nothing is guessed into an instrument,
/// and the cash beside it still becomes claims.
#[test]
fn a_position_row_travels_whole_for_the_symbol_resolution() {
    let parsed = parse_portfolio(
        r#"{
            "cash": [{"currency_code": "RUB", "units": "100", "nanos": 0}],
            "positions": [
                {
                    "symbol": "SBER@MISX",
                    "quantity": {"value": "10"}
                }
            ]
        }"#,
    )
    .expect("the answer parses: the row is kept for resolution, not fatal");

    assert_eq!(parsed.claims.len(), 1, "{:?}", parsed.claims);
    assert_eq!(parsed.refused.len(), 0, "{:?}", parsed.refused);
    assert_eq!(parsed.unresolved.len(), 1, "{:?}", parsed.unresolved);
    let row = &parsed.unresolved[0];
    assert_eq!(row.symbol, "SBER@MISX");
    assert_eq!(row.quantity.0.inner().to_string(), "10");
    assert_eq!(
        row.raw["symbol"],
        serde_json::Value::String("SBER@MISX".to_owned())
    );
}

/// The asset description is what the resolution reads the ISIN from. One
/// that names no ISIN is a valid answer — it is the standing refusal case
/// one sync layer down — while an answer that is not JSON at all is a
/// parse error.
#[test]
fn an_asset_description_names_its_isin_when_it_carries_one() {
    assert_eq!(
        parse_asset(r#"{"isin":"RU000AFIXTUR","name":"Fixture Share"}"#)
            .expect("the description parses"),
        Some("RU000AFIXTUR".to_owned())
    );
    for body in [
        r#"{"name":"Fixture Share"}"#,
        r#"{"isin":""}"#,
        r#"{"isin":"   "}"#,
    ] {
        assert_eq!(
            parse_asset(body).expect("a valid answer without an ISIN"),
            None,
            "{body}"
        );
    }
    assert!(parse_asset("not json").is_err());
}

/// A row whose own fields do not assemble is set aside with the reason
/// naming the field, and its readable neighbour still travels for the
/// symbol resolution.
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

    assert_eq!(parsed.claims.len(), 0, "{:?}", parsed.claims);
    assert_eq!(parsed.unresolved.len(), 1, "{:?}", parsed.unresolved);
    assert_eq!(parsed.unresolved[0].quantity.0.inner().to_string(), "3");
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
