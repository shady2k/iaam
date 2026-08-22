//! Соответствие продакшн-реализации списания эталонной (§15.4).
//!
//! Оба прогоняются на одних входных данных, и оба сверяются
//! с замороженным ожидаемым значением. Совпадение продакшена
//! с эталоном без сверки с фикстурой недостаточно: обе реализации
//! могли бы ошибаться одинаково по случайности.

use iaam_core::dates::TradeDate;
use iaam_core::ids::InstrumentId;
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::rules::lot_disposal::{DisposalInput, FifoV1, Lot, LotDisposalRule, LotId};
use iaam_oracle::lots_reference::{RefLot, dispose_fifo_rational};
use rust_decimal::Decimal;
use serde::Deserialize;
use time::macros::date;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    lots: Vec<RefLotJson>,
    sell_quantity: i64,
    expected_basis_released_minor: i64,
    expected_remaining: Vec<RefLotJson>,
}

#[derive(Deserialize, Clone, Copy)]
struct RefLotJson {
    quantity: i64,
    basis_minor: i64,
}

fn qty(n: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(n)))
}

fn to_core_lots(items: &[RefLotJson]) -> Vec<Lot> {
    let instrument = InstrumentId::new_random();
    items
        .iter()
        .map(|l| Lot {
            id: LotId::new_random(),
            instrument,
            acquired: Some(TradeDate(date!(2026 - 01 - 01))),
            quantity: qty(l.quantity),
            cost_basis: Money::new(PostedMinor::new(l.basis_minor), CurrencyCode::Rub),
        })
        .collect()
}

#[test]
fn production_matches_oracle_and_frozen_expectations() {
    let raw = include_str!("../../../tests/fixtures/fifo_cases.json");
    let fixture: Fixture = serde_json::from_str(raw).expect("фикстура разбирается");
    assert!(!fixture.cases.is_empty(), "фикстура не должна быть пустой");

    for case in &fixture.cases {
        // --- Эталон ---
        let ref_lots: Vec<RefLot> = case
            .lots
            .iter()
            .map(|l| RefLot {
                quantity: l.quantity,
                basis_minor: l.basis_minor,
            })
            .collect();
        let oracle = dispose_fifo_rational(&ref_lots, case.sell_quantity)
            .unwrap_or_else(|e| panic!("эталон упал на случае «{}»: {e:?}", case.name));

        // --- Продакшн ---
        let input = DisposalInput {
            lots: to_core_lots(&case.lots),
            quantity: qty(case.sell_quantity),
        };
        let production = FifoV1
            .apply(&input)
            .unwrap_or_else(|e| panic!("продакшн упал на случае «{}»: {e:?}", case.name));

        // --- Оба против замороженного ожидания ---
        assert_eq!(
            oracle.basis_released_minor, case.expected_basis_released_minor,
            "эталон разошёлся с фикстурой на случае «{}»",
            case.name
        );
        assert_eq!(
            production.basis_released.amount().raw(),
            case.expected_basis_released_minor,
            "продакшн разошёлся с фикстурой на случае «{}»",
            case.name
        );

        // --- Остатки ---
        assert_eq!(
            oracle.remaining.len(),
            case.expected_remaining.len(),
            "эталон: неверное число оставшихся лотов на «{}»",
            case.name
        );
        assert_eq!(
            production.remaining.len(),
            case.expected_remaining.len(),
            "продакшн: неверное число оставшихся лотов на «{}»",
            case.name
        );
        for (i, expected) in case.expected_remaining.iter().enumerate() {
            assert_eq!(
                oracle.remaining[i].basis_minor, expected.basis_minor,
                "эталон: стоимость остатка {i} на «{}»",
                case.name
            );
            assert_eq!(
                production.remaining[i].cost_basis.amount().raw(),
                expected.basis_minor,
                "продакшн: стоимость остатка {i} на «{}»",
                case.name
            );
            // Количество остатка тоже заморожено фикстурой: без этой сверки
            // поле `quantity` в `expected_remaining` не проверялось бы ничем,
            // и разнесение стоимости могло бы попасть на неверное количество.
            assert_eq!(
                oracle.remaining[i].quantity, expected.quantity,
                "эталон: количество остатка {i} на «{}»",
                case.name
            );
            assert_eq!(
                production.remaining[i].quantity,
                qty(expected.quantity),
                "продакшн: количество остатка {i} на «{}»",
                case.name
            );
        }
    }
}
