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
            Err(BrokerError::Refused { detail, .. } | BrokerError::Unparsable { detail, .. }) => {
                tracing::warn!(
                    phase = "resolve_report_payment",
                    reason = detail,
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
        let body = self
            .client
            .generate_broker_report(
                account,
                &rfc3339_midnight(from),
                &rfc3339_operation_end(to),
                context.deadline,
                context.allowance,
            )
            .await
            .map_err(tinkoff_error)?;
        let task = parse_broker_report_task(&body).map_err(parse_error)?;
        let mut rows = Vec::new();
        let mut page_number = 0;
        let mut expected_count = None;
        loop {
            // 30058 is the published asynchronous-task-not-ready response.
            // The shared allowance and deadline apply to every poll, too.
            let mut polls = 0;
            let body = loop {
                match self
                    .client
                    .get_broker_report(&task, page_number, context.deadline, context.allowance)
                    .await
                {
                    Err(TinkoffError::UnexpectedStatus { status: 400, body })
                        if report_not_ready(&body) && polls < 2 =>
                    {
                        polls += 1;
                    }
                    response => break response.map_err(tinkoff_error)?,
                }
            };
            let page = parse_broker_report_page(&body).map_err(parse_error)?;
            if page.page != page_number
                || page.pages_count < 0
                || expected_count.is_some_and(|count| count != page.items_count)
            {
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
                return Err(unparsable("broker report is incomplete".to_owned()));
            }
            page_number += 1;
        }
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
    #[test]
    fn only_report_pending_code_is_polled() {
        assert!(super::report_not_ready(r#"{"code":30058}"#));
        assert!(super::report_not_ready(r#"{"code":"30058"}"#));
        assert!(!super::report_not_ready(r#"{"code":30059}"#));
        assert!(!super::report_not_ready("invalid"));
    }
}
