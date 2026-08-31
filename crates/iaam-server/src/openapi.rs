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
    AccountDto, AddBrokerAccessRequest, AmountDto, BasisCertaintyDto, BasisTransferRuleDto,
    BondPositionMetricsDto, BondScenarioResultDto, BrokerAccessDto, BrokerAccessUpdateRequest,
    BrokerEnvironmentDto, BrokerSyncRequest, CalcMoneyDto, CertaintyDto, ClaimOutcomeDto,
    ClaimRequest, ClassificationRuleDto, ClassificationRuleRequest, ComputedCalcMoneyDto,
    ComputedDto, ComputedLifetimeCohortMetricsDto, ComputedZeroReinvestmentMetricsDto,
    ContourVersionDto, CorporateActionDto, CreateAccountRequest, CreateContourVersionRequest,
    CreateInstrumentRequest, CreateTokenRequest, CurrencyDto, CustodyRepairCaseDto,
    CustodyRepairOutcomeDto, CustodyRepairRequest, DataQualityDto, DateCertaintyDto,
    DimensionStatusDto, DocumentDto, EvaluatedPositionDto, EvidenceDto, ExecutabilitySharesDto,
    ExpectedPostingDto, FeeOriginDto, FractionalTreatmentDto, FxRateDto, HealthDto, IncomeKindDto,
    IrrLabelDto, IssuedTokenDto, JournalEventDto, JournalFactDto, KnowledgeDto,
    LegacyDerivedPositionDto, LifetimeCohortMetricDto, LiquidationEstimateDto, MarketFxDto,
    MarketKeyRateDto, MarketPriceDto, MarketSourceDto, MarketSyncRequest, OfferChoiceDto,
    OpeningAssertionsDto, OperationDatesDto, OperationDto, OperationKindDto, OwnerBalanceRequest,
    PositionCoverageDto, PostingKindDto, PriceFreshnessDto, PriceOriginDto, PriceProvenanceDto,
    PriceQualityDto, PriceSelectionDto, ProspectiveMetricDto, QuotationBasisDto,
    QuotationBasisStatusDto, RateDto, ReconciliationStatusDto, ReturnsReportDto, SelectedPriceDto,
    SubmitJournalEventsRequest, SubmitOperationsRequest, SyncOutcomeDto, TokenDto, TristateDto,
    UncoveredPositionDto, VerdictDto, ZeroReinvestmentMetricsDto,
};
use crate::error::ApiError;
use crate::routes::MarketSyncOutcomeDto;

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
                        .description(Some("Agent token. Issued and revoked by the owner (§14)."))
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
        description = "Investment accounting. Stage 1: cash flows and pre-tax XIRR."
    ),
    modifiers(&BearerSecurity),
    components(schemas(
        AccountDto,
        AddBrokerAccessRequest,
        BrokerEnvironmentDto,
        ApiError,
        BrokerAccessDto,
        BrokerAccessUpdateRequest,
        BrokerSyncRequest,
        ClaimOutcomeDto,
        ClaimRequest,
        ClassificationRuleDto,
        ClassificationRuleRequest,
        ComputedDto,
        ContourVersionDto,
        CreateAccountRequest,
        CreateContourVersionRequest,
        CreateInstrumentRequest,
        CreateTokenRequest,
        CurrencyDto,
        CustodyRepairCaseDto,
        CustodyRepairOutcomeDto,
        CustodyRepairRequest,
        DataQualityDto,
        DimensionStatusDto,
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
        JournalEventDto,
        JournalFactDto,
        IssuedTokenDto,
        LegacyDerivedPositionDto,
        LiquidationEstimateDto,
        MarketFxDto,
        LifetimeCohortMetricDto,
        MarketKeyRateDto,
        MarketPriceDto,
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
        ReconciliationStatusDto,
        ReturnsReportDto,
        SelectedPriceDto,
        SubmitJournalEventsRequest,
        SubmitOperationsRequest,
        SyncOutcomeDto,
        ZeroReinvestmentMetricsDto,
        TokenDto,
        UncoveredPositionDto,
        VerdictDto,
    ))
)]
pub struct ApiDoc;
