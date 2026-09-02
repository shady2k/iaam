use std::collections::BTreeMap;

use crate::error::AppError;
use crate::ports::{
    AccountActivityView, AccountView, ContourView, ControlAssertionView, Scope, Store,
};
use crate::scenarios::reports::MoneyFlowReport;
use iaam_core::event::source_row::RowName;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::Money;
use iaam_core::reconciliation::check::{ClaimOutcome, ClaimValue};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use iaam_ingest::Verdict;

/// The policy-visible kind of an outstanding action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKind {
    CreateFirstAccount,
    CreateFirstContour,
    StartAccountImport,
    ProvideControlAssertion,
    CoverageGapUnrepaired,
    IndependentConfirmationMissing,
    DiscrepancyUnresolved,
    UndecomposedOutflows,
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
            Self::StartAccountImport => "start_account_import",
            Self::ProvideControlAssertion => "provide_control_assertion",
            Self::CoverageGapUnrepaired => "coverage_gap_unrepaired",
            Self::IndependentConfirmationMissing => "independent_confirmation_missing",
            Self::DiscrepancyUnresolved => "discrepancy_unresolved",
            Self::UndecomposedOutflows => "undecomposed_outflows",
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
    CreateContour,
    RecordOwnerBalance,
}
impl OperationKey {
    /// The route operation identifier declared by the transport.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateAccount => "create_account",
            Self::CreateContour => "create_contour_version",
            Self::RecordOwnerBalance => "record_owner_balance",
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
/// A scoped sibling rather than a filter over the returned items: an `Action`
/// publishes no typed subject, only an opaque `id` and prose, so a caller
/// answering about one account can narrow the set only here, on the ledger's own
/// typed gaps and statuses. The predicate is the one
/// `scenarios::reconciliation::report` already applies to its statuses and gaps
/// — the same account, and periods that intersect the requested range.
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
            .not_decomposed_by_account(currency)
            .expect("money flow undecomposed breakdown");
        for (account, count, amount) in undecomposed {
            actions.push(blocked_action(
                format!(
                    "{}:{}:{}",
                    ActionKind::UndecomposedOutflows.id(),
                    account.inner(),
                    currency.code()
                ),
                ActionKind::UndecomposedOutflows,
                ActionCategory::Informational,
                format!(
                    "Account {} has {} undecomposed outflow rows totaling {} {}; the rows are not identified and no report operation can provide a rule.",
                    account.inner(),
                    count,
                    amount.to_calc_dec().inner(),
                    currency.code()
                ),
            ));
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
    reason: String,
) -> Action {
    Action::new(
        ActionFacts {
            id,
            kind,
            category,
            state: ActionState::Blocked,
            required_scope: None,
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
    activity: &[AccountActivityView],
    assertions: &[ControlAssertionView],
) -> Vec<Action> {
    let mut actions = actions_from_views(accounts, contours);
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

fn actions_from_views(accounts: &[AccountView], contours: &[ContourView]) -> Vec<Action> {
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
    actions
}

fn account_completion(accounts: &[AccountView]) -> bool {
    !accounts.is_empty()
}

fn contour_completion(contours: &[ContourView]) -> bool {
    !contours.is_empty()
}

/// Use the inclusive first and last business effective dates: they are the
/// only period bounds justified by the persisted state, not a calendar default.
fn activity_period(activity: &AccountActivityView) -> Option<AssertionPeriod> {
    AssertionPeriod::between(
        activity.first_effective_date?,
        activity.last_effective_date?,
    )
}

fn start_account_import_action(account: AccountId) -> Action {
    Action::new(
        ActionFacts {
            // Scoped to the account: this action is emitted once per account
            // with no facts, and an unscoped id would give every one of them the
            // same identity — which is what an agent deduplicates by.
            id: format!(
                "{}:{}",
                ActionKind::StartAccountImport.id(),
                account.inner()
            ),
            kind: ActionKind::StartAccountImport,
            category: ActionCategory::RequiredForGoal,
            state: ActionState::NeedsOwnerInput,
            required_scope: Some(Scope::Owner),
        },
        format!(
            "Account {} has no business facts; import a statement or connect a broker. \
             Import is continuous and never complete.",
            account.inner()
        ),
        ActionTarget::None,
    )
    .expect("account import action needs owner input")
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

fn first_contour_action(accounts: &[AccountView]) -> Action {
    let mut candidates: Vec<_> = accounts
        .iter()
        .map(|account| AccountCandidate {
            id: account.id,
            title: account.title.clone(),
            institution: account.institution.clone(),
        })
        .collect();
    candidates.sort_by_key(|candidate| candidate.id.inner());

    Action::new(
        ActionFacts {
            id: identity(ActionKind::CreateFirstContour),
            kind: ActionKind::CreateFirstContour,
            category: ActionCategory::RequiredForGoal,
            state: ActionState::NeedsOwnerInput,
            required_scope: Some(Scope::Owner),
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

    #[test]
    fn losing_contour_eligibility_is_not_contour_completion() {
        let account = account();
        let eligible = actions_from_views(&[account], &[]);
        let ineligible = actions_from_views(&[], &[]);

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
            actions_from_state(&[account], &[], &[completed], &[])
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

        let actions = actions_from_state(&[first, second], &[], &activity, &[]);
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
        let actions = actions_from_state(std::slice::from_ref(&account), &[], &[activity], &[]);
        assert!(
            actions
                .iter()
                .any(|action| action.kind() == ActionKind::ProvideControlAssertion)
        );
        assert!(
            actions_from_state(&[], &[], &[], &[])
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
        assert_eq!(undecomposed.category(), ActionCategory::Informational);
        assert!(undecomposed.reason().contains(&account.inner().to_string()));
        assert_eq!(undecomposed.state(), ActionState::Blocked);
        assert_eq!(undecomposed.target(), &ActionTarget::None);
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
        actions.extend(verdict_diagnostics(&Verdict::PossibleDuplicate {
            event: EventId::new_random(),
            of: EventId::new_random(),
            level: iaam_ingest::dedup::DedupLevel::Probabilistic,
        }));
        actions
    }

    /// The universal assertions are worthless over an empty set, so the exact set
    /// of kinds is asserted **first**: the sweep below then runs over something.
    #[test]
    fn every_diagnostic_is_blocked_with_no_target_and_no_scope() {
        let actions = every_diagnostic();

        let kinds: BTreeSet<ActionKind> = actions.iter().map(Action::kind).collect();
        assert_eq!(
            kinds,
            BTreeSet::from([
                ActionKind::CoverageGapUnrepaired,
                ActionKind::IndependentConfirmationMissing,
                ActionKind::DiscrepancyUnresolved,
                ActionKind::UndecomposedOutflows,
                ActionKind::UnexplainedResidual,
                ActionKind::PossibleDuplicateUndecided,
            ]),
            "the sweep must run over every diagnostic kind, not a subset"
        );

        for action in &actions {
            assert_eq!(action.target(), &ActionTarget::None, "{}", action.id());
            assert_eq!(action.state(), ActionState::Blocked, "{}", action.id());
            assert_eq!(action.required_scope(), None, "{}", action.id());
        }
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
