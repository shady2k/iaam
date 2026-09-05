//! What content a profile version already names, on this instance (decision
//! 0019 §5).
//!
//! > An instance records the digest of each profile it loads and refuses to
//! > load a different content under an `(id, version)` it already recorded.
//! > Without that, "the rows version 3 read" is not a set, and constraint 6 —
//! > the facts a buggy plugin wrote are findable and retractable — is not true.
//!
//! **The catalogue cannot keep that true by itself, and it is worth being
//! precise about why.** [`super::catalogue::ProfileCatalogue::admit`] refuses
//! two files claiming one id, but only among the files of a single pass: a
//! profile edited between two starts is compared against nothing, so the
//! instance accepts the new content under the version the old one already
//! stamped on facts in the journal. That is not a hypothetical — a wave changed
//! what a bundled profile read and left its version at 1, and it was caught by
//! a human reading a doc comment, three times running, because nothing
//! mechanical could.
//!
//! So the record has to live where the process does not: in the instance's own
//! durable state. This module is the seam. `iaam-ingest` reads no database and
//! opens no file of its own — a profile is data and this crate is the engine
//! that reads it — so what lives here is the **question**, and the instance's
//! store answers it.
//!
//! What the ledger is asked is deliberately one thing and not two. «Tell me
//! what is recorded» followed by «now record this» is two calls with a gap in
//! the middle, and a second writer in that gap decides which of two contents a
//! version names. [`VersionLedger::bind`] states the content and learns the
//! answer in the same breath, so there is no gap for anything to happen in.

/// What an instance already recorded under one `(id, version)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// Nothing stood under the pair, and this content now does.
    Recorded,
    /// The pair already stood for this content, and still does.
    Unchanged,
    /// The pair stands for a different content, and this one is not it.
    ///
    /// Carries what is recorded rather than only the fact of the mismatch. A
    /// refusal that says «refused» and stops sends the operator to compare
    /// files by hand, which is the work the digest was computed to save.
    Differs { recorded: String },
}

/// The ledger could not be consulted at all.
///
/// Distinct from [`Binding::Differs`] on purpose: «the content changed» and «I
/// do not know whether it changed» are different answers, and only the first is
/// a statement about the profile. An operator whose database is unreadable must
/// be told that, and not that his profile is bad.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the recorded profile versions could not be consulted: {0}")]
pub struct LedgerUnavailable(pub String);

/// The instance's record of which content each profile version names.
///
/// Implemented against durable storage. An implementation that forgets when the
/// process ends implements nothing: the whole of what this trait is for is the
/// comparison across two starts, and a within-load comparison is what the
/// catalogue already did while the defect stood.
pub trait VersionLedger {
    /// Bind this content to this `(id, version)`, or report what is bound
    /// already.
    ///
    /// **An implementation never overwrites.** The content that stands is the
    /// one recorded first; a second one is reported and refused. Rewriting the
    /// digest under a standing pair would perform the very defect the record
    /// exists to catch.
    fn bind(&mut self, id: &str, version: u32, digest: &str) -> Result<Binding, LedgerUnavailable>;
}
