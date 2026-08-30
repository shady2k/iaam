//! Непогашенный остаток номинала на дату (§6.5).
//!
//! Остаток не хранится нигде: он выводится из первоначального номинала
//! и ряда возвратов. Хранить его вторым полем значило бы завести второй
//! источник истины, который разойдётся с первым молча.

use thiserror::Error;
use time::Date;

use crate::bond::BondSchedule;
use crate::bond::offer::ScheduleCompleteness;
use crate::money::PerUnitAmount;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RemainingPrincipalError {
    #[error("первоначальный номинал неизвестен")]
    Unknown,
    #[error("график не проверен: остаток из него брать нельзя")]
    ScheduleNotValidated,
    #[error("доля возврата номинала не положительна")]
    ShareNotPositive,
    #[error("доли возвратов до даты дают больше 100%")]
    PrefixAboveHundred,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Непогашенный номинал на одну бумагу на дату `on`.
///
/// Граница включающая: в день возврата остаток уже уменьшен.
///
/// Доверие к графику проверяется здесь, а не у вызывающего: раньше
/// котировка брала остаток из лота и от графика не зависела вовсе,
/// теперь зависит. Цена из графика, которому система не доверяет,
/// хуже отсутствия цены.
pub fn remaining_principal(
    schedule: &BondSchedule,
    on: Date,
) -> Result<PerUnitAmount, RemainingPrincipalError> {
    match &schedule.completeness {
        ScheduleCompleteness::Validated => {}
        ScheduleCompleteness::Incomplete { .. } | ScheduleCompleteness::Unknown => {
            return Err(RemainingPrincipalError::ScheduleNotValidated);
        }
    }

    let initial = schedule
        .initial_principal
        .ok_or(RemainingPrincipalError::Unknown)?;

    let mut repaid = Dec::zero();
    for item in &schedule.principal_returns {
        if item.repayment_date > on {
            continue;
        }
        if !item.share_percent.is_positive() {
            return Err(RemainingPrincipalError::ShareNotPositive);
        }
        repaid = repaid.checked_add(item.share_percent)?;
    }

    let hundred = Dec::new(rust_decimal::Decimal::ONE_HUNDRED);
    if repaid > hundred {
        return Err(RemainingPrincipalError::PrefixAboveHundred);
    }

    let remaining_share = hundred.checked_sub(repaid)?;
    let value = initial
        .value()
        .checked_mul(remaining_share)?
        .checked_div(hundred)?;
    Ok(PerUnitAmount::new(value, initial.currency()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::{BondSchedule, PrincipalReturn};
    use crate::money::CurrencyCode;
    use rust_decimal::Decimal;
    use time::macros::{date, format_description};

    fn dec(text: &str) -> Dec {
        Dec::new(text.parse::<Decimal>().expect("десятичное число"))
    }

    fn rub(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            dec(text),
            CurrencyCode::from_code("RUB").expect("код валюты"),
        )
    }

    fn schedule(returns: &[(&str, &str)]) -> BondSchedule {
        BondSchedule {
            initial_principal: Some(rub("1000")),
            principal_returns: returns
                .iter()
                .map(|(day, share)| PrincipalReturn {
                    repayment_date: Date::parse(day, format_description!("[year]-[month]-[day]"))
                        .expect("дата"),
                    share_percent: dec(share),
                })
                .collect(),
            completeness: ScheduleCompleteness::Validated,
            ..Default::default()
        }
    }

    #[test]
    fn the_remainder_is_the_initial_principal_before_any_repayment() {
        let schedule = schedule(&[("2026-06-01", "30")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 05 - 31)).unwrap(),
            rub("1000")
        );
    }

    #[test]
    fn the_repayment_date_itself_already_reduces_the_remainder() {
        let schedule = schedule(&[("2026-06-01", "30")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap(),
            rub("700")
        );
    }

    #[test]
    fn repayments_accumulate() {
        let schedule = schedule(&[("2026-06-01", "30"), ("2026-07-01", "20")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 07 - 01)).unwrap(),
            rub("500")
        );
    }

    #[test]
    fn a_fully_repaid_issue_leaves_a_zero_remainder() {
        let schedule = schedule(&[("2026-06-01", "100")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap(),
            rub("0")
        );
    }

    #[test]
    fn an_untrusted_schedule_gives_no_remainder_even_when_the_arithmetic_works() {
        let mut schedule = schedule(&[("2026-06-01", "30")]);
        schedule.completeness = ScheduleCompleteness::Unknown;
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap_err(),
            RemainingPrincipalError::ScheduleNotValidated
        );
    }

    #[test]
    fn a_missing_initial_principal_is_unknown_and_never_zero() {
        let mut schedule = schedule(&[]);
        schedule.initial_principal = None;
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap_err(),
            RemainingPrincipalError::Unknown
        );
    }

    #[test]
    fn a_negative_share_is_named_and_not_silently_added() {
        let schedule = schedule(&[("2026-06-01", "-10")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 06 - 01)).unwrap_err(),
            RemainingPrincipalError::ShareNotPositive
        );
    }

    #[test]
    fn a_prefix_above_one_hundred_percent_is_rejected() {
        let schedule = schedule(&[("2026-06-01", "60"), ("2026-07-01", "60")]);
        assert_eq!(
            remaining_principal(&schedule, date!(2026 - 07 - 01)).unwrap_err(),
            RemainingPrincipalError::PrefixAboveHundred
        );
    }
}
