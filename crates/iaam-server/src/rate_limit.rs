//! Request rate limiting (§14).
//!
//! Implemented here rather than with an external crate: the rule is simple —
//! a fixed window per token, — and an extra dependency in a layer,
//! responsible for security, costs more than forty lines of code.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Maximum number of distinct keys in memory.
///
/// The limiter counts by key, and the key is the hash of the **presented**
/// token — that is, anything at all. Without a limit, a stream of random tokens
/// grows the map without bound: denial of service at the cost of one curl.
const DEFAULT_CAPACITY: usize = 10_000;

/// What the limiter says about one request.
///
/// A bare `bool` was enough while the refusal said nothing back. It is not
/// enough now that the refusal must say how long to wait: the wait is a
/// property of the window this very check looked at, and a caller that had to
/// ask for it in a second call would be told about a window that may have
/// turned over in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateDecision {
    /// The request is served.
    Allowed,
    /// The request is refused, and nothing but time lifts the refusal.
    Refused {
        /// What is left of the window the refusal is waiting on.
        retry_after: Duration,
    },
}

impl RateDecision {
    /// Whether the request goes through.
    #[must_use]
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// How long the caller must wait, where it must wait at all.
    ///
    /// `None` rather than a zero duration for an allowed request: «go ahead»
    /// and «wait no time» are the same instruction only until something
    /// publishes the second one, and a served response carrying a header that
    /// tells its caller to wait is a refusal it did not receive.
    #[must_use]
    pub fn retry_after(self) -> Option<Duration> {
        match self {
            Self::Allowed => None,
            Self::Refused { retry_after } => Some(retry_after),
        }
    }
}

/// Fixed-window limiter.
pub struct RateLimiter {
    window: Duration,
    limit: u32,
    capacity: usize,
    counters: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(limit: u32, window: Duration) -> Self {
        Self::with_capacity(limit, window, DEFAULT_CAPACITY)
    }

    #[must_use]
    pub fn with_capacity(limit: u32, window: Duration, capacity: usize) -> Self {
        Self {
            window,
            limit,
            capacity,
            counters: Mutex::new(HashMap::new()),
        }
    }

    /// Whether the request is allowed, and how long to wait if it is not. The body is
    /// factored out of the constructor because
    /// it is this code that must be checked by the mutation-testing gate.
    #[must_use]
    pub fn allow(&self, key: &str) -> RateDecision {
        self.allow_at(key, Instant::now())
    }

    /// Number of tracked keys. Needed for the memory-limit test:
    /// the assertion «the map does not grow» would otherwise be untestable.
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        match self.counters.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Check at an explicit point in time: the test must be able to advance
    /// time without sleeping for the window duration.
    ///
    /// The wait a refusal carries is measured from the **stored** window start,
    /// never from the configured window length. The two agree only for a caller
    /// counted out on the first request of its window; every other caller is
    /// already part of the way through one, and telling it to wait a full
    /// window would keep it idle past the moment it could have been served.
    #[must_use]
    pub fn allow_at(&self, key: &str, now: Instant) -> RateDecision {
        let mut counters = match self.counters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Expired windows are removed only when approaching the limit: cleaning
        // on every request would require scanning the entire map for no reason.
        if counters.len() >= self.capacity {
            counters.retain(|_, (started, _)| now.duration_since(*started) < self.window);
        }
        // The map is full of active windows — an unknown key
        // is rejected. This denies a new token, not everyone:
        // already known keys continue to be served. The choice is deliberate,
        // unbounded memory growth would deny service to everyone (§14).
        if counters.len() >= self.capacity && !counters.contains_key(key) {
            // This caller has no window of its own to wait out — it was refused
            // for want of room, not for its own count. What it waits for is the
            // earliest of the windows already held, because that is the moment
            // the map next has a slot. The configured length is the wrong
            // answer here for the same reason it is wrong above: it names a
            // wait longer than the truth.
            let earliest = counters
                .values()
                .map(|(started, _)| self.window.saturating_sub(now.duration_since(*started)))
                .min()
                .unwrap_or(self.window);
            return RateDecision::Refused {
                retry_after: earliest,
            };
        }
        let entry = counters.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return RateDecision::Refused {
                retry_after: self.window.saturating_sub(now.duration_since(entry.0)),
            };
        }
        entry.1 += 1;
        RateDecision::Allowed
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_entry_point_limits_too_and_counts_what_it_tracks() {
        // `allow` is the only entry point called by the transport.
        // Testing via `allow_at` does not cover it: the call itself could
        // be lost between them, and the limiter would silently allow everything through.
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        assert_eq!(
            limiter.tracked_keys(),
            0,
            "there are no keys before the first request"
        );
        assert!(limiter.allow("token").is_allowed());
        assert!(limiter.allow("token").is_allowed());
        assert!(
            !limiter.allow("token").is_allowed(),
            "the third request within a window must be rejected"
        );
        assert_eq!(limiter.tracked_keys(), 1);
        assert!(limiter.allow("other").is_allowed());
        assert_eq!(limiter.tracked_keys(), 2, "the key counter is a counter");
    }

    #[test]
    fn a_window_that_has_lasted_exactly_its_length_is_already_over() {
        // Window boundary: exactly one window length is already a new window, not
        // the last instant of the old one. A non-strict comparison would delay the reset
        // by one tick and make the limit one stricter than stated.
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.allow_at("token", start).is_allowed());
        assert!(
            !limiter
                .allow_at("token", start + Duration::from_secs(59))
                .is_allowed()
        );
        assert!(
            limiter
                .allow_at("token", start + Duration::from_secs(60))
                .is_allowed(),
            "a window exactly one window long has already ended"
        );

        // The same boundary applies when cleaning up expired windows: a window whose age
        // is exactly one window length is considered expired and frees a slot.
        // A non-strict comparison would leave it occupied for one extra tick, and
        // make the key-count limit one stricter than stated.
        let tight = RateLimiter::with_capacity(10, Duration::from_secs(60), 1);
        let start = Instant::now();
        assert!(tight.allow_at("first", start).is_allowed());
        assert!(
            !tight
                .allow_at("second", start + Duration::from_secs(59))
                .is_allowed(),
            "the key limit is exhausted, the first window is still active"
        );
        assert!(
            tight
                .allow_at("second", start + Duration::from_secs(60))
                .is_allowed(),
            "the first window expired exactly at the boundary and freed a slot"
        );
        assert_eq!(tight.tracked_keys(), 1);
    }

    #[test]
    fn requests_within_the_limit_are_allowed_and_the_next_one_is_not() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("token", now).is_allowed());
        assert!(limiter.allow_at("token", now).is_allowed());
        assert!(!limiter.allow_at("token", now).is_allowed());
    }

    #[test]
    fn a_new_window_resets_the_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("token", now).is_allowed());
        assert!(!limiter.allow_at("token", now).is_allowed());
        assert!(
            limiter
                .allow_at("token", now + Duration::from_secs(61))
                .is_allowed()
        );
    }

    #[test]
    fn different_tokens_do_not_share_a_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("first", now).is_allowed());
        assert!(limiter.allow_at("second", now).is_allowed());
    }

    #[test]
    fn a_refusal_says_how_much_of_the_window_is_left() {
        // The wait is read from the window that is running, not from the
        // configured length of one: a caller counted out part of the way
        // through waits for the part that remains, and one sent away for a
        // whole window would sit out a window it has already spent.
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.allow_at("token", start).is_allowed());
        assert_eq!(
            limiter
                .allow_at("token", start + Duration::from_secs(20))
                .retry_after(),
            Some(Duration::from_secs(40)),
            "forty seconds of this window are left, not sixty"
        );
    }

    #[test]
    fn an_allowed_request_names_no_wait() {
        // «Allowed» and «wait zero seconds» are different statements, and the
        // transport publishes a wait only for the first refusal that has one.
        // A zero here would have every served request carry a header telling
        // its caller to wait.
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        assert_eq!(limiter.allow("token").retry_after(), None);
    }

    #[test]
    fn a_key_with_no_slot_waits_for_the_first_window_to_turn_over() {
        // Refused for want of room in the map rather than for its own count,
        // this caller has no window of its own to wait out. What it waits for
        // is the earliest of the windows already held, because that is when the
        // map next has room for it. Naming the configured length instead would
        // send it away for longer than the truth on every request but the one
        // that arrives as a window opens.
        let tight = RateLimiter::with_capacity(10, Duration::from_secs(60), 2);
        let start = Instant::now();
        assert!(tight.allow_at("first", start).is_allowed());
        assert!(
            tight
                .allow_at("second", start + Duration::from_secs(10))
                .is_allowed()
        );
        assert_eq!(
            tight
                .allow_at("third", start + Duration::from_secs(30))
                .retry_after(),
            Some(Duration::from_secs(30)),
            "the first window ends thirty seconds from here, and frees the slot"
        );
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn the_map_does_not_grow_without_bound() {
        // A stream of random tokens must not cause unbounded memory growth.
        let limiter = RateLimiter::with_capacity(10, Duration::from_secs(60), 4);
        let now = Instant::now();
        for i in 0..100 {
            let _ = limiter.allow_at(&format!("token-{i}"), now);
        }
        assert!(limiter.tracked_keys() <= 4);
    }

    #[test]
    fn an_expired_window_frees_its_slot() {
        let limiter = RateLimiter::with_capacity(10, Duration::from_secs(60), 2);
        let now = Instant::now();
        assert!(limiter.allow_at("first", now).is_allowed());
        assert!(limiter.allow_at("second", now).is_allowed());
        // While the windows are active, there is no room for a third key.
        assert!(!limiter.allow_at("third", now).is_allowed());
        // Once the window expires, space becomes available.
        assert!(
            limiter
                .allow_at("third", now + Duration::from_secs(61))
                .is_allowed()
        );
    }
}
