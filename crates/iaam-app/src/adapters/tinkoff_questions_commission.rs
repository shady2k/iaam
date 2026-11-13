//! Selected-field evidence for commission allocation; never changes imported facts.

use super::*;
use iaam_core::numeric::commission_evidence;
use serde_json::Value;

fn fills(raw: &Value) -> &[Value] {
    raw.pointer("/tradesInfo/trades")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn candidate(raw: &Value) -> bool {
    let trades = fills(raw);
    let Some(first_price) = trades
        .first()
        .and_then(|fill| fill.get("price"))
        .and_then(money)
    else {
        return false;
    };
    trades.len() > 1
        && raw.get("commission").and_then(money).is_some()
        && trades
            .iter()
            .skip(1)
            .filter_map(|fill| fill.get("price").and_then(money))
            .any(|price| price != first_price)
}

pub(super) fn affected_dates(operations: &[ChannelOperation]) -> Vec<Date> {
    let mut dates = BTreeSet::new();
    for operation in operations
        .iter()
        .filter(|operation| candidate(&operation.raw))
    {
        dates.extend(operation.date);
        for fill in fills(&operation.raw) {
            if let Some(date) = fill.get("date").and_then(Value::as_str).and_then(|date| {
                OffsetDateTime::parse(date, &time::format_description::well_known::Rfc3339).ok()
            }) {
                dates.insert(date.to_offset(time::UtcOffset::UTC).date());
            }
        }
    }
    dates.into_iter().collect()
}

fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

fn money(value: &Value) -> Option<(Decimal, String)> {
    let units = integer(value.get("units")?)?;
    let nano = integer(value.get("nano")?)?;
    let currency = value.get("currency")?.as_str()?.to_ascii_uppercase();
    if currency.is_empty() {
        return None;
    }
    let amount = commission_evidence::money_decimal(units, nano)?;
    Some((amount, currency))
}

struct FillEvidence<'a> {
    id: &'a str,
    quantity: Decimal,
    price: Decimal,
    commission: Option<Decimal>,
    report_status: &'static str,
}

struct Evidence<'a> {
    fills: Vec<FillEvidence<'a>>,
    order_commission: Decimal,
    total_quantity: Decimal,
    total_value: Decimal,
    quantity_matches: bool,
    value_matches: bool,
    reports_complete: bool,
}

fn evidence<'a>(raw: &'a Value, rows: &[Value]) -> Option<Evidence<'a>> {
    let (order_commission, currency) = money(raw.get("commission")?)?;
    let figi = raw
        .get("figi")
        .and_then(Value::as_str)
        .filter(|figi| !figi.is_empty());
    let mut seen = HashSet::new();
    let mut parsed = Vec::new();
    for fill in fills(raw) {
        let id = fill.get("num")?.as_str().filter(|id| !id.is_empty())?;
        if !seen.insert(id) {
            return None;
        }
        let quantity = Decimal::from(integer(fill.get("quantity")?)?);
        let (price, price_currency) = money(fill.get("price")?)?;
        if quantity <= Decimal::ZERO || price <= Decimal::ZERO || price_currency != currency {
            return None;
        }
        let mut matches = rows.iter().filter(|row| {
            row.get("tradeId").and_then(Value::as_str) == Some(id)
                && figi.is_none_or(|figi| row.get("figi").and_then(Value::as_str) == Some(figi))
        });
        let (commission, report_status) = match (matches.next(), matches.next()) {
            (None, _) => (None, "missing_report"),
            (Some(_), Some(_)) => (None, "ambiguous_report"),
            (Some(row), None) => match row.get("brokerCommission").and_then(money) {
                Some((commission, report_currency)) if report_currency == currency => {
                    (Some(commission), "matched")
                }
                Some(_) => (None, "currency_mismatch"),
                None => (None, "malformed_commission"),
            },
        };
        parsed.push(FillEvidence {
            id,
            quantity,
            price,
            commission,
            report_status,
        });
    }
    if parsed.len() < 2 {
        return None;
    }
    let (total_quantity, total_value) = commission_evidence::weights(
        &parsed
            .iter()
            .map(|fill| (fill.quantity, fill.price))
            .collect::<Vec<_>>(),
    )?;
    let reports_complete = parsed.iter().all(|fill| fill.commission.is_some());
    let mut quantity_matches = reports_complete;
    let mut value_matches = reports_complete;
    for fill in &parsed {
        if let Some(commission) = fill.commission {
            let (by_quantity, by_value) = commission_evidence::exact_matches(
                commission,
                order_commission,
                fill.quantity,
                fill.price,
                total_quantity,
                total_value,
            )?;
            quantity_matches &= by_quantity;
            value_matches &= by_value;
        }
    }
    Some(Evidence {
        fills: parsed,
        order_commission,
        total_quantity,
        total_value,
        quantity_matches,
        value_matches,
        reports_complete,
    })
}

pub(super) fn trace_commission(operations: &[ChannelOperation], report_rows: &[Value]) {
    let mut orders = 0;
    let mut fill_count = 0;
    let mut quantity_weight = 0;
    let mut value_weight = 0;
    let mut both = 0;
    let mut neither = 0;
    let mut unmatched = 0;
    for operation in operations
        .iter()
        .filter(|operation| candidate(&operation.raw))
    {
        orders += 1;
        fill_count += fills(&operation.raw).len();
        let Some(evidence) = evidence(&operation.raw, report_rows) else {
            unmatched += 1;
            for fill in fills(&operation.raw) {
                tracing::debug!(operation_id = operation.operation_id,
                    trade_id = ?fill.get("num"), quantity = ?fill.get("quantity"),
                    price = ?fill.get("price"),
                    status = "missing_ambiguous_or_invalid_evidence",
                    "commission allocation fill evidence unavailable");
            }
            continue;
        };
        if evidence.reports_complete {
            match (evidence.quantity_matches, evidence.value_matches) {
                (true, true) => both += 1,
                (true, false) => quantity_weight += 1,
                (false, true) => value_weight += 1,
                (false, false) => neither += 1,
            }
        } else {
            unmatched += 1;
        }
        for fill in &evidence.fills {
            let (by_quantity, by_value) = commission_evidence::allocations(
                evidence.order_commission,
                fill.quantity,
                fill.price,
                evidence.total_quantity,
                evidence.total_value,
            );
            tracing::debug!(operation_id = operation.operation_id, trade_id = fill.id,
                quantity = %fill.quantity, price = %fill.price,
                broker_commission = ?fill.commission, report_status = fill.report_status,
                allocation_by_quantity_approx = ?by_quantity, allocation_by_value_approx = ?by_value,
                quantity_weight_exact_match = evidence.quantity_matches,
                value_weight_exact_match = evidence.value_matches,
                "commission allocation fill evidence");
        }
    }
    tracing::info!(
        question = "commission_allocation",
        orders,
        fills = fill_count,
        matched = quantity_weight + value_weight + both + neither,
        quantity_weight,
        value_weight,
        both,
        neither,
        unmatched,
        status = if tracing::enabled!(tracing::Level::DEBUG) {
            "diagnostic"
        } else {
            "not_requested"
        },
        "T-Invest commission allocation summary"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn amount(units: i64) -> Value {
        json!({"units": units.to_string(), "nano": 0, "currency": "rub"})
    }

    fn order(commission: i64) -> Value {
        json!({"figi": "invented-figi", "commission": amount(commission), "tradesInfo": {"trades": [
            {"num": "fill-a", "quantity": "1", "price": amount(10)},
            {"num": "fill-b", "quantity": "1", "price": amount(20)}
        ]}})
    }

    fn rows(first: i64, second: i64) -> Vec<Value> {
        vec![
            json!({"tradeId": "fill-a", "figi": "invented-figi", "brokerCommission": amount(first)}),
            json!({"tradeId": "fill-b", "figi": "invented-figi", "brokerCommission": amount(second)}),
        ]
    }

    #[test]
    fn distinguishes_quantity_value_both_and_neither_exactly() {
        for (commission, first, second, expected) in [
            (6, 3, 3, (true, false)),
            (6, 2, 4, (false, true)),
            (0, 0, 0, (true, true)),
            (6, 1, 5, (false, false)),
        ] {
            let raw = order(commission);
            assert!(candidate(&raw));
            let result = evidence(&raw, &rows(first, second)).unwrap();
            assert_eq!((result.quantity_matches, result.value_matches), expected);
        }
    }

    #[test]
    fn partial_missing_ambiguous_and_currency_mismatch_keep_fill_allocations() {
        let raw = order(6);
        let empty = evidence(&raw, &[]).unwrap();
        assert!(!empty.reports_complete);
        assert_eq!(empty.total_quantity, Decimal::from(2));
        assert_eq!(empty.total_value, Decimal::from(30));
        assert_eq!(empty.fills[0].report_status, "missing_report");
        let mut report = rows(3, 3);
        report.pop();
        let partial = evidence(&raw, &report).unwrap();
        assert!(!partial.reports_complete);
        assert!(!partial.quantity_matches && !partial.value_matches);
        assert_eq!(partial.fills[0].commission, Some(Decimal::from(3)));
        assert_eq!(partial.fills[1].report_status, "missing_report");
        let mut report = rows(3, 3);
        report.push(report[0].clone());
        let ambiguous = evidence(&raw, &report).unwrap();
        assert!(!ambiguous.reports_complete);
        assert_eq!(ambiguous.fills[0].report_status, "ambiguous_report");
        assert_eq!(ambiguous.fills[1].commission, Some(Decimal::from(3)));
        let mut report = rows(3, 3);
        report[0]["brokerCommission"]["currency"] = json!("usd");
        let mismatch = evidence(&raw, &report).unwrap();
        assert!(!mismatch.reports_complete);
        assert_eq!(mismatch.fills[0].report_status, "currency_mismatch");
        let mut report = rows(3, 3);
        report[0]["brokerCommission"]["nano"] = json!(1_000_000_000);
        assert_eq!(
            evidence(&raw, &report).unwrap().fills[0].report_status,
            "malformed_commission"
        );
        let mut malformed = raw.clone();
        malformed["tradesInfo"]["trades"][0]["price"]["nano"] = json!(1_000_000_000);
        assert!(evidence(&malformed, &rows(3, 3)).is_none());
    }

    #[test]
    fn candidate_prices_are_numeric_and_commission_must_be_valid() {
        let mut raw = order(6);
        raw["tradesInfo"]["trades"][1]["price"] =
            json!({"units": 10, "nano": "0", "currency": "RUB"});
        assert!(!candidate(&raw));
        raw["tradesInfo"]["trades"][1]["price"] = amount(20);
        raw["commission"]["units"] = json!("not-a-number");
        assert!(!candidate(&raw));
    }
}
