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
}
