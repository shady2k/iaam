//! Золотые сценарии этапа 1 (§15.9).
//!
//! Из обязательного набора спеки здесь те сценарии, которые этап 1
//! обязан отработать. Остальные — амортизация, ЛДВ, замещающие
//! облигации, налог прошлого периода — относятся к E3 и E5 и появятся
//! там вместе с механикой, которую проверяют. Пропуск не тихий:
//! каждый отсутствующий сценарий назван в конце файла.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId, TransferId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::lots::LotKey;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::returns::{MaterialIssue, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceQuality};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

struct World {
    owner: OwnerId,
    account: AccountId,
    other: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    source: SourceId,
    sequence: u32,
}

impl World {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            other: AccountId::new_random(),
            custody: CustodyId::new_random(),
            instrument: InstrumentId::new_random(),
            source: SourceId::new_random(),
            sequence: 0,
        }
    }

    fn event(&mut self, day: Date, kind: EventKind, legs: Vec<Leg>) -> Event {
        self.sequence += 1;
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, self.sequence),
            legs,
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&"9".repeat(64)).expect("хеш"),
                ParserVersion("golden/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn deposit(&mut self, day: Date, minor: i64) -> Event {
        let amount = rub(minor);
        let account = self.account;
        self.event(
            day,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        )
    }

    fn buy(&mut self, day: Date, units: i64, gross_minor: i64) -> Event {
        let (account, custody, instrument) = (self.account, self.custody, self.instrument);
        let gross = rub(gross_minor);
        self.event(
            day,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(units),
                gross,
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-gross_minor)),
                Leg::security(account, custody, instrument, qty(units)),
            ],
        )
    }

    fn sell(&mut self, day: Date, units: i64, gross_minor: i64) -> Event {
        let (account, custody, instrument) = (self.account, self.custody, self.instrument);
        let gross = rub(gross_minor);
        self.event(
            day,
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(units),
                gross,
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, gross),
                Leg::security(account, custody, instrument, qty(-units)),
            ],
        )
    }

    fn valuation(&mut self, day: Date, price: i64) -> Event {
        let instrument = self.instrument;
        self.event(
            day,
            EventKind::Valuation {
                instrument,
                price: Dec::new(Decimal::from(price)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::PreviousClose,
            },
            vec![],
        )
    }

    fn transfer_inside(&mut self, day: Date, minor: i64) -> Event {
        let (from, to) = (self.account, self.other);
        let amount = rub(minor);
        self.event(
            day,
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount,
            },
            vec![Leg::cash(from, rub(-minor)), Leg::cash(to, amount)],
        )
    }

    fn opening_position(&mut self, day: Date, units: i64) -> Event {
        let (account, custody, instrument) = (self.account, self.custody, self.instrument);
        self.event(
            day,
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(units),
                cost_basis: None,
                assertions: iaam_core::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(account, custody, instrument, qty(units))],
        )
    }
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn dec(value: i64) -> Dec {
    Dec::new(Decimal::from(value))
}

fn report_of(world: &World, events: &[Event], both_accounts: bool, as_of: Date) -> ReportPair {
    let accounts: Vec<AccountId> = if both_accounts {
        vec![world.account, world.other]
    } else {
        vec![world.account]
    };
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), accounts);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(events, &ctx).expect("проекция строится");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Сверка и периметр в этом тесте не участвуют: он проверяет расчёт,
    // а не подтверждение данных. Пустые реестр и оценка означают
    // «ничего не подтверждено», что для расчёта нейтрально.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let report = returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &contour,
            coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
            as_of,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &std::collections::BTreeMap::new(),
            accrued_observations: &std::collections::BTreeMap::new(),
        },
    );
    ReportPair {
        report,
        projection: Box::new(projection),
    }
}

struct ReportPair {
    report: iaam_core::returns::ReturnsReport,
    projection: Box<iaam_core::projection::Projection>,
}

/// §15.9: перевод между счетами внутри контура — XIRR не меняется.
#[test]
fn a_transfer_inside_the_contour_does_not_change_the_rate() {
    let mut world = World::new();
    let base = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        world.valuation(date!(2026 - 01 - 01), 1),
    ];
    let mut with_transfer = base.clone();
    with_transfer.push(world.transfer_inside(date!(2025 - 06 - 01), 3_000_000));

    let without = report_of(&world, &base, true, date!(2026 - 01 - 01));
    let with = report_of(&world, &with_transfer, true, date!(2026 - 01 - 01));

    let left = without.report.xirr.value().expect("ставка без перевода");
    let right = with.report.xirr.value().expect("ставка с переводом");
    assert!(
        (left.rate().value() - right.rate().value()).abs() < 1e-12,
        "перевод внутри контура изменил ставку: {} против {}",
        left.rate().value(),
        right.rate().value()
    );
    assert_eq!(with.projection.state().flows().internal(), 1);
    assert_eq!(with.report.contributed.value(), Some(&dec(100_000)));
}

/// §15.9: продажа части позиции — списание стоимости и перенос
/// нереализованного результата в реализованный.
#[test]
fn a_partial_sale_releases_basis_and_realizes_result() {
    let mut world = World::new();
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        // 100 бумаг за 9 000 рублей: по 90 рублей за бумагу.
        world.buy(date!(2025 - 02 - 01), 100, 900_000),
        // 40 бумаг проданы за 4 000 рублей: списанная стоимость
        // 9 000 × 40 / 100 = 3 600, реализовано 4 000 − 3 600 = 400.
        world.sell(date!(2025 - 09 - 01), 40, 400_000),
        world.valuation(date!(2026 - 01 - 01), 100),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    let key = LotKey {
        account: world.account,
        instrument: world.instrument,
    };
    let entry = pair
        .projection
        .state()
        .book()
        .entry(&key)
        .expect("книга лотов");

    assert_eq!(entry.quantity().unwrap(), qty(60));
    assert_eq!(entry.released_basis(), Some(rub(360_000)));
    assert_eq!(entry.realized(), Some(rub(40_000)));
    assert_eq!(entry.remaining_basis().unwrap(), Some(rub(540_000)));
    // Деньги: 100 000 − 9 000 + 4 000 = 95 000; бумаги: 60 × 100 = 6 000.
    assert_eq!(pair.report.terminal_value.value(), Some(&dec(101_000)));
}

/// §15.9: отрицательный денежный остаток — обязательство в стоимости,
/// а не исчезнувшая величина.
#[test]
fn negative_cash_lowers_the_terminal_value_and_is_reported() {
    let mut world = World::new();
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 1_000_000),
        world.buy(date!(2025 - 02 - 01), 100, 1_200_000),
        world.valuation(date!(2026 - 01 - 01), 100),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));

    assert_eq!(
        pair.projection
            .state()
            .balances()
            .cash(world.account, CurrencyCode::Rub),
        Some(rub(-200_000))
    );
    // −2 000 рублей денег плюс 100 бумаг по 100 = 8 000.
    assert_eq!(pair.report.terminal_value.value(), Some(&dec(8_000)));
    assert!(
        pair.report
            .data_quality
            .material_issues
            .iter()
            .any(|issue| matches!(issue, MaterialIssue::NegativeCash { .. })),
        "отрицательный остаток обязан попасть в блок качества данных"
    );
}

/// §15.9: частичная история без налоговой стоимости — `not_computable`
/// вместо выдуманной цифры.
#[test]
fn a_restored_position_without_basis_makes_the_realized_result_not_computable() {
    let mut world = World::new();
    let events = vec![
        world.opening_position(date!(2024 - 01 - 01), 50),
        world.deposit(date!(2025 - 01 - 01), 1_000_000),
        world.sell(date!(2025 - 06 - 01), 20, 300_000),
        world.valuation(date!(2026 - 01 - 01), 150),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    let key = LotKey {
        account: world.account,
        instrument: world.instrument,
    };
    let entry = pair
        .projection
        .state()
        .book()
        .entry(&key)
        .expect("книга лотов");

    assert_eq!(entry.unpriced(), qty(30));
    assert_eq!(
        entry.realized(),
        None,
        "прибыль от продажи бумаги неизвестной стоимости не вычисляется"
    );
    // Стоимость позиции при этом известна: 30 бумаг по 150 = 4 500,
    // плюс 10 000 + 3 000 денег.
    assert_eq!(pair.report.terminal_value.value(), Some(&dec(17_500)));
    assert!(
        pair.report
            .data_quality
            .material_issues
            .iter()
            .any(|issue| matches!(issue, MaterialIssue::RestoredWithoutBasis { .. }))
    );
}

/// §15.9: две одинаковые сделки в один день — обе учтены.
#[test]
fn two_identical_purchases_on_the_same_day_are_both_projected() {
    let mut world = World::new();
    let day = date!(2025 - 03 - 03);
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        world.buy(day, 10, 100_000),
        world.buy(day, 10, 100_000),
        world.valuation(date!(2026 - 01 - 01), 100),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    let key = LotKey {
        account: world.account,
        instrument: world.instrument,
    };
    let entry = pair.projection.state().book().entry(&key).expect("книга");
    assert_eq!(entry.lots().len(), 2, "две сделки — две партии");
    assert_eq!(entry.quantity().unwrap(), qty(20));
}

/// §15.9: цена без оценки — отчёт отказывается называть стоимость.
#[test]
fn a_position_without_a_price_makes_the_terminal_value_not_computable() {
    let mut world = World::new();
    let events = vec![
        world.deposit(date!(2025 - 01 - 01), 10_000_000),
        world.buy(date!(2025 - 02 - 01), 100, 900_000),
    ];
    let pair = report_of(&world, &events, false, date!(2026 - 01 - 01));
    assert_eq!(
        pair.report.terminal_value.reason().map(|r| r.code()),
        Some("missing_price")
    );
    assert_eq!(
        pair.report.xirr.reason().map(|r| r.code()),
        Some("missing_price"),
        "ставка без стоимости не определена и не подменяется нулём"
    );
}

// Сценарии §15.9, НЕ реализованные на этапе 1, и их адрес:
//   облигация с амортизацией ............................ E3
//   вклад с капитализацией и досрочным расторжением ..... E3
//   лот, доживший до ЛДВ ................................ E5
//   перевод бумаг между брокерами ....................... E3
//   доудержанный в январе налог за прошлый год .......... E5
//   возврат излишне удержанного налога .................. E5
//   дивиденд в валюте с разложением FX .................. E4
//   замещающая облигация ................................ E3
//   оферта без предъявления ............................. E3
//   сплит с дробным остатком ............................ E3
//   делистинг ........................................... E3
//   компенсирующая ошибка парсера ....................... E2
