//! Source raw material: documents and rows (§10.1).
//!
//! Parsing can be repeated, but raw material cannot: without the stored document body
//! a corrected parser is useless for an already loaded report.

use iaam_core::event::provenance::{ParserVersion, RawHash};
use iaam_core::ids::{OwnerId, SourceId};
use iaam_store::SqliteStore;
use iaam_store::documents::{
    BrokerCode, DocumentStored, NewDocument, RawRow, ReportFormat, RowStatus,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn hash(seed: &str) -> RawHash {
    RawHash::parse(&seed.repeat(64)).unwrap()
}

fn upload(owner: OwnerId, seed: &str) -> NewDocument {
    NewDocument {
        id: SourceId::new_random(),
        owner,
        broker: BrokerCode::parse("tinkoff").unwrap(),
        format: ReportFormat::parse("xlsx").unwrap(),
        parser_version: ParserVersion("tinkoff-xlsx/1".to_owned()),
        document_hash: hash(seed),
        // A real XLSX starts with a ZIP signature and contains
        // arbitrary bytes: the body is not text.
        body: [
            b"PK\x03\x04".as_slice(),
            "report body".as_bytes(),
            &[0x00, 0xff],
        ]
        .concat(),
    }
}

#[test]
fn a_document_survives_a_write_and_a_read() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let uploaded = upload(owner, "a");

    let stored = store.insert_document(&uploaded).unwrap();
    assert_eq!(stored, DocumentStored::Inserted { id: uploaded.id });

    let read = store.load_document(owner, uploaded.id).unwrap();
    assert_eq!(read.id, uploaded.id);
    assert_eq!(read.owner, owner);
    assert_eq!(read.broker, uploaded.broker);
    assert_eq!(read.format, uploaded.format);
    assert_eq!(read.parser_version, uploaded.parser_version);
    assert_eq!(read.document_hash, uploaded.document_hash);
    // The body is returned byte-for-byte: reparsing with a new parser version
    // does not access the source.
    assert_eq!(read.body, uploaded.body);
    OffsetDateTime::parse(&read.uploaded_at, &Rfc3339)
        .expect("the upload moment can be parsed back");
}

#[test]
fn the_same_file_uploaded_twice_keeps_the_first_document() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let first = upload(owner, "b");
    store.insert_document(&first).unwrap();

    // The same file, a different upload identifier: a resend, not
    // a second document.
    let again = NewDocument {
        id: SourceId::new_random(),
        ..upload(owner, "b")
    };
    assert_eq!(
        store.insert_document(&again).unwrap(),
        DocumentStored::AlreadyPresent { existing: first.id }
    );
}

#[test]
fn the_same_file_from_another_owner_is_a_separate_document() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let mine = upload(OwnerId::new_random(), "c");
    let theirs = upload(OwnerId::new_random(), "c");
    store.insert_document(&mine).unwrap();

    // The same file owned by different owners represents different facts about different
    // portfolios, not a duplicate.
    assert_eq!(
        store.insert_document(&theirs).unwrap(),
        DocumentStored::Inserted { id: theirs.id }
    );
}

#[test]
fn a_document_of_another_owner_is_not_readable() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let theirs = upload(OwnerId::new_random(), "d");
    store.insert_document(&theirs).unwrap();

    let stranger = OwnerId::new_random();
    assert!(
        store.load_document(stranger, theirs.id).is_err(),
        "a document owned by someone else cannot be read even with the exact identifier"
    );
}

#[test]
fn a_row_keeps_the_locator_that_provenance_needs() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let document = upload(owner, "e");
    store.insert_document(&document).unwrap();

    let rows = vec![
        RawRow {
            sheet: Some("Trades".to_owned()),
            row: 118,
            payload: "purchase;10;100,50".to_owned(),
            status: RowStatus::Parsed,
        },
        RawRow {
            sheet: Some("Trades".to_owned()),
            row: 119,
            payload: "unknown row".to_owned(),
            status: RowStatus::Unparsed,
        },
    ];
    store.insert_rows(owner, document.id, &rows).unwrap();

    assert_eq!(store.rows_of_document(owner, document.id).unwrap(), rows);
}

#[test]
fn a_row_without_a_sheet_is_stored_and_its_locator_stays_unique() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let document = upload(owner, "f");
    store.insert_document(&document).unwrap();

    // A CSV has no sheet. `None` here means “there was no sheet,” not
    // “the sheet was not parsed,” and it is not replaced with an empty string.
    let row = RawRow {
        sheet: None,
        row: 7,
        payload: "2026-02-01;CASH_IN;1000".to_owned(),
        status: RowStatus::Parsed,
    };
    store
        .insert_rows(owner, document.id, std::slice::from_ref(&row))
        .unwrap();
    assert_eq!(
        store.rows_of_document(owner, document.id).unwrap(),
        vec![row.clone()]
    );

    // The same locator a second time—the same raw-material chunk twice.
    assert!(
        store.insert_rows(owner, document.id, &[row]).is_err(),
        "a locator without a sheet must remain unique"
    );
}

#[test]
fn rows_are_not_added_to_another_owners_document() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let theirs = upload(OwnerId::new_random(), "6");
    store.insert_document(&theirs).unwrap();

    let stranger = OwnerId::new_random();
    let row = RawRow {
        sheet: None,
        row: 1,
        payload: "injected row".to_owned(),
        status: RowStatus::Parsed,
    };
    assert!(
        store.insert_rows(stranger, theirs.id, &[row]).is_err(),
        "rows are not appended to another document"
    );
}

#[test]
fn rows_of_another_owners_document_are_not_readable() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let document = upload(owner, "0");
    store.insert_document(&document).unwrap();
    store
        .insert_rows(
            owner,
            document.id,
            &[RawRow {
                sheet: None,
                row: 1,
                payload: "own row".to_owned(),
                status: RowStatus::Parsed,
            }],
        )
        .unwrap();

    let stranger = OwnerId::new_random();
    assert!(
        store.rows_of_document(stranger, document.id).is_err(),
        "rows from another document are not readable"
    );
}

#[test]
fn a_partly_rejected_batch_of_rows_leaves_no_half_document() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let document = upload(owner, "1");
    store.insert_document(&document).unwrap();

    let good = RawRow {
        sheet: None,
        row: 1,
        payload: "first".to_owned(),
        status: RowStatus::Parsed,
    };
    let clash = RawRow {
        row: 1,
        payload: "second with the same locator".to_owned(),
        ..good.clone()
    };
    assert!(
        store
            .insert_rows(owner, document.id, &[good, clash])
            .is_err()
    );
    assert_eq!(
        store.rows_of_document(owner, document.id).unwrap(),
        vec![],
        "rejecting one row cancels the entire batch: half the raw material is worse than none"
    );
}

#[test]
fn stored_raw_material_resists_a_direct_repair_script() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let document = upload(owner, "2");
    store.insert_document(&document).unwrap();
    store
        .insert_rows(
            owner,
            document.id,
            &[RawRow {
                sheet: None,
                row: 1,
                payload: "row".to_owned(),
                status: RowStatus::Parsed,
            }],
        )
        .unwrap();

    // Checked with direct SQL rather than through the wrapper: the guard must cover
    // the data-repair script as well.
    let conn = store.connection();
    for sql in [
        "UPDATE source_documents SET body = x'00'",
        "DELETE FROM source_documents",
        "UPDATE raw_rows SET payload = 'replacement'",
        "DELETE FROM raw_rows",
    ] {
        assert!(
            conn.execute(sql, []).is_err(),
            "raw material is immutable: {sql}"
        );
    }
}

#[test]
fn only_documents_parsed_by_another_version_await_reparse() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let old = NewDocument {
        parser_version: ParserVersion("tinkoff-xlsx/1".to_owned()),
        ..upload(owner, "3")
    };
    let current = NewDocument {
        parser_version: ParserVersion("tinkoff-xlsx/2".to_owned()),
        ..upload(owner, "4")
    };
    store.insert_document(&old).unwrap();
    store.insert_document(&current).unwrap();

    let awaiting = store
        .documents_needing_reparse(owner, &ParserVersion("tinkoff-xlsx/2".to_owned()))
        .unwrap();
    let ids: Vec<SourceId> = awaiting.iter().map(|document| document.id).collect();
    assert_eq!(ids, vec![old.id]);
}

#[test]
fn documents_of_another_owner_never_await_our_reparse() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let theirs = upload(OwnerId::new_random(), "5");
    store.insert_document(&theirs).unwrap();

    let stranger = OwnerId::new_random();
    assert_eq!(
        store
            .documents_needing_reparse(stranger, &ParserVersion("tinkoff-xlsx/9".to_owned()))
            .unwrap(),
        vec![]
    );
}

#[test]
fn a_broker_or_format_without_a_name_is_refused() {
    // An empty broker code in the database is indistinguishable from “broker unknown”, while
    // an unknown value is an `Option`, not an empty string (§4.9).
    assert!(BrokerCode::parse("").is_none());
    assert!(BrokerCode::parse("   ").is_none());
    assert!(ReportFormat::parse("").is_none());
    assert!(ReportFormat::parse(" \t ").is_none());
    assert_eq!(BrokerCode::parse("finam").unwrap().as_str(), "finam");
    assert_eq!(ReportFormat::parse("xlsx").unwrap().as_str(), "xlsx");
}
