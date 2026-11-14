//! ADR 0042: a write is one transaction after the network.
//!
//! Holds `Store::append_events` and `Store::transact` to what the decision
//! promises: a failed insert leaves nothing of the batch, a panic leaves
//! nothing and the store stays usable, and a read inside the work sees what
//! the work already wrote.

use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Store, StoreTx, TransactionalWork};
use iaam_app::storage::SqliteStore;
use iaam_core::dates::{EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{AccountId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_ingest::dedup::IdentityScope;
use time::macros::date;

async fn adapter_with_account() -> (SqliteAdapter, OwnerId, AccountId) {
    let adapter = SqliteAdapter::new(SqliteStore::open_in_memory().expect("in-memory database"));
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    adapter
        .upsert_account(
            owner,
            AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Invented Bank".to_owned()),
            },
        )
        .await
        .expect("seed account");
    (adapter, owner, account)
}

fn assertion(owner: OwnerId, account: AccountId, sequence: u32, claim: ControlClaim) -> Event {
    Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::ControlAssertion {
            period: AssertionPeriod::between(date!(2026 - 01 - 01), date!(2026 - 01 - 31))
                .expect("valid period"),
            claim,
        },
        dates: EventDates::empty(),
        order: EffectiveOrder::new(date!(2026 - 01 - 31), sequence),
        legs: vec![],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"a".repeat(64)).expect("invented hash"),
            ParserVersion("transact-test/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn cash_balance() -> ControlClaim {
    ControlClaim::CashBalance {
        currency: CurrencyCode::Rub,
        amount: PostedMinor::new(987_654_321),
        at: BalancePoint::Closing,
    }
}

fn missing_instrument_position() -> ControlClaim {
    ControlClaim::PositionQuantity {
        instrument: InstrumentId::new_random(),
        quantity: Quantity(Dec::one()),
        at: BalancePoint::Closing,
    }
}

#[tokio::test]
async fn a_batch_whose_second_insert_fails_leaves_nothing_of_the_batch() {
    let (adapter, owner, account) = adapter_with_account().await;
    let first = assertion(owner, account, 1, cash_balance());
    let failing = assertion(owner, account, 2, missing_instrument_position());

    adapter
        .append_events(vec![first, failing], IdentityScope::Source)
        .await
        .expect_err("the missing instrument must fail the second insert");

    let journal = adapter
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("read the journal back");
    assert!(
        journal.is_empty(),
        "a failed batch must leave nothing behind, found {} event(s)",
        journal.len()
    );
}

#[tokio::test]
async fn a_read_inside_the_work_sees_the_event_the_work_appended() {
    let (adapter, owner, account) = adapter_with_account().await;
    let event = assertion(owner, account, 1, cash_balance());
    let id = event.id;

    let work: TransactionalWork = Box::new(move |tx: &mut dyn StoreTx| {
        let appended = tx.append_events_in_order(vec![event], IdentityScope::Source)?;
        let seen = tx.load_events_through(owner, time::Date::MAX)?;
        Ok(Box::new((appended, seen)))
    });
    let result = adapter.transact(work).await.expect("the work commits");
    let (appended, seen) = *result
        .downcast::<(Vec<iaam_app::ports::Recorded>, Vec<Event>)>()
        .expect("the port returns what the work returned");

    assert_eq!(appended, vec![iaam_app::ports::Recorded::Inserted { id }]);
    assert_eq!(seen.len(), 1, "the read sees the event the work wrote");
    assert_eq!(seen[0].id, id);
    assert_eq!(
        seen[0].order.sequence(),
        1,
        "the sequence is assigned inside the same transaction"
    );
}

#[tokio::test]
async fn a_panic_inside_the_work_leaves_nothing_and_the_store_stays_usable() {
    let (adapter, owner, account) = adapter_with_account().await;
    let event = assertion(owner, account, 1, cash_balance());

    let work: TransactionalWork = Box::new(move |tx: &mut dyn StoreTx| {
        tx.append_events_in_order(vec![event], IdentityScope::Source)?;
        panic!("an interruption in the middle of the work");
    });
    let error = adapter
        .transact(work)
        .await
        .expect_err("a panic inside the work must not report success");
    assert!(
        matches!(error, iaam_app::error::AppError::Store(_)),
        "a failed blocking task is a store error, got {error:?}"
    );

    let journal = adapter
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("read the journal back");
    assert!(
        journal.is_empty(),
        "a panic must roll the transaction back, found {} event(s)",
        journal.len()
    );

    let later = assertion(owner, account, 1, cash_balance());
    adapter
        .append_events(vec![later], IdentityScope::Source)
        .await
        .expect("the store is usable after the panic");
    assert_eq!(
        adapter
            .load_events_through(owner, time::Date::MAX)
            .await
            .expect("read the journal back")
            .len(),
        1
    );
}
