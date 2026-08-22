//! Шесть семантических дат (§4.2).
//!
//! Одной даты недостаточно: сделка 30 декабря с расчётами 3 января
//! попадает в другой налоговый год; дивиденд имеет дату отсечки и дату
//! выплаты; налог имеет дату удержания и период, к которому относится.

use serde::{Deserialize, Serialize};
use time::Date;

/// Макрос объявления типизированной даты.
///
/// Каждая дата — отдельный тип, поэтому передать одну вместо другой
/// невозможно. Это первый слой проверки (§15.1).
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
    /// Дата заключения сделки.
    TradeDate
);
typed_date!(
    /// Дата расчётов и перехода прав.
    SettledDate
);
typed_date!(
    /// Дата движения денег по счёту.
    CashPostedDate
);
typed_date!(
    /// Дата, определяющая право на выплату (отсечка).
    EntitlementDate
);
typed_date!(
    /// Дата фактической выплаты.
    PaidDate
);

/// Налоговый период — календарный год, к которому относится событие.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaxPeriod(pub i32);

/// Набор дат события. Заполнены не все — это нормально (§4.9),
/// но схема обязана их допускать без переинтерпретации.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDates {
    pub trade: Option<TradeDate>,
    pub settled: Option<SettledDate>,
    pub cash_posted: Option<CashPostedDate>,
    pub entitlement: Option<EntitlementDate>,
    pub paid: Option<PaidDate>,
    /// Явно заданный налоговый период. Если `None` — выводится
    /// правилом [`EventDates::tax_period`].
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

    /// Дата, по которой событие попадает в отчётный период.
    ///
    /// Приоритет: расчёты → движение денег → выплата → сделка.
    /// Расчёты важнее сделки, потому что права переходят при расчётах.
    #[must_use]
    pub fn effective_date(&self) -> Option<Date> {
        self.settled
            .map(|d| d.0)
            .or_else(|| self.cash_posted.map(|d| d.0))
            .or_else(|| self.paid.map(|d| d.0))
            .or_else(|| self.trade.map(|d| d.0))
    }

    /// Налоговый период события.
    ///
    /// Сделка 30 декабря с расчётами 3 января относится к следующему году —
    /// именно поэтому одной даты недостаточно.
    #[must_use]
    pub fn tax_period(&self) -> Option<TaxPeriod> {
        self.tax_period_override
            .or_else(|| self.effective_date().map(|d| TaxPeriod(d.year())))
    }
}

/// Детерминированный порядок событий.
///
/// При одинаковой дате порядок задаёт `sequence`, а не порядок импорта —
/// иначе проекция зависела бы от того, в каком порядке загрузили файлы,
/// и инвариант детерминизма (§15.3) не выполнялся бы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectiveOrder {
    date: Date,
    sequence: u32,
}

impl EffectiveOrder {
    /// Тривиальная упаковка полей: логики, которую стоило бы вынести
    /// в отдельную функцию ради мутационного заслона, здесь нет
    /// (ср. [`crate::money::PostedMinor::new`]). Смысл типа задаёт не эта
    /// функция, а порядок объявления полей, который и проверяется тестами.
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
        // Отсечка 29.12 даёт право на выплату, но период определяет
        // фактическая выплата 12.01 — это разные годы (§4.2).
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
        // Неизвестное — None, а не подставной год (§4.9).
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
