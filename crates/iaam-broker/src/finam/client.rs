use crate::credentials::BrokerToken;
use iaam_http::client::HttpClient;
use iaam_http::{Destination, HttpRequest};
use serde_json::Value;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, Time};

/// Ошибки HTTP-доступа к Finam Trade API.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FinamError {
    #[error("сетевой отказ шлюза Finam")]
    Network,
    #[error("шлюз Finam ограничил частоту запросов")]
    RateLimited,
    #[error("токен Finam недействителен")]
    InvalidToken,
    #[error("неожиданный код HTTP {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },
    #[error("ответ Finam с пагинацией оборван: отсутствует токен следующей страницы")]
    PartialResponse,
    #[error("успешный ответ Finam не соответствует JSON-схеме")]
    MalformedResponse,
}

/// HTTP-клиент Finam, возвращающий сырые тела ответов.
pub struct FinamClient {
    token: BrokerToken,
    http: HttpClient,
}

impl FinamClient {
    /// Создаёт клиент; токен остаётся в зануляемой обёртке.
    #[must_use]
    pub fn new(token: BrokerToken) -> Self {
        Self {
            token,
            http: HttpClient::new(),
        }
    }

    /// Возвращает сырое тело текущего портфеля счёта.
    pub async fn get_portfolio(&self, account_id: &str) -> Result<String, FinamError> {
        self.get(&format!("/v1/accounts/{account_id}"), &[]).await
    }

    /// Возвращает сырое тело страницы транзакций за интервал.
    pub async fn get_transactions(
        &self,
        account_id: &str,
        from: Date,
        to: Date,
    ) -> Result<String, FinamError> {
        let query = [
            ("interval.start_time", rfc3339_midnight(from)),
            ("interval.end_time", rfc3339_midnight(to)),
        ];
        let body = self
            .get(&format!("/v1/accounts/{account_id}/transactions"), &query)
            .await?;
        validate_transactions_page(&body)?;
        Ok(body)
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, FinamError> {
        let mut request =
            HttpRequest::get(Destination::FinamApi, path).with_bearer(self.token.expose());
        for (key, value) in query {
            request = request.with_query(key, value);
        }
        let response = self
            .http
            .send(&request)
            .await
            .map_err(|_| FinamError::Network)?;
        let status = response.status;
        let body = String::from_utf8(response.body).map_err(|_| FinamError::MalformedResponse)?;
        classify_response(status, &body, self.token.expose())?;
        Ok(body)
    }
}

fn rfc3339_midnight(date: Date) -> String {
    OffsetDateTime::new_utc(date, Time::MIDNIGHT)
        .format(&Rfc3339)
        .unwrap_or_else(|_| format!("{date}T00:00:00Z"))
}

fn classify_response(status: u16, body: &str, token: &str) -> Result<(), FinamError> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(FinamError::InvalidToken),
        429 => Err(FinamError::RateLimited),
        status => Err(FinamError::UnexpectedStatus {
            status,
            body: redact_token(body, token),
        }),
    }
}

fn validate_transactions_page(body: &str) -> Result<(), FinamError> {
    let value: Value = serde_json::from_str(body).map_err(|_| FinamError::MalformedResponse)?;
    let has_more = value
        .get("hasMore")
        .or_else(|| value.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let next_page_token = value
        .get("nextPageToken")
        .or_else(|| value.get("next_page_token"))
        .and_then(Value::as_str);
    if has_more && next_page_token.is_none_or(str::is_empty) {
        return Err(FinamError::PartialResponse);
    }
    Ok(())
}

fn redact_token(body: &str, token: &str) -> String {
    if token.is_empty() {
        body.to_owned()
    } else {
        body.replace(token, "<токен скрыт>")
    }
}

#[cfg(test)]
mod tests {
    use super::{FinamError, classify_response, validate_transactions_page};

    #[test]
    fn classifies_auth_rate_limit_and_unexpected_statuses() {
        assert!(matches!(
            classify_response(401, "", "secret"),
            Err(FinamError::InvalidToken)
        ));
        assert!(matches!(
            classify_response(429, "", "secret"),
            Err(FinamError::RateLimited)
        ));
        assert!(matches!(
            classify_response(500, "failure", "secret"),
            Err(FinamError::UnexpectedStatus { status: 500, .. })
        ));
    }

    #[test]
    fn unexpected_status_never_prints_the_token() {
        let error = match classify_response(500, "upstream secret", "secret") {
            Ok(()) => panic!("HTTP 500 must be rejected"),
            Err(error) => error,
        };
        assert!(!error.to_string().contains("secret"));
        assert!(!format!("{error:?}").contains("secret"));
    }

    #[test]
    fn refuses_a_page_that_claims_more_without_a_token() {
        assert!(matches!(
            validate_transactions_page(r#"{"hasMore":true,"transactions":[]}"#),
            Err(FinamError::PartialResponse)
        ));
    }
}
