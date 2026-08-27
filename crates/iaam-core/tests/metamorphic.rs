//! Метаморфные тесты (§15.6).
//!
//! Проверяют не значение, а **преобразование**: что должно измениться
//! и что обязано остаться прежним. Область применимости у каждого своя
//! и указана явно — метаморфное свойство без оговорки так же опасно,
//! как обычное (§15.3).

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::returns::{Computed, ReturnsReport, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceQuality};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

struct Ledger {
    owner: OwnerId,
    source: SourceId,
    sequence: u32,
    events: Vec<Event>,
}

impl Ledger {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            source: SourceId::new_random(),
            sequence: 0,
            events: Vec::new(),
        }
    }

    fn push(&mut self, account: AccountId, day: Date, kind: EventKind, legs: Vec<Leg>) {
        self.sequence += 1;
        self.events.push(Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, self.sequence),
            legs,
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&"5".repeat(64)).expect("хеш"),
                ParserVersion("meta/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        });
    }

    fn deposit(&mut self, account: AccountId, day: Date, minor: i64) {
        let amount = rub(minor);
        self.push(
            account,
            day,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
    }

    fn withdraw(&mut self, account: AccountId, day: Date, minor: i64) {
        let amount = rub(-minor);
        self.push(
            account,
            day,
            EventKind::CashOut { amount },
            vec![Leg::cash(account, amount)],
        );
    }

    fn buy(
        &mut self,
        account: AccountId,
        day: Date,
        instrument: InstrumentId,
        units: i64,
        gross: i64,
    ) {
        let custody = CustodyId::new_random();
        self.push(
            account,
            day,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(units),
                gross: rub(gross),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-gross)),
                Leg::security(account, custody, instrument, qty(units)),
            ],
        );
    }

    fn valuation(&mut self, account: AccountId, day: Date, instrument: InstrumentId, price: i64) {
        self.push(
            account,
            day,
            EventKind::Valuation {
                instrument,
                price: Dec::new(Decimal::from(price)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::PreviousClose,
            },
            vec![],
        );
    }
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn report(events: &[Event], accounts: &[AccountId], as_of: Date) -> ReturnsReport {
    let contour = ContourDefinition::new(
        ContourId::new_random(),
        ContourVersion(1),
        accounts.to_vec(),
    );
    let rules = RuleRegistry::with_defaults();
    let projection = project(
        events,
        &ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        },
    )
    .expect("проекция строится");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Сверка и периметр в этом тесте не участвуют: он проверяет расчёт,
    // а не подтверждение данных. Пустые реестр и оценка означают
    // «ничего не подтверждено», что для расчёта нейтрально.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    returns_report(
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
    )
}

fn rate_of(report: &ReturnsReport) -> f64 {
    match &report.xirr {
        Computed::Value(outcome) => outcome.rate().value(),
        Computed::NotComputable { reason } => {
            panic!("ставка не вычислена: {}", reason.code())
        }
    }
}

/// Область: всегда (§4.10). Счёт вне контура на доходность контура
/// не влияет — именно из-за нарушения этого правила чужие сервисы
/// показывают доходность, в которую попали чужие деньги.
#[test]
fn an_account_outside_the_contour_does_not_change_the_rate() {
    let inside = AccountId::new_random();
    let outside = AccountId::new_random();
    let instrument = InstrumentId::new_random();

    let mut ledger = Ledger::new();
    ledger.deposit(inside, date!(2025 - 01 - 01), 10_000_000);
    ledger.buy(inside, date!(2025 - 02 - 01), instrument, 100, 9_000_000);
    ledger.valuation(inside, date!(2026 - 01 - 01), instrument, 1_000);
    let base = report(&ledger.events, &[inside], date!(2026 - 01 - 01));

    // Тот же журнал плюс бурная деятельность на счёте вне контура.
    ledger.deposit(outside, date!(2025 - 03 - 01), 50_000_000);
    ledger.withdraw(outside, date!(2025 - 04 - 01), 20_000_000);
    let widened = report(&ledger.events, &[inside], date!(2026 - 01 - 01));

    assert!(
        (rate_of(&base) - rate_of(&widened)).abs() < 1e-12,
        "счёт вне контура изменил ставку"
    );
    assert_eq!(base.contributed, widened.contributed);
    assert_eq!(base.terminal_value, widened.terminal_value);
}

/// Область: инструменты без корпоративных действий и без правил,
/// зависящих от количества (минимальная комиссия, лот). На этапе 1
/// таких правил нет; при их появлении свойство перестанет выполняться,
/// и его придётся сузить, а не «починить».
#[test]
fn splitting_one_instrument_into_two_identical_halves_keeps_the_aggregates() {
    let account = AccountId::new_random();
    let single = InstrumentId::new_random();
    let first = InstrumentId::new_random();
    let second = InstrumentId::new_random();
    let as_of = date!(2026 - 01 - 01);

    let mut whole = Ledger::new();
    whole.deposit(account, date!(2025 - 01 - 01), 10_000_000);
    whole.buy(account, date!(2025 - 02 - 01), single, 100, 9_000_000);
    whole.valuation(account, as_of, single, 1_000);

    let mut halves = Ledger::new();
    halves.deposit(account, date!(2025 - 01 - 01), 10_000_000);
    halves.buy(account, date!(2025 - 02 - 01), first, 50, 4_500_000);
    halves.buy(account, date!(2025 - 02 - 01), second, 50, 4_500_000);
    halves.valuation(account, as_of, first, 1_000);
    halves.valuation(account, as_of, second, 1_000);

    let left = report(&whole.events, &[account], as_of);
    let right = report(&halves.events, &[account], as_of);

    assert_eq!(left.terminal_value, right.terminal_value);
    assert_eq!(left.contributed, right.contributed);
    assert!((rate_of(&left) - rate_of(&right)).abs() < 1e-12);
}

/// Область: **масштабная инвариантность ставки** при отключённых налогах,
/// порогах и минимальных комиссиях (§15.3). Линейные величины умножаются
/// на `k`, ставка не меняется. При появлении прогрессивной шкалы
/// свойство становится неверным.
#[test]
fn scaling_every_flow_scales_the_amounts_and_leaves_the_rate() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let as_of = date!(2026 - 01 - 01);
    let factor = 7;

    let mut plain = Ledger::new();
    plain.deposit(account, date!(2025 - 01 - 01), 10_000_000);
    plain.buy(account, date!(2025 - 02 - 01), instrument, 100, 9_000_000);
    plain.withdraw(account, date!(2025 - 08 - 01), 500_000);
    plain.valuation(account, as_of, instrument, 1_000);

    let mut scaled = Ledger::new();
    scaled.deposit(account, date!(2025 - 01 - 01), 10_000_000 * factor);
    scaled.buy(
        account,
        date!(2025 - 02 - 01),
        instrument,
        100 * factor,
        9_000_000 * factor,
    );
    scaled.withdraw(account, date!(2025 - 08 - 01), 500_000 * factor);
    // Цена за единицу не масштабируется: масштабируется количество.
    scaled.valuation(account, as_of, instrument, 1_000);

    let left = report(&plain.events, &[account], as_of);
    let right = report(&scaled.events, &[account], as_of);

    assert!(
        (rate_of(&left) - rate_of(&right)).abs() < 1e-9,
        "ставка изменилась при масштабировании: {} против {}",
        rate_of(&left),
        rate_of(&right)
    );
    let scaled_contribution = left
        .contributed
        .value()
        .expect("внесено")
        .checked_mul(Dec::new(Decimal::from(factor)))
        .expect("умножение");
    assert_eq!(right.contributed.value(), Some(&scaled_contribution));
}
