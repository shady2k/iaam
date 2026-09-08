//! Scenarios for viewing and modifying classification rules.
//!
//! The store is responsible only for version history. Parsing domain JSON and
//! invoking pure recomputation remain in the application scenario; the classification
//! algorithm itself belongs to `iaam-ingest`.

use std::collections::BTreeMap;

use iaam_core::event::Event;
use iaam_core::event::kind::{EventKind, FeeOrigin, IncomeKind};
use iaam_core::ids::{AccountId, ClassificationRuleId, EventId, OwnerId};
use iaam_ingest::classification::{
    Classification, ClassificationRule, ClassificationSubject, Correction, Counterparty, FarSide,
    Movement, RuleMatcher, recompute_plan,
};
use serde_json::Value;
use time::Date;
use uuid::Uuid;

use crate::AppServices;
use crate::error::{AppError, FieldRejection};
use crate::ports::{ClassificationRuleView, Principal};

pub async fn list_rules(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<ClassificationRuleView>, AppError> {
    services.rules.list_rules(principal.owner).await
}

/// A classification named the way the rule that decides it names one.
///
/// The vocabulary is the rule outcome's own — `internal_transfer`,
/// `own_account_movement`, `external_flow`, `refund`, `income`, `fee` — so the
/// plan answers in the words
/// the owner wrote the rule in, rather than in the journal's event
/// discriminants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedAs {
    pub kind: &'static str,
    /// The receiving account, for `internal_transfer` only.
    pub to: Option<AccountId>,
    /// The fee's origin, for `fee` only.
    pub origin: Option<&'static str>,
    /// The earning the owner named, for `income` only.
    ///
    /// Optional twice over, and the two absences are not the same one. The field
    /// is absent from every outcome but `income`, where it means the outcome
    /// takes no such word; and it is absent on `income` itself where the owner
    /// named no kind, which is a decision he made about the rows the rule
    /// matches (§4.9). Only the second can be sent back.
    pub income_kind: Option<&'static str>,
}

/// One event a rule change requires correcting, and what it would become.
///
/// This is [`Correction`] in the transport's vocabulary. It is deliberately not
/// a correction *request*: nothing here has been written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCorrection {
    pub event: EventId,
    pub was: ClassifiedAs,
    pub becomes: ClassifiedAs,
    pub refusal: Option<PlannedCorrectionRefusal>,
}

/// Why one planned correction cannot be constructed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCorrectionRefusal {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// What applying a recomputation plan will write.
///
/// The preview is folded from the same reversal and replacement candidates the
/// correction route will append, so account count and cash figures do not ask
/// the caller to derive a second operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecomputePlan {
    pub corrections: Vec<PlannedCorrection>,
    pub preview: crate::scenarios::correction::ImportCorrectionPreview,
}

/// A rule that was stored, together with what storing it would correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleChange {
    pub rule: ClassificationRuleView,
    pub plan: RecomputePlan,
}

/// Store a rule stated in the classifier's own types, and say what it corrects.
///
/// The condition and the outcome arrive as [`RuleMatcher`] and
/// [`Classification`] and are handed to the store port as exactly that: the
/// store's typed columns (spec §4.6) carry them without a serialisation step,
/// so there is no format for a second writer to drift from.
pub async fn create_rule(
    services: &AppServices,
    principal: &Principal,
    matcher: &RuleMatcher,
    outcome: Classification,
    replaces: Option<Uuid>,
) -> Result<RuleChange, AppError> {
    refuse_unreadable_rules(services, principal.owner).await?;
    let rule = services
        .rules
        .create_rule(principal.owner, matcher.clone(), outcome, replaces)
        .await?;
    let plan = recompute_history(services, principal.owner).await?;
    Ok(RuleChange { rule, plan })
}

/// Read every rule the recomputation will read, before anything is written.
///
/// **This is the ordering, and it is the whole of iaam-y6kt.**
/// [`recompute_history`] reads the owner's entire active rule set with
/// [`rule_from_view`], and a rule it cannot read is an
/// [`AppError::Invalid`] — a 422 at the transport. Run after the write, that
/// refusal reached the caller while the store already held the rule it had just
/// been told was refused, and the only reasonable response to a refusal —
/// sending it again — added a second copy of it. Nothing in the response said
/// so, and `GET /v1/classification-rules` refuses for the same reason, so the
/// caller could not even look.
///
/// The proposal itself cannot be unreadable any more: it arrives as
/// [`RuleMatcher`] and [`Classification`], and the store's columns carry them
/// with no serialisation step to fail (spec §4.6) — the database's own `CHECK`
/// refuses anything outside the closed vocabulary at the write, before this
/// function is ever reached. What this still guards against is a rule already
/// in the set that this build's [`outcome_from`] does not recognise — written
/// by a different build, or by something other than this application
/// entirely — and the ordering fix stands regardless of why a rule turns out
/// unreadable: the check runs before the write, not after it.
///
/// This is **not** a recomputation moved before the write. The plan is still
/// computed afterwards, from the rule set the store actually holds, for the
/// reason [`recompute_history`] gives in its third point: there is exactly one
/// write, so nothing can half-happen. Computing the plan first would mean
/// modelling here what the write will do — which rule the amendment retires,
/// what version the new one is given, and therefore which rule wins a tie — and
/// a second model of the store's own behaviour is a model that drifts, silently,
/// into a plan the owner applies to his journal.
///
/// What remains after the write can still fail: `recompute_plan` refuses a
/// journal it cannot resolve. That is a fact about the journal and not about the
/// rule, it is reported as a 5xx and not as a refusal, and the rule it leaves
/// stored is a valid standing decision whose plan any later call recomputes.
async fn refuse_unreadable_rules(services: &AppServices, owner: OwnerId) -> Result<(), AppError> {
    for rule in services.rules.list_rules(owner).await? {
        if rule.retired_at.is_none() {
            rule_from_view(rule)?;
        }
    }
    Ok(())
}

/// A rule matcher, in the shape a request body carries it — the shape
/// `RuleMatcherDto` publishes, which omits an absent field rather than
/// stating it as `null`.
///
/// Exists because the action queue presets a body from a domain [`RuleMatcher`]
/// it is not itself sending: a preset built by any other encoding would
/// publish one rule in two shapes — once as the proposal on the question it
/// came from and once as the body to send — with nothing to keep them
/// agreeing.
#[must_use]
pub fn matcher_request_json(matcher: &RuleMatcher) -> Value {
    let mut object = serde_json::Map::new();
    for (field, value) in [
        (
            "counterparty_account",
            matcher.counterparty_account.as_ref(),
        ),
        (
            "description_contains",
            matcher.description_contains.as_ref(),
        ),
        ("kind", matcher.kind.as_ref()),
        ("source_category", matcher.source_category.as_ref()),
        ("owner_category", matcher.owner_category.as_ref()),
        ("source_code", matcher.source_code.as_ref()),
    ] {
        if let Some(value) = value {
            object.insert(field.to_owned(), Value::String(value.clone()));
        }
    }
    if let Some(movement) = matcher.movement {
        object.insert(
            "movement".to_owned(),
            Value::String(match movement {
                Movement::In => "in".to_owned(),
                Movement::Out => "out".to_owned(),
            }),
        );
    }
    Value::Object(object)
}

/// A classification, in the wire shape `ClassifiedAsDto` publishes.
///
/// Used to build an action-queue preset body from a domain [`Classification`]
/// — the wire contract's own concern, kept separate from how the store holds
/// the value (spec §4.6 typed columns, no serialisation).
#[must_use]
pub fn outcome_json(classification: Classification) -> Value {
    let named = classified_as(classification);
    // Built by walking the named fields rather than by a ladder over the
    // outcomes. The ladder had one arm per shape of outcome and needed a new one
    // for every field an outcome learned to carry, which is how `income` could
    // gain a kind that this function would have written nowhere — a rule stored
    // without the word the owner chose, and unreadable as the decision he made.
    let mut object = serde_json::Map::new();
    object.insert("kind".to_owned(), Value::String(named.kind.to_owned()));
    if let Some(to) = named.to {
        object.insert("to".to_owned(), Value::String(to.inner().to_string()));
    }
    if let Some(origin) = named.origin {
        object.insert("origin".to_owned(), Value::String(origin.to_owned()));
    }
    if let Some(income_kind) = named.income_kind {
        object.insert(
            "income_kind".to_owned(),
            Value::String(income_kind.to_owned()),
        );
    }
    Value::Object(object)
}

/// Retire a rule and say what the remaining set corrects.
///
/// The readability check runs first for the reason
/// [`refuse_unreadable_rules`] gives, and this route had the same defect:
/// retiring wrote, and the recomputation that followed could refuse with a 422
/// naming a different rule entirely — leaving the rule retired, and a second
/// attempt answered `404`, because a retired rule cannot be retired again. A
/// caller reading the two responses would conclude it had never retired
/// anything.
///
/// Checking the set *before* the retirement is a superset of what the
/// recomputation afterwards reads: retiring only removes a rule from it. So the
/// check cannot pass here and fail there.
pub async fn retire_rule(
    services: &AppServices,
    principal: &Principal,
    id: Uuid,
) -> Result<RecomputePlan, AppError> {
    refuse_unreadable_rules(services, principal.owner).await?;
    services.rules.retire_rule(principal.owner, id).await?;
    recompute_history(services, principal.owner).await
}

/// What the current rule set says the recorded history should be corrected to.
///
/// **The plan is returned, not applied**, and the caller is the owner's
/// transport, which shows it to them. Three reasons, in the order they bind:
///
/// 1. Applying means writing reversal and replacement facts into an
///    append-only journal (§4.8), and [`crate::scenarios::correction`] already
///    refuses to do that without `acknowledge_retraction`, because a retracted
///    fact stops counting in every report the owner has already read and
///    re-submitting the same rows does not bring it back. A rule submitted
///    through `POST /v1/classification-rules` carries no such acknowledgement,
///    and inventing one here would route around a control the owner was
///    deliberately given.
/// 2. The replacement fact is not in the plan. A [`Correction`] names the
///    classification an event *becomes*; the event that expresses it needs a
///    `TransferId`, a sign convention, and — for an internal transfer — a cash
///    leg on a second account the outcome names. That is a new fact about
///    another account, invented by this wrapper, and a wrong sign in it cannot
///    be taken back.
/// 3. Storing the rule and correcting the journal are two writes, and this
///    order is the only safe one. With the plan returned there is exactly one
///    write, so nothing can half-happen. Were the corrections applied first and
///    the rule stored second, a failure of the second write would leave the
///    journal corrected by a rule that does not exist and that no later run
///    could reconstruct; applied second, a failure leaves the rule stored and
///    the plan uncorrected — which is precisely the state this function
///    reports, and a repeat run recomputes the same plan, because the plan is
///    built from the effective set and is idempotent by construction.
///
/// The operation that applies it already exists: `POST /v1/corrections`, which
/// takes the acknowledgement and writes the reversal and replacement facts.
///
/// `recompute_plan` remains the sole place that determines which events require
/// correction. This scenario does not modify events itself or perform monetary
/// arithmetic in the wrapper; it only builds the same correction candidates and
/// preview that the acknowledged route will later write.
async fn recompute_history(
    services: &AppServices,
    owner: OwnerId,
) -> Result<RecomputePlan, AppError> {
    let events = services.store.load_events_through(owner, Date::MAX).await?;
    let stored = services.rules.list_rules(owner).await?;
    let rules = stored
        .into_iter()
        .filter(|rule| rule.retired_at.is_none())
        .map(rule_from_view)
        .collect::<Result<Vec<_>, _>>()?;
    let subjects = events
        .iter()
        .filter_map(|event| subject(event).map(|subject| (event.id, subject)))
        .collect::<BTreeMap<EventId, ClassificationSubject>>();
    let corrections = recompute_plan(&events, &subjects, &rules)
        .map_err(|error| AppError::Store(format!("classification recomputation: {error}")))?;
    let effective = iaam_core::event::correction::resolve(&events).map_err(AppError::Correction)?;
    let by_id: BTreeMap<EventId, &Event> = events.iter().map(|event| (event.id, event)).collect();
    let mut targets = Vec::with_capacity(corrections.len());
    let mut candidates = Vec::with_capacity(corrections.len() * 2);
    let mut planned_corrections = Vec::with_capacity(corrections.len());
    for (index, correction) in corrections.iter().enumerate() {
        let Some(target) = by_id.get(&correction.target).copied() else {
            planned_corrections.push(planned(
                correction,
                Some(PlannedCorrectionRefusal {
                    field: format!("corrections[{index}].target"),
                    expected: "an identifier of an event in this owner's journal".to_owned(),
                    actual: correction.target.inner().to_string(),
                }),
            ));
            continue;
        };
        match crate::scenarios::correction::reclassification_candidates_for_plan(
            owner,
            target,
            correction.becomes,
            index,
        ) {
            Ok(mut row_candidates) => {
                targets.push(target);
                candidates.append(&mut row_candidates);
                planned_corrections.push(planned(correction, None));
            }
            Err(error) => {
                planned_corrections.push(planned(correction, Some(planned_refusal(error, index))));
            }
        }
    }
    let preview = crate::scenarios::correction::preview_for_reclassification_batch(
        &effective,
        &targets,
        &candidates,
    )?;
    Ok(RecomputePlan {
        corrections: planned_corrections,
        preview,
    })
}

fn planned(
    correction: &Correction,
    refusal: Option<PlannedCorrectionRefusal>,
) -> PlannedCorrection {
    PlannedCorrection {
        event: correction.target,
        was: classified_as(correction.was),
        becomes: classified_as(correction.becomes),
        refusal,
    }
}

fn planned_refusal(error: AppError, index: usize) -> PlannedCorrectionRefusal {
    match error {
        AppError::Invalid {
            field,
            expected,
            actual,
        } => PlannedCorrectionRefusal {
            field,
            expected,
            actual,
        },
        AppError::InvalidField(rejection) => PlannedCorrectionRefusal {
            field: rejection.field,
            expected: rejection.expected,
            actual: rejection.actual,
        },
        other => PlannedCorrectionRefusal {
            field: format!("corrections[{index}].correction"),
            expected: "a correction the server can construct".to_owned(),
            actual: other.to_string(),
        },
    }
}

/// The inverse of [`parse_outcome`], and it must stay so: the plan speaks the
/// vocabulary the owner writes rules in, or it names a decision they cannot
/// restate as a rule.
///
/// Public because the transport renders a stored rule's outcome with it. A rule
/// read back in a vocabulary other than the one it may be written in is a rule
/// a caller cannot send again, which is the whole of iaam-gpo3.
#[must_use]
pub const fn classified_as(classification: Classification) -> ClassifiedAs {
    match classification {
        Classification::InternalTransfer { to } => ClassifiedAs {
            kind: "internal_transfer",
            to: Some(to),
            origin: None,
            income_kind: None,
        },
        Classification::ExternalFlow => ClassifiedAs {
            kind: "external_flow",
            to: None,
            origin: None,
            income_kind: None,
        },
        // No `to`, and that is the outcome rather than a field left out: the
        // whole difference from `internal_transfer` is that no far account is
        // named. A reader that filled it in from somewhere would be writing the
        // one thing the source did not say.
        Classification::OwnAccountMovement => ClassifiedAs {
            kind: "own_account_movement",
            to: None,
            origin: None,
            income_kind: None,
        },
        Classification::Refund => ClassifiedAs {
            kind: "refund",
            to: None,
            origin: None,
            income_kind: None,
        },
        Classification::Income { kind } => ClassifiedAs {
            kind: "income",
            to: None,
            origin: None,
            income_kind: match kind {
                None => None,
                Some(IncomeKind::Coupon) => Some("coupon"),
                Some(IncomeKind::Dividend) => Some("dividend"),
                Some(IncomeKind::DepositInterest) => Some("deposit_interest"),
            },
        },
        Classification::Fee { origin } => ClassifiedAs {
            kind: "fee",
            to: None,
            origin: Some(match origin {
                FeeOrigin::Brokerage => "brokerage",
                FeeOrigin::Depositary => "depositary",
                FeeOrigin::AccountMaintenance => "account_maintenance",
                FeeOrigin::MarginInterest => "margin_interest",
                FeeOrigin::Other => "other",
            }),
            income_kind: None,
        },
    }
}

/// A stored rule in the classifier's own vocabulary.
///
/// A field move now, not a parse: [`ClassificationRuleView`] already carries
/// `RuleMatcher` and `Classification` (spec §4.6), so there is no format left
/// for this and a second reader to disagree about. Kept as its own function,
/// with a `Result` it now always fulfils, because every external caller of it
/// already goes through the `?` this signature invites — changing it would
/// touch every one of them for no remaining reason.
///
/// Shared with the import session on purpose: the session classifies an incoming
/// row against the same rules the recomputation replays history with, and two
/// readings of one stored matcher would eventually disagree about what the owner
/// decided.
pub fn rule_from_view(rule: ClassificationRuleView) -> Result<ClassificationRule, AppError> {
    Ok(ClassificationRule {
        id: ClassificationRuleId(rule.id),
        version: rule.version,
        matcher: rule.matcher,
        outcome: rule.outcome,
    })
}

/// The outcome vocabulary, read from three fields.
///
/// The request body carries these as named DTO members on the wire, and this
/// is the one place that turns them into [`Classification`] — a second reader
/// would eventually admit a word this one refuses, and a rule accepted at the
/// door that the classifier cannot construct is a decision lost in the same
/// call.
pub fn outcome_from(
    kind: &str,
    to: Option<&str>,
    origin: Option<&str>,
    income_kind: Option<&str>,
) -> Result<Classification, AppError> {
    match kind {
        "internal_transfer" => {
            let raw = to.ok_or_else(|| invalid_outcome("field to"))?;
            let to = Uuid::parse_str(raw).map_err(|_| invalid_outcome(raw))?;
            Ok(Classification::InternalTransfer { to: AccountId(to) })
        }
        "external_flow" => Ok(Classification::ExternalFlow),
        // Refused if it names a `to`, unlike `internal_transfer`, which
        // requires one. The two outcomes differ by exactly that field, so a
        // caller that sent both words and an account has contradicted itself
        // and must be told rather than have one half quietly dropped.
        "own_account_movement" => match to {
            None => Ok(Classification::OwnAccountMovement),
            Some(actual) => Err(invalid_outcome(actual)),
        },
        "refund" => Ok(Classification::Refund),
        // Absent means the owner named no kind, and that is a rule he is
        // entitled to write: it says the rows this matches are income of a kind
        // nobody stated. A word this reader does not know is refused rather than
        // dropped to `None` — dropping it would store a weaker decision than the
        // one he sent and tell him nothing (§4.9).
        "income" => Ok(Classification::Income {
            kind: match income_kind {
                None => None,
                Some("coupon") => Some(IncomeKind::Coupon),
                Some("dividend") => Some(IncomeKind::Dividend),
                Some("deposit_interest") => Some(IncomeKind::DepositInterest),
                Some(actual) => Err(invalid_income_kind(actual))?,
            },
        }),
        "fee" => Ok(Classification::Fee {
            origin: match origin {
                Some("brokerage") => FeeOrigin::Brokerage,
                Some("depositary") => FeeOrigin::Depositary,
                Some("account_maintenance") => FeeOrigin::AccountMaintenance,
                Some("margin_interest") => FeeOrigin::MarginInterest,
                Some("other") => FeeOrigin::Other,
                Some(actual) => Err(invalid_outcome(actual))?,
                None => Err(invalid_outcome("field origin"))?,
            },
        }),
        actual => Err(invalid_outcome(actual)),
    }
}

/// The outcome vocabulary is closed, so the refusal publishes it as values.
///
/// `expected` still spells the same four out in prose, because that sentence is
/// what the error message reads as. The list beside it is the half a client can
/// retry from without parsing anything.
fn invalid_outcome(actual: &str) -> AppError {
    FieldRejection::new(
        "outcome",
        "internal_transfer, own_account_movement, external_flow, refund, income or fee",
        actual,
    )
    .admitting_codes(&[
        "internal_transfer",
        "own_account_movement",
        "external_flow",
        "refund",
        "income",
        "fee",
    ])
    .into()
}

/// The income vocabulary is closed too, and names its own field.
///
/// Separate from [`invalid_outcome`] so the refusal points at the word that was
/// wrong. A caller told that `income` is not one of the outcomes, when `income`
/// is exactly what it sent and `interest` was the unknown word beside it, is
/// being sent to fix a field it got right.
fn invalid_income_kind(actual: &str) -> AppError {
    FieldRejection::new(
        "outcome.income_kind",
        "coupon, dividend or deposit_interest, or nothing where the owner named no kind",
        actual,
    )
    .admitting_codes(&["coupon", "dividend", "deposit_interest"])
    .into()
}

/// One recorded fact, as a standing rule is tested against it.
///
/// `None` for a fact no rule classifies — a trade, a valuation, a corporate
/// action — and that absence is a decision rather than a gap: those are facts
/// and not the owner's classifications, so no rule of his is ever tested
/// against them.
///
/// **Visible to the crate since `iaam-uibl`, and to exactly one other caller.**
/// The import session's forecast has to answer «which recorded movements would
/// this condition reach» before the condition stands, and that question is only
/// worth answering if it is answered by the same reader the recomputation
/// replays history with. A second reading of a fact into a subject there would
/// let a forecast promise a reach the classifier does not perform — the
/// `source_category` boundary below is exactly the kind of rule the two copies
/// would disagree about first.
pub(crate) fn subject(event: &Event) -> Option<ClassificationSubject> {
    let (counterparty, movement, far_side) = match event.kind {
        EventKind::CashIn { .. } | EventKind::Income { .. } | EventKind::Refund { .. } => {
            (Counterparty::Unknown, Some(Movement::In), FarSide::Unstated)
        }
        EventKind::CashOut { .. } | EventKind::Fee { .. } => (
            Counterparty::Unknown,
            Some(Movement::Out),
            FarSide::Unstated,
        ),
        EventKind::CashTransfer { from, to, .. } => {
            if from == event.account {
                (
                    Counterparty::Named(to.inner().to_string()),
                    Some(Movement::Out),
                    FarSide::Unstated,
                )
            } else {
                (
                    Counterparty::Named(from.inner().to_string()),
                    Some(Movement::In),
                    FarSide::Unstated,
                )
            }
        }
        // The assertion is carried back into the subject, because it is what
        // the source said and recomputation must reconsider the row on the same
        // evidence it was first read on. Dropping it would make every one of
        // these events look, to `recompute_plan`, like a row whose source said
        // nothing — and a rule written for exactly such rows would then be
        // proposed against them as a correction.
        EventKind::OwnAccountMovement { amount } => (
            Counterparty::Unknown,
            Some(if amount.amount().raw() < 0 {
                Movement::Out
            } else {
                Movement::In
            }),
            FarSide::OwnAccount,
        ),
        // No direction, for the reason the fact has no leg. `None` here is the
        // same `None` an observation carries, and it reaches the same place.
        EventKind::UnresolvedOwnAccountMovement { .. } => {
            (Counterparty::Unknown, None, FarSide::OwnAccount)
        }
        EventKind::Trade { .. }
        | EventKind::OpeningPosition { .. }
        | EventKind::OpeningCash { .. }
        | EventKind::Valuation { .. }
        | EventKind::ControlAssertion { .. }
        | EventKind::ImportCoverageGap { .. }
        // A corporate action and an offer do not become classification
        // subjects. Returning them as inflows would mean asking
        // the owner «this inflow — is it income?» about amortisation and recording
        // their answer as a rule: a return of their own capital (§6.5)
        // would forever be counted as income. This is also why
        // `classification_of` returns `None` for them.
        // A tax already identifies its own fact and is not a rule-classified expense.
        | EventKind::Tax { .. }
        | EventKind::CorporateAction { .. }
        | EventKind::OfferExercise { .. } => return None,
    };
    Some(ClassificationSubject {
        account: event.account,
        counterparty,
        // Both come from provenance, which retained what the source said about
        // the row and never rewrites it. Anything else would ask the
        // recomputation to reconsider a classification using the previous
        // classification as its input: the event discriminant — `cash_out`,
        // `income` — is the answer a rule is meant to revise, not the question
        // the rule was written about.
        description: event.provenance.description().map(str::to_owned),
        // The source's operation word, from the field that holds it. `None`
        // is a source that printed no such word, or a fact recorded before
        // the field existed.
        source_kind: event.provenance.source_kind().map(str::to_owned),
        // The source's own category, straight off provenance: nothing rewrites
        // it, and nothing needs to be read around it — every fact in the
        // journal was written by a path that keeps this word and the operation
        // word (`source_kind`, above) in their own fields.
        source_category: event.provenance.source_category().map(str::to_owned),
        // Straight off provenance, for the same reason as `source_category`
        // above. `None` is a fact recorded before the profile read the
        // columns, or a source that printed nothing — and both mean the
        // journal holds no such evidence about this row.
        owner_category: event.provenance.owner_category().map(str::to_owned),
        source_code: event.provenance.source_code().map(str::to_owned),
        movement,
        far_side,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use iaam_core::event::corporate_action::CorporateAction;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId};
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Relation};
    use iaam_core::ids::{CustodyId, InstrumentId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn event_of(account: AccountId, kind: EventKind, legs: Vec<Leg>) -> Event {
        let day = date!(2026 - 06 - 15);
        Event {
            id: EventId::new_random(),
            owner: OwnerId::new_random(),
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, 0),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"e".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn cash_out_of(account: AccountId, provenance: Provenance) -> Event {
        let amount = rub(-500_000);
        Event {
            provenance,
            ..event_of(
                account,
                EventKind::CashOut { amount },
                vec![Leg::cash(account, amount)],
            )
        }
    }

    fn provenance_of() -> Provenance {
        Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"a".repeat(64)).unwrap(),
            ParserVersion("test/1".into()),
        )
    }

    fn rule_matching(matcher: RuleMatcher) -> ClassificationRule {
        ClassificationRule {
            id: ClassificationRuleId(Uuid::new_v4()),
            version: 1,
            matcher,
            outcome: Classification::Fee {
                origin: FeeOrigin::AccountMaintenance,
            },
        }
    }

    #[test]
    fn the_rebuilt_subject_carries_the_description_the_source_printed() {
        // Without this the `description_contains` third of `RuleMatcher` is dead
        // on the recompute path: a rule the owner wrote about what the bank
        // printed on the row matches at intake and matches nothing afterwards.
        let account = AccountId::new_random();
        let event = cash_out_of(account, provenance_of().with_description("Shop One"));

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(subject.description.as_deref(), Some("Shop One"));
        assert!(
            rule_matching(RuleMatcher {
                counterparty_account: None,
                description_contains: Some("shop one".to_owned()),
                kind: None,
                source_category: None,
                owner_category: None,
                source_code: None,
                movement: None,
            })
            .matcher
            .matches(&subject),
            "a rule naming the description must match on recompute"
        );
    }

    #[test]
    fn the_rebuilt_subject_carries_the_word_the_source_used_not_the_event_kind() {
        // `source_kind` is «what the source called the operation». Filling it
        // with the event's own discriminant asks the recomputation to reconsider
        // a classification using the previous classification as its input.
        let account = AccountId::new_random();
        let event = cash_out_of(account, provenance_of().with_source_kind("Transfers"));

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(subject.source_kind.as_deref(), Some("Transfers"));
        assert_ne!(
            subject.source_kind.as_deref(),
            Some(event.kind.discriminant()),
            "the event discriminant is the answer, not the question"
        );
        assert!(
            rule_matching(RuleMatcher {
                counterparty_account: None,
                description_contains: None,
                kind: Some("Transfers".to_owned()),
                source_category: None,
                owner_category: None,
                source_code: None,
                movement: None,
            })
            .matcher
            .matches(&subject),
            "a rule naming the source's own word must match on recompute"
        );
    }

    #[test]
    fn a_source_that_named_neither_leaves_both_fields_unknown() {
        // Unknown is `None`, not a substitute (§4.9). Falling back to the
        // event discriminant here would restore the circularity for exactly
        // the rows whose source said nothing.
        let account = AccountId::new_random();
        let event = cash_out_of(account, provenance_of());

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(subject.description, None);
        assert_eq!(subject.source_kind, None);
    }

    #[test]
    fn the_source_category_is_not_read_back_as_the_operation_word() {
        // The defect this pair was written through (`iaam-p683`): the two are
        // different facts, and a subject that took the category as the
        // operation word made every classification rule on a source's word fire
        // on rows the owner had described by category instead.
        let account = AccountId::new_random();
        let event = cash_out_of(
            account,
            provenance_of()
                .with_source_category("Groceries")
                .with_source_kind("card payment"),
        );

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(
            subject.source_kind.as_deref(),
            Some("card payment"),
            "the operation word, from the field that holds it"
        );
        assert_ne!(
            subject.source_kind.as_deref(),
            Some("Groceries"),
            "the category is what the money was for, and answers a different question"
        );
    }

    #[test]
    fn the_rebuilt_subject_carries_the_category_the_source_filed_the_row_under() {
        // Without this the fourth arm of `RuleMatcher` is dead on the recompute
        // path exactly as the description once was: a rule the owner wrote about
        // the category his bank printed would settle a row at intake and match
        // nothing when he edited the rule and asked for the plan.
        let account = AccountId::new_random();
        let event = cash_out_of(
            account,
            provenance_of().with_source_category("Bank interest"),
        );

        let subject = subject(&event).expect("a cash outflow is a classification subject");

        assert_eq!(subject.source_category.as_deref(), Some("Bank interest"));
        assert!(
            rule_matching(RuleMatcher {
                counterparty_account: None,
                description_contains: None,
                kind: None,
                source_category: Some("Bank interest".to_owned()),
                owner_category: None,
                source_code: None,
                movement: None,
            })
            .matcher
            .matches(&subject),
            "a rule naming the source's own category must match on recompute"
        );
    }

    #[test]
    fn the_request_shape_omits_what_the_rule_does_not_ask_about() {
        // The store no longer holds a serialisation to round-trip through
        // (spec §4.6 typed columns): `matcher_request_json` has exactly one
        // remaining job, building the action queue's preset request body from
        // a domain `RuleMatcher`, and this is the shape that body must have —
        // an absent condition is an absent key, not an explicit `null`, which
        // is what the schema a caller validates against describes.
        let matcher = RuleMatcher {
            movement: None,
            counterparty_account: None,
            description_contains: None,
            kind: None,
            source_category: Some("Bank interest".to_owned()),
            owner_category: None,
            source_code: None,
        };

        assert_eq!(
            matcher_request_json(&matcher),
            serde_json::json!({ "source_category": "Bank interest" }),
            "the request shape omits what the rule does not ask about"
        );
    }

    #[test]
    fn an_amortisation_is_not_offered_to_the_owner_as_an_inflow() {
        // Returning amortisation as a subject with `Movement::In` would mean
        // asking the owner «this inflow — is it income?» and recording their answer
        // as a rule: a return of their own capital (§6.5) would forever
        // be counted as income.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let compensation = rub(2_000_000);
        let event = event_of(
            account,
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: Quantity(Dec::new(Decimal::from(100))),
                    principal_returned_per_unit: PerUnitAmount::new(
                        Dec::new(Decimal::from(200)),
                        CurrencyCode::Rub,
                    ),
                    compensation,
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
                },
            },
            vec![Leg::principal(account, instrument, compensation)],
        );
        assert!(subject(&event).is_none());
    }

    #[test]
    fn a_settled_offer_is_not_offered_to_the_owner_either() {
        let account = AccountId::new_random();
        let event = event_of(
            account,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: Quantity(Dec::new(Decimal::from(10))),
                    gross: rub(1_000_000),
                    fee: None,
                    accrued_interest: None,
                },
            },
            Vec::new(),
        );
        assert!(subject(&event).is_none());
    }
}
