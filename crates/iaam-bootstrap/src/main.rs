//! Точка сборки (§3.2).
//!
//! Единственное место, знающее одновременно про транспорт и про адаптеры.
//! Заслон архитектуры проверяет, что это остаётся правдой.

mod config;

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::SystemClock;
use iaam_core::ids::OwnerId;
use iaam_server::auth::hash_token;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use iaam_store::tokens::{TokenRecord, TokenScope};
use rand::TryRng;
use uuid::Uuid;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Логирование обязательно: без него отладка приёмки невозможна.
    // Чувствительные поля не логируются никогда — сам токен в лог
    // не попадает, только его хеш (§14).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let store = SqliteStore::open(&config.database)?;

    // Разовая выдача токена владельца: без него в систему не войти,
    // а откладывать аутентификацию нельзя (§14).
    if let Ok(label) = std::env::var("IAAM_ISSUE_OWNER_TOKEN") {
        let token = issue_owner_token(&store, &label)?;
        println!("{token}");
        return Ok(());
    }

    let services = Arc::new(AppServices::new(
        Arc::new(SqliteAdapter::new(store)),
        Arc::new(SystemClock),
    ));
    let limiter = Arc::new(RateLimiter::new(config.rate_limit, config.rate_window));
    let (router, _api) = build(ServerState::new(services, limiter));

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address = %config.listen, "сервер запущен");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Выдача токена владельца. Сам токен печатается **один раз** и нигде
/// не сохраняется: в базе лежит только его хеш.
fn issue_owner_token(
    store: &SqliteStore,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Криптографический источник, а не `rand::rng()`: токен — это ключ
    // от чужих денег, и слабый генератор здесь дороже всего остального
    // в этом файле.
    let mut bytes = [0_u8; 32];
    rand::rngs::SysRng.try_fill_bytes(&mut bytes)?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    store.insert_token(
        &TokenRecord {
            id: Uuid::new_v4(),
            owner: OwnerId::new_random(),
            label: label.to_owned(),
            scope: TokenScope::Owner,
            revoked: false,
        },
        &hash_token(&token),
    )?;
    Ok(token)
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("получен сигнал остановки");
}
