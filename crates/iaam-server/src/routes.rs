//! Routes.
//!
//! The handler does three things: parses the DTO, calls the use case, and serialises
//! the result. There are no arithmetic operations on money here —
//! this is enforced by the architecture guard (§3.1, §13).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::Response;
use axum::{Extension, Json};
use iaam_app::AppServices;
use iaam_app::actions::{
    AccountCandidate, AccountScope, Action, ActionCategory, ActionState, ActionSubject,
    ActionTarget, InputAlternative, MissingInput, OperationKey, ProvidedBy, RequestPlan,
    account_scope,
};
use iaam_app::ingest::csv_source::{Directory, ParsedRow, parse};
use iaam_app::ingest::observation::Intake;
use iaam_app::ingest::{Rejection, SubmittedJournalEvent, SubmittedOperation, Verdict};
use iaam_app::ports::{
    AccountAliasView, AccountCreated, AccountDeclarations, AccountDetailView, AccountIdentityView,
    AccountScopeExclusionView, AccountTransferStatementView, AccountView, ContourView, Declared,
    Principal, Scope,
};
use iaam_app::scenarios::categories::{
    CategoryRuleInput, create_category, create_category_rule, create_group, list_categories,
    list_category_rules, list_groups, preview_category_rule, retire_category,
};
use iaam_app::scenarios::classification::{create_rule, list_rules, retire_rule};
use iaam_app::scenarios::correction::{ImportTarget, correct_events};
use iaam_app::scenarios::documents::{reparse_report, upload_report};
use iaam_app::scenarios::import_session::{HeldRow, IntakeOutcome, SessionContents, submit_intake};
use iaam_app::scenarios::ingest::{submit_journal_events, submit_operations};
use iaam_app::scenarios::journal::{DeclaredSource, JournalReadQuery, read_journal};
use iaam_app::scenarios::market_reference::{
    MarketFxQuery, MarketKeyRateQuery, MarketPricesQuery, list_market_fx as read_market_fx,
    list_market_key_rate as read_market_key_rate, list_market_prices as read_market_prices,
};
use iaam_app::scenarios::reconciliation::{OwnerBalance, record_owner_balance, report, statuses};
use iaam_app::scenarios::reports::{
    MoneyFlowQuery, ReturnsQuery, account_balances, asset_snapshot, money_flow, returns,
};
use iaam_app::sync::{
    MarketSource, MarketSyncRequest as AppMarketSyncRequest, sync_broker as run_sync_broker,
    sync_market_with_services as run_market_sync,
};
use iaam_core::category::{CategoryInterval, CategoryMatcher, CategoryRuleProposal};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{
    AccountId, CategoryId, CategoryRuleId, CustodyId, EventId, ImportId, ImportQuestionId,
    ImportSessionId, InstrumentId, SourceId,
};
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::PROJECTION_VERSION;
use iaam_core::reconciliation::ReconciliationStatus;
use iaam_core::reconciliation::claim::AssertionPeriod;
use iaam_core::rules::LotRuleVersion;
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use utoipa::IntoParams;
use uuid::Uuid;

use crate::ServerState;
use crate::action_catalog::ActionCatalog;
use crate::api_catalog::ApiCatalog;
use crate::dto::{
    AccountAliasDto, AccountCandidateDto, AccountCashClassStatementDto, AccountDeclarationsDto,
    AccountDto, AccountIdentityNotDoneDto, AccountIdentityRepointedDto, AccountIdentityStatedDto,
    AccountIdentityStatementDto, AccountNegativeBalanceExpectationStatementDto,
    AccountScopeDispositionDto, AccountScopeDto, AccountTransferPartnersBatchDto,
    AccountTransferPartnersDto, ActionDto, ActionSubjectDto, ActionTargetDto,
    AddContourVersionRequest, AssetSnapshotDto, BalancesReportDto, BrokerAccessDto,
    BrokerSyncRequest, CashAssetClassDto, CategoryDto, CategoryGroupDto, CategoryGroupRequest,
    CategoryRequest, CategoryRuleDto, CategoryRuleImpactDto, CategoryRuleRequest,
    ClassificationRuleChangeDto, ClassificationRuleDto, ClassificationRuleRequest, ContourDto,
    ContourVersionDto, CorrectImportRequest, CreateAccountRequest, CreateContourVersionRequest,
    CreateInstrumentRequest, CreateTokenRequest, CurrencyDto, CustodyRepairOutcomeDto,
    CustodyRepairRequest, DeclaredAccountDto, DeclaredSourceDto, DocumentDto, DocumentParams,
    FxRateDto, HealthDto, ImportCorrectionDto, InputAlternativeDto, InstrumentDto, IssuedTokenDto,
    JournalEventReadDto, JournalPageDto, MarketFxDto, MarketFxSeriesDto, MarketKeyRateDto,
    MarketKeyRateSeriesDto, MarketPriceDto, MarketPriceSeriesDto, MarketSourceDto,
    MarketSyncRequest, MissingInputDto, MoneyFlowReportDto, NegativeBalanceExpectationDto,
    OwnerBalanceRequest, QuotationBasisDto, QuotationBasisStatusDto, RecomputePlanDto,
    ReconciliationParams, ReconciliationResponseDto, ReconciliationStatusDto,
    RecordAccountScopeRequest, RecordAccountTransferPartnersBatchRequest,
    RecordAccountTransferPartnersRequest, ReplaceAccountAliasesRequest,
    ReplaceAccountDeclarationsRequest, RequestPlanDto, RequiredInputDto, ResolutionOptionDto,
    ResolveInstrumentRequest, ResolvedInstrumentDto, ReturnsAnswerDto, SubmitCorrectionsRequest,
    SubmitJournalEventsRequest, SubmitOperationsRequest, SyncOutcomeDto, TokenDto, TokenScopeDto,
    VerdictDto,
};
use crate::dto::{
    AddImportRowsRequest, AnswerAlternativeDto, AnswerImportQuestionRequest,
    CommitImportSessionRequest, ConfirmTransferPairingRequest, ConfirmedPairingDto,
    ControlSectionDto, CrossSourceMatchingDto, ImportCommitDto, ImportPlanDto, ImportQuestionDto,
    ImportRowDto, ImportSessionContentsDto, ImportSessionDto, OpenImportSessionRequest,
    RecordedEventDto, StateImportControlFiguresRequest,
};
use crate::error::{ApiError, ApiFailure};
use crate::extract::{ApiBytes, ApiJson, ApiJsonOrDefault, ApiPath, ApiQuery};
use iaam_app::scenarios::documents::UploadedDocument;
use iaam_app::scenarios::import_session::SessionRevision;
use iaam_core::batch::ControlSection;

pub const CREATE_ACCOUNT_OPERATION_ID: &str = "create_account";
pub const CREATE_CONTOUR_VERSION_OPERATION_ID: &str = "create_contour_version";
pub const ADD_CONTOUR_VERSION_OPERATION_ID: &str = "add_contour_version";
pub const RECORD_ACCOUNT_SCOPE_OPERATION_ID: &str = "record_account_scope";
pub const REPLACE_ACCOUNT_ALIASES_OPERATION_ID: &str = "replace_account_aliases";
pub const REPLACE_ACCOUNT_DECLARATIONS_OPERATION_ID: &str = "replace_account_declarations";
pub const RECORD_ACCOUNT_TRANSFER_PARTNERS_OPERATION_ID: &str = "record_account_transfer_partners";
/// The batch form. Deliberately absent from [`OperationKey`]: the action queue
/// names the per-account operation, one item per account, and this is the
/// transport a caller holding several of those items may use instead.
pub const RECORD_ACCOUNT_TRANSFER_PARTNERS_BATCH_OPERATION_ID: &str =
    "record_account_transfer_partners_batch";
pub const RECORD_OWNER_BALANCE_OPERATION_ID: &str = "record_owner_balance";
pub const CREATE_CATEGORY_RULE_OPERATION_ID: &str = "create_category_rule";

/// The computed actions currently blocking or advancing owner setup.
#[utoipa::path(
    get,
    path = "/v1/actions",
    responses((status = 200, description = "Computed owner actions", body = Vec<ActionDto>)),
    security(("bearer" = []))
)]
pub async fn list_actions(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
) -> Result<Json<Vec<ActionDto>>, ApiFailure> {
    let actions =
        iaam_app::actions::frontier(principal.owner, state.services.store.as_ref()).await?;
    Ok(Json(
        actions
            .iter()
            .map(|action| action_dto(action, &catalog))
            .collect(),
    ))
}

fn action_dto(action: &Action, catalog: &ActionCatalog) -> ActionDto {
    let target = match action.target() {
        ActionTarget::Operation { operation, request } => {
            let resolved = resolution_option_dto(*operation, request, catalog);
            ActionTargetDto::Operation {
                operation_id: resolved.operation_id,
                method: resolved.method,
                path: resolved.path,
                request_schema: resolved.request_schema,
                request: resolved.request,
            }
        }
        // Every option is resolved through the same catalogue as a sole target,
        // so an alternative way out is addressed exactly as well as the first.
        ActionTarget::Options(options) => ActionTargetDto::Options {
            options: options
                .iter()
                .map(|option| resolution_option_dto(option.operation, &option.request, catalog))
                .collect(),
        },
        ActionTarget::None => ActionTargetDto::None,
    };
    ActionDto {
        id: action.id().to_owned(),
        kind: action.kind().id().to_owned(),
        category: match action.category() {
            ActionCategory::Blocking => "blocking",
            ActionCategory::RequiredForGoal(_) => "required_for_goal",
            ActionCategory::Recommended => "recommended",
            ActionCategory::Informational => "informational",
        }
        .to_owned(),
        // Empty for every category but the required one, which is where
        // `ActionCategory::goals` puts the emptiness rather than this mapping
        // repeating the case analysis one line below the one above.
        goals: action
            .category()
            .goals()
            .iter()
            .map(|goal| goal.code().to_owned())
            .collect(),
        state: match action.state() {
            ActionState::Ready => "ready",
            ActionState::NeedsOwnerInput => "needs_owner_input",
            ActionState::Blocked => "blocked",
        }
        .to_owned(),
        reason: action.reason().to_owned(),
        required_scope: action.required_scope().map(|scope| scope.code().to_owned()),
        // Copied, not looked up: the pairing of an account with what the owner
        // calls it is made where the item is made, so the name here and the name
        // inside `reason` are one reading of the store rather than two.
        subject: action.subject().map(|subject| match subject {
            ActionSubject::Account(account) => ActionSubjectDto::Account {
                id: account.id.inner(),
                title: account.title.clone(),
                institution: account.institution.clone(),
            },
            ActionSubject::Event(event) => ActionSubjectDto::Event { id: event.inner() },
        }),
        target,
    }
}

/// One way to close an action, addressed against the completed contract.
///
/// Visible to the crate because a rejection publishes the same shape: a refusal
/// whose remedy is another call names it exactly as an action does, and a second
/// construction of the address would eventually offer a route the queue does not.
pub(crate) fn resolution_option_dto(
    operation: OperationKey,
    request: &RequestPlan,
    catalog: &ActionCatalog,
) -> ResolutionOptionDto {
    let resolved = catalog.operation(operation);
    ResolutionOptionDto {
        operation_id: resolved.operation_id.clone(),
        method: resolved.method.clone(),
        path: resolved.path.clone(),
        request_schema: resolved.request_schema.clone(),
        request: RequestPlanDto {
            preset: request.preset.clone(),
            missing: request.missing.iter().map(missing_input_dto).collect(),
        },
    }
}

/// One missing field, the alternatives it admits, and what each of them needs.
fn missing_input_dto(missing: &MissingInput) -> MissingInputDto {
    MissingInputDto {
        pointer: missing.pointer.clone(),
        provided_by: provided_by_code(missing.provided_by),
        candidates: missing.candidates.as_deref().map(account_candidate_dtos),
        alternatives: missing
            .alternatives
            .iter()
            .map(input_alternative_dto)
            .collect(),
    }
}

/// One admissible value, and the fields choosing it then needs.
///
/// Lifted out of [`missing_input_dto`] because a rejection publishes the same
/// list without a missing input around it: the values a field admits are the
/// values it admits whether the caller has yet to supply one or supplied a wrong
/// one.
pub(crate) fn input_alternative_dto(alternative: &InputAlternative) -> InputAlternativeDto {
    InputAlternativeDto {
        value: alternative.value.clone(),
        requires: alternative
            .requires
            .iter()
            .map(|required| RequiredInputDto {
                pointer: required.pointer.clone(),
                provided_by: provided_by_code(required.provided_by),
                candidates: required.candidates.as_deref().map(account_candidate_dtos),
            })
            .collect(),
    }
}

fn account_candidate_dtos(candidates: &[AccountCandidate]) -> Vec<AccountCandidateDto> {
    candidates
        .iter()
        .map(|candidate| AccountCandidateDto {
            id: candidate.id.inner(),
            title: candidate.title.clone(),
            institution: candidate.institution.clone(),
        })
        .collect()
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

/// Re-parse a source the system already keeps.
#[utoipa::path(
    post,
    path = "/v1/documents/{id}/reparse",
    params(
        ("id" = String, Path, description = "SHA-256 of the source document"),
        DocumentParams
    ),
    request_body(
        content = String,
        description = "Empty: the document stored under this hash is parsed again, so a \
                       caller holding only the hash needs nothing else. A binary XLSX/XLS \
                       workbook is still accepted, and its hash still checked, for a \
                       document uploaded before the system began storing sources — one \
                       recorded facts but kept no body. Sending it stores it, and the next \
                       reparse of that document needs no body."
    ),
    responses(
        (status = 200, description = "Outcome for each row", body = DocumentDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 422, description = "Hash invalid, document invalid, or no stored document and no body", body = ApiError),
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
    // An empty body means «parse what you kept», not «parse an empty workbook»:
    // the second is never a valid report, so nothing legitimate is lost by
    // reading the absence this way, and the founding constraint is served — the
    // caller names the document and holds none of it.
    let supplied = (!body.is_empty()).then(|| body.as_ref());
    let result = reparse_report(
        &state.services,
        &principal,
        &document_hash,
        supplied,
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

/// Correct events the owner names: retract one, or supersede one with another.
///
/// A separate route rather than a relation field on the ingest DTOs, and this is
/// a security decision rather than a stylistic one. `Scope::may_submit` admits
/// an agent token, so an ingest row able to carry a relation would make every
/// ingest handler — operations, CSV, journal facts, broker synchronisation — a
/// surface on which an agent could retract the owner's history, guarded only by
/// a per-row check that any one of those inputs could forget to make. Here the
/// authority is a property of the route and is checked once, exactly as
/// `require_admin` guards every other owner-only route in this file.
///
/// Permission is checked before the body is parsed: an agent token receives 403
/// even for a body that is itself invalid (§7, §14).
#[utoipa::path(
    post,
    path = "/v1/corrections",
    request_body = SubmitCorrectionsRequest,
    responses(
        (status = 200, description = "Verdict for each correction", body = Vec<VerdictDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 409, description = "A correction key is held by an unrelated event", body = ApiError),
        (status = 422, description = "The correction would not resolve, or the acknowledgement is missing", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn submit_corrections(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiBytes(body): ApiBytes,
) -> Result<Json<Vec<VerdictDto>>, ApiFailure> {
    require_admin(&principal)?;
    let request: SubmitCorrectionsRequest = serde_json::from_slice(&body)
        .map_err(|error| invalid_field("body", "correction JSON object", error.to_string()))?;

    // The batch is converted whole before anything is written: a correction is
    // one deliberate act, and half of one applied is worse than none.
    let mut corrections = Vec::with_capacity(request.corrections.len());
    for (index, correction) in request.corrections.iter().enumerate() {
        let domain = correction.to_domain().map_err(|rejection| {
            invalid_field(
                format!("corrections[{index}].{}", rejection.field),
                &rejection.expected,
                rejection.actual,
            )
        })?;
        corrections.push(domain);
    }

    let verdicts = correct_events(
        &state.services,
        &principal,
        request.acknowledge_retraction,
        &corrections,
    )
    .await?;
    Ok(Json(
        verdicts
            .iter()
            .enumerate()
            .map(|(index, verdict)| VerdictDto::from_domain(index + 1, verdict))
            .collect(),
    ))
}

/// Retract one import, keyed on the declaration that was made for it.
///
/// The remedy for a month imported against the wrong account map: one request,
/// and one reversal fact per event that import left effective. Nothing is
/// deleted and nothing is mutated — the originals stay in the journal and stop
/// counting, which is what §4.8 means by a correction.
///
/// What it retracts, exactly: the rows of the account, channel **and label**
/// named in the request. Other imports through the same account and channel,
/// under other labels, are left in force. A request that names no label
/// retracts instead every row of that account and channel which named no
/// import — the rows recorded before an import could be named, and the only
/// way to reach them.
///
/// **Who may call it (`iaam-rond`).** The owner, for any import. An agent, for
/// an import it declared itself and has not yet built anything on — the exact
/// bound, and the reasoning for it, are on
/// `iaam_app::scenarios::correction::correct_import`. The route no longer
/// refuses the agent outright: committing an import is open to it and rewrites
/// every downstream report, so refusing it the retraction closed only the safer
/// of the two directions and left an agent that discovers its own mistake by
/// control total with nothing to do but wake the owner.
#[utoipa::path(
    post,
    path = "/v1/corrections/imports",
    description = "Retract one import: the rows submitted under the account, \
                   channel and label named here. Other imports through the same \
                   account and channel, under other labels, keep counting. A \
                   request without a label retracts every row of that account \
                   and channel that named no import. One reversal fact per \
                   retracted event; nothing is deleted and nothing is mutated. \
                   The owner may retract any import. An agent may retract only \
                   one it declared itself, under the label it submitted under, \
                   and only while every row of it is still effective and no \
                   control assertion covers them; anything else is refused and \
                   the refusal says which of those it was.",
    request_body = CorrectImportRequest,
    responses(
        (status = 200, description = "What the correction retracted", body = ImportCorrectionDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 409, description = "A correction key is held by an unrelated event", body = ApiError),
        (status = 422, description = "Invalid source, the acknowledgement is missing, or this import is not the caller's to retract", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn correct_import(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiBytes(body): ApiBytes,
) -> Result<Json<ImportCorrectionDto>, ApiFailure> {
    // Only the floor is checked here. What an agent may retract depends on what
    // the journal says it declared, and the transport has no journal: the
    // scenario decides it against the same read the reversal is computed from.
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let request: CorrectImportRequest = serde_json::from_slice(&body).map_err(|error| {
        invalid_field("body", "import correction JSON object", error.to_string())
    })?;
    let account = declared_account(&state, &principal, &request.source)
        .await?
        .id;
    let source = declared_source(principal.owner, account, &request.source)?;
    let target = match declared_import(principal.owner, account, &request.source)? {
        Some(import) => ImportTarget::Named { source, import },
        None => ImportTarget::Unnamed { source },
    };
    let outcome = iaam_app::scenarios::correction::correct_import(
        &state.services,
        &principal,
        request.acknowledge_retraction,
        target,
    )
    .await?;
    Ok(Json(ImportCorrectionDto::from_domain(outcome)))
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
    // The point is no longer parsed here: `BalancePointDto` is the enumeration,
    // so a value outside it is refused by the body extractor with both codes
    // named, and this handler cannot be reached holding a third one.
    let at = request.at.to_domain();
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
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

/// Store a rule and answer with what it would correct.
///
/// The plan is returned rather than applied. Correcting the journal writes
/// reversal and replacement facts that stop counting in every report the owner
/// has already read, and this codebase demands an explicit acknowledgement for
/// exactly that — see `POST /v1/corrections`, which is the operation that
/// applies the plan. A rule creation carries no such acknowledgement, so it
/// says what it would do and does not pretend to have done it.
#[utoipa::path(
    post,
    path = "/v1/classification-rules",
    request_body = ClassificationRuleRequest,
    responses(
        (status = 201, description = "Rule added, with the history it would correct", body = ClassificationRuleChangeDto),
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
) -> Result<(StatusCode, Json<ClassificationRuleChangeDto>), ApiFailure> {
    require_admin(&principal)?;
    let change = create_rule(
        &state.services,
        &principal,
        &request.matcher.to_domain(),
        request.outcome.to_domain()?,
        request.replaces,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(ClassificationRuleChangeDto::from_domain(change)?),
    ))
}

/// Retire a rule and answer with what its absence would correct.
///
/// `200` with the plan, not `204`: retirement recomputes exactly what creation
/// does, and a body-less response would discard it — which is the defect this
/// route had. Symmetry with [`create_classification_rule`] is the point.
#[utoipa::path(
    delete,
    path = "/v1/classification-rules/{id}",
    params(("id" = Uuid, Path, description = "Rule identifier")),
    responses(
        (status = 200, description = "Rule retired, with the history its absence would correct", body = RecomputePlanDto),
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
) -> Result<Json<RecomputePlanDto>, ApiFailure> {
    require_admin(&principal)?;
    let plan = retire_rule(&state.services, &principal, id).await?;
    Ok(Json(RecomputePlanDto::from_domain(plan)))
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
    operation_id = CREATE_CATEGORY_RULE_OPERATION_ID,
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
#[into_params(parameter_in = Query)]
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
#[into_params(parameter_in = Query)]
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
#[into_params(parameter_in = Query)]
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
///
/// Not a directory of the sixty-six routes — the contract behind `service-desc`
/// is that, and it is generated. This document is the ordering the contract has
/// no way to express: the four goals this API answers and the route that answers
/// each, the queue that says which of them this instance cannot answer yet, and
/// the scopes every goal route takes an id from. See [`crate::api_catalog`] for
/// why it is resolved from the router rather than written out.
#[utoipa::path(
    get,
    path = "/.well-known/api-catalog",
    responses((
        status = 200,
        description = "The contract, the health resource, the outstanding-work queue, the scopes, \
                       and the route answering each of the four report goals",
        content_type = "application/linkset+json"
    ))
)]
pub async fn api_catalog(Extension(catalog): Extension<Arc<ApiCatalog>>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/linkset+json")
        .body(Body::from(catalog.body()))
        .expect("catalog response is valid")
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
    let accounts = state
        .services
        .store
        .list_account_details(principal.owner)
        .await?;
    Ok(Json(accounts.into_iter().map(account_dto).collect()))
}

/// Create an account.
#[utoipa::path(
    post,
    path = "/v1/accounts",
    operation_id = CREATE_ACCOUNT_OPERATION_ID,
    request_body = CreateAccountRequest,
    responses(
        (status = 201, description = "Account created", body = AccountDto),
        (status = 200, description = "The external identity was already known: this is the \
                                      account created last time, unchanged", body = AccountDto),
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
    // The pair is the identity, so half of it is refused rather than silently
    // stored as no identity at all. This is a check on the shape of the pair and
    // never on the value: `provider_account_id` stays opaque, and the refusal
    // does not echo it back.
    let (provider, provider_account_id) = match (request.provider, request.provider_account_id) {
        (Some(provider), Some(provider_account_id)) => (Some(provider), Some(provider_account_id)),
        (None, None) => (None, None),
        (Some(_), None) => {
            return Err(unprocessable(
                "provider_account_id",
                "both halves of the external identity, or neither",
                "provider alone",
                "an account identified at a source is identified by the pair: a \
                 request naming only the source would be stored as naming no \
                 identity, and the next import would mint a second account",
            ));
        }
        (None, Some(_)) => {
            return Err(unprocessable(
                "provider",
                "both halves of the external identity, or neither",
                "provider_account_id alone",
                "an identifier without the source that printed it has no scope: \
                 two sources printing short sequential identifiers would collide \
                 on values neither of them controls",
            ));
        }
    };

    let aliases = alias_views(request.aliases)?;

    let account = AccountDetailView {
        id: AccountId::new_random(),
        title: request.title,
        institution: request.institution,
        provider,
        provider_account_id,
        cash_class: request.cash_class.map(CashAssetClassDto::to_domain),
        negative_balance_expectation: request
            .negative_balance_expectation
            .map(NegativeBalanceExpectationDto::to_domain),
        aliases,
    };
    let created = state
        .services
        .store
        .create_account(principal.owner, account)
        .await?;

    // `200 OK` when the identity was already known: nothing was created, and
    // reporting `201` for an account minted on an earlier call would be a lie a
    // client cannot check.
    let status = match created {
        AccountCreated::Created(_) => StatusCode::CREATED,
        AccountCreated::Existing(_) => StatusCode::OK,
    };
    let account = match created {
        AccountCreated::Created(account) | AccountCreated::Existing(account) => account,
    };
    Ok((status, Json(account_dto(account))))
}

/// State an account's aliases.
///
/// The route decision 0004 asks for by name: an account's further identifiers
/// must be addable, closable and readable after it exists, because the case the
/// decision was written about — a second card over one underlying account —
/// usually arrives after the account does.
///
/// The whole set is replaced, following the transfer statement: the owner says
/// what is true now, and a diff against what he said last time is a second thing
/// to get wrong. A card that stopped working is sent as an alias whose
/// `valid_to` is set, beside whichever alias replaced it.
#[utoipa::path(
    put,
    path = "/v1/accounts/{id}/aliases",
    operation_id = REPLACE_ACCOUNT_ALIASES_OPERATION_ID,
    params(("id" = Uuid, Path, description = "Account identifier")),
    request_body = ReplaceAccountAliasesRequest,
    responses(
        (status = 200, description = "Aliases recorded", body = AccountDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "Account does not exist or belongs to someone else", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn replace_account_aliases(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<ReplaceAccountAliasesRequest>,
) -> Result<Json<AccountDto>, ApiFailure> {
    // Which printed identifier reaches which account decides which account a row
    // lands on: the owner's judgement, by the rule that keeps account creation
    // out of the agent's hands.
    require_admin(&principal)?;
    let account = AccountId(id);
    owned_account(&state, &principal, account).await?;

    let aliases = alias_views(request.aliases)?;
    state
        .services
        .store
        .replace_account_aliases(principal.owner, account, aliases)
        .await?;

    let stored = state
        .services
        .store
        .list_account_details(principal.owner)
        .await?
        .into_iter()
        .find(|held| held.id == account)
        .ok_or_else(|| {
            ApiFailure::new(
                StatusCode::NOT_FOUND,
                ApiError::simple("not_found", format!("not found: account {id}")),
            )
        })?;
    Ok(Json(account_dto(stored)))
}

/// State an account's declarations: its external identity, its cash class, and
/// what a negative balance on it would mean.
///
/// **The three could previously be stated only at creation.** `POST /v1/accounts`
/// is an upsert by external identity: a create repeating a known identity
/// returns the account made last time and deliberately changes nothing about it,
/// because it is idempotent rather than an update. `PUT
/// /v1/accounts/{id}/aliases` maintains the alias set and nothing else. So every
/// account the owner already had — which is every account created before
/// decision 0004 — could never acquire an identity, a class or an expectation.
///
/// Shaped like the alias route: a replacement, not a patch. What differs is that
/// these are three separate statements rather than one set, so the replacement
/// is per field and an **absent** field is left exactly as it stands. A present
/// field carries `stated`, and `stated: false` clears it — the distinction
/// `AccountTransferPartnersDto` already draws between «he says none» and «he has
/// not said».
///
/// **Two of the three change freely.** The cash class is a grouping label read
/// by one report heading, which decision 0004 §3 forbids any rule from branching
/// on; the negative-balance expectation is a warning that sets a flag beside a
/// figure the report states either way (`iaam-d41s`). Neither invalidates
/// anything recorded, so neither is guarded by anything beyond the owner's word.
///
/// **Re-pointing an identity is recorded and reported, not refused.** The
/// response's `identity_repointed` block says what the call did not do, and
/// [`AccountIdentityRepointedDto`] records why a refusal is not available here:
/// the journal does not know which identity a fact arrived under.
#[utoipa::path(
    put,
    path = "/v1/accounts/{id}/declarations",
    operation_id = REPLACE_ACCOUNT_DECLARATIONS_OPERATION_ID,
    params(("id" = Uuid, Path, description = "Account identifier")),
    request_body = ReplaceAccountDeclarationsRequest,
    responses(
        (status = 200, description = "Declarations recorded", body = AccountDeclarationsDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "Account does not exist or belongs to someone else", body = ApiError),
        (status = 409, description = "Another of the owner's accounts already answers to that identity", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn replace_account_declarations(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<ReplaceAccountDeclarationsRequest>,
) -> Result<Json<AccountDeclarationsDto>, ApiFailure> {
    // Which source an account answers to decides which account a row lands on,
    // by the same rule that keeps account creation and aliases out of the
    // agent's hands. The class and the expectation are the owner's word about
    // his own money, and there is nobody else to take it from.
    require_admin(&principal)?;
    let account = AccountId(id);
    owned_account(&state, &principal, account).await?;

    let declarations = AccountDeclarations {
        identity: identity_statement(request.identity)?,
        cash_class: statement(
            request.cash_class,
            |stated| stated.class,
            "cash_class.class",
        )?
        .map_stated(CashAssetClassDto::to_domain),
        negative_balance_expectation: statement(
            request.negative_balance_expectation,
            |stated| stated.expectation,
            "negative_balance_expectation.expectation",
        )?
        .map_stated(NegativeBalanceExpectationDto::to_domain),
    };

    let recorded = state
        .services
        .store
        .replace_account_declarations(principal.owner, account, declarations)
        .await?;

    let identity_repointed = match recorded.previous_identity {
        None => None,
        Some(previous) => {
            // Asked only when an identity was displaced: the answer is about the
            // account, not about the identity, and it is the most the journal
            // can say.
            let facts_recorded = state
                .services
                .store
                .list_account_activity(principal.owner)
                .await?
                .into_iter()
                .find(|activity| activity.account == account)
                .is_some_and(|activity| activity.has_business_fact);
            Some(AccountIdentityRepointedDto {
                previous: AccountIdentityStatedDto {
                    provider: previous.provider,
                    provider_account_id: previous.provider_account_id,
                },
                facts_recorded,
                not_done: identity_repointed_not_done(),
            })
        }
    };

    Ok(Json(AccountDeclarationsDto {
        account: account_dto(recorded.account),
        identity_repointed,
    }))
}

/// What re-pointing an identity did not do.
///
/// A constant register, in the shape `CaveatDto` uses and for its reason: each
/// entry names one thing, and its `detail` interpolates nothing, so this block
/// can never contradict the rest of the response by restating a value wrongly.
fn identity_repointed_not_done() -> Vec<AccountIdentityNotDoneDto> {
    [
        (
            "facts_not_moved",
            "The facts already recorded against this account stay on it. \
             Re-pointing the identity changes which account a later import \
             addresses; it moves nothing already journalled.",
        ),
        (
            "previous_identity_not_reserved",
            "The identity this account has stopped answering to is now free. A \
             create carrying it mints a new account, and this account's earlier \
             facts do not follow it there.",
        ),
        (
            "no_fact_records_the_identity_it_arrived_under",
            "The journal records a fact against an account, never against the \
             identity in force at the time. Neither you nor iaam can now tell \
             which of this account's facts arrived under the previous identity.",
        ),
    ]
    .into_iter()
    .map(|(kind, detail)| AccountIdentityNotDoneDto {
        kind: kind.to_owned(),
        detail: detail.to_owned(),
    })
    .collect()
}

/// One declaration from the wire, with the one check the `stated` flag carries.
///
/// A statement is refused when it disagrees with itself: `stated: true` with
/// nothing stated says two things, and so does `stated: false` beside a value.
/// Storing either would record a statement the owner did not make.
fn statement<S, T>(
    statement: Option<S>,
    value: impl FnOnce(S) -> Option<T>,
    field: &str,
) -> Result<Declared<T>, ApiFailure>
where
    S: HasStated,
{
    let Some(statement) = statement else {
        return Ok(Declared::Untouched);
    };
    let stated = statement.stated();
    match (stated, value(statement)) {
        (true, Some(value)) => Ok(Declared::Stated(value)),
        (false, None) => Ok(Declared::Cleared),
        (true, None) => Err(unprocessable(
            field,
            "a value, because stated is true",
            "nothing",
            "a declaration stated without a value says two things at once; omit \
             the whole field to leave it alone, or send stated: false to \
             withdraw it",
        )),
        (false, Some(_)) => Err(unprocessable(
            field,
            "nothing, because stated is false",
            "a value",
            "stated: false withdraws the declaration; sending a value beside it \
             says two things at once",
        )),
    }
}

/// The `stated` flag, so [`statement`] can read it off any of the three.
trait HasStated {
    fn stated(&self) -> bool;
}

impl HasStated for AccountCashClassStatementDto {
    fn stated(&self) -> bool {
        self.stated
    }
}

impl HasStated for AccountNegativeBalanceExpectationStatementDto {
    fn stated(&self) -> bool {
        self.stated
    }
}

/// The identity statement from the wire.
///
/// Not routed through [`statement`], because this value has two halves and the
/// pair is what makes it an identity. Half of one is refused in the words the
/// create route already uses: it is a check on the shape of the pair and never
/// on the value, and the refusal does not echo `provider_account_id` back.
fn identity_statement(
    statement: Option<AccountIdentityStatementDto>,
) -> Result<Declared<AccountIdentityView>, ApiFailure> {
    let Some(statement) = statement else {
        return Ok(Declared::Untouched);
    };
    match (
        statement.stated,
        statement.provider,
        statement.provider_account_id,
    ) {
        (true, Some(provider), Some(provider_account_id)) => {
            Ok(Declared::Stated(AccountIdentityView {
                provider,
                provider_account_id,
            }))
        }
        (false, None, None) => Ok(Declared::Cleared),
        (true, Some(_), None) => Err(unprocessable(
            "identity.provider_account_id",
            "both halves of the external identity",
            "provider alone",
            "an account identified at a source is identified by the pair: a \
             statement naming only the source would be stored as naming no \
             identity, and the next import would mint a second account",
        )),
        (true, None, _) => Err(unprocessable(
            "identity.provider",
            "both halves of the external identity",
            "provider_account_id alone, or neither",
            "an identifier without the source that printed it has no scope: two \
             sources printing short sequential identifiers would collide on \
             values neither of them controls",
        )),
        (false, _, _) => Err(unprocessable(
            "identity",
            "nothing beside stated: false",
            "a half of an identity",
            "stated: false withdraws the identity; sending a half of one beside \
             it says two things at once",
        )),
    }
}

/// Aliases from the wire, with the one check the dates carry.
///
/// The value is never inspected: it is opaque for the same reason
/// `provider_account_id` is. Only the interval around it is checked, and only
/// for being an interval at all.
fn alias_views(aliases: Vec<AccountAliasDto>) -> Result<Vec<AccountAliasView>, ApiFailure> {
    let mut views = Vec::with_capacity(aliases.len());
    for alias in aliases {
        // The interval is half-open, so an end on the start day covers nothing.
        if alias.valid_to.is_some_and(|end| end <= alias.valid_from) {
            return Err(unprocessable(
                "aliases.valid_to",
                "a date after valid_from, or nothing for an open interval",
                "a date on or before valid_from",
                "the interval is half-open, so an alias that ends on or before \
                 the day it begins is valid on no day at all",
            ));
        }
        views.push(AccountAliasView {
            value: alias.value,
            valid_from: alias.valid_from,
            valid_to: alias.valid_to,
        });
    }
    Ok(views)
}

/// One account on the wire.
fn account_dto(account: AccountDetailView) -> AccountDto {
    AccountDto {
        id: account.id.inner(),
        title: account.title,
        institution: account.institution,
        provider: account.provider,
        provider_account_id: account.provider_account_id,
        cash_class: account.cash_class.map(CashAssetClassDto::from_domain),
        negative_balance_expectation: account
            .negative_balance_expectation
            .map(NegativeBalanceExpectationDto::from_domain),
        aliases: account
            .aliases
            .into_iter()
            .map(|alias| AccountAliasDto {
                value: alias.value,
                valid_from: alias.valid_from,
                valid_to: alias.valid_to,
            })
            .collect(),
    }
}

/// An account's scope disposition.
#[utoipa::path(
    get,
    path = "/v1/accounts/{id}/scope",
    params(("id" = Uuid, Path, description = "Account identifier")),
    responses(
        (status = 200, description = "The account's disposition", body = AccountScopeDto),
        (status = 404, description = "Account does not exist or belongs to someone else", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn get_account_scope(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AccountScopeDto>, ApiFailure> {
    let account = owned_account(&state, &principal, AccountId(id)).await?;
    Ok(Json(account_scope_dto(&state, &principal, &account).await?))
}

/// Record the owner's decision about an account's place in the perimeter.
///
/// The route that makes the third state resolvable. Without it the queue could
/// name an account belonging to no contour and the owner had exactly one way to
/// silence it — put the account inside a contour — which is the answer he may
/// not have.
#[utoipa::path(
    post,
    path = "/v1/accounts/{id}/scope",
    operation_id = RECORD_ACCOUNT_SCOPE_OPERATION_ID,
    params(("id" = Uuid, Path, description = "Account identifier")),
    request_body = RecordAccountScopeRequest,
    responses(
        (status = 200, description = "Disposition recorded", body = AccountScopeDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 404, description = "Account does not exist or belongs to someone else", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn record_account_scope(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<RecordAccountScopeRequest>,
) -> Result<Json<AccountScopeDto>, ApiFailure> {
    // Drawing the perimeter is the owner's judgement, in either direction: the
    // same rule that keeps contour composition out of the agent's hands.
    require_admin(&principal)?;
    let account = AccountId(id);
    let named = owned_account(&state, &principal, account).await?;

    match request.disposition {
        AccountScopeDispositionDto::Inside => {
            return Err(unprocessable(
                "disposition",
                "outside or undecided",
                "inside",
                "membership is a contour's composition and is recorded by creating                  a contour version, not by a flag on the account",
            ));
        }
        AccountScopeDispositionDto::Outside => {
            let reason = request
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .ok_or_else(|| {
                    unprocessable(
                        "reason",
                        "a non-empty reason",
                        "nothing",
                        "an account ruled outside the perimeter without a reason is                          indistinguishable, a year later, from one that was overlooked",
                    )
                })?;
            state
                .services
                .store
                .record_account_scope_exclusion(
                    principal.owner,
                    AccountScopeExclusionView {
                        account,
                        reason: reason.to_owned(),
                    },
                )
                .await?;
        }
        AccountScopeDispositionDto::Undecided => {
            if request.reason.is_some() {
                return Err(unprocessable(
                    "reason",
                    "no reason",
                    "a reason",
                    "withdrawing a decision leaves nothing for a reason to explain",
                ));
            }
            state
                .services
                .store
                .clear_account_scope_exclusion(principal.owner, account)
                .await?;
        }
    }

    Ok(Json(account_scope_dto(&state, &principal, &named).await?))
}

/// An account's stated transfer partners.
#[utoipa::path(
    get,
    path = "/v1/accounts/{id}/transfer-partners",
    params(("id" = Uuid, Path, description = "Account identifier")),
    responses(
        (status = 200, description = "The accounts money moves between this one and", body = AccountTransferPartnersDto),
        (status = 404, description = "Account does not exist or belongs to someone else", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn get_account_transfer_partners(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AccountTransferPartnersDto>, ApiFailure> {
    let account = AccountId(id);
    owned_account(&state, &principal, account).await?;
    Ok(Json(
        account_transfer_partners_dto(&state, &principal, account).await?,
    ))
}

/// Record which of the owner's accounts money moves between this one and.
///
/// The route that makes the discovery item answerable. One transfer between two
/// institutions is printed twice, once by each side, and nothing in the rows
/// relates the two legs; the system may not decide the relationship for him,
/// because a relationship inferred from two amounts that happen to match is a
/// fabricated fact about his money. So he states it, and he states it before
/// the import rather than after, which is the order the queue now asks in.
#[utoipa::path(
    put,
    path = "/v1/accounts/{id}/transfer-partners",
    operation_id = RECORD_ACCOUNT_TRANSFER_PARTNERS_OPERATION_ID,
    params(("id" = Uuid, Path, description = "Account identifier")),
    request_body = RecordAccountTransferPartnersRequest,
    responses(
        (status = 200, description = "Statement recorded", body = AccountTransferPartnersDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 404, description = "Account does not exist or belongs to someone else", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn record_account_transfer_partners(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<RecordAccountTransferPartnersRequest>,
) -> Result<Json<AccountTransferPartnersDto>, ApiFailure> {
    // Saying which two accounts are the two sides of one movement is the
    // owner's judgement, by the same rule that keeps the contour composition
    // out of the agent's hands.
    require_admin(&principal)?;
    let account = AccountId(id);
    let statement =
        validated_transfer_statement(&state, &principal, account, request.partners, "partners")
            .await?;

    state
        .services
        .store
        .record_account_transfer_statement(principal.owner, statement)
        .await?;

    Ok(Json(
        account_transfer_partners_dto(&state, &principal, account).await?,
    ))
}

/// Record those statements for several accounts in one call.
///
/// Transport only, and the shape says so. The batch is one entry per account,
/// each carrying that account's whole enumeration, because the relation is not
/// what is being recorded — the closure is. Naming `B` inside `A`'s list
/// establishes that money moves between them and says nothing about whether `B`
/// also moves money with `C`; closing `B`'s question from `A`'s answer would
/// assert that `B` has no partners beyond those already named, which is a
/// fabricated fact about the owner's money. The completeness statement is
/// irreducible per account, so the count of statements cannot shrink. The count
/// of round trips can, and this is that.
///
/// Every check the single-account route makes is made here, by calling the same
/// function it calls, once per entry. A partial failure refuses everything: the
/// route the owner would otherwise have called twelve times refuses one bad call
/// outright, and a batch that wrote eleven and refused the twelfth would leave
/// him having said something he was saying all at once.
#[utoipa::path(
    put,
    path = "/v1/accounts/transfer-partners",
    operation_id = RECORD_ACCOUNT_TRANSFER_PARTNERS_BATCH_OPERATION_ID,
    request_body = RecordAccountTransferPartnersBatchRequest,
    responses(
        (status = 200, description = "Every statement recorded", body = AccountTransferPartnersBatchDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 404, description = "An account named in the batch does not exist or belongs to someone else; nothing was recorded", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn record_account_transfer_partners_batch(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<RecordAccountTransferPartnersBatchRequest>,
) -> Result<Json<AccountTransferPartnersBatchDto>, ApiFailure> {
    require_admin(&principal)?;

    let mut accounts: Vec<AccountId> = Vec::with_capacity(request.statements.len());
    let mut statements = Vec::with_capacity(request.statements.len());
    for (index, entry) in request.statements.into_iter().enumerate() {
        let account = AccountId(entry.account);
        // Two enumerations for one account cannot both be the complete one, and
        // last-write-wins would discard a statement the owner made without
        // telling him which.
        if accounts.contains(&account) {
            return Err(unprocessable(
                &format!("/statements/{index}/account"),
                "an account named at most once in the batch",
                &format!("a second statement for account {}", account.inner()),
                "an account's transfer partners are one complete enumeration, and a batch naming \
                 the same account twice does not say which of the two is it",
            ));
        }
        accounts.push(account);
        statements.push(
            validated_transfer_statement(
                &state,
                &principal,
                account,
                entry.partners,
                &format!("/statements/{index}/partners"),
            )
            .await?,
        );
    }

    // Nothing is written until every entry has passed, and then all of it is
    // written in one transaction: validation refuses a bad batch before the
    // first row, and the store's batch method keeps a failure below this layer
    // from leaving half the statements behind.
    state
        .services
        .store
        .record_account_transfer_statements(principal.owner, statements)
        .await?;

    let mut recorded = Vec::with_capacity(accounts.len());
    for account in accounts {
        recorded.push(account_transfer_partners_dto(&state, &principal, account).await?);
    }
    Ok(Json(AccountTransferPartnersBatchDto {
        statements: recorded,
    }))
}

/// The checks that stand between a request and a recorded transfer statement.
///
/// One function, called by the single-account route and once per entry by the
/// batch, so that the two cannot drift into accepting different things. The
/// `field` is the only thing that varies: it names `partners` for the single
/// form and the JSON pointer of the offending entry for the batch, and it
/// changes what the refusal points at, never what is refused.
///
/// An account the owner does not hold — in either position — leaves through
/// [`owned_account`]'s `404`, unchanged by the batching. A batch is a cheaper
/// way to make the same twelve calls, and a caller must be able to read the
/// answer the same way whichever way it made them.
async fn validated_transfer_statement(
    state: &ServerState,
    principal: &Principal,
    account: AccountId,
    named: Vec<Uuid>,
    field: &str,
) -> Result<AccountTransferStatementView, ApiFailure> {
    owned_account(state, principal, account).await?;

    let mut partners = Vec::with_capacity(named.len());
    for partner in named {
        let partner = AccountId(partner);
        if partner == account {
            return Err(unprocessable(
                field,
                "the owner's other accounts",
                "the account itself",
                "a transfer has two sides, and an account is not the other side of itself",
            ));
        }
        // Refused rather than ignored: a named account that does not exist is a
        // mistake in the statement, and silently dropping it would record a
        // statement the owner did not make.
        owned_account(state, principal, partner).await?;
        if !partners.contains(&partner) {
            partners.push(partner);
        }
    }

    Ok(AccountTransferStatementView { account, partners })
}

/// Withdraw the statement, returning the account to awaiting a decision.
#[utoipa::path(
    delete,
    path = "/v1/accounts/{id}/transfer-partners",
    params(("id" = Uuid, Path, description = "Account identifier")),
    responses(
        (status = 200, description = "Statement withdrawn", body = AccountTransferPartnersDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 404, description = "Account does not exist or belongs to someone else", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn clear_account_transfer_partners(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AccountTransferPartnersDto>, ApiFailure> {
    require_admin(&principal)?;
    let account = AccountId(id);
    owned_account(&state, &principal, account).await?;
    state
        .services
        .store
        .clear_account_transfer_statement(principal.owner, account)
        .await?;
    Ok(Json(
        account_transfer_partners_dto(&state, &principal, account).await?,
    ))
}

/// Read the statement back, distinguishing «none» from «not said».
async fn account_transfer_partners_dto(
    state: &ServerState,
    principal: &Principal,
    account: AccountId,
) -> Result<AccountTransferPartnersDto, ApiFailure> {
    let statements = state
        .services
        .store
        .list_account_transfer_statements(principal.owner)
        .await?;
    let stated = statements
        .iter()
        .find(|statement| statement.account == account);
    Ok(AccountTransferPartnersDto {
        account: account.inner(),
        stated: stated.is_some(),
        partners: stated
            .map(|statement| {
                statement
                    .partners
                    .iter()
                    .map(|partner| partner.inner())
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Refuse an account identifier the owner does not hold.
///
/// A missing account and someone else's return the same `404` for the reason
/// token revocation does: a different answer would tell an outsider that such a
/// record exists (§14).
async fn owned_account(
    state: &ServerState,
    principal: &Principal,
    account: AccountId,
) -> Result<AccountView, ApiFailure> {
    let accounts = state.services.store.list_accounts(principal.owner).await?;
    accounts
        .into_iter()
        .find(|held| held.id == account)
        .ok_or_else(|| {
            ApiFailure::new(
                StatusCode::NOT_FOUND,
                ApiError::simple(
                    "not_found",
                    format!("not found: account {}", account.inner()),
                ),
            )
        })
}

/// Read the disposition back from the two places that hold one.
async fn account_scope_dto(
    state: &ServerState,
    principal: &Principal,
    account: &AccountView,
) -> Result<AccountScopeDto, ApiFailure> {
    let id = account.id;
    let contours = state.services.store.list_contours(principal.owner).await?;
    let exclusions = state
        .services
        .store
        .list_account_scope_exclusions(principal.owner)
        .await?;
    let naming: Vec<Uuid> = contours
        .iter()
        .filter(|contour| contour.accounts.contains(&id))
        .map(|contour| contour.id.0)
        .collect();
    let (disposition, reason) = match account_scope(id, &contours, &exclusions) {
        AccountScope::Inside => (AccountScopeDispositionDto::Inside, None),
        AccountScope::Outside => (
            AccountScopeDispositionDto::Outside,
            exclusions
                .iter()
                .find(|exclusion| exclusion.account == id)
                .map(|exclusion| exclusion.reason.clone()),
        ),
        AccountScope::Undecided => (AccountScopeDispositionDto::Undecided, None),
    };
    Ok(AccountScopeDto {
        account: id.inner(),
        title: account.title.clone(),
        institution: account.institution.clone(),
        disposition,
        reason,
        contours: naming,
    })
}

fn unprocessable(field: &str, expected: &str, actual: &str, message: &str) -> ApiFailure {
    ApiFailure::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::simple("invalid_request", message)
            .about(field)
            .expecting(expected)
            .receiving(actual),
    )
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
                ApiError::simple(
                    "invalid_request",
                    "an owner token cannot be issued via the API: the owner is created with \
                     `iaam claim --label <label>`",
                )
                .about("scope")
                .expecting("agent or read_only")
                .receiving("owner")
                // The two scopes a token may be issued with, as values: the
                // sentence beside them says the same thing, and a caller
                // retrying should not have to split it on the word "or".
                .admitting(vec![
                    InputAlternativeDto {
                        value: "agent".to_owned(),
                        requires: Vec::new(),
                    },
                    InputAlternativeDto {
                        value: "read_only".to_owned(),
                        requires: Vec::new(),
                    },
                ]),
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

/// Create a contour.
///
/// This route creates a contour and nothing else. It used to do two jobs — create
/// one, and add a version to one — chosen by whether the body carried a `contour`
/// field, so the destructive reading was the one an omission gave you. An agent
/// that had drawn a perimeter for one bank called it again for the second and was
/// given a second perimeter holding that bank alone: every operation recorded,
/// every verdict positive, and the report over the newer contour showing one
/// bank. Adding a version is now `POST /v1/contours/{contour}/versions`, which
/// cannot create anything because it has nothing to create from.
///
/// Repeating the call with the same intent — the same title and the same
/// composition — is a replay of that intent and not a second perimeter: it
/// answers `200` with `created: false` and the contour that already says it. An
/// agent retrying a call it is not sure landed is the ordinary case, and a
/// perimeter is not a thing to acquire twice by accident.
#[utoipa::path(
    post,
    path = "/v1/contours",
    operation_id = CREATE_CONTOUR_VERSION_OPERATION_ID,
    request_body = CreateContourVersionRequest,
    responses(
        (status = 201, description = "Contour created", body = ContourVersionDto),
        (status = 200, description = "The same intent was already recorded; nothing was written", body = ContourVersionDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read, or it named a contour", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_contour_version(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CreateContourVersionRequest>,
) -> Result<(StatusCode, Json<ContourVersionDto>), ApiFailure> {
    require_admin(&principal)?;
    // Refused rather than ignored. Ignoring it would leave every client still
    // sending the field creating perimeters it did not ask for, and saying
    // nothing — which is the defect, only quieter.
    if request.contour.is_some() {
        return Err(unprocessable(
            "contour",
            "no contour identifier",
            "a contour identifier",
            "this route creates a contour; to add a version to one that exists, \
             call POST /v1/contours/{contour}/versions",
        ));
    }
    let accounts = bounded_composition(&request.accounts)?;

    // A replay of the same intent, not a second perimeter.
    let existing = state.services.store.list_contours(principal.owner).await?;
    if let Some(already) = existing
        .iter()
        .find(|contour| contour.title == request.title && same_composition(contour, &accounts))
    {
        return Ok((StatusCode::OK, Json(contour_version_dto(already, false))));
    }

    let contour = ContourId(Uuid::new_v4());
    let version = ContourVersion(1);
    state
        .services
        .store
        .insert_contour_version(
            principal.owner,
            ContourDefinition::new(contour, version, accounts.clone()),
            request.title.clone(),
            accounts.clone(),
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ContourVersionDto {
            contour: contour.0,
            version: version.0,
            title: request.title,
            accounts: accounts.iter().map(|id| id.inner()).collect(),
            created: true,
        }),
    ))
}

/// Add a version to a contour that exists.
///
/// The contour is named by the path, so there is no field whose absence could be
/// read as «make me a new one» — which is the whole point of the split. This is
/// the act an owner wants when a second bank's account has to come inside the
/// perimeter he already has, and until it existed the only route the API offered
/// him created a second one.
///
/// The composition is written whole. Sending only the account being added would
/// drop every existing member, so the request carries the membership the contour
/// is to have from this version onwards.
#[utoipa::path(
    post,
    path = "/v1/contours/{contour}/versions",
    operation_id = ADD_CONTOUR_VERSION_OPERATION_ID,
    params(("contour" = Uuid, Path, description = "Contour identifier")),
    request_body = AddContourVersionRequest,
    responses(
        (status = 201, description = "Version created", body = ContourVersionDto),
        (status = 200, description = "The contour already holds this composition; nothing was written", body = ContourVersionDto),
        (status = 403, description = "Insufficient privileges", body = ApiError),
        (status = 404, description = "Contour does not exist or belongs to someone else", body = ApiError),
        (status = 409, description = "The contour moved past the version the caller named", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn add_contour_version(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<AddContourVersionRequest>,
) -> Result<(StatusCode, Json<ContourVersionDto>), ApiFailure> {
    require_admin(&principal)?;
    let current = owned_contour(&state, &principal, ContourId(id)).await?;

    // The precondition, checked before anything is read out of the body: a
    // caller that reasoned from a version someone has since replaced would not
    // merge with that writer, it would discard them, and it cannot tell from the
    // result which happened.
    if let Some(expected) = request.expected_version
        && expected != current.version.0
    {
        return Err(ApiFailure::new(
            StatusCode::CONFLICT,
            ApiError::simple(
                "version_conflict",
                "the contour has moved on since the version this request names; read it back \
                 and decide the composition again",
            )
            .about("expected_version")
            .expecting(current.version.0.to_string())
            .receiving(expected.to_string()),
        ));
    }

    let accounts = bounded_composition(&request.accounts)?;
    // The title the contour already carries, unless the caller renames it. The
    // owner is asked for the judgement, never for a name he has already given.
    let title = request.title.unwrap_or_else(|| current.title.clone());

    // Nothing to record. An identical version is not history, it is noise, and a
    // retried call must not push the version the owner's reports cite forward.
    if title == current.title && same_composition(&current, &accounts) {
        return Ok((StatusCode::OK, Json(contour_version_dto(&current, false))));
    }

    let version = ContourVersion(current.version.0.saturating_add(1));
    state
        .services
        .store
        .insert_contour_version(
            principal.owner,
            ContourDefinition::new(current.id, version, accounts.clone()),
            title.clone(),
            accounts.clone(),
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ContourVersionDto {
            contour: current.id.0,
            version: version.0,
            title,
            accounts: accounts.iter().map(|id| id.inner()).collect(),
            // Versioning creates no contour. The field is not decoration here:
            // it is what tells a caller which of the two acts it performed.
            created: false,
        }),
    ))
}

/// The owner's contours, each at its current version.
///
/// The composition was write-only over HTTP, so an import skill had to be handed
/// the perimeter as run-time input and had no way to check it against what the
/// system believes. This is a view of the same composition the write routes
/// build — derived from it, never a second copy.
#[utoipa::path(
    get,
    path = "/v1/contours",
    operation_id = "list_contours",
    responses(
        (status = 200, description = "Owner's contours", body = Vec<ContourDto>),
        (status = 401, description = "Authentication required", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_contours(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<ContourDto>>, ApiFailure> {
    let contours = state.services.store.list_contours(principal.owner).await?;
    Ok(Json(contours.iter().map(contour_dto).collect()))
}

/// One contour, with the composition its current version names.
#[utoipa::path(
    get,
    path = "/v1/contours/{contour}",
    operation_id = "get_contour",
    params(("contour" = Uuid, Path, description = "Contour identifier")),
    responses(
        (status = 200, description = "The contour and its composition", body = ContourDto),
        (status = 404, description = "Contour does not exist or belongs to someone else", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn get_contour(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<ContourDto>, ApiFailure> {
    let contour = owned_contour(&state, &principal, ContourId(id)).await?;
    Ok(Json(contour_dto(&contour)))
}

/// Refuse a contour identifier the owner does not hold.
///
/// A missing contour and someone else's return the same `404`, for the reason
/// `owned_account` does: a different answer would tell an outsider that such a
/// record exists (§14).
async fn owned_contour(
    state: &ServerState,
    principal: &Principal,
    contour: ContourId,
) -> Result<ContourView, ApiFailure> {
    let contours = state.services.store.list_contours(principal.owner).await?;
    contours
        .into_iter()
        .find(|held| held.id == contour)
        .ok_or_else(|| {
            ApiFailure::new(
                StatusCode::NOT_FOUND,
                ApiError::simple("not_found", format!("not found: contour {}", contour.0)),
            )
        })
}

/// The membership a contour version may be given.
///
/// A contour with no accounts has no boundary, and a report over one would be a
/// confident answer about nothing.
fn bounded_composition(accounts: &[Uuid]) -> Result<Vec<AccountId>, ApiFailure> {
    if accounts.is_empty() {
        return Err(ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::simple(
                "invalid_request",
                "a contour with no accounts has no boundary",
            )
            .about("accounts")
            .expecting("at least one account")
            .receiving("empty list"),
        ));
    }
    Ok(accounts.iter().copied().map(AccountId).collect())
}

/// Whether a contour already covers exactly this set of accounts.
///
/// Compared as a set: a composition is which accounts are inside, and neither
/// the order they were listed in nor a repetition changes that.
fn same_composition(contour: &ContourView, accounts: &[AccountId]) -> bool {
    let held: BTreeSet<AccountId> = contour.accounts.iter().copied().collect();
    let asked: BTreeSet<AccountId> = accounts.iter().copied().collect();
    held == asked
}

fn contour_dto(contour: &ContourView) -> ContourDto {
    ContourDto {
        contour: contour.id.0,
        title: contour.title.clone(),
        version: contour.version.0,
        accounts: contour.accounts.iter().map(|id| id.inner()).collect(),
    }
}

/// The answer for a call that wrote nothing: the contour as it already stands.
fn contour_version_dto(contour: &ContourView, created: bool) -> ContourVersionDto {
    ContourVersionDto {
        contour: contour.id.0,
        version: contour.version.0,
        title: contour.title.clone(),
        accounts: contour.accounts.iter().map(|id| id.inner()).collect(),
        created,
    }
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
    let declared = match &request.source {
        Some(declared) => Some((
            declared,
            declared_account(&state, &principal, declared).await?.id,
        )),
        None => None,
    };
    let (source, import) = match declared {
        Some((declared, account)) => (
            declared_source(principal.owner, account, declared)?,
            declared_import(principal.owner, account, declared)?,
        ),
        // No declaration: today's behaviour, so existing callers keep working.
        None => (SourceId::new_random(), None),
    };

    // The declaration says whose rows these are, and the rows must say the
    // same. This is checked before anything is written and refuses the whole
    // batch rather than the row, unlike an unreadable operation: an
    // unreadable row is one row the caller got wrong, while a row for another
    // account contradicts the declaration the caller made over all of them,
    // and writing the agreeing half would leave a half-import recorded under
    // an identity that names the wrong account.
    //
    // The comparison is against the account the declaration **resolved** to,
    // not against the text it was written as: a caller may declare the batch by
    // the number its bank prints, and the rows still name the account by its
    // iaam identifier, which is the one thing both sides can agree on.
    if let Some((_, account)) = declared {
        for (index, operation) in request.operations.iter().enumerate() {
            if operation.account != account.inner() {
                return Err(invalid_field(
                    format!("operations[{index}].account"),
                    &account.inner().to_string(),
                    operation.account.to_string(),
                ));
            }
        }
    }

    // Parsing the DTO yields a verdict for each row: one unrecognised operation
    // does not invalidate the others (§10.1).
    //
    // `to_intake` rather than `to_domain`, and that is the whole change on this
    // route: a caller that concluded still sends a conclusion and still gets the
    // verdict it always got, while a caller that did not is no longer forced to
    // invent one.
    let mut verdicts: Vec<VerdictDto> = Vec::with_capacity(request.operations.len());
    let mut accepted: Vec<(usize, Intake)> = Vec::new();
    for (index, operation) in request.operations.iter().enumerate() {
        match operation.to_intake() {
            Ok(intake) => accepted.push((index + 1, intake)),
            Err(rejection) => verdicts.push(VerdictDto::from_domain(
                index + 1,
                &Verdict::Rejected { rejection },
            )),
        }
    }

    let domain: Vec<Intake> = accepted.iter().map(|(_, intake)| intake.clone()).collect();
    let outcomes = submit_intake(&state.services, &principal, source, import, &domain).await?;
    for ((row, _), outcome) in accepted.iter().zip(outcomes.iter()) {
        verdicts.push(intake_verdict_dto(*row, outcome));
    }
    verdicts.sort_by_key(|verdict| verdict.row);
    Ok(Json(verdicts))
}

/// A verdict, plus the identifiers of the question when the row raised one.
///
/// The sentence lives in the published verdict; the identifiers do not, and
/// without them the caller has a question it cannot answer. This is where the
/// two are put back together.
fn intake_verdict_dto(row: usize, outcome: &IntakeOutcome) -> VerdictDto {
    let base = VerdictDto::from_domain(row, &outcome.verdict);
    let Some(asked) = &outcome.asked else {
        return base;
    };
    VerdictDto {
        session_id: Some(asked.session.inner()),
        question_id: Some(asked.question.inner()),
        alternatives: Some(
            asked
                .alternatives
                .iter()
                .copied()
                .map(AnswerAlternativeDto::from_domain)
                .collect(),
        ),
        ..base
    }
}

// ---------------------------------------------------------------------------
// Import sessions (iaam-3kru)
// ---------------------------------------------------------------------------

/// Open an import session.
///
/// The declared account may be named by its iaam identifier or by the identifier
/// its source prints for it — its `provider_account_id`, or one of its aliases,
/// a card among them. An identifier two accounts answer to is refused rather
/// than guessed at, and the refusal names both. The response carries the account
/// the declaration resolved to, and that identifier is the one the rows of this
/// session must name.
///
/// Refused when the declared import already has a session open **and that
/// session holds rows**: it is a statement half imported, and only the caller
/// knows whether the file in its hand is that statement or another one. The
/// refusal names the session, says how long it has been open and what it holds,
/// and publishes the two calls that end it. A session found holding nothing is
/// handed back as before — that is a caller retrying the open call, and there is
/// nothing in an empty session to mix a second statement into.
#[utoipa::path(
    post,
    path = "/v1/import-sessions",
    request_body = OpenImportSessionRequest,
    responses(
        (status = 201, description = "Session opened, or the empty one this import already had", body = ImportSessionDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read, or this import already has a session holding rows", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn open_import_session(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiJson(request): ApiJson<OpenImportSessionRequest>,
) -> Result<(StatusCode, Json<ImportSessionDto>), ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let (account, source, import) = match &request.source {
        Some(declared) => {
            let account = declared_account(&state, &principal, declared).await?;
            (
                Some(account.clone()),
                Some(declared_source(principal.owner, account.id, declared)?),
                declared_import(principal.owner, account.id, declared)?,
            )
        }
        None => (None, None, None),
    };
    // Converted through the catalogue rather than with `?`: the refusal below
    // offers calls rather than values — no label written into this request ends
    // a session that is already open — and their addresses resolve only against
    // the completed document. `ApiFailure::from(AppError)` has no catalogue to
    // reach and would drop them.
    let session = iaam_app::scenarios::import_session::open_session(
        &state.services,
        &principal,
        source,
        import,
    )
    .await
    .map_err(|error| ApiFailure::from_app(error, &catalog))?;
    Ok((
        StatusCode::CREATED,
        Json(ImportSessionDto {
            // The account goes back with the session, and only here: this is the
            // one response that holds it, and the rows the caller is about to
            // send have to name it. Without it a caller that declared the batch
            // by the number its statement prints would have to go and read the
            // directory anyway — the very read this declaration removed.
            account: account.map(|account| DeclaredAccountDto {
                id: account.id.inner(),
                title: account.title,
                institution: account.institution,
            }),
            ..ImportSessionDto::from_domain(&session)
        }),
    ))
}

/// Every import session of the owner's, newest first.
///
/// This is what makes a question survive the response that carried it: a caller
/// that lost the response finds the session here and the question in it.
#[utoipa::path(
    get,
    path = "/v1/import-sessions",
    responses(
        (status = 200, description = "The owner's import sessions", body = Vec<ImportSessionDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_import_sessions(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<ImportSessionDto>>, ApiFailure> {
    let sessions =
        iaam_app::scenarios::import_session::list_sessions(&state.services, &principal).await?;
    Ok(Json(
        sessions.iter().map(ImportSessionDto::from_domain).collect(),
    ))
}

/// What one session holds, and what it is waiting on.
#[utoipa::path(
    get,
    path = "/v1/import-sessions/{session}",
    params(("session" = Uuid, Path, description = "Import session identifier")),
    responses(
        (status = 200, description = "The session, its rows and its questions", body = ImportSessionContentsDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "No such session, or it belongs to someone else", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn get_import_session(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<ImportSessionContentsDto>, ApiFailure> {
    let contents = iaam_app::scenarios::import_session::read_session(
        &state.services,
        &principal,
        ImportSessionId(id),
    )
    .await?;
    Ok(Json(session_contents_dto(&contents)))
}

/// Feed rows into a session.
///
/// Nothing reaches the journal here, whatever the rows say — including a row the
/// caller concluded. That is what a session is for: both legs of one transfer
/// can sit in it before either is recorded.
#[utoipa::path(
    post,
    path = "/v1/import-sessions/{session}/rows",
    params(("session" = Uuid, Path, description = "Import session identifier")),
    request_body = AddImportRowsRequest,
    responses(
        (status = 200, description = "Outcome for each row; nothing was recorded", body = Vec<ImportRowDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "No such open session", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn add_import_rows(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<AddImportRowsRequest>,
) -> Result<Json<Vec<ImportRowDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    // An unreadable row does not invalidate the others (§10.1), and it is
    // reported against its position in the request: it never reached the
    // session, so it has no position in that.
    let mut rejected: Vec<ImportRowDto> = Vec::new();
    let mut accepted: Vec<Intake> = Vec::new();
    for (index, operation) in request.operations.iter().enumerate() {
        match operation.to_intake() {
            Ok(intake) => accepted.push(intake),
            Err(rejection) => rejected.push(ImportRowDto::from_domain(&HeldRow::Rejected {
                row: u32::try_from(index + 1).unwrap_or(u32::MAX),
                rejection,
            })),
        }
    }
    let held = iaam_app::scenarios::import_session::add_rows(
        &state.services,
        &principal,
        ImportSessionId(id),
        &accepted,
    )
    .await?;
    let mut rows: Vec<ImportRowDto> = held.iter().map(ImportRowDto::from_domain).collect();
    rows.extend(rejected);
    Ok(Json(rows))
}

/// Answer one of the session's questions.
///
/// The answer is written onto the row, and — for an owner token only — as a
/// durable classification rule beside it, so the next import of a matching row
/// settles without asking. Nothing is recorded in the journal: the answer
/// settles what the row is, and commit is what records it.
///
/// **The split is `iaam-hnod`.** Settling one row is import mechanics and
/// belongs to whoever is running the import. Generalising that settlement into a
/// standing rule decides rows nobody has looked at yet, which is the same act
/// `POST /v1/classification-rules` performs under an owner-only gate — so an
/// agent that could do it here would be making the decision through a route
/// whose name does not mention rules. Under an agent token the row settles and
/// `rule` comes back absent; the owner turns the answer into a rule with his own
/// token if he wants it to stand.
///
/// An answer carrying a field its own word does not take — an `account` beside
/// `received`, an `origin` beside anything but `fee` — is refused rather than
/// applied with the extra field dropped. Sending one is the signature of a
/// caller that meant a different answer, and settling the row as the word it
/// typed would record a decision nobody made.
#[utoipa::path(
    post,
    path = "/v1/import-sessions/{session}/questions/{question}/answer",
    params(
        ("session" = Uuid, Path, description = "Import session identifier"),
        ("question" = Uuid, Path, description = "Question identifier")
    ),
    request_body = AnswerImportQuestionRequest,
    responses(
        (status = 200, description = "The answered question", body = ImportQuestionDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "No such open session or unanswered question", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "The answer is not one this question admits, or it carries a field its own word does not take", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn answer_import_question(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath((session, question)): ApiPath<(Uuid, Uuid)>,
    ApiJson(request): ApiJson<AnswerImportQuestionRequest>,
) -> Result<Json<ImportQuestionDto>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let answer = request.to_domain().map_err(|rejection| {
        invalid_field(rejection.field, &rejection.expected, rejection.actual)
    })?;
    let answered = iaam_app::scenarios::import_session::answer_question(
        &state.services,
        &principal,
        ImportSessionId(session),
        ImportQuestionId(question),
        answer,
    )
    .await?;
    Ok(Json(ImportQuestionDto::from_domain(&answered)))
}

/// What committing this session would do, before it does it (iaam-k1xa).
///
/// Computed by the code that commits, not beside it. That is the whole property:
/// a preview written as a second implementation of the import describes a
/// different import from the one that runs, and drifts from it silently — which
/// is how rows came back with positive verdicts and were absent from the report
/// the owner was shown.
///
/// The answer carries a `revision`. Send it back to the commit route and a
/// session that changed in between refuses rather than writing something else.
#[utoipa::path(
    get,
    path = "/v1/import-sessions/{session}/assessment",
    params(("session" = Uuid, Path, description = "Import session identifier")),
    responses(
        (status = 200, description = "What committing would record, and what it would not", body = ImportPlanDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "No such session, or it belongs to someone else", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn assess_import_session(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<ImportPlanDto>, ApiFailure> {
    let planned = iaam_app::scenarios::import_session::plan_session(
        &state.services,
        &principal,
        ImportSessionId(id),
    )
    .await?;
    Ok(Json(ImportPlanDto::from_domain(&planned.plan)))
}

/// Commit the session: write everything it holds, once.
///
/// Refused while any question is unanswered. That refusal is the point of the
/// session — committing with a question open records the guess the question
/// exists to prevent.
///
/// Refused, too, when the caller names a `revision` the session no longer
/// answers to: what would be written is not what the caller read.
#[utoipa::path(
    post,
    path = "/v1/import-sessions/{session}/commit",
    params(("session" = Uuid, Path, description = "Import session identifier")),
    request_body = CommitImportSessionRequest,
    responses(
        (status = 200, description = "The session, closed, and a verdict per row", body = ImportCommitDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "No such open session", body = ApiError),
        (status = 422, description = "Unanswered questions remain, or the revision no longer describes what would happen", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn commit_import_session(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJsonOrDefault(request): ApiJsonOrDefault<CommitImportSessionRequest>,
) -> Result<Json<ImportCommitDto>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    // The body is optional, and so is the revision inside it: a caller that
    // never read an assessment still commits, and is told in the answer which
    // revision it committed under. Making the body mandatory would break every
    // caller that already commits with nothing to say.
    let revision = request.revision.map(SessionRevision);
    // Converted through the catalog rather than with `?`: this is the one
    // refusal in the API whose remedy is a call — an unanswered question is not
    // settled by writing anything into the request — and the address of that
    // call is only resolvable against the completed document.
    let outcome = iaam_app::scenarios::import_session::commit_session(
        &state.services,
        &principal,
        ImportSessionId(id),
        revision.as_ref(),
        request.accept_control_mismatch,
    )
    .await
    .map_err(|error| ApiFailure::from_app(error, &catalog))?;
    let contents = iaam_app::scenarios::import_session::read_session(
        &state.services,
        &principal,
        ImportSessionId(id),
    )
    .await?;
    Ok(Json(ImportCommitDto {
        session: ImportSessionDto::from_domain(&contents.session),
        revision: outcome.revision.0,
        rows: outcome
            .verdicts
            .iter()
            .enumerate()
            .map(|(index, verdict)| VerdictDto::from_domain(index + 1, verdict))
            .collect(),
        control_assertions: outcome
            .control_assertions
            .iter()
            .map(RecordedEventDto::from_domain)
            .collect(),
    }))
}

/// State the control figures this session's source printed for itself.
///
/// A bank statement prints its own arithmetic — opening balance, closing
/// balance, and how much crossed the account each way — and until now a session
/// could not take it. The figures went in afterwards, through the owner-only
/// reconciliation route, against a journal that already held whatever the import
/// got wrong. So the one moment a mismatch was cheap was the one moment nothing
/// compared anything, while the source had printed the answer on the same page
/// as the rows.
///
/// Stating them makes the assessment compare: `control_reconciliation` puts both
/// numbers beside each other per account and currency, and a disagreement takes
/// the readiness to `does_not_reconcile`. Committing over one stays possible —
/// a statement can itself be wrong — but takes `accept_control_mismatch`.
///
/// One interval per call, because a control section belongs to the document it
/// is printed on. Restating a section replaces it: a transcription corrected is
/// a correction.
///
/// **This is not the owner stating his balance.** That is
/// `POST /v1/reconciliation/balance`, which is owner-only and writes under the
/// owner-stated parser version, capped at `accepted_internal` by §10.4. What is
/// stated here is what a document says, by whoever fed the document in — an
/// agent may do it, exactly as an agent may feed in the rows — and it is written
/// under its own parser version and its own key namespace, so neither claim can
/// supersede or deduplicate the other.
#[utoipa::path(
    post,
    path = "/v1/import-sessions/{session}/control-figures",
    params(("session" = Uuid, Path, description = "Import session identifier")),
    request_body = StateImportControlFiguresRequest,
    responses(
        (status = 200, description = "Every control section the session now holds", body = Vec<ControlSectionDto>),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "No such open session", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "A section states nothing, names an account and currency twice, gives a signed turnover, or names an inverted interval", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn state_import_control_figures(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<StateImportControlFiguresRequest>,
) -> Result<Json<Vec<ControlSectionDto>>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let period = AssertionPeriod::between(request.from, request.to).ok_or_else(|| {
        invalid_field(
            "period",
            "from no later than to",
            format!("{}..{}", request.from, request.to),
        )
    })?;
    let mut stated = Vec::with_capacity(request.figures.len());
    for (index, figures) in request.figures.iter().enumerate() {
        let currency = figures.currency.to_domain();
        // Each figure is parsed under its own pointer. A batch of four rejected
        // as «one of them is not a decimal» would send the caller to read all
        // four.
        let amount = |value: &Option<String>, field: &str| {
            value
                .as_ref()
                .map(|amount| {
                    let pointer = format!("figures[{index}].{field}");
                    let decimal = amount.parse::<Decimal>().map_err(|_| {
                        invalid_field(pointer.clone(), "decimal string", amount.clone())
                    })?;
                    iaam_app::ingest::operation::to_minor_units(decimal, currency, "amount")
                        .map(PostedMinor::new)
                        .map_err(|rejection| {
                            invalid_field(pointer, &rejection.expected, rejection.actual)
                        })
                })
                .transpose()
        };
        stated.push(ControlSection {
            account: AccountId(figures.account),
            currency,
            period,
            opening: amount(&figures.opening, "opening")?,
            closing: amount(&figures.closing, "closing")?,
            debit_turnover: amount(&figures.debit_turnover, "debit_turnover")?,
            credit_turnover: amount(&figures.credit_turnover, "credit_turnover")?,
        });
    }
    let held = iaam_app::scenarios::import_session::state_control_figures(
        &state.services,
        &principal,
        ImportSessionId(id),
        stated,
    )
    .await?;
    Ok(Json(
        held.iter().map(ControlSectionDto::from_domain).collect(),
    ))
}

// ---------------------------------------------------------------------------
// Transfer pairing (iaam-3ul2)
// ---------------------------------------------------------------------------

/// Transfers the journal's one-sided movements may be halves of.
///
/// One movement between two of the owner's banks is printed twice, once by each
/// side, and nothing in either row says the two are one thing. Recorded
/// independently they make a contour spanning both banks report an external
/// outflow and an external inflow that never happened.
///
/// **Nothing is decided here.** Each candidate is published with the evidence it
/// rests on — amount, currency, both dates, what each source printed — and the
/// owner confirms. Legs nothing paired with are published too: a leg dropped
/// from the answer is a leg read as external flow by default.
///
/// `without_counterpart` is therefore most of the journal, permanently, and is
/// not work waiting to be done: every cash movement with a posting date is
/// offered to the matcher, and a payment in a shop has no counterpart to propose
/// and never will. An empty `candidates` beside a long `without_counterpart` is
/// this route's ordinary answer, not an error.
#[utoipa::path(
    get,
    path = "/v1/transfer-pairings",
    responses(
        (status = 200, description = "Candidate pairs, and the cash movements nothing was proposed against", body = CrossSourceMatchingDto),
        (status = 403, description = "Insufficient permissions", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_transfer_pairings(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<CrossSourceMatchingDto>, ApiFailure> {
    let proposals = iaam_app::scenarios::transfer_pairing::propose_journal_pairings(
        &state.services,
        &principal,
    )
    .await?;
    Ok(Json(CrossSourceMatchingDto::from_domain(&proposals)))
}

/// Relate two recorded legs, on the owner's word.
///
/// Two correction facts, and no new kind of state: the outgoing leg is superseded
/// by one transfer carrying a leg on each account, and the incoming leg is
/// retracted because that transfer already states it. A relation kept outside the
/// journal would be a second notion of what is effective.
///
/// Refused unless the two are a pair this build proposes. Without that check the
/// route would relate any outflow to any inflow — the fabrication the proposal
/// exists to prevent, with the owner's name on it.
#[utoipa::path(
    post,
    path = "/v1/transfer-pairings",
    request_body = ConfirmTransferPairingRequest,
    responses(
        (status = 200, description = "The pairing, recorded as corrections", body = ConfirmedPairingDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "These two events are not a proposed pair", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError),
        (status = 415, description = "Body sent without Content-Type: application/json", body = ApiError),
        (status = 422, description = "The retraction was not acknowledged", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn confirm_transfer_pairing(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<ConfirmTransferPairingRequest>,
) -> Result<Json<ConfirmedPairingDto>, ApiFailure> {
    let confirmed = iaam_app::scenarios::transfer_pairing::confirm_journal_pairing(
        &state.services,
        &principal,
        EventId(request.outgoing),
        EventId(request.incoming),
        request.acknowledge_retraction,
    )
    .await?;
    Ok(Json(ConfirmedPairingDto::from_domain(&confirmed)))
}

/// Abandon the session.
///
/// The journal is neither read nor written: what the session held was never a
/// fact, so there is nothing to retract.
#[utoipa::path(
    post,
    path = "/v1/import-sessions/{session}/abandon",
    params(("session" = Uuid, Path, description = "Import session identifier")),
    responses(
        (status = 200, description = "The abandoned session; the journal is untouched", body = ImportSessionDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 404, description = "No such open session", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn abandon_import_session(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<ImportSessionDto>, ApiFailure> {
    if !principal.scope.may_submit() {
        return Err(ApiFailure::forbidden(principal.scope.code()));
    }
    let session = iaam_app::scenarios::import_session::abandon_session(
        &state.services,
        &principal,
        ImportSessionId(id),
    )
    .await?;
    Ok(Json(ImportSessionDto::from_domain(&session)))
}

fn session_contents_dto(contents: &SessionContents) -> ImportSessionContentsDto {
    ImportSessionContentsDto {
        session: ImportSessionDto::from_domain(&contents.session),
        row_count: contents.observations.len(),
        questions: contents
            .questions
            .iter()
            .map(ImportQuestionDto::from_domain)
            .collect(),
        unanswered: contents
            .questions
            .iter()
            .filter(|question| question.is_open())
            .count(),
        control_figures: contents
            .control_figures
            .iter()
            .map(ControlSectionDto::from_domain)
            .collect(),
    }
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

    // A CSV body declares no source, so it names no import either: its rows
    // arrive under a source minted for this request alone, and
    // `POST /v1/corrections/imports` cannot reach them. Declaring a source for
    // this route is a separate change to its request shape.
    let source = SourceId::new_random();
    let domain: Vec<SubmittedOperation> = accepted
        .iter()
        .map(|(_, operation)| operation.clone())
        .collect();
    let outcomes = submit_operations(&state.services, &principal, source, None, &domain).await?;
    for ((row, _), verdict) in accepted.iter().zip(outcomes.iter()) {
        verdicts.push(VerdictDto::from_domain(*row, verdict));
    }
    verdicts.sort_by_key(|verdict| verdict.row);
    Ok(Json(verdicts))
}

/// Money flow report parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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
    let outcome = money_flow(&state.services, &principal, &query).await?;
    // No scoping: the projection admits no leg from outside the contour, so the
    // report cannot name an account it does not cover. The accounts are read for
    // the names the items carry, not to narrow them.
    let accounts = state.services.store.list_accounts(principal.owner).await?;
    let actions = iaam_app::actions::flow_diagnostics(&outcome.report, &accounts)?
        .iter()
        .map(|action| action_dto(action, &catalog))
        .collect();
    let dto = MoneyFlowReportDto::from_domain(&outcome, actions, &catalog)
        .map_err(iaam_app::error::AppError::from)?;
    Ok(Json(dto))
}

/// Account balances at a date.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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
        (status = 200, description = "Cash and positions by account, and the scope's negative cash", body = BalancesReportDto),
        (status = 404, description = "Scope not found", body = ApiError),
        (status = 422, description = "Invalid report date", body = ApiError),
        (status = 500, description = "Balances could not be built", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn balances_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiQuery(params): ApiQuery<BalancesParams>,
) -> Result<Json<BalancesReportDto>, ApiFailure> {
    let as_of = parse_query_date("as_of", &params.as_of)?;
    let report = account_balances(
        &state.services,
        &principal,
        ContourId(params.contour),
        params.contour_version.map(ContourVersion),
        as_of,
    )
    .await?;
    Ok(Json(BalancesReportDto::from_domain(&report, &catalog)))
}

/// What the owner holds at a date.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AssetSnapshotParams {
    /// Scope identifier.
    pub contour: Uuid,
    /// Scope composition version. By default — the latest.
    #[serde(default)]
    pub contour_version: Option<u32>,
    /// Report date in YYYY-MM-DD format.
    pub as_of: String,
}

/// What the owner holds at a date, grouped by the class of cash he declared.
///
/// The same fold as `/v1/reports/balances`, regrouped: the rows in `accounts`
/// are that report's rows, and every total is folded from them in the core.
/// A second path to a total could disagree with the rows it summarises.
#[utoipa::path(
    get,
    path = "/v1/reports/assets",
    params(AssetSnapshotParams),
    responses(
        (status = 200, description = "Cash by class, positions at the prices the journal holds, \
                                      and the whole", body = AssetSnapshotDto),
        (status = 404, description = "Scope not found", body = ApiError),
        (status = 422, description = "Invalid report date", body = ApiError),
        (status = 500, description = "Snapshot could not be built", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn asset_snapshot_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiQuery(params): ApiQuery<AssetSnapshotParams>,
) -> Result<Json<AssetSnapshotDto>, ApiFailure> {
    let as_of = parse_query_date("as_of", &params.as_of)?;
    let snapshot = asset_snapshot(
        &state.services,
        &principal,
        ContourId(params.contour),
        params.contour_version.map(ContourVersion),
        as_of,
    )
    .await?;
    Ok(Json(AssetSnapshotDto::from_domain(&snapshot, &catalog)))
}

/// Returns report parameters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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
        (status = 200, description = "Report", body = ReturnsAnswerDto),
        (status = 404, description = "Scope not found", body = ApiError),
        (status = 500, description = "Invariant violated", body = ApiError),
        (status = 422, description = "Request could not be read", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn returns_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiQuery(params): ApiQuery<ReturnsParams>,
) -> Result<Json<ReturnsAnswerDto>, ApiFailure> {
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
    let outcome = returns(&state.services, &principal, &query).await?;
    Ok(Json(ReturnsAnswerDto::from_domain(&outcome, &catalog)))
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
        (status = 200, description = "Report using the specified exchange rates", body = ReturnsAnswerDto),
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
    Extension(catalog): Extension<Arc<ActionCatalog>>,
    ApiQuery(params): ApiQuery<ReturnsParams>,
    ApiJson(rates): ApiJson<Vec<FxRateDto>>,
) -> Result<Json<ReturnsAnswerDto>, ApiFailure> {
    let mut fx = FxTable::new(FxSource::OwnerSupplied);
    for rate in &rates {
        let parsed = rate.rate.parse::<Decimal>().map_err(|_| {
            ApiFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::simple("invalid_request", "exchange rate must be a decimal number")
                    .about("rate")
                    .expecting("decimal number represented as a string")
                    .receiving(rate.rate.clone()),
            )
        })?;
        fx = fx.with_rate(
            rate.base.to_domain(),
            rate.quote.to_domain(),
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
    let outcome = returns(&state.services, &principal, &query).await?;
    Ok(Json(ReturnsAnswerDto::from_domain(&outcome, &catalog)))
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
        base: CurrencyDto::from_domain(view.base),
        quote: CurrencyDto::from_domain(view.quote),
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
#[into_params(parameter_in = Query)]
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
    /// The import session whose commit wrote the rows. Narrower than the
    /// declared source, which covers every import that came through one
    /// channel: this names one act of importing, and it is the identifier
    /// `POST /v1/import-sessions` returned and every row here carries back.
    #[serde(default)]
    pub import_session: Option<Uuid>,
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
            import_session: params.import_session.map(ImportSessionId),
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
        // No `receiving`: the field is absent, and quoting the companion's
        // value instead would name one field and echo another.
        ApiError::simple(
            "invalid_request",
            format!("required query parameter {field} is missing"),
        )
        .about(field)
        .expecting(expected),
    )
}

fn parse_query_date(field: &'static str, value: &str) -> Result<Date, ApiFailure> {
    Date::parse(
        value,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| invalid_field(field, "YYYY-MM-DD", value.to_owned()))
}

/// The currency vocabulary is closed and short, so the refusal publishes it.
///
/// The codes come from [`CurrencyCode::ALL`] rather than from the sentence
/// beside them: a sixth currency would otherwise be accepted by `from_code` and
/// withheld from the caller told which five it may send.
fn parse_currency(field: &'static str, value: &str) -> Result<CurrencyCode, ApiFailure> {
    CurrencyCode::from_code(value).ok_or_else(|| {
        ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::simple("invalid_request", format!("invalid field {field}"))
                .about(field)
                .expecting("RUB, USD, EUR, CNY or XAU")
                .receiving(value)
                .admitting(
                    CurrencyCode::ALL
                        .iter()
                        .map(|currency| InputAlternativeDto {
                            value: currency.code().to_owned(),
                            requires: Vec::new(),
                        })
                        .collect(),
                ),
        )
    })
}

fn invalid_field(field: impl Into<String>, expected: &str, actual: String) -> ApiFailure {
    let field = field.into();
    ApiFailure::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::simple("invalid_request", format!("invalid field {field}"))
            .about(field)
            .expecting(expected)
            .receiving(actual),
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
            ApiError::simple(
                "invalid_request",
                "report date must be in YYYY-MM-DD format",
            )
            .about("as_of")
            .expecting("YYYY-MM-DD")
            .receiving(raw),
        )
    })
}

/// The channel a declaration names, refused when it names none.
///
/// Shared by every derivation below on purpose: the source and the import are
/// both built from this text, and two copies of the bound would eventually
/// admit a channel one derivation accepted and the other refused.
fn declared_channel(declared: &DeclaredSourceDto) -> Result<&str, ApiFailure> {
    let channel = declared.channel.trim();
    if channel.is_empty() || channel.len() > 32 {
        return Err(ApiFailure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::simple("invalid_request", "channel must be 1..=32 characters")
                .about("source.channel")
                .expecting("a short channel name such as file, paste or manual")
                .receiving(declared.channel.clone()),
        ));
    }
    Ok(channel)
}

/// The source identity a declared source names, refused when it names none.
///
/// Shared by ingestion and by the import correction on purpose: the correction
/// reports the very source the import was written under, and two copies of this
/// derivation would eventually disagree about it.
///
/// The label is deliberately **not** part of it. Deduplication is scoped by the
/// source — a source operation identifier is unique within a source (§10.6) —
/// so a source that narrowed to one submission would stop one bank's
/// identifiers being compared across two of its own exports. What a retraction
/// needs is [`declared_import`], beside this and not inside it.
fn declared_source(
    owner: iaam_core::ids::OwnerId,
    account: AccountId,
    declared: &DeclaredSourceDto,
) -> Result<SourceId, ApiFailure> {
    let channel = declared_channel(declared)?;
    Ok(SourceId::declared(owner, account, channel))
}

/// The account a declaration names, however it named it.
///
/// A thin wrapper over the scenario, and the wrapping is the point: the tiering
/// that turns a printed identifier into one of the owner's accounts lives beside
/// the one that does it for a row's counterparty, so a batch cannot be declared
/// against one account while its rows resolve against another. The route holds
/// no rule of its own about what a source's identifier means.
///
/// Every derivation below takes the resolved account rather than the
/// declaration, so that the resolution happens exactly once per request: the
/// source and the import are both keyed on it, and deriving it twice is how two
/// keys for one import get written.
async fn declared_account(
    state: &ServerState,
    principal: &Principal,
    declared: &DeclaredSourceDto,
) -> Result<AccountDetailView, ApiFailure> {
    Ok(
        iaam_app::scenarios::import_session::resolve_declared_account(
            &state.services,
            principal,
            &declared.account,
        )
        .await?,
    )
}

/// The import identity a declaration names, or `None` when it names no import.
///
/// The single place the import key is derived, used by ingestion to stamp rows
/// and by the correction to find them again. Two copies of it would eventually
/// disagree about which import a retraction takes, and a retraction that took
/// the wrong rows cannot be undone by re-sending them.
fn declared_import(
    owner: iaam_core::ids::OwnerId,
    account: AccountId,
    declared: &DeclaredSourceDto,
) -> Result<Option<ImportId>, ApiFailure> {
    let channel = declared_channel(declared)?;
    let Some(label) = declared.label.as_deref().map(str::trim) else {
        return Ok(None);
    };
    if label.is_empty() || label.len() > 128 {
        return Err(invalid_field(
            "source.label",
            "a label of 1 to 128 characters naming this import, such as a \
             statement period or an export file name",
            declared.label.clone().unwrap_or_default(),
        ));
    }
    Ok(Some(ImportId::declared(owner, account, channel, label)))
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
