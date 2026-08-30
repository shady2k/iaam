//! Отчёт о доходности (§6.1, §10.5, §16.3).
//!
//! Честная формулировка результата этапа 1: **XIRR до налога** для
//! простых long-only бумаг. Налоги появляются в E5, и до тех пор ни
//! одно поле этого отчёта не притворяется доходностью после налога.
//!
//! **Период отчёта — вся история счёта.** XIRR за произвольный интервал
//! требует оценки NAV на начало интервала как терминального потока,
//! а оценка на этапе 1 существует только на дату отчёта. Считать
//! интервал, подставив вместо начальной стоимости себестоимость,
//! означало бы выдать за доходность величину, которой не соответствует
//! ни одна сделка.

pub mod xirr;
pub mod zero_reinvestment;

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Date, OffsetDateTime, UtcOffset};

use crate::bond::{
    BondSchedule,
    finality::{PrincipalReturnFinality, finality_of},
    offer::{OfferChoice, available_choices},
    posting::next_posting_date,
};
use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::ids::{AccountId, InstrumentId, SourceId};
use crate::money::PerUnitAmount;
use crate::money::{CalcMoney, CurrencyCode};
use crate::numeric::approx::SolverPolicy;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverRefusal};
use crate::perimeter::{PerimeterAssessment, PerimeterPolicy};
use crate::projection::lots::{LotBook, LotKey};
use crate::projection::offers::{OfferBook, unresolved_submissions};
use crate::projection::ownership::Ownership;
use crate::projection::state::LedgerState;
use crate::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use crate::rules::lot_disposal::RuleId;
use crate::rules::quotation::{QuotationError, QuotationRule, QuotationRuleVersion, QuotationV1};
use crate::rules::{
    AccruedInterestError, AccruedInterestRule, AccruedInterestRuleVersion, AccruedInterestV1,
    CashflowInput, CashflowPlan, CashflowProjectionVersion, PostingKind, PostingMatchV2,
    PostingMatchVersion, SourcePriorityVersion, ValuationPolicyV1, ValuationRule, Verdict,
    historical_schedule_postings,
};
use crate::valuation::{
    FxSource, FxTable, LegacyValuationOutcome, PriceCandidate, PriceQuality, PriceQuery,
    QuotationBasis, SelectedPrice, SourceExecutability, UncoveredReason as PolicyUncoveredReason,
    ValuationError, Venue, candidate_from_legacy_valuation,
};

/// Величина, которую система может отказаться вычислить.
///
/// Отказ — часть контракта, а не исключительная ситуация: неизвестная
/// цена, отсутствующий курс и уравнение без единственного корня
/// встречаются в нормальной работе (§5.4, §6.1, §10.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Computed<T> {
    Value(T),
    NotComputable { reason: NotComputable },
}

impl<T> Computed<T> {
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Value(v) => Some(v),
            Self::NotComputable { .. } => None,
        }
    }

    #[must_use]
    pub const fn reason(&self) -> Option<&NotComputable> {
        match self {
            Self::Value(_) => None,
            Self::NotComputable { reason } => Some(reason),
        }
    }
}

/// Почему величина не вычислена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotComputable {
    /// Нет цены инструмента: стоимость позиции неизвестна.
    MissingPrice { instrument: InstrumentId },
    /// Нет курса на дату.
    MissingFxRate {
        from: CurrencyCode,
        to: CurrencyCode,
        date: Date,
    },
    /// Основание котировки не доказано источником.
    QuotationBasisUnknown { instrument: InstrumentId },
    /// Записанное основание противоречит доказательству источника.
    QuotationBasisContradictsEvidence { instrument: InstrumentId },
    /// Номинал бумаги неизвестен.
    RemainingFaceUnknown { instrument: InstrumentId },
    /// Лоты одной пары «счёт и бумага» несут разные номиналы.
    RemainingFaceAmbiguous { instrument: InstrumentId },
    /// Для пересчёта котировки не передан номинал.
    PrincipalUnknown,
    /// Решатель отказался: корня нет, корней несколько, не сошлось.
    SolverRefused { refusal: SolverRefusal },
    /// Ни одного потока, пересекающего границу контура.
    NoExternalFlows,
    /// Срез журнала содержит события позже даты отчёта: он собран неверно.
    StateNewerThanReport { last_event: Date, as_of: Date },
    /// Арифметическая невозможность: переполнение, деление на ноль.
    Numeric { code: &'static str },
    /// На счёте финансирование вне периметра: экономику система не достраивает.
    UnsupportedFinancing { account: AccountId },
    /// Снимка графика выпуска на координату знания отсутствует.
    ScheduleMissing { instrument: InstrumentId },
    /// Наблюдение НКД на дату выхода отсутствует.
    AccruedObservationMissing { instrument: InstrumentId },
    /// Сумма купона текущего периода не определена.
    CouponUndetermined { instrument: InstrumentId },
    /// Дата отчёта вне покрытия графика.
    OutsideScheduleCoverage { instrument: InstrumentId },
    /// Дата отчёта покрыта несколькими периодами графика.
    OverlappingScheduleCoverage { instrument: InstrumentId },
    /// Исполнимого выхода нет: реализовать НКД сегодня нельзя.
    ExitNotExecutable,
    /// Дата окончания горизонта не позже координаты метрики.
    NonPositiveDuration {
        coordinate: Date,
        terminal_date: Date,
    },
    /// Начальная стоимость не положительна.
    NonPositiveInitialCapital,
    /// Терминальное благосостояние отрицательно.
    NegativeTerminalWealth,
    /// Лоты несут разные состояния номинала.
    PrincipalStateAmbiguous { instrument: InstrumentId },
    /// Историческая стоимость приобретения когорты неизвестна.
    AcquisitionBasisUnknown,
    /// Уплаченный при приобретении НКД неизвестен.
    AccruedInterestAtAcquisitionUnknown,
    /// История полученных выплат агрегирована неизвестно.
    HistoricalReceiptsUnknown,
    /// Когорта не может быть построена.
    CohortGap {
        gap: crate::projection::lots::CohortGap,
    },
    /// Денежные величины имеют разные валюты.
    CurrencyMismatch {
        expected: CurrencyCode,
        actual: CurrencyCode,
    },
    /// Расход неизвестен и не ограничен сверху.
    ExpenseUnknown,
}

impl NotComputable {
    /// Машиночитаемый код для API (§13). Внешний агент разбирает код,
    /// а текст предназначен человеку.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingPrice { .. } => "missing_price",
            Self::MissingFxRate { .. } => "missing_fx_rate",
            Self::QuotationBasisContradictsEvidence { .. } => {
                "quotation_basis_contradicts_evidence"
            }
            Self::QuotationBasisUnknown { .. } => "quotation_basis_unknown",
            Self::RemainingFaceUnknown { .. } => "remaining_face_unknown",
            Self::PrincipalUnknown => "principal_unknown",
            Self::RemainingFaceAmbiguous { .. } => "remaining_face_ambiguous",
            Self::SolverRefused { .. } => "solver_refused",
            Self::NoExternalFlows => "no_external_flows",
            Self::StateNewerThanReport { .. } => "state_newer_than_report",
            Self::Numeric { .. } => "numeric",
            Self::UnsupportedFinancing { .. } => "unsupported_financing",
            Self::ScheduleMissing { .. } => "schedule_missing",
            Self::AccruedObservationMissing { .. } => "accrued_observation_missing",
            Self::CouponUndetermined { .. } => "coupon_undetermined",
            Self::OutsideScheduleCoverage { .. } => "outside_schedule_coverage",
            Self::OverlappingScheduleCoverage { .. } => "overlapping_schedule_coverage",
            Self::ExitNotExecutable => "exit_not_executable",
            Self::NonPositiveDuration { .. } => "non_positive_duration",
            Self::NonPositiveInitialCapital => "non_positive_initial_capital",
            Self::NegativeTerminalWealth => "negative_terminal_wealth",
            Self::PrincipalStateAmbiguous { .. } => "principal_state_ambiguous",
            Self::AcquisitionBasisUnknown => "acquisition_basis_unknown",
            Self::AccruedInterestAtAcquisitionUnknown => "accrued_interest_at_acquisition_unknown",
            Self::HistoricalReceiptsUnknown => "historical_receipts_unknown",
            Self::CohortGap { .. } => "cohort_gap",
            Self::CurrencyMismatch { .. } => "currency_mismatch",
            Self::ExpenseUnknown => "expense_unknown",
        }
    }
}

impl From<ValuationError> for NotComputable {
    fn from(error: ValuationError) -> Self {
        match error {
            ValuationError::MissingPrice { instrument } => Self::MissingPrice { instrument },
            ValuationError::MissingFxRate { from, to, date } => {
                Self::MissingFxRate { from, to, date }
            }
            ValuationError::Numeric(_) => Self::Numeric { code: "numeric" },
        }
    }
}

/// Состояние качества данных (§10.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataQualityStatus {
    /// Все данные подтверждены. На этапе 1 недостижимо: сверки нет.
    Clean,
    /// Часть данных не подтверждена независимо.
    Mixed,
    /// Данных не хватает для полного ответа.
    Incomplete,
}

impl DataQualityStatus {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Mixed => "mixed",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Материальная проблема качества данных. Показывается владельцу
/// только тогда, когда влияет на ответ (§10.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialIssue {
    /// Позиция восстановлена без документированной стоимости (§10.7).
    RestoredWithoutBasis { account: AccountId },
    /// Отрицательный денежный остаток — обязательство в NAV (§15.9).
    NegativeCash {
        account: AccountId,
        currency: CurrencyCode,
    },
    /// Данных до этой даты нет; всё, что раньше, в расчёт не вошло.
    HistoryStartsAt { date: Date },
    /// Независимого подтверждения по счёту нет (§10.5).
    NoIndependentSource {
        account: AccountId,
        dimension: Dimension,
    },
    /// Сверка по счёту не сходится.
    Discrepancy {
        account: AccountId,
        dimension: Dimension,
    },
    /// На счёте присутствует финансирование вне периметра (§11).
    UnsupportedFinancing { account: AccountId },
    /// Поданная заявка ссылается на окно, которого нет в графике.
    OfferWindowUnresolved {
        submission: crate::event::offer::OfferSubmissionId,
    },
    /// Запланированная выплата не подтверждена датированным фактом
    /// дохода.
    ///
    /// Счёт обязателен: одна бумага на двух счетах иначе даёт две
    /// неразличимые проблемы. Вид выплаты обязателен: «не пришёл купон»
    /// и «не пришёл возврат номинала» требуют от владельца разных
    /// действий и ищутся в разных событиях журнала.
    ScheduledPostingNotReceived {
        account: AccountId,
        instrument: InstrumentId,
        date: Date,
        kind: PostingKind,
    },
    /// Сверку одной запланированной выплаты провести нечем.
    ///
    /// Дата и вид делают проблему адресной: недоказуемость одной выплаты
    /// не должна скрывать доказуемый пропуск другой выплаты той же пары.
    ScheduledPostingUnverifiable {
        account: AccountId,
        instrument: InstrumentId,
        date: Date,
        kind: PostingKind,
        reason: UnverifiableReason,
    },
    /// Несколько запланированных выплат недоказуемы по одной причине.
    ///
    /// Свёртка, а не список одиночных проблем: причина уровня источника
    /// чинится одним действием, и десять одинаковых строк не несут
    /// десяти единиц информации. Число и границы дат оставлены,
    /// потому что «где-то в истории плохая дата» так же бесполезно,
    /// как десять повторов.
    ScheduledPostingsUnverifiable {
        account: AccountId,
        instrument: InstrumentId,
        kind: PostingKind,
        reason: UnverifiableReason,
        count: u32,
        first_date: Date,
        last_date: Date,
    },
    /// Расчётный и наблюдённый НКД разошлись больше допуска.
    AccruedInterestMismatch {
        instrument: InstrumentId,
        computed: Dec,
        computed_currency: CurrencyCode,
        observed: Dec,
        observed_currency: CurrencyCode,
        quantity: crate::money::Quantity,
        date: Date,
    },
}

impl MaterialIssue {
    /// Делает ли проблема ответ **неполным**.
    ///
    /// Две проблемы неполнотой не являются и потому не переводят статус
    /// в `Incomplete`:
    ///
    /// - начало истории — это факт о периоде, а не дефект (§10.7);
    /// - отсутствие независимого источника — нормальное состояние
    ///   данных: §10.5 прямо требует считать такие записи в отчётах по
    ///   умолчанию, иначе система бесполезна именно для банков без
    ///   экспорта и ручного ввода. Показывать это надо, объявлять ответ
    ///   неполным — нельзя, иначе `Incomplete` перестанет что-либо
    ///   означать, потому что будет стоять почти всегда.
    ///
    /// Насколько велика неподтверждённая доля, говорит `navCoverage`,
    /// а не статус.
    #[must_use]
    pub const fn is_defect(&self) -> bool {
        match self {
            Self::HistoryStartsAt { .. } | Self::NoIndependentSource { .. } => false,
            // Горизонт журнала зеркалит `HistoryStartsAt`: это факт о
            // периоде, а не дефект. Владелец, чей журнал начинается
            // позже выпуска бумаги, иначе получил бы вечный `Incomplete`
            // Остальные шесть причин чинятся дозагрузкой фактов и потому
            // являются дефектами.
            Self::ScheduledPostingUnverifiable { reason, .. }
            | Self::ScheduledPostingsUnverifiable { reason, .. } => {
                !matches!(reason, UnverifiableReason::HistoryStartsAfterSchedule)
            }
            Self::AccruedInterestMismatch { .. }
            | Self::RestoredWithoutBasis { .. }
            | Self::NegativeCash { .. }
            | Self::Discrepancy { .. }
            | Self::UnsupportedFinancing { .. }
            | Self::OfferWindowUnresolved { .. }
            | Self::ScheduledPostingNotReceived { .. } => true,
        }
    }
}

/// Почему сверку запланированных выплат провести нечем (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnverifiableReason {
    /// Границу владения провести нечем: у партии нет даты приобретения
    /// либо есть количество, восстановленное без стоимости.
    AcquisitionDateUnknown,
    /// Владение на дату фиксации выплаты нельзя доказать.
    OwnershipUnknown,
    /// Источник не сообщил дату, на которую определяется право на выплату.
    EntitlementDateUnknown,
    /// По паре есть выплата, вид которой не установлен: на график её
    /// не положить.
    IncomeKindUnknown,
    /// По паре есть выплата без даты зачисления и без даты выплаты.
    PaymentDateUnknown,
    /// Выплата прошла границу владения, но её дата раньше первого
    /// события журнала: фактов под неё нет и быть не может.
    HistoryStartsAfterSchedule,
    /// График нельзя использовать как доказательство прошлых выплат.
    ScheduleNotTrusted,
}
impl UnverifiableReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AcquisitionDateUnknown => "acquisition_date_unknown",
            Self::OwnershipUnknown => "ownership_unknown",
            Self::EntitlementDateUnknown => "entitlement_date_unknown",
            Self::IncomeKindUnknown => "income_kind_unknown",
            Self::PaymentDateUnknown => "payment_date_unknown",
            Self::HistoryStartsAfterSchedule => "history_starts_after_schedule",
            Self::ScheduleNotTrusted => "schedule_not_trusted",
        }
    }
}

/// Причина, по которой позиция не вошла в денежную стоимость.
///
/// Первые варианты описывают решение политики выбора цены. Вариант
/// `NotComputable` сохраняет конкретную причину отказа пересчёта, чтобы
/// покрытие не объявляло позицию оценённой без денежного вклада.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UncoveredReason {
    /// Для инструмента нет ни одного наблюдения.
    NoObservation,
    /// Все наблюдения старше предельного возраста.
    TooOld,
    /// Нельзя однозначно определить площадку.
    AmbiguousVenue,
    /// После отбора осталось несколько кандидатов.
    AmbiguousCandidate,
    /// Выбранное наблюдение нельзя перевести в деньги.
    NotComputable { reason: NotComputable },
}

/// Позиция без выбранного кандидата и причина отсутствия покрытия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncoveredPosition {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub reason: UncoveredReason,
}

/// Позиция, оставшаяся на вычисленном старым правилом значении.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyDerivedPosition {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub quality: PriceQuality,
}

/// Позиция с выбранным кандидатом и полным основанием решения политики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedPosition {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub quantity: crate::money::Quantity,
    pub price: SelectedPrice,
}
/// Атрибуты облигационной позиции (§5.1: атрибуты, не оценочная база).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondPositionAttributes {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    /// Начисленный на дату доход по позиции: НКД на бумагу × количество.
    pub accrued_interest: Computed<Dec>,
    /// Фактически реализуемая сегодня сумма (§4.2). Не договорная.
    pub accrued_interest_payable_on_termination: Computed<Dec>,
    /// Ближайшая любая выплата.
    pub next_posting_date: Option<Date>,
    /// Окончателен ли ближайший возврат номинала, если он и есть.
    pub next_principal_return_finality: Option<PrincipalReturnFinality>,
}
/// Метрики всех сценариев одной облигационной позиции.
#[derive(Debug, Clone, PartialEq)]
pub struct BondPositionMetrics {
    pub account: AccountId,
    pub custody: Option<crate::ids::CustodyId>,
    pub instrument: InstrumentId,
    pub scenarios: Vec<crate::returns::zero_reinvestment::BondScenarioResult>,
}

/// Покрытие ценой: только количество позиций, без выдуманного денежного
/// знаменателя для позиций, которым цена не найдена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionCoverage {
    pub evaluated_positions: u32,
    pub total_positions: u32,
    pub selected: Vec<EvaluatedPosition>,
    pub uncovered: Vec<UncoveredPosition>,
    pub legacy_derived: Vec<LegacyDerivedPosition>,
}

/// Доли исполнимости от стоимости **оценённых позиций**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutabilityShares {
    pub evaluated_positions_value: Dec,
    pub executable: Dec,
    pub indicative_previous_close: Dec,
    pub unknown: Dec,
}

/// Денежная величина, для которой отсутствие знания нельзя заменить нулём.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountQualification {
    Known(Dec),
    Unknown,
}

/// Оценка до издержек выхода и до налога.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidationEstimate {
    pub value_before_exit_costs_and_tax: Computed<Dec>,
    pub executability: ExecutabilityShares,
    pub exit_costs: AmountQualification,
    pub tax: AmountQualification,
    /// Реализуемый сегодня НКД по всем облигационным позициям.
    ///
    /// `NotComputable` здесь делает неполноценной именно эту оценку,
    /// но не переносит неизвестность в `terminal_value` (§4.2).
    pub accrued_interest_payable_on_termination: Computed<Dec>,
}

/// Покрытие стоимости портфеля уровнями достоверности (§10.5).
///
/// Доли считаются по **модулю** стоимости счёта: счёт с отрицательным
/// остатком тоже покрыт или не покрыт сверкой, и выбросить его значило
/// бы посчитать долю от неполного портфеля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavCoverage {
    pub accepted_independent: Dec,
    pub accepted_internal: Dec,
    pub provisional: Dec,
    /// Доля стоимости, по которой сверка не сходится.
    ///
    /// §10.5 показывает в примере три доли. Четвёртая добавлена
    /// намеренно: без неё расходящийся счёт попадал бы в `provisional`
    /// и выглядел как «просто пока не подтверждён» — то есть проблема
    /// пряталась бы ровно в той цифре, которая существует, чтобы её
    /// показывать.
    pub discrepant: Dec,
}

/// Блок качества данных.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataQuality {
    pub status: DataQualityStatus,
    /// Сверка денежных и позиционных измерений по счетам.
    pub nav_coverage: NavCoverage,
    /// Покрытие ценами и причины непокрытых позиций.
    pub position_coverage: PositionCoverage,
    /// Доли исполнимости от стоимости оценённых позиций.
    pub executability: ExecutabilityShares,
    pub material_issues: Vec<MaterialIssue>,
}
/// Что именно применялось при расчёте. Без этого цифру не воспроизвести
/// (§3.2, §6.1).
///
/// `Eq` не выводится: политика решателя содержит допуск в двоичной
/// плавающей точке, а равенство таких величин не рефлексивно.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedRules {
    pub contour: ContourId,
    pub contour_version: ContourVersion,
    pub lot_rule: Option<RuleId>,
    pub fx_source: FxSource,
    pub day_count: DayCount,
    /// Версия правила построения будущего потока облигации.
    pub cashflow_projection: CashflowProjectionVersion,
    /// Версия политики учёта расходов.
    pub expense_policy: zero_reinvestment::ExpensePolicyVersion,
    pub solver_policy: SolverPolicy,
    /// Порог, по которому классифицирован отрицательный остаток (§11).
    /// Цифра, зависящая от порога, обязана нести порог рядом с собой.
    pub perimeter_policy: PerimeterPolicy,
    /// Версия единого правила пересчёта котировки в деньги.
    pub quotation_rule: QuotationRuleVersion,
    /// Версия правила расчёта НКД.
    pub accrued_interest_rule: AccruedInterestRuleVersion,
    /// Версия правила сверки запланированных выплат с журналом.
    pub posting_match: PostingMatchVersion,
}

/// Координата знания, зафиксированная отчётом (§4).
///
/// Это тройка версий и момента знания, а не перечень идентификаторов
/// наблюдений: append-only журнал и детерминированный выбор восстанавливают
/// набор входов по одной и той же координате.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeCoordinate {
    pub knowledge_as_of: OffsetDateTime,
    pub source_priority_version: u32,
    pub valuation_policy_version: u32,
}

impl Default for KnowledgeCoordinate {
    fn default() -> Self {
        Self {
            knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
            source_priority_version: 1,
            valuation_policy_version: 1,
        }
    }
}

/// Запрос отчёта.
#[derive(Debug, Clone, Copy)]
pub struct ReturnsRequest<'a> {
    pub contour: &'a ContourDefinition,
    pub as_of: Date,
    pub report_currency: CurrencyCode,
    pub fx: &'a FxTable,
    pub solver_policy: SolverPolicy,
    /// Координата набора наблюдений, использованного при расчёте.
    pub coordinate: KnowledgeCoordinate,
    /// Реестр сверки: без него доля подтверждённого неизвестна (§10.5).
    pub ledger: &'a ReconciliationLedger,
    /// Оценка периметра: без неё отчёт не знает, где отказаться
    /// считать (§11).
    pub perimeter: &'a PerimeterAssessment,
    /// Кандидаты из рыночного хранилища.
    ///
    /// Приходят отдельным входом, а не событиями журнала, — тем же путём,
    /// каким уже приходят официальные курсы (E3.3, дизайн 2.1). Пустой
    /// срез означает «биржевых наблюдений нет», а не ошибку: решение о
    /// покрытии принимает политика.
    pub market_prices: &'a [PriceCandidate],
    /// График выплат на координату знания, по инструменту.
    pub bond_schedules: &'a BTreeMap<InstrumentId, BondSchedule>,
    /// Наблюдённый НКД на одну бумагу, с привязкой к площадке и дате сделки.
    pub accrued_observations: &'a BTreeMap<(InstrumentId, Venue, Date), PerUnitAmount>,
}

/// Ответ на три вопроса этапа 1.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnsReport {
    pub as_of: Date,
    pub history_starts: Option<Date>,
    pub report_currency: CurrencyCode,
    /// Координата, по которой выбран набор входов отчёта.
    pub coordinate: KnowledgeCoordinate,
    /// SHA-256 канонической выборки входов.
    pub inputs_hash: String,
    /// Внесено в контур за всю историю.
    pub contributed: Computed<Dec>,
    /// Выведено из контура за всю историю.
    pub withdrawn: Computed<Dec>,
    /// Стоимость контура на дату отчёта: деньги плюс позиции по цене.
    pub terminal_value: Computed<Dec>,
    /// Метрики облигационных позиций по каждому доступному сценарию.
    pub bond_metrics: Vec<BondPositionMetrics>,
    /// Стоимость до издержек выхода и до налога.
    pub liquidation_value_before_exit_costs_and_tax: LiquidationEstimate,
    /// Внутренняя норма доходности **до налога**.
    pub xirr: Computed<RateOutcome>,
    pub applied_rules: AppliedRules,
    /// Атрибуты облигационных позиций (§4 спеки E3.4.4).
    pub bond_attributes: Vec<BondPositionAttributes>,
    pub data_quality: DataQuality,
}

impl ReturnsReport {
    /// Ярлык результата. Существует, чтобы никакой потребитель API
    /// не назвал эту величину «доходностью» без оговорки (§16.3).
    pub const XIRR_LABEL: &'static str = "xirr_pre_tax";
    /// Оценка без издержек гипотетического выхода и без налога (§6.2).
    pub const LIQUIDATION_LABEL: &'static str = "liquidation_value_before_exit_costs_and_tax";
}

#[derive(Serialize)]
struct SelectedPosition {
    account: AccountId,
    custody: Option<crate::ids::CustodyId>,
    instrument: InstrumentId,
    quantity: crate::money::Quantity,
    valuation: PositionValuation,
}

#[derive(Serialize)]
enum PositionValuation {
    /// Наблюдение в боксе: с основанием котировки вариант перевесил
    /// остальные вчетверо, и перечисление стало занимать размер самого
    /// большого на каждой позиции (clippy::large_enum_variant). Тот же
    /// приём уже применён к `PositionAssessmentKind::Selected`.
    Selected(Box<SelectedObservation>),
    LegacyDerived {
        quality: PriceQuality,
        price: Option<crate::valuation::InstrumentPrice>,
    },
    Uncovered {
        reason: &'static str,
    },
}

#[derive(Serialize)]
struct SelectedObservation {
    instrument: InstrumentId,
    price: Dec,
    currency: CurrencyCode,
    #[serde(default)]
    quotation_basis: &'static str,
    #[serde(default)]
    basis_evidence: String,
    trade_date: Date,
    observed_at: Option<OffsetDateTime>,
    executability: &'static str,
    selection: SelectedSelection,
    freshness: SelectedFreshness,
    provenance: SelectedProvenance,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SelectedSelection {
    AsObserved,
    CarriedForward { observed_on: Date, days: u16 },
    LegacyDerived { quality: PriceQuality },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SelectedFreshness {
    Fresh,
    Stale { days: u16 },
}

#[derive(Serialize)]
struct SelectedProvenance {
    price_kind: Option<String>,
    origin: SelectedOrigin,
    venue: Option<String>,
    observed_at: Option<OffsetDateTime>,
    valuation_policy_version: u32,
    source_priority_version: u32,
    carry_forward_limit: u16,
    price_max_age: u16,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SelectedOrigin {
    Market { venue: String, price_kind: String },
    ReportParsed { source: SourceId },
    OwnerAsserted,
}

fn selected_observation(price: &SelectedPrice) -> SelectedObservation {
    let selection = match price.selection {
        crate::valuation::PriceSelection::AsObserved => SelectedSelection::AsObserved,
        crate::valuation::PriceSelection::CarriedForward { observed_on, days } => {
            SelectedSelection::CarriedForward { observed_on, days }
        }
        crate::valuation::PriceSelection::LegacyDerived { quality } => {
            SelectedSelection::LegacyDerived { quality }
        }
    };
    let freshness = match price.freshness {
        crate::valuation::PriceFreshness::Fresh => SelectedFreshness::Fresh,
        crate::valuation::PriceFreshness::Stale { days } => SelectedFreshness::Stale { days },
    };
    let origin = match &price.provenance.origin {
        crate::valuation::PriceOrigin::Market { venue, kind } => SelectedOrigin::Market {
            venue: venue.board.clone(),
            price_kind: match kind {
                crate::valuation::PriceKind::Close => "close",
                crate::valuation::PriceKind::LegalClose => "legal_close",
                crate::valuation::PriceKind::WeightedAverage => "weighted_average",
                crate::valuation::PriceKind::MarketPrice2 => "market_price_2",
                crate::valuation::PriceKind::MarketPrice3 => "market_price_3",
                crate::valuation::PriceKind::AdmittedQuote => "admitted_quote",
            }
            .to_owned(),
        },
        crate::valuation::PriceOrigin::ReportParsed { source } => {
            SelectedOrigin::ReportParsed { source: *source }
        }
        crate::valuation::PriceOrigin::OwnerAsserted => SelectedOrigin::OwnerAsserted,
    };
    SelectedObservation {
        quotation_basis: price.candidate.basis.code(),
        basis_evidence: price.candidate.basis_evidence.clone(),
        instrument: price.candidate.instrument,
        price: price.candidate.price,
        currency: price.candidate.currency,
        trade_date: price.candidate.trade_date,
        observed_at: price.candidate.observed_at,
        executability: match price.candidate.executability {
            SourceExecutability::Executable => "executable",
            SourceExecutability::IndicativePreviousClose => "indicative_previous_close",
            SourceExecutability::Unknown => "unknown",
        },
        selection,
        freshness,
        provenance: SelectedProvenance {
            price_kind: price.provenance.price_kind.clone(),
            origin,
            venue: price.provenance.venue.clone(),
            observed_at: price.provenance.observed_at,
            valuation_policy_version: price.provenance.valuation_policy_version,
            source_priority_version: price.provenance.source_priority_version,
            carry_forward_limit: price.provenance.carry_forward_limit,
            price_max_age: price.provenance.price_max_age,
        },
    }
}

fn uncovered_reason_code(reason: &UncoveredReason) -> &'static str {
    match reason {
        UncoveredReason::NoObservation => "no_observation",
        UncoveredReason::TooOld => "too_old",
        UncoveredReason::AmbiguousVenue => "ambiguous_venue",
        UncoveredReason::AmbiguousCandidate => "ambiguous_candidate",
        UncoveredReason::NotComputable { reason } => reason.code(),
    }
}

fn policy_uncovered_reason(reason: PolicyUncoveredReason) -> UncoveredReason {
    match reason {
        PolicyUncoveredReason::NoObservation => UncoveredReason::NoObservation,
        PolicyUncoveredReason::TooOld => UncoveredReason::TooOld,
        PolicyUncoveredReason::AmbiguousVenue => UncoveredReason::AmbiguousVenue,
        PolicyUncoveredReason::AmbiguousCandidate => UncoveredReason::AmbiguousCandidate,
    }
}

#[derive(Serialize)]
struct SelectedFx<'a> {
    source: &'a FxSource,
    rates: Vec<(CurrencyCode, CurrencyCode, Date, Option<Dec>)>,
}

#[derive(Serialize)]
struct SelectedCoordinate {
    knowledge_as_of: OffsetDateTime,
    source_priority_version: u32,
    valuation_policy_version: u32,
}

#[derive(Serialize)]
struct SelectedInputs<'a> {
    coordinate: SelectedCoordinate,
    as_of: Date,
    contour: &'a ContourDefinition,
    report_currency: CurrencyCode,
    flows: Vec<crate::projection::flows::ExternalFlow>,
    cash: Vec<(AccountId, crate::money::Money)>,
    positions: Vec<SelectedPosition>,
    fx: SelectedFx<'a>,
    bond_schedules: &'a BTreeMap<InstrumentId, BondSchedule>,
    accrued_observations: &'a BTreeMap<(InstrumentId, Venue, Date), PerUnitAmount>,
    accrued_interest_rule: AccruedInterestRuleVersion,
    offer_book: &'a OfferBook,
    cashflow_projection: CashflowProjectionVersion,
    expense_policy: u32,
    /// Версия правила сверки: изменение входа отчёта обязано менять
    /// отпечаток, иначе два несравнимых ответа выглядели бы
    /// воспроизведением одного.
    posting_match: PostingMatchVersion,
}

#[cfg(test)]
fn inputs_hash_with_assessments(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
    assessments: Vec<PositionAssessment>,
) -> String {
    let offer_book = OfferBook::default();
    inputs_hash_with_bond_inputs(state, request, assessments, &offer_book)
}

fn inputs_hash_with_bond_inputs(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
    assessments: Vec<PositionAssessment>,
    offer_book: &OfferBook,
) -> String {
    let mut flows: Vec<_> = state
        .flows()
        .external()
        .iter()
        .filter(|flow| {
            flow.date <= request.as_of
                && flow.contour == request.contour.id()
                && flow.version == request.contour.version()
        })
        .copied()
        .collect();
    flows.sort_by_key(|flow| (flow.date, flow.event));

    let mut cash: Vec<_> = state
        .balances()
        .iter_cash()
        .filter(|(account, _)| request.contour.contains(*account))
        .collect();
    cash.sort_by_key(|(account, money)| (*account, money.currency()));

    let positions: Vec<_> = assessments
        .into_iter()
        .map(|assessment| {
            let PositionAssessment {
                account,
                custody,
                instrument,
                quantity,
                raw_price,
                kind,
                remaining_face: _,
            } = assessment;
            let valuation = match kind {
                PositionAssessmentKind::Selected(selected) => {
                    PositionValuation::Selected(Box::new(selected_observation(&selected)))
                }
                PositionAssessmentKind::LegacyDerived(quality) => {
                    PositionValuation::LegacyDerived {
                        quality,
                        price: raw_price,
                    }
                }
                PositionAssessmentKind::Uncovered(reason) => PositionValuation::Uncovered {
                    reason: uncovered_reason_code(&reason),
                },
            };
            SelectedPosition {
                account,
                custody,
                instrument,
                quantity,
                valuation,
            }
        })
        .collect();

    let mut fx_keys = std::collections::BTreeSet::new();
    for flow in &flows {
        fx_keys.insert((flow.amount.currency(), flow.date));
    }
    for (_, money) in &cash {
        fx_keys.insert((money.currency(), request.as_of));
    }
    for position in &positions {
        let currency = match &position.valuation {
            PositionValuation::Selected(observation) => Some(observation.currency),
            PositionValuation::LegacyDerived { price, .. } => price.map(|price| price.currency),
            PositionValuation::Uncovered { .. } => None,
        };
        if let Some(currency) = currency {
            fx_keys.insert((currency, request.as_of));
        }
    }
    let rates = fx_keys
        .into_iter()
        .map(|(from, date)| {
            (
                from,
                request.report_currency,
                date,
                request.fx.rate(from, request.report_currency, date),
            )
        })
        .collect();

    let selected = SelectedInputs {
        coordinate: SelectedCoordinate {
            knowledge_as_of: request.coordinate.knowledge_as_of.to_offset(UtcOffset::UTC),
            source_priority_version: request.coordinate.source_priority_version,
            valuation_policy_version: request.coordinate.valuation_policy_version,
        },
        as_of: request.as_of,
        contour: request.contour,
        report_currency: request.report_currency,
        flows,
        cash,
        positions,
        fx: SelectedFx {
            source: request.fx.source(),
            rates,
        },
        bond_schedules: request.bond_schedules,
        accrued_observations: request.accrued_observations,
        accrued_interest_rule: accrued_interest_rule().0,
        offer_book,
        cashflow_projection: cashflow_projection_rule().0,
        expense_policy: expense_policy_rule().0,
        posting_match: posting_match_rule().0,
    };
    hash_selected(&selected)
}

/// Отпечаток отобранных входов.
///
/// Вынесен из [`inputs_hash_with_bond_inputs`], чтобы тест мог менять
/// одно поле входа и проверять, что отпечаток на это отзывается:
/// собрать `SelectedInputs` руками дешевле, чем воспроизводить целое
/// состояние ради одной версии правила.
fn hash_selected(selected: &SelectedInputs<'_>) -> String {
    let mut encoded = Vec::new();
    if ciborium::into_writer(selected, &mut encoded).is_err() {
        encoded.extend_from_slice(b"serialization_error");
    }

    let mut hasher = Sha256::new();
    hasher.update(b"iaam/returns-inputs/v2");
    hasher.update(encoded);
    let digest = hasher.finalize();
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        result.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        result.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    result
}
/// Правило пересчёта котировки, применяемое отчётом.
///
/// Один помощник нужен, чтобы оценка позиции и XIRR не получили
/// расходящиеся реализации пересчёта.
pub(crate) const fn quotation_rule() -> (QuotationRuleVersion, QuotationV1) {
    (QuotationRuleVersion(1), QuotationV1)
}

/// Версия правила построения сценарных потоков.
pub(crate) const fn cashflow_projection_rule() -> (
    CashflowProjectionVersion,
    crate::rules::CashflowProjectionV2,
) {
    (
        CashflowProjectionVersion(2),
        crate::rules::CashflowProjectionV2,
    )
}

/// Правило сверки запланированных выплат с датированными фактами.
pub(crate) const fn posting_match_rule() -> (PostingMatchVersion, PostingMatchV2) {
    (PostingMatchVersion(2), PostingMatchV2::new())
}

/// Версия политики расходов, применяемой до появления налогового контура.
pub(crate) const fn expense_policy_rule() -> zero_reinvestment::ExpensePolicyVersion {
    zero_reinvestment::ExpensePolicyVersion(1)
}

/// Правило расчёта НКД, применяемое отчётом.
pub(crate) const fn accrued_interest_rule() -> (AccruedInterestRuleVersion, AccruedInterestV1) {
    (AccruedInterestRuleVersion(1), AccruedInterestV1)
}

fn accrued_error(error: AccruedInterestError, instrument: InstrumentId) -> NotComputable {
    match error {
        AccruedInterestError::OutsideCoverage => {
            NotComputable::OutsideScheduleCoverage { instrument }
        }
        AccruedInterestError::OverlappingCoverage => {
            NotComputable::OverlappingScheduleCoverage { instrument }
        }
        AccruedInterestError::CouponUndetermined => {
            NotComputable::CouponUndetermined { instrument }
        }
        AccruedInterestError::Numeric(_) => NotComputable::Numeric { code: "numeric" },
    }
}

fn accrued_per_unit(
    request: &ReturnsRequest<'_>,
    rule: &dyn AccruedInterestRule,
    instrument: InstrumentId,
) -> Result<PerUnitAmount, NotComputable> {
    let schedule = request
        .bond_schedules
        .get(&instrument)
        .ok_or(NotComputable::ScheduleMissing { instrument })?;
    rule.accrued_per_unit(&schedule.periods, request.as_of)
        .map_err(|error| accrued_error(error, instrument))
}

fn position_amount(
    per_unit: PerUnitAmount,
    quantity: crate::money::Quantity,
    request: &ReturnsRequest<'_>,
) -> Computed<Dec> {
    let local = match per_unit.checked_mul_quantity(quantity) {
        Ok(value) => value,
        Err(_) => {
            return Computed::NotComputable {
                reason: NotComputable::Numeric { code: "numeric" },
            };
        }
    };
    let Some(rate) = request
        .fx
        .rate(per_unit.currency(), request.report_currency, request.as_of)
    else {
        return Computed::NotComputable {
            reason: NotComputable::MissingFxRate {
                from: per_unit.currency(),
                to: request.report_currency,
                date: request.as_of,
            },
        };
    };
    match local.checked_mul(rate) {
        Ok(value) => Computed::Value(value),
        Err(_) => Computed::NotComputable {
            reason: NotComputable::Numeric { code: "numeric" },
        },
    }
}

fn selected_executability(assessment: &PositionAssessment) -> Option<SourceExecutability> {
    match &assessment.kind {
        PositionAssessmentKind::Selected(selected) => Some(selected.candidate.executability),
        PositionAssessmentKind::LegacyDerived(_) | PositionAssessmentKind::Uncovered(_) => None,
    }
}
fn selected_accrued_observation<'a>(
    assessment: &PositionAssessment,
    request: &'a ReturnsRequest<'_>,
) -> Option<&'a PerUnitAmount> {
    let PositionAssessmentKind::Selected(selected) = &assessment.kind else {
        return None;
    };
    if selected.candidate.trade_date != request.as_of {
        return None;
    }
    let crate::valuation::PriceOrigin::Market { venue, .. } = &selected.candidate.origin else {
        return None;
    };
    request
        .accrued_observations
        .get(&(assessment.instrument, venue.clone(), request.as_of))
}

fn bond_position_attributes(
    positions: &[PositionValue],
    request: &ReturnsRequest<'_>,
    rule: &dyn AccruedInterestRule,
) -> Vec<BondPositionAttributes> {
    positions
        .iter()
        .filter(|position| {
            matches!(
                &position.assessment.kind,
                PositionAssessmentKind::Selected(selected)
                    if selected.candidate.basis == QuotationBasis::PercentOfRemainingFace
            )
        })
        .map(|position| {
            let assessment = &position.assessment;
            let accrued = match accrued_per_unit(request, rule, assessment.instrument) {
                Ok(per_unit) => position_amount(per_unit, assessment.quantity, request),
                Err(reason) => Computed::NotComputable { reason },
            };
            let payable_observation = selected_accrued_observation(assessment, request)
                .map(|per_unit| position_amount(*per_unit, assessment.quantity, request))
                .unwrap_or_else(|| Computed::NotComputable {
                    reason: NotComputable::AccruedObservationMissing {
                        instrument: assessment.instrument,
                    },
                });
            let payable = payable_on_termination(
                &payable_observation,
                selected_executability(assessment).unwrap_or(SourceExecutability::Unknown),
            );
            let (next_posting_date, next_principal_return_finality) = request
                .bond_schedules
                .get(&assessment.instrument)
                .map(|schedule| {
                    let next = next_posting_date(
                        &schedule.periods,
                        &schedule.principal_returns,
                        &[],
                        request.as_of,
                    );
                    let finality = next.and_then(|date| {
                        finality_of(&schedule.principal_returns)
                            .ok()?
                            .into_iter()
                            .find(|(item, _)| item.repayment_date == date)
                            .map(|(_, finality)| finality)
                    });
                    (next, finality)
                })
                .unwrap_or((None, None));
            BondPositionAttributes {
                account: assessment.account,
                custody: assessment.custody,
                instrument: assessment.instrument,
                accrued_interest: accrued,
                accrued_interest_payable_on_termination: payable,
                next_posting_date,
                next_principal_return_finality,
            }
        })
        .collect()
}

/// Материально ли расхождение расчёта НКД с наблюдением.
///
/// Допуск — одна минорная единица валюты: расхождение в копейку
/// объясняется округлением, а не ошибкой правила.
fn accrued_mismatch_is_material(computed: Dec, observed: Dec, currency: CurrencyCode) -> bool {
    let Ok(difference) = computed.checked_sub(observed) else {
        return true;
    };
    let tolerance = Dec::new(Decimal::new(1, currency.minor_units()));
    difference.inner().abs() > tolerance.inner()
}

fn accrued_mismatch_issues(
    positions: &[PositionValue],
    request: &ReturnsRequest,
    rule: &dyn AccruedInterestRule,
) -> Vec<MaterialIssue> {
    let mut quantities = BTreeMap::new();
    for position in positions {
        let assessment = &position.assessment;
        let total = quantities
            .entry(assessment.instrument)
            .or_insert_with(Dec::zero);
        if let Ok(sum) = total.checked_add(assessment.quantity.0) {
            *total = sum;
        }
    }
    quantities
        .into_iter()
        .filter_map(|(instrument, quantity)| {
            let computed = accrued_per_unit(request, rule, instrument).ok()?;
            let observed = positions
                .iter()
                .find(|position| position.assessment.instrument == instrument)
                .and_then(|position| selected_accrued_observation(&position.assessment, request))?;
            let material = computed.currency() != observed.currency()
                || accrued_mismatch_is_material(
                    computed.value(),
                    observed.value(),
                    observed.currency(),
                );
            if !material {
                return None;
            }
            let quantity = crate::money::Quantity(quantity);
            Some(MaterialIssue::AccruedInterestMismatch {
                instrument,
                computed: computed.checked_mul_quantity(quantity).ok()?,
                computed_currency: computed.currency(),
                observed: observed.checked_mul_quantity(quantity).ok()?,
                observed_currency: observed.currency(),
                quantity,
                date: request.as_of,
            })
        })
        .collect()
}

/// Реализуемая при выходе сумма (§4.2).
///
/// НКД становится реализуемым только при цене, которую источник объявил
/// исполнимой. Индикативное закрытие и отсутствие выбранной цены — отказ,
/// а не нулевой результат и не гарантия ликвидности.
fn payable_on_termination(
    accrued: &Computed<Dec>,
    executability: SourceExecutability,
) -> Computed<Dec> {
    match executability {
        SourceExecutability::Executable => accrued.clone(),
        SourceExecutability::IndicativePreviousClose | SourceExecutability::Unknown => {
            Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable,
            }
        }
    }
}

fn aggregate_payable_on_termination(attributes: &[BondPositionAttributes]) -> Computed<Dec> {
    let mut total = Dec::zero();
    for attribute in attributes {
        let Computed::Value(value) = &attribute.accrued_interest_payable_on_termination else {
            return attribute.accrued_interest_payable_on_termination.clone();
        };
        total = match total.checked_add(*value) {
            Ok(value) => value,
            Err(_) => {
                return Computed::NotComputable {
                    reason: NotComputable::Numeric { code: "numeric" },
                };
            }
        };
    }
    Computed::Value(total)
}

/// Расчёт отчёта без отдельной книги заявок.
#[must_use]
pub fn returns_report(state: &LedgerState, request: &ReturnsRequest) -> ReturnsReport {
    let offer_book = OfferBook::default();
    returns_report_with_bond_inputs(state, request, &offer_book)
}

/// Расчёт отчёта с входами, которые относятся только к облигационным
/// сценариям. Книга заявок строится оболочкой приложения из журнала.
///
/// Ядро не ходит за данными: цены и курсы приходят готовыми, границы
/// контура заданы явно. Всё, чего не хватает, превращается в отказ
/// с указанием причины, а не в подставленное значение.
#[must_use]
pub fn returns_report_with_bond_inputs(
    state: &LedgerState,
    request: &ReturnsRequest,
    offer_book: &OfferBook,
) -> ReturnsReport {
    let (quotation_rule_version, _) = quotation_rule();
    let (accrued_interest_rule_version, accrued_interest_rule) = accrued_interest_rule();
    let (cashflow_projection_version, _) = cashflow_projection_rule();
    let (posting_match_version, _) = posting_match_rule();
    let expense_policy_version = expense_policy_rule();
    let positions = position_values(state, request);
    let series = xirr::flow_series(state, request);
    let terminal = xirr::terminal_value_from_position_values(state, request, &positions);
    let (contributed, withdrawn) = match &series {
        Ok(series) => (
            Computed::Value(series.contributed),
            Computed::Value(series.withdrawn),
        ),
        Err(reason) => (
            Computed::NotComputable {
                reason: reason.clone(),
            },
            Computed::NotComputable {
                reason: reason.clone(),
            },
        ),
    };
    let terminal_value = match &terminal {
        Ok(value) => Computed::Value(*value),
        Err(reason) => Computed::NotComputable {
            reason: reason.clone(),
        },
    };
    let rate = xirr::rate(&series, &terminal, request);
    let bond_attributes = bond_position_attributes(&positions, request, &accrued_interest_rule);
    let mut data_quality = data_quality(state, request, &positions);
    let bond_metrics = bond_position_metrics(state, request, &positions, offer_book);
    for issue in unresolved_offer_issues(request, offer_book) {
        data_quality.material_issues.push(issue);
    }
    data_quality
        .material_issues
        .extend(historical_reconciliation_issues(state, request));
    if data_quality
        .material_issues
        .iter()
        .any(MaterialIssue::is_defect)
    {
        data_quality.status = DataQualityStatus::Incomplete;
    }
    let liquidation_value_before_exit_costs_and_tax = LiquidationEstimate {
        value_before_exit_costs_and_tax: terminal_value.clone(),
        executability: data_quality.executability,
        exit_costs: AmountQualification::Unknown,
        tax: AmountQualification::Unknown,
        accrued_interest_payable_on_termination: aggregate_payable_on_termination(&bond_attributes),
    };

    ReturnsReport {
        as_of: request.as_of,
        history_starts: state.coverage().first_event(),
        report_currency: request.report_currency,
        coordinate: request.coordinate,
        inputs_hash: inputs_hash_with_bond_inputs(
            state,
            request,
            position_assessments(state, request),
            offer_book,
        ),
        contributed,
        withdrawn,
        terminal_value,
        bond_metrics,
        liquidation_value_before_exit_costs_and_tax,
        xirr: rate,
        applied_rules: AppliedRules {
            contour: request.contour.id(),
            contour_version: request.contour.version(),
            lot_rule: state.book().applied_rule().cloned(),
            fx_source: request.fx.source().clone(),
            day_count: DayCount::Act365,
            solver_policy: request.solver_policy,
            perimeter_policy: request.perimeter.policy(),
            quotation_rule: quotation_rule_version,
            accrued_interest_rule: accrued_interest_rule_version,
            cashflow_projection: cashflow_projection_version,
            expense_policy: expense_policy_version,
            posting_match: posting_match_version,
        },
        bond_attributes,
        data_quality,
    }
}

fn cashflow_reason(error: crate::rules::CashflowError, instrument: InstrumentId) -> NotComputable {
    match error {
        crate::rules::CashflowError::CouponUndetermined { .. } => {
            NotComputable::CouponUndetermined { instrument }
        }
        crate::rules::CashflowError::PrincipalUnknown => NotComputable::PrincipalUnknown,
        crate::rules::CashflowError::ScheduleIncomplete { .. }
        | crate::rules::CashflowError::ScheduleCompletenessUnknown
        | crate::rules::CashflowError::IssueTermsUnknown => {
            NotComputable::ScheduleMissing { instrument }
        }
        crate::rules::CashflowError::IssuerDefaultDeclared
        | crate::rules::CashflowError::IssuerTechnicalDefault
        | crate::rules::CashflowError::SharesDoNotSumToWhole { .. }
        | crate::rules::CashflowError::CurrencyFormulaUnknown { .. }
        | crate::rules::CashflowError::OfferWindowNotExercisable { .. }
        | crate::rules::CashflowError::Numeric(_) => NotComputable::Numeric { code: "cashflow" },
    }
}

fn bond_c0(
    assessment: &PositionAssessment,
    request: &ReturnsRequest<'_>,
    accrued_rule: &dyn AccruedInterestRule,
) -> Computed<CalcMoney> {
    let accrued =
        match accrued_per_unit(request, accrued_rule, assessment.instrument).and_then(|value| {
            value
                .checked_mul_quantity(assessment.quantity)
                .map(|total| CalcMoney::new(total, value.currency()))
                .map_err(|_| NotComputable::Numeric {
                    code: "accrued_total",
                })
        }) {
            Ok(value) => value,
            Err(reason) => return Computed::NotComputable { reason },
        };
    let selected = match &assessment.kind {
        PositionAssessmentKind::Selected(selected) => selected,
        PositionAssessmentKind::LegacyDerived(_)
        | PositionAssessmentKind::Uncovered(UncoveredReason::NoObservation)
        | PositionAssessmentKind::Uncovered(UncoveredReason::TooOld)
        | PositionAssessmentKind::Uncovered(UncoveredReason::AmbiguousVenue)
        | PositionAssessmentKind::Uncovered(UncoveredReason::AmbiguousCandidate) => {
            return Computed::NotComputable {
                reason: NotComputable::MissingPrice {
                    instrument: assessment.instrument,
                },
            };
        }
        PositionAssessmentKind::Uncovered(UncoveredReason::NotComputable { reason }) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    let remaining_face = match &assessment.remaining_face {
        Ok(value) => *value,
        Err(reason) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    zero_reinvestment::prospective_c0(
        assessment.quantity,
        selected.candidate.basis,
        selected.candidate.price,
        selected.candidate.currency,
        remaining_face,
        accrued,
    )
}

fn unavailable_prospective(
    as_of: Date,
    terminal_date: Date,
    c0: Computed<CalcMoney>,
    choice: OfferChoice,
    reason: NotComputable,
) -> zero_reinvestment::ProspectiveMetric {
    let irr_label = match choice {
        OfferChoice::HoldToMaturity => zero_reinvestment::IrrLabel::YieldToMaturity,
        OfferChoice::ExerciseAtOffer { .. } => zero_reinvestment::IrrLabel::YieldToOffer,
    };
    zero_reinvestment::ProspectiveMetric {
        as_of,
        terminal_date,
        c0,
        metrics: Computed::NotComputable {
            reason: reason.clone(),
        },
        irr: Computed::NotComputable { reason },
        irr_label,
    }
}

/// Входы одной облигационной позиции, общие для всех её сценариев.
///
/// Структура, а не семь аргументов: набор един на позицию, а меняется
/// между вызовами только сценарий, и перепутать местами две ссылки
/// одного типа в списке из семи слишком легко.
struct BondScenarioInputs<'a> {
    assessment: &'a PositionAssessment,
    request: &'a ReturnsRequest<'a>,
    schedule: &'a BondSchedule,
    lots: Option<&'a crate::projection::lots::InstrumentLots>,
    cashflow: &'a dyn crate::rules::CashflowProjection,
    accrued_rule: &'a dyn AccruedInterestRule,
}

/// Поток одного сценария. Вынесен из [`bond_scenario`], потому что
/// сверка прошлого строит его отдельно и обязана строить тем же
/// правилом: иначе `past` у сверки и у сценария разошлись бы.
fn scenario_plan(
    inputs: &BondScenarioInputs<'_>,
    choice: &OfferChoice,
) -> Result<CashflowPlan, NotComputable> {
    let BondScenarioInputs {
        assessment,
        request,
        schedule,
        lots: _,
        cashflow,
        accrued_rule: _,
    } = *inputs;
    cashflow
        .future_postings(&CashflowInput {
            schedule,
            quantity: assessment.quantity,
            choice,
            as_of: request.as_of,
            report_currency: request.report_currency,
        })
        .map_err(|error| cashflow_reason(error, assessment.instrument))
}

fn bond_scenario(
    inputs: &BondScenarioInputs<'_>,
    choice: OfferChoice,
) -> zero_reinvestment::BondScenarioResult {
    let BondScenarioInputs {
        assessment,
        request,
        schedule,
        lots,
        cashflow: _,
        accrued_rule,
    } = *inputs;
    let c0 = bond_c0(assessment, request, accrued_rule);
    let plan = scenario_plan(inputs, &choice);
    match plan {
        Ok(plan) => {
            let prospective =
                zero_reinvestment::prospective_metric(request.as_of, &plan, c0, &choice);
            let lifetime = lots.map_or_else(
                || Computed::NotComputable {
                    reason: NotComputable::CohortGap {
                        gap: crate::projection::lots::CohortGap::AcquisitionDateUnknown,
                    },
                },
                |lots| zero_reinvestment::lifetime_metrics_from_lots(lots, &plan),
            );
            zero_reinvestment::BondScenarioResult {
                choice,
                prospective,
                lifetime,
            }
        }
        Err(reason) => {
            let terminal_date = match choice {
                OfferChoice::HoldToMaturity => schedule
                    .principal_returns
                    .iter()
                    .map(|item| item.repayment_date)
                    .max()
                    .unwrap_or(request.as_of),
                OfferChoice::ExerciseAtOffer { window } => schedule
                    .offer_windows
                    .iter()
                    .find(|terms| terms.window == window)
                    .map_or(request.as_of, |terms| terms.execution_date),
            };
            zero_reinvestment::BondScenarioResult {
                choice: choice.clone(),
                prospective: unavailable_prospective(
                    request.as_of,
                    terminal_date,
                    c0,
                    choice,
                    reason.clone(),
                ),
                lifetime: Computed::NotComputable { reason },
            }
        }
    }
}

/// Сворачивает повторяющиеся недоказуемости после завершения всей сверки.
///
/// Источник проблемы чинится одним действием, поэтому повтор одной причины
/// не должен занимать в отчёте место каждой отдельной выплаты. При этом
/// остальные проблемы проходят без фильтрации: обвинение в пропуске остаётся
/// адресным, а разные причины не смешиваются.
fn collapse_scheduled_posting_unverifiable(issues: Vec<MaterialIssue>) -> Vec<MaterialIssue> {
    type Key = (AccountId, InstrumentId, PostingKind, UnverifiableReason);

    let mut groups: BTreeMap<Key, (u32, Date, Date)> = BTreeMap::new();
    for issue in &issues {
        let MaterialIssue::ScheduledPostingUnverifiable {
            account,
            instrument,
            date,
            kind,
            reason,
        } = issue
        else {
            continue;
        };
        // Профиль источника намеренно не входит в ключ: книга лотов не
        // отслеживает происхождение событий, а количество и период уже
        // дают владельцу нужное действие для исправления.
        let key = (*account, *instrument, *kind, *reason);
        groups
            .entry(key)
            .and_modify(|(count, first_date, last_date)| {
                *count = count.checked_add(1).expect("слишком много выплат");
                *first_date = (*first_date).min(*date);
                *last_date = (*last_date).max(*date);
            })
            .or_insert((1, *date, *date));
    }

    let mut emitted = BTreeSet::new();
    let mut collapsed = Vec::with_capacity(issues.len());
    for issue in issues {
        let MaterialIssue::ScheduledPostingUnverifiable {
            account,
            instrument,
            date,
            kind,
            reason,
        } = issue
        else {
            collapsed.push(issue);
            continue;
        };
        let key = (account, instrument, kind, reason);
        if !emitted.insert(key) {
            continue;
        }
        let (count, first_date, last_date) =
            groups.remove(&key).expect("группа недоказуемости потеряна");
        if count == 1 {
            collapsed.push(MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date,
                kind,
                reason,
            });
        } else {
            collapsed.push(MaterialIssue::ScheduledPostingsUnverifiable {
                account,
                instrument,
                kind,
                reason,
                count,
                first_date,
                last_date,
            });
        }
    }
    collapsed
}

/// Сверка прошлого по книге лотов, независимо от текущих позиций.
///
/// Полностью проданная бумага остаётся в книге лотов, но исчезает из
/// позиций вместе с местом хранения. Поэтому проход обязан идти по
/// [`LotKey`]: здесь собираются только проблемы, а метрики по-прежнему
/// считает отдельный проход по позициям.
fn historical_reconciliation_issues(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
) -> Vec<MaterialIssue> {
    let (_, posting_match) = posting_match_rule();
    let mut issues = Vec::new();

    for (key, lots) in state.book().iter() {
        if !request.contour.contains(key.account) {
            continue;
        }
        let Some(schedule) = request.bond_schedules.get(&key.instrument) else {
            continue;
        };
        let postings = match historical_schedule_postings(schedule, request.as_of) {
            Ok(postings) => postings,
            Err(_) => {
                // Ошибка доверия относится ко всей паре: даты и вида
                // выплаты взять неоткуда, но форма проблемы обязана быть
                // заполнена, поэтому здесь честно используем дату отчёта
                // и нейтральный вид Coupon.
                issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                    account: key.account,
                    instrument: key.instrument,
                    date: request.as_of,
                    kind: PostingKind::Coupon,
                    reason: UnverifiableReason::ScheduleNotTrusted,
                });
                continue;
            }
        };

        let gap = state.income().gap(key);
        let history_starts = state.coverage().first_event_for(key.account);
        let mut judged = Vec::new();
        for posting in postings
            .into_iter()
            .filter(|posting| posting_match.is_due(posting, request.as_of))
        {
            if let Some(gap) = gap {
                let reason = match gap {
                    crate::projection::income::IncomeGap::IncomeKindUnknown => {
                        UnverifiableReason::IncomeKindUnknown
                    }
                    crate::projection::income::IncomeGap::PaymentDateUnknown => {
                        UnverifiableReason::PaymentDateUnknown
                    }
                };
                issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                    account: key.account,
                    instrument: key.instrument,
                    date: posting.date,
                    kind: posting.kind,
                    reason,
                });
                continue;
            }
            if history_starts.is_some_and(|start| posting.date < start) {
                issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                    account: key.account,
                    instrument: key.instrument,
                    date: posting.date,
                    kind: posting.kind,
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                });
                continue;
            }
            let ownership = match posting.entitlement {
                Some(entitlement) => lots.ownership_at(entitlement),
                None => Ownership::Unknown,
            };
            judged.push((posting, ownership));
        }

        // Все выплаты, дошедшие до третьего шага, передаются одним вызовом:
        // иначе факт недоказуемой выплаты мог бы закрыть соседнюю и спрятать
        // настоящий пропуск.
        for ((posting, _), verdict) in judged
            .iter()
            .zip(posting_match.judge_all(&judged, state.income().postings(key)))
        {
            match verdict {
                Verdict::NotReceived => {
                    issues.push(MaterialIssue::ScheduledPostingNotReceived {
                        account: key.account,
                        instrument: key.instrument,
                        date: posting.date,
                        kind: posting.kind,
                    });
                }
                Verdict::Unverifiable(reason) => {
                    issues.push(MaterialIssue::ScheduledPostingUnverifiable {
                        account: key.account,
                        instrument: key.instrument,
                        date: posting.date,
                        kind: posting.kind,
                        reason,
                    });
                }
                Verdict::Silent => {}
            }
        }
    }

    collapse_scheduled_posting_unverifiable(issues)
}

fn bond_position_metrics(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
    positions: &[PositionValue],
    offer_book: &OfferBook,
) -> Vec<BondPositionMetrics> {
    let (_, cashflow) = cashflow_projection_rule();
    let (_, accrued_rule) = accrued_interest_rule();
    positions
        .iter()
        .filter_map(|position| {
            let assessment = &position.assessment;
            let schedule = request.bond_schedules.get(&assessment.instrument)?;
            if !request.contour.contains(assessment.account) || assessment.quantity.0.is_zero() {
                return None;
            }
            let key = LotKey {
                account: assessment.account,
                instrument: assessment.instrument,
            };
            let lots = state.book().entry(&key);
            let unresolved: std::collections::BTreeSet<_> =
                unresolved_submissions(offer_book, schedule)
                    .into_iter()
                    .filter(|submission| {
                        offer_book
                            .submission(*submission)
                            .is_some_and(|state| state.instrument == assessment.instrument)
                    })
                    .collect();
            let inputs = BondScenarioInputs {
                assessment,
                request,
                schedule,
                lots,
                cashflow: &cashflow,
                accrued_rule: &accrued_rule,
            };
            let scenarios = available_choices(schedule, request.as_of)
                .into_iter()
                .filter(|choice| match choice {
                    OfferChoice::HoldToMaturity => true,
                    OfferChoice::ExerciseAtOffer { .. } => unresolved.is_empty(),
                })
                .map(|choice| bond_scenario(&inputs, choice))
                .collect();
            Some(BondPositionMetrics {
                account: assessment.account,
                custody: assessment.custody,
                instrument: assessment.instrument,
                scenarios,
            })
        })
        .collect()
}

fn unresolved_offer_issues(
    request: &ReturnsRequest<'_>,
    offer_book: &OfferBook,
) -> Vec<MaterialIssue> {
    let submissions: std::collections::BTreeSet<_> = request
        .bond_schedules
        .values()
        .flat_map(|schedule| unresolved_submissions(offer_book, schedule))
        .collect();
    submissions
        .into_iter()
        .map(|submission| MaterialIssue::OfferWindowUnresolved { submission })
        .collect()
}

#[derive(Debug)]
enum PositionAssessmentKind {
    /// Кандидат в боксе: без него перечисление раздувается до размера
    /// самого большого варианта на каждой позиции (clippy::large_enum_variant).
    Selected(Box<SelectedPrice>),
    LegacyDerived(PriceQuality),
    Uncovered(UncoveredReason),
}

#[derive(Debug)]
struct PositionAssessment {
    account: AccountId,
    custody: Option<crate::ids::CustodyId>,
    instrument: InstrumentId,
    quantity: crate::money::Quantity,
    raw_price: Option<crate::valuation::InstrumentPrice>,
    kind: PositionAssessmentKind,
    remaining_face: Result<Option<PerUnitAmount>, NotComputable>,
}

fn position_assessments(
    state: &LedgerState,
    request: &ReturnsRequest<'_>,
) -> Vec<PositionAssessment> {
    let defaults = ValuationPolicyV1::default();
    let policy = ValuationPolicyV1 {
        carry_forward_limit: defaults.carry_forward_limit,
        price_max_age: defaults.price_max_age,
        source_priority_version: SourcePriorityVersion(request.coordinate.source_priority_version),
    };
    let source = SourceId(uuid::Uuid::nil());
    state
        .balances()
        .iter_positions()
        .filter(|(key, quantity)| request.contour.contains(key.account) && !quantity.0.is_zero())
        .map(|(key, quantity)| {
            let observations: Vec<_> = state
                .prices()
                .observations_at_or_before(key.instrument, request.as_of)
                .copied()
                .collect();
            let raw_price = observations.first().copied();
            let mut candidates = Vec::new();
            let mut legacy_quality = None;
            for price in &observations {
                let candidate = PriceCandidate {
                    instrument: price.instrument,
                    price: price.price,
                    currency: price.currency,
                    // §10.3: цена владельца — деньги за единицу
                    // по определению, а не по догадке. Ввод процента
                    // номинала через `EventKind::Valuation` запрещён.
                    basis: crate::valuation::QuotationBasis::MoneyPerUnit,
                    basis_evidence: "journal:valuation".to_owned(),
                    basis_evidence_contradicts: false,
                    trade_date: price.as_of,
                    observed_at: None,
                    origin: crate::valuation::PriceOrigin::ReportParsed { source },
                    executability: SourceExecutability::Unknown,
                };
                match candidate_from_legacy_valuation(price.quality, candidate) {
                    LegacyValuationOutcome::Candidate(candidate) => candidates.push(candidate),
                    LegacyValuationOutcome::LegacyDerived(quality) => {
                        legacy_quality.get_or_insert(quality);
                    }
                }
            }
            candidates.extend(
                request
                    .market_prices
                    .iter()
                    .filter(|candidate| {
                        candidate.instrument == key.instrument
                            && candidate.trade_date <= request.as_of
                    })
                    .cloned(),
            );
            let kind = if candidates.is_empty() {
                match legacy_quality {
                    Some(quality) => PositionAssessmentKind::LegacyDerived(quality),
                    None => PositionAssessmentKind::Uncovered(UncoveredReason::NoObservation),
                }
            } else {
                let result = policy.select(
                    &PriceQuery {
                        instrument: key.instrument,
                        as_of: request.as_of,
                        knowledge_as_of: request.coordinate.knowledge_as_of,
                    },
                    &candidates,
                );
                match result.selected() {
                    Some(selected) => PositionAssessmentKind::Selected(Box::new(selected.clone())),
                    None => PositionAssessmentKind::Uncovered(policy_uncovered_reason(
                        result
                            .uncovered_reason()
                            .unwrap_or(PolicyUncoveredReason::NoObservation),
                    )),
                }
            };
            PositionAssessment {
                account: key.account,
                custody: key.custody,
                instrument: key.instrument,
                quantity,
                raw_price,
                remaining_face: remaining_face(
                    state.book(),
                    LotKey {
                        account: key.account,
                        instrument: key.instrument,
                    },
                ),
                kind,
            }
        })
        .collect()
}

fn position_values(state: &LedgerState, request: &ReturnsRequest<'_>) -> Vec<PositionValue> {
    position_values_from_assessments(position_assessments(state, request), request)
}

struct PositionValue {
    assessment: PositionAssessment,
    value: Result<Dec, NotComputable>,
}

fn position_values_from_assessments(
    assessments: Vec<PositionAssessment>,
    request: &ReturnsRequest<'_>,
) -> Vec<PositionValue> {
    let (_, rule) = quotation_rule();
    assessments
        .into_iter()
        .map(|assessment| {
            let value = match &assessment.kind {
                PositionAssessmentKind::Selected(selected) => position_value(
                    &assessment,
                    PositionQuotation {
                        price: selected.candidate.price,
                        basis: selected.candidate.basis,
                        venue_currency: selected.candidate.currency,
                        remaining_face: assessment.remaining_face.clone(),
                        rule: &rule,
                    },
                    request,
                ),
                PositionAssessmentKind::LegacyDerived(_) => {
                    if let Some(price) = assessment.raw_price {
                        position_value(
                            &assessment,
                            PositionQuotation {
                                price: price.price,
                                basis: QuotationBasis::MoneyPerUnit,
                                venue_currency: price.currency,
                                remaining_face: Ok(None),
                                rule: &rule,
                            },
                            request,
                        )
                    } else {
                        Err(NotComputable::MissingPrice {
                            instrument: assessment.instrument,
                        })
                    }
                }
                PositionAssessmentKind::Uncovered(reason) => Err(match reason {
                    UncoveredReason::NotComputable { reason } => reason.clone(),
                    UncoveredReason::NoObservation
                    | UncoveredReason::TooOld
                    | UncoveredReason::AmbiguousVenue
                    | UncoveredReason::AmbiguousCandidate => NotComputable::MissingPrice {
                        instrument: assessment.instrument,
                    },
                }),
            };
            PositionValue { assessment, value }
        })
        .collect()
}

struct PositionQuotation<'a> {
    price: Dec,
    basis: QuotationBasis,
    venue_currency: CurrencyCode,
    remaining_face: Result<Option<PerUnitAmount>, NotComputable>,
    rule: &'a dyn QuotationRule,
}

/// Возвращает единый остаточный номинал лотов пары «счёт и бумага».
fn remaining_face(book: &LotBook, key: LotKey) -> Result<Option<PerUnitAmount>, NotComputable> {
    let Some(entry) = book.entry(&key) else {
        return Ok(None);
    };
    let mut found = None;
    for lot in entry.lots() {
        let Some(remaining) = lot.principal.remaining_per_unit() else {
            continue;
        };
        match found {
            None => found = Some(remaining),
            Some(previous) if previous == remaining => {}
            Some(_) => {
                return Err(NotComputable::RemainingFaceAmbiguous {
                    instrument: key.instrument,
                });
            }
        }
    }
    Ok(found)
}

fn quotation_error(error: QuotationError, instrument: InstrumentId) -> NotComputable {
    match error {
        QuotationError::BasisUnknown => NotComputable::QuotationBasisUnknown { instrument },
        QuotationError::PrincipalUnknown => NotComputable::RemainingFaceUnknown { instrument },
        QuotationError::Numeric(_) => NotComputable::Numeric { code: "numeric" },
    }
}

fn position_value(
    assessment: &PositionAssessment,
    quotation: PositionQuotation<'_>,
    request: &ReturnsRequest<'_>,
) -> Result<Dec, NotComputable> {
    if let PositionAssessmentKind::Selected(selected) = &assessment.kind {
        if selected.candidate.basis_evidence_contradicts {
            return Err(NotComputable::QuotationBasisContradictsEvidence {
                instrument: assessment.instrument,
            });
        }
    }
    let remaining_face = match quotation.basis {
        QuotationBasis::PercentOfRemainingFace => quotation.remaining_face?,
        QuotationBasis::MoneyPerUnit | QuotationBasis::Unknown => None,
    };
    let (money_per_unit, currency) = quotation
        .rule
        .money_per_unit(
            quotation.basis,
            quotation.price,
            quotation.venue_currency,
            remaining_face,
        )
        .map_err(|error| quotation_error(error, assessment.instrument))?;
    let local = assessment
        .quantity
        .0
        .checked_mul(money_per_unit)
        .map_err(|_| NotComputable::Numeric { code: "numeric" })?;
    let rate = request
        .fx
        .rate(currency, request.report_currency, request.as_of)
        .ok_or(NotComputable::MissingFxRate {
            from: currency,
            to: request.report_currency,
            date: request.as_of,
        })?;
    local
        .checked_mul(rate)
        .map_err(|_| NotComputable::Numeric { code: "numeric" })
}

/// Блок качества данных строится из состояния, реестра сверки и оценки
/// периметра, а не из желания показать зелёный статус.
fn data_quality(
    state: &LedgerState,
    request: &ReturnsRequest,
    positions: &[PositionValue],
) -> DataQuality {
    let mut issues = Vec::new();
    for account in state.coverage().restored_accounts() {
        issues.push(MaterialIssue::RestoredWithoutBasis { account: *account });
    }
    for (account, money) in state.balances().negative_cash() {
        issues.push(MaterialIssue::NegativeCash {
            account,
            currency: money.currency(),
        });
    }
    if let Some(date) = state.coverage().first_event() {
        issues.push(MaterialIssue::HistoryStartsAt { date });
    }

    let (_, accrued_rule) = accrued_interest_rule();
    issues.extend(accrued_mismatch_issues(positions, request, &accrued_rule));
    let mut position_coverage = PositionCoverage {
        evaluated_positions: 0,
        total_positions: positions.len() as u32,
        selected: Vec::new(),
        uncovered: Vec::new(),
        legacy_derived: Vec::new(),
    };
    let mut executability = ExecutabilityAccumulator::default();
    for position in positions {
        let assessment = &position.assessment;
        match (&assessment.kind, &position.value) {
            (PositionAssessmentKind::Selected(selected), Ok(value)) => {
                position_coverage.evaluated_positions += 1;
                position_coverage.selected.push(EvaluatedPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    quantity: assessment.quantity,
                    price: (**selected).clone(),
                });
                executability.add(selected.candidate.executability, *value);
            }
            (PositionAssessmentKind::Selected(_), Err(reason)) => {
                position_coverage.uncovered.push(UncoveredPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    reason: UncoveredReason::NotComputable {
                        reason: reason.clone(),
                    },
                });
            }
            (PositionAssessmentKind::LegacyDerived(quality), Ok(value)) => {
                position_coverage.evaluated_positions += 1;
                position_coverage
                    .legacy_derived
                    .push(LegacyDerivedPosition {
                        account: assessment.account,
                        custody: assessment.custody,
                        instrument: assessment.instrument,
                        quality: *quality,
                    });
                executability.add(SourceExecutability::Unknown, *value);
            }
            (PositionAssessmentKind::LegacyDerived(_), Err(reason)) => {
                position_coverage.uncovered.push(UncoveredPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    reason: UncoveredReason::NotComputable {
                        reason: reason.clone(),
                    },
                });
            }
            (PositionAssessmentKind::Uncovered(reason), _) => {
                position_coverage.uncovered.push(UncoveredPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    reason: reason.clone(),
                });
            }
        }
    }

    // Стоимость по счетам может не посчитаться — например, без цены.
    // Тогда взвешивать покрытие нечем, и оно честно остаётся
    // неизвестным, а не выдаётся за полное.
    let values =
        xirr::account_values_from_position_values(state, request, positions).unwrap_or_default();
    let mut shares = Shares::default();
    for (account, value) in &values {
        if request.perimeter.financing_present(*account) {
            issues.push(MaterialIssue::UnsupportedFinancing { account: *account });
        }
        // Деньги подтверждаются измерением `cash`, бумаги — измерением
        // `positions`. Это разные утверждения о разных частях счёта,
        // и взвешивать их одним статусом значило бы либо занижать
        // подтверждение денег из-за неподтверждённых бумаг, либо
        // наоборот.
        for (part, dimension) in [
            (value.cash, Dimension::Cash),
            (value.positions, Dimension::Positions),
        ] {
            if part.is_zero() {
                // Измерению, в котором у счёта ничего нет, нечего
                // подтверждать: сообщать о его неподтверждённости —
                // это шум, а не проблема.
                continue;
            }
            let status = request
                .ledger
                .status_for(*account, request.as_of, dimension);
            match status {
                DimensionStatus::Discrepant => issues.push(MaterialIssue::Discrepancy {
                    account: *account,
                    dimension,
                }),
                DimensionStatus::Provisional => {
                    issues.push(MaterialIssue::NoIndependentSource {
                        account: *account,
                        dimension,
                    });
                }
                DimensionStatus::AcceptedInternal | DimensionStatus::AcceptedIndependent => {}
            }
            shares.add(status, part.inner().abs());
        }
    }
    let nav_coverage = shares.finish();

    let material =
        !position_coverage.uncovered.is_empty() || issues.iter().any(MaterialIssue::is_defect);
    let status = if material {
        DataQualityStatus::Incomplete
    } else if nav_coverage.provisional.is_zero() && nav_coverage.discrepant.is_zero() {
        DataQualityStatus::Clean
    } else {
        DataQualityStatus::Mixed
    };
    DataQuality {
        status,
        nav_coverage,
        position_coverage,
        executability: executability.finish(),
        material_issues: issues,
    }
}

#[derive(Debug, Default)]
struct ExecutabilityAccumulator {
    evaluated_positions_value: rust_decimal::Decimal,
    executable: rust_decimal::Decimal,
    indicative_previous_close: rust_decimal::Decimal,
    unknown: rust_decimal::Decimal,
}

impl ExecutabilityAccumulator {
    fn add(&mut self, executability: SourceExecutability, value: Dec) {
        let value = value.inner().abs();
        self.evaluated_positions_value += value;
        match executability {
            SourceExecutability::Executable => self.executable += value,
            SourceExecutability::IndicativePreviousClose => self.indicative_previous_close += value,
            SourceExecutability::Unknown => self.unknown += value,
        }
    }

    fn finish(self) -> ExecutabilityShares {
        let total = self.evaluated_positions_value;
        if total.is_zero() {
            return ExecutabilityShares {
                evaluated_positions_value: Dec::zero(),
                executable: Dec::zero(),
                indicative_previous_close: Dec::zero(),
                unknown: Dec::one(),
            };
        }
        let executable = self.executable / total;
        let indicative_previous_close = self.indicative_previous_close / total;
        let unknown = self.unknown / total;
        ExecutabilityShares {
            evaluated_positions_value: Dec::new(total),
            executable: Dec::new(executable),
            indicative_previous_close: Dec::new(indicative_previous_close),
            unknown: Dec::new(unknown),
        }
    }
}

/// Накопитель долей.
///
/// Считает в `rust_decimal`, потому что доля — расчётная величина,
/// а не проведённая сумма (§3.4).
#[derive(Debug, Default)]
struct Shares {
    independent: rust_decimal::Decimal,
    internal: rust_decimal::Decimal,
    provisional: rust_decimal::Decimal,
    discrepant: rust_decimal::Decimal,
}

impl Shares {
    fn add(&mut self, level: DimensionStatus, weight: rust_decimal::Decimal) {
        let slot = match level {
            DimensionStatus::AcceptedIndependent => &mut self.independent,
            DimensionStatus::AcceptedInternal => &mut self.internal,
            DimensionStatus::Provisional => &mut self.provisional,
            DimensionStatus::Discrepant => &mut self.discrepant,
        };
        *slot += weight;
    }

    /// Доли от суммы весов.
    ///
    /// Нулевая сумма означает пустой портфель или непосчитанную
    /// стоимость: доли неопределимы, и честный ответ — «ничего не
    /// подтверждено», а не деление на ноль и не выдуманная единица
    /// в независимом подтверждении.
    fn finish(self) -> NavCoverage {
        let total = self.independent + self.internal + self.provisional + self.discrepant;
        if total.is_zero() {
            return NavCoverage {
                accepted_independent: Dec::zero(),
                accepted_internal: Dec::zero(),
                provisional: Dec::one(),
                discrepant: Dec::zero(),
            };
        }
        NavCoverage {
            accepted_independent: Dec::new(self.independent / total),
            accepted_internal: Dec::new(self.internal / total),
            provisional: Dec::new(self.provisional / total),
            discrepant: Dec::new(self.discrepant / total),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::xirr::SolverRefusal;

    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{Money, PerUnitAmount, PostedMinor, Quantity};
    use crate::projection::lots::LotBook;
    use crate::projection::{ProjectionContext, project};
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use crate::valuation::{PriceKind as CorePriceKind, PriceOrigin, PriceQuality};
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    static EMPTY_BOND_SCHEDULES: BTreeMap<InstrumentId, BondSchedule> = BTreeMap::new();
    static EMPTY_ACCRUED_OBSERVATIONS: BTreeMap<(InstrumentId, Venue, Date), PerUnitAmount> =
        BTreeMap::new();

    fn report_for(state: &LedgerState, coordinate: KnowledgeCoordinate) -> ReturnsReport {
        let contour = ContourDefinition::new(
            ContourId(uuid::Uuid::nil()),
            ContourVersion(1),
            Vec::<AccountId>::new(),
        );
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate,
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        returns_report(state, &request)
    }

    fn report_for_schedules(
        state: &LedgerState,
        schedules: &BTreeMap<InstrumentId, BondSchedule>,
    ) -> ReturnsReport {
        let contour = ContourDefinition::new(
            ContourId(uuid::Uuid::nil()),
            ContourVersion(1),
            Vec::<AccountId>::new(),
        );
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        returns_report(
            state,
            &ReturnsRequest {
                contour: &contour,
                as_of: date!(2026 - 08 - 26),
                report_currency: CurrencyCode::Rub,
                fx: &fx,
                solver_policy: SolverPolicy::returns_default(),
                coordinate: KnowledgeCoordinate::default(),
                ledger: &ledger,
                perimeter: &perimeter,
                market_prices: &[],
                bond_schedules: schedules,
                accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            },
        )
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn position_assessment(
        account: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> PositionAssessment {
        PositionAssessment {
            account,
            custody: None,
            instrument,
            quantity,
            raw_price: None,
            remaining_face: Ok(None),
            kind: PositionAssessmentKind::Uncovered(UncoveredReason::NoObservation),
        }
    }
    fn legacy_position_assessment(
        account: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
        raw_price: Option<crate::valuation::InstrumentPrice>,
    ) -> PositionAssessment {
        PositionAssessment {
            account,
            custody: None,
            instrument,
            quantity,
            raw_price,
            remaining_face: Ok(None),
            kind: PositionAssessmentKind::LegacyDerived(PriceQuality::CarriedForward),
        }
    }

    fn legacy_price(instrument: InstrumentId, price: &str) -> crate::valuation::InstrumentPrice {
        crate::valuation::InstrumentPrice {
            instrument,
            price: dec(price),
            currency: CurrencyCode::Rub,
            quality: PriceQuality::CarriedForward,
            as_of: date!(2026 - 08 - 25),
        }
    }

    fn position_values_for_tests(assessments: Vec<PositionAssessment>) -> Vec<PositionValue> {
        assessments
            .into_iter()
            .map(|assessment| PositionValue {
                assessment,
                value: Ok(Dec::zero()),
            })
            .collect()
    }

    fn selected_market_position_assessment(
        account: AccountId,
        instrument: InstrumentId,
        quantity: Quantity,
        venue: Venue,
        trade_date: Date,
    ) -> PositionAssessment {
        let candidate = PriceCandidate {
            instrument,
            price: dec("100"),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::Unknown,
            basis_evidence: String::new(),
            basis_evidence_contradicts: false,
            trade_date,
            observed_at: Some(datetime!(2026 - 08 - 26 09:00:00 UTC)),
            origin: PriceOrigin::Market {
                venue: venue.clone(),
                kind: CorePriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        };
        let selected = SelectedPrice {
            candidate: candidate.clone(),
            selection: crate::valuation::PriceSelection::AsObserved,
            freshness: crate::valuation::PriceFreshness::Fresh,
            provenance: crate::valuation::PriceProvenance {
                price_kind: Some("legal_close".to_owned()),
                origin: candidate.origin,
                venue: Some(venue.board.clone()),
                quotation_basis: QuotationBasis::Unknown,
                basis_evidence: String::new(),
                observed_at: candidate.observed_at,
                valuation_policy_version: 1,
                source_priority_version: 1,
                carry_forward_limit: 10,
                price_max_age: 30,
            },
        };
        PositionAssessment {
            account,
            custody: None,
            instrument,
            quantity,
            raw_price: None,
            remaining_face: Ok(None),
            kind: PositionAssessmentKind::Selected(Box::new(selected)),
        }
    }

    fn position_request<'a>(
        contour: &'a ContourDefinition,
        fx: &'a FxTable,
        ledger: &'a ReconciliationLedger,
        perimeter: &'a PerimeterAssessment,
    ) -> ReturnsRequest<'a> {
        ReturnsRequest {
            contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger,
            perimeter,
            market_prices: &[],
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        }
    }
    fn report_with_market_basis(basis: QuotationBasis) -> (ReturnsReport, InstrumentId) {
        report_with_market_basis_and_schedule(basis, None)
    }

    fn report_with_market_basis_and_schedule(
        basis: QuotationBasis,
        schedule: Option<BondSchedule>,
    ) -> (ReturnsReport, InstrumentId) {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let quantity = Quantity(dec("10"));
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        );
        let state = project(&[opening], &context)
            .expect("проекция позиции")
            .snapshot()
            .state()
            .clone();
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let candidate = PriceCandidate {
            instrument,
            price: dec("98.5"),
            currency: CurrencyCode::Rub,
            basis,
            basis_evidence: "test:market".to_owned(),
            basis_evidence_contradicts: false,
            trade_date: date!(2026 - 08 - 03),
            observed_at: Some(datetime!(2026 - 08 - 26 09:00:00 UTC)),
            origin: PriceOrigin::Market {
                venue: crate::valuation::Venue {
                    board: "TQOB".to_owned(),
                    session: 0,
                },
                kind: CorePriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        };
        let bond_schedules = schedule
            .map(|schedule| BTreeMap::from([(instrument, schedule)]))
            .unwrap_or_default();
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate {
                knowledge_as_of: datetime!(2026 - 08 - 26 12:00:00 UTC),
                source_priority_version: 1,
                valuation_policy_version: 1,
            },
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &bond_schedules,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        (returns_report(&state, &request), instrument)
    }
    #[test]
    fn legacy_оценка_входит_в_terminal_value_как_деньги_за_единицу() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let without_legacy_positions = position_values_from_assessments(Vec::new(), &request);
        let legacy_positions = position_values_from_assessments(
            vec![legacy_position_assessment(
                account,
                instrument,
                Quantity(dec("10")),
                Some(legacy_price(instrument, "12")),
            )],
            &request,
        );

        let without_legacy_value =
            xirr::terminal_value_from_position_values(&state, &request, &without_legacy_positions)
                .unwrap();
        let legacy_value =
            xirr::terminal_value_from_position_values(&state, &request, &legacy_positions).unwrap();
        let quality = data_quality(&state, &request, &legacy_positions);
        assert_eq!(quality.position_coverage.evaluated_positions, 1);
        assert_eq!(quality.position_coverage.legacy_derived.len(), 1);

        assert_eq!(without_legacy_value, Dec::zero());
        assert_eq!(
            legacy_value.checked_sub(without_legacy_value).unwrap(),
            dec("120")
        );
    }

    #[test]
    fn legacy_оценка_без_raw_price_становится_непокрытой_с_missing_price() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = legacy_position_assessment(account, instrument, Quantity(dec("10")), None);

        let positions = position_values_from_assessments(vec![assessment], &request);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let quality = data_quality(&state, &request, &positions);

        assert_eq!(
            positions[0].value,
            Err(NotComputable::MissingPrice { instrument })
        );
        assert_eq!(quality.position_coverage.evaluated_positions, 0);
        assert_eq!(quality.position_coverage.uncovered.len(), 1);
        assert_eq!(
            quality.position_coverage.uncovered[0].reason,
            UncoveredReason::NotComputable {
                reason: NotComputable::MissingPrice { instrument }
            }
        );
    }

    #[test]
    fn непокрытая_позиция_отображает_все_причины_в_not_computable() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let not_computable_instrument = InstrumentId::new_random();
        let cases = [
            (InstrumentId::new_random(), UncoveredReason::NoObservation),
            (InstrumentId::new_random(), UncoveredReason::TooOld),
            (InstrumentId::new_random(), UncoveredReason::AmbiguousVenue),
            (
                InstrumentId::new_random(),
                UncoveredReason::AmbiguousCandidate,
            ),
            (
                not_computable_instrument,
                UncoveredReason::NotComputable {
                    reason: NotComputable::QuotationBasisUnknown {
                        instrument: not_computable_instrument,
                    },
                },
            ),
        ];
        let assessments = cases
            .iter()
            .map(|(instrument, reason)| {
                let mut assessment = position_assessment(account, *instrument, Quantity(dec("1")));
                assessment.kind = PositionAssessmentKind::Uncovered(reason.clone());
                assessment
            })
            .collect();

        let positions = position_values_from_assessments(assessments, &request);

        for ((instrument, reason), position) in cases.iter().zip(&positions) {
            let expected = match reason {
                UncoveredReason::NotComputable { reason } => reason.clone(),
                UncoveredReason::NoObservation
                | UncoveredReason::TooOld
                | UncoveredReason::AmbiguousVenue
                | UncoveredReason::AmbiguousCandidate => NotComputable::MissingPrice {
                    instrument: *instrument,
                },
            };
            assert_eq!(position.value, Err(expected));
        }
    }

    #[test]
    fn uncovered_reason_code_имеет_коды_для_всех_ветвей() {
        let instrument = InstrumentId::new_random();
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::NoObservation),
            "no_observation"
        );
        assert_eq!(uncovered_reason_code(&UncoveredReason::TooOld), "too_old");
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::AmbiguousVenue),
            "ambiguous_venue"
        );
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::AmbiguousCandidate),
            "ambiguous_candidate"
        );
        assert_eq!(
            uncovered_reason_code(&UncoveredReason::NotComputable {
                reason: NotComputable::MissingPrice { instrument }
            }),
            "missing_price"
        );
    }

    #[test]
    fn policy_uncovered_reason_отображает_все_четыре_варианта() {
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::NoObservation),
            UncoveredReason::NoObservation
        );
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::TooOld),
            UncoveredReason::TooOld
        );
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::AmbiguousVenue),
            UncoveredReason::AmbiguousVenue
        );
        assert_eq!(
            policy_uncovered_reason(PolicyUncoveredReason::AmbiguousCandidate),
            UncoveredReason::AmbiguousCandidate
        );
    }

    #[test]
    fn биржевая_оценка_без_журнальной_оценки_входит_в_стоимость_контура() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::MoneyPerUnit);

        assert_eq!(report.terminal_value, Computed::Value(dec("985")));
        assert_eq!(report.data_quality.position_coverage.evaluated_positions, 1);
        assert!(
            report
                .data_quality
                .position_coverage
                .uncovered
                .iter()
                .all(|position| position.instrument != instrument)
        );
    }
    #[test]
    fn неизвестное_основание_делает_позицию_непокрытой_с_причиной_пересчёта() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::Unknown);
        let coverage = &report.data_quality.position_coverage;

        assert_eq!(coverage.evaluated_positions, 0);
        assert_eq!(coverage.uncovered.len(), 1);
        assert_eq!(
            coverage.uncovered[0].reason,
            UncoveredReason::NotComputable {
                reason: NotComputable::QuotationBasisUnknown { instrument },
            }
        );
        assert_eq!(
            report.terminal_value.reason(),
            Some(&NotComputable::QuotationBasisUnknown { instrument })
        );
        assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
    }

    #[test]
    fn uncovered_position_alone_makes_quality_incomplete() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let positions = position_values_for_tests(vec![position_assessment(
            account,
            instrument,
            Quantity(dec("1")),
        )]);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));

        let quality = data_quality(&state, &request, &positions);

        assert_eq!(quality.position_coverage.uncovered.len(), 1);
        assert_eq!(quality.status, DataQualityStatus::Incomplete);
    }

    #[test]
    fn противоречие_основания_имеет_отдельную_причину_позиции() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let mut assessment = selected_market_position_assessment(
            account,
            instrument,
            Quantity(dec("10")),
            Venue {
                board: "TQBR".to_owned(),
                session: 3,
            },
            date!(2026 - 08 - 26),
        );
        if let PositionAssessmentKind::Selected(selected) = &mut assessment.kind {
            selected.candidate.basis = QuotationBasis::Unknown;
            selected.candidate.basis_evidence_contradicts = true;
        }

        let positions = position_values_from_assessments(vec![assessment], &request);

        assert_eq!(
            positions[0].value,
            Err(NotComputable::QuotationBasisContradictsEvidence { instrument })
        );
    }

    #[test]
    fn покрытие_считает_каждую_позицию_ровно_один_раз() {
        let (report, _) = report_with_market_basis(QuotationBasis::MoneyPerUnit);
        let coverage = &report.data_quality.position_coverage;

        assert_eq!(
            coverage.evaluated_positions as usize + coverage.uncovered.len(),
            coverage.total_positions as usize
        );
        assert_eq!(
            coverage.evaluated_positions as usize,
            coverage.selected.len() + coverage.legacy_derived.len()
        );
        assert!(report.terminal_value.value().is_some());
    }

    #[test]
    fn a_share_does_not_get_bond_position_attributes() {
        let (report, _) = report_with_market_basis(QuotationBasis::MoneyPerUnit);

        assert!(report.bond_attributes.is_empty());
    }

    #[test]
    fn a_bond_without_a_schedule_keeps_a_schedule_missing_attribute() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::PercentOfRemainingFace);

        let attributes = report
            .bond_attributes
            .first()
            .expect("облигация должна иметь атрибуты");
        assert!(matches!(
            attributes.accrued_interest,
            Computed::NotComputable {
                reason: NotComputable::ScheduleMissing { instrument: actual }
            } if actual == instrument
        ));
    }

    #[test]
    fn a_coupon_as_the_next_posting_has_no_principal_return_finality() {
        let (report, instrument) = report_with_market_basis_and_schedule(
            QuotationBasis::PercentOfRemainingFace,
            Some(BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![crate::bond::PrincipalReturn {
                    repayment_date: date!(2026 - 10 - 01),
                    share_percent: dec("100"),
                }],
                ..Default::default()
            }),
        );
        let attributes = report
            .bond_attributes
            .iter()
            .find(|attributes| attributes.instrument == instrument)
            .expect("процентная облигация должна иметь атрибуты");

        assert_eq!(attributes.next_posting_date, Some(date!(2026 - 09 - 01)));
        assert_eq!(attributes.next_principal_return_finality, None);
    }

    #[test]
    fn principal_return_finality_comes_from_the_return_on_the_next_date() {
        let (report, instrument) = report_with_market_basis_and_schedule(
            QuotationBasis::PercentOfRemainingFace,
            Some(BondSchedule {
                periods: vec![],
                principal_returns: vec![
                    crate::bond::PrincipalReturn {
                        repayment_date: date!(2026 - 09 - 15),
                        share_percent: dec("40"),
                    },
                    crate::bond::PrincipalReturn {
                        repayment_date: date!(2026 - 10 - 15),
                        share_percent: dec("60"),
                    },
                ],
                ..Default::default()
            }),
        );
        let attributes = report
            .bond_attributes
            .iter()
            .find(|attributes| attributes.instrument == instrument)
            .expect("процентная облигация должна иметь атрибуты");

        assert_eq!(attributes.next_posting_date, Some(date!(2026 - 09 - 15)));
        assert_eq!(
            attributes.next_principal_return_finality,
            Some(PrincipalReturnFinality::Partial)
        );
    }

    #[test]
    fn a_bond_quoted_in_percent_is_valued_through_its_remaining_face() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        let value = position_value(
            &assessment,
            PositionQuotation {
                price: dec("98.5"),
                basis: QuotationBasis::PercentOfRemainingFace,
                venue_currency: CurrencyCode::Rub,
                remaining_face: Ok(Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub))),
                rule: &rule,
            },
            &request,
        );

        assert_eq!(value, Ok(dec("9850.0")));
    }

    #[test]
    fn процентная_котировка_входит_в_nav_через_остаточный_номинал() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let venue = Venue {
            board: "TQOB".to_owned(),
            session: 0,
        };
        let mut assessment = selected_market_position_assessment(
            account,
            instrument,
            Quantity(dec("10")),
            venue,
            date!(2026 - 08 - 26),
        );
        if let PositionAssessmentKind::Selected(selected) = &mut assessment.kind {
            selected.candidate.price = dec("98.5");
            selected.candidate.basis = QuotationBasis::PercentOfRemainingFace;
            selected.provenance.quotation_basis = QuotationBasis::PercentOfRemainingFace;
        }
        assessment.remaining_face = Ok(Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub)));
        let positions = position_values_from_assessments(vec![assessment], &request);
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));

        assert_eq!(
            xirr::terminal_value_from_position_values(&state, &request, &positions),
            Ok(dec("9850.0"))
        );
    }

    #[test]
    fn a_bond_without_a_known_face_is_not_computable_with_its_own_reason() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        assert_eq!(
            position_value(
                &assessment,
                PositionQuotation {
                    price: dec("98.5"),
                    basis: QuotationBasis::PercentOfRemainingFace,
                    venue_currency: CurrencyCode::Rub,
                    remaining_face: Ok(None),
                    rule: &rule,
                },
                &request,
            ),
            Err(NotComputable::RemainingFaceUnknown { instrument }),
        );
    }

    #[test]
    fn an_undecided_basis_is_not_computable_rather_than_valued_as_money() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        assert_eq!(
            position_value(
                &assessment,
                PositionQuotation {
                    price: dec("98.5"),
                    basis: QuotationBasis::Unknown,
                    venue_currency: CurrencyCode::Rub,
                    remaining_face: Ok(None),
                    rule: &rule,
                },
                &request,
            ),
            Err(NotComputable::QuotationBasisUnknown { instrument }),
        );
    }

    #[test]
    fn lots_that_disagree_about_the_remaining_face_refuse_instead_of_averaging() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        assert_eq!(
            position_value(
                &assessment,
                PositionQuotation {
                    price: dec("98.5"),
                    basis: QuotationBasis::PercentOfRemainingFace,
                    venue_currency: CurrencyCode::Rub,
                    remaining_face: Err(NotComputable::RemainingFaceAmbiguous { instrument }),
                    rule: &rule,
                },
                &request,
            ),
            Err(NotComputable::RemainingFaceAmbiguous { instrument }),
        );
    }

    #[test]
    fn a_share_quoted_in_money_is_valued_exactly_as_before() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let assessment = position_assessment(account, instrument, Quantity(dec("10")));
        let (_, rule) = quotation_rule();

        let value = position_value(
            &assessment,
            PositionQuotation {
                price: dec("270.13"),
                basis: QuotationBasis::MoneyPerUnit,
                venue_currency: CurrencyCode::Rub,
                remaining_face: Ok(None),
                rule: &rule,
            },
            &request,
        );

        assert_eq!(value, Ok(dec("2701.30")));
    }

    #[test]
    fn the_report_names_the_quotation_rule_it_applied() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let report = report_for(&state, KnowledgeCoordinate::default());
        assert_eq!(report.applied_rules.quotation_rule, QuotationRuleVersion(1));
    }

    #[test]
    fn the_same_coordinate_yields_the_same_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let coordinate = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };

        let first = report_for(&state, coordinate);
        let second = report_for(&state, coordinate);
        assert_eq!(first.coordinate, coordinate);
        assert_eq!(second.coordinate, coordinate);
        assert_eq!(first.inputs_hash, second.inputs_hash);
        assert_eq!(first.inputs_hash.len(), 64);
        let equivalent_coordinate = KnowledgeCoordinate {
            knowledge_as_of: coordinate
                .knowledge_as_of
                .to_offset(UtcOffset::from_hms(3, 0, 0).unwrap()),
            ..coordinate
        };
        assert_eq!(
            first.inputs_hash,
            report_for(&state, equivalent_coordinate).inputs_hash
        );
    }

    #[test]
    fn a_different_knowledge_time_yields_a_different_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let first = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };
        let second = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-27 09:00:00 UTC),
            ..first
        };

        let first_report = report_for(&state, first);
        let second_report = report_for(&state, second);
        assert_eq!(first_report.coordinate, first);
        assert_eq!(second_report.coordinate, second);
        assert_ne!(first_report.inputs_hash, second_report.inputs_hash);
    }

    #[test]
    fn a_source_correction_inside_the_window_changes_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let coordinate = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate,
            ledger: &ledger,
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            perimeter: &perimeter,
            market_prices: &[],
        };

        let hash_for_venue = |venue: &str| {
            let origin = crate::valuation::PriceOrigin::Market {
                venue: crate::valuation::Venue {
                    board: venue.to_owned(),
                    session: 0,
                },
                kind: crate::valuation::PriceKind::LegalClose,
            };
            let selected = SelectedPrice {
                candidate: PriceCandidate {
                    instrument,
                    price: Dec::new(rust_decimal::Decimal::from(100)),
                    currency: CurrencyCode::Rub,
                    basis: crate::valuation::QuotationBasis::Unknown,
                    basis_evidence: String::new(),
                    basis_evidence_contradicts: false,
                    trade_date: date!(2026 - 08 - 26),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    origin: origin.clone(),
                    executability: SourceExecutability::Executable,
                },
                selection: crate::valuation::PriceSelection::AsObserved,
                freshness: crate::valuation::PriceFreshness::Fresh,
                provenance: crate::valuation::PriceProvenance {
                    price_kind: Some("legal_close".to_owned()),
                    origin,
                    venue: Some(venue.to_owned()),
                    quotation_basis: crate::valuation::QuotationBasis::Unknown,
                    basis_evidence: String::new(),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    valuation_policy_version: coordinate.valuation_policy_version,
                    source_priority_version: coordinate.source_priority_version,
                    carry_forward_limit: 10,
                    price_max_age: 30,
                },
            };
            inputs_hash_with_assessments(
                &state,
                &request,
                vec![PositionAssessment {
                    account,
                    custody: None,
                    instrument,
                    quantity: crate::money::Quantity(Dec::one()),
                    raw_price: None,
                    remaining_face: Ok(None),
                    kind: PositionAssessmentKind::Selected(Box::new(selected)),
                }],
            )
        };

        assert_ne!(
            hash_for_venue("moex"),
            hash_for_venue("corrected-source"),
            "исправление provenance выбранного наблюдения внутри окна обязано менять хеш"
        );
    }
    #[test]
    fn a_quotation_basis_change_inside_the_window_changes_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let coordinate = KnowledgeCoordinate {
            knowledge_as_of: datetime!(2026-08-26 09:00:00 UTC),
            source_priority_version: 1,
            valuation_policy_version: 1,
        };
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate,
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &BTreeMap::new(),
            accrued_observations: &BTreeMap::new(),
        };

        let hash_for_basis = |basis: QuotationBasis| {
            let origin = crate::valuation::PriceOrigin::Market {
                venue: crate::valuation::Venue {
                    board: "moex".to_owned(),
                    session: 0,
                },
                kind: crate::valuation::PriceKind::LegalClose,
            };
            let selected = SelectedPrice {
                candidate: PriceCandidate {
                    instrument,
                    price: Dec::new(rust_decimal::Decimal::from(98)),
                    currency: CurrencyCode::Rub,
                    basis,
                    basis_evidence: "iss:engines/stock/markets/bonds".to_owned(),
                    basis_evidence_contradicts: false,
                    trade_date: date!(2026 - 08 - 26),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    origin: origin.clone(),
                    executability: SourceExecutability::Executable,
                },
                selection: crate::valuation::PriceSelection::AsObserved,
                freshness: crate::valuation::PriceFreshness::Fresh,
                provenance: crate::valuation::PriceProvenance {
                    price_kind: Some("legal_close".to_owned()),
                    origin,
                    venue: Some("moex".to_owned()),
                    quotation_basis: basis,
                    basis_evidence: "iss:engines/stock/markets/bonds".to_owned(),
                    observed_at: Some(datetime!(2026-08-26 08:00:00 UTC)),
                    valuation_policy_version: coordinate.valuation_policy_version,
                    source_priority_version: coordinate.source_priority_version,
                    carry_forward_limit: 10,
                    price_max_age: 30,
                },
            };
            inputs_hash_with_assessments(
                &state,
                &request,
                vec![PositionAssessment {
                    account,
                    custody: None,
                    instrument,
                    quantity: crate::money::Quantity(Dec::one()),
                    raw_price: None,
                    remaining_face: Ok(None),
                    kind: PositionAssessmentKind::Selected(Box::new(selected)),
                }],
            )
        };

        assert_ne!(
            hash_for_basis(QuotationBasis::MoneyPerUnit),
            hash_for_basis(QuotationBasis::PercentOfRemainingFace),
            "смена доказанного основания обязана менять хеш входов"
        );
    }

    #[test]
    fn an_owner_valuation_is_money_per_unit_by_contract_not_by_guess() {
        // §10.3: журнальная цена владельца — деньги за единицу
        // по определению, а не процент номинала.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let quantity = Quantity(Dec::new(rust_decimal::Decimal::from(10)));
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let opening = event_with(
            account,
            date!(2026 - 08 - 02),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        );
        let valuation = event_with(
            account,
            date!(2026 - 08 - 03),
            2,
            EventKind::Valuation {
                instrument,
                price: Dec::new(rust_decimal::Decimal::from(98)),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::OwnerEstimate,
            },
            vec![],
        );
        let state = project(&[opening, valuation], &context)
            .expect("проекция владельческой оценки")
            .snapshot()
            .state()
            .clone();
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 03),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            perimeter: &perimeter,
            market_prices: &[],
        };
        let assessments = position_assessments(&state, &request);
        let PositionAssessmentKind::Selected(selected) = &assessments[0].kind else {
            panic!("владельческая оценка обязана быть выбрана");
        };
        assert_eq!(
            selected.candidate.basis,
            crate::valuation::QuotationBasis::MoneyPerUnit
        );
        assert_eq!(selected.candidate.basis_evidence, "journal:valuation");
        assert_eq!(selected.candidate.observed_at, None);
        assert_eq!(selected.provenance.observed_at, None);
    }

    #[test]
    fn future_or_foreign_inputs_do_not_change_the_inputs_hash() {
        let base = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let account = AccountId::new_random();
        let foreign_contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &foreign_contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let future_event = event_with(
            account,
            date!(2026 - 09 - 01),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let future_state = project(&[future_event], &context)
            .expect("будущее состояние")
            .snapshot()
            .state()
            .clone();
        let coordinate = KnowledgeCoordinate::default();

        assert_eq!(
            report_for(&base, coordinate).inputs_hash,
            report_for(&future_state, coordinate).inputs_hash
        );
    }

    #[test]
    fn every_data_quality_status_has_a_machine_readable_code() {
        // Внешний агент разбирает код, а не текст. Пустая строка вместо
        // кода неотличима от «статуса нет».
        assert_eq!(DataQualityStatus::Clean.code(), "clean");
        assert_eq!(DataQualityStatus::Mixed.code(), "mixed");
        assert_eq!(DataQualityStatus::Incomplete.code(), "incomplete");
    }
    #[test]
    fn overlapping_accrual_is_a_distinct_not_computable_reason() {
        let instrument = InstrumentId::new_random();
        let reason = accrued_error(AccruedInterestError::OverlappingCoverage, instrument);
        assert_eq!(
            reason,
            NotComputable::OverlappingScheduleCoverage { instrument }
        );
        assert_eq!(reason.code(), "overlapping_schedule_coverage");
    }
    #[test]
    fn shares_add_accumulates_weights_before_normalizing() {
        let mut shares = Shares::default();
        shares.add(
            DimensionStatus::AcceptedIndependent,
            rust_decimal::Decimal::new(2, 0),
        );
        shares.add(
            DimensionStatus::Provisional,
            rust_decimal::Decimal::new(1, 0),
        );
        assert_eq!(shares.independent, rust_decimal::Decimal::new(2, 0));
        assert_eq!(shares.provisional, rust_decimal::Decimal::new(1, 0));

        let coverage = shares.finish();
        assert_eq!(
            coverage.accepted_independent,
            Dec::new(rust_decimal::Decimal::new(2, 0) / rust_decimal::Decimal::new(3, 0))
        );
        assert_eq!(
            coverage.provisional,
            Dec::new(rust_decimal::Decimal::new(1, 0) / rust_decimal::Decimal::new(3, 0))
        );
    }

    /// Строит состояние из одного пополнения и одной оценки заданного
    /// качества. Больше в блоке качества данных ничего не участвует.
    fn quality_of(price_quality: PriceQuality) -> DataQuality {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let events = vec![
            event_with(
                account,
                date!(2025 - 01 - 01),
                1,
                EventKind::CashIn { amount },
                vec![Leg::cash(account, amount)],
            ),
            event_with(
                account,
                date!(2025 - 02 - 01),
                2,
                EventKind::Valuation {
                    instrument,
                    price: Dec::one(),
                    currency: CurrencyCode::Rub,
                    quality: price_quality,
                },
                vec![],
            ),
        ];
        let projection = project(&events, &ctx).expect("проекция");
        // Реестр сверки пуст, оценка периметра пуста: этот помощник
        // проверяет ровно материальные проблемы состояния, а покрытие
        // и периметр разобраны отдельными тестами.
        let ledger = crate::reconciliation::ReconciliationLedger::default();
        let perimeter = crate::perimeter::PerimeterAssessment::empty(PerimeterPolicy::default());
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let request = ReturnsRequest {
            contour: &contour,
            coordinate: KnowledgeCoordinate::default(),
            as_of: date!(2025 - 03 - 01),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            perimeter: &perimeter,
            market_prices: &[],
        };
        let positions = position_values(projection.snapshot().state(), &request);
        data_quality(projection.snapshot().state(), &request, &positions)
    }

    #[test]
    fn the_start_of_history_is_a_fact_about_the_period_not_a_defect() {
        // Полная цена и никаких других проблем: остаётся только отметка
        // «данных ранее такой-то даты нет». Она сообщается всегда, но
        // неполнотой не является — иначе статус `Incomplete` перестал бы
        // что-либо означать, потому что стоял бы всегда.
        let quality = quality_of(PriceQuality::Executable);
        assert_eq!(quality.status, DataQualityStatus::Mixed);
        assert!(
            quality
                .material_issues
                .iter()
                .any(|issue| matches!(issue, MaterialIssue::HistoryStartsAt { .. })),
            "начало истории обязано быть названо"
        );
    }

    #[test]
    fn executability_shares_are_weighted_by_evaluated_position_value() {
        let mut shares = ExecutabilityAccumulator::default();
        shares.add(
            SourceExecutability::Executable,
            Dec::new(rust_decimal::Decimal::new(2, 0)),
        );
        shares.add(
            SourceExecutability::IndicativePreviousClose,
            Dec::new(rust_decimal::Decimal::new(1, 0)),
        );
        shares.add(
            SourceExecutability::Unknown,
            Dec::new(rust_decimal::Decimal::new(1, 0)),
        );

        let shares = shares.finish();
        assert_eq!(
            shares.evaluated_positions_value,
            Dec::new(rust_decimal::Decimal::new(4, 0))
        );
        assert_eq!(
            shares.executable,
            Dec::new(rust_decimal::Decimal::new(50, 2))
        );
        assert_eq!(
            shares.indicative_previous_close,
            Dec::new(rust_decimal::Decimal::new(25, 2))
        );
        assert_eq!(
            shares.executable.inner()
                + shares.indicative_previous_close.inner()
                + shares.unknown.inner(),
            rust_decimal::Decimal::ONE
        );
    }

    #[test]
    fn uncovered_positions_have_reasons_but_no_cost_percentage() {
        let instrument = InstrumentId::new_random();
        let coverage = PositionCoverage {
            evaluated_positions: 1,
            total_positions: 2,
            selected: Vec::new(),
            uncovered: vec![UncoveredPosition {
                account: AccountId::new_random(),
                custody: None,
                instrument,
                reason: UncoveredReason::TooOld,
            }],
            legacy_derived: Vec::new(),
        };

        assert_eq!(coverage.evaluated_positions, 1);
        assert_eq!(coverage.total_positions, 2);
        assert_eq!(coverage.uncovered[0].reason, UncoveredReason::TooOld);
    }

    #[test]
    fn liquidation_estimate_keeps_unknown_costs_and_tax_typed() {
        let estimate = LiquidationEstimate {
            value_before_exit_costs_and_tax: Computed::Value(Dec::new(rust_decimal::Decimal::new(
                100, 0,
            ))),
            executability: ExecutabilityShares {
                evaluated_positions_value: Dec::new(rust_decimal::Decimal::new(100, 0)),
                executable: Dec::zero(),
                indicative_previous_close: Dec::one(),
                unknown: Dec::zero(),
            },
            exit_costs: AmountQualification::Unknown,
            tax: AmountQualification::Unknown,
            accrued_interest_payable_on_termination: Computed::Value(Dec::zero()),
        };

        assert!(matches!(estimate.exit_costs, AmountQualification::Unknown));
        assert!(matches!(estimate.tax, AmountQualification::Unknown));
        assert_eq!(
            ReturnsReport::LIQUIDATION_LABEL,
            "liquidation_value_before_exit_costs_and_tax"
        );
    }

    #[test]
    fn indicative_previous_close_has_zero_executable_share_without_error() {
        let mut shares = ExecutabilityAccumulator::default();
        shares.add(
            SourceExecutability::IndicativePreviousClose,
            Dec::new(rust_decimal::Decimal::new(100, 0)),
        );
        let shares = shares.finish();
        assert!(shares.executable.is_zero());
        assert_eq!(shares.indicative_previous_close, Dec::one());
        assert_eq!(
            shares.executable.inner()
                + shares.indicative_previous_close.inner()
                + shares.unknown.inner(),
            rust_decimal::Decimal::ONE
        );
    }

    #[test]
    fn a_kopeck_of_disagreement_with_the_exchange_is_rounding_not_an_issue() {
        assert!(!accrued_mismatch_is_material(
            dec("15.17"),
            dec("15.18"),
            CurrencyCode::Rub
        ));
    }

    #[test]
    fn a_real_disagreement_names_both_numbers() {
        assert!(accrued_mismatch_is_material(
            dec("15.17"),
            dec("22.40"),
            CurrencyCode::Rub
        ));
    }
    #[test]
    fn accrued_mismatch_amounts_are_position_totals_and_duplicate_accounts_are_merged() {
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), []);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let mut schedules = BTreeMap::new();
        schedules.insert(
            instrument,
            BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![],
                ..Default::default()
            },
        );
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let mut observed = BTreeMap::new();
        observed.insert(
            (instrument, venue.clone(), date!(2026 - 08 - 26)),
            PerUnitAmount::new(dec("22.40"), CurrencyCode::Rub),
        );
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &schedules,
            accrued_observations: &observed,
        };
        let assessments = vec![
            selected_market_position_assessment(
                AccountId::new_random(),
                instrument,
                Quantity(dec("60")),
                venue.clone(),
                date!(2026 - 08 - 26),
            ),
            selected_market_position_assessment(
                AccountId::new_random(),
                instrument,
                Quantity(dec("40")),
                venue,
                date!(2026 - 08 - 26),
            ),
        ];

        let issues = accrued_mismatch_issues(
            &position_values_for_tests(assessments),
            &request,
            &AccruedInterestV1,
        );
        assert_eq!(issues.len(), 1);
        let MaterialIssue::AccruedInterestMismatch {
            computed,
            computed_currency,
            observed,
            observed_currency,
            quantity,
            ..
        } = &issues[0]
        else {
            panic!("ожидалось расхождение НКД");
        };
        assert_eq!(*computed, dec("2500.00"));
        assert_eq!(*computed_currency, CurrencyCode::Rub);
        assert_eq!(*observed, dec("2240.00"));
        assert_eq!(*observed_currency, CurrencyCode::Rub);
        assert_eq!(*quantity, Quantity(dec("100")));
    }
    #[test]
    fn accrued_mismatch_marks_close_values_in_different_currencies_as_material() {
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), []);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let schedules = BTreeMap::from([(
            instrument,
            BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![],
                ..Default::default()
            },
        )]);
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let observed = BTreeMap::from([(
            (instrument, venue.clone(), date!(2026 - 08 - 26)),
            PerUnitAmount::new(dec("25.01"), CurrencyCode::Usd),
        )]);
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &schedules,
            accrued_observations: &observed,
        };
        let issues = accrued_mismatch_issues(
            &position_values_for_tests(vec![selected_market_position_assessment(
                AccountId::new_random(),
                instrument,
                Quantity(dec("1")),
                venue,
                date!(2026 - 08 - 26),
            )]),
            &request,
            &AccruedInterestV1,
        );
        let MaterialIssue::AccruedInterestMismatch {
            computed,
            computed_currency,
            observed,
            observed_currency,
            ..
        } = &issues[0]
        else {
            panic!("ожидалось расхождение НКД");
        };
        assert_eq!(*computed_currency, CurrencyCode::Rub);
        assert_eq!(*computed, dec("25.00"));
        assert_eq!(*observed, dec("25.01"));
        assert_eq!(*observed_currency, CurrencyCode::Usd);
    }
    #[test]
    fn an_older_trade_date_does_not_supply_the_current_day_observation() {
        let instrument = InstrumentId::new_random();
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), []);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let observed = BTreeMap::from([(
            (instrument, venue.clone(), date!(2026 - 08 - 25)),
            PerUnitAmount::new(dec("22.40"), CurrencyCode::Rub),
        )]);
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &observed,
        };
        let assessment = selected_market_position_assessment(
            account,
            instrument,
            Quantity(dec("1")),
            venue,
            date!(2026 - 08 - 25),
        );

        assert!(
            accrued_mismatch_issues(
                &position_values_for_tests(vec![assessment]),
                &request,
                &AccruedInterestV1,
            )
            .is_empty()
        );
    }

    #[test]
    fn an_accrued_interest_mismatch_is_a_defect() {
        let issue = MaterialIssue::AccruedInterestMismatch {
            instrument: InstrumentId::new_random(),
            computed: dec("15.17"),
            computed_currency: CurrencyCode::Rub,
            observed: dec("22.40"),
            observed_currency: CurrencyCode::Rub,
            quantity: Quantity(dec("1")),
            date: date!(2026 - 08 - 26),
        };
        assert!(issue.is_defect());
    }
    #[test]
    fn a_report_with_an_accrued_mismatch_is_not_clean() {
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(dec("10"));
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: Some(Money::new(PostedMinor::new(100_000), CurrencyCode::Rub)),
                assertions: Default::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        );
        let valuation = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::Valuation {
                instrument,
                price: dec("100"),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::OwnerEstimate,
            },
            vec![],
        );
        let period = crate::reconciliation::claim::AssertionPeriod::between(
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 31),
        )
        .unwrap();
        let assertion = |sequence, claim| {
            event_with(
                account,
                date!(2026 - 08 - 26),
                sequence,
                EventKind::ControlAssertion { period, claim },
                vec![],
            )
        };
        let events = vec![
            opening,
            valuation,
            assertion(
                3,
                crate::reconciliation::claim::ControlClaim::PositionQuantity {
                    instrument,
                    custody,
                    quantity,
                    at: crate::reconciliation::claim::BalancePoint::Closing,
                },
            ),
            assertion(
                4,
                crate::reconciliation::claim::ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: crate::reconciliation::claim::BalancePoint::Closing,
                },
            ),
        ];
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let state = project(&events, &context)
            .unwrap()
            .snapshot()
            .state()
            .clone();
        let ledger = ReconciliationLedger::build(&events).unwrap();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let mut schedules = BTreeMap::new();
        schedules.insert(
            instrument,
            BondSchedule {
                periods: vec![crate::bond::AccrualPeriod {
                    period_start: date!(2026 - 08 - 01),
                    accrual_end: date!(2026 - 09 - 01),
                    payment_date: date!(2026 - 09 - 01),
                    record_date: None,
                    coupon_per_unit: Some(PerUnitAmount::new(dec("31"), CurrencyCode::Rub)),
                }],
                principal_returns: vec![],
                ..Default::default()
            },
        );
        let venue = Venue {
            board: "TQBR".to_owned(),
            session: 3,
        };
        let mut accrued = BTreeMap::new();
        accrued.insert(
            (instrument, venue.clone(), date!(2026 - 08 - 26)),
            PerUnitAmount::new(dec("22.40"), CurrencyCode::Rub),
        );
        let market_prices = [
            match selected_market_position_assessment(
                account,
                instrument,
                Quantity(dec("100")),
                venue,
                date!(2026 - 08 - 26),
            )
            .kind
            {
                PositionAssessmentKind::Selected(selected) => selected.candidate,
                _ => unreachable!(),
            },
        ];
        let report = returns_report(
            &state,
            &ReturnsRequest {
                contour: &contour,
                as_of: date!(2026 - 08 - 26),
                report_currency: CurrencyCode::Rub,
                fx: &fx,
                solver_policy: SolverPolicy::returns_default(),
                coordinate: KnowledgeCoordinate {
                    knowledge_as_of: datetime!(2026 - 08 - 26 12:00:00 UTC),
                    ..KnowledgeCoordinate::default()
                },
                ledger: &ledger,
                perimeter: &perimeter,
                market_prices: &market_prices,
                bond_schedules: &schedules,
                accrued_observations: &accrued,
            },
        );
        assert_ne!(report.data_quality.status, DataQualityStatus::Clean);
        assert!(
            report
                .data_quality
                .material_issues
                .iter()
                .any(|issue| matches!(issue, MaterialIssue::AccruedInterestMismatch { .. }))
        );
    }
    #[test]
    fn termination_value_without_an_executable_exit_is_unknown_not_the_accrual() {
        let value = payable_on_termination(
            &Computed::Value(dec("15.17")),
            SourceExecutability::IndicativePreviousClose,
        );
        assert!(matches!(
            value,
            Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable
            }
        ));
    }

    #[test]
    fn unknown_termination_values_make_only_their_aggregate_unknown() {
        let attributes = vec![BondPositionAttributes {
            account: AccountId::new_random(),
            custody: None,
            instrument: InstrumentId::new_random(),
            accrued_interest: Computed::Value(dec("15.17")),
            accrued_interest_payable_on_termination: Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable,
            },
            next_posting_date: None,
            next_principal_return_finality: None,
        }];
        assert!(matches!(
            aggregate_payable_on_termination(&attributes),
            Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable
            }
        ));
    }

    #[test]
    fn every_refusal_has_a_machine_readable_code() {
        assert_eq!(NotComputable::NoExternalFlows.code(), "no_external_flows");
        assert_eq!(
            NotComputable::SolverRefused {
                refusal: SolverRefusal::NoSignChange
            }
            .code(),
            "solver_refused"
        );
        assert_eq!(
            NotComputable::MissingPrice {
                instrument: crate::ids::InstrumentId::new_random()
            }
            .code(),
            "missing_price"
        );
    }

    #[test]
    fn a_not_computable_value_carries_no_number() {
        // Тип не позволяет прочитать число там, где его нет:
        // «ноль с предупреждением» невозможно построить (§15.2).
        let value: Computed<Dec> = Computed::NotComputable {
            reason: NotComputable::NoExternalFlows,
        };
        assert!(value.value().is_none());
        assert_eq!(
            value.reason().map(NotComputable::code),
            Some("no_external_flows")
        );
    }
    #[test]
    fn a_missing_accrued_observation_has_its_own_reason() {
        let (report, instrument) = report_with_market_basis(QuotationBasis::PercentOfRemainingFace);

        let attributes = report
            .bond_attributes
            .first()
            .expect("облигация должна иметь атрибуты");
        assert!(matches!(
            attributes.accrued_interest_payable_on_termination,
            Computed::NotComputable {
                reason: NotComputable::AccruedObservationMissing { instrument: actual }
            } if actual == instrument
        ));
    }
    fn состояние_с_номиналами(
        state: LedgerState,
        faces: &[&str],
    ) -> LedgerState {
        fn known(face: &str) -> ciborium::Value {
            let principal = crate::rules::lot_disposal::PrincipalState::known(
                PerUnitAmount::new(dec(face), CurrencyCode::Rub),
                PerUnitAmount::new(dec(face), CurrencyCode::Rub),
            )
            .expect("известный номинал");
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&principal, &mut bytes).expect("сериализация номинала");
            ciborium::de::from_reader(bytes.as_slice()).expect("разбор номинала")
        }

        fn replace(value: &mut ciborium::Value, faces: &[&str], next: &mut usize) {
            match value {
                ciborium::Value::Map(entries) => {
                    for (key, value) in entries {
                        if matches!(key, ciborium::Value::Text(text) if text == "principal") {
                            let face = faces
                                .get(*next)
                                .copied()
                                .expect("для каждой партии задан номинал");
                            *value = known(face);
                            *next += 1;
                        } else {
                            replace(value, faces, next);
                        }
                    }
                }
                ciborium::Value::Array(values) => {
                    for value in values {
                        replace(value, faces, next);
                    }
                }
                ciborium::Value::Tag(_, value) => replace(value, faces, next),
                _ => {}
            }
        }

        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&state, &mut bytes).expect("сериализация состояния");
        let mut value: ciborium::Value =
            ciborium::de::from_reader(bytes.as_slice()).expect("разбор состояния");
        let mut next = 0;
        replace(&mut value, faces, &mut next);
        assert_eq!(next, faces.len(), "все партии должны получить номинал");
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).expect("сериализация изменённого состояния");
        ciborium::de::from_reader(bytes.as_slice()).expect("разбор изменённого состояния")
    }

    fn покупка_для_номинала(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        sequence: u32,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let mut event = event_with(
            account,
            day,
            sequence,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: Some(Money::new(PostedMinor::new(100_000), CurrencyCode::Rub)),
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                quantity,
            )],
        );
        // Помощник моделирует источник, сообщающий дату перехода прав.
        event.dates.settled = Some(crate::dates::SettledDate(day));
        event
    }

    fn отчёт_процентной_цены_по_покупкам(
        faces: &[&str],
    ) -> ReturnsReport {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let events: Vec<_> = faces
            .iter()
            .enumerate()
            .map(|(index, _)| {
                покупка_для_номинала(
                    account,
                    instrument,
                    date!(2026 - 08 - 01),
                    u32::try_from(index + 1).expect("номер покупки"),
                )
            })
            .collect();
        let state = состояние_из_события(&contour, &events[0]);
        let state = if events.len() == 1 {
            состояние_с_номиналами(state, faces)
        } else {
            let rules = RuleRegistry::with_defaults();
            let context = ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            };
            let state = project(&events, &context)
                .expect("проекция покупок")
                .snapshot()
                .state()
                .clone();
            состояние_с_номиналами(state, faces)
        };
        let mut candidate = рыночная_цена(instrument, date!(2026 - 08 - 25));
        candidate.price = dec("98.5");
        candidate.basis = QuotationBasis::PercentOfRemainingFace;
        отчёт_с_рыночной_ценой(&state, &contour, candidate)
    }

    #[test]
    fn процентная_цена_в_отчёте_использует_найденный_номинал_лота() {
        let report = отчёт_процентной_цены_по_покупкам(&["1000"]);

        assert_eq!(report.terminal_value, Computed::Value(dec("9850")));
    }

    #[test]
    fn одинаковый_номинал_лотов_остаётся_вычислимым_в_отчёте() {
        let report =
            отчёт_процентной_цены_по_покупкам(&["1000", "1000"]);

        assert_eq!(report.terminal_value, Computed::Value(dec("19700")));
    }

    #[test]
    fn разные_номиналы_лотов_дают_явную_ошибку_в_отчёте() {
        let report =
            отчёт_процентной_цены_по_покупкам(&["1000", "2000"]);

        assert!(matches!(
            report.terminal_value,
            Computed::NotComputable {
                reason: NotComputable::RemainingFaceAmbiguous { .. }
            }
        ));
    }
    fn состояние_из_события(
        contour: &ContourDefinition,
        event: &crate::event::Event,
    ) -> LedgerState {
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        project(std::slice::from_ref(event), &context)
            .expect("проекция тестового события")
            .snapshot()
            .state()
            .clone()
    }

    fn хеш_отчёта(state: &LedgerState, contour: &ContourDefinition) -> String {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(contour, &fx, &ledger, &perimeter);
        returns_report(state, &request).inputs_hash
    }

    fn рыночная_цена(instrument: InstrumentId, trade_date: Date) -> PriceCandidate {
        PriceCandidate {
            instrument,
            price: dec("100"),
            currency: CurrencyCode::Rub,
            basis: QuotationBasis::MoneyPerUnit,
            basis_evidence: "test:market".to_owned(),
            basis_evidence_contradicts: false,
            trade_date,
            observed_at: None,
            origin: PriceOrigin::Market {
                venue: Venue {
                    board: "TQBR".to_owned(),
                    session: 0,
                },
                kind: CorePriceKind::LegalClose,
            },
            executability: SourceExecutability::Executable,
        }
    }

    fn отчёт_с_рыночной_ценой(
        state: &LedgerState,
        contour: &ContourDefinition,
        candidate: PriceCandidate,
    ) -> ReturnsReport {
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        returns_report(state, &request)
    }

    #[test]
    fn поток_после_даты_отчёта_не_попадает_в_публичный_отпечаток() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let event = event_with(
            account,
            date!(2026 - 08 - 26),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let mut future_event = event.clone();
        future_event.dates =
            crate::dates::EventDates::for_cash(crate::dates::CashPostedDate(date!(2026 - 08 - 27)));
        future_event.order = crate::dates::EffectiveOrder::new(date!(2026 - 08 - 27), 1);
        let included = состояние_из_события(&contour, &event);
        let excluded = состояние_из_события(&contour, &future_event);
        let baseline_event = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::OpeningCash { amount },
            vec![Leg::cash(account, amount)],
        );
        let baseline = состояние_из_события(&contour, &baseline_event);

        assert_ne!(
            хеш_отчёта(&included, &contour),
            хеш_отчёта(&baseline, &contour)
        );
        assert_eq!(
            хеш_отчёта(&excluded, &contour),
            хеш_отчёта(&baseline, &contour)
        );
    }

    #[test]
    fn поток_чужого_контура_исключается_из_публичного_отпечатка() {
        let account = AccountId::new_random();
        let id = ContourId::new_random();
        let contour = ContourDefinition::new(id, ContourVersion(1), [account]);
        let foreign = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let event = event_with(
            account,
            date!(2026 - 08 - 26),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let included = состояние_из_события(&contour, &event);
        let excluded = состояние_из_события(&foreign, &event);
        let baseline_event = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::OpeningCash { amount },
            vec![Leg::cash(account, amount)],
        );
        let baseline = состояние_из_события(&contour, &baseline_event);

        assert_ne!(
            хеш_отчёта(&included, &contour),
            хеш_отчёта(&baseline, &contour)
        );
        assert_eq!(
            хеш_отчёта(&excluded, &contour),
            хеш_отчёта(&baseline, &contour)
        );
    }

    #[test]
    fn поток_старой_версии_контура_исключается_из_публичного_отпечатка() {
        let account = AccountId::new_random();
        let id = ContourId::new_random();
        let contour = ContourDefinition::new(id, ContourVersion(2), [account]);
        let foreign = ContourDefinition::new(id, ContourVersion(1), [account]);
        let amount = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
        let event = event_with(
            account,
            date!(2026 - 08 - 26),
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        );
        let included = состояние_из_события(&contour, &event);
        let excluded = состояние_из_события(&foreign, &event);
        let baseline_event = event_with(
            account,
            date!(2026 - 08 - 26),
            2,
            EventKind::OpeningCash { amount },
            vec![Leg::cash(account, amount)],
        );
        let baseline = состояние_из_события(&contour, &baseline_event);

        assert_ne!(
            хеш_отчёта(&included, &contour),
            хеш_отчёта(&baseline, &contour)
        );
        assert_eq!(
            хеш_отчёта(&excluded, &contour),
            хеш_отчёта(&baseline, &contour)
        );
    }

    fn состояние_позиции_с_унаследованной_оценкой(
        contour: &ContourDefinition,
        instrument: InstrumentId,
        account: AccountId,
    ) -> LedgerState {
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity(dec("10")),
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                Quantity(dec("10")),
            )],
        );
        let valuation = event_with(
            account,
            date!(2026 - 08 - 25),
            2,
            EventKind::Valuation {
                instrument,
                price: dec("12"),
                currency: CurrencyCode::Rub,
                quality: PriceQuality::CarriedForward,
            },
            vec![],
        );
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        project(&[opening, valuation], &context)
            .expect("проекция позиции с унаследованной оценкой")
            .snapshot()
            .state()
            .clone()
    }

    #[test]
    fn будущий_рыночный_кандидат_не_отменяет_унаследованную_оценку() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let state =
            состояние_позиции_с_унаследованной_оценкой(
                &contour, instrument, account,
            );
        let report = отчёт_с_рыночной_ценой(
            &state,
            &contour,
            рыночная_цена(instrument, date!(2026 - 08 - 27)),
        );

        assert_eq!(
            report.data_quality.position_coverage.legacy_derived.len(),
            1
        );
        assert!(report.data_quality.position_coverage.uncovered.is_empty());
    }

    #[test]
    fn кандидат_чужого_инструмента_не_отменяет_унаследованную_оценку() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let state =
            состояние_позиции_с_унаследованной_оценкой(
                &contour, instrument, account,
            );
        let report = отчёт_с_рыночной_ценой(
            &state,
            &contour,
            рыночная_цена(InstrumentId::new_random(), date!(2026 - 08 - 25)),
        );

        assert_eq!(
            report.data_quality.position_coverage.legacy_derived.len(),
            1
        );
        assert!(report.data_quality.position_coverage.uncovered.is_empty());
    }

    #[test]
    fn рыночная_цена_чужого_инструмента_не_покрывает_позицию() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity(dec("10")),
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                Quantity(dec("10")),
            )],
        );
        let state = состояние_из_события(&contour, &opening);
        let report = отчёт_с_рыночной_ценой(
            &state,
            &contour,
            рыночная_цена(InstrumentId::new_random(), date!(2026 - 08 - 25)),
        );

        assert!(matches!(
            report.data_quality.position_coverage.uncovered.first(),
            Some(UncoveredPosition {
                reason: UncoveredReason::NoObservation,
                ..
            })
        ));
    }

    #[test]
    fn будущая_рыночная_цена_не_покрывает_позицию() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity(dec("10")),
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                Quantity(dec("10")),
            )],
        );

        let state = состояние_из_события(&contour, &opening);
        let report = отчёт_с_рыночной_ценой(
            &state,
            &contour,
            рыночная_цена(instrument, date!(2026 - 08 - 27)),
        );

        assert!(matches!(
            report.data_quality.position_coverage.uncovered.first(),
            Some(UncoveredPosition {
                reason: UncoveredReason::NoObservation,
                ..
            })
        ));
    }

    #[test]
    fn отчёт_показывает_накопленную_долю_unknown_по_двум_позициям() {
        let mut shares = ExecutabilityAccumulator::default();
        shares.add(SourceExecutability::Executable, dec("2"));
        shares.add(SourceExecutability::IndicativePreviousClose, dec("1"));
        shares.add(SourceExecutability::Unknown, dec("1"));

        let shares = shares.finish();

        assert_eq!(shares.evaluated_positions_value, dec("4"));
        assert_eq!(shares.executable, dec("0.5"));
        assert_eq!(shares.indicative_previous_close, dec("0.25"));
        assert_eq!(shares.unknown, dec("0.25"));
    }
    #[test]
    fn bond_positions_receive_scenario_metrics_and_rule_versions() {
        let schedule = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                period_start: date!(2026 - 08 - 01),
                accrual_end: date!(2026 - 12 - 01),
                payment_date: date!(2026 - 12 - 02),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("5"), CurrencyCode::Rub)),
            }],
            principal_returns: vec![crate::bond::PrincipalReturn {
                repayment_date: date!(2026 - 12 - 02),
                share_percent: dec("100"),
            }],
            initial_principal: Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub)),
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window: crate::bond::OfferWindowId::new_random(),
                right: crate::bond::OfferRight::HolderPut,
                execution_date: date!(2026 - 09 - 15),
                submission_start: None,
                submission_end: None,
                price_percent: Some(dec("100")),
            }],
            completeness: crate::bond::ScheduleCompleteness::Validated,
            default_flags: Some(crate::bond::DefaultFlags {
                declared: false,
                technical: false,
            }),
            currency_roles: Some(crate::instrument::CurrencyRoles::uniform(CurrencyCode::Rub)),
        };
        let (report, _) =
            report_with_market_basis_and_schedule(QuotationBasis::MoneyPerUnit, Some(schedule));

        assert_eq!(report.bond_metrics.len(), 1);
        assert_eq!(report.bond_metrics[0].scenarios.len(), 2);
        assert_ne!(
            report.bond_metrics[0].scenarios[0]
                .prospective
                .terminal_date,
            report.bond_metrics[0].scenarios[1]
                .prospective
                .terminal_date
        );
        let (share_report, _) = report_with_market_basis(QuotationBasis::MoneyPerUnit);
        assert!(share_report.bond_metrics.is_empty());
        assert_eq!(
            report.applied_rules.cashflow_projection,
            crate::rules::CashflowProjectionVersion(2)
        );
        assert_eq!(
            report.applied_rules.expense_policy,
            crate::returns::zero_reinvestment::ExpensePolicyVersion(1)
        );
    }

    #[test]
    fn failed_offer_scenario_keeps_matching_execution_date() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let execution_date = date!(2026 - 09 - 15);
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = position_request(&contour, &fx, &ledger, &perimeter);
        let schedule = BondSchedule {
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window: crate::bond::OfferWindowId::new_random(),
                right: crate::bond::OfferRight::HolderPut,
                execution_date,
                submission_start: None,
                submission_end: None,
                price_percent: Some(dec("100")),
            }],
            ..BondSchedule::default()
        };
        let choice = OfferChoice::ExerciseAtOffer {
            window: schedule.offer_windows[0].window,
        };
        let assessment = position_assessment(account, instrument, Quantity(dec("1")));
        let (_, cashflow) = cashflow_projection_rule();
        let (_, accrued_rule) = accrued_interest_rule();

        let result = bond_scenario(
            &BondScenarioInputs {
                assessment: &assessment,
                request: &request,
                schedule: &schedule,
                lots: None,
                cashflow: &cashflow,
                accrued_rule: &accrued_rule,
            },
            choice.clone(),
        );

        assert_eq!(result.choice, choice);
        assert_eq!(result.prospective.terminal_date, execution_date);
        assert!(matches!(
            &result.prospective.metrics,
            Computed::NotComputable {
                reason: NotComputable::ScheduleMissing { instrument: actual }
            } if *actual == instrument
        ));
    }

    #[test]
    fn zero_quantity_bond_position_is_not_reported() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let mut request = position_request(&contour, &fx, &ledger, &perimeter);
        let schedules = BTreeMap::from([(instrument, BondSchedule::default())]);
        request.bond_schedules = &schedules;
        let positions = vec![PositionValue {
            assessment: position_assessment(account, instrument, Quantity::zero()),
            value: Ok(Dec::zero()),
        }];
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));

        let metrics = bond_position_metrics(&state, &request, &positions, &OfferBook::default());

        assert!(metrics.is_empty());
        // Нулевая позиция не создаёт записи LotKey в пустой книге, поэтому
        // отдельный проход по книге также не находит пару для сверки.
        assert!(historical_reconciliation_issues(&state, &request).is_empty());
    }

    #[test]
    fn unresolved_offer_submission_only_removes_its_instrument_offer_scenarios() {
        let account = AccountId::new_random();
        let first_instrument = InstrumentId::new_random();
        let second_instrument = InstrumentId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let quantity = Quantity(dec("10"));
        let first_opening = event_with(
            account,
            date!(2026 - 08 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: first_instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                first_instrument,
                quantity,
            )],
        );
        let second_opening = event_with(
            account,
            date!(2026 - 08 - 02),
            1,
            EventKind::OpeningPosition {
                instrument: second_instrument,
                quantity,
                cost_basis: None,
                assertions: Default::default(),
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                second_instrument,
                quantity,
            )],
        );
        let state = project(&[first_opening, second_opening], &context)
            .expect("проекция двух позиций")
            .snapshot()
            .state()
            .clone();
        let first_window = crate::bond::OfferWindowId::new_random();
        let second_window = crate::bond::OfferWindowId::new_random();
        let unknown_window = crate::bond::OfferWindowId::new_random();
        let schedule = |window| BondSchedule {
            principal_returns: vec![crate::bond::PrincipalReturn {
                repayment_date: date!(2026 - 12 - 02),
                share_percent: dec("100"),
            }],
            offer_windows: vec![crate::bond::OfferWindowTerms {
                window,
                right: crate::bond::OfferRight::HolderPut,
                execution_date: date!(2026 - 09 - 15),
                submission_start: None,
                submission_end: None,
                price_percent: Some(dec("100")),
            }],
            completeness: crate::bond::ScheduleCompleteness::Validated,
            currency_roles: Some(crate::instrument::CurrencyRoles::uniform(CurrencyCode::Rub)),
            ..Default::default()
        };
        let schedules = BTreeMap::from([
            (first_instrument, schedule(first_window)),
            (second_instrument, schedule(second_window)),
        ]);
        let submission = crate::event::offer::OfferSubmissionId::new_random();
        let mut offer_book = OfferBook::default();
        offer_book
            .apply(&crate::event::offer::OfferExerciseAction::Submitted {
                submission,
                window: unknown_window,
                instrument: first_instrument,
                quantity,
            })
            .expect("подача заявки");
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let request = ReturnsRequest {
            contour: &contour,
            as_of: date!(2026 - 08 - 26),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &schedules,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        let report = returns_report_with_bond_inputs(&state, &request, &offer_book);

        assert!(report.data_quality.material_issues.iter().any(|issue| {
            matches!(
                issue,
                MaterialIssue::OfferWindowUnresolved { submission: id } if *id == submission
            )
        }));
        let first_metrics = report
            .bond_metrics
            .iter()
            .find(|metrics| metrics.instrument == first_instrument)
            .expect("метрики первой бумаги");
        assert_eq!(first_metrics.scenarios.len(), 1);
        assert!(matches!(
            first_metrics.scenarios[0].choice,
            OfferChoice::HoldToMaturity
        ));
        let second_metrics = report
            .bond_metrics
            .iter()
            .find(|metrics| metrics.instrument == second_instrument)
            .expect("метрики второй бумаги");
        assert_eq!(second_metrics.scenarios.len(), 2);
    }
    #[test]
    fn a_record_date_change_changes_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let instrument = InstrumentId::new_random();
        let first = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                period_start: date!(2026 - 01 - 01),
                accrual_end: date!(2026 - 06 - 30),
                payment_date: date!(2026 - 07 - 01),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("5"), CurrencyCode::Rub)),
            }],
            ..Default::default()
        };
        let second = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                record_date: Some(date!(2026 - 06 - 30)),
                ..first.periods[0]
            }],
            ..first.clone()
        };
        let first_hash =
            report_for_schedules(&state, &BTreeMap::from([(instrument, first)])).inputs_hash;
        let second_hash =
            report_for_schedules(&state, &BTreeMap::from([(instrument, second)])).inputs_hash;
        assert_ne!(first_hash, second_hash);
    }

    #[test]
    fn resyncing_unchanged_schedule_keeps_inputs_hash() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let instrument = InstrumentId::new_random();
        let schedule = BondSchedule {
            periods: vec![crate::bond::AccrualPeriod {
                period_start: date!(2026 - 01 - 01),
                accrual_end: date!(2026 - 06 - 30),
                payment_date: date!(2026 - 07 - 01),
                record_date: None,
                coupon_per_unit: Some(PerUnitAmount::new(dec("5"), CurrencyCode::Rub)),
            }],
            ..Default::default()
        };
        let inputs = BTreeMap::from([(instrument, schedule)]);
        let first_hash = report_for_schedules(&state, &inputs).inputs_hash;
        let second_hash = report_for_schedules(&state, &inputs).inputs_hash;
        assert_eq!(first_hash, second_hash);
    }

    /// График облигации с перечисленными купонными датами и одним
    /// погашением.
    ///
    /// Полнота, флаги дефолта и валютные роли заданы явно: без них
    /// правило потока отказывается строить план, `past` не появляется
    /// вовсе, и тест проверял бы отказ построения, а не сверку.
    fn график_купонов(coupons: &[Date], repayment: Date) -> BondSchedule {
        график_с_офертами(coupons, repayment, &[])
    }

    fn график_с_офертами(
        coupons: &[Date],
        repayment: Date,
        offers: &[Date],
    ) -> BondSchedule {
        BondSchedule {
            // Период начинается предыдущей выплатой: замкнутая цепь
            // нужна не сверке, а НКД — без покрытия даты отчёта отказ
            // приходит из расчёта НКД, и тест перестал бы отличать
            // молчание сверки от несостоявшегося сценария.
            periods: coupons
                .iter()
                .enumerate()
                .map(|(index, payment_date)| crate::bond::AccrualPeriod {
                    period_start: if index == 0 {
                        payment_date.saturating_sub(time::Duration::days(180))
                    } else {
                        coupons[index - 1]
                    },
                    accrual_end: *payment_date,
                    payment_date: *payment_date,
                    // Тесты сверки проверяют владение на дату права, поэтому
                    // дата фиксации в этом валидном графике известна.
                    record_date: Some(*payment_date),
                    coupon_per_unit: Some(PerUnitAmount::new(dec("50"), CurrencyCode::Rub)),
                })
                .collect(),
            principal_returns: vec![crate::bond::PrincipalReturn {
                repayment_date: repayment,
                share_percent: dec("100"),
            }],
            initial_principal: Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub)),
            offer_windows: offers
                .iter()
                .map(|execution_date| crate::bond::OfferWindowTerms {
                    window: crate::bond::OfferWindowId::derive(
                        InstrumentId::new_random(),
                        *execution_date,
                    ),
                    right: crate::bond::OfferRight::HolderPut,
                    execution_date: *execution_date,
                    submission_start: None,
                    submission_end: None,
                    price_percent: Some(dec("100")),
                })
                .collect(),
            completeness: crate::bond::ScheduleCompleteness::Validated,
            default_flags: Some(crate::bond::DefaultFlags {
                declared: false,
                technical: false,
            }),
            currency_roles: Some(crate::instrument::CurrencyRoles::uniform(CurrencyCode::Rub)),
        }
    }

    fn пополнение(account: AccountId, day: Date) -> crate::event::Event {
        let amount = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Rub);
        event_with(
            account,
            day,
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        )
    }

    /// Покупка облигации. Дата сделки задаётся отдельным полем, потому
    /// что именно её книга лотов кладёт в `Lot.acquired`, а конверт
    /// `event_with` заполняет только дату зачисления денег. `None`
    /// воспроизводит обычную покупку без даты сделки: схема её
    /// допускает (§4.9), и счёт при этом не становится восстановленным.
    fn покупка_облигации(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        trade: Option<Date>,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let gross = Money::new(PostedMinor::new(100_000), CurrencyCode::Rub);
        let settlement = Money::new(PostedMinor::new(-100_000), CurrencyCode::Rub);
        let mut event = event_with(
            account,
            day,
            2,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Buy,
                instrument,
                quantity,
                gross,
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, settlement),
                Leg::security(account, CustodyId::new_random(), instrument, quantity),
            ],
        );
        event.dates.trade = trade.map(crate::dates::TradeDate);
        // Помощник моделирует источник, сообщающий дату расчётов; при
        // отсутствии даты сделки источник также не сообщает и расчёты.
        event.dates.settled = trade.map(crate::dates::SettledDate);
        event
    }

    /// Восстановленная позиция с заявленной датой сделки пятилетней
    /// давности. Дата зачисления остаётся днём записи, поэтому журнал
    /// начинается позже, чем куплена бумага, — ровно тот случай,
    /// который обязан отличаться от пропущенной выплаты.
    fn восстановленная_позиция(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        trade: Date,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let mut event = event_with(
            account,
            day,
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: Some(Money::new(PostedMinor::new(100_000), CurrencyCode::Rub)),
                assertions: crate::event::kind::OpeningAssertions {
                    acquisition_date: Some(trade),
                    acquisition_date_certainty: crate::event::kind::DateCertainty::Known,
                    ..crate::event::kind::OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                account,
                CustodyId::new_random(),
                instrument,
                quantity,
            )],
        );
        event.dates.trade = Some(crate::dates::TradeDate(trade));
        // Восстановление моделирует источник с известным расчётом в день
        // записи, а историческая дата сделки остаётся отдельным фактом.
        event.dates.settled = Some(crate::dates::SettledDate(day));
        event
    }

    /// Продажа облигации целиком одной партией. Дата сделки задаётся
    /// отдельно от даты зачисления денег по той же причине, что
    /// и у покупки.
    fn продажа_облигации(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        trade: Option<Date>,
        custody: CustodyId,
        sequence: u32,
    ) -> crate::event::Event {
        let quantity = Quantity(dec("10"));
        let gross = Money::new(PostedMinor::new(100_000), CurrencyCode::Rub);
        let mut event = event_with(
            account,
            day,
            sequence,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Sell,
                instrument,
                quantity,
                gross,
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, gross),
                Leg::security(account, custody, instrument, Quantity(dec("-10"))),
            ],
        );
        event.dates.trade = trade.map(crate::dates::TradeDate);
        // Помощник моделирует источник, сообщающий дату расчётов; она
        // совпадает с датой сделки, если та известна.
        event.dates.settled = trade.map(crate::dates::SettledDate);
        event
    }

    /// Покупка в названное место хранения. Отдельным помощником, потому
    /// что `покупка_облигации` выдумывает депозитарий на каждый вызов,
    /// а здесь важно, что бумага легла именно в этот.
    fn покупка_облигации_в_депозитарии(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        custody: CustodyId,
        sequence: u32,
    ) -> crate::event::Event {
        let mut event = покупка_облигации(account, instrument, day, Some(day));
        event.order = crate::dates::EffectiveOrder::new(day, sequence);
        for leg in &mut event.legs {
            if leg.quantity.is_some() {
                leg.custody = Some(custody);
            }
        }
        event
    }

    fn купонный_факт(
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        sequence: u32,
    ) -> crate::event::Event {
        let amount = Money::new(PostedMinor::new(50_000), CurrencyCode::Rub);
        event_with(
            account,
            day,
            sequence,
            EventKind::Income {
                instrument: Some(instrument),
                gross: amount,
                kind: Some(crate::event::kind::IncomeKind::Coupon),
            },
            vec![Leg::cash(account, amount)],
        )
    }

    /// Отчёт по журналу облигации.
    ///
    /// Номинал проставляется лотам отдельно: событие покупки его не
    /// знает, а без него правило потока отказывается строить план.
    /// Позиция получает биржевую цену, потому что непокрытая позиция
    /// сама делает отчёт неполным и скрыла бы вклад сверки в статус.
    fn отчёт_сверки(
        accounts: &[AccountId],
        instrument: InstrumentId,
        events: &[crate::event::Event],
        faces: &[&str],
        schedule: &BondSchedule,
    ) -> ReturnsReport {
        отчёт_сверки_на(
            accounts,
            instrument,
            events,
            faces,
            schedule,
            date!(2026 - 08 - 26),
        )
    }

    /// Тот же отчёт с явной датой отчёта. Отдельным параметром, потому
    /// что отсрочка перед тревогой считается от неё: без вариации
    /// `as_of` границу срока ожидания проверить нечем.
    fn отчёт_сверки_на(
        accounts: &[AccountId],
        instrument: InstrumentId,
        events: &[crate::event::Event],
        faces: &[&str],
        schedule: &BondSchedule,
        as_of: Date,
    ) -> ReturnsReport {
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            accounts.to_vec(),
        );
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let state = project(events, &context)
            .expect("проекция журнала облигации")
            .snapshot()
            .state()
            .clone();
        let state = if faces.is_empty() {
            // Пустой список намеренно оставляет номинал неизвестным:
            // сценарий должен отказаться, а независимая сверка — продолжить.
            state
        } else {
            состояние_с_номиналами(state, faces)
        };
        // Цена наблюдена накануне отчёта: политика отбора цену из
        // будущего не берёт, и позиция осталась бы непокрытой.
        let candidate =
            рыночная_цена(instrument, as_of.saturating_sub(time::Duration::days(1)));
        let fx = FxTable::new(FxSource::OwnerSupplied);
        let ledger = ReconciliationLedger::default();
        let perimeter = PerimeterAssessment::empty(PerimeterPolicy::default());
        let schedules = BTreeMap::from([(instrument, schedule.clone())]);
        let request = ReturnsRequest {
            contour: &contour,
            as_of,
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            coordinate: KnowledgeCoordinate::default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: std::slice::from_ref(&candidate),
            bond_schedules: &schedules,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
        };
        returns_report(&state, &request)
    }

    fn содержит(
        report: &ReturnsReport,
        predicate: impl Fn(&MaterialIssue) -> bool,
    ) -> bool {
        report.data_quality.material_issues.iter().any(predicate)
    }

    fn непринятые(report: &ReturnsReport) -> Vec<&MaterialIssue> {
        report
            .data_quality
            .material_issues
            .iter()
            .filter(|issue| matches!(issue, MaterialIssue::ScheduledPostingNotReceived { .. }))
            .collect()
    }

    /// Журнал с одной пропущенной выплатой: купон 15.03 подтверждён
    /// фактом 16.03, купон 15.06 не подтверждён ничем.
    fn журнал_с_пропущенным_купоном(
        account: AccountId,
        instrument: InstrumentId,
    ) -> Vec<crate::event::Event> {
        vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
            купонный_факт(account, instrument, date!(2026 - 03 - 16), 3),
        ]
    }

    #[test]
    fn a_missing_coupon_is_named_with_its_account_instrument_date_and_kind() {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let report = отчёт_сверки(
            &[account],
            instrument,
            &журнал_с_пропущенным_купоном(account, instrument),
            &["1000"],
            &schedule,
        );

        let issues = непринятые(&report);
        assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
        assert!(matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived {
                account: issue_account,
                instrument: issue_instrument,
                date,
                kind: PostingKind::Coupon,
            } if *issue_account == account
                && *issue_instrument == instrument
                && *date == date!(2026 - 06 - 15)
        ));
    }

    #[test]
    fn a_coupon_before_the_history_horizon_is_unverifiable_even_before_purchase() {
        // Владение до покупки доказано как `NotOwned`, но порядок правил
        // сначала обязан назвать отсутствие покрытия истории.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![
            пополнение(account, date!(2026 - 07 - 01)),
            покупка_облигации(
                account,
                instrument,
                date!(2026 - 07 - 05),
                Some(date!(2026 - 07 - 05)),
            ),
        ];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        // Оба купона раньше первого события счёта: здесь нельзя обвинять
        // эмитента, даже если книга уже знает, что покупка была позже.
        let reasons: Vec<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingUnverifiable {
                    date,
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                    ..
                } => Some((1, *date, *date)),
                MaterialIssue::ScheduledPostingsUnverifiable {
                    first_date,
                    last_date,
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                    count,
                    ..
                } => Some((*count, *first_date, *last_date)),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasons,
            vec![(2, date!(2026 - 03 - 15), date!(2026 - 06 - 15))]
        );
        assert!(непринятые(&report).is_empty());
    }

    #[test]
    fn a_restored_history_reports_that_it_cannot_verify_rather_than_crying_wolf() {
        // Заявленная дата сделки пятилетней давности проводит границу
        // владения до начала журнала: купоны 2021–2025 её проходят,
        // но фактов под них в журнале нет и быть не может.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2021 - 06 - 15),
                date!(2022 - 06 - 15),
                date!(2023 - 06 - 15),
                date!(2024 - 06 - 15),
                date!(2025 - 06 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![восстановленная_позиция(
            account,
            instrument,
            date!(2026 - 01 - 01),
            date!(2021 - 05 - 01),
        )];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        // Июнь 2026 уже покрыт журналом, поэтому его отсутствие теперь
        // проверяем отдельно; пять купонов 2021–2025 названы недоказуемыми.
        assert_eq!(непринятые(&report).len(), 1);
        assert!(содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::HistoryStartsAfterSchedule,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::HistoryStartsAfterSchedule,
                ..
            }
        )));
    }
    #[test]
    fn a_posting_before_the_journal_does_not_silence_a_provable_miss_after_it() {
        // Восстановленная позиция 2021 года, журнал с 01.01.2026,
        // купоны 15.09.2025, 15.12.2025 и 15.03.2026, мартовский не пришёл.
        // Ранние возвраты объявлялись недоказуемыми по одному, а мартовский
        // пропуск не должен исчезнуть вместе со свёрткой.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2025 - 09 - 15),
                date!(2025 - 12 - 15),
                date!(2026 - 03 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![восстановленная_позиция(
            account,
            instrument,
            date!(2026 - 01 - 01),
            date!(2021 - 05 - 01),
        )];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);
        // Две одинаковые недоказуемости должны слиться только после
        // полного обхода, не поглотив отдельное обвинение ниже.
        assert!(
            report
                .data_quality
                .material_issues
                .iter()
                .any(|issue| matches!(
                    issue,
                    MaterialIssue::ScheduledPostingsUnverifiable {
                        reason: UnverifiableReason::HistoryStartsAfterSchedule,
                        count: 2,
                        first_date,
                        last_date,
                        ..
                    } if *first_date == date!(2025 - 09 - 15)
                        && *last_date == date!(2025 - 12 - 15)
                ))
        );

        let даты: Vec<_> = непринятые(&report)
            .into_iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingNotReceived { date, .. } => Some(*date),
                _ => None,
            })
            .collect();
        assert!(
            даты.contains(&date!(2026 - 03 - 15)),
            "пропуск внутри покрытия обязан быть назван: {даты:?}"
        );
        assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
    }

    #[test]
    fn a_posting_on_the_first_journal_day_is_covered_but_settlement_is_unknown() {
        // Граница начала журнала полуоткрыта: день первого события
        // покрыт, поэтому недоказуемость не должна маскироваться
        // причиной `HistoryStartsAfterSchedule`.
        // Но дата права совпадает с точной датой расчётов, а закрытая
        // граница владения намеренно даёт `OwnershipUnknown`.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let первый_день = date!(2026 - 01 - 01);
        let schedule = график_купонов(
            &[первый_день, date!(2026 - 06 - 15)],
            date!(2026 - 12 - 15),
        );
        // Дата сделки заявлена раньше журнала: границу владения купон
        // первого дня проходит, и решает его судьбу именно граница
        // истории, а не отбор по владению.
        let mut restored = восстановленная_позиция(
            account,
            instrument,
            первый_день,
            date!(2025 - 06 - 01),
        );
        // Проверяем именно границу `Exact`: утверждение совпадает с днём
        // открытия журнала, поэтому владение в этот день ещё неоднозначно.
        let EventKind::OpeningPosition { assertions, .. } = &mut restored.kind else {
            unreachable!("помощник создаёт opening_position");
        };
        assertions.acquisition_date = Some(первый_день);
        assertions.acquisition_date_certainty = crate::event::kind::DateCertainty::Known;
        let events = vec![restored];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        assert!(
            !содержит(&report, |issue| matches!(
                issue,
                MaterialIssue::ScheduledPostingUnverifiable {
                    reason: UnverifiableReason::HistoryStartsAfterSchedule,
                    ..
                }
            )),
            "день первого события журналом покрыт: недоказуемости здесь нет"
        );
        assert!(содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                date,
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } if *date == первый_день
        )));
        // Купон июня обвинён по существу: владение на его отсечку
        // доказано датой расчётов восстановленной позиции, а факта нет.
        // Прежнее ожидание тишины досталось от эпохи, когда одна
        // недоказуемая выплата глушила всю пару (iaam-d8b.21).
        let непринятые_даты: Vec<_> = непринятые(&report)
            .into_iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingNotReceived { date, .. } => Some(*date),
                _ => None,
            })
            .collect();
        assert_eq!(непринятые_даты, vec![date!(2026 - 06 - 15)]);
    }

    #[test]
    fn a_purchase_without_a_trade_date_leaves_the_ownership_bound_undrawable() {
        // `Lot.acquired` — `Option<TradeDate>`, и схема отсутствие даты
        // допускает (§4.9). Здесь источник также не сообщает расчёты,
        // поэтому владение на дату права остаётся недоказуемым.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации(account, instrument, date!(2026 - 01 - 10), None),
        ];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        assert!(содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            }
        )));
        assert!(!содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::RestoredWithoutBasis { .. }
        )));
        assert_eq!(report.data_quality.status, DataQualityStatus::Incomplete);
        // Статус неполон именно из-за сверки: других дефектов в отчёте
        // нет, иначе тест проходил бы и без нового варианта.
        let другие: Vec<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter(|issue| {
                issue.is_defect()
                    && !matches!(
                        issue,
                        MaterialIssue::ScheduledPostingUnverifiable { .. }
                            | MaterialIssue::ScheduledPostingsUnverifiable { .. }
                    )
            })
            .collect();
        assert!(другие.is_empty(), "посторонние дефекты: {другие:?}");
    }

    #[test]
    fn an_event_without_settlement_date_reports_unknown_ownership() {
        // Источник, не сообщающий дату перехода прав, делает владение
        // недоказуемым: система обязана признаться, а не угадать дату
        // сделки или выдать обвинение.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
        let mut purchase = покупка_облигации(
            account,
            instrument,
            date!(2026 - 01 - 10),
            Some(date!(2026 - 01 - 10)),
        );
        purchase.dates.settled = None;
        let events = vec![пополнение(account, date!(2026 - 01 - 05)), purchase];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        assert!(непринятые(&report).is_empty());
        assert!(содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            }
        )));
    }

    #[test]
    fn the_history_horizon_is_reported_but_does_not_make_the_answer_incomplete() {
        // Зеркалит `HistoryStartsAt`: факт о периоде, а не дефект.
        assert!(
            !MaterialIssue::ScheduledPostingUnverifiable {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                date: date!(2026 - 06 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::HistoryStartsAfterSchedule,
            }
            .is_defect()
        );
    }

    #[test]
    fn the_other_unverifiable_reasons_are_defects_because_loading_facts_fixes_them() {
        for reason in [
            UnverifiableReason::AcquisitionDateUnknown,
            UnverifiableReason::OwnershipUnknown,
            UnverifiableReason::EntitlementDateUnknown,
            UnverifiableReason::IncomeKindUnknown,
            UnverifiableReason::PaymentDateUnknown,
        ] {
            assert!(
                MaterialIssue::ScheduledPostingUnverifiable {
                    account: AccountId::new_random(),
                    instrument: InstrumentId::new_random(),
                    date: date!(2026 - 06 - 15),
                    kind: PostingKind::Coupon,
                    reason,
                }
                .is_defect(),
                "{reason:?} чинится дозагрузкой фактов и потому дефект"
            );
        }
    }

    #[test]
    fn a_scheduled_posting_not_received_is_a_defect() {
        assert!(
            MaterialIssue::ScheduledPostingNotReceived {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                date: date!(2026 - 06 - 15),
                kind: PostingKind::Coupon,
            }
            .is_defect()
        );
    }

    #[test]
    fn every_unverifiable_reason_has_a_machine_readable_code() {
        let codes: std::collections::BTreeSet<_> = [
            UnverifiableReason::AcquisitionDateUnknown,
            UnverifiableReason::OwnershipUnknown,
            UnverifiableReason::EntitlementDateUnknown,
            UnverifiableReason::IncomeKindUnknown,
            UnverifiableReason::PaymentDateUnknown,
            UnverifiableReason::HistoryStartsAfterSchedule,
            UnverifiableReason::ScheduleNotTrusted,
        ]
        .into_iter()
        .map(UnverifiableReason::code)
        .collect();

        // Семь вариантов должны оставаться разными: каждый дефект
        // чинится своей дозагрузкой и не может сливаться в один код.
        assert_eq!(codes.len(), 7);
    }

    #[test]
    fn an_income_of_unknown_kind_is_reported_before_the_ownership_bound() {
        // Неизвестный вид ломает сторону фактов: сверять нечем при любой
        // границе владения, поэтому причина именно эта, а не отсутствие
        // даты сделки, которого здесь нет.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let amount = Money::new(PostedMinor::new(50_000), CurrencyCode::Rub);
        let events = vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
            event_with(
                account,
                date!(2026 - 03 - 16),
                3,
                EventKind::Income {
                    instrument: Some(instrument),
                    gross: amount,
                    kind: None,
                },
                vec![Leg::cash(account, amount)],
            ),
        ];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        assert!(непринятые(&report).is_empty());
        assert!(содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::IncomeKindUnknown,
                ..
            } | MaterialIssue::ScheduledPostingsUnverifiable {
                reason: UnverifiableReason::IncomeKindUnknown,
                ..
            }
        )));
    }

    #[test]
    fn a_bond_with_several_offer_windows_reports_one_miss_once() {
        // `bond_scenario` вызывается по разу на сценарий, а прошлое у
        // сценариев общее: сверка внутри сценария задвоила бы проблему.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_с_офертами(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
            &[date!(2026 - 09 - 15), date!(2026 - 10 - 15)],
        );
        let report = отчёт_сверки(
            &[account],
            instrument,
            &журнал_с_пропущенным_купоном(account, instrument),
            &["1000"],
            &schedule,
        );

        assert_eq!(
            report.bond_metrics[0].scenarios.len(),
            3,
            "сценариев должно быть больше одного, иначе тест ничего не доказывает"
        );
        assert_eq!(непринятые(&report).len(), 1);
    }

    /// Журнал здоровой бумаги: две покупки и продажа ранней партии,
    /// все прошедшие купоны подтверждены фактами.
    fn журнал_с_проданной_ранней_партией(
        account: AccountId,
        instrument: InstrumentId,
        факты: &[Date],
    ) -> Vec<crate::event::Event> {
        // Одно место хранения на весь журнал: иначе продажа списала бы
        // бумагу из депозитария, в который её не клали, и позиций стало
        // бы три — тест проверял бы задвоение, а не границу владения.
        let custody = CustodyId::new_random();
        let mut events = vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации_в_депозитарии(
                account,
                instrument,
                date!(2026 - 01 - 10),
                custody,
                2,
            ),
            покупка_облигации_в_депозитарии(
                account,
                instrument,
                date!(2026 - 04 - 10),
                custody,
                3,
            ),
            продажа_облигации(
                account,
                instrument,
                date!(2026 - 07 - 10),
                Some(date!(2026 - 07 - 10)),
                custody,
                4,
            ),
        ];
        events.extend(факты.iter().enumerate().map(|(index, day)| {
            купонный_факт(
                account,
                instrument,
                *day,
                5 + u32::try_from(index).expect("номер факта"),
            )
        }));
        events
    }

    #[test]
    fn a_coupon_whose_waiting_window_is_still_running_is_not_reported_as_missing() {
        // Деньги идут по депозитарной цепочке до трёх недель, и всё это
        // время отсутствие факта ничего не доказывает. Граница: +20
        // дней — молчание, +21 — тревога.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = журнал_с_пропущенным_купоном(account, instrument);

        let ещё_идёт = отчёт_сверки_на(
            &[account],
            instrument,
            &events,
            &["1000"],
            &schedule,
            date!(2026 - 07 - 05),
        );
        // Поток построен, значит сверка до плана дошла и промолчала
        // по существу, а не из-за отказа построения.
        assert!(matches!(
            ещё_идёт.bond_metrics[0].scenarios[0].prospective.metrics,
            Computed::Value(_)
        ));
        assert!(непринятые(&ещё_идёт).is_empty());
        assert!(!содержит(&ещё_идёт, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));

        let истёк = отчёт_сверки_на(
            &[account],
            instrument,
            &events,
            &["1000"],
            &schedule,
            date!(2026 - 07 - 06),
        );
        assert_eq!(
            непринятые(&истёк).len(),
            1,
            "проблемы: {:?}",
            непринятые(&истёк)
        );
    }

    #[test]
    fn a_payment_whose_waiting_window_is_still_running_is_not_a_reason_to_call_the_report_unverifiable()
     {
        // Выплата, срок которой ещё идёт, исключается из проверки
        // целиком: она не повод ни для тревоги, ни для причины
        // недоказуемости. Здесь её дата раньше первого события журнала
        // — прежде это давало `HistoryStartsAfterSchedule`.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[date!(2026 - 08 - 20), date!(2026 - 12 - 15)],
            date!(2026 - 12 - 15),
        );
        let events = vec![восстановленная_позиция(
            account,
            instrument,
            date!(2026 - 08 - 22),
            date!(2021 - 05 - 01),
        )];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        assert!(непринятые(&report).is_empty());
        assert!(!содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));
    }

    #[test]
    fn a_fully_sold_bond_is_still_reconciled_from_the_lot_book() {
        // Проданная в ноль бумага исчезает из позиций, но запись LotKey
        // остаётся: мартовский купон за период владения нельзя потерять
        // вместе с текущим количеством.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let custody = CustodyId::new_random();
        let schedule = график_купонов(&[date!(2026 - 03 - 15)], date!(2026 - 12 - 15));
        let events = vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации_в_депозитарии(
                account,
                instrument,
                date!(2026 - 01 - 10),
                custody,
                2,
            ),
            продажа_облигации(
                account,
                instrument,
                date!(2026 - 05 - 10),
                Some(date!(2026 - 05 - 10)),
                custody,
                3,
            ),
        ];
        let report = отчёт_сверки(&[account], instrument, &events, &[], &schedule);

        assert!(report.bond_metrics.is_empty());
        let issues = непринятые(&report);
        assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
        assert!(matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        ));
    }

    #[test]
    fn reconciliation_survives_an_unknown_nominal_when_scenario_cannot_be_built() {
        // Номинал нужен сценарию для денег, но не датам сверки: отказ
        // сценария не должен выключать проверку пропущенного купона.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        schedule.initial_principal = None;
        let events = vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
        ];
        let report = отчёт_сверки(&[account], instrument, &events, &[], &schedule);

        assert!(matches!(
            report.bond_metrics[0].scenarios[0].prospective.metrics,
            Computed::NotComputable {
                reason: NotComputable::PrincipalUnknown
            }
        ));
        assert!(непринятые(&report).iter().any(|issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        )));
    }

    #[test]
    fn an_untrusted_schedule_reports_one_pair_level_refusal() {
        // Неполному графику нельзя приписывать ни пропуск, ни отсутствие
        // права: владелец должен увидеть отдельную причину доверия к графику.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let mut schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        schedule.completeness = crate::bond::ScheduleCompleteness::Unknown;
        let events = vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации(
                account,
                instrument,
                date!(2026 - 01 - 10),
                Some(date!(2026 - 01 - 10)),
            ),
        ];
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        let refusals: Vec<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter(|issue| {
                matches!(
                    issue,
                    MaterialIssue::ScheduledPostingUnverifiable {
                        reason: UnverifiableReason::ScheduleNotTrusted,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(refusals.len(), 1, "проблемы: {refusals:?}");
        assert!(непринятые(&report).is_empty());
    }

    #[test]
    fn a_missing_coupon_is_still_named_after_the_earliest_lot_was_sold() {
        // Граница владения — самая ранняя дата приобретения, когда-либо
        // наблюдённая по паре. Иначе продажа январской партии подняла бы
        // границу до апреля и спрятала мартовский пропуск.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = журнал_с_проданной_ранней_партией(
            account,
            instrument,
            &[date!(2026 - 06 - 16)],
        );
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        let issues = непринятые(&report);
        assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
        assert!(matches!(
            issues[0],
            MaterialIssue::ScheduledPostingNotReceived { date, .. }
                if *date == date!(2026 - 03 - 15)
        ));
    }

    #[test]
    fn a_healthy_history_with_a_sold_early_lot_raises_no_alarm() {
        // Обратная сторона той же границы: полная история полученных
        // купонов не даёт ни одной тревоги.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = журнал_с_проданной_ранней_партией(
            account,
            instrument,
            &[date!(2026 - 03 - 16), date!(2026 - 06 - 16)],
        );
        let report = отчёт_сверки(&[account], instrument, &events, &["1000"], &schedule);

        assert!(непринятые(&report).is_empty());
        assert!(!содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));
    }

    #[test]
    fn a_bond_kept_in_two_custodies_reports_one_miss_once() {
        // Позиция обходится по `PositionKey` с местом хранения,
        // а сверяется `LotKey` без него: одна и та же проблема иначе
        // выдаётся по разу на депозитарий.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = журнал_бумаги_в_двух_депозитариях(
            account,
            instrument,
            &[date!(2026 - 03 - 16)],
        );
        let report = отчёт_сверки(
            &[account],
            instrument,
            &events,
            &["1000", "1000"],
            &schedule,
        );

        assert_eq!(
            report.bond_metrics.len(),
            2,
            "позиций должно быть две, иначе тест ничего не доказывает"
        );
        let issues = непринятые(&report);
        assert_eq!(issues.len(), 1, "проблемы: {issues:?}");
    }

    #[test]
    fn moving_a_bond_between_custodies_raises_no_false_alarm() {
        // Отдельного вида события для перевода бумаги между
        // депозитариями в модели нет: перевод виден только состоянием —
        // один `LotKey` под двумя `PositionKey`. Оно и проверяется:
        // полная история купонов не даёт ни одной тревоги.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let events = журнал_бумаги_в_двух_депозитариях(
            account,
            instrument,
            &[date!(2026 - 03 - 16), date!(2026 - 06 - 16)],
        );
        let report = отчёт_сверки(
            &[account],
            instrument,
            &events,
            &["1000", "1000"],
            &schedule,
        );

        assert_eq!(report.bond_metrics.len(), 2);
        assert!(непринятые(&report).is_empty());
        assert!(!содержит(&report, |issue| matches!(
            issue,
            MaterialIssue::ScheduledPostingUnverifiable { .. }
        )));
    }

    /// Одна бумага на одном счёте в двух местах хранения.
    fn журнал_бумаги_в_двух_депозитариях(
        account: AccountId,
        instrument: InstrumentId,
        факты: &[Date],
    ) -> Vec<crate::event::Event> {
        let mut events = vec![
            пополнение(account, date!(2026 - 01 - 05)),
            покупка_облигации_в_депозитарии(
                account,
                instrument,
                date!(2026 - 01 - 10),
                CustodyId::new_random(),
                2,
            ),
            покупка_облигации_в_депозитарии(
                account,
                instrument,
                date!(2026 - 01 - 11),
                CustodyId::new_random(),
                3,
            ),
        ];
        events.extend(факты.iter().enumerate().map(|(index, day)| {
            купонный_факт(
                account,
                instrument,
                *day,
                4 + u32::try_from(index).expect("номер факта"),
            )
        }));
        events
    }

    #[test]
    fn two_accounts_holding_the_same_bond_give_two_distinguishable_issues() {
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let schedule = график_купонов(
            &[
                date!(2026 - 03 - 15),
                date!(2026 - 06 - 15),
                date!(2026 - 12 - 15),
            ],
            date!(2026 - 12 - 15),
        );
        let mut events = журнал_с_пропущенным_купоном(first, instrument);
        events.extend(журнал_с_пропущенным_купоном(
            second, instrument,
        ));
        let report = отчёт_сверки(
            &[first, second],
            instrument,
            &events,
            &["1000", "1000"],
            &schedule,
        );

        let accounts: std::collections::BTreeSet<_> = report
            .data_quality
            .material_issues
            .iter()
            .filter_map(|issue| match issue {
                MaterialIssue::ScheduledPostingNotReceived { account, .. } => Some(*account),
                _ => None,
            })
            .collect();
        assert_eq!(accounts, std::collections::BTreeSet::from([first, second]));
    }

    #[test]
    fn the_report_names_the_applied_rule_versions() {
        let state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        let report = report_for(&state, KnowledgeCoordinate::default());

        assert_eq!(
            report.applied_rules.cashflow_projection,
            crate::rules::CashflowProjectionVersion(2)
        );
        assert_eq!(
            report.applied_rules.posting_match,
            crate::rules::PostingMatchVersion(2)
        );
    }

    #[test]
    fn the_posting_match_version_reaches_the_inputs_hash() {
        // Версия правила — вход отчёта: её смена обязана менять
        // отпечаток, иначе два несравнимых ответа выглядели бы
        // воспроизведением одного.
        let contour = ContourDefinition::new(
            ContourId(uuid::Uuid::nil()),
            ContourVersion(1),
            Vec::<AccountId>::new(),
        );
        let fx_source = FxSource::OwnerSupplied;
        let offer_book = OfferBook::default();
        let inputs = |posting_match| SelectedInputs {
            coordinate: SelectedCoordinate {
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
                source_priority_version: 1,
                valuation_policy_version: 1,
            },
            as_of: date!(2026 - 08 - 26),
            contour: &contour,
            report_currency: CurrencyCode::Rub,
            flows: Vec::new(),
            cash: Vec::new(),
            positions: Vec::new(),
            fx: SelectedFx {
                source: &fx_source,
                rates: Vec::new(),
            },
            bond_schedules: &EMPTY_BOND_SCHEDULES,
            accrued_observations: &EMPTY_ACCRUED_OBSERVATIONS,
            accrued_interest_rule: accrued_interest_rule().0,
            offer_book: &offer_book,
            cashflow_projection: cashflow_projection_rule().0,
            expense_policy: expense_policy_rule().0,
            posting_match,
        };

        assert_ne!(
            hash_selected(&inputs(crate::rules::PostingMatchVersion(1))),
            hash_selected(&inputs(crate::rules::PostingMatchVersion(2)))
        );
    }
    #[test]
    fn five_unverifiable_postings_with_one_reason_become_one_issue() {
        // Одна причина уровня источника чинится одним действием, поэтому
        // пять адресных строк должны сообщить одну проблему с полным
        // количеством и периодом, а не пять раз повторить одно указание.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let issues = (0..5)
            .map(|offset| MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 01 - 15)
                    .saturating_add(time::Duration::days(i64::from(offset) * 30)),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::EntitlementDateUnknown,
            })
            .collect();

        let collapsed = collapse_scheduled_posting_unverifiable(issues);

        assert!(matches!(
            collapsed.as_slice(),
            [MaterialIssue::ScheduledPostingsUnverifiable {
                count: 5,
                first_date,
                last_date,
                ..
            }] if *first_date == date!(2026 - 01 - 15)
                && *last_date == date!(2026 - 05 - 15)
        ));
    }

    #[test]
    fn one_unverifiable_posting_stays_an_addressed_issue() {
        // Свёртка нужна только для повторов: одна выплата должна сохранить
        // старую адресную форму, чтобы не создавать ложную семантику группы.
        let issue = MaterialIssue::ScheduledPostingUnverifiable {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
            date: date!(2026 - 01 - 15),
            kind: PostingKind::Coupon,
            reason: UnverifiableReason::OwnershipUnknown,
        };

        let collapsed = collapse_scheduled_posting_unverifiable(vec![issue.clone()]);

        assert_eq!(collapsed, vec![issue]);
    }

    #[test]
    fn collapsing_unverifiable_postings_keeps_an_interleaved_missing_posting() {
        // Обвинение в конкретном пропуске чинится поиском этого платежа,
        // поэтому оно обязано пережить глобальную свёртку соседних причин.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let issues = vec![
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 01 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::PaymentDateUnknown,
            },
            MaterialIssue::ScheduledPostingNotReceived {
                account,
                instrument,
                date: date!(2026 - 03 - 15),
                kind: PostingKind::Coupon,
            },
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 05 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::PaymentDateUnknown,
            },
        ];

        let collapsed = collapse_scheduled_posting_unverifiable(issues);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            collapsed[0],
            MaterialIssue::ScheduledPostingsUnverifiable {
                count: 2,
                first_date,
                last_date,
                ..
            } if first_date == date!(2026 - 01 - 15)
                && last_date == date!(2026 - 05 - 15)
        ));
        assert!(matches!(
            collapsed[1],
            MaterialIssue::ScheduledPostingNotReceived {
                date,
                kind: PostingKind::Coupon,
                ..
            } if date == date!(2026 - 03 - 15)
        ));
    }

    #[test]
    fn different_unverifiable_reasons_do_not_merge() {
        // Разные причины требуют разных исправляющих действий, поэтому
        // одинаковая пара и вид выплаты не дают права потерять различие.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let issues = vec![
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 01 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::OwnershipUnknown,
            },
            MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 05 - 15),
                kind: PostingKind::Coupon,
                reason: UnverifiableReason::PaymentDateUnknown,
            },
        ];

        let collapsed = collapse_scheduled_posting_unverifiable(issues);

        assert_eq!(collapsed.len(), 2);
        assert!(matches!(
            collapsed[0],
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::OwnershipUnknown,
                ..
            }
        ));
        assert!(matches!(
            collapsed[1],
            MaterialIssue::ScheduledPostingUnverifiable {
                reason: UnverifiableReason::PaymentDateUnknown,
                ..
            }
        ));
    }
}
