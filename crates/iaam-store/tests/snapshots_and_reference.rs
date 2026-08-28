//! Снимки, справочники и версии контуров.

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

/// Убрать поле из снимка, представленного значением CBOR.
///
/// Именно CBOR, а не JSON: карты состояния имеют составные ключи, и
/// `serde_json` их не берёт — попытка пройти снимок через JSON падает
/// с «key must be a string». Ради этого проект и хранит снимки в CBOR
/// (см. комментарий к зависимости `ciborium` в `iaam-core/Cargo.toml`).
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
    // Состояние содержит карты с составными ключами: JSON их не берёт,
    // поэтому снимок хранится в CBOR. Тест ловит возврат к JSON.
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
        .expect("снимок найден");

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
        .expect("старый снимок найден");
    let entry = loaded
        .state()
        .book()
        .entry(&LotKey {
            account,
            instrument,
        })
        .expect("лот восстановлен");
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
    // Изменение состава контура задним числом молча переписало бы
    // историческую доходность (§4.10).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Брокерский".into(),
            institution: None,
        })
        .unwrap();
    store
        .insert_contour_version(owner, &contour, "Мой портфель", &[account])
        .unwrap();

    let update = store
        .connection()
        .execute("UPDATE contour_accounts SET account = 'подмена'", []);
    assert!(
        update.is_err(),
        "UPDATE состава контура обязан быть отклонён"
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
fn accounts_round_trip() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = AccountRecord {
        id: AccountId::new_random(),
        owner,
        title: "Брокерский".into(),
        institution: Some("Т-Банк".into()),
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
        label: "агент".into(),
        scope: TokenScope::Agent,
        revoked: false,
    };
    store.insert_token(&record, "хеш-токена").unwrap();
    assert_eq!(
        store.find_token("хеш-токена").unwrap(),
        Some(record.clone())
    );

    store.revoke_token(record.owner, record.id).unwrap();
    assert_eq!(store.find_token("хеш-токена").unwrap(), None);
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
    // Контур из чужих счетов — это доступ к чужим деньгам, а не ошибка
    // ввода. Отказывает база по внешнему ключу (owner, account) (§14).
    let mut store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let foreign_account = AccountId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: foreign_account,
            owner: stranger,
            title: "Чужой".into(),
            institution: None,
        })
        .unwrap();

    let contour = ContourId::new_random();
    let attempt = store.insert_contour_version(
        owner,
        &ContourDefinition::new(contour, ContourVersion(1), [foreign_account]),
        "Чужие деньги",
        &[foreign_account],
    );
    assert!(
        attempt.is_err(),
        "чужой счёт в контуре обязан быть отклонён"
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
            title: "Свой".into(),
            institution: None,
        })
        .unwrap();
    let contour = ContourId::new_random();
    store
        .insert_contour_version(
            owner,
            &ContourDefinition::new(contour, ContourVersion(1), [account]),
            "Мой",
            &[account],
        )
        .unwrap();

    // Знание идентификатора не даёт доступа.
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
            title: "Мой счёт".into(),
            institution: None,
        })
        .unwrap();
    // Тот же идентификатор, другой владелец: строка не должна измениться.
    let attempt = store.upsert_account(&AccountRecord {
        id,
        owner: stranger,
        title: "Захвачено".into(),
        institution: None,
    });
    assert!(attempt.is_ok(), "конфликт не должен быть ошибкой записи");
    assert_eq!(store.list_accounts(owner).unwrap()[0].title, "Мой счёт");
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
    // Единственное удаление в хранилище обязано удалять. Метод, молча
    // возвращающий успех, оставляет протухший кэш и делает следующий
    // `advance` продвижением снимка, который решили выбросить.
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
        "снимок обязан исчезнуть, а не остаться протухшим кэшем"
    );
}

#[test]
fn an_upserted_instrument_reaches_the_table() {
    // Справочник читается задачей приёмки, а не этой крейтой, поэтому
    // проверка идёт прямым запросом: без неё запись инструмента можно
    // заменить на молчаливый успех, и приёмка перестала бы находить
    // инструмент, который «уже добавили».
    let store = SqliteStore::open_in_memory().unwrap();
    let instrument = InstrumentRecord {
        id: InstrumentId::new_random(),
        kind: Some(InstrumentKind::Share),
        symbol: "SBER".into(),
        title: "Сбербанк, обыкновенные".into(),
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
        .expect("инструмент найден в таблице");
    assert_eq!(symbol, "SBER");

    // Повторный вызов обновляет, а не задваивает.
    let renamed = InstrumentRecord {
        title: "Сбербанк России, ао".into(),
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
    // Журнал использования нужен ровно ради отклонённых попыток (§14).
    // Метод, возвращающий успех без записи, оставляет их невидимыми.
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = TokenRecord {
        id: Uuid::new_v4(),
        owner,
        label: "агент".into(),
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
    // Область действия токена хранится строкой. Разбор, потерявший
    // ветку, молча превратил бы владельца в «неизвестно» — или, хуже,
    // читателя в агента, если бы ветки перепутались.
    for scope in [TokenScope::Owner, TokenScope::Agent, TokenScope::ReadOnly] {
        assert_eq!(TokenScope::parse(scope.code()), Some(scope));
    }
    assert_eq!(TokenScope::Owner.code(), "owner");
    assert_eq!(TokenScope::Agent.code(), "agent");
    assert_eq!(TokenScope::ReadOnly.code(), "read_only");
    assert_eq!(TokenScope::parse("administrator"), None);
    assert_eq!(TokenScope::parse(""), None);
}
