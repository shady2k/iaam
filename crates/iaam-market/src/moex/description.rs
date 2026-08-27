//! Разбор описания выпуска MOEX ISS.
//!
//! Ответ приходит парами «имя поля → значение», а не табличной строкой.
//!
//! Базы начисления дней и календаря в ответе нет вовсе — ни здесь, ни в
//! графике. Это `Unknown`, а не повод для значения по умолчанию:
//! подставленный day-count даёт правдоподобно неверный НКД, которого не
//! покажет ни один тест на бумаге с целым числом периодов.
//!
//! Текущий номинал источник даёт, и он сюда НЕ попадает: остаток выводится
//! из первоначального номинала и ряда возвратов, а два источника истины
//! расходятся молча.

use std::collections::BTreeMap;

use iaam_http::{Destination, HttpRequest};
use rust_decimal::Decimal;
use serde_json::Value;
use time::Date;
use time::format_description::well_known::Iso8601;

use iaam_core::ids::InstrumentId;
use iaam_core::numeric::decimal::Dec;

use crate::error::MarketError;
use crate::observation::ObservedAt;
use crate::schedule::Knowledge;
use crate::schedule::terms::{DefaultFlags, IssueTerms};

/// Запрос описания выпуска.
#[must_use]
pub fn terms_request(secid: &str) -> HttpRequest {
    let path = format!("/iss/securities/{secid}.json");
    HttpRequest::get(Destination::MoexIss, &path)
        .with_query("iss.meta", "off")
        .with_query("iss.only", "description")
}

fn fields(root: &Value) -> Result<BTreeMap<String, String>, MarketError> {
    let node = root
        .get("description")
        .ok_or_else(|| MarketError::Malformed("нет блока description".to_owned()))?;
    let columns = node
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет columns у description".to_owned()))?;
    let name_at = columns
        .iter()
        .position(|column| column.as_str() == Some("name"))
        .ok_or_else(|| MarketError::Malformed("нет колонки name".to_owned()))?;
    let value_at = columns
        .iter()
        .position(|column| column.as_str() == Some("value"))
        .ok_or_else(|| MarketError::Malformed("нет колонки value".to_owned()))?;
    let data = node
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет data у description".to_owned()))?;
    let mut map = BTreeMap::new();
    for row in data {
        let (Some(name), Some(value)) = (row.get(name_at), row.get(value_at)) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let value = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        if let Some(name) = name.as_str() {
            map.insert(name.to_owned(), value);
        }
    }
    Ok(map)
}

fn flag(fields: &BTreeMap<String, String>, name: &str) -> bool {
    fields.get(name).map(String::as_str) == Some("1")
}

/// Разобрать описание выпуска в снимок условий.
pub fn parse_description(
    body: &[u8],
    instrument: InstrumentId,
    observed_at: ObservedAt,
) -> Result<IssueTerms, MarketError> {
    let root: Value =
        serde_json::from_slice(body).map_err(|error| MarketError::Malformed(error.to_string()))?;
    let fields = fields(&root)?;

    let maturity_date = match fields.get("MATDATE") {
        Some(text) => Knowledge::Known(
            Date::parse(text, &Iso8601::DATE)
                .map_err(|_| MarketError::Malformed(format!("MATDATE не дата: {text}")))?,
        ),
        None => Knowledge::Unknown,
    };
    let initial_face_value =
        match fields.get("INITIALFACEVALUE") {
            Some(text) => Knowledge::Known(Dec::new(text.parse::<Decimal>().map_err(|_| {
                MarketError::Malformed(format!("INITIALFACEVALUE не число: {text}"))
            })?)),
            None => Knowledge::Unknown,
        };
    let coupon_periods_per_year = match fields.get("COUPONFREQUENCY") {
        Some(text) => Knowledge::Known(
            text.parse::<u32>()
                .map_err(|_| MarketError::Malformed(format!("COUPONFREQUENCY не целое: {text}")))?,
        ),
        None => Knowledge::Unknown,
    };

    Ok(IssueTerms {
        instrument,
        observed_at,
        // Источник даты вступления условий в силу не сообщает.
        // Подставить observed_at значит выдать догадку за факт.
        effective_from: Knowledge::Unknown,
        maturity_date,
        initial_face_value,
        face_currency_code: fields
            .get("FACEUNIT")
            .cloned()
            .map_or(Knowledge::Unknown, Knowledge::Known),
        coupon_periods_per_year,
        // Источник не даёт ни того, ни другого — ни здесь, ни в графике.
        day_count: Knowledge::Unknown,
        calendar: Knowledge::Unknown,
        default_flags: DefaultFlags {
            declared: flag(&fields, "HASDEFAULT"),
            technical: flag(&fields, "HASTECHNICALDEFAULT"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::ids::InstrumentId;
    use time::macros::{date, datetime};

    const BODY: &str = r#"{
      "description": {
        "columns": ["name", "title", "value"],
        "data": [
          ["MATDATE", "Дата погашения", "2036-02-06"],
          ["INITIALFACEVALUE", "Первоначальная номинальная стоимость", "1000"],
          ["FACEVALUE", "Номинальная стоимость", "375"],
          ["FACEUNIT", "Валюта номинала", "SUR"],
          ["COUPONFREQUENCY", "Периодичность выплаты купона в год", "2"],
          ["HASDEFAULT", "Допущен дефолт", "0"],
          ["HASTECHNICALDEFAULT", "Допущен технический дефолт", "0"]
        ]
      }
    }"#;

    fn parsed() -> IssueTerms {
        parse_description(
            BODY.as_bytes(),
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .expect("описание разобрано")
    }

    #[test]
    fn day_count_and_calendar_are_unknown_because_the_source_has_none() {
        let terms = parsed();
        assert!(matches!(terms.day_count, Knowledge::Unknown));
        assert!(matches!(terms.calendar, Knowledge::Unknown));
    }

    #[test]
    fn the_currency_code_arrives_untranslated() {
        // SUR здесь и RUB в графике — два кода одного источника на одну
        // валюту. Переводит их словарь, а не разборщик.
        let terms = parsed();
        assert_eq!(terms.face_currency_code, Knowledge::Known("SUR".to_owned()));
    }

    #[test]
    fn effective_from_is_unknown_and_not_backfilled_from_observed_at() {
        let terms = parsed();
        assert!(matches!(terms.effective_from, Knowledge::Unknown));
    }

    #[test]
    fn the_maturity_date_is_read() {
        let terms = parsed();
        assert_eq!(terms.maturity_date, Knowledge::Known(date!(2036 - 02 - 06)));
    }
}
