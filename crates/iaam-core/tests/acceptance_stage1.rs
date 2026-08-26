//! Приёмка этапа 1 (§16.3): по одному счёту с ручным вводом система
//! отвечает, сколько внесено, сколько выведено и какова доходность
//! до налога.
//!
//! Ожидаемая ставка получена независимым эталоном на Python
//! (`scripts/gen-xirr-fixtures.py`, арифметика `decimal`, 50 знаков),
//! а не выводом проверяемой программы (§15.5).

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::returns::{ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable, PriceQuality};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

struct Fixture {
    owner: OwnerId,
    account: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    source: SourceId,
    sequence: u32,
}

impl Fixture {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            custody: CustodyId::new_random(),
            instrument: InstrumentId::new_random(),
            source: SourceId::new_random(),
            sequence: 0,
        }
    }

    fn provenance(&self) -> Provenance {
        Provenance::new(
            self.source,
            RawHash::parse(&"a".repeat(64)).expect("хеш фикстуры"),
            ParserVersion("manual/1".into()),
        )
    }

    fn event(&mut self, day: Date, kind: EventKind, dates: EventDates, legs: Vec<Leg>) -> Event {
        self.sequence += 1;
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind,
            dates,
            order: EffectiveOrder::new(day, self.sequence),
            legs,
            provenance: self.provenance(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
}

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

/// Ручной ввод: пополнение, покупка, дивиденд, вывод, оценка.
fn journal(fixture: &mut Fixture) -> Vec<Event> {
    let deposit = rub(10_000_000);
    let gross = rub(9_000_000);
    // Комиссия сделки задаётся ПОЛОЖИТЕЛЬНОЙ величиной: `trade_settlement`
    // прибавляет её к телу сделки и уже потом меняет знак при покупке
    // (у продажи — вычитает из выручки). Отрицательная комиссия здесь
    // уменьшила бы стоимость покупки — проверено: `AmountMismatch`.
    let fee = rub(10_000);
    let dividend = rub(300_000);
    let withdrawal = rub(-1_000_000);

    vec![
        fixture.event(
            date!(2025 - 01 - 01),
            EventKind::CashIn { amount: deposit },
            EventDates::for_cash(CashPostedDate(date!(2025 - 01 - 01))),
            vec![Leg::cash(fixture.account, deposit)],
        ),
        {
            let settlement = rub(-(9_000_000 + 10_000));
            let account = fixture.account;
            let custody = fixture.custody;
            let instrument = fixture.instrument;
            fixture.event(
                date!(2025 - 01 - 15),
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: qty(100),
                    gross,
                    fee: Some(fee),
                    accrued_interest: None,
                },
                EventDates::for_trade(TradeDate(date!(2025 - 01 - 15)), None),
                vec![
                    Leg::cash(account, settlement),
                    Leg::security(account, custody, instrument, qty(100)),
                ],
            )
        },
        {
            let account = fixture.account;
            let instrument = fixture.instrument;
            fixture.event(
                date!(2025 - 07 - 01),
                EventKind::Income {
                    instrument: Some(instrument),
                    gross: dividend,
                },
                EventDates::for_cash(CashPostedDate(date!(2025 - 07 - 01))),
                vec![Leg::cash(account, dividend)],
            )
        },
        {
            let account = fixture.account;
            fixture.event(
                date!(2025 - 09 - 01),
                EventKind::CashOut { amount: withdrawal },
                EventDates::for_cash(CashPostedDate(date!(2025 - 09 - 01))),
                vec![Leg::cash(account, withdrawal)],
            )
        },
        {
            let instrument = fixture.instrument;
            fixture.event(
                date!(2026 - 01 - 01),
                EventKind::Valuation {
                    instrument,
                    price: Dec::new(Decimal::from(1_000)),
                    currency: CurrencyCode::Rub,
                    quality: PriceQuality::PreviousClose,
                },
                EventDates::for_cash(CashPostedDate(date!(2026 - 01 - 01))),
                vec![],
            )
        },
    ]
}

#[test]
fn single_account_answers_the_three_questions_of_stage_one() {
    let mut fixture = Fixture::new();
    let events = journal(&mut fixture);
    let contour = ContourDefinition::new(
        ContourId::new_random(),
        ContourVersion(1),
        [fixture.account],
    );
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };

    let projection = project(&events, &ctx).expect("проекция строится");
    let state = projection.state();

    // Деньги: 100 000 − 90 100 + 3 000 − 10 000 = 2 900 рублей.
    assert_eq!(
        state.balances().cash(fixture.account, CurrencyCode::Rub),
        Some(rub(290_000))
    );
    // Позиция: 100 бумаг, стоимость приобретения 90 100 рублей.
    assert_eq!(
        state
            .balances()
            .quantity_of(fixture.account, fixture.instrument)
            .expect("количество"),
        qty(100)
    );

    // Инварианты проверены, а не просто «не упало»: отчёт перечисляет,
    // что именно проверялось.
    assert!(!projection.invariants().checked().is_empty());

    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Сверка и периметр в этом тесте не участвуют: он проверяет расчёт,
    // а не подтверждение данных. Пустые реестр и оценка означают
    // «ничего не подтверждено», что для расчёта нейтрально.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of: date!(2026 - 01 - 01),
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
            market_prices: &[],
    };
    let report = returns_report(state, &request);

    assert_eq!(
        report.contributed.value(),
        Some(&Dec::new(Decimal::from(100_000)))
    );
    assert_eq!(
        report.withdrawn.value(),
        Some(&Dec::new(Decimal::from(10_000)))
    );
    // 2 900 денег + 100 бумаг по 1 000 = 102 900.
    assert_eq!(
        report.terminal_value.value(),
        Some(&Dec::new(Decimal::from(102_900)))
    );

    let outcome = report.xirr.value().expect("ставка вычислена");
    let expected = 0.133_270_341_032_f64;
    assert!(
        (outcome.rate().value() - expected).abs() < 1e-7,
        "ставка {} против эталонной {expected}",
        outcome.rate().value()
    );
    // Дивиденд границу контура не пересекает: он не является вложением.
    assert_eq!(state.flows().external().len(), 2);
}
