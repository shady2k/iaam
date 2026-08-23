//! REST-транспорт (§13).
//!
//! API отдаёт **готовые отчёты**, а не сырые данные: числа считает ядро,
//! транспорт их сериализует. Агенту запрещена собственная арифметика,
//! и требование проверяемо — число в ответе агента, отсутствующее
//! в ответах API, является ошибкой.

pub mod auth;
pub mod dto;
pub mod error;
pub mod openapi;
pub mod rate_limit;
pub mod routes;

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router, middleware};
use iaam_app::AppServices;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::openapi::ApiDoc;
use crate::rate_limit::RateLimiter;

/// Состояние сервера.
#[derive(Clone)]
pub struct ServerState {
    pub services: Arc<AppServices>,
    pub limiter: Arc<RateLimiter>,
}

impl ServerState {
    #[must_use]
    pub fn new(services: Arc<AppServices>, limiter: Arc<RateLimiter>) -> Self {
        Self { services, limiter }
    }
}

/// Сборка приложения axum вместе с порождённой спекой.
///
/// Публичным остаётся только `/v1/health` и сама спека: аутентификация
/// с первого дня, и отложенной она не станет никогда (§14).
pub fn build(state: ServerState) -> (Router, utoipa::openapi::OpenApi) {
    let protected = OpenApiRouter::new()
        .routes(routes!(routes::list_accounts, routes::create_account))
        .routes(routes!(
            routes::list_broker_access,
            routes::add_broker_access
        ))
        .routes(routes!(routes::revoke_broker_access))
        .routes(routes!(routes::create_contour_version))
        .routes(routes!(routes::ingest_operations))
        .routes(routes!(routes::ingest_csv))
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
