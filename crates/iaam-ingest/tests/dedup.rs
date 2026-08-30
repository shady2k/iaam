//! Идемпотентность и дедупликация (§10.6).
//!
//! Пять уровней иерархии и правило, ради которого она существует:
//! вероятностный дубликат показывается владельцу, а не удаляется.

use iaam_core::event::provenance::RawHash;
use iaam_core::ids::{AccountId, EventId, InstrumentId};
use iaam_core::money::CurrencyCode;
use iaam_ingest::dedup::{
    DedupDecision, DedupKey, DedupLevel, DocumentContext, KnownRecord, assess, canonical_form,
    choose_key, fingerprint,
};
use iaam_ingest::{OperationDates, OperationKind, SubmittedOperation};
use time::macros::date;
use uuid::Uuid;

fn hash(seed: &str) -> RawHash {
    RawHash::parse(&seed.repeat(64)).unwrap()
}

fn deposit(account: AccountId, minor: i64) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind: OperationKind::Deposit {
            amount_minor: minor,
            currency: CurrencyCode::Rub,
        },
        dates: OperationDates {
            cash_posted: Some(date!(2026 - 04 - 01)),
            ..OperationDates::default()
        },
        idempotency_key: None,
        source_operation_id: None,
    }
}

/// Строка документа: и документ, и локатор известны.
fn from_row(document: &RawHash, row: u64) -> DocumentContext {
    DocumentContext {
        document: Some(document.clone()),
        sheet: Some("Сделки".to_owned()),
        row: Some(row),
    }
}

/// Канал без файла: API брокера.
fn from_stream() -> DocumentContext {
    DocumentContext {
        document: None,
        sheet: None,
        row: None,
    }
}

fn recorded(operation: &SubmittedOperation, context: &DocumentContext) -> KnownRecord {
    KnownRecord {
        event: EventId::new_random(),
        source_operation_id: operation.source_operation_id.clone(),
        idempotency_key: operation.idempotency_key.clone(),
        fingerprint: fingerprint(operation),
        document: context.document.clone(),
        sheet: context.sheet.clone(),
        row: context.row,
    }
}

#[test]
fn the_strongest_available_key_wins() {
    // Иерархия от сильного к слабому: стабильный идентификатор источника
    // сильнее клиентского ключа, а клиентский — сильнее места в файле.
    // Взять слабый при доступном сильном значит потерять точность
    // дедупликации на ровном месте.
    let mut operation = deposit(AccountId::new_random(), 100_000);
    operation.source_operation_id = Some("OP-4417".to_owned());
    operation.idempotency_key = Some("client-1".to_owned());
    let document = hash("a");

    assert_eq!(
        choose_key(&operation, &from_row(&document, 7)),
        Some(DedupKey::SourceOperationId("OP-4417".to_owned()))
    );
}

#[test]
fn the_client_key_is_taken_when_the_source_names_nothing() {
    let mut operation = deposit(AccountId::new_random(), 100_000);
    operation.idempotency_key = Some("client-1".to_owned());

    assert_eq!(
        choose_key(&operation, &from_stream()),
        Some(DedupKey::IdempotencyKey("client-1".to_owned()))
    );
}

#[test]
fn the_row_locator_outranks_a_bare_fingerprint() {
    // Отпечаток не является тождеством: две законные одинаковые покупки
    // дают один отпечаток. Локатор — является.
    let operation = deposit(AccountId::new_random(), 100_000);
    let document = hash("b");

    assert_eq!(
        choose_key(&operation, &from_row(&document, 7)),
        Some(DedupKey::DocumentRow {
            document,
            sheet: Some("Сделки".to_owned()),
            row: 7,
        })
    );
}

#[test]
fn a_channel_without_a_row_number_falls_back_to_the_fingerprint() {
    // Выписка, разобранная не по таблице: документ есть, номера строки
    // нет. Это и есть уровень 3 §10.6.
    let operation = deposit(AccountId::new_random(), 100_000);
    let document = hash("c");
    let context = DocumentContext {
        document: Some(document.clone()),
        sheet: None,
        row: None,
    };

    assert_eq!(
        choose_key(&operation, &context),
        Some(DedupKey::NormalizedFingerprint {
            document,
            fingerprint: fingerprint(&operation),
        })
    );
}

#[test]
fn a_submission_that_nothing_identifies_has_no_key() {
    // Канал без файла и без идентификаторов: жёсткого ключа нет, и
    // выдумывать его нельзя — остаётся вероятностный уровень.
    let operation = deposit(AccountId::new_random(), 100_000);
    assert_eq!(choose_key(&operation, &from_stream()), None);
}

#[test]
fn two_identical_purchases_on_one_day_are_not_a_duplicate() {
    // Прямое требование §10.6. Естественный ключ «счёт + дата + сумма»
    // объявил бы их одной сделкой, и вторая покупка исчезла бы из
    // портфеля — молча и навсегда.
    let account = AccountId::new_random();
    let document = hash("d");
    let first = deposit(account, 100_000);
    let first_context = from_row(&document, 7);
    let known = vec![recorded(&first, &first_context)];

    // Та же операция, но другая строка того же документа.
    let second = deposit(account, 100_000);
    let second_context = from_row(&document, 8);
    let key = choose_key(&second, &second_context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&second), &second_context, &known),
        DedupDecision::Fresh,
        "документ и есть свидетельство того, что операций было две"
    );
}

#[test]
fn reloading_the_same_document_duplicates_every_row() {
    // Тот же файл, те же строки: нормальный путь — владелец загрузил
    // отчёт дважды.
    let account = AccountId::new_random();
    let document = hash("e");
    let rows = [7_u64, 8, 9];
    let known: Vec<KnownRecord> = rows
        .iter()
        .map(|row| recorded(&deposit(account, 100_000), &from_row(&document, *row)))
        .collect();

    for row in rows {
        let operation = deposit(account, 100_000);
        let context = from_row(&document, row);
        let key = choose_key(&operation, &context);
        let decision = assess(key.as_ref(), &fingerprint(&operation), &context, &known);
        let DedupDecision::Duplicate { key, .. } = decision else {
            panic!("повторная загрузка строки {row} обязана быть дубликатом: {decision:?}");
        };
        assert_eq!(key.level(), DedupLevel::DocumentRow);
    }
}

#[test]
fn the_same_fingerprint_across_documents_is_only_a_hint() {
    // Отпечаток совпал, но документы разные: это может быть законная
    // одинаковая операция. Показываем, не удаляем.
    let account = AccountId::new_random();
    let earlier = deposit(account, 100_000);
    let known = vec![recorded(&earlier, &from_row(&hash("a"), 7))];

    let later = deposit(account, 100_000);
    let context = from_row(&hash("b"), 3);
    let key = choose_key(&later, &context);
    let decision = assess(key.as_ref(), &fingerprint(&later), &context, &known);

    assert_eq!(
        decision,
        DedupDecision::PossibleDuplicate {
            of: known[0].event,
            level: DedupLevel::Probabilistic,
        }
    );
}

#[test]
fn a_possible_duplicate_is_still_recorded() {
    // Вероятностная оценка не приводит к автоматическому удалению
    // (§10.6): решение обязано отличаться от `Duplicate` именно тем,
    // что записи не отменяет.
    let account = AccountId::new_random();
    let known = vec![recorded(&deposit(account, 100_000), &from_stream())];
    let operation = deposit(account, 100_000);
    let context = from_row(&hash("f"), 1);
    let key = choose_key(&operation, &context);

    let decision = assess(key.as_ref(), &fingerprint(&operation), &context, &known);
    assert!(
        decision.records_the_row(),
        "вероятностный дубликат записывается: {decision:?}"
    );
    assert!(
        !DedupDecision::Duplicate {
            key: DedupKey::IdempotencyKey("k".to_owned()),
            existing: EventId::new_random(),
        }
        .records_the_row()
    );
    assert!(DedupDecision::Fresh.records_the_row());
}

#[test]
fn two_identical_submissions_from_a_stream_are_a_hint() {
    // У канала без файла нет свидетельства, что операций было две:
    // документа, в котором видны две строки, не существует. Молчаливый
    // `Fresh` здесь удвоил бы позицию, а §10.6 требует показать.
    let account = AccountId::new_random();
    let known = vec![recorded(&deposit(account, 100_000), &from_stream())];
    let repeat = deposit(account, 100_000);
    let context = from_stream();

    assert_eq!(
        assess(
            choose_key(&repeat, &context).as_ref(),
            &fingerprint(&repeat),
            &context,
            &known
        ),
        DedupDecision::PossibleDuplicate {
            of: known[0].event,
            level: DedupLevel::Probabilistic,
        }
    );
}

#[test]
fn the_client_key_catches_a_resubmission_without_a_document() {
    let account = AccountId::new_random();
    let mut operation = deposit(account, 100_000);
    operation.idempotency_key = Some("client-1".to_owned());
    let context = from_stream();
    let known = vec![recorded(&operation, &context)];

    // Повтор с тем же ключом, но другой суммой: клиент повторил запрос,
    // а не прислал новую операцию.
    let mut repeat = deposit(account, 999_000);
    repeat.idempotency_key = Some("client-1".to_owned());
    let key = choose_key(&repeat, &context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&repeat), &context, &known),
        DedupDecision::Duplicate {
            key: DedupKey::IdempotencyKey("client-1".to_owned()),
            existing: known[0].event,
        }
    );
}

#[test]
fn repeated_source_wins_locator_and_becomes_duplicate() {
    let account = AccountId::new_random();
    let document = hash("1");
    let mut first = deposit(account, 100_000);
    first.source_operation_id = Some("OP-4417".to_owned());
    let known = vec![recorded(&first, &from_row(&document, 7))];

    let mut repeat = deposit(account, 999_000);
    repeat.source_operation_id = Some("OP-4417".to_owned());
    let context = from_row(&document, 8);
    let key = choose_key(&repeat, &context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&repeat), &context, &known),
        DedupDecision::Duplicate {
            key: DedupKey::SourceOperationId("OP-4417".to_owned()),
            existing: known[0].event,
        }
    );
}

#[test]
fn different_source_identifiers_do_not_merge() {
    let account = AccountId::new_random();
    let mut first = deposit(account, 100_000);
    first.source_operation_id = Some("OP-1".to_owned());
    let known = vec![recorded(&first, &from_stream())];

    let mut incoming = deposit(account, 100_001);
    incoming.source_operation_id = Some("OP-2".to_owned());
    let context = from_stream();
    let key = choose_key(&incoming, &context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&incoming), &context, &known),
        DedupDecision::Fresh
    );
}

#[test]
fn same_document_and_fingerprint_become_duplicate() {
    let account = AccountId::new_random();
    let document = hash("2");
    let context = DocumentContext {
        document: Some(document.clone()),
        sheet: None,
        row: None,
    };
    let first = deposit(account, 100_000);
    let known = vec![recorded(&first, &context)];
    let repeat = deposit(account, 100_000);
    let key = choose_key(&repeat, &context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&repeat), &context, &known),
        DedupDecision::Duplicate {
            key: DedupKey::NormalizedFingerprint {
                document,
                fingerprint: fingerprint(&repeat),
            },
            existing: known[0].event,
        }
    );
}

#[test]
fn same_document_with_different_fingerprint_enters_journal() {
    let account = AccountId::new_random();
    let document = hash("3");
    let context = DocumentContext {
        document: Some(document),
        sheet: None,
        row: None,
    };
    let known = vec![recorded(&deposit(account, 100_000), &context)];
    let incoming = deposit(account, 100_001);
    let key = choose_key(&incoming, &context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&incoming), &context, &known),
        DedupDecision::Fresh
    );
}

#[test]
fn different_document_with_same_fingerprint_remains_a_hint() {
    let account = AccountId::new_random();
    let known_context = DocumentContext {
        document: Some(hash("4")),
        sheet: None,
        row: None,
    };
    let incoming_context = DocumentContext {
        document: Some(hash("5")),
        sheet: None,
        row: None,
    };
    let first = deposit(account, 100_000);
    let known = vec![recorded(&first, &known_context)];
    let incoming = deposit(account, 100_000);
    let key = choose_key(&incoming, &incoming_context);

    assert_eq!(
        assess(
            key.as_ref(),
            &fingerprint(&incoming),
            &incoming_context,
            &known
        ),
        DedupDecision::PossibleDuplicate {
            of: known[0].event,
            level: DedupLevel::Probabilistic,
        }
    );
}

#[test]
fn owner_hint_contains_fifth_level() {
    let account = AccountId::new_random();
    let known_context = from_row(&hash("6"), 7);
    let incoming_context = from_row(&hash("7"), 8);
    let first = deposit(account, 100_000);
    let known = vec![recorded(&first, &known_context)];
    let incoming = deposit(account, 100_000);
    let key = choose_key(&incoming, &incoming_context);

    let DedupDecision::PossibleDuplicate { level, .. } = assess(
        key.as_ref(),
        &fingerprint(&incoming),
        &incoming_context,
        &known,
    ) else {
        panic!("совпадение отпечатка разных документов обязано стать подсказкой");
    };
    assert_eq!(
        level,
        DedupLevel::Probabilistic,
        "владелец должен видеть вероятностный уровень"
    );
    assert_eq!(level.number(), 5, "номер уровня — часть объяснения решения");
}

#[test]
fn the_fingerprint_ignores_the_keys_that_name_the_submission() {
    // Ключ идемпотентности называет подачу, а не операцию: одна и та же
    // операция, посланная с разными ключами, обязана давать один
    // отпечаток — иначе уровень 3 не поймает ничего.
    let account = AccountId::new_random();
    let mut named = deposit(account, 100_000);
    named.idempotency_key = Some("client-1".to_owned());
    named.source_operation_id = Some("OP-1".to_owned());

    assert_eq!(fingerprint(&named), fingerprint(&deposit(account, 100_000)));
}

#[test]
fn different_operations_have_different_fingerprints() {
    let account = AccountId::new_random();
    assert_ne!(
        fingerprint(&deposit(account, 100_000)),
        fingerprint(&deposit(account, 100_001))
    );
    assert_ne!(
        fingerprint(&deposit(account, 100_000)),
        fingerprint(&deposit(AccountId::new_random(), 100_000))
    );
    let mut other_kind = deposit(account, 100_000);
    other_kind.kind = OperationKind::Income {
        instrument: Some(InstrumentId::new_random()),
        gross_minor: 100_000,
        currency: CurrencyCode::Rub,
        kind: None,
    };
    assert_ne!(
        fingerprint(&other_kind),
        fingerprint(&deposit(account, 100_000))
    );
}

/// Каноническая форма, от которой считается отпечаток.
///
/// Записана здесь дословно и **вручную**: по ней уже дедуплицировано,
/// и молчаливое изменение формы обесценило бы все прежние отпечатки.
const FROZEN_CANONICAL: &str = concat!(
    r#"{"v":1,"account":"00000000-0000-0000-0000-000000000001","#,
    r#""kind":{"Deposit":{"amount_minor":100000,"currency":"Rub"}},"#,
    r#""dates":{"trade":null,"settled":null,"cash_posted":"2026-04-01","paid":null}}"#,
);

#[test]
fn the_canonical_form_is_the_frozen_one() {
    let operation = deposit(AccountId(Uuid::from_u128(1)), 100_000);
    assert_eq!(canonical_form(&operation), FROZEN_CANONICAL);
}

#[test]
fn the_canonical_fingerprint_is_frozen() {
    // Значение посчитано независимо от программы (§15.5):
    //
    //   printf '%s' "$FROZEN_CANONICAL" | sha256sum
    //
    // Если этот тест упал, значит изменилась каноническая форма, и
    // прежние отпечатки перестали совпадать с новыми. Чинить его
    // подстановкой нового значения нельзя: формат версионирован полем
    // `v`, и смена формы обязана поднимать версию.
    let operation = deposit(AccountId(Uuid::from_u128(1)), 100_000);
    assert_eq!(
        fingerprint(&operation).as_str(),
        "366eff5d4b6007730b75786c85d7d9fc73d317e48b1df5c71545dafa3eb3831c"
    );
}
