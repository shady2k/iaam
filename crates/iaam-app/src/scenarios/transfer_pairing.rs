//! One transfer, seen twice, related once (iaam-3ul2).
//!
//! One economic movement between two of the owner's banks is printed by each
//! side as its own row: an outgoing row at one institution and an incoming row
//! at the other. Nothing in either row says the two are one movement, and
//! recorded independently they become a `cash_out` and a `cash_in` related by
//! nothing. For a contour spanning both institutions that is wrong twice over —
//! a flow report counts an external outflow and an external inflow that never
//! happened.
//!
//! **Nothing here decides that two rows are one movement.** [`propose`] names
//! candidates and the evidence they rest on; the owner confirms one, and only
//! then is anything written. Two rows that merely look alike are not one fact,
//! and deciding they are is exactly the fabrication this system exists to
//! refuse. The rule holds even for a single exact match: two payments of the
//! same round amount on the same day between two of the owner's own accounts
//! are entirely ordinary, and no field in either row tells them apart from one
//! transfer printed twice.
//!
//! **A leg nothing paired with stays visible.** [`Proposals::unmatched`] is
//! reported beside the candidates rather than dropped, because a leg that
//! disappears from the answer is a leg the owner reads as external flow by
//! default — which is the defect, not the fix.
//!
//! That list is therefore where an ordinary one-sided movement stays, and stays
//! permanently. A payment in a shop is a cash movement no counterpart will ever
//! be proposed for, and it is reported unmatched on every reading, for good. A
//! source none of whose rows are transfers yields no candidates and an unmatched
//! leg for every row: that is this module working, not failing at anything.
//!
//! The same [`propose`] serves two callers, and deliberately one function:
//! [`propose_journal_pairings`] over the events already recorded, and the import
//! session's assessment over the rows it is about to record. A second matcher
//! for the second caller would drift from the first, and the owner would be
//! shown candidates the commit does not agree with.

use std::collections::BTreeSet;

use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::ids::{AccountId, EventId, ImportId, ImportSessionId};
use iaam_core::money::CurrencyCode;
use iaam_ingest::classification::Movement;
use iaam_ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use time::Date;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::Principal;
use crate::scenarios::correction::{CorrectionRequest, correct_events};

/// How far apart two legs of one transfer may be posted and still be proposed.
///
/// Three days, and it is a **window for proposing**, never for deciding: the
/// two banks post the same movement on their own schedules, and a same-day-only
/// rule would silently drop every pair that crossed a weekend. Widening it adds
/// candidates the owner has to read, never facts the system writes.
pub const MATCH_WINDOW_DAYS: i64 = 3;

/// Where one leg is: already a fact, or still a row waiting to become one.
///
/// Two variants rather than one identifier, because the two are not
/// interchangeable. A recorded leg is corrected — the journal is append-only and
/// a fact already read cannot be edited. An observed leg has not been asserted
/// yet, so relating it costs nothing but rewriting the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LegOrigin {
    /// Recorded in the journal.
    Recorded { event: EventId },
    /// Held in an import session, not yet committed.
    Observed { session: ImportSessionId, row: u32 },
}

/// One side of a cash movement, which may or may not be half of a transfer.
///
/// Built from a recorded event or from a session's planned row, and identical in
/// both cases: the matcher must not be able to tell them apart, or it would
/// propose different pairs before and after a commit.
///
/// Named for cash rather than for transfers, and renamed from `TransferLeg` to
/// say so. Every `CashOut` and `CashIn` carrying a posting date becomes one of
/// these, and most of them are a shop payment or a salary that no transfer will
/// ever claim; the old name asserted of every such row the one thing [`propose`]
/// exists to leave to the owner. The transport type is still `TransferLegDto`,
/// because that name is published in the OpenAPI schema and clients hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CashLeg {
    pub origin: LegOrigin,
    pub account: AccountId,
    /// Which way the money went on this account.
    pub direction: Movement,
    /// The magnitude in minor units, always positive: the direction is carried
    /// by `direction` and a sign beside it would be a second, disagreeable
    /// statement of the same thing.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    /// The day the cash moved, as the source posted it.
    pub date: Date,
    /// What the source printed beside the row — its description, its own
    /// operation identifier. Evidence the owner reads, never a thing matched on:
    /// the two banks describe one movement in two vocabularies.
    pub reference: Option<String>,
    /// The import the row arrived in, when the submission named one.
    pub import: Option<ImportId>,
}

/// What two legs agree on, for the owner to judge the proposal by.
///
/// Reported rather than scored. A number saying "87% confident" would be an
/// opinion this system has no basis for, and the owner cannot check it; the
/// fields the match was made on, printed as they stand, are checkable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingEvidence {
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    pub outgoing_date: Date,
    pub incoming_date: Date,
    /// Whole days between the two postings, never negative.
    pub days_apart: i64,
    pub outgoing_reference: Option<String>,
    pub incoming_reference: Option<String>,
    /// Whether each leg has exactly this one counterpart.
    ///
    /// `false` is the case that matters: three rows of one amount on one day
    /// produce candidates that cannot all be true, and a proposal that did not
    /// say so would invite the owner to confirm two of them.
    pub sole_candidate: bool,
}

/// Two legs proposed as one movement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCandidate {
    pub outgoing: CashLeg,
    pub incoming: CashLeg,
    pub evidence: PairingEvidence,
}

/// Everything the matching pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Proposals {
    pub candidates: Vec<PairingCandidate>,
    /// Legs no candidate covers: reported, never dropped, and ordinarily most
    /// of them. Emptying this is not a goal — it empties only when every
    /// movement handed to [`propose`] happens to be half of a proposed pair.
    pub unmatched: Vec<CashLeg>,
}

/// What confirming one pairing wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmedPairing {
    /// The outgoing leg, now superseded by the transfer.
    pub outgoing: EventId,
    /// The incoming leg, now retracted: the transfer carries both sides.
    pub incoming: EventId,
    /// The transfer that replaced the outgoing leg, when the journal accepted
    /// it as a new fact. Absent when the key was already held, which is what a
    /// repeated confirmation of one pairing looks like.
    pub transfer: Option<EventId>,
}

/// Pair what can be paired, and say what could not.
///
/// Pure: the whole decision is the five fields below, so the same rows produce
/// the same candidates in an assessment and at commit.
///
/// Two legs are proposed when they agree on currency and magnitude, went
/// opposite ways, are on two different accounts, and were posted within
/// [`MATCH_WINDOW_DAYS`] of each other. Nothing else — in particular not the
/// description, which the two banks write in two vocabularies, and not the
/// institution, which this layer has no business knowing.
#[must_use]
pub fn propose(legs: &[CashLeg]) -> Proposals {
    let mut candidates = Vec::new();
    for outgoing in legs.iter().filter(|leg| leg.direction == Movement::Out) {
        for incoming in legs.iter().filter(|leg| leg.direction == Movement::In) {
            if pairs(outgoing, incoming) {
                candidates.push((outgoing.clone(), incoming.clone()));
            }
        }
    }

    // A leg that appears in more than one candidate makes every candidate it is
    // in doubtful, and the doubt belongs on all of them: told about one pair and
    // not the other, the owner would confirm the first he was shown.
    let mut seen_out: BTreeSet<LegOrigin> = BTreeSet::new();
    let mut twice_out: BTreeSet<LegOrigin> = BTreeSet::new();
    let mut seen_in: BTreeSet<LegOrigin> = BTreeSet::new();
    let mut twice_in: BTreeSet<LegOrigin> = BTreeSet::new();
    for (outgoing, incoming) in &candidates {
        if !seen_out.insert(outgoing.origin) {
            twice_out.insert(outgoing.origin);
        }
        if !seen_in.insert(incoming.origin) {
            twice_in.insert(incoming.origin);
        }
    }

    let paired: BTreeSet<LegOrigin> = candidates
        .iter()
        .flat_map(|(outgoing, incoming)| [outgoing.origin, incoming.origin])
        .collect();

    Proposals {
        candidates: candidates
            .into_iter()
            .map(|(outgoing, incoming)| {
                let sole_candidate =
                    !twice_out.contains(&outgoing.origin) && !twice_in.contains(&incoming.origin);
                let evidence = PairingEvidence {
                    amount_minor: outgoing.amount_minor,
                    currency: outgoing.currency,
                    outgoing_date: outgoing.date,
                    incoming_date: incoming.date,
                    days_apart: days_apart(outgoing.date, incoming.date),
                    outgoing_reference: outgoing.reference.clone(),
                    incoming_reference: incoming.reference.clone(),
                    sole_candidate,
                };
                PairingCandidate {
                    outgoing,
                    incoming,
                    evidence,
                }
            })
            .collect(),
        unmatched: legs
            .iter()
            .filter(|leg| !paired.contains(&leg.origin))
            .cloned()
            .collect(),
    }
}

fn pairs(outgoing: &CashLeg, incoming: &CashLeg) -> bool {
    outgoing.account != incoming.account
        && outgoing.currency == incoming.currency
        && outgoing.amount_minor == incoming.amount_minor
        && days_apart(outgoing.date, incoming.date) <= MATCH_WINDOW_DAYS
}

fn days_apart(left: Date, right: Date) -> i64 {
    (left - right).whole_days().abs()
}

/// The legs one recorded event offers the matcher, if it offers any.
///
/// Three kinds and not every kind: a `CashTransfer` already names both of its
/// accounts, so it is not half of anything, and neither is a fee, a trade or a
/// valuation. An event with no cash-posted date is skipped rather than dated
/// from something else — a proposal resting on a date nobody stated is a
/// proposal resting on nothing.
///
/// Every event of those kinds qualifies, deliberately and without judgement of
/// what it looks like. Whether a row is half of a transfer is the question
/// [`propose`] puts to the owner; withholding rows here on a guess about the
/// description or the counterparty would answer it earlier, on worse evidence,
/// and invisibly.
///
/// # Why an own-account movement is one of the three (`iaam-9ck1`)
///
/// `EventKind::OwnAccountMovement` is a movement the source said was between
/// the owner's own accounts **without saying which two**: one signed leg, no
/// far side, and `contour::classify` therefore answers `Indeterminate` for it —
/// rightly, because «an account of the owner's» is not «inside this contour»
/// and no membership test can make it one. Two such rows, one on each side of
/// one movement, are the ordinary shape of a bank that files its internal
/// transfers under a word and names nothing.
///
/// While this function offered only `CashOut` and `CashIn`, those two rows
/// could not be **proposed** as a pair, did not appear in
/// [`Proposals::unmatched`], and could not be handed to
/// [`confirm_journal_pairing`]. So every internal transfer a source settled by
/// asserting a far side became two indeterminate quantities in the money-flow
/// report, permanently, with no owner-facing route back. The fact was right and
/// the classification was right; what was missing was the one mechanism that
/// turns two half-facts into the movement they were.
///
/// `EventKind::UnresolvedOwnAccountMovement` is deliberately **not** offered.
/// It is the same assertion with no direction: it posts no leg at all, and a
/// leg needs a side. Proposing it would mean choosing a direction for it here,
/// which is the guess the whole shape exists to refuse; what it needs is the
/// direction, and that is a question for the owner and not a pairing.
#[must_use]
pub fn leg_of_event(event: &Event) -> Option<CashLeg> {
    let (direction, amount) = match &event.kind {
        EventKind::CashOut { amount } => (Movement::Out, *amount),
        EventKind::CashIn { amount } => (Movement::In, *amount),
        // The direction is read off the sign of the leg the fact posts, which
        // is the only place it is stated: the kind carries no direction word,
        // and `normalize` is what put the sign there from the owner's or the
        // source's own statement of it.
        EventKind::OwnAccountMovement { amount } => {
            let raw = amount.amount().raw();
            if raw > 0 {
                (Movement::In, *amount)
            } else if raw < 0 {
                (Movement::Out, *amount)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let magnitude = amount.amount().raw().checked_abs().filter(|it| *it > 0)?;
    let date = event.dates.cash_posted.map(|posted| posted.0)?;
    Some(CashLeg {
        origin: LegOrigin::Recorded { event: event.id },
        account: event.account,
        direction,
        amount_minor: magnitude,
        currency: amount.currency(),
        date,
        reference: event
            .provenance
            .description()
            .or_else(|| event.provenance.source_operation_id())
            .map(str::to_owned),
        import: event.provenance.import(),
    })
}

/// Candidates over everything the journal already holds.
///
/// The effective set, not every row ever written: a leg already superseded by a
/// confirmed pairing must not be proposed a second time, and a retracted one is
/// not a movement at all.
pub async fn propose_journal_pairings(
    services: &AppServices,
    principal: &Principal,
) -> Result<Proposals, AppError> {
    let events = services
        .store
        .load_events_through(principal.owner, Date::MAX)
        .await?;
    let effective = iaam_core::event::correction::resolve(&events).map_err(AppError::Correction)?;
    let legs: Vec<CashLeg> = effective.into_iter().filter_map(leg_of_event).collect();
    Ok(propose(&legs))
}

/// Relate two recorded legs, on the owner's word.
///
/// Refused unless the two are a pair this build would propose. That is not
/// ceremony: the confirmation names two identifiers, and without the check a
/// caller could relate any outflow to any inflow — which would make this route
/// the fabrication the proposal exists to prevent, only with the owner's name on
/// it.
///
/// What it writes is two correction facts, and no new kind of state:
///
/// 1. The outgoing leg is **replaced** by one `CashTransfer` from its account to
///    the incoming leg's. That event carries a leg on each side, so the money is
///    where it always was and neither crosses the contour boundary.
/// 2. The incoming leg is **reversed**, because the transfer already states it.
///    Left standing it would be counted twice.
///
/// A relation kept outside the journal — a table saying "these two are one" —
/// would be a second notion of what is effective, and the append-only journal
/// would stop being the whole account of what the owner knows. `correct_import`
/// refuses the same shortcut for the same reason.
pub async fn confirm_journal_pairing(
    services: &AppServices,
    principal: &Principal,
    outgoing: EventId,
    incoming: EventId,
    acknowledge_retraction: bool,
) -> Result<ConfirmedPairing, AppError> {
    let proposals = propose_journal_pairings(services, principal).await?;
    let candidate = proposals
        .candidates
        .iter()
        .find(|candidate| {
            candidate.outgoing.origin == LegOrigin::Recorded { event: outgoing }
                && candidate.incoming.origin == LegOrigin::Recorded { event: incoming }
        })
        .ok_or_else(|| AppError::NotFound {
            what: "a proposed transfer pairing of these two events",
            id: format!("{} and {}", outgoing.inner(), incoming.inner()),
        })?;

    let operation = transfer_for(candidate, outgoing, incoming);
    let outcomes = correct_events(
        services,
        principal,
        acknowledge_retraction,
        &[
            CorrectionRequest::Replacement {
                target: outgoing,
                operation: Box::new(operation),
            },
            CorrectionRequest::Reversal { target: incoming },
        ],
    )
    .await?;

    Ok(ConfirmedPairing {
        outgoing,
        incoming,
        transfer: outcomes.first().and_then(|outcome| match outcome.verdict {
            iaam_ingest::Verdict::Provisional { event }
            | iaam_ingest::Verdict::Accepted { event } => Some(event),
            _ => None,
        }),
    })
}

/// The transfer the confirmed pair becomes.
///
/// Submitted from the sending side, which is what `Transfer` means everywhere
/// else in this system: `account` is where the money left and `to` is where it
/// arrived. The dates are the outgoing leg's, because a transfer is one movement
/// and it has one posting day; the incoming leg's day stays visible in the
/// evidence the owner confirmed and in the reversed fact itself.
fn transfer_for(
    candidate: &PairingCandidate,
    outgoing: EventId,
    incoming: EventId,
) -> SubmittedOperation {
    SubmittedOperation {
        account: candidate.outgoing.account,
        kind: OperationKind::Transfer {
            to: candidate.incoming.account,
            amount_minor: candidate.evidence.amount_minor,
            currency: candidate.evidence.currency,
        },
        dates: OperationDates {
            trade: None,
            settled: None,
            cash_posted: Some(candidate.evidence.outgoing_date),
            paid: None,
        },
        source_time: None,
        // Derived from the two events, so confirming one pairing twice is
        // recognised as the same act rather than written as a second transfer.
        idempotency_key: Some(format!("pairing/{}/{}", outgoing.inner(), incoming.inner())),
        source_operation_id: None,
        source_category: None,
        // Neither the owner's own word at either bank nor either bank's code:
        // for the reason the description is dropped just below, and one of its
        // own — the two legs may carry two different words of his, and choosing
        // one would file the transfer under a decision he made about one side.
        owner_category: None,
        source_code: None,
        source_kind: None,
        // The two banks described this movement in two vocabularies; neither is
        // the transfer's own description, and picking one would state that the
        // sending bank's words describe both sides.
        description: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::event::provenance::ParserVersion;
    use iaam_core::ids::{AccountId, OwnerId, SourceId};
    use iaam_ingest::operation::{NormalizationContext, PARSER_VERSION, normalize};
    use time::macros::date;

    /// A recorded own-account movement, built through `normalize` rather than
    /// assembled by hand: the direction this function is being tested for is
    /// put on the leg **there**, and a literal event would be this test
    /// agreeing with itself about where the sign comes from.
    fn own_account_movement(account: AccountId, movement: Option<Movement>, day: Date) -> Event {
        normalize(
            &SubmittedOperation {
                account,
                kind: OperationKind::OwnAccountMovement {
                    movement,
                    amount_minor: 1_200_000,
                    currency: CurrencyCode::Rub,
                },
                dates: OperationDates {
                    trade: None,
                    settled: None,
                    cash_posted: Some(day),
                    paid: None,
                },
                source_time: None,
                idempotency_key: None,
                source_operation_id: None,
                source_category: None,
                owner_category: None,
                source_code: None,
                source_kind: None,
                description: None,
            },
            &NormalizationContext {
                owner: OwnerId::new_random(),
                source: SourceId::new_random(),
                parser_version: ParserVersion(PARSER_VERSION.to_owned()),
            },
        )
        .expect("an own-account movement normalises")
        .event
    }

    fn leg(
        origin: u32,
        account: AccountId,
        direction: Movement,
        amount_minor: i64,
        day: Date,
    ) -> CashLeg {
        CashLeg {
            origin: LegOrigin::Observed {
                session: ImportSessionId::new_random(),
                row: origin,
            },
            account,
            direction,
            amount_minor,
            currency: CurrencyCode::Rub,
            date: day,
            reference: None,
            import: None,
        }
    }

    #[test]
    fn two_legs_of_one_transfer_are_proposed_and_nothing_is_left_unmatched() {
        let main = AccountId::new_random();
        let everyday = AccountId::new_random();
        let proposals = propose(&[
            leg(1, main, Movement::Out, 1_200_000, date!(2025 - 03 - 15)),
            leg(2, everyday, Movement::In, 1_200_000, date!(2025 - 03 - 15)),
        ]);
        assert_eq!(proposals.candidates.len(), 1);
        assert!(proposals.candidates[0].evidence.sole_candidate);
        assert_eq!(proposals.candidates[0].evidence.days_apart, 0);
        assert!(proposals.unmatched.is_empty());
    }

    #[test]
    fn a_leg_nothing_pairs_with_is_reported_as_unmatched() {
        let main = AccountId::new_random();
        let everyday = AccountId::new_random();
        let proposals = propose(&[
            leg(1, main, Movement::Out, 1_200_000, date!(2025 - 03 - 15)),
            leg(2, everyday, Movement::In, 999, date!(2025 - 03 - 15)),
        ]);
        assert!(proposals.candidates.is_empty());
        assert_eq!(proposals.unmatched.len(), 2);
    }

    #[test]
    fn a_leg_with_two_counterparts_says_it_is_not_the_only_candidate() {
        let main = AccountId::new_random();
        let everyday = AccountId::new_random();
        let reserve = AccountId::new_random();
        let proposals = propose(&[
            leg(1, main, Movement::Out, 1_200_000, date!(2025 - 03 - 15)),
            leg(2, everyday, Movement::In, 1_200_000, date!(2025 - 03 - 15)),
            leg(3, reserve, Movement::In, 1_200_000, date!(2025 - 03 - 16)),
        ]);
        assert_eq!(proposals.candidates.len(), 2);
        assert!(
            proposals
                .candidates
                .iter()
                .all(|candidate| !candidate.evidence.sole_candidate),
            "both candidates share one outgoing leg and cannot both be true"
        );
    }

    #[test]
    fn a_movement_on_one_account_is_never_paired_with_itself() {
        let main = AccountId::new_random();
        let proposals = propose(&[
            leg(1, main, Movement::Out, 1_200_000, date!(2025 - 03 - 15)),
            leg(2, main, Movement::In, 1_200_000, date!(2025 - 03 - 15)),
        ]);
        assert!(proposals.candidates.is_empty());
        assert_eq!(proposals.unmatched.len(), 2);
    }

    #[test]
    fn legs_posted_further_apart_than_the_window_are_not_proposed() {
        let main = AccountId::new_random();
        let everyday = AccountId::new_random();
        let proposals = propose(&[
            leg(1, main, Movement::Out, 1_200_000, date!(2025 - 03 - 15)),
            leg(2, everyday, Movement::In, 1_200_000, date!(2025 - 03 - 19)),
        ]);
        assert!(proposals.candidates.is_empty());
    }

    #[test]
    fn two_own_account_movements_are_offered_to_the_matcher_as_two_legs() {
        // `iaam-9ck1`: a source that asserts the far side is the owner's and
        // names no account settles both rows without a question, and the two
        // facts were then related by nothing for good. Each posts one signed
        // leg, and the sign is the direction.
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let out = own_account_movement(main, Some(Movement::Out), date!(2025 - 03 - 15));
        let into = own_account_movement(savings, Some(Movement::In), date!(2025 - 03 - 15));
        let legs: Vec<CashLeg> = [&out, &into].into_iter().filter_map(leg_of_event).collect();
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].direction, Movement::Out);
        assert_eq!(legs[1].direction, Movement::In);
        let proposals = propose(&legs);
        assert_eq!(proposals.candidates.len(), 1);
        assert!(proposals.unmatched.is_empty());
    }

    #[test]
    fn a_movement_whose_direction_nobody_stated_is_not_offered_as_a_leg() {
        // It posts no leg, and a leg needs a side. Choosing one here would be
        // the guess the shape exists to refuse; what it wants is the direction.
        let main = AccountId::new_random();
        let event = own_account_movement(main, None, date!(2025 - 03 - 15));
        assert!(matches!(
            event.kind,
            EventKind::UnresolvedOwnAccountMovement { .. }
        ));
        assert!(leg_of_event(&event).is_none());
    }

    #[test]
    fn two_currencies_of_one_amount_are_two_movements() {
        let main = AccountId::new_random();
        let everyday = AccountId::new_random();
        let mut incoming = leg(2, everyday, Movement::In, 1_200_000, date!(2025 - 03 - 15));
        incoming.currency = CurrencyCode::Usd;
        let proposals = propose(&[
            leg(1, main, Movement::Out, 1_200_000, date!(2025 - 03 - 15)),
            incoming,
        ]);
        assert!(proposals.candidates.is_empty());
    }
}
