//! Резолвинг инструмента по внешнему коду на дату (E3.1).

use iaam_core::ids::{CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_store::reference::{AliasRecord, AliasRename, CustodyRecord, InstrumentRecord};
use iaam_store::{ResolveError, SqliteStore, StoreError};
use time::macros::date;

fn store_with_one_bond() -> (SqliteStore, InstrumentId) {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "RU000A0JX0J2".to_owned(),
            title: "ОФЗ 26207".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент заведён");
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
fn a_code_resolves_on_the_first_day_of_its_interval() {
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            None,
        ))
        .expect("псевдоним записан");

    let found = store
        .resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", date!(2020 - 01 - 01))
        .expect("резолвинг");

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
        .expect("псевдоним записан");

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
        .expect("псевдоним записан");

    let absent =
        store.resolve_instrument(AliasNamespace::Isin, "RU000A0ZZZZ9", date!(2021 - 06 - 01));
    let out_of_range =
        store.resolve_instrument(AliasNamespace::Isin, "RU000A0JX0J2", date!(2025 - 06 - 01));

    assert!(
        matches!(absent, Err(ResolveError::Unknown { .. })),
        "новая бумага и испорченная дата — разные ответы разбирающемуся"
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
        .expect("исходный псевдоним");

    store
        .rename_alias(&AliasRename {
            namespace: AliasNamespace::Isin,
            from: "RU000AOLD001".to_owned(),
            to: "RU000ANEW002".to_owned(),
            on: date!(2024 - 01 - 01),
            instrument,
            source: SourceId::new_random(),
        })
        .expect("смена кода");

    let before = store
        .resolve_instrument(AliasNamespace::Isin, "RU000AOLD001", date!(2023 - 06 - 01))
        .expect("документ до смены");
    let after = store
        .resolve_instrument(AliasNamespace::Isin, "RU000ANEW002", date!(2024 - 06 - 01))
        .expect("документ после смены");

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
        .expect("исходный псевдоним");
    store
        .rename_alias(&AliasRename {
            namespace: AliasNamespace::Isin,
            from: "RU000AOLD001".to_owned(),
            to: "RU000ANEW002".to_owned(),
            on: date!(2024 - 01 - 01),
            instrument,
            source: SourceId::new_random(),
        })
        .expect("смена кода");

    let anachronism =
        store.resolve_instrument(AliasNamespace::Isin, "RU000ANEW002", date!(2023 - 06 - 01));

    assert!(
        matches!(anachronism, Err(ResolveError::NotOnDate { .. })),
        "новый код в документе, датированном до смены, — признак порчи данных"
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
        store.list_aliases().expect("список алиасов").is_empty(),
        "ошибка переименования не должна заводить новый код"
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
            title: "Чужой выпуск".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("чужой инструмент заведён");
    store
        .record_alias(&alias(
            instrument,
            "RU000AOLD001",
            date!(2020 - 01 - 01),
            None,
        ))
        .expect("исходный псевдоним");

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
    let aliases = store.list_aliases().expect("список алиасов");
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].instrument, instrument);
    assert_eq!(aliases[0].value, "RU000AOLD001");
    assert_eq!(aliases[0].interval.valid_to, None);
}

#[test]
fn a_custody_place_of_another_owner_is_not_overwritten() {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let place = CustodyId::new_random();
    let mine = OwnerId::new_random();
    let theirs = OwnerId::new_random();

    store
        .upsert_custody_place(&CustodyRecord {
            id: place,
            owner: mine,
            title: "Депозитарий А".to_owned(),
            institution: None,
        })
        .expect("моё место хранения");

    store
        .upsert_custody_place(&CustodyRecord {
            id: place,
            owner: theirs,
            title: "Захвачено".to_owned(),
            institution: None,
        })
        .expect("запрос чужого владельца выполняется, но ничего не меняет");

    let places = store.list_custody_places(mine).expect("список");
    assert_eq!(places[0].title, "Депозитарий А");
}

#[test]
fn a_code_never_resolves_to_two_instruments() {
    // При непересекающихся интервалах резолвинг на явных датах,
    // включая обе границы и стык, даёт не более одного кандидата.
    let (store, instrument) = store_with_one_bond();
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2020 - 01 - 01),
            Some(date!(2024 - 01 - 01)),
        ))
        .expect("первый интервал");
    store
        .record_alias(&alias(
            instrument,
            "RU000A0JX0J2",
            date!(2024 - 01 - 01),
            None,
        ))
        .expect("смежный интервал");

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
