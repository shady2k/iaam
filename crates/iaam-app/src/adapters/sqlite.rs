//! Порт хранилища поверх `iaam-store`.
//!
//! Здесь и только здесь пересекается граница async/blocking (§3.2).
//! `rusqlite` блокирует поток; вызов его прямо из обработчика `axum`
//! останавливает исполнитель, поэтому каждая операция уходит в
//! `spawn_blocking`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use iaam_broker::credentials::{BrokerScope, Key, SealedToken, open, seal};
use iaam_broker::environment::Environment;
use iaam_broker::operation_kind::OperationKindDictionary;
use iaam_broker::tinkoff::TinkoffClient;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::Event;
use iaam_core::ids::{AccountId, ClassificationRuleId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::AliasNamespace;
use iaam_core::projection::Snapshot;
use iaam_core::rules::LotRuleVersion;
use iaam_store::SqliteStore;
use iaam_store::broker_access::{NewBrokerAccess, SoleOwner as StoredSoleOwner};
use iaam_store::broker_operation_kinds::BrokerOperationKind;
use iaam_store::documents::BrokerCode;
use iaam_store::events::Appended;
use iaam_store::reference::{AccountRecord, AliasRecord, InstrumentRecord};
use iaam_store::tokens::{TokenRecord, TokenScope};
use time::Date;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::AppError;
use crate::ports::{
    AccountView, AliasUpsert, AliasView, BrokerAccessView, BrokerChannel, BrokerChannelFactory,
    BrokerEnvironment, BrokerVault, ClassificationRuleStore, ClassificationRuleView, CustodyView,
    InstrumentDirectory, InstrumentUpsert, InstrumentView, IssuedToken, Principal, Recorded, Scope,
    SoleOwner, Store, TokenAdmin, TokenView,
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

/// Три случая резолвинга обязаны остаться различимыми и по эту сторону
/// порта: слитые в один `NotFound` они перестают отвечать на вопрос
/// «новая это бумага или испорченная дата» (E3.1, §5.1 спеки задачи).
fn resolve_error(error: iaam_store::ResolveError) -> AppError {
    match error {
        iaam_store::ResolveError::Unknown { namespace, value } => AppError::NotFound {
            what: "инструмент по коду",
            id: format!("{namespace}:{value}"),
        },
        iaam_store::ResolveError::NotOnDate {
            namespace,
            value,
            on,
            known_from,
            known_to,
        } => AppError::Invalid {
            field: "on".to_owned(),
            expected: format!("дата в интервале действия кода {known_from}..{known_to}"),
            actual: format!("{namespace}:{value} на {on}"),
        },
        iaam_store::ResolveError::Ambiguous {
            namespace,
            value,
            on,
            candidates,
        } => AppError::DirectoryInvariant {
            correlation: Uuid::new_v4(),
            detail: format!(
                "код {namespace}:{value} на {on} разрешается в {candidates} инструментов: \
                 триггер instrument_aliases_do_not_overlap пробит"
            ),
        },
        iaam_store::ResolveError::Store(error) => store_error(error),
    }
}

fn instrument_view(record: iaam_store::reference::InstrumentRecord) -> InstrumentView {
    InstrumentView {
        id: record.id,
        kind: record.kind.map(|kind| kind.code().to_owned()),
        symbol: record.symbol,
        title: record.title,
        denomination_currency: record.currencies.denomination.code().to_owned(),
        settlement_currency: record.currencies.settlement.code().to_owned(),
        quote_currency: record.currencies.quote.code().to_owned(),
    }
}

fn alias_view(record: iaam_store::reference::AliasRecord) -> AliasView {
    AliasView {
        namespace: record.namespace.code().to_owned(),
        value: record.value,
        instrument: record.instrument,
        valid_from: record.interval.valid_from,
        valid_to: record.interval.valid_to,
    }
}

fn custody_view(record: iaam_store::reference::CustodyRecord) -> CustodyView {
    CustodyView {
        id: record.id,
        title: record.title,
        institution: record.institution,
    }
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
impl InstrumentDirectory for SqliteAdapter {
    async fn record_instrument(&self, record: InstrumentUpsert) -> Result<InstrumentId, AppError> {
        let InstrumentUpsert {
            id,
            kind,
            symbol,
            title,
            currencies,
            lineage,
        } = record;
        self.blocking(move |store| {
            store
                .upsert_instrument(&InstrumentRecord {
                    id,
                    kind,
                    symbol,
                    title,
                    currencies,
                    lineage,
                })
                .map_err(store_error)?;
            Ok(id)
        })
        .await
    }

    async fn record_alias(&self, alias: AliasUpsert) -> Result<(), AppError> {
        let AliasUpsert {
            namespace,
            value,
            instrument,
            interval,
            source,
        } = alias;
        self.blocking(move |store| {
            store
                .record_alias(&AliasRecord {
                    namespace,
                    value,
                    instrument,
                    interval,
                    source,
                })
                .map_err(store_error)
        })
        .await
    }
    async fn resolve(
        &self,
        namespace: &str,
        value: &str,
        on: Date,
    ) -> Result<InstrumentId, AppError> {
        let Some(namespace) = AliasNamespace::from_code(namespace) else {
            return Err(AppError::Invalid {
                field: "namespace".to_owned(),
                expected: "isin, moex_secid, ticker, figi или broker_code".to_owned(),
                actual: namespace.to_owned(),
            });
        };
        let value = value.to_owned();
        self.blocking(move |store| {
            store
                .resolve_instrument(namespace, &value, on)
                .map_err(resolve_error)
        })
        .await
    }

    async fn instrument(&self, id: InstrumentId) -> Result<Option<InstrumentView>, AppError> {
        self.blocking(move |store| {
            store
                .instrument(id)
                .map(|found| found.map(instrument_view))
                .map_err(store_error)
        })
        .await
    }

    async fn list_instruments(&self) -> Result<Vec<InstrumentView>, AppError> {
        self.blocking(|store| {
            store
                .list_instruments()
                .map(|rows| rows.into_iter().map(instrument_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn list_aliases(&self) -> Result<Vec<AliasView>, AppError> {
        self.blocking(|store| {
            store
                .list_aliases()
                .map(|rows| rows.into_iter().map(alias_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyView>, AppError> {
        self.blocking(move |store| {
            store
                .list_custody_places(owner)
                .map(|rows| rows.into_iter().map(custody_view).collect())
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
        environment: BrokerEnvironment,
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
            // Среда приходит снаружи, и это единственное место, где она
            // называется: дальше её берут из записи. Заведение — момент,
            // когда человек знает, какой токен держит в руках. Перевод
            // словаря порта в словарь брокера идёт здесь: адаптер —
            // единственный, кто знает оба.
            environment: broker_environment(environment).code().to_owned(),
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
        let environment_of_access = access.environment.clone();
        self.blocking(move |store| {
            store
                .insert_broker_access(&access)
                .map_err(|error| match error {
                    iaam_store::StoreError::AlreadyExists { what } => AppError::Conflict {
                        what: format!("{what} уже заведён: сначала отзовите действующий"),
                    },
                    other => store_error(other),
                })?;

            // Словарь видов операций заселяется здесь, в тот же момент
            // и той же задачей: заведённый доступ без словаря отклонит
            // первую же выгрузку целиком, и владелец пойдёт разбираться
            // с брокером вместо настройки.
            //
            // Сети этот шаг не требует: контракт перечисляет коды, но
            // не сообщает, во что они превращаются у нас, — заселять
            // приходится собственным знанием (`dictionary_seed`).
            // Сверка с контрактом существует отдельно и зовётся явно.
            //
            // Пополнение существующие строки не трогает: заведение
            // доступа заново не имеет права отменить решение владельца.
            let Some((dictionary, entries)) =
                iaam_broker::operation_kind::seed_for(broker_of_access.as_str())
            else {
                return Err(AppError::Invalid {
                    field: "broker".to_owned(),
                    expected: "брокер, для которого известен словарь видов операций".to_owned(),
                    actual: broker_of_access.as_str().to_owned(),
                });
            };
            let entries: Vec<BrokerOperationKind> = entries
                .iter()
                .map(|(source_kind, kind)| BrokerOperationKind {
                    source_kind: (*source_kind).to_owned(),
                    kind: (*kind).to_owned(),
                })
                .collect();
            store
                .extend_broker_operation_kinds(&broker_of_access, dictionary, &entries)
                .map_err(store_error)?;
            let stored = store
                .find_broker_access(owner_of_access, &broker_of_access, &environment_of_access)
                .map_err(store_error)?
                .ok_or(AppError::Store(
                    "доступ заведён, но не прочитан обратно".to_owned(),
                ))?;
            Ok(BrokerAccessView {
                id: stored.id,
                broker: stored.broker.as_str().to_owned(),
                environment: stored.environment,
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
                    environment: access.environment,
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

struct ChannelAccess {
    id: Uuid,
    environment: String,
    scope: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

#[async_trait]
impl crate::ports::BrokerDictionary for SqliteAdapter {
    async fn operation_kinds(
        &self,
        broker: &BrokerCode,
    ) -> Result<std::collections::BTreeMap<String, String>, AppError> {
        let broker = broker.clone();
        self.blocking(move |store| store.broker_operation_kinds(&broker).map_err(store_error))
            .await
    }
}

#[async_trait]
impl BrokerChannelFactory for SqliteAdapter {
    async fn open(&self, owner: OwnerId, broker: &str) -> Result<Arc<dyn BrokerChannel>, AppError> {
        let code = BrokerCode::parse(broker).ok_or_else(|| AppError::Invalid {
            field: "broker".to_owned(),
            expected: "поддерживаемый код брокера".to_owned(),
            actual: broker.to_owned(),
        })?;
        if code.as_str() != "tinkoff" {
            return Err(AppError::Invalid {
                field: "broker".to_owned(),
                expected: "tinkoff".to_owned(),
                actual: broker.to_owned(),
            });
        }

        let key = self.key()?.clone();
        let broker = broker.to_owned();
        let access = self
            .blocking(move |store| {
                let mut active = store
                    .broker_access_history(owner)
                    .map_err(store_error)?
                    .into_iter()
                    .filter(|access| {
                        access.broker.as_str() == broker && access.revoked_at.is_none()
                    });
                let Some(first) = active.next() else {
                    return Err(AppError::NotConfigured {
                        what: "доступ к брокеру",
                    });
                };
                if active.next().is_some() {
                    return Err(AppError::Invalid {
                        field: "broker".to_owned(),
                        expected: "ровно один действующий доступ".to_owned(),
                        actual: broker,
                    });
                }
                Ok(ChannelAccess {
                    id: first.id,
                    environment: first.environment,
                    scope: first.scope,
                    nonce: first.nonce,
                    ciphertext: first.ciphertext,
                })
            })
            .await?;

        if BrokerScope::parse(&access.scope) != Some(BrokerScope::ReadOnly) {
            return Err(AppError::Invalid {
                field: "scope".to_owned(),
                expected: BrokerScope::ReadOnly.code().to_owned(),
                actual: access.scope,
            });
        }
        let environment =
            Environment::parse(&access.environment).ok_or_else(|| AppError::Invalid {
                field: "environment".to_owned(),
                expected: "prod или sandbox".to_owned(),
                actual: access.environment.clone(),
            })?;
        let token =
            open(&key, &SealedToken::of(access.nonce, access.ciphertext)).map_err(|_| {
                AppError::NotConfigured {
                    what: "доступ к брокеру",
                }
            })?;
        let client = TinkoffClient::new(environment, token)
            .map_err(|error| AppError::Store(format!("клиент брокера не создан: {error}")))?;
        // Словарь читается здесь, а не в разборе: `iaam-broker` про
        // хранилище не знает намеренно (см. его `lib.rs`), и связывает
        // их адаптер — тем же приёмом, что уже сделан для SQLite.
        let code = code.clone();
        let rows = self
            .blocking(move |store| store.broker_operation_kinds(&code).map_err(store_error))
            .await?;
        let (dictionary, unreadable) = OperationKindDictionary::build(rows);
        // Строка словаря, которую эта сборка не понимает, означает, что
        // база новее кода. Молча её отбросив, канал превратил бы
        // известный код брокера в неизвестный — то есть выдал бы отказ
        // импорта, из текста которого о рассинхронизации не догадаться.
        if let Some(first) = unreadable.first() {
            return Err(AppError::Invalid {
                field: "broker_operation_kinds".to_owned(),
                expected: "вид операции, известный этой сборке".to_owned(),
                actual: format!("{} -> {}", first.source_kind, first.kind),
            });
        }
        Ok(Arc::new(crate::adapters::tinkoff::TinkoffChannel::new(
            client,
            SourceId(access.id),
            dictionary,
        )))
    }
}

fn classification_rule_view(rule: iaam_store::rules::StoredRule) -> ClassificationRuleView {
    ClassificationRuleView {
        id: rule.id.inner(),
        version: rule.version,
        matcher: rule.matcher,
        outcome: rule.outcome,
        created_at: rule.created_at,
        retired_at: rule.retired_at,
        replaces: rule.replaces.map(|id| id.inner()),
    }
}

#[async_trait]
impl ClassificationRuleStore for SqliteAdapter {
    async fn list_rules(&self, owner: OwnerId) -> Result<Vec<ClassificationRuleView>, AppError> {
        self.blocking(move |store| {
            store
                .rule_history(owner)
                .map(|rules| rules.into_iter().map(classification_rule_view).collect())
                .map_err(store_error)
        })
        .await
    }

    async fn create_rule(
        &self,
        owner: OwnerId,
        matcher: String,
        outcome: String,
        replaces: Option<Uuid>,
    ) -> Result<ClassificationRuleView, AppError> {
        self.blocking(move |store| {
            let rule = match replaces {
                Some(previous) => {
                    store.amend_rule(owner, ClassificationRuleId(previous), &matcher, &outcome)
                }
                None => store.insert_rule(owner, &matcher, &outcome),
            }
            .map_err(|error| match error {
                iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                    what: "действующее правило классификации",
                    id: replaces.map_or_else(String::new, |id| id.to_string()),
                },
                iaam_store::StoreError::AlreadyExists { what } => AppError::Conflict {
                    what: what.to_owned(),
                },
                other => store_error(other),
            })?;
            Ok(classification_rule_view(rule))
        })
        .await
    }

    async fn retire_rule(&self, owner: OwnerId, id: Uuid) -> Result<(), AppError> {
        self.blocking(move |store| {
            store
                .retire_rule(owner, ClassificationRuleId(id))
                .map_err(|error| match error {
                    iaam_store::StoreError::NotFound { .. } => AppError::NotFound {
                        what: "действующее правило классификации",
                        id: id.to_string(),
                    },
                    other => store_error(other),
                })
        })
        .await
    }
}

/// Среда порта в среду брокера.
///
/// Перевод, а не общий тип: транспорт зовёт порт и про `iaam-broker`
/// не знает — заслон архитектуры это проверяет.
const fn broker_environment(environment: BrokerEnvironment) -> Environment {
    match environment {
        BrokerEnvironment::Prod => Environment::Prod,
        BrokerEnvironment::Sandbox => Environment::Sandbox,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_error_preserves_unknown_date_and_ambiguous_distinctions() {
        let unknown = resolve_error(iaam_store::ResolveError::Unknown {
            namespace: "isin",
            value: "RU000A".to_owned(),
        });
        let not_on_date = resolve_error(iaam_store::ResolveError::NotOnDate {
            namespace: "isin",
            value: "RU000A".to_owned(),
            on: "2026-08-25".to_owned(),
            known_from: "2020-01-01".to_owned(),
            known_to: "2025-12-31".to_owned(),
        });
        let ambiguous = resolve_error(iaam_store::ResolveError::Ambiguous {
            namespace: "ticker",
            value: "ABC".to_owned(),
            on: "2026-08-25".to_owned(),
            candidates: 2,
        });

        assert_eq!(unknown.code(), "not_found");
        assert_eq!(not_on_date.code(), "invalid_request");
        assert_eq!(ambiguous.code(), "directory_invariant_violated");
        assert!(matches!(
            &unknown,
            AppError::NotFound {
                what: "инструмент по коду",
                id,
            } if id == "isin:RU000A"
        ));
        assert!(matches!(
            &not_on_date,
            AppError::Invalid {
                field,
                expected,
                actual,
            } if field == "on"
                && expected == "дата в интервале действия кода 2020-01-01..2025-12-31"
                && actual == "isin:RU000A на 2026-08-25"
        ));
        assert!(matches!(
            &ambiguous,
            AppError::DirectoryInvariant { detail, .. }
                if detail
                    == "код ticker:ABC на 2026-08-25 разрешается в 2 инструментов: \
                       триггер instrument_aliases_do_not_overlap пробит"
        ));
        let message = ambiguous.to_string();
        assert!(message.contains("инвариант справочника"));
        assert!(message.contains("ticker:ABC"));
        assert!(message.contains("2026-08-25"));
        assert!(message.contains("2"));
    }
}
