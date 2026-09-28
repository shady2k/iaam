use std::ffi::OsString;
use std::path::PathBuf;

use thiserror::Error;

pub const BROKER_EGRESS_ENV: &str = "IAAM_BROKER_EGRESS";
pub const OUTBOUND_TALLY_ENV: &str = "IAAM_OUTBOUND_TALLY";

/// Whether this process may send requests to a broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerEgress {
    /// Broker destinations are refused before the tally or transport.
    Off,
    /// Broker destinations use the named tally and acquire one lifetime owner
    /// lock per endpoint.
    On { tally: PathBuf },
}

/// Invalid broker-egress process configuration.
#[derive(Debug, Error)]
pub enum BrokerEgressConfigError {
    #[error("{BROKER_EGRESS_ENV} has invalid value {value:?}; use `on` or `off`")]
    InvalidSwitch { value: String },
    #[error("{BROKER_EGRESS_ENV} is not valid Unicode; use `on` or `off`")]
    NonUnicodeSwitch,
    #[error(
        "{BROKER_EGRESS_ENV}=on requires {OUTBOUND_TALLY_ENV} to name the canonical per-machine tally file"
    )]
    MissingTally,
}

impl BrokerEgress {
    /// Read the process's broker-egress switch and tally path.
    ///
    /// The switch defaults to `off`. `IAAM_OUTBOUND_TALLY` is required only
    /// when the switch is `on`; no path is guessed.
    ///
    /// # Errors
    /// An invalid switch value, a non-Unicode switch, or a missing tally path.
    pub fn from_env() -> Result<Self, BrokerEgressConfigError> {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    fn from_lookup<F>(get: F) -> Result<Self, BrokerEgressConfigError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let Some(value) = get(BROKER_EGRESS_ENV) else {
            return Ok(Self::Off);
        };
        let value = value
            .into_string()
            .map_err(|_| BrokerEgressConfigError::NonUnicodeSwitch)?;
        match value.as_str() {
            "off" => Ok(Self::Off),
            "on" => {
                let tally = get(OUTBOUND_TALLY_ENV)
                    .map(PathBuf::from)
                    .ok_or(BrokerEgressConfigError::MissingTally)?;
                Ok(Self::On { tally })
            }
            _ => Err(BrokerEgressConfigError::InvalidSwitch { value }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn broker_egress_defaults_to_off() {
        assert_eq!(
            BrokerEgress::from_lookup(values(&[])).unwrap(),
            BrokerEgress::Off
        );
    }

    #[test]
    fn broker_egress_accepts_explicit_off_without_a_tally() {
        assert_eq!(
            BrokerEgress::from_lookup(values(&[(BROKER_EGRESS_ENV, "off")])).unwrap(),
            BrokerEgress::Off
        );
    }

    #[test]
    fn broker_egress_on_requires_and_keeps_the_tally_path() {
        let configured = BrokerEgress::from_lookup(values(&[
            (BROKER_EGRESS_ENV, "on"),
            (OUTBOUND_TALLY_ENV, "/run/iaam/outbound-tally"),
        ]))
        .unwrap();

        assert_eq!(
            configured,
            BrokerEgress::On {
                tally: PathBuf::from("/run/iaam/outbound-tally")
            }
        );
    }

    #[test]
    fn invalid_switch_names_the_value_and_allowed_values() {
        let error = BrokerEgress::from_lookup(values(&[(BROKER_EGRESS_ENV, "yes")])).unwrap_err();
        let message = error.to_string();

        assert!(message.contains(BROKER_EGRESS_ENV), "{message}");
        assert!(message.contains("yes"), "{message}");
        assert!(message.contains("on"), "{message}");
        assert!(message.contains("off"), "{message}");
    }

    #[test]
    fn enabled_switch_without_a_tally_names_the_missing_input() {
        let error = BrokerEgress::from_lookup(values(&[(BROKER_EGRESS_ENV, "on")])).unwrap_err();
        let message = error.to_string();

        assert!(message.contains(BROKER_EGRESS_ENV), "{message}");
        assert!(message.contains(OUTBOUND_TALLY_ENV), "{message}");
    }
}
