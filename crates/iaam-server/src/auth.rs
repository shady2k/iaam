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
use sha2::{Digest, Sha256};

use crate::ServerState;
use crate::error::ApiFailure;

/// Хеш токена.
///
/// SHA-256, а не пароль-хеш: токен — это 256 случайных бит из системного
/// источника, перебирать его нечем, и argon2 на каждом запросе стоит
/// дороже, чем даёт. Для паролей владельца — если они когда-нибудь
/// появятся — вывод обратный.
///
/// **Сравнения за постоянное время здесь нет, и это осознанно.** Поиск
/// идёт запросом `WHERE token_hash = ?`, то есть сравнение выполняет
/// SQLite, и оно не является постоянным по времени. Утечка времени
/// сравнения даёт атакующему возможность подбирать хеш по префиксу —
/// но подбирать нужно образ SHA-256 от 256-битного случайного значения,
/// а не сам токен. Функция «постоянное сравнение», не используемая
/// на пути аутентификации, обещала бы защиту, которой нет: такая
/// функция здесь была и удалена.
#[must_use]
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

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
