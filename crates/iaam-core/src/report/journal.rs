//! Journal movement aggregates.
//!
//! The application selects and resolves journal events; this module folds the
//! selected facts into groups. Keeping the fold here makes every published
//! monetary total come from the same core arithmetic as the other reports.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::Date;

use crate::event::Event;
use crate::ids::AccountId;
use crate::money::{CurrencyCode, Money, MoneyError};

/// The closed grouping vocabulary for journal movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JournalAggregateGroupBy {
    Account,
    Currency,
    Kind,
    Month,
}

/// A refusal or arithmetic failure while folding journal movement.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JournalAggregateError {
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error("aggregate group ceiling exceeded: {actual} groups over a ceiling of {ceiling}")]
    GroupCeiling { ceiling: usize, actual: usize },
}

/// The complete answer to a journal movement query.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalAggregate {
    pub groups: Vec<JournalAggregateGroup>,
}

/// One aggregate group. Account and currency grouping keys describe the legs
/// that contributed to the group, never the event's filing account.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalAggregateGroup {
    pub account: Option<AccountId>,
    pub currency: Option<CurrencyCode>,
    pub kind: Option<String>,
    pub month: Option<String>,
    pub events: u64,
    pub cash: Vec<Money>,
    pub first_effective_date: Option<Date>,
    pub last_effective_date: Option<Date>,
}

/// Fold selected journal events into movement groups.
///
/// `events` must already be narrowed by the application, including standing
/// membership. This function is pure and performs no store access or
/// resolution. Grouping by account and currency uses each cash-bearing leg's
/// own account and currency; without either leg dimension, every cash-bearing
/// leg contributes to one event group.
///
/// With no grouping, an empty selection still returns one all-None group with
/// zero events, so the ungrouped answer has one stable shape.
pub fn aggregate_journal<'a, I>(
    events: I,
    group_by: &[JournalAggregateGroupBy],
    max_groups: usize,
) -> Result<JournalAggregate, JournalAggregateError>
where
    I: IntoIterator<Item = &'a Event>,
{
    let has_leg_dimension = group_by.iter().any(|dimension| {
        matches!(
            dimension,
            JournalAggregateGroupBy::Account | JournalAggregateGroupBy::Currency
        )
    });
    let has_kind = group_by.contains(&JournalAggregateGroupBy::Kind);
    let has_month = group_by.contains(&JournalAggregateGroupBy::Month);
    let mut groups = BTreeMap::<AggregateKey, AggregateGroupBuilder>::new();

    for event in events {
        let legs: Vec<_> = event
            .legs
            .iter()
            .filter_map(|leg| leg.cash_effect().map(|money| (leg, money)))
            .collect();
        let keys: BTreeSet<_> = if has_leg_dimension {
            legs.iter()
                .map(|(leg, money)| AggregateKey {
                    account: group_by
                        .contains(&JournalAggregateGroupBy::Account)
                        .then_some(leg.account),
                    currency: group_by
                        .contains(&JournalAggregateGroupBy::Currency)
                        .then_some(money.currency()),
                    kind: has_kind.then(|| event.kind.discriminant().to_owned()),
                    month: has_month.then(|| month_key(event.order.date())),
                })
                .collect()
        } else {
            [AggregateKey {
                account: None,
                currency: None,
                kind: has_kind.then(|| event.kind.discriminant().to_owned()),
                month: has_month.then(|| month_key(event.order.date())),
            }]
            .into_iter()
            .collect()
        };

        for key in keys {
            let group = groups.entry(key.clone()).or_default();
            group.events += 1;
            group.first_effective_date = Some(
                group
                    .first_effective_date
                    .map_or(event.order.date(), |date| date.min(event.order.date())),
            );
            group.last_effective_date = Some(
                group
                    .last_effective_date
                    .map_or(event.order.date(), |date| date.max(event.order.date())),
            );
            for (leg, money) in &legs {
                if !has_leg_dimension
                    || (key.account.is_none_or(|account| account == leg.account)
                        && key
                            .currency
                            .is_none_or(|currency| currency == money.currency()))
                {
                    let total = group
                        .cash
                        .entry(money.currency())
                        .or_insert_with(|| Money::zero(money.currency()));
                    *total = total.try_add(*money)?;
                }
            }
        }
    }

    if group_by.is_empty() && groups.is_empty() {
        groups.insert(
            AggregateKey {
                account: None,
                currency: None,
                kind: None,
                month: None,
            },
            AggregateGroupBuilder::default(),
        );
    }
    let group_count = groups.len();
    if group_count > max_groups {
        return Err(JournalAggregateError::GroupCeiling {
            ceiling: max_groups,
            actual: group_count,
        });
    }

    Ok(JournalAggregate {
        groups: groups
            .into_iter()
            .map(|(key, group)| JournalAggregateGroup {
                account: key.account,
                currency: key.currency,
                kind: key.kind,
                month: key.month,
                events: group.events,
                cash: group.cash.into_values().collect(),
                first_effective_date: group.first_effective_date,
                last_effective_date: group.last_effective_date,
            })
            .collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AggregateKey {
    account: Option<AccountId>,
    currency: Option<CurrencyCode>,
    kind: Option<String>,
    month: Option<String>,
}

#[derive(Debug, Default)]
struct AggregateGroupBuilder {
    events: u64,
    cash: BTreeMap<CurrencyCode, Money>,
    first_effective_date: Option<Date>,
    last_effective_date: Option<Date>,
}

fn month_key(date: Date) -> String {
    format!("{:04}-{:02}", date.year(), u8::from(date.month()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{kind::EventKind, leg::Leg, test_support::event_with};
    use crate::ids::{AccountId, TransferId};
    use crate::money::{CurrencyCode, PostedMinor};
    use time::macros::date;
    use uuid::Uuid;

    fn account(index: u128) -> AccountId {
        AccountId(Uuid::from_u128(index))
    }

    fn money(minor: i64, currency: CurrencyCode) -> Money {
        Money::new(PostedMinor::new(minor), currency)
    }

    fn cash_event(account: AccountId, day: Date, amount: Money) -> Event {
        event_with(
            account,
            day,
            1,
            EventKind::CashIn { amount },
            vec![Leg::cash(account, amount)],
        )
    }

    #[test]
    fn grouping_by_account_uses_each_leg_account_not_the_filing_account() {
        let filing_account = account(1);
        let receiving_account = account(2);
        let amount = money(1_250, CurrencyCode::Rub);
        let event = event_with(
            filing_account,
            date!(2026 - 08 - 04),
            1,
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: filing_account,
                to: receiving_account,
                amount,
            },
            vec![
                Leg::cash(filing_account, money(-1_250, CurrencyCode::Rub)),
                Leg::cash(receiving_account, amount),
            ],
        );

        let aggregate =
            aggregate_journal([&event], &[JournalAggregateGroupBy::Account], 10).unwrap();

        assert_eq!(aggregate.groups.len(), 2);
        assert_eq!(
            aggregate
                .groups
                .iter()
                .find(|group| group.account == Some(filing_account))
                .unwrap()
                .cash,
            vec![money(-1_250, CurrencyCode::Rub)]
        );
        assert_eq!(
            aggregate
                .groups
                .iter()
                .find(|group| group.account == Some(receiving_account))
                .unwrap()
                .cash,
            vec![amount]
        );
    }

    #[test]
    fn a_group_keeps_cash_entries_in_distinct_currencies() {
        let account = account(3);
        let rub = money(2_750, CurrencyCode::Rub);
        let usd = money(-125, CurrencyCode::Usd);
        let rub_event = cash_event(account, date!(2026 - 08 - 05), rub);
        let usd_event = cash_event(account, date!(2026 - 08 - 06), usd);

        let aggregate = aggregate_journal([&rub_event, &usd_event], &[], 10).unwrap();

        assert_eq!(aggregate.groups.len(), 1);
        assert_eq!(aggregate.groups[0].cash, vec![rub, usd]);
    }

    #[test]
    fn exceeding_the_group_ceiling_is_refused_with_actual_count() {
        let first = cash_event(
            account(4),
            date!(2026 - 08 - 07),
            money(300, CurrencyCode::Rub),
        );
        let second = cash_event(
            account(5),
            date!(2026 - 08 - 08),
            money(450, CurrencyCode::Rub),
        );

        let error = aggregate_journal([&first, &second], &[JournalAggregateGroupBy::Account], 1)
            .unwrap_err();

        assert_eq!(
            error,
            JournalAggregateError::GroupCeiling {
                ceiling: 1,
                actual: 2,
            }
        );
    }

    #[test]
    fn an_empty_ungrouped_selection_has_one_zero_event_group() {
        let aggregate = aggregate_journal(std::iter::empty::<&Event>(), &[], 1).unwrap();

        assert_eq!(
            aggregate.groups,
            vec![JournalAggregateGroup {
                account: None,
                currency: None,
                kind: None,
                month: None,
                events: 0,
                cash: Vec::new(),
                first_effective_date: None,
                last_effective_date: None,
            }]
        );
    }
}
