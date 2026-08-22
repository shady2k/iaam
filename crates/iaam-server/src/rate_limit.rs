//! Ограничение частоты запросов (§14).
//!
//! Реализовано на месте, а не внешней крейтой: правило простое —
//! фиксированное окно на один токен, — а лишняя зависимость в слое,
//! отвечающем за безопасность, стоит дороже сорока строк кода.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Максимум различных ключей в памяти.
///
/// Ограничитель считает по ключу, а ключом является хеш **предъявленного**
/// токена — то есть чего угодно. Без предела поток случайных токенов
/// растит карту неограниченно: отказ в обслуживании ценой одного curl.
const DEFAULT_CAPACITY: usize = 10_000;

/// Ограничитель с фиксированным окном.
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

    /// Разрешён ли запрос. Тело вынесено из конструктора, потому что
    /// именно оно должно проверяться мутационным заслоном.
    #[must_use]
    pub fn allow(&self, key: &str) -> bool {
        self.allow_at(key, Instant::now())
    }

    /// Число ключей под наблюдением. Нужно тесту предела памяти:
    /// утверждение «карта не растёт» иначе непроверяемо.
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        match self.counters.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Проверка с явным моментом времени: тест обязан уметь двигать
    /// время, не засыпая на длину окна.
    #[must_use]
    pub fn allow_at(&self, key: &str, now: Instant) -> bool {
        let mut counters = match self.counters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Протухшие окна удаляются только при подходе к пределу: чистка
        // на каждом запросе стоила бы обхода всей карты ради ничего.
        if counters.len() >= self.capacity {
            counters.retain(|_, (started, _)| now.duration_since(*started) < self.window);
        }
        // Карта заполнена действующими окнами — незнакомый ключ
        // не принимается. Это отказ для нового токена, а не для всех:
        // уже известные ключи продолжают обслуживаться. Выбор осознанный,
        // неограниченный рост памяти отказал бы вообще всем (§14).
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
        // `allow` — единственная точка, которую зовёт транспорт.
        // Проверка через `allow_at` её не покрывает: между ними может
        // потеряться сам вызов, и ограничитель молча пропускал бы всё.
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        assert_eq!(limiter.tracked_keys(), 0, "до первого запроса ключей нет");
        assert!(limiter.allow("токен"));
        assert!(limiter.allow("токен"));
        assert!(
            !limiter.allow("токен"),
            "третий запрос за окно обязан быть отклонён"
        );
        assert_eq!(limiter.tracked_keys(), 1);
        assert!(limiter.allow("другой"));
        assert_eq!(limiter.tracked_keys(), 2, "счётчик ключей — это счётчик");
    }

    #[test]
    fn a_window_that_has_lasted_exactly_its_length_is_already_over() {
        // Граница окна: ровно длина окна — это уже новое окно, а не
        // последний миг старого. Нестрогое сравнение задержало бы сброс
        // на один тик и сделало бы предел на единицу строже объявленного.
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.allow_at("токен", start));
        assert!(!limiter.allow_at("токен", start + Duration::from_secs(59)));
        assert!(
            limiter.allow_at("токен", start + Duration::from_secs(60)),
            "окно длиной ровно в окно уже закончилось"
        );

        // Та же граница со стороны чистки протухших окон: окно возрастом
        // ровно в длину окна считается протухшим и освобождает слот.
        // Нестрогое сравнение оставило бы его занятым лишний тик, и
        // предел числа ключей оказался бы на единицу строже объявленного.
        let tight = RateLimiter::with_capacity(10, Duration::from_secs(60), 1);
        let start = Instant::now();
        assert!(tight.allow_at("первый", start));
        assert!(
            !tight.allow_at("второй", start + Duration::from_secs(59)),
            "предел ключей исчерпан, окно первого ещё действует"
        );
        assert!(
            tight.allow_at("второй", start + Duration::from_secs(60)),
            "окно первого протухло ровно в границе и освободило слот"
        );
        assert_eq!(tight.tracked_keys(), 1);
    }

    #[test]
    fn requests_within_the_limit_are_allowed_and_the_next_one_is_not() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("токен", now));
        assert!(limiter.allow_at("токен", now));
        assert!(!limiter.allow_at("токен", now));
    }

    #[test]
    fn a_new_window_resets_the_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("токен", now));
        assert!(!limiter.allow_at("токен", now));
        assert!(limiter.allow_at("токен", now + Duration::from_secs(61)));
    }

    #[test]
    fn different_tokens_do_not_share_a_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("первый", now));
        assert!(limiter.allow_at("второй", now));
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn the_map_does_not_grow_without_bound() {
        // Поток случайных токенов не должен превращаться в рост памяти.
        let limiter = RateLimiter::with_capacity(10, Duration::from_secs(60), 4);
        let now = Instant::now();
        for i in 0..100 {
            let _ = limiter.allow_at(&format!("токен-{i}"), now);
        }
        assert!(limiter.tracked_keys() <= 4);
    }

    #[test]
    fn an_expired_window_frees_its_slot() {
        let limiter = RateLimiter::with_capacity(10, Duration::from_secs(60), 2);
        let now = Instant::now();
        assert!(limiter.allow_at("первый", now));
        assert!(limiter.allow_at("второй", now));
        // Пока окна живы, третий ключ не помещается.
        assert!(!limiter.allow_at("третий", now));
        // После истечения окна место освобождается.
        assert!(limiter.allow_at("третий", now + Duration::from_secs(61)));
    }
}
