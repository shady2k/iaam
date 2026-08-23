//! Правила классификации владельца (§10.4).
//!
//! Правило не удаляется, а выводится из обращения датой: по нему уже
//! классифицирована история, и объяснять её после удаления будет нечем.
//! Правка заводит новую строку, ссылающуюся на прежнюю, — прежняя
//! остаётся ровно такой, какой была в момент, когда по ней считали.
//!
//! `matcher` и `outcome` — JSON доменных типов классификатора.
//! Хранилище не знает их устройства: оно хранит. Но разбираемость как
//! JSON проверяет при записи — правило, которое классификатор не сможет
//! прочитать, не должно ложиться в базу молча.

use iaam_core::ids::{ClassificationRuleId, OwnerId};
use rusqlite::{Connection, TransactionBehavior, params};

use crate::{SqliteStore, StoreError, now};

/// Сохранённое правило.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRule {
    pub id: ClassificationRuleId,
    pub owner: OwnerId,
    /// Номер решения владельца по порядку. Сквозной внутри владельца:
    /// пересчёт истории по нему узнаёт, каким решением он вызван.
    pub version: u32,
    pub matcher: String,
    pub outcome: String,
    pub created_at: String,
    /// Момент вывода из обращения. `None` — правило действует.
    pub retired_at: Option<String>,
    /// Правило, которое это заменило.
    pub replaces: Option<ClassificationRuleId>,
}

impl SqliteStore {
    /// Заведение нового правила.
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

    /// Правка правила: прежнее выводится из обращения, новое заводится.
    ///
    /// Обе записи идут одной транзакцией: между ними нет момента, когда
    /// действуют оба правила или не действует ни одного, — а классификация,
    /// попавшая в такой момент, была бы необъяснима.
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

    /// Вывод правила из обращения.
    ///
    /// Уже выведенное правило вывести нельзя: дата вывода не
    /// переписывается задним числом.
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

    /// Действующие правила владельца в порядке решений.
    pub fn list_active_rules(&self, owner: OwnerId) -> Result<Vec<StoredRule>, StoreError> {
        self.query_rules(
            "SELECT id, version, matcher, outcome, created_at, retired_at, replaces
             FROM classification_rules
             WHERE owner = ?1 AND retired_at IS NULL
             ORDER BY version",
            owner,
        )
    }

    /// Все правила владельца, включая выведенные из обращения.
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

/// Вставка правила со следующим номером решения.
///
/// Номер назначается в той же транзакции, что и вставка: раздельно это
/// та же гонка, что в журнале, — два одновременных запроса получают один
/// номер, и порядок правил перестаёт быть порядком.
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

/// Вывод из обращения с проверкой владельца.
///
/// Отсутствие, чужое владение и уже выведенное правило дают одну
/// ошибку: разные ответы сообщили бы постороннему, что такое правило
/// существует.
fn retire(conn: &Connection, owner: OwnerId, id: ClassificationRuleId) -> Result<(), StoreError> {
    let updated = conn.execute(
        "UPDATE classification_rules SET retired_at = ?3
         WHERE owner = ?1 AND id = ?2 AND retired_at IS NULL",
        params![owner.inner().to_string(), id.inner().to_string(), now()],
    )?;
    if updated == 0 {
        return Err(StoreError::NotFound {
            what: "действующее правило классификации",
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
        what: "правило классификации",
        id: value.to_owned(),
    })
}
