//! Perimeter: shorts, margin, repos, and derivatives are outside the perimeter (§11).
//!
//! The boundary is **capability-based, not document-based**: encountering an
//! unsupported operation does not reject the report. Its observable cash effect
//! is always retained; the system refuses to invent unsupported financing
//! economics.
//!
//! Negative cash is supported even in a long-only system: it can arise from
//! settlement timing, fees, and technical overdrafts. It enters NAV as a
//! liability; it does not disappear.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::event::Event;
use crate::event::kind::{EventKind, FeeOrigin};
use crate::ids::{AccountId, EventId};
use crate::money::{CurrencyCode, PostedMinor};
use crate::reconciliation::Dimension;
use crate::reconciliation::check::ReconciliationException;

/// Perimeter policy.
///
/// The settlement window is a parameter, not a constant: a “permitted term”
/// (§11) cannot be computed without a trading calendar, and the calendar is
/// E3. The default covers T+2 with weekends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerimeterPolicy {
    pub settlement_window_days: u16,
}

impl Default for PerimeterPolicy {
    fn default() -> Self {
        Self {
            settlement_window_days: 5,
        }
    }
}

/// Classification of negative cash (§11, table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NegativeCashClassification {
    /// Closed by known settlement within the permitted term: settlement is allowed.
    TemporarySettlementDeficit,
    /// Margin interest or a credit indicator is present.
    UnsupportedMarginLiability,
    /// The reason is unknown.
    UnclassifiedNegativeCash,
}

impl NegativeCashClassification {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TemporarySettlementDeficit => "temporary_settlement_deficit",
            Self::UnsupportedMarginLiability => "unsupported_margin_liability",
            Self::UnclassifiedNegativeCash => "unclassified_negative_cash",
        }
    }

    /// Whether this classification blocks tax and financial reports for the
    /// period (§11). A temporary settlement deficit does not.
    #[must_use]
    pub const fn blocks_reports(self) -> bool {
        match self {
            Self::TemporarySettlementDeficit => false,
            Self::UnsupportedMarginLiability | Self::UnclassifiedNegativeCash => true,
        }
    }
}

/// Negative-cash interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegativeCashSpan {
    pub account: AccountId,
    pub currency: CurrencyCode,
    pub from: Date,
    /// Date on which the balance returned to non-negative. `None` means it remains open.
    pub resolved: Option<Date>,
    pub classification: NegativeCashClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PerimeterError {
    #[error("event {event:?} has no date and cannot be assigned to a span")]
    EventWithoutDate { event: EventId },
    #[error("account {account:?} balance overflow in {currency:?}")]
    Overflow {
        account: AccountId,
        currency: CurrencyCode,
    },
}

/// Reconciliation exceptions explained by the perimeter boundary.
///
/// They prevent the owner from being told to “fix” something the system
/// deliberately does not support (§11).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerimeterExceptions {
    entries: Vec<(AccountId, Dimension, ReconciliationException)>,
}

impl PerimeterExceptions {
    pub fn add(
        &mut self,
        account: AccountId,
        dimension: Dimension,
        exception: ReconciliationException,
    ) {
        if !self
            .entries
            .iter()
            .any(|(a, d, e)| *a == account && *d == dimension && *e == exception)
        {
            self.entries.push((account, dimension, exception));
        }
    }

    #[must_use]
    pub fn covers(
        &self,
        account: AccountId,
        dimension: Dimension,
    ) -> Option<ReconciliationException> {
        self.entries
            .iter()
            .find(|(a, d, _)| *a == account && *d == dimension)
            .map(|(_, _, exception)| *exception)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of distinct recorded exceptions.
    ///
    /// This is not just a convenience: without it one cannot verify that
    /// repeating an exception does not duplicate the list, while `is_empty`
    /// cannot answer that question.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Perimeter assessment from the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerimeterAssessment {
    policy: PerimeterPolicy,
    spans: Vec<NegativeCashSpan>,
    financing: BTreeSet<AccountId>,
}

impl PerimeterAssessment {
    /// Empty assessment: there is no journal, or it was not examined.
    #[must_use]
    pub fn empty(policy: PerimeterPolicy) -> Self {
        Self {
            policy,
            spans: Vec::new(),
            financing: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn spans(&self) -> &[NegativeCashSpan] {
        &self.spans
    }

    #[must_use]
    pub const fn policy(&self) -> PerimeterPolicy {
        self.policy
    }

    /// Whether the account has financing outside the perimeter.
    #[must_use]
    pub fn financing_present(&self, account: AccountId) -> bool {
        self.financing.contains(&account)
    }

    /// Whether tax and financial reports should refuse calculation for this
    /// account.
    ///
    /// This says nothing about **other** accounts: §11 requires that the
    /// remainder continue to be calculated.
    #[must_use]
    pub fn blocks_period_reports(&self, account: AccountId) -> bool {
        self.spans
            .iter()
            .any(|span| span.account == account && span.classification.blocks_reports())
    }

    /// Reconciliation exceptions implied by the assessment.
    #[must_use]
    pub fn exceptions(&self) -> PerimeterExceptions {
        let mut exceptions = PerimeterExceptions::default();
        for account in &self.financing {
            exceptions.add(
                *account,
                Dimension::Cash,
                ReconciliationException::UnsupportedFinancingPresent,
            );
        }
        exceptions
    }
}

/// Perimeter assessment from the journal.
///
/// Logic is intentionally outside the constructor named `new` (§15.7).
pub fn assess(
    events: &[Event],
    policy: PerimeterPolicy,
) -> Result<PerimeterAssessment, PerimeterError> {
    let mut ordered: Vec<(Date, &Event)> = Vec::with_capacity(events.len());
    for event in events {
        let date = event
            .dates
            .effective_date()
            .ok_or(PerimeterError::EventWithoutDate { event: event.id })?;
        ordered.push((date, event));
    }
    ordered.sort_by_key(|(date, event)| (*date, event.order));

    // Detect credit across the whole journal up front: margin interest may be
    // charged after the deficit closes, but still belongs to that deficit.
    let mut financing: BTreeSet<AccountId> = BTreeSet::new();
    for (_, event) in &ordered {
        if matches!(
            event.kind,
            EventKind::Fee {
                origin: FeeOrigin::MarginInterest,
                ..
            }
        ) {
            financing.insert(event.account);
        }
    }

    let mut balances: BTreeMap<(AccountId, CurrencyCode), PostedMinor> = BTreeMap::new();
    let mut open: BTreeMap<(AccountId, CurrencyCode), Date> = BTreeMap::new();
    let mut spans: Vec<NegativeCashSpan> = Vec::new();

    for (date, event) in &ordered {
        for leg in &event.legs {
            let Some(money) = leg.cash_effect() else {
                continue;
            };
            let key = (leg.account, money.currency());
            let slot = balances.entry(key).or_insert_with(|| PostedMinor::new(0));
            *slot = slot
                .checked_add(money.amount())
                .ok_or(PerimeterError::Overflow {
                    account: leg.account,
                    currency: money.currency(),
                })?;

            let negative = slot.raw() < 0;
            match (negative, open.get(&key).copied()) {
                (true, None) => {
                    open.insert(key, *date);
                }
                (false, Some(start)) => {
                    open.remove(&key);
                    spans.push(classify(key, start, Some(*date), &financing, policy));
                }
                _ => {}
            }
        }
    }
    // Intervals that remain open at the end of the journal.
    for (key, start) in open {
        spans.push(classify(key, start, None, &financing, policy));
    }
    spans.sort_by_key(|span| (span.from, span.account, span.currency));
    Ok(PerimeterAssessment {
        policy,
        spans,
        financing,
    })
}

fn classify(
    key: (AccountId, CurrencyCode),
    from: Date,
    resolved: Option<Date>,
    financing: &BTreeSet<AccountId>,
    policy: PerimeterPolicy,
) -> NegativeCashSpan {
    let (account, currency) = key;
    // Branch order matters: a credit indicator outranks the time limit. A
    // deficit closed within a day but accompanied by margin interest remains a
    // margin liability—the system does not invent financing economics regardless
    // of how quickly it closed.
    let classification = if financing.contains(&account) {
        NegativeCashClassification::UnsupportedMarginLiability
    } else if resolved
        .is_some_and(|end| (end - from).whole_days() <= i64::from(policy.settlement_window_days))
    {
        NegativeCashClassification::TemporarySettlementDeficit
    } else {
        NegativeCashClassification::UnclassifiedNegativeCash
    };
    NegativeCashSpan {
        account,
        currency,
        from,
        resolved,
        classification,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AccountId;

    #[test]
    fn every_classification_has_a_distinct_machine_readable_code() {
        // The external agent parses the code, not the text. An empty code is
        // indistinguishable from “no classification”, while one code for all
        // three is indistinguishable from “there is a deficit, but its kind is
        // unknown”.
        let all = [
            NegativeCashClassification::TemporarySettlementDeficit,
            NegativeCashClassification::UnsupportedMarginLiability,
            NegativeCashClassification::UnclassifiedNegativeCash,
        ];
        let mut codes: Vec<&str> = all.iter().map(|c| c.code()).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "classification codes collided");
        assert_eq!(
            codes,
            vec![
                "temporary_settlement_deficit",
                "unclassified_negative_cash",
                "unsupported_margin_liability",
            ]
        );
    }

    #[test]
    fn only_the_temporary_deficit_lets_period_reports_through() {
        // §11: in two of three cases, tax and financial reports for the period
        // return not_computable. A mistake here would report unsupported
        // financing economics as calculated.
        assert!(!NegativeCashClassification::TemporarySettlementDeficit.blocks_reports());
        assert!(NegativeCashClassification::UnsupportedMarginLiability.blocks_reports());
        assert!(NegativeCashClassification::UnclassifiedNegativeCash.blocks_reports());
    }

    #[test]
    fn an_exception_is_recorded_once_per_account_and_dimension() {
        // Repeating the same exception does not duplicate the list: the owner
        // would see one reason twice and infer two problems.
        let account = AccountId::new_random();
        let mut exceptions = PerimeterExceptions::default();
        assert!(exceptions.is_empty());

        exceptions.add(
            account,
            Dimension::Cash,
            ReconciliationException::UnsupportedFinancingPresent,
        );
        exceptions.add(
            account,
            Dimension::Cash,
            ReconciliationException::UnsupportedFinancingPresent,
        );
        assert!(!exceptions.is_empty());
        assert_eq!(
            exceptions.covers(account, Dimension::Cash),
            Some(ReconciliationException::UnsupportedFinancingPresent)
        );
        assert_eq!(exceptions.len(), 1, "repeat did not add a second entry");
    }

    #[test]
    fn exceptions_are_kept_apart_by_account_dimension_and_reason() {
        // Three key fields, and each must distinguish entries. A collapsed key
        // either hides another account's exception or covers a dimension that
        // the reason does not explain.
        let ours = AccountId::new_random();
        let theirs = AccountId::new_random();
        let mut exceptions = PerimeterExceptions::default();

        exceptions.add(
            ours,
            Dimension::Cash,
            ReconciliationException::UnsupportedFinancingPresent,
        );
        exceptions.add(
            ours,
            Dimension::Positions,
            ReconciliationException::UnsupportedRepoEncumbrance,
        );
        exceptions.add(
            theirs,
            Dimension::Cash,
            ReconciliationException::UnsupportedFinancingPresent,
        );
        exceptions.add(
            ours,
            Dimension::Cash,
            ReconciliationException::UnsupportedRepoEncumbrance,
        );

        assert_eq!(exceptions.len(), 4, "distinct entries were merged");
        assert_eq!(
            exceptions.covers(ours, Dimension::Positions),
            Some(ReconciliationException::UnsupportedRepoEncumbrance)
        );
        assert_eq!(
            exceptions.covers(theirs, Dimension::Cash),
            Some(ReconciliationException::UnsupportedFinancingPresent)
        );
        assert_eq!(
            exceptions.covers(AccountId::new_random(), Dimension::Cash),
            None,
            "account without exceptions is covered by nothing"
        );
    }

    #[test]
    fn an_empty_assessment_reports_no_spans_and_no_exceptions() {
        let policy = PerimeterPolicy {
            settlement_window_days: 3,
        };
        let assessment = PerimeterAssessment::empty(policy);
        assert!(assessment.spans().is_empty());
        assert!(assessment.exceptions().is_empty());
        assert_eq!(assessment.policy().settlement_window_days, 3);
        assert!(!assessment.financing_present(AccountId::new_random()));
    }
}
