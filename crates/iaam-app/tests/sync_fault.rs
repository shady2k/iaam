//! ADR 0042 point 1 for the broker sync: every network call is over before the
//! transaction opens, and the binding, every fact, the coverage gap, the
//! assertions and the owner's refusal questions are written in one transaction
//! (iaam-mkbfh.1, iaam-ge08).
//!
//! Each child test below is the closure the parent runs in another process: it
//! arms the barrier named by the parent, performs the sync, and is killed
//! there. A closure cannot cross a process boundary, so the same binary is
//! re-executed with `--exact <test name>` and the test function is the body.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{
    BrokerAccountDescription, BrokerChannel, BrokerRequestContext, Clock, ParsedOperations,
    PortfolioAsOf, PortfolioSnapshot, Principal, Quarantined, Scope,
};
use iaam_app::sync::{BrokerSyncRequest, SyncOutcome, sync_broker};
use iaam_core::event::Event;
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, InstrumentId, OwnerId, SourceId};
use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
use iaam_core::money::{CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{BalancePoint, ControlClaim};
use iaam_core::reconciliation::evidence::SourceChannel;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
use iaam_ingest::{SubmittedOperation, Verdict};
use iaam_store::SqliteStore;
use iaam_store::documents::BrokerCode;
use iaam_store::fault::{self, KillAt};
use iaam_store::reference::{AccountRecord, InstrumentRecord};
use time::Date;
use time::macros::date;
use uuid::Uuid;

const OWNER_VARIABLE: &str = "IAAM_FAULT_TEST_OWNER";
const ACCOUNT_VARIABLE: &str = "IAAM_FAULT_TEST_ACCOUNT";
const FIRST_INSTRUMENT_VARIABLE: &str = "IAAM_FAULT_TEST_INSTRUMENT_ONE";
const SECOND_INSTRUMENT_VARIABLE: &str = "IAAM_FAULT_TEST_INSTRUMENT_TWO";
const SOURCE_VARIABLE: &str = "IAAM_FAULT_TEST_SOURCE";

/// The interval every run below asks the channel for.
const FROM: Date = date!(2026 - 03 - 01);
const TO: Date = date!(2026 - 03 - 31);
/// The broker number the channel's access sees, and which the sync binds.
const BROKER_NUMBER: &str = "invented-account";

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("iaam-app-sync-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temporary directory");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

/// The invented answer every run below is given: two purchases inside the
/// interval, one row the channel itself set aside, and two cash figures. The
/// sync writes the facts, a coverage gap for the set-aside row, the two
/// assertions and the owner's question about the row — seven writes in one
/// transaction, so a kill at the second proves more than one was reached.
struct FixtureChannel {
    account: AccountId,
    instruments: [InstrumentId; 2],
    source: SourceId,
}

impl FixtureChannel {
    fn purchase(&self, index: usize, operation_id: &str, day: Date) -> SubmittedOperation {
        SubmittedOperation {
            account: self.account,
            kind: OperationKind::Buy {
                instrument: self.instruments[index],
                custody: None,
                quantity: Dec::one(),
                gross_minor: 10_000,
                fee_minor: None,
                basis_fee: None,
                accrued_interest_minor: None,
                currency: CurrencyCode::Rub,
            },
            dates: OperationDates {
                trade: Some(day),
                settled: Some(day),
                cash_posted: Some(day),
                paid: None,
            },
            source_time: None,
            idempotency_key: None,
            source_operation_id: Some(operation_id.to_owned()),
            source_position_id: None,
            source_category: None,
            owner_category: None,
            source_code: None,
            source_kind: None,
            description: None,
            counterparty: None,
        }
    }
}

#[async_trait]
impl BrokerChannel for FixtureChannel {
    async fn fetch_accounts(
        &self,
        _context: BrokerRequestContext<'_>,
    ) -> Result<Vec<BrokerAccountDescription>, iaam_app::ports::BrokerError> {
        Ok(vec![BrokerAccountDescription {
            broker_account: BROKER_NUMBER.to_owned(),
            ..Default::default()
        }])
    }

    async fn fetch_operations(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _from: Date,
        _to: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<ParsedOperations, iaam_app::ports::BrokerError> {
        Ok(ParsedOperations {
            duplicates: Vec::new(),
            has_out_of_interval_trade: false,
            accepted: vec![
                self.purchase(0, "SYNC-BUY-1", date!(2026 - 03 - 10)),
                self.purchase(1, "SYNC-BUY-2", date!(2026 - 03 - 12)),
            ],
            journal: Vec::new(),
            pending_redemptions: Vec::new(),
            quarantined: vec![Quarantined {
                raw: serde_json::json!({"symbol": "FIXT@MISX"}),
                reason: "symbol FIXT@MISX is not resolved to an instrument".to_owned(),
                dimensions: [Dimension::Positions].into_iter().collect(),
            }],
        })
    }

    async fn fetch_portfolio(
        &self,
        _account: AccountId,
        _broker_account: &str,
        _at: Date,
        _context: BrokerRequestContext<'_>,
    ) -> Result<PortfolioSnapshot, iaam_app::ports::BrokerError> {
        Ok(PortfolioSnapshot {
            as_of: PortfolioAsOf::Requested,
            claims: vec![
                ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(0),
                    at: BalancePoint::Opening,
                },
                ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(-20_000),
                    at: BalancePoint::Closing,
                },
            ],
            refused: Vec::new(),
            observed_on: None,
        })
    }

    fn channel(&self) -> SourceChannel {
        SourceChannel {
            source: self.source,
            parser_version: ParserVersion("invented-broker/1".to_owned()),
            document: None,
        }
    }

    fn identity_scope(&self) -> IdentityScope {
        IdentityScope::Account
    }
}

fn broker_code() -> BrokerCode {
    BrokerCode::parse("tinkoff").expect("broker code")
}

/// The account and the two instruments the invented answer names, which the
/// journal's foreign keys require. Nothing here resembles a real statement.
fn seed(database: &Path, owner: OwnerId, account: AccountId, instruments: [InstrumentId; 2]) {
    let store = SqliteStore::open(database).expect("create database");
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Main".to_owned(),
            institution: Some("Invented Bank".to_owned()),
        })
        .expect("seed account");
    for (index, instrument) in instruments.into_iter().enumerate() {
        store
            .upsert_instrument(&InstrumentRecord {
                id: instrument,
                kind: Some(InstrumentKind::Share),
                symbol: format!("INVENT{}", index + 1),
                title: format!("Invented security {}", index + 1),
                currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
                lineage: None,
                metadata: None,
                blocked_tca_flag: false,
            })
            .expect("seed instrument");
    }
}

fn services(database: &Path) -> AppServices {
    let adapter = std::sync::Arc::new(SqliteAdapter::new(
        SqliteStore::open_existing(database).expect("the database opens"),
    ));
    AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        std::sync::Arc::new(FixedClock(TO)),
    )
}

async fn sync(
    database: &Path,
    owner: OwnerId,
    account: AccountId,
    instruments: [InstrumentId; 2],
    source: SourceId,
) -> Result<SyncOutcome, iaam_app::error::AppError> {
    let services = services(database);
    let channel = FixtureChannel {
        account,
        instruments,
        source,
    };
    let principal = Principal {
        token_id: Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    };
    sync_broker(
        &services,
        &principal,
        &channel,
        BrokerSyncRequest {
            broker_code: broker_code(),
            account,
            from: FROM,
            to: TO,
        },
    )
    .await
}

fn child_parts() -> Option<(PathBuf, OwnerId, AccountId, [InstrumentId; 2], SourceId)> {
    let database = std::env::var_os(fault::DATABASE_VARIABLE)?;
    let identifier = |name: &str| {
        Uuid::parse_str(&std::env::var(name).expect("child identifier configured"))
            .expect("invented identifier")
    };
    Some((
        PathBuf::from(database),
        OwnerId(identifier(OWNER_VARIABLE)),
        AccountId(identifier(ACCOUNT_VARIABLE)),
        [
            InstrumentId(identifier(FIRST_INSTRUMENT_VARIABLE)),
            InstrumentId(identifier(SECOND_INSTRUMENT_VARIABLE)),
        ],
        SourceId(identifier(SOURCE_VARIABLE)),
    ))
}

fn run_child_sync() {
    let (database, owner, account, instruments, source) =
        child_parts().expect("child configuration");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async {
        let _ = sync(&database, owner, account, instruments, source).await;
    });
}

#[test]
fn killed_before_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    run_child_sync();
}

#[test]
fn killed_after_commit_child() {
    if !fault::arm_child_barrier() {
        return;
    }
    run_child_sync();
}

fn kill(
    directory: &TempDirectory,
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    database: &Path,
    identifiers: (OwnerId, AccountId, [InstrumentId; 2], SourceId),
) -> KillAt {
    let (owner, account, instruments, source) = identifiers;
    let values = [
        owner.inner().to_string(),
        account.inner().to_string(),
        instruments[0].inner().to_string(),
        instruments[1].inner().to_string(),
        source.inner().to_string(),
    ];
    fault::kill_at(
        child_test,
        point,
        after_write,
        &directory.file("barrier-reached"),
        database,
        &[
            (OWNER_VARIABLE, OsStr::new(&values[0])),
            (ACCOUNT_VARIABLE, OsStr::new(&values[1])),
            (FIRST_INSTRUMENT_VARIABLE, OsStr::new(&values[2])),
            (SECOND_INSTRUMENT_VARIABLE, OsStr::new(&values[3])),
            (SOURCE_VARIABLE, OsStr::new(&values[4])),
        ],
    )
}

/// Everything one attempt is meant to leave, without the identifiers and
/// moments a new attempt generates: the comparison ADR 0042 point 7 asks for.
#[derive(Debug, PartialEq, Eq)]
struct Attempt {
    journal: Vec<String>,
    binding: Option<String>,
    refusals: Vec<String>,
}

/// What one event means: its kind, its place in the day, the identity the
/// source gave it and the content hash both attempts derive the same way.
fn meaning(event: &Event) -> String {
    format!(
        "{} {} {} {:?} {} {:?}",
        event.kind.discriminant(),
        event.order.date(),
        event.order.sequence(),
        event.idempotency_key,
        event.provenance.raw_hash().as_str(),
        event.provenance.source_operation_id(),
    )
}

fn attempt(database: &Path, owner: OwnerId, account: AccountId, source: SourceId) -> Attempt {
    let store = SqliteStore::open_existing(database).expect("the database reopens");
    let journal = store
        .load_events_through(owner, Date::MAX)
        .expect("read the journal")
        .iter()
        .map(meaning)
        .collect();
    let binding = store
        .broker_account_binding(owner, account, &broker_code())
        .expect("read the binding");
    let refusals = store
        .list_open_sync_refusals(
            owner,
            account,
            &source.inner().to_string(),
            &FROM.to_string(),
            &TO.to_string(),
        )
        .expect("read the owner's questions")
        .iter()
        .map(|record| {
            format!(
                "{} {} {} {} {}",
                record.row_key,
                record.range_from,
                record.range_to,
                record.dimensions,
                record.reason
            )
        })
        .collect();
    Attempt {
        journal,
        binding,
        refusals,
    }
}

/// A clean run of the same invented answer on its own database: what the
/// interrupted attempt is compared against, in its store and in its answer.
struct CleanRun {
    outcome: SyncOutcome,
    attempt: Attempt,
}

async fn clean_run(
    directory: &TempDirectory,
    owner: OwnerId,
    account: AccountId,
    instruments: [InstrumentId; 2],
    source: SourceId,
) -> CleanRun {
    let database = directory.file("clean.sqlite");
    seed(&database, owner, account, instruments);
    let outcome = sync(&database, owner, account, instruments, source)
        .await
        .expect("the clean run completes");
    CleanRun {
        outcome,
        attempt: attempt(&database, owner, account, source),
    }
}

/// What one sync answered, without the identifiers it mints: the comparison of
/// two answers the acceptance asks for beside the comparison of two stores.
fn answer(outcome: &SyncOutcome) -> String {
    let mut kinds: Vec<&str> = outcome
        .recorded
        .iter()
        .map(|verdict| match verdict {
            Verdict::Accepted { .. } => "accepted",
            Verdict::Provisional { .. } => "provisional",
            Verdict::PossibleDuplicate { .. } => "possible_duplicate",
            Verdict::Discrepancy { .. } => "discrepancy",
            Verdict::NeedsReconciliation { .. } => "needs_reconciliation",
            Verdict::NeedsClassification { .. } => "needs_classification",
            Verdict::Duplicate { .. } => "duplicate",
            Verdict::Unsupported { .. } => "unsupported",
            Verdict::Rejected { .. } => "rejected",
            Verdict::Quarantined { .. } => "quarantined",
            Verdict::NoFact { .. } => "no_fact",
        })
        .collect();
    kinds.sort_unstable();
    format!(
        "{kinds:?} duplicates={} possible_duplicates={} set_aside={} assertions={} \
         withheld={:?} binding_recorded={}",
        outcome.duplicates,
        outcome.possible_duplicates,
        outcome.set_aside,
        outcome.assertions,
        outcome.assertions_withheld,
        outcome.binding_recorded,
    )
}

#[tokio::test]
async fn a_kill_inside_the_sync_leaves_nothing_and_the_rerun_records_it_all() {
    let directory = TempDirectory::new("before-commit");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instruments = [InstrumentId::new_random(), InstrumentId::new_random()];
    let source = SourceId::new_random();
    seed(&database, owner, account, instruments);

    // The attempt's second write, not its first: the binding is the first, and
    // a store that committed each write on its own would leave it behind and
    // pass a threshold of one.
    let KillAt { child, .. } = kill(
        &directory,
        "killed_before_commit_child",
        fault::BEFORE_COMMIT,
        Some(2),
        &database,
        (owner, account, instruments, source),
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the sync's second write, before COMMIT"
    );

    let interrupted = attempt(&database, owner, account, source);
    assert!(
        interrupted.journal.is_empty(),
        "a kill inside the transaction leaves no fact, no coverage gap and no assertion: {:?}",
        interrupted.journal
    );
    assert_eq!(
        interrupted.binding, None,
        "and no binding: the number the access saw was confirmed by facts that were rolled back"
    );
    assert!(
        interrupted.refusals.is_empty(),
        "and no question about a row the attempt never kept: {:?}",
        interrupted.refusals
    );

    let retried = sync(&database, owner, account, instruments, source)
        .await
        .expect("the re-run completes the sync");
    assert!(
        retried.binding_recorded,
        "the re-run discovers and records the binding the killed attempt rolled back"
    );
    let clean = clean_run(&directory, owner, account, instruments, source).await;
    assert_eq!(
        attempt(&database, owner, account, source),
        clean.attempt,
        "the re-run records what a clean run records, by meaning"
    );
    assert_eq!(
        answer(&retried),
        answer(&clean.outcome),
        "and answers what a clean run answers"
    );
}

#[tokio::test]
async fn a_kill_after_commit_leaves_the_whole_attempt_and_the_repeat_records_duplicates() {
    let directory = TempDirectory::new("after-commit");
    let database = directory.file("iaam.sqlite");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let instruments = [InstrumentId::new_random(), InstrumentId::new_random()];
    let source = SourceId::new_random();
    seed(&database, owner, account, instruments);

    let KillAt { child, .. } = kill(
        &directory,
        "killed_after_commit_child",
        fault::AFTER_COMMIT,
        None,
        &database,
        (owner, account, instruments, source),
    );
    assert!(
        child.marker.exists(),
        "the child was killed after the commit"
    );

    let committed = attempt(&database, owner, account, source);
    assert!(
        !committed.journal.is_empty(),
        "a kill after COMMIT leaves every fact of the attempt"
    );
    assert_eq!(
        committed.binding.as_deref(),
        Some(BROKER_NUMBER),
        "and the binding, which the same transaction wrote"
    );
    assert_eq!(
        committed.refusals.len(),
        1,
        "and the owner's question about the set-aside row"
    );
    assert_eq!(
        committed,
        clean_run(&directory, owner, account, instruments, source)
            .await
            .attempt,
        "the killed run leaves what a clean run leaves, by meaning"
    );

    let repeated = sync(&database, owner, account, instruments, source)
        .await
        .expect("the repeat is answered, not refused");
    assert_eq!(
        repeated.duplicates,
        committed.journal.len(),
        "every fact the committed attempt holds is recognised as a duplicate: {:?}",
        repeated.recorded
    );
    assert_eq!(repeated.assertions, 0, "and none of them is written again");
    assert_eq!(
        attempt(&database, owner, account, source),
        committed,
        "the repeat leaves the journal, the binding and the questions as they were"
    );
}
