//! Доступ к брокеру: шифрование и права (§14).

use iaam_broker::credentials::{BrokerScope, CryptoError, Key, SealedToken, open, seal};

const SECRET: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";

fn key(seed: u8) -> Key {
    Key::from_bytes([seed; 32])
}

#[test]
fn a_sealed_token_opens_back_into_the_same_secret() {
    let key = key(1);
    let sealed = seal(&key, SECRET);

    let opened = open(&key, &sealed).unwrap();
    assert_eq!(opened.expose(), SECRET);
}

#[test]
fn the_same_token_sealed_twice_looks_different() {
    // Одинаковый шифротекст на одинаковый секрет означает, что nonce
    // постоянен, а постоянный nonce у потокового шифра — это способ
    // восстановить открытый текст по двум записям.
    let key = key(2);
    let first = seal(&key, SECRET);
    let second = seal(&key, SECRET);

    assert_ne!(first.nonce(), second.nonce());
    assert_ne!(first.ciphertext(), second.ciphertext());
}

#[test]
fn opening_with_a_wrong_key_fails() {
    let sealed = seal(&key(3), SECRET);
    assert_eq!(
        open(&key(4), &sealed).unwrap_err(),
        CryptoError::NotAuthentic
    );
}

#[test]
fn a_tampered_ciphertext_is_refused() {
    // Подделка обязана быть отказом, а не расшифровкой в мусор:
    // мусор, попавший в заголовок запроса, — это утечка поведения.
    let key = key(5);
    let sealed = seal(&key, SECRET);
    let mut bytes = sealed.ciphertext().to_vec();
    bytes[0] ^= 0xff;
    let tampered = SealedToken::of(sealed.nonce().to_vec(), bytes);

    assert_eq!(
        open(&key, &tampered).unwrap_err(),
        CryptoError::NotAuthentic
    );
}

#[test]
fn debug_never_prints_the_secret() {
    // Секрет утекает в лог через `Debug` чаще, чем через ответ API.
    let key = key(6);
    let sealed = seal(&key, SECRET);
    let opened = open(&key, &sealed).unwrap();

    for printed in [
        format!("{sealed:?}"),
        format!("{opened:?}"),
        format!("{key:?}"),
    ] {
        assert!(!printed.contains(SECRET), "секрет напечатан: {printed}");
        assert!(
            !printed.contains("secret-broker-token"),
            "секрет напечатан: {printed}"
        );
    }
}

#[test]
fn an_error_message_never_carries_the_secret() {
    let sealed = seal(&key(7), SECRET);
    let error = open(&key(8), &sealed).unwrap_err();
    let text = error.to_string();

    assert!(!text.contains(SECRET), "секрет в тексте ошибки: {text}");
}

#[test]
fn a_key_must_be_thirty_two_bytes() {
    // Короткий ключ — это не «слабее», а другой алгоритм, которого
    // здесь нет. Дополнять его нулями значит выдумывать ключ.
    assert_eq!(
        Key::from_base64("c2hvcnQ=").unwrap_err(),
        CryptoError::KeyLength { found: 5 }
    );
    assert!(matches!(
        Key::from_base64("не base64"),
        Err(CryptoError::KeyNotBase64)
    ));
}

#[test]
fn a_missing_key_variable_is_an_error_not_a_default_key() {
    // Умолчания у ключа шифрования нет и быть не может: ключ
    // «по умолчанию» известен всем, кто читал исходники.
    let absent = "IAAM_TEST_KEY_THAT_IS_NOT_SET";
    assert_eq!(
        Key::from_env(absent).unwrap_err(),
        CryptoError::KeyMissing {
            variable: absent.to_owned()
        }
    );
}

#[test]
fn only_read_only_access_is_accepted() {
    // Торговые права не запрашиваются ни при каких условиях (§14),
    // и строка, обещающая их, не толкуется как доступ.
    assert_eq!(BrokerScope::parse("read_only"), Some(BrokerScope::ReadOnly));
    assert_eq!(BrokerScope::ReadOnly.code(), "read_only");
    for refused in ["full_access", "trade", "read_write", "", "READ_ONLY "] {
        assert_eq!(BrokerScope::parse(refused), None, "принято: {refused}");
    }
}
