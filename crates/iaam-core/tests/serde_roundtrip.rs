//! Journal JSON round trip (an unresolved first-order issue).
//!
//! `docs/irreversible-core.md` stated that correctness of `Serialize`/
//! `Deserialize` rests on the derives compiling. A fact journal
//! that does not survive serialization is useless — the storage layer writes the event
//! to a text field and reads it back.

use std::collections::BTreeSet;

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::allocation::{AllocationGap, BasisAllocation};
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{EventKind, FeeOrigin, IncomeKind, TaxOrigin, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RowLocator};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId, TransferId};
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, ControlClaim};
use iaam_core::rules::lot_disposal::{Lot, LotId};
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use time::macros::date;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn per_unit(text: &str) -> PerUnitAmount {
    PerUnitAmount::new(
        Dec::new(Decimal::from_str_exact(text).expect("decimal number")),
        CurrencyCode::Rub,
    )
}

fn envelope(kind: EventKind, legs: Vec<Leg>) -> Event {
    let account = AccountId::new_random();
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner: OwnerId::new_random(),
        account,
        kind,
        dates: EventDates {
            trade: Some(TradeDate(date!(2025 - 12 - 30))),
            cash_posted: Some(CashPostedDate(date!(2026 - 01 - 03))),
            ..EventDates::empty()
        },
        order: EffectiveOrder::new(date!(2026 - 01 - 03), 7),
        legs,
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"f".repeat(64)).expect("hash"),
            ParserVersion("manual/1".into()),
        )
        .with_source_operation_id("op-42")
        .with_row(RowLocator {
            // The document is identified by its hash, not its filename: the same report,
            // saved under a different name, must remain the same
            // document.
            document: RawHash::parse(&"a".repeat(64)).expect("document hash"),
            sheet: Some("Сделки".into()),
            row: 17,
        }),
        relation: Relation::Replacement {
            target: EventId::new_random(),
        },
        confidence: Confidence::Estimated,
        idempotency_key: Some("key-1".into()),
    }
}

/// Every variant of `EventKind`, so that adding a new variant breaks this test
/// and the build, rather than silently leaving it untested.
fn every_kind() -> Vec<Event> {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    vec![
        envelope(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(10),
                gross: rub(100_000),
                fee: Some(rub(500)),
                basis_fee: None,
                basis_fee_exact: None,
                accrued_interest: Some(rub(1_234)),
            },
            vec![
                Leg::cash(account, rub(99_500)),
                Leg::security(account, custody, instrument, qty(-10)),
            ],
        ),
        envelope(
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        ),
        envelope(
            EventKind::CashOut { amount: rub(-1) },
            vec![Leg::cash(account, rub(-1))],
        ),
        envelope(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: account,
                to: AccountId::new_random(),
                amount: rub(5_000),
            },
            vec![Leg::cash(account, rub(-5_000))],
        ),
        envelope(
            EventKind::Income {
                instrument: Some(instrument),
                gross: rub(700),
                kind: Some(IncomeKind::Coupon),
            },
            vec![Leg::cash(account, rub(700))],
        ),
        envelope(
            EventKind::Fee {
                amount: rub(-99),
                origin: FeeOrigin::MarginInterest,
            },
            vec![Leg::fee(account, rub(-99))],
        ),
        envelope(
            EventKind::Tax {
                amount: rub(-17),
                origin: TaxOrigin::WithheldAtSource,
            },
            vec![Leg::tax(account, rub(-17))],
        ),
        envelope(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(3),
                cost_basis: None,
                assertions: iaam_core::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(account, custody, instrument, qty(3))],
        ),
        envelope(
            EventKind::OpeningCash { amount: rub(-42) },
            vec![Leg::cash(account, rub(-42))],
        ),
        envelope(
            EventKind::Valuation {
                instrument,
                price: Dec::new(Decimal::new(1_234_567, 4)),
                currency: CurrencyCode::Usd,
                quality: PriceQuality::Stale,
            },
            vec![],
        ),
        envelope(
            EventKind::ControlAssertion {
                period: AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
                    .expect("interval"),
                claim: ControlClaim::CashTurnover {
                    currency: CurrencyCode::Rub,
                    debit: PostedMinor::new(150_000),
                    credit: PostedMinor::new(20_000),
                },
            },
            vec![],
        ),
        envelope(
            EventKind::ImportCoverageGap {
                period: AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
                    .expect("interval"),
                dimensions: [Dimension::Cash].into_iter().collect(),
                refused: 3,
                rows: vec![
                    RefusedRow {
                        key: SourceRowKey {
                            source: SourceId::new_random(),
                            row: RowName::Given("OP-1".to_owned()),
                        },
                        dimensions: [Dimension::Cash].into_iter().collect(),
                    },
                    RefusedRow {
                        key: SourceRowKey {
                            source: SourceId::new_random(),
                            row: RowName::Given("OP-2".to_owned()),
                        },
                        dimensions: [Dimension::Cash].into_iter().collect(),
                    },
                    RefusedRow {
                        key: SourceRowKey {
                            source: SourceId::new_random(),
                            row: RowName::Given("OP-3".to_owned()),
                        },
                        dimensions: [Dimension::Cash].into_iter().collect(),
                    },
                ],
            },
            vec![],
        ),
        envelope(
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody,
                    quantity: qty(10),
                    principal_returned_per_unit: per_unit("200.0000"),
                    compensation: rub(200_000),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: Some(date!(2026 - 06 - 13)),
                    grounds: Some("решение эмитента №4".to_owned()),
                    basis_allocation: BasisAllocation::default(),
                },
            },
            vec![Leg::principal(account, instrument, rub(200_000))],
        ),
        envelope(
            EventKind::CorporateAction {
                action: CorporateAction::Redemption {
                    instrument,
                    custody,
                    quantity: qty(10),
                    principal_returned_per_unit: per_unit("800.0000"),
                    compensation: rub(800_000),
                    effective_date: date!(2026 - 12 - 15),
                    record_date: None,
                    grounds: None,
                },
            },
            vec![
                Leg::principal(account, instrument, rub(800_000)),
                Leg::security(account, custody, instrument, qty(-10)),
            ],
        ),
        envelope(
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: instrument,
                    successor: InstrumentId::new_random(),
                    custody,
                    ratio: Dec::new(Decimal::new(15, 1)),
                    quantity_in: qty(10),
                    quantity_out: qty(15),
                    fractional: FractionalTreatment::NotApplicable,
                    compensation: None,
                    effective_date: date!(2026 - 09 - 01),
                    record_date: Some(date!(2026 - 08 - 30)),
                    grounds: None,
                    basis_transfer: BasisTransferRule::CarryOver,
                },
            },
            vec![],
        ),
        envelope(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Submitted {
                    submission: OfferSubmissionId::new_random(),
                    window: OfferWindowId::new_random(),
                    instrument,
                    quantity: qty(10),
                },
            },
            vec![],
        ),
        envelope(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Cancelled {
                    submission: OfferSubmissionId::new_random(),
                    quantity: qty(4),
                },
            },
            vec![],
        ),
        envelope(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument,
                    custody,
                    quantity: qty(6),
                    gross: rub(600_000),
                    fee: Some(rub(1_000)),
                    accrued_interest: Some(rub(12_345)),
                },
            },
            vec![
                Leg::cash(account, rub(611_345)),
                Leg::security(account, custody, instrument, qty(-6)),
            ],
        ),
    ]
}

/// All event kinds that the JSON round trip must cover.
///
/// The list is pinned manually and checked against the samples: without this check, the test
/// would still call itself «every kind» without covering every kind. This has already
/// happened — `control_assertion` was missing from the samples.
const EVERY_DISCRIMINANT: [&str; 14] = [
    "trade",
    "cash_in",
    "cash_out",
    "cash_transfer",
    "income",
    "fee",
    "tax",
    "opening_position",
    "opening_cash",
    "valuation",
    "control_assertion",
    "import_coverage_gap",
    "corporate_action",
    "offer_exercise",
];

/// Exhaustiveness guard for the list above: a new variant must break the build
/// here. There is intentionally no `_` arm — it is exactly the hole that
/// this guard exists to prevent (§15.1).
fn is_known(kind: &EventKind) -> bool {
    match kind {
        EventKind::Trade { .. }
        | EventKind::CashIn { .. }
        | EventKind::CashOut { .. }
        | EventKind::CashTransfer { .. }
        | EventKind::Income { .. }
        | EventKind::Fee { .. }
        | EventKind::Tax { .. }
        | EventKind::OpeningPosition { .. }
        | EventKind::OpeningCash { .. }
        | EventKind::Valuation { .. }
        | EventKind::ControlAssertion { .. }
        | EventKind::ImportCoverageGap { .. }
        | EventKind::CorporateAction { .. }
        | EventKind::OfferExercise { .. } => true,
    }
}

#[test]
fn the_round_trip_covers_every_event_kind() {
    let covered: BTreeSet<&str> = every_kind()
        .iter()
        .inspect(|event| assert!(is_known(&event.kind)))
        .map(|event| event.kind.discriminant())
        .collect();
    let expected: BTreeSet<&str> = EVERY_DISCRIMINANT.into_iter().collect();
    assert_eq!(
        covered, expected,
        "the event kind has no sample: the JSON round trip does not test it"
    );
}

#[test]
fn every_event_kind_survives_a_json_round_trip() {
    for event in every_kind() {
        let json = serde_json::to_string(&event).expect("serialization");
        let back: Event = serde_json::from_str(&json).expect("parsing");
        assert_eq!(back, event, "round trip changed the event: {json}");
    }
}

#[test]
fn a_partial_redemption_written_before_the_allocation_field_reads_as_unknown() {
    // The body was written before `basis_allocation` was introduced. It must still parse,
    // and the fraction must be unknown, not zero.
    let text = r#"{"PartialRedemption":{"instrument":"8e27804a-de75-417e-a6ad-a68e919aed97","custody":"269bd88e-c7f0-422b-85ac-e56b0eba6485","quantity":"10","principal_returned_per_unit":{"value":"200.0000","currency":"Rub"},"compensation":{"amount":200000,"currency":"Rub"},"effective_date":[2026,166],"record_date":[2026,164],"grounds":"решение эмитента №4"}}"#;
    let action: CorporateAction = serde_json::from_str(text).expect("legacy body parses");
    let CorporateAction::PartialRedemption {
        basis_allocation, ..
    } = action
    else {
        panic!("expected an amortization event");
    };
    assert_eq!(
        basis_allocation,
        BasisAllocation::Unknown(AllocationGap::NotComputed)
    );
}

#[test]
fn a_lot_archive_with_removed_principal_field_still_reads() {
    let value = serde_json::json!({
        "id": LotId::new_random(),
        "instrument": InstrumentId::new_random(),
        "acquired": null,
        "quantity": qty(10),
        "cost_basis": rub(100_000),
        "principal": "Unknown",
    });
    let text = serde_json::to_string(&value).expect("legacy body serializes");
    let lot: Lot = serde_json::from_str(&text).expect("legacy body parses");
    assert_eq!(lot.quantity, qty(10));
    assert_eq!(lot.cost_basis, rub(100_000));
    assert_eq!(lot.acquisition_basis, None);
    assert_eq!(lot.accrued_interest_paid, None);
    assert_eq!(lot.received_to_date, None);
}

#[test]
fn a_decimal_keeps_its_scale_through_json() {
    // Scale is part of the value: 1.2340 and 1.234 have different source precision,
    // and losing the scale changes the meaning of the price (§3.4).
    let price = Dec::new(Decimal::new(12_340, 4));
    let json = serde_json::to_string(&price).expect("serialization");
    let back: Dec = serde_json::from_str(&json).expect("parsing");
    assert_eq!(back.inner().scale(), 4, "scale lost: {json}");
    assert_eq!(back, price);
}
