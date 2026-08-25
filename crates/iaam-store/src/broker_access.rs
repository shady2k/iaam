//! Доступ к брокерскому каналу (§14).
//!
//! Хранилище держит nonce и шифротекст **непрозрачными байтами**:
//! расшифровка живёт в `iaam-broker`, и адаптер хранилища о ней
//! не знает. Адаптер, знающий криптографию соседнего адаптера,
//! превращает слои в клубок.
//!
//! Область прав и среда хранятся строками по той же причине, что
//! `matcher` и `outcome` правил: хранилище их не толкует, оно хранит.
//! Разбирает их `iaam-broker`: строка, обещающая торговые права, даёт
//! там отказ, а не доступ, а среда решает, к какому шлюзу идти.
//!
//! Сред у брокера может быть несколько, и токены у них **разные**:
//! боевой токен песочница не принимает, песочный не принимает бой.
//! Поэтому действующий доступ один на тройку владелец+брокер+среда,
//! а не на пару владелец+брокер.

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
    /// Среда брокера: боевая или песочница. Толкуется в `iaam-broker`.
    pub environment: String,
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
    pub environment: String,
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

/// Кого считать владельцем, когда его не назвали явно.
///
/// Идентификатор владельца нигде не печатается — при выпуске токена
/// наружу уходит только сам токен, — и человеку взять его неоткуда.
/// Поэтому единственного владельца система умеет узнать сама, а
/// выбирать между несколькими отказывается: завести брокерский доступ
/// не тому владельцу означает обнаружить это по чужим сделкам
/// в портфеле.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoleOwner {
    /// Токен ещё не выпускался.
    None,
    Single(OwnerId),
    /// Владельцев несколько: выбирать за человека нельзя.
    Several,
}

impl SqliteStore {
    /// Владелец, если он в системе один.
    pub fn sole_token_owner(&self) -> Result<SoleOwner, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT owner FROM api_tokens LIMIT 2")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut owners = Vec::new();
        for row in rows {
            let raw = row?;
            owners.push(OwnerId(Uuid::parse_str(&raw).map_err(|_| {
                StoreError::NotFound {
                    what: "владелец",
                    id: raw,
                }
            })?));
        }
        Ok(match owners.as_slice() {
            [] => SoleOwner::None,
            [single] => SoleOwner::Single(*single),
            _ => SoleOwner::Several,
        })
    }

    /// Заведение доступа.
    ///
    /// Второй действующий доступ к тому же брокеру отбивается
    /// уникальным индексом: неизвестно, каким из двух система ходила
    /// бы к брокеру.
    pub fn insert_broker_access(&mut self, access: &NewBrokerAccess) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT INTO broker_access (
                 id, owner, broker, environment, scope, nonce, ciphertext, created_at, revoked_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
            params![
                access.id.to_string(),
                access.owner.inner().to_string(),
                access.broker.as_str(),
                access.environment,
                access.scope,
                access.nonce,
                access.ciphertext,
                now(),
            ],
        );
        // Нарушение уникальности — это «доступ в этой среде уже есть»,
        // а не сбой хранилища. Разбирается здесь: дальше по слоям едет
        // ответ на вопрос владельца, а не текст SQLite, из которого
        // наружу видно устройство схемы.
        if let Err(rusqlite::Error::SqliteFailure(error, _)) = &inserted
            && error.code == rusqlite::ErrorCode::ConstraintViolation
        {
            return Err(StoreError::AlreadyExists {
                what: "действующий доступ к брокеру в этой среде",
            });
        }
        inserted?;
        transaction.commit()?;
        Ok(())
    }

    /// Действующий доступ владельца к брокеру в названной среде.
    ///
    /// Отозванный не находится: отозванным доступом не ходят. Среда
    /// обязательна и умолчания не имеет: доступ, выбранный за вызывающего,
    /// — это поход в чужую среду, замеченный по чужому ответу.
    pub fn find_broker_access(
        &self,
        owner: OwnerId,
        broker: &BrokerCode,
        environment: &str,
    ) -> Result<Option<BrokerAccess>, StoreError> {
        let mut found = self.query_access(
            "SELECT id, broker, environment, scope, nonce, ciphertext, created_at, revoked_at
             FROM broker_access
             WHERE owner = ?1 AND broker = ?2 AND environment = ?3 AND revoked_at IS NULL",
            params![owner.inner().to_string(), broker.as_str(), environment],
            owner,
        )?;
        Ok(found.pop())
    }

    /// Все доступы владельца, включая отозванные.
    pub fn broker_access_history(&self, owner: OwnerId) -> Result<Vec<BrokerAccess>, StoreError> {
        self.query_access(
            "SELECT id, broker, environment, scope, nonce, ciphertext, created_at, revoked_at
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
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, Vec<u8>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut access = Vec::new();
        for row in rows {
            let (id, broker, environment, scope, nonce, ciphertext, created_at, revoked_at) = row?;
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
                environment,
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
