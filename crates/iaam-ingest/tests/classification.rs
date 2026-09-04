//! Classification rules and history recalculation (§10.4).

use std::collections::BTreeMap;

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::{EventKind, FeeOrigin, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, CustodyId, EventId, InstrumentId, OwnerId, SourceId,
    TransferId,
};
use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_ingest::classification::{
    Basis, Classification, ClassificationResult, ClassificationRule, ClassificationSubject,
    Correction, CorrectionStep, Counterparty, FarSide, Movement, Question, RuleMatcher,
    classification_of, classify, recompute_plan,
};
use rust_decimal::Decimal;
use time::macros::date;

fn matcher(
    counterparty: Option<&str>,
    description: Option<&str>,
    kind: Option<&str>,
) -> RuleMatcher {
    RuleMatcher {
        counterparty_account: counterparty.map(str::to_owned),
        description_contains: description.map(str::to_owned),
        kind: kind.map(str::to_owned),
        source_category: None,
    }
}

/// A condition asking only about the category the source filed the row under.
fn filed_under(category: &str) -> RuleMatcher {
    RuleMatcher {
        counterparty_account: None,
        description_contains: None,
        kind: None,
        source_category: Some(category.to_owned()),
    }
}

fn rule(version: u32, matcher: RuleMatcher, outcome: Classification) -> ClassificationRule {
    ClassificationRule {
        id: ClassificationRuleId::new_random(),
        version,
        matcher,
        outcome,
    }
}

fn transfer_to(name: &str, account: AccountId) -> ClassificationSubject {
    ClassificationSubject {
        account,
        counterparty: Counterparty::Named(name.to_owned()),
        description: Some("Перевод по номеру счёта".to_owned()),
        source_kind: Some("Перевод".to_owned()),
        source_category: None,
        movement: Some(Movement::Out),
        far_side: FarSide::Unstated,
    }
}

#[test]
fn a_transfer_to_an_own_account_needs_no_rule() {
    // The rule is needed where data is missing. Asking about what is
    // already known means making the owner do pointless work.
    let mine = AccountId::new_random();
    let other = AccountId::new_random();
    let subject = ClassificationSubject {
        account: mine,
        counterparty: Counterparty::OwnAccount(other),
        description: None,
        source_kind: None,
        source_category: None,
        movement: Some(Movement::Out),
        far_side: FarSide::Unstated,
    };

    assert_eq!(
        classify(&subject, &[]),
        ClassificationResult::Resolved {
            classification: Classification::InternalTransfer { to: other },
            basis: Basis::Derived,
        }
    );
}

#[test]
fn a_transfer_to_an_unknown_counterparty_asks_the_owner() {
    let mine = AccountId::new_random();
    let subject = transfer_to("40817810099910004312", mine);

    assert_eq!(
        classify(&subject, &[]),
        ClassificationResult::Ambiguous {
            question: Question::IsTransferInternal {
                account: mine,
                counterparty: "40817810099910004312".to_owned(),
            },
        }
    );
}

#[test]
fn the_owners_answer_becomes_a_rule_and_the_question_stops() {
    let mine = AccountId::new_random();
    let theirs = AccountId::new_random();
    let subject = transfer_to("40817810099910004312", mine);
    let answered = rule(
        1,
        matcher(Some("40817810099910004312"), None, None),
        Classification::InternalTransfer { to: theirs },
    );

    assert_eq!(
        classify(&subject, std::slice::from_ref(&answered)),
        ClassificationResult::Resolved {
            classification: Classification::InternalTransfer { to: theirs },
            basis: Basis::Rule {
                rule: answered.id,
                version: 1,
            },
        }
    );
}

#[test]
fn the_newest_matching_rule_wins() {
    // Editing a rule creates a new version: the higher number represents the
    // owner's latest decision, not one of two equally valid alternatives.
    let mine = AccountId::new_random();
    let subject = transfer_to("40817810099910004312", mine);
    let old = rule(
        1,
        matcher(Some("40817810099910004312"), None, None),
        Classification::InternalTransfer {
            to: AccountId::new_random(),
        },
    );
    let new = rule(
        2,
        matcher(Some("40817810099910004312"), None, None),
        Classification::ExternalFlow,
    );

    let ClassificationResult::Resolved {
        classification,
        basis,
    } = classify(&subject, &[old, new.clone()])
    else {
        panic!("the rule matched; there should be no question");
    };
    assert_eq!(classification, Classification::ExternalFlow);
    assert_eq!(
        basis,
        Basis::Rule {
            rule: new.id,
            version: 2
        }
    );
}

#[test]
fn a_matcher_that_asks_for_nothing_matches_nothing() {
    // An "all-purpose" rule is created only by mistake, and silently
    // reclassifying the entire portfolio with it is not allowed.
    let mine = AccountId::new_random();
    let subject = transfer_to("40817810099910004312", mine);
    let catch_all = rule(1, matcher(None, None, None), Classification::ExternalFlow);

    assert!(matches!(
        classify(&subject, &[catch_all]),
        ClassificationResult::Ambiguous { .. }
    ));
}

#[test]
fn the_description_matcher_ignores_letter_case() {
    // Brokers write payment descriptions however they please; a case-sensitive
    // rule would stop working in the next report from the same broker.
    //
    let mine = AccountId::new_random();
    let subject = ClassificationSubject {
        account: mine,
        counterparty: Counterparty::Unknown,
        description: Some("КОМИССИЯ ЗА ОБСЛУЖИВАНИЕ".to_owned()),
        source_kind: None,
        source_category: None,
        movement: Some(Movement::Out),
        far_side: FarSide::Unstated,
    };
    let by_description = rule(
        1,
        matcher(None, Some("комиссия за"), None),
        Classification::Fee {
            origin: FeeOrigin::AccountMaintenance,
        },
    );

    let ClassificationResult::Resolved { classification, .. } =
        classify(&subject, &[by_description])
    else {
        panic!("the description rule must match");
    };
    assert_eq!(
        classification,
        Classification::Fee {
            origin: FeeOrigin::AccountMaintenance
        }
    );
}

#[test]
fn every_matcher_condition_must_hold() {
    // Matcher conditions are joined with "and": a rule matching on one
    // of two fields would classify unrelated operations.
    let mine = AccountId::new_random();
    let subject = transfer_to("40817810099910004312", mine);
    let too_specific = rule(
        1,
        matcher(Some("40817810099910004312"), Some("зарплата"), None),
        Classification::ExternalFlow,
    );

    assert!(matches!(
        classify(&subject, &[too_specific]),
        ClassificationResult::Ambiguous { .. }
    ));
}

#[test]
fn a_rule_explains_itself_in_words() {
    // The rule is visible: its wording is shown to the owner, and it must
    // unambiguously explain the previous classification.
    let explained = rule(
        3,
        matcher(
            Some("40817810099910004312"),
            Some("commission"),
            Some("Other"),
        ),
        Classification::Fee {
            origin: FeeOrigin::Brokerage,
        },
    );
    let text = explained.describe();

    assert!(text.contains("40817810099910004312"), "{text}");
    assert!(text.contains("commission"), "{text}");
    assert!(text.contains("Other"), "{text}");
    assert!(!text.is_empty());
}

#[test]
fn an_outflow_without_a_counterparty_asks_fee_or_withdrawal() {
    let mine = AccountId::new_random();
    let subject = ClassificationSubject {
        account: mine,
        counterparty: Counterparty::Unknown,
        description: None,
        source_kind: None,
        source_category: None,
        movement: Some(Movement::Out),
        far_side: FarSide::Unstated,
    };

    assert_eq!(
        classify(&subject, &[]),
        ClassificationResult::Ambiguous {
            question: Question::IsOutflowAFee { account: mine },
        }
    );
}

#[test]
fn an_inflow_without_a_counterparty_asks_income_or_return() {
    let mine = AccountId::new_random();
    let subject = ClassificationSubject {
        account: mine,
        counterparty: Counterparty::Unknown,
        description: None,
        source_kind: None,
        source_category: None,
        movement: Some(Movement::In),
        far_side: FarSide::Unstated,
    };

    assert_eq!(
        classify(&subject, &[]),
        ClassificationResult::Ambiguous {
            question: Question::IsInflowIncome { account: mine },
        }
    );
}

// --- history recalculation ---

struct Journal {
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
}

impl Journal {
    fn start() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            source: SourceId::new_random(),
        }
    }

    fn event(&self, kind: EventKind, legs: Vec<Leg>) -> Event {
        let day = date!(2026 - 05 - 12);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: self.owner,
            account: self.account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, 1),
            legs,
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&"7".repeat(64)).unwrap(),
                ParserVersion("ingest/manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn transfer(&self, to: AccountId) -> Event {
        let amount = Money::new(PostedMinor::new(50_000), CurrencyCode::Rub);
        let outgoing = amount.checked_negate().unwrap();
        self.event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: self.account,
                to,
                amount,
            },
            vec![Leg::cash(self.account, outgoing), Leg::cash(to, amount)],
        )
    }

    fn purchase(&self) -> Event {
        let gross = Money::new(PostedMinor::new(100_000), CurrencyCode::Rub);
        let instrument = InstrumentId::new_random();
        self.event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: Quantity(Dec::new(Decimal::from(10))),
                gross,
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(self.account, gross.checked_negate().unwrap()),
                Leg::security(
                    self.account,
                    CustodyId::new_random(),
                    instrument,
                    Quantity(Dec::new(Decimal::from(10))),
                ),
            ],
        )
    }
}

fn subjects(
    pairs: Vec<(EventId, ClassificationSubject)>,
) -> BTreeMap<EventId, ClassificationSubject> {
    pairs.into_iter().collect()
}

#[test]
fn amending_a_rule_produces_a_reversal_and_a_replacement() {
    // Recalculating history adds new facts; it does not modify old ones (§4.8).
    let journal = Journal::start();
    let recipient = AccountId::new_random();
    let moved = journal.transfer(recipient);
    let subject = transfer_to("40817810099910004312", journal.account);
    let amended = rule(
        2,
        matcher(Some("40817810099910004312"), None, None),
        Classification::ExternalFlow,
    );

    let plan = recompute_plan(
        std::slice::from_ref(&moved),
        &subjects(vec![(moved.id, subject)]),
        &[amended],
    )
    .unwrap();

    assert_eq!(
        plan,
        vec![Correction {
            target: moved.id,
            was: Classification::InternalTransfer { to: recipient },
            becomes: Classification::ExternalFlow,
        }]
    );
    assert_eq!(
        plan[0].steps(),
        [
            CorrectionStep::Reverse { target: moved.id },
            CorrectionStep::Replace {
                target: moved.id,
                classification: Classification::ExternalFlow,
            },
        ]
    );
}

#[test]
fn an_event_the_rule_does_not_touch_stays_out_of_the_plan() {
    let journal = Journal::start();
    let recipient = AccountId::new_random();
    let moved = journal.transfer(recipient);
    // The rule confirms exactly what has already been recorded.
    let same = rule(
        1,
        matcher(Some("40817810099910004312"), None, None),
        Classification::InternalTransfer { to: recipient },
    );

    let plan = recompute_plan(
        std::slice::from_ref(&moved),
        &subjects(vec![(
            moved.id,
            transfer_to("40817810099910004312", journal.account),
        )]),
        &[same],
    )
    .unwrap();

    assert_eq!(plan, vec![]);
}

#[test]
fn recomputing_twice_produces_nothing_the_second_time() {
    let journal = Journal::start();
    let recipient = AccountId::new_random();
    let moved = journal.transfer(recipient);
    let subject = transfer_to("40817810099910004312", journal.account);
    let amended = rule(
        2,
        matcher(Some("40817810099910004312"), None, None),
        Classification::ExternalFlow,
    );

    let plan = recompute_plan(
        std::slice::from_ref(&moved),
        &subjects(vec![(moved.id, subject.clone())]),
        std::slice::from_ref(&amended),
    )
    .unwrap();
    assert_eq!(plan.len(), 1);

    // Plan applied: the reversal and replacement were appended to the journal; the original
    // event is no longer active.
    let amount = Money::new(PostedMinor::new(50_000), CurrencyCode::Rub);
    let reversal = Event {
        relation: Relation::Reversal { target: moved.id },
        ..journal.event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: journal.account,
                to: recipient,
                amount,
            },
            vec![
                Leg::cash(journal.account, amount),
                Leg::cash(recipient, amount.checked_negate().unwrap()),
            ],
        )
    };
    let replacement = Event {
        relation: Relation::Replacement { target: moved.id },
        ..journal.event(
            EventKind::CashOut {
                amount: amount.checked_negate().unwrap(),
            },
            vec![Leg::cash(journal.account, amount.checked_negate().unwrap())],
        )
    };

    let again = recompute_plan(
        &[moved.clone(), reversal, replacement.clone()],
        &subjects(vec![(moved.id, subject.clone()), (replacement.id, subject)]),
        &[amended],
    )
    .unwrap();

    assert_eq!(
        again,
        vec![],
        "recalculating with the same rule does not create corrections"
    );
}

#[test]
fn an_event_that_carries_no_classification_is_never_recomputed() {
    // A trade has no classification: it is a fact, not an owner's decision.
    let journal = Journal::start();
    let bought = journal.purchase();

    let plan = recompute_plan(
        std::slice::from_ref(&bought),
        &subjects(vec![(
            bought.id,
            transfer_to("40817810099910004312", journal.account),
        )]),
        &[rule(
            1,
            matcher(Some("40817810099910004312"), None, None),
            Classification::ExternalFlow,
        )],
    )
    .unwrap();

    assert_eq!(plan, vec![]);
}

#[test]
fn an_ambiguous_subject_is_left_alone_by_the_recompute() {
    // Guessing is forbidden here as well: an event not covered by a rule remains
    // unchanged; it is not arbitrarily reclassified.
    let journal = Journal::start();
    let moved = journal.transfer(AccountId::new_random());

    let plan = recompute_plan(
        std::slice::from_ref(&moved),
        &subjects(vec![(
            moved.id,
            transfer_to("40817810099910004312", journal.account),
        )]),
        &[],
    )
    .unwrap();

    assert_eq!(plan, vec![]);
}

// --- corporate actions are not owner's decisions ---

fn amortisation_event(journal: &Journal) -> Event {
    let instrument = InstrumentId::new_random();
    let compensation = Money::new(PostedMinor::new(2_000_000), CurrencyCode::Rub);
    journal.event(
        EventKind::CorporateAction {
            action: CorporateAction::PartialRedemption {
                instrument,
                custody: CustodyId::new_random(),
                quantity: Quantity(Dec::new(Decimal::from(100))),
                principal_returned_per_unit: PerUnitAmount::new(
                    Dec::new(Decimal::from(200)),
                    CurrencyCode::Rub,
                ),
                compensation,
                effective_date: date!(2026 - 06 - 15),
                record_date: None,
                grounds: None,
                basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
            },
        },
        vec![Leg::principal(journal.account, instrument, compensation)],
    )
}

#[test]
fn amortisation_is_not_classified_as_income() {
    // The error is plausible and silent: amortization is a return
    // of own capital (§6.5), and classifying it as income would overstate
    // income by the full amount of the returned principal.
    let journal = Journal::start();
    assert_ne!(
        classification_of(&amortisation_event(&journal)),
        Some(Classification::Income { kind: None })
    );
}

#[test]
fn a_corporate_action_carries_no_classification_at_all() {
    // Not “a different classification,” but its absence: the issuer's fact
    // is not subject to recalculation by the owner's rules.
    let journal = Journal::start();
    assert_eq!(classification_of(&amortisation_event(&journal)), None);
}

#[test]
fn a_settled_offer_carries_no_classification_either() {
    let journal = Journal::start();
    let instrument = InstrumentId::new_random();
    let gross = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Rub);
    let custody = CustodyId::new_random();
    let quantity = Quantity(Dec::new(Decimal::from(10)));
    let event = journal.event(
        EventKind::OfferExercise {
            action: OfferExerciseAction::Settled {
                submission: OfferSubmissionId::new_random(),
                instrument,
                custody,
                quantity,
                gross,
                fee: None,
                accrued_interest: None,
            },
        },
        vec![
            Leg::cash(journal.account, gross),
            Leg::security(
                journal.account,
                custody,
                instrument,
                Quantity(Dec::new(Decimal::from(-10))),
            ),
        ],
    );
    assert_eq!(classification_of(&event), None);
}

// --- the category the source filed the row under (iaam-93lz) ----------------

/// A row an institution filed under a category and named nothing else by.
///
/// This is the shape the first source profile produces: the export prints no
/// operation-type column at all, so `source_kind` is `None` on every row, and
/// the category is the only word the source contributes.
fn filed_only(account: AccountId, category: &str, movement: Movement) -> ClassificationSubject {
    ClassificationSubject {
        account,
        counterparty: Counterparty::Unknown,
        description: None,
        source_kind: None,
        source_category: Some(category.to_owned()),
        movement: Some(movement),
        far_side: FarSide::Unstated,
    }
}

#[test]
fn a_row_the_source_filed_under_a_category_is_a_question_until_a_rule_reads_it() {
    // The falsification for the arm below: without the rule the row is asked
    // about, so a rule that settles it is settling something that was open.
    let mine = AccountId::new_random();
    let subject = filed_only(mine, "Bank interest", Movement::In);

    assert_eq!(
        classify(&subject, &[]),
        ClassificationResult::Ambiguous {
            question: Question::IsInflowIncome { account: mine },
        }
    );
}

#[test]
fn a_standing_rule_on_the_sources_category_settles_a_row_naming_no_operation_word() {
    // The whole of iaam-93lz. Decision 0019 §6 has a profile transcribe the
    // source's category and never map it, because the owner's rules do that job
    // — and this vocabulary had no arm for the field, so «a row this institution
    // filed under this category is interest on a balance» could not be written
    // as a standing rule at all. For the profile that ships it could not be
    // written any other way either: the export names no operation word.
    let mine = AccountId::new_random();
    let subject = filed_only(mine, "Bank interest", Movement::In);
    let standing = rule(
        1,
        filed_under("Bank interest"),
        Classification::Income {
            kind: Some(iaam_core::event::kind::IncomeKind::DepositInterest),
        },
    );

    assert_eq!(
        classify(&subject, std::slice::from_ref(&standing)),
        ClassificationResult::Resolved {
            classification: Classification::Income {
                kind: Some(iaam_core::event::kind::IncomeKind::DepositInterest),
            },
            basis: Basis::Rule {
                rule: standing.id,
                version: 1,
            },
        }
    );
}

#[test]
fn a_condition_on_the_category_does_not_fire_on_the_operation_word() {
    // The two words are two fields end to end (decision 0020 §2), and this is
    // the vocabulary keeping its half of that. A source that calls the
    // *operation* by the same string the owner wrote a *category* rule about
    // must not have the rule fire on it, and the reverse must hold too — the
    // pair used to travel through one slot, and everything round-tripped while
    // meaning the wrong thing.
    let mine = AccountId::new_random();
    let by_category = filed_under("Bank interest");
    let by_word = matcher(None, None, Some("Bank interest"));

    let filed = filed_only(mine, "Bank interest", Movement::In);
    let named = ClassificationSubject {
        source_kind: Some("Bank interest".to_owned()),
        source_category: None,
        ..filed_only(mine, "Bank interest", Movement::In)
    };

    assert!(by_category.matches(&filed));
    assert!(
        !by_category.matches(&named),
        "a category is not an operation word"
    );
    assert!(by_word.matches(&named));
    assert!(
        !by_word.matches(&filed),
        "an operation word is not a category"
    );
}

#[test]
fn a_category_is_matched_whole_and_not_as_a_substring() {
    // A source's category is a value out of a vocabulary that source controls,
    // like the operation word beside it and unlike the payment purpose. A
    // substring test would let a rule about one of an institution's categories
    // reach every other category whose name contains it.
    let mine = AccountId::new_random();
    let subject = filed_only(mine, "Bank interest", Movement::In);

    assert!(!filed_under("Bank").matches(&subject));
    assert!(!filed_under("bank interest").matches(&subject));
    assert!(filed_under("Bank interest").matches(&subject));
}

#[test]
fn a_condition_naming_only_the_category_still_asks_about_something() {
    // `asks_nothing` is the guard against an «everything» rule, and a fourth
    // field it did not know about would make a perfectly ordinary rule read as
    // one that asks nothing and match nothing — the defect that makes a matcher
    // unable to fire.
    assert!(!filed_under("Bank interest").asks_nothing());
    assert!(matcher(None, None, None).asks_nothing());
}

#[test]
fn the_fields_of_one_condition_are_joined_with_and() {
    // How a rule written on one institution's vocabulary is kept off another's:
    // the category condition is narrowed by a counterparty beside it, and both
    // must hold. Decision 0026 rests on this — it is why the matcher is not
    // scoped to a source.
    let mine = AccountId::new_random();
    let both = RuleMatcher {
        counterparty_account: Some("Shop One".to_owned()),
        description_contains: None,
        kind: None,
        source_category: Some("Bank interest".to_owned()),
    };
    let category_only = filed_only(mine, "Bank interest", Movement::Out);
    let with_counterparty = ClassificationSubject {
        counterparty: Counterparty::Named("Shop One".to_owned()),
        ..filed_only(mine, "Bank interest", Movement::Out)
    };

    assert!(
        !both.matches(&category_only),
        "the counterparty is required too"
    );
    assert!(both.matches(&with_counterparty));
}

#[test]
fn a_rule_reads_back_every_condition_it_was_written_with() {
    // Without wording there is nothing to explain a past classification with
    // (§10.4), and a condition missing from the wording is a rule the owner
    // reads as narrower than it is.
    let written = ClassificationRule {
        id: ClassificationRuleId::new_random(),
        version: 3,
        matcher: RuleMatcher {
            counterparty_account: None,
            description_contains: None,
            kind: Some("credit".to_owned()),
            source_category: Some("Bank interest".to_owned()),
        },
        outcome: Classification::Income { kind: None },
    };

    let wording = written.describe();
    assert!(wording.contains("«credit»"), "{wording}");
    assert!(wording.contains("«Bank interest»"), "{wording}");
}

// --- what the source says about the far side (iaam-cp94) --------------------

/// A row whose source asserted the far side is the owner's, naming nobody and
/// no direction.
fn asserted_own(account: AccountId) -> ClassificationSubject {
    ClassificationSubject {
        account,
        counterparty: Counterparty::Unknown,
        description: None,
        source_kind: Some("INNER".to_owned()),
        source_category: None,
        movement: None,
        far_side: FarSide::OwnAccount,
    }
}

#[test]
fn a_source_that_says_the_far_side_is_the_owners_settles_the_row_without_a_question() {
    // The four rows this bead was filed for: an amount, a date, no direction,
    // no counterparty, and a word meaning «between your own accounts». Every
    // one of them used to raise `UnresolvedDirection` and block the commit.
    let account = AccountId::new_random();
    assert_eq!(
        classify(&asserted_own(account), &[]),
        ClassificationResult::Resolved {
            classification: Classification::OwnAccountMovement,
            basis: Basis::Derived,
        }
    );
}

#[test]
fn the_assertion_says_nothing_about_direction() {
    // It is not a direction, and nothing may read one out of it: a fact with a
    // direction debits or credits the account, and the source said which way
    // for neither of these.
    assert_eq!(Classification::OwnAccountMovement.implied_movement(), None);
}

#[test]
fn a_rule_the_owner_wrote_beats_the_word_the_source_printed() {
    // The order matters and is the owner's: his standing decision about this
    // counterparty is a stronger statement than a bank's own filing of the row.
    let account = AccountId::new_random();
    let rule = ClassificationRule {
        id: iaam_core::ids::ClassificationRuleId::new_random(),
        version: 1,
        matcher: RuleMatcher {
            counterparty_account: None,
            description_contains: None,
            kind: Some("INNER".to_owned()),
            source_category: None,
        },
        outcome: Classification::ExternalFlow,
    };
    assert!(matches!(
        classify(&asserted_own(account), &[rule]),
        ClassificationResult::Resolved {
            classification: Classification::ExternalFlow,
            ..
        }
    ));
}

#[test]
fn a_far_side_the_directory_recognised_beats_the_assertion_too() {
    // The directory names *which* account, which is strictly more than the
    // source said. Answering the weaker outcome here would throw away the one
    // thing that makes a complete transfer possible.
    let account = AccountId::new_random();
    let savings = AccountId::new_random();
    let subject = ClassificationSubject {
        counterparty: Counterparty::OwnAccount(savings),
        ..asserted_own(account)
    };
    assert!(matches!(
        classify(&subject, &[]),
        ClassificationResult::Resolved {
            classification: Classification::InternalTransfer { to },
            ..
        } if to == savings
    ));
}

#[test]
fn a_row_whose_source_said_nothing_about_the_far_side_is_still_asked_about() {
    // The falsification: if `Unstated` settled anything, the assertion would be
    // buying nothing and every directionless row would stop being a question.
    let account = AccountId::new_random();
    let subject = ClassificationSubject {
        far_side: FarSide::Unstated,
        ..asserted_own(account)
    };
    assert!(matches!(
        classify(&subject, &[]),
        ClassificationResult::Ambiguous {
            question: Question::UnresolvedDirection { .. }
        }
    ));
}

#[test]
fn the_two_words_the_far_side_may_carry_survive_the_wire() {
    for value in [FarSide::Unstated, FarSide::OwnAccount] {
        assert_eq!(FarSide::parse(value.code()).expect("its own code"), value);
    }
    assert!(FarSide::parse("own").is_err(), "a near miss is refused");
}

// --- The same decision put twice (iaam-q5og, decision 0029) ----------------
//
// A statement names one shop on thirty lines and every one of them is the same
// question. What makes them the same is not the row and not the question alone;
// it is the pair `QuestionSubject` holds, and the half that is easy to leave out
// is the direction the source stated.

/// Two rows naming one counterparty, both leaving the account, are one decision.
#[test]
fn one_counterparty_on_two_rows_that_ran_the_same_way_is_one_decision() {
    let account = AccountId::new_random();
    let named = |movement| ClassificationSubject {
        account,
        counterparty: Counterparty::Named("Shop One".to_owned()),
        description: None,
        source_kind: None,
        source_category: None,
        movement: Some(movement),
        far_side: FarSide::Unstated,
    };
    let first = named(Movement::Out);
    let second = named(Movement::Out);
    let (
        ClassificationResult::Ambiguous { question: one },
        ClassificationResult::Ambiguous { question: two },
    ) = (classify(&first, &[]), classify(&second, &[]))
    else {
        panic!("a named counterparty no rule covers is a question");
    };
    assert_eq!(one.about(first.movement), two.about(second.movement));
}

/// One counterparty, two directions: the question is equal and the decision is not.
///
/// This is the whole reason the subject is a pair. `question_for` builds
/// `IsTransferInternal` for a named counterparty in either direction, so the two
/// questions here **are** equal — and an answer states a direction of its own
/// that `resolve_with` records, so carrying one answer across both would file
/// money that arrived as money that left.
#[test]
fn one_counterparty_on_two_rows_the_source_ran_opposite_ways_is_two_decisions() {
    let account = AccountId::new_random();
    let named = |movement| ClassificationSubject {
        account,
        counterparty: Counterparty::Named("Shop One".to_owned()),
        description: None,
        source_kind: None,
        source_category: None,
        movement: Some(movement),
        far_side: FarSide::Unstated,
    };
    let paid = named(Movement::Out);
    let arrived = named(Movement::In);
    let (
        ClassificationResult::Ambiguous { question: one },
        ClassificationResult::Ambiguous { question: two },
    ) = (classify(&paid, &[]), classify(&arrived, &[]))
    else {
        panic!("a named counterparty no rule covers is a question");
    };
    assert_eq!(one, two, "the question itself does not distinguish them");
    assert_ne!(
        one.about(paid.movement),
        two.about(arrived.movement),
        "the decision does"
    );
}

/// Two counterparties are two decisions however alike the rows are otherwise.
#[test]
fn two_counterparties_are_two_decisions() {
    let account = AccountId::new_random();
    let named = |name: &str| ClassificationSubject {
        account,
        counterparty: Counterparty::Named(name.to_owned()),
        description: None,
        source_kind: None,
        source_category: None,
        movement: Some(Movement::Out),
        far_side: FarSide::Unstated,
    };
    let one = named("Shop One");
    let two = named("Shop Two");
    let (
        ClassificationResult::Ambiguous { question: first },
        ClassificationResult::Ambiguous { question: second },
    ) = (classify(&one, &[]), classify(&two, &[]))
    else {
        panic!("a named counterparty no rule covers is a question");
    };
    assert_ne!(first.about(one.movement), second.about(two.movement));
}

/// The same account asked two different questions is two decisions.
///
/// The falsification for reading the account alone: both of these are asked of
/// one account, neither names a counterparty, and they are opposite questions.
#[test]
fn one_account_asked_about_an_outflow_and_an_inflow_is_two_decisions() {
    let account = AccountId::new_random();
    let anonymous = |movement| ClassificationSubject {
        account,
        counterparty: Counterparty::Unknown,
        description: None,
        source_kind: None,
        source_category: None,
        movement: Some(movement),
        far_side: FarSide::Unstated,
    };
    let out = anonymous(Movement::Out);
    let into = anonymous(Movement::In);
    let (
        ClassificationResult::Ambiguous { question: first },
        ClassificationResult::Ambiguous { question: second },
    ) = (classify(&out, &[]), classify(&into, &[]))
    else {
        panic!("an unnamed counterparty no rule covers is a question");
    };
    assert_ne!(first.about(out.movement), second.about(into.movement));
}
