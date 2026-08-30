//! Parsing CBR exchange rates.
//!
//! Two source conventions are easy to miss, and both fail silently:
//! the response uses `windows-1251`, and the decimal separator is a comma.

use encoding_rs::WINDOWS_1251;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use rust_decimal::Decimal;
use time::{Date, Month, Weekday};

use crate::error::MarketError;
use crate::observation::{FxObservation, ObservedAt, TradeDate};

/// One raw CBR record before mapping to a domain currency.
///
/// `char_code` intentionally remains a string: the CBR dictionary is broader than
/// [`CurrencyCode`], and an unknown currency must not break the entire
/// response. For dynamic responses the field carries the CBR identifier,
/// because `Record` elements have no `CharCode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CbrRate {
    pub char_code: String,
    pub nominal: u32,
    pub value: Decimal,
    pub unit_rate: Decimal,
    pub date: Date,
}

/// CBR response bytes as a string.
///
/// A separate function rather than `String::from_utf8_lossy`: lossy decoding would insert
/// question marks instead of currency names and make corruption
/// invisible. `windows-1251` is declared in the response preamble.
#[must_use]
pub fn decode_cp1251(bytes: &[u8]) -> String {
    let (text, _, _) = WINDOWS_1251.decode(bytes);
    text.into_owned()
}

/// Number in the CBR convention: decimal comma.
///
/// A dot is rejected intentionally. Accepting both separators would mean
/// we would stop noticing that the source changed convention—and a change
/// in the source-of-truth convention must be a refusal, not a guess.
pub(crate) fn parse_cbr_decimal(value: &str) -> Result<Decimal, MarketError> {
    if !value.contains(',') && value.contains('.') {
        return Err(MarketError::Malformed(format!(
            "CBR separator is a comma, got {value}"
        )));
    }
    value
        .replace(',', ".")
        .parse::<Decimal>()
        .map_err(|error| MarketError::Malformed(format!("number {value}: {error}")))
}

/// Date in the CBR convention: `DD.MM.YYYY`.
pub(crate) fn parse_cbr_date(value: &str) -> Result<Date, MarketError> {
    let parts: Vec<&str> = value.split('.').collect();
    let [day, month, year] = parts.as_slice() else {
        return Err(MarketError::Malformed(format!(
            "CBR date expected as DD.MM.YYYY, got {value}"
        )));
    };
    let parsed = |part: &str| {
        part.parse::<u16>()
            .map_err(|error| MarketError::Malformed(format!("date {value}: {error}")))
    };
    let month = Month::try_from(u8::try_from(parsed(month)?).unwrap_or(0))
        .map_err(|error| MarketError::Malformed(format!("month {value}: {error}")))?;
    Date::from_calendar_date(
        i32::from(parsed(year)?),
        month,
        u8::try_from(parsed(day)?).unwrap_or(0),
    )
    .map_err(|error| MarketError::Malformed(format!("date {value}: {error}")))
}

/// Parse daily XML into the raw layer without dropping unknown currencies.
pub fn parse_daily_raw(xml: &str) -> Result<Vec<CbrRate>, MarketError> {
    parse_rates(xml, RateContainer::Daily)
}

/// Parse daily rates and retain only currencies known to core.
pub fn parse_daily(xml: &str, observed_at: ObservedAt) -> Result<Vec<FxObservation>, MarketError> {
    let raw = parse_daily_raw(xml)?;
    Ok(raw
        .into_iter()
        .filter_map(|rate| {
            // CBR publishes more currencies than the domain core knows.
            // Unknown codes are skipped intentionally, rather than treated as
            // an error in the whole response.
            currency_from_iso(&rate.char_code).map(|from| FxObservation {
                from,
                to: CurrencyCode::Rub,
                trade_date: TradeDate(rate.date),
                observed_at,
                nominal: rate.nominal,
                value: Dec::new(rate.value),
                unit_rate: Dec::new(rate.unit_rate),
            })
        })
        .collect())
}

/// Parse one currency series, dropping weekends.
pub fn parse_dynamic(
    xml: &str,
    to: CurrencyCode,
    observed_at: ObservedAt,
) -> Result<Vec<FxObservation>, MarketError> {
    let raw = parse_rates(xml, RateContainer::Dynamic)?;
    Ok(raw
        .into_iter()
        .filter(|rate| !matches!(rate.date.weekday(), Weekday::Saturday | Weekday::Sunday))
        .filter_map(|rate| {
            // XML_dynamic records contain only a CBR ID. Known
            // identifiers map to core’s exhaustive enum;
            // the remaining records, like unknown CharCode values, are skipped.
            currency_from_cbr_id(&rate.char_code).map(|from| FxObservation {
                from,
                to,
                trade_date: TradeDate(rate.date),
                observed_at,
                nominal: rate.nominal,
                value: Dec::new(rate.value),
                unit_rate: Dec::new(rate.unit_rate),
            })
        })
        .collect())
}

#[derive(Clone, Copy)]
enum RateContainer {
    Daily,
    Dynamic,
}

#[derive(Clone, Copy)]
enum Field {
    CharCode,
    Nominal,
    Value,
    UnitRate,
}

#[derive(Default)]
struct RateBuilder {
    char_code: Option<String>,
    nominal: Option<u32>,
    value: Option<Decimal>,
    unit_rate: Option<Decimal>,
    date: Option<Date>,
}

impl RateBuilder {
    fn finish(self) -> Result<CbrRate, MarketError> {
        let char_code = self
            .char_code
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                MarketError::Malformed("CBR record is missing a currency code".into())
            })?;
        let nominal = self
            .nominal
            .filter(|value| *value > 0)
            .ok_or_else(|| MarketError::Malformed("CBR record is missing a nominal".into()))?;
        let value = self
            .value
            .ok_or_else(|| MarketError::Malformed("CBR record is missing Value".into()))?;
        let unit_rate = self
            .unit_rate
            .ok_or_else(|| MarketError::Malformed("CBR record is missing VunitRate".into()))?;
        let date = self
            .date
            .ok_or_else(|| MarketError::Malformed("CBR record is missing a date".into()))?;
        Ok(CbrRate {
            char_code,
            nominal,
            value,
            unit_rate,
            date,
        })
    }

    fn set(&mut self, field: Field, text: &str) -> Result<(), MarketError> {
        match field {
            Field::CharCode => self.char_code = Some(text.to_owned()),
            Field::Nominal => {
                self.nominal =
                    Some(text.parse::<u32>().map_err(|error| {
                        MarketError::Malformed(format!("nominal {text}: {error}"))
                    })?)
            }
            Field::Value => self.value = Some(parse_cbr_decimal(text)?),
            Field::UnitRate => self.unit_rate = Some(parse_cbr_decimal(text)?),
        }
        Ok(())
    }
}

fn parse_rates(xml: &str, container: RateContainer) -> Result<Vec<CbrRate>, MarketError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut rates = Vec::new();
    let mut root_date = None;
    let mut root_id = None;
    let mut current = None;
    let mut field = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = start.name();
                if name.as_ref() == b"ValCurs" {
                    root_date = attribute(&start, b"Date")?
                        .map(|date| parse_cbr_date(&date))
                        .transpose()?;
                    root_id = attribute(&start, b"ID")?;
                } else if is_container(name.as_ref(), container) {
                    if current.is_some() {
                        return Err(MarketError::Malformed("nested CBR record".into()));
                    }
                    let mut builder = RateBuilder {
                        char_code: match container {
                            RateContainer::Daily => None,
                            RateContainer::Dynamic => {
                                attribute(&start, b"Id")?.or_else(|| root_id.clone())
                            }
                        },
                        date: match container {
                            RateContainer::Daily => root_date,
                            RateContainer::Dynamic => attribute(&start, b"Date")?
                                .map(|date| parse_cbr_date(&date))
                                .transpose()?,
                        },
                        ..RateBuilder::default()
                    };
                    if matches!(container, RateContainer::Daily) {
                        builder.char_code = attribute(&start, b"CharCode")?;
                    }
                    current = Some(builder);
                } else if current.is_some() {
                    field = field_for(name.as_ref());
                }
            }
            Ok(Event::Text(text)) => {
                if let (Some(builder), Some(field)) = (&mut current, field) {
                    let value = text.decode().map_err(|error| {
                        MarketError::Malformed(format!("CBR XML text: {error}"))
                    })?;
                    builder.set(field, value.trim())?;
                }
            }
            Ok(Event::End(end)) => {
                let name = end.name();
                if is_container(name.as_ref(), container) {
                    let builder = current.take().ok_or_else(|| {
                        MarketError::Malformed("closed incomplete CBR record".into())
                    })?;
                    rates.push(builder.finish()?);
                    field = None;
                } else if field_for(name.as_ref()).is_some() {
                    field = None;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(MarketError::Malformed(format!("CBR XML: {error}")));
            }
        }
    }

    if current.is_some() {
        return Err(MarketError::Malformed("truncated CBR record".into()));
    }
    if matches!(container, RateContainer::Daily) && root_date.is_none() {
        return Err(MarketError::Malformed(
            "CBR daily response is missing Date".into(),
        ));
    }
    Ok(rates)
}

fn is_container(name: &[u8], container: RateContainer) -> bool {
    match container {
        RateContainer::Daily => name == b"Valute",
        RateContainer::Dynamic => name == b"Record",
    }
}

fn field_for(name: &[u8]) -> Option<Field> {
    match name {
        b"CharCode" => Some(Field::CharCode),
        b"Nominal" => Some(Field::Nominal),
        b"Value" => Some(Field::Value),
        b"VunitRate" => Some(Field::UnitRate),
        _ => None,
    }
}

fn attribute(start: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>, MarketError> {
    for attribute in start.attributes() {
        let attribute = attribute
            .map_err(|error| MarketError::Malformed(format!("CBR XML attribute: {error}")))?;
        if attribute.key.as_ref() == key {
            return attribute
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(|value| Some(value.into_owned()))
                .map_err(|error| MarketError::Malformed(format!("CBR XML attribute: {error}")));
        }
    }
    Ok(None)
}

fn currency_from_iso(code: &str) -> Option<CurrencyCode> {
    match code {
        "RUB" => Some(CurrencyCode::Rub),
        "USD" => Some(CurrencyCode::Usd),
        "EUR" => Some(CurrencyCode::Eur),
        "CNY" => Some(CurrencyCode::Cny),
        "XAU" => Some(CurrencyCode::Xau),
        _ => None,
    }
}

fn currency_from_cbr_id(id: &str) -> Option<CurrencyCode> {
    match id {
        "R01235" => Some(CurrencyCode::Usd),
        "R01239" => Some(CurrencyCode::Eur),
        "R01375" => Some(CurrencyCode::Cny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::money::CurrencyCode;
    use time::macros::{date, datetime};

    const DAILY: &[u8] = include_bytes!("../../../../tests/fixtures/market/cbr-xml-daily.xml");
    const DYNAMIC: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/cbr-xml-dynamic-usd.xml");

    fn observed() -> ObservedAt {
        ObservedAt(datetime!(2026-08-26 09:00:00 UTC))
    }

    #[test]
    fn the_response_is_cp1251_and_utf8_decoding_would_fail() {
        // The response preamble declares windows-1251, and currency names contain
        // bytes that UTF-8 rejects.
        let bytes = DAILY.to_vec();
        assert!(
            core::str::from_utf8(&bytes).is_err(),
            "fixture is no longer cp1251—it was replaced"
        );
        let text = decode_cp1251(DAILY);
        assert!(
            text.contains("Австралийский доллар"),
            "decoding produced no Cyrillic text"
        );
    }

    #[test]
    fn a_decimal_comma_is_the_source_convention_not_a_typo() {
        assert_eq!(
            parse_cbr_decimal("85,1293").expect("number").to_string(),
            "85.1293"
        );
        assert!(
            parse_cbr_decimal("85.1293").is_err(),
            "a dot is not the CBR convention"
        );
    }

    #[test]
    fn mixed_decimal_separators_report_a_malformed_number() {
        let error = parse_cbr_decimal("85,1293.").expect_err("mixed separators are forbidden");

        match error {
            MarketError::Malformed(message) => {
                assert!(
                    message.starts_with("number 85,1293.:"),
                    "expected a number parse error, got: {message}"
                );
            }
            other => panic!("expected a number error, got: {other:?}"),
        }
    }

    #[test]
    fn zero_nominal_is_rejected_as_an_invalid_cbr_record() {
        let error = RateBuilder {
            char_code: Some("USD".to_owned()),
            nominal: Some(0),
            value: Some(Decimal::ONE),
            unit_rate: Some(Decimal::ONE),
            date: Some(date!(2026 - 08 - 04)),
        }
        .finish()
        .expect_err("zero nominal is not a CBR record");

        assert_eq!(
            error,
            MarketError::Malformed("CBR record is missing a nominal".to_owned())
        );
    }

    #[test]
    fn nominal_and_unit_rate_are_both_kept() {
        // Checked in the RAW layer, not observations, and this is not a workaround:
        // for every currency known to core (RUB, USD, EUR, CNY), CBR nominal
        // is one, so value/unit_rate cannot differ observably there.
        // Nominals above one occur for yen (100) and lira (10), currencies
        // that core does not know. That is exactly why the raw layer exists:
        // parsing must be testable regardless of which currencies
        // the system accounts for today.
        let text = decode_cp1251(DAILY);
        let raw = parse_daily_raw(&text).expect("parsing");
        let jpy = raw
            .iter()
            .find(|r| r.char_code == "JPY")
            .expect("yen is in the CBR dictionary");
        assert_eq!(jpy.nominal, 100, "CBR publishes yen per one hundred units");
        assert_ne!(
            jpy.value, jpy.unit_rate,
            "nominal and per-unit values are equal—the nominal was lost"
        );
    }

    #[test]
    fn a_currency_the_core_does_not_know_is_skipped_not_an_error() {
        // The CBR dictionary contains dozens of currencies the system
        // does not account for. Calling them errors would break parsing
        // of the whole response because of a currency nobody needs.
        let text = decode_cp1251(DAILY);
        let raw = parse_daily_raw(&text).expect("parsing");
        let observations = parse_daily(&text, observed()).expect("parsing");
        assert!(
            !observations.is_empty(),
            "daily response must yield known currencies, not only omissions"
        );
        for expected in [CurrencyCode::Usd, CurrencyCode::Eur, CurrencyCode::Cny] {
            assert!(
                observations
                    .iter()
                    .any(|observation| observation.from == expected),
                "known currency {expected:?} did not appear in observations"
            );
        }

        assert!(
            raw.len() > observations.len(),
            "CBR dictionary has more currencies than core knows: {} versus {}",
            raw.len(),
            observations.len()
        );
        assert!(
            raw.iter().any(|r| r.char_code == "JPY"),
            "yen exists in the raw layer"
        );
        assert!(
            observations.iter().all(|o| o.from != CurrencyCode::Rub),
            "rouble is not a source currency in CBR quotes"
        );
    }

    #[test]
    fn every_supported_iso_currency_maps_to_its_domain_code() {
        let cases = [
            ("RUB", CurrencyCode::Rub),
            ("USD", CurrencyCode::Usd),
            ("EUR", CurrencyCode::Eur),
            ("CNY", CurrencyCode::Cny),
            ("XAU", CurrencyCode::Xau),
        ];

        for (source, expected) in cases {
            assert_eq!(
                currency_from_iso(source),
                Some(expected),
                "source code {source} must be known to core"
            );
        }
    }

    #[test]
    fn an_unknown_iso_currency_is_not_mapped() {
        assert_eq!(currency_from_iso("JPY"), None);
    }

    #[test]
    fn every_supported_cbr_id_maps_to_its_domain_code() {
        let cases = [
            ("R01235", CurrencyCode::Usd),
            ("R01239", CurrencyCode::Eur),
            ("R01375", CurrencyCode::Cny),
        ];

        for (source, expected) in cases {
            assert_eq!(
                currency_from_cbr_id(source),
                Some(expected),
                "CBR identifier {source} must be known to core"
            );
        }
    }

    #[test]
    fn an_unknown_cbr_id_is_not_mapped() {
        assert_eq!(currency_from_cbr_id("R99999"), None);
    }

    #[test]
    fn the_series_covers_business_days_only() {
        let text = decode_cp1251(DYNAMIC);
        let series = parse_dynamic(&text, CurrencyCode::Rub, observed()).expect("parsing");
        assert!(!series.is_empty());
        let has_weekend = series.iter().any(|o| {
            matches!(
                o.trade_date.0.weekday(),
                time::Weekday::Saturday | time::Weekday::Sunday
            )
        });
        assert!(
            !has_weekend,
            "the CBR series has no weekends—a Sunday rate does not exist"
        );
    }

    #[test]
    fn the_source_date_format_is_dotted_not_iso() {
        assert_eq!(
            parse_cbr_date("04.08.2026").expect("date"),
            date!(2026 - 08 - 04)
        );
        assert!(parse_cbr_date("2026-08-04").is_err());
    }
}
