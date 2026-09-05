//! OpenAPI spec generated from handler types (§17.1).
//!
//! Generation eliminates **data schema** discrepancies, but not behavioural ones:
//! runtime response codes, authentication requirements, and actual
//! serialisation of custom types remain outside generation. Therefore,
//! black-box contract tests exist (task 15).

use utoipa::Modify;
use utoipa::OpenApi;
use utoipa::openapi::path::Operation;
use utoipa::openapi::response::{Response, ResponseBuilder};
use utoipa::openapi::schema::{Object, Type};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{ContentBuilder, HeaderBuilder, Ref, RefOr};

use crate::dto::{
    AccountBalanceDto, AccountCandidateDto, AccountDto, AccountNameDispositionDto,
    AccountResidualDto, AccountScopeDispositionDto, AccountScopeDto, ActionDto, ActionSubjectDto,
    ActionTargetDto, AddContourVersionRequest, AmountDto, AssetAccountDto, AssetSnapshotDto,
    BalancesReportDto, BasisCertaintyDto, BasisTransferRuleDto, BondPositionMetricsDto,
    BondScenarioResultDto, BrokerAccessDto, BrokerEnvironmentDto, BrokerSyncRequest, CalcMoneyDto,
    CashClassTotalDto, CashFigureDto, CashSideDto, CategoryAmountDto, CategoryDto, CategoryMoveDto,
    CategoryRequest, CategoryRuleDto, CategoryRuleImpactDto, CategoryRuleRequest, CaveatDto,
    CaveatSubjectDto, CertaintyDto, ClaimDto, ClaimOutcomeDetailDto, ClaimOutcomeDto,
    ClaimValueDto, ClassificationRuleChangeDto, ClassificationRuleDto, ClassificationRuleRequest,
    ClassifiedAsDto, ClosingOperationDto, ComputedCalcMoneyDto, ComputedDto,
    ComputedLifetimeCohortMetricsDto, ComputedZeroReinvestmentMetricsDto, ConfidenceDto,
    ContourDto, ContourVersionDto, CorporateActionDto, CorrectImportRequest, CorrectionDto,
    CreateAccountRequest, CreateContourVersionRequest, CreateInstrumentRequest, CreateTokenRequest,
    CurrencyDto, CustodyRepairCaseDto, CustodyRepairOutcomeDto, CustodyRepairRequest,
    DataQualityDto, DateCertaintyDto, DeclaredAccountDto, DeclaredSourceDto, DimensionStatusDto,
    DiscrepancyDto, DocumentDto, EvaluatedPositionDto, EvidenceDto, ExecutabilitySharesDto,
    ExpectedPostingDto, FeeOriginDto, FractionalTreatmentDto, FxRateDto, HealthDto, HeldRowsDto,
    HeldSessionDto, HoldingPriceDto, HoldingValueDto, ImportCorrectionDto, IncomeKindDto,
    InputAlternativeDto, IrrLabelDto, IssuedTokenDto, JournalConfidenceDto, JournalEventDatesDto,
    JournalEventDto, JournalEventReadDto, JournalFactDto, JournalLegDto, JournalLegKindDto,
    JournalPageDto, JournalRelationDto, JournalRelationKindDto, KnowledgeDto,
    LegacyDerivedPositionDto, LifetimeCohortMetricDto, LiquidationEstimateDto, MarketFxDto,
    MarketFxSeriesDto, MarketKeyRateDto, MarketKeyRateSeriesDto, MarketPriceDto,
    MarketPriceSeriesDto, MarketSourceDto, MarketSyncRequest, MissingInputDto,
    MoneyFlowCurrencyDto, MoneyFlowReportDto, NegativeBalanceExpectationDto, NegativeCashDto,
    NotDecomposedAccountDto, NotDecomposedDto, ObservationBasisDto, OfferChoiceDto,
    OpeningAssertionsDto, OperationDatesDto, OperationDto, OperationKindDto, OwnerBalanceRequest,
    OwnerQuestionDto, PerimeterRefusalDto, PlannedCorrectionDto, PopulationAccountDto,
    PopulationDto, PositionCoverageDto, PositionsSideDto, PostingKindDto, PriceFreshnessDto,
    PriceOriginDto, PriceProvenanceDto, PriceQualityDto, PriceSelectionDto, PrintedAccountNameDto,
    ProposedAnswerDto, ProspectiveMetricDto, QuotationBasisDto, QuotationBasisStatusDto, RateDto,
    RecomputePlanDto, ReconciliationResponseDto, ReconciliationStatusDto,
    RecordAccountNameDispositionRequest, RecordAccountScopeRequest, RefusedRowDto, RequestPlanDto,
    RequiredInputDto, ResolutionOptionDto, ReturnsAnswerDto, ReturnsReportDto, RowNameDto,
    RuleMatcherDto, SelectedPriceDto, SubmitCorrectionsRequest, SubmitJournalEventsRequest,
    SubmitOperationsRequest, SyncOutcomeDto, TaintDto, TokenDto, TristateDto, UncoveredPositionDto,
    VerdictDto, ZeroReinvestmentMetricsDto,
};
// The import assessment's own types, in a block of their own rather than merged
// into the list above. Merging one name into a wrapped list of two hundred
// reflows a third of them, and this file is edited by several changes at once;
// a second `use` of the same module is the shape `routes.rs` already uses for
// the same reason.
use crate::dto::{
    BatchTotalDto, ControlCheckDto, ControlComparisonDto, ControlReconciliationDto,
    ControlSectionDto, RecordedEventDto, StateImportControlFiguresRequest, StatedControlFiguresDto,
};
// Wave AB's own two, in a block of their own for the same reason.
use crate::dto::{ActionsResponseDto, ReportStandingDto};
// Wave K's own names, in a block of their own for the reason stated above:
// these belong to changes made in parallel, and merging them into the wrapped
// list would reflow lines nobody touched.
use crate::dto::{IntervalFitDto, OwnerBalanceOutcomeDto, QuestionGeneralisationDto};
// Wave O's types, in a block of their own for the reason the block above gives.
use crate::dto::{AccountRetirementDto, AccountRetirementStateDto, RecordAccountRetirementRequest};
// Wave T's types, in a block of their own for the reason the block above gives.
use crate::dto::{
    ReadingProfileDto, RefusedProfileDto, SourceDocumentDto, SourceDocumentRowDto,
    SourceProfileCatalogueDto, SourceProfileDto,
};
// Wave U's types, in a block of their own for the reason the block above gives.
use crate::dto::UnresolvedAccountDto;
use crate::error::ApiError;
use crate::routes::MarketSyncOutcomeDto;
use crate::vocabulary::{
    DataQualityStatusDto, NegativeCashClassificationDto, NotComputableCodeDto, ProvidedByDto,
    VerdictCodeDto,
};

/// The name the bearer scheme is declared and required under.
///
/// One constant for both, because the refusal for frequency is published on
/// exactly the operations that require this scheme: if the declaration and the
/// requirement were spelled separately, a rename would leave the refusal
/// published on nothing while every route still asked for a token.
const BEARER_SCHEME: &str = "bearer";

/// The status a refusal for request frequency is given.
const TOO_MANY_REQUESTS: &str = "429";

/// What a client is told when it is refused for calling too often.
///
/// Written once, here, and attached below to every operation that can give it.
/// It says what the count is over, where to read the wait, and what not to do —
/// and it names neither the window's length nor the number of calls allowed,
/// because both are configuration of one deployment and a published document
/// that quoted them would be wrong at the next restart that changed either.
const FREQUENCY_REFUSAL: &str = "Too many requests. This instance counts calls per token over a fixed window, and \
     `Retry-After` on the response says how many seconds of that window are left. Wait that long \
     before calling again. Lower the frequency rather than repeating immediately: a repeat inside \
     the same window is refused again and brings the answer no closer.";

/// What the `Retry-After` header on that refusal carries.
const RETRY_AFTER_HEADER: &str = "Whole seconds left of the window this token is counted over. Rounded up, and never zero, so \
     waiting exactly this long is enough and no answer is lost by waiting it.";

/// Authentication scheme. Declared separately: `utoipa` generates it
/// from types, but the `Bearer` requirement cannot be expressed by a type.
pub struct BearerSecurity;

impl Modify for BearerSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                BEARER_SCHEME,
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

/// The refusal for request frequency, on every operation that can give it.
///
/// The limit is enforced in the authentication layer, and every operation that
/// requires a token passes through it — so every one of them can answer this
/// way, and none of them can be written to avoid it. Declared per route, it was
/// declared on three operations out of seventy-two, which is worse than nowhere:
/// the skill tells a client to read this document instead of asking anyone what
/// a refusal will look like, so a client built from what is published would meet
/// an undeclared refusal on the other sixty-nine and answer it by sending the
/// same request again — the one move that keeps it refused.
///
/// So the operations are found by the security requirement they carry, never by
/// a list of paths kept here. A route added tomorrow is covered by asking for a
/// token, which it must do anyway, rather than by somebody remembering to come
/// back to this file.
///
/// A route with something of its own to say about frequency declares a `429`
/// carrying **only** that sentence; it is appended to the shared paragraph
/// below, so the two are one text with an extra clause and not two texts that
/// can drift.
pub struct FrequencyRefusal;

impl Modify for FrequencyRefusal {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        for item in openapi.paths.paths.values_mut() {
            // The eight methods are separate fields rather than a map in this
            // version of the model, so they are gathered by hand. Listing all
            // eight, including the ones no route uses, is deliberate: a list
            // trimmed to what exists today is a list to be wrong about later.
            let operations = [
                item.get.as_mut(),
                item.put.as_mut(),
                item.post.as_mut(),
                item.delete.as_mut(),
                item.options.as_mut(),
                item.head.as_mut(),
                item.patch.as_mut(),
                item.trace.as_mut(),
            ];
            for operation in operations.into_iter().flatten() {
                if !requires_bearer(operation) {
                    continue;
                }
                let particular = operation
                    .responses
                    .responses
                    .remove(TOO_MANY_REQUESTS)
                    .and_then(own_description);
                operation.responses.responses.insert(
                    TOO_MANY_REQUESTS.to_owned(),
                    RefOr::T(frequency_refusal(particular.as_deref())),
                );
            }
        }
    }
}

/// Whether this operation asks for a bearer token, and so passes the limiter.
fn requires_bearer(operation: &Operation) -> bool {
    operation.security.iter().flatten().any(|requirement| {
        // The requirement keeps its scheme names private and publishes them
        // only by serialising, which is also the form the document is read in.
        // Comparing against a requirement built here would instead ask whether
        // the scopes match, and a route that one day asks for a scope would
        // quietly stop being covered.
        serde_json::to_value(requirement)
            .ok()
            .and_then(|value| {
                value
                    .as_object()
                    .map(|schemes| schemes.contains_key(BEARER_SCHEME))
            })
            .unwrap_or(false)
    })
}

/// The sentence a route declared for itself, where it declared one.
fn own_description(declared: RefOr<Response>) -> Option<String> {
    match declared {
        RefOr::T(response) => Some(response.description),
        // A reference to a shared response component holds no text of its own
        // to carry over.
        RefOr::Ref(_) => None,
    }
}

/// The published refusal, with whatever one route adds to it.
fn frequency_refusal(particular: Option<&str>) -> Response {
    let description = match particular {
        Some(particular) => format!("{FREQUENCY_REFUSAL} {particular}"),
        None => FREQUENCY_REFUSAL.to_owned(),
    };
    ResponseBuilder::new()
        .description(description)
        // Published as a header and not only as prose: a client reads the wait
        // from the response, and a wait described in a sentence has to be
        // guessed at from the sentence.
        .header(
            "Retry-After",
            HeaderBuilder::new()
                .schema(Object::with_type(Type::Integer))
                .description(Some(RETRY_AFTER_HEADER))
                .build(),
        )
        .content(
            "application/json",
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("ApiError")))
                .build(),
        )
        .build()
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
        ActionsResponseDto,
        ReportStandingDto,
        ActionDto,
        ActionSubjectDto,
        AccountScopeDto,
        AccountScopeDispositionDto,
        RecordAccountScopeRequest,
        AccountNameDispositionDto,
        RecordAccountNameDispositionRequest,
        PrintedAccountNameDto,
        AccountRetirementDto,
        AccountRetirementStateDto,
        RecordAccountRetirementRequest,
        ActionTargetDto,
        ResolutionOptionDto,
        InputAlternativeDto,
        OwnerQuestionDto,
        ProposedAnswerDto,
        RequiredInputDto,
        MissingInputDto,
        RequestPlanDto,
        AccountBalanceDto,
        CashFigureDto,
        BalancesReportDto,
        AssetSnapshotDto,
        AssetAccountDto,
        CashSideDto,
        CashClassTotalDto,
        PositionsSideDto,
        HoldingValueDto,
        HoldingPriceDto,
        NegativeCashDto,
        NegativeBalanceExpectationDto,
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
        ObservationBasisDto,
        ClassificationRuleChangeDto,
        ClassificationRuleDto,
        ClassifiedAsDto,
        RuleMatcherDto,
        PlannedCorrectionDto,
        RecomputePlanDto,
        ClassificationRuleRequest,
        ComputedDto,
        ContourDto,
        ContourVersionDto,
        AddContourVersionRequest,
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
        DeclaredAccountDto,
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
        ReturnsAnswerDto,
        PopulationDto,
        PopulationAccountDto,
        HeldRowsDto,
        HeldSessionDto,
        ConfidenceDto,
        CaveatDto,
        CaveatSubjectDto,
        ClosingOperationDto,
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
        BatchTotalDto,
        StateImportControlFiguresRequest,
        StatedControlFiguresDto,
        ControlSectionDto,
        ControlReconciliationDto,
        ControlComparisonDto,
        ControlCheckDto,
        IntervalFitDto,
        RecordedEventDto,
        OwnerBalanceOutcomeDto,
        QuestionGeneralisationDto,
        // The source-profile channel (decision 0019): the format catalogue this
        // deployment reads with, and one document read through it.
        SourceProfileDto,
        RefusedProfileDto,
        SourceProfileCatalogueDto,
        ReadingProfileDto,
        SourceDocumentRowDto,
        UnresolvedAccountDto,
        SourceDocumentDto,
        // The published vocabularies: every code the API can return, each with
        // the sentence that explains it (§13).
        VerdictCodeDto,
        NotComputableCodeDto,
        DataQualityStatusDto,
        ProvidedByDto,
    ))
)]
pub struct ApiDoc;
