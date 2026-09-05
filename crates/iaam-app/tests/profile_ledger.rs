//! A profile version names one content, across a real restart (`iaam-mr25`).
//!
//! The catalogue's own tests bind against a ledger held in memory, which proves
//! that the record and not a within-load comparison is what refuses. This file
//! proves the other half: that the record is written where a process ending
//! does not take it — a database file on disk, closed, reopened, and asked
//! again.
//!
//! Nothing here is derived from any real export. The profile is invented end to
//! end: an institution that does not exist, English column headings, and no
//! value from anybody's statement (CLAUDE.md, "Conventions & Patterns").

use std::path::PathBuf;

use iaam_app::adapters::profile_ledger::StoreVersionLedger;
use iaam_app::ingest::profile::ProfileCatalogue;
use iaam_store::SqliteStore;
use uuid::Uuid;

/// One invented profile, with the version and the label the caller asks for.
///
/// `document_label` is what varies to change the file's content without
/// changing what it reads — precisely the edit the binding has to catch, since
/// a version names a content and not an intention.
fn a_profile(version: u32, label: &str) -> String {
    serde_json::json!({
        "schema_version": 1,
        "id": "example-bank-statement",
        "version": version,
        "issuer": "Example Bank",
        "document_label": label,
        "document": {
            "format": "csv",
            "encoding": "utf-8",
            "delimiter": "semicolon",
            "header_row": 1
        },
        "recognise": { "header_cells": ["Posted", "Operation", "Sum", "Ccy", "State"] },
        "row": {
            "account": { "from": "declaration" },
            "dates": {
                "cash_posted": { "column": "Posted", "format": "day_month_year_dot" }
            },
            "amount": {
                "decimal": {
                    "decimal_separator": "comma",
                    "group_separator": "space",
                    "negative": "leading_minus"
                },
                "carried_by": { "from": "signed_column", "column": "Sum" }
            },
            "currency": { "from": "column", "column": "Ccy", "spellings": { "USDollar": "USD" } },
            "direction": {
                "from": "column",
                "column": "Operation",
                "tokens": { "Debit": "out", "Credit": "in" }
            },
            "status": {
                "column": "State",
                "tokens": { "Settled": "completed", "Refused": "declined" }
            }
        }
    })
    .to_string()
}

/// One instance: a directory of profiles and the database file beside it.
struct Instance {
    profiles: PathBuf,
    database: PathBuf,
}

impl Instance {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("iaam-profile-ledger-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("a directory to keep the instance's files in");
        Self {
            profiles: root.join("profiles"),
            database: root.join("iaam.db"),
        }
    }

    fn publish(&self, body: &str) {
        std::fs::create_dir_all(&self.profiles).expect("the profile directory");
        std::fs::write(self.profiles.join("example.json"), body).expect("the profile file");
    }

    /// Start the instance: open its database, assemble its catalogue, bind.
    ///
    /// Everything this function opens is dropped when it returns, which is what
    /// makes calling it twice a restart and not a second look at live state.
    fn start(&self) -> ProfileCatalogue {
        let store = SqliteStore::open(&self.database).expect("the instance's database");
        let mut ledger = StoreVersionLedger::new(&store);
        ProfileCatalogue::with_local(&self.profiles).bound_by(&mut ledger)
    }

    fn clean_up(&self) {
        if let Some(root) = self.database.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

/// A profile edited between two starts is refused on the second one, and the
/// refusal names the content the version already stands for.
///
/// This is the defect: a wave changed what a bundled profile read and left the
/// version at 1, and nothing mechanical noticed, because the only comparison in
/// the tree was among the files of one pass.
#[test]
fn a_content_changed_between_two_starts_is_refused_on_the_second() {
    let instance = Instance::new();
    instance.publish(&a_profile(1, "Account statement"));
    let recorded = instance
        .start()
        .get("example-bank-statement")
        .expect("the first start installs the profile")
        .profile
        .digest()
        .to_owned();

    instance.publish(&a_profile(1, "Statement of account"));
    let catalogue = instance.start();

    assert!(
        catalogue.get("example-bank-statement").is_none(),
        "the version already names a content, and this is a different one"
    );
    let refused = catalogue
        .refused()
        .iter()
        .find(|refused| refused.id.as_deref() == Some("example-bank-statement"))
        .expect("the refusal is published rather than logged");
    assert!(
        refused.reason.contains(&recorded),
        "the refusal names what the version stands for: {refused:?}"
    );
    // What this instance still reads is unaffected: one refused profile takes
    // nothing else down with it.
    assert!(catalogue.get("tbank-operations-csv").is_some());
    instance.clean_up();
}

/// The same file starts the instance again and again, and the binding is
/// silent.
///
/// An instance that refused its own profile on the second start would refuse
/// every import from then on.
#[test]
fn an_unchanged_profile_starts_the_instance_again_and_again() {
    let instance = Instance::new();
    instance.publish(&a_profile(1, "Account statement"));

    for _ in 0..3 {
        let catalogue = instance.start();
        assert!(
            catalogue.get("example-bank-statement").is_some(),
            "the same content under the same version is the same profile"
        );
        assert!(catalogue.refused().is_empty(), "{:?}", catalogue.refused());
    }
    instance.clean_up();
}

/// A changed reading published under a new version starts normally.
///
/// Raising the version is the supported way to change what a profile reads, and
/// the record must not stand in its way.
#[test]
fn a_new_version_starts_the_instance_normally() {
    let instance = Instance::new();
    instance.publish(&a_profile(1, "Account statement"));
    drop(instance.start());

    instance.publish(&a_profile(2, "Statement of account"));
    let catalogue = instance.start();

    let installed = catalogue
        .get("example-bank-statement")
        .expect("a new version is how a reading changes");
    assert_eq!(installed.profile.version(), 2);
    assert!(catalogue.refused().is_empty(), "{:?}", catalogue.refused());
    instance.clean_up();
}
