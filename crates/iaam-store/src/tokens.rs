//! Агентские токены (§14).
//!
//! Хранится **хеш** токена, а не токен: утечка файла базы не должна
//! давать доступ к API. Сам хеш считает транспортный слой — хранилище
//! не знает, чем именно, и потому не может ослабить алгоритм.

use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// Права токена. Исчерпаемый `enum`, а не строка в базе: добавление
/// права обязано сломать сборку везде, где его не обработали (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// Полный доступ владельца.
    Owner,
    /// Внешний агент: чтение отчётов и отправка операций в приёмку.
    /// Прямой записи в журнал у него нет — она результат прохождения
    /// приёмки, а не отдельное разрешённое действие (§13).
    Agent,
    /// Только чтение отчётов.
    ReadOnly,
}

impl TokenScope {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Agent => "agent",
            Self::ReadOnly => "read_only",
        }
    }

    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "owner" => Some(Self::Owner),
            "agent" => Some(Self::Agent),
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }

    /// Может ли токен отправлять операции в приёмку.
    #[must_use]
    pub const fn may_submit(self) -> bool {
        match self {
            Self::Owner | Self::Agent => true,
            Self::ReadOnly => false,
        }
    }

    /// Может ли токен управлять другими токенами и справочниками.
    #[must_use]
    pub const fn may_administer(self) -> bool {
        match self {
            Self::Owner => true,
            Self::Agent | Self::ReadOnly => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    pub id: Uuid,
    pub owner: OwnerId,
    pub label: String,
    pub scope: TokenScope,
    pub revoked: bool,
}

/// Выданный токен в том виде, в каком его показывают владельцу.
///
/// **Ни токена, ни его хеша здесь нет и быть не может.** Хеш — это то,
/// что достаточно подставить в `WHERE token_hash = ?`, чтобы система
/// признала запрос своим; список выданных токенов, показывающий хеши,
/// был бы списком отмычек. То, чего структура не несёт, транспорт
/// не может отдать наружу ни ответом, ни логом (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSummary {
    pub id: Uuid,
    pub label: String,
    pub scope: TokenScope,
    pub created_at: String,
    /// Момент отзыва. `None` — токен действует.
    pub revoked_at: Option<String>,
}

impl SqliteStore {
    /// Регистрация токена по его хешу.
    pub fn insert_token(&self, record: &TokenRecord, token_hash: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO api_tokens (id, owner, label, token_hash, scope, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                record.id.to_string(),
                record.owner.inner().to_string(),
                record.label,
                token_hash,
                record.scope.code(),
                now(),
            ],
        )?;
        Ok(())
    }

    /// Поиск действующего токена по хешу. Отозванный не находится.
    pub fn find_token(&self, token_hash: &str) -> Result<Option<TokenRecord>, StoreError> {
        let row: Option<(String, String, String, String)> = self
            .conn
            .query_row(
                "SELECT id, owner, label, scope FROM api_tokens
                 WHERE token_hash = ?1 AND revoked_at IS NULL",
                params![token_hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((id, owner, label, scope)) = row else {
            return Ok(None);
        };
        let scope = TokenScope::parse(&scope).ok_or(StoreError::NotFound {
            what: "token_scope",
            id: scope.clone(),
        })?;
        Ok(Some(TokenRecord {
            id: Uuid::parse_str(&id).map_err(|_| StoreError::NotFound {
                what: "token",
                id: id.clone(),
            })?,
            owner: OwnerId(Uuid::parse_str(&owner).map_err(|_| StoreError::NotFound {
                what: "owner",
                id: owner.clone(),
            })?),
            label,
            scope,
            revoked: false,
        }))
    }

    /// Отзыв токена.
    ///
    /// Владелец входит в запрос, а не проверяется вызывающим:
    /// идентификатор токена не является правом на него, и без владельца
    /// в `WHERE` любой знающий чужой идентификатор отзывал бы чужой
    /// токен (§14). Ничего не обновилось — `NotFound`: отозванный
    /// заранее, несуществующий и чужой обязаны быть неотличимы, иначе
    /// ответ сообщает постороннему, что такая запись есть.
    pub fn revoke_token(&self, owner: OwnerId, id: Uuid) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE api_tokens SET revoked_at = ?3
             WHERE owner = ?1 AND id = ?2 AND revoked_at IS NULL",
            params![owner.inner().to_string(), id.to_string(), now()],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "действующий токен",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Выданные токены владельца, включая отозванные.
    ///
    /// Отозванные показываются по той же причине, что и отозванные
    /// брокерские доступы: «когда токен перестал пускать» является
    /// вопросом, на который нужен ответ. Хеша в ответе нет — см.
    /// `TokenSummary`.
    pub fn list_tokens(&self, owner: OwnerId) -> Result<Vec<TokenSummary>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, label, scope, created_at, revoked_at FROM api_tokens
             WHERE owner = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut tokens = Vec::new();
        for row in rows {
            let (id, label, scope, created_at, revoked_at) = row?;
            tokens.push(TokenSummary {
                id: Uuid::parse_str(&id).map_err(|_| StoreError::NotFound {
                    what: "token",
                    id: id.clone(),
                })?,
                label,
                scope: TokenScope::parse(&scope).ok_or(StoreError::NotFound {
                    what: "token_scope",
                    id: scope.clone(),
                })?,
                created_at,
                revoked_at,
            });
        }
        Ok(tokens)
    }

    /// Журнал использования токена (§14). Пишется на каждый запрос,
    /// включая отклонённый: попытки с отозванным токеном — то, ради
    /// чего журнал и нужен.
    pub fn record_token_use(
        &self,
        token_hash: &str,
        route: &str,
        outcome: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO token_usage (token, used_at, route, outcome) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, now(), route, outcome],
        )?;
        Ok(())
    }
}
