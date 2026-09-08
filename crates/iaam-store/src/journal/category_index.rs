//! `event_category_assignments` — a projection, not a fact (spec §4.7).
//!
//! **There is exactly one implementation of the priority ladder, and it is
//! [`iaam_core::category::assign`].** Nothing here re-expresses `Row` before
//! `SourceCategory` before `Description`, or the longest-match/version/id
//! tie-break within a class: every row this module writes is the return
//! value of that one function, stored as-is. A predicate compiled into SQL
//! was rejected for two reasons the spec gives — a per-category predicate
//! needs "and no higher-priority rule matches", which is quadratic in the
//! rule set, and `Description` matching folds Unicode case in Rust, which
//! SQLite's `NOCASE`/`lower()` cannot reproduce for non-ASCII text.
//!
//! `NotDecomposed` is the **absence of a row**, never a row with a null or a
//! sentinel category: a silent bucket is exactly what the category design
//! refuses. [`rebuild`] therefore deletes before it repopulates, and
//! [`store_assignment`] deletes a row outright when the current rule set no
//! longer decomposes that event.
//!
//! `rules_revision` is the highest active `category_rules.version` a row was
//! computed from. [`revision`] answers "what is that number right now", so a
//! reader can compare it against a stored row and rebuild before answering
//! when they differ (Task 11 wires the read side; this module only builds
//! the mechanism).

use rusqlite::{Connection, Transaction, params};

use iaam_core::category::{
    self, CategoryAssignment, CategoryBasis, CategoryInterval, CategoryRule, CategorySubject,
};
use iaam_core::event::Event;
use iaam_core::ids::{CategoryId, CategoryRuleId, EventId, OwnerId};

use super::read::hydrate;
use crate::StoreError;
use crate::categories::{matcher_from_columns, parse_uuid, text_to_date};

/// Every active (`retired_at IS NULL`) category rule the owner has, as the
/// domain type `iaam_core::category::assign` reads.
fn active_rules(tx: &Transaction<'_>, owner: OwnerId) -> Result<Vec<CategoryRule>, StoreError> {
    let mut statement = tx.prepare(
        "SELECT id, version, matcher_kind, value, text, description_mode,
                category, valid_from, valid_to
         FROM category_rules
         WHERE owner = ?1 AND retired_at IS NULL",
    )?;
    let rows = statement.query_map(params![owner.inner().to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;

    let mut rules = Vec::new();
    for row in rows {
        let (
            id,
            version,
            matcher_kind,
            value,
            text,
            description_mode,
            category,
            valid_from,
            valid_to,
        ) = row?;
        rules.push(CategoryRule {
            id: CategoryRuleId(parse_uuid(&id, "category rule")?),
            version,
            interval: CategoryInterval {
                from: valid_from
                    .as_deref()
                    .map(|value| text_to_date(value, "valid_from"))
                    .transpose()?,
                to: valid_to
                    .as_deref()
                    .map(|value| text_to_date(value, "valid_to"))
                    .transpose()?,
            },
            matcher: matcher_from_columns(&matcher_kind, value, text, description_mode)?,
            category: CategoryId(parse_uuid(&category, "category")?),
        });
    }
    Ok(rules)
}

/// Every event id the owner's journal holds, in no particular order: a
/// rebuild recomputes the whole projection and does not depend on the order
/// it visits events in.
fn owner_event_ids(tx: &Transaction<'_>, owner: OwnerId) -> Result<Vec<EventId>, StoreError> {
    let mut statement = tx.prepare("SELECT id FROM events WHERE owner = ?1")?;
    let rows = statement.query_map(params![owner.inner().to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(EventId(parse_uuid(&row?, "event")?));
    }
    Ok(ids)
}

/// The highest active `category_rules.version` among `rules`, or `0` when
/// the owner has no active rule at all.
fn highest_version(rules: &[CategoryRule]) -> i64 {
    rules
        .iter()
        .map(|rule| i64::from(rule.version))
        .max()
        .unwrap_or(0)
}

/// Computes `event`'s assignment against `rules` and makes the stored row
/// agree with it: an upsert when it is `Assigned`, a delete when it is
/// `NotDecomposed` — never a row carrying a null or a sentinel category.
///
/// Returns whether a row was written, so [`rebuild`] can report how many of
/// the owner's events came out decomposed.
fn store_assignment(
    tx: &Transaction<'_>,
    owner: OwnerId,
    event: &Event,
    rules: &[CategoryRule],
    rules_revision: i64,
) -> Result<bool, StoreError> {
    match category::assign(&CategorySubject::of(event), rules) {
        CategoryAssignment::NotDecomposed => {
            tx.execute(
                "DELETE FROM event_category_assignments WHERE owner = ?1 AND event = ?2",
                params![owner.inner().to_string(), event.id.inner().to_string()],
            )?;
            Ok(false)
        }
        CategoryAssignment::Assigned { category, basis } => {
            let (basis_text, rule) = match basis {
                CategoryBasis::Row { rule } => ("row", rule),
                CategoryBasis::SourceCategory { rule } => ("source_category", rule),
                CategoryBasis::Description { rule } => ("description", rule),
            };
            tx.execute(
                "INSERT INTO event_category_assignments
                     (owner, event, category, rule, basis, rules_revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (owner, event) DO UPDATE SET
                     category = excluded.category,
                     rule = excluded.rule,
                     basis = excluded.basis,
                     rules_revision = excluded.rules_revision",
                params![
                    owner.inner().to_string(),
                    event.id.inner().to_string(),
                    category.inner().to_string(),
                    rule.inner().to_string(),
                    basis_text,
                    rules_revision,
                ],
            )?;
            Ok(true)
        }
    }
}

/// Recomputes the owner's whole projection from the journal and the
/// currently active rules, and returns how many events came out decomposed.
///
/// Rebuild points (spec §4.7): any create, edit or retirement of a category
/// rule (the caller in `iaam-app/src/scenarios/categories.rs`), and any
/// append to the journal, which [`super::write::insert_event_in`] already
/// keeps current one event at a time — a rebuild here is for when the *rule
/// set* changed underneath a journal that did not.
///
/// Old rows are deleted before the projection is repopulated: an event that
/// matched under the old rules and matches nothing under the new ones must
/// end up with no row at all, not a row nobody wrote deliberately.
pub(crate) fn rebuild(tx: &Transaction<'_>, owner: OwnerId) -> Result<u32, StoreError> {
    let rules = active_rules(tx, owner)?;
    let rules_revision = highest_version(&rules);
    let event_ids = owner_event_ids(tx, owner)?;
    let events = hydrate(tx, &event_ids)?;

    tx.execute(
        "DELETE FROM event_category_assignments WHERE owner = ?1",
        params![owner.inner().to_string()],
    )?;

    let mut decomposed = 0_u32;
    for event in &events {
        if store_assignment(tx, owner, event, &rules, rules_revision)? {
            decomposed = decomposed.saturating_add(1);
        }
    }
    Ok(decomposed)
}

/// Assigns categories for exactly `events`, against the rules active right
/// now, without touching any other event's row.
///
/// Called from [`super::write::insert_event_in`], the one place an event
/// enters the journal, so every writer — the ordinary append path and the
/// bundle importer alike — gets this for free rather than each remembering
/// to call it.
pub(crate) fn assign_for(
    tx: &Transaction<'_>,
    owner: OwnerId,
    events: &[Event],
) -> Result<(), StoreError> {
    if events.is_empty() {
        return Ok(());
    }
    let rules = active_rules(tx, owner)?;
    let rules_revision = highest_version(&rules);
    for event in events {
        store_assignment(tx, owner, event, &rules, rules_revision)?;
    }
    Ok(())
}

/// The revision a read must be built from: the highest active
/// `category_rules.version` the owner currently has, or `0` with none.
///
/// A stored row whose `rules_revision` does not equal this is stale — some
/// rule create/edit/retirement happened after that row was computed — and
/// must be rebuilt before it answers a query (Task 11 wires the read side).
pub(crate) fn revision(conn: &Connection, owner: OwnerId) -> Result<i64, StoreError> {
    let highest: Option<i64> = conn.query_row(
        "SELECT MAX(version) FROM category_rules WHERE owner = ?1 AND retired_at IS NULL",
        params![owner.inner().to_string()],
        |row| row.get(0),
    )?;
    Ok(highest.unwrap_or(0))
}

/// Whether the owner's projection must be rebuilt before a `category` or
/// `uncategorised` filter answers a query (Task 11, spec §4.7).
///
/// The application layer rebuilds eagerly on every rule create, edit or
/// retirement (`scenarios/categories.rs`), and `assign_for` keeps a freshly
/// appended event current as it lands (`write::insert_event_in`). Both of
/// those are the ordinary path and, by the time a read reaches here, the
/// projection is usually already right. This exists for the caller that
/// wrote rules or events straight through the store instead — a bundle
/// import, a test — and so cannot be trusted to have called either.
///
/// Two independent signs of staleness, because neither alone covers the
/// other's blind spot:
///
/// - **Some stored row disagrees with the current revision.** A rule was
///   edited or retired after that row was computed, and a full rebuild —
///   never a per-row patch, since one rule's edit can change what a
///   *different* category's rows decompose to via the priority ladder — is
///   what fixes it.
/// - **No stored row exists at all, despite an active rule.** A row's
///   absence is `NotDecomposed`, and reading a bundle-imported rule set with
///   zero rows this way is indistinguishable from "genuinely nothing
///   matches" and "never built past this rule" — so the ambiguous case is
///   treated as stale. A rebuild here is idempotent: if nothing actually
///   matches, it recomputes the same empty answer.
pub(crate) fn is_stale(conn: &Connection, owner: OwnerId) -> Result<bool, StoreError> {
    let current = revision(conn, owner)?;
    let mismatched: bool = conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM event_category_assignments
             WHERE owner = ?1 AND rules_revision != ?2
         )",
        params![owner.inner().to_string(), current],
        |row| row.get(0),
    )?;
    if mismatched {
        return Ok(true);
    }
    if current == 0 {
        return Ok(false);
    }
    let has_any_row: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM event_category_assignments WHERE owner = ?1)",
        params![owner.inner().to_string()],
        |row| row.get(0),
    )?;
    Ok(!has_any_row)
}

#[cfg(test)]
mod tests {
    use iaam_core::category::{CategoryMatcher, DescriptionMatchMode};
    use iaam_core::dates::{EffectiveOrder, EventDates};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Event, Relation};
    use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use rusqlite::OptionalExtension;
    use time::macros::date;
    use uuid::Uuid;

    use super::*;
    use crate::SqliteStore;
    use crate::categories::NewCategoryRule;
    use crate::journal::write::insert_event_in;
    use crate::reference::AccountRecord;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    /// Everything a category-index test needs: an owner, an account it can
    /// legally use in a leg, one category group with two categories, and a
    /// helper to insert events and rules. Invented from scratch (CLAUDE.md).
    struct Fixture {
        owner: OwnerId,
        account: AccountId,
        source: SourceId,
        groceries: CategoryId,
        transport: CategoryId,
    }

    impl Fixture {
        fn new(store: &mut SqliteStore) -> Self {
            let owner = OwnerId::new_random();
            let account = AccountId::new_random();
            let source = SourceId::new_random();

            store
                .upsert_account(&AccountRecord {
                    id: account,
                    owner,
                    title: "Main".to_owned(),
                    institution: Some("Test Bank".to_owned()),
                })
                .expect("account created");

            let group = store
                .insert_category_group(owner, "Spending")
                .expect("category group created");
            let groceries = store
                .insert_category(owner, group, "Groceries")
                .expect("groceries category created");
            let transport = store
                .insert_category(owner, group, "Transport")
                .expect("transport category created");

            Self {
                owner,
                account,
                source,
                groceries: CategoryId(groceries),
                transport: CategoryId(transport),
            }
        }

        fn base_provenance(&self) -> Provenance {
            Provenance::new(
                self.source,
                RawHash::parse(&"a".repeat(64)).unwrap(),
                ParserVersion("test/1".to_owned()),
            )
        }

        fn event(
            &self,
            sequence: u32,
            on: time::Date,
            description: Option<&str>,
            row_key: Option<&str>,
        ) -> Event {
            Event {
                id: EventId::new_random(),
                owner: self.owner,
                account: self.account,
                kind: EventKind::CashOut {
                    amount: rub(-1_000),
                },
                dates: EventDates::empty(),
                order: EffectiveOrder::new(on, sequence),
                legs: vec![Leg::cash(self.account, rub(-1_000))],
                provenance: match description {
                    Some(text) => self.base_provenance().with_description(text.to_owned()),
                    None => self.base_provenance(),
                },
                relation: Relation::None,
                confidence: Confidence::Known,
                idempotency_key: row_key.map(str::to_owned),
            }
        }

        fn insert_event(&self, store: &mut SqliteStore, event: &Event) {
            let tx = store.connection_mut().transaction().expect("open tx");
            insert_event_in(&tx, event).expect("insert event");
            tx.commit().expect("commit");
        }

        fn insert_rule(
            &self,
            store: &mut SqliteStore,
            matcher: CategoryMatcher,
            category: CategoryId,
            valid_from: Option<time::Date>,
            valid_to: Option<time::Date>,
        ) -> CategoryRuleId {
            store
                .insert_category_rule(
                    self.owner,
                    NewCategoryRule {
                        matcher,
                        category: category.inner(),
                        valid_from,
                        valid_to,
                    },
                    None,
                )
                .expect("category rule created")
                .id
        }
    }

    fn stored_assignment(
        conn: &rusqlite::Connection,
        owner: OwnerId,
        event: EventId,
    ) -> CategoryAssignment {
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT category, rule, basis FROM event_category_assignments
                 WHERE owner = ?1 AND event = ?2",
                params![owner.inner().to_string(), event.inner().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .expect("query stored assignment");
        match row {
            None => CategoryAssignment::NotDecomposed,
            Some((category, rule, basis)) => {
                let category = CategoryId(Uuid::parse_str(&category).expect("category uuid"));
                let rule = CategoryRuleId(Uuid::parse_str(&rule).expect("rule uuid"));
                let basis = match basis.as_str() {
                    "row" => CategoryBasis::Row { rule },
                    "source_category" => CategoryBasis::SourceCategory { rule },
                    "description" => CategoryBasis::Description { rule },
                    other => panic!("unexpected basis {other}"),
                };
                CategoryAssignment::Assigned { category, basis }
            }
        }
    }

    fn stored_revision(conn: &rusqlite::Connection, owner: OwnerId, event: EventId) -> Option<i64> {
        conn.query_row(
            "SELECT rules_revision FROM event_category_assignments
             WHERE owner = ?1 AND event = ?2",
            params![owner.inner().to_string(), event.inner().to_string()],
            |row| row.get(0),
        )
        .optional()
        .expect("query stored revision")
    }

    /// The test that is the point: for a generated journal and rule set,
    /// every row this projection writes equals `category::assign` computed
    /// directly for that event (spec §4.7, plan Task 9).
    ///
    /// The fixture deliberately includes: an event matched by a description
    /// rule of one category that is outranked by a row rule of another (the
    /// case a naive `OR` over one category's rules gets wrong), a rule whose
    /// validity interval excludes the event's date, and an event no rule
    /// matches at all.
    #[test]
    fn the_projection_agrees_with_the_core_for_every_event() {
        let mut store = SqliteStore::open_in_memory().expect("in-memory store");
        let fixture = Fixture::new(&mut store);

        // Outranked by class: a description rule for Transport would match,
        // but a Row rule for Groceries wins regardless of matcher class.
        let outranked = fixture.event(
            1,
            date!(2026 - 03 - 10),
            Some("Corner Shop"),
            Some("row-outranked"),
        );
        fixture.insert_event(&mut store, &outranked);

        // Interval excludes the event: the rule below is only valid from
        // 2026-06-01, so this March event must not match it.
        let outside_interval = fixture.event(2, date!(2026 - 03 - 15), Some("Bus Pass"), None);
        fixture.insert_event(&mut store, &outside_interval);
        // A second event the same rule *does* cover, once it becomes valid.
        let inside_interval = fixture.event(3, date!(2026 - 07 - 01), Some("Bus Pass"), None);
        fixture.insert_event(&mut store, &inside_interval);

        // No rule matches this one at all.
        let unmatched = fixture.event(4, date!(2026 - 03 - 20), Some("Mystery"), None);
        fixture.insert_event(&mut store, &unmatched);

        fixture.insert_rule(
            &mut store,
            CategoryMatcher::Description {
                text: "Corner Shop".to_owned(),
                mode: DescriptionMatchMode::Contains,
            },
            fixture.transport,
            None,
            None,
        );
        fixture.insert_rule(
            &mut store,
            CategoryMatcher::Row {
                key: "row-outranked".to_owned(),
            },
            fixture.groceries,
            None,
            None,
        );
        fixture.insert_rule(
            &mut store,
            CategoryMatcher::Description {
                text: "Bus Pass".to_owned(),
                mode: DescriptionMatchMode::Contains,
            },
            fixture.transport,
            Some(date!(2026 - 06 - 01)),
            None,
        );

        let active_rules = {
            let tx = store.connection_mut().transaction().expect("open tx");
            let rules = active_rules(&tx, fixture.owner).expect("load active rules");
            tx.commit().expect("commit");
            rules
        };

        let tx = store.connection_mut().transaction().expect("open tx");
        rebuild(&tx, fixture.owner).expect("rebuild");
        tx.commit().expect("commit");

        for event in [&outranked, &outside_interval, &inside_interval, &unmatched] {
            let stored = stored_assignment(store.connection(), fixture.owner, event.id);
            let computed = category::assign(&CategorySubject::of(event), &active_rules);
            assert_eq!(stored, computed, "event {}", event.id.inner());
        }
    }

    /// A rule edit (retire the old, create the new) followed by a rebuild
    /// leaves no stale row: neither one pointing at the old category, nor
    /// one for an event that now matches nothing.
    #[test]
    fn a_rule_edit_followed_by_rebuild_leaves_no_stale_row() {
        let mut store = SqliteStore::open_in_memory().expect("in-memory store");
        let fixture = Fixture::new(&mut store);

        let event = fixture.event(1, date!(2026 - 03 - 10), Some("Corner Shop"), None);
        fixture.insert_event(&mut store, &event);

        let original_rule = fixture.insert_rule(
            &mut store,
            CategoryMatcher::DescriptionContains {
                text: "Corner Shop".to_owned(),
            },
            fixture.groceries,
            None,
            None,
        );

        {
            let tx = store.connection_mut().transaction().expect("open tx");
            rebuild(&tx, fixture.owner).expect("rebuild");
            tx.commit().expect("commit");
        }
        assert_eq!(
            stored_assignment(store.connection(), fixture.owner, event.id),
            CategoryAssignment::Assigned {
                category: fixture.groceries,
                basis: CategoryBasis::Description {
                    rule: original_rule,
                },
            }
        );

        // Retire the rule and replace it with one that no longer matches
        // this event's description at all.
        store
            .amend_category_rule(
                fixture.owner,
                original_rule,
                NewCategoryRule {
                    matcher: CategoryMatcher::DescriptionContains {
                        text: "Unrelated Merchant".to_owned(),
                    },
                    category: fixture.transport.inner(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("amend rule");

        {
            let tx = store.connection_mut().transaction().expect("open tx");
            rebuild(&tx, fixture.owner).expect("rebuild");
            tx.commit().expect("commit");
        }

        assert_eq!(
            stored_assignment(store.connection(), fixture.owner, event.id),
            CategoryAssignment::NotDecomposed,
            "no row must survive for an event the new rule set no longer matches"
        );
    }

    /// [`revision`] tracks the highest active rule version, and a stored
    /// row's `rules_revision` matches it right after a rebuild — the
    /// mechanism a stale read (Task 11) will compare against.
    #[test]
    fn revision_tracks_the_highest_active_rule_version_and_rows_record_it() {
        let mut store = SqliteStore::open_in_memory().expect("in-memory store");
        let fixture = Fixture::new(&mut store);
        assert_eq!(
            revision(store.connection(), fixture.owner).expect("revision"),
            0,
            "no active rule at all means revision zero"
        );

        let event = fixture.event(1, date!(2026 - 03 - 10), Some("Corner Shop"), None);
        fixture.insert_event(&mut store, &event);
        fixture.insert_rule(
            &mut store,
            CategoryMatcher::DescriptionContains {
                text: "Corner Shop".to_owned(),
            },
            fixture.groceries,
            None,
            None,
        );

        let after_first_rule = revision(store.connection(), fixture.owner).expect("revision");
        assert_eq!(after_first_rule, 1);

        {
            let tx = store.connection_mut().transaction().expect("open tx");
            rebuild(&tx, fixture.owner).expect("rebuild");
            tx.commit().expect("commit");
        }
        assert_eq!(
            stored_revision(store.connection(), fixture.owner, event.id),
            Some(after_first_rule)
        );

        // A second, unrelated rule bumps the owner-wide version counter even
        // though it does not touch this event, and `revision` follows it.
        fixture.insert_rule(
            &mut store,
            CategoryMatcher::SourceCategory {
                value: "Whatever".to_owned(),
            },
            fixture.transport,
            None,
            None,
        );
        assert_eq!(
            revision(store.connection(), fixture.owner).expect("revision"),
            2
        );
    }

    /// `assign_for`, the incremental path `insert_event_in` calls, gives a
    /// freshly appended event its row without a full rebuild.
    #[test]
    fn insert_event_in_gives_an_appended_event_its_row_through_assign_for() {
        let mut store = SqliteStore::open_in_memory().expect("in-memory store");
        let fixture = Fixture::new(&mut store);
        fixture.insert_rule(
            &mut store,
            CategoryMatcher::DescriptionContains {
                text: "Corner Shop".to_owned(),
            },
            fixture.groceries,
            None,
            None,
        );

        let event = fixture.event(1, date!(2026 - 03 - 10), Some("Corner Shop"), None);
        fixture.insert_event(&mut store, &event);

        assert!(matches!(
            stored_assignment(store.connection(), fixture.owner, event.id),
            CategoryAssignment::Assigned { category, .. } if category == fixture.groceries
        ));

        // An event no rule matches gets no row at all — never a sentinel.
        let unmatched = fixture.event(2, date!(2026 - 03 - 11), Some("Nothing Matches"), None);
        fixture.insert_event(&mut store, &unmatched);
        assert_eq!(
            stored_assignment(store.connection(), fixture.owner, unmatched.id),
            CategoryAssignment::NotDecomposed
        );
    }

    /// [`is_stale`] catches the two shapes of staleness Task 11's read-time
    /// check exists for: a rule created straight through the store, bypassing
    /// the application layer's eager rebuild, first with zero rows to show
    /// for it and then, once rebuilt, with a stale row surviving a further
    /// rule edit.
    #[test]
    fn is_stale_catches_a_missing_rebuild_and_a_row_left_behind_by_a_rule_edit() {
        let mut store = SqliteStore::open_in_memory().expect("in-memory store");
        let fixture = Fixture::new(&mut store);

        assert!(
            !is_stale(store.connection(), fixture.owner).expect("stale check"),
            "no active rule and no row is not stale"
        );

        let event = fixture.event(1, date!(2026 - 03 - 10), Some("Corner Shop"), None);
        fixture.insert_event(&mut store, &event);

        // Insert the rule directly through the store, the way a bundle import
        // or a test would — bypassing `scenarios/categories.rs`'s eager
        // rebuild. The projection now has zero rows despite an active rule
        // that matches an already-journalled event.
        let rule = fixture.insert_rule(
            &mut store,
            CategoryMatcher::DescriptionContains {
                text: "Corner Shop".to_owned(),
            },
            fixture.groceries,
            None,
            None,
        );
        assert!(
            is_stale(store.connection(), fixture.owner).expect("stale check"),
            "an active rule with zero assignment rows must be treated as stale"
        );

        {
            let tx = store.connection_mut().transaction().expect("open tx");
            rebuild(&tx, fixture.owner).expect("rebuild");
            tx.commit().expect("commit");
        }
        assert!(
            !is_stale(store.connection(), fixture.owner).expect("stale check"),
            "freshly rebuilt is not stale"
        );

        // Edit the rule directly through the store again, leaving the old
        // row's `rules_revision` behind.
        store
            .amend_category_rule(
                fixture.owner,
                rule,
                NewCategoryRule {
                    matcher: CategoryMatcher::DescriptionContains {
                        text: "Unrelated Merchant".to_owned(),
                    },
                    category: fixture.transport.inner(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("amend rule");
        assert!(
            is_stale(store.connection(), fixture.owner).expect("stale check"),
            "a stored row whose rules_revision disagrees with the current one is stale"
        );
    }
}
