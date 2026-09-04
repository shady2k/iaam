use std::collections::BTreeMap;

use crate::error::AppError;
use crate::ports::{
    AccountActivityView, AccountScopeExclusionView, AccountTransferStatementView, AccountView,
    ClassificationRuleStore, ContourView, ControlAssertionView, ImportQuestionView,
    ImportSessionState, Scope, Store,
};
use crate::scenarios::classification::{matcher_request_json, outcome_json, rule_from_view};
use crate::scenarios::import_session::{self, Generalisation};
use crate::scenarios::reports::MoneyFlowReport;
use iaam_core::event::source_row::RowName;
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::money::{CurrencyCode, Money};
use iaam_core::projection::money_flow::UndecomposedCause;
use iaam_core::reconciliation::check::{ClaimOutcome, ClaimValue, Discrepancy};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger, Taint};
use iaam_ingest::Verdict;
use iaam_ingest::classification::{
    Classification, ClassificationRule, ClassificationSubject, Question, RuleMatcher,
};

/// The policy-visible kind of an outstanding action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKind {
    CreateFirstAccount,
    CreateFirstContour,
    AccountScopeUndecided,
    /// The owner has not said which of his accounts money moves between this
    /// one and. A discovery goal: it is asked before anything is imported.
    ResolveTransferRelationships,
    StartAccountImport,
    ProvideControlAssertion,
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
            Self::CreateFirstContour => "create_first_contour",
            Self::AccountScopeUndecided => "account_scope_undecided",
            Self::ResolveTransferRelationships => "resolve_transfer_relationships",
            Self::StartAccountImport => "start_account_import",
            Self::ProvideControlAssertion => "provide_control_assertion",
            Self::AnswerClassificationQuestion => "answer_classification_question",
            Self::AdoptClassificationRule => "adopt_classification_rule",
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
    pub const ALL: [Self; 15] = [
        Self::CreateFirstAccount,
        Self::CreateFirstContour,
        Self::AccountScopeUndecided,
        Self::ResolveTransferRelationships,
        Self::StartAccountImport,
        Self::ProvideControlAssertion,
        Self::AnswerClassificationQuestion,
        Self::AdoptClassificationRule,
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
    /// Exhaustive on purpose. A sixteenth kind cannot compile until someone has
    /// answered, for that kind, the question this whole type exists to answer.
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
    /// - `StartAccountImport`, `AnswerClassificationQuestion` — a row that is in
    ///   no journal is in no report, and an account with no facts has nothing
    ///   for any of the four to say.
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
    #[must_use]
    pub const fn goals(self) -> ReportGoals {
        use ReportGoal::{AssetSnapshot, MoneyFlow, Reconciliation, Returns};
        match self {
            // Blocking, not required work: no goal.
            Self::CreateFirstAccount => ReportGoals::NONE,
            Self::CreateFirstContour | Self::AccountScopeUndecided => {
                ReportGoals::of(&[AssetSnapshot, MoneyFlow, Returns])
            }
            Self::ResolveTransferRelationships => ReportGoals::of(&[MoneyFlow, Returns]),
            Self::StartAccountImport
            | Self::AnswerClassificationQuestion
            | Self::PossibleDuplicateUndecided => ReportGoals::ALL,
            Self::ProvideControlAssertion => ReportGoals::of(&[AssetSnapshot, Reconciliation]),
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
/// is not a missing field of any request this type describes: it is the body of
/// `POST /v1/import-sessions/{session}/rows`, a later call, and a pointer into
/// it could not be satisfied by filling in the request it was published on. What
/// the item gains instead is a sentence naming the shape a row is submitted in,
/// which is a fact about this API and therefore something the queue may state.
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

/// One required request field not supplied by the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingInput {
    pub pointer: String,
    pub provided_by: ProvidedBy,
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
    pub candidates: Option<Vec<AccountCandidate>>,
}

impl MissingInput {
    /// A field with no closed value set and no account candidates.
    fn plain(pointer: &str, provided_by: ProvidedBy) -> Self {
        Self {
            pointer: pointer.to_owned(),
            provided_by,
            candidates: None,
            alternatives: Vec::new(),
        }
    }
}

/// Request information attached to an operation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPlan {
    pub preset: BTreeMap<String, serde_json::Value>,
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
    BlockedWithScope,
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
    pub required_scope: Option<Scope>,
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
    required_scope: Option<Scope>,
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
        if facts.state == ActionState::Blocked && facts.required_scope.is_some() {
            return Err(ActionInvariantError::BlockedWithScope);
        }
        if facts.state != ActionState::Blocked && facts.required_scope.is_none() {
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
            required_scope: facts.required_scope,
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

    pub const fn required_scope(&self) -> Option<Scope> {
        self.required_scope
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
    // The two reads that make a question outlive the response that raised it.
    // Every session is asked, not only the open ones: eligibility is a property
    // of the item and is decided beside the gap and the completion, not by
    // narrowing what is loaded.
    let mut questions = Vec::new();
    for session in store.list_import_sessions(owner).await? {
        let held = store.list_import_questions(owner, session.id).await?;
        if held.is_empty() {
            // The observations are read only to derive what a question's answer
            // generalised into, so a session that raised none is not read at
            // all. Every import the owner ever ran is listed here, and most of
            // them asked nothing.
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
        questions: &questions,
        rules: &rules,
    })
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
/// `Scope::Agent` on the promoted half, for the reason
/// `start_account_import_action` gives: `sync_broker` checks `may_submit`, which
/// an agent token satisfies, and marking the item owner-only would tell an agent
/// it may not send a request the server would accept.
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
            required_scope: Some(Scope::Agent),
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
                missing: vec![MissingInput::plain("/broker", ProvidedBy::Owner)],
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
/// `Scope::Owner`: `submit_corrections` is behind `require_admin`, and it is
/// there so that an agent token cannot retract the owner's history.
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
            required_scope: Some(Scope::Owner),
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
                    MissingInput::plain("/corrections", ProvidedBy::Owner),
                    MissingInput::plain("/acknowledge_retraction", ProvidedBy::Owner),
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
/// completed contract, not a report-local namespace, and owner-only is what
/// `required_scope` says, not what `Blocked` says. `first_contour_action` is the
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
            required_scope: Some(Scope::Owner),
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
                    MissingInput {
                        pointer: "/matcher".into(),
                        provided_by: ProvidedBy::Owner,
                        candidates: None,
                        alternatives: Vec::new(),
                    },
                    MissingInput {
                        pointer: "/category".into(),
                        provided_by: ProvidedBy::Owner,
                        candidates: None,
                        alternatives: Vec::new(),
                    },
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
            required_scope: None,
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
    questions: &'a [ClassificationQuestion],
    /// The owner's standing classification rules, as the classifier reads them.
    ///
    /// Read for one purpose: to find out whether a proposal the queue would
    /// offer has already been adopted. Nothing else here consults them, and an
    /// empty slice is «he has written none», never «they were not fetched».
    rules: &'a [ClassificationRule],
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
        questions,
        rules,
    } = *state;
    let names = AccountNames::new(accounts);
    let mut actions = actions_from_views(accounts, contours, exclusions, transfers);
    actions.reserve(activity.len() + assertions.len() + questions.len());
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
/// `Scope::Agent`, and this is the first item whose scope is not `Scope::Owner`.
/// The route that answers checks `may_submit`, which an agent token satisfies,
/// and the queue's business is to say what may be called: an item marked `owner`
/// would tell an agent it may not send a request the server would accept. Who
/// decides the answer and who may transmit it are different questions, and
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
            required_scope: Some(Scope::Agent),
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
/// owner», which this must never be. `Scope::Owner`, because generalising is the
/// administer decision arriving by another door — the same gate
/// `may_generalise` reads when it declines to write the rule in the first place.
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
            required_scope: Some(Scope::Owner),
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
        pointer: "/answer".to_owned(),
        provided_by: ProvidedBy::Owner,
        candidates: None,
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
                        pointer: "/account".to_owned(),
                        provided_by: ProvidedBy::Owner,
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
) -> Vec<Action> {
    let account_completion = account_completion(accounts);
    let contour_eligibility = !accounts.is_empty();
    let contour_completion = contour_completion(contours);
    let contour_gap = !contour_completion;
    let mut actions = Vec::with_capacity(2);

    if !account_completion {
        actions.push(first_account_action());
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
            required_scope: Some(Scope::Owner),
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
                missing: vec![MissingInput {
                    pointer: "/partners".into(),
                    provided_by: ProvidedBy::Owner,
                    candidates: Some(account_candidates(
                        &accounts
                            .iter()
                            .filter(|candidate| candidate.id != account.id)
                            .cloned()
                            .collect::<Vec<_>>(),
                    )),
                    alternatives: Vec::new(),
                }],
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
/// Two options rather than one, ordered and not ranked, which is what
/// `account_scope_action` established `Options` for: a statement is the ordinary
/// answer and a broker channel is the answer for the accounts that have one, and
/// publishing either alone would leave the other reachable only by reading the
/// specification.
///
/// `Scope::Agent`, for the reason `answer_classification_question_action` gives:
/// both routes check `may_submit`, which an agent token satisfies, and an item
/// marked `owner` would tell an agent it may not send a request the server would
/// accept.
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
                MissingInput::plain("/source/channel", ProvidedBy::Caller),
                // The label is what makes this import retractable on its own,
                // and it is a statement period or an export file name — it is
                // read off the document the owner fetched, which is why this is
                // the first field marked `ExternalDocument`. Optional in the
                // schema and published as missing anyway, on the same ground as
                // `/cash` in the control assertion: `missing` states what the
                // plan needs supplied, and a plan that quietly omitted it would
                // produce unlabelled rows retractable only together with every
                // other unlabelled row of the same account and channel.
                MissingInput::plain("/source/label", ProvidedBy::ExternalDocument),
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
                MissingInput::plain("/broker", ProvidedBy::Owner),
                MissingInput::plain("/from", ProvidedBy::Owner),
                MissingInput::plain("/to", ProvidedBy::Owner),
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
            required_scope: Some(Scope::Agent),
            subject: Some(ActionSubject::Account(AccountSubject::of(account))),
        },
        format!(
            "Account {} ({}) has no business facts; import a statement or connect a broker. \
             Fetching the statement out of the bank is a step outside this API — no \
             operation here downloads the document, and the owner obtains it himself. \
             Recording it is not: open an import session for this account and feed it the \
             rows the document printed. Deciding what a row was is not a step between \
             those two — a row whose direction or nature the reader cannot tell is sent \
             as `unresolved_direction`, carrying the source's own sign, its direction \
             word and the party it named, and the session settles it against the owner's \
             accounts and rules or asks him about it. Then read the assessment the \
             session publishes to see what committing would record and what it would \
             not, and commit under the revision that assessment carries; or synchronise \
             a broker channel over an interval. An import already \
             under way for this account is not something this item can see — a session \
             records the source and the import it was opened for, and neither can be read \
             back as an account — so opening one again is what finds it: the call refuses, \
             names the session, and publishes the calls that end it. Import is \
             continuous and never complete.",
            account.id.inner(),
            account.title
        ),
        ActionTarget::from_options(vec![session, sync]),
    )
    .expect("account import action publishes both of its resolutions")
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
            required_scope: Some(Scope::Owner),
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
                missing: vec![MissingInput::plain("/cash", ProvidedBy::Owner)],
            },
        },
    )
    .expect("control assertion action has an operation target")
}

fn first_account_action() -> Action {
    Action::new(
        ActionFacts {
            id: identity(ActionKind::CreateFirstAccount),
            kind: ActionKind::CreateFirstAccount,
            category: ActionCategory::Blocking,
            state: ActionState::NeedsOwnerInput,
            required_scope: Some(Scope::Owner),
            // Existential: no account exists, so the item names none.
            subject: None,
        },
        "No account exists; create one before portfolio actions can be offered.",
        ActionTarget::Operation {
            operation: OperationKey::CreateAccount,
            request: RequestPlan {
                preset: BTreeMap::new(),
                missing: vec![MissingInput::plain("/title", ProvidedBy::Owner)],
            },
        },
    )
    .expect("first account action has an operation target")
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
                MissingInput {
                    pointer: "/contour".into(),
                    provided_by: ProvidedBy::Owner,
                    candidates: None,
                    alternatives: Vec::new(),
                },
                MissingInput {
                    pointer: "/accounts".into(),
                    provided_by: ProvidedBy::Owner,
                    candidates: Some(account_candidates(accounts)),
                    alternatives: Vec::new(),
                },
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
            missing: vec![MissingInput::plain("/reason", ProvidedBy::Owner)],
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
            required_scope: Some(Scope::Owner),
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
            required_scope: Some(Scope::Owner),
            // Existential: no contour exists, so the item names no one account.
            subject: None,
        },
        "No contour exists; report boundaries cannot be computed until one is created.",
        ActionTarget::Operation {
            operation: OperationKey::CreateContour,
            request: RequestPlan {
                preset: BTreeMap::new(),
                missing: vec![
                    MissingInput {
                        pointer: "/title".into(),
                        provided_by: ProvidedBy::Owner,
                        candidates: None,
                        alternatives: Vec::new(),
                    },
                    MissingInput {
                        pointer: "/accounts".into(),
                        provided_by: ProvidedBy::Owner,
                        candidates: Some(candidates),
                        alternatives: Vec::new(),
                    },
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
    use iaam_ingest::classification::{Answer, Counterparty, FarSide};
    use iaam_store::SqliteStore;
    use std::collections::BTreeSet;
    use time::macros::date;

    fn store() -> SqliteAdapter {
        SqliteAdapter::new(SqliteStore::open_in_memory().expect("in-memory store"))
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
        assert_eq!(ActionKind::ALL.len(), 15, "a kind was added without a goal");
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
    /// going stale. A fifteenth kind does not slip through — it stops the test
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
                // has nothing for any of the four to say; and a possible
                // duplicate **is** recorded, so it may be the same money twice.
                ActionKind::StartAccountImport
                | ActionKind::AnswerClassificationQuestion
                | ActionKind::PossibleDuplicateUndecided => {
                    &[AssetSnapshot, MoneyFlow, Returns, Reconciliation]
                }
                // The closing assertion is reconciliation's claim side; the
                // opening one is what makes the snapshot's cash a balance rather
                // than movement, which `account_balances` decides per account
                // and currency. It has no legs, so it moves no number in flow or
                // returns.
                ActionKind::ProvideControlAssertion => &[AssetSnapshot, Reconciliation],
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
                    required_scope: Some(Scope::Owner),
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
                missing: vec![MissingInput::plain("/title", ProvidedBy::Owner)],
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
                    required_scope: Some(Scope::Owner),
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
            questions: &[],
            rules: &[],
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
        // wrong. It is no longer blocked — two operations begin an import — so
        // what it witnesses now is the other half of the same rule: an item that
        // names operations must not say `blocked`.
        let import = actions
            .iter()
            .find(|action| action.kind() == ActionKind::StartAccountImport)
            .expect("account import action");
        assert_ne!(import.state(), ActionState::Blocked);
        assert_eq!(import.target().resolutions().len(), 2);
        assert_eq!(
            import.subject().and_then(ActionSubject::account),
            Some(account.id)
        );
    }

    #[test]
    fn losing_contour_eligibility_is_not_contour_completion() {
        let account = account();
        let eligible = actions_from_views(&[account], &[], &[], &[]);
        let ineligible = actions_from_views(&[], &[], &[], &[]);

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
            questions: &[],
            rules: &[],
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

        let actions = actions_from_views(&accounts, &[], &[], &[]);
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
            actions_from_views(std::slice::from_ref(&only), &[], &[], &[])
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
            actions_from_views(&accounts, &[], &[], &statements)
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
        let asked: Vec<_> = actions_from_views(&[main, savings.clone()], &[], &[], &statements)
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
            actions_from_views(&[main.clone(), savings.clone()], &[], &[], &statements)
                .iter()
                .all(|action| action.kind() != ActionKind::ResolveTransferRelationships)
        );

        let everyday = named("Everyday");
        let asked: Vec<_> =
            actions_from_views(&[main, savings, everyday.clone()], &[], &[], &statements)
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

        let asked: Vec<_> = actions_from_views(&accounts, &contours, &exclusions, &[])
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

        let asked = actions_from_views(&accounts, &contours, &exclusions, &[])
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
            questions: &[],
            rules: &[],
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
                required_scope: Some(Scope::Owner),
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

    /// An account with nothing in it is offered both routes that begin an import.
    ///
    /// The item used to be `Blocked`, on a sentence that was true of fetching the
    /// document and of nothing else. An agent reads `state` as its map of what it
    /// may call, so the queue disowned the two routes that do exist.
    #[test]
    fn an_account_awaiting_its_first_import_names_both_routes_that_begin_one() {
        let account = account();
        let actions = actions_from_state(&OwnerState {
            accounts: std::slice::from_ref(&account),
            contours: &[],
            exclusions: &[],
            transfers: &[],
            activity: &[no_facts(account.id)],
            assertions: &[],
            questions: &[],
            rules: &[],
        })
        .expect("actions from state");
        let import = actions
            .iter()
            .find(|action| action.kind() == ActionKind::StartAccountImport)
            .expect("account import action");

        assert_eq!(import.state(), ActionState::NeedsOwnerInput);
        // Both routes check `may_submit`, so an agent may send either.
        assert_eq!(import.required_scope(), Some(Scope::Agent));

        let resolutions = import.target().resolutions();
        assert_eq!(
            resolutions
                .iter()
                .map(|(operation, _)| *operation)
                .collect::<Vec<_>>(),
            vec![OperationKey::OpenImportSession, OperationKey::SyncBroker],
            "the statement is the ordinary answer and the broker channel the other"
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

        // The sync knows the account; which broker, and over what interval, are
        // the owner's to name.
        let sync = resolutions[1].1;
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
            questions: &[],
            rules: &[],
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
                questions: &[],
                rules: &[],
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
            questions: &[],
            rules: &[],
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
            questions: &[],
            rules: &[],
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
            questions: &[],
            rules: &[],
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
                questions: &[],
                rules: &[],
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
                required_scope: None,
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
    fn blocked_action_rejects_an_operation_and_a_scope() {
        let operation = Action::new(
            ActionFacts {
                id: "blocked-operation".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::Blocked,
                required_scope: None,
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

        let scope = Action::new(
            ActionFacts {
                id: "blocked-scope".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::Blocked,
                required_scope: Some(Scope::Owner),
                subject: None,
            },
            "nothing can call this",
            ActionTarget::None,
        );
        assert_eq!(scope, Err(ActionInvariantError::BlockedWithScope));
    }

    #[test]
    fn a_nonblocked_action_requires_a_scope() {
        let result = Action::new(
            ActionFacts {
                id: "missing-scope".to_owned(),
                kind: ActionKind::CreateFirstAccount,
                category: ActionCategory::Blocking,
                state: ActionState::NeedsOwnerInput,
                required_scope: None,
                subject: None,
            },
            "invalid",
            ActionTarget::Operation {
                operation: OperationKey::CreateAccount,
                request: RequestPlan {
                    preset: BTreeMap::new(),
                    missing: Vec::new(),
                },
            },
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
            questions: std::slice::from_ref(&question),
            rules: &[],
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
            questions: std::slice::from_ref(&question),
            rules: &[],
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
            questions: std::slice::from_ref(&question),
            rules: &[],
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
            questions: std::slice::from_ref(&question),
            rules: &[],
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
            questions: std::slice::from_ref(&question),
            rules: &[],
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
            questions: std::slice::from_ref(&question),
            rules: &[],
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
                questions: std::slice::from_ref(&question),
                rules: &[],
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
            questions: &[first.clone(), second.clone()],
            rules: &[],
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
            questions: std::slice::from_ref(question),
            rules,
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
}
