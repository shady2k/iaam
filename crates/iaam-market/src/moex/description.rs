//! Parsing an MOEX ISS issue description.
//!
//! The response arrives as “field name → value” pairs, not a table row.
//!
//! The response contains no day-count basis or calendar—neither here nor in the
//! schedule. This is `Unknown`, not a reason for a default value:
//! a substituted day-count produces plausibly wrong accrued interest that no
//! test on an issue with whole periods will expose.
//!
//! The source supplies current principal, but it does NOT belong here: the balance
//! is derived from initial principal and the return series; two sources of truth
//! would silently diverge.

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

/// Request an issue description.
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
        .ok_or_else(|| MarketError::Malformed("missing block description".to_owned()))?;
    let columns = node
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("missing columns in description".to_owned()))?;
    let name_at = columns
        .iter()
        .position(|column| column.as_str() == Some("name"))
        .ok_or_else(|| MarketError::Malformed("missing name column".to_owned()))?;
    let value_at = columns
        .iter()
        .position(|column| column.as_str() == Some("value"))
        .ok_or_else(|| MarketError::Malformed("missing value column".to_owned()))?;
    let data = node
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("missing data in description".to_owned()))?;
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

/// Parse an issue description into a terms snapshot.
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
                .map_err(|_| MarketError::Malformed(format!("MATDATE is not a date: {text}")))?,
        ),
        None => Knowledge::Unknown,
    };
    let initial_face_value = match fields.get("INITIALFACEVALUE") {
        Some(text) => Knowledge::Known(Dec::new(text.parse::<Decimal>().map_err(|_| {
            MarketError::Malformed(format!("INITIALFACEVALUE is not a number: {text}"))
        })?)),
        None => Knowledge::Unknown,
    };
    let coupon_periods_per_year = match fields.get("COUPONFREQUENCY") {
        Some(text) => Knowledge::Known(text.parse::<u32>().map_err(|_| {
            MarketError::Malformed(format!("COUPONFREQUENCY is not an integer: {text}"))
        })?),
        None => Knowledge::Unknown,
    };

    Ok(IssueTerms {
        instrument,
        observed_at,
        // The source does not report the terms’ effective date.
        // Substituting observed_at would turn a guess into a fact.
        effective_from: Knowledge::Unknown,
        maturity_date,
        initial_face_value,
        face_currency_code: fields
            .get("FACEUNIT")
            .cloned()
            .map_or(Knowledge::Unknown, Knowledge::Known),
        coupon_periods_per_year,
        // The source provides neither, here or in the schedule.
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

    // Live ISS response from 2026-08-27, frozen as a reference. It has no day-count
    // basis or calendar—this is a source property, not a property of our
    // literal, and must be checked against the source.
    const DESCRIPTION: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-description-amortised.json");

    fn parsed() -> IssueTerms {
        parse_description(
            DESCRIPTION,
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .expect("description parsed")
    }

    #[test]
    fn day_count_and_calendar_are_unknown_because_the_source_has_none() {
        let terms = parsed();
        assert!(matches!(terms.day_count, Knowledge::Unknown));
        assert!(matches!(terms.calendar, Knowledge::Unknown));
    }

    #[test]
    fn the_currency_code_arrives_untranslated() {
        // SUR here and RUB in the schedule are two source codes for one
        // currency. The dictionary translates them, not the parser.
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

    #[test]
    fn both_default_flags_are_parsed() {
        // Both flags, not one: technical default and declared default are
        // different states, and losing the second would compute a metric
        // for a defaulted issue as if payments would occur.
        let terms = parsed();
        assert!(!terms.default_flags.declared);
        assert!(!terms.default_flags.technical);
    }
}
