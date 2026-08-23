//! Статус полноты счёта на интервале по измерению (§10.3).

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

fn deposit(channel: &TestChannel, owner: OwnerId, account: AccountId, minor: i64) -> Event {
    event_on(
        channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 10),
            sequence: 1,
        },
        EventKind::CashIn { amount: rub(minor) },
        vec![Leg::cash(account, rub(minor))],
    )
}

/// Контрольные величины одного документа.
struct Sections {
    opening: i64,
    closing: i64,
    debit: i64,
    credit: i64,
}

/// Полный набор контрольных секций одного документа: остаток на начало,
/// остаток на конец и обороты. Именно такой набор даёт основание 5.
///
/// Дата утверждений — конец интервала: контрольная секция говорит о
/// периоде целиком, и отдельным аргументом её задавать незачем.
fn full_sections(
    channel: &TestChannel,
    owner: OwnerId,
    account: AccountId,
    period: AssertionPeriod,
    sections: Sections,
) -> Vec<Event> {
    [
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(sections.opening),
            at: BalancePoint::Opening,
        },
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(sections.closing),
            at: BalancePoint::Closing,
        },
        ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(sections.debit),
            credit: PostedMinor::new(sections.credit),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(index, claim)| {
        event_on(
            channel,
            Posting {
                owner,
                account,
                day: period.to,
                sequence: u32::try_from(index).unwrap() + 10,
            },
            EventKind::ControlAssertion { period, claim },
            vec![],
        )
    })
    .collect()
}

#[test]
fn separate_sections_that_all_agree_raise_the_period_to_internal() {
    // Основание 5: независимые уравнения, но один документ и один
    // парсер. Выше internal подняться не может по устройству.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::TaxBasis),
        DimensionStatus::Provisional,
        "налоговая стоимость денежным остатком не подтверждается"
    );
}

#[test]
fn one_agreeing_section_is_not_enough_for_ground_five() {
    // Один сошедшийся остаток не является совпадением независимых
    // уравнений: он подтверждает сам себя. Основание 5 требует, чтобы
    // сошлись и остаток, и оборот — величины, считающиеся по-разному.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let events = vec![
        deposit(&march_channel, owner, account, 100_000),
        event_on(
            &march_channel,
            Posting {
                owner,
                account,
                day: date!(2026 - 03 - 31),
                sequence: 10,
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
        ),
    ];

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Provisional
    );
}

#[test]
fn a_discrepancy_wins_over_any_amount_of_confirmation() {
    // Подтверждение не затирает несошедшуюся цифру. Иначе достаточно
    // было бы приложить второй документ, чтобы расхождение исчезло
    // с экрана, оставшись в данных.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    // Обороты сойдутся, а конечный остаток — нет.
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 999_999,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Discrepant
    );
}

#[test]
fn two_independent_channels_over_the_same_period_reach_independent() {
    // Основание 3. Тот же период, те же цифры, другой парсер и другой
    // документ — условие независимости §10.3 выполнено.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let apimarch_channel = TestChannel::new("tinkoff-api/1", "apimarch");
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));
    events.extend(full_sections(
        &apimarch_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn two_statements_of_the_same_parser_never_reach_independent() {
    // Прямая формулировка §10.3. Два разных документа одного брокера,
    // разобранные одним парсером, — это непрерывность, а не
    // независимость.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let copyone_channel = TestChannel::new("tinkoff-xlsx/1", "copyone");
    let copytwo_channel = TestChannel::new("tinkoff-xlsx/1", "copytwo");
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &copyone_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));
    events.extend(full_sections(
        &copytwo_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal
    );
}

#[test]
fn the_opening_of_the_next_statement_confirms_the_previous_period() {
    // Основание 1. Апрельский отчёт начинается с того остатка, который
    // мы насчитали за март: подтверждается МАРТ, а не апрель — в апреле
    // подтверждать ещё нечего.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let april_channel = TestChannel::new("tinkoff-xlsx/1", "april");
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let april = AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30)).unwrap();

    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    // Март: только конечный остаток, без оборотов — основания 5 не даёт.
    events.push(event_on(
        &march_channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 03 - 31),
            sequence: 10,
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
    // Апрель: начальный остаток совпадает с вычисленным мартовским.
    events.push(event_on(
        &april_channel,
        Posting {
            owner,
            account,
            day: date!(2026 - 04 - 30),
            sequence: 10,
        },
        EventKind::ControlAssertion {
            period: april,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(100_000),
                at: BalancePoint::Opening,
            },
        },
        vec![],
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        ledger.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedInternal,
        "подтверждён март"
    );
    assert_eq!(
        ledger.status_for(account, date!(2026 - 04 - 15), Dimension::Cash),
        DimensionStatus::Provisional,
        "апрель начальным остатком не подтверждается: в нём подтверждать нечего"
    );
}

#[test]
fn a_period_without_assertions_stays_provisional() {
    // Отсутствие утверждений — это отсутствие подтверждения, а не
    // подтверждение отсутствия проблем.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let none_channel = TestChannel::new("manual/1", "none");
    let events = vec![deposit(&none_channel, owner, account, 100_000)];
    let ledger = ReconciliationLedger::build(&events).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            ledger.status_for(account, date!(2026 - 03 - 15), dimension),
            DimensionStatus::Provisional
        );
    }
}

#[test]
fn the_ledger_is_a_pure_function_of_the_journal() {
    // Тот же журнал — тот же статус. Иначе воспроизвести показанную
    // владельцу цифру невозможно, а §3.1 требует именно этого.
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");
    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let first = ReconciliationLedger::build(&events).unwrap();
    let second = ReconciliationLedger::build(&events).unwrap();
    for dimension in Dimension::all() {
        assert_eq!(
            first.status_for(account, date!(2026 - 03 - 15), dimension),
            second.status_for(account, date!(2026 - 03 - 15), dimension)
        );
    }
}

#[test]
fn a_discrepancy_covered_by_a_perimeter_exception_is_excepted_not_discrepant() {
    // §11: система знает, почему цифры не сходятся, и не отправляет
    // владельца чинить то, что не поддерживает. Но подтверждением это
    // не становится: измерение не поднимается выше provisional.
    use iaam_core::perimeter::PerimeterExceptions;
    use iaam_core::reconciliation::check::{ClaimOutcome, ReconciliationException};

    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");

    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 999_999,
            debit: 100_000,
            credit: 0,
        },
    ));

    let bare = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        bare.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Discrepant,
        "без исключения это обычное расхождение"
    );

    let mut exceptions = PerimeterExceptions::default();
    exceptions.add(
        account,
        Dimension::Cash,
        ReconciliationException::UnsupportedFinancingPresent,
    );
    let excused = ReconciliationLedger::build_with(&events, &exceptions).unwrap();

    assert_eq!(
        excused.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::Provisional,
        "исключение снимает требование чинить, но не подтверждает данные"
    );
    let status = excused
        .statuses()
        .find(|status| status.account() == account)
        .expect("статус за март");
    assert!(
        status
            .outcomes()
            .iter()
            .any(|check| matches!(check.outcome, ClaimOutcome::Excepted { .. })),
        "исход обязан быть помечен исключением, а не расхождением"
    );
    assert!(
        !status
            .outcomes()
            .iter()
            .any(|check| matches!(check.outcome, ClaimOutcome::Discrepant(_))),
        "накрытое исключением расхождение не остаётся расхождением"
    );
}

#[test]
fn a_status_carries_the_grounds_that_produced_it() {
    // Владелец спрашивает не только «можно ли верить», но и «почему».
    // Статус без оснований — это цифра без объяснения, а §10.3 вводит
    // основания именно для того, чтобы уровень можно было проверить.
    use iaam_core::reconciliation::evidence::Ground;

    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let march_channel = TestChannel::new("tinkoff-xlsx/1", "march");

    let mut events = vec![deposit(&march_channel, owner, account, 100_000)];
    events.extend(full_sections(
        &march_channel,
        owner,
        account,
        march(),
        Sections {
            opening: 0,
            closing: 100_000,
            debit: 100_000,
            credit: 0,
        },
    ));

    let ledger = ReconciliationLedger::build(&events).unwrap();
    let status = ledger
        .statuses()
        .find(|status| status.account() == account)
        .expect("статус за март");

    assert_eq!(status.period(), march());
    let grounds: Vec<Ground> = status
        .evidence()
        .iter()
        .map(iaam_core::reconciliation::evidence::Evidence::ground)
        .collect();
    assert_eq!(
        grounds,
        vec![Ground::SeparateSectionsAgree],
        "статус обязан назвать основание, по которому он получен"
    );
    assert_eq!(
        status.outcomes().len(),
        3,
        "все три проверенных утверждения остаются видимыми"
    );
}
