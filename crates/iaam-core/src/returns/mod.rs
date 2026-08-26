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

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Date, OffsetDateTime, UtcOffset};

use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::ids::{AccountId, InstrumentId, SourceId};
use crate::money::CurrencyCode;
use crate::numeric::approx::SolverPolicy;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverRefusal};
use crate::perimeter::{PerimeterAssessment, PerimeterPolicy};
use crate::projection::state::LedgerState;
use crate::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use crate::rules::lot_disposal::RuleId;
use crate::rules::{SourcePriorityVersion, ValuationPolicyV1, ValuationRule};
use crate::valuation::{
    FxSource, FxTable, LegacyValuationOutcome, PriceCandidate, PriceQuality, PriceQuery,
    SelectedPrice, SourceExecutability, UncoveredReason, ValuationError,
    candidate_from_legacy_valuation,
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
    /// Решатель отказался: корня нет, корней несколько, не сошлось.
    SolverRefused { refusal: SolverRefusal },
    /// Ни одного потока, пересекающего границу контура.
    NoExternalFlows,
    /// Срез журнала содержит события позже даты отчёта: он собран неверно.
    StateNewerThanReport { last_event: Date, as_of: Date },
    /// Арифметическая невозможность: переполнение, деление на ноль.
    Numeric { code: &'static str },
    /// На счёте финансирование вне периметра: экономику система
    /// не достраивает (§11).
    UnsupportedFinancing { account: AccountId },
}

impl NotComputable {
    /// Машиночитаемый код для API (§13). Внешний агент разбирает код,
    /// а не текст: текст предназначен человеку.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingPrice { .. } => "missing_price",
            Self::MissingFxRate { .. } => "missing_fx_rate",
            Self::SolverRefused { .. } => "solver_refused",
            Self::NoExternalFlows => "no_external_flows",
            Self::StateNewerThanReport { .. } => "state_newer_than_report",
            Self::Numeric { .. } => "numeric",
            Self::UnsupportedFinancing { .. } => "unsupported_financing",
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
}

impl MaterialIssue {
    /// Делает ли проблема ответ **неполным**.
    ///
    /// Две проблемы неполнотой не являются и потому не переводят статус
    /// в `Incomplete`:
    ///
    /// - начало истории — это факт о периоде, а не дефект (§10.7);
    /// - отсутствие независимого источника — нормальное состояние
    ///   данных: §10.5 прямо требует считать такие записи в отчётах
    ///   по умолчанию, иначе система бесполезна именно для банков без
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
            Self::RestoredWithoutBasis { .. }
            | Self::NegativeCash { .. }
            | Self::Discrepancy { .. }
            | Self::UnsupportedFinancing { .. } => true,
        }
    }
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

/// Покрытие ценой: только количество позиций, без выдуманного денежного
/// знаменателя для позиций, которым цена не найдена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionCoverage {
    pub evaluated_positions: u32,
    pub total_positions: u32,
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
    pub solver_policy: SolverPolicy,
    /// Порог, по которому классифицирован отрицательный остаток (§11).
    /// Цифра, зависящая от порога, обязана нести порог рядом с собой.
    pub perimeter_policy: PerimeterPolicy,
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
    /// Внутренняя норма доходности **до налога**.
    pub xirr: Computed<RateOutcome>,
    pub applied_rules: AppliedRules,
    pub data_quality: DataQuality,
}

impl ReturnsReport {
    /// Ярлык результата. Существует, чтобы никакой потребитель API
    /// не назвал эту величину «доходностью» без оговорки (§16.3).
    pub const XIRR_LABEL: &'static str = "xirr_pre_tax";
}

#[derive(Serialize)]
struct SelectedPosition {
    account: AccountId,
    custody: Option<crate::ids::CustodyId>,
    instrument: InstrumentId,
    quantity: crate::money::Quantity,
    price: Option<crate::valuation::InstrumentPrice>,
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
}

fn inputs_hash(state: &LedgerState, request: &ReturnsRequest<'_>) -> String {
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

    let positions: Vec<_> = state
        .balances()
        .iter_positions()
        .filter(|(key, quantity)| request.contour.contains(key.account) && !quantity.0.is_zero())
        .map(|(key, quantity)| SelectedPosition {
            account: key.account,
            custody: key.custody,
            instrument: key.instrument,
            quantity,
            price: state
                .prices()
                .price_at_or_before(key.instrument, request.as_of)
                .copied(),
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
        if let Some(price) = position.price {
            fx_keys.insert((price.currency, request.as_of));
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
    };
    let mut encoded = Vec::new();
    ciborium::into_writer(&selected, &mut encoded)
        .unwrap_or_else(|error| panic!("входы отчёта не сериализуются: {error}"));

    let mut hasher = Sha256::new();
    hasher.update(b"iaam/returns-inputs/v1");
    hasher.update(encoded);
    let digest = hasher.finalize();
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        result.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        result.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    result
}

/// Расчёт отчёта.
///
/// Ядро не ходит за данными: цены и курсы приходят готовыми, границы
/// контура заданы явно. Всё, чего не хватает, превращается в отказ
/// с указанием причины, а не в подставленное значение.
#[must_use]
pub fn returns_report(state: &LedgerState, request: &ReturnsRequest) -> ReturnsReport {
    let series = xirr::flow_series(state, request);
    let terminal = xirr::terminal_value(state, request);
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

    ReturnsReport {
        as_of: request.as_of,
        history_starts: state.coverage().first_event(),
        report_currency: request.report_currency,
        coordinate: request.coordinate,
        inputs_hash: inputs_hash(state, request),
        contributed,
        withdrawn,
        terminal_value,
        xirr: rate,
        applied_rules: AppliedRules {
            contour: request.contour.id(),
            contour_version: request.contour.version(),
            lot_rule: state.book().applied_rule().cloned(),
            fx_source: request.fx.source().clone(),
            day_count: DayCount::Act365,
            solver_policy: request.solver_policy,
            perimeter_policy: request.perimeter.policy(),
        },
        data_quality: data_quality(state, request),
    }
}

#[derive(Debug)]
enum PositionAssessmentKind {
    Selected(SelectedPrice),
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
                    trade_date: price.as_of,
                    observed_at: request.coordinate.knowledge_as_of,
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
                    Some(selected) => PositionAssessmentKind::Selected(selected.clone()),
                    None => PositionAssessmentKind::Uncovered(
                        result
                            .uncovered_reason()
                            .unwrap_or(UncoveredReason::NoObservation),
                    ),
                }
            };
            PositionAssessment {
                account: key.account,
                custody: key.custody,
                instrument: key.instrument,
                quantity,
                raw_price,
                kind,
            }
        })
        .collect()
}

fn position_value(
    assessment: &PositionAssessment,
    price: Dec,
    currency: CurrencyCode,
    request: &ReturnsRequest<'_>,
) -> Result<Dec, NotComputable> {
    let local = assessment
        .quantity
        .0
        .checked_mul(price)
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
fn data_quality(state: &LedgerState, request: &ReturnsRequest) -> DataQuality {
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

    let assessments = position_assessments(state, request);
    let mut position_coverage = PositionCoverage {
        evaluated_positions: 0,
        total_positions: assessments.len() as u32,
        uncovered: Vec::new(),
        legacy_derived: Vec::new(),
    };
    let mut executability = ExecutabilityAccumulator::default();
    for assessment in &assessments {
        match &assessment.kind {
            PositionAssessmentKind::Selected(selected) => {
                position_coverage.evaluated_positions += 1;
                if let Ok(value) = position_value(
                    assessment,
                    selected.candidate.price,
                    selected.candidate.currency,
                    request,
                ) {
                    executability.add(selected.candidate.executability, value);
                }
            }
            PositionAssessmentKind::LegacyDerived(quality) => {
                position_coverage.evaluated_positions += 1;
                position_coverage.legacy_derived.push(LegacyDerivedPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    quality: *quality,
                });
                if let Some(price) = assessment.raw_price {
                    if let Ok(value) =
                        position_value(assessment, price.price, price.currency, request)
                    {
                        executability.add(SourceExecutability::Unknown, value);
                    }
                }
            }
            PositionAssessmentKind::Uncovered(reason) => {
                position_coverage.uncovered.push(UncoveredPosition {
                    account: assessment.account,
                    custody: assessment.custody,
                    instrument: assessment.instrument,
                    reason: *reason,
                });
            }
        }
    }

    // Стоимость по счетам может не посчитаться — например, без цены.
    // Тогда взвешивать покрытие нечем, и оно честно остаётся
    // неизвестным, а не выдаётся за полное.
    let values = xirr::account_values(state, request).unwrap_or_default();
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
        let unknown = rust_decimal::Decimal::ONE - executable - indicative_previous_close;
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

    use crate::contour::{ContourId, ContourVersion};
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, InstrumentId};
    use crate::money::{Money, PostedMinor};
    use crate::projection::lots::LotBook;
    use crate::projection::{ProjectionContext, project};
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use crate::valuation::PriceQuality;
    use time::macros::{date, datetime};

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
        };
        returns_report(state, &request)
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
            perimeter: &perimeter,
        };
        data_quality(projection.snapshot().state(), &request)
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
        shares.add(SourceExecutability::Executable, Dec::new(rust_decimal::Decimal::new(2, 0)));
        shares.add(
            SourceExecutability::IndicativePreviousClose,
            Dec::new(rust_decimal::Decimal::new(1, 0)),
        );
        shares.add(SourceExecutability::Unknown, Dec::new(rust_decimal::Decimal::new(1, 0)));

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
}
