//! Outgoing HTTP: transport, trust, resilience.
//!
//! The only crate in the tree that declares the HTTP client. Source crates
//! (`iaam-broker`, `iaam-market`) describe requests and parse responses; neither
//! operation touches the network, so both are checked against frozen samples.
//!
//! The rule is enforced by a guard: `scripts/check-architecture.sh` forbids
//! `reqwest` in every crate except this one.

pub mod client;
pub mod destination;
pub mod request;
pub mod resilience;
pub mod response;
pub mod trust;

pub use destination::Destination;
pub use request::{HttpMethod, HttpRequest, RequestBody, Secret};
pub use response::{HttpError, HttpResponse};
