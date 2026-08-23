//! Свойства сверки с указанием области применимости (§15.3).
//!
//! Свойства сформулированы из правил §10.3, а не выведены из прогона
//! программы (§15.5). Каждое сопровождается оговоркой о том, где оно
//! выполняется: свойство без области — источник ложных падений, на
//! которые проще всего ответить ослаблением генератора до тавтологии.

use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use proptest::prelude::*;
use time::macros::date;

mod support;
use support::{Posting, TestChannel, event_on};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

/// Журнал из нескольких документов **одного парсера**.
///
/// Документы различаются именем, суммы произвольны, контрольные секции
/// согласованы с операцией — то есть сверка сойдётся. Именно на таком
/// входе проверяется, что совпадение внутри одного кода разбора не
/// повышает статус до независимого.
fn one_parser_journal(deposits: &[i64]) -> (AccountId, Vec<Event>) {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let total: i64 = deposits.iter().sum();
    let mut events = Vec::new();

    for (index, amount) in deposits.iter().enumerate() {
        let channel = TestChannel::new("same/1", &format!("doc{index}"));
        events.push(event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 10),
                sequence: u32::try_from(index).unwrap() + 1,
            },
            EventKind::CashIn {
                amount: rub(*amount),
            },
            vec![Leg::cash(account, rub(*amount))],
        ));
    }
    // Контрольные секции последнего документа согласованы с итогом.
    let channel = TestChannel::new("same/1", "control");
    for (index, claim) in [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(total),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(total),
            credit: PostedMinor::new(0),
        },
    ]
    .into_iter()
    .enumerate()
    {
        events.push(event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: u32::try_from(index).unwrap() + 100,
            },
            EventKind::ControlAssertion {
                period: march(),
                claim,
            },
            vec![],
        ));
    }
    (account, events)
}

proptest! {
    /// **Область:** журналы, все каналы которых делят версию парсера.
    ///
    /// Правило §10.3: подтверждающие данные не должны проходить через
    /// тот же код разбора. Пока парсер один, независимости нет ни при
    /// каком совпадении цифр — сколько бы документов ни сошлось.
    #[test]
    fn one_parser_never_reaches_independent(
        deposits in prop::collection::vec(1_i64..=1_000_000, 1..=5)
    ) {
        let (account, events) = one_parser_journal(&deposits);
        let ledger = ReconciliationLedger::build(&events).unwrap();
        for dimension in Dimension::all() {
            let status = ledger.status_for(account, date!(2026 - 03 - 15), dimension);
            prop_assert_ne!(
                status,
                DimensionStatus::AcceptedIndependent,
                "измерение {:?} объявлено независимо подтверждённым на одном парсере",
                dimension
            );
        }
    }

    /// **Область:** журналы, в которых ровно одно утверждение заведомо
    /// не сходится, а остальные сходятся.
    ///
    /// Расхождение поглощает: подтверждение не затирает несошедшуюся
    /// цифру, сколько бы сошедшихся утверждений ни стояло рядом.
    #[test]
    fn a_single_discrepancy_absorbs_any_number_of_confirmations(
        deposits in prop::collection::vec(1_i64..=1_000_000, 1..=5),
        skew in 1_i64..=999_999,
    ) {
        let (account, mut events) = one_parser_journal(&deposits);
        let total: i64 = deposits.iter().sum();
        let owner = OwnerId::new_random();
        let channel = TestChannel::new("same/1", "broken");
        events.push(event_on(
            &channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: 200,
            },
            EventKind::ControlAssertion {
                period: march(),
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(total + skew),
                    at: BalancePoint::Closing,
                },
            },
            vec![],
        ));

        let ledger = ReconciliationLedger::build(&events).unwrap();
        prop_assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
            DimensionStatus::Discrepant
        );
    }

    /// **Область:** любой журнал контрольных утверждений.
    ///
    /// Реестр — чистая функция: тот же вход даёт тот же выход. Без
    /// этого показанную владельцу цифру невозможно воспроизвести (§3.1).
    #[test]
    fn the_ledger_is_deterministic(
        deposits in prop::collection::vec(1_i64..=1_000_000, 1..=5)
    ) {
        let (account, events) = one_parser_journal(&deposits);
        let first = ReconciliationLedger::build(&events).unwrap();
        let second = ReconciliationLedger::build(&events).unwrap();
        for dimension in Dimension::all() {
            prop_assert_eq!(
                first.status_for(account, date!(2026 - 03 - 15), dimension),
                second.status_for(account, date!(2026 - 03 - 15), dimension)
            );
        }
    }
}
