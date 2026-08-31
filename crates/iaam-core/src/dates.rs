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
/// Within a day, source times are ordered before events without a source time.
/// An event the source dated to a day but not to a moment is a fact observed
/// *over* that day, so it settles after the moments that are actually known.
/// This is a chosen convention, not a derived time: §4.9 forbids inventing a
/// time for an event that has none, and this ordering is the consequence of
/// not inventing one. `sequence` remains the technical insertion tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectiveOrder {
    date: Date,
    #[serde(default)]
    source_time: Option<time::Time>,
    sequence: u32,
}

impl PartialOrd for EffectiveOrder {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EffectiveOrder {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.date
            .cmp(&other.date)
            .then_with(|| match (self.source_time, other.source_time) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

impl EffectiveOrder {
    /// Trivial field packing: there is no logic here worth extracting into a
    /// separate function for the mutation guard (see
    /// [`crate::money::PostedMinor::new`]). The type's meaning comes from the
    /// field declaration order, which the tests check.
    #[must_use]
    pub const fn new(date: Date, sequence: u32) -> Self {
        Self {
            date,
            source_time: None,
            sequence,
        }
    }

    /// Construct an order with the source's time-of-day and a technical
    /// insertion tie-break.
    #[must_use]
    pub const fn with_source_time(date: Date, source_time: time::Time, sequence: u32) -> Self {
        Self {
            date,
            source_time: Some(source_time),
            sequence,
        }
    }

    #[must_use]
    pub const fn date(&self) -> Date {
        self.date
    }

    #[must_use]
    pub const fn source_time(&self) -> Option<time::Time> {
        self.source_time
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
    #[test]
    fn timed_events_sort_by_source_time_before_sequence() {
        let day = date!(2026 - 03 - 01);
        let late =
            EffectiveOrder::with_source_time(day, time::Time::from_hms(12, 0, 0).unwrap(), 0);
        let early =
            EffectiveOrder::with_source_time(day, time::Time::from_hms(9, 0, 0).unwrap(), 99);
        assert!(early < late);
    }

    #[test]
    fn timed_events_sort_before_untimed_events() {
        let day = date!(2026 - 03 - 01);
        let timed =
            EffectiveOrder::with_source_time(day, time::Time::from_hms(23, 59, 59).unwrap(), 99);
        let untimed = EffectiveOrder::new(day, 0);
        assert!(timed < untimed);
    }

    #[test]
    fn old_effective_order_payload_defaults_to_untimed() {
        let day = date!(2026 - 03 - 01);
        let value = serde_json::to_value(EffectiveOrder::new(day, 7)).unwrap();
        let mut object = value.as_object().unwrap().clone();
        object.remove("source_time");
        let restored: EffectiveOrder = serde_json::from_value(object.into()).unwrap();
        assert_eq!(restored, EffectiveOrder::new(day, 7));
        assert_eq!(restored.source_time(), None);
    }
}
