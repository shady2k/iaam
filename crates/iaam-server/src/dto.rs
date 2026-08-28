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

use iaam_app::ingest::journal_event::{JournalFact, SubmittedJournalEvent};
use iaam_app::ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use iaam_app::ingest::{Rejection, Verdict};
use iaam_app::ports::{
    BrokerAccessView, BrokerEnvironment, ClassificationRuleView, IssuedToken, Scope, TokenView,
};
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{FeeOrigin, IncomeKind};
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::bond::offer::OfferChoice;
use iaam_core::rules::{ExpectedPosting, PostingKind};
use iaam_core::returns::zero_reinvestment::{
    BondScenarioResult, IrrLabel, LifetimeCohortMetric, ProspectiveMetric,
    ZeroReinvestmentMetrics,
};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::returns::{
    AmountQualification, BondPositionAttributes, Computed, DataQuality, EvaluatedPosition,
    ExecutabilityShares, LiquidationEstimate, MaterialIssue, NotComputable, PositionCoverage,
    ReturnsReport, UncoveredPosition,
};
use iaam_core::valuation::{
    PriceFreshness, PriceOrigin, PriceProvenance, PriceQuality, PriceSelection, QuotationBasis,
    SelectedPrice, SourceExecutability,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
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

/// Вид дохода в транспорте.
///
/// Варианта «прочее» нет — как и в ядре: мешок, по которому нельзя
/// принять решение, не отличается от незнания, а незнание выражается
/// отсутствием поля.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IncomeKindDto {
    Coupon,
    Dividend,
    DepositInterest,
}

impl IncomeKindDto {
    #[must_use]
    pub const fn to_domain(self) -> IncomeKind {
        match self {
            Self::Coupon => IncomeKind::Coupon,
            Self::Dividend => IncomeKind::Dividend,
            Self::DepositInterest => IncomeKind::DepositInterest,
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
        /// Вид дохода. Отсутствие поля означает «не утверждалось»:
        /// без него API продолжил бы терять вид у журнала, который
        /// его уже умеет хранить.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<IncomeKindDto>,
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
                kind,
            } => OperationKind::Income {
                instrument: instrument.map(InstrumentId),
                gross_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
                kind: kind.map(IncomeKindDto::to_domain),
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
        NotComputable::QuotationBasisUnknown { instrument } => {
            format!(
                "неизвестно основание котировки инструмента {}",
                instrument.inner()
            )
        }
        NotComputable::QuotationBasisContradictsEvidence { instrument } => {
            format!(
                "основание котировки противоречит доказательству инструмента {}",
                instrument.inner()
            )
        }
        NotComputable::RemainingFaceUnknown { instrument } => {
            format!(
                "неизвестен остаточный номинал инструмента {}",
                instrument.inner()
            )
        }
        NotComputable::RemainingFaceAmbiguous { instrument } => {
            format!(
                "неоднозначен остаточный номинал инструмента {}",
                instrument.inner()
            )
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
        NotComputable::ScheduleMissing { instrument } => {
            format!("нет графика выпуска инструмента {}", instrument.inner())
        }
        NotComputable::AccruedObservationMissing { instrument } => {
            format!("нет наблюдения НКД инструмента {}", instrument.inner())
        }
        NotComputable::CouponUndetermined { instrument } => {
            format!(
                "не определена сумма купона инструмента {}",
                instrument.inner()
            )
        }
        NotComputable::OutsideScheduleCoverage { instrument } => {
            format!(
                "дата отчёта вне покрытия графика инструмента {}",
                instrument.inner()
            )
        }
        NotComputable::OverlappingScheduleCoverage { instrument } => {
            format!(
                "дата отчёта покрыта несколькими периодами графика инструмента {}",
                instrument.inner()
            )
        }
        NotComputable::ExitNotExecutable => "нет исполнимого выхода для реализации НКД".to_owned(),
        NotComputable::PrincipalUnknown => "неизвестен номинал для пересчёта котировки".into(),
        NotComputable::NonPositiveDuration { coordinate, terminal_date } => {
            format!("дата окончания {terminal_date} не позже координаты {coordinate}")
        }
        NotComputable::NonPositiveInitialCapital => "начальная стоимость не положительна".into(),
        NotComputable::NegativeTerminalWealth => "терминальное благосостояние отрицательно".into(),
        NotComputable::PrincipalStateAmbiguous { instrument } => {
            format!("неоднозначное состояние номинала инструмента {}", instrument.inner())
        }
        NotComputable::AcquisitionBasisUnknown => "историческая стоимость приобретения неизвестна".into(),
        NotComputable::AccruedInterestAtAcquisitionUnknown => {
            "НКД, уплаченный при приобретении, неизвестен".into()
        }
        NotComputable::HistoricalReceiptsUnknown => "история полученных выплат неизвестна".into(),
        NotComputable::CohortGap { gap } => gap.to_string(),
        NotComputable::CurrencyMismatch { expected, actual } => {
            format!("валюты не совпадают: {} и {}", expected.code(), actual.code())
        }
        NotComputable::ExpenseUnknown => "расход неизвестен".into(),
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

/// Расчётная денежная величина вместе с валютой.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CalcMoneyDto {
    /// Десятичное расчётное значение строкой.
    pub value: String,
    pub currency: CurrencyDto,
}

impl CalcMoneyDto {
    fn from_domain(value: &iaam_core::money::CalcMoney) -> Self {
        Self {
            value: value.value().inner().to_string(),
            currency: CurrencyDto::from_domain(value.currency()),
        }
    }
}

/// Расчётная денежная величина, которая может быть невычислимой.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComputedCalcMoneyDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<CalcMoneyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ComputedCalcMoneyDto {
    fn from_domain(value: &Computed<iaam_core::money::CalcMoney>) -> Self {
        match value {
            Computed::Value(amount) => Self {
                value: Some(CalcMoneyDto::from_domain(amount)),
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

/// Один ожидаемый платёж в нулевом реинвестиционном сценарии.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExpectedPostingDto {
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub amount: CalcMoneyDto,
    pub kind: PostingKindDto,
}

impl ExpectedPostingDto {
    fn from_domain(value: &ExpectedPosting) -> Self {
        Self {
            date: value.date,
            amount: CalcMoneyDto::from_domain(&value.amount),
            kind: PostingKindDto::from_domain(value.kind),
        }
    }
}

/// Вид ожидаемого платежа.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostingKindDto {
    Coupon,
    PrincipalReturn,
    OfferSettlement,
}

impl PostingKindDto {
    fn from_domain(value: PostingKind) -> Self {
        match value {
            PostingKind::Coupon => Self::Coupon,
            PostingKind::PrincipalReturn => Self::PrincipalReturn,
            PostingKind::OfferSettlement => Self::OfferSettlement,
        }
    }
}

const ZERO_REINVESTMENT_NOTE: &str = "Полученные купоны и возвраты номинала считаются сохранёнными до конца срока и не приносящими дохода; если их тратить или реинвестировать, единой цифры терминального капитала не существует — остаются график выплат и IRR.";
/// Пять величин §7.1 для одного сценария.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ZeroReinvestmentMetricsDto {
    pub postings: Vec<ExpectedPostingDto>,
    pub terminal_wealth: CalcMoneyDto,
    pub surplus: CalcMoneyDto,
    pub hpr: ComputedDto,
    pub cagr_0r: RateDto,
    /// Полученные купоны и возвраты номинала считаются сохранёнными до конца
    /// срока и не приносящими дохода; если их тратить или реинвестировать,
    /// единой цифры терминального капитала не существует — остаются график
    /// выплат и IRR.
    pub zero_reinvestment_assumed: bool,
    /// Пояснение допущения, показываемое рядом с рассчитанными величинами.
    pub zero_reinvestment_note: String,
    /// Ряд до налога; налоговая политика появится в E5.
    pub pre_tax: bool,
}

impl ZeroReinvestmentMetricsDto {
    fn from_domain(value: &ZeroReinvestmentMetrics) -> Self {
        Self {
            postings: value
                .postings
                .iter()
                .map(ExpectedPostingDto::from_domain)
                .collect(),
            terminal_wealth: CalcMoneyDto::from_domain(&value.terminal_wealth),
            surplus: CalcMoneyDto::from_domain(&value.surplus),
            hpr: ComputedDto::from_dec(&value.hpr),
            cagr_0r: rate_dto(&value.cagr_0r, "act/365"),
            zero_reinvestment_assumed: value.zero_reinvestment_assumed,
            zero_reinvestment_note: ZERO_REINVESTMENT_NOTE.to_owned(),
            pre_tax: value.pre_tax,
        }
    }
}

/// Метрики сценария, которые могут быть невычислимы целиком.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComputedZeroReinvestmentMetricsDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ZeroReinvestmentMetricsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ComputedZeroReinvestmentMetricsDto {
    fn from_domain(value: &Computed<ZeroReinvestmentMetrics>) -> Self {
        match value {
            Computed::Value(metrics) => Self {
                value: Some(ZeroReinvestmentMetricsDto::from_domain(metrics)),
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

/// Сценарий удержания до погашения или предъявления к оферте.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OfferChoiceDto {
    HoldToMaturity,
    ExerciseAtOffer { window: Uuid },
}

impl OfferChoiceDto {
    fn from_domain(value: &OfferChoice) -> Self {
        match value {
            OfferChoice::HoldToMaturity => Self::HoldToMaturity,
            OfferChoice::ExerciseAtOffer { window } => Self::ExerciseAtOffer {
                window: window.0,
            },
        }
    }
}

/// Ярлык ставки на проспективной координате.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IrrLabelDto {
    YieldToMaturity,
    YieldToOffer,
}

impl IrrLabelDto {
    fn from_domain(value: IrrLabel) -> Self {
        match value {
            IrrLabel::YieldToMaturity => Self::YieldToMaturity,
            IrrLabel::YieldToOffer => Self::YieldToOffer,
        }
    }
}

/// Проспективная метрика от даты отчёта.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProspectiveMetricDto {
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub as_of: Date,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub terminal_date: Date,
    pub c0: ComputedCalcMoneyDto,
    pub metrics: ComputedZeroReinvestmentMetricsDto,
    pub irr: RateDto,
    pub irr_label: IrrLabelDto,
}

impl ProspectiveMetricDto {
    fn from_domain(value: &ProspectiveMetric) -> Self {
        Self {
            as_of: value.as_of,
            terminal_date: value.terminal_date,
            c0: ComputedCalcMoneyDto::from_domain(&value.c0),
            metrics: ComputedZeroReinvestmentMetricsDto::from_domain(&value.metrics),
            irr: rate_dto(&value.irr, "act/365"),
            irr_label: IrrLabelDto::from_domain(value.irr_label),
        }
    }
}

/// Пожизненная метрика одной когорты.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LifetimeCohortMetricDto {
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub acquired: Date,
    pub quantity: String,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub terminal_date: Date,
    pub c0: ComputedCalcMoneyDto,
    pub metrics: ComputedZeroReinvestmentMetricsDto,
    /// Исторический IRR отсутствует, потому что прошлые выплаты агрегированы
    /// без дат.
    pub irr_absent_because: String,
}

impl LifetimeCohortMetricDto {
    fn from_domain(value: &LifetimeCohortMetric) -> Self {
        Self {
            acquired: value.acquired.inner(),
            quantity: value.quantity.0.inner().to_string(),
            terminal_date: value.terminal_date,
            c0: ComputedCalcMoneyDto::from_domain(&value.c0),
            metrics: ComputedZeroReinvestmentMetricsDto::from_domain(&value.metrics),
            irr_absent_because: value.irr_absent_because.to_owned(),
        }
    }
}

/// Метрики одной облигационной позиции по одному сценарию.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BondScenarioResultDto {
    pub choice: OfferChoiceDto,
    pub prospective: ProspectiveMetricDto,
    pub lifetime: ComputedLifetimeCohortMetricsDto,
}

impl BondScenarioResultDto {
    fn from_domain(value: &BondScenarioResult) -> Self {
        Self {
            choice: OfferChoiceDto::from_domain(&value.choice),
            prospective: ProspectiveMetricDto::from_domain(&value.prospective),
            lifetime: ComputedLifetimeCohortMetricsDto::from_domain(&value.lifetime),
        }
    }
}

/// Пожизненные метрики могут целиком отказаться из-за пробела в истории.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComputedLifetimeCohortMetricsDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Vec<LifetimeCohortMetricDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ComputedLifetimeCohortMetricsDto {
    fn from_domain(value: &Computed<Vec<LifetimeCohortMetric>>) -> Self {
        match value {
            Computed::Value(cohorts) => Self {
                value: Some(
                    cohorts
                        .iter()
                        .map(LifetimeCohortMetricDto::from_domain)
                        .collect(),
                ),
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

/// Метрики всех сценариев одной облигационной позиции.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BondPositionMetricsDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub scenarios: Vec<BondScenarioResultDto>,
}

impl BondPositionMetricsDto {
    fn from_domain(value: &iaam_core::returns::BondPositionMetrics) -> Self {
        Self {
            account: value.account.inner(),
            custody: value.custody.map(|id| id.inner()),
            instrument: value.instrument.inner(),
            scenarios: value
                .scenarios
                .iter()
                .map(BondScenarioResultDto::from_domain)
                .collect(),
        }
    }
}

fn rate_dto(value: &Computed<iaam_core::numeric::xirr::RateOutcome>, fallback_day_count: &str) -> RateDto {
    match value {
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
            day_count: fallback_day_count.to_owned(),
            not_computable: Some(reason.code().to_owned()),
            detail: Some(describe(reason)),
        },
    }
}


/// Выбранная цена позиции с выводами политики и provenance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SelectedPriceDto {
    pub instrument: Uuid,
    pub price: String,
    pub currency: CurrencyDto,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub trade_date: Date,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    pub executability: String,
    pub selection: PriceSelectionDto,
    pub freshness: PriceFreshnessDto,
    pub provenance: PriceProvenanceDto,
}

/// Способ, которым политика выбрала наблюдение.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PriceSelectionDto {
    AsObserved,
    CarriedForward {
        #[serde(with = "iso_date")]
        #[schema(value_type = String, format = Date)]
        observed_on: Date,
        days: u16,
    },
    LegacyDerived {
        quality: String,
    },
}

/// Свежесть выбранного наблюдения относительно порога политики.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PriceFreshnessDto {
    Fresh,
    Stale { days: u16 },
}

/// Происхождение выбранного наблюдения.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PriceOriginDto {
    Market { venue: String, price_kind: String },
    ReportParsed { source: Uuid },
    OwnerAsserted,
}

/// Единица, в которой источник назвал цену.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotationBasisDto {
    MoneyPerUnit,
    PercentOfRemainingFace,
    /// Источник основания не доказал: цена этой строки в деньги
    /// не пересчитывается.
    #[default]
    Unknown,
}

impl QuotationBasisDto {
    #[must_use]
    pub const fn from_domain(basis: QuotationBasis) -> Self {
        match basis {
            QuotationBasis::MoneyPerUnit => Self::MoneyPerUnit,
            QuotationBasis::PercentOfRemainingFace => Self::PercentOfRemainingFace,
            QuotationBasis::Unknown => Self::Unknown,
        }
    }
}

/// Статус доказанности записанного основания цены.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotationBasisStatusDto {
    Proven,
    Contradicts,
    NotProven,
}

impl QuotationBasisStatusDto {
    #[must_use]
    pub const fn from_domain(
        status: iaam_app::scenarios::market_reference::QuotationBasisStatus,
    ) -> Self {
        match status {
            iaam_app::scenarios::market_reference::QuotationBasisStatus::Proven => Self::Proven,
            iaam_app::scenarios::market_reference::QuotationBasisStatus::Contradicts => {
                Self::Contradicts
            }
            iaam_app::scenarios::market_reference::QuotationBasisStatus::NotProven => {
                Self::NotProven
            }
        }
    }
}

/// Основание выбора: вид источника, площадка, версии и оба порога.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PriceProvenanceDto {
    pub price_kind: Option<String>,
    pub origin: PriceOriginDto,
    pub venue: Option<String>,
    #[serde(default)]
    pub quotation_basis: QuotationBasisDto,
    #[serde(default)]
    pub basis_evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    pub valuation_policy_version: u32,
    pub source_priority_version: u32,
    pub carry_forward_limit: u16,
    pub price_max_age: u16,
}

/// Позиция, оценённая выбранным наблюдением.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluatedPositionDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub quantity: String,
    pub price: SelectedPriceDto,
}

/// Позиция без выбранной цены и причина отказа.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UncoveredPositionDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub reason: String,
}

/// Позиция, оставшаяся на старом вычисленном качестве.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LegacyDerivedPositionDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub quality: String,
}

/// Покрытие ценами без выдуманного процента стоимости.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PositionCoverageDto {
    pub evaluated_positions: u32,
    pub total_positions: u32,
    pub selected: Vec<EvaluatedPositionDto>,
    pub uncovered: Vec<UncoveredPositionDto>,
    pub legacy_derived: Vec<LegacyDerivedPositionDto>,
}

/// Доли исполнимости от стоимости оценённых позиций.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExecutabilitySharesDto {
    pub evaluated_positions_value: String,
    pub executable: String,
    pub indicative_previous_close: String,
    pub unknown: String,
}

/// Денежная величина с явным квалификатором знания.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AmountQualificationDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub qualification: String,
}

/// Оценка до издержек выхода и до налога.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LiquidationEstimateDto {
    pub value_before_exit_costs_and_tax: ComputedDto,
    pub executability: ExecutabilitySharesDto,
    pub exit_costs: AmountQualificationDto,
    pub tax: AmountQualificationDto,
    pub accrued_interest_payable_on_termination: ComputedDto,
}
/// Атрибуты облигационной позиции (§5.1).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BondPositionAttributesDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub accrued_interest: ComputedDto,
    pub accrued_interest_payable_on_termination: ComputedDto,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub next_posting_date: Option<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_principal_return_finality: Option<String>,
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
    pub position_coverage: PositionCoverageDto,
    pub executability: ExecutabilitySharesDto,
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
            position_coverage: PositionCoverageDto::from_domain(&quality.position_coverage),
            executability: ExecutabilitySharesDto::from_domain(&quality.executability),
            material_issues: quality.material_issues.iter().map(issue).collect(),
        }
    }
}

impl PositionCoverageDto {
    fn from_domain(coverage: &PositionCoverage) -> Self {
        Self {
            evaluated_positions: coverage.evaluated_positions,
            total_positions: coverage.total_positions,
            selected: coverage
                .selected
                .iter()
                .map(EvaluatedPositionDto::from_domain)
                .collect(),
            uncovered: coverage
                .uncovered
                .iter()
                .map(UncoveredPositionDto::from_domain)
                .collect(),
            legacy_derived: coverage
                .legacy_derived
                .iter()
                .map(LegacyDerivedPositionDto::from_domain)
                .collect(),
        }
    }
}

impl EvaluatedPositionDto {
    fn from_domain(position: &EvaluatedPosition) -> Self {
        Self {
            account: position.account.inner(),
            custody: position.custody.map(|custody| custody.inner()),
            instrument: position.instrument.inner(),
            quantity: position.quantity.0.inner().to_string(),
            price: SelectedPriceDto::from_domain(&position.price),
        }
    }
}

impl UncoveredPositionDto {
    fn from_domain(position: &UncoveredPosition) -> Self {
        Self {
            account: position.account.inner(),
            custody: position.custody.map(|custody| custody.inner()),
            instrument: position.instrument.inner(),
            reason: uncovered_reason(&position.reason).to_owned(),
        }
    }
}

impl LegacyDerivedPositionDto {
    fn from_domain(position: &iaam_core::returns::LegacyDerivedPosition) -> Self {
        Self {
            account: position.account.inner(),
            custody: position.custody.map(|custody| custody.inner()),
            instrument: position.instrument.inner(),
            quality: position.quality.code().to_owned(),
        }
    }
}

impl SelectedPriceDto {
    fn from_domain(price: &SelectedPrice) -> Self {
        Self {
            instrument: price.candidate.instrument.inner(),
            price: price.candidate.price.inner().to_string(),
            currency: CurrencyDto::from_domain(price.candidate.currency),
            trade_date: price.candidate.trade_date,
            observed_at: format_timestamp(price.candidate.observed_at),
            executability: executability(price.candidate.executability).to_owned(),
            selection: PriceSelectionDto::from_domain(price.selection),
            freshness: PriceFreshnessDto::from_domain(price.freshness),
            provenance: PriceProvenanceDto::from_domain(&price.provenance),
        }
    }
}

impl PriceSelectionDto {
    fn from_domain(selection: PriceSelection) -> Self {
        match selection {
            PriceSelection::AsObserved => Self::AsObserved,
            PriceSelection::CarriedForward { observed_on, days } => {
                Self::CarriedForward { observed_on, days }
            }
            PriceSelection::LegacyDerived { quality } => Self::LegacyDerived {
                quality: quality.code().to_owned(),
            },
        }
    }
}

impl PriceFreshnessDto {
    fn from_domain(freshness: PriceFreshness) -> Self {
        match freshness {
            PriceFreshness::Fresh => Self::Fresh,
            PriceFreshness::Stale { days } => Self::Stale { days },
        }
    }
}

impl PriceProvenanceDto {
    fn from_domain(provenance: &PriceProvenance) -> Self {
        Self {
            price_kind: provenance.price_kind.clone(),
            origin: PriceOriginDto::from_domain(&provenance.origin),
            venue: provenance.venue.clone(),
            quotation_basis: QuotationBasisDto::from_domain(provenance.quotation_basis),
            basis_evidence: (!provenance.basis_evidence.is_empty())
                .then(|| provenance.basis_evidence.clone()),
            observed_at: format_timestamp(provenance.observed_at),
            valuation_policy_version: provenance.valuation_policy_version,
            source_priority_version: provenance.source_priority_version,
            carry_forward_limit: provenance.carry_forward_limit,
            price_max_age: provenance.price_max_age,
        }
    }
}

impl PriceOriginDto {
    fn from_domain(origin: &PriceOrigin) -> Self {
        match origin {
            PriceOrigin::Market { venue, kind } => Self::Market {
                venue: venue.board.clone(),
                price_kind: match kind {
                    iaam_core::valuation::PriceKind::Close => "close",
                    iaam_core::valuation::PriceKind::LegalClose => "legal_close",
                    iaam_core::valuation::PriceKind::WeightedAverage => "weighted_average",
                    iaam_core::valuation::PriceKind::MarketPrice2 => "market_price_2",
                    iaam_core::valuation::PriceKind::MarketPrice3 => "market_price_3",
                    iaam_core::valuation::PriceKind::AdmittedQuote => "admitted_quote",
                }
                .to_owned(),
            },
            PriceOrigin::ReportParsed { source } => Self::ReportParsed {
                source: source.inner(),
            },
            PriceOrigin::OwnerAsserted => Self::OwnerAsserted,
        }
    }
}

impl ExecutabilitySharesDto {
    fn from_domain(shares: &ExecutabilityShares) -> Self {
        Self {
            evaluated_positions_value: shares.evaluated_positions_value.inner().to_string(),
            executable: shares.executable.inner().to_string(),
            indicative_previous_close: shares.indicative_previous_close.inner().to_string(),
            unknown: shares.unknown.inner().to_string(),
        }
    }
}

impl AmountQualificationDto {
    fn from_domain(amount: AmountQualification) -> Self {
        match amount {
            AmountQualification::Known(value) => Self {
                value: Some(value.inner().to_string()),
                qualification: "known".to_owned(),
            },
            AmountQualification::Unknown => Self {
                value: None,
                qualification: "unknown".to_owned(),
            },
        }
    }
}

impl LiquidationEstimateDto {
    fn from_domain(estimate: &LiquidationEstimate) -> Self {
        Self {
            value_before_exit_costs_and_tax: ComputedDto::from_dec(
                &estimate.value_before_exit_costs_and_tax,
            ),
            executability: ExecutabilitySharesDto::from_domain(&estimate.executability),
            exit_costs: AmountQualificationDto::from_domain(estimate.exit_costs),
            tax: AmountQualificationDto::from_domain(estimate.tax),
            accrued_interest_payable_on_termination: ComputedDto::from_dec(
                &estimate.accrued_interest_payable_on_termination,
            ),
        }
    }
}

impl BondPositionAttributesDto {
    fn from_domain(attributes: &BondPositionAttributes) -> Self {
        Self {
            account: attributes.account.0,
            custody: attributes.custody.map(|custody| custody.0),
            instrument: attributes.instrument.0,
            accrued_interest: ComputedDto::from_dec(&attributes.accrued_interest),
            accrued_interest_payable_on_termination: ComputedDto::from_dec(
                &attributes.accrued_interest_payable_on_termination,
            ),
            next_posting_date: attributes.next_posting_date,
            next_principal_return_finality: attributes
                .next_principal_return_finality
                .map(principal_return_finality),
        }
    }
}

fn principal_return_finality(value: iaam_core::bond::finality::PrincipalReturnFinality) -> String {
    match value {
        iaam_core::bond::finality::PrincipalReturnFinality::Final => "final",
        iaam_core::bond::finality::PrincipalReturnFinality::Partial => "partial",
        iaam_core::bond::finality::PrincipalReturnFinality::Unknown => "unknown",
    }
    .to_owned()
}

fn format_timestamp(value: Option<OffsetDateTime>) -> Option<String> {
    value.map(|value| {
        value
            .format(&time::format_description::well_known::Rfc3339)
            .expect("временная метка provenance должна форматироваться")
    })
}

fn executability(value: SourceExecutability) -> &'static str {
    match value {
        SourceExecutability::Executable => "executable",
        SourceExecutability::IndicativePreviousClose => "indicative_previous_close",
        SourceExecutability::Unknown => "unknown",
    }
}

fn uncovered_reason(value: &iaam_core::returns::UncoveredReason) -> &'static str {
    match value {
        iaam_core::returns::UncoveredReason::NoObservation => "no_observation",
        iaam_core::returns::UncoveredReason::TooOld => "too_old",
        iaam_core::returns::UncoveredReason::AmbiguousVenue => "ambiguous_venue",
        iaam_core::returns::UncoveredReason::AmbiguousCandidate => "ambiguous_candidate",
        iaam_core::returns::UncoveredReason::NotComputable { reason } => reason.code(),
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
        MaterialIssue::OfferWindowUnresolved { submission } => format!(
            "заявка оферты {} ссылается на неизвестное окно",
            submission.inner()
        ),
        MaterialIssue::ScheduledPostingNotReceived { instrument, date } => format!(
            "ожидаемая выплата инструмента {} на {} не подтверждена",
            instrument.inner(),
            date
        ),
        MaterialIssue::AccruedInterestMismatch {
            instrument,
            computed,
            computed_currency,
            observed,
            observed_currency,
            quantity,
            date,
        } => format!(
            "НКД инструмента {} расходится: расчёт {} {} против наблюдения {} {} для количества {} на {}",
            instrument.inner(),
            computed.inner(),
            computed_currency.code(),
            observed.inner(),
            observed_currency.code(),
            quantity.0.inner(),
            date
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
    /// Оценка до гипотетических издержек выхода и до налога.
    pub liquidation_value_before_exit_costs_and_tax: LiquidationEstimateDto,
    /// Атрибуты облигационных позиций (§5.1).
    pub bond_attributes: Vec<BondPositionAttributesDto>,
    /// Метрики облигационных позиций по каждому доступному сценарию (§7.1).
    pub bond_metrics: Vec<BondPositionMetricsDto>,
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
            liquidation_value_before_exit_costs_and_tax: LiquidationEstimateDto::from_domain(
                &report.liquidation_value_before_exit_costs_and_tax,
            ),
            bond_attributes: report
                .bond_attributes
                .iter()
                .map(BondPositionAttributesDto::from_domain)
                .collect(),
            bond_metrics: report
                .bond_metrics
                .iter()
                .map(BondPositionMetricsDto::from_domain)
                .collect(),
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
    #[serde(default)]
    pub quotation_basis: QuotationBasisDto,
    /// Основание ровно в том виде, в каком его записал источник.
    pub recorded_quotation_basis: String,
    pub quotation_basis_status: QuotationBasisStatusDto,
    #[serde(default)]
    pub basis_evidence: Option<String>,
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

    fn amount(value: &str) -> AmountDto {
        AmountDto {
            amount: value.to_owned(),
            currency: CurrencyDto::Rub,
        }
    }

    /// Имя члена в JSON — часть контракта: по нему внешний агент
    /// выбирает разбор. `match` исчерпывающий, поэтому новый член
    /// обязан сломать сборку, а не тихо появиться безымянным (§15.1).
    fn corporate_action_tag(action: &CorporateActionDto) -> &'static str {
        match action {
            CorporateActionDto::PartialRedemption { .. } => "partial_redemption",
            CorporateActionDto::Redemption { .. } => "redemption",
            CorporateActionDto::Conversion { .. } => "conversion",
        }
    }

    fn offer_tag(action: &OfferExerciseDto) -> &'static str {
        match action {
            OfferExerciseDto::Submitted { .. } => "submitted",
            OfferExerciseDto::Cancelled { .. } => "cancelled",
            OfferExerciseDto::Settled { .. } => "settled",
        }
    }

    #[test]
    fn every_corporate_action_member_names_itself_in_json() {
        let redemption_fields = || CorporateActionDto::Redemption {
            instrument: Uuid::new_v4(),
            custody: Uuid::new_v4(),
            quantity: "10".into(),
            principal_returned_per_unit: amount("1000"),
            compensation: amount("10000.00"),
            effective_date: time::macros::date!(2026 - 06 - 01),
            record_date: None,
            grounds: None,
        };
        let members = [
            CorporateActionDto::PartialRedemption {
                instrument: Uuid::new_v4(),
                custody: Uuid::new_v4(),
                quantity: "10".into(),
                principal_returned_per_unit: amount("100"),
                compensation: amount("1000.00"),
                effective_date: time::macros::date!(2026 - 05 - 20),
                record_date: Some(time::macros::date!(2026 - 05 - 18)),
                grounds: None,
            },
            redemption_fields(),
            CorporateActionDto::Conversion {
                predecessor: Uuid::new_v4(),
                successor: Uuid::new_v4(),
                custody: Uuid::new_v4(),
                ratio: "1".into(),
                quantity_in: "10".into(),
                quantity_out: "10".into(),
                fractional: FractionalTreatmentDto::NotApplicable,
                compensation: None,
                effective_date: time::macros::date!(2026 - 07 - 01),
                record_date: None,
                grounds: None,
                basis_transfer: BasisTransferRuleDto::CarryOver,
            },
        ];
        for member in &members {
            let json = serde_json::to_value(member).expect("член представим в JSON");
            assert_eq!(json["type"], corporate_action_tag(member));
            // Разбор обратно обязан пройти: имя, которое сериализуется,
            // но не разбирается, — это контракт только на бумаге.
            let parsed: CorporateActionDto =
                serde_json::from_value(json).expect("член разбирается обратно");
            assert_eq!(corporate_action_tag(&parsed), corporate_action_tag(member));
            member.to_domain().expect("член доезжает до домена");
        }
    }

    #[test]
    fn every_offer_member_names_itself_in_json() {
        let members = [
            OfferExerciseDto::Submitted {
                submission: Uuid::new_v4(),
                window: Uuid::new_v4(),
                instrument: Uuid::new_v4(),
                quantity: "5".into(),
            },
            OfferExerciseDto::Cancelled {
                submission: Uuid::new_v4(),
                quantity: "5".into(),
            },
            OfferExerciseDto::Settled {
                submission: Uuid::new_v4(),
                instrument: Uuid::new_v4(),
                custody: Uuid::new_v4(),
                quantity: "5".into(),
                gross: amount("5000.00"),
                fee: Some(amount("10.00")),
                accrued_interest: Some(amount("20.00")),
            },
        ];
        for member in &members {
            let json = serde_json::to_value(member).expect("член представим в JSON");
            assert_eq!(json["type"], offer_tag(member));
            let parsed: OfferExerciseDto =
                serde_json::from_value(json).expect("член разбирается обратно");
            assert_eq!(offer_tag(&parsed), offer_tag(member));
            member.to_domain().expect("член доезжает до домена");
        }
    }

    /// Квалификатор исполнимости переводится в строку API, и строка
    /// эта — контракт: по ней внешний агент решает, можно ли цене
    /// верить. Пустая строка вместо `indicative_previous_close`
    /// выглядит как ответ, а не как отказ.
    ///
    /// Ожидаемое имя задаётся здесь ОТДЕЛЬНЫМ исчерпывающим `match`,
    /// а не берётся из проверяемой функции: тест, зовущий её же,
    /// согласится с любым её ответом. Новый член ломает сборку теста.
    #[test]
    fn every_executability_names_itself_in_the_api() {
        fn expected(value: SourceExecutability) -> &'static str {
            match value {
                SourceExecutability::Executable => "executable",
                SourceExecutability::IndicativePreviousClose => "indicative_previous_close",
                SourceExecutability::Unknown => "unknown",
            }
        }
        for value in [
            SourceExecutability::Executable,
            SourceExecutability::IndicativePreviousClose,
            SourceExecutability::Unknown,
        ] {
            assert_eq!(executability(value), expected(value));
        }
    }

    /// Причина, по которой позиция осталась без цены, — это то, что
    /// владелец увидит вместо суммы. Подменить её пустой строкой
    /// значит показать «цены нет» без объяснения, почему.
    ///
    /// Ожидаемое имя задаётся отдельным исчерпывающим `match`
    /// со строковыми литералами, а не через проверяемую функцию
    /// или метод домена. Поэтому тест ловит неверный код и обязан
    /// сломать сборку при добавлении нового варианта причины.
    /// Дополнительные проверки требуют непустых и различных кодов:
    /// одинаковый код скрывает конкретную причину непокрытия.
    #[test]
    fn every_uncovered_reason_names_itself_in_the_api() {
        use iaam_core::returns::{NotComputable, UncoveredReason};
        use std::collections::HashSet;

        fn expected(value: &UncoveredReason) -> &'static str {
            match value {
                UncoveredReason::NoObservation => "no_observation",
                UncoveredReason::TooOld => "too_old",
                UncoveredReason::AmbiguousVenue => "ambiguous_venue",
                UncoveredReason::AmbiguousCandidate => "ambiguous_candidate",
                UncoveredReason::NotComputable { reason } => match reason {
                    NotComputable::MissingPrice { .. } => "missing_price",
                    NotComputable::MissingFxRate { .. } => "missing_fx_rate",
                    NotComputable::QuotationBasisUnknown { .. } => "quotation_basis_unknown",
                    NotComputable::QuotationBasisContradictsEvidence { .. } => {
                        "quotation_basis_contradicts_evidence"
                    }
                    NotComputable::RemainingFaceUnknown { .. } => "remaining_face_unknown",
                    NotComputable::RemainingFaceAmbiguous { .. } => "remaining_face_ambiguous",
                    NotComputable::SolverRefused { .. } => "solver_refused",
                    NotComputable::NoExternalFlows => "no_external_flows",
                    NotComputable::StateNewerThanReport { .. } => "state_newer_than_report",
                    NotComputable::Numeric { .. } => "numeric",
                    NotComputable::UnsupportedFinancing { .. } => "unsupported_financing",
                    NotComputable::ScheduleMissing { .. } => "schedule_missing",
                    NotComputable::AccruedObservationMissing { .. } => {
                        "accrued_observation_missing"
                    }
                    NotComputable::CouponUndetermined { .. } => "coupon_undetermined",
                    NotComputable::OutsideScheduleCoverage { .. } => "outside_schedule_coverage",
                    NotComputable::OverlappingScheduleCoverage { .. } => {
                        "overlapping_schedule_coverage"
                    }
                    NotComputable::PrincipalUnknown => "principal_unknown",
                    NotComputable::ExitNotExecutable => "exit_not_executable",
                    NotComputable::NonPositiveDuration { .. } => "non_positive_duration",
                    NotComputable::NonPositiveInitialCapital => "non_positive_initial_capital",
                    NotComputable::NegativeTerminalWealth => "negative_terminal_wealth",
                    NotComputable::PrincipalStateAmbiguous { .. } => "principal_state_ambiguous",
                    NotComputable::AcquisitionBasisUnknown => "acquisition_basis_unknown",
                    NotComputable::AccruedInterestAtAcquisitionUnknown => {
                        "accrued_interest_at_acquisition_unknown"
                    }
                    NotComputable::HistoricalReceiptsUnknown => "historical_receipts_unknown",
                    NotComputable::CohortGap { .. } => "cohort_gap",
                    NotComputable::CurrencyMismatch { .. } => "currency_mismatch",
                    NotComputable::ExpenseUnknown => "expense_unknown",
                },
            }
        }

        let instrument = InstrumentId::new_random();
        let account = AccountId::new_random();
        let date = time::macros::date!(2026 - 08 - 28);
        let values = [
            UncoveredReason::NoObservation,
            UncoveredReason::TooOld,
            UncoveredReason::AmbiguousVenue,
            UncoveredReason::AmbiguousCandidate,
            UncoveredReason::NotComputable {
                reason: NotComputable::MissingPrice { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::MissingFxRate {
                    from: CurrencyCode::Rub,
                    to: CurrencyCode::Usd,
                    date,
                },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::QuotationBasisUnknown { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::QuotationBasisContradictsEvidence { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::RemainingFaceUnknown { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::RemainingFaceAmbiguous { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::SolverRefused {
                    refusal: SolverRefusal::TooFewFlows,
                },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::NoExternalFlows,
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::StateNewerThanReport {
                    last_event: date,
                    as_of: date,
                },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::Numeric { code: "overflow" },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::UnsupportedFinancing { account },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::ScheduleMissing { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::AccruedObservationMissing { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::CouponUndetermined { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::OutsideScheduleCoverage { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::OverlappingScheduleCoverage { instrument },
            },
            UncoveredReason::NotComputable {
                reason: NotComputable::ExitNotExecutable,
            },
        ];
        for value in &values {
            let expected = expected(value);
            assert_eq!(uncovered_reason(value), expected);
        }

        let codes: Vec<_> = values.iter().map(uncovered_reason).collect();
        assert!(
            codes.iter().all(|code| !code.is_empty()),
            "код причины не должен быть пустым"
        );
        let duplicate = codes
            .iter()
            .enumerate()
            .find_map(|(index, code)| codes[..index].contains(code).then_some(*code));
        let unique_codes = codes.iter().copied().collect::<HashSet<_>>();
        assert_eq!(
            unique_codes.len(),
            codes.len(),
            "код причины повторяется: {}",
            duplicate.unwrap_or("<неизвестен>")
        );
    }

    /// Время наблюдения уходит в отчёт строкой, а неизвестное время
    /// отсутствует в ответе вместо подстановки пустой строки.
    #[test]
    fn a_timestamp_travels_as_rfc_3339_in_utc() {
        let value =
            OffsetDateTime::from_unix_timestamp(1_787_000_000).expect("метка времени представима");
        assert_eq!(
            format_timestamp(Some(value)),
            Some("2026-08-17T20:53:20Z".to_owned())
        );
        assert_eq!(format_timestamp(None), None);
    }

    /// Сумма без валюты не бывает: если бы валюта была необязательной,
    /// пропущенное поле пришлось бы чем-то заменять — и заменялось бы
    /// оно рублём, потому что так удобнее.
    #[test]
    fn an_amount_without_a_currency_is_not_representable() {
        let raw = serde_json::json!({ "amount": "100.00" });
        assert!(serde_json::from_value::<AmountDto>(raw).is_err());
    }

    fn income_operation(kind: Option<IncomeKindDto>) -> OperationDto {
        OperationDto {
            account: Uuid::from_u128(3),
            kind: OperationKindDto::Income {
                instrument: Some(Uuid::from_u128(7)),
                amount: "1200.00".to_owned(),
                currency: CurrencyDto::Rub,
                kind,
            },
            dates: OperationDatesDto::default(),
            idempotency_key: None,
            source_operation_id: None,
        }
    }

    #[test]
    fn the_api_does_not_drop_the_income_kind() {
        // Журнал вид уже хранит: потерять его в транспорте значит
        // оставить внешнего агента без того, что система знает.
        assert!(matches!(
            income_operation(Some(IncomeKindDto::Coupon))
                .to_domain()
                .unwrap()
                .kind,
            OperationKind::Income {
                kind: Some(IncomeKind::Coupon),
                ..
            }
        ));
        assert!(matches!(
            income_operation(Some(IncomeKindDto::DepositInterest))
                .to_domain()
                .unwrap()
                .kind,
            OperationKind::Income {
                kind: Some(IncomeKind::DepositInterest),
                ..
            }
        ));
    }

    #[test]
    fn an_income_without_a_kind_stays_without_one() {
        // Отсутствие поля означает «не утверждалось», а не «дивиденд».
        assert!(matches!(
            income_operation(None).to_domain().unwrap().kind,
            OperationKind::Income { kind: None, .. }
        ));
    }

    #[test]
    fn an_income_kind_survives_a_json_round_trip() {
        let dto = income_operation(Some(IncomeKindDto::Dividend));
        let text = serde_json::to_string(&dto).unwrap();
        assert!(text.contains(r#""kind":"dividend""#), "{text}");
        let restored: OperationDto = serde_json::from_str(&text).unwrap();
        assert!(matches!(
            restored.to_domain().unwrap().kind,
            OperationKind::Income {
                kind: Some(IncomeKind::Dividend),
                ..
            }
        ));
    }

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
    fn a_missing_accrued_observation_serialises_with_a_distinct_reason() {
        let instrument = InstrumentId::new_random();
        let dto = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::AccruedObservationMissing { instrument },
        });

        assert_eq!(
            dto.not_computable.as_deref(),
            Some("accrued_observation_missing")
        );
        assert!(
            dto.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("наблюдения НКД"))
        );
    }
    #[test]
    fn overlapping_accrual_serialises_with_a_distinct_reason() {
        let instrument = InstrumentId::new_random();
        let dto = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::OverlappingScheduleCoverage { instrument },
        });

        assert_eq!(
            dto.not_computable.as_deref(),
            Some("overlapping_schedule_coverage")
        );
        assert!(
            dto.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("несколькими периодами"))
        );
    }
    #[test]
    fn an_accrued_mismatch_issue_names_totals_currencies_and_quantity() {
        let text = issue(&MaterialIssue::AccruedInterestMismatch {
            instrument: InstrumentId::new_random(),
            computed: Dec::new(Decimal::new(1_517, 2)),
            computed_currency: CurrencyCode::Usd,
            observed: Dec::new(Decimal::new(2_240, 2)),
            observed_currency: CurrencyCode::Rub,
            quantity: iaam_core::money::Quantity(Dec::new(Decimal::new(100, 0))),
            date: time::macros::date!(2026 - 08 - 26),
        });

        assert!(text.contains("15.17 USD"));
        assert!(text.contains("22.40 RUB"));
        assert!(text.contains("100"));
    }

    #[test]
    fn zero_reinvestment_note_is_present_in_serialized_metrics_body() {
        let computed =
            iaam_core::returns::zero_reinvestment::zero_reinvestment_metrics(
                Vec::new(),
                iaam_core::money::CalcMoney::new(
                    Dec::new(Decimal::new(100, 0)),
                    CurrencyCode::Rub,
                ),
                time::macros::date!(2026 - 01 - 01),
                time::macros::date!(2027 - 01 - 01),
            );
        let Computed::Value(metrics) = computed else {
            panic!("простые метрики должны вычисляться");
        };
        let json = serde_json::to_value(ZeroReinvestmentMetricsDto::from_domain(&metrics))
            .expect("метрики представимы в JSON");
        let note = json["zero_reinvestment_note"]
            .as_str()
            .expect("допущение должно быть строкой в теле");
        assert!(!note.is_empty());
        assert!(note.contains("купоны"));
        assert!(note.contains("реинвестировать"));
    }

    #[test]
    fn an_unknown_termination_value_serialises_with_a_reason_not_a_zero() {
        let dto = BondPositionAttributesDto::from_domain(&BondPositionAttributes {
            account: AccountId::new_random(),
            custody: None,
            instrument: InstrumentId::new_random(),
            accrued_interest: Computed::Value(Dec::new(Decimal::from_str_exact("15.17").unwrap())),
            accrued_interest_payable_on_termination: Computed::NotComputable {
                reason: NotComputable::ExitNotExecutable,
            },
            next_posting_date: Some(time::macros::date!(2026 - 12 - 02)),
            next_principal_return_finality: Some(
                iaam_core::bond::finality::PrincipalReturnFinality::Final,
            ),
        });
        let json = serde_json::to_value(&dto).unwrap();
        assert!(json["accrued_interest_payable_on_termination"]["value"].is_null());
        assert_eq!(
            json["accrued_interest_payable_on_termination"]["not_computable"],
            "exit_not_executable"
        );
        assert_eq!(json["next_posting_date"], "2026-12-02");
        assert_eq!(json["next_principal_return_finality"], "final");
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

    #[test]
    fn the_wire_explains_where_the_money_came_from() {
        let provenance = PriceProvenance {
            price_kind: Some("legal_close".to_owned()),
            origin: PriceOrigin::OwnerAsserted,
            venue: Some("moex".to_owned()),
            quotation_basis: iaam_core::valuation::QuotationBasis::PercentOfRemainingFace,
            basis_evidence: "iss:engines/stock/markets/bonds".to_owned(),
            observed_at: Some(time::macros::datetime!(2026-08-26 08:00:00 UTC)),
            valuation_policy_version: 1,
            source_priority_version: 1,
            carry_forward_limit: 10,
            price_max_age: 30,
        };

        let dto = PriceProvenanceDto::from_domain(&provenance);
        assert_eq!(
            dto.quotation_basis,
            QuotationBasisDto::PercentOfRemainingFace
        );
        assert_eq!(
            dto.basis_evidence.as_deref(),
            Some("iss:engines/stock/markets/bonds")
        );
    }
    /// Отображение доменного статуса в DTO — место, где `Proven`
    /// и `Contradicts` можно переставить местами, не сломав ни сборку,
    /// ни один существующий тест: наружу оба уходят строкой, и до сих
    /// пор через `from_domain` проходил только `NotProven`. Перепутанная
    /// пара назвала бы противоречивую запись доказанной — то есть ровно
    /// то, что витрина обязана различать.
    #[test]
    fn отображение_доменного_статуса_в_dto_не_путает_ветви() {
        use iaam_app::scenarios::market_reference::QuotationBasisStatus;

        assert_eq!(
            QuotationBasisStatusDto::from_domain(QuotationBasisStatus::Proven),
            QuotationBasisStatusDto::Proven
        );
        assert_eq!(
            QuotationBasisStatusDto::from_domain(QuotationBasisStatus::Contradicts),
            QuotationBasisStatusDto::Contradicts
        );
        assert_eq!(
            QuotationBasisStatusDto::from_domain(QuotationBasisStatus::NotProven),
            QuotationBasisStatusDto::NotProven
        );
    }

    #[test]
    fn каждый_статус_основания_называет_себя_в_api() {
        use std::collections::HashSet;

        let values = [
            QuotationBasisStatusDto::Proven,
            QuotationBasisStatusDto::Contradicts,
            QuotationBasisStatusDto::NotProven,
        ];
        let expected = ["proven", "contradicts", "not_proven"];
        let codes: Vec<_> = values
            .iter()
            .map(|value| {
                serde_json::to_value(value)
                    .expect("статус представим в JSON")
                    .as_str()
                    .expect("код статуса — строка")
                    .to_owned()
            })
            .collect();

        for (value, expected) in codes.iter().zip(expected) {
            assert_eq!(value, expected);
        }
        let duplicate = codes
            .iter()
            .enumerate()
            .find_map(|(index, code)| codes[..index].contains(code).then_some(code));
        let unique_codes = codes.iter().collect::<HashSet<_>>();
        assert_eq!(
            unique_codes.len(),
            codes.len(),
            "код статуса повторяется: {}",
            duplicate
                .map(|code| code.as_str())
                .unwrap_or("<неизвестен>")
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

// ---------------------------------------------------------------------
// Журнальные факты: корпоративные действия и оферта (§4.7, §3.5).
//
// Отдельный вход, а не новые члены `OperationKindDto`. Причина
// механическая: у корпоративного действия дата фиксации реестра — часть
// факта, а операционная модель дат её выразить не умеет вовсе
// (`OperationDates` жёстко проставляет `entitlement: None`).
//
// Приёма произвольного `EventKind` здесь нет: вход принимает ровно те
// семьи, которые перечислены ниже.
// ---------------------------------------------------------------------

/// Сумма с валютой в транспорте.
///
/// Вложенным объектом, а не парой полей рядом: у замещения компенсация
/// необязательна, и плоская пара потребовала бы необязательной валюты —
/// то есть состояния «валюта без суммы», которого не бывает.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AmountDto {
    /// Десятичное число строкой: двоичная плавающая точка теряет копейки.
    #[schema(example = "1000.00")]
    pub amount: String,
    pub currency: CurrencyDto,
}

impl AmountDto {
    fn to_money(&self, field: &str) -> Result<Money, Rejection> {
        Ok(Money::new(
            PostedMinor::new(minor(&self.amount, self.currency, field)?),
            self.currency.to_domain(),
        ))
    }

    /// Величина на одну бумагу: не деньги счёта, а номинал, поэтому
    /// минорными единицами не меряется и округлению не подлежит.
    fn to_per_unit(&self, field: &str) -> Result<PerUnitAmount, Rejection> {
        Ok(PerUnitAmount::new(
            Dec::new(decimal(&self.amount, field)?),
            self.currency.to_domain(),
        ))
    }
}

/// Что сделали с дробной частью при замещении.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FractionalTreatmentDto {
    CashCompensated,
    RoundedDown,
    NotApplicable,
}

impl FractionalTreatmentDto {
    #[must_use]
    pub const fn to_domain(self) -> FractionalTreatment {
        match self {
            Self::CashCompensated => FractionalTreatment::CashCompensated,
            Self::RoundedDown => FractionalTreatment::RoundedDown,
            Self::NotApplicable => FractionalTreatment::NotApplicable,
        }
    }
}

/// Правило переноса налоговой стоимости при замещении.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BasisTransferRuleDto {
    CarryOver,
    Restart,
}

impl BasisTransferRuleDto {
    #[must_use]
    pub const fn to_domain(self) -> BasisTransferRule {
        match self {
            Self::CarryOver => BasisTransferRule::CarryOver,
            Self::Restart => BasisTransferRule::Restart,
        }
    }
}

/// Корпоративное действие в транспорте. Величины **положительные**:
/// знак выбытия ставит приёмка, а не клиент.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CorporateActionDto {
    /// Амортизация: номинал уменьшается, деньги приходят, количество
    /// бумаг не меняется.
    PartialRedemption {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        principal_returned_per_unit: AmountDto,
        compensation: AmountDto,
        #[serde(with = "iso_date")]
        #[schema(value_type = String, format = Date, example = "2026-05-20")]
        effective_date: Date,
        #[serde(
            default,
            with = "iso_date::option",
            skip_serializing_if = "Option::is_none"
        )]
        #[schema(value_type = Option<String>, format = Date)]
        record_date: Option<Date>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grounds: Option<String>,
    },
    /// Окончательное погашение: номинал возвращён целиком, бумага
    /// выбывает.
    Redemption {
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        principal_returned_per_unit: AmountDto,
        compensation: AmountDto,
        #[serde(with = "iso_date")]
        #[schema(value_type = String, format = Date)]
        effective_date: Date,
        #[serde(
            default,
            with = "iso_date::option",
            skip_serializing_if = "Option::is_none"
        )]
        #[schema(value_type = Option<String>, format = Date)]
        record_date: Option<Date>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grounds: Option<String>,
    },
    /// Замещение: бумага предшественника меняется на бумагу преемника.
    Conversion {
        predecessor: Uuid,
        successor: Uuid,
        custody: Uuid,
        /// Сколько бумаг преемника приходится на одну бумагу
        /// предшественника.
        ratio: String,
        quantity_in: String,
        quantity_out: String,
        fractional: FractionalTreatmentDto,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compensation: Option<AmountDto>,
        #[serde(with = "iso_date")]
        #[schema(value_type = String, format = Date)]
        effective_date: Date,
        #[serde(
            default,
            with = "iso_date::option",
            skip_serializing_if = "Option::is_none"
        )]
        #[schema(value_type = Option<String>, format = Date)]
        record_date: Option<Date>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grounds: Option<String>,
        basis_transfer: BasisTransferRuleDto,
    },
}

impl CorporateActionDto {
    fn to_domain(&self) -> Result<CorporateAction, Rejection> {
        Ok(match self {
            Self::PartialRedemption {
                instrument,
                custody,
                quantity,
                principal_returned_per_unit,
                compensation,
                effective_date,
                record_date,
                grounds,
            } => CorporateAction::PartialRedemption {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Quantity(Dec::new(decimal(quantity, "quantity")?)),
                principal_returned_per_unit: principal_returned_per_unit
                    .to_per_unit("principal_returned_per_unit")?,
                compensation: compensation.to_money("compensation")?,
                effective_date: *effective_date,
                record_date: *record_date,
                grounds: grounds.clone(),
            },
            Self::Redemption {
                instrument,
                custody,
                quantity,
                principal_returned_per_unit,
                compensation,
                effective_date,
                record_date,
                grounds,
            } => CorporateAction::Redemption {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Quantity(Dec::new(decimal(quantity, "quantity")?)),
                principal_returned_per_unit: principal_returned_per_unit
                    .to_per_unit("principal_returned_per_unit")?,
                compensation: compensation.to_money("compensation")?,
                effective_date: *effective_date,
                record_date: *record_date,
                grounds: grounds.clone(),
            },
            Self::Conversion {
                predecessor,
                successor,
                custody,
                ratio,
                quantity_in,
                quantity_out,
                fractional,
                compensation,
                effective_date,
                record_date,
                grounds,
                basis_transfer,
            } => CorporateAction::Conversion {
                predecessor: InstrumentId(*predecessor),
                successor: InstrumentId(*successor),
                custody: CustodyId(*custody),
                ratio: Dec::new(decimal(ratio, "ratio")?),
                quantity_in: Quantity(Dec::new(decimal(quantity_in, "quantity_in")?)),
                quantity_out: Quantity(Dec::new(decimal(quantity_out, "quantity_out")?)),
                fractional: fractional.to_domain(),
                compensation: match compensation {
                    Some(amount) => Some(amount.to_money("compensation")?),
                    None => None,
                },
                effective_date: *effective_date,
                record_date: *record_date,
                grounds: grounds.clone(),
                basis_transfer: basis_transfer.to_domain(),
            },
        })
    }
}

/// Факт оферты в транспорте.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OfferExerciseDto {
    /// Поданная заявка: ни денег, ни бумаг она не двигает.
    Submitted {
        submission: Uuid,
        /// Идентификатор окна выводится из `(instrument, execution_date)`.
        /// Произвольный UUID приведёт к неразрешённой заявке; старые факты
        /// не валидируются повторно, поскольку журнал append-only.
        window: Uuid,
        instrument: Uuid,
        quantity: String,
    },
    /// Отзыв заявки целиком или частично.
    Cancelled { submission: Uuid, quantity: String },
    /// Совершённый выкуп: бумага выбывает за деньги.
    Settled {
        submission: Uuid,
        instrument: Uuid,
        custody: Uuid,
        quantity: String,
        gross: AmountDto,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fee: Option<AmountDto>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accrued_interest: Option<AmountDto>,
    },
}

impl OfferExerciseDto {
    fn to_domain(&self) -> Result<OfferExerciseAction, Rejection> {
        Ok(match self {
            Self::Submitted {
                submission,
                window,
                instrument,
                quantity,
            } => OfferExerciseAction::Submitted {
                submission: OfferSubmissionId(*submission),
                window: OfferWindowId(*window),
                instrument: InstrumentId(*instrument),
                quantity: Quantity(Dec::new(decimal(quantity, "quantity")?)),
            },
            Self::Cancelled {
                submission,
                quantity,
            } => OfferExerciseAction::Cancelled {
                submission: OfferSubmissionId(*submission),
                quantity: Quantity(Dec::new(decimal(quantity, "quantity")?)),
            },
            Self::Settled {
                submission,
                instrument,
                custody,
                quantity,
                gross,
                fee,
                accrued_interest,
            } => OfferExerciseAction::Settled {
                submission: OfferSubmissionId(*submission),
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Quantity(Dec::new(decimal(quantity, "quantity")?)),
                gross: gross.to_money("gross")?,
                fee: match fee {
                    Some(amount) => Some(amount.to_money("fee")?),
                    None => None,
                },
                accrued_interest: match accrued_interest {
                    Some(amount) => Some(amount.to_money("accrued_interest")?),
                    None => None,
                },
            },
        })
    }
}

/// Журнальный факт: корпоративное действие или оферта.
///
/// Две семьи под одной крышей — это общий канал приёмки, а не общая
/// природа: корпоративное действие решает эмитент, оферту предъявляет
/// владелец (`iaam-core/src/event/offer.rs`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalFactDto {
    /// Даты внутри самого факта: дата вступления в силу — часть его
    /// идентичности, а не свойство подачи.
    CorporateAction { action: CorporateActionDto },
    /// У оферты собственной даты нет, поэтому день присылает клиент:
    /// выдумать его приёмке нечем.
    OfferExercise {
        action: OfferExerciseDto,
        #[serde(with = "iso_date")]
        #[schema(value_type = String, format = Date, example = "2026-04-20")]
        day: Date,
    },
}

/// Один журнальный факт в пачке.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JournalEventDto {
    pub account: Uuid,
    /// Плоско, как у операции: клиент одного API не должен помнить,
    /// что у одного входа вид факта лежит в корне, а у соседнего —
    /// во вложенном объекте.
    #[serde(flatten)]
    pub fact: JournalFactDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_operation_id: Option<String>,
}

impl JournalEventDto {
    /// Единственное место, где транспорт журнального факта встречается
    /// с доменом. Отказ возвращается с полем, ожидаемым и полученным —
    /// это тело ответа `422` (§13).
    pub fn to_domain(&self) -> Result<SubmittedJournalEvent, Rejection> {
        let fact = match &self.fact {
            JournalFactDto::CorporateAction { action } => {
                JournalFact::CorporateAction(action.to_domain()?)
            }
            JournalFactDto::OfferExercise { action, day } => JournalFact::OfferExercise {
                action: action.to_domain()?,
                day: *day,
            },
        };
        Ok(SubmittedJournalEvent {
            account: AccountId(self.account),
            fact,
            idempotency_key: self.idempotency_key.clone(),
            source_operation_id: self.source_operation_id.clone(),
        })
    }
}

/// Запрос приёмки журнальных фактов.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubmitJournalEventsRequest {
    /// Метка источника: ручной ввод, конкретный агент, конкретный файл.
    pub source_label: String,
    pub events: Vec<JournalEventDto>,
}
