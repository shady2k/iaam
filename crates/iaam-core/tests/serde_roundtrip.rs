//! Round-trip журнала через JSON (незакрытый вопрос первого плана).
//!
//! `docs/irreversible-core.md` фиксировал: корректность `Serialize`/
//! `Deserialize` держится на том, что derive компилируется. Журнал фактов,
//! не переживающий сериализацию, бесполезен — хранилище кладёт событие
//! в текстовое поле и читает обратно.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::kind::{EventKind, FeeOrigin, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RowLocator};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId, TransferId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use time::macros::date;

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn envelope(kind: EventKind, legs: Vec<Leg>) -> Event {
    let account = AccountId::new_random();
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner: OwnerId::new_random(),
        account,
        kind,
        dates: EventDates {
            trade: Some(TradeDate(date!(2025 - 12 - 30))),
            cash_posted: Some(CashPostedDate(date!(2026 - 01 - 03))),
            ..EventDates::empty()
        },
        order: EffectiveOrder::new(date!(2026 - 01 - 03), 7),
        legs,
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"f".repeat(64)).expect("хеш"),
            ParserVersion("manual/1".into()),
        )
        .with_source_operation_id("op-42")
        .with_row(RowLocator {
            // Документ назван хешом, а не именем файла: тот же отчёт,
            // сохранённый под другим именем, обязан остаться тем же
            // документом.
            document: RawHash::parse(&"a".repeat(64)).expect("хеш документа"),
            sheet: Some("Сделки".into()),
            row: 17,
        }),
        relation: Relation::Replacement {
            target: EventId::new_random(),
        },
        confidence: Confidence::Estimated,
        idempotency_key: Some("key-1".into()),
    }
}

/// Каждый вариант `EventKind`, чтобы новый вариант ломал этот тест
/// вместе со сборкой, а не молча оставался непроверенным.
fn every_kind() -> Vec<Event> {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    vec![
        envelope(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(10),
                gross: rub(100_000),
                fee: Some(rub(500)),
                accrued_interest: Some(rub(1_234)),
            },
            vec![
                Leg::cash(account, rub(99_500)),
                Leg::security(account, custody, instrument, qty(-10)),
            ],
        ),
        envelope(
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(account, rub(1))],
        ),
        envelope(
            EventKind::CashOut { amount: rub(-1) },
            vec![Leg::cash(account, rub(-1))],
        ),
        envelope(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: account,
                to: AccountId::new_random(),
                amount: rub(5_000),
            },
            vec![Leg::cash(account, rub(-5_000))],
        ),
        envelope(
            EventKind::Income {
                instrument: Some(instrument),
                gross: rub(700),
            },
            vec![Leg::cash(account, rub(700))],
        ),
        envelope(
            EventKind::Fee {
                amount: rub(-99),
                origin: FeeOrigin::MarginInterest,
            },
            vec![Leg::fee(account, rub(-99))],
        ),
        envelope(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(3),
                cost_basis: None,
                assertions: iaam_core::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(account, custody, instrument, qty(3))],
        ),
        envelope(
            EventKind::OpeningCash { amount: rub(-42) },
            vec![Leg::cash(account, rub(-42))],
        ),
        envelope(
            EventKind::Valuation {
                instrument,
                price: Dec::new(Decimal::new(1_234_567, 4)),
                currency: CurrencyCode::Usd,
                quality: PriceQuality::Stale,
            },
            vec![],
        ),
    ]
}

#[test]
fn every_event_kind_survives_a_json_round_trip() {
    for event in every_kind() {
        let json = serde_json::to_string(&event).expect("сериализация");
        let back: Event = serde_json::from_str(&json).expect("разбор");
        assert_eq!(back, event, "round-trip изменил событие: {json}");
    }
}

#[test]
fn a_decimal_keeps_its_scale_through_json() {
    // Масштаб — часть значения: 1.2340 и 1.234 различаются точностью
    // источника, и потеря масштаба меняет смысл цены (§3.4).
    let price = Dec::new(Decimal::new(12_340, 4));
    let json = serde_json::to_string(&price).expect("сериализация");
    let back: Dec = serde_json::from_str(&json).expect("разбор");
    assert_eq!(back.inner().scale(), 4, "масштаб потерян: {json}");
    assert_eq!(back, price);
}
