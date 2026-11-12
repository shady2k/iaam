use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Store};
use iaam_app::storage::SqliteStore;
use iaam_core::dates::{EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{AccountId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_ingest::dedup::IdentityScope;
use time::macros::date;

#[derive(Clone, Default)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn failed_detail_write_logs_the_statement_and_batch_row_without_financial_values() {
    let adapter = SqliteAdapter::new(SqliteStore::open_in_memory().expect("in-memory database"));
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let missing_instrument = InstrumentId::new_random();
    adapter
        .upsert_account(
            owner,
            AccountView {
                id: account,
                title: "Invented account".to_owned(),
                institution: None,
            },
        )
        .await
        .expect("seed account");

    let source = SourceId::new_random();
    let assertion = |sequence, claim| Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::ControlAssertion {
            period: AssertionPeriod::between(date!(2026 - 01 - 01), date!(2026 - 01 - 31))
                .expect("valid period"),
            claim,
        },
        dates: EventDates::empty(),
        order: EffectiveOrder::new(date!(2026 - 01 - 31), sequence),
        legs: vec![],
        provenance: Provenance::new(
            source,
            RawHash::parse(&"a".repeat(64)).expect("invented hash"),
            ParserVersion("logging-test/1".to_owned()),
        ),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    };
    let first = assertion(
        1,
        ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(987_654_321),
            at: BalancePoint::Closing,
        },
    );
    // No security leg can fail first: the missing reference exists only in the detail row.
    let failing = assertion(
        2,
        ControlClaim::PositionQuantity {
            instrument: missing_instrument,
            quantity: Quantity(Dec::one()),
            at: BalancePoint::Closing,
        },
    );
    let failing_id = failing.id;
    let writer = LogWriter::default();
    let captured = writer.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);

    let error = adapter
        .append_events(vec![first, failing], IdentityScope::Source)
        .await
        .expect_err("missing instrument must fail the detail foreign key");
    let log = String::from_utf8(captured.0.lock().expect("log buffer").clone())
        .expect("UTF-8 tracing output");
    let line = log
        .lines()
        .find(|line| line.contains("ERROR") && line.contains("insert_control_assertion_detail"))
        .unwrap_or_else(|| panic!("missing detail-write error log; error={error:?}; logs={log}"));
    eprintln!("captured FK log:\n{log}");
    for field in [
        "write=\"insert_control_assertion_detail\"".to_owned(),
        "table=\"event_control_assertion\"".to_owned(),
        "row_index=1".to_owned(),
        format!("event_id={}", failing_id.inner()),
        format!("account_id={}", account.inner()),
        format!("instrument_id=Some(\"{}\")", missing_instrument.inner()),
        "sqlite_extended_code=Some(787)".to_owned(),
        "FOREIGN KEY constraint failed".to_owned(),
    ] {
        assert!(line.contains(&field), "missing {field}: {line}");
    }
    assert!(
        !log.contains("quantity"),
        "financial quantities must not be logged: {log}"
    );
    assert!(
        !log.contains("amount"),
        "financial amounts must not be logged: {log}"
    );
    assert!(
        !log.contains("987654321"),
        "financial values must not be logged: {log}"
    );
}
