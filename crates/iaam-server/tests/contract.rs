//! Contract tests against the generated spec (§17.1).
//!
//! `utoipa` generates the spec from types and therefore eliminates
//! **data schema** drift. Behaviour — response codes, authentication requirements,
//! actual serialisation — remains outside generation, and is tested
//! only by calling a running server. For a contract used by
//! an external agent, a syntactically valid but behaviourally incorrect spec
//! means the agent will fix itself based on incorrect guidance.

use std::collections::BTreeSet;
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
use iaam_core::report::confidence::{CaveatKind, ReportGoal};
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
        // A fixture key, not an imitation of one the production path writes:
        // this seed is stamped `contract-test`, and nothing here depends on it
        // colliding with a claim the owner-stated route would record.
        idempotency_key: Some(format!(
            "contract-test-assertion:{}:{}:{}",
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
    assert_eq!(body["schema_version"], 12);
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
    // The bytes are the same on both instances, which is the disclosure property:
    // the document is resolved from the generated contract and reads no state, so
    // it cannot say whether an owner exists here.

    let catalog: Value = serde_json::from_slice(&provisioned_body).expect("catalog JSON");
    let context = catalog["linkset"][0].as_object().expect("link context");
    // Every relation the document carries, not a list of the ones it carried when
    // this test was written: a test enumerating relations by hand drifts from the
    // catalog exactly as the catalog used to drift from the router.
    for (relation, links) in context.iter().filter(|(key, _)| key.as_str() != "anchor") {
        for link in links.as_array().expect("link relation") {
            let href = link["href"].as_str().expect("catalog href");
            assert!(
                link["title"]
                    .as_str()
                    .is_some_and(|title| !title.is_empty()),
                "{relation} href {href} is published without saying what it is"
            );
            let request = Request::builder()
                .uri(href)
                .method("GET")
                .header(
                    "Authorization",
                    format!("Bearer {}", provisioned.owner_token),
                )
                .body(Body::empty())
                .expect("request");
            let (status, body) = call(&provisioned.router, request).await;
            // A linked route may refuse the call — most of them require a scope
            // this request does not carry. What it may not do is not exist: an
            // empty `404` is axum reporting no route at all, which is the dead
            // link the entry point must never publish.
            assert!(
                status != StatusCode::NOT_FOUND || body.get("code").is_some(),
                "{relation} href {href} addresses no route"
            );
        }
    }
}

#[tokio::test]
async fn the_catalog_names_the_four_goals_in_the_vocabulary_the_reports_use() {
    // The catalog is built from `ReportGoal::code`, so the two cannot disagree at
    // runtime. This is the guard against the change that would make them able to:
    // a `goal` spelled into the catalog by hand, which reads identically today and
    // drifts the first time the vocabulary moves.
    let harness = harness();
    let (_, _, body) = call_raw(&harness.router, get("/.well-known/api-catalog", None)).await;
    let catalog: Value = serde_json::from_slice(&body).expect("catalog JSON");

    let goals: Vec<&str> = catalog["linkset"][0]["related"]
        .as_array()
        .expect("related links")
        .iter()
        .filter_map(|link| link["goal"].as_str())
        .collect();
    assert_eq!(
        goals,
        ReportGoal::ALL.map(ReportGoal::code).to_vec(),
        "the catalog's goals are not the four the reports publish: {catalog}"
    );

    // And each of them addresses a different route: two goals sharing an href
    // would tell a client the API answers three questions, not four.
    let hrefs: std::collections::BTreeSet<&str> = catalog["linkset"][0]["related"]
        .as_array()
        .expect("related links")
        .iter()
        .filter(|link| link["goal"].is_string())
        .filter_map(|link| link["href"].as_str())
        .collect();
    assert_eq!(
        hrefs.len(),
        goals.len(),
        "two goals share a route: {catalog}"
    );
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

/// One share, one synced closing price, and the evidence that proves its unit.
///
/// Separate from [`seed_market`], whose rows carry a placeholder for the basis
/// evidence and are therefore quoted in a unit nothing establishes — correct
/// for the tests that use them, and unusable for a test about a figure.
async fn seed_share_quote(harness: &Harness) {
    let mut store = harness.market_store.lock().await;
    let lease_expires_at = OffsetDateTime::now_utc() + TimeDuration::days(1);
    store
        .upsert_instrument(&InstrumentRecord {
            id: harness.instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SHR".into(),
            title: "Share One".into(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
        })
        .expect("market instrument");

    let series = SeriesKey {
        source_id: "moex-iss".into(),
        dataset: "prices".into(),
        series_key: format!("{}:TQBR:1", harness.instrument.inner()),
    };
    let run = store
        .begin_run(
            series,
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            lease_expires_at,
        )
        .expect("price run");
    store
        .record_prices(
            &run,
            &"d".repeat(64),
            &[PriceRow {
                instrument_id: harness.instrument.inner().to_string(),
                board: "TQBR".into(),
                session: 1,
                trade_date: "2026-08-03".into(),
                kind: "close".into(),
                observed_at: "2026-08-04T00:00:00Z".into(),
                price: "101.00".into(),
                currency: "RUB".into(),
                quotation_basis: "money_per_unit".into(),
                // The request path the row came from: what proves the unit is
                // money per share rather than a percentage of face value.
                basis_evidence: "iss:engines/stock/markets/shares".into(),
                executability: "executable".into(),
            }],
        )
        .expect("price rows");
    store
        .finish_run(
            &run,
            RunOutcome::Succeeded,
            Some(Coverage {
                from: date!(2026 - 08 - 03),
                to: date!(2026 - 08 - 03),
            }),
        )
        .expect("price publication");
}

/// One instrument, one date, two routes — and one observation behind both.
///
/// The core proves that the two reports share a selection; this proves that the
/// application actually wires the asset snapshot to it. `/v1/reports/assets`
/// once valued holdings from the journal's board alone, so an owner who had
/// synced market data and entered no valuation of his own read a securities
/// half made of caveats while `/v1/reports/returns`, over the same holding on
/// the same day, published a figure.
#[tokio::test]
async fn the_two_report_routes_publish_one_price_for_one_instrument() {
    let harness = harness();
    seed_share_quote(&harness).await;

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Brokerage", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    // A position and no `valuation` operation: the journal knows what he holds
    // and nothing about what it is worth.
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "opening_position",
                    "instrument": harness.instrument.inner(),
                    "custody": harness.custody.inner(),
                    "quantity": "10",
                    "cost_basis": "900.00",
                    "currency": "RUB",
                    "dates": { "trade": "2026-08-01" },
                    "idempotency_key": "agreement-position"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, snapshot) = call(
        &harness.router,
        get(
            &format!("/v1/reports/assets?contour={contour_id}&as_of=2026-08-03"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");

    let (status, returns) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-08-03"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{returns}");

    let snapshot_price = &snapshot["positions"]["holdings"][0]["price"];
    let returns_price = &returns["data_quality"]["position_coverage"]["selected"][0]["price"];
    assert_eq!(snapshot_price["kind"], "selected", "{snapshot}");
    assert_eq!(
        snapshot_price["price"], returns_price["price"],
        "one figure, or the two routes disagree: {snapshot}"
    );
    assert_eq!(snapshot_price["trade_date"], returns_price["trade_date"]);
    assert_eq!(snapshot_price["currency"], returns_price["currency"]);
    assert_eq!(
        snapshot_price["provenance"], returns_price["provenance"],
        "the same source, chosen by the same policy: {snapshot}"
    );
    assert_eq!(snapshot_price["provenance"]["origin"]["kind"], "market");

    // And the figure the snapshot published is that price applied to what he
    // holds, so the agreement above is about the number he reads.
    assert_eq!(
        snapshot["positions"]["holdings"][0]["value"]["value"], "1010.00",
        "{snapshot}"
    );
    assert_eq!(snapshot["confidence"]["complete"], true, "{snapshot}");
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
        "matcher": { "kind": "income" },
        "outcome": { "kind": "external_flow" },
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
    assert_eq!(history[0]["matcher"], json!({ "kind": "income" }));
    assert!(!history.to_string().contains(BROKER_TOKEN), "{history}");

    let (status, body) = call(
        &harness.router,
        delete(
            &format!("/v1/classification-rules/{id}"),
            &harness.owner_token,
        ),
    )
    .await;
    // 200 with the plan, not 204: retiring a rule recomputes what history it
    // leaves needing correction, and a body-less answer would discard it.
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], false, "{body}");

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
        "matcher": { "kind": "income" },
        "outcome": { "kind": "external_flow" },
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

    let from_report = &balances["accounts"][0]["reconciliation"][0];
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
async fn a_two_word_namespace_resolves_under_the_spelling_the_contract_publishes() {
    // `moex_secid` is the one register whose wire code could drift when it
    // passes through a transport enum: the variant is two words, the code is
    // one lower-case string, and the store matches on the string.
    let store = SqliteStore::open_in_memory().expect("in-memory database");
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
            namespace: AliasNamespace::MoexSecid,
            value: "SBER".into(),
            instrument,
            interval: AliasInterval {
                valid_from: date!(2020 - 01 - 01),
                valid_to: None,
            },
            source: SourceId::new_random(),
        })
        .expect("alias");
    let harness = harness_with(store);

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/instruments/resolve",
            &harness.owner_token,
            &json!({"namespace": "moex_secid", "value": "SBER", "on": "2024-03-01"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["instrument"], instrument.inner().to_string());
}

/// The five registers an external code can belong to, in the order the
/// contract lists them.
///
/// Pinned as literals rather than read from `AliasNamespace`: these are wire
/// codes, and a test that derives them from the type it is checking would
/// accept a rename that breaks every client.
const NAMESPACE_CODES: [&str; 5] = ["isin", "moex_secid", "ticker", "figi", "broker_code"];

#[tokio::test]
async fn an_invalid_namespace_is_refused_with_the_valid_ones_named() {
    let (app, token, _) = server_with_one_alias();
    // Typing this field moved its refusal upstream into the body extractor, and
    // for as long as that extractor was axum's own the refusal arrived as text.
    // `refusal` asserts the envelope; the assertions below assert what is in it.
    // Both halves matter: an enumeration in an unparseable body is not an answer.
    let (status, body) = refusal(
        &app,
        post(
            "/v1/instruments/resolve",
            &token,
            &json!({"namespace": "cusip", "value": "037833100", "on": "2024-03-01"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body.get("field").and_then(Value::as_str),
        Some("namespace"),
        "the refusal must name the field it is about: {body}"
    );
    // What the client needs from a refusal is the list it should have chosen
    // from: an agent that has to look the registers up somewhere else guesses,
    // and guessing is what this route was reported for.
    let rendered = body.to_string();
    for code in NAMESPACE_CODES {
        assert!(
            rendered.contains(code),
            "a refused namespace must carry {code}, the enumeration of what is valid: {body}"
        );
    }
}

#[tokio::test]
async fn the_openapi_document_enumerates_every_namespace_and_explains_the_resolve_request() {
    // Reported from outside: an agent guessed `code_kind`, `code` and `as_of`
    // before reading the schema, and the schema then did not say which
    // namespaces exist. Both halves are checked here.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let request = &spec["components"]["schemas"]["ResolveInstrumentRequest"];
    assert!(
        refers_to(&request["properties"]["namespace"], "AliasNamespaceDto"),
        "the namespace field must point at the enumerated vocabulary: {}",
        request["properties"]["namespace"]
    );
    assert_eq!(
        published_vocabulary(&spec, "AliasNamespaceDto"),
        NAMESPACE_CODES,
        "the contract must list every register an external code can belong to"
    );

    for field in ["namespace", "value", "on"] {
        assert!(
            !request["properties"][field]["description"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .is_empty(),
            "the resolve request field {field} arrives without a meaning"
        );
    }
}

#[tokio::test]
async fn every_namespace_code_arrives_with_the_sentence_that_explains_it() {
    // The five registers were published as a bare list of strings: utoipa
    // renders a unit-variant enum as `{"type":"string","enum":[…]}` and
    // discards the doc comment beside each variant, so the meanings written in
    // `dto.rs` reached a reader of `dto.rs` and nobody else. `moex_secid` next
    // to `ticker` is exactly the choice a client gets wrong when the document
    // says only that both exist.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    // Published by the same mechanism as the verdict codes, and read the same
    // way: `published_vocabulary` fails if a code arrives without a meaning.
    let codes = published_vocabulary(&spec, "AliasNamespaceDto");
    assert_eq!(codes, NAMESPACE_CODES);

    let meanings: Vec<&str> = spec["components"]["schemas"]["AliasNamespaceDto"]["oneOf"]
        .as_array()
        .expect("the registers")
        .iter()
        .map(|item| item["description"].as_str().expect("a meaning"))
        .collect();
    let mut distinct = meanings.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        meanings.len(),
        // One sentence repeated across five codes explains none of them.
        "two registers are explained by the same sentence: {meanings:?}"
    );

    // A schema change is not permission to change what the route accepts: the
    // codes above are the ones the route still resolves under, which
    // `a_two_word_namespace_resolves_under_the_spelling_the_contract_publishes`
    // exercises end to end for the one code that could drift.
    assert!(
        spec["components"]["schemas"]["AliasNamespaceDto"]
            .get("enum")
            .is_none(),
        "the bare enumeration is still published beside the explained one"
    );
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
        let (status, price_series) = call(&harness.router, get(&prices_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(price_series["complete_through"], "2026-08-03");
        let prices = price_series["rows"].as_array().expect("price rows");
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

        let (status, fx_series) = call(&harness.router, get(fx_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fx_series["complete_through"], "2026-08-03");
        let fx = fx_series["rows"].as_array().expect("exchange-rate rows");
        assert_eq!(fx.len(), 1);
        for field in ["value", "date", "source", "observed_at", "quality"] {
            assert!(
                fx[0].get(field).is_some(),
                "exchange rate has no {field}: {}",
                fx[0]
            );
        }
        assert_eq!(fx[0]["source"], "cbr");
        assert_eq!(fx[0]["quality"], "official");

        let (status, key_rate_series) =
            call(&harness.router, get(key_rate_path, Some(token))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(key_rate_series["complete_through"], "2026-08-10");
        let key_rates = key_rate_series["rows"]
            .as_array()
            .expect("key-rate interval rows");
        assert_eq!(key_rates.len(), 2);
        assert_eq!(key_rates[0]["observed_at"], "2026-08-20T00:00:00Z");
        assert_eq!(key_rates[0]["quality"], "observed");
        assert_eq!(key_rates[1]["boundary"], "inferred_across_non_trading_days");
        assert_eq!(key_rates[1]["quality"], "inferred");
    }

    for path in [prices_path.as_str(), fx_path, key_rate_path] {
        let (status, _) = call(&harness.router, get(path, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "route is open: {path}");
    }
}

#[tokio::test]
async fn an_empty_series_still_says_how_far_the_data_goes() {
    // An agent asking for a period this instance holds nothing for was told `[]`,
    // which reads the same as "the series is complete and there is simply no
    // value in that interval". The two are different facts, and only the
    // completeness boundary tells them apart, so it rides on the answer rather
    // than on a row that may not exist.
    let held = harness();
    seed_market(&held).await;
    let empty = harness();

    let prices_of = |instrument: iaam_core::ids::InstrumentId, from: &str, to: &str| {
        format!(
            "/v1/market/prices?instrument={}&board=TQBR&session=1&from={from}&to={to}&knowledge_as_of=2099-01-01T00:00:00Z",
            instrument.inner()
        )
    };
    // Same shape of question on each of the three routes: an interval the
    // instance holds no row for.
    let cases = [
        (
            "prices",
            prices_of(held.instrument, "2026-08-04", "2026-08-10"),
            prices_of(empty.instrument, "2026-08-04", "2026-08-10"),
            "2026-08-03",
        ),
        (
            "fx",
            "/v1/market/fx?base=USD&quote=RUB&from=2026-08-01&to=2026-08-02&knowledge_as_of=2099-01-01T00:00:00Z".to_owned(),
            "/v1/market/fx?base=USD&quote=RUB&from=2026-08-01&to=2026-08-02&knowledge_as_of=2099-01-01T00:00:00Z".to_owned(),
            "2026-08-03",
        ),
        (
            "key-rate",
            "/v1/market/key-rate?from=2026-07-01&to=2026-07-31&knowledge_as_of=2099-01-01T00:00:00Z".to_owned(),
            "/v1/market/key-rate?from=2026-07-01&to=2026-07-31&knowledge_as_of=2099-01-01T00:00:00Z".to_owned(),
            "2026-08-10",
        ),
    ];

    for (route, held_path, empty_path, boundary) in cases {
        // No value in this interval — but the series is known through `boundary`.
        let (status, body) = call(&held.router, get(&held_path, Some(&held.agent_token))).await;
        assert_eq!(status, StatusCode::OK, "{route} was refused: {body}");
        assert!(
            body["rows"].as_array().expect("rows").is_empty(),
            "{route} answered rows for an interval it holds none for: {body}"
        );
        assert_eq!(
            body["complete_through"], boundary,
            "{route} lost the completeness boundary on an empty answer: {body}"
        );

        // Nothing held for the series at all — the boundary is explicitly absent.
        let (status, body) = call(&empty.router, get(&empty_path, Some(&empty.agent_token))).await;
        assert_eq!(status, StatusCode::OK, "{route} was refused: {body}");
        assert!(
            body["rows"].as_array().expect("rows").is_empty(),
            "{route} answered rows from an instance holding nothing: {body}"
        );
        assert!(
            body.get("complete_through").is_some_and(Value::is_null),
            "{route} did not say that nothing is held: {body}"
        );
    }
}

#[tokio::test]
async fn the_market_series_wrapper_is_written_down_in_the_contract() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    for (route, schema_name, row_schema) in [
        (
            "/v1/market/prices",
            "MarketPriceSeriesDto",
            "MarketPriceDto",
        ),
        ("/v1/market/fx", "MarketFxSeriesDto", "MarketFxDto"),
        (
            "/v1/market/key-rate",
            "MarketKeyRateSeriesDto",
            "MarketKeyRateDto",
        ),
    ] {
        let body = &spec["paths"][route]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"];
        assert_eq!(
            body["$ref"],
            format!("#/components/schemas/{schema_name}"),
            "{route} does not answer the series wrapper: {body}"
        );

        let schema = &spec["components"]["schemas"][schema_name];
        assert_eq!(
            schema["properties"]["rows"]["items"]["$ref"],
            format!("#/components/schemas/{row_schema}"),
            "{schema_name} does not name its rows: {schema}"
        );
        assert_eq!(
            schema["properties"]["complete_through"]["format"], "date",
            "{schema_name} does not describe the boundary as a date: {schema}"
        );
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required fields")
            .iter()
            .map(|field| field.as_str().expect("field name"))
            .collect();
        assert!(
            required.contains(&"rows") && required.contains(&"complete_through"),
            // A boundary the contract lets an answer omit is one an agent
            // will not look for.
            "{schema_name} lets an answer omit a field: {schema}"
        );

        // The boundary is a property of the answer, not of a row: a row that
        // repeated it would invite a client to believe it could differ per row.
        assert!(
            spec["components"]["schemas"][row_schema]["properties"]
                .get("complete_through")
                .is_none(),
            "{row_schema} still carries the series boundary"
        );
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
    assert_eq!(
        body["rows"].as_array().expect("exchange-rate rows").len(),
        1
    );

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
async fn one_name_for_the_currency_pair_runs_from_the_query_to_the_row() {
    // The query learned `base`/`quote` when `from` and `to` collided with the
    // interval, and the two bodies kept the old spelling: an agent asked for a
    // pair under one pair of names and read it back under another, which is the
    // same defect one level down. Request body, response row and query all say
    // `base`/`quote` now.
    let harness = harness();
    seed_market(&harness).await;

    let path = "/v1/market/fx?base=USD&quote=RUB&from=2026-08-01&to=2026-08-03";
    let (status, body) = call(&harness.router, get(path, Some(&harness.agent_token))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "exchange rates were refused: {body}"
    );
    let row = &body["rows"][0];
    assert_eq!(row["base"], "USD", "the row does not name the base: {row}");
    assert_eq!(
        row["quote"], "RUB",
        "the row does not name the quote: {row}"
    );
    assert!(
        row.get("from").is_none() && row.get("to").is_none(),
        "the row still spells the pair the way the interval is spelled: {row}"
    );

    // The owner-supplied rates are a request body, and they take the same two
    // names: a client that sends `base` and reads `base` learns one vocabulary.
    let (_, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Pair", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    let contour_id = contour_response["contour"].as_str().expect("scope");
    let report_path =
        format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-01");

    let (status, body) = call(
        &harness.router,
        post(
            &report_path,
            &harness.owner_token,
            &json!([{ "base": "USD", "quote": "RUB", "date": "2026-01-01", "rate": "90.00" }]),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a rate spelled as a pair was refused: {body}"
    );

    let (status, _) = call(
        &harness.router,
        post(
            &report_path,
            &harness.owner_token,
            &json!([{ "from": "USD", "to": "RUB", "date": "2026-01-01", "rate": "90.00" }]),
        ),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "the old spelling is still accepted beside the new one"
    );

    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);
    for schema in ["FxRateDto", "MarketFxDto"] {
        let properties = &spec["components"]["schemas"][schema]["properties"];
        for field in ["base", "quote"] {
            assert!(
                properties.get(field).is_some(),
                "{schema} does not publish {field}: {properties}"
            );
        }
        for field in ["from", "to"] {
            assert!(
                properties.get(field).is_none(),
                "{schema} still spells the pair {field}: {properties}"
            );
        }
    }
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
    let rows = body["accounts"].as_array().expect("balance rows");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["account"], harness.account.inner().to_string());
    assert_eq!(row["cash"][0]["currency"], "RUB");
    assert_eq!(row["cash"][0]["kind"], "movement_since_unknown_start");
    assert_eq!(row["cash"][0]["movement"], "3000.00");
    // Nothing anchors this figure, so nothing in it is spelled `balance`.
    assert!(row["cash"][0].get("balance").is_none());
    assert_eq!(body["negative_cash"], json!([]));
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
    let rows = body["accounts"].as_array().expect("balance rows");
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
                // Every outcome says what it was compared against, this one
                // included: no events, no window, and nothing folded from a
                // state (`iaam-lg2t`).
                "basis": {
                    "events_folded": 0,
                    "folded_from": null,
                    "folded_through": null,
                    "start": "no_recorded_movement",
                    "compared": "level",
                    "compared_since": null,
                },
            }],
            "taints": [],
        }])
    );
    assert_eq!(unstated["reconciliation"], json!([]));

    drop(harness);
    let _ = std::fs::remove_file(&path);
}

/// The reported defect: an instance with one month imported and no opening
/// assertion showed a negative cash balance on an account that cannot be
/// overdrawn. The number was a running sum from an unknown start presented as a
/// balance, and nothing on the answer said so.
///
/// It said so afterwards, in an `opening` field beside the amount, and that was
/// still not enough — the amount could be read without the field, and was. So
/// the figure now names itself: there is no `amount` to read, `movement` is
/// spelled `movement`, and only an anchored figure is spelled `balance`.
#[tokio::test]
async fn a_cash_figure_says_whether_its_start_was_asserted() {
    let harness = harness();
    let contour = json!({
        "title": "August starts",
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
            "type": "deposit",
            "amount": "3000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-05" },
            "idempotency_key": "start-deposit"
        }]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let balances = format!("/v1/reports/balances?contour={contour_id}&as_of=2026-08-31");
    let (status, body) = call(&harness.router, get(&balances, Some(&harness.owner_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cash = &body["accounts"][0]["cash"][0];
    assert_eq!(cash["kind"], "movement_since_unknown_start", "{body}");
    assert_eq!(cash["movement"], "3000.00", "{body}");
    // The figure a reader would have lifted out and called a balance is not
    // there under any name it could be mistaken for.
    assert!(cash.get("amount").is_none(), "{body}");
    assert!(cash.get("balance").is_none(), "{body}");

    // The remedy the queue asks for: an assertion about the state before the
    // interval's first event, recorded through the ordinary operation.
    let opening = json!({
        "account": harness.account.inner(),
        "from": "2026-08-01",
        "to": "2026-08-31",
        "at": "opening",
        "cash": { "currency": "RUB", "amount": "0.00" },
    });
    let (status, recorded) = call(
        &harness.router,
        post("/v1/reconciliation/balance", &harness.owner_token, &opening),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");

    let (status, body) = call(&harness.router, get(&balances, Some(&harness.owner_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cash = &body["accounts"][0]["cash"][0];
    assert_eq!(cash["kind"], "balance", "{body}");
    assert_eq!(cash["balance"], "3000.00", "{body}");
    assert!(cash.get("movement").is_none(), "{body}");
}

/// An assertion that opens after the first movement leaves everything before it
/// unasserted: the sum is still a running one, and saying otherwise would mark
/// as a balance a figure with an unknown start.
#[tokio::test]
async fn an_opening_assertion_that_starts_too_late_asserts_nothing() {
    let harness = harness();
    let contour = json!({
        "title": "Late opening",
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
            "type": "deposit",
            "amount": "3000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-05" },
            "idempotency_key": "late-opening-deposit"
        }]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let late = json!({
        "account": harness.account.inner(),
        "from": "2026-08-10",
        "to": "2026-08-31",
        "at": "opening",
        "cash": { "currency": "RUB", "amount": "3000.00" },
    });
    let (status, recorded) = call(
        &harness.router,
        post("/v1/reconciliation/balance", &harness.owner_token, &late),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");

    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-08-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["accounts"][0]["cash"][0]["kind"], "movement_since_unknown_start",
        "{body}"
    );
}

/// A negative cash balance is a fact the answer states, not an error, not a
/// refusal, and not a row the report drops. A technical overdraft is real;
/// whether this one is a problem is the reader's judgement, and the answer
/// gives them the number and the start it was accumulated from.
///
/// The second account pins the finding the negative one only illustrates: from
/// an unasserted start the plausible positive figure is exactly as unfounded as
/// the impossible negative. Both are marked; only one is anomalous.
#[tokio::test]
async fn a_negative_cash_balance_is_stated_by_the_answer_and_not_refused() {
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Plausible" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let plausible = created["id"].as_str().expect("account id").to_owned();
    let contour = json!({
        "title": "Overdrawn",
        "accounts": [harness.account.inner(), plausible],
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
                "amount": "1000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": "negative-deposit"
            },
            {
                "account": harness.account.inner(),
                "type": "withdrawal",
                "amount": "2500.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-06" },
                "idempotency_key": "negative-withdrawal"
            },
            {
                "account": plausible,
                "type": "deposit",
                "amount": "4200.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": "plausible-deposit"
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
    let rows = body["accounts"].as_array().expect("balance rows");
    let overdrawn_id = harness.account.inner().to_string();
    let overdrawn = rows
        .iter()
        .find(|row| row["account"].as_str() == Some(overdrawn_id.as_str()))
        .expect("overdrawn row");
    let positive = rows
        .iter()
        .find(|row| row["account"].as_str() == Some(plausible.as_str()))
        .expect("plausible row");

    // The row is there, carrying the negative number rather than hiding it.
    assert_eq!(overdrawn["cash"][0]["movement"], "-1500.00", "{body}");
    assert_eq!(positive["cash"][0]["movement"], "4200.00", "{body}");
    // Both were accumulated from an unasserted start, so neither is spelled as
    // a balance. The plausible one is not exempt for looking plausible.
    assert_eq!(
        overdrawn["cash"][0]["kind"], "movement_since_unknown_start",
        "{body}"
    );
    assert_eq!(
        positive["cash"][0]["kind"], "movement_since_unknown_start",
        "{body}"
    );

    // The entry is the open §11 span seen with its amount: same key, plus the
    // date it opened and what the perimeter makes of it. `resolved` is null
    // because the balance is still negative at the report date.
    assert_eq!(
        body["negative_cash"],
        json!([{
            "account": overdrawn_id,
            "currency": "RUB",
            "amount": "-1500.00",
            "from": "2026-08-06",
            "resolved": null,
            "classification": "unclassified_negative_cash",
            // The owner has said nothing about this account, so there is
            // nothing to contradict. Silence is not a statement.
            "expectation": null,
            "contradicts_expectation": false,
        }]),
        "{body}"
    );

    // An unexplained deficit refuses the period's reports for its own account
    // (§11) — and for no other. The refusal does not take the figure away: the
    // row above still carries `-1500.00`.
    assert_eq!(overdrawn["period_reports"], "refused", "{body}");
    assert_eq!(
        overdrawn["period_reports_refused"],
        json!([{
            "currency": "RUB",
            "from": "2026-08-06",
            "resolved": null,
            "classification": "unclassified_negative_cash",
        }]),
        "{body}"
    );
    assert_eq!(positive["period_reports"], "calculated", "{body}");
    assert_eq!(positive["period_reports_refused"], json!([]), "{body}");
}

/// The defect this pins: §11 was implemented in full — three classifications,
/// spans, and `blocks_period_reports` — and no request executed it, so the
/// balances answer replied as if the perimeter did not exist.
///
/// The margin case is the one the perimeter exists for: the account carries
/// financing from outside it, the system does not reconstruct that economics,
/// and the period's reports are refused **for that account**. The second
/// account is the other half of §11 and the half that is easy to lose: the
/// remainder must still be calculated. A refusal that spread to the scope would
/// let one unrecognised row disable the whole portfolio.
#[tokio::test]
async fn margin_financing_refuses_one_accounts_period_reports_and_no_others() {
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Unencumbered" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let healthy = created["id"].as_str().expect("account id").to_owned();
    let margin = harness.account.inner().to_string();

    let contour = json!({
        "title": "Margin and not",
        "accounts": [margin, healthy],
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
                "account": margin,
                "type": "deposit",
                "amount": "1000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": "margin-deposit"
            },
            {
                "account": margin,
                "type": "withdrawal",
                "amount": "2500.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-06" },
                "idempotency_key": "margin-withdrawal"
            },
            // The credit indicator. Without it the same deficit would be
            // unclassified; with it the system knows it is financing it does
            // not support.
            {
                "account": margin,
                "type": "fee",
                "amount": "40.00",
                "currency": "RUB",
                "origin": "margin_interest",
                "dates": { "cash_posted": "2026-08-07" },
                "idempotency_key": "margin-interest"
            },
            {
                "account": healthy,
                "type": "deposit",
                "amount": "4200.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": "healthy-deposit"
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
    // A per-account exclusion inside a 200, not a 4xx for the request: the
    // answer is calculable, and refusing all of it would refuse the accounts
    // §11 says to keep calculating.
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["accounts"].as_array().expect("balance rows");
    let blocked = rows
        .iter()
        .find(|row| row["account"].as_str() == Some(margin.as_str()))
        .expect("margin row");
    let calculated = rows
        .iter()
        .find(|row| row["account"].as_str() == Some(healthy.as_str()))
        .expect("healthy row");

    assert_eq!(blocked["period_reports"], "refused", "{body}");
    assert_eq!(
        blocked["period_reports_refused"],
        json!([{
            "currency": "RUB",
            "from": "2026-08-06",
            "resolved": null,
            "classification": "unsupported_margin_liability",
        }]),
        "{body}"
    );
    // The observable cash effect is retained: the perimeter declines to
    // reconstruct the economics, not to state the figure.
    assert_eq!(blocked["cash"][0]["movement"], "-1540.00", "{body}");

    assert_eq!(calculated["period_reports"], "calculated", "{body}");
    assert_eq!(calculated["period_reports_refused"], json!([]), "{body}");
    assert_eq!(calculated["cash"][0]["movement"], "4200.00", "{body}");

    assert_eq!(
        body["negative_cash"],
        json!([{
            "account": margin,
            "currency": "RUB",
            "amount": "-1540.00",
            "from": "2026-08-06",
            "resolved": null,
            "classification": "unsupported_margin_liability",
            // §11 classified this from evidence and needed no owner input. The
            // owner's expectation is a separate, absent statement, and the two
            // are layered rather than competing.
            "expectation": null,
            "contradicts_expectation": false,
        }]),
        "{body}"
    );
}

/// A deficit closed by settlement inside the permitted term is ordinary
/// operation, and §11 says so in as many words. The period's reports go on
/// being calculated, and the answer says `calculated` rather than staying
/// silent — silence would be indistinguishable from an account nobody assessed.
#[tokio::test]
async fn a_temporary_settlement_deficit_does_not_refuse_the_period_reports() {
    let harness = harness();
    let account = harness.account.inner().to_string();
    let contour = json!({
        "title": "Settlement timing",
        "accounts": [account],
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
                "account": account,
                "type": "withdrawal",
                "amount": "2500.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-06" },
                "idempotency_key": "deficit-withdrawal"
            },
            // Two days later, inside the default five-day window.
            {
                "account": account,
                "type": "deposit",
                "amount": "3000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-08" },
                "idempotency_key": "deficit-settlement"
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
    let row = &body["accounts"][0];
    assert_eq!(row["period_reports"], "calculated", "{body}");
    assert_eq!(row["period_reports_refused"], json!([]), "{body}");
    assert_eq!(row["cash"][0]["movement"], "500.00", "{body}");
    // The span closed before the report date, so nothing is negative now.
    assert_eq!(body["negative_cash"], json!([]), "{body}");
}

/// The two ends of an interval, in the order the contract lists them.
///
/// Pinned as literals rather than read from `BalancePoint`: these are wire
/// codes, and a test that derives them from the type it is checking would
/// accept a rename that breaks every client and every action preset already
/// issued.
const BALANCE_POINT_CODES: [&str; 2] = ["opening", "closing"];

#[tokio::test]
async fn the_openapi_document_enumerates_and_explains_both_balance_points() {
    // The field was a bare `String`, so the document said only that a string
    // was wanted, and a caller reaching for the start of the interval could
    // write `open`, `start` or `begin` and learn the answer by being refused.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let request = &spec["components"]["schemas"]["OwnerBalanceRequest"];
    assert!(
        refers_to(&request["properties"]["at"], "BalancePointDto"),
        "the balance point field must point at the enumerated vocabulary: {}",
        request["properties"]["at"]
    );
    assert_eq!(
        published_vocabulary(&spec, "BalancePointDto"),
        BALANCE_POINT_CODES,
        "the contract must list both ends of the interval"
    );

    // One sentence for the field cannot carry the distinction the caller needs:
    // whether the interval's own events are inside the figure or outside it.
    let meanings: Vec<&str> = spec["components"]["schemas"]["BalancePointDto"]["oneOf"]
        .as_array()
        .expect("the points")
        .iter()
        .map(|item| item["description"].as_str().expect("a meaning"))
        .collect();
    assert_ne!(
        meanings[0], meanings[1],
        "both points are explained by the same sentence: {meanings:?}"
    );

    assert!(
        spec["components"]["schemas"]["BalancePointDto"]
            .get("enum")
            .is_none(),
        "the bare enumeration is still published beside the explained one"
    );
}

#[tokio::test]
async fn an_invalid_balance_point_is_refused_with_both_codes_named() {
    // `open` is the guess the old contract invited: it published a string, and
    // the two values it would accept lived in the handler.
    let harness = harness();
    let (status, body) = refusal(
        &harness.router,
        post(
            "/v1/reconciliation/balance",
            &harness.owner_token,
            &json!({
                "account": harness.account.inner(),
                "from": "2026-08-01",
                "to": "2026-08-31",
                "at": "open",
                "cash": { "currency": "RUB", "amount": "0.00" },
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body.get("field").and_then(Value::as_str),
        Some("at"),
        "the refusal must name the field it is about: {body}"
    );
    let rendered = body.to_string();
    for code in BALANCE_POINT_CODES {
        assert!(
            rendered.contains(code),
            "a refused balance point must carry {code}, the enumeration of what is valid: {body}"
        );
    }
}

/// The preset is a value the caller reads out of an action and sends back
/// unread. Typing the field is not permission to change what the queue writes
/// there, so the round trip is exercised with the preset itself rather than
/// with a literal a test author chose.
#[tokio::test]
async fn a_balance_point_taken_from_an_action_is_accepted_verbatim() {
    let harness = harness();
    let operations = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "1500.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-05" },
            "idempotency_key": "preset-round-trip-deposit"
        }]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    // Both points, in the order the queue asks for them: the closing question
    // is not put until the opening one is answered.
    for expected in BALANCE_POINT_CODES {
        let (status, body) = call(
            &harness.router,
            get("/v1/actions", Some(&harness.owner_token)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let action = body
            .as_array()
            .expect("action items")
            .iter()
            .find(|item| item["kind"] == "provide_control_assertion")
            .unwrap_or_else(|| panic!("no assertion request for the {expected} point: {body}"))
            .clone();
        let preset = &action["target"]["request"]["preset"];
        assert_eq!(preset["at"], expected, "{action}");

        let recorded = json!({
            "account": preset["account"],
            "from": preset["from"],
            "to": preset["to"],
            // Sent back exactly as it was read: no mapping, no spelling of our
            // own. A preset the route will not accept is a queue that asks for
            // work it then refuses.
            "at": preset["at"],
            "cash": { "currency": "RUB", "amount": "0.00" },
        });
        let (status, response) = call(
            &harness.router,
            post(
                "/v1/reconciliation/balance",
                &harness.owner_token,
                &recorded,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the {expected} preset: {response}");
    }
}

/// The queue asks for the opening point first and does not put the closing
/// question before the opening one is answered.
#[tokio::test]
async fn the_action_queue_asks_for_the_opening_balance_before_the_closing_one() {
    let harness = harness();
    let operations = json!({
        "source_label": "manual entry",
        "operations": [{
            "account": harness.account.inner(),
            "type": "deposit",
            "amount": "3000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-05" },
            "idempotency_key": "queue-deposit"
        }]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let assertion_requests = |body: &Value| -> Vec<Value> {
        body.as_array()
            .expect("action items")
            .iter()
            .filter(|item| item["kind"] == "provide_control_assertion")
            .cloned()
            .collect()
    };

    let (status, body) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outstanding = assertion_requests(&body);
    assert_eq!(outstanding.len(), 1, "{body}");
    let opening = &outstanding[0];
    assert_eq!(opening["target"]["request"]["preset"]["at"], "opening");
    assert_eq!(opening["target"]["operationId"], "record_owner_balance");
    let opening_id = opening["id"].as_str().expect("action id").to_owned();

    let recorded = json!({
        "account": harness.account.inner(),
        "from": opening["target"]["request"]["preset"]["from"],
        "to": opening["target"]["request"]["preset"]["to"],
        "at": "opening",
        "cash": { "currency": "RUB", "amount": "0.00" },
    });
    let (status, response) = call(
        &harness.router,
        post(
            "/v1/reconciliation/balance",
            &harness.owner_token,
            &recorded,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let (status, body) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outstanding = assertion_requests(&body);
    assert_eq!(outstanding.len(), 1, "{body}");
    let closing = &outstanding[0];
    assert_eq!(closing["target"]["request"]["preset"]["at"], "closing");
    // One kind, two identities: an agent deduplicating by id sees new work
    // rather than the question it already answered.
    assert_eq!(closing["kind"], opening["kind"]);
    assert_ne!(closing["id"].as_str().expect("action id"), opening_id);
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
    // A bare array, under §1 of `docs/api/conventions.md`. The wrapper existed
    // for `policy_version`, and `policy_version` was a literal nothing derived
    // and nothing bumped — so there was no fact about the answer as a whole for
    // the object to carry, which is exactly what the rule asks.
    let items = body.as_array().expect("action items");
    assert!(body.get("policy_version").is_none(), "{body}");
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
    let items = body.as_array().expect("action items");
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
        // An alternative resolution is resolved through the same catalogue as a
        // sole one: an option the queue publishes and the catalogue cannot
        // address would be a route named and not reachable.
        (OperationKey::RecordAccountScope, "/v1/accounts/{id}/scope"),
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
    for field in ["operationId", "method", "path", "request"] {
        assert!(required.iter().any(|value| value == field), "{field}");
    }
    // `requestSchema` is declared and is not required: a call that takes no
    // request body has no request schema, and the one such call an action or a
    // refusal offers is abandoning an import session. Present in the properties
    // so a client knows to look for it; absent from `required` so its absence is
    // a shape the contract admits rather than a violation of it.
    assert!(
        operation["properties"]
            .get("requestSchema")
            .is_some_and(|schema| !schema.is_null()),
        "the schema reference must still be described: {operation}"
    );
    assert!(
        !required.iter().any(|value| value == "requestSchema"),
        "a body-less call has no request schema: {operation}"
    );
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

        for item in body.as_array().expect("action items") {
            let target = &item["target"];
            // A target carrying several ways out is swept option by option, the
            // same way `ActionTarget::resolutions` reads it. Checking only the
            // sole-operation shape would leave every alternative resolution
            // unchecked, which is precisely the reachable-but-undescribed state
            // the `options` variant was added to end.
            let resolutions: Vec<&serde_json::Value> = match target["type"].as_str() {
                Some("none") => Vec::new(),
                Some("options") => target["options"]
                    .as_array()
                    .expect("resolution options")
                    .iter()
                    .collect(),
                _ => vec![target],
            };
            for resolution in resolutions {
                let schema_name = resolution["requestSchema"]
                    .as_str()
                    .expect("request schema reference")
                    .strip_prefix("#/components/schemas/")
                    .expect("component schema reference")
                    .to_owned();
                let advertised: Vec<String> = resolution["request"]["missing"]
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
                let preset = resolution["request"]["preset"].as_object();

                // A schema whose every field is optional emits no `required`
                // list at all, and requires nothing; the sweep is then vacuous
                // rather than absent. `OpenImportSessionRequest` is the first
                // such schema an action addresses.
                let required = body_of_spec["components"]["schemas"][&schema_name]["required"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for field in &required {
                    let name = field.as_str().expect("field name");
                    let pointer = format!("/{name}");
                    assert!(
                        advertised.iter().any(|value| value == &pointer)
                            || preset.is_some_and(|values| values.contains_key(name)),
                        "{schema_name} requires {pointer}, and the action neither presets \
                         it nor lists it as missing: {resolution}"
                    );
                }
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
        // `subject` joined the envelope with the coverage goal: a blocked
        // diagnostic still names the account it is about, in a typed field
        // rather than only in its sentence. `goals` joined it with
        // `iaam-f5e7`: `required_for_goal` named no goal, so a client could not
        // tell an item that stops one report from an item that stops all four.
        std::collections::BTreeSet::from([
            "id", "kind", "category", "goals", "state", "reason", "subject", "target",
        ]),
        "{item}"
    );
    assert_eq!(
        item["subject"],
        json!({
            "type": "account",
            "id": harness.account.inner(),
            "title": "Brokerage",
        }),
        "{item}"
    );
    assert_eq!(item["category"], "required_for_goal", "{item}");
    // A coverage gap is a statement about one import attempt's confirmation —
    // `EventKind::ImportCoverageGap` says the refused rows may already be in the
    // journal from another channel — so it stands in the way of reconciliation
    // and of nothing else.
    assert_eq!(item["goals"], json!(["reconciliation"]), "{item}");
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

/// `iaam-z7q6`. The undecomposed total is two unlike things, and the queue used to
/// answer both with `blocked` — "no operation in this API is available for this
/// item" — while category-rule creation sits in this same contract. Split at the
/// source: the rows a rule can reach name that operation and the fields only the
/// owner can supply, and the transfer says truthfully that no rule applies to it.
#[tokio::test]
async fn an_outflow_names_the_rule_operation_and_a_transfer_names_no_remedy() {
    let harness = harness();
    let (status, outside_account) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Outside the contour" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{outside_account}");
    let outside = outside_account["id"]
        .as_str()
        .expect("account id")
        .to_owned();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "One account only",
                "accounts": [harness.account.inner()],
            }),
        ),
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
                "amount": "12.00",
                "currency": "RUB",
                "dates": {"cash_posted": "2026-08-05"},
                "idempotency_key": "unmatched-outflow",
            },
            {
                "account": harness.account.inner(),
                "type": "transfer",
                "to_account": outside,
                "amount": "34.00",
                "currency": "RUB",
                "dates": {"cash_posted": "2026-08-06"},
                "idempotency_key": "transfer-out",
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

    let actions = body["actions"].as_array().expect("actions");
    let outflow = actions
        .iter()
        .find(|action| action["kind"] == "undecomposed_outflows")
        .unwrap_or_else(|| panic!("a rule-remediable item: {body}"));
    assert_eq!(outflow["state"], "needs_owner_input", "{outflow}");
    assert_eq!(outflow["category"], "recommended", "{outflow}");
    assert_eq!(outflow["required_scope"], "owner", "{outflow}");
    let target = &outflow["target"];
    assert_eq!(target["type"], "operation", "{outflow}");
    assert_eq!(target["operationId"], "create_category_rule", "{outflow}");
    assert_eq!(target["method"], "POST", "{outflow}");
    assert_eq!(target["path"], "/v1/category-rules", "{outflow}");
    assert!(
        target["request"].get("preset").is_none(),
        "a report window is not a rule's validity interval, and no matcher is \
         derivable from this aggregate: {outflow}"
    );
    let missing: Vec<&str> = target["request"]["missing"]
        .as_array()
        .expect("missing inputs")
        .iter()
        .map(|input| input["pointer"].as_str().expect("pointer"))
        .collect();
    assert_eq!(missing, vec!["/matcher", "/category"], "{outflow}");
    for input in target["request"]["missing"]
        .as_array()
        .expect("missing inputs")
    {
        assert_eq!(input["provided_by"], "owner", "{outflow}");
    }

    // The same invariant `/v1/actions` is held to, asserted here because this
    // action reaches the owner through the report and not through that endpoint.
    let spec = serde_json::to_value(&harness.api).expect("OpenAPI JSON");
    let schema_name = target["requestSchema"]
        .as_str()
        .expect("request schema reference")
        .strip_prefix("#/components/schemas/")
        .expect("component schema reference");
    for field in spec["components"]["schemas"][schema_name]["required"]
        .as_array()
        .expect("required request fields")
    {
        let pointer = format!("/{}", field.as_str().expect("field name"));
        assert!(
            missing.contains(&pointer.as_str()),
            "{schema_name} requires {pointer} and the action does not advertise it: {outflow}"
        );
    }

    let transfer = actions
        .iter()
        .find(|action| action["kind"] == "external_transfers_uncategorised")
        .unwrap_or_else(|| panic!("a transfer item: {body}"));
    assert_eq!(transfer["state"], "blocked", "{transfer}");
    assert_eq!(transfer["category"], "informational", "{transfer}");
    assert_eq!(transfer["target"], json!({"type": "none"}), "{transfer}");
    assert!(transfer.get("required_scope").is_none(), "{transfer}");
    assert!(
        transfer["reason"]
            .as_str()
            .expect("reason")
            .contains("category rule cannot decompose"),
        "{transfer}"
    );
    assert_ne!(outflow["id"], transfer["id"], "{body}");
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

/// The flow projection admits no leg from outside the contour, so no figure and
/// no action may name an account the report does not cover. An account with its
/// own unexplained residual, left out of the contour, is what proves that.
///
/// `population` is the one deliberate exception, and the assertion below
/// excludes it: that block exists to name the known accounts the report left
/// out, because a report that could not say so reads as an answer about all of
/// the owner's money (iaam-si5v). Naming them there is the opposite of letting
/// them ride along in the figures.
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
    let mut figures = body.clone();
    let object = figures.as_object_mut().expect("report object");
    object.remove("population");
    // And the register, for the same reason: it is the population's summary,
    // so it names exactly the accounts the population names and nothing else.
    // The prohibition is on an outside account reaching the **figures**.
    object.remove("confidence");
    assert!(
        !figures.to_string().contains(&outside),
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

/// A refusal from an extractor, in the shape the contract promises.
///
/// The response is read as bytes rather than through `call`, because the
/// defect these tests were written for was a `text/plain` body that `call`
/// would quietly turn into `Value::Null`.
async fn refusal(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let (status, headers, bytes) = call_raw(router, request).await;
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned()),
        Some("application/json".to_owned()),
        "a refusal must be JSON, status {status}, body {}",
        String::from_utf8_lossy(&bytes)
    );
    let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "a refusal must deserialise as ApiError: {error}, body {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    assert!(
        body.get("code").and_then(Value::as_str).is_some(),
        "a refusal must carry a machine-readable code: {body}"
    );
    assert!(
        body.get("message").and_then(Value::as_str).is_some(),
        "a refusal must carry a message: {body}"
    );
    (status, body)
}

#[tokio::test]
async fn a_missing_query_parameter_is_refused_in_the_documented_shape() {
    // axum's own `Query` answers `400 text/plain` here. A client parsing
    // errors would then need two encodings, and the operation declares
    // neither the status nor the media type it was actually served.
    let harness = harness();
    let (status, body) = refusal(
        &harness.router,
        get("/v1/reconciliation", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request", "{body}");
    assert_eq!(body["field"], "account", "{body}");

    let declared = harness.api.paths.paths["/v1/reconciliation"]
        .get
        .as_ref()
        .expect("the reconciliation operation");
    assert!(
        declared.responses.responses.contains_key("422"),
        "the status served must be the status declared"
    );
}

#[tokio::test]
async fn a_body_missing_a_required_field_is_refused_in_the_documented_shape() {
    let harness = harness();
    let (status, body) = refusal(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({"institution": "Bank One"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request", "{body}");
    assert_eq!(body["field"], "title", "{body}");

    let declared = harness.api.paths.paths["/v1/accounts"]
        .post
        .as_ref()
        .expect("the account creation operation");
    for status in ["400", "413", "415", "422"] {
        assert!(
            declared.responses.responses.contains_key(status),
            "the operation can serve {status} and must declare it"
        );
    }
}

#[tokio::test]
async fn a_refused_body_does_not_come_back_in_the_refusal() {
    // The value that failed is the caller's, and a rejected body is exactly
    // the kind of thing that carries the owner's data. Only the name of the
    // field and the type expected of it may be returned.
    let harness = harness();
    let (status, body) = refusal(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({"title": {"nested": "not-a-title"}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "title", "{body}");
    assert_eq!(body["expected"], "a string", "{body}");
    assert!(body.get("actual").is_none(), "{body}");
    assert!(!body.to_string().contains("not-a-title"), "{body}");
}

#[tokio::test]
async fn a_syntactically_broken_body_is_refused_in_the_documented_shape() {
    let harness = harness();
    let request = Request::builder()
        .uri("/v1/accounts")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "application/json")
        .body(Body::from("{\"title\": "))
        .expect("request");
    let (status, body) = refusal(&harness.router, request).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request", "{body}");
}

#[tokio::test]
async fn a_body_without_the_json_content_type_is_refused_in_the_documented_shape() {
    // The one refusal that is deliberately not a `422`: nothing was parsed,
    // so there is no field to name. The status differs; the shape does not.
    let harness = harness();
    let request = Request::builder()
        .uri("/v1/accounts")
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header("Content-Type", "text/plain")
        .body(Body::from(json!({"title": "Main"}).to_string()))
        .expect("request");
    let (status, body) = refusal(&harness.router, request).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(body["code"], "unsupported_media_type", "{body}");
}

#[tokio::test]
async fn an_unparsable_path_parameter_is_refused_in_the_documented_shape() {
    let harness = harness();
    let (status, body) = refusal(
        &harness.router,
        get("/v1/instruments/not-a-uuid", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request", "{body}");
    assert_eq!(body["field"], "id", "{body}");
}

/// The correction routes, their schemas, and the refusal an agent token gets.
///
/// The permission is the point of the route existing separately from ingestion:
/// `Scope::may_submit` admits an agent, so an agent that could carry a relation
/// on an ingest row could retract the owner's history. That reasoning is
/// unchanged by `iaam-rond`, which moved the gate on one of the two routes and
/// not the shape of either.
#[tokio::test]
async fn corrections_are_described_and_a_foreign_token_is_refused() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let events = &spec["paths"]["/v1/corrections"]["post"];
    assert!(events.is_object(), "correction route is missing: {spec}");
    assert_eq!(
        events["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/SubmitCorrectionsRequest"
    );
    let imports = &spec["paths"]["/v1/corrections/imports"]["post"];
    assert!(
        imports.is_object(),
        "import correction route is missing: {spec}"
    );
    assert_eq!(
        imports["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/CorrectImportRequest"
    );
    assert_eq!(
        imports["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ImportCorrectionDto"
    );
    for field in ["source", "affected", "already_reversed", "written"] {
        assert!(
            spec["components"]["schemas"]["ImportCorrectionDto"]["properties"][field].is_object(),
            "response schema is missing {field}: {spec}"
        );
    }
    for schema in ["SubmitCorrectionsRequest", "CorrectImportRequest"] {
        assert!(
            spec["components"]["schemas"][schema]["properties"]["acknowledge_retraction"]
                .is_object(),
            "{schema} does not require the acknowledgement: {spec}"
        );
    }
    // The wire word for a relation is the journal's own word: a caller reading
    // the contract must not have to translate between two vocabularies.
    let relations = spec["components"]["schemas"]["CorrectionDto"]["oneOf"]
        .as_array()
        .expect("CorrectionDto is a tagged union");
    let tags: std::collections::BTreeSet<String> = relations
        .iter()
        .filter_map(|variant| {
            variant["properties"]["relation"]["enum"][0]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(
        tags,
        ["replacement".to_owned(), "reversal".to_owned()]
            .into_iter()
            .collect::<std::collections::BTreeSet<String>>(),
        "unexpected relation tags: {relations:?}"
    );

    // Reversing a named event of the owner's is a judgement about his history,
    // and nothing about the caller's own conduct bounds it: both non-owner
    // scopes are refused at the door.
    for token in [&harness.agent_token, &harness.readonly_token] {
        let (status, body) = call(
            &harness.router,
            post(
                "/v1/corrections",
                token,
                &json!({"acknowledge_retraction": true}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["code"], "forbidden", "{body}");
    }

    // Retracting a whole import is not the same act (iaam-rond): an agent may
    // take back its own declaration, so the route keeps only the floor and the
    // scenario decides the rest against the journal. Read-only is still refused
    // here; what an agent may and may not reach is asserted where the journal
    // exists to decide it.
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.readonly_token,
            &json!({"acknowledge_retraction": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden", "{body}");
}

/// Seed one deposit and return the event identifier the server minted for it.
async fn seed_correctable_deposit(
    harness: &Harness,
    channel: &str,
    key: &str,
    amount: &str,
) -> Uuid {
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "correction-contract",
                "source": { "account": harness.account.inner(), "channel": channel },
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": amount,
                    "currency": "RUB",
                    "dates": { "cash_posted": "2026-08-05" },
                    "idempotency_key": key
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "provisional", "{verdicts}");
    verdicts[0]["event_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("recorded event identifier")
}

#[tokio::test]
async fn a_correction_is_refused_until_the_owner_acknowledges_the_retraction() {
    let harness = harness();
    let event = seed_correctable_deposit(&harness, "file", "correction-ack", "100.00").await;

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections",
            &harness.owner_token,
            &json!({ "corrections": [{ "relation": "reversal", "target": event }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "acknowledge_retraction");

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.owner_token,
            &json!({ "source": { "account": harness.account.inner(), "channel": "file" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "acknowledge_retraction");
}

#[tokio::test]
async fn a_correction_naming_an_event_the_journal_does_not_hold_is_refused() {
    let harness = harness();
    let missing = Uuid::new_v4();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "corrections": [{ "relation": "reversal", "target": missing }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "corrections[0].target");
    assert_eq!(body["actual"], missing.to_string());
}

/// A second replacement of one event is refused, and refused **before** anything
/// is written: the journal is append-only, and a conflicting replacement in it
/// would fail every later read rather than only the request that added it.
#[tokio::test]
async fn a_conflicting_replacement_is_refused_and_leaves_the_journal_untouched() {
    let (harness, path) = harness_on_disk();
    let event = seed_correctable_deposit(&harness, "file", "correction-conflict", "100.00").await;

    let replacement = json!({
        "acknowledge_retraction": true,
        "corrections": [{
            "relation": "replacement",
            "target": event,
            "operation": {
                "account": harness.account.inner(),
                "type": "deposit",
                "amount": "120.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2026-08-05" }
            }
        }]
    });
    let (status, body) = call(
        &harness.router,
        post("/v1/corrections", &harness.owner_token, &replacement),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["verdict"], "provisional", "{body}");

    let before = journal_of(&path, harness.owner).len();
    let (status, body) = call(
        &harness.router,
        post("/v1/corrections", &harness.owner_token, &replacement),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "corrections[0].target");
    assert_eq!(body["actual"], event.to_string());
    assert_eq!(
        journal_of(&path, harness.owner).len(),
        before,
        "a refused correction must write nothing"
    );

    drop(harness);
    let _ = std::fs::remove_file(path);
}

/// Reversing an event that is not a fact changes nothing, and saying so is
/// better than writing a correction whose effect the owner cannot observe.
#[tokio::test]
async fn reversing_a_reversal_is_refused() {
    let harness = harness();
    let event = seed_correctable_deposit(&harness, "file", "correction-double", "100.00").await;
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "corrections": [{ "relation": "reversal", "target": event }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reversal = body[0]["event_id"].as_str().expect("reversal identifier");

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "corrections": [{ "relation": "reversal", "target": reversal }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "corrections[0].target");
}

/// A repeated import correction reports what an earlier run retracted and adds
/// no second reversal of the same event.
#[tokio::test]
async fn correcting_one_import_twice_writes_nothing_the_second_time() {
    let harness = harness();
    seed_correctable_deposit(&harness, "file", "correction-repeat-a", "100.00").await;
    seed_correctable_deposit(&harness, "file", "correction-repeat-b", "200.00").await;

    let request = json!({
        "acknowledge_retraction": true,
        "source": { "account": harness.account.inner(), "channel": "file" }
    });
    let (status, first) = call(
        &harness.router,
        post("/v1/corrections/imports", &harness.owner_token, &request),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["affected"], 2);
    assert_eq!(first["already_reversed"], 0);
    assert_eq!(first["written"], 2);

    let (status, second) = call(
        &harness.router,
        post("/v1/corrections/imports", &harness.owner_token, &request),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(
        second["source"], first["source"],
        "the same declared source"
    );
    assert_eq!(
        second["affected"], 0,
        "nothing effective is left to retract"
    );
    assert_eq!(second["already_reversed"], 2);
    assert_eq!(second["written"], 0);
}

fn journal_of(path: &std::path::Path, owner: OwnerId) -> Vec<iaam_core::event::Event> {
    SqliteStore::open(path)
        .expect("second connection")
        .load_events(owner)
        .expect("owner journal")
}

/// The reported case, end to end.
///
/// A month imported against the wrong account map, corrected in one request;
/// entries that were already right survive untouched; nothing is deleted, and
/// each retracted event gains a reversal fact referencing it. Then one of the
/// retracted rows is re-stated against the account it belonged to, and the
/// reports move to match.
#[tokio::test]
async fn an_import_against_the_wrong_account_map_is_corrected_end_to_end() {
    let (harness, path) = harness_on_disk();
    let wrong = harness.account.inner();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let right = created["id"].as_str().expect("account identifier");
    let right = Uuid::parse_str(right).expect("account uuid");

    let contour_of =
        |accounts: Vec<Uuid>, title: &str| json!({ "title": title, "accounts": accounts });
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &contour_of(vec![wrong], "Mis-mapped"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mismapped = body["contour"].as_str().expect("contour").to_owned();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &contour_of(vec![right], "Intended"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let intended = body["contour"].as_str().expect("contour").to_owned();

    // The month, imported against the wrong account map: every row landed on
    // the account the map named instead of the one the rows belong to.
    let (status, mismapped_rows) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "August statement",
                "source": { "account": wrong, "channel": "file" },
                "operations": [
                    {
                        "account": wrong,
                        "type": "deposit",
                        "amount": "3000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2026-08-05" },
                        "idempotency_key": "august-row-1"
                    },
                    {
                        "account": wrong,
                        "type": "deposit",
                        "amount": "500.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2026-08-06" },
                        "idempotency_key": "august-row-2"
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{mismapped_rows}");
    let first_row = mismapped_rows[0]["event_id"]
        .as_str()
        .expect("recorded event")
        .to_owned();

    // Entries that were already right. They must survive the correction.
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "source": { "account": right, "channel": "manual" },
                "operations": [{
                    "account": right,
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2026-08-07" },
                    "idempotency_key": "august-correct-row"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let came_in = |contour: &str| {
        let router = harness.router.clone();
        let token = harness.owner_token.clone();
        let path = format!("/v1/reports/flow?contour={contour}&from=2026-08-01&to=2026-08-31");
        async move {
            let (status, body) = call(&router, get(&path, Some(&token))).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            // A contour with nothing left in it reports no currency at all, and
            // that is the same statement as an inflow of zero.
            body["currencies"]
                .as_array()
                .expect("currencies")
                .iter()
                .find(|entry| entry["currency"] == "RUB")
                .and_then(|entry| entry["came_in"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "0.00".to_owned())
        }
    };

    assert_eq!(came_in(&mismapped).await, "3500.00");
    assert_eq!(came_in(&intended).await, "1000.00");

    let before = journal_of(&path, harness.owner);
    assert_eq!(before.len(), 3, "three imported facts");

    let (status, corrected) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "source": { "account": wrong, "channel": "file" }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{corrected}");
    assert_eq!(corrected["affected"], 2);
    assert_eq!(corrected["written"], 2);

    assert_eq!(
        came_in(&mismapped).await,
        "0.00",
        "the mis-mapped import no longer counts"
    );
    assert_eq!(
        came_in(&intended).await,
        "1000.00",
        "entries that were already right survive the correction"
    );

    // Nothing deleted, nothing mutated: the originals are still there, and each
    // has gained a reversal fact that references it.
    let after = journal_of(&path, harness.owner);
    assert_eq!(after.len(), before.len() + 2, "two reversal facts appended");
    for original in &before {
        let stored = after
            .iter()
            .find(|event| event.id == original.id)
            .expect("the original fact is still in the journal");
        assert_eq!(stored, original, "an original fact was mutated");
    }
    let reversed: std::collections::BTreeSet<Uuid> = after
        .iter()
        .filter_map(|event| match event.relation {
            iaam_core::event::Relation::Reversal { target } => Some(target.inner()),
            _ => None,
        })
        .collect();
    let mismapped_ids: std::collections::BTreeSet<Uuid> = before
        .iter()
        .filter(|event| event.account.inner() == wrong)
        .map(|event| event.id.inner())
        .collect();
    assert_eq!(reversed, mismapped_ids, "one reversal per mis-mapped fact");

    // The retraction is only half the repair: the row still belongs somewhere.
    // Re-stating it as a replacement moves it to the account it was always for.
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "corrections": [{
                    "relation": "replacement",
                    "target": first_row,
                    "operation": {
                        "account": right,
                        "type": "deposit",
                        "amount": "3000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2026-08-05" },
                        "idempotency_key": "august-row-1-corrected"
                    }
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["verdict"], "provisional", "{body}");

    assert_eq!(came_in(&mismapped).await, "0.00");
    assert_eq!(
        came_in(&intended).await,
        "4000.00",
        "the re-stated row now counts where it belongs"
    );

    drop(harness);
    let _ = std::fs::remove_file(path);
}

/// Post one deposit and return the identifier of the event it became.
///
/// Amounts and titles here are invented — `Main`, `Savings`, `Shop One` — and
/// no line of this file is trimmed from a real export (CLAUDE.md).
async fn ingest_deposit(
    harness: &Harness,
    account: AccountId,
    amount: &str,
    posted: &str,
    idempotency_key: &str,
    declared_channel: Option<&str>,
) -> Uuid {
    let mut body = json!({
        "source_label": "contract test",
        "operations": [{
            "account": account.inner(),
            "type": "deposit",
            "amount": amount,
            "currency": "RUB",
            "dates": { "cash_posted": posted },
            "idempotency_key": idempotency_key,
            "description": "Shop One"
        }]
    });
    if let Some(channel) = declared_channel {
        body["source"] = json!({ "account": account.inner(), "channel": channel });
    }
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "provisional", "{verdicts}");
    verdicts[0]["event_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("the ingest verdict names the event it recorded")
}
async fn create_account(harness: &Harness, title: &str) -> AccountId {
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": title }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    AccountId(
        body["id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .expect("the created account names itself"),
    )
}
#[tokio::test]
async fn an_ingested_operation_can_be_read_back_by_its_idempotency_key() {
    // The defect this route exists for: 177 rows went in with verdict
    // `provisional` and then could not be looked at. An agent forbidden its own
    // arithmetic has nothing to quote about a single row unless it can read it.
    let harness = harness();
    let event = ingest_deposit(
        &harness,
        harness.account,
        "1000.00",
        "2026-03-01",
        "key-one",
        None,
    )
    .await;
    let (status, page) = call(
        &harness.router,
        get(
            "/v1/journal/events?idempotency_key=key-one",
            Some(&harness.agent_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let rows = page["rows"].as_array().expect("journal rows");
    assert_eq!(rows.len(), 1, "an idempotency key addresses one event");
    let row = &rows[0];
    assert_eq!(row["event"], event.to_string());
    assert_eq!(row["account"], harness.account.inner().to_string());
    assert_eq!(row["idempotency_key"], "key-one");
    assert_eq!(row["kind"], "cash_in");
    assert_eq!(row["effective_date"], "2026-03-01");
    assert_eq!(row["dates"]["cash_posted"], "2026-03-01");
    assert_eq!(row["description"], "Shop One");
    assert_eq!(row["relation"]["kind"], "none");
    assert!(row["source"].is_string(), "the row names its source: {row}");
    // The amount is returned leg by leg, exactly as recorded: the route sums
    // nothing, so there is a number the agent may quote verbatim (§13).
    let legs = row["legs"].as_array().expect("legs");
    assert_eq!(legs.len(), 1);
    assert_eq!(legs[0]["kind"], "cash");
    assert_eq!(legs[0]["amount"], "1000.00");
    assert_eq!(legs[0]["currency"], "RUB");
}
#[tokio::test]
async fn an_idempotency_key_that_addresses_nothing_is_a_clean_not_found() {
    // An empty page would say "the journal holds no such row" in the same
    // breath as "you narrowed to nothing", and the caller cannot tell which.
    let harness = harness();
    let (status, body) = call(
        &harness.router,
        get(
            "/v1/journal/events?idempotency_key=never-submitted",
            Some(&harness.agent_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}
#[tokio::test]
async fn the_journal_narrows_by_account_and_by_date_range() {
    let harness = harness();
    let savings = create_account(&harness, "Savings").await;
    ingest_deposit(
        &harness,
        harness.account,
        "1000.00",
        "2026-03-01",
        "main-march",
        None,
    )
    .await;
    ingest_deposit(
        &harness,
        harness.account,
        "2000.00",
        "2026-04-01",
        "main-april",
        None,
    )
    .await;
    ingest_deposit(
        &harness,
        savings,
        "3000.00",
        "2026-03-01",
        "savings-march",
        None,
    )
    .await;
    let account_only = format!("/v1/journal/events?account={}", harness.account.inner());
    let (status, page) = call(
        &harness.router,
        get(&account_only, Some(&harness.agent_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["rows"].as_array().expect("rows").len(), 2);
    let with_range = format!(
        "/v1/journal/events?account={}&from=2026-03-01&to=2026-03-31",
        harness.account.inner()
    );
    let (status, page) = call(
        &harness.router,
        get(&with_range, Some(&harness.agent_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let rows = page["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "the range excludes April: {page}");
    assert_eq!(rows[0]["idempotency_key"], "main-march");
}
#[tokio::test]
async fn the_journal_narrows_by_the_source_the_caller_declared() {
    // The identity of a declared source is derived, never handed out, so the
    // only way to ask "what did that import put in" is to name the account and
    // channel again. If this read derived it differently from ingest, the
    // answer would be empty and look like an import that never landed.
    let harness = harness();
    ingest_deposit(
        &harness,
        harness.account,
        "1000.00",
        "2026-03-01",
        "from-file",
        Some("file"),
    )
    .await;
    ingest_deposit(
        &harness,
        harness.account,
        "2000.00",
        "2026-03-02",
        "from-paste",
        Some("paste"),
    )
    .await;
    let path = format!(
        "/v1/journal/events?source_account={}&source_channel=file",
        harness.account.inner()
    );
    let (status, page) = call(&harness.router, get(&path, Some(&harness.agent_token))).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let rows = page["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "a channel is part of the source: {page}");
    assert_eq!(rows[0]["idempotency_key"], "from-file");
    // Half a declared source is a different question, not a wider one.
    let half = format!(
        "/v1/journal/events?source_account={}",
        harness.account.inner()
    );
    let (status, body) = call(&harness.router, get(&half, Some(&harness.agent_token))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["field"], "source_channel");
}
#[tokio::test]
async fn the_journal_lists_without_any_narrowing() {
    // Reading the list is not a privilege beyond reading the aggregates: every
    // balance, flow and return this API serves is computed from these very rows.
    let harness = harness();
    let savings = create_account(&harness, "Savings").await;
    ingest_deposit(
        &harness,
        harness.account,
        "1000.00",
        "2026-03-01",
        "one",
        None,
    )
    .await;
    ingest_deposit(&harness, savings, "2000.00", "2026-03-02", "two", None).await;
    for token in [
        &harness.owner_token,
        &harness.agent_token,
        &harness.readonly_token,
    ] {
        let (status, page) = call(&harness.router, get("/v1/journal/events", Some(token))).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let rows = page["rows"].as_array().expect("rows");
        assert_eq!(rows.len(), 2, "an unfiltered listing returns both: {page}");
        assert!(
            page.get("next").is_none(),
            "a last page names no position to resume from: {page}"
        );
    }
    let (status, _) = call(&harness.router, get("/v1/journal/events", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the journal read is open");
}
#[tokio::test]
async fn paging_the_journal_neither_skips_nor_repeats_a_row() {
    // Two events on one day is the case an offset gets wrong and a date-only
    // cursor gets wrong in the other direction: the second row of the day would
    // either be served twice or never.
    let harness = harness();
    for (index, key) in ["one", "two", "three", "four"].iter().enumerate() {
        let day = format!("2026-03-{:02}", index / 2 + 1);
        ingest_deposit(&harness, harness.account, "1000.00", &day, key, None).await;
    }
    let mut seen: Vec<String> = Vec::new();
    let mut path = "/v1/journal/events?limit=1".to_owned();
    for _ in 0..10 {
        let (status, page) = call(&harness.router, get(&path, Some(&harness.agent_token))).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let rows = page["rows"].as_array().expect("rows");
        assert!(rows.len() <= 1, "a page of one returned more: {page}");
        for row in rows {
            seen.push(
                row["idempotency_key"]
                    .as_str()
                    .expect("each row names its key")
                    .to_owned(),
            );
        }
        let Some(next) = page["next"].as_str() else {
            break;
        };
        path = format!("/v1/journal/events?limit=1&after={next}");
    }
    assert_eq!(
        seen,
        vec![
            "one".to_owned(),
            "two".to_owned(),
            "three".to_owned(),
            "four".to_owned()
        ],
        "paging must walk the journal once, in order, with nothing dropped"
    );
}
#[tokio::test]
async fn a_page_size_outside_the_permitted_range_is_refused_by_name() {
    let harness = harness();
    for limit in ["0", "201"] {
        let path = format!("/v1/journal/events?limit={limit}");
        let (status, body) = call(&harness.router, get(&path, Some(&harness.agent_token))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "invalid_request");
        assert_eq!(body["field"], "limit");
    }
}
#[tokio::test]
async fn one_owners_journal_is_invisible_to_another() {
    // A read scoped by a filter and not by the owner would let anyone holding a
    // token read every journal on the instance (§14).
    let mine = harness();
    let theirs = harness();
    ingest_deposit(&mine, mine.account, "1000.00", "2026-03-01", "mine", None).await;
    let (status, page) = call(
        &theirs.router,
        get("/v1/journal/events", Some(&theirs.agent_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page["rows"].as_array().expect("rows").is_empty(),
        "another owner's journal reached this token: {page}"
    );
}

/// Both routes read one journal, so they must render one status for one account.
///
/// The balances answer moved to the excepted ledger when the perimeter was wired;
/// `/v1/reconciliation` was still building the plain one, so a financing account
/// would have read `excepted` in one place and `discrepant` in the other from the
/// same facts. The existing test that pins the two together has no financing in its
/// fixture and could not have caught it.
#[tokio::test]
async fn the_two_routes_agree_about_a_financing_account() {
    let harness = harness();
    let account = harness.account.inner().to_string();

    let contour = json!({ "title": "Financing", "accounts": [account] });
    let (status, contour_response) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &contour),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("contour id");

    // A deficit with the credit indicator beside it: the perimeter classifies this
    // as financing it does not reconstruct, which is what raises the exception.
    let operations = json!({
        "source_label": "manual entry",
        "operations": [
            {
                "account": account, "type": "deposit", "amount": "1000.00",
                "currency": "RUB", "dates": { "cash_posted": "2026-08-05" },
                "idempotency_key": "agree-deposit"
            },
            {
                "account": account, "type": "withdrawal", "amount": "2500.00",
                "currency": "RUB", "dates": { "cash_posted": "2026-08-06" },
                "idempotency_key": "agree-withdrawal"
            },
            {
                "account": account, "type": "fee", "amount": "40.00",
                "currency": "RUB", "origin": "margin_interest",
                "dates": { "cash_posted": "2026-08-07" },
                "idempotency_key": "agree-margin-interest"
            }
        ]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    // An assertion the journal cannot match, so there is a status to compare at all.
    let (status, recorded) = call(
        &harness.router,
        post(
            "/v1/reconciliation/balance",
            &harness.owner_token,
            &json!({
                "account": account,
                "from": "2026-08-01",
                "to": "2026-08-31",
                "at": "closing",
                "cash": { "currency": "RUB", "amount": "9999.00" }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");

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
            &format!("/v1/reconciliation?account={account}&from=2026-08-01&to=2026-08-31"),
            Some(&harness.readonly_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reconciliation}");

    let from_balances = balances["accounts"][0]["reconciliation"]
        .as_array()
        .expect("statuses on the balances answer");
    let from_route = reconciliation["statuses"]
        .as_array()
        .expect("statuses on the reconciliation route");
    assert!(
        !from_balances.is_empty() && !from_route.is_empty(),
        "the fixture must produce a status to compare: {balances} / {reconciliation}"
    );
    // The whole status object, not one field of it: the two routes read one
    // journal for one account and one period, so every dimension, outcome and
    // exception in it has to match. Comparing a single field is how a
    // divergence like this one stays invisible.
    assert_eq!(
        from_balances, from_route,
        "one account, one journal, two answers: {balances} / {reconciliation}"
    );
}

#[tokio::test]
async fn every_documented_parameter_sits_where_the_route_reads_it() {
    // `IntoParams` defaults a struct's parameters to `in: path` whenever the
    // operation has any path parameter at all, and says nothing when it has
    // none. Nine structs read by `ApiQuery` were therefore published as path
    // parameters — 42 of them, every filter on every report, market,
    // reconciliation and journal route — so a client generated from this
    // document could not send a single one of them.
    //
    // The check is structural rather than a list of names: a parameter belongs
    // in `path` exactly when the path template names it, and in `query`
    // otherwise. That way a route added later is covered by the guard on the
    // day it is written, which a list of known names would not do.
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let mut wrong = Vec::new();
    for (path, item) in spec["paths"].as_object().expect("OpenAPI paths") {
        for (method, operation) in item.as_object().expect("path item") {
            let Some(parameters) = operation["parameters"].as_array() else {
                continue;
            };
            for parameter in parameters {
                let name = parameter["name"].as_str().expect("parameter name");
                let location = parameter["in"].as_str().expect("parameter location");
                let templated = path.contains(&format!("{{{name}}}"));
                let expected = if templated { "path" } else { "query" };
                if location != expected {
                    wrong.push(format!(
                        "{method} {path}: `{name}` is documented in `{location}`, \
                         but the path template {} it",
                        if templated { "names" } else { "does not name" }
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a parameter is documented somewhere the route does not read it:\n{}",
        wrong.join("\n")
    );
}

/// Two imports through one account and one channel are two imports.
///
/// The ordinary case: two months of the same account, exported the same way.
/// Retracting the second must leave the first in force — a route published as
/// «retract a whole import» that swept both would be a destructive operation
/// keyed more coarsely than its own description.
#[tokio::test]
async fn two_labelled_imports_through_one_channel_retract_separately() {
    let (harness, path) = harness_on_disk();
    let account = harness.account.inner();

    let import = |label: &str, key: &str, amount: &str, day: &str| {
        json!({
            "source": { "account": account, "channel": "file", "label": label },
            "operations": [{
                "account": account,
                "type": "deposit",
                "amount": amount,
                "currency": "RUB",
                "dates": { "cash_posted": day },
                "idempotency_key": key
            }]
        })
    };

    let (status, first) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &import("statement-january", "import-jan-1", "100.00", "2026-01-05"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let january = first[0]["event_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("the January fact");

    let (status, second) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &import("statement-february", "import-feb-1", "200.00", "2026-02-05"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let february = second[0]["event_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("the February fact");

    let (status, corrected) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "source": {
                    "account": account,
                    "channel": "file",
                    "label": "statement-february"
                }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{corrected}");
    assert_eq!(
        corrected["affected"], 1,
        "only the February import was named: {corrected}"
    );
    assert_eq!(corrected["written"], 1, "{corrected}");

    let reversed: std::collections::BTreeSet<Uuid> = journal_of(&path, harness.owner)
        .iter()
        .filter_map(|event| match event.relation {
            iaam_core::event::Relation::Reversal { target } => Some(target.inner()),
            _ => None,
        })
        .collect();
    assert!(
        reversed.contains(&february),
        "the named import was not retracted"
    );
    assert!(
        !reversed.contains(&january),
        "retracting one import retracted another one made through the same channel"
    );

    drop(harness);
    let _ = std::fs::remove_file(path);
}

/// A batch declares the account its rows belong to, and the rows must agree.
///
/// Otherwise rows for one account are recorded against it while carrying the
/// import identity of another, and retracting that import reaches into an
/// account the caller never named.
#[tokio::test]
async fn an_operation_disagreeing_with_the_declared_account_is_refused() {
    let harness = harness();
    let declared = harness.account.inner();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let other = created["id"]
        .as_str()
        .expect("account identifier")
        .to_owned();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                    "source": { "account": declared, "channel": "file", "label": "statement" },
                "operations": [{
                    "account": other,
                    "type": "deposit",
                    "amount": "100.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2026-01-05" },
                    "idempotency_key": "disagreeing-row"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request", "{body}");
    assert_eq!(body["field"], "operations[0].account", "{body}");
    assert_eq!(body["expected"], declared.to_string(), "{body}");
    assert_eq!(body["actual"], other, "{body}");
}

/// The route says what it retracts, in the document an external agent reads.
///
/// The published description is the only account of the key an agent has: one
/// that promised «a whole import» while retracting every import of an account
/// and channel is how a destructive operation gets called on the wrong rows.
#[tokio::test]
async fn the_import_correction_publishes_the_key_it_actually_retracts_on() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let described = spec["paths"]["/v1/corrections/imports"]["post"]["description"]
        .as_str()
        .expect("the import correction route carries a description")
        .to_owned();
    for word in ["label", "without a label"] {
        assert!(
            described.contains(word),
            "the description does not say what it retracts on ({word}): {described}"
        );
    }

    let declared = &spec["components"]["schemas"]["DeclaredSourceDto"]["properties"];
    for field in ["account", "channel", "label"] {
        assert!(
            declared[field].is_object(),
            "a declared source no longer publishes {field}: {spec}"
        );
    }
    // A field accepted and ignored is worse than one that is absent: the label
    // that names an import now lives inside the declaration, and the old
    // free-text one, which reached no production path, is gone.
    assert!(
        spec["components"]["schemas"]["SubmitOperationsRequest"]["properties"]["source_label"]
            .is_null(),
        "the ignored source_label is still published: {spec}"
    );
}

/// A rule written on what the source printed reaches history, and says so.
///
/// Three things at once, because they are one behaviour: the rebuilt
/// classification subject carries the description and the source's own word
/// (without them two thirds of the matcher are dead on the recompute path), and
/// the plan the rule change computes is returned instead of being discarded.
#[tokio::test]
async fn a_classification_rule_reports_the_history_it_would_correct() {
    let harness = harness();
    let operations = json!({
        "source_label": "test",
        "operations": [{
            "account": harness.account.inner(),
            "type": "withdrawal",
            "amount": "1200.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2026-08-12" },
            "source_category": "Card operation",
            "description": "Shop One",
            "idempotency_key": "reclass-withdrawal"
        }]
    });
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, &operations),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "provisional", "{verdicts}");
    let event = verdicts[0]["event_id"]
        .as_str()
        .expect("the recorded event")
        .to_owned();

    // A rule on the description the source printed. Matching it requires the
    // subject rebuilt from the event to carry that description.
    let by_description = json!({
        "matcher": { "description_contains": "shop one" },
        "outcome": { "kind": "fee", "origin": "account_maintenance" },
    });
    let (status, first) = call(
        &harness.router,
        post(
            "/v1/classification-rules",
            &harness.owner_token,
            &by_description,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["plan"]["applied"], false, "{first}");
    let corrections = first["plan"]["corrections"]
        .as_array()
        .expect("a plan, not silence");
    assert_eq!(corrections.len(), 1, "{first}");
    assert_eq!(corrections[0]["event"], event, "{first}");
    assert_eq!(corrections[0]["was"]["kind"], "external_flow", "{first}");
    assert_eq!(corrections[0]["becomes"]["kind"], "fee", "{first}");
    assert_eq!(
        corrections[0]["becomes"]["origin"], "account_maintenance",
        "{first}"
    );

    // A rule on the word the source used for the row — not on `cash_out`,
    // which is the classification this rule exists to revise.
    let by_source_kind = json!({
        "matcher": { "kind": "Card operation" },
        "outcome": { "kind": "income" },
    });
    let (status, second) = call(
        &harness.router,
        post(
            "/v1/classification-rules",
            &harness.owner_token,
            &by_source_kind,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let corrections = second["plan"]["corrections"]
        .as_array()
        .expect("a plan, not silence");
    assert_eq!(corrections.len(), 1, "{second}");
    assert_eq!(corrections[0]["event"], event, "{second}");
    // The later decision wins: the rule naming the source's own word is the
    // owner's most recent answer about this row.
    assert_eq!(corrections[0]["becomes"]["kind"], "income", "{second}");
    let newest = second["id"].as_str().expect("identifier").to_owned();

    // Retiring is symmetric: it recomputes and answers with the plan too,
    // here the one the surviving earlier rule produces.
    let (status, plan) = call(
        &harness.router,
        delete(
            &format!("/v1/classification-rules/{newest}"),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["applied"], false, "{plan}");
    let corrections = plan["corrections"].as_array().expect("a plan, not silence");
    assert_eq!(corrections.len(), 1, "{plan}");
    assert_eq!(corrections[0]["event"], event, "{plan}");
    assert_eq!(corrections[0]["becomes"]["kind"], "fee", "{plan}");
}

/// What the listing prints is what the create route accepts, unchanged.
///
/// The one thing an LLM client cannot infer is a write shape that differs from
/// the read shape, because inference is copying the shape it just saw. So the
/// test sends nothing of its own on the second write: it takes `matcher` and
/// `outcome` out of the response verbatim and posts them back, and the rule that
/// comes out has to be the rule that went in.
///
/// A rule created by answering an import question is round-tripped too, and it
/// is the harder half: nothing about it was composed by a client, so it is the
/// shape the server itself writes that has to be readable as a request.
#[tokio::test]
async fn a_classification_rule_round_trips_through_the_shape_it_is_read_in() {
    let harness = harness();
    let savings = account_with(&harness, &json!({ "title": "Savings" })).await;

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/classification-rules",
            &harness.owner_token,
            &json!({
                "matcher": { "counterparty_account": "Savings", "kind": "INNER" },
                "outcome": { "kind": "internal_transfer", "to": savings },
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, history) = call(
        &harness.router,
        get("/v1/classification-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    let read = &history.as_array().expect("history")[0];
    assert_eq!(
        read["matcher"],
        json!({ "counterparty_account": "Savings", "kind": "INNER" }),
        "the matcher is read as the object it was written as: {read}"
    );
    assert_eq!(
        read["outcome"],
        json!({ "kind": "internal_transfer", "to": savings }),
        "the outcome is read as the object it was written as: {read}"
    );

    // Nothing composed here: exactly what was read, sent back.
    let (status, again) = call(
        &harness.router,
        post(
            "/v1/classification-rules",
            &harness.owner_token,
            &json!({ "matcher": read["matcher"], "outcome": read["outcome"] }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "what the listing prints must be accepted verbatim: {again}"
    );
    assert_eq!(again["matcher"], read["matcher"], "{again}");
    assert_eq!(again["outcome"], read["outcome"], "{again}");

    // The rule the server writes for itself, when the owner answers a question,
    // has to survive the same journey.
    let account = harness.account.inner();
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "march" },
                "operations": [unresolved_row(account, "round-trip-one")],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    let session = verdicts[0]["session_id"].as_str().expect("session");
    let question = verdicts[0]["question_id"].as_str().expect("question");
    let (status, answered) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "sent_to_own_account", "account": savings }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    let learned = answered["rule"]
        .as_str()
        .expect("the rule the answer wrote");

    let (status, history) = call(
        &harness.router,
        get("/v1/classification-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    let read = history
        .as_array()
        .expect("history")
        .iter()
        .find(|rule| rule["id"] == json!(learned))
        .expect("the rule the answer wrote is in the listing");
    let (status, again) = call(
        &harness.router,
        post(
            "/v1/classification-rules",
            &harness.owner_token,
            &json!({ "matcher": read["matcher"], "outcome": read["outcome"] }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a rule the server wrote must be readable as a request: {again}"
    );
    assert_eq!(again["matcher"], read["matcher"], "{again}");
    assert_eq!(again["outcome"], read["outcome"], "{again}");
}

/// A rule matching nothing says so, rather than saying nothing.
#[tokio::test]
async fn a_classification_rule_that_matches_nothing_returns_an_empty_plan() {
    let harness = harness();
    let rule = json!({
        "matcher": { "description_contains": "nothing here" },
        "outcome": { "kind": "income" },
    });
    let (status, created) = call(
        &harness.router,
        post("/v1/classification-rules", &harness.owner_token, &rule),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["plan"]["applied"], false, "{created}");
    assert!(
        created["plan"]["corrections"]
            .as_array()
            .expect("an empty plan is still a plan")
            .is_empty(),
        "{created}"
    );
}

/// Two contours exist and one account belongs to neither.
///
/// The queue was silent about this for the whole life of an instance after its
/// first contour: `create_first_contour` fires once, and nothing afterwards said
/// anything about membership. An account created later — a second bank —
/// imported every row correctly and was absent from every report.
#[tokio::test]
async fn an_account_in_no_contour_is_named_by_the_queue() {
    let harness = harness();
    let (status, second) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let second = second["id"].as_str().expect("account id").to_owned();

    let (status, orphan) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Second Bank Current" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{orphan}");
    let orphan = orphan["id"].as_str().expect("account id").to_owned();

    for (title, member) in [
        ("Household", harness.account.inner().to_string()),
        ("Reserve", second),
    ] {
        let (status, created) = call(
            &harness.router,
            post(
                "/v1/contours",
                &harness.owner_token,
                &json!({ "title": title, "accounts": [member] }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
    }

    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");

    let named: Vec<&Value> = actions
        .as_array()
        .expect("action items")
        .iter()
        .filter(|item| item["kind"] == "account_scope_undecided")
        .collect();
    assert_eq!(named.len(), 1, "{actions}");
    let item = named[0];
    assert_eq!(
        item["subject"],
        json!({
            "type": "account",
            "id": orphan,
            "title": "Second Bank Current",
        }),
        "the account is named in a typed field, not only in prose: {item}"
    );
    assert_eq!(item["category"], "required_for_goal", "{item}");
    // An account in no contour is outside the covered population, so it is
    // absent from the three contour-scoped reports. Not reconciliation: that
    // route takes an account and resolves no contour at all.
    assert_eq!(
        item["goals"],
        json!(["asset_snapshot", "money_flow", "returns"]),
        "{item}"
    );
    assert_eq!(item["state"], "needs_owner_input", "{item}");
    assert_eq!(item["required_scope"], "owner", "{item}");
    // Two ways out, so the target is a set of options and not one of them.
    assert_eq!(item["target"]["type"], "options", "{item}");
    let membership = published_option(item, "add_contour_version")
        .expect("the item must offer membership of an existing contour");
    assert_eq!(membership["method"], "POST", "{item}");
    assert_eq!(
        membership["path"], "/v1/contours/{contour}/versions",
        "the creating route answered this item with a second perimeter: {item}"
    );
    let candidates = membership["request"]["missing"]
        .as_array()
        .expect("missing inputs")
        .iter()
        .find(|missing| missing["pointer"] == "/accounts")
        .expect("account selection input");
    assert!(
        candidates["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .any(|candidate| candidate["id"] == orphan),
        "{item}"
    );

    // The other way out, published rather than left in the sentence. Until it
    // was, a client that reads `target` as the contract — which is what it is
    // for — could only ever put the account inside a contour, and reaching this
    // route meant reading prose and searching the specification for it.
    let exclusion = published_option(item, "record_account_scope")
        .expect("the item must offer ruling the account outside the perimeter");
    assert_eq!(exclusion["method"], "POST", "{item}");
    assert_eq!(exclusion["path"], "/v1/accounts/{id}/scope", "{item}");
    assert_eq!(
        exclusion["request"]["preset"]["disposition"], "outside",
        "an option that leaves the disposition to be guessed publishes a route, not a resolution: {item}"
    );
    let reason = exclusion["request"]["missing"]
        .as_array()
        .expect("missing inputs")
        .iter()
        .find(|missing| missing["pointer"] == "/reason")
        .expect("the reason is a required field of this option");
    assert_eq!(reason["provided_by"], "owner", "{item}");
}

/// One published resolution of an action, by the operation it names.
fn published_option<'a>(item: &'a Value, operation_id: &str) -> Option<&'a Value> {
    item["target"]["options"]
        .as_array()?
        .iter()
        .find(|option| option["operationId"] == operation_id)
}

/// Every queue item about an account says what the owner calls it.
///
/// The queue is the surface the naming rule (`docs/api/conventions.md` §3) was
/// written for. A client that opened an instance after an import got one
/// `record_owner_balance` item per account, each naming a UUID and nothing
/// else, and could not tell which bank any of them was about without a second
/// request to `GET /v1/accounts` and a join it was left to perform. Asked which
/// account, the owner cannot answer from a bare identifier, and neither can the
/// agent asking on his behalf.
///
/// `institution` travels with the title for the case the title alone cannot
/// settle: two accounts he calls `Savings` at two banks are one word apart in a
/// list and are not the same question.
#[tokio::test]
async fn every_queue_item_about_an_account_names_the_account() {
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Northline" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let savings = created["id"].as_str().expect("account id").to_owned();

    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");
    let items = actions.as_array().expect("action items");
    assert!(!items.is_empty(), "{actions}");

    let about_accounts: Vec<&Value> = items
        .iter()
        .filter(|item| item["subject"]["type"] == "account")
        .collect();
    assert!(
        !about_accounts.is_empty(),
        "a fresh instance with two accounts raises items about them: {actions}"
    );
    for item in &about_accounts {
        let title = item["subject"]["title"]
            .as_str()
            .unwrap_or_else(|| panic!("an account subject carries the owner's title: {item}"));
        assert!(!title.is_empty(), "{item}");
        // One reading of the store, not two: the sentence and the field are
        // filled from the same account at the moment the item is built, so a
        // client that renders either sees the same name.
        assert!(
            item["reason"].as_str().expect("reason").contains(title),
            "the name beside the identifier and the name in the sentence differ: {item}"
        );
    }

    let about_savings: Vec<&&Value> = about_accounts
        .iter()
        .filter(|item| item["subject"]["id"] == savings)
        .collect();
    assert!(!about_savings.is_empty(), "{actions}");
    for item in about_savings {
        assert_eq!(item["subject"]["title"], "Savings", "{item}");
        assert_eq!(item["subject"]["institution"], "Northline", "{item}");
    }

    // The account the harness created carries no institution, and the key is
    // absent rather than null: he has not said, which is not «it has none».
    for item in &about_accounts {
        if item["subject"]["id"] == json!(harness.account.inner()) {
            assert_eq!(item["subject"]["title"], "Brokerage", "{item}");
            assert!(
                item["subject"].get("institution").is_none(),
                "an institution the owner never gave is absent, not null: {item}"
            );
        }
    }
}

/// An event subject stays a bare identifier, and that is the rule, not a gap.
///
/// The naming rule is about things the owner named. Nothing he said names an
/// event: its identity is the identifier, and what it was is in the item's
/// sentence. Printing a title for one would mean inventing a name for a fact.
#[tokio::test]
async fn an_event_subject_carries_no_name() {
    let harness = harness();
    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");
    for item in actions.as_array().expect("action items") {
        if item["subject"]["type"] == "event" {
            let keys: std::collections::BTreeSet<&str> = item["subject"]
                .as_object()
                .expect("subject object")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                std::collections::BTreeSet::from(["type", "id"]),
                "{item}"
            );
        }
    }
}

/// The disposition answer names the account it is about.
///
/// `GET /v1/accounts/{id}/scope` exists to be read back to the owner — «is this
/// one inside your perimeter, and if not, why» — and it answers about exactly
/// one account, so nothing else in the response says which. A client that has
/// just been handed the identifier by the queue can render the answer without a
/// second call, and one that resolved the account from a report cannot render
/// the wrong one.
#[tokio::test]
async fn an_account_scope_answer_names_the_account() {
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Northline" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let savings = created["id"].as_str().expect("account id").to_owned();
    let scope_path = format!("/v1/accounts/{savings}/scope");

    let (status, before) = call(
        &harness.router,
        get(&scope_path, Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["account"], savings, "{before}");
    assert_eq!(before["title"], "Savings", "{before}");
    assert_eq!(before["institution"], "Northline", "{before}");
    assert_eq!(before["disposition"], "undecided", "{before}");

    // The write answers in the same shape as the read: an owner who has just
    // ruled an account outside is told which account he ruled on.
    let (status, recorded) = call(
        &harness.router,
        post(
            &scope_path,
            &harness.owner_token,
            &json!({
                "disposition": "outside",
                "reason": "A counterparty's account, not the owner's money.",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["account"], savings, "{recorded}");
    assert_eq!(recorded["title"], "Savings", "{recorded}");
    assert_eq!(recorded["institution"], "Northline", "{recorded}");
    assert_eq!(recorded["disposition"], "outside", "{recorded}");
}

/// The third state, reachable through the API.
///
/// An account can be outside the perimeter on purpose — a counterparty's, a
/// closed one — and the queue must stop asking once the owner has said so. If
/// the only way to silence the item were to put the account inside a contour,
/// the fix would have replaced a silent omission with a permanent nag.
#[tokio::test]
async fn an_account_ruled_outside_the_perimeter_stops_being_asked_about() {
    let harness = harness();
    let (status, outside) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Shop One" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{outside}");
    let outside = outside["id"].as_str().expect("account id").to_owned();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Household",
                "accounts": [harness.account.inner()],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let scope_path = format!("/v1/accounts/{outside}/scope");
    let (status, before) = call(
        &harness.router,
        get(&scope_path, Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["disposition"], "undecided", "{before}");

    let (status, recorded) = call(
        &harness.router,
        post(
            &scope_path,
            &harness.owner_token,
            &json!({
                "disposition": "outside",
                "reason": "A counterparty's account, not the owner's money.",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["disposition"], "outside", "{recorded}");
    assert_eq!(
        recorded["reason"], "A counterparty's account, not the owner's money.",
        "{recorded}"
    );

    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");
    assert!(
        actions
            .as_array()
            .expect("action items")
            .iter()
            .all(|item| item["kind"] != "account_scope_undecided"),
        "a decided account raises nothing: {actions}"
    );

    // Withdrawing the decision reopens the goal rather than leaving the account
    // silently settled for ever.
    let (status, cleared) = call(
        &harness.router,
        post(
            &scope_path,
            &harness.owner_token,
            &json!({ "disposition": "undecided" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["disposition"], "undecided", "{cleared}");

    let (status, reopened) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert!(
        reopened
            .as_array()
            .expect("action items")
            .iter()
            .any(|item| item["kind"] == "account_scope_undecided"
                && item["subject"]
                    == json!({ "type": "account", "id": outside, "title": "Shop One" })),
        "{reopened}"
    );
}

/// The disposition route refuses what it cannot honour, and says why.
#[tokio::test]
async fn a_scope_decision_needs_a_reason_and_cannot_claim_membership() {
    let harness = harness();
    let scope_path = format!("/v1/accounts/{}/scope", harness.account.inner());

    let (status, inside) = call(
        &harness.router,
        post(
            &scope_path,
            &harness.owner_token,
            &json!({ "disposition": "inside" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{inside}");
    assert_eq!(inside["field"], "disposition", "{inside}");

    let (status, unreasoned) = call(
        &harness.router,
        post(
            &scope_path,
            &harness.owner_token,
            &json!({ "disposition": "outside" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{unreasoned}");
    assert_eq!(unreasoned["field"], "reason", "{unreasoned}");

    let (status, blank) = call(
        &harness.router,
        post(
            &scope_path,
            &harness.owner_token,
            &json!({ "disposition": "outside", "reason": "   " }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{blank}");

    // The perimeter is the owner's judgement in either direction.
    let (status, refused) = call(
        &harness.router,
        post(
            &scope_path,
            &harness.agent_token,
            &json!({ "disposition": "outside", "reason": "Closed years ago." }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");

    // A missing account and someone else's are the same answer.
    let (status, unknown) = call(
        &harness.router,
        get(
            &format!("/v1/accounts/{}/scope", Uuid::new_v4()),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
}

/// An item the agent cannot call says so in its state, not only in its target.
///
/// The queue's states are the agent's map of what it may do. `needs_owner_input`
/// elsewhere accompanies a real operation with a list of fields to collect;
/// carrying it on an item with no operation at all made «collect these and call
/// this» indistinguishable from «there is nothing here for you to call».
///
/// The rule is swept in both directions, but only one of them has a witness
/// here, and that is a fact about the queue rather than a hole in the test:
/// since `start_account_import` was promoted, `frontier` emits no `blocked` item
/// at all. The blocked diagnostics are all carried by reports and by broker
/// synchronisation, not by `/v1/actions`. The witness kept is therefore the item
/// that once got this wrong, asserted the other way round.
#[tokio::test]
async fn no_queue_item_promises_a_call_it_does_not_have() {
    let harness = harness();
    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");

    let items = actions.as_array().expect("action items");
    assert!(
        !items.is_empty(),
        "the fixture must produce a queue to sweep"
    );
    for item in items {
        if item["target"]["type"] == "none" {
            assert_eq!(item["state"], "blocked", "{item}");
            assert!(item["required_scope"].is_null(), "{item}");
        } else {
            assert_ne!(item["state"], "blocked", "{item}");
        }
    }
    assert!(
        items.iter().any(|item| {
            item["kind"] == "start_account_import"
                && item["state"] == "needs_owner_input"
                && item["target"]["type"] == "options"
        }),
        "{actions}"
    );
}

/// Every report states the population it answered about, and says when that
/// population omits an account nobody has ruled on.
///
/// The quality fields of a report all concern defects inside the calculation
/// and can every one be clean while the wrong accounts were selected —
/// selection happens before the fold, so nothing computed afterwards can see
/// what was left out. That is the shape of the failure an importer hit with two
/// banks: every verdict positive, quality clean, and half the money outside the
/// scope the figures were computed over. Without this block the response reads
/// as an answer about all of it.
#[tokio::test]
async fn every_report_names_the_population_it_answered_about() {
    let harness = harness();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Reported", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("scope")
        .to_owned();

    // A second bank, known to the system and in no scope at all: nobody has
    // ruled on whether it belongs in the answer.
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Second Bank" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let savings = created["id"].as_str().expect("identifier").to_owned();

    let balances = format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31");
    let flow = format!("/v1/reports/flow?contour={contour_id}&from=2026-01-01&to=2026-01-31");
    let returns = format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-31");
    for path in [balances, flow, returns] {
        let (status, report) = call(&harness.router, get(&path, Some(&harness.owner_token))).await;
        assert_eq!(status, StatusCode::OK, "{path}: {report}");

        let population = &report["population"];
        assert_eq!(population["contour"], contour_id, "{path}: {report}");
        assert_eq!(
            population["known_account_coverage"], "undecided",
            "{path}: an account in no scope is one nobody has ruled on: {report}"
        );

        let covered = population["covered"].as_array().expect("covered accounts");
        assert_eq!(covered.len(), 1, "{path}: {report}");
        assert_eq!(covered[0]["account"], harness.account.inner().to_string());
        assert_eq!(covered[0]["standing"], "covered");

        let outside = population["outside"].as_array().expect("outside accounts");
        assert_eq!(outside.len(), 1, "{path}: {report}");
        assert_eq!(outside[0]["account"], savings, "{path}: {report}");
        // Named, not merely counted: an owner asked to rule on an omission
        // cannot act on a bare identifier.
        assert_eq!(outside[0]["title"], "Savings", "{path}: {report}");
        assert_eq!(
            outside[0]["standing"], "outside_undecided",
            "{path}: {report}"
        );
    }
}

/// The manifest follows the scope the figures were folded over: a report over a
/// scope that covers everything says so, in the same field.
///
/// This is the pair to the test above. A block that said `undecided` whatever
/// the scope contained would pass that test and mean nothing.
#[tokio::test]
async fn a_report_over_every_known_account_says_its_population_is_whole() {
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Second Bank" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let savings = created["id"].as_str().expect("identifier").to_owned();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Everything",
                "accounts": [harness.account.inner().to_string(), savings],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("scope")
        .to_owned();

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(
        report["population"]["known_account_coverage"], "whole",
        "{report}"
    );
    assert_eq!(
        report["population"]["covered"]
            .as_array()
            .expect("covered accounts")
            .len(),
        2,
        "{report}"
    );
    assert_eq!(
        report["population"]["outside"]
            .as_array()
            .expect("outside accounts")
            .len(),
        0,
        "{report}"
    );
    // The manifest and the rows are one selection: what the report covered is
    // what it computed over.
    assert_eq!(
        report["accounts"].as_array().expect("rows").len(),
        2,
        "{report}"
    );
}

/// The verdict names what it counted, and the thing it counted is published
/// beside it in full.
///
/// The field was `population.completeness`, and a report over four accounts of
/// a source that held seven answered `whole` — correctly, over the accounts the
/// instance had been told about, and read by a client as "these figures are all
/// of his money". Nothing here can see a source document: the import path
/// receives the rows a client chose to send it, so the second claim is one this
/// API cannot make and must not appear to.
///
/// So the fix is not a new field asserting source coverage. It is the name, and
/// the denominator published whole: `covered` and `outside` together are
/// exactly the accounts this instance knows of, which is what makes the
/// comparison against a source possible for the one party that holds it.
#[tokio::test]
async fn the_population_verdict_names_its_denominator_and_publishes_it() {
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Second Bank" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Reported", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let population = &report["population"];

    // The word that claimed more than the fold can know is gone from the wire.
    assert!(
        population.get("completeness").is_none(),
        "the population still publishes a verdict whose name outruns it: {population}"
    );
    assert!(
        population["known_account_coverage"].is_string(),
        "{population}"
    );

    // And the denominator is the account list, entire: nothing is counted that
    // a reader cannot see, and nothing a reader can see is left uncounted.
    let (status, accounts) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accounts}");
    let known: BTreeSet<String> = accounts
        .as_array()
        .expect("accounts")
        .iter()
        .map(|account| account["id"].as_str().expect("identifier").to_owned())
        .collect();
    let counted: BTreeSet<String> = ["covered", "outside"]
        .iter()
        .flat_map(|side| population[side].as_array().expect("a side of the manifest"))
        .map(|entry| entry["account"].as_str().expect("identifier").to_owned())
        .collect();
    assert_eq!(known, counted, "{report}");
}

/// An account the owner has placed in another scope is outside on a decision,
/// and the response says which of the two kinds of omission it is.
#[tokio::test]
async fn an_account_placed_in_another_scope_is_not_reported_as_undecided() {
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Second Bank" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let savings = created["id"].as_str().expect("identifier").to_owned();

    let (status, reported) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Reported", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reported}");
    let contour_id = reported["contour"].as_str().expect("scope").to_owned();

    let (status, elsewhere) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Elsewhere", "accounts": [savings] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{elsewhere}");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(
        report["population"]["known_account_coverage"], "bounded",
        "{report}"
    );
    let outside = report["population"]["outside"]
        .as_array()
        .expect("outside accounts");
    assert_eq!(outside.len(), 1, "{report}");
    assert_eq!(
        outside[0]["standing"], "outside_placed_elsewhere",
        "{report}"
    );
}

/// The owner rules an account outside, in as many words and with a reason, and
/// the report stops calling it an account nobody has ruled on.
///
/// This is the promise `closed_by` makes for `account_in_no_scope`: the
/// register names `record_account_scope` as one of the two calls that close it.
/// It was published before the report read the disposition, so a caller could
/// make the call, be answered `200`, fetch the report again and find the same
/// caveat over its own ruling — a silent gap turned into a broken promise. The
/// assertions below are the promise: the standing changes, the completeness
/// changes, and the line that offered the call is gone.
#[tokio::test]
async fn an_account_the_owner_ruled_outside_stops_being_one_nobody_ruled_on() {
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Second Bank" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let savings = created["id"].as_str().expect("identifier").to_owned();

    let (status, reported) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Reported", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reported}");
    let contour_id = reported["contour"].as_str().expect("scope").to_owned();

    let path = format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31");

    // Before the ruling: nobody has decided, and the register says so and says
    // which call would settle it.
    let (status, before) = call(&harness.router, get(&path, Some(&harness.owner_token))).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(
        before["population"]["known_account_coverage"], "undecided",
        "{before}"
    );
    assert!(
        before["confidence"]["caveats"]
            .as_array()
            .expect("caveats")
            .iter()
            .any(|caveat| caveat["kind"] == "account_in_no_scope"),
        "{before}"
    );

    let (status, ruled) = call(
        &harness.router,
        post(
            &format!("/v1/accounts/{savings}/scope"),
            &harness.owner_token,
            &json!({ "disposition": "outside", "reason": "held for somebody else" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ruled}");
    assert_eq!(ruled["disposition"], "outside", "{ruled}");

    let (status, after) = call(&harness.router, get(&path, Some(&harness.owner_token))).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(
        after["population"]["known_account_coverage"], "bounded",
        "a ruling makes the omission deliberate: {after}"
    );
    let outside = after["population"]["outside"]
        .as_array()
        .expect("outside accounts");
    assert_eq!(outside.len(), 1, "{after}");
    assert_eq!(outside[0]["account"], savings, "{after}");
    assert_eq!(outside[0]["standing"], "outside_by_decision", "{after}");
    // Named and located: this is the list the owner is read back, and two
    // accounts he calls one word are one line apart in it.
    assert_eq!(outside[0]["title"], "Savings", "{after}");
    assert_eq!(outside[0]["institution"], "Second Bank", "{after}");

    let caveats = after["confidence"]["caveats"]
        .as_array()
        .expect("caveats")
        .clone();
    assert!(
        !caveats
            .iter()
            .any(|caveat| caveat["kind"] == "account_in_no_scope"),
        "the report still says nobody ruled on an account the owner ruled on: {after}"
    );
    let ruled_out = caveats
        .iter()
        .find(|caveat| caveat["kind"] == "account_ruled_outside")
        .unwrap_or_else(|| panic!("no caveat about the account ruled outside: {after}"));
    assert_eq!(
        ruled_out["subject"],
        json!({ "type": "account", "id": savings }),
        "{ruled_out}"
    );
    assert_eq!(ruled_out["see"], "population.outside[]", "{ruled_out}");
    // The figures are still partial — deliberately — so the caveat stands. What
    // it must not do is offer him the call he has already made.
    assert_eq!(
        ruled_out["closed_by"],
        json!([{
            "operationId": "add_contour_version",
            "method": "POST",
            "path": "/v1/contours/{contour}/versions",
            "requestSchema": "#/components/schemas/AddContourVersionRequest",
        }]),
        "{ruled_out}"
    );
}

/// The returns report keeps every field it had, with the population beside
/// them.
///
/// The manifest is added to that response through a wrapper, so this checks the
/// wrapper did not move the figures: a client reading `data_quality` at the top
/// level goes on reading it there.
#[tokio::test]
async fn the_population_joins_the_returns_report_without_moving_its_fields() {
    let harness = harness();
    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Reported", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert!(report["data_quality"]["status"].is_string(), "{report}");
    assert!(report["applied_rules"]["contour"].is_string(), "{report}");
    assert!(report["xirr_pre_tax"].is_object(), "{report}");
    assert!(report["population"]["covered"].is_array(), "{report}");
}

/// The vocabulary a client must switch on is published, and it is the
/// vocabulary the server sends.
#[tokio::test]
async fn the_openapi_document_declares_the_population_a_report_covered() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    for schema in ["PopulationDto", "PopulationAccountDto"] {
        assert!(
            spec["components"]["schemas"][schema].is_object(),
            "schema {schema} must be in OpenAPI"
        );
    }
    let population = &spec["components"]["schemas"]["PopulationDto"]["properties"];
    assert!(
        population["known_account_coverage"].is_object(),
        "{population}"
    );
    assert!(population["covered"].is_object(), "{population}");
    assert!(population["outside"].is_object(), "{population}");

    let balances = &spec["components"]["schemas"]["BalancesReportDto"]["properties"];
    assert!(balances["population"].is_object(), "{balances}");
    let flow = &spec["components"]["schemas"]["MoneyFlowReportDto"]["properties"];
    assert!(flow["population"].is_object(), "{flow}");
}

/// Follows a caveat's `see` path through the response it was published in.
///
/// `[]` stands for every element of an array: the path resolves when at least
/// one element carries the rest of it. A caveat's whole contract is that the
/// reader can check it at the field it names instead of believing the summary,
/// so a pointer leading nowhere is a caveat nobody can check.
fn see_resolves(report: &Value, path: &str) -> bool {
    fn walk(value: &Value, segments: &[&str]) -> bool {
        let Some((head, rest)) = segments.split_first() else {
            return !value.is_null();
        };
        if let Some(name) = head.strip_suffix("[]") {
            return value
                .get(name)
                .and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|item| walk(item, rest)));
        }
        value.get(head).is_some_and(|next| walk(next, rest))
    }
    walk(report, &path.split('.').collect::<Vec<_>>())
}

/// Every report opens by saying what would have to be true for its figures to
/// be complete, and which of those things are not.
///
/// The failure this pins is not a missing fact. `population.completeness`,
/// `accounts[].cash[].opening` and the rest were each published and each
/// correct; `population` was simply the **last** top-level field of the
/// balances answer, after `accounts` and `negative_cash`, and a run that read
/// `covered=3, outside=15` took the rows for a complete statement of what the
/// owner held. A caveat published after the figures has already lost to the
/// reader who stopped at the figures.
///
/// So the assertions here are about position and about pointing: the register
/// is the first key on the wire, and every caveat in it names a field of the
/// same response that states the fact in full. It is never a second source of
/// truth — that is why nothing here asserts an amount.
#[tokio::test]
async fn every_report_opens_with_what_its_figures_do_not_account_for() {
    let harness = harness();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Reported", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("scope")
        .to_owned();

    // A second account in no scope at all: nobody has ruled on whether its
    // money belongs in these figures.
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Second Bank" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let savings = created["id"].as_str().expect("identifier").to_owned();

    let reports = [
        (
            format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            "asset_snapshot",
        ),
        (
            format!("/v1/reports/flow?contour={contour_id}&from=2026-01-01&to=2026-01-31"),
            "money_flow",
        ),
        (
            format!("/v1/reports/returns?contour={contour_id}&currency=RUB&as_of=2026-01-31"),
            "returns",
        ),
    ];

    for (path, goal) in reports {
        let (status, _headers, bytes) =
            call_raw(&harness.router, get(&path, Some(&harness.owner_token))).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let body = String::from_utf8(bytes).expect("utf-8 response");

        // On the wire, not merely present. `serde_json::Value` sorts keys, so
        // the ordering this test exists for can only be checked on the bytes.
        assert!(
            body.starts_with("{\"confidence\":"),
            "{path}: the register must be the first thing read: {body}"
        );

        let report: Value = serde_json::from_str(&body).expect("json response");
        let confidence = &report["confidence"];
        assert_eq!(confidence["goal"], goal, "{path}: {confidence}");
        assert_eq!(
            confidence["complete"], false,
            "{path}: an account nobody has ruled on is outside these figures: {confidence}"
        );

        let caveats = confidence["caveats"].as_array().expect("caveats");
        assert!(!caveats.is_empty(), "{path}: {confidence}");
        // No score anywhere: what is published is a list of checkable things.
        for key in ["score", "confidence_score", "percentage", "grade", "level"] {
            assert!(
                confidence.get(key).is_none(),
                "{path}: the register must not grade the answer: {confidence}"
            );
        }

        // Every caveat points at a field of this same response, and the field
        // is there.
        for caveat in caveats {
            let see = caveat["see"].as_str().expect("a field to check");
            assert!(
                see_resolves(&report, see),
                "{path}: caveat {caveat} points at {see}, which this response does not carry"
            );
            assert!(
                !caveat["detail"].as_str().expect("a sentence").is_empty(),
                "{path}: {caveat}"
            );
        }

        let outside = caveats
            .iter()
            .find(|caveat| caveat["kind"] == "account_in_no_scope")
            .unwrap_or_else(|| panic!("{path}: no caveat about the account in no scope: {report}"));
        assert_eq!(
            outside["subject"],
            json!({ "type": "account", "id": savings }),
            "{path}: {outside}"
        );
        assert_eq!(outside["see"], "population.outside[]", "{path}: {outside}");
    }
}

/// A cash figure accumulated from a start nothing asserts is named in the
/// register, by account and currency.
///
/// `cash.amount` under `opening: unasserted` is a running sum from an unknown
/// start and is not a balance. The DTO said so precisely and the endpoint is
/// still called balances and the field is still called amount; the register
/// says it before either is read, and points back at the `opening` that carries
/// the same fact.
#[tokio::test]
async fn a_running_cash_sum_is_named_by_account_and_currency() {
    let harness = harness();
    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Only account", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "3000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2026-01-05" },
                    "idempotency_key": "confidence-deposit"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");

    // The population is whole here, so the register can only be speaking about
    // the figure itself.
    assert_eq!(
        report["population"]["known_account_coverage"], "whole",
        "{report}"
    );
    assert_eq!(
        report["accounts"][0]["cash"][0]["kind"],
        "movement_since_unknown_start"
    );
    let confidence = &report["confidence"];
    assert_eq!(
        confidence["complete"], false,
        "a running sum is not a balance: {report}"
    );
    let caveat = confidence["caveats"]
        .as_array()
        .expect("caveats")
        .iter()
        .find(|caveat| caveat["kind"] == "running_cash_sum")
        .unwrap_or_else(|| panic!("no caveat about the running sum: {report}"));
    assert_eq!(
        caveat["subject"],
        json!({
            "type": "account_currency",
            "account": harness.account.inner().to_string(),
            "currency": "RUB",
        }),
        "{caveat}"
    );
    assert_eq!(caveat["see"], "accounts[].cash[].kind", "{caveat}");
    // The register summarises; it does not restate the amount, because a second
    // copy of a figure is a second chance to state it wrongly.
    assert!(
        !caveat["detail"]
            .as_str()
            .expect("a sentence")
            .contains("3000"),
        "{caveat}"
    );
}

/// The register's vocabulary is published, and the block is declared on every
/// report that carries it.
#[tokio::test]
async fn the_openapi_document_declares_the_register_a_report_opens_with() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    for schema in [
        "ConfidenceDto",
        "CaveatDto",
        "CaveatSubjectDto",
        "ClosingOperationDto",
    ] {
        assert!(
            spec["components"]["schemas"][schema].is_object(),
            "schema {schema} must be in OpenAPI"
        );
    }
    let confidence = &spec["components"]["schemas"]["ConfidenceDto"]["properties"];
    assert!(confidence["goal"].is_object(), "{confidence}");
    assert!(confidence["complete"].is_object(), "{confidence}");
    assert!(confidence["caveats"].is_object(), "{confidence}");

    let caveat = &spec["components"]["schemas"]["CaveatDto"]["properties"];
    for field in ["kind", "subject", "detail", "see", "closed_by"] {
        assert!(caveat[field].is_object(), "{field}: {caveat}");
    }

    // Spelled as an action's target spells it, so one client reader serves
    // both.
    let closing = &spec["components"]["schemas"]["ClosingOperationDto"]["properties"];
    for field in ["operationId", "method", "path", "requestSchema"] {
        assert!(closing[field].is_object(), "{field}: {closing}");
    }

    // `ReturnsAnswerDto` flattens the report into itself, which utoipa renders
    // as a composition rather than one property map, so the search follows
    // `allOf` too.
    fn declares_confidence(schema: &Value) -> bool {
        schema["properties"]["confidence"].is_object()
            || schema["allOf"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(declares_confidence))
    }

    for report in [
        "BalancesReportDto",
        "MoneyFlowReportDto",
        "ReturnsAnswerDto",
    ] {
        let schema = &spec["components"]["schemas"][report];
        assert!(
            declares_confidence(schema),
            "{report} must declare the register: {schema}"
        );
    }
}

/// Every remedy the caveat register names is a call this API actually declares.
///
/// This is the guard on the join the register now publishes. `CaveatKind` lives
/// in the core and the action queue lives in the application, so the operation
/// a caveat names could once have been written twice — a table in the transport
/// beside the queue that actually offers those calls — and the two would drift
/// on the first rename, leaving a report pointing at a call nothing answers.
/// They are not written twice: `OperationKey` is one enum owned by the core, so
/// a name that does not exist does not compile, and `ActionCatalog::from_openapi`
/// resolves the whole vocabulary — not a list repeated by hand — against the
/// completed contract, so a key nothing routes fails the server's start-up.
///
/// What is left for a test is that those two facts stay true: every key the
/// register can name resolves through the same catalogue the queue uses, and
/// resolves to the operation the document declares under that identifier.
#[test]
fn every_remedy_the_register_names_is_a_call_the_contract_publishes() {
    let harness = harness();
    let catalog = ActionCatalog::from_openapi(&harness.api).expect("action catalog");

    // The catalogue addresses the whole vocabulary. If it ever went back to a
    // hand-written subset, a caveat naming a key outside it would resolve to
    // nothing at the moment a client asked for the report.
    for key in OperationKey::ALL {
        assert_eq!(
            catalog.operation(key).operation_id,
            key.as_str(),
            "{} is not addressed by the catalogue",
            key.as_str()
        );
    }

    for kind in CaveatKind::ALL {
        for key in kind.closed_by() {
            let resolved = catalog.operation(*key);
            let item = harness
                .api
                .paths
                .paths
                .get(&resolved.path)
                .unwrap_or_else(|| {
                    panic!(
                        "{} names {}, and the contract declares no {}",
                        kind.code(),
                        key.as_str(),
                        resolved.path
                    )
                });
            let operation = match resolved.method.as_str() {
                "POST" => item.post.as_ref(),
                "PUT" => item.put.as_ref(),
                "PATCH" => item.patch.as_ref(),
                method => panic!("{} names {key:?} under an unexpected {method}", kind.code()),
            }
            .unwrap_or_else(|| {
                panic!(
                    "{} names {}, and {} {} is not declared",
                    kind.code(),
                    key.as_str(),
                    resolved.method,
                    resolved.path
                )
            });
            assert_eq!(
                operation.operation_id.as_deref(),
                Some(key.as_str()),
                "{} names {}, and {} {} answers to another identifier",
                kind.code(),
                key.as_str(),
                resolved.method,
                resolved.path
            );
            // A caveat's remedy is always a call that takes a body: it is a
            // fact the owner supplies, and there is nowhere but the body to put
            // it. The `Option` exists for the calls a *refusal* offers, which
            // include one that takes nothing — abandoning an import session.
            assert!(
                resolved
                    .request_schema
                    .as_ref()
                    .is_some_and(|schema| !schema.is_empty()),
                "{} names {}, whose request shape is not published",
                kind.code(),
                key.as_str()
            );
        }
    }
}

/// A caveat carries the call that closes it, and says so when nothing does.
///
/// The gap this closes (iaam-f234): the register said what a report was silent
/// about and not what to do about it. The only join was by `goal` — fetch
/// `/v1/actions`, filter by the report's goal — which answers what stands
/// between the caller and the *whole* report rather than what removes the line
/// in hand, and an external agent read `complete: false` and went hunting
/// through separate sections anyway.
///
/// Addressed, not merely named: the caveat carries the method and the path, in
/// the spelling an action's target uses, so the call can be copied rather than
/// composed. What it does not carry is a request plan — that is the queue's,
/// because only the queue knows the account and the interval.
#[tokio::test]
async fn a_caveat_carries_the_call_that_closes_it_and_says_so_when_nothing_does() {
    let harness = harness();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Reported", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"]
        .as_str()
        .expect("scope")
        .to_owned();

    // A second account nobody has ruled on, and a movement on the first so that
    // its cash figure is a running sum from an unknown start.
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Second Bank" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "1200.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2026-01-05" },
                    "idempotency_key": "closed-by-deposit"
                }, {
                    "account": harness.account.inner(),
                    "type": "opening_position",
                    "instrument": harness.instrument.inner(),
                    "custody": harness.custody.inner(),
                    "quantity": "10",
                    "cost_basis": "100.00",
                    "currency": "RUB",
                    "dates": { "trade": "2026-01-01" },
                    "idempotency_key": "closed-by-position"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let caveats = report["confidence"]["caveats"]
        .as_array()
        .expect("caveats")
        .clone();

    let find = |kind: &str| {
        caveats
            .iter()
            .find(|caveat| caveat["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} caveat: {report}"))
            .clone()
    };

    // Both ways out, in the order the outstanding-work queue offers them: place
    // the account in a contour, or rule it deliberately outside.
    let outside = find("account_in_no_scope");
    assert_eq!(
        outside["closed_by"],
        json!([
            {
                "operationId": "add_contour_version",
                "method": "POST",
                "path": "/v1/contours/{contour}/versions",
                "requestSchema": "#/components/schemas/AddContourVersionRequest",
            },
            {
                "operationId": "record_account_scope",
                "method": "POST",
                "path": "/v1/accounts/{id}/scope",
                "requestSchema": "#/components/schemas/RecordAccountScopeRequest",
            },
        ]),
        "{outside}"
    );

    // The opening assertion is what turns the figure into a balance, and it is
    // the operation the queue names for the same state.
    let running = find("running_cash_sum");
    assert_eq!(
        running["closed_by"][0]["operationId"],
        "record_owner_balance"
    );
    assert_eq!(running["closed_by"][0]["method"], "POST");
    assert_eq!(
        running["closed_by"][0]["path"], "/v1/reconciliation/balance",
        "{running}"
    );
    // The register points at the call; the queue fills it in. A caveat that
    // carried a request plan would be computing one from what a caveat holds,
    // which is its own identity and nothing else.
    assert!(
        running["closed_by"][0].get("request").is_none(),
        "{running}"
    );

    // Absent, and it means nothing closes this — not that nobody decided. The
    // holding is in the journal and no quote covers it; this API records prices
    // from sources and accepts no value for a holding, so there is no call to
    // name. The field is always present, so a client never has to tell «nothing
    // closes this» from «nobody decided».
    let (status, snapshot) = call(
        &harness.router,
        get(
            &format!("/v1/reports/assets?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");
    let unclosable = snapshot["confidence"]["caveats"]
        .as_array()
        .expect("caveats")
        .iter()
        .find(|caveat| caveat["kind"] == "holding_not_valued")
        .unwrap_or_else(|| panic!("no holding_not_valued caveat: {snapshot}"))
        .clone();
    assert_eq!(unclosable["closed_by"], json!([]), "{unclosable}");
}

/// Creating a contour and versioning one are two acts, and the create route
/// performs only the first.
///
/// The defect this pins: `POST /v1/contours` carried both, and the destructive
/// reading — «mint a fresh contour» — was what an omitted identifier gave you.
/// An agent that had already created a contour for one bank called the same
/// route again for the second and silently acquired a second contour, whose
/// report showed one bank. Repeating the create call is now a replay of the
/// same intent, not a second perimeter.
#[tokio::test]
async fn creating_a_contour_twice_with_the_same_intent_writes_one_contour() {
    let harness = harness();
    let intent = json!({
        "title": "Household",
        "accounts": [harness.account.inner()],
    });

    let (status, first) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &intent),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["created"], true, "the first call created it: {first}");
    let contour = first["contour"].as_str().expect("contour id").to_owned();

    let (status, second) = call(
        &harness.router,
        post("/v1/contours", &harness.owner_token, &intent),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the same intent is a replay, not a second perimeter: {second}"
    );
    assert_eq!(second["created"], false, "{second}");
    assert_eq!(second["contour"], contour, "{second}");
    assert_eq!(second["version"], 1, "a replay writes no version: {second}");

    let (status, listed) = call(
        &harness.router,
        get("/v1/contours", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let listed = listed.as_array().expect("contour list");
    assert_eq!(listed.len(), 1, "two calls, one contour: {listed:?}");
}

/// The create route refuses an identifier rather than honouring it.
///
/// Silently ignoring the field would leave the original defect intact for every
/// client already sending it: the request would still mint a second contour and
/// still say nothing. The refusal names the route that does what the caller
/// meant.
#[tokio::test]
async fn the_create_route_refuses_a_contour_identifier_and_names_the_versions_route() {
    let harness = harness();
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "contour": Uuid::new_v4(),
                "title": "Household",
                "accounts": [harness.account.inner()],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["field"], "contour", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .expect("message")
            .contains("/v1/contours/{contour}/versions"),
        "the refusal must name the act the caller meant: {refused}"
    );

    let (status, listed) = call(
        &harness.router,
        get("/v1/contours", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert!(
        listed.as_array().expect("contour list").is_empty(),
        "a refused request writes nothing: {listed}"
    );
}

/// The composition is readable, so an import skill can check the perimeter it
/// was handed against what the system believes.
#[tokio::test]
async fn a_contour_can_be_listed_and_read_back_with_its_accounts() {
    let harness = harness();
    let (status, created) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Household",
                "accounts": [harness.account.inner()],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let contour = created["contour"].as_str().expect("contour id").to_owned();

    let (status, listed) = call(
        &harness.router,
        get("/v1/contours", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let listed = listed.as_array().expect("contour list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["contour"], contour, "{listed:?}");
    assert_eq!(listed[0]["title"], "Household", "{listed:?}");
    assert_eq!(listed[0]["version"], 1, "{listed:?}");
    assert_eq!(
        listed[0]["accounts"],
        json!([harness.account.inner()]),
        "{listed:?}"
    );

    let (status, one) = call(
        &harness.router,
        get(
            &format!("/v1/contours/{contour}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["contour"], contour, "{one}");
    assert_eq!(one["accounts"], json!([harness.account.inner()]), "{one}");

    // A contour identifier is a UUID, and a UUID is not an access right (§14).
    let (status, absent) = call(
        &harness.router,
        get(
            &format!("/v1/contours/{}", Uuid::new_v4()),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{absent}");
}

/// Adding an account to the contour the owner already has is expressible.
///
/// The act the reporter needed and could not perform: every route the queue
/// offered him minted a contour, so a second bank's account could only arrive
/// inside a second perimeter.
#[tokio::test]
async fn an_account_is_added_to_an_existing_contour_without_a_second_contour() {
    let harness = harness();
    let (status, second) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let second = second["id"].as_str().expect("account id").to_owned();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Household",
                "accounts": [harness.account.inner()],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let contour = created["contour"].as_str().expect("contour id").to_owned();

    let both = json!({ "accounts": [harness.account.inner().to_string(), second] });
    let versions = format!("/v1/contours/{contour}/versions");
    let (status, added) = call(
        &harness.router,
        post(&versions, &harness.owner_token, &both),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{added}");
    assert_eq!(added["contour"], contour, "{added}");
    assert_eq!(added["version"], 2, "{added}");
    assert_eq!(
        added["created"], false,
        "versioning creates no contour: {added}"
    );
    // The title the contour already carries is not retyped to keep it.
    assert_eq!(added["title"], "Household", "{added}");

    // The same call again is a replay: the composition already holds, so no
    // version is written and the caller is told the current one.
    let (status, replayed) = call(
        &harness.router,
        post(&versions, &harness.owner_token, &both),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed["version"], 2, "{replayed}");

    let (status, listed) = call(
        &harness.router,
        get("/v1/contours", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let listed = listed.as_array().expect("contour list");
    assert_eq!(listed.len(), 1, "no second perimeter: {listed:?}");
    assert_eq!(listed[0]["version"], 2, "{listed:?}");
    assert_eq!(
        listed[0]["accounts"].as_array().expect("accounts").len(),
        2,
        "{listed:?}"
    );

    // A contour the caller does not hold is not versionable by knowing a UUID.
    let (status, absent) = call(
        &harness.router,
        post(
            &format!("/v1/contours/{}/versions", Uuid::new_v4()),
            &harness.owner_token,
            &both,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{absent}");
}

/// A caller may state the version it believes is current, and is refused if the
/// contour moved under it.
#[tokio::test]
async fn versioning_a_contour_that_moved_under_the_caller_is_refused() {
    let harness = harness();
    let (status, second) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let second = second["id"].as_str().expect("account id").to_owned();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Household",
                "accounts": [harness.account.inner()],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let contour = created["contour"].as_str().expect("contour id").to_owned();
    let versions = format!("/v1/contours/{contour}/versions");

    let (status, moved) = call(
        &harness.router,
        post(
            &versions,
            &harness.owner_token,
            &json!({ "accounts": [second] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{moved}");
    assert_eq!(moved["version"], 2, "{moved}");

    // The stale caller still believes version 1 is current and would drop the
    // account the other writer added.
    let (status, stale) = call(
        &harness.router,
        post(
            &versions,
            &harness.owner_token,
            &json!({
                "accounts": [harness.account.inner()],
                "expected_version": 1,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["field"], "expected_version", "{stale}");
    // `expected` is what the route required, `actual` what arrived — the same
    // reading every other refusal in this API has.
    assert_eq!(stale["expected"], "2", "{stale}");
    assert_eq!(stale["actual"], "1", "{stale}");

    let (status, unchanged) = call(
        &harness.router,
        get(
            &format!("/v1/contours/{contour}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    assert_eq!(unchanged["version"], 2, "a refusal writes nothing");
    assert_eq!(unchanged["accounts"], json!([second]), "{unchanged}");
}

/// The queue's item for an undecided account offers the act that adds it to a
/// contour the owner already has.
///
/// It used to offer the only shape the API had — the route that mints a
/// contour — so following the queue literally is what produced the second
/// perimeter in the report.
#[tokio::test]
async fn the_queue_points_an_undecided_account_at_an_existing_contour() {
    let harness = harness();
    let (status, orphan) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Second Bank Current" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{orphan}");
    let orphan = orphan["id"].as_str().expect("account id").to_owned();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Household",
                "accounts": [harness.account.inner()],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let contour = created["contour"].as_str().expect("contour id").to_owned();

    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");
    let item = actions
        .as_array()
        .expect("action items")
        .iter()
        .find(|item| item["kind"] == "account_scope_undecided")
        .expect("the orphaned account is named");
    let membership = published_option(item, "add_contour_version")
        .expect("the item must add the account to the contour that exists");
    assert_eq!(membership["method"], "POST", "{item}");
    assert_eq!(
        membership["path"], "/v1/contours/{contour}/versions",
        "{item}"
    );
    assert_eq!(
        membership["request"]["preset"]["contour"], contour,
        "with one contour there is no doubt which is meant: {item}"
    );

    // The action is a call the agent can make as written.
    let preset = &membership["request"]["preset"];
    let (status, added) = call(
        &harness.router,
        post(
            &format!("/v1/contours/{contour}/versions"),
            &harness.owner_token,
            &json!({ "accounts": preset["accounts"] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{added}");
    assert_eq!(added["contour"], contour, "{added}");

    let (status, listed) = call(
        &harness.router,
        get("/v1/contours", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let listed = listed.as_array().expect("contour list");
    assert_eq!(listed.len(), 1, "following the queue mints nothing");
    let accounts = listed[0]["accounts"].as_array().expect("accounts");
    assert!(
        accounts.iter().any(|account| *account == json!(orphan)),
        "{listed:?}"
    );
}

/// The workbook every document test in this block uploads.
///
/// Synthetic throughout: a real statement would put the owner's money into the
/// repository, and no assertion here needs a real one.
const SYNTHETIC_REPORT: &[u8] =
    include_bytes!("../../../tests/fixtures/reports/tinkoff-synthetic.xlsx").as_slice();

fn document_upload(harness: &Harness, workbook: &[u8]) -> Request<Body> {
    Request::builder()
        .uri(format!("/v1/documents?account={}", harness.account.inner()))
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header(
            "Content-Type",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
        .body(Body::from(workbook.to_vec()))
        .expect("request")
}

fn document_reparse(harness: &Harness, document_hash: &str, workbook: &[u8]) -> Request<Body> {
    Request::builder()
        .uri(format!(
            "/v1/documents/{document_hash}/reparse?account={}",
            harness.account.inner()
        ))
        .method("POST")
        .header("Authorization", format!("Bearer {}", harness.owner_token))
        .header(
            "Content-Type",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
        .body(Body::from(workbook.to_vec()))
        .expect("request")
}

/// The system keeps the document, so the agent driving a reparse need not.
///
/// The founding constraint is that the agent holds none of the owner's data.
/// A reparse route that demands the workbook back contradicts it: the caller
/// could only satisfy it by having kept the owner's statement. Reparsing with
/// the bytes and reparsing without them must reach the same answer, or the
/// empty-bodied call is a different operation wearing the same name.
#[tokio::test]
async fn a_stored_document_is_reparsed_without_being_sent_again() {
    let harness = harness();

    let (status, uploaded) =
        call(&harness.router, document_upload(&harness, SYNTHETIC_REPORT)).await;
    assert_eq!(status, StatusCode::OK, "{uploaded}");
    let document_hash = uploaded["document_hash"]
        .as_str()
        .expect("the upload names the document by its hash")
        .to_owned();

    let (status, with_bytes) = call(
        &harness.router,
        document_reparse(&harness, &document_hash, SYNTHETIC_REPORT),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{with_bytes}");

    let (status, without_bytes) = call(
        &harness.router,
        document_reparse(&harness, &document_hash, b""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{without_bytes}");
    assert_eq!(
        without_bytes, with_bytes,
        "a reparse from the stored document must produce what the resent bytes produced"
    );
}

/// The same file twice is one document, so the source identifier is stable.
///
/// The uploaded document is deduplicated by owner and hash in the store. If the
/// response invented a fresh source identifier on the second upload, it would
/// name a document that is not on record.
#[tokio::test]
async fn the_same_document_uploaded_twice_keeps_one_source_identifier() {
    let harness = harness();

    let (status, first) = call(&harness.router, document_upload(&harness, SYNTHETIC_REPORT)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = call(&harness.router, document_upload(&harness, SYNTHETIC_REPORT)).await;
    assert_eq!(status, StatusCode::OK, "{second}");

    assert_eq!(first["document_hash"], second["document_hash"]);
    assert_eq!(
        first["source"], second["source"],
        "the same file is one document, not two"
    );
}

/// The fallback, and the refusal when it cannot help.
///
/// A document uploaded before the system began storing sources left facts and
/// no bytes; a reparse of one has nothing to read. The route still accepts the
/// workbook for that case, and says so when it is given neither.
#[tokio::test]
async fn a_reparse_of_a_document_that_was_never_stored_says_why_it_cannot() {
    let harness = harness();
    let never_uploaded = "b".repeat(64);

    let (status, refusal) = call(
        &harness.router,
        document_reparse(&harness, &never_uploaded, b""),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");
    let message = refusal["message"]
        .as_str()
        .expect("a refusal has a message");
    assert!(
        message.contains("before the system began storing"),
        "the refusal must name why the stored document is missing: {message}"
    );
}

/// The published contract tells a client when the body is still needed.
#[tokio::test]
async fn the_openapi_document_says_when_a_reparse_still_needs_the_bytes() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let reparse = &spec["paths"]["/v1/documents/{id}/reparse"]["post"]["requestBody"];
    let description = reparse["description"]
        .as_str()
        .expect("the reparse body is described");
    assert!(
        description.contains("Empty"),
        "the contract must say that an empty body reparses the stored document: {description}"
    );
    assert!(
        description.contains("before the system began storing"),
        "the contract must say when the bytes are still needed: {description}"
    );
}

// ---------------------------------------------------------------------------
// The discovery stage: which of the owner's accounts are the two sides of one
// internal movement (iaam-7xh3)
// ---------------------------------------------------------------------------

/// `PUT`, which no other test in this file needs.
fn put(path: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("PUT")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn transfer_partners_queue(harness: &Harness) -> Vec<Value> {
    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");
    actions
        .as_array()
        .expect("action items")
        .iter()
        .filter(|item| item["kind"] == "resolve_transfer_relationships")
        .cloned()
        .collect()
}

/// The owner states which of his accounts money moves between, and the queue
/// stops asking — for the accounts he has answered, and only those.
///
/// The goal is quantified over his accounts, so a statement is not «some
/// relationship exists»: an account added afterwards — the second bank, which is
/// the case this exists for — reopens it, and nothing about the answers already
/// given closes the new question.
#[tokio::test]
async fn the_owner_states_his_transfer_relationships_and_a_new_account_reopens_the_question() {
    let harness = harness();
    let main = harness.account.inner().to_string();

    // One account: there is no other side, so nothing is asked.
    assert!(
        transfer_partners_queue(&harness).await.is_empty(),
        "with one account a transfer has no other side"
    );

    let (status, savings) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Northline" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{savings}");
    let savings = savings["id"].as_str().expect("account id").to_owned();

    let asked = transfer_partners_queue(&harness).await;
    assert_eq!(asked.len(), 2, "both accounts are asked: {asked:#?}");
    let item = asked
        .iter()
        .find(|item| item["subject"]["id"] == json!(main))
        .expect("the item names the account it is about");
    assert_eq!(item["category"], "required_for_goal", "{item}");
    // An unpaired leg is counted as money crossing the perimeter by the flow
    // report and by the returns projection's flow log. It is **not** in the way
    // of an asset snapshot: the leg lands on its own account's cash whether or
    // not its partner is known.
    assert_eq!(item["goals"], json!(["money_flow", "returns"]), "{item}");
    assert_eq!(item["state"], "needs_owner_input", "{item}");
    assert_eq!(item["required_scope"], "owner", "{item}");
    assert_eq!(
        item["target"]["operationId"], "record_account_transfer_partners",
        "{item}"
    );
    assert_eq!(item["target"]["method"], "PUT", "{item}");
    assert_eq!(
        item["target"]["path"], "/v1/accounts/{id}/transfer-partners",
        "{item}"
    );
    // Candidates are proposed; the choice is not made. The account itself is not
    // among them, because a transfer has two sides.
    let candidates = item["target"]["request"]["missing"][0]["candidates"]
        .as_array()
        .expect("the owner is offered his other accounts");
    assert_eq!(
        candidates
            .iter()
            .map(|entry| &entry["id"])
            .collect::<Vec<_>>(),
        vec![&json!(savings)],
        "{item}"
    );

    // Drawing this relationship is the owner's judgement, not the agent's.
    let (status, refusal) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{main}/transfer-partners"),
            &harness.agent_token,
            &json!({ "partners": [savings] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");

    // An account is not the other side of itself.
    let (status, refusal) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{main}/transfer-partners"),
            &harness.owner_token,
            &json!({ "partners": [main] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");

    let (status, recorded) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{main}/transfer-partners"),
            &harness.owner_token,
            &json!({ "partners": [savings] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["stated"], true, "{recorded}");
    assert_eq!(recorded["partners"], json!([savings]), "{recorded}");

    // Being named by his statement about `Main` is not a statement about
    // `Savings`: the far side of one relationship says nothing about the ones
    // this account is the near side of.
    let asked = transfer_partners_queue(&harness).await;
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert_eq!(asked[0]["subject"]["id"], json!(savings), "{asked:#?}");

    // «None of my others» is an answer, and it closes the item.
    let (status, none) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{savings}/transfer-partners"),
            &harness.owner_token,
            &json!({ "partners": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{none}");
    assert_eq!(none["stated"], true, "an empty list is a statement: {none}");
    assert!(
        transfer_partners_queue(&harness).await.is_empty(),
        "every account has been ruled on"
    );

    // A third account reopens the goal, and reopens it only for itself.
    let (status, everyday) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Everyday", "institution": "Southgate" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{everyday}");
    let everyday = everyday["id"].as_str().expect("account id").to_owned();
    let asked = transfer_partners_queue(&harness).await;
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert_eq!(asked[0]["subject"]["id"], json!(everyday), "{asked:#?}");

    // Withdrawing returns the account to awaiting a decision, and the read-back
    // says «not stated» rather than «none», which are different answers.
    let (status, withdrawn) = call(
        &harness.router,
        delete(
            &format!("/v1/accounts/{savings}/transfer-partners"),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{withdrawn}");
    assert_eq!(withdrawn["stated"], false, "{withdrawn}");
    let (status, read_back) = call(
        &harness.router,
        get(
            &format!("/v1/accounts/{savings}/transfer-partners"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(read_back["stated"], false, "{read_back}");
    let asked = transfer_partners_queue(&harness).await;
    assert_eq!(asked.len(), 2, "{asked:#?}");
}

/// An account identifier is not an access right, in either position.
#[tokio::test]
async fn a_transfer_statement_cannot_name_an_account_the_owner_does_not_hold() {
    let harness = harness();
    let main = harness.account.inner().to_string();
    let stranger = Uuid::new_v4();

    let (status, refusal) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{stranger}/transfer-partners"),
            &harness.owner_token,
            &json!({ "partners": [main] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refusal}");

    let (status, refusal) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{main}/transfer-partners"),
            &harness.owner_token,
            &json!({ "partners": [stranger] }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a named account that is not the owner's is a mistake in the statement, \
         not something to drop quietly: {refusal}"
    );
}

/// The partners of one statement, in a comparable order.
///
/// The store returns them ordered by identifier rather than by the order the
/// owner typed them, because what he stated is a set.
fn sorted_partners(statement: &Value) -> Vec<&str> {
    let mut named: Vec<&str> = statement["partners"]
        .as_array()
        .map(|partners| {
            partners
                .iter()
                .map(|entry| entry.as_str().expect("account id"))
                .collect()
        })
        .unwrap_or_default();
    named.sort_unstable();
    named
}

/// Twelve accounts were twelve calls; one batch answers them together.
///
/// The batch is transport and nothing else. It carries one entry per account,
/// each a complete enumeration, because naming `Savings` inside `Main`'s answer
/// establishes that money moves between the two and says nothing about whether
/// `Savings` also moves money with `Everyday`. So the batch does not shrink the
/// number of statements — it shrinks the number of round trips.
#[tokio::test]
async fn a_batch_answers_several_accounts_at_once_without_answering_any_for_another() {
    let harness = harness();
    let main = harness.account.inner().to_string();

    let mut opened = Vec::new();
    for title in ["Savings", "Everyday"] {
        let (status, account) = call(
            &harness.router,
            post(
                "/v1/accounts",
                &harness.owner_token,
                &json!({ "title": title, "institution": "Northline" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{account}");
        opened.push(account["id"].as_str().expect("account id").to_owned());
    }
    let savings = opened[0].clone();
    let everyday = opened[1].clone();

    assert_eq!(
        transfer_partners_queue(&harness).await.len(),
        3,
        "each account is asked its own question"
    );

    // One call, three statements, and «none of my others» travels in it like any
    // other answer.
    let (status, recorded) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.owner_token,
            &json!({
                "statements": [
                    { "account": main, "partners": [savings] },
                    { "account": savings, "partners": [main, everyday] },
                    { "account": everyday, "partners": [] },
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");
    let statements = recorded["statements"].as_array().expect("statements");
    assert_eq!(statements.len(), 3, "{recorded}");
    assert_eq!(statements[0]["account"], json!(main), "{recorded}");
    assert_eq!(statements[0]["partners"], json!([savings]), "{recorded}");
    assert_eq!(statements[1]["account"], json!(savings), "{recorded}");
    // Read back in the store's order, which is by identifier rather than by the
    // order the owner typed: what he stated is a set.
    let mut expected = vec![main.as_str(), everyday.as_str()];
    expected.sort_unstable();
    assert_eq!(sorted_partners(&statements[1]), expected, "{recorded}");
    assert_eq!(statements[2]["account"], json!(everyday), "{recorded}");
    assert_eq!(
        statements[2]["stated"], true,
        "an empty list is a statement, in a batch as in a single call: {recorded}"
    );

    assert!(
        transfer_partners_queue(&harness).await.is_empty(),
        "every account has been ruled on"
    );

    // The batch is the same fact as the single call: read one back on its own
    // route and it is what the batch put there.
    let (status, read_back) = call(
        &harness.router,
        get(
            &format!("/v1/accounts/{savings}/transfer-partners"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(sorted_partners(&read_back), expected, "{read_back}");

    // A fourth account reopens the goal for itself alone, exactly as it does
    // after three single calls: being named in nobody's enumeration and naming
    // nobody are different states, and the batch did not blur them.
    let (status, spare) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Spare", "institution": "Southgate" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{spare}");
    let asked = transfer_partners_queue(&harness).await;
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert_eq!(asked[0]["subject"]["id"], spare["id"], "{asked:#?}");
}

/// Every refusal the single-account route makes, the batch makes.
///
/// The two share the checking function, so this test is about the answers being
/// the same ones — a caller must be able to read a batch's refusal the way it
/// reads the refusal of the call the batch replaced.
#[tokio::test]
async fn a_batch_refuses_exactly_what_a_single_transfer_statement_refuses() {
    let harness = harness();
    let main = harness.account.inner().to_string();
    let stranger = Uuid::new_v4();

    let (status, savings) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Northline" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{savings}");
    let savings = savings["id"].as_str().expect("account id").to_owned();

    // Drawing the relationship is the owner's judgement, in bulk as singly.
    let (status, refusal) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.agent_token,
            &json!({ "statements": [{ "account": main, "partners": [savings] }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");

    // An account is not the other side of itself.
    let (status, refusal) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.owner_token,
            &json!({ "statements": [{ "account": main, "partners": [main] }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");

    // An account identifier is not an access right, in either position, and the
    // status is the one the single call gives: the batch is a cheaper way to
    // make the same requests, not a different answer to them.
    let (status, refusal) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.owner_token,
            &json!({ "statements": [{ "account": stranger, "partners": [main] }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refusal}");

    let (status, refusal) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.owner_token,
            &json!({ "statements": [{ "account": main, "partners": [stranger] }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refusal}");

    // Two enumerations for one account cannot both be the complete one. The
    // request is well formed and its meaning is not, which is the line between
    // `400` and `422` everywhere else in this API.
    let (status, refusal) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.owner_token,
            &json!({
                "statements": [
                    { "account": main, "partners": [savings] },
                    { "account": main, "partners": [] },
                ]
            }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an account named twice is a bad request, not a last-write-wins: {refusal}"
    );
    assert_eq!(
        refusal["field"],
        json!("/statements/1/account"),
        "{refusal}"
    );

    // An empty batch is accepted and changes nothing: a caller with no items
    // left to answer must not have to special-case the call it does not make.
    let (status, empty) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.owner_token,
            &json!({ "statements": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["statements"], json!([]), "{empty}");
}

/// A refused batch writes nothing — not even the entries before the bad one.
///
/// This is the property the batch exists to keep, and it is the reason the store
/// grew a batch method rather than the route looping over the single one: that
/// method commits per call, so a loop refused on the third entry would leave two
/// statements replaced. The owner answering several accounts together has not
/// half-said anything.
#[tokio::test]
async fn a_refused_batch_leaves_every_standing_statement_as_it_was() {
    let harness = harness();
    let main = harness.account.inner().to_string();
    let stranger = Uuid::new_v4();

    let (status, savings) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Northline" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{savings}");
    let savings = savings["id"].as_str().expect("account id").to_owned();

    let (status, everyday) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Everyday", "institution": "Southgate" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{everyday}");
    let everyday = everyday["id"].as_str().expect("account id").to_owned();

    // A statement stands for `Main`, and `Everyday` has not been ruled on.
    let (status, recorded) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{main}/transfer-partners"),
            &harness.owner_token,
            &json!({ "partners": [savings] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");

    // A batch that would replace `Main`'s statement and answer `Everyday`, with
    // an account the owner does not hold in the last entry.
    let (status, refusal) = call(
        &harness.router,
        put(
            "/v1/accounts/transfer-partners",
            &harness.owner_token,
            &json!({
                "statements": [
                    { "account": main, "partners": [] },
                    { "account": everyday, "partners": [main] },
                    { "account": savings, "partners": [stranger] },
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refusal}");

    // The first entry did not replace what stood.
    let (status, read_back) = call(
        &harness.router,
        get(
            &format!("/v1/accounts/{main}/transfer-partners"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(
        read_back["partners"],
        json!([savings]),
        "the refused batch must not have emptied Main's statement: {read_back}"
    );

    // The second entry did not answer an unanswered question.
    let (status, read_back) = call(
        &harness.router,
        get(
            &format!("/v1/accounts/{everyday}/transfer-partners"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(
        read_back["stated"], false,
        "the refused batch must not have recorded a statement for Everyday: {read_back}"
    );

    // And the queue still asks the two questions that were never answered.
    let asked = transfer_partners_queue(&harness).await;
    let subjects: Vec<&Value> = asked.iter().map(|item| &item["subject"]["id"]).collect();
    assert_eq!(asked.len(), 2, "{asked:#?}");
    assert!(subjects.contains(&&json!(savings)), "{asked:#?}");
    assert!(subjects.contains(&&json!(everyday)), "{asked:#?}");
}

/// The queue names the per-account operation; the batch is found in the spec.
///
/// The decision, asserted so that changing it is deliberate. A single queue item
/// covering many accounts would need a `RequestPlan` whose `missing` pointers
/// address array elements — a shape nothing else in the catalogue uses — and it
/// would give one state to twelve independent questions, so that answering eight
/// of them could not be expressed. The item stays per account. What the batch
/// gets instead is an `operation_id` in the completed document, which is where a
/// caller finds the shape of every other route it calls.
#[tokio::test]
async fn the_queue_asks_per_account_and_the_batch_is_discoverable_from_the_specification() {
    let harness = harness();

    let (status, savings) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Savings", "institution": "Northline" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{savings}");

    for item in transfer_partners_queue(&harness).await {
        assert_eq!(
            item["target"]["operationId"], "record_account_transfer_partners",
            "the queue names the per-account operation: {item}"
        );
        assert_eq!(
            item["target"]["path"], "/v1/accounts/{id}/transfer-partners",
            "{item}"
        );
    }

    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK, "{spec}");
    let batch = &spec["paths"]["/v1/accounts/transfer-partners"]["put"];
    assert_eq!(
        batch["operationId"], "record_account_transfer_partners_batch",
        "{spec}"
    );
    assert!(
        batch["requestBody"]["content"]["application/json"]["schema"]["$ref"]
            .as_str()
            .is_some_and(
                |reference| reference.ends_with("RecordAccountTransferPartnersBatchRequest")
            ),
        "{batch}"
    );
}

// ---------------------------------------------------------------------------
// Import sessions: a row that can say "I don't know" (iaam-3kru, iaam-6qsa)
// ---------------------------------------------------------------------------

/// A second account of the owner's, for answers that name one.
async fn another_account(harness: &Harness, title: &str) -> Uuid {
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": title, "institution": "Northline" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("account identifier")
}

/// A row whose direction the source did not give.
///
/// Invented from nothing: `INNER` is the shape of word a bank prints for a
/// movement it considers internal to itself, and every amount and label here was
/// made up for this test.
fn unresolved_row(account: Uuid, key: &str) -> Value {
    json!({
        "account": account,
        "type": "unresolved_direction",
        "amount": "2500.00",
        "currency": "RUB",
        "dates": { "cash_posted": "2025-03-18" },
        "source_category": "INNER",
        "idempotency_key": key,
    })
}

/// The same row with the source stating which way the money went.
///
/// Everything a rule matches on is unchanged — the word the source used is still
/// `INNER` — so a rule written for the row above matches this one too. What is
/// added is the one thing no rule can supply.
fn directed_row(account: Uuid, key: &str, direction: &str) -> Value {
    let mut row = unresolved_row(account, key);
    row["direction"] = json!(direction);
    row
}

async fn journal_rows(harness: &Harness) -> usize {
    let (status, page) = call(
        &harness.router,
        get("/v1/journal/events", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["rows"].as_array().expect("journal rows").len()
}

/// The question is a stored resource, not a sentence in a response body.
///
/// The response carrying it is deliberately thrown away: what is asserted is
/// that the question is still there afterwards, reachable from the session list
/// with its wording and the answers it admits — and that nothing was recorded
/// while it waits.
#[tokio::test]
async fn a_question_about_an_unresolved_row_outlives_the_response_that_carried_it() {
    let harness = harness();
    let account = harness.account.inner();
    let before = journal_rows(&harness).await;

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "march" },
                "operations": [unresolved_row(account, "inner-one")],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "needs_classification", "{verdicts}");

    // Everything the response said is now dropped, exactly as a caller that lost
    // it would have to manage.
    let (status, sessions) = call(
        &harness.router,
        get("/v1/import-sessions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sessions}");
    let session = sessions.as_array().expect("sessions")[0]["session"]
        .as_str()
        .expect("session identifier")
        .to_owned();

    let (status, contents) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{session}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contents}");
    assert_eq!(contents["state"], "open", "{contents}");
    assert_eq!(contents["unanswered"], 1, "{contents}");

    let question = &contents["questions"].as_array().expect("questions")[0];
    assert!(
        question["prompt"]
            .as_str()
            .is_some_and(|text| text.contains("which way")),
        "the stored question must say what is being asked: {question}"
    );
    let alternatives: Vec<&str> = question["alternatives"]
        .as_array()
        .expect("alternatives")
        .iter()
        .map(|entry| entry["answer"].as_str().expect("answer code"))
        .collect();
    assert!(
        alternatives.contains(&"sent_to_own_account")
            && alternatives.contains(&"received_from_own_account"),
        "a directionless row must offer both directions: {question}"
    );

    assert_eq!(
        journal_rows(&harness).await,
        before,
        "nothing may be recorded while the question waits"
    );
}

/// A count is named as a count, so no client reads it as the list beside it.
///
/// `rows` sat next to `questions`, which is a list of the same per-row shape,
/// and an external agent wrote `len(rows)` against it twice. The count is now
/// `row_count` in both places that publish one — the session's contents and the
/// assessment's source inventory — and the word `rows` appears in neither,
/// because a name a client can be wrong about is what caused the mistake.
#[tokio::test]
async fn a_session_publishes_its_row_count_under_a_name_no_client_can_index() {
    let harness = harness();
    let account = harness.account.inner();

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "march" },
                "operations": [unresolved_row(account, "counted-one")],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    let session = verdicts[0]["session_id"]
        .as_str()
        .expect("session identifier")
        .to_owned();

    let (status, contents) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{session}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contents}");
    assert_eq!(contents["row_count"], 1, "{contents}");
    assert!(
        contents.get("rows").is_none(),
        "a count must not answer to the name of the list it sits beside: {contents}"
    );

    let (status, plan) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{session}/assessment"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["source_inventory"]["row_count"], 1, "{plan}");
    assert!(
        plan["source_inventory"].get("rows").is_none(),
        "the inventory's count sits between two lists and must not read as a third: {plan}"
    );
}

/// The answer is a durable rule, and what the rule makes durable is **what the
/// row is** — not which way the next one runs.
///
/// Three submissions of the same shape under different keys. The first states no
/// direction and raises a question; the answer writes a rule. The second states
/// a direction, so the rule settles it and it reaches the journal without asking
/// anyone: that is the durability the rule exists for, and it covers every row a
/// source gives a direction for.
///
/// The third states no direction either, and it is asked again — not about what
/// it is, which the rule answers, but about which way it ran. A rule matches on
/// a counterparty, a payment purpose and the word the source used, and none of
/// those is a direction: two rows matching one rule run opposite ways, which is
/// exactly what money between two of the owner's own accounts does. Replaying
/// the direction of the row the rule was learned from would record half of them
/// backwards and ask nobody, which is iaam-xf49 generalised over every future
/// import.
#[tokio::test]
async fn answering_the_question_writes_a_rule_that_settles_what_the_next_row_is() {
    let harness = harness();
    let account = harness.account.inner();
    let savings = another_account(&harness, "Savings").await;

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "march" },
                "operations": [unresolved_row(account, "inner-one")],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    let session = verdicts[0]["session_id"]
        .as_str()
        .expect("session")
        .to_owned();
    let question = verdicts[0]["question_id"]
        .as_str()
        .expect("question")
        .to_owned();

    let (status, answered) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "sent_to_own_account", "account": savings }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert!(
        answered["rule"].is_string(),
        "the answer must be recorded as a rule: {answered}"
    );

    let (status, rules) = call(
        &harness.router,
        get("/v1/classification-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rules}");
    assert_eq!(
        rules.as_array().expect("rules").len(),
        1,
        "one answer, one rule: {rules}"
    );

    // The same shape again, this time with the source stating the direction. The
    // rule settles what it is, the source settles which way it went, and nothing
    // is left to ask — it is recorded as the transfer the owner named, not as a
    // deposit.
    let (status, again) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "april" },
                "operations": [directed_row(account, "inner-two", "out")],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_ne!(
        again[0]["verdict"], "needs_classification",
        "the rule must answer the second row without asking: {again}"
    );
    assert!(
        again[0]["event_id"].is_string(),
        "the settled row must reach the journal: {again}"
    );

    let (status, page) = call(
        &harness.router,
        get(
            "/v1/journal/events?idempotency_key=inner-two",
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        page["rows"][0]["kind"], "cash_transfer",
        "the owner answered «sent to my own account», so that is the fact: {page}"
    );

    // And a third row the source again gave no direction for. The rule still
    // says what it is; it cannot say which way this one ran, and answering that
    // out of the rule would be the guess (iaam-xf49). So it is asked — about the
    // direction, with both directions among the alternatives.
    let (status, third) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "may" },
                "operations": [unresolved_row(account, "inner-three")],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{third}");
    assert_eq!(
        third[0]["verdict"], "needs_classification",
        "a direction the source never stated is the owner's to give, every \
         time: {third}"
    );
    let alternatives: Vec<&str> = third[0]["alternatives"]
        .as_array()
        .expect("alternatives")
        .iter()
        .map(|entry| entry["answer"].as_str().expect("answer code"))
        .collect();
    assert!(
        alternatives.contains(&"sent_to_own_account")
            && alternatives.contains(&"received_from_own_account"),
        "and the question offers both, which is what makes it answerable: \
         {third}"
    );

    let (status, page) = call(
        &harness.router,
        get(
            "/v1/journal/events?idempotency_key=inner-three",
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "nothing may be recorded while the question waits: {page}"
    );
}

/// A session defers everything, and abandoning it leaves the journal untouched.
#[tokio::test]
async fn an_abandoned_import_session_writes_nothing_to_the_journal() {
    let harness = harness();
    let account = harness.account.inner();
    let before = journal_rows(&harness).await;

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": account, "channel": "file", "label": "march" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["session"].as_str().expect("session").to_owned();

    // A conclusive row and an unresolved one, in the same session. Neither is
    // recorded: a session holds both until it commits, which is what lets two
    // legs of one transfer be seen together.
    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({
                "operations": [
                    {
                        "account": account,
                        "type": "deposit",
                        "amount": "1000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-02" },
                        "idempotency_key": "held-deposit",
                    },
                    unresolved_row(account, "held-inner"),
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    // A held row is not given a verdict: a verdict answers "what was recorded",
    // and the answer here is "nothing, yet" — which is not `quarantined`, whose
    // published meaning is that no fact could be written from the row.
    assert_eq!(rows[0]["state"], "held", "{rows}");
    assert_eq!(rows[1]["state"], "needs_classification", "{rows}");
    assert!(rows[0]["verdict"].is_null(), "{rows}");
    assert_eq!(
        journal_rows(&harness).await,
        before,
        "a session records nothing before it commits: {rows}"
    );

    let (status, abandoned) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/abandon"),
            &harness.owner_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{abandoned}");
    assert_eq!(abandoned["state"], "abandoned", "{abandoned}");
    assert_eq!(
        journal_rows(&harness).await,
        before,
        "abandoning must leave the journal exactly as it was"
    );
}

/// An import is declared by what the statement prints, and walks to a commit
/// without the caller ever reading the directory.
///
/// This is the friction the route was reported for: opening a session used to
/// cost a `GET /v1/accounts` and a client-side join, once per account in the
/// export, before a single row could be sent. The account number is on the
/// statement; iaam already stores it; so the declaration takes it, and hands
/// back the identifier the rows need.
#[tokio::test]
async fn a_session_is_declared_by_the_identifier_the_source_prints() {
    let harness = harness();
    let created = account_with(
        &harness,
        &json!({
            "title": "Main",
            "institution": "Bank One",
            "provider": "bank-one",
            "provider_account_id": "acct-1",
        }),
    )
    .await;
    let before = journal_rows(&harness).await;

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": "acct-1", "channel": "file", "label": "march" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    assert_eq!(session["account"]["id"], created, "{session}");
    assert_eq!(
        session["account"]["title"], "Main",
        "the identifier comes back with the owner's own name beside it, so the \
         caller can see it reached the account it meant: {session}"
    );
    assert_eq!(session["account"]["institution"], "Bank One", "{session}");

    // Nothing else in this test is written by hand: the account the rows name is
    // the one the response just handed back.
    let id = session["session"].as_str().expect("session").to_owned();
    let account = session["account"]["id"].clone();
    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({
                "operations": [{
                    "account": account,
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-03-02" },
                    "idempotency_key": "declared-by-identifier",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows[0]["state"], "held", "{rows}");

    let (status, committed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert!(
        journal_rows(&harness).await > before,
        "the whole path was walked without a directory read: {committed}"
    );
}

/// A row names its account the way its statement prints it (`iaam-varx`).
///
/// The declaration has taken the source's identifier since it was widened, and
/// the row did not: `account` was a `Uuid`, so one flow answered «which account
/// is this» in two vocabularies and the caller had to copy an identifier out of
/// one response to say the same thing again on every row. Both ends now go
/// through the one directory, and a converter that never read `/v1/accounts` can
/// submit a batch.
#[tokio::test]
async fn a_row_names_its_account_by_the_identifier_the_source_prints() {
    let harness = harness();
    let created = account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "acct-1",
        }),
    )
    .await;
    let before = journal_rows(&harness).await;

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": "acct-1", "channel": "file", "label": "march" },
                "operations": [{
                    "account": "acct-1",
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-03-02" },
                    "idempotency_key": "row-named-by-identifier",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "accepted", "{verdicts}");
    assert!(
        journal_rows(&harness).await > before,
        "the row was recorded without the caller ever naming a uuid: {verdicts}"
    );

    // And the account it landed on is the one the identifier names, not a
    // second account minted from the string.
    let (status, accounts) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accounts}");
    assert_eq!(
        accounts.as_array().expect("accounts").len(),
        1,
        "{accounts}"
    );
    assert_eq!(accounts[0]["id"], created, "{accounts}");
}

/// A row naming no account of the owner's is one rejected row, not a rejected
/// request.
///
/// The field used to be a `Uuid`, so a value that named nothing failed the whole
/// body at deserialisation and the readable rows beside it were never judged.
/// §10.1 says an unreadable operation is one row's problem, and this is now one:
/// the rejection carries `account`, and the row beside it is recorded.
#[tokio::test]
async fn a_row_naming_no_account_is_rejected_beside_rows_that_are_not() {
    let harness = harness();
    account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "acct-1",
        }),
    )
    .await;

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "operations": [
                    {
                        "account": "acct-1",
                        "type": "deposit",
                        "amount": "1000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-02" },
                        "idempotency_key": "readable-row",
                    },
                    {
                        "account": "an-account-he-never-declared",
                        "type": "deposit",
                        "amount": "2000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-03" },
                        "idempotency_key": "row-naming-nothing",
                    },
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "accepted", "{verdicts}");
    assert_eq!(verdicts[1]["verdict"], "rejected", "{verdicts}");
    assert_eq!(verdicts[1]["field"], "account", "{verdicts}");
    assert!(
        verdicts[1]["actual"]
            .as_str()
            .is_some_and(|actual| actual.contains("an-account-he-never-declared")),
        "the refusal quotes what arrived, so the owner can see which string \
         reached nothing: {verdicts}"
    );
}

/// The identifier and iaam's own name for the account reach one session.
///
/// Two declarations of one import must not open two sessions holding half the
/// answers each, and the source and import keys are derived from the resolved
/// account — so a caller that switches vocabularies mid-import finds the session
/// it already had.
#[tokio::test]
async fn a_declaration_by_identifier_and_by_uuid_reach_the_same_session() {
    let harness = harness();
    let created = account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "acct-1",
        }),
    )
    .await;

    let mut opened = Vec::new();
    for named in [json!("acct-1"), json!(created)] {
        let (status, session) = call(
            &harness.router,
            post(
                "/v1/import-sessions",
                &harness.owner_token,
                &json!({ "source": { "account": named, "channel": "file", "label": "march" } }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{session}");
        assert_eq!(session["account"]["id"], created, "{session}");
        opened.push(session["session"].clone());
    }
    assert_eq!(
        opened[0], opened[1],
        "one import has one open session, however the account was named"
    );
}

/// An import already under way is named, not silently continued.
///
/// One import has one open session, and re-declaring it used to answer
/// `201 Created` with the session that already existed — indistinguishable from
/// one just made. A caller that reused a label for a different file had its rows
/// join the earlier ones, and the commit was then refused over questions raised
/// by rows it had never sent. Nothing said which session was in the way and
/// nothing said it could be thrown away; the owner found `abandon` by
/// experiment.
///
/// The empty case still hands the session back: that is a caller retrying the
/// open call, and there is nothing in an empty session to mix a statement into.
#[tokio::test]
async fn a_declared_import_that_already_holds_rows_is_refused_and_the_refusal_names_the_session() {
    let harness = harness();
    let account = account_with(
        &harness,
        &json!({ "title": "Main", "provider": "bank-one", "provider_account_id": "acct-1" }),
    )
    .await;
    let declaration =
        json!({ "source": { "account": "acct-1", "channel": "file", "label": "march" } });

    let (status, opened) = call(
        &harness.router,
        post("/v1/import-sessions", &harness.owner_token, &declaration),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{opened}");
    let session = opened["session"].as_str().expect("session").to_owned();

    // Nothing fed yet, so the same declaration is the same open call.
    let (status, again) = call(
        &harness.router,
        post("/v1/import-sessions", &harness.owner_token, &declaration),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    assert_eq!(
        again["session"], opened["session"],
        "one import has one open session: {again}"
    );

    let account = Uuid::parse_str(&account).expect("account uuid");
    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/rows"),
            &harness.owner_token,
            &json!({ "operations": [unresolved_row(account, "march-1")] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows[0]["state"], "needs_classification", "{rows}");

    let (status, refused) = call(
        &harness.router,
        post("/v1/import-sessions", &harness.owner_token, &declaration),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a statement half imported is not silently continued: {refused}"
    );
    assert_eq!(refused["code"], "invalid_request", "{refused}");
    assert_eq!(refused["field"], "source.label", "{refused}");
    assert_eq!(refused["pointer"], "/source/label", "{refused}");
    let actual = refused["actual"].as_str().expect("what stands in the way");
    assert!(
        actual.contains(&session),
        "the refusal must name the session standing in the way: {refused}"
    );
    assert!(
        actual.contains("1 rows") && actual.contains("1 unanswered"),
        "and say what it holds: {refused}"
    );

    // Both calls that end the session, and answering leads because a session
    // waiting on a question cannot be committed.
    let offered: Vec<&str> = refused["resolutions"]
        .as_array()
        .expect("the calls that end it")
        .iter()
        .map(|option| option["operationId"].as_str().expect("operation"))
        .collect();
    assert_eq!(
        offered,
        ["answer_import_question", "abandon_import_session"],
        "{refused}"
    );
    let abandon = &refused["resolutions"][1];
    assert_eq!(abandon["method"], "POST", "{refused}");
    assert_eq!(
        abandon["path"], "/v1/import-sessions/{session}/abandon",
        "{refused}"
    );
    assert_eq!(
        abandon["request"]["preset"]["session"],
        json!(session),
        "{refused}"
    );
    assert!(
        abandon.get("requestSchema").is_none(),
        "abandoning takes no body, and the refusal must not invent one: {refused}"
    );

    // Abandoning is the way out, and the declaration works again afterwards.
    let (status, abandoned) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/abandon"),
            &harness.owner_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{abandoned}");
    let (status, fresh) = call(
        &harness.router,
        post("/v1/import-sessions", &harness.owner_token, &declaration),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{fresh}");
    assert_ne!(
        fresh["session"], opened["session"],
        "the abandoned session is not reopened: {fresh}"
    );
}

/// With every question answered, committing is what the refusal leads with.
#[tokio::test]
async fn a_settled_import_under_way_is_refused_with_the_commit_that_ends_it() {
    let harness = harness();
    let account = harness.account.inner();
    let declaration =
        json!({ "source": { "account": account, "channel": "file", "label": "march" } });

    let (status, opened) = call(
        &harness.router,
        post("/v1/import-sessions", &harness.owner_token, &declaration),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{opened}");
    let session = opened["session"].as_str().expect("session").to_owned();

    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/rows"),
            &harness.owner_token,
            &json!({ "operations": [{
                "account": account,
                "type": "deposit",
                "amount": "1000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-02" },
                "idempotency_key": "settled-one",
            }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows[0]["state"], "held", "{rows}");

    let (status, refused) = call(
        &harness.router,
        post("/v1/import-sessions", &harness.owner_token, &declaration),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    let offered: Vec<&str> = refused["resolutions"]
        .as_array()
        .expect("the calls that end it")
        .iter()
        .map(|option| option["operationId"].as_str().expect("operation"))
        .collect();
    assert_eq!(
        offered,
        ["commit_import_session", "abandon_import_session"],
        "a session waiting on nobody is committed, not answered: {refused}"
    );
}

/// A card is an identifier too, and the interval on it does not gate the file.
#[tokio::test]
async fn a_session_is_declared_by_an_alias() {
    let harness = harness();
    let created = account_with(
        &harness,
        &json!({
            "title": "Savings",
            "aliases": [{ "value": "card-one", "valid_from": "2024-01-01", "valid_to": "2025-03-01" }],
        }),
    )
    .await;

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": "card-one", "channel": "file", "label": "march" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    assert_eq!(
        session["account"]["id"], created,
        "a declaration is about a file, not about a day: the interval decides on \
         the rows, each against its own date: {session}"
    );
}

/// Two accounts answering to one identifier are refused, not picked between.
#[tokio::test]
async fn an_ambiguous_declared_account_is_refused_and_the_refusal_says_why() {
    let harness = harness();
    let first = account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "shared-1",
        }),
    )
    .await;
    let second = account_with(
        &harness,
        &json!({
            "title": "Savings",
            "aliases": [{ "value": "shared-1", "valid_from": "2024-01-01" }],
        }),
    )
    .await;

    let (status, refusal) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": "shared-1", "channel": "file", "label": "march" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");
    assert_eq!(refusal["code"], "invalid_request", "{refusal}");
    assert_eq!(refusal["field"], "source.account", "{refusal}");
    let actual = refusal["actual"].as_str().expect("what was ambiguous");
    for named in [&first, &second, "Main", "Savings"] {
        assert!(
            actual.contains(named),
            "the refusal must name what was ambiguous ({named}): {actual}"
        );
    }
    assert!(
        refusal["expected"]
            .as_str()
            .is_some_and(|expected| expected.contains("provider_account_id")),
        "and what would disambiguate it: {refusal}"
    );

    let (status, sessions) = call(
        &harness.router,
        get("/v1/import-sessions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sessions}");
    assert!(
        sessions.as_array().expect("sessions").is_empty(),
        "a refused declaration opens nothing: {sessions}"
    );
}

/// Commit refuses while a question is open, and writes once when it is answered.
#[tokio::test]
async fn a_session_commits_only_after_every_question_has_been_answered() {
    let harness = harness();
    let account = harness.account.inner();
    let savings = another_account(&harness, "Savings").await;
    let before = journal_rows(&harness).await;

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": account, "channel": "file", "label": "march" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["session"].as_str().expect("session").to_owned();

    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({ "operations": [unresolved_row(account, "session-inner")] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    let question = rows[0]["question_id"]
        .as_str()
        .expect("question")
        .to_owned();

    let (status, refused) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|text| text.contains("answered")),
        "the refusal must say what is missing: {refused}"
    );
    assert_eq!(journal_rows(&harness).await, before);

    let (status, answered) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "received_from_own_account", "account": savings }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert_eq!(
        journal_rows(&harness).await,
        before,
        "an answer settles the row; commit is what records it"
    );

    let (status, committed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(committed["state"], "committed", "{committed}");
    assert_eq!(
        committed["rows"][0]["verdict"], "provisional",
        "{committed}"
    );
    assert_eq!(
        journal_rows(&harness).await,
        before + 1,
        "commit writes what the session held, once"
    );

    // And a committed session takes nothing more.
    let (status, closed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({ "operations": [unresolved_row(account, "session-late")] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{closed}");
}

/// An answer the question never offered is refused before anything is written.
#[tokio::test]
async fn an_answer_the_question_does_not_admit_is_refused() {
    let harness = harness();
    let account = harness.account.inner();

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "march" },
                "operations": [unresolved_row(account, "inner-one")],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    let session = verdicts[0]["session_id"]
        .as_str()
        .expect("session")
        .to_owned();
    let question = verdicts[0]["question_id"]
        .as_str()
        .expect("question")
        .to_owned();

    let (status, refused) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "sent_to_own_account" }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an answer naming no account cannot name an account: {refused}"
    );

    let (status, refused) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "yes" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");

    // The question is still open: a refused answer settles nothing.
    let (status, contents) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{session}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contents}");
    assert_eq!(contents["unanswered"], 1, "{contents}");
}

/// A caller that has concluded is still right to say so.
///
/// The conclusive route keeps every shape it had: this is the regression the
/// observation shape must not cause.
#[tokio::test]
async fn a_concluded_row_is_recorded_exactly_as_it_was_before_sessions_existed() {
    let harness = harness();
    let account = harness.account.inner();

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "operations": [{
                    "account": account,
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-03-02" },
                    "idempotency_key": "plain-deposit",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "provisional", "{verdicts}");
    assert!(verdicts[0]["session_id"].is_null(), "{verdicts}");

    let (status, sessions) = call(
        &harness.router,
        get("/v1/import-sessions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sessions}");
    assert!(
        sessions.as_array().expect("sessions").is_empty(),
        "a batch that raised no question must open no session: {sessions}"
    );
}

/// Every schema the contract points at is a schema the contract defines.
///
/// A `$ref` to a component that does not exist is a document a generator cannot
/// read, and nothing else here would notice: the routes still answer.
#[tokio::test]
async fn every_schema_reference_in_the_contract_resolves() {
    let harness = harness();
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK);

    let defined = spec["components"]["schemas"]
        .as_object()
        .expect("the contract defines schemas");
    let mut dangling = Vec::new();
    collect_refs(&spec, &mut |reference| {
        if let Some(name) = reference.strip_prefix("#/components/schemas/")
            && !defined.contains_key(name)
        {
            dangling.push(name.to_owned());
        }
    });
    dangling.sort_unstable();
    dangling.dedup();
    assert!(
        dangling.is_empty(),
        "unresolved schema references: {dangling:?}"
    );
}

fn collect_refs(value: &Value, found: &mut impl FnMut(&str)) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                if key == "$ref"
                    && let Some(reference) = entry.as_str()
                {
                    found(reference);
                }
                collect_refs(entry, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, found);
            }
        }
        _ => {}
    }
}

/// Every open classification question is an item in `/v1/actions`.
///
/// The ingest response is discarded on purpose, because that is the defect: a
/// question that lives only in a response body is invisible once the body is
/// gone. Everything below — the session, the question, the shapes it admits and
/// the route that answers it — is taken from the queue alone.
#[tokio::test]
async fn an_open_classification_question_is_an_item_in_the_action_queue() {
    let harness = harness();
    let account = harness.account.inner();
    let savings = another_account(&harness, "Savings").await;

    let (status, raised) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "march" },
                // Two rows, so a second, unrelated question is a second item.
                // Both name the declared source account: a batch may not carry
                // rows for an account its source did not declare.
                "operations": [
                    unresolved_row(account, "queue-one"),
                    unresolved_row(account, "queue-two"),
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{raised}");
    drop(raised);

    let items = open_question_items(&harness).await;
    assert_eq!(items.len(), 2, "one item per open question: {items:?}");
    let identities: std::collections::BTreeSet<&str> = items
        .iter()
        .map(|item| item["id"].as_str().expect("an identity"))
        .collect();
    assert_eq!(identities.len(), 2, "one identity each: {items:?}");
    for item in &items {
        assert_eq!(
            item["subject"]["id"],
            json!(account),
            "the item names the account the row is on: {item}"
        );
    }
    let item = items[0].clone();

    assert_eq!(item["kind"], "answer_classification_question", "{item}");
    assert_eq!(item["category"], "required_for_goal", "{item}");
    // A row held unclassified is in no journal, so it is in no report: this one
    // stands in the way of all four, and a client comparing it against the
    // transfer item above can see that the two are not the same demand.
    assert_eq!(
        item["goals"],
        json!(["asset_snapshot", "money_flow", "returns", "reconciliation"]),
        "{item}"
    );
    assert_eq!(item["state"], "needs_owner_input", "{item}");
    assert_eq!(item["required_scope"], "agent", "{item}");
    assert_eq!(item["subject"]["type"], "account", "{item}");

    // The operation that answers it, so the item is actionable and not a notice.
    let target = &item["target"];
    assert_eq!(target["type"], "operation", "{item}");
    assert_eq!(target["operationId"], "answer_import_question", "{item}");
    assert_eq!(target["method"], "POST", "{item}");
    assert_eq!(
        target["path"], "/v1/import-sessions/{session}/questions/{question}/answer",
        "{item}"
    );

    // The typed answer shapes, and the account the two of them that need one need.
    let answer = target["request"]["missing"]
        .as_array()
        .expect("missing fields")
        .iter()
        .find(|missing| missing["pointer"] == "/answer")
        .expect("the answer field");
    let shapes: Vec<&str> = answer["alternatives"]
        .as_array()
        .expect("alternatives")
        .iter()
        .map(|alternative| alternative["value"].as_str().expect("a value"))
        .collect();
    assert_eq!(
        shapes,
        vec![
            "sent_to_own_account",
            "received_from_own_account",
            "paid",
            "received",
            "fee",
            "income",
        ],
        "{item}"
    );
    for alternative in answer["alternatives"].as_array().expect("alternatives") {
        let requires = alternative["requires"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        match alternative["value"].as_str().expect("a value") {
            "sent_to_own_account" | "received_from_own_account" => {
                assert_eq!(requires[0]["pointer"], "/account", "{alternative}");
                let offered: Vec<&str> = requires[0]["candidates"]
                    .as_array()
                    .expect("the owner's other accounts")
                    .iter()
                    .map(|candidate| candidate["id"].as_str().expect("an identifier"))
                    .collect();
                assert_eq!(
                    offered,
                    vec![savings.to_string().as_str()],
                    "an account is not the other side of itself: {alternative}"
                );
            }
            _ => assert!(requires.is_empty(), "{alternative}"),
        }
    }

    // Answering removes its own item, and only its own.
    let session = target["request"]["preset"]["session"]
        .as_str()
        .expect("the session is preset");
    let question = target["request"]["preset"]["question"]
        .as_str()
        .expect("the question is preset");
    let (status, answered) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "paid" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");

    let left = open_question_items(&harness).await;
    assert_eq!(left.len(), 1, "{left:?}");
    assert_ne!(left[0]["id"], item["id"], "{left:?}");
}

/// The queue's items for open classification questions, and nothing else.
async fn open_question_items(harness: &Harness) -> Vec<Value> {
    let (status, actions) = call(
        &harness.router,
        get("/v1/actions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{actions}");
    actions
        .as_array()
        .expect("action items")
        .iter()
        .filter(|item| item["kind"] == "answer_classification_question")
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// The assessment, and the revision commit checks (iaam-k1xa)
// ---------------------------------------------------------------------------

/// The session says where its own assessment is, and the path it gives answers.
///
/// The gap this closes (iaam-51c0): the assessment answered, section by section,
/// what a reviewer asked for as a wishlist — he ran a whole import without
/// finding it. The queue could not lead him there, and still cannot: an action's
/// target is an `OperationKey`, `ActionCatalog::from_openapi` demands a JSON
/// request schema of every key it registers, and `assess_import_session` is a
/// GET with no request body. So the link lives on the session, where a client
/// that has one already looks — on every response built from
/// `ImportSessionDto`, which is the open response, the list, the contents and
/// the commit outcome alike.
///
/// The path is followed rather than merely matched: a published link that does
/// not answer is worse than none, and asserting its spelling alone would not
/// have noticed.
#[tokio::test]
async fn a_session_publishes_the_path_to_its_own_assessment() {
    let harness = harness();
    let account = harness.account.inner();

    let (status, opened) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": account, "channel": "file", "label": "linked" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{opened}");
    let id = opened["session"].as_str().expect("session").to_owned();
    let link = opened["assessment"]
        .as_str()
        .unwrap_or_else(|| panic!("the open response names no assessment: {opened}"))
        .to_owned();
    assert_eq!(link, format!("/v1/import-sessions/{id}/assessment"));

    let (status, plan) = call(&harness.router, get(&link, Some(&harness.owner_token))).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert!(plan["revision"].is_string(), "{plan}");
    // The one field the commit route takes whose only source is this answer.
    assert_eq!(plan["session"], json!(id), "{plan}");

    // The same link on the responses a client reaches later, so a caller that
    // lost the open response is not sent back to the specification.
    let (status, listed) = call(
        &harness.router,
        get("/v1/import-sessions", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(
        listed.as_array().expect("sessions")[0]["assessment"],
        json!(link),
        "{listed}"
    );

    let (status, contents) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{id}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contents}");
    assert_eq!(contents["assessment"], json!(link), "{contents}");
}

// ---------------------------------------------------------------------------
// The source's own control section, checked before the commit (iaam-jc3y)
// ---------------------------------------------------------------------------

/// Open a session against `account` and feed it `rows`, returning its identifier.
async fn session_holding(harness: &Harness, account: Uuid, label: &str, rows: Value) -> String {
    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": account, "channel": "file", "label": label } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["session"].as_str().expect("session").to_owned();

    let (status, held) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({ "operations": rows }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{held}");
    id
}

/// State one control section on a session and assert it was taken.
async fn state_control_figures(harness: &Harness, session: &str, body: &Value) {
    let (status, stated) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/control-figures"),
            &harness.owner_token,
            body,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stated}");
}

async fn assessment_of(harness: &Harness, session: &str) -> Value {
    let (status, plan) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{session}/assessment"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    plan
}

/// The check one figure of one comparison came to.
fn check_of<'a>(plan: &'a Value, figure: &str) -> &'a Value {
    plan["control_reconciliation"]["comparisons"]
        .as_array()
        .expect("comparisons")
        .iter()
        .flat_map(|comparison| comparison["checks"].as_array().expect("checks"))
        .find(|check| check["figure"] == json!(figure))
        .unwrap_or_else(|| panic!("no check for {figure}: {plan}"))
}

/// The transcription of a control section is refused where it is a transcription
/// mistake, and taken where it is a finding (iaam-jc3y).
///
/// Four refusals, each because the transcriber is the only one who can fix it: a
/// section stating nothing would read as «checked» to anybody skimming; one
/// account and currency stated twice would let the store keep whichever came
/// last and the caller never learn its two readings disagreed; a signed turnover
/// is a sign convention misread, and every comparison built on it would be
/// nonsense; an inverted interval reconciles with nothing, forever.
///
/// A **negative balance** is not among them: §11 says an overdrawn account is a
/// valid state, and refusing one would refuse the statement that reports it.
///
/// Every account and amount is invented (CLAUDE.md).
#[tokio::test]
async fn a_control_section_is_refused_where_it_is_a_transcription_mistake() {
    let harness = harness();
    let account = another_account(&harness, "Main").await;
    let session = session_holding(&harness, account, "refusals", json!([])).await;

    let refuse = async |body: Value, why: &str| {
        let (status, refusal) = call(
            &harness.router,
            post(
                &format!("/v1/import-sessions/{session}/control-figures"),
                &harness.owner_token,
                &body,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}: {refusal}");
    };

    refuse(
        json!({
            "from": "2025-06-01",
            "to": "2025-06-30",
            "figures": [{ "account": account, "currency": "RUB" }],
        }),
        "a section stating no figure",
    )
    .await;

    refuse(
        json!({
            "from": "2025-06-01",
            "to": "2025-06-30",
            "figures": [
                { "account": account, "currency": "RUB", "closing": "100.00" },
                { "account": account, "currency": "RUB", "closing": "120.00" },
            ],
        }),
        "one account and currency stated twice in one call",
    )
    .await;

    refuse(
        json!({
            "from": "2025-06-01",
            "to": "2025-06-30",
            "figures": [{
                "account": account,
                "currency": "RUB",
                "credit_turnover": "-50.00",
            }],
        }),
        "a turnover carries no sign, the side does",
    )
    .await;

    refuse(
        json!({
            "from": "2025-06-30",
            "to": "2025-06-01",
            "figures": [{ "account": account, "currency": "RUB", "closing": "100.00" }],
        }),
        "an interval that ends before it starts",
    )
    .await;

    // And the overdrawn account, which is a fact rather than a mistake.
    let (status, stated) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/control-figures"),
            &harness.owner_token,
            &json!({
                "from": "2025-06-01",
                "to": "2025-06-30",
                "figures": [{
                    "account": account,
                    "currency": "RUB",
                    "opening": "0.00",
                    "closing": "-40.00",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stated}");
    assert_eq!(stated[0]["closing"], "-40.00", "{stated}");

    // Restating it replaces it: a transcription corrected is a correction, not a
    // second section for the assessment to choose between.
    let (status, restated) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/control-figures"),
            &harness.owner_token,
            &json!({
                "from": "2025-06-01",
                "to": "2025-06-30",
                "figures": [{
                    "account": account,
                    "currency": "RUB",
                    "opening": "0.00",
                    "closing": "-45.00",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restated}");
    assert_eq!(
        restated.as_array().expect("sections").len(),
        1,
        "{restated}"
    );
    assert_eq!(restated[0]["closing"], "-45.00", "{restated}");

    // And the session reports what it holds, so a caller that stated figures
    // onto the wrong session finds out by reading it back.
    let (status, contents) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{session}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contents}");
    assert_eq!(
        contents["control_figures"]
            .as_array()
            .expect("control figures")
            .len(),
        1,
        "{contents}"
    );
}

/// A converter that mirrored both legs of an internal transfer is caught on the
/// first attempt rather than the fifth (iaam-jc3y).
///
/// The real failure: an operator's converter emitted the far leg of every
/// internal transfer onto the near account as well, so every account was
/// inflated by the sum of its own transfers. Every verdict was positive, the
/// journal took it all, and the discrepancy surfaced only when a report was read
/// weeks later — because at commit nothing knew what right looked like, while
/// the statement had printed its turnover on the same page as the rows.
///
/// Note which check catches it: the **turnover**. The mirrored leg here inflates
/// only what arrived, so a system that compared closing balances alone and not
/// both sides would have to see the net wrong before it saw anything at all.
///
/// Every account and amount is invented (CLAUDE.md).
#[tokio::test]
async fn a_mirrored_transfer_leg_fails_the_turnover_the_statement_printed() {
    let harness = harness();
    let account = another_account(&harness, "Main").await;
    let session = session_holding(
        &harness,
        account,
        "mirrored",
        json!([
            {
                "account": account,
                "type": "deposit",
                "amount": "1000.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-05" },
                "idempotency_key": "mirrored-in",
            },
            {
                "account": account,
                "type": "withdrawal",
                "amount": "500.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-10" },
                "idempotency_key": "mirrored-out",
            },
            // The far leg of an internal transfer, written onto this account as
            // well: the whole of the defect, in one row.
            {
                "account": account,
                "type": "deposit",
                "amount": "300.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-12" },
                "idempotency_key": "mirrored-far-leg",
            },
        ]),
    )
    .await;

    state_control_figures(
        &harness,
        &session,
        &json!({
            "from": "2025-03-01",
            "to": "2025-03-31",
            "figures": [{
                "account": account,
                "currency": "RUB",
                "opening": "0.00",
                "closing": "500.00",
                "debit_turnover": "1000.00",
                "credit_turnover": "500.00",
            }],
        }),
    )
    .await;

    let plan = assessment_of(&harness, &session).await;
    assert_eq!(
        plan["readiness"], "does_not_reconcile",
        "neither an unreadable row nor a decision of the owner's: {plan}"
    );
    assert_eq!(
        plan["control_reconciliation"]["mismatched_figures"], 2,
        "{plan}"
    );

    let debit = check_of(&plan, "debit_turnover");
    assert_eq!(debit["outcome"], "mismatched", "{plan}");
    assert_eq!(debit["claimed"], "1000.00", "{plan}");
    assert_eq!(debit["observed"], "1300.00", "the mirrored leg: {plan}");
    assert_eq!(debit["delta"], "-300.00", "{plan}");

    // The side the mirror did not touch still agrees, and says so with both
    // numbers on the page: «matched» without them would not say what it
    // compared.
    let credit = check_of(&plan, "credit_turnover");
    assert_eq!(credit["outcome"], "matched", "{plan}");
    assert_eq!(credit["claimed"], "500.00", "{plan}");
    assert_eq!(credit["observed"], "500.00", "{plan}");

    let closing = check_of(&plan, "closing_balance");
    assert_eq!(closing["outcome"], "mismatched", "{plan}");
    assert_eq!(closing["observed"], "800.00", "{plan}");

    // Committing is refused, and the refusal names the figures rather than
    // saying that something is wrong somewhere.
    let before = journal_rows(&harness).await;
    let (status, refusal) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/commit"),
            &harness.owner_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");
    let detail = refusal.to_string();
    assert!(detail.contains("debit_turnover"), "{refusal}");
    assert!(detail.contains("1000.00"), "{refusal}");
    assert!(detail.contains("1300.00"), "{refusal}");
    assert_eq!(
        journal_rows(&harness).await,
        before,
        "a refused commit writes nothing: {refusal}"
    );

    // And it stays possible, because a statement can itself be wrong — as a
    // stated act rather than the default.
    let (status, committed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/commit"),
            &harness.owner_token,
            &json!({ "accept_control_mismatch": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(
        committed["control_assertions"]
            .as_array()
            .expect("control assertions")
            .len(),
        3,
        "two balances and one turnover, written as the assertions they are: {committed}"
    );
}

/// A converter emitting minor units where the rest emit major misses the closing
/// balance by three orders of magnitude, and is told so before it commits
/// (iaam-jc3y).
///
/// The second real failure, and the one the assessment could not have caught by
/// any means it already had: every row is well formed, every account is known,
/// nothing is ambiguous, and the whole batch is a hundred times too large. The
/// only thing in the world that knew otherwise was the figure printed at the
/// bottom of the statement the rows came from.
///
/// Every account and amount is invented (CLAUDE.md).
#[tokio::test]
async fn an_import_off_by_a_factor_of_a_hundred_misses_the_closing_balance() {
    let harness = harness();
    let account = another_account(&harness, "Main").await;
    let session = session_holding(
        &harness,
        account,
        "minor-units",
        json!([{
            "account": account,
            "type": "deposit",
            // The converter sent the minor-unit figure through the major-unit
            // field.
            "amount": "150000.00",
            "currency": "RUB",
            "dates": { "cash_posted": "2025-04-05" },
            "idempotency_key": "minor-units-in",
        }]),
    )
    .await;

    state_control_figures(
        &harness,
        &session,
        &json!({
            "from": "2025-04-01",
            "to": "2025-04-30",
            "figures": [{
                "account": account,
                "currency": "RUB",
                "opening": "0.00",
                "closing": "1500.00",
                "debit_turnover": "1500.00",
            }],
        }),
    )
    .await;

    let plan = assessment_of(&harness, &session).await;
    assert_eq!(plan["readiness"], "does_not_reconcile", "{plan}");

    let closing = check_of(&plan, "closing_balance");
    assert_eq!(closing["outcome"], "mismatched", "{plan}");
    assert_eq!(closing["claimed"], "1500.00", "{plan}");
    assert_eq!(closing["observed"], "150000.00", "{plan}");
    assert_eq!(closing["delta"], "-148500.00", "{plan}");

    // The credit side was never printed, so nothing is claimed about it and
    // nothing is reported: a figure the source did not state is not a figure
    // that failed.
    assert!(
        plan["control_reconciliation"]["comparisons"][0]["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .all(|check| check["figure"] != json!("credit_turnover")),
        "{plan}"
    );
}

/// A batch that agrees with its source is ready, says what it compared, and
/// leaves the reconciliation already made (iaam-jc3y).
///
/// Three properties in one scenario, because they are one property: the figures
/// the assessment checked are the figures the commit writes. After it, the
/// reconciliation the owner would otherwise have had to make separately — open a
/// second route, retype the same numbers, against a journal that already holds
/// whatever went wrong — is in the journal and reports `matched`.
///
/// Every account and amount is invented (CLAUDE.md).
#[tokio::test]
async fn a_batch_that_agrees_with_its_source_commits_the_reconciliation_with_it() {
    let harness = harness();
    let account = another_account(&harness, "Savings").await;
    let session = session_holding(
        &harness,
        account,
        "agreeing",
        json!([
            {
                "account": account,
                "type": "deposit",
                "amount": "200.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-05-04" },
                "idempotency_key": "agreeing-in",
            },
            {
                "account": account,
                "type": "withdrawal",
                "amount": "50.00",
                "currency": "RUB",
                "dates": { "cash_posted": "2025-05-06" },
                "idempotency_key": "agreeing-out",
            },
        ]),
    )
    .await;

    state_control_figures(
        &harness,
        &session,
        &json!({
            "from": "2025-05-01",
            "to": "2025-05-31",
            "figures": [{
                "account": account,
                "currency": "RUB",
                "opening": "0.00",
                "closing": "150.00",
                "debit_turnover": "200.00",
                "credit_turnover": "50.00",
            }],
        }),
    )
    .await;

    let plan = assessment_of(&harness, &session).await;
    assert_eq!(plan["readiness"], "ready", "{plan}");
    assert_eq!(
        plan["control_reconciliation"]["mismatched_figures"], 0,
        "{plan}"
    );
    for figure in ["debit_turnover", "credit_turnover", "closing_balance"] {
        assert_eq!(check_of(&plan, figure)["outcome"], "matched", "{plan}");
    }

    let (status, committed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/commit"),
            &harness.owner_token,
            // No flag: a batch that adds up commits the way it always did.
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    let assertions = committed["control_assertions"]
        .as_array()
        .expect("control assertions");
    assert_eq!(assertions.len(), 3, "{committed}");
    assert!(
        assertions
            .iter()
            .all(|assertion| assertion["outcome"] == json!("inserted")),
        "{committed}"
    );

    let (status, reconciliation) = call(
        &harness.router,
        get(
            &format!("/v1/reconciliation?account={account}&from=2025-05-01&to=2025-05-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reconciliation}");
    let outcomes = reconciliation["statuses"][0]["outcomes"]
        .as_array()
        .expect("assertion outcomes");
    assert_eq!(
        outcomes.len(),
        3,
        "the section the import stated, reconciled against the journal it wrote: {reconciliation}"
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome["outcome"]["code"] == json!("matched")),
        "{reconciliation}"
    );
}

/// The commit delta totals its rows, per account and currency (iaam-o1ni).
///
/// The defect: `commit_delta` published three lists, each row carrying a signed
/// amount and a currency, and no sum anywhere. An operator checking a
/// two-hundred-row import against the one figure printed on his statement had to
/// add two hundred decimal strings on the client — and the client of this API is
/// a language model, in a system that deliberately keeps money arithmetic inside
/// the core.
///
/// Every amount and account here is invented (CLAUDE.md).
#[tokio::test]
async fn the_commit_delta_totals_its_rows_per_account_and_currency() {
    let harness = harness();
    let main = harness.account.inner();
    let savings = another_account(&harness, "Savings").await;

    // One row committed on its own first, so the session below holds a row the
    // journal already has: a duplicate totals separately from a fact, and the
    // two must not be summed together.
    let (status, recorded) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "operations": [{
                    "account": main,
                    "type": "deposit",
                    "amount": "400.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-03-02" },
                    "idempotency_key": "totalled-already-held",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": main, "channel": "file", "label": "totalled" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["session"].as_str().expect("session").to_owned();

    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({
                "operations": [
                    {
                        "account": main,
                        "type": "deposit",
                        "amount": "1000.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-03" },
                        "idempotency_key": "totalled-in",
                    },
                    {
                        "account": main,
                        "type": "withdrawal",
                        "amount": "250.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-04" },
                        "idempotency_key": "totalled-out",
                    },
                    {
                        "account": savings,
                        "type": "deposit",
                        "amount": "60.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-05" },
                        "idempotency_key": "totalled-savings",
                    },
                    {
                        "account": main,
                        "type": "deposit",
                        "amount": "400.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-02" },
                        "idempotency_key": "totalled-already-held",
                    },
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");

    let (status, plan) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{id}/assessment"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");

    let totals = plan["commit_delta"]["fact_totals"]
        .as_array()
        .expect("fact totals");
    assert_eq!(
        totals.len(),
        2,
        "one total per account and currency: {plan}"
    );
    let on_main = totals
        .iter()
        .find(|total| total["account"] == json!(main.to_string()))
        .expect("a total for the declared account");
    assert_eq!(on_main["rows"], 2, "{plan}");
    assert_eq!(on_main["debit"], "1000.00", "{plan}");
    assert_eq!(on_main["credit"], "250.00", "both sides positive: {plan}");
    assert_eq!(on_main["net"], "750.00", "{plan}");
    assert_eq!(on_main["currency"], "RUB", "{plan}");

    let on_savings = totals
        .iter()
        .find(|total| total["account"] == json!(savings.to_string()))
        .expect("a total for the second account");
    assert_eq!(on_savings["debit"], "60.00", "{plan}");
    assert_eq!(
        on_savings["net"], "60.00",
        "two accounts are never folded into one figure: {plan}"
    );

    // The row the journal already holds is totalled apart. Folded in with the
    // facts it would say the import adds money it adds nothing of.
    let duplicates = plan["commit_delta"]["duplicate_totals"]
        .as_array()
        .expect("duplicate totals");
    assert_eq!(duplicates.len(), 1, "{plan}");
    assert_eq!(duplicates[0]["account"], json!(main.to_string()), "{plan}");
    assert_eq!(duplicates[0]["debit"], "400.00", "{plan}");
    assert_eq!(duplicates[0]["rows"], 1, "{plan}");
}

/// An import says what it will and will not record, before it records it.
///
/// The defect: an import committed before anything said what it would do. Rows
/// came back with positive verdicts and part of them were absent from the report
/// the owner was shown, and no step in between had ever stated the difference.
///
/// What is asserted is the assessment's substance, not merely that a route
/// answers: the row that is settled appears among the facts to be written, the
/// row that is not appears as retained and unrecorded, the account's scope
/// disposition is named, and the readiness says whose decision is outstanding.
#[tokio::test]
async fn an_assessment_says_what_the_import_will_and_will_not_record() {
    let harness = harness();
    let account = harness.account.inner();
    let savings = another_account(&harness, "Savings").await;
    let before = journal_rows(&harness).await;

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": account, "channel": "file", "label": "assessed" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["session"].as_str().expect("session").to_owned();

    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({
                "operations": [
                    {
                        "account": account,
                        "type": "withdrawal",
                        "amount": "1500.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-20" },
                        "description": "Shop One",
                        "idempotency_key": "assessed-spend",
                    },
                    unresolved_row(account, "assessed-inner"),
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");

    let (status, plan) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{id}/assessment"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");

    assert_eq!(plan["source_inventory"]["row_count"], 2, "{plan}");
    assert_eq!(
        plan["source_inventory"]["accounts"]
            .as_array()
            .expect("accounts"),
        &vec![json!(account.to_string())],
        "{plan}"
    );
    assert_eq!(
        plan["account_resolution"]["missing"],
        json!([]),
        "both rows are on an account the owner holds: {plan}"
    );
    // The account sits in no contour, and the assessment says so rather than
    // recording rows that will appear in no report.
    assert_eq!(
        plan["scope_assessment"]["awaiting_disposition"]
            .as_array()
            .expect("awaiting disposition")
            .len(),
        1,
        "{plan}"
    );

    let facts = plan["commit_delta"]["facts"].as_array().expect("facts");
    assert_eq!(facts.len(), 1, "one row can be written: {plan}");
    assert_eq!(facts[0]["records_as"], "cash_out", "{plan}");
    assert_eq!(facts[0]["amount"], "-1500.00", "{plan}");
    assert_eq!(facts[0]["idempotency_key"], "assessed-spend", "{plan}");

    let retained = plan["commit_delta"]["retained_unrecorded"]
        .as_array()
        .expect("retained rows");
    assert_eq!(retained.len(), 1, "one row cannot be written yet: {plan}");
    assert_eq!(retained[0]["reason"], "unanswered", "{plan}");
    assert_eq!(
        plan["interpretation"]["open_questions"]
            .as_array()
            .expect("open questions")
            .len(),
        1,
        "{plan}"
    );
    assert_eq!(plan["readiness"], "requires_owner_decision", "{plan}");

    // Reading the assessment wrote nothing, which is the other half of the
    // property: it is a plan, and a plan is not an import.
    assert_eq!(journal_rows(&harness).await, before, "{plan}");

    // And it is the code that commits: answer the question, commit against the
    // revision the assessment now stamps, and both rows are recorded.
    let (status, contents) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{id}"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contents}");
    let question = contents["questions"].as_array().expect("questions")[0]["question"]
        .as_str()
        .expect("question")
        .to_owned();
    let (status, answered) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "sent_to_own_account", "account": savings }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");

    let (status, replanned) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{id}/assessment"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replanned}");
    assert_eq!(replanned["readiness"], "ready", "{replanned}");
    assert_eq!(
        replanned["commit_delta"]["facts"]
            .as_array()
            .expect("facts")
            .len(),
        2,
        "{replanned}"
    );

    let (status, committed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({ "revision": replanned["revision"] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(
        committed["revision"], replanned["revision"],
        "commit reports the reading it wrote under: {committed}"
    );
    assert_eq!(
        journal_rows(&harness).await,
        before + 2,
        "the commit wrote exactly what the assessment said it would"
    );
}

/// A commit against a reading the session no longer answers to is refused.
///
/// The revision is stale when what the plan describes has changed — here by a
/// row arriving after the assessment was read. Committing anyway would write
/// something other than what the caller approved, which is the whole defect in
/// miniature.
#[tokio::test]
async fn a_commit_against_a_stale_revision_is_refused() {
    let harness = harness();
    let account = harness.account.inner();
    let before = journal_rows(&harness).await;

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": account, "channel": "file", "label": "stale" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["session"].as_str().expect("session").to_owned();

    let spend = |key: &str, amount: &str| {
        json!({
            "account": account,
            "type": "withdrawal",
            "amount": amount,
            "currency": "RUB",
            "dates": { "cash_posted": "2025-03-20" },
            "idempotency_key": key,
        })
    };

    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({ "operations": [spend("stale-one", "1500.00")] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");

    let (status, plan) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{id}/assessment"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let read = plan["revision"].as_str().expect("revision").to_owned();

    // A second row arrives after the reading. What would be committed is no
    // longer what was read.
    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({ "operations": [spend("stale-two", "700.00")] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");

    let (status, refused) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({ "revision": read }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["field"], "revision", "{refused}");
    assert_eq!(
        journal_rows(&harness).await,
        before,
        "a refused commit writes nothing"
    );

    // Reading again and committing against that reading works, and writes both.
    let (status, plan) = call(
        &harness.router,
        get(
            &format!("/v1/import-sessions/{id}/assessment"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_ne!(
        plan["revision"].as_str().expect("revision"),
        read,
        "a session that changed carries a different revision: {plan}"
    );

    let (status, committed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({ "revision": plan["revision"] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(journal_rows(&harness).await, before + 2, "{committed}");
}

/// Two legs of one transfer are proposed, never merged, and relate on the
/// owner's word (iaam-3ul2).
///
/// The refusals are the substance. A pair the system does not propose cannot be
/// confirmed — otherwise the route would relate any outflow to any inflow, which
/// is the fabrication the proposal exists to prevent with the owner's name on it
/// — and a confirmation that does not acknowledge the retraction is refused like
/// every other correction, because two movements the owner has already seen
/// stop counting.
#[tokio::test]
async fn a_transfer_pairing_is_proposed_with_its_evidence_and_never_confirmed_blindly() {
    let harness = harness();
    let account = harness.account.inner();
    let elsewhere = another_account(&harness, "Elsewhere").await;

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "near" },
                "operations": [{
                    "account": account,
                    "type": "withdrawal",
                    "amount": "12000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-03-15" },
                    "description": "Transfer out",
                    "idempotency_key": "pairing-out",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    let outgoing = verdicts[0]["event_id"].as_str().expect("event").to_owned();

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source": { "account": elsewhere, "channel": "file", "label": "far" },
                "operations": [{
                    "account": elsewhere,
                    "type": "deposit",
                    "amount": "12000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-03-16" },
                    "description": "Transfer in",
                    "idempotency_key": "pairing-in",
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    let incoming = verdicts[0]["event_id"].as_str().expect("event").to_owned();

    let (status, proposals) = call(
        &harness.router,
        get("/v1/transfer-pairings", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proposals}");
    let candidates = proposals["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1, "{proposals}");
    let evidence = &candidates[0]["evidence"];
    assert_eq!(evidence["amount"], "12000.00", "{proposals}");
    assert_eq!(evidence["days_apart"], 1, "{proposals}");
    assert_eq!(
        evidence["outgoing_reference"], "Transfer out",
        "{proposals}"
    );
    assert_eq!(evidence["incoming_reference"], "Transfer in", "{proposals}");
    assert_eq!(evidence["sole_candidate"], true, "{proposals}");

    // A pair nothing proposed cannot be confirmed: the two identifiers here are
    // the same event, and no candidate relates an event to itself.
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/transfer-pairings",
            &harness.owner_token,
            &json!({
                "outgoing": outgoing,
                "incoming": outgoing,
                "acknowledge_retraction": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refused}");

    // Nor is it confirmed without acknowledging what stops counting.
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/transfer-pairings",
            &harness.owner_token,
            &json!({ "outgoing": outgoing, "incoming": incoming }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["field"], "acknowledge_retraction", "{refused}");

    let (status, confirmed) = call(
        &harness.router,
        post(
            "/v1/transfer-pairings",
            &harness.owner_token,
            &json!({
                "outgoing": outgoing,
                "incoming": incoming,
                "acknowledge_retraction": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert!(confirmed["transfer"].is_string(), "{confirmed}");

    // And the pair is gone from the proposals: both legs are accounted for by
    // the transfer, so neither is half of anything any more.
    let (status, proposals) = call(
        &harness.router,
        get("/v1/transfer-pairings", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proposals}");
    assert!(
        proposals["candidates"]
            .as_array()
            .expect("candidates")
            .is_empty(),
        "{proposals}"
    );
    assert!(
        proposals["without_counterpart"]
            .as_array()
            .expect("legs with no counterpart")
            .is_empty(),
        "{proposals}"
    );
}

// --- Decision 0004: the identity a source prints for an account -------------

#[tokio::test]
async fn an_account_created_without_an_identity_states_none() {
    // Every account that existed before decision 0004 is in this state, and the
    // wire shape must keep saying so rather than filling the gap in.
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": "Main" }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(created.get("provider").is_none(), "{created}");
    assert!(created.get("provider_account_id").is_none(), "{created}");
    assert!(created.get("cash_class").is_none(), "{created}");
    assert!(created.get("aliases").is_none(), "{created}");
}

#[tokio::test]
async fn a_create_repeating_an_external_identity_returns_the_account_created_last_time() {
    // A re-import must find the account it created last time. The title differs
    // on the second call on purpose: a title is a display name, so repeating an
    // identity under a new one is not a rename and must not become one.
    let harness = harness();
    let identity = json!({ "provider": "bank-one", "provider_account_id": "opaque-1" });

    let (status, first) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Main",
                "provider": identity["provider"],
                "provider_account_id": identity["provider_account_id"],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["provider"], "bank-one");

    let (status, second) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Main, renamed at the source",
                "provider": identity["provider"],
                "provider_account_id": identity["provider_account_id"],
            }),
        ),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a create that minted nothing must not report a creation: {second}"
    );
    assert_eq!(second["id"], first["id"], "{second}");
    assert_eq!(
        second["title"], "Main",
        "the identity was already known, so the title the owner reads is untouched"
    );

    let (status, list) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let minted = list
        .as_array()
        .expect("account list")
        .iter()
        .filter(|account| account["provider"] == "bank-one")
        .count();
    assert_eq!(minted, 1, "a second account must not exist: {list}");
}

#[tokio::test]
async fn one_provider_account_id_at_two_providers_is_two_accounts() {
    // Uniqueness is scoped by provider: two sources that both print short
    // sequential identifiers would otherwise collide on values neither controls.
    let harness = harness();

    for provider in ["bank-one", "bank-two"] {
        let (status, body) = call(
            &harness.router,
            post(
                "/v1/accounts",
                &harness.owner_token,
                &json!({
                    "title": format!("Main at {provider}"),
                    "provider": provider,
                    "provider_account_id": "7",
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (_, list) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.owner_token)),
    )
    .await;
    let with_seven = list
        .as_array()
        .expect("account list")
        .iter()
        .filter(|account| account["provider_account_id"] == "7")
        .count();
    assert_eq!(with_seven, 2, "{list}");
}

#[tokio::test]
async fn half_an_external_identity_is_refused() {
    // The pair is the identity. One half alone would be stored as no identity at
    // all, and the caller would learn that only on the re-import that minted a
    // duplicate. This is a check on the shape of the pair, never on the value:
    // `provider_account_id` stays opaque.
    let harness = harness();

    for body in [
        json!({ "title": "Main", "provider": "bank-one" }),
        json!({ "title": "Main", "provider_account_id": "opaque-1" }),
    ] {
        let (status, refusal) = call(
            &harness.router,
            post("/v1/accounts", &harness.owner_token, &body),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");
        assert_eq!(refusal["code"], "invalid_request");
        assert!(
            !refusal.to_string().contains("opaque-1"),
            "the refusal must not echo the identifier back: {refusal}"
        );
    }
}

#[tokio::test]
async fn two_cards_over_one_account_are_one_account_with_two_aliases() {
    // The balance is counted once because there is one account. A card that
    // stopped working is an alias whose interval closed, and that is all the
    // model records about it.
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Main",
                "aliases": [
                    { "value": "card-one", "valid_from": "2024-01-01", "valid_to": "2025-03-01" },
                    { "value": "card-two", "valid_from": "2025-03-01" },
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let (_, list) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.owner_token)),
    )
    .await;
    let account = list
        .as_array()
        .expect("account list")
        .iter()
        .find(|account| account["id"] == created["id"])
        .expect("the created account")
        .clone();
    assert_eq!(
        account["aliases"],
        json!([
            { "value": "card-one", "valid_from": "2024-01-01", "valid_to": "2025-03-01" },
            { "value": "card-two", "valid_from": "2025-03-01" },
        ]),
        "{account}"
    );
}

#[tokio::test]
async fn an_alias_interval_that_ends_before_it_begins_is_refused() {
    let harness = harness();

    let (status, refusal) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Main",
                "aliases": [
                    { "value": "card-one", "valid_from": "2025-03-01", "valid_to": "2024-01-01" },
                ],
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");
    assert_eq!(refusal["code"], "invalid_request");
}

#[tokio::test]
async fn a_cash_class_the_owner_states_survives_the_round_trip() {
    let harness = harness();

    for class in ["deposit", "savings", "card_account", "wallet"] {
        let (status, created) = call(
            &harness.router,
            post(
                "/v1/accounts",
                &harness.owner_token,
                &json!({ "title": format!("Account: {class}"), "cash_class": class }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["cash_class"], class, "{created}");
    }
}

#[tokio::test]
async fn a_cash_class_outside_the_cash_perimeter_is_refused() {
    // `brokerage` and `security_position` are not values here: positions are
    // what the journal records, and the projection separates them from cash
    // structurally. An unknown class is refused rather than defaulted.
    let harness = harness();

    for class in ["brokerage", "security_position", "invented"] {
        let (status, refusal) = call(
            &harness.router,
            post(
                "/v1/accounts",
                &harness.owner_token,
                &json!({ "title": "Main", "cash_class": class }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refusal}");
    }
}

#[tokio::test]
async fn an_alias_the_owner_adds_later_reaches_the_same_account() {
    // The two-card case usually arrives in two steps: the account exists, then a
    // second card appears over it. Without a route for that, an account's
    // aliases could only ever be stated at creation, and the case decision 0004
    // was written about would still need two accounts.
    let harness = harness();

    let (status, created) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Main",
                "aliases": [{ "value": "card-one", "valid_from": "2024-01-01" }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().expect("account id").to_owned();

    // The first card stopped working and a second took its place: one interval
    // closes, another opens, and there is nothing else to record.
    let (status, updated) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{id}/aliases"),
            &harness.owner_token,
            &json!({
                "aliases": [
                    { "value": "card-one", "valid_from": "2024-01-01", "valid_to": "2025-03-01" },
                    { "value": "card-two", "valid_from": "2025-03-01" },
                ],
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        updated["aliases"],
        json!([
            { "value": "card-one", "valid_from": "2024-01-01", "valid_to": "2025-03-01" },
            { "value": "card-two", "valid_from": "2025-03-01" },
        ]),
        "{updated}"
    );
    assert_eq!(updated["id"], created["id"], "still one account: {updated}");
}

#[tokio::test]
async fn aliases_cannot_be_written_against_an_account_the_owner_does_not_hold() {
    let harness = harness();

    let (status, refusal) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{}/aliases", Uuid::new_v4()),
            &harness.owner_token,
            &json!({ "aliases": [{ "value": "card-one", "valid_from": "2024-01-01" }] }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{refusal}");
    assert_eq!(refusal["code"], "not_found");
}

#[tokio::test]
async fn an_agent_may_not_state_an_accounts_aliases() {
    // An alias decides which printed identifier reaches which account, and
    // therefore which account a row lands on. That is the owner's statement, by
    // the same rule that keeps account creation out of the agent's hands.
    let harness = harness();

    let (status, refusal) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{}/aliases", harness.account.inner()),
            &harness.agent_token,
            &json!({ "aliases": [] }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");
}

/// Create an account and return its identifier.
async fn account_with(harness: &Harness, body: &serde_json::Value) -> String {
    let (status, created) = call(
        &harness.router,
        post("/v1/accounts", &harness.owner_token, body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["id"].as_str().expect("account id").to_owned()
}

async fn declare(
    harness: &Harness,
    id: &str,
    body: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    call(
        &harness.router,
        put(
            &format!("/v1/accounts/{id}/declarations"),
            &harness.owner_token,
            body,
        ),
    )
    .await
}

#[tokio::test]
async fn an_account_that_states_no_identity_can_be_given_one_later() {
    // The defect: `POST /v1/accounts` is an upsert by external identity, so a
    // repeat create returns the account made last time and changes nothing about
    // it — correctly, because it is idempotent rather than an update. Nothing
    // else wrote these three. So every account created before decision 0004
    // could never acquire an identity, a class or an expectation.
    let harness = harness();
    let id = account_with(&harness, &json!({ "title": "Main" })).await;

    let (status, recorded) = declare(
        &harness,
        &id,
        &json!({
            "identity": {
                "stated": true,
                "provider": "bank-one",
                "provider_account_id": "opaque-1",
            },
            "cash_class": { "stated": true, "class": "savings" },
            "negative_balance_expectation": { "stated": true, "expectation": "unexpected" },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["account"]["provider"], "bank-one", "{recorded}");
    assert_eq!(recorded["account"]["provider_account_id"], "opaque-1");
    assert_eq!(recorded["account"]["cash_class"], "savings");
    assert_eq!(
        recorded["account"]["negative_balance_expectation"],
        "unexpected"
    );
    assert!(
        recorded.get("identity_repointed").is_none(),
        "a first statement displaces nothing: {recorded}"
    );

    // And the next import addresses it: a create carrying that identity now
    // finds this account instead of minting a second.
    let (status, repeated) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Main",
                "provider": "bank-one",
                "provider_account_id": "opaque-1",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repeated}");
    assert_eq!(repeated["id"], id, "{repeated}");

    // The route is discoverable, and the third state is discoverable with it:
    // an agent reading the specification must be able to see that omitting a
    // field is not the same as clearing it.
    let (status, spec) = call(&harness.router, get("/v1/openapi.json", None)).await;
    assert_eq!(status, StatusCode::OK, "{spec}");
    let route = &spec["paths"]["/v1/accounts/{id}/declarations"]["put"];
    assert_eq!(
        route["operationId"], "replace_account_declarations",
        "{spec}"
    );
    let request = &spec["components"]["schemas"]["ReplaceAccountDeclarationsRequest"];
    assert!(
        request["required"].as_array().is_none_or(Vec::is_empty),
        "every declaration is optional, and absent means «leave it alone»: {request}"
    );
    let identity = &spec["components"]["schemas"]["AccountIdentityStatementDto"];
    assert_eq!(
        identity["required"],
        json!(["stated"]),
        "a present statement must say whether he states one at all: {identity}"
    );
}

#[tokio::test]
async fn a_declaration_the_request_does_not_mention_is_left_alone() {
    // Absence is the third state. A replacement that read an unmentioned field
    // as «none» would withdraw, on every call, everything the caller did not
    // happen to repeat — including the identity a later import resolves by.
    let harness = harness();
    let id = account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "opaque-1",
            "cash_class": "savings",
            "negative_balance_expectation": "unexpected",
        }),
    )
    .await;

    let (status, recorded) = declare(
        &harness,
        &id,
        &json!({ "cash_class": { "stated": true, "class": "deposit" } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["account"]["cash_class"], "deposit");
    assert_eq!(
        recorded["account"]["provider"], "bank-one",
        "an identity nobody mentioned is not withdrawn: {recorded}"
    );
    assert_eq!(recorded["account"]["provider_account_id"], "opaque-1");
    assert_eq!(
        recorded["account"]["negative_balance_expectation"], "unexpected",
        "an expectation nobody mentioned is not withdrawn: {recorded}"
    );
    assert!(recorded.get("identity_repointed").is_none(), "{recorded}");
}

#[tokio::test]
async fn stating_none_clears_a_declaration_and_is_not_the_same_call_as_omitting_it() {
    let harness = harness();
    let id = account_with(
        &harness,
        &json!({
            "title": "Main",
            "cash_class": "savings",
            "negative_balance_expectation": "unexpected",
        }),
    )
    .await;

    let (status, recorded) =
        declare(&harness, &id, &json!({ "cash_class": { "stated": false } })).await;

    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert!(
        recorded["account"].get("cash_class").is_none(),
        "cleared on his word: {recorded}"
    );
    assert_eq!(
        recorded["account"]["negative_balance_expectation"], "unexpected",
        "and only the one he cleared: {recorded}"
    );
}

#[tokio::test]
async fn re_pointing_an_identity_is_recorded_and_says_what_it_did_not_do() {
    // The refusal one reaches for first — «facts were imported under the old
    // identity, so refuse» — cannot be stated against this journal: an event
    // records its account and a free source label, and nothing records the
    // external identity in force when it arrived. So the change is made and the
    // response says what it did not do.
    let harness = harness();
    let id = account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "opaque-1",
        }),
    )
    .await;

    let (status, ingested) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "test",
                "operations": [{
                    "account": id,
                    "type": "deposit",
                    "amount": "1000.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2025-01-01" }
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ingested}");
    assert_ne!(ingested[0]["verdict"], "rejected", "{ingested}");

    let (status, recorded) = declare(
        &harness,
        &id,
        &json!({
            "identity": {
                "stated": true,
                "provider": "bank-two",
                "provider_account_id": "opaque-2",
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["account"]["provider"], "bank-two");
    let repointed = &recorded["identity_repointed"];
    assert_eq!(repointed["previous"]["provider"], "bank-one", "{recorded}");
    assert_eq!(repointed["previous"]["provider_account_id"], "opaque-1");
    assert_eq!(
        repointed["facts_recorded"], true,
        "the account is not empty, and that is the most the journal can say: {recorded}"
    );
    let kinds: Vec<&str> = repointed["not_done"]
        .as_array()
        .expect("not_done")
        .iter()
        .map(|entry| entry["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "facts_not_moved",
            "previous_identity_not_reserved",
            "no_fact_records_the_identity_it_arrived_under",
        ]
    );

    // The displaced identity is not reserved, which is what the second entry
    // says: a create carrying it now mints a second account.
    let (status, minted) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Main again",
                "provider": "bank-one",
                "provider_account_id": "opaque-1",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    assert_ne!(minted["id"], json!(id), "{minted}");
}

#[tokio::test]
async fn withdrawing_an_identity_reports_the_one_it_displaced() {
    let harness = harness();
    let id = account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "opaque-1",
        }),
    )
    .await;

    let (status, recorded) =
        declare(&harness, &id, &json!({ "identity": { "stated": false } })).await;

    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert!(recorded["account"].get("provider").is_none(), "{recorded}");
    assert_eq!(
        recorded["identity_repointed"]["previous"]["provider"], "bank-one",
        "{recorded}"
    );
    assert_eq!(
        recorded["identity_repointed"]["facts_recorded"], false,
        "this account has no facts, and the response says so: {recorded}"
    );
}

#[tokio::test]
async fn an_identity_another_account_already_answers_to_is_refused() {
    // Two accounts under one identity would leave the next import's upsert
    // picking between them.
    let harness = harness();
    account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "opaque-1",
        }),
    )
    .await;
    let other = account_with(&harness, &json!({ "title": "Savings" })).await;

    let (status, refusal) = declare(
        &harness,
        &other,
        &json!({
            "identity": {
                "stated": true,
                "provider": "bank-one",
                "provider_account_id": "opaque-1",
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
    assert_eq!(refusal["code"], "already_exists", "{refusal}");
}

#[tokio::test]
async fn half_an_identity_is_refused_when_it_is_stated_as_it_is_at_creation() {
    let harness = harness();
    let id = account_with(&harness, &json!({ "title": "Main" })).await;

    for body in [
        json!({ "identity": { "stated": true, "provider": "bank-one" } }),
        json!({ "identity": { "stated": true, "provider_account_id": "opaque-1" } }),
        json!({ "identity": { "stated": true } }),
        // A withdrawal beside a value says two things at once.
        json!({ "identity": { "stated": false, "provider": "bank-one" } }),
    ] {
        let (status, refusal) = declare(&harness, &id, &body).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {refusal}"
        );
        assert_eq!(refusal["code"], "invalid_request", "{refusal}");
    }

    let (status, account) = call(
        &harness.router,
        get("/v1/accounts", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    let held = account
        .as_array()
        .expect("accounts")
        .iter()
        .find(|held| held["id"] == json!(id))
        .expect("the account");
    assert!(
        held.get("provider").is_none(),
        "nothing was written: {held}"
    );
}

#[tokio::test]
async fn a_declaration_stated_without_a_value_is_refused() {
    let harness = harness();
    let id = account_with(&harness, &json!({ "title": "Main" })).await;

    for body in [
        json!({ "cash_class": { "stated": true } }),
        json!({ "cash_class": { "stated": false, "class": "savings" } }),
        json!({ "negative_balance_expectation": { "stated": true } }),
        json!({ "negative_balance_expectation": { "stated": false, "expectation": "ordinary" } }),
    ] {
        let (status, refusal) = declare(&harness, &id, &body).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {refusal}"
        );
    }
}

#[tokio::test]
async fn declarations_cannot_be_written_against_an_account_the_owner_does_not_hold() {
    let harness = harness();

    let (status, refusal) = declare(
        &harness,
        &Uuid::new_v4().to_string(),
        &json!({ "cash_class": { "stated": true, "class": "savings" } }),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{refusal}");
    assert_eq!(refusal["code"], "not_found");
}

#[tokio::test]
async fn an_agent_may_not_state_an_accounts_declarations() {
    // An identity decides which account a later import addresses, by the same
    // rule that keeps account creation and aliases out of the agent's hands.
    let harness = harness();

    let (status, refusal) = call(
        &harness.router,
        put(
            &format!("/v1/accounts/{}/declarations", harness.account.inner()),
            &harness.agent_token,
            &json!({ "cash_class": { "stated": true, "class": "savings" } }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");
}

#[tokio::test]
async fn an_empty_declaration_request_changes_nothing() {
    // Every field absent is «he mentioned nothing», and the honest answer is the
    // account exactly as it stood.
    let harness = harness();
    let id = account_with(
        &harness,
        &json!({
            "title": "Main",
            "provider": "bank-one",
            "provider_account_id": "opaque-1",
            "cash_class": "savings",
        }),
    )
    .await;

    let (status, recorded) = declare(&harness, &id, &json!({})).await;

    assert_eq!(status, StatusCode::OK, "{recorded}");
    assert_eq!(recorded["account"]["provider"], "bank-one");
    assert_eq!(recorded["account"]["provider_account_id"], "opaque-1");
    assert_eq!(recorded["account"]["cash_class"], "savings");
    assert!(recorded.get("identity_repointed").is_none(), "{recorded}");
}

/// The owner's primary question, answered in one call: how much is on deposit,
/// how much on savings, how much where he has not said, and what the whole is
/// worth.
///
/// The defect this pins: `/v1/reports/balances` answers per account and
/// currency with no total and no grouping, so assembling the answer meant
/// grouping accounts by reading their titles — the guess this repository
/// refuses everywhere else.
#[tokio::test]
async fn the_asset_snapshot_groups_cash_by_the_class_the_owner_declared() {
    let harness = harness();

    let mut created = Vec::new();
    for (title, class) in [
        ("Term", Some("deposit")),
        ("Rainy day", Some("savings")),
        ("Unlabelled", None),
    ] {
        let mut body = json!({ "title": title, "institution": "One Bank" });
        if let Some(class) = class {
            body["cash_class"] = json!(class);
        }
        let (status, account) = call(
            &harness.router,
            post("/v1/accounts", &harness.owner_token, &body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{account}");
        created.push(account["id"].as_str().expect("identifier").to_owned());
    }

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Everything",
                "accounts": [
                    harness.account.inner().to_string(),
                    created[0].clone(),
                    created[1].clone(),
                    created[2].clone(),
                ],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let deposit = |account: &str, amount: &str, key: &str| {
        json!({
            "account": account,
            "type": "deposit",
            "amount": amount,
            "currency": "RUB",
            "dates": { "cash_posted": "2026-01-05" },
            "idempotency_key": key
        })
    };
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [
                    deposit(&created[0], "1000.00", "snapshot-term"),
                    deposit(&created[1], "250.00", "snapshot-rainy"),
                    deposit(&created[2], "40.00", "snapshot-unlabelled"),
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, snapshot) = call(
        &harness.router,
        get(
            &format!("/v1/reports/assets?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");

    let classes = snapshot["cash"]["classes"]
        .as_array()
        .expect("one entry per class");
    let group = |code: Option<&str>| {
        classes
            .iter()
            .find(|class| class["cash_class"] == json!(code))
            .unwrap_or_else(|| panic!("no group for {code:?}: {snapshot}"))
    };
    // Nothing anchors any of these accounts, so every class total is movement
    // from an unknown start and says so in the only field that carries a
    // number.
    let deposit = &group(Some("deposit"))["totals"][0];
    assert_eq!(
        deposit["kind"], "movement_since_unknown_start",
        "{snapshot}"
    );
    assert_eq!(deposit["movement"], "1000.00", "{snapshot}");
    assert_eq!(
        group(Some("savings"))["totals"][0]["movement"],
        "250.00",
        "{snapshot}"
    );
    // The account whose class the owner never stated is its own group. It is
    // never folded into a default one, which would put his money under a
    // heading he did not choose.
    let unstated = group(None);
    assert_eq!(unstated["totals"][0]["movement"], "40.00", "{snapshot}");
    assert_eq!(
        unstated["accounts"].as_array().expect("accounts").len(),
        2,
        "the harness account has no class either: {snapshot}"
    );

    // The classes added up, and it is the sum of the parts rather than a second
    // reading.
    assert_eq!(snapshot["cash"]["totals"][0]["movement"], "1290.00");
    assert_eq!(snapshot["cash"]["totals"][0]["currency"], "RUB");
    // No whole, because there is none to state: every figure inside it is
    // movement from an unknown start, and adding those to position values would
    // produce a number that is not what the owner holds.
    assert_eq!(snapshot["total"], json!([]), "{snapshot}");

    // The register the answer opens with, naming the goal the outstanding-work
    // queue grades by.
    assert_eq!(snapshot["confidence"]["goal"], "asset_snapshot");
}

/// Two accounts in one class and one currency, one anchored by an opening
/// assertion and one not. Their sum is neither a balance nor a movement — a
/// stock added to a flow — so the class publishes both parts and no sum.
///
/// The shape this replaced said `unasserted` for the whole class and printed
/// one number, which understated the anchored account and made the class total
/// unusable at the same time. Nothing in this answer states the combined
/// figure: a reader who wants it adds two labelled parts and knows what he
/// added.
#[tokio::test]
async fn a_class_total_whose_accounts_disagree_states_both_parts_and_no_sum() {
    let harness = harness();

    let mut created = Vec::new();
    for title in ["Savings One", "Savings Two"] {
        let (status, account) = call(
            &harness.router,
            post(
                "/v1/accounts",
                &harness.owner_token,
                &json!({ "title": title, "cash_class": "savings" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{account}");
        created.push(account["id"].as_str().expect("identifier").to_owned());
    }

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "title": "Savings only",
                "accounts": [created[0].clone(), created[1].clone()],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let deposit = |account: &str, amount: &str, key: &str| {
        json!({
            "account": account,
            "type": "deposit",
            "amount": amount,
            "currency": "RUB",
            "dates": { "cash_posted": "2026-01-05" },
            "idempotency_key": key
        })
    };
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [
                    deposit(&created[0], "600.00", "mixed-anchored"),
                    deposit(&created[1], "400.00", "mixed-adrift"),
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    // Only the first account is anchored. The second is left as it arrived,
    // which is how every account looks after a first import.
    let (status, recorded) = call(
        &harness.router,
        post(
            "/v1/reconciliation/balance",
            &harness.owner_token,
            &json!({
                "account": created[0],
                "from": "2026-01-01",
                "to": "2026-01-31",
                "at": "opening",
                "cash": { "currency": "RUB", "amount": "0.00" },
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");

    let (status, snapshot) = call(
        &harness.router,
        get(
            &format!("/v1/reports/assets?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");

    let savings = &snapshot["cash"]["classes"][0];
    assert_eq!(savings["cash_class"], "savings", "{snapshot}");
    let total = &savings["totals"][0];
    assert_eq!(total["kind"], "mixed", "{snapshot}");
    assert_eq!(total["balance"], "600.00", "{snapshot}");
    assert_eq!(total["movement"], "400.00", "{snapshot}");
    // The figure the old shape published, and the one this refuses to.
    assert!(total.get("amount").is_none(), "{snapshot}");
    assert!(
        !snapshot.to_string().contains("1000.00"),
        "nothing states the sum of a stock and a flow: {snapshot}"
    );

    // The same treatment one level up, and no whole for the currency.
    assert_eq!(snapshot["cash"]["totals"][0]["kind"], "mixed", "{snapshot}");
    assert_eq!(snapshot["total"], json!([]), "{snapshot}");

    // Which account is unanchored is said once, in the register, and is not
    // copied onto the class row where it could fall out of step.
    let caveat = snapshot["confidence"]["caveats"]
        .as_array()
        .expect("caveats")
        .iter()
        .find(|caveat| caveat["kind"] == "running_cash_sum")
        .unwrap_or_else(|| panic!("no caveat about the running sum: {snapshot}"));
    assert_eq!(caveat["subject"]["account"], created[1], "{snapshot}");
    assert_eq!(caveat["see"], "accounts[].cash[].kind", "{snapshot}");
}

/// Cash is exact; a position is worth what a quote said on a date. Both halves
/// and the price behind the second are stated **before** the total, so a
/// market-dependent figure cannot read as a bank figure.
#[tokio::test]
async fn the_asset_snapshot_states_both_halves_and_the_price_date_before_the_total() {
    let harness = harness();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Brokerage", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let (status, verdicts) = call(
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
                        "amount": "500.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2026-01-05" },
                        "idempotency_key": "halves-deposit"
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "opening_position",
                        "instrument": harness.instrument.inner(),
                        "custody": harness.custody.inner(),
                        "quantity": "10",
                        "cost_basis": "100.00",
                        "currency": "RUB",
                        "dates": { "trade": "2026-01-01" },
                        "idempotency_key": "halves-position"
                    },
                    {
                        "account": harness.account.inner(),
                        "type": "valuation",
                        "instrument": harness.instrument.inner(),
                        "price": "30",
                        "currency": "RUB",
                        "quality": "previous_close",
                        "dates": { "cash_posted": "2026-01-29" },
                        "idempotency_key": "halves-price"
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    // The opening the whole depends on. Without it the cash half is movement
    // from an unknown start, and then no whole exists to state at all — which
    // is the subject of another test; this one is about the order the three
    // figures are read in when there *is* a whole.
    let (status, recorded) = call(
        &harness.router,
        post(
            "/v1/reconciliation/balance",
            &harness.owner_token,
            &json!({
                "account": harness.account.inner(),
                "from": "2026-01-01",
                "to": "2026-01-31",
                "at": "opening",
                "cash": { "currency": "RUB", "amount": "0.00" },
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");

    let (status, _headers, bytes) = call_raw(
        &harness.router,
        get(
            &format!("/v1/reports/assets?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("a JSON body");
    let snapshot: Value = serde_json::from_str(&body).expect("a JSON body");

    // The exact half does not move when a quote does, and it is anchored, so
    // it is spelled as the balance it is.
    assert_eq!(snapshot["cash"]["totals"][0]["kind"], "balance", "{body}");
    assert_eq!(snapshot["cash"]["totals"][0]["balance"], "500.00", "{body}");
    // The market-dependent half, and the date the price it used was for.
    assert_eq!(snapshot["positions"]["totals"][0]["value"], "300", "{body}");
    assert_eq!(
        snapshot["positions"]["oldest_price_date"], "2026-01-29",
        "{body}"
    );
    let holding = &snapshot["positions"]["holdings"][0];
    assert_eq!(
        holding["instrument"],
        harness.instrument.inner().to_string(),
        "{body}"
    );
    // The decision, not a bare figure: the same shape, from the same selection,
    // that the returns report publishes for this instrument on this date.
    assert_eq!(holding["price"]["kind"], "selected", "{body}");
    assert_eq!(holding["price"]["trade_date"], "2026-01-29", "{body}");
    assert_eq!(holding["value"]["value"], "300", "{body}");
    // The whole, after the halves.
    assert_eq!(snapshot["total"][0]["value"], "800.00", "{body}");

    // Order on the wire, not merely presence: a reader who stops at the first
    // figure must meet the two halves before the number that mixes them.
    let at = |key: &str| body.find(key).unwrap_or_else(|| panic!("{key}: {body}"));
    assert!(at("\"cash\"") < at("\"positions\""), "{body}");
    assert!(at("\"positions\"") < at("\"total\""), "{body}");
    assert!(at("\"oldest_price_date\"") < at("\"total\""), "{body}");
    assert!(at("\"confidence\"") < at("\"cash\""), "{body}");
}

/// A holding no quote covers is absent from the total rather than valued at
/// zero, and the register names it. Zero would be a number the owner could add
/// up; absence is a question.
#[tokio::test]
async fn an_unvalued_holding_is_absent_from_the_snapshot_total_and_is_a_caveat() {
    let harness = harness();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Brokerage", "accounts": [harness.account.inner()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "opening_position",
                    "instrument": harness.instrument.inner(),
                    "custody": harness.custody.inner(),
                    "quantity": "10",
                    "cost_basis": "100.00",
                    "currency": "RUB",
                    "dates": { "trade": "2026-01-01" },
                    "idempotency_key": "unvalued-position"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, snapshot) = call(
        &harness.router,
        get(
            &format!("/v1/reports/assets?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");

    let holding = &snapshot["positions"]["holdings"][0];
    assert_eq!(holding["quantity"], "10", "{snapshot}");
    assert!(holding["value"].is_null(), "absent, never zero: {snapshot}");
    // «I do not know», with the reason. Null said only that the report could
    // not value the holding; it never said whether nothing had been observed or
    // whether what had been observed was too old to use.
    assert_eq!(holding["price"]["kind"], "uncovered", "{snapshot}");
    assert_eq!(holding["price"]["reason"], "no_observation", "{snapshot}");
    assert_eq!(
        snapshot["positions"]["totals"],
        json!([]),
        "an unpriced holding is in no total: {snapshot}"
    );
    assert!(
        snapshot["positions"]["oldest_price_date"].is_null(),
        "{snapshot}"
    );

    assert_eq!(snapshot["confidence"]["complete"], false, "{snapshot}");
    let caveat = snapshot["confidence"]["caveats"]
        .as_array()
        .expect("caveats")
        .iter()
        .find(|caveat| caveat["kind"] == "holding_not_valued")
        .unwrap_or_else(|| panic!("the unvalued holding is named: {snapshot}"));
    assert_eq!(caveat["see"], "positions.holdings[].value", "{snapshot}");
    assert_eq!(
        caveat["subject"]["id"],
        harness.instrument.inner().to_string(),
        "{snapshot}"
    );
}

/// The owner can record that a negative balance on an account would be
/// unexpected, and such a balance is then reported as contradicting that — as a
/// warning, never as a refusal.
///
/// The defect this pins: `Balances::negative_cash` could only say «at stage 1
/// this is not an error», because nothing recorded what the owner expected. A
/// minus on an account he never expects to go negative passed unremarked beside
/// a minus on a margin account, and the two mean opposite things.
#[tokio::test]
async fn a_negative_balance_the_owner_called_unexpected_is_reported_as_contradicting_him() {
    let harness = harness();

    let (status, account) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({
                "title": "Rainy day",
                "institution": "One Bank",
                "negative_balance_expectation": "unexpected"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{account}");
    assert_eq!(
        account["negative_balance_expectation"], "unexpected",
        "{account}"
    );
    // The class is a different declaration and was never asked for, so it is
    // absent. Nothing infers one from the other.
    assert!(account.get("cash_class").is_none(), "{account}");
    let account_id = account["id"].as_str().expect("identifier").to_owned();

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Household", "accounts": [account_id.clone()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [{
                    "account": account_id,
                    "type": "withdrawal",
                    "amount": "80.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2026-01-07" },
                    "idempotency_key": "expectation-withdrawal"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    // A warning, not a refusal: the request succeeds and the figure is stated.
    assert_eq!(status, StatusCode::OK, "{report}");
    let entry = &report["negative_cash"][0];
    assert_eq!(entry["account"], account_id, "{report}");
    assert_eq!(entry["amount"], "-80.00", "{report}");
    assert_eq!(entry["expectation"], "unexpected", "{report}");
    assert_eq!(entry["contradicts_expectation"], true, "{report}");
    // Nothing is suppressed: the row states the figure exactly as it would have
    // without the expectation.
    assert_eq!(
        report["accounts"][0]["cash"][0]["movement"], "-80.00",
        "{report}"
    );
    // And the expectation contributes no caveat. The register is about what the
    // figures leave unsaid; a contradicted expectation is a warning about a
    // figure the report does state, and one that fired on it would be a second
    // completeness mechanism. §11 refuses this account's period reports for its
    // own reason — an unclassified negative span — which is `iaam-sbht` working
    // and is unrelated to what the owner expects.
    let kinds: Vec<&str> = report["confidence"]["caveats"]
        .as_array()
        .expect("caveats")
        .iter()
        .map(|caveat| caveat["kind"].as_str().expect("a kind"))
        .collect();
    assert!(
        kinds.iter().all(|kind| !kind.contains("expect")),
        "a contradicted expectation is a warning on a figure, not a gap in the \
         figures: {kinds:?}"
    );
}

/// The pair to the test above, and the one that keeps it honest: an account the
/// owner said nothing about behaves exactly as every account did before the
/// expectation existed, and one he called ordinary is not a contradiction
/// either.
///
/// **The class is not consulted.** Both accounts here are savings accounts, and
/// a savings account is the very case that tempts «cannot be overdrawn,
/// therefore warn». Decision 0004 §3 forbids that derivation by name, and this
/// is where the code would show it.
#[tokio::test]
async fn a_savings_account_the_owner_said_nothing_about_is_not_warned_on() {
    let harness = harness();

    let mut ids = Vec::new();
    for (title, expectation) in [
        ("Silent savings", None),
        ("Ordinary savings", Some("ordinary")),
    ] {
        let mut body = json!({
            "title": title,
            "institution": "One Bank",
            "cash_class": "savings"
        });
        if let Some(expectation) = expectation {
            body["negative_balance_expectation"] = json!(expectation);
        }
        let (status, account) = call(
            &harness.router,
            post("/v1/accounts", &harness.owner_token, &body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{account}");
        assert_eq!(account["cash_class"], "savings", "{account}");
        ids.push(account["id"].as_str().expect("identifier").to_owned());
    }

    let (status, contour_response) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": "Household", "accounts": [ids[0].clone(), ids[1].clone()] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{contour_response}");
    let contour_id = contour_response["contour"].as_str().expect("scope");

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "manual entry",
                "operations": [
                    {
                        "account": ids[0],
                        "type": "withdrawal",
                        "amount": "30.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2026-01-07" },
                        "idempotency_key": "silent-withdrawal"
                    },
                    {
                        "account": ids[1],
                        "type": "withdrawal",
                        "amount": "40.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2026-01-07" },
                        "idempotency_key": "ordinary-withdrawal"
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, report) = call(
        &harness.router,
        get(
            &format!("/v1/reports/balances?contour={contour_id}&as_of=2026-01-31"),
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");

    let entries = report["negative_cash"].as_array().expect("entries");
    assert_eq!(entries.len(), 2, "both figures are stated: {report}");
    let entry = |account: &str| {
        entries
            .iter()
            .find(|entry| entry["account"] == account)
            .unwrap_or_else(|| panic!("no entry for {account}: {report}"))
    };

    // Silence is not a statement, and a savings class does not become one.
    let silent = entry(&ids[0]);
    assert!(
        silent["expectation"].is_null(),
        "a class must never fill in an expectation: {report}"
    );
    assert_eq!(silent["contradicts_expectation"], false, "{report}");

    // And the opposite statement is not a contradiction either.
    let ordinary = entry(&ids[1]);
    assert_eq!(ordinary["expectation"], "ordinary", "{report}");
    assert_eq!(ordinary["contradicts_expectation"], false, "{report}");
}

// ---------------------------------------------------------------------------
// A rejection a client can act on without reading documentation (iaam-yszw)
// ---------------------------------------------------------------------------

/// RFC 6901 §3: a pointer is a sequence of `/`-prefixed reference tokens, and
/// inside a token a tilde may only be followed by `0` or `1`.
fn is_rfc6901_pointer(pointer: &str) -> bool {
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') {
        return false;
    }
    pointer[1..].split('/').all(|token| {
        let mut characters = token.chars();
        while let Some(character) = characters.next() {
            if character == '~' && !matches!(characters.next(), Some('0' | '1')) {
                return false;
            }
        }
        true
    })
}

/// A rejection addresses the offending field mechanically, not in prose.
///
/// The nested indexed case is the one that matters: `corrections[0].target`
/// reads well and is not a form anything can look up. A client holding the body
/// it just sent can apply `/corrections/0/target` to it and reach the value that
/// was refused, with no parsing of its own.
#[tokio::test]
async fn a_rejected_field_is_addressed_by_a_json_pointer() {
    let harness = harness();
    let missing = Uuid::new_v4();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/corrections",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "corrections": [{ "relation": "reversal", "target": missing }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // The readable form stays: it is what `message` quotes.
    assert_eq!(body["field"], "corrections[0].target", "{body}");
    assert_eq!(body["pointer"], "/corrections/0/target", "{body}");
    let pointer = body["pointer"].as_str().expect("a pointer");
    assert!(
        is_rfc6901_pointer(pointer),
        "the pointer must be one a client can apply to its own body: {body}"
    );

    // And a field with no structure to it still points at itself.
    let (status, flat) = call(
        &harness.router,
        get(
            "/v1/reports/balances?contour=00000000-0000-0000-0000-000000000000&as_of=yesterday",
            Some(&harness.owner_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{flat}");
    assert_eq!(flat["field"], "as_of", "{flat}");
    assert_eq!(flat["pointer"], "/as_of", "{flat}");
}

/// A field with a closed vocabulary says which values it takes.
///
/// `expected` spells the same five out in a sentence, and a client that wanted
/// to retry from it would have to split prose on commas and the word "or".
#[tokio::test]
async fn a_rejected_field_with_a_closed_vocabulary_publishes_its_values() {
    let harness = harness();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/instruments",
            &harness.owner_token,
            &json!({
                "kind": "share",
                "symbol": "SHR",
                "title": "Test share",
                "denomination_currency": "ZZZ",
                "settlement_currency": "RUB",
                "quote_currency": "RUB"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "denomination_currency", "{body}");
    assert_eq!(body["pointer"], "/denomination_currency", "{body}");
    let admitted: Vec<&str> = body["alternatives"]
        .as_array()
        .expect("the values the field admits")
        .iter()
        .map(|alternative| alternative["value"].as_str().expect("a value"))
        .collect();
    assert_eq!(admitted, ["RUB", "USD", "EUR", "CNY", "XAU"], "{body}");
}

/// The classification outcome vocabulary travels as values, not as a sentence.
///
/// `outcome.kind` is one of four words and the schema does not say which, so the
/// refusal carries them. The check runs at the door — before the rule is stored
/// — because a rule the classifier cannot read is a decision written and lost in
/// the same call.
#[tokio::test]
async fn a_rejected_classification_outcome_publishes_the_four_it_admits() {
    let harness = harness();
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/classification-rules",
            &harness.owner_token,
            &json!({
                "matcher": { "kind": "income" },
                "outcome": { "kind": "gift" },
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "outcome", "{body}");
    assert_eq!(body["pointer"], "/outcome", "{body}");
    let admitted: Vec<&str> = body["alternatives"]
        .as_array()
        .expect("the classifications the field admits")
        .iter()
        .map(|alternative| alternative["value"].as_str().expect("a value"))
        .collect();
    assert_eq!(
        admitted,
        ["internal_transfer", "external_flow", "income", "fee"],
        "{body}"
    );
}

/// Where the remedy is a call rather than a value, the refusal names the call.
///
/// Nothing may be written into the commit request that makes an unanswered
/// question answered, so a rejection that only named the field would leave the
/// caller to find the answering route in the specification. It is published in
/// the shape the action queue publishes a resolution with: the operation, its
/// address, the two path segments already known, and the one field still wanted.
#[tokio::test]
async fn a_commit_refused_for_an_open_question_names_the_call_that_answers_it() {
    let harness = harness();
    let account = harness.account.inner();
    let savings = another_account(&harness, "Savings").await;

    let (status, session) = call(
        &harness.router,
        post(
            "/v1/import-sessions",
            &harness.owner_token,
            &json!({ "source": { "account": account, "channel": "file", "label": "march" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let id = session["session"].as_str().expect("session").to_owned();

    let (status, rows) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/rows"),
            &harness.owner_token,
            &json!({ "operations": [unresolved_row(account, "remedy-one")] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    let question = rows[0]["question_id"]
        .as_str()
        .expect("question")
        .to_owned();

    let (status, refused) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["field"], "session", "{refused}");
    assert_eq!(refused["pointer"], "/session", "{refused}");

    let resolutions = refused["resolutions"]
        .as_array()
        .expect("the call that lifts the refusal");
    assert_eq!(
        resolutions.len(),
        1,
        "one call, not the whole backlog: {refused}"
    );
    let resolution = &resolutions[0];
    assert_eq!(
        resolution["operationId"], "answer_import_question",
        "{refused}"
    );
    assert_eq!(resolution["method"], "POST", "{refused}");
    assert_eq!(
        resolution["path"], "/v1/import-sessions/{session}/questions/{question}/answer",
        "{refused}"
    );

    // Both path segments are already filled in: the caller copies them rather
    // than composing them out of identifiers it would have to carry.
    assert_eq!(
        resolution["request"]["preset"]["session"],
        json!(id),
        "{refused}"
    );
    assert_eq!(
        resolution["request"]["preset"]["question"],
        json!(question),
        "{refused}"
    );

    // And the one field still wanted carries the shapes this question admits,
    // built by the same function the queue publishes `/answer` with.
    let answer = resolution["request"]["missing"]
        .as_array()
        .expect("the fields still wanted")
        .iter()
        .find(|missing| missing["pointer"] == "/answer")
        .unwrap_or_else(|| panic!("no /answer field: {refused}"));
    assert_eq!(answer["provided_by"], "owner", "{refused}");
    let shapes: Vec<&str> = answer["alternatives"]
        .as_array()
        .expect("the shapes the question admits")
        .iter()
        .map(|alternative| alternative["value"].as_str().expect("a value"))
        .collect();
    assert!(
        shapes.contains(&"received_from_own_account") && shapes.contains(&"paid"),
        "the shapes must be this question's own: {refused}"
    );
    let naming_an_account = answer["alternatives"]
        .as_array()
        .expect("alternatives")
        .iter()
        .find(|alternative| alternative["value"] == "received_from_own_account")
        .expect("the shape that names an account");
    let candidates = naming_an_account["requires"][0]["candidates"]
        .as_array()
        .expect("the accounts that shape may name");
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate["id"] == json!(savings)),
        "an account a shape may name is offered by identifier and title: {refused}"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate["id"] != json!(account)),
        "the account the row is already on is not the far side of itself: {refused}"
    );

    // A refusal that offers no call says so by omission rather than by an empty
    // list: most rejections have no next call, and one is not manufactured.
    let (status, other) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{id}/commit"),
            &harness.owner_token,
            &json!({ "revision": "not-the-revision" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{other}");
    assert_eq!(other["field"], "revision", "{other}");
    assert!(other.get("resolutions").is_none(), "{other}");
    assert!(other.get("alternatives").is_none(), "{other}");
}

// ---------------------------------------------------------------------------
// Scope is drawn by consequence (iaam-hnod, iaam-rond)
// ---------------------------------------------------------------------------

/// Feed one row nothing settles and hand back the session and its question.
///
/// Parametrised by token because the whole point of the two tests below is that
/// one act reaches two outcomes depending on who performed it, and a helper that
/// fixed the owner's token could not express the other half.
async fn ask_one_question(harness: &Harness, token: &str, key: &str) -> (String, String) {
    let account = harness.account.inner();
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            token,
            &json!({
                "source": { "account": account, "channel": "file", "label": "march" },
                "operations": [unresolved_row(account, key)],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    (
        verdicts[0]["session_id"]
            .as_str()
            .expect("session")
            .to_owned(),
        verdicts[0]["question_id"]
            .as_str()
            .expect("question")
            .to_owned(),
    )
}

async fn classification_rule_count(harness: &Harness) -> usize {
    let (status, rules) = call(
        &harness.router,
        get("/v1/classification-rules", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rules}");
    rules.as_array().expect("rules").len()
}

/// An agent's answer settles the row and generalises nothing (iaam-hnod).
///
/// The defect: writing a classification rule is owner-only at the route that
/// says so, and answering a question wrote one too. The decision the agent could
/// not make directly it made through a route whose name does not mention rules,
/// and the only thing forbidding it was a sentence in the agent's document.
///
/// Both halves are asserted in one test on purpose. "An agent writes no rule" is
/// satisfiable by breaking the feature outright, and the owner's half is what
/// says the rule still exists for the caller entitled to it.
#[tokio::test]
async fn an_agents_answer_settles_the_row_and_writes_no_rule() {
    let harness = harness();
    let savings = another_account(&harness, "Savings").await;
    let before = journal_rows(&harness).await;

    let (session, question) = ask_one_question(&harness, &harness.agent_token, "agent-inner").await;
    let (status, answered) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/questions/{question}/answer"),
            &harness.agent_token,
            &json!({ "answer": "sent_to_own_account", "account": savings }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the row still settles under an agent token: {answered}"
    );
    assert!(
        answered["rule"].is_null(),
        "an agent's answer must not become a standing rule: {answered}"
    );
    assert!(
        answered["answered_at"].is_string(),
        "the question is answered even though nothing was generalised: {answered}"
    );
    assert_eq!(
        classification_rule_count(&harness).await,
        0,
        "the agent wrote a rule through the route that does not say it writes one"
    );

    // The settled row commits like any other: the refusal is of the
    // generalisation, not of the import.
    let (status, committed) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/commit"),
            &harness.agent_token,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_eq!(journal_rows(&harness).await, before + 1, "{committed}");

    // And the same act under the owner's token does generalise.
    let (session, question) = ask_one_question(&harness, &harness.owner_token, "owner-inner").await;
    let (status, answered) = call(
        &harness.router,
        post(
            &format!("/v1/import-sessions/{session}/questions/{question}/answer"),
            &harness.owner_token,
            &json!({ "answer": "sent_to_own_account", "account": savings }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert!(
        answered["rule"].is_string(),
        "the owner's answer is still recorded as a rule: {answered}"
    );
    assert_eq!(classification_rule_count(&harness).await, 1);
}

/// Ingest one deposit under a declared import and report what the journal holds.
async fn declare_import(harness: &Harness, token: &str, label: &str, key: &str) -> Value {
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            token,
            &json!({
                "source": {
                    "account": harness.account.inner(),
                    "channel": "file",
                    "label": label,
                },
                "operations": [{
                    "account": harness.account.inner(),
                    "type": "deposit",
                    "amount": "100.00",
                    "currency": "RUB",
                    "dates": { "cash_posted": "2026-08-05" },
                    "idempotency_key": key
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_eq!(verdicts[0]["verdict"], "provisional", "{verdicts}");
    json!({
        "account": harness.account.inner(),
        "channel": "file",
        "label": label,
    })
}

/// An agent takes back the import it declared, and nothing else (iaam-rond).
///
/// Committing an import is open to the agent and rewrites every downstream
/// report; the retraction was not, so an agent that found by control total that
/// it had written nonsense could only wake the owner to undo the agent's own
/// mistake. Retracting one's own declaration reverses no decision of the
/// owner's — he made none about rows the agent put there.
#[tokio::test]
async fn an_agent_retracts_the_import_it_declared() {
    let harness = harness();
    let before = journal_rows(&harness).await;
    let source = declare_import(&harness, &harness.agent_token, "agent-august", "agent-own").await;
    assert_eq!(journal_rows(&harness).await, before + 1);

    let (status, retracted) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.agent_token,
            &json!({ "acknowledge_retraction": true, "source": source }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{retracted}");
    assert_eq!(retracted["affected"], 1, "{retracted}");
    assert_eq!(retracted["written"], 1, "{retracted}");

    // The acknowledgement is not waived for the agent: a retracted fact stops
    // counting in every report either way.
    let source_two = declare_import(
        &harness,
        &harness.agent_token,
        "agent-september",
        "agent-two",
    )
    .await;
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.agent_token,
            &json!({ "source": source_two }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["field"], "acknowledge_retraction", "{refused}");
}

/// Everything wider than its own untouched declaration is still the owner's.
///
/// Four refusals, one per condition of the bound, because a gate that passes the
/// happy case and lets one of these through is the defect it was meant to close.
#[tokio::test]
async fn an_agent_may_not_retract_anything_it_did_not_declare() {
    let harness = harness();
    let owners = declare_import(&harness, &harness.owner_token, "owner-august", "owner-own").await;

    // 1. Not the caller's declaration.
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.agent_token,
            &json!({ "acknowledge_retraction": true, "source": owners }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["field"], "source", "{refused}");
    assert!(
        refused["expected"]
            .as_str()
            .is_some_and(|text| text.contains("declared")),
        "the refusal must say which condition failed: {refused}"
    );

    // 2. No label: the rows that named no import are nobody's to take back.
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.agent_token,
            &json!({
                "acknowledge_retraction": true,
                "source": { "account": harness.account.inner(), "channel": "file" },
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["field"], "source.label", "{refused}");

    // 3. Already reversed: the second retraction is refused, and the refusal is
    // also the answer to "did my first call land".
    let mine = declare_import(&harness, &harness.agent_token, "agent-august", "agent-own").await;
    let (status, done) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.agent_token,
            &json!({ "acknowledge_retraction": true, "source": mine.clone() }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.agent_token,
            &json!({ "acknowledge_retraction": true, "source": mine }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["actual"]
            .as_str()
            .is_some_and(|text| text.contains("effective")),
        "{refused}"
    );

    // 4. Something is built on it: the owner has reconciled a balance against
    // the interval these rows fall in.
    let reconciled = declare_import(
        &harness,
        &harness.agent_token,
        "agent-october",
        "agent-three",
    )
    .await;
    let (status, recorded) = call(
        &harness.router,
        post(
            "/v1/reconciliation/balance",
            &harness.owner_token,
            &json!({
                "account": harness.account.inner(),
                "from": "2026-08-01",
                "to": "2026-08-31",
                "at": "closing",
                "cash": { "currency": "RUB", "amount": "100.00" },
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recorded}");
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.agent_token,
            &json!({ "acknowledge_retraction": true, "source": reconciled.clone() }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["actual"]
            .as_str()
            .is_some_and(|text| text.contains("control assertion")),
        "{refused}"
    );

    // The owner is refused none of it.
    let (status, retracted) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.owner_token,
            &json!({ "acknowledge_retraction": true, "source": reconciled }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{retracted}");
    assert_eq!(retracted["written"], 1, "{retracted}");
}

/// A read-only token is refused at the transport, before any journal is read.
///
/// The floor the route still keeps: no state of the journal makes a read-only
/// token entitled to write a reversal, so that one refusal does not need the
/// evidence the agent's does.
#[tokio::test]
async fn a_read_only_token_may_not_retract_an_import() {
    let harness = harness();
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/corrections/imports",
            &harness.readonly_token,
            &json!({
                "acknowledge_retraction": true,
                "source": { "account": harness.account.inner(), "channel": "file", "label": "x" },
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "forbidden", "{refused}");
}
