//! Resilience: when to retry, how long to wait, and how often to call (§12).
//!
//! The retry decision is a **pure function**. It can then be checked without
//! a network or sleep: a retry-policy test that sleeps also tests the thread
//! scheduler and fails mysteriously.

use std::time::{Duration, Instant};

use crate::response::HttpError;

/// Backoff ceiling.
///
/// Without it, the exponential delay on the sixth attempt would exceed the
/// daily synchronisation window, leaving the job hanging instead of honestly
/// reporting a partial refusal.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Attempt outcome.
#[derive(Debug)]
pub enum Outcome {
    /// Transport did not deliver the request.
    Transport(HttpError),
    /// Endpoint returned a status code.
    Status(u16),
}

/// Action after an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Retry after the specified delay.
    After(Duration),
    /// Do not retry: attempts are exhausted or retrying is pointless.
    GiveUp,
}

/// Retry policy.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    attempts: u32,
    base: Duration,
}

impl RetryPolicy {
    #[must_use]
    pub const fn new(attempts: u32, base: Duration) -> Self {
        Self { attempts, base }
    }

    /// Decide from the attempt number (starting at one) and its outcome.
    #[must_use]
    pub fn decide(&self, attempt: u32, outcome: &Outcome) -> Retry {
        if attempt >= self.attempts || !is_transient(outcome) {
            return Retry::GiveUp;
        }
        Retry::After(self.backoff(attempt))
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1_u32 << (attempt.clamp(1, 16) - 1);
        self.base.saturating_mul(factor).min(MAX_BACKOFF)
    }
}

/// A transient refusal, where a retry may produce a different response.
///
/// 4xx statuses other than 429 are deliberately excluded: an authorization
/// refusal or invalid request will be repeated exactly, wasting attempts on a
/// known response.
fn is_transient(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Transport(HttpError::Network | HttpError::Timeout) => true,
        Outcome::Transport(HttpError::ClientNotBuilt(_) | HttpError::TrustAnchorNotParsed(_)) => {
            false
        }
        Outcome::Status(status) => matches!(status, 429 | 502 | 503 | 504),
    }
}

/// Rate limit: no more than one request in the specified interval.
///
/// This prevents the initial history load from looking like a request flood
/// to MOEX: receiving 429 and retrying costs more than waiting.
pub struct RateLimiter {
    min_interval: Duration,
    last: std::sync::Mutex<Option<Instant>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: std::sync::Mutex::new(None),
        }
    }

    /// How long to wait before the next request. Zero means proceed now.
    #[must_use]
    pub fn delay_before_next(&self, now: Instant) -> Duration {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wait = match *last {
            Some(previous) => self
                .min_interval
                .checked_sub(now.saturating_duration_since(previous))
                .unwrap_or_default(),
            None => Duration::ZERO,
        };
        *last = Some(now + wait);
        wait
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::HttpError;

    fn policy() -> RetryPolicy {
        RetryPolicy::new(4, Duration::from_millis(100))
    }

    #[test]
    fn a_network_failure_is_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::Transport(HttpError::Network)),
            Retry::After(_)
        ));
    }

    #[test]
    fn a_timeout_is_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::Transport(HttpError::Timeout)),
            Retry::After(_)
        ));
    }

    #[test]
    fn rate_limiting_and_gateway_failures_are_retried() {
        for status in [429, 502, 503, 504] {
            assert!(
                matches!(
                    policy().decide(1, &Outcome::Status(status)),
                    Retry::After(_)
                ),
                "status {status} must be retried"
            );
        }
    }

    #[test]
    fn a_rejection_is_not_retried() {
        for status in [400, 401, 403, 404, 422] {
            assert!(
                matches!(policy().decide(1, &Outcome::Status(status)), Retry::GiveUp),
                "retrying status {status} is pointless: the response will be the same"
            );
        }
    }

    #[test]
    fn a_success_is_not_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::Status(200)),
            Retry::GiveUp
        ));
    }

    #[test]
    fn the_delay_grows_and_stays_bounded() {
        let policy = policy();
        let first = match policy.decide(1, &Outcome::Status(503)) {
            Retry::After(delay) => delay,
            Retry::GiveUp => panic!("it should have been retried"),
        };
        let third = match policy.decide(3, &Outcome::Status(503)) {
            Retry::After(delay) => delay,
            Retry::GiveUp => panic!("it should have been retried"),
        };
        assert!(third > first, "delay must grow: {first:?} → {third:?}");
        assert!(
            third <= MAX_BACKOFF,
            "delay exceeded the ceiling: {third:?}"
        );
    }

    #[test]
    fn exponential_backoff_uses_the_attempt_index_as_a_zero_based_power() {
        let policy = policy();

        assert_eq!(
            policy.decide(1, &Outcome::Status(503)),
            Retry::After(Duration::from_millis(100))
        );
        assert_eq!(
            policy.decide(3, &Outcome::Status(503)),
            Retry::After(Duration::from_millis(400))
        );
    }

    #[test]
    fn attempts_are_exhausted_rather_than_looping_forever() {
        assert!(matches!(
            policy().decide(4, &Outcome::Status(503)),
            Retry::GiveUp
        ));
    }

    #[test]
    fn zero_attempt_is_handled_without_underflow() {
        assert!(matches!(
            policy().decide(0, &Outcome::Status(503)),
            Retry::After(_)
        ));
    }

    #[test]
    fn rate_limiter_enforces_the_minimum_interval() {
        let limiter = RateLimiter::new(Duration::from_millis(100));
        let start = Instant::now();

        assert_eq!(limiter.delay_before_next(start), Duration::ZERO);
        assert_eq!(
            limiter.delay_before_next(start + Duration::from_millis(50)),
            Duration::from_millis(50)
        );
        assert_eq!(
            limiter.delay_before_next(start + Duration::from_millis(100)),
            Duration::from_millis(100)
        );
    }
}
