//! Scenarios for the owner's category reference and assignment rules.

use iaam_core::category::{CategoryAssignment, CategoryInterval, CategoryMatcher, CategoryRule, CategorySubject};
use iaam_core::event::Event;
use iaam_core::ids::{CategoryGroupId, CategoryId, CategoryRuleId};
use iaam_core::projection::money_flow::CategoryIndex;
use serde_json::{Value, json};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{CategoryRuleView, CategoryView, Principal};

#[derive(Debug, Clone)]
pub struct CategoryRuleInput {
    pub matcher: CategoryMatcher,
    pub category: CategoryId,
    pub interval: CategoryInterval,
    pub replaces: Option<CategoryRuleId>,
}

pub async fn create_group(
    services: &AppServices,
    principal: &Principal,
    title: &str,
) -> Result<CategoryGroupId, AppError> {
    services
        .categories
        .create_group(principal.owner, title.to_owned())
        .await
        .map(|group| group.id)
}

pub async fn retire_group(
    services: &AppServices,
    principal: &Principal,
    group: CategoryGroupId,
) -> Result<(), AppError> {
    services.categories.retire_group(principal.owner, group).await
}

pub async fn list_categories(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<CategoryView>, AppError> {
    services.categories.list_categories(principal.owner).await
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
    services
        .categories
        .create_category_rule(
            principal.owner,
            matcher_json(&input.matcher)?,
            input.category,
            input.interval.from,
            input.interval.to,
            input.replaces,
        )
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
}

impl LoadedCategoryIndex {
    pub(crate) fn versions(&self) -> &[u32] {
        &self.versions
    }
}

impl CategoryIndex for LoadedCategoryIndex {
    fn assignment(&self, event: &Event) -> CategoryAssignment {
        let subject = CategorySubject {
            row_key: event.provenance.source_operation_id(),
            source_category: event.provenance.source_category(),
            counterparty: None,
            description: None,
            on: event.order.date(),
        };
        iaam_core::category::assign(&subject, &self.rules)
    }
}

pub(crate) async fn load_index(
    services: &AppServices,
    principal: &Principal,
) -> Result<LoadedCategoryIndex, AppError> {
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
    if let Some(text) = object
        .get("description_contains")
        .and_then(Value::as_str)
    {
        return Ok(CategoryMatcher::DescriptionContains {
            text: text.to_owned(),
        });
    }
    Err(AppError::Invalid {
        field: "matcher".to_owned(),
        expected: "row, source_category, or description_contains".to_owned(),
        actual: raw.to_owned(),
    })
}
