//! Порт хранилища поверх `iaam-store`.
//!
//! Здесь и только здесь пересекается граница async/blocking (§3.2).
//! `rusqlite` блокирует поток; вызов его прямо из обработчика `axum`
//! останавливает исполнитель, поэтому каждая операция уходит в
//! `spawn_blocking`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, OwnerId};
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use iaam_store::SqliteStore;
use iaam_store::events::Appended;
use iaam_store::reference::AccountRecord;
use iaam_store::tokens::TokenScope;
use time::Date;

use crate::error::AppError;
use crate::ports::{AccountView, Principal, Recorded, Scope, Store};

/// Соединение под мьютексом: `rusqlite::Connection` не `Sync`, а писатель
/// у однопользовательской базы один. Пул появится тогда, когда появится
/// второй писатель, а не раньше.
pub struct SqliteAdapter {
    store: Arc<Mutex<SqliteStore>>,
}

impl SqliteAdapter {
    #[must_use]
    pub fn new(store: SqliteStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
        }
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

    async fn load_events(&self, owner: OwnerId) -> Result<Vec<Event>, AppError> {
        self.blocking(move |store| store.load_events(owner).map_err(store_error))
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
                scope: match record.scope {
                    TokenScope::Owner => Scope::Owner,
                    TokenScope::Agent => Scope::Agent,
                    TokenScope::ReadOnly => Scope::ReadOnly,
                },
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
