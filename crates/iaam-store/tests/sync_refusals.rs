//! A sync refusal record round-trips owner-scoped, deduplicates per row and
//! settles when a later sync no longer lists it (iaam-vg8te.1.2).

use iaam_core::ids::{AccountId, OwnerId};
use iaam_store::SqliteStore;
use iaam_store::sync_refusals::SyncRefusalRecord;
use uuid::Uuid;

// Seven named fields read better than a struct-with-defaults dance in a test.
#[allow(clippy::too_many_arguments)]
fn refusal(
    owner: OwnerId,
    account: AccountId,
    row_key: &str,
    from: &str,
    to: &str,
    reason: &str,
    payload: &str,
) -> SyncRefusalRecord {
    SyncRefusalRecord {
        id: Uuid::new_v4(),
        owner,
        account,
        source: "finam".to_owned(),
        row_key: row_key.to_owned(),
        range_from: from.to_owned(),
        range_to: to.to_owned(),
        dimensions: "positions".to_owned(),
        reason: reason.to_owned(),
        payload: payload.to_owned(),
        settled: false,
    }
}

#[test]
fn a_sync_refusal_round_trips_and_stays_owner_scoped() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let other = OwnerId::new_random();
    let account = AccountId::new_random();

    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-1",
            "2026-01-01",
            "2026-01-31",
            "no instrument carries ISIN RU000AFIXTUR",
            r#"{"symbol":"FIXT@MISX"}"#,
        ))
        .expect("record refuses row-1");
    store
        .upsert_sync_refusal(&refusal(
            other,
            account,
            "row-1",
            "2026-01-01",
            "2026-01-31",
            "someone else's row",
            r#"{"symbol":"OTHER@MISX"}"#,
        ))
        .expect("record refuses the other owner's row-1");

    let listed = store
        .list_open_sync_refusals(owner, account, "finam", "2026-01-01", "2026-01-31")
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].row_key, "row-1");
    assert_eq!(listed[0].reason, "no instrument carries ISIN RU000AFIXTUR");
    assert_eq!(listed[0].payload, r#"{"symbol":"FIXT@MISX"}"#);

    assert!(
        store
            .list_open_sync_refusals(owner, account, "tinkoff", "2026-01-01", "2026-01-31")
            .expect("list")
            .is_empty(),
        "a different channel's refusals are not another channel's"
    );
}

#[test]
fn a_re_sync_of_the_same_row_updates_and_never_duplicates() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-1",
            "2026-01-01",
            "2026-01-31",
            "old reason",
            r#"{"symbol":"FIXT@MISX"}"#,
        ))
        .expect("first record");
    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-1",
            "2026-01-01",
            "2026-01-31",
            "new reason after the owner recorded a wrong ISIN",
            r#"{"symbol":"FIXT@MISX","quantity":2}"#,
        ))
        .expect("second record");

    let listed = store
        .list_open_sync_refusals(owner, account, "finam", "2026-01-01", "2026-01-31")
        .expect("list");
    assert_eq!(
        listed.len(),
        1,
        "one row, one question, however often re-synced"
    );
    assert_eq!(
        listed[0].reason,
        "new reason after the owner recorded a wrong ISIN"
    );
    assert_eq!(listed[0].payload, r#"{"symbol":"FIXT@MISX","quantity":2}"#);
}

#[test]
fn an_open_refusal_is_listed_only_when_the_interval_overlaps() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-1",
            "2026-03-01",
            "2026-03-31",
            "reasons",
            r#"{}"#,
        ))
        .expect("record");

    assert!(
        store
            .list_open_sync_refusals(owner, account, "finam", "2026-02-01", "2026-02-28")
            .expect("list")
            .is_empty(),
        "an earlier interval is not overlapped"
    );
    assert_eq!(
        store
            .list_open_sync_refusals(owner, account, "finam", "2026-03-15", "2026-04-15")
            .expect("list")
            .len(),
        1,
        "an interval that reaches into the record's range is overlapped"
    );
}

#[test]
fn settling_keeps_the_row_refused_again_and_settles_the_rest_of_the_overlap() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-a",
            "2026-01-01",
            "2026-01-31",
            "a",
            r#"{}"#,
        ))
        .expect("row-a");
    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-b",
            "2026-01-01",
            "2026-01-31",
            "b",
            r#"{}"#,
        ))
        .expect("row-b");
    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-old",
            "2025-11-01",
            "2025-11-30",
            "old",
            r#"{}"#,
        ))
        .expect("row-old");

    // A later sync of the same range refused only row-a again: row-b is
    // settled (the row now imports), row-old lies outside the overlap and
    // stays open, row-a stays open.
    store
        .settle_sync_refusals_besides(
            owner,
            account,
            "finam",
            "2026-01-01",
            "2026-01-31",
            &["row-a".to_owned()],
        )
        .expect("settle");
    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-a",
            "2026-01-01",
            "2026-01-31",
            "a again",
            r#"{}"#,
        ))
        .expect("row-a refreshed");

    let listed = store
        .list_open_sync_refusals(owner, account, "finam", "2025-11-15", "2026-01-31")
        .expect("list");
    let keys: Vec<&str> = listed
        .iter()
        .map(|record| record.row_key.as_str())
        .collect();
    assert_eq!(
        keys,
        vec!["row-a", "row-old"],
        "the refused-again row and the untouched old range stay open"
    );

    let settled = store
        .list_open_sync_refusals(owner, account, "finam", "2026-01-01", "2026-01-31")
        .expect("list");
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].row_key, "row-a");
}
