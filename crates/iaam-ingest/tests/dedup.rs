//! Idempotency and deduplication (§10.6).
//!
//! Five hierarchy levels and the rule they exist for:
//! a probable duplicate is shown to the owner rather than deleted.

use iaam_core::event::provenance::RawHash;
use iaam_core::ids::{AccountId, EventId, InstrumentId};
use iaam_core::money::CurrencyCode;
use iaam_ingest::dedup::{
    DedupDecision, DedupKey, DedupLevel, DocumentContext, IdentityScope, KnownRecord, assess,
    canonical_form, choose_key, fingerprint,
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
        source_time: None,
        idempotency_key: None,
        source_operation_id: None,
        source_category: None,
    }
}

/// Document row: both the document and locator are known.
fn from_row(account: AccountId, document: &RawHash, row: u64) -> DocumentContext {
    DocumentContext {
        account,
        document: Some(document.clone()),
        sheet: Some("Сделки".to_owned()),
        row: Some(row),
        identity_scope: IdentityScope::Source,
    }
}

/// Channel without a file: broker API.
fn from_stream(account: AccountId) -> DocumentContext {
    DocumentContext {
        account,
        document: None,
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
    }
}

fn recorded(operation: &SubmittedOperation, context: &DocumentContext) -> KnownRecord {
    KnownRecord {
        event: EventId::new_random(),
        account: operation.account,
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
    // Hierarchy from strongest to weakest: a stable source identifier
    // is stronger than a client key, and a client key is stronger than a file location.
    // Using a weak key when a strong one is available means losing deduplication
    // accuracy for no reason.
    let mut operation = deposit(AccountId::new_random(), 100_000);
    operation.source_operation_id = Some("OP-4417".to_owned());
    operation.idempotency_key = Some("client-1".to_owned());
    let document = hash("a");

    assert_eq!(
        choose_key(&operation, &from_row(operation.account, &document, 7)),
        Some(DedupKey::SourceOperationId("OP-4417".to_owned()))
    );
}

#[test]
fn the_client_key_is_taken_when_the_source_names_nothing() {
    let mut operation = deposit(AccountId::new_random(), 100_000);
    operation.idempotency_key = Some("client-1".to_owned());

    assert_eq!(
        choose_key(&operation, &from_stream(operation.account)),
        Some(DedupKey::IdempotencyKey("client-1".to_owned()))
    );
}

#[test]
fn the_row_locator_outranks_a_bare_fingerprint() {
    // A fingerprint is not identity: two legitimate identical purchases
    // produce one fingerprint. A locator is identity.
    let operation = deposit(AccountId::new_random(), 100_000);
    let document = hash("b");

    assert_eq!(
        choose_key(&operation, &from_row(operation.account, &document, 7)),
        Some(DedupKey::DocumentRow {
            document,
            sheet: Some("Сделки".to_owned()),
            row: 7,
        })
    );
}

#[test]
fn a_channel_without_a_row_number_falls_back_to_the_fingerprint() {
    // A statement parsed without a table: the document exists, but there is no row number.
    // This is level 3 of §10.6.
    let operation = deposit(AccountId::new_random(), 100_000);
    let document = hash("c");
    let context = DocumentContext {
        account: operation.account,
        document: Some(document.clone()),
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
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
    // A channel without a file or identifiers: there is no hard key, and
    // one must not be invented—the probabilistic level remains.
    let operation = deposit(AccountId::new_random(), 100_000);
    assert_eq!(
        choose_key(&operation, &from_stream(operation.account)),
        None
    );
}

#[test]
fn two_identical_purchases_on_one_day_are_not_a_duplicate() {
    // Direct requirement of §10.6. The natural key “account + date + amount”
    // would declare them to be the same transaction, and the second purchase would disappear
    // from the portfolio—silently and forever.
    let account = AccountId::new_random();
    let document = hash("d");
    let first = deposit(account, 100_000);
    let first_context = from_row(account, &document, 7);
    let known = vec![recorded(&first, &first_context)];

    // Same operation, but a different row of the same document.
    let second = deposit(account, 100_000);
    let second_context = from_row(account, &document, 8);
    let key = choose_key(&second, &second_context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&second), &second_context, &known),
        DedupDecision::Fresh,
        "the document itself is evidence that there were two operations"
    );
}

#[test]
fn reloading_the_same_document_duplicates_every_row() {
    // Same file, same rows: the normal path is that the owner uploaded
    // the report twice.
    let account = AccountId::new_random();
    let document = hash("e");
    let rows = [7_u64, 8, 9];
    let known: Vec<KnownRecord> = rows
        .iter()
        .map(|row| {
            recorded(
                &deposit(account, 100_000),
                &from_row(account, &document, *row),
            )
        })
        .collect();

    for row in rows {
        let operation = deposit(account, 100_000);
        let context = from_row(account, &document, row);
        let key = choose_key(&operation, &context);
        let decision = assess(key.as_ref(), &fingerprint(&operation), &context, &known);
        let DedupDecision::Duplicate { key, .. } = decision else {
            panic!("reloading row {row} must be a duplicate: {decision:?}");
        };
        assert_eq!(key.level(), DedupLevel::DocumentRow);
    }
}

#[test]
fn the_same_fingerprint_across_documents_is_only_a_hint() {
    // The fingerprint matched, but the documents differ: this may be a legitimate
    // identical operation. Show it, do not delete it.
    let account = AccountId::new_random();
    let earlier = deposit(account, 100_000);
    let known = vec![recorded(&earlier, &from_row(account, &hash("a"), 7))];

    let later = deposit(account, 100_000);
    let context = from_row(account, &hash("b"), 3);
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
    // A probabilistic assessment does not result in automatic deletion
    // (§10.6): the decision must differ from `Duplicate` precisely in that
    // it does not cancel the record.
    let account = AccountId::new_random();
    let known = vec![recorded(&deposit(account, 100_000), &from_stream(account))];
    let operation = deposit(account, 100_000);
    let context = from_row(account, &hash("f"), 1);
    let key = choose_key(&operation, &context);

    let decision = assess(key.as_ref(), &fingerprint(&operation), &context, &known);
    assert!(
        decision.records_the_row(),
        "probable duplicate is recorded: {decision:?}"
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
    // A channel without a file has no evidence that there were two operations:
    // there is no document showing two rows. Silently treating this as
    // `Fresh` would double the position, and §10.6 requires showing it.
    let account = AccountId::new_random();
    let known = vec![recorded(&deposit(account, 100_000), &from_stream(account))];
    let repeat = deposit(account, 100_000);
    let context = from_stream(account);

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
    let context = from_stream(account);
    let known = vec![recorded(&operation, &context)];

    // A repeat with the same key but a different amount: the client repeated the request,
    // rather than submitting a new operation.
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
    let known = vec![recorded(&first, &from_row(account, &document, 7))];

    let mut repeat = deposit(account, 999_000);
    repeat.source_operation_id = Some("OP-4417".to_owned());
    let context = from_row(account, &document, 8);
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
    let known = vec![recorded(&first, &from_stream(account))];

    let mut incoming = deposit(account, 100_001);
    incoming.source_operation_id = Some("OP-2".to_owned());
    let context = from_stream(account);
    let key = choose_key(&incoming, &context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&incoming), &context, &known),
        DedupDecision::Fresh
    );
}

#[test]
fn account_scope_allows_reused_source_identifier_across_accounts() {
    let first_account = AccountId::new_random();
    let second_account = AccountId::new_random();
    let mut first = deposit(first_account, 100_000);
    first.source_operation_id = Some("OP-ACCOUNT-1".to_owned());
    let context = DocumentContext {
        account: second_account,
        document: None,
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Account,
    };
    let known = vec![recorded(&first, &context)];

    let mut incoming = deposit(second_account, 100_000);
    incoming.source_operation_id = Some("OP-ACCOUNT-1".to_owned());
    let key = choose_key(&incoming, &context);

    assert_eq!(
        assess(key.as_ref(), &fingerprint(&incoming), &context, &known),
        DedupDecision::Fresh
    );
}

#[test]
fn account_scope_repeats_same_account_source_identifier_as_duplicate() {
    let account = AccountId::new_random();
    let mut first = deposit(account, 100_000);
    first.source_operation_id = Some("OP-ACCOUNT-2".to_owned());
    let context = DocumentContext {
        account,
        document: None,
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Account,
    };
    let known = vec![recorded(&first, &context)];

    let mut incoming = deposit(account, 999_000);
    incoming.source_operation_id = Some("OP-ACCOUNT-2".to_owned());
    let key = choose_key(&incoming, &context);

    assert!(matches!(
        assess(key.as_ref(), &fingerprint(&incoming), &context, &known),
        DedupDecision::Duplicate {
            key: DedupKey::SourceOperationId(id),
            existing,
        } if id == "OP-ACCOUNT-2" && existing == known[0].event
    ));
}

#[test]
fn source_scope_reused_identifier_across_accounts_remains_duplicate() {
    let first_account = AccountId::new_random();
    let second_account = AccountId::new_random();
    let mut first = deposit(first_account, 100_000);
    first.source_operation_id = Some("OP-SOURCE-1".to_owned());
    let context = DocumentContext {
        account: second_account,
        document: None,
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
    };
    let known = vec![recorded(&first, &context)];

    let mut incoming = deposit(second_account, 100_000);
    incoming.source_operation_id = Some("OP-SOURCE-1".to_owned());
    let key = choose_key(&incoming, &context);

    assert!(matches!(
        assess(key.as_ref(), &fingerprint(&incoming), &context, &known),
        DedupDecision::Duplicate { existing, .. } if existing == known[0].event
    ));
}

#[test]
fn source_scope_repeated_identifier_same_account_remains_duplicate() {
    let account = AccountId::new_random();
    let mut first = deposit(account, 100_000);
    first.source_operation_id = Some("OP-SOURCE-2".to_owned());
    let context = DocumentContext {
        account,
        document: None,
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
    };
    let known = vec![recorded(&first, &context)];

    let mut incoming = deposit(account, 999_000);
    incoming.source_operation_id = Some("OP-SOURCE-2".to_owned());
    let key = choose_key(&incoming, &context);

    assert!(matches!(
        assess(key.as_ref(), &fingerprint(&incoming), &context, &known),
        DedupDecision::Duplicate { existing, .. } if existing == known[0].event
    ));
}

#[test]
fn same_document_and_fingerprint_become_duplicate() {
    let account = AccountId::new_random();
    let document = hash("2");
    let context = DocumentContext {
        account,
        document: Some(document.clone()),
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
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
        account,
        document: Some(document),
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
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
        account,
        document: Some(hash("4")),
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
    };
    let incoming_context = DocumentContext {
        account,
        document: Some(hash("5")),
        sheet: None,
        row: None,
        identity_scope: IdentityScope::Source,
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
    let known_context = from_row(account, &hash("6"), 7);
    let incoming_context = from_row(account, &hash("7"), 8);
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
        panic!("a fingerprint match across different documents must become a hint");
    };
    assert_eq!(
        level,
        DedupLevel::Probabilistic,
        "the owner must see the probabilistic level"
    );
    assert_eq!(
        level.number(),
        5,
        "the level number is part of the decision explanation"
    );
}

#[test]
fn the_fingerprint_ignores_the_keys_that_name_the_submission() {
    // The idempotency key identifies the submission, not the operation: one and the same
    // An operation sent with different keys must produce the same
    // fingerprint—otherwise level 3 will catch nothing.
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

/// Canonical form used to compute the fingerprint.
///
/// Written here verbatim and **manually**: deduplication has already been performed against it,
/// and silently changing the form would devalue all previous fingerprints.
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
    // The value was computed independently of the program (§15.5):
    //
    //   printf '%s' "$FROZEN_CANONICAL" | sha256sum
    //
    // If this test fails, the canonical form has changed, and
    // previous fingerprints no longer match the new ones. Fixing it
    // by substituting a new value is not allowed: the format is versioned by
    // `v`, and changing the form must bump the version.
    let operation = deposit(AccountId(Uuid::from_u128(1)), 100_000);
    assert_eq!(
        fingerprint(&operation).as_str(),
        "366eff5d4b6007730b75786c85d7d9fc73d317e48b1df5c71545dafa3eb3831c"
    );
}
