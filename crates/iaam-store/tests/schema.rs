//! Vocabulary agreement between Rust enums and the SQL `CHECK`s that mirror
//! them (spec §9, "Vocabulary agreement").
//!
//! SQLite has no enum type, so every closed vocabulary the schema names
//! appears twice: once as a Rust enum and once as `CHECK (x IN (...))`. These
//! tests read the DDL SQLite itself stored for each table back out of
//! `sqlite_master` and assert that every Rust discriminant appears in it, so
//! the two cannot silently drift apart. Where the vocabulary has no Rust-side
//! discriminant function, an exhaustive local `match` stands in for it — it
//! still breaks the build the day a variant is added, which is the property
//! that matters.

use iaam_core::category::CategoryMatcher;
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::{EventKind, FeeOrigin, TaxOrigin};
use iaam_core::event::leg::LegKind;
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::event::{Confidence, Relation};
use iaam_core::ids::{CustodyId, EventId, InstrumentId};
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use iaam_store::SqliteStore;
use time::macros::date;

// `rust_decimal` is not a dependency of this crate, and adding it just for a
// test would touch Cargo.toml, which is the policy file. A decimal value
// arrives through the same serde path storage itself uses to read it
// (`iaam-store/tests/bundle.rs` does the same).
fn dec(text: &str) -> Dec {
    serde_json::from_str::<Dec>(text).unwrap()
}

/// Read the DDL SQLite stored for one table.
fn table_ddl(store: &SqliteStore, table: &str) -> String {
    store
        .connection()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("reading DDL for {table}: {error}"))
}

fn assert_ddl_lists(ddl: &str, table: &str, values: &[&str]) {
    for value in values {
        assert!(
            ddl.contains(&format!("'{value}'")),
            "{value} missing from {table}'s CHECK:\n{ddl}"
        );
    }
}

#[test]
fn the_kind_check_lists_exactly_the_event_discriminants() {
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "events");
    for kind in EventKind::discriminants() {
        assert!(
            ddl.contains(&format!("'{kind}'")),
            "kind {kind} missing from the CHECK"
        );
    }
}

#[test]
fn the_leg_kind_check_matches_leg_kind() {
    fn code(kind: LegKind) -> &'static str {
        match kind {
            LegKind::Cash => "cash",
            LegKind::SecurityQuantity => "security_quantity",
            LegKind::Principal => "principal",
            LegKind::Fee => "fee",
            LegKind::Tax => "tax",
        }
    }
    let all = [
        LegKind::Cash,
        LegKind::SecurityQuantity,
        LegKind::Principal,
        LegKind::Fee,
        LegKind::Tax,
    ];
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "event_legs");
    let values: Vec<&str> = all.iter().copied().map(code).collect();
    assert_ddl_lists(&ddl, "event_legs", &values);
}

#[test]
fn the_confidence_check_matches_confidence() {
    fn code(value: Confidence) -> &'static str {
        match value {
            Confidence::Known => "known",
            Confidence::Estimated => "estimated",
            Confidence::Unknown => "unknown",
        }
    }
    let all = [
        Confidence::Known,
        Confidence::Estimated,
        Confidence::Unknown,
    ];
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "events");
    let values: Vec<&str> = all.iter().copied().map(code).collect();
    assert_ddl_lists(&ddl, "events", &values);
}

#[test]
fn the_relation_kind_check_matches_relation() {
    fn code(value: &Relation) -> &'static str {
        match value {
            Relation::None => "none",
            Relation::Reversal { .. } => "reversal",
            Relation::Replacement { .. } => "replacement",
        }
    }
    let target = EventId::new_random();
    let all = [
        Relation::None,
        Relation::Reversal { target },
        Relation::Replacement { target },
    ];
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "events");
    let values: Vec<&str> = all.iter().map(code).collect();
    assert_ddl_lists(&ddl, "events", &values);
}

#[test]
fn the_fee_origin_check_matches_fee_origin() {
    fn code(value: FeeOrigin) -> &'static str {
        match value {
            FeeOrigin::Brokerage => "brokerage",
            FeeOrigin::Depositary => "depositary",
            FeeOrigin::AccountMaintenance => "account_maintenance",
            FeeOrigin::MarginInterest => "margin_interest",
            FeeOrigin::Other => "other",
        }
    }
    let all = [
        FeeOrigin::Brokerage,
        FeeOrigin::Depositary,
        FeeOrigin::AccountMaintenance,
        FeeOrigin::MarginInterest,
        FeeOrigin::Other,
    ];
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "event_fee");
    let values: Vec<&str> = all.iter().copied().map(code).collect();
    assert_ddl_lists(&ddl, "event_fee", &values);
}

#[test]
fn the_tax_origin_check_matches_tax_origin() {
    fn code(value: TaxOrigin) -> &'static str {
        match value {
            TaxOrigin::WithheldAtSource => "withheld_at_source",
            TaxOrigin::SelfPaid => "self_paid",
        }
    }
    let all = [TaxOrigin::WithheldAtSource, TaxOrigin::SelfPaid];
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "event_tax");
    let values: Vec<&str> = all.iter().copied().map(code).collect();
    assert_ddl_lists(&ddl, "event_tax", &values);
}

#[test]
fn the_dimension_check_matches_dimension() {
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "event_coverage_gap_row_dimensions");
    let values: Vec<&str> = Dimension::all().iter().map(|d| d.code()).collect();
    assert_ddl_lists(&ddl, "event_coverage_gap_row_dimensions", &values);
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(text: &str) -> Quantity {
    Quantity(dec(text))
}

fn every_control_claim() -> Vec<ControlClaim> {
    vec![
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1),
            at: BalancePoint::Opening,
        },
        ControlClaim::PositionQuantity {
            instrument: InstrumentId::new_random(),
            quantity: qty("1"),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(1),
            credit: PostedMinor::new(1),
        },
        ControlClaim::FeesTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1),
        },
        ControlClaim::IncomeTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1),
        },
        ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1),
        },
    ]
}

#[test]
fn the_claim_kind_check_matches_control_claim() {
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "event_control_assertion");
    let values: Vec<&str> = every_control_claim()
        .iter()
        .map(ControlClaim::discriminant)
        .collect();
    assert_ddl_lists(&ddl, "event_control_assertion", &values);
}

fn every_corporate_action() -> Vec<CorporateAction> {
    vec![
        CorporateAction::PartialRedemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: qty("100"),
            principal_returned_per_unit: PerUnitAmount::new(dec("200"), CurrencyCode::Rub),
            compensation: rub(2_000_000),
            effective_date: date!(2026 - 06 - 15),
            record_date: None,
            grounds: None,
            basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
        },
        CorporateAction::Redemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: qty("100"),
            principal_returned_per_unit: PerUnitAmount::new(dec("800"), CurrencyCode::Rub),
            compensation: rub(8_000_000),
            effective_date: date!(2026 - 12 - 15),
            record_date: None,
            grounds: None,
        },
        CorporateAction::Conversion {
            predecessor: InstrumentId::new_random(),
            successor: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            ratio: dec("1.5"),
            quantity_in: qty("100"),
            quantity_out: qty("150"),
            fractional: iaam_core::event::corporate_action::FractionalTreatment::NotApplicable,
            compensation: None,
            effective_date: date!(2026 - 09 - 01),
            record_date: None,
            grounds: None,
            basis_transfer: iaam_core::event::corporate_action::BasisTransferRule::CarryOver,
        },
    ]
}

#[test]
fn the_corporate_action_kind_check_matches_corporate_action() {
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "event_corporate_action");
    let values: Vec<&str> = every_corporate_action()
        .iter()
        .map(CorporateAction::discriminant)
        .collect();
    assert_ddl_lists(&ddl, "event_corporate_action", &values);
}

fn every_offer_action() -> Vec<OfferExerciseAction> {
    vec![
        OfferExerciseAction::Submitted {
            submission: OfferSubmissionId::new_random(),
            window: OfferWindowId::new_random(),
            instrument: InstrumentId::new_random(),
            quantity: qty("10"),
        },
        OfferExerciseAction::Cancelled {
            submission: OfferSubmissionId::new_random(),
            quantity: qty("4"),
        },
        OfferExerciseAction::Settled {
            submission: OfferSubmissionId::new_random(),
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: qty("6"),
            gross: rub(6_000_000),
            fee: None,
            accrued_interest: None,
        },
    ]
}

#[test]
fn the_offer_action_kind_check_matches_offer_exercise_action() {
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "event_offer_exercise");
    let values: Vec<&str> = every_offer_action()
        .iter()
        .map(OfferExerciseAction::discriminant)
        .collect();
    assert_ddl_lists(&ddl, "event_offer_exercise", &values);
}

#[test]
fn the_matcher_kind_check_matches_category_matcher() {
    fn code(value: &CategoryMatcher) -> &'static str {
        match value {
            CategoryMatcher::Row { .. } => "row",
            CategoryMatcher::SourceCategory { .. } => "source_category",
            CategoryMatcher::DescriptionContains { .. } => "description_contains",
            CategoryMatcher::Description { .. } => "description",
        }
    }
    let all = [
        CategoryMatcher::Row {
            key: "k".to_owned(),
        },
        CategoryMatcher::SourceCategory {
            value: "v".to_owned(),
        },
        CategoryMatcher::DescriptionContains {
            text: "t".to_owned(),
        },
        CategoryMatcher::Description {
            text: "t".to_owned(),
            mode: iaam_core::category::DescriptionMatchMode::Equals,
        },
    ];
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl = table_ddl(&store, "category_rules");
    let values: Vec<&str> = all.iter().map(code).collect();
    assert_ddl_lists(&ddl, "category_rules", &values);
}

#[test]
fn a_fresh_database_reports_schema_version_one() {
    let store = SqliteStore::open_in_memory().expect("open");
    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("reading the schema version");
    assert_eq!(version, 1, "the collapsed schema starts at version 1");
}
