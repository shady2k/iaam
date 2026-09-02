//! Snapshots, reference data, and contour versions.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::CustodyId;
use iaam_core::ids::{AccountId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::projection::lots::LotKey;
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_store::SqliteStore;
use iaam_store::reference::{AccountRecord, InstrumentRecord};
use iaam_store::tokens::{TokenRecord, TokenScope};
use time::macros::date;
use uuid::Uuid;

fn deposit(owner: OwnerId, account: AccountId, sequence: u32, minor: i64) -> Event {
    let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
    let day = date!(2026 - 02 - 01);
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::CashIn { amount },
        dates: EventDates::for_cash(CashPostedDate(day)),
        order: EffectiveOrder::new(day, sequence),
        legs: vec![Leg::cash(account, amount)],
        provenance: Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"3".repeat(64)).unwrap(),
            ParserVersion("manual/1".into()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}

fn purchase(owner: OwnerId, account: AccountId, instrument: InstrumentId) -> Event {
    let amount = Money::new(PostedMinor::new(100_000), CurrencyCode::Rub);
    let mut event = deposit(owner, account, 1, amount.amount().raw());
    event.kind = EventKind::Trade {
        side: iaam_core::event::kind::TradeSide::Buy,
        instrument,
        quantity: iaam_core::money::Quantity(Dec::new(10_i64.into())),
        gross: amount,
        fee: None,
        accrued_interest: None,
        basis_fee: None,
        basis_fee_exact: None,
    };
    event.legs = vec![
        Leg::cash(
            account,
            Money::new(PostedMinor::new(-100_000), CurrencyCode::Rub),
        ),
        Leg::security(
            account,
            CustodyId::new_random(),
            instrument,
            iaam_core::money::Quantity(Dec::new(10_i64.into())),
        ),
    ];
    event
}

/// Remove a field from a snapshot represented as a CBOR value.
///
/// Specifically CBOR, not JSON: state maps have composite keys, and
/// `serde_json` cannot handle them—the attempt to pass the snapshot through JSON fails
/// with “key must be a string”. That is why the project stores snapshots in CBOR
/// (see the comment for the `ciborium` dependency in `iaam-core/Cargo.toml`).
fn strip_acquisition_basis(value: &mut ciborium::value::Value) {
    match value {
        ciborium::value::Value::Map(entries) => {
            entries.retain(|(key, _)| key.as_text() != Some("acquisition_basis"));
            for (_, child) in entries.iter_mut() {
                strip_acquisition_basis(child);
            }
        }
        ciborium::value::Value::Array(values) => {
            for child in values {
                strip_acquisition_basis(child);
            }
        }
        _ => {}
    }
}

#[test]
fn a_snapshot_survives_a_write_and_a_read() {
    // The state contains maps with composite keys: JSON cannot handle them,
    // so the snapshot is stored in CBOR. The test catches a regression to JSON.
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let events = vec![
        deposit(owner, account, 1, 100_000),
        deposit(owner, account, 2, 50_000),
    ];
    let snapshot = project(&events, &ctx).unwrap().into_snapshot();

    store.save_snapshot(owner, &snapshot).unwrap();
    let loaded = store
        .load_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
        .unwrap()
        .expect("snapshot found");

    assert_eq!(loaded.fingerprint(), snapshot.fingerprint());
    assert_eq!(
        loaded.state().balances().cash(account, CurrencyCode::Rub),
        snapshot.state().balances().cash(account, CurrencyCode::Rub)
    );
    assert_eq!(loaded, snapshot);
}

#[test]
fn a_projection_snapshot_written_before_acquisition_basis_loads() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let snapshot = project(&[purchase(owner, account, instrument)], &ctx)
        .unwrap()
        .into_snapshot();

    store.save_snapshot(owner, &snapshot).unwrap();
    let mut legacy = ciborium::value::Value::serialized(&snapshot).unwrap();
    strip_acquisition_basis(&mut legacy);
    let mut body = Vec::new();
    ciborium::into_writer(&legacy, &mut body).unwrap();
    store
        .connection()
        .execute(
            "UPDATE snapshots SET body = ?1 WHERE owner = ?2",
            rusqlite::params![body, owner.inner().to_string()],
        )
        .unwrap();

    let loaded = store
        .load_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
        .unwrap()
        .expect("old snapshot found");
    let entry = loaded
        .state()
        .book()
        .entry(&LotKey {
            account,
            instrument,
        })
        .expect("lot restored");
    assert_eq!(entry.lots()[0].acquisition_basis, None);
    assert_eq!(
        entry.lots()[0].cost_basis,
        Money::new(PostedMinor::new(100_000), CurrencyCode::Rub)
    );
}

#[test]
fn saving_a_snapshot_twice_replaces_it() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let first = project(&[deposit(owner, account, 1, 100_000)], &ctx)
        .unwrap()
        .into_snapshot();
    let second = project(
        &[
            deposit(owner, account, 1, 100_000),
            deposit(owner, account, 2, 1),
        ],
        &ctx,
    )
    .unwrap()
    .into_snapshot();

    store.save_snapshot(owner, &first).unwrap();
    store.save_snapshot(owner, &second).unwrap();
    let loaded = store
        .load_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
        .unwrap()
        .unwrap();
    assert_eq!(loaded.fingerprint(), second.fingerprint());
}

#[test]
fn a_contour_version_cannot_be_edited_in_place() {
    // Changing the contour composition retroactively would silently rewrite
    // historical returns (§4.10).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Brokerage".into(),
            institution: None,
        })
        .unwrap();
    store
        .insert_contour_version(owner, &contour, "My portfolio", &[account])
        .unwrap();

    let update = store
        .connection()
        .execute("UPDATE contour_accounts SET account = 'replacement'", []);
    assert!(
        update.is_err(),
        "UPDATE contour composition must be rejected"
    );

    let loaded = store
        .load_contour(owner, contour.id(), ContourVersion(1))
        .unwrap()
        .unwrap();
    assert!(loaded.contains(account));
    assert_eq!(
        store.latest_contour_version(owner, contour.id()).unwrap(),
        Some(ContourVersion(1))
    );
}
#[test]
fn list_contours_returns_latest_version_per_owner_and_contour() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let other_owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourId::new_random();
    let other_contour = ContourId::new_random();

    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Main".into(),
            institution: None,
        })
        .unwrap();

    for version in [1, 2] {
        let definition = ContourDefinition::new(contour, ContourVersion(version), [account]);
        store
            .insert_contour_version(owner, &definition, "Main", &[account])
            .unwrap();
    }
    let empty = ContourDefinition::new(other_contour, ContourVersion(1), []);
    store
        .insert_contour_version(owner, &empty, "Savings", &[])
        .unwrap();
    let foreign = ContourDefinition::new(contour, ContourVersion(7), []);
    store
        .insert_contour_version(other_owner, &foreign, "Main", &[])
        .unwrap();

    let listed = store.list_contours(owner).unwrap();

    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .any(|entry| entry.id == contour && entry.version == ContourVersion(2))
    );
    assert!(
        listed
            .iter()
            .any(|entry| entry.id == other_contour && entry.version == ContourVersion(1))
    );
    let foreign_list = store.list_contours(other_owner).unwrap();
    assert_eq!(foreign_list.len(), 1);
    assert_eq!(foreign_list[0].id, foreign.id());
    assert_eq!(foreign_list[0].version, ContourVersion(7));
}

#[test]
fn accounts_round_trip() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = AccountRecord {
        id: AccountId::new_random(),
        owner,
        title: "Brokerage".into(),
        institution: Some("T-Bank".into()),
    };
    store.upsert_account(&record).unwrap();
    assert_eq!(store.list_accounts(owner).unwrap(), vec![record]);
}

#[test]
fn a_revoked_token_is_not_found() {
    let store = SqliteStore::open_in_memory().unwrap();
    let record = TokenRecord {
        id: Uuid::new_v4(),
        owner: OwnerId::new_random(),
        label: "agent".into(),
        scope: TokenScope::Agent,
        revoked: false,
    };
    store.insert_token(&record, "token-hash").unwrap();
    assert_eq!(
        store.find_token("token-hash").unwrap(),
        Some(record.clone())
    );

    store.revoke_token(record.owner, record.id).unwrap();
    assert_eq!(store.find_token("token-hash").unwrap(), None);
}

#[test]
fn an_agent_token_may_submit_but_not_administer() {
    assert!(TokenScope::Agent.may_submit());
    assert!(!TokenScope::Agent.may_administer());
    assert!(!TokenScope::ReadOnly.may_submit());
    assert!(TokenScope::Owner.may_administer());
}

#[test]
fn a_contour_cannot_include_an_account_of_another_owner() {
    // A contour containing someone else's accounts is access to someone else's money, not an input error.
    // The database rejects it via the foreign key (owner, account) (§14).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let foreign_account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: foreign_account,
            owner: stranger,
            title: "Someone else's".into(),
            institution: None,
        })
        .unwrap();

    let contour = ContourId::new_random();
    let attempt = store.insert_contour_version(
        owner,
        &ContourDefinition::new(contour, ContourVersion(1), [foreign_account]),
        "Someone else's money",
        &[foreign_account],
    );
    assert!(
        attempt.is_err(),
        "a foreign account in the contour must be rejected"
    );
}

#[test]
fn a_contour_of_another_owner_is_not_found() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Own".into(),
            institution: None,
        })
        .unwrap();
    let contour = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(1), [account]),
            "Mine",
            &[account],
        )
        .unwrap();

    // Knowing the identifier does not grant access.
    assert_eq!(
        store
            .load_contour(stranger, contour, ContourVersion(1))
            .unwrap(),
        None
    );
    assert_eq!(
        store.latest_contour_version(stranger, contour).unwrap(),
        None
    );
}

#[test]
fn an_account_of_another_owner_is_not_overwritten() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let id = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id,
            owner,
            title: "My account".into(),
            institution: None,
        })
        .unwrap();
    // The same identifier, a different owner: the row must not change.
    let attempt = store.upsert_account(&AccountRecord {
        id,
        owner: stranger,
        title: "Taken over".into(),
        institution: None,
    });
    assert!(attempt.is_ok(), "the conflict must not be a write error");
    assert_eq!(store.list_accounts(owner).unwrap()[0].title, "My account");
    assert!(store.list_accounts(stranger).unwrap().is_empty());
}

#[test]
fn a_snapshot_of_another_owner_is_not_found() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let snapshot = project(&[deposit(owner, account, 1, 1_000)], &ctx)
        .unwrap()
        .into_snapshot();
    store.save_snapshot(owner, &snapshot).unwrap();

    assert!(
        store
            .load_snapshot(stranger, contour.id(), contour.version(), LotRuleVersion(1))
            .unwrap()
            .is_none()
    );
}

#[test]
fn dropping_a_snapshot_actually_removes_it() {
    // The only deletion in the store must actually delete. A method that silently
    // returns success leaves a stale cache and makes the next
    // `advance` advance a snapshot that was meant to be discarded.
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let ctx = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let snapshot = project(&[deposit(owner, account, 1, 100_000)], &ctx)
        .unwrap()
        .into_snapshot();
    store.save_snapshot(owner, &snapshot).unwrap();
    assert!(
        store
            .load_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
            .unwrap()
            .is_some()
    );

    store
        .drop_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
        .unwrap();
    assert!(
        store
            .load_snapshot(owner, contour.id(), contour.version(), LotRuleVersion(1))
            .unwrap()
            .is_none(),
        "the snapshot must disappear, not remain as a stale cache"
    );
}

#[test]
fn an_upserted_instrument_reaches_the_table() {
    // The reference is read by the acceptance test, not this crate, so
    // the check uses a direct query: without it, the instrument write could
    // be replaced with a silent success, and the acceptance test would stop finding
    // the instrument that was “already added”.
    let store = SqliteStore::open_in_memory().unwrap();
    let instrument = InstrumentRecord {
        id: InstrumentId::new_random(),
        kind: Some(InstrumentKind::Share),
        symbol: "SBER".into(),
        title: "Sberbank common shares".into(),
        currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
        lineage: None,
    };
    store.upsert_instrument(&instrument).unwrap();

    let symbol: String = store
        .connection()
        .query_row(
            "SELECT symbol FROM instruments WHERE id = ?1",
            [instrument.id.inner().to_string()],
            |row| row.get(0),
        )
        .expect("instrument found in the table");
    assert_eq!(symbol, "SBER");

    // A repeated call updates rather than creates a duplicate.
    let renamed = InstrumentRecord {
        title: "Sberbank Russia ordinary shares".into(),
        ..instrument
    };
    store.upsert_instrument(&renamed).unwrap();
    let count: u32 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM instruments", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn every_use_of_a_token_is_recorded_including_the_rejected_one() {
    // The usage log exists specifically for rejected attempts (§14).
    // A method that returns success without recording them leaves them invisible.
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = TokenRecord {
        id: Uuid::new_v4(),
        owner,
        label: "agent".into(),
        scope: TokenScope::Agent,
        revoked: false,
    };
    store.insert_token(&record, "hash-1").unwrap();

    store
        .record_token_use("hash-1", "/v1/returns", "ok")
        .unwrap();
    store
        .record_token_use("hash-1", "/v1/tokens", "forbidden")
        .unwrap();

    let outcomes: Vec<String> = store
        .connection()
        .prepare("SELECT outcome FROM token_usage WHERE token = ?1 ORDER BY route")
        .unwrap()
        .query_map(["hash-1"], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(outcomes, vec!["ok".to_string(), "forbidden".to_string()]);
}

#[test]
fn every_token_scope_survives_a_round_trip_through_its_code() {
    // The token scope is stored as a string. A parser that drops
    // a branch would silently turn the owner into “unknown”—or, worse,
    // the reader into an agent if the branches were swapped.
    for scope in [TokenScope::Owner, TokenScope::Agent, TokenScope::ReadOnly] {
        assert_eq!(TokenScope::parse(scope.code()), Some(scope));
    }
    assert_eq!(TokenScope::Owner.code(), "owner");
    assert_eq!(TokenScope::Agent.code(), "agent");
    assert_eq!(TokenScope::ReadOnly.code(), "read_only");
    assert_eq!(TokenScope::parse("administrator"), None);
    assert_eq!(TokenScope::parse(""), None);
}
