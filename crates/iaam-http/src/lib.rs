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
//! [`Outbound`] the one type a caller holds it by. Routing, retries and the
//! circuit breaker live there. MOEX and CBR pacing uses an in-process host
//! lane; broker budgets, host spacing and the UTC daily ceiling use a locked
//! per-machine outbound tally. A caller that reached the transport directly
//! would skip those rules without a single error. So the production transport
//! cannot be built outside this crate — [`Gateway::production`] is how a
//! process gets it, already inside its gateway — and outside it neither of
//! these compiles:
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
//! - no crate but this one depends on `reqwest` or any other HTTP client
//!   crate, under its own name, under another (`package = "reqwest"`) or
//!   through the workspace, and none builds a `reqwest` client by path;
//! - `serve` builds one gateway with [`Gateway::production`] and shares it
//!   throughout the server process. [`Gateway::new`] and
//!   `Gateway::with_parts` stay public for tests, which build their own over a
//!   fake transport and clock.
//!
//! In-process lanes, named waits and breakers do not survive a restart. The
//! broker tally does and is shared by every process given its path. The
//! deployment contract is in `docs/deployment.md` §1.1.

pub mod client;
pub mod destination;
mod egress;
pub mod gateway;
pub mod request;
pub mod resilience;
pub mod response;
mod tally;
pub mod trust;

pub use destination::Destination;
pub use egress::{BrokerEgress, BrokerEgressConfigError};
pub use gateway::{Gateway, GatewayError, Outbound};
pub use request::{AuthScheme, HttpMethod, HttpRequest, RequestBody, Secret};
pub use response::{HttpError, HttpResponse};
