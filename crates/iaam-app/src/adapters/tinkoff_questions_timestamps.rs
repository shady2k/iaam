//! Diagnostic timestamp comparisons. Source dates never change mapping or filtering.

use super::*;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;

const NARROW_INTERVAL_DAYS: i64 = 31;
const MOSCOW_OFFSET_HOURS: i8 = 3;

struct TimestampEvidence {
    cross_day: bool,
    order: Value,
    fills: Vec<Value>,
}

fn timestamp(raw: &Value) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw.as_str()?, &Rfc3339)
        .ok()
        .map(|at| at.to_offset(time::UtcOffset::UTC))
}

fn moscow_date(at: OffsetDateTime) -> Option<Date> {
    Some(
        at.to_offset(time::UtcOffset::from_hms(MOSCOW_OFFSET_HOURS, 0, 0).ok()?)
            .date(),
    )
}

fn relation(at: Option<OffsetDateTime>, from: Date, to: Date) -> &'static str {
    match at {
        None => "unknown",
        Some(at) if at.date() < from || at.date() > to => "outside",
        Some(_) => "inside",
    }
}

fn date_fields(raw: &Value, from: Date, to: Date) -> Value {
    let at = timestamp(raw);
    json!({
        "timestamp": raw.as_str().unwrap_or("unknown"),
        "utc_date": at.map(|at| at.date().to_string()).unwrap_or_else(|| "unknown".to_owned()),
        "moscow_date": at.and_then(moscow_date).map(|date| date.to_string()).unwrap_or_else(|| "unknown".to_owned()),
        "interval_relation": relation(at, from, to),
    })
}

fn evidence(raw: &Value, from: Date, to: Date) -> TimestampEvidence {
    let order_at = timestamp(&raw["date"]);
    let mut cross_day = false;
    let fills = raw["tradesInfo"]["trades"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|fill| {
            if let (Some(order), Some(fill_at)) = (order_at, timestamp(&fill["date"])) {
                cross_day |= order.date() != fill_at.date()
                    && matches!((moscow_date(order), moscow_date(fill_at)), (Some(order), Some(fill)) if order != fill);
            }
            let mut fields = date_fields(&fill["date"], from, to);
            fields["quantity"] = fill.get("quantity").cloned().unwrap_or_else(|| json!("unknown"));
            fields
        })
        .collect();
    TimestampEvidence {
        cross_day,
        order: date_fields(&raw["date"], from, to),
        fills,
    }
}

pub(super) fn trace_timestamps(operations: &[ChannelOperation], from: Date, to: Date) {
    let narrow = from <= to && (to - from).whole_days() < NARROW_INTERVAL_DAYS;
    let mut cross_day_orders = 0usize;
    for operation in operations {
        let evidence = evidence(&operation.raw, from, to);
        cross_day_orders += usize::from(evidence.cross_day);
        if evidence.cross_day {
            tracing::debug!(
                phase = "timestamp_questions",
                operation_id = operation.operation_id,
                order_date = %evidence.order,
                fill_dates_and_quantities = ?evidence.fills,
                "order and fill dates differ in both UTC and Moscow"
            );
        }
        if narrow {
            tracing::debug!(
                phase = "timestamp_questions",
                operation_id = operation.operation_id,
                request_from = %rfc3339_midnight(from),
                request_to = %rfc3339_operation_end(to),
                order = %evidence.order,
                fills = ?evidence.fills,
                "received operation timestamps relative to request interval"
            );
        }
    }
    tracing::info!(
        phase = "timestamp_questions",
        cross_day_orders,
        "T-Invest order and fill date evidence summary"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn row(order: &str, fills: &[&str]) -> Value {
        json!({"date": order, "tradesInfo": {"trades": fills.iter().map(|date| json!({"date": date, "quantity": "2"})).collect::<Vec<_>>()}})
    }

    #[test]
    fn cross_day_requires_the_same_fill_to_differ_in_both_zones() {
        let from = date!(2026 - 11 - 01);
        let to = date!(2026 - 11 - 03);
        let utc_only = row("2026-11-01T23:30:00Z", &["2026-11-02T00:30:00Z"]);
        let moscow_only = row("2026-11-01T20:30:00Z", &["2026-11-01T21:30:00Z"]);
        let both = row("2026-11-01T20:30:00Z", &["2026-11-02T21:30:00Z"]);
        let different_fills = row(
            "2026-11-01T21:30:00Z",
            &["2026-11-02T00:30:00Z", "2026-11-01T20:30:00Z"],
        );
        let rows = [utc_only, moscow_only, both, different_fills];
        let count = rows
            .iter()
            .filter(|raw| evidence(raw, from, to).cross_day)
            .count();
        assert_eq!(count, 1);
        let fields = evidence(&rows[2], from, to);
        assert_eq!(fields.order["moscow_date"], "2026-11-01");
        assert_eq!(fields.fills[0]["moscow_date"], "2026-11-03");
        assert_eq!(fields.fills[0]["quantity"], "2");
    }

    #[test]
    fn interval_uses_inclusive_utc_request_boundaries() {
        let from = date!(2026 - 11 - 01);
        let to = date!(2026 - 11 - 01);
        for (value, expected) in [
            ("2026-11-01T00:00:00Z", "inside"),
            ("2026-11-01T23:59:59.999999999Z", "inside"),
            ("2026-10-31T23:59:59.999999999Z", "outside"),
            ("2026-11-02T00:00:00Z", "outside"),
            ("2026-11-02T01:00:00+03:00", "inside"),
            ("malformed", "unknown"),
        ] {
            assert_eq!(relation(timestamp(&json!(value)), from, to), expected);
        }
        let missing = evidence(&json!({}), from, to);
        assert_eq!(missing.order["interval_relation"], "unknown");
        assert!(missing.fills.is_empty());
        assert!(!missing.cross_day);
    }
}
