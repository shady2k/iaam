//! Контрактные тесты против порождённой спеки (§17.1).
//!
//! `utoipa` порождает спеку из типов и потому устраняет расхождение
//! **схемы данных**. Поведение — коды ответов, требования аутентификации,
//! фактическая сериализация — остаётся вне генерации, и проверяется
//! только вызовом поднятого сервера. Для контракта, которым пользуется
//! внешний агент, синтаксически верная, но поведенчески неверная спека
//! означает, что агент будет чиниться по неверной подсказке.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use ciborium::value::Value as CborValue;
use http_body_util::BodyExt;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ingest::{OperationDates, OperationKind, Rejection, SubmittedOperation, Verdict};
use iaam_app::ports::{
    BrokerChannel, BrokerChannelFactory, BrokerError, BrokerVault, ClassificationRuleStore, Clock,
    ParsedOperations, TokenAdmin, UnavailableOutboundHttp,
};
use iaam_app::storage::SqliteStore;
use iaam_app::storage::{
    AccountRecord, AliasRecord, BrokerCode, Coverage, FxRow, InstrumentRecord, KeyRateRow,
    PriceRow, RunOutcome, SeriesKey, TokenRecord, TokenScope,
};
use iaam_broker::credentials::Key;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind};
use iaam_core::money::{CurrencyCode, PerUnitAmount};
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::numeric::decimal::Dec;
use iaam_core::perimeter::{PerimeterPolicy, assess};
use iaam_core::projection::{ProjectionContext, Snapshot, project};
use iaam_core::reconciliation::{Dimension, ReconciliationLedger};
use iaam_core::returns::{
    KnowledgeCoordinate, MaterialIssue, ReturnsRequest, UnverifiableReason, returns_report,
};
use iaam_core::rules::lot_disposal::PrincipalState;
use iaam_core::rules::{LotRuleVersion, PostingKind, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use iaam_server::auth::hash_token;
use iaam_server::dto::{ReturnsReportDto, VerdictDto};
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::market::AccruedInterestRow;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::schedule::{
    CouponPeriodRow, IssueTermsRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};
use serde_json::{Value, json};
use std::io::Cursor;
use std::time::Duration;
use time::macros::date;
use time::{Date, Duration as TimeDuration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

/// Часы с зафиксированной датой: отчёт «на сегодня» иначе
/// невоспроизводим в тесте.
struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

struct EmptyChannel {
    source: iaam_core::reconciliation::evidence::SourceChannel,
}

#[async_trait::async_trait]
impl BrokerChannel for EmptyChannel {
    async fn fetch_operations(
        &self,
        _account: AccountId,
        _from: Date,
        _to: Date,
    ) -> Result<ParsedOperations, BrokerError> {
        Ok(ParsedOperations {
            accepted: Vec::new(),
            quarantined: Vec::new(),
        })
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _at: Date,
    ) -> Result<Vec<iaam_core::reconciliation::claim::ControlClaim>, BrokerError> {
        Ok(Vec::new())
    }

    fn channel(&self) -> iaam_core::reconciliation::evidence::SourceChannel {
        self.source.clone()
    }
}

struct PopulatedChannel {
    source: iaam_core::reconciliation::evidence::SourceChannel,
}

#[async_trait::async_trait]
impl BrokerChannel for PopulatedChannel {
    async fn fetch_operations(
        &self,
        account: AccountId,
        _from: Date,
        _to: Date,
    ) -> Result<ParsedOperations, BrokerError> {
        Ok(ParsedOperations {
            accepted: vec![SubmittedOperation {
                account,
                kind: OperationKind::Deposit {
                    amount_minor: 1_000,
                    currency: CurrencyCode::Rub,
                },
                dates: OperationDates {
                    cash_posted: Some(date!(2025 - 01 - 01)),
                    ..Default::default()
                },
                idempotency_key: Some("sync-row-1".to_owned()),
                source_operation_id: Some("broker-row-1".to_owned()),
            }],
            quarantined: Vec::new(),
        })
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _at: Date,
    ) -> Result<Vec<iaam_core::reconciliation::claim::ControlClaim>, BrokerError> {
        Ok(Vec::new())
    }

    fn channel(&self) -> iaam_core::reconciliation::evidence::SourceChannel {
        self.source.clone()
    }
}

struct FixedChannelFactory {
    channel: Arc<dyn BrokerChannel>,
}

#[async_trait::async_trait]
impl BrokerChannelFactory for FixedChannelFactory {
    async fn open(
        &self,
        _owner: OwnerId,
        _broker: &str,
    ) -> Result<Arc<dyn BrokerChannel>, iaam_app::error::AppError> {
        Ok(self.channel.clone())
    }
}

struct Harness {
    router: Router,
    api: utoipa::openapi::OpenApi,
    owner_token: String,
    agent_token: String,
    readonly_token: String,
    owner: OwnerId,
    account: AccountId,
    instrument: InstrumentId,
    custody: CustodyId,
    market_store: Arc<tokio::sync::Mutex<SqliteStore>>,
}

fn harness() -> Harness {
    harness_with(SqliteStore::open_in_memory().expect("база в памяти"))
}

/// Тот же стенд, но на базе файлом: тесты, проверяющие, что запись
/// действительно легла в таблицу, обязаны иметь второе соединение
/// к той же базе. Через `open_in_memory` второго соединения не бывает.
fn harness_on_disk() -> (Harness, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("iaam-contract-{}.db", Uuid::new_v4()));
    let store = SqliteStore::open(&path).expect("база файлом");
    (harness_with(store), path)
}

fn add_reconciliation_assertion(path: &std::path::Path, owner: OwnerId, account: AccountId) {
    let period = iaam_core::reconciliation::claim::AssertionPeriod::between(
        date!(2025 - 01 - 01),
        date!(2025 - 01 - 31),
    )
    .expect("период");
    let source = SourceId::new_random();
    let event = iaam_core::event::Event {
        id: iaam_core::ids::EventId::new_random(),
        schema_version: iaam_core::event::SCHEMA_VERSION,
        owner,
        account,
        kind: iaam_core::event::kind::EventKind::ControlAssertion {
            period,
            claim: iaam_core::reconciliation::claim::ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: iaam_core::money::PostedMinor::new(10_000),
                at: iaam_core::reconciliation::claim::BalancePoint::Closing,
            },
        },
        dates: EventDates::for_cash(CashPostedDate(period.to)),
        order: EffectiveOrder::new(period.to, 1),
        legs: Vec::new(),
        provenance: iaam_core::event::provenance::Provenance::new(
            source,
            iaam_core::event::provenance::RawHash::parse(&"b".repeat(64)).expect("хеш"),
            ParserVersion("contract-test".to_owned()),
        ),
        relation: iaam_core::event::Relation::None,
        confidence: iaam_core::event::Confidence::Known,
        idempotency_key: Some(format!(
            "owner-balance:{}:{}:{}",
            account.inner(),
            period.from,
            period.to
        )),
    };
    SqliteStore::open(path)
        .expect("второе соединение")
        .append_event(&event)
        .expect("утверждение сверки");
}

fn harness_with(store: SqliteStore) -> Harness {
    harness_with_factory(store, None)
}

fn harness_with_factory(
    store: SqliteStore,
    channel_factory: Option<Arc<dyn BrokerChannelFactory>>,
) -> Harness {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Брокерский".into(),
            institution: None,
        })
        .expect("счёт");

    let owner_token = "owner-secret-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "владелец".into(),
                scope: TokenScope::Owner,
                revoked: false,
            },
            &hash_token(owner_token),
        )
        .expect("токен владельца");

    let agent_token = "agent-secret-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "агент".into(),
                scope: TokenScope::Agent,
                revoked: false,
            },
            &hash_token(agent_token),
        )
        .expect("токен агента");

    let readonly_token = "read-only-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "чтение".into(),
                scope: TokenScope::ReadOnly,
                revoked: false,
            },
            &hash_token(readonly_token),
        )
        .expect("токен чтения");

    // Ключ прямо из байтов, а не из файла: файл во временном каталоге
    // пришлось бы удалять, и тест, упавший до удаления, оставлял бы
    // за собой ключ. Постоянные байты здесь безопасны — база живёт
    // ровно столько же, сколько тест.
    let adapter = Arc::new(SqliteAdapter::with_broker_key(
        store,
        Some(Key::from_bytes([7; 32])),
    ));
    let broker: Arc<dyn BrokerVault> = adapter.clone();
    let channels: Arc<dyn BrokerChannelFactory> =
        channel_factory.unwrap_or_else(|| adapter.clone());
    let rules: Arc<dyn ClassificationRuleStore> = adapter.clone();
    let tokens: Arc<dyn TokenAdmin> = adapter.clone();
    let market_store = Arc::new(tokio::sync::Mutex::new(
        SqliteStore::open_in_memory().expect("market store"),
    ));
    let broker_dictionary: Arc<dyn iaam_app::ports::BrokerDictionary> = adapter.clone();
    let services = Arc::new(AppServices {
        store: adapter.clone(),
        directory: adapter,
        broker,
        tokens,
        clock: Arc::new(FixedClock(date!(2026 - 01 - 01))),
        channels,
        rules,
        http: Arc::new(UnavailableOutboundHttp),
        broker_dictionary,
        market_store: market_store.clone(),
    });
    let state = ServerState::new(
        services,
        Arc::new(RateLimiter::new(1_000, Duration::from_secs(60))),
    );
    let (router, api) = build(state);

    Harness {
        router,
        api,
        owner_token: owner_token.to_owned(),
        agent_token: agent_token.to_owned(),
        readonly_token: readonly_token.to_owned(),
        owner,
        account,
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        market_store,
    }
}

async fn seed_market(harness: &Harness) {
    let mut store = harness.market_store.lock().await;
    let lease_expires_at = OffsetDateTime::now_utc() + TimeDuration::days(1);
    store
        .upsert_instrument(&InstrumentRecord {
            id: harness.instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".into(),
            title: "Сбербанк".into(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент рынка");

    let price_series = SeriesKey {
        source_id: "moex-iss".into(),
        dataset: "prices".into(),
        series_key: format!("{}:TQBR:1", harness.instrument.inner()),
    };
    let price_run = store
        .begin_run(
            price_series,
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 03),
            lease_expires_at,
        )
        .expect("запуск цен");
    store
        .record_prices(
            &price_run,
            &"a".repeat(64),
            &[
                PriceRow {
                    instrument_id: harness.instrument.inner().to_string(),
                    board: "TQBR".into(),
                    session: 1,
                    trade_date: "2026-08-01".into(),
                    kind: "close".into(),
                    observed_at: "2026-08-20T00:00:00Z".into(),
                    price: "100.00".into(),
                    currency: "RUB".into(),
                    quotation_basis: "money_per_unit".into(),
                    basis_evidence: "test:contract".into(),
                    executability: "executable".into(),
                },
                PriceRow {
                    instrument_id: harness.instrument.inner().to_string(),
                    board: "TQBR".into(),
                    session: 1,
                    trade_date: "2026-08-03".into(),
                    kind: "close".into(),
                    observed_at: "2026-08-20T00:00:00Z".into(),
                    price: "101.00".into(),
                    currency: "RUB".into(),
                    quotation_basis: "money_per_unit".into(),
                    basis_evidence: "test:contract".into(),
                    executability: "indicative_previous_close".into(),
                },
            ],
        )
        .expect("строки цен");
    store
        .finish_run(
            &price_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("публикация цен");

    let fx_series = SeriesKey {
        source_id: "cbr".into(),
        dataset: "fx".into(),
        series_key: "USD:RUB".into(),
    };
    let fx_run = store
        .begin_run(
            fx_series,
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 03),
            lease_expires_at,
        )
        .expect("запуск курсов");
    store
        .record_fx(
            &fx_run,
            &"b".repeat(64),
            &[FxRow {
                from_code: "USD".into(),
                to_code: "RUB".into(),
                trade_date: "2026-08-03".into(),
                observed_at: "2026-08-20T00:00:00Z".into(),
                nominal: 1,
                value: "80.00".into(),
                unit_rate: "80.00".into(),
            }],
        )
        .expect("строка курса");
    store
        .finish_run(
            &fx_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("публикация курса");

    let key_rate_series = SeriesKey {
        source_id: "cbr".into(),
        dataset: "key_rate".into(),
        series_key: "key_rate".into(),
    };
    let key_rate_run = store
        .begin_run(
            key_rate_series,
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 10),
            lease_expires_at,
        )
        .expect("запуск ставки");
    store
        .record_key_rate(
            &key_rate_run,
            &"c".repeat(64),
            &[
                KeyRateRow {
                    trade_date: "2026-08-03".into(),
                    observed_at: "2026-08-20T00:00:00Z".into(),
                    rate: "18.00".into(),
                },
                KeyRateRow {
                    trade_date: "2026-08-10".into(),
                    observed_at: "2026-08-20T00:00:00Z".into(),
                    rate: "17.00".into(),
                },
            ],
        )
        .expect("строки ставки");
    store
        .finish_run(
            &key_rate_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),

                to: date!(2026 - 08 - 10),
            }),
        )
        .expect("публикация ставки");
}

async fn seed_bond_market(harness: &Harness) {
    let mut store = harness.market_store.lock().await;
    store
        .upsert_instrument(&InstrumentRecord {
            id: harness.instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "BOND".into(),
            title: "Тестовая облигация".into(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент рынка облигации");
    store
        .extend_market_source_codes(
            "moex-iss",
            "offer_kind",
            &[SourceCodeEntry {
                domain: "offer_kind".into(),
                source_code: "Оферта".into(),
                meaning: "put_option".into(),
            }],
        )
        .expect("словарь оферт");

    let price_run = store
        .begin_run(
            SeriesKey {
                source_id: "moex-iss".into(),
                dataset: "prices".into(),
                series_key: format!("{}:TQOB:0", harness.instrument.inner()),
            },
            date!(2026 - 08 - 01),
            date!(2026 - 08 - 03),
            OffsetDateTime::now_utc() + TimeDuration::days(1),
        )
        .expect("запуск цен облигации");
    store
        .record_prices(
            &price_run,
            &"d".repeat(64),
            &[PriceRow {
                instrument_id: harness.instrument.inner().to_string(),
                board: "TQOB".into(),
                session: 0,
                trade_date: "2026-08-03".into(),
                kind: "close".into(),
                observed_at: "2026-08-20T00:00:00Z".into(),
                price: "98.5".into(),
                currency: "RUB".into(),
                quotation_basis: "percent_of_remaining_face".into(),
                basis_evidence: "iss:engines/stock/markets/bonds".into(),
                executability: "executable".into(),
            }],
        )
        .expect("цена облигации");
    store
        .finish_run(
            &price_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("публикация цены облигации");
    let accrued_run = store
        .begin_run(
            SeriesKey {
                source_id: "moex-iss".into(),
                dataset: "accrued_interest".into(),
                series_key: format!("{}:TQOB:0", harness.instrument.inner()),
            },
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            OffsetDateTime::now_utc() + TimeDuration::days(1),
        )
        .expect("запуск НКД");
    store
        .record_accrued_interest(
            &accrued_run,
            &"e".repeat(64),
            &[AccruedInterestRow {
                instrument_id: harness.instrument.inner().to_string(),
                board: "TQOB".into(),
                session: 0,
                trade_date: "2026-08-03".into(),
                observed_at: "2026-08-20T00:00:00Z".into(),
                per_unit: "1".into(),
                currency: "RUB".into(),
            }],
        )
        .expect("НКД облигации");
    store
        .finish_run(
            &accrued_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("публикация НКД облигации");

    let snapshot = store
        .record_schedule_snapshot(
            &ScheduleSnapshotRow {
                instrument_id: harness.instrument.inner().to_string(),
                source_id: "moex-iss".into(),
                observed_at: "2026-08-20T00:00:00Z".into(),
                content_hash: "bond-contract-schedule".into(),
            },
            &[CouponPeriodRow {
                period_start: "2026-08-01".into(),
                accrual_end: "2026-12-01".into(),
                payment_date: "2026-12-02".into(),
                record_date: None,
                amount_status: "amount_fixed".into(),
                amount_per_unit: Some("5".into()),
                amount_currency: Some("RUB".into()),
                rate_percent: None,
                source_entry_id: Some("coupon-1".into()),
            }],
            &[PrincipalRepaymentRow {
                repayment_date: "2026-12-02".into(),
                share_percent: "100".into(),
                source_kind: "maturity".into(),
                source_entry_id: Some("principal-1".into()),
            }],
            &[
                OfferWindowRow {
                    execution_date: "2026-08-26".into(),
                    submission_start: None,
                    submission_end: None,
                    price_percent: Some("100".into()),
                    agent: None,
                    source_kind: "Оферта".into(),
                    source_entry_id: Some("offer-now".into()),
                },
                OfferWindowRow {
                    execution_date: "2026-09-15".into(),
                    submission_start: None,
                    submission_end: None,
                    price_percent: Some("100".into()),
                    agent: None,
                    source_kind: "Оферта".into(),
                    source_entry_id: Some("offer-future".into()),
                },
            ],
        )
        .expect("снимок графика");
    store
        .record_schedule_completeness(&snapshot.snapshot_id, true, true, None, &[0])
        .expect("полнота графика");
    store
        .record_issue_terms(&IssueTermsRow {
            instrument_id: harness.instrument.inner().to_string(),
            source_id: "moex-iss".into(),
            observed_at: "2026-08-20T00:00:00Z".into(),
            effective_from: Some("2026-08-01".into()),
            maturity_date: Some("2026-12-02".into()),
            initial_face_value: Some("1000".into()),
            face_currency_code: Some("RUB".into()),
            coupon_periods_per_year: Some(1),
            day_count: Some("act/365".into()),
            calendar: Some("MOEX".into()),
            default_declared: false,
            default_technical: false,
        })
        .expect("условия выпуска");
}

fn replace_unknown_principal(value: &mut CborValue, known: &CborValue) -> usize {
    match value {
        CborValue::Map(entries) => {
            let mut replaced = 0;
            for (key, value) in entries {
                let is_principal = matches!(key, CborValue::Text(name) if name == "principal");
                if is_principal && matches!(value, CborValue::Text(name) if name == "Unknown") {
                    *value = known.clone();
                    replaced += 1;
                } else {
                    replaced += replace_unknown_principal(value, known);
                }
            }
            replaced
        }
        CborValue::Array(values) => values
            .iter_mut()
            .map(|value| replace_unknown_principal(value, known))
            .sum(),
        CborValue::Tag(_, value) => replace_unknown_principal(value, known),
        _ => 0,
    }
}

fn install_known_principal_snapshot(
    path: &std::path::Path,
    owner: OwnerId,
    account: AccountId,
    contour_id: Uuid,
) {
    let store = SqliteStore::open(path).expect("второе соединение для снимка");
    let events = store
        .load_events_through(owner, Date::MAX)
        .expect("события позиции");
    let contour = ContourDefinition::new(ContourId(contour_id), ContourVersion(1), [account]);
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(&events, &context).expect("проекция позиции");

    let principal = PrincipalState::known(
        PerUnitAmount::new(
            Dec::new("1000".parse().expect("номинал")),
            CurrencyCode::Rub,
        ),
        PerUnitAmount::new(
            Dec::new("1000".parse().expect("остаток номинала")),
            CurrencyCode::Rub,
        ),
    )
    .expect("известный номинал");
    let mut encoded_principal = Vec::new();
    ciborium::ser::into_writer(&principal, &mut encoded_principal).expect("кодирование номинала");
    let known: CborValue =
        ciborium::de::from_reader(Cursor::new(encoded_principal)).expect("разбор номинала");

    let mut encoded_state = Vec::new();
    ciborium::ser::into_writer(projection.snapshot().state(), &mut encoded_state)
        .expect("кодирование состояния");
    let mut state_value: CborValue =
        ciborium::de::from_reader(Cursor::new(encoded_state)).expect("разбор состояния");
    let replaced = replace_unknown_principal(&mut state_value, &known);
    assert_eq!(replaced, 1, "только лот тестовой облигации требует номинал");
    let mut patched_state_bytes = Vec::new();
    ciborium::ser::into_writer(&state_value, &mut patched_state_bytes)
        .expect("кодирование исправленного состояния");
    let mut parts = projection.snapshot().clone().into_parts();
    parts.state = ciborium::de::from_reader(Cursor::new(patched_state_bytes))
        .expect("исправленное состояние");
    parts.fingerprint = parts.state.fingerprint();
    store
        .save_snapshot(owner, &Snapshot::restore(parts))
        .expect("снимок с известным номиналом");
}

async fn call(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("обработчик ответил");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("тело ответа")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(path).method("GET");
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("запрос")
}

fn delete(path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("DELETE")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("запрос")
}

fn post(path: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("запрос")
}

/// Запрос без токена: присвоение экземпляра зовут тогда, когда токена
/// ещё нет и взять его неоткуда.
fn post_public(path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("запрос")
}

/// Стенд без владельца: экземпляр ещё не присвоен.
///
/// Отдельный стенд, а не признак у общего: в общем владелец заведён
/// с первой строки, и присваивать там нечего. Код присвоения берётся
/// из той же функции, которой его порождает точка сборки, — иначе тест
/// проверял бы не тот путь, которым код попадает к человеку.
async fn unclaimed_harness() -> (Router, String) {
    unclaimed_harness_with(SqliteStore::open_in_memory().expect("база в памяти")).await
}

/// Тот же стенд на базе файлом: проверка того, что владелец завёлся
/// **помимо** присвоения, требует второго соединения к той же базе.
async fn unclaimed_harness_on_disk() -> (Router, String, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("iaam-claim-{}.db", Uuid::new_v4()));
    let store = SqliteStore::open(&path).expect("база файлом");
    let (router, code) = unclaimed_harness_with(store).await;
    (router, code, path)
}

async fn unclaimed_harness_with(store: SqliteStore) -> (Router, String) {
    let state = claim_state(store);
    let code = iaam_server::claim::arm(&state)
        .await
        .expect("состояние базы прочитано")
        .expect("владельца нет — код присвоения обязан быть порождён");
    let (router, _) = build(state);
    (router, code)
}

/// Состояние сервера поверх готовой базы.
///
/// Общее для стендов присвоения: собирать его в каждом означало бы,
/// что тесты проверяют разные сборки одного и того же.
fn claim_state(store: SqliteStore) -> ServerState {
    let adapter = Arc::new(SqliteAdapter::new(store));
    let broker: Arc<dyn BrokerVault> = adapter.clone();
    let tokens: Arc<dyn TokenAdmin> = adapter.clone();
    let services = Arc::new(AppServices::new(
        adapter.clone(),
        adapter.clone(),
        broker,
        tokens,
        Arc::new(FixedClock(date!(2026 - 01 - 01))),
    ));
    ServerState::new(
        services,
        Arc::new(RateLimiter::new(1_000, Duration::from_secs(60))),
    )
}

#[tokio::test]
async fn health_is_public_and_reports_versions() {
    let harness = harness();
    let (status, body) = call(&harness.router, get("/v1/health", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    // Версия 4: после ControlAssertion (версия 3) добавлены
    // CorporateAction и OfferExercise, а Income получил вид дохода
    // (§4.7); одна версия не может обозначать две схемы (§4.1). Внешний
    // агент читает эту цифру, чтобы понять, разберёт ли он ответ, —
    // поэтому она закреплена здесь, а не выводится из кода.
    assert_eq!(body["schema_version"], 4);
    // Версия 4: проекция обзавелась датированными фактами дохода,
    // которых снимок версии 3 не содержит. Снимок при этом кэш:
    // несовпадение версии ведёт к полному пересчёту журнала, а не
    // к отказу в обслуживании.
    assert_eq!(body["projection_version"], 4);
}

#[tokio::test]
async fn every_documented_path_answers_something_other_than_404() {
    // Спека, описывающая несуществующий маршрут, — это инструкция
    // внешнему агенту чинить себя по неверной подсказке.
    let harness = harness();
    for (path, item) in harness.api.paths.paths.clone() {
        // `PathItem` в utoipa 5 хранит операции отдельными полями,
        // а не картой: перечисляем ровно те методы, которые использует API.
        let methods = [
            ("GET", item.get.is_some()),
            ("POST", item.post.is_some()),
            ("PUT", item.put.is_some()),
            ("DELETE", item.delete.is_some()),
            ("PATCH", item.patch.is_some()),
        ];
        for (verb, present) in methods {
            if !present {
                continue;
            }
            let request = Request::builder()
                .uri(path.replace("{id}", &Uuid::new_v4().to_string()))
                .method(verb)
                .header("Authorization", format!("Bearer {}", harness.owner_token))
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .expect("запрос");
            let (status, body) = call(&harness.router, request).await;
            // `404` бывает двух разных родов, и заслон различает их по
            // телу ответа. Отсутствующий **маршрут** отдаёт пустой `404`
            // самого axum — это и есть расхождение со спекой, ради
            // которого тест написан. Отсутствующий **ресурс** отдаёт наш
            // `ApiError` с машиночитаемым кодом, и это законный ответ:
            // идентификатор в запросе случайный, и записи с ним нет.
            if status == StatusCode::NOT_FOUND {
                assert!(
                    body.get("code").and_then(Value::as_str).is_some(),
                    "маршрут {path} {verb} описан в спеке, но не существует"
                );
            }
            assert_ne!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "метод {verb} для {path} описан в спеке, но не поддерживается"
            );
        }
    }
}

#[tokio::test]
async fn a_request_without_a_token_is_rejected() {
    // Аутентификация с первого дня (§14).
    let harness = harness();
    let (status, body) = call(&harness.router, get("/v1/accounts", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
}

#[tokio::test]
async fn an_unknown_token_is_rejected() {
    let harness = harness();
    let (status, _) = call(&harness.router, get("/v1/accounts", Some("чужой"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_read_only_token_may_not_submit_operations() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.readonly_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(response["code"], "forbidden");
}

#[tokio::test]
async fn an_invalid_amount_is_reported_as_422_with_field_expected_actual() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.005",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    // Вердикт на строку, а не отказ всего документа (§10.1).
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response[0]["verdict"], "rejected");
    assert_eq!(response[0]["field"], "amount");
    assert_eq!(response[0]["actual"], "1000.005");
}

#[tokio::test]
async fn a_carried_forward_price_is_not_accepted_from_the_api() {
    let harness = harness();
    let body = json!({
        "source_label": "ручной ввод",
        "operations": [{
            "account": harness.account.inner(),
            "type": "valuation",
            "instrument": harness.instrument.inner(),
            "price": "1000",
            "currency": "RUB",
            "quality": "carried_forward",
            "dates": { "cash_posted": "2026-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
}

#[tokio::test]
async fn a_stale_price_is_not_accepted_from_the_api() {
    let harness = harness();
    let body = json!({
        "source_label": "ручной ввод",
        "operations": [{
            "account": harness.account.inner(),
            "type": "valuation",
            "instrument": harness.instrument.inner(),
            "price": "1000",
            "currency": "RUB",
            "quality": "stale",
            "dates": { "cash_posted": "2026-01-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
}

#[tokio::test]
async fn the_stage_one_question_is_answered_end_to_end() {
    // Приёмочный критерий эпика через API: сколько внесено, сколько
    // выведено, какова доходность до налога.
    let harness = harness();

    let contour = json!({
        "title": "Мой портфель",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "ручной ввод",
        "operations": [
            {
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": "100000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-01-01" },
                "idempotency_key": "dep-1"
            },
            {
                "account": harness.account.inner(),
                "type": "buy",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "100",
                "amount": "90000.00",
                "fee": "100.00",
                "currency": "RUB",
                "dates": { "trade": "2025-01-15", "cash_posted": "2025-01-15" }
            },
            {
                "account": harness.account.inner(),
                "type": "income",
                "instrument": harness.instrument.inner(),
                "amount": "3000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-07-01" }
            },
            {
                "account": harness.account.inner(),
                "type": "withdrawal",
                "amount": "10000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-09-01" }
            },
            {
                "account": harness.account.inner(),
                "type": "valuation",
                "instrument": harness.instrument.inner(),
                "price": "1000",
                "currency": "RUB",
                "quality": "previous_close",
                "dates": { "cash_posted": "2026-01-01" }
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    for verdict in verdicts.as_array().expect("массив вердиктов") {
        assert_eq!(verdict["verdict"], "provisional", "{verdict}");
    }

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    // Масштаб сохраняется: рубль имеет две минимальные единицы, и сумма,
    // переведённая из проведённой в расчётную, остаётся с двумя знаками.
    assert_eq!(report["contributed"]["value"], "100000.00");
    assert_eq!(report["withdrawn"]["value"], "10000.00");
    // 2 900,00 рубля денег плюс 100 бумаг по 1 000 = 102 900,00.
    assert_eq!(report["terminal_value"]["value"], "102900.00");
    assert_eq!(report["history_starts"], "2025-01-01");
    assert_eq!(report["bond_metrics"], json!([]));
    assert!(
        report["data_quality"]["nav_coverage"]
            .get("bond_metrics")
            .is_none(),
        "метрики облигаций не должны попадать в data_quality"
    );
    assert_eq!(report["applied_rules"]["fx_source"], "cbr_official");
    assert_eq!(report["applied_rules"]["day_count"], "act/365");
    let (status, missing_rate_report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=USD&as_of=2026-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{missing_rate_report}");
    for field in ["contributed", "terminal_value", "xirr_pre_tax"] {
        assert_eq!(
            missing_rate_report[field]["not_computable"], "missing_fx_rate",
            "отсутствующий курс не должен превращаться в единицу: {field}"
        );
    }

    // Ставка получена независимым эталоном (scripts/gen-xirr-fixtures.py),
    // а не выводом проверяемой программы (§15.5).
    let rate: f64 = report["xirr_pre_tax"]["value"]
        .as_str()
        .expect("ставка")
        .parse()
        .expect("число");
    assert!(
        (rate - 0.133_270_341_032).abs() < 1e-7,
        "ставка {rate} не совпадает с эталонной"
    );
    // Данные введены руками и ничем не подтверждены: вся стоимость
    // портфеля лежит в доле `provisional`. Это не дефект — §10.5
    // требует считать такие записи в отчётах по умолчанию, — но
    // владелец обязан видеть, какая именно доля не подтверждена.
    assert_eq!(report["data_quality"]["nav_coverage"]["provisional"], "1");
    assert_eq!(
        report["data_quality"]["nav_coverage"]["accepted_independent"],
        "0"
    );
    assert_eq!(report["data_quality"]["nav_coverage"]["discrepant"], "0");
    assert_eq!(
        report["data_quality"]["position_coverage"]["evaluated_positions"],
        1
    );
    assert_eq!(
        report["data_quality"]["position_coverage"]["total_positions"],
        1
    );
    assert_eq!(
        report["data_quality"]["position_coverage"]["selected"][0]["price"]["provenance"]["price_kind"],
        Value::Null
    );
    assert_eq!(
        report["data_quality"]["position_coverage"]["selected"][0]["price"]["provenance"]["origin"]
            ["kind"],
        "report_parsed"
    );
    assert_eq!(
        report["data_quality"]["position_coverage"]["selected"][0]["price"]["provenance"]["source_priority_version"],
        1
    );
    assert_eq!(
        report["data_quality"]["position_coverage"]["selected"][0]["quantity"],
        "100"
    );
    assert_eq!(
        report["data_quality"]["position_coverage"]["selected"][0]["price"]["provenance"]["carry_forward_limit"],
        10
    );
    assert_eq!(
        report["data_quality"]["position_coverage"]["selected"][0]["price"]["provenance"]["price_max_age"],
        30
    );
    assert_eq!(
        report["data_quality"]["executability"]["evaluated_positions_value"],
        "100000"
    );
    assert_eq!(report["data_quality"]["executability"]["executable"], "0");
    assert_eq!(
        report["data_quality"]["executability"]["indicative_previous_close"],
        "1"
    );
    assert_eq!(
        report["liquidation_value_before_exit_costs_and_tax"]["exit_costs"]["qualification"],
        "unknown"
    );
    assert_eq!(
        report["liquidation_value_before_exit_costs_and_tax"]["tax"]["qualification"],
        "unknown"
    );
    assert!(report["liquidation_value_before_exit_costs_and_tax"]["exit_costs"]["value"].is_null());
}

#[tokio::test]
async fn returns_report_serializes_bond_metrics_and_all_nested_dto_branches() {
    let (harness, path) = harness_on_disk();
    let (status, instrument_response) = call(
        &harness.router,
        post(
            "/v1/instruments",
            &harness.owner_token,
            &json!({
                "id": harness.instrument.inner(),
                "kind": "bond",
                "symbol": "BOND",
                "title": "Тестовая облигация",
                "denomination_currency": "RUB",
                "settlement_currency": "RUB",
                "quote_currency": "RUB"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{instrument_response}");
    seed_bond_market(&harness).await;

    let contour = json!({
        "title": "Облигационный портфель",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "ручной ввод",
        "operations": [
            {
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": "10000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-01" }
            },
            {
                "account": harness.account.inner(),
                "type": "buy",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "10",
                "amount": "985.00",
                "accrued_interest": "1.00",
                "currency": "RUB",
                "dates": { "trade": "2026-08-10", "cash_posted": "2026-08-10" }
            },
            {
                "account": harness.account.inner(),
                "type": "income",
                "instrument": harness.instrument.inner(),
                "amount": "5.00",
                "currency": "RUB",
                "kind": "coupon",
                "dates": { "cash_posted": "2026-08-20" }
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert!(
        verdicts
            .as_array()
            .expect("массив вердиктов")
            .iter()
            .all(|verdict| verdict["verdict"] == "provisional"),
        "{verdicts}"
    );
    install_known_principal_snapshot(
        &path,
        harness.owner,
        harness.account,
        contour_id.parse().expect("идентификатор контура"),
    );

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-08-26"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");

    let bond_metrics = report["bond_metrics"].as_array().expect("bond_metrics");
    assert_eq!(bond_metrics.len(), 1);
    let bond = &bond_metrics[0];
    assert_eq!(bond["account"], json!(harness.account.inner()));
    assert_eq!(bond["custody"], json!(harness.custody.inner()));
    assert_eq!(bond["instrument"], json!(harness.instrument.inner()));
    let attributes = report["bond_attributes"]
        .as_array()
        .expect("bond_attributes");
    assert_eq!(attributes.len(), 1);
    assert_eq!(attributes[0]["accrued_interest"]["value"], "10.20");
    assert_eq!(
        attributes[0]["accrued_interest_payable_on_termination"]["not_computable"],
        "accrued_observation_missing"
    );
    assert_eq!(attributes[0]["next_posting_date"], "2026-12-02");
    assert_eq!(attributes[0]["next_principal_return_finality"], "final");

    let scenarios = bond["scenarios"].as_array().expect("сценарии");
    assert_eq!(scenarios.len(), 3);
    assert!(
        scenarios
            .iter()
            .any(|scenario| scenario["prospective"]["irr_label"] == "yield_to_maturity")
    );
    assert!(
        scenarios
            .iter()
            .any(|scenario| scenario["prospective"]["irr_label"] == "yield_to_offer")
    );

    let ytm = scenarios
        .iter()
        .find(|scenario| scenario["prospective"]["irr_label"] == "yield_to_maturity")
        .expect("YTM");
    assert_eq!(
        ytm["prospective"]["c0"]["value"]["currency"], "RUB",
        "{report:#}"
    );
    assert_eq!(
        ytm["prospective"]["c0"]["value"]["value"], "9860.200",
        "{report:#}"
    );
    assert_eq!(
        ytm["prospective"]["metrics"]["value"]["terminal_wealth"]["value"],
        "10050",
    );
    assert_eq!(
        ytm["prospective"]["metrics"]["value"]["postings"][0]["amount"]["value"],
        "50",
    );
    assert_eq!(
        ytm["prospective"]["metrics"]["value"]["terminal_wealth"]["currency"],
        "RUB"
    );
    assert!(
        !ytm["prospective"]["metrics"]["value"]["zero_reinvestment_note"]
            .as_str()
            .expect("пояснение")
            .is_empty()
    );
    let lifetime = ytm["lifetime"]["value"].as_array().expect("когорты");
    assert_eq!(lifetime.len(), 1);
    assert_eq!(lifetime[0]["quantity"], "10");
    assert_eq!(lifetime[0]["c0"]["value"]["currency"], "RUB");
    assert_eq!(lifetime[0]["c0"]["value"]["value"], "986.00");
    assert_eq!(
        lifetime[0]["metrics"]["value"]["terminal_wealth"]["value"],
        "10055.00"
    );
    assert!(
        !lifetime[0]["irr_absent_because"]
            .as_str()
            .expect("причина отсутствия IRR")
            .is_empty()
    );

    let refusal = scenarios
        .iter()
        .find(|scenario| scenario["prospective"]["terminal_date"] == "2026-08-26")
        .expect("отказная offer-ставка");
    assert_eq!(refusal["prospective"]["irr"]["value"], "");
    assert_eq!(refusal["prospective"]["irr"]["error_bound"], "");
    assert_eq!(
        refusal["prospective"]["irr"]["not_computable"],
        "solver_refused"
    );
    assert!(
        !refusal["prospective"]["irr"]["detail"]
            .as_str()
            .expect("деталь отказа")
            .is_empty()
    );
    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn returns_report_loads_official_fx_from_market_store() {
    let harness = harness();
    seed_market(&harness).await;

    let contour = json!({
        "title": "Долларовый отчёт",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "рыночный курс",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "100.00",
            "currency": "USD",
            "dates": { "cash_posted": "2026-08-03" },
            "idempotency_key": "usd-deposit"
        }]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-08-03"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["contributed"]["value"], "8000.0000");
    assert_eq!(report["terminal_value"]["value"], "8000.0000");
    assert_eq!(report["applied_rules"]["fx_source"], "cbr_official");
}

#[tokio::test]
async fn repeating_an_idempotent_operation_returns_the_same_event() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" },
            "idempotency_key": "one"
        }]
    });
    let (_, first) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    let (_, second) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;

    assert_eq!(first[0]["verdict"], "provisional");
    assert_eq!(second[0]["verdict"], "duplicate");
    assert_eq!(first[0]["event_id"], second[0]["event_id"]);
}

#[tokio::test]
async fn the_openapi_document_declares_bearer_security() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        spec["components"]["securitySchemes"]["bearer"].is_object(),
        "спека обязана описывать схему аутентификации"
    );
}

#[tokio::test]
async fn the_openapi_document_exposes_only_source_price_qualities() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        spec["components"]["schemas"]["PriceQualityDto"]["enum"],
        json!(["executable", "previous_close", "owner_estimate"])
    );
}

#[tokio::test]
async fn the_openapi_document_declares_report_quality_and_liquidation_fields() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let report_properties = &spec["components"]["schemas"]["ReturnsReportDto"]["properties"];
    assert!(report_properties["liquidation_value_before_exit_costs_and_tax"].is_object());

    let quality_properties = &spec["components"]["schemas"]["DataQualityDto"]["properties"];
    assert!(quality_properties["position_coverage"].is_object());
    assert!(quality_properties["executability"].is_object());

    for schema in [
        "PositionCoverageDto",
        "ExecutabilitySharesDto",
        "LiquidationEstimateDto",
        "AmountQualificationDto",
        "PriceProvenanceDto",
        "PriceOriginDto",
    ] {
        assert!(
            spec["components"]["schemas"][schema].is_object(),
            "схема {schema} обязана быть в OpenAPI"
        );
    }
}

#[tokio::test]
async fn the_openapi_document_declares_quotation_basis_provenance_fields() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let schema = &spec["components"]["schemas"]["MarketPriceDto"];
    assert!(schema["properties"]["recorded_quotation_basis"].is_object());
    assert!(schema["properties"]["quotation_basis_status"].is_object());
    assert_eq!(
        spec["components"]["schemas"]["QuotationBasisStatusDto"]["enum"],
        json!(["proven", "contradicts", "not_proven"])
    );
}

#[tokio::test]
async fn the_report_shape_is_frozen_by_a_snapshot() {
    // Поштучные проверки полей ловят неверное значение, но не ловят
    // исчезнувшее поле и не ловят появление лишнего. Снапшот ловит
    // форму целиком (§15.8).
    let harness = harness();
    let contour = json!({
        "title": "Снапшот",
        "accounts": [harness.account.inner()],
    });
    let (_, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("контур")
        .to_owned();

    let operations = json!({
        "source_label": "снапшот",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "50000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" }
        }]
    });
    call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;

    let (status, report) = call(
        &harness.router,
        post(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-01"),
            &harness.owner_token,
            &json!([]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Идентификаторы счетов случайны в каждом прогоне, а материальные
    // проблемы их называют. Заменяются они фильтром, а не редакцией
    // поля: редакция скрыла бы текст проблемы целиком, и снимок
    // перестал бы проверять то, ради чего существует, — какие именно
    // проблемы система сообщает владельцу.
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(
        r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        "[uuid]",
    );
    settings.bind(|| {
        insta::assert_json_snapshot!(report, {
            ".applied_rules.contour" => "[contour]",
        });
    });
}
#[tokio::test]
async fn an_agent_may_submit_but_may_not_administer() {
    // Область действия — заслон, а не подсказка. Агент отправляет
    // операции, но не заводит счета и не меняет состав контура: иначе
    // внешний агент, которому доверили ввод данных, получает право
    // переопределить границу контура и тем самым переписать доходность.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.agent_token,
            &json!({ "title": "Чужой счёт" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.agent_token,
            &json!({ "title": "Свой контур", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Но отправлять операции — может.
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.agent_token,
            &json!({
                "source_label": "агент",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-01-01" },
                    "idempotency_key": "agent-1",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["verdict"], "provisional");
}

#[tokio::test]
async fn a_created_account_appears_in_the_list_and_a_readonly_token_can_read_it() {
    // Счёт, который завели, обязан читаться обратно: пустой список
    // выглядит как «счетов нет», а не как «список сломан».
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Второй брокерский", "institution": "Банк" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, list) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.readonly_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let titles: Vec<&str> = list
        .as_array()
        .expect("список счетов")
        .iter()
        .map(|account| account["title"].as_str().expect("название"))
        .collect();
    assert!(
        titles.contains(&"Второй брокерский"),
        "заведённый счёт обязан быть в списке: {titles:?}"
    );
    assert!(titles.contains(&"Брокерский"), "и прежний тоже: {titles:?}");
}

#[tokio::test]
async fn each_verdict_names_the_row_it_belongs_to() {
    // Вердикты приходят по строке на операцию, и агент чинит именно ту,
    // которую ему назвали. Сбитая нумерация отправляет его править
    // здоровую строку, а больную оставляет как есть.
    let harness = harness();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "ручной ввод",
                "operations": [
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "1000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-01" },
                        "idempotency_key": "row-1",
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "-5.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-02" },
                        "idempotency_key": "row-2",
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "не число",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-03" },
                        "idempotency_key": "row-3",
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "deposit",
                        "amount": "2000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-01-04" },
                        "idempotency_key": "row-4",
                    },
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows: Vec<u64> = body
        .as_array()
        .expect("вердикты")
        .iter()
        .map(|verdict| verdict["row"].as_u64().expect("номер строки"))
        .collect();
    assert_eq!(
        rows,
        vec![1, 2, 3, 4],
        "нумерация начинается с единицы подряд"
    );
    assert_eq!(body[0]["verdict"], "provisional");
    // Вторая строка отклонена приёмкой: величина разобралась, но
    // отрицательной быть не может.
    assert_eq!(body[1]["verdict"], "rejected");
    // Третья — отклонена ещё на разборе тела запроса. Обе дороги к
    // вердикту нумеруют строки, и обе обязаны нумеровать одинаково.
    assert_eq!(body[2]["verdict"], "rejected");
    assert_eq!(body[3]["verdict"], "provisional");
}

#[tokio::test]
async fn a_csv_document_resolves_account_names_and_numbers_its_rows() {
    // Справочник имён строится из счетов владельца. Пустой справочник
    // отклонил бы весь документ по полю account, и «не завели счёт»
    // стало бы неотличимо от «сломался справочник».
    let harness = harness();
    let document = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key\n\
        2025-01-01,deposit,Брокерский,,,,,1000.00,,,RUB,csv-1\n\
        2025-01-02,deposit,Нет такого счёта,,,,,1000.00,,,RUB,csv-2\n\
        2025-01-03,withdrawal,Брокерский,,,,,500.00,,,RUB,csv-3\n";
    let request = Request::builder()
        .uri("/v1/ingest/csv")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "text/csv")
        .body(Body::from(document))
        .expect("запрос");
    let (status, body) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let verdicts = body.as_array().expect("вердикты");
    assert_eq!(verdicts.len(), 3);
    let rows: Vec<u64> = verdicts
        .iter()
        .map(|verdict| verdict["row"].as_u64().expect("номер строки"))
        .collect();
    assert_eq!(rows, vec![1, 2, 3]);
    assert_eq!(verdicts[0]["verdict"], "provisional");
    assert_eq!(verdicts[1]["verdict"], "rejected");
    assert_eq!(verdicts[1]["field"], "account");
    assert_eq!(verdicts[2]["verdict"], "provisional");
}

#[tokio::test]
async fn неоднозначное_название_счёта_отвергается_при_разрешении_строки() {
    let (harness, path) = harness_on_disk();
    {
        let store = SqliteStore::open(&path).expect("второе соединение");
        store
            .upsert_account(&AccountRecord {
                id: AccountId::new_random(),
                owner: harness.owner,
                title: "Брокерский".into(),
                institution: None,
            })
            .expect("дубликат счёта");
        store
            .upsert_account(&AccountRecord {
                id: AccountId::new_random(),
                owner: harness.owner,
                title: "Однозначный".into(),
                institution: None,
            })
            .expect("однозначный счёт");
    }

    let document = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key\n\
        2025-01-01,deposit,Брокерский,,,,,1000.00,,,RUB,duplicate\n\
        2025-01-02,deposit,Однозначный,,,,,1000.00,,,RUB,unique\n";
    let request = Request::builder()
        .uri("/v1/ingest/csv")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "text/csv")
        .body(Body::from(document))
        .expect("запрос");
    let (status, body) = call(&harness.router, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["verdict"], "rejected");
    assert_eq!(body[0]["field"], "account");
    let actual = body[0]["actual"].as_str().expect("причина отказа");
    assert_eq!(actual, "Брокерский: название счёта неоднозначно: 2 счёта");
    assert_eq!(body[1]["verdict"], "provisional");

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn an_unparsable_report_date_is_refused_and_a_valid_one_is_honoured() {
    // Молчаливое умолчание «сегодня» вместо непонятой даты выдало бы
    // отчёт не на ту дату — с виду нормальный, но про другой период.
    let harness = harness();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Портфель", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("контур").to_owned();

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "ручной ввод",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-06-01" },
                    "idempotency_key": "as-of-1",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=вчера"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "as_of");

    // Дата раньше операции: отчёт на неё обязан отличаться от отчёта
    // на сегодня, иначе параметр ни на что не влияет.
    let (status, early) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2025-01-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{early}");
    assert_eq!(early["as_of"], "2025-01-01");

    let (status, today) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{today}");
    assert_eq!(today["as_of"], "2026-01-01", "умолчание — дата часов");
    assert_ne!(
        early["contributed"], today["contributed"],
        "отчёт до первой операции обязан отличаться от отчёта после неё"
    );
}

#[tokio::test]
async fn a_report_for_today_leaves_a_snapshot_and_a_report_for_a_past_date_does_not() {
    // Ключ снимка — контур, его версия и версия правила; даты в ключе
    // нет. Снимок, построенный по срезу на прошлую дату, лёг бы под тем
    // же ключом и молча подменил бы состояние следующему запросу.
    // Проверяется прямым запросом к базе: снаружи подмена выглядит
    // как обычный ответ, просто с неверными числами.
    let (harness, path) = harness_on_disk();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Портфель", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("контур").to_owned();

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "ручной ввод",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-06-01" },
                    "idempotency_key": "snap-1",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let snapshots = |path: &std::path::Path| -> u32 {
        let probe = SqliteStore::open(path).expect("второе соединение");
        probe
            .connection()
            .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
            .expect("счёт снимков")
    };
    let usages = |path: &std::path::Path| -> u32 {
        let probe = SqliteStore::open(path).expect("второе соединение");
        probe
            .connection()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .expect("счёт обращений")
    };

    // Отчёт на прошлую дату снимка не оставляет.
    let (status, _) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2025-12-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        snapshots(&path),
        0,
        "снимок по срезу на прошлую дату сохраняться не должен"
    );

    // Отчёт на сегодня — оставляет, и он читается обратно: повторный
    // запрос обязан дать те же числа.
    let (status, first) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(snapshots(&path), 1, "отчёт на сегодня оставляет снимок");

    let (status, second) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first, second,
        "отчёт, посчитанный со снимка, обязан совпасть с посчитанным без него"
    );
    assert_eq!(snapshots(&path), 1, "снимок заменяется, а не задваивается");

    // Каждое обращение с токеном попадает в журнал (§14).
    assert!(
        usages(&path) >= 4,
        "журнал обращений пуст: попытки с токеном обязаны быть видны"
    );

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn an_event_added_behind_the_snapshot_boundary_forces_a_recompute_not_a_failure() {
    // Снимок — кэш, и его непригодность не является ошибкой работы.
    // Событие, пришедшее задним числом до границы снимка, меняет
    // отпечаток свёрнутого префикса: ядро отказывается продвигать
    // снимок, а оболочка обязана пересчитать журнал целиком и всё
    // равно ответить — причём ответить с учётом нового события.
    let harness = harness();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Портфель", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("контур").to_owned();

    let deposit = |key: &str, day: &str, amount: &str| {
        json!({
            "source_label": "ручной ввод",
            "operations": [{
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": amount,
                "currency": "RUB",
                "dates": { "cash_posted": day },
                "idempotency_key": key,
            }],
        })
    };

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &deposit("late", "2025-06-01", "1000.00"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Первый отчёт на сегодня оставляет снимок.
    let (status, before) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["contributed"]["value"], "1000.00");

    // Событие задним числом — раньше уже свёрнутого.
    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &deposit("early", "2025-01-01", "500.00"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, after) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "непригодный снимок — повод пересчитать, а не отказать: {after}"
    );
    assert_eq!(
        after["contributed"]["value"], "1500.00",
        "событие задним числом обязано войти в расчёт"
    );
    assert_eq!(
        after["history_starts"], "2025-01-01",
        "и сдвинуть начало истории"
    );
}

/// Токен брокера, который тесты отдают серверу. Значение подобрано
/// так, чтобы его подстрока не встречалась в ответе случайно.
const BROKER_TOKEN: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";

fn add_broker_access_body() -> Value {
    json!({ "broker": "tinkoff", "environment": "sandbox", "token": BROKER_TOKEN })
}

#[tokio::test]
async fn a_provisioned_broker_access_never_echoes_the_token_back() {
    // Токен, вернувшийся в ответе, попадёт в лог клиента, в историю
    // отладочного запроса и в снимок экрана. То, чего сервер не отдал,
    // туда попасть не может (§14).
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &add_broker_access_body(),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["broker"], "tinkoff");
    assert!(
        !body.to_string().contains(BROKER_TOKEN),
        "токен не возвращается наружу ни одним полем: {body}"
    );
    assert!(
        body["revoked_at"].is_null(),
        "заведённый доступ действует: {body}"
    );
}

#[tokio::test]
async fn the_scope_of_a_broker_access_is_read_only_whatever_the_client_sends() {
    // Область прав задаёт система, а не клиент: торговые права
    // не запрашиваются ни при каких условиях (§14). Присланное клиентом
    // поле игнорируется молча — принять его означало бы завести доступ,
    // которым можно торговать.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &json!({
                "broker": "tinkoff",
                "environment": "sandbox",
                "token": BROKER_TOKEN,
                "scope": "full_access",
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["scope"], "read_only",
        "область прав задаёт система, а не клиент: {body}"
    );
}

#[tokio::test]
async fn a_provisioned_access_is_listed_and_a_revoked_one_stops_being_current() {
    // Отзыв — это не удаление: запись остаётся историей, но перестаёт
    // быть действующей. Пропавшая из списка запись отвечала бы «доступа
    // не было», а не «доступ отозван тогда-то».
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &add_broker_access_body(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("идентификатор").to_owned();

    let (status, list) = call(
        &harness.router,
        get("/v1/broker-access", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed = find_access(&list, &id).expect("заведённый доступ обязан быть в списке");
    assert!(
        listed["revoked_at"].is_null(),
        "только что заведённый доступ действует: {listed}"
    );

    let (status, body) = call(
        &harness.router,
        delete(&format!("/v1/broker-access/{id}"), &harness.owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, list) = call(
        &harness.router,
        get("/v1/broker-access", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed = find_access(&list, &id).expect("отозванный доступ остаётся историей");
    assert!(
        !listed["revoked_at"].is_null(),
        "отозванный доступ перестаёт быть действующим: {listed}"
    );
}

#[tokio::test]
async fn both_environments_of_one_broker_are_provisioned_side_by_side() {
    // Токены у сред разные: боевой песочница не принимает, песочный
    // не принимает бой. Значит оба доступа обязаны уживаться, иначе
    // живая проверка и боевой канал исключают друг друга.
    let harness = harness();

    let (status, sandbox) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &json!({ "broker": "tinkoff", "environment": "sandbox", "token": BROKER_TOKEN }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sandbox}");
    assert_eq!(sandbox["environment"], "sandbox");

    let (status, prod) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &json!({ "broker": "tinkoff", "environment": "prod", "token": "t.другой-токен-боевой" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{prod}");
    assert_eq!(prod["environment"], "prod");
    assert_ne!(sandbox["id"], prod["id"]);

    let (status, list) = call(
        &harness.router,
        get("/v1/broker-access", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let environments: Vec<&str> = list
        .as_array()
        .expect("список доступов")
        .iter()
        .filter_map(|access| access["environment"].as_str())
        .collect();
    assert!(environments.contains(&"prod"), "{list}");
    assert!(environments.contains(&"sandbox"), "{list}");
}

#[tokio::test]
async fn a_second_access_in_the_same_environment_is_refused_understandably() {
    // Два действующих доступа в одной среде означают, что неизвестно,
    // каким из них система ходит. Отказ обязан называть причину:
    // «внутренняя ошибка» отправила бы владельца искать поломку
    // там, где её нет, — на самом деле нужно сначала отозвать старый.
    let harness = harness();

    let (status, first) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &add_broker_access_body(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &add_broker_access_body(),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn an_access_without_a_named_environment_is_refused() {
    // Умолчание здесь означало бы песочный токен, молча записанный
    // боевым: шлюз ответит отказом на первом же обращении, а по тексту
    // отказа о среде не догадаться — проверено на живом шлюзе.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &json!({ "broker": "tinkoff", "token": BROKER_TOKEN }),
        ),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "среда без умолчания: {body}"
    );
}

#[tokio::test]
async fn an_unknown_environment_is_refused() {
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &json!({ "broker": "tinkoff", "environment": "стенд", "token": BROKER_TOKEN }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn a_read_only_token_may_not_touch_broker_access_at_all() {
    // Заведение чужого токена, чтение списка и отзыв — управление
    // доступом, а не чтение портфеля. Токен чтения не управляет ничем.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.readonly_token,
            &add_broker_access_body(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");

    let (status, body) = call(
        &harness.router,
        get("/v1/broker-access", Some(&harness.readonly_token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = call(
        &harness.router,
        delete(
            &format!("/v1/broker-access/{}", Uuid::new_v4()),
            &harness.readonly_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// Запись списка по идентификатору.
fn find_access(list: &Value, id: &str) -> Option<Value> {
    list.as_array()?
        .iter()
        .find(|access| access["id"] == id)
        .cloned()
}

// --- Присвоение экземпляра и управление токенами (§14) ---

#[tokio::test]
async fn the_printed_code_claims_the_instance_and_the_token_it_gives_works() {
    // Присвоение — не регистрация: код печатается один раз при старте,
    // и прочитать его может только тот, кто запустил программу. Доступ
    // к консоли и есть доказательство права на экземпляр.
    let (router, code) = unclaimed_harness().await;

    let (status, body) = call(
        &router,
        post_public("/v1/claim", &json!({ "code": code, "label": "ноутбук" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["label"], "ноутбук");
    assert_eq!(body["scope"], "owner");
    let token = body["token"].as_str().expect("токен владельца").to_owned();
    assert!(!token.is_empty(), "присвоение обязано выдать токен: {body}");

    // Выданный токен — настоящий: им проходит защищённый запрос.
    let (status, accounts) = call(&router, get("/v1/accounts", Some(&token))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "токен присвоения обязан пускать: {accounts}"
    );
}

#[tokio::test]
async fn the_same_claim_code_never_works_twice() {
    // Код одноразовый, и это свойство самого кода: он стирается из
    // памяти в момент использования. Второй обмен — это либо повтор
    // запроса, либо чужая рука; различить их нечем, и оба получают
    // тот же отказ, что и неверный код.
    let (router, code) = unclaimed_harness().await;

    let (status, body) = call(
        &router,
        post_public("/v1/claim", &json!({ "code": code, "label": "ноутбук" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = call(
        &router,
        post_public("/v1/claim", &json!({ "code": code, "label": "ещё раз" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "claim_refused");
}

#[tokio::test]
async fn a_wrong_claim_code_is_refused_and_does_not_burn_the_right_one() {
    // Неверный код не стирает верный: иначе любой посторонний одним
    // запросом с мусором закрывал бы присвоение навсегда.
    let (router, code) = unclaimed_harness().await;

    let (status, body) = call(
        &router,
        post_public(
            "/v1/claim",
            &json!({ "code": "не тот код", "label": "чужой" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "claim_refused");

    let (status, body) = call(
        &router,
        post_public("/v1/claim", &json!({ "code": code, "label": "ноутбук" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "верный код обязан пережить чужую попытку: {body}"
    );
}

#[tokio::test]
async fn claiming_is_closed_once_an_owner_exists_even_with_a_valid_code() {
    // Владелец мог завестись помимо присвоения — командой консоли уже
    // после старта, — и напечатанный тогда код устарел. Принять его
    // значило бы завести второго владельца: пустой портфель в чужой
    // базе, а у первого пропавшие деньги.
    let (router, code, path) = unclaimed_harness_on_disk().await;

    {
        let store = SqliteStore::open(&path).expect("второе соединение");
        store
            .insert_token(
                &TokenRecord {
                    id: Uuid::new_v4(),
                    owner: OwnerId::new_random(),
                    label: "заведён с консоли".into(),
                    scope: TokenScope::Owner,
                    revoked: false,
                },
                &hash_token("issued-from-the-console"),
            )
            .expect("токен владельца");
    }

    let (status, body) = call(
        &router,
        post_public("/v1/claim", &json!({ "code": code, "label": "опоздавший" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_claimed");
}

#[tokio::test]
async fn an_instance_with_an_owner_generates_no_claim_code_at_all() {
    // Секрет, который никому не нужен, всё равно остаётся секретом,
    // лежащим в памяти. Владелец есть — код не порождается вовсе,
    // и присвоение отвечает отказом на любой предъявленный код.
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner: OwnerId::new_random(),
                label: "владелец".into(),
                scope: TokenScope::Owner,
                revoked: false,
            },
            &hash_token("owner-secret-token"),
        )
        .expect("токен владельца");
    let state = claim_state(store);

    assert!(
        iaam_server::claim::arm(&state)
            .await
            .expect("состояние базы прочитано")
            .is_none(),
        "владелец есть — код присвоения порождаться не должен"
    );

    let (router, _) = build(state);
    let (status, body) = call(
        &router,
        post_public(
            "/v1/claim",
            &json!({ "code": "любой", "label": "посторонний" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "claim_refused");
}

#[tokio::test]
async fn an_owner_token_is_never_issued_through_the_api() {
    // Владелец заводится присвоением экземпляра или консолью. Маршрут,
    // выпускающий полный доступ, превращал бы один украденный токен
    // в неотличимые копии, и отзыв исходного ничего бы не менял.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.owner_token,
            &json!({ "label": "второй владелец", "scope": "owner" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "scope");
    assert_eq!(body["actual"], "owner");
}

#[tokio::test]
async fn an_agent_token_may_not_manage_tokens_at_all() {
    // Агент отправляет операции, но не раздаёт права на портфель:
    // иначе украденный агентский токен выписывал бы себе замену
    // быстрее, чем владелец успевал бы его отозвать.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.agent_token,
            &json!({ "label": "ещё агент", "scope": "agent" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");

    let (status, body) = call(
        &harness.router,
        get("/v1/tokens", Some(&harness.agent_token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = call(
        &harness.router,
        delete(
            &format!("/v1/tokens/{}", Uuid::new_v4()),
            &harness.agent_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn the_token_list_carries_neither_tokens_nor_their_hashes() {
    // Хеш — это то, что достаточно подставить в запрос поиска, чтобы
    // система признала предъявителя своим. Список выданных токенов,
    // показывающий хеши, был бы списком отмычек. Проверка подстрокой
    // по всему телу, а не по полям: поле, добавленное завтра, глазами
    // не проверяется.
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.owner_token,
            &json!({ "label": "домашний агент", "scope": "agent" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let issued = created["token"].as_str().expect("токен").to_owned();

    let (status, list) = call(
        &harness.router,
        get("/v1/tokens", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let body = list.to_string();

    assert!(
        body.contains("домашний агент"),
        "выданный токен обязан быть в списке: {body}"
    );
    for secret in [&issued, &harness.owner_token, &harness.agent_token] {
        assert!(
            !body.contains(secret.as_str()),
            "токен утёк в список выданных токенов: {body}"
        );
        assert!(
            !body.contains(&hash_token(secret)),
            "хеш токена утёк в список выданных токенов: {body}"
        );
    }
}

#[tokio::test]
async fn a_revoked_token_stops_being_accepted_and_stays_in_the_history() {
    // Отзыв — это не удаление: запись остаётся историей, но перестаёт
    // пускать. Пропавшая из списка запись отвечала бы «токена не было»,
    // а не «токен отозван тогда-то».
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.owner_token,
            &json!({ "label": "телефон", "scope": "read_only" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("идентификатор").to_owned();
    let token = created["token"].as_str().expect("токен").to_owned();

    let (status, accounts) = call(&harness.router, get("/v1/accounts", Some(&token))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "выпущенный токен пускает: {accounts}"
    );

    let (status, body) = call(
        &harness.router,
        delete(&format!("/v1/tokens/{id}"), &harness.owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = call(&harness.router, get("/v1/accounts", Some(&token))).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "отозванный токен не пускает: {body}"
    );

    let (status, list) = call(
        &harness.router,
        get("/v1/tokens", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed = find_access(&list, &id).expect("отозванный токен остаётся историей");
    assert!(
        !listed["revoked_at"].is_null(),
        "отозванный токен перестаёт быть действующим: {listed}"
    );
}

#[tokio::test]
async fn a_token_of_another_owner_is_as_absent_as_a_missing_one() {
    // Идентификатор токена не является правом на него: без владельца
    // в запросе отзыва любой знающий чужой идентификатор отзывал бы
    // чужой токен. Ответ одинаков с «нет такого» намеренно — разные
    // сообщили бы постороннему, что запись существует (§14).
    let (harness, path) = harness_on_disk();

    // Токен второго владельца заводится вторым соединением к той же
    // базе: через API его не завести — владелец в системе один, и это
    // ровно то состояние, в котором чужой токен вообще может появиться.
    let stranger_token = "stranger-secret-token";
    let stranger = TokenRecord {
        id: Uuid::new_v4(),
        owner: OwnerId::new_random(),
        label: "чужой".into(),
        scope: TokenScope::Agent,
        revoked: false,
    };
    {
        let store = SqliteStore::open(&path).expect("второе соединение");
        store
            .insert_token(&stranger, &hash_token(stranger_token))
            .expect("чужой токен");
    }

    let (status, body) = call(
        &harness.router,
        delete(&format!("/v1/tokens/{}", stranger.id), &harness.owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "not_found");

    // Отсутствующий токен отвечает ровно тем же: отличить одно
    // от другого по ответу нельзя.
    let (missing, body) = call(
        &harness.router,
        delete(
            &format!("/v1/tokens/{}", Uuid::new_v4()),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(missing, status, "{body}");

    // И чужой токен остался действующим: отказ обязан быть отказом,
    // а не «не сказали, но сделали».
    let (status, accounts) = call(&harness.router, get("/v1/accounts", Some(stranger_token))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "чужой токен не отозван чужими руками: {accounts}"
    );
}

#[tokio::test]
async fn classification_rules_are_visible_versioned_and_retirable() {
    let harness = harness();
    let request = json!({
        "matcher": r#"{"kind":"income"}"#,
        "outcome": r#"{"kind":"external_flow"}"#,
    });
    let (status, created) = call(
        &harness.router,
        post("/v1/classification-rules", &harness.owner_token, &request),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["version"], 1);
    assert!(!created.to_string().contains(BROKER_TOKEN), "{created}");
    let id = created["id"].as_str().expect("идентификатор").to_owned();

    let (status, history) = call(
        &harness.router,
        get("/v1/classification-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history.as_array().expect("история").len(), 1);
    assert_eq!(history[0]["matcher"], r#"{"kind":"income"}"#);
    assert!(!history.to_string().contains(BROKER_TOKEN), "{history}");

    let (status, body) = call(
        &harness.router,
        delete(
            &format!("/v1/classification-rules/{id}"),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, history) = call(
        &harness.router,
        get("/v1/classification-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert!(!history[0]["retired_at"].is_null(), "{history}");
}

#[tokio::test]
async fn only_the_owner_can_manage_classification_rules() {
    let harness = harness();
    let rule = json!({
        "matcher": r#"{"kind":"income"}"#,
        "outcome": r#"{"kind":"external_flow"}"#,
    });
    for (method, body) in [
        (
            "GET",
            get("/v1/classification-rules", Some(&harness.readonly_token)),
        ),
        (
            "POST",
            post("/v1/classification-rules", &harness.readonly_token, &rule),
        ),
        (
            "DELETE",
            delete(
                &format!("/v1/classification-rules/{}", Uuid::new_v4()),
                &harness.readonly_token,
            ),
        ),
    ] {
        let (status, response) = call(&harness.router, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {response}");
        assert_eq!(response["code"], "forbidden", "{method}: {response}");
    }
}

#[tokio::test]
async fn reconciliation_returns_nonempty_status_content() {
    let (harness, path) = harness_on_disk();
    add_reconciliation_assertion(&path, harness.owner, harness.account);

    let (status, response) = call(
        &harness.router,
        get(
            &format!(
                "/v1/reconciliation?account={}&from=2025-01-01&to=2025-01-31",
                harness.account.inner()
            ),
            Some(&harness.readonly_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let statuses = response.as_array().expect("статусы сверки");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0]["account"], json!(harness.account.inner()));
    assert_eq!(statuses[0]["from"], "2025-01-01");
    assert_eq!(statuses[0]["to"], "2025-01-31");
    assert_eq!(statuses[0]["dimensions"][0]["dimension"], "cash");
    assert_eq!(statuses[0]["dimensions"][0]["status"], "provisional");
    assert_eq!(statuses[0]["outcomes"][0]["claim"], "cash_balance");
    assert_eq!(statuses[0]["outcomes"][0]["outcome"], "not_comparable");
    drop(harness);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn reconciliation_balance_returns_nonempty_status_content() {
    let (harness, path) = harness_on_disk();
    add_reconciliation_assertion(&path, harness.owner, harness.account);
    let balance = json!({
        "account": harness.account.inner(),
        "from": "2025-01-01",
        "to": "2025-01-31",
        "at": "closing",
        "cash": { "currency": "RUB", "amount": "100.00" },
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/reconciliation/balance", &harness.owner_token, &balance),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let statuses = response.as_array().expect("статусы сверки");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0]["account"], json!(harness.account.inner()));
    assert_eq!(statuses[0]["dimensions"][0]["dimension"], "cash");
    assert_eq!(statuses[0]["dimensions"][0]["status"], "provisional");
    assert_eq!(statuses[0]["outcomes"][0]["claim"], "cash_balance");
    assert_eq!(statuses[0]["outcomes"][0]["outcome"], "not_comparable");
    drop(harness);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn broker_sync_numbers_recorded_verdicts_from_one() {
    let channel: Arc<dyn BrokerChannel> = Arc::new(PopulatedChannel {
        source: iaam_core::reconciliation::evidence::SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("contract-test".to_owned()),
            document: None,
        },
    });
    let factory: Arc<dyn BrokerChannelFactory> = Arc::new(FixedChannelFactory { channel });
    let harness = harness_with_factory(
        SqliteStore::open_in_memory().expect("база в памяти"),
        Some(factory),
    );
    let body = json!({
        "account": harness.account.inner(),
        "from": "2025-01-01",
        "to": "2025-01-31",
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/brokers/tinkoff/sync", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["recorded"].as_array().expect("вердикты").len(), 1);
    assert_eq!(response["recorded"][0]["verdict"], "provisional");
    assert_eq!(response["recorded"][0]["row"], 1);
}

#[tokio::test]
async fn broker_sync_returns_the_scenario_outcome() {
    let channel: Arc<dyn BrokerChannel> = Arc::new(EmptyChannel {
        source: iaam_core::reconciliation::evidence::SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("contract-test".to_owned()),
            document: None,
        },
    });
    let factory: Arc<dyn BrokerChannelFactory> = Arc::new(FixedChannelFactory { channel });
    let harness = harness_with_factory(
        SqliteStore::open_in_memory().expect("база в памяти"),
        Some(factory),
    );
    let body = json!({
        "account": harness.account.inner(),
        "from": "2025-01-01",
        "to": "2025-01-31",
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/brokers/tinkoff/sync", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["recorded"], json!([]));
    assert_eq!(response["duplicates"], 0);
    assert_eq!(response["assertions"], 0);
    assert!(!response.to_string().contains(BROKER_TOKEN), "{response}");
}

#[tokio::test]
async fn broker_sync_reports_unconfigured_access_as_503_and_rejects_read_only() {
    let harness = harness();
    let body = json!({
        "account": harness.account.inner(),
        "from": "2025-01-01",
        "to": "2025-01-31",
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/brokers/tinkoff/sync", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{response}");
    assert_eq!(response["code"], "not_configured");
    assert!(!response.to_string().contains(BROKER_TOKEN), "{response}");

    let (status, response) = call(
        &harness.router,
        post("/v1/brokers/tinkoff/sync", &harness.readonly_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    assert_eq!(response["code"], "forbidden");
}

#[tokio::test]
async fn ingest_verdicts_return_their_populated_fields() {
    let harness = harness();
    let body = json!({
        "source_label": "контракт",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" },
            "idempotency_key": "verdict-fields",
        }],
    });

    let (status, first) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let provisional_event = first[0]["event_id"].as_str().expect("provisional event_id");
    assert!(
        Uuid::parse_str(provisional_event).is_ok(),
        "event_id обязан быть UUID: {provisional_event}"
    );

    let (status, second) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second[0]["verdict"], "duplicate");
    assert_eq!(second[0]["event_id"], provisional_event);
    assert!(
        second[0]["event_id"].as_str().is_some(),
        "duplicate обязан назвать существующее событие"
    );

    let rejected_body = json!({
        "source_label": "контракт",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "-5.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-02" },
        }],
    });
    let (status, rejected) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &rejected_body,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rejected}");
    assert_eq!(rejected[0]["verdict"], "rejected");
    assert_eq!(rejected[0]["field"], "amount");
    assert_eq!(rejected[0]["expected"], "положительная величина");
    assert_eq!(rejected[0]["actual"], "-5.00");
}

#[tokio::test]
async fn a_document_verdict_return_contains_its_detail() {
    let harness = harness();
    let request = Request::builder()
        .uri(format!("/v1/documents?account={}", harness.account.inner()))
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header(
            "Content-Type",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
        .body(Body::from(
            include_bytes!("../../../tests/fixtures/reports/tinkoff-synthetic.xlsx").as_slice(),
        ))
        .expect("запрос");
    let (status, response) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let rows = response["rows"].as_array().expect("строки документа");
    let unsupported = rows
        .iter()
        .find(|row| row["verdict"] == "unsupported")
        .expect("отчёт обязан вернуть строку вне периметра");
    assert_eq!(unsupported["detail"], "repo");
}

#[test]
fn verdict_dto_json_contains_every_variant_field() {
    let event = iaam_core::ids::EventId::new_random();
    let account = AccountId::new_random();
    let cases = [
        (
            Verdict::Accepted { event },
            json!({
                "row": 7,
                "verdict": "accepted",
                "event_id": event.inner(),
            }),
        ),
        (
            Verdict::Provisional { event },
            json!({
                "row": 7,
                "verdict": "provisional",
                "event_id": event.inner(),
            }),
        ),
        (
            Verdict::Discrepancy {
                event,
                account,
                dimension: Dimension::Cash,
                detail: "остаток не сошёлся".into(),
            },
            json!({
                "row": 7,
                "verdict": "discrepancy",
                "event_id": event.inner(),
                "account_id": account.inner(),
                "dimension": "cash",
                "detail": "остаток не сошёлся",
            }),
        ),
        (
            Verdict::NeedsReconciliation {
                account,
                dimension: Dimension::Cash,
            },
            json!({
                "row": 7,
                "verdict": "needs_reconciliation",
                "account_id": account.inner(),
                "dimension": "cash",
            }),
        ),
        (
            Verdict::Duplicate { existing: event },
            json!({
                "row": 7,
                "verdict": "duplicate",
                "event_id": event.inner(),
            }),
        ),
        (
            Verdict::NeedsClassification {
                question: "перевод внутренний?".into(),
            },
            json!({
                "row": 7,
                "verdict": "needs_classification",
                "detail": "перевод внутренний?",
            }),
        ),
        (
            Verdict::Unsupported {
                reason: "repo".into(),
            },
            json!({
                "row": 7,
                "verdict": "unsupported",
                "detail": "repo",
            }),
        ),
        (
            Verdict::Rejected {
                rejection: Rejection {
                    field: "amount".into(),
                    expected: "положительная величина".into(),
                    actual: "-5.00".into(),
                },
            },
            json!({
                "row": 7,
                "verdict": "rejected",
                "field": "amount",
                "expected": "положительная величина",
                "actual": "-5.00",
            }),
        ),
    ];

    for (domain, expected) in cases {
        let actual = serde_json::to_value(VerdictDto::from_domain(7, &domain))
            .expect("вердикт сериализуется");
        assert_eq!(actual, expected, "содержимое вердикта {domain:?}");
    }
}

fn seed_directory(store: &SqliteStore) -> InstrumentId {
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".into(),
            title: "Сбербанк".into(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("инструмент");
    store
        .record_alias(&AliasRecord {
            namespace: AliasNamespace::Isin,
            value: "RU000A0JX0J2".into(),
            instrument,
            interval: AliasInterval {
                valid_from: date!(2020 - 01 - 01),
                valid_to: None,
            },
            source: SourceId::new_random(),
        })
        .expect("псевдоним");
    instrument
}

fn seeded_harness() -> Harness {
    let store = SqliteStore::open_in_memory().expect("база в памяти");
    let instrument = seed_directory(&store);
    let mut harness = harness_with(store);
    harness.instrument = instrument;
    harness
}

fn server_with_one_alias() -> (Router, String, InstrumentId) {
    let harness = seeded_harness();
    (harness.router, harness.owner_token, harness.instrument)
}

fn server_with_one_alias_and_agent_token() -> (Router, String, InstrumentId) {
    let harness = seeded_harness();
    (harness.router, harness.agent_token, harness.instrument)
}

#[tokio::test]
async fn listing_instruments_returns_the_global_directory() {
    let (app, token, instrument) = server_with_one_alias();
    let (status, body) = call(&app, get("/v1/instruments", Some(&token))).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.as_array()
            .expect("список")
            .iter()
            .any(|item| item["id"] == instrument.inner().to_string())
    );
}

#[tokio::test]
async fn resolving_a_known_code_returns_its_instrument() {
    let (app, token, instrument) = server_with_one_alias();
    let (status, body) = call(
        &app,
        post(
            "/v1/instruments/resolve",
            &token,
            &json!({"namespace": "isin", "value": "RU000A0JX0J2", "on": "2024-03-01"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["instrument"], instrument.inner().to_string());
}

#[tokio::test]
async fn resolving_an_unknown_code_is_a_404() {
    let (app, token, _) = server_with_one_alias();
    let (status, _) = call(
        &app,
        post(
            "/v1/instruments/resolve",
            &token,
            &json!({"namespace": "isin", "value": "RU000ANOPE00", "on": "2024-03-01"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolving_a_code_outside_its_interval_names_the_known_range() {
    let (app, token, _) = server_with_one_alias();
    let (status, body) = call(
        &app,
        post(
            "/v1/instruments/resolve",
            &token,
            &json!({"namespace": "isin", "value": "RU000A0JX0J2", "on": "1999-01-01"}),
        ),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "известный код вне интервала — не то же самое, что неизвестный код"
    );
    assert_eq!(body["field"], "on");
    assert!(
        body["expected"]
            .as_str()
            .is_some_and(|value| value.contains("2020-01-01"))
    );
    assert!(
        body["actual"]
            .as_str()
            .is_some_and(|value| value.contains("1999-01-01"))
    );
}

#[tokio::test]
async fn an_invalid_namespace_is_a_422_naming_the_field() {
    let (app, token, _) = server_with_one_alias();
    let (status, body) = call(
        &app,
        post(
            "/v1/instruments/resolve",
            &token,
            &json!({"namespace": "cusip", "value": "037833100", "on": "2024-03-01"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["field"], "namespace");
}

#[tokio::test]
async fn the_instrument_dto_does_not_leak_the_alias_source() {
    let (app, token, instrument) = server_with_one_alias();
    let (status, body) = call(
        &app,
        get(
            &format!("/v1/instruments/{}", instrument.inner()),
            Some(&token),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.to_string().contains("source"),
        "SourceId указывает на документ владельца: наружу он не идёт (§14)"
    );
}

#[tokio::test]
async fn an_agent_token_may_not_write_to_the_directory() {
    let (app, agent_token, _) = server_with_one_alias_and_agent_token();
    let (status, _) = call(
        &app,
        post(
            "/v1/instruments",
            &agent_token,
            &json!({"symbol": "HACK", "title": "Подменыш", "kind": "share"}),
        ),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "справочник глобален: чужая запись портит данные всех владельцев"
    );
}

#[tokio::test]
async fn an_owner_can_record_an_instrument_in_directory() {
    let harness = harness();
    let instrument = Uuid::new_v4().to_string();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/instruments",
            &harness.owner_token,
            &json!({
                "id": instrument,
                "kind": "share",
                "symbol": "GAZP",
                "title": "Газпром",
                "denomination_currency": "RUB",
                "settlement_currency": "RUB",
                "quote_currency": "RUB",
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["id"], instrument);
    assert_eq!(body["symbol"], "GAZP");

    let (status, stored) = call(
        &harness.router,
        get(
            &format!("/v1/instruments/{instrument}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored["title"], "Газпром");
    assert_eq!(stored["quote_currency"], "RUB");
}

#[tokio::test]
async fn market_reference_routes_require_auth_and_preserve_provenance() {
    let harness = harness();
    seed_market(&harness).await;

    let prices_path = format!(
        "/v1/market/prices?instrument={}&board=TQBR&session=1&from=2026-08-01&to=2026-08-03&knowledge_as_of=2099-01-01T00:00:00Z",
        harness.instrument.inner()
    );
    let fx_path = "/v1/market/fx?from=USD&to=RUB&from_date=2026-08-01&to_date=2026-08-03&knowledge_as_of=2099-01-01T00:00:00Z";
    let key_rate_path =
        "/v1/market/key-rate?from=2026-08-03&to=2026-08-10&knowledge_as_of=2099-01-01T00:00:00Z";

    for token in [&harness.owner_token, &harness.agent_token] {
        let (status, prices) = call(&harness.router, get(&prices_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        let prices = prices.as_array().expect("массив цен");
        assert_eq!(prices.len(), 2);
        for price in prices {
            for field in [
                "value",
                "date",
                "source",
                "observed_at",
                "quality",
                "quotation_basis",
                "recorded_quotation_basis",
                "quotation_basis_status",
            ] {
                assert!(price.get(field).is_some(), "у цены нет {field}: {price}");
            }
            assert_eq!(price["source"], "moex-iss");
            assert_eq!(price["complete_through"], "2026-08-03");
            // Доказательство основания котировки — это то, чем цена
            // отличается от догадки (§10.2). Потерянное по дороге,
            // оно не оставляет следа: ответ выглядит так же.
            assert_eq!(
                price["basis_evidence"], "test:contract",
                "основание цены не доехало: {price}"
            );
            assert_eq!(price["quotation_basis"], "unknown");
            assert_eq!(price["recorded_quotation_basis"], "money_per_unit");
            assert_eq!(price["quotation_basis_status"], "not_proven");
        }

        let (status, fx) = call(&harness.router, get(fx_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        let fx = fx.as_array().expect("массив курсов");
        assert_eq!(fx.len(), 1);
        for field in [
            "value",
            "date",
            "source",
            "observed_at",
            "quality",
            "complete_through",
        ] {
            assert!(fx[0].get(field).is_some(), "у курса нет {field}: {}", fx[0]);
        }
        assert_eq!(fx[0]["source"], "cbr");
        assert_eq!(fx[0]["quality"], "official");

        let (status, key_rates) = call(&harness.router, get(key_rate_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        let key_rates = key_rates.as_array().expect("массив интервалов ставки");
        assert_eq!(key_rates.len(), 2);
        assert_eq!(key_rates[0]["observed_at"], "2026-08-20T00:00:00Z");
        assert_eq!(key_rates[0]["quality"], "observed");
        assert_eq!(key_rates[1]["boundary"], "inferred_across_non_trading_days");
        assert_eq!(key_rates[1]["quality"], "inferred");
        assert_eq!(key_rates[1]["complete_through"], "2026-08-10");
    }

    for path in [prices_path.as_str(), fx_path, key_rate_path] {
        let (status, _) = call(&harness.router, get(path, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "маршрут открыт: {path}");
    }
}

// --- Журнальные факты: корпоративные действия и оферта -----------------

#[tokio::test]
async fn an_amortisation_is_recorded_through_the_journal_route() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "events": [{
            "account": harness.account.inner(),
            "type": "corporate_action",
            "action": {
                "type": "partial_redemption",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "10",
                "principal_returned_per_unit": { "amount": "100", "currency": "RUB" },
                "compensation": { "amount": "1000.00", "currency": "RUB" },
                "effective_date": "2026-05-20",
                "record_date": "2026-05-18"
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");
}

#[tokio::test]
async fn an_offer_settlement_is_recorded_through_the_journal_route() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "events": [{
            "account": harness.account.inner(),
            "type": "offer_exercise",
            "day": "2026-04-20",
            "action": {
                "type": "settled",
                "submission": Uuid::new_v4(),
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "5",
                "gross": { "amount": "5000.00", "currency": "RUB" },
                "fee": { "amount": "10.00", "currency": "RUB" }
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");
}

/// Один непонятый факт не отменяет соседний (§10.1) — и номер строки
/// в ответе называет именно тот факт, который отклонён.
#[tokio::test]
async fn a_mixed_batch_accepts_one_fact_and_refuses_its_neighbour() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "events": [
            {
                "account": harness.account.inner(),
                "type": "corporate_action",
                "action": {
                    "type": "partial_redemption",
                    "instrument": harness.instrument.inner(),
                    "custody": harness.custody.inner(),
                    "quantity": "не число",
                    "principal_returned_per_unit": { "amount": "100", "currency": "RUB" },
                    "compensation": { "amount": "1000.00", "currency": "RUB" },
                    "effective_date": "2026-05-20"
                }
            },
            {
                "account": harness.account.inner(),
                "type": "corporate_action",
                "action": {
                    "type": "partial_redemption",
                    "instrument": harness.instrument.inner(),
                    "custody": harness.custody.inner(),
                    "quantity": "10",
                    "principal_returned_per_unit": { "amount": "100", "currency": "RUB" },
                    "compensation": { "amount": "1000.00", "currency": "RUB" },
                    "effective_date": "2026-05-20"
                }
            }
        ]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["row"], 1, "{response}");
    assert_eq!(response[0]["verdict"], "rejected", "{response}");
    assert_eq!(response[0]["field"], "quantity", "{response}");
    assert_eq!(response[1]["row"], 2, "{response}");
    assert_eq!(response[1]["verdict"], "provisional", "{response}");
}

/// Нулевая выплата — не «амортизация на ноль», а брак источника. Отказ
/// обязан случиться до записи: журнал append-only.
#[tokio::test]
async fn a_zero_compensation_is_refused_and_never_becomes_cash() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "events": [{
            "account": harness.account.inner(),
            "type": "corporate_action",
            "action": {
                "type": "partial_redemption",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "10",
                "principal_returned_per_unit": { "amount": "0", "currency": "RUB" },
                "compensation": { "amount": "0.00", "currency": "RUB" },
                "effective_date": "2026-05-20"
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "rejected", "{response}");
    assert_eq!(response[0]["field"], "fact", "{response}");
}

#[tokio::test]
async fn a_read_only_token_may_not_submit_journal_events() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "events": [{
            "account": harness.account.inner(),
            "type": "offer_exercise",
            "day": "2026-04-10",
            "action": {
                "type": "cancelled",
                "submission": Uuid::new_v4(),
                "quantity": "5"
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.readonly_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(response["code"], "forbidden");
}

/// Круг через JSON по каждому оставшемуся члену: разбор, что не прошёл
/// ни одного факта, отличается от разобранного только тем, что ошибку
/// в нём никто не увидит.
#[tokio::test]
async fn a_redemption_is_recorded_through_the_journal_route() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "events": [{
            "account": harness.account.inner(),
            "type": "corporate_action",
            "action": {
                "type": "redemption",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "10",
                "principal_returned_per_unit": { "amount": "1000", "currency": "RUB" },
                "compensation": { "amount": "10000.00", "currency": "RUB" },
                "effective_date": "2026-06-01",
                "grounds": "решение эмитента"
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");
}

#[tokio::test]
async fn a_conversion_is_recorded_through_the_journal_route() {
    let harness = harness();
    let successor = Uuid::new_v4();
    let body = json!({
        "source_label": "тест",
        "events": [{
            "account": harness.account.inner(),
            "type": "corporate_action",
            "action": {
                "type": "conversion",
                "predecessor": harness.instrument.inner(),
                "successor": successor,
                "custody": harness.custody.inner(),
                "ratio": "1",
                "quantity_in": "10",
                "quantity_out": "10",
                "fractional": "not_applicable",
                "effective_date": "2026-07-01",
                "basis_transfer": "carry_over"
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");
}

/// Выкупленная деньгами дробь добавляет денежную ногу — и валюта
/// компенсации приезжает вместе с суммой, а не отдельным полем.
#[tokio::test]
async fn a_cash_compensated_fraction_travels_with_its_currency() {
    let harness = harness();
    let body = json!({
        "source_label": "тест",
        "events": [{
            "account": harness.account.inner(),
            "type": "corporate_action",
            "action": {
                "type": "conversion",
                "predecessor": harness.instrument.inner(),
                "successor": Uuid::new_v4(),
                "custody": harness.custody.inner(),
                "ratio": "1.5",
                "quantity_in": "11",
                "quantity_out": "16",
                "fractional": "cash_compensated",
                "compensation": { "amount": "50.00", "currency": "RUB" },
                "effective_date": "2026-07-01",
                "record_date": "2026-06-28",
                "basis_transfer": "restart"
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");
}

#[tokio::test]
async fn an_offer_application_and_its_withdrawal_are_recorded() {
    let harness = harness();
    let submission = Uuid::new_v4();
    let body = json!({
        "source_label": "тест",
        "events": [
            {
                "account": harness.account.inner(),
                "type": "offer_exercise",
                "day": "2026-04-10",
                "action": {
                    "type": "submitted",
                    "submission": submission,
                    "window": Uuid::new_v4(),
                    "instrument": harness.instrument.inner(),
                    "quantity": "5"
                }
            },
            {
                "account": harness.account.inner(),
                "type": "offer_exercise",
                "day": "2026-04-12",
                "action": {
                    "type": "cancelled",
                    "submission": submission,
                    "quantity": "5"
                }
            }
        ]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");
    assert_eq!(response[1]["verdict"], "provisional", "{response}");
}

/// Синхронизация рынка пишет в журнал наблюдений, поэтому read-only
/// токену она закрыта. Проверка на месте, но без теста её снятие
/// неотличимо от рабочего кода: ответ на владельческий токен тот же.
#[tokio::test]
async fn a_read_only_token_may_not_sync_the_market() {
    let harness = harness();
    let body = json!({
        "source": { "source": "cbr_daily" },
        "from": "2026-08-01",
        "to": "2026-08-03"
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/market/sync", &harness.readonly_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    assert_eq!(response["code"], "forbidden");
}

/// Заведённый доступ без словаря отклонил бы первую же выгрузку целиком,
/// и владелец пошёл бы разбираться с брокером вместо настройки. Словарь
/// заселяется тем же действием, и сети для этого не нужно: контракт
/// перечисляет коды, но не сообщает, во что они превращаются у нас.
#[tokio::test]
async fn provisioning_an_access_fills_the_channel_dictionary() {
    let (harness, path) = harness_on_disk();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/broker-access",
            &harness.owner_token,
            &add_broker_access_body(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let store = SqliteStore::open(&path).expect("второе соединение");
    let broker = BrokerCode::parse("tinkoff").expect("код брокера");
    let dictionary = store.broker_operation_kinds(&broker).expect("словарь");
    assert_eq!(
        dictionary.get("OPERATION_TYPE_COUPON").map(String::as_str),
        Some("coupon"),
        "словарь не заселён"
    );
    // Синоним теряется незаметнее прочего: он не ломает ни один
    // очевидный случай, а половина выгрузки перестаёт разбираться.
    assert_eq!(
        dictionary.get("OPERATION_TYPE_DIV_EXT").map(String::as_str),
        Some("dividend"),
        "синоним потерян при заселении"
    );
    assert!(
        dictionary.len() >= 35,
        "заселено меньше, чем знал прежний разбор: {}",
        dictionary.len()
    );
}

/// Прогоняет проблемы через ту же конверсию, по которой текст доходит
/// до владельца.
///
/// Пустой журнал здесь — не упрощение, а способ изолировать текст: сам
/// отчёт не должен добавить ни одной своей проблемы, иначе тест
/// закреплял бы чужие строки вместе со своими. Публичного пути к
/// формированию отдельной строки нет, и делать его ради теста нельзя:
/// владелец получает строки только целым отчётом.
fn issue_texts(issues: Vec<MaterialIssue>) -> Vec<String> {
    let events: Vec<iaam_core::event::Event> = Vec::new();
    let contour = ContourDefinition::new(
        ContourId(Uuid::new_v4()),
        ContourVersion(1),
        [AccountId::new_random()],
    );
    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &contour,
        rules: &rules,
        lot_rule: LotRuleVersion(1),
    };
    let projection = project(&events, &context).expect("проекция пустого журнала");
    let perimeter = assess(&events, PerimeterPolicy::default()).expect("периметр");
    let ledger =
        ReconciliationLedger::build_with(&events, &perimeter.exceptions()).expect("реестр сверки");
    let fx = FxTable::new(FxSource::OwnerSupplied);
    let mut report = returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &contour,
            coordinate: KnowledgeCoordinate::default(),
            as_of: date!(2026 - 03 - 31),
            report_currency: CurrencyCode::Rub,
            fx: &fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter: &perimeter,
            market_prices: &[],
            bond_schedules: &std::collections::BTreeMap::new(),
            accrued_observations: &std::collections::BTreeMap::new(),
        },
    );
    assert!(
        report.data_quality.material_issues.is_empty(),
        "пустой журнал дал собственные проблемы: {:?}",
        report.data_quality.material_issues
    );
    report.data_quality.material_issues = issues;

    ReturnsReportDto::from_domain(&report)
        .data_quality
        .material_issues
}

/// Кода у этой проблемы в ответе нет: владелец видит только строку.
/// Незакреплённая строка молча меняется вместе с `fn issue`, и владелец
/// получает другое сообщение без единого красного теста.
///
/// Проверяются все четыре величины сразу: вид выплаты — потому что
/// «не пришёл купон» и «не пришёл возврат номинала» требуют разных
/// действий; счёт — потому что одна бумага на двух счетах иначе даёт
/// две неразличимые строки; инструмент и дата — потому что без них
/// искать в журнале нечего.
#[test]
fn an_unreceived_scheduled_posting_names_kind_instrument_account_and_date() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let texts = issue_texts(vec![
        MaterialIssue::ScheduledPostingNotReceived {
            account,
            instrument,
            date: date!(2026 - 03 - 31),
            kind: PostingKind::Coupon,
        },
        MaterialIssue::ScheduledPostingNotReceived {
            account,
            instrument,
            date: date!(2026 - 03 - 31),
            kind: PostingKind::PrincipalReturn,
        },
    ]);

    assert_eq!(
        texts[0],
        format!(
            "выплата coupon инструмента {} на счёте {} за 2026-03-31 не подтверждена",
            instrument.inner(),
            account.inner()
        )
    );
    assert_eq!(
        texts[1],
        format!(
            "выплата principal_return инструмента {} на счёте {} за 2026-03-31 не подтверждена",
            instrument.inner(),
            account.inner()
        )
    );
}

/// Причина, дата и вид обязаны быть в тексте: без них владелец не знает,
/// какую выплату искать и чем чинить.
#[test]
fn an_unverifiable_scheduled_posting_names_kind_instrument_account_date_and_reason() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let texts = issue_texts(vec![MaterialIssue::ScheduledPostingUnverifiable {
        account,
        instrument,
        date: date!(2026 - 03 - 15),
        kind: PostingKind::Coupon,
        reason: UnverifiableReason::AcquisitionDateUnknown,
    }]);

    assert_eq!(
        texts[0],
        format!(
            "сверку выплаты coupon инструмента {} на счёте {} за 2026-03-15 провести нечем: acquisition_date_unknown",
            instrument.inner(),
            account.inner()
        )
    );
}

/// Четыре причины чинятся по-разному, а одна из них вообще не дефект.
/// Совпади хотя бы две строки — владелец не отличил бы «дозагрузи даты»
/// от «журнал начинается позже, чинить нечего».
#[test]
fn the_four_unverifiable_scheduled_posting_reasons_are_distinguishable() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let reasons = [
        UnverifiableReason::AcquisitionDateUnknown,
        UnverifiableReason::IncomeKindUnknown,
        UnverifiableReason::PaymentDateUnknown,
        UnverifiableReason::HistoryStartsAfterSchedule,
    ];
    let texts = issue_texts(
        reasons
            .iter()
            .map(|reason| MaterialIssue::ScheduledPostingUnverifiable {
                account,
                instrument,
                date: date!(2026 - 03 - 15),
                kind: PostingKind::Coupon,
                reason: *reason,
            })
            .collect(),
    );

    let distinct: std::collections::BTreeSet<&String> = texts.iter().collect();
    assert_eq!(distinct.len(), 4, "причины неразличимы в тексте: {texts:?}");
}
