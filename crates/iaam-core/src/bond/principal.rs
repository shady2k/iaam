//! Remaining principal on a date (§6.5).
//!
//! The remainder is stored nowhere: it is derived from the initial principal
//! and the sequence of returns. Storing it as a second field would create a
//! second source of truth that could silently diverge from the first.

use thiserror::Error;
use time::Date;

use crate::bond::BondSchedule;
use crate::bond::offer::ScheduleCompleteness;
use crate::money::PerUnitAmount;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RemainingPrincipalError {
    #[error("initial principal is unknown")]
    Unknown,
    #[error("schedule is not validated: its remainder cannot be used")]
    ScheduleNotValidated,
    #[error("principal return share is not positive")]
    ShareNotPositive,
    #[error("returns through this date exceed 100%")]
    PrefixAboveHundred,
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Remaining principal per security as of `on`.
///
/// Inclusive boundary: on the return date, the remainder is already reduced.
///
/// Trust in the schedule is checked here rather than by the caller: quotation
/// used to take the remainder from the lot and did not depend on the schedule,
/// but now it does. A price from a schedule the system does not trust is worse
/// than no price.
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
        Dec::new(text.parse::<Decimal>().expect("decimal number"))
    }

    fn rub(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            dec(text),
            CurrencyCode::from_code("RUB").expect("currency code"),
        )
    }

    fn schedule(returns: &[(&str, &str)]) -> BondSchedule {
        BondSchedule {
            initial_principal: Some(rub("1000")),
            principal_returns: returns
                .iter()
                .map(|(day, share)| PrincipalReturn {
                    repayment_date: Date::parse(day, format_description!("[year]-[month]-[day]"))
                        .expect("date"),
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
