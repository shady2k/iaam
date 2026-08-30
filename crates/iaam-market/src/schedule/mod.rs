//! Domain types for a payment schedule (§2.1 of E3.4).
//!
//! The split follows each row's role in calculation, not source columns.
//! `CouponPeriod` supplies cashflow without moving basis; `PrincipalRepayment`
//! supplies cashflow and reduces outstanding principal; `OfferWindow` supplies
//! no cashflow at all—it supplies an option. One table with a row-kind field
//! would force every consumer to branch on kind, reintroducing the `match`
//! moved from parsing into the database dictionary (migration 0009).
//!
//! No type here interprets source codes: principal-return kind and offer-right
//! kind are stored as named by the source and translated by the dictionary at
//! the application boundary (§2.5).

pub mod completeness;
pub mod terms;

use iaam_core::ids::InstrumentId;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::Date;

use crate::observation::ObservedAt;

/// Knowledge of an attribute: known or unknown.
///
/// A separate type rather than `Option` is intentional: `Option` invites
/// `unwrap_or_default`, and a default day-count basis produces plausible but
/// wrong accrued interest that no test using whole periods will expose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Knowledge<T> {
    Known(T),
    Unknown,
}

impl<T> Knowledge<T> {
    /// Known value, if present.
    ///
    /// This exists for reading; there is no default value here and there never
    /// will be one.
    pub const fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }
}

/// What is known about payment for a coupon period (§2.3).
///
/// Zero is a present numeric value; absence is its negation. Substituting one
/// for the other understates both the resulting cashflow and YTM, plausibly.
/// Status is **not derived from dates**: a checked floating-rate issue had a
/// 2020 coupon with neither amount nor rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CouponAmount {
    /// Amount per unit of initial principal and its currency are known.
    AmountFixed {
        per_unit: Dec,
        currency: CurrencyCode,
    },
    /// Rate is known; amount is not yet determined.
    RateFixedAmountUndetermined { rate_percent: Dec },
    /// Neither is known.
    Undetermined,
}

/// Income accrual for a coupon period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouponPeriod {
    /// Period start. The issuer does not move it, unlike payment date.
    pub period_start: Date,
    /// Accrual end. Accrued interest is calculated against it.
    pub accrual_end: Date,
    /// Payment date. It moves when a weekend is shifted or the issuer revises it.
    pub payment_date: Date,
    /// Date on which entitlement is fixed. The source does not always report it.
    pub record_date: Knowledge<Date>,
    pub amount: CouponAmount,
    /// Source's own entry identifier.
    ///
    /// `Option` because MOEX has none at all (§2.11). Absence is normal, not
    /// an empty required field.
    pub source_entry_id: Option<String>,
}

/// Partial principal return on a date.
///
/// Return finality is **not stored** here: it is derived from the accumulated
/// share total (§2.1). The source may have no finality code, and a conclusion
/// recorded as an observation is forbidden by ADR-0002.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalRepayment {
    pub repayment_date: Date,
    /// Share of **initial** principal, as a percentage.
    pub share_percent: Dec,
    /// Kind as named by the source. Not interpreted here.
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Right to submit for redemption during a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferWindow {
    pub execution_date: Date,
    pub submission_start: Knowledge<Date>,
    pub submission_end: Knowledge<Date>,
    /// Redemption price as a percentage of principal.
    pub price_percent: Knowledge<Dec>,
    pub agent: Knowledge<String>,
    /// Right kind as named by the source. MOEX uses free Russian text here.
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Complete schedule snapshot for an issue—the observation unit (§2.2).
///
/// The unit is a snapshot, not a row, because a row model cannot express a
/// **disappearance**: no new version at an old coordinate is indistinguishable
/// from “the source sent no updates”, leaving a cancelled amortisation beside
/// the new schedule. The source provides no stable identifier with which to
/// repair that problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleSnapshot {
    pub instrument: InstrumentId,
    pub observed_at: ObservedAt,
    pub coupon_periods: Vec<CouponPeriod>,
    pub principal_repayments: Vec<PrincipalRepayment>,
    pub offer_windows: Vec<OfferWindow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use time::macros::date;

    #[test]
    fn a_coupon_period_keeps_accrual_end_and_payment_date_apart() {
        // Shifting payment from a weekend moves payment date, not accrual end.
        // One field for both meanings would silently lose the shift, while
        // accrued interest is calculated from accrual end.
        let period = CouponPeriod {
            period_start: date!(2026 - 02 - 15),
            accrual_end: date!(2026 - 08 - 15),
            payment_date: date!(2026 - 08 - 17),
            record_date: Knowledge::Unknown,
            amount: CouponAmount::Undetermined,
            source_entry_id: None,
        };
        assert_ne!(period.accrual_end, period.payment_date);
    }

    #[test]
    fn a_repayment_carries_a_share_not_an_amount() {
        // Amount depends on outstanding principal, which is derived from the
        // initial principal and the return series. Storing amount would create
        // a second source of truth beside that derivation.
        let repayment = PrincipalRepayment {
            repayment_date: date!(2034 - 08 - 09),
            share_percent: Dec::new(Decimal::from(25)),
            source_kind: "amortization".to_owned(),
            source_entry_id: None,
        };
        assert_eq!(repayment.share_percent, Dec::new(Decimal::from(25)));
    }

    #[test]
    fn an_offer_window_without_dates_is_unknown_not_absent() {
        // The source commonly returns windows without submission dates or a
        // price. An empty window means the terms are unknown, not that no
        // window exists.
        let window = OfferWindow {
            execution_date: date!(2027 - 08 - 26),
            submission_start: Knowledge::Unknown,
            submission_end: Knowledge::Unknown,
            price_percent: Knowledge::Unknown,
            agent: Knowledge::Unknown,
            source_kind: "Оферта".to_owned(),
            source_entry_id: None,
        };
        assert!(matches!(window.price_percent, Knowledge::Unknown));
    }
}
