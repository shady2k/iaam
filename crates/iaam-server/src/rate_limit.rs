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

    /// Whether the request is allowed. The body is factored out of the constructor because
    /// it is this code that must be checked by the mutation-testing gate.
    #[must_use]
    pub fn allow(&self, key: &str) -> bool {
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
    #[must_use]
    pub fn allow_at(&self, key: &str, now: Instant) -> bool {
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
            return false;
        }
        let entry = counters.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
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
        assert!(limiter.allow("token"));
        assert!(limiter.allow("token"));
        assert!(
            !limiter.allow("token"),
            "the third request within a window must be rejected"
        );
        assert_eq!(limiter.tracked_keys(), 1);
        assert!(limiter.allow("other"));
        assert_eq!(limiter.tracked_keys(), 2, "the key counter is a counter");
    }

    #[test]
    fn a_window_that_has_lasted_exactly_its_length_is_already_over() {
        // Window boundary: exactly one window length is already a new window, not
        // the last instant of the old one. A non-strict comparison would delay the reset
        // by one tick and make the limit one stricter than stated.
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.allow_at("token", start));
        assert!(!limiter.allow_at("token", start + Duration::from_secs(59)));
        assert!(
            limiter.allow_at("token", start + Duration::from_secs(60)),
            "a window exactly one window long has already ended"
        );

        // The same boundary applies when cleaning up expired windows: a window whose age
        // is exactly one window length is considered expired and frees a slot.
        // A non-strict comparison would leave it occupied for one extra tick, and
        // make the key-count limit one stricter than stated.
        let tight = RateLimiter::with_capacity(10, Duration::from_secs(60), 1);
        let start = Instant::now();
        assert!(tight.allow_at("first", start));
        assert!(
            !tight.allow_at("second", start + Duration::from_secs(59)),
            "the key limit is exhausted, the first window is still active"
        );
        assert!(
            tight.allow_at("second", start + Duration::from_secs(60)),
            "the first window expired exactly at the boundary and freed a slot"
        );
        assert_eq!(tight.tracked_keys(), 1);
    }

    #[test]
    fn requests_within_the_limit_are_allowed_and_the_next_one_is_not() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("token", now));
        assert!(limiter.allow_at("token", now));
        assert!(!limiter.allow_at("token", now));
    }

    #[test]
    fn a_new_window_resets_the_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("token", now));
        assert!(!limiter.allow_at("token", now));
        assert!(limiter.allow_at("token", now + Duration::from_secs(61)));
    }

    #[test]
    fn different_tokens_do_not_share_a_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("first", now));
        assert!(limiter.allow_at("second", now));
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
        assert!(limiter.allow_at("first", now));
        assert!(limiter.allow_at("second", now));
        // While the windows are active, there is no room for a third key.
        assert!(!limiter.allow_at("third", now));
        // Once the window expires, space becomes available.
        assert!(limiter.allow_at("third", now + Duration::from_secs(61)));
    }
}
