//! Domain types for an instrument's payment schedule, needed for calculation
//! (§7 of plan E3.4.4).
//!
//! This is NOT a mirror of `iaam_market::schedule`. The core does not depend
//! on the workspace crate (§3.2), and the accrued-interest rule is policy that
//! must live here beside `ValuationPolicyV1`. `iaam-app` translates the source
//! snapshot into these types, and does so **structurally**: any condition in
//! that layer is evidence that a rule leaked out of the core.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::instrument::CurrencyRoles;
use crate::money::PerUnitAmount;
use crate::numeric::decimal::Dec;

pub mod finality;
pub mod offer;
pub mod posting;
pub mod principal;
pub use offer::{
    OfferRight, OfferWindowError, OfferWindowId, OfferWindowTerms, ScheduleCompleteness,
};
pub use principal::{RemainingPrincipalError, remaining_principal};

/// Declared issue default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultFlags {
    pub declared: bool,
    pub technical: bool,
}

/// Coupon period: accrual and payment are different dates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccrualPeriod {
    pub period_start: Date,
    /// Accrual end. Accrued interest is calculated through this date.
    pub accrual_end: Date,
    /// Payment date. Moved when it falls on a weekend.
    pub payment_date: Date,
    /// Record date — it determines WHO is paid.
    ///
    /// `None` means “the source did not report it.” Substituting the payment
    /// date is forbidden: the gap between them is not constant (0–5 days in
    /// fixtures), and in 157 of 275 cases it is one day — exactly the days
    /// when a trade changes the answer.
    pub record_date: Option<Date>,
    /// Coupon amount for the period per security.
    ///
    /// `None` means the amount is undetermined (a floater or future period).
    /// Zero would mean a security that pays nothing.
    pub coupon_per_unit: Option<PerUnitAmount>,
}

/// Return of part of the principal.
///
/// A share, not an amount: the amount depends on the remainder, and the
/// remainder is derived from the initial principal and the sequence of returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalReturn {
    pub repayment_date: Date,
    /// Share of the INITIAL principal, in percent.
    pub share_percent: Dec,
}

/// An instrument's payment schedule at a knowledge coordinate.
///
/// This is a compact domain input to the core, not a mirror of the source
/// structure: the application layer translates the snapshot into it.
///
/// `Default` is needed for test literals using `..Default::default()`.
/// Production code must not call `BondSchedule::default()`: an empty schedule
/// with completeness `Unknown` means an unknown source, not an absent schedule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BondSchedule {
    pub periods: Vec<AccrualPeriod>,
    pub principal_returns: Vec<PrincipalReturn>,
    /// Initial principal per security.
    ///
    /// `None` means the source did not report it or the security is not debt.
    /// Substituting zero is forbidden (§4.9): “zero principal” and “unknown
    /// principal” require different actions from the owner.
    ///
    /// Current principal is intentionally absent: the remainder is derived
    /// from the initial value and the sequence of returns, and a second source
    /// of truth would silently diverge from the first.
    #[serde(default)]
    pub initial_principal: Option<PerUnitAmount>,
    #[serde(default)]
    pub offer_windows: Vec<offer::OfferWindowTerms>,
    #[serde(default)]
    pub completeness: offer::ScheduleCompleteness,
    #[serde(default)]
    pub default_flags: Option<DefaultFlags>,
    #[serde(default)]
    pub currency_roles: Option<CurrencyRoles>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn an_accrual_period_keeps_accrual_end_and_payment_date_apart() {
        // Accrued interest is calculated through accrual_end; the nearest
        // payment is determined by payment_date. Moving a weekend shifts the
        // latter, not the former.
        let period = AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 03),
            record_date: None,
            coupon_per_unit: None,
        };
        assert_ne!(period.accrual_end, period.payment_date);
    }

    #[test]
    fn an_undetermined_coupon_is_absent_not_zero() {
        // A zero coupon would mean a security that pays nothing,
        // and would understate accrued interest and all §7.1 metrics.
        let period = AccrualPeriod {
            period_start: date!(2026 - 06 - 03),
            accrual_end: date!(2026 - 12 - 02),
            payment_date: date!(2026 - 12 - 02),
            record_date: None,
            coupon_per_unit: None,
        };
        assert!(period.coupon_per_unit.is_none());
    }

    #[test]
    fn bond_schedule_carries_typed_offer_and_quality_inputs() {
        let instrument = crate::ids::InstrumentId::new_random();
        let window = offer::OfferWindowId::derive(instrument, date!(2026 - 12 - 01));
        let schedule = BondSchedule {
            periods: Vec::new(),
            principal_returns: Vec::new(),
            initial_principal: None,
            offer_windows: vec![offer::OfferWindowTerms {
                window,
                right: offer::OfferRight::HolderPut,
                execution_date: date!(2026 - 12 - 01),
                submission_start: None,
                submission_end: None,
                price_percent: None,
            }],
            completeness: offer::ScheduleCompleteness::Validated,
            default_flags: Some(DefaultFlags {
                declared: false,
                technical: false,
            }),
            currency_roles: Some(CurrencyRoles::uniform(crate::money::CurrencyCode::Rub)),
        };

        assert_eq!(schedule.offer_windows[0].window, window);
        assert_eq!(
            schedule.completeness,
            offer::ScheduleCompleteness::Validated
        );
        assert!(schedule.currency_roles.is_some());
        assert!(BondSchedule::default().default_flags.is_none());
    }
}
