//! Instance claiming (§14).
//!
//! The system is single-user, and open registration will never be available
//! here: a second user could create an empty portfolio in someone else's database.
//! Therefore, instead of registration, there is a **one-time claim**:
//! while there is no owner, the program prints a one-time code at startup,
//! which whoever reads it exchanges for an owner token.
//! Only the person who started the program can read what it prints to standard error —
//! access to the console is itself proof of entitlement to the instance.
//!
//! The code lives **in process memory and nowhere else**. It is not stored in the database:
//! leaking the database file must not surrender the instance — otherwise a stolen
//! file would confer the right to create an owner in it. For the same
//! reason, the code's hash is stored rather than the code itself: a process memory dump is
//! a less likely, but not impossible, leak.

use std::time::{Duration, Instant};

use iaam_app::error::AppError;
use iaam_app::ports::SoleOwner;
use iaam_app::tokens::secret_hex;

use crate::ServerState;
use crate::auth::hash_token;

/// How long the claim code remains valid.
///
/// Fifteen minutes means ‘enough time to copy it from the adjacent window’, not
/// ‘I'll deal with it tomorrow’. A code valid until the process exits becomes
/// a permanent secondary entry point on a long-running server.
pub const CLAIM_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// Number of random bytes in the code.
///
/// Sixteen bytes (128 bits): the claim route is open without
/// authentication, so brute-forcing it must be impossible, not merely
/// difficult.
const CLAIM_BYTES: usize = 16;

/// An issued claim code.
///
/// The hash is stored rather than the code, for the same reason that the database stores
/// the token hash. Verification also compares hashes: comparing the codes themselves
/// gives an attacker a timing clue about the length of the matching
/// prefix, whereas hashes differ from the first byte.
///
/// The issue time is an `Instant`, not the time of day: the lifetime must not
/// depend on system clock adjustments.
pub struct ClaimCode {
    hash: String,
    issued_at: Instant,
}

impl ClaimCode {
    /// Generate a code. Returns the code itself — for the caller to print — and
    /// the state retained in server memory.
    ///
    /// The code itself is not stored anywhere: there is nowhere to retrieve it for a second display,
    /// and that is a property, not an inconvenience.
    pub fn issue() -> Result<(String, Self), AppError> {
        let code = secret_hex(CLAIM_BYTES)?;
        let stored = Self {
            hash: hash_token(&code),
            issued_at: Instant::now(),
        };
        Ok((code, stored))
    }

    /// Whether the submitted code is valid.
    ///
    /// Invalid and expired codes are deliberately indistinguishable to the caller:
    /// different responses would reveal that the code had been partly guessed.
    #[must_use]
    pub fn accepts(&self, code: &str) -> bool {
        self.issued_at.elapsed() < CLAIM_LIFETIME && self.hash == hash_token(code)
    }
}

/// Arm claiming if the database does not yet have an owner.
///
/// Returns the code to print; `None` means an owner exists and no code is
/// generated **at all**: a secret that nobody needs still remains
/// a secret held in memory.
///
/// This lives in the transport layer rather than the composition root for two reasons. The code is
/// state belonging to the `/v1/claim` route, and keeping its condition in another
/// crate would mean that a differently assembled server could silently remain
/// unclaimable. And the program, not the library, must print the code:
/// therefore the decision is made here, while printing is left to the caller.
///
/// If there are multiple owners, no code is generated either: there is nothing to claim,
/// and a split owner must be dealt with via the console.
pub async fn arm(state: &ServerState) -> Result<Option<String>, AppError> {
    match state.services.tokens.sole_owner().await? {
        SoleOwner::None => {}
        SoleOwner::Single(_) | SoleOwner::Several => return Ok(None),
    }
    let (code, stored) = ClaimCode::issue()?;
    state.arm_claim(stored);
    Ok(Some(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_accepted_once_issued_and_nothing_else_is() {
        let (code, stored) = ClaimCode::issue().expect("code generated");
        assert!(stored.accepts(&code));
        assert!(!stored.accepts(&format!("{code}0")));
        assert!(!stored.accepts(""));
    }

    #[test]
    fn the_code_itself_is_not_kept_in_memory() {
        // A hash is stored: a process memory snapshot must not reveal the code.
        let (code, stored) = ClaimCode::issue().expect("code generated");
        assert_ne!(stored.hash, code);
        assert_eq!(stored.hash, hash_token(&code));
    }

    #[test]
    fn two_codes_never_coincide() {
        // A match would mean that the source of randomness is not random,
        // and the claim code is the only barrier guarding the instance.
        let (first, _) = ClaimCode::issue().expect("code generated");
        let (second, _) = ClaimCode::issue().expect("code generated");
        assert_ne!(first, second);
        assert_eq!(first.len(), CLAIM_BYTES * 2);
    }
}
