//! Broker access: encryption and permissions (§14).

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
    // Equal ciphertext for equal secrets would mean a constant nonce, and a
    // constant nonce in a stream cipher can recover plaintext from two records.
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
    // Tampering must be a refusal, not decryption into garbage: garbage in a
    // request header is a leak of behavior.
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
    // Secrets leak into logs through `Debug` more often than through an API response.
    let key = key(6);
    let sealed = seal(&key, SECRET);
    let opened = open(&key, &sealed).unwrap();

    for printed in [
        format!("{sealed:?}"),
        format!("{opened:?}"),
        format!("{key:?}"),
    ] {
        assert!(!printed.contains(SECRET), "secret was printed: {printed}");
        assert!(
            !printed.contains("secret-broker-token"),
            "secret was printed: {printed}"
        );
    }
}

#[test]
fn an_error_message_never_carries_the_secret() {
    let sealed = seal(&key(7), SECRET);
    let error = open(&key(8), &sealed).unwrap_err();
    let text = error.to_string();

    assert!(!text.contains(SECRET), "secret in error text: {text}");
}

#[test]
fn a_key_must_be_thirty_two_bytes() {
    // A short key is not “weaker” but a different algorithm, which this code
    // does not have. Padding it with zeroes would invent a key.
    assert_eq!(
        Key::from_base64("c2hvcnQ=").unwrap_err(),
        CryptoError::KeyLength { found: 5 }
    );
    assert!(matches!(
        Key::from_base64("not base64"),
        Err(CryptoError::KeyNotBase64)
    ));
}

#[test]
fn a_missing_key_variable_is_an_error_not_a_default_key() {
    // There can be no encryption-key default: a “default” key would be known
    // to everyone who read the source.
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
    // Trading permissions are never requested (§14), and a string promising
    // them is not interpreted as access.
    assert_eq!(BrokerScope::parse("read_only"), Some(BrokerScope::ReadOnly));
    assert_eq!(BrokerScope::ReadOnly.code(), "read_only");
    for refused in ["full_access", "trade", "read_write", "", "READ_ONLY "] {
        assert_eq!(BrokerScope::parse(refused), None, "accepted: {refused}");
    }
}
