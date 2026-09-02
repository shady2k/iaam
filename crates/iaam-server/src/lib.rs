//! REST transport (§13).
//!
//! The API returns **fully prepared reports**, not raw data: the core calculates the figures,
//! and the transport serialises them. The agent must not perform its own arithmetic,
//! and the requirement is testable — any number in the agent's response that is absent
//! from the API responses is an error.

pub mod action_catalog;
pub mod auth;
pub use action_catalog::{ActionCatalog, ActionCatalogError, ActionOperation};
pub mod dto;
pub mod error;
pub mod openapi;
pub mod rate_limit;
pub mod routes;
pub mod vocabulary;

use std::sync::Arc;

use axum::routing::get;
use axum::{Extension, Json, Router, middleware};
use iaam_app::AppServices;
use iaam_app::jobs::{MarketScheduler, MarketSyncJob};
use thiserror::Error;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::openapi::ApiDoc;
use crate::rate_limit::RateLimiter;

/// Server state.
#[derive(Clone)]
pub struct ServerState {
    pub services: Arc<AppServices>,
    pub limiter: Arc<RateLimiter>,
    /// The market-series scheduler. No other task types reach it.
    pub market_scheduler: Arc<MarketScheduler>,
}

impl ServerState {
    #[must_use]
    pub fn new(services: Arc<AppServices>, limiter: Arc<RateLimiter>) -> Self {
        Self {
            market_scheduler: Arc::new(MarketScheduler::new(services.clone())),
            services,
            limiter,
        }
    }

    /// Register one market series before starting the server.
    pub fn register_market_job(&self, job: Arc<MarketSyncJob>) {
        self.market_scheduler.register(job);
    }
}

/// A failure while assembling the server and its action catalog.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error(transparent)]
    ActionCatalog(#[from] ActionCatalogError),
}

/// Builds the axum application together with the generated specification.
///
/// The public endpoints are `/v1/health` and the specification; all other
/// endpoints require authentication from the first request (§14).
pub fn build(state: ServerState) -> Result<(Router, utoipa::openapi::OpenApi), BuildError> {
    let protected = OpenApiRouter::new()
        .routes(routes!(routes::list_actions))
        .routes(routes!(routes::list_accounts, routes::create_account))
        .routes(routes!(routes::list_instruments, routes::create_instrument))
        .routes(routes!(routes::get_instrument))
        .routes(routes!(routes::resolve_instrument))
        .routes(routes!(routes::list_broker_access))
        .routes(routes!(routes::revoke_broker_access))
        .routes(routes!(routes::list_tokens, routes::create_token))
        .routes(routes!(routes::revoke_token))
        .routes(routes!(routes::create_contour_version))
        .routes(routes!(routes::ingest_operations))
        .routes(routes!(routes::ingest_journal_events))
        .routes(routes!(routes::ingest_csv))
        .routes(routes!(routes::upload_document))
        .routes(routes!(routes::reparse_document))
        .routes(routes!(routes::repair_custody))
        .routes(routes!(routes::reconciliation))
        .routes(routes!(routes::reconciliation_balance))
        .routes(routes!(
            routes::list_classification_rules,
            routes::create_classification_rule
        ))
        .routes(routes!(routes::delete_classification_rule))
        .routes(routes!(
            routes::list_category_groups,
            routes::create_category_group_route
        ))
        .routes(routes!(
            routes::list_category_reference,
            routes::create_category_route
        ))
        .routes(routes!(routes::delete_category))
        .routes(routes!(routes::list_category_rules_route))
        .routes(routes!(routes::create_category_rule_route))
        .routes(routes!(routes::preview_category_rule_route))
        .routes(routes!(routes::sync_broker))
        .routes(routes!(routes::sync_market))
        .routes(routes!(routes::list_market_key_rate))
        .routes(routes!(routes::list_market_fx))
        .routes(routes!(routes::list_market_prices))
        .routes(routes!(
            routes::returns_report,
            routes::returns_report_with_rates
        ))
        .routes(routes!(routes::flow_report))
        .routes(routes!(routes::balances_report))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ));

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(routes::api_catalog))
        .routes(routes!(routes::health))
        .merge(protected)
        .split_for_parts();
    let catalog = ActionCatalog::from_openapi(&api)?;

    // In production, build is called from within the tokio runtime. This check
    // preserves the ability to build a Router in a normal synchronous test.
    if tokio::runtime::Handle::try_current().is_ok() {
        std::mem::drop(state.market_scheduler.clone().spawn());
    }

    let spec = api.clone();
    let router = router
        .layer(Extension(Arc::new(catalog)))
        .route(
            "/v1/openapi.json",
            get(move || {
                let spec = spec.clone();
                async move { Json(spec) }
            }),
        )
        .with_state(state);
    Ok((router, api))
}
