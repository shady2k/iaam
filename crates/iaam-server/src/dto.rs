//! Транспортные представления (§3.2).
//!
//! DTO живут здесь и никогда не переезжают в общий крейт: общий крейт
//! типов быстро превращается в свалку, и формально независимое ядро
//! оказывается зависимым от слоя, который знает обо всём.
//!
//! **Суммы передаются десятичными строками**, а не числами с плавающей
//! точкой: JSON-число `0.1` в двоичной плавающей точке не равно одной
//! десятой, и денежная сумма, прошедшая через него, перестаёт быть фактом.

use std::fmt;

use iaam_app::ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use iaam_app::ingest::{Rejection, Verdict};
use iaam_app::ports::{
    BrokerAccessView, BrokerEnvironment, ClassificationRuleView, IssuedToken, Scope, TokenView,
};
use iaam_core::event::kind::FeeOrigin;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::returns::{Computed, DataQuality, MaterialIssue, NotComputable, ReturnsReport};
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Date;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

// Собственный формат дат: штатная сериализация `time::Date` не является
// строкой «ГГГГ-ММ-ДД», и без этой строки API принимал бы даты
// в непредсказуемом виде. Проверено исполнением: без неё разбор тела
// падает с «invalid type: string "2025-01-01", expected a `Date`».
time::serde::format_description!(iso_date, Date, "[year]-[month]-[day]");

/// Код валюты в транспорте. Отдельный тип, потому что `CurrencyCode`
/// ядра не знает про OpenAPI и знать не должен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum CurrencyDto {
    Rub,
    Usd,
    Eur,
    Cny,
    Xau,
}

impl CurrencyDto {
    #[must_use]
    pub const fn to_domain(self) -> CurrencyCode {
        match self {
            Self::Rub => CurrencyCode::Rub,
            Self::Usd => CurrencyCode::Usd,
            Self::Eur => CurrencyCode::Eur,
            Self::Cny => CurrencyCode::Cny,
            Self::Xau => CurrencyCode::Xau,
        }
    }

    #[must_use]
    pub const fn from_domain(currency: CurrencyCode) -> Self {
        match currency {
            CurrencyCode::Rub => Self::Rub,
            CurrencyCode::Usd => Self::Usd,
            CurrencyCode::Eur => Self::Eur,
            CurrencyCode::Cny => Self::Cny,
            CurrencyCode::Xau => Self::Xau,
        }
    }
}

/// Качество цены в транспорте.
///
/// Уже вычисленные нами величины — перенос на нерабочий день и
/// устаревание по порогу — представимым вводом не являются: это выводы
/// политики оценки, а не то, что утверждает источник. Записать их фактом
/// значит стереть различие между наблюдением и нашим выводом
/// (docs/decisions/0002-polnota-ocenki-i-ispolnimost-ceny-dve-osi.md).
/// Доменный PriceQuality шире: он обязан читать старый журнал.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceQualityDto {
    Executable,
    PreviousClose,
    OwnerEstimate,
}

impl PriceQualityDto {
    #[must_use]
    pub const fn to_domain(self) -> PriceQuality {
        match self {
            Self::Executable => PriceQuality::Executable,
            Self::PreviousClose => PriceQuality::PreviousClose,
            Self::OwnerEstimate => PriceQuality::OwnerEstimate,
        }
    }
}

/// Происхождение комиссии в транспорте.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeeOriginDto {
    Brokerage,
    Depositary,
    AccountMaintenance,
    MarginInterest,
    Other,
}

impl FeeOriginDto {
    #[must_use]
    pub const fn to_domain(self) -> FeeOrigin {
        match self {
            Self::Brokerage => FeeOrigin::Brokerage,
            Self::Depositary => FeeOrigin::Depositary,
            Self::AccountMaintenance => FeeOrigin::AccountMaintenance,
            Self::MarginInterest => FeeOrigin::MarginInterest,
            Self::Other => FeeOrigin::Other,
        }
    }
}

/// Даты операции.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct OperationDatesDto {
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date, example = "2026-01-15")]
    pub trade: Option<Date>,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub settled: Option<Date>,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub cash_posted: Option<Date>,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub paid: Option<Date>,
}

/// Вид операции. Величины **положительные**: знак задаёт вид, а не клиент.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationKindDto {
    Deposit {
        amount: String,
        currency: CurrencyDto,
    },
    Withdrawal {
        amount: String,
        currency: CurrencyDto,
    },
    Transfer {
        to_account: Uuid,
        amount: String,
        currency: CurrencyDto,
    },
    Buy {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        amount: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fee: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accrued_interest: Option<String>,
        currency: CurrencyDto,
    },
    Sell {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        amount: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fee: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accrued_interest: Option<String>,
        currency: CurrencyDto,
    },
    Income {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instrument: Option<Uuid>,
        amount: String,
        currency: CurrencyDto,
    },
    Fee {
        amount: String,
        currency: CurrencyDto,
        origin: FeeOriginDto,
    },
    OpeningCash {
        amount: String,
        currency: CurrencyDto,
    },
    OpeningPosition {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_basis: Option<String>,
        currency: CurrencyDto,
    },
    Valuation {
        instrument: Uuid,
        price: String,
        currency: CurrencyDto,
        quality: PriceQualityDto,
    },
}

/// Операция целиком.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OperationDto {
    pub account: Uuid,
    #[serde(flatten)]
    pub kind: OperationKindDto,
    #[serde(default)]
    pub dates: OperationDatesDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_operation_id: Option<String>,
}

fn decimal(value: &str, field: &str) -> Result<Decimal, Rejection> {
    value.parse::<Decimal>().map_err(|_| Rejection {
        field: field.to_owned(),
        expected: "десятичное число в виде строки".into(),
        actual: value.to_owned(),
    })
}

fn minor(value: &str, currency: CurrencyDto, field: &str) -> Result<i64, Rejection> {
    iaam_app::ingest::operation::to_minor_units(decimal(value, field)?, currency.to_domain(), field)
}

fn optional_minor(
    value: Option<&String>,
    currency: CurrencyDto,
    field: &str,
) -> Result<Option<i64>, Rejection> {
    match value {
        None => Ok(None),
        Some(raw) => minor(raw, currency, field).map(Some),
    }
}

impl OperationDto {
    /// Преобразование в доменную операцию.
    ///
    /// Единственное место, где транспорт встречается с доменом. Отказ
    /// возвращается с полем, ожидаемым и полученным — это тело ответа
    /// `422` (§13).
    pub fn to_domain(&self) -> Result<SubmittedOperation, Rejection> {
        let kind = self.kind_to_domain()?;
        Ok(SubmittedOperation {
            account: AccountId(self.account),
            kind,
            dates: OperationDates {
                trade: self.dates.trade,
                settled: self.dates.settled,
                cash_posted: self.dates.cash_posted,
                paid: self.dates.paid,
            },
            idempotency_key: self.idempotency_key.clone(),
            source_operation_id: self.source_operation_id.clone(),
        })
    }

    fn kind_to_domain(&self) -> Result<OperationKind, Rejection> {
        Ok(match &self.kind {
            OperationKindDto::Deposit { amount, currency } => OperationKind::Deposit {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Withdrawal { amount, currency } => OperationKind::Withdrawal {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Transfer {
                to_account,
                amount,
                currency,
            } => OperationKind::Transfer {
                to: AccountId(*to_account),
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Buy {
                instrument,
                custody,
                quantity,
                amount,
                fee,
                accrued_interest,
                currency,
            } => OperationKind::Buy {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Dec::new(decimal(quantity, "quantity")?),
                gross_minor: minor(amount, *currency, "amount")?,
                fee_minor: optional_minor(fee.as_ref(), *currency, "fee")?,
                accrued_interest_minor: optional_minor(
                    accrued_interest.as_ref(),
                    *currency,
                    "accrued_interest",
                )?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Sell {
                instrument,
                custody,
                quantity,
                amount,
                fee,
                accrued_interest,
                currency,
            } => OperationKind::Sell {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Dec::new(decimal(quantity, "quantity")?),
                gross_minor: minor(amount, *currency, "amount")?,
                fee_minor: optional_minor(fee.as_ref(), *currency, "fee")?,
                accrued_interest_minor: optional_minor(
                    accrued_interest.as_ref(),
                    *currency,
                    "accrued_interest",
                )?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Income {
                instrument,
                amount,
                currency,
            } => OperationKind::Income {
                instrument: instrument.map(InstrumentId),
                gross_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::Fee {
                amount,
                currency,
                origin,
            } => OperationKind::Fee {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
                origin: origin.to_domain(),
            },
            OperationKindDto::OpeningCash { amount, currency } => OperationKind::OpeningCash {
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
            },
            OperationKindDto::OpeningPosition {
                instrument,
                custody,
                quantity,
                cost_basis,
                currency,
            } => OperationKind::OpeningPosition {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Dec::new(decimal(quantity, "quantity")?),
                cost_basis_minor: optional_minor(cost_basis.as_ref(), *currency, "cost_basis")?,
                currency: currency.to_domain(),
                // Утверждения о восстановленном начале (§10.7) через
                // транспорт пока не принимаются: их DTO появится вместе
                // с остальными маршрутами приёмки. `None` означает
                // «владелец ничего не утверждал», и приёмка подставит
                // умолчание, в котором всё неизвестно, — а не выдумает
                // уверенность из наличия стоимости.
                assertions: None,
            },
            OperationKindDto::Valuation {
                instrument,
                price,
                currency,
                quality,
            } => OperationKind::Valuation {
                instrument: InstrumentId(*instrument),
                price: Dec::new(decimal(price, "price")?),
                currency: currency.to_domain(),
                quality: quality.to_domain(),
            },
        })
    }
}

/// Запрос приёмки.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubmitOperationsRequest {
    /// Метка источника: ручной ввод, конкретный агент, конкретный файл.
    pub source_label: String,
    pub operations: Vec<OperationDto>,
}

/// Вердикт по одной операции.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerdictDto {
    /// Номер операции во входной пачке, с единицы.
    pub row: usize,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Счёт, о котором идёт речь. Заполняется у вердиктов сверки:
    /// расхождение без счёта — это задание «поищите где-нибудь».
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    /// Измерение, по которому не сошлось или нечего сверять (§10.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<String>,
}

impl VerdictDto {
    #[must_use]
    pub fn from_domain(row: usize, verdict: &Verdict) -> Self {
        let base = Self {
            row,
            verdict: verdict.code().to_owned(),
            event_id: None,
            field: None,
            expected: None,
            actual: None,
            detail: None,
            account_id: None,
            dimension: None,
        };
        match verdict {
            Verdict::Accepted { event } => Self {
                event_id: Some(event.inner()),
                ..base
            },
            Verdict::Provisional { event } => Self {
                event_id: Some(event.inner()),
                ..base
            },
            Verdict::Discrepancy {
                event,
                account,
                dimension,
                detail,
            } => Self {
                event_id: Some(event.inner()),
                account_id: Some(account.inner()),
                dimension: Some(dimension.code().to_owned()),
                detail: Some(detail.clone()),
                ..base
            },
            Verdict::NeedsReconciliation { account, dimension } => Self {
                account_id: Some(account.inner()),
                dimension: Some(dimension.code().to_owned()),
                ..base
            },
            Verdict::Duplicate { existing } => Self {
                event_id: Some(existing.inner()),
                ..base
            },
            Verdict::NeedsClassification { question } => Self {
                detail: Some(question.clone()),
                ..base
            },
            Verdict::Unsupported { reason } => Self {
                detail: Some(reason.clone()),
                ..base
            },
            Verdict::Rejected { rejection } => Self {
                field: Some(rejection.field.clone()),
                expected: Some(rejection.expected.clone()),
                actual: Some(rejection.actual.clone()),
                ..base
            },
        }
    }
}

/// Величина, которую система могла отказаться вычислить.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComputedDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ComputedDto {
    fn from_dec(value: &Computed<Dec>) -> Self {
        match value {
            Computed::Value(amount) => Self {
                value: Some(amount.inner().to_string()),
                not_computable: None,
                detail: None,
            },
            Computed::NotComputable { reason } => Self {
                value: None,
                not_computable: Some(reason.code().to_owned()),
                detail: Some(describe(reason)),
            },
        }
    }
}

fn describe(reason: &NotComputable) -> String {
    match reason {
        NotComputable::MissingPrice { instrument } => {
            format!("нет цены инструмента {}", instrument.inner())
        }
        NotComputable::MissingFxRate { from, to, date } => {
            format!("нет курса {}→{} на {date}", from.code(), to.code())
        }
        NotComputable::SolverRefused { refusal } => refusal.to_string(),
        NotComputable::NoExternalFlows => "нет потоков, пересекающих границу контура".into(),
        NotComputable::StateNewerThanReport { last_event, as_of } => {
            format!("срез содержит события до {last_event}, отчёт на {as_of}")
        }
        NotComputable::Numeric { code } => format!("арифметический отказ: {code}"),
        NotComputable::UnsupportedFinancing { account } => format!(
            "на счёте {} присутствует финансирование вне периметра",
            account.inner()
        ),
    }
}

/// Печать приближённой величины.
///
/// Печатать `f64` как есть нельзя: последние знаки двоичной плавающей
/// точки — шум, а не результат, и они меняются между платформами.
/// Восемь знаков — на четыре порядка точнее допуска решателя (1e-9
/// по невязке NPV) и ровно настолько, насколько ставка вообще имеет
/// смысл: 0,00000001 — это одна миллионная процента годовых.
fn format_rate(value: f64) -> String {
    let scaled = (value * 1e8).round();
    // −0 и 0 — одно и то же число, но печатаются по-разному.
    let normalized = if scaled == 0.0 { 0.0 } else { scaled / 1e8 };
    format!("{normalized:.8}")
}

/// Ставка доходности вместе с политикой решателя.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RateDto {
    /// Ставка в долях единицы. Приближённая величина: в денежные
    /// тождества не входит (§6.6).
    pub value: String,
    pub error_bound: String,
    pub iterations: u32,
    pub day_count: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Доли стоимости портфеля по уровням достоверности (§10.5).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NavCoverageDto {
    pub accepted_independent: String,
    pub accepted_internal: String,
    pub provisional: String,
    pub discrepant: String,
}

/// Блок качества данных (§10.5).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DataQualityDto {
    pub status: String,
    pub nav_coverage: NavCoverageDto,
    pub material_issues: Vec<String>,
}

impl DataQualityDto {
    fn from_domain(quality: &DataQuality) -> Self {
        Self {
            status: quality.status.code().to_owned(),
            nav_coverage: NavCoverageDto {
                accepted_independent: quality
                    .nav_coverage
                    .accepted_independent
                    .inner()
                    .to_string(),
                accepted_internal: quality.nav_coverage.accepted_internal.inner().to_string(),
                provisional: quality.nav_coverage.provisional.inner().to_string(),
                discrepant: quality.nav_coverage.discrepant.inner().to_string(),
            },
            material_issues: quality.material_issues.iter().map(issue).collect(),
        }
    }
}

fn issue(value: &MaterialIssue) -> String {
    match value {
        MaterialIssue::RestoredWithoutBasis { account } => format!(
            "счёт {} восстановлен без документированной стоимости",
            account.inner()
        ),
        MaterialIssue::NegativeCash { account, currency } => format!(
            "отрицательный остаток на счёте {} в {}",
            account.inner(),
            currency.code()
        ),
        MaterialIssue::HistoryStartsAt { date } => format!("история начинается {date}"),
        MaterialIssue::NoIndependentSource { account, dimension } => format!(
            "по счёту {} нет независимого подтверждения измерения {}",
            account.inner(),
            dimension.code()
        ),
        MaterialIssue::Discrepancy { account, dimension } => format!(
            "сверка счёта {} по измерению {} не сходится",
            account.inner(),
            dimension.code()
        ),
        MaterialIssue::UnsupportedFinancing { account } => format!(
            "на счёте {} присутствует финансирование вне периметра",
            account.inner()
        ),
    }
}

/// Отчёт о доходности.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReturnsReportDto {
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub as_of: Date,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub history_starts: Option<Date>,
    pub report_currency: CurrencyDto,
    pub contributed: ComputedDto,
    pub withdrawn: ComputedDto,
    pub terminal_value: ComputedDto,
    /// **Доходность до налога.** Имя поля содержит оговорку намеренно:
    /// налоги появляются в E5, и до тех пор называть эту величину
    /// «доходностью» без уточнения нельзя (§16.3).
    pub xirr_pre_tax: RateDto,
    pub applied_rules: AppliedRulesDto,
    pub data_quality: DataQualityDto,
}

/// Применённые правила (§3.2, §6.1).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppliedRulesDto {
    pub contour: Uuid,
    pub contour_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lot_rule: Option<String>,
    pub fx_source: String,
    pub day_count: String,
    /// Допустимая ширина интервала по ставке — она же определяет
    /// объявленную погрешность результата.
    pub solver_rate_tolerance: String,
    pub solver_max_iterations: u32,
    /// Окно расчётов, по которому классифицирован отрицательный
    /// остаток (§11). Цифра, зависящая от порога, обязана нести порог
    /// рядом с собой: иначе воспроизвести классификацию невозможно.
    pub perimeter_settlement_window_days: u16,
}

impl ReturnsReportDto {
    #[must_use]
    pub fn from_domain(report: &ReturnsReport) -> Self {
        let rate = match &report.xirr {
            Computed::Value(outcome) => RateDto {
                value: format_rate(outcome.rate().value()),
                error_bound: format_rate(outcome.rate().error_bound()),
                iterations: outcome.rate().iterations(),
                day_count: outcome.day_count().code().to_owned(),
                not_computable: None,
                detail: None,
            },
            Computed::NotComputable { reason } => RateDto {
                value: String::new(),
                error_bound: String::new(),
                iterations: 0,
                day_count: report.applied_rules.day_count.code().to_owned(),
                not_computable: Some(reason.code().to_owned()),
                detail: Some(describe(reason)),
            },
        };
        Self {
            as_of: report.as_of,
            history_starts: report.history_starts,
            report_currency: CurrencyDto::from_domain(report.report_currency),
            contributed: ComputedDto::from_dec(&report.contributed),
            withdrawn: ComputedDto::from_dec(&report.withdrawn),
            terminal_value: ComputedDto::from_dec(&report.terminal_value),
            xirr_pre_tax: rate,
            applied_rules: AppliedRulesDto {
                contour: report.applied_rules.contour.0,
                contour_version: report.applied_rules.contour_version.0,
                lot_rule: report
                    .applied_rules
                    .lot_rule
                    .as_ref()
                    .map(|id| id.0.clone()),
                fx_source: report.applied_rules.fx_source.code().to_owned(),
                day_count: report.applied_rules.day_count.code().to_owned(),
                solver_rate_tolerance: report
                    .applied_rules
                    .solver_policy
                    .rate_tolerance
                    .to_string(),
                solver_max_iterations: report.applied_rules.solver_policy.max_iterations,
                perimeter_settlement_window_days: report
                    .applied_rules
                    .perimeter_policy
                    .settlement_window_days,
            },
            data_quality: DataQualityDto::from_domain(&report.data_quality),
        }
    }
}

/// Счёт.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountDto {
    pub id: Uuid,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Создание счёта.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateAccountRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Приём брокерского токена.
///
/// **`Debug` написан вручную.** Производный напечатал бы токен целиком,
/// а `{:?}` над непонятым запросом — обычный способ разобраться, почему
/// он не разобрался. Из лога токен уже не убрать (§14).
///
/// Области прав здесь нет намеренно: она задаётся системой, а не
/// клиентом (§14). Лишние поля тела молча игнорируются, поэтому
/// присланная клиентом «область» ни на что не влияет.
#[derive(Deserialize, ToSchema)]
pub struct AddBrokerAccessRequest {
    /// Код брокера, например `tinkoff`.
    pub broker: String,
    /// Среда брокера. Поле обязательное и умолчания не имеет: токены
    /// у сред разные, и молча записанная не та среда оборачивается
    /// отказом шлюза при первом обращении — по тексту которого о среде
    /// не догадаться.
    pub environment: BrokerEnvironmentDto,
    /// Токен брокера. Секрет: принимается, но никогда не возвращается,
    /// поэтому в схеме помечен как `password` и `writeOnly`.
    #[schema(format = Password, write_only, example = "<секрет>")]
    pub token: String,
}

impl fmt::Debug for AddBrokerAccessRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AddBrokerAccessRequest")
            .field("broker", &self.broker)
            .field("environment", &self.environment)
            .field("token", &"<скрыт>")
            .finish()
    }
}

/// Среда брокера в транспорте. Отдельный тип, потому что
/// `BrokerEnvironment` порта не знает про OpenAPI и знать не должна.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrokerEnvironmentDto {
    Prod,
    Sandbox,
}

impl BrokerEnvironmentDto {
    #[must_use]
    pub const fn to_domain(self) -> BrokerEnvironment {
        match self {
            Self::Prod => BrokerEnvironment::Prod,
            Self::Sandbox => BrokerEnvironment::Sandbox,
        }
    }
}

/// Заведённый доступ к брокеру.
///
/// `Debug` производный: секрета в этом типе нет — ни токена, ни
/// шифротекста сюда не попадает, потому что их нет и в порте.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BrokerAccessDto {
    pub id: Uuid,
    pub broker: String,
    /// Среда: `prod` или `sandbox`. Строкой, а не перечислением:
    /// запись пришла из базы, и незнакомое значение обязано доехать
    /// до владельца как есть, а не превратиться в отказ на чтении.
    pub environment: String,
    /// Область прав. Всегда `read_only`: торговые права не
    /// запрашиваются ни при каких условиях (§14).
    pub scope: String,
    pub created_at: String,
    /// Момент отзыва. `null` — доступ действует. Поле не опускается
    /// при отсутствии значения: пропавшее поле неотличимо от «не знаем».
    pub revoked_at: Option<String>,
}

impl BrokerAccessDto {
    #[must_use]
    pub fn from_domain(access: BrokerAccessView) -> Self {
        Self {
            id: access.id,
            broker: access.broker,
            environment: access.environment,
            scope: access.scope,
            created_at: access.created_at,
            revoked_at: access.revoked_at,
        }
    }
}

/// Присвоение экземпляра.
///
/// Код прочитан с консоли при старте сервера — см. `claim`. Метка
/// описывает, чем именно владелец будет ходить: «ноутбук», «телефон».
#[derive(Clone, Deserialize, ToSchema)]
pub struct ClaimRequest {
    /// Одноразовый код присвоения. Секрет: принимается, но никогда
    /// не возвращается, поэтому в схеме помечен как `password`.
    #[schema(format = Password, write_only, example = "<код с консоли>")]
    pub code: String,
    /// Метка выпускаемого токена.
    pub label: String,
}

/// `Debug` вручную: код присвоения — это право завести владельца,
/// и производный вывод отправил бы его в первый же лог.
impl fmt::Debug for ClaimRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimRequest")
            .field("code", &"<скрыт>")
            .field("label", &self.label)
            .finish()
    }
}

/// Область прав в транспорте. Отдельный тип, потому что `Scope`
/// приложения не знает про OpenAPI и знать не должен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenScopeDto {
    /// Полный доступ владельца. В запросе на выпуск **не принимается**:
    /// владелец заводится присвоением экземпляра или консолью.
    Owner,
    Agent,
    ReadOnly,
}

impl TokenScopeDto {
    #[must_use]
    pub const fn from_domain(scope: Scope) -> Self {
        match scope {
            Scope::Owner => Self::Owner,
            Scope::Agent => Self::Agent,
            Scope::ReadOnly => Self::ReadOnly,
        }
    }
}

/// Запрос на выпуск токена.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    /// Чем этот токен будет ходить: «домашний агент», «телефон».
    /// Метка — единственное, по чему потом узнают, какой токен отзывать.
    pub label: String,
    pub scope: TokenScopeDto,
}

/// Только что выпущенный токен.
///
/// Один тип и на присвоение экземпляра, и на выпуск токена агенту:
/// в обоих случаях наружу уходит секрет, показываемый **один раз**,
/// и второй такой тип означал бы второе место, где о нём можно забыть.
#[derive(Clone, Serialize, ToSchema)]
pub struct IssuedTokenDto {
    /// Идентификатор записи — по нему токен отзывают.
    pub id: Uuid,
    /// Сам токен. Показывается **один раз**: в базе остаётся только
    /// хеш, и повторить показ неоткуда.
    #[schema(format = Password, example = "<секрет>")]
    pub token: String,
    pub label: String,
    pub scope: TokenScopeDto,
}

/// `Debug` вручную: производный вывел бы токен в первый же лог,
/// а лог переживает и процесс, и сам токен.
impl fmt::Debug for IssuedTokenDto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedTokenDto")
            .field("id", &self.id)
            .field("token", &"<скрыт>")
            .field("label", &self.label)
            .field("scope", &self.scope)
            .finish()
    }
}

impl IssuedTokenDto {
    #[must_use]
    pub fn from_domain(issued: IssuedToken) -> Self {
        Self {
            id: issued.id,
            token: issued.token,
            label: issued.label,
            scope: TokenScopeDto::from_domain(issued.scope),
        }
    }
}

/// Выданный токен в списке.
///
/// `Debug` производный: секрета в этом типе нет — ни токена, ни хеша
/// сюда не попадает, потому что их нет и в порте.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TokenDto {
    pub id: Uuid,
    pub label: String,
    pub scope: TokenScopeDto,
    pub created_at: String,
    /// Момент отзыва. `null` — токен действует. Поле не опускается
    /// при отсутствии значения: пропавшее поле неотличимо от «не знаем».
    pub revoked_at: Option<String>,
}

impl TokenDto {
    #[must_use]
    pub fn from_domain(token: TokenView) -> Self {
        Self {
            id: token.id,
            label: token.label,
            scope: TokenScopeDto::from_domain(token.scope),
            created_at: token.created_at,
            revoked_at: token.revoked_at,
        }
    }
}

/// Новая версия состава контура.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateContourVersionRequest {
    /// Идентификатор контура. Отсутствует — заводится новый.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contour: Option<Uuid>,
    pub title: String,
    pub accounts: Vec<Uuid>,
}

/// Ответ о версии контура.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContourVersionDto {
    pub contour: Uuid,
    pub version: u32,
    pub accounts: Vec<Uuid>,
}

/// Курс валюты на дату, названный владельцем (§6.1).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FxRateDto {
    pub from: CurrencyDto,
    pub to: CurrencyDto,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub rate: String,
}

/// Наблюдение цены с полным происхождением.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketPriceDto {
    pub instrument: Uuid,
    pub board: String,
    pub session: i64,
    pub kind: String,
    pub value: String,
    pub currency: String,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub complete_through: Option<Date>,
}

/// Наблюдение курса с полным происхождением.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketFxDto {
    pub from: CurrencyDto,
    pub to: CurrencyDto,
    pub nominal: u32,
    pub value: String,
    pub unit_rate: String,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub complete_through: Option<Date>,
}

/// Интервал ключевой ставки, выведенный из дневных наблюдений.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketKeyRateDto {
    pub value: String,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub from: Date,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub until: Option<Date>,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
    pub boundary: String,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub complete_through: Option<Date>,
}

/// Состояние сервиса.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthDto {
    pub status: String,
    pub schema_version: u32,
    pub projection_version: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::ids::{EventId, InstrumentId};
    use iaam_core::numeric::xirr::SolverRefusal;

    #[test]
    fn every_verdict_reaches_the_wire_with_the_field_that_explains_it() {
        // Вердикт — это ответ внешнему агенту. Потерянное поле оставляет
        // его с кодом «rejected» и без указания, что именно чинить:
        // такой ответ хуже отсутствующего, потому что выглядит полным.
        let event = EventId::new_random();
        let provisional = VerdictDto::from_domain(1, &Verdict::Provisional { event });
        assert_eq!(provisional.verdict, "provisional");
        assert_eq!(provisional.row, 1, "номер строки идёт в ответ как есть");
        assert_eq!(provisional.event_id, Some(event.inner()));

        let duplicate = VerdictDto::from_domain(2, &Verdict::Duplicate { existing: event });
        assert_eq!(duplicate.event_id, Some(event.inner()));

        let needs = VerdictDto::from_domain(
            3,
            &Verdict::NeedsClassification {
                question: "что это за операция?".into(),
            },
        );
        assert_eq!(needs.detail.as_deref(), Some("что это за операция?"));

        let unsupported = VerdictDto::from_domain(
            4,
            &Verdict::Unsupported {
                reason: "производные вне периметра".into(),
            },
        );
        assert_eq!(
            unsupported.detail.as_deref(),
            Some("производные вне периметра")
        );

        let rejected = VerdictDto::from_domain(
            5,
            &Verdict::Rejected {
                rejection: Rejection {
                    field: "amount".into(),
                    expected: "положительная величина".into(),
                    actual: "-1".into(),
                },
            },
        );
        assert_eq!(rejected.field.as_deref(), Some("amount"));
        assert_eq!(
            rejected.expected.as_deref(),
            Some("положительная величина"),
            "без ожидаемого значения отказ не объясняет, что чинить"
        );
        assert_eq!(rejected.actual.as_deref(), Some("-1"));
    }

    #[test]
    fn the_debug_of_a_broker_request_never_carries_the_token() {
        // `{:?}` над непонятым запросом — обычный способ разобраться,
        // почему он не разобрался, и производный `Debug` отправил бы
        // туда сам токен. Из лога его уже не убрать (§14).
        const TOKEN: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";
        let request = AddBrokerAccessRequest {
            broker: "tinkoff".into(),
            environment: BrokerEnvironmentDto::Sandbox,
            token: TOKEN.into(),
        };

        let printed = format!("{request:?}");
        assert!(
            !printed.contains(TOKEN),
            "токен утёк в отладочный вывод: {printed}"
        );
        assert!(
            printed.contains("tinkoff"),
            "код брокера секретом не является и обязан оставаться видимым: {printed}"
        );
        assert!(
            printed.contains("Sandbox"),
            "среда секретом не является и обязана оставаться видимой: {printed}"
        );
    }

    #[test]
    fn an_issued_token_never_reaches_the_debug_output() {
        // Ответ с токеном показывается один раз — и ровно один раз он
        // существует открытым. Производный `Debug` отправил бы его
        // в первый же лог, а лог переживает и процесс, и сам токен.
        const ISSUED: &str = "0123456789abcdef0123456789abcdef";
        let response = IssuedTokenDto {
            id: Uuid::new_v4(),
            token: ISSUED.into(),
            label: "домашний агент".into(),
            scope: TokenScopeDto::Agent,
        };

        let printed = format!("{response:?}");
        assert!(
            !printed.contains(ISSUED),
            "токен утёк в отладочный вывод: {printed}"
        );
        assert!(
            printed.contains("домашний агент"),
            "метка секретом не является и обязана оставаться видимой: {printed}"
        );
    }

    #[test]
    fn a_claim_code_never_reaches_the_debug_output() {
        // Код присвоения — это право завести владельца в пустой базе.
        const CODE: &str = "0123456789abcdef0123456789abcdef";
        let request = ClaimRequest {
            code: CODE.into(),
            label: "ноутбук".into(),
        };

        let printed = format!("{request:?}");
        assert!(
            !printed.contains(CODE),
            "код присвоения утёк в отладочный вывод: {printed}"
        );
        assert!(printed.contains("ноутбук"), "{printed}");
    }

    #[test]
    fn a_refusal_to_compute_says_what_exactly_was_missing() {
        // `not_computable` даёт код, `detail` — конкретику: какой
        // инструмент, какая пара валют, какая дата. Пустое пояснение
        // превращает «не посчитали» в «неизвестно почему».
        let instrument = InstrumentId::new_random();
        let missing_price = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::MissingPrice { instrument },
        });
        assert_eq!(missing_price.value, None);
        assert_eq!(
            missing_price.not_computable.as_deref(),
            Some("missing_price")
        );
        let detail = missing_price.detail.expect("пояснение");
        assert!(
            detail.contains(&instrument.inner().to_string()),
            "пояснение обязано называть инструмент: {detail}"
        );

        let no_flows = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::NoExternalFlows,
        });
        assert_eq!(
            no_flows.detail.as_deref(),
            Some("нет потоков, пересекающих границу контура")
        );

        let refused = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::SolverRefused {
                refusal: SolverRefusal::NoSignChange,
            },
        });
        let detail = refused.detail.expect("пояснение");
        assert!(!detail.is_empty());
        assert_ne!(
            detail, "нет потоков, пересекающих границу контура",
            "разные причины обязаны объясняться по-разному"
        );
    }
}
/// Параметры загрузки отчёта. Тело маршрута — двоичные байты книги.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct DocumentParams {
    #[serde(default)]
    pub account: Option<Uuid>,
}

/// Ответ загрузки отчёта.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DocumentDto {
    pub document_hash: String,
    pub source: Uuid,
    pub broker: String,
    pub format: String,
    pub parser_version: String,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub period_from: Option<Date>,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub period_to: Option<Date>,
    pub rows: Vec<VerdictDto>,
}

/// Параметры диапазона сверки.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ReconciliationParams {
    pub account: Uuid,
    pub from: String,
    pub to: String,
}

/// Статус одного измерения.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DimensionStatusDto {
    pub dimension: String,
    pub status: String,
}

/// Основание повышения статуса.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvidenceDto {
    pub ground: String,
    pub level: String,
    pub dimensions: Vec<String>,
    pub confirming_parser: String,
    pub confirmed_parser: String,
}

/// Исход одного контрольного утверждения.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClaimOutcomeDto {
    pub claim: String,
    pub outcome: String,
}

/// Статус сверки счёта за интервал.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReconciliationStatusDto {
    pub account: Uuid,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub from: Date,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub to: Date,
    pub dimensions: Vec<DimensionStatusDto>,
    pub evidence: Vec<EvidenceDto>,
    pub outcomes: Vec<ClaimOutcomeDto>,
}

/// Денежный остаток, названный владельцем.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OwnerCashDto {
    pub currency: CurrencyDto,
    pub amount: String,
}

/// Позиция, названная владельцем.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OwnerPositionDto {
    pub instrument: Uuid,
    pub custody: Uuid,
    pub quantity: String,
}

/// Ответ владельца на запрос контрольного остатка.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OwnerBalanceRequest {
    pub account: Uuid,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub from: Date,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub to: Date,
    pub at: String,
    #[serde(default)]
    pub cash: Option<OwnerCashDto>,
    #[serde(default)]
    pub positions: Vec<OwnerPositionDto>,
    #[serde(default)]
    pub source_hash: Option<String>,
}

/// Правило классификации.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClassificationRuleDto {
    pub id: Uuid,
    pub version: u64,
    pub matcher: String,
    pub outcome: String,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<Uuid>,
}

impl ClassificationRuleDto {
    #[must_use]
    pub fn from_port(rule: ClassificationRuleView) -> Self {
        Self {
            id: rule.id,
            version: u64::from(rule.version),
            matcher: rule.matcher,
            outcome: rule.outcome,
            created_at: rule.created_at,
            retired_at: rule.retired_at,
            replaces: rule.replaces,
        }
    }
}

/// Запрос создания или изменения правила.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ClassificationRuleRequest {
    pub matcher: String,
    pub outcome: String,
    #[serde(default)]
    pub replaces: Option<Uuid>,
}

/// Идентификатор правила в DELETE.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ClassificationRuleParams {
    pub id: Uuid,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BrokerSyncRequest {
    pub account: Uuid,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub from: Date,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub to: Date,
}

/// Результат синхронизации брокерского канала.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncOutcomeDto {
    pub recorded: Vec<VerdictDto>,
    pub duplicates: usize,
    pub assertions: usize,
}

impl SyncOutcomeDto {
    #[must_use]
    pub fn from_domain(outcome: iaam_app::sync::SyncOutcome) -> Self {
        Self {
            recorded: outcome
                .recorded
                .iter()
                .enumerate()
                .map(|(row, verdict)| VerdictDto::from_domain(row + 1, verdict))
                .collect(),
            duplicates: outcome.duplicates,
            assertions: outcome.assertions,
        }
    }
}
/// Замена секрета доступа: секрет никогда не является частью ответа.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BrokerAccessUpdateRequest {
    pub environment: BrokerEnvironmentDto,
    pub token: String,
}

/// Инструмент справочника.
///
/// Поля `source` псевдонима здесь нет намеренно: справочник глобален
/// и читается всеми, а `SourceId` указывает на документ конкретного
/// владельца (§14).
#[derive(Debug, Serialize, ToSchema)]
pub struct InstrumentDto {
    pub id: String,
    /// `null` — род не установлен; такой инструмент оценивается
    /// как неполный (§4.9, §5.4).
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
}

/// Данные для записи инструмента администратором или синхронизацией.
///
/// Идентификатор можно не передавать: тогда его назначает сервер. Поля
/// валюты обязательны, потому что отсутствие валюты нельзя отличить от
/// неизвестного значения в сохранённом справочнике.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInstrumentRequest {
    #[serde(default)]
    pub id: Option<Uuid>,
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveInstrumentRequest {
    pub namespace: String,
    pub value: String,
    /// Дата документа. Обязательна: ISIN меняется, и «текущего»
    /// ответа не существует (§4.7).
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub on: Date,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedInstrumentDto {
    pub instrument: String,
}
/// Параметры ручной синхронизации рынка.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MarketSourceDto {
    Moex {
        engine: String,
        market: String,
        board: String,
        secid: String,
        instrument: Uuid,
    },
    CbrDaily,
    CbrDynamic {
        cbr_currency_id: String,
        to: CurrencyDto,
    },
    CbrKeyRate,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MarketSyncRequest {
    pub source: MarketSourceDto,
    pub from: String,
    pub to: String,
}
