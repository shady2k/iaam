//! Narrow fund-withdrawal evidence used to join two rows of one redemption.

use super::*;
use iaam_core::money::Quantity;

const PAIR_WINDOW_DAYS: i64 = 30;

#[derive(Default)]
pub(super) struct Pairing {
    pub withdrawals: HashSet<usize>,
    pub redemptions: HashMap<usize, (Quantity, String)>,
    pub refusals: HashMap<usize, String>,
}

// No other free text is quantity evidence. In particular the fund's name is
// not an instrument identifier: the caller has already resolved identity.
fn described_quantity(operation: &ChannelOperation) -> Option<Result<Quantity, String>> {
    let description = operation.raw.get("description")?.as_str()?;
    let body = description
        .strip_prefix("Вывод ")?
        .strip_suffix(" в другой депозитарий")?;
    let (number, fund) = body.split_once(" лотов фонда ")?;
    if fund.trim().is_empty() || number.is_empty() {
        return None;
    }
    let exact = match Decimal::from_str_exact(&number.replace(',', ".")) {
        Ok(exact) if exact > Decimal::ZERO => exact,
        _ => {
            return Some(Err(
                "fund withdrawal description requires a positive decimal quantity".to_owned(),
            ));
        }
    };
    let reported = operation.quantity.map(|quantity| quantity.0.inner());
    if reported != Some(exact.trunc()) {
        tracing::debug!(operation_id = operation.operation_id, reported = ?reported,
            described = %exact, reason = "fund_quantity_mismatch", "fund withdrawal quantity refused");
        return Some(Err(
            "fund withdrawal description quantity does not match the reported integer quantity"
                .to_owned(),
        ));
    }
    if exact.fract() != Decimal::ZERO {
        tracing::debug!(operation_id = operation.operation_id, reported = ?reported,
            described = %exact, "using exact fractional fund withdrawal quantity");
    }
    Some(Ok(Quantity(Dec::new(exact))))
}

pub(super) fn prepare(
    operations: &mut [ChannelOperation],
    dictionary: &OperationKindDictionary,
) -> Pairing {
    let mut result = Pairing::default();
    let mut withdrawals = Vec::new();
    let mut redemptions = Vec::new();
    for (index, operation) in operations.iter_mut().enumerate() {
        if operation.rejection.is_some() || operation.state != ChannelOrderState::Executed {
            continue;
        }
        match dictionary.kind_of(&operation.source_kind) {
            ChannelOperationKind::SecuritiesTransferOut
                if operation
                    .payment
                    .is_some_and(|money| money.amount.raw() == 0) =>
            {
                if let Some(quantity) = described_quantity(operation) {
                    match quantity {
                        Ok(quantity) => operation.quantity = Some(quantity),
                        Err(reason) => {
                            result.refusals.insert(index, reason);
                        }
                    }
                    withdrawals.push(index);
                }
            }
            ChannelOperationKind::BondRedemption
                if operation.quantity == Some(Quantity::zero())
                    && operation
                        .payment
                        .is_some_and(|money| money.amount.raw() > 0) =>
            {
                redemptions.push(index);
            }
            _ => {}
        }
    }
    let mut edges = Vec::new();
    for &withdrawal in &withdrawals {
        let out = &operations[withdrawal];
        for &redemption in &redemptions {
            let paid = &operations[redemption];
            if out.broker_account_id != paid.broker_account_id
                || out
                    .instrument_uid
                    .as_deref()
                    .is_none_or(|id| Uuid::parse_str(id).is_err())
                || out.instrument_uid != paid.instrument_uid
            {
                continue;
            }
            let (Some(out_date), Some(paid_date)) = (out.date, paid.date) else {
                continue;
            };
            let days = (paid_date - out_date).whole_days();
            if (0..=PAIR_WINDOW_DAYS).contains(&days) {
                edges.push((withdrawal, redemption));
            }
        }
    }
    for &(withdrawal, redemption) in &edges {
        let ambiguous = edges.iter().filter(|(out, _)| *out == withdrawal).count() != 1
            || edges.iter().filter(|(_, paid)| *paid == redemption).count() != 1;
        if ambiguous || result.refusals.contains_key(&withdrawal) {
            let reason = if ambiguous {
                "ambiguous fund withdrawal/redemption pair: no one-to-one match".to_owned()
            } else {
                result.refusals[&withdrawal].clone()
            };
            tracing::debug!(
                withdrawal_operation_id = operations[withdrawal].operation_id,
                redemption_operation_id = operations[redemption].operation_id,
                reason,
                "fund redemption pair refused"
            );
            result.refusals.insert(withdrawal, reason.clone());
            result.refusals.insert(redemption, reason);
        }
    }
    for (withdrawal, redemption) in edges {
        if result.refusals.contains_key(&withdrawal) || result.refusals.contains_key(&redemption) {
            continue;
        }
        let out = &operations[withdrawal];
        result.withdrawals.insert(withdrawal);
        result.redemptions.insert(
            redemption,
            (
                out.quantity.expect("validated withdrawal quantity"),
                out.operation_id.clone(),
            ),
        );
        tracing::info!(
            withdrawal_operation_id = out.operation_id,
            redemption_operation_id = operations[redemption].operation_id,
            window_days = PAIR_WINDOW_DAYS,
            "paired fund withdrawal and redemption"
        );
    }
    for withdrawal in withdrawals {
        if !result.withdrawals.contains(&withdrawal) && !result.refusals.contains_key(&withdrawal) {
            tracing::info!(
                operation_id = operations[withdrawal].operation_id,
                window_days = PAIR_WINDOW_DAYS,
                reason = "no_matching_redemption",
                "unpaired fund withdrawal retained as outbound securities"
            );
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary() -> OperationKindDictionary {
        OperationKindDictionary::build(
            iaam_broker::tinkoff::dictionary_seed::TINKOFF_OPERATION_KINDS
                .iter()
                .copied(),
        )
        .0
    }

    fn rows(quantity: &str, described: &str, paid_date: &str) -> Vec<ChannelOperation> {
        let instrument = "00000000-0000-0000-0000-000000000123";
        let body = serde_json::json!({"hasNext": false, "items": [
            {"id": "fund-withdrawal", "brokerAccountId": "invented-account", "cursor": "out",
             "instrumentUid": instrument, "date": "2026-08-01T10:00:00Z",
             "type": "OPERATION_TYPE_OUTPUT_SECURITIES", "state": "OPERATION_STATE_EXECUTED",
             "quantity": quantity, "payment": {"currency": "rub", "units": "0", "nano": 0},
             "description": format!("Вывод {described} лотов фонда Invented Fund в другой депозитарий")},
            {"id": "fund-redemption", "brokerAccountId": "invented-account", "cursor": "paid",
             "instrumentUid": instrument, "date": paid_date,
             "type": "OPERATION_TYPE_BOND_REPAYMENT_FULL", "state": "OPERATION_STATE_EXECUTED",
             "quantity": "0", "payment": {"currency": "rub", "units": "71", "nano": 0}}
        ]});
        parse_operations(&body.to_string()).unwrap().operations
    }

    #[test]
    fn fractional_pair_reduces_one_source_row_and_preserves_redemption_raw() {
        let operations = rows("12", "12.75", "2026-08-31T10:00:00Z");
        let original = operations[1].raw.clone();
        let (parsed, accounting) =
            adapt_operations_accounted(AccountId::new_random(), operations, &dictionary()).unwrap();
        assert!(parsed.accepted.is_empty());
        assert!(parsed.journal.is_empty());
        assert!(parsed.quarantined.is_empty());
        assert_eq!(parsed.pending_redemptions.len(), 1);
        let pending = &parsed.pending_redemptions[0];
        assert_eq!(
            pending.withdrawn_quantity.unwrap().0.inner(),
            Decimal::from_str_exact("12.75").unwrap()
        );
        assert_eq!(
            pending.withdrawal_operation_id.as_deref(),
            Some("fund-withdrawal")
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pending.raw).unwrap(),
            original
        );
        assert_eq!(accounting.source_rows_accepted, 1);
        assert_eq!(accounting.reduced_paired_fund_withdrawals, 1);
        assert_eq!(accounting.quarantined_conversion, 0);
    }

    #[test]
    fn no_pair_outside_window_or_across_accounts_or_instruments() {
        for variation in 0..4 {
            let mut operations = rows("12", "12,75", "2026-08-02T10:00:00Z");
            match variation {
                0 => operations[1].date = Some(time::macros::date!(2026 - 09 - 01)),
                1 => operations[1].broker_account_id = "another-invented-account".to_owned(),
                2 => operations[1].instrument_uid = Some(Uuid::new_v4().to_string()),
                _ => operations[1].date = Some(time::macros::date!(2026 - 07 - 31)),
            }
            let (parsed, accounting) =
                adapt_operations_accounted(AccountId::new_random(), operations, &dictionary())
                    .unwrap();
            assert_eq!(parsed.accepted.len(), 1);
            assert!(matches!(parsed.accepted[0].kind,
                OperationKind::SecuritiesTransferOut { quantity, .. }
                if quantity.inner() == Decimal::from_str_exact("12.75").unwrap()));
            assert_eq!(parsed.pending_redemptions.len(), 1);
            assert!(parsed.pending_redemptions[0].withdrawn_quantity.is_none());
            assert!(parsed.quarantined.is_empty());
            assert_eq!(accounting.source_rows_accepted, 2);
            assert_eq!(accounting.reduced_paired_fund_withdrawals, 0);
        }
    }

    #[test]
    fn mismatched_fraction_refuses_both_rows() {
        let operations = rows("13", "12.75", "2026-08-02T10:00:00Z");
        let (parsed, accounting) =
            adapt_operations_accounted(AccountId::new_random(), operations, &dictionary()).unwrap();
        assert!(parsed.accepted.is_empty());
        assert!(parsed.pending_redemptions.is_empty());
        assert_eq!(parsed.quarantined.len(), 2);
        assert_eq!(accounting.source_rows_accepted, 0);
        assert_eq!(accounting.quarantined_conversion, 2);
    }

    #[test]
    fn ambiguous_pair_refuses_all_candidates() {
        let mut operations = rows("12", "12.75", "2026-08-02T10:00:00Z");
        let mut duplicate = operations[1].clone();
        duplicate.operation_id = "another-redemption".to_owned();
        operations.push(duplicate);
        let (parsed, accounting) =
            adapt_operations_accounted(AccountId::new_random(), operations, &dictionary()).unwrap();
        assert!(parsed.accepted.is_empty());
        assert!(parsed.pending_redemptions.is_empty());
        assert_eq!(parsed.quarantined.len(), 3);
        assert_eq!(accounting.source_rows_accepted, 0);
    }

    #[test]
    fn subunit_quantity_and_malformed_narrow_quantity() {
        let mut operations = rows("0", "0.75", "2026-08-02T10:00:00Z");
        let pairing = prepare(&mut operations, &dictionary());
        assert_eq!(
            pairing.redemptions[&1].0.0.inner(),
            Decimal::from_str_exact("0.75").unwrap()
        );
        assert!(pairing.refusals.is_empty());
        let mut operations = rows("12", "12.7.5", "2026-08-02T10:00:00Z");
        let pairing = prepare(&mut operations, &dictionary());
        assert!(pairing.withdrawals.is_empty());
        assert_eq!(pairing.refusals.len(), 2);
    }

    #[test]
    fn unrelated_description_does_not_supply_quantity() {
        let mut operations = rows("12", "12.75", "2026-08-02T10:00:00Z");
        operations[0].raw["description"] = serde_json::json!("Other transfer of 12.75 units");
        let pairing = prepare(&mut operations, &dictionary());
        assert!(pairing.withdrawals.is_empty());
        assert_eq!(operations[0].quantity.unwrap().0.inner(), Decimal::from(12));
    }
}
