//! Маршруты.
//!
//! Обработчик делает три вещи: разбирает DTO, зовёт сценарий, сериализует
//! результат. Ни одной арифметической операции над деньгами здесь нет —
//! это проверяется заслоном архитектуры (§3.1, §13).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use iaam_app::AppServices;
use iaam_app::ingest::csv_source::{Directory, ParsedRow, parse};
use iaam_app::ingest::{SubmittedOperation, Verdict};
use iaam_app::ports::{AccountView, Principal};
use iaam_app::scenarios::ingest::submit_operations;
use iaam_app::scenarios::reports::{ReturnsQuery, returns};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, SourceId};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::PROJECTION_VERSION;
use iaam_core::rules::LotRuleVersion;
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::ServerState;
use crate::dto::{
    AccountDto, ContourVersionDto, CreateAccountRequest, CreateContourVersionRequest, CurrencyDto,
    FxRateDto, HealthDto, ReturnsReportDto, SubmitOperationsRequest, VerdictDto,
};
use crate::error::{ApiError, ApiFailure};

/// Состояние сервиса.
#[utoipa::path(
    get,
    path = "/v1/health",
    responses((status = 200, description = "Сервис отвечает", body = HealthDto))
)]
pub async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok".into(),
        schema_version: iaam_core::event::SCHEMA_VERSION,
        projection_version: PROJECTION_VERSION,
    })
}

/// Список счетов.
#[utoipa::path(
    get,
    path = "/v1/accounts",
    responses((status = 200, description = "Счета владельца", body = Vec<AccountDto>)),
    security(("bearer" = []))
)]
pub async fn list_accounts(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<AccountDto>>, ApiFailure> {
    let accounts = state.services.store.list_accounts(principal.owner).await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|account| AccountDto {
                id: account.id.inner(),
                title: account.title,
                institution: account.institution,
            })
            .collect(),
    ))
}

/// Создание счёта.
#[utoipa::path(
    post,
    path = "/v1/accounts",
    request_body = CreateAccountRequest,
    responses(
        (status = 201, description = "Счёт создан", body = AccountDto),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_account(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<AccountDto>), ApiFailure> {
    require_admin(&principal)?;
    let account = AccountView {
        id: AccountId::new_random(),
        title: request.title,
        institution: request.institution,
    };
    state
        .services
        .store
        .upsert_account(principal.owner, account.clone())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(AccountDto {
            id: account.id.inner(),
            title: account.title,
            institution: account.institution,
        }),
    ))
}

/// Новая версия состава контура.
#[utoipa::path(
    post,
    path = "/v1/contours",
    request_body = CreateContourVersionRequest,
    responses(
        (status = 201, description = "Версия создана", body = ContourVersionDto),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_contour_version(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreateContourVersionRequest>,
) -> Result<(StatusCode, Json<ContourVersionDto>), ApiFailure> {
    require_admin(&principal)?;
    if request.accounts.is_empty() {
        return Err(ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError {
                code: "invalid_request".into(),
                message: "контур без счетов не имеет границы".into(),
                field: Some("accounts".into()),
                expected: Some("хотя бы один счёт".into()),
                actual: Some("пустой список".into()),
                correlation_id: None,
            },
        ));
    }
    let contour = ContourId(request.contour.unwrap_or_else(Uuid::new_v4));
    let previous = state
        .services
        .store
        .latest_contour_version(principal.owner, contour)
        .await?;
    let version = ContourVersion(previous.map_or(1, |value| value.0.saturating_add(1)));
    let accounts: Vec<AccountId> = request.accounts.iter().copied().map(AccountId).collect();
    let definition = ContourDefinition::new(contour, version, accounts.clone());

    state
        .services
        .store
        .insert_contour_version(principal.owner, definition, request.title, accounts.clone())
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ContourVersionDto {
            contour: contour.0,
            version: version.0,
            accounts: accounts.iter().map(|id| id.inner()).collect(),
        }),
    ))
}

/// Приёмка операций.
#[utoipa::path(
    post,
    path = "/v1/ingest/operations",
    request_body = SubmitOperationsRequest,
    responses(
        (status = 200, description = "Вердикт по каждой операции", body = Vec<VerdictDto>),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn ingest_operations(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<SubmitOperationsRequest>,
) -> Result<Json<Vec<VerdictDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let source = SourceId::new_random();

    // Разбор DTO даёт вердикт на строку: одна непонятая операция
    // не отменяет остальные (§10.1).
    let mut verdicts: Vec<VerdictDto> = Vec::with_capacity(request.operations.len());
    let mut accepted: Vec<(usize, SubmittedOperation)> = Vec::new();
    for (index, operation) in request.operations.iter().enumerate() {
        match operation.to_domain() {
            Ok(domain) => accepted.push((index + 1, domain)),
            Err(rejection) => verdicts.push(VerdictDto::from_domain(
                index + 1,
                &Verdict::Rejected { rejection },
            )),
        }
    }

    let domain: Vec<SubmittedOperation> = accepted
        .iter()
        .map(|(_, operation)| operation.clone())
        .collect();
    let outcomes = submit_operations(&state.services, &principal, source, &domain).await?;
    for ((row, _), verdict) in accepted.iter().zip(outcomes.iter()) {
        verdicts.push(VerdictDto::from_domain(*row, verdict));
    }
    verdicts.sort_by_key(|verdict| verdict.row);
    Ok(Json(verdicts))
}

/// Приёмка CSV.
#[utoipa::path(
    post,
    path = "/v1/ingest/csv",
    request_body(content = String, description = "Документ CSV", content_type = "text/csv"),
    responses(
        (status = 200, description = "Вердикт по каждой строке", body = Vec<VerdictDto>),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn ingest_csv(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    body: String,
) -> Result<Json<Vec<VerdictDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let directory = build_directory(&state.services, &principal).await?;
    let rows = parse(&body, &directory);

    let mut verdicts = Vec::with_capacity(rows.len());
    let mut accepted: Vec<(usize, SubmittedOperation)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match row {
            ParsedRow::Operation(operation) => {
                accepted.push((index + 1, (**operation).clone()));
            }
            ParsedRow::Rejected(rejection) => verdicts.push(VerdictDto::from_domain(
                index + 1,
                &Verdict::Rejected {
                    rejection: rejection.clone(),
                },
            )),
        }
    }

    let source = SourceId::new_random();
    let domain: Vec<SubmittedOperation> = accepted
        .iter()
        .map(|(_, operation)| operation.clone())
        .collect();
    let outcomes = submit_operations(&state.services, &principal, source, &domain).await?;
    for ((row, _), verdict) in accepted.iter().zip(outcomes.iter()) {
        verdicts.push(VerdictDto::from_domain(*row, verdict));
    }
    verdicts.sort_by_key(|verdict| verdict.row);
    Ok(Json(verdicts))
}

/// Параметры отчёта о доходности.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ReturnsParams {
    /// Идентификатор контура.
    pub contour: Uuid,
    /// Версия состава контура. По умолчанию — последняя.
    #[serde(default)]
    pub contour_version: Option<u32>,
    /// Дата отчёта в формате ГГГГ-ММ-ДД. По умолчанию — сегодня.
    #[serde(default)]
    #[param(value_type = Option<String>, format = Date, example = "2026-01-01")]
    pub as_of: Option<String>,
    /// Валюта отчёта.
    pub currency: CurrencyDto,
}

/// Отчёт о доходности **до налога**.
#[utoipa::path(
    get,
    path = "/v1/reports/returns",
    params(ReturnsParams),
    responses(
        (status = 200, description = "Отчёт", body = ReturnsReportDto),
        (status = 404, description = "Контур не найден", body = ApiError),
        (status = 500, description = "Нарушен инвариант", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn returns_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ReturnsParams>,
) -> Result<Json<ReturnsReportDto>, ApiFailure> {
    let query = ReturnsQuery {
        contour: ContourId(params.contour),
        contour_version: params.contour_version.map(ContourVersion),
        as_of: parse_as_of(params.as_of.as_deref())?,
        report_currency: params.currency.to_domain(),
        // Курсы на этапе 1 называет владелец: рыночные данные — E3.
        // Источник записывается в отчёт, поэтому подмены не происходит.
        fx: FxTable::new(FxSource::OwnerSupplied),
        lot_rule: LotRuleVersion(1),
    };
    let report = returns(&state.services, &principal, &query).await?;
    Ok(Json(ReturnsReportDto::from_domain(&report)))
}

/// Курсы, переданные вместе с запросом отчёта.
///
/// Отдельный обработчик, а не поле запроса `GET`: таблица курсов —
/// это тело, а тело у `GET` бывает, но им никто не пользуется.
#[utoipa::path(
    post,
    path = "/v1/reports/returns",
    params(ReturnsParams),
    request_body = Vec<FxRateDto>,
    responses(
        (status = 200, description = "Отчёт с указанными курсами", body = ReturnsReportDto),
        (status = 422, description = "Некорректный курс", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn returns_report_with_rates(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ReturnsParams>,
    Json(rates): Json<Vec<FxRateDto>>,
) -> Result<Json<ReturnsReportDto>, ApiFailure> {
    let mut fx = FxTable::new(FxSource::OwnerSupplied);
    for rate in &rates {
        let parsed = rate.rate.parse::<Decimal>().map_err(|_| {
            ApiFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: "invalid_request".into(),
                    message: "курс должен быть десятичным числом".into(),
                    field: Some("rate".into()),
                    expected: Some("десятичное число в виде строки".into()),
                    actual: Some(rate.rate.clone()),
                    correlation_id: None,
                },
            )
        })?;
        fx = fx.with_rate(
            rate.from.to_domain(),
            rate.to.to_domain(),
            rate.date,
            Dec::new(parsed),
        );
    }

    let query = ReturnsQuery {
        contour: ContourId(params.contour),
        contour_version: params.contour_version.map(ContourVersion),
        as_of: parse_as_of(params.as_of.as_deref())?,
        report_currency: params.currency.to_domain(),
        fx,
        lot_rule: LotRuleVersion(1),
    };
    let report = returns(&state.services, &principal, &query).await?;
    Ok(Json(ReturnsReportDto::from_domain(&report)))
}

/// Разбор даты отчёта.
///
/// Отдельная функция с явным отказом `422`: `serde` для `time::Date`
/// не принимает строку «ГГГГ-ММ-ДД» без указания формата, и молчаливое
/// умолчание «сегодня» вместо непонятой даты выдало бы отчёт не на ту дату.
fn parse_as_of(value: Option<&str>) -> Result<Option<Date>, ApiFailure> {
    let Some(raw) = value else {
        return Ok(None);
    };
    Date::parse(
        raw,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map(Some)
    .map_err(|_| {
        ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError {
                code: "invalid_request".into(),
                message: "дата отчёта должна быть в формате ГГГГ-ММ-ДД".into(),
                field: Some("as_of".into()),
                expected: Some("ГГГГ-ММ-ДД".into()),
                actual: Some(raw.to_owned()),
                correlation_id: None,
            },
        )
    })
}

fn require_admin(principal: &Principal) -> Result<(), ApiFailure> {
    if principal.scope.may_administer() {
        Ok(())
    } else {
        Err(ApiFailure::forbidden(principal.scope.code()))
    }
}

/// Справочник имён для разбора CSV.
///
/// Инструменты и места хранения на этапе 1 приходят из того же
/// справочника счетов: отдельная таблица инструментов заполняется
/// в E3 вместе с рыночными данными, а до тех пор CSV со сделками
/// требует явных идентификаторов через API операций.
async fn build_directory(
    services: &Arc<AppServices>,
    principal: &Principal,
) -> Result<Directory, ApiFailure> {
    let accounts = services.store.list_accounts(principal.owner).await?;
    let mut directory = Directory::default();
    for account in accounts {
        directory.accounts.insert(account.title, account.id);
    }
    Ok(directory)
}
