//! Outgoing HTTP: transport, trust, resilience, and the gateway every
//! outbound call goes through.
//!
//! The only crate in the tree that declares the HTTP client. Source crates
//! (`iaam-broker`, `iaam-market`) describe requests and parse responses; neither
//! operation touches the network, so both are checked against frozen samples.
//!
//! # Every outbound call goes through the gateway
//!
//! [`Gateway::send`] is the only public way to send a request, and
//! [`Outbound`] the one type a caller holds it by. Budgets, the
//! one-request-per-host lane, retries and the circuit breaker live there, and
//! a caller that reached the transport directly would skip all of them
//! without a single error: that is how the calls this crate once served
//! outgrew their limits. So the production transport cannot be built outside
//! this crate — [`Gateway::production`] is how a process gets it, already
//! inside its gateway — and outside it neither of these compiles:
//!
//! ```compile_fail,E0624
//! use iaam_http::client::HttpClient;
//!
//! let _ = HttpClient::new();
//! ```
//!
//! ```compile_fail,E0599
//! use iaam_http::client::HttpClient;
//!
//! let _ = HttpClient::default();
//! ```
//!
//! What the compiler cannot close is enforced by guards in
//! `scripts/check-architecture.sh`, run by `make arch`:
//!
//! - no crate but this one declares `reqwest` or builds a `reqwest` client;
//! - production code builds the gateway in one place, `serve` in
//!   `iaam-bootstrap`: the gateway is one per process and shared, because two
//!   gateways are two budgets against the same destination.

pub mod client;
pub mod destination;
pub mod gateway;
pub mod request;
pub mod resilience;
pub mod response;
pub mod trust;

pub use destination::Destination;
pub use gateway::{Gateway, GatewayError, Outbound};
pub use request::{HttpMethod, HttpRequest, RequestBody, Secret};
pub use response::{HttpError, HttpResponse};
