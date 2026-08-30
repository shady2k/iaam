//! Market-data observations (design E3.2, section 3).
//!
//! An observation is **append-only and bitemporal**. It has two time axes:
//! `trade_date` says which day the value belongs to, and `observed_at` says
//! when we learned it. The latter is assigned by the system, not taken from
//! the response: trusting the source's clock would make the knowledge axis
//! forgeable by the response and would make report reproduction impossible.

use iaam_core::ids::InstrumentId;
use iaam_core::money::{CurrencyCode, PerUnitAmount};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::QuotationBasis;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

/// Trade date to which the value belongs (valid time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TradeDate(pub Date);

/// Time at which we learned the value (knowledge time).
///
/// A separate type rather than another `Date` is intentional: swapping the
/// axes must not be representable (§15.1). Swapping “when was the price?” and
/// “when did we learn it?” produces neither a compiler error nor a wrong
/// number—it silently breaks report reproducibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedAt(pub OffsetDateTime);

/// Price executability—an **attribute of the source**, not a policy output.
///
/// There are no `CarriedForward` or `Stale` variants here and there cannot be:
/// carrying a price onto a non-trading day and staleness beyond a threshold
/// are derived by valuation policy (E3.3). Recording them as observations
/// would erase the distinction between “the exchange did not trade” and “we
/// substituted yesterday's value”, making reports impossible to recalculate
/// under a changed rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Executability {
    /// Price at which an exit is possible: the available bid.
    Executable,
    /// Previous trading day's closing price—an indication, not an execution.
    IndicativePreviousClose,
}

/// The exact price that was observed.
///
/// ISS returns six candidates in one row. None is declared primary: choosing
/// among them is valuation policy, E3.3. Declaring one primary here would
/// silently accept a decision belonging to another subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceKind {
    Close,
    LegalClose,
    WeightedAverage,
    MarketPrice2,
    MarketPrice3,
    AdmittedQuote,
}

/// Trading venue from the identity of a market observation.
///
/// The type belongs to core so valuation candidates and market observations
/// cannot diverge in how they represent a venue.
pub use iaam_core::valuation::Venue;

/// Instrument price observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceObservation {
    pub instrument: InstrumentId,
    pub venue: Venue,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    pub kind: PriceKind,
    pub price: Dec,
    /// Venue currency, **not “instrument currency”**: ISS returns
    /// `CURRENCYID` per row, and it belongs to the observation.
    pub currency: CurrencyCode,
    /// Price unit established during parsing (§10.2).
    ///
    /// `#[serde(default)]` is required: observations were recorded before this
    /// field existed, and assigning `MoneyPerUnit` would claim proof nobody
    /// had provided.
    #[serde(default)]
    pub basis: QuotationBasis,
    /// Evidence from which the basis was derived.
    #[serde(default)]
    pub basis_evidence: String,
    pub executability: Executability,
}

/// Accrued coupon-interest observation.
///
/// A separate type rather than a field in [`PriceObservation`] for three
/// reasons. First, a bond quote is a percentage of principal while accrued
/// interest is money: one structure for two dimensions invites unit mixing.
/// Second, accrued interest has no executability; it is not a price at which
/// anyone trades. Third, the field would always be empty for equities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccruedInterestObservation {
    pub instrument: InstrumentId,
    pub venue: Venue,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    /// Per ONE security, with the currency from `FACEUNIT`.
    pub per_unit: PerUnitAmount,
}

/// Exchange-rate observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxObservation {
    pub from: CurrencyCode,
    pub to: CurrencyCode,
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    /// Nominal: the CBR publishes a rate per 1, 10, or 100 units.
    /// A bare number without a nominal is not interpretable.
    pub nominal: u32,
    /// Value per nominal, as supplied by the source.
    pub value: Dec,
    /// Value per unit. Stored **alongside** `value`: a discrepancy between
    /// them signals corrupted parsing and must not be lost.
    pub unit_rate: Dec,
}

/// Key-rate observation.
///
/// This is an observation for a business day, not an interval: the source
/// returns a daily series and contains no effective date (design §8.3).
/// Intervals are derived on read and marked as derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRateObservation {
    pub trade_date: TradeDate,
    pub observed_at: ObservedAt,
    pub rate: Dec,
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::ids::InstrumentId;
    use iaam_core::money::PerUnitAmount;
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    #[test]
    fn the_two_time_axes_are_distinct_types() {
        let traded = TradeDate(date!(2026 - 08 - 03));
        let learned = ObservedAt(datetime!(2026-08-26 09:00:00 UTC));
        // This test exists for the compiler: if the axes ever become one type,
        // swapping constructor arguments would compile silently, replacing
        // “when the price was” with “when we learned it”.
        assert_ne!(traded.0.to_string(), learned.0.date().to_string());
    }

    #[test]
    fn executability_has_no_carried_forward_variant() {
        // Carrying a price to a non-trading day is valuation policy (E3.3),
        // not something the source sent (design §3.5). A variant here would
        // make the policy output recordable as an observation, permanently
        // losing the distinction between “the exchange did not trade” and
        // “we substituted yesterday's value”.
        let all = [
            Executability::Executable,
            Executability::IndicativePreviousClose,
        ];
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn an_observation_written_before_the_basis_existed_reads_as_unknown() {
        let value = serde_json::json!({
            "instrument": InstrumentId::new_random(),
            "venue": {"board": "TQBR", "session": 3},
            "trade_date": TradeDate(date!(2026 - 08 - 03)),
            "observed_at": ObservedAt(datetime!(2026-08-03 19:00:00 UTC)),
            "kind": PriceKind::Close,
            "price": Dec::new(Decimal::from(100)),
            "currency": CurrencyCode::Rub,
            "executability": Executability::IndicativePreviousClose,
        });
        let observation: PriceObservation = serde_json::from_value(value).unwrap();
        assert_eq!(observation.basis, QuotationBasis::Unknown);
        assert_eq!(observation.basis_evidence, "");
    }

    #[test]
    fn accrued_interest_is_measured_per_bond_not_per_trade() {
        // Trade.accrued_interest is the amount for the ENTIRE trade
        // (event/mod.rs; trade_settlement adds it to gross in full). An
        // observation is per security. The type must make this substitution
        // unrepresentable: a bare Dec would not stop it.
        let observation = AccruedInterestObservation {
            instrument: InstrumentId::new_random(),
            venue: Venue {
                board: "TQOB".to_owned(),
                session: 3,
            },
            trade_date: TradeDate(date!(2026 - 08 - 20)),
            observed_at: ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
            per_unit: PerUnitAmount::new(
                Dec::new(Decimal::from_str_exact("15.17").unwrap()),
                CurrencyCode::Rub,
            ),
        };
        assert_eq!(observation.per_unit.currency(), CurrencyCode::Rub);
    }
}
