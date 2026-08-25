//! Сценарии видимых и изменяемых правил классификации.
//!
//! Хранилище отвечает только за историю версий. Разбор доменных JSON и
//! вызов чистого пересчёта остаются в application-сценарии; сам алгоритм
//! классификации принадлежит `iaam-ingest`.

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

/// Пересчитывает действующую историю после изменения правил.
///
/// `recompute_plan` остаётся единственным местом, где определяется, какие
/// события требуют исправления. Этот сценарий не меняет события
/// самостоятельно и не выполняет денежную арифметику в оболочке.
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
        .map_err(|error| AppError::Store(format!("пересчёт классификации: {error}")))
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
            expected: "JSON-объект правила классификации".to_owned(),
            actual: "JSON не является объектом".to_owned(),
        }),
        Err(error) => Err(AppError::Invalid {
            field: field.to_owned(),
            expected: "JSON-объект правила классификации".to_owned(),
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
            expected: format!("поле {field} является строкой"),
            actual: actual.to_string(),
        }),
    }
}

fn parse_outcome(outcome: Map<String, Value>) -> Result<Classification, AppError> {
    let kind = outcome
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_outcome("поле kind"))?;
    match kind {
        "internal_transfer" => {
            let raw = outcome
                .get("to")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_outcome("поле to"))?;
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
                None => return Err(invalid_outcome("поле origin")),
            },
        }),
        actual => Err(invalid_outcome(actual)),
    }
}

fn invalid_outcome(actual: &str) -> AppError {
    AppError::Invalid {
        field: "outcome".to_owned(),
        expected: "internal_transfer, external_flow, income или fee".to_owned(),
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
        | EventKind::ControlAssertion { .. } => return None,
    };
    Some(ClassificationSubject {
        account: event.account,
        counterparty,
        description: None,
        source_kind: Some(event.kind.discriminant().to_owned()),
        movement,
    })
}
