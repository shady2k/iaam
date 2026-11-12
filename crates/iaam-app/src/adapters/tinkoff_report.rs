//! Resolve only subminor trade payments from the broker's settled report.

use super::*;
use iaam_broker::tinkoff::{
    needs_report_payment, parse_broker_report_page, parse_broker_report_task,
    resolve_report_payment,
};

impl TinkoffChannel {
    pub(super) async fn resolve_report_payments(
        &self,
        operations: &mut [ChannelOperation],
        account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<(), BrokerError> {
        if !operations.iter().any(needs_report_payment) {
            return Ok(());
        }
        let rows = match self
            .read_settlement_report(account, from, to, context)
            .await
        {
            Ok(rows) => rows,
            Err(BrokerError::Refused { .. } | BrokerError::Unparsable { .. }) => {
                tracing::warn!(
                    phase = "resolve_report_payment",
                    "broker report unavailable; subminor payments remain refused"
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        };
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
        Ok(())
    }

    pub(super) async fn read_settlement_report(
        &self,
        account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) -> Result<Vec<serde_json::Value>, BrokerError> {
        // The published GenerateBrokerReportRequest documents UTC bounds, but
        // no period-length or age limit. Do not apply the foreign-dividend
        // report's same-calendar-year restriction to this method.
        tracing::debug!(
            phase = "broker_report",
            step = "generate",
            account,
            from = %from,
            to = %to,
            "requesting broker settlement report"
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
        trace_report_response("generate", None, &response);
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
                tracing::debug!(
                    phase = "broker_report", step = "get", page = page_number,
                    account, from = %from, to = %to,
                    "requesting broker settlement report page"
                );
                let response = self
                    .client
                    .get_broker_report(&task, page_number, context.deadline, context.allowance)
                    .await;
                trace_report_response("get", Some(page_number), &response);
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
            expected_count = Some(page.items_count);
            let empty = page.broker_report.is_empty();
            rows.extend(page.broker_report);
            if rows.len() == page.items_count {
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

fn trace_report_response(step: &str, page: Option<i32>, response: &Result<String, TinkoffError>) {
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
    async fn broker_report_keeps_multi_year_period_without_undocumented_splitting() {
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
        let rows = channel
            .read_settlement_report(
                "invented-report-account",
                time::macros::date!(2024 - 12 - 30),
                time::macros::date!(2026 - 01 - 02),
                BrokerRequestContext {
                    deadline: None,
                    allowance: &allowance,
                },
            )
            .await
            .expect("empty report");
        assert!(rows.is_empty());
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].contains("2024-12-30T00:00:00Z"),
            "{}",
            requests[0]
        );
        assert!(
            requests[0].contains("2026-01-02T23:59:59"),
            "{}",
            requests[0]
        );
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
