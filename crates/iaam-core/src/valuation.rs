//! Оценка позиций и перевод в валюту отчёта (§5.4, §6.1).
//!
//! На этапе 1 цена приходит событием `Valuation` с provenance и флагом
//! качества, а не из рыночных данных: `iaam-market` появляется в E3.
//! Схема от этого не меняется — меняется источник цены.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::ids::InstrumentId;
use crate::money::{CurrencyCode, Money};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Флаг качества оценки (§5.4). Молчаливая подстановка запрещена:
/// оценка всегда возвращает флаг.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PriceQuality {
    /// Исполнимая цена: доступный bid.
    Executable,
    /// Цена закрытия предыдущего торгового дня.
    PreviousClose,
    /// Перенос последней цены на нерабочий день.
    CarriedForward,
    /// Цена старше порога устаревания.
    Stale,
    /// Оценка владельца для неликвида.
    OwnerEstimate,
}

impl PriceQuality {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Executable => "executable",
            Self::PreviousClose => "previous_close",
            Self::CarriedForward => "carried_forward",
            Self::Stale => "stale",
            Self::OwnerEstimate => "owner_estimate",
        }
    }

    /// Оценка считается полной, только если цена исполнима или является
    /// ценой закрытия. Всё остальное помечает NAV как неполный (§5.4).
    #[must_use]
    pub const fn is_complete(self) -> bool {
        match self {
            Self::Executable | Self::PreviousClose => true,
            Self::CarriedForward | Self::Stale | Self::OwnerEstimate => false,
        }
    }
}

/// Цена за единицу инструмента на дату.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentPrice {
    pub instrument: InstrumentId,
    pub price: Dec,
    pub currency: CurrencyCode,
    pub quality: PriceQuality,
    pub as_of: Date,
}

/// Последние известные цены. Заполняется проекцией из событий `Valuation`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBoard {
    latest: BTreeMap<InstrumentId, InstrumentPrice>,
}

impl PriceBoard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Запись цены. Более ранняя оценка не затирает более позднюю:
    /// порядок применения событий задаёт `EffectiveOrder`, но событие
    /// оценки может прийти задним числом.
    pub fn record(&mut self, price: InstrumentPrice) {
        self.latest
            .entry(price.instrument)
            .and_modify(|existing| {
                if price.as_of >= existing.as_of {
                    *existing = price;
                }
            })
            .or_insert(price);
    }

    #[must_use]
    pub fn latest(&self, instrument: InstrumentId) -> Option<&InstrumentPrice> {
        self.latest.get(&instrument)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&InstrumentId, &InstrumentPrice)> {
        self.latest.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.latest.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }
}

/// Источник курса. Входит в отчёт: без источника и типа курса ставка
/// доходности не определена (§6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FxSource {
    /// Официальный курс ЦБ РФ на дату. Появится в E3.
    CbrOfficial,
    /// Курс, названный владельцем.
    OwnerSupplied,
}

impl FxSource {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CbrOfficial => "cbr_official",
            Self::OwnerSupplied => "owner_supplied",
        }
    }
}

/// Таблица курсов на даты. Неизменяемый вход ядра: добыча курсов —
/// работа оболочки, ядро только применяет их и записывает источник.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxTable {
    source: FxSource,
    rates: BTreeMap<(CurrencyCode, CurrencyCode, Date), Dec>,
}

impl FxTable {
    #[must_use]
    pub fn new(source: FxSource) -> Self {
        Self {
            source,
            rates: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_rate(
        mut self,
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
        rate: Dec,
    ) -> Self {
        self.rates.insert((from, to, date), rate);
        self
    }

    #[must_use]
    pub const fn source(&self) -> &FxSource {
        &self.source
    }

    /// Курс на дату. Единица для одинаковых валют — не подстановка, а
    /// тождество: рубль в рублях стоит рубль.
    #[must_use]
    pub fn rate(&self, from: CurrencyCode, to: CurrencyCode, date: Date) -> Option<Dec> {
        if from == to {
            return Some(Dec::one());
        }
        self.rates.get(&(from, to, date)).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValuationError {
    #[error("нет цены инструмента {instrument:?} — стоимость позиции неизвестна")]
    MissingPrice { instrument: InstrumentId },
    #[error("нет курса {from:?}→{to:?} на {date}")]
    MissingFxRate {
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
    },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

impl ValuationError {
    /// Машиночитаемый код для API (§13).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingPrice { .. } => "missing_price",
            Self::MissingFxRate { .. } => "missing_fx_rate",
            Self::Numeric(_) => "numeric",
        }
    }
}

/// Перевод проведённой суммы в валюту отчёта.
///
/// Возвращает **расчётную** величину, а не проведённую сумму: результат
/// умножения на курс не проходил ни по одному счёту (§3.4).
pub fn convert(
    amount: Money,
    to: CurrencyCode,
    date: Date,
    fx: &FxTable,
) -> Result<Dec, ValuationError> {
    let rate = fx
        .rate(amount.currency(), to, date)
        .ok_or(ValuationError::MissingFxRate {
            from: amount.currency(),
            to,
            date,
        })?;
    Ok(amount.to_calc_dec().checked_mul(rate)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::PostedMinor;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn price(day: time::Date, value: i64, quality: PriceQuality) -> InstrumentPrice {
        InstrumentPrice {
            instrument: InstrumentId::new_random(),
            price: Dec::new(Decimal::from(value)),
            currency: CurrencyCode::Rub,
            quality,
            as_of: day,
        }
    }

    #[test]
    fn a_later_price_replaces_an_earlier_one_and_an_earlier_one_does_not() {
        let instrument = InstrumentId::new_random();
        let mut board = PriceBoard::new();
        let mut early = price(date!(2026 - 01 - 05), 100, PriceQuality::PreviousClose);
        early.instrument = instrument;
        let mut late = price(date!(2026 - 02 - 05), 120, PriceQuality::Executable);
        late.instrument = instrument;

        board.record(late);
        board.record(early);
        assert_eq!(board.latest(instrument).unwrap().price, late.price);
        assert_eq!(board.len(), 1);
    }

    #[test]
    fn only_executable_and_closing_prices_count_as_complete() {
        // Молчаливая подстановка запрещена: перенесённая, устаревшая
        // и оценочная цена помечают NAV как неполный (§5.4).
        assert!(PriceQuality::Executable.is_complete());
        assert!(PriceQuality::PreviousClose.is_complete());
        assert!(!PriceQuality::CarriedForward.is_complete());
        assert!(!PriceQuality::Stale.is_complete());
        assert!(!PriceQuality::OwnerEstimate.is_complete());
    }

    #[test]
    fn the_same_currency_needs_no_rate() {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let amount = Money::new(PostedMinor::new(12_345), CurrencyCode::Rub);
        assert_eq!(
            convert(amount, CurrencyCode::Rub, date!(2026 - 03 - 01), &fx).unwrap(),
            Dec::new(Decimal::new(12_345, 2))
        );
    }

    #[test]
    fn a_missing_rate_is_an_error_not_an_assumed_one() {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Usd);
        assert!(matches!(
            convert(amount, CurrencyCode::Rub, date!(2026 - 03 - 01), &fx),
            Err(ValuationError::MissingFxRate { .. })
        ));
    }

    #[test]
    fn conversion_produces_a_calculated_value() {
        // 100,00 USD по курсу 80,5 = 8050 рублей расчётной величиной,
        // а не проведённой суммой: эта сумма ни по одному счёту не прошла.
        let fx = FxTable::new(FxSource::OwnerSupplied).with_rate(
            CurrencyCode::Usd,
            CurrencyCode::Rub,
            date!(2026 - 03 - 01),
            Dec::new(Decimal::new(805, 1)),
        );
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Usd);
        assert_eq!(
            convert(amount, CurrencyCode::Rub, date!(2026 - 03 - 01), &fx).unwrap(),
            Dec::new(Decimal::new(80_500, 1))
        );
    }
    #[test]
    fn the_board_reports_what_it_holds() {
        let mut board = PriceBoard::new();
        assert!(board.is_empty());
        assert_eq!(board.len(), 0);
        assert_eq!(board.iter().count(), 0);

        board.record(price(date!(2026 - 01 - 05), 100, PriceQuality::Executable));
        board.record(price(date!(2026 - 01 - 05), 200, PriceQuality::Executable));
        assert!(!board.is_empty());
        assert_eq!(board.len(), 2, "две разные бумаги — две цены");
        assert_eq!(board.iter().count(), 2);
    }

    #[test]
    fn every_code_is_stable() {
        // Коды уходят в API и в снапшоты отчётов: их изменение —
        // изменение публичного контракта, а не переименование.
        assert_eq!(PriceQuality::Executable.code(), "executable");
        assert_eq!(PriceQuality::PreviousClose.code(), "previous_close");
        assert_eq!(PriceQuality::CarriedForward.code(), "carried_forward");
        assert_eq!(PriceQuality::Stale.code(), "stale");
        assert_eq!(PriceQuality::OwnerEstimate.code(), "owner_estimate");
        assert_eq!(FxSource::CbrOfficial.code(), "cbr_official");
        assert_eq!(FxSource::OwnerSupplied.code(), "owner_supplied");
        assert_eq!(
            ValuationError::MissingPrice {
                instrument: InstrumentId::new_random()
            }
            .code(),
            "missing_price"
        );
        assert_eq!(
            ValuationError::MissingFxRate {
                from: CurrencyCode::Usd,
                to: CurrencyCode::Rub,
                date: date!(2026 - 01 - 01),
            }
            .code(),
            "missing_fx_rate"
        );
        assert_eq!(
            ValuationError::Numeric(NumericError::Overflow).code(),
            "numeric"
        );
    }
}
