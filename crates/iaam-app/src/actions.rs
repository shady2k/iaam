use std::collections::BTreeMap;

use crate::error::AppError;
use crate::ports::{
    AccountActivityView, AccountScopeExclusionView, AccountTransferStatementView, AccountView,
    ClassificationRuleStore, ContourView, ControlAssertionView, ImportQuestionView,
    ImportSessionState, ImportSessionSummaryView, Scope, Store, required_scope,
};
use crate::scenarios::classification::{matcher_request_json, outcome_json, rule_from_view};
use crate::scenarios::import_session::{self, Generalisation};
use crate::scenarios::reports::MoneyFlowReport;
use iaam_core::event::correction::resolve;
use iaam_core::event::source_row::RowName;
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::money::{CurrencyCode, Money};
use iaam_core::projection::ProjectionError;
use iaam_core::projection::balances::Balances;
use iaam_core::projection::money_flow::UndecomposedCause;
use iaam_core::reconciliation::check::{ClaimOutcome, ClaimValue, Discrepancy};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger, Taint};
use iaam_ingest::Verdict;
use iaam_ingest::classification::{
    Classification, ClassificationRule, ClassificationSubject, Question, RuleMatcher,
};
use time::Date;

/// The policy-visible kind of an outstanding action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKind {
    CreateFirstAccount,
    /// A document this instance kept printed an account name, and the owner's
    /// directory resolves it to no single account of his.
    ///
    /// Declared straight after [`Self::CreateFirstAccount`], and emitted there,
    /// because it is the same act said precisely: «create an account» and
    /// «create the account this statement calls this». An empty instance raises
    /// the first and can raise no second — nothing has been read yet — and the
    /// moment a document has been handed over, this one names what to create
    /// instead of leaving the caller to provoke a refusal for it.
    ///
    /// It outlives the item above it, which is why it is a kind of its own
    /// rather than a widening: `CreateFirstAccount` is existential and stops
    /// being raised after the first account exists, while a statement naming
    /// seven accounts still wants six more.
    CreateAccountNamedByDocument,
    CreateFirstContour,
    AccountScopeUndecided,
    /// The owner has not said which of his accounts money moves between this
    /// one and. A discovery goal: it is asked before anything is imported.
    ResolveTransferRelationships,
    StartAccountImport,
    ProvideControlAssertion,
    /// The owner retired a product and the journal still shows a figure on it,
    /// so the row he asked to have removed is still in the asset snapshot.
    ///
    /// Declared straight after the control assertion, and emitted there, because
    /// the two are the same shortfall seen from two ends: the account's history
    /// begins mid-way, so the fold is a movement from an unknown start and it
    /// does not come to zero. The frontier's kinds must stay non-decreasing in
    /// this enum's order.
    RetiredAccountNotEmpty,
    /// The owner retired something and the queue could not find out whether the
    /// journal agrees, because the journal would not fold (`iaam-4jso`).
    ///
    /// It stands in for [`Self::RetiredAccountNotEmpty`] and is emitted where
    /// that one would be: same read, same place in the order, and exactly one
    /// of the two can be raised for a given fold. Declared straight after it
    /// for that reason — the frontier's kinds must stay non-decreasing in this
    /// enum's order.
    ///
    /// It exists because the alternatives are both dishonest. Failing the
    /// request takes away the queue, which is the surface the owner recovers
    /// *from*; guessing the item away publishes «nothing outstanding» about a
    /// question nobody could answer, which is worse than a loud failure because
    /// it reads as an answer.
    RetirementNotAssessed,
    /// A row the source described without a settled direction or counterparty
    /// is held in an import session, and the owner has not said what it was.
    ///
    /// Declared here, between the import and the diagnostics, because
    /// [`frontier`] emits its items last and one of its tests requires the
    /// frontier's order to be non-decreasing in this enum's order.
    AnswerClassificationQuestion,
    /// A question the owner answered wrote no standing rule, and one was
    /// possible. He is the only one who can make it stand.
    ///
    /// Declared straight after the question it comes out of, because
    /// [`actions_from_state`] emits it there and the frontier's order must be
    /// non-decreasing in this enum's order.
    AdoptClassificationRule,
    /// An import session holds rows and has not ended. The rows are in no
    /// journal, and only the owner can say whether they should be.
    ///
    /// Declared after the two items about a session's questions and before the
    /// diagnostics, because [`actions_from_state`] emits it there and the
    /// frontier's order must be non-decreasing in this enum's order.
    ImportSessionUnfinished,
    CoverageGapUnrepaired,
    IndependentConfirmationMissing,
    DiscrepancyUnresolved,
    UndecomposedOutflows,
    ExternalTransfersUncategorised,
    UnexplainedResidual,
    PossibleDuplicateUndecided,
}

impl ActionKind {
    /// The stable identity used to distinguish this kind from other actions.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::CreateFirstAccount => "create_first_account",
            Self::CreateAccountNamedByDocument => "create_account_named_by_document",
            Self::CreateFirstContour => "create_first_contour",
            Self::AccountScopeUndecided => "account_scope_undecided",
            Self::ResolveTransferRelationships => "resolve_transfer_relationships",
            Self::StartAccountImport => "start_account_import",
            Self::ProvideControlAssertion => "provide_control_assertion",
            // The same code the caveat register carries for the same state, and
            // deliberately so: a client holding a snapshot with
            // `retired_account_not_empty` in its `confidence` and a queue with
            // an item of this kind is holding one fact twice, and the two names
            // agreeing is what lets it say so.
            Self::RetiredAccountNotEmpty => "retired_account_not_empty",
            Self::RetirementNotAssessed => "retirement_not_assessed",
            Self::AnswerClassificationQuestion => "answer_classification_question",
            Self::AdoptClassificationRule => "adopt_classification_rule",
            Self::ImportSessionUnfinished => "import_session_unfinished",
            Self::CoverageGapUnrepaired => "coverage_gap_unrepaired",
            Self::IndependentConfirmationMissing => "independent_confirmation_missing",
            Self::DiscrepancyUnresolved => "discrepancy_unresolved",
            Self::UndecomposedOutflows => "undecomposed_outflows",
            Self::ExternalTransfersUncategorised => "external_transfers_uncategorised",
            Self::UnexplainedResidual => "unexplained_residual",
            Self::PossibleDuplicateUndecided => "possible_duplicate_undecided",
        }
    }

    /// Every kind, in declaration order.
    pub const ALL: [Self; 19] = [
        Self::CreateFirstAccount,
        Self::CreateAccountNamedByDocument,
        Self::CreateFirstContour,
        Self::AccountScopeUndecided,
        Self::ResolveTransferRelationships,
        Self::StartAccountImport,
        Self::ProvideControlAssertion,
        Self::RetiredAccountNotEmpty,
        Self::RetirementNotAssessed,
        Self::AnswerClassificationQuestion,
        Self::AdoptClassificationRule,
        Self::ImportSessionUnfinished,
        Self::CoverageGapUnrepaired,
        Self::IndependentConfirmationMissing,
        Self::DiscrepancyUnresolved,
        Self::UndecomposedOutflows,
        Self::ExternalTransfersUncategorised,
        Self::UnexplainedResidual,
        Self::PossibleDuplicateUndecided,
    ];

    /// The reports this kind's outstanding work stands between the owner and.
    ///
    /// The single table, and the reason it lives on the kind rather than at each
    /// producer: [`ActionCategory::required_for`] is the only way to build the
    /// required category, so a kind cannot be graded required for one set of
    /// goals in one branch and another set in the next.
    ///
    /// Empty for the five kinds the queue never grades required — a blocking
    /// item, two recommendations, two statements of fact — and empty is refused by
    /// [`Action::new`], so an attempt to promote one of them without deciding
    /// what it blocks fails at construction rather than publishing a required
    /// item that names nothing.
    ///
    /// Exhaustive on purpose. A twentieth kind cannot compile until someone
    /// has answered, for that kind, the question this whole type exists to
    /// answer.
    ///
    /// Each entry is what the code does, not what the item's prose suggests:
    ///
    /// - `CreateFirstContour`, `AccountScopeUndecided` — an account in no
    ///   contour is outside `report_population`'s covered set, so it is absent
    ///   from balances, flow and returns. **Not reconciliation**:
    ///   `reconciliation::report` takes an account and never resolves a contour.
    /// - `ResolveTransferRelationships` — an unpaired leg is a `CashOut` or a
    ///   `CashIn`, which `MoneyFlow::apply` counts as money crossing the
    ///   perimeter and `FlowLog` records as an external contribution or
    ///   withdrawal. **Not asset snapshot**: the leg lands on its own account's
    ///   cash whether or not its partner is known. **Not reconciliation**:
    ///   pairing rewrites two events as one with the same two legs, so observed
    ///   cash and turnover per account are unchanged.
    /// - `StartAccountImport`, `AnswerClassificationQuestion`,
    ///   `ImportSessionUnfinished` — a row that is in no journal is in no
    ///   report, and an account with no facts has nothing for any of the four
    ///   to say. A session's rows are pre-journal by construction — nothing it
    ///   holds reaches `events` until the commit writes them — so while it
    ///   stands open every report is computed as though those rows did not
    ///   exist, with nothing on the figure saying so. Abandoning satisfies the
    ///   goal too, and does not contradict this grading: it is the owner saying
    ///   the rows were never facts, after which no report is short of anything.
    /// - `ProvideControlAssertion` — the closing assertion is the claim side of
    ///   reconciliation, and the opening one is what makes the snapshot's cash a
    ///   balance: `reports::account_balances` decides `CashOpening::Asserted`
    ///   or `Unasserted` per account and currency from exactly these events,
    ///   and only the first spells the figure `CashFigure::Balance`.
    ///   **Not returns and not flow**: a control assertion has no legs, so it
    ///   moves no number in either; it only grades confidence there.
    /// - `CoverageGapUnrepaired`, `IndependentConfirmationMissing`,
    ///   `DiscrepancyUnresolved` — all three are about whether a period is
    ///   confirmed, and nothing else. `EventKind::ImportCoverageGap` says so in
    ///   as many words: it is «a statement about this attempt», not about the
    ///   interval, and the refused rows may already be in the journal from
    ///   another channel.
    /// - `PossibleDuplicateUndecided` — `DedupDecision::records_the_row` is true
    ///   for a possible duplicate, so the row **is** in the journal and may be
    ///   the same money counted twice. That is wrong in every report.
    /// - `RetiredAccountNotEmpty` — **the asset snapshot and nothing else**, and
    ///   the boundary is exact rather than cautious: a retirement is read in one
    ///   place, `iaam_core::report::assets::asset_snapshot`, where an all-zero
    ///   row of a ceased product is dropped. `contour::classify` never sees it,
    ///   so flow and returns are the same numbers with or without the
    ///   declaration, and `reconciliation::report` takes an account and never
    ///   asks whether the product still exists. The one goal it names is the one
    ///   whose register carries the caveat for the same state, which is what
    ///   makes the two joinable.
    /// - `RetirementNotAssessed` — **the same one goal, and deliberately not
    ///   more.** The item stands where `RetiredAccountNotEmpty` would have
    ///   stood, so it stands between the owner and the same report. A journal
    ///   that will not fold does of course refuse more than the snapshot; but
    ///   this item is raised only for an owner who has retired something, so
    ///   grading it against every report would tell an owner who has retired
    ///   nothing — and whose journal is just as unfoldable — nothing at all,
    ///   while telling the one who has that his retirement is what stands
    ///   between him and his money flow. The goal an item names is the goal it
    ///   is about.
    #[must_use]
    pub const fn goals(self) -> ReportGoals {
        use ReportGoal::{AssetSnapshot, MoneyFlow, Reconciliation, Returns};
        match self {
            // Blocking, not required work: no goal.
            Self::CreateFirstAccount => ReportGoals::NONE,
            // Every report, and for `StartAccountImport`'s reason exactly: the
            // records that named this account were refused, so they are in no
            // journal, so they are in no report — and nothing anywhere says a
            // month of one account is missing. Not blocking, though: the system
            // accepts every other act while this stands, which is the difference
            // from the item above.
            Self::CreateAccountNamedByDocument => ReportGoals::ALL,
            Self::CreateFirstContour | Self::AccountScopeUndecided => {
                ReportGoals::of(&[AssetSnapshot, MoneyFlow, Returns])
            }
            Self::ResolveTransferRelationships => ReportGoals::of(&[MoneyFlow, Returns]),
            Self::StartAccountImport
            | Self::AnswerClassificationQuestion
            | Self::ImportSessionUnfinished
            | Self::PossibleDuplicateUndecided => ReportGoals::ALL,
            Self::ProvideControlAssertion => ReportGoals::of(&[AssetSnapshot, Reconciliation]),
            Self::RetiredAccountNotEmpty | Self::RetirementNotAssessed => {
                ReportGoals::of(&[AssetSnapshot])
            }
            Self::CoverageGapUnrepaired
            | Self::IndependentConfirmationMissing
            | Self::DiscrepancyUnresolved => ReportGoals::of(&[Reconciliation]),
            // Recommended and informational: never required, so no goal.
            //
            // `AdoptClassificationRule` is here and not beside the question it
            // comes from, and the difference is the whole of its grading. The
            // question holds a row out of the journal, so every report is short
            // of it; the rule changes nothing already imported — the row it came
            // from is settled — and only decides what happens to rows nobody has
            // submitted yet. No report the owner can run today is waiting on it.
            Self::AdoptClassificationRule
            | Self::UndecomposedOutflows
            | Self::ExternalTransfersUncategorised
            | Self::UnexplainedResidual => ReportGoals::NONE,
        }
    }
}

/// One report the owner is trying to reach: the four scenarios this crate
/// computes — [`crate::scenarios::reports::account_balances`],
/// [`crate::scenarios::reports::money_flow`],
/// [`crate::scenarios::reports::returns`] and
/// [`crate::scenarios::reconciliation::report`].
///
/// Re-exported rather than declared here, and for the reason
/// [`OperationKey`] is. Until this wave the queue declared its own enum with the
/// same four variants and the same four `code()` strings as the one a report's
/// confidence register carries, and the two were joined only where the strings
/// met on the wire. A stopgap assertion compared the two arrays of codes, which
/// is the weakest form the promise can take: it lets both enums exist, and it
/// reports a divergence only when someone runs the tests. The vocabulary belongs
/// to neither side, so it lives in [`iaam_core::goal`], which is also where the
/// argument for that placement is written down. Every path that named
/// `iaam_app::actions::ReportGoal` still resolves.
pub use iaam_core::goal::ReportGoal;

/// The queue's goal type **is** the report's, asserted where a comment would
/// otherwise have to be believed.
///
/// A coercion and not a test: it is checked by every build rather than by
/// `cargo test`, and re-declaring a local `ReportGoal` here — the exact defect
/// this wave removed — stops the crate from compiling instead of producing two
/// vocabularies that agree until they do not.
const _: fn(iaam_core::goal::ReportGoal) -> ReportGoal = std::convert::identity;

/// A goal's place in a [`ReportGoals`] bit pattern.
///
/// A free function because the goal is a foreign type now, and it stays an
/// exhaustive `match` for the reason [`ReportGoals`] is four bits wide: a fifth
/// goal must not acquire a bit by accident, and here it acquires a compile
/// error instead.
const fn bit(goal: ReportGoal) -> u8 {
    match goal {
        ReportGoal::AssetSnapshot => 1,
        ReportGoal::MoneyFlow => 1 << 1,
        ReportGoal::Returns => 1 << 2,
        ReportGoal::Reconciliation => 1 << 3,
    }
}

/// A set of goals, held as a bit pattern so that it stays [`Copy`].
///
/// A bitmask rather than a `BTreeSet<ReportGoal>`, and the reason is
/// [`ActionCategory`]. That type is `Copy`, it is returned **by value** from a
/// `const fn` accessor on [`Action`], and every consumer switches on it by
/// value. A heap set inside it would have taken `Copy` away from the category,
/// turned [`Action::category`] into a borrow, and rippled through the server's
/// mapping and every test that compares one — all to carry a set that can never
/// hold more than four elements. Four bits carry it instead, and nothing else
/// changes.
///
/// The empty set is representable, and that is deliberate: a `const fn` cannot
/// build a type whose non-emptiness is enforced by a constructor that fails, so
/// the invariant is enforced where the item is assembled. [`Action::new`]
/// refuses a [`ActionCategory::RequiredForGoal`] that names nothing, which is
/// exactly the defect this type exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ReportGoals(u8);

impl ReportGoals {
    /// No goal at all. Never admissible on [`ActionCategory::RequiredForGoal`].
    pub const NONE: Self = Self(0);

    /// Every goal there is: the item stands in the way of all four reports.
    pub const ALL: Self = Self::of(&ReportGoal::ALL);

    /// The set holding exactly the listed goals.
    #[must_use]
    pub const fn of(goals: &[ReportGoal]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < goals.len() {
            bits |= bit(goals[index]);
            index += 1;
        }
        Self(bits)
    }

    #[must_use]
    pub const fn contains(self, goal: ReportGoal) -> bool {
        self.0 & bit(goal) != 0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The goals in this set, in [`ReportGoal::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = ReportGoal> {
        ReportGoal::ALL
            .into_iter()
            .filter(move |goal| self.contains(*goal))
    }
}

/// The policy category assigned to an action.
///
/// `Copy` still, and the goal set is why that was in doubt: see [`ReportGoals`].
/// `PartialOrd`/`Ord` are **not** derived any more. A derived order over a
/// variant carrying a payload would have ordered two required items by their
/// bit patterns, which is a meaningless order that [`sort_actions`] would have
/// silently adopted for the queue. Urgency is [`Self::rank`], written out, and
/// it is the only order this type has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCategory {
    /// Work that prevents the system from accepting another action.
    Blocking,
    /// Work required for the named goals, and for no others.
    ///
    /// The set is never empty. Before it existed this variant named no goal at
    /// all, and a reader walking the queue could not tell an item that stops one
    /// report from an item that stops every report there is — so the whole queue
    /// read as a precondition on the entire import, which is not what any of it
    /// does.
    RequiredForGoal(ReportGoals),
    /// Work that improves quality but is not required.
    Recommended,
    /// A fact that requires no action.
    Informational,
}

impl ActionCategory {
    /// The required-for-goal category of a kind, read from the one table.
    ///
    /// Every producer goes through this rather than writing a set beside the
    /// kind it is building: two statements of «what this kind blocks» would
    /// eventually disagree, and the queue is the place where a disagreement is
    /// invisible.
    #[must_use]
    pub const fn required_for(kind: ActionKind) -> Self {
        Self::RequiredForGoal(kind.goals())
    }

    /// Urgency, most urgent first. The queue's order, and nothing else.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Blocking => 0,
            Self::RequiredForGoal(_) => 1,
            Self::Recommended => 2,
            Self::Informational => 3,
        }
    }

    /// The goals this category names, which is none unless it is required work.
    #[must_use]
    pub const fn goals(self) -> ReportGoals {
        match self {
            Self::RequiredForGoal(goals) => goals,
            Self::Blocking | Self::Recommended | Self::Informational => ReportGoals::NONE,
        }
    }
}

/// Whether an action can be invoked without asking the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    Ready,
    NeedsOwnerInput,
    /// No operation in this API is available for this item.
    Blocked,
}

/// The typed thing an action is about.
///
/// Published beside the prose rather than only inside it. An action's `id` is
/// opaque by design and its `reason` is a sentence; a caller answering a
/// question about one account — a report scoping its diagnostics, an agent
/// deciding which item its next call would resolve — could previously narrow
/// the queue by neither, and had to be handed a separately scoped list instead.
///
/// Not every action has one: «no account exists» and «no contour exists» are
/// existential and name nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionSubject {
    Account(AccountSubject),
    Event(EventId),
}

impl ActionSubject {
    /// The account this item is about, when it is about an account.
    ///
    /// A reader that only wants to group the queue by account should not have to
    /// destructure a struct it will then ignore.
    #[must_use]
    pub const fn account(&self) -> Option<AccountId> {
        match self {
            Self::Account(account) => Some(account.id),
            Self::Event(_) => None,
        }
    }
}

/// The account an item is about, and the owner's own name for it.
///
/// **The name travels with the identifier**, for the reason
/// [`crate::ports::AccountView`] is read here at all and the reason
/// `PopulationAccount` in the core carries a title: the queue exists to be read
/// by the owner, and an owner asked to record a balance cannot act on a bare
/// UUID. One `record_owner_balance` item per account, each naming an identifier
/// and nothing else, is a list he can only act on after a second request and a
/// join he was left to perform.
///
/// **Paired here rather than at the transport**, and that is the decision this
/// type exists to hold. An identifier and a name joined on the way out are
/// joined from a second read of the store, so the sentence in `reason` — which
/// already interpolates the title where the emitter holds one — and the name
/// beside the identifier could come from two snapshots and disagree inside one
/// response. Pairing them where the item is built makes that impossible: one
/// item, one reading of what the owner calls the account.
///
/// Not [`AccountCandidate`], whose fields are the same three. That one is an
/// account *offered as an answer* to a question the item asks; this one is the
/// account the item is *about*. They are equal today by coincidence of the
/// facts an account has a name by, and merging them would tie a change in what
/// may be offered to a change in what may be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSubject {
    pub id: AccountId,
    pub title: String,
    /// The institution the owner said holds it, when he said. `None` is «he has
    /// not said», never a guess: two accounts at one bank are told apart by the
    /// title, and an invented institution would tell them apart by a fiction.
    pub institution: Option<String>,
}

impl AccountSubject {
    /// The subject of an item about this account.
    #[must_use]
    pub fn of(account: &AccountView) -> Self {
        Self {
            id: account.id,
            title: account.title.clone(),
            institution: account.institution.clone(),
        }
    }
}

/// The owner's accounts, indexed for the one question the queue asks of them.
///
/// Built from the accounts the caller already holds rather than from a store
/// read of its own: the diagnostics are pure functions over a ledger or a
/// report, and giving them a store would let a later rule read something else
/// out of it.
pub struct AccountNames<'a> {
    by_id: BTreeMap<AccountId, &'a AccountView>,
}

impl<'a> AccountNames<'a> {
    #[must_use]
    pub fn new(accounts: &'a [AccountView]) -> Self {
        Self {
            by_id: accounts
                .iter()
                .map(|account| (account.id, account))
                .collect(),
        }
    }

    /// The account under this identifier.
    ///
    /// An error rather than an unnamed item. Every account a ledger, a flow or
    /// an activity row names was created through this API and is one of the
    /// owner's accounts — nothing deletes one — so a miss is the store
    /// contradicting itself, and the queue refuses to answer over it for the
    /// same reason [`frontier`] refuses over an unreadable stored question. The
    /// alternative, publishing the item with no name or with a placeholder for
    /// one, would hand the owner exactly the bare identifier this type exists to
    /// abolish, at the one moment something is already wrong.
    fn get(&self, account: AccountId) -> Result<&'a AccountView, AppError> {
        self.by_id.get(&account).copied().ok_or_else(|| {
            AppError::Store(format!(
                "action queue names account {} which is not one of the owner's accounts",
                account.inner()
            ))
        })
    }
}

/// A source from which the value of a missing request field must come.
///
/// **Three words, and there is deliberately no fourth for a converter**
/// (`iaam-tt71`). The case for one was real while it stood: the queue's
/// `start_account_import` item tells a caller to open a session and *feed it the
/// rows*, and between "the owner obtains the statement" and that clause sat a
/// conversion the item attributed to nobody. `docs/import-boundary.md` §8
/// concluded that the item's honest gain was a word for the converter here.
///
/// That conclusion was conditional on a defect, and the defect is gone. It held
/// because the observation channel could not express two of the outcomes the
/// conclusive one could, so a converter that concluded first was the only thing
/// that could produce a complete import — `iaam-7l7v` and decision 0006 closed
/// that. A caller holding rows the owner pasted now transcribes them as
/// observations and the server reaches the conclusions, so the conversion the
/// item presupposed is no longer a step anybody has to take.
///
/// Writing the word anyway would record a workaround as the design at the moment
/// it stopped being needed. It would also make this enum answer two questions:
/// each word here names **who supplies a value**, and a converter is a step
/// rather than a source. The half that was genuinely unattributed — the rows —
/// was not a missing field of the request it was argued about: it is the body of
/// `POST /v1/import-sessions/{session}/rows`, a later call, and a pointer into
/// it could not be satisfied by filling in the request it was published on. What
/// the item gained instead was a sentence naming the shape a row is submitted
/// in, which is a fact about this API and therefore something the queue may
/// state.
///
/// The later call now has a resolution of its own (`iaam-ripl`), so the rows
/// **are** a missing field — of that call, marked `ExternalDocument`, on the
/// option `start_account_import` publishes beside the one that opens the
/// session. That does not reopen the case for a fourth word; it removes the
/// last thing the word was standing in for. A field the queue can point at is
/// what the sentence was a substitute for.
///
/// **The axis is where the value comes from, not what it cost to get.** That is
/// the sentence `iaam-k6l7` went looking for a fourth word for, and it is the
/// answer to the same question: a figure the owner exported, converted and
/// restated in another unit is still `ExternalDocument`, because the document
/// is what holds it and the conversion is how he came to read it. A word for
/// the derivation would make one field answer two questions — who has the
/// value, and what must be done before it can be typed — and a caller reading
/// such a field could no longer tell from it whom to ask. The distinction was
/// unwritten, which is why it kept being rediscovered as a gap; it is written
/// here and, more to the point, in the meanings below, because the caller that
/// needs it reads the contract and not this file.
///
/// Published through `provided_by_vocabulary!` below, for the reason the module doc
/// of `iaam_server::vocabulary` gives: these three codes reached the wire as a
/// bare `string` with no enumeration and no sentence, which is exactly the
/// shape that made the fourth word look necessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvidedBy {
    Owner,
    ExternalDocument,
    Caller,
}

/// The `provided_by` vocabulary: every variant, its wire code, and what the
/// code means.
///
/// Built like `iaam_ingest::verdict_vocabulary!` and for the same reason: the
/// three codes used to be written out once here and again as string literals in
/// the transport, and the contract published neither the list nor a word of
/// explanation. Pass the name of a macro that accepts
/// `Variant => "code": "meaning",` arms and it will be called with the whole
/// list.
#[macro_export]
macro_rules! provided_by_vocabulary {
    ($receiver:path) => {
        $receiver! {
            Owner => "owner":
                "The owner decides or states this himself. It is a choice, a title or a figure that exists nowhere else, and no document and no client can supply it on his behalf.",
            ExternalDocument => "external_document":
                "The value is printed on something outside this system — a statement, an export, a contract — and is read off it. It stays this word however much work reading it took: fetching the file, converting it and restating a figure in another unit are steps on the way to the value, not sources of it. This field names who holds the value, not what must be done before it can be typed.",
            Caller => "caller":
                "The client fills this in from what it already knows about the transmission — how the rows arrived, which of its own identifiers this is — without putting a question to the owner.",
        }
    };
}

macro_rules! define_provided_by_code {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        impl ProvidedBy {
            /// Machine-readable code for the API (§13).
            #[must_use]
            pub const fn code(self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $code,)+
                }
            }
        }
    };
}

provided_by_vocabulary!(define_provided_by_code);

/// An account the owner can choose for contour membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCandidate {
    pub id: AccountId,
    pub title: String,
    pub institution: Option<String>,
}

/// The question this system puts to the owner about one field it cannot fill.
///
/// **[`ProvidedBy::Owner`] says who supplies a value and never says what to ask
/// him** (`iaam-ytvf`). An agent relaying a queue item to a person held one
/// string per field — the JSON pointer — so it showed him the pointer, and
/// beside it the schema descriptions, which are written for whoever implements a
/// client. The agent was not careless; those were the only strings this surface
/// gave it.
///
/// **A closed vocabulary whose variants are fields of calls, not pointers.**
/// `/title` is an account's title on one route and a perimeter's on another, so
/// a table keyed by the pointer would answer one of them with the other's
/// sentence. Each variant therefore names its call ([`Self::asked_by`]) and
/// derives its pointer ([`Self::pointer`]) instead of being written beside one,
/// and two items asking for the same field of the same call ask it in the same
/// words. That is the arrangement
/// [`iaam_ingest::classification::AnswerShape::consequence`] already has, and it
/// is here for the same reason: two publishers of one question will eventually
/// disagree about it.
///
/// **An item specialises by handing over what it knows, never by writing a
/// second sentence.** [`Self::AccountTitle`] carries the string a document
/// printed, because the item that document raised is asking about *that*
/// account and a bare «what do you call this account?» would leave the owner to
/// work out which one. Both shapes of the question still live here, so the
/// default and the specialisation cannot drift apart — an item supplies the
/// datum and this type supplies the words.
///
/// **Not the item's `reason`.** `iaam-tt71` found that a mapping from field to
/// question, gathered into one prose sentence, has to be taken apart again by
/// the caller that must show the owner **one** field. That is
/// `docs/api/conventions.md` §5's objection to a structure sent as a string, and
/// it is written a second time on [`InputAlternative::consequence`], which
/// exists because a consequence per alternative gathered into one sentence is
/// the same mistake.
///
/// **The register is the owner's own rule**, and he wrote it: every question
/// put to him is asked so that he understands it, with none of our internal
/// words, saying what it is for and what his decision changes. The third
/// obligation is why [`OwnerQuestion`] has two parts rather than one string —
/// the half that says what turns on the answer is the half that gets folded
/// away into a sentence that already reads as finished.
///
/// `a_question_for_a_person_is_not_a_field_name` states the mechanical part of
/// that register and is proved against the strings the field report actually
/// carried. The rest of it — that a person who has never read this codebase can
/// answer the question — is not a property a test can hold, and decision 0027
/// says so rather than pretending otherwise.
///
/// Decision 0027.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerPrompt {
    /// His own name for an account, and the string a document printed for it
    /// where one did.
    ///
    /// The one variant that carries a datum, and the asymmetry decision 0004
    /// argues for is why. `provider_account_id` is preset from the printed
    /// string and the title deliberately is not — a title can be renamed and
    /// would silently stop a statement importing — and that reasoning is
    /// invisible to a caller, which is how an agent came to show the owner a
    /// filled-in field he has no business seeing. Said in the question, it stops
    /// being invisible without the preset having to grow a shape of its own.
    AccountTitle { printed: Option<String> },
    /// His name for a reporting perimeter.
    ContourTitle,
    /// Which accounts a new perimeter holds.
    ContourAccounts,
    /// Which perimeter an account joins, where he has more than one.
    MembershipContour,
    /// The composition a perimeter has once the account joins it.
    MembershipAccounts,
    /// Why an account is deliberately outside every perimeter.
    ExclusionReason,
    /// Which of his other accounts money moves directly between this one and.
    TransferPartners,
    /// Which broker holds an account.
    BrokerChannel,
    /// The first date a broker is asked about.
    SyncFrom,
    /// The last date a broker is asked about.
    SyncTo,
    /// Which recorded facts should stop counting, and what stands instead.
    Corrections,
    /// That he accepts what the correction retracts.
    AcknowledgeRetraction,
    /// The cash figure he is asserting against the journal.
    OwnerBalanceCash,
    /// What an account held before this system knew anything about it.
    OpeningAmount,
    /// The currency that opening figure is in.
    OpeningCurrency,
    /// The date that opening figure speaks about.
    OpeningDate,
    /// What the rows a rule is for have in common.
    RuleMatcher,
    /// What those rows were for.
    RuleCategory,
    /// What one held row of an import was.
    ImportAnswer,
    /// Which of his accounts is the far side of a movement.
    TransferFarSide,
    /// The bank, broker or organisation an account is held at.
    ///
    /// **The question that replaced one nobody could answer** (`iaam-9i83`).
    /// `create_account` carries `provider` beside this, and `provider`'s only
    /// property is that it differs between sources: a person cannot get such a
    /// value right in an interesting way, gains nothing by choosing it, and must
    /// then remember it forever. Told as much, the owner said what should have
    /// been asked instead — «then it should have asked me what the bank is
    /// called» — and then narrowed it himself, because a broker is not a bank
    /// and an account may sit at neither.
    ///
    /// It carries no datum, and the contrast with [`Self::AccountTitle`] is the
    /// reason: that one carries the printed string because what a rename
    /// **costs** differs between the two states it can be asked in. What an
    /// institution is for does not differ, and neither does what turns on it, so
    /// there is nothing for a datum to vary.
    AccountInstitution,
    /// What a name a document printed is, where it is not an account of his.
    ///
    /// The second way out of the item this vocabulary's neighbour raises
    /// (`iaam-mk1n`), and the reason it needs a question of its own is that it
    /// is not the same act as ruling an account outside the perimeter — there is
    /// no account. `/reason` is the pointer both share, which is exactly why a
    /// question is keyed on the pair: the two sentences are about different
    /// things and would otherwise be one entry.
    DeclinedNameReason,
}

impl OwnerPrompt {
    /// The field this question is about, as a JSON pointer into the request.
    ///
    /// Derived rather than written beside the question, so that an item cannot
    /// publish one field's pointer with another field's words. It is also what
    /// makes reusing the wrong variant visible: the pointer changes with it, and
    /// the route rejects a request built from it.
    #[must_use]
    pub const fn pointer(&self) -> &'static str {
        match self {
            Self::AccountTitle { .. } | Self::ContourTitle => "/title",
            Self::ContourAccounts | Self::MembershipAccounts => "/accounts",
            Self::MembershipContour => "/contour",
            Self::ExclusionReason => "/reason",
            Self::TransferPartners => "/partners",
            Self::BrokerChannel => "/broker",
            Self::SyncFrom => "/from",
            Self::SyncTo => "/to",
            Self::Corrections => "/corrections",
            Self::AcknowledgeRetraction => "/acknowledge_retraction",
            Self::OwnerBalanceCash => "/cash",
            Self::OpeningAmount => "/operations/0/amount",
            Self::OpeningCurrency => "/operations/0/currency",
            Self::OpeningDate => "/operations/0/dates/cash_posted",
            Self::RuleMatcher => "/matcher",
            Self::RuleCategory => "/category",
            Self::ImportAnswer => "/answer",
            Self::TransferFarSide => "/account",
            Self::AccountInstitution => "/institution",
            // The same pointer as `ExclusionReason` and a different question,
            // which is what `asked_by` below is for.
            Self::DeclinedNameReason => "/reason",
        }
    }

    /// The call this field belongs to.
    ///
    /// A pointer is not an identity — `/title` is two different questions — so
    /// the key is the pair, and this is the half a pointer cannot carry. The
    /// guard reads it back: a question published on a resolution that calls
    /// something else is a question about a field that request does not have.
    #[must_use]
    pub const fn asked_by(&self) -> OperationKey {
        match self {
            Self::AccountTitle { .. } => OperationKey::CreateAccount,
            Self::ContourTitle | Self::ContourAccounts => OperationKey::CreateContour,
            Self::MembershipContour | Self::MembershipAccounts => OperationKey::AddContourVersion,
            Self::ExclusionReason => OperationKey::RecordAccountScope,
            Self::TransferPartners => OperationKey::RecordAccountTransferPartners,
            Self::BrokerChannel | Self::SyncFrom | Self::SyncTo => OperationKey::SyncBroker,
            Self::Corrections | Self::AcknowledgeRetraction => OperationKey::SubmitCorrections,
            Self::OwnerBalanceCash => OperationKey::RecordOwnerBalance,
            Self::OpeningAmount | Self::OpeningCurrency | Self::OpeningDate => {
                OperationKey::SubmitOperations
            }
            Self::RuleMatcher | Self::RuleCategory => OperationKey::CreateCategoryRule,
            Self::ImportAnswer | Self::TransferFarSide => OperationKey::AnswerImportQuestion,
            Self::AccountInstitution => OperationKey::CreateAccount,
            Self::DeclinedNameReason => OperationKey::RecordAccountNameDisposition,
        }
    }

    /// The words put to the owner: what is being asked, and what turns on it.
    ///
    /// **Two strings and not one**, and the owner wrote the rule they answer to:
    /// every question put to him is to be asked so that he understands it,
    /// without our internal words, saying what it is for and what his decision
    /// changes. The third obligation is the one that gets dropped — an item that
    /// kept the first two told him a title was his own and that he could change
    /// it whenever he liked, and he asked what the question even was and what it
    /// affected — and it is dropped because it is easy to drop when it is a
    /// clause at the end of a sentence that already reads as finished.
    ///
    /// It is also the half that legitimately varies. What a name is *for* is the
    /// same wherever it is asked; what turns on it is not, and
    /// [`Self::AccountTitle`] is the case: an account created from a document
    /// already carries the string that document printed, so a later rename moves
    /// nothing but the label he reads, while an account carrying no printed
    /// string is found by its name and a rename can stop a statement lining up
    /// with it. Splitting the two lets an item vary the second without three
    /// items inventing three answers to the first.
    ///
    /// Beside the field for the reason [`InputAlternative::consequence`] sits
    /// beside the value it belongs to, and it is the same word for the same
    /// idea: what changes if you answer this way rather than that.
    #[must_use]
    pub fn question(&self) -> OwnerQuestion {
        let (ask, consequence): (String, String) = match self {
            Self::AccountTitle { printed: None } => (
                "What do you want to call this account? Use the name you would use for it \
                 yourself — it is the name you will see on every report and in every list this \
                 system shows you."
                    .to_owned(),
                "Nothing else depends on it, with one exception. When a statement arrives for \
                 an account this system holds no account number for, the name printed on the \
                 line is what it is matched against — so while this account has no number \
                 recorded, renaming it can stop a statement lining up with it, and the way out \
                 of that is to record the number rather than to name the account back."
                    .to_owned(),
            ),
            Self::AccountTitle {
                printed: Some(printed),
            } => (
                format!(
                    "What do you want to call the account that a document of yours calls \
                     «{printed}»? Use the name you would use for it yourself — it is the name \
                     you will see on every report and in every list this system shows you."
                ),
                format!(
                    "«{printed}» is kept with the account too, so lines printed that way find \
                     it whatever you call it. Your answer therefore changes only what you read: \
                     renaming the account later moves the labels on your reports, moves no \
                     figure, and breaks no import."
                ),
            ),
            Self::ContourTitle => (
                "What do you want to call this group of accounts — the group whose money your \
                 reports are about?"
                    .to_owned(),
                "The name is only what you read on the reports; no figure depends on it. Which \
                 accounts the group holds is the next question, and that is the one that \
                 decides the figures."
                    .to_owned(),
            ),
            Self::ContourAccounts => (
                "Which of your accounts belong in that group?".to_owned(),
                "Only the accounts you name are counted in your reports. Money moving between \
                 two of them counts as your own money moving and as neither spending nor \
                 income; money going anywhere else counts as having left you. An account you \
                 leave out is in none of the figures — the reports say which accounts they left \
                 out, but they do not add it in."
                    .to_owned(),
            ),
            Self::MembershipContour => (
                "Which of your groups of accounts should this one join? You keep more than one, \
                 so nothing here can choose for you."
                    .to_owned(),
                "This account's money starts counting in the reports for the group you name, \
                 and in no other. If you keep separate groups for separate purposes, this is \
                 what decides which set of figures its spending and its income turn up in."
                    .to_owned(),
            ),
            Self::MembershipAccounts => (
                "Which accounts should that group hold once this one has joined it?".to_owned(),
                "The answer replaces the group's membership outright, so it has to be the whole \
                 list and not only the account being added: any account you leave off is left \
                 out of the group, and out of the figures its reports show, from now on."
                    .to_owned(),
            ),
            Self::ExclusionReason => (
                "Why does this account not belong in any of your groups?".to_owned(),
                "No figure moves either way — the account is already outside them. What the \
                 sentence buys you is a year from now: it is the only thing that tells an \
                 account you left out on purpose from one nobody ever got round to, and this \
                 system stops asking about this one once you have said it."
                    .to_owned(),
            ),
            Self::TransferPartners => (
                "Which of your other accounts does money move directly between this one and? \
                 Name all of them, or say that none of them does."
                    .to_owned(),
                "One movement between two banks is printed twice, once by each of them, and \
                 nothing in the two lines says they are the same movement. Naming the accounts \
                 lets the two be paired, and the money then counts as having moved between your \
                 own accounts. Until then each line is counted separately, as money leaving you \
                 and as money arriving from outside — so both your spending and your income \
                 read larger than they were."
                    .to_owned(),
            ),
            Self::BrokerChannel => (
                "Which broker holds this account?".to_owned(),
                "Naming it lets this system ask that broker directly for what the account holds \
                 and what it did, instead of you fetching a file and handing it over. Nothing \
                 here knows which one it is, so until you say, nothing can be fetched and this \
                 account stays empty."
                    .to_owned(),
            ),
            Self::SyncFrom => (
                "From which date should the broker be asked about this account?".to_owned(),
                "Everything the broker reports from that date onward is taken in, and anything \
                 before it is not. A date later than the account's real beginning leaves the \
                 earlier part missing, and every figure is then about the part that was fetched \
                 rather than about the account. Nothing here can propose a date: this account \
                 has no history recorded yet, so there is nothing to read one off."
                    .to_owned(),
            ),
            Self::SyncTo => (
                "Up to which date should the broker be asked about this account?".to_owned(),
                "Anything after that date is not fetched, so it is in none of your figures \
                 until you ask again over a later stretch. Asking twice over the same stretch \
                 is safe: what was already taken in is not taken in twice."
                    .to_owned(),
            ),
            Self::Corrections => (
                "Which of the things already recorded should stop counting, and what should \
                 stand in their place?"
                    .to_owned(),
                "Whatever you name stops being counted and what you put in its place counts \
                 instead, so every report that included it changes. Nothing is erased: the \
                 original stays recorded and marked as no longer counting, which is what lets \
                 the change be seen afterwards and undone."
                    .to_owned(),
            ),
            Self::AcknowledgeRetraction => (
                "Do you accept that this withdraws things that were already recorded?".to_owned(),
                "Saying yes lets the change through, and figures you have already read will \
                 move. Saying no leaves everything as it stands and records nothing. It is \
                 asked apart from the correction itself because a change that quietly withdrew \
                 what was recorded would move figures you had acted on without telling you."
                    .to_owned(),
            ),
            Self::OwnerBalanceCash => (
                "How much money did this account hold on that date?".to_owned(),
                "The figure is compared with what this system worked out from everything it has \
                 recorded for the account. If the two agree, that stretch is confirmed and you \
                 stop being asked about it. If they disagree, the difference is kept and shown \
                 to you rather than smoothed away — your figure does not overwrite the worked-out \
                 one, and neither of the two is assumed to be the right one."
                    .to_owned(),
            ),
            Self::OpeningAmount => (
                "How much did this account hold before this system knew anything about it?"
                    .to_owned(),
                "Everything recorded since is movement, and movement alone says only how much \
                 the amount changed, never what it is. Give this and the account's balances \
                 come out right from that point on; leave it and every one of them is wrong by \
                 exactly what was there at the start."
                    .to_owned(),
            ),
            Self::OpeningCurrency => (
                "Which currency is that starting figure in?".to_owned(),
                "It is held and counted in that currency and converted only when a report is \
                 drawn up in another one. Naming a currency the account does not hold makes the \
                 starting figure a different sum of money from the one you meant."
                    .to_owned(),
            ),
            Self::OpeningDate => (
                "From which date is that starting figure true?".to_owned(),
                "It has to fall before the earliest movement recorded for this account: put it \
                 later and there is a stretch of movements with nothing behind them, and the \
                 figure explains none of them. Whatever happened before that date is not \
                 counted separately at all — the starting figure stands in for all of it."
                    .to_owned(),
            ),
            Self::RuleMatcher => (
                "What do those payments have in common that could be recognised automatically?"
                    .to_owned(),
                "What you give is applied to every payment that matches it — the ones already \
                 recorded and the ones still to come — so you are not asked about them one at a \
                 time. Something too broad catches payments you did not mean and files them \
                 wrongly; something too narrow leaves them where they are, unexplained."
                    .to_owned(),
            ),
            Self::RuleCategory => (
                "What were those payments for?".to_owned(),
                "Your answer is the heading they are counted under in the report of where your \
                 money went. Until they have one they appear there only as money that left with \
                 nothing to explain it, which is why they were raised with you at all."
                    .to_owned(),
            ),
            Self::ImportAnswer => (
                "What was this line on the statement?".to_owned(),
                "Each answer offered says in its own words what counting the line that way does \
                 to your report of money in and money out. The line is in none of your figures \
                 until you choose, and choosing is what lets the rest of the statement finish."
                    .to_owned(),
            ),
            Self::TransferFarSide => (
                "Which of your accounts is the other side of this movement?".to_owned(),
                "Both accounts get their side of it, and the money counts as having moved \
                 between accounts of yours rather than as spending or as income. It has to be \
                 an account of yours: money that went to somebody else is one of the other \
                 answers, and answering this way about somebody else's account would hide real \
                 spending as an internal move."
                    .to_owned(),
            ),
            Self::AccountInstitution => (
                "Which bank, broker or other organisation is this account held at? Write it \
                 the way you would say it out loud."
                    .to_owned(),
                "No figure moves whichever way you answer, and nothing is worked out from it: \
                 this system does not read it, and no statement is matched against it. It is \
                 read by you. It is what tells two accounts apart in a list when they are \
                 called something similar, and what says, a year from now, where the account a \
                 report is about is actually kept. Saying nothing is also an answer, and this \
                 is one of the few places where it costs nothing today: the account is created \
                 either way and every figure about it is the same. What it costs is later, and \
                 it is exactly that — a year from now, nothing will say where this account is."
                    .to_owned(),
            ),
            Self::DeclinedNameReason => (
                "Why is this name not one of your accounts? A few words are enough — what it \
                 actually is."
                    .to_owned(),
                "The records printed under this name stay out of everything, exactly as they \
                 are now: they are already refused, and saying this does not recover them. \
                 What changes is that they are refused because you decided so rather than \
                 because nobody has got round to them, and this system stops asking you to \
                 create the account. Your sentence is the only thing that will \
                 tell, a year from now, a name you ruled out from one nobody ever looked at. \
                 If one of your accounts turns out to answer to the name after all, saying so \
                 wins: this is not consulted once the name is recognised."
                    .to_owned(),
            ),
        };
        OwnerQuestion { ask, consequence }
    }
}

/// One question put to the owner about one field.
///
/// Two parts, because the owner's rule has three obligations and the third is
/// the one that gets dropped: a question is to be asked in words he
/// understands, saying what it is for and what his answer changes. The first two
/// live in [`Self::ask`] and the third in [`Self::consequence`], and they are
/// separate values so that the third cannot be lost by being folded into a
/// sentence that already reads as finished — which is how an item came to tell
/// him a name was his to change and leave him asking what the question was for
/// (`iaam-ytvf`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerQuestion {
    /// What he is asked, and why he is being asked it.
    pub ask: String,
    /// What is different depending on how he answers.
    ///
    /// Never «this is yours to decide» and never «you can change it later»:
    /// both are true of almost every field here and neither tells him anything
    /// about his choice. Where the honest answer is that nothing turns on it,
    /// it says so **and** names the one case where something would.
    pub consequence: String,
}

/// A value this instance works out for one field, put to the owner as one
/// question over every item it would fill.
///
/// **The unit of his decision is not the unit of the item** (`iaam-hdr7`). The
/// queue publishes one item per name a document printed, and that is right:
/// completion is per name, and an account created for one string does not settle
/// another. What was missing is a way to say «this answer, for these items».
/// Reading seven printed names, an owner was put seven times through two
/// questions apiece and interrupted the eleventh to answer all of them at once,
/// twice: they are all from one institution, and call them what the statement
/// calls them. Both answers were derivable from what the queue already held —
/// the institution from the profile that read the document, the names from the
/// strings that document printed — and neither was offered.
///
/// **A proposal is a question and not a guess.** It is published *as* the
/// question, its value is read out, and nothing is recorded until he answers, so
/// the agent skill's rule — a missing value is asked of the owner, not filled in
/// — is kept rather than bent. That is [`matcher_for`]'s arrangement one surface
/// over: a rule is proposed from a row and adopted by him, and until he adopts
/// it there is no rule.
///
/// [`matcher_for`]: iaam_ingest::classification::matcher_for
///
/// **And a proposal is not a preset under another name.** A preset value is the
/// request already filled in and is never read out to him (decision 0027 §4);
/// publishing a proposal as a preset would hide the very question it exists to
/// ask, which is the defect `iaam-9i83` closed on the field beside this one.
/// Decision 0030 refused presetting the institution from the issuer for exactly
/// that reason, and named the door it was leaving open: a value read out to him
/// is not the value hidden from him.
///
/// **Refused whole, and the set is what it is refused over.** [`Self::covers`]
/// names every item the one answer fills, and an item that cannot take it is in
/// no set at all rather than quietly left out of one — which is `iaam-q5og`'s
/// rule at the surface a publication has: a wider answer either reaches
/// everything it names or is not offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// What is proposed for this item, and the ground it stands on.
    pub proposed: ProposedAnswer,
    /// Every item this one answer fills this field of, this one included.
    ///
    /// **Never a set of one.** One item is one question, and «here is a set of
    /// one» would make a caller take a set apart to find the fact it already
    /// had — the argument [`ActionTarget::from_options`] makes about a list
    /// holding one resolution, and it holds here for the same reason.
    ///
    /// It names the items and not their count, because the caller has to be
    /// able to say to him *which* accounts he is answering for, and because an
    /// answer applied beyond this list is visibly outside the offer rather than
    /// plausibly inside it.
    pub covers: Vec<String>,
}

impl Proposal {
    /// The value proposed for **this** item's field.
    ///
    /// Per item, because one decision does not mean one value: «call them what
    /// the statement calls them» is a single answer that writes a different
    /// string into each request, and «they are all held at that institution» is
    /// a single answer that writes the same one. A shape admitting only the
    /// second would have covered the institution and left the names asked one
    /// at a time, which is eight of the fifteen exchanges.
    #[must_use]
    pub fn value(&self) -> &str {
        self.proposed.value()
    }

    /// The one sentence put to him about the whole set.
    ///
    /// Rendered here and not written per item, which is decision 0027 §2's
    /// arrangement: two publishers of one question eventually disagree about
    /// it. The count is the set's own size rather than a datum an item carries,
    /// so a question that named four accounts while covering seven is not a
    /// state this type can be in.
    #[must_use]
    pub fn question(&self) -> OwnerQuestion {
        self.proposed.question(self.covers.len())
    }
}

/// What may be proposed for a field, and the ground each proposal stands on.
///
/// A closed vocabulary keyed by the field of the call, exactly as [`OwnerPrompt`]
/// is and for the same reason. It is a **second** vocabulary and not two more
/// variants of that one, because the sentences answer different questions: that
/// one asks a person what a title is and what a rename costs, and this one asks
/// him to confirm a value over a set. Folded together, the sweep that compares
/// the questions this system asks with the fields the queue publishes would have
/// to admit questions no field publishes, which is the half of decision 0027 §6
/// that makes the sweep worth running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposedAnswer {
    /// Every account these names were read under is held at the institution the
    /// reading says printed them.
    ///
    /// **A join already recorded and not an inference.** The profile that read
    /// the document declares the institution it is from, and that declaration is
    /// what `provider` is minted from one field over (decision 0030 §1). What is
    /// new here is only that it is read out to him: the institution is his word
    /// for where the account is, the issuer is the profile's word for who
    /// printed the document, and the two are the same thing often enough to be
    /// worth proposing and different often enough that he must be the one to
    /// say.
    AccountInstitutionOfIssuer { issuer: String },
    /// Every such account is called what the document prints for it.
    ///
    /// **Safe only because the identifier is preset**, and that is decision 0004
    /// standing rather than being reversed. Presetting the printed string as the
    /// *title* is refused there, because a title is renameable and a statement
    /// that found its account by the title would stop importing on the first
    /// rename. What the item presets is the identifier, so the printed string is
    /// matched at the tier a rename does not move, and a title that happens to
    /// equal it carries no weight at all. Where the reading could not say which
    /// source printed the string, nothing is preset — and this proposal is not
    /// offered either, because there the title would be the only thing a line
    /// could find the account by.
    AccountTitleAsPrinted { printed: String },
}

impl ProposedAnswer {
    /// The question this proposal is an answer to.
    ///
    /// **Its words are not read from here**, and the shape it is built in says
    /// so: [`OwnerPrompt::AccountTitle`] carries the string a document printed
    /// and this hands it none, because what is wanted is the field and the call
    /// and nothing else. The words for a set are this type's own, above; what
    /// [`OwnerPrompt`] supplies is the identity of the field being answered, so
    /// that a proposal cannot come to name a field or a call the question beside
    /// it does not.
    fn field(&self) -> OwnerPrompt {
        match self {
            Self::AccountInstitutionOfIssuer { .. } => OwnerPrompt::AccountInstitution,
            Self::AccountTitleAsPrinted { .. } => OwnerPrompt::AccountTitle { printed: None },
        }
    }

    /// The field this proposal fills, as a JSON pointer into the request.
    #[must_use]
    pub fn pointer(&self) -> &'static str {
        self.field().pointer()
    }

    /// The call that field belongs to.
    ///
    /// A pointer is not an identity — `/title` is two questions — so the pair is
    /// what a proposal is checked against, exactly as a question is.
    #[must_use]
    pub fn asked_by(&self) -> OperationKey {
        self.field().asked_by()
    }

    /// The value this proposal puts in this item's field.
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::AccountInstitutionOfIssuer { issuer } => issuer,
            Self::AccountTitleAsPrinted { printed } => printed,
        }
    }

    /// The words put to him about the whole set: what is proposed, and what
    /// turns on saying yes to it rather than answering one at a time.
    ///
    /// Decision 0027's two obligations are unchanged by the answer being wide.
    /// What the consequence must now also carry is the cost of the width itself
    /// — that one answer decides for every account named — because that is the
    /// difference between this and the question beside it, and it is the half
    /// he would otherwise discover afterwards.
    #[must_use]
    pub fn question(&self, covered: usize) -> OwnerQuestion {
        let (ask, consequence): (String, String) = match self {
            Self::AccountInstitutionOfIssuer { issuer } => (
                format!(
                    "Shall all {covered} of these accounts be recorded as held at «{issuer}»? \
                     Every one of them was named on a document this system read as coming from \
                     there, so one answer settles all {covered} — and if you would say the name \
                     differently, say it your way and that is what is recorded."
                ),
                format!(
                    "Nothing is worked out from the answer and no figure moves: it is a note you \
                     read, and the only thing that will say a year from now where each of these \
                     accounts is kept. What one answer buys is that you are not asked {covered} \
                     times; what it costs is that any of the {covered} you would have answered \
                     differently has to be corrected afterwards, which is the same act as \
                     answering it now."
                ),
            ),
            Self::AccountTitleAsPrinted { printed } => (
                format!(
                    "Shall each of these {covered} accounts be called exactly what your document \
                     prints for it — this one «{printed}»? These are the names you will see on \
                     every report and in every list this system shows you, and one answer names \
                     all {covered}."
                ),
                format!(
                    "The printed name is kept with each account as the identifier its source \
                     prints, so whatever you call them the lines your statements print go on \
                     finding the right account and no figure moves. What one answer buys is that \
                     you are not asked {covered} times; what it costs is that a name you would \
                     have written differently is one you have to rename afterwards, and renaming \
                     moves only what you read."
                ),
            ),
        };
        OwnerQuestion { ask, consequence }
    }
}

/// Fields the owner is asked for that carry no question, and the bead deciding
/// whether they should be asked at all.
///
/// **Empty, and it was not emptied by writing the missing sentence.** The one
/// entry this register ever held was `create_account`'s `provider`, and
/// `iaam-9i83` answered it the other way: the field stopped being the owner's.
/// Its only property is that it differs between sources, a person cannot get
/// such a value right in an interesting way, gains nothing by choosing it, and
/// `CreateAccountRequest::provider_account_id`'s own doc obliges whoever supplies
/// it to change it whenever the derivation changes — a rule no person can keep.
/// So the queue mints it from the institution the profile that read the document
/// declares, and asks him instead for [`OwnerPrompt::AccountInstitution`], which
/// is a question he can answer without being taught anything first.
///
/// **It stays, empty, on purpose.** The register is the third way
/// `every_field_the_owner_fills_in_carries_the_question_to_put_to_him` is
/// satisfied, and its worth is that the next author facing the same shape has
/// somewhere to put «this should not be asked at all» other than into a fluent
/// sentence that settles the question by making it painless to leave. Deleting
/// the register would leave writing prose as the only way past the guard, which
/// is exactly the pressure decision 0027 §5 refused.
pub const QUESTIONS_UNDER_REVIEW: &[(OperationKey, &str)] = &[];

/// One required request field not supplied by the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingInput {
    pub pointer: String,
    pub provided_by: ProvidedBy,
    /// The question to put to the owner, where the owner is who fills this in.
    ///
    /// **Beside `provided_by` and not inside it** (`iaam-ytvf`). [`ProvidedBy`]
    /// argues at length that each of its three words names *who supplies a
    /// value* and that a word answering a second question would stop a caller
    /// being able to read whom to ask off it; hanging the question inside
    /// `Owner` would do exactly that to the variant that matters most. So the
    /// pair sits where a reader already looks, and the pairing is kept by a
    /// guard rather than by the type.
    ///
    /// **`None` means one of two things, and they are not the same.** For a
    /// field a document or the caller supplies, nobody is asked, so there is
    /// nothing to ask. For a field marked [`ProvidedBy::Owner`] it means the
    /// field is in [`QUESTIONS_UNDER_REVIEW`] — a question whose existence is
    /// itself in question. Silence outside those two cases is the state this
    /// bead was filed on, and
    /// `every_field_the_owner_fills_in_carries_the_question_to_put_to_him`
    /// refuses it.
    pub prompt: Option<OwnerPrompt>,
    pub candidates: Option<Vec<AccountCandidate>>,
    /// The literal values this field admits, when it admits a closed set.
    ///
    /// Empty means the field is not a choice — a title, a balance, a date — and
    /// says nothing about what may be written there. It is **not** the same as
    /// `candidates`: a candidate is one of the owner's accounts offered for a
    /// field whose type is an account, while an alternative is a value of the
    /// field itself, and choosing one may require further fields that choosing
    /// another does not.
    pub alternatives: Vec<InputAlternative>,
    /// Whether the call this field belongs to is accepted without it.
    ///
    /// **The queue could not say a field was skippable, so every field stopped
    /// him** (`iaam-4fsw`). `create_account` takes `institution` as an optional
    /// field: the account is created without it and no figure anywhere reads it.
    /// The item published it beside the title with nothing to tell the two
    /// apart, and the owner was held up over a word he could have left out — by
    /// an agent that had just told him, correctly, that no figure depends on it,
    /// and asked anyway because the item gave it no way to offer skipping.
    ///
    /// **It is a fact about the call and not a grade of the question.** True
    /// means the route accepts the request with the field absent. It does not
    /// mean the field is unimportant and it is not the item saying it would
    /// rather not know: what skipping costs is in the question's `consequence`,
    /// where decision 0027's third obligation already puts it, and «nothing now,
    /// and in a year you will not know where this account is» is exactly the
    /// sentence that rule asks for.
    ///
    /// **False is not «the schema requires it».** A route may refuse a request
    /// for a field its schema marks optional: `/reason` is required for one
    /// disposition and refused for another, and `/cash` is refused on an
    /// assertion carrying neither cash nor positions. Neither is skippable and
    /// neither is marked here, which is why the guard over this checks one
    /// direction and cannot check the other. Decision 0033.
    pub optional: bool,
    /// One answer this instance would put to him for this field and every item
    /// like this one.
    ///
    /// `None` where nothing is proposed, which is every field but two and every
    /// item that stands alone. Present, it carries the value and the whole set
    /// of items that one answer fills — see [`Proposal`] for why it is a
    /// question and not a preset, and why the set is refused whole.
    pub proposal: Option<Proposal>,
}

/// One admissible value of a missing input, with what choosing it then needs.
///
/// The `requires` list is why this is not a bare `Vec<String>`. An answer to a
/// classification question is one word — `paid`, `fee` — except for the two that
/// name one of the owner's accounts, which cannot be written without also
/// naming it. Published as a flat list of words, those two would either look
/// complete when they are not, or oblige every field they need to be marked
/// required for every value, which would refuse `paid` for want of an account
/// no one asked it about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputAlternative {
    /// The value written at the parent's pointer.
    pub value: String,
    /// Fields that become required only if this alternative is chosen.
    pub requires: Vec<RequiredInput>,
    /// What choosing this value does, where the value decides something the
    /// caller cannot see from the word itself.
    ///
    /// `None` for a vocabulary whose words say what they mean — a channel, a
    /// disposition, a currency. It is filled for the answers to an import
    /// question (`iaam-pzm9`), where the seven words are `paid`, `received`,
    /// `fee`, `income` and so on, and the difference between two of them is
    /// which figure of the owner's money-flow report the row lands in. The
    /// sentence is [`iaam_ingest::classification::AnswerShape::consequence`],
    /// read from there rather than written here, so the queue and the session
    /// publish the same words.
    ///
    /// Beside the value rather than in the parent's `reason`, for the reason
    /// `requires` is beside the value: a consequence per alternative gathered
    /// into one sentence is a mapping encoded as prose, and the caller that has
    /// to show the owner one alternative would have to take it apart again.
    pub consequence: Option<String>,
}

/// A field one alternative requires. It does not carry alternatives of its own.
///
/// [`MissingInput`] without the `alternatives`, and the omission is the point
/// rather than an oversight. A required field that were itself a closed choice
/// would be a second question that cannot be phrased until the first is
/// answered, which is the shape `Question::UnresolvedDirection` exists to
/// refuse. Making the two types mutually recursive would also make the OpenAPI
/// schema generated from them non-terminating, and it did: the generator
/// overflowed its stack before this type existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredInput {
    pub pointer: String,
    pub provided_by: ProvidedBy,
    /// The question to put to the owner, read exactly as [`MissingInput::prompt`].
    ///
    /// **This type needed one too, and the doc above nearly said otherwise**
    /// (`iaam-ytvf`). «It carries no alternatives» is a statement about a second
    /// closed choice and says nothing about prose; the one field this type
    /// carries today is `/account`, one of the owner's own accounts chosen from
    /// `candidates`, and «which of your accounts» is as much a question for a
    /// person as a title is. A pointer reading `/account` is exactly as mute as
    /// one reading `/title`.
    pub prompt: Option<OwnerPrompt>,
    pub candidates: Option<Vec<AccountCandidate>>,
}

impl MissingInput {
    /// A field the owner fills in, published with the question to put to him.
    ///
    /// The pointer comes from the question, which is the whole of why this is a
    /// constructor: written separately they can disagree, and the field report
    /// behind `iaam-ytvf` is what a caller does when the pointer is all it has.
    #[must_use]
    pub fn asked(prompt: OwnerPrompt) -> Self {
        Self {
            pointer: prompt.pointer().to_owned(),
            provided_by: ProvidedBy::Owner,
            prompt: Some(prompt),
            candidates: None,
            alternatives: Vec::new(),
            optional: false,
            proposal: None,
        }
    }

    /// The same, for a field the call is accepted without.
    ///
    /// A method rather than a parameter on every constructor: a field that
    /// blocks the call is the ordinary case and the one a reader should not have
    /// to spell out, and `.optional()` at the site is where the claim is
    /// checkable — it sits beside the request schema a reader can go and look
    /// at. See [`Self::optional`] for why this is narrower than «the schema does
    /// not require it» (`iaam-4fsw`).
    #[must_use]
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// The same, carrying one answer that would fill this field on a set of
    /// items.
    ///
    /// Built at the item, because only the item knows which of its neighbours
    /// share the ground. The words are not built there: [`Proposal`] renders
    /// them from its own vocabulary, so an item hands over a value and a set and
    /// never a sentence (`iaam-hdr7`).
    #[must_use]
    pub fn proposing(mut self, proposal: Proposal) -> Self {
        self.proposal = Some(proposal);
        self
    }

    /// The same, for a field whose answer is one of the owner's own accounts.
    #[must_use]
    pub fn asked_from(prompt: OwnerPrompt, candidates: Vec<AccountCandidate>) -> Self {
        Self {
            candidates: Some(candidates),
            ..Self::asked(prompt)
        }
    }

    /// A field the owner fills in for which no question is written.
    ///
    /// The pointer is passed rather than derived, because there is no
    /// [`OwnerPrompt`] to derive it from, and that is the point: the pair must
    /// be in [`QUESTIONS_UNDER_REVIEW`], which names the bead deciding whether
    /// the field is asked for at all. Reach for this only to record that
    /// decision, never to postpone writing a sentence.
    #[must_use]
    pub fn asked_without_a_question(pointer: &str) -> Self {
        Self {
            pointer: pointer.to_owned(),
            provided_by: ProvidedBy::Owner,
            prompt: None,
            candidates: None,
            alternatives: Vec::new(),
            optional: false,
            proposal: None,
        }
    }

    /// A field nobody is asked about: a document holds it, or the caller does.
    ///
    /// [`ProvidedBy::Owner`] is deliberately not reachable through this
    /// constructor. A field the owner fills in goes through [`Self::asked`],
    /// which cannot be called without a question, or through
    /// [`Self::asked_without_a_question`], which says so in its name.
    fn plain(pointer: &str, provided_by: NobodyIsAsked) -> Self {
        Self {
            pointer: pointer.to_owned(),
            provided_by: provided_by.into(),
            prompt: None,
            candidates: None,
            alternatives: Vec::new(),
            optional: false,
            proposal: None,
        }
    }
}

/// The two sources that put no question to anybody.
///
/// [`ProvidedBy`] minus [`ProvidedBy::Owner`], and it exists so that
/// [`MissingInput::plain`] cannot be handed the one word that obliges a
/// question. A guard would catch it afterwards; a parameter type means there is
/// nothing to catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NobodyIsAsked {
    ExternalDocument,
    Caller,
}

impl From<NobodyIsAsked> for ProvidedBy {
    fn from(source: NobodyIsAsked) -> Self {
        match source {
            NobodyIsAsked::ExternalDocument => Self::ExternalDocument,
            NobodyIsAsked::Caller => Self::Caller,
        }
    }
}

/// Request information attached to an operation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPlan {
    pub preset: BTreeMap<String, serde_json::Value>,
    /// Every field of this one call still to be filled in, in the order to ask.
    ///
    /// **They may be put to the owner together, and nothing said so**
    /// (`iaam-zxc6`). Decision 0027 gave every owner-facing field its own
    /// question, and that is right: a question per field is what stops a mapping
    /// from field to question being folded into one sentence the caller has to
    /// take apart again, which is `iaam-tt71`'s finding and `docs/api/conventions.md`
    /// §5. But «each field keeps its own words» is not «each field is a separate
    /// exchange», and with only the first written down the safe reading was the
    /// slow one: an agent relaying the item that asks what to call an account and
    /// where it is held asked the two one after the other, and doubled the length
    /// of a first import for nothing.
    ///
    /// The two obligations do not conflict — the words stay per field and the
    /// fields are shown at once — and this list is already the published fact.
    /// It is ordered, it is one call's, and a caller holds all of it before it
    /// says anything to him. So what was missing was the sentence and not a
    /// structure, and it is written here, on `MissingInputDto` where it reaches
    /// the contract, and in the agent skill where it reaches the reader that was
    /// serialising them.
    pub missing: Vec<MissingInput>,
}

/// A symbolic operation identifier resolved by a transport layer.
///
/// Re-exported rather than declared here. The vocabulary moved to the core when
/// the caveat register in `iaam_core::report::confidence` began naming the
/// operations that close its entries: the queue and the register must point at
/// the same set of calls, and two lists that must agree are a list that will not.
/// Every path that named `iaam_app::actions::OperationKey` still resolves.
pub use iaam_core::operation::OperationKey;

/// One admissible way to close an action: an operation and the call that ends it.
///
/// Carries a [`RequestPlan`] of its own, which is the whole reason a set of
/// resolutions cannot be a `Vec<OperationKey>`. Two ways out of the same state
/// are two different calls with different fields — putting an account in a
/// contour needs the composition, ruling it outside needs a reason — and a bare
/// list of keys would publish the second operation while leaving the caller to
/// discover, from the specification, that it asks for anything at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionOption {
    pub operation: OperationKey,
    pub request: RequestPlan,
}

/// The only target shapes an action may have.
///
/// [`Self::Options`] exists because `reason` and `target` were able to disagree.
/// An action whose sentence named two ways to close it — add this account to a
/// contour, or record that it is deliberately outside the perimeter — could
/// publish only one, and an agent that reads `target` as the contract, which is
/// what `target` is for, could act on that one and no other. The second route
/// existed and was reachable only by reading prose and searching the
/// specification for it.
///
/// A third variant rather than turning every target into a list: most actions
/// genuinely have one way out or none, and saying «here is a set of one» about
/// them would make every consumer index into a list to find the fact it already
/// had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionTarget {
    Operation {
        operation: OperationKey,
        request: RequestPlan,
    },
    /// Two or more admissible resolutions, each with its own request plan.
    ///
    /// In the order the item wants them considered: the first is the ordinary
    /// answer, the rest are the alternatives, and none of them is the only one.
    Options(Vec<ResolutionOption>),
    None,
}

impl ActionTarget {
    /// Build the target for a computed set of resolutions, in the given order.
    ///
    /// Normalising, so that «one way out» has a single encoding no matter which
    /// side computed it: a set of one is a plain [`Self::Operation`] and not a
    /// list holding one element. Without this a builder whose options collapse
    /// at run time would publish a different transport shape for the same
    /// situation depending on how it happened to be reached.
    #[must_use]
    pub fn from_options(mut options: Vec<ResolutionOption>) -> Self {
        match options.len() {
            0 => Self::None,
            1 => {
                let only = options.remove(0);
                Self::Operation {
                    operation: only.operation,
                    request: only.request,
                }
            }
            _ => Self::Options(options),
        }
    }

    /// Every resolution this target publishes, in order.
    ///
    /// The reading side of the same normalisation: a consumer asking "what may I
    /// call to close this?" gets one answer shape whichever variant carries it,
    /// and an empty answer only where nothing in this API closes the item.
    #[must_use]
    pub fn resolutions(&self) -> Vec<(OperationKey, &RequestPlan)> {
        match self {
            Self::Operation { operation, request } => vec![(*operation, request)],
            Self::Options(options) => options
                .iter()
                .map(|option| (option.operation, &option.request))
                .collect(),
            Self::None => Vec::new(),
        }
    }
}

/// One invalid combination of action availability and target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionInvariantError {
    ReadyWithoutOperation,
    BlockedWithOperation,
    /// An item that is not blocked and offers no way out.
    ///
    /// It used to say something narrower — an item that stated no required
    /// scope — and the scope is no longer stated: it is read off the
    /// resolutions, so an item with none has no authority to publish. The
    /// combination it now refuses is [`ActionState::NeedsOwnerInput`] with
    /// [`ActionTarget::None`], which was legal and should not have been: an
    /// item the owner must act on through no call in this API is
    /// [`ActionState::Blocked`], and that is the word for it.
    NonBlockedWithoutScope,
    /// A set of resolutions holding fewer than two of them.
    ///
    /// One way out is [`ActionTarget::Operation`] and none is
    /// [`ActionTarget::None`]; a list of one would be a second encoding of a
    /// state that already has one, and the two would publish different transport
    /// shapes for the same fact. [`ActionTarget::from_options`] normalises, so
    /// reaching this means the variant was built by hand.
    OptionsWithoutChoice,
    /// Work graded required for a goal, naming no goal.
    ///
    /// The defect this whole vocabulary exists to remove, refused at the point
    /// an item is assembled. A required item that names nothing tells a client
    /// only that something stands in its way, and a queue of those reads as a
    /// precondition on everything — which is how the frontier was read, and it
    /// was never what any of it did.
    RequiredForNoGoal,
}

/// The narrower of two floors: the one a token reaching the other also reaches.
///
/// Ordering by [`Scope::admits`] rather than by a rank declared here. A rank
/// would be a second statement of «an owner may do what an agent may», and the
/// predicate pair on [`Scope`] already says it; a rank that drifted from it
/// would grade an item by an ordering the transport does not enforce.
const fn narrower(left: Scope, right: Scope) -> Scope {
    if right.admits(left) { left } else { right }
}

/// What an action is, apart from its prose and its target.
///
/// Packaged as a struct rather than five arguments: `id` and `reason` are both
/// strings and would sit next to each other in a call, where swapping them is
/// easy and noticing it is not. The same reasoning as `Posting` in the core's
/// test support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionFacts {
    pub id: String,
    pub kind: ActionKind,
    pub category: ActionCategory,
    pub state: ActionState,
    /// The account or event this item is about, when it is about one.
    pub subject: Option<ActionSubject>,
}

/// One outstanding item in the owner's computed policy frontier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    id: String,
    kind: ActionKind,
    category: ActionCategory,
    state: ActionState,
    reason: String,
    subject: Option<ActionSubject>,
    target: ActionTarget,
}

impl Action {
    /// Construct an action while rejecting a ready item without an operation.
    pub fn new(
        facts: ActionFacts,
        reason: impl Into<String>,
        target: ActionTarget,
    ) -> Result<Self, ActionInvariantError> {
        if matches!(
            (facts.state, &target),
            (ActionState::Ready, ActionTarget::None)
        ) {
            return Err(ActionInvariantError::ReadyWithoutOperation);
        }
        if matches!(
            (facts.state, &target),
            (
                ActionState::Blocked,
                ActionTarget::Operation { .. } | ActionTarget::Options(_)
            )
        ) {
            return Err(ActionInvariantError::BlockedWithOperation);
        }
        if matches!(&target, ActionTarget::Options(options) if options.len() < 2) {
            return Err(ActionInvariantError::OptionsWithoutChoice);
        }
        if facts.state != ActionState::Blocked && matches!(&target, ActionTarget::None) {
            return Err(ActionInvariantError::NonBlockedWithoutScope);
        }
        if matches!(facts.category, ActionCategory::RequiredForGoal(goals) if goals.is_empty()) {
            return Err(ActionInvariantError::RequiredForNoGoal);
        }
        Ok(Self {
            id: facts.id,
            kind: facts.kind,
            category: facts.category,
            state: facts.state,
            reason: reason.into(),
            subject: facts.subject,
            target,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn kind(&self) -> ActionKind {
        self.kind
    }

    #[must_use]
    pub const fn category(&self) -> ActionCategory {
        self.category
    }

    #[must_use]
    pub const fn state(&self) -> ActionState {
        self.state
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The narrowest scope that reaches **any** of this item's resolutions.
    ///
    /// **Read off the target, never stated beside it.** The authority a call
    /// demands is a property of the call, so the queue cannot hold an opinion
    /// about it separate from the calls it publishes:
    /// [`crate::ports::required_scope`] is the one statement, and
    /// `iaam_server::routes` gates the route by the same one. Before this it
    /// was typed in per item, and `retired_account_not_empty` proved what that
    /// costs — the item was graded owner-only while one of the three calls it
    /// offered admits an agent token, so an agent filtering on this field was
    /// told nothing was available to it when the ordinary remedy was
    /// (`iaam-woeh`).
    ///
    /// **The narrowest and not the widest, and the choice is what a client can
    /// do with the field.** A client filtering the queue by the token it holds
    /// wants one pass over the items, not a pass over every resolution of every
    /// item, and the question it is really asking is «is there anything here I
    /// can act on». The narrowest floor answers exactly that. The widest would
    /// answer «is there anything here I can finish alone», which is a question
    /// no single value can answer anyway: whether a call succeeds depends on
    /// the body, and [`MissingInput::provided_by`] is where the queue says who
    /// holds a value it cannot supply.
    ///
    /// So a client that keeps the items this scope admits sees **every item it
    /// can make at least one call on**, and does not see items where it can
    /// make none. What it does not see is *which* of an item's resolutions it
    /// may call: the item may offer three and admit it to one. That is on the
    /// resolutions, one floor each, and a client acting rather than filtering
    /// reads them there.
    ///
    /// `None` only where the item publishes no resolution at all, which by the
    /// invariants above is exactly [`ActionState::Blocked`]: nothing in this
    /// API closes it, so there is no authority to state.
    #[must_use]
    pub fn required_scope(&self) -> Option<Scope> {
        self.target
            .resolutions()
            .into_iter()
            .map(|(operation, _)| required_scope(operation))
            .reduce(narrower)
    }

    /// The account or event this item is about, when it is about one.
    #[must_use]
    pub const fn subject(&self) -> Option<&ActionSubject> {
        self.subject.as_ref()
    }

    #[must_use]
    pub const fn target(&self) -> &ActionTarget {
        &self.target
    }
}

/// The identity of an action, which is not the same thing as its kind.
///
/// The first two actions are existential — the owner has no account, the owner
/// has no contour — so the kind bounds nothing and one item of each kind can
/// exist. The milestone detectors are scoped by account and observed period:
/// their identities must distinguish simultaneous outstanding work.
fn identity(kind: ActionKind) -> String {
    kind.id().to_owned()
}

/// One question an import session holds, in the form the queue reads it.
///
/// Three facts that live in three places, resolved together because the item
/// needs all three. The stored question is JSON — [`Question`] as `iaam-ingest`
/// writes it — the answer is a column on the same row, and the state of the
/// session holding it is a different row entirely.
///
/// Parsed in [`frontier`] rather than where the item is built: a stored question
/// this build cannot read is a store failure, and it is reported where the store
/// is read rather than carried inwards as an unparsed string. Dropping such a
/// row instead would put the queue back where this action found it — an
/// outstanding question nothing mentions.
///
/// **It still fails the whole queue, and that is now the odd one out.**
/// `iaam-4jso` established the third answer for the retirement fold: name the
/// failure as an item rather than swallow it or propagate it. The same argument
/// applies here word for word — an unreadable question is a content failure,
/// dropping it is the silence this type exists to prevent, and «this session
/// holds a question this build cannot read» is a sentence an item could carry.
/// It is left alone deliberately: it is a different item to design, with a
/// different subject and a different remedy, and doing it in the same change as
/// the fold would have been two designs sharing one argument. Filed, not
/// fixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationQuestion {
    /// The stored question, its wording, and its answer if it has one.
    pub view: ImportQuestionView,
    /// The state of the session holding it.
    pub session_state: ImportSessionState,
    /// The typed question, read from [`ImportQuestionView::question`].
    pub asked: Question,
    /// What the answer did, or could still do, to the standing rules.
    ///
    /// Read through [`import_session::generalisation_of`] and not derived again
    /// here. The scenario owns that derivation — it is the same function the
    /// answering route's response is built with — and a second one would be a
    /// second answer to what one answer generalised into, published on two
    /// surfaces that would eventually disagree.
    pub generalisation: Generalisation,
    /// The row the question is about, as the classifier asks about it.
    ///
    /// Carried beside the generalisation rather than folded into it, because the
    /// two answer different questions: the generalisation says what rule the
    /// answer **would** write, and this is what the owner's existing rules are
    /// tested against to find out whether one already does. `None` where this
    /// build cannot read the row, which is the same row the generalisation calls
    /// `Impossible`.
    pub subject: Option<ClassificationSubject>,
}

/// Compute every currently outstanding setup action for an owner.
///
/// Two ports, not one, since `iaam-4hcy`. The rule store is read because one
/// item the queue publishes — «adopt the rule this answer would have written» —
/// is closed by a rule appearing in it, and a queue that cannot see the act that
/// closes an item publishes an item that never closes. A queue the owner learns
/// to ignore is the failure this whole module is written against, so the second
/// read is the price of the item existing at all.
///
/// Since `iaam-xnhu` the same argument buys a **fold of the journal**, and it is
/// the most expensive thing this function does. See [`retired_products`] for
/// what it costs, when it is paid, and why nothing cheaper answers the question.
///
/// **What fails this function and what does not.** A port that will not answer
/// fails it: there is no queue to publish, and no item could say anything about
/// a store that is not there. A fold that refuses does **not** (`iaam-4jso`) —
/// it becomes an item, because the queue is the surface the owner recovers
/// from and a request that fails takes the recovery with it. The one content
/// failure still left propagating is a stored import question this build cannot
/// parse, and [`ClassificationQuestion`] says why it is where it is.
pub async fn frontier(
    owner: OwnerId,
    store: &dyn Store,
    rules: &dyn ClassificationRuleStore,
) -> Result<Vec<Action>, AppError> {
    let accounts = store.list_accounts(owner).await?;
    let contours = store.list_contours(owner).await?;
    let exclusions = store.list_account_scope_exclusions(owner).await?;
    let transfers = store.list_account_transfer_statements(owner).await?;
    let activity = store.list_account_activity(owner).await?;
    // The reads that make a question outlive the response that raised it, and
    // that make a session outlive its questions. Every session is asked, not
    // only the open ones: eligibility is a property of the item and is decided
    // beside the gap and the completion, not by narrowing what is loaded.
    //
    // The listing carries how many rows each session holds and how many of its
    // questions are unanswered, read in the store's own statement. That is what
    // lets `import_session_unfinished` be decided for every session here
    // without a request per session — and it is why the `continue` below is
    // now only about the questions. It used to skip the session outright, which
    // meant a session holding readable rows and no open question raised no item
    // at all: the queue said nothing was outstanding while the rows sat in no
    // journal, and the next act was to import the same statement again.
    let sessions = store.list_import_sessions(owner).await?;
    let mut questions = Vec::new();
    for summary in &sessions {
        let session = &summary.session;
        let held = store.list_import_questions(owner, session.id).await?;
        if held.is_empty() {
            // The observations are read only to derive what a question's answer
            // generalised into, so a session that raised none is not read at
            // all. Every import the owner ever ran is listed here, and most of
            // them asked nothing. What the session itself holds is already in
            // hand: it is the count the listing carried.
            continue;
        }
        let observations = store.list_import_observations(owner, session.id).await?;
        for view in held {
            let asked = serde_json::from_str(&view.question).map_err(|error| {
                AppError::Store(format!("stored import question could not be read: {error}"))
            })?;
            questions.push(ClassificationQuestion {
                generalisation: import_session::generalisation_of(&observations, &view),
                subject: import_session::subject_of(&observations, &view),
                view,
                session_state: session.state,
                asked,
            });
        }
    }
    let rules = standing_rules(owner, rules).await?;
    let retirements = retired_products(owner, store).await?;
    let wanted_accounts = accounts_named_by_documents(owner, store).await?;
    let mut assertions = Vec::new();
    for account in activity
        .iter()
        .filter(|activity| activity.has_business_fact)
    {
        assertions.extend(
            store
                .list_control_assertions(owner, account.account)
                .await?,
        );
    }
    actions_from_state(&OwnerState {
        accounts: &accounts,
        contours: &contours,
        exclusions: &exclusions,
        transfers: &transfers,
        activity: &activity,
        assertions: &assertions,
        retired: retirements.as_assessment(),
        sessions: &sessions,
        questions: &questions,
        rules: &rules,
        wanted_accounts: &wanted_accounts,
    })
}

/// The accounts the owner's kept documents asked for and his directory does not
/// hold.
///
/// **Two cheap reads, and no document is opened.** The names were recorded when
/// each document was read, as an instance fact beside the bytes: nothing was
/// appended to the journal then and nothing could have been, because every
/// record that printed one of these names was refused. So the queue reads what
/// the reading wrote.
///
/// The rejected alternatives are worth naming, because the obvious one is the
/// expensive one:
///
/// - **Reading every kept document again here.** That is a parse of every
///   statement the owner ever uploaded, on every reading of the queue, to answer
///   a question that was already answered when each was read. `iaam-4jso` was
///   filed for widening this function exactly like that; a fold that fails takes
///   the whole queue with it, and this one would fail on any document a later
///   profile release stopped recognising.
/// - **Reading the refused records out of the session.** There are none. A
///   record the reader could not read never reached the session, which is
///   correct — a session holds rows, and that was not one.
/// - **Recording the names in the journal.** Nothing happened that a journal
///   records. The journal holds facts about the owner's money, and «a document
///   printed a string I could not place» is a fact about a reading.
///
/// **The gap is decided here and not stored.** The record says a document
/// printed a string; whether the directory places it is asked now, against the
/// accounts as they now stand, through
/// [`iaam_ingest::csv_source::AccountNames::resolve`] — the one implementation
/// of decision 0004's tiering, reached through the same translation the reader
/// uses, so the queue and the reader cannot disagree about the same string. A
/// stored verdict would publish an account created an hour ago.
///
/// **Folded per name and per institution, and not per document.** Two statements
/// of one bank naming the same unknown account are one account to create, and an
/// item per document would ask for it twice. Two *institutions* printing one
/// string are not one account, and that is why the institution is in the key:
/// the item mints the label that scopes a printed identifier, and folding two
/// sources into one item would mint one label for both — which is the collision
/// `provider` exists to prevent, arrived at from the other direction
/// (`iaam-9i83`). In every instance anybody has run this makes no difference,
/// because one string is printed by one institution; where it does, the honest
/// answer is two items.
///
/// **The declination is read here and decides nothing about the fold**
/// (`iaam-mk1n`). A name the owner has said is no account of his is still a name
/// his documents printed and his directory does not place, so it still folds and
/// still produces an item. What it produces is a statement of fact rather than
/// required work, which is [`account_named_by_document_action`]'s business and
/// not this function's.
async fn accounts_named_by_documents(
    owner: OwnerId,
    store: &dyn Store,
) -> Result<Vec<AccountNamedByDocument>, AppError> {
    let recorded = store.list_unresolved_accounts(owner).await?;
    if recorded.is_empty() {
        // The ordinary case, and it is worth the branch: an owner whose
        // documents all placed their accounts pays nothing for this item, in the
        // same bargain `retired_products` strikes one function down. The two
        // reads below are inside it for the same reason.
        return Ok(Vec::new());
    }
    let sources = store.list_unresolved_account_sources(owner).await?;
    let issuer_of = |document_hash: &str| -> Option<String> {
        sources
            .iter()
            .find(|source| source.document_hash == document_hash)
            .map(|source| source.issuer.clone())
    };
    let declined = store.list_declined_account_names(owner).await?;
    let directory =
        import_session::AccountDirectory::from_accounts(store.list_account_details(owner).await?);
    let names = directory.names();
    let mut wanted: Vec<AccountNamedByDocument> = Vec::new();
    for record in recorded {
        if !account_named_by_document_gap(&names, &record.printed) {
            continue;
        }
        let issuer = issuer_of(&record.document_hash);
        // By position rather than by a mutable find, so the lookup's borrow ends
        // before the push.
        let seen = wanted
            .iter()
            .position(|seen| seen.printed == record.printed && seen.issuer == issuer);
        match seen {
            Some(index) => {
                wanted[index].records = wanted[index].records.saturating_add(record.records);
                wanted[index].documents = wanted[index].documents.saturating_add(1);
            }
            None => wanted.push(AccountNamedByDocument {
                // Per string and not per institution, which is the one place
                // these two keys deliberately differ. What he declared is that
                // no account of **his** answers to the name, and his directory
                // does not hold a different answer for one source than for
                // another; the institution is in the fold key above because a
                // printed *identifier* is scoped to its source, and that is a
                // statement about identity rather than about his accounts.
                declined: declined
                    .iter()
                    .find(|statement| statement.printed == record.printed)
                    .map(|statement| statement.reason.clone()),
                printed: record.printed,
                records: record.records,
                documents: 1,
                issuer,
            }),
        }
    }
    Ok(wanted)
}

/// The owner's ceased products, each with the journal's verdict on it.
///
/// **A journal read, and it is paid for only where it can produce an item.** The
/// declarations are fetched first, and an owner who has retired nothing — which
/// is where most owners are, most of the time — costs one cheap query and no
/// fold at all. This is the same bargain the question loop above strikes when it
/// skips the observations of a session that asked nothing.
///
/// Where he has retired something, the fold is unavoidable and no cheaper read
/// stands in for it: the item's completion is «the account holds nothing», that
/// is a property of the journal, and the alternatives — asking the declaration,
/// or asking a report's caveat — are the two answers `iaam-4hcy` established
/// must not be used, because one of them is the act that raised the item and the
/// other is the report that reports it.
///
/// `Date::MAX` rather than a clock reading, and the choice is deliberate: the
/// question is "does this account hold anything, as the journal now stands", and
/// a fold bounded by today would answer «no» for an account emptied by a
/// movement the owner recorded with a later effective date. See
/// [`retired_account_completion`] for what this asks and what it does not.
///
/// **A journal that will not fold no longer fails the queue** (`iaam-4jso`). It
/// did, and the argument for that was half right: there is no honest third
/// value for `emptied`, and guessing one is either an item the owner does not
/// owe or a silence about one he does. What the argument left out is that the
/// queue is the surface an owner recovers *from* — [`standing_rules`] says so
/// two functions down, about a rule it cannot read — so an owner whose journal
/// will not fold had no queue to recover through, and the one act that could
/// repair the fold was published nowhere he would look for acts.
///
/// So the fold's refusal is neither swallowed nor propagated: it is **named**.
/// The failure is carried out as [`Retirements::NotAssessed`] and becomes an
/// item of its own — [`ActionKind::RetirementNotAssessed`] — which says that
/// this question could not be answered and offers the call that answers the
/// journal's. Nothing is guessed: the item that would have been raised is not
/// raised, and its absence is stated rather than left to be read as «nothing
/// outstanding».
///
/// **Only the fold degrades.** `list_account_retirements` and
/// `load_events_through` are the store answering at all, and a store that will
/// not answer takes every other read here with it; there is no queue to publish
/// and nothing an item could say about it. What degrades is exactly the two
/// steps that read the *content* of the events: correction resolution and the
/// balance projection.
async fn retired_products(owner: OwnerId, store: &dyn Store) -> Result<Retirements, AppError> {
    let declared = store.list_account_retirements(owner).await?;
    if declared.statements.is_empty() {
        return Ok(Retirements::Assessed(Vec::new()));
    }
    let events = store.load_events_through(owner, Date::MAX).await?;
    // The **effective** set, as every other fold in this workspace reads it: a
    // retracted movement is not on the account any more, and a retirement whose
    // row was emptied by a retraction has to read as emptied here too.
    let effective = match resolve(&events) {
        Ok(effective) => effective,
        Err(error) => return Ok(Retirements::NotAssessed(error.to_string())),
    };
    let mut balances = Balances::new();
    for event in &effective {
        if let Err(error) = balances.apply(event).map_err(ProjectionError::from) {
            return Ok(Retirements::NotAssessed(error.to_string()));
        }
    }
    Ok(Retirements::Assessed(
        declared
            .statements
            .iter()
            .map(|statement| RetiredProduct {
                account: statement.account,
                effective_on: statement.effective_on,
                // The same two tests `iaam_core::report::assets::retired_and_empty`
                // makes, over the same fold: cash in every currency and every
                // position quantity. Stated as one predicate here because the queue
                // has no rows to suppress — it has one question per account.
                emptied: balances
                    .iter_cash()
                    .filter(|(account, _)| *account == statement.account)
                    .all(|(_, money)| money.is_zero())
                    && balances
                        .iter_positions()
                        .filter(|(key, _)| key.account == statement.account)
                        .all(|(_, quantity)| quantity.0.is_zero()),
            })
            .collect(),
    ))
}

/// What one reading of the owner's retirements produced: verdicts, or a refusal.
///
/// Owned, because the fold it comes from is owned; [`RetirementAssessment`] is
/// the borrowed view [`OwnerState`] carries. Two types rather than one for the
/// ordinary reason a `String` and a `&str` are two: the state is computed in an
/// `async fn` that returns it and read in a synchronous one that borrows it,
/// and a single owned type in the state struct would make every caller of
/// [`actions_from_state`] build a `Vec` it does not have.
enum Retirements {
    /// The journal folded, and this is its verdict on each declaration. Empty
    /// is «he has retired nothing», which is also what spares [`frontier`] the
    /// fold entirely.
    Assessed(Vec<RetiredProduct>),
    /// The journal would not fold, and this is what refused.
    ///
    /// A rendered message and not the error: the item built from it prints it
    /// to the owner, and nothing branches on it. A typed error carried this far
    /// would invite a caller to decide something from it, and there is nothing
    /// here to decide — every way a fold refuses is repaired by ruling on the
    /// fact that refused.
    NotAssessed(String),
}

impl Retirements {
    fn as_assessment(&self) -> RetirementAssessment<'_> {
        match self {
            Self::Assessed(products) => RetirementAssessment::Assessed(products),
            Self::NotAssessed(refusal) => RetirementAssessment::NotAssessed(refusal),
        }
    }
}

/// The borrowed form of [`Retirements`], as the frontier's state holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetirementAssessment<'a> {
    Assessed(&'a [RetiredProduct]),
    NotAssessed(&'a str),
}

impl<'a> RetirementAssessment<'a> {
    /// The verdicts, which is none where there was no fold to take them from.
    ///
    /// Not an excuse to treat a refusal as «he has retired nothing»: the caller
    /// that uses this also matches on the refusal and raises an item for it.
    /// The accessor exists so that the loop over the products is written once.
    fn products(self) -> &'a [RetiredProduct] {
        match self {
            Self::Assessed(products) => products,
            Self::NotAssessed(_) => &[],
        }
    }
}

/// The owner's active classification rules, in the classifier's own vocabulary.
///
/// A rule this build cannot read is skipped rather than failing the queue, and
/// that is the opposite of what `create_rule` does with the same rule — on
/// purpose. There the unreadable rule is the reason to refuse, because the write
/// about to happen would be recomputed against it. Here the queue is the surface
/// the owner recovers *from*, and refusing to publish any outstanding work
/// because one stored rule is malformed takes away the only list that would tell
/// him what to do about it. What a skipped rule costs is exact: one item may go
/// on offering a proposal that a rule nobody can read already covers, which is a
/// duplicate rule at worst.
async fn standing_rules(
    owner: OwnerId,
    rules: &dyn ClassificationRuleStore,
) -> Result<Vec<ClassificationRule>, AppError> {
    Ok(rules
        .list_rules(owner)
        .await?
        .into_iter()
        .filter(|rule| rule.retired_at.is_none())
        .filter_map(|rule| rule_from_view(rule).ok())
        .collect())
}

/// Find every unresolved or informational fact in a reconciliation ledger.
///
/// The owner's accounts are an argument because every item about one publishes
/// what he calls it, and a ledger holds identifiers only.
pub fn ledger_diagnostics(
    ledger: &ReconciliationLedger,
    accounts: &[AccountView],
) -> Result<Vec<Action>, AppError> {
    diagnostics(ledger, &AccountNames::new(accounts), None)
}

/// The same facts, restricted to one account and the periods meeting one range.
///
/// A scoped sibling rather than a filter over the returned items. An `Action`
/// now publishes its account in [`Action::subject`], so half of this predicate
/// could be applied afterwards; the period cannot. A diagnostic's interval is
/// not on the envelope — it is in the ledger's own typed gaps and statuses — and
/// filtering here keeps one predicate rather than splitting it across two
/// places. It is the one `scenarios::reconciliation::report` already applies to
/// its statuses and gaps: the same account, and periods that intersect the
/// requested range.
pub fn ledger_diagnostics_for(
    ledger: &ReconciliationLedger,
    account: &AccountView,
    period: AssertionPeriod,
) -> Vec<Action> {
    // Infallible where the unscoped sibling is not: every item this call can
    // emit is about the one account the caller named and already holds, so the
    // name is in hand rather than looked up.
    let names = AccountNames::new(std::slice::from_ref(account));
    diagnostics(ledger, &names, Some((account.id, period)))
        .expect("a scoped diagnostic names only the account it was scoped to")
}

/// Whether one subject is in the requested scope. Everything is, unscoped.
fn in_scope(
    scope: Option<(AccountId, AssertionPeriod)>,
    account: AccountId,
    period: AssertionPeriod,
) -> bool {
    scope.is_none_or(|(wanted, range)| {
        account == wanted && period.from <= range.to && range.from <= period.to
    })
}

fn diagnostics(
    ledger: &ReconciliationLedger,
    names: &AccountNames<'_>,
    scope: Option<(AccountId, AssertionPeriod)>,
) -> Result<Vec<Action>, AppError> {
    let mut actions = Vec::new();
    for gap in ledger
        .gaps()
        .iter()
        .filter(|gap| in_scope(scope, gap.account, gap.period))
    {
        let category = ledger
            .statuses()
            .find(|status| status.account() == gap.account && status.period() == gap.period)
            .map_or(
                ActionCategory::required_for(ActionKind::CoverageGapUnrepaired),
                |status| {
                    if gap.dimensions.iter().all(|dimension| {
                        status.dimension(*dimension) == DimensionStatus::AcceptedIndependent
                    }) {
                        ActionCategory::Informational
                    } else {
                        ActionCategory::required_for(ActionKind::CoverageGapUnrepaired)
                    }
                },
            );
        actions.push(coverage_gap_action(names.get(gap.account)?, gap, category));
    }
    for status in ledger
        .statuses()
        .filter(|status| in_scope(scope, status.account(), status.period()))
    {
        for dimension in Dimension::all() {
            if status.dimension(dimension) == DimensionStatus::AcceptedInternal {
                actions.push(independent_confirmation_action(
                    names.get(status.account())?,
                    status.period(),
                    dimension,
                ));
            }
        }
        for (index, check) in status.outcomes().iter().enumerate() {
            let ClaimOutcome::Discrepant(discrepancy) = check.outcome else {
                continue;
            };
            actions.push(discrepancy_action(
                names.get(status.account())?,
                status.period(),
                discrepancy,
                index,
            ));
        }
    }
    sort_actions(&mut actions);
    Ok(actions)
}

/// A refused row stays refused, and this record stays as the account of it.
///
/// `Blocked` still, and the sentence now says why the two routes a reader
/// reaches for first are not the remedy — which is the half a reader previously
/// had to supply from nothing.
///
/// - `POST /v1/accounts/{account}/repairs/custody` retracts `EventKind::Trade`
///   events whose quantity leg carries a custody identifier equal to the
///   account's own; `sync::is_affected_trade` is the whole of its predicate. It
///   never reads or writes an `ImportCoverageGap`, and retracting a trade cannot
///   record a row that was never parsed.
/// - `POST /v1/corrections/imports` selects the effective journal by provenance
///   alone — `ImportTarget::covers` matches on the import identity or the source,
///   with no filter on the kind — so it retracts this very coverage-gap event
///   along with every row of that import which *did* arrive. The item would stop
///   being published because the record of the refusal was withdrawn, which is
///   the one outcome worse than the gap.
///
/// What changes this item is not an operation addressed to it. Importing the
/// interval again through a channel that reads these rows leaves the record
/// where it is — `EventKind::ImportCoverageGap` is a statement about one attempt
/// — and the `category` computed by the caller drops it to `Informational` once
/// the period reaches independent confirmation in the gap's own dimensions.
fn coverage_gap_action(account: &AccountView, gap: &Taint, category: ActionCategory) -> Action {
    let rows = if gap.rows.is_empty() {
        "the legacy record cannot name the refused rows".to_owned()
    } else {
        let names = gap
            .rows
            .iter()
            .map(|row| format!("{}:{}", row.key.source.inner(), row_name_text(&row.key.row)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("refused rows: {names}")
    };
    blocked_action(
        format!(
            "{}:{}:{}:{}:{}",
            ActionKind::CoverageGapUnrepaired.id(),
            gap.account.inner(),
            gap.period.from,
            gap.period.to,
            gap.source.inner()
        ),
        ActionKind::CoverageGapUnrepaired,
        category,
        Some(ActionSubject::Account(AccountSubject::of(account))),
        format!(
            "Account {} ({}) has a coverage gap from {} through {} in dimensions {}; {} ({} rows \
             refused). No operation in this API records a refused row, and neither obvious \
             route is one: retracting the import withdraws the rows that did arrive and this \
             record of the refusal with them, and the custody repair acts only on trades whose \
             custody was fabricated from the account identifier. Import the interval again \
             through a channel that reads these rows; this record stays, because it is a \
             statement about one attempt and not about the interval.",
            gap.account.inner(),
            account.title,
            gap.period.from,
            gap.period.to,
            gap.dimensions
                .iter()
                .map(|dimension| dimension.code())
                .collect::<Vec<_>>()
                .join(", "),
            rows,
            gap.refused
        ),
    )
}

/// Confirmation of one dimension by a channel that is not the one already used.
///
/// Promoted for cash and positions and still blocked for the other two, and the
/// split is read off the core rather than chosen. `ground_three` is the only
/// ground that reaches `AcceptedIndependent` from a second channel over the same
/// interval; it files its finding under `Ground::BrokerApiAgreesWithStatement`;
/// and `Ground::dimensions` lets that ground promote `Cash` and `Positions`
/// only. For tax basis and income the grounds that reach them — a depository
/// report, a tax-agent certificate, a payout confirming a schedule — enter
/// through `ReconciliationLedger::with_external_evidence`, which no handler
/// calls, so for those two the old claim is true and is kept.
///
/// For cash and positions, `POST /v1/brokers/{broker}/sync` closes the item.
/// `scenarios::sync` records a `ControlAssertion` per claim under the channel's
/// own source and parser version, with a document hash derived from the
/// assertion's identity rather than from a file, so it is independent of a
/// statement channel by `SourceChannel::is_independent_of` — a different parser
/// version **and** a different document, which is the whole conjunction. The
/// account and the interval are this item's own and are preset; the broker code
/// is not, because nothing in the ledger says which channel an account is held
/// at.
///
/// A second **document** closes it too — `scenarios::documents` records control
/// assertions under a source, a parser version and the file's hash — and it is
/// deliberately not published as a second resolution. `POST /v1/documents` takes
/// a binary workbook and declares no `application/json` request body, so
/// `ActionCatalog::from_openapi` refuses it with `MissingRequestSchema` and
/// registering it would fail the server's start rather than help a caller. It
/// stays in the reason, beside the honest half `start_account_import_action`
/// also keeps: fetching a document out of an institution is a step outside this
/// API, and a step outside this API is not a missing route.
///
/// **Not** `POST /v1/reconciliation/balance`. An owner-stated balance is
/// `Ground::OwnerStatedBalance`, capped at `AcceptedInternal` by design — the
/// owner may have read the same figure in the same report that was parsed — so
/// it cannot raise a dimension past the level this item reports.
///
/// The promoted half reads `agent`, for the reason `start_account_import_action`
/// gives: the floor `sync_broker` keeps is [`Scope::Agent`], and an item marked
/// owner-only would tell an agent it may not send a request the server would
/// accept. The item states no scope of its own — it is read off the route it
/// names, through [`crate::ports::required_scope`].
fn independent_confirmation_action(
    account: &AccountView,
    period: AssertionPeriod,
    dimension: Dimension,
) -> Action {
    let id = format!(
        "{}:{}:{}:{}:{}",
        ActionKind::IndependentConfirmationMissing.id(),
        account.id.inner(),
        period.from,
        period.to,
        dimension.code()
    );
    let observed = format!(
        "Account {} ({}) reached internal confirmation for {} from {} through {} but has no \
         confirmation from a different parser and document",
        account.id.inner(),
        account.title,
        dimension.code(),
        period.from,
        period.to
    );
    if !matches!(dimension, Dimension::Cash | Dimension::Positions) {
        return blocked_action(
            id,
            ActionKind::IndependentConfirmationMissing,
            ActionCategory::required_for(ActionKind::IndependentConfirmationMissing),
            Some(ActionSubject::Account(AccountSubject::of(account))),
            format!(
                "{observed}. No operation in this API confirms this dimension from a second \
                 channel: a broker channel agreeing with a statement raises cash and positions \
                 and nothing else, and the grounds that reach tax basis and income — a \
                 depository report, a tax-agent certificate, a payout confirming a schedule — \
                 are recorded as external evidence, which no route accepts."
            ),
        );
    }
    let mut preset = BTreeMap::new();
    preset.insert("account".to_owned(), account.id.inner().to_string().into());
    preset.insert("from".to_owned(), period.from.to_string().into());
    preset.insert("to".to_owned(), period.to.to_string().into());
    Action::new(
        ActionFacts {
            id,
            kind: ActionKind::IndependentConfirmationMissing,
            category: ActionCategory::required_for(ActionKind::IndependentConfirmationMissing),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "{observed}. Synchronise a broker channel over this same interval: its assertions \
             carry the channel's own parser version and a document that is not the statement's, \
             which is what independence means here. A second statement from another institution \
             confirms it as well, and no operation here fetches one — the owner obtains the \
             document himself, and uploading it is not a call the queue can name. Restating the \
             balance yourself does not confirm it: an owner-stated figure is capped at internal \
             confirmation on purpose.",
        ),
        ActionTarget::Operation {
            operation: OperationKey::SyncBroker,
            request: RequestPlan {
                preset,
                // A path segment, published as a missing field exactly as
                // `start_account_import_action` publishes it: which broker holds
                // this account is the owner's to name, and the ledger does not
                // record it.
                missing: vec![MissingInput::asked(OwnerPrompt::BrokerChannel)],
            },
        },
    )
    .expect("an independent confirmation item names the channel that supplies it")
}

/// Which side is wrong is the owner's to say, and one operation says it.
///
/// The old sentence stopped after its true half. The system genuinely cannot
/// tell a claim that is wrong from a journal that is; it does not follow that
/// nothing in this API acts on the item, and `POST /v1/corrections` acts on it
/// from either side:
///
/// - the claim side — `ReconciliationLedger::build_with` resolves corrections
///   before it collects groups, so a retracted `ControlAssertion` forms no group
///   and the check reported here goes with it;
/// - the journal side — `observe` runs over that same effective set, so an event
///   superseded or retracted changes what was observed and `check_claim` is
///   asked again.
///
/// **Not** `POST /v1/reconciliation/balance`. Recording another balance appends
/// a group: `merge_status` extends `outcomes` and keeps `Discrepant` from either
/// side on purpose, because confirmation from a second document must not
/// override a problem already detected. The discrepant check this item reads
/// would sit in the status exactly where it was, and the item would be published
/// again after the call.
///
/// Nothing is preset. A `Discrepancy` carries a field name and three figures and
/// no event identifier, so the target of a correction cannot be proposed from it
/// — the reasoning `undecomposed_outflows_action` states about a matcher, for
/// the same reason: the diagnostic deliberately does not retain what it would
/// take to fill the field.
///
/// The item reads `owner`, because that is the floor `submit_corrections` keeps
/// and it is kept so that an agent token cannot retract the owner's history. The
/// item does not grade itself: the floor comes from the operation it names.
fn discrepancy_action(
    account: &AccountView,
    period: AssertionPeriod,
    discrepancy: Discrepancy,
    index: usize,
) -> Action {
    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}:{}:{}:{}:{}",
                ActionKind::DiscrepancyUnresolved.id(),
                account.id.inner(),
                period.from,
                period.to,
                discrepancy.field,
                index
            ),
            kind: ActionKind::DiscrepancyUnresolved,
            category: ActionCategory::required_for(ActionKind::DiscrepancyUnresolved),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) has an unresolved {} discrepancy from {} through {}: claimed {}, \
             observed {}, delta {}. The system cannot identify which side is wrong; you can, \
             and one operation settles either — retract the control assertion that claimed \
             wrongly, or supersede the journal event that recorded wrongly. Recording another \
             balance does not settle it: a detected discrepancy is kept through every later \
             confirmation, by design.",
            account.id.inner(),
            account.title,
            discrepancy.field,
            period.from,
            period.to,
            claim_value_text(discrepancy.claimed),
            claim_value_text(discrepancy.observed),
            claim_value_text(discrepancy.delta)
        ),
        ActionTarget::Operation {
            operation: OperationKey::SubmitCorrections,
            request: RequestPlan {
                // Nothing: this item names an account, an interval and three
                // figures, and the request names events.
                preset: BTreeMap::new(),
                missing: vec![
                    MissingInput::asked(OwnerPrompt::Corrections),
                    MissingInput::asked(OwnerPrompt::AcknowledgeRetraction),
                ],
            },
        },
    )
    .expect("a discrepancy item names the correction operation that settles it")
}

/// Find undecomposed outflows and unexplained account residuals in a flow report.
pub fn flow_diagnostics(
    report: &MoneyFlowReport,
    accounts: &[AccountView],
) -> Result<Vec<Action>, AppError> {
    let names = AccountNames::new(accounts);
    let mut actions = Vec::new();
    for currency in report.flow.currencies() {
        let undecomposed = report
            .flow
            .not_decomposed_by_account_and_cause(currency)
            .expect("money flow undecomposed breakdown");
        for (account, cause, count, amount) in undecomposed {
            let named = names.get(account)?;
            actions.push(match cause {
                UndecomposedCause::NoRuleMatched => {
                    undecomposed_outflows_action(named, currency, count, amount)
                }
                UndecomposedCause::ExternalTransfer => {
                    external_transfers_action(named, currency, count, amount)
                }
            });
        }
    }
    for (account, amount) in report
        .flow
        .residuals_by_account()
        .expect("money flow residual breakdown")
    {
        actions.push(unexplained_residual_action(names.get(account)?, amount));
    }
    sort_actions(&mut actions);
    Ok(actions)
}

/// The cash an account's own quantities do not account for.
///
/// `Blocked` stands; the sentence that earned it did not. It said no **report**
/// operation can resolve the residual, which is verbatim the phrasing
/// `undecomposed_outflows_action` records as having been wrong for the same
/// reason a few lines below: the action catalogue resolves a target against the
/// whole completed contract, not a report-local namespace, so «no report
/// operation» grades nothing at all. Left there it read as the sentence that had
/// already bought `Blocked` wrongly once.
///
/// The true reason is narrower, and it is now the one published. A residual is
/// `MoneyFlow::residual_of` — an account's cash delta less the seven quantities
/// that explain it — aggregated over one account and one currency. It names no
/// event. Every operation that could move it is addressed to an event the caller
/// names: `POST /v1/corrections` supersedes or retracts one, and this figure
/// identifies none, so publishing that route here would name a request whose
/// only required field this item cannot fill.
///
/// `Informational` is unchanged and is right: nothing is hidden. The figure is
/// published as its own line rather than folded into a bucket, which is what
/// makes it a fact worth stating rather than work waiting to be done.
fn unexplained_residual_action(account: &AccountView, amount: Money) -> Action {
    blocked_action(
        format!(
            "{}:{}:{}",
            ActionKind::UnexplainedResidual.id(),
            account.id.inner(),
            amount.currency().code()
        ),
        ActionKind::UnexplainedResidual,
        ActionCategory::Informational,
        Some(ActionSubject::Account(AccountSubject::of(account))),
        format!(
            "Account {} ({}) has an unexplained residual of {} {}: the seven report quantities \
             do not add up to its cash change. This is an aggregate over one account and one \
             currency and it names no event, so no operation in this API is addressed to it — \
             a correction acts on an event the caller names, and this figure names none.",
            account.id.inner(),
            account.title,
            amount.to_calc_dec().inner(),
            amount.currency().code()
        ),
    )
}

/// The owner's remedy for outflow rows no category rule matched.
///
/// `NeedsOwnerInput` rather than `Blocked`, because `Blocked` means "no operation
/// in this API is available for this item" and category-rule creation is in this
/// same API. The earlier wording — no *report* operation can provide a rule — was
/// true and irrelevant: the action catalogue resolves a target against the whole
/// completed contract, not a report-local namespace, and owner-only is what the
/// floor of `create_category_rule` says, not what `Blocked` says. `first_contour_action` is the
/// precedent: the agent may not draw the boundary, and the action still names the
/// owner-only operation and the inputs only he can supply.
///
/// `Recommended`, not `RequiredForGoal`. The distinction the control-assertion
/// actions were promoted on is whether the absence makes the reported number mean
/// something other than what it says: without an opening assertion the cash figure
/// is a movement and not a balance, so the figure is wrong. Nothing here is wrong.
/// `went_out` already counts these rows in full, the report names the undecomposed
/// amount as its own line rather than hiding it in a bucket, and the identity still
/// closes. What is missing is the breakdown by what the money was for — real quality
/// work, and optional in the sense the category intends.
///
/// Nothing is preset. The rule request takes a matcher, a category and a validity
/// interval, and this aggregate justifies none of them:
///
/// - The **interval** is not the report window. A window is where the owner
///   happened to look; a category's meaning did not begin and end there, and
///   presetting `valid_from`/`valid_to` from `from`/`to` would write that claim
///   into his rules.
/// - The **matcher** cannot be proposed from what the aggregate keeps — an
///   account, a currency, a row count and a net amount, none of which are fields
///   of the rule request. Proposing one would need the diagnostic to retain row
///   keys or source descriptions, which it deliberately does not.
/// - The **category** is the owner's judgement by the same rule that forbids
///   inventing one anywhere else.
fn undecomposed_outflows_action(
    account: &AccountView,
    currency: CurrencyCode,
    count: u64,
    amount: Money,
) -> Action {
    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}:{}",
                ActionKind::UndecomposedOutflows.id(),
                account.id.inner(),
                currency.code()
            ),
            kind: ActionKind::UndecomposedOutflows,
            category: ActionCategory::Recommended,
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) has {} outflow rows totaling {} {} that no category rule matched; \
             create a rule that matches them and names what they were for. The rows are \
             not identified here, so neither the matcher nor the category is proposed, \
             and the interval a rule is valid over is not the interval of this report.",
            account.id.inner(),
            account.title,
            count,
            amount.to_calc_dec().inner(),
            currency.code()
        ),
        ActionTarget::Operation {
            operation: OperationKey::CreateCategoryRule,
            request: RequestPlan {
                preset: BTreeMap::new(),
                missing: vec![
                    MissingInput::asked(OwnerPrompt::RuleMatcher),
                    MissingInput::asked(OwnerPrompt::RuleCategory),
                ],
            },
        },
    )
    .expect("undecomposed outflows action has an operation target")
}

/// Transfers that left the contour and can never carry a category.
///
/// These sit in the same undecomposed total as the rows above and have no remedy
/// in common with them. `MoneyFlow::apply` asks the category index only for
/// `CashOut`, `Refund` and `Income`; a `CashTransfer` is never offered to it, so a
/// rule matching this row would still assign nothing. Pointing the owner at rule
/// creation here would be a falsehood, and pointing him there for a mixed account
/// would be a half-truth about the transfer half — which is why the aggregate is
/// split at its source rather than relabelled.
///
/// So `Blocked` is correct for exactly the reason it was wrong above: no operation
/// in this API acts on this item. `Informational` follows — it is a fact, and the
/// fact is worth emitting because without it the undecomposed total in the report
/// has an unexplained remainder.
fn external_transfers_action(
    account: &AccountView,
    currency: CurrencyCode,
    count: u64,
    amount: Money,
) -> Action {
    blocked_action(
        format!(
            "{}:{}:{}",
            ActionKind::ExternalTransfersUncategorised.id(),
            account.id.inner(),
            currency.code()
        ),
        ActionKind::ExternalTransfersUncategorised,
        ActionCategory::Informational,
        Some(ActionSubject::Account(AccountSubject::of(account))),
        format!(
            "Account {} ({}) has {} transfer rows totaling {} {} that left the contour and \
             carry no category; a category rule cannot decompose them, because category \
             assignment is never consulted for a transfer. Nothing in this API changes that.",
            account.id.inner(),
            account.title,
            count,
            amount.to_calc_dec().inner(),
            currency.code()
        ),
    )
}

/// Find an import-time possible duplicate that has no stored decision.
pub fn verdict_diagnostics(verdict: &Verdict) -> Option<Action> {
    let Verdict::PossibleDuplicate {
        event, of, level, ..
    } = verdict
    else {
        return None;
    };
    Some(blocked_action(
        format!(
            "{}:{}:{}:{}",
            ActionKind::PossibleDuplicateUndecided.id(),
            event.inner(),
            of.inner(),
            level.number()
        ),
        ActionKind::PossibleDuplicateUndecided,
        ActionCategory::required_for(ActionKind::PossibleDuplicateUndecided),
        Some(ActionSubject::Event(*event)),
        format!(
            "Event {} may duplicate event {} at deduplication level {}; the owner must decide and no decision operation exists in this API.",
            event.inner(),
            of.inner(),
            level.number()
        ),
    ))
}

/// The diagnostics for every verdict one import produced, in the settled order.
///
/// The plural of [`verdict_diagnostics`], and the reason it exists is the
/// ordering: a carrier that mapped the verdicts itself would sort at the call
/// site, and two carriers sorting separately is how two orders appear.
#[must_use]
pub fn verdicts_diagnostics(verdicts: &[Verdict]) -> Vec<Action> {
    let mut actions: Vec<Action> = verdicts.iter().filter_map(verdict_diagnostics).collect();
    sort_actions(&mut actions);
    actions
}

fn blocked_action(
    id: String,
    kind: ActionKind,
    category: ActionCategory,
    subject: Option<ActionSubject>,
    reason: String,
) -> Action {
    Action::new(
        ActionFacts {
            id,
            kind,
            category,
            state: ActionState::Blocked,
            subject,
        },
        reason,
        ActionTarget::None,
    )
    .expect("blocked diagnostic has no operation or scope")
}

fn sort_actions(actions: &mut [Action]) {
    actions.sort_by(|left, right| {
        // By urgency, never by the category value: two required items differ
        // only in which goals they name, and that is not an order.
        left.category()
            .rank()
            .cmp(&right.category().rank())
            .then_with(|| left.id().cmp(right.id()))
    });
}

fn claim_value_text(value: ClaimValue) -> String {
    match value {
        ClaimValue::Money { amount, currency } => format!(
            "{} {}",
            Money::new(amount, currency).to_calc_dec().inner(),
            currency.code()
        ),
        ClaimValue::Quantity(quantity) => quantity.0.inner().to_string(),
    }
}

fn row_name_text(name: &RowName) -> String {
    match name {
        RowName::Given(name) => format!("given:{name}"),
        RowName::Fingerprint(name) => format!("fingerprint:{name}"),
    }
}
/// Everything the frontier is computed from, read once and passed together.
///
/// A struct rather than seven positional slices, for the reason [`ActionFacts`]
/// is one: the reads grow by one with each goal the queue learns, and an
/// argument list that long is one a caller gets into the wrong order without the
/// compiler noticing.
struct OwnerState<'a> {
    accounts: &'a [AccountView],
    contours: &'a [ContourView],
    exclusions: &'a [AccountScopeExclusionView],
    transfers: &'a [AccountTransferStatementView],
    activity: &'a [AccountActivityView],
    assertions: &'a [ControlAssertionView],
    /// The owner's products that have ceased, each with the journal's verdict on
    /// whether anything is still on it — or the reason the journal would not
    /// fold, which is a fact the queue publishes rather than a request it fails
    /// (`iaam-4jso`).
    ///
    /// [`RetirementAssessment::Assessed`] holding nothing is «he has retired
    /// nothing», which is where most owners stay and is also what spares
    /// [`frontier`] the journal read this field is folded from. That is not the
    /// same state as [`RetirementAssessment::NotAssessed`], and the enum is
    /// here so that the two cannot be spelled alike: an empty slice standing
    /// for a failed fold is exactly the silence this field exists to break.
    retired: RetirementAssessment<'a>,
    /// Every import session of the owner's, with how much each holds.
    ///
    /// Every session and not only the open ones, for the reason the questions
    /// beside them are all here: whether an item is raised for one is decided
    /// by an eligibility and a gap written beside its completion, and a list
    /// narrowed on the way in would decide it twice — once in the reader, where
    /// nothing says so, and once here.
    sessions: &'a [ImportSessionSummaryView],
    questions: &'a [ClassificationQuestion],
    /// The owner's standing classification rules, as the classifier reads them.
    ///
    /// Read for one purpose: to find out whether a proposal the queue would
    /// offer has already been adopted. Nothing else here consults them, and an
    /// empty slice is «he has written none», never «they were not fetched».
    rules: &'a [ClassificationRule],
    /// The accounts a kept document asked for that the owner's directory still
    /// does not place.
    ///
    /// **Already filtered**, which is the one field here that arrives with its
    /// gap decided. The eligibility — a document printed this string — is what
    /// the instance recorded when it read the document; the gap — and no account
    /// of his answers to it — is a question about the directory, and it is
    /// answered in [`frontier`] through the one implementation of decision
    /// 0004's tiering rather than through a second copy of it here. An empty
    /// slice is «every account his documents named is in his directory», never
    /// «nothing was read».
    wanted_accounts: &'a [AccountNamedByDocument],
}

/// Fallible for one reason: every item about an account publishes what the
/// owner calls it, and the name is read from the account list handed in here.
/// An activity row, a held question or an assertion naming an account that list
/// does not hold is the store contradicting itself — see [`AccountNames::get`].
fn actions_from_state(state: &OwnerState<'_>) -> Result<Vec<Action>, AppError> {
    let OwnerState {
        accounts,
        contours,
        exclusions,
        transfers,
        activity,
        assertions,
        retired,
        sessions,
        questions,
        rules,
        wanted_accounts,
    } = *state;
    let names = AccountNames::new(accounts);
    let mut actions =
        actions_from_views(accounts, contours, exclusions, transfers, wanted_accounts);
    actions.reserve(
        activity.len()
            + assertions.len()
            + retired.products().len()
            + questions.len()
            + sessions.len(),
    );
    for account in activity
        .iter()
        .filter(|activity| account_import_eligibility(activity) && account_import_gap(activity))
    {
        actions.push(start_account_import_action(names.get(account.account)?));
    }
    for account in activity
        .iter()
        .filter(|activity| control_assertion_eligibility(activity).is_some())
    {
        let Some(period) = control_assertion_eligibility(account) else {
            continue;
        };
        let dimension = Dimension::Cash;
        // The opening point is asked for first, and the closing one is not asked
        // for until it is answered. A closing balance compared against a sum
        // accumulated from an unasserted start yields a discrepancy that is not
        // one: it is the opening balance nobody asked for. Emitting both at once
        // would put the second question before the first is answered.
        if let Some(point) = [BalancePoint::Opening, BalancePoint::Closing]
            .into_iter()
            .find(|point| {
                control_assertion_gap(assertions, account.account, period, *point, dimension)
            })
        {
            actions.push(provide_control_assertion_action(
                names.get(account.account)?,
                period,
                point,
            ));
        }
    }
    // After the control assertions, because the kind is declared after theirs
    // and the frontier's order must be non-decreasing in that order. The two
    // belong together for a better reason than the ordering: both are what an
    // account whose history begins mid-way produces.
    for product in retired
        .products()
        .iter()
        .filter(|product| retired_account_eligibility(product) && retired_account_gap(product))
    {
        actions.push(retired_account_action(names.get(product.account)?, product));
    }
    // Exactly where the loop above would have run, and instead of it: the two
    // are the same read, and a fold either produced verdicts or produced this.
    // The kind is declared straight after `RetiredAccountNotEmpty` so that
    // emitting it here keeps the frontier's order non-decreasing.
    if let RetirementAssessment::NotAssessed(refusal) = retired {
        actions.push(retirement_not_assessed_action(refusal));
    }
    // Last, so the frontier's kinds stay non-decreasing in `ActionKind`'s own
    // order: this kind is declared after the control assertion.
    for question in questions.iter().filter(|question| {
        classification_question_eligibility(question) && classification_question_gap(question)
    }) {
        actions.push(answer_classification_question_action(
            question,
            names.get(question.asked.account())?,
            accounts,
        ));
    }
    // After them, for the same reason and in the same order: an answered
    // question's rule is declared after the question it comes out of.
    for question in questions {
        let Some((matcher, outcome)) = adopt_classification_rule_eligibility(question) else {
            continue;
        };
        if adopt_classification_rule_gap(rules, question.subject.as_ref(), outcome) {
            actions.push(adopt_classification_rule_action(
                question,
                names.get(question.asked.account())?,
                matcher,
                outcome,
            ));
        }
    }
    // And after those, for the same reason: the session is declared after the
    // two items about its questions. The order is the queue's, not a ranking —
    // a session with an open question raises both this item and that one, and
    // they say different things: «this row is unclassified» and «this import
    // has not ended».
    for summary in sessions
        .iter()
        .filter(|summary| import_session_eligibility(summary) && import_session_gap(summary))
    {
        actions.push(import_session_unfinished_action(
            summary,
            summary
                .session
                .account
                .map(|account| names.get(account))
                .transpose()?,
        ));
    }
    Ok(actions)
}

/// An account is always eligible to be imported into.
///
/// Kept as a named function beside the gap and the completion rather than
/// folded away: the three are separate concepts everywhere else in this module,
/// and an eligibility that silently does not exist is how the distinction rots.
const fn account_import_eligibility(_activity: &AccountActivityView) -> bool {
    true
}

fn account_import_gap(activity: &AccountActivityView) -> bool {
    !account_import_completion(activity)
}

fn account_import_completion(activity: &AccountActivityView) -> bool {
    activity.has_business_fact
}

fn control_assertion_eligibility(activity: &AccountActivityView) -> Option<AssertionPeriod> {
    activity
        .has_business_fact
        .then(|| activity_period(activity))
        .flatten()
}

fn control_assertion_gap(
    assertions: &[ControlAssertionView],
    account: AccountId,
    period: AssertionPeriod,
    point: BalancePoint,
    dimension: Dimension,
) -> bool {
    !control_assertion_completion(assertions, account, period, point, dimension)
}

fn control_assertion_completion(
    assertions: &[ControlAssertionView],
    account: AccountId,
    period: AssertionPeriod,
    point: BalancePoint,
    dimension: Dimension,
) -> bool {
    assertions.iter().any(|assertion| {
        assertion.account == account
            && assertion.period == period
            && assertion.point == Some(point)
            && assertion.dimension == dimension
    })
}

/// A question can be answered while the session holding it is open.
///
/// The eligibility, and it is not a formality. A committed session has every
/// question answered — commit refuses otherwise — and an abandoned one will
/// never be committed, so the route that answers its questions has nothing to
/// settle and refuses too. An item for either would be work the owner cannot do,
/// which is the kind of item a queue is learned to ignore for.
const fn classification_question_eligibility(question: &ClassificationQuestion) -> bool {
    matches!(question.session_state, ImportSessionState::Open)
}

fn classification_question_gap(question: &ClassificationQuestion) -> bool {
    !classification_question_completion(question)
}

/// The goal is satisfied by an answer to **this** question and by nothing else.
///
/// Quantified over the questions, not over the sessions. «This session has no
/// open question» is not a property a new question preserves, and a goal written
/// that way would close the moment the last question of a session was answered
/// and stay closed when the next row raised another. Asked of each question, a
/// new question reopens the goal however many are already answered — the same
/// property [`transfer_relationships_completion`] has for a new account.
///
/// Abandoning the session does not satisfy this. It removes the item by failing
/// the eligibility above, and «the owner said what the row was» and «the owner
/// threw the row away» are different facts that must not read as one.
fn classification_question_completion(question: &ClassificationQuestion) -> bool {
    !question.view.is_open()
}

/// A row the source described too thinly to classify, waiting on the owner.
///
/// `RequiredForGoal`, not `Blocking`, and the two readings were close enough to
/// need deciding rather than feeling. `Blocking` is documented here as "work
/// that prevents the system from accepting another action", and an open question
/// does not: every other import runs, every other account is asserted over, every
/// other session commits. What it prevents is the commit of the one session the
/// owner himself opened, and he can abandon that session instead — a refusal
/// scoped to one piece of work he controls is not the system refusing to accept
/// another action.
///
/// What it does do is leave a row out of the journal while the owner is shown
/// figures computed without it, with nothing on the figure saying so. That is
/// exactly the line the control assertions and the transfer statements were
/// graded on — the absence changes what the reported numbers mean — and it is
/// `RequiredForGoal` for the same reason. Grading it `Blocking` would also sort
/// it above «create your first account», which is the one item that genuinely
/// stops everything.
///
/// `NeedsOwnerInput`, not `Ready`. `classify` refuses to guess a direction the
/// source did not state; an agent choosing one on the owner's behalf is that
/// same guess made one layer up, and a fully preset request does not change who
/// may send it.
///
/// This is the first item that reads `agent` rather than `owner`, and it reads
/// it because `answer_import_question` keeps [`Scope::Agent`] as its floor. The
/// queue's business is to say what may be called: an item marked `owner` would
/// tell an agent it may not send a request the server would accept. Who decides
/// the answer and who may transmit it are different questions, and
/// `NeedsOwnerInput` is where the first one is already answered.
fn answer_classification_question_action(
    question: &ClassificationQuestion,
    account: &AccountView,
    accounts: &[AccountView],
) -> Action {
    let mut preset = BTreeMap::new();
    // Both are path segments of the answering route, and both are known: the
    // question is the subject of this item and the session is on its row.
    preset.insert(
        "session".to_owned(),
        question.view.session.inner().to_string().into(),
    );
    preset.insert(
        "question".to_owned(),
        question.view.id.inner().to_string().into(),
    );

    Action::new(
        ActionFacts {
            // One identity per question, so a second question is a second item
            // and an agent deduplicating by id never collapses them — the same
            // rule `start_account_import` follows per account.
            id: format!(
                "{}:{}",
                ActionKind::AnswerClassificationQuestion.id(),
                question.view.id.inner()
            ),
            kind: ActionKind::AnswerClassificationQuestion,
            category: ActionCategory::required_for(ActionKind::AnswerClassificationQuestion),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) has row {} of import session {} held unclassified. {} Nothing else \
             is refused while it stands, but the row is in no journal and in no report, and the \
             session holding it will not commit until it is answered. The answer is written as a \
             rule, so a row matching it settles by itself next time.",
            account.id.inner(),
            account.title,
            question.view.row,
            question.view.session.inner(),
            question.view.prompt,
        ),
        ActionTarget::Operation {
            operation: OperationKey::AnswerImportQuestion,
            request: RequestPlan {
                preset,
                missing: vec![answer_input(&question.asked, accounts)],
            },
        },
    )
    .expect("classification question action has an operation target")
}

/// Which answered questions have a rule the owner could still adopt.
///
/// Returns the proposal itself, in the manner of
/// [`control_assertion_eligibility`]: the caller needs the matcher and the
/// outcome to decide the gap and to build the item, and computing eligibility as
/// a bare `bool` would mean destructuring the same state a second time to get
/// them.
///
/// Two conditions, and each removes work the owner cannot do.
///
/// The generalisation must be [`Generalisation::Available`]. The other three
/// states offer nothing to adopt: `Recorded` means the answer already wrote the
/// rule, `Unanswered` means there is no decision to generalise yet, and
/// `Impossible` means the row prints nothing a matcher could ask about — the one
/// state of the four that no call of anybody's can change, which is why it is
/// stated in the enum and why nothing is queued for it.
///
/// And the session must not be abandoned. An abandoned session's rows were never
/// facts — [`crate::scenarios::import_session::abandon_session`] neither reads
/// nor writes the journal — and offering the owner a standing decision learned
/// from a row he threw away would generalise from evidence he withdrew. A
/// **committed** session is not excluded, and deliberately: its rows are in the
/// journal, the answer stands, and the rule is exactly as useful the day after
/// the commit as the day before it.
fn adopt_classification_rule_eligibility(
    question: &ClassificationQuestion,
) -> Option<(&RuleMatcher, Classification)> {
    if matches!(question.session_state, ImportSessionState::Abandoned) {
        return None;
    }
    match &question.generalisation {
        Generalisation::Available { matcher, outcome } => Some((matcher, *outcome)),
        Generalisation::Recorded { .. }
        | Generalisation::Unanswered
        | Generalisation::Impossible => None,
    }
}

fn adopt_classification_rule_gap(
    rules: &[ClassificationRule],
    subject: Option<&ClassificationSubject>,
    outcome: Classification,
) -> bool {
    !adopt_classification_rule_completion(rules, subject, outcome)
}

/// The goal: a row like this one settles by itself next time.
///
/// **Read from the owner's rules, not from the question.** The question goes on
/// reporting `available` after he adopts the proposal — that is decided where
/// [`Generalisation::Available`] is documented, and for a good reason: the rule
/// he creates is his own act, recorded in his rule listing, and claiming the
/// question wrote it would attribute his decision to the import. But an item
/// whose completion cannot be observed is an item that never leaves the queue,
/// and a queue with a permanent entry in it is one the owner learns to ignore.
/// So the queue asks the other question — «does a standing rule of his now
/// settle this row the way he answered?» — and that one has an answer.
///
/// **Matching, not equality with the proposal.** He may narrow the matcher
/// before he sends it, or broaden it, or have written a rule of his own last
/// month that happens to cover the row; all three mean the work is done. Field
/// equality would close the item for exactly one of them and go on nagging about
/// the rest, which is the same defect one comparison further in.
///
/// The subject is the row read **without** resolving its counterparty against
/// the directory — see [`crate::scenarios::import_session::subject_of`] — which
/// is the only reading a stored matcher can be tested under. `None` there is a
/// row this build cannot read; such a row generalises into `Impossible` and is
/// never eligible, so the absence answers «not complete» and is never reached.
fn adopt_classification_rule_completion(
    rules: &[ClassificationRule],
    subject: Option<&ClassificationSubject>,
    outcome: Classification,
) -> bool {
    subject.is_some_and(|subject| {
        rules
            .iter()
            .any(|rule| rule.outcome == outcome && rule.matcher.matches(subject))
    })
}

/// The rule an answer would have written, offered to the one who may write it.
///
/// **This is `iaam-4hcy`.** `Generalisation::Available` was honest and
/// unreachable: a client could read that a rule was possible and that none was
/// written, and no queued act turned it into one. A state the system reports
/// truthfully with no act that resolves it is a dead end dressed as information,
/// and the owner — the only principal who may generalise — was the one reading
/// it.
///
/// **`create_classification_rule`, and no new route.** The proposal is already
/// published in the exact body `POST /v1/classification-rules` takes; the route
/// exists, it is owner-only, and it is the same act. What was missing was its
/// name in [`OperationKey`], so the queue could address it. A route of its own —
/// «adopt the rule of question N» — would have been a second way to write a
/// classification rule, and it would have had to decide whether the rule it
/// created belongs to the question, which is the one thing
/// [`Generalisation::Available`] says it must not.
///
/// **`Recommended`, not required for any goal.** The row this was learned from
/// is already settled and already in the journal; nothing the owner can run
/// today is short of anything while the rule stands unwritten. What it costs him
/// is the same question again next month.
///
/// **`NeedsOwnerInput` with nothing missing, which is not a contradiction.**
/// Every field of the request is preset — the matcher and the outcome are
/// derived from the row and the answer, and `replaces` is absent because a
/// proposal supersedes nothing. What is missing is not a value; it is his
/// decision, and [`ActionState::Ready`] means «may be invoked without asking the
/// owner», which this must never be. It reads `owner`, because generalising is
/// the administer decision arriving by another door — the same gate
/// `may_generalise` reads when it declines to write the rule in the first place,
/// and the floor `create_classification_rule` keeps for the same reason.
fn adopt_classification_rule_action(
    question: &ClassificationQuestion,
    account: &AccountView,
    matcher: &RuleMatcher,
    outcome: Classification,
) -> Action {
    let mut preset = BTreeMap::new();
    // Written through the encoders beside the one reader of the rule format, not
    // assembled here. A second writer would drift from that reader, and the
    // owner would post a body this build composed and the classifier cannot read
    // back. The matcher goes in its request shape rather than its storage one:
    // this preset and the proposal published on the question it came from are
    // one rule, and a rule published twice in two shapes is two that nothing
    // compares.
    preset.insert("matcher".to_owned(), matcher_request_json(matcher));
    preset.insert("outcome".to_owned(), outcome_json(outcome));

    Action::new(
        ActionFacts {
            // One identity per question, as the answering item has: two answered
            // questions are two proposals, and an agent deduplicating by id must
            // not collapse them into one.
            id: format!(
                "{}:{}",
                ActionKind::AdoptClassificationRule.id(),
                question.view.id.inner()
            ),
            kind: ActionKind::AdoptClassificationRule,
            category: ActionCategory::Recommended,
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Row {} of import session {} on account {} ({}) was answered, and the answer \
             wrote no standing rule: whoever answered it may settle a row but may not \
             generalise. The rule it would have made is presented here as the body to \
             send. Nothing already imported changes if you send it; what changes is that \
             the next row it matches settles without asking. Read the condition before you \
             do — you may narrow it or widen it, and a rule you adopt is one you can \
             retire, which replans whatever it classified.",
            question.view.row,
            question.view.session.inner(),
            account.id.inner(),
            account.title,
        ),
        ActionTarget::Operation {
            operation: OperationKey::CreateClassificationRule,
            request: RequestPlan {
                preset,
                missing: Vec::new(),
            },
        },
    )
    .expect("adopt classification rule action has an operation target")
}

/// A session that holds rows is one there is something to decide about.
///
/// The eligibility, and it is what keeps the queue quiet about the sessions
/// nothing is at stake in. A session holding nothing is what `open_session`
/// hands back to a caller retrying the open call — it says so in as many words,
/// and it refuses only the sessions that hold rows — so an item for one would
/// be an item about no rows, closed by abandoning a session the owner never
/// meant to open.
///
/// **Not the session's state.** That is the gap below, and the two must not be
/// swapped: the state is what this item is waiting to change, and a condition
/// this item is waiting on cannot also be what makes it apply.
const fn import_session_eligibility(summary: &ImportSessionSummaryView) -> bool {
    summary.row_count > 0
}

fn import_session_gap(summary: &ImportSessionSummaryView) -> bool {
    !import_session_completion(summary)
}

/// The goal is that this session has stopped being open, and nothing else.
///
/// **Quantified over the session, not over its questions**, and that is the
/// opposite shape to [`classification_question_completion`] — which is
/// deliberately quantified over questions, because a new question of a session
/// whose others are answered is new work and a session-wide predicate would
/// have closed over it. Here the reverse holds. The item is about the session
/// being open; a question answered does not end it, a question raised does not
/// reopen it, and a completion asked of the questions would report an import
/// finished the moment the last one was answered — which is the exact reading
/// this item exists because a caller was entitled to make.
///
/// **Abandoning satisfies it.** «The owner committed the rows» and «the owner
/// threw them away» are different facts, and the question item is right to
/// refuse to read the second as the first: there, abandoning would stand in for
/// the owner saying what a row was. Here the item makes no claim about any row.
/// It says the session is open, and abandoning ends it as finally as committing
/// does — after which the rows were never facts and no report is short of them.
///
/// Read from the state and not from the closing timestamp, because the state is
/// what every other reader of a session checks and what the two routes write.
fn import_session_completion(summary: &ImportSessionSummaryView) -> bool {
    !matches!(summary.session.state, ImportSessionState::Open)
}

/// An import that was started, holds rows, and has not been ended.
///
/// **This is `iaam-8ano`.** The queue used to raise an item for such a session
/// only through its unanswered questions, so a session whose questions were all
/// answered — or that raised none, which is the ordinary outcome of a clean
/// statement — stood open holding rows and appeared nowhere. `GET /v1/actions`
/// is this system's published answer to «what next», and a caller that reads it
/// and finds nothing outstanding is entitled to conclude the import finished.
/// The rows were in no journal, so the next act was to import the same
/// statement again, and that is how a queue that is merely incomplete
/// manufactures duplicate work.
///
/// **Two resolutions, and they are the two ways a session ends.**
/// [`OperationKey::CommitImportSession`] and
/// [`OperationKey::AbandonImportSession`] say so themselves: «a refusal that
/// offers one without the other tells the owner he must finish an import he may
/// have decided against». Committing is first and abandoning second, never the
/// other way round, for the reason `half_imported_refusal` orders them so:
/// abandoning is the way out rather than the way on, and leading with «throw
/// this away» invites a caller to discard rows the owner spent an evening
/// answering questions about.
///
/// **Answering a question is not among them**, although the commit is refused
/// while one is open. A resolution is a call that closes *this* item, and an
/// answer leaves the session exactly as open as it found it. The unanswered
/// count is in the reason instead, beside the item that does close on an
/// answer.
///
/// **The assessment is named in prose and is not a target.** It is a GET with no
/// request body, so it is not an [`OperationKey`] and could not be one; the
/// session responses publish its address in their `assessment` field, which is
/// where `ImportSessionDto::assessment` explains this at length. The reason
/// names the field rather than spelling a path, so the queue does not become a
/// second place the route's address is written down.
///
/// `RequiredForGoal`, not `Blocking`, on the same reading
/// [`answer_classification_question_action`] is graded under. What an open
/// session prevents is one call: opening another session for the same declared
/// import, which is refused while this one holds rows. That refusal is not the
/// system declining to accept work — it is this defect's own remedy, the thing
/// that stops the same statement being imported twice — and a scope of one
/// piece of work the owner controls is not `Blocking`.
///
/// `NeedsOwnerInput`, not `Ready`. Both calls are ones an agent may transmit —
/// both keep [`Scope::Agent`] as their floor, which is what the item reads —
/// while whether these rows become facts or are thrown away is not a choice
/// anything but the owner may make on his behalf.
fn import_session_unfinished_action(
    summary: &ImportSessionSummaryView,
    account: Option<&AccountView>,
) -> Action {
    let session = summary.session.id;
    let mut preset = BTreeMap::new();
    // The path segment of both routes, and it is the only thing either needs
    // that the queue knows.
    preset.insert("session".to_owned(), session.inner().to_string().into());
    let commit = ResolutionOption {
        operation: OperationKey::CommitImportSession,
        request: RequestPlan {
            preset: preset.clone(),
            missing: vec![
                // Optional in the schema and published as missing anyway, on
                // the ground `start_account_import_action` publishes
                // `/source/label` on: `missing` states what the plan needs
                // supplied, and a commit sent without it writes whatever the
                // session holds now rather than what its caller read.
                //
                // `Caller` and not `Owner`: the value is the stamp the
                // assessment stamped on the reading this client fetched, so the
                // client fills it in from what it already knows about its own
                // run, without putting a question to the owner. Deciding to
                // commit is his; quoting the revision he decided over is not.
                MissingInput::plain("/revision", NobodyIsAsked::Caller),
            ],
        },
    };
    let abandon = ResolutionOption {
        operation: OperationKey::AbandonImportSession,
        request: RequestPlan {
            preset,
            missing: Vec::new(),
        },
    };

    let held = match summary.row_count {
        1 => "1 row, and it is in no journal and in no report".to_owned(),
        rows => format!("{rows} rows, and they are in no journal and in no report"),
    };
    let waiting = match summary.unanswered {
        0 => {
            "Every question it raised is answered, so it can be committed as it stands.".to_owned()
        }
        1 => "1 of its questions is still unanswered, and the commit is refused until it is \
              answered; the question is its own item in this queue."
            .to_owned(),
        open => format!(
            "{open} of its questions are still unanswered, and the commit is refused until \
             they are answered; each is its own item in this queue."
        ),
    };
    let whose = match account {
        Some(account) => format!(
            " It was declared for account {} ({}).",
            account.id.inner(),
            account.title
        ),
        // A session opened without a declaration holds rows for as many
        // accounts as its export covers, and naming one of them would name the
        // first row's rather than the session's.
        None => String::new(),
    };

    Action::new(
        ActionFacts {
            // One identity per session, so two half-finished imports are two
            // items and an agent deduplicating by id never collapses them.
            id: format!(
                "{}:{}",
                ActionKind::ImportSessionUnfinished.id(),
                session.inner()
            ),
            kind: ActionKind::ImportSessionUnfinished,
            category: ActionCategory::required_for(ActionKind::ImportSessionUnfinished),
            state: ActionState::NeedsOwnerInput,
            subject: account.map(|account| ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Import session {session} has been open since {opened} and holds {held}.{whose} \
             {waiting} Read what committing would record before recording it: every session \
             response carries the address of its assessment in `assessment`, and the revision \
             that answer stamps is what the commit is sent under. Abandoning ends the session \
             instead and writes nothing — the rows were never facts, so there is nothing to \
             retract — and it is how a statement imported by mistake is dropped. Until one of \
             the two is called this session stays open, and opening another for the same \
             declared import is refused rather than started alongside it.",
            session = session.inner(),
            opened = summary.session.opened_at,
        ),
        ActionTarget::from_options(vec![commit, abandon]),
    )
    .expect("import session action publishes both of the calls that end a session")
}

/// The `/answer` field, carrying the shapes **this** question admits.
///
/// The shapes are read from the question rather than listed here, and they are
/// recomputed from the typed question rather than replayed from the JSON the
/// store holds beside it. That JSON is what was offered when the question was
/// asked; the answering route checks the answer against `Question::alternatives`
/// as this build computes it, and the queue must offer what the route will
/// accept and not what an older build once printed.
///
/// Public, and taking the typed question rather than the queue's own
/// [`ClassificationQuestion`], because a refusal wants the same field. The
/// answering route rejects an answer this question does not admit, and the
/// commit route refuses a session that still holds one; both then publish this
/// field, and a second construction of it would eventually offer a shape the
/// route no longer accepts.
#[must_use]
pub fn answer_input(asked: &Question, accounts: &[AccountView]) -> MissingInput {
    let others = answer_account_candidates(asked, accounts);

    MissingInput {
        alternatives: asked
            .alternatives()
            .into_iter()
            .map(|shape| InputAlternative {
                value: shape.code().to_owned(),
                // Two of the shapes name an account, and only those two. The
                // alternative says so itself rather than the field being marked
                // required for all of them, which would refuse `paid` for want
                // of an account nothing asked it about.
                requires: if shape.needs_account() {
                    vec![RequiredInput {
                        pointer: OwnerPrompt::TransferFarSide.pointer().to_owned(),
                        provided_by: ProvidedBy::Owner,
                        prompt: Some(OwnerPrompt::TransferFarSide),
                        candidates: Some(others.clone()),
                    }]
                } else {
                    Vec::new()
                },
                // What answering this word does to the money-flow report. The
                // queue item's `reason` carries the question's prompt, which
                // says what the row leaves open; it does not say what each
                // answer decides, and an agent that reads only the queue would
                // otherwise be offering the owner seven words and no stakes.
                consequence: Some(shape.consequence().to_owned()),
            })
            .collect(),
        ..MissingInput::asked(OwnerPrompt::ImportAnswer)
    }
}

/// The accounts an answer to this question may name.
///
/// The far side of an internal transfer is one of the owner's **other**
/// accounts: the row is already on this one, and an account is not the other
/// side of itself.
///
/// Split out of [`answer_input`] so that the import question itself can publish
/// the same list the queue publishes for the same question. Two constructions of
/// "which accounts may this answer name" would eventually offer a caller an
/// account the answering route refuses, or withhold one it would have taken —
/// and the caller cannot check either, because it is being handed the list
/// precisely so that it need not fetch one.
#[must_use]
pub fn answer_account_candidates(
    asked: &Question,
    accounts: &[AccountView],
) -> Vec<AccountCandidate> {
    let others: Vec<AccountView> = accounts
        .iter()
        .filter(|candidate| candidate.id != asked.account())
        .cloned()
        .collect();
    account_candidates(&others)
}

fn actions_from_views(
    accounts: &[AccountView],
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
    transfers: &[AccountTransferStatementView],
    wanted: &[AccountNamedByDocument],
) -> Vec<Action> {
    let account_completion = account_completion(accounts);
    let contour_eligibility = !accounts.is_empty();
    let contour_completion = contour_completion(contours);
    let contour_gap = !contour_completion;
    let mut actions = Vec::with_capacity(2 + wanted.len());

    if !account_completion {
        actions.push(first_account_action());
    }
    // Straight after it, because the kind is declared straight after it and the
    // frontier's kinds must stay non-decreasing in that order. The two belong
    // together for a better reason than the ordering: they are the same act, and
    // the second one says which. An empty instance raises the first alone —
    // nothing has been read, so no name exists — and it stays raised beside
    // these until an account exists.
    //
    // The gap is decided before this function is reached, in `frontier`, where
    // the owner's directory is loaded: the eligibility of one of these is «a
    // document printed this string», which is what the slice holds, and the gap
    // is «and the directory still does not place it», which needs the tiering
    // and therefore the accounts in the shape the tiering searches.
    for name in wanted {
        // The whole slice, because an item has to be able to say which of its
        // neighbours one answer would settle with it, and that is a fact about
        // the set rather than about the name (`iaam-hdr7`).
        actions.push(account_named_by_document_action(name, wanted));
    }
    if contour_eligibility && contour_gap {
        actions.push(first_contour_action(accounts));
    }
    if account_scope_eligibility(contours) {
        for account in accounts
            .iter()
            .filter(|account| account_scope_gap(account.id, contours, exclusions))
        {
            actions.push(account_scope_action(account, accounts, contours));
        }
    }
    for account in accounts.iter().filter(|account| {
        transfer_relationships_eligibility(account.id, accounts, contours, exclusions)
            && transfer_relationships_gap(account.id, transfers)
    }) {
        actions.push(transfer_relationships_action(account, accounts));
    }
    actions
}

fn account_completion(accounts: &[AccountView]) -> bool {
    !accounts.is_empty()
}

/// Whether the owner has any contour at all.
///
/// The goal this satisfies is "a contour exists", and that is all it ever meant.
/// It is deliberately no longer asked about an individual account: the coverage
/// question is [`account_scope_completion`], and conflating the two is how "any
/// contour exists" came to stand in for "every account has been placed".
fn contour_completion(contours: &[ContourView]) -> bool {
    !contours.is_empty()
}

/// Where an account stands relative to the owner's reporting perimeter.
///
/// Three states, not two. "Every account must belong to a contour" is as wrong
/// as "any contour exists": an account may be outside the perimeter on purpose —
/// a counterparty's, a closed one, one the owner does not want reported — and a
/// queue that nags about it forever is a queue the owner learns to ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountScope {
    /// Named by the latest version of at least one contour.
    Inside,
    /// The owner has ruled it outside every contour, and said why.
    Outside,
    /// Neither. The state a newly created account is in.
    Undecided,
}

/// Read an account's disposition from the two places that can hold one.
///
/// `Inside` is derived from the contour composition rather than stored beside
/// the exclusions: membership is already a versioned fact of the contour, and a
/// second copy of it would be a second truth to keep in step. `Outside` cannot
/// be derived from anything — it is a statement, and it is not a statement any
/// single contour owns, which is why it is recorded per owner and account.
#[must_use]
pub fn account_scope(
    account: AccountId,
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
) -> AccountScope {
    if contours
        .iter()
        .any(|contour| contour.accounts.contains(&account))
    {
        return AccountScope::Inside;
    }
    if exclusions
        .iter()
        .any(|exclusion| exclusion.account == account)
    {
        return AccountScope::Outside;
    }
    AccountScope::Undecided
}

/// An account can be placed once there is a contour to place it in.
///
/// Without one, `first_contour_action` already asks the same question of every
/// account at once and offers every one of them as a candidate; raising a second
/// item per account beside it would say the same thing twice.
const fn account_scope_eligibility(contours: &[ContourView]) -> bool {
    !contours.is_empty()
}

fn account_scope_gap(
    account: AccountId,
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
) -> bool {
    !account_scope_completion(account, contours, exclusions)
}

/// The goal is satisfied by a decision, not by membership.
///
/// This is the property `!contours.is_empty()` could not have: it is asked of
/// each account, so a newly created account reopens it however many contours
/// already exist.
fn account_scope_completion(
    account: AccountId,
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
) -> bool {
    account_scope(account, contours, exclusions) != AccountScope::Undecided
}

/// The question exists once the owner has a second account to move money to,
/// and only for an account he has not ruled outside the perimeter.
///
/// With one account there is no «other side»: an internal transfer needs two,
/// and asking about a relationship that cannot exist is the kind of item a
/// queue is learned to ignore for.
///
/// The scope condition is the same argument made twice over. A transfer has two
/// ends and at least one of them is inside the perimeter — a pair of accounts
/// both outside it appears in no contour's report, so relating them changes no
/// reported number — and the inside end is asked which of the owner's accounts
/// money moves between it and, with every other account offered as a candidate,
/// the outside one included. Asking the outside account the same thing a second
/// time therefore discovers nothing the first question could not, and it costs
/// the owner a `RequiredForGoal` item about an account he has already ruled out
/// of every report, with a reason. Two institutions imported at once produced
/// one such question per account and no way to tell which were obligatory.
///
/// [`AccountScope::Undecided`] is *not* suppressed. The owner has not ruled on
/// those accounts, and silencing a question because an earlier question is
/// unanswered is a queue that goes quiet exactly when it should not.
///
/// Nothing is lost for good either way: the scope is read from the contours and
/// the exclusions on every call rather than recorded beside the statement, so
/// bringing an account back inside a contour reopens the question.
fn transfer_relationships_eligibility(
    account: AccountId,
    accounts: &[AccountView],
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
) -> bool {
    accounts.len() > 1 && account_scope(account, contours, exclusions) != AccountScope::Outside
}

fn transfer_relationships_gap(
    account: AccountId,
    statements: &[AccountTransferStatementView],
) -> bool {
    !transfer_relationships_completion(account, statements)
}

/// The goal is satisfied by a statement, not by a partner.
///
/// The same property [`account_scope_completion`] has, and for the same reason:
/// it is asked of each account, so a newly created account reopens it however
/// many statements already exist. An account named as somebody else's partner
/// does **not** satisfy it — being on the far side of one relationship says
/// nothing about the ones this account is the near side of — and a statement
/// naming no partners does, because «none of my others» is an answer.
fn transfer_relationships_completion(
    account: AccountId,
    statements: &[AccountTransferStatementView],
) -> bool {
    statements
        .iter()
        .any(|statement| statement.account == account)
}

/// Which of the owner's accounts are the two sides of one internal movement.
///
/// The discovery item. It is asked **before** anything is imported, because the
/// order the reporter had to invent is the failure this exists to remove: one
/// economic transfer between two banks is printed twice, once by each side, and
/// nothing in either row says the two are one movement. Discovering that after
/// the import means reclassifying rows already recorded; discovering it before
/// means the import knows what it is looking at.
///
/// `RequiredForGoal`, not `Recommended`. An unrelated pair of legs makes the
/// report count an outflow and an inflow that never crossed the perimeter — for
/// a contour spanning both institutions, wrong twice over — so the absence
/// changes what the reported numbers mean, which is the line the control
/// assertions were promoted on.
///
/// `NeedsOwnerInput`, and the candidates are proposed rather than chosen. The
/// system may not decide this: a relationship it inferred from two amounts that
/// happen to match would be a fabricated fact about the owner's money, in
/// exactly the way an inferred contour would be. So every other account is
/// offered, with the institution that holds it, and the statement is his.
fn transfer_relationships_action(account: &AccountView, accounts: &[AccountView]) -> Action {
    let mut preset = BTreeMap::new();
    preset.insert("account".to_owned(), account.id.inner().to_string().into());

    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}",
                ActionKind::ResolveTransferRelationships.id(),
                account.id.inner()
            ),
            kind: ActionKind::ResolveTransferRelationships,
            category: ActionCategory::required_for(ActionKind::ResolveTransferRelationships),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) has no statement of which of your accounts money moves between it \
             and. One transfer between two institutions is printed twice, once by each side, and \
             nothing in the rows relates them; until you say which accounts are the two sides, \
             each leg is counted as money crossing the perimeter. Name the accounts, or record \
             that none of your others is on the other side.",
            account.id.inner(),
            account.title
        ),
        ActionTarget::Operation {
            operation: OperationKey::RecordAccountTransferPartners,
            request: RequestPlan {
                preset,
                // The owner's other accounts, and only those: this statement is
                // about two accounts of his own, and a counterparty who is not
                // him is the classification rules' question.
                missing: vec![MissingInput::asked_from(
                    OwnerPrompt::TransferPartners,
                    account_candidates(
                        &accounts
                            .iter()
                            .filter(|candidate| candidate.id != account.id)
                            .cloned()
                            .collect::<Vec<_>>(),
                    ),
                )],
            },
        },
    )
    .expect("transfer relationships action has an operation target")
}

/// Use the inclusive first and last business effective dates: they are the
/// only period bounds justified by the persisted state, not a calendar default.
fn activity_period(activity: &AccountActivityView) -> Option<AssertionPeriod> {
    AssertionPeriod::between(
        activity.first_effective_date?,
        activity.last_effective_date?,
    )
}

/// Both ways into an account that holds nothing, and the one step that is not
/// a route.
///
/// `NeedsOwnerInput`, not `Blocked`, and `undecomposed_outflows_action` is the
/// precedent — the same substitution, made for the same reason. There, `Blocked`
/// had been earned by the sentence «no *report* operation can provide a rule»,
/// which was true and irrelevant: the state means «no operation in this API is
/// available for this item», the rule route was in this same API, and the
/// action catalogue resolves a target against the whole completed contract and
/// not a caller-local namespace. Here the narrower true sentence was «no
/// operation in this API fetches the document», and it bought the same wrong
/// state. Two operations begin an import: `POST /v1/import-sessions` opens the
/// session a statement's rows are fed into, and `POST /v1/brokers/{broker}/sync`
/// is the second remedy entire.
///
/// The cost was not theoretical. An agent walking the queue reads `state` as its
/// map of what it may call; told that nothing here helps with the single most
/// important next step, it stopped. A reviewer who reached the session routes by
/// reading the OpenAPI document instead ran a whole import that the queue had
/// disowned.
///
/// The sentence about the document stays in the reason, because it is the honest
/// half and it is the half the state was never making: getting the statement out
/// of the bank is a step outside this API, and a step outside this API is not a
/// missing route. So the reason now says both — what the owner must do himself,
/// and what to call once he has done it.
///
/// Four options, ordered and not ranked, which is what `account_scope_action`
/// established `Options` for: a statement is the ordinary answer and a broker
/// channel is the answer for the accounts that have one, and publishing either
/// alone would leave the other reachable only by reading the specification.
///
/// **The order is the promise, and the first entry is the call the caller can
/// make now.** `iaam-bhu3` is what happens when it is not: a register offered a
/// state's remedies with the ordinary one absent, and the mapping chose from a
/// list that did not contain the answer. Here the ordinary answer is *two*
/// calls — open a session, then put the statement into it — and the session did
/// not exist a moment ago, so opening it stays first and the two ways of
/// putting a statement in follow it. The broker sync is last because it is the
/// answer for the accounts that have a channel, and it is a remedy entire.
///
/// **A two-step act is said with the mechanism that already exists for a value
/// the caller does not hold** (`iaam-j5oz`). `POST
/// /v1/import-sessions/{session}/document` takes the session in its path, and a
/// resolution that named it while saying nothing about where the session comes
/// from would be a call the caller cannot make — worse than one that does not
/// exist, because a client reads `target` as the contract. So the session is a
/// [`MissingInput`] marked [`ProvidedBy::Caller`], exactly as `/broker` is a
/// path segment marked [`ProvidedBy::Owner`] on the option below it: the queue
/// already says «this field is yours to fill in», and which call the value came
/// out of is what the reason is for.
///
/// What was rejected: a fourth `ProvidedBy` word for «the call before this one»,
/// which would make one field answer two questions and is the same mistake
/// `iaam-tt71` declined; a fifth [`ActionTarget`] variant for an ordered
/// sequence, which is a fourth mechanism for a fact three of them already
/// carry; and leaving the document channel unpublished and describing it in
/// prose, which is `iaam-1tij` again, one level down.
///
/// **Both ways of putting a statement in are offered, and neither is the only
/// one.** `read_import_document` conveys the institution's own file and a
/// reviewed profile reads it; `add_import_rows` takes rows already written in
/// this API's words, which is what a caller holds when the owner pasted them or
/// ran his own converter. A queue that named only the first would tell such a
/// caller to obtain a profile it cannot install, and one that named only the
/// second would hide the ordinary way a cash statement arrives.
///
/// The item reads `agent`, for the reason `answer_classification_question_action`
/// gives: all four routes keep [`Scope::Agent`] as their floor, and an item
/// marked `owner` would tell an agent it may not send a request the server would
/// accept. Every option reads `agent`, which is why this item hid `iaam-woeh`
/// rather than exposing it — its resolutions happen to agree.
///
/// **The reason names the shape a row is submitted in, and that closes
/// `iaam-tt71`.** «Feed it the rows» presupposed something that turns a
/// statement into rows this API accepts, and named nobody: for the owner running
/// his own converter the presupposition held, and for an agent holding rows he
/// pasted it held only through the observation shape, which the item never
/// mentioned. So an agent that read this item and knew only the conclusive kinds
/// had to conclude — from a document it is not allowed to open — or stop.
///
/// The fix is a sentence rather than a fourth `ProvidedBy` word, and the
/// argument for that is on [`ProvidedBy`] itself: the case for the word rested
/// on a parity defect that `iaam-7l7v` removed, and the rows are not a field of
/// this request in the first place. Naming the shape is not naming the tool,
/// which `docs/import-boundary.md` §8 rejects and still should:
/// `unresolved_direction` is a value of this API's own contract, published in
/// the document the same caller is already reading, and the queue is entitled to
/// say what its own calls accept.
fn start_account_import_action(account: &AccountView) -> Action {
    // The session's `source` is what names the account these rows belong to, and
    // it is the whole of what the policy knows here: the account is the subject
    // of this item. Preset as the object the pointers below write into, so a
    // caller merges rather than reassembles.
    let mut session_preset = BTreeMap::new();
    session_preset.insert(
        "source".to_owned(),
        serde_json::json!({ "account": account.id.inner().to_string() }),
    );

    let session = ResolutionOption {
        operation: OperationKey::OpenImportSession,
        request: RequestPlan {
            preset: session_preset,
            missing: vec![
                // How the rows arrived — `file`, `paste`, `manual` — is a fact
                // about the transmission and not about the owner's money, so it
                // is the caller's to state. No alternatives: the route takes any
                // short name, and publishing three would claim a closed set the
                // server does not enforce.
                MissingInput::plain("/source/channel", NobodyIsAsked::Caller),
                // The label is what makes this import retractable on its own,
                // and it is a statement period or an export file name — it is
                // read off the document the owner fetched, which is why this is
                // the first field marked `ExternalDocument`. Optional in the
                // schema and published as missing anyway, on the same ground as
                // `/cash` in the control assertion: `missing` states what the
                // plan needs supplied, and a plan that quietly omitted it would
                // produce unlabelled rows retractable only together with every
                // other unlabelled row of the same account and channel.
                MissingInput::plain("/source/label", NobodyIsAsked::ExternalDocument),
            ],
        },
    };

    // The second step of the ordinary answer, and the first one that puts
    // anything in the session. Nothing is preset: the route reads the account
    // off the session opened above, and which profile reads the document is
    // this instance's to decide — naming one here would have the queue choose
    // a reader for a file it has never seen.
    let document = ResolutionOption {
        operation: OperationKey::ReadImportDocument,
        request: RequestPlan {
            preset: BTreeMap::new(),
            missing: vec![
                // The identifier the resolution above returns. `Caller`, by the
                // word's own meaning: it is one of the client's own identifiers,
                // held from the call it just made, and asking the owner for it
                // would be asking him to read back something he never saw.
                //
                // The document itself is deliberately **not** listed beside it.
                // It is the request body entire and not a field of one, and
                // `ProvidedBy` already settles what that means: a pointer into
                // a body that has no fields could not be satisfied by filling
                // anything in. The route's own description says what the body
                // is, and this item's reason says who fetches it.
                MissingInput::plain("/session", NobodyIsAsked::Caller),
            ],
        },
    };

    // The other way into the same session, for a caller that already holds the
    // rows in this API's own words rather than the institution's file.
    let rows = ResolutionOption {
        operation: OperationKey::AddImportRows,
        request: RequestPlan {
            preset: BTreeMap::new(),
            missing: vec![
                MissingInput::plain("/session", NobodyIsAsked::Caller),
                // `ExternalDocument` and not a fourth word for whatever produced
                // them: the axis is who holds the value, and the statement holds
                // it however much converting it took to type. That is the
                // sentence on [`ProvidedBy`], and this is the request it was
                // waiting for — the rows are a field of *this* call, so the
                // queue can now point at them instead of describing them.
                MissingInput::plain("/operations", NobodyIsAsked::ExternalDocument),
            ],
        },
    };

    // The broker sync knows the account and nothing else. `broker` is a path
    // segment and is not preset: which channel this account is held at is the
    // owner's to name, and the queue cannot read it off an account that has no
    // facts. The interval is his too — with no business fact there is no first
    // or last effective date to propose one from, and a window invented here
    // would decide how much of his history gets imported.
    let mut sync_preset = BTreeMap::new();
    sync_preset.insert("account".to_owned(), account.id.inner().to_string().into());
    let sync = ResolutionOption {
        operation: OperationKey::SyncBroker,
        request: RequestPlan {
            preset: sync_preset,
            missing: vec![
                MissingInput::asked(OwnerPrompt::BrokerChannel),
                MissingInput::asked(OwnerPrompt::SyncFrom),
                MissingInput::asked(OwnerPrompt::SyncTo),
            ],
        },
    };

    Action::new(
        ActionFacts {
            // Scoped to the account: this action is emitted once per account with
            // no facts, and an unscoped id would give every one of them the same
            // identity — which is what an agent deduplicates by.
            id: format!(
                "{}:{}",
                ActionKind::StartAccountImport.id(),
                account.id.inner()
            ),
            kind: ActionKind::StartAccountImport,
            category: ActionCategory::required_for(ActionKind::StartAccountImport),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) has no business facts; import a statement or connect a broker. \
             Fetching the statement out of the bank is a step outside this API — no \
             operation here downloads the document, and the owner obtains it himself. \
             Recording it is not, and it is two calls rather than one: open an import \
             session for this account, then put the statement into the session that call \
             returns. The session's identifier is what the second call takes in its path, \
             which is why it is published as a field to fill in rather than as one already \
             known — nothing here can know it before the first call is made. There are two \
             ways to put the statement in and they end in the same session: send the \
             institution's own export as it prints it and a source profile reads it, or \
             send the rows in this API's own shape when that is what you hold. Deciding \
             what a row was is not a step between those two — a row whose direction or \
             nature the reader cannot tell is sent \
             as `unresolved_direction`, carrying the source's own sign, its direction \
             word and the party it named, and the session settles it against the owner's \
             accounts and rules or asks him about it. Then read the assessment the \
             session publishes to see what committing would record and what it would \
             not, and commit under the revision that assessment carries; or synchronise \
             a broker channel over an interval. An import already under way is its own \
             item in this queue — a session holding rows that has not been committed or \
             abandoned is published as `import_session_unfinished` — and opening one \
             again finds it too: the call refuses, names the session, and publishes the \
             calls that end it. Import is continuous and never complete.",
            account.id.inner(),
            account.title
        ),
        ActionTarget::from_options(vec![session, document, rows, sync]),
    )
    .expect("account import action publishes every one of its resolutions")
}

/// The request for one control assertion, at the point it is wanted for.
///
/// Parameterised by the point rather than split into a second `ActionKind`: the
/// kind names the work — obtain a control assertion from a document and record
/// it — and that work is the same at either end of the interval. The same
/// operation, the same preset fields, the same missing `/cash`, the same
/// category and scope; a second kind would duplicate all of it and oblige every
/// consumer that switches on the kind to learn a second name for one job.
///
/// The point is not lost by that choice: it already sits in the action's id,
/// between the interval and the dimension, so an opening request and a closing
/// request for the same account and interval are two identities and an agent
/// deduplicating by id never collapses them into one.
fn provide_control_assertion_action(
    account: &AccountView,
    period: AssertionPeriod,
    point: BalancePoint,
) -> Action {
    let dimension = Dimension::Cash;
    let mut preset = BTreeMap::new();
    preset.insert("account".to_owned(), account.id.inner().to_string().into());
    preset.insert("from".to_owned(), period.from.to_string().into());
    preset.insert("to".to_owned(), period.to.to_string().into());
    preset.insert("at".to_owned(), point.code().into());
    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}:{}:{}:{}:{}",
                ActionKind::ProvideControlAssertion.id(),
                account.id.inner(),
                period.from,
                period.to,
                point.code(),
                dimension.code()
            ),
            kind: ActionKind::ProvideControlAssertion,
            // Required for a goal, at either point, not `Recommended`.
            //
            // Without the opening assertion the cash figure is a movement over
            // the imported interval and not a balance at all, so the assertion
            // is not work that "improves quality but is not required" — it is
            // what makes the number mean anything. Without the closing one the
            // interval has nothing to reconcile against and its dimensions stay
            // provisional; `IndependentConfirmationMissing` already grades the
            // absence of confirmation as required, and grading the assertion
            // that produces it as optional would contradict that. So neither
            // point is recommended-only, and the queue stops telling the owner
            // that the one thing which would make his numbers trustworthy is
            // his to skip.
            //
            // Which goals, at either point, is `ActionKind::goals`: the snapshot
            // and the reconciliation. Both requests are the same operation with
            // the same fields, so a per-point set would be a second table saying
            // the same thing.
            category: ActionCategory::required_for(ActionKind::ProvideControlAssertion),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        match point {
            BalancePoint::Opening => format!(
                "Account {} ({}) has business facts from {} through {}; record its opening cash \
                 balance. Until it is recorded, the cash figure for this account is a sum \
                 accumulated from an unasserted start, and a closing balance compared against it \
                 reports the missing opening balance as a discrepancy.",
                account.id.inner(),
                account.title,
                period.from,
                period.to
            ),
            BalancePoint::Closing => format!(
                "Account {} ({}) has business facts from {} through {}; record its closing cash \
                 balance. An assertion is evidence to reconcile, not proof of a match; a \
                 discrepancy may remain.",
                account.id.inner(),
                account.title,
                period.from,
                period.to
            ),
        },
        ActionTarget::Operation {
            operation: OperationKey::RecordOwnerBalance,
            request: RequestPlan {
                // `/cash` is the one chosen input, so the request cannot be empty:
                // the scenario rejects a balance carrying neither cash nor positions.
                preset,
                missing: vec![MissingInput::asked(OwnerPrompt::OwnerBalanceCash)],
            },
        },
    )
    .expect("control assertion action has an operation target")
}

/// One product the owner has retired, and whether the journal agrees.
///
/// A derived view rather than a store projection: `emptied` is a fold over the
/// owner's effective journal, and it is computed in [`frontier`] so that
/// [`actions_from_state`] stays a pure function of what it is handed — the same
/// arrangement as [`ClassificationQuestion`], whose generalisation is derived
/// from the session's observations before the policy sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredProduct {
    pub account: AccountId,
    /// The date the owner said the product ceased.
    pub effective_on: Date,
    /// Every cash and position figure the journal now holds on this account is
    /// zero.
    ///
    /// Read from the journal and never from the declaration: see
    /// [`retired_account_completion`].
    pub emptied: bool,
}

/// A retirement the owner has not withdrawn is eligible to be asked about.
///
/// Kept as a named function beside the gap and the completion, as the three
/// other goals in this module are. The eligibility is not vacuous: only
/// retirements **in force** reach here, because
/// [`crate::ports::AccountRetirementsView`] holds only those — an account whose
/// latest statement withdrew its retirement is absent from it, exactly as one
/// never retired is. So withdrawing the statement removes the item by way of
/// this predicate, and the item is right to disappear: there is no longer a
/// declaration for the journal to disagree with.
const fn retired_account_eligibility(_retired: &RetiredProduct) -> bool {
    true
}

fn retired_account_gap(retired: &RetiredProduct) -> bool {
    !retired_account_completion(retired)
}

/// The goal: the product the owner says has ceased holds nothing.
///
/// **Read from the journal, not from the caveat and not from the declaration.**
/// This is `iaam-4hcy`'s rule applied to a second state. The declaration cannot
/// close the item — recording it is what raised the item — and the asset
/// snapshot's caveat cannot either, because a caveat is a per-report statement
/// at one `as_of` and the queue has no `as_of` to take. What has an answer is
/// the account's own figures: **whoever brought them to zero and however**, the
/// disagreement between his statement and the journal is over. A reconstructed
/// opening recorded through the ingest route closes it; so does retracting an
/// event that should never have counted; so does an ordinary import that
/// happened to bring in the missing outflow, with no item consulted at all.
///
/// The fold is the whole journal rather than a window, and the honesty of that
/// is worth stating: this asks «does the account hold anything now», while the
/// snapshot asks «did it hold anything on the day this report is taken». A
/// snapshot taken at a date the fold had not yet reached zero still carries its
/// caveat, and the queue does not repeat itself per date because a queue with
/// one item per possible report date is not a list anyone reads.
fn retired_account_completion(retired: &RetiredProduct) -> bool {
    retired.emptied
}

/// A retirement the journal has not caught up with (`iaam-xnhu`).
///
/// **The state was reported truthfully and its acts were published where a
/// caller does not look for acts.** `retired_account_not_empty` lives in a
/// report's `confidence`; this endpoint is the system's answer to "what should I
/// do next", and it said nothing — so the owner learned that his retirement had
/// not taken effect only by asking for the snapshot again and reading the
/// register. That is the shape `iaam-4hcy` solved once already, and this is the
/// same fix: the act belongs in the queue, and its completion is read from the
/// world.
///
/// **Three ways out, in the register's order and for the register's reasons.**
/// The queue and `CaveatKind::closed_by` name one set of calls because they are
/// one vocabulary; what the queue adds is the request each of them wants. The
/// ordinary answer comes first — the journal is short of the opening the
/// movements were measured from — then the correction for a journal that is
/// wrong, then the withdrawal for a statement that is.
///
/// **`NeedsOwnerInput`, and the missing field is why.** The amount of a
/// reconstructed opening is what the account held before this system knew
/// anything about it. Nothing here holds that figure, nothing can derive it, and
/// presetting a guess would put an invented number into the one call whose whole
/// purpose is to state a real one. So it is published as missing, marked
/// [`ProvidedBy::Owner`] — the word for «a figure that exists nowhere else, and
/// no document and no client can supply it on his behalf». That word is right
/// even where he happens to read the number off an old statement, and
/// [`ProvidedBy`] says why: the axis is who holds the value, not what it cost to
/// get. The precedent is one type up — `provide_control_assertion_action` marks
/// `/cash` the same way, and a control assertion's figure is commonly printed on
/// a document too.
///
/// **Three ways out, and three floors — this is the item `iaam-woeh` was filed
/// on.** `ingest_operations` keeps [`Scope::Agent`]; `submit_corrections` and
/// `record_account_retirement` keep [`Scope::Owner`]. Each resolution publishes
/// its own floor, so a client choosing among the three is told which of them its
/// token reaches. The item's own `required_scope` is now the narrowest of the
/// three — `agent` — and that is a change from what it used to say. It used to
/// say `owner`, on the argument that an agent could not close the item alone;
/// that argument was about the *figure*, not about the *call*, and the figure is
/// already published as `/operations/0/amount` marked [`ProvidedBy::Owner`]. An
/// agent reading the old grading dropped the item entirely and never reached the
/// route it could in fact call.
fn retired_account_action(account: &AccountView, retired: &RetiredProduct) -> Action {
    // The reconstructed opening. Preset is exactly what the policy knows: the
    // account this row is on, and that the row is an opening. The figure, the
    // currency and the date the opening speaks about are the owner's. The date
    // is asked for rather than derived from the retirement: a reconstruction
    // states what was there before *itself*, not before the journal —
    // `iaam_core::reconciliation::OpeningAnchors` compares it against the
    // account's first movement — so a date invented here could anchor nothing
    // and still look like an answer.
    let mut opening_preset = BTreeMap::new();
    opening_preset.insert(
        "operations".to_owned(),
        serde_json::json!([{
            "account": account.id.inner().to_string(),
            "type": "opening_cash",
        }]),
    );
    let opening = ResolutionOption {
        operation: OperationKey::SubmitOperations,
        request: RequestPlan {
            preset: opening_preset,
            missing: vec![
                MissingInput::asked(OwnerPrompt::OpeningAmount),
                MissingInput::asked(OwnerPrompt::OpeningCurrency),
                MissingInput::asked(OwnerPrompt::OpeningDate),
                // A name for the fact so that sending it twice records it once.
                // The caller's, not the owner's: it says which transmission this
                // is and asks him nothing.
                MissingInput::plain("/operations/0/idempotency_key", NobodyIsAsked::Caller),
            ],
        },
    };

    // Ruling on the journal. Nothing is preset, and that is not an omission: a
    // correction is addressed to an event the caller names, and which of this
    // account's events should stop counting is exactly the judgement the item
    // cannot make for him.
    let correction = ResolutionOption {
        operation: OperationKey::SubmitCorrections,
        request: RequestPlan {
            preset: BTreeMap::new(),
            missing: vec![
                MissingInput::asked(OwnerPrompt::Corrections),
                MissingInput::asked(OwnerPrompt::AcknowledgeRetraction),
            ],
        },
    };

    // Withdrawing the statement. Fully written out, and the state is preset
    // rather than asked for: recording a second retirement over one that stands
    // is refused, so `in_use` is the only thing this option can mean, and an
    // option that left the word to the caller would publish the route that
    // produced the caveat as the way out of it.
    let mut withdrawal_preset = BTreeMap::new();
    withdrawal_preset.insert("id".to_owned(), account.id.inner().to_string().into());
    withdrawal_preset.insert("state".to_owned(), "in_use".into());
    let withdrawal = ResolutionOption {
        operation: OperationKey::RecordAccountRetirement,
        request: RequestPlan {
            preset: withdrawal_preset,
            missing: Vec::new(),
        },
    };

    Action::new(
        ActionFacts {
            // One identity per account, as every other per-account item has: two
            // retired products that both still hold something are two items, and
            // an agent deduplicating by id must not collapse them.
            id: format!(
                "{}:{}",
                ActionKind::RetiredAccountNotEmpty.id(),
                account.id.inner()
            ),
            kind: ActionKind::RetiredAccountNotEmpty,
            category: ActionCategory::required_for(ActionKind::RetiredAccountNotEmpty),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) is recorded as having ceased on {}, and the journal still shows \
             a figure on it, so the asset snapshot keeps its row and its class membership. \
             A retirement never hides money: the row is dropped only where every one of its \
             figures is zero. The usual cause is that the product's opening predates the \
             months that were imported, so the recorded movements do not sum to zero — \
             record the reconstructed opening and the retirement then removes the row on its \
             own. If instead a fact on this account should never have counted, rule on that \
             event. If the product had not in fact ceased on that date, withdraw the \
             statement. Do not rule the account outside the perimeter to tidy this up: that \
             is the other axis, and it takes the interest and the closing movement with it.",
            account.id.inner(),
            account.title,
            retired.effective_on,
        ),
        ActionTarget::from_options(vec![opening, correction, withdrawal]),
    )
    .expect("the retired-account item publishes three resolutions")
}

/// A retirement the queue could not check, because the journal would not fold
/// (`iaam-4jso`).
///
/// **It exists so that the failure is an item and not a silence.** The fold that
/// decides whether a retired product still holds a figure is the most expensive
/// thing [`frontier`] does, and it can refuse: a correction graph that will not
/// resolve, or an event the balance projection cannot apply. Refusing the whole
/// queue on that was deliberate and it was wrong in one respect that matters
/// more than the rest — [`standing_rules`] states, two functions up, that this
/// is the surface the owner recovers *from*, and an owner with no queue has
/// nowhere to be told what to do about the journal that took it away.
///
/// **And it exists so that the failure is not guessed away.** The alternative to
/// failing was to drop the retirement item, and that is worse than a loud
/// refusal: the caller reads «nothing outstanding» about a question nobody
/// could answer. `iaam-y1dp` records the same fork for the rules port. So the
/// item says what happened, in as many words, and carries the refusal the fold
/// gave.
///
/// **One item for the owner, not one per retired account.** The fold is over the
/// whole journal and it either produced verdicts for every declaration or for
/// none; an item per account would be one fact repeated, and each copy would
/// name an account that is not what refused. It carries no [`ActionSubject`]
/// for the same reason: the subject is the journal, and the vocabulary has no
/// word for that — inventing one for a state this narrow would be a worse trade
/// than the absence, which already reads as «this is not about one account».
///
/// **`submit_corrections`, and it is the honest remedy for both refusals.** A
/// correction is the only write in this system that changes what an existing
/// fold sees: retracting an event removes it from the effective set, and
/// superseding one replaces it. So it repairs a correction graph that will not
/// resolve — the offending correction is itself retractable — and it repairs an
/// event the projection cannot apply. Nothing is preset, and that is not an
/// omission: which fact should stop counting is the judgement this item cannot
/// make for him, exactly as `retired_account_action` says about the same call.
///
/// **`NeedsOwnerInput` and not `Blocked`.** `Blocked` means no operation in this
/// API is available, and one is. The item's floor is therefore `owner`, read off
/// the route like every other.
fn retirement_not_assessed_action(refusal: &str) -> Action {
    Action::new(
        ActionFacts {
            // Existential in the journal: one unfoldable journal, one item.
            id: identity(ActionKind::RetirementNotAssessed),
            kind: ActionKind::RetirementNotAssessed,
            category: ActionCategory::required_for(ActionKind::RetirementNotAssessed),
            state: ActionState::NeedsOwnerInput,
            subject: None,
        },
        format!(
            "A retirement stands and the effective journal will not fold, so whether the \
             account it names still shows a figure could not be worked out. What refused: \
             {refusal}. The asset snapshot is folded from the same events and refuses for the \
             same reason, so this queue has not gone quiet while the reports work. Until the \
             journal folds, nothing here can say whether the retirement took effect, and \
             nothing here is saying that it did. Rule on the fact that will not fold — retract \
             it, or supersede it with what should have stood — and the question is asked again \
             on the next reading."
        ),
        ActionTarget::Operation {
            operation: OperationKey::SubmitCorrections,
            request: RequestPlan {
                preset: BTreeMap::new(),
                missing: vec![
                    MissingInput::asked(OwnerPrompt::Corrections),
                    MissingInput::asked(OwnerPrompt::AcknowledgeRetraction),
                ],
            },
        },
    )
    .expect("the unassessed-retirement item names the correction route")
}

/// The item for an empty directory, and the one sentence it has to add.
///
/// **It cannot say which account to create, and that is a property of the state
/// rather than a defect of the item.** Nothing has been read; there is no
/// document, no statement and no name, so an item naming one would be inventing
/// it. What the item can do — and did not, which is what sent a live agent round
/// a loop — is say that the question has an answer and name the call that gives
/// it: a statement names its accounts in its own words, and reading one into a
/// session publishes those words back, once each, with the number of records
/// each accounts for.
///
/// That escape works today and always did. Its cost was that it had to be found:
/// the only way to learn the names was to provoke a refusal per record and mine
/// the response for them, and the response repeats one sentence per record.
/// Saying it here costs a paragraph; the alternative cost a reader ninety
/// kilobytes for seven names.
///
/// The target is unchanged and stays one operation. The document channel is not
/// an [`OperationKey`] (`iaam-1tij`), so it cannot be published as a resolution
/// beside this one; naming it in the reason is what this item can honestly do
/// until that bead lands, and it is deliberately not spelled as a route — the
/// queue names calls, and a path typed into prose is a second route table.
fn first_account_action() -> Action {
    Action::new(
        ActionFacts {
            id: identity(ActionKind::CreateFirstAccount),
            kind: ActionKind::CreateFirstAccount,
            category: ActionCategory::Blocking,
            state: ActionState::NeedsOwnerInput,
            // Existential: no account exists, so the item names none.
            subject: None,
        },
        "No account exists; create one before portfolio actions can be offered. Which accounts to \
         create is a question this instance answers rather than guesses at, and it does not have to \
         be guessed at here: a statement names its accounts in its own words, so open an import \
         session — no account has to be declared for one — and hand it the document. Every record \
         naming an account this directory does not hold is refused, and the response summarises those \
         refusals as the distinct account names the document asked for, in the order it printed them, \
         with the number of records each accounts for. Create an account for each, giving it the \
         printed string as the identifier its source prints for it, and read the same document again: \
         the row keys are over the document and the line, so nothing is imported twice. Once a \
         document has been read, the accounts it asked for are published in this queue by name and \
         this item does not have to be read for them.",
        ActionTarget::Operation {
            operation: OperationKey::CreateAccount,
            request: RequestPlan {
                preset: BTreeMap::new(),
                missing: vec![MissingInput::asked(OwnerPrompt::AccountTitle { printed: None })],
            },
        },
    )
    .expect("first account action has an operation target")
}

/// One account a kept document asked for, folded over every reading the instance
/// holds.
///
/// The printed string is the whole subject. There is no [`AccountId`] here and
/// there cannot be: the account does not exist, which is the item's whole point,
/// and minting an identifier for it would be this queue creating the thing it is
/// asking the owner to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountNamedByDocument {
    /// The cell as a document printed it.
    pub printed: String,
    /// Records that printed it, summed over the kept documents that did.
    ///
    /// Arithmetic over readings and nothing more. Not movements: every one of
    /// those records was refused, so nothing was read out of any of them.
    pub records: u32,
    /// How many kept documents printed it.
    pub documents: u32,
    /// The institution whose documents printed it, as their profile declares it.
    ///
    /// **Not a guess and not a proposal**: it is what the profile that read the
    /// document says it read, recorded beside the bytes when the document was
    /// kept and joined back here (`iaam-9i83`). It is what the item mints
    /// `provider` from, which is the only reason it is carried.
    ///
    /// `None` where this instance no longer keeps the document the reading was
    /// of. It is not reachable through any route — a kept document is immutable
    /// and is written before the names are — and it is still a state rather than
    /// an impossibility, because the item that would be built from it can say
    /// nothing about which source prints the string and must therefore not
    /// preset an identity scoped to one.
    pub issuer: Option<String>,
    /// The owner's own sentence, where he has said this is no account of his.
    ///
    /// **It does not decide whether the item is raised** (`iaam-mk1n`). That is
    /// asked of the directory, here as before, so an account answering to the
    /// string removes the item whether or not a statement stands. What this
    /// changes is what the item *says* and what it offers: work the owner has
    /// not done becomes a fact he settled, and the way out becomes the
    /// withdrawal instead of the account.
    pub declined: Option<String>,
}

/// The identity of the item raised for one printed name.
///
/// Scoped to the name **and** to the institution that printed it: several may
/// stand at once, and an unscoped id would give every one of them the identity
/// an agent deduplicates by, which would publish one item for seven accounts.
/// The institution is in it because it is in the fold key, and two items sharing
/// an id would be worse than the two items being separate.
///
/// Lifted out of the item so that a proposal can name its neighbours
/// (`iaam-hdr7`): a set that named items by rebuilding their ids a second way
/// would eventually name items that do not exist.
fn named_by_document_id(wanted: &AccountNamedByDocument) -> String {
    format!(
        "{}:{}:{}",
        ActionKind::CreateAccountNamedByDocument.id(),
        wanted.issuer.as_deref().unwrap_or_default(),
        wanted.printed
    )
}

/// The items one answer about a printed name would fill together, and the
/// institution that is the ground for it.
///
/// **The set is the reading's institution, and the alternatives were weighed**
/// (`iaam-hdr7`). Not the document: the fold above already merges two statements
/// of one bank naming one unknown account into one item, so a set keyed on the
/// reading would be a set an item belongs to twice. Not the item kind either: an
/// owner who conveys two institutions' statements before working his queue would
/// then be asked one question over both, and «they are all from that bank» would
/// be false of half of it. What every member of this set has in common is the
/// claim the proposal makes — these strings were printed by that institution —
/// which is a fact the reading recorded and nobody has to guess at.
///
/// **A declined name is not a member, and it is not an exclusion.** It asks for
/// neither field: the owner has said it is no account of his, so its item offers
/// the withdrawal of that statement and nothing else. Membership is asking the
/// field, not sharing the kind.
///
/// **Refused whole.** A name whose reading this instance can no longer place
/// carries no institution, so there is no ground and it joins no set — rather
/// than being folded into a neighbour's, which would put a claim about one
/// bank's document to him over a name that came from nobody knows where. It is
/// the same refusal decision 0030 makes one field over, where such an item
/// presets neither half of an identity rather than one.
///
/// **Never a set of one**, for [`ActionTarget::from_options`]'s reason: one item
/// is one question already, and «here is a set of one» would make a caller take
/// a set apart to find what it had.
fn shared_answer_set(
    wanted: &AccountNamedByDocument,
    every: &[AccountNamedByDocument],
) -> Option<(String, Vec<String>)> {
    let issuer = wanted.issuer.clone()?;
    let covers: Vec<String> = every
        .iter()
        .filter(|other| {
            other.declined.is_none() && other.issuer.as_deref() == Some(issuer.as_str())
        })
        .map(named_by_document_id)
        .collect();
    (covers.len() > 1).then_some((issuer, covers))
}

/// The account a document named that this instance cannot place.
///
/// **The one item in this queue whose subject is a string.** Every other item
/// about an account names it by identifier and prints the owner's own title
/// beside it; this one has neither, because the account does not exist. What it
/// has is what the document printed, and that is exactly what the owner needs in
/// order to recognise which of his accounts is meant.
///
/// [`ActionSubject`] is therefore `None`. The vocabulary's account subject
/// carries an identifier and a title, and filling either with a printed string
/// would publish, as one of the owner's accounts, something that is not one —
/// which is a worse answer than the absence, and the absence already reads as
/// «this is not about an account you hold».
///
/// **`provider_account_id` is preset and `title` is not**, and the asymmetry is
/// decision 0004's. The printed string is what the source repeats; the title is
/// what the owner reads and may rename tomorrow. Presetting it as the title
/// would resolve the rows — the third tier matches a title — and would do it
/// through the vocabulary the resolver deliberately does not offer in its own
/// refusals, so the first rename would silently stop a statement importing. As
/// the identifier the source prints, it is the second tier, it beats a title,
/// and it survives being renamed.
///
/// **`provider` is preset too, and is no longer his** (`iaam-9i83`). It used to
/// be published as a field the owner fills in, with no question beside it,
/// because there is no question a person can be asked about it: its only
/// property is that it differs between sources. A value whose whole purpose is
/// that it must differ is one this instance should mint — he cannot get it wrong
/// in an interesting way, gains nothing by choosing it, and must then remember
/// it forever, which is exactly what `provider_account_id`'s own doc obliges
/// whoever supplies it to do. And the scope was already known: the name was
/// refused while a document was being read, and the profile that read it
/// declares the institution the document is from.
///
/// **The institution and not the profile.** A profile's `id` is the identity
/// half of its `ParserVersion` and would survive the profile being corrected,
/// which is the argument for it; against it is that one institution ships two
/// documents — a card statement and a deposit statement are two profiles with
/// two ids — and an identifier scoped by the profile would say that one bank's
/// short sequential numbers are two vocabularies. They are one, and the thing
/// `provider` is for is keeping *different* sources' identifiers apart.
///
/// **What it asks him instead is where the account is held.** That is the
/// owner's own correction, and he narrowed it himself: not «what bank», because
/// a broker is not a bank and an account may sit at neither, but the bank,
/// broker or organisation the account is held at. `CreateAccountRequest` already
/// carried `institution` beside `provider`, so the item was publishing the
/// derived field and leaving the answerable one unasked — which is decision
/// 0027 §5's general form, stated on the field that produced it.
///
/// **Two ways out, because there are two** (`iaam-mk1n`). A statement names
/// accounts that are not the owner's at all, and from his side those records are
/// an expense already visible from the account whose statement they are on. The
/// item published `create_account` and nothing else, so «this name is not an
/// account of mine» was unrepresentable and the one act that closed the item was
/// one he had decided against — while the item stood as required work against
/// every report goal, permanently. That is `account_scope_action`'s finding one
/// noun over, and it was found the same way: an agent working a real import
/// reasoned its way to the hole, went looking for a route no item mentioned,
/// found none, and left the items behind without saying so.
///
/// The two are ordered and not ranked, and the account comes first: a name a
/// document printed on the owner's own statement is usually one of his.
///
/// **Two fields of one call, and they are asked in one breath** (`iaam-zxc6`).
/// What he calls the account and where it is held are published together, in
/// order, and nothing about decision 0027 obliges a caller to serialise them. It
/// did anyway, because nothing said otherwise; the sentence is on
/// [`RequestPlan::missing`] now.
///
/// **Where it is held may be left unanswered** (`iaam-4fsw`). `institution` is
/// optional on the request, no figure reads it, and the account is created
/// without it — so the item says so, and the question says what skipping costs.
/// It was published as though the account could not exist without it, and the
/// owner was stopped for a word by an agent that had just told him nothing
/// depended on it.
///
/// **And both fields carry an answer he may give once for every name this
/// reading printed** (`iaam-hdr7`). Seven printed names cost him roughly fifteen
/// exchanges, and he ended them with two sentences: they are all from one
/// institution, and call them what the statement calls them. Both were already
/// derivable here — the institution from the profile that read the document, the
/// names from the strings that document printed — and neither was offered,
/// because nothing published these items as a set with a value and a ground for
/// it. [`Proposal`] is that, and it is a question rather than a preset for the
/// reason decision 0030 refused to preset the institution: a value read out to
/// him is his answer, and a value hidden in the request is not.
///
/// **`NeedsOwnerInput`, with every field the queue can supply supplied.** Whether
/// an account of his is meant by this string — and whether it is one account or
/// two — is his judgement, exactly as the reporting perimeter is. A complete
/// request does not change who may send it.
fn account_named_by_document_action(
    wanted: &AccountNamedByDocument,
    every: &[AccountNamedByDocument],
) -> Action {
    let id = named_by_document_id(wanted);
    // Named in the sentence where it is known, because it is the fact that turns
    // «a document prints this» into «your bank prints this», and that is what
    // the owner recognises the account by.
    let read_as = wanted
        .issuer
        .as_deref()
        .map_or_else(String::new, |issuer| format!(", read as {issuer}'s,"));

    if let Some(reason) = &wanted.declined {
        return declined_account_name_action(wanted, id, &read_as, reason);
    }

    // **The identity travels whole or not at all.** `create_account` refuses
    // half of it, so where this instance cannot say which source printed the
    // string it presets neither half rather than one — an item presetting an
    // identifier with no scope would publish a request the route rejects on
    // arrival. That is unreachable through any route this API publishes, because
    // a kept document is immutable and is written before the names it could not
    // place are; it is written out because a `None` that cannot be reached is
    // still a `None` that has to mean something.
    let mut preset = BTreeMap::new();
    if let Some(issuer) = &wanted.issuer {
        preset.insert("provider".to_owned(), issuer.clone().into());
        preset.insert(
            "provider_account_id".to_owned(),
            wanted.printed.clone().into(),
        );
    }

    // The two fields he may answer once for every name this reading printed.
    // Both proposals are over the same set, because both stand on the same
    // ground — the reading said which institution printed these strings — and
    // an item that cannot say that is in no set and gets neither.
    let shared = shared_answer_set(wanted, every);
    let mut title = MissingInput::asked(OwnerPrompt::AccountTitle {
        printed: Some(wanted.printed.clone()),
    });
    // `institution` is `Option<String>` on the request and no figure reads it,
    // so the account is created whether he answers or not. Publishing it as
    // though the account could not exist without it is what stopped him over a
    // word he could have skipped (`iaam-4fsw`).
    let mut institution = MissingInput::asked(OwnerPrompt::AccountInstitution).optional();
    if let Some((issuer, covers)) = shared {
        title = title.proposing(Proposal {
            proposed: ProposedAnswer::AccountTitleAsPrinted {
                printed: wanted.printed.clone(),
            },
            covers: covers.clone(),
        });
        institution = institution.proposing(Proposal {
            proposed: ProposedAnswer::AccountInstitutionOfIssuer { issuer },
            covers,
        });
    }

    Action::new(
        ActionFacts {
            id,
            kind: ActionKind::CreateAccountNamedByDocument,
            category: ActionCategory::required_for(ActionKind::CreateAccountNamedByDocument),
            state: ActionState::NeedsOwnerInput,
            subject: None,
        },
        format!(
            "A document this instance kept{read_as} prints «{printed}» where it names the account a \
             record is on, and no single account of yours answers to it — by its iaam identifier, by \
             an identifier a source prints for it, or by its title. Either none does, or more than \
             one does and the reading refused rather than choosing. {records} record{record_plural} \
             in {documents} kept document{document_plural} named it, and every one of them was \
             refused when the document was read: they are in no journal, so they are in no report, \
             and nothing else says so. Nothing here decides what the account is. It may be one you \
             hold under another name, in which case give that account this identifier rather than \
             creating a second; it may be one you have not described yet, in which case create it; \
             and it may be no account of yours at all — a party you paid, an account belonging to \
             somebody else — in which case say so, and those records stay refused because you \
             decided it rather than because nobody has got round to them. If it is yours, the \
             remedy ends the same either way: read the document again, and the records that named \
             it are read this time. The row keys are over the document and the line, so the records \
             that already imported do not import twice.",
            printed = wanted.printed,
            records = wanted.records,
            record_plural = if wanted.records == 1 { "" } else { "s" },
            documents = wanted.documents,
            document_plural = if wanted.documents == 1 { "" } else { "s" },
        ),
        ActionTarget::from_options(vec![
            ResolutionOption {
                operation: OperationKey::CreateAccount,
                request: RequestPlan {
                    preset,
                    // Two fields of one call, published together because they
                    // may be put to him together (`iaam-zxc6`). The printed
                    // string travels *in the question*, not as a second preset:
                    // what is preset is the identifier, and saying so to the
                    // owner is what stops a caller showing him the preset
                    // instead (`iaam-ytvf`). The second is the question that
                    // replaced `/provider` — what the label scoping the printed
                    // identifier should be is worked out above; where the
                    // account is held is asked here, and it is the half a person
                    // can answer.
                    missing: vec![title, institution],
                },
            },
            declining_option(&wanted.printed),
        ]),
    )
    .expect("the named-account item publishes both of its resolutions")
}

/// Saying that a printed name is nobody's account of his, as a resolution.
///
/// Everything is known except the judgement and the sentence. The name is the
/// whole subject of the call and is preset; the disposition is what this option
/// **is**, so it is preset too — an option that left the caller to guess
/// `not_mine` would publish a route and not a resolution. The reason is the
/// owner's and is the only field asked for, for the reason `ExclusionReason` is
/// asked for one item over: a name ruled out without one is indistinguishable, a
/// year later, from one nobody ever got round to, and here it is stronger still,
/// because the records printed under the name stay refused on the strength of it.
fn declining_option(printed: &str) -> ResolutionOption {
    let mut preset = BTreeMap::new();
    preset.insert("printed".to_owned(), printed.to_owned().into());
    preset.insert("disposition".to_owned(), "not_mine".into());
    ResolutionOption {
        operation: OperationKey::RecordAccountNameDisposition,
        request: RequestPlan {
            preset,
            missing: vec![MissingInput::asked(OwnerPrompt::DeclinedNameReason)],
        },
    }
}

/// The same name, after the owner has said it is no account of his.
///
/// **`Informational`, and the demotion is the whole of `iaam-mk1n`'s remedy.**
/// The item is the same item, with the same identity, about the same string: the
/// records it counts are still refused and still in no report. What has changed
/// is that they are refused deliberately, and required work is what an owner has
/// not done rather than what he has decided. Left graded required, every report
/// he asked for would go on being flagged short on account of a decision he had
/// already made.
///
/// **The item does not disappear**, and that is deliberate rather than a
/// convenience. Two hundred records of his documents are in no journal because
/// of this statement; a queue that said nothing about them would be hiding the
/// consequence of his own decision from him, which is the silent drop this
/// module refuses everywhere else. So the queue keeps saying how many, and says
/// why.
///
/// **`NeedsOwnerInput` and not `Ready`.** Nothing is asked of him — that is what
/// `Informational` says — but the one act this item still offers is the
/// withdrawal of his own statement, and an agent may not withdraw a judgement it
/// could not have made.
///
/// **The way back is the withdrawal and not `create_account`.** Offering both
/// would publish, on an item that says the matter is settled, the very act he
/// declined; withdrawing puts the name back to being asked about, and the
/// required item that returns offers the account as it always did. And the
/// directory beats both: if an account of his comes to answer to the string, the
/// name is not folded at all and no item is raised, whether or not this
/// statement still stands.
fn declined_account_name_action(
    wanted: &AccountNamedByDocument,
    id: String,
    read_as: &str,
    reason: &str,
) -> Action {
    let mut preset = BTreeMap::new();
    preset.insert("printed".to_owned(), wanted.printed.clone().into());
    preset.insert("disposition".to_owned(), "undecided".into());

    Action::new(
        ActionFacts {
            id,
            kind: ActionKind::CreateAccountNamedByDocument,
            category: ActionCategory::Informational,
            state: ActionState::NeedsOwnerInput,
            subject: None,
        },
        format!(
            "A document this instance kept{read_as} prints «{printed}» where it names the account a \
             record is on, and you have said it is not an account of yours: «{reason}». \
             {records} record{record_plural} in {documents} kept document{document_plural} named \
             it, and every one of them is refused, so they are in no journal and in no report — \
             deliberately, and this line is the only thing that says so. Nothing is asked of you. \
             If one of your accounts does answer to the name after all, withdraw this and the \
             question is put again; and if you give an account that identifier, the name stops \
             being raised whether this stands or not.",
            printed = wanted.printed,
            records = wanted.records,
            record_plural = if wanted.records == 1 { "" } else { "s" },
            documents = wanted.documents,
            document_plural = if wanted.documents == 1 { "" } else { "s" },
        ),
        ActionTarget::Operation {
            operation: OperationKey::RecordAccountNameDisposition,
            request: RequestPlan {
                preset,
                // Nothing. Withdrawing leaves no statement for a reason to
                // explain, which is what the route says when one is sent.
                missing: Vec::new(),
            },
        },
    )
    .expect("the declined-name item offers the withdrawal of the statement")
}

/// Whether a name a document printed still names no single account of his.
///
/// **Asked here and never stored.** The record says a document printed a string;
/// whether the directory places it is a question about the directory, and the
/// directory moves. A stored verdict would keep publishing an account the owner
/// created an hour ago, and a queue that publishes work already done is a queue
/// he learns to ignore.
///
/// Asked through [`iaam_ingest::csv_source::AccountNames::resolve`] and nowhere
/// else, because that is the one implementation of decision 0004's tiering. A
/// second copy here — «is there an account with this title» — would let the
/// queue and the reader disagree about the same string, and the disagreement
/// would look like a queue item that never closes.
fn account_named_by_document_completion(
    directory: &iaam_ingest::csv_source::AccountNames,
    printed: &str,
) -> bool {
    directory.resolve(printed).is_ok()
}

fn account_named_by_document_gap(
    directory: &iaam_ingest::csv_source::AccountNames,
    printed: &str,
) -> bool {
    !account_named_by_document_completion(directory, printed)
}

/// The account no contour names and the owner has not ruled out.
///
/// One item per undecided account, identified by that account and naming it in
/// [`ActionSubject`] rather than only in the sentence. It is `RequiredForGoal`
/// because an account in this state is the mechanism by which a correct import
/// produces a silently incomplete report: every operation lands, every verdict
/// is positive, and the report leaves the account out because it is in no
/// contour, with nothing anywhere saying so.
///
/// `NeedsOwnerInput`, not `Ready`, even when every field is preset. Drawing the
/// reporting perimeter is the owner's judgement — the same rule that keeps
/// `first_contour_action` out of the agent's hands — and a fully formed request
/// does not change who may send it.
///
/// The target publishes both halves of the answer, because there are two and
/// the sentence says so. Membership is one — put the account in a contour — and
/// «this account is outside the perimeter, deliberately, and here is why» is the
/// other, which is a different route with a different body. While the item could
/// carry a single target it published membership alone, and the second way out
/// was reachable only by a caller who read the prose and then went looking
/// through the specification for a route no queue item mentioned. An agent that
/// treats `target` as the contract, which is what `target` is for, could put the
/// account inside a contour and do nothing else — including for an account that
/// belongs in no contour at all.
///
/// The two options are ordered, and membership comes first: an account the owner
/// is being asked about is usually one he means to report on, and the exclusion
/// is the answer for the ones he does not. Ordered, not ranked — neither is a
/// default, and the item stays `NeedsOwnerInput` for both.
///
/// The membership operation is [`OperationKey::AddContourVersion`], not
/// [`OperationKey::CreateContour`]. This item exists because an account is in no
/// contour while contours exist, so the act it wants is «put it in one of
/// those» — and while the only operation the queue could name was the one that
/// mints a contour, an agent following the queue literally answered the item by
/// creating a second perimeter holding that account alone.
fn account_scope_action(
    account: &AccountView,
    accounts: &[AccountView],
    contours: &[ContourView],
) -> Action {
    // A contour version is a complete composition, so «add this account» can be
    // written out only when there is no doubt which contour is meant. With one
    // contour there is none: the request is its current members plus this
    // account. With several, choosing for the owner would be choosing where his
    // money is reported from, so the choice is his and the composition cannot be
    // proposed without it.
    let (membership_preset, membership_missing) = match contours {
        [only] => {
            let mut members: Vec<AccountId> = only.accounts.clone();
            if !members.contains(&account.id) {
                members.push(account.id);
            }
            members.sort_by_key(AccountId::inner);
            let mut preset = BTreeMap::new();
            // The contour the route names in its path. It is preset rather than
            // missing because there is exactly one it could be.
            preset.insert("contour".to_owned(), only.id.0.to_string().into());
            preset.insert(
                "accounts".to_owned(),
                serde_json::Value::Array(
                    members
                        .iter()
                        .map(|member| member.inner().to_string().into())
                        .collect(),
                ),
            );
            // Nothing is missing. The title used to be asked for because the one
            // route this item could name demanded one for a contour that already
            // had one; versioning a contour carries its title forward, so the
            // owner is asked for the judgement and not for retyping.
            (preset, Vec::new())
        }
        _ => (
            BTreeMap::new(),
            vec![
                MissingInput::asked(OwnerPrompt::MembershipContour),
                MissingInput::asked_from(
                    OwnerPrompt::MembershipAccounts,
                    account_candidates(accounts),
                ),
            ],
        ),
    };

    // The other way out, and everything about it is known except the judgement.
    // The account is a path segment of the route and is preset; the disposition
    // is what this option *is*, so it is preset too — an option that left the
    // caller to guess `outside` would publish a route and not a resolution. The
    // reason is the owner's and is the only field asked for: an account ruled
    // out without one is indistinguishable, a year later, from an overlooked
    // one, which is why the route refuses it and why it is published as missing
    // rather than quietly omitted.
    let mut exclusion_preset = BTreeMap::new();
    exclusion_preset.insert("id".to_owned(), account.id.inner().to_string().into());
    exclusion_preset.insert("disposition".to_owned(), "outside".into());
    let exclusion = ResolutionOption {
        operation: OperationKey::RecordAccountScope,
        request: RequestPlan {
            preset: exclusion_preset,
            missing: vec![MissingInput::asked(OwnerPrompt::ExclusionReason)],
        },
    };

    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}",
                ActionKind::AccountScopeUndecided.id(),
                account.id.inner()
            ),
            kind: ActionKind::AccountScopeUndecided,
            category: ActionCategory::required_for(ActionKind::AccountScopeUndecided),
            state: ActionState::NeedsOwnerInput,
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) belongs to no contour and has not been ruled outside one; \
             until it is placed, its operations are absent from every report and nothing \
             else says so. Add it to an existing contour, or record that it is outside \
             the perimeter and why.",
            account.id.inner(),
            account.title
        ),
        ActionTarget::from_options(vec![
            ResolutionOption {
                operation: OperationKey::AddContourVersion,
                request: RequestPlan {
                    preset: membership_preset,
                    missing: membership_missing,
                },
            },
            exclusion,
        ]),
    )
    .expect("account scope action publishes both of its resolutions")
}

/// Every account the owner has, as contour-membership candidates.
fn account_candidates(accounts: &[AccountView]) -> Vec<AccountCandidate> {
    let mut candidates: Vec<_> = accounts
        .iter()
        .map(|account| AccountCandidate {
            id: account.id,
            title: account.title.clone(),
            institution: account.institution.clone(),
        })
        .collect();
    candidates.sort_by_key(|candidate| candidate.id.inner());
    candidates
}

fn first_contour_action(accounts: &[AccountView]) -> Action {
    let candidates = account_candidates(accounts);

    Action::new(
        ActionFacts {
            id: identity(ActionKind::CreateFirstContour),
            kind: ActionKind::CreateFirstContour,
            category: ActionCategory::required_for(ActionKind::CreateFirstContour),
            state: ActionState::NeedsOwnerInput,
            // Existential: no contour exists, so the item names no one account.
            subject: None,
        },
        "No contour exists; report boundaries cannot be computed until one is created.",
        ActionTarget::Operation {
            operation: OperationKey::CreateContour,
            request: RequestPlan {
                preset: BTreeMap::new(),
                missing: vec![
                    MissingInput::asked(OwnerPrompt::ContourTitle),
                    MissingInput::asked_from(OwnerPrompt::ContourAccounts, candidates),
                ],
            },
        },
    )
    .expect("first contour action has an operation target")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::sqlite::SqliteAdapter;
    use crate::ports::DeclinedAccountNameView;
    use crate::ports::ImportSessionView;
    use crate::ports::NewImportQuestion;
    use crate::ports::Store;
    use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
    use iaam_core::dates::{EffectiveOrder, EventDates};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
    use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{EventId, ImportQuestionId, ImportSessionId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use iaam_core::projection::money_flow::{DateWindow, MoneyFlow, NoCategories};
    use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
    use iaam_core::reconciliation::evidence::{Evidence, Ground, SourceChannel};
    use iaam_core::report::confidence::CaveatKind;
    use iaam_ingest::classification::{Answer, Counterparty, FarSide};
    use iaam_ingest::profile::UnresolvedAccountName;
    use iaam_store::SqliteStore;
    use std::collections::BTreeSet;
    use time::macros::date;

    fn store() -> SqliteAdapter {
        SqliteAdapter::new(SqliteStore::open_in_memory().expect("in-memory store"))
    }

    /// This crate's own source, so a guard can check what the queue offers
    /// rather than what a comment says it offers.
    ///
    /// Two files and not one, because a resolution is built in two places: here,
    /// where the frontier's items are assembled, and in the import-session
    /// scenario, where a refusal publishes the calls that end a session. A scan
    /// of one of them would pass by having less to sweep.
    const ACTION_SOURCE: &str = include_str!("actions.rs");
    const IMPORT_SESSION_SOURCE: &str = include_str!("scenarios/import_session.rs");

    /// The operation keys a source names in code, ignoring prose and fixtures.
    ///
    /// Both exclusions are load-bearing, which is why they are proved separately
    /// below. A doc comment naming a key is a mention and not an offer — much of
    /// this file's prose names one call to explain why a *different* one is the
    /// remedy — and a resolution built inside `mod tests` is a fixture the queue
    /// never publishes. A scan that counted either would report every key
    /// covered while an item was missing, which is the failure mode of every
    /// source-reading check.
    ///
    /// A line-based reading, so a key named in a trailing comment on a line of
    /// code would be counted. That over-counts rather than under-counts and no
    /// line in either file does it; the guard below refuses a name that is not a
    /// key at all, which is the drift this reading could otherwise hide.
    fn keys_named_in(source: &str) -> BTreeSet<String> {
        let body = source
            .split_once("\n#[cfg(test)]")
            .map_or(source, |(published, _)| published);
        let mut names = BTreeSet::new();
        for line in body.lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for (index, marker) in line.match_indices("OperationKey::") {
                let name: String = line[index + marker.len()..]
                    .chars()
                    .take_while(|character| character.is_ascii_alphanumeric())
                    .collect();
                if !name.is_empty() {
                    names.insert(name);
                }
            }
        }
        names
    }

    /// Every operation this API publishes is named by an item or by a caveat.
    ///
    /// **The guard that was missing** (`iaam-z36f`). Every other check in this
    /// line is narrower: one says an action names a goal, one says a caveat's
    /// remedy resolves against the contract, one says an offered route is gated
    /// by the floor it publishes, and `iaam-3nqt`'s says a named remedy actually
    /// removes the caveat it is named for. All of them start from a call that is
    /// already offered. None asks whether a call is offered at all.
    ///
    /// «Every reachable state has an act that leaves it» is not enumerable — a
    /// state is whatever a journal can be in. This is the half that is: the
    /// vocabulary of acts is a closed list, so a key **nothing** points at is
    /// either dead weight or an item that was never written, and the queue
    /// cannot tell which until someone asks. `iaam-1tij` was the second answer:
    /// the document channel was a call this API published and no resolution
    /// could name, and it was a field report that found it.
    ///
    /// It is a coverage check and not a correctness one, deliberately. That an
    /// item offering a key offers the *right* key is `iaam-3nqt`'s question, and
    /// this one would be worth little on its own — which is why the write-route
    /// sweep in `iaam_server::routes` is its other half: that one refuses a
    /// route that is neither a key nor declared not to be, so a channel cannot
    /// stay unofferable by staying unnamed, and this one refuses a key that
    /// nothing offers, so it cannot be named and then forgotten.
    #[test]
    fn every_operation_key_is_offered_by_an_item_or_a_caveat() {
        let mut offered = keys_named_in(ACTION_SOURCE);
        offered.extend(keys_named_in(IMPORT_SESSION_SOURCE));
        // The register is read as itself rather than scanned: `closed_by` is a
        // typed table in the core, so there is nothing to parse and nothing to
        // drift.
        for kind in CaveatKind::ALL {
            for key in kind.closed_by() {
                offered.insert(format!("{key:?}"));
            }
        }

        // The scan reads a name as an offer, so a name that is no key at all
        // means it has started reading something else — an associated constant,
        // a variant since renamed — and what it reports is no longer about this
        // vocabulary.
        for name in &offered {
            assert!(
                OperationKey::ALL
                    .iter()
                    .any(|key| format!("{key:?}") == *name),
                "the scan read {name} as an operation key and this vocabulary \
                 has none: teach `keys_named_in` about it, or the coverage it \
                 reports is about something else"
            );
        }

        let orphans: Vec<&str> = OperationKey::ALL
            .iter()
            .filter(|key| !offered.contains(&format!("{key:?}")))
            .map(|key| key.as_str())
            .collect();
        assert!(
            orphans.is_empty(),
            "{orphans:?} are calls this API publishes that no item and no \
             caveat points at. Either a state is missing its item, or the key \
             is dead and belongs out of the vocabulary — and a queue cannot \
             say which, which is why this has to be decided here"
        );
    }

    /// The scan reads resolutions, and not prose and not fixtures.
    ///
    /// A guard that passes vacuously is worse than none — `iaam-3nqt` exists
    /// because existence was all that was checked — and a scan that matched
    /// everything would report full coverage of anything. So the three
    /// exclusions are made against an input written here, where what the answer
    /// should be is not in question.
    #[test]
    fn the_offer_scan_reads_resolutions_and_not_prose_or_fixtures() {
        let source = concat!(
            "/// The membership operation is [`OperationKey::AddContourVersion`].\n",
            "    // Not OperationKey::RecordOwnerBalance, which asserts a figure.\n",
            "let way_out = ResolutionOption { operation: OperationKey::CreateAccount };\n",
            "\n#[cfg(test)]\nmod tests {\n",
            "    let fixture = OperationKey::SyncBroker;\n}\n",
        );
        assert_eq!(
            keys_named_in(source),
            BTreeSet::from(["CreateAccount".to_owned()]),
            "a key named in prose, in a comment, or in a fixture is not one the \
             queue offers"
        );
    }

    /// And it finds the resolutions the two real files build.
    ///
    /// The other half of the same argument: the exclusions above could be got
    /// right by a scan that excluded everything. This pins a key each file is
    /// known to build a resolution for, and that neither file names the whole
    /// vocabulary by itself — a scan that did would be matching, not reading.
    #[test]
    fn the_offer_scan_finds_the_resolutions_the_crate_builds() {
        let items = keys_named_in(ACTION_SOURCE);
        for name in [
            "OpenImportSession",
            "ReadImportDocument",
            "AddImportRows",
            "SyncBroker",
        ] {
            assert!(
                items.contains(name),
                "{name} is a resolution of `start_account_import` and the scan \
                 did not find it"
            );
        }

        let refusals = keys_named_in(IMPORT_SESSION_SOURCE);
        assert!(
            refusals.contains("AnswerImportQuestion"),
            "the session scenario publishes the call that settles a row"
        );
        assert!(
            refusals.len() < OperationKey::ALL.len(),
            "a scan that found every key in every file would pass by matching \
             everything: {refusals:?}"
        );
    }

    // --- The question put to the owner about a field (iaam-ytvf) ------------
    //
    // The defect these cover, in one sentence: an agent relaying a queue item to
    // the owner had one string per field, the JSON pointer, so it showed him
    // `provider_account_id` and this API's own schema descriptions, which are
    // written for whoever implements a client.

    /// Why a string is not a question this system may put to a person.
    ///
    /// Six refusals rather than a boolean, so that a check that passed for the
    /// wrong reason can be told from one that passed for the right one: the
    /// non-vacuity proof below asserts *which* rule fired on each specimen. They
    /// are the mechanical half of the owner's own rule — no internal words, say
    /// what it is for, say what the decision changes — and only the half a rule
    /// can hold. Whether a person who has never read this codebase can answer
    /// the question is not decidable here, and decision 0027 says who decides
    /// it.
    #[derive(Debug, PartialEq, Eq)]
    enum NotAsked {
        /// It names a place in a request rather than a thing in his life.
        APointer,
        /// It carries an identifier spelled for a program: `snake_case` or a
        /// camelCase hump.
        AWireName,
        /// It opens by telling him something instead of asking him something.
        NotAQuestion,
        /// It asks him something where it was to tell him what his answer does.
        AsksAgain,
        /// It is a label, not a sentence.
        NotASentence,
        /// It says his answer is his, or that he may change it, which is true
        /// of nearly every field here and tells him nothing about this one.
        SaysNothingTurnsOnIt,
    }

    /// The part of a string this system wrote, with the source's words removed.
    ///
    /// The register it goes on to pin is the one `iaam-ytvf` is about: «field
    /// name written for a client implementer» against «question written for a
    /// person». Language is not the axis — the surface is English by repository
    /// rule and the owner reads it — so nothing here looks at vocabulary; it
    /// looks at shape, which is the half a rule can hold.
    ///
    /// **A quoted span is exempt.** A value a source printed travels inside
    /// «…», and inside those marks it is the document's word and not ours: a
    /// bank that prints `acct_1` must not make this check read our sentence as
    /// having named a field.
    fn wording_of_ours(text: &str) -> Result<String, NotAsked> {
        let mut ours = String::with_capacity(text.len());
        let mut quoted = false;
        for character in text.chars() {
            match character {
                '«' => quoted = true,
                '»' => quoted = false,
                _ if !quoted => ours.push(character),
                _ => {}
            }
        }

        let characters: Vec<char> = ours.chars().collect();
        for pair in characters.windows(2) {
            if pair[0] == '/' && pair[1].is_ascii_alphabetic() {
                return Err(NotAsked::APointer);
            }
            if pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase() {
                return Err(NotAsked::AWireName);
            }
        }
        for triple in characters.windows(3) {
            let joined = triple[1] == '_';
            let word =
                |character: char| character.is_ascii_lowercase() || character.is_ascii_digit();
            if joined && word(triple[0]) && word(triple[2]) {
                return Err(NotAsked::AWireName);
            }
        }
        if ours.split_whitespace().count() < 5 {
            return Err(NotAsked::NotASentence);
        }
        Ok(ours)
    }

    /// Whether a string opens by asking the owner something.
    ///
    /// The **first** sentence is the question and what follows it is
    /// qualification. A rule reading only the last character would refuse every
    /// question that goes on to say anything, and one reading «is there a
    /// question mark anywhere» would admit a paragraph that asks nothing until
    /// its end — which is a description of the value, which is what a schema
    /// already publishes and what the owner was shown.
    fn opens_by_asking(text: &str) -> bool {
        matches!(text.find(['?', '.', '!']), Some(end) if text[end..].starts_with('?'))
    }

    /// Whether a string says what is different depending on how he answers.
    ///
    /// The two refusals are the two answers he has already been given and
    /// rejected. «It is yours to decide» and «you can change it later» are true
    /// of nearly every field in this vocabulary, so neither is about the field
    /// it is written on, and an item that said the second is what produced
    /// «what does this question even affect».
    fn says_what_turns_on_the_answer(text: &str) -> Result<(), NotAsked> {
        let ours = wording_of_ours(text)?;
        if opens_by_asking(&ours) {
            return Err(NotAsked::AsksAgain);
        }
        let folded = ours.to_lowercase();
        for empty in [
            "yours to",
            "your own to",
            "change it later",
            "change this later",
        ] {
            if folded.contains(empty) {
                return Err(NotAsked::SaysNothingTurnsOnIt);
            }
        }
        Ok(())
    }

    /// The whole question: it asks, and it says what the answer does.
    fn puts_a_question_to_a_person(question: &OwnerQuestion) -> Result<(), NotAsked> {
        let ask = wording_of_ours(&question.ask)?;
        if !opens_by_asking(&ask) {
            return Err(NotAsked::NotAQuestion);
        }
        says_what_turns_on_the_answer(&question.consequence)
    }

    /// One question written for this test, so the refusals below are one change.
    fn asking(ask: &str, consequence: &str) -> OwnerQuestion {
        OwnerQuestion {
            ask: ask.to_owned(),
            consequence: consequence.to_owned(),
        }
    }

    /// A consequence that is not what is under test, so that the `ask` is.
    const A_REAL_CONSEQUENCE: &str = "Every report that names this account shows whatever you answer, and nothing else \
         reads it.";

    /// A question that is not what is under test, so that the consequence is.
    const A_REAL_ASK: &str = "What do you want to call this account?";

    /// The check refuses the strings the field report actually carried.
    ///
    /// **A guard that passes vacuously is worse than none** — `iaam-3nqt` exists
    /// because existence was all that was checked, and the wave before this one
    /// makes the same argument at
    /// `the_offer_scan_reads_resolutions_and_not_prose_or_fixtures`. So each
    /// rule is made against an input written here, where what the answer should
    /// be is not in question, and the *reason* is asserted rather than the
    /// refusal: a check that refused everything would pass a test that only
    /// asked whether it refused. Every specimen differs from a passing question
    /// in exactly one way, so the rule that fires is the rule being proved.
    ///
    /// The first two are what the owner was shown. The third is this API's own
    /// description of that same field: fluent English, no field name in it, and
    /// still nothing a person could answer — the defect was never that the
    /// surface had no prose on it. The last two are the answers he was given
    /// when the prose improved and still told him nothing about his choice, and
    /// they are the reason a consequence is a value of its own.
    #[test]
    fn a_question_for_a_person_is_not_a_field_name() {
        assert_eq!(
            puts_a_question_to_a_person(&asking("provider_account_id", A_REAL_CONSEQUENCE)),
            Err(NotAsked::AWireName)
        );
        assert_eq!(
            puts_a_question_to_a_person(&asking("/title", A_REAL_CONSEQUENCE)),
            Err(NotAsked::APointer)
        );
        assert_eq!(
            puts_a_question_to_a_person(&asking(
                "Whatever the source prints for this account.",
                A_REAL_CONSEQUENCE
            )),
            Err(NotAsked::NotAQuestion)
        );
        assert_eq!(
            puts_a_question_to_a_person(&asking("Which?", A_REAL_CONSEQUENCE)),
            Err(NotAsked::NotASentence)
        );
        assert_eq!(
            puts_a_question_to_a_person(&asking(
                A_REAL_ASK,
                "The title is yours to decide and nobody else writes it."
            )),
            Err(NotAsked::SaysNothingTurnsOnIt)
        );
        assert_eq!(
            puts_a_question_to_a_person(&asking(
                A_REAL_ASK,
                "You can change it later, so nothing is lost by guessing now."
            )),
            Err(NotAsked::SaysNothingTurnsOnIt)
        );
        assert_eq!(
            puts_a_question_to_a_person(&asking(
                A_REAL_ASK,
                "What happens if you call it something else? Very little, mostly."
            )),
            Err(NotAsked::AsksAgain)
        );
        assert_eq!(
            puts_a_question_to_a_person(&asking(A_REAL_ASK, A_REAL_CONSEQUENCE)),
            Ok(())
        );
        // And the exemption is real: a source may print anything at all, and the
        // printed string is the reason this question can be answered.
        assert_eq!(
            puts_a_question_to_a_person(&asking(
                "What do you call the account a document prints «acct_1/2» for?",
                A_REAL_CONSEQUENCE
            )),
            Ok(())
        );
    }

    /// One of each question, with the specialised shapes written out.
    ///
    /// A list and not a derivation: [`OwnerPrompt::AccountTitle`] carries a
    /// datum, so «every variant» is not a set this type can hand over, and the
    /// shape that carries the datum is exactly the one a walk over bare variants
    /// would skip. `every_question_is_written_out_once` keeps it complete.
    fn specimens() -> Vec<OwnerPrompt> {
        vec![
            OwnerPrompt::AccountTitle { printed: None },
            OwnerPrompt::AccountTitle {
                printed: Some("Shop One".to_owned()),
            },
            OwnerPrompt::ContourTitle,
            OwnerPrompt::ContourAccounts,
            OwnerPrompt::MembershipContour,
            OwnerPrompt::MembershipAccounts,
            OwnerPrompt::ExclusionReason,
            OwnerPrompt::TransferPartners,
            OwnerPrompt::BrokerChannel,
            OwnerPrompt::SyncFrom,
            OwnerPrompt::SyncTo,
            OwnerPrompt::Corrections,
            OwnerPrompt::AcknowledgeRetraction,
            OwnerPrompt::OwnerBalanceCash,
            OwnerPrompt::OpeningAmount,
            OwnerPrompt::OpeningCurrency,
            OwnerPrompt::OpeningDate,
            OwnerPrompt::RuleMatcher,
            OwnerPrompt::RuleCategory,
            OwnerPrompt::ImportAnswer,
            OwnerPrompt::TransferFarSide,
            OwnerPrompt::AccountInstitution,
            OwnerPrompt::DeclinedNameReason,
        ]
    }

    /// The name of one question, and the `match` that keeps the list above honest.
    ///
    /// Exhaustive on purpose, and that is what stops this going stale: a
    /// twenty-second question does not slip through — it stops the test from
    /// compiling, so whoever adds one answers, here, that it is written out and
    /// swept before the queue can publish a field with no words on it.
    fn question_name(prompt: &OwnerPrompt) -> &'static str {
        match prompt {
            OwnerPrompt::AccountTitle { .. } => "AccountTitle",
            OwnerPrompt::ContourTitle => "ContourTitle",
            OwnerPrompt::ContourAccounts => "ContourAccounts",
            OwnerPrompt::MembershipContour => "MembershipContour",
            OwnerPrompt::MembershipAccounts => "MembershipAccounts",
            OwnerPrompt::ExclusionReason => "ExclusionReason",
            OwnerPrompt::TransferPartners => "TransferPartners",
            OwnerPrompt::BrokerChannel => "BrokerChannel",
            OwnerPrompt::SyncFrom => "SyncFrom",
            OwnerPrompt::SyncTo => "SyncTo",
            OwnerPrompt::Corrections => "Corrections",
            OwnerPrompt::AcknowledgeRetraction => "AcknowledgeRetraction",
            OwnerPrompt::OwnerBalanceCash => "OwnerBalanceCash",
            OwnerPrompt::OpeningAmount => "OpeningAmount",
            OwnerPrompt::OpeningCurrency => "OpeningCurrency",
            OwnerPrompt::OpeningDate => "OpeningDate",
            OwnerPrompt::RuleMatcher => "RuleMatcher",
            OwnerPrompt::RuleCategory => "RuleCategory",
            OwnerPrompt::ImportAnswer => "ImportAnswer",
            OwnerPrompt::TransferFarSide => "TransferFarSide",
            OwnerPrompt::AccountInstitution => "AccountInstitution",
            OwnerPrompt::DeclinedNameReason => "DeclinedNameReason",
        }
    }

    /// Every question is written out once, and the specialised shape too.
    #[test]
    fn every_question_is_written_out_once() {
        let names: BTreeSet<&str> = specimens().iter().map(question_name).collect();
        assert_eq!(
            names.len(),
            22,
            "a question was added to the vocabulary and not to the list the guards run over"
        );
        assert_eq!(
            specimens().len(),
            23,
            "the account title is asked in two shapes and both are swept"
        );
    }

    /// Every question is written for the owner and not for a client author.
    ///
    /// Distinctness is asserted on both halves and it is not tidiness: two
    /// fields carrying one sentence means one of them is being described in the
    /// other's words, and two fields carrying one consequence means at least one
    /// of them is not saying what turns on *its* answer. That is the defect
    /// wearing better prose, which is the form it took when the prose improved
    /// and the owner asked what the question affected.
    #[test]
    fn every_question_the_owner_is_put_is_written_for_him() {
        let mut asks: BTreeSet<String> = BTreeSet::new();
        let mut consequences: BTreeSet<String> = BTreeSet::new();
        for prompt in specimens() {
            let question = prompt.question();
            assert_eq!(
                puts_a_question_to_a_person(&question),
                Ok(()),
                "{} does not put a question to a person: {question:?}",
                question_name(&prompt)
            );
            assert!(
                !question.ask.contains(prompt.pointer())
                    && !question.consequence.contains(prompt.pointer()),
                "{} shows the owner the pointer it is published on",
                question_name(&prompt)
            );
            assert!(
                asks.insert(question.ask.clone()),
                "{} asks in another field's words: {}",
                question_name(&prompt),
                question.ask
            );
            assert!(
                consequences.insert(question.consequence.clone()),
                "{} borrows another field's consequence: {}",
                question_name(&prompt),
                question.consequence
            );
        }
    }

    /// What turns on a name is not the same on both accounts, and it may not be.
    ///
    /// **The one place obligation three legitimately varies by item**, and the
    /// test that keeps the other two from varying with it. A name is for the
    /// same thing wherever it is asked, so the two questions open the same way;
    /// what a rename costs is decided by whether the account already carries the
    /// string a document printed for it, and `AccountNames::candidates` in
    /// `iaam_ingest::csv_source` is where that is decided — a printed identifier
    /// is matched before a title is, and returns early, so on an account created
    /// from a document the title is never reached and a rename is free, while on
    /// an account with no printed identifier the title is the only thing a
    /// statement line can find it by.
    ///
    /// So this asserts the split rather than the sameness: one field of one
    /// call, one shape of question, two answers to «what does this change».
    #[test]
    fn what_a_rename_costs_depends_on_whether_a_document_named_the_account() {
        let alone = OwnerPrompt::AccountTitle { printed: None };
        let named_by_a_document = OwnerPrompt::AccountTitle {
            printed: Some("Shop One".to_owned()),
        };

        assert_eq!(alone.pointer(), named_by_a_document.pointer());
        assert_eq!(alone.asked_by(), named_by_a_document.asked_by());
        assert_ne!(
            alone.question().consequence,
            named_by_a_document.question().consequence,
            "a rename is free on one of these and is not on the other, and the \
             owner is the one who has to be told which"
        );
    }

    /// Every state this queue can reach, as one heap of items.
    ///
    /// Several states rather than one, because the interesting ones exclude each
    /// other: an owner with no account is asked to create one, and an owner with
    /// accounts is asked everything else. The union is what the sweep needs, and
    /// `the_sweep_sees_every_question_this_vocabulary_has` proves the union is
    /// not a subset.
    fn every_queue_item() -> Vec<Action> {
        let main = named("Main");
        let savings = named("Savings");
        let term = named("Term");
        let mut items = Vec::new();

        // Nothing described, and two documents that named accounts. Two and not
        // one, because one printed name is a set of one and publishes no
        // proposal, so a heap holding one would let the sweeps run over a queue
        // in which nothing was ever offered over a set (`iaam-hdr7`).
        items.extend(queue_wanting(
            &[],
            &[wanted("Shop One", 3, 1), wanted("Shop Two", 5, 1)],
        ));

        // One account, no perimeter, no facts: the first perimeter, the
        // transfer statement, and the routes that begin an import.
        items.extend(
            actions_from_state(&OwnerState {
                accounts: &[main.clone(), savings.clone()],
                contours: &[],
                exclusions: &[],
                transfers: &[],
                activity: &[no_facts(main.id), no_facts(savings.id)],
                assertions: &[],
                retired: RetirementAssessment::Assessed(&[]),
                sessions: &[],
                questions: &[],
                rules: &[],
                wanted_accounts: &[],
            })
            .expect("actions from state"),
        );

        // Two perimeters and an account inside neither: membership, or a reason
        // for staying out.
        let perimeters = [
            ContourView {
                id: ContourId::new_random(),
                version: ContourVersion(1),
                title: "Household".into(),
                accounts: vec![main.id],
            },
            ContourView {
                id: ContourId::new_random(),
                version: ContourVersion(1),
                title: "Business".into(),
                accounts: vec![],
            },
        ];
        items.extend(actions_from_views(
            &[main.clone(), savings.clone()],
            &perimeters,
            &[],
            &[],
            &[],
        ));

        // An account with facts and no control assertion.
        items.extend(assertion_queue(&main, august(), &[]));

        // A product he retired that the journal still shows a figure for.
        items.extend(queue_for_retirement(&term, &[ceased(term.id, false)]));

        // A session holding a row nobody has said what it was.
        let session = ImportSessionId::new_random();
        items.extend(queue_for_sessions(
            std::slice::from_ref(&main),
            &[session_summary(ImportSessionState::Open, 1, 1)],
            &[asked(session, main.id, 1)],
        ));

        // The diagnostics: an unexplained outflow wants a rule, a discrepancy
        // wants a correction, and an unconfirmed period wants a broker.
        items.extend(every_diagnostic());
        items
    }

    /// Every field the owner fills in carries the question to put to him.
    ///
    /// **The guard the bead asked for**, and the type does half of it already:
    /// [`ProvidedBy::Owner`] cannot reach a [`MissingInput`] through
    /// [`MissingInput::plain`] at all, because that constructor takes the two
    /// words that ask nobody anything. What is left for a test is the half a
    /// signature cannot hold — that a struct literal, or
    /// [`MissingInput::asked_without_a_question`], did not put the field back in
    /// the state this bead was filed on.
    ///
    /// **Satisfied three ways, and that is deliberate.** A field carries a
    /// question; or it is in [`QUESTIONS_UNDER_REVIEW`], which says the question
    /// itself is undecided and names the bead deciding; or it stops being the
    /// owner's, which is what happens when a value this instance can work out is
    /// worked out instead of asked for. A guard satisfiable only by writing
    /// prose would push the next author into writing a fluent question for a
    /// question that should not be asked — which is `iaam-9i83`, and it was
    /// found on the very field this bead was reported against.
    ///
    /// The pointer and the call are checked against the question rather than
    /// taken on trust: a question published on a resolution that calls something
    /// else is a question about a field that request does not have.
    #[test]
    fn every_field_the_owner_fills_in_carries_the_question_to_put_to_him() {
        for action in every_queue_item() {
            for (operation, request) in action.target().resolutions() {
                let owned: Vec<(&str, &Option<OwnerPrompt>)> = request
                    .missing
                    .iter()
                    .filter(|missing| missing.provided_by == ProvidedBy::Owner)
                    .map(|missing| (missing.pointer.as_str(), &missing.prompt))
                    .chain(
                        request
                            .missing
                            .iter()
                            .flat_map(|missing| &missing.alternatives)
                            .flat_map(|alternative| &alternative.requires)
                            .filter(|required| required.provided_by == ProvidedBy::Owner)
                            .map(|required| (required.pointer.as_str(), &required.prompt)),
                    )
                    .collect();

                for (pointer, prompt) in owned {
                    let Some(prompt) = prompt else {
                        assert!(
                            QUESTIONS_UNDER_REVIEW.contains(&(operation, pointer)),
                            "{} asks the owner for {pointer} and gives whoever has to put it \
                             to him nothing but the pointer. Write the question, or work the \
                             value out instead of asking, or register the field and name the \
                             bead deciding which",
                            action.id()
                        );
                        continue;
                    };
                    assert_eq!(
                        prompt.pointer(),
                        pointer,
                        "{} publishes {pointer} with the question for {}",
                        action.id(),
                        prompt.pointer()
                    );
                    assert_eq!(
                        prompt.asked_by(),
                        operation,
                        "{} asks {pointer} of {operation:?}, and that question is a field of {:?}",
                        action.id(),
                        prompt.asked_by()
                    );
                }
            }
        }
    }

    /// And the sweep sees every question this vocabulary has.
    ///
    /// The other half of the same argument, in the shape
    /// `the_offer_scan_finds_the_resolutions_the_crate_builds` takes: the guard
    /// above could be satisfied by a heap of items that asks the owner nothing,
    /// and it would report every field covered. So the fields the heap actually
    /// witnesses are compared against the fields the vocabulary declares, in
    /// both directions — a question no item publishes is prose nothing asks, and
    /// an owner field the heap does not reach is a field the guard above never
    /// ran on.
    #[test]
    fn the_sweep_sees_every_question_this_vocabulary_has() {
        let mut witnessed: BTreeSet<(OperationKey, String)> = BTreeSet::new();
        for action in every_queue_item() {
            for (operation, request) in action.target().resolutions() {
                for missing in &request.missing {
                    if missing.provided_by == ProvidedBy::Owner {
                        witnessed.insert((operation, missing.pointer.clone()));
                    }
                    for required in missing
                        .alternatives
                        .iter()
                        .flat_map(|alternative| &alternative.requires)
                    {
                        if required.provided_by == ProvidedBy::Owner {
                            witnessed.insert((operation, required.pointer.clone()));
                        }
                    }
                }
            }
        }

        let mut declared: BTreeSet<(OperationKey, String)> = specimens()
            .iter()
            .map(|prompt| (prompt.asked_by(), prompt.pointer().to_owned()))
            .collect();
        declared.extend(
            QUESTIONS_UNDER_REVIEW
                .iter()
                .map(|(operation, pointer)| (*operation, (*pointer).to_owned())),
        );

        assert_eq!(
            witnessed, declared,
            "the fields the queue asks the owner for and the questions this \
             vocabulary holds have come apart"
        );
    }

    /// Every kind is in [`ActionKind::ALL`] exactly once.
    ///
    /// The walk below is only as complete as this array, so the array is checked
    /// first and separately: `id()` is exhaustive, so two kinds sharing an
    /// identity or a kind repeated here is caught, and the count is stated so
    /// that a kind added to the enum and to `goals` but forgotten here does not
    /// quietly shrink the walk.
    #[test]
    fn every_kind_is_listed_once_in_all() {
        let ids: BTreeSet<&str> = ActionKind::ALL.iter().map(|kind| kind.id()).collect();
        assert_eq!(
            ids.len(),
            ActionKind::ALL.len(),
            "two kinds share an identity, or one is listed twice: {:?}",
            ActionKind::ALL.map(ActionKind::id)
        );
        assert_eq!(ActionKind::ALL.len(), 19, "a kind was added without a goal");
    }

    /// Every kind graded `RequiredForGoal` names at least one goal, and every
    /// goal it names is one of the four.
    ///
    /// The expected sets are restated here rather than read back from
    /// [`ActionKind::goals`]: a test that asked the table what the table says
    /// would pass for any table, including the one that names nothing. This is
    /// the mapping, written a second time, from what the reports actually read.
    ///
    /// The `match` is exhaustive on purpose, and that is what keeps this from
    /// going stale. A twentieth kind does not slip through — it stops the test
    /// from compiling, so whoever adds it answers, here, which reports their new
    /// item stands in the way of, before the queue can publish an item that
    /// names none.
    ///
    /// Each kind is also run through [`Action::new`], so the invariant is
    /// asserted on an item and not only on a table: a required category that
    /// names nothing is refused at construction, and a kind the queue never
    /// grades required is refused if someone grades it.
    #[test]
    fn every_required_action_names_at_least_one_of_the_four_goals() {
        use ReportGoal::{AssetSnapshot, MoneyFlow, Reconciliation, Returns};

        for kind in ActionKind::ALL {
            let expected: &[ReportGoal] = match kind {
                // Blocking: it stops the next call, not a report.
                ActionKind::CreateFirstAccount => &[],
                // The records that named this account were refused when the
                // document was read, so they are in no journal and therefore in
                // no report. The same grading `StartAccountImport` gets, for the
                // same reason, and it is the whole of the item's case for being
                // required rather than recommended.
                ActionKind::CreateAccountNamedByDocument => {
                    &[AssetSnapshot, MoneyFlow, Returns, Reconciliation]
                }
                // An account in no contour is outside `report_population`'s
                // covered set. Not reconciliation: `reconciliation::report`
                // takes an account and resolves no contour.
                ActionKind::CreateFirstContour | ActionKind::AccountScopeUndecided => {
                    &[AssetSnapshot, MoneyFlow, Returns]
                }
                // An unpaired leg is counted as money crossing the perimeter by
                // `MoneyFlow::apply` and by `FlowLog`. Not the snapshot: the leg
                // lands on its own account's cash either way. Not
                // reconciliation: pairing keeps both legs, so observed cash and
                // turnover per account do not move.
                ActionKind::ResolveTransferRelationships => &[MoneyFlow, Returns],
                // A row in no journal is in no report; an account with no facts
                // has nothing for any of the four to say; a session's rows are
                // pre-journal until it commits, so every report is computed as
                // though they did not exist; and a possible duplicate **is**
                // recorded, so it may be the same money twice.
                ActionKind::StartAccountImport
                | ActionKind::AnswerClassificationQuestion
                | ActionKind::ImportSessionUnfinished
                | ActionKind::PossibleDuplicateUndecided => {
                    &[AssetSnapshot, MoneyFlow, Returns, Reconciliation]
                }
                // The closing assertion is reconciliation's claim side; the
                // opening one is what makes the snapshot's cash a balance rather
                // than movement, which `account_balances` decides per account
                // and currency. It has no legs, so it moves no number in flow or
                // returns.
                ActionKind::ProvideControlAssertion => &[AssetSnapshot, Reconciliation],
                // A retirement is read in one place — the asset snapshot's row
                // suppression. `contour::classify` never sees it, so flow and
                // returns are unchanged by it, and `reconciliation::report`
                // never asks whether a product still exists.
                ActionKind::RetiredAccountNotEmpty => &[AssetSnapshot],
                // The item that stands in for it when the fold refuses stands
                // between the owner and the same report, and only that one: it
                // is raised for an owner who has retired something, and the
                // question it could not answer is the snapshot's.
                ActionKind::RetirementNotAssessed => &[AssetSnapshot],
                // All three are about whether a period is confirmed.
                ActionKind::CoverageGapUnrepaired
                | ActionKind::IndependentConfirmationMissing
                | ActionKind::DiscrepancyUnresolved => &[Reconciliation],
                // Recommended and informational: never required work. The
                // adopted rule decides rows nobody has submitted yet; the row it
                // was learned from is already settled, so no report is short of
                // anything while it stands unwritten.
                ActionKind::AdoptClassificationRule
                | ActionKind::UndecomposedOutflows
                | ActionKind::ExternalTransfersUncategorised
                | ActionKind::UnexplainedResidual => &[],
            };

            let goals: Vec<ReportGoal> = kind.goals().iter().collect();
            assert_eq!(
                goals,
                expected.to_vec(),
                "the goals of {} are not what the reports read",
                kind.id()
            );
            assert!(
                goals.iter().all(|goal| ReportGoal::ALL.contains(goal)),
                "{} names a goal outside the four",
                kind.id()
            );

            let built = Action::new(
                ActionFacts {
                    id: kind.id().to_owned(),
                    kind,
                    category: ActionCategory::required_for(kind),
                    state: ActionState::NeedsOwnerInput,
                    subject: None,
                },
                "a reason",
                ActionTarget::Operation {
                    operation: OperationKey::CreateAccount,
                    request: RequestPlan {
                        preset: BTreeMap::new(),
                        missing: Vec::new(),
                    },
                },
            );

            if expected.is_empty() {
                assert_eq!(
                    built.err(),
                    Some(ActionInvariantError::RequiredForNoGoal),
                    "{} is never required for a goal, so grading it required must be refused",
                    kind.id()
                );
            } else {
                let action =
                    built.unwrap_or_else(|error| panic!("{} was refused: {error:?}", kind.id()));
                assert!(
                    !action.category().goals().is_empty(),
                    "{} is required for a goal and names none",
                    kind.id()
                );
                assert_eq!(
                    action.category().goals().iter().collect::<Vec<_>>(),
                    expected.to_vec(),
                    "{} publishes goals other than the ones it carries",
                    kind.id()
                );
            }
        }
    }

    /// The set carries every goal the vocabulary has, and no more.
    ///
    /// The four names themselves are asserted where they are declared, in
    /// `iaam_core::goal`. Restating them here would be a second copy of the
    /// vocabulary in the crate that has just stopped keeping one — and the
    /// stopgap that used to stand here, comparing this crate's four codes with
    /// the core's, is now a comparison of an array with itself.
    #[test]
    fn the_goal_set_covers_the_whole_vocabulary() {
        assert_eq!(ReportGoals::ALL.iter().count(), ReportGoal::ALL.len());
        assert_eq!(
            ReportGoals::ALL.iter().collect::<Vec<_>>(),
            ReportGoal::ALL.to_vec(),
            "a goal has no bit in the set, so an item required for it publishes nothing"
        );
        assert!(ReportGoals::NONE.is_empty());
    }

    /// Asking for the snapshot returns a shorter list than asking for the queue.
    ///
    /// The point of the whole change, asserted rather than described: a
    /// reconciliation-only item is not something that stands between the owner
    /// and a statement of what he holds, and before this it was indistinguishable
    /// from one that is.
    #[test]
    fn the_snapshot_is_blocked_by_fewer_items_than_everything() {
        let required: Vec<ActionKind> = ActionKind::ALL
            .into_iter()
            .filter(|kind| !kind.goals().is_empty())
            .collect();
        let for_snapshot: Vec<ActionKind> = required
            .iter()
            .copied()
            .filter(|kind| kind.goals().contains(ReportGoal::AssetSnapshot))
            .collect();

        assert!(
            for_snapshot.len() < required.len(),
            "every required item still blocks the snapshot: {for_snapshot:?}"
        );
        assert!(
            !for_snapshot.contains(&ActionKind::ResolveTransferRelationships),
            "a leg folds into its own account's balance whether or not its partner is known"
        );
        assert!(
            !for_snapshot.contains(&ActionKind::CoverageGapUnrepaired),
            "a coverage gap is a statement about one import attempt's confirmation"
        );
    }

    fn with_id(id: AccountId) -> AccountView {
        AccountView {
            id,
            title: "Main".into(),
            institution: None,
        }
    }

    /// Every account a hand-built ledger names, as the owner's accounts.
    ///
    /// The diagnostics take the owner's accounts because every item they emit
    /// says what he calls the account it is about. A test that assembles a
    /// ledger from events holds identifiers and nothing else, so the accounts
    /// are invented from them here rather than at every call site.
    fn ledger_accounts(ledger: &ReconciliationLedger) -> Vec<AccountView> {
        let mut ids: BTreeSet<AccountId> = ledger.gaps().iter().map(|gap| gap.account).collect();
        ids.extend(ledger.statuses().map(|status| status.account()));
        ids.into_iter().map(with_id).collect()
    }

    fn ledger_actions(ledger: &ReconciliationLedger) -> Vec<Action> {
        ledger_diagnostics(ledger, &ledger_accounts(ledger))
            .expect("every account the ledger names is one of the owner's")
    }

    /// The same, for a money flow: the accounts its own breakdowns name.
    fn flow_accounts(report: &MoneyFlowReport) -> Vec<AccountView> {
        let mut ids = BTreeSet::new();
        for currency in report.flow.currencies() {
            for (account, _, _, _) in report
                .flow
                .not_decomposed_by_account_and_cause(currency)
                .expect("undecomposed breakdown")
            {
                ids.insert(account);
            }
        }
        for (account, _) in report
            .flow
            .residuals_by_account()
            .expect("residual breakdown")
        {
            ids.insert(account);
        }
        ids.into_iter().map(with_id).collect()
    }

    fn flow_actions(report: &MoneyFlowReport) -> Vec<Action> {
        flow_diagnostics(report, &flow_accounts(report))
            .expect("every account the flow names is one of the owner's")
    }

    fn no_facts(account: AccountId) -> AccountActivityView {
        AccountActivityView {
            account,
            has_business_fact: false,
            first_effective_date: None,
            last_effective_date: None,
        }
    }

    fn named(title: &str) -> AccountView {
        AccountView {
            id: AccountId::new_random(),
            title: title.into(),
            institution: None,
        }
    }

    fn account() -> AccountView {
        AccountView {
            id: AccountId::new_random(),
            title: "Main".into(),
            institution: Some("Savings".into()),
        }
    }

    #[tokio::test]
    async fn an_empty_owner_isoffered_the_first_account_action() {
        let owner = OwnerId::new_random();
        let store = store();
        let actions = frontier(owner, &store, &store).await.expect("frontier");

        assert_eq!(actions.len(), 1);
        let action = &actions[0];
        assert_eq!(action.kind(), ActionKind::CreateFirstAccount);
        assert_eq!(action.category(), ActionCategory::Blocking);
        assert_eq!(action.state(), ActionState::NeedsOwnerInput);
        assert_eq!(action.required_scope(), Some(Scope::Owner));
        let ActionTarget::Operation { operation, request } = action.target() else {
            panic!("first account needs an operation target");
        };
        assert_eq!(*operation, OperationKey::CreateAccount);
        assert_eq!(request.missing.len(), 1);
        assert_eq!(request.missing[0].pointer, "/title");
        assert_eq!(request.missing[0].provided_by, ProvidedBy::Owner);
        assert!(request.missing[0].candidates.is_none());
    }

    // --- The accounts a document asked for (iaam-x9ls) ----------------------
    //
    // The defect these cover, in one sentence: `create_first_account` says
    // «create an account» and cannot say which, so an agent following the queue
    // literally invents a title, the import refuses every row against it, and
    // the only way to learn the real names is to provoke two hundred refusals
    // and mine them.

    /// A name one institution's documents printed, and nothing said about it.
    ///
    /// The issuer is filled in because that is the ordinary case: a name is
    /// recorded by a reading, a reading is of a kept document, and a kept
    /// document says which profile read it. The absent one is written out at
    /// `a_name_whose_document_is_gone_presets_no_half_of_an_identity`.
    fn wanted(printed: &str, records: u32, documents: u32) -> AccountNamedByDocument {
        AccountNamedByDocument {
            printed: printed.to_owned(),
            records,
            documents,
            issuer: Some("Example Bank".to_owned()),
            declined: None,
        }
    }

    fn queue_wanting(accounts: &[AccountView], wanted: &[AccountNamedByDocument]) -> Vec<Action> {
        actions_from_state(&OwnerState {
            accounts,
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: wanted,
        })
        .expect("actions from state")
    }

    /// The one resolution of the item, by the operation it names.
    fn resolution(action: &Action, operation: OperationKey) -> &RequestPlan {
        action
            .target()
            .resolutions()
            .into_iter()
            .find(|(named, _)| *named == operation)
            .unwrap_or_else(|| panic!("the item offers no {operation:?}"))
            .1
    }

    /// The item for one printed name, whatever it is graded.
    fn named_by_document(actions: &[Action]) -> &Action {
        actions
            .iter()
            .find(|action| action.kind() == ActionKind::CreateAccountNamedByDocument)
            .expect("the account a document named is in the queue")
    }

    /// The queue names the account, and hands back a request that resolves it.
    ///
    /// Two assertions and the second is the one that matters. The name is in the
    /// item, so a reader learns it without provoking anything; and the request
    /// presets `provider_account_id` rather than `title`, so the account created
    /// from this item is recognised at decision 0004's *identity* tier — which a
    /// rename does not move — instead of the title tier, which it does.
    #[test]
    fn the_queue_names_the_account_a_document_asked_for() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 220, 1)]);

        let action = named_by_document(&actions);
        assert!(
            action.reason().contains("Shop One"),
            "the item must name the account: {}",
            action.reason()
        );
        assert!(
            action.reason().contains("220"),
            "the item must say how many records named it: {}",
            action.reason()
        );
        assert_eq!(action.state(), ActionState::NeedsOwnerInput);

        let request = resolution(action, OperationKey::CreateAccount);
        assert_eq!(
            request.preset.get("provider_account_id"),
            Some(&serde_json::Value::String("Shop One".to_owned())),
            "the printed string is the identifier the source prints, not a title"
        );
        assert!(
            !request.preset.contains_key("title"),
            "what the owner calls the account is his, and a rename must not stop the import"
        );
        let missing: Vec<&str> = request
            .missing
            .iter()
            .map(|input| input.pointer.as_str())
            .collect();
        assert_eq!(missing, vec!["/title", "/institution"]);
    }

    // --- The label is minted and the question is one he can answer (iaam-9i83)

    /// `provider` is worked out, and what he is asked is where the account is.
    ///
    /// **The whole of `iaam-9i83` in one item.** The label used to be published
    /// as a field he fills in with no question beside it, because there is no
    /// question a person can be asked about it: its only property is that it
    /// differs between sources. So the assertion is made in both directions —
    /// the derived field is preset and no longer his, and the field he is asked
    /// for is the one `CreateAccountRequest` was carrying beside it unasked.
    #[test]
    fn the_label_that_scopes_the_identifier_is_minted_and_the_institution_is_asked() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 3, 1)]);
        let request = resolution(named_by_document(&actions), OperationKey::CreateAccount);

        assert_eq!(
            request.preset.get("provider"),
            Some(&serde_json::Value::String("Example Bank".to_owned())),
            "the scope of a printed identifier is known to the reading and is not his to invent"
        );
        assert!(
            !request
                .missing
                .iter()
                .any(|missing| missing.pointer == "/provider"),
            "a value this instance can work out is worked out, not asked for"
        );

        let institution = request
            .missing
            .iter()
            .find(|missing| missing.pointer == "/institution")
            .expect("the question he can answer is published");
        assert_eq!(institution.provided_by, ProvidedBy::Owner);
        assert_eq!(
            institution.prompt.as_ref().map(OwnerPrompt::asked_by),
            Some(OperationKey::CreateAccount)
        );
    }

    /// The register of unasked questions is empty, and stays.
    ///
    /// It was emptied by the field ceasing to be his and not by a sentence being
    /// written for it, which is the outcome decision 0027 §5 held the register
    /// open for. The assertion is the emptiness; the argument for keeping the
    /// register is on the constant.
    #[test]
    fn no_field_the_owner_fills_in_is_still_waiting_on_a_decision() {
        assert!(
            QUESTIONS_UNDER_REVIEW.is_empty(),
            "a field is registered as unanswered: {QUESTIONS_UNDER_REVIEW:?}"
        );
    }

    // --- One decision over a set, and a field he may skip (iaam-hdr7, iaam-4fsw,
    // iaam-zxc6) ------------------------------------------------------------
    //
    // The observation, in one sentence: seven printed names became roughly
    // fifteen exchanges, because each item asked its two fields one at a time
    // and no item could say that its neighbours took the same answer.

    /// Every item of one reading, with its two owner fields.
    fn named_items(actions: &[Action]) -> Vec<&Action> {
        actions
            .iter()
            .filter(|action| action.kind() == ActionKind::CreateAccountNamedByDocument)
            .collect()
    }

    /// One field of the account this item would create.
    fn account_field<'a>(action: &'a Action, pointer: &str) -> &'a MissingInput {
        resolution(action, OperationKey::CreateAccount)
            .missing
            .iter()
            .find(|missing| missing.pointer == pointer)
            .unwrap_or_else(|| panic!("{} publishes no {pointer}", action.id()))
    }

    /// The field the call is accepted without says so, and its neighbour does not.
    ///
    /// **Both halves, because the flag is only worth having if it discriminates.**
    /// A queue that marked every field optional would be as mute as one that
    /// marked none, and the pair here is the pair the owner met: one field the
    /// account cannot be created without, and one it can — published side by
    /// side with nothing to tell them apart, so he was stopped for both.
    ///
    /// The consequence is asserted too, and it is the thing that makes the flag
    /// an offer rather than a fact nobody can act on: an optional question is
    /// put with a way past it, and what the way past costs is decision 0027's
    /// third obligation, which for this field is «nothing now, and in a year
    /// nothing will say where this account is».
    #[test]
    fn a_field_the_call_is_accepted_without_says_so_and_the_one_beside_it_does_not() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 3, 1)]);
        let action = named_by_document(&actions);

        assert!(
            !account_field(action, "/title").optional,
            "an account cannot be created with no name at all"
        );
        let institution = account_field(action, "/institution");
        assert!(
            institution.optional,
            "the account is created whether or not he says where it is held"
        );
        let question = institution
            .prompt
            .as_ref()
            .expect("the field he fills in carries its question")
            .question();
        assert!(
            question.consequence.contains("a year from now"),
            "an optional question is offered with what skipping it costs: {}",
            question.consequence
        );
    }

    /// One answer names every item it fills, and the value for this one.
    ///
    /// **The unit of his decision is not the unit of the item.** The items stay
    /// one per printed name, because completion is per name; what is added is
    /// that each of them says which of its neighbours the same answer settles,
    /// and what that answer would be here. The two proposals differ in exactly
    /// the way the shape has to admit: the institution is one value for all of
    /// them, and the title is one decision writing a different string into each
    /// request. A shape carrying only the first would have covered two of the
    /// fifteen exchanges and left the names asked one at a time.
    #[test]
    fn an_answer_he_gives_once_names_every_item_it_fills_and_the_value_for_this_one() {
        let actions = queue_wanting(
            &[],
            &[
                wanted("Shop One", 3, 1),
                wanted("Shop Two", 5, 1),
                wanted("Shop Three", 7, 1),
            ],
        );
        let items = named_items(&actions);
        assert_eq!(items.len(), 3);
        let every_id: Vec<&str> = items.iter().map(|action| action.id()).collect();

        for action in &items {
            for pointer in ["/title", "/institution"] {
                let proposal = account_field(action, pointer)
                    .proposal
                    .as_ref()
                    .unwrap_or_else(|| panic!("{} offers nothing for {pointer}", action.id()));
                assert_eq!(
                    proposal.covers,
                    every_id,
                    "{} answers for a set that is not the reading's",
                    action.id()
                );
                assert!(
                    proposal.covers.iter().any(|id| id == action.id()),
                    "a set that does not hold the item publishing it is not this item's answer"
                );
            }
        }

        assert_eq!(
            account_field(items[0], "/institution")
                .proposal
                .as_ref()
                .map(Proposal::value),
            Some("Example Bank"),
            "one institution for all of them, read from what the profile said it read"
        );
        let titles: Vec<&str> = items
            .iter()
            .map(|action| {
                account_field(action, "/title")
                    .proposal
                    .as_ref()
                    .map(Proposal::value)
                    .expect("a title is proposed")
            })
            .collect();
        assert_eq!(
            titles,
            vec!["Shop One", "Shop Two", "Shop Three"],
            "one decision, and a different string in each request"
        );
    }

    /// The proposal is a question and never a filled-in field.
    ///
    /// The distinction decision 0030 turned on and this decision leans on: a
    /// preset value is the request already answered and is never read out to
    /// him, so a value hidden there would settle the question by hiding it —
    /// which is the defect `iaam-9i83` closed. Both proposed values are
    /// therefore asserted absent from `preset`, and the printed string that *is*
    /// preset is asserted to be the identifier, exactly as decision 0004 wants
    /// it.
    #[test]
    fn a_proposed_value_is_read_out_to_him_and_never_preset() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 3, 1), wanted("Shop Two", 5, 1)]);
        let request = resolution(named_by_document(&actions), OperationKey::CreateAccount);

        assert!(
            !request.preset.contains_key("institution") && !request.preset.contains_key("title"),
            "a proposal that reached the preset would be an answer he never saw: {:?}",
            request.preset
        );
        assert_eq!(
            request.preset.get("provider_account_id"),
            Some(&serde_json::Value::String("Shop One".to_owned())),
            "what is preset is the identifier the source prints, which is what makes the \
             proposed title safe to propose"
        );
    }

    /// One printed name is one question, and no set is published for it.
    ///
    /// `ActionTarget::from_options` normalises a set of one resolution away for
    /// this reason, and it holds a level down: «here is a set of one» would make
    /// a caller take a set apart to find the fact it already had, and would put
    /// a sentence about several accounts to the owner about one.
    #[test]
    fn one_printed_name_is_offered_no_set_wide_answer() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 3, 1)]);
        let action = named_by_document(&actions);

        for pointer in ["/title", "/institution"] {
            assert!(
                account_field(action, pointer).proposal.is_none(),
                "a set of one is not a set: {pointer}"
            );
        }
    }

    /// Two institutions' names are two sets, and neither answers for the other.
    ///
    /// The set is the reading's institution and not the item kind, and this is
    /// the case that decides it: an owner who conveys two institutions'
    /// statements before working his queue would otherwise be asked one question
    /// over both, and «they are all from that bank» would be false of half of
    /// it. Two sentences settling four names is the right answer; one sentence
    /// that is wrong about two of them is not.
    #[test]
    fn two_institutions_are_two_sets_and_neither_answers_for_the_other() {
        let broker = |printed: &str| AccountNamedByDocument {
            issuer: Some("Example Broker".to_owned()),
            ..wanted(printed, 2, 1)
        };
        let actions = queue_wanting(
            &[],
            &[
                wanted("Shop One", 3, 1),
                broker("0001"),
                wanted("Shop Two", 5, 1),
                broker("0002"),
            ],
        );
        let items = named_items(&actions);
        assert_eq!(items.len(), 4);

        for action in &items {
            let proposal = account_field(action, "/institution")
                .proposal
                .as_ref()
                .expect("each of the four is in a set of two");
            assert_eq!(proposal.covers.len(), 2, "{}", action.id());
            for id in &proposal.covers {
                assert!(
                    id.contains(proposal.value()),
                    "a set may only name items read as its own institution's: {id}"
                );
            }
        }
    }

    /// A name with no institution behind it joins no set rather than a neighbour's.
    ///
    /// **Refused whole, and this is the shape it takes here.** The ground for
    /// both proposals is that the reading said which institution printed the
    /// string; a name whose kept document this instance can no longer place says
    /// nothing of the sort. Folding it into a neighbour's set would put a claim
    /// about one bank's document to him over a name that came from nobody knows
    /// where, and would do it inside an answer he gives in one word. So it is in
    /// no set, it gets no proposal, and it goes on being asked on its own —
    /// which is also why nothing is preset on it, one field over.
    #[test]
    fn a_name_with_no_institution_behind_it_joins_no_set_rather_than_a_neighbours() {
        let actions = queue_wanting(
            &[],
            &[
                wanted("Shop One", 3, 1),
                AccountNamedByDocument {
                    issuer: None,
                    ..wanted("Shop Two", 5, 1)
                },
                wanted("Shop Three", 7, 1),
            ],
        );
        let items = named_items(&actions);
        assert_eq!(items.len(), 3);

        let orphan = items
            .iter()
            .find(|action| action.reason().contains("Shop Two"))
            .expect("the name whose reading is gone is still an item");
        assert!(
            account_field(orphan, "/institution").proposal.is_none()
                && account_field(orphan, "/title").proposal.is_none(),
            "a name with no ground takes no answer given over one"
        );

        for action in items
            .iter()
            .filter(|action| !action.reason().contains("Shop Two"))
        {
            let proposal = account_field(action, "/institution")
                .proposal
                .as_ref()
                .expect("the two the reading placed are a set");
            assert_eq!(
                proposal.covers.len(),
                2,
                "the set names the items it fills and no others"
            );
            assert!(
                !proposal.covers.iter().any(|id| id.ends_with("Shop Two")),
                "the item that cannot take the answer is not named as taking it"
            );
        }
    }

    /// A name he has declared is not his is in no set, because it asks for neither.
    ///
    /// Membership is asking the field and not sharing the kind. His statement
    /// changed what the item offers — the withdrawal of that statement, and
    /// nothing else — so there is no title and no institution on it for one
    /// answer to fill, and a set that counted it would be telling him he was
    /// answering for an account he had already said was not his.
    #[test]
    fn a_name_he_has_declared_is_not_his_is_in_no_set() {
        let actions = queue_wanting(
            &[],
            &[
                wanted("Shop One", 3, 1),
                AccountNamedByDocument {
                    declined: Some("a shop I pay".to_owned()),
                    ..wanted("Shop Two", 5, 1)
                },
                wanted("Shop Three", 7, 1),
            ],
        );
        let items = named_items(&actions);
        assert_eq!(items.len(), 3);

        let declared = items
            .iter()
            .find(|action| action.category() == ActionCategory::Informational)
            .expect("the declared name is still in the queue, as a fact");
        assert!(
            declared
                .target()
                .resolutions()
                .iter()
                .all(|(operation, _)| *operation == OperationKey::RecordAccountNameDisposition),
            "the declared name offers the withdrawal and nothing else"
        );

        for action in items
            .iter()
            .filter(|action| action.category() != ActionCategory::Informational)
        {
            let proposal = account_field(action, "/title")
                .proposal
                .as_ref()
                .expect("the two still being asked about are a set");
            assert_eq!(proposal.covers.len(), 2, "{}", action.id());
        }
    }

    /// One of each proposal, in the shape [`specimens`] takes and for its reason.
    fn proposal_specimens() -> Vec<ProposedAnswer> {
        vec![
            ProposedAnswer::AccountInstitutionOfIssuer {
                issuer: "Example Bank".to_owned(),
            },
            ProposedAnswer::AccountTitleAsPrinted {
                printed: "Shop One".to_owned(),
            },
        ]
    }

    /// The name of one proposal, and the `match` that keeps the list honest.
    fn proposal_name(proposed: &ProposedAnswer) -> &'static str {
        match proposed {
            ProposedAnswer::AccountInstitutionOfIssuer { .. } => "AccountInstitutionOfIssuer",
            ProposedAnswer::AccountTitleAsPrinted { .. } => "AccountTitleAsPrinted",
        }
    }

    /// A proposal is put to him as a question, in the register the others are.
    ///
    /// **The whole reason a proposal is not a preset.** It exists to be read
    /// out, so it answers to the owner's own rule exactly as every other
    /// question does — no internal words, what it is for, and what turns on the
    /// answer — and the same check is run over it rather than a laxer one
    /// written for it. What its consequence has to carry that the others do not
    /// is the cost of the width itself: one answer decides for every account
    /// named, and that is the half he would otherwise find out afterwards.
    #[test]
    fn a_proposal_is_a_question_and_says_what_answering_it_once_decides() {
        let mut asks: BTreeSet<String> = BTreeSet::new();
        for proposed in proposal_specimens() {
            let question = proposed.question(4);
            assert_eq!(
                puts_a_question_to_a_person(&question),
                Ok(()),
                "{} does not put a question to a person: {question:?}",
                proposal_name(&proposed)
            );
            assert!(
                !question.ask.contains(proposed.pointer())
                    && !question.consequence.contains(proposed.pointer()),
                "{} shows the owner the pointer it fills",
                proposal_name(&proposed)
            );
            assert!(
                question.ask.contains('4') && question.consequence.contains('4'),
                "{} does not say how many accounts one answer decides for: {question:?}",
                proposal_name(&proposed)
            );
            assert!(
                asks.insert(question.ask.clone()),
                "{} asks in another proposal's words",
                proposal_name(&proposed)
            );
        }
        assert_eq!(asks.len(), 2, "a proposal was added and is not swept");
    }

    /// A proposal names the field it fills and the call that field belongs to.
    ///
    /// The sweep decision 0027 runs over questions, run over proposals, and for
    /// the same reason: a pointer is not an identity, so an answer offered for
    /// `/title` of one call could be offered on a resolution that calls
    /// something else and would be a value for a field that request does not
    /// have. The set is checked here too, because a set is only refusable whole
    /// if it names what it reaches: a proposal that did not name the item
    /// publishing it, or that named one item, is not an answer given over a set.
    #[test]
    fn every_proposal_names_the_field_it_fills_and_the_items_it_reaches() {
        let mut seen = 0_usize;
        for action in every_queue_item() {
            for (operation, request) in action.target().resolutions() {
                for missing in &request.missing {
                    let Some(proposal) = &missing.proposal else {
                        continue;
                    };
                    seen += 1;
                    assert_eq!(
                        proposal.proposed.pointer(),
                        missing.pointer,
                        "{} offers an answer for {} on {}",
                        action.id(),
                        proposal.proposed.pointer(),
                        missing.pointer
                    );
                    assert_eq!(
                        proposal.proposed.asked_by(),
                        operation,
                        "{} offers an answer to {operation:?} for a field of {:?}",
                        action.id(),
                        proposal.proposed.asked_by()
                    );
                    assert!(
                        missing.prompt.is_some(),
                        "{} proposes a value for a field it puts no question about",
                        action.id()
                    );
                    assert!(
                        proposal.covers.len() > 1
                            && proposal.covers.iter().any(|id| id == action.id()),
                        "{} publishes a set that is not a set it belongs to: {:?}",
                        action.id(),
                        proposal.covers
                    );
                }
            }
        }
        assert!(
            seen > 0,
            "the heap the sweep runs over offers nothing over a set, so this proved nothing"
        );
    }

    /// Two institutions printing one string are two accounts, not one.
    ///
    /// The fold is per name **and** per institution, and this is why: the item
    /// mints the label that keeps two sources' identifiers apart, so folding two
    /// sources into one item would mint one label for both — which is the
    /// collision `provider` exists to prevent, reached from the other side.
    #[test]
    fn one_string_printed_by_two_institutions_is_two_items() {
        let actions = queue_wanting(
            &[],
            &[
                AccountNamedByDocument {
                    issuer: Some("Example Bank".to_owned()),
                    ..wanted("0001", 3, 1)
                },
                AccountNamedByDocument {
                    issuer: Some("Example Broker".to_owned()),
                    ..wanted("0001", 4, 1)
                },
            ],
        );
        let named: Vec<&Action> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::CreateAccountNamedByDocument)
            .collect();
        assert_eq!(named.len(), 2);
        let ids: BTreeSet<&str> = named.iter().map(|action| action.id()).collect();
        assert_eq!(
            ids.len(),
            2,
            "two items must not share an identity: {ids:?}"
        );
        let scopes: Vec<Option<&serde_json::Value>> = named
            .iter()
            .copied()
            .map(|action| {
                resolution(action, OperationKey::CreateAccount)
                    .preset
                    .get("provider")
            })
            .collect();
        assert_ne!(
            scopes[0], scopes[1],
            "one label for two sources is the collision the label exists to prevent"
        );
    }

    /// A name whose document is gone presets no half of an identity.
    ///
    /// `create_account` refuses half an identity, so an item that presets the
    /// printed string with no scope publishes a request the route rejects on
    /// arrival. The state is not reachable through any route — a kept document
    /// is immutable and is written before the names it could not place are — and
    /// it is written out because a `None` that cannot be reached still has to
    /// mean something.
    #[test]
    fn a_name_whose_document_is_gone_presets_no_half_of_an_identity() {
        let actions = queue_wanting(
            &[],
            &[AccountNamedByDocument {
                issuer: None,
                ..wanted("Shop One", 3, 1)
            }],
        );
        let request = resolution(named_by_document(&actions), OperationKey::CreateAccount);
        assert!(
            !request.preset.contains_key("provider")
                && !request.preset.contains_key("provider_account_id"),
            "half an identity is refused by the route it is addressed to: {:?}",
            request.preset
        );
    }

    // --- A printed name he can say is not his (iaam-mk1n) -------------------

    /// The item publishes the answer he actually has.
    ///
    /// The defect: the item published `create_account` and nothing else, so «this
    /// name is not an account of mine» was unrepresentable and the one act that
    /// closed it was the one he had decided against. An agent working a real
    /// import reasoned its way to the hole, went looking for a route no item
    /// mentioned, and left the items behind without saying so.
    #[test]
    fn a_name_a_document_printed_can_be_said_not_to_be_his() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 220, 1)]);
        let action = named_by_document(&actions);

        let offered: Vec<OperationKey> = action
            .target()
            .resolutions()
            .into_iter()
            .map(|(operation, _)| operation)
            .collect();
        assert_eq!(
            offered,
            vec![
                OperationKey::CreateAccount,
                OperationKey::RecordAccountNameDisposition
            ],
            "ordered and not ranked: a name his own statement printed is usually his"
        );

        let request = resolution(action, OperationKey::RecordAccountNameDisposition);
        assert_eq!(
            request.preset.get("printed"),
            Some(&serde_json::Value::String("Shop One".to_owned())),
            "the name is the whole subject of the call and there is no account to identify it by"
        );
        assert_eq!(
            request.preset.get("disposition"),
            Some(&serde_json::Value::String("not_mine".to_owned())),
            "an option that left the caller to guess the word publishes a route, not a resolution"
        );
        let missing: Vec<&str> = request
            .missing
            .iter()
            .map(|input| input.pointer.as_str())
            .collect();
        assert_eq!(
            missing,
            vec!["/reason"],
            "a name ruled out without a reason is indistinguishable from one nobody looked at"
        );
    }

    /// Once he has said so, the name stops being work and stays a fact.
    ///
    /// Both halves matter. It stops being `RequiredForGoal`, because required
    /// work is what an owner has not done and this is what he decided — left as
    /// it was, every report he asked for would go on being flagged short on
    /// account of a decision already made. And it does not disappear, because
    /// the records are still refused and still in no report, and a queue that
    /// said nothing about them would hide the consequence of his own decision.
    #[test]
    fn a_declared_name_becomes_a_statement_of_fact_and_not_work() {
        let actions = queue_wanting(
            &[],
            &[AccountNamedByDocument {
                declined: Some("a shop I pay".to_owned()),
                ..wanted("Shop One", 220, 1)
            }],
        );
        let action = named_by_document(&actions);

        assert_eq!(action.category(), ActionCategory::Informational);
        assert!(
            action.category().goals().is_empty(),
            "a decision he made is not work standing between him and a report"
        );
        assert!(
            action.reason().contains("220") && action.reason().contains("a shop I pay"),
            "the queue still says how many records are refused, and why: {}",
            action.reason()
        );

        let request = resolution(action, OperationKey::RecordAccountNameDisposition);
        assert_eq!(
            request.preset.get("disposition"),
            Some(&serde_json::Value::String("undecided".to_owned())),
            "the way out of a settled item is the withdrawal of the statement"
        );
        assert!(
            request.missing.is_empty(),
            "withdrawing leaves nothing for a reason to explain"
        );
        assert_eq!(
            action.state(),
            ActionState::NeedsOwnerInput,
            "an agent may not withdraw a judgement it could not have made"
        );
    }

    /// The declaration is beaten by the directory, and is never asked instead.
    ///
    /// The completion is `directory.resolve` and nothing else, exactly as it was:
    /// a stored verdict would say «missing» about an account created an hour
    /// later, and the same argument applies to a statement, which says what he
    /// decided and never what is true of his accounts now. So a name an account
    /// answers to raises nothing whether or not a statement stands against it.
    #[test]
    fn an_account_that_answers_to_a_declared_name_beats_the_declaration() {
        let directory: iaam_ingest::csv_source::AccountNames =
            [iaam_ingest::csv_source::AccountEntry::titled(
                "Shop One",
                AccountId::new_random(),
            )]
            .into_iter()
            .collect();
        assert!(
            account_named_by_document_completion(&directory, "Shop One"),
            "completion is asked of the directory and of nothing else"
        );
        assert!(!account_named_by_document_gap(&directory, "Shop One"));
    }

    /// One item per account, not one per document.
    ///
    /// Two statements of one bank naming the same unknown account are one
    /// account to create. The counts are summed over the readings, because that
    /// is what «how much of my history is unread because of this» asks.
    #[test]
    fn two_documents_naming_one_account_raise_one_item() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 12, 2)]);
        let named: Vec<&Action> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::CreateAccountNamedByDocument)
            .collect();
        assert_eq!(named.len(), 1);
        assert!(
            named[0].reason().contains("2 kept documents"),
            "{}",
            named[0].reason()
        );
    }

    /// Each name is its own item, with its own identity.
    ///
    /// An unscoped identity would give seven accounts one item, and an agent
    /// deduplicating by `id` — which is what `id` is for — would act on one of
    /// them and believe it was done.
    #[test]
    fn each_named_account_is_its_own_item() {
        let actions = queue_wanting(&[], &[wanted("Shop One", 3, 1), wanted("Shop Two", 4, 1)]);
        let ids: BTreeSet<&str> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::CreateAccountNamedByDocument)
            .map(Action::id)
            .collect();
        assert_eq!(ids.len(), 2, "two accounts, two items: {ids:?}");
    }

    /// The blocking item stands beside them and says how the names were learned.
    ///
    /// It cannot name an account itself — in an empty instance nothing has been
    /// read, so there is no name to publish — but it must not be the dead end it
    /// was: the sentence has to say that the question has an answer and that
    /// handing a document over is what gives it.
    #[test]
    fn the_first_account_item_says_how_to_find_out_which() {
        let actions = queue_wanting(&[], &[]);
        let action = actions
            .iter()
            .find(|action| action.kind() == ActionKind::CreateFirstAccount)
            .expect("an empty directory raises the blocking item");
        let reason = action.reason();
        assert!(
            reason.contains("import session") && reason.contains("document"),
            "the item must name the act that publishes the account names: {reason}"
        );
    }

    /// A name the directory now places raises nothing.
    ///
    /// This is the whole reason the stored fact is a transcription and the
    /// verdict is recomputed: an account created after the reading closes the
    /// item without the document being read again, and a queue that kept
    /// publishing it would be one the owner learns to ignore.
    #[tokio::test]
    async fn an_account_that_answers_to_the_name_closes_the_item() {
        let owner = OwnerId::new_random();
        let store = store();
        let session = store
            .open_import_session(owner, None, None, None)
            .await
            .expect("session");
        let document = RawHash::parse(&"d".repeat(64)).expect("raw hash");
        store
            .record_unresolved_accounts(
                owner,
                document,
                session.id,
                vec![
                    UnresolvedAccountName {
                        printed: "Shop One".to_owned(),
                        records: 3,
                    },
                    UnresolvedAccountName {
                        printed: "Shop Two".to_owned(),
                        records: 1,
                    },
                ],
            )
            .await
            .expect("record");

        let before: Vec<String> = frontier(owner, &store, &store)
            .await
            .expect("frontier")
            .iter()
            .filter(|action| action.kind() == ActionKind::CreateAccountNamedByDocument)
            .map(|action| action.id().to_owned())
            .collect();
        assert_eq!(before.len(), 2, "{before:?}");

        store
            .upsert_account(owner, named("Shop One"))
            .await
            .expect("account");

        let after: Vec<String> = frontier(owner, &store, &store)
            .await
            .expect("frontier")
            .iter()
            .filter(|action| action.kind() == ActionKind::CreateAccountNamedByDocument)
            .map(|action| action.id().to_owned())
            .collect();
        assert_eq!(
            after.len(),
            1,
            "the account the owner created must close its own item: {after:?}"
        );
        assert!(after[0].contains("Shop Two"), "{after:?}");
    }

    /// A statement he makes settles the item, and withdrawing it brings it back.
    ///
    /// Through the store and the frontier rather than through the builder, so
    /// what is asserted is the round trip: the queue reads his statement where it
    /// reads the names, and it is the same item — same identity, same string,
    /// same counts — graded differently. The withdrawal is asserted too, because
    /// a statement he cannot take back is not a statement, it is a deletion.
    #[tokio::test]
    async fn saying_a_name_is_not_his_settles_the_item_and_withdrawing_it_returns() {
        let owner = OwnerId::new_random();
        let store = store();
        let session = store
            .open_import_session(owner, None, None, None)
            .await
            .expect("session");
        let document = RawHash::parse(&"f".repeat(64)).expect("raw hash");
        store
            .record_unresolved_accounts(
                owner,
                document,
                session.id,
                vec![UnresolvedAccountName {
                    printed: "Shop One".to_owned(),
                    records: 3,
                }],
            )
            .await
            .expect("record");

        let required = frontier(owner, &store, &store).await.expect("frontier");
        let before = named_by_document(&required);
        assert!(matches!(
            before.category(),
            ActionCategory::RequiredForGoal(_)
        ));
        let identity = before.id().to_owned();

        store
            .decline_account_name(
                owner,
                DeclinedAccountNameView {
                    printed: "Shop One".to_owned(),
                    reason: "a shop I pay".to_owned(),
                },
            )
            .await
            .expect("declined");

        let settled = frontier(owner, &store, &store).await.expect("frontier");
        let after = named_by_document(&settled);
        assert_eq!(
            after.id(),
            identity,
            "it is the same name and the same item, and only its grading moved"
        );
        assert_eq!(after.category(), ActionCategory::Informational);
        assert!(
            after.reason().contains("a shop I pay"),
            "the queue says why those records are refused: {}",
            after.reason()
        );

        store
            .withdraw_declined_account_name(owner, "Shop One".to_owned())
            .await
            .expect("withdrawn");

        let asked_again = frontier(owner, &store, &store).await.expect("frontier");
        assert!(matches!(
            named_by_document(&asked_again).category(),
            ActionCategory::RequiredForGoal(_)
        ));
    }

    /// A second reading of one document replaces what the first recorded.
    ///
    /// Two readings are two answers to the same question against a directory
    /// that moved, and the later one is the current one. Adding to the set
    /// instead would count one document's records twice and leave a name nobody
    /// can close.
    #[tokio::test]
    async fn a_second_reading_replaces_what_the_first_recorded() {
        let owner = OwnerId::new_random();
        let store = store();
        let session = store
            .open_import_session(owner, None, None, None)
            .await
            .expect("session");
        let document = RawHash::parse(&"e".repeat(64)).expect("raw hash");
        store
            .record_unresolved_accounts(
                owner,
                document.clone(),
                session.id,
                vec![UnresolvedAccountName {
                    printed: "Shop One".to_owned(),
                    records: 3,
                }],
            )
            .await
            .expect("first reading");
        store
            .record_unresolved_accounts(owner, document, session.id, Vec::new())
            .await
            .expect("second reading");

        assert!(
            frontier(owner, &store, &store)
                .await
                .expect("frontier")
                .iter()
                .all(|action| action.kind() != ActionKind::CreateAccountNamedByDocument),
            "an empty reading is the statement that every account was placed"
        );
    }

    #[tokio::test]
    async fn creating_an_account_satisfies_the_account_completion_condition() {
        let owner = OwnerId::new_random();
        let store = store();
        let new_account = account();
        store
            .upsert_account(owner, new_account.clone())
            .await
            .expect("account");

        let accounts = store.list_accounts(owner).await.expect("accounts");
        assert!(!accounts.is_empty());
        assert!(account_completion(&accounts));
        assert!(
            frontier(owner, &store, &store)
                .await
                .expect("frontier")
                .iter()
                .all(|action| action.kind() != ActionKind::CreateFirstAccount)
        );
    }

    #[tokio::test]
    async fn an_owner_with_accounts_and_no_contours_isoffered_the_first_contour_action() {
        let owner = OwnerId::new_random();
        let store = store();
        let new_account = account();
        store
            .upsert_account(owner, new_account.clone())
            .await
            .expect("account");

        let actions = frontier(owner, &store, &store).await.expect("frontier");
        let action = actions
            .iter()
            .find(|action| action.kind() == ActionKind::CreateFirstContour)
            .expect("first contour action");
        assert_eq!(
            action.category(),
            ActionCategory::required_for(ActionKind::CreateFirstContour)
        );
        assert_eq!(action.state(), ActionState::NeedsOwnerInput);
        let ActionTarget::Operation { operation, request } = action.target() else {
            panic!("first contour needs an operation target");
        };
        assert_eq!(*operation, OperationKey::CreateContour);
        assert_eq!(request.missing.len(), 2);
        assert!(request.missing.iter().any(|missing| {
            missing.pointer == "/title"
                && missing.provided_by == ProvidedBy::Owner
                && missing.candidates.is_none()
        }));
        let accounts_missing = request
            .missing
            .iter()
            .find(|missing| missing.pointer == "/accounts")
            .expect("account selection input");
        assert_eq!(accounts_missing.provided_by, ProvidedBy::Owner);
        assert_eq!(
            accounts_missing.candidates.as_deref(),
            Some(
                [AccountCandidate {
                    id: new_account.id,
                    title: new_account.title.clone(),
                    institution: new_account.institution.clone(),
                }]
                .as_slice()
            )
        );
    }

    #[tokio::test]
    async fn creating_a_contour_satisfies_the_contour_completion_condition() {
        let owner = OwnerId::new_random();
        let store = store();
        let new_account = account();
        store
            .upsert_account(owner, new_account.clone())
            .await
            .expect("account");
        let contour = ContourId::new_random();
        store
            .insert_contour_version(
                owner,
                ContourDefinition::new(contour, ContourVersion(1), [new_account.id]),
                "Main".into(),
                vec![new_account.id],
            )
            .await
            .expect("contour");

        let contours = store.list_contours(owner).await.expect("contours");
        assert!(!contours.is_empty());
        assert!(contour_completion(&contours));
        assert!(
            frontier(owner, &store, &store)
                .await
                .expect("frontier")
                .iter()
                .all(|action| action.kind() != ActionKind::CreateFirstContour)
        );
    }

    /// Two contours already exist and a third account belongs to neither.
    ///
    /// The queue has to name that account. `!contours.is_empty()` cannot: it is
    /// satisfied by the first contour and says nothing for the rest of the
    /// instance's life, which is how a second bank's accounts import correctly
    /// and stay out of every report with nothing anywhere saying so.
    #[tokio::test]
    async fn an_account_in_no_contour_is_named_even_though_contours_exist() {
        let owner = OwnerId::new_random();
        let store = store();
        let first = named("Main");
        let second = named("Savings");
        let orphan = named("Second Bank Current");
        for account in [&first, &second, &orphan] {
            store
                .upsert_account(owner, account.clone())
                .await
                .expect("account");
        }
        for (index, member) in [first.id, second.id].into_iter().enumerate() {
            let contour = ContourId::new_random();
            store
                .insert_contour_version(
                    owner,
                    ContourDefinition::new(contour, ContourVersion(1), [member]),
                    format!("Contour {index}"),
                    vec![member],
                )
                .await
                .expect("contour");
        }

        let actions = frontier(owner, &store, &store).await.expect("frontier");
        assert!(
            actions.iter().any(|action| action
                .target()
                .resolutions()
                .iter()
                .any(|(operation, _)| *operation == OperationKey::AddContourVersion)),
            "nothing in the queue offers contour membership for the account that has none: {actions:?}"
        );

        let named: Vec<&Action> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::AccountScopeUndecided)
            .collect();
        assert_eq!(
            named.len(),
            1,
            "exactly the account in no contour is named: {actions:?}"
        );
        let action = named[0];
        // The subject is a typed field, not a substring of the sentence: a
        // caller narrowing the queue to one account must not have to parse prose.
        assert_eq!(
            action.subject().and_then(ActionSubject::account),
            Some(orphan.id)
        );
        assert_eq!(
            action.category(),
            ActionCategory::required_for(ActionKind::AccountScopeUndecided)
        );
        assert_eq!(action.state(), ActionState::NeedsOwnerInput);
        assert_eq!(action.required_scope(), Some(Scope::Owner));
        // The act is «add it to one of the contours that exist», not «create a
        // contour»: naming the creating operation here is what let an agent
        // answer this item with a second perimeter.
        let published = action.target().resolutions();
        let (_, request) = published
            .iter()
            .find(|(operation, _)| *operation == OperationKey::AddContourVersion)
            .expect("the account scope action offers contour membership");
        // Two contours exist, so which one the account belongs in is the owner's
        // choice and the composition cannot be written out without it.
        assert!(
            request.preset.is_empty(),
            "the contour cannot be chosen for the owner: {:?}",
            request.preset
        );
        assert!(
            request
                .missing
                .iter()
                .any(|missing| missing.pointer == "/contour")
        );
        let accounts = request
            .missing
            .iter()
            .find(|missing| missing.pointer == "/accounts")
            .expect("account selection input");
        assert!(
            accounts
                .candidates
                .as_ref()
                .expect("candidates")
                .iter()
                .any(|candidate| candidate.id == orphan.id)
        );
    }

    /// The one-contour case, which is the one the reporter actually hit.
    ///
    /// With a single contour there is no doubt which one «add this account»
    /// means, so the call is written out in full: the contour the route names in
    /// its path, and the whole composition it is to have. Nothing is left for
    /// the owner to type — the title the contour already carries is carried
    /// forward — and the item stays `NeedsOwnerInput` because drawing the
    /// perimeter is his judgement, not because a field is blank.
    #[tokio::test]
    async fn with_one_contour_the_membership_call_is_written_out() {
        let owner = OwnerId::new_random();
        let store = store();
        let member = named("Main");
        let orphan = named("Second Bank Current");
        for account in [&member, &orphan] {
            store
                .upsert_account(owner, account.clone())
                .await
                .expect("account");
        }
        let contour = ContourId::new_random();
        store
            .insert_contour_version(
                owner,
                ContourDefinition::new(contour, ContourVersion(1), [member.id]),
                "Household".into(),
                vec![member.id],
            )
            .await
            .expect("contour");

        let actions = frontier(owner, &store, &store).await.expect("frontier");
        let action = actions
            .iter()
            .find(|action| action.kind() == ActionKind::AccountScopeUndecided)
            .expect("the orphaned account is named");
        let published = action.target().resolutions();
        let (_, request) = published
            .iter()
            .find(|(operation, _)| *operation == OperationKey::AddContourVersion)
            .expect("the account scope action offers contour membership");
        assert_eq!(
            request.preset.get("contour"),
            Some(&serde_json::Value::from(contour.0.to_string()))
        );
        // The whole composition, not just the new account: a contour version is
        // a complete membership list, and sending only the account being added
        // would drop every existing member from the contour.
        let mut expected = vec![member.id.inner().to_string(), orphan.id.inner().to_string()];
        expected.sort();
        assert_eq!(
            request.preset.get("accounts"),
            Some(&serde_json::Value::Array(
                expected.into_iter().map(serde_json::Value::from).collect()
            ))
        );
        assert!(
            request.missing.is_empty(),
            "the owner is asked for a judgement, not for a title he already gave: {:?}",
            request.missing
        );
    }

    /// The reason names two ways out, so the contract must publish two.
    ///
    /// The invariant this pins is that the sentence and the target cannot drift:
    /// an agent reads `target` as the contract — that is what `target` is for —
    /// and while the item offered contour membership alone, the only way it
    /// could act on «or record that it is outside the perimeter» was to read the
    /// prose and go hunting through the specification for a route no queue item
    /// mentioned. Prose cannot be asserted mechanically; two published
    /// operations, each with the request that closes the item its own way, can.
    #[tokio::test]
    async fn the_scope_item_publishes_both_ways_it_can_be_closed() {
        let owner = OwnerId::new_random();
        let store = store();
        let member = named("Main");
        let orphan = named("Savings");
        for account in [&member, &orphan] {
            store
                .upsert_account(owner, account.clone())
                .await
                .expect("account");
        }
        let contour = ContourId::new_random();
        store
            .insert_contour_version(
                owner,
                ContourDefinition::new(contour, ContourVersion(1), [member.id]),
                "Household".into(),
                vec![member.id],
            )
            .await
            .expect("contour");

        let actions = frontier(owner, &store, &store).await.expect("frontier");
        let action = actions
            .iter()
            .find(|action| action.kind() == ActionKind::AccountScopeUndecided)
            .expect("the orphaned account is named");

        let published = action.target().resolutions();
        assert_eq!(
            published.len(),
            2,
            "the sentence names two ways to close this item and the contract publishes {}: {:?}",
            published.len(),
            action.target()
        );

        // Each option carries its own plan, and the two plans are not the same
        // plan: what a contour version wants is a composition, and what an
        // exclusion wants is a reason.
        let (_, membership) = published
            .iter()
            .find(|(operation, _)| *operation == OperationKey::AddContourVersion)
            .expect("contour membership is one way out");
        assert_eq!(
            membership.preset.get("contour"),
            Some(&serde_json::Value::from(contour.0.to_string())),
            "the membership option keeps its own written-out request"
        );
        assert!(
            membership
                .missing
                .iter()
                .all(|missing| missing.pointer != "/reason"),
            "a reason is not a field of the membership call: {:?}",
            membership.missing
        );

        let (_, exclusion) = published
            .iter()
            .find(|(operation, _)| *operation == OperationKey::RecordAccountScope)
            .expect("ruling the account outside the perimeter is the other way out");
        assert_eq!(
            exclusion.preset.get("disposition"),
            Some(&serde_json::Value::from("outside")),
            "an option that left the disposition to be guessed publishes a route, not a resolution"
        );
        let reason = exclusion
            .missing
            .iter()
            .find(|missing| missing.pointer == "/reason")
            .expect("the reason is a required field of the exclusion option");
        assert_eq!(
            reason.provided_by,
            ProvidedBy::Owner,
            "why an account is outside the perimeter is the owner's to say"
        );
    }

    /// A new account reopens the goal, which `!contours.is_empty()` cannot.
    #[tokio::test]
    async fn a_new_account_reopens_the_goal_although_contours_exist() {
        let owner = OwnerId::new_random();
        let store = store();
        let member = named("Main");
        store
            .upsert_account(owner, member.clone())
            .await
            .expect("account");
        let contour = ContourId::new_random();
        store
            .insert_contour_version(
                owner,
                ContourDefinition::new(contour, ContourVersion(1), [member.id]),
                "Household".into(),
                vec![member.id],
            )
            .await
            .expect("contour");
        assert!(
            frontier(owner, &store, &store)
                .await
                .expect("frontier")
                .iter()
                .all(|action| action.kind() != ActionKind::AccountScopeUndecided),
            "the placed account must not be nagged about"
        );

        let arrival = named("Second Bank Current");
        store
            .upsert_account(owner, arrival.clone())
            .await
            .expect("account");

        let reopened = frontier(owner, &store, &store).await.expect("frontier");
        assert_eq!(
            reopened
                .iter()
                .filter(|action| action.kind() == ActionKind::AccountScopeUndecided)
                .map(|action| action.subject().and_then(ActionSubject::account))
                .collect::<Vec<_>>(),
            vec![Some(arrival.id)]
        );
    }

    /// The third state, and the reason «every account must be in a contour» is
    /// the wrong predicate: an account can be outside the perimeter on purpose.
    #[tokio::test]
    async fn an_account_ruled_outside_the_perimeter_raises_nothing() {
        let owner = OwnerId::new_random();
        let store = store();
        let member = named("Main");
        let outside = named("Shop One");
        for account in [&member, &outside] {
            store
                .upsert_account(owner, account.clone())
                .await
                .expect("account");
        }
        let contour = ContourId::new_random();
        store
            .insert_contour_version(
                owner,
                ContourDefinition::new(contour, ContourVersion(1), [member.id]),
                "Household".into(),
                vec![member.id],
            )
            .await
            .expect("contour");
        store
            .record_account_scope_exclusion(
                owner,
                AccountScopeExclusionView {
                    account: outside.id,
                    reason: "A counterparty's account, not the owner's money.".into(),
                },
            )
            .await
            .expect("exclusion");

        let actions = frontier(owner, &store, &store).await.expect("frontier");
        assert!(
            actions
                .iter()
                .all(|action| action.kind() != ActionKind::AccountScopeUndecided),
            "a decided account raises nothing: {actions:?}"
        );

        // Withdrawing the statement returns it to awaiting a decision, rather
        // than leaving it silently decided for ever.
        store
            .clear_account_scope_exclusion(owner, outside.id)
            .await
            .expect("cleared");
        assert_eq!(
            frontier(owner, &store, &store)
                .await
                .expect("frontier")
                .iter()
                .filter(|action| action.kind() == ActionKind::AccountScopeUndecided)
                .map(|action| action.subject().and_then(ActionSubject::account))
                .collect::<Vec<_>>(),
            vec![Some(outside.id)]
        );
    }

    #[test]
    fn the_three_scope_states_are_read_from_the_two_places_that_hold_them() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let undecided = AccountId::new_random();
        let contours = [ContourView {
            id: ContourId::new_random(),
            version: ContourVersion(1),
            title: "Household".into(),
            accounts: vec![inside],
        }];
        let exclusions = [AccountScopeExclusionView {
            account: outside,
            reason: "Closed years ago.".into(),
        }];

        assert_eq!(
            account_scope(inside, &contours, &exclusions),
            AccountScope::Inside
        );
        assert_eq!(
            account_scope(outside, &contours, &exclusions),
            AccountScope::Outside
        );
        assert_eq!(
            account_scope(undecided, &contours, &exclusions),
            AccountScope::Undecided
        );
        assert!(account_scope_gap(undecided, &contours, &exclusions));
        assert!(!account_scope_gap(inside, &contours, &exclusions));
        assert!(!account_scope_gap(outside, &contours, &exclusions));
        // Eligibility is separate from the gap: with no contour to place it in,
        // `first_contour_action` already asks the question for every account.
        assert!(!account_scope_eligibility(&[]));
        assert!(account_scope_eligibility(&contours));
    }

    /// One way out has one encoding, whichever side computed the set.
    ///
    /// A list holding a single resolution would publish a transport shape that
    /// `operation` already publishes, and the two would drift: a consumer would
    /// have to read both to answer the same question.
    #[test]
    fn a_set_of_resolutions_holding_one_is_the_single_operation_shape() {
        let only = ResolutionOption {
            operation: OperationKey::CreateAccount,
            request: RequestPlan {
                preset: BTreeMap::new(),
                missing: vec![MissingInput::asked(OwnerPrompt::AccountTitle {
                    printed: None,
                })],
            },
        };
        assert!(matches!(
            ActionTarget::from_options(vec![only.clone()]),
            ActionTarget::Operation {
                operation: OperationKey::CreateAccount,
                ..
            }
        ));
        assert_eq!(ActionTarget::from_options(Vec::new()), ActionTarget::None);
        assert_eq!(
            ActionTarget::from_options(vec![only.clone(), only.clone()])
                .resolutions()
                .len(),
            2
        );

        // Built by hand, the invariant still holds: a choice of one is not a
        // choice, and the constructor is the only thing that normalises.
        assert_eq!(
            Action::new(
                ActionFacts {
                    id: "made_up".to_owned(),
                    kind: ActionKind::CreateFirstAccount,
                    category: ActionCategory::Blocking,
                    state: ActionState::NeedsOwnerInput,
                    subject: None,
                },
                "invented for this test",
                ActionTarget::Options(vec![only]),
            )
            .unwrap_err(),
            ActionInvariantError::OptionsWithoutChoice
        );
    }

    /// An item with no operation is `blocked`, whatever else is true of it.
    #[test]
    fn an_item_the_agent_cannot_call_says_so_in_its_state() {
        let account = account();
        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[no_facts(account.id)],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        for action in &actions {
            match action.target() {
                ActionTarget::None => assert_eq!(
                    action.state(),
                    ActionState::Blocked,
                    "{} has nothing to call and must say so",
                    action.id()
                ),
                ActionTarget::Operation { .. } | ActionTarget::Options(_) => assert_ne!(
                    action.state(),
                    ActionState::Blocked,
                    "{} names an operation and cannot be blocked",
                    action.id()
                ),
            }
        }
        // The witness this sweep keeps is the item that once got the state
        // wrong. It is no longer blocked — four operations begin an import — so
        // what it witnesses now is the other half of the same rule: an item that
        // names operations must not say `blocked`. The count is pinned rather
        // than merely non-zero: this item is the one every wave adds a
        // resolution to, and a sweep that stopped counting would stop noticing.
        let import = actions
            .iter()
            .find(|action| action.kind() == ActionKind::StartAccountImport)
            .expect("account import action");
        assert_ne!(import.state(), ActionState::Blocked);
        assert_eq!(import.target().resolutions().len(), 4);
        assert_eq!(
            import.subject().and_then(ActionSubject::account),
            Some(account.id)
        );
    }

    #[test]
    fn losing_contour_eligibility_is_not_contour_completion() {
        let account = account();
        let eligible = actions_from_views(&[account], &[], &[], &[], &[]);
        let ineligible = actions_from_views(&[], &[], &[], &[], &[]);

        assert!(
            eligible
                .iter()
                .any(|action| action.kind() == ActionKind::CreateFirstContour)
        );
        assert!(
            !ineligible
                .iter()
                .any(|action| action.kind() == ActionKind::CreateFirstContour)
        );
        assert!(!contour_completion(&[]));
    }

    #[test]
    fn two_accounts_awaiting_a_first_import_get_distinct_identities() {
        // Identity is what an agent deduplicates and tracks by. Two accounts
        // sharing one is not a cosmetic collision: the second item is invisible.
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let actions = actions_from_state(&OwnerState {
            accounts: &[with_id(first), with_id(second)],
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[no_facts(first), no_facts(second)],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        let identities: Vec<_> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::StartAccountImport)
            .map(Action::id)
            .collect();
        assert_eq!(identities.len(), 2);
        assert_ne!(identities[0], identities[1]);
    }

    /// The transfer statements the owner has made, spelled the way the store
    /// returns them.
    fn stated(account: AccountId, partners: &[AccountId]) -> AccountTransferStatementView {
        AccountTransferStatementView {
            account,
            partners: partners.to_vec(),
        }
    }

    /// Which of the owner's accounts money moves between is asked of every
    /// account, and it is asked before anything is imported.
    #[test]
    fn every_account_is_asked_which_of_the_others_money_moves_between_it_and() {
        let main = named("Main");
        let savings = named("Savings");
        let accounts = [main.clone(), savings.clone()];

        let actions = actions_from_views(&accounts, &[], &[], &[], &[]);
        let asked: Vec<_> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::ResolveTransferRelationships)
            .collect();
        assert_eq!(asked.len(), 2, "{actions:#?}");
        // One item per account, identified by it: an agent deduplicates by the
        // identity, and one shared identity would hide the second question.
        assert_ne!(asked[0].id(), asked[1].id());
        let subjects: Vec<_> = asked
            .iter()
            .filter_map(|action| action.subject().and_then(ActionSubject::account))
            .collect();
        assert!(subjects.contains(&main.id), "{subjects:?}");
        assert!(subjects.contains(&savings.id), "{subjects:?}");

        // The candidates are proposed and the choice is not made: every *other*
        // account is offered, and the account itself is not among them.
        let ActionTarget::Operation { operation, request } = asked[0].target() else {
            panic!("the statement has an operation to make it");
        };
        assert_eq!(*operation, OperationKey::RecordAccountTransferPartners);
        assert_eq!(request.missing.len(), 1);
        assert_eq!(request.missing[0].pointer, "/partners");
        assert_eq!(request.missing[0].provided_by, ProvidedBy::Owner);
        let subject = match asked[0].subject() {
            Some(ActionSubject::Account(account)) => account.id,
            other => panic!("the item names the account it is about: {other:?}"),
        };
        let candidates = request.missing[0]
            .candidates
            .as_ref()
            .expect("the owner is offered his other accounts");
        assert!(
            candidates.iter().all(|candidate| candidate.id != subject),
            "an account is not the other side of itself: {candidates:#?}"
        );
        assert_eq!(candidates.len(), 1);
    }

    /// With one account there is no other side, so the question is not asked.
    #[test]
    fn one_account_is_not_asked_what_it_transfers_with() {
        let only = account();
        assert!(!transfer_relationships_eligibility(
            only.id,
            std::slice::from_ref(&only),
            &[],
            &[]
        ));
        assert!(
            actions_from_views(std::slice::from_ref(&only), &[], &[], &[], &[])
                .iter()
                .all(|action| action.kind() != ActionKind::ResolveTransferRelationships)
        );
    }

    /// The goal is closed by a statement, and «none of my others» is one.
    #[test]
    fn a_statement_naming_no_partner_closes_the_question_it_answers() {
        let main = named("Main");
        let savings = named("Savings");
        let accounts = [main.clone(), savings.clone()];
        let statements = [stated(main.id, &[savings.id]), stated(savings.id, &[])];

        assert!(transfer_relationships_completion(main.id, &statements));
        assert!(transfer_relationships_completion(savings.id, &statements));
        assert!(
            actions_from_views(&accounts, &[], &[], &statements, &[])
                .iter()
                .all(|action| action.kind() != ActionKind::ResolveTransferRelationships)
        );
    }

    /// Being named by someone else's statement is not having made one.
    ///
    /// The far side of one relationship says nothing about the relationships
    /// this account is the near side of, and reading it as an answer would
    /// silence a question the owner never heard.
    #[test]
    fn being_named_as_a_partner_is_not_a_statement_of_ones_own() {
        let main = named("Main");
        let savings = named("Savings");
        let statements = [stated(main.id, &[savings.id])];

        assert!(!transfer_relationships_completion(savings.id, &statements));
        let asked: Vec<_> =
            actions_from_views(&[main, savings.clone()], &[], &[], &statements, &[])
                .into_iter()
                .filter(|action| action.kind() == ActionKind::ResolveTransferRelationships)
                .collect();
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0].subject().and_then(ActionSubject::account),
            Some(savings.id)
        );
    }

    /// The population is the accounts, so a new account reopens the goal.
    ///
    /// This is the property an existential predicate could not have: «some
    /// relationship has been stated» would be satisfied by the first statement
    /// and never asked again, and the account added afterwards — the second
    /// bank, which is the whole case — would be the one nobody was asked about.
    #[test]
    fn a_new_account_reopens_the_transfer_question_however_many_are_answered() {
        let main = named("Main");
        let savings = named("Savings");
        let statements = [stated(main.id, &[savings.id]), stated(savings.id, &[])];
        assert!(
            actions_from_views(&[main.clone(), savings.clone()], &[], &[], &statements, &[])
                .iter()
                .all(|action| action.kind() != ActionKind::ResolveTransferRelationships)
        );

        let everyday = named("Everyday");
        let asked: Vec<_> = actions_from_views(
            &[main, savings, everyday.clone()],
            &[],
            &[],
            &statements,
            &[],
        )
        .into_iter()
        .filter(|action| action.kind() == ActionKind::ResolveTransferRelationships)
        .collect();
        assert_eq!(asked.len(), 1, "only the new account is asked again");
        assert_eq!(
            asked[0].subject().and_then(ActionSubject::account),
            Some(everyday.id)
        );
    }

    /// A transfer has two ends, and at least one of them is inside the
    /// perimeter. The inside end's question already lets the owner name the
    /// outside one, so asking the outside account the same thing a second time
    /// gathers nothing and costs him a `RequiredForGoal` question about an
    /// account he has already ruled out of every report, with a reason.
    ///
    /// Nothing is lost for good: the scope is read from the contours and the
    /// exclusions on every call, never recorded beside the statement, so
    /// bringing the account back inside a contour reopens the question.
    #[test]
    fn an_account_ruled_outside_the_perimeter_is_not_asked_about_transfers() {
        let main = named("Main");
        let savings = named("Savings");
        let shop = named("Shop One");
        let accounts = [main.clone(), savings.clone(), shop.clone()];
        let contours = [ContourView {
            id: ContourId::new_random(),
            version: ContourVersion(1),
            title: "Household".into(),
            accounts: vec![main.id],
        }];
        let exclusions = [AccountScopeExclusionView {
            account: shop.id,
            reason: "A counterparty's account, not the owner's money.".into(),
        }];

        let asked: Vec<_> = actions_from_views(&accounts, &contours, &exclusions, &[], &[])
            .into_iter()
            .filter(|action| action.kind() == ActionKind::ResolveTransferRelationships)
            .map(|action| action.subject().and_then(ActionSubject::account))
            .collect();

        assert!(
            !asked.contains(&Some(shop.id)),
            "an account ruled outside every contour is not asked: {asked:?}"
        );
        // Inside and undecided both still raise it. The owner has ruled on one
        // and not on the other, and suppressing a question because an earlier
        // one is unanswered is a queue that goes quiet exactly when it should
        // not: the account nobody has placed is the one nobody has looked at.
        assert!(
            asked.contains(&Some(main.id)),
            "an account inside a contour is asked: {asked:?}"
        );
        assert!(
            asked.contains(&Some(savings.id)),
            "an account awaiting a scope decision is asked: {asked:?}"
        );
    }

    /// The outside account stays on the inside account's list of candidates.
    ///
    /// This is what makes the suppression lossless rather than a discovery
    /// thrown away: a movement between an inside account and an outside one is
    /// still nameable, from the end that is inside the perimeter — which is the
    /// end whose report a pair of unrelated legs would distort.
    #[test]
    fn an_outside_account_is_still_offered_as_the_far_side_of_an_inside_one() {
        let main = named("Main");
        let shop = named("Shop One");
        let accounts = [main.clone(), shop.clone()];
        let contours = [ContourView {
            id: ContourId::new_random(),
            version: ContourVersion(1),
            title: "Household".into(),
            accounts: vec![main.id],
        }];
        let exclusions = [AccountScopeExclusionView {
            account: shop.id,
            reason: "Closed years ago.".into(),
        }];

        let asked = actions_from_views(&accounts, &contours, &exclusions, &[], &[])
            .into_iter()
            .find(|action| action.kind() == ActionKind::ResolveTransferRelationships)
            .expect("the inside account is asked");
        assert_eq!(
            asked.subject().and_then(ActionSubject::account),
            Some(main.id)
        );
        let ActionTarget::Operation { request, .. } = asked.target() else {
            panic!("the transfer item names an operation");
        };
        let candidates = request.missing[0]
            .candidates
            .as_ref()
            .expect("the owner is offered his other accounts");
        assert!(
            candidates.iter().any(|candidate| candidate.id == shop.id),
            "the outside account is still a possible far side: {candidates:#?}"
        );
    }

    /// Structure is asked about before an import is offered.
    #[test]
    fn the_structural_question_is_ordered_before_the_import() {
        let main = named("Main");
        let savings = named("Savings");
        let actions = actions_from_state(&OwnerState {
            accounts: &[main.clone(), savings.clone()],
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[no_facts(main.id), no_facts(savings.id)],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");
        let mut sorted = actions;
        sort_actions(&mut sorted);
        let kinds: Vec<_> = sorted.iter().map(Action::kind).collect();
        let structure = kinds
            .iter()
            .position(|kind| *kind == ActionKind::ResolveTransferRelationships)
            .expect("the queue asks about structure");
        let import = kinds
            .iter()
            .position(|kind| *kind == ActionKind::StartAccountImport)
            .expect("the queue offers an import");
        assert!(structure < import, "{kinds:?}");
    }

    #[test]
    fn a_ready_action_requires_an_operation_target() {
        let result = Action::new(
            ActionFacts {
                id: "invalid".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::Ready,
                subject: None,
            },
            "invalid",
            ActionTarget::None,
        );

        assert_eq!(result, Err(ActionInvariantError::ReadyWithoutOperation));
    }

    #[tokio::test]
    async fn frontier_order_is_stable_on_unchanged_state() {
        let owner = OwnerId::new_random();
        let store = store();
        store
            .upsert_account(owner, account())
            .await
            .expect("account");

        let first = frontier(owner, &store, &store).await.expect("frontier");
        let second = frontier(owner, &store, &store).await.expect("frontier");
        assert_eq!(first, second);
        assert!(
            first
                .windows(2)
                .all(|actions| actions[0].kind() <= actions[1].kind())
        );
    }

    /// An account with nothing in it is offered every route that begins an import.
    ///
    /// The item used to be `Blocked`, on a sentence that was true of fetching the
    /// document and of nothing else. An agent reads `state` as its map of what it
    /// may call, so the queue disowned the two routes that do exist.
    ///
    /// It then offered two of the four (`iaam-j5oz`). Opening a session and
    /// synchronising a broker were published; the two calls that actually put a
    /// statement into the session were prose. An agent that read the target as
    /// its contract could open a session and had nowhere to go with it.
    #[test]
    fn an_account_awaiting_its_first_import_names_every_route_that_begins_one() {
        let account = account();
        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[no_facts(account.id)],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");
        let import = actions
            .iter()
            .find(|action| action.kind() == ActionKind::StartAccountImport)
            .expect("account import action");

        assert_eq!(import.state(), ActionState::NeedsOwnerInput);
        // Every route here keeps `Scope::Agent` as its floor, so an agent may
        // send any of them.
        assert_eq!(import.required_scope(), Some(Scope::Agent));

        let resolutions = import.target().resolutions();
        assert_eq!(
            resolutions
                .iter()
                .map(|(operation, _)| *operation)
                .collect::<Vec<_>>(),
            vec![
                OperationKey::OpenImportSession,
                OperationKey::ReadImportDocument,
                OperationKey::AddImportRows,
                OperationKey::SyncBroker,
            ],
            "the order is the promise: the call that can be made now, then the \
             two that put a statement into what it returns, then the channel"
        );

        // The session is opened for this account and nothing else is known: the
        // channel is the caller's, the label is read off the fetched document.
        let session = resolutions[0].1;
        assert_eq!(
            session.preset.get("source"),
            Some(&serde_json::json!({ "account": account.id.inner().to_string() }))
        );
        assert_eq!(
            session
                .missing
                .iter()
                .map(|input| (input.pointer.as_str(), input.provided_by))
                .collect::<Vec<_>>(),
            vec![
                ("/source/channel", ProvidedBy::Caller),
                ("/source/label", ProvidedBy::ExternalDocument),
            ]
        );

        // The two-step act said honestly: the second call takes a session in its
        // path, the item names it as a field the caller fills in, and the
        // caller's own previous call is where the value comes from. Nothing is
        // preset, because nothing here knows the session.
        let document = resolutions[1].1;
        assert!(document.preset.is_empty(), "{document:?}");
        assert_eq!(
            document
                .missing
                .iter()
                .map(|input| (input.pointer.as_str(), input.provided_by))
                .collect::<Vec<_>>(),
            vec![("/session", ProvidedBy::Caller)],
            "the session is the value the caller does not hold yet, and the \
             document is the body entire rather than a field of one"
        );

        // The other way in: rows already in this API's own words. The rows are a
        // field of *this* request, which is why they can be pointed at at all —
        // `iaam-tt71` argued about them as a field of the call above.
        let rows = resolutions[2].1;
        assert!(rows.preset.is_empty(), "{rows:?}");
        assert_eq!(
            rows.missing
                .iter()
                .map(|input| (input.pointer.as_str(), input.provided_by))
                .collect::<Vec<_>>(),
            vec![
                ("/session", ProvidedBy::Caller),
                ("/operations", ProvidedBy::ExternalDocument),
            ]
        );

        // The sync knows the account; which broker, and over what interval, are
        // the owner's to name.
        let sync = resolutions[3].1;
        assert_eq!(
            sync.preset.get("account"),
            Some(&serde_json::Value::from(account.id.inner().to_string()))
        );
        assert_eq!(
            sync.missing
                .iter()
                .map(|input| (input.pointer.as_str(), input.provided_by))
                .collect::<Vec<_>>(),
            vec![
                ("/broker", ProvidedBy::Owner),
                ("/from", ProvidedBy::Owner),
                ("/to", ProvidedBy::Owner),
            ]
        );

        // The honest half of the old sentence survives: the document is still
        // the owner's to fetch, and the reason says so without claiming the API
        // is inert.
        assert!(
            import
                .reason()
                .contains("no operation here downloads the document"),
            "{}",
            import.reason()
        );
        assert!(
            import.reason().contains("open an import session"),
            "{}",
            import.reason()
        );
        // And the two-step act is said in the prose as well as in the fields: a
        // client that reads only `reason` must not be left to guess where the
        // session in the second call's path comes from.
        assert!(
            import.reason().contains("two calls rather than one"),
            "the item must say that putting a statement in takes an open \
             session first: {}",
            import.reason()
        );
        // And the half that was missing (`iaam-tt71`): «feed it the rows» named
        // no way of producing them, so an agent that knew only the conclusive
        // kinds had to conclude — from a document it may not open — or stop. The
        // item now names the shape that lets it do neither.
        assert!(
            import.reason().contains("unresolved_direction"),
            "the item must name the shape a row nobody has concluded is sent \
             in, or it presupposes a converter it cannot address: {}",
            import.reason()
        );
    }

    #[test]
    fn an_account_without_business_facts_gets_a_continuous_import_action() {
        let account = account();
        let activity = AccountActivityView {
            account: account.id,
            has_business_fact: false,
            first_effective_date: None,
            last_effective_date: None,
        };

        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: std::slice::from_ref(&activity),
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");
        // What this test is about is the gap and its closing. The target is
        // asserted in full by the test above, which is where the old
        // `ActionTarget::None` assertion sat and where it was wrong.
        assert!(
            actions
                .iter()
                .any(|action| action.kind() == ActionKind::StartAccountImport)
        );
        assert!(!account_import_completion(&activity));

        let completed = AccountActivityView {
            has_business_fact: true,
            first_effective_date: Some(time::macros::date!(2026 - 03 - 01)),
            last_effective_date: Some(time::macros::date!(2026 - 03 - 01)),
            ..activity
        };
        assert!(account_import_completion(&completed));
        assert!(
            actions_from_state(&OwnerState {
                accounts: &[account],
                contours: &[],
                exclusions: &[],
                transfers: &[],
                activity: &[completed],
                assertions: &[],
                retired: RetirementAssessment::Assessed(&[]),
                sessions: &[],
                questions: &[],
                rules: &[],
                wanted_accounts: &[],
            })
            .expect("actions from state")
            .iter()
            .all(|action| action.kind() != ActionKind::StartAccountImport)
        );
    }

    /// The account, its period, and the assertions already recorded for it.
    fn assertion_queue(
        account: &AccountView,
        period: AssertionPeriod,
        recorded: &[ControlAssertionView],
    ) -> Vec<Action> {
        let activity = AccountActivityView {
            account: account.id,
            has_business_fact: true,
            first_effective_date: Some(period.from),
            last_effective_date: Some(period.to),
        };
        actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: std::slice::from_ref(&activity),
            assertions: recorded,
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state")
    }

    fn recorded_cash_assertion(
        account: AccountId,
        period: AssertionPeriod,
        point: BalancePoint,
    ) -> ControlAssertionView {
        ControlAssertionView {
            account,
            period,
            point: Some(point),
            dimension: Dimension::Cash,
        }
    }

    fn the_only_assertion_action(actions: &[Action]) -> &Action {
        let mut found = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::ProvideControlAssertion);
        let action = found.next().expect("control assertion action");
        assert!(
            found.next().is_none(),
            "the queue must not put the second question before the first is answered"
        );
        action
    }

    fn assertion_preset(action: &Action) -> &RequestPlan {
        let ActionTarget::Operation { operation, request } = action.target() else {
            panic!("control assertion needs an operation target");
        };
        assert_eq!(*operation, OperationKey::RecordOwnerBalance);
        request
    }

    #[test]
    fn a_business_fact_gets_one_scoped_control_assertion_action() {
        let account = account();
        let period = AssertionPeriod::between(
            time::macros::date!(2026 - 03 - 01),
            time::macros::date!(2026 - 03 - 31),
        )
        .expect("period");

        let actions = assertion_queue(&account, period, &[]);
        let request = assertion_preset(the_only_assertion_action(&actions));
        assert_eq!(request.preset["account"], account.id.inner().to_string());
        assert_eq!(request.preset["from"], period.from.to_string());
        assert_eq!(request.preset["to"], period.to.to_string());
        assert_eq!(request.missing.len(), 1);
        assert_eq!(request.missing[0].pointer, "/cash");

        let both = [
            recorded_cash_assertion(account.id, period, BalancePoint::Opening),
            recorded_cash_assertion(account.id, period, BalancePoint::Closing),
        ];
        for point in [BalancePoint::Opening, BalancePoint::Closing] {
            assert!(control_assertion_completion(
                &both,
                account.id,
                period,
                point,
                Dimension::Cash
            ));
        }
        assert!(
            assertion_queue(&account, period, &both)
                .iter()
                .all(|action| action.kind() != ActionKind::ProvideControlAssertion)
        );
    }

    #[test]
    fn the_opening_point_is_asked_for_before_the_closing_one() {
        // The defect this ordering exists for: with nothing asserting the state
        // before the first event, the projection sums from zero, and a closing
        // assertion compared against that sum reports the missing opening
        // balance as a discrepancy. Asking for the closing point first is asking
        // the second question before the first is answered.
        let account = account();
        let period = AssertionPeriod::between(
            time::macros::date!(2026 - 03 - 01),
            time::macros::date!(2026 - 03 - 31),
        )
        .expect("period");

        let fresh = assertion_queue(&account, period, &[]);
        let opening = the_only_assertion_action(&fresh);
        assert_eq!(assertion_preset(opening).preset["at"], "opening");

        let after_opening = assertion_queue(
            &account,
            period,
            &[recorded_cash_assertion(
                account.id,
                period,
                BalancePoint::Opening,
            )],
        );
        let closing = the_only_assertion_action(&after_opening);
        assert_eq!(assertion_preset(closing).preset["at"], "closing");

        // Two questions about the same account and interval, one kind, two
        // identities: an agent deduplicating by id sees the closing request as
        // new work rather than as the opening one it already answered.
        assert_eq!(opening.kind(), closing.kind());
        assert_ne!(opening.id(), closing.id());
    }

    #[test]
    fn a_closing_assertion_alone_does_not_answer_the_opening_question() {
        // A source that stated only its closing balance leaves the start
        // unasserted, and the queue must keep asking for it rather than fall
        // silent because something was recorded.
        let account = account();
        let period = AssertionPeriod::between(
            time::macros::date!(2026 - 03 - 01),
            time::macros::date!(2026 - 03 - 31),
        )
        .expect("period");

        let actions = assertion_queue(
            &account,
            period,
            &[recorded_cash_assertion(
                account.id,
                period,
                BalancePoint::Closing,
            )],
        );
        let request = assertion_preset(the_only_assertion_action(&actions));
        assert_eq!(request.preset["at"], "opening");
    }

    #[test]
    fn two_accounts_have_distinct_control_assertion_action_ids() {
        let first = account();
        let second = account();
        let period = AssertionPeriod::between(
            time::macros::date!(2026 - 03 - 01),
            time::macros::date!(2026 - 03 - 31),
        )
        .expect("period");
        let activity = [
            AccountActivityView {
                account: first.id,
                has_business_fact: true,
                first_effective_date: Some(period.from),
                last_effective_date: Some(period.to),
            },
            AccountActivityView {
                account: second.id,
                has_business_fact: true,
                first_effective_date: Some(period.from),
                last_effective_date: Some(period.to),
            },
        ];

        let actions = actions_from_state(&OwnerState {
            accounts: &[first, second],
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &activity,
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");
        let ids: Vec<_> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::ProvideControlAssertion)
            .map(Action::id)
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn losing_milestone_eligibility_is_not_completion() {
        let account = account();
        let activity = AccountActivityView {
            account: account.id,
            has_business_fact: true,
            first_effective_date: Some(time::macros::date!(2026 - 03 - 01)),
            last_effective_date: Some(time::macros::date!(2026 - 03 - 31)),
        };
        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[activity],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");
        assert!(
            actions
                .iter()
                .any(|action| action.kind() == ActionKind::ProvideControlAssertion)
        );
        assert!(
            actions_from_state(&OwnerState {
                accounts: &[],
                contours: &[],
                exclusions: &[],
                transfers: &[],
                activity: &[],
                assertions: &[],
                retired: RetirementAssessment::Assessed(&[]),
                sessions: &[],
                questions: &[],
                rules: &[],
                wanted_accounts: &[],
            })
            .expect("actions from state")
            .iter()
            .all(|action| action.kind() != ActionKind::ProvideControlAssertion)
        );
        assert!(!control_assertion_completion(
            &[],
            account.id,
            AssertionPeriod::between(
                time::macros::date!(2026 - 03 - 01),
                time::macros::date!(2026 - 03 - 31)
            )
            .expect("period"),
            BalancePoint::Closing,
            Dimension::Cash
        ));
    }
    fn diagnostic_event(account: AccountId, kind: EventKind, day: time::Date) -> Event {
        let source = SourceId::new_random();
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates: EventDates::for_cash(iaam_core::dates::CashPostedDate(day)),
            order: EffectiveOrder::new(day, 0),
            legs: Vec::new(),
            provenance: Provenance::new(
                source,
                RawHash::parse(&"a".repeat(64)).expect("raw hash"),
                ParserVersion("diagnostic/1".to_owned()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn gap_ledger(account: AccountId) -> ReconciliationLedger {
        let period =
            AssertionPeriod::between(date!(2026 - 08 - 01), date!(2026 - 08 - 31)).expect("period");
        let source = SourceId::new_random();
        let dimensions = BTreeSet::from([Dimension::Cash]);
        let row = RefusedRow {
            key: SourceRowKey {
                source,
                row: RowName::Given("row-17".to_owned()),
            },
            dimensions: dimensions.clone(),
        };
        let event = diagnostic_event(
            account,
            EventKind::ImportCoverageGap {
                period,
                dimensions,
                refused: 1,
                rows: vec![row],
            },
            period.to,
        );
        ReconciliationLedger::build(&[event]).expect("gap ledger")
    }

    #[test]
    fn a_coverage_gap_diagnostic_names_the_refused_row_and_has_no_call() {
        let ledger = gap_ledger(AccountId::new_random());
        let action = ledger_actions(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .expect("coverage gap diagnostic");

        assert_eq!(
            action.category(),
            ActionCategory::required_for(ActionKind::CoverageGapUnrepaired)
        );
        assert_eq!(action.state(), ActionState::Blocked);
        assert_eq!(action.required_scope(), None);
        assert_eq!(action.target(), &ActionTarget::None);
        assert!(action.reason().contains("given:row-17"));
        // The half a reader had to supply from nothing: why the two routes that
        // look like a repair are not one.
        assert!(
            action.reason().contains("retracting the import"),
            "{}",
            action.reason()
        );
        assert!(
            action.reason().contains("custody repair"),
            "{}",
            action.reason()
        );
    }

    #[test]
    fn a_gap_without_a_status_is_still_a_required_diagnostic() {
        let actions = ledger_actions(&gap_ledger(AccountId::new_random()));
        assert!(actions.iter().any(|action| {
            action.kind() == ActionKind::CoverageGapUnrepaired
                && action.category()
                    == ActionCategory::required_for(ActionKind::CoverageGapUnrepaired)
        }));
    }

    #[test]
    fn internal_confirmation_without_independence_is_named() {
        let account = AccountId::new_random();
        let period =
            AssertionPeriod::between(date!(2026 - 08 - 01), date!(2026 - 08 - 31)).expect("period");
        let ledger = ReconciliationLedger::build(&[diagnostic_event(
            account,
            EventKind::ControlAssertion {
                period,
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: BalancePoint::Closing,
                },
            },
            period.to,
        )])
        .expect("status ledger")
        .with_external_evidence(vec![(
            account,
            period,
            Evidence::from_match(
                Ground::BrokerApiAgreesWithStatement,
                SourceChannel {
                    source: SourceId::new_random(),
                    parser_version: ParserVersion("same".to_owned()),
                    document: RawHash::parse(&"c".repeat(64)),
                },
                SourceChannel {
                    source: SourceId::new_random(),
                    parser_version: ParserVersion("same".to_owned()),
                    document: RawHash::parse(&"c".repeat(64)),
                },
                BTreeSet::from([Dimension::Cash]),
            )
            .expect("evidence"),
        )]);
        let action = ledger_actions(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::IndependentConfirmationMissing)
            .expect("independence diagnostic");

        assert_eq!(
            action.category(),
            ActionCategory::required_for(ActionKind::IndependentConfirmationMissing)
        );
        assert!(action.reason().contains("different parser and document"));
        // Cash is one of the two dimensions a second channel can raise, so the
        // item names the call that supplies one instead of claiming there is
        // none. `Scope::Agent`, because `sync_broker` checks `may_submit`.
        assert_eq!(action.state(), ActionState::NeedsOwnerInput);
        assert_eq!(action.required_scope(), Some(Scope::Agent));
        let ActionTarget::Operation { operation, request } = action.target() else {
            panic!("an internally confirmed cash dimension names the broker sync");
        };
        assert_eq!(*operation, OperationKey::SyncBroker);
        assert_eq!(
            request
                .preset
                .get("account")
                .and_then(|value| value.as_str()),
            Some(account.inner().to_string().as_str())
        );
        assert_eq!(
            request.preset.get("from").and_then(|value| value.as_str()),
            Some("2026-08-01")
        );
        assert_eq!(
            request.preset.get("to").and_then(|value| value.as_str()),
            Some("2026-08-31")
        );
        let missing: Vec<&str> = request
            .missing
            .iter()
            .map(|input| input.pointer.as_str())
            .collect();
        assert_eq!(missing, vec!["/broker"]);
    }

    /// The half of the same item that has no route, and must keep saying so.
    ///
    /// `Ground::BrokerApiAgreesWithStatement` promotes cash and positions only,
    /// and the grounds that reach tax basis and income enter through
    /// `with_external_evidence`, which no handler calls. Promoting this half
    /// would name a call that cannot raise the dimension it is offered for.
    #[test]
    fn an_internally_confirmed_tax_dimension_still_has_no_operation() {
        let account = AccountId::new_random();
        let channel = SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("same".to_owned()),
            document: RawHash::parse(&"d".repeat(64)),
        };
        let ledger = ReconciliationLedger::default().with_external_evidence(vec![(
            account,
            august(),
            Evidence::from_match(
                Ground::TaxAgentCertificate,
                channel.clone(),
                channel,
                BTreeSet::from([Dimension::TaxBasis]),
            )
            .expect("evidence"),
        )]);

        let action = ledger_actions(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::IndependentConfirmationMissing)
            .expect("independence diagnostic");

        assert_eq!(action.state(), ActionState::Blocked);
        assert_eq!(action.target(), &ActionTarget::None);
        assert_eq!(action.required_scope(), None);
        assert!(action.id().ends_with(":tax_basis"), "{}", action.id());
        assert!(
            action.reason().contains("external evidence"),
            "{}",
            action.reason()
        );
    }

    #[test]
    fn discrepancy_diagnostic_names_both_sides_and_delta() {
        let account = AccountId::new_random();
        let period =
            AssertionPeriod::between(date!(2026 - 08 - 01), date!(2026 - 08 - 31)).expect("period");
        let observed_amount = Money::new(PostedMinor::new(500), CurrencyCode::Rub);
        let mut observed = diagnostic_event(
            account,
            EventKind::CashIn {
                amount: observed_amount,
            },
            period.to,
        );
        observed.legs = vec![Leg::cash(account, observed_amount)];
        // The opening is asserted as well as the closing. Without it the closing
        // figure is a sum from a start nothing states and is not compared at all
        // (`iaam-d7hn`), so there would be no discrepancy for this diagnostic to
        // name. Zero, because the account's history begins with the inflow above.
        let anchor = diagnostic_event(
            account,
            EventKind::ControlAssertion {
                period,
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: BalancePoint::Opening,
                },
            },
            period.from,
        );
        let assertion = diagnostic_event(
            account,
            EventKind::ControlAssertion {
                period,
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(1_000),
                    at: BalancePoint::Closing,
                },
            },
            period.to,
        );
        let ledger =
            ReconciliationLedger::build(&[observed, anchor, assertion]).expect("discrepant ledger");
        let action = ledger_actions(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::DiscrepancyUnresolved)
            .expect("discrepancy diagnostic");

        assert_eq!(
            action.category(),
            ActionCategory::required_for(ActionKind::DiscrepancyUnresolved)
        );
        assert!(
            action.reason().contains("claimed 10.00 RUB"),
            "{}",
            action.reason()
        );
        assert!(action.reason().contains("observed 5.00 RUB"));
        assert!(action.reason().contains("delta 5.00 RUB"));
        // The system still cannot say which side is wrong. One operation settles
        // either side once the owner has, and it is in this same API.
        assert_eq!(action.state(), ActionState::NeedsOwnerInput);
        assert_eq!(action.required_scope(), Some(Scope::Owner));
        let ActionTarget::Operation { operation, request } = action.target() else {
            panic!("a discrepancy names the correction that settles it");
        };
        assert_eq!(*operation, OperationKey::SubmitCorrections);
        assert!(
            request.preset.is_empty(),
            "a discrepancy names no event, so it proposes no correction: {:?}",
            request.preset
        );
        let missing: Vec<&str> = request
            .missing
            .iter()
            .map(|input| input.pointer.as_str())
            .collect();
        assert_eq!(missing, vec!["/corrections", "/acknowledge_retraction"]);
        assert!(
            action
                .reason()
                .contains("Recording another balance does not"),
            "{}",
            action.reason()
        );
    }

    #[test]
    fn flow_diagnostics_names_undecomposed_account_and_residual_account() {
        let account = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let period = DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        };
        let mut flow = MoneyFlow::new();
        let outflow_amount = Money::new(PostedMinor::new(-700), CurrencyCode::Rub);
        let mut outflow = diagnostic_event(
            account,
            EventKind::CashOut {
                amount: outflow_amount,
            },
            date!(2026 - 08 - 03),
        );
        outflow.legs = vec![Leg::cash(account, outflow_amount)];
        flow.apply(&outflow, &contour, period, &NoCategories)
            .expect("outflow");
        let opening_amount = Money::new(PostedMinor::new(-200), CurrencyCode::Rub);
        let mut opening = diagnostic_event(
            account,
            EventKind::OpeningCash {
                amount: opening_amount,
            },
            date!(2026 - 08 - 04),
        );
        opening.legs = vec![Leg::cash(account, opening_amount)];
        flow.apply(&opening, &contour, period, &NoCategories)
            .expect("opening balance");
        let report = MoneyFlowReport {
            contour: contour.id(),
            version: ContourVersion(1),
            from: period.from,
            to: period.to,
            category_rule_versions: Vec::new(),
            flow,
        };
        let actions = flow_actions(&report);

        let undecomposed = actions
            .iter()
            .find(|action| action.kind() == ActionKind::UndecomposedOutflows)
            .expect("undecomposed diagnostic");
        assert_eq!(undecomposed.category(), ActionCategory::Recommended);
        assert!(undecomposed.reason().contains(&account.inner().to_string()));
        assert_eq!(undecomposed.state(), ActionState::NeedsOwnerInput);
        assert_eq!(undecomposed.required_scope(), Some(Scope::Owner));
        let ActionTarget::Operation { operation, request } = undecomposed.target() else {
            panic!("a rule-remediable outflow names the operation that remedies it");
        };
        assert_eq!(*operation, OperationKey::CreateCategoryRule);
        assert!(
            request.preset.is_empty(),
            "nothing in this aggregate justifies a preset field: {:?}",
            request.preset
        );
        let missing: Vec<&str> = request
            .missing
            .iter()
            .map(|input| input.pointer.as_str())
            .collect();
        assert_eq!(missing, vec!["/matcher", "/category"]);
        assert!(
            request
                .missing
                .iter()
                .all(|input| input.provided_by == ProvidedBy::Owner)
        );
        let residual = actions
            .iter()
            .find(|action| action.kind() == ActionKind::UnexplainedResidual)
            .expect("residual diagnostic");
        assert_eq!(residual.category(), ActionCategory::Informational);
        assert!(residual.reason().contains(&account.inner().to_string()));
        assert_eq!(residual.target(), &ActionTarget::None);
    }

    #[test]
    fn possible_duplicate_diagnostic_names_both_events_and_level() {
        let event = EventId::new_random();
        let of = EventId::new_random();
        let action = verdict_diagnostics(&Verdict::PossibleDuplicate {
            event,
            of,
            level: iaam_ingest::dedup::DedupLevel::Probabilistic,
        })
        .expect("duplicate diagnostic");

        assert_eq!(action.kind(), ActionKind::PossibleDuplicateUndecided);
        assert_eq!(
            action.category(),
            ActionCategory::required_for(ActionKind::PossibleDuplicateUndecided)
        );
        assert_eq!(action.state(), ActionState::Blocked);
        assert!(action.id().contains(&event.inner().to_string()));
        assert!(action.id().contains(&of.inner().to_string()));
        assert!(action.id().ends_with(":5"));
        assert_eq!(action.target(), &ActionTarget::None);
    }

    #[test]
    fn a_blocked_action_has_no_operation_and_no_scope() {
        let result = Action::new(
            ActionFacts {
                id: "blocked".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::Blocked,
                subject: None,
            },
            "nothing can call this",
            ActionTarget::None,
        );

        let action = result.expect("valid blocked action");
        assert_eq!(action.target(), &ActionTarget::None);
        assert_eq!(action.required_scope(), None);
    }

    #[test]
    fn blocked_action_rejects_an_operation() {
        let operation = Action::new(
            ActionFacts {
                id: "blocked-operation".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::Blocked,
                subject: None,
            },
            "nothing can call this",
            ActionTarget::Operation {
                operation: OperationKey::CreateAccount,
                request: RequestPlan {
                    preset: BTreeMap::new(),
                    missing: Vec::new(),
                },
            },
        );
        assert_eq!(operation, Err(ActionInvariantError::BlockedWithOperation));
    }

    /// A blocked item cannot state a scope, because it no longer states one at
    /// all: the authority is read off the resolutions, and it publishes none.
    ///
    /// This is what `BlockedWithScope` used to refuse. The refusal is gone
    /// because the combination it refused cannot be built any more — which is
    /// the point of removing the field, and worth a test rather than an
    /// absence, so that a later change putting the field back has to face it.
    #[test]
    fn a_blocked_item_can_no_longer_state_a_scope_at_all() {
        let blocked = Action::new(
            ActionFacts {
                id: "blocked-scope".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::Blocked,
                subject: None,
            },
            "nothing can call this",
            ActionTarget::None,
        )
        .expect("a blocked item with no target is valid");
        assert_eq!(blocked.required_scope(), None);
    }

    /// An item that is not blocked and offers no way out is refused.
    ///
    /// The combination used to be legal and produced an item that named an
    /// authority while naming no call to use it on. It is now
    /// `NeedsOwnerInput` with no target, and the word for that state is
    /// `Blocked`.
    #[test]
    fn a_nonblocked_action_requires_a_resolution() {
        let result = Action::new(
            ActionFacts {
                id: "missing-scope".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::NeedsOwnerInput,
                subject: None,
            },
            "invalid",
            ActionTarget::None,
        );
        assert_eq!(result, Err(ActionInvariantError::NonBlockedWithoutScope));
    }

    fn august() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 08 - 01), date!(2026 - 08 - 31)).expect("period")
    }

    fn cash_gap_event(account: AccountId, refused: u32, rows: Vec<RefusedRow>) -> Event {
        diagnostic_event(
            account,
            EventKind::ImportCoverageGap {
                period: august(),
                dimensions: BTreeSet::from([Dimension::Cash]),
                refused,
                rows,
            },
            august().to,
        )
    }

    fn refused_row(source: SourceId, name: &str) -> RefusedRow {
        RefusedRow {
            key: SourceRowKey {
                source,
                row: RowName::Given(name.to_owned()),
            },
            dimensions: BTreeSet::from([Dimension::Cash]),
        }
    }

    fn cash_balance_assertion(account: AccountId, minor: i64) -> Event {
        diagnostic_event(
            account,
            EventKind::ControlAssertion {
                period: august(),
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(minor),
                    at: BalancePoint::Closing,
                },
            },
            august().to,
        )
    }

    /// The opening half of a control section: what the source says was there
    /// before the interval's first event.
    ///
    /// Zero, and present in every fixture that expects a closing balance to be
    /// compared at all. Without it the closing figure is a sum from a start
    /// nothing states and the outcome is `OpeningNotAsserted`, not a
    /// discrepancy (`iaam-d7hn`).
    fn cash_opening_assertion(account: AccountId) -> Event {
        diagnostic_event(
            account,
            EventKind::ControlAssertion {
                period: august(),
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: BalancePoint::Opening,
                },
            },
            august().from,
        )
    }

    fn cash_in_event(account: AccountId, minor: i64, day: time::Date) -> Event {
        let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
        let mut event = diagnostic_event(account, EventKind::CashIn { amount }, day);
        event.legs = vec![Leg::cash(account, amount)];
        event
    }

    fn channel(parser: &str, document: &str) -> SourceChannel {
        SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion(parser.to_owned()),
            document: RawHash::parse(&document.repeat(64)),
        }
    }

    fn independent_cash_evidence() -> Evidence {
        Evidence::from_match(
            Ground::BrokerApiAgreesWithStatement,
            channel("left", "c"),
            channel("right", "d"),
            BTreeSet::from([Dimension::Cash]),
        )
        .expect("independent evidence")
    }

    fn internal_cash_evidence() -> Evidence {
        Evidence::from_match(
            Ground::BrokerApiAgreesWithStatement,
            channel("same", "c"),
            channel("same", "c"),
            BTreeSet::from([Dimension::Cash]),
        )
        .expect("internal evidence")
    }

    fn flow_report(
        flow: MoneyFlow,
        contour: &ContourDefinition,
        period: DateWindow,
    ) -> MoneyFlowReport {
        MoneyFlowReport {
            contour: contour.id(),
            version: ContourVersion(1),
            from: period.from,
            to: period.to,
            category_rule_versions: Vec::new(),
            flow,
        }
    }

    fn undecomposed_report(accounts: &[AccountId]) -> MoneyFlowReport {
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            accounts.to_vec(),
        );
        let period = DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        };
        let mut flow = MoneyFlow::new();
        for account in accounts {
            let outflow_amount = Money::new(PostedMinor::new(-700), CurrencyCode::Rub);
            let mut outflow = diagnostic_event(
                *account,
                EventKind::CashOut {
                    amount: outflow_amount,
                },
                date!(2026 - 08 - 03),
            );
            outflow.legs = vec![Leg::cash(*account, outflow_amount)];
            flow.apply(&outflow, &contour, period, &NoCategories)
                .expect("outflow");
            let opening_amount = Money::new(PostedMinor::new(-200), CurrencyCode::Rub);
            let mut opening = diagnostic_event(
                *account,
                EventKind::OpeningCash {
                    amount: opening_amount,
                },
                date!(2026 - 08 - 04),
            );
            opening.legs = vec![Leg::cash(*account, opening_amount)];
            flow.apply(&opening, &contour, period, &NoCategories)
                .expect("opening balance");
        }
        flow_report(flow, &contour, period)
    }

    /// A transfer that leaves the contour, on each named account.
    ///
    /// The counterparty is a fresh account outside the contour, which is what makes
    /// `classify` call the transfer `ExternalOut`; the projection then records the
    /// amount as undecomposed without ever asking the category index about it.
    fn external_transfer_report(accounts: &[AccountId]) -> MoneyFlowReport {
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            accounts.to_vec(),
        );
        let period = DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        };
        let mut flow = MoneyFlow::new();
        for account in accounts {
            apply_external_transfer(&mut flow, &contour, period, *account);
        }
        flow_report(flow, &contour, period)
    }

    /// One account holding both an unmatched outflow and a transfer out.
    fn mixed_report(account: AccountId) -> MoneyFlowReport {
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let period = DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        };
        let mut flow = MoneyFlow::new();
        let outflow_amount = Money::new(PostedMinor::new(-700), CurrencyCode::Rub);
        let mut outflow = diagnostic_event(
            account,
            EventKind::CashOut {
                amount: outflow_amount,
            },
            date!(2026 - 08 - 03),
        );
        outflow.legs = vec![Leg::cash(account, outflow_amount)];
        flow.apply(&outflow, &contour, period, &NoCategories)
            .expect("outflow");
        apply_external_transfer(&mut flow, &contour, period, account);
        flow_report(flow, &contour, period)
    }

    fn apply_external_transfer(
        flow: &mut MoneyFlow,
        contour: &ContourDefinition,
        period: DateWindow,
        account: AccountId,
    ) {
        let amount = Money::new(PostedMinor::new(-1_100), CurrencyCode::Rub);
        let mut transfer = diagnostic_event(
            account,
            EventKind::CashTransfer {
                transfer_id: iaam_core::ids::TransferId::new_random(),
                from: account,
                to: AccountId::new_random(),
                amount,
            },
            date!(2026 - 08 - 05),
        );
        transfer.legs = vec![Leg::cash(account, amount)];
        flow.apply(&transfer, contour, period, &NoCategories)
            .expect("external transfer");
    }

    /// Every outflow carries a category and every account closes: the report has
    /// nothing left over to report.
    fn decomposed_report(account: AccountId) -> MoneyFlowReport {
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let period = DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        };
        let mut flow = MoneyFlow::new();
        flow.apply(
            &cash_in_event(account, 700, date!(2026 - 08 - 03)),
            &contour,
            period,
            &NoCategories,
        )
        .expect("inflow");
        flow_report(flow, &contour, period)
    }

    /// A legacy record predates schema 8 and holds no refused rows. Rendering it as
    /// a gap that refused nothing would read as a gap with no consequence, so the
    /// prose says the rows cannot be named and still reports how many there were.
    #[test]
    fn a_legacy_gap_without_rows_cannot_name_them_and_says_so() {
        let ledger =
            ReconciliationLedger::build(&[cash_gap_event(AccountId::new_random(), 3, Vec::new())])
                .expect("legacy gap ledger");

        let action = ledger_actions(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .expect("legacy coverage gap diagnostic");

        assert!(
            action.reason().contains("cannot name the refused rows"),
            "{}",
            action.reason()
        );
        assert!(
            !action.reason().contains("refused rows:"),
            "a legacy gap must not claim to list rows: {}",
            action.reason()
        );
        assert!(
            action.reason().contains("3 rows refused"),
            "the count survives even when the rows do not: {}",
            action.reason()
        );
        assert_eq!(action.target(), &ActionTarget::None);
    }

    /// §6: a clean second channel can carry a dimension to independence while an
    /// older gap stands. The gap is then a fact, not outstanding work — and the
    /// category must be computed from the dimension statuses rather than fixed.
    #[test]
    fn a_gap_whose_tainted_dimensions_are_all_independent_is_informational() {
        let account = AccountId::new_random();
        let source = SourceId::new_random();
        let ledger = ReconciliationLedger::build(&[
            cash_gap_event(account, 1, vec![refused_row(source, "row-4")]),
            cash_balance_assertion(account, 0),
        ])
        .expect("confirmed gap ledger")
        .with_external_evidence(vec![(account, august(), independent_cash_evidence())]);

        let action = ledger_actions(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .expect("coverage gap diagnostic");

        assert_eq!(action.category(), ActionCategory::Informational);
        assert_eq!(action.state(), ActionState::Blocked);
        assert_eq!(action.target(), &ActionTarget::None);
    }

    /// The same fixture with the dimension one level lower stays required: the
    /// two assertions together show the category is computed and not a constant.
    #[test]
    fn a_gap_whose_tainted_dimension_stops_at_internal_stays_required() {
        let account = AccountId::new_random();
        let source = SourceId::new_random();
        let ledger = ReconciliationLedger::build(&[
            cash_gap_event(account, 1, vec![refused_row(source, "row-4")]),
            cash_balance_assertion(account, 0),
        ])
        .expect("internal gap ledger")
        .with_external_evidence(vec![(account, august(), internal_cash_evidence())]);

        let action = ledger_actions(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .expect("coverage gap diagnostic");

        assert_eq!(
            action.category(),
            ActionCategory::required_for(ActionKind::CoverageGapUnrepaired)
        );
    }

    /// One ledger, one flow report and one verdict that between them produce every
    /// diagnostic this task defines.
    fn every_diagnostic() -> Vec<Action> {
        let discrepant = AccountId::new_random();
        let internal = AccountId::new_random();
        let source = SourceId::new_random();
        let ledger = ReconciliationLedger::build(&[
            cash_gap_event(discrepant, 1, vec![refused_row(source, "row-9")]),
            cash_in_event(discrepant, 500, august().to),
            cash_opening_assertion(discrepant),
            cash_balance_assertion(discrepant, 1_000),
            cash_balance_assertion(internal, 0),
        ])
        .expect("diagnostic ledger")
        .with_external_evidence(vec![(internal, august(), internal_cash_evidence())]);

        let mut actions = ledger_actions(&ledger);
        actions.extend(flow_actions(&undecomposed_report(&[
            AccountId::new_random(),
        ])));
        actions.extend(flow_actions(&external_transfer_report(&[
            AccountId::new_random(),
        ])));
        actions.extend(verdict_diagnostics(&Verdict::PossibleDuplicate {
            event: EventId::new_random(),
            of: EventId::new_random(),
            level: iaam_ingest::dedup::DedupLevel::Probabilistic,
        }));
        actions
    }

    /// The universal assertions are worthless over an empty set, so the exact set
    /// of kinds is asserted **first**: the sweep below then runs over something.
    ///
    /// The sweep used to assert that *every* diagnostic is blocked. That was the
    /// defect, not the invariant: `Blocked` means no operation in this API acts on
    /// the item, and a spending row nobody has categorised is remedied by an
    /// operation this same API offers. What holds for every diagnostic is only the
    /// agreement between the three fields, so that is what is asserted here — and
    /// the split kinds are named individually below, so a future diagnostic cannot
    /// quietly rejoin the blocked majority.
    #[test]
    fn every_diagnostic_states_its_availability_truthfully() {
        let actions = every_diagnostic();

        let kinds: BTreeSet<ActionKind> = actions.iter().map(Action::kind).collect();
        assert_eq!(
            kinds,
            BTreeSet::from([
                ActionKind::CoverageGapUnrepaired,
                ActionKind::IndependentConfirmationMissing,
                ActionKind::DiscrepancyUnresolved,
                ActionKind::UndecomposedOutflows,
                ActionKind::ExternalTransfersUncategorised,
                ActionKind::UnexplainedResidual,
                ActionKind::PossibleDuplicateUndecided,
            ]),
            "the sweep must run over every diagnostic kind, not a subset"
        );

        for action in &actions {
            if action.state() == ActionState::Blocked {
                assert_eq!(action.target(), &ActionTarget::None, "{}", action.id());
                assert_eq!(action.required_scope(), None, "{}", action.id());
            } else {
                assert_eq!(
                    action.state(),
                    ActionState::NeedsOwnerInput,
                    "{}",
                    action.id()
                );
                assert!(
                    matches!(action.target(), ActionTarget::Operation { .. }),
                    "{} is not blocked and must name the operation that answers it",
                    action.id()
                );
                // Some scope, and the scope the named route actually checks:
                // `create_category_rule` and `submit_corrections` are owner-only,
                // `sync_broker` admits an agent token, and an item that named the
                // wrong one would tell a caller it may not send a request the
                // server would accept.
                let expected = match action.kind() {
                    ActionKind::IndependentConfirmationMissing => Scope::Agent,
                    _ => Scope::Owner,
                };
                assert_eq!(action.required_scope(), Some(expected), "{}", action.id());
            }
        }

        let blocked: BTreeSet<ActionKind> = actions
            .iter()
            .filter(|action| action.state() == ActionState::Blocked)
            .map(|action| action.kind())
            .collect();
        assert!(blocked.contains(&ActionKind::ExternalTransfersUncategorised));
        assert!(!blocked.contains(&ActionKind::UndecomposedOutflows));
    }

    /// An aggregate holding nothing but transfers out of the contour has no remedy
    /// in this API, and the queue must not invent one: a category rule would never
    /// be consulted for a transfer, so offering rule creation here would be false.
    #[test]
    fn a_transfer_only_aggregate_offers_no_rule() {
        let account = AccountId::new_random();
        let actions = flow_actions(&external_transfer_report(&[account]));

        assert!(
            !actions
                .iter()
                .any(|action| action.kind() == ActionKind::UndecomposedOutflows),
            "a transfer is not remediable by a category rule: {actions:?}"
        );
        let transfers = actions
            .iter()
            .find(|action| action.kind() == ActionKind::ExternalTransfersUncategorised)
            .expect("external transfer diagnostic");
        assert_eq!(transfers.state(), ActionState::Blocked);
        assert_eq!(transfers.category(), ActionCategory::Informational);
        assert_eq!(transfers.target(), &ActionTarget::None);
        assert_eq!(transfers.required_scope(), None);
        assert!(transfers.reason().contains(&account.inner().to_string()));
        assert!(
            transfers
                .reason()
                .contains("category rule cannot decompose"),
            "{}",
            transfers.reason()
        );
    }

    /// The case the single aggregate could only answer with a half-truth: one
    /// account holding both kinds of row gets both items, each naming its own
    /// account and neither claiming the other's remedy.
    #[test]
    fn a_mixed_account_gets_a_remedy_for_the_rows_that_have_one() {
        let account = AccountId::new_random();
        let actions = flow_actions(&mixed_report(account));

        let outflows = actions
            .iter()
            .find(|action| action.kind() == ActionKind::UndecomposedOutflows)
            .expect("rule-remediable diagnostic");
        let transfers = actions
            .iter()
            .find(|action| action.kind() == ActionKind::ExternalTransfersUncategorised)
            .expect("transfer diagnostic");

        assert_ne!(outflows.id(), transfers.id());
        assert_eq!(outflows.state(), ActionState::NeedsOwnerInput);
        assert_eq!(transfers.state(), ActionState::Blocked);
        // 700 minor units of spending and 1100 of transfer, kept apart rather than
        // reported as one 1800 aggregate pointed at a rule that reaches only 700.
        assert!(outflows.reason().contains("7.00"), "{}", outflows.reason());
        assert!(
            transfers.reason().contains("11.00"),
            "{}",
            transfers.reason()
        );
    }

    /// An agent deduplicates by `id`. Two accounts holding the same diagnostic
    /// must not collapse into one item.
    #[test]
    fn two_accounts_with_the_same_diagnostic_get_distinct_ids() {
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let source = SourceId::new_random();
        let ledger = ReconciliationLedger::build(&[
            cash_gap_event(first, 1, vec![refused_row(source, "row-1")]),
            cash_gap_event(second, 1, vec![refused_row(source, "row-2")]),
        ])
        .expect("two-account gap ledger");

        let diagnostics = ledger_actions(&ledger);
        let gaps: Vec<&Action> = diagnostics
            .iter()
            .filter(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .collect();
        assert_eq!(gaps.len(), 2);
        assert_ne!(gaps[0].id(), gaps[1].id());

        let flow = flow_actions(&undecomposed_report(&[first, second]));
        let undecomposed: Vec<&Action> = flow
            .iter()
            .filter(|action| action.kind() == ActionKind::UndecomposedOutflows)
            .collect();
        assert_eq!(undecomposed.len(), 2);
        assert_ne!(undecomposed[0].id(), undecomposed[1].id());
    }

    /// Nothing outstanding, nothing informational: the detectors say nothing
    /// rather than filling the answer with items that mean "all is well".
    #[test]
    fn a_reconciled_and_decomposed_report_yields_no_diagnostics() {
        let account = AccountId::new_random();
        let ledger = ReconciliationLedger::build(&[
            cash_in_event(account, 1_000, august().to),
            cash_balance_assertion(account, 1_000),
        ])
        .expect("matched ledger");

        assert!(
            ledger_actions(&ledger).is_empty(),
            "{:?}",
            ledger_actions(&ledger)
        );
        let report = decomposed_report(account);
        assert!(
            flow_actions(&report).is_empty(),
            "{:?}",
            flow_actions(&report)
        );
        assert!(
            verdict_diagnostics(&Verdict::Accepted {
                event: EventId::new_random()
            })
            .is_none()
        );
    }

    // --- The classification question in the queue ---------------------------
    //
    // The defect these cover: a question raised at intake was durable, and the
    // only way to learn of one was the response that raised it. If the response
    // was lost, the outstanding work was invisible.

    fn unresolved(account: AccountId) -> Question {
        Question::UnresolvedDirection {
            account,
            stated: Some("INNER".to_owned()),
            counterparty: None,
        }
    }

    fn asked(session: ImportSessionId, account: AccountId, row: u32) -> ClassificationQuestion {
        let question = unresolved(account);
        ClassificationQuestion {
            view: ImportQuestionView {
                id: ImportQuestionId::new_random(),
                session,
                row,
                question: serde_json::to_string(&question).expect("question json"),
                alternatives: serde_json::to_string(&question.alternatives())
                    .expect("alternatives json"),
                prompt: "Which was it?".to_owned(),
                asked_at: "2026-03-01T00:00:00Z".to_owned(),
                answered_at: None,
                answer: None,
                rule: None,
            },
            session_state: ImportSessionState::Open,
            asked: question,
            // An open question has generalised nothing yet, and the row it is
            // about is not consulted while that is true.
            generalisation: Generalisation::Unanswered,
            subject: None,
        }
    }

    fn only_question_item(actions: &[Action]) -> &Action {
        let mut found = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::AnswerClassificationQuestion);
        let item = found.next().expect("the queue holds the open question");
        assert!(found.next().is_none(), "one item per question");
        item
    }

    fn answer_field(action: &Action) -> &MissingInput {
        let ActionTarget::Operation { request, .. } = action.target() else {
            panic!("an answerable item names an operation");
        };
        request
            .missing
            .iter()
            .find(|missing| missing.pointer == "/answer")
            .expect("the answer field")
    }

    /// The question is discoverable from persisted state alone.
    #[test]
    fn an_open_classification_question_is_an_item_in_the_queue() {
        let main = named("Main");
        let session = ImportSessionId::new_random();
        let question = asked(session, main.id, 3);

        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&main),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: std::slice::from_ref(&question),
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        let item = only_question_item(&actions);
        assert_eq!(
            item.id(),
            format!(
                "answer_classification_question:{}",
                question.view.id.inner()
            )
        );
        assert_eq!(
            item.subject().and_then(ActionSubject::account),
            Some(main.id)
        );
        assert_eq!(
            item.category(),
            ActionCategory::required_for(ActionKind::AnswerClassificationQuestion)
        );
        assert_eq!(item.state(), ActionState::NeedsOwnerInput);
        assert_eq!(item.required_scope(), Some(Scope::Agent));
    }

    /// The item carries the operation that answers it, addressed to this question.
    #[test]
    fn the_item_names_the_operation_that_answers_it() {
        let main = named("Main");
        let session = ImportSessionId::new_random();
        let question = asked(session, main.id, 3);

        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&main),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: std::slice::from_ref(&question),
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        let item = only_question_item(&actions);
        let ActionTarget::Operation { operation, request } = item.target() else {
            panic!("an item the owner can answer names an operation");
        };
        assert_eq!(*operation, OperationKey::AnswerImportQuestion);
        assert_eq!(
            request.preset.get("session").and_then(|id| id.as_str()),
            Some(session.inner().to_string().as_str())
        );
        assert_eq!(
            request.preset.get("question").and_then(|id| id.as_str()),
            Some(question.view.id.inner().to_string().as_str())
        );
    }

    /// The shapes travel with the item, and they are the ones this question admits.
    #[test]
    fn the_item_publishes_the_answer_shapes_the_question_admits() {
        let main = named("Main");
        let question = asked(ImportSessionId::new_random(), main.id, 1);

        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&main),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: std::slice::from_ref(&question),
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        let offered: Vec<&str> = answer_field(only_question_item(&actions))
            .alternatives
            .iter()
            .map(|alternative| alternative.value.as_str())
            .collect();
        let admitted: Vec<&str> = question
            .asked
            .alternatives()
            .iter()
            .map(|shape| shape.code())
            .collect();
        assert_eq!(offered, admitted);
    }

    /// A question about one direction offers the answers that run that way, and
    /// not the directionless seven.
    ///
    /// Three now rather than two, and `refund` is the third: money arriving that
    /// nobody sent, money the capital earned and money a counterparty returned
    /// are three facts the reports keep apart, and until the third had a word
    /// the queue published a choice that could not express it (`iaam-7l7v`).
    #[test]
    fn a_different_question_publishes_different_shapes() {
        let main = named("Main");
        let inflow = Question::IsInflowIncome { account: main.id };
        let mut question = asked(ImportSessionId::new_random(), main.id, 1);
        question.view.question = serde_json::to_string(&inflow).expect("question json");
        question.asked = inflow;

        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&main),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: std::slice::from_ref(&question),
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        let offered: Vec<&str> = answer_field(only_question_item(&actions))
            .alternatives
            .iter()
            .map(|alternative| alternative.value.as_str())
            .collect();
        assert_eq!(offered, vec!["income", "received", "refund"]);
    }

    /// Only the two shapes that name an account ask for one, and never this one.
    #[test]
    fn only_the_shapes_that_name_an_account_ask_for_one() {
        let main = named("Main");
        let savings = named("Savings");
        let question = asked(ImportSessionId::new_random(), main.id, 1);

        let actions = actions_from_state(&OwnerState {
            accounts: &[main.clone(), savings.clone()],
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: std::slice::from_ref(&question),
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        for alternative in &answer_field(only_question_item(&actions)).alternatives {
            match alternative.value.as_str() {
                "sent_to_own_account" | "received_from_own_account" => {
                    let required = alternative
                        .requires
                        .first()
                        .expect("naming an own account requires one");
                    assert_eq!(required.pointer, "/account");
                    let offered: Vec<_> = required
                        .candidates
                        .as_ref()
                        .expect("the owner's other accounts")
                        .iter()
                        .map(|candidate| candidate.id)
                        .collect();
                    assert_eq!(
                        offered,
                        vec![savings.id],
                        "an account is not the other side of itself"
                    );
                }
                _ => assert!(
                    alternative.requires.is_empty(),
                    "{} needs no account: {alternative:?}",
                    alternative.value
                ),
            }
        }
    }

    /// The goal closes on the answer, and on nothing else.
    #[test]
    fn an_answered_question_is_not_an_item() {
        let main = named("Main");
        let mut question = asked(ImportSessionId::new_random(), main.id, 1);
        question.view.answered_at = Some("2026-03-02T00:00:00Z".to_owned());
        question.view.answer = Some(r#"{"answer":"paid"}"#.to_owned());

        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&main),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: std::slice::from_ref(&question),
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        assert!(
            actions
                .iter()
                .all(|action| action.kind() != ActionKind::AnswerClassificationQuestion),
            "{actions:?}"
        );
        assert!(classification_question_completion(&question));
    }

    /// A session that will never commit raises no work the owner can do.
    #[test]
    fn a_question_in_a_closed_session_is_not_an_item() {
        let main = named("Main");
        for state in [ImportSessionState::Committed, ImportSessionState::Abandoned] {
            let mut question = asked(ImportSessionId::new_random(), main.id, 1);
            question.session_state = state;
            assert!(!classification_question_eligibility(&question));
            let actions = actions_from_state(&OwnerState {
                accounts: std::slice::from_ref(&main),
                contours: &[],
                exclusions: &[],
                transfers: &[],
                activity: &[],
                assertions: &[],
                retired: RetirementAssessment::Assessed(&[]),
                sessions: &[],
                questions: std::slice::from_ref(&question),
                rules: &[],
                wanted_accounts: &[],
            })
            .expect("actions from state");
            assert!(
                actions
                    .iter()
                    .all(|action| action.kind() != ActionKind::AnswerClassificationQuestion),
                "{state:?}: {actions:?}"
            );
            // Losing eligibility is not completion: the question is still open.
            assert!(!classification_question_completion(&question));
        }
    }

    /// A second, unrelated question is a second item with its own identity.
    #[test]
    fn two_open_questions_are_two_items() {
        let main = named("Main");
        let savings = named("Savings");
        let session = ImportSessionId::new_random();
        let first = asked(session, main.id, 1);
        let second = asked(ImportSessionId::new_random(), savings.id, 1);

        let actions = actions_from_state(&OwnerState {
            accounts: &[main.clone(), savings.clone()],
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: &[first.clone(), second.clone()],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        let items: Vec<_> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::AnswerClassificationQuestion)
            .collect();
        assert_eq!(items.len(), 2, "{actions:?}");
        let identities: BTreeSet<_> = items.iter().map(|item| item.id()).collect();
        assert_eq!(identities.len(), 2, "{identities:?}");
        let subjects: BTreeSet<_> = items
            .iter()
            .filter_map(|item| item.subject().and_then(ActionSubject::account))
            .collect();
        assert_eq!(subjects, BTreeSet::from([main.id, savings.id]));
    }

    /// Nothing but the store is consulted, and the answer removes the item.
    ///
    /// This is the defect in one test: the question reaches the queue from the
    /// two persisted rows, with no ingest response anywhere in the call.
    #[tokio::test]
    async fn a_recorded_question_reaches_the_frontier_and_the_answer_removes_it() {
        let owner = OwnerId::new_random();
        let store = store();
        let main = account();
        store
            .upsert_account(owner, main.clone())
            .await
            .expect("account");
        let session = store
            .open_import_session(owner, None, None, None)
            .await
            .expect("session");
        let question = unresolved(main.id);
        let recorded = store
            .record_import_question(
                owner,
                session.id,
                1,
                NewImportQuestion {
                    question: serde_json::to_string(&question).expect("question json"),
                    alternatives: serde_json::to_string(&question.alternatives())
                        .expect("alternatives json"),
                    prompt: "Which was it?".to_owned(),
                },
            )
            .await
            .expect("question recorded");

        let actions = frontier(owner, &store, &store).await.expect("frontier");
        let item = only_question_item(&actions);
        assert_eq!(
            item.id(),
            format!("answer_classification_question:{}", recorded.id.inner())
        );
        assert_eq!(
            item.subject().and_then(ActionSubject::account),
            Some(main.id)
        );

        store
            .answer_import_question(
                owner,
                session.id,
                recorded.id,
                serde_json::to_string(&Answer::Paid).expect("answer json"),
            )
            .await
            .expect("answered");

        let after = frontier(owner, &store, &store).await.expect("frontier");
        assert!(
            after
                .iter()
                .all(|action| action.kind() != ActionKind::AnswerClassificationQuestion),
            "answering removes the item: {after:?}"
        );
    }

    // --- Adopting the rule an answer would have written (iaam-4hcy) ---------
    //
    // The defect these cover: `Generalisation::Available` said a rule was
    // possible and none was written, and no item in the queue turned it into
    // one. The owner is the only principal who may, and the queue is where he is
    // told what only he can do.

    /// The row a proposal was learned from, as a rule is tested against it.
    fn shop_row(account: AccountId) -> ClassificationSubject {
        ClassificationSubject {
            account,
            counterparty: Counterparty::Named("Shop One".to_owned()),
            description: Some("card purchase 0001".to_owned()),
            source_kind: Some("card".to_owned()),
            source_category: None,
            movement: None,
            far_side: FarSide::Unstated,
        }
    }

    /// The condition that proposal asks about: one field, per decision 0008.
    fn shop_matcher() -> RuleMatcher {
        RuleMatcher {
            counterparty_account: Some("Shop One".to_owned()),
            description_contains: None,
            kind: None,
            source_category: None,
        }
    }

    /// A question the owner answered under a token that could not generalise.
    fn answered_without_a_rule(
        session: ImportSessionId,
        account: AccountId,
        row: u32,
    ) -> ClassificationQuestion {
        let mut question = asked(session, account, row);
        question.view.answered_at = Some("2026-03-02T00:00:00Z".to_owned());
        question.view.answer = Some(serde_json::to_string(&Answer::Paid).expect("answer json"));
        question.generalisation = Generalisation::Available {
            matcher: shop_matcher(),
            outcome: Classification::ExternalFlow,
        };
        question.subject = Some(shop_row(account));
        question
    }

    fn standing(matcher: RuleMatcher, outcome: Classification) -> ClassificationRule {
        ClassificationRule {
            id: iaam_core::ids::ClassificationRuleId::new_random(),
            version: 1,
            matcher,
            outcome,
        }
    }

    fn adopt_items(actions: &[Action]) -> Vec<&Action> {
        actions
            .iter()
            .filter(|action| action.kind() == ActionKind::AdoptClassificationRule)
            .collect()
    }

    fn queue_for(
        account: &AccountView,
        question: &ClassificationQuestion,
        rules: &[ClassificationRule],
    ) -> Vec<Action> {
        actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions: &[],
            questions: std::slice::from_ref(question),
            rules,
            wanted_accounts: &[],
        })
        .expect("actions from state")
    }

    /// The defect, in one test: the state was reported and no act resolved it.
    #[test]
    fn an_available_generalisation_is_an_item_the_owner_can_act_on() {
        let main = named("Main");
        let session = ImportSessionId::new_random();
        let question = answered_without_a_rule(session, main.id, 3);

        let actions = queue_for(&main, &question, &[]);
        let items = adopt_items(&actions);
        assert_eq!(items.len(), 1, "{actions:?}");
        let item = items[0];

        assert_eq!(
            item.id(),
            format!("adopt_classification_rule:{}", question.view.id.inner()),
            "one identity per question, so two proposals never collapse into one"
        );
        assert_eq!(
            item.category(),
            ActionCategory::Recommended,
            "the row is settled and in the journal; no report is waiting on this"
        );
        assert_eq!(
            item.state(),
            ActionState::NeedsOwnerInput,
            "every field is filled; what is missing is the owner's decision"
        );
        assert_eq!(
            item.required_scope(),
            Some(Scope::Owner),
            "generalising is the administer decision arriving by another door"
        );
        assert_eq!(
            item.subject().and_then(ActionSubject::account),
            Some(main.id)
        );
    }

    /// The act is a call that exists, addressed by the vocabulary the catalogue
    /// resolves — not a route invented for the occasion.
    #[test]
    fn the_item_offers_the_rule_route_with_the_proposal_already_filled_in() {
        let main = named("Main");
        let question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        let actions = queue_for(&main, &question, &[]);
        let item = adopt_items(&actions)[0];

        let ActionTarget::Operation { operation, request } = item.target() else {
            panic!("adopting a rule names one operation");
        };
        assert_eq!(*operation, OperationKey::CreateClassificationRule);
        assert!(
            request.missing.is_empty(),
            "nothing is missing from the request: {:?}",
            request.missing
        );
        assert_eq!(
            request.preset["matcher"]["counterparty_account"],
            serde_json::json!("Shop One")
        );
        assert_eq!(
            request.preset["outcome"]["kind"],
            serde_json::json!("external_flow")
        );
        assert!(
            !request.preset.contains_key("replaces"),
            "a rule that would have been written for one answer replaces nothing"
        );
    }

    /// The completion, and the reason the item terminates at all.
    ///
    /// The question goes on reporting `available` after the owner adopts the
    /// proposal — that is deliberate and it is why the queue reads his rules
    /// instead of the question. Without this the item would never leave the
    /// queue, which is how a queue is learned to be ignored.
    #[test]
    fn a_standing_rule_that_settles_the_row_takes_the_item_away() {
        let main = named("Main");
        let question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        let adopted = standing(shop_matcher(), Classification::ExternalFlow);

        assert!(
            adopt_items(&queue_for(&main, &question, &[adopted])).is_empty(),
            "the rule now settles a row like this one, which is the whole goal"
        );
    }

    /// And it closes on what the rule does, not on how it is spelled.
    ///
    /// A matcher he narrowed before sending — or one he wrote himself last month
    /// — still settles the row. Comparing the stored rule field for field with
    /// the proposal would close the item for exactly one spelling and go on
    /// nagging about every other.
    #[test]
    fn a_rule_he_worded_differently_still_takes_the_item_away() {
        let main = named("Main");
        let question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        let narrowed = standing(
            RuleMatcher {
                counterparty_account: Some("Shop One".to_owned()),
                description_contains: Some("card purchase".to_owned()),
                kind: None,
                source_category: None,
            },
            Classification::ExternalFlow,
        );

        assert!(adopt_items(&queue_for(&main, &question, &[narrowed])).is_empty());
    }

    /// A rule that matches the row and says something else settles nothing.
    #[test]
    fn a_rule_reaching_the_row_with_another_outcome_leaves_the_item_standing() {
        let main = named("Main");
        let question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        let other = standing(shop_matcher(), Classification::Refund);

        assert_eq!(
            adopt_items(&queue_for(&main, &question, &[other])).len(),
            1,
            "the owner answered «paid»; a refund rule is not that decision"
        );
    }

    /// An answer that did write a rule has nothing left to adopt.
    #[test]
    fn an_answer_that_already_wrote_its_rule_queues_nothing() {
        let main = named("Main");
        let mut question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        question.generalisation = Generalisation::Recorded {
            rule: "11111111-1111-4111-8111-111111111111".to_owned(),
        };

        assert!(adopt_items(&queue_for(&main, &question, &[])).is_empty());
    }

    /// The one state no call of anybody's can change stays out of the queue.
    #[test]
    fn a_row_that_can_never_make_a_rule_queues_nothing() {
        let main = named("Main");
        let mut question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        question.generalisation = Generalisation::Impossible;

        assert!(
            adopt_items(&queue_for(&main, &question, &[])).is_empty(),
            "«no rule can be built from this row» is a statement, not work"
        );
    }

    /// An open question generalises nothing yet, so it offers nothing to adopt —
    /// and it still offers the answering item, which is the work that is real.
    #[test]
    fn an_unanswered_question_offers_an_answer_and_not_a_rule() {
        let main = named("Main");
        let question = asked(ImportSessionId::new_random(), main.id, 3);
        let actions = queue_for(&main, &question, &[]);

        assert!(adopt_items(&actions).is_empty());
        assert_eq!(
            only_question_item(&actions).kind(),
            ActionKind::AnswerClassificationQuestion
        );
    }

    /// A row the owner threw away is not evidence to generalise from.
    #[test]
    fn an_abandoned_session_proposes_no_rule_from_the_rows_it_discarded() {
        let main = named("Main");
        let mut question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        question.session_state = ImportSessionState::Abandoned;

        assert!(adopt_items(&queue_for(&main, &question, &[])).is_empty());
    }

    /// A committed session is not an abandoned one. Its rows are in the journal
    /// and the rule is exactly as useful the day after the commit.
    #[test]
    fn a_committed_session_still_offers_the_rule_its_answer_would_have_written() {
        let main = named("Main");
        let mut question = answered_without_a_rule(ImportSessionId::new_random(), main.id, 3);
        question.session_state = ImportSessionState::Committed;

        assert_eq!(adopt_items(&queue_for(&main, &question, &[])).len(), 1);
    }

    // --- A retirement the journal has not caught up with (iaam-xnhu) ---------
    //
    // The defect these cover: `retired_account_not_empty` was published in a
    // report's `confidence` and nowhere else, so the queue — the answer to
    // "what should I do next" — was silent about a retirement that had not
    // taken effect, and the owner found out by asking for the snapshot again.

    fn ceased(account: AccountId, emptied: bool) -> RetiredProduct {
        RetiredProduct {
            account,
            effective_on: date!(2026 - 01 - 10),
            emptied,
        }
    }

    fn retired_items(actions: &[Action]) -> Vec<&Action> {
        actions
            .iter()
            .filter(|action| action.kind() == ActionKind::RetiredAccountNotEmpty)
            .collect()
    }

    fn queue_for_retirement(account: &AccountView, retired: &[RetiredProduct]) -> Vec<Action> {
        actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(retired),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state")
    }

    // --- The unfinished import session in the queue (iaam-8ano) --------------
    //
    // The defect these cover: a session that held rows raised an item only
    // through an unanswered question, so a session whose questions were all
    // answered — or that raised none, which is what a clean statement does —
    // stood open holding rows and appeared in no item. A caller that reads the
    // queue and finds nothing outstanding concludes the import finished, and
    // the next act is to import the same statement again.

    fn session_summary(
        state: ImportSessionState,
        row_count: usize,
        unanswered: usize,
    ) -> ImportSessionSummaryView {
        ImportSessionSummaryView {
            session: ImportSessionView {
                id: ImportSessionId::new_random(),
                state,
                account: None,
                source: None,
                import: None,
                opened_at: "2026-03-01T00:00:00Z".to_owned(),
                closed_at: match state {
                    ImportSessionState::Open => None,
                    ImportSessionState::Committed | ImportSessionState::Abandoned => {
                        Some("2026-03-02T00:00:00Z".to_owned())
                    }
                },
            },
            row_count,
            unanswered,
        }
    }

    fn session_items(actions: &[Action]) -> Vec<&Action> {
        actions
            .iter()
            .filter(|action| action.kind() == ActionKind::ImportSessionUnfinished)
            .collect()
    }

    fn queue_for_sessions(
        accounts: &[AccountView],
        sessions: &[ImportSessionSummaryView],
        questions: &[ClassificationQuestion],
    ) -> Vec<Action> {
        actions_from_state(&OwnerState {
            accounts,
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[]),
            sessions,
            questions,
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state")
    }

    /// The item exists, and it publishes every call that reaches the state.
    ///
    /// The order is asserted, not merely the set: the ordinary cause is a
    /// journal that is short of the opening, and a client acting on the first
    /// resolution must be acting on the remedy for the ordinary cause. It was
    /// the other way round in the register for a wave, and an agent that had
    /// just retired the account read the first entry as «retire it again».
    #[test]
    fn a_retirement_the_journal_disagrees_with_is_queued_with_its_three_ways_out() {
        let term = named("Term");
        let actions = queue_for_retirement(&term, &[ceased(term.id, false)]);
        let items = retired_items(&actions);
        assert_eq!(items.len(), 1, "{actions:?}");

        let published: Vec<OperationKey> = items[0]
            .target()
            .resolutions()
            .into_iter()
            .map(|(operation, _)| operation)
            .collect();
        assert_eq!(
            published,
            vec![
                OperationKey::SubmitOperations,
                OperationKey::SubmitCorrections,
                OperationKey::RecordAccountRetirement,
            ]
        );
        // The same calls the register names for the same state, and in the same
        // order. Two lists that must agree are one list, so this compares them
        // rather than restating either.
        assert_eq!(
            published,
            CaveatKind::RetiredAccountNotEmpty.closed_by().to_vec()
        );
        assert_eq!(
            items[0].kind().id(),
            CaveatKind::RetiredAccountNotEmpty.code()
        );
    }

    /// The item publishes the narrowest of its three floors, and each
    /// resolution publishes its own (`iaam-woeh`).
    ///
    /// This is the item the finding was filed on. `ingest_operations` keeps
    /// [`Scope::Agent`] and the other two keep [`Scope::Owner`], so a single
    /// grading had to lie in one direction or the other; it said `owner`, and
    /// an agent filtering the queue by its own scope dropped the item and never
    /// reached the call it could in fact make.
    #[test]
    fn the_retirement_item_admits_an_agent_to_the_one_call_that_admits_one() {
        let term = named("Term");
        let actions = queue_for_retirement(&term, &[ceased(term.id, false)]);
        let items = retired_items(&actions);

        assert_eq!(
            items[0].required_scope(),
            Some(Scope::Agent),
            "the ordinary remedy admits an agent, so the item does"
        );
        let floors: Vec<Scope> = items[0]
            .target()
            .resolutions()
            .into_iter()
            .map(|(operation, _)| required_scope(operation))
            .collect();
        assert_eq!(
            floors,
            vec![Scope::Agent, Scope::Owner, Scope::Owner],
            "the three ways out do not want one authority"
        );
    }

    /// A journal that will not fold produces an item, not a failed request
    /// (`iaam-4jso`).
    ///
    /// Three things at once, because they are one promise: the request
    /// succeeds, the retirement item is **not** raised — nothing is guessed —
    /// and an item stands in its place saying the question could not be
    /// answered and naming the call that repairs the journal.
    #[test]
    fn a_journal_that_will_not_fold_is_an_item_and_not_a_failed_queue() {
        let term = named("Term");
        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&term),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::NotAssessed("event A references non-existent B"),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("a fold that refused must not take the queue with it");

        assert!(
            retired_items(&actions).is_empty(),
            "an unanswerable question must not be answered: {actions:?}"
        );
        let item = actions
            .iter()
            .find(|action| action.kind() == ActionKind::RetirementNotAssessed)
            .expect("the fold's refusal is published as an item");
        assert_eq!(item.state(), ActionState::NeedsOwnerInput);
        assert_eq!(
            item.category(),
            ActionCategory::required_for(ActionKind::RetirementNotAssessed)
        );
        assert_eq!(
            item.subject(),
            None,
            "the subject is the journal, and no account is what refused"
        );
        assert_eq!(item.required_scope(), Some(Scope::Owner));
        assert_eq!(
            item.target().resolutions(),
            vec![(
                OperationKey::SubmitCorrections,
                &RequestPlan {
                    preset: BTreeMap::new(),
                    missing: vec![
                        MissingInput::asked(OwnerPrompt::Corrections),
                        MissingInput::asked(OwnerPrompt::AcknowledgeRetraction),
                    ],
                },
            )]
        );
        assert!(
            item.reason().contains("event A references non-existent B"),
            "the item names what refused: {}",
            item.reason()
        );
    }

    /// A fold that refused and a fold that found nothing are two states, and
    /// only one of them raises the item.
    ///
    /// The encoding is what makes this checkable: an empty slice would have
    /// spelled both, and «he has retired nothing» would then have been
    /// indistinguishable from «nobody could tell».
    #[test]
    fn an_owner_who_has_retired_nothing_gets_no_unassessed_item() {
        let term = named("Term");
        let actions = queue_for_retirement(&term, &[]);
        assert!(
            !actions
                .iter()
                .any(|action| action.kind() == ActionKind::RetirementNotAssessed),
            "{actions:?}"
        );
    }

    /// The completion is the journal, not the declaration and not the caveat.
    ///
    /// `iaam-4hcy`'s rule on a second state: whoever emptied the account and
    /// however — a reconstructed opening, a retraction, an ordinary import that
    /// happened to carry the missing outflow — the disagreement is over and the
    /// item goes. The retirement itself still stands and is not withdrawn.
    #[test]
    fn an_emptied_product_leaves_the_queue_with_its_retirement_still_standing() {
        let term = named("Term");
        assert!(
            retired_items(&queue_for_retirement(&term, &[ceased(term.id, true)])).is_empty(),
            "an item whose goal is met is an item the owner learns to ignore"
        );
        assert!(retired_account_completion(&ceased(term.id, true)));
        assert!(!retired_account_completion(&ceased(term.id, false)));
    }

    /// A withdrawn retirement removes the item through the eligibility, not the
    /// goal: the store returns only the statements in force, so there is no
    /// declaration left for the journal to disagree with.
    #[test]
    fn withdrawing_the_statement_removes_the_item_without_emptying_anything() {
        let term = named("Term");
        assert!(retired_items(&queue_for_retirement(&term, &[])).is_empty());
    }

    /// The figure is the owner's and nothing here holds it.
    ///
    /// The opening amount is what the account held before this system knew
    /// anything about it. Presetting a guess would put an invented number into
    /// the one call whose purpose is to state a real one, so it is published as
    /// missing and attributed to the only source that has it.
    #[test]
    fn the_opening_it_offers_asks_the_owner_for_the_amount_and_presets_the_rest() {
        let term = named("Term");
        let actions = queue_for_retirement(&term, &[ceased(term.id, false)]);
        let items = retired_items(&actions);
        let resolutions = items[0].target().resolutions();
        let (operation, plan) = resolutions[0];
        assert_eq!(operation, OperationKey::SubmitOperations);

        assert_eq!(
            plan.preset.get("operations"),
            Some(&serde_json::json!([{
                "account": term.id.inner().to_string(),
                "type": "opening_cash",
            }])),
            "the account and the kind of row are the whole of what the policy knows"
        );
        let asked: Vec<(&str, ProvidedBy)> = plan
            .missing
            .iter()
            .map(|input| (input.pointer.as_str(), input.provided_by))
            .collect();
        assert_eq!(
            asked,
            vec![
                ("/operations/0/amount", ProvidedBy::Owner),
                ("/operations/0/currency", ProvidedBy::Owner),
                ("/operations/0/dates/cash_posted", ProvidedBy::Owner),
                ("/operations/0/idempotency_key", ProvidedBy::Caller),
            ]
        );
    }

    /// The withdrawal names the direction rather than leaving the word open.
    ///
    /// Recording a second retirement over one that stands is refused, so an
    /// option that asked the caller for `state` would publish the act that
    /// produced the item as the way out of it — which is `iaam-bhu3` restated
    /// one layer up.
    #[test]
    fn the_withdrawal_it_offers_is_written_out_and_asks_for_nothing() {
        let term = named("Term");
        let actions = queue_for_retirement(&term, &[ceased(term.id, false)]);
        let items = retired_items(&actions);
        let resolutions = items[0].target().resolutions();
        let (operation, plan) = resolutions[2];

        assert_eq!(operation, OperationKey::RecordAccountRetirement);
        assert_eq!(
            plan.preset.get("state"),
            Some(&serde_json::Value::from("in_use"))
        );
        assert_eq!(
            plan.preset.get("id"),
            Some(&serde_json::Value::from(term.id.inner().to_string()))
        );
        assert!(plan.missing.is_empty(), "{plan:?}");
    }

    /// Two ceased products that both still hold something are two items.
    #[test]
    fn two_retired_products_that_still_hold_something_get_distinct_identities() {
        let first = named("Term");
        let second = named("Savings");
        let actions = actions_from_state(&OwnerState {
            accounts: &[first.clone(), second.clone()],
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[],
            assertions: &[],
            retired: RetirementAssessment::Assessed(&[
                ceased(first.id, false),
                ceased(second.id, false),
            ]),
            sessions: &[],
            questions: &[],
            rules: &[],
            wanted_accounts: &[],
        })
        .expect("actions from state");

        let identities: Vec<&str> = retired_items(&actions)
            .into_iter()
            .map(Action::id)
            .collect();
        assert_eq!(identities.len(), 2);
        assert_ne!(identities[0], identities[1]);
    }

    /// The defect, in one test: rows held, nothing to answer, nothing in the
    /// queue.
    #[test]
    fn a_session_holding_rows_with_nothing_to_answer_is_still_an_item() {
        let main = named("Main");
        let held = session_summary(ImportSessionState::Open, 4, 0);

        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&held),
            &[],
        );
        let items = session_items(&actions);
        assert_eq!(items.len(), 1, "{actions:#?}");
        assert_eq!(
            items[0].id(),
            format!("import_session_unfinished:{}", held.session.id.inner()),
            "one identity per session, so two half-finished imports never collapse"
        );
        assert_eq!(
            items[0].category(),
            ActionCategory::required_for(ActionKind::ImportSessionUnfinished),
            "the rows are in no journal, so every report is computed without them"
        );
        assert_eq!(items[0].state(), ActionState::NeedsOwnerInput);
        assert_eq!(items[0].required_scope(), Some(Scope::Agent));
    }

    /// The item publishes the two calls that end a session, and only those.
    ///
    /// Answering a question is not among them however many are open: a
    /// resolution is a call that closes **this** item, and an answer leaves the
    /// session exactly as open as it found it.
    #[test]
    fn the_item_offers_the_two_calls_that_end_a_session() {
        let main = named("Main");
        let held = session_summary(ImportSessionState::Open, 2, 0);
        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&held),
            &[],
        );
        let item = session_items(&actions)[0];

        let resolutions = item.target().resolutions();
        assert_eq!(
            resolutions
                .iter()
                .map(|(operation, _)| *operation)
                .collect::<Vec<_>>(),
            vec![
                OperationKey::CommitImportSession,
                OperationKey::AbandonImportSession,
            ],
            "committing is the way on and abandoning the way out, in that order"
        );
        for (_, request) in &resolutions {
            assert_eq!(
                request.preset["session"],
                serde_json::Value::from(held.session.id.inner().to_string()),
                "both calls address the session this item is about"
            );
        }
        // The assessment is a GET with no request body, so it is not an
        // `OperationKey` and cannot be a target. It is named in the prose
        // instead, by the field every session response publishes it in.
        assert!(item.reason().contains("assessment"), "{}", item.reason());
    }

    /// A session that holds nothing is not work: it is what a caller retrying
    /// the open call is handed back.
    #[test]
    fn an_empty_session_raises_nothing() {
        let main = named("Main");
        let empty = session_summary(ImportSessionState::Open, 0, 0);

        assert!(!import_session_eligibility(&empty));
        assert!(
            session_items(&queue_for_sessions(
                std::slice::from_ref(&main),
                std::slice::from_ref(&empty),
                &[],
            ))
            .is_empty()
        );
    }

    /// The goal is that the session stopped being open, and both endings do it.
    ///
    /// Abandoning satisfies this where it deliberately does not satisfy the
    /// question item: there it would stand in for the owner saying what a row
    /// was, and here the item makes no claim about any row.
    #[test]
    fn committing_and_abandoning_both_close_the_item() {
        let main = named("Main");
        for ended in [ImportSessionState::Committed, ImportSessionState::Abandoned] {
            let session = session_summary(ended, 7, 0);
            assert!(import_session_completion(&session), "{ended:?}");
            assert!(
                session_items(&queue_for_sessions(
                    std::slice::from_ref(&main),
                    std::slice::from_ref(&session),
                    &[],
                ))
                .is_empty(),
                "{ended:?}"
            );
        }
    }

    /// A question does not stand in for the session, in either direction.
    ///
    /// The completion is quantified over the session and not over its
    /// questions, which is the opposite of `classification_question_completion`
    /// and for the opposite reason. So a session with an open question raises
    /// **both** items — they say different things — and answering that question
    /// leaves this one where it was.
    #[test]
    fn answering_the_last_question_does_not_end_the_session() {
        let main = named("Main");
        let mut waiting = session_summary(ImportSessionState::Open, 3, 1);
        let question = asked(waiting.session.id, main.id, 1);

        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&waiting),
            std::slice::from_ref(&question),
        );
        assert_eq!(session_items(&actions).len(), 1, "{actions:#?}");
        assert_eq!(
            actions
                .iter()
                .filter(|action| action.kind() == ActionKind::AnswerClassificationQuestion)
                .count(),
            1,
            "the row and the session are two facts, and each has its own item"
        );

        // Every question answered, the session unchanged: the item stands.
        waiting.unanswered = 0;
        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&waiting),
            &[],
        );
        assert_eq!(session_items(&actions).len(), 1, "{actions:#?}");
    }

    /// Two sessions are two items, whatever they hold.
    ///
    /// Identity is what an agent deduplicates and tracks by, and one shared
    /// identity would make the second import invisible — which is this item's
    /// own defect, one level down.
    #[test]
    fn two_open_sessions_are_two_items_with_distinct_identities() {
        let main = named("Main");
        let sessions = [
            session_summary(ImportSessionState::Open, 1, 0),
            session_summary(ImportSessionState::Open, 9, 2),
        ];

        let actions = queue_for_sessions(std::slice::from_ref(&main), &sessions, &[]);
        let items = session_items(&actions);
        assert_eq!(items.len(), 2, "{actions:#?}");
        assert_ne!(items[0].id(), items[1].id());
    }

    /// A declared session names the account it was declared for; a free one
    /// names none rather than the account of whichever row came first.
    #[test]
    fn the_item_names_the_account_the_session_was_declared_for() {
        let main = named("Main");
        let mut declared = session_summary(ImportSessionState::Open, 2, 0);
        declared.session.account = Some(main.id);

        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&declared),
            &[],
        );
        assert_eq!(
            session_items(&actions)[0]
                .subject()
                .and_then(ActionSubject::account),
            Some(main.id)
        );

        let free = session_summary(ImportSessionState::Open, 2, 0);
        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&free),
            &[],
        );
        assert!(session_items(&actions)[0].subject().is_none());
    }

    /// The queue says how much is at stake without being asked again.
    ///
    /// The counts are why the listing carries them: an item that said only
    /// «a session is open» would send its reader back to fetch the session to
    /// find out whether anything is in it.
    #[test]
    fn the_reason_says_what_is_held_and_what_is_unanswered() {
        let main = named("Main");
        let waiting = session_summary(ImportSessionState::Open, 5, 2);
        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&waiting),
            &[],
        );
        let reason = session_items(&actions)[0].reason().to_owned();

        assert!(reason.contains("5 rows"), "{reason}");
        assert!(reason.contains("2 of its questions"), "{reason}");
        assert!(
            reason.contains(&waiting.session.id.inner().to_string()),
            "{reason}"
        );

        let settled = session_summary(ImportSessionState::Open, 1, 0);
        let actions = queue_for_sessions(
            std::slice::from_ref(&main),
            std::slice::from_ref(&settled),
            &[],
        );
        let reason = session_items(&actions)[0].reason().to_owned();
        assert!(reason.contains("1 row"), "{reason}");
        assert!(reason.contains("can be committed"), "{reason}");
    }
}
