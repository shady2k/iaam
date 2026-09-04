//! The intake shape that can say "I don't know" (iaam-6qsa).
//!
//! The row this file is written around is invented: an amount, a word meaning
//! "internal to this institution", and nothing else.

use iaam_core::event::kind::{FeeOrigin, IncomeKind};
use iaam_core::ids::{AccountId, ClassificationRuleId};
use iaam_core::money::CurrencyCode;
use iaam_ingest::classification::{
    Answer, AnswerShape, Classification, ClassificationResult, ClassificationRule, Counterparty,
    Movement, Question, RuleMatcher, classify,
};
use iaam_ingest::observation::{ObservedCounterparty, ObservedDirection, ObservedRow, RowIdentity};
use iaam_ingest::operation::{OperationDates, OperationKind};
use time::macros::date;

fn inner_row(account: AccountId) -> ObservedRow {
    ObservedRow {
        account,
        direction: ObservedDirection::Inner,
        amount_minor: 250_000,
        currency: CurrencyCode::Rub,
        counterparty: ObservedCounterparty::Unknown,
        source_kind: Some("INNER".to_owned()),
        description: None,
        dates: OperationDates {
            cash_posted: Some(date!(2025 - 03 - 18)),
            ..OperationDates::default()
        },
        source_time: None,
        identity: RowIdentity {
            document: None,
            row: None,
            idempotency_key: Some("inner-one".to_owned()),
        },
    }
}

#[test]
fn a_row_with_no_direction_is_asked_about_rather_than_guessed() {
    // The three questions that existed all assume a settled direction: two
    // branch on the movement to be asked at all, and the third would take "is
    // this your own account?" as settling a row whose direction is still open.
    let account = AccountId::new_random();
    let row = inner_row(account);
    assert_eq!(row.movement(), None, "«inner» resolves to no direction");

    let result = classify(&row.subject(None), &[]);
    let ClassificationResult::Ambiguous { question } = result else {
        panic!("a row with no direction must not resolve: {result:?}");
    };
    assert_eq!(
        question,
        Question::UnresolvedDirection {
            account,
            stated: Some("INNER".to_owned()),
            counterparty: None,
        }
    );
    // And it offers both directions, which is what makes it answerable.
    let alternatives = question.alternatives();
    assert!(alternatives.contains(&AnswerShape::SentToOwnAccount));
    assert!(alternatives.contains(&AnswerShape::ReceivedFromOwnAccount));
}

#[test]
fn a_counterparty_the_directory_recognises_settles_what_the_row_is() {
    // This is the seam `Counterparty::OwnAccount` was written for and that
    // nothing reached: a recognised counterparty is a derived internal transfer.
    //
    // It settles **what** the row is and not which way it ran. The row below
    // states no direction, and recognising the counterparty does not supply one.
    let account = AccountId::new_random();
    let savings = AccountId::new_random();
    let row = ObservedRow {
        counterparty: ObservedCounterparty::Named("Savings".to_owned()),
        ..inner_row(account)
    };

    let result = classify(&row.subject(Some(savings)), &[]);
    assert_eq!(
        result,
        ClassificationResult::Resolved {
            classification: Classification::InternalTransfer { to: savings },
            basis: iaam_ingest::classification::Basis::Derived,
        }
    );
}

#[test]
fn a_rule_the_owner_already_wrote_settles_the_row_without_asking_again() {
    // The durable half of the answer: the owner decides once, and the next
    // import of the same shape is not put to them a second time.
    let account = AccountId::new_random();
    let savings = AccountId::new_random();
    let row = inner_row(account);
    let rule = ClassificationRule {
        id: ClassificationRuleId::new_random(),
        version: 1,
        matcher: RuleMatcher {
            counterparty_account: None,
            description_contains: None,
            kind: Some("INNER".to_owned()),
        },
        outcome: Classification::InternalTransfer { to: savings },
    };

    let ClassificationResult::Resolved { classification, .. } =
        classify(&row.subject(None), std::slice::from_ref(&rule))
    else {
        panic!("a matching rule must settle the row");
    };
    assert_eq!(
        classification,
        Classification::InternalTransfer { to: savings }
    );
}

#[test]
fn the_far_side_of_an_internal_transfer_is_read_against_the_rows_own_account() {
    // `InternalTransfer { to }` names the far side of the movement, whichever
    // way it went — so the direction has to be supplied, and is a separate
    // argument here. Given one, the far side says which account the operation is
    // submitted from: a received transfer is submitted from the account it left.
    let account = AccountId::new_random();
    let savings = AccountId::new_random();
    let row = inner_row(account);

    let sent = row
        .resolve(
            Classification::InternalTransfer { to: savings },
            Movement::Out,
        )
        .expect("an outgoing internal transfer resolves");
    assert_eq!(sent.account, account);
    assert!(matches!(
        sent.kind,
        OperationKind::Transfer { to, .. } if to == savings
    ));

    let received = row
        .resolve(
            Classification::InternalTransfer { to: savings },
            Movement::In,
        )
        .expect("an incoming internal transfer resolves");
    assert_eq!(
        received.account, savings,
        "a transfer is submitted from the account it left"
    );
    assert!(matches!(
        received.kind,
        OperationKind::Transfer { to, .. } if to == account
    ));
}

#[test]
fn an_internal_transfer_states_no_direction_of_its_own() {
    // The account an internal transfer names is the **far side**, and a far
    // side is not a direction. Reading `to != row.account` as "the money left"
    // was wrong in both directions at once, because
    // `Answer::ReceivedFromOwnAccount { from }` records the far side in that
    // same field for money that arrived.
    //
    // Three of the five outcomes do state a direction, and they state it
    // because the classification *is* the direction: a fee leaves, income
    // arrives, and a refund is money coming back.
    let savings = AccountId::new_random();
    assert_eq!(
        Classification::InternalTransfer { to: savings }.implied_movement(),
        None,
        "the far side is not a direction"
    );
    assert_eq!(
        Classification::ExternalFlow.implied_movement(),
        None,
        "money crossing the perimeter can cross it either way"
    );
    assert_eq!(
        Classification::Fee {
            origin: FeeOrigin::Brokerage
        }
        .implied_movement(),
        Some(Movement::Out)
    );
    assert_eq!(
        Classification::Income { kind: None }.implied_movement(),
        Some(Movement::In)
    );
    assert_eq!(
        Classification::Income {
            kind: Some(IncomeKind::DepositInterest)
        }
        .implied_movement(),
        Some(Movement::In),
        "naming the earning does not change which way it came"
    );
    assert_eq!(
        Classification::Refund.implied_movement(),
        Some(Movement::In),
        "the journal holds no refund that left the account"
    );
}

#[test]
fn the_two_own_account_answers_differ_by_direction_and_not_by_far_side() {
    // Why `Answer` is left alone. The direction is already structural — two
    // variants, and `movement()` is total over them. What both collapse into is
    // the *rule* vocabulary, and a rule must carry no direction: it will fire on
    // rows the owner has never seen, and a replayed direction is the same guess
    // in new clothing.
    let far = AccountId::new_random();
    let sent = Answer::SentToOwnAccount { to: far };
    let received = Answer::ReceivedFromOwnAccount { from: far };

    assert_eq!(sent.movement(), Movement::Out);
    assert_eq!(received.movement(), Movement::In);
    assert_eq!(
        sent.classification(),
        received.classification(),
        "the same pair of accounts, and the rule that recognises them again          says nothing about which way the next row runs"
    );
    assert_eq!(
        sent.classification().implied_movement(),
        None,
        "so nothing downstream can recover a direction from the rule alone"
    );
}

#[test]
fn an_answer_whose_direction_contradicts_what_it_names_is_refused() {
    // A fee that arrived and income that left are not rows this system can
    // record, and refusing is the only answer that writes nothing nobody said.
    let account = AccountId::new_random();
    let row = inner_row(account);

    assert!(
        row.resolve(
            Classification::Fee {
                origin: FeeOrigin::Other
            },
            Movement::In
        )
        .is_err()
    );
    assert!(
        row.resolve(Classification::Income { kind: None }, Movement::Out)
            .is_err()
    );
    // A refund that left is the same contradiction, and unlike the other two it
    // is reachable — not from an answer, both of which say the money arrived,
    // but from a rule, which carries no direction and matches a merchant's
    // purchases as readily as its returns.
    assert!(
        row.resolve(Classification::Refund, Movement::Out).is_err(),
        "a refund is money coming back, so one that left is not a fact"
    );
}

#[test]
fn an_internal_transfer_to_this_very_account_is_refused() {
    let account = AccountId::new_random();
    let row = inner_row(account);
    assert!(
        row.resolve(
            Classification::InternalTransfer { to: account },
            Movement::Out
        )
        .is_err(),
        "a transfer to itself is not a movement"
    );
}

#[test]
fn a_row_stating_no_amount_is_refused_rather_than_recorded_as_zero() {
    let account = AccountId::new_random();
    let row = ObservedRow {
        amount_minor: 0,
        ..inner_row(account)
    };
    assert!(
        row.resolve(Classification::ExternalFlow, Movement::In)
            .is_err(),
        "a row that states no movement is not a movement of zero"
    );
}

#[test]
fn the_sign_the_source_printed_survives_intake() {
    // Making the amount positive would discard the source's own statement about
    // direction, which is the evidence this shape exists to carry.
    let account = AccountId::new_random();
    let row = ObservedRow {
        direction: ObservedDirection::Out,
        amount_minor: -250_000,
        ..inner_row(account)
    };
    assert_eq!(row.amount_minor, -250_000);
    assert_eq!(row.movement(), Some(Movement::Out));

    let resolved = row
        .resolve_with(Answer::Paid)
        .expect("a stated outflow resolves");
    assert!(matches!(
        resolved.kind,
        OperationKind::Withdrawal { amount_minor, .. } if amount_minor == 250_000
    ));
}

#[test]
fn a_direction_word_the_caller_invented_is_refused() {
    // A caller that meant "out" and typed "outgoing" must be told, not silently
    // asked a question it had already answered.
    assert_eq!(
        ObservedDirection::parse("inner").expect("a known word"),
        ObservedDirection::Inner
    );
    let rejection = ObservedDirection::parse("outgoing").expect_err("an invented word");
    assert_eq!(rejection.field, "direction");
    assert_eq!(rejection.actual, "outgoing");
}

#[test]
fn a_question_admits_only_the_answers_it_published() {
    let account = AccountId::new_random();
    let outflow = Question::IsOutflowAFee { account };
    assert!(outflow.admits(&Answer::Fee {
        origin: FeeOrigin::Other
    }));
    assert!(outflow.admits(&Answer::Paid));
    assert!(
        !outflow.admits(&Answer::Income { kind: None }),
        "a question about an outflow does not admit income"
    );
    assert!(
        !outflow.admits(&Answer::Refund),
        "a question whose alternatives both leave the account admits no arrival"
    );
    for question in [
        Question::IsInflowIncome { account },
        Question::IsTransferInternal {
            account,
            counterparty: "Shop One".to_owned(),
        },
        Question::UnresolvedDirection {
            account,
            stated: None,
            counterparty: None,
        },
    ] {
        assert!(
            question.admits(&Answer::Refund),
            "{question:?} leaves an arrival open, so it must offer a refund"
        );
    }
    for shape in outflow.alternatives() {
        assert!(
            !shape.needs_account(),
            "an outflow question names no account: {shape:?}"
        );
    }
    assert!(AnswerShape::SentToOwnAccount.needs_account());
}

#[test]
fn every_answer_names_a_direction_and_a_classification() {
    // A directionless row needs both, and one answer is the only way to give
    // both without something provisional existing in between.
    let to = AccountId::new_random();
    for answer in [
        Answer::SentToOwnAccount { to },
        Answer::ReceivedFromOwnAccount { from: to },
        Answer::Paid,
        Answer::Received,
        Answer::Fee {
            origin: FeeOrigin::Brokerage,
        },
        Answer::Income { kind: None },
        Answer::Income {
            kind: Some(IncomeKind::DepositInterest),
        },
        Answer::Refund,
    ] {
        let classification = answer.classification();
        let movement = answer.movement();
        match (classification, movement) {
            (Classification::Fee { .. }, Movement::Out)
            | (Classification::Income { .. } | Classification::Refund, Movement::In)
            | (Classification::ExternalFlow, _)
            | (Classification::InternalTransfer { .. }, _) => {}
            other => panic!("{answer:?} names a contradiction: {other:?}"),
        }
        assert_eq!(answer.shape().code(), answer.shape().code());
    }
}

/// A row a merchant returned money on, as a source prints one.
///
/// Invented: `Shop One` is nobody. The source states the direction, because a
/// bank prints one for a card return; what no source states is that the money is
/// coming back rather than arriving, which is the thing the owner is asked.
fn merchant_inflow(account: AccountId) -> ObservedRow {
    ObservedRow {
        account,
        direction: ObservedDirection::In,
        amount_minor: 125_000,
        currency: CurrencyCode::Rub,
        counterparty: ObservedCounterparty::Named("Shop One".to_owned()),
        source_kind: Some("RETURN".to_owned()),
        description: None,
        dates: OperationDates {
            cash_posted: Some(date!(2025 - 03 - 20)),
            ..OperationDates::default()
        },
        source_time: None,
        identity: RowIdentity {
            document: None,
            row: None,
            idempotency_key: Some("refund-one".to_owned()),
        },
    }
}

#[test]
fn an_observed_row_the_owner_calls_a_refund_becomes_one() {
    // The parity defect of `iaam-7l7v`, at the seam it lived on. A caller that
    // concluded could send `refund`; a caller that observed could reach four
    // outcomes, none of them a return, so the same row submitted honestly came
    // out as a deposit — and the journal keeps the two apart, subtracting a
    // refund from what went out in the category it was spent in.
    let account = AccountId::new_random();
    let row = merchant_inflow(account);

    let question = match classify(&row.subject(None), &[]) {
        ClassificationResult::Ambiguous { question } => question,
        other => panic!("a merchant the directory does not know is a question: {other:?}"),
    };
    assert!(
        question.admits(&Answer::Refund),
        "money arriving from a named counterparty is the shape of a return: {question:?}"
    );

    let operation = row
        .resolve_with(Answer::Refund)
        .expect("a refund the owner named resolves");
    assert_eq!(operation.account, account);
    assert!(
        matches!(
            operation.kind,
            OperationKind::Refund {
                amount_minor: 125_000,
                ..
            }
        ),
        "{:?}",
        operation.kind
    );
}

#[test]
fn the_owners_answer_is_the_only_thing_that_can_name_an_earning() {
    // The second half of `iaam-7l7v`. An observation resolved as income used to
    // carry no kind at all, on the correct ground that the source named none —
    // and the ground stays correct: what changed is that the owner can now name
    // one, and that his naming travels into the rule, because it is a claim
    // about every row the matcher matches rather than a fact about this row.
    let account = AccountId::new_random();
    let row = merchant_inflow(account);

    let unnamed = row
        .resolve_with(Answer::Income { kind: None })
        .expect("income the owner named no kind for");
    assert!(
        matches!(unnamed.kind, OperationKind::Income { kind: None, .. }),
        "silence is recorded as silence (§4.9): {:?}",
        unnamed.kind
    );

    let named = row
        .resolve_with(Answer::Income {
            kind: Some(IncomeKind::DepositInterest),
        })
        .expect("income the owner named");
    assert!(
        matches!(
            named.kind,
            OperationKind::Income {
                kind: Some(IncomeKind::DepositInterest),
                instrument: None,
                ..
            }
        ),
        "{:?}",
        named.kind
    );
    assert_eq!(
        Answer::Income {
            kind: Some(IncomeKind::DepositInterest)
        }
        .classification(),
        Classification::Income {
            kind: Some(IncomeKind::DepositInterest)
        },
        "the kind must reach the rule vocabulary, or the next statement asks again"
    );
}

#[test]
fn every_conclusion_a_cash_row_can_be_is_reachable_from_an_observation() {
    // The parity list, asserted rather than described. The left column is what
    // an `OperationKind` can say about a statement row of cash; the right is
    // what the observation channel reaches. Buying, selling, an opening balance
    // or a valuation are absent from both, because an observed row carries no
    // instrument, quantity or price for them to be built from — that is a
    // difference in what the shape states, not a channel that concludes less.
    //
    // `Tax` is the one genuine survivor, and it is deliberate rather than
    // forgotten: `classification_of` answers `None` for a recorded tax, so tax
    // sits outside rule recalculation entirely, and a fifth outcome for it would
    // overturn that in passing.
    let account = AccountId::new_random();
    let far = AccountId::new_random();
    let row = merchant_inflow(account);

    let cases: [(Classification, Movement, &str); 7] = [
        (Classification::ExternalFlow, Movement::In, "deposit"),
        (Classification::ExternalFlow, Movement::Out, "withdrawal"),
        (
            Classification::InternalTransfer { to: far },
            Movement::Out,
            "transfer",
        ),
        (
            Classification::InternalTransfer { to: far },
            Movement::In,
            "transfer",
        ),
        (
            Classification::Fee {
                origin: FeeOrigin::AccountMaintenance,
            },
            Movement::Out,
            "fee",
        ),
        (
            Classification::Income {
                kind: Some(IncomeKind::DepositInterest),
            },
            Movement::In,
            "income",
        ),
        (Classification::Refund, Movement::In, "refund"),
    ];

    for (classification, movement, expected) in cases {
        let operation = row
            .resolve(classification, movement)
            .unwrap_or_else(|error| panic!("{classification:?} + {movement:?}: {error:?}"));
        let actual = match operation.kind {
            OperationKind::Deposit { .. } => "deposit",
            OperationKind::Withdrawal { .. } => "withdrawal",
            OperationKind::Transfer { .. } => "transfer",
            OperationKind::Fee { .. } => "fee",
            OperationKind::Income { .. } => "income",
            OperationKind::Refund { .. } => "refund",
            other => panic!("an observation must not become {other:?}"),
        };
        assert_eq!(actual, expected, "{classification:?} + {movement:?}");
    }
}

#[test]
fn a_row_identity_without_anything_stable_says_so() {
    // `None` is honest rather than convenient: a row with no stable key would
    // otherwise open a second question about the same money on re-submission.
    assert_eq!(RowIdentity::default().key(), None);
    assert_eq!(
        RowIdentity {
            document: Some("march.csv".to_owned()),
            row: Some("17".to_owned()),
            idempotency_key: None,
        }
        .key()
        .as_deref(),
        Some("document/march.csv/17")
    );
}

#[test]
fn a_named_counterparty_the_directory_does_not_know_narrows_the_question() {
    // Not recognising a name is not the same as the source naming nobody, and
    // the question keeps the difference.
    let account = AccountId::new_random();
    let row = ObservedRow {
        counterparty: ObservedCounterparty::Named("Shop One".to_owned()),
        ..inner_row(account)
    };
    let subject = row.subject(None);
    assert_eq!(
        subject.counterparty,
        Counterparty::Named("Shop One".to_owned())
    );

    let ClassificationResult::Ambiguous { question } = classify(&subject, &[]) else {
        panic!("an unrecognised counterparty with no direction cannot resolve");
    };
    assert_eq!(
        question,
        Question::UnresolvedDirection {
            account,
            stated: Some("INNER".to_owned()),
            counterparty: Some("Shop One".to_owned()),
        }
    );
}
