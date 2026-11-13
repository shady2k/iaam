//! Optional evidence for the open commission and trading-day questions.

use super::*;

impl TinkoffChannel {
    #[cfg(test)]
    pub(super) async fn trace_question_reports(
        &self,
        operations: &[ChannelOperation],
        account: &str,
        from: Date,
        to: Date,
        context: BrokerRequestContext<'_>,
    ) {
        let rows = self
            .read_question_reports(operations, account, (from, to), context, &[])
            .await;
        questions_commission::trace_commission(operations, &rows);
        questions_zone::trace_trading_zone(operations, &rows);
    }

    pub(super) async fn read_question_reports(
        &self,
        operations: &[ChannelOperation],
        account: &str,
        interval: (Date, Date),
        context: BrokerRequestContext<'_>,
        existing_rows: &[serde_json::Value],
    ) -> Vec<serde_json::Value> {
        let (from, to) = interval;
        let uncovered: Vec<_> = operations
            .iter()
            .filter(|operation| {
                !operation.trades.iter().all(|trade| {
                    existing_rows.iter().any(|row| {
                        row.get("tradeId").and_then(serde_json::Value::as_str)
                            == Some(trade.num.as_str())
                    })
                })
            })
            .cloned()
            .collect();
        let mut dates: Vec<_> = uncovered
            .iter()
            .filter(|operation| !operation.trades.is_empty() && operation.commission.is_some())
            .flat_map(|operation| {
                operation
                    .trades
                    .iter()
                    .map(|trade| trade.at.to_offset(time::UtcOffset::UTC).date())
            })
            .collect();
        if self.question_reports_enabled {
            dates.extend(questions_commission::affected_dates(&uncovered));
            dates.extend(questions_zone::affected_dates(&uncovered));
        }
        let mut rows = existing_rows.to_vec();
        if !dates.is_empty() {
            let windows = report::report_windows(&dates, from, to);
            tracing::debug!(window_count = windows.len(), windows = ?windows,
                question_reports_enabled = self.question_reports_enabled,
                "planned shared fresh-operation report windows");
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
                    Ok(report_rows) => {
                        for row in report_rows {
                            if !rows.contains(&row) {
                                rows.push(row);
                            }
                        }
                    }
                    Err(_) => {
                        tracing::debug!(from = %from, to = %to, reason = "diagnostic_report_read_failed",
                            "T-Invest question report unavailable");
                        break;
                    }
                }
            }
        }
        rows
    }
}
