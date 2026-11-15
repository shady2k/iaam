//! ADR 0042 point 4 for corrections: a repeat of one correction is recognised
//! as the same command and answered as the first call answered it.
//!
//! What a retry must not do: write a second fact, refuse the state its own
//! first call created, or claim «normal» about a fact a later correction
//! removed. What a different command under one key must do: be refused.

use std::path::PathBuf;
use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::error::AppError;
use iaam_app::ports::{AccountView, Clock, Principal, Scope};
use iaam_app::scenarios::correction::{CorrectionOutcome, CorrectionRequest, correct_events};
use iaam_app::storage::SqliteStore;
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RuleSettlement};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{AccountId, ClassificationRuleId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_ingest::Verdict;
use iaam_ingest::classification::Classification;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind, SubmittedOperation};
use time::macros::{date, time};
use uuid::Uuid;

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> time::Date {
        date!(2026 - 08 - 31)
    }
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "iaam-app-correction-retry-{label}-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("temporary directory");
        Self(path)
    }

    fn database(&self) -> PathBuf {
        self.0.join("iaam.sqlite")
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn services(database: &std::path::Path) -> AppServices {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open(database).expect("create database"),
    ));
    AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        Arc::new(FixedClock),
    )
}

fn principal(owner: OwnerId, token_id: Uuid) -> Principal {
    Principal {
        token_id,
        owner,
        scope: Scope::Owner,
    }
}

/// A cash deposit filed by a standing rule, on `account`.
///
/// The rule makes the standing-decision notice part of every answer about this
/// row, and the second row filed by the same rule keeps that notice's count
/// from being zero by accident.
fn deposit(
    owner: OwnerId,
    account: AccountId,
    key: &str,
    rule: ClassificationRuleId,
    rule_version: u32,
) -> Event {
    let money = Money::new(PostedMinor::new(10_000), CurrencyCode::Rub);
    Event {
        id: EventId::new_random(),
        owner,
        account,
        kind: EventKind::CashIn { amount: money },
        dates: EventDates::for_cash(CashPostedDate(date!(2026 - 08 - 01))),
        order: EffectiveOrder::new(date!(2026 - 08 - 01), 1),
        legs: vec![Leg::cash(account, money)],
        provenance: Provenance::new(
            SourceId::declared(owner, account, "file"),
            RawHash::parse(&"d".repeat(64)).expect("invented hash"),
            ParserVersion("correction-retry/1".to_owned()),
        )
        .with_rule_settlement(RuleSettlement::Rule {
            rule,
            version: rule_version,
        }),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: Some(key.to_owned()),
    }
}

/// One owner, two accounts, one rule, and the two deposits the rule filed.
async fn seed(services: &AppServices, owner: OwnerId) -> (AccountId, AccountId, EventId) {
    let account = AccountId::new_random();
    let other = AccountId::new_random();
    for (id, title) in [(account, "Main"), (other, "Savings")] {
        services
            .store
            .upsert_account(
                owner,
                AccountView {
                    id,
                    title: title.to_owned(),
                    institution: Some("Invented Bank".to_owned()),
                },
            )
            .await
            .expect("seed account");
    }
    let rule = ClassificationRuleId::new_random();
    let corrected = deposit(owner, account, "correction-retry-target", rule, 3);
    let sibling = deposit(owner, other, "correction-retry-sibling", rule, 3);
    services
        .store
        .append_events(vec![corrected.clone(), sibling], IdentityScope::Source)
        .await
        .expect("seed the journal");
    (account, other, corrected.id)
}

fn reclassify(target: EventId, to: AccountId) -> CorrectionRequest {
    CorrectionRequest::Reclassification {
        target,
        classification: Classification::InternalTransfer { to },
    }
}

/// A replacement of `target` with a fresh deposit stated at `at`.
fn replace(target: EventId, account: AccountId, key: &str, at: time::Time) -> CorrectionRequest {
    CorrectionRequest::Replacement {
        target,
        operation: Box::new(SubmittedOperation {
            account,
            kind: OperationKind::Deposit {
                amount_minor: 10_000,
                currency: CurrencyCode::Rub,
            },
            dates: OperationDates {
                trade: None,
                settled: None,
                cash_posted: Some(date!(2026 - 08 - 01)),
                paid: None,
            },
            source_time: Some(at),
            idempotency_key: Some(key.to_owned()),
            source_operation_id: None,
            source_position_id: None,
            source_category: None,
            owner_category: None,
            source_code: None,
            source_kind: None,
            description: None,
            counterparty: None,
        }),
    }
}

async fn journal(services: &AppServices, owner: OwnerId) -> Vec<Event> {
    services
        .store
        .load_events_through(owner, time::Date::MAX)
        .await
        .expect("read the journal")
}

/// What one correction answered, without the identifiers a fresh attempt
/// generates for facts it writes: the verdict's kind, and the standing notice.
fn answer_shape(
    outcome: &CorrectionOutcome,
) -> (String, Option<(ClassificationRuleId, u32, usize)>) {
    let kind = match &outcome.verdict {
        Verdict::Provisional { .. } => "provisional".to_owned(),
        Verdict::Duplicate { .. } => "duplicate".to_owned(),
        other => format!("{other:?}"),
    };
    (
        kind,
        outcome
            .standing_rule
            .map(|standing| (standing.rule, standing.version, standing.still_filed)),
    )
}

#[tokio::test]
async fn a_retry_under_another_token_is_the_same_command() {
    let directory = TempDirectory::new("other-token");
    let services = services(&directory.database());
    let owner = OwnerId::new_random();
    let (_, other, target) = seed(&services, owner).await;

    let first_token = Uuid::new_v4();
    let first = correct_events(
        &services,
        &principal(owner, first_token),
        true,
        &[reclassify(target, other)],
    )
    .await
    .expect("the first correction commits");
    assert!(
        matches!(first[0].verdict, Verdict::Provisional { .. }),
        "{first:?}"
    );
    let written = journal(&services, owner).await.len();

    // A rotated token, or the owner's own after the agent's: who presented the
    // command is not part of it (ADR 0021, ADR 0042 point 4).
    let second = correct_events(
        &services,
        &principal(owner, Uuid::new_v4()),
        true,
        &[reclassify(target, other)],
    )
    .await
    .expect("the same command under another token is recognised");
    assert_eq!(
        second[0].verdict, first[0].verdict,
        "the retry answers what the first call answered"
    );
    assert_eq!(
        journal(&services, owner).await.len(),
        written,
        "the retry writes nothing"
    );
}

#[tokio::test]
async fn one_key_with_two_source_times_is_two_commands_and_is_refused() {
    let directory = TempDirectory::new("source-time");
    let services = services(&directory.database());
    let owner = OwnerId::new_random();
    let (account, _, target) = seed(&services, owner).await;
    let token = Uuid::new_v4();
    let key = "correction-retry-replacement";

    let first = correct_events(
        &services,
        &principal(owner, token),
        true,
        &[replace(target, account, key, time!(10:00))],
    )
    .await
    .expect("the first replacement commits");
    assert!(
        matches!(first[0].verdict, Verdict::Provisional { .. }),
        "{first:?}"
    );
    let written = journal(&services, owner).await.len();

    // The same key, the same amount and the same day, stated at another moment:
    // the source's time of day is what the command says, and is not in the
    // fingerprint that a key stands for.
    let refused = correct_events(
        &services,
        &principal(owner, token),
        true,
        &[replace(target, account, key, time!(11:00))],
    )
    .await
    .expect_err("a different command under one key is refused, not answered as a retry");
    assert!(
        matches!(refused, AppError::Conflict { .. }),
        "expected a conflict, got {refused:?}"
    );
    assert_eq!(
        journal(&services, owner).await.len(),
        written,
        "the refusal writes nothing"
    );
}

#[tokio::test]
async fn a_retry_of_a_key_whose_fact_was_reversed_is_refused_with_the_remedy() {
    let directory = TempDirectory::new("reversed-key");
    let services = services(&directory.database());
    let owner = OwnerId::new_random();
    let (account, _, target) = seed(&services, owner).await;
    let token = Uuid::new_v4();
    let key = "correction-retry-reversed";

    let first = correct_events(
        &services,
        &principal(owner, token),
        true,
        &[replace(target, account, key, time!(10:00))],
    )
    .await
    .expect("the first replacement commits");
    let replacement = match &first[0].verdict {
        Verdict::Provisional { event } => *event,
        other => panic!("the first answer records the replacement: {other:?}"),
    };

    // The fact under the key is retracted afterwards, which is what makes a
    // later repeat of the same correction a question rather than a retry.
    correct_events(
        &services,
        &principal(owner, token),
        true,
        &[CorrectionRequest::Reversal {
            target: replacement,
        }],
    )
    .await
    .expect("the replacement is reversed");
    let written = journal(&services, owner).await.len();

    let refused = correct_events(
        &services,
        &principal(owner, token),
        true,
        &[replace(target, account, key, time!(10:00))],
    )
    .await
    .expect_err("a key whose fact was withdrawn still gets the refusal of the write path");
    match refused {
        AppError::Conflict { what } => {
            assert!(what.contains("withdrawn"), "{what}");
            assert!(what.contains("relation: replacement"), "{what}");
        }
        other => panic!("expected a conflict with its remedy, got {other:?}"),
    }
    assert_eq!(
        journal(&services, owner).await.len(),
        written,
        "the refusal writes nothing"
    );
}

#[tokio::test]
async fn a_retry_answers_duplicate_where_the_first_call_answered_duplicate() {
    let directory = TempDirectory::new("duplicate-answer");
    let services = services(&directory.database());
    let owner = OwnerId::new_random();
    let (_, other, target) = seed(&services, owner).await;
    let token = Uuid::new_v4();
    let principal = principal(owner, token);

    // One batch naming the same fact twice: the first reversal is written and
    // the second is the duplicate the store reports.
    let batch = || {
        [
            reclassify(target, other),
            CorrectionRequest::Reversal { target },
        ]
    };
    let first = correct_events(&services, &principal, true, &batch())
        .await
        .expect("the batch commits");
    assert!(
        matches!(first[0].verdict, Verdict::Provisional { .. }),
        "the reclassification's replacement is written: {first:?}"
    );
    assert!(
        matches!(first[1].verdict, Verdict::Duplicate { .. }),
        "the second reversal of the target finds the first one's fact: {first:?}"
    );
    let written = journal(&services, owner).await.len();

    let retried = correct_events(&services, &principal, true, &batch())
        .await
        .expect("the identical retry is answered, not refused");
    assert_eq!(
        retried.iter().map(answer_shape).collect::<Vec<_>>(),
        first.iter().map(answer_shape).collect::<Vec<_>>(),
        "the retry answers what the first call answered, the standing notice included"
    );
    assert_eq!(
        journal(&services, owner).await.len(),
        written,
        "the retry writes nothing"
    );
}

/// A key an ordinary fact holds is not this correction's retry. The retry path
/// leaves it to the write path, whose refusal names the event that holds it —
/// answering «provisional» here would report a write the caller's key did not
/// produce.
#[tokio::test]
async fn a_key_held_by_an_ordinary_fact_is_refused_rather_than_answered_as_a_retry() {
    let directory = TempDirectory::new("occupied-by-ordinary");
    let services = services(&directory.database());
    let owner = OwnerId::new_random();
    let (account, _, target) = seed(&services, owner).await;
    let before = journal(&services, owner).await;

    // The seed's deposit is an ordinary fact under this key: no relation, so
    // it is not the replacement the request describes.
    let key = "correction-retry-target";
    let refused = correct_events(
        &services,
        &principal(owner, Uuid::new_v4()),
        true,
        &[replace(target, account, key, time!(10:00))],
    )
    .await
    .expect_err("a key an ordinary fact holds is occupied, not this correction's");

    let AppError::Conflict { what } = refused else {
        panic!("expected a conflict, got {refused:?}");
    };
    let holder = before
        .iter()
        .find(|event| event.idempotency_key.as_deref() == Some(key))
        .expect("an ordinary fact holds the key");
    assert!(what.contains("not this correction"), "{what}");
    assert!(
        what.contains(&holder.id.inner().to_string()),
        "the refusal names the event that holds the key: {what}"
    );
    assert_eq!(
        journal(&services, owner).await.len(),
        before.len(),
        "the refusal writes nothing"
    );
}
