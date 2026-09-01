//! Transport representations (§3.2).
//!
//! DTOs live here and never move into the common crate: a common crate
//! of types quickly becomes a dumping ground, and the formally independent core
//! ends up depending on the layer that knows about everything.
//!
//! **Amounts are sent as decimal strings**, not floating-point
//! numbers: the JSON number `0.1` in binary floating point is not equal to one
//! tenth, and a monetary amount passed through it ceases to be a fact.

use std::fmt;

use iaam_app::ingest::journal_event::{JournalFact, SubmittedJournalEvent};
use iaam_app::ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use iaam_app::ingest::{Rejection, Verdict};
use iaam_app::ports::{
    BrokerAccessView, BrokerEnvironment, CategoryRuleView, CategoryView, ClassificationRuleView,
    IssuedToken, Scope, TokenView,
};
use iaam_app::scenarios::categories::{CategoryMove, CategoryRuleImpact, MonthlyImpact};
use iaam_app::scenarios::reports::{AccountBalanceRow, MoneyFlowReport};
use iaam_core::bond::offer::OfferChoice;
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{FeeOrigin, IncomeKind, TaxOrigin};
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::money_flow::MoneyFlowError;
use iaam_core::reconciliation::{Dimension, ReconciliationStatus};
use iaam_core::returns::zero_reinvestment::{
    BondScenarioResult, IrrLabel, LifetimeCohortMetric, ProspectiveMetric, ZeroReinvestmentMetrics,
};
use iaam_core::returns::{
    AmountQualification, BondPositionAttributes, Computed, DataQuality, EvaluatedPosition,
    ExecutabilityShares, LiquidationEstimate, MaterialIssue, NotComputable, PositionCoverage,
    ReturnsReport, UncoveredPosition,
};
use iaam_core::rules::{ExpectedPosting, PostingKind};
use iaam_core::valuation::{
    PriceFreshness, PriceOrigin, PriceProvenance, PriceQuality, PriceSelection, QuotationBasis,
    SelectedPrice, SourceExecutability,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

// Custom date format: the standard serialisation of `time::Date` is not
// a «YYYY-MM-DD» string, and without this line the API would accept dates
// in an unpredictable format. Verified by execution: without it, body parsing
// fails with «invalid type: string "2025-01-01", expected a `Date`».
time::serde::format_description!(iso_date, Date, "[year]-[month]-[day]");

/// Transport currency code. A separate type because the core's `CurrencyCode`
/// knows nothing about OpenAPI and should not.
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

/// Transport price quality.
///
/// Values we have already computed — carry-forward to a non-working day and
/// threshold-based staleness — are not representable inputs: they are conclusions
/// of valuation policy, not what the source asserts. Recording them as facts
/// would erase the distinction between an observation and our conclusion
/// (docs/decisions/0002-polnota-ocenki-i-ispolnimost-ceny-dve-osi.md).
/// Domain PriceQuality is broader: it must be able to read the old journal.
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

/// Transport income type.
///
/// There is no «other» variant, just as in the core: a bucket that cannot
/// support a decision is indistinguishable from ignorance, and ignorance is
/// represented by the field's absence.
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

/// Transport fee provenance.
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

/// Transport tax provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaxOriginDto {
    WithheldAtSource,
    SelfPaid,
}

impl TaxOriginDto {
    #[must_use]
    pub const fn to_domain(self) -> TaxOrigin {
        match self {
            Self::WithheldAtSource => TaxOrigin::WithheldAtSource,
            Self::SelfPaid => TaxOrigin::SelfPaid,
        }
    }
}

/// Operation dates.
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

/// Owner's confidence in the quantity of the reconstructed position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CertaintyDto {
    Known,
    #[default]
    Estimated,
}

impl CertaintyDto {
    fn to_domain(self) -> iaam_core::event::kind::Certainty {
        match self {
            Self::Known => iaam_core::event::kind::Certainty::Known,
            Self::Estimated => iaam_core::event::kind::Certainty::Estimated,
        }
    }
}

/// Owner's confidence in the acquisition date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DateCertaintyDto {
    Known,
    Estimated,
    #[default]
    Unknown,
}

impl DateCertaintyDto {
    fn to_domain(self) -> iaam_core::event::kind::DateCertainty {
        match self {
            Self::Known => iaam_core::event::kind::DateCertainty::Known,
            Self::Estimated => iaam_core::event::kind::DateCertainty::Estimated,
            Self::Unknown => iaam_core::event::kind::DateCertainty::Unknown,
        }
    }
}

/// Confidence that the tax basis is documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BasisCertaintyDto {
    Documented,
    Estimated,
    #[default]
    Unknown,
}

impl BasisCertaintyDto {
    fn to_domain(self) -> iaam_core::event::kind::BasisCertainty {
        match self {
            Self::Documented => iaam_core::event::kind::BasisCertainty::Documented,
            Self::Estimated => iaam_core::event::kind::BasisCertainty::Estimated,
            Self::Unknown => iaam_core::event::kind::BasisCertainty::Unknown,
        }
    }
}

/// Owner's ternary assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TristateDto {
    Yes,
    No,
    #[default]
    Unknown,
}

impl TristateDto {
    fn to_domain(self) -> iaam_core::event::kind::Tristate {
        match self {
            Self::Yes => iaam_core::event::kind::Tristate::Yes,
            Self::No => iaam_core::event::kind::Tristate::No,
            Self::Unknown => iaam_core::event::kind::Tristate::Unknown,
        }
    }
}

/// Whether a fact asserted by the owner is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeDto {
    Known,
    #[default]
    Unknown,
}

impl KnowledgeDto {
    fn to_domain(self) -> iaam_core::event::kind::Knowledge {
        match self {
            Self::Known => iaam_core::event::kind::Knowledge::Known,
            Self::Unknown => iaam_core::event::kind::Knowledge::Unknown,
        }
    }
}

/// Owner's assertions about the reconstructed opening (§10.7).
///
/// An absent `assertions` field means that the owner did not
/// assert anything; default values preserve this lack of knowledge rather than
/// infer confidence from whether other fields are populated.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct OpeningAssertionsDto {
    #[serde(default)]
    pub quantity: CertaintyDto,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub acquisition_date: Option<Date>,
    #[serde(default)]
    pub acquisition_date_certainty: DateCertaintyDto,
    #[serde(default)]
    pub tax_basis: BasisCertaintyDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_currency: Option<CurrencyDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_rate: Option<String>,
    #[serde(default)]
    pub fees_included: TristateDto,
    #[serde(default)]
    pub ldv_eligibility: KnowledgeDto,
    #[serde(default)]
    pub prior_corporate_actions: KnowledgeDto,
}

impl OpeningAssertionsDto {
    fn to_domain(&self) -> Result<iaam_core::event::kind::OpeningAssertions, Rejection> {
        let basis_rate = self
            .basis_rate
            .as_ref()
            .map(|value| decimal(value, "assertions.basis_rate").map(Dec::new))
            .transpose()?;
        Ok(iaam_core::event::kind::OpeningAssertions {
            quantity: self.quantity.to_domain(),
            acquisition_date: self.acquisition_date,
            acquisition_date_certainty: self.acquisition_date_certainty.to_domain(),
            tax_basis: self.tax_basis.to_domain(),
            basis_currency: self.basis_currency.map(CurrencyDto::to_domain),
            basis_rate,
            fees_included: self.fees_included.to_domain(),
            ldv_eligibility: self.ldv_eligibility.to_domain(),
            prior_corporate_actions: self.prior_corporate_actions.to_domain(),
        })
    }
}

/// Operation type. Values are **positive**: the type determines the sign, not the client.
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
    /// Money a counterparty returned. It reverses spending rather than adding
    /// to income: a returned purchase must not appear as money arriving.
    Refund {
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
        /// Type of income. An absent field means «not asserted»:
        /// without it, the API would continue to lose the type from a journal that
        /// already knows how to store it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<IncomeKindDto>,
    },
    Fee {
        amount: String,
        currency: CurrencyDto,
        origin: FeeOriginDto,
    },
    Tax {
        amount: String,
        currency: CurrencyDto,
        origin: TaxOriginDto,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assertions: Option<OpeningAssertionsDto>,
    },
    Valuation {
        instrument: Uuid,
        price: String,
        currency: CurrencyDto,
        quality: PriceQualityDto,
    },
}

/// Complete operation.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn decimal(value: &str, field: &str) -> Result<Decimal, Rejection> {
    value.parse::<Decimal>().map_err(|_| Rejection {
        field: field.to_owned(),
        expected: "a decimal number represented as a string".into(),
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
    /// Conversion to a domain operation.
    ///
    /// The only place where transport meets the domain. A rejection
    /// is returned with the field, the expected value and the received value — this is the response body
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
            source_time: None,
            idempotency_key: self.idempotency_key.clone(),
            source_operation_id: self.source_operation_id.clone(),
            source_category: self.source_category.clone(),
            description: self.description.clone(),
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
            OperationKindDto::Refund { amount, currency } => OperationKind::Refund {
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
                basis_fee: None,
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
                basis_fee: None,
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
            OperationKindDto::Tax {
                amount,
                currency,
                origin,
            } => OperationKind::Tax {
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
                assertions,
            } => OperationKind::OpeningPosition {
                instrument: InstrumentId(*instrument),
                custody: CustodyId(*custody),
                quantity: Dec::new(decimal(quantity, "quantity")?),
                cost_basis_minor: optional_minor(cost_basis.as_ref(), *currency, "cost_basis")?,
                currency: currency.to_domain(),
                assertions: assertions
                    .as_ref()
                    .map(OpeningAssertionsDto::to_domain)
                    .transpose()?,
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

/// The source the caller declares for this batch.
///
/// Without it the server mints a random source per request, and nothing
/// deduplicates across requests: a corrected re-submission would add a second
/// set of rows rather than replace the first (spec §6).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeclaredSourceDto {
    /// Account the rows belong to.
    pub account: Uuid,
    /// How the rows arrived: `file`, `paste`, `manual`.
    pub channel: String,
}

/// Intake request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubmitOperationsRequest {
    /// Source label: manual input, a specific agent, a specific file.
    pub source_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DeclaredSourceDto>,
    pub operations: Vec<OperationDto>,
}

/// Acknowledgement required before retracting affected trades without live broker access.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CustodyRepairRequest {
    /// Acknowledge that retracted facts may not be restored by a subsequent synchronisation.
    #[serde(default)]
    pub acknowledge_without_live_access: bool,
}

/// Which case the account was in when the repair ran.
///
/// An enum rather than a free string: the caller decides what to do next from this
/// value, and a schema that does not enumerate the cases leaves them to be guessed
/// from prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CustodyRepairCaseDto {
    /// Affected trades exist and an unrevoked broker access can restore them.
    AffectedWithLiveAccess,
    /// Affected trades exist and no unrevoked broker access can restore them.
    AffectedWithoutLiveAccess,
    /// Nothing was left to repair.
    NothingAffected,
}

/// Outcome of repairing account-derived custody facts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct CustodyRepairOutcomeDto {
    pub case: CustodyRepairCaseDto,
    pub affected_trades: usize,
    /// Reversed by an earlier run: a repeat run reports these and writes nothing.
    pub already_reversed: usize,
    /// Written by this run. A partial run reports what it managed, rather than
    /// leaving the caller to infer it.
    pub written: usize,
}

impl CustodyRepairOutcomeDto {
    #[must_use]
    pub fn from_domain(outcome: iaam_app::scenarios::custody_repair::CustodyRepairOutcome) -> Self {
        use iaam_app::scenarios::custody_repair::CustodyRepairCase;
        let case = match outcome.case {
            CustodyRepairCase::AffectedWithLiveAccess => {
                CustodyRepairCaseDto::AffectedWithLiveAccess
            }
            CustodyRepairCase::AffectedWithoutLiveAccess => {
                CustodyRepairCaseDto::AffectedWithoutLiveAccess
            }
            CustodyRepairCase::NothingAffected => CustodyRepairCaseDto::NothingAffected,
        };
        Self {
            case,
            affected_trades: outcome.affected_trades,
            already_reversed: outcome.already_reversed,
            written: outcome.written,
        }
    }
}

/// Verdict for a single operation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerdictDto {
    /// One-based operation number in the input batch.
    pub row: usize,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    /// Existing event resembling a newly recorded possible duplicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub of_event_id: Option<Uuid>,
    /// Deduplication hierarchy level that produced the possible-duplicate hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The account concerned. Populated for reconciliation verdicts:
    /// a discrepancy without an account is an instruction to «look somewhere».
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    /// The dimension for which values do not match or there is nothing to reconcile (§10.3).
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
            of_event_id: None,
            level: None,
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
            Verdict::PossibleDuplicate { event, of, level } => Self {
                event_id: Some(event.inner()),
                of_event_id: Some(of.inner()),
                level: Some(level.number()),
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
            Verdict::Quarantined { reason } => Self {
                detail: Some(reason.clone()),
                ..base
            },
        }
    }
}

/// A value that the system may have declined to calculate.
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
            format!("no price for instrument {}", instrument.inner())
        }
        NotComputable::MissingFxRate { from, to, date } => {
            format!("no exchange rate {}→{} on {date}", from.code(), to.code())
        }
        NotComputable::QuotationBasisUnknown { instrument } => {
            format!(
                "unknown quotation basis for instrument {}",
                instrument.inner()
            )
        }
        NotComputable::QuotationBasisContradictsEvidence { instrument } => {
            format!(
                "quotation basis contradicts the evidence for instrument {}",
                instrument.inner()
            )
        }
        NotComputable::RemainingFaceUnknown { instrument } => {
            format!(
                "remaining face value is unknown for instrument {}",
                instrument.inner()
            )
        }
        NotComputable::SolverRefused { refusal } => refusal.to_string(),
        NotComputable::NoExternalFlows => "no flows crossing the perimeter boundary".into(),
        NotComputable::StateNewerThanReport { last_event, as_of } => {
            format!("snapshot contains events only up to {last_event}, report is as of {as_of}")
        }
        NotComputable::Numeric { code } => format!("arithmetic failure: {code}"),
        NotComputable::UnsupportedFinancing { account } => format!(
            "the account contains funding from outside the perimeter: {}",
            account.inner()
        ),
        NotComputable::ScheduleMissing { instrument } => {
            format!("no issue schedule for instrument {}", instrument.inner())
        }
        NotComputable::AccruedObservationMissing { instrument } => {
            format!(
                "no accrued interest observation for instrument {}",
                instrument.inner()
            )
        }
        NotComputable::CouponUndetermined { instrument } => {
            format!(
                "coupon amount is undefined for instrument {}",
                instrument.inner()
            )
        }
        NotComputable::OutsideScheduleCoverage { instrument } => {
            format!(
                "report date is outside the schedule coverage for instrument {}",
                instrument.inner()
            )
        }
        NotComputable::OverlappingScheduleCoverage { instrument } => {
            format!(
                "report date is covered by multiple schedule periods for instrument {}",
                instrument.inner()
            )
        }
        NotComputable::ExitNotExecutable => {
            "no executable exit for realising accrued interest".to_owned()
        }
        NotComputable::PrincipalUnknown => {
            "face value for converting the quotation is unknown".into()
        }
        NotComputable::NonPositiveDuration {
            coordinate,
            terminal_date,
        } => {
            format!("terminal date {terminal_date} is not later than coordinate {coordinate}")
        }
        NotComputable::NonPositiveInitialCapital => "initial value is not positive".into(),
        NotComputable::NegativeTerminalWealth => "terminal wealth is negative".into(),
        NotComputable::AcquisitionBasisUnknown => "historical acquisition cost is unknown".into(),
        NotComputable::AccruedInterestAtAcquisitionUnknown => {
            "accrued interest paid on acquisition is unknown".into()
        }
        NotComputable::HistoricalReceiptsUnknown => "historical receipts are unknown".into(),
        NotComputable::CohortGap { gap } => gap.to_string(),
        NotComputable::CurrencyMismatch { expected, actual } => {
            format!(
                "currencies do not match: {} and {}",
                expected.code(),
                actual.code()
            )
        }
        NotComputable::ExpenseUnknown => "expense is unknown".into(),
    }
}

/// Printing an approximate value.
///
/// `f64` cannot be printed as-is: the final digits of binary floating
/// point are noise, not the result, and vary between platforms.
/// Eight decimal places are four orders of magnitude more precise than the solver tolerance (1e-9
/// on the NPV residual) and exactly as precise as a rate can meaningfully be:
/// 0,00000001 is one millionth of a percentage point per annum.
fn format_rate(value: f64) -> String {
    let scaled = (value * 1e8).round();
    // −0 and 0 are the same number, but are printed differently.
    let normalized = if scaled == 0.0 { 0.0 } else { scaled / 1e8 };
    format!("{normalized:.8}")
}

/// A rate of return together with the solver policy.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RateDto {
    /// The rate as a decimal fraction. An approximate value: it does not enter
    /// into monetary identities (§6.6).
    pub value: String,
    pub error_bound: String,
    pub iterations: u32,
    pub day_count: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_computable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A calculated monetary value together with its currency.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CalcMoneyDto {
    /// The calculated decimal value as a string.
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

/// A calculated monetary value that may not be computable.
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

/// One expected payment in the zero-reinvestment scenario.
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

/// Expected payment type.
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

const ZERO_REINVESTMENT_NOTE: &str = "Coupons received and principal repayments are assumed to be held until the end of the term without earning a return; if they are spent or reinvested, there is no single terminal capital figure — only the payment schedule and IRR remain.";
/// Five quantities from §7.1 for a single scenario.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ZeroReinvestmentMetricsDto {
    pub postings: Vec<ExpectedPostingDto>,
    pub terminal_wealth: CalcMoneyDto,
    pub surplus: CalcMoneyDto,
    pub hpr: ComputedDto,
    pub cagr_0r: RateDto,
    /// Coupons received and principal repayments are assumed to be held until the end
    /// of the term without earning a return; if they are spent or reinvested,
    /// there is no single terminal capital figure — only the payment schedule
    /// and IRR remain.
    pub zero_reinvestment_assumed: bool,
    /// Explanation of the assumption shown alongside the calculated quantities.
    pub zero_reinvestment_note: String,
    /// Pre-tax series; tax policy will be added in E5.
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

/// Scenario metrics that may not be fully computable.
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

/// Scenario of holding to maturity or tendering under a put offer.
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
            OfferChoice::ExerciseAtOffer { window } => Self::ExerciseAtOffer { window: window.0 },
        }
    }
}

/// Rate label on the prospective coordinate.
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

/// Prospective metric from the reporting date.
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

/// Lifetime metric for a single cohort.
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
    /// Historical IRR is unavailable because past payments are aggregated
    /// without dates.
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

/// Metrics for a single bond position under a single scenario.
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

/// Lifetime metrics may be entirely unavailable because of a gap in the history.
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

/// Metrics for all scenarios of a single bond position.
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

fn rate_dto(
    value: &Computed<iaam_core::numeric::xirr::RateOutcome>,
    fallback_day_count: &str,
) -> RateDto {
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

/// Selected position price with policy conclusions and provenance.
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

/// Method by which the policy selected the observation.
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

/// Freshness of the selected observation relative to the policy threshold.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PriceFreshnessDto {
    Fresh,
    Stale { days: u16 },
}

/// Origin of the selected observation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PriceOriginDto {
    Market { venue: String, price_kind: String },
    ReportParsed { source: Uuid },
    OwnerAsserted,
}

/// Unit in which the source quoted the price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotationBasisDto {
    MoneyPerUnit,
    PercentOfRemainingFace,
    /// The source did not establish the basis: this row's price is not
    /// converted into money.
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

/// Status indicating whether the recorded price basis has been established.
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

/// Selection basis: source type, venue, versions and both thresholds.
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

/// Position valued using the selected observation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluatedPositionDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub quantity: String,
    pub price: SelectedPriceDto,
}

/// Position without a selected price and the reason for rejection.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UncoveredPositionDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub reason: String,
}

/// Position that retained its previous computed quality.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LegacyDerivedPositionDto {
    pub account: Uuid,
    pub custody: Option<Uuid>,
    pub instrument: Uuid,
    pub quality: String,
}

/// Price coverage without a fabricated percentage of value.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PositionCoverageDto {
    pub evaluated_positions: u32,
    pub total_positions: u32,
    pub selected: Vec<EvaluatedPositionDto>,
    pub uncovered: Vec<UncoveredPositionDto>,
    pub legacy_derived: Vec<LegacyDerivedPositionDto>,
}

/// Executability proportions by value of valued positions.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExecutabilitySharesDto {
    pub evaluated_positions_value: String,
    pub executable: String,
    pub indicative_previous_close: String,
    pub unknown: String,
}

/// Monetary amount with an explicit knowledge qualifier.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AmountQualificationDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub qualification: String,
}

/// Estimate before exit costs and tax.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LiquidationEstimateDto {
    pub value_before_exit_costs_and_tax: ComputedDto,
    pub executability: ExecutabilitySharesDto,
    pub exit_costs: AmountQualificationDto,
    pub tax: AmountQualificationDto,
    pub accrued_interest_payable_on_termination: ComputedDto,
}
/// Bond position attributes (§5.1).
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
/// Shares of portfolio value by confidence level (§10.5).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NavCoverageDto {
    pub accepted_independent: String,
    pub accepted_internal: String,
    pub provisional: String,
    pub discrepant: String,
}

/// Data quality block (§10.5).
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
            .expect("provenance timestamp should be formattable")
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

fn posting_kind(value: iaam_core::rules::PostingKind) -> &'static str {
    match value {
        iaam_core::rules::PostingKind::Coupon => "coupon",
        iaam_core::rules::PostingKind::PrincipalReturn => "principal_return",
        iaam_core::rules::PostingKind::OfferSettlement => "offer_settlement",
    }
}

fn issue(value: &MaterialIssue) -> String {
    match value {
        MaterialIssue::RestoredWithoutBasis { account } => format!(
            "account {} was reconstructed without a documented value",
            account.inner()
        ),
        MaterialIssue::AmortisationAllocationUnknown {
            account,
            instrument,
        } => format!(
            "the amortisation allocation share for instrument {} in account {} could not be derived: load a verified issue schedule",
            instrument.inner(),
            account.inner()
        ),
        MaterialIssue::NegativeCash { account, currency } => format!(
            "negative balance in account {} in {}",
            account.inner(),
            currency.code()
        ),
        MaterialIssue::HistoryStartsAt { date } => format!("history starts on {date}"),
        MaterialIssue::NoIndependentSource { account, dimension } => format!(
            "account {} has no independent confirmation of the {} measurement",
            account.inner(),
            dimension.code()
        ),
        MaterialIssue::Discrepancy { account, dimension } => format!(
            "account {} reconciliation for measurement {} does not balance",
            account.inner(),
            dimension.code()
        ),
        MaterialIssue::UnsupportedFinancing { account } => format!(
            "account {} contains funding outside the perimeter",
            account.inner()
        ),
        MaterialIssue::OfferWindowUnresolved { submission } => format!(
            "offer request {} refers to an unknown window",
            submission.inner()
        ),
        MaterialIssue::ScheduledPostingNotReceived {
            account,
            instrument,
            date,
            kind,
        } => format!(
            "payment {} for instrument {} in account {} for {} has not been confirmed",
            posting_kind(*kind),
            instrument.inner(),
            account.inner(),
            date
        ),
        MaterialIssue::ScheduledPostingUnverifiable {
            account,
            instrument,
            date,
            kind,
            reason,
        } => format!(
            "payment {} for instrument {} in account {} for {} cannot be reconciled: {}",
            posting_kind(*kind),
            instrument.inner(),
            account.inner(),
            date,
            reason.code()
        ),
        MaterialIssue::ScheduledPostingsUnverifiable {
            account,
            instrument,
            kind,
            reason,
            count,
            first_date,
            last_date,
        } => format!(
            "the {count} payments of type {} for instrument {} in account {} from {first_date} to {last_date} cannot be reconciled: {}",
            posting_kind(*kind),
            instrument.inner(),
            account.inner(),
            reason.code()
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
            "accrued coupon interest for instrument {} differs: calculated {} {} versus observed {} {} for quantity {} on {}",
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

/// Cash movement report over an inclusive interval.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MoneyFlowReportDto {
    pub contour: Uuid,
    pub contour_version: u32,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub from: Date,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub to: Date,
    pub category_rule_versions: Vec<u32>,
    pub currencies: Vec<MoneyFlowCurrencyDto>,
    /// Accounts whose own cash change the six quantities do not explain.
    pub unexplained: Vec<AccountResidualDto>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountResidualDto {
    pub account: Uuid,
    pub currency: CurrencyDto,
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MoneyFlowCurrencyDto {
    pub currency: CurrencyDto,
    pub came_in: String,
    pub went_out: String,
    pub earned_by_capital: String,
    pub moved_into_assets: String,
    pub fees: String,
    pub taxes: String,
    pub internal_transfers: String,
    pub cash_delta: String,
    pub residual: String,
    pub went_out_by_category: Vec<CategoryAmountDto>,
    /// What the capital earned, split by what produced it. Sums to
    /// `earned_by_capital`: the same amounts along another axis, never a second
    /// set of figures.
    pub earned_by_capital_by_source: Vec<EarningSourceAmountDto>,
    pub not_decomposed: NotDecomposedDto,
}

/// One source of earnings and what it produced over the interval.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EarningSourceAmountDto {
    /// Which account produced it. For a deposit or a savings account this is
    /// the asset itself.
    pub account: Uuid,
    /// Which security produced it, where one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instrument: Option<Uuid>,
    /// Which income category the owner's rules put it in — cashback, interest
    /// on a balance, a coupon. Absent means no rule covers it: an undecomposed
    /// earning is its own line, never folded into a bucket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<Uuid>,
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryAmountDto {
    pub category: Uuid,
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NotDecomposedDto {
    pub count: u64,
    pub amount: String,
}

impl MoneyFlowReportDto {
    pub fn from_domain(report: &MoneyFlowReport) -> Result<Self, MoneyFlowError> {
        let currencies = report
            .flow
            .currencies()
            .map(|currency| {
                let went_out_by_category = report
                    .flow
                    .went_out_by_category(currency)?
                    .into_iter()
                    .map(|(category, amount)| CategoryAmountDto {
                        category: category.inner(),
                        amount: amount.to_calc_dec().inner().to_string(),
                    })
                    .collect();
                let earned_by_capital_by_source = report
                    .flow
                    .earned_by_capital_by_source(currency)?
                    .into_iter()
                    .map(|(source, amount)| EarningSourceAmountDto {
                        account: source.account.inner(),
                        instrument: source.instrument.map(|id| id.inner()),
                        category: source.category.map(|category| category.inner()),
                        amount: amount.to_calc_dec().inner().to_string(),
                    })
                    .collect();
                let (count, amount) = report.flow.not_decomposed(currency)?;
                Ok(MoneyFlowCurrencyDto {
                    currency: CurrencyDto::from_domain(currency),
                    came_in: report
                        .flow
                        .came_in(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    went_out: report
                        .flow
                        .went_out(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    earned_by_capital: report
                        .flow
                        .earned_by_capital(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    moved_into_assets: report
                        .flow
                        .moved_into_assets(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    fees: report
                        .flow
                        .fees(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    taxes: report
                        .flow
                        .taxes(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    internal_transfers: report
                        .flow
                        .internal_transfers(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    cash_delta: report
                        .flow
                        .cash_delta(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    residual: report
                        .flow
                        .residual(currency)?
                        .to_calc_dec()
                        .inner()
                        .to_string(),
                    went_out_by_category,
                    earned_by_capital_by_source,
                    not_decomposed: NotDecomposedDto {
                        count,
                        amount: amount.to_calc_dec().inner().to_string(),
                    },
                })
            })
            .collect::<Result<Vec<_>, MoneyFlowError>>()?;
        let unexplained = report
            .flow
            .residuals_by_account()?
            .into_iter()
            .map(|(account, money)| AccountResidualDto {
                account: account.inner(),
                currency: CurrencyDto::from_domain(money.currency()),
                amount: money.to_calc_dec().inner().to_string(),
            })
            .collect();
        Ok(Self {
            contour: report.contour.0,
            contour_version: report.version.0,
            from: report.from,
            to: report.to,
            category_rule_versions: report.category_rule_versions.clone(),
            currencies,
            unexplained,
        })
    }
}

/// Cash and positions for one contour account.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountBalanceDto {
    pub account: Uuid,
    pub cash: Vec<BalanceCashDto>,
    pub reconciliation: Vec<ReconciliationStatusDto>,
    pub positions: Vec<PositionQuantityDto>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BalanceCashDto {
    pub currency: CurrencyDto,
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PositionQuantityDto {
    pub instrument: Uuid,
    pub custody: Option<Uuid>,
    pub quantity: String,
}

impl AccountBalanceDto {
    pub fn from_domain(row: &AccountBalanceRow) -> Self {
        Self {
            account: row.account.inner(),
            cash: row
                .cash
                .iter()
                .map(|money| BalanceCashDto {
                    currency: CurrencyDto::from_domain(money.currency()),
                    amount: money.to_calc_dec().inner().to_string(),
                })
                .collect(),
            reconciliation: row
                .reconciliation
                .iter()
                .map(ReconciliationStatusDto::from_domain)
                .collect(),
            positions: row
                .positions
                .iter()
                .map(|(key, quantity)| PositionQuantityDto {
                    instrument: key.instrument.inner(),
                    custody: key.custody.map(|custody| custody.inner()),
                    quantity: quantity.0.inner().to_string(),
                })
                .collect(),
        }
    }
}

impl ReconciliationStatusDto {
    pub(crate) fn from_domain(status: &ReconciliationStatus) -> Self {
        Self {
            account: status.account().inner(),
            from: status.period().from,
            to: status.period().to,
            dimensions: Dimension::all()
                .into_iter()
                .map(|dimension| DimensionStatusDto {
                    dimension: dimension.code().to_owned(),
                    status: status.dimension(dimension).code().to_owned(),
                })
                .collect(),
            evidence: status
                .evidence()
                .iter()
                .map(|evidence| EvidenceDto {
                    ground: evidence.ground().code().to_owned(),
                    level: evidence.level().code().to_owned(),
                    dimensions: evidence
                        .dimensions()
                        .into_iter()
                        .map(|dimension| dimension.code().to_owned())
                        .collect(),
                    confirming_parser: evidence.confirming().parser_version.0.clone(),
                    confirmed_parser: evidence.confirmed().parser_version.0.clone(),
                })
                .collect(),
            outcomes: status
                .outcomes()
                .iter()
                .map(|outcome| ClaimOutcomeDto {
                    claim: outcome.claim.discriminant().to_owned(),
                    outcome: outcome.outcome.code().to_owned(),
                })
                .collect(),
        }
    }
}

/// Return report.
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
    /// Valuation before hypothetical exit costs and before tax.
    pub liquidation_value_before_exit_costs_and_tax: LiquidationEstimateDto,
    /// Bond position attributes (§5.1).
    pub bond_attributes: Vec<BondPositionAttributesDto>,
    /// Bond position metrics under each available scenario (§7.1).
    pub bond_metrics: Vec<BondPositionMetricsDto>,
    /// **Pre-tax return.** The field name deliberately includes the qualification:
    /// taxes are introduced in E5, and until then this value cannot be called
    /// «return» without qualification (§16.3).
    pub xirr_pre_tax: RateDto,
    pub applied_rules: AppliedRulesDto,
    pub data_quality: DataQualityDto,
}

/// Applied rules (§3.2, §6.1).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppliedRulesDto {
    pub contour: Uuid,
    pub contour_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lot_rule: Option<String>,
    pub fx_source: String,
    pub day_count: String,
    /// Permissible width of the rate interval — it also determines
    /// the declared error margin of the result.
    pub solver_rate_tolerance: String,
    pub solver_max_iterations: u32,
    /// The calculation window used to classify a balance as negative
    /// (§11). A threshold-dependent number must include that threshold
    /// alongside it: otherwise the classification cannot be reproduced.
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

/// Account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountDto {
    pub id: Uuid,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Account creation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateAccountRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Broker token submission.
///
/// **`Debug` is implemented manually.** A derived implementation would print the token in full,
/// and `{:?}` on a request that failed to parse is a common way to find out why
/// it failed to parse. Once the token is in the log, it cannot be removed (§14).
///
/// There is intentionally no permission scope here: it is set by the system, not
/// the client (§14). Extra body fields are silently ignored, so
/// any «scope» sent by the client has no effect.
#[derive(Deserialize, ToSchema)]
pub struct AddBrokerAccessRequest {
    /// Broker code, for example `tinkoff`.
    pub broker: String,
    /// Broker environment. The field is required and has no default: tokens
    /// differ between environments, and silently recording the wrong environment causes
    /// the gateway to reject the first request — with no indication in the message
    /// that the environment is the cause.
    pub environment: BrokerEnvironmentDto,
    /// Broker token. A secret: accepted but never returned,
    /// so it is marked as `password` and `writeOnly` in the schema.
    #[schema(format = Password, write_only, example = "<secret>")]
    pub token: String,
}

impl fmt::Debug for AddBrokerAccessRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AddBrokerAccessRequest")
            .field("broker", &self.broker)
            .field("environment", &self.environment)
            .field("token", &"<hidden>")
            .finish()
    }
}

/// Broker environment in the transport layer. A separate type because
/// the port's `BrokerEnvironment` knows nothing about OpenAPI and should not.
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

/// Configured broker access.
///
/// `Debug` is derived: there is no secret in this type — neither the token nor
/// the ciphertext reaches it, because neither exists in the port.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BrokerAccessDto {
    pub id: Uuid,
    pub broker: String,
    /// Environment: `prod` or `sandbox`. A string rather than an enum:
    /// the record came from the database, and an unfamiliar value must reach
    /// the owner as-is rather than be turned into a read failure.
    pub environment: String,
    /// Permission scope. Always `read_only`: trading permissions are not
    /// are not requested under any circumstances (§14).
    pub scope: String,
    pub created_at: String,
    /// Revocation time. `null` — access is active. The field is not omitted
    /// when there is no value: an omitted field is indistinguishable from «unknown».
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

/// Claiming an instance.
///
/// The code is read from the console at server start-up — see `claim`. The label
/// describes what the owner will use for access: «laptop», «phone».
#[derive(Clone, Deserialize, ToSchema)]
pub struct ClaimRequest {
    /// One-time claim code. A secret: accepted but never
    /// returned, so it is marked as `password` in the schema.
    #[schema(format = Password, write_only, example = "<code from console>")]
    pub code: String,
    /// Label for the token being issued.
    pub label: String,
}

/// Custom `Debug`: the claim code grants the right to create an owner,
/// and derived output would write it to the very first log.
impl fmt::Debug for ClaimRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimRequest")
            .field("code", &"<redacted>")
            .field("label", &self.label)
            .finish()
    }
}

/// Permission scope in the transport layer. A separate type because the application's `Scope`
/// knows nothing about OpenAPI and should not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenScopeDto {
    /// Full owner access. It is **not accepted** in an issuance request:
    /// the owner is created by claiming the instance or via the console.
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

/// Token issuance request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    /// What this token will be used by: «home agent», «phone».
    /// The label is the only means of later identifying which token to revoke.
    pub label: String,
    pub scope: TokenScopeDto,
}

/// Newly issued token.
///
/// One type for both claiming an instance and issuing a token to an agent:
/// in both cases, a secret is returned, shown **once**,
/// and a second such type would be another place where that could be forgotten.
#[derive(Clone, Serialize, ToSchema)]
pub struct IssuedTokenDto {
    /// Record identifier — used to revoke the token.
    pub id: Uuid,
    /// The token itself. Shown **once**: only
    /// its hash remains in the database, so it cannot be displayed again.
    #[schema(format = Password, example = "<secret>")]
    pub token: String,
    pub label: String,
    pub scope: TokenScopeDto,
}

/// Custom `Debug`: derived output would write the token to the very first log,
/// and the log outlives both the process and the token itself.
impl fmt::Debug for IssuedTokenDto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedTokenDto")
            .field("id", &self.id)
            .field("token", &"<redacted>")
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

/// Issued token in a list.
///
/// Derived `Debug`: this type contains no secret — neither the token nor its hash
/// gets here, because neither is present in the port.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TokenDto {
    pub id: Uuid,
    pub label: String,
    pub scope: TokenScopeDto,
    pub created_at: String,
    /// Revocation time. `null` — the token is active. The field is not omitted
    /// when there is no value: an omitted field is indistinguishable from «unknown».
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

/// New version of the perimeter composition.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateContourVersionRequest {
    /// Perimeter identifier. Absent — a new one is created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contour: Option<Uuid>,
    pub title: String,
    pub accounts: Vec<Uuid>,
}

/// Perimeter version response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContourVersionDto {
    pub contour: Uuid,
    pub version: u32,
    pub accounts: Vec<Uuid>,
}

/// Exchange rate for a date specified by the owner (§6.1).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FxRateDto {
    pub from: CurrencyDto,
    pub to: CurrencyDto,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub rate: String,
}

/// Price observation with full provenance.
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
    /// Basis exactly as recorded by the source.
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

/// Exchange-rate observation with full provenance.
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

/// Key rate interval derived from daily observations.
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

/// Service status.
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

    /// The variant name in JSON is part of the contract: the external agent
    /// uses it to choose how to parse the value. The `match` is exhaustive, so a new variant
    /// must break the build rather than silently appear unnamed (§15.1).
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
            let json = serde_json::to_value(member).expect("variant is representable in JSON");
            assert_eq!(json["type"], corporate_action_tag(member));
            // Parsing it back must succeed: a name that is serialised
            // but cannot be parsed is only a contract on paper.
            let parsed: CorporateActionDto =
                serde_json::from_value(json).expect("variant can be parsed back");
            assert_eq!(corporate_action_tag(&parsed), corporate_action_tag(member));
            member
                .to_domain()
                .expect("variant reaches the domain layer");
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
            let json = serde_json::to_value(member).expect("variant is representable in JSON");
            assert_eq!(json["type"], offer_tag(member));
            let parsed: OfferExerciseDto =
                serde_json::from_value(json).expect("variant can be parsed back");
            assert_eq!(offer_tag(&parsed), offer_tag(member));
            member
                .to_domain()
                .expect("variant reaches the domain layer");
        }
    }

    /// The executability qualifier is mapped to an API string, and that string
    /// is part of the contract: the external agent uses it to decide whether the price
    /// can be trusted. An empty string instead of `indicative_previous_close`
    /// looks like a response rather than a refusal.
    ///
    /// The expected name is specified here by a SEPARATE exhaustive `match`,
    /// rather than taken from the function under test: a test that calls the same function
    /// will accept any answer it gives. A new variant breaks the test build.
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

    /// The reason a position remains unpriced is what
    /// its owner will see instead of an amount. Replacing it with an empty string
    /// means showing «no price» without explaining why.
    ///
    /// The expected name is specified by a separate exhaustive `match`
    /// using string literals, rather than through the function under test
    /// or a domain method. The test therefore catches an incorrect code and must
    /// break the build when a new reason variant is added.
    /// Additional checks require codes to be non-empty and distinct:
    /// using the same code conceals the specific reason for the lack of coverage.
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
            "reason code must not be empty"
        );
        let duplicate = codes
            .iter()
            .enumerate()
            .find_map(|(index, code)| codes[..index].contains(code).then_some(*code));
        let unique_codes = codes.iter().copied().collect::<HashSet<_>>();
        assert_eq!(
            unique_codes.len(),
            codes.len(),
            "duplicate reason code: {}",
            duplicate.unwrap_or("<unknown>")
        );
    }

    /// The observation time is included in the report as a string, while an unknown time
    /// is omitted from the response rather than replaced with an empty string.
    #[test]
    fn a_timestamp_travels_as_rfc_3339_in_utc() {
        let value =
            OffsetDateTime::from_unix_timestamp(1_787_000_000).expect("timestamp is representable");
        assert_eq!(
            format_timestamp(Some(value)),
            Some("2026-08-17T20:53:20Z".to_owned())
        );
        assert_eq!(format_timestamp(None), None);
    }

    /// An amount cannot exist without a currency: if the currency were optional,
    /// a missing field would have to be replaced with something — and it would be
    /// replaced with the rouble, because that is more convenient.
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
            source_category: None,
            description: None,
        }
    }

    #[test]
    fn the_api_does_not_drop_the_income_kind() {
        // The journal already stores the kind: losing it in transport means
        // withholding from the external agent information the system has.
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
        // The absence of the field means «not asserted», not «dividend».
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
        // The verdict is the response to the external agent. A missing field leaves
        // it with the code «rejected» and no indication of exactly what to fix:
        // such a response is worse than no response, because it looks complete.
        let event = EventId::new_random();
        let provisional = VerdictDto::from_domain(1, &Verdict::Provisional { event });
        assert_eq!(provisional.verdict, "provisional");
        assert_eq!(
            provisional.row, 1,
            "line number is included in the response unchanged"
        );
        assert_eq!(provisional.event_id, Some(event.inner()));

        let duplicate = VerdictDto::from_domain(2, &Verdict::Duplicate { existing: event });
        assert_eq!(duplicate.event_id, Some(event.inner()));

        let possible_event = EventId::new_random();
        let possible = VerdictDto::from_domain(
            3,
            &Verdict::PossibleDuplicate {
                event: possible_event,
                of: event,
                level: iaam_app::ingest::dedup::DedupLevel::Probabilistic,
            },
        );
        assert_eq!(possible.verdict, "possible_duplicate");
        assert_eq!(possible.event_id, Some(possible_event.inner()));
        assert_eq!(possible.of_event_id, Some(event.inner()));
        assert_eq!(possible.level, Some(5));

        let needs = VerdictDto::from_domain(
            3,
            &Verdict::NeedsClassification {
                question: "what kind of transaction is this?".into(),
            },
        );
        assert_eq!(
            needs.detail.as_deref(),
            Some("what kind of transaction is this?")
        );

        let unsupported = VerdictDto::from_domain(
            4,
            &Verdict::Unsupported {
                reason: "derivatives outside the perimeter".into(),
            },
        );
        assert_eq!(
            unsupported.detail.as_deref(),
            Some("derivatives outside the perimeter")
        );

        let rejected = VerdictDto::from_domain(
            5,
            &Verdict::Rejected {
                rejection: Rejection {
                    field: "amount".into(),
                    expected: "positive value".into(),
                    actual: "-1".into(),
                },
            },
        );
        assert_eq!(rejected.field.as_deref(), Some("amount"));
        assert_eq!(
            rejected.expected.as_deref(),
            Some("positive value"),
            "without an expected value, the refusal does not explain what to fix"
        );
        assert_eq!(rejected.actual.as_deref(), Some("-1"));
    }

    #[test]
    fn the_debug_of_a_broker_request_never_carries_the_token() {
        // Using `{:?}` on a request that failed to parse is a common way to investigate,
        // why it failed, and a derived `Debug` would send
        // the token itself there. It cannot then be removed from the log (§14).
        const TOKEN: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";
        let request = AddBrokerAccessRequest {
            broker: "tinkoff".into(),
            environment: BrokerEnvironmentDto::Sandbox,
            token: TOKEN.into(),
        };

        let printed = format!("{request:?}");
        assert!(
            !printed.contains(TOKEN),
            "token leaked into debug output: {printed}"
        );
        assert!(
            printed.contains("tinkoff"),
            "the broker code is not secret and must remain visible: {printed}"
        );
        assert!(
            printed.contains("Sandbox"),
            "the environment is not secret and must remain visible: {printed}"
        );
    }

    #[test]
    fn an_issued_token_never_reaches_the_debug_output() {
        // The response containing the token is shown once — and that is the only time it
        // exists in the clear. A derived `Debug` would send it
        // to the very first log, and the log outlives both the process and the token itself.
        const ISSUED: &str = "0123456789abcdef0123456789abcdef";
        let response = IssuedTokenDto {
            id: Uuid::new_v4(),
            token: ISSUED.into(),
            label: "home agent".into(),
            scope: TokenScopeDto::Agent,
        };

        let printed = format!("{response:?}");
        assert!(
            !printed.contains(ISSUED),
            "token leaked into debug output: {printed}"
        );
        assert!(
            printed.contains("home agent"),
            "the label is not secret and must remain visible: {printed}"
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
                .is_some_and(|detail| detail.contains("accrued interest observation"))
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
                .is_some_and(|detail| detail.contains("multiple schedule periods"))
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
    fn grouped_unverifiable_postings_name_count_and_date_range() {
        // The count and period boundaries replace repetitive itemised lines:
        // the owner understands the scale of the problem and knows what to backfill.
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let text = issue(&MaterialIssue::ScheduledPostingsUnverifiable {
            account,
            instrument,
            kind: PostingKind::Coupon,
            reason: iaam_core::returns::UnverifiableReason::PaymentDateUnknown,
            count: 5,
            first_date: time::macros::date!(2026 - 01 - 15),
            last_date: time::macros::date!(2026 - 05 - 15),
        });

        assert_eq!(
            text,
            format!(
                "the 5 payments of type coupon for instrument {} in account {} from 2026-01-15 to 2026-05-15 cannot be reconciled: payment_date_unknown",
                instrument.inner(),
                account.inner()
            )
        );
    }

    #[test]
    fn zero_reinvestment_note_is_present_in_serialized_metrics_body() {
        let computed = iaam_core::returns::zero_reinvestment::zero_reinvestment_metrics(
            Vec::new(),
            iaam_core::money::CalcMoney::new(Dec::new(Decimal::new(100, 0)), CurrencyCode::Rub),
            time::macros::date!(2026 - 01 - 01),
            time::macros::date!(2027 - 01 - 01),
        );
        let Computed::Value(metrics) = computed else {
            panic!("simple metrics should be computable");
        };
        let json = serde_json::to_value(ZeroReinvestmentMetricsDto::from_domain(&metrics))
            .expect("metrics should be representable as JSON");
        let note = json["zero_reinvestment_note"]
            .as_str()
            .expect("the assumption should be a string in the body");
        assert!(!note.is_empty());
        assert!(note.contains("Coupons"));
        assert!(note.contains("reinvested"));
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
        // The assignment code grants the right to create an owner in an empty database.
        const CODE: &str = "0123456789abcdef0123456789abcdef";
        let request = ClaimRequest {
            code: CODE.into(),
            label: "laptop".into(),
        };

        let printed = format!("{request:?}");
        assert!(
            !printed.contains(CODE),
            "assignment code leaked into debug output: {printed}"
        );
        assert!(printed.contains("laptop"), "{printed}");
    }

    #[test]
    fn a_refusal_to_compute_says_what_exactly_was_missing() {
        // `not_computable` provides the code, `detail` provides the specifics: which
        // instrument, which currency pair, which date. An empty explanation
        // turns «not computed» into «reason unknown».
        let instrument = InstrumentId::new_random();
        let missing_price = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::MissingPrice { instrument },
        });
        assert_eq!(missing_price.value, None);
        assert_eq!(
            missing_price.not_computable.as_deref(),
            Some("missing_price")
        );
        let detail = missing_price.detail.expect("explanation");
        assert!(
            detail.contains(&instrument.inner().to_string()),
            "the explanation must identify the instrument: {detail}"
        );

        let no_flows = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::NoExternalFlows,
        });
        assert_eq!(
            no_flows.detail.as_deref(),
            Some("no flows crossing the perimeter boundary")
        );

        let refused = ComputedDto::from_dec(&Computed::NotComputable {
            reason: NotComputable::SolverRefused {
                refusal: SolverRefusal::NoSignChange,
            },
        });
        let detail = refused.detail.expect("explanation");
        assert!(!detail.is_empty());
        assert_ne!(
            detail, "no flows crossing the perimeter boundary",
            "different reasons must be explained differently"
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
    /// Mapping a domain status to the DTO is where `Proven`
    /// and `Contradicts` can be swapped without breaking either the build,
    /// or any existing test: both are exposed as strings, and so far
    /// only `NotProven` has passed through `from_domain`. A swapped
    /// a pair would deem a contradictory record proven — exactly
    /// what the view must distinguish.
    #[test]
    fn domain_status_to_dto_does_not_confuse_branches() {
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
    fn each_basis_status_names_itself_in_api() {
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
                    .expect("status can be represented as JSON")
                    .as_str()
                    .expect("status code is a string")
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
            "duplicate status code: {}",
            duplicate.map(|code| code.as_str()).unwrap_or("<unknown>")
        );
    }
}
/// Report upload parameters. The route body is the workbook's binary bytes.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct DocumentParams {
    #[serde(default)]
    pub account: Option<Uuid>,
}

/// Report upload response.
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

/// Reconciliation range parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ReconciliationParams {
    pub account: Uuid,
    pub from: String,
    pub to: String,
}

/// Status of one measurement.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DimensionStatusDto {
    pub dimension: String,
    pub status: String,
}

/// Reason for raising the status.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvidenceDto {
    pub ground: String,
    pub level: String,
    pub dimensions: Vec<String>,
    pub confirming_parser: String,
    pub confirmed_parser: String,
}

/// Outcome of one control assertion.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClaimOutcomeDto {
    pub claim: String,
    pub outcome: String,
}

/// Account reconciliation status for an interval.
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

/// Owner category in the living reference list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryDto {
    pub id: Uuid,
    pub group: Uuid,
    pub title: String,
    pub retired_at: Option<String>,
}

impl CategoryDto {
    #[must_use]
    pub fn from_port(category: CategoryView) -> Self {
        Self {
            id: category.id.inner(),
            group: category.group.inner(),
            title: category.title,
            retired_at: category.retired_at,
        }
    }
}

/// Owner category rule.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryRuleDto {
    pub id: Uuid,
    pub version: u32,
    pub matcher: String,
    pub category: Uuid,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub valid_from: Option<Date>,
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub valid_to: Option<Date>,
    pub created_at: String,
    pub retired_at: Option<String>,
    pub replaces: Option<Uuid>,
}

impl CategoryRuleDto {
    #[must_use]
    pub fn from_port(rule: CategoryRuleView) -> Self {
        Self {
            id: rule.id.inner(),
            version: rule.version,
            matcher: rule.matcher,
            category: rule.category.inner(),
            valid_from: rule.valid_from,
            valid_to: rule.valid_to,
            created_at: rule.created_at,
            retired_at: rule.retired_at,
            replaces: rule.replaces.map(|id| id.inner()),
        }
    }
}

/// A new category group.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CategoryGroupRequest {
    pub title: String,
    /// Whether the group holds income categories. Income is the same list with
    /// a flag, not a second mechanism: cashback and interest on a balance are
    /// the owner's categories exactly as groceries are.
    #[serde(default)]
    pub is_income: bool,
}

/// A category group, active or retired.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryGroupDto {
    pub id: Uuid,
    pub title: String,
    pub retired_at: Option<String>,
    pub is_income: bool,
}

/// Request to create a category under an existing group.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CategoryRequest {
    pub group: Uuid,
    pub title: String,
}

/// Category matcher and validity interval for a new rule.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CategoryRuleRequest {
    /// Matcher object. Its accepted forms mirror the stored category matcher.
    pub matcher: serde_json::Value,
    pub category: Uuid,
    #[serde(default, with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub valid_from: Option<Date>,
    #[serde(default, with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub valid_to: Option<Date>,
    #[serde(default)]
    pub replaces: Option<Uuid>,
}

/// The rows and monthly movements caused by a proposed category rule.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryRuleImpactDto {
    pub rows: u64,
    pub months: Vec<MonthlyImpactDto>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MonthlyImpactDto {
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub month: Date,
    pub moved: Vec<CategoryMoveDto>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryMoveDto {
    pub from: Option<Uuid>,
    pub to: Uuid,
    pub amount: String,
    pub rows: u64,
}

impl CategoryRuleImpactDto {
    #[must_use]
    pub fn from_domain(impact: CategoryRuleImpact) -> Self {
        Self {
            rows: impact.rows,
            months: impact
                .months
                .into_iter()
                .map(MonthlyImpactDto::from_domain)
                .collect(),
        }
    }
}

impl MonthlyImpactDto {
    fn from_domain(month: MonthlyImpact) -> Self {
        Self {
            month: month.month,
            moved: month
                .moved
                .into_iter()
                .map(CategoryMoveDto::from_domain)
                .collect(),
        }
    }
}

impl CategoryMoveDto {
    fn from_domain(movement: CategoryMove) -> Self {
        Self {
            from: movement.from.map(|id| id.inner()),
            to: movement.to.inner(),
            amount: movement.amount.to_calc_dec().inner().to_string(),
            rows: movement.rows,
        }
    }
}

/// Cash balance stated by the owner.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OwnerCashDto {
    pub currency: CurrencyDto,
    pub amount: String,
}

/// Position stated by the owner.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OwnerPositionDto {
    pub instrument: Uuid,
    pub custody: Uuid,
    pub quantity: String,
}

/// Owner's response to a control balance request.
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

/// Classification rule.
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

/// Request to create or update a rule.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ClassificationRuleRequest {
    pub matcher: String,
    pub outcome: String,
    #[serde(default)]
    pub replaces: Option<Uuid>,
}

/// Rule identifier in DELETE.
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

/// Why a broker synchronisation withheld control assertions.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssertionsWithheldDto {
    pub code: String,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub as_of: Date,
}

/// Broker channel synchronisation result.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncOutcomeDto {
    pub recorded: Vec<VerdictDto>,
    pub duplicates: usize,
    pub possible_duplicates: usize,
    pub assertions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions_withheld: Option<AssertionsWithheldDto>,
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
            possible_duplicates: outcome.possible_duplicates,
            assertions: outcome.assertions,
            assertions_withheld: outcome.assertions_withheld.map(|withheld| match withheld {
                iaam_app::sync::AssertionsWithheld::PortfolioDescribesAnotherDay { as_of } => {
                    AssertionsWithheldDto {
                        code: withheld.code().to_owned(),
                        as_of,
                    }
                }
            }),
        }
    }
}
/// Access secret replacement: the secret is never part of the response.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BrokerAccessUpdateRequest {
    pub environment: BrokerEnvironmentDto,
    pub token: String,
}

/// Instrument catalogue entry.
///
/// The alias's `source` field is deliberately absent here: the catalogue is global
/// and readable by everyone, while `SourceId` points to a particular owner's
/// document (§14).
#[derive(Debug, Serialize, ToSchema)]
pub struct InstrumentDto {
    pub id: String,
    /// `null` — no kind is set; such an instrument is assessed
    /// as incomplete (§4.9, §5.4).
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
}

/// Data for recording an instrument by an administrator or synchronisation.
///
/// The identifier may be omitted: the server then assigns it. The currency
/// fields are required because a missing currency cannot be distinguished from
/// an unknown value in the stored catalogue.
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
    /// Document date. Required: ISIN changes, and there is no «current»
    /// answer (§4.7).
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub on: Date,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedInstrumentDto {
    pub instrument: String,
}
/// Manual market synchronisation parameters.
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
// Journal facts: corporate actions and offers (§4.7, §3.5).
//
// A separate input, rather than new members of `OperationKindDto`. The reason
// is mechanical: a corporate action's record date is part of the
// fact, while the operation date model cannot express it at all
// (`OperationDates` hard-codes `entitlement: None`).
//
// No arbitrary `EventKind` is accepted here: the input accepts exactly the
// families listed below.
// ---------------------------------------------------------------------

/// Amount with currency in the transport layer.
///
/// A nested object rather than a pair of adjacent fields: compensation for a replacement
/// is optional, and a flat pair would require an optional currency —
/// that is, a «currency without an amount» state, which cannot occur.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AmountDto {
    /// Decimal number as a string: binary floating-point loses pennies.
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

    /// Value per security: this is not account money but face value, so it
    /// is not measured in minor units and must not be rounded.
    fn to_per_unit(&self, field: &str) -> Result<PerUnitAmount, Rejection> {
        Ok(PerUnitAmount::new(
            Dec::new(decimal(&self.amount, field)?),
            self.currency.to_domain(),
        ))
    }
}

/// How the fractional part was handled during replacement.
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

/// Rule for carrying over tax cost during replacement.
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

/// Corporate action in the transport layer. Amounts are **positive**:
/// the ingestion layer applies the disposal sign, not the client.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CorporateActionDto {
    /// Amortisation: face value decreases, cash is received, and the number
    /// of securities does not change.
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
    /// Final redemption: face value is repaid in full and the security
    /// is disposed of.
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
    /// Replacement: a predecessor security is exchanged for a successor security.
    Conversion {
        predecessor: Uuid,
        successor: Uuid,
        custody: Uuid,
        /// How many successor securities correspond to one
        /// predecessor security.
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
                basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
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

/// Offer fact in the transport layer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OfferExerciseDto {
    /// Submitted application: it moves neither cash nor securities.
    Submitted {
        submission: Uuid,
        /// The window identifier is derived from `(instrument, execution_date)`.
        /// An arbitrary UUID will result in an unresolved application; old facts
        /// are not revalidated because the journal is append-only.
        window: Uuid,
        instrument: Uuid,
        quantity: String,
    },
    /// Withdrawal of an application in full or in part.
    Cancelled { submission: Uuid, quantity: String },
    /// Completed buyback: the security is disposed of for cash.
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

/// Journal fact: a corporate action or an offer.
///
/// Two families under one roof — a shared ingestion channel, not a shared
/// nature: a corporate action is decided by the issuer, while an offer is submitted by
/// the owner (`iaam-core/src/event/offer.rs`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalFactDto {
    /// Dates within the fact itself: the effective date is part of its
    /// identity, not a property of submission.
    CorporateAction { action: CorporateActionDto },
    /// An offer has no date of its own, so the client supplies the day:
    /// the ingestion layer has no basis for inventing it.
    OfferExercise {
        action: OfferExerciseDto,
        #[serde(with = "iso_date")]
        #[schema(value_type = String, format = Date, example = "2026-04-20")]
        day: Date,
    },
}

/// One journal fact in a batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JournalEventDto {
    pub account: Uuid,
    /// Flat, as with an operation: the client of a single API should not have to remember,
    /// that one input has the fact kind at the root, while the neighbouring one has it —
    /// inside a nested object.
    #[serde(flatten)]
    pub fact: JournalFactDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_operation_id: Option<String>,
}

impl JournalEventDto {
    /// The only place where the journal fact transport meets
    /// the domain. A rejection is returned with the field, expected value, and received value —
    /// this is the `422` response body (§13).
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

/// Request to ingest journal facts.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubmitJournalEventsRequest {
    /// Source label: manual entry, a specific agent, a specific file.
    pub source_label: String,
    pub events: Vec<JournalEventDto>,
}
