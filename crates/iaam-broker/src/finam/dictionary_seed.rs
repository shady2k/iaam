//! Initial dictionary of Finam operation kinds (§14).
//!
//! Reproduces the former `match` in `parse.rs` (commit 5320fb0).
//! The codes are upper-case here because channel parsing converts them to
//! upper case: that conversion belongs to this channel, not the dictionary.
//!
//! The application does not currently connect this channel, so it does not
//! need the dictionary today. The table exists anyway: knowing that `INTEREST`
//! means a coupon for Finam and `TRADE_BUY` means a purchase would otherwise
//! live only in git history, and a channel connected later would have to
//! reconstruct it.

/// “Channel code → kind name” pairs for initial seeding.
pub const FINAM_OPERATION_KINDS: &[(&str, &str)] = &[
    ("BUY", "buy"),
    ("PURCHASE", "buy"),
    ("TRADE_BUY", "buy"),
    ("SELL", "sell"),
    ("TRADE_SELL", "sell"),
    ("DEPOSIT", "deposit"),
    ("CASH_DEPOSIT", "deposit"),
    ("INPUT", "deposit"),
    ("WITHDRAWAL", "withdrawal"),
    ("CASH_WITHDRAWAL", "withdrawal"),
    ("OUTPUT", "withdrawal"),
    ("DIVIDEND", "dividend"),
    ("COUPON", "coupon"),
    ("INTEREST", "coupon"),
    ("COMMISSION", "commission"),
    ("FEE", "commission"),
];

/// Name for the source of these rows in the provenance record.
pub const FINAM_SEED_NAME: &str = "embedded Finam dictionary";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_kind::OperationKindDictionary;

    #[test]
    fn every_seeded_kind_is_readable_by_this_build() {
        let (_, unreadable) = OperationKindDictionary::build(FINAM_OPERATION_KINDS.iter().copied());
        assert!(unreadable.is_empty(), "{unreadable:?}");
    }

    #[test]
    fn the_codes_are_upper_case_as_the_channel_leaves_them() {
        for (code, _) in FINAM_OPERATION_KINDS {
            assert_eq!(
                *code,
                code.to_ascii_uppercase(),
                "channel parsing returns an upper-case code; the dictionary must agree"
            );
        }
    }
}
