//! Owner category reference data.
//!
//! Categories are reference records rather than event fields. They can be
//! reorganized without rewriting the append-only journal, while retirement
//! keeps the records needed to explain historical reports.

use iaam_core::category::{CategoryMatcher, DescriptionMatchMode};
use iaam_core::ids::{CategoryId, CategoryRuleId, OwnerId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use time::Date;
use time::format_description::well_known::Iso8601;
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// A category group owned by one portfolio owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryGroupRow {
    pub id: Uuid,
    pub owner: OwnerId,
    pub title: String,
    pub retired_at: Option<String>,
    /// Whether the group holds income categories rather than spending ones.
    pub is_income: bool,
}

/// A category belonging to exactly one category group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRow {
    pub id: Uuid,
    pub owner: OwnerId,
    pub group_id: Uuid,
    pub title: String,
    pub retired_at: Option<String>,
}

/// A versioned category assignment rule, including retired history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRuleRow {
    pub id: CategoryRuleId,
    pub owner: OwnerId,
    pub version: u32,
    /// `CategoryMatcher`'s four variants, spelled as the `matcher_kind`,
    /// `value`, `text` and `description_mode` columns (spec §4.6).
    pub matcher: CategoryMatcher,
    pub category: CategoryId,
    pub valid_from: Option<Date>,
    pub valid_to: Option<Date>,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<CategoryRuleId>,
}
/// A category rule to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCategoryRule {
    pub matcher: CategoryMatcher,
    pub category: Uuid,
    pub valid_from: Option<Date>,
    pub valid_to: Option<Date>,
}

impl SqliteStore {
    /// Add a category group and return its stable identifier.
    pub fn insert_category_group(
        &mut self,
        owner: OwnerId,
        title: &str,
    ) -> Result<Uuid, StoreError> {
        self.insert_category_group_of_kind(owner, title, false)
    }

    /// Insert a group, stating whether it holds income categories.
    ///
    /// Income is the same list with a flag rather than a second mechanism, so
    /// this is one column and not a parallel table.
    pub fn insert_category_group_of_kind(
        &mut self,
        owner: OwnerId,
        title: &str,
        is_income: bool,
    ) -> Result<Uuid, StoreError> {
        let id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO category_groups (id, owner, title, created_at, retired_at, is_income)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            params![
                id.to_string(),
                owner.inner().to_string(),
                title,
                now(),
                i64::from(is_income)
            ],
        )?;
        Ok(id)
    }
    /// List every category group owned by the portfolio owner, including retired rows.
    pub fn list_groups(&self, owner: OwnerId) -> Result<Vec<CategoryGroupRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, title, retired_at, is_income
             FROM category_groups
             WHERE owner = ?1
             ORDER BY created_at",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut groups = Vec::new();
        for row in rows {
            let (id, title, retired_at, is_income) = row?;
            groups.push(CategoryGroupRow {
                id: parse_uuid(&id, "category group")?,
                owner,
                title,
                retired_at,
                is_income: is_income != 0,
            });
        }
        Ok(groups)
    }

    /// Add a category to one of the owner's groups.
    pub fn insert_category(
        &mut self,
        owner: OwnerId,
        group_id: Uuid,
        title: &str,
    ) -> Result<Uuid, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let group_retired: Option<Option<String>> = transaction
            .query_row(
                "SELECT retired_at FROM category_groups WHERE id = ?1 AND owner = ?2",
                params![group_id.to_string(), owner.inner().to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match group_retired {
            None => {
                return Err(StoreError::NotFound {
                    what: "category group",
                    id: group_id.to_string(),
                });
            }
            Some(Some(_)) => {
                return Err(StoreError::CategoryGroupRetired {
                    id: group_id.to_string(),
                });
            }
            Some(None) => {}
        }

        let id = Uuid::new_v4();
        transaction.execute(
            "INSERT INTO categories (id, owner, group_id, title, created_at, retired_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                id.to_string(),
                owner.inner().to_string(),
                group_id.to_string(),
                title,
                now(),
            ],
        )?;
        transaction.commit()?;
        Ok(id)
    }

    /// Create a category rule with the next owner-local version.
    pub fn insert_category_rule(
        &mut self,
        owner: OwnerId,
        rule: NewCategoryRule,
        replaces: Option<CategoryRuleId>,
    ) -> Result<CategoryRuleRow, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = write_category_rule(&transaction, owner, rule, replaces)?;
        transaction.commit()?;
        Ok(stored)
    }

    /// Edit a rule: retire the previous one and create a new one.
    ///
    /// Both records are written in one transaction: there is no point at which
    /// both rules are active or neither is active—and a report computed at such
    /// a point would be inexplicable.
    pub fn amend_category_rule(
        &mut self,
        owner: OwnerId,
        previous: CategoryRuleId,
        rule: NewCategoryRule,
    ) -> Result<CategoryRuleRow, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_category_rule_row(&transaction, owner, previous)?;
        let stored = write_category_rule(&transaction, owner, rule, Some(previous))?;
        transaction.commit()?;
        Ok(stored)
    }

    /// List every category rule owned by the portfolio owner, including retired
    /// rows, in version order.
    pub fn list_category_rules(&self, owner: OwnerId) -> Result<Vec<CategoryRuleRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, version, matcher_kind, value, text, description_mode,
                    category, valid_from, valid_to,
                    created_at, retired_at, replaces
             FROM category_rules
             WHERE owner = ?1
             ORDER BY version",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?;
        let mut rules = Vec::new();
        for row in rows {
            let (
                id,
                version,
                matcher_kind,
                value,
                text,
                description_mode,
                category,
                valid_from,
                valid_to,
                created_at,
                retired_at,
                replaces,
            ) = row?;
            rules.push(CategoryRuleRow {
                id: CategoryRuleId(parse_uuid(&id, "category rule")?),
                owner,
                version,
                matcher: matcher_from_columns(&matcher_kind, value, text, description_mode)?,
                category: CategoryId(parse_uuid(&category, "category")?),
                valid_from: valid_from
                    .as_deref()
                    .map(|value| text_to_date(value, "valid_from"))
                    .transpose()?,
                valid_to: valid_to
                    .as_deref()
                    .map(|value| text_to_date(value, "valid_to"))
                    .transpose()?,
                created_at,
                retired_at,
                replaces: replaces
                    .as_deref()
                    .map(|value| parse_uuid(value, "category rule"))
                    .transpose()?
                    .map(CategoryRuleId),
            });
        }
        Ok(rules)
    }

    /// Retire a category rule without deleting its historical row.
    pub fn retire_category_rule(
        &mut self,
        owner: OwnerId,
        id: CategoryRuleId,
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_category_rule_row(&transaction, owner, id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Retire a category group without deleting its historical row.
    pub fn retire_category_group(&mut self, owner: OwnerId, id: Uuid) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE category_groups SET retired_at = ?3
             WHERE owner = ?1 AND id = ?2 AND retired_at IS NULL",
            params![owner.inner().to_string(), id.to_string(), now()],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "active category group",
                id: id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(())
    }

    /// Retire a category without deleting or rewriting its historical row.
    pub fn retire_category(&mut self, owner: OwnerId, id: Uuid) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE categories SET retired_at = ?3
             WHERE owner = ?1 AND id = ?2 AND retired_at IS NULL",
            params![owner.inner().to_string(), id.to_string(), now()],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "active category",
                id: id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(())
    }

    /// List every category owned by the portfolio owner, including retired rows.
    pub fn list_categories(&self, owner: OwnerId) -> Result<Vec<CategoryRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, group_id, title, retired_at
             FROM categories
             WHERE owner = ?1
             ORDER BY title, id",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut categories = Vec::new();
        for row in rows {
            let (id, group_id, title, retired_at) = row?;
            categories.push(CategoryRow {
                id: parse_uuid(&id, "category")?,
                owner,
                group_id: parse_uuid(&group_id, "category group")?,
                title,
                retired_at,
            });
        }
        Ok(categories)
    }
}

/// Insert a rule with the next owner-local decision number.
///
/// The number is assigned inside the caller's immediate transaction: doing
/// this separately creates the same race as in the classification rules.
fn write_category_rule(
    conn: &Connection,
    owner: OwnerId,
    rule: NewCategoryRule,
    replaces: Option<CategoryRuleId>,
) -> Result<CategoryRuleRow, StoreError> {
    let category_exists: Option<()> = conn
        .query_row(
            "SELECT 1 FROM categories WHERE id = ?1 AND owner = ?2",
            params![rule.category.to_string(), owner.inner().to_string()],
            |_| Ok(()),
        )
        .optional()?;
    if category_exists.is_none() {
        return Err(StoreError::NotFound {
            what: "category",
            id: rule.category.to_string(),
        });
    }

    let used: Option<u32> = conn.query_row(
        "SELECT MAX(version) FROM category_rules WHERE owner = ?1",
        [owner.inner().to_string()],
        |row| row.get(0),
    )?;
    let stored = CategoryRuleRow {
        id: CategoryRuleId::new_random(),
        owner,
        version: used.map_or(1, |value| value.saturating_add(1)),
        matcher: rule.matcher,
        category: CategoryId(rule.category),
        valid_from: rule.valid_from,
        valid_to: rule.valid_to,
        created_at: now(),
        retired_at: None,
        replaces,
    };
    let (matcher_kind, value, text, description_mode) = matcher_to_columns(&stored.matcher);
    conn.execute(
        "INSERT INTO category_rules (
             id, owner, version, matcher_kind, value, text, description_mode,
             category, valid_from, valid_to,
             created_at, retired_at, replaces
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12)",
        params![
            stored.id.inner().to_string(),
            owner.inner().to_string(),
            stored.version,
            matcher_kind,
            value,
            text,
            description_mode,
            stored.category.inner().to_string(),
            stored.valid_from.map(date_to_text),
            stored.valid_to.map(date_to_text),
            &stored.created_at,
            replaces.map(|id| id.inner().to_string()),
        ],
    )?;
    Ok(stored)
}

/// [`CategoryMatcher`]'s four variants, spelled as the columns the schema's
/// `CHECK` requires for each `matcher_kind` (spec §4.6).
fn matcher_to_columns(
    matcher: &CategoryMatcher,
) -> (
    &'static str,
    Option<&str>,
    Option<&str>,
    Option<&'static str>,
) {
    match matcher {
        CategoryMatcher::Row { key } => ("row", Some(key.as_str()), None, None),
        CategoryMatcher::SourceCategory { value } => {
            ("source_category", Some(value.as_str()), None, None)
        }
        CategoryMatcher::DescriptionContains { text } => {
            ("description_contains", None, Some(text.as_str()), None)
        }
        CategoryMatcher::Description { text, mode } => {
            ("description", None, Some(text.as_str()), Some(mode.code()))
        }
    }
}

/// The inverse of [`matcher_to_columns`].
///
/// `pub(crate)`: the category-assignment projection (`journal::category_index`)
/// loads active rules straight from `category_rules` inside its own
/// transaction and decodes the same four columns — reusing this keeps the
/// column layout in exactly one place.
pub(crate) fn matcher_from_columns(
    matcher_kind: &str,
    value: Option<String>,
    text: Option<String>,
    description_mode: Option<String>,
) -> Result<CategoryMatcher, StoreError> {
    match matcher_kind {
        "row" => Ok(CategoryMatcher::Row {
            key: value.ok_or(StoreError::InvalidValue {
                field: "value",
                value: String::new(),
            })?,
        }),
        "source_category" => Ok(CategoryMatcher::SourceCategory {
            value: value.ok_or(StoreError::InvalidValue {
                field: "value",
                value: String::new(),
            })?,
        }),
        "description_contains" => Ok(CategoryMatcher::DescriptionContains {
            text: text.ok_or(StoreError::InvalidValue {
                field: "text",
                value: String::new(),
            })?,
        }),
        "description" => {
            let text = text.ok_or(StoreError::InvalidValue {
                field: "text",
                value: String::new(),
            })?;
            let mode = match description_mode.as_deref() {
                Some("equals") => DescriptionMatchMode::Equals,
                Some("starts_with") => DescriptionMatchMode::StartsWith,
                Some("contains") => DescriptionMatchMode::Contains,
                other => {
                    return Err(StoreError::InvalidValue {
                        field: "description_mode",
                        value: other.unwrap_or_default().to_owned(),
                    });
                }
            };
            Ok(CategoryMatcher::Description { text, mode })
        }
        other => Err(StoreError::InvalidValue {
            field: "matcher_kind",
            value: other.to_owned(),
        }),
    }
}

fn retire_category_rule_row(
    conn: &Connection,
    owner: OwnerId,
    id: CategoryRuleId,
) -> Result<(), StoreError> {
    let updated = conn.execute(
        "UPDATE category_rules SET retired_at = ?3
         WHERE owner = ?1 AND id = ?2 AND retired_at IS NULL",
        params![owner.inner().to_string(), id.inner().to_string(), now()],
    )?;
    if updated == 0 {
        return Err(StoreError::NotFound {
            what: "active category rule",
            id: id.inner().to_string(),
        });
    }
    Ok(())
}

fn date_to_text(value: Date) -> String {
    value
        .format(&Iso8601::DATE)
        .expect("date is formatted as ISO-8601")
}

pub(crate) fn text_to_date(value: &str, field: &'static str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::InvalidValue {
        field,
        value: value.to_owned(),
    })
}

pub(crate) fn parse_uuid(value: &str, what: &'static str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
