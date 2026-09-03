//! Scenarios for viewing and modifying classification rules.
//!
//! The store is responsible only for version history. Parsing domain JSON and
//! invoking pure recomputation remain in the application scenario; the classification
//! algorithm itself belongs to `iaam-ingest`.

use std::collections::BTreeMap;

use iaam_core::event::Event;
use iaam_core::event::kind::{EventKind, FeeOrigin};
use iaam_core::ids::{AccountId, ClassificationRuleId, EventId, OwnerId};
use iaam_ingest::classification::{
    Classification, ClassificationRule, ClassificationSubject, Correction, Counterparty, Movement,
    RuleMatcher, recompute_plan,
};
use serde_json::{Map, Value};
use time::Date;
use uuid::Uuid;

use crate::AppServices;
use crate::error::{AppError, FieldRejection};
use crate::ports::{ClassificationRuleView, Principal};

pub async fn list_rules(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<ClassificationRuleView>, AppError> {
    services.rules.list_rules(principal.owner).await
}

/// A classification named the way the rule that decides it names one.
///
/// The vocabulary is the rule outcome's own — `internal_transfer`,
/// `external_flow`, `income`, `fee` — so the plan answers in the words the
/// owner wrote the rule in, rather than in the journal's event discriminants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedAs {
    pub kind: &'static str,
    /// The receiving account, for `internal_transfer` only.
    pub to: Option<AccountId>,
    /// The fee's origin, for `fee` only.
    pub origin: Option<&'static str>,
}

/// One event a rule change requires correcting, and what it would become.
///
/// This is [`Correction`] in the transport's vocabulary. It is deliberately not
/// a correction *request*: nothing here has been written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCorrection {
    pub event: EventId,
    pub was: ClassifiedAs,
    pub becomes: ClassifiedAs,
}

/// A rule that was stored, together with what storing it would correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleChange {
    pub rule: ClassificationRuleView,
    /// Empty means the rule changes nothing already in the journal — not that
    /// the plan was not computed.
    pub plan: Vec<PlannedCorrection>,
}

pub async fn create_rule(
    services: &AppServices,
    principal: &Principal,
    matcher: String,
    outcome: String,
    replaces: Option<Uuid>,
) -> Result<RuleChange, AppError> {
    let rule = services
        .rules
        .create_rule(principal.owner, matcher, outcome, replaces)
        .await?;
    let plan = recompute_history(services, principal.owner).await?;
    Ok(RuleChange { rule, plan })
}

pub async fn retire_rule(
    services: &AppServices,
    principal: &Principal,
    id: Uuid,
) -> Result<Vec<PlannedCorrection>, AppError> {
    services.rules.retire_rule(principal.owner, id).await?;
    recompute_history(services, principal.owner).await
}

/// What the current rule set says the recorded history should be corrected to.
///
/// **The plan is returned, not applied**, and the caller is the owner's
/// transport, which shows it to them. Three reasons, in the order they bind:
///
/// 1. Applying means writing reversal and replacement facts into an
///    append-only journal (§4.8), and [`crate::scenarios::correction`] already
///    refuses to do that without `acknowledge_retraction`, because a retracted
///    fact stops counting in every report the owner has already read and
///    re-submitting the same rows does not bring it back. A rule submitted
///    through `POST /v1/classification-rules` carries no such acknowledgement,
///    and inventing one here would route around a control the owner was
///    deliberately given.
/// 2. The replacement fact is not in the plan. A [`Correction`] names the
///    classification an event *becomes*; the event that expresses it needs a
///    `TransferId`, a sign convention, and — for an internal transfer — a cash
///    leg on a second account the outcome names. That is a new fact about
///    another account, invented by this wrapper, and a wrong sign in it cannot
///    be taken back.
/// 3. Storing the rule and correcting the journal are two writes, and this
///    order is the only safe one. With the plan returned there is exactly one
///    write, so nothing can half-happen. Were the corrections applied first and
///    the rule stored second, a failure of the second write would leave the
///    journal corrected by a rule that does not exist and that no later run
///    could reconstruct; applied second, a failure leaves the rule stored and
///    the plan uncorrected — which is precisely the state this function
///    reports, and a repeat run recomputes the same plan, because the plan is
///    built from the effective set and is idempotent by construction.
///
/// The operation that applies it already exists: `POST /v1/corrections`, which
/// takes the acknowledgement and writes the reversal and replacement facts.
///
/// `recompute_plan` remains the sole place that determines which events require
/// correction. This scenario does not modify events itself or perform monetary
/// arithmetic in the wrapper.
async fn recompute_history(
    services: &AppServices,
    owner: OwnerId,
) -> Result<Vec<PlannedCorrection>, AppError> {
    let events = services.store.load_events_through(owner, Date::MAX).await?;
    let stored = services.rules.list_rules(owner).await?;
    let rules = stored
        .into_iter()
        .filter(|rule| rule.retired_at.is_none())
        .map(rule_from_view)
        .collect::<Result<Vec<_>, _>>()?;
    let subjects = events
        .iter()
        .filter_map(|event| subject(event).map(|subject| (event.id, subject)))
        .collect::<BTreeMap<EventId, ClassificationSubject>>();
    recompute_plan(&events, &subjects, &rules)
        .map(|plan| plan.iter().map(planned).collect())
        .map_err(|error| AppError::Store(format!("classification recomputation: {error}")))
}

fn planned(correction: &Correction) -> PlannedCorrection {
    PlannedCorrection {
        event: correction.target,
        was: classified_as(correction.was),
        becomes: classified_as(correction.becomes),
    }
}

/// The inverse of [`parse_outcome`], and it must stay so: the plan speaks the
/// vocabulary the owner writes rules in, or it names a decision they cannot
/// restate as a rule.
const fn classified_as(classification: Classification) -> ClassifiedAs {
    match classification {
        Classification::InternalTransfer { to } => ClassifiedAs {
            kind: "internal_transfer",
            to: Some(to),
            origin: None,
        },
        Classification::ExternalFlow => ClassifiedAs {
            kind: "external_flow",
            to: None,
            origin: None,
        },
        Classification::Income => ClassifiedAs {
            kind: "income",
            to: None,
            origin: None,
        },
        Classification::Fee { origin } => ClassifiedAs {
            kind: "fee",
            to: None,
            origin: Some(match origin {
                FeeOrigin::Brokerage => "brokerage",
                FeeOrigin::Depositary => "depositary",
                FeeOrigin::AccountMaintenance => "account_maintenance",
                FeeOrigin::MarginInterest => "margin_interest",
                FeeOrigin::Other => "other",
            }),
        },
    }
}

/// A stored rule in the classifier's own vocabulary.
///
/// Shared with the import session on purpose: the session classifies an incoming
/// row against the same rules the recomputation replays history with, and two
/// readings of one stored matcher would eventually disagree about what the owner
/// decided.
pub fn rule_from_view(rule: ClassificationRuleView) -> Result<ClassificationRule, AppError> {
    let matcher = json_object(&rule.matcher, "matcher")?;
    let outcome = json_object(&rule.outcome, "outcome")?;
    Ok(ClassificationRule {
        id: ClassificationRuleId(rule.id),
        version: rule.version,
        matcher: RuleMatcher {
            counterparty_account: optional_string(&matcher, "counterparty_account", "matcher")?,
            description_contains: optional_string(&matcher, "description_contains", "matcher")?,
            kind: optional_string(&matcher, "kind", "matcher")?,
        },
        outcome: parse_outcome(outcome)?,
    })
}

fn json_object(raw: &str, field: &str) -> Result<Map<String, Value>, AppError> {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(AppError::Invalid {
            field: field.to_owned(),
            expected: "classification rule JSON object".to_owned(),
            actual: "JSON is not an object".to_owned(),
        }),
        Err(error) => Err(AppError::Invalid {
            field: field.to_owned(),
            expected: "classification rule JSON object".to_owned(),
            actual: error.to_string(),
        }),
    }
}

fn optional_string(
    object: &Map<String, Value>,
    field: &str,
    group: &str,
) -> Result<Option<String>, AppError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(actual) => Err(AppError::Invalid {
            field: group.to_owned(),
            expected: format!("field {field} is a string"),
            actual: actual.to_string(),
        }),
    }
}

fn parse_outcome(outcome: Map<String, Value>) -> Result<Classification, AppError> {
    let kind = outcome
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_outcome("field kind"))?;
    match kind {
        "internal_transfer" => {
            let raw = outcome
                .get("to")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_outcome("field to"))?;
            let to = Uuid::parse_str(raw).map_err(|_| invalid_outcome(raw))?;
            Ok(Classification::InternalTransfer { to: AccountId(to) })
        }
        "external_flow" => Ok(Classification::ExternalFlow),
        "income" => Ok(Classification::Income),
        "fee" => Ok(Classification::Fee {
            origin: match outcome.get("origin").and_then(Value::as_str) {
                Some("brokerage") => FeeOrigin::Brokerage,
                Some("depositary") => FeeOrigin::Depositary,
                Some("account_maintenance") => FeeOrigin::AccountMaintenance,
                Some("margin_interest") => FeeOrigin::MarginInterest,
                Some("other") => FeeOrigin::Other,
                Some(actual) => return Err(invalid_outcome(actual)),
                None => return Err(invalid_outcome("field origin")),
            },
        }),
        actual => Err(invalid_outcome(actual)),
    }
}

/// The outcome vocabulary is closed, so the refusal publishes it as values.
///
/// `expected` still spells the same four out in prose, because that sentence is
/// what the error message reads as. The list beside it is the half a client can
/// retry from without parsing anything.
fn invalid_outcome(actual: &str) -> AppError {
    FieldRejection::new(
        "outcome",
        "internal_transfer, external_flow, income or fee",
        actual,
    )
    .admitting_codes(&["internal_transfer", "external_flow", "income", "fee"])
    .into()
}

fn subject(event: &Event) -> Option<ClassificationSubject> {
    let (counterparty, movement) = match event.kind {
        EventKind::CashIn { .. } | EventKind::Income { .. } | EventKind::Refund { .. } => {
            (Counterparty::Unknown, Movement::In)
        }
        EventKind::CashOut { .. } | EventKind::Fee { .. } => (Counterparty::Unknown, Movement::Out),
        EventKind::CashTransfer { from, to, .. } => {
            if from == event.account {
                (Counterparty::Named(to.inner().to_string()), Movement::Out)
            } else {
                (Counterparty::Named(from.inner().to_string()), Movement::In)
            }
        }
        EventKind::Trade { .. }
        | EventKind::OpeningPosition { .. }
        | EventKind::OpeningCash { .. }
        | EventKind::Valuation { .. }
        | EventKind::ControlAssertion { .. }
        | EventKind::ImportCoverageGap { .. }
        // A corporate action and an offer do not become classification
        // subjects. Returning them as inflows would mean asking
        // the owner «this inflow — is it income?» about amortisation and recording
        // their answer as a rule: a return of their own capital (§6.5)
        // would forever be counted as income. This is also why
        // `classification_of` returns `None` for them.
        // A tax already identifies its own fact and is not a rule-classified expense.
        | EventKind::Tax { .. }
        | EventKind::CorporateAction { .. }
        | EventKind::OfferExercise { .. } => return None,
    };
    Some(ClassificationSubject {
        account: event.account,
        counterparty,
        // Both come from provenance, which retained what the source said about
        // the row and never rewrites it. Anything else would ask the
        // recomputation to reconsider a classification using the previous
        // classification as its input: the event discriminant — `cash_out`,
        // `income` — is the answer a rule is meant to revise, not the question
        // the rule was written about.
        description: event.provenance.description().map(str::to_owned),
        source_kind: event.provenance.source_category().map(str::to_owned),
        movement: Some(movement),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use iaam_core::event::corporate_action::CorporateAction;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId};
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{CustodyId, InstrumentId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn event_of(account: AccountId, kind: EventKind, legs: Vec<Leg>) -> Event {
        let day = date!(2026 - 06 - 15);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, 0),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"e".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn cash_out_of(account: AccountId, provenance: Provenance) -> Event {
        let amount = rub(-500_000);
        Event {
            provenance,
            ..event_of(
                account,
                EventKind::CashOut { amount },
                vec![Leg::cash(account, amount)],
            )
        }
    }

    fn provenance_of() -> Provenance {
        Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"a".repeat(64)).unwrap(),
            ParserVersion("test/1".into()),
        )
    }

    fn rule_matching(matcher: RuleMatcher) -> ClassificationRule {
        ClassificationRule {
            id: ClassificationRuleId(Uuid::new_v4()),
            version: 1,
            matcher,
            outcome: Classification::Fee {
                origin: FeeOrigin::AccountMaintenance,
            },
        }
    }

    #[test]
    fn the_rebuilt_subject_carries_the_description_the_source_printed() {
        // Without this the `description_contains` third of `RuleMatcher` is dead
        // on the recompute path: a rule the owner wrote about what the bank
        // printed on the row matches at intake and matches nothing afterwards.
        let account = AccountId::new_random();
        let event = cash_out_of(account, provenance_of().with_description("Shop One"));

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(subject.description.as_deref(), Some("Shop One"));
        assert!(
            rule_matching(RuleMatcher {
                counterparty_account: None,
                description_contains: Some("shop one".to_owned()),
                kind: None,
            })
            .matcher
            .matches(&subject),
            "a rule naming the description must match on recompute"
        );
    }

    #[test]
    fn the_rebuilt_subject_carries_the_word_the_source_used_not_the_event_kind() {
        // `source_kind` is «what the source called the operation». Filling it
        // with the event's own discriminant asks the recomputation to reconsider
        // a classification using the previous classification as its input.
        let account = AccountId::new_random();
        let event = cash_out_of(account, provenance_of().with_source_category("Transfers"));

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(subject.source_kind.as_deref(), Some("Transfers"));
        assert_ne!(
            subject.source_kind.as_deref(),
            Some(event.kind.discriminant()),
            "the event discriminant is the answer, not the question"
        );
        assert!(
            rule_matching(RuleMatcher {
                counterparty_account: None,
                description_contains: None,
                kind: Some("Transfers".to_owned()),
            })
            .matcher
            .matches(&subject),
            "a rule naming the source's own word must match on recompute"
        );
    }

    #[test]
    fn a_source_that_named_neither_leaves_both_fields_unknown() {
        // Unknown is `None`, not a substitute (§4.9). Falling back to the
        // event discriminant here would restore the circularity for exactly
        // the rows whose source said nothing.
        let account = AccountId::new_random();
        let event = cash_out_of(account, provenance_of());

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(subject.description, None);
        assert_eq!(subject.source_kind, None);
    }

    #[test]
    fn an_amortisation_is_not_offered_to_the_owner_as_an_inflow() {
        // Returning amortisation as a subject with `Movement::In` would mean
        // asking the owner «this inflow — is it income?» and recording their answer
        // as a rule: a return of their own capital (§6.5) would forever
        // be counted as income.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let compensation = rub(2_000_000);
        let event = event_of(
            account,
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: Quantity(Dec::new(Decimal::from(100))),
                    principal_returned_per_unit: PerUnitAmount::new(
                        Dec::new(Decimal::from(200)),
                        CurrencyCode::Rub,
                    ),
                    compensation,
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
                },
            },
            vec![Leg::principal(account, instrument, compensation)],
        );
        assert!(subject(&event).is_none());
    }

    #[test]
    fn a_settled_offer_is_not_offered_to_the_owner_either() {
        let account = AccountId::new_random();
        let event = event_of(
            account,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: Quantity(Dec::new(Decimal::from(10))),
                    gross: rub(1_000_000),
                    fee: None,
                    accrued_interest: None,
                },
            },
            Vec::new(),
        );
        assert!(subject(&event).is_none());
    }
}
