//! Конфигурация из окружения.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("переменная {name} задана неверно: {value}")]
    Invalid { name: &'static str, value: String },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database: PathBuf,
    /// Файл с ключом шифрования брокерских доступов.
    ///
    /// Необязателен: нужен только командам, работающим с доступом
    /// к брокеру. Умолчания у него нет и быть не может — ключ
    /// «в известном месте» известен всем.
    pub broker_key: Option<PathBuf>,
    pub listen: SocketAddr,
    pub rate_limit: u32,
    pub rate_window: Duration,
}

impl Config {
    /// Чтение конфигурации.
    ///
    /// Умолчания есть у всего, кроме пути к базе: база в неожиданном
    /// месте — худший вид умолчания.
    pub fn from_env() -> Result<Self, ConfigError> {
        let database = std::env::var("IAAM_DATABASE").map_err(|_| ConfigError::Invalid {
            name: "IAAM_DATABASE",
            value: "не задана".into(),
        })?;
        let listen = std::env::var("IAAM_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());
        let listen = listen.parse().map_err(|_| ConfigError::Invalid {
            name: "IAAM_LISTEN",
            value: listen.clone(),
        })?;
        let rate_limit = parse_u32("IAAM_RATE_LIMIT", 120)?;
        let rate_window =
            Duration::from_secs(u64::from(parse_u32("IAAM_RATE_WINDOW_SECONDS", 60)?));

        Ok(Self {
            database: PathBuf::from(database),
            broker_key: std::env::var("IAAM_BROKER_KEY_FILE")
                .ok()
                .map(PathBuf::from),
            listen,
            rate_limit,
            rate_window,
        })
    }
}

fn parse_u32(name: &'static str, default: u32) -> Result<u32, ConfigError> {
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(value) => value
            .parse()
            .map_err(|_| ConfigError::Invalid { name, value }),
    }
}
