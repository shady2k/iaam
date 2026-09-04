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
/// **Whether the fact was recorded matters more than which code it was.** A
/// fact received but not yet explained is recorded anyway, because hiding it
/// until it is explained would lose data; a code that reports a question with
/// no fact behind it records nothing, because there is nothing to record.
/// `is_recorded` below draws that line, and every code's published meaning
/// states which side of it the code falls on — but the reason is here, because
/// it belongs to the whole vocabulary rather than to any one of its eleven
/// entries.
///
/// The line is drawn over all eleven, including the three no path emits. That is
/// not idle: it is what makes the last paragraph below able to say that
/// `NeedsReconciliation` is on the side it belongs on and false about every
/// situation it would describe.
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
/// `Discrepancy` and `NeedsReconciliation` are unproduced too, and decision
/// 0011 reserves them as well — for two reasons this comment keeps apart,
/// because collapsing them into one loses the sharper of the two.
///
/// `Discrepancy` fails for the reason above, and its payload is the proof.
/// `{ account, dimension }` is the read-time claim itself:
/// `iaam_core::returns::MaterialIssue::Discrepancy` carries those same two
/// fields and nothing else, raised where the ledger's fold comes out
/// `Discrepant`. What the verdict lacks is the interval — without one,
/// «reconciliation does not match» names no period to not match over. A commit
/// that overrides a control mismatch does know something real, but not this: it
/// knows that a batch disagrees with the control section one document printed,
/// per account, currency and figure. That fact is already carried three times
/// over — by the import session's assessment, which names every disagreeing
/// figure with both numbers and the difference; by the control assertions the
/// commit writes into the journal beside the rows they contradict, which the
/// ledger folds into `discrepant` for as long as they stand; and by the
/// `discrepancy_unresolved` queue item, which carries the operations that
/// settle it. None of the three is a property of one row, and `verdicts` is a
/// list a caller reads by row position.
///
/// `NeedsReconciliation` fails harder: emitting it would be false. Its
/// published sentence is «nothing was recorded», and no write is ever declined
/// for want of an owner remainder. The rows are recorded — as `Provisional` —
/// and the need for a remainder is then derived *from* them, per account and
/// interval, by the queue's `provide_control_assertion`, which asks the opening
/// point before the closing one. The need is discovered after the write this
/// code would have to be the answer to, so there is no moment at which it could
/// be said truthfully. `is_recorded` puts this code on the «nothing was
/// recorded» side, and that placement is right about the sentence and wrong
/// about every situation the sentence would describe.
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
    /// Recorded, but reconciliation does not match — which nothing constructs.
    ///
    /// **Reserved**, for `Accepted`'s reason. `{ account, dimension }` is the
    /// read-time claim itself, and `MaterialIssue::Discrepancy` carries exactly
    /// those two fields out of the fold that can decide them. Kept rather than
    /// deleted on the same grounds as `Accepted`; the reasoning is on the type
    /// above, including what a commit that overrides a control mismatch knows
    /// instead and the three places that already report it.
    Discrepancy {
        event: EventId,
        account: AccountId,
        dimension: Dimension,
        detail: String,
    },
    /// Nothing to reconcile against — which nothing constructs, and which
    /// nothing could construct truthfully.
    ///
    /// **Reserved**, and for a stronger reason than `Accepted` or
    /// `Discrepancy`: a missing owner remainder declines no write, so «nothing
    /// was recorded» is false of every row this code would describe. The
    /// request for the figure is the action queue's `provide_control_assertion`,
    /// which is derived from the recorded facts and so cannot precede them. Kept
    /// rather than deleted on `Accepted`'s grounds; the reasoning is on the type
    /// above.
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
    /// The row was read, understood, and correctly produced no fact.
    ///
    /// The eleventh code, and the one the vocabulary was missing rather than
    /// reserving (`iaam-tb5o`). Every other code that records nothing says
    /// something went wrong or is pending: `rejected` could not be parsed,
    /// `quarantined` could not be written, `needs_classification` is waiting on
    /// the owner. None of those is true of a row whose honest financial record
    /// **is** nothing — a movement between two payment instruments over one
    /// account, where the balance does not change and there is no second leg to
    /// wait for.
    ///
    /// It is not `quarantined` wearing a friendlier reason, and the difference
    /// is not a nicety: `quarantined` is what `ImportCoverageGap` is computed
    /// from, so filing this row there would record a fact saying the import
    /// could not confirm the dimensions this row moves — when the row moves
    /// none and the import is complete without it.
    ///
    /// `reason` is a code from a closed list, not free text, for the reason
    /// `iaam_core::event::source_row` gives about a refused row's identity: a
    /// determination that a row needed no fact has to be auditable, and an
    /// importer that merely came up empty must not be able to present itself
    /// as one of these.
    NoFact { reason: String },
}

/// The verdict vocabulary: every variant, its wire code, and what the code means.
///
/// This is the single source for both. `Verdict::code` below is expanded from
/// it, and so is the enumerated, described `verdict` schema the API publishes:
/// pass the name of a macro that accepts
/// `Variant => "code": "meaning",` arms and it will be called with the whole
/// list. A code therefore cannot exist without a meaning, and a client reading
/// the contract sees the same eleven entries this type declares.
///
/// It does **not** follow that all eleven are emitted, and the table used to claim
/// it did. A code no path produces is a promise a client waits on for ever, so
/// where that is the case the meaning says so in the sentence the client
/// actually reads — the schema is the only document it has, and a caveat kept
/// anywhere else is a caveat it never sees.
///
/// A hand-written copy of this table drifts — the one in the agent skill
/// listed eight of the then ten and omitted `possible_duplicate` and `quarantined`,
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
                "Reserved, and no path emits it. A verdict answers one write, while whether reconciliation of a dimension matches is a property of an account, a dimension and an interval that is folded when a report is read. Do not wait for this code. A batch that disagrees with the control section its own source printed is reported figure by figure, with both numbers and the difference, by the import session's assessment; a disagreement the journal holds is reported by the data quality block as `discrepant` and by the action queue as `discrepancy_unresolved`, which carries the operations that settle it.",
            NeedsReconciliation => "needs_reconciliation":
                "Reserved, and no path emits it. Nothing is ever declined for want of an owner remainder: the rows are recorded, and the need for a remainder is derived from them afterwards, per account and interval. Do not wait for this code — the request for the figure is published by the action queue as `provide_control_assertion`, naming the account, the interval and which end of it the balance is wanted at.",
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
            NoFact => "no_fact":
                "Nothing was recorded, and nothing should have been: the row was read and understood to require no journal fact. `detail` carries the code of the determination that established it — today `one_account_two_instruments`, a movement between two payment instruments over one account, which changes no balance and has no second leg. This is a settled row, not a failure and not something to retry.",
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
            | Self::Quarantined { .. }
            | Self::NoFact { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_verdict() -> [Verdict; 11] {
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
            Verdict::NoFact {
                reason: "one_account_two_instruments".to_owned(),
            },
        ]
    }

    #[test]
    fn a_settled_row_that_produced_nothing_is_not_a_quarantined_one() {
        // Both record nothing, and only one of them is a failure. The codes
        // must differ, because `quarantined` is what a coverage gap is computed
        // from and this row leaves no gap.
        let settled = Verdict::NoFact {
            reason: "one_account_two_instruments".to_owned(),
        };
        let failed = Verdict::Quarantined {
            reason: "row could not be recorded".to_owned(),
        };
        assert_ne!(settled.code(), failed.code());
        assert!(!settled.is_recorded());
    }

    #[test]
    fn every_verdict_has_a_distinct_code_and_all_six_spec_verdicts_exist() {
        // §10.4 names six verdicts. Duplicate, Rejected, Quarantined and
        // NoFact are service variants: they handle a duplicate, an unparsed
        // row, a parsed row that could not be recorded, and a row that
        // correctly produced none. Losing any verdict becomes silence where
        // the owner expects a response.
        let all = every_verdict();
        let mut codes: Vec<&str> = all.iter().map(Verdict::code).collect();
        assert_eq!(codes.len(), 11, "every verdict is listed here");
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
