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
use time::Date;

use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::ids::{AccountId, InstrumentId};
use crate::money::CurrencyCode;
use crate::numeric::approx::SolverPolicy;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverRefusal};
use crate::projection::state::LedgerState;
use crate::rules::lot_disposal::RuleId;
use crate::valuation::{FxSource, FxTable, PriceQuality, ValuationError};

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
    /// Цена устарела или является оценкой владельца.
    PriceNotExecutable {
        instrument: InstrumentId,
        quality: PriceQuality,
    },
    /// Отрицательный денежный остаток — обязательство в NAV (§15.9).
    NegativeCash {
        account: AccountId,
        currency: CurrencyCode,
    },
    /// Данных до этой даты нет; всё, что раньше, в расчёт не вошло.
    HistoryStartsAt { date: Date },
}

/// Блок качества данных.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataQuality {
    pub status: DataQualityStatus,
    /// Доля данных без независимого подтверждения.
    ///
    /// На этапе 1 равна единице **по определению, а не по подсчёту**:
    /// сверки не существует, подтверждать нечем. Считать её по полю
    /// `Confidence` было бы подменой: `Confidence` описывает уверенность
    /// в значении (§4.9), а не факт сверки (§10.3).
    pub unconfirmed_share: Dec,
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
}

/// Запрос отчёта.
#[derive(Debug, Clone, Copy)]
pub struct ReturnsRequest<'a> {
    pub contour: &'a ContourDefinition,
    pub as_of: Date,
    pub report_currency: CurrencyCode,
    pub fx: &'a FxTable,
    pub solver_policy: SolverPolicy,
}

/// Ответ на три вопроса этапа 1.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnsReport {
    pub as_of: Date,
    pub history_starts: Option<Date>,
    pub report_currency: CurrencyCode,
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
        },
        data_quality: data_quality(state),
    }
}

/// Блок качества данных строится из состояния, а не из желания
/// показать зелёный статус: на этапе 1 подтверждать нечем, поэтому
/// `Clean` недостижим, и это записано прямо здесь.
fn data_quality(state: &LedgerState) -> DataQuality {
    let mut issues = Vec::new();
    for account in state.coverage().restored_accounts() {
        issues.push(MaterialIssue::RestoredWithoutBasis { account: *account });
    }
    for (instrument, price) in state.prices().iter() {
        if !price.quality.is_complete() {
            issues.push(MaterialIssue::PriceNotExecutable {
                instrument: *instrument,
                quality: price.quality,
            });
        }
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
    // Начало истории сообщается всегда, но неполнотой не является:
    // «данных до 01.03.2024 нет» — это факт о периоде, а не дефект.
    // Статусом управляют остальные проблемы.
    let material = issues
        .iter()
        .any(|issue| !matches!(issue, MaterialIssue::HistoryStartsAt { .. }));
    DataQuality {
        // `Clean` на этапе 1 недостижим и не выставляется: подтверждать
        // данные нечем, пока нет сверки (E2).
        status: if material {
            DataQualityStatus::Incomplete
        } else {
            DataQualityStatus::Mixed
        },
        // Этап 1: независимого подтверждения нет ни у одного события,
        // потому что механизма подтверждения ещё не существует (E2).
        unconfirmed_share: Dec::one(),
        material_issues: issues,
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
    use crate::projection::{ProjectionContext, project};
    use crate::rules::{LotRuleVersion, RuleRegistry};
    use crate::valuation::PriceQuality;
    use time::macros::date;

    #[test]
    fn every_data_quality_status_has_a_machine_readable_code() {
        // Внешний агент разбирает код, а не текст. Пустая строка вместо
        // кода неотличима от «статуса нет».
        assert_eq!(DataQualityStatus::Clean.code(), "clean");
        assert_eq!(DataQualityStatus::Mixed.code(), "mixed");
        assert_eq!(DataQualityStatus::Incomplete.code(), "incomplete");
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
        data_quality(projection.snapshot().state())
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
        assert!(
            !quality
                .material_issues
                .iter()
                .any(|issue| matches!(issue, MaterialIssue::PriceNotExecutable { .. })),
            "исполнимая цена проблемой не является"
        );
    }

    #[test]
    fn a_price_that_is_not_executable_makes_the_report_incomplete() {
        // Оценка владельца — не рыночная цена. Стоимость позиции по ней
        // посчитать можно, но выдавать её как подтверждённую нельзя.
        let quality = quality_of(PriceQuality::OwnerEstimate);
        assert_eq!(quality.status, DataQualityStatus::Incomplete);
        assert!(
            quality
                .material_issues
                .iter()
                .any(|issue| matches!(issue, MaterialIssue::PriceNotExecutable { .. }))
        );
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
