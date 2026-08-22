//! Архивный бандл: экспорт, импорт, повреждение.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_store::SqliteStore;
use iaam_store::bundle::ImportOutcome;
use iaam_store::reference::AccountRecord;
use time::macros::date;

fn deposit(owner: OwnerId, account: AccountId, sequence: u32, minor: i64) -> Event {
    let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
    let day = date!(2026 - 05 - 05);
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
            title: "Брокерский".into(),
            institution: Some("Т-Банк".into()),
        })
        .unwrap();
    let contour = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(1), [account]),
            "Мой портфель",
            &[account],
        )
        .unwrap();
    store
        .append_event(&deposit(owner, account, 1, 100_000))
        .unwrap();
    store
        .append_event(&deposit(owner, account, 2, 250_000))
        .unwrap();
    (store, owner, account, contour)
}

#[test]
fn a_bundle_restores_a_complete_working_state() {
    // Экспорт одних событий не является бэкапом: из него получатся
    // другие проекции, потому что состав контуров останется снаружи.
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
    // Повреждённый архив хуже отсутствующего: он выглядит как целый.
    let (source, owner, _, _) = populated();
    let mut bundle = source.export_bundle(owner).unwrap();
    bundle.events.truncate(1);
    let mut restored = SqliteStore::open_in_memory().unwrap();
    assert!(restored.import_bundle(&bundle).is_err());
}

#[test]
fn a_changed_amount_breaks_the_checksum() {
    // Первая редакция суммы хешировала только идентификаторы событий:
    // подменённая денежная величина проходила проверку, и архив
    // с неверными суммами выглядел целым.
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
        "подменённая сумма обязана ломать контрольную сумму"
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
    // Ничего не записалось: импорт идёт одной транзакцией.
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
    // Бандл — переносимый архив: он обязан пережить текстовый формат.
    let (source, owner, _, _) = populated();
    let bundle = source.export_bundle(owner).unwrap();
    let json = serde_json::to_string(&bundle).unwrap();
    let back: iaam_store::bundle::Bundle = serde_json::from_str(&json).unwrap();
    assert_eq!(back, bundle);
}

#[test]
fn versions_of_a_contour_are_exported_as_separate_sections_with_all_their_accounts() {
    // Состав контура выгружается построчно: одна строка на счёт. Сборка
    // строк в секции обязана различать И контур, И версию — иначе счета
    // разных версий сливаются в одну, и восстановленный контур получает
    // состав, которого у него никогда не было. Цифры отчёта после такого
    // восстановления выглядят нормально и являются неверными.
    let (mut store, owner, first_account, contour) = populated();

    let second_account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: second_account,
            owner,
            title: "Второй брокерский".into(),
            institution: None,
        })
        .unwrap();

    // Версия 2 того же контура: счёт добавлен.
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(2), [first_account, second_account]),
            "Мой портфель",
            &[first_account, second_account],
        )
        .unwrap();

    // И отдельный контур из одного счёта.
    let other = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(other, ContourVersion(1), [second_account]),
            "Только второй",
            &[second_account],
        )
        .unwrap();

    let bundle = store.export_bundle(owner).unwrap();
    assert_eq!(
        bundle.contours.len(),
        3,
        "две версии первого контура и одна второго — три секции, а не меньше"
    );

    let section = |id: ContourId, version: u32| {
        bundle
            .contours
            .iter()
            .find(|section| section.contour == id.0 && section.version == version)
            .unwrap_or_else(|| panic!("секция {id:?} версии {version} не найдена"))
    };
    assert_eq!(section(contour, 1).accounts, vec![first_account.inner()]);
    assert_eq!(section(contour, 2).accounts.len(), 2);
    assert!(
        section(contour, 2)
            .accounts
            .contains(&second_account.inner())
    );
    assert_eq!(section(other, 1).accounts, vec![second_account.inner()]);

    // И всё это переживает круг через архив.
    let mut restored = SqliteStore::open_in_memory().unwrap();
    restored.import_bundle(&bundle).unwrap();
    let back = restored.export_bundle(owner).unwrap();
    assert_eq!(back.contours.len(), 3);
    let version_two = restored
        .load_contour(owner, contour, ContourVersion(2))
        .unwrap()
        .expect("версия 2 восстановлена");
    assert!(version_two.contains(first_account));
    assert!(version_two.contains(second_account));

    let version_one = restored
        .load_contour(owner, contour, ContourVersion(1))
        .unwrap()
        .expect("версия 1 восстановлена");
    assert!(version_one.contains(first_account));
    assert!(
        !version_one.contains(second_account),
        "счёт версии 2 не должен протечь в версию 1"
    );
}
