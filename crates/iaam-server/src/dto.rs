//! Transport representations (§3.2).
//!
//! DTOs live here and never move into the common crate: a common crate
//! of types quickly becomes a dumping ground, and the formally independent core
//! ends up depending on the layer that knows about everything.
//!
//! **Amounts are sent as decimal strings**, not floating-point
//! numbers: the JSON number `0.1` in binary floating point is not equal to one
//! tenth, and a monetary amount passed through it ceases to be a fact.

use crate::action_catalog::ActionCatalog;
use iaam_app::ingest::classification::{Answer, AnswerShape, Movement};
use iaam_app::ingest::journal_event::{JournalFact, SubmittedJournalEvent};
use iaam_app::ingest::observation::{
    Intake, ObservedCounterparty, ObservedDirection, ObservedRow, RowIdentity,
};
use iaam_app::ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use iaam_app::ingest::{Rejection, Verdict};
use iaam_app::ports::{
    BrokerAccessView, BrokerEnvironment, CashAssetClass, CategoryRuleView, CategoryView,
    ClassificationRuleView, IssuedToken, NegativeBalanceExpectation, Scope, TokenView,
};
use iaam_app::ports::{ImportQuestionView, ImportSessionView};
use iaam_app::scenarios::categories::{CategoryMove, CategoryRuleImpact, MonthlyImpact};
use iaam_app::scenarios::classification::{ClassifiedAs, PlannedCorrection, RuleChange};
use iaam_app::scenarios::correction::{CorrectionRequest, ImportCorrectionOutcome};
use iaam_app::scenarios::import_session::{
    HeldRow, ImportPlan, PlannedFact, Readiness, RetainedRow, RetentionReason,
};
use iaam_app::scenarios::reports::{
    AccountBalanceRow, AssetSnapshot, BalancesReport, CashFigure, Caveat, CaveatSubject,
    MoneyFlowOutcome, PopulationAccount, ReportConfidence, ReportPopulation, ReturnsOutcome,
};
use iaam_app::scenarios::transfer_pairing::{CashLeg, ConfirmedPairing, LegOrigin, Proposals};
use iaam_core::bond::offer::OfferChoice;
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{FeeOrigin, IncomeKind, TaxOrigin};
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::event::source_row::{RefusedRow, RowName};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId};
use iaam_core::instrument::AliasNamespace;
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::money_flow::MoneyFlowError;
use iaam_core::reconciliation::check::{ClaimOutcome, ClaimValue};
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use iaam_core::reconciliation::{ClaimCheck, Dimension, ReconciliationStatus, Taint};
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
    PriceDecision, PriceFreshness, PriceOrigin, PriceProvenance, PriceQuality, PriceSelection,
    QuotationBasis, SelectedPrice, SourceExecutability,
    UncoveredReason as CandidateUncoveredReason,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use time::{Date, OffsetDateTime};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;
use utoipa::{IntoParams, PartialSchema, ToSchema};
use uuid::Uuid;

use crate::vocabulary::{
    DataQualityStatusDto, NegativeCashClassificationDto, NotComputableCodeDto, VerdictCodeDto,
    described_vocabulary,
};

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
    /// A row the source did not resolve, submitted as the source stated it.
    ///
    /// Every other variant here is a conclusion. A bank row that prints a word
    /// meaning "internal to this institution" beside an amount is not one:
    /// `deposit` and `withdrawal` assert a direction the source did not give,
    /// and `transfer` demands an account the caller does not know. Sending one
    /// of them anyway is how a withdrawal was recorded as a deposit and had to
    /// be corrected afterwards.
    ///
    /// So this variant states the observation and nothing more. It is not a
    /// weaker version of the others and does not replace them: a caller that
    /// **has** concluded is still right to say so, and should.
    UnresolvedDirection {
        /// The amount with the sign the source printed. It is not made
        /// positive: the sign is evidence about direction where the source used
        /// one, and making it positive discards that.
        amount: String,
        currency: CurrencyDto,
        /// The direction the source stated: `in`, `out`, `inner` or `unknown`.
        ///
        /// `inner` is a stated direction that does not resolve to one — the
        /// money did not leave the institution, and which of the owner's
        /// accounts was on which side is not said. Omitting the field means
        /// `unknown`: the source said nothing at all, which is a weaker
        /// statement than `inner` and is kept apart from it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<String>,
        /// The party the source named, verbatim, when it named one.
        ///
        /// A name, not an account identifier: recognising it as one of the
        /// owner's accounts is a conclusion, and this shape exists so the
        /// caller does not have to reach one. The server resolves it against
        /// the owner's directory, and a counterparty it recognises settles the
        /// row with no question asked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counterparty: Option<String>,
        /// The document the row was read out of, as the source names it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_document: Option<String>,
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
    /// A name the caller gives **the fact**, so sending it twice records it
    /// once (§10.6, level two).
    ///
    /// A key names a fact, not a slot. Re-sending under a key the journal
    /// already holds is answered `duplicate` with the identifier of the first
    /// event, whatever the rest of the body says — the key is matched before
    /// the operation is compared to anything, so a **corrected** row under a
    /// key already used writes nothing and the journal keeps the wrong number.
    /// The response is a success, and it is easy to read as "the correction
    /// landed".
    ///
    /// This is the natural first move of an agent client — "the numbers were
    /// wrong, so I fixed them and resent" — and it is precisely the one that
    /// does nothing. **A fact that turned out wrong is corrected, never
    /// resent**: `POST /v1/corrections` with a `replacement` retracts the
    /// recorded event and states what should have stood instead, and it is the
    /// only thing that changes a number already in the journal. Re-use is not
    /// reversal: nothing on the ingest path writes a retraction, so a repeated
    /// submission is a no-op rather than a retract-and-add.
    ///
    /// The key is scoped to the **owner**, not to the account, the source or
    /// the import: two unrelated statements whose rows are keyed `row-1` are
    /// one fact as far as this field is concerned. Construct keys that are
    /// unique across everything the owner will ever import — the document and
    /// the row within it, rather than the row alone.
    ///
    /// Omitting it is not a lesser version of sending it. Without a key a
    /// second submission of the same row is a second event, because two
    /// identical purchases on one day are an ordinary thing and the system has
    /// no right to merge them.
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
    /// Conversion to whatever the caller actually submitted.
    ///
    /// The two arms are the point: a conclusion is converted to an operation
    /// exactly as it always was, and an observation is converted to an
    /// observation rather than to a conclusion nobody made.
    pub fn to_intake(&self) -> Result<Intake, Rejection> {
        let OperationKindDto::UnresolvedDirection {
            amount,
            currency,
            direction,
            counterparty,
            source_document,
        } = &self.kind
        else {
            return Ok(Intake::Concluded {
                operation: Box::new(self.to_domain()?),
            });
        };
        Ok(Intake::Observed {
            row: Box::new(ObservedRow {
                account: AccountId(self.account),
                direction: match direction.as_deref() {
                    Some(stated) => ObservedDirection::parse(stated)?,
                    // Absent means the source said nothing, which is
                    // `Unknown` and deliberately not `Inner`.
                    None => ObservedDirection::Unknown,
                },
                // `minor` and not the positivity check every conclusive kind
                // runs after it: those kinds derive the sign from the kind,
                // while an observation has no kind to derive it from. The
                // source's own sign is the only statement about direction there
                // is, and normalising it away would discard the evidence this
                // shape exists to carry.
                amount_minor: minor(amount, *currency, "amount")?,
                currency: currency.to_domain(),
                counterparty: counterparty
                    .clone()
                    .map_or(ObservedCounterparty::Unknown, ObservedCounterparty::Named),
                source_kind: self.source_category.clone(),
                description: self.description.clone(),
                dates: OperationDates {
                    trade: self.dates.trade,
                    settled: self.dates.settled,
                    cash_posted: self.dates.cash_posted,
                    paid: self.dates.paid,
                },
                source_time: None,
                identity: RowIdentity {
                    document: source_document.clone(),
                    row: self.source_operation_id.clone(),
                    idempotency_key: self.idempotency_key.clone(),
                },
            }),
        })
    }

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
            // An observation is not an operation, and refusing here rather than
            // inventing one is the whole of iaam-6qsa. The callers that reach
            // this function want a fact: a replacement correction supersedes an
            // event with a stated fact, and a row nobody has concluded cannot
            // supersede anything.
            OperationKindDto::UnresolvedDirection { .. } => {
                return Err(Rejection {
                    field: "type".into(),
                    expected: "an operation whose kind states what happened".into(),
                    actual: "unresolved_direction".into(),
                });
            }
        })
    }
}

/// The source the caller declares for this batch, and the import within it.
///
/// Without it the server mints a random source per request, and nothing
/// deduplicates across requests: a corrected re-submission would add a second
/// set of rows rather than replace the first (spec §6).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeclaredSourceDto {
    /// Account the rows belong to, named however the caller can name it: this
    /// account's iaam identifier, or the identifier its source prints for it —
    /// its `provider_account_id`, or one of its aliases, a card among them.
    ///
    /// **One field and not two**, and that is the decision. A second field
    /// beside a `Uuid` one would create a request that fills both, and the
    /// server would then have to publish a precedence rule for a case the
    /// caller never meant. There is nothing to disambiguate here because the
    /// tiering already answers it: an identifier that parses as an account of
    /// the owner's *is* that account, before anything else is consulted, so a
    /// caller sending a `Uuid` — every caller written before this — is answered
    /// exactly as it was. Anything else is matched against the identity a source
    /// prints, then against aliases, then against the title, stopping at the
    /// first tier that matches anything; two accounts in that tier are refused
    /// rather than picked between, and the refusal names both.
    ///
    /// Widening this field is what removes the read-then-join an import used to
    /// begin with: a statement prints an account number, and a caller holding
    /// one no longer has to fetch `/v1/accounts` to translate it before it can
    /// send a single row.
    ///
    /// Every operation in the batch must still name the account this resolves
    /// to, by its iaam identifier: the declaration says whose rows these are,
    /// and a row that disagreed would be recorded against one account while
    /// carrying the import identity of another.
    pub account: String,
    /// How the rows arrived: `file`, `paste`, `manual`.
    pub channel: String,
    /// What names this import within the account and channel — a statement
    /// period, an export file name, a run identifier.
    ///
    /// Two submissions carrying the same label are one import: re-sending a
    /// batch under its own label retracts and adds nothing. Two submissions
    /// labelled differently are two imports, and
    /// `POST /v1/corrections/imports` retracts exactly the one it names.
    ///
    /// Optional, and omitting it has a meaning rather than being a default:
    /// the rows belong to no named import, and they are retracted together
    /// with every other unnamed row of the same account and channel. Every
    /// import worth retracting on its own should be labelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Intake request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubmitOperationsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DeclaredSourceDto>,
    pub operations: Vec<OperationDto>,
}

/// One correction the owner submits.
///
/// Tagged on `relation` and named exactly as [`iaam_core::event::Relation`] is,
/// because the tag *is* the relation written into the journal. A wire word that
/// differed from the journal's would make the caller translate between two
/// vocabularies for one concept.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "relation", rename_all = "snake_case")]
pub enum CorrectionDto {
    /// Retract the target. It stays in the journal and stops being effective.
    Reversal {
        /// Identifier of the event to retract.
        target: Uuid,
    },
    /// Supersede the target with the operation given here.
    ///
    /// The whole operation, not a patch of one: the journal records facts as
    /// stated, and a partial correction would leave it holding a value nobody
    /// ever submitted.
    Replacement {
        /// Identifier of the event being superseded.
        target: Uuid,
        /// Boxed so the enum is not sized for its largest variant: a reversal
        /// carries an identifier, a replacement carries a whole operation.
        operation: Box<OperationDto>,
    },
}

/// Correct events the owner names.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubmitCorrectionsRequest {
    /// Acknowledge that a retracted fact stops counting in every report, and
    /// that re-submitting the same rows does not bring it back.
    ///
    /// Required rather than implied: a correction rewrites what every
    /// downstream report says, and a bare call is indistinguishable from a
    /// mistake.
    #[serde(default)]
    pub acknowledge_retraction: bool,
    /// Applied together or not at all: a correction batch is one deliberate act,
    /// unlike an import, whose rows are judged one by one.
    pub corrections: Vec<CorrectionDto>,
}

/// Correct one import, keyed on the declaration the caller made for it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CorrectImportRequest {
    /// Acknowledge that the retracted facts stop counting in every report, and
    /// that re-submitting the same rows does not bring them back.
    #[serde(default)]
    pub acknowledge_retraction: bool,
    /// The declaration the import was submitted under — the same account,
    /// channel and label.
    ///
    /// With the label, exactly that import is retracted and other imports
    /// through the same account and channel are left in force. Without it,
    /// what is retracted is every row of that account and channel that named
    /// no import — which is what rows recorded before imports could be named
    /// look like, and is the only way to reach them.
    pub source: DeclaredSourceDto,
}

/// Outcome of correcting one whole declared import.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct ImportCorrectionDto {
    /// The source the retracted rows arrived through.
    pub source: Uuid,
    /// The import identity the correction was keyed on. Absent when the
    /// request named no label, in which case the unnamed rows of `source`
    /// were the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import: Option<Uuid>,
    /// Effective events this import still had in the journal.
    pub affected: usize,
    /// Reversed by an earlier correction: a repeat run reports these and writes
    /// nothing.
    pub already_reversed: usize,
    /// Reversal facts written by this run. Nothing was deleted and nothing was
    /// mutated: each is a new event referencing the one it retracts.
    pub written: usize,
}

impl CorrectionDto {
    /// Conversion to a domain correction.
    ///
    /// The rejection names the field of the operation, without the batch index:
    /// the caller's loop position is known to the handler, not to one element.
    pub fn to_domain(&self) -> Result<CorrectionRequest, Rejection> {
        Ok(match self {
            Self::Reversal { target } => CorrectionRequest::Reversal {
                target: EventId(*target),
            },
            Self::Replacement { target, operation } => CorrectionRequest::Replacement {
                target: EventId(*target),
                operation: Box::new(operation.to_domain()?),
            },
        })
    }
}

impl ImportCorrectionDto {
    #[must_use]
    pub const fn from_domain(outcome: ImportCorrectionOutcome) -> Self {
        Self {
            source: outcome.source.inner(),
            // `Option::map` is not a const function, and this conversion is
            // worth keeping const beside its neighbours.
            import: match outcome.import {
                Some(import) => Some(import.inner()),
                None => None,
            },
            affected: outcome.affected,
            already_reversed: outcome.already_reversed,
            written: outcome.written,
        }
    }
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
    pub verdict: VerdictCodeDto,
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
    /// The import session holding the row, for a row that needs an answer.
    ///
    /// The published verdict carries the question as a sentence, and a sentence
    /// is not something a caller can answer. These two identifiers are, and they
    /// are what makes the question reachable after this response is gone: the
    /// question is a stored row, not a line in a response body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    /// The question raised about the row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_id: Option<Uuid>,
    /// What may be said in answer to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<Vec<AnswerAlternativeDto>>,
}

/// One alternative a question offers.
///
/// Published with the question rather than assumed by the caller: an answer the
/// question does not admit is a different mistake from an answer that is wrong,
/// and only the first can be refused before anything is written.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnswerAlternativeDto {
    /// The word to send back: `sent_to_own_account`, `received_from_own_account`,
    /// `paid`, `received`, `fee`, `income`.
    pub answer: String,
    /// Whether the answer must also name one of the owner's accounts.
    pub needs_account: bool,
}

impl AnswerAlternativeDto {
    #[must_use]
    pub fn from_domain(shape: AnswerShape) -> Self {
        Self {
            answer: shape.code().to_owned(),
            needs_account: shape.needs_account(),
        }
    }
}

impl VerdictDto {
    #[must_use]
    pub fn from_domain(row: usize, verdict: &Verdict) -> Self {
        let base = Self {
            row,
            verdict: VerdictCodeDto::from_domain(verdict),
            event_id: None,
            of_event_id: None,
            level: None,
            field: None,
            expected: None,
            actual: None,
            detail: None,
            account_id: None,
            dimension: None,
            session_id: None,
            question_id: None,
            alternatives: None,
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
    pub not_computable: Option<NotComputableCodeDto>,
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
                not_computable: Some(NotComputableCodeDto::from_domain(reason)),
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
    pub not_computable: Option<NotComputableCodeDto>,
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
    pub(crate) fn from_domain(value: &iaam_core::money::CalcMoney) -> Self {
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
    pub not_computable: Option<NotComputableCodeDto>,
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
                not_computable: Some(NotComputableCodeDto::from_domain(reason)),
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
    pub not_computable: Option<NotComputableCodeDto>,
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
                not_computable: Some(NotComputableCodeDto::from_domain(reason)),
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
    pub not_computable: Option<NotComputableCodeDto>,
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
                not_computable: Some(NotComputableCodeDto::from_domain(reason)),
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
            not_computable: Some(NotComputableCodeDto::from_domain(reason)),
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
    pub status: DataQualityStatusDto,
    pub nav_coverage: NavCoverageDto,
    pub position_coverage: PositionCoverageDto,
    pub executability: ExecutabilitySharesDto,
    pub material_issues: Vec<String>,
}

impl DataQualityDto {
    fn from_domain(quality: &DataQuality) -> Self {
        Self {
            status: DataQualityStatusDto::from_domain(&quality.status),
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

/// What would have to be true for a report's figures to be complete, and which
/// of those things are not.
///
/// **First in every report that carries it**, before the accounts, the
/// currencies and the population. A caveat published after the figures has
/// already lost to the reader who stopped at the figures — which is the
/// difficulty this block answers: `population` was the last
/// top-level field of the balances answer, and a run that read `covered=3,
/// outside=15` as an ordinary complete result never got that far.
///
/// **There is no score here.** No number, no percentage, no grade. A confidence
/// figure is an opinion the owner cannot check — the reason `PairingEvidenceDto`
/// publishes the fields two legs agree on rather than a match score. What is
/// published is a list of specific caveats, each naming one thing and the field
/// of this same response that states it in full.
///
/// **It is never a second source of truth.** Every caveat's `see` points at the
/// field that already says the same thing, and its `detail` is a constant of its
/// kind with nothing interpolated into it: a summary that restated an amount
/// could restate it wrongly, and then the report would contradict itself. The
/// whole register is computed in the core, from the values the response
/// publishes, for the same reason.
///
/// A caveat is not an error and does not make the figures wrong. It says what
/// they are an answer about. `complete: true` beside an empty `caveats` is the
/// statement that nothing known is missing.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfidenceDto {
    /// Which of the four goals this report answers: `asset_snapshot`,
    /// `money_flow`, `returns` or `reconciliation`.
    ///
    /// The same four names the outstanding-work queue grades its items by, so a
    /// caller holding a report with caveats can ask the queue what closes them.
    pub goal: String,
    /// Whether everything that would have to be true for these figures to be
    /// complete is true.
    ///
    /// Exactly `caveats == []`, and derived from it rather than stated beside
    /// it: there is no way to build this block asserting completeness over a
    /// non-empty register.
    ///
    /// Bounded by what the report can see. Every caveat is read off a
    /// computation the report itself performed, so `true` says "nothing the
    /// fold could check is missing" — and for the population half of the
    /// register, what the fold can check is the accounts this instance has been
    /// told about. See `population.known_account_coverage`.
    pub complete: bool,
    /// The specific things that are not. Always present; empty exactly when
    /// `complete` is true.
    pub caveats: Vec<CaveatDto>,
}

/// One specific, checkable thing a report's figures do not account for.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CaveatDto {
    /// What sort of gap this is — `account_in_no_scope`,
    /// `account_in_another_scope`, `account_ruled_outside`, `running_cash_sum`,
    /// `period_reports_refused`, `undecomposed_movements`,
    /// `unexplained_cash_change`, `unpriced_position`, `holding_not_valued`,
    /// `terminal_value_not_computed`, `return_not_computed`.
    ///
    /// A closed set. Every one of them is read off a computation the report
    /// already performs: nothing here folds the journal a second time.
    pub kind: String,
    /// What the caveat is about. Identifiers only — the title, the amount and
    /// the dates live at `see`.
    pub subject: CaveatSubjectDto,
    /// What this kind of caveat means, in one sentence. Constant for the kind:
    /// no figure of this report is interpolated into it.
    pub detail: String,
    /// The field of **this same response** that states the fact in full, as a
    /// path through the answer: `[]` stands for every element of an array, and
    /// `subject` says which element. Read it instead of believing this block.
    pub see: String,
    /// What to call about it. Always present; **empty means nothing in this API
    /// acts on this kind of gap**, which is a decision and not an omission.
    ///
    /// The other half of `see`: `see` is where to check the fact, this is where
    /// to act on it. Before it, a client that read `complete: false` had to
    /// fetch `/v1/actions` and filter it by `goal` — which answers "what stands
    /// between me and this whole report", not "what removes this line" — and a
    /// run through the flow shows an agent doing exactly that and still hunting
    /// through separate sections.
    ///
    /// Addressed the way an action's target is, through the same catalogue and
    /// against the same completed contract, so one reader serves both. What it
    /// does **not** carry is a `request`: the preset fields and the missing ones
    /// depend on the account, the interval and what the owner has already said,
    /// and only the outstanding-work queue computes them. A caveat says which
    /// call; `/v1/actions` says how to fill it in.
    ///
    /// One call does not always empty a caveat — a caveat is one line per
    /// account, currency or instrument, and closing it may take more than one
    /// fact. `see` remains the check.
    pub closed_by: Vec<ClosingOperationDto>,
}

/// One operation a caveat names, addressed against the completed contract.
///
/// `ResolutionOptionDto` without the `request`, and spelled identically in the
/// three fields they share so that a client reading an action's target reads
/// this with the same code. The request plan is what the two genuinely differ
/// on: the queue holds an account and an interval and can preset fields from
/// them, and a caveat kind holds nothing but its own identity.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClosingOperationDto {
    #[serde(rename = "operationId")]
    pub operation_id: String,
    pub method: String,
    pub path: String,
    #[serde(rename = "requestSchema")]
    pub request_schema: String,
}

/// The typed subject of a caveat.
///
/// Shaped like `ActionSubjectDto`, and for the reason that field exists: a
/// client that wants the caveats about one account must be able to select them
/// without parsing prose. `account_currency` is its own variant because a cash
/// figure and an opening assertion are both per account **and** currency, and a
/// caveat naming only the account would send the reader to the wrong row.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaveatSubjectDto {
    /// The answer as a whole: a figure the report declined to compute.
    Report,
    Account {
        id: Uuid,
    },
    AccountCurrency {
        account: Uuid,
        currency: CurrencyDto,
    },
    Instrument {
        id: Uuid,
    },
}

impl ConfidenceDto {
    /// `catalog` is a parameter for the reason `MoneyFlowReportDto::from_domain`
    /// takes its actions as one: the transport resolves an operation through its
    /// catalogue and the DTO cannot, and a register that could be built without
    /// addressing its remedies would eventually be built that way.
    #[must_use]
    pub fn from_domain(confidence: &ReportConfidence, catalog: &ActionCatalog) -> Self {
        Self {
            goal: confidence.goal().code().to_owned(),
            // From the register, never beside it: the domain type has no
            // `complete` field to copy, so the two cannot fall out of step.
            complete: confidence.complete(),
            caveats: confidence
                .caveats()
                .iter()
                .map(|caveat| CaveatDto::from_domain(caveat, catalog))
                .collect(),
        }
    }
}

impl CaveatDto {
    fn from_domain(caveat: &Caveat, catalog: &ActionCatalog) -> Self {
        Self {
            kind: caveat.kind().code().to_owned(),
            subject: CaveatSubjectDto::from_domain(caveat.subject()),
            detail: caveat.detail().to_owned(),
            see: caveat.see().to_owned(),
            // Resolved, not spelled. The core names the operation and the
            // catalogue says where it lives, so the register cannot publish a
            // route the contract does not declare: `ActionCatalog::from_openapi`
            // fails the server's start-up before it could.
            closed_by: caveat
                .closed_by()
                .iter()
                .map(|key| {
                    let resolved = catalog.operation(*key);
                    ClosingOperationDto {
                        operation_id: resolved.operation_id.clone(),
                        method: resolved.method.clone(),
                        path: resolved.path.clone(),
                        request_schema: resolved.request_schema.clone(),
                    }
                })
                .collect(),
        }
    }
}

impl CaveatSubjectDto {
    fn from_domain(subject: CaveatSubject) -> Self {
        match subject {
            CaveatSubject::Report => Self::Report,
            CaveatSubject::Account(account) => Self::Account {
                id: account.inner(),
            },
            CaveatSubject::AccountCurrency { account, currency } => Self::AccountCurrency {
                account: account.inner(),
                currency: CurrencyDto::from_domain(currency),
            },
            CaveatSubject::Instrument(instrument) => Self::Instrument {
                id: instrument.inner(),
            },
        }
    }
}

/// The population a report answered about.
///
/// Every report carries one. The quality fields of a report — data quality,
/// uncovered positions, unproven bases — all concern defects **inside** the
/// calculation, and each of them can be clean while the accounts selected for
/// it were the wrong ones: selection happens before the fold, so nothing
/// computed afterwards can see what was left out. This block is the second
/// statement, and without it a report over part of the owner's money reads as
/// an answer about all of it.
///
/// **`covered` and `outside` together are the whole denominator, and it is the
/// accounts this instance has been told about.** An account of the owner's that
/// was never created here appears in neither list, and it is not reported as
/// missing: it is invisible to the fold rather than omitted by it. That bound
/// is why the verdict below is called `known_account_coverage` and not
/// `completeness` — the old name invited a client to read `whole` as "these
/// figures are all of his money", which this API cannot know and does not say.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PopulationDto {
    /// The scope the report was computed over.
    pub contour: Uuid,
    pub contour_version: u32,
    /// How much of what the system knows about this report answered about.
    ///
    /// `whole` — every account the system knows of is covered. `bounded` —
    /// accounts are outside, and the owner has ruled on each of them, whether
    /// by placing it in a scope of his own or by ruling it outside every scope.
    /// `undecided` — accounts are outside that nobody has ruled on at all, so
    /// the report answers about a part of the owner's money that nobody has
    /// delimited.
    ///
    /// `undecided` outranks `bounded`: one account nobody has ruled on is
    /// enough, however many deliberate omissions stand beside it. And an
    /// account he ruled outside deliberately is `bounded`, never `whole`: this
    /// field says what the figures cover, not how tidy his decisions are.
    ///
    /// **Read the name before reporting the value.** `whole` says "every
    /// account we know of", never "everything he has". Nothing in this API sees
    /// a source document — the import path receives the rows a client chose to
    /// send it — so an export holding seven accounts of which four were ever
    /// created here produces `whole` over the four, and a client that reports
    /// that as complete coverage is making a claim this API did not make. The
    /// check is not in this field: it is comparing `covered` and `outside`
    /// against the accounts the source actually holds, which only the holder of
    /// the source can do.
    pub known_account_coverage: String,
    /// The accounts inside the report's scope — the population the figures were
    /// folded over. Always present; the same set the report's own rows are
    /// built from.
    pub covered: Vec<PopulationAccountDto>,
    /// The accounts the system knows about that this report did not cover.
    /// Always present: an empty array says the report covered everything known,
    /// while an absent key is indistinguishable from a report that never asked.
    pub outside: Vec<PopulationAccountDto>,
}

/// One account in a report's population.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PopulationAccountDto {
    pub account: Uuid,
    /// The account's title, so that an owner asked to rule on an omission is
    /// not asked about a bare identifier.
    pub title: String,
    /// The institution he said holds it, when he said. Absent, never null and
    /// never guessed: two accounts he calls one word, at two banks, are one
    /// line apart in `outside` and are not the same question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    /// `covered` — inside the report's scope. `outside_by_decision` — outside
    /// it because the owner ruled the account outside every scope and said why.
    /// `outside_placed_elsewhere` — outside it, and he has placed the account
    /// in a scope of his own: he said where it belongs, not that it does not
    /// belong here. `outside_undecided` — outside it and in no scope at all:
    /// **nobody has ruled on whether it belongs**, which is a different
    /// statement from a deliberate omission and must not be read as one.
    pub standing: String,
}

impl PopulationDto {
    #[must_use]
    pub fn from_domain(population: &ReportPopulation) -> Self {
        Self {
            contour: population.contour.0,
            contour_version: population.version.0,
            known_account_coverage: population.known_account_coverage().code().to_owned(),
            covered: population
                .covered()
                .map(PopulationAccountDto::from_domain)
                .collect(),
            outside: population
                .outside()
                .map(PopulationAccountDto::from_domain)
                .collect(),
        }
    }
}

impl PopulationAccountDto {
    fn from_domain(entry: &PopulationAccount) -> Self {
        Self {
            account: entry.account.inner(),
            title: entry.title.clone(),
            institution: entry.institution.clone(),
            standing: entry.standing.code().to_owned(),
        }
    }
}

/// Cash movement report over an inclusive interval.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MoneyFlowReportDto {
    /// What would have to be true for these figures to be a complete account of
    /// the interval's money, and which of those things are not. First, for the
    /// reason it is first on the balances answer.
    pub confidence: ConfidenceDto,
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
    /// What this report leaves outstanding. Always present: an empty array says
    /// the report was examined and found nothing, while an absent key would be
    /// indistinguishable from a bug to the agent reading it.
    pub actions: Vec<ActionDto>,
    /// The accounts this report covered, and the known accounts it did not.
    pub population: PopulationDto,
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
    pub by_account: Vec<NotDecomposedAccountDto>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NotDecomposedAccountDto {
    pub account: Uuid,
    pub count: u64,
    pub amount: String,
}

impl MoneyFlowReportDto {
    /// `actions` is a parameter rather than a field set afterwards: the transport
    /// resolves an operation through its catalogue and the DTO cannot, and a
    /// carrier that could be built without them would eventually be.
    pub fn from_domain(
        outcome: &MoneyFlowOutcome,
        actions: Vec<ActionDto>,
        catalog: &ActionCatalog,
    ) -> Result<Self, MoneyFlowError> {
        // The whole outcome rather than its two halves: the register is a
        // statement about the pair, and a signature that took them separately
        // would let a caller summarise one flow beside another's population.
        let confidence = ConfidenceDto::from_domain(&outcome.confidence()?, catalog);
        let report = &outcome.report;
        let population = &outcome.population;
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
                let by_account = report
                    .flow
                    .not_decomposed_by_account(currency)?
                    .into_iter()
                    .map(|(account, count, amount)| NotDecomposedAccountDto {
                        account: account.inner(),
                        count,
                        amount: amount.to_calc_dec().inner().to_string(),
                    })
                    .collect();
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
                        by_account,
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
            confidence,
            contour: report.contour.0,
            contour_version: report.version.0,
            from: report.from,
            to: report.to,
            category_rule_versions: report.category_rule_versions.clone(),
            currencies,
            unexplained,
            actions,
            population: PopulationDto::from_domain(population),
        })
    }
}

/// The balances answer: a row per contour account, and the negative cash the
/// answer as a whole carries.
///
/// An object rather than a bare array of rows, for the reason the market series
/// wrappers are objects: `negative_cash` is one fact about the whole answer, and
/// a copy of it on every row would invite a client to believe it could differ
/// between them.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BalancesReportDto {
    /// What would have to be true for these figures to be a complete statement
    /// of what the owner holds, and which of those things are not.
    ///
    /// **First**, before the rows. It was the reader who stopped at the numbers
    /// that this answer failed: a cash figure over an unasserted opening is
    /// movement from an unknown start and not a balance, and `population` stood
    /// last, after `accounts` and `negative_cash`. Both facts were published
    /// and neither was reached. `accounts[].cash[]` no longer lets the number
    /// be reached without the distinction; this block still says which rows are
    /// affected, in one place, without reading every row.
    pub confidence: ConfidenceDto,
    pub accounts: Vec<AccountBalanceDto>,
    /// Every account-and-currency in the scope whose cash balance is negative.
    /// Always present; empty when none is.
    ///
    /// A **warning, not a prohibition**: a technical overdraft happens on an
    /// ordinary account, and a margin balance is a liability that belongs in
    /// NAV. Nothing refuses, suppresses, or drops a row over it — the answer
    /// states it and the reader judges. A negative figure on an account that
    /// cannot be overdrawn is most often not a balance at all:
    /// `accounts[].cash[].kind` says `movement_since_unknown_start` for it, and
    /// that is the first thing to read.
    pub negative_cash: Vec<NegativeCashDto>,
    /// The accounts this report covered, and the known accounts it did not.
    ///
    /// On the answer, beside `negative_cash`, for the same reason: it is a fact
    /// about the set. No row can carry it — there is no row for an account the
    /// report left out, and that silence is what this block breaks.
    pub population: PopulationDto,
}

/// One account-and-currency carrying a negative cash balance at the report
/// date, and the §11 span it is the tail of.
///
/// Keyed the way a perimeter negative-cash span is keyed, and the assessment is
/// now the source of the dates and the classification: the entry is one fact
/// stated once, not two notions of negative cash to be reconciled with each
/// other.
///
/// The classification is **not** a verdict on the number. Only
/// `unsupported_margin_liability` and `unclassified_negative_cash` refuse the
/// account's period reports, and even then the figure above is stated and the
/// rest of the scope is calculated as usual — read `accounts[].period_reports`
/// for that.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NegativeCashDto {
    pub account: Uuid,
    pub currency: CurrencyDto,
    pub amount: String,
    /// The date the balance first went negative in this currency and stayed so.
    ///
    /// Null only if the assessment produced no open span for this account and
    /// currency, which the fold that produces both makes unreachable. It is
    /// null rather than the entry being absent because a figure is never
    /// withheld for want of an explanation.
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub from: Option<Date>,
    /// The date the balance returned to non-negative — always null here, and
    /// that is the point: an entry exists because the balance is still negative
    /// at the report date, so its span is still open. A closed span can appear
    /// in `accounts[].period_reports_refused`, and there this field is dated.
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub resolved: Option<Date>,
    /// Why the balance is negative (§11). Null under the same unreachable
    /// condition as `from`.
    pub classification: Option<NegativeCashClassificationDto>,
    /// What the owner said a negative balance on this account would mean. Null
    /// where he has not said, which is most accounts.
    ///
    /// It is layered over `classification` rather than competing with it: the
    /// classification is evidence about *why* this balance is negative, derived
    /// from settlement terms and credit indicators and needing no owner input
    /// (`iaam-sbht`); this is his prior about whether it should be negative at
    /// all.
    pub expectation: Option<NegativeBalanceExpectationDto>,
    /// Whether this figure contradicts what the owner expected — true only
    /// where he said a negative balance here would be unexpected.
    ///
    /// **A warning about a probable error, not a verdict and not a refusal.** A
    /// technical overdraft on a debit card is real and ordinary, which is why
    /// the owner records an expectation rather than a prohibition. Nothing is
    /// refused, suppressed or recalculated when this is true: the figure above
    /// stands, the report stays complete, and the reader is told where to look
    /// first — usually at `accounts[].cash[].kind`, since the reported case
    /// behind this was a missing opening assertion rather than an overdraft.
    ///
    /// Derived from `expectation`, never stored beside it: silence is not a
    /// contradiction, and `ordinary` is the opposite of one.
    pub contradicts_expectation: bool,
}

/// Cash and positions for one contour account.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountBalanceDto {
    pub account: Uuid,
    pub cash: Vec<CashFigureDto>,
    pub reconciliation: Vec<ReconciliationStatusDto>,
    pub positions: Vec<PositionQuantityDto>,
    /// `calculated` — nothing in §11 stops the period's tax and financial
    /// reports for this account. `refused` — §11 stops them, and
    /// `period_reports_refused` says why.
    ///
    /// The refusal is **this account's alone**: every other row in `accounts`
    /// is calculated exactly as it would have been, which is what §11 requires.
    /// It is also not a refusal of this row: `cash` and `positions` above are
    /// observed facts and are stated either way, because the perimeter always
    /// retains an observable cash effect and declines only to reconstruct
    /// financing economics it does not support.
    pub period_reports: String,
    /// The spans that refuse this account's period reports. Always present;
    /// empty exactly when `period_reports` is `calculated`.
    ///
    /// A span already closed at the report date can appear here: a deficit that
    /// nothing explained still refuses the period it fell in, even though the
    /// balance has since recovered and no `negative_cash` entry remains.
    pub period_reports_refused: Vec<PerimeterRefusalDto>,
}

/// One §11 negative-cash span, as the reason an account's period reports are
/// refused.
///
/// No account field: it is a property of the row it hangs on, and repeating it
/// would invite a client to believe it could differ.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PerimeterRefusalDto {
    pub currency: CurrencyDto,
    /// The date the balance went negative.
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub from: Date,
    /// The date it returned to non-negative; null while it is still open.
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub resolved: Option<Date>,
    pub classification: NegativeCashClassificationDto,
}

/// A cash figure, in a shape that cannot be read without deciding what kind of
/// figure it is.
///
/// This was `{currency, amount, opening}`, and the flag said correctly that an
/// `unasserted` amount is a running sum from an unknown start and not a
/// balance. It was not enough: `amount` could be read without reading
/// `opening`, and it was — an agent ran a first import, took
/// `accounts[].cash[].amount` for holdings, and reported an impossible negative
/// asset. A caveat beside a number loses to the number.
///
/// So there is no field called `amount` any more. The figure is spelled
/// `balance` exactly where it is one, `movement` where it is not, and the
/// variant has to be settled before either can be reached. This is the answer
/// `HoldingPriceDto` already gives for a price, in the same shape and for the
/// same reason: one reader serves both.
///
/// **The `mixed` case is only ever a total.** One account and currency is
/// covered by an opening assertion or is not, so its figure is `balance` or
/// `movement`. A total folded across accounts can be a mixture of the two, and
/// then it carries both parts and no sum of them — see `CashClassTotalDto`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CashFigureDto {
    /// Every figure folded in rests on an opening assertion covering the state
    /// before the first cash movement in this currency. `balance` is a balance.
    Balance {
        currency: CurrencyDto,
        /// Decimal number as a string: binary floating-point loses pennies.
        #[schema(example = "1000.00")]
        balance: String,
    },
    /// Nothing asserts the state this accumulated from, so `movement` is what
    /// the recorded interval moved and says nothing about what was there
    /// before it. It can be negative on an account that cannot be overdrawn
    /// without anything being wrong: money that arrived before the journal
    /// began and was spent after it makes exactly that figure.
    MovementSinceUnknownStart {
        currency: CurrencyDto,
        #[schema(example = "-500.00")]
        movement: String,
    },
    /// Part of what was folded in is a balance and part is movement. Both parts
    /// are stated and **their sum is not**: it would be neither of them.
    Mixed {
        currency: CurrencyDto,
        /// The part covered by opening assertions.
        #[schema(example = "1000.00")]
        balance: String,
        /// The part accumulated from an unknown start.
        #[schema(example = "-500.00")]
        movement: String,
    },
}

impl CashFigureDto {
    fn from_domain(figure: CashFigure) -> Self {
        let currency = CurrencyDto::from_domain(figure.currency());
        match figure {
            CashFigure::Balance(money) => Self::Balance {
                currency,
                balance: decimal_amount(money),
            },
            CashFigure::Movement(money) => Self::MovementSinceUnknownStart {
                currency,
                movement: decimal_amount(money),
            },
            CashFigure::Mixed { balance, movement } => Self::Mixed {
                currency,
                balance: decimal_amount(balance),
                movement: decimal_amount(movement),
            },
        }
    }
}

/// A posted amount as the decimal string every cash figure in this module is
/// spelled with.
fn decimal_amount(money: Money) -> String {
    money.to_calc_dec().inner().to_string()
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PositionQuantityDto {
    pub instrument: Uuid,
    pub custody: Option<Uuid>,
    pub quantity: String,
}

impl BalancesReportDto {
    pub fn from_domain(report: &BalancesReport, catalog: &ActionCatalog) -> Self {
        Self {
            // Asked of the report itself. The transport neither folds the rows
            // nor decides what a caveat is: a register assembled here could
            // disagree with the answer printed beside it.
            confidence: ConfidenceDto::from_domain(&report.confidence(), catalog),
            accounts: report
                .accounts
                .iter()
                .map(AccountBalanceDto::from_domain)
                .collect(),
            negative_cash: report
                .negative_cash
                .iter()
                .map(|entry| NegativeCashDto {
                    account: entry.account.inner(),
                    currency: CurrencyDto::from_domain(entry.money.currency()),
                    amount: entry.money.to_calc_dec().inner().to_string(),
                    from: entry.span.map(|span| span.from),
                    resolved: entry.span.and_then(|span| span.resolved),
                    classification: entry.span.map(|span| {
                        NegativeCashClassificationDto::from_domain(&span.classification)
                    }),
                    expectation: entry
                        .expectation
                        .map(NegativeBalanceExpectationDto::from_domain),
                    // Asked of the entry, not recomputed here: the transport
                    // does not decide what contradicts what.
                    contradicts_expectation: entry.contradicts_expectation(),
                })
                .collect(),
            population: PopulationDto::from_domain(&report.population),
        }
    }
}

impl AccountBalanceDto {
    pub fn from_domain(row: &AccountBalanceRow) -> Self {
        Self {
            account: row.account.inner(),
            cash: row
                .cash
                .iter()
                .map(|cash| CashFigureDto::from_domain(CashFigure::for_account(*cash)))
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
            period_reports: row.period_reports.code().to_owned(),
            period_reports_refused: row
                .period_reports
                .refusals()
                .iter()
                .map(|span| PerimeterRefusalDto {
                    currency: CurrencyDto::from_domain(span.currency),
                    from: span.from,
                    resolved: span.resolved,
                    classification: NegativeCashClassificationDto::from_domain(
                        &span.classification,
                    ),
                })
                .collect(),
        }
    }
}

/// What the owner holds at a date, grouped by the class of cash he declared.
///
/// The report the balances answer is not: `/v1/reports/balances` states a
/// figure per account and currency, with no total and no grouping, so the
/// owner's own question — how much is on deposit, how much on savings, how
/// much is invested, what the whole is worth — could only be assembled by
/// grouping accounts on their titles.
///
/// **The fields are in the order they must be read.** The register first, for
/// the reason it is first on the balances answer. Then the two halves: `cash`
/// is exact, and `positions` is worth what a quote said on the date it names.
/// Only then `total`, which mixes them. A reader who met the total first could
/// not tell which part of it a market can move overnight.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssetSnapshotDto {
    /// What would have to be true for these figures to be a complete statement
    /// of what the owner holds, and which of those things are not.
    pub confidence: ConfidenceDto,
    /// The date the snapshot is taken at.
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub as_of: Date,
    /// The exact half.
    pub cash: CashSideDto,
    /// The market-dependent half.
    pub positions: PositionsSideDto,
    /// Both halves added, per currency. Nothing is converted, so a currency
    /// present in only one half appears here as that half alone.
    ///
    /// A calculated value rather than a posted amount: the moment a quote
    /// enters a figure the figure stops being a bank balance, and the type says
    /// so.
    ///
    /// **A currency whose cash is not entirely balances is absent from this
    /// list, halves and all.** It is the one figure here that cannot say what
    /// it is — a whole is a single number by definition — and it is the figure
    /// a reader in a hurry stops at. While part of a currency's cash is
    /// movement from an unknown start there is no whole to state, so none is
    /// stated, exactly as an unvalued holding is absent from the position total
    /// rather than valued at zero. Nothing is withheld but the addition:
    /// `cash.totals` and `positions.totals` state both halves for every
    /// currency, and `confidence` names the accounts that would anchor them.
    pub total: Vec<CalcMoneyDto>,
    /// The rows every total above was folded from — the balances answer's own
    /// rows. They are here so a total can be checked against them inside one
    /// response rather than against a second request that may have read a
    /// different journal.
    pub accounts: Vec<AssetAccountDto>,
    /// The accounts this answer covered, and the known accounts it did not. A
    /// total that silently omits an account is worse than no total.
    pub population: PopulationDto,
}

/// Cash, as the journal recorded it, grouped by the class the owner declared.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CashSideDto {
    /// One entry per class present among the covered accounts, with the
    /// unstated class first. Always present; empty when the scope holds no
    /// account at all.
    pub classes: Vec<CashClassTotalDto>,
    /// Every class added up, per currency, each entry saying what kind of
    /// figure it is on the same terms a class total does.
    pub totals: Vec<CashFigureDto>,
}

/// One class of cash and what the accounts declared to be it hold.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CashClassTotalDto {
    /// The class the owner declared, in the vocabulary `AccountDto.cash_class`
    /// uses — `deposit`, `savings`, `card_account`, `wallet`. **Null is a
    /// group, not a missing field**: it holds the accounts whose class the
    /// owner has not stated, and nothing guesses one for them.
    pub cash_class: Option<String>,
    /// The accounts folded into these figures, so a heading can be traced to
    /// the rows beneath it.
    pub accounts: Vec<Uuid>,
    /// One figure per currency. Nothing is converted.
    ///
    /// **A class no longer carries one `opening` word for all its currencies.**
    /// An opening assertion is per account *and* currency, so a class can be
    /// anchored in one currency and not in another, and the single flag said
    /// `unasserted` for both. Each figure now says it for itself.
    ///
    /// **When the accounts in a class disagree**, the figure is `mixed`: it
    /// carries the balance part and the movement part and no sum of them. That
    /// is the decision this shape records. The alternatives were worse. Calling
    /// the whole thing movement understates a real balance and makes the
    /// anchored accounts useless at the class level. Publishing one number
    /// under a `mixed` label is worse still — a stock added to a flow denotes
    /// nothing, and a labelled number is still a number a reader can lift out.
    /// Two labelled parts are each a sum of like figures, and a reader who
    /// wants one number must add them himself, which is the moment he decides
    /// the addition means something.
    ///
    /// Which accounts lack the anchor is not repeated here: `confidence`
    /// carries one `running_cash_sum` caveat per account and currency, and a
    /// second copy on this row could fall out of step with it.
    pub totals: Vec<CashFigureDto>,
}

/// Positions, at the prices the journal holds.
///
/// The prices are the ones the journal itself records, the same board the
/// projection builds from `valuation` events. This report runs no market
/// selection of its own: an instrument the journal never priced is reported as
/// unvalued rather than valued from a source this report chose.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PositionsSideDto {
    /// One entry per instrument held across the scope. Always present.
    pub holdings: Vec<HoldingValueDto>,
    /// The earliest date any price behind `totals` was for — the oldest link,
    /// and the honest summary of «as of when». Null when nothing was priced.
    ///
    /// The per-holding dates are on `holdings[].price.as_of`: one summary date
    /// cannot say that one instrument is a day stale and another a year.
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date)]
    pub oldest_price_date: Option<Date>,
    /// The priced holdings added up, per currency of the quote. An unvalued
    /// holding is in no total.
    pub totals: Vec<CalcMoneyDto>,
}

/// One instrument the owner holds, and what a quote said it was worth.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HoldingValueDto {
    pub instrument: Uuid,
    /// The quantity across every account and custody location in the scope. The
    /// per-account keys stay on `accounts[].positions`.
    pub quantity: String,
    /// What the valuation policy decided for this instrument, whichever way it
    /// decided. Null only where an older rule's determination lost the
    /// observation it was made from.
    pub price: Option<HoldingPriceDto>,
    /// Null whenever the decision yields no figure this report can turn into
    /// money: an unvalued holding is **absent from the total, not valued at
    /// zero**. Zero is a number the owner would add up; null is a question, and
    /// `confidence` names it.
    pub value: Option<CalcMoneyDto>,
}

/// What the valuation policy decided for one holding.
///
/// The same decision, made by the same call, that the returns report publishes
/// for the same instrument on the same date: `selected` here and a
/// `selected_price` there are one observation, not two agreeing figures.
///
/// Three variants because there are three answers, and the last two are not
/// «null». A holding this report could not value says why it could not.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HoldingPriceDto {
    /// The policy chose an observation, with its full rationale.
    Selected {
        #[serde(flatten)]
        price: Box<SelectedPriceDto>,
    },
    /// The journal held only a determination an older rule had already made.
    /// It is reported, never re-derived: the event records the date the price
    /// was assigned to, not the date it was observed.
    LegacyDerived {
        /// Price per unit, as a decimal string.
        amount: String,
        currency: CurrencyDto,
        /// The date the price was assigned to.
        #[serde(with = "iso_date")]
        #[schema(value_type = String, format = Date)]
        as_of: Date,
        /// The determination the older rule recorded.
        quality: String,
    },
    /// Nothing was selected, and why: `no_observation`, `too_old`,
    /// `ambiguous_venue` or `ambiguous_candidate`.
    Uncovered { reason: String },
}

impl HoldingPriceDto {
    fn from_domain(decision: &PriceDecision) -> Option<Self> {
        match decision {
            PriceDecision::Selected(price) => Some(Self::Selected {
                price: Box::new(SelectedPriceDto::from_domain(price)),
            }),
            PriceDecision::LegacyDerived { quality, price } => {
                price.as_ref().map(|price| Self::LegacyDerived {
                    amount: price.price.inner().to_string(),
                    currency: CurrencyDto::from_domain(price.currency),
                    as_of: price.as_of,
                    quality: quality.code().to_owned(),
                })
            }
            PriceDecision::Uncovered(reason) => Some(Self::Uncovered {
                reason: holding_uncovered_reason(*reason).to_owned(),
            }),
        }
    }
}

/// Why the policy selected nothing.
///
/// The same words the returns report uses for the same four outcomes: a reader
/// comparing the reports must not have to translate between two vocabularies
/// for one refusal.
const fn holding_uncovered_reason(reason: CandidateUncoveredReason) -> &'static str {
    match reason {
        CandidateUncoveredReason::NoObservation => "no_observation",
        CandidateUncoveredReason::TooOld => "too_old",
        CandidateUncoveredReason::AmbiguousVenue => "ambiguous_venue",
        CandidateUncoveredReason::AmbiguousCandidate => "ambiguous_candidate",
    }
}

/// One account inside the snapshot: the class it was grouped under, and what it
/// holds.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssetAccountDto {
    pub account: Uuid,
    /// The class the owner declared, or null where he has not.
    pub cash_class: Option<String>,
    pub cash: Vec<CashFigureDto>,
    pub positions: Vec<PositionQuantityDto>,
}

impl AssetSnapshotDto {
    pub fn from_domain(snapshot: &AssetSnapshot, catalog: &ActionCatalog) -> Self {
        Self {
            // Asked of the snapshot itself. The transport neither folds the
            // rows nor decides what a caveat is: a register assembled here
            // could disagree with the answer printed beside it.
            confidence: ConfidenceDto::from_domain(&snapshot.confidence(), catalog),
            as_of: snapshot.as_of,
            cash: CashSideDto {
                classes: snapshot
                    .cash
                    .classes
                    .iter()
                    .map(|class| CashClassTotalDto {
                        cash_class: class.cash_class.clone(),
                        accounts: class
                            .accounts
                            .iter()
                            .map(|account| account.inner())
                            .collect(),
                        totals: class
                            .totals
                            .iter()
                            .copied()
                            .map(CashFigureDto::from_domain)
                            .collect(),
                    })
                    .collect(),
                totals: snapshot
                    .cash
                    .totals
                    .iter()
                    .copied()
                    .map(CashFigureDto::from_domain)
                    .collect(),
            },
            positions: PositionsSideDto {
                holdings: snapshot
                    .positions
                    .holdings
                    .iter()
                    .map(|holding| HoldingValueDto {
                        instrument: holding.instrument.inner(),
                        quantity: holding.quantity.0.inner().to_string(),
                        price: HoldingPriceDto::from_domain(&holding.price),
                        value: holding.value.as_ref().map(CalcMoneyDto::from_domain),
                    })
                    .collect(),
                oldest_price_date: snapshot.positions.oldest_price_date,
                totals: snapshot
                    .positions
                    .totals
                    .iter()
                    .map(CalcMoneyDto::from_domain)
                    .collect(),
            },
            total: snapshot
                .total
                .iter()
                .map(CalcMoneyDto::from_domain)
                .collect(),
            accounts: snapshot
                .accounts
                .iter()
                .map(|row| AssetAccountDto {
                    account: row.account.inner(),
                    cash_class: row.cash_class.clone(),
                    cash: row
                        .cash
                        .iter()
                        .map(|cash| CashFigureDto::from_domain(CashFigure::for_account(*cash)))
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
                })
                .collect(),
            population: PopulationDto::from_domain(&snapshot.population),
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
                .map(EvidenceDto::from_domain)
                .collect(),
            outcomes: status
                .outcomes()
                .iter()
                .map(ClaimOutcomeDto::from_domain)
                .collect(),
            taints: status.taints().iter().map(TaintDto::from_domain).collect(),
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

/// The returns answer: the report, and the population it answered about.
///
/// The report's fields sit at the top level, exactly where they have always
/// sat, and `population` joins them. It is a separate type rather than a field
/// on `ReturnsReportDto` because the report is a conversion of
/// `iaam_core::returns::ReturnsReport`, which knows only the scope it was
/// folded over and cannot know what the owner has outside it. Constructing the
/// answer therefore **requires** the manifest: a response carrying the figures
/// without the population it computed them over cannot be built.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReturnsAnswerDto {
    /// What would have to be true for these figures to be a complete statement
    /// of what the money earned, and which of those things are not. First, for
    /// the reason it is first on the balances answer.
    pub confidence: ConfidenceDto,
    #[serde(flatten)]
    pub report: ReturnsReportDto,
    /// The accounts this report covered, and the known accounts it did not.
    pub population: PopulationDto,
}

impl ReturnsAnswerDto {
    #[must_use]
    pub fn from_domain(outcome: &ReturnsOutcome, catalog: &ActionCatalog) -> Self {
        Self {
            confidence: ConfidenceDto::from_domain(&outcome.confidence(), catalog),
            report: ReturnsReportDto::from_domain(&outcome.report),
            population: PopulationDto::from_domain(&outcome.population),
        }
    }
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
                not_computable: Some(NotComputableCodeDto::from_domain(reason)),
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
///
/// The three fields decision 0004 adds are absent from an account that carries
/// none of them, which is every account created before it. Absent is the honest
/// wire shape for "the owner has not said", and it keeps every client written
/// against the old response working unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountDto {
    pub id: Uuid,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    /// The client's own label for the source this account's identity came from.
    /// Present exactly when `provider_account_id` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// What the source prints for this account. Opaque: iaam does not parse it,
    /// does not check its shape, and never renders it where a title belongs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cash_class: Option<CashAssetClassDto>,
    /// What the owner says a negative balance here would mean. Absent is «he
    /// has not said», and that account behaves exactly as it did before this
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_balance_expectation: Option<NegativeBalanceExpectationDto>,
    /// Further identifiers reaching this same account. Two cards over one
    /// underlying account are one account with two aliases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<AccountAliasDto>,
}

/// One alias of an account, valid over a half-open interval.
///
/// `valid_to` absent is an open-ended interval. A card that stopped working is
/// an alias whose `valid_to` is set: there is no binding lifecycle, so an
/// expired, a reissued, a blocked and a closed card are the same fact here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountAliasDto {
    /// Opaque for the same reason `provider_account_id` is opaque.
    pub value: String,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub valid_from: Date,
    #[serde(
        default,
        with = "iso_date::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, format = Date)]
    pub valid_to: Option<Date>,
}

/// The class of cash an account holds, as the owner declares it.
///
/// **A grouping label, and nothing branches on it.** Report grouping reads it to
/// render a heading; no rule, no projection, no classification, no validation
/// and no refusal reads it (decision 0004 §3). In particular it must not be used
/// to decide which negative balances are impossible — that is a separate need
/// with a separate declaration, and deriving it here is the branch `iaam-d41s`
/// refuses.
///
/// Cash only: `brokerage` and `security_position` are deliberately not values,
/// because a position on an instrument is what the journal records and needs no
/// declaration. Unset is a value — "not stated" — expressed by the field's
/// absence, and it is never inferred from a title or a transaction pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CashAssetClassDto {
    Deposit,
    Savings,
    CardAccount,
    Wallet,
}

impl CashAssetClassDto {
    #[must_use]
    pub const fn to_domain(self) -> CashAssetClass {
        match self {
            Self::Deposit => CashAssetClass::Deposit,
            Self::Savings => CashAssetClass::Savings,
            Self::CardAccount => CashAssetClass::CardAccount,
            Self::Wallet => CashAssetClass::Wallet,
        }
    }

    #[must_use]
    pub const fn from_domain(class: CashAssetClass) -> Self {
        match class {
            CashAssetClass::Deposit => Self::Deposit,
            CashAssetClass::Savings => Self::Savings,
            CashAssetClass::CardAccount => Self::CardAccount,
            CashAssetClass::Wallet => Self::Wallet,
        }
    }
}

/// What the owner expects a negative balance on an account to mean.
///
/// **A warning, never a constraint.** A first draft of `iaam-d41s` had the
/// owner record that an account *cannot be overdrawn*, and he corrected it: a
/// technical overdraft on a debit card is real and ordinary. Nothing in iaam
/// refuses a request, drops a row, suppresses a figure or fails a check on this
/// value. The only thing it does is set `contradicts_expectation` beside a
/// figure the report states either way.
///
/// **It is not derived from `cash_class`, and `cash_class` is not derived from
/// it.** Decision 0004 §3 forbids that merge by name — «a savings account
/// cannot be overdrawn, therefore warn» is wrong on the first ordinary
/// technical overdraft. Two values, two consumers: the class reaches a report
/// heading, this reaches a warning.
///
/// The field's absence is «the owner has not said», which is a third state and
/// is never filled in by inference from a title, a class or a transaction
/// pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NegativeBalanceExpectationDto {
    /// A negative balance here would probably be an error.
    Unexpected,
    /// A negative balance here is ordinary — a credit line, a margin balance.
    Ordinary,
}

impl NegativeBalanceExpectationDto {
    #[must_use]
    pub const fn to_domain(self) -> NegativeBalanceExpectation {
        match self {
            Self::Unexpected => NegativeBalanceExpectation::Unexpected,
            Self::Ordinary => NegativeBalanceExpectation::Ordinary,
        }
    }

    #[must_use]
    pub const fn from_domain(expectation: NegativeBalanceExpectation) -> Self {
        match expectation {
            NegativeBalanceExpectation::Unexpected => Self::Unexpected,
            NegativeBalanceExpectation::Ordinary => Self::Ordinary,
        }
    }
}

/// The computed action policy returned for an owner.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActionsResponseDto {
    pub policy_version: u32,
    pub items: Vec<ActionDto>,
}

/// One computed action.
///
/// `GET /v1/actions` returns a bare array of these, and there is no envelope
/// around it. There was one, holding a `policy_version`, and §1.5 of
/// `docs/api/conventions.md` asks the question that removed it: is there a fact
/// about the answer as a whole that no item can carry? The version would have
/// been such a fact if anything moved it, and nothing did — it was the literal
/// `1`, written at the one place the response was built, derived from nothing
/// and bumped by nothing. A client invited to branch on it would have branched
/// never. Three further responses already embed `Vec<ActionDto>` with no version
/// beside it — the reconciliation answer, the broker sync outcome, the money-flow
/// report — so the field was also absent from three quarters of the surface that
/// publishes these items.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActionDto {
    pub id: String,
    pub kind: String,
    pub category: String,
    /// The reports this item stands between the owner and.
    ///
    /// Non-empty exactly when `category` is `required_for_goal`, and empty —
    /// so, absent from the response — for every other category. A blocking item
    /// stops the next call rather than a report, a recommendation stops nothing,
    /// and a fact stops nothing.
    ///
    /// Published beside `category` rather than folded into it, because the
    /// category is a wire string a client already switches on and the goals are
    /// a set it filters by. A client asking "what stands between me and an asset
    /// snapshot" keeps the items whose `goals` hold `asset_snapshot`, and gets a
    /// shorter list than the queue — which is the whole point: the required
    /// items were previously indistinguishable, so the queue read as a
    /// precondition on everything, which is not what any of it does.
    ///
    /// The vocabulary is closed and is exactly four values, in this order:
    /// `asset_snapshot`, `money_flow`, `returns`, `reconciliation`. They name
    /// the four reports this API computes, and the goals a report publishes for
    /// itself use the same four names.
    ///
    /// Always present, empty array included, for the reason
    /// `a_clean_instance_carries_actions_present_and_empty` gives about the
    /// list that holds these items: an absent key is indistinguishable from a
    /// bug, while `[]` says this item stands in the way of no report at all —
    /// which is a fact about a blocking item, and one a client should be able
    /// to read rather than infer.
    #[serde(default)]
    pub goals: Vec<String>,
    pub state: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_scope: Option<String>,
    /// What the item is about, when it is about one thing. Absent on the
    /// existential items — «no account exists», «no contour exists» — which name
    /// nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<ActionSubjectDto>,
    pub target: ActionTargetDto,
}

/// The typed subject of an action.
///
/// A field of its own rather than a name buried in `reason`: `id` is opaque by
/// contract and `reason` is a sentence, so a client that wants the items about
/// one account had no way to select them without parsing prose.
///
/// An account subject carries the owner's own name for the account beside the
/// identifier, under the naming rule in `docs/api/conventions.md` §3. The queue
/// is the surface that rule was written for: a client is handed a dozen items,
/// one per account, and nothing else in the response says which account any of
/// them is about. Before the name travelled with the item, reading the queue
/// meant a second request to `GET /v1/accounts` and a join the client was left
/// to perform — and an item the owner cannot name is an item he cannot act on.
///
/// The pair is built in `iaam_app::actions`, where the item is built, and not
/// joined here. See `iaam_app::actions::AccountSubject` for why: `reason`
/// already interpolates the title, and a name joined from a second read of the
/// store could contradict the sentence printed beside it.
///
/// An event has no name. `Event` stays a bare identifier because nothing the
/// owner said names one — the id is the whole of its identity, and the item's
/// `reason` states what the event is.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionSubjectDto {
    Account {
        id: Uuid,
        /// What the owner calls this account.
        title: String,
        /// The institution he said holds it, when he said. Absent means he has
        /// not said, never that the account has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        institution: Option<String>,
    },
    Event {
        id: Uuid,
    },
}

/// The tagged target of an action.
///
/// `options` is the third shape and the reason it exists is drift. An action
/// whose `reason` named two ways to close it could publish only one, so a client
/// that reads `target` as the contract — which is what `target` is for — could
/// act on that one and never learn the other existed without reading prose and
/// searching the specification. Each option carries its own `request`, because
/// two ways out of one state are two calls wanting different fields.
///
/// A separate variant rather than an `options` array on every target: most
/// actions genuinely have one way out or none, and `operation` and `none` keep
/// exactly the shape they had for them.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionTargetDto {
    Operation {
        #[serde(rename = "operationId")]
        operation_id: String,
        method: String,
        path: String,
        #[serde(rename = "requestSchema")]
        request_schema: String,
        request: RequestPlanDto,
    },
    /// Two or more admissible resolutions, in the order the item offers them.
    ///
    /// Ordered, not ranked: the first is the ordinary answer and none of them is
    /// a default the caller may take without the owner.
    Options {
        options: Vec<ResolutionOptionDto>,
    },
    None,
}

/// One admissible way to close an action.
///
/// The body of [`ActionTargetDto::Operation`] as a value, so that one resolution
/// among several is described exactly as the sole resolution of another action
/// is, and a client needs one reader for both.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolutionOptionDto {
    #[serde(rename = "operationId")]
    pub operation_id: String,
    pub method: String,
    pub path: String,
    #[serde(rename = "requestSchema")]
    pub request_schema: String,
    pub request: RequestPlanDto,
}

/// Request fields that the policy cannot fill.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RequestPlanDto {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preset: BTreeMap<String, serde_json::Value>,
    pub missing: Vec<MissingInputDto>,
}

/// One missing request field and its source.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MissingInputDto {
    pub pointer: String,
    pub provided_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<AccountCandidateDto>>,
    /// The literal values this field admits, when it admits a closed set.
    ///
    /// Absent where the field is not a choice. Distinct from `candidates`,
    /// which offers accounts for a field whose type is an account; an
    /// alternative is a value of this field itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<InputAlternativeDto>,
}

/// One admissible value of a missing input, and what choosing it then needs.
///
/// `requires` is why an alternative is an object and not a bare string: some
/// values need a further field that the others do not, and a flat list of words
/// would either look complete when it is not, or make every such field required
/// for every value.
///
/// Deliberately not [`AnswerAlternativeDto`], which the ingest and session
/// responses publish. That one answers "what may be said to this question" and
/// says `needs_account: true`; this one answers "what goes in which request
/// field, and what else that value then needs", and names the accounts. A queue
/// item that only said an account was needed would leave the caller to find out
/// which are eligible somewhere else.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InputAlternativeDto {
    /// The value written at the parent's pointer.
    pub value: String,
    /// Fields that become required only if this alternative is chosen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<RequiredInputDto>,
}

/// A field one alternative requires. It carries no alternatives of its own.
///
/// `MissingInputDto` without the `alternatives`, so the two do not nest. A
/// required field that were itself a closed choice would be a second question
/// asked before the first is answered, and the mutually recursive schemas would
/// not terminate.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RequiredInputDto {
    pub pointer: String,
    pub provided_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<AccountCandidateDto>>,
}

/// An account that can be selected for a contour.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountCandidateDto {
    pub id: Uuid,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Where an account stands relative to the owner's reporting perimeter.
///
/// Three values, because two are not enough. An account may be inside a
/// contour, outside every contour on purpose — a counterparty's, a closed one,
/// one the owner does not want reported — or waiting for him to say which. The
/// third is the state a newly created account is in, and the one the action
/// queue asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountScopeDispositionDto {
    Inside,
    Outside,
    Undecided,
}

/// An account's current disposition.
///
/// `title` and `institution` travel with `account` under the naming rule in
/// `docs/api/conventions.md` §3. This answer exists to be read back to the
/// owner — «is this one inside your perimeter, and if not, why» — and it names
/// exactly one account, so nothing else in it says which.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountScopeDto {
    pub account: Uuid,
    /// What the owner calls this account.
    pub title: String,
    /// The institution he said holds it, when he said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    pub disposition: AccountScopeDispositionDto,
    /// The owner's reason, present only for `outside`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The contours naming this account. Empty unless the disposition is
    /// `inside`, and the reason it is returned: «inside» is not a stored flag
    /// but a fact of the contour composition, and the answer says whose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contours: Vec<Uuid>,
}

/// Recording the owner's decision about one account.
///
/// `inside` is not accepted here. Membership is the contour's composition, and
/// writing it twice — once as a version and once as a flag on the account —
/// would create two answers to one question. The route says so rather than
/// silently accepting a value it cannot honour.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordAccountScopeRequest {
    pub disposition: AccountScopeDispositionDto,
    /// Required for `outside` and refused for `undecided`. A perimeter decision
    /// without a reason is indistinguishable, a year later, from an oversight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The owner's statement about which of his accounts money moves between.
///
/// `stated` is the field that carries the third state. An empty `partners` list
/// means two different things — «none of my others» and «he has not said» — and
/// a response that spelled both as `[]` would let a caller read a silence as an
/// answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountTransferPartnersDto {
    pub account: Uuid,
    /// Whether the owner has ruled at all.
    pub stated: bool,
    /// The accounts he named. Empty while `stated` is false, and legitimately
    /// empty when he has stated that none of his others is on the other side.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partners: Vec<Uuid>,
}

/// Recording that statement for one account.
///
/// An empty list is accepted and is not the same as not calling the route: it
/// is «money moves between this account and none of my others», and it is what
/// closes the queue item for an account that genuinely stands alone.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordAccountTransferPartnersRequest {
    pub partners: Vec<Uuid>,
}

/// One account's statement inside a batch.
///
/// The account moves from the path into the body, and nothing else changes: the
/// list is still «these, and no others», still about this one account, and
/// still says nothing about whether an account it names moves money with a
/// third. That is why the batch carries one entry per account rather than a set
/// of pairs — the relation is not what is being recorded, the closure is, and a
/// closure is a fact about exactly one account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountTransferPartnersStatementDto {
    pub account: Uuid,
    /// Legitimately empty: «money moves between this account and none of my
    /// others», the answer that closes the queue item for an account that
    /// stands alone.
    #[serde(default)]
    pub partners: Vec<Uuid>,
}

/// Recording those statements for several accounts in one call.
///
/// The queue asks the question once per account and twelve accounts were twelve
/// round trips. This collapses the round trips and nothing else: every check the
/// single-account route makes is made here, per entry, and the whole batch is
/// refused if any entry fails one.
///
/// An account may appear at most once. Two enumerations for one account cannot
/// both be the complete one, and picking the later would silently discard a
/// statement the owner made.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordAccountTransferPartnersBatchRequest {
    pub statements: Vec<AccountTransferPartnersStatementDto>,
}

/// The statements as they stand after the batch, read back in the order asked.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountTransferPartnersBatchDto {
    pub statements: Vec<AccountTransferPartnersDto>,
}

/// Account creation.
///
/// Every field decision 0004 adds is optional, and a request that omits them all
/// is exactly the request this endpoint accepted before it.
///
/// Sending `provider` and `provider_account_id` makes the call an upsert by
/// external identity: a create repeating an identity already recorded returns
/// the account created last time, with `200 OK`, rather than minting a second
/// one. The pair travels together — one half alone is refused, because a request
/// that stated half an identity would be stored as having stated none and the
/// caller would discover it only on the re-import that duplicated the account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateAccountRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    /// The client's own label for the source. iaam does not interpret it; it
    /// scopes the identifier below so that two sources printing short
    /// sequential identifiers cannot collide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Whatever the source prints for this account. iaam accepts any string and
    /// stores it as given: it does not require a fingerprint and does not
    /// compute one. Client tooling is advised to send a stable derived value
    /// rather than the printed number, and to change `provider` whenever it
    /// changes that derivation — a re-derivation must present as a new source
    /// rather than as new accounts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cash_class: Option<CashAssetClassDto>,
    /// What a negative balance on this account would mean. Optional, and
    /// defaulting to no statement at all. It is the owner's, never inferred,
    /// and never read off `cash_class`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_balance_expectation: Option<NegativeBalanceExpectationDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<AccountAliasDto>,
}

/// The aliases an account carries, as the owner now states them.
///
/// The whole set, not a change to it. An empty list is a real statement —
/// "this account is reached by no further identifier" — and it is how the last
/// alias is withdrawn.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReplaceAccountAliasesRequest {
    pub aliases: Vec<AccountAliasDto>,
}

/// The declarations an account carries beside its title, as the owner now
/// states them.
///
/// Every field is optional, and **absent means «leave this one alone»**. That is
/// the third state, and it is the reason each present field is an object with a
/// `stated` flag rather than a bare value: a request shape where absence meant
/// «none» would withdraw, on every call, every declaration the caller did not
/// happen to repeat — and one of these decides which account a later import
/// lands on.
///
/// `AccountTransferPartnersDto` carries the same `stated` flag for the same
/// reason one noun away. So the three states of one field read:
///
/// - the key is absent — he has not mentioned it, and nothing changes;
/// - `{"stated": false}` — he states none, and the stored value is cleared;
/// - `{"stated": true, ...}` — he states this.
///
/// Aliases are not here. They are a set replaced whole, and
/// [`ReplaceAccountAliasesRequest`] is their route.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReplaceAccountDeclarationsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<AccountIdentityStatementDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cash_class: Option<AccountCashClassStatementDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_balance_expectation: Option<AccountNegativeBalanceExpectationStatementDto>,
}

/// The owner's statement about the identity a source prints for this account.
///
/// The pair travels together, and a statement carrying one half is refused for
/// the reason account creation refuses one: an identifier without the source
/// that printed it has no scope, and a source without an identifier names no
/// account. Both halves are absent exactly when `stated` is false.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountIdentityStatementDto {
    /// Whether the account has an external identity at all. `false` withdraws
    /// the one it carried, and is not the same call as omitting `identity`.
    pub stated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Opaque, exactly as at creation: iaam does not parse it, does not check
    /// its shape, and never renders it where a title belongs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
}

/// The owner's statement about the class of cash this account holds.
///
/// A grouping label read by one report heading and by nothing else (decision
/// 0004 §3). Changing it is safe by construction — no rule consults it — so it
/// needs no ceremony beyond his word, and the route asks for none.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountCashClassStatementDto {
    /// Whether he states a class at all. `false` returns the account to «not
    /// stated», which groups under its own heading and is never guessed.
    pub stated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<CashAssetClassDto>,
}

/// The owner's statement about what a negative balance on this account means.
///
/// A warning and never a constraint (`iaam-d41s`): the only thing that reads it
/// sets `contradicts_expectation` beside a figure the report states either way.
/// Changing it therefore invalidates nothing already recorded, and the route
/// asks no more than it asks for a class.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountNegativeBalanceExpectationStatementDto {
    /// Whether he has said anything about a minus here. `false` returns the
    /// account to «he has not said», which is never filled in by inference.
    pub stated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expectation: Option<NegativeBalanceExpectationDto>,
}

/// The account as its declarations now stand, with what the call did not do.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountDeclarationsDto {
    pub account: AccountDto,
    /// Present exactly when the call displaced an identity the account was
    /// already carrying — replaced it with another, or withdrew it.
    ///
    /// Absent for the three cases that are ordinary: the account carried no
    /// identity, the call did not mention the identity, or the identity stated
    /// is the one already recorded. Giving an identity to an account that had
    /// none is a first statement and needs no announcement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_repointed: Option<AccountIdentityRepointedDto>,
}

/// An identity was re-pointed, and here is what that did not do.
///
/// **The change is recorded rather than refused, and this block is why that is
/// safe to publish rather than a thing to hide.** The refusal one reaches for
/// first — «facts were imported under the old identity, so refuse» — cannot be
/// stated against this journal. An event records the account it belongs to and a
/// free `source` label; no column and no event kind records the external
/// identity in force when the row arrived, and the journal is append-only in the
/// database, so nothing can be backfilled to make one. A refusal would have to
/// be conditioned instead on «this account has facts at all», which refuses an
/// account whose whole history was typed in by hand under no identity, and still
/// does not answer the question anyone asked.
///
/// So the owner is told what happened and what did not, and he decides.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountIdentityRepointedDto {
    /// The identity the account answered to until this call.
    ///
    /// Returned rather than withheld, for the reason decision 0004 §1 gives for
    /// storing what a source prints at all: a mismatched import is debuggable by
    /// reading the value, and an owner who has just re-pointed an account needs
    /// to see which identity he displaced.
    pub previous: AccountIdentityStatedDto,
    /// Whether the journal holds any business fact recorded against this
    /// account.
    ///
    /// About the **account**, not about the identity — the journal cannot answer
    /// the second question, and this field is deliberately not named as though
    /// it could. `true` is the case worth reading twice: those facts stayed
    /// where they were.
    pub facts_recorded: bool,
    /// The specific things this call did not do. A closed set, and each `detail`
    /// is a constant of its `kind` with nothing interpolated into it.
    pub not_done: Vec<AccountIdentityNotDoneDto>,
}

/// Both halves of an identity, as a value rather than a statement.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountIdentityStatedDto {
    pub provider: String,
    pub provider_account_id: String,
}

/// One thing re-pointing an identity did not do.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountIdentityNotDoneDto {
    /// Which of them: `facts_not_moved`, `previous_identity_not_reserved` or
    /// `no_fact_records_the_identity_it_arrived_under`.
    pub kind: String,
    /// What that means, in one sentence, constant for the kind.
    pub detail: String,
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

/// Permission scope in the transport layer. A separate type because the application's `Scope`
/// knows nothing about OpenAPI and should not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenScopeDto {
    /// Full owner access. It is **not accepted** in an issuance request:
    /// the owner is created with `iaam claim --label <label>`.
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
/// The token is returned and shown **once**; only its hash remains in the
/// database, so it cannot be shown again.
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

/// Creating a perimeter.
///
/// This request creates a contour and does nothing else. Adding a version to a
/// contour that exists is `POST /v1/contours/{contour}/versions`, which names
/// the contour in its path.
///
/// The two were one route, whose `contour` field was optional and whose absence
/// meant «mint a fresh perimeter». An agent that had already drawn one called it
/// again to bring a second bank inside and was given a second contour holding
/// that bank alone — every operation recorded, every verdict positive, and the
/// report over the newer contour showing one bank.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateContourVersionRequest {
    /// Not accepted. A request naming a contour is refused with `422` rather
    /// than honoured or ignored: honouring it is the defect, and ignoring it
    /// would leave every client already sending the field creating perimeters it
    /// did not ask for. Use `POST /v1/contours/{contour}/versions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contour: Option<Uuid>,
    pub title: String,
    pub accounts: Vec<Uuid>,
}

/// Adding a version to a contour that already exists.
///
/// The contour is named by the path, so there is no field whose absence could be
/// read as «make me a new one».
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddContourVersionRequest {
    /// The complete composition the contour is to have from this version.
    ///
    /// A version is a whole membership list, not a delta: sending only the
    /// account being added would drop every existing member.
    pub accounts: Vec<Uuid>,
    /// The title for the new version. Absent — the title the contour already
    /// carries is kept, so the owner is never asked to retype a name he has
    /// already given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The version the caller believes is current. Absent — no precondition is
    /// checked.
    ///
    /// A composition is written whole, so a caller that read version 1, decided
    /// on a composition and sent it after someone else wrote version 2 does not
    /// merge with that writer — it silently discards them. Stating the version
    /// it reasoned from turns that into a `409`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u32>,
}

/// Perimeter version response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContourVersionDto {
    pub contour: Uuid,
    pub version: u32,
    /// The title the version carries.
    pub title: String,
    pub accounts: Vec<Uuid>,
    /// Whether this call brought the contour into existence.
    ///
    /// The field the split makes necessary: a caller must be able to tell «I
    /// created the perimeter» from «the perimeter was already there and I wrote
    /// into it», and could not, because both answers looked identical.
    pub created: bool,
}

/// A contour at its current version, with the composition that version names.
///
/// The read side of the same composition the write routes build, derived from
/// it rather than stored beside it: an import skill has to be able to check the
/// perimeter it was handed against what the system believes, and a second copy
/// would be a second thing to keep in step.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContourDto {
    pub contour: Uuid,
    pub title: String,
    /// The current version. Every earlier one is still in the store; this is the
    /// one a report uses when no version is asked for.
    pub version: u32,
    /// The accounts this version covers. Empty is a real answer: a version can
    /// be recorded with no members, and it covers nothing.
    pub accounts: Vec<Uuid>,
}

/// Exchange rate for a date specified by the owner (§6.1).
///
/// The pair is spelled `base`/`quote`, as it is in the `market/fx` query and in
/// the rows that route answers: `from` and `to` are the interval on every other
/// route, and one name per thing is worth more than a shorter name here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FxRateDto {
    /// The currency being priced — the `USD` of `USD/RUB`.
    pub base: CurrencyDto,
    /// The currency it is priced in — the `RUB` of `USD/RUB`.
    pub quote: CurrencyDto,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub rate: String,
}

/// A price series: its rows and the boundary the data is complete through.
///
/// The boundary is a property of the answer rather than of a row — it is one
/// value for the whole series — and it is returned even when `rows` is empty.
/// That is the only way a client can tell "this instance holds nothing for the
/// series" (`complete_through` is `null`) from "the series is complete and
/// holds no value in this interval".
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketPriceSeriesDto {
    pub rows: Vec<MarketPriceDto>,
    /// The date the series is known to be complete through, or `null` when this
    /// instance has published nothing for the series at all. Always present:
    /// an answer that omitted it would say nothing about how far the data goes.
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date, required = true)]
    pub complete_through: Option<Date>,
}

/// An exchange-rate series: its rows and the boundary the data is complete
/// through. Same shape and same reading as [`MarketPriceSeriesDto`].
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketFxSeriesDto {
    pub rows: Vec<MarketFxDto>,
    /// The date the series is known to be complete through, or `null` when this
    /// instance has published nothing for the series at all. Always present:
    /// an answer that omitted it would say nothing about how far the data goes.
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date, required = true)]
    pub complete_through: Option<Date>,
}

/// A key-rate series: its intervals and the boundary the data is complete
/// through. Same shape and same reading as [`MarketPriceSeriesDto`].
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketKeyRateSeriesDto {
    pub rows: Vec<MarketKeyRateDto>,
    /// The date the series is known to be complete through, or `null` when this
    /// instance has published nothing for the series at all. Always present:
    /// an answer that omitted it would say nothing about how far the data goes.
    #[serde(with = "iso_date::option")]
    #[schema(value_type = Option<String>, format = Date, required = true)]
    pub complete_through: Option<Date>,
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
}

/// Exchange-rate observation with full provenance.
///
/// The pair is spelled as the query spells it, `base`/`quote`: a client that
/// asked for `base=USD&quote=RUB` reads the same two names back.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketFxDto {
    /// The currency being priced — the `USD` of `USD/RUB`.
    pub base: CurrencyDto,
    /// The currency it is priced in — the `RUB` of `USD/RUB`.
    pub quote: CurrencyDto,
    pub nominal: u32,
    pub value: String,
    pub unit_rate: String,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    pub source: String,
    pub observed_at: String,
    pub quality: String,
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
    use iaam_core::event::provenance::{ParserVersion, RawHash};
    use iaam_core::ids::{EventId, InstrumentId};
    use iaam_core::numeric::xirr::SolverRefusal;
    use iaam_core::reconciliation::evidence::{Evidence, Ground, SourceChannel};

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
        assert_eq!(provisional.verdict.code(), "provisional");
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
        assert_eq!(possible.verdict.code(), "possible_duplicate");
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
            dto.not_computable.map(NotComputableCodeDto::code),
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
            dto.not_computable.map(NotComputableCodeDto::code),
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
            missing_price.not_computable.map(NotComputableCodeDto::code),
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

    fn claim_check(claim: ControlClaim, outcome: ClaimOutcome) -> ClaimCheck {
        ClaimCheck { claim, outcome }
    }

    fn rendered(check: &ClaimCheck) -> serde_json::Value {
        serde_json::to_value(ClaimOutcomeDto::from_domain(check)).expect("claim outcome renders")
    }

    fn cash_balance(minor: i64) -> ControlClaim {
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(minor),
            at: iaam_core::reconciliation::claim::BalancePoint::Closing,
        }
    }

    /// The outcome object carries `code` plus **exactly one** of the three details,
    /// and `matched` carries none. A renderer that filled a second key would let an
    /// agent read a stale discrepancy off an excepted outcome.
    #[test]
    fn each_claim_outcome_renders_only_its_own_detail() {
        let discrepancy = iaam_core::reconciliation::check::Discrepancy {
            field: "amount",
            claimed: ClaimValue::Money {
                amount: PostedMinor::new(1_000),
                currency: CurrencyCode::Rub,
            },
            observed: ClaimValue::Money {
                amount: PostedMinor::new(400),
                currency: CurrencyCode::Rub,
            },
            delta: ClaimValue::Money {
                amount: PostedMinor::new(600),
                currency: CurrencyCode::Rub,
            },
        };
        let cases = [
            (ClaimOutcome::Matched, "matched", Vec::<&str>::new()),
            (
                ClaimOutcome::Discrepant(discrepancy),
                "discrepant",
                vec!["discrepancy"],
            ),
            (
                ClaimOutcome::NotComparable {
                    reason: iaam_core::reconciliation::check::NotComparable::NoJournalCoverage,
                },
                "not_comparable",
                vec!["reason"],
            ),
            (
                ClaimOutcome::Excepted {
                    exception:
                        iaam_core::reconciliation::check::ReconciliationException::UnsupportedRepoEncumbrance,
                },
                "excepted",
                vec!["exception"],
            ),
        ];

        for (outcome, code, present) in cases {
            let value = rendered(&claim_check(cash_balance(1_000), outcome));
            let outcome = &value["outcome"];
            assert_eq!(outcome["code"], code);
            for key in ["discrepancy", "reason", "exception"] {
                assert_eq!(
                    outcome.get(key).is_some(),
                    present.contains(&key),
                    "{code} rendered {key} as {outcome}"
                );
            }
        }

        let discrepant = rendered(&claim_check(
            cash_balance(1_000),
            ClaimOutcome::Discrepant(discrepancy),
        ));
        assert_eq!(
            discrepant["outcome"]["discrepancy"],
            serde_json::json!({
                "field": "amount",
                "claimed": { "money": { "amount": "10.00", "currency": "RUB" } },
                "observed": { "money": { "amount": "4.00", "currency": "RUB" } },
                "delta": { "money": { "amount": "6.00", "currency": "RUB" } },
            })
        );
    }

    /// A turnover asserts two values. A single `claimed` would have to pick one of
    /// them, and the reader could not tell which.
    #[test]
    fn a_cash_turnover_claim_renders_debit_and_credit_and_no_single_claimed() {
        let value = rendered(&claim_check(
            ControlClaim::CashTurnover {
                currency: CurrencyCode::Rub,
                debit: PostedMinor::new(1_500),
                credit: PostedMinor::new(2_500),
            },
            ClaimOutcome::Matched,
        ));

        assert_eq!(value["claim"]["kind"], "cash_turnover");
        assert_eq!(value["claim"]["debit"], "15.00");
        assert_eq!(value["claim"]["credit"], "25.00");
        assert!(
            value["claim"].get("claimed").is_none(),
            "a turnover has two sides, not one claimed value: {value}"
        );
    }

    /// A quantity is not money and must not be rendered as though it were.
    #[test]
    fn a_position_quantity_claim_renders_a_tagged_quantity() {
        let value = rendered(&claim_check(
            ControlClaim::PositionQuantity {
                instrument: InstrumentId::new_random(),
                custody: iaam_core::ids::CustodyId::new_random(),
                quantity: iaam_core::money::Quantity(Dec::new(rust_decimal::Decimal::from(10))),
                at: iaam_core::reconciliation::claim::BalancePoint::Closing,
            },
            ClaimOutcome::Matched,
        ));

        assert_eq!(value["claim"]["kind"], "position_quantity");
        assert_eq!(
            value["claim"]["claimed"],
            serde_json::json!({ "quantity": "10" })
        );
    }

    fn source_channel(parser: &str, document: Option<&str>) -> SourceChannel {
        SourceChannel {
            source: iaam_core::ids::SourceId::new_random(),
            parser_version: ParserVersion(parser.to_owned()),
            document: document.and_then(|hex| RawHash::parse(&hex.repeat(64))),
        }
    }

    /// Independence needs the document, not the source: two channels sharing a
    /// parser version differ only by document, and the DTO must show it.
    #[test]
    fn evidence_renders_the_documents_that_decide_independence() {
        let confirming = source_channel("shared/1", Some("c"));
        let confirmed = source_channel("shared/1", Some("d"));
        assert!(
            !confirming.is_independent_of(&confirmed),
            "a shared parser version is not independence"
        );
        let evidence = Evidence::from_match(
            Ground::BrokerApiAgreesWithStatement,
            confirming,
            confirmed,
            std::collections::BTreeSet::from([Dimension::Cash]),
        )
        .expect("evidence");

        let value =
            serde_json::to_value(EvidenceDto::from_domain(&evidence)).expect("evidence renders");
        assert_eq!(value["confirming_parser"], "shared/1");
        assert_eq!(value["confirmed_parser"], "shared/1");
        assert_eq!(value["confirming_document"], "c".repeat(64));
        assert_eq!(value["confirmed_document"], "d".repeat(64));
        assert_ne!(value["confirming_document"], value["confirmed_document"]);
    }

    /// Two absent documents compare equal, so the pair is **not** independent —
    /// the case a reader would most easily assume it was. The DTO renders the
    /// absence as absence rather than as two indistinguishable blanks.
    #[test]
    fn two_absent_documents_are_not_independent_and_render_as_absent() {
        let confirming = source_channel("api/1", None);
        let confirmed = source_channel("api/2", None);
        assert!(
            !confirming.is_independent_of(&confirmed),
            "an absent document is not a distinct document"
        );
        let evidence = Evidence::from_match(
            Ground::BrokerApiAgreesWithStatement,
            confirming,
            confirmed,
            std::collections::BTreeSet::from([Dimension::Cash]),
        )
        .expect("evidence");

        let value =
            serde_json::to_value(EvidenceDto::from_domain(&evidence)).expect("evidence renders");
        assert_eq!(value["level"], "accepted_internal");
        assert!(
            value.get("confirming_document").is_none(),
            "an absent document is omitted, not blank: {value}"
        );
        assert!(
            value.get("confirmed_document").is_none(),
            "an absent document is omitted, not blank: {value}"
        );
    }

    fn answer(word: &str) -> AnswerImportQuestionRequest {
        AnswerImportQuestionRequest {
            answer: word.to_owned(),
            account: None,
            origin: None,
        }
    }

    #[test]
    fn an_answer_that_names_no_account_refuses_one() {
        // The mistake this catches is not untidiness. A caller that sends an
        // account beside `received` believes it answered
        // `received_from_own_account`, and the two say different things about
        // where the money came from. Applying the answer it typed rather than
        // the one it meant settles the row wrongly and says nothing.
        for word in ["paid", "received", "fee", "income"] {
            let request = AnswerImportQuestionRequest {
                account: Some(Uuid::new_v4()),
                ..answer(word)
            };
            let rejection = request
                .to_domain()
                .expect_err("an account the answer does not take is refused");
            assert_eq!(rejection.field, "account");
            assert!(
                rejection.expected.contains(word),
                "the refusal names the answer that was given: {rejection:?}"
            );
        }
    }

    #[test]
    fn the_two_answers_that_name_an_account_still_take_one() {
        for word in ["sent_to_own_account", "received_from_own_account"] {
            let account = Uuid::new_v4();
            let request = AnswerImportQuestionRequest {
                account: Some(account),
                ..answer(word)
            };
            let decision = request.to_domain().expect("the account is required here");
            assert!(decision.shape().needs_account());
            // And absence is still the malformed answer it always was.
            let rejection = answer(word)
                .to_domain()
                .expect_err("this answer is incomplete without an account");
            assert_eq!(rejection.field, "account");
            assert_eq!(rejection.actual, "absent");
        }
    }

    #[test]
    fn only_the_fee_answer_carries_an_origin() {
        for word in [
            "sent_to_own_account",
            "received_from_own_account",
            "paid",
            "received",
            "income",
        ] {
            let request = AnswerImportQuestionRequest {
                account: matches!(word, "sent_to_own_account" | "received_from_own_account")
                    .then(Uuid::new_v4),
                origin: Some(FeeOriginDto::Brokerage),
                ..answer(word)
            };
            let rejection = request
                .to_domain()
                .expect_err("an origin the answer does not take is refused");
            assert_eq!(rejection.field, "origin");
            // The value is named as the caller spelled it, not as Rust spells it.
            assert_eq!(rejection.actual, "brokerage");
        }
        let accepted = AnswerImportQuestionRequest {
            origin: Some(FeeOriginDto::Depositary),
            ..answer("fee")
        }
        .to_domain()
        .expect("a fee carries its origin");
        assert_eq!(
            accepted,
            Answer::Fee {
                origin: FeeOrigin::Depositary
            }
        );
    }
}
/// Report upload parameters. The route body is the workbook's binary bytes.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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
#[into_params(parameter_in = Query)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirming_document: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_document: Option<String>,
}

impl EvidenceDto {
    pub(crate) fn from_domain(evidence: &iaam_core::reconciliation::Evidence) -> Self {
        Self {
            ground: evidence.ground().code().to_owned(),
            level: evidence.level().code().to_owned(),
            dimensions: evidence
                .dimensions()
                .into_iter()
                .map(|dimension| dimension.code().to_owned())
                .collect(),
            confirming_parser: evidence.confirming().parser_version.0.clone(),
            confirmed_parser: evidence.confirmed().parser_version.0.clone(),
            confirming_document: evidence
                .confirming()
                .document
                .as_ref()
                .map(|document| document.as_str().to_owned()),
            confirmed_document: evidence
                .confirmed()
                .document
                .as_ref()
                .map(|document| document.as_str().to_owned()),
        }
    }
}

/// A money or quantity value in a claim or discrepancy.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimValueDto {
    Money {
        amount: String,
        currency: CurrencyDto,
    },
    Quantity(String),
}

/// The tagged claim asserted by a source.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimDto {
    CashBalance {
        currency: CurrencyDto,
        at: String,
        claimed: ClaimValueDto,
    },
    PositionQuantity {
        instrument: Uuid,
        custody: Uuid,
        at: String,
        claimed: ClaimValueDto,
    },
    CashTurnover {
        currency: CurrencyDto,
        debit: String,
        credit: String,
    },
    FeesTotal {
        currency: CurrencyDto,
        claimed: ClaimValueDto,
    },
    IncomeTotal {
        currency: CurrencyDto,
        claimed: ClaimValueDto,
    },
    TaxWithheldTotal {
        currency: CurrencyDto,
        claimed: ClaimValueDto,
    },
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiscrepancyDto {
    pub field: String,
    pub claimed: ClaimValueDto,
    pub observed: ClaimValueDto,
    pub delta: ClaimValueDto,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClaimOutcomeDetailDto {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discrepancy: Option<DiscrepancyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<String>,
}

/// Outcome of one control assertion.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClaimOutcomeDto {
    pub claim: ClaimDto,
    pub outcome: ClaimOutcomeDetailDto,
}

impl ClaimOutcomeDto {
    pub(crate) fn from_domain(check: &ClaimCheck) -> Self {
        let outcome = match check.outcome {
            ClaimOutcome::Matched => ClaimOutcomeDetailDto {
                code: "matched".to_owned(),
                discrepancy: None,
                reason: None,
                exception: None,
            },
            ClaimOutcome::Discrepant(discrepancy) => ClaimOutcomeDetailDto {
                code: "discrepant".to_owned(),
                discrepancy: Some(DiscrepancyDto {
                    field: discrepancy.field.to_owned(),
                    claimed: claim_value_dto(discrepancy.claimed),
                    observed: claim_value_dto(discrepancy.observed),
                    delta: claim_value_dto(discrepancy.delta),
                }),
                reason: None,
                exception: None,
            },
            ClaimOutcome::NotComparable { reason } => ClaimOutcomeDetailDto {
                code: "not_comparable".to_owned(),
                discrepancy: None,
                reason: Some(reason.code().to_owned()),
                exception: None,
            },
            ClaimOutcome::Excepted { exception } => ClaimOutcomeDetailDto {
                code: "excepted".to_owned(),
                discrepancy: None,
                reason: None,
                exception: Some(exception.code().to_owned()),
            },
        };
        Self {
            claim: claim_dto(check.claim),
            outcome,
        }
    }
}

fn money_text(amount: iaam_core::money::PostedMinor, currency: CurrencyCode) -> String {
    Money::new(amount, currency)
        .to_calc_dec()
        .inner()
        .to_string()
}

fn claim_value_dto(value: ClaimValue) -> ClaimValueDto {
    match value {
        ClaimValue::Money { amount, currency } => ClaimValueDto::Money {
            amount: money_text(amount, currency),
            currency: CurrencyDto::from_domain(currency),
        },
        ClaimValue::Quantity(quantity) => ClaimValueDto::Quantity(quantity.0.inner().to_string()),
    }
}

fn claim_dto(claim: ControlClaim) -> ClaimDto {
    match claim {
        ControlClaim::CashBalance {
            currency,
            amount,
            at,
        } => ClaimDto::CashBalance {
            currency: CurrencyDto::from_domain(currency),
            at: at.code().to_owned(),
            claimed: claim_value_dto(ClaimValue::Money { amount, currency }),
        },
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at,
        } => ClaimDto::PositionQuantity {
            instrument: instrument.inner(),
            custody: custody.inner(),
            at: at.code().to_owned(),
            claimed: claim_value_dto(ClaimValue::Quantity(quantity)),
        },
        ControlClaim::CashTurnover {
            currency,
            debit,
            credit,
        } => ClaimDto::CashTurnover {
            currency: CurrencyDto::from_domain(currency),
            debit: money_text(debit, currency),
            credit: money_text(credit, currency),
        },
        ControlClaim::FeesTotal { currency, amount } => ClaimDto::FeesTotal {
            currency: CurrencyDto::from_domain(currency),
            claimed: claim_value_dto(ClaimValue::Money { amount, currency }),
        },
        ControlClaim::IncomeTotal { currency, amount } => ClaimDto::IncomeTotal {
            currency: CurrencyDto::from_domain(currency),
            claimed: claim_value_dto(ClaimValue::Money { amount, currency }),
        },
        ControlClaim::TaxWithheldTotal { currency, amount } => ClaimDto::TaxWithheldTotal {
            currency: CurrencyDto::from_domain(currency),
            claimed: claim_value_dto(ClaimValue::Money { amount, currency }),
        },
    }
}

/// A refused row preserved in a coverage-gap projection.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RefusedRowDto {
    pub source: Uuid,
    pub row: RowNameDto,
    pub dimensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowNameDto {
    Given(String),
    Fingerprint(String),
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TaintDto {
    pub account: Uuid,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub from: Date,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub to: Date,
    pub source: Uuid,
    pub parser_version: String,
    pub dimensions: Vec<String>,
    pub refused: u32,
    pub rows: Vec<RefusedRowDto>,
}

impl TaintDto {
    pub(crate) fn from_domain(taint: &Taint) -> Self {
        Self {
            account: taint.account.inner(),
            from: taint.period.from,
            to: taint.period.to,
            source: taint.source.inner(),
            parser_version: taint.parser_version.0.clone(),
            dimensions: taint
                .dimensions
                .iter()
                .map(|dimension| dimension.code().to_owned())
                .collect(),
            refused: taint.refused,
            rows: taint.rows.iter().map(refused_row_dto).collect(),
        }
    }
}

fn refused_row_dto(row: &RefusedRow) -> RefusedRowDto {
    let row_name = match &row.key.row {
        RowName::Given(value) => RowNameDto::Given(value.clone()),
        RowName::Fingerprint(value) => RowNameDto::Fingerprint(value.clone()),
    };
    RefusedRowDto {
        source: row.key.source.inner(),
        row: row_name,
        dimensions: row
            .dimensions
            .iter()
            .map(|dimension| dimension.code().to_owned())
            .collect(),
    }
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
    pub taints: Vec<TaintDto>,
}

/// Reconciliation statuses and every effective coverage gap in the requested range.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReconciliationResponseDto {
    pub statuses: Vec<ReconciliationStatusDto>,
    pub gaps: Vec<TaintDto>,
    /// What these statuses and gaps leave outstanding, bound to the account and
    /// range that were asked for. Always present, empty included.
    pub actions: Vec<ActionDto>,
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

/// Which end of the interval a balance assertion is about. A separate type
/// because the core's `BalancePoint` knows nothing about OpenAPI and should not.
///
/// The two values are the whole domain, and the field used to be a bare
/// `String`: a caller that wanted the start of the interval had `open`,
/// `start`, `begin` and `opening` to choose from, and learned which one the
/// route meant by being refused. Enumerating them in the contract is the point.
///
/// This matters more than it did while the field was only ever written by hand:
/// the action queue presets it at both points, so the value is something a
/// caller reads out of an action and sends back, and an action whose preset the
/// contract cannot explain is an action the caller has to guess at.
///
/// Each point is explained in [`BalancePointDto::VOCABULARY`] rather than in a
/// doc comment per variant, for the reason [`AliasNamespaceDto`] gives: utoipa
/// renders a unit-variant enum as a bare list of strings and discards those
/// comments. The published schema is a `oneOf` built by
/// `vocabulary::described_vocabulary`, the same shape the verdict, refusal,
/// data-quality and namespace codes are published in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalancePointDto {
    Opening,
    Closing,
}

impl BalancePointDto {
    /// Both points with the sentence that explains each, in declaration order.
    ///
    /// The code half is taken from the domain, so the contract cannot come to
    /// disagree with what the server accepts; only the meaning is written here.
    ///
    /// The two sentences are the ones a caller most needs, and they are what a
    /// single sentence for the whole field cannot carry: the difference between
    /// the two points is not a spelling but a question of whether the interval's
    /// own events are inside the figure or outside it.
    const VOCABULARY: &'static [(&'static str, &'static str)] = &[
        (
            BalancePoint::Opening.code(),
            "The opening balance: the state before the first event in the interval. Without it the figure a report shows for the interval is a movement over the interval and not a balance at all, because the sum starts from zero rather than from what was there.",
        ),
        (
            BalancePoint::Closing.code(),
            "The closing balance: the state including the last event in the interval. This is the figure a statement prints at the foot of the period, and the one the interval's own reconciliation is compared against.",
        ),
    ];

    /// The code as it appears on the wire.
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.to_domain().code()
    }

    #[must_use]
    pub const fn to_domain(self) -> BalancePoint {
        match self {
            Self::Opening => BalancePoint::Opening,
            Self::Closing => BalancePoint::Closing,
        }
    }

    #[must_use]
    pub const fn from_domain(point: BalancePoint) -> Self {
        match point {
            BalancePoint::Opening => Self::Opening,
            BalancePoint::Closing => Self::Closing,
        }
    }
}

impl PartialSchema for BalancePointDto {
    fn schema() -> RefOr<Schema> {
        described_vocabulary(
            "Which end of the interval the assertion is about. The two points are not interchangeable: an opening figure states what was there before the interval began, a closing one states what was there after it ended, and there is no third answer. When the value comes from an action's preset it is sent back exactly as it was read.",
            Self::VOCABULARY,
        )
    }
}

impl ToSchema for BalancePointDto {}

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
    /// Which end of the interval the assertion is about. Sent back unchanged
    /// when it was read from an action's preset.
    pub at: BalancePointDto,
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

/// A classification named in the vocabulary a rule outcome uses.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClassifiedAsDto {
    pub kind: String,
    /// Receiving account, for `internal_transfer` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Uuid>,
    /// Fee origin, for `fee` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl ClassifiedAsDto {
    fn from_domain(classification: ClassifiedAs) -> Self {
        Self {
            kind: classification.kind.to_owned(),
            to: classification.to.map(|account| account.inner()),
            origin: classification.origin.map(str::to_owned),
        }
    }
}

/// One event a rule change requires correcting.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlannedCorrectionDto {
    pub event: Uuid,
    pub was: ClassifiedAsDto,
    pub becomes: ClassifiedAsDto,
}

impl PlannedCorrectionDto {
    fn from_domain(correction: PlannedCorrection) -> Self {
        Self {
            event: correction.event.inner(),
            was: ClassifiedAsDto::from_domain(correction.was),
            becomes: ClassifiedAsDto::from_domain(correction.becomes),
        }
    }
}

/// What a rule change would correct, and the fact that it has not been applied.
///
/// `applied: false` is stated rather than implied: an empty `corrections` list
/// and a list nobody acted on look the same to a client that has to guess.
/// Applying is a separate, acknowledged act — `POST /v1/corrections`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecomputePlanDto {
    pub applied: bool,
    pub corrections: Vec<PlannedCorrectionDto>,
}

impl RecomputePlanDto {
    #[must_use]
    pub fn from_domain(plan: Vec<PlannedCorrection>) -> Self {
        Self {
            applied: false,
            corrections: plan
                .into_iter()
                .map(PlannedCorrectionDto::from_domain)
                .collect(),
        }
    }
}

/// A stored rule together with the history its arrival or retirement corrects.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClassificationRuleChangeDto {
    #[serde(flatten)]
    pub rule: ClassificationRuleDto,
    pub plan: RecomputePlanDto,
}

impl ClassificationRuleChangeDto {
    #[must_use]
    pub fn from_domain(change: RuleChange) -> Self {
        Self {
            rule: ClassificationRuleDto::from_port(change.rule),
            plan: RecomputePlanDto::from_domain(change.plan),
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
    /// What this synchronisation's own verdicts leave outstanding. Always
    /// present, empty included.
    pub actions: Vec<ActionDto>,
}

impl SyncOutcomeDto {
    #[must_use]
    pub fn from_domain(outcome: iaam_app::sync::SyncOutcome, actions: Vec<ActionDto>) -> Self {
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
            actions,
        }
    }
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

/// Transport namespace of an external instrument code. A separate type because
/// the core's `AliasNamespace` knows nothing about OpenAPI and should not.
///
/// The five values are the whole domain: a code belongs to exactly one of these
/// registers, and there is no «other». Enumerating them in the contract is the
/// point — a client that has to guess the register guesses wrong.
///
/// Each register is explained in [`AliasNamespaceDto::VOCABULARY`] rather than
/// in a doc comment per variant: utoipa renders a unit-variant enum as a bare
/// list of strings and discards those comments, so a meaning written there
/// reaches a reader of this file and nobody else. The published schema is a
/// `oneOf` built by `vocabulary::described_vocabulary`, the same shape the
/// verdict, refusal and data-quality codes are published in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasNamespaceDto {
    Isin,
    MoexSecid,
    Ticker,
    Figi,
    BrokerCode,
}

impl AliasNamespaceDto {
    /// Every register with the sentence that explains it, in declaration order.
    ///
    /// The code half is taken from the domain, so the contract cannot come to
    /// disagree with what the server accepts; only the meaning is written here.
    const VOCABULARY: &'static [(&'static str, &'static str)] = &[
        (
            AliasNamespace::Isin.code(),
            "ISIN — the international securities identification number, twelve characters beginning with a country code. The register a prospectus, a depositary statement or a broker report prints by default, and the one to reach for when the document offers more than one code.",
        ),
        (
            AliasNamespace::MoexSecid.code(),
            "The Moscow Exchange security identifier, exactly as the exchange publishes it in `SECID`. It names a security on that exchange and nowhere else.",
        ),
        (
            AliasNamespace::Ticker.code(),
            "An exchange ticker. The shortest code and the least reliable one: tickers are reused between venues and reassigned over time, so a ticker identifies a security only next to the venue and the date it was read on.",
        ),
        (
            AliasNamespace::Figi.code(),
            "FIGI — the OpenFIGI instrument identifier, twelve characters. Unlike an ISIN it is never reassigned, which makes it the register to prefer when a security's history matters.",
        ),
        (
            AliasNamespace::BrokerCode.code(),
            "A broker's internal code. Different brokers use different codes for one security, so this register is meaningful only beside the broker whose report the code was read from.",
        ),
    ];

    /// The code as it appears on the wire.
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.to_domain().code()
    }

    #[must_use]
    pub const fn to_domain(self) -> AliasNamespace {
        match self {
            Self::Isin => AliasNamespace::Isin,
            Self::MoexSecid => AliasNamespace::MoexSecid,
            Self::Ticker => AliasNamespace::Ticker,
            Self::Figi => AliasNamespace::Figi,
            Self::BrokerCode => AliasNamespace::BrokerCode,
        }
    }

    #[must_use]
    pub const fn from_domain(namespace: AliasNamespace) -> Self {
        match namespace {
            AliasNamespace::Isin => Self::Isin,
            AliasNamespace::MoexSecid => Self::MoexSecid,
            AliasNamespace::Ticker => Self::Ticker,
            AliasNamespace::Figi => Self::Figi,
            AliasNamespace::BrokerCode => Self::BrokerCode,
        }
    }
}

impl PartialSchema for AliasNamespaceDto {
    fn schema() -> RefOr<Schema> {
        described_vocabulary(
            "The register an external instrument code belongs to. Together with the code itself it is the external code: neither half means anything alone, and there is no «other» register to fall back on — a code that fits none of these five is a code this route cannot resolve.",
            Self::VOCABULARY,
        )
    }
}

impl ToSchema for AliasNamespaceDto {}

/// The question the agent skill calls «resolve an external code as of a date»:
/// which instrument is behind this code, on this document's date.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveInstrumentRequest {
    /// The register the code belongs to. Together with `value` it is the
    /// **external code**: `isin` and `RU000A0JX0J2` name one register and one
    /// code in it, and neither half means anything alone.
    pub namespace: AliasNamespaceDto,
    /// The code itself, exactly as the document prints it — the other half of
    /// the external code.
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
    /// As on [`OperationDto::idempotency_key`], including the failure it makes
    /// easy: a corrected fact re-sent under a key already recorded is answered
    /// `duplicate` and changes nothing.
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

/// One page of the owner's journal.
///
/// The page carries `next` rather than a total count: counting the whole
/// journal to answer "how many more" is work nobody asked for, while the
/// position to resume from is what the caller actually needs.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JournalPageDto {
    pub rows: Vec<JournalEventReadDto>,
    /// Pass back as `after` to read the next page. Absent means this was the
    /// last page; it is not an empty string, which would read as "start again".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// A recorded journal event.
///
/// This is the fact as the journal holds it, not the operation that was
/// submitted: ingest normalises an operation into an event and keeps only the
/// event. A caller comparing this against what it posted is comparing two
/// different shapes of the same fact.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JournalEventReadDto {
    /// The event's identity, the same value ingest returned as `event_id`.
    pub event: Uuid,
    pub account: Uuid,
    /// The date the journal orders this event by.
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub effective_date: Date,
    /// Order within `effective_date`. The pair names the row uniquely and is
    /// what `next` encodes.
    pub sequence: u32,
    /// Time of day as the source stated it, when it stated one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time: Option<String>,
    /// Event family: `cash_in`, `cash_out`, `trade`, `income` and so on.
    pub kind: String,
    /// The semantic dates the fact carries. Distinct from `effective_date`,
    /// which is the ordering date.
    pub dates: JournalEventDatesDto,
    /// The movement, leg by leg, exactly as recorded. Nothing here is summed:
    /// a total would be a computed number, and this route computes none.
    pub legs: Vec<JournalLegDto>,
    pub relation: JournalRelationDto,
    pub confidence: JournalConfidenceDto,
    /// The client key supplied at ingest, if one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Identity of the source this row arrived from. Derived from the owner,
    /// the account and the channel when the caller declared a source, and
    /// minted per request when it did not.
    pub source: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_operation_id: Option<String>,
    /// The category the source itself put on the row. Evidence about what the
    /// source said, never the owner's own decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_category: Option<String>,
    /// The description or counterparty the source printed on the row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The semantic dates a journal event carries. Absent means unknown, which is
/// not the same as a date equal to the ordering date.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct JournalEventDatesDto {
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub trade: Option<Date>,
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub settled: Option<Date>,
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub cash_posted: Option<Date>,
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub entitlement: Option<Date>,
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub paid: Option<Date>,
    /// The tax year the event belongs to, when the fact states one of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tax_period: Option<i32>,
}

impl JournalEventDatesDto {
    fn from_domain(dates: iaam_core::dates::EventDates) -> Self {
        Self {
            trade: dates.trade.map(|date| date.inner()),
            settled: dates.settled.map(|date| date.inner()),
            cash_posted: dates.cash_posted.map(|date| date.inner()),
            entitlement: dates.entitlement.map(|date| date.inner()),
            paid: dates.paid.map(|date| date.inner()),
            tax_period: dates.tax_period_override.map(|period| period.0),
        }
    }
}

/// One movement leg of a journal event.
///
/// The sign is the direction: positive is into the named account or custody
/// place, negative is out of it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JournalLegDto {
    pub kind: JournalLegKindDto,
    pub account: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custody: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instrument: Option<Uuid>,
    /// The posted amount in major units, as recorded. Present on every leg
    /// that moves money.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<CurrencyDto>,
    /// The quantity moved, on a securities leg.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<String>,
}

impl JournalLegDto {
    fn from_domain(leg: &iaam_core::event::leg::Leg) -> Self {
        Self {
            kind: JournalLegKindDto::from_domain(leg.kind),
            account: leg.account.inner(),
            custody: leg.custody.map(|custody| custody.inner()),
            instrument: leg.instrument.map(|instrument| instrument.inner()),
            amount: leg
                .money
                .map(|money| money.to_calc_dec().inner().to_string()),
            currency: leg
                .money
                .map(|money| CurrencyDto::from_domain(money.currency())),
            quantity: leg.quantity.map(|quantity| quantity.0.inner().to_string()),
        }
    }
}

/// What a leg moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalLegKindDto {
    /// Cash in an account.
    Cash,
    /// Quantity of a security.
    SecurityQuantity,
    /// Outstanding principal being amortised.
    Principal,
    Fee,
    Tax,
}

impl JournalLegKindDto {
    const fn from_domain(kind: iaam_core::event::leg::LegKind) -> Self {
        match kind {
            iaam_core::event::leg::LegKind::Cash => Self::Cash,
            iaam_core::event::leg::LegKind::SecurityQuantity => Self::SecurityQuantity,
            iaam_core::event::leg::LegKind::Principal => Self::Principal,
            iaam_core::event::leg::LegKind::Fee => Self::Fee,
            iaam_core::event::leg::LegKind::Tax => Self::Tax,
        }
    }
}

/// Whether this event stands alone or corrects another.
///
/// A reader that cannot see this reads a retracted fact as a live one, which
/// is the one misreading of a journal that changes every number derived from it.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct JournalRelationDto {
    pub kind: JournalRelationKindDto,
    /// The event corrected. Present exactly when `kind` is not `none`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Uuid>,
}

impl JournalRelationDto {
    const fn from_domain(relation: iaam_core::event::Relation) -> Self {
        match relation {
            iaam_core::event::Relation::None => Self {
                kind: JournalRelationKindDto::None,
                target: None,
            },
            iaam_core::event::Relation::Reversal { target } => Self {
                kind: JournalRelationKindDto::Reversal,
                target: Some(target.inner()),
            },
            iaam_core::event::Relation::Replacement { target } => Self {
                kind: JournalRelationKindDto::Replacement,
                target: Some(target.inner()),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalRelationKindDto {
    /// A standalone fact.
    None,
    /// Reverses the event named by `target`.
    Reversal,
    /// Replaces the event named by `target`. Always follows a reversal.
    Replacement,
}

/// How far the recorded fact is confirmed by its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalConfidenceDto {
    /// The source confirms the fact.
    Known,
    /// The value was reconstructed or estimated.
    Estimated,
    /// The value is unknown, and is not to be read as zero.
    Unknown,
}

impl JournalConfidenceDto {
    const fn from_domain(confidence: iaam_core::event::Confidence) -> Self {
        match confidence {
            iaam_core::event::Confidence::Known => Self::Known,
            iaam_core::event::Confidence::Estimated => Self::Estimated,
            iaam_core::event::Confidence::Unknown => Self::Unknown,
        }
    }
}

impl JournalEventReadDto {
    #[must_use]
    pub fn from_domain(view: &iaam_app::scenarios::journal::JournalEventView) -> Self {
        Self {
            event: view.event.inner(),
            account: view.account.inner(),
            effective_date: view.effective_date,
            sequence: view.sequence,
            source_time: view.source_time.map(format_source_time),
            kind: view.kind.to_owned(),
            dates: JournalEventDatesDto::from_domain(view.dates),
            legs: view.legs.iter().map(JournalLegDto::from_domain).collect(),
            relation: JournalRelationDto::from_domain(view.relation),
            confidence: JournalConfidenceDto::from_domain(view.confidence),
            idempotency_key: view.idempotency_key.clone(),
            source: view.source.inner(),
            source_operation_id: view.source_operation_id.clone(),
            source_category: view.source_category.clone(),
            description: view.description.clone(),
        }
    }
}

/// `HH:MM:SS`, the precision a source states a time of day to. The journal
/// stores nanoseconds because its ordering needs them; a caller reading a row
/// back does not.
fn format_source_time(time: time::Time) -> String {
    let (hour, minute, second) = time.as_hms();
    format!("{hour:02}:{minute:02}:{second:02}")
}

// ---------------------------------------------------------------------------
// Import sessions (iaam-3kru, iaam-6qsa)
// ---------------------------------------------------------------------------

/// Open an import session.
///
/// The declaration is optional and means what it means everywhere else: it names
/// the account, channel and label these rows belong to. Naming it is what lets a
/// second submission of the same import reach the same session rather than
/// opening a parallel one holding half the answers.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenImportSessionRequest {
    /// Declaring it and omitting it open **two different products**, and the
    /// choice cannot be revised afterwards: a session's identity is fixed when
    /// it opens.
    ///
    /// **Declared** — a named import. The account is resolved once, here, by
    /// whatever identifier the caller has (see [`DeclaredSourceDto::account`]),
    /// and the response echoes it back. Everything the session later commits is
    /// stamped with a source derived from that account and channel, and with an
    /// import identity when a `label` was given. What this buys is retraction as
    /// a unit: `POST /v1/corrections/imports`, given the same three fields,
    /// retracts exactly this import and leaves the account's other imports in
    /// force. A labelled declaration also reaches the session it already
    /// opened, rather than opening a second one holding half the answers —
    /// **that reuse is keyed on the label**, so a declaration without one opens
    /// a fresh session on every call.
    ///
    /// **Absent** — a free session. Nothing scopes it to an account, and rows
    /// for several accounts sit in it together. This is the shape for an export
    /// that covers a whole institution: one session, questions answered once,
    /// one commit. Reading the account requirement on `DeclaredSourceDto` as a
    /// property of sessions is what turns such an export into four staged
    /// imports, and it is not one.
    ///
    /// What a free session costs is the handle. Its rows are committed under a
    /// source minted for the occasion, which is neither declared nor reported
    /// back — the session's own `source` stays absent — and under no import at
    /// all. `POST /v1/corrections/imports` keys on a declaration and so cannot
    /// reach them: a free session's rows are corrected one event at a time
    /// through `POST /v1/corrections`, having been found in the journal first.
    ///
    /// One thing a declaration does **not** buy: the session does not check that
    /// the rows fed to it name the declared account, and cannot. It stores the
    /// source and import, both one-way derivations, and the account itself is
    /// gone by the time rows arrive. A row for another account is held, and
    /// committed, under this import's identity. (The batch route does check it,
    /// because there the declaration and the rows are in one request.) Feed a
    /// declared session only the account it was declared for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DeclaredSourceDto>,
}

/// The account a declaration resolved to, named as the owner named it.
///
/// The identifier with the title beside it, because a caller that declared an
/// account by the number its bank prints has no way to check it reached the
/// right one — and the rows it is about to send have to carry this `id`. Sending
/// it back is what lets the whole import be walked without a directory read:
/// what the caller sends is what its statement prints, and what it copies into
/// the rows is what this response just handed it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeclaredAccountDto {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

/// Rows fed into a session.
///
/// The same shape the conclusive route takes, and deliberately so: a session is
/// not a second intake vocabulary, it is the same rows held rather than recorded.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddImportRowsRequest {
    pub operations: Vec<OperationDto>,
}

/// A session as the owner reads it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportSessionDto {
    pub session: Uuid,
    /// `open`, `committed` or `abandoned`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<Uuid>,
    pub opened_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// Where to read what committing this session would do, before it does it.
    ///
    /// A path on the session rather than a line of prose somewhere, and the
    /// reason is that the action queue **cannot** carry it. An item's target is
    /// an `OperationKey` resolved through `ActionCatalog::from_openapi`, which
    /// requires an `application/json` request schema of every key it registers;
    /// `assess_import_session` is a GET with no request body, so registering it
    /// would fail the catalog build at start-up rather than lead anybody
    /// anywhere. A client that reads `target` as its map of what to call could
    /// never have been brought here by any action, however the queue was worded.
    ///
    /// The cost was not theoretical. A reviewer ran a whole import without
    /// finding this route and wrote out, as a wishlist, the seven sections it
    /// already answers. Nothing named it: not the queue, not the session
    /// responses, and not the commit route, which takes a `revision` whose only
    /// source is this answer.
    ///
    /// It is here rather than on one response because `ImportSessionDto` is what
    /// the open response, the list, the session contents, the commit outcome and
    /// the assessment itself are all built from, so one field puts the route in
    /// front of every client that holds a session at all. It is valid whatever
    /// the state: `plan_session` reads a committed or abandoned session as
    /// readily as an open one, and what it would have recorded is exactly what a
    /// caller asks about after the fact.
    pub assessment: String,
    /// The account the declaration named, resolved.
    ///
    /// Present on the response that opened the session, and absent everywhere
    /// else — which is a fact about the session rather than an oversight. A
    /// session stores its `source` and `import`, and both are one-way
    /// derivations from the account: nothing read back later can recover it.
    /// The moment the server holds the account is the moment it resolved the
    /// declaration, so that is the response that carries it, and a caller
    /// wanting it afterwards asks the directory as it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<DeclaredAccountDto>,
}

impl ImportSessionDto {
    #[must_use]
    pub fn from_domain(session: &ImportSessionView) -> Self {
        Self {
            session: session.id.inner(),
            state: session.state.code().to_owned(),
            source: session.source.map(|id| id.inner()),
            import: session.import.map(|id| id.inner()),
            opened_at: session.opened_at.clone(),
            closed_at: session.closed_at.clone(),
            assessment: format!("/v1/import-sessions/{}/assessment", session.id.inner()),
            account: None,
        }
    }
}

/// One question the session is waiting on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportQuestionDto {
    pub question: Uuid,
    pub session: Uuid,
    /// The row in the session the question is about.
    pub row: u32,
    /// The question in words, with the owner's own account titles in it.
    pub prompt: String,
    pub alternatives: Vec<AnswerAlternativeDto>,
    pub asked_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
    /// The classification rule the answer created, so the same counterparty is
    /// not asked about twice.
    ///
    /// Absent for either of two reasons, and a client cannot tell them apart
    /// from this field alone — it knows which applies because it knows what
    /// token it holds:
    ///
    /// - the row offered nothing a rule can match on, so no rule was written
    ///   under any scope: a rule that asks nothing matches nothing, and writing
    ///   one would record a decision that never applies;
    /// - the answer came in under an agent token, which settles the row and
    ///   generalises nothing (`iaam-hnod`). A standing rule decides rows nobody
    ///   has looked at, and that is the owner's judgement, made with his own
    ///   token through `POST /v1/classification-rules`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<Uuid>,
}

impl ImportQuestionDto {
    #[must_use]
    pub fn from_domain(question: &ImportQuestionView) -> Self {
        Self {
            question: question.id.inner(),
            session: question.session.inner(),
            row: question.row,
            prompt: question.prompt.clone(),
            alternatives: serde_json::from_str::<Vec<AnswerShape>>(&question.alternatives)
                .unwrap_or_default()
                .into_iter()
                .map(AnswerAlternativeDto::from_domain)
                .collect(),
            asked_at: question.asked_at.clone(),
            answered_at: question.answered_at.clone(),
            rule: question
                .rule
                .as_deref()
                .and_then(|id| Uuid::parse_str(id).ok()),
        }
    }
}

/// Everything a session holds.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportSessionContentsDto {
    #[serde(flatten)]
    pub session: ImportSessionDto,
    /// How many rows are held, conclusive and observed together.
    pub rows: usize,
    /// Every question, answered and unanswered.
    pub questions: Vec<ImportQuestionDto>,
    /// How many are still waiting on the owner. Commit refuses while this is
    /// not zero.
    pub unanswered: usize,
}

/// The owner's answer to one question.
///
/// `answer` is one of the words the question published in its alternatives.
/// `account` is required by exactly the two that name one, and refused by the
/// rest: an answer carrying an account the question does not take is a caller
/// mistake worth naming rather than ignoring. `origin` follows the same rule
/// for the one answer that takes it.
///
/// The refusal is the point rather than tidiness. An LLM client resends fields
/// it no longer needs, and a superfluous `account` beside `received` is the
/// signature of a caller that believes it answered `received_from_own_account`
/// — the difference between money arriving from outside the perimeter and money
/// arriving from another of the owner's own accounts. Accepting it would settle
/// the row as the answer it did not mean, silently, and every report would then
/// be computed from that.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnswerImportQuestionRequest {
    pub answer: String,
    /// The owner's account on the other side, for `sent_to_own_account` and
    /// `received_from_own_account` only. Required by those two, refused by the
    /// other four.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Uuid>,
    /// Where a fee came from, for the `fee` answer only. Refused on the other
    /// five; optional on `fee` itself, where absence means the origin was not
    /// stated and the fee is recorded as `other` rather than as a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<FeeOriginDto>,
}

impl AnswerImportQuestionRequest {
    /// Conversion to the owner's decision.
    ///
    /// The account is checked here rather than deeper: a missing one is a
    /// malformed answer, and a superfluous one is a caller that believes it
    /// answered something else.
    ///
    /// Both checks read [`AnswerShape`] rather than re-listing which answers
    /// take what. The list already exists — it is what a question publishes in
    /// its alternatives — and a second copy here would eventually admit a field
    /// the published alternative said nothing about.
    pub fn to_domain(&self) -> Result<Answer, Rejection> {
        let account = self.account.map(AccountId);
        let answer = match self.answer.as_str() {
            "sent_to_own_account" => Answer::SentToOwnAccount {
                to: account.ok_or_else(|| self.missing_account())?,
            },
            "received_from_own_account" => Answer::ReceivedFromOwnAccount {
                from: account.ok_or_else(|| self.missing_account())?,
            },
            "paid" => Answer::Paid,
            "received" => Answer::Received,
            "fee" => Answer::Fee {
                origin: self
                    .origin
                    .map_or(FeeOrigin::Other, FeeOriginDto::to_domain),
            },
            "income" => Answer::Income,
            other => {
                return Err(Rejection {
                    field: "answer".into(),
                    expected: "sent_to_own_account, received_from_own_account, paid, \
                               received, fee or income"
                        .into(),
                    actual: other.to_owned(),
                });
            }
        };
        // A field the answer does not take, refused rather than dropped. The
        // answer word is validated first, so the refusal can name the answer the
        // caller actually gave instead of guessing at what it meant.
        let shape = answer.shape();
        if let Some(superfluous) = self.account.filter(|_| !shape.needs_account()) {
            return Err(Rejection {
                field: "account".into(),
                expected: format!("no account: the `{}` answer names none", shape.code()),
                actual: superfluous.to_string(),
            });
        }
        if let Some(superfluous) = self.origin.filter(|_| shape != AnswerShape::Fee) {
            return Err(Rejection {
                field: "origin".into(),
                expected: format!(
                    "no origin: only the `{}` answer carries one",
                    AnswerShape::Fee.code()
                ),
                actual: wire_word(superfluous),
            });
        }
        Ok(answer)
    }

    fn missing_account(&self) -> Rejection {
        Rejection {
            field: "account".into(),
            expected: "the owner's account this answer names".into(),
            actual: "absent".into(),
        }
    }
}

/// The word a serialisable value goes over the wire as.
///
/// Read back out of `serde` rather than written out again beside the type: a
/// rejection must name the value the caller sent, and a hand-copied list of
/// wire words drifts from the `rename_all` that produces them.
fn wire_word<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|json| json.as_str().map(str::to_owned))
        .unwrap_or_else(|| "a value".to_owned())
}

/// What committing a session wrote.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportCommitDto {
    #[serde(flatten)]
    pub session: ImportSessionDto,
    /// The reading of the session this commit was planned from. A caller that
    /// sent one has it echoed; a caller that sent none learns what it wrote.
    pub revision: String,
    /// A verdict per held row, in the order the rows were fed.
    pub rows: Vec<VerdictDto>,
}

/// One row a session is holding.
///
/// Deliberately not a [`VerdictDto`]. A verdict answers "what was recorded", and
/// the answer for every row a session holds is "nothing, yet" — which is not
/// `quarantined`, whose published meaning is that no fact *could* be written
/// from the row. A held row will be written, at commit and at no other moment.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportRowDto {
    /// The row's position in the session, one-based.
    pub row: u32,
    /// `held`, `needs_classification` or `rejected`.
    pub state: String,
    /// The question this row raised, for `needs_classification`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<Vec<AnswerAlternativeDto>>,
    /// Why the row could not be read, for `rejected`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
}

impl ImportRowDto {
    #[must_use]
    pub fn from_domain(held: &HeldRow) -> Self {
        let base = Self {
            row: held.row(),
            state: String::new(),
            question_id: None,
            prompt: None,
            alternatives: None,
            field: None,
            expected: None,
            actual: None,
        };
        match held {
            HeldRow::Held { .. } => Self {
                state: "held".to_owned(),
                ..base
            },
            HeldRow::Questioned { asked } => Self {
                state: "needs_classification".to_owned(),
                question_id: Some(asked.question.inner()),
                prompt: Some(asked.prompt.clone()),
                alternatives: Some(
                    asked
                        .alternatives
                        .iter()
                        .copied()
                        .map(AnswerAlternativeDto::from_domain)
                        .collect(),
                ),
                ..base
            },
            HeldRow::Rejected { rejection, .. } => Self {
                state: "rejected".to_owned(),
                field: Some(rejection.field.clone()),
                expected: Some(rejection.expected.clone()),
                actual: Some(rejection.actual.clone()),
                ..base
            },
        }
    }
}

// ---------------------------------------------------------------------------
// The import assessment (iaam-k1xa) and transfer pairing (iaam-3ul2)
// ---------------------------------------------------------------------------

/// Commit a session, optionally against the reading the caller approved.
///
/// `revision` is the stamp the assessment carried. Sent, a commit whose plan no
/// longer matches it is refused: the rows, the answers or the owner's directory
/// changed between the reading and the writing, so what would be recorded is not
/// what was approved. Omitted, the commit proceeds and the answer says which
/// revision it wrote under.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CommitImportSessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// What an import will and will not record.
///
/// Seven sections, and they are separate because their answers can disagree: a
/// row can be interpretable and on an account no contour covers, or resolved and
/// already in the journal under its key.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportPlanDto {
    #[serde(flatten)]
    pub session: ImportSessionDto,
    /// The stamp to send back to the commit route.
    pub revision: String,
    pub source_inventory: SourceInventoryDto,
    pub account_resolution: AccountResolutionDto,
    pub scope_assessment: ScopeAssessmentDto,
    pub interpretation: InterpretationDto,
    /// Transfer candidates, and the cash movements nothing was proposed
    /// against — ordinarily most of the rows, and not pending work.
    pub cross_source_matching: CrossSourceMatchingDto,
    pub commit_delta: CommitDeltaDto,
    /// `ready`, `blocked` or `requires_owner_decision`.
    pub readiness: String,
    /// Why, for `blocked` and `requires_owner_decision`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_detail: Option<String>,
}

/// What the session's rows turned out to name.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceInventoryDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<Uuid>,
    pub documents: Vec<String>,
    pub rows: usize,
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub period_from: Option<Date>,
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub period_to: Option<Date>,
    pub accounts: Vec<Uuid>,
}

/// What the rows' accounts resolved to.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccountResolutionDto {
    pub resolved: Vec<Uuid>,
    /// Named by a row and absent from the owner's directory.
    pub missing: Vec<Uuid>,
    /// Counterparty strings naming more than one of the owner's accounts.
    pub conflicting: Vec<String>,
}

/// Where the rows' accounts stand relative to the reporting perimeter.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScopeAssessmentDto {
    pub in_contour: Vec<Uuid>,
    pub explicitly_outside: Vec<Uuid>,
    pub awaiting_disposition: Vec<Uuid>,
}

/// What each row was read as, and what is still unread.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InterpretationDto {
    pub resolved: Vec<PlannedFactDto>,
    pub open_questions: Vec<OpenQuestionDto>,
}

/// One question the session is waiting on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenQuestionDto {
    pub row: u32,
    pub question: Uuid,
    pub prompt: String,
}

/// One fact the commit would write.
///
/// No event identifier: it is minted at commit, and naming one here would name a
/// fact that does not exist.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlannedFactDto {
    pub row: u32,
    pub account: Uuid,
    /// The event kind, in the journal's own vocabulary.
    pub records_as: String,
    /// Signed cash this row moves on its own account, as a decimal string.
    pub amount: String,
    pub currency: CurrencyDto,
    #[serde(with = "iso_date::option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = Date)]
    pub date: Option<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// What the journal gains, and what it does not.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CommitDeltaDto {
    pub facts: Vec<PlannedFactDto>,
    /// Rows whose key the journal already holds. They commit to `duplicate` and
    /// add nothing.
    pub duplicates: Vec<PlannedFactDto>,
    /// Rows the session keeps and the journal will not receive.
    pub retained_unrecorded: Vec<RetainedRowDto>,
}

/// A row that stays in the session and becomes no fact.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RetainedRowDto {
    pub row: u32,
    /// `unreadable` or `unanswered`.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
}

/// Transfers proposed out of two one-sided movements, and the movements nothing
/// paired with.
///
/// Both halves are published. A leg that vanished from the answer because
/// nothing matched it is a leg the owner reads as an external flow by default,
/// which is the defect rather than the fix.
///
/// **`without_counterpart` is a state, not a queue.** Every cash movement
/// carrying a posting date is offered to the matcher, because nothing printed in
/// a row says whether it is half of a transfer: a payment in a shop and the
/// outgoing leg of a transfer between two of the owner's banks are the same row
/// until a counterpart turns up on another account. So a movement no counterpart
/// was proposed for is listed there, and for everything that is not a transfer —
/// a card payment, a salary, a cash withdrawal — that is its correct and
/// permanent state. There is nothing for the owner to do about it.
///
/// The ordinary shape of an import containing no transfers is therefore
/// `candidates: []` beside a `without_counterpart` holding every row of it: five
/// deposits and withdrawals produce five legs with no counterpart and nothing to
/// confirm. What deserves attention is `candidates`, which are the pairs put to
/// the owner to judge, and, inside an import session, the readiness that counts
/// them.
///
/// The field was called `unmatched`, and the paragraph above used to be twice
/// this length because the name was working against it: under a parent called
/// `cross_source_matching`, `unmatched` reads as *failed to match* or *still
/// being worked on*, and an external agent read it exactly that way in spite of
/// the documentation. [`TransferLegDto`] below records the opposite decision —
/// an imprecise wire name kept rather than broken — and the two are consistent:
/// a type name that overstates what a row is costs a reader a second, while a
/// field name that reads as pending work cost a false alarm on every import the
/// owner made, for as long as the name stood.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CrossSourceMatchingDto {
    /// Pairs proposed for the owner to confirm. Empty is the ordinary case: most
    /// sources contain no transfer between two of the owner's own accounts.
    pub candidates: Vec<PairingCandidateDto>,
    /// Cash movements for which no counterpart was proposed — permanently, for
    /// every movement that is not half of a transfer.
    ///
    /// A long list here beside an empty `candidates` says only that the source
    /// held no transfer, which is what most sources hold.
    pub without_counterpart: Vec<TransferLegDto>,
}

/// One side of a cash movement, which may or may not be half of a transfer.
///
/// Rendered from any recorded or planned `CashOut` or `CashIn` that carries a
/// posting date, so appearing here — in `without_counterpart` above all —
/// asserts nothing about the row beyond its having moved cash on a day.
///
/// The domain type behind it is called `CashLeg` for exactly that reason: naming
/// it after transfers claimed of every row the thing the owner alone decides.
/// The two names differ on purpose, and this one keeps `Transfer` because it is
/// published in the OpenAPI schema and clients hold it; renaming the wire would
/// break them for a word. That the field beside it *was* renamed is not a
/// reversal: what is bought here is a more accurate noun, and what was bought
/// there was the end of a recurring false alarm.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TransferLegDto {
    /// The journal event, when the leg is already recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<Uuid>,
    /// The session and row, when the leg is still an observation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<u32>,
    pub account: Uuid,
    /// `in` or `out`.
    pub direction: String,
    pub amount: String,
    pub currency: CurrencyDto,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub date: Date,
    /// What the source printed beside the row. Evidence, never matched on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<Uuid>,
}

/// Two legs proposed as one movement, and what the proposal rests on.
///
/// A proposal and nothing more: appearing here relates the two legs in no way
/// the journal knows about, and both go on counting separately until the owner
/// confirms the pairing.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PairingCandidateDto {
    pub outgoing: TransferLegDto,
    pub incoming: TransferLegDto,
    pub evidence: PairingEvidenceDto,
}

/// What the two legs agree on.
///
/// The fields, not a score: a confidence number would be an opinion the owner
/// cannot check, and these are checkable.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PairingEvidenceDto {
    pub amount: String,
    pub currency: CurrencyDto,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub outgoing_date: Date,
    #[serde(with = "iso_date")]
    #[schema(value_type = String, format = Date)]
    pub incoming_date: Date,
    pub days_apart: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outgoing_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incoming_reference: Option<String>,
    /// Whether each leg has exactly this one counterpart. `false` means the
    /// candidates cannot all be true.
    pub sole_candidate: bool,
}

/// The owner relating two recorded legs.
///
/// `acknowledge_retraction` is required for the same reason every correction
/// requires it: confirming stops both one-sided movements counting, in reports
/// the owner has already read.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfirmTransferPairingRequest {
    pub outgoing: Uuid,
    pub incoming: Uuid,
    #[serde(default)]
    pub acknowledge_retraction: bool,
}

/// What confirming one pairing wrote.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfirmedPairingDto {
    pub outgoing: Uuid,
    pub incoming: Uuid,
    /// The transfer that supersedes the outgoing leg. Absent when the journal
    /// already held it, which is what confirming one pairing twice looks like.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<Uuid>,
}

impl ImportPlanDto {
    #[must_use]
    pub fn from_domain(plan: &ImportPlan) -> Self {
        let (readiness_detail, readiness) = match &plan.readiness {
            Readiness::Ready => (None, plan.readiness.code()),
            Readiness::Blocked { reason } => (Some(reason.clone()), plan.readiness.code()),
            Readiness::RequiresOwnerDecision {
                unanswered_questions,
                transfer_candidates,
            } => (
                Some(format!(
                    "{unanswered_questions} question(s) unanswered, \
                     {transfer_candidates} transfer candidate(s) unconfirmed"
                )),
                plan.readiness.code(),
            ),
        };
        Self {
            session: ImportSessionDto::from_domain(&plan.session),
            revision: plan.revision.0.clone(),
            source_inventory: SourceInventoryDto {
                source: plan.source_inventory.source.map(|id| id.inner()),
                import: plan.source_inventory.import.map(|id| id.inner()),
                documents: plan.source_inventory.documents.clone(),
                rows: plan.source_inventory.rows,
                period_from: plan.source_inventory.period.map(|(from, _)| from),
                period_to: plan.source_inventory.period.map(|(_, to)| to),
                accounts: plan
                    .source_inventory
                    .accounts
                    .iter()
                    .map(|id| id.inner())
                    .collect(),
            },
            account_resolution: AccountResolutionDto {
                resolved: plan
                    .account_resolution
                    .resolved
                    .iter()
                    .map(|id| id.inner())
                    .collect(),
                missing: plan
                    .account_resolution
                    .missing
                    .iter()
                    .map(|id| id.inner())
                    .collect(),
                conflicting: plan.account_resolution.conflicting.clone(),
            },
            scope_assessment: ScopeAssessmentDto {
                in_contour: plan
                    .scope_assessment
                    .in_contour
                    .iter()
                    .map(|id| id.inner())
                    .collect(),
                explicitly_outside: plan
                    .scope_assessment
                    .explicitly_outside
                    .iter()
                    .map(|id| id.inner())
                    .collect(),
                awaiting_disposition: plan
                    .scope_assessment
                    .awaiting_disposition
                    .iter()
                    .map(|id| id.inner())
                    .collect(),
            },
            interpretation: InterpretationDto {
                resolved: plan
                    .interpretation
                    .resolved
                    .iter()
                    .map(PlannedFactDto::from_domain)
                    .collect(),
                open_questions: plan
                    .interpretation
                    .open_questions
                    .iter()
                    .map(|open| OpenQuestionDto {
                        row: open.row,
                        question: open.question.inner(),
                        prompt: open.prompt.clone(),
                    })
                    .collect(),
            },
            cross_source_matching: CrossSourceMatchingDto::from_domain(&plan.cross_source_matching),
            commit_delta: CommitDeltaDto {
                facts: plan
                    .commit_delta
                    .facts
                    .iter()
                    .map(PlannedFactDto::from_domain)
                    .collect(),
                duplicates: plan
                    .commit_delta
                    .duplicates
                    .iter()
                    .map(PlannedFactDto::from_domain)
                    .collect(),
                retained_unrecorded: plan
                    .commit_delta
                    .retained_unrecorded
                    .iter()
                    .map(RetainedRowDto::from_domain)
                    .collect(),
            },
            readiness: readiness.to_owned(),
            readiness_detail,
        }
    }
}

impl PlannedFactDto {
    #[must_use]
    pub fn from_domain(fact: &PlannedFact) -> Self {
        Self {
            row: fact.row,
            account: fact.account.inner(),
            records_as: fact.records_as.to_owned(),
            amount: minor_amount(fact.amount_minor, fact.currency),
            currency: CurrencyDto::from_domain(fact.currency),
            date: fact.date,
            idempotency_key: fact.idempotency_key.clone(),
        }
    }
}

impl RetainedRowDto {
    #[must_use]
    pub fn from_domain(retained: &RetainedRow) -> Self {
        match &retained.reason {
            RetentionReason::Unreadable {
                field,
                expected,
                actual,
            } => Self {
                row: retained.row,
                reason: "unreadable".to_owned(),
                question: None,
                field: Some(field.clone()),
                expected: Some(expected.clone()),
                actual: Some(actual.clone()),
            },
            RetentionReason::Unanswered { question } => Self {
                row: retained.row,
                reason: "unanswered".to_owned(),
                question: Some(question.inner()),
                field: None,
                expected: None,
                actual: None,
            },
        }
    }
}

impl CrossSourceMatchingDto {
    #[must_use]
    pub fn from_domain(proposals: &Proposals) -> Self {
        Self {
            candidates: proposals
                .candidates
                .iter()
                .map(|candidate| PairingCandidateDto {
                    outgoing: TransferLegDto::from_domain(&candidate.outgoing),
                    incoming: TransferLegDto::from_domain(&candidate.incoming),
                    evidence: PairingEvidenceDto {
                        amount: minor_amount(
                            candidate.evidence.amount_minor,
                            candidate.evidence.currency,
                        ),
                        currency: CurrencyDto::from_domain(candidate.evidence.currency),
                        outgoing_date: candidate.evidence.outgoing_date,
                        incoming_date: candidate.evidence.incoming_date,
                        days_apart: candidate.evidence.days_apart,
                        outgoing_reference: candidate.evidence.outgoing_reference.clone(),
                        incoming_reference: candidate.evidence.incoming_reference.clone(),
                        sole_candidate: candidate.evidence.sole_candidate,
                    },
                })
                .collect(),
            without_counterpart: proposals
                .unmatched
                .iter()
                .map(TransferLegDto::from_domain)
                .collect(),
        }
    }
}

impl TransferLegDto {
    #[must_use]
    pub fn from_domain(leg: &CashLeg) -> Self {
        let (event, session, row) = match leg.origin {
            LegOrigin::Recorded { event } => (Some(event.inner()), None, None),
            LegOrigin::Observed { session, row } => (None, Some(session.inner()), Some(row)),
        };
        Self {
            event,
            session,
            row,
            account: leg.account.inner(),
            direction: match leg.direction {
                Movement::In => "in".to_owned(),
                Movement::Out => "out".to_owned(),
            },
            amount: minor_amount(leg.amount_minor, leg.currency),
            currency: CurrencyDto::from_domain(leg.currency),
            date: leg.date,
            reference: leg.reference.clone(),
            import: leg.import.map(|id| id.inner()),
        }
    }
}

impl ConfirmedPairingDto {
    #[must_use]
    pub fn from_domain(confirmed: &ConfirmedPairing) -> Self {
        Self {
            outgoing: confirmed.outgoing.inner(),
            incoming: confirmed.incoming.inner(),
            transfer: confirmed.transfer.map(|id| id.inner()),
        }
    }
}

/// An amount in minor units as the decimal string this API publishes.
///
/// The same rendering every other amount uses: a JSON number would stop being
/// the fact it states, because `0.1` is not one tenth in binary floating point.
fn minor_amount(minor: i64, currency: CurrencyCode) -> String {
    Money::new(PostedMinor::new(minor), currency)
        .to_calc_dec()
        .inner()
        .to_string()
}
