//! Маршруты.
//!
//! Обработчик делает три вещи: разбирает DTO, зовёт сценарий, сериализует
//! результат. Ни одной арифметической операции над деньгами здесь нет —
//! это проверяется заслоном архитектуры (§3.1, §13).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use iaam_app::AppServices;
use iaam_app::ingest::csv_source::{Directory, ParsedRow, parse};
use iaam_app::ingest::{SubmittedOperation, Verdict};
use iaam_app::ports::{AccountView, Principal, Scope, SoleOwner};
use iaam_app::scenarios::ingest::submit_operations;
use iaam_app::scenarios::reports::{ReturnsQuery, returns};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, OwnerId, SourceId};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::PROJECTION_VERSION;
use iaam_core::rules::LotRuleVersion;
use iaam_core::valuation::{FxSource, FxTable};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use utoipa::IntoParams;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::ServerState;
use crate::dto::{
    AccountDto, AddBrokerAccessRequest, BrokerAccessDto, ClaimRequest, ContourVersionDto,
    CreateAccountRequest, CreateContourVersionRequest, CreateTokenRequest, CurrencyDto, FxRateDto,
    HealthDto, IssuedTokenDto, ReturnsReportDto, SubmitOperationsRequest, TokenDto, TokenScopeDto,
    VerdictDto,
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

/// Заведение брокерского доступа.
///
/// Токен приходит от владельца и наружу не возвращается: в ответе —
/// только идентификатор записи, по которому доступ отзывают (§14).
#[utoipa::path(
    post,
    path = "/v1/broker-access",
    request_body = AddBrokerAccessRequest,
    responses(
        (status = 201, description = "Доступ заведён", body = BrokerAccessDto),
        (status = 403, description = "Недостаточно прав", body = ApiError),
        (status = 422, description = "Код брокера или токен пусты", body = ApiError),
        (status = 503, description = "Шифрование доступа не настроено", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn add_broker_access(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<AddBrokerAccessRequest>,
) -> Result<(StatusCode, Json<BrokerAccessDto>), ApiFailure> {
    require_admin(&principal)?;
    // Токен заворачивается в зануляемую память сразу при разборе тела
    // и дальше не копируется: открытым он живёт до шифрования в адаптере
    // и зануляется при уничтожении.
    let token = Zeroizing::new(request.token);
    let created = state
        .services
        .broker
        .add_access(principal.owner, request.broker, token)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(BrokerAccessDto::from_domain(created)),
    ))
}

/// Список брокерских доступов.
///
/// Отозванные тоже показываются: «когда система перестала ходить
/// к брокеру» является вопросом, на который нужен ответ. Действующий
/// от отозванного отличается полем `revoked_at`.
#[utoipa::path(
    get,
    path = "/v1/broker-access",
    responses(
        (status = 200, description = "Доступы владельца", body = Vec<BrokerAccessDto>),
        (status = 403, description = "Недостаточно прав", body = ApiError),
        (status = 503, description = "Шифрование доступа не настроено", body = ApiError)
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

/// Отзыв брокерского доступа.
#[utoipa::path(
    delete,
    path = "/v1/broker-access/{id}",
    params(("id" = Uuid, Path, description = "Идентификатор заведённого доступа")),
    responses(
        (status = 204, description = "Доступ отозван"),
        (status = 403, description = "Недостаточно прав", body = ApiError),
        (status = 503, description = "Шифрование доступа не настроено", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn revoke_broker_access(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiFailure> {
    require_admin(&principal)?;
    state
        .services
        .broker
        .revoke_access(principal.owner, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Присвоение экземпляра.
///
/// **Единственный маршрут без аутентификации, кроме `/v1/health`.**
/// Иначе и быть не может: токена у присваивающего ещё нет, и позвать
/// защищённый маршрут ему нечем. Вместо токена его пускает одноразовый
/// код, напечатанный при старте в консоль, — то есть доказательством
/// служит доступ к машине, а не знание чего-либо пересылаемого.
///
/// Открытой регистрации здесь не будет никогда: второй пришедший завёл
/// бы себе пустой портфель в чужой базе. Владелец уже есть — присвоение
/// закрыто навсегда.
#[utoipa::path(
    post,
    path = "/v1/claim",
    request_body = ClaimRequest,
    responses(
        (status = 201, description = "Экземпляр присвоен", body = IssuedTokenDto),
        (status = 403, description = "Код неверен, просрочен или уже использован", body = ApiError),
        (status = 409, description = "Владелец уже есть: присвоение закрыто", body = ApiError)
    )
)]
pub async fn claim(
    State(state): State<ServerState>,
    Json(request): Json<ClaimRequest>,
) -> Result<(StatusCode, Json<IssuedTokenDto>), ApiFailure> {
    // Код проверяется до состояния базы: одноразовость — свойство
    // самого кода, а не следствие того, что владелец завёлся. Проверка
    // и стирание идут одной операцией под одним замком — разделив их,
    // два одновременных запроса с верным кодом получили бы по токену
    // владельца каждый.
    if !state.accept_claim(&request.code) {
        // Неверный, просроченный и уже использованный код дают
        // **одинаковый** ответ: разные сообщили бы, что код угадан
        // наполовину (§14).
        return Err(claim_refused());
    }

    // Код был верен, но владелец успел появиться — например, его завели
    // с консоли уже после старта, и напечатанный код устарел. Присвоение
    // закрыто навсегда: второй владелец в однопользовательской системе
    // означает пустой портфель в чужой базе. Код при этом уже истрачен,
    // и это не потеря — присваивать всё равно нечего.
    match state.services.tokens.sole_owner().await? {
        SoleOwner::None => {}
        SoleOwner::Single(_) | SoleOwner::Several => {
            return Err(ApiFailure::new(
                StatusCode::CONFLICT,
                ApiError::simple(
                    "already_claimed",
                    "экземпляр уже присвоен: потерянный токен восстанавливается с консоли",
                ),
            ));
        }
    }

    // Владелец заводится здесь и только здесь через API: дальше он
    // существует, и второго присвоения не будет.
    let issued = state
        .services
        .tokens
        .issue_token(OwnerId::new_random(), request.label, Scope::Owner)
        .await?;
    Ok((StatusCode::CREATED, Json(IssuedTokenDto::from_domain(issued))))
}

/// Отказ в присвоении.
///
/// Один текст на три разные причины намеренно: сообщение, различающее
/// «код не тот» и «код просрочен», подтверждает угадавшему, что он
/// угадал (§14).
fn claim_refused() -> ApiFailure {
    ApiFailure::new(
        StatusCode::FORBIDDEN,
        ApiError::simple(
            "claim_refused",
            "код присвоения не принят: неверен, просрочен или уже использован",
        ),
    )
}

/// Выпуск токена.
///
/// Токен показывается **один раз**: в базе остаётся только его хеш,
/// и повторить показ неоткуда (§14).
#[utoipa::path(
    post,
    path = "/v1/tokens",
    request_body = CreateTokenRequest,
    responses(
        (status = 201, description = "Токен выпущен и показан один раз", body = IssuedTokenDto),
        (status = 403, description = "Недостаточно прав", body = ApiError),
        (status = 422, description = "Область owner через API не выпускается", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_token(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<IssuedTokenDto>), ApiFailure> {
    require_admin(&principal)?;
    let scope = match request.scope {
        // Полный доступ через API не выпускается: владелец заводится
        // присвоением экземпляра или консолью. Иначе украденный токен
        // владельца немедленно размножался бы в неотличимые копии,
        // и отзыв исходного ничего бы не менял.
        TokenScopeDto::Owner => {
            return Err(ApiFailure::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: "invalid_request".into(),
                    message: "токен владельца через API не выпускается: владелец заводится \
                              присвоением экземпляра или командой консоли"
                        .into(),
                    field: Some("scope".into()),
                    expected: Some("agent или read_only".into()),
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
    Ok((StatusCode::CREATED, Json(IssuedTokenDto::from_domain(issued))))
}

/// Список выданных токенов.
///
/// Ни токенов, ни их хешей в ответе нет и быть не может: хеш — это то,
/// что достаточно подставить в запрос поиска, чтобы система признала
/// предъявителя своим. Отозванные показываются: «когда токен перестал
/// пускать» является вопросом, на который нужен ответ.
#[utoipa::path(
    get,
    path = "/v1/tokens",
    responses(
        (status = 200, description = "Токены владельца", body = Vec<TokenDto>),
        (status = 403, description = "Недостаточно прав", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_tokens(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<TokenDto>>, ApiFailure> {
    require_admin(&principal)?;
    let tokens = state.services.tokens.list_tokens(principal.owner).await?;
    Ok(Json(tokens.into_iter().map(TokenDto::from_domain).collect()))
}

/// Отзыв токена.
///
/// Отсутствующий и чужой токен дают одинаковый `404` намеренно: разные
/// ответы сообщили бы постороннему, что такая запись есть (§14).
#[utoipa::path(
    delete,
    path = "/v1/tokens/{id}",
    params(("id" = Uuid, Path, description = "Идентификатор выданного токена")),
    responses(
        (status = 204, description = "Токен отозван"),
        (status = 403, description = "Недостаточно прав", body = ApiError),
        (status = 404, description = "Токена нет или он чужой", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn revoke_token(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiFailure> {
    require_admin(&principal)?;
    state
        .services
        .tokens
        .revoke_token(principal.owner, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
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
