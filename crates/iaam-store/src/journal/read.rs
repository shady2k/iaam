//! Two-phase read and bulk hydration (spec §6).
//!
//! Phase 1 — which events, under which filters and keyset cursor — is Task
//! 8's query builder and is not this module's concern. This module is phase
//! 2 only: given a page of [`EventId`]s someone else already selected, load
//! every row that page needs and reassemble each [`Event`] in Rust.
//!
//! **Never a join with `LIMIT`.** A leg multiplies the row a join over
//! `events`, `event_legs` and a detail table would produce, so a page
//! boundary on a joined query could cut one event's legs in half and hand
//! the caller a fact silently missing a leg. Instead, [`hydrate`] issues one
//! `SELECT ... WHERE <key> IN (...)` per table this journal has — sixteen
//! statements: `events`, `event_legs`, the twelve detail tables of spec
//! §4.3, and the two `event_coverage_gap_*` collection tables of §4.4 —
//! groups the returned rows by event in Rust, and hands each group to
//! [`super::kind::from_rows`]. The statement count does not depend on how
//! many ids were asked for, only on how many tables the journal has; the
//! test at the bottom of this file proves it by counting through
//! [`rusqlite::Connection::trace_v2`] rather than by reading the code.

use std::collections::HashMap;

use iaam_core::event::Event;
use iaam_core::ids::EventId;
use rusqlite::{Connection, params_from_iter};

use super::kind::from_rows;
use super::rows::{
    CashTransferRow, ControlAssertionRow, CorporateActionRow, CoverageGapDetail,
    CoverageGapDimensionRow, CoverageGapRow, CoverageGapSourceRow, DetailRows, EventRow, FeeRow,
    IncomeRow, LegRow, OfferExerciseRow, OpeningPositionRow, TaxRow, TradeRow,
    UnresolvedMovementRow, ValuationRow,
};
use crate::StoreError;

/// Loads the events named by `ids`, **in the order `ids` gave them**.
///
/// An id with no `events` row is silently absent from the result — this is
/// a bulk lookup for a page someone else already selected, not a check that
/// every id exists. Costs a fixed number of SQL statements regardless of
/// `ids.len()` (see the module doc); an empty page short-circuits to no
/// statements at all.
///
/// No production caller yet: Task 8's query builder is phase 1, the
/// selection this function's `ids` are meant to come from, and it has not
/// landed. Exercised today by this file's own tests.
#[allow(dead_code)]
pub(crate) fn hydrate(conn: &Connection, ids: &[EventId]) -> Result<Vec<Event>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let keys: Vec<String> = ids.iter().map(|id| id.inner().to_string()).collect();

    let headers = load_headers(conn, &keys)?;
    let legs = load_legs(conn, &keys)?;
    let details = load_details(conn, &keys)?;

    let mut events = Vec::with_capacity(ids.len());
    for key in &keys {
        let Some(header) = headers.get(key).cloned() else {
            continue;
        };
        let legs_for_event = legs.get(key).cloned().unwrap_or_default();
        let detail = details.get(key).cloned().unwrap_or(DetailRows::None);
        events.push(from_rows(header, legs_for_event, detail)?);
    }
    Ok(events)
}

/// [`hydrate`] narrowed to one id: `None` when it names no `events` row.
///
/// No production caller yet, for the same reason as [`hydrate`].
#[allow(dead_code)]
pub(crate) fn hydrate_one(conn: &Connection, id: EventId) -> Result<Option<Event>, StoreError> {
    Ok(hydrate(conn, std::slice::from_ref(&id))?.into_iter().next())
}

/// `?,?,...` for `count` placeholders, used to build an `IN (...)` clause
/// whose arity is not known until the page is in hand.
fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

fn load_headers(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, EventRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM events WHERE id IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(EventRow {
            id: row.get("id")?,
            owner: row.get("owner")?,
            account: row.get("account")?,
            kind: row.get("kind")?,
            effective_date: row.get("effective_date")?,
            source_time: row.get("source_time")?,
            sequence: row.get("sequence")?,
            confidence: row.get("confidence")?,
            relation_kind: row.get("relation_kind")?,
            relation_target: row.get("relation_target")?,
            idempotency_key: row.get("idempotency_key")?,
            recorded_at: row.get("recorded_at")?,
            date_trade: row.get("date_trade")?,
            date_settled: row.get("date_settled")?,
            date_cash_posted: row.get("date_cash_posted")?,
            date_entitlement: row.get("date_entitlement")?,
            date_paid: row.get("date_paid")?,
            tax_period_override: row.get("tax_period_override")?,
            source: row.get("source")?,
            raw_hash: row.get("raw_hash")?,
            parser_version: row.get("parser_version")?,
            source_operation_id: row.get("source_operation_id")?,
            source_category: row.get("source_category")?,
            source_kind: row.get("source_kind")?,
            owner_category: row.get("owner_category")?,
            source_code: row.get("source_code")?,
            source_description: row.get("source_description")?,
            import: row.get("import")?,
            import_session: row.get("import_session")?,
            declared_by: row.get("declared_by")?,
            rule_settlement: row.get("rule_settlement")?,
            settled_by_rule: row.get("settled_by_rule")?,
            settled_by_rule_version: row.get("settled_by_rule_version")?,
            row_document: row.get("row_document")?,
            row_sheet: row.get("row_sheet")?,
            row_number: row.get("row_number")?,
        })
    })?;

    let mut map = HashMap::with_capacity(ids.len());
    for row in rows {
        let header = row?;
        map.insert(header.id.clone(), header);
    }
    Ok(map)
}

fn load_legs(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, Vec<LegRow>>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_legs WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(LegRow {
            event: row.get("event")?,
            ordinal: row.get("ordinal")?,
            kind: row.get("kind")?,
            account: row.get("account")?,
            custody: row.get("custody")?,
            instrument: row.get("instrument")?,
            amount: row.get("amount")?,
            currency: row.get("currency")?,
            quantity: row.get("quantity")?,
        })
    })?;

    let mut map: HashMap<String, Vec<LegRow>> = HashMap::new();
    for row in rows {
        let leg = row?;
        map.entry(leg.event.clone()).or_default().push(leg);
    }
    Ok(map)
}

fn load_trade(conn: &Connection, ids: &[String]) -> Result<Vec<TradeRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_trade WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(TradeRow {
            event: row.get("event")?,
            side: row.get("side")?,
            instrument: row.get("instrument")?,
            quantity: row.get("quantity")?,
            gross_amount: row.get("gross_amount")?,
            gross_currency: row.get("gross_currency")?,
            fee_amount: row.get("fee_amount")?,
            fee_currency: row.get("fee_currency")?,
            basis_fee_amount: row.get("basis_fee_amount")?,
            basis_fee_currency: row.get("basis_fee_currency")?,
            basis_fee_exact_value: row.get("basis_fee_exact_value")?,
            basis_fee_exact_currency: row.get("basis_fee_exact_currency")?,
            accrued_interest_amount: row.get("accrued_interest_amount")?,
            accrued_interest_currency: row.get("accrued_interest_currency")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_cash_transfer(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<CashTransferRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_cash_transfer WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(CashTransferRow {
            event: row.get("event")?,
            transfer_id: row.get("transfer_id")?,
            from_account: row.get("from_account")?,
            to_account: row.get("to_account")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_unresolved_movement(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<UnresolvedMovementRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_unresolved_movement WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(UnresolvedMovementRow {
            event: row.get("event")?,
            amount: row.get("amount")?,
            currency: row.get("currency")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_income(conn: &Connection, ids: &[String]) -> Result<Vec<IncomeRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_income WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(IncomeRow {
            event: row.get("event")?,
            instrument: row.get("instrument")?,
            income_kind: row.get("income_kind")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_fee(conn: &Connection, ids: &[String]) -> Result<Vec<FeeRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_fee WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(FeeRow {
            event: row.get("event")?,
            origin: row.get("origin")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_tax(conn: &Connection, ids: &[String]) -> Result<Vec<TaxRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_tax WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(TaxRow {
            event: row.get("event")?,
            origin: row.get("origin")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_opening_position(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<OpeningPositionRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_opening_position WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(OpeningPositionRow {
            event: row.get("event")?,
            instrument: row.get("instrument")?,
            quantity: row.get("quantity")?,
            cost_basis_amount: row.get("cost_basis_amount")?,
            cost_basis_currency: row.get("cost_basis_currency")?,
            quantity_certainty: row.get("quantity_certainty")?,
            acquisition_date: row.get("acquisition_date")?,
            acquisition_date_certainty: row.get("acquisition_date_certainty")?,
            tax_basis_certainty: row.get("tax_basis_certainty")?,
            basis_currency: row.get("basis_currency")?,
            basis_rate: row.get("basis_rate")?,
            fees_included: row.get("fees_included")?,
            ldv_eligibility: row.get("ldv_eligibility")?,
            prior_corporate_actions: row.get("prior_corporate_actions")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_valuation(conn: &Connection, ids: &[String]) -> Result<Vec<ValuationRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_valuation WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(ValuationRow {
            event: row.get("event")?,
            instrument: row.get("instrument")?,
            price: row.get("price")?,
            currency: row.get("currency")?,
            quality: row.get("quality")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_control_assertion(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<ControlAssertionRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_control_assertion WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(ControlAssertionRow {
            event: row.get("event")?,
            period_from: row.get("period_from")?,
            period_to: row.get("period_to")?,
            claim_kind: row.get("claim_kind")?,
            balance_point: row.get("balance_point")?,
            currency: row.get("currency")?,
            amount: row.get("amount")?,
            instrument: row.get("instrument")?,
            custody: row.get("custody")?,
            quantity: row.get("quantity")?,
            debit: row.get("debit")?,
            credit: row.get("credit")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_coverage_gap_header(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<CoverageGapRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_coverage_gap WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(CoverageGapRow {
            event: row.get("event")?,
            period_from: row.get("period_from")?,
            period_to: row.get("period_to")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_coverage_gap_sources(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<CoverageGapSourceRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_coverage_gap_rows WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(CoverageGapSourceRow {
            event: row.get("event")?,
            ordinal: row.get("ordinal")?,
            source: row.get("source")?,
            row_name_kind: row.get("row_name_kind")?,
            row_name_value: row.get("row_name_value")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_coverage_gap_dimensions(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<CoverageGapDimensionRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_coverage_gap_row_dimensions WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(CoverageGapDimensionRow {
            event: row.get("event")?,
            ordinal: row.get("ordinal")?,
            dimension: row.get("dimension")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_corporate_action(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<CorporateActionRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_corporate_action WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(CorporateActionRow {
            event: row.get("event")?,
            action_kind: row.get("action_kind")?,
            instrument: row.get("instrument")?,
            predecessor: row.get("predecessor")?,
            successor: row.get("successor")?,
            custody: row.get("custody")?,
            quantity: row.get("quantity")?,
            quantity_in: row.get("quantity_in")?,
            quantity_out: row.get("quantity_out")?,
            ratio: row.get("ratio")?,
            principal_returned_per_unit_value: row.get("principal_returned_per_unit_value")?,
            principal_returned_per_unit_currency: row
                .get("principal_returned_per_unit_currency")?,
            fractional: row.get("fractional")?,
            basis_transfer: row.get("basis_transfer")?,
            effective_date: row.get("effective_date")?,
            record_date: row.get("record_date")?,
            grounds: row.get("grounds")?,
            allocation_kind: row.get("allocation_kind")?,
            allocation_gap: row.get("allocation_gap")?,
            allocation_share: row.get("allocation_share")?,
            allocation_inputs_hash: row.get("allocation_inputs_hash")?,
            allocation_known_as_of: row.get("allocation_known_as_of")?,
            allocation_algorithm: row.get("allocation_algorithm")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn load_offer_exercise(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<OfferExerciseRow>, StoreError> {
    let sql = format!(
        "SELECT * FROM event_offer_exercise WHERE event IN ({})",
        placeholders(ids.len())
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids), |row| {
        Ok(OfferExerciseRow {
            event: row.get("event")?,
            action_kind: row.get("action_kind")?,
            submission: row.get("submission")?,
            window: row.get("window")?,
            instrument: row.get("instrument")?,
            custody: row.get("custody")?,
            quantity: row.get("quantity")?,
            gross_amount: row.get("gross_amount")?,
            gross_currency: row.get("gross_currency")?,
            fee_amount: row.get("fee_amount")?,
            fee_currency: row.get("fee_currency")?,
            accrued_interest_amount: row.get("accrued_interest_amount")?,
            accrued_interest_currency: row.get("accrued_interest_currency")?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

/// Every detail table's rows for `ids`, folded into one map keyed by event
/// id. Each id matches at most one of the twelve tables — `to_rows` never
/// writes a detail row to two families for the same event — so a plain
/// `insert` is safe here; the five kinds with nothing beyond their legs
/// simply leave no entry, and [`hydrate`] treats that absence as
/// [`DetailRows::None`].
fn load_details(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, DetailRows>, StoreError> {
    let mut map = HashMap::with_capacity(ids.len());

    for row in load_trade(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::Trade(row));
    }
    for row in load_cash_transfer(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::CashTransfer(row));
    }
    for row in load_unresolved_movement(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::UnresolvedMovement(row));
    }
    for row in load_income(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::Income(row));
    }
    for row in load_fee(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::Fee(row));
    }
    for row in load_tax(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::Tax(row));
    }
    for row in load_opening_position(conn, ids)? {
        map.insert(
            row.event.clone(),
            DetailRows::OpeningPosition(Box::new(row)),
        );
    }
    for row in load_valuation(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::Valuation(row));
    }
    for row in load_control_assertion(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::ControlAssertion(row));
    }
    for row in load_corporate_action(conn, ids)? {
        map.insert(
            row.event.clone(),
            DetailRows::CorporateAction(Box::new(row)),
        );
    }
    for row in load_offer_exercise(conn, ids)? {
        map.insert(row.event.clone(), DetailRows::OfferExercise(row));
    }

    // The collection family: always three statements, whether or not any id
    // in this page is an `ImportCoverageGap` — the fixed-statement-count
    // promise holds over the whole page, not per kind.
    let coverage_gap_headers = load_coverage_gap_header(conn, ids)?;
    let mut coverage_gap_sources: HashMap<String, Vec<CoverageGapSourceRow>> = HashMap::new();
    for row in load_coverage_gap_sources(conn, ids)? {
        coverage_gap_sources
            .entry(row.event.clone())
            .or_default()
            .push(row);
    }
    let mut coverage_gap_dimensions: HashMap<String, Vec<CoverageGapDimensionRow>> = HashMap::new();
    for row in load_coverage_gap_dimensions(conn, ids)? {
        coverage_gap_dimensions
            .entry(row.event.clone())
            .or_default()
            .push(row);
    }
    for header in coverage_gap_headers {
        let event = header.event.clone();
        let sources = coverage_gap_sources.remove(&event).unwrap_or_default();
        let dimensions = coverage_gap_dimensions.remove(&event).unwrap_or_default();
        map.insert(
            event,
            DetailRows::CoverageGap(CoverageGapDetail {
                header,
                sources,
                dimensions,
            }),
        );
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use iaam_core::dates::{EffectiveOrder, EventDates};
    use iaam_core::event::kind::{EventKind, TradeSide};
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
    use iaam_core::event::{Confidence, Event, Relation};
    use iaam_core::ids::{
        AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId, TransferId,
    };
    use iaam_core::instrument::CurrencyRoles;
    use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use iaam_core::reconciliation::Dimension;
    use iaam_core::reconciliation::claim::AssertionPeriod;
    use rusqlite::params;
    use rusqlite::trace::{TraceEvent, TraceEventCodes};
    use rust_decimal::Decimal;
    use time::macros::date;

    use super::super::kind::to_rows;
    use super::super::rows::{
        CashTransferRow, ControlAssertionRow, CorporateActionRow, CoverageGapDetail,
        CoverageGapDimensionRow, CoverageGapRow, CoverageGapSourceRow, DetailRows, EventRow,
        FeeRow, IncomeRow, LegRow, OfferExerciseRow, OpeningPositionRow, TaxRow, TradeRow,
        UnresolvedMovementRow, ValuationRow,
    };
    use super::{hydrate, hydrate_one};
    use crate::SqliteStore;
    use crate::reference::{AccountRecord, CustodyRecord, InstrumentRecord};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(text: &str) -> Quantity {
        Quantity(Dec::new(Decimal::from_str_exact(text).unwrap()))
    }

    /// Reference rows a test event's account, instrument and custody place
    /// must exist against — invented from scratch, never derived from a
    /// real export (CLAUDE.md).
    struct Fixture {
        owner: OwnerId,
        account: AccountId,
        other_account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
        source: SourceId,
    }

    impl Fixture {
        fn new(store: &SqliteStore) -> Self {
            let owner = OwnerId::new_random();
            let account = AccountId::new_random();
            let other_account = AccountId::new_random();
            let instrument = InstrumentId::new_random();
            let custody = CustodyId::new_random();
            let source = SourceId::new_random();

            store
                .upsert_account(&AccountRecord {
                    id: account,
                    owner,
                    title: "Main".to_owned(),
                    institution: Some("Test Bank".to_owned()),
                })
                .expect("account created");
            store
                .upsert_account(&AccountRecord {
                    id: other_account,
                    owner,
                    title: "Savings".to_owned(),
                    institution: Some("Test Bank".to_owned()),
                })
                .expect("other account created");
            store
                .upsert_instrument(&InstrumentRecord {
                    id: instrument,
                    kind: None,
                    symbol: "TESTBOND".to_owned(),
                    title: "Test Bond".to_owned(),
                    currencies: CurrencyRoles::uniform(CurrencyCode::Rub),
                    lineage: None,
                })
                .expect("instrument created");
            store
                .upsert_custody_place(&CustodyRecord {
                    id: custody,
                    owner,
                    title: "Shop One Custody".to_owned(),
                    institution: None,
                })
                .expect("custody place created");

            Self {
                owner,
                account,
                other_account,
                instrument,
                custody,
                source,
            }
        }

        fn event(&self, sequence: u32, kind: EventKind, legs: Vec<Leg>) -> Event {
            Event {
                id: EventId::new_random(),
                owner: self.owner,
                account: self.account,
                kind,
                dates: EventDates::empty(),
                order: EffectiveOrder::new(date!(2026 - 01 - 01), sequence),
                legs,
                provenance: Provenance::new(
                    self.source,
                    RawHash::parse(&"a".repeat(64)).unwrap(),
                    ParserVersion("test/1".to_owned()),
                ),
                relation: Relation::None,
                confidence: Confidence::Known,
                idempotency_key: None,
            }
        }
    }

    /// Writes `event` through the same `to_rows` mapper the write path will
    /// use (Task 7 has not landed yet, so this is the test's own insert,
    /// not a production one) and returns its id.
    fn insert(conn: &rusqlite::Connection, event: &Event) -> EventId {
        let (mut header, legs, detail) = to_rows(event).expect("to_rows");
        header.recorded_at = crate::now();
        insert_header(conn, &header);
        for leg in &legs {
            insert_leg(conn, leg);
        }
        insert_detail(conn, detail);
        event.id
    }

    fn insert_header(conn: &rusqlite::Connection, header: &EventRow) {
        conn.execute(
            "INSERT INTO events (
                id, owner, account, kind, effective_date, source_time, sequence, confidence,
                relation_kind, relation_target, idempotency_key, recorded_at,
                date_trade, date_settled, date_cash_posted, date_entitlement, date_paid,
                tax_period_override,
                source, raw_hash, parser_version, source_operation_id, source_category,
                source_kind, owner_category, source_code, source_description, import,
                import_session, declared_by, rule_settlement, settled_by_rule,
                settled_by_rule_version, row_document, row_sheet, row_number
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28,
                ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36
            )",
            params![
                header.id,
                header.owner,
                header.account,
                header.kind,
                header.effective_date,
                header.source_time,
                header.sequence,
                header.confidence,
                header.relation_kind,
                header.relation_target,
                header.idempotency_key,
                header.recorded_at,
                header.date_trade,
                header.date_settled,
                header.date_cash_posted,
                header.date_entitlement,
                header.date_paid,
                header.tax_period_override,
                header.source,
                header.raw_hash,
                header.parser_version,
                header.source_operation_id,
                header.source_category,
                header.source_kind,
                header.owner_category,
                header.source_code,
                header.source_description,
                header.import,
                header.import_session,
                header.declared_by,
                header.rule_settlement,
                header.settled_by_rule,
                header.settled_by_rule_version,
                header.row_document,
                header.row_sheet,
                header.row_number,
            ],
        )
        .expect("insert event header");
    }

    fn insert_leg(conn: &rusqlite::Connection, leg: &LegRow) {
        conn.execute(
            "INSERT INTO event_legs
                 (event, ordinal, kind, account, custody, instrument, amount, currency, quantity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                leg.event,
                leg.ordinal,
                leg.kind,
                leg.account,
                leg.custody,
                leg.instrument,
                leg.amount,
                leg.currency,
                leg.quantity,
            ],
        )
        .expect("insert leg");
    }

    fn insert_detail(conn: &rusqlite::Connection, detail: DetailRows) {
        match detail {
            DetailRows::None => {}
            DetailRows::Trade(row) => insert_trade(conn, &row),
            DetailRows::CashTransfer(row) => insert_cash_transfer(conn, &row),
            DetailRows::UnresolvedMovement(row) => insert_unresolved_movement(conn, &row),
            DetailRows::Income(row) => insert_income(conn, &row),
            DetailRows::Fee(row) => insert_fee(conn, &row),
            DetailRows::Tax(row) => insert_tax(conn, &row),
            DetailRows::OpeningPosition(row) => insert_opening_position(conn, &row),
            DetailRows::Valuation(row) => insert_valuation(conn, &row),
            DetailRows::ControlAssertion(row) => insert_control_assertion(conn, &row),
            DetailRows::CoverageGap(detail) => insert_coverage_gap(conn, &detail),
            DetailRows::CorporateAction(row) => insert_corporate_action(conn, &row),
            DetailRows::OfferExercise(row) => insert_offer_exercise(conn, &row),
        }
    }

    fn insert_trade(conn: &rusqlite::Connection, row: &TradeRow) {
        conn.execute(
            "INSERT INTO event_trade (
                event, side, instrument, quantity, gross_amount, gross_currency,
                fee_amount, fee_currency, basis_fee_amount, basis_fee_currency,
                basis_fee_exact_value, basis_fee_exact_currency,
                accrued_interest_amount, accrued_interest_currency
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                row.event,
                row.side,
                row.instrument,
                row.quantity,
                row.gross_amount,
                row.gross_currency,
                row.fee_amount,
                row.fee_currency,
                row.basis_fee_amount,
                row.basis_fee_currency,
                row.basis_fee_exact_value,
                row.basis_fee_exact_currency,
                row.accrued_interest_amount,
                row.accrued_interest_currency,
            ],
        )
        .expect("insert trade detail");
    }

    fn insert_cash_transfer(conn: &rusqlite::Connection, row: &CashTransferRow) {
        conn.execute(
            "INSERT INTO event_cash_transfer (event, transfer_id, from_account, to_account)
             VALUES (?1,?2,?3,?4)",
            params![row.event, row.transfer_id, row.from_account, row.to_account],
        )
        .expect("insert cash transfer detail");
    }

    fn insert_unresolved_movement(conn: &rusqlite::Connection, row: &UnresolvedMovementRow) {
        conn.execute(
            "INSERT INTO event_unresolved_movement (event, amount, currency) VALUES (?1,?2,?3)",
            params![row.event, row.amount, row.currency],
        )
        .expect("insert unresolved movement detail");
    }

    fn insert_income(conn: &rusqlite::Connection, row: &IncomeRow) {
        conn.execute(
            "INSERT INTO event_income (event, instrument, income_kind) VALUES (?1,?2,?3)",
            params![row.event, row.instrument, row.income_kind],
        )
        .expect("insert income detail");
    }

    fn insert_fee(conn: &rusqlite::Connection, row: &FeeRow) {
        conn.execute(
            "INSERT INTO event_fee (event, origin) VALUES (?1,?2)",
            params![row.event, row.origin],
        )
        .expect("insert fee detail");
    }

    fn insert_tax(conn: &rusqlite::Connection, row: &TaxRow) {
        conn.execute(
            "INSERT INTO event_tax (event, origin) VALUES (?1,?2)",
            params![row.event, row.origin],
        )
        .expect("insert tax detail");
    }

    fn insert_opening_position(conn: &rusqlite::Connection, row: &OpeningPositionRow) {
        conn.execute(
            "INSERT INTO event_opening_position (
                event, instrument, quantity, cost_basis_amount, cost_basis_currency,
                quantity_certainty, acquisition_date, acquisition_date_certainty,
                tax_basis_certainty, basis_currency, basis_rate, fees_included,
                ldv_eligibility, prior_corporate_actions
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                row.event,
                row.instrument,
                row.quantity,
                row.cost_basis_amount,
                row.cost_basis_currency,
                row.quantity_certainty,
                row.acquisition_date,
                row.acquisition_date_certainty,
                row.tax_basis_certainty,
                row.basis_currency,
                row.basis_rate,
                row.fees_included,
                row.ldv_eligibility,
                row.prior_corporate_actions,
            ],
        )
        .expect("insert opening position detail");
    }

    fn insert_valuation(conn: &rusqlite::Connection, row: &ValuationRow) {
        conn.execute(
            "INSERT INTO event_valuation (event, instrument, price, currency, quality)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                row.event,
                row.instrument,
                row.price,
                row.currency,
                row.quality
            ],
        )
        .expect("insert valuation detail");
    }

    fn insert_control_assertion(conn: &rusqlite::Connection, row: &ControlAssertionRow) {
        conn.execute(
            "INSERT INTO event_control_assertion (
                event, period_from, period_to, claim_kind, balance_point, currency,
                amount, instrument, custody, quantity, debit, credit
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                row.event,
                row.period_from,
                row.period_to,
                row.claim_kind,
                row.balance_point,
                row.currency,
                row.amount,
                row.instrument,
                row.custody,
                row.quantity,
                row.debit,
                row.credit,
            ],
        )
        .expect("insert control assertion detail");
    }

    fn insert_coverage_gap(conn: &rusqlite::Connection, detail: &CoverageGapDetail) {
        insert_coverage_gap_header(conn, &detail.header);
        for row in &detail.sources {
            insert_coverage_gap_source(conn, row);
        }
        for row in &detail.dimensions {
            insert_coverage_gap_dimension(conn, row);
        }
    }

    fn insert_coverage_gap_header(conn: &rusqlite::Connection, row: &CoverageGapRow) {
        conn.execute(
            "INSERT INTO event_coverage_gap (event, period_from, period_to) VALUES (?1,?2,?3)",
            params![row.event, row.period_from, row.period_to],
        )
        .expect("insert coverage gap header");
    }

    fn insert_coverage_gap_source(conn: &rusqlite::Connection, row: &CoverageGapSourceRow) {
        conn.execute(
            "INSERT INTO event_coverage_gap_rows
                 (event, ordinal, source, row_name_kind, row_name_value)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                row.event,
                row.ordinal,
                row.source,
                row.row_name_kind,
                row.row_name_value,
            ],
        )
        .expect("insert coverage gap source row");
    }

    fn insert_coverage_gap_dimension(conn: &rusqlite::Connection, row: &CoverageGapDimensionRow) {
        conn.execute(
            "INSERT INTO event_coverage_gap_row_dimensions (event, ordinal, dimension)
             VALUES (?1,?2,?3)",
            params![row.event, row.ordinal, row.dimension],
        )
        .expect("insert coverage gap dimension row");
    }

    fn insert_corporate_action(conn: &rusqlite::Connection, row: &CorporateActionRow) {
        conn.execute(
            "INSERT INTO event_corporate_action (
                event, action_kind, instrument, predecessor, successor, custody,
                quantity, quantity_in, quantity_out, ratio,
                principal_returned_per_unit_value, principal_returned_per_unit_currency,
                fractional, basis_transfer, effective_date, record_date, grounds,
                allocation_kind, allocation_gap, allocation_share,
                allocation_inputs_hash, allocation_known_as_of, allocation_algorithm
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)",
            params![
                row.event,
                row.action_kind,
                row.instrument,
                row.predecessor,
                row.successor,
                row.custody,
                row.quantity,
                row.quantity_in,
                row.quantity_out,
                row.ratio,
                row.principal_returned_per_unit_value,
                row.principal_returned_per_unit_currency,
                row.fractional,
                row.basis_transfer,
                row.effective_date,
                row.record_date,
                row.grounds,
                row.allocation_kind,
                row.allocation_gap,
                row.allocation_share,
                row.allocation_inputs_hash,
                row.allocation_known_as_of,
                row.allocation_algorithm,
            ],
        )
        .expect("insert corporate action detail");
    }

    fn insert_offer_exercise(conn: &rusqlite::Connection, row: &OfferExerciseRow) {
        conn.execute(
            "INSERT INTO event_offer_exercise (
                event, action_kind, submission, window, instrument, custody, quantity,
                gross_amount, gross_currency, fee_amount, fee_currency,
                accrued_interest_amount, accrued_interest_currency
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                row.event,
                row.action_kind,
                row.submission,
                row.window,
                row.instrument,
                row.custody,
                row.quantity,
                row.gross_amount,
                row.gross_currency,
                row.fee_amount,
                row.fee_currency,
                row.accrued_interest_amount,
                row.accrued_interest_currency,
            ],
        )
        .expect("insert offer exercise detail");
    }

    /// `n` events cycling through four shapes: a leg-only kind with no
    /// detail table (`CashIn`), a detail-table kind (`Trade`), a two-leg,
    /// two-endpoint kind (`CashTransfer`), and a collection-child kind
    /// (`ImportCoverageGap`) — so a page mixes every shape hydration must
    /// handle.
    fn store_with_events(n: usize) -> (SqliteStore, Vec<EventId>) {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        let fixture = Fixture::new(&store);
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            let sequence = u32::try_from(i).unwrap();
            let event = match i % 4 {
                0 => fixture.event(
                    sequence,
                    EventKind::CashIn { amount: rub(1_000) },
                    vec![Leg::cash(fixture.account, rub(1_000))],
                ),
                1 => fixture.event(
                    sequence,
                    EventKind::Trade {
                        side: TradeSide::Buy,
                        instrument: fixture.instrument,
                        quantity: qty("10"),
                        gross: rub(-100_000),
                        fee: None,
                        basis_fee: None,
                        basis_fee_exact: None,
                        accrued_interest: None,
                    },
                    vec![
                        Leg::cash(fixture.account, rub(-100_000)),
                        Leg::security(
                            fixture.account,
                            fixture.custody,
                            fixture.instrument,
                            qty("10"),
                        ),
                    ],
                ),
                2 => fixture.event(
                    sequence,
                    EventKind::CashTransfer {
                        transfer_id: TransferId::new_random(),
                        from: fixture.account,
                        to: fixture.other_account,
                        amount: rub(5_000),
                    },
                    vec![
                        Leg::cash(fixture.account, rub(-5_000)),
                        Leg::cash(fixture.other_account, rub(5_000)),
                    ],
                ),
                _ => fixture.event(
                    sequence,
                    EventKind::ImportCoverageGap {
                        period: AssertionPeriod::between(
                            date!(2026 - 01 - 01),
                            date!(2026 - 01 - 31),
                        )
                        .unwrap(),
                        dimensions: BTreeSet::from([Dimension::Cash]),
                        refused: 1,
                        rows: vec![RefusedRow {
                            key: SourceRowKey {
                                source: fixture.source,
                                row: RowName::Given(format!("OP-{i}")),
                            },
                            dimensions: BTreeSet::from([Dimension::Cash]),
                        }],
                    },
                    vec![],
                ),
            };
            ids.push(insert(store.connection(), &event));
        }
        (store, ids)
    }

    static TRACE_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn count_statement(event: TraceEvent<'_>) {
        if let TraceEvent::Stmt(_, _) = event {
            TRACE_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Runs `hydrate` over `ids` with the statement tracer armed, and
    /// returns how many `SQLITE_TRACE_STMT` events it produced — one per
    /// prepared statement actually run, so this counts statements, not
    /// rows, and is oblivious to how large `ids` is.
    fn hydrate_and_count(store: &SqliteStore, ids: &[EventId]) -> (usize, usize) {
        store
            .connection()
            .trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(count_statement));
        TRACE_COUNT.store(0, Ordering::SeqCst);
        let events = hydrate(store.connection(), ids).expect("hydrate");
        let statements = TRACE_COUNT.load(Ordering::SeqCst);
        store
            .connection()
            .trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, None);
        (statements, events.len())
    }

    #[test]
    fn hydrating_a_page_costs_a_fixed_number_of_statements() {
        let (store50, ids50) = store_with_events(50);
        let (store5, ids5) = store_with_events(5);

        let (counted50, found50) = hydrate_and_count(&store50, &ids50);
        let (counted5, found5) = hydrate_and_count(&store5, &ids5);

        assert_eq!(found50, 50, "every id in the 50-event page resolves");
        assert_eq!(found5, 5, "every id in the 5-event page resolves");
        assert!(
            counted50 > 0,
            "the trace probe must observe some statements"
        );
        assert_eq!(
            counted50, counted5,
            "statement count must not grow with the page: {counted50} for 50 ids, {counted5} for 5"
        );
    }

    #[test]
    fn hydrate_preserves_the_callers_order() {
        let (store, ids) = store_with_events(6);
        let mut reversed = ids.clone();
        reversed.reverse();

        let events = hydrate(store.connection(), &reversed).expect("hydrate");

        let returned_ids: Vec<EventId> = events.iter().map(|event| event.id).collect();
        assert_eq!(
            returned_ids, reversed,
            "hydrate must return events in the given order"
        );
    }

    #[test]
    fn a_missing_id_is_absent_rather_than_an_error() {
        let (store, ids) = store_with_events(2);
        let missing = EventId::new_random();
        let requested = vec![ids[0], missing, ids[1]];

        let events = hydrate(store.connection(), &requested).expect("hydrate");

        let returned_ids: Vec<EventId> = events.iter().map(|event| event.id).collect();
        assert_eq!(
            returned_ids,
            vec![ids[0], ids[1]],
            "the missing id is skipped, not errored or nulled"
        );
    }

    #[test]
    fn hydrate_one_finds_a_single_event() {
        let (store, ids) = store_with_events(3);

        let found = hydrate_one(store.connection(), ids[1]).expect("hydrate_one");

        assert_eq!(found.map(|event| event.id), Some(ids[1]));
    }

    #[test]
    fn hydrate_one_returns_none_for_a_missing_id() {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        let missing = EventId::new_random();

        let found = hydrate_one(store.connection(), missing).expect("hydrate_one");

        assert_eq!(found, None);
    }

    #[test]
    fn hydrate_reassembles_every_shape_in_one_mixed_page() {
        // Four kinds in one page: no detail table, a detail table, two
        // legs naming both endpoints, and a collection child table.
        let (store, ids) = store_with_events(4);

        let events = hydrate(store.connection(), &ids).expect("hydrate");

        assert_eq!(events.len(), 4);
        assert!(matches!(events[0].kind, EventKind::CashIn { .. }));
        assert!(matches!(events[1].kind, EventKind::Trade { .. }));
        assert!(matches!(events[2].kind, EventKind::CashTransfer { .. }));
        assert!(matches!(
            events[3].kind,
            EventKind::ImportCoverageGap { .. }
        ));
        assert_eq!(events[1].legs.len(), 2, "the trade keeps both its legs");
        assert_eq!(
            events[2].legs.len(),
            2,
            "the transfer keeps both endpoints' legs"
        );
        assert!(
            events[0].legs.len() == 1,
            "the leg-only kind keeps its single leg"
        );
    }

    #[test]
    fn hydrate_of_an_empty_page_issues_no_statements() {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        let events = hydrate(store.connection(), &[]).expect("hydrate");
        assert!(events.is_empty());
    }
}
