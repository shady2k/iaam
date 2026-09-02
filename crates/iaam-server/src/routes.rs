//! Routes.
//!
//! The handler does three things: parses the DTO, calls the use case, and serialises
//! the result. There are no arithmetic operations on money here —
//! this is enforced by the architecture guard (§3.1, §13).

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::Response;
use axum::{Extension, Json};
use iaam_app::AppServices;
use iaam_app::actions::{Action, ActionCategory, ActionState, ActionTarget, ProvidedBy};
use iaam_app::ingest::csv_source::{Directory, ParsedRow, parse};
use iaam_app::ingest::{Rejection, SubmittedJournalEvent, SubmittedOperation, Verdict};
use iaam_app::ports::{AccountView, Principal, Scope};
use iaam_app::scenarios::categories::{
    CategoryRuleInput, create_category, create_category_rule, create_group, list_categories,
    list_category_rules, list_groups, preview_category_rule, retire_category,
};
use iaam_app::scenarios::classification::{create_rule, list_rules, retire_rule};
use iaam_app::scenarios::documents::{reparse_report, upload_report};
use iaam_app::scenarios::ingest::{submit_journal_events, submit_operations};
use iaam_app::scenarios::journal::{DeclaredSource, JournalReadQuery, read_journal};
use iaam_app::scenarios::market_reference::{
    MarketFxQuery, MarketKeyRateQuery, MarketPricesQuery, list_market_fx as read_market_fx,
    list_market_key_rate as read_market_key_rate, list_market_prices as read_market_prices,
};
use iaam_app::scenarios::reconciliation::{OwnerBalance, record_owner_balance, report, statuses};
use iaam_app::scenarios::reports::{
    MoneyFlowQuery, ReturnsQuery, account_balances, money_flow, returns,
};
use iaam_app::sync::{
    MarketSource, MarketSyncRequest as AppMarketSyncRequest, sync_broker as run_sync_broker,
    sync_market_with_services as run_market_sync,
};
use iaam_core::category::{CategoryInterval, CategoryMatcher, CategoryRuleProposal};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, CategoryId, CategoryRuleId, CustodyId, InstrumentId, SourceId};
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::PROJECTION_VERSION;
use iaam_core::reconciliation::ReconciliationStatus;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_core::rules::LotRuleVersion;
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use utoipa::IntoParams;
use uuid::Uuid;

use crate::ServerState;
use crate::action_catalog::ActionCatalog;
use crate::dto::{
    AccountBalanceDto, AccountCandidateDto, AccountDto, ActionDto, ActionTargetDto,
    ActionsResponseDto, BrokerAccessDto, BrokerSyncRequest, CategoryDto, CategoryGroupDto,
    CategoryGroupRequest, CategoryRequest, CategoryRuleDto, CategoryRuleImpactDto,
    CategoryRuleRequest, ClassificationRuleDto, ClassificationRuleRequest, ContourVersionDto,
    CreateAccountRequest, CreateContourVersionRequest, CreateInstrumentRequest, CreateTokenRequest,
    CurrencyDto, CustodyRepairOutcomeDto, CustodyRepairRequest, DocumentDto, DocumentParams,
    FxRateDto, HealthDto, InstrumentDto, IssuedTokenDto, JournalEventReadDto, JournalPageDto,
    MarketFxDto, MarketFxSeriesDto, MarketKeyRateDto, MarketKeyRateSeriesDto, MarketPriceDto,
    MarketPriceSeriesDto, MarketSourceDto, MarketSyncRequest, MissingInputDto, MoneyFlowReportDto,
    OwnerBalanceRequest, QuotationBasisDto, QuotationBasisStatusDto, ReconciliationParams,
    ReconciliationResponseDto, ReconciliationStatusDto, RequestPlanDto, ResolveInstrumentRequest,
    ResolvedInstrumentDto, ReturnsReportDto, SubmitJournalEventsRequest, SubmitOperationsRequest,
    SyncOutcomeDto, TokenDto, TokenScopeDto, VerdictDto,
};
use crate::error::{ApiError, ApiFailure};
use crate::extract::{ApiBytes, ApiJson, ApiPath, ApiQuery};
use iaam_app::scenarios::documents::UploadedDocument;

pub const CREATE_ACCOUNT_OPERATION_ID: &str = "create_account";
pub const CREATE_CONTOUR_VERSION_OPERATION_ID: &str = "create_contour_version";
pub const RECORD_OWNER_BALANCE_OPERATION_ID: &str = "record_owner_balance";

/// The computed actions currently blocking or advancing owner setup.
#[utoipa::path(
    get,
    path = "/v1/actions",
    responses((status = 200, description = "Computed owner actions", body = ActionsResponseDto)),
    security(("bearer" = []))
)]
pub async fn list_actions(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
) -> Result<Json<ActionsResponseDto>, ApiFailure> {
    let actions =
        iaam_app::actions::frontier(principal.owner, state.services.store.as_ref()).await?;
    Ok(Json(ActionsResponseDto {
        policy_version: 1,
        items: actions
            .iter()
            .map(|action| action_dto(action, &catalog))
            .collect(),
    }))
}

fn action_dto(action: &Action, catalog: &ActionCatalog) -> ActionDto {
    let target = match action.target() {
        ActionTarget::Operation { operation, request } => {
            let resolved = catalog.operation(*operation);
            ActionTargetDto::Operation {
                operation_id: resolved.operation_id.clone(),
                method: resolved.method.clone(),
                path: resolved.path.clone(),
                request_schema: resolved.request_schema.clone(),
                request: RequestPlanDto {
                    preset: request.preset.clone(),
                    missing: request
                        .missing
                        .iter()
                        .map(|missing| MissingInputDto {
                            pointer: missing.pointer.clone(),
                            provided_by: provided_by_code(missing.provided_by),
                            candidates: missing.candidates.as_ref().map(|candidates| {
                                candidates
                                    .iter()
                                    .map(|candidate| AccountCandidateDto {
                                        id: candidate.id.inner(),
                                        title: candidate.title.clone(),
                                        institution: candidate.institution.clone(),
                                    })
                                    .collect()
                            }),
                        })
                        .collect(),
                },
            }
        }
        ActionTarget::None => ActionTargetDto::None,
    };
    ActionDto {
        id: action.id().to_owned(),
        kind: action.kind().id().to_owned(),
        category: match action.category() {
            ActionCategory::Blocking => "blocking",
            ActionCategory::RequiredForGoal => "required_for_goal",
            ActionCategory::Recommended => "recommended",
            ActionCategory::Informational => "informational",
        }
        .to_owned(),
        state: match action.state() {
            ActionState::Ready => "ready",
            ActionState::NeedsOwnerInput => "needs_owner_input",
            ActionState::Blocked => "blocked",
        }
        .to_owned(),
        reason: action.reason().to_owned(),
        required_scope: action.required_scope().map(|scope| scope.code().to_owned()),
        target,
    }
}

fn provided_by_code(source: ProvidedBy) -> String {
    match source {
        ProvidedBy::Owner => "owner",
        ProvidedBy::ExternalDocument => "external_document",
        ProvidedBy::Caller => "caller",
    }
    .to_owned()
}

/// List of instruments in the global reference catalogue.
#[utoipa::path(
    get,
    path = "/v1/instruments",
    responses((status = 200, description = "Reference catalogue instruments", body = [InstrumentDto])),
    security(("bearer" = []))
)]
pub async fn list_instruments(
    State(state): State<ServerState>,
    Extension(_principal): Extension<Principal>,
) -> Result<Json<Vec<InstrumentDto>>, ApiFailure> {
    let instruments = state.services.directory.list_instruments().await?;
    Ok(Json(instruments.into_iter().map(instrument_dto).collect()))
}

/// One instrument from the global reference catalogue.
#[utoipa::path(
    get,
    path = "/v1/instruments/{id}",
    params(("id" = Uuid, Path, description = "Instrument identifier")),
    responses(
        (status = 200, description = "Reference catalogue instrument", body = InstrumentDto),
        (status = 404, description = "Instrument unknown", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn get_instrument(
    State(state): State<ServerState>,
    Extension(_principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<InstrumentDto>, ApiFailure> {
    let instrument = state
        .services
        .directory
        .instrument(InstrumentId(id))
        .await?
        .ok_or_else(|| iaam_app::error::AppError::NotFound {
            what: "instrument",
            id: id.to_string(),
        })?;
    Ok(Json(instrument_dto(instrument)))
}

/// Resolve an external instrument code as at the document date.
#[utoipa::path(
    post,
    path = "/v1/instruments/resolve",
    request_body = ResolveInstrumentRequest,
    responses(
        (status = 200, description = "Instrument by code as at date", body = ResolvedInstrumentDto),
        (status = 404, description = "Code unknown", body = ApiError),
        (status = 422, description = "Code known, but not on this date, or namespace invalid", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn resolve_instrument(
    State(state): State<ServerState>,
    Extension(_principal): Extension<Principal>,
    ApiJson(request): ApiJson<ResolveInstrumentRequest>,
) -> Result<Json<ResolvedInstrumentDto>, ApiFailure> {
    let instrument = state
        .services
        .directory
        .resolve(
            request.namespace.to_domain().code(),
            &request.value,
            request.on,
        )
        .await?;
    Ok(Json(ResolvedInstrumentDto {
        instrument: instrument.inner().to_string(),
    }))
}

/// Add an instrument to the global reference catalogue.
///
/// Permission is checked before parsing the body: an agent token must receive 403
/// even for a body that is itself invalid (§7, §14).
#[utoipa::path(
    post,
    path = "/v1/instruments",
    request_body = CreateInstrumentRequest,
    responses(
        (status = 201, description = "Instrument recorded", body = InstrumentDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 422, description = "Invalid instrument data", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_instrument(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiBytes(body): ApiBytes,
) -> Result<(StatusCode, Json<InstrumentDto>), ApiFailure> {
    require_admin(&principal)?;
    let request: CreateInstrumentRequest = serde_json::from_slice(&body)
        .map_err(|error| invalid_field("body", "instrument JSON object", error.to_string()))?;
    let id = InstrumentId(request.id.unwrap_or_else(Uuid::new_v4));
    let kind = request
        .kind
        .as_deref()
        .map(|kind| {
            InstrumentKind::from_code(kind).ok_or_else(|| {
                invalid_field(
                    "kind",
                    "share, depositary_receipt, bond, etf, mutual_fund, currency, crypto, \
                     real_estate, private_share or loan",
                    kind.to_owned(),
                )
            })
        })
        .transpose()?;
    let currencies = CurrencyRoles {
        denomination: parse_currency("denomination_currency", &request.denomination_currency)?,
        settlement: parse_currency("settlement_currency", &request.settlement_currency)?,
        quote: parse_currency("quote_currency", &request.quote_currency)?,
    };
    let id = state
        .services
        .directory
        .record_instrument(iaam_app::ports::InstrumentUpsert {
            id,
            kind,
            symbol: request.symbol,
            title: request.title,
            currencies,
            lineage: None,
        })
        .await?;
    let instrument = state
        .services
        .directory
        .instrument(id)
        .await?
        .ok_or_else(|| iaam_app::error::AppError::NotFound {
            what: "recorded instrument",
            id: id.inner().to_string(),
        })?;
    Ok((StatusCode::CREATED, Json(instrument_dto(instrument))))
}

/// Upload a report with per-row outcomes.
#[utoipa::path(
    post,
    path = "/v1/documents",
    params(DocumentParams),
    request_body(content = String, description = "Binary XLSX/XLS workbook"),
    responses(
        (status = 200, description = "Outcome for each row", body = DocumentDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 422, description = "Document unrecognised or invalid", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn upload_document(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<DocumentParams>,
    ApiBytes(body): ApiBytes,
) -> Result<Json<DocumentDto>, ApiFailure> {
    let directory = build_directory(&state.services, &principal).await?;
    let result = upload_report(
        &state.services,
        &principal,
        &body,
        &directory,
        params.account.map(AccountId),
    )
    .await?;
    Ok(Json(document_dto(result)))
}

/// Re-parse the source while verifying that it is identical.
#[utoipa::path(
    post,
    path = "/v1/documents/{id}/reparse",
    params(
        ("id" = String, Path, description = "SHA-256 of the source document"),
        DocumentParams
    ),
    request_body(content = String, description = "Binary XLSX/XLS workbook"),
    responses(
        (status = 200, description = "Outcome for each row", body = DocumentDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 422, description = "Hash or document invalid", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn reparse_document(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(document_hash): ApiPath<String>,
    ApiQuery(params): ApiQuery<DocumentParams>,
    ApiBytes(body): ApiBytes,
) -> Result<Json<DocumentDto>, ApiFailure> {
    let directory = build_directory(&state.services, &principal).await?;
    let result = reparse_report(
        &state.services,
        &principal,
        &document_hash,
        &body,
        &directory,
        params.account.map(AccountId),
    )
    .await?;
    Ok(Json(document_dto(result)))
}

/// Retract trades whose custody was fabricated from the account identifier.
#[utoipa::path(
    post,
    path = "/v1/accounts/{account}/repairs/custody",
    params(("account" = Uuid, Path, description = "Account identifier")),
    request_body = CustodyRepairRequest,
    responses(
        (status = 200, description = "Custody repair outcome", body = CustodyRepairOutcomeDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 422, description = "Invalid repair request", body = ApiError),
        (status = 409, description = "Repair idempotency conflict", body = ApiError),
        (status = 503, description = "Broker access is not configured", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn repair_custody(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(account): ApiPath<Uuid>,
    ApiJson(request): ApiJson<CustodyRepairRequest>,
) -> Result<Json<CustodyRepairOutcomeDto>, ApiFailure> {
    let outcome = iaam_app::scenarios::custody_repair::repair_custody(
        &state.services,
        &principal,
        AccountId(account),
        request.acknowledge_without_live_access,
    )
    .await?;
    Ok(Json(CustodyRepairOutcomeDto::from_domain(outcome)))
}

/// Reconciliation statuses with grounds and assertion outcomes.
#[utoipa::path(
    get,
    path = "/v1/reconciliation",
    params(ReconciliationParams),
    responses(
        (status = 200, description = "Reconciliation statuses and coverage gaps", body = ReconciliationResponseDto),
        (status = 422, description = "Invalid date range", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn reconciliation(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiQuery(params): ApiQuery<ReconciliationParams>,
) -> Result<Json<ReconciliationResponseDto>, ApiFailure> {
    let from = parse_query_date("from", &params.from)?;
    let to = parse_query_date("to", &params.to)?;
    let reconciliation = report(
        &state.services,
        &principal,
        AccountId(params.account),
        from,
        to,
    )
    .await?;
    Ok(Json(ReconciliationResponseDto {
        statuses: reconciliation
            .statuses
            .iter()
            .map(reconciliation_status_dto)
            .collect(),
        gaps: reconciliation
            .gaps
            .iter()
            .map(crate::dto::TaintDto::from_domain)
            .collect(),
        // The scenario computed these while the ledger was in hand; this handler
        // renders them through the one conversion `/v1/actions` also uses.
        actions: reconciliation
            .actions
            .iter()
            .map(|action| action_dto(action, &catalog))
            .collect(),
    }))
}

/// Record a balance declared by the owner.
#[utoipa::path(
    post,
    path = "/v1/reconciliation/balance",
    operation_id = RECORD_OWNER_BALANCE_OPERATION_ID,
    request_body = OwnerBalanceRequest,
    responses(
        (status = 200, description = "Updated statuses", body = Vec<ReconciliationStatusDto>),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 422, description = "Invalid balance", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn reconciliation_balance(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<OwnerBalanceRequest>,
) -> Result<Json<Vec<ReconciliationStatusDto>>, ApiFailure> {
    require_admin(&principal)?;
    let period = AssertionPeriod::between(request.from, request.to).ok_or_else(|| {
        invalid_field(
            "period",
            "from no later than to",
            format!("{}..{}", request.from, request.to),
        )
    })?;
    let at = match request.at.as_str() {
        "opening" => BalancePoint::Opening,
        "closing" => BalancePoint::Closing,
        actual => {
            return Err(invalid_field("at", "opening or closing", actual.to_owned()));
        }
    };
    let cash = request
        .cash
        .map(|cash| {
            let amount = cash
                .amount
                .parse::<Decimal>()
                .map_err(|_| invalid_field("cash.amount", "decimal string", cash.amount.clone()))?;
            let amount = iaam_app::ingest::operation::to_minor_units(
                amount,
                cash.currency.to_domain(),
                "cash.amount",
            )
            .map_err(invalid_rejection)?;
            Ok::<_, ApiFailure>((cash.currency.to_domain(), PostedMinor::new(amount)))
        })
        .transpose()?;
    let mut positions = Vec::with_capacity(request.positions.len());
    for position in request.positions {
        let quantity = position.quantity.parse::<Decimal>().map_err(|_| {
            invalid_field(
                "positions.quantity",
                "decimal string",
                position.quantity.clone(),
            )
        })?;
        positions.push((
            iaam_core::ids::InstrumentId(position.instrument),
            CustodyId(position.custody),
            Quantity(Dec::new(quantity)),
        ));
    }
    let raw_hash = request.source_hash.unwrap_or_else(|| "0".repeat(64));
    let raw_hash = iaam_core::event::provenance::RawHash::parse(&raw_hash)
        .ok_or_else(|| invalid_field("source_hash", "64 hex-characters", raw_hash))?;
    let _ = record_owner_balance(
        &state.services,
        &principal,
        OwnerBalance {
            account: AccountId(request.account),
            period,
            at,
            cash,
            positions,
            raw_hash,
        },
    )
    .await?;
    let statuses = statuses(
        &state.services,
        &principal,
        AccountId(request.account),
        period.from,
        period.to,
    )
    .await?;
    Ok(Json(
        statuses.iter().map(reconciliation_status_dto).collect(),
    ))
}

/// Active and retired classification rules.
#[utoipa::path(
    get,
    path = "/v1/classification-rules",
    responses(
        (status = 200, description = "Classification rule history", body = Vec<ClassificationRuleDto>),
        (status = 403, description = "Owner only", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_classification_rules(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<ClassificationRuleDto>>, ApiFailure> {
    require_admin(&principal)?;
    let rules = list_rules(&state.services, &principal).await?;
    Ok(Json(
        rules
            .into_iter()
            .map(ClassificationRuleDto::from_port)
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/classification-rules",
    request_body = ClassificationRuleRequest,
    responses(
        (status = 201, description = "Rule added", body = ClassificationRuleDto),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 422, description = "Invalid rule", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_classification_rule(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<ClassificationRuleRequest>,
) -> Result<(StatusCode, Json<ClassificationRuleDto>), ApiFailure> {
    require_admin(&principal)?;
    let rule = create_rule(
        &state.services,
        &principal,
        request.matcher,
        request.outcome,
        request.replaces,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(ClassificationRuleDto::from_port(rule)),
    ))
}

#[utoipa::path(
    delete,
    path = "/v1/classification-rules/{id}",
    params(("id" = Uuid, Path, description = "Rule identifier")),
    responses(
        (status = 204, description = "Rule retired"),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 404, description = "Rule not found", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn delete_classification_rule(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiFailure> {
    require_admin(&principal)?;
    retire_rule(&state.services, &principal, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
/// Active and retired owner category groups.
#[utoipa::path(
    get,
    path = "/v1/category-groups",
    responses(
        (status = 200, description = "Owner category groups", body = Vec<CategoryGroupDto>),
        (status = 403, description = "Owner only", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_category_groups(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<CategoryGroupDto>>, ApiFailure> {
    require_admin(&principal)?;
    let groups = list_groups(&state.services, &principal).await?;
    Ok(Json(
        groups
            .into_iter()
            .map(|group| CategoryGroupDto {
                id: group.id.inner(),
                title: group.title,
                retired_at: group.retired_at,
                is_income: group.is_income,
            })
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/category-groups",
    request_body = CategoryGroupRequest,
    responses(
        (status = 201, description = "Category group added", body = CategoryGroupDto),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 422, description = "Invalid category group", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_category_group_route(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CategoryGroupRequest>,
) -> Result<(StatusCode, Json<CategoryGroupDto>), ApiFailure> {
    require_admin(&principal)?;
    let title = request.title.trim();
    if title.is_empty() {
        return Err(invalid_field("title", "a non-empty title", request.title));
    }
    let id = create_group(&state.services, &principal, title, request.is_income).await?;
    Ok((
        StatusCode::CREATED,
        Json(CategoryGroupDto {
            id: id.inner(),
            title: title.to_owned(),
            retired_at: None,
            is_income: request.is_income,
        }),
    ))
}

/// Active and retired owner categories.
#[utoipa::path(
    get,
    path = "/v1/categories",
    responses(
        (status = 200, description = "Owner category history", body = Vec<CategoryDto>),
        (status = 403, description = "Owner only", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_category_reference(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<CategoryDto>>, ApiFailure> {
    require_admin(&principal)?;
    let categories = list_categories(&state.services, &principal).await?;
    Ok(Json(
        categories.into_iter().map(CategoryDto::from_port).collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/categories",
    request_body = CategoryRequest,
    responses(
        (status = 201, description = "Category added", body = CategoryDto),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 404, description = "Category group not found", body = ApiError),
        (status = 422, description = "Invalid category", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_category_route(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CategoryRequest>,
) -> Result<(StatusCode, Json<CategoryDto>), ApiFailure> {
    require_admin(&principal)?;
    let category = create_category(
        &state.services,
        &principal,
        iaam_core::ids::CategoryGroupId(request.group),
        &request.title,
    )
    .await?;
    let categories = list_categories(&state.services, &principal).await?;
    let category = categories
        .into_iter()
        .find(|item| item.id == category)
        .ok_or_else(|| {
            ApiFailure::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::simple("category_missing", "created category could not be loaded"),
            )
        })?;
    Ok((StatusCode::CREATED, Json(CategoryDto::from_port(category))))
}

#[utoipa::path(
    delete,
    path = "/v1/categories/{id}",
    params(("id" = Uuid, Path, description = "Category identifier")),
    responses(
        (status = 204, description = "Category retired"),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 404, description = "Category not found", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn delete_category(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiFailure> {
    require_admin(&principal)?;
    retire_category(&state.services, &principal, CategoryId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Active and retired owner category rules.
#[utoipa::path(
    get,
    path = "/v1/category-rules",
    responses(
        (status = 200, description = "Category rule history", body = Vec<CategoryRuleDto>),
        (status = 403, description = "Owner only", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_category_rules_route(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<CategoryRuleDto>>, ApiFailure> {
    require_admin(&principal)?;
    let rules = list_category_rules(&state.services, &principal).await?;
    Ok(Json(
        rules.into_iter().map(CategoryRuleDto::from_port).collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/category-rules",
    request_body = CategoryRuleRequest,
    responses(
        (status = 201, description = "Category rule added", body = CategoryRuleDto),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 422, description = "Invalid category rule", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_category_rule_route(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CategoryRuleRequest>,
) -> Result<(StatusCode, Json<CategoryRuleDto>), ApiFailure> {
    require_admin(&principal)?;
    let matcher = parse_category_matcher(request.matcher)?;
    let rule = create_category_rule(
        &state.services,
        &principal,
        CategoryRuleInput {
            matcher,
            category: CategoryId(request.category),
            interval: CategoryInterval {
                from: request.valid_from,
                to: request.valid_to,
            },
            replaces: request.replaces.map(CategoryRuleId),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(CategoryRuleDto::from_port(rule))))
}

#[utoipa::path(
    post,
    path = "/v1/category-rules/preview",
    request_body = CategoryRuleRequest,
    responses(
        (status = 200, description = "Category rule impact", body = CategoryRuleImpactDto),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 422, description = "Invalid category rule", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn preview_category_rule_route(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CategoryRuleRequest>,
) -> Result<Json<CategoryRuleImpactDto>, ApiFailure> {
    require_admin(&principal)?;
    let matcher = parse_category_matcher(request.matcher)?;
    let impact = preview_category_rule(
        &state.services,
        &principal,
        &CategoryRuleProposal {
            id: CategoryRuleId::new_random(),
            interval: CategoryInterval {
                from: request.valid_from,
                to: request.valid_to,
            },
            matcher,
            category: CategoryId(request.category),
        },
    )
    .await?;
    Ok(Json(CategoryRuleImpactDto::from_domain(impact)))
}

/// Synchronise one broker channel over an interval.
#[utoipa::path(
    post,
    path = "/v1/brokers/{broker}/sync",
    params(("broker" = String, Path, description = "Broker code")),
    request_body = BrokerSyncRequest,
    responses(
        (status = 200, description = "Synchronisation result", body = SyncOutcomeDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 503, description = "Broker channel or access is not configured", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn sync_broker(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiPath(broker): ApiPath<String>,
    ApiJson(request): ApiJson<BrokerSyncRequest>,
) -> Result<Json<SyncOutcomeDto>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let channel = state
        .services
        .channels
        .open(principal.owner, &broker)
        .await?;
    let outcome = run_sync_broker(
        &state.services,
        &principal,
        channel.as_ref(),
        AccountId(request.account),
        request.from,
        request.to,
    )
    .await?;
    let actions = iaam_app::actions::verdicts_diagnostics(&outcome.recorded)
        .iter()
        .map(|action| action_dto(action, &catalog))
        .collect();
    Ok(Json(SyncOutcomeDto::from_domain(outcome, actions)))
}

/// Manually synchronise one market series.
#[utoipa::path(
    post,
    path = "/v1/market/sync",
    request_body = MarketSyncRequest,
    responses(
        (status = 200, description = "Market synchronisation result", body = MarketSyncOutcomeDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 422, description = "Invalid range", body = ApiError),
        (status = 503, description = "Market transport is not configured", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn sync_market(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<MarketSyncRequest>,
) -> Result<Json<MarketSyncOutcomeDto>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let from = parse_query_date("from", &request.from)?;
    let to = parse_query_date("to", &request.to)?;
    let source = market_source(request.source);
    let request = AppMarketSyncRequest { source, from, to };
    let outcome = run_market_sync(state.services.as_ref(), request).await?;
    Ok(Json(MarketSyncOutcomeDto::from_domain(outcome)))
}

/// Price series parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct MarketPricesParams {
    /// Instrument identifier.
    pub instrument: Uuid,
    /// Trading board — the MOEX `BOARDID` of the observation, stored verbatim
    /// from the source. The source owns the set of values; `TQBR` (shares) and
    /// `TQOB` (bonds) are the ones the parser's fixtures carry.
    pub board: String,
    /// Trading session — the MOEX ISS `TRADINGSESSION` of the observation.
    /// Together with the board it fixes the trading mode a price belongs to,
    /// which is what separates regular trading from the evening session. The
    /// source owns the set of values and this code does not enumerate them:
    /// `3` is the only one the parser's fixtures carry, and a response without
    /// the column is stored as `0`.
    pub session: i64,
    /// Inclusive start of the interval, YYYY-MM-DD.
    #[param(value_type = String, format = Date)]
    pub from: String,
    /// Inclusive end of the interval, YYYY-MM-DD.
    #[param(value_type = String, format = Date)]
    pub to: String,
    /// Knowledge cut-off, RFC 3339: rows recorded after this moment are
    /// invisible to the answer. By default — now.
    #[serde(default)]
    pub knowledge_as_of: Option<String>,
}

/// Exchange-rate series parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct MarketFxParams {
    /// Base currency of the pair: the currency being priced.
    pub base: CurrencyDto,
    /// Quote currency of the pair: the currency the rate is expressed in.
    pub quote: CurrencyDto,
    /// Inclusive start of the interval, YYYY-MM-DD.
    #[param(value_type = String, format = Date)]
    pub from: String,
    /// Inclusive end of the interval, YYYY-MM-DD.
    #[param(value_type = String, format = Date)]
    pub to: String,
    /// Knowledge cut-off, RFC 3339: rows recorded after this moment are
    /// invisible to the answer. By default — now.
    #[serde(default)]
    pub knowledge_as_of: Option<String>,
}

/// Key-rate series parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct MarketKeyRateParams {
    /// Inclusive start of the interval, YYYY-MM-DD.
    #[param(value_type = String, format = Date)]
    pub from: String,
    /// Inclusive end of the interval, YYYY-MM-DD.
    #[param(value_type = String, format = Date)]
    pub to: String,
    /// Knowledge cut-off, RFC 3339: rows recorded after this moment are
    /// invisible to the answer. By default — now.
    #[serde(default)]
    pub knowledge_as_of: Option<String>,
}

/// Price series with provenance for each row.
#[utoipa::path(
    get,
    path = "/v1/market/prices",
    params(MarketPricesParams),
    responses(
        (status = 200, description = "Prices with provenance and the completeness boundary", body = MarketPriceSeriesDto),
        (status = 422, description = "Invalid range", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_market_prices(
    State(state): State<ServerState>,
    Extension(_principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<MarketPricesParams>,
) -> Result<Json<MarketPriceSeriesDto>, ApiFailure> {
    let from = parse_query_date("from", &params.from)?;
    let to = parse_query_date("to", &params.to)?;
    let knowledge_as_of = parse_knowledge_as_of(params.knowledge_as_of.as_deref())?;
    let series = read_market_prices(
        state.services.as_ref(),
        MarketPricesQuery {
            instrument: InstrumentId(params.instrument),
            board: params.board,
            session: params.session,
            from,
            to,
            knowledge_as_of,
        },
    )
    .await?;
    Ok(Json(MarketPriceSeriesDto {
        rows: series.rows.into_iter().map(market_price_dto).collect(),
        complete_through: series.complete_through,
    }))
}

/// Official exchange-rate series with provenance for each row.
#[utoipa::path(
    get,
    path = "/v1/market/fx",
    params(MarketFxParams),
    responses(
        (status = 200, description = "Exchange rates with provenance and the completeness boundary", body = MarketFxSeriesDto),
        (status = 422, description = "Invalid range", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_market_fx(
    State(state): State<ServerState>,
    Extension(_principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<MarketFxParams>,
) -> Result<Json<MarketFxSeriesDto>, ApiFailure> {
    let from = parse_query_date("from", &params.from)?;
    let to = parse_query_date("to", &params.to)?;
    let knowledge_as_of = parse_knowledge_as_of(params.knowledge_as_of.as_deref())?;
    let series = read_market_fx(
        state.services.as_ref(),
        MarketFxQuery {
            base: params.base.to_domain(),
            quote: params.quote.to_domain(),
            from,
            to,
            knowledge_as_of,
        },
    )
    .await?;
    Ok(Json(MarketFxSeriesDto {
        rows: series.rows.into_iter().map(market_fx_dto).collect(),
        complete_through: series.complete_through,
    }))
}

/// Official key-rate intervals with boundary provenance.
#[utoipa::path(
    get,
    path = "/v1/market/key-rate",
    params(MarketKeyRateParams),
    responses(
        (status = 200, description = "Key-rate intervals with provenance and the completeness boundary", body = MarketKeyRateSeriesDto),
        (status = 422, description = "Invalid range", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_market_key_rate(
    State(state): State<ServerState>,
    Extension(_principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<MarketKeyRateParams>,
) -> Result<Json<MarketKeyRateSeriesDto>, ApiFailure> {
    let from = parse_query_date("from", &params.from)?;
    let to = parse_query_date("to", &params.to)?;
    let knowledge_as_of = parse_knowledge_as_of(params.knowledge_as_of.as_deref())?;
    let series = read_market_key_rate(
        state.services.as_ref(),
        MarketKeyRateQuery {
            from,
            to,
            knowledge_as_of,
        },
    )
    .await?;
    Ok(Json(MarketKeyRateSeriesDto {
        rows: series.rows.into_iter().map(market_key_rate_dto).collect(),
        complete_through: series.complete_through,
    }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MarketSyncOutcomeDto {
    pub status: String,
    pub rows: usize,
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
}

impl MarketSyncOutcomeDto {
    fn from_domain(outcome: iaam_app::sync::MarketSyncResult) -> Self {
        Self {
            status: outcome.status().to_owned(),
            rows: outcome.rows,
            covered_from: outcome.covered.map(|coverage| coverage.from.to_string()),
            covered_to: outcome.covered.map(|coverage| coverage.to.to_string()),
        }
    }
}

fn market_source(source: MarketSourceDto) -> MarketSource {
    match source {
        MarketSourceDto::Moex {
            engine,
            market,
            board,
            secid,
            instrument,
        } => MarketSource::Moex {
            engine,
            market,
            board,
            secid,
            instrument: InstrumentId(instrument),
        },
        MarketSourceDto::CbrDaily => MarketSource::CbrDaily,
        MarketSourceDto::CbrDynamic {
            cbr_currency_id,
            to,
        } => MarketSource::CbrDynamic {
            cbr_currency_id,
            to: to.to_domain(),
        },
        MarketSourceDto::CbrKeyRate => MarketSource::CbrKeyRate,
    }
}
/// The standards discovery document for this API.
pub const API_CATALOG_BODY: &[u8] = br#"{"linkset":[{"anchor":"/v1","service-desc":[{"href":"/v1/openapi.json","type":"application/json"}],"status":[{"href":"/v1/health","type":"application/json"}]}]}"#;

#[utoipa::path(
    get,
    path = "/.well-known/api-catalog",
    responses((
        status = 200,
        description = "API discovery links",
        content_type = "application/linkset+json"
    ))
)]
pub async fn api_catalog() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/linkset+json")
        .body(Body::from(Bytes::from_static(API_CATALOG_BODY)))
        .expect("static catalog response is valid")
}

/// Service status.
#[utoipa::path(
    get,
    path = "/v1/health",
    responses((status = 200, description = "Service is responding", body = HealthDto))
)]
pub async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok".into(),
        schema_version: iaam_core::event::SCHEMA_VERSION,
        projection_version: PROJECTION_VERSION,
    })
}

/// Account list.
#[utoipa::path(
    get,
    path = "/v1/accounts",
    responses((status = 200, description = "Owner's accounts", body = Vec<AccountDto>)),
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

/// Create an account.
#[utoipa::path(
    post,
    path = "/v1/accounts",
    operation_id = CREATE_ACCOUNT_OPERATION_ID,
    request_body = CreateAccountRequest,
    responses(
        (status = 201, description = "Account created", body = AccountDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_account(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CreateAccountRequest>,
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

/// List of broker access entries.
///
/// Revoked entries are also shown: «when the system stopped contacting
/// the broker» is a question that needs answering. An active entry
/// differs from a revoked one by the `revoked_at` field.
#[utoipa::path(
    get,
    path = "/v1/broker-access",
    responses(
        (status = 200, description = "Owner's broker access entries", body = Vec<BrokerAccessDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 503, description = "Broker access encryption is not configured", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_broker_access(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<BrokerAccessDto>>, ApiFailure> {
    require_admin(&principal)?;
    let access = state.services.broker.list_access(principal.owner).await?;
    Ok(Json(
        access
            .into_iter()
            .map(BrokerAccessDto::from_domain)
            .collect(),
    ))
}

/// Revoking broker access.
#[utoipa::path(
    delete,
    path = "/v1/broker-access/{id}",
    params(("id" = Uuid, Path, description = "Identifier of the created access entry")),
    responses(
        (status = 204, description = "Access revoked"),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 503, description = "Broker access encryption is not configured", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn revoke_broker_access(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiFailure> {
    require_admin(&principal)?;
    state
        .services
        .broker
        .revoke_access(principal.owner, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Token issuance.
///
/// The token is shown **once only**: only its hash remains in the database,
/// so it cannot be shown again (§14).
#[utoipa::path(
    post,
    path = "/v1/tokens",
    request_body = CreateTokenRequest,
    responses(
        (status = 201, description = "Token issued and shown once only", body = IssuedTokenDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 422, description = "The owner scope cannot be issued via the API", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_token(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CreateTokenRequest>,
) -> Result<(StatusCode, Json<IssuedTokenDto>), ApiFailure> {
    require_admin(&principal)?;
    let scope = match request.scope {
        // Full-access tokens cannot be issued via the API: the owner is created
        // with `iaam claim --label <label>`. Otherwise, a stolen owner token
        // could immediately be replicated into indistinguishable copies,
        // and revoking the original would change nothing.
        TokenScopeDto::Owner => {
            return Err(ApiFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: "invalid_request".into(),
                    message: "an owner token cannot be issued via the API: the owner is created \
                              with `iaam claim --label <label>`"
                        .into(),
                    field: Some("scope".into()),
                    expected: Some("agent or read_only".into()),
                    actual: Some("owner".into()),
                    correlation_id: None,
                },
            ));
        }
        TokenScopeDto::Agent => Scope::Agent,
        TokenScopeDto::ReadOnly => Scope::ReadOnly,
    };
    let issued = state
        .services
        .tokens
        .issue_token(principal.owner, request.label, scope)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedTokenDto::from_domain(issued)),
    ))
}

/// List of issued tokens.
///
/// Neither tokens nor their hashes are, or can be, included in the response: the hash is all
/// that needs to be supplied in a lookup request for the system to accept
/// the bearer as authenticated. Revoked tokens are shown: «when the token stopped
/// granting access» is a question that needs an answer.
#[utoipa::path(
    get,
    path = "/v1/tokens",
    responses(
        (status = 200, description = "Owner's tokens", body = Vec<TokenDto>),
        (status = 403, description = "Insufficient privileges", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_tokens(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<TokenDto>>, ApiFailure> {
    require_admin(&principal)?;
    let tokens = state.services.tokens.list_tokens(principal.owner).await?;
    Ok(Json(
        tokens.into_iter().map(TokenDto::from_domain).collect(),
    ))
}

/// Token revocation.
///
/// A missing token and one belonging to another owner deliberately return the same `404`: different
/// responses would reveal to an outsider that such a record exists (§14).
#[utoipa::path(
    delete,
    path = "/v1/tokens/{id}",
    params(("id" = Uuid, Path, description = "Identifier of the issued token")),
    responses(
        (status = 204, description = "Token revoked"),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 404, description = "Token does not exist or belongs to someone else", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn revoke_token(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiFailure> {
    require_admin(&principal)?;
    state
        .services
        .tokens
        .revoke_token(principal.owner, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// New contour composition version.
#[utoipa::path(
    post,
    path = "/v1/contours",
    operation_id = CREATE_CONTOUR_VERSION_OPERATION_ID,
    request_body = CreateContourVersionRequest,
    responses(
        (status = 201, description = "Version created", body = ContourVersionDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_contour_version(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CreateContourVersionRequest>,
) -> Result<(StatusCode, Json<ContourVersionDto>), ApiFailure> {
    require_admin(&principal)?;
    if request.accounts.is_empty() {
        return Err(ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError {
                code: "invalid_request".into(),
                message: "a contour with no accounts has no boundary".into(),
                field: Some("accounts".into()),
                expected: Some("at least one account".into()),
                actual: Some("empty list".into()),
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

/// Operation ingestion.
#[utoipa::path(
    post,
    path = "/v1/ingest/operations",
    request_body = SubmitOperationsRequest,
    responses(
        (status = 200, description = "Verdict for each operation", body = Vec<VerdictDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn ingest_operations(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<SubmitOperationsRequest>,
) -> Result<Json<Vec<VerdictDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let source = match &request.source {
        Some(declared) => {
            let channel = declared.channel.trim();
            if channel.is_empty() || channel.len() > 32 {
                return Err(ApiFailure::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    ApiError {
                        code: "invalid_request".into(),
                        message: "channel must be 1..=32 characters".into(),
                        field: Some("source.channel".into()),
                        expected: Some("a short channel name such as file, paste or manual".into()),
                        actual: Some(declared.channel.clone()),
                        correlation_id: None,
                    },
                ));
            }
            SourceId::declared(principal.owner, AccountId(declared.account), channel)
        }
        // No declaration: today's behaviour, so existing callers keep working.
        None => SourceId::new_random(),
    };

    // Parsing the DTO yields a verdict for each row: one unrecognised operation
    // does not invalidate the others (§10.1).
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

/// Journal fact ingestion: corporate actions and offers.
#[utoipa::path(
    post,
    path = "/v1/ingest/journal-events",
    request_body = SubmitJournalEventsRequest,
    responses(
        (status = 200, description = "Verdict for each fact", body = Vec<VerdictDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn ingest_journal_events(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<SubmitJournalEventsRequest>,
) -> Result<Json<Vec<VerdictDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let source = SourceId::new_random();

    // Parsing the DTO yields a verdict for each element: one unrecognised fact
    // does not invalidate the others (§10.1). Response order — batch order,
    // so the line number accompanies the candidate.
    let mut verdicts: Vec<VerdictDto> = Vec::with_capacity(request.events.len());
    let mut accepted: Vec<(usize, SubmittedJournalEvent)> = Vec::new();
    for (index, event) in request.events.iter().enumerate() {
        match event.to_domain() {
            Ok(domain) => accepted.push((index + 1, domain)),
            Err(rejection) => verdicts.push(VerdictDto::from_domain(
                index + 1,
                &Verdict::Rejected { rejection },
            )),
        }
    }

    let domain: Vec<SubmittedJournalEvent> =
        accepted.iter().map(|(_, event)| event.clone()).collect();
    let outcomes = submit_journal_events(&state.services, &principal, source, &domain).await?;
    for ((row, _), verdict) in accepted.iter().zip(outcomes.iter()) {
        verdicts.push(VerdictDto::from_domain(*row, verdict));
    }
    verdicts.sort_by_key(|verdict| verdict.row);
    Ok(Json(verdicts))
}

/// CSV ingestion.
#[utoipa::path(
    post,
    path = "/v1/ingest/csv",
    request_body(content = String, description = "CSV document", content_type = "text/csv"),
    responses(
        (status = 200, description = "Verdict for each row", body = Vec<VerdictDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError)
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

/// Money flow report parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct MoneyFlowParams {
    /// Scope identifier.
    pub contour: Uuid,
    /// Scope composition version. By default — the latest.
    #[serde(default)]
    pub contour_version: Option<u32>,
    /// Inclusive start, ISO-8601.
    pub from: String,
    /// Inclusive end, ISO-8601.
    pub to: String,
}

/// The flow of money over an interval.
#[utoipa::path(
    get,
    path = "/v1/reports/flow",
    params(MoneyFlowParams),
    responses(
        (status = 200, description = "Flow of money over the interval", body = MoneyFlowReportDto),
        (status = 404, description = "Scope not found", body = ApiError),
        (status = 422, description = "Invalid interval", body = ApiError),
        (status = 500, description = "Money flow could not be built", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn flow_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiQuery(params): ApiQuery<MoneyFlowParams>,
) -> Result<Json<MoneyFlowReportDto>, ApiFailure> {
    let query = MoneyFlowQuery {
        contour: ContourId(params.contour),
        contour_version: params.contour_version.map(ContourVersion),
        from: parse_query_date("from", &params.from)?,
        to: parse_query_date("to", &params.to)?,
    };
    let report = money_flow(&state.services, &principal, &query).await?;
    // No scoping: the projection admits no leg from outside the contour, so the
    // report cannot name an account it does not cover.
    let actions = iaam_app::actions::flow_diagnostics(&report)
        .iter()
        .map(|action| action_dto(action, &catalog))
        .collect();
    let dto = MoneyFlowReportDto::from_domain(&report, actions)
        .map_err(iaam_app::error::AppError::from)?;
    Ok(Json(dto))
}

/// Account balances at a date.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct BalancesParams {
    /// Scope identifier.
    pub contour: Uuid,
    /// Scope composition version. By default — the latest.
    #[serde(default)]
    pub contour_version: Option<u32>,
    /// Report date in YYYY-MM-DD format.
    pub as_of: String,
}

/// Cash and positions by contour account.
#[utoipa::path(
    get,
    path = "/v1/reports/balances",
    params(BalancesParams),
    responses(
        (status = 200, description = "Cash and positions by account", body = [AccountBalanceDto]),
        (status = 404, description = "Scope not found", body = ApiError),
        (status = 422, description = "Invalid report date", body = ApiError),
        (status = 500, description = "Balances could not be built", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn balances_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<BalancesParams>,
) -> Result<Json<Vec<AccountBalanceDto>>, ApiFailure> {
    let as_of = parse_query_date("as_of", &params.as_of)?;
    let rows = account_balances(
        &state.services,
        &principal,
        ContourId(params.contour),
        params.contour_version.map(ContourVersion),
        as_of,
    )
    .await?;
    Ok(Json(
        rows.iter().map(AccountBalanceDto::from_domain).collect(),
    ))
}

/// Returns report parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ReturnsParams {
    /// Scope identifier.
    pub contour: Uuid,
    /// Scope composition version. By default — the latest.
    #[serde(default)]
    pub contour_version: Option<u32>,
    /// Report date in YYYY-MM-DD format. By default — today.
    #[serde(default)]
    #[param(value_type = Option<String>, format = Date, example = "2026-01-01")]
    pub as_of: Option<String>,
    /// Report currency.
    pub currency: CurrencyDto,
}

/// Returns report **before tax**.
#[utoipa::path(
    get,
    path = "/v1/reports/returns",
    params(ReturnsParams),
    responses(
        (status = 200, description = "Report", body = ReturnsReportDto),
        (status = 404, description = "Scope not found", body = ApiError),
        (status = 500, description = "Invariant violated", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn returns_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<ReturnsParams>,
) -> Result<Json<ReturnsReportDto>, ApiFailure> {
    let query = ReturnsQuery {
        contour: ContourId(params.contour),
        contour_version: params.contour_version.map(ContourVersion),
        as_of: parse_as_of(params.as_of.as_deref())?,
        report_currency: params.currency.to_domain(),
        // The use case reads official exchange rates from MarketStore: the server
        // knows neither the adapter nor the source format.
        fx: FxTable::new(FxSource::CbrOfficial),
        lot_rule: LotRuleVersion(1),
    };
    let report = returns(&state.services, &principal, &query).await?;
    Ok(Json(ReturnsReportDto::from_domain(&report)))
}

/// Exchange rates supplied with the report request.
///
/// This is an explicit path for an owner-supplied source: the response is marked
/// `owner_supplied` and is not mixed with the official route above.
#[utoipa::path(
    post,
    path = "/v1/reports/returns",
    params(ReturnsParams),
    request_body = Vec<FxRateDto>,
    responses(
        (status = 200, description = "Report using the specified exchange rates", body = ReturnsReportDto),
        (status = 422, description = "Invalid exchange rate", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn returns_report_with_rates(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<ReturnsParams>,
    ApiJson(rates): ApiJson<Vec<FxRateDto>>,
) -> Result<Json<ReturnsReportDto>, ApiFailure> {
    let mut fx = FxTable::new(FxSource::OwnerSupplied);
    for rate in &rates {
        let parsed = rate.rate.parse::<Decimal>().map_err(|_| {
            ApiFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: "invalid_request".into(),
                    message: "exchange rate must be a decimal number".into(),
                    field: Some("rate".into()),
                    expected: Some("decimal number represented as a string".into()),
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

fn instrument_dto(instrument: iaam_app::ports::InstrumentView) -> InstrumentDto {
    InstrumentDto {
        id: instrument.id.inner().to_string(),
        kind: instrument.kind,
        symbol: instrument.symbol,
        title: instrument.title,
        denomination_currency: instrument.denomination_currency,
        settlement_currency: instrument.settlement_currency,
        quote_currency: instrument.quote_currency,
    }
}

fn document_dto(document: UploadedDocument) -> DocumentDto {
    let (period_from, period_to) = document
        .period
        .map_or((None, None), |period| (Some(period.from), Some(period.to)));
    DocumentDto {
        document_hash: document.document_hash,
        source: document.source.inner(),
        broker: document.broker.code().to_owned(),
        format: document.format.code().to_owned(),
        parser_version: document.parser_version.0,
        period_from,
        period_to,
        rows: document
            .rows
            .into_iter()
            .map(|row| VerdictDto::from_domain(row.row as usize, &row.verdict))
            .collect(),
    }
}

fn reconciliation_status_dto(status: &ReconciliationStatus) -> ReconciliationStatusDto {
    ReconciliationStatusDto::from_domain(status)
}

fn parse_knowledge_as_of(value: Option<&str>) -> Result<OffsetDateTime, ApiFailure> {
    let Some(value) = value else {
        return Ok(OffsetDateTime::now_utc());
    };
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| invalid_field("knowledge_as_of", "RFC 3339", value.to_owned()))
}

fn market_price_dto(
    view: iaam_app::scenarios::market_reference::MarketPriceView,
) -> MarketPriceDto {
    MarketPriceDto {
        instrument: view.instrument.inner(),
        board: view.board,
        session: view.session,
        kind: view.kind,
        value: view.value,
        currency: view.currency,
        recorded_quotation_basis: view.recorded_quotation_basis,
        quotation_basis_status: QuotationBasisStatusDto::from_domain(view.quotation_basis_status),
        quotation_basis: QuotationBasisDto::from_domain(view.quotation_basis),
        basis_evidence: (!view.basis_evidence.is_empty()).then_some(view.basis_evidence),
        date: view.date,
        source: view.source,
        observed_at: view.observed_at,
        quality: view.quality,
    }
}

fn market_fx_dto(view: iaam_app::scenarios::market_reference::MarketFxView) -> MarketFxDto {
    MarketFxDto {
        from: CurrencyDto::from_domain(view.from),
        to: CurrencyDto::from_domain(view.to),
        nominal: view.nominal,
        value: view.value,
        unit_rate: view.unit_rate,
        date: view.date,
        source: view.source,
        observed_at: view.observed_at,
        quality: view.quality,
    }
}

fn market_key_rate_dto(
    view: iaam_app::scenarios::market_reference::MarketKeyRateView,
) -> MarketKeyRateDto {
    MarketKeyRateDto {
        value: view.value,
        from: view.from,
        until: view.until,
        source: view.source,
        observed_at: view.observed_at,
        quality: view.quality,
        boundary: view.boundary,
    }
}

fn parse_category_matcher(value: serde_json::Value) -> Result<CategoryMatcher, ApiFailure> {
    if let Some(raw) = value.as_str() {
        let parsed = serde_json::from_str(raw)
            .map_err(|_| invalid_field("matcher", "a category matcher object", raw.to_owned()))?;
        return parse_category_matcher(parsed);
    }
    let Some(object) = value.as_object() else {
        return Err(invalid_field(
            "matcher",
            "a category matcher object",
            value.to_string(),
        ));
    };

    if let Some(kind) = object.get("kind").and_then(serde_json::Value::as_str) {
        let payload = object.get("value").unwrap_or(&serde_json::Value::Null);
        return Ok(match kind {
            "row" => CategoryMatcher::Row {
                key: matcher_text(payload, "key")?,
            },
            "source_category" => CategoryMatcher::SourceCategory {
                value: matcher_text(payload, "value")?,
            },
            "description_contains" => CategoryMatcher::DescriptionContains {
                text: matcher_text(payload, "text")?,
            },
            _ => {
                return Err(invalid_field(
                    "matcher.kind",
                    "row, source_category or description_contains",
                    kind.to_owned(),
                ));
            }
        });
    }

    let (kind, payload) = [
        "Row",
        "row",
        "row_key",
        "SourceCategory",
        "source_category",
        "DescriptionContains",
        "description_contains",
    ]
    .iter()
    .find_map(|key| object.get(*key).map(|payload| (*key, payload)))
    .ok_or_else(|| {
        invalid_field(
            "matcher",
            "row, source_category or description_contains",
            value.to_string(),
        )
    })?;
    let text = matcher_text(
        payload,
        match kind {
            "Row" | "row" | "row_key" => "key",
            "SourceCategory" | "source_category" => "value",
            "DescriptionContains" | "description_contains" => "text",
            _ => unreachable!("matcher key was selected above"),
        },
    )?;
    Ok(match kind {
        "Row" | "row" | "row_key" => CategoryMatcher::Row { key: text },
        "SourceCategory" | "source_category" => CategoryMatcher::SourceCategory { value: text },
        "DescriptionContains" | "description_contains" => {
            CategoryMatcher::DescriptionContains { text }
        }
        _ => unreachable!("matcher key was selected above"),
    })
}

fn matcher_text(value: &serde_json::Value, field: &str) -> Result<String, ApiFailure> {
    value
        .as_str()
        .or_else(|| value.get(field).and_then(serde_json::Value::as_str))
        .or_else(|| value.get("value").and_then(serde_json::Value::as_str))
        .or_else(|| value.get("text").and_then(serde_json::Value::as_str))
        .or_else(|| value.get("key").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| {
            invalid_field(
                "matcher",
                "a category matcher with a string value",
                value.to_string(),
            )
        })
}
/// Journal read parameters. Every filter is optional and they combine.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct JournalParams {
    /// The client key supplied at ingest. It addresses at most one event, so a
    /// key that matches nothing is reported as a missing resource rather than
    /// as an empty page.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Only events recorded against this account.
    #[serde(default)]
    pub account: Option<Uuid>,
    /// Account of the source the caller declared when it submitted. Supplied
    /// together with `source_channel`; the pair is how a caller asks what one
    /// import put in.
    #[serde(default)]
    pub source_account: Option<Uuid>,
    /// Channel of the declared source: `file`, `paste`, `manual`.
    #[serde(default)]
    pub source_channel: Option<String>,
    /// Inclusive start of the effective-date interval, YYYY-MM-DD.
    #[serde(default)]
    #[param(value_type = Option<String>, format = Date)]
    pub from: Option<String>,
    /// Inclusive end of the effective-date interval, YYYY-MM-DD.
    #[serde(default)]
    #[param(value_type = Option<String>, format = Date)]
    pub to: Option<String>,
    /// Position returned as `next` by an earlier page. Absent reads from the
    /// start of the journal.
    #[serde(default)]
    pub after: Option<String>,
    /// Rows per page, 1 to 200. Absent means 50.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// The owner's journal events, a page at a time.
///
/// **These are journal events, not the operations that were submitted.** Ingest
/// normalises an operation into an event and keeps the event; the operation as
/// posted is not stored and cannot be handed back. A deposit submitted as an
/// operation comes back here as a `cash_in` event carrying one cash leg, and an
/// agent that reports it as "the operation" will misdescribe what the system
/// holds.
///
/// Every filter is optional. Without any, the whole journal is readable a page
/// at a time, oldest first, ordered by effective date and then by the order
/// within that date — the pair the journal's own uniqueness is built on, so a
/// page can neither skip nor repeat a row.
///
/// No number here is computed: legs are returned exactly as recorded and
/// nothing is summed.
#[utoipa::path(
    get,
    path = "/v1/journal/events",
    params(JournalParams),
    responses(
        (status = 200, description = "One page of journal events", body = JournalPageDto),
        (status = 404, description = "An idempotency key that addresses no event", body = ApiError),
        (status = 422, description = "A parameter could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_journal_events(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(params): ApiQuery<JournalParams>,
) -> Result<Json<JournalPageDto>, ApiFailure> {
    let from = params
        .from
        .as_deref()
        .map(|value| parse_query_date("from", value))
        .transpose()?;
    let to = params
        .to
        .as_deref()
        .map(|value| parse_query_date("to", value))
        .transpose()?;
    let source = declared_source_filter(params.source_account, params.source_channel)?;
    let page = read_journal(
        state.services.store.as_ref(),
        principal.owner,
        JournalReadQuery {
            idempotency_key: params.idempotency_key,
            account: params.account.map(AccountId),
            source,
            from,
            to,
            after: params.after,
            limit: params.limit,
        },
    )
    .await?;
    Ok(Json(JournalPageDto {
        rows: page
            .rows
            .iter()
            .map(JournalEventReadDto::from_domain)
            .collect(),
        next: page.next,
    }))
}

/// The two halves of a declared source travel together.
///
/// Half a source is not a narrower filter, it is a different question the
/// caller did not mean to ask: an account alone would silently widen the answer
/// to every channel, and a channel alone to every account.
fn declared_source_filter(
    account: Option<Uuid>,
    channel: Option<String>,
) -> Result<Option<DeclaredSource>, ApiFailure> {
    match (account, channel) {
        (None, None) => Ok(None),
        (Some(account), Some(channel)) => Ok(Some(DeclaredSource {
            account: AccountId(account),
            channel,
        })),
        (Some(_), None) => Err(missing_companion(
            "source_channel",
            "a channel, supplied together with source_account",
        )),
        (None, Some(_)) => Err(missing_companion(
            "source_account",
            "an account, supplied together with source_channel",
        )),
    }
}

/// A parameter that is required because another one was supplied.
///
/// There is no `actual` to report — the field is absent, and echoing the
/// companion's value instead would name one field and quote another.
fn missing_companion(field: &'static str, expected: &'static str) -> ApiFailure {
    ApiFailure::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError {
            code: "invalid_request".into(),
            message: format!("required query parameter {field} is missing"),
            field: Some(field.to_owned()),
            expected: Some(expected.to_owned()),
            actual: None,
            correlation_id: None,
        },
    )
}

fn parse_query_date(field: &'static str, value: &str) -> Result<Date, ApiFailure> {
    Date::parse(
        value,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| invalid_field(field, "YYYY-MM-DD", value.to_owned()))
}

fn parse_currency(field: &'static str, value: &str) -> Result<CurrencyCode, ApiFailure> {
    CurrencyCode::from_code(value)
        .ok_or_else(|| invalid_field(field, "RUB, USD, EUR, CNY or XAU", value.to_owned()))
}

fn invalid_field(field: impl Into<String>, expected: &str, actual: String) -> ApiFailure {
    let field = field.into();
    ApiFailure::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError {
            code: "invalid_request".into(),
            message: format!("invalid field {field}"),
            field: Some(field),
            expected: Some(expected.into()),
            actual: Some(actual),
            correlation_id: None,
        },
    )
}
fn invalid_rejection(rejection: Rejection) -> ApiFailure {
    invalid_field(rejection.field, &rejection.expected, rejection.actual)
}

///
/// Separate function with an explicit `422` rejection: `serde` for `time::Date`
/// does not accept a «YYYY-MM-DD» string without a specified format, and silently
/// defaulting to «today» for an unrecognised date would produce a report for the wrong date.
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
                message: "report date must be in YYYY-MM-DD format".into(),
                field: Some("as_of".into()),
                expected: Some("YYYY-MM-DD".into()),
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

/// Name lookup for CSV parsing.
///
/// Accounts and custody locations belonging to the owner are resolved by name.
/// Instruments are preloaded with all validity intervals for external
/// codes, so each document row can be resolved as at its own date.
async fn build_directory(
    services: &Arc<AppServices>,
    principal: &Principal,
) -> Result<Directory, ApiFailure> {
    let accounts = services.store.list_accounts(principal.owner).await?;
    let places = iaam_app::ports::InstrumentDirectory::list_custody_places(
        &*services.directory,
        principal.owner,
    )
    .await?;
    let aliases = iaam_app::ports::InstrumentDirectory::list_aliases(&*services.directory).await?;

    let mut directory = Directory::default();
    for account in accounts {
        directory
            .accounts
            .entry(account.title)
            .or_default()
            .push(account.id);
    }
    for place in places {
        directory
            .custodies
            .entry(place.title)
            .or_default()
            .push(place.id);
    }
    // All aliases are inserted: otherwise document parsing would query the database
    // for each row, and there are thousands of rows in the report.
    for alias in aliases {
        directory.instruments.entry(alias.value).or_default().push((
            alias.namespace,
            iaam_core::instrument::AliasInterval {
                valid_from: alias.valid_from,
                valid_to: alias.valid_to,
            },
            alias.instrument,
        ));
    }
    Ok(directory)
}
