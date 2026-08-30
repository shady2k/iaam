//! CBR key rate through SOAP.
//! CBR key rate (design E3.2, section 8).
//!
//! The only documented machine interface for historical data is the
//! `DailyInfoWebServ` SOAP service. A full SOAP framework is unnecessary:
//! the envelope is static, and the response is parsed with the same
//! `quick-xml` used for exchange rates.

use iaam_core::numeric::decimal::Dec;
use iaam_http::{Destination, HttpRequest, RequestBody};
use quick_xml::Reader;
use quick_xml::events::Event;
use rust_decimal::Decimal;
use std::str::FromStr;
use time::format_description::well_known::Iso8601;
use time::{Date, Duration, OffsetDateTime};

use crate::error::MarketError;
use crate::observation::{KeyRateObservation, ObservedAt, TradeDate};
/// Service action. Without this header the service returns a refusal rather
/// than a parse error, and the reason is not obvious.
const SOAP_ACTION: &str = "http://web.cbr.ru/KeyRateXML";

/// Build a SOAP request for key-rate history.
#[must_use]
pub fn key_rate_request(from: Date, till: Date) -> HttpRequest {
    let envelope = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <KeyRateXML xmlns="http://web.cbr.ru/">
      <fromDate>{}T00:00:00</fromDate>
      <ToDate>{}T00:00:00</ToDate>
    </KeyRateXML>
  </soap:Body>
</soap:Envelope>"#,
        iso(from),
        iso(till)
    );
    HttpRequest::post(
        Destination::CbrDailyInfo,
        "/DailyInfoWebServ/DailyInfo.asmx",
        RequestBody::Xml(envelope),
    )
    .with_soap_action(SOAP_ACTION)
}

fn iso(date: Date) -> String {
    date.format(&Iso8601::DATE)
        .expect("date formats as ISO-8601")
}

/// Parse daily `DT`/`Rate` observations from `KeyRate/KR` elements.
///
/// Parse `DT` as `OffsetDateTime` so the input offset is checked and not
/// silently lost. The calendar day recorded by the source in its offset is
/// what matters for a rate, so UTC conversion is intentionally not performed.
pub fn parse_key_rate(
    xml: &str,
    observed_at: ObservedAt,
) -> Result<Vec<KeyRateObservation>, MarketError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut observations = Vec::new();
    let mut in_kr = false;
    let mut current_date = None;
    let mut current_rate = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"KR" => {
                if in_kr {
                    return Err(MarketError::Malformed("nested KR element".to_owned()));
                }
                in_kr = true;
                current_date = None;
                current_rate = None;
            }
            Ok(Event::Start(element)) if in_kr && element.local_name().as_ref() == b"DT" => {
                let value = reader
                    .read_text(element.name())
                    .map_err(|error| MarketError::Malformed(format!("DT: {error}")))?;
                let value = decode_text(&value)?;
                current_date = Some(parse_key_rate_date(&value)?);
            }
            Ok(Event::Start(element)) if in_kr && element.local_name().as_ref() == b"Rate" => {
                let value = reader
                    .read_text(element.name())
                    .map_err(|error| MarketError::Malformed(format!("Rate: {error}")))?;
                let value = decode_text(&value)?;
                current_rate = Some(parse_key_rate_decimal(&value)?);
            }
            Ok(Event::End(element)) if element.local_name().as_ref() == b"KR" => {
                if !in_kr {
                    return Err(MarketError::Malformed(
                        "closing KR without an opening element".to_owned(),
                    ));
                }
                let trade_date = current_date
                    .ok_or_else(|| MarketError::Malformed("KR has no DT".to_owned()))?;
                let rate = current_rate
                    .ok_or_else(|| MarketError::Malformed("KR has no Rate".to_owned()))?;
                observations.push(KeyRateObservation {
                    trade_date: TradeDate(trade_date),
                    observed_at,
                    rate,
                });
                in_kr = false;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(MarketError::Malformed(format!("SOAP XML: {error}")));
            }
        }
    }

    if in_kr {
        return Err(MarketError::Malformed(
            "response is truncated inside KR".to_owned(),
        ));
    }
    if observations.is_empty() {
        return Err(MarketError::Malformed(
            "response contains no KR elements".to_owned(),
        ));
    }
    Ok(observations)
}

fn parse_key_rate_date(value: &str) -> Result<Date, MarketError> {
    OffsetDateTime::parse(value.trim(), &time::format_description::well_known::Rfc3339)
        .map(|timestamp| timestamp.date())
        .map_err(|error| MarketError::Malformed(format!("date {value}: {error}")))
}

fn parse_key_rate_decimal(value: &str) -> Result<Dec, MarketError> {
    Decimal::from_str(value.trim())
        .map(Dec::new)
        .map_err(|error| MarketError::Malformed(format!("rate {value}: {error}")))
}

/// Convert element text to a string.
///
/// In `quick-xml` 0.41, `read_text` returns `BytesText`, not a string:
/// decoding is an explicit step. The version was raised from 0.38 because of
/// RUSTSEC-2026-0194 and RUSTSEC-2026-0195—quadratic time on duplicate
/// attributes and unbounded allocation for namespace declarations.
fn decode_text(value: &quick_xml::events::BytesText<'_>) -> Result<String, MarketError> {
    core::str::from_utf8(value.as_ref())
        .map(str::to_owned)
        .map_err(|error| MarketError::Malformed(format!("element text: {error}")))
}

/// How the interval's left boundary was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// First observation in the series: the date was observed, not derived.
    Observed,
    /// Non-trading days lie between adjacent observations.
    InferredAcrossNonTradingDays,
}

/// Rate-application interval derived from daily observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateInterval {
    pub from: Date,
    /// `None` for the last interval: it is open on the right.
    pub until: Option<Date>,
    pub rate: Dec,
    pub boundary: Boundary,
}

/// Derive intervals from observations sorted by date.
///
/// A CBR SOAP response usually runs from newer dates to older ones, so the
/// series is normalised by `trade_date` first. `until` is the first observation
/// of the next rate; if calendar days are missing before it, the new interval's
/// left boundary is marked as derived.
#[must_use]
pub fn derive_intervals(observations: &[KeyRateObservation]) -> Vec<RateInterval> {
    if observations.is_empty() {
        return Vec::new();
    }

    let mut sorted = observations.to_vec();
    sorted.sort_by_key(|observation| observation.trade_date);

    let mut intervals = Vec::new();
    let mut current = &sorted[0];
    let mut previous = current;
    let mut current_boundary = Boundary::Observed;
    for next in sorted.iter().skip(1) {
        if next.rate != current.rate {
            let gap = next.trade_date.0 - previous.trade_date.0;
            intervals.push(RateInterval {
                from: current.trade_date.0,
                until: Some(next.trade_date.0),
                rate: current.rate,
                boundary: current_boundary,
            });
            current = next;
            current_boundary = if gap <= Duration::days(1) {
                Boundary::Observed
            } else {
                Boundary::InferredAcrossNonTradingDays
            };
        }
        previous = next;
    }

    intervals.push(RateInterval {
        from: current.trade_date.0,
        until: None,
        rate: current.rate,
        boundary: current_boundary,
    });
    intervals
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    const FIXTURE: &str = include_str!("../../../../tests/fixtures/market/cbr-keyrate-soap.xml");

    fn observed() -> ObservedAt {
        ObservedAt(datetime!(2026-08-26 09:00:00 UTC))
    }

    #[test]
    fn the_envelope_carries_the_soap_action() {
        let request = key_rate_request(date!(2026 - 02 - 01), date!(2026 - 04 - 30));
        assert_eq!(
            request.soap_action(),
            Some("http://web.cbr.ru/KeyRateXML"),
            "without SOAPAction the service returns a refusal, not a parse error"
        );
    }

    #[test]
    fn the_soap_envelope_uses_iso_dates_for_both_bounds() {
        let request = key_rate_request(date!(2026 - 02 - 01), date!(2026 - 04 - 30));
        let body = request.body().expect("SOAP request must have a body");
        let payload = body.payload();

        assert!(payload.contains("<fromDate>2026-02-01T00:00:00</fromDate>"));
        assert!(payload.contains("<ToDate>2026-04-30T00:00:00</ToDate>"));
    }

    #[test]
    fn the_source_gives_business_day_observations_not_intervals() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("parsed");
        assert_eq!(observations.len(), 63);
        assert!(
            !observations.iter().any(|o| matches!(
                o.trade_date.0.weekday(),
                time::Weekday::Saturday | time::Weekday::Sunday
            )),
            "the series contains business days only"
        );
    }

    #[test]
    fn rate_elements_outside_key_rate_records_are_ignored() {
        let xml = r#"
            <Envelope>
                <Rate>not-a-rate</Rate>
                <KeyRate>
                    <KR>
                        <DT>2026-08-04T00:00:00+03:00</DT>
                        <Rate>16.00</Rate>
                    </KR>
                </KeyRate>
            </Envelope>
        "#;

        let observations = parse_key_rate(xml, observed()).expect("parsed");

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].trade_date, TradeDate(date!(2026 - 08 - 04)));
    }

    #[test]
    fn intervals_are_derived_and_their_boundaries_are_marked_inferred() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("parsed");
        let intervals = derive_intervals(&observations);
        // Three transitions in the fixture: 16.00 → 15.50 → 15.00 → 14.50.
        assert_eq!(intervals.len(), 4, "received {intervals:?}");
        // Each change occurs on the Monday after Friday: a weekend lies
        // between the last observation of the old rate and the first of the
        // new rate, and the source does not name the exact effective date.
        for interval in intervals.iter().skip(1) {
            assert_eq!(
                interval.boundary,
                Boundary::InferredAcrossNonTradingDays,
                "boundary {interval:?} must be marked as derived"
            );
        }
    }

    #[test]
    fn the_first_interval_starts_at_an_observed_date() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("parsed");
        let intervals = derive_intervals(&observations);
        let first = intervals.first().expect("at least one interval");
        assert_eq!(first.boundary, Boundary::Observed);
        assert_eq!(first.from, date!(2026 - 02 - 02));
    }

    #[test]
    fn the_last_interval_is_open_on_the_right() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("parsed");
        let intervals = derive_intervals(&observations);
        assert!(intervals.last().expect("interval").until.is_none());
    }
    #[test]
    fn adjacent_rate_change_has_an_observed_boundary() {
        let observations = [
            KeyRateObservation {
                trade_date: TradeDate(date!(2026 - 02 - 02)),
                observed_at: observed(),
                rate: Dec::new(Decimal::from_str("16.00").expect("rate")),
            },
            KeyRateObservation {
                trade_date: TradeDate(date!(2026 - 02 - 03)),
                observed_at: observed(),
                rate: Dec::new(Decimal::from_str("16.00").expect("rate")),
            },
            KeyRateObservation {
                trade_date: TradeDate(date!(2026 - 02 - 04)),
                observed_at: observed(),
                rate: Dec::new(Decimal::from_str("15.50").expect("rate")),
            },
        ];

        let intervals = derive_intervals(&observations);
        assert_eq!(intervals[1].boundary, Boundary::Observed);
        assert_eq!(intervals[1].from, date!(2026 - 02 - 04));
    }
}
