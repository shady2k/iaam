//! The intake shape that can say "I don't know" (iaam-6qsa).
//!
//! The row this file is written around is invented: an amount, a word meaning
//! "internal to this institution", and nothing else.

use iaam_core::event::kind::FeeOrigin;
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
fn a_counterparty_the_directory_recognises_settles_the_row_with_no_question() {
    // This is the seam `Counterparty::OwnAccount` was written for and that
    // nothing reached: a recognised counterparty is a derived internal transfer.
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
fn the_far_side_of_an_internal_transfer_says_which_way_the_money_went() {
    // `InternalTransfer { to }` names the far side of the movement, whichever
    // way it went. Read against this row's own account, that is a direction: a
    // received transfer is submitted from the account it left.
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
    assert!(row.resolve(Classification::Income, Movement::Out).is_err());
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
        !outflow.admits(&Answer::Income),
        "a question about an outflow does not admit income"
    );
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
        Answer::Income,
    ] {
        let classification = answer.classification();
        let movement = answer.movement();
        match (classification, movement) {
            (Classification::Fee { .. }, Movement::Out)
            | (Classification::Income, Movement::In)
            | (Classification::ExternalFlow, _)
            | (Classification::InternalTransfer { .. }, _) => {}
            other => panic!("{answer:?} names a contradiction: {other:?}"),
        }
        assert_eq!(answer.shape().code(), answer.shape().code());
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
