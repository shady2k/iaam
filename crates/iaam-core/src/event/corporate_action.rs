//! Corporate actions — a typed family (§4.7).
//!
//! A single universal `corporate_action` with a bag of optional fields
//! would become impossible-to-validate JSON: such a bag has no invariant that
//! distinguishes amortization from replacement. Here every variant has
//! its own fields, and the family `match` is exhaustive — a new variant must
//! break the build everywhere it is not handled (§15.1).

use serde::{Deserialize, Serialize};
use time::Date;

use crate::ids::{CustodyId, InstrumentId};
use crate::money::{Money, PerUnitAmount, Quantity};
use crate::numeric::decimal::Dec;

/// Corporate action for a security.
///
/// An exhaustive `enum` without `#[non_exhaustive]` — for the same reason as
/// [`crate::event::kind::EventKind`]: adding a variant must
/// break the build wherever matching is incomplete (§15.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CorporateAction {
    /// Amortization: outstanding face value decreases, cash is received,
    /// **the number of securities does not change** (§6.5).
    PartialRedemption {
        instrument: InstrumentId,
        /// Custody location is a fact about the payment, **not** a lot lookup key:
        /// `LotKey` intentionally does not distinguish between depositories
        /// (`projection/lots.rs`), and moving a security between them
        /// does not create a lot.
        custody: CustodyId,
        /// The quantity covered by the payment. The projection checks it,
        /// rather than scaling face value by it: a position mismatch —
        /// bad source data, not a reason to recalculate.
        quantity: Quantity,
        principal_returned_per_unit: PerUnitAmount,
        /// Cash compensation actually received by the holder.
        /// It may differ from the face value repaid — because of withheld
        /// tax, for example — and is therefore recorded separately.
        compensation: Money,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
        /// The portion of outstanding face value repaid by this event.
        ///
        /// Defaulting to `Unknown` is honest: an event recorded before this
        /// field existed really asserted nothing.
        #[serde(default)]
        basis_allocation: crate::event::allocation::BasisAllocation,
    },
    /// Final redemption: face value is repaid in full and the security
    /// is removed from the position.
    ///
    /// A separate variant rather than amortization to zero: zeroing the balance and
    /// leaving the quantity would mean retaining a position in redeemed
    /// securities, which does not exist.
    Redemption {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        principal_returned_per_unit: PerUnitAmount,
        compensation: Money,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
    },
    /// Replacement: a predecessor security is exchanged for a successor security.
    ///
    /// The fields are chosen so E5 can calculate the carryover of tax basis
    /// and holding period without guessing (§16.1). The carryover rule
    /// is stored in the fact itself: it cannot be inferred later — the terms
    /// of the replacement are set by the issuer's decision, not reference data.
    Conversion {
        predecessor: InstrumentId,
        successor: InstrumentId,
        custody: CustodyId,
        /// How many successor securities are received for one
        /// predecessor security.
        ratio: Dec,
        quantity_in: Quantity,
        quantity_out: Quantity,
        fractional: FractionalTreatment,
        /// Cash in lieu of fractions. Its tax-basis treatment —
        /// an E5 rule; part 1 only stores the compensation.
        compensation: Option<Money>,
        effective_date: Date,
        record_date: Option<Date>,
        grounds: Option<String>,
        basis_transfer: BasisTransferRule,
    },
}

impl CorporateAction {
    /// Variant name for diagnostics and guards. The same approach as
    /// in [`crate::event::kind::EventKind::discriminant`].
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::PartialRedemption { .. } => "partial_redemption",
            Self::Redemption { .. } => "redemption",
            Self::Conversion { .. } => "conversion",
        }
    }

    /// The effective date is part of the fact's identity — consequently, it is required
    /// for every variant and available without inspecting the family.
    #[must_use]
    pub const fn effective_date(&self) -> Date {
        match self {
            Self::PartialRedemption { effective_date, .. }
            | Self::Redemption { effective_date, .. }
            | Self::Conversion { effective_date, .. } => *effective_date,
        }
    }

    /// The record date. Optional for every variant: the source
    /// does not always provide it, and it cannot be invented — `None` means
    /// «not asserted», not «the same as the effective date».
    #[must_use]
    pub const fn record_date(&self) -> Option<Date> {
        match self {
            Self::PartialRedemption { record_date, .. }
            | Self::Redemption { record_date, .. }
            | Self::Conversion { record_date, .. } => *record_date,
        }
    }
}

/// How the fractional part was handled during replacement.
///
/// A separate variant for «no fraction resulted»: `None` would mean «unknown»,
/// and those are different things (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FractionalTreatment {
    /// The fraction was paid out in cash.
    CashCompensated,
    /// The fraction was rounded down without compensation.
    RoundedDown,
    /// No fraction resulted.
    NotApplicable,
}

/// The rule for carrying over tax basis and holding period during replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisTransferRule {
    /// Tax basis and holding period carry over to the successor in full.
    CarryOver,
    /// The replacement is treated as a sale and purchase: the holding period restarts.
    Restart,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), CurrencyCode::Rub)
    }

    fn sample_partial_redemption() -> CorporateAction {
        CorporateAction::PartialRedemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: Quantity(dec("100")),
            principal_returned_per_unit: per_unit("200.0000"),
            compensation: rub(2_000_000),
            effective_date: date!(2026 - 06 - 15),
            record_date: Some(date!(2026 - 06 - 13)),
            grounds: Some("решение эмитента №4".to_owned()),
            basis_allocation: crate::event::allocation::BasisAllocation::default(),
        }
    }

    fn sample_redemption() -> CorporateAction {
        CorporateAction::Redemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: Quantity(dec("100")),
            principal_returned_per_unit: per_unit("800.0000"),
            compensation: rub(8_000_000),
            effective_date: date!(2026 - 12 - 15),
            record_date: None,
            grounds: None,
        }
    }

    fn sample_conversion() -> CorporateAction {
        CorporateAction::Conversion {
            predecessor: InstrumentId::new_random(),
            successor: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            ratio: dec("1.5"),
            quantity_in: Quantity(dec("100")),
            quantity_out: Quantity(dec("150")),
            fractional: FractionalTreatment::NotApplicable,
            compensation: None,
            effective_date: date!(2026 - 09 - 01),
            record_date: Some(date!(2026 - 08 - 30)),
            grounds: None,
            basis_transfer: BasisTransferRule::CarryOver,
        }
    }

    #[test]
    fn every_corporate_action_survives_a_json_round_trip() {
        for action in [
            sample_partial_redemption(),
            sample_redemption(),
            sample_conversion(),
        ] {
            let text = serde_json::to_string(&action).unwrap();
            assert_eq!(
                serde_json::from_str::<CorporateAction>(&text).unwrap(),
                action
            );
        }
    }

    #[test]
    fn every_corporate_action_names_itself() {
        assert_eq!(
            sample_partial_redemption().discriminant(),
            "partial_redemption"
        );
        assert_eq!(sample_redemption().discriminant(), "redemption");
        assert_eq!(sample_conversion().discriminant(), "conversion");
    }

    #[test]
    fn the_effective_date_of_every_action_is_reachable_without_a_match() {
        // The effective date is part of the fact's identity — consequently, every
        // variant has one: the projection must obtain it without inspecting
        // the family anew on every call.
        assert_eq!(
            sample_partial_redemption().effective_date(),
            date!(2026 - 06 - 15)
        );
        assert_eq!(sample_redemption().effective_date(), date!(2026 - 12 - 15));
        assert_eq!(sample_conversion().effective_date(), date!(2026 - 09 - 01));
    }

    #[test]
    fn record_date_remains_available_for_entitlement_check() {
        let action = sample_conversion();
        let record_date = action
            .record_date()
            .expect("a known record date must be available");

        assert!(
            record_date < action.effective_date(),
            "record date must precede the effective date"
        );
    }

    #[test]
    fn a_fractional_treatment_survives_a_json_round_trip() {
        for treatment in [
            FractionalTreatment::CashCompensated,
            FractionalTreatment::RoundedDown,
            FractionalTreatment::NotApplicable,
        ] {
            let text = serde_json::to_string(&treatment).unwrap();
            assert_eq!(
                serde_json::from_str::<FractionalTreatment>(&text).unwrap(),
                treatment
            );
        }
    }

    #[test]
    fn a_basis_transfer_rule_survives_a_json_round_trip() {
        for rule in [BasisTransferRule::CarryOver, BasisTransferRule::Restart] {
            let text = serde_json::to_string(&rule).unwrap();
            assert_eq!(
                serde_json::from_str::<BasisTransferRule>(&text).unwrap(),
                rule
            );
        }
    }
}
