//! Устойчивость: когда повторять, через сколько и как часто ходить (§12).
//!
//! Решение о повторе — **чистая функция**. Так оно проверяется без сети
//! и без сна: тест на политику повторов, который спит, проверяет ещё
//! и планировщик потоков, а падает загадочно.

use std::time::{Duration, Instant};

use crate::response::HttpError;

/// Потолок задержки.
///
/// Без него экспонента на шестой попытке ушла бы за пределы окна
/// суточной синхронизации, и задание висело бы вместо того, чтобы
/// честно отчитаться о частичном отказе.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Чем закончилась попытка.
#[derive(Debug)]
pub enum Outcome {
    /// Транспорт не довёл запрос.
    Transport(HttpError),
    /// Узел ответил кодом.
    Status(u16),
}

/// Что делать после попытки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Повторить через указанную задержку.
    After(Duration),
    /// Не повторять: попытки исчерпаны либо повтор бессмыслен.
    GiveUp,
}

/// Политика повторов.
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

    /// Решение по номеру попытки (с единицы) и её исходу.
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

/// Отказ временный, то есть повтор имеет шанс дать другой ответ.
///
/// 4xx, кроме 429, сюда не входят намеренно: отказ в правах или
/// неверный запрос повторятся ровно тем же, и попытки будут потрачены
/// на заведомо известный ответ.
fn is_transient(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Transport(HttpError::Network | HttpError::Timeout) => true,
        Outcome::Transport(HttpError::ClientNotBuilt(_) | HttpError::TrustAnchorNotParsed(_)) => {
            false
        }
        Outcome::Status(status) => matches!(status, 429 | 502 | 503 | 504),
    }
}

/// Ограничение частоты: не чаще одного запроса в заданный интервал.
///
/// Существует, чтобы первичная загрузка истории не выглядела для MOEX
/// как поток запросов: получить 429 и уйти в повторы дороже, чем
/// подождать.
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

    /// Сколько ждать до следующего запроса. Ноль — можно сразу.
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
                "код {status} обязан повторяться"
            );
        }
    }

    #[test]
    fn a_rejection_is_not_retried() {
        for status in [400, 401, 403, 404, 422] {
            assert!(
                matches!(policy().decide(1, &Outcome::Status(status)), Retry::GiveUp),
                "код {status} повторять бессмысленно: ответ будет тот же"
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
            Retry::GiveUp => panic!("должен был повториться"),
        };
        let third = match policy.decide(3, &Outcome::Status(503)) {
            Retry::After(delay) => delay,
            Retry::GiveUp => panic!("должен был повториться"),
        };
        assert!(
            third > first,
            "задержка обязана расти: {first:?} → {third:?}"
        );
        assert!(third <= MAX_BACKOFF, "задержка вышла за потолок: {third:?}");
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
