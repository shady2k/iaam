//! The intake shape that can say "I don't know" (iaam-6qsa).
//!
//! The row this file is written around is invented: an amount, a word meaning
//! "internal to this institution", and nothing else.

use iaam_core::event::kind::{FeeOrigin, FlowEndpoints, IncomeKind};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, ClassificationRuleId, OwnerId, SourceId};
use iaam_core::money::CurrencyCode;
use iaam_ingest::classification::{
    Answer, AnswerShape, Classification, ClassificationResult, ClassificationRule, Counterparty,
    FarSide, Movement, Question, RuleMatcher, classify,
};
use iaam_ingest::normalize;
use iaam_ingest::observation::{ObservedCounterparty, ObservedDirection, ObservedRow, RowIdentity};
use iaam_ingest::operation::{NormalizationContext, OperationDates, OperationKind, PARSER_VERSION};
use time::macros::date;

fn inner_row(account: AccountId) -> ObservedRow {
    ObservedRow {
        account,
        direction: ObservedDirection::Inner,
        amount_minor: 250_000,
        currency: CurrencyCode::Rub,
        counterparty: ObservedCounterparty::Unknown,
        far_side: FarSide::Unstated,
        source_kind: Some("INNER".to_owned()),
        source_category: None,
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
            Some(Movement::Out),
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
            Some(Movement::In),
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
            Some(Movement::In)
        )
        .is_err()
    );
    assert!(
        row.resolve(Classification::Income { kind: None }, Some(Movement::Out))
            .is_err()
    );
    // A refund that left is the same contradiction, and unlike the other two it
    // is reachable — not from an answer, both of which say the money arrived,
    // but from a rule, which carries no direction and matches a merchant's
    // purchases as readily as its returns.
    assert!(
        row.resolve(Classification::Refund, Some(Movement::Out))
            .is_err(),
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
            Some(Movement::Out)
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
        row.resolve(Classification::ExternalFlow, Some(Movement::In))
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
        far_side: FarSide::Unstated,
        source_kind: Some("RETURN".to_owned()),
        source_category: None,
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

    let cases: [(Classification, Movement, &str); 8] = [
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
        (
            Classification::OwnAccountMovement,
            Movement::Out,
            "own_account_movement",
        ),
    ];

    for (classification, movement, expected) in cases {
        let operation = row
            .resolve(classification, Some(movement))
            .unwrap_or_else(|error| panic!("{classification:?} + {movement:?}: {error:?}"));
        let actual = match operation.kind {
            OperationKind::Deposit { .. } => "deposit",
            OperationKind::Withdrawal { .. } => "withdrawal",
            OperationKind::Transfer { .. } => "transfer",
            OperationKind::Fee { .. } => "fee",
            OperationKind::Income { .. } => "income",
            OperationKind::Refund { .. } => "refund",
            OperationKind::OwnAccountMovement { .. } => "own_account_movement",
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

// ---------------------------------------------------------------------------
// The source's two words, kept apart (iaam-p683)
// ---------------------------------------------------------------------------

#[test]
fn the_operation_word_and_the_source_category_survive_resolution_in_their_own_fields() {
    // The defect: `envelope` filled the operation's `source_category` from the
    // row's `source_kind`, so an observed row could not carry a source's
    // category at all, and the owner's category rules — which match
    // `source_category` — matched an operation word instead.
    let account = AccountId::new_random();
    let row = ObservedRow {
        direction: ObservedDirection::Out,
        source_kind: Some("card payment".to_owned()),
        source_category: Some("Groceries".to_owned()),
        ..inner_row(account)
    };

    let operation = row
        .resolve(Classification::ExternalFlow, Some(Movement::Out))
        .expect("a stated outflow resolves");

    assert_eq!(
        operation.source_kind.as_deref(),
        Some("card payment"),
        "the word the source used for what the operation was"
    );
    assert_eq!(
        operation.source_category.as_deref(),
        Some("Groceries"),
        "the word the source used for what it was for"
    );
}

#[test]
fn a_row_whose_source_printed_no_category_carries_none_rather_than_its_operation_word() {
    // `None` here is «the source printed no category». Filling it from
    // `source_kind` is the transcription this pair was written to stop: it
    // states, as the source's category, a word the source used for something
    // else.
    let account = AccountId::new_random();
    let row = ObservedRow {
        direction: ObservedDirection::Out,
        ..inner_row(account)
    };
    assert_eq!(row.source_kind.as_deref(), Some("INNER"));

    let operation = row
        .resolve(Classification::ExternalFlow, Some(Movement::Out))
        .expect("a stated outflow resolves");

    assert_eq!(operation.source_category, None);
    assert_eq!(operation.source_kind.as_deref(), Some("INNER"));
}

#[test]
fn both_words_survive_a_transfer_submitted_from_the_far_side() {
    // The one arm of `resolve` that rebuilds the envelope against another
    // account. It is where a field added to the envelope is most easily
    // dropped, and the evidence belongs to the row either way.
    let account = AccountId::new_random();
    let far = AccountId::new_random();
    let row = ObservedRow {
        direction: ObservedDirection::In,
        source_kind: Some("transfer".to_owned()),
        source_category: Some("Between own accounts".to_owned()),
        ..inner_row(account)
    };

    let operation = row
        .resolve(
            Classification::InternalTransfer { to: far },
            Some(Movement::In),
        )
        .expect("an incoming internal transfer resolves");

    assert_eq!(
        operation.account, far,
        "an arriving transfer is filed from the sending side"
    );
    assert_eq!(operation.source_kind.as_deref(), Some("transfer"));
    assert_eq!(
        operation.source_category.as_deref(),
        Some("Between own accounts")
    );
}

#[test]
fn a_stored_row_written_before_the_category_existed_reads_back_without_one() {
    // An import session parks its rows as JSON, and a session opened by an
    // earlier build holds rows with no `source_category` key at all. Such a row
    // must still read back — the session outlives the request that opened it —
    // and it must read back as «the source said nothing», not borrow the
    // operation word beside it.
    //
    // The stored shape is produced by serialising and then dropping the key,
    // rather than typed out here: a literal would fix this crate's date
    // encoding into a test that is not about dates.
    let account = AccountId::new_random();
    let mut stored = serde_json::to_value(inner_row(account)).expect("a row serialises");
    let object = stored.as_object_mut().expect("a row is an object");
    assert!(
        object.remove("source_category").is_some(),
        "the field this test is about must be in the written shape"
    );

    let row: ObservedRow =
        serde_json::from_value(stored).expect("a row stored by an earlier build still reads");

    assert_eq!(row.source_category, None);
    assert_eq!(row.source_kind.as_deref(), Some("INNER"));
}

// ---------------------------------------------------------------------------
// What each alternative does to the money (iaam-pzm9)
// ---------------------------------------------------------------------------

/// The journal fact one answer produces, as the money-flow projection sees it.
///
/// The whole chain in one call, because the chain is the claim:
/// [`Answer::classification`] and [`Answer::movement`] together decide the
/// operation, `normalize` decides the event, and `MoneyFlow::absorb` matches on
/// the event's kind to choose which of its seven quantities the amount lands in.
fn recorded_as(answer: Answer, account: AccountId) -> (String, FlowEndpoints) {
    let operation = inner_row(account)
        .resolve_with(answer)
        .expect("the answer names a direction its classification admits");
    let event = normalize(
        &operation,
        &NormalizationContext {
            owner: OwnerId::new_random(),
            source: SourceId::new_random(),
            parser_version: ParserVersion(PARSER_VERSION.to_owned()),
        },
    )
    .expect("a dated cash row normalises")
    .event;
    (
        event.kind.discriminant().to_owned(),
        event.kind.flow_endpoints(),
    )
}

/// `AnswerShape::consequence` claims what each answer does to the report. This
/// pins the claim to the code that decides it.
///
/// The sentence cannot run the projection — a question is asked before there is
/// a journal, a contour or a category index — so what it says is checked here
/// instead: the event kind is what `MoneyFlow::absorb` matches on, and
/// `flow_endpoints` is what decides whether the cash crossed the boundary.
#[test]
fn each_answer_produces_the_journal_fact_its_consequence_claims() {
    let account = AccountId::new_random();
    let far = AccountId::new_random();

    assert_eq!(
        recorded_as(Answer::SentToOwnAccount { to: far }, account),
        (
            "cash_transfer".to_owned(),
            FlowEndpoints::BetweenAccounts {
                from: account,
                to: far,
            }
        ),
        "«between your own accounts» is a transfer with a leg on each"
    );
    assert_eq!(
        recorded_as(Answer::ReceivedFromOwnAccount { from: far }, account),
        (
            "cash_transfer".to_owned(),
            FlowEndpoints::BetweenAccounts {
                from: far,
                to: account,
            }
        ),
        "the same fact from the sending side, which is where a transfer is \
         recorded from"
    );
    assert_eq!(
        recorded_as(Answer::Paid, account).0,
        "cash_out",
        "«money that went out» is the outflow the category rules decompose"
    );
    assert_eq!(
        recorded_as(Answer::Received, account),
        ("cash_in".to_owned(), FlowEndpoints::InboundFromOutside),
        "«money that came in» is money crossing the boundary inward"
    );
    assert_eq!(
        recorded_as(
            Answer::Fee {
                origin: FeeOrigin::AccountMaintenance
            },
            account
        )
        .0,
        "fee",
        "a fee is its own kind, so it lands under fees and not under spending"
    );
    assert_eq!(
        recorded_as(Answer::Income { kind: None }, account).0,
        "income",
        "an earning is its own kind, so it lands under what the capital earned"
    );
    assert_eq!(
        recorded_as(Answer::Refund, account).0,
        "refund",
        "a return is its own kind, which is what lets the report subtract it \
         from what went out instead of adding it to what came in"
    );
}

/// The pair the owner actually got wrong, and the reason the sentence had to be
/// published (iaam-pzm9).
///
/// «Money came in from outside» and «my own money came back» are one word apart
/// in the answer vocabulary and are not neighbouring shades of one fact: one
/// crosses the boundary inward and one does not move across it at all. Chosen
/// from a question that never mentioned the difference, the wrong one turns a
/// movement between the owner's own accounts into an inflow for the year.
#[test]
fn arriving_from_outside_and_arriving_from_your_own_account_are_different_facts() {
    let account = AccountId::new_random();
    let far = AccountId::new_random();

    let outside = recorded_as(Answer::Received, account);
    let own = recorded_as(Answer::ReceivedFromOwnAccount { from: far }, account);

    assert_ne!(
        outside, own,
        "if these two produced the same fact the question would not need asking"
    );
    assert_eq!(outside.1, FlowEndpoints::InboundFromOutside);
    assert!(
        matches!(own.1, FlowEndpoints::BetweenAccounts { .. }),
        "{own:?}"
    );
    assert_ne!(
        AnswerShape::Received.consequence(),
        AnswerShape::ReceivedFromOwnAccount.consequence(),
        "and the words the owner reads must not be the same either"
    );
}

/// No two alternatives of one question read alike.
///
/// A consequence that repeated across two answers would be worse than none: it
/// would state, in the place the owner looks to tell them apart, that there is
/// nothing to tell apart.
#[test]
fn no_two_alternatives_of_a_question_say_the_same_thing() {
    let question = Question::UnresolvedDirection {
        account: AccountId::new_random(),
        stated: Some("INNER".to_owned()),
        counterparty: None,
    };
    let said: Vec<&str> = question
        .alternatives()
        .into_iter()
        .map(AnswerShape::consequence)
        .collect();

    assert!(
        said.iter().all(|text| !text.is_empty()),
        "every alternative decides something: {said:?}"
    );
    let distinct: std::collections::BTreeSet<&&str> = said.iter().collect();
    assert_eq!(distinct.len(), said.len(), "{said:?}");
}

// --- the two journal shapes an unnamed own account produces (iaam-fmih) -----

/// The row this whole wave is about: an amount, a date, the source's own word
/// for a movement between the owner's accounts, no direction, nobody named.
fn own_account_row(account: AccountId) -> ObservedRow {
    ObservedRow {
        far_side: FarSide::OwnAccount,
        ..inner_row(account)
    }
}

#[test]
fn a_directionless_own_account_movement_resolves_without_a_direction() {
    let account = AccountId::new_random();
    let operation = own_account_row(account)
        .resolve(Classification::OwnAccountMovement, None)
        .expect("the one classification that survives an absent direction");
    assert_eq!(operation.account, account);
    assert!(matches!(
        operation.kind,
        OperationKind::OwnAccountMovement {
            movement: None,
            amount_minor: 250_000,
            ..
        }
    ));
}

#[test]
fn every_other_classification_still_needs_a_direction() {
    // The refusal is the whole reason `UnresolvedDirection` is still asked:
    // «the source printed a word for it» is not «the source said which way it
    // went», and only one outcome can be recorded without the second.
    let account = AccountId::new_random();
    let row = own_account_row(account);
    for classification in [
        Classification::ExternalFlow,
        Classification::Refund,
        Classification::Income { kind: None },
        Classification::Fee {
            origin: FeeOrigin::Other,
        },
        Classification::InternalTransfer {
            to: AccountId::new_random(),
        },
    ] {
        assert!(
            row.resolve(classification, None).is_err(),
            "{classification:?} cannot be recorded without a direction"
        );
    }
}

#[test]
fn a_stated_direction_makes_the_same_row_a_posted_movement() {
    let account = AccountId::new_random();
    let row = ObservedRow {
        direction: ObservedDirection::Out,
        ..own_account_row(account)
    };
    let operation = row
        .resolve(Classification::OwnAccountMovement, Some(Movement::Out))
        .expect("a stated direction resolves");
    assert!(matches!(
        operation.kind,
        OperationKind::OwnAccountMovement {
            movement: Some(Movement::Out),
            ..
        }
    ));
}

#[test]
fn the_assertion_reaches_the_classifier_through_the_subject() {
    // The seam: the value the caller stated has to arrive where the decision is
    // made, or the whole shape is a field nobody reads.
    let account = AccountId::new_random();
    assert_eq!(
        own_account_row(account).subject(None).far_side,
        FarSide::OwnAccount
    );
    assert_eq!(
        inner_row(account).subject(None).far_side,
        FarSide::Unstated,
        "a row that asserted nothing carries nothing"
    );
}

#[test]
fn a_row_stored_before_the_assertion_existed_reads_as_stating_nothing() {
    // The session keeps rows as JSON, so a session opened by an older build
    // must still parse — and what such a row meant is «the source said nothing
    // about the far side», which is `Unstated` and not «somebody else's».
    let account = AccountId::new_random();
    let mut value = serde_json::to_value(inner_row(account)).expect("serialises");
    value
        .as_object_mut()
        .expect("a row is an object")
        .remove("far_side");
    let restored: ObservedRow = serde_json::from_value(value).expect("an older row still parses");
    assert_eq!(restored.far_side, FarSide::Unstated);
}
