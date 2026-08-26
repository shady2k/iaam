//! Покрытие NAV по уровням достоверности (§10.5).
//!
//! Ожидаемые доли посчитаны вручную от стоимостей счетов, а не сняты
//! с вывода программы (§15.5).

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::event::kind::{EventKind, FeeOrigin};
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterPolicy, assess};
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::ReconciliationLedger;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::returns::{DataQualityStatus, MaterialIssue, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use time::macros::date;

mod support;
use support::{Posting, TestChannel, event_on};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

fn deposit(channel: &TestChannel, owner: OwnerId, account: AccountId, minor: i64) -> Event {
    event_on(
        channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 10),
            sequence: 1,
        },
        EventKind::CashIn { amount: rub(minor) },
        vec![Leg::cash(account, rub(minor))],
    )
}

/// Контрольные секции, подтверждающие остаток и оборот.
fn sections(
    channel: &TestChannel,
    owner: OwnerId,
    account: AccountId,
    closing: i64,
    debit: i64,
) -> Vec<Event> {
    [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(closing),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(debit),
            credit: PostedMinor::new(0),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, claim)| {
        event_on(
            channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: u32::try_from(index).unwrap() + 10,
            },
            EventKind::ControlAssertion {
                period: march(),
                claim,
            },
            vec![],
        )
    })
    .collect()
}

struct Fixture {
    events: Vec<Event>,
    contour: ContourDefinition,
}

fn report_of(fixture: &Fixture) -> iaam_core::returns::ReturnsReport {
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &fixture.contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(&fixture.events, &ctx).expect("проекция");
    let perimeter = assess(&fixture.events, PerimeterPolicy::default()).expect("периметр");
    let ledger = ReconciliationLedger::build_with(&fixture.events, &perimeter.exceptions())
        .expect("реестр сверки");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &fixture.contour,
            coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
            as_of: date!(2026 - 03 - 31),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
        },
    )
}

#[test]
fn shares_are_weighted_by_value_and_sum_to_one() {
    // Счёт на 300 000 подтверждён контрольными секциями, счёт на
    // 100 000 — нет. Ожидаемые доли считаются вручную: 300/400 = 0,75
    // подтверждено внутренне, 100/400 = 0,25 не подтверждено.
    // Доля по числу записей дала бы 0,5 на 0,5 — и это была бы ложь
    // о том, какой части портфеля можно верить.
    let owner = OwnerId::new_random();
    let confirmed = AccountId::new_random();
    let bare = AccountId::new_random();
    let confirmed_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let bare_channel = TestChannel::new("manual/1", "hand");

    let mut events = vec![
        deposit(&confirmed_channel, owner, confirmed, 300_000),
        deposit(&bare_channel, owner, bare, 100_000),
    ];
    events.extend(sections(
        &confirmed_channel,
        owner,
        confirmed,
        300_000,
        300_000,
    ));

    let contour = ContourDefinition::new(
        ContourId::new_random(),
        ContourVersion(1),
        [confirmed, bare],
    );
    let report = report_of(&Fixture { events, contour });
    let coverage = report.data_quality.nav_coverage;

    assert_eq!(
        coverage.accepted_internal,
        Dec::new(Decimal::new(75, 2)),
        "три четверти стоимости подтверждены внутренне"
    );
    assert_eq!(
        coverage.provisional,
        Dec::new(Decimal::new(25, 2)),
        "четверть стоимости не подтверждена"
    );
    assert_eq!(coverage.accepted_independent, Dec::zero());
    assert_eq!(coverage.discrepant, Dec::zero());

    let total = coverage
        .accepted_independent
        .checked_add(coverage.accepted_internal)
        .and_then(|sum| sum.checked_add(coverage.provisional))
        .and_then(|sum| sum.checked_add(coverage.discrepant))
        .expect("сумма долей");
    assert_eq!(total, Dec::one(), "доли обязаны покрывать портфель целиком");
}

#[test]
fn a_discrepant_account_lands_in_the_discrepant_share() {
    // Расходящийся счёт не растворяется в provisional: иначе проблема
    // пряталась бы ровно в той цифре, которая существует, чтобы её
    // показывать.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let channel = TestChannel::new("tinkoff-xlsx/1", "march");

    let mut events = vec![deposit(&channel, owner, account, 100_000)];
    // Отчёт утверждает остаток, которого нет.
    events.extend(sections(&channel, owner, account, 999_999, 100_000));

    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let report = report_of(&Fixture { events, contour });

    assert_eq!(report.data_quality.nav_coverage.discrepant, Dec::one());
    assert_eq!(report.data_quality.nav_coverage.provisional, Dec::zero());
    assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
    assert!(
        report
            .data_quality
            .material_issues
            .iter()
            .any(|issue| matches!(issue, MaterialIssue::Discrepancy { .. })),
        "расхождение обязано быть названо, а не только посчитано"
    );
}

#[test]
fn financing_marks_its_account_and_leaves_the_others_alone() {
    // §11: отказ считать один счёт не отменяет остальные.
    let owner = OwnerId::new_random();
    let margined = AccountId::new_random();
    let healthy = AccountId::new_random();
    let channel = TestChannel::new("manual/1", "hand");

    let events = vec![
        deposit(&channel, owner, healthy, 100_000),
        event_on(
            &channel,
            Posting {
                owner,
                account: margined,
                day: date!(2026 - 03 - 10),
                sequence: 1,
            },
            EventKind::CashOut {
                amount: rub(-50_000),
            },
            vec![Leg::cash(margined, rub(-50_000))],
        ),
        event_on(
            &channel,
            Posting {
                owner,
                account: margined,
                day: date!(2026 - 03 - 11),
                sequence: 2,
            },
            EventKind::Fee {
                amount: rub(-120),
                origin: FeeOrigin::MarginInterest,
            },
            vec![Leg::fee(margined, rub(-120))],
        ),
    ];

    let contour = ContourDefinition::new(
        ContourId::new_random(),
        ContourVersion(1),
        [margined, healthy],
    );
    let report = report_of(&Fixture { events, contour });

    let flagged: Vec<AccountId> = report
        .data_quality
        .material_issues
        .iter()
        .filter_map(|issue| match issue {
            MaterialIssue::UnsupportedFinancing { account } => Some(*account),
            _ => None,
        })
        .collect();
    assert_eq!(
        flagged,
        vec![margined],
        "помечен только счёт с финансированием вне периметра"
    );

    // Остальные счета продолжают считаться: стоимость портфеля
    // посчитана, и отчёт не превратился в отказ целиком.
    assert!(
        report.terminal_value.value().is_some(),
        "стоимость обязана считаться, несмотря на счёт вне периметра"
    );
}

#[test]
fn a_fully_confirmed_portfolio_without_defects_is_clean() {
    // `Clean` обязан быть достижим: недостижимый статус — это молчаливое
    // обещание, которое система никогда не выполнит.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let report_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let api_channel = TestChannel::new("tinkoff-api/1", "apimarch");

    let mut events = vec![deposit(&report_channel, owner, account, 100_000)];
    events.extend(sections(&report_channel, owner, account, 100_000, 100_000));
    events.extend(sections(&api_channel, owner, account, 100_000, 100_000));

    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let report = report_of(&Fixture { events, contour });

    assert_eq!(
        report.data_quality.nav_coverage.accepted_independent,
        Dec::one(),
        "два независимых канала подтверждают весь портфель"
    );
    assert_eq!(report.data_quality.status, DataQualityStatus::Clean);
}
