//! The names of the calls this system publishes.
//!
//! **Why an API vocabulary lives in the core.** Two things point at an
//! operation, and they are computed on opposite sides of the workspace: the
//! outstanding-work queue in `iaam-app` says what a caller may do next, and the
//! caveat register in [`crate::report::confidence`] says what would close a gap
//! a report is silent about. If each owned its own list, the report could name
//! a call the queue does not offer, or spell one that no route answers to, and
//! nothing would notice until a client followed it. So neither owns it: the
//! names live here, where both sides already depend, and there is one list.
//!
//! It is not a route table. An [`OperationKey`] is a symbol — the transport
//! resolves it to a method, a path and a request schema against the completed
//! contract, and refuses to start if any key resolves to nothing. The core
//! knows the name and nothing else about the call, in the same way
//! [`crate::report::confidence::CaveatKind::see`] knows the name of a field in
//! a response it never builds.

/// A symbolic operation identifier resolved by a transport layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationKey {
    CreateAccount,
    /// Create a contour. It creates one and only one: an existing contour is
    /// versioned through [`Self::AddContourVersion`], and the two are separate
    /// keys because they were one route, where omitting the identifier meant
    /// «mint a fresh perimeter» and produced one for an owner who wanted a
    /// second bank inside the perimeter he already had.
    CreateContour,
    /// Add a version to a contour that exists, naming it in the path.
    AddContourVersion,
    RecordOwnerBalance,
    CreateCategoryRule,
    /// Record the owner's statement about one account's transfer partners.
    RecordAccountTransferPartners,
    /// Rule an account outside the reporting perimeter, with a reason.
    ///
    /// The half of the scope decision that a contour cannot express: membership
    /// is a contour's composition, and «this account is deliberately not in any
    /// of them» is a fact about the account itself.
    RecordAccountScope,
    /// Open the import session a document's rows are held in before commit.
    ///
    /// The first of the two ways into an account that holds nothing, and the one
    /// that takes a statement the owner fetched himself.
    OpenImportSession,
    /// Synchronise one broker channel over an interval.
    ///
    /// The second, and the only one that needs no document: the channel fetches
    /// and records in a single call, which is why it is a remedy entire rather
    /// than the first step of one.
    SyncBroker,
    /// Answer one classification question held open by an import session.
    AnswerImportQuestion,
    /// Write everything one import session holds into the journal, once.
    ///
    /// Named beside [`Self::AbandonImportSession`] because the two are the only
    /// ways an open session ends, and a refusal that offers one without the
    /// other tells the owner he must finish an import he may have decided
    /// against.
    CommitImportSession,
    /// End an import session without writing anything.
    ///
    /// The journal is neither read nor written: what the session held was never
    /// a fact. This is the only key whose route takes no request body, which is
    /// why an operation's request schema is optional — see
    /// `iaam_server::action_catalog`.
    AbandonImportSession,
    /// Retract or supersede events the owner names, one correction fact each.
    ///
    /// The only operation that acts on a reconciliation discrepancy, and it acts
    /// on both of its sides: `ReconciliationLedger::build_with` resolves
    /// corrections before it collects assertion groups, so retracting a
    /// `ControlAssertion` removes the claim, and `observe` runs over the same
    /// effective set, so superseding a journal event changes what was observed.
    SubmitCorrections,
}

impl OperationKey {
    /// Every key, in declaration order.
    ///
    /// Here so that the transport can resolve the whole vocabulary rather than a
    /// list it repeats by hand. A key omitted from such a list is a key nothing
    /// checks against the contract, and a caveat or an action naming it would
    /// have found out at the moment a caller asked for it.
    ///
    /// The declared length is the only thing holding a fourteenth variant to
    /// this list: adding one without extending `ALL` leaves it unresolved
    /// against the contract, so extend both in the same edit.
    pub const ALL: [Self; 13] = [
        Self::CreateAccount,
        Self::CreateContour,
        Self::AddContourVersion,
        Self::RecordOwnerBalance,
        Self::CreateCategoryRule,
        Self::RecordAccountTransferPartners,
        Self::RecordAccountScope,
        Self::OpenImportSession,
        Self::SyncBroker,
        Self::AnswerImportQuestion,
        Self::CommitImportSession,
        Self::AbandonImportSession,
        Self::SubmitCorrections,
    ];

    /// The route operation identifier declared by the transport.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateAccount => "create_account",
            Self::CreateContour => "create_contour_version",
            Self::AddContourVersion => "add_contour_version",
            Self::RecordOwnerBalance => "record_owner_balance",
            Self::CreateCategoryRule => "create_category_rule",
            Self::RecordAccountTransferPartners => "record_account_transfer_partners",
            Self::RecordAccountScope => "record_account_scope",
            Self::OpenImportSession => "open_import_session",
            Self::SyncBroker => "sync_broker",
            Self::AnswerImportQuestion => "answer_import_question",
            Self::CommitImportSession => "commit_import_session",
            Self::AbandonImportSession => "abandon_import_session",
            Self::SubmitCorrections => "submit_corrections",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`OperationKey::ALL`] is what the transport resolves against the
    /// contract, so a key missing from it is a key nothing checks.
    #[test]
    fn every_key_is_listed_once_and_named_once() {
        let mut codes: Vec<&str> = OperationKey::ALL.iter().map(|key| key.as_str()).collect();
        let listed = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), listed, "two operation keys share a name");
    }
}
