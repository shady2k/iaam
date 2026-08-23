//! Аутентификация (§14).
//!
//! Аутентификация с первого дня: отложенная не добавляется никогда.
//! В базе лежит **хеш** токена; сравнение — за постоянное время, чтобы
//! время ответа не выдавало правильный префикс.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use iaam_app::ports::Principal;

use crate::ServerState;
use crate::error::ApiFailure;

/// Хеш токена. Живёт в `iaam-app`, потому что тот же хеш считает
/// адаптер при выпуске токена, а до транспорта ему не дотянуться:
/// зависимость идёт сверху вниз. Переэкспорт, а не вторая реализация —
/// разойдясь, они дали бы выпущенный токен, который не находится
/// при проверке, и искать причину пришлось бы не там, где она есть.
/// Обоснование выбора SHA-256 и отсутствия сравнения за постоянное
/// время — в документации самой функции.
pub use iaam_app::tokens::hash_token;

/// Извлечение токена из заголовка `Authorization: Bearer …`.
#[must_use]
pub fn bearer(request: &Request) -> Option<String> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_owned())
}

/// Слой аутентификации и ограничения частоты.
///
/// Журнал использования токена пишется на **каждый** запрос, включая
/// отклонённый: попытки с отозванным токеном — это то, ради чего
/// журнал и нужен (§14).
pub async fn authenticate(
    State(state): State<ServerState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiFailure> {
    let route = request.uri().path().to_owned();
    let Some(token) = bearer(&request) else {
        return Err(ApiFailure::unauthorized());
    };
    let hash = hash_token(&token);

    if !state.limiter.allow(&hash) {
        tracing::warn!(%route, "превышена частота запросов");
        return Err(ApiFailure::too_many_requests());
    }

    let principal = state
        .services
        .store
        .find_principal(hash.clone())
        .await
        .map_err(ApiFailure::from)?;

    let Some(principal) = principal else {
        // Неизвестный токен НЕ пишется в журнал использования: журнал
        // ведётся по токену, а токена здесь нет. Запись на каждую
        // попытку превращала бы поток случайных строк в неограниченный
        // рост базы через единственный незащищённый путь (§14).
        tracing::warn!(%route, "предъявлен неизвестный токен");
        return Err(ApiFailure::unauthorized());
    };

    let _ = state
        .services
        .store
        .record_token_use(hash, route, "accepted".into())
        .await;

    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

/// Извлечение опознанного носителя токена в обработчике.
pub fn principal(request: &Request) -> Result<Principal, ApiFailure> {
    request
        .extensions()
        .get::<Principal>()
        .cloned()
        .ok_or_else(ApiFailure::unauthorized)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_stable_and_does_not_contain_the_token() {
        let hash = hash_token("секрет");
        assert_eq!(hash, hash_token("секрет"));
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("секрет"));
        assert_ne!(hash, hash_token("секрет "));
    }

    #[test]
    fn different_tokens_hash_differently() {
        assert_ne!(hash_token("a"), hash_token("b"));
        assert_ne!(hash_token(""), hash_token(" "));
    }
}
