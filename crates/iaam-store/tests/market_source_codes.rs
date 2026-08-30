//! Market-source code dictionary: seeding, owner decision, reading.

use iaam_store::SqliteStore;
use iaam_store::market_source_codes::SourceCodeEntry;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().expect("in-memory database")
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
    // One source provides SUR in the issue description and RUB in the schedule for the same
    // issue. Without the dictionary, these are two different currencies, and positions diverge.
    let mut store = store();
    store
        .extend_market_source_codes(
            "moex-iss",
            "source profile 2026-08-27",
            &[
                entry("currency", "SUR", "RUB"),
                entry("currency", "RUB", "RUB"),
            ],
        )
        .expect("seeding");
    let dictionary = store
        .market_source_codes("moex-iss", "currency")
        .expect("reading");
    assert_eq!(dictionary.get("SUR").map(String::as_str), Some("RUB"));
    assert_eq!(dictionary.get("RUB").map(String::as_str), Some("RUB"));
}

#[test]
fn seeding_does_not_override_an_owner_decision() {
    // Otherwise, the owner's decision would be overridden whenever the source was registered,
    // and the discrepancy would be indistinguishable from the decision.
    let mut store = store();
    store
        .set_market_source_code("moex-iss", &entry("offer_kind", "Оферта", "put_option"))
        .expect("owner decision");
    let outcome = store
        .extend_market_source_codes(
            "moex-iss",
            "source profile 2026-08-27",
            &[entry("offer_kind", "Оферта", "call_option")],
        )
        .expect("seeding");
    assert_eq!(outcome.added, 0);
    assert_eq!(outcome.already_known, 1);
    let dictionary = store
        .market_source_codes("moex-iss", "offer_kind")
        .expect("reading");
    assert_eq!(
        dictionary.get("Оферта").map(String::as_str),
        Some("put_option")
    );
}

#[test]
fn an_unknown_code_is_absent_rather_than_other() {
    // “Code not found in the dictionary” is represented by the absence of a string. The 'other' member
    // would mean that a decision was made not to parse it—but no such decision was made.
    let store = store();
    let dictionary = store
        .market_source_codes("moex-iss", "offer_kind")
        .expect("reading");
    assert!(!dictionary.contains_key("Досрочное погашение"));
}
