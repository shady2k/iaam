//! One instrument, one date, two reports — and one price behind both.
//!
//! The asset snapshot and the returns report both publish a figure for a
//! holding. They once reached it by different means: the returns report ran the
//! versioned valuation policy over the journal's board **and** the market
//! store, while the snapshot read the journal's board and nothing else. Two
//! answers about the same instrument on the same day, and nothing in either
//! report to say which the owner should believe.
//!
//! These tests are the statement that there is now one answer. They do not
//! check that the two figures happen to be equal — a coincidence of arithmetic
//! would satisfy that. They check that the **observation** behind them is the
//! same one: the same price, the same currency, the same trade date, the same
//! origin, and the same policy rationale, compared as one value. A future
//! change to either path that reintroduces a selection of its own breaks these,
//! because two selections cannot produce one `SelectedPrice`.

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{EventDates, TradeDate};
use iaam_core::event::Event;
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterAssessment, PerimeterPolicy};
use iaam_core::projection::state::LedgerState;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::ReconciliationLedger;
use iaam_core::report::assets::{SnapshotPrices, asset_snapshot};
use iaam_core::report::balances::{
    AccountBalanceRow, AccountCash, BalancesReport, CashOpening, PeriodReports,
};
use iaam_core::report::population::{AccountStanding, PopulationAccount, ReportPopulation};
use iaam_core::returns::{KnowledgeCoordinate, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{
    FxSource, FxTable, PriceCandidate, PriceDecision, PriceKind, PriceOrigin, PriceQuality,
    QuotationBasis, SelectedPrice, SourceExecutability, Venue,
};
use rust_decimal::Decimal;
use time::macros::{date, datetime};
use time::{Date, OffsetDateTime};

mod support;
use support::{Posting, TestChannel, event_on};

const AS_OF: Date = date!(2026 - 01 - 31);
const KNOWN_AT: OffsetDateTime = datetime!(2026 - 02 - 01 09:00 UTC);
const UNITS: i64 = 40;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

/// One owner, one brokerage account, one instrument bought and still held.
struct Fixture {
    owner: OwnerId,
    account: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    contour: ContourDefinition,
    channel: TestChannel,
}

impl Fixture {
    fn new() -> Self {
        let account = AccountId::new_random();
        Self {
            owner: OwnerId::new_random(),
            account,
            custody: CustodyId::new_random(),
            instrument: InstrumentId::new_random(),
            contour: ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]),
            channel: TestChannel::new("agreement/1", "one-price-two-reports"),
        }
    }

    /// A deposit and a purchase. Deliberately **no** `Valuation` event: the
    /// owner has entered no price of his own, which is the state in which the
    /// snapshot used to have nothing to say.
    fn journal(&self) -> Vec<Event> {
        let deposit = rub(500_000);
        let gross = rub(400_000);
        vec![
            event_on(
                &self.channel,
                Posting {
                    owner: self.owner,
                    account: self.account,
                    day: date!(2026 - 01 - 05),
                    sequence: 1,
                },
                EventKind::CashIn { amount: deposit },
                vec![Leg::cash(self.account, deposit)],
            ),
            Event {
                dates: EventDates::for_trade(TradeDate(date!(2026 - 01 - 12)), None),
                ..event_on(
                    &self.channel,
                    Posting {
                        owner: self.owner,
                        account: self.account,
                        day: date!(2026 - 01 - 12),
                        sequence: 2,
                    },
                    EventKind::Trade {
                        side: TradeSide::Buy,
                        instrument: self.instrument,
                        quantity: qty(UNITS),
                        gross,
                        fee: None,
                        accrued_interest: None,
                        basis_fee: None,
                        basis_fee_exact: None,
                    },
                    vec![
                        Leg::cash(self.account, rub(-400_000)),
                        Leg::security(self.account, self.custody, self.instrument, qty(UNITS)),
                    ],
                )
            },
        ]
    }

    /// A price the owner entered himself, for the case where both channels have
    /// something to say.
    fn owner_valuation(&self, minor: i64, day: Date) -> Event {
        event_on(
            &self.channel,
            Posting {
                owner: self.owner,
                account: self.account,
                day,
                sequence: 3,
            },
            EventKind::Valuation {
                instrument: self.instrument,
                price: Dec::new(Decimal::from(minor)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::PreviousClose,
            },
            vec![],
        )
    }

    fn market_quote(&self, minor: i64, trade_date: Date) -> PriceCandidate {
        PriceCandidate {
            instrument: self.instrument,
            price: Dec::new(Decimal::from(minor)),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::MoneyPerUnit,
            basis_evidence: "market:board".to_owned(),
            basis_evidence_contradicts: false,
            trade_date,
            observed_at: Some(datetime!(2026 - 01 - 30 18:45 UTC)),
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
}

/// The coordinate both reports are asked at. One value, used twice: asking the
/// two reports at two coordinates would let them disagree honestly, and prove
/// nothing about whether they can disagree dishonestly.
fn coordinate() -> KnowledgeCoordinate {
    KnowledgeCoordinate {
        knowledge_as_of: KNOWN_AT,
        source_priority_version: 1,
        valuation_policy_version: 1,
    }
}

/// The balances answer, built from the same projection the returns report reads.
///
/// The application scenario assembles this from the store; here it is assembled
/// from the projection directly, because what these tests are about is the
/// price, and both reports must be looking at the same positions for the
/// comparison to mean anything.
fn balances_from(state: &LedgerState, fixture: &Fixture) -> BalancesReport {
    let cash: Vec<AccountCash> = state
        .balances()
        .iter_cash()
        .filter(|(account, _)| *account == fixture.account)
        .map(|(_, money)| AccountCash {
            money,
            opening: CashOpening::Asserted,
        })
        .collect();
    let positions = state
        .balances()
        .iter_positions()
        .filter(|(key, _)| key.account == fixture.account)
        .map(|(key, quantity)| (*key, quantity))
        .collect();
    BalancesReport {
        accounts: vec![AccountBalanceRow {
            account: fixture.account,
            cash,
            reconciliation: Vec::new(),
            positions,
            period_reports: PeriodReports::Calculated,
        }],
        negative_cash: Vec::new(),
        population: ReportPopulation {
            contour: fixture.contour.id(),
            version: fixture.contour.version(),
            retirement_revision: iaam_core::retirement::RetirementRevision::NONE,
            accounts: vec![PopulationAccount {
                account: fixture.account,
                title: "Brokerage".to_owned(),
                institution: None,
                standing: AccountStanding::Covered,
                retirement: None,
            }],
        },
    }
}

/// The price each report reached for the one instrument held.
///
/// Returns the pair so that every assertion below compares the two halves of
/// one call, and no test can accidentally compare a report against itself.
fn both_prices(
    fixture: &Fixture,
    extra_events: Vec<Event>,
    market: &[PriceCandidate],
) -> (
    PriceDecision,
    Vec<SelectedPrice>,
    iaam_core::report::assets::AssetSnapshot,
) {
    let mut events = fixture.journal();
    events.extend(extra_events);
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &fixture.contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(&events, &context).expect("projection builds");
    let state = projection.state();

    let schedules = BTreeMap::new();
    let report = balances_from(state, fixture);
    let snapshot = asset_snapshot(
        AS_OF,
        &report,
        &BTreeMap::new(),
        SnapshotPrices {
            board: state.prices(),
            market,
            schedules: &schedules,
            coordinate: coordinate(),
        },
    )
    .expect("snapshot folds");

    // The report currency is the currency the quote is already in, so the rate
    // is the identity and no FX table is needed. That is not a convenience of
    // the test: it is the property the snapshot depends on — the selection is
    // reached before any conversion, and the snapshot has no rate to offer.
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let ledger = ReconciliationLedger::default();
    let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
    let returns = returns_report(
        state,
        &ReturnsRequest {
            contour: &fixture.contour,
            coordinate: coordinate(),
            as_of: AS_OF,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: market,
            bond_schedules: &BTreeMap::new(),
            accrued_observations: &BTreeMap::new(),
        },
    );

    let holding = snapshot
        .positions
        .holdings
        .iter()
        .find(|holding| holding.instrument == fixture.instrument)
        .expect("the holding is listed");
    let returns_prices: Vec<SelectedPrice> = returns
        .data_quality
        .position_coverage
        .selected
        .iter()
        .filter(|position| position.instrument == fixture.instrument)
        .map(|position| position.price.clone())
        .collect();
    (holding.price.clone(), returns_prices, snapshot.clone())
}

/// The deliverable. One instrument, one date, both reports asked: the
/// observation behind the snapshot's holding value **is** the observation
/// behind the returns report's valuation — the same figure, from the same
/// source, with the same trade date, carrying the same policy rationale.
///
/// Compared as one value rather than field by field: a field added to
/// `SelectedPrice` tomorrow is covered by this assertion the day it is added,
/// and a second selection path cannot produce a value equal to the first's.
#[test]
fn the_two_reports_publish_one_observation_for_one_instrument() {
    let fixture = Fixture::new();
    let market = [fixture.market_quote(300, date!(2026 - 01 - 30))];
    let (snapshot_price, returns_prices, snapshot) = both_prices(&fixture, Vec::new(), &market);

    let returns_price = match returns_prices.as_slice() {
        [only] => only,
        other => panic!(
            "exactly one valued position was expected, got {}",
            other.len()
        ),
    };
    let selected = snapshot_price
        .selected()
        .expect("the snapshot valued the holding");

    assert_eq!(
        selected, returns_price,
        "the two reports must rest on one observation, not on two that agree"
    );

    // And the figure the snapshot published is that observation applied to the
    // quantity — so the equality above is about the number the owner reads, not
    // about a field beside it.
    let value = snapshot
        .positions
        .holdings
        .iter()
        .find(|holding| holding.instrument == fixture.instrument)
        .and_then(|holding| holding.value)
        .expect("valued");
    assert_eq!(value.value(), Dec::new(Decimal::from(300 * UNITS)));
    assert_eq!(value.currency(), CurrencyCode::Rub);
}

/// The pre-fix failure, made permanent. Before the shared selection, the
/// snapshot read the journal's board alone: with no `Valuation` event, this
/// holding was unvalued and its instrument was named in a caveat, while the
/// returns report valued it from the market store on the same date.
///
/// This is the owner's complaint stated as a test — «my securities half is
/// caveats» — and it fails against the old snapshot.
#[test]
fn a_market_quote_reaches_the_snapshot_as_it_reaches_the_returns_report() {
    let fixture = Fixture::new();
    let market = [fixture.market_quote(300, date!(2026 - 01 - 30))];
    let (snapshot_price, returns_prices, snapshot) = both_prices(&fixture, Vec::new(), &market);

    assert!(
        snapshot_price.selected().is_some(),
        "the journal holds no price at all; only the market channel can cover this"
    );
    assert_eq!(returns_prices.len(), 1);
    assert!(
        snapshot.confidence().complete(),
        "no caveat remains: {:?}",
        snapshot.confidence().caveats()
    );
}

/// The selection is a ranking, not a preference for whichever channel a report
/// happens to consult. With both channels holding a price for the same day, the
/// two reports must not only agree with each other — they must agree on the
/// candidate the policy ranks first, which is the market observation.
#[test]
fn both_reports_rank_the_same_candidate_first_when_both_channels_speak() {
    let fixture = Fixture::new();
    let market = [fixture.market_quote(300, AS_OF)];
    let (snapshot_price, returns_prices, _) =
        both_prices(&fixture, vec![fixture.owner_valuation(275, AS_OF)], &market);

    let selected = snapshot_price.selected().expect("valued");
    assert_eq!(&returns_prices[0], selected);
    assert_eq!(selected.candidate.price, Dec::new(Decimal::from(300)));
    assert!(
        matches!(selected.candidate.origin, PriceOrigin::Market { .. }),
        "origin priority decides, not which channel the report read first"
    );
}

/// Broadening the price source must not turn «I do not know» into a number.
/// Where the policy selects nothing, the snapshot leaves the holding out of the
/// total and keeps its caveat — and the returns report leaves the same position
/// out of its own coverage, for the same reason.
#[test]
fn where_the_policy_selects_nothing_neither_report_invents_a_figure() {
    let fixture = Fixture::new();
    // An observation the reader does not yet know of at the report's
    // coordinate: present in the store, and correctly invisible to both.
    let mut unknown = fixture.market_quote(300, AS_OF);
    unknown.observed_at = Some(datetime!(2026 - 02 - 02 18:45 UTC));
    let (snapshot_price, returns_prices, snapshot) = both_prices(&fixture, Vec::new(), &[unknown]);

    assert!(matches!(snapshot_price, PriceDecision::Uncovered(_)));
    assert!(returns_prices.is_empty());

    let holding = snapshot
        .positions
        .holdings
        .iter()
        .find(|holding| holding.instrument == fixture.instrument)
        .expect("still listed");
    assert!(holding.value.is_none(), "absent from the total, never zero");
    assert!(snapshot.positions.totals.is_empty());
    let caveats: BTreeSet<&'static str> = snapshot
        .confidence()
        .caveats()
        .iter()
        .map(|caveat| caveat.kind().code())
        .collect();
    assert!(caveats.contains("holding_not_valued"));
}
