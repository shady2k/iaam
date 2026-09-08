//! Archived bundle: export, import, corruption.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{EventKind, IncomeKind};
use iaam_core::event::leg::Leg;
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RuleSettlement};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, CustodyId, EventId, ImportSessionId, InstrumentId, OwnerId,
    SourceId,
};
use iaam_core::instrument::CurrencyRoles;
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::evidence::IdentityScope;
use iaam_store::SqliteStore;
use iaam_store::bundle::ImportOutcome;
use iaam_store::reference::{AccountRecord, CustodyRecord, InstrumentRecord};
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
/// (relational journal design §4.2) — unlike `import_session` or
/// `settled_by_rule`, they are not exempted because the bundle carries no
/// reference-data section at all (design §4.1: "events, accounts and
/// contours and nothing else"). So a fact naming an instrument or custody
/// place is only writable where that reference data already exists — on
/// export, because it was registered before the fact was; on import, the
/// same way an independent market-data sync would have populated it before
/// a real restore. Both stores this test writes to must register this data
/// themselves; the bundle does not carry it for them.
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
    // The bundle carries events, accounts and contours and nothing else
    // (design §4.1) — the target of a restore must already hold the
    // instrument and custody reference data its events' legs point to, the
    // way an independent market-data sync would have populated it.
    refs.register_in(&restored, owner);
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
