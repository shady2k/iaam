//! Dictionary of channel operation kinds: funding, owner decisions, and reading.

use iaam_store::SqliteStore;
use iaam_store::broker_operation_kinds::BrokerOperationKind;
use iaam_store::documents::BrokerCode;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().expect("in-memory database")
}

fn tinkoff() -> BrokerCode {
    BrokerCode::parse("tinkoff").expect("broker code")
}

fn entry(source_kind: &str, kind: &str) -> BrokerOperationKind {
    BrokerOperationKind {
        source_kind: source_kind.to_owned(),
        kind: kind.to_owned(),
    }
}

#[test]
fn a_dictionary_is_read_back_whole() {
    let mut store = store();
    let outcome = store
        .extend_broker_operation_kinds(
            &tinkoff(),
            "operations.proto@2026-08",
            &[
                entry("OPERATION_TYPE_COUPON", "coupon"),
                entry("OPERATION_TYPE_BOND_REPAYMENT", "bond_amortisation"),
            ],
        )
        .expect("dictionary written");
    assert_eq!(outcome.added, 2);
    assert_eq!(outcome.already_known, 0);

    let dictionary = store.broker_operation_kinds(&tinkoff()).expect("read");
    assert_eq!(
        dictionary
            .get("OPERATION_TYPE_BOND_REPAYMENT")
            .map(String::as_str),
        Some("bond_amortisation")
    );
}

/// “Completed successfully” must differ from “did nothing”; otherwise
/// updating the dictionary when nothing is found looks like an update
/// with nothing to add.
#[test]
fn a_repeated_update_adds_nothing_and_says_so() {
    let mut store = store();
    let entries = [entry("OPERATION_TYPE_COUPON", "coupon")];
    store
        .extend_broker_operation_kinds(&tinkoff(), "first", &entries)
        .expect("first funding");
    let outcome = store
        .extend_broker_operation_kinds(&tinkoff(), "second", &entries)
        .expect("second funding");
    assert_eq!(outcome.added, 0);
    assert_eq!(outcome.already_known, 1);
}

/// A nightly run must not silently undo a manually added entry: an owner decision
/// is knowledge about the portfolio that
/// is not present in the contract.
#[test]
fn an_update_from_the_contract_does_not_overwrite_the_owners_decision() {
    let mut store = store();
    store
        .set_broker_operation_kind(&tinkoff(), &entry("OPERATION_TYPE_OVERNIGHT", "coupon"))
        .expect("owner decision");
    store
        .extend_broker_operation_kinds(
            &tinkoff(),
            "contract",
            &[entry("OPERATION_TYPE_OVERNIGHT", "commission")],
        )
        .expect("funding");

    let dictionary = store.broker_operation_kinds(&tinkoff()).expect("read");
    assert_eq!(
        dictionary
            .get("OPERATION_TYPE_OVERNIGHT")
            .map(String::as_str),
        Some("coupon"),
        "contract overwrote owner decision"
    );
}

/// The owner, conversely, may override the contract.
#[test]
fn the_owner_may_overrule_the_contract() {
    let mut store = store();
    store
        .extend_broker_operation_kinds(
            &tinkoff(),
            "contract",
            &[entry("OPERATION_TYPE_OVERNIGHT", "commission")],
        )
        .expect("funding");
    store
        .set_broker_operation_kind(&tinkoff(), &entry("OPERATION_TYPE_OVERNIGHT", "coupon"))
        .expect("owner decision");

    let dictionary = store.broker_operation_kinds(&tinkoff()).expect("read");
    assert_eq!(
        dictionary
            .get("OPERATION_TYPE_OVERNIGHT")
            .map(String::as_str),
        Some("coupon")
    );
}

/// The key is composite: one broker's code does not account for another's,
/// even if the strings match. `BUY` can mean
/// different things for two channels, and a dictionary without the broker in
#[test]
fn one_brokers_code_does_not_answer_for_another() {
    let mut store = store();
    let finam = BrokerCode::parse("finam").expect("broker code");
    store
        .extend_broker_operation_kinds(&tinkoff(), "contract", &[entry("BUY", "buy")])
        .expect("Tinkoff");
    store
        .extend_broker_operation_kinds(&finam, "contract", &[entry("BUY", "sell")])
        .expect("Finam");

    assert_eq!(
        store
            .broker_operation_kinds(&tinkoff())
            .expect("read")
            .get("BUY")
            .map(String::as_str),
        Some("buy")
    );
    assert_eq!(
        store
            .broker_operation_kinds(&finam)
            .expect("read")
            .get("BUY")
            .map(String::as_str),
        Some("sell")
    );
}

/// A type outside the closed list must be rejected by the schema: a string
/// "unknown type" in the dictionary would mean choosing not to parse it,
/// but no such choice was made—the absence of a row means "we don't know".
#[test]
fn a_kind_outside_the_vocabulary_is_refused_by_the_schema() {
    let mut store = store();
    let error = store.extend_broker_operation_kinds(
        &tinkoff(),
        "contract",
        &[entry("OPERATION_TYPE_MYSTERY", "other")],
    );
    assert!(
        error.is_err(),
        "the schema accepted a type outside the dictionary"
    );
}
