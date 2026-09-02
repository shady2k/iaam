//! Contract tests against the generated spec (§17.1).
//!
//! `utoipa` generates the spec from types and therefore eliminates
//! **data schema** drift. Behaviour — response codes, authentication requirements,
//! actual serialisation — remains outside generation, and is tested
//! only by calling a running server. For a contract used by
//! an external agent, a syntactically valid but behaviourally incorrect spec
//! means the agent will fix itself based on incorrect guidance.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use iaam_app::AppServices;
use iaam_app::actions::OperationKey;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ingest::dedup::IdentityScope;
use iaam_app::ingest::{OperationDates, OperationKind, Rejection, SubmittedOperation, Verdict};
use iaam_app::ports::{
    BrokerChannel, BrokerChannelFactory, BrokerError, BrokerVault, ClassificationRuleStore, Clock,
    ParsedOperations, PortfolioAsOf, PortfolioSnapshot, TokenAdmin, UnavailableOutboundHttp,
};
use iaam_app::storage::SqliteStore;
use iaam_app::storage::{
    AccountRecord, AliasRecord, BrokerCode, Coverage, FxRow, InstrumentRecord, KeyRateRow,
    PriceRow, RunOutcome, SeriesKey, TokenRecord, TokenScope,
};
use iaam_broker::credentials::Key;
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::{DateCertainty, EventKind};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{AliasInterval, AliasNamespace, CurrencyRoles, InstrumentKind};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::perimeter::{PerimeterPolicy, assess};
use iaam_core::projection::{ProjectionContext, project};
use iaam_core::reconciliation::{Dimension, ReconciliationLedger};
use iaam_core::returns::{
    KnowledgeCoordinate, MaterialIssue, ReturnsRequest, UnverifiableReason, returns_report,
};
use iaam_core::rules::{LotRuleVersion, PostingKind, RuleRegistry};
use iaam_core::valuation::{FxSource, FxTable};
use iaam_server::action_catalog::{ActionCatalog, ActionCatalogError};
use iaam_server::auth::hash_token;
use iaam_server::dto::{ReturnsReportDto, VerdictDto};
use iaam_server::error::ApiFailure;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::broker_access::NewBrokerAccess;
use iaam_store::market::AccruedInterestRow;
use iaam_store::market_source_codes::SourceCodeEntry;
use iaam_store::schedule::{
    CouponPeriodRow, IssueTermsRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};
use serde_json::{Value, json};
use std::time::Duration;
use time::macros::date;
use time::{Date, Duration as TimeDuration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

/// A clock with a fixed date: otherwise the report «as at today» is
/// not reproducible in a test.
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
    ) -> Result<PortfolioSnapshot, BrokerError> {
        Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Current,
            claims: Vec::new(),
        })
    }

    fn channel(&self) -> iaam_core::reconciliation::evidence::SourceChannel {
        self.source.clone()
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Source
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
                source_time: None,
                idempotency_key: Some("sync-row-1".to_owned()),
                source_operation_id: Some("broker-row-1".to_owned()),
                source_category: None,
                description: None,
            }],
            quarantined: Vec::new(),
        })
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _at: Date,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Current,
            claims: Vec::new(),
        })
    }

    fn channel(&self) -> iaam_core::reconciliation::evidence::SourceChannel {
        self.source.clone()
    }
    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Source
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
    harness_with(SqliteStore::open_in_memory().expect("in-memory database"))
}

/// The same harness, but with a file-backed database: tests verifying that a record
/// was actually written to the table must use a second connection
/// to the same database. There is no second connection with `open_in_memory`.
fn harness_on_disk() -> (Harness, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("iaam-contract-{}.db", Uuid::new_v4()));
    let store = SqliteStore::open(&path).expect("file-backed database");
    (harness_with(store), path)
}

fn add_reconciliation_assertion(path: &std::path::Path, owner: OwnerId, account: AccountId) {
    add_reconciliation_assertion_for_period(
        path,
        owner,
        account,
        date!(2025 - 01 - 01),
        date!(2025 - 01 - 31),
    );
}

fn add_reconciliation_assertion_for_period(
    path: &std::path::Path,
    owner: OwnerId,
    account: AccountId,
    from: Date,
    to: Date,
) {
    let period =
        iaam_core::reconciliation::claim::AssertionPeriod::between(from, to).expect("period");
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
            iaam_core::event::provenance::RawHash::parse(&"b".repeat(64)).expect("hash"),
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
        .expect("second connection")
        .append_event(&event, IdentityScope::Source)
        .expect("reconciliation assertion");
}

fn harness_with(store: SqliteStore) -> Harness {
    harness_with_factory(store, None)
}

fn unprovisioned_harness() -> Harness {
    harness_with_factory_and_provisioning(
        SqliteStore::open_in_memory().expect("in-memory database"),
        None,
        false,
        false,
        false,
    )
}

fn empty_owner_harness() -> Harness {
    harness_with_factory_and_provisioning(
        SqliteStore::open_in_memory().expect("in-memory database"),
        None,
        true,
        false,
        false,
    )
}

fn broker_access_harness() -> Harness {
    harness_with_factory_and_provisioning(
        SqliteStore::open_in_memory().expect("in-memory database"),
        None,
        true,
        true,
        true,
    )
}

fn harness_with_factory(
    store: SqliteStore,
    channel_factory: Option<Arc<dyn BrokerChannelFactory>>,
) -> Harness {
    harness_with_factory_and_provisioning(store, channel_factory, true, true, false)
}

fn harness_with_factory_and_provisioning(
    mut store: SqliteStore,
    channel_factory: Option<Arc<dyn BrokerChannelFactory>>,
    provisioned: bool,
    with_account: bool,
    with_broker_access: bool,
) -> Harness {
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();

    if provisioned {
        if with_account {
            store
                .upsert_account(&AccountRecord {
                    id: account,
                    owner,
                    title: "Brokerage".into(),
                    institution: None,
                })
                .expect("account");
        }

        let owner_token = "owner-secret-token";
        store
            .insert_token(
                &TokenRecord {
                    id: Uuid::new_v4(),
                    owner,
                    label: "owner".into(),
                    scope: TokenScope::Owner,
                    revoked: false,
                },
                &hash_token(owner_token),
            )
            .expect("owner token");

        let agent_token = "agent-secret-token";
        store
            .insert_token(
                &TokenRecord {
                    id: Uuid::new_v4(),
                    owner,
                    label: "agent".into(),
                    scope: TokenScope::Agent,
                    revoked: false,
                },
                &hash_token(agent_token),
            )
            .expect("agent token");

        let readonly_token = "read-only-token";
        store
            .insert_token(
                &TokenRecord {
                    id: Uuid::new_v4(),
                    owner,
                    label: "read".into(),
                    scope: TokenScope::ReadOnly,
                    revoked: false,
                },
                &hash_token(readonly_token),
            )
            .expect("read token");
    }

    if with_broker_access {
        let sealed = iaam_broker::credentials::seal(&Key::from_bytes([7; 32]), BROKER_TOKEN);
        store
            .insert_broker_access(&NewBrokerAccess {
                id: Uuid::new_v4(),
                owner,
                broker: BrokerCode::parse("tinkoff").expect("broker code"),
                environment: "sandbox".to_owned(),
                scope: "read_only".to_owned(),
                nonce: sealed.nonce().to_vec(),
                ciphertext: sealed.ciphertext().to_vec(),
            })
            .expect("broker access");
    }

    // The key is created directly from bytes, not from a file: a file in a temporary directory
    // would have to be deleted, and a test that failed before deletion would leave
    // the key behind. Fixed bytes are safe here — the database lives
    // exactly as long as the test.
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
        directory: adapter.clone(),
        broker,
        tokens,
        clock: Arc::new(FixedClock(date!(2026 - 01 - 01))),
        channels,
        rules,
        categories: adapter.clone(),
        http: Arc::new(UnavailableOutboundHttp),
        broker_dictionary,
        market_store: market_store.clone(),
    });
    let state = ServerState::new(
        services,
        Arc::new(RateLimiter::new(1_000, Duration::from_secs(60))),
    );
    let (router, api) = build(state).expect("build");

    Harness {
        router,
        api,
        owner_token: "owner-secret-token".to_owned(),
        agent_token: "agent-secret-token".to_owned(),
        readonly_token: "read-only-token".to_owned(),
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
            title: "Sberbank".into(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("market instrument");

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
        .expect("price run");
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
        .expect("price rows");
    store
        .finish_run(
            &price_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("price publication");

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
        .expect("exchange-rate run");
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
        .expect("exchange-rate row");
    store
        .finish_run(
            &fx_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("exchange-rate publication");

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
        .expect("rate run");
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
        .expect("rate rows");
    store
        .finish_run(
            &key_rate_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),

                to: date!(2026 - 08 - 10),
            }),
        )
        .expect("rate publication");
}

async fn seed_bond_market(harness: &Harness) {
    let mut store = harness.market_store.lock().await;
    store
        .upsert_instrument(&InstrumentRecord {
            id: harness.instrument,
            kind: Some(InstrumentKind::Bond),
            symbol: "BOND".into(),
            title: "Test bond".into(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("bond market instrument");
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
        .expect("offer dictionary");

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
        .expect("bond price run");
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
        .expect("bond price");
    store
        .finish_run(
            &price_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 01),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("bond price publication");
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
        .expect("starting accrued interest");
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
        .expect("bond accrued interest");
    store
        .finish_run(
            &accrued_run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("bond accrued interest publication");

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
        .expect("schedule snapshot");
    store
        .record_schedule_completeness(&snapshot.snapshot_id, true, true, None, &[0])
        .expect("schedule completeness");
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
        .expect("issue terms");
}

async fn call_raw(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("handler responded");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

async fn call(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let (status, _headers, bytes) = call_raw(router, request).await;
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
    builder.body(Body::empty()).expect("request")
}

fn delete(path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("DELETE")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

fn post(path: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// Request without a token, used for public-route and missing-route checks.
fn post_public(path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

#[tokio::test]
async fn health_is_public_and_reports_versions() {
    let harness = harness();
    let (status, body) = call(&harness.router, get("/v1/health", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    // Version 11: version 4 added CorporateAction, OfferExercise and the income
    // type (§4.7); version 5 added the source time inside EffectiveOrder;
    // version 6 added the basis-only trade fee; version 7 added
    // ImportCoverageGap for refused import dimensions; version 8 made that gap
    // carry the rows it refused; version 9 added Tax, so that a tax stops being
    // indistinguishable from ordinary spending; version 10 added the source
    // description inside Provenance, without which a rule on the description
    // can match nothing; version 11 added Refund, so that money a counterparty
    // returns reverses spending instead of being reported as income nobody
    // earned. One version cannot denote two schemas (§4.1). An
    // external agent reads this number to determine whether it can parse the
    // response, so it is fixed here rather than derived from the code — a
    // silent bump would tell that agent nothing had changed, and a silent
    // omission would tell it nothing had changed when a new event kind
    // appeared.
    assert_eq!(body["schema_version"], 11);
    // Version 8: version 7 removed the face value from the lot and made the
    // prefix fingerprint cover the event contents; version 8 orders events
    // within a day by the source's time. Snapshots from either earlier version
    // are incompatible and trigger a full recalculation.
    assert_eq!(body["projection_version"], 8);
}

#[tokio::test]
async fn api_catalog_is_public_state_independent_and_route_complete() {
    let provisioned = harness();
    let unprovisioned = unprovisioned_harness();
    assert!(
        provisioned
            .api
            .paths
            .paths
            .contains_key("/.well-known/api-catalog"),
        "the catalog route must be in the generated OpenAPI document"
    );

    let (provisioned_status, provisioned_headers, provisioned_body) =
        call_raw(&provisioned.router, get("/.well-known/api-catalog", None)).await;
    let (unprovisioned_status, unprovisioned_headers, unprovisioned_body) =
        call_raw(&unprovisioned.router, get("/.well-known/api-catalog", None)).await;

    assert_eq!(provisioned_status, StatusCode::OK);
    assert_eq!(unprovisioned_status, StatusCode::OK);
    assert_eq!(provisioned_headers, unprovisioned_headers);
    assert_eq!(provisioned_body, unprovisioned_body);
    assert_eq!(
        provisioned_headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/linkset+json")
    );
    assert_eq!(
        provisioned_body,
        iaam_server::routes::API_CATALOG_BODY.to_vec()
    );

    let catalog: Value = serde_json::from_slice(&provisioned_body).expect("catalog JSON");
    let linkset = catalog["linkset"].as_array().expect("linkset array");
    for relation in ["service-desc", "status"] {
        for link in linkset[0][relation].as_array().expect("link relation") {
            let href = link["href"].as_str().expect("catalog href");
            let (status, _, _) = call_raw(&provisioned.router, get(href, None)).await;
            assert_eq!(status, StatusCode::OK, "{relation} href {href}");
        }
    }
}

#[tokio::test]
async fn every_documented_path_answers_something_other_than_404() {
    // A spec describing a non-existent route is an instruction
    // for the external agent to correct itself based on false guidance.
    let harness = harness();
    for (path, item) in harness.api.paths.paths.clone() {
        // `PathItem` in utoipa 5 stores operations in separate fields
        // rather than a map: enumerate exactly the methods used by the API.
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
                .expect("request");
            let (status, body) = call(&harness.router, request).await;
            // There are two distinct kinds of `404`, and the guard distinguishes them by
            // the response body. A missing **route** returns an empty `404`
            // axum itself — this is exactly the discrepancy from the specification that
            // the test was written for. A missing **resource** returns our
            // `ApiError` with a machine-readable code, and this is a valid response:
            // the identifier in the request is random, and there is no record with it.
            if status == StatusCode::NOT_FOUND {
                assert!(
                    body.get("code").and_then(Value::as_str).is_some(),
                    "route {path} {verb} is described in the specification but does not exist"
                );
            }
            assert_ne!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "method {verb} for {path} is described in the specification but is not supported"
            );
        }
    }
}

#[tokio::test]
async fn a_request_without_a_token_is_rejected_with_bare_bearer_challenge() {
    // Authentication from day one (§14).
    let harness = harness();
    let (status, headers, bytes) = call_raw(&harness.router, get("/v1/accounts", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        headers.get("vary").and_then(|value| value.to_str().ok()),
        Some("Authorization")
    );
    assert_eq!(
        bytes,
        br#"{"code":"unauthorized","message":"a token is issued at the console by iaam claim --label <label>; no API route issues one"}"#
    );
}

#[tokio::test]
async fn an_unknown_token_is_rejected_with_invalid_token_challenge() {
    let harness = harness();
    let (status, headers, bytes) =
        call_raw(&harness.router, get("/v1/accounts", Some("unknown-token"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer error=\"invalid_token\"")
    );
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        headers.get("vary").and_then(|value| value.to_str().ok()),
        Some("Authorization")
    );
    assert_eq!(
        bytes,
        br#"{"code":"unauthorized","message":"a token is issued at the console by iaam claim --label <label>; no API route issues one"}"#
    );
}

#[tokio::test]
async fn a_read_only_token_may_not_submit_operations() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
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
async fn an_invalid_amount_is_reported_as_a_200_row_verdict_with_field_expected_actual() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
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
    // A verdict per row, rather than rejection of the entire document (§10.1).
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response[0]["verdict"], "rejected");
    assert_eq!(response[0]["field"], "amount");
    assert_eq!(response[0]["actual"], "1000.005");
}

#[tokio::test]
async fn opening_position_assertions_reach_the_event_through_the_api() {
    let (harness, path) = harness_on_disk();
    let claimed = date!(2021 - 05 - 01);
    let body = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "opening_position",
            "instrument": harness.instrument.inner(),
            "custody": harness.custody.inner(),
            "quantity": "10",
            "cost_basis": "1000.00",
            "currency": "RUB",
            "assertions": {
                "acquisition_date": claimed.to_string(),
                "acquisition_date_certainty": "known"
            },
            "dates": { "trade": "2026-01-01" }
        }]
    });

    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");

    let store = SqliteStore::open(&path).expect("second connection");
    let events = store
        .load_events(harness.owner)
        .expect("events for the restored position");
    let event = events
        .into_iter()
        .find(|event| matches!(&event.kind, EventKind::OpeningPosition { .. }))
        .expect("restored position");
    let EventKind::OpeningPosition { assertions, .. } = event.kind else {
        unreachable!("only opening_position was found above");
    };
    assert_eq!(assertions.acquisition_date, Some(claimed));
    assert_eq!(assertions.acquisition_date_certainty, DateCertainty::Known);

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_carried_forward_price_is_not_accepted_from_the_api() {
    let harness = harness();
    let body = json!({
        "source_label": "manual entry",
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
        "source_label": "manual entry",
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
    // The epic's acceptance criterion via the API: how much was contributed, how much
    // was withdrawn, and the pre-tax return.
    let harness = harness();

    let contour = json!({
        "title": "My portfolio",
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
        .expect("scope")
        .to_owned();

    let operations = json!({
        "source_label": "manual entry",
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
    for verdict in verdicts.as_array().expect("array of verdicts") {
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
    // Scale is preserved: the rouble has two minor units, and an amount,
    // converted from posted to calculated retains two decimal places.
    assert_eq!(report["contributed"]["value"], "100000.00");
    assert_eq!(report["withdrawn"]["value"], "10000.00");
    // 2 900,00 roubles in cash plus 100 securities at 1 000 = 102 900,00.
    assert_eq!(report["terminal_value"]["value"], "102900.00");
    assert_eq!(report["history_starts"], "2025-01-01");
    assert_eq!(report["bond_metrics"], json!([]));
    assert!(
        report["data_quality"]["nav_coverage"]
            .get("bond_metrics")
            .is_none(),
        "bond metrics must not appear in data_quality"
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
            "a missing exchange rate must not be treated as 1: {field}"
        );
    }

    // The rate was obtained using an independent reference (scripts/gen-xirr-fixtures.py),
    // not from the output of the program under test (§15.5).
    let rate: f64 = report["xirr_pre_tax"]["value"]
        .as_str()
        .expect("rate")
        .parse()
        .expect("number");
    assert!(
        (rate - 0.133_270_341_032).abs() < 1e-7,
        "rate {rate} does not match the reference value"
    );
    // The data were entered manually and are uncorroborated: the entire value
    // of the portfolio is in the `provisional` share. This is not a defect — §10.5
    // requires such records to be included in reports by default, — but
    // the owner must see exactly what proportion is unverified.
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
                "title": "Test bond",
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
        "title": "Bond portfolio",
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
        .expect("scope")
        .to_owned();

    let operations = json!({
        "source_label": "manual entry",
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
            .expect("array of verdicts")
            .iter()
            .all(|verdict| verdict["verdict"] == "provisional"),
        "{verdicts}"
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

    let scenarios = bond["scenarios"].as_array().expect("scenarios");
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
            .expect("explanation")
            .is_empty()
    );
    let lifetime = ytm["lifetime"]["value"].as_array().expect("cohorts");
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
            .expect("reason IRR is unavailable")
            .is_empty()
    );

    let refusal = scenarios
        .iter()
        .find(|scenario| scenario["prospective"]["terminal_date"] == "2026-08-26")
        .expect("rejection offer-rate");
    assert_eq!(refusal["prospective"]["irr"]["value"], "");
    assert_eq!(refusal["prospective"]["irr"]["error_bound"], "");
    assert_eq!(
        refusal["prospective"]["irr"]["not_computable"],
        "solver_refused"
    );
    assert!(
        !refusal["prospective"]["irr"]["detail"]
            .as_str()
            .expect("rejection detail")
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
        "title": "Dollar-denominated report",
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
        .expect("scope")
        .to_owned();

    let operations = json!({
        "source_label": "market rate",
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
        "source_label": "test",
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
        "the spec must describe the authentication scheme"
    );
    let description = spec["components"]["securitySchemes"]["bearer"]["description"]
        .as_str()
        .expect("security scheme description");
    assert!(description.contains("iaam claim --label <label>"));
    assert!(description.contains("no API route issues one"));
}

#[tokio::test]
async fn no_openapi_request_body_accepts_credential_fields() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let paths = spec["paths"].as_object().expect("OpenAPI paths");
    for (path, item) in paths {
        let Some(operations) = item.as_object() else {
            continue;
        };
        for (method, operation) in operations {
            let request_body = resolve_request_body(&operation["requestBody"], &spec["components"]);
            let Some(content) = request_body["content"].as_object() else {
                continue;
            };
            for (content_type, media) in content {
                let schema = &media["schema"];
                assert!(
                    !schema_contains_credential(
                        schema,
                        &spec["components"]["schemas"],
                        &mut std::collections::HashSet::new(),
                    ),
                    "{method} {path} {content_type} accepts a credential-shaped field"
                );
            }
        }
    }
}

fn resolve_request_body<'a>(request_body: &'a Value, components: &'a Value) -> &'a Value {
    let Some(reference) = request_body["$ref"].as_str() else {
        return request_body;
    };
    let Some(name) = reference.strip_prefix("#/components/requestBodies/") else {
        return request_body;
    };
    &components["requestBodies"][name]
}

fn schema_contains_credential(
    schema: &Value,
    components: &Value,
    seen_refs: &mut std::collections::HashSet<String>,
) -> bool {
    if schema["format"].as_str() == Some("password") || schema["writeOnly"].as_bool() == Some(true)
    {
        return true;
    }

    if let Some(reference) = schema["$ref"].as_str() {
        if !seen_refs.insert(reference.to_owned()) {
            return false;
        }
        let Some(name) = reference.strip_prefix("#/components/schemas/") else {
            return false;
        };
        return schema_contains_credential(&components[name], components, seen_refs);
    }

    if schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties
                .values()
                .any(|property| schema_contains_credential(property, components, seen_refs))
        })
    {
        return true;
    }

    for key in ["items", "not", "additionalProperties"] {
        if schema
            .get(key)
            .is_some_and(|child| schema_contains_credential(child, components, seen_refs))
        {
            return true;
        }
    }
    for key in ["allOf", "oneOf", "anyOf"] {
        if schema
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|children| {
                children
                    .iter()
                    .any(|child| schema_contains_credential(child, components, seen_refs))
            })
        {
            return true;
        }
    }
    false
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

/// Every code of one published vocabulary, in document order, with the sentence
/// that explains it.
///
/// Read from the document rather than from the Rust type on purpose: what a
/// client can learn is what the document says, and a check against the enum
/// would pass just as happily with an empty schema.
fn published_vocabulary(spec: &serde_json::Value, schema: &str) -> Vec<String> {
    let items = spec["components"]["schemas"][schema]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{schema} must publish its codes as a oneOf: {spec}"));
    assert!(
        !spec["components"]["schemas"][schema]["description"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "{schema} must say what the vocabulary as a whole is for"
    );
    items
        .iter()
        .map(|item| {
            let code = item["enum"][0]
                .as_str()
                .unwrap_or_else(|| panic!("a code in {schema} is not a string: {item}"))
                .to_owned();
            let meaning = item["description"].as_str().unwrap_or_default();
            assert!(
                !meaning.trim().is_empty(),
                "code {code} in {schema} arrives without a meaning"
            );
            code
        })
        .collect()
}

/// Whether a property points at the named schema, directly or through the
/// `oneOf` that an optional field is rendered as.
fn refers_to(property: &serde_json::Value, schema: &str) -> bool {
    let reference = json!(format!("#/components/schemas/{schema}"));
    if property["$ref"] == reference {
        return true;
    }
    property["oneOf"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["$ref"] == reference))
}

#[tokio::test]
async fn the_openapi_document_enumerates_and_explains_every_verdict() {
    // A verdict code the document does not list is a code the agent has to
    // look up somewhere else, and every hand-written list drifts: the one in
    // the agent skill listed eight of these ten and omitted `possible_duplicate`
    // and `quarantined`, both of which production emits.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        refers_to(
            &spec["components"]["schemas"]["VerdictDto"]["properties"]["verdict"],
            "VerdictCodeDto"
        ),
        "the verdict field must point at the vocabulary: {}",
        spec["components"]["schemas"]["VerdictDto"]["properties"]["verdict"]
    );

    assert_eq!(
        published_vocabulary(&spec, "VerdictCodeDto"),
        vec![
            "accepted",
            "provisional",
            "possible_duplicate",
            "discrepancy",
            "needs_reconciliation",
            "duplicate",
            "needs_classification",
            "unsupported",
            "rejected",
            "quarantined",
        ]
    );
}

#[tokio::test]
async fn the_openapi_document_enumerates_and_explains_every_refusal() {
    // `not_computable` is a refusal the owner is told about. A bare code says
    // nothing without a document beside it; the vocabulary carries the sentence
    // itself, and the same list types every value that may be refused.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    for schema in [
        "ComputedDto",
        "RateDto",
        "ComputedCalcMoneyDto",
        "ComputedZeroReinvestmentMetricsDto",
        "ComputedLifetimeCohortMetricsDto",
    ] {
        let property = &spec["components"]["schemas"][schema]["properties"]["not_computable"];
        assert!(
            refers_to(property, "NotComputableCodeDto"),
            "{schema}.not_computable must point at the vocabulary: {property}"
        );
    }

    assert_eq!(
        published_vocabulary(&spec, "NotComputableCodeDto"),
        vec![
            "missing_price",
            "missing_fx_rate",
            "quotation_basis_contradicts_evidence",
            "quotation_basis_unknown",
            "remaining_face_unknown",
            "principal_unknown",
            "solver_refused",
            "no_external_flows",
            "state_newer_than_report",
            "numeric",
            "unsupported_financing",
            "schedule_missing",
            "accrued_observation_missing",
            "coupon_undetermined",
            "outside_schedule_coverage",
            "overlapping_schedule_coverage",
            "exit_not_executable",
            "non_positive_duration",
            "non_positive_initial_capital",
            "negative_terminal_wealth",
            "acquisition_basis_unknown",
            "accrued_interest_at_acquisition_unknown",
            "historical_receipts_unknown",
            "cohort_gap",
            "currency_mismatch",
            "expense_unknown",
        ]
    );
}

#[tokio::test]
async fn the_openapi_document_enumerates_and_explains_the_data_quality_status() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    assert!(refers_to(
        &spec["components"]["schemas"]["DataQualityDto"]["properties"]["status"],
        "DataQualityStatusDto"
    ));
    assert_eq!(
        published_vocabulary(&spec, "DataQualityStatusDto"),
        vec!["clean", "mixed", "incomplete"]
    );
}

#[tokio::test]
async fn a_published_code_is_the_code_the_response_carries() {
    // The vocabularies enumerate; they must enumerate what actually arrives.
    // A schema that lists ten plausible codes while the server sends an
    // eleventh is worse than no schema, because it is believed.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    let verdicts = published_vocabulary(&spec, "VerdictCodeDto");
    let statuses = published_vocabulary(&spec, "DataQualityStatusDto");

    let contour = json!({
        "title": "Vocabulary",
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
        .expect("scope")
        .to_owned();

    let operations = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" },
        }],
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let code = response[0]["verdict"].as_str().expect("verdict code");
    assert!(
        verdicts.iter().any(|published| published == code),
        "the verdict {code} is not in the published vocabulary {verdicts:?}"
    );

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2025-12-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let quality = report["data_quality"]["status"]
        .as_str()
        .expect("data quality status");
    assert!(
        statuses.iter().any(|published| published == quality),
        "the status {quality} is not in the published vocabulary {statuses:?}"
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
            "schema {schema} must be in OpenAPI"
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
    // Field-by-field checks catch an incorrect value, but do not catch
    // a missing field or the appearance of an extra one. A snapshot captures
    // the whole shape (§15.8).
    let harness = harness();
    let contour = json!({
        "title": "Snapshot",
        "accounts": [harness.account.inner()],
    });
    let (_, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("scope")
        .to_owned();

    let operations = json!({
        "source_label": "snapshot",
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

    // Account IDs are random on each run, while material
    // issues mention them. They are replaced by a filter, not by redacting
    // the field: redaction would hide the issue text entirely, and the snapshot
    // would stop checking the very thing it exists to check — exactly which
    // issues the system reports to the owner.
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
    // Scope is a barrier, not a hint. The agent submits
    // transactions, but does not create accounts or change the scope's composition: otherwise
    // an external agent entrusted with data entry gains the right
    // to redefine the scope boundary and thereby rewrite returns.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.agent_token,
            &json!({ "title": "Someone else's account" }),
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
            &json!({ "title": "Own scope", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // But it can submit transactions.
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.agent_token,
            &json!({
                "source_label": "agent",
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
    // A newly created account must be retrievable: an empty list
    // looks like «there are no accounts», not «the list is broken».
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Second brokerage account", "institution": "Bank" }),
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
        .expect("account list")
        .iter()
        .map(|account| account["title"].as_str().expect("title"))
        .collect();
    assert!(
        titles.contains(&"Second brokerage account"),
        "the created account must be in the list: {titles:?}"
    );
    assert!(
        titles.contains(&"Brokerage"),
        "and the existing one too: {titles:?}"
    );
}

#[tokio::test]
async fn each_verdict_names_the_row_it_belongs_to() {
    // Verdicts arrive one row per transaction, and the agent fixes exactly the one
    // it was told about. Incorrect numbering sends it to fix
    // a valid row, while leaving the invalid one unchanged.
    let harness = harness();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
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
                        "amount": "not a number",
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
        .expect("verdicts")
        .iter()
        .map(|verdict| verdict["row"].as_u64().expect("row number"))
        .collect();
    assert_eq!(
        rows,
        vec![1, 2, 3, 4],
        "numbering starts at one and is consecutive"
    );
    assert_eq!(body[0]["verdict"], "provisional");
    // The second row was rejected during validation: the value was parsed, but
    // it cannot be negative.
    assert_eq!(body[1]["verdict"], "rejected");
    // The third was rejected while parsing the request body. Both routes to
    // a verdict number the rows, and both must number them identically.
    assert_eq!(body[2]["verdict"], "rejected");
    assert_eq!(body[3]["verdict"], "provisional");
}

#[tokio::test]
async fn a_csv_document_resolves_account_names_and_numbers_its_rows() {
    // The name lookup is built from the owner's accounts. An empty lookup
    // would reject the entire document on the account field, and «no account was set up»
    // would become indistinguishable from «the lookup failed».
    let harness = harness();
    let document = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key\n\
        2025-01-01,deposit,Brokerage,,,,,1000.00,,,RUB,csv-1\n\
        2025-01-02,deposit,No such account,,,,,1000.00,,,RUB,csv-2\n\
        2025-01-03,withdrawal,Brokerage,,,,,500.00,,,RUB,csv-3\n";
    let request = Request::builder()
        .uri("/v1/ingest/csv")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "text/csv")
        .body(Body::from(document))
        .expect("request");
    let (status, body) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let verdicts = body.as_array().expect("verdicts");
    assert_eq!(verdicts.len(), 3);
    let rows: Vec<u64> = verdicts
        .iter()
        .map(|verdict| verdict["row"].as_u64().expect("row number"))
        .collect();
    assert_eq!(rows, vec![1, 2, 3]);
    assert_eq!(verdicts[0]["verdict"], "provisional");
    assert_eq!(verdicts[1]["verdict"], "rejected");
    assert_eq!(verdicts[1]["field"], "account");
    assert_eq!(verdicts[2]["verdict"], "provisional");
}

#[tokio::test]
async fn ambiguous_account_name_is_rejected_when_resolving_row() {
    let (harness, path) = harness_on_disk();
    {
        let store = SqliteStore::open(&path).expect("second connection");
        store
            .upsert_account(&AccountRecord {
                id: AccountId::new_random(),
                owner: harness.owner,
                title: "Brokerage".into(),
                institution: None,
            })
            .expect("duplicate account");
        store
            .upsert_account(&AccountRecord {
                id: AccountId::new_random(),
                owner: harness.owner,
                title: "Unambiguous".into(),
                institution: None,
            })
            .expect("unambiguous account");
    }

    let document = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key\n\
        2025-01-01,deposit,Brokerage,,,,,1000.00,,,RUB,duplicate\n\
        2025-01-02,deposit,Unambiguous,,,,,1000.00,,,RUB,unique\n";
    let request = Request::builder()
        .uri("/v1/ingest/csv")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "text/csv")
        .body(Body::from(document))
        .expect("request");
    let (status, body) = call(&harness.router, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["verdict"], "rejected");
    assert_eq!(body[0]["field"], "account");
    let actual = body[0]["actual"].as_str().expect("reason for rejection");
    assert_eq!(actual, "Brokerage: account name is ambiguous: 2 accounts");
    assert_eq!(body[1]["verdict"], "provisional");

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn an_unparsable_report_date_is_refused_and_a_valid_one_is_honoured() {
    // Silently defaulting to «today» instead of rejecting an unrecognised date would produce
    // a report for the wrong date — apparently valid, but for a different period.
    let harness = harness();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Portfolio", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("contour").to_owned();

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
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
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=yesterday"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "as_of");

    // The date precedes the transaction: its report must differ from the report
    // for today, otherwise the parameter has no effect.
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
    assert_eq!(today["as_of"], "2026-01-01", "default — clock date");
    assert_ne!(
        early["contributed"], today["contributed"],
        "a report before the first transaction must differ from a report after it"
    );
}

#[tokio::test]
async fn a_report_for_today_leaves_a_snapshot_and_a_report_for_a_past_date_does_not() {
    // The snapshot key comprises the contour, its version and the rule version; the date is
    // absent. A snapshot built from a slice at a past date would be stored under the
    // same key and silently substitute its state into the next request.
    // This is checked with a direct database query: from the outside, the substitution looks
    // like an ordinary response, just with incorrect figures.
    let (harness, path) = harness_on_disk();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Portfolio", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("contour").to_owned();

    let (status, _) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
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
        let probe = SqliteStore::open(path).expect("second connection");
        probe
            .connection()
            .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
            .expect("snapshot count")
    };
    let usages = |path: &std::path::Path| -> u32 {
        let probe = SqliteStore::open(path).expect("second connection");
        probe
            .connection()
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .expect("request count")
    };

    // A report for a past date does not save a snapshot.
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
        "an as-of snapshot for a past date must not be saved"
    );

    // A report for today does save a snapshot, and it can be read back: a repeated
    // request must return the same figures.
    let (status, first) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(snapshots(&path), 1, "a report for today saves a snapshot");

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
        "a report calculated from a snapshot must match one calculated without it"
    );
    assert_eq!(
        snapshots(&path),
        1,
        "the snapshot is replaced, not duplicated"
    );

    // Every request made with a token is recorded in the access log (§14).
    assert!(
        usages(&path) >= 4,
        "the access log is empty: attempts made with a token must be visible"
    );

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn an_event_added_behind_the_snapshot_boundary_forces_a_recompute_not_a_failure() {
    // A snapshot is a cache, and an unusable one is not an operational error.
    // A backdated event before the snapshot boundary changes
    // the fingerprint of the folded prefix: the core refuses to advance
    // the snapshot, while the wrapper must recalculate the entire log and still
    // return a response — one that accounts for the new event.
    let harness = harness();
    let (status, contour) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Portfolio", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour}");
    let contour_id = contour["contour"].as_str().expect("contour").to_owned();

    let deposit = |key: &str, day: &str, amount: &str| {
        json!({
            "source_label": "manual entry",
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

    // The first report for today saves a snapshot.
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

    // A backdated event — before the data already folded.
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
        "an unusable snapshot calls for recalculation, not failure: {after}"
    );
    assert_eq!(
        after["contributed"]["value"], "1500.00",
        "the backdated event must be included in the calculation"
    );
    assert_eq!(
        after["history_starts"], "2025-01-01",
        "and shift the start of the history"
    );
}

/// The broker token that the tests pass to the server. The value was chosen
/// so that a substring of it does not occur in the response by chance.
const BROKER_TOKEN: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";

#[tokio::test]
async fn a_provisioned_access_is_listed_and_a_revoked_one_stops_being_current() {
    // Revocation is not deletion: the record remains in the history, but ceases to
    // be active. A record missing from the list would mean «there was no
    // access», not «access was revoked at such-and-such time».
    let harness = broker_access_harness();

    let (status, list) = call(
        &harness.router,
        get("/v1/broker-access", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let id = list[0]["id"]
        .as_str()
        .expect("seeded identifier")
        .to_owned();
    let listed = find_access(&list, &id).expect("the seeded access must be in the list");
    assert!(
        listed["revoked_at"].is_null(),
        "the seeded access is active: {listed}"
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
    let listed = find_access(&list, &id).expect("the revoked access remains in the history");
    assert!(
        !listed["revoked_at"].is_null(),
        "the revoked access is no longer active: {listed}"
    );
}

#[tokio::test]
async fn a_read_only_token_may_not_touch_broker_access_at_all() {
    // Reading the list and revoking an access are management operations, not portfolio
    // reading. A read token manages nothing.
    let harness = broker_access_harness();

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

/// List entry by identifier.
fn find_access(list: &Value, id: &str) -> Option<Value> {
    list.as_array()?
        .iter()
        .find(|access| access["id"] == id)
        .cloned()
}

// --- Token management (§14) ---

#[tokio::test]
async fn the_removed_claim_route_is_not_documented_or_available() {
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/claim",
            &harness.owner_token,
            &json!({ "code": "retired", "label": "laptop" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        body.is_null(),
        "a missing route must have an empty body: {body}"
    );
    assert!(
        !harness.api.paths.paths.contains_key("/v1/claim"),
        "the retired claim route must not appear in OpenAPI"
    );
}

#[tokio::test]
async fn an_owner_token_is_never_issued_through_the_api() {
    // An owner is created with `iaam claim --label <label>`. A route issuing
    // full access would turn one stolen token into indistinguishable copies,
    // and revoking the original would change nothing.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.owner_token,
            &json!({ "label": "second owner", "scope": "owner" }),
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
    // An agent submits operations, but does not grant access to the portfolio:
    // otherwise a stolen agent token could issue itself a replacement
    // faster than the owner could revoke it.
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.agent_token,
            &json!({ "label": "another agent", "scope": "agent" }),
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
    // The hash is all that needs to be supplied in a lookup request for
    // the system to recognise the bearer as authorised. A list of issued tokens
    // exposing hashes would be a list of skeleton keys. Check for the substring
    // throughout the entire body, not by field: a field added tomorrow will not
    // be checked by eye.
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.owner_token,
            &json!({ "label": "home agent", "scope": "agent" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let issued = created["token"].as_str().expect("token").to_owned();

    let (status, list) = call(
        &harness.router,
        get("/v1/tokens", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let body = list.to_string();

    assert!(
        body.contains("home agent"),
        "the issued token must be listed: {body}"
    );
    for secret in [&issued, &harness.owner_token, &harness.agent_token] {
        assert!(
            !body.contains(secret.as_str()),
            "token leaked into the list of issued tokens: {body}"
        );
        assert!(
            !body.contains(&hash_token(secret)),
            "token hash leaked into the list of issued tokens: {body}"
        );
    }
}

#[tokio::test]
async fn a_revoked_token_stops_being_accepted_and_stays_in_the_history() {
    // Revocation is not deletion: the record remains as history, but no longer
    // grants access. A record missing from the list would answer «no such token»,
    // rather than «the token was revoked at such-and-such time».
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/tokens",
            &harness.owner_token,
            &json!({ "label": "phone", "scope": "read_only" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("identifier").to_owned();
    let token = created["token"].as_str().expect("token").to_owned();

    let (status, accounts) = call(&harness.router, get("/v1/accounts", Some(&token))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the issued token grants access: {accounts}"
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
        "the revoked token does not grant access: {body}"
    );

    let (status, list) = call(
        &harness.router,
        get("/v1/tokens", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed = find_access(&list, &id).expect("the revoked token remains in the history");
    assert!(
        !listed["revoked_at"].is_null(),
        "the revoked token ceases to be active: {listed}"
    );
}

#[tokio::test]
async fn a_token_of_another_owner_is_as_absent_as_a_missing_one() {
    // A token identifier does not confer authority over it: without owner authentication
    // on the revocation request, anyone knowing someone else's identifier could revoke
    // their token. The response deliberately matches «not found» — different responses
    // would tell an outsider that the record exists (§14).
    let (harness, path) = harness_on_disk();

    // The second owner's token is created through a second connection to the same
    // database: it cannot be created via the API — the system has only one owner, and this is
    // exactly the state in which another owner's token can appear at all.
    let stranger_token = "stranger-secret-token";
    let stranger = TokenRecord {
        id: Uuid::new_v4(),
        owner: OwnerId::new_random(),
        label: "someone else's".into(),
        scope: TokenScope::Agent,
        revoked: false,
    };
    {
        let store = SqliteStore::open(&path).expect("second connection");
        store
            .insert_token(&stranger, &hash_token(stranger_token))
            .expect("another user's token");
    }

    let (status, body) = call(
        &harness.router,
        delete(&format!("/v1/tokens/{}", stranger.id), &harness.owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "not_found");

    // A non-existent token returns exactly the same response: one cannot
    // be distinguished from the other by the response.
    let (missing, body) = call(
        &harness.router,
        delete(
            &format!("/v1/tokens/{}", Uuid::new_v4()),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(missing, status, "{body}");

    // And the other user's token remained valid: a refusal must be a refusal,
    // not ‘we did not say so, but did it anyway’.
    let (status, accounts) = call(&harness.router, get("/v1/accounts", Some(stranger_token))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "another user's token was not revoked by an unauthorised user: {accounts}"
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
    let id = created["id"].as_str().expect("identifier").to_owned();

    let (status, history) = call(
        &harness.router,
        get("/v1/classification-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history.as_array().expect("history").len(), 1);
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
    let statuses = response["statuses"]
        .as_array()
        .expect("reconciliation statuses");
    assert_eq!(statuses.len(), 1);
    assert!(response["gaps"].is_array());
    assert_eq!(statuses[0]["account"], json!(harness.account.inner()));
    assert_eq!(statuses[0]["from"], "2025-01-01");
    assert_eq!(statuses[0]["to"], "2025-01-31");
    assert_eq!(statuses[0]["dimensions"][0]["dimension"], "cash");
    assert_eq!(statuses[0]["dimensions"][0]["status"], "provisional");
    assert_eq!(statuses[0]["outcomes"][0]["claim"]["kind"], "cash_balance");
    assert_eq!(
        statuses[0]["outcomes"][0]["outcome"]["code"],
        "not_comparable"
    );
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
    let statuses = response.as_array().expect("reconciliation statuses");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0]["account"], json!(harness.account.inner()));
    assert_eq!(statuses[0]["dimensions"][0]["dimension"], "cash");
    assert_eq!(statuses[0]["dimensions"][0]["status"], "provisional");
    assert_eq!(statuses[0]["outcomes"][0]["claim"]["kind"], "cash_balance");
    assert_eq!(
        statuses[0]["outcomes"][0]["outcome"]["code"],
        "not_comparable"
    );
    drop(harness);
    let _ = std::fs::remove_file(&path);
}

/// A coverage gap is written by the broker sync path before any assertion is
/// recorded, so it can exist with no reconciliation status to hang it on. Written
/// straight into the journal here for the same reason
/// `add_reconciliation_assertion_for_period` is: no route records a gap.
fn add_coverage_gap(
    path: &std::path::Path,
    owner: OwnerId,
    account: AccountId,
    from: Date,
    to: Date,
) {
    let period =
        iaam_core::reconciliation::claim::AssertionPeriod::between(from, to).expect("period");
    let source = SourceId::new_random();
    let event = iaam_core::event::Event {
        id: iaam_core::ids::EventId::new_random(),
        schema_version: iaam_core::event::SCHEMA_VERSION,
        owner,
        account,
        kind: EventKind::ImportCoverageGap {
            period,
            dimensions: std::collections::BTreeSet::from([Dimension::Cash]),
            refused: 1,
            rows: vec![iaam_core::event::source_row::RefusedRow {
                key: iaam_core::event::source_row::SourceRowKey {
                    source,
                    row: iaam_core::event::source_row::RowName::Given("row-17".to_owned()),
                },
                dimensions: std::collections::BTreeSet::from([Dimension::Cash]),
            }],
        },
        dates: EventDates::for_cash(CashPostedDate(period.to)),
        order: EffectiveOrder::new(period.to, 1),
        legs: Vec::new(),
        provenance: iaam_core::event::provenance::Provenance::new(
            source,
            iaam_core::event::provenance::RawHash::parse(&"c".repeat(64)).expect("hash"),
            ParserVersion("contract-test".to_owned()),
        ),
        relation: iaam_core::event::Relation::None,
        confidence: iaam_core::event::Confidence::Known,
        idempotency_key: None,
    };
    SqliteStore::open(path)
        .expect("second connection")
        .append_event(&event, IdentityScope::Source)
        .expect("coverage gap");
}

/// The gap correlates with no assertion group, so nothing in `statuses` mentions
/// it. A response that reported only statuses would answer "nothing to report"
/// about an account whose import demonstrably refused a row.
#[tokio::test]
async fn the_reconciliation_response_carries_a_gap_that_matched_no_status() {
    let (harness, path) = harness_on_disk();
    add_coverage_gap(
        &path,
        harness.owner,
        harness.account,
        date!(2025 - 01 - 01),
        date!(2025 - 01 - 31),
    );

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
    assert_eq!(
        response["statuses"].as_array().expect("statuses").len(),
        0,
        "a gap forms no assertion group: {response}"
    );
    let gaps = response["gaps"].as_array().expect("gaps");
    assert_eq!(gaps.len(), 1, "{response}");
    assert_eq!(gaps[0]["account"], json!(harness.account.inner()));
    assert_eq!(gaps[0]["from"], "2025-01-01");
    assert_eq!(gaps[0]["to"], "2025-01-31");
    assert_eq!(gaps[0]["parser_version"], "contract-test");
    assert_eq!(gaps[0]["dimensions"], json!(["cash"]));
    assert_eq!(gaps[0]["refused"], 1);
    assert_eq!(gaps[0]["rows"][0]["row"], json!({ "given": "row-17" }));
    assert_eq!(gaps[0]["rows"][0]["dimensions"], json!(["cash"]));

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

/// `dto.rs` and `routes.rs` held independent copies of this conversion, which is
/// how a change to it ships half done and looks finished. The two are now one
/// function, and this pins that: the balances report and `/v1/reconciliation`
/// must render the same status, so a re-divergence fails here and not at a reader.
#[tokio::test]
async fn the_balances_report_and_the_reconciliation_route_render_one_status() {
    let (harness, path) = harness_on_disk();
    add_reconciliation_assertion_for_period(
        &path,
        harness.owner,
        harness.account,
        date!(2026 - 08 - 01),
        date!(2026 - 08 - 31),
    );

    let contour = json!({
        "title": "August reconciliation",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");

    let (status, balances) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{balances}");

    let (status, reconciliation) = call(
        &harness.router,
        get(
            &format!(
                "/v1/reconciliation?account={}&from=2026-08-01&to=2026-08-31",
                harness.account.inner()
            ),
            Some(&harness.readonly_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reconciliation}");

    let from_report = &balances[0]["reconciliation"][0];
    let from_route = &reconciliation["statuses"][0];
    // Assert the shared shape is the rich one before asserting equality: two
    // renderers that both dropped the claim would agree just as well.
    assert_eq!(
        from_route["outcomes"][0]["claim"]["claimed"],
        json!({ "money": { "amount": "100.00", "currency": "RUB" } }),
        "{from_route}"
    );
    assert_eq!(
        from_route["outcomes"][0]["outcome"]["reason"], "no_journal_coverage",
        "{from_route}"
    );
    assert_eq!(from_report, from_route);

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
        SqliteStore::open_in_memory().expect("in-memory database"),
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
    assert_eq!(response["recorded"].as_array().expect("verdicts").len(), 1);
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
        SqliteStore::open_in_memory().expect("in-memory database"),
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
        "source_label": "contract",
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
        "event_id must be a UUID: {provisional_event}"
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
        "duplicate must identify the existing event"
    );

    let rejected_body = json!({
        "source_label": "contract",
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
    assert_eq!(rejected[0]["expected"], "positive value");
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
        .expect("request");
    let (status, response) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let rows = response["rows"].as_array().expect("document rows");
    let unsupported = rows
        .iter()
        .find(|row| row["verdict"] == "unsupported")
        .expect("the report must return a row outside the scope");
    assert_eq!(unsupported["detail"], "repo");
}

#[test]
fn verdict_dto_json_contains_every_variant_field() {
    let event = iaam_core::ids::EventId::new_random();
    let account = AccountId::new_random();
    let possible_event = iaam_core::ids::EventId::new_random();
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
                detail: "balance did not reconcile".into(),
            },
            json!({
                "row": 7,
                "verdict": "discrepancy",
                "event_id": event.inner(),
                "account_id": account.inner(),
                "dimension": "cash",
                "detail": "balance did not reconcile",
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
            Verdict::PossibleDuplicate {
                event: possible_event,
                of: event,
                level: iaam_app::ingest::dedup::DedupLevel::Probabilistic,
            },
            json!({
                "row": 7,
                "verdict": "possible_duplicate",
                "event_id": possible_event.inner(),
                "of_event_id": event.inner(),
                "level": 5,
            }),
        ),
        (
            Verdict::NeedsClassification {
                question: "internal transfer?".into(),
            },
            json!({
                "row": 7,
                "verdict": "needs_classification",
                "detail": "internal transfer?",
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
                    expected: "a positive value".into(),
                    actual: "-5.00".into(),
                },
            },
            json!({
                "row": 7,
                "verdict": "rejected",
                "field": "amount",
                "expected": "a positive value",
                "actual": "-5.00",
            }),
        ),
    ];

    for (domain, expected) in cases {
        let actual =
            serde_json::to_value(VerdictDto::from_domain(7, &domain)).expect("verdict serialises");
        assert_eq!(actual, expected, "verdict content for {domain:?}");
    }
}

fn seed_directory(store: &SqliteStore) -> InstrumentId {
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".into(),
            title: "Sberbank".into(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("instrument");
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
        .expect("alias");
    instrument
}

fn seeded_harness() -> Harness {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
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
            .expect("list")
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
        "a known code outside the interval is not the same as an unknown code"
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
        "SourceId points to the owner's document: it is not exposed externally (§14)"
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
            &json!({"symbol": "HACK", "title": "Impostor", "kind": "share"}),
        ),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the reference catalogue is global: another owner's entry corrupts every owner's data"
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
                "title": "Gazprom",
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
    assert_eq!(stored["title"], "Gazprom");
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
    let fx_path = "/v1/market/fx?base=USD&quote=RUB&from=2026-08-01&to=2026-08-03&knowledge_as_of=2099-01-01T00:00:00Z";
    let key_rate_path =
        "/v1/market/key-rate?from=2026-08-03&to=2026-08-10&knowledge_as_of=2099-01-01T00:00:00Z";

    for token in [&harness.owner_token, &harness.agent_token] {
        let (status, prices) = call(&harness.router, get(&prices_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        let prices = prices.as_array().expect("price array");
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
                assert!(
                    price.get(field).is_some(),
                    "price is missing {field}: {price}"
                );
            }
            assert_eq!(price["source"], "moex-iss");
            assert_eq!(price["complete_through"], "2026-08-03");
            // Proof of the quotation's basis is what distinguishes a price
            // from a guess (§10.2). If lost in transit,
            // it leaves no trace: the response looks the same.
            assert_eq!(
                price["basis_evidence"], "test:contract",
                "price basis was lost in transit: {price}"
            );
            assert_eq!(price["quotation_basis"], "unknown");
            assert_eq!(price["recorded_quotation_basis"], "money_per_unit");
            assert_eq!(price["quotation_basis_status"], "not_proven");
        }

        let (status, fx) = call(&harness.router, get(fx_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        let fx = fx.as_array().expect("array of exchange rates");
        assert_eq!(fx.len(), 1);
        for field in [
            "value",
            "date",
            "source",
            "observed_at",
            "quality",
            "complete_through",
        ] {
            assert!(
                fx[0].get(field).is_some(),
                "exchange rate has no {field}: {}",
                fx[0]
            );
        }
        assert_eq!(fx[0]["source"], "cbr");
        assert_eq!(fx[0]["quality"], "official");

        let (status, key_rates) = call(&harness.router, get(key_rate_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        let key_rates = key_rates.as_array().expect("array of key-rate intervals");
        assert_eq!(key_rates.len(), 2);
        assert_eq!(key_rates[0]["observed_at"], "2026-08-20T00:00:00Z");
        assert_eq!(key_rates[0]["quality"], "observed");
        assert_eq!(key_rates[1]["boundary"], "inferred_across_non_trading_days");
        assert_eq!(key_rates[1]["quality"], "inferred");
        assert_eq!(key_rates[1]["complete_through"], "2026-08-10");
    }

    for path in [prices_path.as_str(), fx_path, key_rate_path] {
        let (status, _) = call(&harness.router, get(path, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "route is open: {path}");
    }
}

#[tokio::test]
async fn the_exchange_rate_route_spells_the_pair_and_the_interval_apart() {
    // `from` and `to` meant a currency here and an interval everywhere else,
    // so an agent that had learned the interval sent `from=2026-01-01` and was
    // told to send a currency instead. The pair is `base`/`quote`; `from` and
    // `to` are the interval, as on every other route.
    let harness = harness();
    seed_market(&harness).await;

    let path = "/v1/market/fx?base=USD&quote=RUB&from=2026-08-01&to=2026-08-03";
    let (status, body) = call(&harness.router, get(path, Some(&harness.agent_token))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "exchange rates were refused: {body}"
    );
    assert_eq!(body.as_array().expect("array of exchange rates").len(), 1);

    // The old spelling is gone rather than quietly accepted beside the new one.
    let old_spelling = "/v1/market/fx?from=USD&to=RUB&from_date=2026-08-01&to_date=2026-08-03";
    let (status, _) = call(
        &harness.router,
        get(old_spelling, Some(&harness.agent_token)),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "the old parameter names still answer: {old_spelling}"
    );

    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    let declared: Vec<&str> = spec["paths"]["/v1/market/fx"]["get"]["parameters"]
        .as_array()
        .expect("declared parameters")
        .iter()
        .map(|parameter| parameter["name"].as_str().expect("parameter name"))
        .collect();
    assert_eq!(
        declared,
        vec!["base", "quote", "from", "to", "knowledge_as_of"],
        "the contract spells the exchange-rate parameters differently"
    );
}

#[tokio::test]
async fn every_market_parameter_is_described_and_the_moex_ones_name_their_origin() {
    // A bare `board` or `session` cannot be guessed: both are MOEX ISS column
    // values, and the contract is the only place an agent can learn that.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let described = |route: &str| -> Vec<(String, String)> {
        spec["paths"][route]["get"]["parameters"]
            .as_array()
            .unwrap_or_else(|| panic!("declared parameters of {route}"))
            .iter()
            .map(|parameter| {
                let name = parameter["name"]
                    .as_str()
                    .expect("parameter name")
                    .to_owned();
                let description = parameter["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                assert!(
                    !description.trim().is_empty(),
                    "{route} leaves {name} undescribed"
                );
                (name, description)
            })
            .collect()
    };

    for route in ["/v1/market/prices", "/v1/market/fx", "/v1/market/key-rate"] {
        assert!(
            !described(route).is_empty(),
            "{route} declares no parameter"
        );
    }

    let prices: std::collections::BTreeMap<String, String> =
        described("/v1/market/prices").into_iter().collect();
    assert!(
        prices["board"].contains("BOARDID") && prices["board"].contains("MOEX"),
        "board does not name its MOEX origin: {}",
        prices["board"]
    );
    assert!(
        prices["session"].contains("TRADINGSESSION") && prices["session"].contains("MOEX"),
        "session does not name its MOEX origin: {}",
        prices["session"]
    );
}

// --- Journal facts: corporate actions and an offer -----------------

#[tokio::test]
async fn an_amortisation_is_recorded_through_the_journal_route() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
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
        "source_label": "test",
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

/// One unrecognised fact does not invalidate an adjacent one (§10.1) — and the line number
/// in the response identifies the exact fact that was rejected.
#[tokio::test]
async fn a_mixed_batch_accepts_one_fact_and_refuses_its_neighbour() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
        "events": [
            {
                "account": harness.account.inner(),
                "type": "corporate_action",
                "action": {
                    "type": "partial_redemption",
                    "instrument": harness.instrument.inner(),
                    "custody": harness.custody.inner(),
                    "quantity": "not a number",
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

/// A zero payment is not «amortisation to zero», but bad source data. Rejection
/// must occur before writing: the journal is append-only.
#[tokio::test]
async fn a_zero_compensation_is_refused_and_never_becomes_cash() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
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
async fn the_ingest_route_ignores_a_client_supplied_allocation() {
    let (harness, path) = harness_on_disk();
    let body = json!({
        "source_label": "test",
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
                "basis_allocation": { "state": "known", "share": "0.9" }
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

    let store = SqliteStore::open(&path).expect("second connection");
    let allocation = store
        .load_events_through(harness.owner, Date::MAX)
        .expect("amortisation events")
        .into_iter()
        .find_map(|event| match event.kind {
            EventKind::CorporateAction {
                action:
                    iaam_core::event::corporate_action::CorporateAction::PartialRedemption {
                        basis_allocation,
                        ..
                    },
            } => Some(basis_allocation),
            _ => None,
        })
        .expect("amortisation");
    assert!(
        matches!(
            &allocation,
            iaam_core::event::allocation::BasisAllocation::Unknown(
                iaam_core::event::allocation::AllocationGap::ScheduleMissing
            )
        ),
        "the share must be inferred by the application without a schedule: {allocation:?}"
    );
    assert!(
        !matches!(
            &allocation,
            iaam_core::event::allocation::BasisAllocation::Known { .. }
        ),
        "the client-supplied share must not enter the event: {allocation:?}"
    );

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn an_unknown_allocation_is_named_to_the_owner() {
    let harness = harness();
    let contour = json!({
        "title": "Portfolio with amortisation",
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
        .expect("scope")
        .to_owned();

    let operations = json!({
        "source_label": "test",
        "operations": [{
            "account": harness.account.inner(),
            "type": "buy",
            "instrument": harness.instrument.inner(),
            "custody": harness.custody.inner(),
            "quantity": "10",
            "amount": "1000.00",
            "currency": "RUB",
            "dates": { "trade": "2026-05-01", "cash_posted": "2026-05-01" }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");

    let journal = json!({
        "source_label": "test",
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
                "effective_date": "2026-05-20"
            }
        }]
    });
    let (status, response) = call(
        &harness.router,
        post("/v1/ingest/journal-events", &harness.owner_token, &journal),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response[0]["verdict"], "provisional", "{response}");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-05-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["data_quality"]["status"], "incomplete", "{report}");
    let expected = format!(
        "the amortisation allocation share for instrument {} in account {} could not be derived: load a verified issue schedule",
        harness.instrument.inner(),
        harness.account.inner()
    );
    assert!(
        report["data_quality"]["material_issues"]
            .as_array()
            .expect("material issues")
            .iter()
            .any(|issue| issue.as_str() == Some(expected.as_str())),
        "the owner did not see the gap: {report}"
    );
}

#[tokio::test]
async fn a_read_only_token_may_not_submit_journal_events() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
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

/// A JSON round trip for every remaining member: a parsing path not exercised
/// by any fact differs from an exercised one only in that nobody will see
/// an error in it.
#[tokio::test]
async fn a_redemption_is_recorded_through_the_journal_route() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
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
                "grounds": "issuer decision"
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
        "source_label": "test",
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

/// A fraction bought out for cash adds a cash leg — and the currency
/// of the compensation comes with the amount, rather than in a separate field.
#[tokio::test]
async fn a_cash_compensated_fraction_travels_with_its_currency() {
    let harness = harness();
    let body = json!({
        "source_label": "test",
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
        "source_label": "test",
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

/// Market synchronisation writes to the observation journal, so it is closed
/// to a read-only token. The check is in place, but without a test its removal
/// is indistinguishable from working code: the response for an owner token is the same.
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

/// Runs issues through the same conversion used to deliver the text
/// to the owner.
///
/// The empty journal here serves — not as a simplification, but as a way to isolate the text: the
/// report itself must not add any issues of its own, otherwise the test
/// would lock down unrelated strings along with its own. There is no public route to
/// produce an individual string, and one must not be added solely for the test:
/// the owner receives strings only as a complete report.
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
    let projection = project(&events, &context).expect("projection of the empty journal");
    let perimeter = assess(&events, PerimeterPolicy::default()).expect("perimeter");
    let ledger = ReconciliationLedger::build_with(&events, &perimeter.exceptions())
        .expect("reconciliation ledger");
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
        "empty journal produced issues of its own: {:?}",
        report.data_quality.material_issues
    );
    report.data_quality.material_issues = issues;

    ReturnsReportDto::from_domain(&report)
        .data_quality
        .material_issues
}

/// There is no code for this issue in the response: the owner sees only the string.
/// An unpinned string changes silently along with `fn issue`, and the owner
/// receives a different message without a single test failing.
///
/// All four values are checked together: payment type — because
/// «coupon not received» and «principal repayment not received» require different
/// actions; account — because otherwise the same security in two accounts produces
/// two indistinguishable strings; instrument and date — because without them
/// there is nothing to search for in the journal.
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
            "payment coupon for instrument {} in account {} for 2026-03-31 has not been confirmed",
            instrument.inner(),
            account.inner()
        )
    );
    assert_eq!(
        texts[1],
        format!(
            "payment principal_return for instrument {} in account {} for 2026-03-31 has not been confirmed",
            instrument.inner(),
            account.inner()
        )
    );
}

/// The reason, date and type must be included in the text: without them the owner does not know,
/// which payment to look for or how to fix it.
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
            "payment coupon for instrument {} in account {} for 2026-03-15 cannot be reconciled: acquisition_date_unknown",
            instrument.inner(),
            account.inner()
        )
    );
}

/// The six reasons require different fixes, and one of them concerns the whole
/// schedule and must also have distinct text.
/// If even two strings were identical — the owner could not distinguish «clarify the
/// calculation dates» from «load the record date», «the journal starts later» or
/// «load the trusted schedule».
#[test]
fn the_seven_unverifiable_scheduled_posting_reasons_are_distinguishable() {
    let account = AccountId::new_random();
    let instrument = InstrumentId::new_random();
    let reasons = [
        UnverifiableReason::AcquisitionDateUnknown,
        UnverifiableReason::OwnershipUnknown,
        UnverifiableReason::EntitlementDateUnknown,
        UnverifiableReason::IncomeKindUnknown,
        UnverifiableReason::PaymentDateUnknown,
        UnverifiableReason::HistoryStartsAfterSchedule,
        UnverifiableReason::ScheduleNotTrusted,
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
    assert_eq!(
        distinct.len(),
        7,
        "reasons are indistinguishable in the text: {texts:?}"
    );
}
#[tokio::test]
async fn custody_repair_is_described_and_scope_refusal_reaches_the_client() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let operation = &spec["paths"]["/v1/accounts/{account}/repairs/custody"]["post"];
    assert!(
        operation.is_object(),
        "custody repair route is missing: {spec}"
    );
    assert_eq!(
        operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/CustodyRepairRequest"
    );
    assert_eq!(
        operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/CustodyRepairOutcomeDto"
    );
    for field in ["case", "affected_trades", "already_reversed", "written"] {
        assert!(
            spec["components"]["schemas"]["CustodyRepairOutcomeDto"]["properties"][field]
                .is_object(),
            "response schema is missing {field}: {spec}"
        );
    }
    assert!(
        spec["components"]["schemas"]["CustodyRepairRequest"]["properties"]
            ["acknowledge_without_live_access"]
            .is_object(),
        "request schema is missing the acknowledgement: {spec}"
    );

    let (status, body) = call(
        &harness.router,
        post(
            &format!("/v1/accounts/{}/repairs/custody", harness.account.inner()),
            &harness.readonly_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "scope");
}

#[tokio::test]
async fn custody_repair_requires_acknowledgement_and_is_idempotent() {
    let harness = harness();
    let (status, seeded) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "custody-repair-contract",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "buy",
                    "instrument": harness.instrument.inner(),
                    "custody": harness.account.inner(),
                    "quantity": "1",
                    "amount": "100.00",
                    "currency": "RUB",
                    "dates": {
                        "trade": "2025-01-01",
                        "cash_posted": "2025-01-01"
                    },
                    "idempotency_key": "custody-repair-affected-trade"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{seeded}");
    assert_eq!(seeded[0]["verdict"], "provisional");

    let path = format!("/v1/accounts/{}/repairs/custody", harness.account.inner());
    let (status, refused) = call(
        &harness.router,
        post(&path, &harness.owner_token, &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refused}");
    assert_eq!(refused["case"], "affected_without_live_access");
    assert_eq!(refused["affected_trades"], 1);
    assert_eq!(refused["already_reversed"], 0);
    assert_eq!(refused["written"], 0);

    let (status, repaired) = call(
        &harness.router,
        post(
            &path,
            &harness.owner_token,
            &json!({ "acknowledge_without_live_access": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repaired}");
    assert_eq!(repaired["case"], "affected_without_live_access");
    assert_eq!(repaired["affected_trades"], 1);
    assert_eq!(repaired["already_reversed"], 0);
    assert_eq!(repaired["written"], 1);

    let (status, repeated) = call(
        &harness.router,
        post(
            &path,
            &harness.owner_token,
            &json!({ "acknowledge_without_live_access": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repeated}");
    assert_eq!(repeated["case"], "nothing_affected");
    assert_eq!(repeated["affected_trades"], 0);
    assert_eq!(repeated["already_reversed"], 1);
    assert_eq!(repeated["written"], 0);
}
#[tokio::test]
async fn the_same_declared_source_yields_the_same_source_id() {
    let (harness, path) = harness_on_disk();
    let account = harness.account.inner();
    let body = json!({
        "source_label": "paste",
        "source": { "account": account, "channel": "paste" },
        "operations": [{
            "account": account,
            "type": "withdrawal",
            "amount": "123.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-05" }
        }]
    });

    let (first, _) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(first, StatusCode::OK);
    let (second, _) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(second, StatusCode::OK);

    let sources = distinct_source_ids(&path, &harness.owner_token);
    assert_eq!(sources.len(), 1, "expected one source, got {sources:?}");
    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn source_category_survives_api_and_store_round_trip() {
    let (harness, path) = harness_on_disk();
    let body = json!({
        "source_label": "paste",
        "source": { "account": harness.account.inner(), "channel": "paste" },
        "operations": [{
            "account": harness.account.inner(),
            "type": "withdrawal",
            "amount": "1200.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-05" },
            "source_category": "Супермаркеты",
        }]
    });

    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "provisional", "{verdicts}");

    let events = SqliteStore::open(&path)
        .expect("second connection")
        .load_events(harness.owner)
        .expect("stored events");
    let event = events.into_iter().next().expect("stored event");
    assert_eq!(
        event.provenance.source_category(),
        Some("Супермаркеты"),
        "source category is stored verbatim"
    );

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn an_empty_channel_is_rejected() {
    let harness = harness();
    let account = harness.account.inner();

    for channel in ["", "x23456789012345678901234567890123"] {
        let (status, body) = call(
            &harness.router,
            post(
                "/v1/ingest/operations",
                &harness.owner_token,
                &json!({
                    "source_label": "paste",
                    "source": { "account": account, "channel": channel },
                    "operations": []
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["field"], "source.channel");
    }
}

fn distinct_source_ids(
    path: &std::path::Path,
    token: &str,
) -> std::collections::BTreeSet<SourceId> {
    let store = SqliteStore::open(path).expect("second connection");
    let owner = store
        .find_token(&hash_token(token))
        .expect("owner token")
        .expect("owner token record")
        .owner;
    store
        .load_events(owner)
        .expect("owner events")
        .into_iter()
        .map(|event| event.provenance.source())
        .collect()
}
#[tokio::test]
async fn flow_report_exposes_all_quantities_and_residual() {
    let harness = harness();
    let contour = json!({
        "title": "August flow",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");
    let operations = json!({
        "source_label": "manual entry",
        "operations": [
            {
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": "3000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": "flow-deposit"
            },
            {
                "account": harness.account.inner(),
                "type": "withdrawal",
                "amount": "1200.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-12" },
                "idempotency_key": "flow-withdrawal"
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/flow?contour={contour_id}&from=2026-08-01&to=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["contour"], contour_id);
    assert_eq!(body["contour_version"], 1);
    let currencies = body["currencies"].as_array().expect("currencies");
    assert_eq!(currencies.len(), 1);
    let rub = &currencies[0];
    assert_eq!(rub["currency"], "RUB");
    assert_eq!(rub["came_in"], "3000.00");
    assert_eq!(rub["went_out"], "1200.00");
    assert_eq!(rub["cash_delta"], "1800.00");
    assert_eq!(rub["residual"], "0.00");
    for field in [
        "came_in",
        "went_out",
        "earned_by_capital",
        "moved_into_assets",
        "fees",
        "taxes",
        "internal_transfers",
        "cash_delta",
        "residual",
    ] {
        assert!(rub.get(field).is_some(), "missing quantity {field}: {rub}");
    }
    assert_eq!(body["unexplained"], json!([]));
}

#[tokio::test]
async fn flow_report_rejects_a_reversed_interval() {
    let harness = harness();
    let contour = json!({
        "title": "August flow",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/flow?contour={contour_id}&from=2026-08-31&to=2026-08-01"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "period");
}

#[tokio::test]
async fn balances_keep_cash_and_positions_as_separate_fields() {
    let harness = harness();
    let contour = json!({
        "title": "August balances",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");
    let operations = json!({
        "source_label": "manual entry",
        "operations": [
            {
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": "3000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": "balance-deposit"
            },
            {
                "account": harness.account.inner(),
                "type": "opening_position",
                "instrument": harness.instrument.inner(),
                "custody": harness.custody.inner(),
                "quantity": "10",
                "cost_basis": "1000.00",
                "currency": "RUB",
                "dates": { "trade": "2026-01-01" },
                "idempotency_key": "balance-position"
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().expect("balance rows");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["account"], harness.account.inner().to_string());
    assert_eq!(row["cash"][0]["currency"], "RUB");
    assert_eq!(row["cash"][0]["amount"], "3000.00");
    assert_eq!(
        row["positions"][0]["instrument"],
        harness.instrument.inner().to_string()
    );
    assert_eq!(
        row["positions"][0]["custody"],
        harness.custody.inner().to_string()
    );
    assert_eq!(row["positions"][0]["quantity"], "10");
    assert!(row["reconciliation"].is_array());
    assert!(row.get("total").is_none());
}

#[tokio::test]
async fn balances_report_distinguishes_reconciled_and_unstated_accounts() {
    let (harness, path) = harness_on_disk();
    let unstated_account = AccountId::new_random();
    SqliteStore::open(&path)
        .expect("second connection")
        .upsert_account(&AccountRecord {
            id: unstated_account,
            owner: harness.owner,
            title: "Unstated".into(),
            institution: None,
        })
        .expect("unstated account");
    add_reconciliation_assertion_for_period(
        &path,
        harness.owner,
        harness.account,
        date!(2026 - 08 - 01),
        date!(2026 - 08 - 31),
    );

    let contour = json!({
        "title": "August balances with reconciliation",
        "accounts": [harness.account.inner(), unstated_account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().expect("balance rows");
    assert_eq!(rows.len(), 2);
    let reconciled_id = harness.account.inner().to_string();
    let unstated_id = unstated_account.inner().to_string();
    let reconciled = rows
        .iter()
        .find(|row| row["account"].as_str() == Some(reconciled_id.as_str()))
        .expect("reconciled account row");
    let unstated = rows
        .iter()
        .find(|row| row["account"].as_str() == Some(unstated_id.as_str()))
        .expect("unstated account row");

    assert_eq!(
        reconciled["reconciliation"],
        json!([{
            "account": reconciled_id,
            "from": "2026-08-01",
            "to": "2026-08-31",
            "dimensions": [
                { "dimension": "cash", "status": "provisional" },
                { "dimension": "positions", "status": "provisional" },
                { "dimension": "tax_basis", "status": "provisional" },
                { "dimension": "income", "status": "provisional" },
            ],
            "evidence": [],
            "outcomes": [{
                "claim": {
                    "kind": "cash_balance",
                    "at": "closing",
                    "currency": "RUB",
                    "claimed": { "money": { "amount": "100.00", "currency": "RUB" } },
                },
                "outcome": {
                    "code": "not_comparable",
                    "reason": "no_journal_coverage",
                },
            }],
            "taints": [],
        }])
    );
    assert_eq!(unstated["reconciliation"], json!([]));

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn flow_and_balances_reports_require_authentication() {
    let harness = harness();
    for path in [
        "/v1/reports/flow?contour=00000000-0000-0000-0000-000000000000&from=2026-08-01&to=2026-08-31",
        "/v1/reports/balances?contour=00000000-0000-0000-0000-000000000000&as_of=2026-08-31",
    ] {
        let (status, body) = call(&harness.router, get(path, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {body}");
    }
}
#[tokio::test]
async fn flow_report_names_an_unexplained_account() {
    let harness = harness();
    let contour = json!({
        "title": "Opening balance flow",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");
    let operations = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "opening_cash",
            "amount": "500.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-05" },
            "idempotency_key": "opening-cash"
        }]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/flow?contour={contour_id}&from=2026-08-01&to=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["currencies"][0]["residual"], "500.00");
    assert_eq!(
        body["unexplained"][0]["account"],
        harness.account.inner().to_string()
    );
    assert_eq!(body["unexplained"][0]["currency"], "RUB");
    assert_eq!(body["unexplained"][0]["amount"], "500.00");
}

#[tokio::test]
async fn a_tax_operation_reaches_the_store_as_one_negative_tax_leg() {
    let (harness, path) = harness_on_disk();
    let body = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "tax",
            "amount": "1300.00",
            "currency": "RUB",
            "origin": "self_paid",
            "dates": { "cash_posted": "2026-08-25" },
            "idempotency_key": "tax-leg",
        }],
    });

    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "provisional", "{verdicts}");

    let events = SqliteStore::open(&path)
        .expect("second connection")
        .load_events(harness.owner)
        .expect("stored events");
    let event = events
        .into_iter()
        .find(|event| matches!(&event.kind, EventKind::Tax { .. }))
        .expect("stored tax event");

    match &event.kind {
        EventKind::Tax { amount, origin } => {
            assert_eq!(amount.amount().raw(), -130_000);
            assert_eq!(*origin, iaam_core::event::kind::TaxOrigin::SelfPaid);
        }
        other => panic!("expected a tax event, got {other:?}"),
    }

    let tax_legs: Vec<_> = event
        .legs
        .iter()
        .filter(|leg| leg.kind == iaam_core::event::leg::LegKind::Tax)
        .collect();
    assert_eq!(tax_legs.len(), 1);
    let leg = tax_legs[0];
    assert_eq!(leg.account, harness.account);
    let money = leg.money.expect("tax leg amount");
    assert_eq!(money.amount().raw(), -130_000);
    assert_eq!(money.currency(), CurrencyCode::Rub);

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn tax_amounts_are_rejected_per_row_when_not_positive() {
    let harness = harness();
    for (row, amount) in ["0.00", "-1.00"].into_iter().enumerate() {
        let body = json!({
            "source_label": "manual entry",
            "operations": [{
                "account": harness.account.inner(),
                "type": "tax",
                "amount": amount,
                "currency": "RUB",
                "origin": "withheld_at_source",
                "dates": { "cash_posted": "2026-08-25" },
                "idempotency_key": format!("tax-rejected-{row}"),
            }],
        });

        let (status, response) = call(
            &harness.router,
            post("/v1/ingest/operations", &harness.owner_token, &body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response[0]["verdict"], "rejected", "{response}");
        assert_eq!(response[0]["field"], "amount", "{response}");
    }
}

#[tokio::test]
async fn a_category_group_can_be_created_and_then_holds_a_category() {
    let (harness, _path) = harness_on_disk();

    let (status, group) = call(
        &harness.router,
        post(
            "/v1/category-groups",
            &harness.owner_token,
            &json!({"title": "Usual Expenses"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{group}");
    let group_id = group["id"].as_str().expect("group id").to_owned();

    let (status, listed) = call(
        &harness.router,
        get("/v1/category-groups", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed.as_array().expect("group list").len(), 1);

    // The point of the route: the group it returns is usable straight away.
    let (status, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group_id, "title": "Food"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{category}");
}

#[tokio::test]
async fn a_category_group_without_a_title_is_refused_by_field() {
    let (harness, _path) = harness_on_disk();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/category-groups",
            &harness.owner_token,
            &json!({"title": "   "}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "title", "{body}");
}

#[tokio::test]
async fn a_read_only_token_may_not_touch_category_groups_at_all() {
    let harness = harness();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/category-groups",
            &harness.readonly_token,
            &json!({"title": "Usual Expenses"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = call(
        &harness.router,
        get("/v1/category-groups", Some(&harness.readonly_token)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn categories_can_be_retired_without_disappearing() {
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Food"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let category_id = created["id"].as_str().expect("category id");

    let (status, listed) = call(
        &harness.router,
        get("/v1/categories", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed.as_array().expect("category list").len(), 1);

    let (status, body) = call(
        &harness.router,
        delete(
            &format!("/v1/categories/{category_id}"),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, listed) = call(
        &harness.router,
        get("/v1/categories", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed.as_array().expect("category list").len(), 1);
    assert!(listed[0]["retired_at"].is_string(), "{listed}");

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn category_rule_preview_does_not_write_and_rules_are_listed() {
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");
    let (status, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Food"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{category}");
    let category_id = category["id"].as_str().expect("category id");
    let rule_body = json!({
        "matcher": {"SourceCategory": {"value": "Supermarkets"}},
        "category": category_id,
    });

    let (status, created) = call(
        &harness.router,
        post("/v1/category-rules", &harness.owner_token, &rule_body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["version"], 1);

    let (status, before) = call(
        &harness.router,
        get("/v1/category-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    let before_count = before.as_array().expect("rule list").len();
    let operations = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "withdrawal",
            "amount": "500.00",
            "currency": "RUB",
            "dates": {"cash_posted": "2026-08-06"},
            "source_category": "Other",
            "idempotency_key": "preview-other",
        }],
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, impact) = call(
        &harness.router,
        post(
            "/v1/category-rules/preview",
            &harness.owner_token,
            &json!({
                "matcher": {"SourceCategory": {"value": "Other"}},
                "category": category_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["rows"], 1);
    assert_eq!(impact["months"][0]["month"], "2026-08-01");
    assert_eq!(impact["months"][0]["moved"][0]["from"], Value::Null);
    assert_eq!(impact["months"][0]["moved"][0]["to"], category_id);
    assert_eq!(impact["months"][0]["moved"][0]["amount"], "500.00");
    assert_eq!(impact["months"][0]["moved"][0]["rows"], 1);

    let (status, after) = call(
        &harness.router,
        get("/v1/category-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after.as_array().expect("rule list").len(), before_count);

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_row_rule_pins_a_row_whose_source_named_no_identifier() {
    // The Row matcher is the owner's hand-made decision about one specific row
    // and outranks every blanket rule. It keys off the source's own identifier,
    // which a card statement never states — so without the fallback to the
    // client's idempotency key the strongest precedence level is unreachable
    // for exactly the imports that need it.
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");
    let (_, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Gifts"}),
        ),
    )
    .await;
    let category_id = category["id"].as_str().expect("category id").to_owned();

    let account = harness.account;
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "test",
                "source": {"account": account, "channel": "file"},
                "operations": [{
                    "account": account,
                    "type": "withdrawal",
                    "amount": "999.00",
                    "currency": "RUB",
                    "dates": {"cash_posted": "2026-08-28"},
                    "idempotency_key": "tbank/file/deadbeef/1"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_ne!(verdicts[0]["verdict"], "rejected", "{verdicts}");

    let (status, impact) = call(
        &harness.router,
        post(
            "/v1/category-rules/preview",
            &harness.owner_token,
            &json!({
                "matcher": {"kind": "row", "value": {"key": "tbank/file/deadbeef/1"}},
                "category": category_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["rows"], 1, "{impact}");
}

#[tokio::test]
async fn a_description_rule_decomposes_a_row_the_source_category_cannot_separate() {
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");
    let (_, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Groceries"}),
        ),
    )
    .await;
    let category_id = category["id"].as_str().expect("category id").to_owned();

    let account = harness.account;
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "test",
                "source": {"account": account, "channel": "paste"},
                "operations": [{
                    "account": account,
                    "type": "withdrawal",
                    "amount": "123.45",
                    "currency": "RUB",
                    "dates": {"cash_posted": "2026-08-31"},
                    "idempotency_key": "row-1",
                    "source_category": "Супермаркеты",
                    "description": "Corner Shop"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    // Not "accepted": an account with no independent confirmation yields
    // "provisional", and that is the system working as designed. This test is
    // about the description rule, so it pins only that the row was taken.
    assert_ne!(verdicts[0]["verdict"], "rejected", "{verdicts}");

    // Case-insensitive substring, per category.rs:78.
    let (status, impact) = call(
        &harness.router,
        post(
            "/v1/category-rules/preview",
            &harness.owner_token,
            &json!({
                "matcher": {"kind": "description_contains", "value": {"text": "corner shop"}},
                "category": category_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["rows"], 1, "{impact}");

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn flow_report_exposes_category_decomposition_residual_and_rule_versions() {
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");
    let (status, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Food"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{category}");
    let category_id = category["id"].as_str().expect("category id");
    let (status, created_rule) = call(
        &harness.router,
        post(
            "/v1/category-rules",
            &harness.owner_token,
            &json!({
                "matcher": {"SourceCategory": {"value": "Supermarkets"}},
                "category": category_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created_rule}");

    let contour = json!({
        "title": "August flow",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");
    let operations = json!({
        "source_label": "manual entry",
        "operations": [
            {
                "account": harness.account.inner(),
                "type": "withdrawal",
                "amount": "1200.00",
                "currency": "RUB",
                "dates": {"cash_posted": "2026-08-05"},
                "source_category": "Supermarkets",
                "idempotency_key": "food",
            },
            {
                "account": harness.account.inner(),
                "type": "withdrawal",
                "amount": "500.00",
                "currency": "RUB",
                "dates": {"cash_posted": "2026-08-06"},
                "source_category": "Other",
                "idempotency_key": "other",
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/flow?contour={contour_id}&from=2026-08-01&to=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["category_rule_versions"], json!([1]));
    let rub = &body["currencies"][0];
    assert_eq!(rub["went_out_by_category"][0]["category"], category_id);
    assert_eq!(rub["went_out_by_category"][0]["amount"], "1200.00");
    assert_eq!(rub["not_decomposed"]["count"], 1);
    assert_eq!(rub["not_decomposed"]["amount"], "500.00");

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn category_routes_cover_matcher_forms_and_reference_refusals() {
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": Uuid::new_v4(), "title": "Missing"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "not_found");

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Food"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let category = created["id"].as_str().expect("category id");

    let (status, listed) = call(
        &harness.router,
        get("/v1/categories", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed[0]["id"], category);
    assert_eq!(listed[0]["group"], group.to_string());
    assert_eq!(listed[0]["title"], "Food");

    let (status, body) = call(
        &harness.router,
        delete(
            &format!("/v1/categories/{}", Uuid::new_v4()),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "not_found");

    let matcher_forms = [
        json!(r#"{"row":"row-1"}"#),
        json!({"kind": "row", "value": "row-2"}),
        json!({"kind": "source_category", "value": "Supermarkets"}),
        json!({"kind": "description_contains", "value": "cafe"}),
        json!({"description_contains": "bakery"}),
    ];
    for (index, matcher) in matcher_forms.into_iter().enumerate() {
        let (status, body) = call(
            &harness.router,
            post(
                "/v1/category-rules",
                &harness.owner_token,
                &json!({"matcher": matcher, "category": category}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "matcher {index}: {body}");
        assert_eq!(body["category"], category);
        assert_eq!(body["version"], index + 1);
    }

    for (matcher, field, expected) in [
        (json!(42), "matcher", "a category matcher object"),
        (
            json!(r#"{not-json"#),
            "matcher",
            "a category matcher object",
        ),
        (
            json!({}),
            "matcher",
            "row, source_category or description_contains",
        ),
        (
            json!({"kind": "unknown", "value": "x"}),
            "matcher.kind",
            "row, source_category or description_contains",
        ),
        (
            json!({"kind": "row", "value": {"not": "text"}}),
            "matcher",
            "a category matcher with a string value",
        ),
    ] {
        let (status, body) = call(
            &harness.router,
            post(
                "/v1/category-rules",
                &harness.owner_token,
                &json!({"matcher": matcher, "category": category}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "invalid_request");
        assert_eq!(body["field"], field);
        assert_eq!(body["expected"], expected);
    }

    let (status, rules) = call(
        &harness.router,
        get("/v1/category-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rules}");
    assert_eq!(rules.as_array().expect("rule list").len(), 5);
    let (status, impact) = call(
        &harness.router,
        post(
            "/v1/category-rules/preview",
            &harness.owner_token,
            &json!({
                "matcher": {"kind": "source_category", "value": "unused"},
                "category": category,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["rows"], 0);

    for raw in ["not-json", "[]"] {
        store
            .connection()
            .execute(
                "UPDATE category_rules SET matcher = ?1 WHERE version = 1",
                [raw],
            )
            .expect("corrupt matcher");
        let (status, body) = call(
            &harness.router,
            post(
                "/v1/category-rules/preview",
                &harness.owner_token,
                &json!({
                    "matcher": {"kind": "source_category", "value": "x"},
                    "category": category,
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "invalid_request");
        assert_eq!(body["field"], "matcher");
        assert_eq!(body["expected"], "a category matcher object");
    }

    store
        .connection()
        .execute(
            "UPDATE category_rules SET matcher = ?1 WHERE version = 1",
            [r#"{"unknown":"value"}"#],
        )
        .expect("corrupt matcher");
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/category-rules/preview",
            &harness.owner_token,
            &json!({
                "matcher": {"kind": "source_category", "value": "x"},
                "category": category,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "matcher");
    assert_eq!(
        body["expected"],
        "row, source_category, or description_contains"
    );

    drop(harness);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn categories_and_category_rules_require_authentication() {
    let harness = harness();
    let requests = [
        call(&harness.router, get("/v1/categories", None)),
        call(&harness.router, post_public("/v1/categories", &json!({}))),
        call(&harness.router, get("/v1/category-rules", None)),
        call(
            &harness.router,
            post_public("/v1/category-rules", &json!({})),
        ),
        call(
            &harness.router,
            post_public("/v1/category-rules/preview", &json!({})),
        ),
    ];
    for request in requests {
        let (status, body) = request.await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(body["code"], "unauthorized", "{body}");
    }
    let request = Request::builder()
        .uri("/v1/categories/00000000-0000-0000-0000-000000000000")
        .method("DELETE")
        .body(Body::empty())
        .expect("request");
    let (status, body) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "unauthorized", "{body}");
}

#[tokio::test]
async fn retired_category_group_failure_has_actionable_response_fields() {
    let response = ApiFailure::from(AppError::CategoryGroupRetired {
        id: "group-42".to_owned(),
    })
    .into_response();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("JSON response");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "group");
    assert_eq!(body["expected"], "an active category group");
    assert_eq!(body["actual"], "group-42");
}

#[tokio::test]
async fn actions_endpoint_is_authenticated_and_reports_the_empty_owner_frontier() {
    let harness = empty_owner_harness();
    let (status, body) = call(&harness.router, get("/v1/actions", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");

    let (status, body) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["policy_version"], 1);
    let items = body["items"].as_array().expect("action items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "create_first_account");
    assert_eq!(items[0]["kind"], "create_first_account");
    assert_eq!(items[0]["category"], "blocking");
    assert_eq!(items[0]["state"], "needs_owner_input");
    assert_eq!(items[0]["required_scope"], "owner");
    assert_eq!(items[0]["target"]["type"], "operation");
    assert_eq!(items[0]["target"]["operationId"], "create_account");
    assert_eq!(items[0]["target"]["method"], "POST");
    assert_eq!(items[0]["target"]["path"], "/v1/accounts");
    assert_eq!(
        items[0]["target"]["requestSchema"],
        "#/components/schemas/CreateAccountRequest"
    );
    assert_eq!(
        items[0]["target"]["request"]["missing"][0]["pointer"],
        "/title"
    );
    assert_eq!(
        items[0]["target"]["request"]["missing"][0]["provided_by"],
        "owner"
    );
}

#[tokio::test]
async fn actions_endpoint_reports_the_first_contour_and_its_candidates() {
    let harness = harness();
    let (status, body) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().expect("action items");
    assert_eq!(items.len(), 2);
    let item = items
        .iter()
        .find(|item| item["kind"] == "create_first_contour")
        .expect("first contour action");
    assert_eq!(item["kind"], "create_first_contour");
    assert_eq!(item["target"]["operationId"], "create_contour_version");
    assert_eq!(item["target"]["method"], "POST");
    assert_eq!(item["target"]["path"], "/v1/contours");
    assert_eq!(
        item["target"]["requestSchema"],
        "#/components/schemas/CreateContourVersionRequest"
    );
    let missing = item["target"]["request"]["missing"]
        .as_array()
        .expect("missing inputs");
    assert_eq!(missing.len(), 2);
    assert!(missing.iter().any(|entry| entry["pointer"] == "/title"));
    let accounts = missing
        .iter()
        .find(|entry| entry["pointer"] == "/accounts")
        .expect("account candidate input");
    assert_eq!(accounts["provided_by"], "owner");
    assert_eq!(
        accounts["candidates"][0]["id"],
        harness.account.inner().to_string()
    );
}

#[tokio::test]
async fn each_advertised_action_address_reaches_its_handler() {
    let empty = empty_owner_harness();
    let (status, body) = call(
        &empty.router,
        post(
            "/v1/accounts",
            &empty.owner_token,
            &json!({"title": "Main"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let contour = harness();
    let (status, body) = call(
        &contour.router,
        post(
            "/v1/contours",
            &contour.owner_token,
            &json!({"title": "Main", "accounts": [contour.account.inner()]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[test]
fn every_action_kind_resolves_to_one_matching_post_operation() {
    let harness = harness();
    let catalog = ActionCatalog::from_openapi(&harness.api).expect("action catalog");
    for (key, path) in [
        (OperationKey::CreateAccount, "/v1/accounts"),
        (OperationKey::CreateContour, "/v1/contours"),
        (
            OperationKey::RecordOwnerBalance,
            "/v1/reconciliation/balance",
        ),
    ] {
        let resolved = catalog.operation(key);
        assert_eq!(resolved.method, "POST");
        assert_eq!(resolved.path, path);
        let item = harness.api.paths.paths.get(path).expect("path item");
        let operation = item.post.as_ref().expect("post operation");
        assert_eq!(
            operation.operation_id.as_deref(),
            Some(resolved.operation_id.as_str())
        );
    }
}

#[test]
fn action_catalog_rejects_missing_and_duplicate_operation_ids() {
    let harness = harness();

    let mut missing = harness.api.clone();
    missing
        .paths
        .paths
        .get_mut("/v1/accounts")
        .expect("accounts path")
        .post
        .as_mut()
        .expect("accounts operation")
        .operation_id = None;
    assert!(matches!(
        ActionCatalog::from_openapi(&missing),
        Err(ActionCatalogError::MissingOperationId { .. })
    ));

    let mut duplicate = harness.api.clone();
    duplicate
        .paths
        .paths
        .get_mut("/v1/contours")
        .expect("contours path")
        .post
        .as_mut()
        .expect("contours operation")
        .operation_id = Some("create_account".into());
    assert!(matches!(
        ActionCatalog::from_openapi(&duplicate),
        Err(ActionCatalogError::DuplicateOperationId { .. })
    ));

    let mut absent = harness.api;
    absent.paths.paths.remove("/v1/contours");
    assert!(matches!(
        ActionCatalog::from_openapi(&absent),
        Err(ActionCatalogError::MissingActionOperation { operation_id })
            if operation_id == "create_contour_version"
    ));
}

#[test]
fn action_target_is_tagged_and_round_trips_with_an_exclusive_schema() {
    let target = json!({
        "type": "operation",
        "operationId": "create_account",
        "method": "POST",
        "path": "/v1/accounts",
        "requestSchema": "#/components/schemas/CreateAccountRequest",
        "request": {"missing": [{"pointer": "/title", "provided_by": "owner"}]}
    });
    let parsed: iaam_server::dto::ActionTargetDto =
        serde_json::from_value(target.clone()).expect("tagged target");
    assert_eq!(serde_json::to_value(parsed).expect("target JSON"), target);

    let harness = harness();
    let schema = serde_json::to_value(&harness.api).expect("OpenAPI JSON")["components"]["schemas"]
        ["ActionTargetDto"]
        .clone();
    let variants = schema["oneOf"].as_array().expect("tagged oneOf schema");
    assert!(variants.len() >= 2);
    let operation = variants
        .iter()
        .find(|variant| {
            variant["properties"]["type"]["enum"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == "operation"))
        })
        .expect("operation variant");
    let required = operation["required"]
        .as_array()
        .expect("operation required");
    for field in ["operationId", "method", "path", "requestSchema", "request"] {
        assert!(required.iter().any(|value| value == field), "{field}");
    }
}

#[tokio::test]
async fn every_action_request_schema_required_input_is_advertised_as_missing() {
    // The advertised list is read from the endpoint, not written here. An
    // earlier version of this test compared the schema against a literal, which
    // would have stayed green while the response stopped advertising a field —
    // the same shape of mistake this epic exists to remove.
    for harness in [empty_owner_harness(), harness()] {
        let body_of_spec = serde_json::to_value(&harness.api).expect("OpenAPI JSON");
        let (status, body) = call(
            &harness.router,
            get("/v1/actions", Some(&harness.owner_token)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        for item in body["items"].as_array().expect("action items") {
            let target = &item["target"];
            if target["type"] == "none" {
                continue;
            }
            let schema_name = target["requestSchema"]
                .as_str()
                .expect("request schema reference")
                .strip_prefix("#/components/schemas/")
                .expect("component schema reference")
                .to_owned();
            let advertised: Vec<String> = target["request"]["missing"]
                .as_array()
                .expect("missing inputs")
                .iter()
                .map(|entry| {
                    entry["pointer"]
                        .as_str()
                        .expect("missing pointer")
                        .to_owned()
                })
                .collect();
            let preset = target["request"]["preset"].as_object();

            for field in body_of_spec["components"]["schemas"][&schema_name]["required"]
                .as_array()
                .expect("required request fields")
            {
                let name = field.as_str().expect("field name");
                let pointer = format!("/{name}");
                assert!(
                    advertised.iter().any(|value| value == &pointer)
                        || preset.is_some_and(|values| values.contains_key(name)),
                    "{schema_name} requires {pointer}, and the action neither presets \
                     it nor lists it as missing: {target}"
                );
            }
        }
    }
}

/// E9.T5. Every assertion below is made on a carrier's raw JSON, and none of
/// them calls a diagnostic function: `ledger_diagnostics` and `flow_diagnostics`
/// shipped in T4 with their ordering and their filtering already tested, so a
/// test that called them would pass before this attachment existed and prove
/// nothing about it.
///
/// The account filter cannot be checked against the envelope afterwards — the
/// reconciliation response does not name its own subject (`iaam-647w`) — so the
/// fixture carries a gap on a **second** account and the proof is that account's
/// absence from the whole response body.
#[tokio::test]
async fn reconciliation_actions_name_only_the_requested_account() {
    let (harness, path) = harness_on_disk();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Second account" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let other = AccountId(
        Uuid::parse_str(created["id"].as_str().expect("created account id")).expect("account uuid"),
    );

    add_coverage_gap(
        &path,
        harness.owner,
        harness.account,
        date!(2025 - 01 - 01),
        date!(2025 - 01 - 31),
    );
    // A day short of the first gap's end, and still inside the requested range:
    // the journal admits one event per owner, effective date and sequence, so two
    // gaps cannot share an end date. The range covers both, which leaves the
    // account as the only reason either could be excluded.
    add_coverage_gap(
        &path,
        harness.owner,
        other,
        date!(2025 - 01 - 01),
        date!(2025 - 01 - 30),
    );

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

    let actions = response["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 1, "{response}");
    assert_eq!(actions[0]["kind"], "coverage_gap_unrepaired");
    assert!(
        actions[0]["id"]
            .as_str()
            .expect("action id")
            .contains(&harness.account.inner().to_string()),
        "the item must name the account it was asked about: {response}"
    );
    assert!(
        !response.to_string().contains(&other.inner().to_string()),
        "another account's gap must not ride along: {response}"
    );

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

/// The same binding on the other coordinate. Asserted in both directions in one
/// test: a filter that dropped everything would satisfy the exclusion alone.
#[tokio::test]
async fn reconciliation_actions_exclude_a_period_outside_the_requested_range() {
    let (harness, path) = harness_on_disk();
    add_coverage_gap(
        &path,
        harness.owner,
        harness.account,
        date!(2025 - 03 - 01),
        date!(2025 - 03 - 31),
    );

    let (status, march) = call(
        &harness.router,
        get(
            &format!(
                "/v1/reconciliation?account={}&from=2025-03-01&to=2025-03-31",
                harness.account.inner()
            ),
            Some(&harness.readonly_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{march}");
    assert_eq!(
        march["actions"].as_array().expect("actions").len(),
        1,
        "the gap's own range must carry its item: {march}"
    );

    let (status, january) = call(
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
    assert_eq!(status, StatusCode::OK, "{january}");
    assert_eq!(
        january["actions"],
        json!([]),
        "a March gap must not answer a January question: {january}"
    );

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

/// An attached item is the same envelope `/v1/actions` returns, and a blocked one
/// says so by carrying no address and no authorisation for a call that does not
/// exist. This cannot prove the conversion was reused — identical hand-built JSON
/// would pass — and the reuse is a review point, not a test.
#[tokio::test]
async fn an_attached_action_carries_the_whole_envelope_and_names_no_scope() {
    let (harness, path) = harness_on_disk();
    add_coverage_gap(
        &path,
        harness.owner,
        harness.account,
        date!(2025 - 01 - 01),
        date!(2025 - 01 - 31),
    );

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

    let item = &response["actions"][0];
    let keys: std::collections::BTreeSet<&str> = item
        .as_object()
        .expect("action object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["id", "kind", "category", "state", "reason", "target"]),
        "{item}"
    );
    assert_eq!(item["category"], "required_for_goal", "{item}");
    assert_eq!(item["state"], "blocked", "{item}");
    assert_eq!(item["target"], json!({ "type": "none" }), "{item}");
    assert!(
        item["reason"].as_str().expect("reason").contains("row-17"),
        "the prose must carry the refused row it names: {item}"
    );

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

/// `skip_serializing_if` on the new field is exactly the mistake this asserts
/// against: an absent key is indistinguishable from a bug to an agent, while an
/// empty array says the carrier looked and found nothing.
#[tokio::test]
async fn a_clean_instance_carries_actions_present_and_empty() {
    let harness = harness();
    let contour = json!({
        "title": "Empty contour",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");

    let (status, reconciliation) = call(
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
    assert_eq!(status, StatusCode::OK, "{reconciliation}");
    assert!(
        reconciliation.get("actions").is_some(),
        "the key must be present on a clean instance: {reconciliation}"
    );
    assert_eq!(reconciliation["actions"], json!([]), "{reconciliation}");

    let (status, flow) = call(
        &harness.router,
        get(
            &format!("/v1/reports/flow?contour={contour_id}&from=2026-08-01&to=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{flow}");
    assert!(
        flow.get("actions").is_some(),
        "the key must be present on a clean instance: {flow}"
    );
    assert_eq!(flow["actions"], json!([]), "{flow}");
}

/// Category alone leaves ties in generation order, which is not assertable. Two
/// gaps of one category prove the second key is applied.
#[tokio::test]
async fn actions_of_one_category_come_back_in_id_order() {
    let (harness, path) = harness_on_disk();
    add_coverage_gap(
        &path,
        harness.owner,
        harness.account,
        date!(2025 - 01 - 11),
        date!(2025 - 01 - 20),
    );
    add_coverage_gap(
        &path,
        harness.owner,
        harness.account,
        date!(2025 - 01 - 01),
        date!(2025 - 01 - 10),
    );

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

    let actions = response["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 2, "{response}");
    let categories: std::collections::BTreeSet<&str> = actions
        .iter()
        .map(|action| action["category"].as_str().expect("category"))
        .collect();
    assert_eq!(categories.len(), 1, "the tie must be within one category");
    let ids: Vec<&str> = actions
        .iter()
        .map(|action| action["id"].as_str().expect("id"))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "{response}");

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

/// The flow projection admits no leg from outside the contour, so the report
/// cannot name an account it does not cover — and neither may the items riding
/// along with it. An account with its own unexplained residual, left out of the
/// contour, is what proves that.
#[tokio::test]
async fn flow_report_actions_name_only_accounts_in_the_contour() {
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Outside the contour" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let outside = created["id"]
        .as_str()
        .expect("created account id")
        .to_owned();

    let contour = json!({
        "title": "One account only",
        "accounts": [harness.account.inner()],
    });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");

    for (account, key) in [
        (harness.account.inner().to_string(), "inside-opening"),
        (outside.clone(), "outside-opening"),
    ] {
        let operations = json!({
            "source_label": "manual entry",
            "operations": [{
                "account": account,
                "type": "opening_cash",
                "amount": "500.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": key,
            }]
        });
        let (status, verdicts) = call(
            &harness.router,
            post("/v1/ingest/operations", &harness.owner_token, &operations),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{verdicts}");
    }

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/flow?contour={contour_id}&from=2026-08-01&to=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let actions = body["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 1, "{body}");
    assert_eq!(actions[0]["kind"], "unexplained_residual");
    assert!(
        actions[0]["id"]
            .as_str()
            .expect("action id")
            .contains(&harness.account.inner().to_string()),
        "{body}"
    );
    assert!(
        !body.to_string().contains(&outside),
        "an account outside the contour must not ride along: {body}"
    );
}

/// Two rows against one prior event.
///
/// Both rows carry the prior event's canonical content — the fingerprint ignores
/// the source's own row identifier — so each is a possible duplicate of it, and
/// each is recorded and added to what the next row is compared against. The
/// identity of the item keys on the **new** event, so collapsing the two would
/// discard the event the second item exists to name.
struct TwinRowsChannel {
    source: iaam_core::reconciliation::evidence::SourceChannel,
}

#[async_trait::async_trait]
impl BrokerChannel for TwinRowsChannel {
    async fn fetch_operations(
        &self,
        account: AccountId,
        _from: Date,
        _to: Date,
    ) -> Result<ParsedOperations, BrokerError> {
        let row = |operation_id: &str| SubmittedOperation {
            account,
            kind: OperationKind::Deposit {
                amount_minor: 1_000,
                currency: CurrencyCode::Rub,
            },
            dates: OperationDates {
                cash_posted: Some(date!(2025 - 01 - 01)),
                ..Default::default()
            },
            source_time: None,
            idempotency_key: None,
            source_operation_id: Some(operation_id.to_owned()),
            source_category: None,
            description: None,
        };
        Ok(ParsedOperations {
            accepted: vec![row("twin-a"), row("twin-b")],
            quarantined: Vec::new(),
        })
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _at: Date,
    ) -> Result<PortfolioSnapshot, BrokerError> {
        Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Current,
            claims: Vec::new(),
        })
    }

    fn channel(&self) -> iaam_core::reconciliation::evidence::SourceChannel {
        self.source.clone()
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Source
    }
}

#[tokio::test]
async fn a_sync_carries_one_bound_item_per_possible_duplicate() {
    let channel: Arc<dyn BrokerChannel> = Arc::new(TwinRowsChannel {
        source: iaam_core::reconciliation::evidence::SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("contract-test".to_owned()),
            document: None,
        },
    });
    let factory: Arc<dyn BrokerChannelFactory> = Arc::new(FixedChannelFactory { channel });
    let harness = harness_with_factory(
        SqliteStore::open_in_memory().expect("in-memory database"),
        Some(factory),
    );

    // The prior event, recorded through another channel: the same operation,
    // named by neither a row identifier nor a document, so a later import can
    // only suspect it and never prove it.
    let seed = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "10.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-01-01" },
        }]
    });
    let (status, seeded) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &seed),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{seeded}");
    let prior = seeded[0]["event_id"].as_str().expect("seeded event id");

    let (status, response) = call(
        &harness.router,
        post(
            "/v1/brokers/tinkoff/sync",
            &harness.owner_token,
            &json!({
                "account": harness.account.inner(),
                "from": "2025-01-01",
                "to": "2025-01-31",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["possible_duplicates"], 2, "{response}");

    let recorded = response["recorded"].as_array().expect("recorded");
    let new_events: Vec<&str> = recorded
        .iter()
        .filter(|verdict| verdict["verdict"] == "possible_duplicate")
        .map(|verdict| {
            assert_eq!(verdict["of_event_id"], prior, "{response}");
            verdict["event_id"].as_str().expect("event id")
        })
        .collect();
    assert_eq!(new_events.len(), 2, "{response}");

    let actions = response["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 2, "{response}");
    for event in &new_events {
        assert!(
            actions.iter().any(|action| {
                action["kind"] == "possible_duplicate_undecided"
                    && action["id"].as_str().expect("action id").contains(event)
            }),
            "every recorded possible duplicate must have its own item naming it: {response}"
        );
    }
    let ids: std::collections::BTreeSet<&str> = actions
        .iter()
        .map(|action| action["id"].as_str().expect("action id"))
        .collect();
    assert_eq!(ids.len(), 2, "two rows are two items: {response}");
}

/// A bare `Provisional` carries nothing: the frontier already asks for an
/// assertion over the account's whole observed span, a sync knows only its own
/// requested range, and nothing decides which of the two periods to ask for.
#[tokio::test]
async fn a_sync_whose_verdicts_are_all_provisional_carries_an_empty_actions_array() {
    let channel: Arc<dyn BrokerChannel> = Arc::new(PopulatedChannel {
        source: iaam_core::reconciliation::evidence::SourceChannel {
            source: SourceId::new_random(),
            parser_version: ParserVersion("contract-test".to_owned()),
            document: None,
        },
    });
    let factory: Arc<dyn BrokerChannelFactory> = Arc::new(FixedChannelFactory { channel });
    let harness = harness_with_factory(
        SqliteStore::open_in_memory().expect("in-memory database"),
        Some(factory),
    );
    let (status, response) = call(
        &harness.router,
        post(
            "/v1/brokers/tinkoff/sync",
            &harness.owner_token,
            &json!({
                "account": harness.account.inner(),
                "from": "2025-01-01",
                "to": "2025-01-31",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["recorded"][0]["verdict"], "provisional");
    assert!(response.get("actions").is_some(), "{response}");
    assert_eq!(response["actions"], json!([]), "{response}");
}

/// Nothing the document and CSV paths produce has an item: `PossibleDuplicate`
/// is constructed in the broker sync path alone, and every other verdict has no
/// diagnostic. An always-empty array on those two would be a field that can
/// never say anything.
#[tokio::test]
async fn the_csv_and_document_responses_carry_no_actions_key() {
    let harness = harness();
    let document = "date,type,account,counterparty_account,instrument,custody,quantity,amount,fee,accrued_interest,currency,idempotency_key\n\
        2025-01-01,deposit,Brokerage,,,,,1000.00,,,RUB,csv-actions-1\n";
    let request = Request::builder()
        .uri("/v1/ingest/csv")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "text/csv")
        .body(Body::from(document))
        .expect("request");
    let (status, body) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.is_array(), "{body}");
    assert!(!body.to_string().contains("\"actions\""), "{body}");

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
        .expect("request");
    let (status, response) = call(&harness.router, request).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert!(response.get("actions").is_none(), "{response}");
}
