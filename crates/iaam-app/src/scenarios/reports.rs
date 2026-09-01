//! Reports.

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::bond::BondSchedule;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::kind::EventKind;
use iaam_core::ids::{AccountId, InstrumentId};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterPolicy, assess};
use iaam_core::projection::balances::{Balances, PositionKey};
use iaam_core::projection::money_flow::{DateWindow, MoneyFlow};
use iaam_core::projection::offers::OfferBook;
use iaam_core::projection::{Projection, ProjectionContext, ProjectionError, advance, project};
use iaam_core::reconciliation::claim::AssertionPeriod;
use iaam_core::reconciliation::{ReconciliationLedger, ReconciliationStatus};
use iaam_core::returns::{ReturnsReport, ReturnsRequest, returns_report_with_bond_inputs};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceCandidate, QuotationBasis, Venue as CoreVenue};
use iaam_market::{Executability, ObservedAt, PriceKind, PriceObservation, TradeDate, Venue};
use iaam_store::market::SeriesKey;
use iaam_store::market::{MarketWindow, PriceRow, PriceVenue};
use rust_decimal::Decimal;
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::categories::load_index;
use crate::AppServices;
use crate::error::AppError;
use crate::market_candidate::MOEX_ISS_SOURCE_ID;
use crate::ports::Principal;
/// Yield report request.
#[derive(Debug, Clone)]
pub struct ReturnsQuery {
    pub contour: ContourId,
    pub contour_version: Option<ContourVersion>,
    pub as_of: Option<Date>,
    pub report_currency: CurrencyCode,
    pub fx: FxTable,
    pub lot_rule: LotRuleVersion,
}

/// Money flow report request.
#[derive(Debug, Clone, Copy)]
pub struct MoneyFlowQuery {
    pub contour: ContourId,
    pub contour_version: Option<ContourVersion>,
    pub from: Date,
    pub to: Date,
}

/// Report of cash movement over an interval.
#[derive(Debug, Clone)]
pub struct MoneyFlowReport {
    pub contour: ContourId,
    pub version: ContourVersion,
    pub from: Date,
    pub to: Date,
    /// The active owner rule versions used to derive the decomposition.
    pub category_rule_versions: Vec<u32>,
    pub flow: MoneyFlow,
}

/// Cash, reconciliation, and positions for one contour account.
#[derive(Debug, Clone)]
pub struct AccountBalanceRow {
    pub account: iaam_core::ids::AccountId,
    pub cash: Vec<Money>,
    pub reconciliation: Vec<ReconciliationStatus>,
    pub positions: Vec<(PositionKey, Quantity)>,
}

async fn resolve_contour(
    services: &AppServices,
    principal: &Principal,
    contour: ContourId,
    requested_version: Option<ContourVersion>,
) -> Result<(ContourVersion, ContourDefinition), AppError> {
    let version = match requested_version {
        Some(version) => version,
        None => services
            .store
            .latest_contour_version(principal.owner, contour)
            .await?
            .ok_or_else(|| AppError::NotFound {
                what: "contour",
                id: contour.0.to_string(),
            })?,
    };
    let definition = services
        .store
        .load_contour(principal.owner, contour, version)
        .await?
        .ok_or_else(|| AppError::NotFound {
            what: "contour_version",
            id: format!("{}/{}", contour.0, version.0),
        })?;
    Ok((version, definition))
}

/// The flow of money over an interval.
///
/// The contour version is resolved exactly as `returns` resolves it, and is
/// reported back so the result remains comparable after contour changes.
pub async fn money_flow(
    services: &AppServices,
    principal: &Principal,
    query: &MoneyFlowQuery,
) -> Result<MoneyFlowReport, AppError> {
    if query.to < query.from {
        return Err(AppError::Invalid {
            field: "period".into(),
            expected: "from no later than to".into(),
            actual: format!("{}..{}", query.from, query.to),
        });
    }
    let (version, definition) =
        resolve_contour(services, principal, query.contour, query.contour_version).await?;
    let categories = load_index(services, principal).await?;
    let category_rule_versions = categories.versions().to_vec();
    let events = services
        .store
        .load_events_through(principal.owner, query.to)
        .await?;
    let window = DateWindow {
        from: query.from,
        to: query.to,
    };
    let mut flow = MoneyFlow::new();
    for event in &events {
        flow.apply(event, &definition, window, &categories)?;
    }
    Ok(MoneyFlowReport {
        contour: query.contour,
        version,
        from: query.from,
        to: query.to,
        category_rule_versions,
        flow,
    })
}

/// Cash balances, reconciliation statuses, and positions by contour account.
pub async fn account_balances(
    services: &AppServices,
    principal: &Principal,
    contour: ContourId,
    contour_version: Option<ContourVersion>,
    as_of: Date,
) -> Result<Vec<AccountBalanceRow>, AppError> {
    let (_version, definition) =
        resolve_contour(services, principal, contour, contour_version).await?;
    let events = services
        .store
        .load_events_through(principal.owner, as_of)
        .await?;
    let mut balances = Balances::new();
    for event in &events {
        balances
            .apply(event)
            .map_err(ProjectionError::from)
            .map_err(AppError::from_projection)?;
    }
    let ledger = ReconciliationLedger::build(&events)?;
    let period = AssertionPeriod::between(as_of, as_of).ok_or_else(|| AppError::Invalid {
        field: "period".into(),
        expected: "from no later than to".into(),
        actual: format!("{as_of}..{as_of}"),
    })?;

    // Accounts are never deleted, so this owner list equals contour membership.
    let contour_accounts: Vec<AccountId> = services
        .store
        .list_accounts(principal.owner)
        .await?
        .into_iter()
        .map(|account| account.id)
        .filter(|account| definition.contains(*account))
        .collect();
    let mut rows = Vec::with_capacity(contour_accounts.len());
    for account in contour_accounts {
        let cash = balances
            .iter_cash()
            .filter_map(|(owner_account, money)| (owner_account == account).then_some(money))
            .collect();
        let reconciliation =
            crate::scenarios::reconciliation::statuses_for_account(&ledger, account, period);
        let positions = balances
            .iter_positions()
            .filter_map(|(key, quantity)| (key.account == account).then_some((*key, quantity)))
            .collect();
        rows.push(AccountBalanceRow {
            account,
            cash,
            reconciliation,
            positions,
        });
    }
    Ok(rows)
}

struct ReportInputs<'a> {
    fx: &'a FxTable,
    market_prices: &'a [PriceCandidate],
    /// Schedule at the knowledge coordinate, by instrument.
    schedules: &'a BTreeMap<InstrumentId, BondSchedule>,
    /// Observed accrued coupon interest per security, tied to the venue and trade date.
    accrued_observations: &'a BTreeMap<(InstrumentId, CoreVenue, Date), PerUnitAmount>,
    knowledge_as_of: OffsetDateTime,
}

/// Report for a scope.
pub async fn returns(
    services: &AppServices,
    principal: &Principal,
    query: &ReturnsQuery,
) -> Result<ReturnsReport, AppError> {
    let version = match query.contour_version {
        Some(version) => version,
        None => services
            .store
            .latest_contour_version(principal.owner, query.contour)
            .await?
            .ok_or_else(|| AppError::NotFound {
                what: "contour",
                id: query.contour.0.to_string(),
            })?,
    };
    // The scope is loaded TOGETHER with its owner: someone else's scope is not found,
    // rather than found and rejected later (§14).
    let definition = services
        .store
        .load_contour(principal.owner, query.contour, version)
        .await?
        .ok_or_else(|| AppError::NotFound {
            what: "contour_version",
            id: format!("{}/{}", query.contour.0, version.0),
        })?;

    let today = services.clock.today();
    let as_of = query.as_of.unwrap_or(today);
    let knowledge_as_of = OffsetDateTime::now_utc();
    let fx = match query.fx.source() {
        FxSource::CbrOfficial => {
            official_fx_table(services, query.report_currency, as_of, knowledge_as_of).await?
        }
        FxSource::OwnerSupplied => query.fx.clone(),
    };
    let projection_events = services
        .store
        .load_events_through(principal.owner, as_of)
        .await?;

    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &definition,
        rules: &rules,
        lot_rule: query.lot_rule,
    };

    let projection = build_projection(
        services,
        principal.owner,
        query,
        &definition,
        &projection_events,
        &context,
    )
    .await?;
    let market_inputs =
        market_price_candidates(services, &projection, &definition, as_of, knowledge_as_of).await?;

    if snapshot_may_be_saved(as_of, today) {
        services
            .store
            .save_snapshot(principal.owner, projection.snapshot().clone())
            .await?;
    }

    // Projection is a historical snapshot; reconciliation may use facts
    // received later because they can confirm an earlier period.
    let reconciliation_events = services
        .store
        .load_events_through(principal.owner, Date::MAX)
        .await?;

    report_from_projection(
        &projection,
        query,
        ReportInputs {
            fx: &fx,
            market_prices: &market_inputs.candidates,
            schedules: &market_inputs.schedules,
            accrued_observations: &market_inputs.accrued_observations,
            knowledge_as_of,
        },
        &definition,
        as_of,
        &reconciliation_events,
    )
}

struct ReportMarketInputs {
    candidates: Vec<PriceCandidate>,
    schedules: BTreeMap<InstrumentId, BondSchedule>,
    accrued_observations: BTreeMap<(InstrumentId, CoreVenue, Date), PerUnitAmount>,
}

async fn market_price_candidates(
    services: &AppServices,
    projection: &Projection,
    definition: &ContourDefinition,
    as_of: Date,
    knowledge_as_of: OffsetDateTime,
) -> Result<ReportMarketInputs, AppError> {
    let instruments: BTreeSet<_> = projection
        .state()
        .balances()
        .iter_positions()
        .filter(|(key, quantity)| definition.contains(key.account) && !quantity.0.is_zero())
        .map(|(key, _)| key.instrument)
        .collect();
    let from_date = Date::MIN.to_string();
    let to_date = as_of.to_string();
    let knowledge_as_of = knowledge_as_of
        .format(&Rfc3339)
        .map_err(|error| AppError::Store(error.to_string()))?;

    let mut currency_roles = BTreeMap::new();
    for instrument in &instruments {
        let roles = services
            .directory
            .instrument(*instrument)
            .await?
            .map(|view| {
                let denomination = CurrencyCode::from_code(&view.denomination_currency)
                    .ok_or_else(|| {
                        AppError::Store(format!(
                            "unknown obligation currency: {}",
                            view.denomination_currency
                        ))
                    })?;
                let settlement =
                    CurrencyCode::from_code(&view.settlement_currency).ok_or_else(|| {
                        AppError::Store(format!(
                            "unknown settlement currency: {}",
                            view.settlement_currency
                        ))
                    })?;
                let quote = CurrencyCode::from_code(&view.quote_currency).ok_or_else(|| {
                    AppError::Store(format!(
                        "unknown quotation currency: {}",
                        view.quote_currency
                    ))
                })?;
                Ok::<CurrencyRoles, AppError>(CurrencyRoles {
                    denomination,
                    settlement,
                    quote,
                })
            })
            .transpose()?;
        currency_roles.insert(*instrument, roles);
    }

    let offer_kinds = {
        let store = services.market_store.lock().await;
        store
            .market_source_codes(MOEX_ISS_SOURCE_ID, "offer_kind")
            .map_err(|error| AppError::Store(error.to_string()))?
    };

    let store = services.market_store.lock().await;
    let mut candidates = Vec::new();
    let mut schedules = BTreeMap::new();
    let mut accrued_observations = BTreeMap::new();
    for instrument in instruments {
        let rows = store
            .prices_for_instrument_between(
                MOEX_ISS_SOURCE_ID,
                "prices",
                &instrument.inner().to_string(),
                MarketWindow {
                    from: &from_date,
                    to: &to_date,
                    knowledge_as_of: &knowledge_as_of,
                },
            )
            .map_err(|error| AppError::Store(error.to_string()))?;
        let mut venues = Vec::new();
        for row in rows {
            let venue = PriceVenue {
                board: row.board.clone(),
                session: row.session,
            };
            if !venues.contains(&venue) {
                venues.push(venue);
            }
            candidates.push(market_candidate_from_row(row)?);
        }

        if let Some((schedule, _snapshot_id)) = crate::market_candidate::schedule_from_store(
            &store,
            instrument,
            &knowledge_as_of,
            &offer_kinds,
            currency_roles.get(&instrument).copied().flatten(),
        )? {
            schedules.insert(instrument, schedule);
        }

        for venue in venues {
            let Some(row) = store
                .accrued_interest_at_or_before(
                    &instrument.inner().to_string(),
                    &venue,
                    &to_date,
                    &knowledge_as_of,
                )
                .map_err(|error| AppError::Store(error.to_string()))?
            else {
                continue;
            };
            let value = row
                .per_unit
                .parse::<Decimal>()
                .map_err(|error| AppError::Store(error.to_string()))?;
            let currency = CurrencyCode::from_code(&row.currency).ok_or_else(|| {
                AppError::Store(format!(
                    "unknown accrued coupon interest currency: {}",
                    row.currency
                ))
            })?;
            let trade_date = Date::parse(&row.trade_date, &Iso8601::DATE)
                .map_err(|error| AppError::Store(error.to_string()))?;
            accrued_observations.insert(
                (
                    instrument,
                    CoreVenue {
                        board: venue.board.clone(),
                        session: venue.session,
                    },
                    trade_date,
                ),
                PerUnitAmount::new(Dec::new(value), currency),
            );
        }
    }
    Ok(ReportMarketInputs {
        candidates,
        schedules,
        accrued_observations,
    })
}

fn market_candidate_from_row(row: PriceRow) -> Result<PriceCandidate, AppError> {
    let instrument = row
        .instrument_id
        .parse::<Uuid>()
        .map(iaam_core::ids::InstrumentId)
        .map_err(|error| AppError::Store(error.to_string()))?;
    let kind = match row.kind.as_str() {
        "close" => PriceKind::Close,
        "legal_close" => PriceKind::LegalClose,
        "weighted_average" => PriceKind::WeightedAverage,
        "market_price_2" => PriceKind::MarketPrice2,
        "market_price_3" => PriceKind::MarketPrice3,
        "admitted_quote" => PriceKind::AdmittedQuote,
        kind => {
            return Err(AppError::Store(format!(
                "unknown market price type: {kind}"
            )));
        }
    };
    let trade_date = Date::parse(&row.trade_date, &Iso8601::DATE)
        .map_err(|error| AppError::Store(error.to_string()))?;
    let observed_at = OffsetDateTime::parse(&row.observed_at, &Rfc3339)
        .map_err(|error| AppError::Store(error.to_string()))?;
    let price = row
        .price
        .parse::<Decimal>()
        .map_err(|error| AppError::Store(error.to_string()))?;
    let currency = CurrencyCode::from_code(&row.currency)
        .ok_or_else(|| AppError::Store(format!("unknown price currency: {}", row.currency)))?;
    let basis = QuotationBasis::from_code(&row.quotation_basis)
        .ok_or_else(|| AppError::Store(format!("unknown price basis: {}", row.quotation_basis)))?;
    let executability = match row.executability.as_str() {
        "executable" => Executability::Executable,
        "indicative_previous_close" => Executability::IndicativePreviousClose,
        quality => {
            return Err(AppError::Store(format!(
                "unknown market price executability: {quality}"
            )));
        }
    };
    Ok(crate::market_candidate::candidate_from_market_observation(
        PriceObservation {
            instrument,
            venue: Venue {
                board: row.board,
                session: row.session,
            },
            trade_date: TradeDate(trade_date),
            observed_at: ObservedAt(observed_at),
            kind,
            price: iaam_core::numeric::decimal::Dec::new(price),
            currency,
            basis,
            basis_evidence: row.basis_evidence,
            executability,
        },
    ))
}

async fn official_fx_table(
    services: &AppServices,
    report_currency: CurrencyCode,
    as_of: Date,
    knowledge_as_of: OffsetDateTime,
) -> Result<FxTable, AppError> {
    let knowledge_as_of = knowledge_as_of
        .format(&Rfc3339)
        .map_err(|error| AppError::Store(error.to_string()))?;
    let from_date = Date::MIN.to_string();
    let to_date = as_of.to_string();
    let mut table = FxTable::new(FxSource::CbrOfficial);
    let store = services.market_store.lock().await;

    for from in [
        CurrencyCode::Rub,
        CurrencyCode::Usd,
        CurrencyCode::Eur,
        CurrencyCode::Cny,
        CurrencyCode::Xau,
    ] {
        if from == report_currency {
            continue;
        }
        let from_code = from.code();
        let to_code = report_currency.code();
        let series = SeriesKey {
            source_id: "cbr".to_owned(),
            dataset: "fx".to_owned(),
            series_key: format!("{from_code}:{to_code}"),
        };
        let rows = store
            .fx_between(
                &series,
                from_code,
                to_code,
                MarketWindow {
                    from: &from_date,
                    to: &to_date,
                    knowledge_as_of: &knowledge_as_of,
                },
            )
            .map_err(|error| AppError::Store(error.to_string()))?;
        for row in rows {
            let date = Date::parse(
                &row.trade_date,
                time::macros::format_description!("[year]-[month]-[day]"),
            )
            .map_err(|error| AppError::Store(error.to_string()))?;
            let rate = row
                .unit_rate
                .parse::<Decimal>()
                .map_err(|error| AppError::Store(error.to_string()))?;
            table = table.with_rate(
                from,
                report_currency,
                date,
                iaam_core::numeric::decimal::Dec::new(rate),
            );
        }
    }
    Ok(table)
}

/// Whether a snapshot built from this slice may be saved.
///
/// Only for a report as at today: the snapshot key is the scope, its version and
/// the rule version, so a snapshot for a slice at a past date would be stored under
/// the same key and silently give the next request the wrong state.
///
/// Kept as a separate function for testability: comparing dates within the
/// scenario can otherwise be tested only through a running server and database, while an error
/// here does not look like one — it produces a figure, just not the right one.
const fn snapshot_may_be_saved(as_of: Date, today: Date) -> bool {
    // `Date` does not implement `PartialEq` in a const context via `==`
    // for references, but for values — it does.
    as_of.ordinal() == today.ordinal() && as_of.year() == today.year()
}
fn offer_book_through(
    events: &[iaam_core::event::Event],
    as_of: Date,
) -> Result<OfferBook, AppError> {
    let mut book = OfferBook::default();
    for event in events {
        if !event
            .dates
            .effective_date()
            .is_some_and(|date| date <= as_of)
        {
            continue;
        }
        if let EventKind::OfferExercise { action } = &event.kind {
            book.apply(action)
                .map_err(|error| AppError::Store(error.to_string()))?;
        }
    }
    Ok(book)
}

fn report_from_projection(
    projection: &Projection,
    query: &ReturnsQuery,
    inputs: ReportInputs<'_>,
    definition: &ContourDefinition,
    as_of: Date,
    reconciliation_events: &[iaam_core::event::Event],
) -> Result<ReturnsReport, AppError> {
    let perimeter = assess(reconciliation_events, PerimeterPolicy::default())?;
    let ledger = ReconciliationLedger::build_with(reconciliation_events, &perimeter.exceptions())?;
    let offer_book = offer_book_through(reconciliation_events, as_of)?;

    Ok(returns_report_with_bond_inputs(
        projection.state(),
        &ReturnsRequest {
            contour: definition,
            coordinate: iaam_core::returns::KnowledgeCoordinate {
                knowledge_as_of: inputs.knowledge_as_of,
                source_priority_version: 1,
                valuation_policy_version: 1,
            },
            as_of,
            report_currency: query.report_currency,
            fx: inputs.fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: inputs.market_prices,
            bond_schedules: inputs.schedules,
            accrued_observations: inputs.accrued_observations,
        },
        &offer_book,
    ))
}
/// Whether to recompute the entire journal after `advance` fails.
///
/// A snapshot is a cache, and being unusable is not an operational error: almost
/// any failure is a legitimate reason to recompute. Except one: an invariant
/// violation will not be fixed by recomputation; it will produce exactly the same result, so we avoid
/// doing the work twice and propagate the failure with a correlation ID
/// (§15.2).
fn recompute_is_worth_it(error: &ProjectionError) -> bool {
    !error.is_invariant_violation()
}

/// Building the projection: advancing the snapshot if applicable,
/// otherwise a full recomputation.
///
/// The slice is passed to `advance` **in full**: the core decides what has
/// already been incorporated. The wrapper must not select «only
/// new» — an event arriving later with a date before the snapshot boundary would,
/// under such filtering, silently disappear from the calculation.
///
/// Any `advance` failure is a legitimate reason to recompute the entire journal:
/// the snapshot is a cache, and being unusable is not an operational error.
/// An invariant violation will not go away — it
/// will also occur during full recomputation.
async fn build_projection(
    services: &AppServices,
    owner: iaam_core::ids::OwnerId,
    query: &ReturnsQuery,
    definition: &ContourDefinition,
    events: &[iaam_core::event::Event],
    context: &ProjectionContext<'_>,
) -> Result<Projection, AppError> {
    let snapshot = services
        .store
        .load_snapshot(owner, definition.id(), definition.version(), query.lot_rule)
        .await?;

    if let Some(snapshot) = snapshot {
        match advance(&snapshot, events, context) {
            Ok(projection) => return Ok(projection),
            Err(error) if !recompute_is_worth_it(&error) => {
                // An invariant violation is no reason to recompute: a full
                // recomputation will produce the same result. We propagate it so that
                // it is logged with a correlation ID (§15.2).
                return Err(AppError::from_projection(error));
            }
            Err(error) => tracing::info!(
                contour = %definition.id().0,
                reason = error.code(),
                "snapshot is unusable, recomputing the entire journal"
            ),
        }
    }

    project(events, context).map_err(AppError::from_projection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::projection::invariants::InvariantViolation;
    use time::macros::date;

    #[test]
    fn a_later_opening_assertion_confirms_the_earlier_report() {
        use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
        use iaam_core::event::kind::EventKind;
        use iaam_core::event::leg::Leg;
        use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
        use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
        use iaam_core::ids::{EventId, SourceId};
        use iaam_core::money::{CurrencyCode, Money, PostedMinor};
        use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
        use iaam_core::returns::DataQualityStatus;
        use iaam_core::valuation::{FxSource, FxTable};

        let owner = iaam_core::ids::OwnerId::new_random();
        let account = iaam_core::ids::AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let march = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
            .unwrap_or_else(|| panic!("valid March period"));
        let april = AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30))
            .unwrap_or_else(|| panic!("valid April period"));
        let provenance = Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"a".repeat(64)).unwrap_or_else(|| panic!("valid raw hash")),
            ParserVersion("tinkoff-xlsx/1".to_owned()),
        );
        let later_provenance = Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"b".repeat(64)).unwrap_or_else(|| panic!("valid later raw hash")),
            ParserVersion("tinkoff-xlsx/1".to_owned()),
        );
        let event = |day, sequence, kind| Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner,
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: Vec::new(),
            provenance: provenance.clone(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        };
        let deposit = Event {
            legs: vec![Leg::cash(
                account,
                Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
            )],
            kind: EventKind::CashIn {
                amount: Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
            },
            ..event(
                date!(2026 - 03 - 02),
                1,
                EventKind::CashIn {
                    amount: Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
                },
            )
        };
        let march_closing = event(
            date!(2026 - 03 - 31),
            2,
            EventKind::ControlAssertion {
                period: march,
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(100_000),
                    at: BalancePoint::Closing,
                },
            },
        );
        let april_opening = Event {
            provenance: later_provenance,
            ..event(
                date!(2026 - 04 - 30),
                1,
                EventKind::ControlAssertion {
                    period: april,
                    claim: ControlClaim::CashBalance {
                        currency: CurrencyCode::Rub,
                        amount: PostedMinor::new(100_000),
                        at: BalancePoint::Opening,
                    },
                },
            )
        };
        let all_events = vec![deposit, march_closing, april_opening];
        let projection_events: Vec<_> = all_events
            .iter()
            .filter(|event| {
                event
                    .dates
                    .effective_date()
                    .is_some_and(|date| date <= date!(2026 - 03 - 31))
            })
            .cloned()
            .collect();
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let projection = project(&projection_events, &context)
            .unwrap_or_else(|error| panic!("projection: {error}"));
        let query = ReturnsQuery {
            contour: contour.id(),
            contour_version: Some(contour.version()),
            as_of: Some(date!(2026 - 03 - 31)),
            report_currency: CurrencyCode::Rub,
            fx: FxTable::new(FxSource::OwnerSupplied),
            lot_rule: LotRuleVersion(1),
        };
        let report = report_from_projection(
            &projection,
            &query,
            ReportInputs {
                fx: &query.fx,
                market_prices: &[],
                schedules: &BTreeMap::new(),
                accrued_observations: &BTreeMap::new(),
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
            },
            &contour,
            date!(2026 - 03 - 31),
            &all_events,
        )
        .unwrap_or_else(|error| panic!("report: {error}"));

        assert_eq!(
            report.data_quality.nav_coverage.accepted_internal,
            iaam_core::numeric::decimal::Dec::one()
        );
        assert_eq!(
            report.data_quality.nav_coverage.provisional,
            iaam_core::numeric::decimal::Dec::zero()
        );
        assert_eq!(report.data_quality.status, DataQualityStatus::Clean);
    }

    #[test]
    fn a_snapshot_is_saved_only_for_a_report_dated_today() {
        // Putting yesterday's slice under today's key gives the next request the wrong state,
        // rather than saving work.
        let today = date!(2026 - 01 - 01);
        assert!(snapshot_may_be_saved(today, today));
        assert!(!snapshot_may_be_saved(date!(2025 - 12 - 31), today));
        assert!(!snapshot_may_be_saved(date!(2026 - 01 - 02), today));
        // The same day in a different year is not the same date.
        assert!(!snapshot_may_be_saved(date!(2025 - 01 - 01), today));
    }

    #[test]
    fn every_failure_except_a_broken_invariant_is_worth_a_full_recompute() {
        // An unusable snapshot is routine: recalculate. A violated
        // invariant will be reproduced verbatim by recalculation, so propagate it
        // immediately.
        assert!(recompute_is_worth_it(
            &ProjectionError::SnapshotFingerprintMismatch
        ));
        assert!(recompute_is_worth_it(
            &ProjectionError::SnapshotRuleMismatch {
                snapshot: LotRuleVersion(1),
                requested: LotRuleVersion(2),
            }
        ));
        assert!(!recompute_is_worth_it(&ProjectionError::Invariant(
            InvariantViolation::ZeroExternalFlow {
                event: iaam_core::ids::EventId::new_random(),
            }
        )));
    }
}
