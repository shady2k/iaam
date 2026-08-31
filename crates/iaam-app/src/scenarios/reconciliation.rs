//! Reconciliation status scenarios.
//!
//! There are no calculations here: a journal slice is passed to the core, and
//! its statuses, assertion outcomes, and grounds for escalation are returned.

use iaam_core::dates::{EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{ReconciliationLedger, ReconciliationStatus};
use iaam_ingest::dedup::IdentityScope;
use time::Date;

use crate::AppServices;
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

/// Builds statuses for intervals intersecting the requested range.
pub async fn statuses(
    services: &AppServices,
    principal: &Principal,
    account: AccountId,
    from: Date,
    to: Date,
) -> Result<Vec<ReconciliationStatus>, AppError> {
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
    let ledger = ReconciliationLedger::build(&events)?;
    Ok(ledger
        .statuses()
        .filter(|status| {
            status.account() == account
                && status.period().from <= period.to
                && period.from <= status.period().to
        })
        .cloned()
        .collect())
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
    let source = SourceId::new_random();
    let parser_version = ParserVersion("owner-stated/1".to_owned());
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
            dates: EventDates::empty(),
            order: EffectiveOrder::new(balance.period.to, sequence as u32),
            legs: Vec::new(),
            provenance: provenance.clone(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: Some(format!(
                "owner-balance:{}:{}:{}",
                balance.account.inner(),
                balance.period.from,
                balance.period.to
            )),
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
