//! Точка сборки (§3.2).
//!
//! Единственное место, знающее одновременно про транспорт и про адаптеры.
//! Заслон архитектуры проверяет, что это остаётся правдой.

mod config;
mod provision;

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::SystemClock;
use iaam_broker::credentials::Key;
use iaam_core::ids::OwnerId;
use iaam_server::auth::hash_token;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use iaam_store::tokens::{TokenRecord, TokenScope};
use rand::TryRng;
use uuid::Uuid;
use zeroize::Zeroizing;

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
    let mut store = SqliteStore::open(&config.database)?;

    // Разовая выдача токена владельца: без него в систему не войти,
    // а откладывать аутентификацию нельзя (§14).
    if let Ok(label) = std::env::var("IAAM_ISSUE_OWNER_TOKEN") {
        let token = issue_owner_token(&store, &label)?;
        println!("{token}");
        return Ok(());
    }

    // Заведение ключа шифрования брокерских доступов. Ключ создаёт сама
    // программа и наружу не отдаёт: то, чего человек не увидел, он не
    // может ни переслать, ни записать не туда (§14).
    if std::env::var("IAAM_GENERATE_BROKER_KEY").is_ok() {
        let path = broker_key_path(&config)?;
        Key::create_at(&path)?;
        println!("ключ заведён: {}", path.display());
        return Ok(());
    }

    // Приём брокерского токена. Токен читается со стандартного ввода,
    // а не из аргумента командной строки: список процессов виден всей
    // машине, а история командной оболочки переживает сессию.
    if let Ok(broker) = std::env::var("IAAM_ADD_BROKER_ACCESS") {
        let key = Key::from_file(&broker_key_path(&config)?)?;
        let id = provision::add_broker_access(&mut store, &key, &broker, &read_token()?)?;
        println!("доступ к брокеру {broker} заведён: {id}");
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

/// Путь к файлу ключа. Отсутствие переменной — отказ, а не умолчание.
fn broker_key_path(config: &Config) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    config
        .broker_key
        .clone()
        .ok_or_else(|| "переменная IAAM_BROKER_KEY_FILE не задана".into())
}

/// Чтение токена со стандартного ввода.
///
/// Возвращается в зануляемой памяти: открытый токен живёт ровно до
/// шифрования и не остаётся в освобождённой памяти процесса.
fn read_token() -> Result<Zeroizing<String>, Box<dyn std::error::Error>> {
    // Подсказка идёт в поток ошибок: стандартный вывод занят ответом
    // команды, и подмешивать в него приглашение нельзя.
    eprintln!("вставьте токен брокера и завершите ввод (Ctrl-D):");
    let mut token = Zeroizing::new(String::new());
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut token)?;
    Ok(token)
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
