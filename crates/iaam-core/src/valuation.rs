//! Оценка позиций и перевод в валюту отчёта (§5.4, §6.1).
//!
//! На этапе 1 цена приходит событием `Valuation` с provenance и флагом
//! качества, а не из рыночных данных: `iaam-market` появляется в E3.
//! Схема от этого не меняется — меняется источник цены.

pub mod candidate;

pub use candidate::{
    LegacyValuationOutcome, PriceCandidate, PriceFreshness, PriceKind, PriceOrigin,
    PriceProvenance, PriceQuery, PriceSelection, SelectedPrice, SourceExecutability, Uncovered,
    UncoveredReason, candidate_from_legacy_valuation,
};
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

/// Набор наблюдений цен по инструментам и датам. Заполняется проекцией
/// из событий `Valuation`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBoard {
    prices: BTreeMap<InstrumentId, BTreeMap<Date, InstrumentPrice>>,
}

impl PriceBoard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Запись цены. Несколько наблюдений одного инструмента сохраняются
    /// по своим датам: более раннее наблюдение не должно исчезать при
    /// появлении более позднего.
    pub fn record(&mut self, price: InstrumentPrice) {
        self.prices
            .entry(price.instrument)
            .or_default()
            .insert(price.as_of, price);
    }

    /// Цена инструмента на указанную дату или последнее наблюдение до неё.
    #[must_use]
    pub fn price_at_or_before(
        &self,
        instrument: InstrumentId,
        as_of: Date,
    ) -> Option<&InstrumentPrice> {
        self.prices
            .get(&instrument)?
            .range(..=as_of)
            .next_back()
            .map(|(_, price)| price)
    }

    /// Все наблюдения инструмента не позже даты оценки, от новых к старым.
    #[must_use]
    pub fn observations_at_or_before(
        &self,
        instrument: InstrumentId,
        as_of: Date,
    ) -> impl DoubleEndedIterator<Item = &InstrumentPrice> {
        self.prices
            .get(&instrument)
            .into_iter()
            .flat_map(move |prices| prices.range(..=as_of).rev().map(|(_, price)| price))
    }

    /// Последнее наблюдение каждого инструмента для совместимости с
    /// потребителями, которым нужен список текущих цен.
    pub fn iter(&self) -> impl Iterator<Item = (&InstrumentId, &InstrumentPrice)> {
        self.prices.iter().filter_map(|(instrument, prices)| {
            prices
                .iter()
                .next_back()
                .map(|(_, price)| (instrument, price))
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.prices.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
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
    fn a_later_price_is_selected_and_an_earlier_one_is_retained() {
        let instrument = InstrumentId::new_random();
        let mut board = PriceBoard::new();
        let mut early = price(date!(2026 - 01 - 05), 100, PriceQuality::PreviousClose);
        early.instrument = instrument;
        let mut late = price(date!(2026 - 02 - 05), 120, PriceQuality::Executable);
        late.instrument = instrument;

        board.record(late);
        board.record(early);
        assert_eq!(
            board
                .price_at_or_before(instrument, date!(2026 - 02 - 05))
                .unwrap()
                .price,
            late.price
        );
        assert_eq!(board.len(), 1);
    }

    #[test]
    fn a_price_observed_after_the_report_date_is_not_used() {
        let instrument = InstrumentId::new_random();
        let mut board = PriceBoard::new();
        let mut early = price(date!(2025 - 12 - 31), 100, PriceQuality::PreviousClose);
        early.instrument = instrument;
        let mut late = price(date!(2026 - 08 - 01), 200, PriceQuality::Executable);
        late.instrument = instrument;

        board.record(early);
        board.record(late);

        let chosen = board
            .price_at_or_before(instrument, date!(2025 - 12 - 31))
            .expect("цена на дату");
        assert_eq!(chosen.as_of, date!(2025 - 12 - 31));
    }

    #[test]
    fn a_gap_falls_back_to_the_latest_earlier_observation() {
        let instrument = InstrumentId::new_random();
        let mut board = PriceBoard::new();
        let mut earlier = price(date!(2026 - 01 - 05), 100, PriceQuality::PreviousClose);
        earlier.instrument = instrument;
        board.record(earlier);

        let chosen = board
            .price_at_or_before(instrument, date!(2026 - 01 - 06))
            .expect("более ранняя цена");
        assert_eq!(chosen.as_of, date!(2026 - 01 - 05));
    }

    #[test]
    fn an_instrument_without_any_earlier_observation_has_no_price() {
        let instrument = InstrumentId::new_random();
        let mut board = PriceBoard::new();
        let mut later = price(date!(2026 - 01 - 05), 100, PriceQuality::PreviousClose);
        later.instrument = instrument;
        board.record(later);

        assert!(
            board
                .price_at_or_before(instrument, date!(2026 - 01 - 04))
                .is_none()
        );
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
