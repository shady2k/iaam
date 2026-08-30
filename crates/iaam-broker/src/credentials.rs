//! Broker access encryption (§14).
//!
//! A leaked database file must not grant access to a brokerage account,
//! so the token is stored in the database only as ciphertext, while the key
//! lives outside the database—in the process environment.
//!
//! **The secret is never printed.** Every type in this module has a manual
//! `Debug` implementation: a derived one would print the contents, and the
//! first `{:?}` in a log would send the token somewhere it could not be removed.

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

/// ChaCha20-Poly1305 key length.
const KEY_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CryptoError {
    #[error("encryption-key environment variable {variable} is not set")]
    KeyMissing { variable: String },
    #[error("encryption key is not base64")]
    KeyNotBase64,
    #[error("encryption key is {found} bytes long instead of 32")]
    KeyLength { found: usize },
    // The text intentionally contains neither key nor ciphertext: the error
    // message is certain to reach the log.
    #[error("access cannot be decrypted: wrong key or forged record")]
    NotAuthentic,
    #[error("decrypted access is not text")]
    NotText,
    #[error("key file {path} cannot be read: {detail}")]
    KeyFileUnreadable { path: String, detail: String },
    #[error(
        "key file {path} already exists: overwriting it would make every configured access unreadable"
    )]
    KeyFileExists { path: String },
    #[error("key file {path} was not written: {detail}")]
    KeyFileNotWritten { path: String, detail: String },
}

/// Encryption key for broker access. It lives outside the database.
#[derive(Clone)]
pub struct Key {
    bytes: Zeroizing<[u8; KEY_BYTES]>,
}

impl Key {
    /// Key from ready-made bytes. Needed by tests and callers that obtain the
    /// key from somewhere other than the environment.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self {
            bytes: Zeroizing::new(bytes),
        }
    }

    /// Key from base64. Raw 32-byte environment values do not survive copy and
    /// paste, so the key is passed as text.
    ///
    /// A key of another length is refused, not padded with zeroes: a padded
    /// key looks usable while providing different protection than promised.
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

    /// Key from a file.
    ///
    /// The primary production path: an environment variable is visible in
    /// `/proc/<pid>/environ` to the same user and is inherited by every child
    /// process, while a file is not.
    pub fn from_file(path: &Path) -> Result<Self, CryptoError> {
        let encoded = Zeroizing::new(fs::read_to_string(path).map_err(|error| {
            CryptoError::KeyFileUnreadable {
                path: path.display().to_string(),
                detail: error.to_string(),
            }
        })?);
        Self::from_base64(&encoded)
    }

    /// Create a new random key in a file.
    ///
    /// Returns `()`, not the key: a caller cannot print or log what it never
    /// received. The key exists only in a file accessible to one process owner.
    ///
    /// An existing file is **never overwritten**: a new key in place of the old
    /// one makes every previously configured access unreadable—silently and
    /// irreversibly.
    pub fn create_at(path: &Path) -> Result<(), CryptoError> {
        // The key comes from the cipher library's cryptographic source, not a
        // general-purpose generator: this key grants access to someone else's
        // money, so a weak source is the most expensive failure in this file.
        let generated = AeadKey::<ChaCha20Poly1305>::generate();
        let mut material = Zeroizing::new([0_u8; KEY_BYTES]);
        material.copy_from_slice(&generated);
        let encoded = Zeroizing::new(STANDARD.encode(&material[..]));
        // Set the mode while creating the file, not afterwards: a file
        // temporarily available to everyone is available to everyone.
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
        // The umask may have removed permission bits during creation: confirm
        // the mode explicitly rather than assuming it.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            CryptoError::KeyFileNotWritten {
                path: path.display().to_string(),
                detail: error.to_string(),
            }
        })?;
        Ok(())
    }

    /// Key from an environment variable.
    ///
    /// A missing variable is a refusal. There can be no encryption-key
    /// default: a “default” key would be known to everyone who read the source.
    pub fn from_env(variable: &str) -> Result<Self, CryptoError> {
        let encoded = env::var(variable).map_err(|_| CryptoError::KeyMissing {
            variable: variable.to_owned(),
        })?;
        Self::from_base64(&encoded)
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Key(<hidden>)")
    }
}

/// Encrypted access: the value stored in the database.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedToken {
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

impl SealedToken {
    /// Assemble from bytes read from storage.
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
    /// Print lengths, not contents: ciphertext in a log is useless to a reader
    /// and useful to anyone who obtains the log together with the key.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealedToken")
            .field("nonce_len", &self.nonce.len())
            .field("ciphertext_len", &self.ciphertext.len())
            .finish()
    }
}

/// Decrypted access.
///
/// A wrapper around zeroizing memory, not a bare `Zeroizing<String>`:
/// `Zeroizing` clears memory on destruction, but its own `Debug` prints the
/// contents, so the secret would leak into the log through the first `{:?}`.
pub struct BrokerToken {
    secret: Zeroizing<String>,
}

impl BrokerToken {
    /// Token value. The long name is intentional: `token.expose()` is visible
    /// during review, while `token.value()` is not.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for BrokerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BrokerToken(<hidden>)")
    }
}

/// Encrypt access.
///
/// The nonce is random for every record: a constant nonce in a stream cipher
/// allows the plaintext to be recovered from two records.
#[must_use]
pub fn seal(key: &Key, token: &str) -> SealedToken {
    let cipher = ChaCha20Poly1305::new((&*key.bytes).into());
    let nonce = Nonce::generate();
    // Encryption cannot refuse with a valid key and nonce: that would mean
    // insufficient memory for the buffer, not a data error.
    let ciphertext = cipher
        .encrypt(&nonce, token.as_bytes())
        .unwrap_or_else(|_| unreachable_encryption());
    SealedToken {
        nonce: nonce.to_vec(),
        ciphertext,
    }
}

/// Decrypt access.
///
/// A wrong key and a forged record produce one error intentionally: different
/// responses would reveal which of the two causes is true.
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

/// Separate function instead of `unwrap`: `unwrap` would read as “what if?”,
/// although refusal means insufficient memory, not a data error.
fn unreachable_encryption() -> ! {
    panic!("encryption with a valid key and nonce cannot refuse")
}

/// Permissions with which broker access was configured.
///
/// Exactly one variant exists: **trading permissions are never requested**
/// (§14). An enum rather than an absent field because the permission scope is
/// stored beside access and parsed before the call: a string promising full
/// access is not interpreted as access; it produces a refusal.
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

    /// Parse a permission scope.
    ///
    /// No `trim` and no case folding: the value is written by the system,
    /// not a person, and “almost the same” here means another party changed
    /// the record.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }
}
