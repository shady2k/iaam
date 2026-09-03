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
    /// One act of importing: the rows one declared submission brought in.
    ///
    /// Distinct from [`SourceId`] on purpose. The source answers «where do
    /// these rows come from», and deduplication is scoped by it: a source
    /// operation identifier is unique within a source, so comparing two of
    /// them across sources would suppress a legitimate fact (§10.6). The
    /// import answers «which submission carried this row», which is finer —
    /// two months of one account exported the same way are one source and two
    /// imports. Folding the second question into the first would make every
    /// import its own source, and the same bank's operation identifiers would
    /// stop being compared across two of its own exports.
    ImportId
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
    /// One import session: observations accumulated before anything is committed.
    ///
    /// Pre-journal state, and named separately from [`ImportId`] for that
    /// reason. The import names rows that are already in the journal and is what
    /// a retraction takes; the session names rows that are not in it yet and may
    /// never be. Folding the two together would give a retraction a handle on
    /// something that was never recorded.
    ImportSessionId
);
typed_id!(
    /// One question put to the owner about one observed row.
    ///
    /// A question needs an identity of its own because it outlives the response
    /// that carried it: the answer arrives in a later request, possibly days
    /// later, and must name the question it answers rather than restate it.
    ImportQuestionId
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

/// Namespace for declared imports.
///
/// A namespace of its own rather than a longer name in the source namespace:
/// two derivations sharing a namespace can collide whenever one derived name
/// happens to spell another, and an import identity is the key of a
/// destructive operation.
const DECLARED_IMPORT_NAMESPACE: uuid::Uuid = uuid::uuid!("2a7d5b90-4c31-5f28-8b6a-1e9c0d47f532");

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

impl ImportId {
    /// The identity of one declared import.
    ///
    /// Derived rather than minted, for the same reason [`SourceId::declared`]
    /// is: the caller holds no server-assigned handle after a submission — the
    /// event identifiers were minted here and the row identifiers belong to the
    /// file — so the only key it can name an import by afterwards is the one it
    /// declared.
    ///
    /// The label is what separates two imports the rest of the declaration
    /// cannot: two months of one account, exported the same way, differ in
    /// nothing else. It is caller-supplied text, so two imports the caller
    /// labels alike are one import — which is the deduplicating half of the
    /// same property, and is why re-sending a batch under its own label
    /// replaces nothing and adds nothing.
    ///
    /// The channel's length is part of the derived name so that the pair
    /// (channel, label) can be read back out of it. Without it, a channel
    /// `file/x` with label `y` and a channel `file` with label `x/y` would
    /// spell one name and share one identity.
    #[must_use]
    pub fn declared(owner: OwnerId, account: AccountId, channel: &str, label: &str) -> Self {
        let name = format!(
            "{}/{}/{}/{}/{}",
            owner.inner(),
            account.inner(),
            channel.len(),
            channel,
            label
        );
        Self(uuid::Uuid::new_v5(
            &DECLARED_IMPORT_NAMESPACE,
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
    fn a_declared_import_separates_two_labels_of_one_channel() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        // The reported defect: two months of one account exported the same way
        // differ in nothing but the label, and retracting one must not retract
        // the other.
        assert_ne!(
            ImportId::declared(owner, account, "file", "january"),
            ImportId::declared(owner, account, "file", "february")
        );
        assert_eq!(
            ImportId::declared(owner, account, "file", "january"),
            ImportId::declared(owner, account, "file", "january"),
            "re-declaring one import must name that import, not a second one"
        );
    }

    #[test]
    fn a_declared_import_separates_channels_accounts_and_owners() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let other_owner = OwnerId(uuid::Uuid::from_u128(9));
        let account = AccountId(uuid::Uuid::from_u128(2));
        let other_account = AccountId(uuid::Uuid::from_u128(3));
        let import = ImportId::declared(owner, account, "file", "january");
        assert_ne!(
            import,
            ImportId::declared(owner, account, "paste", "january")
        );
        assert_ne!(
            import,
            ImportId::declared(owner, other_account, "file", "january")
        );
        assert_ne!(
            import,
            ImportId::declared(other_owner, account, "file", "january")
        );
    }

    #[test]
    fn a_declared_import_cannot_be_spelled_by_another_split_of_its_parts() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        // Channel and label are both free text, so without the length in the
        // derived name these two would be one identity — and retracting one
        // would retract an import the caller never named.
        assert_ne!(
            ImportId::declared(owner, account, "file/x", "y"),
            ImportId::declared(owner, account, "file", "x/y")
        );
    }

    #[test]
    fn a_declared_import_is_not_the_source_it_arrived_through() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        // Two questions, two identities: deduplication is scoped by the source,
        // and it must not narrow to one import.
        assert_ne!(
            ImportId::declared(owner, account, "file", "january").inner(),
            SourceId::declared(owner, account, "file").inner()
        );
        assert_eq!(
            ImportId::declared(owner, account, "file", "january")
                .inner()
                .get_version_num(),
            5
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
