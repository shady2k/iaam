//! Reconciliation status scenarios.
//!
//! There are no calculations here: a journal slice is passed to the core, and
//! its statuses, assertion outcomes, and grounds for escalation are returned.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::perimeter::{PerimeterPolicy, assess};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{ReconciliationLedger, ReconciliationStatus};
use iaam_ingest::dedup::IdentityScope;
use time::Date;

use crate::AppServices;
use crate::actions::{Action, ledger_diagnostics_for};
use crate::error::AppError;
use crate::ports::{Principal, Recorded};

/// Balance stated by the owner. Its composition is deliberately limited by §10.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerBalance {
    pub account: AccountId,
    pub period: AssertionPeriod,
    pub at: BalancePoint,
    pub cash: Option<(CurrencyCode, PostedMinor)>,
    pub positions: Vec<(InstrumentId, CustodyId, Quantity)>,
    pub raw_hash: RawHash,
}

/// Reconciliation statuses and effective coverage gaps for a requested range.
#[derive(Debug, Clone)]
pub struct ReconciliationReport {
    pub statuses: Vec<ReconciliationStatus>,
    pub gaps: Vec<iaam_core::reconciliation::Taint>,
    /// What this range's statuses and gaps leave outstanding.
    ///
    /// Computed here, where the ledger is already in hand, rather than by a
    /// caller: a handler that rebuilt the ledger would fold the journal twice
    /// for one request, and the second fold could disagree with the first.
    pub actions: Vec<Action>,
}

/// Builds statuses and gaps for intervals intersecting the requested range.
pub async fn report(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
    from: Date,
    to: Date,
) -> Result<ReconciliationReport, AppError> {
    let Some(period) = AssertionPeriod::between(from, to) else {
        return Err(AppError::Invalid {
            field: "period".into(),
            expected: "from no later than to".into(),
            actual: format!("{from}..{to}"),
        });
    };
    let events = services
        .store
        .load_events_through(principal.owner, period.to)
        .await?;
    // `build_with`, as the balances and returns paths do it. Plain `build` here
    // told the owner to reconcile financing the system deliberately does not
    // reconstruct, and — since the balances answer moved to the excepted ledger —
    // would have made two routes render one account's cash dimension differently,
    // one `discrepant` and one `excepted`, from the same journal.
    let perimeter = assess(&events, PerimeterPolicy::default())?;
    let ledger = ReconciliationLedger::build_with(&events, &perimeter.exceptions())?;
    // The account itself, not only its identifier: every item this range emits
    // says what the owner calls the account it is about, and the name is his
    // and lives on the account.
    let accounts = services.store.list_accounts(principal.owner).await?;
    let named = accounts
        .iter()
        .find(|held| held.id == account)
        .ok_or_else(|| AppError::NotFound {
            what: "account",
            id: account.inner().to_string(),
        })?;
    Ok(ReconciliationReport {
        statuses: statuses_for_account(&ledger, account, period),
        actions: ledger_diagnostics_for(&ledger, named, period),
        gaps: ledger
            .gaps()
            .iter()
            .filter(|gap| {
                gap.account == account
                    && gap.period.from <= period.to
                    && period.from <= gap.period.to
            })
            .cloned()
            .collect(),
    })
}

/// Builds statuses for intervals intersecting the requested range.
pub async fn statuses(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
    from: Date,
    to: Date,
) -> Result<Vec<ReconciliationStatus>, AppError> {
    Ok(report(services, principal, account, from, to)
        .await?
        .statuses)
}

pub(super) fn statuses_for_account(
    ledger: &ReconciliationLedger,
    account: AccountId,
    period: AssertionPeriod,
) -> Vec<ReconciliationStatus> {
    ledger
        .statuses()
        .filter(|status| {
            status.account() == account
                && status.period().from <= period.to
                && period.from <= status.period().to
        })
        .cloned()
        .collect()
}

/// Parser version stamped on every fact the owner states about a balance.
///
/// Named rather than spelled inline because two things depend on the value: the
/// provenance below, and [`SourceChannel::is_independent_of`], which is the
/// reason it matters at all — see [`OWNER_STATED_CHANNEL`].
///
/// [`SourceChannel::is_independent_of`]: iaam_core::reconciliation::evidence::SourceChannel::is_independent_of
pub const OWNER_STATED_PARSER_VERSION: &str = "owner-stated/1";

/// Channel the owner's own statement about a balance arrives through.
///
/// The owner saying what he holds is a channel beside `file`, `paste` and
/// `correction`, not one of them: he did not read the figure out of a document
/// this system parsed, and sharing a source with one would make his word and
/// the document's word one source.
///
/// **Why the source is derived and not minted.** Every call used to mint
/// `SourceId::new_random()`. Since the idempotency key gained the balance
/// point, an opening claim and a closing claim for one account and period are
/// two separate calls and therefore two events — and under a per-call source
/// they became two `StatementGroup`s where they are one statement about one
/// interval.
///
/// Nothing visibly broke, and the reason is narrow. Evidence 3 needs two
/// channels that [`SourceChannel::is_independent_of`], which demands a
/// differing parser version **and** a differing document; every event written
/// here carries [`OWNER_STATED_PARSER_VERSION`], so no two of them are ever
/// independent and the owner cannot corroborate himself. That is a guard
/// standing one condition away from failing: give owner-stated facts a second
/// parser version, or relax the independence rule, and an owner restating a
/// balance starts confirming it. Deriving the source closes it structurally
/// instead — the owner's word about one account is one source, so one period
/// holds one group of it and there is no pair to compare.
///
/// **Keyed on the account and not on the period.** A source answers «where do
/// these rows come from», and the answer is the same in March as in April; the
/// period is already carried by the assertion itself, and grouping separates by
/// it. Putting the period in the key would mint a source per month, which is
/// the shape [`ImportId`] exists to express and [`SourceId`] exists not to.
///
/// [`SourceChannel::is_independent_of`]: iaam_core::reconciliation::evidence::SourceChannel::is_independent_of
/// [`ImportId`]: iaam_core::ids::ImportId
pub const OWNER_STATED_CHANNEL: &str = "owner-stated";

/// Version of the owner-stated idempotency key form.
///
/// Part of the key itself, as `CANONICAL_VERSION` is part of the ingest
/// fingerprint: keys have already been deduplicated against, so a change of
/// form must be visible in the value rather than inferred from its shape.
const OWNER_BALANCE_KEY_VERSION: u8 = 2;

/// The key under which one owner-stated claim is the same submission twice.
///
/// Version 1 was `owner-balance:{account}:{from}:{to}` and named none of what
/// separates one claim from another. Two consequences, both observed: an
/// opening claim and a closing claim for one account and period collided, so
/// the closing one was answered at §10.6 level 2 with the opening event and
/// never reached the journal; and every event of a single call — cash plus each
/// position — carried one key, so a call stating four facts wrote one.
///
/// # Events already written under version 1
///
/// They keep their old key, and a resubmission of the very same claim now
/// misses them and inserts a second event. That is accepted deliberately.
///
/// Rewriting them was considered first and rejected on this schema's own
/// terms: `events` carries `events_are_immutable`, a trigger whose comment
/// says the log is append-only «not as an agreement but as behaviour of the
/// database», because code discipline does not survive the first data-repair
/// script. A migration that suspended it to restamp a key would be that
/// script.
///
/// Looking the old key up alongside the new one was considered second and is
/// worse than useless. The old key cannot tell «the same claim, restated» from
/// «the other balance point of the same period»: it collapses both, which is
/// the bug. Colliding with it would therefore keep refusing a closing claim
/// posted after an opening one — reasserting the defect for exactly the
/// journals that already suffer it. To collide only in the honest case you
/// must compare claims, and comparing claims is what the new key does.
///
/// What the accepted duplication costs is bounded: the unique index on
/// `(owner, idempotency_key)` means at most one version-1 event exists per
/// account and period, so at most one duplicate per period can arise, and only
/// for an owner who restates a claim he already stated. The duplicate states a
/// fact he did state; it is not a wrong number. It cannot fabricate agreement
/// between sources either — E3 requires two channels that
/// `is_independent_of` each other, and every owner-stated event shares the
/// `owner-stated/1` parser version, so no pair of them is ever independent.
/// A duplicate that bothers him is retracted like any other event.
fn owner_balance_key(account: AccountId, period: AssertionPeriod, claim: &ControlClaim) -> String {
    format!(
        "owner-balance:v{OWNER_BALANCE_KEY_VERSION}:{}:{}:{}:{}",
        account.inner(),
        period.from,
        period.to,
        claim.subject_key()
    )
}

/// Records only the owner's cash and position assertions.
pub async fn record_owner_balance(
    services: &AppServices,
    principal: &Principal,
    balance: OwnerBalance,
) -> Result<Vec<Recorded>, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "right to submit transactions".into(),
            actual: principal.scope.code().to_owned(),
        });
    }
    // Derived, not minted: see `OWNER_STATED_CHANNEL` for what a per-call
    // source cost and for why the account is the whole of the key.
    let source = SourceId::declared(principal.owner, balance.account, OWNER_STATED_CHANNEL);
    let parser_version = ParserVersion(OWNER_STATED_PARSER_VERSION.to_owned());
    let provenance = Provenance::new(source, balance.raw_hash, parser_version);
    let mut claims = Vec::new();
    if let Some((currency, amount)) = balance.cash {
        claims.push(ControlClaim::CashBalance {
            currency,
            amount,
            at: balance.at,
        });
    }
    claims.extend(
        balance
            .positions
            .into_iter()
            .map(
                |(instrument, custody, quantity)| ControlClaim::PositionQuantity {
                    instrument,
                    custody,
                    quantity,
                    at: balance.at,
                },
            ),
    );
    if claims.is_empty() {
        return Err(AppError::Invalid {
            field: "balance".into(),
            expected: "cash or positions".into(),
            actual: "empty".into(),
        });
    }
    let events = claims
        .into_iter()
        .enumerate()
        .map(|(sequence, claim)| Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: principal.owner,
            account: balance.account,
            kind: EventKind::ControlAssertion {
                period: balance.period,
                claim,
            },
            // Dated at the end of the interval it speaks about, as the sync
            // path dates the assertions it parses out of a report, and as the
            // store already stamps this event's `effective_date` column from
            // its order. An undated assertion is not merely unordered:
            // `reconciliation::observe` refuses a journal containing an event
            // that "falls within no period", so one such event made every
            // reconciliation, balances and returns report fail to build.
            dates: EventDates::for_cash(CashPostedDate(balance.period.to)),
            order: EffectiveOrder::new(balance.period.to, sequence as u32),
            legs: Vec::new(),
            provenance: provenance.clone(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: Some(owner_balance_key(balance.account, balance.period, &claim)),
        })
        .collect();
    crate::scenarios::ingest::append_checked(services, events, IdentityScope::Source).await
}

#[cfg(test)]
mod tests {
    use iaam_core::reconciliation::Dimension;

    #[test]
    fn owner_balance_scope_is_limited_to_two_dimensions() {
        assert_eq!(Dimension::Cash.code(), "cash");
        assert_eq!(Dimension::Positions.code(), "positions");
    }
}
