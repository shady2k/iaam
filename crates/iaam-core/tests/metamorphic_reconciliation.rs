//! Метаморфные тесты сверки (§15.6).
//!
//! Метаморфное отношение проверяет не конкретное число, а поведение при
//! известном преобразовании входа. Здесь это главное: сверка обязана
//! **не заметить** компенсирующую ошибку разбора — и обязана не выдать
//! за независимое подтверждение то, что им не является.

use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::{Dimension, DimensionStatus, ReconciliationLedger};
use time::macros::date;

mod support;
use support::{Posting, TestChannel, event_on};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
}

/// Журнал одного документа: операция и полный набор контрольных секций,
/// согласованных между собой.
///
/// `shift` сдвигает **обе** стороны сразу — это и есть компенсирующая
/// ошибка парсера: одна и та же ошибка разбора попала и в операцию,
/// и в контрольную секцию.
fn statement(owner: OwnerId, account: AccountId, channel: &TestChannel, shift: i64) -> Vec<Event> {
    let deposit = 100_000 + shift;
    let mut events = vec![event_on(
        channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 10),
            sequence: 1,
        },
        EventKind::CashIn {
            amount: rub(deposit),
        },
        vec![Leg::cash(account, rub(deposit))],
    )];
    for (index, claim) in [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(0),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(deposit),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(deposit),
            credit: PostedMinor::new(0),
        },
    ]
    .into_iter()
    .enumerate()
    {
        events.push(event_on(
            channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: u32::try_from(index).unwrap() + 10,
            },
            EventKind::ControlAssertion {
                period: march(),
                claim,
            },
            vec![],
        ));
    }
    events
}

#[test]
fn a_compensating_parser_error_never_reaches_independent() {
    // Критерий приёмки эпика. Парсер ошибся на семь копеек одинаково
    // в операции и в контрольной секции: обе стороны съехали, сверка
    // сошлась, и это ровно тот случай, ради которого §10.3 вводит
    // третий уровень вместо двух. Статус обязан остаться internal.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    let honest = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        0,
    ))
    .unwrap();
    let skewed = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        7,
    ))
    .unwrap();

    let honest_status = honest.status_for(account, date!(2026 - 03 - 15), Dimension::Cash);
    let skewed_status = skewed.status_for(account, date!(2026 - 03 - 15), Dimension::Cash);

    assert_eq!(
        honest_status, skewed_status,
        "сверка внутри одного документа не отличает верный разбор от \
         компенсирующе неверного — и именно поэтому не имеет права \
         называть его независимым"
    );
    assert_eq!(skewed_status, DimensionStatus::AcceptedInternal);
    assert_ne!(skewed_status, DimensionStatus::AcceptedIndependent);
}

#[test]
fn a_second_channel_catches_what_one_document_cannot() {
    // Обратная сторона: ошибка, невидимая внутри документа, становится
    // видимой, как только появляется независимый канал. Если бы это
    // было не так, второй канал не имел бы смысла.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    let mut events = statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        7,
    );
    // Второй канал видит верную сумму — и расходится с журналом,
    // в который попала ошибка первого.
    let api = TestChannel::new("tinkoff-api/1", "apimarch");
    events.push(event_on(
        &api,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 31),
            sequence: 20,
        },
        EventKind::ControlAssertion {
            period: march(),
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(100_000),
                at: BalancePoint::Closing,
            },
        },
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Discrepant,
        "независимый канал обязан поймать ошибку, которую документ \
         скрыл от самого себя"
    );
}

#[test]
fn reordering_the_journal_does_not_change_any_status() {
    // Проекция определяется журналом, а не порядком его чтения.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let events = statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        0,
    );
    let mut reversed = events.clone();
    reversed.reverse();

    let straight = ReconciliationLedger::build(&events).unwrap();
    let backwards = ReconciliationLedger::build(&reversed).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            straight.status_for(account, date!(2026 - 03 - 15), dimension),
            backwards.status_for(account, date!(2026 - 03 - 15), dimension),
            "порядок чтения журнала изменил статус измерения {dimension:?}"
        );
    }
}

#[test]
fn scaling_every_amount_keeps_the_status() {
    // Умножение всех сумм на одно число — преобразование, при котором
    // сверка обязана вести себя одинаково: она сравнивает стороны,
    // а не оценивает масштаб.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let small = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        0,
    ))
    .unwrap();
    let large = ReconciliationLedger::build(&statement(
        owner,
        account,
        &TestChannel::new("tinkoff-xlsx/1", "march"),
        900_000,
    ))
    .unwrap();
    assert_eq!(
        small.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        large.status_for(account, date!(2026 - 03 - 15), Dimension::Cash)
    );
}
