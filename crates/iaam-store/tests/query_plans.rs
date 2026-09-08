//! Query-plan assertions for the journal's phase-1 SQL (spec §6.2, Task 8).
//!
//! Every filter this task ported off SQLite's JSON-path extraction and
//! iteration functions must now be answered by the index the spec names for
//! it, not by a full scan of `events` or `event_legs`. Each test here runs
//! `EXPLAIN QUERY PLAN` over the exact SQL [`journal_sql`] produces — never a
//! copy of it, which would prove only that the copy is indexed while the
//! real query drifted into a scan unnoticed.
//!
//! Keyset pagination is tested separately, over a journal that deliberately
//! shares one `effective_date` across several events — the case the cursor
//! exists for (spec §6, §4.8).

use iaam_core::category::CategoryMatcher;
use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash, RuleSettlement};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{
    AccountId, CategoryId, ClassificationRuleId, EventId, ImportId, ImportSessionId, OwnerId,
    SourceId,
};
use iaam_core::money::{CurrencyCode, Money, PostedMinor};
use iaam_core::reconciliation::evidence::IdentityScope;
use iaam_store::SqliteStore;
use iaam_store::categories::NewCategoryRule;
use iaam_store::journal::query::journal_sql;
use iaam_store::journal::{JournalCursor, JournalQuery};
use iaam_store::reference::AccountRecord;
use time::macros::date;

/// A fixture journal wide enough that each filter has both matching and
/// non-matching rows — a query with nothing to exclude tells the planner
/// nothing about which index is worth using.
struct Fixture {
    store: SqliteStore,
    owner: OwnerId,
    account: AccountId,
    other_account: AccountId,
    source: SourceId,
    other_source: SourceId,
    import: ImportId,
    import_session: ImportSessionId,
    rule: ClassificationRuleId,
    category: CategoryId,
}

impl Fixture {
    fn build() -> Self {
        let mut store = SqliteStore::open_in_memory().expect("in-memory store");
        let owner = OwnerId::new_random();
        let account = AccountId::new_random();
        let other_account = AccountId::new_random();
        let source = SourceId::new_random();
        let other_source = SourceId::new_random();
        let import = ImportId::new_random();
        let import_session = ImportSessionId::new_random();
        let rule = ClassificationRuleId::new_random();

        store
            .upsert_account(&AccountRecord {
                id: account,
                owner,
                title: "Main".to_owned(),
                institution: Some("Test Bank".to_owned()),
            })
            .expect("account");
        store
            .upsert_account(&AccountRecord {
                id: other_account,
                owner,
                title: "Savings".to_owned(),
                institution: Some("Test Bank".to_owned()),
            })
            .expect("other account");

        let category_group = store
            .insert_category_group(owner, "Spending")
            .expect("category group");
        let category = CategoryId(
            store
                .insert_category(owner, category_group, "Groceries")
                .expect("category"),
        );
        store
            .insert_category_rule(
                owner,
                NewCategoryRule {
                    matcher: CategoryMatcher::DescriptionContains {
                        text: "Corner Shop".to_owned(),
                    },
                    category: category.inner(),
                    valid_from: None,
                    valid_to: None,
                },
                None,
            )
            .expect("category rule");

        let mut fixture = Self {
            store,
            owner,
            account,
            other_account,
            source,
            other_source,
            import,
            import_session,
            rule,
            category,
        };
        fixture.populate();
        fixture
    }

    fn cash_in(&self, sequence: u32, day: time::Date, currency: CurrencyCode) -> Event {
        let amount = Money::new(PostedMinor::new(1_000), currency);
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
                RawHash::parse(&format!("{sequence:064x}")).expect("hash"),
                ParserVersion("query-plans/1".to_owned()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    /// A transfer to `other_account` — the only way this fixture's journal
    /// touches an account it did not record the event against.
    fn transfer(&self, sequence: u32, day: time::Date) -> Event {
        self.transfer_to(self.other_account, sequence, day)
    }

    /// A transfer to an arbitrary far account, so `event_cash_transfer` holds
    /// enough distinct account pairs that filtering by exactly one of them is
    /// a choice worth indexing — a table with one row tells the planner
    /// nothing about which access path pays off.
    fn transfer_to(&self, to: AccountId, sequence: u32, day: time::Date) -> Event {
        let amount = Money::new(PostedMinor::new(2_000), CurrencyCode::Rub);
        Event {
            id: EventId::new_random(),
            owner: self.owner,
            account: self.account,
            kind: EventKind::CashTransfer {
                transfer_id: iaam_core::ids::TransferId::new_random(),
                from: self.account,
                to,
                amount,
            },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![
                Leg::cash(
                    self.account,
                    Money::new(PostedMinor::new(-2_000), CurrencyCode::Rub),
                ),
                Leg::cash(to, amount),
            ],
            provenance: Provenance::new(
                self.source,
                RawHash::parse(&format!("{sequence:064x}")).expect("hash"),
                ParserVersion("query-plans/1".to_owned()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn append(&mut self, event: Event) {
        self.store
            .append_event(&event, IdentityScope::Source)
            .expect("event accepted");
    }

    /// A hundred plain deposits (RUB, `source`, no import/session/rule), plus
    /// a handful of rows each filter can select apart from that background:
    /// a USD leg, a transfer touching `other_account`, an event from
    /// `other_source`, one carrying `import`, one carrying `import_session`,
    /// one `settled_by_rule`, and one of a different kind.
    fn populate(&mut self) {
        let base = date!(2026 - 03 - 01);
        for day_offset in 0..25u32 {
            let day = base + time::Duration::days(i64::from(day_offset));
            for slot in 0..4u32 {
                let sequence = day_offset * 4 + slot + 1;
                self.append(self.cash_in(sequence, day, CurrencyCode::Rub));
            }
        }

        let far_day = base + time::Duration::days(30);
        self.append(self.cash_in(9001, far_day, CurrencyCode::Usd));
        self.append(self.transfer(9002, far_day));

        // Nine more transfers to nine other far accounts: one row in
        // `event_cash_transfer` tells the planner nothing about whether
        // indexing pays off, since any access path is free on a single row.
        // With ten distinct far accounts, filtering to exactly one of them
        // is a genuinely selective predicate.
        for slot in 0..9u32 {
            let far_account = AccountId::new_random();
            self.store
                .upsert_account(&AccountRecord {
                    id: far_account,
                    owner: self.owner,
                    title: format!("Far {slot}"),
                    institution: Some("Test Bank".to_owned()),
                })
                .expect("far account");
            self.append(self.transfer_to(far_account, 9100 + slot, far_day));
        }

        let mut other_source_event = self.cash_in(9003, far_day, CurrencyCode::Rub);
        other_source_event.provenance = Provenance::new(
            self.other_source,
            RawHash::parse(&"a".repeat(64)).expect("hash"),
            ParserVersion("query-plans/1".to_owned()),
        );
        self.append(other_source_event);

        let mut imported = self.cash_in(9004, far_day, CurrencyCode::Rub);
        imported.provenance = imported.provenance.with_import(self.import);
        self.append(imported);

        let mut sessioned = self.cash_in(9005, far_day, CurrencyCode::Rub);
        sessioned.provenance = sessioned
            .provenance
            .with_import_session(self.import_session);
        self.append(sessioned);

        let mut ruled = self.cash_in(9006, far_day, CurrencyCode::Rub);
        ruled.provenance = ruled.provenance.with_rule_settlement(RuleSettlement::Rule {
            rule: self.rule,
            version: 1,
        });
        self.append(ruled);

        self.append({
            let mut event = self.cash_in(9007, far_day, CurrencyCode::Rub);
            event.kind = EventKind::CashOut {
                amount: Money::new(PostedMinor::new(-1_000), CurrencyCode::Rub),
            };
            event.legs = vec![Leg::cash(
                self.account,
                Money::new(PostedMinor::new(-1_000), CurrencyCode::Rub),
            )];
            event
        });

        // A description matching the fixture's own rule, so `assign_for`
        // (called synchronously on append) gives it a row in `self.category`.
        // Every plain deposit above carries no description, so it stays
        // `NotDecomposed` — the background the `uncategorised` filter needs.
        let mut categorised = self.cash_in(9008, far_day, CurrencyCode::Rub);
        categorised.provenance = categorised
            .provenance
            .with_description("Corner Shop".to_owned());
        self.append(categorised);
    }
}

/// `EXPLAIN QUERY PLAN` over the exact SQL and parameters [`journal_sql`]
/// produced, collapsed into one string — a query with an `EXISTS`
/// sub-select produces one plan line per query level, and a filter is only
/// proven indexed when none of those lines is a full scan.
fn explain(store: &SqliteStore, query: &JournalQuery, owner: OwnerId) -> String {
    let (sql, parameters) = journal_sql(owner, query).expect("journal sql");
    let mut statement = store
        .connection()
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare plan");
    let mut rows = statement
        .query(rusqlite::params_from_iter(parameters.iter()))
        .expect("plan rows");
    let mut lines = Vec::new();
    while let Some(row) = rows.next().expect("plan row") {
        let detail: String = row.get(3).expect("detail column");
        lines.push(detail);
    }
    lines.join("\n")
}

fn no_full_scan(plan: &str) -> bool {
    !plan.contains("SCAN events") && !plan.contains("SCAN event_legs")
}

fn base_query() -> JournalQuery {
    JournalQuery {
        limit: 1_000,
        ..JournalQuery::default()
    }
}

#[test]
fn the_kind_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        kinds: vec!["cash_out".to_owned()],
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(plan.contains("events_by_kind"), "plan was: {plan}");
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

#[test]
fn the_account_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        account: Some(fixture.account),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(plan.contains("events_by_account"), "plan was: {plan}");
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

#[test]
fn the_source_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        source: Some(fixture.other_source),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(plan.contains("events_by_source"), "plan was: {plan}");
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

#[test]
fn the_import_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        import: Some(fixture.import),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(
        plan.contains("events_by_import") && !plan.contains("events_by_import_session"),
        "plan was: {plan}"
    );
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

#[test]
fn the_import_session_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        import_session: Some(fixture.import_session),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(
        plan.contains("events_by_import_session"),
        "plan was: {plan}"
    );
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

#[test]
fn the_settled_by_rule_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        settled_by_rule: Some(fixture.rule),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(
        plan.contains("events_by_settled_by_rule"),
        "plan was: {plan}"
    );
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

/// `currencies` used to be a `json_each` over `payload`'s legs; it is now an
/// `EXISTS` over `event_legs`, and the spec names `event_legs_by_currency`
/// as the index that answers it.
#[test]
fn the_currency_filter_uses_an_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        currencies: vec![CurrencyCode::Usd],
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(plan.contains("event_legs_by_currency"), "plan was: {plan}");
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

/// `touching` used to be the same `json_each` shape over each leg's
/// `account` path; it is now an `EXISTS` over `event_legs`, and the spec
/// names `event_legs_by_account` as the index for the leg branch.
#[test]
fn the_touching_filter_uses_an_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        touching: Some(fixture.other_account),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(plan.contains("event_legs_by_account"), "plan was: {plan}");
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

/// `counterparty` (Task 11, spec §6.1) is deliberately narrower than
/// `touching`: it matches only the far endpoint of a transfer, never the
/// near one, and it is answered by the `event_cash_transfer` indexes rather
/// than a scan of that table.
#[test]
fn the_counterparty_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        counterparty: Some(fixture.other_account),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(
        plan.contains("event_cash_transfer_by_from") || plan.contains("event_cash_transfer_by_to"),
        "plan was: {plan}"
    );
    assert!(no_full_scan(&plan), "plan was: {plan}");
    assert!(
        !plan.contains("SCAN event_cash_transfer"),
        "plan was: {plan}"
    );
}

/// `category` (Task 11, spec §4.7, §6.1) joins `event_category_assignments`,
/// answered by `event_category_assignments_by_category`.
#[test]
fn the_category_filter_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        category: Some(fixture.category),
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(
        plan.contains("event_category_assignments_by_category"),
        "plan was: {plan}"
    );
    assert!(no_full_scan(&plan), "plan was: {plan}");
    assert!(
        !plan.contains("SCAN event_category_assignments"),
        "plan was: {plan}"
    );
}

/// `uncategorised` is `NOT EXISTS` against the projection's own primary key
/// `(owner, event)` — never a sentinel category — and must not scan either
/// table to answer it.
#[test]
fn the_uncategorised_filter_does_not_scan_either_table() {
    let fixture = Fixture::build();
    let query = JournalQuery {
        uncategorised: true,
        ..base_query()
    };
    let plan = explain(&fixture.store, &query, fixture.owner);
    assert!(no_full_scan(&plan), "plan was: {plan}");
    assert!(
        !plan.contains("SCAN event_category_assignments"),
        "plan was: {plan}"
    );
}

/// The source-category projection used to be a path extraction of
/// `provenance.source_category`; it is now the `events.source_category`
/// column, answered by `events_by_source_category`.
#[test]
fn the_source_category_listing_uses_its_index_and_not_a_scan() {
    let fixture = Fixture::build();
    let mut statement = fixture
        .store
        .connection()
        .prepare(
            "EXPLAIN QUERY PLAN SELECT DISTINCT source_category FROM events \
             WHERE owner = ?1 AND source_category IS NOT NULL",
        )
        .expect("prepare plan");
    let mut rows = statement
        .query(rusqlite::params![fixture.owner.inner().to_string()])
        .expect("plan rows");
    let mut lines = Vec::new();
    while let Some(row) = rows.next().expect("plan row") {
        let detail: String = row.get(3).expect("detail column");
        lines.push(detail);
    }
    let plan = lines.join("\n");
    assert!(
        plan.contains("events_by_source_category"),
        "plan was: {plan}"
    );
    assert!(no_full_scan(&plan), "plan was: {plan}");
}

/// Keyset pagination returns each event exactly once, over a journal that
/// deliberately puts several events on one `effective_date` — the case the
/// `(effective_date, sequence)` cursor exists for (spec §6, §4.8).
#[test]
fn keyset_pagination_returns_each_event_exactly_once_across_pages() {
    let mut store = SqliteStore::open_in_memory().expect("in-memory store");
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let source = SourceId::new_random();
    store
        .upsert_account(&AccountRecord {
            id: account,
            owner,
            title: "Main".to_owned(),
            institution: None,
        })
        .expect("account");

    let day = date!(2026 - 04 - 01);
    let mut expected_ids = Vec::new();
    // Twelve events sharing one `effective_date`, distinguished only by
    // `sequence` — the shape a same-day cursor must not skip or repeat.
    for sequence in 1..=12u32 {
        let amount = Money::new(PostedMinor::new(1_000), CurrencyCode::Rub);
        let event = Event {
            id: EventId::new_random(),
            owner,
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                source,
                RawHash::parse(&format!("{sequence:064x}")).expect("hash"),
                ParserVersion("query-plans/1".to_owned()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        };
        expected_ids.push(event.id);
        store
            .append_event(&event, IdentityScope::Source)
            .expect("event accepted");
    }

    let page_size = 5u32;
    let mut cursor: Option<JournalCursor> = None;
    let mut collected: Vec<EventId> = Vec::new();
    loop {
        let query = JournalQuery {
            after: cursor,
            limit: page_size,
            ..JournalQuery::default()
        };
        let page = store
            .list_journal_events(owner, &query)
            .expect("journal page");
        if page.is_empty() {
            break;
        }
        for event in &page {
            collected.push(event.id);
        }
        let last = page.last().expect("non-empty page");
        cursor = Some(JournalCursor {
            effective_date: last.order.date(),
            sequence: last.order.sequence(),
        });
        if page.len() < page_size as usize {
            break;
        }
    }

    assert_eq!(
        collected.len(),
        expected_ids.len(),
        "every event must come back exactly once across pages: {collected:?}"
    );
    let mut sorted_collected = collected.clone();
    sorted_collected.sort();
    let mut sorted_expected = expected_ids.clone();
    sorted_expected.sort();
    assert_eq!(
        sorted_collected, sorted_expected,
        "the set of ids returned across pages must equal the set written"
    );
    // And in the declared order: `(effective_date, sequence)`, same day.
    assert_eq!(
        collected, expected_ids,
        "same-day events must come back in sequence order, unbroken across the page boundary"
    );
}
