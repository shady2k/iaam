//! Owner classification rules (§10.4).
//!
//! A rule is not deleted; it is retired as of a date: the history has already
//! been classified by it, and after deletion there would be no way to explain it.
//! An edit creates a new row referring to the previous one; the previous row
//! remains exactly as it was when it was used for classification.
//!
//! `matcher` and `outcome` are JSON values of the classifier's domain types.
//! The store does not know their structure: it stores them. But it validates
//! that they can be parsed as JSON on write—a rule the classifier cannot
//! read must not be silently written to the database.

use iaam_core::ids::{ClassificationRuleId, OwnerId};
use rusqlite::{Connection, TransactionBehavior, params};

use crate::{SqliteStore, StoreError, now};

/// A stored rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRule {
    pub id: ClassificationRuleId,
    pub owner: OwnerId,
    /// The owner's decision number in sequence. Sequential within the owner:
    /// history replay uses it to determine which decision triggered it.
    pub version: u32,
    pub matcher: String,
    pub outcome: String,
    pub created_at: String,
    /// The time the rule was retired. `None` means the rule is active.
    pub retired_at: Option<String>,
    /// The rule that replaced this one.
    pub replaces: Option<ClassificationRuleId>,
}

impl SqliteStore {
    /// Create a new rule.
    pub fn insert_rule(
        &mut self,
        owner: OwnerId,
        matcher: &str,
        outcome: &str,
    ) -> Result<StoredRule, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = write_rule(&transaction, owner, matcher, outcome, None)?;
        transaction.commit()?;
        Ok(stored)
    }

    /// Edit a rule: retire the previous one and create a new one.
    ///
    /// Both records are written in one transaction: there is no point at which
    /// both rules are active or neither is active—and a classification
    /// occurring at such a point would be inexplicable.
    pub fn amend_rule(
        &mut self,
        owner: OwnerId,
        previous: ClassificationRuleId,
        matcher: &str,
        outcome: &str,
    ) -> Result<StoredRule, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire(&transaction, owner, previous)?;
        let stored = write_rule(&transaction, owner, matcher, outcome, Some(previous))?;
        transaction.commit()?;
        Ok(stored)
    }

    /// Retire a rule.
    ///
    /// A withdrawn rule cannot be withdrawn again: the withdrawal date is not
    /// rewritten retroactively.
    pub fn retire_rule(
        &mut self,
        owner: OwnerId,
        id: ClassificationRuleId,
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire(&transaction, owner, id)?;
        transaction.commit()?;
        Ok(())
    }

    /// The owner's active rules in decision order.
    pub fn list_active_rules(&self, owner: OwnerId) -> Result<Vec<StoredRule>, StoreError> {
        self.query_rules(
            "SELECT id, version, matcher, outcome, created_at, retired_at, replaces
             FROM classification_rules
             WHERE owner = ?1 AND retired_at IS NULL
             ORDER BY version",
            owner,
        )
    }

    /// All of the owner's rules, including withdrawn ones.
    pub fn rule_history(&self, owner: OwnerId) -> Result<Vec<StoredRule>, StoreError> {
        self.query_rules(
            "SELECT id, version, matcher, outcome, created_at, retired_at, replaces
             FROM classification_rules
             WHERE owner = ?1
             ORDER BY version",
            owner,
        )
    }

    fn query_rules(&self, sql: &str, owner: OwnerId) -> Result<Vec<StoredRule>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut rules = Vec::new();
        for row in rows {
            let (id, version, matcher, outcome, created_at, retired_at, replaces) = row?;
            rules.push(StoredRule {
                id: ClassificationRuleId(parse_uuid(&id)?),
                owner,
                version,
                matcher,
                outcome,
                created_at,
                retired_at,
                replaces: replaces
                    .as_deref()
                    .map(parse_uuid)
                    .transpose()?
                    .map(ClassificationRuleId),
            });
        }
        Ok(rules)
    }
}

/// Insert a rule with the next decision number.
///
/// The number is assigned in the same transaction as the insertion: doing this
/// separately creates the same race as in the journal—two concurrent requests receive one
/// number, and the rule order ceases to be the decision order.
fn write_rule(
    conn: &Connection,
    owner: OwnerId,
    matcher: &str,
    outcome: &str,
    replaces: Option<ClassificationRuleId>,
) -> Result<StoredRule, StoreError> {
    check_json(matcher, "matcher")?;
    check_json(outcome, "outcome")?;
    let used: Option<u32> = conn.query_row(
        "SELECT MAX(version) FROM classification_rules WHERE owner = ?1",
        [owner.inner().to_string()],
        |row| row.get(0),
    )?;
    let stored = StoredRule {
        id: ClassificationRuleId::new_random(),
        owner,
        version: used.map_or(1, |value| value.saturating_add(1)),
        matcher: matcher.to_owned(),
        outcome: outcome.to_owned(),
        created_at: now(),
        retired_at: None,
        replaces,
    };
    conn.execute(
        "INSERT INTO classification_rules (
             id, owner, version, matcher, outcome, created_at, retired_at, replaces
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
        params![
            stored.id.inner().to_string(),
            owner.inner().to_string(),
            stored.version,
            stored.matcher,
            stored.outcome,
            stored.created_at,
            replaces.map(|id| id.inner().to_string()),
        ],
    )?;
    Ok(stored)
}

/// Withdraw a rule after checking its owner.
///
/// A missing rule, a rule owned by someone else, and an already withdrawn rule produce one
/// error: different responses would tell an outsider that such a rule
/// exists.
fn retire(conn: &Connection, owner: OwnerId, id: ClassificationRuleId) -> Result<(), StoreError> {
    let updated = conn.execute(
        "UPDATE classification_rules SET retired_at = ?3
         WHERE owner = ?1 AND id = ?2 AND retired_at IS NULL",
        params![owner.inner().to_string(), id.inner().to_string(), now()],
    )?;
    if updated == 0 {
        return Err(StoreError::NotFound {
            what: "active classification rule",
            id: id.inner().to_string(),
        });
    }
    Ok(())
}

fn check_json(value: &str, field: &'static str) -> Result<(), StoreError> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|source| StoreError::RuleNotJson { field, source })
}

fn parse_uuid(value: &str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what: "classification rule",
        id: value.to_owned(),
    })
}
