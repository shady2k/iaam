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
/// `NeedsClassification`, `Unsupported`. `Duplicate`, `PossibleDuplicate`,
/// `Rejected`, and `Quarantined` are operational: the former handles retries
/// (§10.6), the possible duplicate preserves an uncertain match for the owner,
/// the rejected row could not be parsed (§10.1), and the quarantined row was
/// parsed but could not be recorded.
///
/// **Whether the fact was recorded matters more than which code it was.**
/// `Discrepancy` is recorded deliberately: the fact was received, and hiding
/// it until it is explained would lose data. `NeedsReconciliation` is not
/// recorded, just as deliberately: there is nothing to record, and the
/// question has been put to the owner. `is_recorded` below draws that line,
/// and every code's published meaning states which side of it the code falls
/// on — but the reason is here, because it belongs to the whole vocabulary
/// rather than to any one of its ten entries.
///
/// **A verdict answers a write; confirmation answers a read.** That is why
/// `Accepted` is in this list and is produced by nothing, and why it is not an
/// omission waiting to be filled in. A verdict is computed once, in the
/// response to the request that wrote the row; it is never stored and there is
/// no call that restates it. Whether reconciliation matched is the opposite
/// kind of thing: a property of an account, a dimension and an interval,
/// folded when a report is read, and raised or lowered by evidence that
/// arrives afterwards. Nothing on the write path can say «reconciliation
/// matched» about a row, and a code that said it would already be stale by the
/// time the response was parsed. The system does have the word, in the
/// vocabulary built to carry it: `iaam_core::reconciliation::DimensionStatus`,
/// published in the data quality block as `accepted_internal` and
/// `accepted_independent`, which is where §10.3's «the status will rise by
/// itself» actually happens. Decision 0009 is this argument at length,
/// including why the code is described rather than removed.
///
/// `Discrepancy` and `NeedsReconciliation` are unproduced too, and for a
/// different reason that this comment must not blur into the one above:
/// theirs is a gap rather than an impossibility. A commit that overrides a
/// control mismatch writes rows the source's own figures contradict, and that
/// is a discrepancy something could report; a missing owner remainder is asked
/// for by the action queue instead. Both are recorded as open work, not
/// settled here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Recorded, and reconciliation matched — which nothing constructs.
    ///
    /// **Reserved.** Kept rather than deleted because §10.4 names it and
    /// because dropping a value from a published enum is a breaking change that
    /// buys nothing — the branch is unreachable, so no client can ever have
    /// taken it, and no client's behaviour changes when it goes. What a reader
    /// needs instead is to be told, which the published meaning below does. The
    /// reasoning is on the type above.
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
    /// The row was read and no fact was recorded from it.
    Quarantined { reason: String },
}

/// The verdict vocabulary: every variant, its wire code, and what the code means.
///
/// This is the single source for both. `Verdict::code` below is expanded from
/// it, and so is the enumerated, described `verdict` schema the API publishes:
/// pass the name of a macro that accepts
/// `Variant => "code": "meaning",` arms and it will be called with the whole
/// list. A code therefore cannot exist without a meaning, and a client reading
/// the contract sees the same ten entries this type declares.
///
/// It does **not** follow that all ten are emitted, and the table used to claim
/// it did. A code no path produces is a promise a client waits on for ever, so
/// where that is the case the meaning says so in the sentence the client
/// actually reads — the schema is the only document it has, and a caveat kept
/// anywhere else is a caveat it never sees.
///
/// A hand-written copy of this table drifts — the one in the agent skill
/// listed eight of the ten and omitted `possible_duplicate` and `quarantined`,
/// both of which are emitted in production.
#[macro_export]
macro_rules! verdict_vocabulary {
    ($receiver:path) => {
        $receiver! {
            Accepted => "accepted":
                "Reserved, and no path emits it. A verdict answers one write, while whether reconciliation matched is a property of an account, a dimension and an interval that is folded when a report is read and moves as later evidence arrives. Do not wait for this code, and do not read its absence as a failure to confirm: confirmation is reported by the data quality block, as `accepted_internal` or `accepted_independent`.",
            Provisional => "provisional":
                "The fact was recorded; no independent confirmation is available yet.",
            PossibleDuplicate => "possible_duplicate":
                "The fact was recorded and resembles one already in the journal. Neither is deleted and neither is merged: the owner is shown both and decides.",
            Discrepancy => "discrepancy":
                "The fact was recorded, but reconciliation of the dimension does not match, and the owner is investigating.",
            NeedsReconciliation => "needs_reconciliation":
                "Nothing was recorded: there is no owner remainder for the dimension to reconcile against.",
            Duplicate => "duplicate":
                "Nothing new was recorded: the idempotency key already recorded this fact, and the existing event is returned.",
            NeedsClassification => "needs_classification":
                "Nothing was recorded: the classification is ambiguous and an answer from the owner is required.",
            Unsupported => "unsupported":
                "Nothing was recorded: the operation lies outside the perimeter, so its economic interpretation is not reconstructed.",
            Rejected => "rejected":
                "Nothing was recorded: the row could not be parsed.",
            Quarantined => "quarantined":
                "Nothing was recorded: the row was read, but no fact could be written from it.",
        }
    };
}

macro_rules! define_verdict_code {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        impl Verdict {
            /// Machine-readable code for the API (§13).
            #[must_use]
            pub const fn code(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $code,)+
                }
            }
        }
    };
}

verdict_vocabulary!(define_verdict_code);

impl Verdict {
    /// Whether the row was recorded in the journal.
    ///
    /// The line the type-level comment above draws: a discrepancy is a
    /// recorded fact with an open question, and a reconciliation requirement is
    /// a question with no fact behind it.
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
            | Self::Rejected { .. }
            | Self::Quarantined { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_verdict() -> [Verdict; 10] {
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
            Verdict::Quarantined {
                reason: "row could not be recorded".to_owned(),
            },
        ]
    }

    #[test]
    fn every_verdict_has_a_distinct_code_and_all_six_spec_verdicts_exist() {
        // §10.4 names six verdicts. Duplicate, Rejected, and Quarantined are
        // service variants: they handle a duplicate, an unparsed row, or a
        // parsed row that could not be recorded. Losing any verdict becomes
        // silence where the owner expects a response.
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
