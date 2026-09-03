//! Reports.

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::bond::BondSchedule;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::correction::resolve;
use iaam_core::event::kind::EventKind;
use iaam_core::ids::{AccountId, InstrumentId};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CurrencyCode, PerUnitAmount};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{
    NegativeCashSpan, PerimeterAssessment, PerimeterPolicy, assess, assess_effective,
};
use iaam_core::projection::balances::Balances;
use iaam_core::projection::money_flow::{DateWindow, MoneyFlow};
use iaam_core::projection::offers::OfferBook;
use iaam_core::projection::{Projection, ProjectionContext, ProjectionError, advance, project};
use iaam_core::reconciliation::claim::AssertionPeriod;
use iaam_core::reconciliation::{OpeningAnchor, OpeningAnchors, ReconciliationLedger};
use iaam_core::report::assets;
use iaam_core::returns::{
    KnowledgeCoordinate, ReturnsReport, ReturnsRequest, returns_report_with_bond_inputs,
};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{
    FxSource, FxTable, PriceBoard, PriceCandidate, QuotationBasis, Venue as CoreVenue,
};
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
use crate::ports::{AccountView, NegativeBalanceExpectation, Principal};

pub use iaam_core::goal::ReportGoal;
/// The report vocabulary, in the core.
///
/// These types were defined here, beside the scenario that fills them, and
/// moved into `iaam_core::report` when the caveat register was written: the
/// register is derived from exactly these fields, and a summary computed
/// outside the core can disagree with the report it summarises. Re-exported
/// under their old paths, because every caller of this module names the report
/// by the same word whichever crate defines it.
pub use iaam_core::report::assets::{
    AssetAccount, AssetSnapshot, CashClassTotal, CashSide, HoldingValue, PositionsSide,
};
pub use iaam_core::report::balances::{
    AccountBalanceRow, AccountCash, BalancesReport, CashFigure, CashOpening, NegativeCash,
    PeriodReports,
};
pub use iaam_core::report::confidence::{
    Caveat, CaveatKind, CaveatSubject, ReportConfidence, money_flow_confidence, returns_confidence,
};
pub use iaam_core::report::population::{
    AccountStanding, KnownAccountCoverage, PopulationAccount, ReportPopulation,
};

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

/// The returns answer: the report, and the population it answered about.
///
/// A wrapper rather than a field on `iaam_core::returns::ReturnsReport`: the
/// core computes a fold over the contour it is handed and has no way to know
/// what else the owner has, so the second statement cannot be made there. It
/// travels **with** the report rather than beside it in the caller, because a
/// caller free to drop it would eventually publish the figures alone — which is
/// the state this type exists to end.
#[derive(Debug, Clone)]
pub struct ReturnsOutcome {
    pub report: ReturnsReport,
    /// The accounts the report covered, and the known accounts it did not.
    pub population: ReportPopulation,
}

impl ReturnsOutcome {
    /// What would have to be true for these figures to be complete, and which
    /// of those are not.
    ///
    /// A method that delegates, not a computation: the fold is
    /// [`returns_confidence`] in the core, beside the numbers it reads.
    #[must_use]
    pub fn confidence(&self) -> ReportConfidence {
        returns_confidence(&self.population, &self.report)
    }
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

/// The flow answer: the report, and the population it answered about.
///
/// A wrapper for the reason `ReturnsOutcome` is one — the population is a
/// statement about which of the owner's accounts were selected, made before the
/// fold, and a `MoneyFlowReport` is what the fold produced. The scenario returns
/// the pair so that no caller can obtain one without the other.
#[derive(Debug, Clone)]
pub struct MoneyFlowOutcome {
    pub report: MoneyFlowReport,
    /// The accounts this answer covered, and the known accounts it did not.
    pub population: ReportPopulation,
}

impl MoneyFlowOutcome {
    /// What would have to be true for these figures to be complete, and which
    /// of those are not.
    ///
    /// Fallible for the reason every reader of a [`MoneyFlow`] aggregate is:
    /// the register asks the fold what it did not decompose and what it could
    /// not explain, and those are sums that can overflow. A register that
    /// swallowed the error would report a complete answer over figures the
    /// report itself refuses to state.
    pub fn confidence(
        &self,
    ) -> Result<ReportConfidence, iaam_core::projection::money_flow::MoneyFlowError> {
        money_flow_confidence(&self.population, &self.report.flow)
    }
}

/// The population, from the contour the fold was given, the accounts the system
/// knows about, and the two things that can put an account outside on purpose.
///
/// `placed_elsewhere` is the set of accounts some other contour of the owner
/// claims. `ruled_outside` is the set he has ruled outside every contour of his
/// through `record_account_scope`, with a reason. Both are parameters rather
/// than lookups here so that the decision this function makes — which of four
/// standings an account has — can be tested without a store, and so that the
/// store round-trips happen once per report.
///
/// **Membership outranks a disposition.** Nothing clears an exclusion when an
/// account is later added to a contour, so an account can carry both, and then
/// the two disagree about whether the owner wants it reported. When they do,
/// this reports the membership — which is what [`crate::actions::account_scope`]
/// reports to the outstanding-work queue. The queue and the report reading one
/// pair of facts in two different orders would be one question with two
/// answers, and the reader would have no way to tell which of them the owner
/// had actually said last.
fn population_from(
    definition: &ContourDefinition,
    accounts: Vec<AccountView>,
    placed_elsewhere: &BTreeSet<AccountId>,
    ruled_outside: &BTreeSet<AccountId>,
) -> ReportPopulation {
    let entries = accounts
        .into_iter()
        .map(|account| {
            let standing = if definition.contains(account.id) {
                AccountStanding::Covered
            } else if placed_elsewhere.contains(&account.id) {
                AccountStanding::OutsidePlacedElsewhere
            } else if ruled_outside.contains(&account.id) {
                AccountStanding::OutsideByDecision
            } else {
                AccountStanding::OutsideUndecided
            };
            PopulationAccount {
                account: account.id,
                title: account.title,
                institution: account.institution,
                standing,
            }
        })
        .collect();
    ReportPopulation {
        contour: definition.id(),
        version: definition.version(),
        accounts: entries,
    }
}

/// Every account claimed by a contour of this owner other than the one the
/// report was computed over.
///
/// Membership elsewhere is evidence that the owner has ruled on where an
/// account belongs: he drew a contour and put it in one. It is **not** evidence
/// that leaving it out of this report was intended — only that the account is
/// not one the system has never been told anything about.
///
/// The authoritative disposition is a separate notion, and it did not replace
/// this derivation when it arrived: the two answer different questions, so they
/// stand beside each other as two of the four standings. This one says the
/// account is somewhere; [`account_scope_exclusions`] says the owner wants it
/// nowhere.
///
/// [`account_scope_exclusions`]: crate::ports::ReferenceStore::list_account_scope_exclusions
///
/// Each contour is read at the version the store currently holds: the question
/// is what the owner has decided by now, not what he had decided when the
/// report's own contour version was drawn.
async fn accounts_placed_elsewhere(
    services: &AppServices,
    principal: &Principal,
    definition: &ContourDefinition,
    accounts: &[AccountView],
) -> Result<BTreeSet<AccountId>, AppError> {
    let mut placed = BTreeSet::new();
    for contour in services.store.list_contours(principal.owner).await? {
        if contour.id == definition.id() && contour.version == definition.version() {
            continue;
        }
        let Some(other) = services
            .store
            .load_contour(principal.owner, contour.id, contour.version)
            .await?
        else {
            continue;
        };
        for account in accounts {
            if other.contains(account.id) {
                placed.insert(account.id);
            }
        }
    }
    Ok(placed)
}

/// The population a report answers about, computed where the population is
/// chosen: from the contour definition the fold itself is given.
async fn report_population(
    services: &AppServices,
    principal: &Principal,
    definition: &ContourDefinition,
) -> Result<ReportPopulation, AppError> {
    let accounts = services.store.list_accounts(principal.owner).await?;
    let placed_elsewhere =
        accounts_placed_elsewhere(services, principal, definition, &accounts).await?;
    // The owner's own ruling, read rather than inferred. Before this read the
    // report derived every standing from contour membership, so a disposition
    // recorded in as many words changed nothing a reader of the report could
    // see — while the register beside it went on naming the call that recorded
    // it as the way to close the caveat.
    let ruled_outside = services
        .store
        .list_account_scope_exclusions(principal.owner)
        .await?
        .into_iter()
        .map(|exclusion| exclusion.account)
        .collect();
    Ok(population_from(
        definition,
        accounts,
        &placed_elsewhere,
        &ruled_outside,
    ))
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
) -> Result<MoneyFlowOutcome, AppError> {
    if query.to < query.from {
        return Err(AppError::Invalid {
            field: "period".into(),
            expected: "from no later than to".into(),
            actual: format!("{}..{}", query.from, query.to),
        });
    }
    let (version, definition) =
        resolve_contour(services, principal, query.contour, query.contour_version).await?;
    // Built from the definition the fold below is given, at the point the
    // population is chosen. A manifest assembled afterwards from the flow's own
    // account keys would name only the accounts that happened to move money.
    let population = report_population(services, principal, &definition).await?;
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
    // The **effective** set, not the raw journal: a reversed or replaced event is
    // still recorded, and folding it would report money that the owner has
    // already retracted. `resolve` is the only definition of that set — the same
    // one `projection::project` folds — so there is no second answer here to
    // what the journal currently says (§4.8).
    let effective = resolve(&events).map_err(AppError::Correction)?;
    let mut flow = MoneyFlow::new();
    for event in effective {
        flow.apply(event, &definition, window, &categories)?;
    }
    Ok(MoneyFlowOutcome {
        report: MoneyFlowReport {
            contour: query.contour,
            version,
            from: query.from,
            to: query.to,
            category_rule_versions,
            flow,
        },
        population,
    })
}

/// Cash balances, reconciliation statuses, and positions by contour account.
pub async fn account_balances(
    services: &AppServices,
    principal: &Principal,
    contour: ContourId,
    contour_version: Option<ContourVersion>,
    as_of: Date,
) -> Result<BalancesReport, AppError> {
    let (report, _prices) =
        balances_with_prices(services, principal, contour, contour_version, as_of).await?;
    Ok(report)
}

/// What the owner holds at a date, grouped by the class of cash he declared.
///
/// One journal read, one fold, two statements. The rows are the balances
/// answer's own rows and the totals are folded from them in the core
/// ([`iaam_core::report::assets::asset_snapshot`]); nothing is summed here.
///
/// The class reaches the core as an **opaque code**. Report grouping is the one
/// consumer decision 0004 §3 allows it, and the core cannot branch on a class
/// it cannot name.
///
/// The market store is read through the same call the returns report makes, and
/// the observations are handed to the core, which chooses among them with the
/// same policy. This scenario picks no price: it could not, and `make arch`
/// says so, but the reason is older than the guard — a selection made here
/// would be a second one, and the snapshot's holding could then disagree with
/// `terminal_value` over the same instrument on the same day.
pub async fn asset_snapshot(
    services: &AppServices,
    principal: &Principal,
    contour: ContourId,
    contour_version: Option<ContourVersion>,
    as_of: Date,
) -> Result<AssetSnapshot, AppError> {
    let (report, prices) =
        balances_with_prices(services, principal, contour, contour_version, as_of).await?;
    // Only the accounts the owner declared a class for appear here. An account
    // missing from the map has said nothing, which is a value the fold groups
    // on its own and never fills in.
    let classes: BTreeMap<AccountId, String> = services
        .store
        .list_account_details(principal.owner)
        .await?
        .into_iter()
        .filter_map(|account| {
            account
                .cash_class
                .map(|class| (account.id, class.code().to_owned()))
        })
        .collect();
    let knowledge_as_of = OffsetDateTime::now_utc();
    let instruments: BTreeSet<InstrumentId> = report
        .accounts
        .iter()
        .flat_map(|row| row.positions.iter())
        .filter(|(_, quantity)| !quantity.0.is_zero())
        .map(|(key, _)| key.instrument)
        .collect();
    let market_inputs =
        market_price_candidates(services, instruments, as_of, knowledge_as_of).await?;
    assets::asset_snapshot(
        as_of,
        &report,
        &classes,
        assets::SnapshotPrices {
            board: &prices,
            market: &market_inputs.candidates,
            schedules: &market_inputs.schedules,
            // The same coordinate the returns report states, so the two answers
            // are answers to the same question. `1` is the version both paths
            // carry today; when it becomes a stored decision, both read it.
            coordinate: KnowledgeCoordinate {
                knowledge_as_of,
                source_priority_version: 1,
                valuation_policy_version: 1,
            },
        },
    )
    .map_err(AppError::AssetSnapshot)
}

/// The balances answer and the journal's price board, from one read of the
/// journal.
///
/// The board is folded beside the balances rather than by a second pass,
/// because the snapshot's two halves must describe one state of the world: a
/// price read from a journal loaded a moment later could postdate the cash it
/// stands beside.
async fn balances_with_prices(
    services: &AppServices,
    principal: &Principal,
    contour: ContourId,
    contour_version: Option<ContourVersion>,
    as_of: Date,
) -> Result<(BalancesReport, PriceBoard), AppError> {
    let (_version, definition) =
        resolve_contour(services, principal, contour, contour_version).await?;
    let events = services
        .store
        .load_events_through(principal.owner, as_of)
        .await?;
    // Balances and the reconciliation ledger must read the same journal. The
    // ledger resolves internally; folding the raw slice into `Balances` beside
    // it would give one function two answers to what is currently effective,
    // and a retracted deposit would keep counting in the cash figure while the
    // status beside it had already stopped confirming it (§4.8).
    let effective = resolve(&events).map_err(AppError::Correction)?;
    let mut balances = Balances::new();
    // The board is filled by `PriceBoard::observe`, the same call the projection
    // makes: one definition of "this event carries a price", so a report cannot
    // value a holding from a price the projection would not have recorded.
    let mut prices = PriceBoard::new();
    for event in &effective {
        balances
            .apply(event)
            .map_err(ProjectionError::from)
            .map_err(AppError::from_projection)?;
        prices.observe(event);
    }
    // §11 is assessed from the set already in hand rather than from the raw
    // journal: `assess` would resolve it a second time, and a request that
    // folds one journal three times leaves the next reader to work out which
    // fold is authoritative.
    let perimeter = assess_effective(&effective, PerimeterPolicy::default())?;
    // `build_with`, as the returns path does it: a discrepancy the perimeter
    // explains becomes `Excepted` rather than something the owner is sent to
    // fix. Plain `build` here would have told him to reconcile financing the
    // system deliberately does not reconstruct.
    let ledger = ReconciliationLedger::build_with(&events, &perimeter.exceptions())?;
    let period = AssertionPeriod::between(as_of, as_of).ok_or_else(|| AppError::Invalid {
        field: "period".into(),
        expected: "from no later than to".into(),
        actual: format!("{as_of}..{as_of}"),
    })?;

    // The manifest is built first and the rows are built **from** it: one
    // selection of accounts serves both, so a report cannot cover one set and
    // name another. Accounts are never deleted, so the covered side of the
    // manifest is exactly contour membership.
    let population = report_population(services, principal, &definition).await?;
    let contour_accounts: Vec<AccountId> =
        population.covered().map(|entry| entry.account).collect();
    // The effective set for the same reason `Balances` uses it above: a
    // retracted movement is not this account's first one, and a retracted
    // assertion anchors nothing.
    //
    // The rule itself is `iaam_core::reconciliation::OpeningAnchors` and is not
    // restated here. It used to be, and reconciliation applied a different one
    // over the same silence: this answer refused to call an unanchored fold a
    // balance while reconciliation called it zero and told the owner his own
    // anchor was wrong (`iaam-d7hn`). Two copies of one rule is what made that
    // possible.
    let anchors = OpeningAnchors::of(&effective);
    let mut rows = Vec::with_capacity(contour_accounts.len());
    for account in contour_accounts {
        let cash = balances
            .iter_cash()
            .filter(|(owner_account, _)| *owner_account == account)
            .map(|(_, money)| AccountCash {
                money,
                opening: cash_opening(anchors.cash(account, money.currency())),
            })
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
            period_reports: period_reports(&perimeter, account),
        });
    }
    // At the report date at most one span per account and currency is still
    // open, so this is a lookup and not a search: each negative figure below is
    // the tail of exactly one of these.
    let open_spans: BTreeMap<(AccountId, CurrencyCode), NegativeCashSpan> = perimeter
        .spans()
        .iter()
        .filter(|span| span.resolved.is_none())
        .map(|span| ((span.account, span.currency), *span))
        .collect();
    // What the owner said about a negative balance on each account, for the
    // accounts where he said anything. Read here and nowhere else, and read
    // ALONE: `cash_class` is not consulted, and no expectation is ever derived
    // from one. «A savings account cannot be overdrawn, therefore warn» is the
    // branch decision 0004 §3 forbids by name, and it is wrong on the first
    // ordinary technical overdraft.
    let expectations: BTreeMap<AccountId, NegativeBalanceExpectation> = services
        .store
        .list_account_details(principal.owner)
        .await?
        .into_iter()
        .filter_map(|account| {
            account
                .negative_balance_expectation
                .map(|expectation| (account.id, expectation))
        })
        .collect();
    // Restricted to the contour: the projection holds every account the owner
    // has, and a liability outside the requested boundary is not a fact about
    // this answer.
    let negative_cash = balances
        .negative_cash()
        .filter(|(account, _)| definition.contains(*account))
        .map(|(account, money)| NegativeCash {
            account,
            money,
            span: open_spans.get(&(account, money.currency())).copied(),
            // The owner's statement travels with the figure, and nothing acts
            // on it: the entry exists because the balance is negative, and it
            // would exist unchanged if he had said nothing.
            expectation: expectations.get(&account).copied(),
        })
        .collect();
    Ok((
        BalancesReport {
            accounts: rows,
            negative_cash,
            population,
        },
        prices,
    ))
}

/// What §11 says about one account's period reports.
///
/// The predicate comes from the assessment rather than being re-derived here:
/// `blocks_period_reports` is the §11 question, and a second answer to it in
/// the wrapper is the kind of divergence that leaves the reason on the wire
/// disagreeing with the refusal beside it. The spans are that answer's reason,
/// filtered to the blocking ones — a temporary settlement deficit is not a
/// reason for a refusal it did not cause.
fn period_reports(perimeter: &PerimeterAssessment, account: AccountId) -> PeriodReports {
    if !perimeter.blocks_period_reports(account) {
        return PeriodReports::Calculated;
    }
    PeriodReports::Refused(
        perimeter
            .spans()
            .iter()
            .filter(|span| span.account == account && span.classification.blocks_reports())
            .copied()
            .collect(),
    )
}

/// The balances answer's word for what the core rule decided.
///
/// A translation and nothing else: the rule lives in
/// [`OpeningAnchors`](iaam_core::reconciliation::OpeningAnchors), which
/// reconciliation reads too, and this maps its answer onto the vocabulary this
/// report publishes. Two spellings of one distinction are tolerable; two rules
/// were not.
const fn cash_opening(anchor: OpeningAnchor) -> CashOpening {
    match anchor {
        OpeningAnchor::Asserted => CashOpening::Asserted,
        OpeningAnchor::Unasserted => CashOpening::Unasserted,
    }
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
) -> Result<ReturnsOutcome, AppError> {
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

    // Here, where the contour that selects the projection's accounts has just
    // been resolved and before anything is folded over it. The report's own
    // quality fields are computed downstream and can all be clean while this
    // says half the owner's money was never in the calculation.
    let population = report_population(services, principal, &definition).await?;

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
    let instruments: BTreeSet<InstrumentId> = projection
        .state()
        .balances()
        .iter_positions()
        .filter(|(key, quantity)| definition.contains(key.account) && !quantity.0.is_zero())
        .map(|(key, _)| key.instrument)
        .collect();
    let market_inputs =
        market_price_candidates(services, instruments, as_of, knowledge_as_of).await?;

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

    let report = report_from_projection(
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
    )?;
    Ok(ReturnsOutcome { report, population })
}

struct ReportMarketInputs {
    candidates: Vec<PriceCandidate>,
    schedules: BTreeMap<InstrumentId, BondSchedule>,
    accrued_observations: BTreeMap<(InstrumentId, CoreVenue, Date), PerUnitAmount>,
}

/// Everything the market store holds about a set of instruments at a
/// coordinate: the price observations, the payment schedules, and the accrued
/// coupon interest.
///
/// The instruments are a parameter rather than derived from a projection so
/// that the asset snapshot, which folds balances and never builds one, reads
/// the market through **this** function too. Two readers, one store round, one
/// set of rows — and therefore one set of candidates for the core to choose
/// among.
async fn market_price_candidates(
    services: &AppServices,
    instruments: BTreeSet<InstrumentId>,
    as_of: Date,
    knowledge_as_of: OffsetDateTime,
) -> Result<ReportMarketInputs, AppError> {
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
            coordinate: KnowledgeCoordinate {
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
    use iaam_core::operation::OperationKey;
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
    fn the_manifest_separates_a_deliberate_exclusion_from_an_open_question() {
        // The distinction the manifest exists for. Without it the two outside
        // accounts read alike, and a report over a contour nobody has finished
        // drawing reads as an answer about everything the owner has.
        let inside = AccountId::new_random();
        let elsewhere = AccountId::new_random();
        let undecided = AccountId::new_random();
        let definition =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(3), [inside]);
        let accounts = vec![
            AccountView {
                id: inside,
                title: "Main".to_owned(),
                institution: None,
            },
            AccountView {
                id: elsewhere,
                title: "Savings".to_owned(),
                institution: None,
            },
            AccountView {
                id: undecided,
                title: "Shop One".to_owned(),
                institution: None,
            },
        ];
        let placed: BTreeSet<AccountId> = [elsewhere].into_iter().collect();

        let population = population_from(&definition, accounts, &placed, &BTreeSet::new());

        assert_eq!(population.contour, definition.id());
        assert_eq!(population.version, ContourVersion(3));
        assert_eq!(
            population
                .covered()
                .map(|entry| entry.account)
                .collect::<Vec<_>>(),
            vec![inside]
        );
        assert_eq!(
            population
                .outside()
                .map(|entry| (entry.account, entry.standing))
                .collect::<Vec<_>>(),
            vec![
                (elsewhere, AccountStanding::OutsidePlacedElsewhere),
                (undecided, AccountStanding::OutsideUndecided),
            ]
        );
        assert_eq!(
            population
                .undecided()
                .map(|entry| entry.account)
                .collect::<Vec<_>>(),
            vec![undecided]
        );
        // One account nobody has ruled on outranks any number of deliberate
        // exclusions beside it: the answer is about an undecided part.
        assert_eq!(
            population.known_account_coverage(),
            KnownAccountCoverage::Undecided
        );
    }

    #[test]
    fn a_population_with_every_known_account_inside_is_whole() {
        let inside = AccountId::new_random();
        let definition =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [inside]);
        let accounts = vec![AccountView {
            id: inside,
            title: "Main".to_owned(),
            institution: None,
        }];

        let population = population_from(&definition, accounts, &BTreeSet::new(), &BTreeSet::new());

        assert_eq!(population.outside().count(), 0);
        assert_eq!(
            population.known_account_coverage(),
            KnownAccountCoverage::Whole
        );
    }

    #[test]
    fn a_population_whose_omissions_are_all_placed_is_bounded_not_undecided() {
        let inside = AccountId::new_random();
        let elsewhere = AccountId::new_random();
        let definition =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [inside]);
        let accounts = vec![
            AccountView {
                id: inside,
                title: "Main".to_owned(),
                institution: None,
            },
            AccountView {
                id: elsewhere,
                title: "Savings".to_owned(),
                institution: None,
            },
        ];
        let placed: BTreeSet<AccountId> = [elsewhere].into_iter().collect();

        let population = population_from(&definition, accounts, &placed, &BTreeSet::new());

        assert_eq!(population.undecided().count(), 0);
        assert_eq!(
            population.known_account_coverage(),
            KnownAccountCoverage::Bounded
        );
    }

    /// The bug this standing was added for. The owner ruled the account
    /// outside, in as many words and with a reason; before the disposition was
    /// read, the manifest still called it an account nobody had ruled on, and
    /// the register beside it still offered him the call he had already made.
    #[test]
    fn an_account_the_owner_ruled_outside_is_not_reported_as_one_nobody_ruled_on() {
        let inside = AccountId::new_random();
        let ruled_out = AccountId::new_random();
        let definition =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [inside]);
        let accounts = vec![
            AccountView {
                id: inside,
                title: "Main".to_owned(),
                institution: None,
            },
            AccountView {
                id: ruled_out,
                title: "Savings".to_owned(),
                institution: Some("Second Bank".to_owned()),
            },
        ];
        let ruled_outside: BTreeSet<AccountId> = [ruled_out].into_iter().collect();

        let population = population_from(&definition, accounts, &BTreeSet::new(), &ruled_outside);

        assert_eq!(
            population
                .outside()
                .map(|entry| (entry.account, entry.standing))
                .collect::<Vec<_>>(),
            vec![(ruled_out, AccountStanding::OutsideByDecision)]
        );
        assert_eq!(population.undecided().count(), 0);
        assert_eq!(
            population.known_account_coverage(),
            KnownAccountCoverage::Bounded
        );
        // The name and the bank travel with the identifier: this is the list
        // the owner is asked to rule on, and two accounts he calls one word are
        // one line apart in it.
        let entry = population.outside().next().expect("the ruled-out account");
        assert_eq!(entry.title, "Savings");
        assert_eq!(entry.institution.as_deref(), Some("Second Bank"));
        // And the register no longer offers him the call he already made.
        let kinds: Vec<_> = population
            .caveats()
            .iter()
            .map(|caveat| caveat.kind())
            .collect();
        assert_eq!(kinds, vec![CaveatKind::AccountRuledOutside]);
        assert!(
            !kinds
                .iter()
                .any(|kind| kind.closed_by().contains(&OperationKey::RecordAccountScope)),
            "the register still names the call the owner has already made"
        );
    }

    /// Membership and a disposition can both be recorded for one account —
    /// nothing clears the exclusion when the account joins a contour — and the
    /// report must then say what the outstanding-work queue says.
    #[test]
    fn membership_outranks_a_disposition_the_owner_never_withdrew() {
        let inside = AccountId::new_random();
        let elsewhere = AccountId::new_random();
        let definition =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [inside]);
        let accounts = vec![
            AccountView {
                id: inside,
                title: "Main".to_owned(),
                institution: None,
            },
            AccountView {
                id: elsewhere,
                title: "Savings".to_owned(),
                institution: None,
            },
        ];
        let both: BTreeSet<AccountId> = [elsewhere].into_iter().collect();

        let population = population_from(&definition, accounts, &both, &both);

        assert_eq!(
            population
                .outside()
                .map(|entry| entry.standing)
                .collect::<Vec<_>>(),
            vec![AccountStanding::OutsidePlacedElsewhere]
        );
    }

    #[test]
    fn every_standing_and_completeness_has_a_distinct_machine_readable_code() {
        // The wire distinguishes them or the reader cannot: two outside
        // standings sharing a code is the defect with a manifest bolted on.
        let standings = [
            AccountStanding::Covered,
            AccountStanding::OutsideByDecision,
            AccountStanding::OutsidePlacedElsewhere,
            AccountStanding::OutsideUndecided,
        ];
        let codes: BTreeSet<&str> = standings.iter().map(|standing| standing.code()).collect();
        assert_eq!(codes.len(), standings.len());
        assert!(!AccountStanding::Covered.is_outside());
        assert!(AccountStanding::OutsideByDecision.is_outside());
        assert!(AccountStanding::OutsidePlacedElsewhere.is_outside());
        assert!(AccountStanding::OutsideUndecided.is_outside());

        let completeness = [
            KnownAccountCoverage::Whole,
            KnownAccountCoverage::Bounded,
            KnownAccountCoverage::Undecided,
        ];
        let codes: BTreeSet<&str> = completeness.iter().map(|value| value.code()).collect();
        assert_eq!(codes.len(), completeness.len());
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
