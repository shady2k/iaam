//! Settlement knowledge for events that change quantity.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Date, Duration};

use crate::dates::EventDates;
use crate::event::provenance::ParserVersion;

/// What is known about an event's actual settlement date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementKnowledge {
    /// The source reported the settlement date.
    Exact(Date),
    /// The trade date is known; settlement occurred somewhere within the band.
    Bounded { earliest: Date, latest: Date },
    /// The source date's meaning is unproven: settlement may have happened at any time.
    Unbounded,
}

/// Whether the event had taken effect by a date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Yes,
    No,
    Maybe,
}

impl SettlementKnowledge {
    /// The interval is closed at both ends: there is no intraday time, so
    /// settlement exactly on `latest` is possible. `Exact(d)` is the same
    /// degenerate interval `[d, d]`, so the answer on `d` itself is `Maybe`.
    #[must_use]
    pub fn applied_before(&self, day: Date) -> Applied {
        match self {
            Self::Exact(date) => Self::bounded(*date, *date, day),
            Self::Bounded { earliest, latest } => Self::bounded(*earliest, *latest, day),
            Self::Unbounded => Applied::Maybe,
        }
    }

    fn bounded(earliest: Date, latest: Date, day: Date) -> Applied {
        if latest < day {
            Applied::Yes
        } else if day < earliest {
            Applied::No
        } else {
            Applied::Maybe
        }
    }
}

/// Version of the settlement-lag table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SettlementLagPolicyVersion(pub u32);

/// Maximum settlement delay for a source profile.
///
/// Days are calendar days, not business days: the core has no production calendar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementLagPolicy {
    version: SettlementLagPolicyVersion,
    /// Table v1 is intentionally empty: no profile yet has a written
    /// justification for an upper delay bound. Adding a profile requires that
    /// justification, not an observation of habits such as “usually T+1”.
    max_calendar_days: BTreeMap<ParserVersion, u32>,
}

impl SettlementLagPolicy {
    pub const VERSION: SettlementLagPolicyVersion = SettlementLagPolicyVersion(1);

    #[must_use]
    pub fn new(version: SettlementLagPolicyVersion) -> Self {
        Self {
            version,
            max_calendar_days: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn v1() -> Self {
        Self::new(Self::VERSION)
    }

    #[must_use]
    pub const fn version(&self) -> SettlementLagPolicyVersion {
        self.version
    }

    /// Add a proven calendar band for one parser version.
    #[must_use]
    pub fn with_profile(mut self, profile: ParserVersion, max_calendar_days: u32) -> Self {
        self.max_calendar_days.insert(profile, max_calendar_days);
        self
    }

    /// Derive settlement knowledge from event dates and parser profile.
    #[must_use]
    pub fn knowledge(
        &self,
        dates: &EventDates,
        parser_version: &ParserVersion,
    ) -> SettlementKnowledge {
        if let Some(settled) = dates.settled {
            return SettlementKnowledge::Exact(settled.0);
        }

        let Some(trade) = dates.trade else {
            return SettlementKnowledge::Unbounded;
        };
        let Some(max_days) = self.max_calendar_days.get(parser_version) else {
            return SettlementKnowledge::Unbounded;
        };
        let Some(latest) = trade.0.checked_add(Duration::days(i64::from(*max_days))) else {
            return SettlementKnowledge::Unbounded;
        };
        SettlementKnowledge::Bounded {
            earliest: trade.0,
            latest,
        }
    }
}

impl Default for SettlementLagPolicy {
    fn default() -> Self {
        Self::v1()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{SettledDate, TradeDate};

    #[test]
    fn exact_settlement_uses_a_closed_calendar_boundary() {
        // On the settlement date itself, the event cannot be assigned to the
        // start of the day without a time; that would discard real uncertainty.
        let date = time::macros::date!(2026 - 03 - 10);
        let knowledge = SettlementKnowledge::Exact(date);
        assert_eq!(
            knowledge.applied_before(date.previous_day().unwrap()),
            Applied::No
        );
        assert_eq!(knowledge.applied_before(date), Applied::Maybe);
        assert_eq!(
            knowledge.applied_before(date.next_day().unwrap()),
            Applied::Yes
        );
    }

    #[test]
    fn policy_prefers_exact_settlement_and_keeps_unknown_profiles_unbounded() {
        // The same date field in different profiles does not permit carrying
        // one source's proven band over to another.
        let trade = time::macros::date!(2026 - 03 - 10);
        let exact = EventDates::for_trade(
            TradeDate(trade),
            Some(SettledDate(time::macros::date!(2026 - 03 - 11))),
        );
        let profile = ParserVersion("broker/1".to_owned());
        let policy = SettlementLagPolicy::default().with_profile(profile.clone(), 2);
        assert_eq!(
            policy.knowledge(&exact, &profile),
            SettlementKnowledge::Exact(time::macros::date!(2026 - 03 - 11))
        );

        let without_settled = EventDates::for_trade(TradeDate(trade), None);
        assert_eq!(
            policy.knowledge(&without_settled, &ParserVersion("other/1".to_owned())),
            SettlementKnowledge::Unbounded
        );
        assert_eq!(
            policy.knowledge(&without_settled, &profile),
            SettlementKnowledge::Bounded {
                earliest: trade,
                latest: time::macros::date!(2026 - 03 - 12),
            }
        );
    }
}
