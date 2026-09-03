//! Scenarios: collect a snapshot, call the core, save the result.
//!
//! There is not a single line of money arithmetic here. Every number
//! appearing in the API response comes from `iaam-core` (§3.1, §13).

pub mod categories;
pub mod import_session;
pub mod ingest;
pub mod journal;
pub mod market_reference;
pub mod reports;
pub mod schedule;

pub mod broker_dictionary;
pub mod classification;
pub mod correction;
pub mod custody_repair;
pub mod documents;
pub mod reconciliation;
