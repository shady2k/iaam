//! Порт хранилища поверх `iaam-store`.
//!
//! Здесь и только здесь пересекается граница async/blocking (§3.2).
//! `rusqlite` блокирует поток; вызов его прямо из обработчика `axum`
//! останавливает исполнитель, поэтому каждая операция уходит в
//! `spawn_blocking`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use iaam_broker::credentials::{BrokerScope, Key, seal};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use iaam_store::SqliteStore;
use iaam_store::broker_access::{NewBrokerAccess, SoleOwner as StoredSoleOwner};
use iaam_store::documents::BrokerCode;
use iaam_store::events::Appended;
use iaam_store::reference::AccountRecord;
use iaam_store::tokens::{TokenRecord, TokenScope};
use time::Date;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::AppError;
use crate::ports::{
    AccountView, BrokerAccessView, BrokerVault, IssuedToken, Principal, Recorded, Scope, SoleOwner,
    Store, TokenAdmin, TokenView,
};
use crate::tokens::{hash_token, secret_hex};

/// Соединение под мьютексом: `rusqlite::Connection` не `Sync`, а писатель
/// у однопользовательской базы один. Пул появится тогда, когда появится
/// второй писатель, а не раньше.
pub struct SqliteAdapter {
    store: Arc<Mutex<SqliteStore>>,
    /// Ключ шифрования брокерских доступов. Живёт вне базы и потому
    /// приходит извне, а не из неё. `None` — ключ не задан настройкой,
    /// и тогда брокерские доступы не заводятся и не читаются: заводить
    /// их «пока без шифрования» означало бы положить чужой токен
    /// открытым и обнаружить это по утечке базы (§14).
    broker_key: Option<Key>,
}

impl SqliteAdapter {
    #[must_use]
    pub fn new(store: SqliteStore) -> Self {
        Self::with_broker_key(store, None)
    }

    /// Тот же адаптер с ключом шифрования брокерских доступов.
    #[must_use]
    pub fn with_broker_key(store: SqliteStore, key: Option<Key>) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            broker_key: key,
        }
    }

    /// Ключ или отказ.
    ///
    /// Отказ отдельным вариантом, а не `Store`: отсутствие ключа —
    /// это незаконченная настройка сервера, а не сбой хранилища, и
    /// повтор запроса её не исправит.
    fn key(&self) -> Result<&Key, AppError> {
        self.broker_key.as_ref().ok_or(AppError::NotConfigured {
            what: "шифрование доступа к брокеру",
        })
    }

    /// Выполнение блокирующей операции.
    ///
    /// Отравленный мьютекс восстанавливается, а не приводит к панике:
    /// паника в одном запросе не должна выводить из строя весь сервис,
    /// а состояние `SqliteStore` — это соединение, которое паника
    /// предыдущего вызова не повреждает.
    async fn blocking<T, F>(&self, work: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut SqliteStore) -> Result<T, AppError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut guard = match store.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            work(&mut guard)
        })
        .await
        .map_err(|error| AppError::Store(format!("блокирующая задача не выполнена: {error}")))?
    }
}

fn store_error(error: iaam_store::StoreError) -> AppError {
    AppError::Store(error.to_string())
}

/// Права токена: из хранилища в порт.
///
/// Перевод в обе стороны исчерпывающим `match`, а не по строке-коду:
/// новое право обязано сломать сборку здесь, а не молча превратиться
/// в «читатель» при разборе неизвестного кода (§15.1).
const fn scope_from_store(scope: TokenScope) -> Scope {
    match scope {
        TokenScope::Owner => Scope::Owner,
        TokenScope::Agent => Scope::Agent,
        TokenScope::ReadOnly => Scope::ReadOnly,
    }
}

const fn scope_to_store(scope: Scope) -> TokenScope {
    match scope {
        Scope::Owner => TokenScope::Owner,
        Scope::Agent => TokenScope::Agent,
        Scope::ReadOnly => TokenScope::ReadOnly,
    }
}

#[async_trait]
impl Store for SqliteAdapter {
    async fn append_events(&self, events: Vec<Event>) -> Result<Vec<Recorded>, AppError> {
        self.blocking(move |store| {
            let mut recorded = Vec::with_capacity(events.len());
            for event in &events {
                let outcome = store.append_event_in_order(event).map_err(store_error)?;
                recorded.push(match outcome {
                    Appended::Inserted { id } => Recorded::Inserted { id },
                    Appended::Duplicate { existing } => Recorded::Duplicate { existing },
                });
            }
            Ok(recorded)
        })
        .await
    }

    async fn load_events_through(
        &self,
        owner: OwnerId,
        through: Date,
    ) -> Result<Vec<Event>, AppError> {
        self.blocking(move |store| {
            store
                .load_events_through(owner, through)
                .map_err(store_error)
        })
        .await
    }

    async fn load_contour(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
    ) -> Result<Option<ContourDefinition>, AppError> {
        self.blocking(move |store| {
            store
                .load_contour(owner, contour, version)
                .map_err(store_error)
        })
        .await
    }

    async fn latest_contour_version(
        &self,
        owner: OwnerId,
        contour: ContourId,
    ) -> Result<Option<ContourVersion>, AppError> {
        self.blocking(move |store| {
            store
                .latest_contour_version(owner, contour)
                .map_err(store_error)
        })
        .await
    }

    async fn insert_contour_version(
        &self,
        owner: OwnerId,
        definition: ContourDefinition,
        title: String,
        accounts: Vec<AccountId>,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .insert_contour_version(owner, &definition, &title, &accounts)
                .map_err(store_error)
        })
        .await
    }

    async fn upsert_account(&self, owner: OwnerId, account: AccountView) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .upsert_account(&AccountRecord {
                    id: account.id,
                    owner,
                    title: account.title,
                    institution: account.institution,
                })
                .map_err(store_error)
        })
        .await
    }

    async fn list_accounts(&self, owner: OwnerId) -> Result<Vec<AccountView>, AppError> {
        self.blocking(move |store| {
            let accounts = store.list_accounts(owner).map_err(store_error)?;
            Ok(accounts
                .into_iter()
                .map(|record| AccountView {
                    id: record.id,
                    title: record.title,
                    institution: record.institution,
                })
                .collect())
        })
        .await
    }

    async fn save_snapshot(&self, owner: OwnerId, snapshot: Snapshot) -> Result<(), AppError> {
        self.blocking(move |store| store.save_snapshot(owner, &snapshot).map_err(store_error))
            .await
    }

    async fn load_snapshot(
        &self,
        owner: OwnerId,
        contour: ContourId,
        version: ContourVersion,
        lot_rule: LotRuleVersion,
    ) -> Result<Option<Snapshot>, AppError> {
        self.blocking(move |store| {
            store
                .load_snapshot(owner, contour, version, lot_rule)
                .map_err(store_error)
        })
        .await
    }

    async fn find_principal(&self, token_hash: String) -> Result<Option<Principal>, AppError> {
        self.blocking(move |store| {
            let found = store.find_token(&token_hash).map_err(store_error)?;
            Ok(found.map(|record| Principal {
                token_id: record.id,
                owner: record.owner,
                scope: scope_from_store(record.scope),
            }))
        })
        .await
    }

    async fn record_token_use(
        &self,
        token_hash: String,
        route: String,
        outcome: String,
    ) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .record_token_use(&token_hash, &route, &outcome)
                .map_err(store_error)
        })
        .await
    }
}

#[async_trait]
impl BrokerVault for SqliteAdapter {
    async fn add_access(
        &self,
        owner: OwnerId,
        broker: String,
        token: Zeroizing<String>,
    ) -> Result<BrokerAccessView, AppError> {
        let key = self.key()?;
        let code = BrokerCode::parse(&broker).ok_or_else(|| AppError::Invalid {
            field: "broker".to_owned(),
            expected: "непустой код брокера".to_owned(),
            actual: broker.clone(),
        })?;
        // Обрамляющие пробелы не часть токена: заголовок с лишним
        // пробелом брокер отвергнет, а причину назовёт невнятно.
        let token = Zeroizing::new(token.trim().to_owned());
        if token.is_empty() {
            return Err(AppError::Invalid {
                field: "token".to_owned(),
                expected: "непустой токен".to_owned(),
                // Значение не называется даже здесь: текст ошибки —
                // это то, что точно попадёт в лог (§14).
                actual: "пустая строка".to_owned(),
            });
        }

        // Шифрование до ухода в блокирующую задачу: открытым токен
        // не пересекает границу потоков и не копируется в замыкание.
        let sealed = seal(key, &token);
        let access = NewBrokerAccess {
            id: Uuid::new_v4(),
            owner,
            broker: code,
            // Область прав задаётся здесь, а не приходит снаружи:
            // торговые права не запрашиваются ни при каких условиях (§14).
            scope: BrokerScope::ReadOnly.code().to_owned(),
            nonce: sealed.nonce().to_vec(),
            ciphertext: sealed.ciphertext().to_vec(),
        };
        // Запись и чтение обратно — одной блокирующей задачей. Момент
        // заведения ставит хранилище, и собрать представление здесь
        // значило бы показать владельцу выдуманное время; а вторым
        // вызовом читать нельзя — между ними доступ успевают отозвать.
        let owner_of_access = access.owner;
        let broker_of_access = access.broker.clone();
        self.blocking(move |store| {
            store.insert_broker_access(&access).map_err(store_error)?;
            let stored = store
                .find_broker_access(owner_of_access, &broker_of_access)
                .map_err(store_error)?
                .ok_or(AppError::Store(
                    "доступ заведён, но не прочитан обратно".to_owned(),
                ))?;
            Ok(BrokerAccessView {
                id: stored.id,
                broker: stored.broker.as_str().to_owned(),
                scope: stored.scope,
                created_at: stored.created_at,
                revoked_at: stored.revoked_at,
            })
        })
        .await
    }

    async fn list_access(&self, owner: OwnerId) -> Result<Vec<BrokerAccessView>, AppError> {
        // Ключ требуется и на чтении списка: без него показанный доступ
        // обещал бы то, чем воспользоваться нельзя, и разбираться с этим
        // пришлось бы по пустому ответу брокера, а не по настройке.
        self.key()?;
        self.blocking(move |store| {
            let history = store.broker_access_history(owner).map_err(store_error)?;
            Ok(history
                .into_iter()
                .map(|access| BrokerAccessView {
                    id: access.id,
                    broker: access.broker.as_str().to_owned(),
                    scope: access.scope,
                    created_at: access.created_at,
                    revoked_at: access.revoked_at,
                })
                .collect())
        })
        .await
    }

    async fn revoke_access(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError> {
        self.key()?;
        self.blocking(move |store| {
            store.revoke_broker_access(owner, id).map_err(|error| {
                // Отсутствующий доступ — ошибка запроса, а не сбой
                // хранилища: повтор её не исправит, и `500` отправил бы
                // владельца искать поломку там, где её нет. Чужой доступ
                // даёт тот же ответ намеренно — иначе он сообщал бы
                // постороннему, что такая запись существует (§14).
                match error {
                    iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                        what: "доступ к брокеру",
                        id: id.to_string(),
                    },
                    other => store_error(other),
                }
            })
        })
        .await
    }
}

#[async_trait]
impl TokenAdmin for SqliteAdapter {
    async fn sole_owner(&self) -> Result<SoleOwner, AppError> {
        self.blocking(move |store| {
            let found = store.sole_token_owner().map_err(store_error)?;
            Ok(match found {
                StoredSoleOwner::None => SoleOwner::None,
                StoredSoleOwner::Single(owner) => SoleOwner::Single(owner),
                StoredSoleOwner::Several => SoleOwner::Several,
            })
        })
        .await
    }

    /// Выпуск токена: случайные 32 байта, хеш в базу, сам токен наружу.
    ///
    /// 32 байта из системного источника, а не «достаточно длинная»
    /// строка: токен — это ключ от чужих денег, и стойкость здесь
    /// задаётся один раз на всю систему. Токен возвращается открытым
    /// ровно один раз — в базе остаётся только хеш, и утечка файла базы
    /// не отдаёт доступ к API (§14).
    async fn issue_token(
        &self,
        owner: OwnerId,
        label: String,
        scope: Scope,
    ) -> Result<IssuedToken, AppError> {
        // Секрет порождается до ухода в блокирующую задачу: отказ
        // источника случайности не должен выглядеть как отказ базы.
        let token = secret_hex(32)?;
        let hash = hash_token(&token);
        let record = TokenRecord {
            id: Uuid::new_v4(),
            owner,
            label: label.clone(),
            scope: scope_to_store(scope),
            revoked: false,
        };
        let id = record.id;
        self.blocking(move |store| store.insert_token(&record, &hash).map_err(store_error))
            .await?;
        Ok(IssuedToken {
            id,
            token,
            label,
            scope,
        })
    }

    async fn list_tokens(&self, owner: OwnerId) -> Result<Vec<TokenView>, AppError> {
        self.blocking(move |store| {
            let tokens = store.list_tokens(owner).map_err(store_error)?;
            Ok(tokens
                .into_iter()
                .map(|token| TokenView {
                    id: token.id,
                    label: token.label,
                    scope: scope_from_store(token.scope),
                    created_at: token.created_at,
                    revoked_at: token.revoked_at,
                })
                .collect())
        })
        .await
    }

    async fn revoke_token(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError> {
        self.blocking(move |store| {
            store.revoke_token(owner, id).map_err(|error| {
                // Отсутствующий токен — ошибка запроса, а не сбой
                // хранилища: повтор её не исправит. Чужой токен даёт
                // тот же ответ намеренно — иначе он сообщал бы
                // постороннему, что такая запись существует (§14).
                match error {
                    iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                        what: "токен",
                        id: id.to_string(),
                    },
                    other => store_error(other),
                }
            })
        })
        .await
    }
}
