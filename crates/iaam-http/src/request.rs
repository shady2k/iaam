//! Описание исходящего запроса (§3.1).
//!
//! Описание запроса — данные, а не действие: оно строится и проверяется
//! без сети, и именно поэтому крейты источников могут не знать транспорта
//! вовсе. Отправкой занимается `HttpClient`.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use zeroize::Zeroizing;

use crate::destination::Destination;

/// Метод запроса. Расширяется по потребности: варианты, которых ни один
/// источник не использует, — это непроверенный код.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// Тело запроса вместе с типом содержимого: тип нельзя забыть выставить,
/// потому что он не отделён от тела.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestBody {
    Json(String),
    /// Конверт SOAP тоже приходит сюда: отдельного варианта он не требует,
    /// от XML его отличает лишь заголовок `SOAPAction` (см. `soap_action`).
    Xml(String),
}

impl RequestBody {
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        match self {
            Self::Json(_) => "application/json",
            Self::Xml(_) => "text/xml; charset=utf-8",
        }
    }

    #[must_use]
    pub fn payload(&self) -> &str {
        match self {
            Self::Json(body) | Self::Xml(body) => body,
        }
    }
}

/// Предъявляемый секрет.
///
/// `Debug` написан вручную и печатает заглушку. Производный `Debug`
/// напечатал бы токен в первом же логе отказа, а `Zeroizing` затирает
/// копию при уничтожении.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    #[must_use]
    pub fn new(value: &str) -> Self {
        Self(Zeroizing::new(value.to_owned()))
    }

    /// Единственная точка, где секрет обращается в строку. Названа так,
    /// чтобы вызов был заметен на ревью.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(<скрыт>)")
    }
}

/// Полное описание исходящего запроса.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    destination: Destination,
    method: HttpMethod,
    path: String,
    query: Vec<(String, String)>,
    body: Option<RequestBody>,
    bearer: Option<Secret>,
    soap_action: Option<String>,
}

impl HttpRequest {
    #[must_use]
    pub fn get(destination: Destination, path: &str) -> Self {
        Self::new(destination, HttpMethod::Get, path, None)
    }

    #[must_use]
    pub fn post(destination: Destination, path: &str, body: RequestBody) -> Self {
        Self::new(destination, HttpMethod::Post, path, Some(body))
    }

    fn new(
        destination: Destination,
        method: HttpMethod,
        path: &str,
        body: Option<RequestBody>,
    ) -> Self {
        Self {
            destination,
            method,
            path: path.to_owned(),
            query: Vec::new(),
            body,
            bearer: None,
            soap_action: None,
        }
    }

    #[must_use]
    pub fn with_query(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.to_owned(), value.to_owned()));
        self
    }

    #[must_use]
    pub fn with_bearer(mut self, token: &str) -> Self {
        self.bearer = Some(Secret::new(token));
        self
    }

    /// Заголовок `SOAPAction`. Нужен ЦБ РФ: без него сервис отвечает
    /// отказом, а не ошибкой разбора, и причина неочевидна.
    #[must_use]
    pub fn with_soap_action(mut self, action: &str) -> Self {
        self.soap_action = Some(action.to_owned());
        self
    }

    #[must_use]
    pub const fn destination(&self) -> Destination {
        self.destination
    }

    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    #[must_use]
    pub const fn body(&self) -> Option<&RequestBody> {
        self.body.as_ref()
    }

    #[must_use]
    pub const fn bearer(&self) -> Option<&Secret> {
        self.bearer.as_ref()
    }

    #[must_use]
    pub fn soap_action(&self) -> Option<&str> {
        self.soap_action.as_deref()
    }

    /// Полный URL запроса.
    #[must_use]
    pub fn url(&self) -> String {
        let base = self.destination.base_url().trim_end_matches('/');
        let path = self.path.trim_start_matches('/');
        let mut url = format!("{base}/{path}");
        if !self.query.is_empty() {
            url.push('?');
            let encoded: Vec<String> = self
                .query
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        utf8_percent_encode(key, NON_ALPHANUMERIC),
                        utf8_percent_encode(value, NON_ALPHANUMERIC)
                    )
                })
                .collect();
            url.push_str(&encoded.join("&"));
        }
        url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_joins_base_and_path_without_doubling_the_slash() {
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");
        assert_eq!(request.url(), "https://iss.moex.com/iss/history.json");
    }

    #[test]
    fn an_empty_query_leaves_no_dangling_question_mark() {
        let request = HttpRequest::get(Destination::MoexIss, "/iss/history.json");
        assert!(!request.url().contains('?'));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        let request = HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp")
            .with_query("name", "Австралийский доллар")
            .with_query("range", "a b");
        let url = request.url();
        assert!(!url.contains(' '), "пробел обязан быть экранирован: {url}");
        assert!(
            !url.contains('Д'),
            "кириллица обязана быть экранирована: {url}"
        );
        assert!(url.contains("range=a%20b"), "{url}");
    }

    #[test]
    fn a_bearer_secret_never_appears_in_debug_output() {
        let request = HttpRequest::post(
            Destination::TinkoffProd,
            "/OperationsService/GetOperationsByCursor",
            RequestBody::Json("{}".to_owned()),
        )
        .with_bearer("t.SUPER-SECRET-VALUE");
        let printed = format!("{request:?}");
        assert!(
            !printed.contains("SUPER-SECRET-VALUE"),
            "секрет утёк в Debug: {printed}"
        );
    }

    #[test]
    fn request_body_payload_preserves_json_and_xml_contents() {
        let json = RequestBody::Json(r#"{"cursor":7}"#.to_owned());
        let xml = RequestBody::Xml("<Envelope/>".to_owned());

        assert_eq!(json.payload(), r#"{"cursor":7}"#);
        assert_eq!(xml.payload(), "<Envelope/>");
    }

    #[test]
    fn secret_expose_returns_the_original_token() {
        let secret = Secret::new("token-value");

        assert_eq!(secret.expose(), "token-value");
    }

    #[test]
    fn a_bearer_request_retains_its_token_for_transport() {
        let request = HttpRequest::get(Destination::MoexIss, "/").with_bearer("bearer-token");

        assert_eq!(request.bearer().map(Secret::expose), Some("bearer-token"));
    }

    #[test]
    fn secret_debug_is_redacted_but_not_empty() {
        assert_eq!(
            format!("{:?}", Secret::new("token-value")),
            "Secret(<скрыт>)"
        );
    }

    #[test]
    fn the_sandbox_is_a_different_host_not_a_different_path() {
        assert_ne!(
            Destination::TinkoffProd.base_url(),
            Destination::TinkoffSandbox.base_url()
        );
        assert!(Destination::TinkoffSandbox.base_url().contains("sandbox"));
        assert!(!Destination::TinkoffProd.base_url().contains("sandbox"));
    }

    #[test]
    fn every_destination_serves_https() {
        for destination in Destination::ALL {
            assert!(
                destination.base_url().starts_with("https://"),
                "{destination:?} ходит не по HTTPS"
            );
        }
    }
}
