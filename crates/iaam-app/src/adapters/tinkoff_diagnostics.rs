//! Ephemeral, selected-field evidence for position refusals. Never changes facts.

use super::*;
use serde_json::{Value, json};

pub(super) struct ReceivedOperation {
    pub(super) instrument: Option<InstrumentId>,
    fields: Value,
}

pub(super) fn capture(operations: &[ChannelOperation]) -> Vec<ReceivedOperation> {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return Vec::new();
    }
    operations
        .iter()
        .map(|operation| ReceivedOperation {
            instrument: None,
            fields: selected_operation(&operation.raw),
        })
        .collect()
}

fn payment_sign(raw: &Value) -> &'static str {
    let number = |key| {
        raw.get("payment")
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse::<i64>().ok()))
    };
    match (number("units"), number("nano")) {
        (Some(units), nano) if units < 0 || (units == 0 && nano.is_some_and(|n| n < 0)) => {
            "negative"
        }
        (Some(units), nano) if units > 0 || (units == 0 && nano.is_some_and(|n| n > 0)) => {
            "positive"
        }
        (Some(0), Some(0)) => "zero",
        _ => "unknown",
    }
}

fn selected_operation(raw: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    for key in [
        "id",
        "operationId",
        "parentOperationId",
        "name",
        "type",
        "operationType",
        "state",
        "date",
        "quantity",
        "quantityRest",
        "quantityDone",
        "description",
        "instrumentUid",
        "positionUid",
        "figi",
        "ticker",
    ] {
        if let Some(value) = raw.get(key) {
            fields.insert(key.to_owned(), value.clone());
        }
    }
    fields.insert("paymentSign".to_owned(), json!(payment_sign(raw)));
    if let Some(children) = raw.get("childOperations").and_then(Value::as_array) {
        fields.insert(
            "childOperations".to_owned(),
            Value::Array(children.iter().map(selected_operation).collect()),
        );
    }
    Value::Object(fields)
}

fn identities(row: &Value) -> Vec<(String, String)> {
    let mut handles: Vec<_> = ["instrumentUid", "positionUid", "figi"]
        .into_iter()
        .filter_map(|key| {
            let value = row.get(key)?.as_str().filter(|value| !value.is_empty())?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect();
    if let Some(children) = row.get("childOperations").and_then(Value::as_array) {
        handles.extend(children.iter().flat_map(identities));
    }
    handles
}

fn negative_id(row: &Value) -> bool {
    row.get("id")
        .or_else(|| row.get("operationId"))
        .is_some_and(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.parse::<i64>().ok())
                .is_some_and(|id| id < 0)
        })
}

fn related_rows(rows: &[ReceivedOperation], instruments: &[InstrumentId]) -> Vec<usize> {
    let mut selected: HashSet<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            (row.instrument.is_some_and(|id| instruments.contains(&id)) || negative_id(&row.fields))
                .then_some(index)
        })
        .collect();
    // Each source handle can bridge rows that omit a different handle.
    loop {
        let handles: HashSet<_> = selected
            .iter()
            .flat_map(|index| identities(&rows[*index].fields))
            .collect();
        let before = selected.len();
        selected.extend(rows.iter().enumerate().filter_map(|(index, row)| {
            identities(&row.fields)
                .into_iter()
                .any(|id| handles.contains(&id))
                .then_some(index)
        }));
        if before == selected.len() {
            break;
        }
    }
    let mut selected: Vec<_> = selected.into_iter().collect();
    selected.sort_unstable();
    selected
}

fn report_matches(row: &Value, figis: &HashSet<String>, tickers: &HashSet<String>) -> bool {
    row.get("figi")
        .and_then(Value::as_str)
        .is_some_and(|figi| figis.contains(figi))
        || row
            .get("ticker")
            .and_then(Value::as_str)
            .is_some_and(|ticker| tickers.contains(ticker))
}

impl TinkoffChannel {
    pub(super) async fn trace_instrument_history(
        &self,
        instruments: &[InstrumentId],
        account: &str,
        (from, to): (Date, Date),
        context: BrokerRequestContext<'_>,
    ) {
        if instruments.is_empty() || !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        let Some(deadline) = context.deadline else {
            self.run_instrument_history(instruments, account, (from, to), context)
                .await;
            return;
        };
        if deadline.saturating_duration_since(tokio::time::Instant::now().into_std())
            <= crate::ports::BROKER_DIAGNOSTIC_RESERVE
        {
            tracing::debug!(
                phase = "position_refusal_history",
                status = "skipped",
                reason = "sync_deadline_near_or_elapsed",
                "instrument diagnostic skipped at sync deadline"
            );
            return;
        }
        if tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.run_instrument_history(instruments, account, (from, to), context),
        )
        .await
        .is_err()
        {
            tracing::debug!(
                phase = "position_refusal_history",
                status = "deadline_elapsed",
                "instrument diagnostic stopped at sync deadline"
            );
        }
    }

    async fn run_instrument_history(
        &self,
        instruments: &[InstrumentId],
        account: &str,
        (from, to): (Date, Date),
        context: BrokerRequestContext<'_>,
    ) {
        let rows = self.diagnostic_operations.lock().await;
        let selected = related_rows(&rows, instruments);
        let mut figis = HashSet::new();
        let mut tickers = HashSet::new();
        let mut selected_instruments: HashSet<_> = instruments.iter().copied().collect();
        for index in selected {
            let row = &rows[index];
            selected_instruments.extend(row.instrument);
            if let Some(figi) = row
                .fields
                .get("figi")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                figis.insert(figi.to_owned());
            }
            if let Some(ticker) = row
                .fields
                .get("ticker")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                tickers.insert(ticker.to_owned());
            }
            tracing::debug!(phase = "position_refusal_history", operation = %row.fields, "received broker operation for affected instrument");
        }
        drop(rows);
        let aliases = match self.instruments.list_aliases().await {
            Ok(aliases) => aliases,
            Err(_) => {
                tracing::warn!(
                    phase = "position_refusal_history",
                    failure = "catalogue_aliases",
                    "instrument diagnostic enrichment unavailable"
                );
                Vec::new()
            }
        };
        let mut catalogue_isins = HashMap::new();
        for instrument in selected_instruments {
            match self.instruments.instrument(instrument).await {
                Ok(Some(view)) => {
                    if !view.symbol.is_empty() {
                        tickers.insert(view.symbol.clone());
                        let isins: Vec<_> = aliases
                            .iter()
                            .filter(|alias| {
                                alias.instrument == instrument
                                    && alias.namespace == "isin"
                                    && alias.valid_from <= to
                                    && alias.valid_to.is_none_or(|end| end > from)
                            })
                            .map(|alias| alias.value.clone())
                            .collect();
                        catalogue_isins.insert(view.symbol, isins);
                    }
                }
                Ok(None) => {}
                Err(_) => tracing::warn!(
                    phase = "position_refusal_history",
                    failure = "catalogue_instrument",
                    "instrument diagnostic enrichment unavailable"
                ),
            }
        }
        match self
            .read_settlement_report(account, from, to, context)
            .await
        {
            Ok(report) => {
                let matching: Vec<_> = report
                    .iter()
                    .filter(|row| report_matches(row, &figis, &tickers))
                    .collect();
                tracing::debug!(
                    phase = "position_refusal_report",
                    received_rows = report.len(),
                    matching_rows = matching.len(),
                    "broker report diagnostic selection"
                );
                for row in matching {
                    let ticker = row.get("ticker").and_then(Value::as_str);
                    // The published BrokerReport has direction, not operation name,
                    // and no ISIN. Keep catalogue enrichment explicitly separate.
                    tracing::debug!(phase = "position_refusal_report", figi = ?row.get("figi"), security_code = ticker, catalogue_isins = ?ticker.and_then(|ticker| catalogue_isins.get(ticker)), isin_source = "catalogue", asset_name = ?row.get("name"), direction = ?row.get("direction"), quantity = ?row.get("quantity"), trade_datetime = ?row.get("tradeDatetime"), "broker report row for affected instrument");
                }
            }
            Err(_) => tracing::warn!(
                phase = "position_refusal_report",
                failure = "report_unavailable",
                "broker report diagnostic unavailable; refusal unchanged"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affected_and_negative_id_instruments_include_all_source_handle_matches() {
        let affected = InstrumentId::new_random();
        let rows = vec![
            ReceivedOperation {
                instrument: Some(affected),
                fields: json!({"id":"1", "instrumentUid":"a", "positionUid":"p"}),
            },
            ReceivedOperation {
                instrument: None,
                fields: json!({"id":"2", "positionUid":"p", "figi":"FIGI_A"}),
            },
            ReceivedOperation {
                instrument: None,
                fields: json!({"id":"3", "figi":"FIGI_A"}),
            },
            ReceivedOperation {
                instrument: None,
                fields: json!({"id":"-901", "instrumentUid":"b"}),
            },
            ReceivedOperation {
                instrument: None,
                fields: json!({"id":"5", "instrumentUid":"b"}),
            },
            ReceivedOperation {
                instrument: None,
                fields: json!({"id":"6", "instrumentUid":"unrelated"}),
            },
        ];
        assert_eq!(related_rows(&rows, &[affected]), vec![0, 1, 2, 3, 4]);
        assert!(report_matches(
            &json!({"figi":"FIGI_A"}),
            &HashSet::from(["FIGI_A".to_owned()]),
            &HashSet::new()
        ));
        assert!(!report_matches(
            &json!({"figi":"OTHER"}),
            &HashSet::from(["FIGI_A".to_owned()]),
            &HashSet::new()
        ));
    }

    #[test]
    fn selected_fields_exclude_amounts_and_unrelated_payloads_including_children() {
        let selected = selected_operation(
            &json!({"id":"-2", "description":"invented exchange", "quantity":"3", "payment":{"units":"-12", "nano":0}, "brokerAccountId":"secret", "price":{"units":"4"}, "childOperations":[{"instrumentUid":"child", "payment":{"units":"0", "nano":1}, "secret":"hidden"}]}),
        );
        assert_eq!(selected["paymentSign"], "negative");
        assert_eq!(selected["childOperations"][0]["paymentSign"], "positive");
        assert!(selected.get("payment").is_none());
        assert!(selected.get("price").is_none());
        assert!(selected.get("brokerAccountId").is_none());
        assert!(selected["childOperations"][0].get("secret").is_none());
    }
}
