//! Append-only journal and idempotency.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, ImportSessionId, OwnerId, SourceId, TransferId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::reconciliation::evidence::IdentityScope;
use iaam_store::SqliteStore;
use iaam_store::events::{AccountActivityRecord, Appended, JournalQuery};
use iaam_store::reference::AccountRecord;
use std::collections::BTreeSet;
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

fn insert_account(store: &SqliteStore, ctx: &Ctx) {
    store
        .upsert_account(&AccountRecord {
            id: ctx.account,
            owner: ctx.owner,
            title: "Main".into(),
            institution: Some("Savings".into()),
        })
        .unwrap();
}

fn bookkeeping_event(ctx: &Ctx, sequence: u32, kind: EventKind) -> Event {
    let mut event = ctx.deposit(sequence, 100_000);
    event.kind = kind;
    event.dates = EventDates::empty();
    event.legs.clear();
    event
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

/// A page of the journal can be narrowed to the one import session that wrote
/// it.
///
/// The session travels inside the payload as part of the event's provenance,
/// and a filter applied after the page was selected would return short pages
/// and a cursor that skips rows — so the value is lifted into a column and
/// bound here. What is being checked is that the column agrees with the
/// provenance: an event stamped with a session is found by it, and an event
/// stamped with none is not swept in beside it.
#[test]
fn the_journal_narrows_to_the_import_session_that_wrote_it() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    let session = ImportSessionId::new_random();

    let committed = {
        let mut event = ctx.deposit(1, 100_000);
        event.provenance = event.provenance.with_import_session(session);
        event
    };
    let free = ctx.deposit(2, 200_000);
    for event in [&committed, &free] {
        store.append_event(event, IdentityScope::Source).unwrap();
    }

    let narrowed = store
        .list_journal_events(
            ctx.owner,
            &JournalQuery {
                import_session: Some(session),
                limit: 10,
                ..JournalQuery::default()
            },
        )
        .unwrap();
    assert_eq!(
        narrowed.iter().map(|event| event.id).collect::<Vec<_>>(),
        vec![committed.id],
        "only the rows that session committed"
    );
    assert_eq!(
        narrowed[0].provenance.import_session(),
        Some(session),
        "the column and the provenance name one session"
    );

    let another = store
        .list_journal_events(
            ctx.owner,
            &JournalQuery {
                import_session: Some(ImportSessionId::new_random()),
                limit: 10,
                ..JournalQuery::default()
            },
        )
        .unwrap();
    assert!(
        another.is_empty(),
        "a session that wrote nothing here matches nothing: {another:?}"
    );

    let everything = store
        .list_journal_events(
            ctx.owner,
            &JournalQuery {
                limit: 10,
                ..JournalQuery::default()
            },
        )
        .unwrap();
    assert_eq!(everything.len(), 2, "the unnarrowed page still holds both");
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
    let recorded = store.load_events(ctx.owner).unwrap();
    assert_eq!(recorded.len(), 1);
    // The key names the fact, and the amount is never compared: the second
    // event is a *corrected* row under a key already used, and what survives is
    // the first number rather than the right one. This is why re-sending a row
    // does not fix it — a correction does, and nothing on this path writes one.
    assert_eq!(
        recorded[0].cash_effect(CurrencyCode::Rub).unwrap().amount(),
        PostedMinor::new(100_000),
        "the journal keeps the value it already held, not the corrected one"
    );
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

#[test]
fn account_activity_keeps_an_empty_owned_account() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    insert_account(&store, &ctx);

    let activity = store.list_account_activity(ctx.owner).unwrap();
    assert_eq!(
        activity,
        vec![AccountActivityRecord {
            account: ctx.account,
            has_business_fact: false,
            first_effective_date: None,
            last_effective_date: None,
        }]
    );
    assert!(
        store
            .list_control_assertions(ctx.owner, ctx.account)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn account_activity_excludes_both_bookkeeping_kinds() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    insert_account(&store, &ctx);
    let period = AssertionPeriod::between(date!(2026 - 02 - 01), date!(2026 - 02 - 28)).unwrap();
    store
        .append_event(
            &bookkeeping_event(
                &ctx,
                1,
                EventKind::ControlAssertion {
                    period,
                    claim: ControlClaim::CashBalance {
                        currency: CurrencyCode::Rub,
                        amount: PostedMinor::new(100_000),
                        at: BalancePoint::Closing,
                    },
                },
            ),
            IdentityScope::Source,
        )
        .unwrap();
    let mut dimensions = BTreeSet::new();
    dimensions.insert(Dimension::Cash);
    store
        .append_event(
            &bookkeeping_event(
                &ctx,
                2,
                EventKind::ImportCoverageGap {
                    period,
                    dimensions,
                    refused: 1,
                    rows: Vec::new(),
                },
            ),
            IdentityScope::Source,
        )
        .unwrap();

    let activity = store.list_account_activity(ctx.owner).unwrap();
    assert!(!activity[0].has_business_fact);
    assert_eq!(activity[0].first_effective_date, None);
    let assertions = store
        .list_control_assertions(ctx.owner, ctx.account)
        .unwrap();
    assert_eq!(assertions.len(), 1);
    assert_eq!(assertions[0].period, period);
    assert_eq!(assertions[0].point, Some(BalancePoint::Closing));
    assert_eq!(assertions[0].dimension, Dimension::Cash);
}

#[test]
fn account_activity_reports_bounds_for_business_facts() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    insert_account(&store, &ctx);
    let first = ctx.deposit(1, 100_000);
    let mut last = ctx.deposit(2, 200_000);
    last.order = EffectiveOrder::new(date!(2026 - 03 - 15), 2);
    last.dates = EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 15)));
    store.append_event(&first, IdentityScope::Source).unwrap();
    store.append_event(&last, IdentityScope::Source).unwrap();

    let activity = store.list_account_activity(ctx.owner).unwrap();
    assert_eq!(activity.len(), 1);
    assert!(activity[0].has_business_fact);
    assert_eq!(
        activity[0].first_effective_date,
        Some(date!(2026 - 02 - 01))
    );
    assert_eq!(activity[0].last_effective_date, Some(date!(2026 - 03 - 15)));
}

/// A transfer counts for the account it arrives at, not only for the one it is
/// recorded against (iaam-8axt).
///
/// `events.account` names one of the two accounts a transfer moves money
/// between. An account whose whole content arrived that way — savings fed from
/// a current account, a deposit opened by moving money across — looked empty:
/// the queue never asked it for a balance and offered it an import instead,
/// while it held money at the end of the month.
#[test]
fn account_activity_counts_both_accounts_a_transfer_touched() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    insert_account(&store, &ctx);
    let savings = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: savings,
            owner: ctx.owner,
            title: "Savings".into(),
            institution: Some("Northline".into()),
        })
        .unwrap();

    // One event, recorded against `Main`, moving money to `Savings`. Nothing at
    // all is recorded against `Savings`.
    let amount = Money::new(PostedMinor::new(500_000), CurrencyCode::Rub);
    let day = date!(2026 - 02 - 10);
    let mut transfer = ctx.deposit(1, 500_000);
    transfer.kind = EventKind::CashTransfer {
        transfer_id: TransferId::new_random(),
        from: ctx.account,
        to: savings,
        amount,
    };
    transfer.order = EffectiveOrder::new(day, 1);
    transfer.dates = EventDates::for_cash(CashPostedDate(day));
    store
        .append_event(&transfer, IdentityScope::Source)
        .unwrap();

    let activity = store.list_account_activity(ctx.owner).unwrap();
    let arrived = activity
        .iter()
        .find(|record| record.account == savings)
        .expect("the receiving account is still listed");
    assert!(
        arrived.has_business_fact,
        "money reached this account and nothing said so: {arrived:?}"
    );
    // The bounds are data-coverage bounds, and the day money arrived is a day
    // this account is covered on.
    assert_eq!(arrived.first_effective_date, Some(day));
    assert_eq!(arrived.last_effective_date, Some(day));

    // The sending side is unchanged: it was already counted by the account the
    // event is recorded against, and counting it twice must not widen anything.
    let sent = activity
        .iter()
        .find(|record| record.account == ctx.account)
        .expect("the sending account");
    assert!(sent.has_business_fact);
    assert_eq!(sent.first_effective_date, Some(day));
    assert_eq!(sent.last_effective_date, Some(day));
}

/// A transfer widens the receiving account's bounds without narrowing them.
#[test]
fn a_transfer_widens_the_coverage_it_reaches_beyond() {
    let store = SqliteStore::open_in_memory().unwrap();
    let ctx = Ctx::new();
    insert_account(&store, &ctx);
    let savings = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: savings,
            owner: ctx.owner,
            title: "Savings".into(),
            institution: Some("Northline".into()),
        })
        .unwrap();

    // `Savings` has one fact of its own on the first of the month...
    let mut own = ctx.deposit(1, 100_000);
    own.account = savings;
    own.legs = vec![Leg::cash(
        savings,
        Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
    )];
    store.append_event(&own, IdentityScope::Source).unwrap();

    // ...and money reaches it from `Main` a fortnight later.
    let amount = Money::new(PostedMinor::new(500_000), CurrencyCode::Rub);
    let day = date!(2026 - 02 - 15);
    let mut transfer = ctx.deposit(2, 500_000);
    transfer.kind = EventKind::CashTransfer {
        transfer_id: TransferId::new_random(),
        from: ctx.account,
        to: savings,
        amount,
    };
    transfer.order = EffectiveOrder::new(day, 1);
    transfer.dates = EventDates::for_cash(CashPostedDate(day));
    store
        .append_event(&transfer, IdentityScope::Source)
        .unwrap();

    let activity = store.list_account_activity(ctx.owner).unwrap();
    let arrived = activity
        .iter()
        .find(|record| record.account == savings)
        .expect("the receiving account");
    assert_eq!(arrived.first_effective_date, Some(date!(2026 - 02 - 01)));
    assert_eq!(arrived.last_effective_date, Some(day));
}
