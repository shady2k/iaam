//! Resilience: when to retry and how long to wait (§12). How often to call
//! is the gateway's budget table.
//!
//! The retry decision is a **pure function**. It can then be checked without
//! a network or sleep: a retry-policy test that sleeps also tests the thread
//! scheduler and fails mysteriously.

use std::time::{Duration, SystemTime};

use crate::response::HttpError;

/// Backoff ceiling.
///
/// Without it, the exponential delay on the sixth attempt would exceed the
/// daily synchronisation window, leaving the job hanging instead of honestly
/// reporting a partial refusal.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The longest wait a source may name that the gateway waits out.
///
/// A synchronisation runs under a fifteen-minute deadline: a longer wait
/// could not end inside the run that met it, so the caller is told when to
/// come back rather than held. Up to it, the source's word is obeyed in full.
pub const MAX_NAMED_WAIT: Duration = Duration::from_secs(15 * 60);

/// Attempt outcome.
#[derive(Debug)]
pub enum Outcome {
    /// Transport did not deliver the request.
    Transport(HttpError),
    /// Endpoint returned a status code, with the `Retry-After` it named, if
    /// any. The interval travels as data on the outcome rather than being
    /// read from a response object at decision time, so `decide` stays
    /// checkable without a network call.
    Status {
        status: u16,
        retry_after: Option<Duration>,
    },
}

impl Outcome {
    /// A status with no stated `Retry-After`.
    #[must_use]
    pub const fn status(status: u16) -> Self {
        Self::Status {
            status,
            retry_after: None,
        }
    }

    /// The delay the source named, if it named one.
    #[must_use]
    pub const fn named_delay(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            Self::Transport(_) => None,
        }
    }

    /// A status carrying the delay the source named.
    #[must_use]
    pub const fn status_with_retry_after(status: u16, retry_after: Duration) -> Self {
        Self::Status {
            status,
            retry_after: Some(retry_after),
        }
    }
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
        Retry::After(self.delay(attempt, outcome))
    }

    /// The wait before the next attempt, also when no attempt is left: a
    /// caller that gave up is told when trying again is worth it.
    ///
    /// A `Retry-After` the source named wins over the guess, in full: the
    /// source knows its own refusal window, we do not, and coming back sooner
    /// is exactly the hammering it asked us to stop. `MAX_BACKOFF` bounds our
    /// guess, not the source's word; a named wait too long to hold a caller
    /// for is the gateway's to refuse (`MAX_NAMED_WAIT`).
    #[must_use]
    pub fn delay(&self, attempt: u32, outcome: &Outcome) -> Duration {
        match outcome {
            Outcome::Status {
                retry_after: Some(interval),
                ..
            } => *interval,
            _ => self.backoff(attempt),
        }
    }

    /// Exponential backoff for when the source named no interval.
    ///
    /// No jitter: jitter spreads out many independent clients converging on
    /// the same refusal at once. This process is the only client of these
    /// destinations, and its outbound calls are already serialised one at a
    /// time per host by the gateway — there is no thundering herd here to
    /// break up, only a single caller whose wait would become less
    /// predictable for no benefit.
    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1_u32 << (attempt.clamp(1, 16) - 1);
        self.base.saturating_mul(factor).min(MAX_BACKOFF)
    }
}

/// Parse a `Retry-After` header value, read at `now`.
///
/// Both forms the spec allows: delay-seconds (`Retry-After: 120`) and an
/// HTTP-date (`Retry-After: Wed, 21 Oct 2026 07:28:00 GMT`), the latter as the
/// time left until it; a date already past asks for no wait. A value that
/// parses as neither returns `None` rather than zero or an error, so the
/// caller can fall back to the computed backoff instead of retrying
/// immediately or aborting the attempt outright.
#[must_use]
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse() {
        return Some(Duration::from_secs(seconds));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// A transient refusal, where a retry may produce a different response.
///
/// 4xx statuses other than 429 are deliberately excluded: an authorization
/// refusal or invalid request will be repeated exactly, wasting attempts on a
/// known response. 500 is included: the brokers answer a momentary internal
/// fault with it, and the same request succeeds on the next attempt.
#[must_use]
pub fn is_transient(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Transport(HttpError::Network | HttpError::Timeout) => true,
        Outcome::Transport(HttpError::ClientNotBuilt(_) | HttpError::TrustAnchorNotParsed(_)) => {
            false
        }
        Outcome::Status { status, .. } => matches!(status, 429 | 500 | 502 | 503 | 504),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

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
                    policy().decide(1, &Outcome::status(status)),
                    Retry::After(_)
                ),
                "status {status} must be retried"
            );
        }
    }

    #[test]
    fn an_internal_server_error_is_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::status(500)),
            Retry::After(_)
        ));
    }

    #[test]
    fn a_rejection_is_not_retried() {
        for status in [400, 401, 403, 404, 422] {
            assert!(
                matches!(policy().decide(1, &Outcome::status(status)), Retry::GiveUp),
                "retrying status {status} is pointless: the response will be the same"
            );
        }
    }

    #[test]
    fn a_success_is_not_retried() {
        assert!(matches!(
            policy().decide(1, &Outcome::status(200)),
            Retry::GiveUp
        ));
    }

    #[test]
    fn the_delay_grows_and_stays_bounded() {
        let policy = policy();
        let first = match policy.decide(1, &Outcome::status(503)) {
            Retry::After(delay) => delay,
            Retry::GiveUp => panic!("it should have been retried"),
        };
        let third = match policy.decide(3, &Outcome::status(503)) {
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
            policy.decide(1, &Outcome::status(503)),
            Retry::After(Duration::from_millis(100))
        );
        assert_eq!(
            policy.decide(3, &Outcome::status(503)),
            Retry::After(Duration::from_millis(400))
        );
    }

    #[test]
    fn attempts_are_exhausted_rather_than_looping_forever() {
        assert!(matches!(
            policy().decide(4, &Outcome::status(503)),
            Retry::GiveUp
        ));
    }

    #[test]
    fn zero_attempt_is_handled_without_underflow() {
        assert!(matches!(
            policy().decide(0, &Outcome::status(503)),
            Retry::After(_)
        ));
    }

    #[test]
    fn a_stated_retry_after_is_used_instead_of_the_computed_backoff() {
        let policy = policy();
        let outcome = Outcome::status_with_retry_after(503, Duration::from_secs(5));

        assert_eq!(
            policy.decide(1, &outcome),
            Retry::After(Duration::from_secs(5))
        );
    }

    #[test]
    fn a_stated_retry_after_beyond_the_backoff_ceiling_is_kept_in_full() {
        let policy = policy();
        let outcome = Outcome::status_with_retry_after(503, Duration::from_secs(3600));

        assert_eq!(
            policy.decide(1, &outcome),
            Retry::After(Duration::from_secs(3600)),
            "the ceiling bounds our guess, not the source's instruction"
        );
    }

    #[test]
    fn the_longest_named_wait_is_a_synchronisation_deadline() {
        assert_eq!(MAX_NAMED_WAIT, Duration::from_secs(15 * 60));
    }

    #[test]
    fn a_status_without_a_stated_retry_after_still_uses_the_computed_backoff() {
        let policy = policy();
        let outcome = Outcome::status(503);

        assert_eq!(
            policy.decide(1, &outcome),
            Retry::After(Duration::from_millis(100))
        );
    }

    #[test]
    fn a_stated_retry_after_does_not_revive_a_status_that_is_not_worth_retrying() {
        let policy = policy();
        let outcome = Outcome::status_with_retry_after(404, Duration::from_secs(5));

        assert!(matches!(policy.decide(1, &outcome), Retry::GiveUp));
    }

    /// 2026-10-21 07:28:00 UTC, the instant the dates below are read at.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_567_680)
    }

    #[test]
    fn parse_retry_after_reads_delay_seconds() {
        assert_eq!(
            parse_retry_after("120", at()),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn parse_retry_after_tolerates_surrounding_whitespace() {
        assert_eq!(
            parse_retry_after(" 30 ", at()),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn parse_retry_after_reads_an_http_date_as_the_time_left_until_it() {
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2026 07:30:30 GMT", at()),
            Some(Duration::from_secs(150))
        );
    }

    #[test]
    fn an_http_date_already_past_asks_for_no_wait() {
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2026 07:00:00 GMT", at()),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn parse_retry_after_rejects_garbage() {
        assert_eq!(parse_retry_after("not-a-number", at()), None);
    }

    #[test]
    fn an_unparseable_retry_after_falls_back_to_the_computed_backoff_not_zero() {
        let policy = policy();
        let outcome = match parse_retry_after("not-a-number", at()) {
            Some(interval) => Outcome::status_with_retry_after(503, interval),
            None => Outcome::status(503),
        };

        assert_eq!(
            policy.decide(1, &outcome),
            Retry::After(Duration::from_millis(100))
        );
    }
}
