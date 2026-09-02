//! Secrets presented over HTTP: the token itself and its hash (§14).
//!
//! They reside in the application, not the transport, because the token is issued by
//! the `TokenAdmin` port, implemented by the storage adapter: the adapter needs to
//! calculate **the same** hash that the transport later uses to look it up. A second
//! hash implementation could silently diverge — the issued token would simply
//! stop being found, and the cause would have to be sought in
//! authentication, not issuance. `iaam-server` re-exports
//! `hash_token` and does not maintain its own copy.

use rand::TryRng;
use sha2::{Digest, Sha256};

use crate::error::AppError;

/// Token hash.
///
/// SHA-256, not a password hash: the token is 256 random bits from a system
/// source, there is no practical way to brute-force it, and argon2 on every request costs
/// more than it provides. For owner passwords — if they ever
/// appear — the conclusion is the opposite.
///
/// **There is no constant-time comparison here, and that is deliberate.** Lookup
/// uses `WHERE token_hash = ?`, so the comparison is performed by
/// SQLite, and is not constant-time. The comparison timing leak
/// lets an attacker guess the hash one prefix at a time —
/// but what must be guessed is the SHA-256 output of a random 256-bit value,
/// not the token itself. A «constant-time comparison» function not used
/// on the authentication path would promise protection that does not exist: such a
/// function used to be here and was removed.
#[must_use]
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A random secret in hexadecimal.
///
/// The source is `SysRng`, not `rand::rng()`: tokens are effectively keys to
/// other people's money, and a weak generator here costs more than everything
/// else in this file. Source failure is returned as an error, not replaced with
/// a fallback generator: a secret issued by unknown means is worse than one not
/// issued — no one will know about the former.
pub fn secret_hex(bytes: usize) -> Result<String, AppError> {
    let mut buffer = vec![0_u8; bytes];
    rand::rngs::SysRng
        .try_fill_bytes(&mut buffer)
        .map_err(|error| AppError::Random(error.to_string()))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_stable_and_does_not_contain_the_token() {
        let hash = hash_token("secret");
        assert_eq!(hash, hash_token("secret"));
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("secret"));
        assert_ne!(hash, hash_token("secret "));
    }

    #[test]
    fn a_secret_is_hex_of_the_requested_length_and_never_repeats() {
        // The character length is twice the byte length: a secret
        // shorter than requested provides less security than requested, and
        // there is no way to detect this later.
        let first = secret_hex(32).expect("randomness source");
        let second = secret_hex(32).expect("randomness source");
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|symbol| symbol.is_ascii_hexdigit()));
        assert_ne!(
            first, second,
            "a repeated secret means the generator is not random"
        );
    }
}
