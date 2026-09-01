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
    Classification, ClassificationRule, ClassificationSubject, Counterparty, Movement, RuleMatcher,
    recompute_plan,
};
use serde_json::{Map, Value};
use time::Date;
use uuid::Uuid;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{ClassificationRuleView, Principal};

pub async fn list_rules(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<ClassificationRuleView>, AppError> {
    services.rules.list_rules(principal.owner).await
}

pub async fn create_rule(
    services: &AppServices,
    principal: &Principal,
    matcher: String,
    outcome: String,
    replaces: Option<Uuid>,
) -> Result<ClassificationRuleView, AppError> {
    let rule = services
        .rules
        .create_rule(principal.owner, matcher, outcome, replaces)
        .await?;
    recompute_history(services, principal.owner).await?;
    Ok(rule)
}

pub async fn retire_rule(
    services: &AppServices,
    principal: &Principal,
    id: Uuid,
) -> Result<(), AppError> {
    services.rules.retire_rule(principal.owner, id).await?;
    recompute_history(services, principal.owner).await
}

/// Recomputes the current history after rule changes.
///
/// `recompute_plan` remains the sole place that determines which
/// events require correction. This scenario does not modify events
/// itself or perform monetary arithmetic in the wrapper.
async fn recompute_history(services: &AppServices, owner: OwnerId) -> Result<(), AppError> {
    let events = services.store.load_events_through(owner, Date::MAX).await?;
    let stored = services.rules.list_rules(owner).await?;
    let rules = stored
        .into_iter()
        .filter(|rule| rule.retired_at.is_none())
        .map(domain_rule)
        .collect::<Result<Vec<_>, _>>()?;
    let subjects = events
        .iter()
        .filter_map(|event| subject(event).map(|subject| (event.id, subject)))
        .collect::<BTreeMap<EventId, ClassificationSubject>>();
    recompute_plan(&events, &subjects, &rules)
        .map(|_| ())
        .map_err(|error| AppError::Store(format!("classification recomputation: {error}")))
}

fn domain_rule(rule: ClassificationRuleView) -> Result<ClassificationRule, AppError> {
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

fn invalid_outcome(actual: &str) -> AppError {
    AppError::Invalid {
        field: "outcome".to_owned(),
        expected: "internal_transfer, external_flow, income or fee".to_owned(),
        actual: actual.to_owned(),
    }
}

fn subject(event: &Event) -> Option<ClassificationSubject> {
    let (counterparty, movement) = match event.kind {
        EventKind::CashIn { .. } | EventKind::Income { .. } => {
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
        description: None,
        source_kind: Some(event.kind.discriminant().to_owned()),
        movement,
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
