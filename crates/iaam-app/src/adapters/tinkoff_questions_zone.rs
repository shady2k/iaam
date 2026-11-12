//! Selected, ephemeral evidence for fills near the UTC/Moscow date boundary.

use super::*;
use serde_json::Value;
use time::{UtcOffset, format_description::well_known::Rfc3339};

struct ZoneFill<'a> {
    id: Option<&'a str>,
    at: OffsetDateTime,
}

fn text<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get(field)?.as_str().filter(|value| !value.is_empty())
}

fn zone_fills(raw: &Value) -> impl Iterator<Item = ZoneFill<'_>> {
    raw.get("tradesInfo")
        .and_then(|info| info.get("trades"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|fill| {
            let at = OffsetDateTime::parse(text(fill, "date")?, &Rfc3339)
                .ok()?
                .to_offset(UtcOffset::UTC);
            (at.hour() >= 21).then_some(ZoneFill {
                id: text(fill, "num"),
                at,
            })
        })
}

pub(super) fn affected_dates(operations: &[ChannelOperation]) -> Vec<Date> {
    operations
        .iter()
        .flat_map(|operation| zone_fills(&operation.raw))
        .map(|fill| fill.at.date())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn matching_report<'a>(
    fill_id: Option<&str>,
    figi: Option<&str>,
    rows: &'a [Value],
) -> Result<&'a Value, &'static str> {
    let fill_id = fill_id.ok_or("missing_fill_id")?;
    if rows.is_empty() {
        return Err("not_requested");
    }
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| text(row, "tradeId") == Some(fill_id))
        .collect();
    let row = match matches.as_slice() {
        [] => return Err("missing_report_trade"),
        [row] => *row,
        _ => return Err("ambiguous_report_trade"),
    };
    if let Some(figi) = figi {
        match text(row, "figi") {
            Some(report_figi) if report_figi == figi => {}
            Some(_) => return Err("report_figi_mismatch"),
            None => return Err("missing_report_figi"),
        }
    }
    Ok(row)
}

pub(super) fn trace_trading_zone(operations: &[ChannelOperation], report_rows: &[Value]) {
    let mut fills = 0;
    let mut orders = 0;
    let mut matched = 0;
    for operation in operations {
        let mut order_has_fill = false;
        for fill in zone_fills(&operation.raw) {
            fills += 1;
            order_has_fill = true;
            let report = matching_report(
                fill.id,
                operation.figi.as_deref().filter(|figi| !figi.is_empty()),
                report_rows,
            );
            matched += usize::from(report.is_ok());
            if tracing::enabled!(tracing::Level::DEBUG) {
                let moscow_date = time::UtcOffset::from_hms(3, 0, 0)
                    .ok()
                    .map(|offset| fill.at.to_offset(offset).date());
                let row = report.as_ref().ok().copied();
                tracing::debug!(
                    phase = "questions_trading_zone",
                    operation_id = %operation.operation_id,
                    fill_id = ?fill.id,
                    fill_utc_timestamp = %fill.at.format(&Rfc3339).unwrap_or_default(),
                    utc_date = %fill.at.date(),
                    moscow_date = ?moscow_date,
                    status = report.as_ref().map_or_else(|reason| *reason, |_| "matched"),
                    report_trade_datetime = ?row.and_then(|row| text(row, "tradeDatetime")),
                    report_clear_value_date = ?row.and_then(|row| text(row, "clearValueDate")),
                    report_sec_value_date = ?row.and_then(|row| text(row, "secValueDate")),
                    "T-Invest late UTC fill and broker report date evidence"
                );
            }
        }
        orders += usize::from(order_has_fill);
    }
    tracing::info!(
        phase = "questions_trading_zone",
        orders,
        fills,
        matched,
        status = if report_rows.is_empty() {
            "not_requested"
        } else {
            "compared"
        },
        "T-Invest fills between 21:00 and 24:00 UTC"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::date;

    #[test]
    fn utc_boundary_selects_every_fill_and_crosses_to_next_moscow_date() {
        let raw = json!({"tradesInfo": {"trades": [
            {"num": "before", "date": "2026-01-01T20:59:59Z"},
            {"num": "start", "date": "2026-01-01T21:00:00Z"},
            {"num": "end", "date": "2026-01-01T23:59:59.999999999Z"},
            {"num": "after", "date": "2026-01-02T00:00:00Z"}
        ]}});
        let fills: Vec<_> = zone_fills(&raw).collect();
        assert_eq!(
            fills.iter().map(|fill| fill.id).collect::<Vec<_>>(),
            vec![Some("start"), Some("end")]
        );
        for fill in fills {
            assert_eq!(fill.at.date(), date!(2026 - 01 - 01));
            assert_eq!(
                (fill.at + time::Duration::hours(3)).date(),
                date!(2026 - 01 - 02)
            );
        }
    }

    #[test]
    fn selection_uses_utc_instead_of_the_written_offset() {
        let raw = json!({"tradesInfo": {"trades": [
            {"num": "inside", "date": "2026-01-02T00:00:00+03:00"},
            {"num": "outside", "date": "2026-01-01T21:30:00+03:00"}
        ]}});
        let fills: Vec<_> = zone_fills(&raw).collect();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].id, Some("inside"));
        assert_eq!(fills[0].at.hour(), 21);
    }

    #[test]
    fn report_match_requires_unique_native_id_and_verifies_provided_figi() {
        let row = json!({"tradeId": "42", "figi": "invented-figi",
            "tradeDatetime": "2026-01-01T21:00:00Z",
            "clearValueDate": "2026-01-03T00:00:00Z",
            "secValueDate": "2026-01-03T00:00:00Z"});
        let rows = vec![row.clone()];
        assert_eq!(
            matching_report(Some("42"), Some("invented-figi"), &rows),
            Ok(&row)
        );
        assert_eq!(matching_report(Some("42"), None, &rows), Ok(&row));
        assert_eq!(
            matching_report(Some("other"), None, &rows),
            Err("missing_report_trade")
        );
        assert_eq!(matching_report(None, None, &rows), Err("missing_fill_id"));
        assert_eq!(
            matching_report(Some("42"), Some("different"), &rows),
            Err("report_figi_mismatch")
        );
        assert_eq!(
            matching_report(Some("42"), None, &[row.clone(), row]),
            Err("ambiguous_report_trade")
        );
        assert_eq!(matching_report(Some("42"), None, &[]), Err("not_requested"));
    }

    #[test]
    fn numeric_ids_and_missing_figi_do_not_become_invented_matches() {
        assert_eq!(
            matching_report(Some("42"), None, &[json!({"tradeId": 42})]),
            Err("missing_report_trade")
        );
        assert_eq!(
            matching_report(
                Some("42"),
                Some("invented-figi"),
                &[json!({"tradeId": "42"})]
            ),
            Err("missing_report_figi")
        );
    }
}
