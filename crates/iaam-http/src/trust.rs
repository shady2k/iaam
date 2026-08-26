//! Якорь доверия задаётся здесь и только здесь (§14).
//!
//! Политика доверия объявлена **одной таблицей назначений**, а не
//! рассыпана по крейтам источников. «Глобально» здесь означает единство
//! управления, а не слияние якорей: вшитый корень применяется ровно
//! к тому узлу, ради которого он вшит.
//!
//! Причина, по которой у Т-Инвестиций якорь свой: корень Минцифры
//! отсутствует в общедоступных хранилищах, и пиннинг был единственным
//! способом соединиться. У MOEX (ZeroSSL) и ЦБ (HARICA) сертификаты
//! публичных центров — вшивать там нечего, а пиннинг публичного
//! DV-центра ломался бы при смене выпускающего и не покупал бы ничего.
//!
//! Проверка подлинности не отключается ни для одного назначения.
//! Меняется только то, откуда берётся якорь.

use reqwest::{Certificate, Client};

use crate::destination::Destination;
use crate::response::HttpError;

/// Корневой сертификат Минцифры.
///
/// `include_str!`, а не чтение файла при запуске: файл на диске рядом
/// с программой подменить проще, чем содержимое двоичного файла,
/// а якорь доверия — ровно то, что подменяют в первую очередь.
pub const RUSSIAN_TRUSTED_ROOT_CA_PEM: &str = include_str!("../certs/russian-trusted-root-ca.pem");

/// Считает сертификаты в произвольной PEM-связке.
fn certificate_count_in_pem(pem: &str) -> usize {
    pem.matches("BEGIN CERTIFICATE").count()
}

/// Сколько сертификатов лежит в PEM-связке.
#[must_use]
pub fn certificate_count(pem: &str) -> usize {
    certificate_count_in_pem(pem)
}

/// Откуда берётся якорь для назначения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchors {
    /// Общедоступные корни. Узел подписан публичным центром.
    WebRoots,
    /// Ровно один вшитый корень; веб-корни выключены.
    Pinned(&'static str),
}

/// Клиент, собранный с выбранной политикой доверия.
#[derive(Clone)]
pub(crate) struct ConfiguredClient(pub(crate) Client);

/// Якорь доверия назначения.
///
/// `impl` живёт здесь, а не рядом с объявлением `Destination`: база узла
/// нужна для сборки URL и не имеет отношения к доверию, а якорь нужен
/// только при сборке клиента. Разные вопросы — разные модули; крейта
/// та же, так что дополнительный `impl` законен.
impl Destination {
    #[must_use]
    pub const fn anchors(self) -> Anchors {
        match self {
            // Обе среды шлюза — один удостоверяющий центр.
            Self::TinkoffProd | Self::TinkoffSandbox => {
                Anchors::Pinned(RUSSIAN_TRUSTED_ROOT_CA_PEM)
            }
            Self::FinamApi | Self::MoexIss | Self::CbrScripts | Self::CbrDailyInfo => {
                Anchors::WebRoots
            }
        }
    }
}

fn client_anchors(destination: Destination) -> Anchors {
    destination.anchors()
}

/// Собирает клиента под якорь назначения.
pub(crate) fn client_for(destination: Destination) -> Result<ConfiguredClient, HttpError> {
    let anchors = client_anchors(destination);
    let builder = Client::builder().tls_backend_rustls();
    let builder = match anchors {
        Anchors::WebRoots => builder,
        Anchors::Pinned(pem) => {
            let root = Certificate::from_pem(pem.as_bytes())
                .map_err(|error| HttpError::TrustAnchorNotParsed(error.to_string()))?;
            // Именно `only`, а не `merge`: `merge` добавил бы наш корень
            // к веб-корням, и клиент продолжил бы доверять всему
            // публичному интернету ради узла, которому это не нужно.
            builder.tls_certs_only([root])
        }
    };
    let client = builder
        .build()
        .map_err(|error| HttpError::ClientNotBuilt(error.to_string()))?;
    Ok(ConfiguredClient(client))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_count_counts_each_certificate_in_a_pem_bundle() {
        assert_eq!(certificate_count_in_pem(""), 0);
        assert_eq!(
            certificate_count_in_pem(
                "-----BEGIN CERTIFICATE-----\nfirst\n-----END CERTIFICATE-----"
            ),
            1
        );
        assert_eq!(
            certificate_count_in_pem(
                "-----BEGIN CERTIFICATE-----\nfirst\n-----END CERTIFICATE-----\n\
                 -----BEGIN CERTIFICATE-----\nsecond\n-----END CERTIFICATE-----"
            ),
            2
        );
    }

    #[test]
    fn client_build_uses_the_destination_trust_policy() {
        assert_eq!(
            client_anchors(Destination::TinkoffProd),
            Anchors::Pinned(RUSSIAN_TRUSTED_ROOT_CA_PEM)
        );
        assert_eq!(client_anchors(Destination::MoexIss), Anchors::WebRoots);
    }
}
