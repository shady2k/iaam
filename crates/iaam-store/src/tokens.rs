//! Agent tokens (§14).
//!
//! The **hash** of the token is stored, not the token itself: a database file leak must not
//! grant access to the API. The transport layer computes the hash itself—the store
//! does not know how, and therefore cannot weaken the algorithm.

use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::{SqliteStore, StoreError, now};

/// Token permissions. An exhaustive `enum`, not a database string: adding a
/// permission must break the build everywhere it has not been handled (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// Full owner access.
    Owner,
    /// External agent: report reading and submitting operations for acceptance.
    /// It has no direct journal write access—that is the result of passing
    /// acceptance, not a separately permitted action (§13).
    Agent,
    /// Report reading only.
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

    /// Whether the token can submit operations for acceptance.
    #[must_use]
    pub const fn may_submit(self) -> bool {
        match self {
            Self::Owner | Self::Agent => true,
            Self::ReadOnly => false,
        }
    }

    /// Whether the token can manage other tokens and reference data.
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

/// An issued token in the form in which it is shown to the owner.
///
/// **Neither the token nor its hash can be present here.** The hash is what
/// is sufficient to substitute into `WHERE token_hash = ?` for the system to
/// recognize the request as its own; a list of issued tokens showing hashes
/// would be a list of skeleton keys. What the structure does not carry, the transport
/// cannot expose externally, either in a response or a log (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSummary {
    pub id: Uuid,
    pub label: String,
    pub scope: TokenScope,
    pub created_at: String,
    /// Revocation time. `None` — the token is active.
    pub revoked_at: Option<String>,
}

impl SqliteStore {
    /// Register a token by its hash.
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

    /// Find an active token by its hash. A revoked token is not found.
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

    /// Revoke a token.
    ///
    /// The owner is part of the query rather than being checked by the caller:
    /// a token identifier is not authorization to use it, and without the owner
    /// in `WHERE`, anyone who knows someone else's identifier could revoke that person's
    /// token (§14). If nothing was updated, `NotFound`: a token revoked
    /// beforehand, nonexistent, and belonging to someone else must be indistinguishable; otherwise
    /// the response tells an outsider that such a record exists.
    pub fn revoke_token(&self, owner: OwnerId, id: Uuid) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE api_tokens SET revoked_at = ?3
             WHERE owner = ?1 AND id = ?2 AND revoked_at IS NULL",
            params![owner.inner().to_string(), id.to_string(), now()],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "active token",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Tokens issued to the owner, including revoked ones.
    ///
    /// Revoked tokens are shown for the same reason as revoked
    /// broker access: “when a token stopped granting access” is a
    /// question that needs an answer. The hash is not included in the response—see
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

    /// Token usage log (§14). Written for every request,
    /// including rejected ones: attempts using a revoked token are exactly
    /// what the log is for.
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
