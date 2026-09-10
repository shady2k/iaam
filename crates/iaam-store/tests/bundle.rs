//! Archived bundle: export, import, corruption.

use iaam_core::category::CategoryMatcher;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::custody::CustodyOrigin;
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{EventKind, IncomeKind};
use iaam_core::event::leg::Leg;
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RuleSettlement};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, CustodyId, EventId, ImportSessionId, InstrumentId, OwnerId,
    PrincipalId, SourceId,
};
use iaam_core::instrument::{AliasInterval, CurrencyRoles, Lineage, LineageReason};
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::evidence::IdentityScope;
use iaam_core::retirement::AccountRetirement;
use iaam_store::SqliteStore;
use iaam_store::bundle::{Bundle, ImportOutcome};
use iaam_store::categories::NewCategoryRule;
use iaam_store::reference::{
    AccountAliasRecord, AccountRecord, AccountScopeExclusionRecord, CustodyRecord, InstrumentRecord,
};
use iaam_store::rules::NewRule;
use time::macros::date;

fn deposit(owner: OwnerId, account: AccountId, sequence: u32, minor: i64) -> Event {
    let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
    let day = date!(2026 - 05 - 05);
    Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::CashIn { amount },
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs: vec![Leg::cash(account, amount)],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"7".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn populated() -> (SqliteStore, OwnerId, AccountId, ContourId) {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Brokerage".into(),
            institution: Some("T-Bank".into()),
        })
        .unwrap();
    let contour = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(1), [account]),
            "My portfolio",
            &[account],
        )
        .unwrap();
    store
        .append_event(&deposit(owner, account, 1, 100_000), IdentityScope::Source)
        .unwrap();
    store
        .append_event(&deposit(owner, account, 2, 250_000), IdentityScope::Source)
        .unwrap();
    (store, owner, account, contour)
}

#[test]
fn a_bundle_restores_a_complete_working_state() {
    // Exporting events alone is not a backup: it produces
    // different projections because the set of scopes remains external.
    let (source, owner, account, contour) = populated();
    let bundle = source.export_bundle(owner).unwrap();
    assert_eq!(bundle.events.len(), 2);
    assert_eq!(bundle.accounts.len(), 1);
    assert_eq!(bundle.contours.len(), 1);
    assert_eq!(bundle.contours[0].accounts, vec![account.inner()]);

    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert_eq!(
        restored.import_bundle(&bundle).unwrap(),
        ImportOutcome::Applied {
            inserted: 2,
            duplicates: 0
        }
    );
    assert_eq!(
        restored.load_events(owner).unwrap(),
        source.load_events(owner).unwrap()
    );
    assert_eq!(restored.list_accounts(owner).unwrap().len(), 1);
    assert!(
        restored
            .load_contour(owner, contour, ContourVersion(1))
            .unwrap()
            .unwrap()
            .contains(account)
    );
}

#[test]
fn importing_the_same_bundle_twice_changes_nothing() {
    let (source, owner, _, _) = populated();
    let bundle = source.export_bundle(owner).unwrap();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();
    assert_eq!(
        restored.import_bundle(&bundle).unwrap(),
        ImportOutcome::Applied {
            inserted: 0,
            duplicates: 2
        }
    );
    assert_eq!(restored.load_events(owner).unwrap().len(), 2);
}

#[test]
fn a_tampered_bundle_is_refused() {
    // A corrupted archive is worse than a missing one: it looks intact.
    let (source, owner, _, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events.truncate(1);
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
}

#[test]
fn a_changed_amount_breaks_the_checksum() {
    // The first version of the checksum hashed only event identifiers:
    // a substituted monetary value passed verification, and an archive
    // with incorrect amounts looked intact.
    let (source, owner, account, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events[0] = Event {
        kind: EventKind::CashIn {
            amount: Money::new(PostedMinor::new(999_999_999), CurrencyCode::Rub),
        },
        legs: vec![Leg::cash(
            account,
            Money::new(PostedMinor::new(999_999_999), CurrencyCode::Rub),
        )],
        ..bundle.events[0].clone()
    };
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(
        restored.import_bundle(&bundle).is_err(),
        "the substituted amount must invalidate the checksum"
    );
}

#[test]
fn a_bundle_carrying_a_foreign_event_is_refused() {
    let (source, owner, account, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events[0] = Event {
        owner: OwnerId::new_random(),
        ..bundle.events[0].clone()
    };
    bundle.checksum = bundle.compute_checksum();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
    // Nothing was written: the import runs in a single transaction.
    assert!(restored.load_events(owner).unwrap().is_empty());
    assert!(restored.list_accounts(owner).unwrap().is_empty());
    let _ = account;
}

#[test]
fn a_bundle_written_by_a_newer_schema_is_refused() {
    let (source, owner, _, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.schema_version = iaam_store::schema::SCHEMA_VERSION + 1;
    bundle.checksum = bundle.compute_checksum();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
}

#[test]
fn a_bundle_of_a_newer_format_is_refused() {
    let (source, owner, _, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.bundle_version = iaam_store::bundle::BUNDLE_VERSION + 1;
    bundle.checksum = bundle.compute_checksum();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
}

#[test]
fn a_bundle_survives_json() {
    // A bundle is a portable archive: it must survive a text format.
    let (source, owner, _, _) = populated();
    let bundle = source.export_bundle(owner).unwrap();
    let json = serde_json::to_string(&bundle).unwrap();
    let back: iaam_store::bundle::Bundle = serde_json::from_str(&json).unwrap();
    assert_eq!(back, bundle);
}

#[test]
fn versions_of_a_contour_are_exported_as_separate_sections_with_all_their_accounts() {
    // The scope composition is exported one line at a time: one line per account. Rebuilding
    // the section must distinguish BOTH the scope AND the version — otherwise accounts
    // from different versions are merged into one, and the restored scope gets a composition
    // it never had. The report figures look normal after such a restoration and are incorrect.
    // recoveries look normal but are incorrect.
    let (mut store, owner, first_account, contour) = populated();

    let second_account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: second_account,
            owner,
            title: "Second brokerage".into(),
            institution: None,
        })
        .unwrap();

    // Version 2 of the same scope: the account was added.
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(2), [first_account, second_account]),
            "My portfolio",
            &[first_account, second_account],
        )
        .unwrap();

    // And a separate scope consisting of one account.
    let other = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(other, ContourVersion(1), [second_account]),
            "Only the second",
            &[second_account],
        )
        .unwrap();

    let bundle = store.export_bundle(owner).unwrap();
    assert_eq!(
        bundle.contours.len(),
        3,
        "two versions of the first scope and one of the second — three sections, not fewer"
    );

    let section = |id: ContourId, version: u32| {
        bundle
            .contours
            .iter()
            .find(|section| section.contour == id.0 && section.version == version)
            .unwrap_or_else(|| panic!("section {id:?} of version {version} not found"))
    };
    assert_eq!(section(contour, 1).accounts, vec![first_account.inner()]);
    assert_eq!(section(contour, 2).accounts.len(), 2);
    assert!(
        section(contour, 2)
            .accounts
            .contains(&second_account.inner())
    );
    assert_eq!(section(other, 1).accounts, vec![second_account.inner()]);

    // And all of this survives a round trip through the archive.
    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();
    let back = restored.export_bundle(owner).unwrap();
    assert_eq!(back.contours.len(), 3);
    let version_two = restored
        .load_contour(owner, contour, ContourVersion(2))
        .unwrap()
        .expect("version 2 restored");
    assert!(version_two.contains(first_account));
    assert!(version_two.contains(second_account));

    let version_one = restored
        .load_contour(owner, contour, ContourVersion(1))
        .unwrap()
        .expect("version 1 restored");
    assert!(version_one.contains(first_account));
    assert!(
        !version_one.contains(second_account),
        "the account from version 2 must not leak into version 1"
    );
}

// --- new E3.4 facts ---

fn bond_event(
    owner: OwnerId,
    account: AccountId,
    sequence: u32,
    kind: EventKind,
    legs: Vec<Leg>,
) -> Event {
    let day = date!(2026 - 06 - 15);
    Event {
        id: EventId::new_random(),
        owner,
        account,
        kind,
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs,
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"9".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

/// The instrument and custody place `every_new_fact`'s events name.
///
/// `event_legs.instrument` and `.custody` are real, undeferred foreign keys
/// (relational journal design §4.2), so a fact naming an instrument or
/// custody place is only writable where that reference data already exists.
/// On `source`, that means registering it here, before the fact is appended
/// — nothing else would populate it. On `restored`, it no longer does: the
/// bundle now carries a `custody_places` section (every `declared` row and
/// the `minted` rows the exported events reach) and an `instruments` section
/// (the closure those events reach, plus `lineage_parent` transitively), and
/// `import_bundle` writes both before the events loop (design §7). A restore
/// into a genuinely empty store is exactly what this is meant to prove, so
/// `register_in` is called on `source` only — never on `restored`.
struct BondReferenceData {
    instrument: InstrumentId,
    successor: InstrumentId,
    custody: CustodyId,
}

impl BondReferenceData {
    fn new() -> Self {
        Self {
            instrument: InstrumentId::new_random(),
            successor: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
        }
    }

    fn register_in(&self, store: &SqliteStore, owner: OwnerId) {
        store
            .upsert_instrument(&InstrumentRecord {
                id: self.instrument,
                kind: None,
                symbol: "TESTBOND".to_owned(),
                title: "Test Bond".to_owned(),
                currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
                lineage: None,
            })
            .unwrap();
        store
            .upsert_instrument(&InstrumentRecord {
                id: self.successor,
                kind: None,
                symbol: "TESTBOND2".to_owned(),
                title: "Test Bond, Successor".to_owned(),
                currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
                lineage: None,
            })
            .unwrap();
        store
            .upsert_custody_place(&CustodyRecord {
                id: self.custody,
                owner,
                title: "Shop One Custody".to_owned(),
                institution: None,
                origin: CustodyOrigin::Declared,
            })
            .unwrap();
    }
}

/// One event of each new kind. The test must fail when
/// a member is added to the family: an archive that has lost a fact looks intact.
fn every_new_fact(owner: OwnerId, account: AccountId, refs: &BondReferenceData) -> Vec<Event> {
    let instrument = refs.instrument;
    let successor = refs.successor;
    let custody = refs.custody;
    let submission = OfferSubmissionId::new_random();
    let money = |minor| Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
    // `rust_decimal` is not a dependency of this crate, and adding it
    // just for the test would modify Cargo.toml, which is the policy file. The number
    // arrives through the same serde that the storage reads it with.
    let dec = |text: &str| serde_json::from_str::<Dec>(text).unwrap();
    let qty = |text: &str| Quantity(dec(text));
    let per_unit = |text: &str| PerUnitAmount::new(dec(text), CurrencyCode::Rub);

    vec![
        bond_event(
            owner,
            account,
            10,
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody,
                    quantity: qty("10"),
                    principal_returned_per_unit: per_unit("200"),
                    compensation: money(200_000),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: Some(date!(2026 - 06 - 13)),
                    grounds: Some("issuer decision no. 4".to_owned()),
                    basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
                },
            },
            vec![Leg::principal(account, instrument, money(200_000))],
        ),
        bond_event(
            owner,
            account,
            11,
            EventKind::CorporateAction {
                action: CorporateAction::Redemption {
                    instrument,
                    custody,
                    quantity: qty("10"),
                    principal_returned_per_unit: per_unit("800"),
                    compensation: money(800_000),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                },
            },
            vec![
                Leg::principal(account, instrument, money(800_000)),
                Leg::security(account, custody, instrument, qty("-10")),
            ],
        ),
        bond_event(
            owner,
            account,
            12,
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: instrument,
                    successor,
                    custody,
                    ratio: Dec::one(),
                    quantity_in: qty("10"),
                    quantity_out: qty("10"),
                    fractional: FractionalTreatment::NotApplicable,
                    compensation: None,
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_transfer: BasisTransferRule::CarryOver,
                },
            },
            vec![
                Leg::security(account, custody, instrument, qty("-10")),
                Leg::security(account, custody, successor, qty("10")),
            ],
        ),
        bond_event(
            owner,
            account,
            13,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Submitted {
                    submission,
                    window: OfferWindowId::new_random(),
                    instrument,
                    quantity: qty("10"),
                },
            },
            Vec::new(),
        ),
        bond_event(
            owner,
            account,
            14,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Cancelled {
                    submission,
                    quantity: qty("4"),
                },
            },
            Vec::new(),
        ),
        bond_event(
            owner,
            account,
            15,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission,
                    instrument,
                    custody,
                    quantity: qty("6"),
                    gross: money(600_000),
                    fee: Some(money(1_000)),
                    accrued_interest: Some(money(12_345)),
                },
            },
            vec![
                Leg::cash(account, money(611_345)),
                Leg::security(account, custody, instrument, qty("-6")),
            ],
        ),
        bond_event(
            owner,
            account,
            16,
            EventKind::Income {
                instrument: Some(instrument),
                gross: money(70_000),
                kind: Some(IncomeKind::Coupon),
            },
            vec![Leg::cash(account, money(70_000))],
        ),
    ]
}

#[test]
fn a_bundle_round_trip_keeps_the_new_facts() {
    // An archive that has lost a new fact looks intact—and will be detected
    // only during restoration, when the original database is no longer available.
    let (mut source, owner, account, _) = populated();
    let refs = BondReferenceData::new();
    refs.register_in(&source, owner);
    let facts = every_new_fact(owner, account, &refs);
    for event in &facts {
        source.append_event(event, IdentityScope::Source).unwrap();
    }

    let bundle = source.export_bundle(owner).unwrap();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    // Genuinely empty: no pre-seeding. The bundle's own custody_places and
    // instruments sections are what makes a security leg writable here.
    restored.import_bundle(&bundle).unwrap();

    assert_eq!(
        restored.load_events(owner).unwrap(),
        source.load_events(owner).unwrap()
    );
    let stored = restored.load_events(owner).unwrap();
    for event in &facts {
        assert!(
            stored.iter().any(|kept| kept == event),
            "fact {} did not survive a round trip through the archive",
            event.kind.discriminant()
        );
    }
}

// --- Task 7: reference data a restore into an empty database needs -------

/// A store holding exactly one event that names an instrument and a custody
/// place: `every_new_fact`'s first fact, `PartialRedemption`, references
/// both through its `CorporateAction` variant regardless of what its legs
/// carry (`Event::referenced_custodies` matches the variant, not the legs).
fn a_store_holding_one_security_event() -> (SqliteStore, OwnerId, BondReferenceData) {
    let (mut store, owner, account, _) = populated();
    let refs = BondReferenceData::new();
    refs.register_in(&store, owner);
    let event = every_new_fact(owner, account, &refs)
        .into_iter()
        .next()
        .expect("every_new_fact always yields at least one fact");
    store.append_event(&event, IdentityScope::Source).unwrap();
    (store, owner, refs)
}

/// A bundle as the previous build produced it: no `custody_places` key, no
/// `instruments` key. Produced with the version-1 serialiser — `git stash`
/// on this change, `cargo test -p iaam-store --test _tmp -- --nocapture`
/// against a throwaway test exporting a store with one invented cash-in
/// event, paste, `git stash pop`, delete the throwaway test — rather than by
/// exporting with the current code and deleting the two keys: that would
/// yield `bundle_version: 2` and a checksum that never matched anything
/// real, proving only that a version-2 object with empty skipped fields
/// works, which is not the claim. Every value below — the owner, the
/// account, the event — is invented; none of it is derived from a real
/// export.
const ARCHIVE_WITHOUT_REFERENCE_SECTIONS: &str = r#"{"bundle_version":1,"schema_version":1,"exported_at":"2026-09-09T08:31:19.115697019Z","owner":"1a85610c-2e6f-4844-999a-cad3bf831ef8","events":[{"id":"072ca098-ea42-489e-b8ba-313e5d117b92","owner":"1a85610c-2e6f-4844-999a-cad3bf831ef8","account":"37a8b6c4-3d54-4e07-a9fa-fa9509a03cfd","kind":{"CashIn":{"amount":{"amount":150000,"currency":"Rub"}}},"dates":{"trade":null,"settled":null,"cash_posted":[2026,91],"entitlement":null,"paid":null,"tax_period_override":null},"order":{"date":[2026,91],"source_time":null,"sequence":1},"legs":[{"kind":"Cash","account":"37a8b6c4-3d54-4e07-a9fa-fa9509a03cfd","custody":null,"instrument":null,"money":{"amount":150000,"currency":"Rub"},"quantity":null}],"provenance":{"source":"4351072a-7e4e-44c9-bf5d-3462f8cf30ec","raw_hash":"3333333333333333333333333333333333333333333333333333333333333333","parser_version":"manual/1","source_operation_id":null,"row":null},"relation":"None","confidence":"Known","idempotency_key":null}],"accounts":[{"id":"37a8b6c4-3d54-4e07-a9fa-fa9509a03cfd","title":"Main","institution":"Broker One"}],"contours":[{"contour":"0cc213b0-cf7b-4d45-b6b8-b563ba04e078","version":1,"title":"My portfolio","accounts":["37a8b6c4-3d54-4e07-a9fa-fa9509a03cfd"]}],"checksum":"286f4c4daa9115c206e3dff80b2f6806d2cb7a39ecbc765f144363f0f1bcad5d"}"#;

#[test]
fn an_archive_written_before_the_reference_sections_still_verifies() {
    // A bundle as the previous build produced it: no custody_places key, no
    // instruments key. Deserialising it must yield empty sections, and its
    // checksum must still match — a section nobody wrote contributes no bytes.
    let bundle: Bundle = serde_json::from_str(ARCHIVE_WITHOUT_REFERENCE_SECTIONS)
        .expect("an old archive still reads");
    assert!(bundle.custody_places.is_empty());
    assert!(bundle.instruments.is_empty());
    assert_eq!(bundle.checksum, bundle.compute_checksum());
}

#[test]
fn a_tampered_custody_place_breaks_the_checksum() {
    let (store, owner, _refs) = a_store_holding_one_security_event();
    let mut bundle = store.export_bundle(owner).expect("export");
    assert!(!bundle.custody_places.is_empty());
    bundle.custody_places[0].title.push_str(" (substituted)");
    assert_ne!(bundle.checksum, bundle.compute_checksum());
}

#[test]
fn a_restore_does_not_overwrite_an_instrument_already_in_the_target() {
    // Instruments are global reference data shared with whatever else the
    // target database holds. Unlike accounts and custody places, which are
    // the owner's own and do update on conflict, an archive supplies what is
    // missing and never overwrites what is there: `ON CONFLICT DO NOTHING`.
    let (store, owner, refs) = a_store_holding_one_security_event();
    let bundle = store.export_bundle(owner).expect("export");
    assert!(
        bundle
            .instruments
            .iter()
            .any(|section| section.id == refs.instrument.inner())
    );

    let mut restored = SqliteStore::open_in_memory().unwrap();
    // A live market sync already knows this instrument, under a title the
    // archive does not carry.
    restored
        .upsert_instrument(&InstrumentRecord {
            id: refs.instrument,
            kind: None,
            symbol: "TESTBOND".to_owned(),
            title: "Already Known Title".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .unwrap();

    restored.import_bundle(&bundle).unwrap();

    let after = restored
        .instrument(refs.instrument)
        .unwrap()
        .expect("the instrument is still there");
    assert_eq!(
        after.title, "Already Known Title",
        "the archive's own title for this instrument must not overwrite what \
         the target already had"
    );
}

#[test]
fn a_security_event_restores_into_a_genuinely_empty_store() {
    // The claim this task exists to prove: a bundle holding one security
    // leg restores into a database with no custody place and no instrument
    // in it at all — no pre-seeding, no market sync first.
    let (store, owner, _refs) = a_store_holding_one_security_event();
    let bundle = store.export_bundle(owner).expect("export");
    assert!(!bundle.custody_places.is_empty());
    assert!(!bundle.instruments.is_empty());

    let mut restored = SqliteStore::open_in_memory().unwrap();
    let outcome = restored
        .import_bundle(&bundle)
        .expect("a bundle carrying its own reference data must restore into an empty store");
    assert_eq!(
        outcome,
        ImportOutcome::Applied {
            inserted: bundle.events.len(),
            duplicates: 0
        }
    );
}

#[test]
fn a_lineage_chain_two_deep_imports_regardless_of_the_sections_own_order() {
    // instruments.lineage_parent is a self-referencing foreign key: an
    // instrument whose parent is also in the bundle's instruments section
    // must be inserted after it. Symbols are chosen so ORDER BY symbol (the
    // closure query's own order) lists the child before the parent before
    // the grandparent — the worst order for a single insertion pass, and
    // exactly the case a naive one-level-of-indirection fix would miss.
    let (mut store, owner, account, _) = populated();
    let grandparent = InstrumentId::new_random();
    let parent = InstrumentId::new_random();
    let child = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: grandparent,
            kind: None,
            symbol: "CCC-GRANDPARENT".to_owned(),
            title: "Grandparent Bond".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .unwrap();
    store
        .upsert_instrument(&InstrumentRecord {
            id: parent,
            kind: None,
            symbol: "BBB-PARENT".to_owned(),
            title: "Parent Bond".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: Some(Lineage {
                parent: grandparent,
                reason: LineageReason::Replacement,
            }),
        })
        .unwrap();
    store
        .upsert_instrument(&InstrumentRecord {
            id: child,
            kind: None,
            symbol: "AAA-CHILD".to_owned(),
            title: "Child Bond".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: Some(Lineage {
                parent,
                reason: LineageReason::Replacement,
            }),
        })
        .unwrap();

    let event = Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::Income {
            instrument: Some(child),
            gross: Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
            kind: Some(IncomeKind::Coupon),
        },
        dates: EventDates::for_cash(CashPostedDate(date!(2026 - 07 - 01))),
        order: EffectiveOrder::new(date!(2026 - 07 - 01), 30),
        legs: vec![Leg::cash(
            account,
            Money::new(PostedMinor::new(1_000), CurrencyCode::Rub),
        )],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"6".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    };
    store.append_event(&event, IdentityScope::Source).unwrap();

    let bundle = store.export_bundle(owner).unwrap();
    let ids: Vec<uuid::Uuid> = bundle
        .instruments
        .iter()
        .map(|section| section.id)
        .collect();
    assert!(ids.contains(&child.inner()), "the named instrument travels");
    assert!(
        ids.contains(&parent.inner()),
        "lineage_parent must be walked one level"
    );
    assert!(
        ids.contains(&grandparent.inner()),
        "lineage_parent must be walked transitively, not just one level"
    );
    assert_eq!(
        bundle.instruments[0].id,
        child.inner(),
        "ORDER BY symbol must put the child ahead of its own parent, or this \
         test is not exercising the ordering fix at all"
    );

    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored
        .import_bundle(&bundle)
        .expect("a lineage chain two deep must import regardless of the section's own order");
    let restored_child = restored
        .instrument(child)
        .unwrap()
        .expect("child instrument restored");
    let restored_parent = restored
        .instrument(parent)
        .unwrap()
        .expect("parent instrument restored");
    assert_eq!(restored_child.lineage.map(|l| l.parent), Some(parent));
    assert_eq!(restored_parent.lineage.map(|l| l.parent), Some(grandparent));
}

// --- Task 10: relation_target carries no key, deferred or otherwise -------

#[test]
fn a_bundle_whose_replacement_precedes_its_target_imports_cleanly() {
    // An earlier draft of the design gave `relation_target` a deferred
    // `(owner, relation_target)` key so a bundle could import a graph whose
    // replacement precedes its target. That key was removed: `events` has no
    // key on `relation_target` at all, deferred or not, so there is nothing
    // left to order around — a replacement ahead of its target in the
    // bundle's event list must import exactly as cleanly as one behind it.
    let (source, owner, account, _) = populated();
    let target = deposit(owner, account, 3, 100_000);
    let replacement = Event {
        relation: Relation::Replacement { target: target.id },
        ..deposit(owner, account, 4, 100_000)
    };

    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events = vec![replacement.clone(), target.clone()];
    bundle.checksum = bundle.compute_checksum();

    let mut restored = SqliteStore::open_in_memory().unwrap();
    let outcome = restored
        .import_bundle(&bundle)
        .expect("a replacement ahead of its target must not be refused");
    assert_eq!(
        outcome,
        ImportOutcome::Applied {
            inserted: 2,
            duplicates: 0
        }
    );
}

#[test]
fn a_bundle_import_of_a_replacement_naming_a_target_it_never_held_succeeds() {
    // The journal has a named state for a correction whose target it does
    // not hold: `resolve_with_unheld_targets` resolves such a chain and
    // `HistoryAct::Arrived` publishes it — a fact naming a target outside the
    // journal is where its history begins. A key on `relation_target` would
    // make that state unwritable by any path, including a bundle import, so
    // this must succeed rather than merely survive.
    let (source, owner, account, _) = populated();
    let unheld_target = EventId::new_random();
    let replacement = Event {
        relation: Relation::Replacement {
            target: unheld_target,
        },
        ..deposit(owner, account, 3, 100_000)
    };

    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events.push(replacement.clone());
    bundle.checksum = bundle.compute_checksum();

    let mut restored = SqliteStore::open_in_memory().unwrap();
    let outcome = restored
        .import_bundle(&bundle)
        .expect("an unheld target must not be a reason to refuse the import");
    assert_eq!(
        outcome,
        ImportOutcome::Applied {
            inserted: 3,
            duplicates: 0
        }
    );
    let stored = restored.load_events(owner).unwrap();
    assert!(stored.iter().any(|kept| kept.id == replacement.id));
}

#[test]
fn an_import_of_an_event_carrying_import_session_and_rule_identifiers_the_bundle_does_not_hold_succeeds()
 {
    // `import_session` and `settled_by_rule` look like foreign keys and are
    // deliberately not: the bundle carries events, accounts and contours and
    // nothing else (design §4.1), so an archive restore must not depend on
    // an import session or a classification rule existing anywhere.
    let (source, owner, account, _) = populated();
    let session = ImportSessionId::new_random();
    let rule = ClassificationRuleId::new_random();
    let event = Event {
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"5".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        )
        .with_import_session(session)
        .with_rule_settlement(RuleSettlement::Rule { rule, version: 1 }),
        ..deposit(owner, account, 3, 50_000)
    };

    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events.push(event.clone());
    bundle.checksum = bundle.compute_checksum();

    let mut restored = SqliteStore::open_in_memory().unwrap();
    let outcome = restored
        .import_bundle(&bundle)
        .expect("neither import_session nor settled_by_rule is a foreign key");
    assert_eq!(
        outcome,
        ImportOutcome::Applied {
            inserted: 3,
            duplicates: 0
        }
    );
    let stored = restored.load_events(owner).unwrap();
    let kept = stored
        .iter()
        .find(|kept| kept.id == event.id)
        .expect("the event carrying the dangling identifiers was imported");
    assert_eq!(kept.provenance.import_session(), Some(session));
    assert_eq!(
        kept.provenance.rule_settlement(),
        Some(&RuleSettlement::Rule { rule, version: 1 })
    );
}

// --- iaam-k3gh.9.2: the owner's decisions travel --------------------------

/// A minimal, always-valid classification rule: one condition
/// (`description_contains`) and an outcome (`external_flow`) that needs none
/// of the fields the schema's `CHECK` makes conditional on the others.
fn invented_classification_rule() -> NewRule {
    NewRule {
        counterparty_account: None,
        description_contains: Some("Coffee".to_owned()),
        source_kind: None,
        source_category: None,
        owner_category: None,
        source_code: None,
        movement: None,
        outcome_kind: "external_flow".to_owned(),
        to_account: None,
        fee_origin: None,
        income_kind: None,
    }
}

/// A store touching every table iaam-k3gh.9.2 carries: account aliases, a
/// scope exclusion, a transfer statement with a partner, a retirement
/// history two revisions deep, a declined account name, a recorded decision,
/// a category group and category, a classification rule that has been
/// amended once (so `retired_at` and `replaces` are both exercised), a
/// category rule amended the same way, and the category-assignment
/// projection rebuilt over an event that matches it.
///
/// Every value here is invented for this test; none of it is derived from a
/// real export.
fn owner_decisions_fixture() -> (SqliteStore, OwnerId, AccountId, uuid::Uuid) {
    let (mut store, owner, account, _contour) = populated();
    let partner = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: partner,
            owner,
            title: "Savings".into(),
            institution: None,
        })
        .unwrap();

    store
        .replace_account_aliases(
            owner,
            account,
            &[AccountAliasRecord {
                value: "**** 0001".into(),
                interval: AliasInterval {
                    valid_from: date!(2026 - 01 - 01),
                    valid_to: None,
                },
            }],
        )
        .unwrap();

    store
        .record_account_scope_exclusion(
            owner,
            &AccountScopeExclusionRecord {
                account: partner,
                reason: "a gift card, not really his money".into(),
            },
        )
        .unwrap();

    store
        .record_account_transfer_statement(owner, account, &[partner])
        .unwrap();

    // Two revisions: a retirement, then its withdrawal — the append-only
    // history a round trip must keep both rows of.
    store
        .record_account_retirement(
            owner,
            &AccountRetirement {
                account: partner,
                effective_on: date!(2026 - 06 - 01),
            },
        )
        .unwrap();
    store.withdraw_account_retirement(owner, partner).unwrap();

    store
        .decline_account_name(owner, "SOME BANK CARD ****9999", "not one of his cards")
        .unwrap();

    store
        .record_decision(
            owner,
            Some(PrincipalId::new_random()),
            "test_decision",
            "invented-subject",
            &serde_json::json!({"invented": true}),
            "undo_test_decision",
        )
        .unwrap();

    let group = store
        .insert_category_group_of_kind(owner, "Groceries", false)
        .unwrap();
    let category = store.insert_category(owner, group, "Coffee").unwrap();

    let rule = store
        .insert_rule(owner, invented_classification_rule())
        .unwrap();
    store
        .amend_rule(owner, rule.id, invented_classification_rule())
        .unwrap();

    let category_rule = store
        .insert_category_rule(
            owner,
            NewCategoryRule {
                matcher: CategoryMatcher::DescriptionContains {
                    text: "Coffee".to_owned(),
                },
                category,
                valid_from: None,
                valid_to: None,
            },
            None,
        )
        .unwrap();
    store
        .amend_category_rule(
            owner,
            category_rule.id,
            NewCategoryRule {
                matcher: CategoryMatcher::DescriptionContains {
                    text: "Coffee Shop".to_owned(),
                },
                category,
                valid_from: None,
                valid_to: None,
            },
        )
        .unwrap();

    let matching_event = Event {
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"8".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        )
        .with_description("Coffee Shop purchase"),
        ..deposit(owner, account, 3, 5_000)
    };
    store
        .append_event(&matching_event, IdentityScope::Source)
        .unwrap();
    let decomposed = store.rebuild_category_index(owner).unwrap();
    assert_eq!(
        decomposed, 1,
        "the fixture event must actually match the active category rule, or this test proves \
         nothing about event_category_assignments"
    );

    (store, owner, account, category)
}

#[test]
fn the_owners_decisions_round_trip_into_a_genuinely_empty_database() {
    let (source, owner, _account, _category) = owner_decisions_fixture();
    let bundle = source.export_bundle(owner).unwrap();

    assert_eq!(bundle.account_aliases.len(), 1);
    assert_eq!(bundle.account_scope_exclusions.len(), 1);
    assert_eq!(bundle.account_transfers.len(), 1);
    assert_eq!(
        bundle.account_retirements.len(),
        2,
        "both the retirement and its withdrawal must travel — the append-only history, not \
         only the state in force"
    );
    assert_eq!(bundle.declined_account_names.len(), 1);
    assert_eq!(bundle.decision_history.len(), 1);
    assert_eq!(bundle.category_groups.len(), 1);
    assert_eq!(bundle.categories.len(), 1);
    assert_eq!(
        bundle.classification_rules.len(),
        2,
        "the original rule and the one that replaced it — retirement does not delete a row"
    );
    assert_eq!(bundle.category_rules.len(), 2);
    assert_eq!(bundle.event_category_assignments.len(), 1);

    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored
        .import_bundle(&bundle)
        .expect("the owner's decisions must restore into a genuinely empty database");

    let back = restored.export_bundle(owner).unwrap();
    assert_eq!(back.account_aliases, bundle.account_aliases);
    assert_eq!(
        back.account_scope_exclusions,
        bundle.account_scope_exclusions
    );
    assert_eq!(back.account_transfers, bundle.account_transfers);
    assert_eq!(back.account_retirements, bundle.account_retirements);
    assert_eq!(back.declined_account_names, bundle.declined_account_names);
    assert_eq!(back.decision_history, bundle.decision_history);
    assert_eq!(back.category_groups, bundle.category_groups);
    assert_eq!(back.categories, bundle.categories);
    assert_eq!(back.classification_rules, bundle.classification_rules);
    assert_eq!(back.category_rules, bundle.category_rules);
    assert_eq!(
        back.event_category_assignments,
        bundle.event_category_assignments
    );
}

#[test]
fn a_retired_classification_rule_is_not_live_after_a_round_trip() {
    // The failure this exists to catch: dropping `retired_at` on restore
    // would leave both the original and its replacement active, and a
    // classification computed afterward would see two rules answer to one
    // decision instead of one.
    let (source, owner, _account, _category) = owner_decisions_fixture();
    let bundle = source.export_bundle(owner).unwrap();
    let retired = bundle
        .classification_rules
        .iter()
        .find(|rule| rule.retired_at.is_some())
        .expect("amend_rule must have retired the original rule");
    assert!(retired.replaces.is_none());
    let replacement = bundle
        .classification_rules
        .iter()
        .find(|rule| rule.replaces == Some(retired.id))
        .expect("the amended rule must name the one it replaces");
    assert!(replacement.retired_at.is_none());

    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();
    let active = restored.list_active_rules(owner).unwrap();
    assert_eq!(
        active.len(),
        1,
        "exactly one rule must be active after the restore, matching the source"
    );
    assert_eq!(active[0].id, ClassificationRuleId(replacement.id));
}

#[test]
fn a_withdrawn_account_retirement_is_not_in_force_after_a_round_trip() {
    // The failure this exists to catch: if only the "in force" state
    // travelled, this fixture's account has none — the retirement was
    // withdrawn — so the case worth proving is that a round trip does not
    // resurrect the withdrawn retirement by dropping the second revision.
    let (source, owner, _account, _category) = owner_decisions_fixture();
    let bundle = source.export_bundle(owner).unwrap();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();

    let in_force = restored.list_account_retirements(owner).unwrap();
    assert!(
        in_force.statements.is_empty(),
        "the retirement was withdrawn in the source; it must not be in force after restore"
    );
    assert_eq!(
        in_force.revision.0, 2,
        "the revision counter must reflect both rows the history carried, not just the one \
         in force"
    );
}

#[test]
fn declined_account_names_and_decision_history_round_trip() {
    let (source, owner, _account, _category) = owner_decisions_fixture();
    let bundle = source.export_bundle(owner).unwrap();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();

    let declined = restored.list_declined_account_names(owner).unwrap();
    assert_eq!(declined.len(), 1);
    assert_eq!(declined[0].printed, "SOME BANK CARD ****9999");

    let decisions = restored.list_decisions(owner, None, None).unwrap();
    assert!(
        decisions
            .iter()
            .any(|decision| decision.subject == "invented-subject"),
        "the recorded decision must be readable back through the ordinary decision API"
    );
}

#[test]
fn importing_the_owners_decisions_twice_changes_nothing() {
    let (source, owner, _account, _category) = owner_decisions_fixture();
    let bundle = source.export_bundle(owner).unwrap();
    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();
    restored.import_bundle(&bundle).unwrap();

    let back = restored.export_bundle(owner).unwrap();
    assert_eq!(back.account_aliases, bundle.account_aliases);
    assert_eq!(back.account_retirements, bundle.account_retirements);
    assert_eq!(
        back.decision_history.len(),
        1,
        "decision_history has no natural key beyond the id this section does not carry — a \
         repeat import must not duplicate the row"
    );
    assert_eq!(back.classification_rules, bundle.classification_rules);
    assert_eq!(back.category_rules, bundle.category_rules);
}

#[test]
fn an_archive_written_before_the_owners_decisions_existed_still_verifies_and_restores() {
    // The same pre-existing archive `an_archive_written_before_the_reference_sections_still_verifies`
    // reads, extended one claim further: it must not merely deserialize —
    // `import_bundle` must actually restore it, exactly as it did before any
    // of the sections this task added existed.
    let bundle: Bundle = serde_json::from_str(ARCHIVE_WITHOUT_REFERENCE_SECTIONS)
        .expect("an old archive still reads");
    assert!(bundle.classification_rules.is_empty());
    assert!(bundle.category_groups.is_empty());
    assert!(bundle.categories.is_empty());
    assert!(bundle.category_rules.is_empty());
    assert!(bundle.event_category_assignments.is_empty());
    assert!(bundle.account_scope_exclusions.is_empty());
    assert!(bundle.account_transfers.is_empty());
    assert!(bundle.account_retirements.is_empty());
    assert!(bundle.account_aliases.is_empty());
    assert!(bundle.declined_account_names.is_empty());
    assert!(bundle.decision_history.is_empty());
    assert_eq!(bundle.checksum, bundle.compute_checksum());

    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored
        .import_bundle(&bundle)
        .expect("an archive written before the owner's decisions travelled must still restore");
    assert_eq!(restored.load_events(bundle.owner).unwrap().len(), 1);
}
