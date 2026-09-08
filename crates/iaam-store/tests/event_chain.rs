//! One operation's correction chain, entered from anywhere in it.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::evidence::IdentityScope;
use iaam_store::SqliteStore;
use iaam_store::events::{CHAIN_STEP_BACK_SQL, CHAIN_STEP_FORWARD_SQL, RecordedEvent};
use rusqlite::params;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::date;

struct Ctx {
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
}

impl Ctx {
    fn new() -> Self {
        Self {
            owner: OwnerId::new_random(),
            account: AccountId::new_random(),
            source: SourceId::new_random(),
        }
    }

    /// A standalone fact: the shape everything in this file is a correction of.
    fn deposit(&self, sequence: u32) -> Event {
        let amount = Money::new(PostedMinor::new(100_000), CurrencyCode::Rub);
        let day = date!(2026 - 02 - 01);
        Event {
            id: EventId::new_random(),
            owner: self.owner,
            account: self.account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(self.account, amount)],
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&format!("{sequence:064x}")).expect("a raw hash"),
                ParserVersion("manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn reversal_of(&self, target: &Event, sequence: u32) -> Event {
        Event {
            relation: Relation::Reversal { target: target.id },
            ..self.deposit(sequence)
        }
    }

    fn replacement_of(&self, target: &Event, sequence: u32) -> Event {
        Event {
            relation: Relation::Replacement { target: target.id },
            ..self.deposit(sequence)
        }
    }
}

fn write(store: &mut SqliteStore, event: &Event) {
    store
        .append_event(event, IdentityScope::Source)
        .expect("the journal accepts the fact");
}

fn ids(chain: &[RecordedEvent]) -> Vec<EventId> {
    chain.iter().map(|recorded| recorded.event.id).collect()
}

/// One correction is a reversal and a replacement of the same target, so a
/// chain corrected twice holds five facts. Returns them in the order the walk
/// must produce: the original, then each act's reversal before its replacement.
fn corrected_twice(store: &mut SqliteStore, ctx: &Ctx) -> Vec<Event> {
    let original = ctx.deposit(1);
    let first_reversal = ctx.reversal_of(&original, 2);
    let first_replacement = ctx.replacement_of(&original, 3);
    let second_reversal = ctx.reversal_of(&first_replacement, 4);
    let second_replacement = ctx.replacement_of(&first_replacement, 5);

    for event in [
        &original,
        &first_reversal,
        &first_replacement,
        &second_reversal,
        &second_replacement,
    ] {
        write(store, event);
    }

    vec![
        original,
        first_reversal,
        first_replacement,
        second_reversal,
        second_replacement,
    ]
}

/// The identifier the owner most often holds is the one a correction just
/// handed him, not the one the import gave him — and a reversal is an
/// identifier he holds too. All three name the same operation, so all three
/// must answer with the same history; otherwise which handle he happens to
/// have decides which question he is allowed to ask.
#[test]
fn every_identifier_in_a_chain_returns_the_same_chain() {
    let mut store = SqliteStore::open_in_memory().expect("a store");
    let ctx = Ctx::new();
    let written = corrected_twice(&mut store, &ctx);
    let expected: Vec<EventId> = written.iter().map(|event| event.id).collect();

    for entered in &written {
        let chain = store
            .event_chain(ctx.owner, entered.id)
            .expect("a chain the journal can be walked into");
        assert_eq!(
            ids(&chain),
            expected,
            "entering by {} answered a different question",
            entered.id.inner()
        );
    }
}

/// A fact nobody ever corrected still has a history: it arrived, and that is
/// the whole of it. Answering with nothing would be indistinguishable from
/// answering about an event that is not his.
#[test]
fn a_fact_nothing_ever_touched_is_a_chain_of_one() {
    let mut store = SqliteStore::open_in_memory().expect("a store");
    let ctx = Ctx::new();
    let untouched = ctx.deposit(1);
    write(&mut store, &untouched);

    let chain = store
        .event_chain(ctx.owner, untouched.id)
        .expect("a chain of one");

    assert_eq!(ids(&chain), vec![untouched.id]);
    OffsetDateTime::parse(&chain[0].recorded_at, &Rfc3339)
        .expect("the moment this instance wrote the fact down parses back as RFC 3339");
}

/// The walk is two prepared statements and nothing else, so the whole of its
/// cost is how the planner answers those two. Both are proved here against the
/// very SQL the walk runs, rather than a copy of it: a copy would keep passing
/// while the walk turned into a full scan of the journal.
#[test]
fn a_chain_costs_index_lookups_and_never_scans_the_journal() {
    let mut store = SqliteStore::open_in_memory().expect("a store");
    let ctx = Ctx::new();
    let written = corrected_twice(&mut store, &ctx);

    let forward: String = store
        .connection()
        .query_row(
            &format!("EXPLAIN QUERY PLAN {CHAIN_STEP_FORWARD_SQL}"),
            params![
                ctx.owner.inner().to_string(),
                written[0].id.inner().to_string()
            ],
            |row| row.get(3),
        )
        .expect("a query plan for the forward step");
    assert!(
        forward.contains("events_by_relation_target"),
        "looking forward must use the index migration 0030 added: {forward}"
    );
    assert!(
        !forward.contains("SCAN"),
        "looking forward must not read the journal through: {forward}"
    );

    let backward: String = store
        .connection()
        .query_row(
            &format!("EXPLAIN QUERY PLAN {CHAIN_STEP_BACK_SQL}"),
            params![
                ctx.owner.inner().to_string(),
                written[0].id.inner().to_string()
            ],
            |row| row.get(3),
        )
        .expect("a query plan for the backward step");
    assert!(
        !backward.contains("SCAN"),
        "looking backward must not read the journal through: {backward}"
    );

    // And the walk assembles the whole chain out of those two statements and
    // nothing else: two corrections are five facts and five come back. What
    // the walk costs is therefore some number of the lookups the plans above
    // were just shown to be — the number is not counted here — and what it
    // returns is bounded by the chain rather than by the journal.
    let chain = store
        .event_chain(ctx.owner, written[4].id)
        .expect("a chain");
    assert_eq!(chain.len(), 5);
}

/// `resolve` refuses to write a chain that closes on itself, so this database
/// is corrupt. The walk must say so: returning the part of it that happens to
/// be reachable would hand the owner a fragment of his history presented as
/// the whole of it, and he has no way to tell.
#[test]
fn a_chain_that_closes_on_itself_is_refused_rather_than_walked_forever() {
    let mut store = SqliteStore::open_in_memory().expect("a store");
    let ctx = Ctx::new();

    let mut first = ctx.deposit(1);
    let mut second = ctx.deposit(2);
    first.relation = Relation::Replacement { target: second.id };
    second.relation = Relation::Replacement { target: first.id };
    write(&mut store, &first);
    write(&mut store, &second);

    let refusal = store
        .event_chain(ctx.owner, first.id)
        .expect_err("a cycle is not a history");

    let said = refusal.to_string();
    assert!(
        said.contains(&first.id.inner().to_string())
            || said.contains(&second.id.inner().to_string()),
        "the refusal must name the fact the chain returned to: {said}"
    );
}

/// An event identifier is a UUID, and a UUID confers no right to read someone
/// else's journal. Holding one of another owner's must read exactly as holding
/// an identifier of nothing at all.
#[test]
fn an_event_of_another_owner_is_not_found() {
    let mut store = SqliteStore::open_in_memory().expect("a store");
    let mine = Ctx::new();
    let theirs = Ctx::new();
    let written = corrected_twice(&mut store, &theirs);

    for entered in &written {
        let chain = store
            .event_chain(mine.owner, entered.id)
            .expect("a lookup that finds nothing is not a failure");
        assert!(
            chain.is_empty(),
            "another owner's chain reached through {}",
            entered.id.inner()
        );
    }
}
