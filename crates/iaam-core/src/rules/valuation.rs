//! Версионированная политика выбора цены (§4–5 E3.3).

use std::collections::BTreeSet;


use crate::valuation::{
    PriceCandidate, PriceFreshness, PriceOrigin, PriceProvenance, PriceQuery, PriceSelection,
    SelectedPrice, UncoveredReason,
};

/// Версия политики выбора цены.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ValuationPolicyVersion(pub u32);

/// Версия таблицы приоритетов происхождений.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourcePriorityVersion(pub u32);

/// Результат выбора: отсутствие цены является свойством выборки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceSelectionResult {
    selected: Option<SelectedPrice>,
    uncovered_reason: Option<UncoveredReason>,
}

impl PriceSelectionResult {
    #[must_use]
    pub fn selected(&self) -> Option<&SelectedPrice> {
        self.selected.as_ref()
    }

    #[must_use]
    pub const fn uncovered_reason(&self) -> Option<UncoveredReason> {
        self.uncovered_reason
    }
}

/// Доменный порт политики выбора цены.
pub trait ValuationRule: Send + Sync {
    fn version(&self) -> ValuationPolicyVersion;
    fn source_priority_version(&self) -> SourcePriorityVersion;
    fn select(&self, query: &PriceQuery, candidates: &[PriceCandidate]) -> PriceSelectionResult;
}

/// Политика выбора цены версии 1.
#[derive(Debug, Clone, Copy)]
pub struct ValuationPolicyV1 {
    pub carry_forward_limit: u16,
    pub price_max_age: u16,
    pub source_priority_version: SourcePriorityVersion,
}

impl Default for ValuationPolicyV1 {
    fn default() -> Self {
        Self {
            carry_forward_limit: 10,
            price_max_age: 30,
            source_priority_version: SourcePriorityVersion(1),
        }
    }
}

impl ValuationPolicyV1 {
    pub const VERSION: ValuationPolicyVersion = ValuationPolicyVersion(1);

    fn origin_rank(origin: &PriceOrigin, _version: SourcePriorityVersion) -> u8 {
        match origin {
            PriceOrigin::Market { .. } => 0,
            PriceOrigin::ReportParsed { .. } => 1,
            PriceOrigin::OwnerAsserted => 2,
        }
    }

    fn price_kind_rank(origin: &PriceOrigin) -> Option<u8> {
        let PriceOrigin::Market { kind, .. } = origin else {
            return None;
        };
        match kind.to_ascii_lowercase().as_str() {
            "legalclose" | "legal_close" | "legalcloseprice" => Some(0),
            "marketprice2" | "market_price2" => Some(1),
            "admittedquote" | "admitted_quote" => Some(2),
            "close" => Some(3),
            // These are deliberately retained by the source but are not
            // candidates in valuation policy v1.
            "weightedaverage" | "weighted_average" | "marketprice3" | "market_price3" => None,
            _ => None,
        }
    }

    fn selection_for(
        &self,
        query: &PriceQuery,
        candidate: &PriceCandidate,
        age: u16,
    ) -> (PriceSelection, PriceFreshness) {
        let selection = if age == 0 {
            PriceSelection::AsObserved
        } else {
            PriceSelection::CarriedForward {
                observed_on: candidate.trade_date,
                days: age,
            }
        };
        let freshness = if age <= self.carry_forward_limit {
            PriceFreshness::Fresh
        } else {
            PriceFreshness::Stale { days: age }
        };
        let _ = query;
        (selection, freshness)
    }

    fn provenance(&self, candidate: &PriceCandidate) -> PriceProvenance {
        let (price_kind, venue) = match &candidate.origin {
            PriceOrigin::Market { venue, kind } => (Some(kind.clone()), Some(venue.clone())),
            PriceOrigin::ReportParsed { .. } | PriceOrigin::OwnerAsserted => (None, None),
        };
        PriceProvenance {
            price_kind,
            origin: candidate.origin.clone(),
            venue,
            observed_at: candidate.observed_at,
            valuation_policy_version: Self::VERSION.0,
            source_priority_version: self.source_priority_version.0,
            carry_forward_limit: self.carry_forward_limit,
            price_max_age: self.price_max_age,
        }
    }
}

impl ValuationRule for ValuationPolicyV1 {
    fn version(&self) -> ValuationPolicyVersion {
        Self::VERSION
    }

    fn source_priority_version(&self) -> SourcePriorityVersion {
        self.source_priority_version
    }

    fn select(&self, query: &PriceQuery, candidates: &[PriceCandidate]) -> PriceSelectionResult {
        let mut matching = Vec::new();
        let mut too_old = false;
        for candidate in candidates {
            if candidate.instrument != query.instrument {
                continue;
            }
            if candidate.trade_date > query.as_of || candidate.observed_at > query.knowledge_as_of {
                continue;
            }
            let age = (query.as_of - candidate.trade_date).whole_days();
            if age < 0 {
                continue;
            }
            let Ok(age) = u16::try_from(age) else {
                too_old = true;
                continue;
            };
            if age > self.price_max_age {
                too_old = true;
                continue;
            }
            if matches!(candidate.origin, PriceOrigin::Market { .. })
                && Self::price_kind_rank(&candidate.origin).is_none()
            {
                continue;
            }
            matching.push((age, candidate));
        }

        if matching.is_empty() {
            return PriceSelectionResult {
                selected: None,
                uncovered_reason: Some(if too_old {
                    UncoveredReason::TooOld
                } else {
                    UncoveredReason::NoObservation
                }),
            };
        }

        let min_age = matching.iter().map(|(age, _)| *age).min().expect("not empty");
        matching.retain(|(age, _)| *age == min_age);
        let min_origin = matching
            .iter()
            .map(|(_, candidate)| Self::origin_rank(&candidate.origin, self.source_priority_version))
            .min()
            .expect("not empty");
        matching.retain(|(_, candidate)| {
            Self::origin_rank(&candidate.origin, self.source_priority_version) == min_origin
        });

        if matching.iter().all(|(_, candidate)| {
            matches!(candidate.origin, PriceOrigin::Market { .. })
        }) {
            let venues: BTreeSet<&str> = matching
                .iter()
                .filter_map(|(_, candidate)| match &candidate.origin {
                    PriceOrigin::Market { venue, .. } => Some(venue.as_str()),
                    _ => None,
                })
                .collect();
            if venues.len() > 1 {
                return PriceSelectionResult {
                    selected: None,
                    uncovered_reason: Some(UncoveredReason::AmbiguousVenue),
                };
            }
        }

        let min_kind = matching
            .iter()
            .filter_map(|(_, candidate)| Self::price_kind_rank(&candidate.origin))
            .min();
        if let Some(min_kind) = min_kind {
            matching.retain(|(_, candidate)| {
                Self::price_kind_rank(&candidate.origin) == Some(min_kind)
            });
        }

        let latest_observed_at = matching
            .iter()
            .map(|(_, candidate)| candidate.observed_at)
            .max()
            .expect("not empty");
        matching.retain(|(_, candidate)| candidate.observed_at == latest_observed_at);

        if matching.len() != 1 {
            return PriceSelectionResult {
                selected: None,
                uncovered_reason: Some(UncoveredReason::AmbiguousCandidate),
            };
        }

        let (age, candidate) = matching.pop().expect("one candidate");
        let (selection, freshness) = self.selection_for(query, candidate, age);
        PriceSelectionResult {
            selected: Some(SelectedPrice {
                candidate: candidate.clone(),
                selection,
                freshness,
                provenance: self.provenance(candidate),
            }),
            uncovered_reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    use super::*;
    use crate::ids::InstrumentId;
    use crate::money::CurrencyCode;
    use crate::numeric::decimal::Dec;

    fn policy() -> ValuationPolicyV1 {
        ValuationPolicyV1::default()
    }

    fn query(as_of: time::Date) -> PriceQuery {
        PriceQuery {
            instrument: instrument(),
            as_of,
            knowledge_as_of: datetime!(2026 - 08 - 10 12:00 UTC),
        }
    }

    fn instrument() -> InstrumentId {
        InstrumentId::new_random()
    }

    fn candidate(instrument: InstrumentId, trade_date: time::Date) -> PriceCandidate {
        PriceCandidate {
            instrument,
            price: Dec::new(Decimal::from(281)),
            currency: CurrencyCode::Rub,
            trade_date,
            observed_at: datetime!(2026 - 08 - 10 12:00 UTC),
            origin: PriceOrigin::ReportParsed {
                source: crate::ids::SourceId::new_random(),
            },
            executability: crate::valuation::SourceExecutability::Executable,
        }
    }

    fn candidate_from_origin(
        instrument: InstrumentId,
        trade_date: time::Date,
        observed_at: time::OffsetDateTime,
        origin: PriceOrigin,
    ) -> PriceCandidate {
        let mut candidate = candidate(instrument, trade_date);
        candidate.observed_at = observed_at;
        candidate.origin = origin;
        candidate
    }

    fn market_candidate(
        instrument: InstrumentId,
        venue: &str,
        kind: &str,
        trade_date: time::Date,
        observed_at: time::OffsetDateTime,
    ) -> PriceCandidate {
        candidate_from_origin(
            instrument,
            trade_date,
            observed_at,
            PriceOrigin::Market {
                venue: venue.to_owned(),
                kind: kind.to_owned(),
            },
        )
    }

    #[test]
    fn a_fresh_report_price_beats_a_stale_exchange_price() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(
            &query,
            &[
                market_candidate(
                    query.instrument,
                    "TQBR",
                    "close",
                    date!(2026 - 08 - 01),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                ),
                candidate_from_origin(
                    query.instrument,
                    date!(2026 - 08 - 09),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                    PriceOrigin::ReportParsed {
                        source: crate::ids::SourceId::new_random(),
                    },
                ),
            ],
        );
        assert!(matches!(
            out.selected().expect("цена есть").candidate.origin,
            PriceOrigin::ReportParsed { .. }
        ));
    }

    #[test]
    fn equal_age_prefers_market_then_report_then_owner() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(
            &query,
            &[
                candidate_from_origin(
                    query.instrument,
                    date!(2026 - 08 - 09),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                    PriceOrigin::OwnerAsserted,
                ),
                candidate_from_origin(
                    query.instrument,
                    date!(2026 - 08 - 09),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                    PriceOrigin::ReportParsed {
                        source: crate::ids::SourceId::new_random(),
                    },
                ),
                market_candidate(
                    query.instrument,
                    "TQBR",
                    "close",
                    date!(2026 - 08 - 09),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                ),
            ],
        );
        assert!(matches!(
            out.selected().expect("цена есть").candidate.origin,
            PriceOrigin::Market { .. }
        ));
    }

    #[test]
    fn two_venues_without_a_directory_preference_are_a_refusal_not_a_guess() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(
            &query,
            &[
                market_candidate(
                    query.instrument,
                    "TQBR",
                    "close",
                    date!(2026 - 08 - 09),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                ),
                market_candidate(
                    query.instrument,
                    "SMAL",
                    "close",
                    date!(2026 - 08 - 09),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                ),
            ],
        );
        assert!(out.selected().is_none());
        assert_eq!(out.uncovered_reason(), Some(UncoveredReason::AmbiguousVenue));
    }

    #[test]
    fn price_kind_priority_excludes_weighted_average_and_market_price3() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(
            &query,
            &[
                market_candidate(
                    query.instrument,
                    "TQBR",
                    "marketprice3",
                    date!(2026 - 08 - 10),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                ),
                market_candidate(
                    query.instrument,
                    "TQBR",
                    "weightedaverage",
                    date!(2026 - 08 - 10),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                ),
                market_candidate(
                    query.instrument,
                    "TQBR",
                    "close",
                    date!(2026 - 08 - 10),
                    datetime!(2026 - 08 - 10 12:00 UTC),
                ),
            ],
        );
        assert_eq!(
            out.selected()
                .expect("Close остаётся допустимым видом")
                .provenance
                .price_kind
                .as_deref(),
            Some("close")
        );
        let out = policy().select(
            &query,
            &[market_candidate(
                query.instrument,
                "TQBR",
                "marketprice3",
                date!(2026 - 08 - 10),
                datetime!(2026 - 08 - 10 12:00 UTC),
            )],
        );
        assert!(out.selected().is_none());
    }

    #[test]
    fn equal_candidates_are_ambiguous_instead_of_ordered_by_incidental_input() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(
            &query,
            &[
                candidate_from_origin(
                    query.instrument,
                    query.as_of,
                    datetime!(2026 - 08 - 10 12:00 UTC),
                    PriceOrigin::ReportParsed {
                        source: crate::ids::SourceId::new_random(),
                    },
                ),
                candidate_from_origin(
                    query.instrument,
                    query.as_of,
                    datetime!(2026 - 08 - 10 12:00 UTC),
                    PriceOrigin::ReportParsed {
                        source: crate::ids::SourceId::new_random(),
                    },
                ),
            ],
        );
        assert!(out.selected().is_none());
        assert_eq!(
            out.uncovered_reason(),
            Some(UncoveredReason::AmbiguousCandidate)
        );
    }

    #[test]
    fn latest_observed_version_not_after_knowledge_coordinate_wins() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(
            &query,
            &[
                candidate_from_origin(
                    query.instrument,
                    query.as_of,
                    datetime!(2026 - 08 - 10 09:00 UTC),
                    PriceOrigin::ReportParsed {
                        source: crate::ids::SourceId::new_random(),
                    },
                ),
                candidate_from_origin(
                    query.instrument,
                    query.as_of,
                    datetime!(2026 - 08 - 10 11:00 UTC),
                    PriceOrigin::ReportParsed {
                        source: crate::ids::SourceId::new_random(),
                    },
                ),
                candidate_from_origin(
                    query.instrument,
                    query.as_of,
                    datetime!(2026 - 08 - 10 13:00 UTC),
                    PriceOrigin::ReportParsed {
                        source: crate::ids::SourceId::new_random(),
                    },
                ),
            ],
        );
        assert_eq!(
            out.selected().expect("версия до knowledge_as_of").provenance.observed_at,
            datetime!(2026 - 08 - 10 11:00 UTC)
        );
    }

    #[test]
    fn an_observation_on_the_valuation_date_is_not_carried_forward() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(&query, &[candidate(query.instrument, date!(2026 - 08 - 10))]);
        let picked = out.selected().expect("цена есть");
        assert_eq!(picked.selection, PriceSelection::AsObserved);
        assert_eq!(picked.freshness, PriceFreshness::Fresh);
    }

    #[test]
    fn a_price_can_be_carried_forward_and_stale_at_the_same_time() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(&query, &[candidate(query.instrument, date!(2026 - 07 - 11))]);
        let picked = out.selected().expect("30 дней ещё в окне");
        assert_eq!(
            picked.selection,
            PriceSelection::CarriedForward {
                observed_on: date!(2026 - 07 - 11),
                days: 30
            }
        );
        assert_eq!(picked.freshness, PriceFreshness::Stale { days: 30 });
    }

    #[test]
    fn a_price_older_than_the_search_window_is_not_returned_at_all() {
        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(&query, &[candidate(query.instrument, date!(2026 - 07 - 10))]);
        assert!(out.selected().is_none());
        assert_eq!(out.uncovered_reason(), Some(UncoveredReason::TooOld));
    }
    #[test]
    fn valuation_age_boundaries_are_inclusive_and_named() {
        let cases = [
            (0, PriceSelection::AsObserved, PriceFreshness::Fresh),
            (
                1,
                PriceSelection::CarriedForward {
                    observed_on: date!(2026 - 08 - 09),
                    days: 1,
                },
                PriceFreshness::Fresh,
            ),
            (
                10,
                PriceSelection::CarriedForward {
                    observed_on: date!(2026 - 07 - 31),
                    days: 10,
                },
                PriceFreshness::Fresh,
            ),
            (
                11,
                PriceSelection::CarriedForward {
                    observed_on: date!(2026 - 07 - 30),
                    days: 11,
                },
                PriceFreshness::Stale { days: 11 },
            ),
            (
                30,
                PriceSelection::CarriedForward {
                    observed_on: date!(2026 - 07 - 11),
                    days: 30,
                },
                PriceFreshness::Stale { days: 30 },
            ),
        ];
        for (age, selection, freshness) in cases {
            let query = query(date!(2026 - 08 - 10));
            let trade_date = query.as_of - time::Duration::days(age);
            let out = policy().select(&query, &[candidate(query.instrument, trade_date)]);
            let picked = out.selected().expect("граница должна быть в окне");
            assert_eq!(picked.selection, selection);
            assert_eq!(picked.freshness, freshness);
        }

        let query = query(date!(2026 - 08 - 10));
        let out = policy().select(
            &query,
            &[candidate(query.instrument, date!(2026 - 07 - 10))],
        );
        assert_eq!(out.uncovered_reason(), Some(UncoveredReason::TooOld));
    }
}
