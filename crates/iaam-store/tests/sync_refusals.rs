//! A sync refusal record round-trips owner-scoped, deduplicates per row and
//! settles when a later sync that covers its whole interval no longer lists
//! it (iaam-vg8te.1.2).

use iaam_core::ids::{AccountId, OwnerId};
use iaam_store::SqliteStore;
use iaam_store::sync_refusals::{SyncRefusalFilter, SyncRefusalRecord};
use uuid::Uuid;

fn refusal(
    owner: OwnerId,
    account: AccountId,
    row_key: &str,
    from: &str,
    to: &str,
    reason: &str,
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
        // The payload is set where a test needs one; the empty object is the
        // honest default for the rest.
        payload: r#"{}"#.to_owned(),
        settled: false,
    }
}

#[test]
fn a_sync_refusal_round_trips_and_stays_owner_scoped() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let other = OwnerId::new_random();
    let account = AccountId::new_random();

    let mut first = refusal(
        owner,
        account,
        "row-1",
        "2026-01-01",
        "2026-01-31",
        "no instrument carries ISIN RU000AFIXTUR",
    );
    first.payload = r#"{"symbol":"FIXT@MISX"}"#.to_owned();
    store
        .upsert_sync_refusal(&first)
        .expect("record refuses row-1");
    store
        .upsert_sync_refusal(&refusal(
            other,
            account,
            "row-1",
            "2026-01-01",
            "2026-01-31",
            "someone else's row",
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
        ))
        .expect("first record");
    let mut refresh = refusal(
        owner,
        account,
        "row-1",
        "2026-01-01",
        "2026-01-31",
        "new reason after the owner recorded a wrong ISIN",
    );
    refresh.payload = r#"{"symbol":"FIXT@MISX","quantity":2}"#.to_owned();
    store.upsert_sync_refusal(&refresh).expect("second record");

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
fn settling_keeps_the_row_refused_again_and_settles_the_rest_of_the_covered_range() {
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
        ))
        .expect("row-old");

    // A later sync of the same range refused only row-a again: row-b is
    // settled (the row now imports), row-old lies outside the covered range
    // and stays open, row-a stays open.
    store
        .settle_sync_refusals_besides(
            &SyncRefusalFilter {
                owner,
                account,
                source: "finam".to_owned(),
                from: "2026-01-01".to_owned(),
                to: "2026-01-31".to_owned(),
            },
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

#[test]
fn a_later_sync_that_does_not_cover_the_records_interval_settles_nothing() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    // A March 1–31 sync refused one row. A later sync of March 31 only
    // refuses nothing: its silence about the row proves nothing about March
    // 1–30, so the record settles only when the later sync covers its own
    // interval (iaam-vg8te.1.2).
    store
        .upsert_sync_refusal(&refusal(
            owner,
            account,
            "row-1",
            "2026-03-01",
            "2026-03-31",
            "reasons",
        ))
        .expect("record");

    // March 31 only: neither an empty refusal list nor a list of other keys
    // may settle a record the sync did not read whole.
    store
        .settle_sync_refusals_besides(
            &SyncRefusalFilter {
                owner,
                account,
                source: "finam".to_owned(),
                from: "2026-03-31".to_owned(),
                to: "2026-03-31".to_owned(),
            },
            &[],
        )
        .expect("settle with an empty list");
    assert_eq!(
        store
            .list_open_sync_refusals(owner, account, "finam", "2026-03-01", "2026-03-31")
            .expect("list")
            .len(),
        1,
        "a sync of March 31 only must not settle a March 1–31 refusal"
    );

    store
        .settle_sync_refusals_besides(
            &SyncRefusalFilter {
                owner,
                account,
                source: "finam".to_owned(),
                from: "2026-03-31".to_owned(),
                to: "2026-03-31".to_owned(),
            },
            &["row-other".to_owned()],
        )
        .expect("settle with a list of other keys");
    assert_eq!(
        store
            .list_open_sync_refusals(owner, account, "finam", "2026-03-01", "2026-03-31")
            .expect("list")
            .len(),
        1,
        "nor must an absent row key by itself prove anything"
    );

    // A later sync that does cover the whole record's interval and refuses
    // nothing settles it: the re-read is complete, the row is not in its
    // refusal list, and the question is answered.
    store
        .settle_sync_refusals_besides(
            &SyncRefusalFilter {
                owner,
                account,
                source: "finam".to_owned(),
                from: "2026-03-01".to_owned(),
                to: "2026-03-31".to_owned(),
            },
            &[],
        )
        .expect("settle over the full range");
    assert!(
        store
            .list_open_sync_refusals(owner, account, "finam", "2026-03-01", "2026-03-31")
            .expect("list")
            .is_empty(),
        "a sync that covers the record's whole interval and refuses nothing settles it"
    );
}

#[test]
fn a_settle_over_thousands_of_keys_stays_within_the_variable_limit() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    for row in ["row-0", "row-1", "row-2"] {
        store
            .upsert_sync_refusal(&refusal(
                owner,
                account,
                row,
                "2026-01-01",
                "2026-01-31",
                "a refused row",
            ))
            .expect("record");
    }
    // A refusal list larger than SQLite's variable limit: the settle must
    // not fail (an unbounded NOT IN list would), and the rows whose keys
    // are absent from it settle in memory (iaam-vg8te.1.1 acceptance).
    let many: Vec<String> = (0..33_000)
        .map(|index| format!("row-{index}"))
        .filter(|key| key != "row-1" && key != "row-2")
        .collect();
    store
        .settle_sync_refusals_besides(
            &SyncRefusalFilter {
                owner,
                account,
                source: "finam".to_owned(),
                from: "2026-01-01".to_owned(),
                to: "2026-01-31".to_owned(),
            },
            &many,
        )
        .expect("settle over thousands of keys");

    let open = store
        .list_open_sync_refusals(owner, account, "finam", "2026-01-01", "2026-01-31")
        .expect("list");
    assert_eq!(open.len(), 1, "the kept row stays open");
    assert_eq!(open[0].row_key, "row-0");
}

#[test]
fn a_concurrent_interval_widen_cannot_be_settled_from_a_stale_selection() {
    // Two connections to one database: a settle on A must never close a
    // question that a concurrent upsert on B widens past A's coverage. With
    // the selection inside A's immediate transaction, either B wins before
    // the begin (A then sees the widened interval and leaves the record
    // alone) or B waits for A's commit (its upsert reopens the record it
    // settled) (iaam-vg8te.1.1 round 4).
    let path = std::env::temp_dir().join(format!("iaam-sync-refusals-{}.db", Uuid::new_v4()));
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let a = SqliteStore::open(&path).expect("connection A");
    let b = SqliteStore::open(&path).expect("connection B");

    // B widens the refusals interval to February–March; A's settle covers
    // only March — the widened record is outside it and must stay open.
    b.upsert_sync_refusal(&refusal(
        owner,
        account,
        "row-1",
        "2026-02-01",
        "2026-03-31",
        "widened by B",
    ))
    .expect("B records");

    a.settle_sync_refusals_besides(
        &SyncRefusalFilter {
            owner,
            account,
            source: "finam".to_owned(),
            from: "2026-03-01".to_owned(),
            to: "2026-03-31".to_owned(),
        },
        &[],
    )
    .expect("A settles its March range");

    let open = a
        .list_open_sync_refusals(owner, account, "finam", "2026-02-01", "2026-03-31")
        .expect("list");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        open.iter().map(|r| r.row_key.as_str()).collect::<Vec<_>>(),
        vec!["row-1"],
        "the widened question survives A's March-only settle"
    );
}
