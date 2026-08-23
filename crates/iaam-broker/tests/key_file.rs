//! Ключ шифрования доступа как файл (§14).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use iaam_broker::credentials::{CryptoError, Key, open, seal};

const SECRET: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn create(label: &str) -> Self {
        let nonce: u128 = u128::from_le_bytes(
            *b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10",
        );
        let path = std::env::temp_dir().join(format!("iaam-key-{label}-{nonce}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("каталог под ключ");
        Self { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn a_generated_key_opens_what_it_sealed() {
    let directory = TempDir::create("roundtrip");
    let path = directory.file("broker-key");

    Key::create_at(&path).unwrap();
    let key = Key::from_file(&path).unwrap();

    let sealed = seal(&key, SECRET);
    assert_eq!(open(&key, &sealed).unwrap().expose(), SECRET);
}

#[test]
fn two_generated_keys_are_not_the_same_key() {
    // Генератор, дающий один и тот же ключ, превратил бы шифрование
    // в кодирование: файл базы открывался бы чужой копией программы.
    let directory = TempDir::create("distinct");
    let first = directory.file("first");
    let second = directory.file("second");
    Key::create_at(&first).unwrap();
    Key::create_at(&second).unwrap();

    let sealed = seal(&Key::from_file(&first).unwrap(), SECRET);
    assert_eq!(
        open(&Key::from_file(&second).unwrap(), &sealed).unwrap_err(),
        CryptoError::NotAuthentic
    );
}

#[test]
fn an_existing_key_file_is_never_overwritten() {
    // Перезапись ключа делает нечитаемыми все ранее зашифрованные
    // доступы — молча и необратимо.
    let directory = TempDir::create("keep");
    let path = directory.file("broker-key");
    Key::create_at(&path).unwrap();
    let sealed = seal(&Key::from_file(&path).unwrap(), SECRET);

    let error = Key::create_at(&path).unwrap_err();
    assert!(matches!(error, CryptoError::KeyFileExists { .. }));
    assert_eq!(
        open(&Key::from_file(&path).unwrap(), &sealed)
            .unwrap()
            .expose(),
        SECRET,
        "прежний ключ остался на месте"
    );
}

#[test]
fn a_key_file_is_readable_only_by_its_owner() {
    // Ключ, доступный на чтение группе или всем, защищает ровно ни от
    // кого на общей машине.
    let directory = TempDir::create("mode");
    let path = directory.file("broker-key");
    Key::create_at(&path).unwrap();

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "режим доступа к ключу: {mode:o}");
}

#[test]
fn a_missing_key_file_is_an_error_not_a_default_key() {
    let directory = TempDir::create("missing");
    let path = directory.file("нет-такого");

    assert!(matches!(
        Key::from_file(&path),
        Err(CryptoError::KeyFileUnreadable { .. })
    ));
}

#[test]
fn a_key_file_that_is_not_a_key_is_refused() {
    let directory = TempDir::create("rubbish");
    let path = directory.file("broker-key");
    fs::write(&path, "это не ключ").unwrap();

    assert!(matches!(
        Key::from_file(&path),
        Err(CryptoError::KeyNotBase64)
    ));
}
