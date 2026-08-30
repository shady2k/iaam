//! Правила классификации и пересчёт истории (§10.4).

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
    Correction, CorrectionStep, Counterparty, Movement, Question, RuleMatcher, classification_of,
    classify, recompute_plan,
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
        movement: Movement::Out,
    }
}

#[test]
fn a_transfer_to_an_own_account_needs_no_rule() {
    // Правило нужно там, где данных не хватает. Спрашивать о том, что
    // уже известно, — значит требовать от владельца работы впустую.
    let mine = AccountId::new_random();
    let other = AccountId::new_random();
    let subject = ClassificationSubject {
        account: mine,
        counterparty: Counterparty::OwnAccount(other),
        description: None,
        source_kind: None,
        movement: Movement::Out,
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
    // Правка правила заводит новую версию: старшая — последнее решение
    // владельца, а не одно из двух равноправных.
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
        panic!("правило подошло, вопроса быть не должно");
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
    // Правило «на всё» заводится только по ошибке, и молча
    // переклассифицировать им весь портфель нельзя.
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
    // Назначение платежа брокеры пишут как придётся; правило,
    // чувствительное к регистру, перестало бы работать на следующем
    // отчёте того же брокера.
    let mine = AccountId::new_random();
    let subject = ClassificationSubject {
        account: mine,
        counterparty: Counterparty::Unknown,
        description: Some("КОМИССИЯ ЗА ОБСЛУЖИВАНИЕ".to_owned()),
        source_kind: None,
        movement: Movement::Out,
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
        panic!("правило по описанию обязано подойти");
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
    // Условия матчера соединяются «и»: правило, подошедшее по одному
    // полю из двух, классифицировало бы чужие операции.
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
    // Правило видимо: формулировку показывают владельцу, и она обязана
    // однозначно объяснять прошлую классификацию.
    let explained = rule(
        3,
        matcher(
            Some("40817810099910004312"),
            Some("комиссия"),
            Some("Прочее"),
        ),
        Classification::Fee {
            origin: FeeOrigin::Brokerage,
        },
    );
    let text = explained.describe();

    assert!(text.contains("40817810099910004312"), "{text}");
    assert!(text.contains("комиссия"), "{text}");
    assert!(text.contains("Прочее"), "{text}");
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
        movement: Movement::Out,
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
        movement: Movement::In,
    };

    assert_eq!(
        classify(&subject, &[]),
        ClassificationResult::Ambiguous {
            question: Question::IsInflowIncome { account: mine },
        }
    );
}

// --- пересчёт истории ---

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
    // Пересчёт истории — это новые факты, а не правка старых (§4.8).
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
    // Правило подтверждает ровно то, что уже записано.
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

    // План применён: сторно и замена дописаны в журнал, исходное
    // событие перестало быть действующим.
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
        "повторный пересчёт тем же правилом не создаёт исправлений"
    );
}

#[test]
fn an_event_that_carries_no_classification_is_never_recomputed() {
    // У сделки классификации нет: она факт, а не решение владельца.
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
    // Догадка запрещена и здесь: не покрытое правилом событие остаётся
    // как есть, а не переклассифицируется наугад.
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

// --- корпоративные действия не являются решениями владельца ---

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
    // Ошибка правдоподобна и молчалива: амортизация — возврат
    // собственного капитала (§6.5), и отнесение её к доходу завысило бы
    // доход на всю сумму возвращённого номинала.
    let journal = Journal::start();
    assert_ne!(
        classification_of(&amortisation_event(&journal)),
        Some(Classification::Income)
    );
}

#[test]
fn a_corporate_action_carries_no_classification_at_all() {
    // Не «другая классификация», а её отсутствие: факт эмитента
    // пересчёту правилами владельца не подлежит.
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
