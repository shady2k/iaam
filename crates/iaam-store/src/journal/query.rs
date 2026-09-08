//! The paginated journal listing and the source-category projection (spec §6).
//!
//! Every predicate here used to reach into `events.payload` through SQLite's
//! JSON-path extraction and iteration functions; none of them do any more:
//!
//! - `currencies` was an iteration over the `legs` array pulling out each
//!   leg's `money.currency` path — now an `EXISTS` over `event_legs.currency`.
//! - `touching` was the same shape over each leg's `account` path — now an
//!   `EXISTS` over `event_legs.account`.
//! - `import` was a path extraction of `provenance.import` — now the
//!   `events.import` column.
//! - the source-category listing was a path extraction of
//!   `provenance.source_category` — now the `events.source_category` column.
//!
//! [`journal_sql`] is phase 1 of the two-phase read (spec §6): it selects
//! **event ids only**, filtered and ordered by `(effective_date, sequence)`
//! with the keyset cursor. A single join with a `LIMIT` on it would be wrong
//! — legs multiply the rows a join over `events` and `event_legs` would
//! produce, so a page boundary on a joined query could cut one event's legs
//! in half. [`SqliteStore::list_journal_events`] is phase 1 plus
//! [`super::hydrate`], phase 2.

use iaam_core::ids::{
    AccountId, CategoryId, ClassificationRuleId, EventId, ImportId, ImportSessionId, OwnerId,
    SourceId,
};
use iaam_core::money::CurrencyCode;
use time::Date;

use super::{hydrate, parse_uuid};
use crate::{SqliteStore, StoreError};

/// Position in the journal's total order.
///
/// `(owner, effective_date, sequence)` is unique by index, so this pair names
/// exactly one row and paging can resume from it without an offset. An offset
/// would shift under a concurrent write and silently skip or repeat a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalCursor {
    pub effective_date: Date,
    pub sequence: u32,
}

/// One narrowed page of the owner's journal.
///
/// Every handle is optional here, and the rule that a read must narrow by
/// *something* deliberately is not. That rule is a policy about what an API
/// caller may ask for, it lives in the application where the refusal is
/// written, and a second copy of it in the store would drift from the first.
#[derive(Debug, Clone, Default)]
pub struct JournalQuery {
    pub event: Option<EventId>,
    pub idempotency_key: Option<String>,
    pub account: Option<AccountId>,
    /// Only facts whose event account or one of their legs is this account.
    pub touching: Option<AccountId>,
    /// Only facts of one of these event-family discriminants.
    pub kinds: Vec<String>,
    /// Only facts with at least one leg whose money has one of these currencies.
    /// The whole event remains selected, including legs in other currencies.
    pub currencies: Vec<CurrencyCode>,
    pub source: Option<SourceId>,
    /// Only facts committed out of this import session.
    pub import_session: Option<ImportSessionId>,
    /// Only facts carrying this declared import. Unlike `import_session`, the
    /// import survives across several sessions and is the key a retraction
    /// takes.
    pub import: Option<ImportId>,
    /// Only facts one of the owner's standing classification rules filed.
    ///
    /// A fact whose reading found no rule, and a fact recorded before the rule
    /// was recorded at all, are both outside every value of this filter: they
    /// name no rule, and this selects by a named one. What tells those two
    /// apart is the fact's own provenance, not this handle.
    pub settled_by_rule: Option<ClassificationRuleId>,
    /// Only facts `event_category_assignments` assigns to this category
    /// (spec §4.7, §6.1). Mutually exclusive with [`Self::uncategorised`] —
    /// the two ask opposite questions and the caller decides which.
    pub category: Option<CategoryId>,
    /// Only facts with no row in `event_category_assignments` at all —
    /// `NotDecomposed`, the honest absence, never a sentinel category
    /// (spec §4.7). Mutually exclusive with [`Self::category`].
    pub uncategorised: bool,
    /// Only facts that are a movement whose **far** endpoint is this account:
    /// a transfer read from one of its own two ends, naming the other
    /// (spec §6.1). Deliberately narrower than [`Self::touching`], which is
    /// symmetric and also matches this account as the *near* side — a
    /// transfer `Main -> Savings` read from `Main` is matched by
    /// `counterparty = Savings` and not by `counterparty = Main`.
    pub counterparty: Option<AccountId>,
    /// Inclusive lower bound on the effective date.
    pub from: Option<Date>,
    /// Inclusive upper bound on the effective date.
    pub to: Option<Date>,
    /// Resume after this position, exclusive.
    pub after: Option<JournalCursor>,
    /// Maximum rows to return.
    pub limit: u32,
}

/// Filters for the distinct source-category values in the journal.
#[derive(Debug, Clone, Default)]
pub struct SourceCategoryQuery {
    pub account: Option<AccountId>,
    pub from: Option<Date>,
    pub to: Option<Date>,
}

impl SqliteStore {
    /// Read a narrowed page of the owner's journal in `(date, sequence)` order.
    ///
    /// The owner is part of every clause, not merely of the identifier the
    /// caller supplied: an event identifier is a UUID, and a UUID confers no
    /// right to read someone else's journal (§14).
    pub fn list_journal_events(
        &mut self,
        owner: OwnerId,
        query: &JournalQuery,
    ) -> Result<Vec<iaam_core::event::Event>, StoreError> {
        // The staleness check is load-bearing (spec §4.7, §6.1): a category
        // filter must never quietly answer from a rule set the owner has
        // since changed. `assign_for` keeps a freshly appended event current
        // as it lands, and the application layer rebuilds eagerly on every
        // rule create/edit/retirement — but a caller that wrote rules or
        // events straight through the store (a bundle import, a test) can
        // leave the projection behind either path, so the read side checks
        // for itself rather than trusting that everyone remembered.
        if (query.category.is_some() || query.uncategorised)
            && super::category_index::is_stale(&self.conn, owner)?
        {
            self.rebuild_category_index(owner)?;
        }
        let (sql, parameters) = journal_sql(owner, query)?;
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(EventId(parse_uuid(&row?, "event")?));
        }
        hydrate(&self.conn, &ids)
    }

    /// Read the owner's correction graph without decoding event payloads.
    ///
    /// The relation columns are the complete input to supersession resolution,
    /// so this projection keeps a page read from materialising the whole
    /// journal just to annotate its rows.
    pub fn list_journal_event_relations(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<(EventId, iaam_core::event::Relation)>, StoreError> {
        use iaam_core::event::Relation;

        let mut statement = self.conn.prepare(
            "SELECT id, relation_kind, relation_target
             FROM events
             WHERE owner = ?1
             ORDER BY effective_date, sequence",
        )?;
        let rows = statement.query_map(rusqlite::params![owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (id, kind, target) = row?;
            let id = EventId(parse_uuid(&id, "event")?);
            let relation = match kind.as_str() {
                "none" => Relation::None,
                "reversal" => Relation::Reversal {
                    target: EventId(parse_uuid(
                        target.as_deref().ok_or_else(|| StoreError::InvalidValue {
                            field: "relation_target",
                            value: "NULL for reversal".to_owned(),
                        })?,
                        "event relation target",
                    )?),
                },
                "replacement" => Relation::Replacement {
                    target: EventId(parse_uuid(
                        target.as_deref().ok_or_else(|| StoreError::InvalidValue {
                            field: "relation_target",
                            value: "NULL for replacement".to_owned(),
                        })?,
                        "event relation target",
                    )?),
                },
                _ => {
                    return Err(StoreError::InvalidValue {
                        field: "relation_kind",
                        value: kind,
                    });
                }
            };
            Ok((id, relation))
        })
        .collect()
    }

    /// Distinct source-category evidence for the owner's journal scope.
    pub fn list_journal_source_categories(
        &self,
        owner: OwnerId,
        query: &SourceCategoryQuery,
    ) -> Result<Vec<String>, StoreError> {
        let (sql, parameters) = source_categories_sql(owner, query);
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// Assemble the narrowed query and its bound parameters.
fn source_categories_sql(
    owner: OwnerId,
    query: &SourceCategoryQuery,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut sql = String::from(
        "SELECT DISTINCT source_category
         FROM events
         WHERE owner = ?1
           AND source_category IS NOT NULL",
    );
    let mut parameters: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(owner.inner().to_string())];

    let mut bind = |clause: &str, value: Box<dyn rusqlite::ToSql>| {
        parameters.push(value);
        sql.push_str(&clause.replace('?', &format!("?{}", parameters.len())));
    };

    if let Some(account) = query.account {
        bind(" AND account = ?", Box::new(account.inner().to_string()));
    }
    if let Some(from) = query.from {
        bind(" AND effective_date >= ?", Box::new(from.to_string()));
    }
    if let Some(to) = query.to {
        bind(" AND effective_date <= ?", Box::new(to.to_string()));
    }
    sql.push_str(" ORDER BY 1");
    (sql, parameters)
}

/// Phase 1 of the two-phase read (spec §6): the page of event ids the
/// filters and the keyset cursor select, in `(effective_date, sequence)`
/// order. Callers hydrate the ids this returns; nothing here reads a column
/// beyond what `events` and `event_legs` carry.
///
/// The SQL is built rather than written out because the handles are
/// independent: spelling every combination would be sixteen statements that
/// must agree on the ordering, and one of them would eventually not.
/// Nothing from the caller is interpolated — only placeholder numbers are —
/// so a value can never become SQL.
pub fn journal_sql(
    owner: OwnerId,
    query: &JournalQuery,
) -> Result<(String, Vec<Box<dyn rusqlite::ToSql>>), StoreError> {
    // `counterparty` joins `event_cash_transfer` in the `FROM` clause,
    // leading with `t` under `CROSS JOIN` rather than an ordinary join or an
    // `EXISTS` correlated on `t.event = events.id`. Both of those let SQLite
    // reorder the join, and it always chooses to drive from `events` instead
    // — `events_by_order` already satisfies `WHERE owner = ?` and the
    // `ORDER BY` for free, so the optimizer never even considers seeking
    // `event_cash_transfer` by its own far-account columns, no matter how
    // selective that seek is. `CROSS JOIN` is SQLite's documented way to
    // pin join order left to right, which is what lets
    // `event_cash_transfer_by_from`/`_by_to` answer the far-account equality
    // first; the small join back to `events` by primary key, and the sort
    // `ORDER BY` now costs, are cheap next to scanning every one of the
    // owner's events to find the few that are transfers at all (spec §6.2).
    // The join is safe regardless of order because the table is strictly one
    // row per event (its primary key is `event` alone): it can only narrow
    // the result, never multiply a row, which is the hazard `EXISTS` exists
    // for against `event_legs`.
    let mut sql = if query.counterparty.is_some() {
        String::from(
            "SELECT events.id FROM event_cash_transfer AS t \
             CROSS JOIN events ON events.id = t.event \
             WHERE events.owner = ?1",
        )
    } else {
        String::from("SELECT id FROM events WHERE owner = ?1")
    };
    let mut parameters: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(owner.inner().to_string())];

    if !query.kinds.is_empty() {
        sql.push_str(" AND kind IN (");
        for (index, kind) in query.kinds.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            parameters.push(Box::new(kind.clone()));
            sql.push_str(&format!("?{}", parameters.len()));
        }
        sql.push(')');
    }
    if !query.currencies.is_empty() {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM event_legs \
             WHERE event_legs.event = events.id AND event_legs.currency IN (",
        );
        for (index, currency) in query.currencies.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            parameters.push(Box::new(currency.code().to_owned()));
            sql.push_str(&format!("?{}", parameters.len()));
        }
        sql.push_str("))");
    }
    let mut bind = |sql: &mut String, clause: &str, value: Box<dyn rusqlite::ToSql>| {
        parameters.push(value);
        sql.push_str(&clause.replace('?', &format!("?{}", parameters.len())));
    };

    if let Some(event) = query.event {
        bind(&mut sql, " AND id = ?", Box::new(event.inner().to_string()));
    }
    if let Some(key) = query.idempotency_key.as_ref() {
        bind(&mut sql, " AND idempotency_key = ?", Box::new(key.clone()));
    }
    if let Some(account) = query.account {
        bind(
            &mut sql,
            " AND account = ?",
            Box::new(account.inner().to_string()),
        );
    }
    if let Some(touching) = query.touching {
        // Ordinary events carry their account in `events.account`, and
        // transfer events carry the receiving account only in a leg — keep
        // both branches.
        bind(
            &mut sql,
            " AND (account = ?",
            Box::new(touching.inner().to_string()),
        );
        bind(
            &mut sql,
            " OR EXISTS (SELECT 1 FROM event_legs \
             WHERE event_legs.event = events.id AND event_legs.account = ?))",
            Box::new(touching.inner().to_string()),
        );
    }
    if let Some(source) = query.source {
        bind(
            &mut sql,
            " AND source = ?",
            Box::new(source.inner().to_string()),
        );
    }
    if let Some(import) = query.import {
        bind(
            &mut sql,
            " AND import = ?",
            Box::new(import.inner().to_string()),
        );
    }
    if let Some(session) = query.import_session {
        bind(
            &mut sql,
            " AND import_session = ?",
            Box::new(session.inner().to_string()),
        );
    }
    if let Some(rule) = query.settled_by_rule {
        bind(
            &mut sql,
            " AND settled_by_rule = ?",
            Box::new(rule.inner().to_string()),
        );
    }
    if let Some(category) = query.category {
        // Correlated on `events.owner` rather than a bound parameter: the
        // assignment's owner is always the event's owner (§4.7), and reading
        // it off the outer row lets the index (owner, category, event) answer
        // the whole predicate without an extra bind.
        bind(
            &mut sql,
            " AND EXISTS (SELECT 1 FROM event_category_assignments a \
             WHERE a.owner = events.owner AND a.event = events.id AND a.category = ?)",
            Box::new(category.inner().to_string()),
        );
    }
    if query.uncategorised {
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM event_category_assignments a \
             WHERE a.owner = events.owner AND a.event = events.id)",
        );
    }
    if let Some(counterparty) = query.counterparty {
        // Deliberately narrower than `touching` (spec §6.1): a transfer
        // matches only when the named account is the *far* end from the
        // event's own account, never the near one. `t` is already joined
        // above, one row per event.
        let far_account = counterparty.inner().to_string();
        bind(
            &mut sql,
            " AND ((events.account = t.from_account AND t.to_account = ?)",
            Box::new(far_account.clone()),
        );
        bind(
            &mut sql,
            " OR (events.account = t.to_account AND t.from_account = ?))",
            Box::new(far_account),
        );
    }
    if let Some(from) = query.from {
        bind(
            &mut sql,
            " AND effective_date >= ?",
            Box::new(from.to_string()),
        );
    }
    if let Some(to) = query.to {
        bind(
            &mut sql,
            " AND effective_date <= ?",
            Box::new(to.to_string()),
        );
    }
    if let Some(after) = query.after {
        // Strictly after the cursor in the same order the rows come back in.
        // Comparing the pair, rather than the date alone, is what stops the
        // last row of a page from opening the next one.
        bind(
            &mut sql,
            " AND (effective_date > ?",
            Box::new(after.effective_date.to_string()),
        );
        bind(
            &mut sql,
            " OR (effective_date = ?",
            Box::new(after.effective_date.to_string()),
        );
        bind(&mut sql, " AND sequence > ?))", Box::new(after.sequence));
    }

    sql.push_str(" ORDER BY effective_date, sequence");
    bind(&mut sql, " LIMIT ?", Box::new(i64::from(query.limit)));
    Ok((sql, parameters))
}
