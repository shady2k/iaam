//! Parity between the production and reference lot-disposal implementations (§15.4).
//!
//! Both run on the same inputs, and both are checked against frozen expected
//! values. Matching the production implementation to the reference without
//! checking the fixture is insufficient: both could happen to make the same
//! mistake.

use iaam_core::dates::TradeDate;
use iaam_core::ids::InstrumentId;
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::rules::lot_disposal::{DisposalInput, FifoV1, Lot, LotDisposalRule, LotId};
use iaam_oracle::lots_reference::{RefLot, dispose_fifo_rational};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::macros::date;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    lots: Vec<RefLotJson>,
    sell_quantity: i64,
    expected_basis_released_minor: i64,
    expected_remaining: Vec<RefLotJson>,
}

#[derive(Deserialize, Clone, Copy)]
struct RefLotJson {
    quantity: i64,
    basis_minor: i64,
}

fn qty(n: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(n)))
}

fn to_core_lots(items: &[RefLotJson]) -> Vec<Lot> {
    let instrument = InstrumentId::new_random();
    items
        .iter()
        .map(|l| Lot {
            id: LotId::new_random(),
            instrument,
            acquired: Some(TradeDate(date!(2026 - 01 - 01))),
            quantity: qty(l.quantity),
            cost_basis: Money::new(PostedMinor::new(l.basis_minor), CurrencyCode::Rub),
            acquisition_basis: None,
            accrued_interest_paid: None,
            received_to_date: None,
        })
        .collect()
}

#[test]
fn production_matches_oracle_and_frozen_expectations() {
    let raw = include_str!("../../../tests/fixtures/fifo_cases.json");
    let fixture: Fixture = serde_json::from_str(raw).expect("fixture parses");
    assert!(!fixture.cases.is_empty(), "fixture must not be empty");

    for case in &fixture.cases {
        // --- Reference ---
        let ref_lots: Vec<RefLot> = case
            .lots
            .iter()
            .map(|l| RefLot {
                quantity: l.quantity,
                basis_minor: l.basis_minor,
            })
            .collect();
        let oracle = dispose_fifo_rational(&ref_lots, case.sell_quantity)
            .unwrap_or_else(|e| panic!("reference failed on case “{}”: {e:?}", case.name));

        // --- Production ---
        let input = DisposalInput {
            lots: to_core_lots(&case.lots),
            quantity: qty(case.sell_quantity),
        };
        let production = FifoV1
            .apply(&input)
            .unwrap_or_else(|e| panic!("production failed on case “{}”: {e:?}", case.name));

        // --- Both against frozen expected values ---
        assert_eq!(
            oracle.basis_released_minor, case.expected_basis_released_minor,
            "reference differs from fixture on case “{}”",
            case.name
        );
        assert_eq!(
            production.basis_released.amount().raw(),
            case.expected_basis_released_minor,
            "production differs from fixture on case “{}”",
            case.name
        );

        // --- Remainders ---
        assert_eq!(
            oracle.remaining.len(),
            case.expected_remaining.len(),
            "reference: wrong number of remaining lots for “{}”",
            case.name
        );
        assert_eq!(
            production.remaining.len(),
            case.expected_remaining.len(),
            "production: wrong number of remaining lots for “{}”",
            case.name
        );
        for (i, expected) in case.expected_remaining.iter().enumerate() {
            assert_eq!(
                oracle.remaining[i].basis_minor, expected.basis_minor,
                "reference: remaining lot {i} cost for “{}”",
                case.name
            );
            assert_eq!(
                production.remaining[i].cost_basis.amount().raw(),
                expected.basis_minor,
                "production: remaining lot {i} cost for “{}”",
                case.name
            );
            // The remaining quantity is frozen by the fixture too: without
            // this check, `quantity` in `expected_remaining` would have no
            // other verification, and cost allocation could use the wrong
            // quantity.
            assert_eq!(
                oracle.remaining[i].quantity, expected.quantity,
                "reference: remaining lot {i} quantity for “{}”",
                case.name
            );
            assert_eq!(
                production.remaining[i].quantity,
                qty(expected.quantity),
                "production: remaining lot {i} quantity for “{}”",
                case.name
            );
        }
    }
}
