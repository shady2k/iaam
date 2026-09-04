//! Ingestion (§10).
//!
//! Unified entry point: manual input, CSV, and external agents all come here.
//! Parsing is line-by-line—the document as a whole is not rejected because of one
//! unrecognized line (§10.1), and every line receives a verdict (§10.4).
//!
//! **Ingestion builds signs and legs, not the client.** The client sends
//! a positive amount and the operation kind; converting them into event legs
//! with the correct signs is this crate's job. Otherwise, the sign convention
//! becomes part of the public contract, and the external agent must
//! know it, even though arithmetic is forbidden to it (§13).

pub mod classification;
pub mod csv_source;
pub mod dedup;
pub mod journal_event;
pub mod observation;
pub mod operation;
pub mod profile;
pub mod report;
pub mod verdict;

pub use journal_event::{
    JournalEventEnrichment, JournalFact, SubmittedJournalEvent, normalize_journal_event,
};
pub use observation::{ObservedCounterparty, ObservedDirection, ObservedRow, RowIdentity};
pub use operation::{Normalized, OperationDates, OperationKind, SubmittedOperation, normalize};
pub use profile::{ProfileCatalogue, SourceProfile};
pub use verdict::{Rejection, Verdict};
