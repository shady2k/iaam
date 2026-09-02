//! OpenAPI spec generated from handler types (§17.1).
//!
//! Generation eliminates **data schema** discrepancies, but not behavioural ones:
//! runtime response codes, authentication requirements, and actual
//! serialisation of custom types remain outside generation. Therefore,
//! black-box contract tests exist (task 15).

use utoipa::Modify;
use utoipa::OpenApi;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

use crate::dto::{
    AccountBalanceDto, AccountCandidateDto, AccountDto, AccountResidualDto, ActionDto,
    ActionTargetDto, ActionsResponseDto, AmountDto, BalanceCashDto, BalancesReportDto,
    BasisCertaintyDto, BasisTransferRuleDto, BondPositionMetricsDto, BondScenarioResultDto,
    BrokerAccessDto, BrokerEnvironmentDto, BrokerSyncRequest, CalcMoneyDto, CategoryAmountDto,
    CategoryDto, CategoryMoveDto, CategoryRequest, CategoryRuleDto, CategoryRuleImpactDto,
    CategoryRuleRequest, CertaintyDto, ClaimDto, ClaimOutcomeDetailDto, ClaimOutcomeDto,
    ClaimValueDto, ClassificationRuleDto, ClassificationRuleRequest, ComputedCalcMoneyDto,
    ComputedDto, ComputedLifetimeCohortMetricsDto, ComputedZeroReinvestmentMetricsDto,
    ContourVersionDto, CorporateActionDto, CorrectImportRequest, CorrectionDto,
    CreateAccountRequest, CreateContourVersionRequest, CreateInstrumentRequest, CreateTokenRequest,
    CurrencyDto, CustodyRepairCaseDto, CustodyRepairOutcomeDto, CustodyRepairRequest,
    DataQualityDto, DateCertaintyDto, DeclaredSourceDto, DimensionStatusDto, DiscrepancyDto,
    DocumentDto, EvaluatedPositionDto, EvidenceDto, ExecutabilitySharesDto, ExpectedPostingDto,
    FeeOriginDto, FractionalTreatmentDto, FxRateDto, HealthDto, ImportCorrectionDto, IncomeKindDto,
    IrrLabelDto, IssuedTokenDto, JournalConfidenceDto, JournalEventDatesDto, JournalEventDto,
    JournalEventReadDto, JournalFactDto, JournalLegDto, JournalLegKindDto, JournalPageDto,
    JournalRelationDto, JournalRelationKindDto, KnowledgeDto, LegacyDerivedPositionDto,
    LifetimeCohortMetricDto, LiquidationEstimateDto, MarketFxDto, MarketFxSeriesDto,
    MarketKeyRateDto, MarketKeyRateSeriesDto, MarketPriceDto, MarketPriceSeriesDto,
    MarketSourceDto, MarketSyncRequest, MissingInputDto, MoneyFlowCurrencyDto, MoneyFlowReportDto,
    NegativeCashDto, NotDecomposedAccountDto, NotDecomposedDto, OfferChoiceDto,
    OpeningAssertionsDto, OperationDatesDto, OperationDto, OperationKindDto, OwnerBalanceRequest,
    PerimeterRefusalDto, PositionCoverageDto, PostingKindDto, PriceFreshnessDto, PriceOriginDto,
    PriceProvenanceDto, PriceQualityDto, PriceSelectionDto, ProspectiveMetricDto,
    QuotationBasisDto, QuotationBasisStatusDto, RateDto, ReconciliationResponseDto,
    ReconciliationStatusDto, RefusedRowDto, RequestPlanDto, ReturnsReportDto, RowNameDto,
    SelectedPriceDto, SubmitCorrectionsRequest, SubmitJournalEventsRequest,
    SubmitOperationsRequest, SyncOutcomeDto, TaintDto, TokenDto, TristateDto, UncoveredPositionDto,
    VerdictDto, ZeroReinvestmentMetricsDto,
};
use crate::error::ApiError;
use crate::routes::MarketSyncOutcomeDto;
use crate::vocabulary::{
    DataQualityStatusDto, NegativeCashClassificationDto, NotComputableCodeDto, VerdictCodeDto,
};

/// Authentication scheme. Declared separately: `utoipa` generates it
/// from types, but the `Bearer` requirement cannot be expressed by a type.
pub struct BearerSecurity;

impl Modify for BearerSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("opaque")
                        .description(Some(
                            "Bearer token issued at the console by `iaam claim --label <label>`; no API route issues one.",
                        ))
                        .build(),
                ),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "IAAM",
        version = "1.0.0",
        description = "Personal accounting over a contour the owner draws: what entered it, \
                       what left it, what the money was for, and what it all returned before tax. \
                       Stage 1: cash flows, categorised spending and income, and pre-tax XIRR."
    ),
    modifiers(&BearerSecurity),
    components(schemas(
        AccountDto,
        AccountCandidateDto,
        ActionDto,
        ActionTargetDto,
        ActionsResponseDto,
        MissingInputDto,
        RequestPlanDto,
        AccountBalanceDto,
        BalanceCashDto,
        BalancesReportDto,
        NegativeCashDto,
        PerimeterRefusalDto,
        NegativeCashClassificationDto,
        AccountResidualDto,
        BrokerEnvironmentDto,
        ApiError,
        BrokerAccessDto,
        BrokerSyncRequest,
        CategoryAmountDto,
        CategoryDto,
        CategoryMoveDto,
        CategoryRuleDto,
        CategoryRuleImpactDto,
        CategoryRuleRequest,
        CategoryRequest,
        ClaimDto,
        ClaimOutcomeDetailDto,
        ClaimOutcomeDto,
        ClaimValueDto,
        ClassificationRuleDto,
        ClassificationRuleRequest,
        ComputedDto,
        ContourVersionDto,
        CreateAccountRequest,
        CreateContourVersionRequest,
        CreateInstrumentRequest,
        CreateTokenRequest,
        CurrencyDto,
        CorrectImportRequest,
        CorrectionDto,
        CustodyRepairCaseDto,
        CustodyRepairOutcomeDto,
        CustodyRepairRequest,
        DeclaredSourceDto,
        ImportCorrectionDto,
        DataQualityDto,
        DimensionStatusDto,
        DiscrepancyDto,
        DocumentDto,
        EvaluatedPositionDto,
        EvidenceDto,
        ExecutabilitySharesDto,
        AmountDto,
        BondPositionMetricsDto,
        BondScenarioResultDto,
        CalcMoneyDto,
        ComputedCalcMoneyDto,
        ComputedLifetimeCohortMetricsDto,
        ComputedZeroReinvestmentMetricsDto,
        BasisTransferRuleDto,
        ExpectedPostingDto,
        CorporateActionDto,
        FeeOriginDto,
        FractionalTreatmentDto,
        FxRateDto,
        HealthDto,
        IncomeKindDto,
        IrrLabelDto,
        JournalConfidenceDto,
        JournalEventDatesDto,
        JournalEventDto,
        JournalEventReadDto,
        JournalFactDto,
        JournalLegDto,
        JournalLegKindDto,
        JournalPageDto,
        JournalRelationDto,
        JournalRelationKindDto,
        IssuedTokenDto,
        LegacyDerivedPositionDto,
        LiquidationEstimateDto,
        MarketFxDto,
        MarketFxSeriesDto,
        LifetimeCohortMetricDto,
        MarketKeyRateDto,
        MarketKeyRateSeriesDto,
        MarketPriceDto,
        MarketPriceSeriesDto,
        MarketSourceDto,
        MarketSyncOutcomeDto,
        OfferChoiceDto,
        MarketSyncRequest,
        OperationDatesDto,
        OperationDto,
        BasisCertaintyDto,
        CertaintyDto,
        DateCertaintyDto,
        TristateDto,
        KnowledgeDto,
        OpeningAssertionsDto,
        OperationKindDto,
        OwnerBalanceRequest,
        PriceFreshnessDto,
        PriceOriginDto,
        PriceProvenanceDto,
        PriceQualityDto,
        PostingKindDto,
        PriceSelectionDto,
        ProspectiveMetricDto,
        QuotationBasisStatusDto,
        QuotationBasisDto,
        PositionCoverageDto,
        RateDto,
        ReconciliationResponseDto,
        ReconciliationStatusDto,
        RefusedRowDto,
        RowNameDto,
        ReturnsReportDto,
        SelectedPriceDto,
        SubmitCorrectionsRequest,
        SubmitJournalEventsRequest,
        SubmitOperationsRequest,
        SyncOutcomeDto,
        MoneyFlowCurrencyDto,
        MoneyFlowReportDto,
        NotDecomposedAccountDto,
        NotDecomposedDto,
        ZeroReinvestmentMetricsDto,
        TaintDto,
        TokenDto,
        UncoveredPositionDto,
        VerdictDto,
        // The published vocabularies: every code the API can return, each with
        // the sentence that explains it (§13).
        VerdictCodeDto,
        NotComputableCodeDto,
        DataQualityStatusDto,
    ))
)]
pub struct ApiDoc;
