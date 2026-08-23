//! Корень доверия шлюза брокера (§14).

use iaam_broker::trust::{
    RUSSIAN_TRUSTED_ROOT_CA_PEM, certificate_count, tinkoff_client, tls_root,
};
use sha2::{Digest, Sha256};

/// Отпечаток корня, посчитанный **вне программы** (§15.5):
///
/// ```text
/// sha256sum crates/iaam-broker/certs/russian-trusted-root-ca.pem
/// ```
///
/// Если тест упал, значит якорь доверия подменили. Чинить его
/// подстановкой нового значения нельзя: корень меняют отдельным
/// коммитом с обоснованием, а не мимоходом (§15.7).
const FROZEN_ROOT_SHA256: &str = "936a43fea6e8e525bcc0f81acd9c3d21b4fc4b9b68acea7906d698005afc6504";

#[test]
fn the_embedded_root_is_the_frozen_one() {
    let digest = Sha256::digest(RUSSIAN_TRUSTED_ROOT_CA_PEM.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();

    assert_eq!(hex, FROZEN_ROOT_SHA256, "якорь доверия подменён");
}

#[test]
fn only_the_root_is_embedded() {
    // Промежуточный сертификат сервер присылает сам. Лишний якорь —
    // это лишнее доверие и вторая дата истечения.
    assert_eq!(certificate_count(), 1);
}

#[test]
fn the_root_parses_into_a_trust_anchor() {
    // Якорь обязан быть не просто текстом, а разбираемым сертификатом:
    // текст, который не разобрался, отказал бы уже в бою.
    assert!(tls_root().is_ok());
}

#[test]
fn the_tinkoff_client_is_built_with_that_root() {
    // Клиент собирается без обращения к сети: сборка, падающая только
    // при первом запросе, отложила бы отказ на самое неудобное время.
    assert!(tinkoff_client().is_ok());
}
