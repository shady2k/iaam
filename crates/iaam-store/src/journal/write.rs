//! The one insert primitive (spec §5, §D6).
//!
//! [`insert_event_in`] is the only place in this crate that writes a whole
//! event. It takes an **already open** transaction and opens none of its
//! own: the caller decides where the transaction boundary is, because one of
//! the three callers (the bundle importer) already holds one transaction
//! across many events and must not have this primitive open a nested one.
//!
//! Append-only is a property of what this crate's API publishes, not of a
//! trigger: there is no second way to write a fact into the journal, and no
//! `UPDATE` or `DELETE` is published for one either (spec §D6).
//!
//! Completeness is a different property from atomicity, and this file is
//! also where it lives. A `Transaction` is all-or-nothing by construction —
//! an error rolls back, and an unwinding panic drops the transaction, whose
//! `Drop` rolls back too — but nothing in SQLite stops a mapper that forgot a
//! leg or a detail row from returning `Ok` anyway. [`insert_event_in`] reads
//! the event back, inside the same transaction, and compares it to what it
//! was given; unequal or missing is [`StoreError::IncompleteWrite`], which
//! rolls the whole write back just as a genuine SQL error would.
//!
//! Once the read-back confirms the write is whole, [`insert_event_in`] also
//! gives the event its `event_category_assignments` row through
//! [`category_index::assign_for`] (spec §4.7), inside the same transaction:
//! every writer reaches the journal only through this primitive, so this is
//! the one place that can guarantee no appended event is ever missing from
//! the projection.

use rusqlite::{Transaction, params};

use iaam_core::event::Event;
use iaam_core::ids::OwnerId;

use super::category_index;
use super::kind::to_rows;
use super::read::hydrate_one;
use super::rows::{
    CashTransferRow, ControlAssertionRow, CorporateActionRow, CoverageGapDetail, DetailRows,
    EventRow, FeeRow, IncomeRow, LegRow, OfferExerciseRow, OpeningPositionRow, TaxRow, TradeRow,
    UnresolvedMovementRow, ValuationRow,
};
use crate::StoreError;

/// Every `(table, column)` pair in `0001_schema.sql` where a table whose
/// name starts with `event` declares `REFERENCES custody_places`.
///
/// This is the ground truth [`Event::referenced_custodies`] promises to
/// cover: `event_legs.custody` through the leg loop, the other three
/// through the exhaustive `match` over the event's detail. Re-exported (via
/// `crate::journal`) so that `schema_coverage.rs` — an integration test,
/// outside this crate's own boundary — can hold the schema to it: growing
/// this list without adding the matching arm is exactly the drift that test
/// exists to catch.
pub const CUSTODY_REFERENCE_COLUMNS: &[(&str, &str)] = &[
    ("event_legs", "custody"),
    ("event_control_assertion", "custody"),
    ("event_corporate_action", "custody"),
    ("event_offer_exercise", "custody"),
];

/// Inserts `event` as a whole fact: the header, its legs, its family detail
/// row and any collection rows, inside `tx`.
///
/// Order among those inserts is free — there are no triggers and no deferred
/// sealing to respect (spec §5) — so this inserts the header, then the legs,
/// then the detail, purely because that is the order [`to_rows`] hands them
/// back in.
///
/// Does not commit `tx`: that is the caller's decision, since the bundle
/// importer holds one transaction across many events and must not have this
/// primitive commit or open one of its own.
pub(crate) fn insert_event_in(tx: &Transaction<'_>, event: &Event) -> Result<(), StoreError> {
    ensure_accounts_and_custodies_are_owned(tx, event)?;

    let (mut header, legs, detail) = to_rows(event)?;
    // `Event` carries no field for this: it is stamped from the clock at
    // insert time. `to_rows` leaves it empty on purpose (spec §5, rows.rs).
    header.recorded_at = crate::now();

    insert_header(tx, &header)?;
    for leg in &legs {
        insert_leg(tx, leg)?;
    }
    insert_detail(tx, &detail)?;

    match hydrate_one(tx, event.id) {
        Ok(Some(reconstructed)) if &reconstructed == event => {}
        // Missing, mismatched, or not even decodable — all three are the
        // write coming back incomplete, and the caller does not need to
        // distinguish them: the transaction rolls back either way (spec §D6).
        Ok(Some(_) | None) | Err(_) => {
            return Err(StoreError::IncompleteWrite {
                event: event.id.inner().to_string(),
            });
        }
    }

    // The category-assignment projection (spec §4.7) is kept current for
    // every writer through this one primitive, rather than each caller
    // remembering to call it: the ordinary append path and the bundle
    // importer both reach the journal only through here.
    category_index::assign_for(tx, event.owner, std::slice::from_ref(event))
}

/// Checks that every account a leg names, and every custody place the event
/// names anywhere, belongs to the event's owner (spec §4.2).
///
/// `events.account` needs no such check: the schema's own composite
/// `FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)` already
/// refuses it. A leg's `account` and a custody column carry no `owner`
/// column to declare that key against — duplicating `owner` onto every
/// child table for tenant isolation in a single-owner system was not judged
/// worth it — so this check exists in code or it does not exist at all.
///
/// Accounts are still read off the legs: every account this journal moves
/// money or securities through appears there. Custody places are not — a
/// control assertion has no legs at all, and a partial redemption's only leg
/// is the cash — so custody is checked over
/// [`Event::referenced_custodies`], the one traversal that also reaches the
/// detail rows.
///
/// An account or custody place that exists but belongs to someone else is
/// reported the same way as one that does not exist at all: an identifier is
/// a globally unique UUID, and telling the two apart would tell the caller a
/// stranger's record exists (the same choice `event_chain` makes for a
/// correction target belonging to another owner, in `events.rs`).
fn ensure_accounts_and_custodies_are_owned(
    tx: &Transaction<'_>,
    event: &Event,
) -> Result<(), StoreError> {
    for leg in &event.legs {
        ensure_owned(tx, "accounts", "account", event.owner, leg.account.inner())?;
    }
    for custody in event.referenced_custodies() {
        ensure_owned(
            tx,
            "custody_places",
            "custody place",
            event.owner,
            custody.inner(),
        )?;
    }
    Ok(())
}

fn ensure_owned(
    tx: &Transaction<'_>,
    table: &'static str,
    what: &'static str,
    owner: OwnerId,
    id: uuid::Uuid,
) -> Result<(), StoreError> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE owner = ?1 AND id = ?2)");
    let owned: bool = tx.query_row(
        &sql,
        params![owner.inner().to_string(), id.to_string()],
        |row| row.get(0),
    )?;
    if owned {
        Ok(())
    } else {
        Err(StoreError::NotFound {
            what,
            id: id.to_string(),
        })
    }
}

fn insert_header(tx: &Transaction<'_>, header: &EventRow) -> Result<(), StoreError> {
    tx.execute(
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
    )?;
    Ok(())
}

fn insert_leg(tx: &Transaction<'_>, leg: &LegRow) -> Result<(), StoreError> {
    tx.execute(
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
    )?;
    Ok(())
}

fn insert_detail(tx: &Transaction<'_>, detail: &DetailRows) -> Result<(), StoreError> {
    match detail {
        DetailRows::None => Ok(()),
        DetailRows::Trade(row) => insert_trade(tx, row),
        DetailRows::CashTransfer(row) => insert_cash_transfer(tx, row),
        DetailRows::UnresolvedMovement(row) => insert_unresolved_movement(tx, row),
        DetailRows::Income(row) => insert_income(tx, row),
        DetailRows::Fee(row) => insert_fee(tx, row),
        DetailRows::Tax(row) => insert_tax(tx, row),
        DetailRows::OpeningPosition(row) => insert_opening_position(tx, row),
        DetailRows::Valuation(row) => insert_valuation(tx, row),
        DetailRows::ControlAssertion(row) => insert_control_assertion(tx, row),
        DetailRows::CoverageGap(detail) => insert_coverage_gap(tx, detail),
        DetailRows::CorporateAction(row) => insert_corporate_action(tx, row),
        DetailRows::OfferExercise(row) => insert_offer_exercise(tx, row),
    }
}

fn insert_trade(tx: &Transaction<'_>, row: &TradeRow) -> Result<(), StoreError> {
    tx.execute(
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
    )?;
    Ok(())
}

fn insert_cash_transfer(tx: &Transaction<'_>, row: &CashTransferRow) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO event_cash_transfer (event, transfer_id, from_account, to_account)
         VALUES (?1,?2,?3,?4)",
        params![row.event, row.transfer_id, row.from_account, row.to_account],
    )?;
    Ok(())
}

fn insert_unresolved_movement(
    tx: &Transaction<'_>,
    row: &UnresolvedMovementRow,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO event_unresolved_movement (event, amount, currency) VALUES (?1,?2,?3)",
        params![row.event, row.amount, row.currency],
    )?;
    Ok(())
}

fn insert_income(tx: &Transaction<'_>, row: &IncomeRow) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO event_income (event, instrument, income_kind) VALUES (?1,?2,?3)",
        params![row.event, row.instrument, row.income_kind],
    )?;
    Ok(())
}

fn insert_fee(tx: &Transaction<'_>, row: &FeeRow) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO event_fee (event, origin) VALUES (?1,?2)",
        params![row.event, row.origin],
    )?;
    Ok(())
}

fn insert_tax(tx: &Transaction<'_>, row: &TaxRow) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO event_tax (event, origin) VALUES (?1,?2)",
        params![row.event, row.origin],
    )?;
    Ok(())
}

fn insert_opening_position(
    tx: &Transaction<'_>,
    row: &OpeningPositionRow,
) -> Result<(), StoreError> {
    tx.execute(
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
    )?;
    Ok(())
}

fn insert_valuation(tx: &Transaction<'_>, row: &ValuationRow) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO event_valuation (event, instrument, price, currency, quality)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            row.event,
            row.instrument,
            row.price,
            row.currency,
            row.quality
        ],
    )?;
    Ok(())
}

fn insert_control_assertion(
    tx: &Transaction<'_>,
    row: &ControlAssertionRow,
) -> Result<(), StoreError> {
    tx.execute(
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
    )?;
    Ok(())
}

fn insert_coverage_gap(tx: &Transaction<'_>, detail: &CoverageGapDetail) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO event_coverage_gap (event, period_from, period_to) VALUES (?1,?2,?3)",
        params![
            detail.header.event,
            detail.header.period_from,
            detail.header.period_to
        ],
    )?;
    for row in &detail.sources {
        tx.execute(
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
        )?;
    }
    for row in &detail.dimensions {
        tx.execute(
            "INSERT INTO event_coverage_gap_row_dimensions (event, ordinal, dimension)
             VALUES (?1,?2,?3)",
            params![row.event, row.ordinal, row.dimension],
        )?;
    }
    Ok(())
}

fn insert_corporate_action(
    tx: &Transaction<'_>,
    row: &CorporateActionRow,
) -> Result<(), StoreError> {
    tx.execute(
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
    )?;
    Ok(())
}

fn insert_offer_exercise(tx: &Transaction<'_>, row: &OfferExerciseRow) -> Result<(), StoreError> {
    tx.execute(
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
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};

    use iaam_core::dates::{EffectiveOrder, EventDates};
    use iaam_core::event::allocation::BasisAllocation;
    use iaam_core::event::corporate_action::CorporateAction;
    use iaam_core::event::kind::{EventKind, TradeSide};
    use iaam_core::event::leg::Leg;
    use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId};
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Event, Relation};
    use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
    use iaam_core::instrument::CurrencyRoles;
    use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
    use rust_decimal::Decimal;
    use time::macros::date;

    use super::*;
    use crate::SqliteStore;
    use crate::reference::{AccountRecord, CustodyRecord, InstrumentRecord};

    /// Every table the journal can write to (spec §5, §6): sixteen names, in
    /// no particular order. A fault-injection test proves rollback by
    /// summing row counts across all of them for one event id — the same
    /// check the spec asks for ("no row of that event exists in any table")
    /// without hardcoding which table a given kind would have used.
    const JOURNAL_TABLES: &[&str] = &[
        "events",
        "event_legs",
        "event_trade",
        "event_cash_transfer",
        "event_unresolved_movement",
        "event_income",
        "event_fee",
        "event_tax",
        "event_opening_position",
        "event_valuation",
        "event_control_assertion",
        "event_coverage_gap",
        "event_corporate_action",
        "event_offer_exercise",
        "event_coverage_gap_rows",
        "event_coverage_gap_row_dimensions",
    ];

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(text: &str) -> Quantity {
        Quantity(Dec::new(Decimal::from_str_exact(text).unwrap()))
    }

    /// Total rows carrying `event` across all sixteen journal tables.
    fn total_rows_for_event(conn: &rusqlite::Connection, event: EventId) -> i64 {
        let id = event.inner().to_string();
        JOURNAL_TABLES
            .iter()
            .map(|table| {
                let column = if *table == "events" { "id" } else { "event" };
                let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
                conn.query_row(&sql, params![id], |row| row.get::<_, i64>(0))
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Reference rows a test event's account, instrument and custody place
    /// must exist against — invented from scratch, never derived from a real
    /// export (CLAUDE.md).
    struct Fixture {
        owner: OwnerId,
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
        source: SourceId,
    }

    impl Fixture {
        fn new(store: &SqliteStore) -> Self {
            let owner = OwnerId::new_random();
            let account = AccountId::new_random();
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
                instrument,
                custody,
                source,
            }
        }

        fn base_event(&self, sequence: u32, kind: EventKind, legs: Vec<Leg>) -> Event {
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

        fn trade(&self, sequence: u32) -> Event {
            self.base_event(
                sequence,
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument: self.instrument,
                    quantity: qty("10.500"),
                    gross: rub(-100_000),
                    fee: None,
                    basis_fee: None,
                    basis_fee_exact: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(self.account, rub(-100_000)),
                    Leg::security(self.account, self.custody, self.instrument, qty("10.500")),
                ],
            )
        }

        /// A no-leg event whose only external references — `instrument` and
        /// `custody` — live entirely in the detail row, not in any leg. Used
        /// to induce a fault-injection failure at the detail insert without
        /// the leg insert already having failed on the same reference.
        fn position_assertion(
            &self,
            sequence: u32,
            instrument: InstrumentId,
            custody: CustodyId,
        ) -> Event {
            self.base_event(
                sequence,
                EventKind::ControlAssertion {
                    period: AssertionPeriod::between(date!(2026 - 01 - 01), date!(2026 - 01 - 31))
                        .unwrap(),
                    claim: ControlClaim::PositionQuantity {
                        instrument,
                        custody,
                        quantity: qty("1"),
                        at: BalancePoint::Closing,
                    },
                },
                vec![],
            )
        }
    }

    fn open_store() -> SqliteStore {
        SqliteStore::open_in_memory().expect("in-memory store")
    }

    // --- Happy path -----------------------------------------------------

    #[test]
    fn insert_event_in_writes_a_trade_and_reads_it_back_unchanged() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let event = fixture.trade(1);

        let tx = store.connection_mut().transaction().expect("open tx");
        insert_event_in(&tx, &event).expect("insert succeeds");
        tx.commit().expect("commit");

        let reconstructed = hydrate_one(store.connection(), event.id)
            .expect("hydrate")
            .expect("event exists");
        assert_eq!(reconstructed, event);
    }

    // --- The postcondition itself: a saboteur that skips the detail row --

    /// Mirrors [`insert_event_in`] except that it never calls
    /// [`insert_detail`] — a test-only saboteur standing in for a mapper bug
    /// that forgot a child row. Exercises exactly the same postcondition
    /// [`insert_event_in`] runs, so this proves the read-back — not some
    /// separate check — is what refuses the write.
    fn insert_without_detail(store: &mut SqliteStore, event: &Event) -> Result<(), StoreError> {
        let tx = store.connection_mut().transaction()?;
        let (mut header, legs, _detail) = to_rows(event)?;
        header.recorded_at = crate::now();
        insert_header(&tx, &header)?;
        for leg in &legs {
            insert_leg(&tx, leg)?;
        }
        // Deliberately omitted: insert_detail(&tx, &detail).

        let outcome = match hydrate_one(&tx, event.id) {
            Ok(Some(reconstructed)) if &reconstructed == event => Ok(()),
            Ok(Some(_) | None) | Err(_) => Err(StoreError::IncompleteWrite {
                event: event.id.inner().to_string(),
            }),
        };
        if outcome.is_ok() {
            tx.commit()?;
        }
        // On the error path `tx` is dropped here without a commit, which
        // rolls back everything the saboteur wrote above.
        outcome
    }

    #[test]
    fn a_writer_that_forgets_the_detail_row_is_refused() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let event = fixture.trade(1);

        let outcome = insert_without_detail(&mut store, &event);

        assert!(
            matches!(outcome, Err(StoreError::IncompleteWrite { .. })),
            "expected IncompleteWrite, got {outcome:?}"
        );
        assert_eq!(
            total_rows_for_event(store.connection(), event.id),
            0,
            "the header and legs the saboteur did write must not survive"
        );
    }

    // --- Owner scoping (spec §4.2): legs only, per the write path's remit -

    #[test]
    fn a_leg_naming_an_account_of_another_owner_is_refused() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        // A second owner's account, registered and real — just not his.
        let foreign_owner = OwnerId::new_random();
        let foreign_account = AccountId::new_random();
        store
            .upsert_account(&AccountRecord {
                id: foreign_account,
                owner: foreign_owner,
                title: "Savings".to_owned(),
                institution: None,
            })
            .expect("foreign account created");

        let event = fixture.base_event(
            1,
            EventKind::CashIn { amount: rub(1_000) },
            vec![Leg::cash(foreign_account, rub(1_000))],
        );

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(
                outcome,
                Err(StoreError::NotFound {
                    what: "account",
                    ..
                })
            ),
            "expected NotFound, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    #[test]
    fn a_leg_naming_a_custody_place_of_another_owner_is_refused() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let foreign_owner = OwnerId::new_random();
        let foreign_custody = CustodyId::new_random();
        store
            .upsert_custody_place(&CustodyRecord {
                id: foreign_custody,
                owner: foreign_owner,
                title: "Shop One Custody".to_owned(),
                institution: None,
            })
            .expect("foreign custody place created");

        let event = fixture.base_event(
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: fixture.instrument,
                quantity: qty("1"),
                gross: rub(-1_000),
                fee: None,
                basis_fee: None,
                basis_fee_exact: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(fixture.account, rub(-1_000)),
                Leg::security(
                    fixture.account,
                    foreign_custody,
                    fixture.instrument,
                    qty("1"),
                ),
            ],
        );

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(
                outcome,
                Err(StoreError::NotFound {
                    what: "custody place",
                    ..
                })
            ),
            "expected NotFound, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    /// A place of custody in a per-unit amount's currency does not exist —
    /// this helper only ever needs rubles, so it is inlined rather than
    /// pulled from the other event modules' own test helpers.
    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    #[test]
    fn a_control_assertion_naming_a_custody_place_of_another_owner_is_refused() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let foreign_owner = OwnerId::new_random();
        let foreign_custody = CustodyId::new_random();
        store
            .upsert_custody_place(&CustodyRecord {
                id: foreign_custody,
                owner: foreign_owner,
                title: "Shop One Custody".to_owned(),
                institution: None,
            })
            .expect("foreign custody place created");

        // Legless: a `PositionQuantity` assertion carries no leg at all, so
        // only the detail row names the custody place.
        let event = fixture.position_assertion(1, fixture.instrument, foreign_custody);

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(
                outcome,
                Err(StoreError::NotFound {
                    what: "custody place",
                    ..
                })
            ),
            "expected NotFound, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    #[test]
    fn a_corporate_action_naming_a_custody_place_of_another_owner_is_refused() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let foreign_owner = OwnerId::new_random();
        let foreign_custody = CustodyId::new_random();
        store
            .upsert_custody_place(&CustodyRecord {
                id: foreign_custody,
                owner: foreign_owner,
                title: "Shop One Custody".to_owned(),
                institution: None,
            })
            .expect("foreign custody place created");

        // A partial redemption's only leg is the principal, which carries no
        // custody at all: the custody the fact names lives solely in the
        // corporate-action detail row.
        let compensation = rub(2_000_000);
        let event = fixture.base_event(
            1,
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument: fixture.instrument,
                    custody: foreign_custody,
                    quantity: qty("100"),
                    principal_returned_per_unit: per_unit("200.0000"),
                    compensation,
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation: BasisAllocation::default(),
                },
            },
            vec![Leg::principal(
                fixture.account,
                fixture.instrument,
                compensation,
            )],
        );

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(
                outcome,
                Err(StoreError::NotFound {
                    what: "custody place",
                    ..
                })
            ),
            "expected NotFound, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    #[test]
    fn a_settled_offer_exercise_naming_a_custody_place_of_another_owner_is_refused() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let foreign_owner = OwnerId::new_random();
        let foreign_custody = CustodyId::new_random();
        store
            .upsert_custody_place(&CustodyRecord {
                id: foreign_custody,
                owner: foreign_owner,
                title: "Shop One Custody".to_owned(),
                institution: None,
            })
            .expect("foreign custody place created");

        // The security leg names the fixture's own, owned custody place: the
        // stranger's place lives only in `OfferExerciseAction::Settled`
        // itself, which is exactly the field this test exists to cover.
        let gross = rub(6_000_000);
        let event = fixture.base_event(
            1,
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: fixture.instrument,
                    custody: foreign_custody,
                    quantity: qty("6"),
                    gross,
                    fee: None,
                    accrued_interest: None,
                },
            },
            vec![
                Leg::cash(fixture.account, gross),
                Leg::security(
                    fixture.account,
                    fixture.custody,
                    fixture.instrument,
                    qty("-6"),
                ),
            ],
        );

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(
                outcome,
                Err(StoreError::NotFound {
                    what: "custody place",
                    ..
                })
            ),
            "expected NotFound, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    // --- Fault injection (spec §5): a genuine SQL failure at each step ---

    #[test]
    fn a_foreign_key_failure_inserting_the_header_leaves_no_rows() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        // The event's own account is never registered; its leg's account is
        // fine, so the pre-check passes and the failure is the schema's own
        // `events` FK, at header-insert time.
        let unregistered_account = AccountId::new_random();
        let event = Event {
            account: unregistered_account,
            ..fixture.base_event(
                1,
                EventKind::CashIn { amount: rub(1_000) },
                vec![Leg::cash(fixture.account, rub(1_000))],
            )
        };

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(outcome, Err(StoreError::Sqlite(_))),
            "expected a foreign key failure, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    #[test]
    fn a_foreign_key_failure_inserting_a_leg_leaves_no_rows() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        // The security leg names an instrument nobody registered: legs are
        // inserted before the detail row, so this fails at the leg insert.
        let unregistered_instrument = InstrumentId::new_random();
        let event = fixture.base_event(
            1,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: unregistered_instrument,
                quantity: qty("1"),
                gross: rub(-1_000),
                fee: None,
                basis_fee: None,
                basis_fee_exact: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(fixture.account, rub(-1_000)),
                Leg::security(
                    fixture.account,
                    fixture.custody,
                    unregistered_instrument,
                    qty("1"),
                ),
            ],
        );

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(outcome, Err(StoreError::Sqlite(_))),
            "expected a foreign key failure, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    #[test]
    fn a_foreign_key_failure_inserting_the_detail_row_leaves_no_rows() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        // No legs at all for this kind, and the custody place is real and
        // owned — the ownership check passes — only the detail row's own
        // instrument reference is bad. Instruments carry no ownership check
        // (they are global reference data), so this reaches genuine SQL at
        // the detail insert specifically, unlike an unregistered custody
        // place, which the ownership check now refuses before any SQL runs.
        let unregistered_instrument = InstrumentId::new_random();
        let event = fixture.position_assertion(1, unregistered_instrument, fixture.custody);

        let tx = store.connection_mut().transaction().expect("open tx");
        let outcome = insert_event_in(&tx, &event);
        assert!(
            matches!(outcome, Err(StoreError::Sqlite(_))),
            "expected a foreign key failure, got {outcome:?}"
        );
        drop(tx);
        assert_eq!(total_rows_for_event(store.connection(), event.id), 0);
    }

    #[test]
    fn a_correction_naming_a_target_this_journal_does_not_hold_is_written() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        // `events.relation_target` carries no foreign key, and that is a
        // decision rather than an omission: the journal has a named state for
        // a correction whose target it does not hold. `HistoryAct::Arrived`
        // publishes it and `resolve_with_unheld_targets` reads it — «a fact
        // naming a target outside his journal is where his history begins».
        // A key here, deferred or not, would make that state unwritable by
        // any path, so the database would forbid a fact the domain has a word
        // for. This test is what keeps the key from being added back.
        let unheld_target = EventId::new_random();
        let event = Event {
            relation: Relation::Reversal {
                target: unheld_target,
            },
            ..fixture.base_event(
                1,
                EventKind::CashIn { amount: rub(1_000) },
                vec![Leg::cash(fixture.account, rub(1_000))],
            )
        };

        let tx = store.connection_mut().transaction().expect("open tx");
        insert_event_in(&tx, &event).expect("an unheld target is not a reason to refuse the fact");
        tx.commit().expect("commit");
        // Two rows: the header and the one cash leg a `CashIn` posts. The
        // helper counts rows across all sixteen tables, not events.
        assert_eq!(total_rows_for_event(store.connection(), event.id), 2);
    }

    // --- Fault injection: a panic mid-transaction ------------------------

    #[test]
    fn a_panic_between_header_and_legs_rolls_back_everything() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let event = fixture.trade(1);

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let tx = store.connection_mut().transaction().expect("open tx");
            let (mut header, _legs, _detail) = to_rows(&event).expect("to_rows");
            header.recorded_at = crate::now();
            insert_header(&tx, &header).expect("insert header");
            panic!("simulated failure between header and legs");
        }));
        assert!(result.is_err(), "the closure was expected to panic");

        assert_eq!(
            total_rows_for_event(store.connection(), event.id),
            0,
            "the transaction's Drop must have rolled the header insert back"
        );
    }

    #[test]
    fn a_panic_between_legs_and_detail_rolls_back_everything() {
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let event = fixture.trade(1);

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let tx = store.connection_mut().transaction().expect("open tx");
            let (mut header, legs, _detail) = to_rows(&event).expect("to_rows");
            header.recorded_at = crate::now();
            insert_header(&tx, &header).expect("insert header");
            for leg in &legs {
                insert_leg(&tx, leg).expect("insert leg");
            }
            panic!("simulated failure between legs and detail");
        }));
        assert!(result.is_err(), "the closure was expected to panic");

        assert_eq!(
            total_rows_for_event(store.connection(), event.id),
            0,
            "the transaction's Drop must have rolled the header and legs back"
        );
    }

    // --- Digest stability (spec §9) --------------------------------------

    #[test]
    fn a_round_tripped_event_encodes_to_identical_cbor() {
        // `10.500` is chosen because it is a case where a naive TEXT
        // round trip could plausibly drop or rewrite trailing zeros; if it
        // ever did, this is the test that would catch it before it silently
        // changed every snapshot's prefix digest (projection/state.rs).
        let mut store = open_store();
        let fixture = Fixture::new(&store);
        let event = fixture.trade(1);

        let tx = store.connection_mut().transaction().expect("open tx");
        insert_event_in(&tx, &event).expect("insert succeeds");
        let reconstructed = hydrate_one(&tx, event.id)
            .expect("hydrate")
            .expect("event exists");
        tx.commit().expect("commit");

        let mut original_cbor = Vec::new();
        ciborium::into_writer(&event, &mut original_cbor).expect("encode original");
        let mut reconstructed_cbor = Vec::new();
        ciborium::into_writer(&reconstructed, &mut reconstructed_cbor)
            .expect("encode reconstructed");

        assert_eq!(
            original_cbor, reconstructed_cbor,
            "canonical CBOR must be byte-identical, or every snapshot's prefix digest \
             (projection/state.rs) silently changes"
        );
    }
}
