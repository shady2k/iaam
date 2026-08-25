//! Сценарии статусов сверки.
//!
//! Здесь нет расчётов: срез журнала передаётся ядру, а наружу возвращаются
//! его статусы, исходы утверждений и основания повышения.

use iaam_core::dates::{EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{ReconciliationLedger, ReconciliationStatus};
use time::Date;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{Principal, Recorded};

/// Баланс, названный владельцем. Состав намеренно ограничен §10.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerBalance {
    pub account: AccountId,
    pub period: AssertionPeriod,
    pub at: BalancePoint,
    pub cash: Option<(CurrencyCode, PostedMinor)>,
    pub positions: Vec<(InstrumentId, CustodyId, Quantity)>,
    pub raw_hash: RawHash,
}

/// Строит статусы по интервалам, пересекающим запрошенный диапазон.
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
            expected: "from не позже to".into(),
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

/// Записывает только денежные и позиционные утверждения владельца.
pub async fn record_owner_balance(
    services: &AppServices,
    principal: &Principal,
    balance: OwnerBalance,
) -> Result<Vec<Recorded>, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "право отправки операций".into(),
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
            expected: "cash или positions".into(),
            actual: "пусто".into(),
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
    services.store.append_events(events).await
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
