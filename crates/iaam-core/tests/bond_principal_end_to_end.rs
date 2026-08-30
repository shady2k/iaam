//! Сквозная проверка: номинал доезжает до расчёта обычным путём.
//!
//! Тест намеренно не подменяет CBOR-поля состояния. Прежние хелперы
//! подставляли номинал мимо рабочего пути, и потому вся линия E3.4
//! годами проверялась на данных, которых рабочий код никогда не увидит.

use std::collections::BTreeMap;

use iaam_core::bond::{AccrualPeriod, BondSchedule, DefaultFlags, PrincipalReturn};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, SettledDate, TradeDate};
use iaam_core::event::allocation::{
    AllocationAlgorithmVersion, AllocationEvidence, AllocationInputsHash, BasisAllocation,
};
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterAssessment, PerimeterPolicy};
use iaam_core::projection::lots::BasisGap;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::ReconciliationLedger;
use iaam_core::returns::{Computed, KnowledgeCoordinate, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, ReturnedShare, RuleRegistry};
use iaam_core::valuation::{
    PriceCandidate, PriceKind, PriceOrigin, QuotationBasis, SourceExecutability, Venue,
};
use rust_decimal::Decimal;
use time::OffsetDateTime;
use time::macros::date;
use uuid::Uuid;

const OWNER: OwnerId = OwnerId(Uuid::from_u128(1));
const ACCOUNT: AccountId = AccountId(Uuid::from_u128(2));
const INSTRUMENT: InstrumentId = InstrumentId(Uuid::from_u128(3));
const CUSTODY: CustodyId = CustodyId(Uuid::from_u128(4));
const SOURCE: SourceId = SourceId(Uuid::from_u128(5));

fn dec(text: &str) -> Dec {
    Dec::new(Decimal::from_str_exact(text).expect("десятичная константа"))
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn per_unit(text: &str) -> PerUnitAmount {
    PerUnitAmount::new(dec(text), CurrencyCode::Rub)
}

fn event(day: time::Date, sequence: u32, kind: EventKind, legs: Vec<Leg>) -> Event {
    Event {
        id: EventId(Uuid::from_u128(u128::from(sequence))),
        schema_version: SCHEMA_VERSION,
        owner: OWNER,
        account: ACCOUNT,
        kind,
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs,
        provenance: Provenance::new(
            SOURCE,
            RawHash::parse(&"b".repeat(64)).expect("шестнадцатеричный хеш"),
            ParserVersion("test/bond-principal-e2e/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn buy(day: time::Date, sequence: u32, gross: i64) -> Event {
    let quantity = Quantity(dec("10"));
    let cost = rub(gross);
    let mut event = event(
        day,
        sequence,
        EventKind::Trade {
            side: TradeSide::Buy,
            instrument: INSTRUMENT,
            quantity,
            gross: cost,
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(
                ACCOUNT,
                Money::new(PostedMinor::new(-gross), CurrencyCode::Rub),
            ),
            Leg::security(ACCOUNT, CUSTODY, INSTRUMENT, quantity),
        ],
    );
    event.dates.settled = Some(SettledDate(day));
    event.dates.trade = Some(TradeDate(day));
    event
}
fn sell(day: time::Date, sequence: u32, gross: i64) -> Event {
    let quantity = Quantity(dec("10"));
    let proceeds = rub(gross);
    let mut event = event(
        day,
        sequence,
        EventKind::Trade {
            side: TradeSide::Sell,
            instrument: INSTRUMENT,
            quantity,
            gross: proceeds,
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(ACCOUNT, proceeds),
            Leg::security(ACCOUNT, CUSTODY, INSTRUMENT, Quantity(dec("-10"))),
        ],
    );
    event.dates.settled = Some(SettledDate(day));
    event.dates.trade = Some(TradeDate(day));
    event
}

fn cash_in(day: time::Date, sequence: u32, amount: i64) -> Event {
    let money = rub(amount);
    event(
        day,
        sequence,
        EventKind::CashIn { amount: money },
        vec![Leg::cash(ACCOUNT, money)],
    )
}

fn known_allocation(share: &str) -> BasisAllocation {
    BasisAllocation::Known {
        share: ReturnedShare::new(dec(share)).expect("доля в пределах единицы"),
        evidence: AllocationEvidence {
            inputs_hash: AllocationInputsHash::new("c".repeat(64)).expect("хеш входов"),
            knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
            algorithm_version: AllocationAlgorithmVersion(1),
        },
    }
}

fn partial_redemption(
    day: time::Date,
    sequence: u32,
    quantity: &str,
    principal_returned_per_unit: &str,
    compensation_minor: i64,
    allocation: BasisAllocation,
) -> Event {
    let compensation = rub(compensation_minor);
    event(
        day,
        sequence,
        EventKind::CorporateAction {
            action: CorporateAction::PartialRedemption {
                instrument: INSTRUMENT,
                custody: CUSTODY,
                quantity: Quantity(dec(quantity)),
                principal_returned_per_unit: per_unit(principal_returned_per_unit),
                compensation,
                effective_date: day,
                record_date: None,
                grounds: Some("амортизационная выплата".to_owned()),
                basis_allocation: allocation,
            },
        },
        vec![Leg::principal(ACCOUNT, INSTRUMENT, compensation)],
    )
}

fn schedule_with_principal() -> BondSchedule {
    BondSchedule {
        periods: vec![AccrualPeriod {
            period_start: date!(2026 - 01 - 01),
            accrual_end: date!(2026 - 07 - 01),
            payment_date: date!(2026 - 07 - 01),
            record_date: Some(date!(2026 - 07 - 01)),
            coupon_per_unit: Some(per_unit("50")),
        }],
        principal_returns: vec![PrincipalReturn {
            repayment_date: date!(2026 - 07 - 01),
            share_percent: dec("100"),
        }],
        initial_principal: Some(per_unit("1000")),
        offer_windows: Vec::new(),
        completeness: iaam_core::bond::offer::ScheduleCompleteness::Validated,
        default_flags: Some(DefaultFlags {
            declared: false,
            technical: false,
        }),
        currency_roles: Some(CurrencyRoles::uniform(CurrencyCode::Rub)),
    }
}

fn context<'a>(contour: &'a ContourDefinition, rules: &'a RuleRegistry) -> ProjectionContext<'a> {
    ProjectionContext {
        contour,
        rules,
        lot_rule: LotRuleVersion(1),
    }
}

fn market_price(day: time::Date) -> PriceCandidate {
    PriceCandidate {
        instrument: INSTRUMENT,
        price: dec("1000"),
        currency: CurrencyCode::Rub,
        basis: QuotationBasis::MoneyPerUnit,
        basis_evidence: "test:bond-principal-e2e".to_owned(),
        basis_evidence_contradicts: false,
        trade_date: day.previous_day().expect("дата до отчёта"),
        observed_at: None,
        origin: PriceOrigin::Market {
            venue: Venue {
                board: "TQBR".to_owned(),
                session: 0,
            },
            kind: PriceKind::LegalClose,
        },
        executability: SourceExecutability::Executable,
    }
}

#[test]
fn a_bond_from_journal_and_catalog_has_computable_metrics_without_state_override() {
    let as_of = date!(2026 - 03 - 01);
    let contour =
        ContourDefinition::new(ContourId(Uuid::from_u128(6)), ContourVersion(1), [ACCOUNT]);
    let rules = RuleRegistry::with_defaults();
    let events = vec![
        cash_in(date!(2026 - 01 - 02), 1, 2_000_000),
        buy(date!(2026 - 01 - 03), 2, 1_000_000),
    ];
    let projection = project(&events, &context(&contour, &rules)).expect("проекция журнала");
    let entry = projection
        .state()
        .book()
        .entry(&iaam_core::projection::lots::LotKey {
            account: ACCOUNT,
            instrument: INSTRUMENT,
        })
        .expect("партия из обычной покупки");
    assert_eq!(projection.state().book().iter().count(), 1);

    assert_eq!(entry.unpriced(), Quantity(dec("0")));
    assert_eq!(entry.lots().len(), 1);
    assert_eq!(entry.released_basis(), None);

    let fx = iaam_core::valuation::FxTable::new(iaam_core::valuation::FxSource::OwnerSupplied);
    let ledger = ReconciliationLedger::default();
    let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
    let schedule = schedule_with_principal();
    let schedules = BTreeMap::from([(INSTRUMENT, schedule)]);
    let report = returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &contour,
            as_of,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&market_price(as_of)),
            bond_schedules: &schedules,
            accrued_observations: &BTreeMap::new(),
        },
    );

    assert_eq!(report.bond_metrics.len(), 1);
    assert!(matches!(
        report.bond_metrics[0].scenarios[0].prospective.metrics,
        Computed::Value(_)
    ));
}

#[test]
fn a_return_after_thirty_percent_prior_amortisation_releases_one_seventh_of_remaining_basis() {
    let day = date!(2026 - 08 - 01);
    let contour =
        ContourDefinition::new(ContourId(Uuid::from_u128(7)), ContourVersion(1), [ACCOUNT]);
    let rules = RuleRegistry::with_defaults();
    let events = vec![
        buy(date!(2026 - 01 - 01), 1, 1_000_000),
        partial_redemption(
            date!(2026 - 02 - 01),
            2,
            "10",
            "300",
            300_000,
            known_allocation("0.3"),
        ),
        sell(date!(2026 - 05 - 01), 3, 700_000),
        buy(date!(2026 - 06 - 01), 4, 700_000),
        // Возврат 10% первоначального номинала после погашения 30%:
        // приложение передало долю 10/70 = 1/7 в самом событии.
        partial_redemption(
            day,
            5,
            "10",
            "100",
            100_000,
            known_allocation("0.1428571428571428571428571429"),
        ),
    ];
    let before_current =
        project(&events[..4], &context(&contour, &rules)).expect("состояние до текущего возврата");
    let before_entry = before_current
        .state()
        .book()
        .entry(&iaam_core::projection::lots::LotKey {
            account: ACCOUNT,
            instrument: INSTRUMENT,
        })
        .expect("новая партия до текущего возврата");
    assert_eq!(before_entry.lots().len(), 1);
    assert_eq!(before_entry.lots()[0].cost_basis, rub(700_000));

    let projection = project(&events, &context(&contour, &rules)).expect("проекция амортизации");
    let key = iaam_core::projection::lots::LotKey {
        account: ACCOUNT,
        instrument: INSTRUMENT,
    };
    let entry = projection
        .state()
        .book()
        .entry(&key)
        .expect("новая партия после покупки");

    // `released_basis` накопительный: 1 000 000 от ранней покупки и её
    // продажи плюс 100 000 от текущего возврата. Эффект 1/7 проверяется
    // на самой новой партии: 700 000 / 7 = 100 000.
    assert_eq!(entry.released_basis(), Some(rub(1_100_000)));
    assert_eq!(entry.lots().len(), 1);
    assert_eq!(
        entry.lots()[0].cost_basis,
        rub(600_000),
        "новая партия: 700 000 − 700 000 / 7, а не расчёт от номинала 1 000 000"
    );
    assert_eq!(entry.gap(), None);
}

#[test]
fn an_amortisation_without_schedule_is_applied_as_unknown_not_zero() {
    let day = date!(2026 - 08 - 02);
    let contour =
        ContourDefinition::new(ContourId(Uuid::from_u128(8)), ContourVersion(1), [ACCOUNT]);
    let rules = RuleRegistry::with_defaults();
    let events = vec![
        buy(day, 1, 1_000_000),
        partial_redemption(
            day,
            2,
            "10",
            "100",
            100_000,
            BasisAllocation::Unknown(iaam_core::event::allocation::AllocationGap::ScheduleMissing),
        ),
    ];
    let projection =
        project(&events, &context(&contour, &rules)).expect("деньги нельзя отвергнуть");
    let key = iaam_core::projection::lots::LotKey {
        account: ACCOUNT,
        instrument: INSTRUMENT,
    };
    let entry = projection
        .state()
        .book()
        .entry(&key)
        .expect("партия после покупки");

    assert_eq!(entry.gap(), Some(BasisGap::AmortisationAllocationUnknown));
    assert_eq!(entry.released_basis(), None);
    assert_eq!(entry.unpriced(), Quantity(dec("0")));
    assert_eq!(entry.lots().len(), 1);
    assert_eq!(entry.lots()[0].cost_basis, rub(1_000_000));
    assert_eq!(entry.lots()[0].received_to_date, Some(rub(100_000)));
}
