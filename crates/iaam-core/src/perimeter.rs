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
use crate::event::correction::resolve;
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

/// The negative-cash vocabulary: every classification, its wire code, and what
/// the code means.
///
/// The single source for both, exactly as `not_computable_vocabulary` is for
/// refusals: `NegativeCashClassification::code` below is expanded from these
/// arms, and so is the enumerated, described schema the API publishes. Pass the
/// name of a macro that accepts `Variant => "code": "meaning",` arms and it will
/// be called with the whole list.
///
/// The point of the arrangement is that a classification cannot reach the wire
/// without its meaning reaching the contract, because neither is written twice;
/// and that a variant added here without an entry fails to compile.
#[macro_export]
macro_rules! negative_cash_classification_vocabulary {
    ($receiver:path) => {
        $receiver! {
            TemporarySettlementDeficit => "temporary_settlement_deficit":
                "The balance went negative and a known settlement restored it within the permitted term. Ordinary operation: the period's tax and financial reports are still calculated for the account.",
            UnsupportedMarginLiability => "unsupported_margin_liability":
                "Margin interest or another credit indicator accompanies the deficit, so the account carries financing from outside the perimeter. The system does not reconstruct that economics, and the period's tax and financial reports are refused for this account.",
            UnclassifiedNegativeCash => "unclassified_negative_cash":
                "The balance is negative for a reason the journal does not explain. The period's tax and financial reports are refused for this account until it is explained.",
        }
    };
}

macro_rules! define_negative_cash_classification_code {
    ($($variant:ident => $code:literal : $meaning:literal),+ $(,)?) => {
        impl NegativeCashClassification {
            /// Machine-readable code for the API (§13). The external agent
            /// parses the code; the meaning beside it in the vocabulary is for
            /// the human reading the contract.
            #[must_use]
            pub const fn code(self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $code,)+
                }
            }
        }
    };
}

negative_cash_classification_vocabulary!(define_negative_cash_classification_code);

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
    #[error(transparent)]
    Correction(#[from] crate::event::correction::CorrectionError),
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
///
/// Corrections are resolved here rather than by the caller: a reversed event
/// must contribute no cash effect, and a caller that forgot would double the
/// very movement the reversal retracts.
pub fn assess(
    events: &[Event],
    policy: PerimeterPolicy,
) -> Result<PerimeterAssessment, PerimeterError> {
    assess_effective(&resolve(events)?, policy)
}

/// Perimeter assessment from a journal whose corrections are already resolved.
///
/// For the caller that has the effective set in hand and needs the assessment
/// beside it. `resolve` is deterministic, so a second fold would not disagree
/// with the first — it would merely do the same work again, and a request that
/// folds the same journal three times invites the next reader to wonder which
/// fold is the authoritative one. `assess` above is this function preceded by
/// the fold, so there remains exactly one definition of what is in force.
pub fn assess_effective(
    effective: &[&Event],
    policy: PerimeterPolicy,
) -> Result<PerimeterAssessment, PerimeterError> {
    let mut ordered: Vec<(Date, &Event)> = Vec::with_capacity(effective.len());
    for event in effective.iter().copied() {
        let date = event
            .dates
            .effective_date()
            .ok_or(PerimeterError::EventWithoutDate { event: event.id })?;
        ordered.push((date, event));
    }
    ordered.sort_by(|(_, left), (_, right)| crate::event::compare_for_replay(left, right));

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

    use crate::event::Relation;
    use crate::event::kind::TradeSide;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::money::{Money, Quantity};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn reversed_trade(account: AccountId) -> (Event, Event) {
        let instrument = crate::ids::InstrumentId::new_random();
        let quantity = Quantity(Dec::new(Decimal::ONE));
        let trade = event_with(
            account,
            date!(2026 - 03 - 10),
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(-100),
                fee: Some(rub(-10)),
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(account, rub(-100)),
                Leg::fee(account, rub(-10)),
                Leg::security(
                    account,
                    crate::ids::CustodyId::new_random(),
                    instrument,
                    quantity,
                ),
            ],
        );
        let mut reversal = trade.clone();
        reversal.id = EventId::new_random();
        reversal.relation = Relation::Reversal { target: trade.id };
        (trade, reversal)
    }

    fn cash_out(account: AccountId, amount: i64) -> Event {
        event_with(
            account,
            date!(2026 - 03 - 11),
            2,
            EventKind::CashOut {
                amount: rub(-amount),
            },
            vec![Leg::cash(account, rub(-amount))],
        )
    }

    #[test]
    fn a_reversed_trade_contributes_nothing_to_the_perimeter() {
        let account = AccountId::new_random();
        let (trade, reversal) = reversed_trade(account);

        let assessment = assess(&[trade, reversal], PerimeterPolicy::default()).unwrap();

        assert!(assessment.spans().is_empty());
    }

    #[test]
    fn a_reversed_trade_does_not_hide_a_real_negative_cash_span() {
        let account = AccountId::new_random();
        let (trade, reversal) = reversed_trade(account);
        let remaining = cash_out(account, 50);

        let assessment = assess(&[trade, reversal, remaining], PerimeterPolicy::default()).unwrap();

        assert_eq!(assessment.spans().len(), 1);
        assert_eq!(assessment.spans()[0].from, date!(2026 - 03 - 11));
    }
    #[test]
    fn a_correction_failure_is_reported_by_perimeter_assessment() {
        let account = AccountId::new_random();
        let (_, mut reversal) = reversed_trade(account);
        reversal.relation = Relation::Reversal {
            target: EventId::new_random(),
        };

        assert!(matches!(
            assess(&[reversal], PerimeterPolicy::default()),
            Err(PerimeterError::Correction(
                crate::event::correction::CorrectionError::DanglingTarget { .. }
            ))
        ));
    }

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
