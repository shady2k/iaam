//! Broker environment: production and sandbox (§14).
//!
//! T-Invest calls this a contour, but “contour” is already occupied in this
//! system by the accounting contour (§11), and the second meaning would be
//! read as the first. Here it is an environment.
//!
//! Environments differ by more than address. Their tokens are **different**:
//! the production token gets `401` “Authentication token is missing or
//! invalid” from the sandbox gateway, while sandbox rejects production
//! methods. Therefore environment is a property of configured access, not a
//! parameter of an individual request: an environment selected in error is a
//! request sent to the wrong place, detected by the other side's response.
//!
//! The gateway address is derived from the environment, not stored beside
//! access: the address belongs to the environment, and an access record that
//! supplied its own would let configured access redirect the program anywhere.

/// Broker channel environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Environment {
    /// Production: real money and real trading history.
    Prod,
    /// Sandbox: simulated trading. No broker report exists there.
    Sandbox,
}

impl Environment {
    /// Code for storage. Storage does not interpret the environment; it stores it.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Sandbox => "sandbox",
        }
    }

    /// Parse a code.
    ///
    /// No `trim` and no case folding: the value is written by the system,
    /// not a person, and “almost the same” here means another party changed
    /// the record.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "prod" => Some(Self::Prod),
            "sandbox" => Some(Self::Sandbox),
            _ => None,
        }
    }

    /// Gateway address.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::Prod => "https://invest-public-api.tbank.ru/rest",
            Self::Sandbox => "https://sandbox-invest-public-api.tbank.ru/rest",
        }
    }

    /// Whether a method exists in this environment.
    ///
    /// Two methods are entirely absent in the sandbox: the broker report and
    /// the income statement for foreign issuers. Refuse here, before making
    /// the request: the gateway answers such a call with an empty response,
    /// and an empty report is indistinguishable from a report containing
    /// nothing. That is the worst kind of error: a silent one.
    #[must_use]
    pub fn serves(self, method: Method) -> bool {
        match self {
            Self::Prod => true,
            Self::Sandbox => match method {
                Method::BrokerReport | Method::DividendsForeignIssuer => false,
                Method::Accounts | Method::Operations | Method::Portfolio => true,
            },
        }
    }
}

/// Gateway method to the extent known by the environment.
///
/// An enum rather than a string containing the full method name: the string
/// answers “what should be sent?”, while this must answer “does this exist
/// here?”, and a typo in the string would silently answer “yes”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Method {
    Accounts,
    Operations,
    Portfolio,
    BrokerReport,
    DividendsForeignIssuer,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_environment_survives_the_round_trip_through_its_code() {
        for environment in [Environment::Prod, Environment::Sandbox] {
            assert_eq!(Environment::parse(environment.code()), Some(environment));
        }
    }

    #[test]
    fn nothing_else_parses_as_an_environment() {
        // “Almost the same” is another party's record, not ours.
        for code in ["", "PROD", " prod", "prod ", "sandbox-ish", "test"] {
            assert_eq!(Environment::parse(code), None, "{code}");
        }
    }

    #[test]
    fn the_environments_never_share_an_address() {
        // One address for two environments would mean production trades
        // from a verification run.
        assert_ne!(
            Environment::Prod.base_url(),
            Environment::Sandbox.base_url()
        );
        assert!(Environment::Sandbox.base_url().contains("sandbox"));
        assert!(!Environment::Prod.base_url().contains("sandbox"));
    }

    #[test]
    fn the_report_is_refused_in_the_sandbox_and_served_in_prod() {
        assert!(!Environment::Sandbox.serves(Method::BrokerReport));
        assert!(!Environment::Sandbox.serves(Method::DividendsForeignIssuer));
        assert!(Environment::Prod.serves(Method::BrokerReport));
        assert!(Environment::Prod.serves(Method::DividendsForeignIssuer));
    }

    #[test]
    fn what_the_sandbox_does_have_is_not_refused() {
        for method in [Method::Accounts, Method::Operations, Method::Portfolio] {
            assert!(Environment::Sandbox.serves(method), "{method:?}");
        }
    }
}
