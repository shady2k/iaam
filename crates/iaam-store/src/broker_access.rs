//! Broker channel access (§14).
//!
//! The store keeps the nonce and ciphertext as **opaque bytes**:
//! decryption lives in `iaam-broker`, and the storage adapter knows
//! nothing about it. An adapter that knows the neighboring adapter's
//! cryptography turns the layers into a tangled mess.
//!
//! Scope and environment are stored as strings for the same reason that
//! `matcher` and `outcome` are: the store does not interpret them; it stores them.
//! `iaam-broker` parses them: a string promising trading permissions is denied
//! there rather than granted access, while the environment determines which gateway to use.
//!
//! A broker may have multiple environments, and their tokens are **different**:
//! the production token is not accepted by the sandbox, and the sandbox token is not accepted in production.
//! Therefore, active access is defined by the owner+broker+environment tuple,
//! not the owner+broker pair.

use iaam_core::ids::OwnerId;
use rusqlite::{TransactionBehavior, params};
use uuid::Uuid;

use crate::documents::BrokerCode;
use crate::{SqliteStore, StoreError, now};

/// Access to be provisioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBrokerAccess {
    pub id: Uuid,
    pub owner: OwnerId,
    pub broker: BrokerCode,
    /// Broker environment: production or sandbox. Interpreted in `iaam-broker`.
    pub environment: String,
    pub scope: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// New cryptographic components for an existing access record.
///
/// The store preserves the record's identity and history; only the
/// nonce and ciphertext change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerAccessCiphertext {
    pub id: Uuid,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Stored access.
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
    /// Revocation time. `None` means the access is active.
    pub revoked_at: Option<String>,
}

impl BrokerAccess {
    /// A `nonce` and ciphertext pair for decryption in `iaam-broker`.
    ///
    /// Returned as a tuple rather than a cryptographic type: the store
    /// does not depend on `iaam-broker` and cannot construct its type.
    #[must_use]
    pub fn sealed_parts(&self) -> (&[u8], &[u8]) {
        (&self.nonce, &self.ciphertext)
    }
}

/// Who to consider the owner when none was named explicitly.
///
/// The owner's identifier is never printed anywhere—when the token is issued
/// externally, only the token itself is sent out—and the person has no way to obtain it.
/// Therefore, the system can identify the sole owner itself, but
/// refuses to choose among several: creating broker access
/// for the wrong owner means discovering it through someone else's trades
/// in the portfolio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoleOwner {
    /// The token has not yet been issued.
    None,
    Single(OwnerId),
    /// There are multiple owners: no choice can be made on the person's behalf.
    Several,
}

impl SqliteStore {
    /// The owner, if there is only one in the system.
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
                    what: "owner",
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

    /// Provisioning access.
    ///
    /// A second active access to the same broker is rejected
    /// by the unique index: it is unknown which of the two the system
    /// would use to access the broker.
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
        // A uniqueness violation means “access already exists in this environment”,

        // not a storage failure. Handle it here: otherwise it propagates through the layers.
        // the answer to the owner's question, not SQLite text that
        // reveals the schema's structure externally.
        if let Err(rusqlite::Error::SqliteFailure(error, _)) = &inserted
            && error.code == rusqlite::ErrorCode::ConstraintViolation
        {
            return Err(StoreError::AlreadyExists {
                what: "active broker access in this environment",
            });
        }
        inserted?;
        transaction.commit()?;
        Ok(())
    }

    /// Atomically replaces the ciphertexts of all supplied accesses.
    ///
    /// The list is built by the calling layer after decryption with the old key.
    /// Revoked rows are also part of the history and therefore must be
    /// passed here. If even one identifier is missing, the transaction
    /// is rolled back in its entirety.
    pub fn rotate_broker_access_ciphertexts(
        &mut self,
        replacements: &[BrokerAccessCiphertext],
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for replacement in replacements {
            let changed = transaction.execute(
                "UPDATE broker_access
                 SET nonce = ?1, ciphertext = ?2
                 WHERE id = ?3",
                params![
                    replacement.nonce,
                    replacement.ciphertext,
                    replacement.id.to_string()
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::NotFound {
                    what: "broker access",
                    id: replacement.id.to_string(),
                });
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// The owner's active broker access in the named environment.
    ///
    /// Revoked access is not returned: a revoked access is not used to reach the broker. The environment
    /// is mandatory and has no default: access selected on the caller's behalf
    /// means a trip into someone else's environment, detected from someone else's response.
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

    /// All of the owner's accesses, including revoked ones.
    pub fn broker_access_history(&self, owner: OwnerId) -> Result<Vec<BrokerAccess>, StoreError> {
        self.query_access(
            "SELECT id, broker, environment, scope, nonce, ciphertext, created_at, revoked_at
             FROM broker_access WHERE owner = ?1 ORDER BY created_at, id",
            params![owner.inner().to_string()],
            owner,
        )
    }

    /// Revoke access.
    ///
    /// Not deletion: revoked access remains part of the history—“when the
    /// system stopped accessing the broker” is a question
    /// that needs an answer.
    ///
    /// All accesses for all owners, including revoked ones.
    pub fn all_broker_access_history(&self) -> Result<Vec<BrokerAccess>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT owner FROM broker_access")?;
        let owners = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut history = Vec::new();
        for owner in owners {
            let owner = owner?;
            let owner = OwnerId(Uuid::parse_str(&owner).map_err(|_| StoreError::NotFound {
                what: "owner",
                id: owner,
            })?);
            history.extend(self.broker_access_history(owner)?);
        }
        Ok(history)
    }
    pub fn revoke_broker_access(&mut self, owner: OwnerId, id: Uuid) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE broker_access SET revoked_at = ?3
             WHERE owner = ?1 AND id = ?2 AND revoked_at IS NULL",
            params![owner.inner().to_string(), id.to_string(), now()],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound {
                what: "active broker access",
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
                    what: "broker access",
                    id: id.clone(),
                })?,
                owner,
                broker: BrokerCode::parse(&broker).ok_or(StoreError::DocumentDecode {
                    id,
                    detail: "broker code is empty".to_owned(),
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
