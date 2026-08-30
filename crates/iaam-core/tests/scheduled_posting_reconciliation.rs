//! Заслоны сверки запланированных выплат (§7.2, §15.3).
//!
//! Точечные проверки правила сопоставления, проекции фактов дохода и
//! границы владения написаны их авторами рядом с кодом. Здесь проверяется
//! **стык**: журнал событий целиком проходит проекцию, правило потока,
//! правило сопоставления и сборку отчёта, а утверждения делаются только
//! о том, что видит владелец, — о `MaterialIssue` в `data_quality`.
//! Модульный тест этого не ловит: он вызывает правило напрямую и молчит,
//! когда сверка перестала до правила доходить.
//!
//! Файл интеграционный намеренно: он собирает журнал и читает отчёт
//! исключительно через публичный интерфейс крейта. Регрессия, которая
//! спрячет сверку за приватным помощником, здесь покраснеет.
//!
//! ## Про номинал
//!
//! Номинал больше не свойство партии: он приезжает из графика выпуска.
//! Прежний хелпер подменял его прямо в CBOR-снимке состояния — приём,
//! которым вся линия E3.4 годами проверялась на данных, каких рабочий
//! код не видит. Хелпер снесён вместе с `Lot.principal` (T8).
//!
//! График пятилетней бумаги и остальные сценарии передают номинал обычным
//! путём. Отдельный тест `without_the_face_value_the_reconciliation_still_runs`
//! намеренно оставляет `initial_principal` неизвестным: прошлое всё равно
//! должно сверяться прямо из графика.

use std::collections::BTreeMap;

use iaam_core::bond::offer::ScheduleCompleteness;
use iaam_core::bond::{AccrualPeriod, BondSchedule, DefaultFlags, PrincipalReturn};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, SettledDate, TradeDate};
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::{EventKind, IncomeKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterAssessment, PerimeterPolicy};
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::ReconciliationLedger;
use iaam_core::returns::{
    Computed, KnowledgeCoordinate, MaterialIssue, ReturnsReport, ReturnsRequest,
    UnverifiableReason, returns_report,
};
use iaam_core::rules::{LotRuleVersion, PostingKind, RuleRegistry};
use iaam_core::valuation::{
    PriceCandidate, PriceKind, PriceOrigin, QuotationBasis, SourceExecutability, Venue,
};
use rust_decimal::Decimal;
use time::macros::date;
use time::{Date, Duration};
use uuid::Uuid;

// Идентичности заданы числами, а не `new_random`: свойство о порядке
// журнала сравнивает вердикты двух прогонов, и случайный `EventId`
// сделал бы расхождение невоспроизводимым (§15.3).
const OWNER: OwnerId = OwnerId(Uuid::from_u128(1));
const ACCOUNT: AccountId = AccountId(Uuid::from_u128(2));
const INSTRUMENT: InstrumentId = InstrumentId(Uuid::from_u128(3));
const CUSTODY: CustodyId = CustodyId(Uuid::from_u128(4));
const OTHER_CUSTODY: CustodyId = CustodyId(Uuid::from_u128(5));
const SOURCE: SourceId = SourceId(Uuid::from_u128(6));

/// Количество бумаг в одной покупке.
const PURCHASE_QUANTITY_TEXT: &str = "10";
/// Дата отчёта по умолчанию.
const REPORT_DATE: Date = date!(2026 - 08 - 26);

fn dec(text: &str) -> Dec {
    Dec::new(Decimal::from_str_exact(text).expect("десятичная константа"))
}

fn rubles(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn per_unit(text: &str) -> PerUnitAmount {
    PerUnitAmount::new(dec(text), CurrencyCode::Rub)
}

/// Конверт события. Идентификатор выводится из номера в журнале: он же
/// задаёт порядок, поэтому два события с одним номером — ошибка теста,
/// а не повод для случайности.
fn event(date: Date, number: u32, kind: EventKind, legs: Vec<Leg>) -> Event {
    Event {
        id: EventId(Uuid::from_u128(u128::from(number))),
        schema_version: SCHEMA_VERSION,
        owner: OWNER,
        account: ACCOUNT,
        kind,
        dates: EventDates::for_cash(CashPostedDate(date)),
        order: EffectiveOrder::new(date, number),
        legs,
        provenance: Provenance::new(
            SOURCE,
            RawHash::parse(&"a".repeat(64)).expect("шестнадцатеричный хеш"),
            ParserVersion("test/scheduled-posting/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn cash_in(date: Date, number: u32) -> Event {
    let amount = rubles(10_000_000);
    event(
        date,
        number,
        EventKind::CashIn { amount },
        vec![Leg::cash(ACCOUNT, amount)],
    )
}

/// Покупка облигации с заявленной датой сделки.
///
/// Дата сделки обязательна: именно её книга лотов кладёт в `Lot.acquired`,
/// а из неё берётся нижняя граница владения. Покупка без даты сделки
/// делает сверку недоказуемой — это отдельный случай, и он проверен
/// модульным тестом ядра.
fn purchase(custody: CustodyId, date: Date, number: u32) -> Event {
    let quantity = Quantity(dec(PURCHASE_QUANTITY_TEXT));
    let mut event = event(
        date,
        number,
        EventKind::Trade {
            side: TradeSide::Buy,
            instrument: INSTRUMENT,
            quantity,
            gross: rubles(1_000_000),
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(ACCOUNT, rubles(-1_000_000)),
            Leg::security(ACCOUNT, custody, INSTRUMENT, quantity),
        ],
    );
    // Тест моделирует источник Finam, который сообщает дату расчётов;
    // здесь она совпадает с датой сделки, потому что расчёты отдельно
    // не проверяются.
    event.dates.settled = Some(SettledDate(date));
    event.dates.trade = Some(TradeDate(date));
    event
}
/// Покупка от источника, который не сообщает дату расчётов.
///
/// Отсутствие `settled` намеренно оставляет владение недоказуемым:
/// источник не даёт системе права угадывать дату перехода прав.
fn purchase_without_settlement_date(custody: CustodyId, date: Date, number: u32) -> Event {
    let mut event = purchase(custody, date, number);
    event.dates.settled = None;
    event
}

/// Продажа всей партии из названного депозитария.
fn sale(custody: CustodyId, date: Date, number: u32) -> Event {
    let quantity = Quantity(dec(PURCHASE_QUANTITY_TEXT));
    let mut event = event(
        date,
        number,
        EventKind::Trade {
            side: TradeSide::Sell,
            instrument: INSTRUMENT,
            quantity,
            gross: rubles(1_000_000),
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(ACCOUNT, rubles(1_000_000)),
            Leg::security(ACCOUNT, custody, INSTRUMENT, Quantity(dec("-10"))),
        ],
    );
    // Тест моделирует источник Finam, который сообщает дату расчётов;
    // здесь она совпадает с датой сделки, потому что расчёты отдельно
    // не проверяются.
    event.dates.settled = Some(SettledDate(date));
    event.dates.trade = Some(TradeDate(date));
    event
}

/// Пришедший купон: дата зачисления денег и есть дата факта.
fn coupon(date: Date, number: u32) -> Event {
    let amount = rubles(50_000);
    event(
        date,
        number,
        EventKind::Income {
            instrument: Some(INSTRUMENT),
            gross: amount,
            kind: Some(IncomeKind::Coupon),
        },
        vec![Leg::cash(ACCOUNT, amount)],
    )
}

/// Амортизационная выплата: половина номинала деньгами, количество бумаг
/// не меняется (§6.5). Единственный источник факта вида `PrincipalReturn`
/// у бумаги, которая ещё не погашена.
fn partial_redemption(date: Date, number: u32) -> Event {
    let compensation = rubles(500_000);
    event(
        date,
        number,
        EventKind::CorporateAction {
            action: CorporateAction::PartialRedemption {
                instrument: INSTRUMENT,
                custody: CUSTODY,
                quantity: Quantity(dec(PURCHASE_QUANTITY_TEXT)),
                principal_returned_per_unit: per_unit("500"),
                compensation,
                effective_date: date,
                record_date: None,
                grounds: None,
                basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
            },
        },
        vec![Leg::principal(ACCOUNT, INSTRUMENT, compensation)],
    )
}

/// График выпуска: купонные даты и доли возврата номинала.
///
/// Полнота, флаги дефолта и валютные роли заданы явно — без них правило
/// потока отказывается строить план, `past` не появляется вовсе, и тест
/// проверял бы отказ построения, а не сверку. Период начинается
/// предыдущей выплатой: замкнутая цепь нужна расчёту НКД, иначе отказ
/// приходит оттуда.
fn schedule(coupon_dates: &[Date], returns: &[(Date, &str)]) -> BondSchedule {
    BondSchedule {
        // Тестовый график моделирует источник, который сообщает дату
        // фиксации реестра; дата фиксации совпадает с датой платежа.
        periods: coupon_dates
            .iter()
            .enumerate()
            .map(|(index, date)| AccrualPeriod {
                period_start: if index == 0 {
                    date.saturating_sub(Duration::days(180))
                } else {
                    coupon_dates[index - 1]
                },
                accrual_end: *date,
                payment_date: *date,
                record_date: Some(*date),
                coupon_per_unit: Some(per_unit("50")),
            })
            .collect(),
        principal_returns: returns
            .iter()
            .map(|(date, share)| PrincipalReturn {
                repayment_date: *date,
                share_percent: dec(share),
            })
            .collect(),
        initial_principal: None,
        offer_windows: Vec::new(),
        completeness: ScheduleCompleteness::Validated,
        default_flags: Some(DefaultFlags {
            declared: false,
            technical: false,
        }),
        currency_roles: Some(CurrencyRoles::uniform(CurrencyCode::Rub)),
    }
}
/// График с номиналом, который приходит из справочника выпуска.
fn schedule_with_face_value(coupon_dates: &[Date], returns: &[(Date, &str)]) -> BondSchedule {
    let mut schedule = schedule(coupon_dates, returns);
    schedule.initial_principal = Some(per_unit("1000"));
    schedule
}

/// Биржевая цена накануне отчёта: непокрытая позиция сама делает отчёт
/// неполным и скрыла бы вклад сверки в статус качества.
fn price(report_date: Date) -> PriceCandidate {
    PriceCandidate {
        instrument: INSTRUMENT,
        price: dec("1000"),
        currency: CurrencyCode::Rub,
        basis: QuotationBasis::MoneyPerUnit,
        basis_evidence: "test:market".to_owned(),
        basis_evidence_contradicts: false,
        trade_date: report_date.saturating_sub(Duration::days(1)),
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

/// Что подаётся на вход отчёту.
///
/// Структурой, а не пятью позиционными аргументами: перепутать местами
/// журнал и график легко, а заметить по красному тесту — нет.
struct Scenario<'a> {
    events: &'a [Event],
    schedule: &'a BondSchedule,
    report_date: Date,
}

fn build_report(scenario: &Scenario<'_>) -> ReturnsReport {
    let contour =
        ContourDefinition::new(ContourId(Uuid::from_u128(7)), ContourVersion(1), [ACCOUNT]);
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let state = project(scenario.events, &context)
        .expect("проекция журнала облигации")
        .state()
        .clone();
    let fx = iaam_core::valuation::FxTable::new(iaam_core::valuation::FxSource::OwnerSupplied);
    let ledger = ReconciliationLedger::default();
    let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
    let candidate = price(scenario.report_date);
    let schedules = BTreeMap::from([(INSTRUMENT, scenario.schedule.clone())]);
    returns_report(
        &state,
        &ReturnsRequest {
            contour: &contour,
            as_of: scenario.report_date,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &schedules,
            accrued_observations: &BTreeMap::new(),
        },
    )
}

fn missing_postings(report: &ReturnsReport) -> Vec<&MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| matches!(issue, MaterialIssue::ScheduledPostingNotReceived { .. }))
        .collect()
}

fn unverifiable_postings(report: &ReturnsReport) -> Vec<&MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| {
            matches!(
                issue,
                MaterialIssue::ScheduledPostingUnverifiable { .. }
                    | MaterialIssue::ScheduledPostingsUnverifiable { .. }
            )
        })
        .collect()
}

/// Вердикт сверки: обе её проблемы в порядке отчёта.
fn verdict(report: &ReturnsReport) -> Vec<MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| {
            matches!(
                issue,
                MaterialIssue::ScheduledPostingNotReceived { .. }
                    | MaterialIssue::ScheduledPostingUnverifiable { .. }
                    | MaterialIssue::ScheduledPostingsUnverifiable { .. }
            )
        })
        .cloned()
        .collect()
}

/// Поток построен, значит сверка до плана дошла.
///
/// Без этой проверки молчание сверки нельзя отличить от несостоявшегося
/// сценария: отказ построения пропускает сверку целиком.
fn assert_flow_built(report: &ReturnsReport) {
    assert!(
        !report.bond_metrics.is_empty(),
        "облигационная позиция не попала в отчёт: проверять нечего"
    );
    for position in &report.bond_metrics {
        assert!(
            matches!(
                position.scenarios[0].prospective.metrics,
                Computed::Value(_)
            ),
            "поток не построен: {:?}",
            position.scenarios[0].prospective.metrics
        );
    }
}

/// Купонные даты, срок которых к дате отчёта уже прошёл: пять лет
/// полугодовых выплат.
const PAST_COUPON_DATES: [Date; 10] = [
    date!(2021 - 09 - 15),
    date!(2022 - 03 - 15),
    date!(2022 - 09 - 15),
    date!(2023 - 03 - 15),
    date!(2023 - 09 - 15),
    date!(2024 - 03 - 15),
    date!(2024 - 09 - 15),
    date!(2025 - 03 - 15),
    date!(2025 - 09 - 15),
    date!(2026 - 03 - 15),
];

/// График пятилетней бумаги: купоны и погашение без номинала.
fn five_year_bond_schedule_without_face_value() -> BondSchedule {
    let mut coupon_dates = PAST_COUPON_DATES.to_vec();
    coupon_dates.push(date!(2026 - 09 - 15));
    coupon_dates.push(date!(2027 - 03 - 15));
    schedule(&coupon_dates, &[(date!(2027 - 03 - 15), "100")])
}

/// График пятилетней бумаги с номиналом из справочника выпуска.
fn five_year_bond_schedule() -> BondSchedule {
    let mut schedule = five_year_bond_schedule_without_face_value();
    schedule.initial_principal = Some(per_unit("1000"));
    schedule
}

/// Пятилетняя история: покупка и купоны, пришедшие с задержкой
/// депозитарной цепочки.
///
/// Сдвиг факта от плановой даты гуляет по всему разрешённому диапазону
/// 1–7 дней: реальная выплата приходит не день в день, и тест обязан
/// быть зелёным именно на таком журнале, а не на выдуманном идеальном.
fn five_year_journal(missing_date: Option<Date>) -> Vec<Event> {
    let mut events = vec![
        cash_in(date!(2021 - 07 - 25), 1),
        purchase(CUSTODY, date!(2021 - 08 - 01), 2),
    ];
    let mut number = 10;
    for (index, date) in PAST_COUPON_DATES.iter().enumerate() {
        if Some(*date) == missing_date {
            continue;
        }
        let offset = i64::try_from(index % 7).expect("номер купона") + 1;
        events.push(coupon(date.saturating_add(Duration::days(offset)), number));
        number += 1;
    }
    events
}

#[test]
fn five_years_of_coupons_received_late_but_received_raise_no_alarm() {
    // Главный критерий эпика: здоровая бумага молчит. Задержка выплаты
    // на 1–7 дней — норма депозитарной цепочки, а не дефект, и если
    // такой журнал даёт хоть одну тревогу, сверка бесполезна: владелец
    // перестанет читать предупреждения на второй неделе.
    let report = build_report(&Scenario {
        events: &five_year_journal(None),
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    assert!(
        verdict(&report).is_empty(),
        "вердикт: {:?}",
        verdict(&report)
    );
}

#[test]
fn an_amortised_bond_closes_its_principal_returns_with_partial_redemptions() {
    // Возврат номинала подтверждается корпоративным действием, а купон —
    // доходом. Для купонных периодов график сообщает дату фиксации, но
    // `PrincipalReturn` пока не несёт такого поля. Поэтому обе ветки
    // обязаны молчать именно об обвинении: без даты права нельзя объявить
    // возврат пропущенным, даже если факт возврата пришёл.
    let schedule = schedule_with_face_value(
        &[date!(2026 - 03 - 15), date!(2026 - 09 - 15)],
        &[(date!(2026 - 06 - 15), "50"), (date!(2026 - 09 - 15), "50")],
    );
    let healthy_events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
        coupon(date!(2026 - 03 - 17), 3),
        partial_redemption(date!(2026 - 06 - 17), 4),
    ];
    let assert_unverifiable = |report: &ReturnsReport| {
        assert_flow_built(report);
        assert!(
            missing_postings(report).is_empty(),
            "без даты права нельзя объявлять возврат пропущенным: {:?}",
            verdict(report)
        );
        let issues = unverifiable_postings(report);
        assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
        assert!(
            matches!(
                issues[0],
                MaterialIssue::ScheduledPostingUnverifiable {
                    date,
                    kind: PostingKind::PrincipalReturn,
                    reason: UnverifiableReason::EntitlementDateUnknown,
                    ..
                } if *date == date!(2026 - 06 - 15)
            ),
            "проблема: {:?}",
            issues[0]
        );
    };
    let report = build_report(&Scenario {
        events: &healthy_events,
        schedule: &schedule,
        report_date: REPORT_DATE,
    });
    assert_unverifiable(&report);

    // Тот же журнал без амортизационной выплаты: пока в модели нет даты
    // права для `PrincipalReturn`, отсутствие факта также нельзя назвать
    // пропуском — результат остаётся недоказуемым, а не обвинительным.
    let without_amortisation: Vec<Event> = healthy_events[..3].to_vec();
    let report = build_report(&Scenario {
        events: &without_amortisation,
        schedule: &schedule,
        report_date: REPORT_DATE,
    });
    assert_unverifiable(&report);
}

#[test]
fn a_single_gap_in_the_middle_of_the_history_is_named_once_and_exactly() {
    // Пропуск в середине ряда — тот случай, ради которого сопоставление
    // сделано one-to-one: жадное сопоставление «любой факт закрывает
    // любую выплату» съело бы дыру соседними купонами и промолчало.
    let missing_date = date!(2023 - 09 - 15);
    let report = build_report(&Scenario {
        events: &five_year_journal(Some(missing_date)),
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                account,
                instrument,
                date,
                kind: PostingKind::Coupon,
            } if *account == ACCOUNT && *instrument == INSTRUMENT && *date == missing_date
        ),
        "проблема: {:?}",
        issues[0]
    );
    assert!(unverifiable_postings(&report).is_empty());

    // Тот же вопрос ко всему ряду: изъятие любого из десяти купонов
    // обязано давать ровно одну проблему — свою. Без этого прохода
    // сверка могла бы проверять два-три купона из десяти, а остальные
    // молча пропускать, и первый тест файла всё равно был бы зелёным.
    for missing_date in PAST_COUPON_DATES {
        let report = build_report(&Scenario {
            events: &five_year_journal(Some(missing_date)),
            schedule: &five_year_bond_schedule(),
            report_date: REPORT_DATE,
        });
        let issues = missing_postings(&report);
        assert_eq!(issues.len(), 1, "купон {missing_date}: {issues:?}");
        assert!(
            matches!(
                issues[0],
                MaterialIssue::ScheduledPostingNotReceived { date, .. }
                    if *date == missing_date
            ),
            "купон {missing_date}: {:?}",
            issues[0]
        );
    }
}

#[test]
fn the_waiting_window_expires_exactly_twenty_one_days_after_the_scheduled_date() {
    // `is_due` — это `date + 21 <= as_of`. Значит на двадцатый день
    // срок ещё идёт и тревоги нет, а на двадцать первый он истёк и
    // тревога обязана быть. Граница проверяется тремя точками, потому
    // что сдвиг на день в любую сторону — это либо ложная тревога на
    // здоровой бумаге, либо молчание на пропущенной выплате.
    let scheduled_date = date!(2026 - 03 - 15);
    let schedule = schedule_with_face_value(
        &[scheduled_date, date!(2026 - 09 - 15)],
        &[(date!(2026 - 09 - 15), "100")],
    );
    let events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
    ];
    let as_of = |offset: i64| {
        build_report(&Scenario {
            events: &events,
            schedule: &schedule,
            report_date: scheduled_date.saturating_add(Duration::days(offset)),
        })
    };

    let still_pending = as_of(20);
    assert_flow_built(&still_pending);
    assert!(
        verdict(&still_pending).is_empty(),
        "на двадцатый день срок ещё идёт: {:?}",
        verdict(&still_pending)
    );

    let expired = as_of(21);
    assert_flow_built(&expired);
    assert_eq!(
        missing_postings(&expired).len(),
        1,
        "проблемы: {:?}",
        missing_postings(&expired)
    );

    let long_expired = as_of(22);
    assert_flow_built(&long_expired);
    assert_eq!(
        missing_postings(&long_expired).len(),
        1,
        "проблемы: {:?}",
        missing_postings(&long_expired)
    );
}

/// Две покупки и продажа ранней партии из того же депозитария.
///
/// Один депозитарий на весь журнал: иначе продажа списала бы бумагу
/// оттуда, куда её не клали, и позиций стало бы три — тест проверял бы
/// задвоение, а не границу владения.
fn journal_with_early_lot_sold(fact_dates: &[Date]) -> Vec<Event> {
    let mut events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
        purchase(CUSTODY, date!(2026 - 04 - 10), 3),
        sale(CUSTODY, date!(2026 - 07 - 10), 4),
    ];
    for (index, date) in fact_dates.iter().enumerate() {
        events.push(coupon(
            *date,
            10 + u32::try_from(index).expect("номер факта"),
        ));
    }
    events
}

/// График бумаги с двумя прошедшими купонами и погашением в декабре.
fn two_coupon_schedule() -> BondSchedule {
    schedule_with_face_value(
        &[
            date!(2026 - 03 - 15),
            date!(2026 - 06 - 15),
            date!(2026 - 12 - 15),
        ],
        &[(date!(2026 - 12 - 15), "100")],
    )
}

#[test]
fn a_coupon_missed_while_the_early_lot_was_held_is_named_after_it_was_sold() {
    // Граница владения — самая ранняя дата приобретения, когда-либо
    // наблюдённая по паре, а не дата самой старой живой партии. Иначе
    // продажа январской партии подняла бы границу до апреля и спрятала
    // мартовский пропуск: владелец потерял бы деньги ровно там, где
    // сверка обязана его предупредить.
    let report = build_report(&Scenario {
        events: &journal_with_early_lot_sold(&[date!(2026 - 06 - 16)]),
        schedule: &two_coupon_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        ),
        "проблема: {:?}",
        issues[0]
    );
}

#[test]
fn two_purchases_with_a_complete_history_raise_no_alarm() {
    // Обратная сторона той же границы. Без этого теста границу можно
    // было бы «починить», объявив пропущенным всё подряд.
    let report = build_report(&Scenario {
        events: &journal_with_early_lot_sold(&[date!(2026 - 03 - 16), date!(2026 - 06 - 16)]),
        schedule: &two_coupon_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    assert!(
        verdict(&report).is_empty(),
        "вердикт: {:?}",
        verdict(&report)
    );
}

#[test]
fn one_bond_in_two_custodies_reports_a_single_missing_coupon() {
    // Позиции обходятся по месту хранения, а сверяется пара
    // (счёт, бумага) без него: одна и та же выплата иначе была бы
    // названа по разу на депозитарий, и владелец искал бы два пропуска
    // вместо одного.
    let events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase(CUSTODY, date!(2026 - 01 - 10), 2),
        purchase(OTHER_CUSTODY, date!(2026 - 01 - 11), 3),
        coupon(date!(2026 - 03 - 16), 4),
    ];
    let report = build_report(&Scenario {
        events: &events,
        schedule: &two_coupon_schedule(),
        report_date: REPORT_DATE,
    });

    assert_flow_built(&report);
    assert_eq!(
        report.bond_metrics.len(),
        2,
        "позиций должно быть две, иначе тест ничего не доказывает"
    );
    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 06 - 15)
        ),
        "проблема: {:?}",
        issues[0]
    );
}

/// Перестановка журнала с явно заданным семенем.
///
/// Тасование Фишера—Йетса на линейном конгруэнтном генераторе: ядро
/// детерминировано, и `rand` из окружения сделал бы падение свойства
/// невоспроизводимым. Константы — общеизвестные множитель и приращение
/// LCG (Кнут); их качество здесь роли не играет, важна лишь
/// повторяемость от прогона к прогону.
fn shuffled(events: &[Event], seed: u64) -> Vec<Event> {
    let mut order = events.to_vec();
    let mut state = seed;
    for index in (1..order.len()).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let choice = usize::try_from(state >> 33).expect("старшие разряды семени") % (index + 1);
        order.swap(index, choice);
    }
    order
}

#[test]
fn the_verdict_does_not_depend_on_the_order_of_the_journal() {
    // §15.3. Правило сопоставления сортирует свои входы и потому от
    // порядка не зависит — это проверено его собственными тестами.
    // Здесь проверяется стык: проекция обязана привести журнал
    // к действующему набору в порядке `EffectiveOrder` до того, как
    // сверка что-либо увидит. Журнал берётся с пропуском: свойство
    // «всегда пусто» выполнялось бы и у сломанной сверки.
    //
    // Переставляется журнал целиком, и это законно именно здесь: ни одно
    // событие не ссылается на другое (`Relation::None`), а номер в
    // журнале у каждого свой, поэтому `EffectiveOrder` задаёт полный
    // порядок и действующий набор от перестановки не зависит по
    // построению. Журнал с исправлениями так переставлять нельзя.
    let events = five_year_journal(Some(date!(2023 - 09 - 15)));
    let baseline_verdict = verdict(&build_report(&Scenario {
        events: &events,
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    }));
    assert_eq!(
        baseline_verdict.len(),
        1,
        "вердикт эталона: {baseline_verdict:?}"
    );

    // Тасование обязано что-то переставлять: свойство, проверенное на
    // тождественной перестановке, не проверяет ничего.
    let mut order_changed = false;
    for seed in 1..=32_u64 {
        let shuffled_events = shuffled(&events, seed);
        order_changed |= shuffled_events != events;
        let shuffled_verdict = verdict(&build_report(&Scenario {
            events: &shuffled_events,
            schedule: &five_year_bond_schedule(),
            report_date: REPORT_DATE,
        }));
        assert_eq!(
            shuffled_verdict, baseline_verdict,
            "перестановка с семенем {seed} изменила вердикт"
        );
    }

    assert!(order_changed, "тасование не переставило ни один журнал");

    // Обратный порядок — не случайная перестановка, а самый вероятный
    // способ прочитать выгрузку брокера задом наперёд.
    let mut reversed_events = events.clone();
    reversed_events.reverse();
    assert_eq!(
        verdict(&build_report(&Scenario {
            events: &reversed_events,
            schedule: &five_year_bond_schedule(),
            report_date: REPORT_DATE,
        })),
        baseline_verdict,
        "обратный порядок изменил вердикт"
    );
}

#[test]
fn projecting_the_same_journal_twice_gives_the_same_verdict() {
    // §15.3: повторная проекция того же журнала обязана дать то же
    // состояние и тот же отчёт. Сверяется и отпечаток состояния, и
    // отпечаток входов отчёта, и сам вердикт: сверка читает состояние,
    // а не журнал, и разойтись они могут порознь.
    let events = five_year_journal(Some(date!(2024 - 03 - 15)));
    let scenario = Scenario {
        events: &events,
        schedule: &five_year_bond_schedule(),
        report_date: REPORT_DATE,
    };

    let first_report = build_report(&scenario);
    let second_report = build_report(&scenario);

    assert_eq!(
        verdict(&first_report).len(),
        1,
        "вердикт: {:?}",
        verdict(&first_report)
    );
    assert_eq!(verdict(&first_report), verdict(&second_report));
    assert_eq!(first_report.inputs_hash, second_report.inputs_hash);
}

#[test]
fn without_the_face_value_the_reconciliation_still_runs() {
    // Прежнее поведение было дефектом: номинал не доходил до лотов
    // (`iaam-d8b.15`), из-за чего сверка молчала на всех реальных данных.
    // Историческое прошлое теперь строится прямо из графика и обязано
    // назвать пропущенный купон даже при неизвестном номинале.
    let events = five_year_journal(Some(date!(2023 - 09 - 15)));
    let report = build_report(&Scenario {
        events: &events,
        schedule: &five_year_bond_schedule_without_face_value(),
        report_date: REPORT_DATE,
    });

    let issues = missing_postings(&report);
    assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                date,
                kind: PostingKind::Coupon,
                ..
            } if *date == date!(2023 - 09 - 15)
        ),
        "проблема: {:?}",
        issues[0]
    );
    assert!(
        unverifiable_postings(&report).is_empty(),
        "известное владение не должно стать недоказуемостью: {:?}",
        verdict(&report)
    );
}

#[test]
fn a_source_without_settlement_dates_cannot_accuse_anyone() {
    // Источник, не сообщающий даты перехода прав, делает владение
    // недоказуемым. Система обязана признаться, а не угадать: обвинение
    // требует доказательства, признание незнания — нет.
    let events = vec![
        cash_in(date!(2026 - 01 - 05), 1),
        purchase_without_settlement_date(CUSTODY, date!(2026 - 01 - 10), 2),
    ];
    let schedule = schedule(
        &[date!(2026 - 03 - 15), date!(2026 - 12 - 15)],
        &[(date!(2026 - 12 - 15), "100")],
    );
    let report = build_report(&Scenario {
        events: &events,
        schedule: &schedule,
        report_date: REPORT_DATE,
    });

    let issues = unverifiable_postings(&report);
    assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingUnverifiable {
                date,
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } if *date == date!(2026 - 03 - 15)
        ),
        "проблема: {:?}",
        issues[0]
    );
    assert!(
        missing_postings(&report).is_empty(),
        "без доказанного владения нельзя обвинять в пропуске: {:?}",
        verdict(&report)
    );
}
