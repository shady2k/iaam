//! HTTP-адаптер источников рынка.
//!
//! Здесь сосредоточена вся политика исходящих запросов: ограничение частоты,
//! повторы временных отказов и хеширование тела. Сценарий получает уже
//! проверенный ответ через порт и не знает ни о `reqwest`, ни о снах.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use iaam_http::client::HttpClient;
use iaam_http::resilience::{Outcome, RateLimiter, Retry, RetryPolicy};
use iaam_http::{HttpError, HttpRequest};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::ports::{OutboundHttp, OutboundResponse};

/// Реализация рыночного транспорта поверх общего HTTP-клиента.
pub struct HttpOutbound {
    client: HttpClient,
    retry: RetryPolicy,
    limiter: Arc<RateLimiter>,
}

impl HttpOutbound {
    #[must_use]
    pub fn new(client: HttpClient, retry: RetryPolicy, limiter: Arc<RateLimiter>) -> Self {
        Self {
            client,
            retry,
            limiter,
        }
    }
}

#[async_trait]
impl OutboundHttp for HttpOutbound {
    async fn send(&self, request: HttpRequest) -> Result<OutboundResponse, AppError> {
        let mut attempt = 1;
        loop {
            let wait = self.limiter.delay_before_next(Instant::now());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }

            match self.client.send(&request).await {
                Ok(response) if (200..300).contains(&response.status) => {
                    return Ok(OutboundResponse {
                        status: response.status,
                        raw_hash: hash(&response.body),
                        body: response.body,
                    });
                }
                Ok(response) => {
                    if let Retry::After(delay) = self
                        .retry
                        .decide(attempt, &Outcome::Status(response.status))
                    {
                        tokio::time::sleep(delay).await;
                        attempt = attempt.saturating_add(1);
                        continue;
                    }
                    return Err(AppError::Store(format!(
                        "рыночный источник вернул HTTP {}",
                        response.status
                    )));
                }
                Err(error) => {
                    let retry = match &error {
                        HttpError::Network => self
                            .retry
                            .decide(attempt, &Outcome::Transport(HttpError::Network)),
                        HttpError::Timeout => self
                            .retry
                            .decide(attempt, &Outcome::Transport(HttpError::Timeout)),
                        HttpError::ClientNotBuilt(message) => self.retry.decide(
                            attempt,
                            &Outcome::Transport(HttpError::ClientNotBuilt(message.clone())),
                        ),
                        HttpError::TrustAnchorNotParsed(message) => self.retry.decide(
                            attempt,
                            &Outcome::Transport(HttpError::TrustAnchorNotParsed(message.clone())),
                        ),
                    };
                    if let Retry::After(delay) = retry {
                        tokio::time::sleep(delay).await;
                        attempt = attempt.saturating_add(1);
                        continue;
                    }
                    return Err(AppError::Store(format!("рыночный транспорт: {error}")));
                }
            }
        }
    }
}

fn hash(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
