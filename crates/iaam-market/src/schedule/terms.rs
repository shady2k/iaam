//! Issue terms: two time axes and knowledge for every attribute (§2.4).
//!
//! `observed_at` answers “when did we learn it?”; `effective_from` answers
//! “from what date are these terms active?”. One axis for both questions makes
//! a report reproduce terms that did not exist on its selected date.

pub use iaam_core::bond::DefaultFlags;
use iaam_core::ids::InstrumentId;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::Date;

use crate::observation::ObservedAt;
use crate::schedule::Knowledge;

/// Issue-terms snapshot: assertions from **one** source at **one** `observed_at`.
///
/// Combining fields from different observations would create an issue that did
/// not exist at any point in time.
///
/// Current principal is intentionally absent: it is derived from initial
/// principal and the return series. Storing both would create two sources of
/// truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTerms {
    pub instrument: InstrumentId,
    pub observed_at: ObservedAt,
    /// Date from which the terms apply. MOEX does not report it.
    pub effective_from: Knowledge<Date>,
    pub maturity_date: Knowledge<Date>,
    pub initial_face_value: Knowledge<Dec>,
    /// Currency code **as named by the source**. Translation is by dictionary (§2.5).
    pub face_currency_code: Knowledge<String>,
    pub coupon_periods_per_year: Knowledge<u32>,
    /// Day-count basis. MOEX always reports `Unknown` (§2.11).
    pub day_count: Knowledge<String>,
    /// Calendar. MOEX always reports `Unknown` (§2.11).
    pub calendar: Knowledge<String>,
    pub default_flags: DefaultFlags,
}

impl IssueTerms {
    /// Whether these terms apply on `as_of`.
    ///
    /// With unknown `effective_from`, the snapshot describes terms at its
    /// observation time and does not apply to earlier dates: the previous
    /// snapshot or `unknown` governs there. This is a refusal rather than a
    /// guess.
    #[must_use]
    pub fn applies_at(&self, as_of: Date) -> bool {
        match &self.effective_from {
            Knowledge::Known(from) => as_of >= *from,
            Knowledge::Unknown => as_of >= self.observed_at.0.date(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    fn minimal() -> IssueTerms {
        IssueTerms {
            instrument: InstrumentId::new_random(),
            observed_at: ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
            effective_from: Knowledge::Unknown,
            maturity_date: Knowledge::Known(date!(2036 - 02 - 06)),
            initial_face_value: Knowledge::Known(Dec::new(Decimal::from(1000))),
            face_currency_code: Knowledge::Known("SUR".to_owned()),
            coupon_periods_per_year: Knowledge::Known(2),
            day_count: Knowledge::Unknown,
            calendar: Knowledge::Unknown,
            default_flags: DefaultFlags {
                declared: false,
                technical: false,
            },
        }
    }

    #[test]
    fn effective_from_is_a_separate_axis_from_observed_at() {
        // An issuer change effective on a future date, with one axis, would
        // either apply to the whole history or be ignored for as_of. Replacing
        // an unknown effective date with observed_at would turn a guess into a fact.
        let terms = minimal();
        assert!(matches!(terms.effective_from, Knowledge::Unknown));
        assert!(terms.applies_at(date!(2026 - 08 - 27)));
        assert!(!terms.applies_at(date!(2026 - 08 - 26)));
    }

    #[test]
    fn day_count_and_calendar_have_no_default() {
        // MOEX supplies neither, in the schedule or issue description. A
        // substituted day-count produces plausibly wrong accrued interest.
        let terms = minimal();
        assert!(terms.day_count.known().is_none());
        assert!(terms.calendar.known().is_none());
    }
}
