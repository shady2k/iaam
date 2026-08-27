//! Ключевая ставка ЦБ РФ через SOAP.
//! Ключевая ставка ЦБ РФ (раздел 8 дизайна E3.2).
//!
//! Единственный документированный машинный интерфейс истории —
//! SOAP-сервис `DailyInfoWebServ`. Полноценный SOAP-фреймворк не нужен:
//! конверт статический, ответ разбирается тем же `quick-xml`, что и курсы.

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

/// Действие сервиса. Без этого заголовка сервис отвечает отказом,
/// а не ошибкой разбора, и причина неочевидна.
const SOAP_ACTION: &str = "http://web.cbr.ru/KeyRateXML";

/// Формирует SOAP-запрос истории ключевой ставки.
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
        .expect("дата форматируется в ISO-8601")
}

/// Разбирает дневные наблюдения `DT`/`Rate` из элементов `KeyRate/KR`.
///
/// `DT` разбирается как `OffsetDateTime`, чтобы входной offset был
/// проверен и не потерян молча. В ставке важен календарный день,
/// записанный источником в его offset, поэтому UTC-конверсия намеренно
/// не выполняется.
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
                    return Err(MarketError::Malformed("вложенный элемент KR".to_owned()));
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
                        "закрывающий KR без открывающего".to_owned(),
                    ));
                }
                let trade_date =
                    current_date.ok_or_else(|| MarketError::Malformed("в KR нет DT".to_owned()))?;
                let rate = current_rate
                    .ok_or_else(|| MarketError::Malformed("в KR нет Rate".to_owned()))?;
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
        return Err(MarketError::Malformed("ответ оборван внутри KR".to_owned()));
    }
    if observations.is_empty() {
        return Err(MarketError::Malformed(
            "ответ не содержит элементов KR".to_owned(),
        ));
    }
    Ok(observations)
}

fn parse_key_rate_date(value: &str) -> Result<Date, MarketError> {
    OffsetDateTime::parse(value.trim(), &time::format_description::well_known::Rfc3339)
        .map(|timestamp| timestamp.date())
        .map_err(|error| MarketError::Malformed(format!("дата {value}: {error}")))
}

fn parse_key_rate_decimal(value: &str) -> Result<Dec, MarketError> {
    Decimal::from_str(value.trim())
        .map(Dec::new)
        .map_err(|error| MarketError::Malformed(format!("ставка {value}: {error}")))
}

/// Текст элемента в строку.
///
/// В `quick-xml` 0.41 `read_text` отдаёт `BytesText`, а не строку:
/// декодирование стало явным шагом. Версия поднята с 0.38 из-за
/// RUSTSEC-2026-0194 и RUSTSEC-2026-0195 — квадратичное время на
/// дубликатах атрибутов и неограниченное выделение памяти под
/// объявления пространств имён.
fn decode_text(value: &quick_xml::events::BytesText<'_>) -> Result<String, MarketError> {
    core::str::from_utf8(value.as_ref())
        .map(str::to_owned)
        .map_err(|error| MarketError::Malformed(format!("текст элемента: {error}")))
}

/// Как получена левая граница интервала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Первое наблюдение ряда: дата наблюдена, а не выведена.
    Observed,
    /// Между соседними наблюдениями лежат нерабочие дни.
    InferredAcrossNonTradingDays,
}

/// Интервал действия ставки, выводимый из дневных наблюдений.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateInterval {
    pub from: Date,
    /// `None` у последнего интервала: он открыт справа.
    pub until: Option<Date>,
    pub rate: Dec,
    pub boundary: Boundary,
}

/// Выводит интервалы из отсортированного по дате ряда наблюдений.
///
/// Входной SOAP-ответ ЦБ обычно идёт от новых дат к старым, поэтому
/// перед выводом ряд нормализуется по `trade_date`. Дата `until` — первое
/// наблюдение следующей ставки; если перед ним есть пропуск календарных
/// дней, левая граница нового интервала помечается как выведенная.
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
            "без SOAPAction сервис отвечает отказом, а не ошибкой разбора"
        );
    }

    #[test]
    fn the_soap_envelope_uses_iso_dates_for_both_bounds() {
        let request = key_rate_request(date!(2026 - 02 - 01), date!(2026 - 04 - 30));
        let body = request.body().expect("SOAP-запрос должен иметь тело");
        let payload = body.payload();

        assert!(payload.contains("<fromDate>2026-02-01T00:00:00</fromDate>"));
        assert!(payload.contains("<ToDate>2026-04-30T00:00:00</ToDate>"));
    }

    #[test]
    fn the_source_gives_business_day_observations_not_intervals() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        assert_eq!(observations.len(), 63);
        assert!(
            !observations.iter().any(|o| matches!(
                o.trade_date.0.weekday(),
                time::Weekday::Saturday | time::Weekday::Sunday
            )),
            "в ряду только рабочие дни"
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

        let observations = parse_key_rate(xml, observed()).expect("разбор");

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].trade_date, TradeDate(date!(2026 - 08 - 04)));
    }

    #[test]
    fn intervals_are_derived_and_their_boundaries_are_marked_inferred() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        let intervals = derive_intervals(&observations);
        // Три перехода в фикстуре: 16,00 → 15,50 → 15,00 → 14,50.
        assert_eq!(intervals.len(), 4, "получено {intervals:?}");
        // Каждая смена приходится на понедельник после пятницы: между
        // последним наблюдением старой ставки и первым наблюдением новой
        // лежат выходные, и точная дата вступления источником не названа.
        for interval in intervals.iter().skip(1) {
            assert_eq!(
                interval.boundary,
                Boundary::InferredAcrossNonTradingDays,
                "граница {interval:?} обязана быть помечена выведенной"
            );
        }
    }

    #[test]
    fn the_first_interval_starts_at_an_observed_date() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        let intervals = derive_intervals(&observations);
        let first = intervals.first().expect("хотя бы один интервал");
        assert_eq!(first.boundary, Boundary::Observed);
        assert_eq!(first.from, date!(2026 - 02 - 02));
    }

    #[test]
    fn the_last_interval_is_open_on_the_right() {
        let observations = parse_key_rate(FIXTURE, observed()).expect("разбор");
        let intervals = derive_intervals(&observations);
        assert!(intervals.last().expect("интервал").until.is_none());
    }
    #[test]
    fn adjacent_rate_change_has_an_observed_boundary() {
        let observations = [
            KeyRateObservation {
                trade_date: TradeDate(date!(2026 - 02 - 02)),
                observed_at: observed(),
                rate: Dec::new(Decimal::from_str("16.00").expect("ставка")),
            },
            KeyRateObservation {
                trade_date: TradeDate(date!(2026 - 02 - 03)),
                observed_at: observed(),
                rate: Dec::new(Decimal::from_str("16.00").expect("ставка")),
            },
            KeyRateObservation {
                trade_date: TradeDate(date!(2026 - 02 - 04)),
                observed_at: observed(),
                rate: Dec::new(Decimal::from_str("15.50").expect("ставка")),
            },
        ];

        let intervals = derive_intervals(&observations);
        assert_eq!(intervals[1].boundary, Boundary::Observed);
        assert_eq!(intervals[1].from, date!(2026 - 02 - 04));
    }
}
