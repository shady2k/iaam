use std::collections::BTreeMap;

use crate::error::AppError;
use crate::ports::{
    AccountActivityView, AccountScopeExclusionView, AccountView, ContourView, ControlAssertionView,
    Scope, Store,
};
use crate::scenarios::reports::MoneyFlowReport;
use iaam_core::event::source_row::RowName;
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::money::{CurrencyCode, Money};
use iaam_core::projection::money_flow::UndecomposedCause;
use iaam_core::reconciliation::check::{ClaimOutcome, ClaimValue};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use iaam_ingest::Verdict;

/// The policy-visible kind of an outstanding action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKind {
    CreateFirstAccount,
    CreateFirstContour,
    AccountScopeUndecided,
    StartAccountImport,
    ProvideControlAssertion,
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
            Self::StartAccountImport => "start_account_import",
            Self::ProvideControlAssertion => "provide_control_assertion",
            Self::CoverageGapUnrepaired => "coverage_gap_unrepaired",
            Self::IndependentConfirmationMissing => "independent_confirmation_missing",
            Self::DiscrepancyUnresolved => "discrepancy_unresolved",
            Self::UndecomposedOutflows => "undecomposed_outflows",
            Self::ExternalTransfersUncategorised => "external_transfers_uncategorised",
            Self::UnexplainedResidual => "unexplained_residual",
            Self::PossibleDuplicateUndecided => "possible_duplicate_undecided",
        }
    }
}

/// The policy category assigned to an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionCategory {
    /// Work that prevents the system from accepting another action.
    Blocking,
    /// Work required for a named goal.
    RequiredForGoal,
    /// Work that improves quality but is not required.
    Recommended,
    /// A fact that requires no action.
    Informational,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionSubject {
    Account(AccountId),
    Event(EventId),
}

/// A source from which the value of a missing request field must come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvidedBy {
    Owner,
    ExternalDocument,
    Caller,
}

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
}

/// Request information attached to an operation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPlan {
    pub preset: BTreeMap<String, serde_json::Value>,
    pub missing: Vec<MissingInput>,
}

/// A symbolic operation identifier resolved by a transport layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}
impl OperationKey {
    /// The route operation identifier declared by the transport.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateAccount => "create_account",
            Self::CreateContour => "create_contour_version",
            Self::AddContourVersion => "add_contour_version",
            Self::RecordOwnerBalance => "record_owner_balance",
            Self::CreateCategoryRule => "create_category_rule",
        }
    }
}

/// The only target shapes an action may have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionTarget {
    Operation {
        operation: OperationKey,
        request: RequestPlan,
    },
    None,
}

/// One invalid combination of action availability and target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionInvariantError {
    ReadyWithoutOperation,
    BlockedWithOperation,
    BlockedWithScope,
    NonBlockedWithoutScope,
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
            (ActionState::Blocked, ActionTarget::Operation { .. })
        ) {
            return Err(ActionInvariantError::BlockedWithOperation);
        }
        if facts.state == ActionState::Blocked && facts.required_scope.is_some() {
            return Err(ActionInvariantError::BlockedWithScope);
        }
        if facts.state != ActionState::Blocked && facts.required_scope.is_none() {
            return Err(ActionInvariantError::NonBlockedWithoutScope);
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
    pub const fn subject(&self) -> Option<ActionSubject> {
        self.subject
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

/// Compute every currently outstanding setup action for an owner.
pub async fn frontier(owner: OwnerId, store: &dyn Store) -> Result<Vec<Action>, AppError> {
    let accounts = store.list_accounts(owner).await?;
    let contours = store.list_contours(owner).await?;
    let exclusions = store.list_account_scope_exclusions(owner).await?;
    let activity = store.list_account_activity(owner).await?;
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
    Ok(actions_from_state(
        &accounts,
        &contours,
        &exclusions,
        &activity,
        &assertions,
    ))
}

/// Find every unresolved or informational fact in a reconciliation ledger.
pub fn ledger_diagnostics(ledger: &ReconciliationLedger) -> Vec<Action> {
    diagnostics(ledger, None)
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
    account: AccountId,
    period: AssertionPeriod,
) -> Vec<Action> {
    diagnostics(ledger, Some((account, period)))
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
    scope: Option<(AccountId, AssertionPeriod)>,
) -> Vec<Action> {
    let mut actions = Vec::new();
    for gap in ledger
        .gaps()
        .iter()
        .filter(|gap| in_scope(scope, gap.account, gap.period))
    {
        let category = ledger
            .statuses()
            .find(|status| status.account() == gap.account && status.period() == gap.period)
            .map_or(ActionCategory::RequiredForGoal, |status| {
                if gap.dimensions.iter().all(|dimension| {
                    status.dimension(*dimension) == DimensionStatus::AcceptedIndependent
                }) {
                    ActionCategory::Informational
                } else {
                    ActionCategory::RequiredForGoal
                }
            });
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
        actions.push(blocked_action(
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
            Some(ActionSubject::Account(gap.account)),
            format!(
                "Account {} has a coverage gap from {} through {} in dimensions {}; {} ({} rows refused); no repair operation exists in this API.",
                gap.account.inner(),
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
        ));
    }
    for status in ledger
        .statuses()
        .filter(|status| in_scope(scope, status.account(), status.period()))
    {
        for dimension in Dimension::all() {
            if status.dimension(dimension) == DimensionStatus::AcceptedInternal {
                actions.push(blocked_action(
                    format!(
                        "{}:{}:{}:{}:{}",
                        ActionKind::IndependentConfirmationMissing.id(),
                        status.account().inner(),
                        status.period().from,
                        status.period().to,
                        dimension.code()
                    ),
                    ActionKind::IndependentConfirmationMissing,
                    ActionCategory::RequiredForGoal,
                    Some(ActionSubject::Account(status.account())),
                    format!(
                        "Account {} reached internal confirmation for {} from {} through {} but has no confirmation from a different parser and document; no acquisition operation exists in this API.",
                        status.account().inner(),
                        dimension.code(),
                        status.period().from,
                        status.period().to
                    ),
                ));
            }
        }
        for (index, check) in status.outcomes().iter().enumerate() {
            let ClaimOutcome::Discrepant(discrepancy) = check.outcome else {
                continue;
            };
            actions.push(blocked_action(
                format!(
                    "{}:{}:{}:{}:{}:{}",
                    ActionKind::DiscrepancyUnresolved.id(),
                    status.account().inner(),
                    status.period().from,
                    status.period().to,
                    discrepancy.field,
                    index
                ),
                ActionKind::DiscrepancyUnresolved,
                ActionCategory::RequiredForGoal,
                Some(ActionSubject::Account(status.account())),
                format!(
                    "Account {} has an unresolved {} discrepancy from {} through {}: claimed {}, observed {}, delta {}; the system cannot identify which side is wrong and no resolution operation exists in this API.",
                    status.account().inner(),
                    discrepancy.field,
                    status.period().from,
                    status.period().to,
                    claim_value_text(discrepancy.claimed),
                    claim_value_text(discrepancy.observed),
                    claim_value_text(discrepancy.delta)
                ),
            ));
        }
    }
    sort_actions(&mut actions);
    actions
}

/// Find undecomposed outflows and unexplained account residuals in a flow report.
pub fn flow_diagnostics(report: &MoneyFlowReport) -> Vec<Action> {
    let mut actions = Vec::new();
    for currency in report.flow.currencies() {
        let undecomposed = report
            .flow
            .not_decomposed_by_account_and_cause(currency)
            .expect("money flow undecomposed breakdown");
        for (account, cause, count, amount) in undecomposed {
            actions.push(match cause {
                UndecomposedCause::NoRuleMatched => {
                    undecomposed_outflows_action(account, currency, count, amount)
                }
                UndecomposedCause::ExternalTransfer => {
                    external_transfers_action(account, currency, count, amount)
                }
            });
        }
    }
    for (account, amount) in report
        .flow
        .residuals_by_account()
        .expect("money flow residual breakdown")
    {
        actions.push(blocked_action(
            format!(
                "{}:{}:{}",
                ActionKind::UnexplainedResidual.id(),
                account.inner(),
                amount.currency().code()
            ),
            ActionKind::UnexplainedResidual,
            ActionCategory::Informational,
            Some(ActionSubject::Account(account)),
            format!(
                "Account {} has an unexplained residual of {} {}; the report quantities do not explain its cash change and no report operation can resolve it.",
                account.inner(),
                amount.to_calc_dec().inner(),
                amount.currency().code()
            ),
        ));
    }
    sort_actions(&mut actions);
    actions
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
    account: AccountId,
    currency: CurrencyCode,
    count: u64,
    amount: Money,
) -> Action {
    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}:{}",
                ActionKind::UndecomposedOutflows.id(),
                account.inner(),
                currency.code()
            ),
            kind: ActionKind::UndecomposedOutflows,
            category: ActionCategory::Recommended,
            state: ActionState::NeedsOwnerInput,
            required_scope: Some(Scope::Owner),
            subject: Some(ActionSubject::Account(account)),
        },
        format!(
            "Account {} has {} outflow rows totaling {} {} that no category rule matched; \
             create a rule that matches them and names what they were for. The rows are \
             not identified here, so neither the matcher nor the category is proposed, \
             and the interval a rule is valid over is not the interval of this report.",
            account.inner(),
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
                    },
                    MissingInput {
                        pointer: "/category".into(),
                        provided_by: ProvidedBy::Owner,
                        candidates: None,
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
    account: AccountId,
    currency: CurrencyCode,
    count: u64,
    amount: Money,
) -> Action {
    blocked_action(
        format!(
            "{}:{}:{}",
            ActionKind::ExternalTransfersUncategorised.id(),
            account.inner(),
            currency.code()
        ),
        ActionKind::ExternalTransfersUncategorised,
        ActionCategory::Informational,
        Some(ActionSubject::Account(account)),
        format!(
            "Account {} has {} transfer rows totaling {} {} that left the contour and \
             carry no category; a category rule cannot decompose them, because category \
             assignment is never consulted for a transfer. Nothing in this API changes that.",
            account.inner(),
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
        ActionCategory::RequiredForGoal,
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
        left.category()
            .cmp(&right.category())
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
fn actions_from_state(
    accounts: &[AccountView],
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
    activity: &[AccountActivityView],
    assertions: &[ControlAssertionView],
) -> Vec<Action> {
    let mut actions = actions_from_views(accounts, contours, exclusions);
    actions.reserve(activity.len() + assertions.len());
    for account in activity
        .iter()
        .filter(|activity| account_import_eligibility(activity) && account_import_gap(activity))
    {
        actions.push(start_account_import_action(account.account));
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
                account.account,
                period,
                point,
            ));
        }
    }
    actions
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

fn actions_from_views(
    accounts: &[AccountView],
    contours: &[ContourView],
    exclusions: &[AccountScopeExclusionView],
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

/// Use the inclusive first and last business effective dates: they are the
/// only period bounds justified by the persisted state, not a calendar default.
fn activity_period(activity: &AccountActivityView) -> Option<AssertionPeriod> {
    AssertionPeriod::between(
        activity.first_effective_date?,
        activity.last_effective_date?,
    )
}

/// The owner has to fetch a statement, and no operation in this API does it.
///
/// `Blocked`, not `NeedsOwnerInput`. Both readings were true of this item — the
/// owner must act, and there is nothing to call — but the two states do not mean
/// the same thing to the agent reading the queue. `NeedsOwnerInput` everywhere
/// else accompanies a real operation with a list of fields the policy cannot
/// fill: collect these, then call this. Here there is no `this`. `Blocked`'s own
/// documentation is exactly the second reading — "no operation in this API is
/// available for this item" — and the queue's states are the only map an agent
/// has of what it may call.
///
/// The invariant then removes `required_scope` as well, and that is right rather
/// than a loss: a scope answers "who may call it", a question that does not
/// arise when nothing can be called.
fn start_account_import_action(account: AccountId) -> Action {
    blocked_action(
        // Scoped to the account: this action is emitted once per account with no
        // facts, and an unscoped id would give every one of them the same
        // identity — which is what an agent deduplicates by.
        format!(
            "{}:{}",
            ActionKind::StartAccountImport.id(),
            account.inner()
        ),
        ActionKind::StartAccountImport,
        ActionCategory::RequiredForGoal,
        Some(ActionSubject::Account(account)),
        format!(
            "Account {} has no business facts; import a statement or connect a broker. \
             Import is continuous and never complete. No operation in this API fetches \
             the document.",
            account.inner()
        ),
    )
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
    account: AccountId,
    period: AssertionPeriod,
    point: BalancePoint,
) -> Action {
    let dimension = Dimension::Cash;
    let mut preset = BTreeMap::new();
    preset.insert("account".to_owned(), account.inner().to_string().into());
    preset.insert("from".to_owned(), period.from.to_string().into());
    preset.insert("to".to_owned(), period.to.to_string().into());
    preset.insert("at".to_owned(), point.code().into());
    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}:{}:{}:{}:{}",
                ActionKind::ProvideControlAssertion.id(),
                account.inner(),
                period.from,
                period.to,
                point.code(),
                dimension.code()
            ),
            kind: ActionKind::ProvideControlAssertion,
            // Required for the goal, at either point, not `Recommended`.
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
            category: ActionCategory::RequiredForGoal,
            state: ActionState::NeedsOwnerInput,
            required_scope: Some(Scope::Owner),
            subject: Some(ActionSubject::Account(account)),
        },
        match point {
            BalancePoint::Opening => format!(
                "Account {} has business facts from {} through {}; record its opening cash balance. \
                 Until it is recorded, the cash figure for this account is a sum accumulated from \
                 an unasserted start, and a closing balance compared against it reports the missing \
                 opening balance as a discrepancy.",
                account.inner(),
                period.from,
                period.to
            ),
            BalancePoint::Closing => format!(
                "Account {} has business facts from {} through {}; record its closing cash balance. \
                 An assertion is evidence to reconcile, not proof of a match; a discrepancy may remain.",
                account.inner(),
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
                missing: vec![MissingInput {
                    pointer: "/cash".into(),
                    provided_by: ProvidedBy::Owner,
                    candidates: None,
                }],
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
                missing: vec![MissingInput {
                    pointer: "/title".into(),
                    provided_by: ProvidedBy::Owner,
                    candidates: None,
                }],
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
/// The target offers the membership half of the answer. The other half — «this
/// account is outside the perimeter, deliberately» — is a different operation
/// and an action carries one target; the sentence names it so the agent is not
/// left believing membership is the only way out of this state.
///
/// The operation is [`OperationKey::AddContourVersion`], not
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
    let (preset, missing) = match contours {
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
                },
                MissingInput {
                    pointer: "/accounts".into(),
                    provided_by: ProvidedBy::Owner,
                    candidates: Some(account_candidates(accounts)),
                },
            ],
        ),
    };

    Action::new(
        ActionFacts {
            id: format!(
                "{}:{}",
                ActionKind::AccountScopeUndecided.id(),
                account.id.inner()
            ),
            kind: ActionKind::AccountScopeUndecided,
            category: ActionCategory::RequiredForGoal,
            state: ActionState::NeedsOwnerInput,
            required_scope: Some(Scope::Owner),
            subject: Some(ActionSubject::Account(account.id)),
        },
        format!(
            "Account {} ({}) belongs to no contour and has not been ruled outside one; \
             until it is placed, its operations are absent from every report and nothing \
             else says so. Add it to an existing contour, or record that it is outside \
             the perimeter and why.",
            account.id.inner(),
            account.title
        ),
        ActionTarget::Operation {
            operation: OperationKey::AddContourVersion,
            request: RequestPlan { preset, missing },
        },
    )
    .expect("account scope action has an operation target")
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
            category: ActionCategory::RequiredForGoal,
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
                    },
                    MissingInput {
                        pointer: "/accounts".into(),
                        provided_by: ProvidedBy::Owner,
                        candidates: Some(candidates),
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
    use crate::ports::Store;
    use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
    use iaam_core::dates::{EffectiveOrder, EventDates};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
    use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{EventId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use iaam_core::projection::money_flow::{DateWindow, MoneyFlow, NoCategories};
    use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
    use iaam_core::reconciliation::evidence::{Evidence, Ground, SourceChannel};
    use iaam_store::SqliteStore;
    use std::collections::BTreeSet;
    use time::macros::date;

    fn store() -> SqliteAdapter {
        SqliteAdapter::new(SqliteStore::open_in_memory().expect("in-memory store"))
    }

    fn with_id(id: AccountId) -> AccountView {
        AccountView {
            id,
            title: "Main".into(),
            institution: None,
        }
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
        let actions = frontier(owner, &store()).await.expect("frontier");

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
            frontier(owner, &store)
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

        let actions = frontier(owner, &store).await.expect("frontier");
        let action = actions
            .iter()
            .find(|action| action.kind() == ActionKind::CreateFirstContour)
            .expect("first contour action");
        assert_eq!(action.category(), ActionCategory::RequiredForGoal);
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
            frontier(owner, &store)
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

        let actions = frontier(owner, &store).await.expect("frontier");
        assert!(
            actions.iter().any(|action| matches!(
                action.target(),
                ActionTarget::Operation {
                    operation: OperationKey::AddContourVersion,
                    ..
                }
            )),
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
        assert_eq!(action.subject(), Some(ActionSubject::Account(orphan.id)));
        assert_eq!(action.category(), ActionCategory::RequiredForGoal);
        assert_eq!(action.state(), ActionState::NeedsOwnerInput);
        assert_eq!(action.required_scope(), Some(Scope::Owner));
        let ActionTarget::Operation { operation, request } = action.target() else {
            panic!("the account scope action must name an operation");
        };
        // The act is «add it to one of the contours that exist», not «create a
        // contour»: naming the creating operation here is what let an agent
        // answer this item with a second perimeter.
        assert_eq!(*operation, OperationKey::AddContourVersion);
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

        let actions = frontier(owner, &store).await.expect("frontier");
        let action = actions
            .iter()
            .find(|action| action.kind() == ActionKind::AccountScopeUndecided)
            .expect("the orphaned account is named");
        let ActionTarget::Operation { operation, request } = action.target() else {
            panic!("the account scope action must name an operation");
        };
        assert_eq!(*operation, OperationKey::AddContourVersion);
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
            frontier(owner, &store)
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

        let reopened = frontier(owner, &store).await.expect("frontier");
        assert_eq!(
            reopened
                .iter()
                .filter(|action| action.kind() == ActionKind::AccountScopeUndecided)
                .map(Action::subject)
                .collect::<Vec<_>>(),
            vec![Some(ActionSubject::Account(arrival.id))]
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

        let actions = frontier(owner, &store).await.expect("frontier");
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
            frontier(owner, &store)
                .await
                .expect("frontier")
                .iter()
                .filter(|action| action.kind() == ActionKind::AccountScopeUndecided)
                .map(Action::subject)
                .collect::<Vec<_>>(),
            vec![Some(ActionSubject::Account(outside.id))]
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

    /// An item with no operation is `blocked`, whatever else is true of it.
    #[test]
    fn an_item_the_agent_cannot_call_says_so_in_its_state() {
        let account = account();
        let actions = actions_from_state(
            std::slice::from_ref(&account),
            &[],
            &[],
            &[no_facts(account.id)],
            &[],
        );

        for action in &actions {
            match action.target() {
                ActionTarget::None => assert_eq!(
                    action.state(),
                    ActionState::Blocked,
                    "{} has nothing to call and must say so",
                    action.id()
                ),
                ActionTarget::Operation { .. } => assert_ne!(
                    action.state(),
                    ActionState::Blocked,
                    "{} names an operation and cannot be blocked",
                    action.id()
                ),
            }
        }
        let import = actions
            .iter()
            .find(|action| action.kind() == ActionKind::StartAccountImport)
            .expect("account import action");
        assert_eq!(import.state(), ActionState::Blocked);
        assert_eq!(import.subject(), Some(ActionSubject::Account(account.id)));
    }

    #[test]
    fn losing_contour_eligibility_is_not_contour_completion() {
        let account = account();
        let eligible = actions_from_views(&[account], &[], &[]);
        let ineligible = actions_from_views(&[], &[], &[]);

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
        let actions = actions_from_state(
            &[with_id(first), with_id(second)],
            &[],
            &[],
            &[no_facts(first), no_facts(second)],
            &[],
        );

        let identities: Vec<_> = actions
            .iter()
            .filter(|action| action.kind() == ActionKind::StartAccountImport)
            .map(Action::id)
            .collect();
        assert_eq!(identities.len(), 2);
        assert_ne!(identities[0], identities[1]);
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

        let first = frontier(owner, &store).await.expect("frontier");
        let second = frontier(owner, &store).await.expect("frontier");
        assert_eq!(first, second);
        assert!(
            first
                .windows(2)
                .all(|actions| actions[0].kind() <= actions[1].kind())
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

        let actions = actions_from_state(
            std::slice::from_ref(&account),
            &[],
            &[],
            std::slice::from_ref(&activity),
            &[],
        );
        let import = actions
            .iter()
            .find(|action| action.kind() == ActionKind::StartAccountImport)
            .expect("account import action");
        assert_eq!(import.target(), &ActionTarget::None);
        assert!(!account_import_completion(&activity));

        let completed = AccountActivityView {
            has_business_fact: true,
            first_effective_date: Some(time::macros::date!(2026 - 03 - 01)),
            last_effective_date: Some(time::macros::date!(2026 - 03 - 01)),
            ..activity
        };
        assert!(account_import_completion(&completed));
        assert!(
            actions_from_state(&[account], &[], &[], &[completed], &[])
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
        actions_from_state(
            std::slice::from_ref(account),
            &[],
            &[],
            std::slice::from_ref(&activity),
            recorded,
        )
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

        let actions = actions_from_state(&[first, second], &[], &[], &activity, &[]);
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
        let actions =
            actions_from_state(std::slice::from_ref(&account), &[], &[], &[activity], &[]);
        assert!(
            actions
                .iter()
                .any(|action| action.kind() == ActionKind::ProvideControlAssertion)
        );
        assert!(
            actions_from_state(&[], &[], &[], &[], &[])
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
        let action = ledger_diagnostics(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .expect("coverage gap diagnostic");

        assert_eq!(action.category(), ActionCategory::RequiredForGoal);
        assert_eq!(action.state(), ActionState::Blocked);
        assert_eq!(action.required_scope(), None);
        assert_eq!(action.target(), &ActionTarget::None);
        assert!(action.reason().contains("given:row-17"));
    }

    #[test]
    fn a_gap_without_a_status_is_still_a_required_diagnostic() {
        let actions = ledger_diagnostics(&gap_ledger(AccountId::new_random()));
        assert!(actions.iter().any(|action| {
            action.kind() == ActionKind::CoverageGapUnrepaired
                && action.category() == ActionCategory::RequiredForGoal
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
        let action = ledger_diagnostics(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::IndependentConfirmationMissing)
            .expect("independence diagnostic");

        assert_eq!(action.category(), ActionCategory::RequiredForGoal);
        assert_eq!(action.state(), ActionState::Blocked);
        assert!(action.reason().contains("different parser and document"));
        assert_eq!(action.target(), &ActionTarget::None);
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
            ReconciliationLedger::build(&[observed, assertion]).expect("discrepant ledger");
        let action = ledger_diagnostics(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::DiscrepancyUnresolved)
            .expect("discrepancy diagnostic");

        assert_eq!(action.category(), ActionCategory::RequiredForGoal);
        assert_eq!(action.state(), ActionState::Blocked);
        assert!(
            action.reason().contains("claimed 10.00 RUB"),
            "{}",
            action.reason()
        );
        assert!(action.reason().contains("observed 5.00 RUB"));
        assert!(action.reason().contains("delta 5.00 RUB"));
        assert_eq!(action.target(), &ActionTarget::None);
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
        let actions = flow_diagnostics(&report);

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
        assert_eq!(action.category(), ActionCategory::RequiredForGoal);
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
                category: ActionCategory::RequiredForGoal,
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
                category: ActionCategory::RequiredForGoal,
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
                category: ActionCategory::RequiredForGoal,
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

        let action = ledger_diagnostics(&ledger)
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

        let action = ledger_diagnostics(&ledger)
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

        let action = ledger_diagnostics(&ledger)
            .into_iter()
            .find(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .expect("coverage gap diagnostic");

        assert_eq!(action.category(), ActionCategory::RequiredForGoal);
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
            cash_balance_assertion(discrepant, 1_000),
            cash_balance_assertion(internal, 0),
        ])
        .expect("diagnostic ledger")
        .with_external_evidence(vec![(internal, august(), internal_cash_evidence())]);

        let mut actions = ledger_diagnostics(&ledger);
        actions.extend(flow_diagnostics(&undecomposed_report(&[
            AccountId::new_random(),
        ])));
        actions.extend(flow_diagnostics(&external_transfer_report(&[
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
                assert_eq!(
                    action.required_scope(),
                    Some(Scope::Owner),
                    "{}",
                    action.id()
                );
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
        let actions = flow_diagnostics(&external_transfer_report(&[account]));

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
        let actions = flow_diagnostics(&mixed_report(account));

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

        let diagnostics = ledger_diagnostics(&ledger);
        let gaps: Vec<&Action> = diagnostics
            .iter()
            .filter(|action| action.kind() == ActionKind::CoverageGapUnrepaired)
            .collect();
        assert_eq!(gaps.len(), 2);
        assert_ne!(gaps[0].id(), gaps[1].id());

        let flow = flow_diagnostics(&undecomposed_report(&[first, second]));
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
            ledger_diagnostics(&ledger).is_empty(),
            "{:?}",
            ledger_diagnostics(&ledger)
        );
        let report = decomposed_report(account);
        assert!(
            flow_diagnostics(&report).is_empty(),
            "{:?}",
            flow_diagnostics(&report)
        );
        assert!(
            verdict_diagnostics(&Verdict::Accepted {
                event: EventId::new_random()
            })
            .is_none()
        );
    }
}
