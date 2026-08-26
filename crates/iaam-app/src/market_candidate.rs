//! Преобразование рыночных наблюдений в доменные кандидаты.
use iaam_core::valuation::{PriceCandidate, PriceOrigin, SourceExecutability};
use iaam_market::{Executability, PriceKind, PriceObservation};

/// Преобразует рыночное наблюдение в кандидата доменной оценки.
#[must_use]
pub fn candidate_from_market_observation(observation: PriceObservation) -> PriceCandidate {
    let kind = match observation.kind {
        PriceKind::Close => "close",
        PriceKind::LegalClose => "legal_close",
        PriceKind::WeightedAverage => "weighted_average",
        PriceKind::MarketPrice2 => "market_price_2",
        PriceKind::MarketPrice3 => "market_price_3",
        PriceKind::AdmittedQuote => "admitted_quote",
    };
    let executability = match observation.executability {
        Executability::Executable => SourceExecutability::Executable,
        Executability::IndicativePreviousClose => SourceExecutability::IndicativePreviousClose,
    };

    PriceCandidate {
        instrument: observation.instrument,
        price: observation.price,
        currency: observation.currency,
        trade_date: observation.trade_date.0,
        origin: PriceOrigin::Market {
            venue: observation.venue.board,
            kind: kind.to_owned(),
        },
        executability,
    }
}


#[cfg(test)]
mod tests {
    use iaam_core::money::CurrencyCode;
    use iaam_core::numeric::decimal::Dec;
    use iaam_core::valuation::{PriceOrigin, SourceExecutability};
    use iaam_market::moex::parse::parse_history;
    use iaam_market::{Executability, ObservedAt, PriceKind, PriceObservation, TradeDate, Venue};
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    use super::candidate_from_market_observation;

    const FIXTURE: &str =
        include_str!("../../../tests/fixtures/market/moex-iss-history-sber.json");

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
            executability,
        }
    }

    fn market_kind(candidate: &iaam_core::valuation::PriceCandidate) -> &str {
        match &candidate.origin {
            PriceOrigin::Market { kind, .. } => kind,
            _ => panic!("рыночное наблюдение должно стать Market-кандидатом"),
        }
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
            .map(|kind| candidate_from_market_observation(observation(
                kind,
                Executability::Executable,
            )))
            .collect();

        let names: Vec<_> = candidates.iter().map(market_kind).collect();
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

        assert_eq!(
            executable.executability,
            SourceExecutability::Executable
        );
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
                .find(|candidate| market_kind(candidate) == kind)
                .unwrap_or_else(|| panic!("нет кандидата для {kind}"));
            assert_eq!(candidate.price.inner(), price);
            assert_eq!(candidate.instrument, instrument);
            assert_eq!(candidate.currency, CurrencyCode::Rub);
            assert_eq!(
                candidate.executability,
                SourceExecutability::IndicativePreviousClose
            );
        }
        assert!(!candidates
            .iter()
            .any(|candidate| market_kind(candidate) == "admitted_quote"));
    }
}
