//! Dictionary of channel operation kinds (§14, iaam-d8b.2.2 epic).
//!
//! The “source code → operation kind” mapping is **data**,
//! not code: the set of codes belongs to the broker, changes without our
//! involvement, and grows more often than releases are published. While it lived
//! in `match`, the system learned of a new code from a rejected import row
//! — that is, when the owner had already tried to calculate something.
//!
//! The kind is stored as a string and is **not interpreted** by storage — for the same
//! reason as the permissions scope and access environment: it is parsed by `iaam-broker`,
//! which semantically owns this dictionary. The list's closed nature
//! is enforced by the schema's `CHECK`, not by this module's code.
//!
//! The owner is not part of the key: the dictionary describes the broker API, not
//! the owner. `OPERATION_TYPE_COUPON` means a coupon for everyone using T-Invest.
//! in T-Invest.

use std::collections::BTreeMap;

use rusqlite::{Transaction, TransactionBehavior, params};

use crate::documents::BrokerCode;
use crate::{SqliteStore, StoreError, now};

/// Origin of the dictionary entry.
///
/// Distinguishing them is mandatory: updating the dictionary from the contract has no
/// right to overwrite the owner's decision, and without the origin these two
/// entries are indistinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindOrigin {
    /// From the broker's published contract.
    Contract,
    /// The owner's decision.
    Owner,
}

impl KindOrigin {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Owner => "owner",
        }
    }
}

/// Dictionary entry proposed for writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerOperationKind {
    /// The kind under which the channel was named.
    pub source_kind: String,
    /// What it turns into. Interpreted by `iaam-broker`.
    pub kind: String,
}

/// How many entries the dictionary accepted and how many it already knew.
///
/// Returned rather than logged: dictionary updates must
/// be able to say exactly what changed; otherwise “succeeded”
/// is indistinguishable from “did nothing”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DictionaryOutcome {
    pub added: usize,
    pub changed: usize,
    pub already_known: usize,
}

impl SqliteStore {
    /// Refresh contract entries within an existing transaction, preserving owner decisions.
    pub(crate) fn extend_broker_operation_kinds_in_transaction(
        transaction: &Transaction<'_>,
        broker: &BrokerCode,
        dictionary: &str,
        entries: &[BrokerOperationKind],
    ) -> Result<DictionaryOutcome, StoreError> {
        let mut outcome = DictionaryOutcome::default();
        let mut insert = transaction.prepare(
            "INSERT INTO broker_operation_kinds
                 (broker, source_kind, kind, origin, dictionary, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (broker, source_kind) DO NOTHING",
        )?;
        let mut update = transaction.prepare(
            "UPDATE broker_operation_kinds
             SET kind = ?3, dictionary = ?4, recorded_at = ?5
             WHERE broker = ?1 AND source_kind = ?2
               AND origin = 'contract' AND kind != ?3",
        )?;
        for entry in entries {
            let inserted = insert.execute(params![
                broker.as_str(),
                entry.source_kind,
                entry.kind,
                KindOrigin::Contract.code(),
                dictionary,
                now(),
            ])?;
            let action = if inserted != 0 {
                outcome.added += 1;
                "added"
            } else if update.execute(params![
                broker.as_str(),
                entry.source_kind,
                entry.kind,
                dictionary,
                now(),
            ])? != 0
            {
                outcome.changed += 1;
                "changed"
            } else {
                outcome.already_known += 1;
                continue;
            };
            tracing::debug!(
                broker = broker.as_str(),
                dictionary,
                source_kind = entry.source_kind,
                kind = entry.kind,
                action,
                "broker operation dictionary entry refreshed"
            );
        }
        Ok(outcome)
    }

    /// Add missing contract entries and update changed contract kinds.
    ///
    /// Owner-origin entries are never overwritten, even when their kind differs.
    pub fn extend_broker_operation_kinds(
        &mut self,
        broker: &BrokerCode,
        dictionary: &str,
        entries: &[BrokerOperationKind],
    ) -> Result<DictionaryOutcome, StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = Self::extend_broker_operation_kinds_in_transaction(
            &transaction,
            broker,
            dictionary,
            entries,
        )?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Record the owner's decision about the kind.
    ///
    /// It overrides the contract entry: the owner knows about their
    /// portfolio what the contract does not.
    pub fn set_broker_operation_kind(
        &mut self,
        broker: &BrokerCode,
        entry: &BrokerOperationKind,
    ) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO broker_operation_kinds
                 (broker, source_kind, kind, origin, dictionary, recorded_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)
             ON CONFLICT (broker, source_kind) DO UPDATE SET
                 kind = excluded.kind,
                 origin = excluded.origin,
                 dictionary = NULL,
                 recorded_at = excluded.recorded_at",
            params![
                broker.as_str(),
                entry.source_kind,
                entry.kind,
                KindOrigin::Owner.code(),
                now(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The entire channel dictionary in a single read.
    ///
    /// Not “kind by code”: parsing is batched, and a per-row query
    /// would turn one export into a thousand database calls.
    pub fn broker_operation_kinds(
        &self,
        broker: &BrokerCode,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT source_kind, kind FROM broker_operation_kinds WHERE broker = ?1")?;
        let rows = statement.query_map(params![broker.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut dictionary = BTreeMap::new();
        for row in rows {
            let (source_kind, kind) = row?;
            dictionary.insert(source_kind, kind);
        }
        Ok(dictionary)
    }
}
