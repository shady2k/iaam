//! Шифрование доступа к брокеру (§14).
//!
//! Утечка файла базы не должна давать доступа к брокерскому счёту,
//! поэтому токен лежит в базе только шифротекстом, а ключ живёт вне
//! базы — в окружении процесса.
//!
//! **Секрет не печатается никогда.** У всех типов этого модуля `Debug`
//! ручной: производный напечатал бы содержимое, и первый же `{:?}`
//! в логе отправил бы токен туда, откуда его не убрать.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, Generate, Key as AeadKey, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use thiserror::Error;
use zeroize::Zeroizing;

/// Длина ключа ChaCha20-Poly1305.
const KEY_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CryptoError {
    #[error("переменная окружения {variable} с ключом шифрования не задана")]
    KeyMissing { variable: String },
    #[error("ключ шифрования не является base64")]
    KeyNotBase64,
    #[error("ключ шифрования длиной {found} байт вместо 32")]
    KeyLength { found: usize },
    // Текст намеренно не содержит ни ключа, ни шифротекста: сообщение
    // об ошибке — это то, что точно попадёт в лог.
    #[error("доступ не расшифровывается: ключ не тот или запись подделана")]
    NotAuthentic,
    #[error("расшифрованный доступ не является текстом")]
    NotText,
    #[error("файл ключа {path} не читается: {detail}")]
    KeyFileUnreadable { path: String, detail: String },
    #[error(
        "файл ключа {path} уже существует: перезапись сделала бы нечитаемыми все заведённые доступы"
    )]
    KeyFileExists { path: String },
    #[error("файл ключа {path} не записан: {detail}")]
    KeyFileNotWritten { path: String, detail: String },
}

/// Ключ шифрования доступа. Живёт вне базы.
#[derive(Clone)]
pub struct Key {
    bytes: Zeroizing<[u8; KEY_BYTES]>,
}

impl Key {
    /// Ключ из готовых байтов. Нужен тестам и вызывающему, который
    /// берёт ключ не из окружения.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self {
            bytes: Zeroizing::new(bytes),
        }
    }

    /// Ключ из base64. Сырые 32 байта в переменной окружения не
    /// переживают ни один копипаст, поэтому ключ передаётся текстом.
    ///
    /// Ключ другой длины — отказ, а не дополнение нулями: дополненный
    /// ключ выглядит рабочим и защищает не тем, чем обещает.
    pub fn from_base64(encoded: &str) -> Result<Self, CryptoError> {
        let decoded = Zeroizing::new(
            STANDARD
                .decode(encoded.trim())
                .map_err(|_| CryptoError::KeyNotBase64)?,
        );
        let bytes: [u8; KEY_BYTES] =
            decoded
                .as_slice()
                .try_into()
                .map_err(|_| CryptoError::KeyLength {
                    found: decoded.len(),
                })?;
        Ok(Self::from_bytes(bytes))
    }

    /// Ключ из файла.
    ///
    /// Основной способ в бою: переменная окружения видна
    /// в `/proc/<pid>/environ` тому же пользователю и наследуется
    /// каждым дочерним процессом, а файл — нет.
    pub fn from_file(path: &Path) -> Result<Self, CryptoError> {
        let encoded = Zeroizing::new(fs::read_to_string(path).map_err(|error| {
            CryptoError::KeyFileUnreadable {
                path: path.display().to_string(),
                detail: error.to_string(),
            }
        })?);
        Self::from_base64(&encoded)
    }

    /// Заведение нового случайного ключа в файле.
    ///
    /// Возвращает `()`, а не ключ: то, чего вызывающий не получил, он
    /// не может ни напечатать, ни записать в лог. Ключ существует
    /// только в файле, доступном одному владельцу процесса.
    ///
    /// Существующий файл **не перезаписывается**: новый ключ на месте
    /// старого делает нечитаемыми все ранее заведённые доступы — молча
    /// и необратимо.
    pub fn create_at(path: &Path) -> Result<(), CryptoError> {
        // Ключ берётся у криптографического источника библиотеки шифра,
        // а не у общего генератора: ключ — это доступ к чужим деньгам,
        // и слабый источник здесь дороже всего остального в этом файле.
        let generated = AeadKey::<ChaCha20Poly1305>::generate();
        let mut material = Zeroizing::new([0_u8; KEY_BYTES]);
        material.copy_from_slice(&generated);
        let encoded = Zeroizing::new(STANDARD.encode(&material[..]));
        // Режим задаётся при создании, а не после: файл, на мгновение
        // доступный всем, доступен всем.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => CryptoError::KeyFileExists {
                    path: path.display().to_string(),
                },
                _ => CryptoError::KeyFileNotWritten {
                    path: path.display().to_string(),
                    detail: error.to_string(),
                },
            })?;
        io::Write::write_all(&mut file, encoded.as_bytes()).map_err(|error| {
            CryptoError::KeyFileNotWritten {
                path: path.display().to_string(),
                detail: error.to_string(),
            }
        })?;
        // Умаска могла срезать биты доступа при создании: режим
        // подтверждается явно, а не предполагается.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            CryptoError::KeyFileNotWritten {
                path: path.display().to_string(),
                detail: error.to_string(),
            }
        })?;
        Ok(())
    }

    /// Ключ из переменной окружения.
    ///
    /// Отсутствие переменной — отказ. Умолчания у ключа шифрования нет
    /// и быть не может: ключ «по умолчанию» известен каждому, кто читал
    /// исходники.
    pub fn from_env(variable: &str) -> Result<Self, CryptoError> {
        let encoded = env::var(variable).map_err(|_| CryptoError::KeyMissing {
            variable: variable.to_owned(),
        })?;
        Self::from_base64(&encoded)
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Key(<скрыт>)")
    }
}

/// Зашифрованный доступ: то, что лежит в базе.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedToken {
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

impl SealedToken {
    /// Сборка из байтов, прочитанных из хранилища.
    #[must_use]
    pub const fn of(nonce: Vec<u8>, ciphertext: Vec<u8>) -> Self {
        Self { nonce, ciphertext }
    }

    #[must_use]
    pub fn nonce(&self) -> &[u8] {
        &self.nonce
    }

    #[must_use]
    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }
}

impl fmt::Debug for SealedToken {
    /// Печатается длина, а не содержимое: шифротекст в логе бесполезен
    /// читателю и полезен тому, кто получит лог вместе с ключом.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealedToken")
            .field("nonce_len", &self.nonce.len())
            .field("ciphertext_len", &self.ciphertext.len())
            .finish()
    }
}

/// Расшифрованный доступ.
///
/// Обёртка над зануляемой памятью, а не голый `Zeroizing<String>`:
/// `Zeroizing` зануляет память при уничтожении, но его собственный
/// `Debug` печатает содержимое, и секрет утёк бы в лог через первый же
/// `{:?}`.
pub struct BrokerToken {
    secret: Zeroizing<String>,
}

impl BrokerToken {
    /// Значение токена. Имя длинное намеренно: строка `token.expose()`
    /// в обзоре видна, а `token.value()` — нет.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for BrokerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BrokerToken(<скрыт>)")
    }
}

/// Зашифровать доступ.
///
/// Nonce случаен на каждую запись: постоянный nonce у потокового шифра
/// позволяет восстановить открытый текст по двум записям.
#[must_use]
pub fn seal(key: &Key, token: &str) -> SealedToken {
    let cipher = ChaCha20Poly1305::new((&*key.bytes).into());
    let nonce = Nonce::generate();
    // Отказ шифрования при корректном ключе и nonce невозможен: он
    // означал бы нехватку памяти под буфер, а не ошибку данных.
    let ciphertext = cipher
        .encrypt(&nonce, token.as_bytes())
        .unwrap_or_else(|_| unreachable_encryption());
    SealedToken {
        nonce: nonce.to_vec(),
        ciphertext,
    }
}

/// Расшифровать доступ.
///
/// Неверный ключ и подделанная запись дают одну ошибку намеренно:
/// разные ответы сообщили бы, какая из двух причин верна.
pub fn open(key: &Key, sealed: &SealedToken) -> Result<BrokerToken, CryptoError> {
    let cipher = ChaCha20Poly1305::new((&*key.bytes).into());
    let nonce = Nonce::try_from(sealed.nonce.as_slice()).map_err(|_| CryptoError::NotAuthentic)?;
    let plain = Zeroizing::new(
        cipher
            .decrypt(&nonce, sealed.ciphertext.as_slice())
            .map_err(|_| CryptoError::NotAuthentic)?,
    );
    let secret = String::from_utf8(plain.to_vec()).map_err(|_| CryptoError::NotText)?;
    Ok(BrokerToken {
        secret: Zeroizing::new(secret),
    })
}

/// Отдельная функция вместо `unwrap`: `unwrap` здесь читался бы как
/// «а вдруг», хотя отказ означает нехватку памяти, а не ошибку данных.
fn unreachable_encryption() -> ! {
    panic!("шифрование при корректном ключе и nonce не отказывает")
}

/// Права, с которыми заведён доступ к брокеру.
///
/// Вариант ровно один: **торговые права не запрашиваются ни при каких
/// условиях** (§14). Перечисление, а не отсутствие поля, потому что
/// область прав записывается рядом с доступом и разбирается перед
/// вызовом: строка, обещающая полный доступ, не толкуется как доступ,
/// а даёт отказ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BrokerScope {
    ReadOnly,
}

impl BrokerScope {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
        }
    }

    /// Разбор области прав.
    ///
    /// Не `trim` и не приведение регистра: значение пишет система,
    /// а не человек, и «почти то же самое» здесь означает, что запись
    /// изменил кто-то другой.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }
}
