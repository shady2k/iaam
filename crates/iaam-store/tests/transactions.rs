//! ADR 0042 point 3: a panic inside the work rolls the transaction back.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use iaam_core::ids::{AccountId, OwnerId};
use iaam_store::reference::AccountRecord;
use iaam_store::{SqliteStore, StoreError};

/// A log sink the assertions can read back.
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

#[test]
fn a_panic_inside_the_work_rolls_the_transaction_back_and_the_store_stays_usable() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory database");
    let owner = OwnerId::new_random();
    let account = AccountRecord {
        id: AccountId::new_random(),
        owner,
        title: "Main".to_owned(),
        institution: None,
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.with_immediate_transaction(
            |error| error,
            |store| -> Result<(), StoreError> {
                store.upsert_account(&account)?;
                panic!("interrupted after the write");
            },
        )
    }));
    assert!(
        outcome.is_err(),
        "the panic must unwind past the transaction"
    );

    assert!(
        store.list_accounts(owner).expect("read back").is_empty(),
        "the write before the panic must be rolled back"
    );
    store.upsert_account(&account).expect("the store is usable");
    assert_eq!(
        store.list_accounts(owner).expect("read back").len(),
        1,
        "what is written after the panic is written"
    );
}

/// Every market write that must run inside the caller's transaction refuses
/// when it is given none (ADR 0042 point 1). Outside one, each would commit its
/// own statements — the second way to publish that the branch closed.
#[test]
fn a_market_write_outside_a_transaction_is_refused_by_name() {
    use iaam_core::ids::InstrumentId;
    use iaam_core::instrument::{CurrencyRoles, InstrumentKind};
    use iaam_core::money::CurrencyCode;
    use iaam_store::market::{
        AccruedInterestRow, Coverage, FxRow, KeyRateRow, PriceRow, RunOutcome, SeriesKey,
    };
    use iaam_store::reference::InstrumentRecord;
    use time::macros::date;

    let mut store = SqliteStore::open_in_memory().expect("in-memory database");
    let instrument = InstrumentId::new_random();
    store
        .upsert_instrument(&InstrumentRecord {
            id: instrument,
            kind: Some(InstrumentKind::Share),
            symbol: "SBER".to_owned(),
            title: "Invented share".to_owned(),
            currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
            lineage: None,
            metadata: None,
            blocked_tca_flag: false,
        })
        .expect("instrument created");
    let run = store
        .begin_run(
            SeriesKey {
                source_id: "moex-iss".to_owned(),
                dataset: "prices".to_owned(),
                series_key: "SBER:TQBR:1".to_owned(),
            },
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 03),
            time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
        )
        .expect("lease");

    let id = instrument.inner().to_string();
    let refusals = [
        (
            "record_prices_within",
            store
                .record_prices_within(
                    &run,
                    "raw",
                    &[PriceRow {
                        instrument_id: id.clone(),
                        board: "TQBR".to_owned(),
                        session: 1,
                        trade_date: "2026-08-03".to_owned(),
                        kind: "close".to_owned(),
                        observed_at: "2026-08-03T09:00:00Z".to_owned(),
                        price: "100.00".to_owned(),
                        currency: "RUB".to_owned(),
                        quotation_basis: "money_per_unit".to_owned(),
                        basis_evidence: "test:transactions".to_owned(),
                        executability: "executable".to_owned(),
                    }],
                )
                .map(|_| ()),
        ),
        (
            "record_accrued_interest_within",
            store
                .record_accrued_interest_within(
                    &run,
                    "raw",
                    &[AccruedInterestRow {
                        instrument_id: id.clone(),
                        board: "TQBR".to_owned(),
                        session: 1,
                        trade_date: "2026-08-03".to_owned(),
                        observed_at: "2026-08-03T09:00:00Z".to_owned(),
                        per_unit: "1.00".to_owned(),
                        currency: "RUB".to_owned(),
                    }],
                )
                .map(|_| ()),
        ),
        (
            "record_fx_within",
            store
                .record_fx_within(
                    &run,
                    "raw",
                    &[FxRow {
                        from_code: "USD".to_owned(),
                        to_code: "RUB".to_owned(),
                        trade_date: "2026-08-03".to_owned(),
                        observed_at: "2026-08-03T09:00:00Z".to_owned(),
                        nominal: 1,
                        value: "80.00".to_owned(),
                        unit_rate: "80.00".to_owned(),
                    }],
                )
                .map(|_| ()),
        ),
        (
            "record_key_rate_within",
            store
                .record_key_rate_within(
                    &run,
                    "raw",
                    &[KeyRateRow {
                        trade_date: "2026-08-03".to_owned(),
                        observed_at: "2026-08-03T09:00:00Z".to_owned(),
                        rate: "16.00".to_owned(),
                    }],
                )
                .map(|_| ()),
        ),
        (
            "finish_run_within",
            store.finish_run_within(
                &run,
                RunOutcome::Succeeded,
                Some(Coverage {
                    from: date!(2026 - 08 - 03),
                    to: date!(2026 - 08 - 03),
                }),
            ),
        ),
    ];
    for (method, outcome) in refusals {
        match outcome {
            Err(StoreError::NoTransaction { method: named }) => {
                assert_eq!(named, method, "the refusal names the method it refused");
            }
            other => panic!("{method} outside a transaction must refuse, got {other:?}"),
        }
    }
}

/// A transaction found open when another is begun is rolled back first, and the
/// new one runs: without that the `BEGIN` would be refused and the connection
/// would stay inside the abandoned transaction for the life of the process.
#[test]
fn an_abandoned_transaction_is_rolled_back_before_the_next_one_begins() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory database");
    let owner = OwnerId::new_random();
    let abandoned = AccountRecord {
        id: AccountId::new_random(),
        owner,
        title: "Abandoned".to_owned(),
        institution: None,
    };
    let kept = AccountRecord {
        id: AccountId::new_random(),
        owner,
        title: "Saved".to_owned(),
        institution: None,
    };

    // The abandon is logged at error level: work was lost, and the log must
    // account for it. The subscriber is global so the callsite's interest —
    // cached once per process — is decided with a subscriber present; under a
    // thread-local default the message is never formatted.
    let writer = LogWriter::default();
    let captured = writer.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this test binary sets the subscriber once");

    store
        .connection()
        .execute_batch("BEGIN IMMEDIATE")
        .expect("a transaction left open");
    store
        .upsert_account(&abandoned)
        .expect("written, not committed");

    store
        .with_immediate_transaction(
            |error| error,
            |store| {
                store.upsert_account(&kept)?;
                Ok(())
            },
        )
        .expect("the new transaction runs");

    let accounts = store.list_accounts(owner).expect("read back");
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.title.clone())
            .collect::<Vec<_>>(),
        vec!["Saved".to_owned()],
        "the abandoned transaction's write is rolled back and the new one's is kept"
    );

    let log = String::from_utf8(captured.0.lock().expect("log buffer").clone())
        .expect("UTF-8 tracing output");
    let line = log
        .lines()
        .find(|line| line.contains("ERROR") && line.contains("another was begun"))
        .unwrap_or_else(|| panic!("the abandon must be logged at error level: {log}"));
    assert!(
        line.contains("rolling it back first"),
        "the log says what it did with the abandoned work: {line}"
    );
}
