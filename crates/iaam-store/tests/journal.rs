//! Append-only journal and idempotency.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::evidence::IdentityScope;
use iaam_store::SqliteStore;
use iaam_store::events::Appended;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use time::macros::{date, time};

struct TempDatabase {
    path: PathBuf,
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("sqlite-wal"));
        let _ = fs::remove_file(self.path.with_extension("sqlite-shm"));
    }
}

fn concurrent_database(ctx: &Ctx) -> TempDatabase {
    TempDatabase {
        path: std::env::temp_dir().join(format!(
            "iaam-store-concurrent-ordering-{}.sqlite",
            ctx.owner.inner()
        )),
    }
}

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
        store.append_event(&event, IdentityScope::Source).unwrap(),
        Appended::Inserted { id: event.id }
    );
    let loaded = store.load_events(ctx.owner).unwrap();
    assert_eq!(loaded, vec![event]);
}

#[test]
fn the_journal_is_append_only_at_the_database_level() {
    // Code discipline does not survive the very first data-repair script,
    // so the prohibition lives in the database (§4.8).
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let event = ctx.deposit(1, 100_000);
    store.append_event(&event, IdentityScope::Source).unwrap();

    let update = store
        .connection()
        .execute("UPDATE events SET kind = 'cash_out'", []);
    assert!(update.is_err(), "UPDATE must be rejected by the database");

    let delete = store.connection().execute("DELETE FROM events", []);
    assert!(delete.is_err(), "DELETE must be rejected by the database");

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

    store.append_event(&first, IdentityScope::Source).unwrap();
    assert_eq!(
        store.append_event(&second, IdentityScope::Source).unwrap(),
        Appended::Duplicate { existing: first.id }
    );
    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 1);
}

#[test]
fn account_scope_allows_reused_source_operation_across_accounts() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let mut first = ctx.deposit(1, 100_000);
    first.provenance = Provenance::new(
        ctx.source,
        RawHash::parse(&"7".repeat(64)).unwrap(),
        ParserVersion("broker/1".into()),
    )
    .with_source_operation_id("OP-ACCOUNT");
    let mut second = ctx.deposit(2, 100_000);
    second.account = AccountId::new_random();
    second.provenance = first.provenance.clone();

    assert_eq!(
        store.append_event(&first, IdentityScope::Account).unwrap(),
        Appended::Inserted { id: first.id }
    );
    assert_eq!(
        store.append_event(&second, IdentityScope::Account).unwrap(),
        Appended::Inserted { id: second.id }
    );

    let mut repeat = second.clone();
    repeat.id = EventId::new_random();
    assert_eq!(
        store.append_event(&repeat, IdentityScope::Account).unwrap(),
        Appended::Duplicate {
            existing: second.id
        }
    );

    let mut source_scoped = second;
    source_scoped.id = EventId::new_random();
    assert!(matches!(
        store
            .append_event(&source_scoped, IdentityScope::Source)
            .unwrap(),
        Appended::Duplicate { .. }
    ));
    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 2);
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

    store.append_event(&first, IdentityScope::Source).unwrap();
    assert_eq!(
        store.append_event(&second, IdentityScope::Source).unwrap(),
        Appended::Duplicate { existing: first.id }
    );
}

#[test]
fn two_identical_purchases_on_the_same_day_are_both_recorded() {
    // The natural key “account + date + amount” is too weak: two identical
    // operations on the same day are a valid situation (§10.6, §15.9).
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    store
        .append_event(&ctx.deposit(1, 100_000), IdentityScope::Source)
        .unwrap();
    store
        .append_event(&ctx.deposit(2, 100_000), IdentityScope::Source)
        .unwrap();
    assert_eq!(store.load_events(ctx.owner).unwrap().len(), 2);
}

#[test]
fn a_slice_through_a_date_excludes_later_events() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let early = ctx.deposit(1, 100_000);
    let mut late = ctx.deposit(2, 200_000);
    late.order = EffectiveOrder::new(date!(2026 - 03 - 01), 2);
    store.append_event(&early, IdentityScope::Source).unwrap();
    store.append_event(&late, IdentityScope::Source).unwrap();

    let slice = store
        .load_events_through(ctx.owner, date!(2026 - 02 - 15))
        .unwrap();
    assert_eq!(slice, vec![early]);
}

#[test]
fn source_time_orders_events_before_sequence_and_untimed_events() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let day = date!(2026 - 02 - 01);

    let mut untimed = ctx.deposit(1, 100_000);
    untimed.order = EffectiveOrder::new(day, 1);
    let mut late = ctx.deposit(2, 200_000);
    late.order = EffectiveOrder::with_source_time(day, time!(12:00:00), 2);
    let mut early = ctx.deposit(3, 300_000);
    early.order = EffectiveOrder::with_source_time(day, time!(09:00:00), 3);

    store.append_event(&untimed, IdentityScope::Source).unwrap();
    store.append_event(&late, IdentityScope::Source).unwrap();
    store.append_event(&early, IdentityScope::Source).unwrap();

    let loaded = store.load_events(ctx.owner).unwrap();
    assert_eq!(loaded[0].order, early.order);
    assert_eq!(loaded[1].order, late.order);
    assert_eq!(loaded[2].order, untimed.order);
}

#[test]
fn equal_source_times_use_the_raw_hash_before_sequence() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let day = date!(2026 - 02 - 01);
    let mut first = ctx.deposit(1, 100_000);
    first.order = EffectiveOrder::with_source_time(day, time!(09:00:00), 2);
    first.provenance = Provenance::new(
        ctx.source,
        RawHash::parse(&"a".repeat(64)).unwrap(),
        ParserVersion("manual/1".into()),
    );
    let mut second = ctx.deposit(2, 200_000);
    second.order = EffectiveOrder::with_source_time(day, time!(09:00:00), 1);
    second.provenance = Provenance::new(
        ctx.source,
        RawHash::parse(&"b".repeat(64)).unwrap(),
        ParserVersion("manual/1".into()),
    );

    store.append_event(&second, IdentityScope::Source).unwrap();
    store.append_event(&first, IdentityScope::Source).unwrap();

    let loaded = store.load_events(ctx.owner).unwrap();
    assert_eq!(loaded[0].provenance.raw_hash().as_str(), "a".repeat(64));
    assert_eq!(loaded[1].provenance.raw_hash().as_str(), "b".repeat(64));
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
    // The within-day number is a property of the journal, not a client request.
    // Both operations arrive with number 1; the second must be recorded second,
    // otherwise the order within the day will be determined by a random
    // identifier instead of the declared semantics (§4.8).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();

    let first = store
        .append_event_in_order(&ctx.deposit(1, 100_000), IdentityScope::Source)
        .unwrap();
    let second = store
        .append_event_in_order(&ctx.deposit(1, 50_000), IdentityScope::Source)
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
        "the number must be assigned by the store, not taken from the event"
    );
}
#[test]
fn concurrent_writers_assign_distinct_sequences_or_report_an_error() {
    let ctx = Arc::new(Ctx::new());
    let database = concurrent_database(&ctx);
    let initial_store = SqliteStore::open(&database.path).unwrap();
    drop(initial_store);

    let first_store = SqliteStore::open(&database.path).unwrap();
    let second_store = SqliteStore::open(&database.path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let first_barrier = Arc::clone(&barrier);
    let first_event = ctx.deposit(1, 100_000);
    let first = thread::spawn(move || {
        let mut store = first_store;
        first_barrier.wait();
        store
            .append_event_in_order(&first_event, IdentityScope::Source)
            .map_err(|error| error.to_string())
    });

    let second_barrier = Arc::clone(&barrier);
    let second_event = ctx.deposit(1, 50_000);
    let second = thread::spawn(move || {
        let mut store = second_store;
        second_barrier.wait();
        store
            .append_event_in_order(&second_event, IdentityScope::Source)
            .map_err(|error| error.to_string())
    });

    let first = first.join().expect("the first writer must not panic");
    let second = second.join().expect("the second writer must not panic");
    let outcomes = [first, second];
    let successful_writes = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    let failed_writes = outcomes.iter().filter(|outcome| outcome.is_err()).count();
    assert!(
        (1..=2).contains(&successful_writes),
        "one or both writers must complete in time: {outcomes:?}"
    );
    assert_eq!(successful_writes + failed_writes, 2);

    let verification_store = SqliteStore::open(&database.path).unwrap();
    let owner = ctx.owner.inner().to_string();
    let day = date!(2026 - 02 - 01).to_string();
    let duplicate_sequences: Vec<u32> = verification_store
        .connection()
        .prepare(
            "SELECT sequence
             FROM events
             WHERE owner = ?1 AND effective_date = ?2
             GROUP BY sequence
             HAVING COUNT(*) > 1",
        )
        .unwrap()
        .query_map([owner.as_str(), day.as_str()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        duplicate_sequences.is_empty(),
        "one sequence number was issued twice: {duplicate_sequences:?}"
    );

    let stored_rows: u32 = verification_store
        .connection()
        .query_row(
            "SELECT COUNT(*)
             FROM events
             WHERE owner = ?1 AND effective_date = ?2",
            [owner.as_str(), day.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored_rows, successful_writes as u32,
        "a failed write must not leave a row in the database"
    );

    let sequences: Vec<u32> = verification_store
        .load_events(ctx.owner)
        .unwrap()
        .into_iter()
        .map(|event| event.order.sequence())
        .collect();
    assert_eq!(
        sequences,
        (1..=successful_writes as u32).collect::<Vec<_>>(),
        "successful writes must occupy the sequence without gaps"
    );
}
