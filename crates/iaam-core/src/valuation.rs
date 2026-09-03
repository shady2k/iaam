//! Position valuation and conversion to the reporting currency (§5.4, §6.1).
//!
//! In stage 1, price arrives as a `Valuation` event with provenance and a
//! quality flag, not from market data: `iaam-market` appears in E3.
//! The schema does not change; only the price source changes.

pub mod candidate;

pub use candidate::{
    LegacyValuationOutcome, PriceCandidate, PriceFreshness, PriceKind, PriceOrigin,
    PriceProvenance, PriceQuery, PriceSelection, QuotationBasis, SelectedPrice,
    SourceExecutability, Uncovered, UncoveredReason, Venue, candidate_from_legacy_valuation,
};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::event::Event;
use crate::event::kind::EventKind;
use crate::ids::InstrumentId;
use crate::money::{CurrencyCode, Money};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Valuation-quality flag (§5.4). Silent substitution is forbidden:
/// valuation always returns a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PriceQuality {
    /// Executable price: available bid.
    Executable,
    /// Previous trading day's closing price.
    PreviousClose,
    /// Carry-forward of the last price to a non-trading day.
    CarriedForward,
    /// Price older than the staleness threshold.
    Stale,
    /// Owner's valuation for an illiquid asset.
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

/// Price per unit of an instrument on a date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentPrice {
    pub instrument: InstrumentId,
    pub price: Dec,
    pub currency: CurrencyCode,
    pub quality: PriceQuality,
    pub as_of: Date,
}

/// Set of price observations by instrument and date. Populated by projection
/// from `Valuation` events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBoard {
    prices: BTreeMap<InstrumentId, BTreeMap<Date, InstrumentPrice>>,
}

impl PriceBoard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a price. Multiple observations for one instrument remain under
    /// their own dates: an earlier observation must not disappear when a later
    /// one arrives.
    pub fn record(&mut self, price: InstrumentPrice) {
        self.prices
            .entry(price.instrument)
            .or_default()
            .insert(price.as_of, price);
    }

    /// Record whatever price one journal event states, and nothing when it
    /// states none.
    ///
    /// The single definition of «a journal event that carries a price».
    /// [`crate::projection::advance`] calls it while it folds, and so does the
    /// asset snapshot, which needs a board without needing a whole projection.
    /// Written twice, the two would drift, and a report would then value a
    /// holding from a price the projection had decided not to record.
    ///
    /// An event with no effective date records nothing: a price is a fact about
    /// a day, and a price without one cannot be looked up at or before
    /// anything.
    pub fn observe(&mut self, event: &Event) {
        let EventKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } = &event.kind
        else {
            return;
        };
        let Some(as_of) = event.dates.effective_date() else {
            return;
        };
        self.record(InstrumentPrice {
            instrument: *instrument,
            price: *price,
            currency: *currency,
            quality: *quality,
            as_of,
        });
    }

    /// Price for an instrument on a date, or its latest observation before it.
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

    /// All observations for an instrument no later than the valuation date,
    /// newest first.
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

    /// Latest observation for each instrument, for consumers that need a list
    /// of current prices.
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

/// Rate source. Included in the report: without a source and rate type, the
/// return rate is undefined (§6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FxSource {
    /// Official CBR rate for the date. Arrives in E3.
    CbrOfficial,
    /// Rate supplied by the owner.
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

/// Table of dated rates. Immutable core input: extracting rates is shell work;
/// the core only applies them and records the source.
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

    /// Rate on a date. One for identical currencies is not a default; it is an
    /// identity: a rouble is worth one rouble in roubles.
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
    #[error("no price for instrument {instrument:?} — position value is unknown")]
    MissingPrice { instrument: InstrumentId },
    #[error("no rate from {from:?}→{to:?} on {date}")]
    MissingFxRate {
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
    },
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

impl ValuationError {
    /// Machine-readable API code (§13).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingPrice { .. } => "missing_price",
            Self::MissingFxRate { .. } => "missing_fx_rate",
            Self::Numeric(_) => "numeric",
        }
    }
}

/// Convert a posted amount to the reporting currency.
///
/// Returns a **calculated** value, not a posted amount: multiplying by a rate
/// has not passed through any account (§3.4).
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
            .expect("price on date");
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
            .expect("earlier price");
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
        // 100.00 USD at a rate of 80.5 equals 8050 roubles as a calculated
        // value, not a posted amount: no account recorded this sum.
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
        assert_eq!(board.len(), 2, "two different securities — two prices");
        assert_eq!(board.iter().count(), 2);
    }

    #[test]
    fn every_code_is_stable() {
        // Codes are sent to the API and report snapshots: changing one changes
        // the public contract; it is not a rename.
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
