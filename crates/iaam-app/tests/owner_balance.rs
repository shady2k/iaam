//! What the owner's own balance statement is allowed to overwrite (iaam-ihyi).
//!
//! Every test here is about the idempotency key `record_owner_balance` stamps.
//! The key decides which two submissions are one fact, and the whole point of
//! the scenario — the owner telling the system what a statement says — is lost
//! if two different statements are folded into one.

use std::collections::BTreeSet;
use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{Clock, Principal, Recorded, Scope};
use iaam_app::scenarios::reconciliation::{
    OWNER_STATED_CHANNEL, OwnerBalance, record_owner_balance,
};
use iaam_core::event::provenance::RawHash;
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint};
use iaam_store::SqliteStore;
use rust_decimal::Decimal;
use time::Date;
use time::macros::date;
use uuid::Uuid;

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> Date {
        date!(2026 - 03 - 31)
    }
}

struct Ctx {
    services: AppServices,
    principal: Principal,
}

fn harness() -> Ctx {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}")),
    ));
    Ctx {
        services: AppServices::new(
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            Arc::new(FixedClock),
        ),
        principal: Principal {
            token_id: Uuid::new_v4(),
            owner: OwnerId::new_random(),
            scope: Scope::Owner,
        },
    }
}

fn march() -> AssertionPeriod {
    AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
        .unwrap_or_else(|| unreachable!("March is not an inverted interval"))
}

/// The same document hash for every submission on purpose.
///
/// `POST /v1/reconciliation/balance` accepts `source_hash` as optional and
/// substitutes a constant when it is absent, so the ordinary case is exactly
/// this: several statements that differ in nothing the provenance records. If
/// the key needed the hash to tell them apart, the API's own default would
/// defeat it.
fn stated() -> RawHash {
    RawHash::parse(&"a".repeat(64)).unwrap_or_else(|| unreachable!("64 hex characters"))
}

fn inserted(recorded: &[Recorded]) -> Vec<EventId> {
    recorded
        .iter()
        .filter_map(|item| match item {
            Recorded::Inserted { id } => Some(*id),
            Recorded::Duplicate { .. } => None,
        })
        .collect()
}

fn cash_balance(account: AccountId, at: BalancePoint, minor: i64) -> OwnerBalance {
    OwnerBalance {
        account,
        period: march(),
        at,
        cash: Some((CurrencyCode::Rub, PostedMinor::new(minor))),
        positions: Vec::new(),
        raw_hash: stated(),
    }
}

#[tokio::test]
async fn an_opening_and_a_closing_claim_are_two_facts() {
    // Both claims name one account and one interval, and they say different
    // things: what the account held before March and what it held after. A key
    // blind to the balance point answered the second with the first event, so
    // the closing figure never reached the journal and the scoped query that
    // asked for it found the opening one.
    let ctx = harness();
    let account = AccountId::new_random();

    let opening = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        cash_balance(account, BalancePoint::Opening, 100_000),
    )
    .await
    .unwrap_or_else(|error| panic!("opening claim: {error:?}"));
    let closing = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        cash_balance(account, BalancePoint::Closing, 125_000),
    )
    .await
    .unwrap_or_else(|error| panic!("closing claim: {error:?}"));

    let opening = inserted(&opening);
    let closing = inserted(&closing);
    assert_eq!(opening.len(), 1, "the opening claim was not recorded");
    assert_eq!(
        closing.len(),
        1,
        "the closing claim was deduplicated against the opening one"
    );
    assert_ne!(opening[0], closing[0]);

    let recorded = ctx
        .services
        .store
        .list_control_assertions(ctx.principal.owner, account)
        .await
        .unwrap_or_else(|error| panic!("list assertions: {error:?}"));
    let points: Vec<Option<BalancePoint>> = recorded.iter().map(|item| item.point).collect();
    assert!(
        points.contains(&Some(BalancePoint::Opening))
            && points.contains(&Some(BalancePoint::Closing)),
        "both balance points must be visible to a scoped query, saw {points:?}"
    );
}

#[tokio::test]
async fn every_claim_of_one_call_is_recorded() {
    // One call states four facts: how much cash, and how much of each of three
    // holdings. Under a key that named only the account and the interval they
    // shared one, so the first won and the rest were silently answered with it.
    let ctx = harness();
    let account = AccountId::new_random();
    let positions: Vec<(InstrumentId, CustodyId)> = (0..3)
        .map(|_| (InstrumentId::new_random(), CustodyId::new_random()))
        .collect();

    let recorded = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        OwnerBalance {
            account,
            period: march(),
            at: BalancePoint::Closing,
            cash: Some((CurrencyCode::Rub, PostedMinor::new(75_000))),
            positions: positions
                .iter()
                .enumerate()
                .map(|(index, (instrument, custody))| {
                    (
                        *instrument,
                        *custody,
                        Quantity(Dec::new(Decimal::from(10 + index as i64))),
                    )
                })
                .collect(),
            raw_hash: stated(),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("balance: {error:?}"));

    let ids = inserted(&recorded);
    assert_eq!(
        ids.len(),
        4,
        "cash and three positions are four facts, recorded {recorded:?}"
    );
    let mut distinct = ids.clone();
    distinct.sort_unstable_by_key(EventId::inner);
    distinct.dedup();
    assert_eq!(distinct.len(), 4, "one event was reported twice");
}

#[tokio::test]
async fn two_positions_in_one_call_are_not_one_position() {
    // The narrowest form of the same defect: same account, same interval, same
    // balance point, same kind of claim. Only the instrument and the custody
    // separate them, so only a key that names both keeps them apart.
    let ctx = harness();
    let account = AccountId::new_random();
    let custody = CustodyId::new_random();

    let recorded = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        OwnerBalance {
            account,
            period: march(),
            at: BalancePoint::Closing,
            cash: None,
            positions: vec![
                (
                    InstrumentId::new_random(),
                    custody,
                    Quantity(Dec::new(Decimal::from(5))),
                ),
                (
                    InstrumentId::new_random(),
                    custody,
                    Quantity(Dec::new(Decimal::from(7))),
                ),
            ],
            raw_hash: stated(),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("positions: {error:?}"));

    assert_eq!(inserted(&recorded).len(), 2, "recorded {recorded:?}");
}

#[tokio::test]
async fn restating_one_claim_is_still_one_fact() {
    // The guard on the fix. Idempotency is the reason the key exists: a client
    // that did not see the response must be able to retry without writing the
    // statement twice. Splitting the key by balance point and by dimension must
    // not turn a retry into a second event.
    let ctx = harness();
    let account = AccountId::new_random();

    let first = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        cash_balance(account, BalancePoint::Closing, 125_000),
    )
    .await
    .unwrap_or_else(|error| panic!("first submission: {error:?}"));
    let retry = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        cash_balance(account, BalancePoint::Closing, 125_000),
    )
    .await
    .unwrap_or_else(|error| panic!("retry: {error:?}"));

    let first = inserted(&first);
    assert_eq!(first.len(), 1);
    assert_eq!(
        retry,
        vec![Recorded::Duplicate { existing: first[0] }],
        "a retry must be answered with the event it repeats"
    );
}

#[tokio::test]
async fn one_period_does_not_answer_for_another() {
    // The part of the key that was already right, kept under test so that
    // narrowing it later is a failure rather than a silent regression.
    let ctx = harness();
    let account = AccountId::new_random();

    let march_claim = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        cash_balance(account, BalancePoint::Closing, 125_000),
    )
    .await
    .unwrap_or_else(|error| panic!("March: {error:?}"));
    let april_claim = record_owner_balance(
        &ctx.services,
        &ctx.principal,
        OwnerBalance {
            period: AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30))
                .unwrap_or_else(|| unreachable!("April is not an inverted interval")),
            ..cash_balance(account, BalancePoint::Closing, 130_000)
        },
    )
    .await
    .unwrap_or_else(|error| panic!("April: {error:?}"));

    assert_eq!(inserted(&march_claim).len(), 1);
    assert_eq!(inserted(&april_claim).len(), 1);
}

#[tokio::test]
async fn every_claim_about_one_account_arrives_under_one_source() {
    // The defect (iaam-eq2i): each call minted `SourceId::new_random()`, so the
    // opening and the closing statement about one interval — two calls since the
    // key gained the balance point — carried two source identities. The core
    // groups control assertions by (account, period, channel), and the channel
    // holds the source, so one statement about one month became two
    // `StatementGroup`s.
    let ctx = harness();
    let account = AccountId::new_random();

    for at in [BalancePoint::Opening, BalancePoint::Closing] {
        record_owner_balance(
            &ctx.services,
            &ctx.principal,
            cash_balance(account, at, 100_000),
        )
        .await
        .unwrap_or_else(|error| panic!("{at:?} claim: {error:?}"));
    }

    let sources: BTreeSet<SourceId> = ctx
        .services
        .store
        .load_events_through(ctx.principal.owner, Date::MAX)
        .await
        .unwrap_or_else(|error| panic!("journal: {error:?}"))
        .iter()
        .map(|event| event.provenance.source())
        .collect();
    assert_eq!(
        sources,
        BTreeSet::from([SourceId::declared(
            ctx.principal.owner,
            account,
            OWNER_STATED_CHANNEL
        )]),
        "the owner's word about one account is one source, derived from it"
    );
}

#[tokio::test]
async fn two_accounts_are_two_sources() {
    // The other half of the key. A single derived identity for every
    // owner-stated fact would put two accounts' statements in one source, and
    // deduplication is scoped by the source.
    let ctx = harness();
    let main = AccountId::new_random();
    let savings = AccountId::new_random();

    assert_ne!(
        SourceId::declared(ctx.principal.owner, main, OWNER_STATED_CHANNEL),
        SourceId::declared(ctx.principal.owner, savings, OWNER_STATED_CHANNEL),
    );

    for account in [main, savings] {
        record_owner_balance(
            &ctx.services,
            &ctx.principal,
            cash_balance(account, BalancePoint::Closing, 100_000),
        )
        .await
        .unwrap_or_else(|error| panic!("claim: {error:?}"));
    }

    let sources: BTreeSet<SourceId> = ctx
        .services
        .store
        .load_events_through(ctx.principal.owner, Date::MAX)
        .await
        .unwrap_or_else(|error| panic!("journal: {error:?}"))
        .iter()
        .map(|event| event.provenance.source())
        .collect();
    assert_eq!(sources.len(), 2, "one source per account, and no more");
}

#[tokio::test]
async fn a_restated_month_does_not_become_a_second_statement() {
    // What the derivation is worth: two months of one account are two periods
    // and one source, so the second month cannot look like an independent
    // channel confirming the first. The period is deliberately not in the source
    // key — it is carried by the assertion, and grouping separates by it.
    let ctx = harness();
    let account = AccountId::new_random();

    record_owner_balance(
        &ctx.services,
        &ctx.principal,
        cash_balance(account, BalancePoint::Closing, 125_000),
    )
    .await
    .unwrap_or_else(|error| panic!("March: {error:?}"));
    record_owner_balance(
        &ctx.services,
        &ctx.principal,
        OwnerBalance {
            period: AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30))
                .unwrap_or_else(|| unreachable!("April is not an inverted interval")),
            ..cash_balance(account, BalancePoint::Closing, 130_000)
        },
    )
    .await
    .unwrap_or_else(|error| panic!("April: {error:?}"));

    let events = ctx
        .services
        .store
        .load_events_through(ctx.principal.owner, Date::MAX)
        .await
        .unwrap_or_else(|error| panic!("journal: {error:?}"));
    assert_eq!(events.len(), 2, "two months, two facts");
    let sources: BTreeSet<SourceId> = events
        .iter()
        .map(|event| event.provenance.source())
        .collect();
    assert_eq!(sources.len(), 1, "two months, one source");
}
