//! Contours (§4.10).
//!
//! A broker treats a transfer from a deposit as a contribution because its
//! contour is only its own account. The owner sees the whole picture, so the
//! owner draws the boundary.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::Event;
use crate::event::kind::FlowEndpoints;
use crate::ids::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContourId(pub Uuid);

impl ContourId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Version of the contour definition.
///
/// Return calculations reference the version: without it, changing contour
/// membership retroactively and silently changes historical figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContourVersion(pub u32);

/// Contour membership at a specific version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContourDefinition {
    id: ContourId,
    version: ContourVersion,
    accounts: BTreeSet<AccountId>,
}

impl ContourDefinition {
    /// The body lives in `from_parts`: `cargo-mutants` silently skips any
    /// function named `new`, so building membership inside `new` would remain
    /// outside the mutation guard (§15.7).
    #[must_use]
    pub fn new(
        id: ContourId,
        version: ContourVersion,
        accounts: impl IntoIterator<Item = AccountId>,
    ) -> Self {
        Self::from_parts(id, version, accounts)
    }

    fn from_parts(
        id: ContourId,
        version: ContourVersion,
        accounts: impl IntoIterator<Item = AccountId>,
    ) -> Self {
        Self {
            id,
            version,
            accounts: accounts.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> ContourId {
        self.id
    }

    #[must_use]
    pub const fn version(&self) -> ContourVersion {
        self.version
    }

    #[must_use]
    pub fn contains(&self, account: AccountId) -> bool {
        self.accounts.contains(&account)
    }
}

/// An event's relation to the contour boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowClass {
    /// Money entered the contour from outside. It enters XIRR with a plus sign.
    ExternalIn {
        contour: ContourId,
        version: ContourVersion,
    },
    /// Money left the contour. It enters XIRR with a minus sign.
    ExternalOut {
        contour: ContourId,
        version: ContourVersion,
    },
    /// Inside the contour: changes allocation, not return.
    Internal,
    /// The event does not concern this contour.
    Irrelevant,
}

/// Classify an event relative to the contour.
///
/// This is a key point in the system: confusion here makes services report
/// transfers into one's own accounts as earnings. Classification therefore
/// uses the **pair** of memberships, so both accounts must be stored in the event.
#[must_use]
pub fn classify(def: &ContourDefinition, event: &Event) -> FlowClass {
    let inbound = FlowClass::ExternalIn {
        contour: def.id(),
        version: def.version(),
    };
    let outbound = FlowClass::ExternalOut {
        contour: def.id(),
        version: def.version(),
    };

    match event.kind.flow_endpoints() {
        FlowEndpoints::InboundFromOutside => {
            if def.contains(event.account) {
                inbound
            } else {
                FlowClass::Irrelevant
            }
        }
        FlowEndpoints::OutboundToOutside => {
            if def.contains(event.account) {
                outbound
            } else {
                FlowClass::Irrelevant
            }
        }
        FlowEndpoints::BetweenAccounts { from, to } => {
            match (def.contains(from), def.contains(to)) {
                (true, true) => FlowClass::Internal,
                (false, true) => inbound,
                (true, false) => outbound,
                (false, false) => FlowClass::Irrelevant,
            }
        }
        FlowEndpoints::WithinAccount => {
            if def.contains(event.account) {
                FlowClass::Internal
            } else {
                FlowClass::Irrelevant
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::event::kind::{EventKind, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::test_support::sample_event;
    use crate::ids::{AccountId, CustodyId, InstrumentId, TransferId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;

    // Amounts are recorded in minor units as one number: grouping such as
    // `100_000_00` does not compile (clippy::inconsistent_digit_grouping is
    // part of `all`, and `all = deny`).
    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    /// Transfer between two accounts.
    ///
    /// The legs are rewritten in full, not only `kind`: a transfer requires
    /// **two opposing cash legs on the declared accounts** (`validate_structure`,
    /// task 10), while `sample_event` provides one inbound leg. An event that
    /// fails structural validation cannot support classification claims.
    fn transfer(from: AccountId, to: AccountId) -> Event {
        let amount = rub(10_000_000);
        let mut event = sample_event(0);
        event.account = from;
        event.kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount,
        };
        event.legs = vec![Leg::cash(from, rub(-10_000_000)), Leg::cash(to, amount)];
        event
    }

    /// Money entering an account from outside.
    fn cash_in(account: AccountId) -> Event {
        let amount = rub(1_000_000);
        let mut event = sample_event(0);
        event.account = account;
        event.kind = EventKind::CashIn { amount };
        event.legs = vec![Leg::cash(account, amount)];
        event
    }

    /// Money leaving an account.
    fn cash_out(account: AccountId) -> Event {
        let amount = rub(-1_000_000);
        let mut event = sample_event(0);
        event.account = account;
        event.kind = EventKind::CashOut { amount };
        event.legs = vec![Leg::cash(account, amount)];
        event
    }

    /// Security purchase: movement within one account.
    fn purchase(account: AccountId) -> Event {
        let gross = rub(5_000_000);
        let instrument = InstrumentId::new_random();
        // The quantity is positive and matches the leg: structural validation
        // checks the leg against the event (§4.3), and zero would mean a trade
        // that did not happen.
        let quantity = Quantity(Dec::new(rust_decimal::Decimal::from(100)));
        let mut event = sample_event(0);
        event.account = account;
        event.kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument,
            quantity,
            gross,
            fee: None,
            accrued_interest: None,
            basis_fee: None,
            basis_fee_exact: None,
        };
        event.legs = vec![
            Leg::cash(account, rub(-5_000_000)),
            Leg::security(account, CustodyId::new_random(), instrument, quantity),
        ];
        event
    }

    fn contour(accounts: Vec<AccountId>) -> ContourDefinition {
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), accounts)
    }

    #[test]
    fn every_event_used_as_evidence_is_structurally_valid() {
        // Divergence from the plan. Plan tests built a transfer by replacing
        // only `kind` on `sample_event`, leaving its single inbound leg:
        // that event is rejected by `validate_structure`. Classifying an event
        // the journal would reject proves nothing.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        for event in [
            cash_in(broker),
            cash_out(broker),
            transfer(deposit, broker),
            purchase(broker),
        ] {
            let verdict = event.validate_structure();
            assert!(
                verdict.is_ok(),
                "{} fails structural validation: {verdict:?}",
                event.kind.discriminant()
            );
        }
    }

    // --- Acceptance criteria ---

    #[test]
    fn transfer_between_two_inside_accounts_is_internal() {
        // Deposit → brokerage account, both inside the “whole capital” contour.
        // This is not a contribution: return is unchanged; allocation changes.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        let def = contour(vec![deposit, broker]);
        assert_eq!(
            classify(&def, &transfer(deposit, broker)),
            FlowClass::Internal
        );
    }

    #[test]
    fn the_same_event_is_external_for_a_narrower_contour() {
        // The exact same event; only the contour definition changes.
        // The previous plan replaced CashTransfer with CashIn here and therefore
        // did not test a transfer at all.
        let deposit = AccountId::new_random();
        let broker = AccountId::new_random();
        let event = transfer(deposit, broker);

        let wide = contour(vec![deposit, broker]);
        let narrow = contour(vec![broker]);

        assert_eq!(classify(&wide, &event), FlowClass::Internal);
        assert!(
            matches!(classify(&narrow, &event), FlowClass::ExternalIn { .. }),
            "the same transfer is an external inflow for the narrow contour"
        );
    }

    #[test]
    fn transfer_out_of_the_contour_is_external_out() {
        let broker = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![broker]);
        assert!(matches!(
            classify(&def, &transfer(broker, outside)),
            FlowClass::ExternalOut { .. }
        ));
    }

    #[test]
    fn transfer_between_two_outside_accounts_is_irrelevant() {
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        let def = contour(vec![AccountId::new_random()]);
        assert_eq!(classify(&def, &transfer(a, b)), FlowClass::Irrelevant);
    }

    #[test]
    fn the_direction_of_a_transfer_decides_the_sign_of_the_external_flow() {
        // Same contour and same pair of accounts; only direction changes.
        // Reversing the sides would produce an XIRR inflow instead of an outflow.
        let broker = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![broker]);
        assert!(matches!(
            classify(&def, &transfer(broker, outside)),
            FlowClass::ExternalOut { .. }
        ));
        assert!(matches!(
            classify(&def, &transfer(outside, broker)),
            FlowClass::ExternalIn { .. }
        ));
    }

    #[test]
    fn buying_a_security_is_internal_not_a_contribution() {
        let broker = AccountId::new_random();
        let def = contour(vec![broker]);
        assert_eq!(classify(&def, &purchase(broker)), FlowClass::Internal);
    }

    #[test]
    fn cash_in_on_an_outside_account_is_irrelevant() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![inside]);
        assert_eq!(classify(&def, &cash_in(outside)), FlowClass::Irrelevant);
    }

    // --- Complete decision table ---

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Expected {
        In,
        Out,
        Internal,
        Irrelevant,
    }

    fn observed(class: FlowClass) -> Expected {
        match class {
            FlowClass::ExternalIn { .. } => Expected::In,
            FlowClass::ExternalOut { .. } => Expected::Out,
            FlowClass::Internal => Expected::Internal,
            FlowClass::Irrelevant => Expected::Irrelevant,
        }
    }

    #[test]
    fn every_combination_of_movement_and_membership_is_classified() {
        // Four movement forms across four contour memberships. Membership of
        // the whole pair matters for transfers; for the other forms the second
        // account must have no effect—these are the “second account only” and
        // “both accounts” columns.
        use Expected::{In, Internal, Irrelevant, Out};

        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let unrelated = AccountId::new_random();

        let contours = [
            ("neither event account", contour(vec![unrelated])),
            ("first account only", contour(vec![first])),
            ("second account only", contour(vec![second])),
            ("both accounts", contour(vec![first, second])),
        ];

        let rows: [(&str, Event, [Expected; 4]); 4] = [
            (
                "inflow from outside to first account",
                cash_in(first),
                [Irrelevant, In, Irrelevant, In],
            ),
            (
                "outflow from first account",
                cash_out(first),
                [Irrelevant, Out, Irrelevant, Out],
            ),
            (
                "transfer from first account to second",
                transfer(first, second),
                [Irrelevant, Out, In, Internal],
            ),
            (
                "purchase on first account",
                purchase(first),
                [Irrelevant, Internal, Irrelevant, Internal],
            ),
        ];

        for (movement, event, expectations) in &rows {
            for ((shape, def), expected) in contours.iter().zip(expectations) {
                assert_eq!(
                    observed(classify(def, event)),
                    *expected,
                    "{movement} with contour “{shape}”"
                );
            }
        }
    }

    // --- Contour definition ---

    #[test]
    fn contour_version_is_carried_into_the_classification() {
        let broker = AccountId::new_random();
        let id = ContourId::new_random();
        let def = ContourDefinition::new(id, ContourVersion(7), vec![broker]);
        assert_eq!(def.id(), id);
        assert_eq!(def.version(), ContourVersion(7));
        match classify(&def, &cash_in(broker)) {
            FlowClass::ExternalIn { contour, version } => {
                assert_eq!(contour, id);
                assert_eq!(version, ContourVersion(7));
            }
            other => panic!("expected ExternalIn, got {other:?}"),
        }
    }

    #[test]
    fn an_outbound_flow_carries_the_same_definition() {
        // Without a version on an outbound flow, a retroactive recalculation
        // would silently change historical figures on only one side.
        let broker = AccountId::new_random();
        let id = ContourId::new_random();
        let def = ContourDefinition::new(id, ContourVersion(3), vec![broker]);
        match classify(&def, &cash_out(broker)) {
            FlowClass::ExternalOut { contour, version } => {
                assert_eq!(contour, id);
                assert_eq!(version, ContourVersion(3));
            }
            other => panic!("expected ExternalOut, got {other:?}"),
        }
    }

    #[test]
    fn contains_answers_only_for_the_accounts_of_the_definition() {
        let inside = AccountId::new_random();
        let outside = AccountId::new_random();
        let def = contour(vec![inside]);
        assert!(def.contains(inside));
        assert!(!def.contains(outside));
    }

    #[test]
    fn a_repeated_account_does_not_make_a_different_definition() {
        // Membership is a set: naming an account twice does not create a second
        // membership, or definition comparison would depend on input order and
        // duplicates.
        let id = ContourId::new_random();
        let account = AccountId::new_random();
        let twice = ContourDefinition::new(id, ContourVersion(1), vec![account, account]);
        let once = ContourDefinition::new(id, ContourVersion(1), vec![account]);
        assert_eq!(twice, once);
    }

    #[test]
    fn the_order_of_accounts_does_not_change_the_definition() {
        let id = ContourId::new_random();
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let forward = ContourDefinition::new(id, ContourVersion(1), vec![first, second]);
        let backward = ContourDefinition::new(id, ContourVersion(1), vec![second, first]);
        assert_eq!(forward, backward);
    }

    #[test]
    fn a_definition_keeps_every_account_it_was_given() {
        // Dropping membership would make all capital external.
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let def = contour(vec![first, second]);
        assert!(def.contains(first));
        assert!(def.contains(second));
    }

    #[test]
    fn two_random_contour_ids_are_distinct() {
        assert_ne!(ContourId::new_random(), ContourId::new_random());
    }
}
