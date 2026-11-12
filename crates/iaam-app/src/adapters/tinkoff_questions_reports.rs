//! Optional evidence for the open commission and trading-day questions.

use super::*;

impl TinkoffChannel {
    pub(super) async fn trace_question_reports(
        &self,
        operations: &[ChannelOperation],
        account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) {
        let mut dates = questions_commission::affected_dates(operations);
        dates.extend(questions_zone::affected_dates(operations));
        let mut rows = Vec::new();
        if !dates.is_empty() && tracing::enabled!(tracing::Level::DEBUG) {
            let windows = report::report_windows(&dates, from, to);
            tracing::debug!(window_count = windows.len(), windows = ?windows,
                "planned shared question report windows");
            for (from, to) in windows {
                if context.deadline.is_some_and(|deadline| {
                    deadline.saturating_duration_since(tokio::time::Instant::now().into_std())
                        <= crate::ports::BROKER_DIAGNOSTIC_RESERVE
                }) {
                    tracing::debug!(from = %from, to = %to, reason = "sync_deadline_near_or_elapsed",
                        "T-Invest question report skipped");
                    break;
                }
                if context.allowance.remaining() <= iaam_http::gateway::DIAGNOSTIC_REQUEST_RESERVE {
                    tracing::debug!(from = %from, to = %to, reason = "low_rate_capacity_or_host_pause",
                        "T-Invest question report skipped");
                    break;
                }
                let request = self.read_diagnostic_report(account, from, to, context);
                let result = if let Some(deadline) = context.deadline {
                    match tokio::time::timeout_at(
                        tokio::time::Instant::from_std(deadline)
                            - crate::ports::BROKER_DIAGNOSTIC_RESERVE,
                        request,
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            tracing::debug!(from = %from, to = %to, reason = "sync_deadline_elapsed",
                                "T-Invest question report skipped");
                            break;
                        }
                    }
                } else {
                    request.await
                };
                match result {
                    Ok(report_rows) => rows.extend(report_rows),
                    Err(_) => {
                        tracing::debug!(from = %from, to = %to, reason = "diagnostic_report_read_failed",
                            "T-Invest question report unavailable");
                        break;
                    }
                }
            }
        }
        questions_commission::trace_commission(operations, &rows);
        questions_zone::trace_trading_zone(operations, &rows);
    }
}
