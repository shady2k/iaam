//! REST-транспорт (§13).
//!
//! API отдаёт **готовые отчёты**, а не сырые данные: числа считает ядро,
//! транспорт их сериализует. Агенту запрещена собственная арифметика,
//! и требование проверяемо — число в ответе агента, отсутствующее
//! в ответах API, является ошибкой.

pub mod auth;
pub mod claim;
pub mod dto;
pub mod error;
pub mod openapi;
pub mod rate_limit;
pub mod routes;

use std::sync::{Arc, Mutex};

use axum::routing::get;
use axum::{Json, Router, middleware};
use iaam_app::AppServices;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::claim::ClaimCode;
use crate::openapi::ApiDoc;
use crate::rate_limit::RateLimiter;

/// Состояние сервера.
#[derive(Clone)]
pub struct ServerState {
    pub services: Arc<AppServices>,
    pub limiter: Arc<RateLimiter>,
    /// Код присвоения экземпляра. `None` — присваивать нечего:
    /// владелец либо уже есть, либо код уже использован.
    ///
    /// В памяти процесса и только в ней: утечка файла базы не должна
    /// отдавать экземпляр (§14). Мьютекс стандартный, а не `tokio`:
    /// под ним не ждут ввода-вывода, а `Option` меняется одной
    /// операцией — асинхронный мьютекс здесь стоил бы дороже, чем даёт.
    claim: Arc<Mutex<Option<ClaimCode>>>,
}

impl ServerState {
    #[must_use]
    pub fn new(services: Arc<AppServices>, limiter: Arc<RateLimiter>) -> Self {
        Self {
            services,
            limiter,
            claim: Arc::new(Mutex::new(None)),
        }
    }

    /// Взвести код присвоения. Зовётся при старте — см. `claim::arm`.
    pub fn arm_claim(&self, code: ClaimCode) {
        *self.locked_claim() = Some(code);
    }

    /// Принять код, если он верен, и **стереть** его.
    ///
    /// Проверка и стирание — одна операция под одним замком: разделив
    /// их, два одновременных запроса с верным кодом получили бы по
    /// токену владельца каждый. Неверный код кода не стирает: иначе
    /// любой посторонний одним запросом с мусором закрывал бы
    /// присвоение навсегда.
    pub fn accept_claim(&self, code: &str) -> bool {
        let mut guard = self.locked_claim();
        match guard.as_ref() {
            Some(stored) if stored.accepts(code) => {
                *guard = None;
                true
            }
            Some(_) | None => false,
        }
    }

    /// Замок над кодом присвоения.
    ///
    /// Отравленный мьютекс восстанавливается, а не приводит к панике:
    /// паника в одном запросе не должна выводить из строя весь сервис,
    /// а `Option<ClaimCode>` паника предыдущего вызова не повреждает.
    fn locked_claim(&self) -> std::sync::MutexGuard<'_, Option<ClaimCode>> {
        match self.claim.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Сборка приложения axum вместе с порождённой спекой.
///
/// Публичными остаются `/v1/health`, сама спека и присвоение
/// экземпляра: аутентификация с первого дня, и отложенной она не станет
/// никогда (§14). Присвоение — исключение по необходимости, а не по
/// слабости: токена у того, кто присваивает, ещё нет, и позвать
/// защищённый маршрут ему нечем. Вместо токена его пускает одноразовый
/// код, прочитанный с консоли, — см. `claim`.
pub fn build(state: ServerState) -> (Router, utoipa::openapi::OpenApi) {
    let protected = OpenApiRouter::new()
        .routes(routes!(routes::list_accounts, routes::create_account))
        .routes(routes!(
            routes::list_broker_access,
            routes::add_broker_access
        ))
        .routes(routes!(routes::revoke_broker_access))
        .routes(routes!(routes::list_tokens, routes::create_token))
        .routes(routes!(routes::revoke_token))
        .routes(routes!(routes::create_contour_version))
        .routes(routes!(routes::ingest_operations))
        .routes(routes!(routes::ingest_csv))
        .routes(routes!(routes::upload_document))
        .routes(routes!(routes::reparse_document))
        .routes(routes!(routes::reconciliation))
        .routes(routes!(routes::reconciliation_balance))
        .routes(routes!(
            routes::list_classification_rules,
            routes::create_classification_rule
        ))
        .routes(routes!(routes::delete_classification_rule))
        .routes(routes!(routes::sync_broker))
        .routes(routes!(routes::update_broker_access))
        .routes(routes!(
            routes::returns_report,
            routes::returns_report_with_rates
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ));

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(routes::health))
        .routes(routes!(routes::claim))
        .merge(protected)
        .split_for_parts();

    let spec = api.clone();
    let router = router
        .route(
            "/v1/openapi.json",
            get(move || {
                let spec = spec.clone();
                async move { Json(spec) }
            }),
        )
        .with_state(state);
    (router, api)
}
