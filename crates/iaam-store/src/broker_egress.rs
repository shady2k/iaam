//! The instance's stored broker-egress switch (migration `0008`).
//!
//! Broker egress used to live only in the process environment. The stored
//! switch is the owner's word about this instance: a successful `iaam broker
//! connect` turns it on, `iaam broker off` turns it off, and `serve` reads it
//! at start. The store does not interpret the switch beyond its two values —
//! deciding what may be sent stays with the gateway — and the tally beside the
//! database stays the allowance's only ledger: nothing here records attempts.
//!
//! `first_enabled_at` is the one fact the rest of the system reads beside the
//! switch itself: the first enabling vouches for the fresh zero tally minted
//! beside the database. While it is unset, `connect` may mint that tally; once
//! set, never again — a deleted or emptied egress directory is the
//! conservative recovery state, and no later command restores an allowance.

use rusqlite::TransactionBehavior;

use crate::{SqliteStore, StoreError, now};

/// The stored broker-egress switch of one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerEgressSetting {
    /// Broker requests may leave the machine when the process permits it.
    pub enabled: bool,
    /// The moment of the first enabling, when there was one. Set once, never
    /// cleared and never moved by a later enabling.
    pub first_enabled_at: Option<String>,
}

/// The one row's pinned name. A second row would be two answers to one
/// question; the schema's CHECK refuses any other atom.
const SETTING: &str = "broker egress";

impl SqliteStore {
    /// The stored switch.
    ///
    /// No row means the instance has never enabled broker egress: off, and no
    /// first enabling. The row is written by `broker connect` and `broker
    /// off`, never by `serve`: the service reads the owner's word, it does not
    /// write it.
    pub fn broker_egress(&self) -> Result<BrokerEgressSetting, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT enabled, first_enabled_at FROM broker_egress WHERE setting = ?1")?;
        let mut rows = statement.query([SETTING])?;
        if let Some(row) = rows.next()? {
            return Ok(BrokerEgressSetting {
                enabled: row.get::<_, i64>(0)? != 0,
                first_enabled_at: row.get(1)?,
            });
        }
        Ok(BrokerEgressSetting {
            enabled: false,
            first_enabled_at: None,
        })
    }

    /// Store the switch.
    ///
    /// Enabling records the moment of the first enabling and every later
    /// enabling keeps the first: the voucher is for the one tally minted at
    /// it, and a second moment would describe a second mint nobody made.
    /// Disabling leaves the first enabling standing — turning the switch off
    /// does not un-happen it.
    pub fn set_broker_egress_enabled(&mut self, enabled: bool) -> Result<(), StoreError> {
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        set_enabled_in_transaction(&transaction, enabled)?;
        transaction.commit()?;
        Ok(())
    }
}

/// The switch's one row, written inside a transaction the caller commits.
///
/// This is how `connect` turns the switch on in the same transaction that
/// stores the credential: the enabling — including the first-enabling
/// moment — either lands with that write or not at all.
pub(crate) fn set_enabled_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    enabled: bool,
) -> Result<(), StoreError> {
    let at = now();
    transaction.execute(
        "INSERT INTO broker_egress (setting, enabled, first_enabled_at, recorded_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(setting) DO UPDATE SET
            enabled = excluded.enabled,
            first_enabled_at =
                COALESCE(broker_egress.first_enabled_at, excluded.first_enabled_at),
            recorded_at = excluded.recorded_at",
        rusqlite::params![
            SETTING,
            i64::from(enabled),
            enabled.then_some(at.as_str()),
            at.as_str(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_store() -> SqliteStore {
        SqliteStore::open_in_memory().expect("fresh store opens")
    }

    #[test]
    fn a_fresh_instance_reads_off_with_no_first_enabling() {
        let store = fresh_store();
        let setting = store.broker_egress().expect("the switch reads");
        assert!(!setting.enabled);
        assert_eq!(setting.first_enabled_at, None);
    }

    #[test]
    fn enabling_records_the_first_moment_once_across_off_and_on() {
        let mut store = fresh_store();

        store
            .set_broker_egress_enabled(true)
            .expect("the switch turns on");
        let first = store.broker_egress().expect("the switch reads");
        assert!(first.enabled);
        let first_enabled_at = first
            .first_enabled_at
            .expect("the first enabling is recorded");

        store
            .set_broker_egress_enabled(false)
            .expect("the switch turns off");
        let off = store.broker_egress().expect("the switch reads");
        assert!(!off.enabled);
        assert_eq!(
            off.first_enabled_at.as_deref(),
            Some(first_enabled_at.as_str()),
            "disabling does not un-happen the first enabling"
        );

        // The stamps carry sub-second precision, so this pause is what makes
        // "kept the first moment" differ from "wrote the moment again".
        std::thread::sleep(std::time::Duration::from_millis(50));
        store
            .set_broker_egress_enabled(true)
            .expect("the switch turns on again");
        let again = store.broker_egress().expect("the switch reads");
        assert_eq!(
            again.first_enabled_at.as_deref(),
            Some(first_enabled_at.as_str()),
            "a later enabling keeps the first moment"
        );
    }

    #[test]
    fn the_stored_switch_survives_reopening_the_database() {
        let path = std::env::temp_dir().join(format!(
            "iaam-broker-egress-reopen-{}.db",
            uuid::Uuid::new_v4()
        ));
        {
            let mut store = SqliteStore::open(&path).expect("fresh store opens");
            store
                .set_broker_egress_enabled(true)
                .expect("the switch turns on");
        }
        let store = SqliteStore::open(&path).expect("the store reopens");
        assert!(store.broker_egress().expect("the switch reads").enabled);
        std::fs::remove_file(&path).expect("the scratch database is removed");
    }
}
