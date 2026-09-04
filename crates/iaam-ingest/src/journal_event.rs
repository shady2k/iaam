//! Ingestion of journal facts: corporate actions and offers
//! (§4.7, §3.5).
//!
//! A separate entry point, rather than new `OperationKind` members, for
//! a mechanical, not stylistic, reason: [`crate::operation::OperationDates`]
//! hard-codes `entitlement: None`, meaning the operational date model
//! cannot express a record date at all. For a corporate action, it is
//! part of the fact. Extending the operations would require either
//! distorting the shared date model or carrying dates in two places at once.
//!
//! An offer belongs here alongside corporate actions as a **neighbor**,
//! not as a member of the family: `event/offer.rs` establishes that an offer is
//! the holder's right, not the issuer's decision. What they share is the
//! ingestion channel and journal, not the nature of the fact.
//!
//! As with operations, **ingestion constructs the signs and legs**: the client sends
//! positive quantities, while the negative quantity of the departing security
//! and the cash settlement total are calculated here.

use iaam_core::dates::{EffectiveOrder, EntitlementDate, EventDates, SettledDate, TradeDate};
use iaam_core::event::allocation::BasisAllocation;
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::offer::OfferExerciseAction;
use iaam_core::event::provenance::Provenance;
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};

use iaam_core::ids::{AccountId, EventId};
use iaam_core::money::{Money, Quantity};
use serde::{Deserialize, Serialize};
use time::Date;

use crate::operation::{NormalizationContext, Normalized};
use crate::verdict::Rejection;

/// Journal-fact parsing version.
///
/// Its own, rather than shared with operations: the origin must name the
/// parsing that built the event. One version for two dissimilar parsers would
/// not distinguish one parser's error from the other's (§4.1).
pub const PARSER_VERSION: &str = "ingest/journal/1";

/// A journal fact received through the API.
///
/// Two families under one roof mean a shared ingestion channel, not a shared
/// Nature: a corporate action is decided by the issuer, while an offer is submitted by
/// the holder. Accepting an arbitrary `EventKind` here is intentionally impossible:
/// the input accepts exactly the families listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JournalFact {
    /// Dates within the fact itself: for a corporate action, the effective date
    /// is part of its identity, not a property of its submission.
    CorporateAction(CorporateAction),
    /// An offer has no date of its own: `event/offer.rs` describes the
    /// request and settlement, but not the day it occurred. Therefore, the day
    /// is supplied by the client—there is nothing for acceptance to invent.
    OfferExercise {
        action: OfferExerciseAction,
        day: Date,
    },
}

/// Journal event submitted for acceptance.
///
/// There are no signs or legs here: acceptance builds them (see the module
/// comment); the client sends only positive amounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmittedJournalEvent {
    pub account: AccountId,
    pub fact: JournalFact,
    /// Client idempotency key (§10.6).
    pub idempotency_key: Option<String>,
    /// Identifier of the fact in the source, if present.
    pub source_operation_id: Option<String>,
}

/// Additional value calculated by the application before normalization.
///
/// Acceptance knows neither the schedule nor the storage: it receives only the prepared
/// fraction and stores it in amortization.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalEventEnrichment {
    pub basis_allocation: BasisAllocation,
}

/// Conversion of a journal fact into a journal event.
///
/// The event form—which legs each member must have—is checked by
/// the core via `validate_structure()` at the common acceptance boundary, not by this
/// normalizer. A duplicate check would silently diverge from the core.
pub fn normalize_journal_event(
    submitted: &SubmittedJournalEvent,
    enrichment: &JournalEventEnrichment,
    context: &NormalizationContext,
) -> Result<Normalized, Rejection> {
    let (dates, day) = dates_of(&submitted.fact);
    let (kind, legs) = build(submitted.account, &submitted.fact, enrichment)?;
    // The fingerprint is computed in the same place as deduplication: a second instance
    // of this function would silently diverge from the first (§10.6).
    let raw_hash = crate::dedup::fingerprint_journal_event(submitted);

    Ok(Normalized {
        event: Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: context.owner,
            account: submitted.account,
            kind,
            dates,
            // Temporary number: the final one is assigned by storage (§4.8).
            order: EffectiveOrder::new(day, 1),
            legs,
            provenance: {
                // From the context, as on the operation path: what read a
                // fact is the caller's to state, and a constant here would be
                // a second place saying it.
                let base =
                    Provenance::new(context.source, raw_hash, context.parser_version.clone());
                match submitted.source_operation_id.as_deref() {
                    Some(id) => base.with_source_operation_id(id),
                    None => base,
                }
            },
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: submitted.idempotency_key.clone(),
        },
    })
}

/// Event dates and the day on which it enters the ordering.
///
/// The day is returned alongside the dates rather than fetched later through
/// `effective_date()`: both families have it by construction, and the “no dates”
/// failure would be an unreachable branch.
fn dates_of(fact: &JournalFact) -> (EventDates, Date) {
    match fact {
        // The effective date is the day on which the fact occurred
        // on the account: the face value decreased, the security left, or the replacement
        // occurred. The registry record date goes in `entitlement`:
        // the input is kept separate for it from the operations.
        JournalFact::CorporateAction(action) => {
            let day = action.effective_date();
            (
                EventDates {
                    settled: Some(SettledDate(day)),
                    entitlement: action.record_date().map(EntitlementDate),
                    ..EventDates::empty()
                },
                day,
            )
        }
        // Submission and withdrawal move nothing, so their day is `trade`,
        // the owner's action, not settlement, which did not occur.
        JournalFact::OfferExercise {
            action: OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. },
            day,
        } => (
            EventDates {
                trade: Some(TradeDate(*day)),
                ..EventDates::empty()
            },
            *day,
        ),
        JournalFact::OfferExercise {
            action: OfferExerciseAction::Settled { .. },
            day,
        } => (
            EventDates {
                settled: Some(SettledDate(*day)),
                ..EventDates::empty()
            },
            *day,
        ),
    }
}

/// Construct the event type and legs.
///
/// The dispatcher is exhaustive: a new family member must break compilation.
fn build(
    account: AccountId,
    fact: &JournalFact,
    enrichment: &JournalEventEnrichment,
) -> Result<(EventKind, Vec<Leg>), Rejection> {
    let legs = match fact {
        JournalFact::CorporateAction(action) => corporate_action_legs(account, action)?,
        JournalFact::OfferExercise { action, .. } => offer_legs(account, action)?,
    };
    let kind = match fact {
        JournalFact::CorporateAction(action) => {
            let mut action = action.clone();
            if let CorporateAction::PartialRedemption {
                basis_allocation, ..
            } = &mut action
            {
                *basis_allocation = enrichment.basis_allocation.clone();
            }
            EventKind::CorporateAction { action }
        }
        JournalFact::OfferExercise { action, .. } => EventKind::OfferExercise {
            action: action.clone(),
        },
    };
    Ok((kind, legs))
}

fn corporate_action_legs(
    account: AccountId,
    action: &CorporateAction,
) -> Result<Vec<Leg>, Rejection> {
    match action {
        // One `Principal` leg and no security legs: the number of securities
        // is unchanged by amortization (§6.5). There is no “Cash + Principal” pair —
        // `Principal` is already included in the monetary effect, and the pair would double
        // the inflow.
        CorporateAction::PartialRedemption {
            instrument,
            compensation,
            ..
        } => Ok(vec![Leg::principal(account, *instrument, *compensation)]),
        CorporateAction::Redemption {
            instrument,
            custody,
            quantity,
            compensation,
            ..
        } => Ok(vec![
            Leg::principal(account, *instrument, *compensation),
            Leg::security(account, *custody, *instrument, retired(*quantity)?),
        ]),
        CorporateAction::Conversion {
            predecessor,
            successor,
            custody,
            quantity_in,
            quantity_out,
            compensation,
            ..
        } => {
            let mut legs = vec![
                Leg::security(account, *custody, *predecessor, retired(*quantity_in)?),
                Leg::security(account, *custody, *successor, *quantity_out),
            ];
            if let Some(compensation) = compensation {
                legs.push(Leg::cash(account, *compensation));
            }
            Ok(legs)
        }
    }
}

fn offer_legs(account: AccountId, action: &OfferExerciseAction) -> Result<Vec<Leg>, Rejection> {
    match action {
        // There are no legs — and this is a form, not their absence due to oversight.
        OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => {
            Ok(Vec::new())
        }
        // Redemption: the security leaves in exchange for cash. There is no `Principal` leg —
        // the principal is not returned; the security is redeemed.
        OfferExerciseAction::Settled {
            instrument,
            custody,
            quantity,
            gross,
            fee,
            accrued_interest,
            ..
        } => Ok(vec![
            Leg::cash(account, settlement(*gross, *fee, *accrued_interest)?),
            Leg::security(account, *custody, *instrument, retired(*quantity)?),
        ]),
    }
}

/// Redemption cash total: the fee reduces the proceeds, while accrued
/// coupon increases them—the same arithmetic as for a sale.
///
/// Add `Money`, not minor units: adding money in different currencies
/// must fail, whereas adding bare `i64` values would silently succeed.
fn settlement(
    gross: Money,
    fee: Option<Money>,
    accrued: Option<Money>,
) -> Result<Money, Rejection> {
    let mut total = gross;
    if let Some(accrued) = accrued {
        total = total.try_add(accrued).map_err(|error| Rejection {
            field: "accrued_interest".into(),
            expected: "a sum in the redemption currency yielding a representable result".into(),
            actual: error.to_string(),
        })?;
    }
    if let Some(fee) = fee {
        total = total.try_sub(fee).map_err(|error| Rejection {
            field: "fee".into(),
            expected: "a sum in the redemption currency yielding a representable result".into(),
            actual: error.to_string(),
        })?;
    }
    Ok(total)
}

/// Quantity of the security being disposed of. The client sends a positive value —
/// the ingestion layer supplies the sign.
///
/// The error is propagated, although no `Decimal` currently produces one
/// (`checked_neg` is `0 - self`, and the result is always representable for `Decimal`):
/// `unwrap` here would assert that this remains true, which this function
/// cannot know. The same approach as in `operation.rs`
/// for a sale.
fn retired(quantity: Quantity) -> Result<Quantity, Rejection> {
    quantity
        .0
        .checked_neg()
        .map(Quantity)
        .map_err(|error| Rejection {
            field: "quantity".into(),
            expected: "representable quantity".into(),
            actual: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::event::allocation::{AllocationGap, BasisAllocation};
    use iaam_core::event::corporate_action::{
        BasisTransferRule, CorporateAction, FractionalTreatment,
    };
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
    use iaam_core::event::provenance::ParserVersion;
    use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn qty(text: &str) -> Quantity {
        Quantity(dec(text))
    }

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn context() -> NormalizationContext {
        NormalizationContext {
            owner: OwnerId::new_random(),
            source: SourceId::new_random(),
            parser_version: ParserVersion(PARSER_VERSION.to_owned()),
        }
    }

    fn submitted(fact: JournalFact) -> SubmittedJournalEvent {
        SubmittedJournalEvent {
            account: AccountId::new_random(),
            fact,
            idempotency_key: None,
            source_operation_id: None,
        }
    }

    fn partial_redemption() -> CorporateAction {
        CorporateAction::PartialRedemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: qty("10"),
            principal_returned_per_unit: PerUnitAmount::new(dec("100"), CurrencyCode::Rub),
            compensation: rub(100_000),
            effective_date: date!(2026 - 05 - 20),
            record_date: Some(date!(2026 - 05 - 18)),
            grounds: None,
            basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
        }
    }

    /// The event's shape is checked by the core, not ingestion: the normalizer must
    /// construct exactly the legs the core expects, otherwise the record will be rejected
    /// later—at the common boundary.
    fn normalized_and_valid(fact: JournalFact) -> iaam_core::event::Event {
        let event = normalize_journal_event(
            &submitted(fact),
            &JournalEventEnrichment::default(),
            &context(),
        )
        .expect("normalization must succeed")
        .event;
        event
            .validate_structure()
            .expect("legs must match the shape expected by the core");
        event
    }

    #[test]
    fn an_amortisation_pays_money_and_leaves_the_quantity_alone() {
        let event = normalized_and_valid(JournalFact::CorporateAction(partial_redemption()));
        assert_eq!(event.legs.len(), 1, "{:?}", event.legs);
        assert!(
            matches!(event.kind, EventKind::CorporateAction { .. }),
            "{:?}",
            event.kind
        );
    }

    /// The very field for which a separate input was introduced: operations
    /// have no way to express it.
    #[test]
    fn the_record_date_reaches_the_entitlement_date() {
        let event = normalized_and_valid(JournalFact::CorporateAction(partial_redemption()));
        assert_eq!(
            event.dates.entitlement.map(|day| day.0),
            Some(date!(2026 - 05 - 18)),
            "the registry fixing date must reach the event"
        );
    }

    #[test]
    fn a_corporate_action_is_dated_by_the_day_it_takes_effect() {
        let event = normalized_and_valid(JournalFact::CorporateAction(partial_redemption()));
        assert_eq!(
            event.dates.effective_date(),
            Some(date!(2026 - 05 - 20)),
            "without a date, the event will not fall into any period"
        );
    }

    #[test]
    fn a_redemption_retires_the_security() {
        let event =
            normalized_and_valid(JournalFact::CorporateAction(CorporateAction::Redemption {
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: qty("10"),
                principal_returned_per_unit: PerUnitAmount::new(dec("1000"), CurrencyCode::Rub),
                compensation: rub(1_000_000),
                effective_date: date!(2026 - 06 - 01),
                record_date: None,
                grounds: None,
            }));
        assert_eq!(event.legs.len(), 2, "{:?}", event.legs);
    }

    #[test]
    fn a_conversion_swaps_the_predecessor_for_the_successor() {
        let event =
            normalized_and_valid(JournalFact::CorporateAction(CorporateAction::Conversion {
                predecessor: InstrumentId::new_random(),
                successor: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                ratio: dec("1"),
                quantity_in: qty("10"),
                quantity_out: qty("10"),
                fractional: FractionalTreatment::NotApplicable,
                compensation: None,
                effective_date: date!(2026 - 07 - 01),
                record_date: None,
                grounds: None,
                basis_transfer: BasisTransferRule::CarryOver,
            }));
        assert_eq!(event.legs.len(), 2, "{:?}", event.legs);
    }

    #[test]
    fn a_cash_compensated_fraction_adds_a_cash_leg() {
        let event =
            normalized_and_valid(JournalFact::CorporateAction(CorporateAction::Conversion {
                predecessor: InstrumentId::new_random(),
                successor: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                ratio: dec("1.5"),
                quantity_in: qty("11"),
                quantity_out: qty("16"),
                fractional: FractionalTreatment::CashCompensated,
                compensation: Some(rub(5_000)),
                effective_date: date!(2026 - 07 - 01),
                record_date: None,
                grounds: None,
                basis_transfer: BasisTransferRule::CarryOver,
            }));
        assert_eq!(event.legs.len(), 3, "{:?}", event.legs);
    }

    /// Submitting an order moves nothing—and the absence of legs is checked
    /// just like their presence.
    #[test]
    fn an_offer_application_moves_neither_money_nor_securities() {
        let event = normalized_and_valid(JournalFact::OfferExercise {
            action: OfferExerciseAction::Submitted {
                submission: OfferSubmissionId::new_random(),
                window: OfferWindowId::new_random(),
                instrument: InstrumentId::new_random(),
                quantity: qty("5"),
            },
            day: date!(2026 - 04 - 10),
        });
        assert!(event.legs.is_empty(), "{:?}", event.legs);
        assert_eq!(event.dates.effective_date(), Some(date!(2026 - 04 - 10)));
    }

    #[test]
    fn a_cancelled_application_is_a_fact_of_its_own() {
        let event = normalized_and_valid(JournalFact::OfferExercise {
            action: OfferExerciseAction::Cancelled {
                submission: OfferSubmissionId::new_random(),
                quantity: qty("5"),
            },
            day: date!(2026 - 04 - 12),
        });
        assert!(event.legs.is_empty(), "{:?}", event.legs);
    }

    /// An offer calculation is a disposal of the security for cash: the fee
    /// reduces the proceeds, while accrued coupon increases them. The sign
    /// of the quantity is set by ingestion, not by the client.
    #[test]
    fn a_settled_offer_pays_gross_less_fee_plus_accrued_interest() {
        let event = normalized_and_valid(JournalFact::OfferExercise {
            action: OfferExerciseAction::Settled {
                submission: OfferSubmissionId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: qty("5"),
                gross: rub(500_000),
                fee: Some(rub(1_000)),
                accrued_interest: Some(rub(2_000)),
            },
            day: date!(2026 - 04 - 20),
        });
        let cash = event
            .legs
            .iter()
            .find_map(iaam_core::event::leg::Leg::cash_effect)
            .expect("the calculation must move money");
        assert_eq!(cash.amount().raw(), 501_000, "500000 - 1000 + 2000");
        // Without a date, the calculation will not fall into any period: the money arrived
        // and the security disappeared into nowhere.
        assert_eq!(event.dates.effective_date(), Some(date!(2026 - 04 - 20)));
    }

    /// A fingerprint must DISTINGUISH facts, not merely match itself:
    /// a fixed canonical form gives everything in the world one fingerprint,
    /// and deduplication will declare anything a duplicate (§10.6).
    #[test]
    fn two_different_facts_get_two_different_fingerprints() {
        let account = AccountId::new_random();
        let one = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(partial_redemption()),
            idempotency_key: None,
            source_operation_id: None,
        };
        let mut other = partial_redemption();
        if let CorporateAction::PartialRedemption { compensation, .. } = &mut other {
            *compensation = rub(200_000);
        }
        let two = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(other),
            idempotency_key: None,
            source_operation_id: None,
        };
        assert_ne!(
            crate::dedup::fingerprint_journal_event(&one),
            crate::dedup::fingerprint_journal_event(&two),
            "two different payouts must produce different fingerprints"
        );
    }

    /// The account is part of the fingerprint: the same fact on another account is a different
    /// fact, and they must not merge.
    #[test]
    fn the_same_fact_on_another_account_is_another_fingerprint() {
        let fact = partial_redemption();
        let one = SubmittedJournalEvent {
            account: AccountId::new_random(),
            fact: JournalFact::CorporateAction(fact.clone()),
            idempotency_key: None,
            source_operation_id: None,
        };
        let two = SubmittedJournalEvent {
            account: AccountId::new_random(),
            fact: JournalFact::CorporateAction(fact),
            idempotency_key: None,
            source_operation_id: None,
        };
        assert_ne!(
            crate::dedup::fingerprint_journal_event(&one),
            crate::dedup::fingerprint_journal_event(&two)
        );
    }

    /// Zero compensation is not “zero amortization” but a source defect.
    /// The rejection must occur before writing: the journal is append-only.
    #[test]
    fn a_zero_compensation_is_refused_and_never_becomes_cash() {
        let event = normalize_journal_event(
            &submitted(JournalFact::CorporateAction(
                CorporateAction::PartialRedemption {
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: qty("10"),
                    principal_returned_per_unit: PerUnitAmount::new(dec("0"), CurrencyCode::Rub),
                    compensation: rub(0),
                    effective_date: date!(2026 - 05 - 20),
                    record_date: None,
                    grounds: None,
                    basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
                },
            )),
            &JournalEventEnrichment::default(),
            &context(),
        )
        .expect("form normalization is not checked")
        .event;
        assert!(
            event.validate_structure().is_err(),
            "a zero payout must be rejected"
        );
    }

    /// A fee in a different currency is not a “nearly correct” redemption: adding
    /// it to the amount would make acceptance record an inflow that never occurred.
    #[test]
    fn a_fee_in_another_currency_is_refused_instead_of_being_added() {
        let rejection = normalize_journal_event(
            &submitted(JournalFact::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: qty("5"),
                    gross: rub(500_000),
                    fee: Some(Money::new(PostedMinor::new(1_000), CurrencyCode::Usd)),
                    accrued_interest: None,
                },
                day: date!(2026 - 04 - 20),
            }),
            &JournalEventEnrichment::default(),
            &context(),
        )
        .expect_err("a redemption with a fee in a different currency must be rejected");
        assert_eq!(rejection.field, "fee");
    }

    #[test]
    fn accrued_interest_in_another_currency_is_refused_too() {
        let rejection = normalize_journal_event(
            &submitted(JournalFact::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: qty("5"),
                    gross: rub(500_000),
                    fee: None,
                    accrued_interest: Some(Money::new(PostedMinor::new(2_000), CurrencyCode::Usd)),
                },
                day: date!(2026 - 04 - 20),
            }),
            &JournalEventEnrichment::default(),
            &context(),
        )
        .expect_err("an accrued coupon in a different currency must be rejected");
        assert_eq!(rejection.field, "accrued_interest");
    }

    /// The fact identifier from the source reaches provenance: without
    /// it, there is no basis for reconciling with the broker export (§4.1).
    #[test]
    fn the_source_identifier_reaches_the_provenance() {
        let event = normalize_journal_event(
            &SubmittedJournalEvent {
                account: AccountId::new_random(),
                fact: JournalFact::CorporateAction(partial_redemption()),
                idempotency_key: None,
                source_operation_id: Some("амортизация-7".into()),
            },
            &JournalEventEnrichment::default(),
            &context(),
        )
        .expect("normalization must succeed")
        .event;
        assert_eq!(
            event.provenance.source_operation_id(),
            Some("амортизация-7")
        );
    }

    #[test]
    fn enrichment_is_stored_only_in_an_amortisation() {
        let expected = BasisAllocation::Unknown(AllocationGap::CurrencyMismatch);
        let event = normalize_journal_event(
            &submitted(JournalFact::CorporateAction(partial_redemption())),
            &JournalEventEnrichment {
                basis_allocation: expected.clone(),
            },
            &context(),
        )
        .expect("normalization must succeed")
        .event;
        let EventKind::CorporateAction { action } = event.kind else {
            panic!("a corporate action was expected")
        };
        let CorporateAction::PartialRedemption {
            basis_allocation, ..
        } = action
        else {
            panic!("an amortization was expected")
        };
        assert_eq!(basis_allocation, expected);
    }

    /// The fingerprint identifies the fact, not the submission: the same fact with a different idempotency
    /// key must produce the same fingerprint (§10.6).
    #[test]
    fn the_fingerprint_names_the_fact_not_the_submission() {
        let fact = partial_redemption();
        let account = AccountId::new_random();
        let one = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(fact.clone()),
            idempotency_key: Some("first".into()),
            source_operation_id: None,
        };
        let two = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(fact),
            idempotency_key: Some("second".into()),
            source_operation_id: Some("внешний-1".into()),
        };
        assert_eq!(
            crate::dedup::fingerprint_journal_event(&one),
            crate::dedup::fingerprint_journal_event(&two)
        );
    }
}
