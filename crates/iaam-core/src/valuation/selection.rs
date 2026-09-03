//! The one answer to «what price values this instrument on this date».
//!
//! Two reports publish a price for a holding: the returns report values a
//! portfolio, and the asset snapshot says what the owner holds. Before this
//! module they picked one by different means — the returns report ran the
//! versioned policy over the journal's board **and** the market store, while
//! the snapshot read the journal's board directly — and two figures reached for
//! the same instrument on the same date could disagree with nothing to say
//! which was wrong.
//!
//! So the decision is made **here, once**, and both reports read it.
//! [`decide_price`] is the whole of it: assembling the candidates from the two
//! channels, and handing them to [`ValuationPolicyV1`]. A reader that wants a
//! price calls this; a reader that wants a price and something else besides
//! still calls this.
//!
//! **No conversion happens here.** The decision is a price in the currency the
//! source quoted, and nothing in it depends on a report currency or an FX
//! table — which is why the asset snapshot, which deliberately has neither, can
//! use it. Converting is a later step and belongs to the report that wants one
//! number.

use crate::rules::valuation::{SourcePriorityVersion, ValuationPolicyV1, ValuationRule};
use crate::valuation::{
    InstrumentPrice, LegacyValuationOutcome, PriceBoard, PriceCandidate, PriceOrigin, PriceQuality,
    PriceQuery, QuotationBasis, SelectedPrice, SourceExecutability, UncoveredReason,
    candidate_from_legacy_valuation,
};

/// The source identifier a journal price is attributed to.
///
/// Nil rather than the event's own source: the journal board records the price
/// and the date, not which document carried it, and inventing an identifier
/// would put a provenance in the report that no observation supports.
const JOURNAL_SOURCE: crate::ids::SourceId = crate::ids::SourceId(uuid::Uuid::nil());

/// What the price channels held for one instrument.
///
/// A struct rather than three arguments so that a caller cannot silently pass
/// the market slice where the board belongs, and so that adding a channel is a
/// change to one type rather than to every call site.
#[derive(Debug, Clone, Copy)]
pub struct PriceInputs<'a> {
    /// The journal's own board: prices the owner or a document stated, folded
    /// by [`PriceBoard::observe`].
    pub board: &'a PriceBoard,
    /// Observations from the market store. The shell reads them; the core never
    /// reaches for a source itself. An empty slice means «no market
    /// observations» and is not an error — coverage is the policy's answer, not
    /// the caller's.
    pub market: &'a [PriceCandidate],
    /// Origin priority table version, from the report's knowledge coordinate.
    pub source_priority: SourcePriorityVersion,
}

/// What the policy decided for one instrument on one date.
///
/// Three outcomes, because there are three: a price was chosen, an old rule had
/// already decided and may not be re-derived, or nothing covers the instrument
/// and there is a reason. A caller cannot collapse the third into a zero
/// without saying so in its own code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceDecision {
    /// The policy chose an observation, with its full rationale.
    ///
    /// Boxed for the reason the returns report boxes it: the variant otherwise
    /// sets the size of the enum on every position.
    Selected(Box<SelectedPrice>),
    /// Every journal observation carried a determination the old rule had
    /// already made. Such a price is **not** re-selected: the legacy event
    /// records the date the price was assigned to, not the date it was
    /// observed, so re-selection would launder an old carry-forward as fresh.
    LegacyDerived {
        quality: PriceQuality,
        /// The newest journal observation at or before the date, which is the
        /// figure the old determination is attached to.
        price: Option<InstrumentPrice>,
    },
    /// Nothing was chosen, and why.
    Uncovered(UncoveredReason),
}

impl PriceDecision {
    /// The chosen observation, when there is one.
    #[must_use]
    pub fn selected(&self) -> Option<&SelectedPrice> {
        match self {
            Self::Selected(selected) => Some(selected),
            Self::LegacyDerived { .. } | Self::Uncovered(_) => None,
        }
    }
}

/// Decide the price for one instrument at one valuation and knowledge
/// coordinate.
///
/// The journal board and the market store are **one candidate set**, ranked by
/// one policy: a journal price does not win because it was read first, and a
/// market price does not win because it is newer machinery. The policy's origin
/// priority decides, and it is the same object in both reports.
#[must_use]
pub fn decide_price(inputs: PriceInputs<'_>, query: &PriceQuery) -> PriceDecision {
    let defaults = ValuationPolicyV1::default();
    let policy = ValuationPolicyV1 {
        carry_forward_limit: defaults.carry_forward_limit,
        price_max_age: defaults.price_max_age,
        source_priority_version: inputs.source_priority,
    };

    let mut candidates = Vec::new();
    let mut legacy_quality = None;
    let mut newest_journal_price = None;
    for price in inputs
        .board
        .observations_at_or_before(query.instrument, query.as_of)
    {
        if newest_journal_price.is_none() {
            newest_journal_price = Some(*price);
        }
        let candidate = PriceCandidate {
            instrument: price.instrument,
            price: price.price,
            currency: price.currency,
            // §10.3: an owner's price is money per unit by definition, not
            // guesswork. Entering a percentage of face value through
            // `EventKind::Valuation` is prohibited.
            basis: QuotationBasis::MoneyPerUnit,
            basis_evidence: "journal:valuation".to_owned(),
            basis_evidence_contradicts: false,
            trade_date: price.as_of,
            observed_at: None,
            origin: PriceOrigin::ReportParsed {
                source: JOURNAL_SOURCE,
            },
            executability: SourceExecutability::Unknown,
        };
        match candidate_from_legacy_valuation(price.quality, candidate) {
            LegacyValuationOutcome::Candidate(candidate) => candidates.push(candidate),
            LegacyValuationOutcome::LegacyDerived(quality) => {
                legacy_quality.get_or_insert(quality);
            }
        }
    }

    candidates.extend(
        inputs
            .market
            .iter()
            .filter(|candidate| {
                candidate.instrument == query.instrument && candidate.trade_date <= query.as_of
            })
            .cloned(),
    );

    if candidates.is_empty() {
        return match legacy_quality {
            Some(quality) => PriceDecision::LegacyDerived {
                quality,
                price: newest_journal_price,
            },
            None => PriceDecision::Uncovered(UncoveredReason::NoObservation),
        };
    }

    let result = policy.select(query, &candidates);
    match result.selected() {
        Some(selected) => PriceDecision::Selected(Box::new(selected.clone())),
        None => PriceDecision::Uncovered(
            result
                .uncovered_reason()
                .unwrap_or(UncoveredReason::NoObservation),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::InstrumentId;
    use crate::money::CurrencyCode;
    use crate::numeric::decimal::Dec;
    use crate::valuation::{PriceKind, Venue};
    use rust_decimal::Decimal;
    use std::collections::BTreeMap;
    use time::macros::{date, datetime};
    use time::{Date, OffsetDateTime};

    const AS_OF: Date = date!(2026 - 03 - 31);
    const KNOWN_AT: OffsetDateTime = datetime!(2026 - 04 - 01 12:00 UTC);

    fn instrument() -> InstrumentId {
        InstrumentId(uuid::Uuid::from_u128(7))
    }

    fn query() -> PriceQuery {
        PriceQuery {
            instrument: instrument(),
            as_of: AS_OF,
            knowledge_as_of: KNOWN_AT,
        }
    }

    fn market_price(value: i64, trade_date: Date) -> PriceCandidate {
        PriceCandidate {
            instrument: instrument(),
            price: Dec::new(Decimal::from(value)),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::MoneyPerUnit,
            basis_evidence: "market:board".to_owned(),
            basis_evidence_contradicts: false,
            trade_date,
            observed_at: Some(datetime!(2026 - 03 - 31 18:00 UTC)),
            origin: PriceOrigin::Market {
                venue: Venue {
                    board: "MAIN".to_owned(),
                    session: 1,
                },
                kind: PriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        }
    }

    fn journal(quality: PriceQuality, value: i64, as_of: Date) -> PriceBoard {
        let mut board = PriceBoard::new();
        board.record(InstrumentPrice {
            instrument: instrument(),
            price: Dec::new(Decimal::from(value)),
            currency: CurrencyCode::Rub,
            quality,
            as_of,
        });
        board
    }

    #[test]
    fn an_empty_board_and_an_empty_market_leave_the_instrument_uncovered() {
        let board = PriceBoard::new();
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &[],
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        assert_eq!(
            decision,
            PriceDecision::Uncovered(UncoveredReason::NoObservation)
        );
    }

    #[test]
    fn a_market_observation_covers_an_instrument_the_journal_never_priced() {
        // The reason this module exists: an owner who has never entered a
        // valuation event still owns something, and the market store knows what
        // it was worth.
        let board = PriceBoard::new();
        let market = [market_price(281, AS_OF)];
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &market,
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        let selected = decision.selected().expect("a market price was selected");
        assert_eq!(selected.candidate.price, Dec::new(Decimal::from(281)));
        assert!(matches!(
            selected.candidate.origin,
            PriceOrigin::Market { .. }
        ));
    }

    #[test]
    fn a_market_observation_outranks_a_journal_price_of_the_same_day() {
        // Origin priority, not arrival order: the policy ranks `Market` above
        // `ReportParsed`, and broadening the source must not depend on which
        // channel the assembling loop read first.
        let board = journal(PriceQuality::PreviousClose, 100, AS_OF);
        let market = [market_price(281, AS_OF)];
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &market,
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        let selected = decision.selected().expect("selected");
        assert_eq!(selected.candidate.price, Dec::new(Decimal::from(281)));
    }

    #[test]
    fn a_journal_price_still_wins_where_the_market_has_nothing_that_day() {
        // A fresher journal observation beats an older market one: freshness is
        // ranked before origin, so adding the market channel cannot make a
        // report reach further back than it used to.
        let board = journal(PriceQuality::PreviousClose, 100, AS_OF);
        let market = [market_price(281, date!(2026 - 03 - 20))];
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &market,
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        let selected = decision.selected().expect("selected");
        assert_eq!(selected.candidate.price, Dec::new(Decimal::from(100)));
    }

    #[test]
    fn a_legacy_determination_is_reported_rather_than_re_derived() {
        let board = journal(PriceQuality::CarriedForward, 100, AS_OF);
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &[],
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        let PriceDecision::LegacyDerived { quality, price } = decision else {
            panic!("a carry-forward is never re-selected");
        };
        assert_eq!(quality, PriceQuality::CarriedForward);
        assert_eq!(
            price.expect("the observation it was attached to").price,
            Dec::new(Decimal::from(100))
        );
    }

    #[test]
    fn a_market_observation_displaces_a_legacy_determination() {
        // `LegacyDerived` is «nothing else was available», not a veto: a market
        // price the policy can rank is a real observation and wins.
        let board = journal(PriceQuality::Stale, 100, AS_OF);
        let market = [market_price(281, AS_OF)];
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &market,
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        assert_eq!(
            decision.selected().expect("selected").candidate.price,
            Dec::new(Decimal::from(281))
        );
    }

    #[test]
    fn an_observation_the_reader_does_not_yet_know_of_is_not_used() {
        // The knowledge coordinate, honoured by the policy: a row the market
        // store learned after the report's coordinate cannot enter it.
        let board = PriceBoard::new();
        let mut late = market_price(281, AS_OF);
        late.observed_at = Some(datetime!(2026 - 04 - 02 18:00 UTC));
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &[late],
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        assert_eq!(
            decision,
            PriceDecision::Uncovered(UncoveredReason::NoObservation)
        );
    }

    #[test]
    fn an_observation_past_the_maximum_age_says_so() {
        let board = PriceBoard::new();
        let market = [market_price(281, date!(2025 - 12 - 01))];
        let decision = decide_price(
            PriceInputs {
                board: &board,
                market: &market,
                source_priority: SourcePriorityVersion(1),
            },
            &query(),
        );
        assert_eq!(decision, PriceDecision::Uncovered(UncoveredReason::TooOld));
    }

    #[test]
    fn a_decision_needs_no_report_currency_and_no_rate() {
        // Stated as a test because it is the property the asset snapshot
        // depends on: this module's inputs are a board, a slice and a
        // coordinate, and none of them is an `FxTable`. A change that added one
        // would not compile against this call.
        let board = PriceBoard::new();
        let market = [market_price(281, AS_OF)];
        let inputs = PriceInputs {
            board: &board,
            market: &market,
            source_priority: SourcePriorityVersion(1),
        };
        let decision = decide_price(inputs, &query());
        assert_eq!(
            decision.selected().expect("selected").candidate.currency,
            CurrencyCode::Rub,
            "the decision is quoted in the source's own currency"
        );
        // And a map keyed by instrument is all a caller needs to hold them.
        let mut decisions: BTreeMap<InstrumentId, PriceDecision> = BTreeMap::new();
        decisions.insert(instrument(), decision);
        assert_eq!(decisions.len(), 1);
    }
}
