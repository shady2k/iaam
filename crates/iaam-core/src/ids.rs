//! Separate identities (§4.5).
//!
//! A brokerage account is not simultaneously an owner, a cash account, and a
//! place where securities are held: moving securities between custodians at
//! one broker is a real operation.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new_random() -> Self {
                Self(Uuid::new_v4())
            }

            #[must_use]
            pub const fn inner(&self) -> Uuid {
                self.0
            }
        }
    };
}

typed_id!(
    /// Portfolio owner.
    OwnerId
);
typed_id!(
    /// Cash account: brokerage account, bank account, deposit, or wallet.
    AccountId
);
typed_id!(
    /// Place where securities are held (custodian or sub-account).
    CustodyId
);
typed_id!(
    /// Instrument.
    InstrumentId
);
typed_id!(
    /// Data source: a specific report, synchronisation, or manual entry.
    SourceId
);
typed_id!(
    /// Journal event.
    EventId
);
typed_id!(
    /// Transfer of money between accounts. Links both sides of the movement:
    /// without it, the contour classifier cannot see the other account (§4.10).
    TransferId
);
typed_id!(
    /// Owner classification rule (§10.4).
    ///
    /// Do not confuse this with [`crate::rules::lot_disposal::RuleId`], which
    /// names the lot-disposal rule version (`fifo/214.1/v1`) used by the whole
    /// program; this names one owner's decision about one operation, which the
    /// owner creates, edits, and retires.
    ClassificationRuleId
);
typed_id!(
    /// Owner category group.
    CategoryGroupId
);
typed_id!(
    /// Owner category.
    CategoryId
);
typed_id!(
    /// Owner category assignment rule.
    CategoryRuleId
);

/// Namespace for declared sources. A fixed UUID, so the derivation is stable
/// across builds and machines.
const DECLARED_SOURCE_NAMESPACE: uuid::Uuid = uuid::uuid!("6f2b1c4e-6f8a-5a1d-9d0e-2c7f4a3b8e11");

impl SourceId {
    /// A source identity the caller declares rather than one we mint.
    ///
    /// Minting a random source per request means nothing deduplicates across
    /// requests: re-sending a corrected batch creates a second set of rows
    /// instead of replacing the first. The identity is therefore derived from
    /// the triple that actually names the source — the owner, the account, and
    /// the channel the rows arrived through.
    ///
    /// The channel is part of the key on purpose. A file export and a page
    /// paste of the same account are two channels; collapsing them into one
    /// source would make a pasted row deduplicate against an exported one
    /// instead of confirming it, and the two could never be told apart.
    #[must_use]
    pub fn declared(owner: OwnerId, account: AccountId, channel: &str) -> Self {
        let name = format!("{}/{}/{}", owner.inner(), account.inner(), channel);
        Self(uuid::Uuid::new_v5(
            &DECLARED_SOURCE_NAMESPACE,
            name.as_bytes(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keep_the_uuid_they_wrap() {
        let raw = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        assert_eq!(OwnerId(raw).inner(), raw);
        assert_eq!(AccountId(raw).inner(), raw);
        assert_eq!(CustodyId(raw).inner(), raw);
        assert_eq!(InstrumentId(raw).inner(), raw);
        assert_eq!(SourceId(raw).inner(), raw);
        assert_eq!(EventId(raw).inner(), raw);
        assert_eq!(TransferId(raw).inner(), raw);
        assert_eq!(ClassificationRuleId(raw).inner(), raw);
    }

    #[test]
    fn ids_of_different_kinds_are_distinct_types() {
        // Type incompatibility is checked by execution: the line below would
        // produce E0308, “expected `AccountId`, found `OwnerId`”. There is no
        // permanent check for this—it would require trybuild, which is not in
        // this plan; the commented-out line is not a substitute.
        // let _: AccountId = OwnerId::new_random();
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        assert_ne!(a, b, "two random identifiers are equal");
    }

    #[test]
    fn a_declared_source_is_stable_across_calls() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        assert_eq!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(owner, account, "file")
        );
    }

    #[test]
    fn a_declared_source_separates_channels_of_one_account() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        // Two channels of the same account must stay distinct source identities,
        // or a pasted row would deduplicate against an exported one instead of
        // confirming it (spec §6).
        assert_ne!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(owner, account, "paste")
        );
    }

    #[test]
    fn a_declared_source_separates_accounts_and_owners() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let other_owner = OwnerId(uuid::Uuid::from_u128(9));
        let account = AccountId(uuid::Uuid::from_u128(2));
        let other_account = AccountId(uuid::Uuid::from_u128(3));
        assert_ne!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(owner, other_account, "file")
        );
        assert_ne!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(other_owner, account, "file")
        );
    }

    #[test]
    fn a_declared_source_is_never_a_random_one() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        // Version 5, not version 4: a declared source and a random one occupy
        // disjoint spaces, so they cannot be confused by accident.
        assert_eq!(
            SourceId::declared(owner, account, "file")
                .inner()
                .get_version_num(),
            5
        );
        assert_eq!(SourceId::new_random().inner().get_version_num(), 4);
    }
}
