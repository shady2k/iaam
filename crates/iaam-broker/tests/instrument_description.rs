use iaam_broker::{finam, tinkoff};
use iaam_core::instrument::InstrumentKind;
use iaam_core::money::CurrencyCode;

#[test]
fn tinkoff_description_keeps_identity_and_native_board() {
    let description = tinkoff::parse_instrument(r#"{"instrument":{"isin":"XX0000000001","ticker":"TEST","name":"Invented share","instrumentType":"share","currency":"rub","classCode":"TQBR","lot":10}}"#).unwrap();
    assert_eq!(description.isin.as_deref(), Some("XX0000000001"));
    assert_eq!(description.kind, Some(InstrumentKind::Share));
    assert_eq!(description.currency, CurrencyCode::Rub);
    assert_eq!(description.board.as_deref(), Some("TQBR"));
    assert_eq!(description.lot, Some(10));
    assert_eq!(description.mic, None);
    assert_eq!(description.price_decimals, None);
}

#[test]
fn finam_description_reads_its_native_metadata() {
    let description = finam::parse_asset_description(r#"{"isin":"XX0000000002","ticker":"TEST","name":"Invented bond","type":"BOND","quote_currency":"RUB","mic":"MISX","board":"TQCB","lot_size":{"value":"1"},"decimals":4}"#).unwrap();
    assert_eq!(description.kind, Some(InstrumentKind::Bond));
    assert_eq!(description.mic.as_deref(), Some("MISX"));
    assert_eq!(description.board.as_deref(), Some("TQCB"));
    assert_eq!(description.lot, Some(1));
    assert_eq!(description.price_decimals, Some(4));
}

#[test]
fn absent_identity_and_unknown_kind_do_not_become_invented_values() {
    let description = tinkoff::parse_instrument(r#"{"instrument":{"isin":" ","ticker":"TEST","name":"Invented derivative","instrumentType":"futures","currency":"rub"}}"#).unwrap();
    assert_eq!(description.isin, None);
    assert_eq!(description.kind, None);
    assert_eq!(
        finam::parse_asset(r#"{"isin":"XX0000000003"}"#)
            .unwrap()
            .as_deref(),
        Some("XX0000000003")
    );
}

#[test]
fn incomplete_reference_data_and_invalid_metadata_are_refused() {
    assert!(
        tinkoff::parse_instrument(r#"{"instrument":{"ticker":"TEST","name":"Invented share"}}"#)
            .is_err()
    );
    assert!(finam::parse_asset_description(r#"{"ticker":"TEST","name":"Invented share","quote_currency":"RUB","lot_size":{"value":"1.5"}}"#).is_err());
    assert!(
        finam::parse_asset_description(
            r#"{"ticker":"TEST","name":"Invented share","quote_currency":"RUB","decimals":256}"#
        )
        .is_err()
    );
}
