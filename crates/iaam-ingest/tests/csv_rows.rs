//! Построчный разбор CSV: одна непонятая строка не отменяет документ.

use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::instrument::AliasInterval;
use iaam_ingest::csv_source::{Directory, ParsedRow, parse};
use iaam_ingest::operation::OperationKind;
use time::macros::date;

fn directory() -> (Directory, AccountId, InstrumentId) {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let mut dir = Directory {
        default_custody: Some(CustodyId::new_random()),
        ..Directory::default()
    };
    dir.accounts
        .entry("Брокерский".into())
        .or_default()
        .push(account);
    dir.instruments.insert(
        "SBER".into(),
        vec![(
            "ticker".to_owned(),
            AliasInterval {
                valid_from: date!(1900 - 01 - 01),
                valid_to: None,
            },
            instrument,
        )],
    );
    (dir, account, instrument)
}

const HEADER: &str = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key";

#[test]
fn a_good_document_parses_into_operations() {
    let (dir, account, instrument) = directory();
    let document = format!(
        "{HEADER}\n\
         2026-01-10,deposit,Брокерский,,,,,100000.00,,,RUB,dep-1\n\
         2026-01-15,buy,Брокерский,,SBER,,100,29050.50,35.00,,RUB,buy-1\n"
    );
    let rows = parse(&document, &dir);
    assert_eq!(rows.len(), 2);

    match &rows[0] {
        ParsedRow::Operation(operation) => {
            assert_eq!(operation.account, account);
            assert_eq!(operation.idempotency_key.as_deref(), Some("dep-1"));
            assert_eq!(
                operation.kind,
                OperationKind::Deposit {
                    amount_minor: 10_000_000,
                    currency: iaam_core::money::CurrencyCode::Rub,
                }
            );
        }
        other => panic!("ожидалась операция, получено {other:?}"),
    }

    match &rows[1] {
        ParsedRow::Operation(operation) => match &operation.kind {
            OperationKind::Buy {
                instrument: parsed,
                gross_minor,
                fee_minor,
                ..
            } => {
                assert_eq!(*parsed, instrument);
                // 29 050,50 рубля = 2 905 050 копеек; комиссия 35,00 = 3 500.
                assert_eq!(*gross_minor, 2_905_050);
                assert_eq!(*fee_minor, Some(3_500));
            }
            other => panic!("ожидалась покупка, получено {other:?}"),
        },
        other => panic!("ожидалась операция, получено {other:?}"),
    }
}

#[test]
fn неоднозначное_название_счёта_отвергается_только_в_ссылающейся_строке() {
    let (mut dir, _, _) = directory();
    let duplicate = AccountId::new_random();
    let unique = AccountId::new_random();
    dir.accounts
        .entry("Брокерский".into())
        .or_default()
        .push(duplicate);
    dir.accounts
        .entry("Однозначный".into())
        .or_default()
        .push(unique);

    let document = format!(
        "{HEADER}\n\
         2026-01-10,deposit,Брокерский,,,,,100000.00,,,RUB,duplicate\n\
         2026-01-11,deposit,Однозначный,,,,,1000.00,,,RUB,unique\n"
    );
    let rows = parse(&document, &dir);

    let ParsedRow::Rejected(rejection) = &rows[0] else {
        panic!("неоднозначное название должно быть отвергнуто: {:?}", rows[0]);
    };
    assert_eq!(rejection.field, "account");
    assert_eq!(
        rejection.actual,
        "Брокерский: название счёта неоднозначно: 2 счёта"
    );

    let ParsedRow::Operation(operation) = &rows[1] else {
        panic!("однозначное название должно разрешаться: {:?}", rows[1]);
    };
    assert_eq!(operation.account, unique);
}

#[test]
fn one_bad_row_does_not_cancel_the_document() {
    let (dir, _, _) = directory();
    let document = format!(
        "{HEADER}\n\
         2026-01-10,deposit,Брокерский,,,,,100000.00,,,RUB,\n\
         не-дата,deposit,Брокерский,,,,,1000.00,,,RUB,\n\
         2026-01-12,deposit,Неизвестный счёт,,,,,1000.00,,,RUB,\n\
         2026-01-13,летающая операция,Брокерский,,,,,1000.00,,,RUB,\n"
    );
    let rows = parse(&document, &dir);
    assert_eq!(rows.len(), 4);
    assert!(matches!(rows[0], ParsedRow::Operation(_)));

    let fields: Vec<&str> = rows[1..]
        .iter()
        .map(|row| match row {
            ParsedRow::Rejected(rejection) => rejection.field.as_str(),
            ParsedRow::Operation(_) => "операция",
        })
        .collect();
    assert_eq!(fields, vec!["date", "account", "type"]);
}

#[test]
fn an_amount_more_precise_than_the_currency_is_rejected_not_rounded() {
    // Округлив входную сумму, система запишет факт, которого не было.
    let (dir, _, _) = directory();
    let document = format!("{HEADER}\n2026-01-10,deposit,Брокерский,,,,,100.005,,,RUB,\n");
    let rows = parse(&document, &dir);
    match &rows[0] {
        ParsedRow::Rejected(rejection) => {
            assert_eq!(rejection.field, "amount");
            assert_eq!(rejection.actual, "100.005");
        }
        other => panic!("ожидался отказ, получено {other:?}"),
    }
}

#[test]
fn a_parsed_row_carries_the_date_into_both_the_trade_and_the_cash_date() {
    // CSV даёт одну дату, а событие несёт шесть семантических (§4.2).
    // Строка заполняет ту, в которой операция произошла, и ту, в которую
    // прошли деньги: потерянная дата сделки уводит операцию в другой
    // налоговый период, потерянная дата денег — из ряда потоков.
    let (dir, _, _) = directory();
    let document = format!(
        "{HEADER}\n\
         2026-01-10,deposit,Брокерский,,,,,100000.00,,,RUB,dep-1\n"
    );
    let rows = parse(&document, &dir);
    let ParsedRow::Operation(operation) = &rows[0] else {
        panic!("строка обязана разобраться: {:?}", rows[0]);
    };
    let expected = time::macros::date!(2026 - 01 - 10);
    assert_eq!(operation.dates.trade, Some(expected));
    assert_eq!(operation.dates.cash_posted, Some(expected));
}

#[test]
fn an_empty_custody_column_falls_back_to_the_default_and_a_named_one_does_not() {
    // Пустая ячейка и отсутствующая колонка означают одно: места
    // хранения не назвали, поэтому берётся значение по умолчанию.
    // Названное место обязано разрешаться справочником — иначе бумаги
    // легли бы в депозитарий по умолчанию вопреки написанному в файле.
    let (mut dir, _, _) = directory();
    let named = CustodyId::new_random();
    dir.custodies.insert("НРД".into(), vec![named]);
    let default = dir.default_custody.expect("умолчание задано");

    let document = format!(
        "{HEADER}\n\
         2026-01-15,buy,Брокерский,,SBER,,100,29050.50,35.00,,RUB,buy-empty\n\
         2026-01-16,buy,Брокерский,,SBER,НРД,100,29050.50,35.00,,RUB,buy-named\n\
         2026-01-17,buy,Брокерский,,SBER,Неизвестный,100,29050.50,35.00,,RUB,buy-bad\n"
    );
    let rows = parse(&document, &dir);
    assert_eq!(rows.len(), 3);

    let custody_of = |row: &ParsedRow| match row {
        ParsedRow::Operation(operation) => match operation.kind {
            OperationKind::Buy { custody, .. } => Some(custody),
            _ => panic!("ожидалась покупка"),
        },
        ParsedRow::Rejected(_) => None,
    };
    assert_eq!(custody_of(&rows[0]), Some(default));
    assert_eq!(custody_of(&rows[1]), Some(named));
    assert_eq!(
        custody_of(&rows[2]),
        None,
        "неизвестное имя — отказ, а не молчаливое умолчание"
    );
    let ParsedRow::Rejected(rejection) = &rows[2] else {
        unreachable!()
    };
    assert_eq!(rejection.field, "custody");
}
