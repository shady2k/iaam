//! Вычисление доли разнесения как версионированное правило ядра.
//!
//! Денежная арифметика живёт в `iaam-core`, потому что именно ядро
//! обязано вычислять числа в ответах API. Приёмка читает график и
//! координату знания, затем передаёт готовое значение в чистый
//! нормализатор `iaam-ingest`.

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

/// Доля возврата от остатка **до** события.
///
/// Возвраты одной даты агрегируются: две отдельные амортизации нельзя
/// применить каждую с долей от остатка на начало дня — 10% и 10% от
/// убывающей базы дают 19%, а не 20%. Источник различить их не даёт:
/// `source_entry_id` у MOEX всегда `None`.
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

    // Сравниваются суммы, а не доли: доли графика приходят в процентах
    // с точностью источника, а возврат — деньги, округлённые до
    // минимальной единицы валюты. Расхождение хотя бы в одну единицу
    // означает другой возврат или брак источника — обе причины требуют
    // отказа, а не догадки.
    //
    // Округление — половина от нуля, той же конвенцией, что и правило
    // НКД: сверяемся с числом, которое напечатал источник, а он
    // округляет так (см. `rounding_to_a_scale_matches_the_kopeck_of_the_source`).
    // Половина к чётному, как в `split_basis`, дала бы на середине
    // ложный `AmountMismatch` на честной выплате.
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

/// Отпечаток канонической выборки входов вычисления.
///
/// Покрывает ровно то, от чего зависит доля: номинал с валютой, все
/// возвраты (их даты и доли), дату события, идентичность снимка
/// источника и версию правила группировки одинаковых дат. Изменение
/// любого из них обязано менять отпечаток — иначе устаревшее evidence
/// будет выглядеть свежим.
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
        write!(&mut hex, "{byte:02x}").expect("писатель строки не может завершиться ошибкой");
    }
    AllocationInputsHash::new(hex).expect("SHA-256 всегда даёт 64 шестнадцатеричных знака")
}

/// Версия правила: агрегация возвратов одной даты, доля от остатка
/// до события, сверка суммы с точностью до минимальной единицы валюты.
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
        Dec::new(Decimal::from_str_exact(text).expect("десятичное число"))
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
                    .expect("дата возврата"),
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
            BasisAllocation::Unknown(gap) => panic!("ожидалась доля, получена {gap:?}"),
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
            panic!("ожидалось известное вычисление")
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
            panic!("ожидалось известное вычисление")
        };
        let BasisAllocation::Known {
            evidence: second_evidence,
            ..
        } = second
        else {
            panic!("ожидалось известное вычисление")
        };
        assert_ne!(
            first_evidence.inputs_hash, second_evidence.inputs_hash,
            "идентичность снимка входит в отпечаток"
        );
    }
}
