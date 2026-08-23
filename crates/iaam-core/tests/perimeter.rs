//! Периметр: шорты, маржа, РЕПО вне периметра (§11).

use iaam_core::event::Event;
use iaam_core::event::kind::{EventKind, FeeOrigin};
use iaam_core::event::leg::Leg;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::perimeter::{NegativeCashClassification, PerimeterPolicy, assess};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::check::ReconciliationException;
use time::macros::date;

mod support;
use support::{Posting, TestChannel, event_on};

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn cash(account: AccountId, day: time::Date, minor: i64) -> Event {
    let kind = if minor >= 0 {
        EventKind::CashIn { amount: rub(minor) }
    } else {
        EventKind::CashOut { amount: rub(minor) }
    };
    event_on(
        &TestChannel::new("test/1", "journal"),
        Posting {
            owner: OwnerId::new_random(),
            account,
            day,
            sequence: 1,
        },
        kind,
        vec![Leg::cash(account, rub(minor))],
    )
}

fn margin_interest(account: AccountId, day: time::Date, minor: i64) -> Event {
    event_on(
        &TestChannel::new("test/1", "journal"),
        Posting {
            owner: OwnerId::new_random(),
            account,
            day,
            sequence: 2,
        },
        EventKind::Fee {
            amount: rub(minor),
            origin: FeeOrigin::MarginInterest,
        },
        vec![Leg::fee(account, rub(minor))],
    )
}

#[test]
fn a_deficit_closed_within_the_window_is_temporary() {
    // Минус из-за тайминга расчётов — нормальная работа, а не событие
    // вне периметра. Расчёты по счёту продолжаются.
    let account = AccountId::new_random();
    let events = vec![
        cash(account, date!(2026 - 03 - 10), -50_000),
        cash(account, date!(2026 - 03 - 12), 50_000),
    ];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    let span = assessment
        .spans()
        .first()
        .expect("минус обязан быть замечен");
    assert_eq!(
        span.classification,
        NegativeCashClassification::TemporarySettlementDeficit
    );
    assert_eq!(span.resolved, Some(date!(2026 - 03 - 12)));
    assert!(!assessment.blocks_period_reports(account));
}

#[test]
fn margin_interest_makes_it_an_unsupported_liability() {
    // Признак кредита есть — экономику финансирования система не
    // достраивает и отчёты за период не выдаёт (§11).
    let account = AccountId::new_random();
    let events = vec![
        cash(account, date!(2026 - 03 - 10), -50_000),
        margin_interest(account, date!(2026 - 03 - 11), -120),
        cash(account, date!(2026 - 03 - 12), 50_120),
    ];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    let span = assessment.spans().first().unwrap();
    assert_eq!(
        span.classification,
        NegativeCashClassification::UnsupportedMarginLiability,
        "быстро закрывшийся минус с процентами по марже остаётся \
         маржинальным обязательством: признак кредита сильнее срока"
    );
    assert!(assessment.financing_present(account));
    assert!(assessment.blocks_period_reports(account));
}

#[test]
fn an_unexplained_deficit_outside_the_window_is_unclassified() {
    let account = AccountId::new_random();
    let events = vec![cash(account, date!(2026 - 03 - 10), -50_000)];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    let span = assessment.spans().first().unwrap();
    assert_eq!(
        span.classification,
        NegativeCashClassification::UnclassifiedNegativeCash
    );
    assert_eq!(span.resolved, None);
    assert!(assessment.blocks_period_reports(account));
}

#[test]
fn other_accounts_keep_computing() {
    // Ключевое требование §11: отказ считать один счёт не отменяет
    // остальные. Иначе одна непонятая строка гасит весь портфель.
    let broken = AccountId::new_random();
    let healthy = AccountId::new_random();
    let events = vec![
        cash(broken, date!(2026 - 03 - 10), -50_000),
        margin_interest(broken, date!(2026 - 03 - 11), -120),
        cash(healthy, date!(2026 - 03 - 10), 70_000),
    ];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    assert!(assessment.blocks_period_reports(broken));
    assert!(
        !assessment.blocks_period_reports(healthy),
        "здоровый счёт продолжает считаться"
    );
    assert!(!assessment.financing_present(healthy));
}

#[test]
fn the_settlement_window_comes_from_the_policy() {
    // Порог обязан быть параметром: «допустимый срок» без торгового
    // календаря не вычисляется, и цифра, зависящая от порога, обязана
    // нести порог рядом с собой.
    let account = AccountId::new_random();
    let events = vec![
        cash(account, date!(2026 - 03 - 10), -50_000),
        cash(account, date!(2026 - 03 - 20), 50_000),
    ];
    let narrow = assess(
        &events,
        PerimeterPolicy {
            settlement_window_days: 5,
        },
    )
    .unwrap();
    assert_eq!(
        narrow.spans().first().unwrap().classification,
        NegativeCashClassification::UnclassifiedNegativeCash
    );

    let wide = assess(
        &events,
        PerimeterPolicy {
            settlement_window_days: 30,
        },
    )
    .unwrap();
    assert_eq!(
        wide.spans().first().unwrap().classification,
        NegativeCashClassification::TemporarySettlementDeficit
    );
    assert_eq!(
        wide.policy().settlement_window_days,
        30,
        "порог возвращается вместе с оценкой"
    );
}

#[test]
fn financing_produces_a_reconciliation_exception_for_cash_only() {
    // Исключение объясняет расхождение по деньгам. Количества бумаг
    // маржинальным финансированием не объясняются, и накрывать их
    // исключением значило бы прятать настоящее расхождение.
    let account = AccountId::new_random();
    let events = vec![
        cash(account, date!(2026 - 03 - 10), -50_000),
        margin_interest(account, date!(2026 - 03 - 11), -120),
    ];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    let exceptions = assessment.exceptions();
    assert_eq!(
        exceptions.covers(account, Dimension::Cash),
        Some(ReconciliationException::UnsupportedFinancingPresent)
    );
    assert_eq!(exceptions.covers(account, Dimension::Positions), None);
}

#[test]
fn a_positive_only_journal_has_no_spans() {
    // Отсутствие минуса — это отсутствие промежутков, а не пустая
    // оценка «на всякий случай».
    let account = AccountId::new_random();
    let events = vec![cash(account, date!(2026 - 03 - 10), 70_000)];
    let assessment = assess(&events, PerimeterPolicy::default()).unwrap();
    assert!(assessment.spans().is_empty());
    assert!(!assessment.blocks_period_reports(account));
    assert!(assessment.exceptions().is_empty());
}
