//! Словарь видов операций канала: пополнение, решение владельца, чтение.

use iaam_store::SqliteStore;
use iaam_store::broker_operation_kinds::BrokerOperationKind;
use iaam_store::documents::BrokerCode;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().expect("база в памяти")
}

fn tinkoff() -> BrokerCode {
    BrokerCode::parse("tinkoff").expect("код брокера")
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
        .expect("словарь записан");
    assert_eq!(outcome.added, 2);
    assert_eq!(outcome.already_known, 0);

    let dictionary = store.broker_operation_kinds(&tinkoff()).expect("чтение");
    assert_eq!(
        dictionary
            .get("OPERATION_TYPE_BOND_REPAYMENT")
            .map(String::as_str),
        Some("bond_amortisation")
    );
}

/// «Прошло успешно» обязано отличаться от «не сделало ничего»: иначе
/// обновление словаря, переставшее что-либо находить, выглядит как
/// обновление, которому нечего добавить.
#[test]
fn a_repeated_update_adds_nothing_and_says_so() {
    let mut store = store();
    let entries = [entry("OPERATION_TYPE_COUPON", "coupon")];
    store
        .extend_broker_operation_kinds(&tinkoff(), "первый", &entries)
        .expect("первое пополнение");
    let outcome = store
        .extend_broker_operation_kinds(&tinkoff(), "второй", &entries)
        .expect("второе пополнение");
    assert_eq!(outcome.added, 0);
    assert_eq!(outcome.already_known, 1);
}

/// Ночной прогон не имеет права бесшумно отменить разбор, заведённый
/// руками: решение владельца — это знание о портфеле, которого
/// в контракте нет.
#[test]
fn an_update_from_the_contract_does_not_overwrite_the_owners_decision() {
    let mut store = store();
    store
        .set_broker_operation_kind(&tinkoff(), &entry("OPERATION_TYPE_OVERNIGHT", "coupon"))
        .expect("решение владельца");
    store
        .extend_broker_operation_kinds(
            &tinkoff(),
            "контракт",
            &[entry("OPERATION_TYPE_OVERNIGHT", "commission")],
        )
        .expect("пополнение");

    let dictionary = store.broker_operation_kinds(&tinkoff()).expect("чтение");
    assert_eq!(
        dictionary
            .get("OPERATION_TYPE_OVERNIGHT")
            .map(String::as_str),
        Some("coupon"),
        "контракт затёр решение владельца"
    );
}

/// Владелец, наоборот, вправе перекрыть контракт.
#[test]
fn the_owner_may_overrule_the_contract() {
    let mut store = store();
    store
        .extend_broker_operation_kinds(
            &tinkoff(),
            "контракт",
            &[entry("OPERATION_TYPE_OVERNIGHT", "commission")],
        )
        .expect("пополнение");
    store
        .set_broker_operation_kind(&tinkoff(), &entry("OPERATION_TYPE_OVERNIGHT", "coupon"))
        .expect("решение владельца");

    let dictionary = store.broker_operation_kinds(&tinkoff()).expect("чтение");
    assert_eq!(
        dictionary
            .get("OPERATION_TYPE_OVERNIGHT")
            .map(String::as_str),
        Some("coupon")
    );
}

/// Ключ составной: код одного брокера не отвечает за код другого,
/// даже если строки совпали. `BUY` у двух каналов может означать
/// разное, и словарь без брокера в ключе слил бы их в один.
#[test]
fn one_brokers_code_does_not_answer_for_another() {
    let mut store = store();
    let finam = BrokerCode::parse("finam").expect("код брокера");
    store
        .extend_broker_operation_kinds(&tinkoff(), "контракт", &[entry("BUY", "buy")])
        .expect("тинькофф");
    store
        .extend_broker_operation_kinds(&finam, "контракт", &[entry("BUY", "sell")])
        .expect("финам");

    assert_eq!(
        store
            .broker_operation_kinds(&tinkoff())
            .expect("чтение")
            .get("BUY")
            .map(String::as_str),
        Some("buy")
    );
    assert_eq!(
        store
            .broker_operation_kinds(&finam)
            .expect("чтение")
            .get("BUY")
            .map(String::as_str),
        Some("sell")
    );
}

/// Вид вне закрытого списка обязан быть отказом схемы: строка
/// «неизвестный вид» в словаре означала бы решение не разбирать,
/// а такого решения не принимали — отсутствие строки и есть «не знаем».
#[test]
fn a_kind_outside_the_vocabulary_is_refused_by_the_schema() {
    let mut store = store();
    let error = store.extend_broker_operation_kinds(
        &tinkoff(),
        "контракт",
        &[entry("OPERATION_TYPE_MYSTERY", "other")],
    );
    assert!(error.is_err(), "схема приняла вид вне словаря");
}
