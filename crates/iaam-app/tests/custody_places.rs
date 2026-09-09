//! A place of custody can be created through the ports it is read through.

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{Clock, CustodyUpsert, InstrumentDirectory, Principal, Scope};
use iaam_app::scenarios::ingest::submit_journal_events;
use iaam_core::custody::CustodyOrigin;
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId};
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_ingest::{JournalFact, SubmittedJournalEvent};
use iaam_store::SqliteStore;
use iaam_store::reference::{AccountRecord, InstrumentRecord};
use time::Date;
use time::macros::date;

struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

#[tokio::test]
async fn a_place_created_through_the_port_is_listed_by_it() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let directory = Arc::new(SqliteAdapter::new(store));
    let owner = OwnerId::new_random();
    let id = CustodyId::new_random();

    directory
        .record_custody_place(
            owner,
            CustodyUpsert {
                id,
                title: "Broker One".to_owned(),
                institution: Some("Broker One".to_owned()),
                origin: CustodyOrigin::Declared,
            },
        )
        .await
        .expect("record");

    let places = directory.list_custody_places(owner).await.expect("list");
    assert_eq!(places.len(), 1);
    assert_eq!(places[0].id, id);
    assert_eq!(places[0].origin, CustodyOrigin::Declared);
}

/// `POST /v1/ingest/journal-events` shares `append_checked` with the broker
/// sync, and the sync registers the handle it mints before it appends
/// (`crates/iaam-app/src/scenarios/sync.rs`). This route deliberately does
/// not: a corporate action or offer exercise submitted here carries a
/// custody the caller supplies, and registering it on the way in would let
/// an agent bring a place of custody into being by naming one (§4,
/// iaam-klpv). The route must go on refusing it.
#[tokio::test]
async fn journal_events_still_refuse_an_event_naming_a_custody_the_owner_does_not_hold() {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let unregistered_custody = CustodyId::new_random();
    let store = SqliteStore::open_in_memory().expect("memory store");
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Main".to_owned(),
            institution: Some("Broker One".to_owned()),
        })
        .expect("seed account");
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: None,
            symbol: "TESTSHARE".to_owned(),
            title: "TESTSHARE".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("seed instrument");
    let adapter = Arc::new(SqliteAdapter::new(store));
    let services = AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        Arc::new(FixedClock(date!(2026 - 04 - 20))),
    );
    let principal = Principal {
        token_id: uuid::Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    };
    let event = SubmittedJournalEvent {
        account,
        fact: JournalFact::OfferExercise {
            action: OfferExerciseAction::Settled {
                submission: OfferSubmissionId::new_random(),
                instrument,
                custody: unregistered_custody,
                quantity: Quantity(Dec::one()),
                gross: Money::new(PostedMinor::new(500_000), CurrencyCode::Rub),
                fee: None,
                accrued_interest: None,
            },
            day: date!(2026 - 04 - 20),
        },
        idempotency_key: None,
        source_operation_id: None,
    };

    let error = submit_journal_events(
        &services,
        &principal,
        SourceId::new_random(),
        None,
        &[event],
    )
    .await
    .expect_err("an event naming an unregistered custody place must be refused");

    assert!(
        error.to_string().contains("custody place"),
        "refusal must name the missing custody place: {error}"
    );
    assert!(
        services
            .directory
            .list_custody_places(owner)
            .await
            .expect("list custody places")
            .is_empty(),
        "a refused submission must not bring a custody place into being"
    );
}
