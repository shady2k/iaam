//! Спека OpenAPI, порождённая из типов обработчиков (§17.1).
//!
//! Порождение устраняет расхождение **схемы данных**, но не поведения:
//! коды ответов в рантайме, требования аутентификации и фактическая
//! сериализация собственных типов остаются вне генерации. Поэтому
//! существуют чёрноящичные контрактные тесты (задача 15).

use utoipa::Modify;
use utoipa::OpenApi;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

use crate::dto::{
    AccountDto, AddBrokerAccessRequest, BrokerAccessDto, BrokerAccessUpdateRequest,
    BrokerEnvironmentDto, BrokerSyncRequest, ClaimOutcomeDto, ClaimRequest, ClassificationRuleDto,
    ClassificationRuleRequest, ComputedDto, ContourVersionDto, CreateAccountRequest,
    CreateContourVersionRequest, CreateInstrumentRequest, CreateTokenRequest, CurrencyDto,
    DataQualityDto, DimensionStatusDto, DocumentDto, EvaluatedPositionDto, EvidenceDto,
    ExecutabilitySharesDto, FeeOriginDto, FxRateDto, HealthDto, IncomeKindDto, IssuedTokenDto,
    LegacyDerivedPositionDto, LiquidationEstimateDto, MarketFxDto, MarketKeyRateDto,
    MarketPriceDto, MarketSourceDto, MarketSyncRequest, OperationDatesDto, OperationDto,
    OperationKindDto, OwnerBalanceRequest, PositionCoverageDto, PriceFreshnessDto, PriceOriginDto,
    PriceProvenanceDto, PriceQualityDto, PriceSelectionDto, RateDto, ReconciliationStatusDto,
    ReturnsReportDto, SelectedPriceDto, SubmitOperationsRequest, SyncOutcomeDto, TokenDto,
    UncoveredPositionDto, VerdictDto,
};
use crate::error::ApiError;
use crate::routes::MarketSyncOutcomeDto;

/// Схема аутентификации. Объявляется отдельно: `utoipa` порождает её
/// из типов, а требование `Bearer` типом не выражается.
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
                            "Агентский токен. Выдаётся владельцем, отзывается им же (§14).",
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
        description = "Учёт инвестиций. Этап 1: денежные потоки и XIRR до налога."
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
        DataQualityDto,
        DimensionStatusDto,
        DocumentDto,
        EvaluatedPositionDto,
        EvidenceDto,
        ExecutabilitySharesDto,
        FeeOriginDto,
        FxRateDto,
        HealthDto,
        IncomeKindDto,
        IssuedTokenDto,
        LegacyDerivedPositionDto,
        LiquidationEstimateDto,
        MarketFxDto,
        MarketKeyRateDto,
        MarketPriceDto,
        MarketSourceDto,
        MarketSyncOutcomeDto,
        MarketSyncRequest,
        OperationDatesDto,
        OperationDto,
        OperationKindDto,
        OwnerBalanceRequest,
        PriceFreshnessDto,
        PriceOriginDto,
        PriceProvenanceDto,
        PriceQualityDto,
        PriceSelectionDto,
        PositionCoverageDto,
        RateDto,
        ReconciliationStatusDto,
        ReturnsReportDto,
        SelectedPriceDto,
        SubmitOperationsRequest,
        SyncOutcomeDto,
        TokenDto,
        UncoveredPositionDto,
        VerdictDto,
    ))
)]
pub struct ApiDoc;
