//! Configuration from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("variable {name} is not set; set it (allowed values: {allowed})")]
    Missing {
        name: &'static str,
        allowed: &'static str,
    },
    #[error("variable {name} is invalid: {value}; allowed values: {allowed}")]
    Invalid {
        name: &'static str,
        value: String,
        allowed: &'static str,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database: PathBuf,
    /// File containing the encryption key for broker access.
    ///
    /// Optional: needed only by commands that work with broker access.
    /// It has no default and cannot have one — a key “in a known place”
    /// would be known to everyone.
    pub broker_key: Option<PathBuf>,
    pub listen: SocketAddr,
    pub rate_limit: u32,
    pub rate_window: Duration,
}

impl Config {
    /// Read configuration from the environment.
    ///
    /// Everything except the database path has a default: a database in
    /// an unexpected location is the worst kind of default.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let database = get("IAAM_DATABASE").ok_or(ConfigError::Missing {
            name: "IAAM_DATABASE",
            allowed: "database file path",
        })?;
        let listen = get("IAAM_LISTEN").unwrap_or_else(|| "127.0.0.1:8080".into());
        let listen = listen.parse().map_err(|_| ConfigError::Invalid {
            name: "IAAM_LISTEN",
            value: listen,
            allowed: "socket address such as 127.0.0.1:8080",
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
            allowed: "integer from 0 to 4294967295",
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
        assert!(text.contains("allowed values"));
        assert!(text.contains("database file path"));
        assert!(!text.contains("Invalid {"));
    }

    #[test]
    fn invalid_listen_value_names_allowed_form() {
        let error = Config::from_lookup(values(&[
            ("IAAM_DATABASE", "db.sqlite"),
            ("IAAM_LISTEN", "not an address"),
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
        assert!(text.contains("allowed values"));
        assert!(text.contains("socket address"));
        assert!(!text.contains("Invalid {"));
    }
}
