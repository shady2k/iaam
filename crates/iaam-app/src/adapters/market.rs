//! HTTP adapter for market sources.
//!
//! Sends through the process's one gateway, which owns every outgoing
//! request policy — pacing, retries of transient failures, the breaker. What
//! stays here is hashing the body of a success. The use case receives an
//! already validated response through the port and knows nothing about
//! `reqwest` or sleeps.

use std::sync::Arc;

use async_trait::async_trait;
use iaam_http::client::HttpClient;
use iaam_http::gateway::Transport;
use iaam_http::{Destination, Gateway, GatewayError, HttpRequest};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::ports::{OutboundHttp, OutboundResponse};

/// The budget key this port's requests are sent under. The destinations
/// behind the port (MOEX, the CBR, the published contract) keep one budget
/// for every method, so the key names the caller rather than a method.
const METHOD: &str = "OutboundHttp";

/// Outbound transport over the shared gateway.
pub struct HttpOutbound<T = HttpClient> {
    gateway: Arc<Gateway<T>>,
}

impl<T> HttpOutbound<T> {
    #[must_use]
    pub const fn new(gateway: Arc<Gateway<T>>) -> Self {
        Self { gateway }
    }
}

#[async_trait]
impl<T: Transport + 'static> OutboundHttp for HttpOutbound<T> {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError> {
        let origin = origin(request.destination());
        let response = self
            .gateway
            .send(METHOD, &request, None)
            .await
            .map_err(|error| source_error(origin, &error))?;
        Ok(OutboundResponse {
            status: response.status,
            raw_hash: hash(&response.body),
            body: response.body,
        })
    }
}

/// Tells a source that is down from one that said no from our own fault, so
/// the caller learns whether to wait, to fix the request, or to look at us.
/// The gateway's refusal carries statuses, counts and the delay worth waiting,
/// never a header or body, so its text is safe to pass on.
fn source_error(origin: &str, error: &GatewayError) -> AppError {
    match error {
        GatewayError::Exhausted { .. }
        | GatewayError::DeadlineReached { .. }
        | GatewayError::CircuitOpen { .. } => AppError::SourceUnreachable {
            origin: origin.to_owned(),
            detail: error.to_string(),
            retry_after: error.retry_after(),
        },
        GatewayError::Rejected { .. } => AppError::SourceRefused {
            origin: origin.to_owned(),
            detail: error.to_string(),
        },
        // A missing budget, an invalid table, a transport that could not be
        // built: none of them is the source's answer.
        GatewayError::UnknownBudget { .. }
        | GatewayError::InvalidBudgets(_)
        | GatewayError::Transport { .. } => {
            AppError::Store(format!("market source {origin}: {error}"))
        }
    }
}

/// The source a destination belongs to, spelled as the market store's
/// `source_id` spells it, so a refusal and the series it stopped name the
/// same thing.
const fn origin(destination: Destination) -> &'static str {
    match destination {
        Destination::MoexIss => "moex-iss",
        Destination::CbrScripts | Destination::CbrDailyInfo => "cbr",
        Destination::TinvestContract => "tinvest-contract",
        Destination::TinkoffProd | Destination::TinkoffSandbox => "tinkoff",
        Destination::FinamApi => "finam",
    }
}

fn hash(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
