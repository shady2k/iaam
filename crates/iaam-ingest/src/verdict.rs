//! Ingestion verdicts (§10.4).

use iaam_core::ids::{AccountId, EventId};
use iaam_core::reconciliation::Dimension;
use serde::{Deserialize, Serialize};

/// Why the row was rejected. The field, expected value, and received value are required by
/// §13 for `422` responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// Verdict for one row.
///
/// There is no separate confirmation step in the normal flow: there is submission
/// and a verdict (§10.4). The six verdicts in the spec are `Accepted`,
/// `Provisional`, `Discrepancy`, `NeedsReconciliation`,
/// `NeedsClassification`, `Unsupported`. `Duplicate`, `PossibleDuplicate`, and
/// `Rejected` are operational: the former handles retries (§10.6), the
/// possible duplicate preserves an uncertain match for the owner, and the
/// latter a row that could not be parsed (§10.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Recorded; reconciliation matched.
    Accepted { event: EventId },
    /// Recorded; independent confirmation is not yet available.
    Provisional { event: EventId },
    /// Recorded, and it resembles a fact already in the journal (§10.6, level five).
    /// Never deleted and never merged: the owner is shown both, and decides.
    PossibleDuplicate {
        event: EventId,
        of: EventId,
        level: crate::dedup::DedupLevel,
    },
    /// Recorded, but reconciliation does not match: the owner is investigating.
    Discrepancy {
        event: EventId,
        account: AccountId,
        dimension: Dimension,
        detail: String,
    },
    /// Nothing to reconcile against: a remainder from the owner is required.
    NeedsReconciliation {
        account: AccountId,
        dimension: Dimension,
    },
    /// Already recorded previously under the idempotency key (§10.6).
    Duplicate { existing: EventId },
    /// Classification is ambiguous: an answer from the owner is required.
    NeedsClassification { question: String },
    /// Operation outside the scope (§11): the monetary effect is preserved,
    /// but the economic interpretation is not reconstructed.
    Unsupported { reason: String },
    /// The row could not be parsed.
    Rejected { rejection: Rejection },
}

impl Verdict {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Provisional { .. } => "provisional",
            Self::PossibleDuplicate { .. } => "possible_duplicate",
            Self::Discrepancy { .. } => "discrepancy",
            Self::NeedsReconciliation { .. } => "needs_reconciliation",
            Self::Duplicate { .. } => "duplicate",
            Self::NeedsClassification { .. } => "needs_classification",
            Self::Unsupported { .. } => "unsupported",
            Self::Rejected { .. } => "rejected",
        }
    }

    /// Whether the row was recorded in the journal.
    ///
    /// Discrepancy recorded: the fact was received and must not be hidden until clarified.
    /// would mean losing data. The reconciliation requirement does not: there is nothing
    /// to record there; the question has been put to the owner.
    #[must_use]
    pub const fn is_recorded(&self) -> bool {
        match self {
            Self::Accepted { .. }
            | Self::Provisional { .. }
            | Self::Discrepancy { .. }
            | Self::PossibleDuplicate { .. }
            | Self::Duplicate { .. } => true,
            Self::NeedsReconciliation { .. }
            | Self::NeedsClassification { .. }
            | Self::Unsupported { .. }
            | Self::Rejected { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_verdict() -> [Verdict; 9] {
        let event = EventId::new_random();
        let account = AccountId::new_random();
        [
            Verdict::Accepted { event },
            Verdict::Provisional { event },
            Verdict::PossibleDuplicate {
                event,
                of: EventId::new_random(),
                level: crate::dedup::DedupLevel::Probabilistic,
            },
            Verdict::Discrepancy {
                event,
                account,
                dimension: Dimension::Cash,
                detail: "balance at the end of March".to_owned(),
            },
            Verdict::NeedsReconciliation {
                account,
                dimension: Dimension::Cash,
            },
            Verdict::Duplicate { existing: event },
            Verdict::NeedsClassification {
                question: "an internal transfer?".to_owned(),
            },
            Verdict::Unsupported {
                reason: "РЕПО".to_owned(),
            },
            Verdict::Rejected {
                rejection: Rejection {
                    field: "date".to_owned(),
                    expected: "DD.MM.YYYY".to_owned(),
                    actual: "yesterday".to_owned(),
                },
            },
        ]
    }

    #[test]
    fn every_verdict_has_a_distinct_code_and_all_six_spec_verdicts_exist() {
        // §10.4 names six verdicts. Duplicate and Rejected are service variants:
        // they handle a duplicate and an unparsed row, rather than the
        // acceptance result. Both are checked—the loss of a verdict
        // becomes silence where the owner expects a response.
        let all = every_verdict();
        let mut codes: Vec<&str> = all.iter().map(Verdict::code).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "verdict codes match");

        for verdict in [
            "accepted",
            "provisional",
            "discrepancy",
            "needs_reconciliation",
            "needs_classification",
            "unsupported",
        ] {
            assert!(codes.contains(&verdict), "verdict {verdict} lost");
        }
    }

    #[test]
    fn a_discrepancy_is_recorded_and_a_reconciliation_request_is_not() {
        // A discrepancy is a recorded fact with an open question. The reconciliation
        // requirement is a question without a fact. Merging them means either losing
        // data or recording in the log something that did not happen.
        let event = EventId::new_random();
        let account = AccountId::new_random();
        assert!(
            Verdict::Discrepancy {
                event,
                account,
                dimension: Dimension::Positions,
                detail: String::new(),
            }
            .is_recorded()
        );
        assert!(
            !Verdict::NeedsReconciliation {
                account,
                dimension: Dimension::Positions,
            }
            .is_recorded()
        );
    }

    #[test]
    fn a_verdict_survives_a_serde_round_trip() {
        // The verdict is exposed through REST: an option that does not survive
        // serialization will be caught by the external agent, not here.
        for verdict in every_verdict() {
            let json = serde_json::to_string(&verdict).expect("serialization");
            let back: Verdict = serde_json::from_str(&json).expect("parsing");
            assert_eq!(back, verdict);
        }
    }
}
