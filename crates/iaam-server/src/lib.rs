//! REST transport (§13).
//!
//! The API returns **fully prepared reports**, not raw data: the core calculates the figures,
//! and the transport serialises them. The agent must not perform its own arithmetic,
//! and the requirement is testable — any number in the agent's response that is absent
//! from the API responses is an error.

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
use iaam_app::jobs::{MarketScheduler, MarketSyncJob};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::claim::ClaimCode;
use crate::openapi::ApiDoc;
use crate::rate_limit::RateLimiter;

/// Server state.
#[derive(Clone)]
pub struct ServerState {
    pub services: Arc<AppServices>,
    pub limiter: Arc<RateLimiter>,
    /// The market-series scheduler. No other task types reach it.
    pub market_scheduler: Arc<MarketScheduler>,
    /// The instance claim code. `None` means there is nothing to claim:
    /// either it already has an owner, or the code has already been used.
    ///
    /// Kept in process memory and nowhere else: leaking the database file must not
    /// allow the instance to be taken over (§14). The mutex is standard, not `tokio`:
    /// no I/O is awaited while it is held, and `Option` changes in a single
    /// operation — an asynchronous mutex would cost more here than it offers.
    claim: Arc<Mutex<Option<ClaimCode>>>,
}

impl ServerState {
    #[must_use]
    pub fn new(services: Arc<AppServices>, limiter: Arc<RateLimiter>) -> Self {
        Self {
            market_scheduler: Arc::new(MarketScheduler::new(services.clone())),
            services,
            limiter,
            claim: Arc::new(Mutex::new(None)),
        }
    }

    /// Register one market series before starting the server.
    pub fn register_market_job(&self, job: Arc<MarketSyncJob>) {
        self.market_scheduler.register(job);
    }

    /// Arm the claim code. Called at start-up — see `claim::arm`.
    pub fn arm_claim(&self, code: ClaimCode) {
        *self.locked_claim() = Some(code);
    }

    /// Accept the code if it is valid, and **erase** it.
    ///
    /// Checking and erasing are one operation under one lock: separating
    /// them would let two simultaneous requests with the valid code each receive an owner
    /// token. An invalid code does not erase the code: otherwise
    /// any outsider could disable claiming with a single junk request
    /// forever.
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

    /// The lock protecting the claim code.
    ///
    /// A poisoned mutex is recovered rather than causing a panic:
    /// a panic in one request must not bring down the entire service,
    /// and a panic in the previous call cannot corrupt `Option<ClaimCode>`.
    fn locked_claim(&self) -> std::sync::MutexGuard<'_, Option<ClaimCode>> {
        match self.claim.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Builds the axum application together with the generated specification.
///
/// The public endpoints remain `/v1/health`, the specification itself and instance
/// claiming: authentication is required from day one, and will never be deferred
/// (§14). Claiming is an exception born of necessity, not
/// weakness: the claimant does not yet have a token, and has no means to call
/// a protected route. Instead of a token, they are admitted by a one-time
/// code read from the console — see `claim`.
pub fn build(state: ServerState) -> (Router, utoipa::openapi::OpenApi) {
    // In production, build is called from within the tokio runtime. This check
    // preserves the ability to build a Router in a normal synchronous test.
    if tokio::runtime::Handle::try_current().is_ok() {
        std::mem::drop(state.market_scheduler.clone().spawn());
    }
    let protected = OpenApiRouter::new()
        .routes(routes!(routes::list_accounts, routes::create_account))
        .routes(routes!(routes::list_instruments, routes::create_instrument))
        .routes(routes!(routes::get_instrument))
        .routes(routes!(routes::resolve_instrument))
        .routes(routes!(
            routes::list_broker_access,
            routes::add_broker_access
        ))
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
        .routes(routes!(routes::sync_broker))
        .routes(routes!(routes::sync_market))
        .routes(routes!(routes::list_market_key_rate))
        .routes(routes!(routes::list_market_fx))
        .routes(routes!(routes::list_market_prices))
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
