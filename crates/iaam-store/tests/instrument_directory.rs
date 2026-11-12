//! Resolving an instrument by external code on a given date (E3.1).

use iaam_core::custody::CustodyOrigin;
use iaam_core::ids::{CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_store::reference::{AliasRecord, AliasRename, CustodyRecord, InstrumentRecord};
use iaam_store::{ResolveError, SqliteStore, StoreError};
use time::macros::date;

fn store_with_one_bond() -> (SqliteStore, InstrumentId) {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "RU000A0JX0J2".to_owned(),
            title: "OFZ 26207".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
            metadata: None,
            blocked_tca_flag: false,
        })
        .expect("instrument created");
    (store, instrument)
}

fn alias(
    instrument: InstrumentId,
    value: &str,
    from: time::Date,
    to: Option<time::Date>,
) -> AliasRecord {
    AliasRecord {
        namespace: AliasNamespace::Isin,
        value: value.to_owned(),
        instrument,
        interval: AliasInterval {
            valid_from: from,
            valid_to: to,
        },
        source: SourceId::new_random(),
    }
}

#[test]
fn broker_block_refresh_is_readable_and_owner_edits_preserve_it() {
    let (store, id) = store_with_one_bond();
    assert!(!store.instrument(id).unwrap().unwrap().blocked_tca_flag);
    store
        .update_blocked_tca_flag(id, "tinkoff:uid:invented", true)
        .unwrap();
    let mut renamed = store.instrument(id).unwrap().unwrap();
    assert!(renamed.blocked_tca_flag);
    renamed.title = "Owner title".into();
    renamed.blocked_tca_flag = false;
    store.upsert_instrument(&renamed).unwrap();
    let loaded = store.instrument(id).unwrap().unwrap();
    assert_eq!(loaded.title, "Owner title");
    assert!(loaded.blocked_tca_flag);
    assert!(store.list_instruments().unwrap()[0].blocked_tca_flag);
    store
        .update_blocked_tca_flag(id, "tinkoff:uid:invented", false)
        .unwrap();
    assert!(!store.instrument(id).unwrap().unwrap().blocked_tca_flag);
    assert!(matches!(
        store.update_blocked_tca_flag(InstrumentId::new_random(), "tinkoff:uid:invented", true),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn broker_blocks_are_aggregated_per_handle_in_either_read_order() {
    for blocked_first in [true, false] {
        let (store, id) = store_with_one_bond();
        let reads = if blocked_first {
            [
                ("tinkoff:uid:blocked", true),
                ("tinkoff:uid:ordinary", false),
            ]
        } else {
            [
                ("tinkoff:uid:ordinary", false),
                ("tinkoff:uid:blocked", true),
            ]
        };
        for (handle, blocked) in reads {
            store.update_blocked_tca_flag(id, handle, blocked).unwrap();
        }
        assert!(store.instrument(id).unwrap().unwrap().blocked_tca_flag);
        assert!(store.list_instruments().unwrap()[0].blocked_tca_flag);
        store
            .update_blocked_tca_flag(id, "tinkoff:uid:ordinary", false)
            .unwrap();
        assert!(store.instrument(id).unwrap().unwrap().blocked_tca_flag);
        store
            .update_blocked_tca_flag(id, "tinkoff:uid:ordinary", true)
            .unwrap();
        store
            .update_blocked_tca_flag(id, "tinkoff:uid:blocked", false)
            .unwrap();
        assert!(store.instrument(id).unwrap().unwrap().blocked_tca_flag);
        store
            .update_blocked_tca_flag(id, "tinkoff:uid:ordinary", false)
            .unwrap();
        assert!(!store.instrument(id).unwrap().unwrap().blocked_tca_flag);
    }
}

#[test]
fn a_failed_broker_block_refresh_rolls_back_its_handle_evidence() {
    let (store, id) = store_with_one_bond();
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER refuse_block_refresh BEFORE UPDATE OF blocked_tca_flag ON instruments
         BEGIN SELECT RAISE(ABORT, 'invented refresh failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .update_blocked_tca_flag(id, "tinkoff:uid:test", true)
            .is_err()
    );
    let count: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM instrument_broker_blocks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
    assert!(!store.instrument(id).unwrap().unwrap().blocked_tca_flag);
}

#[test]
fn a_code_resolves_on_the_first_day_of_its_interval() {
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            None,
        ))
        .expect("alias recorded");

    let found = store
        .resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", date!(2020 - 01 - 01))
        .expect("resolution");

    assert_eq!(found, instrument);
}

#[test]
fn a_code_does_not_resolve_on_the_day_its_interval_ends() {
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            Some(date!(2024 - 01 - 01)),
        ))
        .expect("alias recorded");

    let refused =
        store.resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", date!(2024 - 01 - 01));

    assert!(matches!(refused, Err(ResolveError::NotOnDate { .. })));
}

#[test]
fn an_absent_code_is_told_apart_from_a_code_outside_its_interval() {
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            Some(date!(2024 - 01 - 01)),
        ))
        .expect("alias recorded");

    let absent =
        store.resolve_instrument(AliasNamespace::Isin, "RU000A0ZZZZ9", date!(2021 - 06 - 01));
    let out_of_range =
        store.resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", date!(2025 - 06 - 01));

    assert!(
        matches!(absent, Err(ResolveError::Unknown { .. })),
        "a new security and a corrupted date must produce different answers for an informed reader"
    );
    assert!(matches!(out_of_range, Err(ResolveError::NotOnDate { .. })));
}

#[test]
fn a_renamed_code_resolves_from_both_sides_of_the_change() {
    let (mut store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000AOLD001",
            date!(2020 - 01 - 01),
            None,
        ))
        .expect("original alias");

    store
        .rename_alias(&AliasRename {
            namespace: AliasNamespace::Isin,
            from: "RU000AOLD001".to_owned(),
            to: "RU000ANEW002".to_owned(),
            on: date!(2024 - 01 - 01),
            instrument,
            source: SourceId::new_random(),
        })
        .expect("code change");

    let before = store
        .resolve_instrument(AliasNamespace::Isin, "RU000AOLD001", date!(2023 - 06 - 01))
        .expect("document before the change");
    let after = store
        .resolve_instrument(AliasNamespace::Isin, "RU000ANEW002", date!(2024 - 06 - 01))
        .expect("document after the change");

    assert_eq!(before, instrument);
    assert_eq!(after, instrument);
}

#[test]
fn the_new_code_does_not_resolve_before_the_change() {
    let (mut store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000AOLD001",
            date!(2020 - 01 - 01),
            None,
        ))
        .expect("original alias");
    store
        .rename_alias(&AliasRename {
            namespace: AliasNamespace::Isin,
            from: "RU000AOLD001".to_owned(),
            to: "RU000ANEW002".to_owned(),
            on: date!(2024 - 01 - 01),
            instrument,
            source: SourceId::new_random(),
        })
        .expect("code change");

    let anachronism =
        store.resolve_instrument(AliasNamespace::Isin, "RU000ANEW002", date!(2023 - 06 - 01));

    assert!(
        matches!(anachronism, Err(ResolveError::NotOnDate { .. })),
        "a new code in a document dated before the change indicates corrupted data"
    );
}

#[test]
fn renaming_a_missing_code_is_rejected_without_creating_the_new_code() {
    let (mut store, instrument) = store_with_one_bond();

    let refused = store.rename_alias(&AliasRename {
        namespace: AliasNamespace::Isin,
        from: "RU000AOLD001".to_owned(),
        to: "RU000ANEW002".to_owned(),
        on: date!(2024 - 01 - 01),
        instrument,
        source: SourceId::new_random(),
    });

    assert!(matches!(
        refused,
        Err(StoreError::AliasNotFoundForInstrument { .. })
    ));
    assert!(
        store.list_aliases().expect("alias list").is_empty(),
        "a rename error must not register a new code"
    );
}

#[test]
fn renaming_with_a_foreign_instrument_is_rejected_without_closing_the_old_code() {
    let (mut store, instrument) = store_with_one_bond();
    let foreign = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: foreign,
            kind: Some(InstrumentKind::Bond),
            symbol: "RU000FOREIGN".to_owned(),
            title: "Foreign issue".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
            metadata: None,
            blocked_tca_flag: false,
        })
        .expect("foreign instrument created");
    store
        .record_alias(&alias(
            instrument,
            "RU000AOLD001",
            date!(2020 - 01 - 01),
            None,
        ))
        .expect("original alias");

    let refused = store.rename_alias(&AliasRename {
        namespace: AliasNamespace::Isin,
        from: "RU000AOLD001".to_owned(),
        to: "RU000ANEW002".to_owned(),
        on: date!(2024 - 01 - 01),
        instrument: foreign,
        source: SourceId::new_random(),
    });

    assert!(matches!(
        refused,
        Err(StoreError::AliasNotFoundForInstrument { .. })
    ));
    let aliases = store.list_aliases().expect("alias list");
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].instrument, instrument);
    assert_eq!(aliases[0].value, "RU000AOLD001");
    assert_eq!(aliases[0].interval.valid_to, None);
}

#[test]
fn a_custody_place_of_another_owner_is_not_overwritten() {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    let place = CustodyId::new_random();
    let mine = OwnerId::new_random();
    let theirs = OwnerId::new_random();

    store
        .upsert_custody_place(&CustodyRecord {
            id: place,
            owner: mine,
            title: "Custody A".to_owned(),
            institution: None,
            origin: CustodyOrigin::Declared,
        })
        .expect("my custody place");

    store
        .upsert_custody_place(&CustodyRecord {
            id: place,
            owner: theirs,
            title: "Captured".to_owned(),
            institution: None,
            origin: CustodyOrigin::Declared,
        })
        .expect("foreign owner's request succeeds but changes nothing");

    let places = store.list_custody_places(mine).expect("list");
    assert_eq!(places[0].title, "Custody A");
}

#[test]
fn a_code_never_resolves_to_two_instruments() {
    // For non-overlapping intervals, resolving on explicit dates,
    // including both boundaries and the junction, yields at most one candidate.
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            Some(date!(2024 - 01 - 01)),
        ))
        .expect("first interval");
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2024 - 01 - 01),
            None,
        ))
        .expect("adjacent interval");

    for on in [
        date!(2019 - 12 - 31),
        date!(2020 - 01 - 01),
        date!(2022 - 01 - 01),
        date!(2024 - 01 - 01),
        date!(2025 - 06 - 01),
        date!(2099 - 12 - 31),
    ] {
        let resolved = store.resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", on);
        assert!(!matches!(resolved, Err(ResolveError::Ambiguous { .. })));
        if let Ok(found) = resolved {
            assert_eq!(found, instrument);
        }
    }
}

#[test]
fn instrument_returns_the_stored_record_and_none_for_an_unknown_id() {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    let expected = InstrumentRecord {
        id: InstrumentId::new_random(),
        kind: Some(InstrumentKind::Bond),
        symbol: "RU000ATEST01".to_owned(),
        title: "Test issue".to_owned(),
        currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
        lineage: None,
        metadata: None,
        blocked_tca_flag: false,
    };
    store
        .upsert_instrument(&expected)
        .expect("instrument created");

    assert_eq!(
        store.instrument(expected.id).expect("read instrument"),
        Some(expected.clone())
    );
    assert_eq!(
        store
            .instrument(InstrumentId::new_random())
            .expect("read unknown instrument"),
        None
    );
}

#[test]
fn list_instruments_returns_all_stored_records_in_symbol_order() {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    let later = InstrumentRecord {
        id: InstrumentId::new_random(),
        kind: Some(InstrumentKind::Bond),
        symbol: "ZZZ".to_owned(),
        title: "Later issue".to_owned(),
        currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
        lineage: None,
        metadata: None,
        blocked_tca_flag: false,
    };
    let earlier = InstrumentRecord {
        id: InstrumentId::new_random(),
        kind: Some(InstrumentKind::Bond),
        symbol: "AAA".to_owned(),
        title: "Earlier issue".to_owned(),
        currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
        lineage: None,
        metadata: None,
        blocked_tca_flag: false,
    };
    store
        .upsert_instrument(&later)
        .expect("first instrument created");
    store
        .upsert_instrument(&earlier)
        .expect("second instrument created");

    assert_eq!(
        store.list_instruments().expect("instrument list"),
        vec![earlier, later]
    );
}

#[test]
fn isin_registration_resolves_existing_and_rolls_back_a_failed_alias() {
    let (mut store, id) = store_with_one_bond();
    let instrument = store.instrument(id).expect("read").expect("instrument");
    let source = SourceId::new_random();
    let on = date!(2026 - 01 - 01);
    assert_eq!(
        store
            .register_instrument_with_isin(&instrument, "TEST00000001", source, on)
            .expect("register"),
        id
    );
    let mut candidate = instrument.clone();
    candidate.id = InstrumentId::new_random();
    candidate.title = "Must not replace existing metadata".to_owned();
    assert_eq!(
        store
            .register_instrument_with_isin(&candidate, "TEST00000001", source, on)
            .expect("resolve"),
        id
    );
    assert!(store.instrument(candidate.id).expect("read").is_none());
    assert_eq!(store.instrument(id).expect("read"), Some(instrument));
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER refuse_test_alias BEFORE INSERT ON instrument_aliases
         BEGIN SELECT RAISE(ABORT, 'invented alias failure'); END;",
        )
        .expect("invented failure");
    assert!(
        store
            .register_instrument_with_isin(&candidate, "TEST00000002", source, on)
            .is_err()
    );
    assert!(store.instrument(candidate.id).expect("read").is_none());
}

#[test]
fn broker_metadata_round_trips_and_absent_metadata_preserves_it() {
    let (store, id) = store_with_one_bond();
    let mut instrument = store.instrument(id).expect("read").expect("instrument");
    let metadata = iaam_core::instrument::InstrumentMetadata {
        mic: Some("XTEST".to_owned()),
        board: Some("TEST".to_owned()),
        lot: Some(10),
        price_decimals: Some(4),
    };
    instrument.metadata = Some(metadata.clone());
    store
        .upsert_instrument(&instrument)
        .expect("metadata recorded");
    assert_eq!(
        store.instrument(id).expect("read"),
        Some(instrument.clone())
    );
    assert_eq!(
        store.list_instruments().expect("list"),
        vec![instrument.clone()]
    );
    instrument.metadata = None;
    store
        .upsert_instrument(&instrument)
        .expect("ordinary update");
    assert_eq!(
        store
            .instrument(id)
            .expect("read")
            .expect("instrument")
            .metadata,
        Some(metadata)
    );
}

#[test]
fn a_custody_place_keeps_the_origin_it_was_written_with() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let declared = CustodyId::new_random();
    let minted = CustodyId::new_random();

    store
        .upsert_custody_place(&CustodyRecord {
            id: declared,
            owner,
            title: "Broker One".to_owned(),
            institution: Some("Broker One".to_owned()),
            origin: CustodyOrigin::Declared,
        })
        .expect("declared place");
    store
        .upsert_custody_place(&CustodyRecord {
            id: minted,
            owner,
            title: "broker-channel handle".to_owned(),
            institution: None,
            origin: CustodyOrigin::Minted,
        })
        .expect("minted place");

    let places = store.list_custody_places(owner).expect("list");
    let origins: Vec<(CustodyId, CustodyOrigin)> = places
        .iter()
        .map(|place| (place.id, place.origin))
        .collect();
    assert!(origins.contains(&(declared, CustodyOrigin::Declared)));
    assert!(origins.contains(&(minted, CustodyOrigin::Minted)));
}

#[test]
fn two_declared_places_of_one_owner_cannot_share_a_title() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();

    let declared = |id| CustodyRecord {
        id,
        owner,
        title: "Broker One".to_owned(),
        institution: Some("Broker One".to_owned()),
        origin: CustodyOrigin::Declared,
    };
    store
        .upsert_custody_place(&declared(CustodyId::new_random()))
        .expect("first declared place");
    assert!(
        store
            .upsert_custody_place(&declared(CustodyId::new_random()))
            .is_err(),
        "a second declared place under one title is a state no lookup can interpret"
    );

    // A minted handle carries a generated title and many of them may repeat it:
    // nothing resolves them by title, so nothing is made ambiguous.
    let minted = |id| CustodyRecord {
        id,
        owner,
        title: "broker-channel handle".to_owned(),
        institution: None,
        origin: CustodyOrigin::Minted,
    };
    store
        .upsert_custody_place(&minted(CustodyId::new_random()))
        .expect("first handle");
    store
        .upsert_custody_place(&minted(CustodyId::new_random()))
        .expect("second handle");
}
