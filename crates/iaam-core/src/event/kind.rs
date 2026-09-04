//! Family of event types (§4.6).
//!
//! Stage 1 implements the subset sufficient for manual input
//! and pre-tax XIRR calculation. Other variants are added in their
//! respective stages — adding a variant must break the build wherever
//! handling is incomplete.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::corporate_action::CorporateAction;
use super::offer::OfferExerciseAction;
use crate::ids::{AccountId, InstrumentId, TransferId};
use crate::money::{CalcMoney, CurrencyCode, Money, Quantity};
use crate::numeric::decimal::Dec;
use crate::reconciliation::Dimension;
use crate::reconciliation::claim::{AssertionPeriod, ControlClaim};
use crate::valuation::PriceQuality;

/// Confidence in the quantity (§10.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Certainty {
    Known,
    Estimated,
}

/// Confidence in the acquisition date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateCertainty {
    Known,
    Estimated,
    Unknown,
}

/// Confidence in the tax basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisCertainty {
    Documented,
    Estimated,
    Unknown,
}

/// A three-valued answer. `Unknown` is a full-fledged value, not “no” (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tristate {
    Yes,
    No,
    Unknown,
}

/// Is anything known at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Knowledge {
    Known,
    Unknown,
}

/// Reconstructed opening as a **set of assertions with confidence**
/// (§10.7), not a string with a price.
///
/// Each item defaults to “unknown”. This is not a placeholder: an event,
/// recorded before this field was introduced, truly asserted none of
/// the listed facts, and marking them `Known` would mean
/// retroactively declaring as documented something no one had seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningAssertions {
    pub quantity: Certainty,
    pub acquisition_date: Option<time::Date>,
    pub acquisition_date_certainty: DateCertainty,
    pub tax_basis: BasisCertainty,
    pub basis_currency: Option<CurrencyCode>,
    pub basis_rate: Option<Dec>,
    pub fees_included: Tristate,
    pub ldv_eligibility: Knowledge,
    pub prior_corporate_actions: Knowledge,
}

impl Default for OpeningAssertions {
    fn default() -> Self {
        Self {
            // The reconstructed position quantity is an estimate until the owner
            // says otherwise: defaulting to “known” would mean that
            // the system itself confirmed what it was told.
            quantity: Certainty::Estimated,
            acquisition_date: None,
            acquisition_date_certainty: DateCertainty::Unknown,
            tax_basis: BasisCertainty::Unknown,
            basis_currency: None,
            basis_rate: None,
            fees_included: Tristate::Unknown,
            ldv_eligibility: Knowledge::Unknown,
            prior_corporate_actions: Knowledge::Unknown,
        }
    }
}

impl OpeningAssertions {
    /// Is enough known to calculate the tax basis.
    ///
    /// The report uses this: if the basis is unknown, the tax report
    /// must return a range or `not_computable`, not an exact number
    /// (§10.7). The calculation itself will appear in E5.
    #[must_use]
    pub const fn basis_is_documented(&self) -> bool {
        matches!(self.tax_basis, BasisCertainty::Documented)
    }
}

/// Trade direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// Type of income paid.
///
/// There is intentionally no `Other` variant: a catch-all on which no
/// decision can be based is indistinguishable from not knowing, while §4.9 requires
/// that unknown state to be explicit — `None` expresses it in the field itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IncomeKind {
    /// Bond coupon.
    Coupon,
    /// Equity dividend.
    Dividend,
    /// Deposit interest paid (E3.5 will build on this).
    DepositInterest,
}

/// Storage discriminants used by SQL projections as well as the event API.
///
/// Keeping these values beside `EventKind::discriminant` prevents a projection
/// from silently drifting when a journal kind is renamed.
pub const CONTROL_ASSERTION_KIND: &str = "control_assertion";
pub const IMPORT_COVERAGE_GAP_KIND: &str = "import_coverage_gap";
/// The one kind whose fact names a second account.
///
/// Named here for the same reason as the two above: a projection that has to
/// find transfers writes this string, and a rename that did not break the
/// build would leave the projection quietly matching nothing.
pub const CASH_TRANSFER_KIND: &str = "cash_transfer";

/// The event type is exhaustive — `#[non_exhaustive]` is intentionally **not**
/// used: the core has no external consumers, and exhaustiveness enables
/// complete handling checks (§15.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    /// Purchase or sale.
    Trade {
        side: TradeSide,
        instrument: InstrumentId,
        quantity: Quantity,
        gross: Money,
        fee: Option<Money>,
        /// Posted basis-only fee; unlike `fee`, it is absent from the cash leg.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basis_fee: Option<Money>,
        /// Exact source commission retained for audit of `basis_fee` rounding.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basis_fee_exact: Option<CalcMoney>,
        /// Accrued coupon interest paid to the seller or received from the buyer (§7.2).
        accrued_interest: Option<Money>,
    },
    /// Money entered the contour from outside (§4.10).
    CashIn { amount: Money },
    /// Money left the contour.
    CashOut { amount: Money },
    /// A counterparty returned money, reversing an earlier outflow.
    ///
    /// Not `CashIn`. Money does cross the boundary inward, and
    /// [`Self::flow_endpoints`] says so, but a household report that counts a
    /// refund as income reports earnings nobody earned: a returned appliance
    /// would appear as tens of thousands arriving. The money-flow projection
    /// therefore reads the same event from the other angle and **subtracts it
    /// from what went out**, in the category it was spent in — exactly the
    /// distinction already drawn for [`Self::Income`], which is a flow within
    /// the contour for returns and earnings for the household.
    Refund { amount: Money },
    /// Money moved between accounts.
    ///
    /// **Both accounts are stored in the event itself.** Classification relative
    /// to the contour is impossible without the second account: a transfer from an external deposit to
    /// an internal brokerage account is an external flow, while one between two internal
    /// accounts is not. The event is immutable, so missing semantics
    /// here would require a journal migration later (§16.1).
    CashTransfer {
        transfer_id: TransferId,
        from: AccountId,
        to: AccountId,
        amount: Money,
    },
    /// Money moved between this account and another account of the owner's
    /// that the source asserted and did not name, and the source said which way
    /// it ran here.
    ///
    /// **Not [`Self::CashOut`] or [`Self::CashIn`].** Those say the money
    /// crossed the boundary of whatever contour holds this account, and the
    /// money-flow projection counts a `CashOut` as money spent. A source that
    /// says the far side is an account of the owner's has said the opposite —
    /// and has *not* said which of his accounts, so the movement cannot be a
    /// [`Self::CashTransfer`] either: that variant is a complete fact and holds
    /// both accounts because contour classification needs both.
    ///
    /// So this is the third thing, and the honest one: one leg is known, the
    /// other endpoint is known to be the owner's and is unidentified.
    /// [`Self::flow_endpoints`] answers `OwnAccountUnnamed`, and the contour
    /// classifier turns that into `Indeterminate` rather than `Internal` — see
    /// [`crate::contour::classify`] for why an account of the owner's is not
    /// the same claim as an account inside this contour.
    ///
    /// `amount` is **signed**, as [`Self::OpeningCash`]'s is: the sign is the
    /// direction on this account, negative for money that left. Two variants
    /// `…In`/`…Out` were considered and refused — the pair would double every
    /// arm below for a distinction the single cash leg already carries and
    /// `validate_structure` already checks against the leg.
    OwnAccountMovement { amount: Money },
    /// The same movement, with the direction the source never stated.
    ///
    /// **A separate variant rather than an `Option<Movement>` on the one
    /// above.** The difference between the two is not a flag: it is whether the
    /// fact has a leg. This one has none and can have none — the journal cannot
    /// debit or credit an account on a movement whose direction nobody stated,
    /// and a variant that carried a signed leg beside `direction: None` would
    /// let a caller construct exactly the fabrication this shape exists to
    /// refuse. Legs against no legs is the sharpest line in this journal:
    /// `validate_structure`, `Balances`, `perimeter::assess` and
    /// `projection::flows` all read it, and a single variant would put it
    /// inside a payload for each of them to rediscover.
    ///
    /// `amount` is the magnitude the source printed, and is positive. It is not
    /// a movement of money in this journal — nothing here is posted anywhere —
    /// it is the record that the source stated one and said too little for it
    /// to be posted. That is what makes it correctable: when a later pass
    /// supplies the far side, this event is **replaced** by a complete
    /// [`Self::CashTransfer`], exactly as `confirm_journal_pairing` replaces a
    /// leg it has paired. Without a fact here there would be nothing to
    /// replace, and the row would have to live outside the journal in a second
    /// notion of what is effective.
    UnresolvedOwnAccountMovement { amount: Money },
    /// Coupon, dividend, or interest actually paid.
    Income {
        instrument: Option<InstrumentId>,
        gross: Money,
        /// Type of income, if the source named it.
        ///
        /// `#[serde(default)]` is required: the journal is append-only, and already
        /// recorded payments do not contain this field. `None` means
        /// “was not asserted”, not “dividend”: retroactively supplying a type
        /// would declare known something no one stated (§4.9).
        #[serde(default)]
        kind: Option<IncomeKind>,
    },
    /// Fee not tied to a trade.
    Fee { amount: Money, origin: FeeOrigin },
    /// Tax, whether withheld at source or paid by the owner.
    ///
    /// Modelled on `Fee`: a cost borne by the contour rather than money
    /// crossing its boundary. Without a fact of its own, a self-paid tax is
    /// indistinguishable from ordinary spending, and discretionary spending is
    /// overstated by exactly the tax bill (spec §2).
    Tax { amount: Money, origin: TaxOrigin },
    /// Reconstructed position for an account with no history (§10.7).
    OpeningPosition {
        instrument: InstrumentId,
        quantity: Quantity,
        cost_basis: Option<Money>,
        /// Set of assertions about the reconstructed opening (§10.7).
        ///
        /// `#[serde(default)]` is required: the journal is append-only, and already
        /// recorded events do not contain this field. An absent field
        /// means “none of this was asserted”, not invented
        /// values.
        #[serde(default)]
        assertions: OpeningAssertions,
    },
    /// Reconstructed cash balance.
    OpeningCash { amount: Money },
    /// Valuation of an instrument at a per-unit price (§5.4).
    ///
    /// A fact with provenance, not a calculation: someone published or supplied the price,
    /// and without it the position's value is unknown. At stage 1, the source is
    /// the owner or an external agent; in E3 the same variant is populated by
    /// `iaam-market`, and the schema remains unchanged.
    ///
    /// Moves no money: the event has no legs.
    Valuation {
        instrument: InstrumentId,
        price: Dec,
        currency: CurrencyCode,
        quality: PriceQuality,
    },
    /// The source's control assertion about interval completeness (§10.3).
    ///
    /// A fact with provenance, not a calculation: the report's control section is
    /// what the source said about itself. Reconciliation compares it with what
    /// the projection computed, and a match provides grounds for raising the
    /// status. Moves no money: the event has no legs.
    ControlAssertion {
        period: AssertionPeriod,
        claim: ControlClaim,
    },
    /// An import attempt refused rows, so it cannot confirm on its own the
    /// dimensions those rows would have moved.
    ///
    /// It is not a statement about the interval: the same operations may already
    /// be in the journal from another channel, and a later attempt that refuses
    /// nothing carries no gap. It is a statement about this attempt.
    ImportCoverageGap {
        period: AssertionPeriod,
        /// What this attempt cannot confirm. Never empty—a gap that taints
        /// nothing is not a fact.
        dimensions: BTreeSet<Dimension>,
        /// How many rows were refused. Carried for the owner, not for the rule.
        refused: u32,
        /// The rows themselves. Empty only in records written before schema 8:
        /// such a gap taints its whole `dimensions` and is never lifted
        /// automatically, because it cannot say what is missing.
        #[serde(default)]
        rows: Vec<crate::event::source_row::RefusedRow>,
    },
    /// Corporate action on a security: amortization, redemption,
    /// replacement (§4.7).
    ///
    /// A single variant with a typed family inside, rather than three
    /// `EventKind` variants: family members share an identity
    /// (“what the issuer decided for this security”) and are handled together
    /// wherever that is what matters.
    CorporateAction { action: CorporateAction },
    /// Exercising an offer is the holder's right, not an issuer decision,
    /// so it is a separate variant, not a corporate-action member.
    OfferExercise { action: OfferExerciseAction },
}

/// Fee origin. Required as early as stage 1 because margin interest
/// is imported as a tagged fee (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeeOrigin {
    Brokerage,
    Depositary,
    AccountMaintenance,
    /// Margin interest. The position is outside the perimeter, but its cash effect is retained.
    MarginInterest,
    Other,
}

/// Tax payment origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaxOrigin {
    /// Withheld by the payer before the money arrived.
    WithheldAtSource,
    /// Paid by the owner: property, transport, a filed return.
    SelfPaid,
}

impl EventKind {
    /// Short machine-readable name. Used in the API and storage.
    ///
    /// Implemented with an exhaustive `match` and no `_` arm: adding
    /// a variant must break the build here.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::Trade { .. } => "trade",
            Self::CashIn { .. } => "cash_in",
            Self::CashOut { .. } => "cash_out",
            Self::Refund { .. } => "refund",
            Self::CashTransfer { .. } => CASH_TRANSFER_KIND,
            Self::OwnAccountMovement { .. } => "own_account_movement",
            Self::UnresolvedOwnAccountMovement { .. } => "unresolved_own_account_movement",
            Self::Income { .. } => "income",
            Self::Fee { .. } => "fee",
            Self::Tax { .. } => "tax",
            Self::OpeningPosition { .. } => "opening_position",
            Self::OpeningCash { .. } => "opening_cash",
            Self::Valuation { .. } => "valuation",
            Self::ControlAssertion { .. } => CONTROL_ASSERTION_KIND,
            Self::ImportCoverageGap { .. } => IMPORT_COVERAGE_GAP_KIND,
            Self::CorporateAction { .. } => "corporate_action",
            Self::OfferExercise { .. } => "offer_exercise",
        }
    }

    /// Where the money comes from and goes.
    ///
    /// By itself, the event **does not know** whether it crosses the contour boundary:
    /// that is a property of the “event + contour definition” pair. Classification
    /// is performed by the contour classifier (module `contour`, the next task),
    /// while only the movement endpoints are described here.
    #[must_use]
    pub const fn flow_endpoints(&self) -> FlowEndpoints {
        match self {
            Self::CashIn { .. } => FlowEndpoints::InboundFromOutside,
            Self::CashOut { .. } => FlowEndpoints::OutboundToOutside,
            // Money genuinely entered from outside, and a returns calculation
            // must see the flow. What it is *called* differs by report: the
            // household one subtracts it from spending instead of adding it to
            // income.
            Self::Refund { .. } => FlowEndpoints::InboundFromOutside,
            Self::CashTransfer { from, to, .. } => FlowEndpoints::BetweenAccounts {
                from: *from,
                to: *to,
            },
            // Both, and the unresolved one deliberately as well. It posts no
            // leg, so nothing of it is summed anywhere; what the answer decides
            // is whether a report may call the amount internal, and the answer
            // is that it may not — for the same reason in both cases, that the
            // far side is an account of the owner's and no contour can prove it
            // holds every account he has.
            Self::OwnAccountMovement { .. } | Self::UnresolvedOwnAccountMovement { .. } => {
                FlowEndpoints::OwnAccountUnnamed
            }
            Self::Trade { .. }
            | Self::Income { .. }
            | Self::Fee { .. }
            | Self::Tax { .. }
            | Self::OpeningPosition { .. }
            | Self::OpeningCash { .. }
            | Self::Valuation { .. }
            | Self::ImportCoverageGap { .. }
            | Self::ControlAssertion { .. }
            // Money does not enter the contour from outside: the security is already inside, and
            // amortization returns invested capital rather than bringing in new money.
            // `InboundFromOutside` would overstate contributions to the contour and
            // corrupt XIRR — just as a coupon would,
            // which is classified the same way here.
            | Self::CorporateAction { .. }
            | Self::OfferExercise { .. } => FlowEndpoints::WithinAccount,
        }
    }
}

/// Endpoints of an event's cash movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowEndpoints {
    /// Money came from a counterparty the system does not observe.
    InboundFromOutside,
    /// Money went to a counterparty the system does not observe.
    OutboundToOutside,
    /// Movement between two known accounts.
    BetweenAccounts { from: AccountId, to: AccountId },
    /// Movement between the event's own account and an account the source
    /// asserted is the owner's and did not name.
    ///
    /// No account is carried, for [`Self::InboundFromOutside`]'s reason: the
    /// near side is `Event::account`, which the classifier already has, and
    /// there is no far side to carry — that is the whole content of this
    /// answer.
    OwnAccountUnnamed,
    /// Movement within one account: purchase, coupon, fee.
    WithinAccount,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::offer::OfferSubmissionId;
    use crate::money::{CurrencyCode, PostedMinor};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn per_unit(text: &str) -> crate::money::PerUnitAmount {
        crate::money::PerUnitAmount::new(
            Dec::new(rust_decimal::Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    fn amortisation() -> EventKind {
        EventKind::CorporateAction {
            action: CorporateAction::PartialRedemption {
                instrument: InstrumentId::new_random(),
                custody: crate::ids::CustodyId::new_random(),
                quantity: Quantity::zero(),
                principal_returned_per_unit: per_unit("200"),
                compensation: rub(2_000_000),
                effective_date: time::macros::date!(2026 - 06 - 15),
                record_date: None,
                grounds: None,
                basis_allocation: crate::event::allocation::BasisAllocation::default(),
            },
        }
    }

    fn offer_settled() -> EventKind {
        EventKind::OfferExercise {
            action: OfferExerciseAction::Settled {
                submission: OfferSubmissionId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: crate::ids::CustodyId::new_random(),
                quantity: Quantity::zero(),
                gross: rub(1_000_000),
                fee: None,
                accrued_interest: None,
            },
        }
    }

    #[test]
    fn the_new_variants_name_themselves() {
        assert_eq!(amortisation().discriminant(), "corporate_action");
        assert_eq!(offer_settled().discriminant(), "offer_exercise");
    }

    #[test]
    fn the_new_variants_move_money_within_the_account() {
        // Money does not enter the contour from outside: the security is already inside.
        // `InboundFromOutside` would overstate contributions to the contour and corrupt
        // XIRR — just as a coupon would.
        assert_eq!(
            amortisation().flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
        assert_eq!(
            offer_settled().flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
    }

    #[test]
    fn an_income_written_before_the_kind_existed_reads_as_not_asserted() {
        // The value is taken from today's Income and stripped of the kind field —
        // exactly what is in the already-recorded journal. We do not invent the JSON
        // shape: EventKind has no rename_all.
        let income = EventKind::Income {
            instrument: Some(InstrumentId::new_random()),
            gross: rub(1_000_000),
            kind: Some(IncomeKind::Coupon),
        };
        let mut value = serde_json::to_value(&income).unwrap();
        value
            .get_mut("Income")
            .and_then(serde_json::Value::as_object_mut)
            .expect("variant serializes as an object")
            .remove("kind");
        let restored: EventKind = serde_json::from_value(value).unwrap();
        assert!(matches!(restored, EventKind::Income { kind: None, .. }));
    }

    #[test]
    fn every_income_kind_survives_a_json_round_trip() {
        for kind in [
            IncomeKind::Coupon,
            IncomeKind::Dividend,
            IncomeKind::DepositInterest,
        ] {
            let text = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<IncomeKind>(&text).unwrap(), kind);
        }
    }

    fn trade(side: TradeSide) -> EventKind {
        EventKind::Trade {
            side,
            instrument: InstrumentId::new_random(),
            quantity: Quantity::zero(),
            gross: rub(5_000_000),
            fee: None,
            accrued_interest: None,
            basis_fee: None,
            basis_fee_exact: None,
        }
    }

    fn transfer() -> EventKind {
        EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from: AccountId::new_random(),
            to: AccountId::new_random(),
            amount: rub(10_000_000),
        }
    }

    fn opening_position() -> EventKind {
        EventKind::OpeningPosition {
            instrument: InstrumentId::new_random(),
            quantity: Quantity::zero(),
            cost_basis: None,
            assertions: OpeningAssertions::default(),
        }
    }

    // --- Discriminant ---

    #[test]
    fn every_variant_has_its_own_discriminant() {
        // The names go into the API and storage: a collision or substitution
        // would mean that two different facts were recorded identically.
        assert_eq!(trade(TradeSide::Buy).discriminant(), "trade");
        assert_eq!(
            EventKind::CashIn { amount: rub(1) }.discriminant(),
            "cash_in"
        );
        assert_eq!(
            EventKind::CashOut { amount: rub(1) }.discriminant(),
            "cash_out"
        );
        assert_eq!(transfer().discriminant(), "cash_transfer");
        assert_eq!(
            EventKind::Income {
                instrument: None,
                gross: rub(1),
                kind: None,
            }
            .discriminant(),
            "income"
        );
        assert_eq!(
            EventKind::Fee {
                amount: rub(-1),
                origin: FeeOrigin::Brokerage
            }
            .discriminant(),
            "fee"
        );
        assert_eq!(opening_position().discriminant(), "opening_position");
        assert_eq!(
            EventKind::OpeningCash { amount: rub(1) }.discriminant(),
            "opening_cash"
        );
    }

    #[test]
    fn the_side_of_a_trade_does_not_change_its_discriminant() {
        // Purchase and sale are one event type with different directions,
        // not two types: lot disposal distinguishes them by `side`.
        assert_eq!(
            trade(TradeSide::Buy).discriminant(),
            trade(TradeSide::Sell).discriminant()
        );
    }

    // --- Movement endpoints ---

    #[test]
    fn external_cash_has_outside_endpoints() {
        assert_eq!(
            EventKind::CashIn { amount: rub(1) }.flow_endpoints(),
            FlowEndpoints::InboundFromOutside
        );
        assert_eq!(
            EventKind::CashOut { amount: rub(1) }.flow_endpoints(),
            FlowEndpoints::OutboundToOutside
        );
    }

    #[test]
    fn transfer_reports_both_accounts() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        assert_eq!(
            kind.flow_endpoints(),
            FlowEndpoints::BetweenAccounts { from, to }
        );
    }

    #[test]
    fn transfer_endpoints_keep_their_direction() {
        // Swapping the accounts produces a different event: a transfer
        // from a deposit to a brokerage account and the reverse are classified
        // differently by the contour.
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        assert_ne!(
            kind.flow_endpoints(),
            FlowEndpoints::BetweenAccounts { from: to, to: from }
        );
    }

    #[test]
    fn buying_a_security_stays_within_the_account() {
        let kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument: InstrumentId::new_random(),
            quantity: Quantity::zero(),
            gross: rub(5_000_000),
            fee: None,
            accrued_interest: None,
            basis_fee: None,
            basis_fee_exact: None,
        };
        assert_eq!(kind.flow_endpoints(), FlowEndpoints::WithinAccount);
    }

    #[test]
    fn income_stays_within_the_account() {
        assert_eq!(
            EventKind::Income {
                instrument: None,
                gross: rub(100_000),
                kind: None,
            }
            .flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
    }

    #[test]
    fn fee_and_opening_balances_stay_within_the_account() {
        // Fees and reconstructed balances do not cross the contour boundary
        // by themselves: a reconstructed balance is not an external cash inflow.
        assert_eq!(
            EventKind::Fee {
                amount: rub(-3_500),
                origin: FeeOrigin::AccountMaintenance
            }
            .flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
        assert_eq!(
            opening_position().flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
        assert_eq!(
            EventKind::OpeningCash {
                amount: rub(10_000_000)
            }
            .flow_endpoints(),
            FlowEndpoints::WithinAccount
        );
    }
}
