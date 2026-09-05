//! The instance's record of which content each profile version names.
//!
//! `iaam-ingest` asks one question — which content does this `(id, version)`
//! already stand for? — and reads no database of its own, because a profile is
//! data and that crate is the engine that reads it. This adapter is the answer,
//! and it is the store's, for the reason decision 0019 §5 gives: the binding
//! has to be compared across two starts of the instance, so it has to live in
//! the one part of the instance that a process ending does not take with it.

use iaam_ingest::profile::{Binding, LedgerUnavailable, VersionLedger};
use iaam_store::SqliteStore;
use iaam_store::source_profiles::ProfileBinding;

/// The recorded bindings of one instance, read straight off its database.
///
/// Borrows the store rather than owning it, and holds no lock: binding happens
/// once, at start-up, before the store is handed to the adapter that serves
/// requests. Doing it there rather than on first use is deliberate — an
/// instance that discovered a changed profile in the middle of an import would
/// refuse a document it had already begun reading.
pub struct StoreVersionLedger<'a> {
    store: &'a SqliteStore,
}

impl<'a> StoreVersionLedger<'a> {
    #[must_use]
    pub const fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }
}

impl VersionLedger for StoreVersionLedger<'_> {
    /// A storage failure is reported as «unknown» and never as «unchanged».
    ///
    /// The catalogue refuses on an unanswered question, which is the only safe
    /// reading: a database that cannot be consulted says nothing about whether
    /// the content changed, and installing on the strength of that is the
    /// silent acceptance the whole decision refuses.
    fn bind(&mut self, id: &str, version: u32, digest: &str) -> Result<Binding, LedgerUnavailable> {
        match self.store.bind_source_profile_version(id, version, digest) {
            Ok(ProfileBinding::Recorded) => Ok(Binding::Recorded),
            Ok(ProfileBinding::Unchanged) => Ok(Binding::Unchanged),
            Ok(ProfileBinding::Differs { recorded }) => Ok(Binding::Differs { recorded }),
            Err(error) => Err(LedgerUnavailable(error.to_string())),
        }
    }
}
