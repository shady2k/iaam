//! Rule matching a scheduled payment to an observed fact (§7.2).

use serde::{Deserialize, Serialize};
use time::{Date, Duration};

use crate::projection::income::ReceivedPosting;
use crate::projection::ownership::Ownership;
use crate::returns::UnverifiableReason;
use crate::rules::cashflow::ScheduledPosting;

/// Matching-rule version. Storage of dated facts is versioned by
/// `PROJECTION_VERSION`; this versions the **matching** itself: window
/// width, one-sidedness, and greediness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PostingMatchVersion(pub u16);

/// Result of checking one scheduled payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Payment was due, but no fact exists.
    NotReceived,
    /// No conclusion is possible because evidence is missing.
    Unverifiable(UnverifiableReason),
    /// Payment was not due or is confirmed by a fact.
    Silent,
}

/// Second matching-rule version: entitlement is determined on the record date,
/// not the payment date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingMatchV2 {
    window_days: u16,
}

impl Default for PostingMatchV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl PostingMatchV2 {
    #[must_use]
    pub const fn new() -> Self {
        Self { window_days: 21 }
    }

    /// Version under which the rule enters the calculation pipeline.
    #[must_use]
    pub const fn version() -> PostingMatchVersion {
        PostingMatchVersion(2)
    }

    /// Whether the payment's waiting period has expired by the report date.
    ///
    /// While money can still be travelling through the depository chain,
    /// absence of a fact is not a missed payment. The period equals the window.
    #[must_use]
    pub fn is_due(&self, scheduled: &ScheduledPosting, as_of: Date) -> bool {
        // `saturating_add`: an extreme date must not crash report calculation.
        scheduled
            .date
            .saturating_add(Duration::days(i64::from(self.window_days)))
            <= as_of
    }

    /// Judge every payment, assigning each fact at most once.
    #[must_use]
    pub fn judge_all(
        &self,
        postings: &[(ScheduledPosting, Ownership)],
        facts: &[ReceivedPosting],
    ) -> Vec<Verdict> {
        let mut ordered: Vec<(usize, &(ScheduledPosting, Ownership))> =
            postings.iter().enumerate().collect();
        ordered.sort_by_key(|(_, (posting, _))| *posting);

        let mut available: Vec<&ReceivedPosting> = facts.iter().collect();
        available.sort_by_key(|fact| (fact.date, fact.event));
        let mut used = vec![false; available.len()];
        let mut fact_found = vec![false; postings.len()];
        let window = Duration::days(i64::from(self.window_days));

        // Facts are assigned across all payments before verdicts are classified,
        // including Unknown and EntitlementDateUnknown: otherwise an excluded
        // unverifiable payment would give its fact to a neighbour and hide a miss.
        for (index, (posting, _)) in ordered {
            let deadline = posting.date.saturating_add(window);
            let matched = (0..available.len()).find(|&fact_index| {
                let fact = available[fact_index];
                !used[fact_index] && fact_matches(posting, fact, deadline)
            });
            if let Some(fact_index) = matched {
                used[fact_index] = true;
                fact_found[index] = true;
            }
        }

        postings
            .iter()
            .enumerate()
            .map(|(index, (posting, ownership))| self.judge(posting, *ownership, fact_found[index]))
            .collect()
    }

    /// Classify one payment using an already-assigned fact.
    fn judge(&self, posting: &ScheduledPosting, ownership: Ownership, fact_found: bool) -> Verdict {
        if posting.entitlement.is_none() {
            return Verdict::Unverifiable(UnverifiableReason::EntitlementDateUnknown);
        }
        if ownership == Ownership::Unknown {
            return Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown);
        }
        if ownership == Ownership::NotOwned || fact_found {
            return Verdict::Silent;
        }
        // Silence is allowed only for proven absence of entitlement or a found
        // fact: uncertainty must be a defect, not an excuse for a missing issue.
        Verdict::NotReceived
    }
}

fn fact_matches(expected: &ScheduledPosting, fact: &ReceivedPosting, deadline: Date) -> bool {
    fact.kind == expected.kind && fact.date >= expected.date && fact.date <= deadline
}

/// First version of the rule.
///
/// The window is 21 calendar days. The depository chain takes about ten
/// business days: the issuer transfers to NSD within two business days, NSD
/// transfers to the broker's depository on the next business day, and the
/// depository transfers to the final owner no later than seven business days
/// after receipt (Article 8.7 of Federal Law 39-FZ, “other depositors”: the
/// same clause gives nominal holders and managers a shorter term, but the final
/// owner is not in that category). Ten business days means at least fourteen
/// calendar days, stretching to twenty-one across New Year or May holidays.
/// Hence 21.
///
/// The window uses calendar days, not business days, because the core has no
/// production calendar; adding one would introduce an external annually
/// published source. The rule is versioned so this decision can be revisited.
///
/// Applicability boundary: the densest real schedule is a monthly coupon,
/// about thirty days. Twenty-one is less than thirty, so the window cannot
/// reach the neighbouring payment, but the margin is only nine days. A more
/// frequent payment requires a new rule version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingMatchV1 {
    window_days: u16,
}

impl Default for PostingMatchV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl PostingMatchV1 {
    #[must_use]
    pub const fn new() -> Self {
        Self { window_days: 21 }
    }

    #[must_use]
    pub const fn window_days(self) -> u16 {
        self.window_days
    }

    /// Whether the payment's waiting period has expired by the report date.
    ///
    /// Money travels through the depository chain for the same 21 days that
    /// define the matching window, so the delay before raising an alert equals
    /// the window and lives here, not in reconciliation: narrowing the window
    /// without narrowing the delay would blame a security for days the rule
    /// itself considers a normal delivery period. One number, one rule version.
    ///
    /// A payment whose period is still running is not checked at all: it is not
    /// “not received”; there is not yet anything to say about it.
    #[must_use]
    pub fn is_due(&self, scheduled: &ScheduledPosting, as_of: Date) -> bool {
        // `saturating_add`: date addition panics beyond the calendar boundary,
        // but the verdict must exist for every input.
        scheduled
            .date
            .saturating_add(Duration::days(i64::from(self.window_days)))
            <= as_of
    }

    /// Scheduled payments for which no fact was found.
    ///
    /// A fact closes a payment when its kind matches and it arrives no earlier
    /// than the scheduled date and no later than that date plus the window. The
    /// window is one-sided: money arrives after the plan, not before it, so a
    /// fact before the scheduled date belongs to another payment.
    ///
    /// Matching is greedy in ascending date and **one-to-one**: a fact is
    /// consumed and cannot be reused. Otherwise a miss in a dense schedule
    /// would disappear — one coupon would close both itself and its missing
    /// neighbour.
    ///
    /// Both slices are sorted internally; facts with equal dates are ordered by
    /// `EventId`, making the order total. The result therefore does not depend
    /// on journal event order (§15.3).
    ///
    /// A payment whose waiting period has not expired by `as_of` is not checked;
    /// see [`Self::is_due`].
    #[must_use]
    pub fn unreceived(
        &self,
        scheduled: &[ScheduledPosting],
        facts: &[ReceivedPosting],
        as_of: Date,
    ) -> Vec<ScheduledPosting> {
        let mut plan = scheduled.to_vec();
        plan.sort();

        let mut available: Vec<&ReceivedPosting> = facts.iter().collect();
        available.sort_by_key(|fact| (fact.date, fact.event));

        let mut used = vec![false; available.len()];
        let mut missing = Vec::new();
        let window = Duration::days(i64::from(self.window_days));

        for expected in plan {
            // The period is still running: there is nothing to say about this
            // payment, and it consumes no fact. Otherwise an excluded payment
            // would consume its neighbour's confirmation and create a false miss.
            if !self.is_due(&expected, as_of) {
                continue;
            }
            // `saturating_add` instead of `+`: date addition panics beyond the
            // calendar boundary, but the rule must return a verdict for every
            // input rather than crash the core.
            let deadline = expected.date.saturating_add(window);
            let matched = (0..available.len()).find(|&index| {
                let fact = available[index];
                !used[index] && fact_matches(&expected, fact, deadline)
            });
            match matched {
                Some(index) => used[index] = true,
                None => missing.push(expected),
            }
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::EventId;
    use crate::money::{CurrencyCode, Money, PostedMinor};
    use crate::projection::income::ReceivedPosting;
    use crate::rules::cashflow::{PostingKind, ScheduledPosting};
    use time::Date;
    use time::macros::date;
    use uuid::Uuid;

    fn march(day: u8) -> Date {
        Date::from_calendar_date(2026, time::Month::March, day).expect("March day exists")
    }

    fn scheduled(day: u8, kind: PostingKind) -> ScheduledPosting {
        ScheduledPosting {
            date: march(day),
            kind,
            entitlement: None,
        }
    }

    fn coupon(day: u8) -> ScheduledPosting {
        scheduled(day, PostingKind::Coupon)
    }

    fn scheduled_with_entitlement(day: u8, entitlement: u8) -> ScheduledPosting {
        ScheduledPosting {
            date: march(day),
            kind: PostingKind::Coupon,
            entitlement: Some(march(entitlement)),
        }
    }

    /// A fact identifier is derived from its number, not `new_random`:
    /// the core is deterministic, and equal-date facts must have reproducible
    /// ordering from run to run.
    fn received(day: u8, kind: PostingKind, event: u128) -> ReceivedPosting {
        ReceivedPosting {
            event: EventId(Uuid::from_u128(event)),
            date: march(day),
            amount: Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
            kind,
        }
    }

    fn fact(day: u8) -> ReceivedPosting {
        received(day, PostingKind::Coupon, u128::from(day))
    }

    /// Report date by which every payment in these tests has long expired.
    /// This keeps matching tests independent of the delay; its boundary is
    /// checked by dedicated tests below.
    fn late_enough() -> Date {
        date!(2026 - 05 - 01)
    }

    fn judge_single(
        posting: ScheduledPosting,
        ownership: Ownership,
        facts: &[ReceivedPosting],
    ) -> Verdict {
        PostingMatchV2::new()
            .judge_all(&[(posting, ownership)], facts)
            .into_iter()
            .next()
            .expect("one payment must produce one verdict")
    }

    #[test]
    fn a_payment_whose_waiting_window_has_not_expired_is_not_checked_at_all() {
        // On the day after the scheduled date, money is still travelling through
        // the depository chain. Requiring a fact would accuse a healthy security
        // of a defect.
        let rule = PostingMatchV1::new();
        assert!(rule.unreceived(&[coupon(1)], &[], march(1)).is_empty());
    }

    #[test]
    fn the_waiting_window_is_exactly_the_matching_window() {
        // Boundary: at +20 days the period is still running; at +21 it has
        // expired, and +22 is even later. The delay equals the window precisely
        // so narrowing the window also narrows the delay.
        let rule = PostingMatchV1::new();
        assert!(rule.unreceived(&[coupon(1)], &[], march(21)).is_empty());
        assert_eq!(
            rule.unreceived(&[coupon(1)], &[], march(22)),
            vec![coupon(1)]
        );
        assert_eq!(
            rule.unreceived(&[coupon(1)], &[], march(23)),
            vec![coupon(1)]
        );
    }

    #[test]
    fn a_payment_whose_waiting_window_has_not_expired_never_consumes_a_fact() {
        // The payment is excluded entirely, not deferred “until better facts”:
        // otherwise it would consume a neighbouring payment's fact and create a
        // miss where none exists.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(1), coupon(10)], &[fact(2)], march(22))
                .is_empty()
        );
    }

    #[test]
    fn the_window_is_twenty_one_days() {
        assert_eq!(PostingMatchV1::new().window_days(), 21);
    }

    #[test]
    fn a_payment_inside_the_window_is_received() {
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(15)], &[fact(18)], late_enough())
                .is_empty()
        );
    }

    #[test]
    fn a_fact_on_the_scheduled_day_closes_it() {
        // Inclusive lower boundary: money arriving on the scheduled day is plan
        // fulfilment, not another payment.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(15)], &[fact(15)], late_enough())
                .is_empty()
        );
    }

    #[test]
    fn the_window_edge_is_inclusive_and_the_day_after_is_not() {
        // Twenty-one calendar days are the ten business days of the depository
        // chain (Article 8.7 of Federal Law 39-FZ), stretched by holidays.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(&[coupon(1)], &[fact(22)], late_enough())
                .is_empty()
        );
        assert_eq!(
            rule.unreceived(&[coupon(1)], &[fact(23)], late_enough()),
            vec![coupon(1)]
        );
    }

    #[test]
    fn money_never_arrives_before_the_schedule_says_it_should() {
        // The window is one-sided. A fact before the scheduled date is another
        // payment, not an early arrival of this one.
        let rule = PostingMatchV1::new();
        assert_eq!(
            rule.unreceived(&[coupon(15)], &[fact(14)], late_enough()),
            vec![coupon(15)]
        );
    }

    #[test]
    fn a_coupon_fact_does_not_confirm_a_principal_return() {
        let rule = PostingMatchV1::new();
        let principal = scheduled(15, PostingKind::PrincipalReturn);
        assert_eq!(
            rule.unreceived(&[principal], &[fact(18)], late_enough()),
            vec![principal]
        );
    }

    #[test]
    fn a_principal_return_fact_does_not_confirm_a_coupon() {
        let rule = PostingMatchV1::new();
        let principal_fact = received(18, PostingKind::PrincipalReturn, 18);
        assert_eq!(
            rule.unreceived(&[coupon(15)], &[principal_fact], late_enough()),
            vec![coupon(15)]
        );
    }

    #[test]
    fn an_offer_settlement_is_confirmed_only_by_its_own_kind() {
        let rule = PostingMatchV1::new();
        let offer = scheduled(15, PostingKind::OfferSettlement);
        assert_eq!(
            rule.unreceived(&[offer], &[fact(18)], late_enough()),
            vec![offer]
        );
        assert!(
            rule.unreceived(
                &[offer],
                &[received(18, PostingKind::OfferSettlement, 18)],
                late_enough()
            )
            .is_empty()
        );
    }

    #[test]
    fn one_fact_cannot_close_two_scheduled_payments() {
        // Otherwise a miss in a dense schedule would disappear: one coupon
        // would close both itself and its neighbour.
        let rule = PostingMatchV1::new();
        assert_eq!(
            rule.unreceived(&[coupon(1), coupon(10)], &[fact(11)], late_enough()),
            vec![coupon(10)]
        );
    }

    #[test]
    fn two_facts_close_two_scheduled_payments() {
        // Exactly one fact is consumed per payment: the second fact must go to
        // the second payment rather than disappear with the first.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(
                &[coupon(1), coupon(10)],
                &[fact(11), fact(12)],
                late_enough()
            )
            .is_empty()
        );
    }

    #[test]
    fn the_earliest_scheduled_payment_takes_the_earliest_fact() {
        // Greedy ascending order: the early fact goes to the early payment, so
        // the later payment keeps the later fact rather than becoming missing.
        let rule = PostingMatchV1::new();
        assert!(
            rule.unreceived(
                &[coupon(1), coupon(20)],
                &[fact(2), fact(21)],
                late_enough()
            )
            .is_empty()
        );
    }

    #[test]
    fn the_verdict_does_not_depend_on_the_order_of_the_inputs() {
        let rule = PostingMatchV1::new();
        let forward = rule.unreceived(
            &[coupon(1), coupon(10)],
            &[fact(2), fact(11)],
            late_enough(),
        );
        let reversed = rule.unreceived(
            &[coupon(10), coupon(1)],
            &[fact(11), fact(2)],
            late_enough(),
        );
        assert_eq!(forward, reversed);
        assert!(forward.is_empty());
    }

    #[test]
    fn facts_of_the_same_day_are_ordered_by_event_id() {
        // Two same-day, same-kind facts are distinguishable only by identifier.
        // Their order is completed by it, so the verdict does not depend on
        // journal event order (§15.3).
        let rule = PostingMatchV1::new();
        let first = received(11, PostingKind::Coupon, 1);
        let second = received(11, PostingKind::Coupon, 2);
        let forward = rule.unreceived(&[coupon(1), coupon(10)], &[first, second], late_enough());
        let reversed = rule.unreceived(&[coupon(10), coupon(1)], &[second, first], late_enough());
        assert_eq!(forward, reversed);
        assert!(forward.is_empty());
    }

    #[test]
    fn without_facts_every_scheduled_payment_is_unreceived() {
        let rule = PostingMatchV1::new();
        assert_eq!(
            rule.unreceived(&[coupon(10), coupon(1)], &[], late_enough()),
            vec![coupon(1), coupon(10)]
        );
    }

    #[test]
    fn without_a_schedule_there_is_nothing_to_confirm() {
        let rule = PostingMatchV1::new();
        assert!(rule.unreceived(&[], &[fact(11)], late_enough()).is_empty());
    }

    #[test]
    fn a_repeated_scheduled_payment_needs_a_second_fact() {
        // One day can carry two payments of the same kind (coupons for two
        // periods shifted by weekends): one fact closes exactly one.
        let rule = PostingMatchV1::new();
        let twice = [coupon(10), coupon(10)];
        assert_eq!(
            rule.unreceived(&twice, &[fact(11)], late_enough()),
            vec![coupon(10)]
        );
        assert!(
            rule.unreceived(&twice, &[fact(11), fact(12)], late_enough())
                .is_empty()
        );
    }

    #[test]
    fn the_window_is_measured_across_a_month_boundary() {
        // The window is calendar-based, so month-end is not a boundary.
        let rule = PostingMatchV1::new();
        let scheduled_in_march = ScheduledPosting {
            date: date!(2026 - 03 - 25),
            kind: PostingKind::Coupon,
            entitlement: None,
        };
        let fact_in_april = ReceivedPosting {
            event: EventId(Uuid::from_u128(100)),
            date: date!(2026 - 04 - 15),
            amount: Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
            kind: PostingKind::Coupon,
        };
        assert!(
            rule.unreceived(&[scheduled_in_march], &[fact_in_april], late_enough())
                .is_empty()
        );
    }
    #[test]
    fn posting_match_v2_has_version_two() {
        // The version records the new rule separately: adding the later lot
        // traversal must not silently change an already issued V1.
        assert_eq!(PostingMatchV2::version(), PostingMatchVersion(2));
    }

    #[test]
    fn known_entitlement_owned_with_fact_is_silent() {
        // A known record date and ownership on it, confirmed by a fact, mean
        // the payment is demonstrably not a problem.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::Owned, &[fact(18)]),
            Verdict::Silent
        );
    }

    #[test]
    fn known_entitlement_owned_without_fact_is_not_received() {
        // With proven entitlement, no matching fact is a proven missed payment,
        // not unverifiable ownership.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::Owned, &[]),
            Verdict::NotReceived
        );
    }

    #[test]
    fn known_entitlement_not_owned_is_silent() {
        // Proven absence of the security on the record date means the payment
        // was not due; missing a fact is not a defect.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::NotOwned, &[]),
            Verdict::Silent
        );
    }

    #[test]
    fn known_entitlement_unknown_ownership_is_unverifiable() {
        // Unknown ownership cannot establish whether the payment was due, so
        // silence would excuse ignorance.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::Unknown, &[]),
            Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown)
        );
    }

    #[test]
    fn unknown_entitlement_date_is_unverifiable_for_any_ownership() {
        // Without a record date there is no day on which to check ownership,
        // so entitlement date takes precedence over every other input fact.
        let posting = coupon(15);
        assert_eq!(
            judge_single(posting, Ownership::NotOwned, &[fact(18)]),
            Verdict::Unverifiable(UnverifiableReason::EntitlementDateUnknown)
        );
    }

    #[test]
    fn silence_is_only_for_proven_absence_of_entitlement() {
        // Silence means “the payment was not due”. Any uncertainty must emerge
        // as an unverifiable defect; otherwise ignorance becomes an excuse.
        let posting = scheduled_with_entitlement(15, 10);
        assert_eq!(
            judge_single(posting, Ownership::NotOwned, &[]),
            Verdict::Silent
        );
        assert_eq!(
            judge_single(posting, Ownership::Unknown, &[]),
            Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown)
        );
    }
    #[test]
    fn one_fact_closes_only_the_first_of_overlapping_postings() {
        // With overlapping windows, one fact must close only the first payment:
        // otherwise a real miss in a dense schedule disappears.
        let postings = [
            (scheduled_with_entitlement(1, 1), Ownership::Owned),
            (scheduled_with_entitlement(10, 10), Ownership::Owned),
        ];
        assert_eq!(
            PostingMatchV2::new().judge_all(&postings, &[fact(11)]),
            vec![Verdict::Silent, Verdict::NotReceived]
        );
    }

    #[test]
    fn an_unverifiable_posting_still_consumes_its_matching_fact() {
        // An unverifiable payment must not be removed before assignment: its fact
        // must not go to a neighbour and hide that neighbour's real miss.
        let postings = [
            (scheduled_with_entitlement(1, 1), Ownership::Unknown),
            (scheduled_with_entitlement(10, 10), Ownership::Owned),
        ];
        assert_eq!(
            PostingMatchV2::new().judge_all(&postings, &[fact(11)]),
            vec![
                Verdict::Unverifiable(UnverifiableReason::OwnershipUnknown),
                Verdict::NotReceived,
            ]
        );
    }
}
