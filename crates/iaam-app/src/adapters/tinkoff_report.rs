//! Resolve only subminor trade payments from the broker's settled report.

use super::*;
use iaam_broker::tinkoff::{
    needs_report_payment, parse_broker_report_page, parse_broker_report_task,
    resolve_report_payment,
};

// The live broker rejects periods over 31 inclusive calendar days (30064).
const REPORT_PERIOD_DAYS: i64 = 31;
const REPORT_OPERATION_MARGIN_DAYS: i64 = 15;

/// Read near affected operations, rather than replaying the full sync history.
pub(super) fn report_windows(dates: &[Date], from: Date, to: Date) -> Vec<(Date, Date)> {
    let mut dates: Vec<_> = dates.to_vec();
    dates.sort_unstable();
    dates.dedup();
    let margin = time::Duration::days(REPORT_OPERATION_MARGIN_DAYS);
    let mut windows: Vec<(Date, Date)> = Vec::new();
    for date in dates {
        if windows.last().is_some_and(|(_, end)| date <= *end) {
            continue;
        }
        let (start, end) = if from <= date && date <= to {
            (
                date.saturating_sub(margin).max(from),
                date.saturating_add(margin).min(to),
            )
        } else {
            (date.saturating_sub(margin), date.saturating_add(margin))
        };
        if let Some((previous_start, previous_end)) = windows.last_mut()
            && start <= previous_end.saturating_add(time::Duration::days(1))
            && (end - *previous_start).whole_days() < REPORT_PERIOD_DAYS
        {
            *previous_end = end;
        } else {
            // Do not request overlapping dates twice when the next affected
            // operation lies just beyond the previous report window.
            let start = windows.last().map_or(start, |(_, previous_end)| {
                start.max(previous_end.saturating_add(time::Duration::days(1)))
            });
            windows.push((start, end));
        }
    }
    windows
}

fn split_report_period(from: Date, to: Date) -> Vec<(Date, Date)> {
    let mut windows = Vec::new();
    let mut start = from;
    while start <= to {
        let end = start
            .saturating_add(time::Duration::days(REPORT_PERIOD_DAYS - 1))
            .min(to);
        windows.push((start, end));
        if end == to {
            break;
        }
        start = end.saturating_add(time::Duration::days(1));
    }
    windows
}

impl TinkoffChannel {
    pub(super) async fn resolve_report_payments(
        &self,
        operations: &mut [ChannelOperation],
        account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<Vec<serde_json::Value>, BrokerError> {
        let dates: Vec<_> = operations
            .iter()
            .filter(|operation| needs_report_payment(operation))
            .filter_map(|operation| operation.date)
            .collect();
        let mut rows = Vec::new();
        let windows = report_windows(&dates, from, to);
        tracing::debug!(
            phase = "resolve_report_payment", affected_dates = ?dates,
            window_count = windows.len(), windows = ?windows,
            "planned broker settlement report windows"
        );
        for (from, to) in windows {
            match self
                .read_settlement_report(account, from, to, context)
                .await
            {
                Ok(window_rows) => rows.extend(window_rows),
                Err(BrokerError::Refused { .. } | BrokerError::Unparsable { .. }) => {
                    tracing::warn!(
                        phase = "resolve_report_payment",
                        "broker report unavailable; subminor payments remain refused"
                    );
                    return Ok(rows);
                }
                Err(error) => return Err(error),
            }
        }
        for operation in operations
            .iter_mut()
            .filter(|operation| needs_report_payment(operation))
        {
            match resolve_report_payment(operation, &rows) {
                Ok(resolved) => *operation = resolved,
                Err(reason) => tracing::warn!(
                    phase = "resolve_report_payment",
                    operation_id = operation.operation_id,
                    reason,
                    "trade payment remains refused"
                ),
            }
        }
        Ok(rows)
    }

    pub(super) async fn read_settlement_report(
        &self,
        account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<Vec<serde_json::Value>, BrokerError> {
        let mut rows = Vec::new();
        for (from, to) in split_report_period(from, to) {
            rows.extend(
                self.read_report_window(account, (from, to), context, false)
                    .await?,
            );
        }
        Ok(rows)
    }

    pub(super) async fn read_diagnostic_report(
        &self,
        account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<Vec<serde_json::Value>, BrokerError> {
        let mut rows = Vec::new();
        for (from, to) in split_report_period(from, to) {
            rows.extend(
                self.read_report_window(account, (from, to), context, true)
                    .await?,
            );
        }
        Ok(rows)
    }

    async fn read_report_window(
        &self,
        account: &str,
        period: (Date, Date),
        context: BrokerRequestContext<'_>,
        diagnostic: bool,
    ) -> Result<Vec<serde_json::Value>, BrokerError> {
        let (from, to) = period;
        if diagnostic
            && (context.allowance.remaining() <= iaam_http::gateway::DIAGNOSTIC_REQUEST_RESERVE
                || !self.client.wait_diagnostic_report(context.deadline).await)
        {
            tracing::debug!(
                phase = "broker_report", account, from = %from, to = %to,
                status = "skipped", reason = "low_rate_capacity_or_host_pause",
                "broker report diagnostic deferred"
            );
            return Err(unparsable(
                "diagnostic report deferred to preserve broker capacity".to_owned(),
            ));
        }
        tracing::debug!(
            phase = "broker_report", step = "generate", account,
            from = %from, to = %to, "requesting broker settlement report"
        );
        let response = self
            .client
            .generate_broker_report(
                account,
                &rfc3339_midnight(from),
                &rfc3339_operation_end(to),
                context.deadline,
                context.allowance,
            )
            .await;
        trace_report_response("generate", None, account, period, &response);
        let body = response.map_err(tinkoff_error)?;
        let task = parse_broker_report_task(&body).map_err(|error| {
            tracing::warn!(
                phase = "broker_report",
                step = "generate",
                failure = "task_parse",
                "broker report response could not be parsed"
            );
            parse_error(error)
        })?;
        let mut rows = Vec::new();
        let mut page_number = 0;
        let mut expected_count = None;
        loop {
            // 30058 is the published asynchronous-task-not-ready response.
            // The shared allowance and deadline apply to every poll, too.
            let mut polls = 0;
            let body = loop {
                if diagnostic
                    && (context.allowance.remaining()
                        <= iaam_http::gateway::DIAGNOSTIC_REQUEST_RESERVE
                        || !self.client.wait_diagnostic_report(context.deadline).await)
                {
                    tracing::debug!(
                        phase = "broker_report", account, from = %from, to = %to,
                        page = page_number, status = "skipped", reason = "low_rate_capacity_or_host_pause",
                        "broker report diagnostic deferred"
                    );
                    return Err(unparsable(
                        "diagnostic report deferred to preserve broker capacity".to_owned(),
                    ));
                }
                tracing::debug!(
                    phase = "broker_report", step = "get", page = page_number,
                    account, from = %from, to = %to,
                    "requesting broker settlement report page"
                );
                let response = self
                    .client
                    .get_broker_report(&task, page_number, context.deadline, context.allowance)
                    .await;
                trace_report_response("get", Some(page_number), account, period, &response);
                match response {
                    Err(TinkoffError::UnexpectedStatus { status: 400, body })
                        if report_not_ready(&body) && polls < 2 =>
                    {
                        polls += 1;
                    }
                    response => break response.map_err(tinkoff_error)?,
                }
            };
            let page = parse_broker_report_page(&body).map_err(|error| {
                tracing::warn!(
                    phase = "broker_report",
                    step = "get",
                    page = page_number,
                    failure = "page_parse",
                    "broker report response could not be parsed"
                );
                parse_error(error)
            })?;
            if page.page != page_number
                || page.pages_count < 0
                || expected_count.is_some_and(|count| count != page.items_count)
            {
                tracing::warn!(
                    phase = "broker_report",
                    step = "get",
                    page = page_number,
                    failure = "pagination_metadata_changed",
                    "broker report pagination invalid"
                );
                return Err(unparsable(
                    "broker report pagination metadata changed".to_owned(),
                ));
            }
            tracing::debug!(
                phase = "broker_report", step = "get", account, from = %from, to = %to,
                page = page_number, received_rows = page.broker_report.len(),
                "broker report page received"
            );
            expected_count = Some(page.items_count);
            let empty = page.broker_report.is_empty();
            rows.extend(page.broker_report);
            if rows.len() == page.items_count {
                tracing::debug!(
                    phase = "broker_report", account, from = %from, to = %to,
                    received_rows = rows.len(), "broker report window complete"
                );
                return Ok(rows);
            }
            if empty || rows.len() > page.items_count || page_number >= page.pages_count {
                tracing::warn!(
                    phase = "broker_report",
                    step = "get",
                    page = page_number,
                    failure = "incomplete",
                    received_rows = rows.len(),
                    expected_rows = page.items_count,
                    "broker report pagination invalid"
                );
                return Err(unparsable("broker report is incomplete".to_owned()));
            }
            page_number += 1;
        }
    }
}

/// Select only the gateway's documented error fields; never log its body.
fn report_error_fields(body: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return (None, None);
    };
    let code = value.get("code").and_then(|code| {
        code.as_str()
            .map(str::to_owned)
            .or_else(|| code.as_u64().map(|code| code.to_string()))
    });
    let message = value
        .get("message")
        .and_then(|message| message.as_str())
        .map(str::to_owned);
    (code, message)
}

fn trace_report_response(
    step: &str,
    page: Option<i32>,
    account: &str,
    (from, to): (Date, Date),
    response: &Result<String, TinkoffError>,
) {
    let (status, code, message) = match response {
        Ok(_) => (Some(200), None, None),
        Err(TinkoffError::UnexpectedStatus { status, body }) => {
            let (code, message) = report_error_fields(body);
            (Some(*status), code, message)
        }
        Err(TinkoffError::Unreachable { status, .. }) => (*status, None, None),
        Err(TinkoffError::InvalidToken) => (Some(401), None, None),
        Err(_) => (None, None, None),
    };
    let failure = match response {
        Ok(_) => None,
        Err(TinkoffError::UnexpectedStatus { .. }) => Some("unexpected_status"),
        Err(TinkoffError::Unreachable { .. }) => Some("unreachable"),
        Err(TinkoffError::RequestCeiling { .. }) => Some("request_ceiling"),
        Err(TinkoffError::InvalidToken) => Some("invalid_token"),
        Err(TinkoffError::MethodUnavailable { .. }) => Some("method_unavailable"),
        Err(TinkoffError::PartialResponse) => Some("partial_response"),
        Err(TinkoffError::MalformedResponse) => Some("malformed_response"),
        Err(TinkoffError::RequestSerialization) => Some("request_serialization"),
        Err(TinkoffError::Transport(_)) => Some("transport"),
        Err(TinkoffError::Gateway(_)) => Some("gateway"),
    };
    macro_rules! trace_response {
        ($level:ident) => {
            tracing::$level!(
                phase = "broker_report",
                step,
                page,
                account,
                from = %from,
                to = %to,
                status,
                broker_error_code = code.as_deref(),
                broker_error_message = message.as_deref(),
                succeeded = response.is_ok(),
                failure,
                "broker settlement report response"
            );
        };
    }
    if response.is_ok() || code.as_deref() == Some("30058") {
        trace_response!(debug);
    } else {
        trace_response!(warn);
    }
}

fn report_not_ready(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("code").cloned())
        .is_some_and(|code| code.as_str() == Some("30058") || code.as_u64() == Some(30058))
}

#[cfg(test)]
mod tests {
    use super::super::fake::{self, Answer};
    use super::*;
    use iaam_broker::credentials::{Key, open, seal};
    use iaam_broker::environment::Environment;
    use tracing::instrument::WithSubscriber;

    #[derive(Clone, Default)]
    struct ReportLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for ReportLog {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("report log").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn report_channel(gateway: Arc<dyn iaam_http::Outbound>) -> TinkoffChannel {
        let key = Key::from_bytes([5; 32]);
        let token = open(&key, &seal(&key, "invented-report-token")).expect("token");
        let store = iaam_store::SqliteStore::open_in_memory().expect("catalogue");
        TinkoffChannel::new(
            TinkoffClient::new(Environment::Prod, token, gateway),
            SourceId::new_random(),
            OperationKindDictionary::default(),
            Arc::new(crate::adapters::sqlite::SqliteAdapter::new(store)),
        )
    }

    #[tokio::test]
    async fn report_failure_logs_selected_gateway_fields_without_raw_payload() {
        let (gateway, requests, _) = fake::gateway(
            vec![Answer::status(
                400,
                r#"{"code":30064,"message":"Invented report failure","details":"private-extra","payment":{"units":"123"}}"#,
            )],
            None,
        );
        let channel = report_channel(gateway);
        let writer = ReportLog::default();
        let capture = writer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .without_time()
            .with_writer(move || capture.clone())
            .finish();
        let allowance = iaam_http::RequestAllowance::new(8);
        let result = channel
            .read_settlement_report(
                "invented-report-account",
                time::macros::date!(2025 - 12 - 30),
                time::macros::date!(2026 - 01 - 02),
                BrokerRequestContext {
                    deadline: None,
                    allowance: &allowance,
                },
            )
            .with_subscriber(subscriber)
            .await;
        assert!(matches!(result, Err(BrokerError::Refused { .. })));
        assert_eq!(requests.lock().expect("requests").len(), 1);
        let logs = String::from_utf8(writer.0.lock().expect("report log").clone()).expect("UTF-8");
        for field in [
            "generate",
            "invented-report-account",
            "2025-12-30",
            "2026-01-02",
            "400",
            "30064",
            "Invented report failure",
        ] {
            assert!(logs.contains(field), "missing {field}: {logs}");
        }
        for excluded in ["private-extra", "payment", "invented-report-token"] {
            assert!(!logs.contains(excluded), "leaked {excluded}: {logs}");
        }
    }

    #[tokio::test]
    async fn settlement_report_splits_period_at_31_inclusive_days() {
        let mut answers = Vec::new();
        for _ in 0..3 {
            answers.push(Answer::status(
                200,
                r#"{"generateBrokerReportResponse":{"taskId":"invented-task"}}"#,
            ));
            answers.push(Answer::status(200,
                r#"{"getBrokerReportResponse":{"brokerReport":[],"itemsCount":0,"pagesCount":0,"page":0}}"#));
        }
        let (gateway, requests, _) = fake::gateway(answers, None);
        let channel = report_channel(gateway);
        let allowance = iaam_http::RequestAllowance::new(8);
        let from = time::macros::date!(2025 - 12 - 15);
        let to = time::macros::date!(2026 - 02 - 15);
        let rows = channel
            .read_settlement_report(
                "invented-report-account",
                from,
                to,
                BrokerRequestContext {
                    deadline: None,
                    allowance: &allowance,
                },
            )
            .await
            .expect("empty reports");
        assert!(rows.is_empty());
        let requests = requests.lock().expect("requests");
        let windows = split_report_period(from, to);
        assert_eq!(requests.len(), windows.len() * 2);
        for (request, (start, end)) in requests.iter().step_by(2).zip(windows) {
            assert!(request.contains(&rfc3339_midnight(start)));
            assert!(request.contains(&rfc3339_operation_end(end)));
            assert!((end - start).whole_days() < REPORT_PERIOD_DAYS);
        }
    }

    #[test]
    fn long_period_windows_cover_every_date_once_including_leap_day() {
        let from = time::macros::date!(2024 - 01 - 31);
        let to = time::macros::date!(2026 - 02 - 01);
        let windows = split_report_period(from, to);
        assert_eq!(windows.first().unwrap().0, from);
        assert_eq!(windows.last().unwrap().1, to);
        for &(start, end) in &windows {
            assert!((end - start).whole_days() < REPORT_PERIOD_DAYS);
        }
        for pair in windows.windows(2) {
            assert_eq!(pair[0].1.next_day().unwrap(), pair[1].0);
        }
    }

    #[test]
    fn affected_dates_reuse_nearby_windows_without_scanning_full_history() {
        use time::macros::date;
        let dates = [
            date!(2026 - 08 - 20),
            date!(2026 - 08 - 21),
            date!(2026 - 10 - 20),
        ];
        let windows = report_windows(&dates, date!(2020 - 01 - 01), date!(2026 - 11 - 01));
        assert_eq!(windows.len(), 2);
        for date in dates {
            assert!(
                windows
                    .iter()
                    .any(|(start, end)| *start <= date && date <= *end)
            );
        }
        for (start, end) in windows {
            assert!((end - start).whole_days() < REPORT_PERIOD_DAYS);
            assert!(start > date!(2026 - 01 - 01));
        }
        assert!(report_windows(&[], date!(2020 - 01 - 01), date!(2026 - 11 - 01)).is_empty());
    }

    #[test]
    fn operation_date_outside_sync_bounds_still_has_a_report_window() {
        use time::macros::date;
        let operation = date!(2026 - 08 - 20);
        let windows = report_windows(&[operation], date!(2026 - 09 - 01), date!(2026 - 09 - 30));
        assert_eq!(windows.len(), 1);
        assert!(windows[0].0 <= operation && operation <= windows[0].1);
        assert!((windows[0].1 - windows[0].0).whole_days() < REPORT_PERIOD_DAYS);
    }

    #[tokio::test]
    async fn settlement_payment_requests_only_the_affected_operation_window() {
        use time::macros::date;
        let body = serde_json::json!({"hasNext": false, "items": [{
            "cursor":"invented", "brokerAccountId":"invented", "id":"invented", "figi":"FIGI1",
            "date":"2026-08-20T10:00:00Z", "type":"OPERATION_TYPE_BUY", "state":"OPERATION_STATE_EXECUTED",
            "quantity":"92", "payment":{"currency":"rub", "units":"-173", "nano":-926000000},
            "tradesInfo":{"trades":[{"num":"trade-1", "date":"2026-08-20T10:00:00Z", "quantity":"92",
                "price":{"currency":"rub", "units":"1", "nano":890500000}}]}
        }]});
        let mut operations = parse_operations(&body.to_string()).unwrap().operations;
        assert!(needs_report_payment(&operations[0]));
        let (gateway, requests, _) = fake::gateway(
            vec![
                Answer::status(
                    200,
                    r#"{"generateBrokerReportResponse":{"taskId":"invented-task"}}"#,
                ),
                Answer::status(
                    200,
                    r#"{"getBrokerReportResponse":{"brokerReport":[],"itemsCount":0,"pagesCount":0,"page":0}}"#,
                ),
            ],
            None,
        );
        let channel = report_channel(gateway);
        let allowance = iaam_http::RequestAllowance::new(8);
        channel
            .resolve_report_payments(
                &mut operations,
                "invented",
                date!(2020 - 01 - 01),
                date!(2026 - 11 - 01),
                BrokerRequestContext {
                    deadline: None,
                    allowance: &allowance,
                },
            )
            .await
            .unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let request: serde_json::Value = serde_json::from_str(&requests[0]).unwrap();
        let period = &request["generateBrokerReportRequest"];
        let start = OffsetDateTime::parse(
            period["from"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let end = OffsetDateTime::parse(
            period["to"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(end - start <= time::Duration::days(REPORT_PERIOD_DAYS));
        assert!(start.date() <= operations[0].date.unwrap());
        assert!(end.date() >= operations[0].date.unwrap());
        assert!(start.date() > date!(2026 - 01 - 01));
    }

    #[test]
    fn report_error_fields_accept_only_published_code_and_message() {
        assert_eq!(
            report_error_fields(r#"{"code":"30058","message":"Pending","details":"excluded"}"#),
            (Some("30058".to_owned()), Some("Pending".to_owned()))
        );
        assert_eq!(report_error_fields("invalid response"), (None, None));
        assert_eq!(
            report_error_fields(r#"{"errorCode":30064,"errorMessage":"not published fields"}"#),
            (None, None)
        );
    }

    #[test]
    fn only_report_pending_code_is_polled() {
        assert!(super::report_not_ready(r#"{"code":30058}"#));
        assert!(super::report_not_ready(r#"{"code":"30058"}"#));
        assert!(!super::report_not_ready(r#"{"code":30059}"#));
        assert!(!super::report_not_ready("invalid"));
    }
}
