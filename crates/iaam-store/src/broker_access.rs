//! Доступ к брокерскому каналу (§14).
//!
//! Хранилище держит nonce и шифротекст **непрозрачными байтами**:
//! расшифровка живёт в `iaam-broker`, и адаптер хранилища о ней
//! не знает. Адаптер, знающий криптографию соседнего адаптера,
//! превращает слои в клубок.
//!
//! Область прав хранится строкой по той же причине, что `matcher`
//! и `outcome` правил: хранилище её не толкует, оно хранит. Разбирает
//! её `iaam-broker`, и строка, обещающая торговые права, там даёт
//! отказ, а не доступ.

use iaam_core::ids::OwnerId;
use rusqlite::{TransactionBehavior, params};
use uuid::Uuid;

use crate::documents::BrokerCode;
use crate::{SqliteStore, StoreError, now};

/// Заводимый доступ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBrokerAccess {
    pub id: Uuid,
    pub owner: OwnerId,
    pub broker: BrokerCode,
    pub scope: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Сохранённый доступ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerAccess {
    pub id: Uuid,
    pub owner: OwnerId,
    pub broker: BrokerCode,
    pub scope: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub created_at: String,
    /// Момент отзыва. `None` — доступ действует.
    pub revoked_at: Option<String>,
}

impl BrokerAccess {
    /// Пара «nonce и шифротекст» для расшифровки в `iaam-broker`.
    ///
    /// Возвращается кортежем, а не типом криптографии: хранилище
    /// не зависит от `iaam-broker` и собрать его тип не может.
    #[must_use]
    pub fn sealed_parts(&self) -> (&[u8], &[u8]) {
        (&self.nonce, &self.ciphertext)
    }
}

impl SqliteStore {
    /// Заведение доступа.
    ///
    /// Второй действующий доступ к тому же брокеру отбивается
    /// уникальным индексом: неизвестно, каким из двух система ходила
    /// бы к брокеру.
    pub fn insert_broker_access(&mut self, access: &NewBrokerAccess) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO broker_access (
                 id, owner, broker, scope, nonce, ciphertext, created_at, revoked_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                access.id.to_string(),
                access.owner.inner().to_string(),
                access.broker.as_str(),
                access.scope,
                access.nonce,
                access.ciphertext,
                now(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Действующий доступ владельца к брокеру.
    ///
    /// Отозванный не находится: отозванным доступом не ходят.
    pub fn find_broker_access(
        &self,
        owner: OwnerId,
        broker: &BrokerCode,
    ) -> Result<Option<BrokerAccess>, StoreError> {
        let mut found = self.query_access(
            "SELECT id, broker, scope, nonce, ciphertext, created_at, revoked_at
             FROM broker_access
             WHERE owner = ?1 AND broker = ?2 AND revoked_at IS NULL",
            params![owner.inner().to_string(), broker.as_str()],
            owner,
        )?;
        Ok(found.pop())
    }

    /// Все доступы владельца, включая отозванные.
    pub fn broker_access_history(&self, owner: OwnerId) -> Result<Vec<BrokerAccess>, StoreError> {
        self.query_access(
            "SELECT id, broker, scope, nonce, ciphertext, created_at, revoked_at
             FROM broker_access WHERE owner = ?1 ORDER BY created_at, id",
            params![owner.inner().to_string()],
            owner,
        )
    }

    /// Отзыв доступа.
    ///
    /// Не удаление: отозванный доступ остаётся историей — «когда
    /// система перестала ходить к брокеру» является вопросом, на
    /// который нужен ответ.
    pub fn revoke_broker_access(&mut self, owner: OwnerId, id: Uuid) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE broker_access SET revoked_at = ?3
             WHERE owner = ?1 AND id = ?2 AND revoked_at IS NULL",
            params![owner.inner().to_string(), id.to_string(), now()],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "действующий доступ к брокеру",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    fn query_access(
        &self,
        sql: &str,
        parameters: &[&dyn rusqlite::ToSql],
        owner: OwnerId,
    ) -> Result<Vec<BrokerAccess>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(parameters, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut access = Vec::new();
        for row in rows {
            let (id, broker, scope, nonce, ciphertext, created_at, revoked_at) = row?;
            access.push(BrokerAccess {
                id: Uuid::parse_str(&id).map_err(|_| StoreError::NotFound {
                    what: "доступ к брокеру",
                    id: id.clone(),
                })?,
                owner,
                broker: BrokerCode::parse(&broker).ok_or(StoreError::DocumentDecode {
                    id,
                    detail: "код брокера пуст".to_owned(),
                })?,
                scope,
                nonce,
                ciphertext,
                created_at,
                revoked_at,
            });
        }
        Ok(access)
    }
}
