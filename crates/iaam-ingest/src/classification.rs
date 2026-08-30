//! Classification rules and history recalculation (§10.4).
//!
//! **Classification is not an event field.** An event carries a fact —
//! `CashTransfer` with both accounts — while “inside or outside the perimeter”
//! is determined by the perimeter classifier (§4.10). Rules are needed where the
//! **type** of operation itself cannot be inferred from the data: a transfer to oneself versus
//! a transfer to a third party, a fee versus a withdrawal, income versus
//! a refund.
//!
//! Therefore, it is not the constructed operation that is classified, but the row's attributes,
//! visible **before** choosing the type: the counterparty account, the payment purpose,
//! and the word the source used to name the operation.
//!
//! **History recalculation means new facts, not editing old ones.** Editing a
//! rule produces a plan of reversal and replacement; the journal remains
//! append-only (§4.8). The [`Correction`] type cannot express a change to an
//! event — this guarantees the form, not the caller's discipline.

use std::collections::BTreeMap;

use iaam_core::event::Event;
use iaam_core::event::correction::{CorrectionError, resolve};
use iaam_core::event::kind::{EventKind, FeeOrigin};
use iaam_core::ids::{AccountId, ClassificationRuleId, EventId};

/// Where the money is moving. Needed so that the question asked of the owner is relevant:
/// debits and credits have different branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Movement {
    In,
    Out,
}

/// Who is on the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Counterparty {
    /// The owner's account, identified by the directory.
    OwnAccount(AccountId),
    /// The account is named but not identified: a details line from the report.
    Named(String),
    /// No party is named at all.
    Unknown,
}

/// Row attributes used to determine the operation type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationSubject {
    pub account: AccountId,
    pub counterparty: Counterparty,
    pub description: Option<String>,
    /// What the source called the operation. An open set: each
    /// broker uses its own words, so this is a string, not an enum.
    pub source_kind: Option<String>,
    pub movement: Movement,
}

/// Rule condition. Specified fields are joined with “and”.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatcher {
    pub counterparty_account: Option<String>,
    pub description_contains: Option<String>,
    pub kind: Option<String>,
}

impl RuleMatcher {
    /// A condition that asks about nothing matches nothing.
    ///
    /// An “everything” rule can only be created by mistake, and silently
    /// reclassifying the entire portfolio with it is forbidden.
    #[must_use]
    pub const fn asks_nothing(&self) -> bool {
        self.counterparty_account.is_none()
            && self.description_contains.is_none()
            && self.kind.is_none()
    }

    /// Whether the condition matches the row.
    #[must_use]
    pub fn matches(&self, subject: &ClassificationSubject) -> bool {
        if self.asks_nothing() {
            return false;
        }
        let by_counterparty = self.counterparty_account.as_deref().is_none_or(
            |wanted| matches!(&subject.counterparty, Counterparty::Named(name) if name == wanted),
        );
        // Brokers write payment purposes however they please: a rule
        // sensitive to case would stop working on the next
        // report from the same broker.
        let by_description = self.description_contains.as_deref().is_none_or(|wanted| {
            subject
                .description
                .as_deref()
                .is_some_and(|text| text.to_lowercase().contains(&wanted.to_lowercase()))
        });
        let by_kind = self
            .kind
            .as_deref()
            .is_none_or(|wanted| subject.source_kind.as_deref() == Some(wanted));
        by_counterparty && by_description && by_kind
    }
}

/// What the operation turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    InternalTransfer { to: AccountId },
    ExternalFlow,
    Fee { origin: FeeOrigin },
    Income,
}

/// The owner's decision recorded by the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationRule {
    pub id: ClassificationRuleId,
    pub version: u32,
    pub matcher: RuleMatcher,
    pub outcome: Classification,
}

impl ClassificationRule {
    /// The rule's wording.
    ///
    /// The rule must be visible: without wording, there is nothing with which to explain
    /// the previous classification (§10.4).
    #[must_use]
    pub fn describe(&self) -> String {
        let mut conditions = Vec::new();
        if let Some(account) = &self.matcher.counterparty_account {
            conditions.push(format!("counterparty account — {account}"));
        }
        if let Some(text) = &self.matcher.description_contains {
            conditions.push(format!("payment purpose contains «{text}»"));
        }
        if let Some(kind) = &self.matcher.kind {
            conditions.push(format!("source called the operation «{kind}»"));
        }
        let conditions = if conditions.is_empty() {
            "no conditions, so the rule does not apply".to_owned()
        } else {
            conditions.join(" and ")
        };
        format!(
            "version {}: if {conditions}, then {}",
            self.version,
            describe_outcome(self.outcome)
        )
    }
}

fn describe_outcome(outcome: Classification) -> &'static str {
    match outcome {
        Classification::InternalTransfer { .. } => "this is a transfer between own accounts",
        Classification::ExternalFlow => "this is movement outside the portfolio",
        Classification::Fee { .. } => "this is a fee",
        Classification::Income => "this is income",
    }
}

/// Why the operation was classified this way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    /// Derived from data: no rule was needed.
    Derived,
    /// Owner's decision.
    Rule {
        rule: ClassificationRuleId,
        version: u32,
    },
}

/// Question for the owner.
///
/// An enumeration, not a string: the question is sent to the API and rendered
/// with human-readable account names, which the pure function does not have,
/// and a string containing a UUID is not a specific question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Recipient account named but not recognized.
    IsTransferInternal {
        account: AccountId,
        counterparty: String,
    },
    /// Debit without a named counterparty: fee or withdrawal?
    IsOutflowAFee { account: AccountId },
    /// Receipt without a named counterparty: income or refund?
    IsInflowIncome { account: AccountId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassificationResult {
    Resolved {
        classification: Classification,
        basis: Basis,
    },
    /// Cannot be inferred from data and is not covered by a rule. Guessing is forbidden.
    Ambiguous { question: Question },
}

/// Classification of a row.
///
/// Among several matching rules, the highest version wins: a change
/// creates a new version, and the highest is the owner's latest decision.
#[must_use]
pub fn classify(
    subject: &ClassificationSubject,
    rules: &[ClassificationRule],
) -> ClassificationResult {
    if let Counterparty::OwnAccount(to) = subject.counterparty {
        return ClassificationResult::Resolved {
            classification: Classification::InternalTransfer { to },
            basis: Basis::Derived,
        };
    }
    let chosen = rules
        .iter()
        .filter(|rule| rule.matcher.matches(subject))
        .max_by_key(|rule| rule.version);
    match chosen {
        Some(rule) => ClassificationResult::Resolved {
            classification: rule.outcome,
            basis: Basis::Rule {
                rule: rule.id,
                version: rule.version,
            },
        },
        None => ClassificationResult::Ambiguous {
            question: question_for(subject),
        },
    }
}

fn question_for(subject: &ClassificationSubject) -> Question {
    match (&subject.counterparty, subject.movement) {
        (Counterparty::Named(counterparty), _) => Question::IsTransferInternal {
            account: subject.account,
            counterparty: counterparty.clone(),
        },
        (Counterparty::Unknown, Movement::Out) => Question::IsOutflowAFee {
            account: subject.account,
        },
        (Counterparty::Unknown, Movement::In) => Question::IsInflowIncome {
            account: subject.account,
        },
        // An own account does not reach this point: it is handled in `classify`
        // as inferred from data.
        (Counterparty::OwnAccount(to), _) => Question::IsTransferInternal {
            account: subject.account,
            counterparty: to.inner().to_string(),
        },
    }
}

/// What classification the event already expresses.
///
/// `None` — the event carries no classification: a trade, valuation,
/// reconstructed beginning, and control assertion are facts,
/// not owner's decisions and are not subject to recalculation.
///
/// Exhaustive `match`: a new event kind must fail to compile
/// here rather than silently fall out of recalculation.
#[must_use]
pub const fn classification_of(event: &Event) -> Option<Classification> {
    match event.kind {
        EventKind::CashTransfer { to, .. } => Some(Classification::InternalTransfer { to }),
        EventKind::CashIn { .. } | EventKind::CashOut { .. } => Some(Classification::ExternalFlow),
        EventKind::Fee { origin, .. } => Some(Classification::Fee { origin }),
        EventKind::Income { .. } => Some(Classification::Income),
        EventKind::Trade { .. }
        | EventKind::OpeningPosition { .. }
        | EventKind::OpeningCash { .. }
        | EventKind::Valuation { .. }
        | EventKind::ControlAssertion { .. }
        // Corporate action and tender offer are facts, not decisions
        // of the owner, and are not subject to recalculation by rules. `Income` here
        // would be a plausible silent error: amortization is a
        // return of own capital (§6.5), and assigning it to income
        // means overstating income by the entire returned principal.
        | EventKind::CorporateAction { .. }
        | EventKind::OfferExercise { .. } => None,
    }
}

/// One recalculation-plan correction.
///
/// Unfolds in exactly two steps — reversal and replacement. There is no “change
/// event” variant in the type: the append-only log is enforced by the
/// form, not by the caller's promise (§4.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub target: EventId,
    pub was: Classification,
    pub becomes: Classification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrectionStep {
    Reverse {
        target: EventId,
    },
    Replace {
        target: EventId,
        classification: Classification,
    },
}

impl Correction {
    #[must_use]
    pub const fn steps(&self) -> [CorrectionStep; 2] {
        [
            CorrectionStep::Reverse {
                target: self.target,
            },
            CorrectionStep::Replace {
                target: self.target,
                classification: self.becomes,
            },
        ]
    }
}

/// Plan for recalculating history after a rule change.
///
/// Built from the **active** set of events: reversed
/// and replaced events are not included. Hence idempotence — after
/// applying the plan, the replacement event becomes active, whose
/// classification already matches the rule, and rerunning produces an
/// empty plan automatically, without a separate “already done” check.
///
/// A row not covered by a rule remains unchanged: recalculation does not
/// guess where ingestion does not guess.
pub fn recompute_plan(
    events: &[Event],
    subjects: &BTreeMap<EventId, ClassificationSubject>,
    rules: &[ClassificationRule],
) -> Result<Vec<Correction>, CorrectionError> {
    let mut plan = Vec::new();
    for event in resolve(events)? {
        let (Some(subject), Some(was)) = (subjects.get(&event.id), classification_of(event)) else {
            continue;
        };
        let ClassificationResult::Resolved {
            classification: becomes,
            ..
        } = classify(subject, rules)
        else {
            continue;
        };
        if becomes != was {
            plan.push(Correction {
                target: event.id,
                was,
                becomes,
            });
        }
    }
    Ok(plan)
}
