//! Owner category reference data.
//!
//! Categories are reference records rather than event fields. They can be
//! reorganized without rewriting the append-only journal, while retirement
//! keeps the records needed to explain historical reports.

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
    pub matcher_json: String,
    pub category: CategoryId,
    pub valid_from: Option<Date>,
    pub valid_to: Option<Date>,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<CategoryRuleId>,
}


impl SqliteStore {
    /// Add a category group and return its stable identifier.
    pub fn insert_category_group(
        &mut self,
        owner: OwnerId,
        title: &str,
    ) -> Result<Uuid, StoreError> {
        let id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO category_groups (id, owner, title, created_at, retired_at)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![id.to_string(), owner.inner().to_string(), title, now()],
        )?;
        Ok(id)
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
        matcher_json: &str,
        category: Uuid,
        valid_from: Option<Date>,
        valid_to: Option<Date>,
        replaces: Option<CategoryRuleId>,
    ) -> Result<CategoryRuleRow, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = write_category_rule(
            &transaction,
            owner,
            matcher_json,
            category,
            valid_from,
            valid_to,
            replaces,
        )?;
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
        matcher_json: &str,
        category: Uuid,
        valid_from: Option<Date>,
        valid_to: Option<Date>,
    ) -> Result<CategoryRuleRow, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_category_rule_row(&transaction, owner, previous)?;
        let stored = write_category_rule(
            &transaction,
            owner,
            matcher_json,
            category,
            valid_from,
            valid_to,
            Some(previous),
        )?;
        transaction.commit()?;
        Ok(stored)
    }

    /// List every category rule owned by the portfolio owner, including retired
    /// rows, in version order.
    pub fn list_category_rules(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<CategoryRuleRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, version, matcher, category, valid_from, valid_to,
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
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;
        let mut rules = Vec::new();
        for row in rows {
            let (
                id,
                version,
                matcher_json,
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
                matcher_json,
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
    pub fn retire_category_group(
        &mut self,
        owner: OwnerId,
        id: Uuid,
    ) -> Result<(), StoreError> {
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
    matcher_json: &str,
    category: Uuid,
    valid_from: Option<Date>,
    valid_to: Option<Date>,
    replaces: Option<CategoryRuleId>,
) -> Result<CategoryRuleRow, StoreError> {
    check_json(matcher_json)?;
    let category_exists: Option<()> = conn
        .query_row(
            "SELECT 1 FROM categories WHERE id = ?1 AND owner = ?2",
            params![category.to_string(), owner.inner().to_string()],
            |_| Ok(()),
        )
        .optional()?;
    if category_exists.is_none() {
        return Err(StoreError::NotFound {
            what: "category",
            id: category.to_string(),
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
        matcher_json: matcher_json.to_owned(),
        category: CategoryId(category),
        valid_from,
        valid_to,
        created_at: now(),
        retired_at: None,
        replaces,
    };
    conn.execute(
        "INSERT INTO category_rules (
             id, owner, version, matcher, category, valid_from, valid_to,
             created_at, retired_at, replaces
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)",
        params![
            stored.id.inner().to_string(),
            owner.inner().to_string(),
            stored.version,
            &stored.matcher_json,
            stored.category.inner().to_string(),
            stored.valid_from.map(date_to_text),
            stored.valid_to.map(date_to_text),
            &stored.created_at,
            replaces.map(|id| id.inner().to_string()),
        ],
    )?;
    Ok(stored)
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

fn check_json(value: &str) -> Result<(), StoreError> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|source| StoreError::RuleNotJson {
            field: "matcher",
            source,
        })
}

fn date_to_text(value: Date) -> String {
    value
        .format(&Iso8601::DATE)
        .expect("date is formatted as ISO-8601")
}

fn text_to_date(value: &str, field: &'static str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::InvalidValue {
        field,
        value: value.to_owned(),
    })
}

fn parse_uuid(value: &str, what: &'static str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
