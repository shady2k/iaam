//! Computing basis allocation as a versioned core rule.
//!
//! Monetary arithmetic belongs in `iaam-core` because the core is responsible
//! for computing the numbers returned by the API. Ingest reads the schedule and
//! knowledge coordinate, then passes the ready value to the pure
//! `iaam-ingest` normaliser.

use std::cmp::Ordering;
use std::fmt::Write as _;

use crate::bond::{BondSchedule, PrincipalReturn, ScheduleCompleteness};
use crate::event::allocation::{
    AllocationAlgorithmVersion, AllocationEvidence, AllocationGap, AllocationInputsHash,
    BasisAllocation,
};
use crate::money::PerUnitAmount;
use crate::numeric::decimal::Dec;
use crate::rules::ReturnedShare;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use time::{Date, OffsetDateTime};

/// Return share from the remainder **before** the event.
///
/// Returns on one date are aggregated: two separate amortisations cannot each
/// use a share of the start-of-day remainder — 10% and 10% of a shrinking base
/// produce 19%, not 20%. The source cannot distinguish them:
/// `source_entry_id` is always `None` at MOEX.
pub fn resolve_basis_allocation(
    returned_per_unit: PerUnitAmount,
    on: Date,
    schedule: Option<&BondSchedule>,
    snapshot_id: &str,
    knowledge_as_of: OffsetDateTime,
) -> BasisAllocation {
    let Some(schedule) = schedule else {
        return BasisAllocation::Unknown(AllocationGap::ScheduleMissing);
    };
    if !matches!(schedule.completeness, ScheduleCompleteness::Validated) {
        return BasisAllocation::Unknown(AllocationGap::ScheduleNotValidated);
    }
    let Some(initial) = schedule.initial_principal else {
        return BasisAllocation::Unknown(AllocationGap::ScheduleMissing);
    };
    if initial.currency() != returned_per_unit.currency() {
        return BasisAllocation::Unknown(AllocationGap::CurrencyMismatch);
    }

    let hundred = Dec::new(Decimal::ONE_HUNDRED);
    let mut scheduled_on_date = Dec::zero();
    let mut repaid_before = Dec::zero();
    for item in &schedule.principal_returns {
        if !item.share_percent.is_positive() {
            return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
        }
        let target = match item.repayment_date.cmp(&on) {
            Ordering::Equal => &mut scheduled_on_date,
            Ordering::Less => &mut repaid_before,
            Ordering::Greater => continue,
        };
        match target.checked_add(item.share_percent) {
            Ok(sum) => *target = sum,
            Err(_) => return BasisAllocation::Unknown(AllocationGap::InvalidPrefix),
        }
    }

    if scheduled_on_date.is_zero() {
        return BasisAllocation::Unknown(AllocationGap::NoRepaymentOnDate);
    }
    let Ok(remaining_before) = hundred.checked_sub(repaid_before) else {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    };
    if !remaining_before.is_positive() {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    }

    // Compare amounts, not shares: schedule shares arrive as percentages with
    // source precision, while the return is money rounded to the currency's
    // minor unit. A discrepancy of even one unit means a different return or
    // corrupt source data — either requires refusal, not a guess.
    //
    // Rounding is half away from zero, the same convention as the accrued-
    // interest rule: compare against the number printed by the source, which
    // rounds this way (see `rounding_to_a_scale_matches_the_kopeck_of_the_source`).
    // Half to even, as in `split_basis`, would produce a false `AmountMismatch`
    // at the midpoint of an honest payment.
    let Ok(planned) = initial
        .value()
        .checked_mul(scheduled_on_date)
        .and_then(|value| value.checked_div(hundred))
    else {
        return BasisAllocation::Unknown(AllocationGap::AmountMismatch);
    };
    let Ok(planned) = planned.checked_round_to_scale(initial.currency().minor_units()) else {
        return BasisAllocation::Unknown(AllocationGap::AmountMismatch);
    };
    let Ok(returned) = returned_per_unit
        .value()
        .checked_round_to_scale(returned_per_unit.currency().minor_units())
    else {
        return BasisAllocation::Unknown(AllocationGap::AmountMismatch);
    };
    if planned != returned {
        return BasisAllocation::Unknown(AllocationGap::AmountMismatch);
    }

    let Ok(share_value) = scheduled_on_date.checked_div(remaining_before) else {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    };
    let Ok(share) = ReturnedShare::new(share_value) else {
        return BasisAllocation::Unknown(AllocationGap::InvalidPrefix);
    };

    BasisAllocation::Known {
        share,
        evidence: AllocationEvidence {
            inputs_hash: allocation_inputs_hash(
                initial,
                &schedule.principal_returns,
                on,
                snapshot_id,
            ),
            knowledge_as_of,
            algorithm_version: ALLOCATION_ALGORITHM_V1,
        },
    }
}

/// Digest of the canonical selection of calculation inputs.
///
/// Covers exactly what the share depends on: principal with currency, all
/// returns (their dates and shares), the event date, the source snapshot
/// identity, and the rule version for grouping equal dates. Any change must
/// change the digest — otherwise stale evidence would look fresh.
fn allocation_inputs_hash(
    initial: PerUnitAmount,
    returns: &[PrincipalReturn],
    on: Date,
    snapshot_id: &str,
) -> AllocationInputsHash {
    let mut hasher = Sha256::new();
    hasher.update(b"iaam/allocation-inputs/v1");
    hasher.update(initial.value().inner().to_string().as_bytes());
    hasher.update(initial.currency().code().as_bytes());
    let mut ordered: Vec<&PrincipalReturn> = returns.iter().collect();
    ordered.sort_by_key(|item| (item.repayment_date, item.share_percent));
    for item in ordered {
        hasher.update(item.repayment_date.to_string().as_bytes());
        hasher.update(item.share_percent.inner().to_string().as_bytes());
    }
    hasher.update(on.to_string().as_bytes());
    hasher.update(snapshot_id.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("string writer cannot fail");
    }
    AllocationInputsHash::new(hex).expect("SHA-256 always yields 64 hexadecimal digits")
}

/// Rule version: aggregate returns on one date, share from the pre-event
/// remainder, and compare the amount to the currency minor-unit precision.
const ALLOCATION_ALGORITHM_V1: AllocationAlgorithmVersion = AllocationAlgorithmVersion(1);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::{BondSchedule, PrincipalReturn, ScheduleCompleteness};
    use crate::event::allocation::{AllocationGap, BasisAllocation};
    use crate::money::{CurrencyCode, PerUnitAmount};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).expect("decimal number"))
    }

    fn per_unit(text: &str, currency: CurrencyCode) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), currency)
    }

    fn schedule(returns: &[(&str, &str)]) -> BondSchedule {
        BondSchedule {
            principal_returns: returns
                .iter()
                .map(|(day, share)| PrincipalReturn {
                    repayment_date: Date::parse(
                        day,
                        &time::format_description::well_known::Iso8601::DATE,
                    )
                    .expect("repayment date"),
                    share_percent: dec(share),
                })
                .collect(),
            initial_principal: Some(per_unit("1000", CurrencyCode::Rub)),
            completeness: ScheduleCompleteness::Validated,
            ..BondSchedule::default()
        }
    }

    fn amortisation_amount(
        day: Date,
        amount: &str,
        currency: CurrencyCode,
    ) -> (PerUnitAmount, Date) {
        (per_unit(amount, currency), day)
    }

    fn share_of(allocation: &BasisAllocation) -> Dec {
        match allocation {
            BasisAllocation::Known { share, .. } => share.inner(),
            BasisAllocation::Unknown(gap) => panic!("expected a share, got {gap:?}"),
        }
    }

    #[test]
    fn two_repayments_on_one_date_are_aggregated_into_one_share() {
        let schedule = schedule(&[("2026-06-01", "10"), ("2026-06-01", "10")]);
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "200", CurrencyCode::Rub);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(share_of(&allocation), dec("0.2"));
        let BasisAllocation::Known { evidence, .. } = allocation else {
            panic!("expected a known calculation")
        };
        assert_eq!(
            evidence.knowledge_as_of,
            datetime!(2026 - 08 - 30 12:00:00 UTC)
        );
        assert_eq!(evidence.algorithm_version.0, 1);
        assert_eq!(evidence.inputs_hash.as_str().len(), 64);
    }

    #[test]
    fn the_share_is_taken_from_the_remainder_before_the_event_not_from_the_original() {
        let schedule = schedule(&[("2026-01-01", "30"), ("2026-06-01", "10")]);
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "100", CurrencyCode::Rub);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(share_of(&allocation), dec("0.1428571428571428571428571429"));
    }

    #[test]
    fn an_amount_that_disagrees_with_the_schedule_is_not_trusted() {
        let schedule = schedule(&[("2026-06-01", "10")]);
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "100.01", CurrencyCode::Rub);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::AmountMismatch)
        );
    }

    #[test]
    fn a_missing_schedule_names_its_reason() {
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "100", CurrencyCode::Rub);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            None,
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::ScheduleMissing)
        );
    }

    #[test]
    fn an_unvalidated_schedule_names_its_reason() {
        let mut schedule = schedule(&[("2026-06-01", "10")]);
        schedule.completeness = ScheduleCompleteness::Unknown;
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "100", CurrencyCode::Rub);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::ScheduleNotValidated)
        );
    }

    #[test]
    fn a_date_without_a_scheduled_repayment_names_its_reason() {
        let schedule = schedule(&[("2026-06-01", "10")]);
        let (amount, day) = amortisation_amount(date!(2026 - 07 - 01), "100", CurrencyCode::Rub);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::NoRepaymentOnDate)
        );
    }

    #[test]
    fn a_currency_that_disagrees_with_the_face_value_names_its_reason() {
        let schedule = schedule(&[("2026-06-01", "10")]);
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "100", CurrencyCode::Usd);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::CurrencyMismatch)
        );
    }

    #[test]
    fn a_schedule_without_initial_principal_names_its_reason() {
        let mut schedule = schedule(&[("2026-06-01", "10")]);
        schedule.initial_principal = None;
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "100", CurrencyCode::Rub);
        let allocation = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        assert_eq!(
            allocation,
            BasisAllocation::Unknown(AllocationGap::ScheduleMissing)
        );
    }

    #[test]
    fn a_different_snapshot_changes_the_allocation_input_hash() {
        let schedule = schedule(&[("2026-06-01", "10")]);
        let (amount, day) = amortisation_amount(date!(2026 - 06 - 01), "100", CurrencyCode::Rub);
        let first = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-1",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        let second = resolve_basis_allocation(
            amount,
            day,
            Some(&schedule),
            "snapshot-2",
            datetime!(2026 - 08 - 30 12:00:00 UTC),
        );
        let BasisAllocation::Known {
            evidence: first_evidence,
            ..
        } = first
        else {
            panic!("expected a known calculation")
        };
        let BasisAllocation::Known {
            evidence: second_evidence,
            ..
        } = second
        else {
            panic!("expected a known calculation")
        };
        assert_ne!(
            first_evidence.inputs_hash, second_evidence.inputs_hash,
            "snapshot identity is part of the digest"
        );
    }
}
