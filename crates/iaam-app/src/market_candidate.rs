//! Conversion of market observations into domain candidates.
use std::collections::BTreeMap;

use crate::error::AppError;
use iaam_core::bond::{
    AccrualPeriod, DefaultFlags, PrincipalReturn,
    offer::{
        OfferRight, OfferWindowId, OfferWindowTerms, ScheduleCompleteness, validate_unique_windows,
    },
};
use iaam_core::ids::InstrumentId;
use iaam_core::money::{CurrencyCode, PerUnitAmount};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::{
    PriceCandidate, PriceKind as CorePriceKind, PriceOrigin, SourceExecutability,
};
use iaam_market::moex::parse::reconcile_quotation_basis;
use iaam_market::{Executability, PriceKind, PriceObservation};
use iaam_store::schedule::{IssueTermsRow, StoredSnapshot};
use rust_decimal::Decimal;
use time::Date;
use time::format_description::well_known::Iso8601;

/// Converts a market observation into a domain valuation candidate.
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

/// Converts schedule snapshot rows into domain coupon periods.
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

            // Transpose, rather than unwrap_or: an unparsed date is a failure —
            // but an absent one is a legitimate «source did not report it».
            let record_date = row
                .record_date
                .as_deref()
                .map(|value| Date::parse(value, &Iso8601::DEFAULT))
                .transpose()
                .map_err(|error| AppError::Store(error.to_string()))?;

            let coupon_per_unit = match row.amount_status.as_str() {
                "amount_fixed" => {
                    let amount_per_unit = row.amount_per_unit.as_deref().ok_or_else(|| {
                        AppError::Store(
                            "known coupon amount does not contain amount_per_unit".to_owned(),
                        )
                    })?;
                    let amount_currency = row.amount_currency.as_deref().ok_or_else(|| {
                        AppError::Store(
                            "known coupon amount does not contain amount_currency".to_owned(),
                        )
                    })?;
                    let amount = Decimal::from_str_exact(amount_per_unit)
                        .map_err(|error| AppError::Store(error.to_string()))?;
                    let currency = CurrencyCode::from_code(amount_currency).ok_or_else(|| {
                        AppError::Store(format!("unknown coupon currency: {amount_currency}"))
                    })?;
                    Some(PerUnitAmount::new(Dec::new(amount), currency))
                }
                _ => None,
            };

            Ok(AccrualPeriod {
                period_start,
                accrual_end,
                payment_date,
                record_date,
                coupon_per_unit,
            })
        })
        .collect()
}

/// Converts schedule snapshot rows into domain principal repayments.
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

/// Converts snapshot window rows into typed rights and terms.
pub fn offer_windows_from_snapshot(
    snapshot: &StoredSnapshot,
    instrument: InstrumentId,
    offer_kinds: &BTreeMap<String, String>,
) -> Result<Vec<OfferWindowTerms>, AppError> {
    let windows = snapshot
        .offer_windows
        .iter()
        .map(|row| {
            let meaning = offer_kinds
                .get(&row.source_kind)
                .ok_or_else(|| AppError::Invalid {
                    field: "offer_kind".to_owned(),
                    expected: "a code known to the source dictionary".to_owned(),
                    actual: row.source_kind.clone(),
                })?;
            let right = OfferRight::from_dictionary_meaning(meaning).map_err(|error| {
                AppError::Invalid {
                    field: "offer_kind".to_owned(),
                    expected: "a known domain right value".to_owned(),
                    actual: error.to_string(),
                }
            })?;
            let execution_date = Date::parse(&row.execution_date, &Iso8601::DEFAULT)
                .map_err(|error| AppError::Store(error.to_string()))?;
            let submission_start = row
                .submission_start
                .as_deref()
                .map(|value| Date::parse(value, &Iso8601::DEFAULT))
                .transpose()
                .map_err(|error| AppError::Store(error.to_string()))?;
            let submission_end = row
                .submission_end
                .as_deref()
                .map(|value| Date::parse(value, &Iso8601::DEFAULT))
                .transpose()
                .map_err(|error| AppError::Store(error.to_string()))?;
            let price_percent = row
                .price_percent
                .as_deref()
                .map(Decimal::from_str_exact)
                .transpose()
                .map_err(|error| AppError::Store(error.to_string()))?
                .map(Dec::new);

            Ok(OfferWindowTerms {
                window: OfferWindowId::derive(instrument, execution_date),
                right,
                execution_date,
                submission_start,
                submission_end,
                price_percent,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    validate_unique_windows(&windows).map_err(|error| AppError::Invalid {
        field: "offer_windows".to_owned(),
        expected: "one row per exercise date".to_owned(),
        actual: error.to_string(),
    })?;
    Ok(windows)
}

/// Convert the stored completeness verdict without rechecking the schedule.
pub fn schedule_completeness_from_row(
    row: Option<&iaam_store::schedule::ScheduleCompletenessRow>,
) -> ScheduleCompleteness {
    let Some(row) = row else {
        return ScheduleCompleteness::Unknown;
    };
    if row.structurally_validated && row.fetch_exhausted {
        ScheduleCompleteness::Validated
    } else if let Some(reason) = &row.incomplete_reason {
        ScheduleCompleteness::Incomplete {
            reason: reason.clone(),
        }
    } else if !row.fetch_exhausted {
        ScheduleCompleteness::Incomplete {
            reason: "source was not read to the end".to_owned(),
        }
    } else {
        ScheduleCompleteness::Unknown
    }
}

pub(crate) const MOEX_ISS_SOURCE_ID: &str = "moex-iss";

/// Builds a unified domain schedule from the stored snapshot and issue terms.
///
/// Both entry points — reporting and journal fact acceptance — must obtain
/// the schedule through this conversion, so that adding a field or changing parsing
/// cannot diverge between two copies. `None` means no snapshot, while
/// `snapshot_id` is returned with the schedule for fingerprinting the inputs
/// to the share calculation. The store lock is owned by the caller.
pub fn schedule_from_store(
    store: &iaam_store::market::MarketStore,
    instrument: InstrumentId,
    knowledge_as_of: &str,
    offer_kinds: &BTreeMap<String, String>,
    currency_roles: Option<iaam_core::instrument::CurrencyRoles>,
) -> Result<Option<(iaam_core::bond::BondSchedule, String)>, AppError> {
    let instrument_id = instrument.inner().to_string();
    let Some(snapshot) = store
        .schedule_at_or_before(&instrument_id, MOEX_ISS_SOURCE_ID, knowledge_as_of)
        .map_err(|error| AppError::Store(error.to_string()))?
    else {
        return Ok(None);
    };
    let completeness_row = store
        .schedule_completeness(&snapshot.snapshot_id)
        .map_err(|error| AppError::Store(error.to_string()))?;
    let terms = store
        .issue_terms_at_or_before(&instrument_id, MOEX_ISS_SOURCE_ID, knowledge_as_of)
        .map_err(|error| AppError::Store(error.to_string()))?;
    let schedule = iaam_core::bond::BondSchedule {
        periods: accrual_periods_from_snapshot(&snapshot)?,
        principal_returns: principal_returns_from_snapshot(&snapshot)?,
        initial_principal: initial_principal_from_terms(terms.as_ref()),
        offer_windows: offer_windows_from_snapshot(&snapshot, instrument, offer_kinds)?,
        completeness: schedule_completeness_from_row(completeness_row.as_ref()),
        default_flags: default_flags_from_terms(terms.as_ref()),
        currency_roles,
    };
    Ok(Some((schedule, snapshot.snapshot_id)))
}

/// Convert known issue terms into typed default flags.
pub fn default_flags_from_terms(row: Option<&IssueTermsRow>) -> Option<DefaultFlags> {
    row.map(|terms| DefaultFlags {
        declared: terms.default_declared,
        technical: terms.default_technical,
    })
}

/// Initial face value from the issue terms row.
///
/// Currency is mandatory: a face value without a currency is not a number but a guess.
/// An unparsed value yields `None`, because «face value is unknown»
/// and «face value is zero» require different actions from the owner (§4.9).
#[must_use]
pub fn initial_principal_from_terms(terms: Option<&IssueTermsRow>) -> Option<PerUnitAmount> {
    let terms = terms?;
    let value = terms.initial_face_value.as_ref()?.parse::<Decimal>().ok()?;
    let currency = CurrencyCode::from_code(terms.face_currency_code.as_ref()?)?;
    Some(PerUnitAmount::new(Dec::new(value), currency))
}

#[cfg(test)]
mod tests {
    use iaam_core::money::{CurrencyCode, PerUnitAmount};
    use iaam_core::numeric::decimal::Dec;
    use iaam_core::rules::{ValuationPolicyV1, ValuationRule};
    use iaam_core::valuation::{
        PriceKind as CorePriceKind, PriceOrigin, PriceQuery, SourceExecutability,
    };
    use iaam_market::moex::parse::{MarketSegment, parse_history};
    use iaam_market::{Executability, ObservedAt, PriceKind, PriceObservation, TradeDate, Venue};
    use iaam_store::market::MarketStore;
    use iaam_store::schedule::{
        CouponPeriodRow, IssueTermsRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
        StoredSnapshot,
    };
    use rust_decimal::Decimal;
    use std::collections::BTreeMap;
    use time::macros::{date, datetime};

    use super::{
        MOEX_ISS_SOURCE_ID, accrual_periods_from_snapshot, candidate_from_market_observation,
        default_flags_from_terms, initial_principal_from_terms, offer_windows_from_snapshot,
        schedule_completeness_from_row, schedule_from_store,
    };

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

    #[test]
    fn record_date_is_translated_into_an_accrual_period() {
        let snapshot = StoredSnapshot {
            snapshot_id: "s1".to_owned(),
            observed_at: "2026-08-27T12:00:00Z".to_owned(),
            coupon_periods: vec![CouponPeriodRow {
                period_start: "2026-06-03".to_owned(),
                accrual_end: "2026-12-02".to_owned(),
                payment_date: "2026-12-03".to_owned(),
                record_date: Some("2026-11-30".to_owned()),
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

        assert_eq!(periods[0].record_date, Some(date!(2026 - 11 - 30)));
    }

    #[test]
    fn missing_record_date_stays_unknown_in_an_accrual_period() {
        let snapshot = StoredSnapshot {
            snapshot_id: "s1".to_owned(),
            observed_at: "2026-08-27T12:00:00Z".to_owned(),
            coupon_periods: vec![CouponPeriodRow {
                period_start: "2026-06-03".to_owned(),
                accrual_end: "2026-12-02".to_owned(),
                payment_date: "2026-12-03".to_owned(),
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

        assert_eq!(periods[0].record_date, None);
    }

    #[test]
    fn offer_translation_derives_stable_ids_and_keeps_unknown_terms_absent() {
        let instrument = iaam_core::ids::InstrumentId::new_random();
        let snapshot = StoredSnapshot {
            snapshot_id: "s1".to_owned(),
            observed_at: "2026-08-27T12:00:00Z".to_owned(),
            coupon_periods: Vec::new(),
            principal_repayments: Vec::new(),
            offer_windows: vec![OfferWindowRow {
                execution_date: "2026-12-01".to_owned(),
                submission_start: None,
                submission_end: None,
                price_percent: None,
                agent: None,
                source_kind: "source wording".to_owned(),
                source_entry_id: None,
            }],
        };
        let dictionary = BTreeMap::from([("source wording".to_owned(), "put_option".to_owned())]);
        let windows = offer_windows_from_snapshot(&snapshot, instrument, &dictionary).unwrap();

        assert_eq!(windows.len(), 1);
        assert_eq!(
            windows[0].right,
            iaam_core::bond::offer::OfferRight::HolderPut
        );
        assert!(windows[0].submission_start.is_none());
        assert!(windows[0].submission_end.is_none());
        assert!(windows[0].price_percent.is_none());
        assert_eq!(
            windows[0].window,
            iaam_core::bond::offer::OfferWindowId::derive(instrument, date!(2026 - 12 - 01))
        );
    }

    #[test]
    fn duplicate_offer_dates_are_rejected_as_ambiguous() {
        let snapshot = StoredSnapshot {
            snapshot_id: "s1".to_owned(),
            observed_at: "2026-08-27T12:00:00Z".to_owned(),
            coupon_periods: Vec::new(),
            principal_repayments: Vec::new(),
            offer_windows: vec![
                OfferWindowRow {
                    execution_date: "2026-12-01".to_owned(),
                    submission_start: None,
                    submission_end: None,
                    price_percent: Some("100".to_owned()),
                    agent: None,
                    source_kind: "offer-a".to_owned(),
                    source_entry_id: None,
                },
                OfferWindowRow {
                    execution_date: "2026-12-01".to_owned(),
                    submission_start: None,
                    submission_end: None,
                    price_percent: Some("101".to_owned()),
                    agent: None,
                    source_kind: "offer-b".to_owned(),
                    source_entry_id: None,
                },
            ],
        };
        let dictionary = BTreeMap::from([
            ("offer-a".to_owned(), "put_option".to_owned()),
            ("offer-b".to_owned(), "put_option".to_owned()),
        ]);

        let error = offer_windows_from_snapshot(
            &snapshot,
            iaam_core::ids::InstrumentId::new_random(),
            &dictionary,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            crate::error::AppError::Invalid { field, expected, .. }
                if field == "offer_windows" && expected == "one row per exercise date"
        ));
    }

    #[test]
    fn completeness_translation_preserves_the_persisted_verdict() {
        let row = iaam_store::schedule::ScheduleCompletenessRow {
            fetch_exhausted: true,
            structurally_validated: false,
            incomplete_reason: Some("schedule truncated".to_owned()),
        };
        assert_eq!(
            schedule_completeness_from_row(Some(&row)),
            iaam_core::bond::offer::ScheduleCompleteness::Incomplete {
                reason: "schedule truncated".to_owned()
            }
        );

        assert_eq!(
            schedule_completeness_from_row(None),
            iaam_core::bond::offer::ScheduleCompleteness::Unknown
        );
    }

    #[test]
    fn report_and_ingest_paths_share_store_schedule_translation() {
        let mut store = MarketStore::open_in_memory().expect("store");
        let instrument = iaam_core::ids::InstrumentId::new_random();
        store
            .upsert_instrument(&iaam_store::reference::InstrumentRecord {
                id: instrument,
                kind: Some(iaam_core::instrument::InstrumentKind::Bond),
                symbol: "BOND".to_owned(),
                title: "Test bond".to_owned(),
                currencies: iaam_core::instrument::CurrencyRoles::uniform(CurrencyCode::Rub),
                lineage: None,
            })
            .expect("instrument");
        let observed_at = "2026-08-27T12:00:00Z";
        let header = ScheduleSnapshotRow {
            instrument_id: instrument.inner().to_string(),
            source_id: MOEX_ISS_SOURCE_ID.to_owned(),
            observed_at: observed_at.to_owned(),
            content_hash: "schedule-hash".to_owned(),
        };
        let coupon_periods = vec![CouponPeriodRow {
            period_start: "2026-06-03".to_owned(),
            accrual_end: "2026-12-02".to_owned(),
            payment_date: "2026-12-03".to_owned(),
            record_date: Some("2026-11-30".to_owned()),
            amount_status: "amount_fixed".to_owned(),
            amount_per_unit: Some("12.50".to_owned()),
            amount_currency: Some("RUB".to_owned()),
            rate_percent: None,
            source_entry_id: Some("coupon-1".to_owned()),
        }];
        let principal_repayments = vec![PrincipalRepaymentRow {
            repayment_date: "2026-12-02".to_owned(),
            share_percent: "25".to_owned(),
            source_kind: "partial".to_owned(),
            source_entry_id: Some("repayment-1".to_owned()),
        }];
        let offer_windows = vec![OfferWindowRow {
            execution_date: "2026-12-01".to_owned(),
            submission_start: Some("2026-11-01".to_owned()),
            submission_end: Some("2026-11-20".to_owned()),
            price_percent: Some("100".to_owned()),
            agent: None,
            source_kind: "put".to_owned(),
            source_entry_id: Some("offer-1".to_owned()),
        }];
        let outcome = store
            .record_schedule_snapshot(
                &header,
                &coupon_periods,
                &principal_repayments,
                &offer_windows,
            )
            .expect("snapshot");
        store
            .record_schedule_completeness(&outcome.snapshot_id, true, true, None, &[0])
            .expect("completeness");
        store
            .record_issue_terms(&IssueTermsRow {
                instrument_id: instrument.inner().to_string(),
                source_id: MOEX_ISS_SOURCE_ID.to_owned(),
                observed_at: observed_at.to_owned(),
                effective_from: Some("2026-06-03".to_owned()),
                maturity_date: Some("2027-06-03".to_owned()),
                initial_face_value: Some("1000".to_owned()),
                face_currency_code: Some("RUB".to_owned()),
                coupon_periods_per_year: Some(2),
                day_count: Some("act/365".to_owned()),
                calendar: Some("MOEX".to_owned()),
                default_declared: true,
                default_technical: false,
            })
            .expect("issue terms");

        let offer_kinds = BTreeMap::from([("put".to_owned(), "put_option".to_owned())]);
        let (report_schedule, report_snapshot_id) = schedule_from_store(
            &store,
            instrument,
            "2026-08-28T00:00:00Z",
            &offer_kinds,
            Some(iaam_core::instrument::CurrencyRoles::uniform(
                CurrencyCode::Rub,
            )),
        )
        .expect("report schedule")
        .expect("report snapshot");
        let (ingest_schedule, ingest_snapshot_id) = schedule_from_store(
            &store,
            instrument,
            "2026-08-28T00:00:00Z",
            &offer_kinds,
            None,
        )
        .expect("acceptance schedule")
        .expect("acceptance snapshot");

        assert_eq!(report_snapshot_id, ingest_snapshot_id);
        assert_eq!(
            report_schedule.currency_roles,
            Some(iaam_core::instrument::CurrencyRoles::uniform(
                CurrencyCode::Rub,
            ))
        );
        let mut report_without_roles = report_schedule;
        report_without_roles.currency_roles = None;
        assert_eq!(report_without_roles, ingest_schedule);
    }

    #[test]
    fn missing_issue_terms_keep_default_flags_unknown() {
        assert_eq!(default_flags_from_terms(None), None);
    }

    fn terms_row(face: Option<&str>, currency: Option<&str>) -> IssueTermsRow {
        IssueTermsRow {
            instrument_id: "instrument".to_owned(),
            source_id: "source".to_owned(),
            observed_at: "2026-08-27T12:00:00Z".to_owned(),
            effective_from: Some("2026-08-01".to_owned()),
            maturity_date: Some("2026-12-02".to_owned()),
            initial_face_value: face.map(str::to_owned),
            face_currency_code: currency.map(str::to_owned),
            coupon_periods_per_year: Some(1),
            day_count: Some("act/365".to_owned()),
            calendar: Some("MOEX".to_owned()),
            default_declared: false,
            default_technical: false,
        }
    }

    #[test]
    fn the_initial_face_value_travels_from_the_terms_row() {
        let terms = terms_row(Some("1000"), Some("RUB"));
        assert_eq!(
            initial_principal_from_terms(Some(&terms)),
            Some(PerUnitAmount::new(
                Dec::new("1000".parse().expect("face value")),
                CurrencyCode::Rub
            ))
        );
    }

    #[test]
    fn a_face_value_without_a_currency_is_unknown_and_never_zero() {
        let terms = terms_row(Some("1000"), None);
        assert_eq!(initial_principal_from_terms(Some(&terms)), None);
    }

    #[test]
    fn a_missing_face_value_is_unknown_and_never_zero() {
        let terms = terms_row(None, Some("RUB"));
        assert_eq!(initial_principal_from_terms(Some(&terms)), None);
    }

    #[test]
    fn a_malformed_face_value_is_unknown_and_does_not_panic() {
        let terms = terms_row(Some("not a number"), Some("RUB"));
        assert_eq!(initial_principal_from_terms(Some(&terms)), None);
    }

    #[test]
    fn missing_terms_make_the_initial_principal_unknown() {
        assert_eq!(initial_principal_from_terms(None), None);
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
            _ => panic!("market observation should become a Market candidate"),
        }
    }
    #[test]
    fn market_candidate_preserves_the_full_venue_identity() {
        let candidate = candidate_from_market_observation(observation(
            PriceKind::Close,
            Executability::Executable,
        ));
        let PriceOrigin::Market { venue, .. } = candidate.origin else {
            panic!("market observation should become a Market candidate");
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
        .expect("fixture parsing");
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
                .unwrap_or_else(|| panic!("no candidate for {kind}"));
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
        .expect("fixture parsing");
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
        let selected = result.selected().expect("MarketPrice2 must cover the row");

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
