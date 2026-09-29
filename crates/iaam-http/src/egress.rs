use std::ffi::OsString;

use thiserror::Error;

pub const BROKER_EGRESS_ENV: &str = "IAAM_BROKER_EGRESS";
pub const EGRESS_DIRECTORY: &str = "/var/lib/iaam/egress";

/// Whether this process may send requests to a broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerEgress {
    /// Broker destinations are refused before the tally or transport.
    Off,
    /// Broker destinations use the fixed per-machine egress directory.
    On,
}

/// Invalid broker-egress process configuration.
#[derive(Debug, Error)]
pub enum BrokerEgressConfigError {
    #[error("{BROKER_EGRESS_ENV} has invalid value {value:?}; use `on` or `off`")]
    InvalidSwitch { value: String },
    #[error("{BROKER_EGRESS_ENV} is not valid Unicode; use `on` or `off`")]
    NonUnicodeSwitch,
}

impl BrokerEgress {
    /// Read the process's broker-egress switch.
    ///
    /// The switch defaults to `off`. When it is `on`, the gateway opens the
    /// compiled-in per-machine egress directory; no environment variable can
    /// choose another tally or endpoint-owner identity.
    ///
    /// # Errors
    /// An invalid switch value or a non-Unicode switch.
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
            "on" => Ok(Self::On),
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
    fn broker_egress_accepts_explicit_on_without_another_setting() {
        let configured = BrokerEgress::from_lookup(values(&[(BROKER_EGRESS_ENV, "on")])).unwrap();

        assert_eq!(configured, BrokerEgress::On);
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
}
