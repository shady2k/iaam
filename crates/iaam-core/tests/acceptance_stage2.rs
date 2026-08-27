//! Приёмка этапа 2 (§16.3): «цифрам можно верить — вот на сколько именно».
//!
//! Один сценарий из конца в конец. Ожидаемые остатки и статусы посчитаны
//! вручную из сумм операций и правил §10.3, а не сняты с вывода
//! программы (§15.5).

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterPolicy, assess};
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use iaam_core::returns::{DataQualityStatus, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use time::macros::date;

mod support;
use support::{Posting, TestChannel, event_on};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

fn april() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30)).unwrap()
}

/// Сцена: владелец, счёт и операции марта.
///
/// Внесено 500 000, куплено на 300 000 с комиссией 150, получен купон
/// 4 000. Остаток на конец марта считается вручную:
/// 500 000 − 300 000 − 150 + 4 000 = 203 850.
const MARCH_CLOSING: i64 = 203_850;
const MARCH_DEBIT: i64 = 504_000;
const MARCH_CREDIT: i64 = 300_150;

struct Scene {
    owner: OwnerId,
    account: AccountId,
    contour: ContourDefinition,
    operations: Vec<Event>,
}

fn scene(channel: &TestChannel) -> Scene {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);

    let mut operations = Vec::new();
    let mut push = |day, sequence, kind, legs| {
        operations.push(event_on(
            channel,
            Posting {
                owner,
                account,
                day,
                sequence,
            },
            kind,
            legs,
        ));
    };

    push(
        date!(2026 - 03 - 02),
        1,
        EventKind::CashIn {
            amount: rub(500_000),
        },
        vec![Leg::cash(account, rub(500_000))],
    );
    push(
        date!(2026 - 03 - 05),
        1,
        EventKind::CashOut {
            amount: rub(-300_000),
        },
        vec![Leg::cash(account, rub(-300_000))],
    );
    push(
        date!(2026 - 03 - 05),
        2,
        EventKind::Fee {
            amount: rub(-150),
            origin: iaam_core::event::kind::FeeOrigin::Brokerage,
        },
        vec![Leg::fee(account, rub(-150))],
    );
    push(
        date!(2026 - 03 - 20),
        1,
        EventKind::Income {
            instrument: None,
            gross: rub(4_000),
            kind: None,
        },
        vec![Leg::cash(account, rub(4_000))],
    );

    Scene {
        owner,
        account,
        contour,
        operations,
    }
}

/// Контрольные секции документа за март.
fn march_sections(
    scene: &Scene,
    channel: &TestChannel,
    closing: i64,
    debit: i64,
    credit: i64,
) -> Vec<Event> {
    [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(0),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(closing),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(debit),
            credit: PostedMinor::new(credit),
        },
        ControlClaim::IncomeTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(4_000),
        },
        ControlClaim::FeesTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(150),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, claim)| {
        event_on(
            channel,
            Posting {
                owner: scene.owner,
                account: scene.account,
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

fn status_of(events: &[Event], account: AccountId, dimension: Dimension) -> DimensionStatus {
    let perimeter = assess(events, PerimeterPolicy::default()).expect("периметр");
    ReconciliationLedger::build_with(events, &perimeter.exceptions())
        .expect("реестр")
        .status_for(account, date!(2026 - 03 - 15), dimension)
}

#[test]
fn the_stage_two_question_is_answered_step_by_step() {
    let report_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let scene = scene(&report_channel);

    // Шаг 1. Только операции, никаких утверждений: подтверждать нечем.
    let mut events = scene.operations.clone();
    assert_eq!(
        status_of(&events, scene.account, Dimension::Cash),
        DimensionStatus::Provisional,
        "без контрольных секций подтверждать нечем"
    );

    // Шаг 2. Пришёл отчёт за март: пять контрольных секций сошлись
    // одновременно — основание 5. Один документ и один парсер, поэтому
    // не выше internal.
    events.extend(march_sections(
        &scene,
        &report_channel,
        MARCH_CLOSING,
        MARCH_DEBIT,
        MARCH_CREDIT,
    ));
    assert_eq!(
        status_of(&events, scene.account, Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
    assert_eq!(
        status_of(&events, scene.account, Dimension::Income),
        DimensionStatus::AcceptedInternal,
        "сумма купонов сошлась — измерение доходов тоже подтверждено"
    );
    assert_eq!(
        status_of(&events, scene.account, Dimension::TaxBasis),
        DimensionStatus::Provisional,
        "об удержанном налоге отчёт ничего не сказал"
    );

    // Шаг 3. Апрельский отчёт того же брокера начинается с мартовского
    // остатка. Это непрерывность, а не независимость: статус остаётся
    // internal, сколько бы отчётов подряд ни сошлось.
    let april_channel = TestChannel::new("tinkoff-xlsx/1", "april");
    events.push(event_on(
        &april_channel,
        Posting {
            owner: scene.owner,
            account: scene.account,
            day: date!(2026 - 04 - 30),
            sequence: 10,
        },
        EventKind::ControlAssertion {
            period: april(),
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(MARCH_CLOSING),
                at: BalancePoint::Opening,
            },
        },
        vec![],
    ));
    assert_eq!(
        status_of(&events, scene.account, Dimension::Cash),
        DimensionStatus::AcceptedInternal,
        "следующий отчёт того же парсера независимости не даёт"
    );

    // Шаг 4. Те же данные пришли вторым каналом — другой код разбора
    // и другой документ. Только теперь появляется independent.
    let api_channel = TestChannel::new("tinkoff-api/1", "apimarch");
    events.extend(march_sections(
        &scene,
        &api_channel,
        MARCH_CLOSING,
        MARCH_DEBIT,
        MARCH_CREDIT,
    ));
    assert_eq!(
        status_of(&events, scene.account, Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );

    // Шаг 5. Отчёт в блоке качества данных сообщает, какой доле
    // стоимости можно верить.
    //
    // Срез для отчёта — события **по дату отчёта**: так его собирает
    // оболочка (`load_events_through`), и ядро отказывается считать
    // по срезу, содержащему более поздние события, — иначе получился бы
    // отчёт на дату, которой на эту дату не было. Апрельское утверждение
    // из среза выпадает; независимость марту даёт второй канал, а не оно.
    let events: Vec<Event> = events
        .into_iter()
        .filter(|event| {
            event
                .dates
                .effective_date()
                .is_some_and(|date| date <= date!(2026 - 03 - 31))
        })
        .collect();

    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &scene.contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(&events, &ctx).expect("проекция");
    let perimeter = assess(&events, PerimeterPolicy::default()).expect("периметр");
    let ledger =
        ReconciliationLedger::build_with(&events, &perimeter.exceptions()).expect("реестр");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let report = returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &scene.contour,
            coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
            as_of: date!(2026 - 03 - 31),
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
    assert_eq!(
        report.data_quality.nav_coverage.accepted_independent,
        Dec::one(),
        "вся стоимость подтверждена двумя независимыми каналами"
    );
    assert_eq!(report.data_quality.status, DataQualityStatus::Clean);
}

#[test]
fn a_wrong_figure_in_one_document_is_reported_as_a_discrepancy() {
    // Испорченная цифра обязана дать расхождение и попасть в долю
    // discrepant, а не раствориться в «пока не подтверждено».
    let report_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let scene = scene(&report_channel);
    let mut events = scene.operations.clone();
    events.extend(march_sections(
        &scene,
        &report_channel,
        MARCH_CLOSING + 1,
        MARCH_DEBIT,
        MARCH_CREDIT,
    ));

    assert_eq!(
        status_of(&events, scene.account, Dimension::Cash),
        DimensionStatus::Discrepant,
        "расхождение в одну копейку остаётся расхождением"
    );

    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &scene.contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(&events, &ctx).expect("проекция");
    let perimeter = assess(&events, PerimeterPolicy::default()).expect("периметр");
    let ledger =
        ReconciliationLedger::build_with(&events, &perimeter.exceptions()).expect("реестр");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let report = returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &scene.contour,
            coordinate: iaam_core::returns::KnowledgeCoordinate::default(),
            as_of: date!(2026 - 03 - 31),
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
    assert!(
        report.data_quality.nav_coverage.discrepant.is_positive(),
        "доля расхождения обязана быть строго положительной"
    );
    assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
}
