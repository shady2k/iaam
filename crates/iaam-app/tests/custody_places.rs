//! A place of custody can be created through the ports it is read through.

use std::sync::Arc;

use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{CustodyUpsert, InstrumentDirectory};
use iaam_core::custody::CustodyOrigin;
use iaam_core::ids::{CustodyId, OwnerId};
use iaam_store::SqliteStore;

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
