//! Identity of a row as the source presented it (§10.3).
//!
//! Deliberately distinct from [`crate::event::provenance::Provenance::source_operation_id`],
//! which identifies an EVENT. One source row can expand into several events —
//! a trade order becomes one event per fill — so an event identifier cannot
//! answer "is this row represented".

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::SourceId;
use crate::reconciliation::Dimension;

/// How a refused row is named.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RowName {
    /// The identifier the source gave the row.
    Given(String),
    /// The source gave none: a hexadecimal SHA-256 of the row's raw payload.
    Fingerprint(String),
}

/// Identity of a source row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceRowKey {
    pub source: SourceId,
    pub row: RowName,
}

/// A row an import refused, and what it alone cannot confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedRow {
    pub key: SourceRowKey,
    pub dimensions: BTreeSet<Dimension>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_given_name_and_a_fingerprint_of_the_same_text_are_different_rows() {
        let source = SourceId::new_random();
        let given = SourceRowKey {
            source,
            row: RowName::Given("OP-1".to_owned()),
        };
        let fingerprint = SourceRowKey {
            source,
            row: RowName::Fingerprint("OP-1".to_owned()),
        };
        assert_ne!(given, fingerprint);
    }

    #[test]
    fn a_given_and_fingerprint_row_key_round_trip_through_serde() {
        for row in [
            RowName::Given("OP-1".to_owned()),
            RowName::Fingerprint("OP-1".to_owned()),
        ] {
            let key = SourceRowKey {
                source: SourceId::new_random(),
                row,
            };
            let json = serde_json::to_string(&key).unwrap();
            let back: SourceRowKey = serde_json::from_str(&json).unwrap();
            assert_eq!(key, back);
        }
    }
}
