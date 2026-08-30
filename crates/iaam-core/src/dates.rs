//! Six semantic dates (§4.2).
//!
//! One date is not enough: a trade on 30 December settling on 3 January
//! falls in a different tax year; a dividend has an entitlement date and a
//! payment date; a tax has a withholding date and the period it belongs to.

use serde::{Deserialize, Serialize};
use time::Date;

/// Macro for declaring a typed date.
///
/// Each date is a separate type, so one cannot be passed in place of another.
/// This is the first layer of validation (§15.1).
macro_rules! typed_date {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Date);

        impl $name {
            #[must_use]
            pub const fn inner(&self) -> Date {
                self.0
            }
        }
    };
}

typed_date!(
    /// Date on which the trade was concluded.
    TradeDate
);
typed_date!(
    /// Settlement date and date on which rights transfer.
    SettledDate
);
typed_date!(
    /// Date when money moves on the account.
    CashPostedDate
);
typed_date!(
    /// Date determining entitlement to a payment (the record date).
    EntitlementDate
);
typed_date!(
    /// Date of the actual payment.
    PaidDate
);

/// Tax period—the calendar year to which the event belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaxPeriod(pub i32);

/// Set of event dates. Not all are populated—that is normal (§4.9),
/// but the schema must allow that without reinterpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDates {
    pub trade: Option<TradeDate>,
    pub settled: Option<SettledDate>,
    pub cash_posted: Option<CashPostedDate>,
    pub entitlement: Option<EntitlementDate>,
    pub paid: Option<PaidDate>,
    /// Explicitly supplied tax period. If `None`, it is derived by
    /// [`EventDates::tax_period`].
    pub tax_period_override: Option<TaxPeriod>,
}

impl EventDates {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            trade: None,
            settled: None,
            cash_posted: None,
            entitlement: None,
            paid: None,
            tax_period_override: None,
        }
    }

    #[must_use]
    pub const fn for_trade(trade: TradeDate, settled: Option<SettledDate>) -> Self {
        Self {
            trade: Some(trade),
            settled,
            ..Self::empty()
        }
    }

    #[must_use]
    pub const fn for_cash(posted: CashPostedDate) -> Self {
        Self {
            cash_posted: Some(posted),
            ..Self::empty()
        }
    }

    /// Date by which the event enters the reporting period.
    ///
    /// Priority: settlement → cash movement → payment → trade.
    /// Settlement takes precedence over trade because rights transfer at settlement.
    #[must_use]
    pub fn effective_date(&self) -> Option<Date> {
        self.settled
            .map(|d| d.0)
            .or_else(|| self.cash_posted.map(|d| d.0))
            .or_else(|| self.paid.map(|d| d.0))
            .or_else(|| self.trade.map(|d| d.0))
    }

    /// Event tax period.
    ///
    /// A trade on 30 December settling on 3 January belongs to the next year—
    /// this is why one date is not enough.
    #[must_use]
    pub fn tax_period(&self) -> Option<TaxPeriod> {
        self.tax_period_override
            .or_else(|| self.effective_date().map(|d| TaxPeriod(d.year())))
    }
}

/// Deterministic event order.
///
/// When dates are equal, `sequence`, not import order, determines the order—
/// otherwise the projection would depend on the order in which files were
/// loaded and the determinism invariant (§15.3) would fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectiveOrder {
    date: Date,
    sequence: u32,
}

impl EffectiveOrder {
    /// Trivial field packing: there is no logic here worth extracting into a
    /// separate function for the mutation guard (see
    /// [`crate::money::PostedMinor::new`]). The type's meaning comes from the
    /// field declaration order, which the tests check.
    #[must_use]
    pub const fn new(date: Date, sequence: u32) -> Self {
        Self { date, sequence }
    }

    #[must_use]
    pub const fn date(&self) -> Date {
        self.date
    }

    #[must_use]
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn tax_period_follows_settlement_not_trade() {
        let dates = EventDates::for_trade(
            TradeDate(date!(2025 - 12 - 30)),
            Some(SettledDate(date!(2026 - 01 - 03))),
        );
        assert_eq!(dates.tax_period(), Some(TaxPeriod(2026)));
    }

    #[test]
    fn tax_period_falls_back_to_trade_when_settlement_unknown() {
        let dates = EventDates::for_trade(TradeDate(date!(2025 - 12 - 30)), None);
        assert_eq!(dates.tax_period(), Some(TaxPeriod(2025)));
    }

    #[test]
    fn cash_movement_alone_defines_the_period() {
        let dates = EventDates::for_cash(CashPostedDate(date!(2026 - 02 - 14)));
        assert_eq!(dates.effective_date(), Some(date!(2026 - 02 - 14)));
        assert_eq!(dates.tax_period(), Some(TaxPeriod(2026)));
    }

    #[test]
    fn payout_date_defines_the_period_not_the_record_date() {
        // An entitlement date of 29 December grants the right to payment, but
        // the actual payment on 12 January determines the period; these are
        // different years (§4.2).
        let dates = EventDates {
            entitlement: Some(EntitlementDate(date!(2025 - 12 - 29))),
            paid: Some(PaidDate(date!(2026 - 01 - 12))),
            ..EventDates::empty()
        };
        assert_eq!(dates.effective_date(), Some(date!(2026 - 01 - 12)));
        assert_eq!(dates.tax_period(), Some(TaxPeriod(2026)));
    }

    #[test]
    fn event_without_dates_has_no_period() {
        // Unknown means `None`, not a placeholder year (§4.9).
        assert_eq!(EventDates::empty().effective_date(), None);
        assert_eq!(EventDates::empty().tax_period(), None);
    }

    #[test]
    fn explicit_tax_period_wins_over_the_derived_one() {
        let dates = EventDates {
            tax_period_override: Some(TaxPeriod(2024)),
            ..EventDates::for_trade(TradeDate(date!(2025 - 12 - 30)), None)
        };
        assert_eq!(dates.tax_period(), Some(TaxPeriod(2024)));
    }

    #[test]
    fn typed_dates_keep_the_calendar_date_they_wrap() {
        let day = date!(2026 - 05 - 20);
        assert_eq!(TradeDate(day).inner(), day);
        assert_eq!(SettledDate(day).inner(), day);
        assert_eq!(CashPostedDate(day).inner(), day);
        assert_eq!(EntitlementDate(day).inner(), day);
        assert_eq!(PaidDate(day).inner(), day);
    }

    #[test]
    fn effective_order_exposes_its_parts() {
        let order = EffectiveOrder::new(date!(2026 - 03 - 01), 7);
        assert_eq!(order.date(), date!(2026 - 03 - 01));
        assert_eq!(order.sequence(), 7);
    }

    #[test]
    fn effective_order_is_total_for_same_date() {
        let a = EffectiveOrder::new(date!(2026 - 03 - 01), 0);
        let b = EffectiveOrder::new(date!(2026 - 03 - 01), 1);
        assert!(a < b);
        assert_ne!(a, b);
    }

    #[test]
    fn effective_order_sorts_by_date_first() {
        let earlier_high_seq = EffectiveOrder::new(date!(2026 - 03 - 01), 99);
        let later_low_seq = EffectiveOrder::new(date!(2026 - 03 - 02), 0);
        assert!(earlier_high_seq < later_low_seq);
    }
}
