//! Корень доверия шлюза брокера (§14).

use iaam_http::Destination;
use iaam_http::trust::{Anchors, RUSSIAN_TRUSTED_ROOT_CA_PEM, certificate_count};
use sha2::{Digest, Sha256};

/// Отпечаток корня, посчитанный **вне программы** (§15.5):
///
/// ```text
/// sha256sum crates/iaam-http/certs/russian-trusted-root-ca.pem
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
    assert_eq!(certificate_count(RUSSIAN_TRUSTED_ROOT_CA_PEM), 1);
}

#[test]
fn a_pinned_anchor_is_used_only_for_the_gateways_that_need_it() {
    for pinned in [Destination::TinkoffProd, Destination::TinkoffSandbox] {
        assert!(
            matches!(pinned.anchors(), Anchors::Pinned(_)),
            "{pinned:?} обязан ходить на вшитом корне: Минцифры нет в общедоступных хранилищах"
        );
    }
    for public in [
        Destination::FinamApi,
        Destination::MoexIss,
        Destination::CbrScripts,
        Destination::CbrDailyInfo,
    ] {
        assert!(
            matches!(public.anchors(), Anchors::WebRoots),
            "{public:?} не должен ходить на вшитом корне: он подписан публичным центром"
        );
    }
}

#[test]
fn certificate_count_matches_the_number_of_pem_certificates() {
    assert_eq!(certificate_count(""), 0);
    assert_eq!(
        certificate_count("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----"),
        1
    );
    assert_eq!(
        certificate_count(
            "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n\
             -----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----"
        ),
        2
    );
}
