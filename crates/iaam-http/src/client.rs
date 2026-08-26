//! Отправка запроса.
//!
//! Клиент на назначение собирается один раз: сборка клиента `reqwest`
//! поднимает пул соединений и разбирает якорь доверия, и делать это
//! на каждый запрос значит терять и то, и другое.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::destination::Destination;
use crate::request::{HttpMethod, HttpRequest};
use crate::response::{HttpError, HttpResponse};
use crate::trust::{ConfiguredClient, client_for};

/// Предел ожидания ответа.
///
/// Задан явно: у `reqwest` таймаута по умолчанию нет, и его отсутствие
/// превратило бы зависший узел в вечно висящее фоновое задание.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Клиент исходящих запросов.
pub struct HttpClient {
    pool: Mutex<HashMap<Destination, ConfiguredClient>>,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn client_for(
        &self,
        destination: Destination,
    ) -> Result<ConfiguredClient, HttpError> {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = pool.get(&destination) {
            return Ok(existing.clone());
        }
        let built = client_for(destination)?;
        pool.insert(destination, built.clone());
        Ok(built)
    }

    #[cfg(test)]
    pub(crate) fn pool_len(&self) -> usize {
        self.pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Отправляет запрос и возвращает код с телом.
    ///
    /// Код ответа **не классифицируется** здесь: смысл 401 у шлюза
    /// брокера и у биржи разный, и трактовка принадлежит источнику.
    pub async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let client = self.client_for(request.destination())?;
        let mut builder = match request.method() {
            HttpMethod::Get => client.0.get(request.url()),
            HttpMethod::Post => client.0.post(request.url()),
        };
        builder = builder.timeout(REQUEST_TIMEOUT);
        if let Some(secret) = request.bearer() {
            builder = builder.bearer_auth(secret.expose());
        }
        if let Some(action) = request.soap_action() {
            builder = builder.header("SOAPAction", format!("\"{action}\""));
        }
        if let Some(body) = request.body() {
            builder = builder
                .header("Content-Type", body.content_type())
                .body(body.payload().to_owned());
        }
        let response = builder.send().await.map_err(classify_transport_error)?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(classify_transport_error)?
            .to_vec();
        Ok(HttpResponse { status, body })
    }
}

fn classify_transport_error(error: reqwest::Error) -> HttpError {
    if error.is_timeout() {
        HttpError::Timeout
    } else {
        HttpError::Network
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RequestBody;

    #[test]
    fn a_client_is_built_once_per_destination() {
        let client = HttpClient::new();
        let first = client.pool_len();
        let _ = client.client_for(Destination::MoexIss).expect("клиент");
        let _ = client.client_for(Destination::MoexIss).expect("клиент");
        assert_eq!(first, 0);
        assert_eq!(client.pool_len(), 1, "второй запрос собрал второй клиент");
    }

    #[test]
    fn distinct_destinations_get_distinct_clients() {
        let client = HttpClient::new();
        let _ = client.client_for(Destination::MoexIss).expect("клиент");
        let _ = client.client_for(Destination::TinkoffProd).expect("клиент");
        assert_eq!(client.pool_len(), 2);
    }

    #[test]
    fn the_two_gateway_environments_do_not_share_a_client() {
        let client = HttpClient::new();
        let _ = client.client_for(Destination::TinkoffProd).expect("клиент");
        let _ = client
            .client_for(Destination::TinkoffSandbox)
            .expect("клиент");
        assert_eq!(
            client.pool_len(),
            2,
            "песочница и бой — разные хосты, общий клиент увёл бы запрос не туда"
        );
    }

    #[test]
    fn a_soap_request_carries_its_action_header() {
        let request = HttpRequest::post(
            Destination::CbrDailyInfo,
            "/DailyInfoWebServ/DailyInfo.asmx",
            RequestBody::Xml("<soap:Envelope/>".to_owned()),
        )
        .with_soap_action("http://web.cbr.ru/KeyRateXML");
        assert_eq!(request.soap_action(), Some("http://web.cbr.ru/KeyRateXML"));
        assert_eq!(
            request.body().map(RequestBody::content_type),
            Some("text/xml; charset=utf-8")
        );
    }
}
