//! The whole-journal and narrowed reads that are not the paginated listing:
//! `load_events`/`load_events_through` (the projection's own two entry
//! points), account activity, and control-assertion lookup.
//!
//! Every one of these read `events.payload` before this task; none of them
//! do now. Each is a phase-1 column query — never reaching into a JSON leg
//! or provenance field — followed by [`super::hydrate`] where the event body
//! itself is needed.

use iaam_core::event::Event;
use iaam_core::event::kind::{
    CASH_TRANSFER_KIND, CONTROL_ASSERTION_KIND, EventKind, IMPORT_COVERAGE_GAP_KIND,
};
use iaam_core::ids::{AccountId, EventId, OwnerId};
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use rusqlite::params;
use time::Date;
use time::format_description::well_known::Iso8601;

use super::{hydrate, parse_uuid};
use crate::{SqliteStore, StoreError};

/// Position ordering shared by [`SqliteStore::load_events`] and
/// [`SqliteStore::load_events_through`]: the database defines the order, but
/// the projection still sorts the slice itself — the core need not trust the
/// order received from outside (§4.8). Known source times sort before untimed
/// events, and raw hashes make equal source times reproducible.
const PROJECTION_ORDER: &str = "ORDER BY effective_date,
     source_time IS NULL,
     CASE WHEN source_time IS NULL THEN sequence ELSE 0 END,
     source_time,
     CASE WHEN source_time IS NULL THEN '' ELSE raw_hash END,
     sequence,
     id";

impl SqliteStore {
    /// The owner's entire log in `EffectiveOrder`.
    pub fn load_events(&self, owner: OwnerId) -> Result<Vec<Event>, StoreError> {
        let sql = format!("SELECT id FROM events WHERE owner = ?1 {PROJECTION_ORDER}");
        let ids = self.event_ids(&sql, params![owner.inner().to_string()])?;
        hydrate(&self.conn, &ids)
    }

    /// Owner ledger through and including the date. The report-date slice
    /// is assembled by the wrapper: the event core does not filter by date (§6.1).
    pub fn load_events_through(
        &self,
        owner: OwnerId,
        through: Date,
    ) -> Result<Vec<Event>, StoreError> {
        let sql = format!(
            "SELECT id FROM events WHERE owner = ?1 AND effective_date <= ?2 {PROJECTION_ORDER}"
        );
        let ids = self.event_ids(
            &sql,
            params![owner.inner().to_string(), through.to_string()],
        )?;
        hydrate(&self.conn, &ids)
    }

    fn event_ids(
        &self,
        sql: &str,
        parameters: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<EventId>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(parameters, |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(EventId(parse_uuid(&row?, "event")?));
        }
        Ok(ids)
    }
}

/// Activity bounds for one account, excluding bookkeeping events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountActivityRecord {
    pub account: AccountId,
    pub has_business_fact: bool,
    pub first_effective_date: Option<Date>,
    pub last_effective_date: Option<Date>,
}

impl AccountActivityRecord {
    /// Widen the record to cover one more day this account had a fact on.
    ///
    /// The bounds are data-coverage bounds, so a fact reaching the account from
    /// the other side of a transfer moves them exactly as one recorded against
    /// it does: an account whose only day is the day money arrived is covered
    /// on that day and on no other.
    fn touch(&mut self, date: Date) {
        self.has_business_fact = true;
        self.first_effective_date = Some(
            self.first_effective_date
                .map_or(date, |first| first.min(date)),
        );
        self.last_effective_date =
            Some(self.last_effective_date.map_or(date, |last| last.max(date)));
    }
}

impl SqliteStore {
    /// Summarise every owned account, counting **both** accounts a transfer
    /// touched.
    ///
    /// `events.account` is the account an event is *recorded against*, and a
    /// `CashTransfer` is recorded against one of the two it moves money
    /// between. Joining on that column alone therefore reports an account whose
    /// entire content arrived by internal transfer — savings fed from a current
    /// account, a deposit opened by moving money across — as having no business
    /// fact at all: the queue never asks it for a balance and offers it an
    /// import instead, while it holds money at month end (iaam-8axt).
    ///
    /// The far endpoint used to live only in the event payload and be read in
    /// Rust from `EventKind::flow_endpoints`. It now lives in
    /// `event_cash_transfer.to_account`/`from_account` — the same columns
    /// [`kind::to_rows`](super::kind) wrote it into — so this reads a column
    /// instead of decoding a fact just to find the account it names. A
    /// `CashTransfer` is the only kind [`EventKind::flow_endpoints`] answers
    /// `BetweenAccounts` for, so joining `event_cash_transfer` finds exactly
    /// the events the old payload scan did.
    pub fn list_account_activity(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountActivityRecord>, StoreError> {
        let mut activity = self.activity_recorded_against(owner)?;
        for (from, to, date) in self.transfer_endpoints(owner)? {
            for account in [from, to] {
                if let Some(record) = activity.iter_mut().find(|record| record.account == account) {
                    record.touch(date);
                }
            }
        }
        Ok(activity)
    }

    /// The bounds an account gets from the events recorded against it.
    fn activity_recorded_against(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<AccountActivityRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT a.id, COUNT(e.id), MIN(e.effective_date), MAX(e.effective_date)
             FROM accounts AS a
             LEFT JOIN events AS e
               ON e.owner = a.owner
              AND e.account = a.id
              AND e.kind NOT IN (?2, ?3)
             WHERE a.owner = ?1
             GROUP BY a.id
             ORDER BY a.id",
        )?;
        let rows = statement.query_map(
            params![
                owner.inner().to_string(),
                CONTROL_ASSERTION_KIND,
                IMPORT_COVERAGE_GAP_KIND
            ],
            |row| {
                let id: String = row.get(0)?;
                let first: Option<String> = row.get(2)?;
                let last: Option<String> = row.get(3)?;
                Ok((id, row.get::<_, i64>(1)? > 0, first, last))
            },
        )?;
        let mut activity = Vec::new();
        for row in rows {
            let (id, has_business_fact, first, last) = row?;
            activity.push(AccountActivityRecord {
                account: AccountId(parse_uuid(&id, "account")?),
                has_business_fact,
                first_effective_date: first.as_deref().map(parse_date).transpose()?,
                last_effective_date: last.as_deref().map(parse_date).transpose()?,
            });
        }
        Ok(activity)
    }

    /// Both accounts of every transfer the owner's journal holds, with its day.
    fn transfer_endpoints(
        &self,
        owner: OwnerId,
    ) -> Result<Vec<(AccountId, AccountId, Date)>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT e.effective_date, t.from_account, t.to_account
             FROM event_cash_transfer AS t
             JOIN events AS e ON e.id = t.event
             WHERE e.owner = ?1 AND e.kind = ?2
             ORDER BY e.effective_date, e.sequence, e.id",
        )?;
        let rows = statement.query_map(
            params![owner.inner().to_string(), CASH_TRANSFER_KIND],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let mut endpoints = Vec::new();
        for row in rows {
            let (date, from, to) = row?;
            endpoints.push((
                AccountId(parse_uuid(&from, "account")?),
                AccountId(parse_uuid(&to, "account")?),
                parse_date(&date)?,
            ));
        }
        Ok(endpoints)
    }
}

/// The state needed to match one control assertion without loading the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlAssertionRecord {
    pub account: AccountId,
    pub period: AssertionPeriod,
    pub point: Option<BalancePoint>,
    pub dimension: Dimension,
    pub reconstructed_opening: bool,
}

impl SqliteStore {
    /// List only an account's control assertions.
    ///
    /// The kind filter narrows phase 1 to `events` columns alone; the claim
    /// shape itself only exists on the hydrated [`Event`], so phase 2 still
    /// matches on the decoded kind, exactly as it did when the event came out
    /// of a JSON payload.
    pub fn list_control_assertions(
        &self,
        owner: OwnerId,
        account: AccountId,
    ) -> Result<Vec<ControlAssertionRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id FROM events
             WHERE owner = ?1 AND account = ?2 AND kind IN (?3, ?4)
             ORDER BY effective_date, sequence, id",
        )?;
        let rows = statement.query_map(
            params![
                owner.inner().to_string(),
                account.inner().to_string(),
                CONTROL_ASSERTION_KIND,
                "opening_cash"
            ],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(EventId(parse_uuid(&row?, "event")?));
        }

        let mut assertions = Vec::new();
        for event in hydrate(&self.conn, &ids)? {
            let (period, point, dimension, reconstructed_opening) = match event.kind {
                EventKind::ControlAssertion { period, claim } => {
                    let point = match &claim {
                        ControlClaim::CashBalance { at, .. }
                        | ControlClaim::PositionQuantity { at, .. } => Some(*at),
                        _ => None,
                    };
                    (period, point, claim.dimension(), false)
                }
                EventKind::OpeningCash { .. } => (
                    AssertionPeriod::between(event.order.date(), event.order.date())
                        .expect("an opening event has a one-day assertion period"),
                    Some(BalancePoint::Opening),
                    Dimension::Cash,
                    true,
                ),
                _ => continue,
            };
            assertions.push(ControlAssertionRecord {
                account: event.account,
                period,
                point,
                dimension,
                reconstructed_opening,
            });
        }
        Ok(assertions)
    }
}

fn parse_date(value: &str) -> Result<Date, StoreError> {
    Date::parse(value, &Iso8601::DATE).map_err(|_| StoreError::InvalidValue {
        field: "effective_date",
        value: value.to_owned(),
    })
}
