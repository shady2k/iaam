//! Properties with their domains of applicability (§15.3).
//!
//! Each property includes a qualification specifying where it applies.
//! Properties without a domain cause false failures, which are most
//! easily addressed by weakening the generator into a tautology.
//!
//! The following are **intentionally absent** and must not be added:
//! - concatenating periods for XIRR: IRR is not chainable, so no such property exists;
//! - scaling all amounts when taxes are enabled: progressive
//!   tax brackets, thresholds, and minimum fees violate this property;
//! - shifting dates under tax rules: this changes the day-count basis,
//!   tax year, and long-term ownership exemption.

use iaam_core::dates::TradeDate;
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::AliasInterval;
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::rules::lot_disposal::{DisposalInput, FifoV1, Lot, LotDisposalRule, LotId};
use iaam_core::rules::quotation::QuotationRule;
use proptest::prelude::*;
use rust_decimal::Decimal;
use time::macros::date;

fn lot_strategy() -> impl Strategy<Value = (i64, i64)> {
    // Quantity 1..=1000, basis 1..=100_000_000 minor units.
    (1_i64..=1_000, 1_i64..=100_000_000)
}

/// Allocation ties are rounded to even, and the basis is preserved.
/// This is a deterministic counterpart to the property: `proptest` may
/// never reach this case.
#[test]
fn tie_rounding_preserves_total_basis() {
    let instrument = InstrumentId::new_random();
    let lots = vec![Lot {
        id: LotId::new_random(),
        instrument,
        acquired: None,
        quantity: Quantity(Dec::new(Decimal::from(2))),
        cost_basis: Money::new(PostedMinor::new(5), CurrencyCode::Rub),
        acquisition_basis: None,
        accrued_interest_paid: None,
        received_to_date: None,
    }];

    let out = FifoV1
        .apply(&DisposalInput {
            lots,
            quantity: Quantity(Dec::new(Decimal::from(1))),
        })
        .expect("one of the two units is available");

    // 5 * 1 / 2 = 2.5 — a tie, rounded to even, i.e. to 2.
    assert_eq!(out.basis_released.amount().raw(), 2);
    assert_eq!(out.remaining[0].cost_basis.amount().raw(), 3);
    assert_eq!(
        out.basis_released.amount().raw() + out.remaining[0].cost_basis.amount().raw(),
        5,
        "total lot basis must be preserved"
    );
}

proptest! {
        /// Domain: any set of lots in one currency, any valid quantity.
        /// The invariant is exact — rounding is allocated so that the total
        /// lot basis is preserved (§6.6).
    ///
        /// What this property **does not** catch: an error in the allocation itself. The remaining
        /// portion is calculated as `cost_basis - taken`, using the same value
        /// returned by `split_basis`; their sum equals the original basis for
        /// any result it returns. Verified: a `split_basis` that returns
        /// `value + 1` leaves the property passing for 200 000 cases.
        /// The allocation amount is checked by `tie_rounding_preserves_total_basis`
        /// and the `rules::lot_disposal` module tests. This test checks something else:
        /// that no lot is lost or counted twice during the transition from
        /// `lots` to `disposed` and `remaining`.
    #[test]
    fn released_plus_remaining_equals_original_basis(
        raw_lots in prop::collection::vec(lot_strategy(), 1..8),
        sell_fraction in 0_u32..=100,
    ) {
        let instrument = InstrumentId::new_random();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: Some(TradeDate(date!(2026 - 01 - 01))),
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                accrued_interest_paid: None,
                received_to_date: None,
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
                acquisition_basis: None,
            })
            .collect();

        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let total_basis: i64 = raw_lots.iter().map(|(_, b)| *b).sum();
            // Integer division: the percentage does not exceed 100, so the result
            // never exceeds the available quantity.
        let sell_qty = total_qty * i64::from(sell_fraction) / 100;

        let out = FifoV1
            .apply(&DisposalInput {
                lots,
                quantity: Quantity(Dec::new(Decimal::from(sell_qty))),
            })
                .expect("quantity does not exceed the available amount");

        let remaining_basis: i64 =
            out.remaining.iter().map(|l| l.cost_basis.amount().raw()).sum();

        prop_assert_eq!(
            out.basis_released.amount().raw() + remaining_basis,
            total_basis,
                "disposed and remaining basis must sum to the original basis"
        );
    }

        /// Domain: any valid quantity. The disposed quantity
        /// equals the requested quantity — no more and no less.
    #[test]
    fn disposed_quantity_equals_requested(
        raw_lots in prop::collection::vec(lot_strategy(), 1..8),
        sell_fraction in 0_u32..=100,
    ) {
        let instrument = InstrumentId::new_random();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: None,
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
                acquisition_basis: None,
                accrued_interest_paid: None,
                received_to_date: None,
            })
            .collect();

        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let sell_qty = total_qty * i64::from(sell_fraction) / 100;

        let out = FifoV1
            .apply(&DisposalInput {
                lots,
                quantity: Quantity(Dec::new(Decimal::from(sell_qty))),
            })
                .expect("quantity does not exceed the available amount");

        let disposed: Decimal = out.disposed.iter().map(|d| d.quantity.0.inner()).sum();
        prop_assert_eq!(disposed, Decimal::from(sell_qty));
    }

    /// Domain: any quantities exceeding what is available.
    /// Reject rather than produce a negative balance.
    #[test]
    fn overselling_always_errors(
        raw_lots in prop::collection::vec(lot_strategy(), 1..5),
        excess in 1_i64..=1_000,
    ) {
        let instrument = InstrumentId::new_random();
        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: None,
                accrued_interest_paid: None,
                received_to_date: None,
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
                acquisition_basis: None,
            })
            .collect();

        let out = FifoV1.apply(&DisposalInput {
            lots,
            quantity: Quantity(Dec::new(Decimal::from(total_qty + excess))),
        });
        prop_assert!(out.is_err());
    }
}
mod projection_properties {
    use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
    use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use iaam_core::projection::{ProjectionContext, project};
    use iaam_core::rules::{LotRuleVersion, RuleRegistry};
    use proptest::prelude::*;
    use time::macros::date;

    fn deposit(account: AccountId, sequence: u32, minor: i64) -> Event {
        let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
        let day = date!(2025 - 01 - 01) + time::Duration::days(i64::from(sequence));
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"e".repeat(64)).unwrap(),
                ParserVersion("prop/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    proptest! {
        /// Domain: always (§4.8). Ordering is defined by `EffectiveOrder`,
        /// not by file loading order.
        #[test]
        fn import_order_never_changes_the_projection(
            amounts in prop::collection::vec(1_i64..1_000_000, 1..12),
            rotation in 0_usize..12,
        ) {
            let account = AccountId::new_random();
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [account],
            );
            let rules = RuleRegistry::with_defaults();
            let ctx = ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            };

            let events: Vec<Event> = amounts
                .iter()
                .enumerate()
                .map(|(i, minor)| {
                    let index = u32::try_from(i).unwrap_or(u32::MAX);
                    deposit(account, index + 1, *minor)
                })
                .collect();

            let mut rotated = events.clone();
            let shift = rotation % events.len().max(1);
            rotated.rotate_left(shift);

            prop_assert_eq!(
                project(&events, &ctx).unwrap().snapshot().fingerprint(),
                project(&rotated, &ctx).unwrap().snapshot().fingerprint()
            );
        }

        /// Domain: always. A reversal together with the original event leaves no
        /// trace in either balances or flows.
        #[test]
        fn an_event_and_its_reversal_leave_no_trace(minor in 1_i64..1_000_000) {
            let account = AccountId::new_random();
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [account],
            );
            let rules = RuleRegistry::with_defaults();
            let ctx = ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            };

            let original = deposit(account, 1, minor);
            let mut reversal = deposit(account, 2, minor);
            reversal.relation = Relation::Reversal { target: original.id };

            let projection = project(&[original, reversal], &ctx).unwrap();
            prop_assert!(projection.state().flows().external().is_empty());
            prop_assert_eq!(
                projection.state().balances().cash(account, CurrencyCode::Rub),
                None
            );
        }
    }
}

proptest! {
    /// With the quote, quantity, and exchange rate fixed, the value
    /// of a percentage-basis position is linear in its outstanding
    /// face value. Price and amortization change between dates, so without
    /// this qualification the property would not hold.
    #[test]
    fn value_is_linear_in_the_remaining_face_at_a_fixed_quote(
        quote in 1_i64..20_000,
        face in 1_i64..1_000_000,
        multiplier in 2_i64..10,
    ) {
        let value_at = |remaining_face: i64| {
            let quote = Dec::new(Decimal::from(quote));
            let face = iaam_core::money::PerUnitAmount::new(
                Dec::new(Decimal::from(remaining_face)),
                CurrencyCode::Rub,
            );
            let (money_per_unit, _) = iaam_core::rules::quotation::QuotationV1
                .money_per_unit(
                    iaam_core::valuation::QuotationBasis::PercentOfRemainingFace,
                    quote,
                    CurrencyCode::Rub,
                    Some(face),
                )
                .expect("outstanding face value is fixed");
            money_per_unit
                .checked_mul(Dec::new(Decimal::from(7)))
                .expect("fixed quantity fits in Decimal")
        };

        let single = value_at(face);
        let scaled = value_at(face * multiplier);
        let factor = Dec::new(Decimal::from(multiplier));
        prop_assert_eq!(
            scaled,
            single.checked_mul(factor).unwrap(),
            "with fixed inputs, value scales with face value"
        );
    }
}

proptest! {
    /// Code parsing does not infer a kind: any string that does not match
    /// a variant code must return None.
    #[test]
    fn an_arbitrary_string_is_never_mistaken_for_a_kind(text in r"\PC{0,16}") {
        let parsed = iaam_core::instrument::InstrumentKind::from_code(&text);
        let expected = iaam_core::instrument::InstrumentKind::ALL
            .into_iter()
            .find(|kind| kind.code() == text);
        prop_assert_eq!(parsed, expected);
    }
}

proptest! {
    /// Among non-overlapping intervals, any date is covered by at most
    /// one.
    ///
    /// This is the mathematical substance of the resolution uniqueness property
    /// (E3.1): the resolver must return one instrument or none,
    /// but never two. This is tested here rather than in `iaam-store`, because
    /// the database has no bearing on the claim: uniqueness follows from
    /// the geometry of half-open intervals, while the storage layer is only required to
    /// use it. That it actually does so is verified by
    /// the boundary test `a_code_never_resolves_to_two_instruments`
    /// in `crates/iaam-store/tests/instrument_directory.rs`.
    ///
    /// **Applicability.** Intervals are non-overlapping
    /// by construction—as pairs formed from sorted distinct boundaries,
    /// with gaps between pairs. Overlapping intervals do not
    /// satisfy this property, and this is not a flaw in the property: they are prohibited by the
    /// `instrument_aliases_do_not_overlap` trigger in the schema, not by arithmetic.
    #[test]
    fn at_most_one_of_several_disjoint_intervals_covers_any_day(
        bounds in prop::collection::vec(0_i64..3_000, 2..12),
        probe in -100_i64..3_100,
    ) {
        let origin = date!(2020 - 01 - 01);
        let day = |offset: i64| {
            origin
                .checked_add(time::Duration::days(offset))
                .expect("date is within the calendar range")
        };

        let mut sorted = bounds;
        sorted.sort_unstable();

        // A chain of ADJACENT intervals: [b0, b1), [b1, b2), [b2, b3), …
        //
        // This is exactly what an alias history looks like: an ISIN change closes
        // the old interval and opens the new one on the same date. Only in
        // this shape can the property fail—with an inclusive
        // end, every internal boundary would belong to two
        // intervals at once. Disjoint pairs with gaps are unsuitable here:
        // a junction between them would require a random
        // match between two numbers, i.e. would occur practically never, and
        // the property would silently degenerate into a tautology.
        //
        // Degenerate segments (b_i == b_{i+1}) are discarded: an empty
        // interval is prohibited by a CHECK constraint in the schema and covers the empty
        // set.
        let intervals: Vec<AliasInterval> = sorted
            .windows(2)
            .filter(|pair| pair[0] < pair[1])
            .map(|pair| AliasInterval {
                valid_from: day(pair[0]),
                valid_to: Some(day(pair[1])),
            })
            .collect();
        prop_assume!(!intervals.is_empty());

        // ALL chain boundaries are tested, not just a random date.
        // A random probe lands exactly on a boundary in about one
        // case out of three hundred, so it detects an off-by-one only
        // occasionally—which means it does not. A boundary is exactly where
        // intervals can overlap.
        let mut probes: Vec<i64> = sorted.clone();
        probes.push(probe);

        for point in probes {
            let covering = intervals
                .iter()
                .filter(|interval| interval.covers(day(point)))
                .count();
            prop_assert!(
                covering <= 1,
                "day {point} is covered by {covering} intervals in {intervals:?}"
            );
        }
    }
}
