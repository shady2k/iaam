//! HTTP adapter for market sources.
//!
//! Sends through the process's one gateway, which owns every outgoing
//! request policy — pacing, retries of transient failures, the breaker. What
//! stays here is hashing the body of a success. The use case receives an
//! already validated response through the port and knows nothing about
//! `reqwest` or sleeps.

use std::sync::Arc;

use async_trait::async_trait;
use iaam_http::{Destination, GatewayError, HttpRequest, Outbound};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::ports::{OutboundHttp, OutboundResponse};

/// The budget key this port's requests are sent under. The destinations
/// behind the port (MOEX, the CBR, the published contract) keep one budget
/// for every method, so the key names the caller rather than a method.
const METHOD: &str = "OutboundHttp";

/// Outbound transport over the shared gateway.
pub struct HttpOutbound {
    gateway: Arc<dyn Outbound>,
}

impl HttpOutbound {
    #[must_use]
    pub fn new(gateway: Arc<dyn Outbound>) -> Self {
        Self { gateway }
    }
}

#[async_trait]
impl OutboundHttp for HttpOutbound {
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
///
/// Transience is read from `retry_after` rather than from the variants, so a
/// transient refusal the gateway adds later is "retry later" here too.
fn source_error(origin: &str, error: &GatewayError) -> AppError {
    if let Some(retry_after) = error.retry_after() {
        return AppError::SourceUnreachable {
            origin: origin.to_owned(),
            detail: error.to_string(),
            retry_after: Some(retry_after),
        };
    }
    match error {
        GatewayError::Rejected { .. } => AppError::SourceRefused {
            origin: origin.to_owned(),
            detail: error.to_string(),
        },
        // A missing budget, an invalid table, a transport that could not be
        // built: none of them is the source's answer.
        _ => AppError::Store(format!("market source {origin}: {error}")),
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
