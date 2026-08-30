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
