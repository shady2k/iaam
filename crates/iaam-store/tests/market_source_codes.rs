//! Словарь кодов рыночного источника: засев, решение владельца, чтение.

use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().expect("база в памяти")
}

fn entry(domain: &str, code: &str, meaning: &str) -> SourceCodeEntry {
    SourceCodeEntry {
        domain: domain.to_owned(),
        source_code: code.to_owned(),
        meaning: meaning.to_owned(),
    }
}

#[test]
fn both_rouble_codes_of_one_source_mean_one_currency() {
    // Один источник даёт SUR в описании выпуска и RUB в графике того же
    // выпуска. Без словаря это две разные валюты, и позиции разъезжаются.
    let mut store = store();
    store
        .extend_market_source_codes(
            "moex-iss",
            "профиль источника 2026-08-27",
            &[
                entry("currency", "SUR", "RUB"),
                entry("currency", "RUB", "RUB"),
            ],
        )
        .expect("засев");
    let dictionary = store
        .market_source_codes("moex-iss", "currency")
        .expect("чтение");
    assert_eq!(dictionary.get("SUR").map(String::as_str), Some("RUB"));
    assert_eq!(dictionary.get("RUB").map(String::as_str), Some("RUB"));
}

#[test]
fn seeding_does_not_override_an_owner_decision() {
    // Иначе решение владельца отменялось бы при каждом заведении источника,
    // и расхождение было бы неотличимо от решения.
    let mut store = store();
    store
        .set_market_source_code("moex-iss", &entry("offer_kind", "Оферта", "put_option"))
        .expect("решение владельца");
    let outcome = store
        .extend_market_source_codes(
            "moex-iss",
            "профиль источника 2026-08-27",
            &[entry("offer_kind", "Оферта", "call_option")],
        )
        .expect("засев");
    assert_eq!(outcome.added, 0);
    assert_eq!(outcome.already_known, 1);
    let dictionary = store
        .market_source_codes("moex-iss", "offer_kind")
        .expect("чтение");
    assert_eq!(
        dictionary.get("Оферта").map(String::as_str),
        Some("put_option")
    );
}

#[test]
fn an_unknown_code_is_absent_rather_than_other() {
    // «Кода нет в словаре» выражается отсутствием строки. Член 'other'
    // означал бы принятое решение не разбирать — а такого не принимали.
    let store = store();
    let dictionary = store
        .market_source_codes("moex-iss", "offer_kind")
        .expect("чтение");
    assert!(!dictionary.contains_key("Досрочное погашение"));
}
