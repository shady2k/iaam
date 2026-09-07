//! Scenarios for the owner's category reference and assignment rules.

use iaam_core::category::{
    CategoryAssignment, CategoryImpactRow, CategoryInterval, CategoryMatcher, CategoryRule,
    CategoryRuleProposal, CategorySubject, DescriptionMatchMode, assign_with_proposed,
    group_category_impacts, row_key as category_row_key,
};
use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::ids::{CategoryGroupId, CategoryId, CategoryRuleId};
use iaam_core::money::Money;
use iaam_core::projection::money_flow::CategoryIndex;
use serde_json::{Value, json};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{
    CategoryGroupView, CategoryRuleUpsert, CategoryRuleView, CategoryView, Principal,
};

#[derive(Debug, Clone)]
pub struct CategoryRuleInput {
    pub matcher: CategoryMatcher,
    pub category: CategoryId,
    pub interval: CategoryInterval,
    pub replaces: Option<CategoryRuleId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRuleImpact {
    pub rows: u64,
    /// Every affected journal row, before the monthly aggregates below.
    pub preview_rows: Vec<CategoryPreviewRow>,
    /// By month, oldest first: what moved, and between which categories.
    pub months: Vec<MonthlyImpact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryPreviewRow {
    /// The exact key consumed by `CategoryMatcher::Row`, when the event has one.
    pub row_key: Option<String>,
    /// `None` means the row was not previously decomposed.
    pub from: Option<CategoryId>,
    pub to: CategoryId,
    pub amount: Money,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyImpact {
    /// First day of the month.
    pub month: time::Date,
    pub moved: Vec<CategoryMove>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryMove {
    /// `None` means the row was not previously decomposed.
    pub from: Option<CategoryId>,
    pub to: CategoryId,
    pub amount: Money,
    pub rows: u64,
}

pub async fn create_group(
    services: &AppServices,
    principal: &Principal,
    title: &str,
    is_income: bool,
) -> Result<CategoryGroupId, AppError> {
    services
        .categories
        .create_group(principal.owner, title.to_owned(), is_income)
        .await
        .map(|group| group.id)
}

pub async fn retire_group(
    services: &AppServices,
    principal: &Principal,
    group: CategoryGroupId,
) -> Result<(), AppError> {
    services
        .categories
        .retire_group(principal.owner, group)
        .await
}

pub async fn list_categories(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<CategoryView>, AppError> {
    services.categories.list_categories(principal.owner).await
}

pub async fn list_groups(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<CategoryGroupView>, AppError> {
    services.categories.list_groups(principal.owner).await
}

pub async fn create_category(
    services: &AppServices,
    principal: &Principal,
    group: CategoryGroupId,
    title: &str,
) -> Result<CategoryId, AppError> {
    match services
        .categories
        .create_category(principal.owner, group, title.to_owned())
        .await
    {
        Err(AppError::CategoryGroupRetired { id }) => Err(AppError::Invalid {
            field: "group".to_owned(),
            expected: "an active category group".to_owned(),
            actual: id,
        }),
        Ok(category) => Ok(category.id),
        Err(error) => Err(error),
    }
}

pub async fn retire_category(
    services: &AppServices,
    principal: &Principal,
    category: CategoryId,
) -> Result<(), AppError> {
    services
        .categories
        .retire_category(principal.owner, category)
        .await
}

pub async fn create_category_rule(
    services: &AppServices,
    principal: &Principal,
    input: CategoryRuleInput,
) -> Result<CategoryRuleView, AppError> {
    if input
        .interval
        .from
        .zip(input.interval.to)
        .is_some_and(|(from, to)| from > to)
    {
        return Err(AppError::Invalid {
            field: "interval".to_owned(),
            expected: "from no later than to".to_owned(),
            actual: format!("{:?}", input.interval),
        });
    }

    let rule = CategoryRuleUpsert {
        matcher: matcher_json(&input.matcher)?,
        category: input.category,
        valid_from: input.interval.from,
        valid_to: input.interval.to,
    };
    services
        .categories
        .create_category_rule(principal.owner, rule, input.replaces)
        .await
}

pub async fn list_category_rules(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<CategoryRuleView>, AppError> {
    services
        .categories
        .list_category_rules(principal.owner)
        .await
}

pub async fn preview_category_rule(
    services: &AppServices,
    principal: &Principal,
    proposed: &CategoryRuleProposal,
) -> Result<CategoryRuleImpact, AppError> {
    preview_category_rules(services, principal, std::slice::from_ref(proposed))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Store("category rule preview returned no result".to_owned()))
}

pub async fn preview_category_rules(
    services: &AppServices,
    principal: &Principal,
    proposed: &[CategoryRuleProposal],
) -> Result<Vec<CategoryRuleImpact>, AppError> {
    let active_rules = services
        .categories
        .list_category_rules(principal.owner)
        .await?
        .into_iter()
        .filter(|rule| rule.retired_at.is_none())
        .map(domain_rule)
        .collect::<Result<Vec<_>, _>>()?;
    let events = services
        .store
        .load_events_through(principal.owner, time::Date::MAX)
        .await?;

    proposed
        .iter()
        .map(|proposal| preview_category_rule_from(&events, &active_rules, proposal))
        .collect()
}

fn preview_category_rule_from(
    events: &[Event],
    active_rules: &[CategoryRule],
    proposed: &CategoryRuleProposal,
) -> Result<CategoryRuleImpact, AppError> {
    let current = LoadedCategoryIndex {
        rules: active_rules.to_vec(),
        versions: Vec::new(),
        category_count: 0,
        proposed: None,
    };
    let proposed_index = LoadedCategoryIndex {
        rules: active_rules.to_vec(),
        versions: Vec::new(),
        category_count: 0,
        proposed: Some(proposed.clone()),
    };
    let mut moved = Vec::new();
    let mut preview_rows = Vec::new();
    for event in events {
        if !matches!(event.kind, EventKind::CashOut { .. }) {
            continue;
        }

        let previous = match current.assignment(event) {
            CategoryAssignment::Assigned { category, .. } => Some(category),
            CategoryAssignment::NotDecomposed => None,
        };
        let CategoryAssignment::Assigned { category: next, .. } =
            proposed_index.assignment(event)
        else {
            continue;
        };
        if previous == Some(next) {
            continue;
        }

        let on = event
            .dates
            .effective_date()
            .ok_or_else(|| AppError::Invalid {
                field: "event.date".to_owned(),
                expected: "an effective date".to_owned(),
                actual: event.id.0.to_string(),
            })?;
        let month = time::Date::from_calendar_date(on.year(), on.month(), 1).map_err(|error| {
            AppError::Invalid {
                field: "event.date".to_owned(),
                expected: "a valid calendar date".to_owned(),
                actual: error.to_string(),
            }
        })?;
        let EventKind::CashOut { amount } = event.kind else {
            unreachable!("cash-out kind was checked above");
        };
        moved.push(CategoryImpactRow {
            month,
            from: previous,
            to: next,
            amount,
        });
        preview_rows.push(CategoryPreviewRow {
            row_key: category_row_key(event).map(str::to_owned),
            from: previous,
            to: next,
            amount,
        });
    }

    let grouped = group_category_impacts(moved)
        .map_err(|error| AppError::Store(format!("aggregate category preview: {error}")))?;
    Ok(CategoryRuleImpact {
        rows: grouped.rows,
        preview_rows,
        months: grouped
            .months
            .into_iter()
            .map(|month| MonthlyImpact {
                month: month.month,
                moved: month
                    .moved
                    .into_iter()
                    .map(|movement| CategoryMove {
                        from: movement.from,
                        to: movement.to,
                        amount: movement.amount,
                        rows: movement.rows,
                    })
                    .collect(),
            })
            .collect(),
    })
}

pub async fn retire_category_rule(
    services: &AppServices,
    principal: &Principal,
    rule: CategoryRuleId,
) -> Result<(), AppError> {
    services
        .categories
        .retire_category_rule(principal.owner, rule)
        .await
}

pub(crate) struct LoadedCategoryIndex {
    rules: Vec<CategoryRule>,
    versions: Vec<u32>,
    category_count: usize,
    proposed: Option<CategoryRuleProposal>,
}

impl LoadedCategoryIndex {
    pub(crate) fn versions(&self) -> &[u32] {
        &self.versions
    }

    pub(crate) fn has_categories(&self) -> bool {
        self.category_count > 0
    }
}

impl CategoryIndex for LoadedCategoryIndex {
    fn assignment(&self, event: &Event) -> CategoryAssignment {
        // The source's own identifier when it stated one; otherwise the key the
        // client supplied. Without the fallback a source that names no
        // identifier — a card statement, typically — cannot be corrected row by
        // row at all, and the strongest precedence level is dead for exactly
        // the imports that need it most. Provenance is untouched: it still
        // records that the source named nothing.
        // The same helper is published on journal and preview rows, so a Row
        // matcher always receives the identity the API tells the caller to use.
        let row_key = category_row_key(event);
        let subject = CategorySubject {
            row_key,
            source_category: event.provenance.source_category(),
            // The two were hard-wired to None, which made every
            // DescriptionContains rule dead on arrival. The source states one
            // string; it fills both roles until a source that separates them
            // arrives.
            counterparty: event.provenance.description(),
            description: event.provenance.description(),
            on: event.order.date(),
        };
        match self.proposed.as_ref() {
            Some(proposed) => assign_with_proposed(&subject, &self.rules, proposed),
            None => iaam_core::category::assign(&subject, &self.rules),
        }
    }
}

pub(crate) async fn load_index(
    services: &AppServices,
    principal: &Principal,
) -> Result<LoadedCategoryIndex, AppError> {
    let category_count = services
        .categories
        .list_categories(principal.owner)
        .await?
        .into_iter()
        .filter(|category| category.retired_at.is_none())
        .count();
    let stored = services
        .categories
        .list_category_rules(principal.owner)
        .await?;
    let active = stored
        .into_iter()
        .filter(|rule| rule.retired_at.is_none())
        .map(domain_rule)
        .collect::<Result<Vec<_>, _>>()?;
    let versions = active.iter().map(|rule| rule.version).collect();
    Ok(LoadedCategoryIndex {
        rules: active,
        versions,
        category_count,
        proposed: None,
    })
}

fn domain_rule(rule: CategoryRuleView) -> Result<CategoryRule, AppError> {
    Ok(CategoryRule {
        id: rule.id,
        version: rule.version,
        interval: CategoryInterval {
            from: rule.valid_from,
            to: rule.valid_to,
        },
        matcher: parse_matcher(&rule.matcher)?,
        category: rule.category,
    })
}

fn matcher_json(matcher: &CategoryMatcher) -> Result<String, AppError> {
    let value = match matcher {
        CategoryMatcher::Row { key } => json!({ "row": key }),
        CategoryMatcher::SourceCategory { value } => json!({ "source_category": value }),
        CategoryMatcher::DescriptionContains { text } => json!({ "description_contains": text }),
        CategoryMatcher::Description {
            text,
            mode: DescriptionMatchMode::Equals,
        } => json!({ "description_equals": text }),
        CategoryMatcher::Description {
            text,
            mode: DescriptionMatchMode::StartsWith,
        } => json!({ "description_starts_with": text }),
        CategoryMatcher::Description {
            text,
            mode: DescriptionMatchMode::Contains,
        } => json!({ "description_contains": text }),
    };
    serde_json::to_string(&value)
        .map_err(|error| AppError::Store(format!("serialize category matcher: {error}")))
}

fn parse_matcher(raw: &str) -> Result<CategoryMatcher, AppError> {
    let value = serde_json::from_str::<Value>(raw).map_err(|error| AppError::Invalid {
        field: "matcher".to_owned(),
        expected: "a category matcher object".to_owned(),
        actual: error.to_string(),
    })?;
    let object = value.as_object().ok_or_else(|| AppError::Invalid {
        field: "matcher".to_owned(),
        expected: "a category matcher object".to_owned(),
        actual: raw.to_owned(),
    })?;
    if let Some(key) = object.get("row").and_then(Value::as_str) {
        return Ok(CategoryMatcher::Row {
            key: key.to_owned(),
        });
    }
    if let Some(value) = object.get("source_category").and_then(Value::as_str) {
        return Ok(CategoryMatcher::SourceCategory {
            value: value.to_owned(),
        });
    }
    if let Some(text) = object.get("description_equals").and_then(Value::as_str) {
        return Ok(CategoryMatcher::Description {
            text: text.to_owned(),
            mode: DescriptionMatchMode::Equals,
        });
    }
    if let Some(text) = object
        .get("description_starts_with")
        .and_then(Value::as_str)
    {
        return Ok(CategoryMatcher::Description {
            text: text.to_owned(),
            mode: DescriptionMatchMode::StartsWith,
        });
    }
    if let Some(text) = object.get("description_contains").and_then(Value::as_str) {
        return Ok(CategoryMatcher::DescriptionContains {
            text: text.to_owned(),
        });
    }
    Err(AppError::Invalid {
        field: "matcher".to_owned(),
        expected: "row, source_category, description_equals, description_starts_with, or description_contains".to_owned(),
        actual: raw.to_owned(),
    })
}
