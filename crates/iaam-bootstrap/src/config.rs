//! Конфигурация из окружения.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("переменная {name} не задана; задайте её (допустимые значения: {allowed})")]
    Missing {
        name: &'static str,
        allowed: &'static str,
    },
    #[error("переменная {name} задана неверно: {value}; допустимые значения: {allowed}")]
    Invalid {
        name: &'static str,
        value: String,
        allowed: &'static str,
    },
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
    /// Чтение конфигурации из окружения.
    ///
    /// Умолчания есть у всего, кроме пути к базе: база в неожиданном
    /// месте — худший вид умолчания.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let database = get("IAAM_DATABASE").ok_or(ConfigError::Missing {
            name: "IAAM_DATABASE",
            allowed: "путь к файлу базы данных",
        })?;
        let listen = get("IAAM_LISTEN").unwrap_or_else(|| "127.0.0.1:8080".into());
        let listen = listen.parse().map_err(|_| ConfigError::Invalid {
            name: "IAAM_LISTEN",
            value: listen,
            allowed: "адрес socket вида 127.0.0.1:8080",
        })?;
        let rate_limit = parse_u32("IAAM_RATE_LIMIT", 120, &get)?;
        let rate_window =
            Duration::from_secs(u64::from(parse_u32("IAAM_RATE_WINDOW_SECONDS", 60, &get)?));

        Ok(Self {
            database: PathBuf::from(database),
            broker_key: get("IAAM_BROKER_KEY_FILE").map(PathBuf::from),
            listen,
            rate_limit,
            rate_window,
        })
    }
}

fn parse_u32<F>(name: &'static str, default: u32, get: &F) -> Result<u32, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    match get(name) {
        None => Ok(default),
        Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
            name,
            value,
            allowed: "целое число от 0 до 4294967295",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError};

    fn values<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn missing_database_variable_explains_how_to_set_it() {
        let error = Config::from_lookup(values(&[])).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Missing {
                name: "IAAM_DATABASE",
                ..
            }
        ));
        let text = error.to_string();
        assert!(text.contains("IAAM_DATABASE"));
        assert!(text.contains("допустимые значения"));
        assert!(text.contains("путь к файлу базы данных"));
        assert!(!text.contains("Invalid {"));
    }

    #[test]
    fn invalid_listen_value_names_allowed_form() {
        let error = Config::from_lookup(values(&[
            ("IAAM_DATABASE", "db.sqlite"),
            ("IAAM_LISTEN", "не адрес"),
        ]))
        .unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Invalid {
                name: "IAAM_LISTEN",
                ..
            }
        ));
        let text = error.to_string();
        assert!(text.contains("IAAM_LISTEN"));
        assert!(text.contains("допустимые значения"));
        assert!(text.contains("адрес socket"));
        assert!(!text.contains("Invalid {"));
    }
}
