//! Достаточность факта замещения (§16.1).
//!
//! Тест стартует **с лотов предшественника и самого события** и получает
//! лоты преемника. Иначе он доказывал бы достаточность правила, а не
//! факта: правило можно дописать, а поля уже записанного события — нет.
//!
//! Это единственная проверка эпика, чей провал означает необратимую
//! потерю. Если перенос налоговой стоимости и срока владения не выводится
//! из факта, E5 будет угадывать, и угадывать по данным, которых больше
//! неоткуда взять.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates, TradeDate};
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::lots::{LotBook, LotKey};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use rust_decimal::Decimal;
use time::macros::date;

const ACQUIRED: time::Date = date!(2024 - 03 - 01);
const EFFECTIVE: time::Date = date!(2026 - 09 - 01);

fn rub(minor: i64) -> Money {
    Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
}

fn qty(units: i64) -> Quantity {
    Quantity(Dec::new(Decimal::from(units)))
}

fn dec(text: &str) -> Dec {
    Dec::new(Decimal::from_str_exact(text).unwrap())
}

struct Swap {
    account: AccountId,
    custody: CustodyId,
    predecessor: InstrumentId,
    successor: InstrumentId,
}

impl Swap {
    fn new() -> Self {
        Self {
            account: AccountId::new_random(),
            custody: CustodyId::new_random(),
            predecessor: InstrumentId::new_random(),
            successor: InstrumentId::new_random(),
        }
    }

    fn event(&self, day: time::Date, sequence: u32, kind: EventKind, legs: Vec<Leg>) -> Event {
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account: self.account,
            kind,
            dates: EventDates {
                trade: Some(TradeDate(day)),
                ..EventDates::for_cash(CashPostedDate(day))
            },
            order: EffectiveOrder::new(day, sequence),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"f".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    /// Покупка предшественника — единственный источник лотов в тесте.
    fn bought(&self) -> Event {
        self.event(
            ACQUIRED,
            0,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: self.predecessor,
                quantity: qty(10),
                gross: rub(1_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(self.account, rub(-1_000_000)),
                Leg::security(self.account, self.custody, self.predecessor, qty(10)),
            ],
        )
    }

    fn converted(
        &self,
        ratio: &str,
        quantity_out: i64,
        transfer: BasisTransferRule,
        fractional: FractionalTreatment,
        compensation: Option<Money>,
    ) -> Event {
        let mut legs = vec![
            Leg::security(self.account, self.custody, self.predecessor, qty(-10)),
            Leg::security(
                self.account,
                self.custody,
                self.successor,
                qty(quantity_out),
            ),
        ];
        if let Some(compensation) = compensation {
            legs.push(Leg::cash(self.account, compensation));
        }
        self.event(
            EFFECTIVE,
            1,
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: self.predecessor,
                    successor: self.successor,
                    custody: self.custody,
                    ratio: dec(ratio),
                    quantity_in: qty(10),
                    quantity_out: qty(quantity_out),
                    fractional,
                    compensation,
                    effective_date: EFFECTIVE,
                    record_date: None,
                    grounds: None,
                    basis_transfer: transfer,
                },
            },
            legs,
        )
    }

    fn predecessor_key(&self) -> LotKey {
        LotKey {
            account: self.account,
            instrument: self.predecessor,
        }
    }

    fn successor_key(&self) -> LotKey {
        LotKey {
            account: self.account,
            instrument: self.successor,
        }
    }
}

fn book_after(swap: &Swap, conversion: &Event) -> LotBook {
    let rules = RuleRegistry::with_defaults();
    let mut book = LotBook::new(LotRuleVersion(1));
    book.apply(&swap.bought(), &rules).expect("покупка");
    // Форма факта проверяется тем же заслоном, что и в журнале: тест
    // не имеет права применить событие, которое журнал бы не принял.
    conversion.validate_structure().expect("форма замещения");
    book.apply(conversion, &rules).expect("замещение");
    book
}

#[test]
fn successor_lots_are_derived_from_predecessor_lots_and_the_event_alone() {
    let swap = Swap::new();
    let book = book_after(
        &swap,
        &swap.converted(
            "1",
            10,
            BasisTransferRule::CarryOver,
            FractionalTreatment::NotApplicable,
            None,
        ),
    );

    let successor = book.entry(&swap.successor_key()).expect("лоты преемника");
    assert_eq!(successor.quantity().unwrap(), qty(10));
    assert_eq!(successor.remaining_basis().unwrap(), Some(rub(1_000_000)));
    // Срок владения переходит целиком: замещение не является
    // приобретением, и обнулить его значило бы отнять ЛДВ.
    assert_eq!(
        successor.lots()[0].acquired,
        Some(TradeDate(ACQUIRED)),
        "срок владения обнулился"
    );

    // Стоимость обязана прийти к преемнику как приобретённая, иначе
    // тождество «приобретено = осталось + списано» перестанет держаться
    // и инвариант проекции остановит отчёт (projection/invariants.rs).
    assert_eq!(successor.acquired_basis(), Some(rub(1_000_000)));
    assert_eq!(
        successor.lots()[0].acquisition_basis,
        Some(rub(1_000_000)),
        "историческая стоимость должна перейти к преемнику"
    );
    assert_eq!(successor.released_basis(), None);

    let predecessor = book
        .entry(&swap.predecessor_key())
        .expect("запись предшественника");
    assert_eq!(predecessor.quantity().unwrap(), qty(0));
    assert_eq!(predecessor.released_basis(), Some(rub(1_000_000)));

    // Тождество сохранения стоимости по обеим записям.
    for entry in [predecessor, successor] {
        let acquired = entry.acquired_basis().expect("стоимость приобретения");
        let remaining = entry.remaining_basis().unwrap().unwrap_or_else(|| rub(0));
        let released = entry.released_basis().unwrap_or_else(|| rub(0));
        assert_eq!(
            acquired,
            remaining.try_add(released).unwrap(),
            "замещение потеряло стоимость"
        );
    }
}

#[test]
fn a_restart_rule_starts_the_holding_period_at_the_effective_date() {
    let swap = Swap::new();
    let book = book_after(
        &swap,
        &swap.converted(
            "1",
            10,
            BasisTransferRule::Restart,
            FractionalTreatment::NotApplicable,
            None,
        ),
    );

    let successor = book.entry(&swap.successor_key()).expect("лоты преемника");
    assert_eq!(successor.lots()[0].acquired, Some(TradeDate(EFFECTIVE)));
}

#[test]
fn a_cash_compensated_fraction_does_not_silently_reduce_the_basis() {
    // Как компенсация дробей влияет на налоговую базу — правило E5.
    // Часть 1 её только хранит; вычесть её здесь значило бы решить
    // за E5 и записать решение в необратимую проекцию.
    let swap = Swap::new();
    let book = book_after(
        &swap,
        &swap.converted(
            "1.55",
            15,
            BasisTransferRule::CarryOver,
            FractionalTreatment::CashCompensated,
            Some(rub(500)),
        ),
    );

    let successor = book.entry(&swap.successor_key()).expect("лоты преемника");
    assert_eq!(successor.remaining_basis().unwrap(), Some(rub(1_000_000)));
    assert_eq!(successor.quantity().unwrap(), qty(15));
}

#[test]
fn the_ratio_of_the_event_decides_the_successor_quantity() {
    let swap = Swap::new();
    let book = book_after(
        &swap,
        &swap.converted(
            "1.5",
            15,
            BasisTransferRule::CarryOver,
            FractionalTreatment::NotApplicable,
            None,
        ),
    );

    assert_eq!(
        book.entry(&swap.successor_key())
            .expect("лоты преемника")
            .quantity()
            .unwrap(),
        qty(15)
    );
}
