//! Границы отчёта: дата отчёта включительно и запрет считать по срезу,
//! собранному не на ту дату.
//!
//! Обе проверки — про строгость сравнения дат. Ошибка в один день здесь
//! не выглядит ошибкой: она даёт цифру, просто не ту.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::{Projection, ProjectionContext, project};
use iaam_core::returns::xirr::{flow_series, terminal_value};
use iaam_core::returns::{NotComputable, ReturnsRequest};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn deposit(owner: OwnerId, account: AccountId, day: Date, sequence: u32, minor: i64) -> Event {
    let amount = rub(minor);
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::CashIn { amount },
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs: vec![Leg::cash(account, amount)],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"8".repeat(64)).expect("хеш"),
            ParserVersion("boundary/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

struct Fixture {
    contour: ContourDefinition,
    projection: Projection,
}

fn project_days(days: &[(Date, i64)]) -> Fixture {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let events: Vec<Event> = days
        .iter()
        .enumerate()
        .map(|(i, (day, minor))| {
            deposit(
                owner,
                account,
                *day,
                u32::try_from(i).unwrap_or(u32::MAX) + 1,
                *minor,
            )
        })
        .collect();
    let projection = project(
        &events,
        &ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        },
    )
    .expect("проекция строится");
    Fixture {
        contour,
        projection,
    }
}

#[test]
fn a_flow_on_the_report_date_is_included() {
    // Дата отчёта включительна. Строгое «раньше» отрезало бы операцию
    // того же дня, и отчёт «на сегодня» не видел бы сегодняшнее
    // пополнение.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(date!(2025 - 06 - 01), 10_000_000), (as_of, 5_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Сверка и периметр в этом тесте не участвуют: он проверяет расчёт,
    // а не подтверждение данных. Пустые реестр и оценка означают
    // «ничего не подтверждено», что для расчёта нейтрально.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &fixture.contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
            market_prices: &[],
    };

    let series = flow_series(fixture.projection.state(), &request).expect("ряд потоков");
    assert_eq!(series.flows.len(), 2, "поток на дату отчёта обязан войти");
    assert_eq!(series.contributed, Dec::new(Decimal::from(150_000)));
}

#[test]
fn a_slice_containing_events_after_the_report_date_is_refused() {
    // Срез на дату собирает оболочка. Событие позже даты отчёта означает,
    // что срез собран неверно, и посчитать по нему — значит выдать отчёт
    // на дату, которого на эту дату не существовало.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(as_of, 10_000_000), (date!(2026 - 02 - 01), 1_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Сверка и периметр в этом тесте не участвуют: он проверяет расчёт,
    // а не подтверждение данных. Пустые реестр и оценка означают
    // «ничего не подтверждено», что для расчёта нейтрально.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &fixture.contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
            market_prices: &[],
    };

    assert!(matches!(
        flow_series(fixture.projection.state(), &request),
        Err(NotComputable::StateNewerThanReport { .. })
    ));
    assert!(matches!(
        terminal_value(fixture.projection.state(), &request),
        Err(NotComputable::StateNewerThanReport { .. })
    ));
}

#[test]
fn a_slice_ending_exactly_on_the_report_date_is_accepted() {
    // Граница на единицу: последнее событие ровно на дату отчёта —
    // это нормальный срез, а не сбор на будущее.
    let as_of = date!(2026 - 01 - 01);
    let fixture = project_days(&[(as_of, 10_000_000)]);
    let fx = FxTable::new(FxSource::OwnerSupplied);
    // Сверка и периметр в этом тесте не участвуют: он проверяет расчёт,
    // а не подтверждение данных. Пустые реестр и оценка означают
    // «ничего не подтверждено», что для расчёта нейтрально.
    let ledger = iaam_core::reconciliation::ReconciliationLedger::default();
    let perimeter = iaam_core::perimeter::PerimeterAssessment::empty(
        iaam_core::perimeter::PerimeterPolicy::default(),
    );
    let request = ReturnsRequest {
        contour: &fixture.contour,
        coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
        as_of,
        report_currency: CurrencyCode::Rub,
        fx: &fx,
        solver_policy: SolverPolicy::returns_default(),
        ledger: &ledger,
        perimeter: &perimeter,
            market_prices: &[],
    };
    assert!(flow_series(fixture.projection.state(), &request).is_ok());
    assert!(terminal_value(fixture.projection.state(), &request).is_ok());
}
