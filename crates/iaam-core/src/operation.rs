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
//!
//! **A key is a call that changes something, and only such a call gets one.**
//! Both readers of this list point at a call the same way: a resolution's
//! target is what would settle the item, and a caveat's remedy is what would
//! close the gap. A read settles and closes nothing, so naming one here would
//! put an entry in the queue that a client could follow to the end and find the
//! journal exactly as it was. The catalogue of source profiles an instance
//! reads exports with is the standing example — a caller choosing a profile
//! needs it, and it is still not a key. What such a caller needs is a link
//! where it already looks, which is how the assessment of an import session is
//! published: on the session, not as a target.

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
    /// Write a standing classification rule: what a row matching a condition is.
    ///
    /// Named here because the queue offers it. An answered import question whose
    /// answer wrote no rule — the answerer held a token that may not generalise
    /// — publishes the rule it would have made, and this is the call that makes
    /// it stand. Distinct from [`Self::CreateCategoryRule`], which files a
    /// journal event under one of the owner's categories: this one decides what
    /// the row **is**.
    CreateClassificationRule,
    /// Record the owner's statement about one account's transfer partners.
    RecordAccountTransferPartners,
    /// Rule an account outside the reporting perimeter, with a reason.
    ///
    /// The half of the scope decision that a contour cannot express: membership
    /// is a contour's composition, and «this account is deliberately not in any
    /// of them» is a fact about the account itself.
    RecordAccountScope,
    /// Record, or withdraw, the owner's statement that a product ceased to
    /// exist on a date (`iaam-gua5`).
    ///
    /// A **second axis** beside [`Self::RecordAccountScope`], and the two must
    /// not be read as spellings of one act. A scope decision says whether an
    /// account's money belongs in a report; a retirement says whether the
    /// product still exists. The closed term deposit that motivated it stays
    /// inside the contour precisely so that the interest it paid keeps counting
    /// as an earning and the movement that returned its balance stays internal
    /// — so the call that would have been reached for, ruling it outside the
    /// perimeter, is the one that destroys the answer.
    ///
    /// Named here because a caveat offers it: an account the owner retired that
    /// still shows a figure in the asset snapshot is a disagreement between his
    /// statement and the journal, and **withdrawing** the statement is one of
    /// the three ways the disagreement ends. The route carries both directions,
    /// which is why one key serves for both — and why the caveat that names it
    /// has to say which direction it means: recording a second retirement over
    /// one that stands is refused, so the remedy is the withdrawal and never a
    /// repetition of the act that produced the caveat.
    RecordAccountRetirement,
    /// Record operations directly into the journal, one fact each.
    ///
    /// **The only key in this vocabulary that writes a business fact without a
    /// session behind it**, and that is the whole of its case for existing
    /// separately. [`Self::OpenImportSession`] begins an import: rows are held
    /// out of the journal, questioned, and become facts at a commit. This
    /// records what the caller already knows to be true, at once — which is the
    /// shape of a §10.7 reconstructed opening, the owner's statement of what an
    /// account held before his journal begins. There is no document to open a
    /// session for and no row to question; there is one fact, and this is the
    /// call that puts it in.
    ///
    /// Not [`Self::RecordOwnerBalance`], and the difference is what a caveat
    /// naming the wrong one costs. A control assertion has no legs: it is
    /// checked against the fold and never summed into it, so it changes how a
    /// cash figure is *spelled* — `crate::reconciliation::OpeningAnchors` reads
    /// it and the figure becomes a balance — and moves no number. A
    /// reconstructed opening is an event with legs, and it is what makes the
    /// movements on an account sum to what the account actually held.
    ///
    /// Not [`Self::SubmitCorrections`] either: a correction is addressed to an
    /// event the owner names, and the state this key answers is one where the
    /// event is *absent*.
    ///
    /// Its absence is `iaam-bhu3`. The register offered the two calls above for
    /// a retired account that still shows a figure, and the mapping was choosing
    /// from a list that did not contain the answer.
    SubmitOperations,
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
    /// Read one institution's own export into an open session, through a source
    /// profile (decision 0019).
    ///
    /// **The step between opening a session and the session holding anything**,
    /// and that is its case for being a name of its own. Not
    /// [`Self::OpenImportSession`]: opening declares a session — an account, a
    /// channel, a label — and the session it makes is empty. An import stopped
    /// there records no movement, so an item that offered only the opening
    /// would send a caller to a call that leaves the account as empty as it
    /// found it. This is the call that puts the statement in, and for a cash
    /// account it is the ordinary way a history arrives at all.
    ///
    /// Not [`Self::SubmitOperations`] either, and the difference is what the
    /// caller claims to know. That key records facts the caller has already
    /// concluded, one each, straight into the journal. This one conveys a
    /// document nobody has interpreted: the profile says which column carries
    /// which cell and translates the source's own words into iaam's, and what
    /// each row **is** stays open — settled afterwards by the owner's
    /// directory, by a standing rule of his, or by his answer to a question the
    /// session raises. That is decision 0022's line, between conveying a
    /// document and interpreting one, and it is why the two are separate names
    /// for what a client could otherwise read as one act of «sending rows».
    ///
    /// Named here because it was reachable and unpublishable (`iaam-1tij`). A
    /// resolution's target is an [`OperationKey`], so while this channel had no
    /// key no item could offer it, and an agent learned the ordinary way to
    /// import a statement from a document or not at all.
    ReadImportDocument,
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
    /// The declared length is the only thing holding an eighteenth variant to
    /// this list: adding one without extending `ALL` leaves it unresolved
    /// against the contract, so extend both in the same edit.
    pub const ALL: [Self; 17] = [
        Self::CreateAccount,
        Self::CreateContour,
        Self::AddContourVersion,
        Self::RecordOwnerBalance,
        Self::CreateCategoryRule,
        Self::CreateClassificationRule,
        Self::RecordAccountTransferPartners,
        Self::RecordAccountScope,
        Self::RecordAccountRetirement,
        Self::SubmitOperations,
        Self::OpenImportSession,
        Self::SyncBroker,
        Self::ReadImportDocument,
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
            Self::CreateClassificationRule => "create_classification_rule",
            Self::RecordAccountTransferPartners => "record_account_transfer_partners",
            Self::RecordAccountScope => "record_account_scope",
            Self::RecordAccountRetirement => "record_account_retirement",
            Self::SubmitOperations => "ingest_operations",
            Self::OpenImportSession => "open_import_session",
            Self::SyncBroker => "sync_broker",
            Self::ReadImportDocument => "read_import_document",
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
