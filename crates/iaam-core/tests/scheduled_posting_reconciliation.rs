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
//! ## Про подмену номинала
//!
//! Ни `apply_trade`, ни восстановление позиции номинал лоту не ставят —
//! он остаётся `PrincipalState::Unknown` (известная дыра `iaam-d8b.15`).
//! Без номинала правило потока отказывается строить план, сверка молча
//! пропускается, и любой тест этого файла был бы зелёным, ничего не
//! проверяя. Поэтому номинал подставляется состоянию после проекции —
//! так же, как это делает `состояние_с_номиналами` в модульных тестах
//! `returns/mod.rs`. Что подмена действительно решает исход, а не
//! украшает журнал, доказывает
//! `without_the_face_value_the_reconciliation_is_silently_skipped`.

use std::collections::BTreeMap;

use iaam_core::bond::offer::ScheduleCompleteness;
use iaam_core::bond::{AccrualPeriod, BondSchedule, DefaultFlags, PrincipalReturn};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
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
use iaam_core::projection::state::LedgerState;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::ReconciliationLedger;
use iaam_core::returns::{
    Computed, KnowledgeCoordinate, MaterialIssue, NotComputable, ReturnsReport, ReturnsRequest,
    returns_report,
};
use iaam_core::rules::lot_disposal::PrincipalState;
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
const ВЛАДЕЛЕЦ: OwnerId = OwnerId(Uuid::from_u128(1));
const СЧЁТ: AccountId = AccountId(Uuid::from_u128(2));
const БУМАГА: InstrumentId = InstrumentId(Uuid::from_u128(3));
const ДЕПОЗИТАРИЙ: CustodyId = CustodyId(Uuid::from_u128(4));
const ДРУГОЙ_ДЕПОЗИТАРИЙ: CustodyId = CustodyId(Uuid::from_u128(5));
const ИСТОЧНИК: SourceId = SourceId(Uuid::from_u128(6));

/// Номинал одной бумаги. Тот же у всех партий: разные номиналы делают
/// остаточный номинал позиции неоднозначным, и отчёт отказался бы
/// строить поток по другой причине, чем проверяет тест.
const НОМИНАЛ: &str = "1000";
/// Количество бумаг в одной покупке.
const КОЛИЧЕСТВО: &str = "10";
/// Дата отчёта по умолчанию.
const ДАТА_ОТЧЁТА: Date = date!(2026 - 08 - 26);

fn dec(text: &str) -> Dec {
    Dec::new(Decimal::from_str_exact(text).expect("десятичная константа"))
}

fn рубли(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn за_единицу(text: &str) -> PerUnitAmount {
    PerUnitAmount::new(dec(text), CurrencyCode::Rub)
}

/// Конверт события. Идентификатор выводится из номера в журнале: он же
/// задаёт порядок, поэтому два события с одним номером — ошибка теста,
/// а не повод для случайности.
fn событие(день: Date, номер: u32, вид: EventKind, ноги: Vec<Leg>) -> Event {
    Event {
        id: EventId(Uuid::from_u128(u128::from(номер))),
        schema_version: SCHEMA_VERSION,
        owner: ВЛАДЕЛЕЦ,
        account: СЧЁТ,
        kind: вид,
        dates: EventDates::for_cash(CashPostedDate(день)),
        order: EffectiveOrder::new(день, номер),
        legs: ноги,
        provenance: Provenance::new(
            ИСТОЧНИК,
            RawHash::parse(&"a".repeat(64)).expect("шестнадцатеричный хеш"),
            ParserVersion("test/scheduled-posting/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn пополнение(день: Date, номер: u32) -> Event {
    let сумма = рубли(10_000_000);
    событие(
        день,
        номер,
        EventKind::CashIn { amount: сумма },
        vec![Leg::cash(СЧЁТ, сумма)],
    )
}

/// Покупка облигации с заявленной датой сделки.
///
/// Дата сделки обязательна: именно её книга лотов кладёт в `Lot.acquired`,
/// а из неё берётся нижняя граница владения. Покупка без даты сделки
/// делает сверку недоказуемой — это отдельный случай, и он проверен
/// модульным тестом ядра.
fn покупка(депозитарий: CustodyId, день: Date, номер: u32) -> Event {
    let количество = Quantity(dec(КОЛИЧЕСТВО));
    let mut event = событие(
        день,
        номер,
        EventKind::Trade {
            side: TradeSide::Buy,
            instrument: БУМАГА,
            quantity: количество,
            gross: рубли(1_000_000),
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(СЧЁТ, рубли(-1_000_000)),
            Leg::security(СЧЁТ, депозитарий, БУМАГА, количество),
        ],
    );
    event.dates.trade = Some(TradeDate(день));
    event
}

/// Продажа всей партии из названного депозитария.
fn продажа(депозитарий: CustodyId, день: Date, номер: u32) -> Event {
    let количество = Quantity(dec(КОЛИЧЕСТВО));
    let mut event = событие(
        день,
        номер,
        EventKind::Trade {
            side: TradeSide::Sell,
            instrument: БУМАГА,
            quantity: количество,
            gross: рубли(1_000_000),
            fee: None,
            accrued_interest: None,
        },
        vec![
            Leg::cash(СЧЁТ, рубли(1_000_000)),
            Leg::security(СЧЁТ, депозитарий, БУМАГА, Quantity(dec("-10"))),
        ],
    );
    event.dates.trade = Some(TradeDate(день));
    event
}

/// Пришедший купон: дата зачисления денег и есть дата факта.
fn купон(день: Date, номер: u32) -> Event {
    let сумма = рубли(50_000);
    событие(
        день,
        номер,
        EventKind::Income {
            instrument: Some(БУМАГА),
            gross: сумма,
            kind: Some(IncomeKind::Coupon),
        },
        vec![Leg::cash(СЧЁТ, сумма)],
    )
}

/// Амортизационная выплата: половина номинала деньгами, количество бумаг
/// не меняется (§6.5). Единственный источник факта вида `PrincipalReturn`
/// у бумаги, которая ещё не погашена.
fn частичное_погашение(день: Date, номер: u32) -> Event {
    let компенсация = рубли(500_000);
    событие(
        день,
        номер,
        EventKind::CorporateAction {
            action: CorporateAction::PartialRedemption {
                instrument: БУМАГА,
                custody: ДЕПОЗИТАРИЙ,
                quantity: Quantity(dec(КОЛИЧЕСТВО)),
                principal_returned_per_unit: за_единицу("500"),
                compensation: компенсация,
                effective_date: день,
                record_date: None,
                grounds: None,
            },
        },
        vec![Leg::principal(СЧЁТ, БУМАГА, компенсация)],
    )
}

/// График выпуска: купонные даты и доли возврата номинала.
///
/// Полнота, флаги дефолта и валютные роли заданы явно — без них правило
/// потока отказывается строить план, `past` не появляется вовсе, и тест
/// проверял бы отказ построения, а не сверку. Период начинается
/// предыдущей выплатой: замкнутая цепь нужна расчёту НКД, иначе отказ
/// приходит оттуда.
fn график(купоны: &[Date], возвраты: &[(Date, &str)]) -> BondSchedule {
    BondSchedule {
        periods: купоны
            .iter()
            .enumerate()
            .map(|(индекс, дата)| AccrualPeriod {
                period_start: if индекс == 0 {
                    дата.saturating_sub(Duration::days(180))
                } else {
                    купоны[индекс - 1]
                },
                accrual_end: *дата,
                payment_date: *дата,
                coupon_per_unit: Some(за_единицу("50")),
            })
            .collect(),
        principal_returns: возвраты
            .iter()
            .map(|(дата, доля)| PrincipalReturn {
                repayment_date: *дата,
                share_percent: dec(доля),
            })
            .collect(),
        offer_windows: Vec::new(),
        completeness: ScheduleCompleteness::Validated,
        default_flags: Some(DefaultFlags {
            declared: false,
            technical: false,
        }),
        currency_roles: Some(CurrencyRoles::uniform(CurrencyCode::Rub)),
    }
}

/// Биржевая цена накануне отчёта: непокрытая позиция сама делает отчёт
/// неполным и скрыла бы вклад сверки в статус качества.
fn цена(дата_отчёта: Date) -> PriceCandidate {
    PriceCandidate {
        instrument: БУМАГА,
        price: dec("1000"),
        currency: CurrencyCode::Rub,
        basis: QuotationBasis::MoneyPerUnit,
        basis_evidence: "test:market".to_owned(),
        basis_evidence_contradicts: false,
        trade_date: дата_отчёта.saturating_sub(Duration::days(1)),
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
struct Сценарий<'a> {
    события: &'a [Event],
    график: &'a BondSchedule,
    дата_отчёта: Date,
    /// `None` — номинал лотам не подставляется, то есть воспроизводится
    /// рабочий путь `iaam-d8b.15`, на котором сверка молчит.
    номинал: Option<&'a str>,
}

fn отчёт(сценарий: &Сценарий<'_>) -> ReturnsReport {
    let contour = ContourDefinition::new(ContourId(Uuid::from_u128(7)), ContourVersion(1), [СЧЁТ]);
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let state = project(сценарий.события, &context)
        .expect("проекция журнала облигации")
        .state()
        .clone();
    let state = match сценарий.номинал {
        Some(номинал) => состояние_с_номиналом(&state, номинал),
        None => state,
    };
    let fx = iaam_core::valuation::FxTable::new(iaam_core::valuation::FxSource::OwnerSupplied);
    let ledger = ReconciliationLedger::default();
    let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
    let candidate = цена(сценарий.дата_отчёта);
    let schedules = BTreeMap::from([(БУМАГА, сценарий.график.clone())]);
    returns_report(
        &state,
        &ReturnsRequest {
            contour: &contour,
            as_of: сценарий.дата_отчёта,
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

/// Номинал каждой партии в состоянии проекции.
///
/// Живёт здесь, а не берётся из ядра: помощник ядра лежит в `mod tests`
/// внутри `returns/mod.rs` и внешнему потребителю недоступен, а
/// публиковать внутренности ядра ради теста нельзя. Работает через тот
/// же публичный `serde`, что и отпечаток состояния, и исчезнет вместе
/// с `iaam-d8b.15`: как только покупка начнёт ставить номинал, подмена
/// станет не нужна.
///
/// Число замен проверяется: если поле переименуют, молчаливая подмена
/// нуля партий вернула бы весь файл в проверку пустоты.
fn состояние_с_номиналом(state: &LedgerState, номинал: &str) -> LedgerState {
    fn известный(номинал: &str) -> ciborium::Value {
        let principal = PrincipalState::known(за_единицу(номинал), за_единицу(номинал))
            .expect("известный номинал");
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&principal, &mut bytes).expect("сериализация номинала");
        ciborium::de::from_reader(bytes.as_slice()).expect("разбор номинала")
    }

    fn подменить(value: &mut ciborium::Value, номинал: &ciborium::Value, заменено: &mut usize) {
        match value {
            ciborium::Value::Map(entries) => {
                for (key, value) in entries {
                    if matches!(key, ciborium::Value::Text(text) if text == "principal") {
                        *value = номинал.clone();
                        *заменено += 1;
                    } else {
                        подменить(value, номинал, заменено);
                    }
                }
            }
            ciborium::Value::Array(values) => {
                for value in values {
                    подменить(value, номинал, заменено);
                }
            }
            ciborium::Value::Tag(_, value) => подменить(value, номинал, заменено),
            _ => {}
        }
    }

    let mut bytes = Vec::new();
    ciborium::ser::into_writer(state, &mut bytes).expect("сериализация состояния");
    let mut value: ciborium::Value =
        ciborium::de::from_reader(bytes.as_slice()).expect("разбор состояния");
    let mut заменено = 0;
    подменить(&mut value, &известный(номинал), &mut заменено);
    assert!(
        заменено > 0,
        "ни одной партии не досталось номинала: подмена перестала работать"
    );
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes).expect("сериализация изменённого состояния");
    ciborium::de::from_reader(bytes.as_slice()).expect("разбор изменённого состояния")
}

fn непринятые(report: &ReturnsReport) -> Vec<&MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| matches!(issue, MaterialIssue::ScheduledPostingNotReceived { .. }))
        .collect()
}

fn недоказуемые(report: &ReturnsReport) -> Vec<&MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| matches!(issue, MaterialIssue::ScheduledPostingUnverifiable { .. }))
        .collect()
}

/// Вердикт сверки: обе её проблемы в порядке отчёта.
fn вердикт(report: &ReturnsReport) -> Vec<MaterialIssue> {
    report
        .data_quality
        .material_issues
        .iter()
        .filter(|issue| {
            matches!(
                issue,
                MaterialIssue::ScheduledPostingNotReceived { .. }
                    | MaterialIssue::ScheduledPostingUnverifiable { .. }
            )
        })
        .cloned()
        .collect()
}

/// Поток построен, значит сверка до плана дошла.
///
/// Без этой проверки молчание сверки нельзя отличить от несостоявшегося
/// сценария: отказ построения пропускает сверку целиком.
fn поток_построен(report: &ReturnsReport) {
    assert!(
        !report.bond_metrics.is_empty(),
        "облигационная позиция не попала в отчёт: проверять нечего"
    );
    for позиция in &report.bond_metrics {
        assert!(
            matches!(
                позиция.scenarios[0].prospective.metrics,
                Computed::Value(_)
            ),
            "поток не построен: {:?}",
            позиция.scenarios[0].prospective.metrics
        );
    }
}

/// Купонные даты, срок которых к дате отчёта уже прошёл: пять лет
/// полугодовых выплат.
const ПРОШЛЫЕ_КУПОНЫ: [Date; 10] = [
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

/// График пятилетней бумаги: прошлые купоны, ближайший будущий (он
/// накрывает дату отчёта и нужен расчёту НКД) и погашение.
fn график_пятилетней_бумаги() -> BondSchedule {
    let mut купоны = ПРОШЛЫЕ_КУПОНЫ.to_vec();
    купоны.push(date!(2026 - 09 - 15));
    купоны.push(date!(2027 - 03 - 15));
    график(&купоны, &[(date!(2027 - 03 - 15), "100")])
}

/// Пятилетняя история: покупка и купоны, пришедшие с задержкой
/// депозитарной цепочки.
///
/// Сдвиг факта от плановой даты гуляет по всему разрешённому диапазону
/// 1–7 дней: реальная выплата приходит не день в день, и тест обязан
/// быть зелёным именно на таком журнале, а не на выдуманном идеальном.
fn журнал_пятилетней_истории(пропущенный: Option<Date>) -> Vec<Event> {
    let mut events = vec![
        пополнение(date!(2021 - 07 - 25), 1),
        покупка(ДЕПОЗИТАРИЙ, date!(2021 - 08 - 01), 2),
    ];
    let mut номер = 10;
    for (индекс, дата) in ПРОШЛЫЕ_КУПОНЫ.iter().enumerate() {
        if Some(*дата) == пропущенный {
            continue;
        }
        let сдвиг = i64::try_from(индекс % 7).expect("номер купона") + 1;
        events.push(купон(дата.saturating_add(Duration::days(сдвиг)), номер));
        номер += 1;
    }
    events
}

#[test]
fn five_years_of_coupons_received_late_but_received_raise_no_alarm() {
    // Главный критерий эпика: здоровая бумага молчит. Задержка выплаты
    // на 1–7 дней — норма депозитарной цепочки, а не дефект, и если
    // такой журнал даёт хоть одну тревогу, сверка бесполезна: владелец
    // перестанет читать предупреждения на второй неделе.
    let report = отчёт(&Сценарий {
        события: &журнал_пятилетней_истории(None),
        график: &график_пятилетней_бумаги(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });

    поток_построен(&report);
    assert!(вердикт(&report).is_empty(), "вердикт: {:?}", вердикт(&report));
}

#[test]
fn an_amortised_bond_closes_its_principal_returns_with_partial_redemptions() {
    // Возврат номинала подтверждается корпоративным действием, а купон —
    // доходом. Если бы `past` носил одни даты без вида, эти два факта
    // стали бы взаимозаменяемыми, и пропущенная амортизация закрылась бы
    // пришедшим купоном. Контрольный прогон ниже показывает, что вид
    // выплаты действительно проверяется.
    let график = график(
        &[date!(2026 - 03 - 15), date!(2026 - 09 - 15)],
        &[(date!(2026 - 06 - 15), "50"), (date!(2026 - 09 - 15), "50")],
    );
    let здоровый = vec![
        пополнение(date!(2026 - 01 - 05), 1),
        покупка(ДЕПОЗИТАРИЙ, date!(2026 - 01 - 10), 2),
        купон(date!(2026 - 03 - 17), 3),
        частичное_погашение(date!(2026 - 06 - 17), 4),
    ];
    let report = отчёт(&Сценарий {
        события: &здоровый,
        график: &график,
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });

    поток_построен(&report);
    assert!(вердикт(&report).is_empty(), "вердикт: {:?}", вердикт(&report));

    // Тот же журнал без амортизационной выплаты: пропущен возврат
    // номинала, и назван он должен быть именно как `PrincipalReturn`.
    let без_амортизации: Vec<Event> = здоровый[..3].to_vec();
    let report = отчёт(&Сценарий {
        события: &без_амортизации,
        график: &график,
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });

    поток_построен(&report);
    let issues = непринятые(&report);
    assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                date,
                kind: PostingKind::PrincipalReturn,
                ..
            } if *date == date!(2026 - 06 - 15)
        ),
        "проблема: {:?}",
        issues[0]
    );
}

#[test]
fn a_single_gap_in_the_middle_of_the_history_is_named_once_and_exactly() {
    // Пропуск в середине ряда — тот случай, ради которого сопоставление
    // сделано one-to-one: жадное сопоставление «любой факт закрывает
    // любую выплату» съело бы дыру соседними купонами и промолчало.
    let пропущенный = date!(2023 - 09 - 15);
    let report = отчёт(&Сценарий {
        события: &журнал_пятилетней_истории(Some(пропущенный)),
        график: &график_пятилетней_бумаги(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });

    поток_построен(&report);
    let issues = непринятые(&report);
    assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    assert!(
        matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                account,
                instrument,
                date,
                kind: PostingKind::Coupon,
            } if *account == СЧЁТ && *instrument == БУМАГА && *date == пропущенный
        ),
        "проблема: {:?}",
        issues[0]
    );
    assert!(недоказуемые(&report).is_empty());

    // Тот же вопрос ко всему ряду: изъятие любого из десяти купонов
    // обязано давать ровно одну проблему — свою. Без этого прохода
    // сверка могла бы проверять два-три купона из десяти, а остальные
    // молча пропускать, и первый тест файла всё равно был бы зелёным.
    for пропущенный in ПРОШЛЫЕ_КУПОНЫ {
        let report = отчёт(&Сценарий {
            события: &журнал_пятилетней_истории(Some(пропущенный)),
            график: &график_пятилетней_бумаги(),
            дата_отчёта: ДАТА_ОТЧЁТА,
            номинал: Some(НОМИНАЛ),
        });
        let issues = непринятые(&report);
        assert_eq!(issues.len(), 1, "купон {пропущенный}: {issues:?}");
        assert!(
            matches!(
                issues[0],
                MaterialIssue::ScheduledPostingNotReceived { date, .. }
                    if *date == пропущенный
            ),
            "купон {пропущенный}: {:?}",
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
    let плановая = date!(2026 - 03 - 15);
    let график = график(&[плановая, date!(2026 - 09 - 15)], &[(date!(2026 - 09 - 15), "100")]);
    let события = vec![
        пополнение(date!(2026 - 01 - 05), 1),
        покупка(ДЕПОЗИТАРИЙ, date!(2026 - 01 - 10), 2),
    ];
    let на = |сдвиг: i64| {
        отчёт(&Сценарий {
            события: &события,
            график: &график,
            дата_отчёта: плановая.saturating_add(Duration::days(сдвиг)),
            номинал: Some(НОМИНАЛ),
        })
    };

    let ещё_идёт = на(20);
    поток_построен(&ещё_идёт);
    assert!(
        вердикт(&ещё_идёт).is_empty(),
        "на двадцатый день срок ещё идёт: {:?}",
        вердикт(&ещё_идёт)
    );

    let истёк = на(21);
    поток_построен(&истёк);
    assert_eq!(непринятые(&истёк).len(), 1, "проблемы: {:?}", непринятые(&истёк));

    let давно_истёк = на(22);
    поток_построен(&давно_истёк);
    assert_eq!(
        непринятые(&давно_истёк).len(),
        1,
        "проблемы: {:?}",
        непринятые(&давно_истёк)
    );
}

/// Две покупки и продажа ранней партии из того же депозитария.
///
/// Один депозитарий на весь журнал: иначе продажа списала бы бумагу
/// оттуда, куда её не клали, и позиций стало бы три — тест проверял бы
/// задвоение, а не границу владения.
fn журнал_с_проданной_ранней_партией(факты: &[Date]) -> Vec<Event> {
    let mut events = vec![
        пополнение(date!(2026 - 01 - 05), 1),
        покупка(ДЕПОЗИТАРИЙ, date!(2026 - 01 - 10), 2),
        покупка(ДЕПОЗИТАРИЙ, date!(2026 - 04 - 10), 3),
        продажа(ДЕПОЗИТАРИЙ, date!(2026 - 07 - 10), 4),
    ];
    for (индекс, день) in факты.iter().enumerate() {
        events.push(купон(*день, 10 + u32::try_from(индекс).expect("номер факта")));
    }
    events
}

/// График бумаги с двумя прошедшими купонами и погашением в декабре.
fn график_двух_купонов() -> BondSchedule {
    график(
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
    let report = отчёт(&Сценарий {
        события: &журнал_с_проданной_ранней_партией(&[date!(2026 - 06 - 16)]),
        график: &график_двух_купонов(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });

    поток_построен(&report);
    let issues = непринятые(&report);
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
    let report = отчёт(&Сценарий {
        события: &журнал_с_проданной_ранней_партией(&[
            date!(2026 - 03 - 16),
            date!(2026 - 06 - 16),
        ]),
        график: &график_двух_купонов(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });

    поток_построен(&report);
    assert!(вердикт(&report).is_empty(), "вердикт: {:?}", вердикт(&report));
}

#[test]
fn one_bond_in_two_custodies_reports_a_single_missing_coupon() {
    // Позиции обходятся по месту хранения, а сверяется пара
    // (счёт, бумага) без него: одна и та же выплата иначе была бы
    // названа по разу на депозитарий, и владелец искал бы два пропуска
    // вместо одного.
    let события = vec![
        пополнение(date!(2026 - 01 - 05), 1),
        покупка(ДЕПОЗИТАРИЙ, date!(2026 - 01 - 10), 2),
        покупка(ДРУГОЙ_ДЕПОЗИТАРИЙ, date!(2026 - 01 - 11), 3),
        купон(date!(2026 - 03 - 16), 4),
    ];
    let report = отчёт(&Сценарий {
        события: &события,
        график: &график_двух_купонов(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });

    поток_построен(&report);
    assert_eq!(
        report.bond_metrics.len(),
        2,
        "позиций должно быть две, иначе тест ничего не доказывает"
    );
    let issues = непринятые(&report);
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
fn переставленный(события: &[Event], семя: u64) -> Vec<Event> {
    let mut порядок = события.to_vec();
    let mut состояние = семя;
    for индекс in (1..порядок.len()).rev() {
        состояние = состояние
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let выбор = usize::try_from(состояние >> 33).expect("старшие разряды семени")
            % (индекс + 1);
        порядок.swap(индекс, выбор);
    }
    порядок
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
    let события = журнал_пятилетней_истории(Some(date!(2023 - 09 - 15)));
    let эталон = вердикт(&отчёт(&Сценарий {
        события: &события,
        график: &график_пятилетней_бумаги(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    }));
    assert_eq!(эталон.len(), 1, "вердикт эталона: {эталон:?}");

    // Тасование обязано что-то переставлять: свойство, проверенное на
    // тождественной перестановке, не проверяет ничего.
    let mut порядок_менялся = false;
    for семя in 1..=32_u64 {
        let переставленные = переставленный(&события, семя);
        порядок_менялся |= переставленные != события;
        let вердикт_перестановки = вердикт(&отчёт(&Сценарий {
            события: &переставленные,
            график: &график_пятилетней_бумаги(),
            дата_отчёта: ДАТА_ОТЧЁТА,
            номинал: Some(НОМИНАЛ),
        }));
        assert_eq!(
            вердикт_перестановки, эталон,
            "перестановка с семенем {семя} изменила вердикт"
        );
    }

    assert!(порядок_менялся, "тасование не переставило ни один журнал");

    // Обратный порядок — не случайная перестановка, а самый вероятный
    // способ прочитать выгрузку брокера задом наперёд.
    let mut обратный = события.clone();
    обратный.reverse();
    assert_eq!(
        вердикт(&отчёт(&Сценарий {
            события: &обратный,
            график: &график_пятилетней_бумаги(),
            дата_отчёта: ДАТА_ОТЧЁТА,
            номинал: Some(НОМИНАЛ),
        })),
        эталон,
        "обратный порядок изменил вердикт"
    );
}

#[test]
fn projecting_the_same_journal_twice_gives_the_same_verdict() {
    // §15.3: повторная проекция того же журнала обязана дать то же
    // состояние и тот же отчёт. Сверяется и отпечаток состояния, и
    // отпечаток входов отчёта, и сам вердикт: сверка читает состояние,
    // а не журнал, и разойтись они могут порознь.
    let события = журнал_пятилетней_истории(Some(date!(2024 - 03 - 15)));
    let сценарий = Сценарий {
        события: &события,
        график: &график_пятилетней_бумаги(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    };

    let первый = отчёт(&сценарий);
    let второй = отчёт(&сценарий);

    assert_eq!(вердикт(&первый).len(), 1, "вердикт: {:?}", вердикт(&первый));
    assert_eq!(вердикт(&первый), вердикт(&второй));
    assert_eq!(первый.inputs_hash, второй.inputs_hash);
}

#[test]
fn without_the_face_value_the_reconciliation_is_silently_skipped() {
    // Доказательство, что остальные тесты этого файла не проверяют
    // пустоту. Номинал не выставляет ни одна рабочая запись журнала
    // (`iaam-d8b.15`), поэтому без подмены правило потока отказывается
    // строить план и сверка пропускается целиком — тот же журнал
    // с изъятым купоном не даёт ни одной проблемы. Как только дыра
    // будет закрыта, подмена станет лишней, а этот тест — красным
    // напоминанием её убрать.
    let события = журнал_пятилетней_истории(Some(date!(2023 - 09 - 15)));

    let без_номинала = отчёт(&Сценарий {
        события: &события,
        график: &график_пятилетней_бумаги(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: None,
    });
    assert!(
        matches!(
            без_номинала.bond_metrics[0].scenarios[0].prospective.metrics,
            Computed::NotComputable {
                reason: NotComputable::PrincipalUnknown
            }
        ),
        "без номинала поток обязан отказываться по названной причине: {:?}",
        без_номинала.bond_metrics[0].scenarios[0].prospective.metrics
    );
    assert!(
        вердикт(&без_номинала).is_empty(),
        "без номинала сверке нечего сказать: {:?}",
        вердикт(&без_номинала)
    );

    let с_номиналом = отчёт(&Сценарий {
        события: &события,
        график: &график_пятилетней_бумаги(),
        дата_отчёта: ДАТА_ОТЧЁТА,
        номинал: Some(НОМИНАЛ),
    });
    поток_построен(&с_номиналом);
    assert_eq!(
        непринятые(&с_номиналом).len(),
        1,
        "подмена номинала обязана менять исход: {:?}",
        вердикт(&с_номиналом)
    );
}
