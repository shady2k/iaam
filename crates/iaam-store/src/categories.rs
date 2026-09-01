//! Owner category reference data.
//!
//! Categories are reference records rather than event fields. They can be
//! reorganized without rewriting the append-only journal, while retirement
//! keeps the records needed to explain historical reports.

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use iaam_core::ids::OwnerId;

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
        let group_exists: Option<()> = transaction
            .query_row(
                "SELECT 1 FROM category_groups WHERE id = ?1 AND owner = ?2",
                params![group_id.to_string(), owner.inner().to_string()],
                |_| Ok(()),
            )
            .optional()?;
        if group_exists.is_none() {
            return Err(StoreError::NotFound {
                what: "category group",
                id: group_id.to_string(),
            });
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

fn parse_uuid(value: &str, what: &'static str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
