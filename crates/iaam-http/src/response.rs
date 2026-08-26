//! Ответ и отказы транспорта.

use thiserror::Error;

/// Ответ узла: код и тело как есть.
///
/// Тело не разбирается и не перекодируется здесь: ЦБ отвечает
/// в `windows-1251`, MOEX — в UTF-8, и знание об этом принадлежит
/// крейте источника, а не транспорту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Тело как строка UTF-8. Источники с иной кодировкой этим методом
    /// не пользуются — они берут `body` и перекодируют сами.
    #[must_use]
    pub fn text_utf8(&self) -> Option<&str> {
        core::str::from_utf8(&self.body).ok()
    }
}

/// Отказ транспорта.
///
/// Варианты не несут ни тела ответа, ни предъявленного секрета:
/// классификация ответа по смыслу принадлежит источнику, а отказ
/// транспорта не должен превращаться в утечку.
#[derive(Debug, Error)]
pub enum HttpError {
    #[error("сетевой отказ")]
    Network,
    #[error("истекло время ожидания ответа")]
    Timeout,
    #[error("клиент не собран: {0}")]
    ClientNotBuilt(String),
    #[error("вшитый корень доверия не разобран: {0}")]
    TrustAnchorNotParsed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_utf8_returns_the_exact_valid_body() {
        let response = HttpResponse {
            status: 200,
            body: "ответ".as_bytes().to_vec(),
        };

        assert_eq!(response.text_utf8(), Some("ответ"));
    }

    #[test]
    fn text_utf8_rejects_invalid_utf8() {
        let response = HttpResponse {
            status: 200,
            body: vec![0xff, 0xfe],
        };

        assert_eq!(response.text_utf8(), None);
    }
}
