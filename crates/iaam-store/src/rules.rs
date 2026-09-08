//! Owner classification rules (§10.4, spec §4.6).
//!
//! A rule is not deleted; it is retired as of a date: the history has already
//! been classified by it, and after deletion there would be no way to explain it.
//! An edit creates a new row referring to the previous one; the previous row
//! remains exactly as it was when it was used for classification.
//!
//! `RuleMatcher`'s seven independent optional conditions and `Classification`'s
//! tagged outcome are typed columns, not a serialised blob: the store validates
//! and stores each field on its own, and the database's `CHECK` constraints
//! enforce the closed vocabulary (an outcome kind out of the six, a fee origin
//! out of its five, an income kind out of its three) and the shape rule — a
//! condition that asks about nothing cannot be stored at all.

use iaam_core::ids::{AccountId, ClassificationRuleId, OwnerId};
use rusqlite::{Connection, TransactionBehavior, params};

use crate::{SqliteStore, StoreError, now};

/// A stored rule: `RuleMatcher`'s seven conditions plus `Classification`'s
/// tagged outcome, spelled as the columns `crates/iaam-store/migrations/0001_schema.sql`
/// declares for `classification_rules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRule {
    pub id: ClassificationRuleId,
    pub owner: OwnerId,
    /// The owner's decision number in sequence. Sequential within the owner:
    /// history replay uses it to determine which decision triggered it.
    pub version: u32,

    // `RuleMatcher`'s seven independent optional conditions, joined by AND.
    pub counterparty_account: Option<String>,
    pub description_contains: Option<String>,
    /// The word the source used for the operation. Named `source_kind` here,
    /// matching the column; the classifier's own field is named `kind` for a
    /// reason `iaam_ingest::classification::RuleMatcher::kind` documents.
    pub source_kind: Option<String>,
    pub source_category: Option<String>,
    pub owner_category: Option<String>,
    pub source_code: Option<String>,
    /// `"in"` or `"out"`, enforced by the schema's `CHECK`.
    pub movement: Option<String>,

    // `Classification`'s tagged outcome: `outcome_kind` plus the fields the
    // schema's `CHECK` makes conditional on it.
    pub outcome_kind: String,
    pub to_account: Option<AccountId>,
    pub fee_origin: Option<String>,
    pub income_kind: Option<String>,

    pub created_at: String,
    /// The time the rule was retired. `None` means the rule is active.
    pub retired_at: Option<String>,
    /// The rule that replaced this one.
    pub replaces: Option<ClassificationRuleId>,
}

/// A classification rule to write, in the same columns as [`StoredRule`] minus
/// the identity, version and timestamps the store assigns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRule {
    pub counterparty_account: Option<String>,
    pub description_contains: Option<String>,
    pub source_kind: Option<String>,
    pub source_category: Option<String>,
    pub owner_category: Option<String>,
    pub source_code: Option<String>,
    pub movement: Option<String>,
    pub outcome_kind: String,
    pub to_account: Option<AccountId>,
    pub fee_origin: Option<String>,
    pub income_kind: Option<String>,
}

impl SqliteStore {
    /// Create a new rule.
    pub fn insert_rule(&mut self, owner: OwnerId, rule: NewRule) -> Result<StoredRule, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = write_rule(&transaction, owner, rule, None)?;
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
        rule: NewRule,
    ) -> Result<StoredRule, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire(&transaction, owner, previous)?;
        let stored = write_rule(&transaction, owner, rule, Some(previous))?;
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
            "SELECT id, version, counterparty_account, description_contains, source_kind,
                    source_category, owner_category, source_code, movement,
                    outcome_kind, to_account, fee_origin, income_kind,
                    created_at, retired_at, replaces
             FROM classification_rules
             WHERE owner = ?1 AND retired_at IS NULL
             ORDER BY version",
            owner,
        )
    }

    /// All of the owner's rules, including withdrawn ones.
    pub fn rule_history(&self, owner: OwnerId) -> Result<Vec<StoredRule>, StoreError> {
        self.query_rules(
            "SELECT id, version, counterparty_account, description_contains, source_kind,
                    source_category, owner_category, source_code, movement,
                    outcome_kind, to_account, fee_origin, income_kind,
                    created_at, retired_at, replaces
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
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
            ))
        })?;
        let mut rules = Vec::new();
        for row in rows {
            let (
                id,
                version,
                counterparty_account,
                description_contains,
                source_kind,
                source_category,
                owner_category,
                source_code,
                movement,
                outcome_kind,
                to_account,
                fee_origin,
                income_kind,
                created_at,
                retired_at,
                replaces,
            ) = row?;
            rules.push(StoredRule {
                id: ClassificationRuleId(parse_uuid(&id)?),
                owner,
                version,
                counterparty_account,
                description_contains,
                source_kind,
                source_category,
                owner_category,
                source_code,
                movement,
                outcome_kind,
                to_account: to_account
                    .as_deref()
                    .map(parse_uuid)
                    .transpose()?
                    .map(AccountId),
                fee_origin,
                income_kind,
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
    rule: NewRule,
    replaces: Option<ClassificationRuleId>,
) -> Result<StoredRule, StoreError> {
    let used: Option<u32> = conn.query_row(
        "SELECT MAX(version) FROM classification_rules WHERE owner = ?1",
        [owner.inner().to_string()],
        |row| row.get(0),
    )?;
    let stored = StoredRule {
        id: ClassificationRuleId::new_random(),
        owner,
        version: used.map_or(1, |value| value.saturating_add(1)),
        counterparty_account: rule.counterparty_account,
        description_contains: rule.description_contains,
        source_kind: rule.source_kind,
        source_category: rule.source_category,
        owner_category: rule.owner_category,
        source_code: rule.source_code,
        movement: rule.movement,
        outcome_kind: rule.outcome_kind,
        to_account: rule.to_account,
        fee_origin: rule.fee_origin,
        income_kind: rule.income_kind,
        created_at: now(),
        retired_at: None,
        replaces,
    };
    conn.execute(
        "INSERT INTO classification_rules (
             id, owner, version,
             counterparty_account, description_contains, source_kind,
             source_category, owner_category, source_code, movement,
             outcome_kind, to_account, fee_origin, income_kind,
             created_at, retired_at, replaces
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, NULL, ?16
         )",
        params![
            stored.id.inner().to_string(),
            owner.inner().to_string(),
            stored.version,
            stored.counterparty_account,
            stored.description_contains,
            stored.source_kind,
            stored.source_category,
            stored.owner_category,
            stored.source_code,
            stored.movement,
            stored.outcome_kind,
            stored.to_account.map(|id| id.inner().to_string()),
            stored.fee_origin,
            stored.income_kind,
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

fn parse_uuid(value: &str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what: "classification rule",
        id: value.to_owned(),
    })
}
