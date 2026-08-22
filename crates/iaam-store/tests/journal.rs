//! Журнал append-only и идемпотентность.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_store::SqliteStore;
use iaam_store::events::Appended;
use time::macros::date;

struct Ctx {
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
}

impl Ctx {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            source: SourceId::new_random(),
        }
    }

    fn deposit(&self, sequence: u32, minor: i64) -> Event {
        let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
        let day = date!(2026 - 02 - 01);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(self.account, amount)],
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&"1".repeat(64)).unwrap(),
                ParserVersion("manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
}

#[test]
fn an_event_survives_a_write_and_a_read() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let event = ctx.deposit(1, 100_000);
    assert_eq!(
        store.append_event(&event).unwrap(),
        Appended::Inserted { id: event.id }
    );
    let loaded = store.load_events(ctx.owner).unwrap();
    assert_eq!(loaded, vec![event]);
}

#[test]
fn the_journal_is_append_only_at_the_database_level() {
    // Дисциплина кода не переживает первый же скрипт починки данных,
    // поэтому запрет живёт в базе (§4.8).
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let event = ctx.deposit(1, 100_000);
    store.append_event(&event).unwrap();

    let update = store
        .connection()
        .execute("UPDATE events SET kind = 'cash_out'", []);
    assert!(update.is_err(), "UPDATE обязан быть отклонён базой");

    let delete = store.connection().execute("DELETE FROM events", []);
    assert!(delete.is_err(), "DELETE обязан быть отклонён базой");

    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 1);
}

#[test]
fn the_same_idempotency_key_returns_the_first_event() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let mut first = ctx.deposit(1, 100_000);
    first.idempotency_key = Some("import-42".into());
    let mut second = ctx.deposit(2, 555_000);
    second.idempotency_key = Some("import-42".into());

    store.append_event(&first).unwrap();
    assert_eq!(
        store.append_event(&second).unwrap(),
        Appended::Duplicate { existing: first.id }
    );
    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 1);
}

#[test]
fn the_same_source_operation_is_not_recorded_twice() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let mut first = ctx.deposit(1, 100_000);
    first.provenance = Provenance::new(
        ctx.source,
        RawHash::parse(&"2".repeat(64)).unwrap(),
        ParserVersion("broker/1".into()),
    )
    .with_source_operation_id("OP-7");
    let mut second = ctx.deposit(2, 100_000);
    second.provenance = first.provenance.clone();

    store.append_event(&first).unwrap();
    assert_eq!(
        store.append_event(&second).unwrap(),
        Appended::Duplicate { existing: first.id }
    );
}

#[test]
fn two_identical_purchases_on_the_same_day_are_both_recorded() {
    // Естественный ключ «счёт + дата + сумма» слишком слаб: две одинаковые
    // операции в один день — законная ситуация (§10.6, §15.9).
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    store.append_event(&ctx.deposit(1, 100_000)).unwrap();
    store.append_event(&ctx.deposit(2, 100_000)).unwrap();
    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 2);
}

#[test]
fn a_slice_through_a_date_excludes_later_events() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let early = ctx.deposit(1, 100_000);
    let mut late = ctx.deposit(2, 200_000);
    late.order = EffectiveOrder::new(date!(2026 - 03 - 01), 2);
    store.append_event(&early).unwrap();
    store.append_event(&late).unwrap();

    let slice = store
        .load_events_through(ctx.owner, date!(2026 - 02 - 15))
        .unwrap();
    assert_eq!(slice, vec![early]);
}

#[test]
fn migrations_are_idempotent() {
    let store = SqliteStore::open_in_memory().unwrap();
    iaam_store::schema::migrate(store.connection()).unwrap();
    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, iaam_store::schema::SCHEMA_VERSION);
}

#[test]
fn the_store_assigns_the_sequence_and_does_not_take_it_from_the_caller() {
    // Номер внутри дня — свойство журнала, а не пожелание клиента.
    // Обе операции приходят с номером 1; вторая обязана лечь второй,
    // иначе порядок внутри дня начнёт определяться случайным
    // идентификатором вместо объявленной семантики (§4.8).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();

    let first = store
        .append_event_in_order(&ctx.deposit(1, 100_000))
        .unwrap();
    let second = store
        .append_event_in_order(&ctx.deposit(1, 50_000))
        .unwrap();
    assert!(matches!(first, Appended::Inserted { .. }));
    assert!(matches!(second, Appended::Inserted { .. }));

    let stored = store.load_events(ctx.owner).unwrap();
    assert_eq!(stored.len(), 2);
    let day = date!(2026 - 02 - 01);
    assert_eq!(stored[0].order, EffectiveOrder::new(day, 1));
    assert_eq!(
        stored[1].order,
        EffectiveOrder::new(day, 2),
        "номер обязан быть назначен хранилищем, а не взят из события"
    );
}
