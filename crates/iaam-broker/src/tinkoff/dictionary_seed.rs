//! Initial dictionary of T-Invest operation kinds (§14).
//!
//! This is **our** knowledge, not the broker's: the contract lists codes but
//! does not say that `OPERATION_TYPE_DIV_EXT` and
//! `OPERATION_TYPE_DIVIDEND` are both dividends for us, or that
//! `OPERATION_TYPE_OVER_COM` is a fee. Therefore the table lives in code and
//! is inserted into the database once, when access is configured.
//!
//! It is **not** the source of truth afterwards: the dictionary is edited in
//! the database, and seeding from here does not touch existing rows. Otherwise
//! the owner's decision would be cancelled on every access setup.
//!
//! The contents reproduce the former `match` in `parse.rs` (commit 5320fb0)
//! word for word. A missing synonym here is a code that would silently become
//! unknown after migration, and imports would stop parsing what they parsed
//! yesterday.
//!
//! Amortisation and redemption are intentionally absent: the contract
//! declares their codes (`OPERATION_TYPE_BOND_REPAYMENT`,
//! `OPERATION_TYPE_BOND_REPAYMENT_FULL`), but the channel reports neither the
//! returned principal per unit nor its storage location, so no fact can be
//! built from them. The owner can add them by decision once the necessary data
//! exists.

/// “Channel code → kind name” pairs for initial seeding.
pub const TINKOFF_OPERATION_KINDS: &[(&str, &str)] = &[
    ("OPERATION_TYPE_BUY", "buy"),
    ("OPERATION_TYPE_BUY_CARD", "buy"),
    ("OPERATION_TYPE_BUY_MARGIN", "buy"),
    ("OPERATION_TYPE_DELIVERY_BUY", "buy"),
    ("OPERATION_TYPE_SELL", "sell"),
    ("OPERATION_TYPE_SELL_CARD", "sell"),
    ("OPERATION_TYPE_SELL_MARGIN", "sell"),
    ("OPERATION_TYPE_DELIVERY_SELL", "sell"),
    ("OPERATION_TYPE_DIVIDEND", "dividend"),
    ("OPERATION_TYPE_DIV_EXT", "dividend"),
    ("OPERATION_TYPE_COUPON", "coupon"),
    ("OPERATION_TYPE_BROKER_FEE", "commission"),
    ("OPERATION_TYPE_SERVICE_FEE", "commission"),
    ("OPERATION_TYPE_MARGIN_FEE", "commission"),
    ("OPERATION_TYPE_SUCCESS_FEE", "commission"),
    ("OPERATION_TYPE_TRACK_MFEE", "commission"),
    ("OPERATION_TYPE_TRACK_PFEE", "commission"),
    ("OPERATION_TYPE_CASH_FEE", "commission"),
    ("OPERATION_TYPE_OUT_FEE", "commission"),
    ("OPERATION_TYPE_OUT_STAMP_DUTY", "commission"),
    ("OPERATION_TYPE_OUTPUT_PENALTY", "commission"),
    ("OPERATION_TYPE_ADVICE_FEE", "commission"),
    ("OPERATION_TYPE_OVER_COM", "commission"),
    ("OPERATION_TYPE_INPUT", "deposit"),
    ("OPERATION_TYPE_INPUT_SECURITIES", "deposit"),
    ("OPERATION_TYPE_INPUT_SWIFT", "deposit"),
    ("OPERATION_TYPE_INPUT_ACQUIRING", "deposit"),
    ("OPERATION_TYPE_INP_MULTI", "deposit"),
    ("OPERATION_TYPE_OUTPUT", "withdrawal"),
    ("OPERATION_TYPE_OUTPUT_SECURITIES", "withdrawal"),
    ("OPERATION_TYPE_OUTPUT_SWIFT", "withdrawal"),
    ("OPERATION_TYPE_OUTPUT_ACQUIRING", "withdrawal"),
    ("OPERATION_TYPE_OUT_MULTI", "withdrawal"),
    ("OPERATION_TYPE_TRANS_IIS_BS", "transfer"),
    ("OPERATION_TYPE_TRANS_BS_BS", "transfer"),
];

/// Name for the source of these rows in the provenance record.
pub const TINKOFF_SEED_NAME: &str = "embedded T-Invest dictionary";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_kind::{ChannelOperationKind, OperationKindDictionary};

    /// Every kind name must be known to this build: a string that `parse`
    /// cannot read would enter the database and become an import refusal,
    /// rather than a seed.
    #[test]
    fn every_seeded_kind_is_readable_by_this_build() {
        let (_, unreadable) =
            OperationKindDictionary::build(TINKOFF_OPERATION_KINDS.iter().copied());
        assert!(unreadable.is_empty(), "{unreadable:?}");
    }

    /// A code cannot mean two different kinds. A conflicting duplicate would
    /// silently win according to insertion order.
    #[test]
    fn no_code_is_listed_twice() {
        let mut seen = std::collections::BTreeMap::new();
        for (code, kind) in TINKOFF_OPERATION_KINDS {
            if let Some(previous) = seen.insert(*code, *kind) {
                assert_eq!(previous, *kind, "code {code} has two different names");
                panic!("code {code} is listed twice");
            }
        }
    }

    /// Guard against silently losing a synonym: the dictionary must parse
    /// exactly what the former `match` parsed. These are the synonyms for
    /// which loss is not immediately obvious.
    #[test]
    fn the_synonyms_that_the_old_match_knew_are_all_here() {
        let (dictionary, _) =
            OperationKindDictionary::build(TINKOFF_OPERATION_KINDS.iter().copied());
        for (code, expected) in [
            ("OPERATION_TYPE_DIV_EXT", ChannelOperationKind::Dividend),
            ("OPERATION_TYPE_DELIVERY_BUY", ChannelOperationKind::Buy),
            ("OPERATION_TYPE_DELIVERY_SELL", ChannelOperationKind::Sell),
            ("OPERATION_TYPE_OVER_COM", ChannelOperationKind::Commission),
            ("OPERATION_TYPE_INP_MULTI", ChannelOperationKind::Deposit),
            ("OPERATION_TYPE_OUT_MULTI", ChannelOperationKind::Withdrawal),
            (
                "OPERATION_TYPE_TRANS_IIS_BS",
                ChannelOperationKind::Transfer,
            ),
        ] {
            assert_eq!(dictionary.kind_of(code), expected, "{code}");
        }
    }
}
