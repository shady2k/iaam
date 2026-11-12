//! Broker channel access (§14).
//!
//! This crate knows about access encryption and the permissions with which
//! the system talks to a broker. It knows **nothing** about the application
//! or storage: the `BrokerChannel` port lives in `iaam-app`, because
//! object-safe async traits exist only there (§3.2), and the application
//! adapter connects them, as it already does for SQLite.

pub mod credentials;
pub mod environment;
pub mod finam;
pub mod operation_kind;
pub mod tinkoff;

/// Reference data read from a broker's instrument description.
/// Missing ISIN and unsupported kinds remain explicit for the application to refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerInstrumentDescription {
    pub isin: Option<String>,
    pub ticker: String,
    pub name: String,
    pub kind: Option<iaam_core::instrument::InstrumentKind>,
    pub currency: iaam_core::money::CurrencyCode,
    pub mic: Option<String>,
    pub board: Option<String>,
    pub lot: Option<u32>,
    pub price_decimals: Option<u8>,
    /// T-Invest reports that the instrument is blocked by the trading/clearing centre.
    pub blocked_tca_flag: bool,
}
