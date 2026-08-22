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
    AccountDto, ComputedDto, ContourVersionDto, CreateAccountRequest, CreateContourVersionRequest,
    CurrencyDto, DataQualityDto, FeeOriginDto, FxRateDto, HealthDto, OperationDatesDto,
    OperationDto, OperationKindDto, PriceQualityDto, RateDto, ReturnsReportDto,
    SubmitOperationsRequest, VerdictDto,
};
use crate::error::ApiError;

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
        ApiError,
        ComputedDto,
        ContourVersionDto,
        CreateAccountRequest,
        CreateContourVersionRequest,
        CurrencyDto,
        DataQualityDto,
        FeeOriginDto,
        FxRateDto,
        HealthDto,
        OperationDatesDto,
        OperationDto,
        OperationKindDto,
        PriceQualityDto,
        RateDto,
        ReturnsReportDto,
        SubmitOperationsRequest,
        VerdictDto,
    ))
)]
pub struct ApiDoc;
