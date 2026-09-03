//! A normalized operation must produce an event that passes the core's
//! structural validation. This is the seam where everything breaks:
//! ingestion builds the legs, and the core defines their shape.

use iaam_core::event::kind::{EventKind, FeeOrigin, IncomeKind, TaxOrigin, TradeSide};
use iaam_core::ids::EventId;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CalcMoney, CurrencyCode, PostedMinor};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use iaam_ingest::operation::NormalizationContext;
use iaam_ingest::{
    OperationDates, OperationKind, Rejection, SubmittedOperation, Verdict, normalize,
};
use rust_decimal::Decimal;
use time::macros::date;

fn context() -> NormalizationContext {
    NormalizationContext {
        owner: OwnerId::new_random(),
        source: SourceId::new_random(),
    }
}

fn submit(kind: OperationKind) -> SubmittedOperation {
    SubmittedOperation {
        account: AccountId::new_random(),
        kind,
        dates: OperationDates {
            cash_posted: Some(date!(2026 - 04 - 01)),
            trade: Some(date!(2026 - 04 - 01)),
            ..OperationDates::default()
        },
        source_time: None,
        idempotency_key: None,
        source_operation_id: None,
        source_category: None,
        description: None,
    }
}

fn all_kinds() -> Vec<OperationKind> {
    let instrument = InstrumentId::new_random();
    let custody = CustodyId::new_random();
    let quantity = Dec::new(Decimal::from(10));
    vec![
        OperationKind::Deposit {
            amount_minor: 100_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Withdrawal {
            amount_minor: 25_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Transfer {
            to: AccountId::new_random(),
            amount_minor: 40_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor: 900_000,
            fee_minor: Some(1_500),
            accrued_interest_minor: Some(700),
            basis_fee: None,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor: 950_000,
            fee_minor: Some(1_500),
            accrued_interest_minor: Some(300),
            basis_fee: None,
            currency: CurrencyCode::Rub,
        },
        OperationKind::Income {
            instrument: Some(instrument),
            gross_minor: 12_000,
            currency: CurrencyCode::Rub,
            kind: Some(IncomeKind::Coupon),
        },
        OperationKind::Fee {
            amount_minor: 900,
            currency: CurrencyCode::Rub,
            origin: FeeOrigin::Depositary,
        },
        OperationKind::Tax {
            amount_minor: 13_000,
            currency: CurrencyCode::Rub,
            origin: TaxOrigin::SelfPaid,
        },
        OperationKind::OpeningCash {
            amount_minor: -5_000,
            currency: CurrencyCode::Rub,
        },
        OperationKind::OpeningPosition {
            instrument,
            custody,
            quantity,
            cost_basis_minor: None,
            currency: CurrencyCode::Rub,
            assertions: None,
        },
        OperationKind::Valuation {
            instrument,
            price: Dec::new(Decimal::new(1_005, 1)),
            currency: CurrencyCode::Rub,
            quality: PriceQuality::OwnerEstimate,
        },
    ]
}

#[test]
fn every_operation_kind_produces_a_structurally_valid_event() {
    for kind in all_kinds() {
        let operation = submit(kind.clone());
        let normalized = normalize(&operation, context())
            .unwrap_or_else(|rejection| panic!("{kind:?} rejected: {rejection:?}"));
        normalized
            .event
            .validate_structure()
            .unwrap_or_else(|error| panic!("{kind:?} has an invalid shape: {error}"));
    }
}

#[test]
fn a_submitted_tax_becomes_one_negative_tax_leg() {
    let operation = submit(OperationKind::Tax {
        amount_minor: 130_000,
        currency: CurrencyCode::Rub,
        origin: TaxOrigin::SelfPaid,
    });
    let event = normalize(&operation, context())
        .expect("tax normalizes")
        .event;
    event.validate_structure().expect("valid shape");
    match &event.kind {
        EventKind::Tax { amount, origin } => {
            // The client sent a magnitude; the sign is ours.
            assert_eq!(amount.amount(), PostedMinor::new(-130_000));
            assert_eq!(*origin, TaxOrigin::SelfPaid);
        }
        other => panic!("expected a tax, got {other:?}"),
    }
    let cash = event
        .cash_effect(CurrencyCode::Rub)
        .expect("monetary effect");
    assert_eq!(cash.amount(), PostedMinor::new(-130_000));
}

#[test]
fn a_non_positive_tax_is_rejected() {
    // A client that sends -130_000 believing it helps must be told, not
    // silently obeyed: normalising the sign here would hide a caller's bug.
    for amount in [0_i64, -1] {
        let operation = submit(OperationKind::Tax {
            amount_minor: amount,
            currency: CurrencyCode::Rub,
            origin: TaxOrigin::SelfPaid,
        });
        let rejection = normalize(&operation, context()).expect_err("rejected");
        assert_eq!(rejection.field, "amount");
    }
}

#[test]
fn a_purchase_settles_for_body_plus_accrued_plus_fee() {
    // 9 000,00 principal + 7,00 accrued interest + 15,00 commission = debit of 9 022,00.
    let operation = submit(OperationKind::Buy {
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        quantity: Dec::new(Decimal::from(10)),
        gross_minor: 900_000,
        fee_minor: Some(1_500),
        accrued_interest_minor: Some(700),
        basis_fee: None,
        currency: CurrencyCode::Rub,
    });
    let event = normalize(&operation, context()).unwrap().event;
    let cash = event
        .cash_effect(CurrencyCode::Rub)
        .expect("monetary effect");
    assert_eq!(cash.amount(), PostedMinor::new(-902_200));
}

#[test]
fn a_sale_settles_for_body_plus_accrued_minus_fee() {
    // 9 500,00 principal + 3,00 accrued interest − 15,00 commission = credit of 9 488,00.
    let operation = submit(OperationKind::Sell {
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        quantity: Dec::new(Decimal::from(10)),
        gross_minor: 950_000,
        fee_minor: Some(1_500),
        accrued_interest_minor: Some(300),
        currency: CurrencyCode::Rub,
        basis_fee: None,
    });
    let event = normalize(&operation, context()).unwrap().event;
    let cash = event
        .cash_effect(CurrencyCode::Rub)
        .expect("monetary effect");
    assert_eq!(cash.amount(), PostedMinor::new(948_800));
    match event.kind {
        EventKind::Trade { side, .. } => assert_eq!(side, TradeSide::Sell),
        other => panic!("expected a trade, got {other:?}"),
    }
}

#[test]
fn a_basis_fee_is_rounded_and_retained_without_changing_cash_settlement() {
    let exact = CalcMoney::new(
        Dec::new(Decimal::from_str_exact("-0.135065").expect("exact commission")),
        CurrencyCode::Rub,
    );
    let expected_exact = CalcMoney::new(
        Dec::new(Decimal::from_str_exact("0.135065").expect("positive exact commission")),
        CurrencyCode::Rub,
    );
    let operation = submit(OperationKind::Buy {
        instrument: InstrumentId::new_random(),
        custody: CustodyId::new_random(),
        quantity: Dec::one(),
        gross_minor: 27_013,
        fee_minor: None,
        accrued_interest_minor: None,
        basis_fee: Some(exact),
        currency: CurrencyCode::Rub,
    });

    let event = normalize(&operation, context()).unwrap().event;
    event
        .validate_structure()
        .expect("negative source commission becomes a valid positive basis fee");
    assert_eq!(
        event.cash_effect(CurrencyCode::Rub).unwrap().amount(),
        PostedMinor::new(-27_013)
    );
    match event.kind {
        EventKind::Trade {
            basis_fee,
            basis_fee_exact,
            ..
        } => {
            assert_eq!(
                basis_fee.map(|fee| fee.amount()),
                Some(PostedMinor::new(14))
            );
            assert_eq!(basis_fee_exact, Some(expected_exact));
        }
        other => panic!("expected a trade, got {other:?}"),
    }
}

#[test]
fn a_negative_amount_is_rejected_with_field_expected_actual() {
    // The sign determines the operation type, not the client: a negative deposit
    // is not “corrected” into a withdrawal (§13, 422 response).
    let operation = submit(OperationKind::Deposit {
        amount_minor: -1,
        currency: CurrencyCode::Rub,
    });
    let rejection = normalize(&operation, context()).unwrap_err();
    // The field and amount are named exactly as the client sent them: one
    // kopeck is “-0.01”, not “-1”. A rejection stated in
    // internal units sends us to fix something other than what was sent.
    assert_eq!(rejection.field, "amount");
    assert_eq!(rejection.actual, "-0.01");
    assert_eq!(rejection.expected, "positive value");
}

#[test]
fn an_operation_without_any_date_is_rejected() {
    let mut operation = submit(OperationKind::Deposit {
        amount_minor: 1_000,
        currency: CurrencyCode::Rub,
    });
    operation.dates = OperationDates::default();
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "dates");
}

#[test]
fn a_transfer_to_the_same_account_is_rejected_before_the_legs_are_built() {
    let account = AccountId::new_random();
    let mut operation = submit(OperationKind::Transfer {
        to: account,
        amount_minor: 1_000,
        currency: CurrencyCode::Rub,
    });
    operation.account = account;
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "to");
}

/// Signed cash a set of events moves on one account, summed over their legs.
fn cash_on(events: &[iaam_core::event::Event], account: AccountId) -> i64 {
    events
        .iter()
        .flat_map(|event| event.legs.iter())
        .filter(|leg| leg.account == account)
        .filter_map(|leg| leg.cash_effect())
        .map(|money| money.amount().raw())
        .sum()
}

#[test]
fn a_transfer_is_submitted_once_and_moves_both_accounts_by_itself() {
    // One row states the whole movement. The receiving account is moved by the
    // leg ingestion writes for it, not by a second submission, which is why
    // there is nothing to send from the other statement.
    let main = AccountId::new_random();
    let savings = AccountId::new_random();
    let mut operation = submit(OperationKind::Transfer {
        to: savings,
        amount_minor: 50_000,
        currency: CurrencyCode::Rub,
    });
    operation.account = main;

    let event = normalize(&operation, context())
        .expect("a transfer between two accounts")
        .event;
    match event.kind {
        EventKind::CashTransfer { from, to, .. } => {
            assert_eq!(
                from, main,
                "the operation's own account is the sending side"
            );
            assert_eq!(to, savings);
        }
        other => panic!("expected a cash transfer, got {other:?}"),
    }
    let events = [event];
    assert_eq!(cash_on(&events, main), -50_000);
    assert_eq!(cash_on(&events, savings), 50_000);
}

#[test]
fn mirroring_a_transfer_from_the_other_statement_counts_it_twice() {
    // The mistake this pins: two banks each print the movement, and the caller
    // submits a row per printed side. Nothing refuses it — a second submission
    // of one movement is indistinguishable from a second movement — so both
    // accounts move by twice the sum. The transfer is submitted once, and
    // `Transfer` has no shape for the receiving half precisely because of this.
    let main = AccountId::new_random();
    let savings = AccountId::new_random();
    let one_side = |from: AccountId, to: AccountId| {
        let mut operation = submit(OperationKind::Transfer {
            to,
            amount_minor: 50_000,
            currency: CurrencyCode::Rub,
        });
        operation.account = from;
        normalize(&operation, context())
            .expect("a transfer between two accounts")
            .event
    };

    let mirrored = [one_side(main, savings), one_side(main, savings)];
    assert_eq!(
        cash_on(&mirrored, main),
        -100_000,
        "the sending account is debited once per submission, not once per movement"
    );
    assert_eq!(cash_on(&mirrored, savings), 100_000);
}

#[test]
fn a_negative_transfer_amount_is_refused_rather_than_read_as_the_outgoing_leg() {
    // The model that produces plausible output and is wrong: "one leg with a
    // negative amount". Direction is carried by the two accounts, so a sign has
    // nothing to say, and stating one is refused on the field the caller sent.
    let mut operation = submit(OperationKind::Transfer {
        to: AccountId::new_random(),
        amount_minor: -50_000,
        currency: CurrencyCode::Rub,
    });
    operation.account = AccountId::new_random();
    let rejection = normalize(&operation, context()).unwrap_err();
    assert_eq!(rejection.field, "amount");
    assert_eq!(rejection.expected, "positive value");
    assert_eq!(rejection.actual, "-500.00");
}

#[test]
fn every_verdict_has_a_machine_readable_code_and_says_whether_it_was_recorded() {
    // The verdict is a contract with the external agent (§10.4): it parses the code
    // and decides whether to retry submission. An empty code is indistinguishable from “no verdict”,
    // while “recorded” and “not recorded”, collapsed into one value,
    // turn a retry into either a duplicate or a lost operation.
    let rejection = Rejection {
        field: "amount".into(),
        expected: "positive value".into(),
        actual: "-1".into(),
    };
    let table = [
        (
            Verdict::Provisional {
                event: EventId::new_random(),
            },
            "provisional",
            true,
        ),
        (
            Verdict::Duplicate {
                existing: EventId::new_random(),
            },
            "duplicate",
            true,
        ),
        (
            Verdict::NeedsClassification {
                question: "operation kind not understood".into(),
            },
            "needs_classification",
            false,
        ),
        (
            Verdict::Unsupported {
                reason: "derivatives outside scope".into(),
            },
            "unsupported",
            false,
        ),
        (Verdict::Rejected { rejection }, "rejected", false),
    ];
    for (verdict, code, recorded) in table {
        assert_eq!(verdict.code(), code);
        assert_eq!(
            verdict.is_recorded(),
            recorded,
            "verdict {code}: recorded in the log"
        );
    }
}

#[test]
fn a_zero_amount_is_rejected_just_like_a_negative_one() {
    // Boundary: zero is not a positive amount. An operation for
    // a zero amount is not an operation, and recording it as a fact
    // clutters the log with events that did not occur.
    let zero = submit(OperationKind::Deposit {
        amount_minor: 0,
        currency: CurrencyCode::Rub,
    });
    let rejection = normalize(&zero, context()).expect_err("zero must be rejected");
    assert_eq!(rejection.field, "amount");
    assert_eq!(
        rejection.actual, "0.00",
        "amount is printed in the same units sent by the client"
    );
}

#[test]
fn the_sources_own_category_survives_normalisation_verbatim() {
    let mut operation = submit(OperationKind::Withdrawal {
        amount_minor: 120_000,
        currency: CurrencyCode::Rub,
    });
    // The source's word, with its capital letter and its spacing. A rule maps
    // it to the owner's category by exact value; normalising it here would
    // silently stop that rule matching.
    operation.source_category = Some("Супермаркеты".to_owned());
    let event = normalize(&operation, context()).expect("normalises").event;
    assert_eq!(event.provenance.source_category(), Some("Супермаркеты"));
}

#[test]
fn an_operation_without_a_source_category_carries_none() {
    let operation = submit(OperationKind::Withdrawal {
        amount_minor: 120_000,
        currency: CurrencyCode::Rub,
    });
    let event = normalize(&operation, context()).expect("normalises").event;
    assert_eq!(event.provenance.source_category(), None);
}
