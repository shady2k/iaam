use crate::credentials::BrokerToken;
use crate::environment::{Environment, Method};
use crate::trust::{TrustError, tinkoff_client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

/// Ошибки HTTP-шлюза Т-Инвестиций.
///
/// Варианты не содержат токен: даже случайный `Debug` или `Display` ошибки
/// не должен превращать отказ удалённого узла в утечку доступа.
#[derive(Debug, Error)]
pub enum TinkoffError {
    /// Соединение не установлено или тело ответа не дочитано.
    #[error("сетевой отказ шлюза Т-Инвестиций")]
    Network,
    /// Шлюз временно ограничил частоту запросов.
    #[error("шлюз Т-Инвестиций ограничил частоту запросов")]
    RateLimited,
    /// Шлюз отверг предъявленный токен.
    #[error("токен Т-Инвестиций недействителен")]
    InvalidToken,
    /// В выбранной среде такого метода нет.
    #[error("метод {method:?} недоступен в среде {environment:?}")]
    MethodUnavailable {
        /// Метод, который нельзя вызвать в этой среде.
        method: Method,
        /// Выбранная среда шлюза.
        environment: Environment,
    },
    /// Шлюз ответил кодом, который клиент не умеет принять.
    #[error("неожиданный код HTTP {status}: {body}")]
    UnexpectedStatus {
        /// Код HTTP.
        status: u16,
        /// Тело ответа после удаления предъявленного токена.
        body: String,
    },
    /// Ответ с пагинацией не позволяет продолжить выгрузку.
    #[error("ответ с пагинацией оборван: шлюз сообщил следующий элемент без курсора")]
    PartialResponse,
    /// Успешный ответ не соответствует минимальной схеме метода.
    #[error("ответ шлюза Т-Инвестиций не разобран")]
    MalformedResponse,
    /// Запрос не удалось превратить в JSON до отправки.
    #[error("не удалось сериализовать запрос к шлюзу Т-Инвестиций")]
    RequestSerialization,
    /// Не удалось собрать HTTP-клиент с закреплённым корнем доверия.
    #[error(transparent)]
    Trust(#[from] TrustError),
}

/// Запрос страницы операций с курсорной пагинацией.
///
/// Даты и значения перечислений остаются строками: этот слой отвечает за
/// транспорт, а смысл полей и разбор операций принадлежат следующему слою.
#[derive(Debug, Clone, Serialize)]
pub struct GetOperationsByCursorRequest {
    /// Идентификатор счёта.
    #[serde(rename = "accountId")]
    pub account_id: String,
    /// FIGI или UID инструмента.
    #[serde(rename = "instrumentId", skip_serializing_if = "Option::is_none")]
    pub instrument_id: Option<String>,
    /// Начало периода по UTC в формате RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Окончание периода по UTC в формате RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Курсор начала страницы.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Ограничение размера страницы.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    /// Фильтр по типам операций.
    #[serde(rename = "operationTypes", skip_serializing_if = "Vec::is_empty")]
    pub operation_types: Vec<String>,
    /// Фильтр по состоянию операции.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Не возвращать комиссии.
    #[serde(rename = "withoutCommissions")]
    pub without_commissions: bool,
    /// Не возвращать сделки.
    #[serde(rename = "withoutTrades")]
    pub without_trades: bool,
    /// Не возвращать overnight-операции.
    #[serde(rename = "withoutOvernights")]
    pub without_overnights: bool,
}

impl GetOperationsByCursorRequest {
    /// Создаёт запрос первой страницы счёта с настройками шлюза по умолчанию.
    #[must_use]
    pub fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            instrument_id: None,
            from: None,
            to: None,
            cursor: None,
            limit: None,
            operation_types: Vec::new(),
            state: None,
            without_commissions: false,
            without_trades: false,
            without_overnights: false,
        }
    }
}

/// HTTP-клиент методов T-Invest REST API.
pub struct TinkoffClient {
    environment: Environment,
    token: BrokerToken,
    http: reqwest::Client,
}

impl TinkoffClient {
    /// Создаёт клиент с тем же закреплённым корнем доверия, что и probe.
    pub fn new(environment: Environment, token: BrokerToken) -> Result<Self, TinkoffError> {
        Ok(Self {
            environment,
            token,
            http: tinkoff_client()?,
        })
    }

    /// Возвращает сырое тело ответа `UsersService/GetAccounts`.
    pub async fn get_accounts(&self) -> Result<String, TinkoffError> {
        self.post(Method::Accounts, "UsersService/GetAccounts", json!({}))
            .await
    }

    /// Возвращает сырое тело ответа `OperationsService/GetPortfolio`.
    pub async fn get_portfolio(&self, account_id: &str) -> Result<String, TinkoffError> {
        self.post(
            Method::Portfolio,
            "OperationsService/GetPortfolio",
            json!({ "accountId": account_id }),
        )
        .await
    }

    /// Возвращает сырое тело страницы `OperationsService/GetOperationsByCursor`.
    pub async fn get_operations_by_cursor(
        &self,
        request: &GetOperationsByCursorRequest,
    ) -> Result<String, TinkoffError> {
        let body = self
            .post(
                Method::Operations,
                "OperationsService/GetOperationsByCursor",
                serde_json::to_value(request).map_err(|_| TinkoffError::RequestSerialization)?,
            )
            .await?;
        validate_cursor_page(&body)?;
        Ok(body)
    }

    async fn post(&self, method: Method, path: &str, body: Value) -> Result<String, TinkoffError> {
        ensure_method_available(self.environment, method)?;
        let response = self
            .http
            .post(method_url(self.environment, path))
            .bearer_auth(self.token.expose())
            .json(&body)
            .send()
            .await
            .map_err(|_| TinkoffError::Network)?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|_| TinkoffError::Network)?;
        classify_response_with_token(status, &body, self.token.expose())?;
        Ok(body)
    }
}

fn method_url(environment: Environment, path: &str) -> String {
    format!(
        "{}/{}",
        environment.base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn ensure_method_available(environment: Environment, method: Method) -> Result<(), TinkoffError> {
    if environment.serves(method) {
        Ok(())
    } else {
        Err(TinkoffError::MethodUnavailable {
            method,
            environment,
        })
    }
}

fn classify_response(status: u16, body: &str) -> Result<(), TinkoffError> {
    match status {
        429 => Err(TinkoffError::RateLimited),
        401 | 403 => Err(TinkoffError::InvalidToken),
        200..=299 => Ok(()),
        _ if body_contains_token_code(body) => Err(TinkoffError::InvalidToken),
        _ => Err(TinkoffError::UnexpectedStatus {
            status,
            body: body.to_owned(),
        }),
    }
}

fn classify_response_with_token(status: u16, body: &str, token: &str) -> Result<(), TinkoffError> {
    match classify_response(status, body) {
        Err(TinkoffError::UnexpectedStatus { status, .. }) => Err(TinkoffError::UnexpectedStatus {
            status,
            body: redact_token(body, token),
        }),
        result => result,
    }
}

fn body_contains_token_code(body: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str(body) else {
        return false;
    };
    ["description", "code", "message"]
        .iter()
        .any(|field| fields.get(*field).is_some_and(value_is_token_code))
}

fn value_is_token_code(value: &Value) -> bool {
    match value {
        Value::String(value) => value == "40003" || value == "70001",
        Value::Number(value) => value.to_string() == "40003" || value.to_string() == "70001",
        Value::Array(_) | Value::Object(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn redact_token(body: &str, token: &str) -> String {
    if token.is_empty() {
        body.to_owned()
    } else {
        body.replace(token, "<токен скрыт>")
    }
}

#[derive(Deserialize)]
struct CursorPage {
    #[serde(rename = "hasNext", alias = "has_next")]
    has_next: bool,
    #[serde(rename = "nextCursor", alias = "next_cursor")]
    next_cursor: Option<String>,
}

fn validate_cursor_page(body: &str) -> Result<(), TinkoffError> {
    let page: CursorPage =
        serde_json::from_str(body).map_err(|_| TinkoffError::MalformedResponse)?;
    if page.has_next && page.next_cursor.as_deref().is_none_or(str::is_empty) {
        return Err(TinkoffError::PartialResponse);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_gateway_statuses_and_token_codes() {
        assert!(matches!(
            classify_response(429, "{}"),
            Err(TinkoffError::RateLimited)
        ));
        assert!(matches!(
            classify_response(401, "{}"),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(matches!(
            classify_response(403, "{}"),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(matches!(
            classify_response(500, r#"{"description":"40003"}"#),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(matches!(
            classify_response(500, r#"{"description":"70001"}"#),
            Err(TinkoffError::InvalidToken)
        ));
        assert!(classify_response(200, r#"{"code":"40003"}"#).is_ok());
        assert!(classify_response(200, r#"{"positions":[{"quantity":70001}]}"#).is_ok());
        assert!(matches!(
            classify_response(500, r#"{"code":70001}"#),
            Err(TinkoffError::InvalidToken)
        ));
        let error = classify_response(500, r#"{"data":{"quantity":70001}}"#)
            .expect_err("вложенное значение не является кодом отказа");
        assert!(matches!(
            error,
            TinkoffError::UnexpectedStatus { status: 500, .. }
        ));
    }

    #[test]
    fn preserves_unexpected_status_body_without_treating_success_as_error() {
        assert!(classify_response(200, r#"{"ok":true}"#).is_ok());
        let error = classify_response(500, r#"{"message":"gateway failed"}"#)
            .expect_err("неожиданный код должен быть отказом");
        assert!(matches!(
            error,
            TinkoffError::UnexpectedStatus { status: 500, body }
                if body == r#"{"message":"gateway failed"}"#
        ));
    }

    #[test]
    fn refuses_methods_absent_from_the_selected_environment() {
        let error = ensure_method_available(Environment::Sandbox, Method::BrokerReport)
            .expect_err("отчёт отсутствует в песочнице");
        assert!(matches!(error, TinkoffError::MethodUnavailable { .. }));
        assert!(ensure_method_available(Environment::Sandbox, Method::Portfolio).is_ok());
    }

    #[test]
    fn builds_method_url_from_environment_base_url() {
        assert_eq!(
            method_url(Environment::Prod, "OperationsService/GetPortfolio"),
            "https://invest-public-api.tbank.ru/rest/OperationsService/GetPortfolio"
        );
        assert_eq!(
            method_url(Environment::Sandbox, "UsersService/GetAccounts"),
            "https://sandbox-invest-public-api.tbank.ru/rest/UsersService/GetAccounts"
        );
    }

    #[test]
    fn rejects_an_incomplete_cursor_page() {
        assert!(matches!(
            validate_cursor_page(r#"{"hasNext":true,"items":[]}"#),
            Err(TinkoffError::PartialResponse)
        ));
        assert!(matches!(
            validate_cursor_page(r#"{"hasNext":true,"nextCursor":""}"#),
            Err(TinkoffError::PartialResponse)
        ));
        assert!(validate_cursor_page(r#"{"hasNext":true,"nextCursor":"next"}"#).is_ok());
        assert!(validate_cursor_page(r#"{"hasNext":false,"items":[]}"#).is_ok());
    }

    #[test]
    fn error_text_never_contains_the_token() {
        let token = "secret-token-42";
        let error =
            classify_response_with_token(500, &format!(r#"{{"message":"{token}"}}"#), token)
                .expect_err("код ответа должен быть отказом");
        assert!(!error.to_string().contains(token));
    }
}
