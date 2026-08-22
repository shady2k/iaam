//! Нормализованная операция обязана давать событие, проходящее
//! структурную проверку ядра. Это шов, на котором ломается всё:
//! приёмка строит ноги, а форму этих ног задаёт ядро.

use iaam_core::event::kind::{EventKind, FeeOrigin, TradeSide};
use iaam_core::ids::EventId;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{
    OperationDates, OperationKind, Rejection, SubmittedOperation, Verdict, normalize,
};
use rust_decimal::Decimal;
use time::macros::date;

fn context() -> NormalizationContext {
    NormalizationContext {
        owner: OwnerId::new_random(),
        source: SourceId::new_random(),
    }
}

fn submit(kind: OperationKind) -> SubmittedOperation {
    SubmittedOperation {
        account: AccountId::new_random(),
        kind,
        dates: OperationDates {
            cash_posted: Some(date!(2026 - 04 - 01)),
            trade: Some(date!(2026 - 04 - 01)),
            ..OperationDates::default()
        },
        idempotency_key: None,
        source_operation_id: None,
    }
}

fn all_kinds() -> Vec<OperationKind> {
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let quantity = Dec::new(Decimal::from(10));
    vec![
        OperationKind::Deposit {
            amount_minor: 100_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Withdrawal {
            amount_minor: 25_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Transfer {
            to: AccountId::new_random(),
            amount_minor: 40_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor: 900_000,
            fee_minor: Some(1_500),
            accrued_interest_minor: Some(700),
            currency: CurrencyCode::Rub,
        },
        OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor: 950_000,
            fee_minor: Some(1_500),
            accrued_interest_minor: Some(300),
            currency: CurrencyCode::Rub,
        },
        OperationKind::Income {
            instrument: Some(instrument),
            gross_minor: 12_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Fee {
            amount_minor: 900,
            currency: CurrencyCode::Rub,
            origin: FeeOrigin::Depositary,
        },
        OperationKind::OpeningCash {
            amount_minor: -5_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::OpeningPosition {
            instrument,
            custody,
            quantity,
            cost_basis_minor: None,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Valuation {
            instrument,
            price: Dec::new(Decimal::new(1_005, 1)),
            currency: CurrencyCode::Rub,
            quality: PriceQuality::OwnerEstimate,
        },
    ]
}

#[test]
fn every_operation_kind_produces_a_structurally_valid_event() {
    for kind in all_kinds() {
        let operation = submit(kind.clone());
        let normalized = normalize(&operation, context())
            .unwrap_or_else(|rejection| panic!("{kind:?} отклонена: {rejection:?}"));
        normalized
            .event
            .validate_structure()
            .unwrap_or_else(|error| panic!("{kind:?} даёт неверную форму: {error}"));
    }
}

#[test]
fn a_purchase_settles_for_body_plus_accrued_plus_fee() {
    // 9 000,00 тела + 7,00 НКД + 15,00 комиссии = списание 9 022,00.
    let operation = submit(OperationKind::Buy {
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        quantity: Dec::new(Decimal::from(10)),
        gross_minor: 900_000,
        fee_minor: Some(1_500),
        accrued_interest_minor: Some(700),
        currency: CurrencyCode::Rub,
    });
    let event = normalize(&operation, context()).unwrap().event;
    let cash = event
        .cash_effect(CurrencyCode::Rub)
        .expect("денежный эффект");
    assert_eq!(cash.amount(), PostedMinor::new(-902_200));
}

#[test]
fn a_sale_settles_for_body_plus_accrued_minus_fee() {
    // 9 500,00 тела + 3,00 НКД − 15,00 комиссии = приход 9 488,00.
    let operation = submit(OperationKind::Sell {
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        quantity: Dec::new(Decimal::from(10)),
        gross_minor: 950_000,
        fee_minor: Some(1_500),
        accrued_interest_minor: Some(300),
        currency: CurrencyCode::Rub,
    });
    let event = normalize(&operation, context()).unwrap().event;
    let cash = event
        .cash_effect(CurrencyCode::Rub)
        .expect("денежный эффект");
    assert_eq!(cash.amount(), PostedMinor::new(948_800));
    match event.kind {
        EventKind::Trade { side, .. } => assert_eq!(side, TradeSide::Sell),
        other => panic!("ожидалась сделка, получено {other:?}"),
    }
}

#[test]
fn a_negative_amount_is_rejected_with_field_expected_actual() {
    // Знак задаёт вид операции, а не клиент: отрицательное пополнение
    // не «исправляется» в вывод средств (§13, ответ 422).
    let operation = submit(OperationKind::Deposit {
        amount_minor: -1,
        currency: CurrencyCode::Rub,
    });
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "amount_minor");
    assert_eq!(rejection.actual, "-1");
}

#[test]
fn an_operation_without_any_date_is_rejected() {
    let mut operation = submit(OperationKind::Deposit {
        amount_minor: 1_000,
        currency: CurrencyCode::Rub,
    });
    operation.dates = OperationDates::default();
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "dates");
}

#[test]
fn a_transfer_to_the_same_account_is_rejected_before_the_legs_are_built() {
    let account = AccountId::new_random();
    let mut operation = submit(OperationKind::Transfer {
        to: account,
        amount_minor: 1_000,
        currency: CurrencyCode::Rub,
    });
    operation.account = account;
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "to");
}

#[test]
fn every_verdict_has_a_machine_readable_code_and_says_whether_it_was_recorded() {
    // Вердикт — это контракт с внешним агентом (§10.4): он разбирает код
    // и решает, повторять ли отправку. Пустой код неотличим от «вердикта
    // нет», а «записано» и «не записано», слипшись в одно значение,
    // превращают повтор либо в дубль, либо в потерю операции.
    let rejection = Rejection {
        field: "amount".into(),
        expected: "положительная величина".into(),
        actual: "-1".into(),
    };
    let table = [
        (
            Verdict::Provisional {
                event: EventId::new_random(),
            },
            "provisional",
            true,
        ),
        (
            Verdict::Duplicate {
                existing: EventId::new_random(),
            },
            "duplicate",
            true,
        ),
        (
            Verdict::NeedsClassification {
                question: "не понят вид операции".into(),
            },
            "needs_classification",
            false,
        ),
        (
            Verdict::Unsupported {
                reason: "производные вне периметра".into(),
            },
            "unsupported",
            false,
        ),
        (Verdict::Rejected { rejection }, "rejected", false),
    ];
    for (verdict, code, recorded) in table {
        assert_eq!(verdict.code(), code);
        assert_eq!(
            verdict.is_recorded(),
            recorded,
            "вердикт {code}: записано ли в журнал"
        );
    }
}

#[test]
fn a_zero_amount_is_rejected_just_like_a_negative_one() {
    // Граница: ноль положительной величиной не является. Операция на
    // нулевую сумму — это не операция, и записывать её как факт значит
    // засорять журнал событиями, которых не было.
    let zero = submit(OperationKind::Deposit {
        amount_minor: 0,
        currency: CurrencyCode::Rub,
    });
    let rejection = normalize(&zero, context()).expect_err("ноль обязан быть отклонён");
    assert_eq!(rejection.field, "amount_minor");
    assert_eq!(rejection.actual, "0");
}
