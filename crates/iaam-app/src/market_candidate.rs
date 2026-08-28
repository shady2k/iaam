//! Преобразование рыночных наблюдений в доменные кандидаты.
use crate::error::AppError;
use iaam_core::bond::{AccrualPeriod, PrincipalReturn};
use iaam_core::money::{CurrencyCode, PerUnitAmount};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::{
    PriceCandidate, PriceKind as CorePriceKind, PriceOrigin, SourceExecutability,
};
use iaam_market::moex::parse::reconcile_quotation_basis;
use iaam_market::{Executability, PriceKind, PriceObservation};
use iaam_store::schedule::StoredSnapshot;
use rust_decimal::Decimal;
use time::Date;
use time::format_description::well_known::Iso8601;

/// Преобразует рыночное наблюдение в кандидата доменной оценки.
#[must_use]
pub fn candidate_from_market_observation(observation: PriceObservation) -> PriceCandidate {
    let kind = match observation.kind {
        PriceKind::Close => CorePriceKind::Close,
        PriceKind::LegalClose => CorePriceKind::LegalClose,
        PriceKind::WeightedAverage => CorePriceKind::WeightedAverage,
        PriceKind::MarketPrice2 => CorePriceKind::MarketPrice2,
        PriceKind::MarketPrice3 => CorePriceKind::MarketPrice3,
        PriceKind::AdmittedQuote => CorePriceKind::AdmittedQuote,
    };
    let executability = match observation.executability {
        Executability::Executable => SourceExecutability::Executable,
        Executability::IndicativePreviousClose => SourceExecutability::IndicativePreviousClose,
    };
    let (basis, basis_evidence_contradicts) =
        reconcile_quotation_basis(observation.basis, &observation.basis_evidence);
    PriceCandidate {
        instrument: observation.instrument,
        price: observation.price,
        currency: observation.currency,
        basis,
        basis_evidence: observation.basis_evidence,
        basis_evidence_contradicts,
        trade_date: observation.trade_date.0,
        observed_at: Some(observation.observed_at.0),
        origin: PriceOrigin::Market {
            venue: observation.venue,
            kind,
        },
        executability,
    }
}

/// Преобразует строки снимка графика в доменные купонные периоды.
pub fn accrual_periods_from_snapshot(
    snapshot: &StoredSnapshot,
) -> Result<Vec<AccrualPeriod>, AppError> {
    snapshot
        .coupon_periods
        .iter()
        .map(|row| {
            let period_start = Date::parse(&row.period_start, &Iso8601::DEFAULT)
                .map_err(|error| AppError::Store(error.to_string()))?;
            let accrual_end = Date::parse(&row.accrual_end, &Iso8601::DEFAULT)
                .map_err(|error| AppError::Store(error.to_string()))?;
            let payment_date = Date::parse(&row.payment_date, &Iso8601::DEFAULT)
                .map_err(|error| AppError::Store(error.to_string()))?;

            let coupon_per_unit = match row.amount_status.as_str() {
                "amount_fixed" => {
                    let amount_per_unit = row.amount_per_unit.as_deref().ok_or_else(|| {
                        AppError::Store(
                            "известная сумма купона не содержит amount_per_unit".to_owned(),
                        )
                    })?;
                    let amount_currency = row.amount_currency.as_deref().ok_or_else(|| {
                        AppError::Store(
                            "известная сумма купона не содержит amount_currency".to_owned(),
                        )
                    })?;
                    let amount = Decimal::from_str_exact(amount_per_unit)
                        .map_err(|error| AppError::Store(error.to_string()))?;
                    let currency = CurrencyCode::from_code(amount_currency).ok_or_else(|| {
                        AppError::Store(format!("неизвестная валюта купона: {amount_currency}"))
                    })?;
                    Some(PerUnitAmount::new(Dec::new(amount), currency))
                }
                _ => None,
            };

            Ok(AccrualPeriod {
                period_start,
                accrual_end,
                payment_date,
                coupon_per_unit,
            })
        })
        .collect()
}

/// Преобразует строки снимка графика в доменные возвраты номинала.
pub fn principal_returns_from_snapshot(
    snapshot: &StoredSnapshot,
) -> Result<Vec<PrincipalReturn>, AppError> {
    snapshot
        .principal_repayments
        .iter()
        .map(|row| {
            let repayment_date = Date::parse(&row.repayment_date, &Iso8601::DEFAULT)
                .map_err(|error| AppError::Store(error.to_string()))?;
            let share_percent = Decimal::from_str_exact(&row.share_percent)
                .map_err(|error| AppError::Store(error.to_string()))?;

            Ok(PrincipalReturn {
                repayment_date,
                share_percent: Dec::new(share_percent),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use iaam_core::money::CurrencyCode;
    use iaam_core::numeric::decimal::Dec;
    use iaam_core::rules::{ValuationPolicyV1, ValuationRule};
    use iaam_core::valuation::{
        PriceKind as CorePriceKind, PriceOrigin, PriceQuery, SourceExecutability,
    };
    use iaam_market::moex::parse::{MarketSegment, parse_history};
    use iaam_market::{Executability, ObservedAt, PriceKind, PriceObservation, TradeDate, Venue};
    use iaam_store::schedule::{CouponPeriodRow, StoredSnapshot};
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    use super::{accrual_periods_from_snapshot, candidate_from_market_observation};

    #[test]
    fn a_row_without_a_fixed_amount_translates_to_none_not_zero() {
        let snapshot = StoredSnapshot {
            snapshot_id: "s1".to_owned(),
            observed_at: "2026-08-27T12:00:00Z".to_owned(),
            coupon_periods: vec![CouponPeriodRow {
                period_start: "2026-06-03".to_owned(),
                accrual_end: "2026-12-02".to_owned(),
                payment_date: "2026-12-02".to_owned(),
                record_date: None,
                amount_status: "undetermined".to_owned(),
                amount_per_unit: None,
                amount_currency: None,
                rate_percent: None,
                source_entry_id: None,
            }],
            principal_repayments: Vec::new(),
            offer_windows: Vec::new(),
        };
        let periods = accrual_periods_from_snapshot(&snapshot).unwrap();
        assert!(periods[0].coupon_per_unit.is_none());
    }

    const FIXTURE: &str = include_str!("../../../tests/fixtures/market/moex-iss-history-sber.json");

    fn observation(kind: PriceKind, executability: Executability) -> PriceObservation {
        PriceObservation {
            instrument: iaam_core::ids::InstrumentId::new_random(),
            venue: Venue {
                board: "TQBR".to_owned(),
                session: 3,
            },
            trade_date: TradeDate(date!(2026 - 08 - 03)),
            observed_at: ObservedAt(datetime!(2026 - 08 - 26 09:00:00 UTC)),
            kind,
            price: Dec::new(Decimal::new(1, 0)),
            currency: CurrencyCode::Rub,
            basis: iaam_core::valuation::QuotationBasis::Unknown,
            basis_evidence: String::new(),
            executability,
        }
    }

    fn market_kind(candidate: &iaam_core::valuation::PriceCandidate) -> CorePriceKind {
        match &candidate.origin {
            PriceOrigin::Market { kind, .. } => *kind,
            _ => panic!("рыночное наблюдение должно стать Market-кандидатом"),
        }
    }
    #[test]
    fn market_candidate_preserves_the_full_venue_identity() {
        let candidate = candidate_from_market_observation(observation(
            PriceKind::Close,
            Executability::Executable,
        ));
        let PriceOrigin::Market { venue, .. } = candidate.origin else {
            panic!("рыночное наблюдение должно стать Market-кандидатом");
        };
        assert_eq!(venue.board, "TQBR");
        assert_eq!(venue.session, 3);
    }

    #[test]
    fn maps_all_price_kinds_to_distinguishable_canonical_names() {
        let kinds = [
            PriceKind::Close,
            PriceKind::LegalClose,
            PriceKind::WeightedAverage,
            PriceKind::MarketPrice2,
            PriceKind::MarketPrice3,
            PriceKind::AdmittedQuote,
        ];
        let candidates: Vec<_> = kinds
            .into_iter()
            .map(|kind| {
                candidate_from_market_observation(observation(kind, Executability::Executable))
            })
            .collect();

        let names: Vec<_> = candidates
            .iter()
            .map(|candidate| market_kind(candidate).as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "close",
                "legal_close",
                "weighted_average",
                "market_price_2",
                "market_price_3",
                "admitted_quote",
            ]
        );
        assert_eq!(
            candidates
                .iter()
                .filter_map(|candidate| match &candidate.origin {
                    PriceOrigin::Market { kind, .. } => Some(kind),
                    _ => None,
                })
                .collect::<std::collections::HashSet<_>>()
                .len(),
            6
        );
    }

    #[test]
    fn maps_both_source_executability_variants_totally() {
        let executable = candidate_from_market_observation(observation(
            PriceKind::Close,
            Executability::Executable,
        ));
        let indicative = candidate_from_market_observation(observation(
            PriceKind::Close,
            Executability::IndicativePreviousClose,
        ));

        assert_eq!(executable.executability, SourceExecutability::Executable);
        assert_eq!(
            indicative.executability,
            SourceExecutability::IndicativePreviousClose
        );
    }

    #[test]
    fn moex_history_row_becomes_candidates_for_each_non_null_price() {
        let instrument = iaam_core::ids::InstrumentId::new_random();
        let observations = parse_history(
            FIXTURE,
            instrument,
            ObservedAt(datetime!(2026 - 08 - 26 09:00:00 UTC)),
            MarketSegment {
                engine: "stock",
                market: "shares",
            },
        )
        .expect("разбор фикстуры");
        let candidates: Vec<_> = observations
            .into_iter()
            .filter(|observation| observation.trade_date == TradeDate(date!(2026 - 08 - 03)))
            .map(candidate_from_market_observation)
            .collect();

        assert_eq!(candidates.len(), 5);
        for (kind, price) in [
            ("close", Decimal::new(28139, 2)),
            ("legal_close", Decimal::new(28015, 2)),
            ("weighted_average", Decimal::new(27978, 2)),
            ("market_price_2", Decimal::new(28021, 2)),
            ("market_price_3", Decimal::new(28021, 2)),
        ] {
            let candidate = candidates
                .iter()
                .find(|candidate| market_kind(candidate).as_str() == kind)
                .unwrap_or_else(|| panic!("нет кандидата для {kind}"));
            assert_eq!(candidate.price.inner(), price);
            assert_eq!(candidate.instrument, instrument);
            assert_eq!(candidate.currency, CurrencyCode::Rub);
            assert_eq!(
                candidate.executability,
                SourceExecutability::IndicativePreviousClose
            );
        }
        assert!(
            !candidates
                .iter()
                .any(|candidate| market_kind(candidate).as_str() == "admitted_quote")
        );
    }
    #[test]
    fn policy_selects_market_price2_when_fixture_legal_close_is_absent() {
        let instrument = iaam_core::ids::InstrumentId::new_random();
        let observations = parse_history(
            FIXTURE,
            instrument,
            ObservedAt(datetime!(2026 - 08 - 26 09:00:00 UTC)),
            MarketSegment {
                engine: "stock",
                market: "shares",
            },
        )
        .expect("разбор фикстуры");
        let mut candidates: Vec<_> = observations
            .into_iter()
            .filter(|observation| observation.trade_date == TradeDate(date!(2026 - 08 - 03)))
            .map(candidate_from_market_observation)
            .collect();
        candidates.retain(|candidate| {
            !matches!(
                candidate.origin,
                PriceOrigin::Market {
                    kind: CorePriceKind::LegalClose,
                    ..
                }
            )
        });

        let result = ValuationPolicyV1::default().select(
            &PriceQuery {
                instrument,
                as_of: date!(2026 - 08 - 03),
                knowledge_as_of: datetime!(2026 - 08 - 26 09:00:00 UTC),
            },
            &candidates,
        );
        let selected = result
            .selected()
            .expect("MarketPrice2 должен покрывать строку");

        assert_eq!(
            selected.provenance.price_kind.as_deref(),
            Some("market_price_2")
        );
        assert!(matches!(
            selected.candidate.origin,
            PriceOrigin::Market {
                kind: CorePriceKind::MarketPrice2,
                ..
            }
        ));
    }
}
