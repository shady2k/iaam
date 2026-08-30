//! Projection state and its fingerprint.
//!
//! The fingerprint is needed not for storage integrity, but so that
//! `advance` can refuse to advance a snapshot that someone assembled
//! or modified outside the core (§3.1). It is computed over ordered structures:
//! `BTreeMap` traversal order is deterministic, so the same
//! journal always produces the same fingerprint.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Date;

use super::balances::Balances;
use super::flows::FlowLog;
use super::income::IncomeLedger;
use super::lots::LotBook;
use crate::event::{Confidence, Event};
use crate::ids::AccountId;
use crate::valuation::PriceBoard;

/// State fingerprint: SHA-256 over an ordered traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateHash([u8; 32]);

impl StateHash {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// What the journal has seen: history boundaries and the unverified share (§10.7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    events_applied: u64,
    /// Coverage start for each account.
    ///
    /// The global boundary answers the report question, «from what date
    /// is any data available at all» (§10.7), but it is misleading for payout reconciliation:
    /// an account created later with a restored balance would inherit
    /// another account's coverage and be reported as a mismatch rather than unprovable.
    first_event_by_account: BTreeMap<AccountId, Date>,
    first_event: Option<Date>,
    last_event: Option<Date>,
    /// Accounts whose history starts with a restored balance,
    /// rather than an observed transaction.
    restored_accounts: BTreeSet<AccountId>,
    /// Events whose value is recorded as an estimate rather than a known fact
    /// (§4.9). This is **not** a reconciliation level: reconciliation appears in E2 and exists
    /// as a separate assertion about an account and interval, not as an event field.
    estimated_events: u64,
}

impl Coverage {
    #[must_use]
    pub const fn events_applied(&self) -> u64 {
        self.events_applied
    }

    /// Date of the first recorded event. The report must show it:
    /// «XIRR calculated from 01.03.2024; no earlier data is available» (§10.7).
    #[must_use]
    pub const fn first_event(&self) -> Option<Date> {
        self.first_event
    }

    /// Coverage start for a specific account.
    #[must_use]
    pub fn first_event_for(&self, account: AccountId) -> Option<Date> {
        self.first_event_by_account.get(&account).copied()
    }

    #[must_use]
    pub const fn last_event(&self) -> Option<Date> {
        self.last_event
    }

    #[must_use]
    pub fn restored_accounts(&self) -> &BTreeSet<AccountId> {
        &self.restored_accounts
    }

    #[must_use]
    pub const fn estimated_events(&self) -> u64 {
        self.estimated_events
    }

    fn observe(&mut self, event: &Event) {
        self.events_applied += 1;
        if let Some(date) = event.dates.effective_date() {
            self.first_event = Some(match self.first_event {
                Some(existing) => existing.min(date),
                None => date,
            });
            self.first_event_by_account
                .entry(event.account)
                .and_modify(|existing| *existing = (*existing).min(date))
                .or_insert(date);
            self.last_event = Some(match self.last_event {
                Some(existing) => existing.max(date),
                None => date,
            });
        }
        match event.confidence {
            Confidence::Known => {}
            Confidence::Estimated | Confidence::Unknown => self.estimated_events += 1,
        }
        if matches!(
            event.kind,
            crate::event::kind::EventKind::OpeningCash { .. }
                | crate::event::kind::EventKind::OpeningPosition { .. }
        ) {
            self.restored_accounts.insert(event.account);
        }
    }
}

/// Complete projection state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerState {
    balances: Balances,
    book: LotBook,
    flows: FlowLog,
    income: IncomeLedger,
    prices: PriceBoard,
    coverage: Coverage,
}

impl LedgerState {
    #[must_use]
    pub fn new(book: LotBook) -> Self {
        Self {
            balances: Balances::new(),
            book,
            flows: FlowLog::new(),
            income: IncomeLedger::default(),
            prices: PriceBoard::new(),
            coverage: Coverage::default(),
        }
    }

    #[must_use]
    pub const fn balances(&self) -> &Balances {
        &self.balances
    }

    #[must_use]
    pub const fn book(&self) -> &LotBook {
        &self.book
    }

    #[must_use]
    pub const fn flows(&self) -> &FlowLog {
        &self.flows
    }

    /// Dated income facts used to reconcile the payout schedule.
    #[must_use]
    pub const fn income(&self) -> &IncomeLedger {
        &self.income
    }

    #[must_use]
    pub const fn prices(&self) -> &PriceBoard {
        &self.prices
    }

    #[must_use]
    pub const fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    pub(super) const fn parts_mut(&mut self) -> (&mut Balances, &mut LotBook, &mut FlowLog) {
        (&mut self.balances, &mut self.book, &mut self.flows)
    }

    pub(super) const fn income_mut(&mut self) -> &mut IncomeLedger {
        &mut self.income
    }

    pub(super) const fn prices_mut(&mut self) -> &mut PriceBoard {
        &mut self.prices
    }

    pub(super) fn observe(&mut self, event: &Event) {
        self.coverage.observe(event);
    }

    /// State fingerprint.
    ///
    /// Computed from the **canonical serialization of the entire state**, not by
    /// enumerating fields manually. The manual enumeration was reviewed
    /// and found to be incomplete: it omitted realised results,
    /// acquisition and disposal costs, the disposal rule version,
    /// and history boundaries. A fingerprint covering only part of the state promises
    /// more than it provides: a snapshot with a modified uncovered field would pass
    /// validation. Serialization covers everything contained in the state
    /// by construction.
    ///
    /// CBOR rather than JSON, for the same reason as in storage: state maps
    /// have compound keys, which JSON cannot represent.
    /// `BTreeMap` traversal is deterministic, `Decimal` is serialized exactly,
    /// and the state contains no binary floating-point values, so the same
    /// journal always produces the same fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> StateHash {
        let mut body = Vec::new();
        // Serialization cannot fail here: we write to an in-memory vector,
        // and the state consists of types with derived `Serialize`.
        // Nevertheless, the fingerprint is not replaced with a placeholder: the same
        // fingerprint for different states is worse than a panic.
        ciborium::into_writer(self, &mut body)
            .unwrap_or_else(|error| panic!("state cannot be serialized: {error}"));
        let mut hasher = Sha256::new();
        hasher.update(b"iaam/ledger-state/v2");
        hasher.update(body);
        StateHash(hasher.finalize().into())
    }
}

/// Fingerprint of the journal prefix folded into the snapshot.
///
/// Answers the question that the state fingerprint does not:
/// «are these the same events». An event backdated **before** the snapshot
/// boundary changes neither the boundary nor the snapshot state, and without this
/// check would simply disappear from the calculation.
///
/// The fingerprint includes the canonical CBOR body of each event, not just
/// its identity. `provenance.raw_hash()` is not suitable for this purpose:
/// it is the fingerprint of the raw submitted fact, which does not change when
/// the application derives a field. At the same time, `raw_hash` must
/// remain unchanged for deduplication: resubmitting the same brokerage fact
/// must remain a duplicate.
///
/// The fingerprint is sensitive to any future [`Event`] field. Adding
/// a field will invalidate all snapshots and trigger a full recalculation. This is a deliberate
/// tradeoff: silently calculating from a stale snapshot is worse than recalculating.
#[must_use]
pub fn prefix_digest(events: &[&Event]) -> StateHash {
    let mut hasher = Sha256::new();
    hasher.update(b"iaam/journal-prefix/v2");
    hasher.update(
        u64::try_from(events.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for event in events {
        // The identity is fed separately even though the body contains it:
        // this ensures that coverage of key fields does not depend on what may eventually
        // be done to their serialization.
        hasher.update(event.id.inner().as_bytes());
        feed_date(&mut hasher, event.order.date());
        hasher.update(event.order.sequence().to_be_bytes());
        let mut body = Vec::new();
        ciborium::into_writer(event, &mut body).expect(
            "event must be serializable: otherwise this is a type defect, not a data error",
        );
        hasher.update(&body);
    }
    StateHash(hasher.finalize().into())
}

fn feed_date(hasher: &mut Sha256, date: Date) {
    hasher.update(date.year().to_be_bytes());
    hasher.update(date.ordinal().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::allocation::{
        AllocationAlgorithmVersion, AllocationEvidence, AllocationInputsHash, BasisAllocation,
    };
    use crate::event::corporate_action::CorporateAction;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::provenance::{ParserVersion, Provenance, RawHash};
    use crate::event::test_support::event_with;
    use crate::ids::{CustodyId, EventId, InstrumentId, OwnerId, SourceId};
    use crate::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::rules::LotRuleVersion;
    use crate::rules::ReturnedShare;
    use rust_decimal::Decimal;
    use time::OffsetDateTime;
    use time::macros::date;
    use uuid::Uuid;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn cash_in(account: AccountId, day: Date, sequence: u32) -> Event {
        event_with(
            account,
            day,
            sequence,
            EventKind::CashIn {
                amount: rub(10_000),
            },
            vec![Leg::cash(account, rub(10_000))],
        )
    }

    fn known_allocation() -> BasisAllocation {
        BasisAllocation::Known {
            share: ReturnedShare::new(Dec::new(Decimal::new(1, 1)))
                .expect("share is within invariant"),
            evidence: AllocationEvidence {
                inputs_hash: AllocationInputsHash::new("a".repeat(64)).expect("inputs hash"),
                knowledge_as_of: OffsetDateTime::UNIX_EPOCH,
                algorithm_version: AllocationAlgorithmVersion(1),
            },
        }
    }

    fn amortisation_event(basis_allocation: BasisAllocation) -> Event {
        let account = AccountId(Uuid::from_u128(1));
        let instrument = InstrumentId(Uuid::from_u128(2));
        let custody = CustodyId(Uuid::from_u128(3));
        let mut event = event_with(
            account,
            date!(2026 - 06 - 15),
            5,
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody,
                    quantity: Quantity(Dec::new(Decimal::from(1))),
                    principal_returned_per_unit: PerUnitAmount::new(
                        Dec::new(Decimal::from(100)),
                        CurrencyCode::Rub,
                    ),
                    compensation: rub(10),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation,
                },
            },
            vec![Leg::principal(account, instrument, rub(10))],
        );
        event.id = EventId(Uuid::from_u128(4));
        event.owner = OwnerId(Uuid::from_u128(5));
        event.provenance = Provenance::new(
            SourceId(Uuid::from_u128(6)),
            RawHash::parse(&"d".repeat(64)).expect("raw fact hash"),
            ParserVersion("test/1".into()),
        );
        event
    }

    #[test]
    fn two_events_differing_only_in_allocation_get_different_digests() {
        let unknown = amortisation_event(BasisAllocation::default());
        let known = amortisation_event(known_allocation());
        assert_ne!(
            prefix_digest(&[&unknown]),
            prefix_digest(&[&known]),
            "fingerprint must cover event contents"
        );
    }

    #[test]
    fn those_same_events_keep_one_raw_hash_so_deduplication_still_works() {
        let unknown = amortisation_event(BasisAllocation::default());
        let known = amortisation_event(known_allocation());
        assert_eq!(
            unknown.provenance.raw_hash(),
            known.provenance.raw_hash(),
            "resubmitting the same brokerage fact must remain a duplicate"
        );
    }

    #[test]
    fn a_state_hash_prints_as_lowercase_hex_of_every_byte() {
        // The fingerprint is printed in logs and API responses. An empty string
        // in its place is indistinguishable from «no fingerprint», and a truncated one is
        // indistinguishable from a match with another state.
        let mut bytes = [0_u8; 32];
        bytes[0] = 0x0a;
        bytes[31] = 0xff;
        let printed = StateHash(bytes).to_string();
        assert_eq!(printed.len(), 64);
        assert!(printed.starts_with("0a"), "{printed}");
        assert!(printed.ends_with("ff"), "{printed}");
    }

    #[test]
    fn coverage_counts_events_and_keeps_the_outer_bounds_of_history() {
        // History boundaries are min and max, not the first and last
        // events applied: events arrive in arbitrary order.
        let account = AccountId::new_random();
        let mut coverage = Coverage::default();
        assert_eq!(coverage.events_applied(), 0);
        assert_eq!(coverage.first_event(), None);
        assert_eq!(coverage.last_event(), None);

        coverage.observe(&cash_in(account, date!(2025 - 06 - 01), 1));
        coverage.observe(&cash_in(account, date!(2025 - 01 - 15), 2));
        coverage.observe(&cash_in(account, date!(2025 - 12 - 31), 3));

        assert_eq!(coverage.events_applied(), 3);
        assert_eq!(coverage.first_event(), Some(date!(2025 - 01 - 15)));
        assert_eq!(coverage.last_event(), Some(date!(2025 - 12 - 31)));
    }

    #[test]
    fn each_account_carries_its_own_history_horizon() {
        // A global horizon would declare account B's history covered
        // since 2020 solely because account A has existed since 2020.
        // Account B's payouts for 2021–2025 would be reported as mismatches instead of
        // honestly stating «the journal starts later than the schedule».
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        let mut coverage = Coverage::default();
        coverage.observe(&cash_in(a, date!(2020 - 01 - 15), 1));
        coverage.observe(&cash_in(b, date!(2026 - 01 - 01), 2));

        assert_eq!(coverage.first_event_for(a), Some(date!(2020 - 01 - 15)));
        assert_eq!(coverage.first_event_for(b), Some(date!(2026 - 01 - 01)));
        // The global boundary remains: it is shown by the coverage report (§10.7).
        assert_eq!(coverage.first_event(), Some(date!(2020 - 01 - 15)));
    }

    #[test]
    fn coverage_counts_estimated_values_but_not_known_ones() {
        // `Confidence` describes confidence in the value (§4.9). A known
        // value is not an estimate, and vice versa; otherwise the
        // unverified share in the report ceases to mean anything.
        let account = AccountId::new_random();
        let mut coverage = Coverage::default();
        coverage.observe(&cash_in(account, date!(2025 - 02 - 02), 1));
        assert_eq!(coverage.estimated_events(), 0);

        let mut estimated = cash_in(account, date!(2025 - 02 - 03), 2);
        estimated.confidence = Confidence::Estimated;
        coverage.observe(&estimated);
        assert_eq!(coverage.estimated_events(), 1);

        let mut unknown = cash_in(account, date!(2025 - 02 - 04), 3);
        unknown.confidence = Confidence::Unknown;
        coverage.observe(&unknown);
        assert_eq!(coverage.estimated_events(), 2);
        assert_eq!(coverage.events_applied(), 3);
    }

    #[test]
    fn only_a_restored_opening_marks_the_account_as_restored() {
        // An account whose history starts with a restored balance is correctly
        // marked in the quality section: there are no observed transactions before this date,
        // and returns for the earlier period cannot be calculated (§10.7).
        let observed = AccountId::new_random();
        let restored = AccountId::new_random();
        let mut coverage = Coverage::default();
        coverage.observe(&cash_in(observed, date!(2025 - 03 - 01), 1));
        assert!(coverage.restored_accounts().is_empty());

        coverage.observe(&event_with(
            restored,
            date!(2025 - 03 - 02),
            2,
            EventKind::OpeningCash {
                amount: rub(50_000),
            },
            vec![Leg::cash(restored, rub(50_000))],
        ));
        assert_eq!(coverage.restored_accounts().len(), 1);
        assert!(coverage.restored_accounts().contains(&restored));
        assert!(!coverage.restored_accounts().contains(&observed));
    }

    #[test]
    fn observing_through_the_state_reaches_its_coverage() {
        // State is the only external gateway to coverage: if
        // `observe` stops propagating the event to `Coverage`, the
        // data-completeness report will be empty while the calculation is not.
        let account = AccountId::new_random();
        let mut state = LedgerState::new(LotBook::new(LotRuleVersion(1)));
        assert_eq!(state.coverage().events_applied(), 0);
        state.observe(&cash_in(account, date!(2025 - 04 - 04), 1));
        assert_eq!(state.coverage().events_applied(), 1);
        assert_eq!(state.coverage().first_event(), Some(date!(2025 - 04 - 04)));
    }

    #[test]
    fn the_prefix_digest_notices_a_different_date_at_the_same_position() {
        // The date is included in the prefix fingerprint: an event moved to
        // another day within the folded period must change it,
        // otherwise `advance` will advance a stale state.
        let account = AccountId::new_random();
        let first = cash_in(account, date!(2025 - 05 - 05), 1);
        let mut moved = first.clone();
        moved.order = crate::dates::EffectiveOrder::new(date!(2025 - 05 - 06), 1);

        assert_ne!(prefix_digest(&[&first]), prefix_digest(&[&moved]));
        assert_eq!(prefix_digest(&[&first]), prefix_digest(&[&first]));
        assert_ne!(prefix_digest(&[&first]), prefix_digest(&[]));
    }
}
