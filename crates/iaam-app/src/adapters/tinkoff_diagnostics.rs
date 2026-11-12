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
        "isin",
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
        if !self.client.diagnostic_report_allowed().await {
            tracing::debug!(
                phase = "position_refusal_history",
                status = "skipped",
                reason = "low_rate_capacity_or_host_pause",
                "instrument diagnostic skipped to preserve broker capacity"
            );
            return;
        }
        let rows = self.diagnostic_operations.lock().await;
        let selected = related_rows(&rows, instruments);
        let mut figis = HashSet::new();
        let mut tickers = HashSet::new();
        let mut operation_dates = Vec::new();
        let mut selected_instruments: HashSet<_> = instruments.iter().copied().collect();
        for index in selected {
            let row = &rows[index];
            if let Some(date) = row
                .fields
                .get("date")
                .and_then(Value::as_str)
                .and_then(|date| {
                    time::OffsetDateTime::parse(
                        date,
                        &time::format_description::well_known::Rfc3339,
                    )
                    .ok()
                    .map(|date| date.date())
                })
            {
                operation_dates.push(date);
            }
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
        let windows = report::report_windows(&operation_dates, from, to);
        if windows.is_empty() {
            tracing::debug!(
                phase = "position_refusal_report",
                status = "skipped",
                reason = "no_affected_operation_date",
                "broker report diagnostic skipped"
            );
        }
        for (from, to) in windows {
            match self
                .read_diagnostic_report(account, from, to, context)
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
                Err(_) => {
                    tracing::debug!(
                        phase = "position_refusal_report",
                        failure = "report_unavailable",
                        account,
                        from = %from,
                        to = %to,
                        "broker report diagnostic request unavailable"
                    );
                    tracing::warn!(
                        phase = "position_refusal_report",
                        failure = "report_unavailable",
                        "broker report diagnostic unavailable; refusal unchanged"
                    );
                    break;
                }
            }
        }
    }
}

// This is a requested historical cross-check, not an assertion about every fund's formation.
const FUND_FORMATION_DIAGNOSTIC_FROM: Date = time::macros::date!(2023 - 10 - 01);
const FUND_FORMATION_DIAGNOSTIC_TO: Date = time::macros::date!(2023 - 12 - 31);

fn formation_windows() -> Vec<(Date, Date)> {
    let mut windows = Vec::new();
    let mut start = FUND_FORMATION_DIAGNOSTIC_FROM;
    while start <= FUND_FORMATION_DIAGNOSTIC_TO {
        let end = start
            .saturating_add(time::Duration::days(30))
            .min(FUND_FORMATION_DIAGNOSTIC_TO);
        windows.push((start, end));
        start = end.saturating_add(time::Duration::days(1));
    }
    windows
}

fn input_like(row: &Value) -> bool {
    ["operationType", "type"].iter().any(|key| {
        row.get(key).is_some_and(|value| {
            if let Some(number) = value.as_i64() {
                return [1, 17, 51, 54, 60].contains(&number);
            }
            let Some(value) = value.as_str() else {
                return false;
            };
            let value = value.strip_prefix("OPERATION_TYPE_").unwrap_or(value);
            value == "INPUT"
                || value.starts_with("INPUT_")
                || value.starts_with("INP_")
                || value
                    .parse::<i64>()
                    .is_ok_and(|number| [1, 17, 51, 54, 60].contains(&number))
        })
    })
}

fn fund_name(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['"', '«', '»']).to_lowercase();
    // A generic asset class is not a fund name.
    (value.chars().any(char::is_alphabetic)
        && ![
            "фонд",
            "паи",
            "вывод",
            "зачисление",
            "паевой инвестиционный фонд",
            "закрытый паевой инвестиционный фонд",
            "инвестиционный фонд",
            "mutual fund",
        ]
        .contains(&value.as_str()))
    .then_some(value)
}

#[derive(Default)]
struct FundSearch {
    isins: HashSet<String>,
    names: HashSet<String>,
    instrument_isins: HashMap<InstrumentId, HashSet<String>>,
    figi_instruments: HashMap<String, InstrumentId>,
    handle_instruments: HashMap<(String, String), InstrumentId>,
    twin_figis: HashSet<String>,
}

impl FundSearch {
    fn reasons(&self, row: &Value, instrument: Option<InstrumentId>) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if input_like(row) {
            reasons.push("input_like");
        }
        let resolved = instrument
            .or_else(|| {
                row.get("figi")
                    .and_then(Value::as_str)
                    .and_then(|figi| self.figi_instruments.get(figi).copied())
            })
            .or_else(|| {
                identities(row)
                    .iter()
                    .find_map(|handle| self.handle_instruments.get(handle).copied())
            });
        if row
            .get("isin")
            .and_then(Value::as_str)
            .is_some_and(|isin| self.isins.contains(isin))
            || resolved
                .and_then(|id| self.instrument_isins.get(&id))
                .is_some_and(|isins| !isins.is_disjoint(&self.isins))
        {
            reasons.push("same_isin");
        }
        if ["name", "description", "type"]
            .iter()
            .filter_map(|key| row.get(key).and_then(Value::as_str))
            .any(|value| {
                let value = value.to_lowercase();
                self.names.iter().any(|name| value.contains(name))
            })
        {
            reasons.push("fund_name");
        }
        if row
            .get("figi")
            .and_then(Value::as_str)
            .is_some_and(|figi| self.twin_figis.contains(figi))
        {
            reasons.push("twin_figi");
        }
        reasons
    }

    fn trace(&mut self, row: &Value, instrument: Option<InstrumentId>, source: &'static str) {
        let reasons = self.reasons(row, instrument);
        if !reasons.is_empty() {
            if reasons.iter().any(|reason| *reason != "input_like")
                && let Some(figi) = row
                    .get("figi")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            {
                self.twin_figis.insert(figi.to_owned());
            }
            tracing::debug!(phase = "missing_fund_units", source, match_reasons = ?reasons, operation = %selected_operation(row), "received operation in missing fund units search");
        }
        if let Some(children) = row.get("childOperations").and_then(Value::as_array) {
            for child in children {
                self.trace(child, None, source);
            }
        }
    }
}

impl TinkoffChannel {
    pub(super) async fn trace_missing_fund_units(
        &self,
        instruments: &[InstrumentId],
        account: &str,
        _period: (Date, Date),
        context: BrokerRequestContext<'_>,
    ) {
        if instruments.is_empty() || !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        if let Some(deadline) = context.deadline {
            if deadline.saturating_duration_since(tokio::time::Instant::now().into_std())
                <= crate::ports::BROKER_DIAGNOSTIC_RESERVE
            {
                tracing::debug!(
                    phase = "missing_fund_units",
                    reason = "sync_deadline_near_or_elapsed",
                    "fund diagnostic skipped"
                );
                return;
            }
            if tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.run_missing_fund_units(instruments, account, context),
            )
            .await
            .is_err()
            {
                tracing::debug!(
                    phase = "missing_fund_units",
                    reason = "deadline_elapsed",
                    "fund diagnostic stopped"
                );
            }
        } else {
            self.run_missing_fund_units(instruments, account, context)
                .await;
        }
    }

    async fn run_missing_fund_units(
        &self,
        instruments: &[InstrumentId],
        account: &str,
        context: BrokerRequestContext<'_>,
    ) {
        let mut search = FundSearch::default();
        match self.instruments.list_aliases().await {
            Ok(aliases) => {
                for alias in aliases {
                    if alias.namespace == "isin" && !alias.value.is_empty() {
                        search
                            .instrument_isins
                            .entry(alias.instrument)
                            .or_default()
                            .insert(alias.value.clone());
                        if instruments.contains(&alias.instrument) {
                            search.isins.insert(alias.value);
                        }
                    } else if alias.namespace == "figi" && !alias.value.is_empty() {
                        search
                            .figi_instruments
                            .insert(alias.value, alias.instrument);
                    }
                }
            }
            Err(_) => tracing::debug!(
                phase = "missing_fund_units",
                failure = "catalogue_aliases",
                "fund diagnostic enrichment unavailable"
            ),
        }
        for instrument in instruments {
            if let Ok(Some(view)) = self.instruments.instrument(*instrument).await
                && let Some(name) = fund_name(&view.title)
            {
                search.names.insert(name);
            }
        }
        let rows = self.diagnostic_operations.lock().await;
        for row in rows.iter() {
            if let Some(instrument) = row.instrument {
                for handle in identities(&row.fields) {
                    search.handle_instruments.insert(handle, instrument);
                }
            }
        }
        for row in rows
            .iter()
            .filter(|row| row.instrument.is_some_and(|id| instruments.contains(&id)))
        {
            if let Some(isin) = row
                .fields
                .get("isin")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                search.isins.insert(isin.to_owned());
            }
            if let Some(name) = row
                .fields
                .get("name")
                .and_then(Value::as_str)
                .and_then(fund_name)
            {
                search.names.insert(name);
            }
            if let Some(description) = row.fields.get("description").and_then(Value::as_str) {
                let description = description.to_lowercase();
                if let Some((_, tail)) = description.split_once("лотов фонда ")
                    && let Some((title, _)) = tail.split_once(" в другой депозитарий")
                    && let Some(name) = fund_name(title)
                {
                    search.names.insert(name);
                }
            }
            if let Some(figi) = row
                .fields
                .get("figi")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                search.twin_figis.insert(figi.to_owned());
            }
        }
        let catalogue_twins: Vec<_> = search
            .figi_instruments
            .iter()
            .filter_map(|(figi, instrument)| {
                search
                    .instrument_isins
                    .get(instrument)
                    .is_some_and(|isins| !isins.is_disjoint(&search.isins))
                    .then_some(figi.clone())
            })
            .collect();
        search.twin_figis.extend(catalogue_twins);
        for row in rows.iter() {
            search.trace(&row.fields, row.instrument, "cursor_received");
        }
        drop(rows);
        for (from, to) in formation_windows() {
            if !self.client.diagnostic_report_allowed().await {
                tracing::debug!(phase = "missing_fund_units", source = "legacy_formation", status = "skipped", from = %from, to = %to, reason = "low_rate_capacity_or_host_pause", "fund cross-check skipped to preserve broker capacity");
                return;
            }
            let from_timestamp = format!("{from}T00:00:00Z");
            let to_timestamp = format!("{to}T23:59:59.999999999Z");
            tracing::debug!(phase = "missing_fund_units", source = "legacy_formation", status = "requesting", from = %from, to = %to, "fund cross-check requested");
            match self
                .client
                .get_legacy_operations(
                    account,
                    (&from_timestamp, &to_timestamp),
                    context.deadline,
                    context.allowance,
                )
                .await
            {
                Ok(body) => match serde_json::from_str::<Value>(&body) {
                    Ok(body) => {
                        if let Some(rows) = body.get("operations").and_then(Value::as_array) {
                            tracing::debug!(phase = "missing_fund_units", source = "legacy_formation", status = "received", from = %from, to = %to, received_rows = rows.len(), "fund cross-check received");
                            for row in rows {
                                search.trace(row, None, "legacy_formation");
                            }
                        } else {
                            tracing::debug!(phase = "missing_fund_units", source = "legacy_formation", from = %from, to = %to, failure = "legacy_response_unparsable", "fund cross-check unavailable");
                        }
                    }
                    Err(_) => {
                        tracing::debug!(phase = "missing_fund_units", source = "legacy_formation", from = %from, to = %to, failure = "legacy_response_unparsable", "fund cross-check unavailable")
                    }
                },
                Err(_) => {
                    tracing::debug!(phase = "missing_fund_units", source = "legacy_formation", from = %from, to = %to, failure = "legacy_unavailable", "fund cross-check unavailable")
                }
            }
        }
        for (from, to) in formation_windows() {
            if !self.client.diagnostic_report_allowed().await {
                tracing::debug!(phase = "missing_fund_units", source = "report_formation", status = "skipped", from = %from, to = %to, reason = "low_rate_capacity_or_host_pause", "fund cross-check skipped to preserve broker capacity");
                return;
            }
            match self
                .read_diagnostic_report(account, from, to, context)
                .await
            {
                Ok(rows) => {
                    for row in rows.iter().filter(|row| {
                        row.get("figi")
                            .and_then(Value::as_str)
                            .is_some_and(|figi| search.twin_figis.contains(figi))
                    }) {
                        tracing::debug!(phase = "missing_fund_units", source = "report_formation", from = %from, to = %to, reason = "twin_figi", figi = ?row.get("figi"), asset_name = ?row.get("name"), direction = ?row.get("direction"), quantity = ?row.get("quantity"), trade_datetime = ?row.get("tradeDatetime"), "broker report row in missing fund units cross-check");
                    }
                }
                Err(_) => {
                    tracing::debug!(
                        phase = "missing_fund_units",
                        failure = "report_unavailable",
                        "fund cross-check unavailable"
                    );
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broad_search_includes_inputs_same_isin_and_unicode_fund_names_only() {
        let affected = InstrumentId::new_random();
        let twin = InstrumentId::new_random();
        let unrelated = InstrumentId::new_random();
        let mut search = FundSearch::default();
        search.isins.insert("INVENTED_ISIN".to_owned());
        search
            .instrument_isins
            .insert(affected, search.isins.clone());
        search.instrument_isins.insert(twin, search.isins.clone());
        search
            .names
            .insert(fund_name("Фонд Северный Доход").unwrap());
        assert_eq!(search.reasons(&json!({"instrumentUid":"other", "operationType":"OPERATION_TYPE_INPUT_SECURITIES"}), Some(unrelated)), vec!["input_like"]);
        assert_eq!(
            search.reasons(
                &json!({"instrumentUid":"twin", "operationType":"OPERATION_TYPE_OUTPUT"}),
                Some(twin)
            ),
            vec!["same_isin"]
        );
        assert_eq!(search.reasons(&json!({"description":"Зачислены паи ФОНД СЕВЕРНЫЙ ДОХОД", "instrumentUid":"unknown"}), None), vec!["fund_name"]);
        assert!(
            search
                .reasons(
                    &json!({"description":"Другой фонд", "operationType":"OPERATION_TYPE_OUTPUT"}),
                    Some(unrelated)
                )
                .is_empty()
        );
        search.trace(
            &json!({"operationType":"OPERATION_TYPE_INPUT", "figi":"UNRELATED"}),
            None,
            "invented",
        );
        assert!(!search.twin_figis.contains("UNRELATED"));
        search.trace(
            &json!({"isin":"INVENTED_ISIN", "figi":"TWIN"}),
            None,
            "invented",
        );
        assert!(search.twin_figis.contains("TWIN"));
        assert!(fund_name("").is_none());
        assert!(fund_name("Паевой инвестиционный фонд").is_none());
    }

    #[test]
    fn legacy_type_text_does_not_hide_input_operation_type() {
        assert!(input_like(
            &json!({"type":"Пополнение", "operationType":"OPERATION_TYPE_INPUT"})
        ));
        for number in [1, 17, 51, 54, 60] {
            assert!(input_like(&json!({"operationType":number})));
        }
        assert!(!input_like(
            &json!({"operationType":"OPERATION_TYPE_OUTPUT"})
        ));
    }

    #[test]
    fn formation_windows_cover_requested_quarter_without_overlap_or_oversized_requests() {
        let windows = formation_windows();
        assert_eq!(
            windows.first().unwrap().0,
            time::macros::date!(2023 - 10 - 01)
        );
        assert_eq!(
            windows.last().unwrap().1,
            time::macros::date!(2023 - 12 - 31)
        );
        for (from, to) in &windows {
            assert!((0..31).contains(&(*to - *from).whole_days()));
        }
        for pair in windows.windows(2) {
            assert_eq!(pair[0].1.next_day().unwrap(), pair[1].0);
        }
    }

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
