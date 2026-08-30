//! Lot book (§4.12).
//!
//! Lots are built **by event type**: a purchase adds a lot, while a sale
//! disposes of it using a versioned registry rule. The quantity of securities
//! is calculated independently from the event legs (`super::balances`).
//!
//! A reconstructed position without documented cost (§10.7)
//! **does not become a zero-cost lot**: it is stored as a separate
//! quantity, disposed of first, and makes the realised result
//! uncomputable. A zero placeholder here would imply fabricated profit
//! equal to the entire proceeds.

use super::ownership::Ownership;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dates::TradeDate;
use crate::event::Event;
use crate::event::allocation::BasisAllocation;
use crate::event::corporate_action::{BasisTransferRule, CorporateAction};
use crate::event::kind::{DateCertainty, EventKind, IncomeKind, TradeSide};
use crate::event::offer::OfferExerciseAction;
use crate::ids::{AccountId, EventId, InstrumentId};
use crate::money::{Money, MoneyError, Quantity};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::rules::amortisation::{AmortisationError, AmortisationRuleVersion};
use crate::rules::lot_disposal::{
    DisposalError, DisposalInput, DisposalResult, Lot, LotId, RuleId, split_basis,
};
use crate::rules::{LotRuleVersion, RuleRegistry};
use crate::settlement::{SettlementKnowledge, SettlementLagPolicy};

/// Lots do not distinguish custody location: transferring a security between depositories
/// is not an acquisition and does not create a new lot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LotKey {
    pub account: AccountId,
    pub instrument: InstrumentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LotError {
    #[error("the registry has no disposal rule for version {version:?}")]
    UnknownRule { version: LotRuleVersion },
    #[error("sale {event:?} without a preceding position in instrument {instrument:?}")]
    SaleWithoutPosition {
        event: EventId,
        instrument: InstrumentId,
    },
    #[error("the registry has no amortisation rule for version {version:?}")]
    UnknownAmortisationRule { version: AmortisationRuleVersion },
    #[error(
        "the event declares quantity {declared:?}, but the account holds {held:?} of this security: \
         the corporate action applies to the entire position, and the discrepancy is a source defect"
    )]
    QuantityMismatch { held: Quantity, declared: Quantity },
    #[error(transparent)]
    Amortisation(#[from] AmortisationError),
    #[error(transparent)]
    Disposal(#[from] DisposalError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Why the realised result for the instrument cannot be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisGap {
    /// The position was reconstructed without documented cost (§10.7).
    RestoredWithoutBasis,
    /// The allocation ratio is unknown, so there is no basis for calculating
    /// the share of cost returned through amortisation (§4.9). The fact is applied,
    /// but the realised result is uncomputable.
    AmortisationAllocationUnknown,
}

impl BasisGap {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RestoredWithoutBasis => "restored_without_basis",
            Self::AmortisationAllocationUnknown => "amortisation_allocation_unknown",
        }
    }
}

/// A group of lots acquired on the same date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cohort {
    pub acquired: TradeDate,
    pub quantity: Quantity,
    pub cost_basis: Money,
    #[serde(default)]
    pub acquisition_basis: Option<Money>,
    pub accrued_interest_paid: Option<Money>,
    pub received_to_date: Option<Money>,
}

/// Why the lifetime cohort metric cannot be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum CohortGap {
    #[error("acquisition date is unknown")]
    AcquisitionDateUnknown,
    #[error("income type is unknown")]
    IncomeKindUnknown,
    #[error("position was reconstructed without documented cost")]
    RestoredWithoutBasis,
    #[error("lot quantity overflows when added")]
    InconsistentQuantity,
    #[error("lot cost currencies differ")]
    InconsistentCostBasisCurrency,
    #[error("lot cost overflows when added")]
    InconsistentCostBasisOverflow,
    #[error("currencies of additional lot monetary amounts differ")]
    InconsistentOptionalMoneyCurrency,
    #[error("additional lot monetary amounts overflow when added")]
    InconsistentOptionalMoneyOverflow,
}

/// Acquisitions ever observed for the (account, instrument) pair.
///
/// Kept separately from live lots because disposal of a lot does not erase
/// the fact that the security was already held on that date: an ownership boundary calculated
/// from the remaining lots would move forward after an early lot is sold and
/// hide a missing payment for a period when the security was held.
/// The value is monotonic: disposal does not change it.
///
/// `#[serde(default)]` is intentionally omitted: a snapshot without it would look like
/// a position with no acquisition history, thereby reporting «did not own»
/// as «unknown». `PROJECTION_VERSION` rejects snapshots from the previous version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AcquisitionHistory {
    /// The earliest observed acquisition date.
    earliest: Option<TradeDate>,
    /// An acquisition without a date was observed: either a lot without a trade date or
    /// a quantity reconstructed without cost. The flag is sticky for the
    /// same reason as the date itself: disposing of an undated
    /// lot does not turn an unknown boundary into a known one.
    undated: bool,
}

impl AcquisitionHistory {
    fn observe(&mut self, acquired: Option<TradeDate>) {
        match acquired {
            Some(date) => {
                self.earliest = Some(match self.earliest {
                    Some(known) => known.min(date),
                    None => date,
                });
            }
            None => self.undated = true,
        }
    }

    /// Lower ownership boundary. `None` when an acquisition
    /// without a date was observed: any boundary based on the other lots would be later
    /// than the actual one and would hide the omission.
    const fn lower_bound(self) -> Option<TradeDate> {
        if self.undated { None } else { self.earliest }
    }
}

/// Lots for one instrument in one account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentLots {
    /// Quantity reconstructed without cost. Disposed of first:
    /// it was acquired before everything else the system has seen.
    unpriced: Quantity,
    /// Lots in acquisition order.
    lots: Vec<Lot>,
    /// Pretax realised result. `None` if at least one
    /// disposal affected a quantity without cost.
    realized: Option<Money>,
    /// Total cost of all acquisitions with a documented price.
    acquired_basis: Option<Money>,
    /// Total cost disposed of through disposals.
    released_basis: Option<Money>,
    gap: Option<BasisGap>,
    /// A payment of unknown type that cannot be attributed to lots.
    #[serde(default)]
    income_kind_unknown: bool,
    /// A known payment, part of which was attributable to a reconstructed
    /// quantity or to a position without lots.
    #[serde(default)]
    unallocated_income: Option<Money>,
    /// The share of a known payment attributable to the reconstructed quantity.
    #[serde(default)]
    unpriced_income: Option<Money>,
    /// Acquisitions ever observed for the pair.
    acquisitions: AcquisitionHistory,
    /// Quantity-change history with settlement knowledge.
    ///
    /// `#[serde(default)]` is intentionally omitted: a snapshot without history cannot honestly
    /// be treated as a position without ownership; the projection version must reject it.
    ownership: super::ownership::OwnershipHistory,
}

/// An empty book for an instrument. Implemented manually because `Quantity`
/// intentionally does not implement `Default`: a zero quantity must be created
/// explicitly, not as the default value of an unknown field (§4.9).
impl Default for InstrumentLots {
    fn default() -> Self {
        Self {
            unpriced: Quantity::zero(),
            lots: Vec::new(),
            realized: None,
            acquired_basis: None,
            released_basis: None,
            gap: None,
            income_kind_unknown: false,
            unallocated_income: None,
            unpriced_income: None,
            acquisitions: AcquisitionHistory::default(),
            ownership: super::ownership::OwnershipHistory::default(),
        }
    }
}

impl InstrumentLots {
    #[must_use]
    pub fn lots(&self) -> &[Lot] {
        &self.lots
    }

    #[must_use]
    pub const fn unpriced(&self) -> Quantity {
        self.unpriced
    }

    #[must_use]
    pub const fn realized(&self) -> Option<Money> {
        self.realized
    }

    #[must_use]
    pub const fn gap(&self) -> Option<BasisGap> {
        self.gap
    }

    #[must_use]
    pub const fn income_kind_unknown(&self) -> bool {
        self.income_kind_unknown
    }

    /// The earliest acquisition date ever observed for the pair.
    ///
    /// Calculated from the entire history, not from live lots: selling an early
    /// lot does not move the ownership boundary forward, otherwise a missing payment for
    /// a period when the security was held would remain unidentified.
    ///
    /// `None` if there is a quantity reconstructed without cost,
    /// or if at least one acquisition had no date: there is then no basis
    /// for establishing the ownership boundary, while approximating it would
    /// either fabricate a defect or hide a real one.
    #[must_use]
    pub fn earliest_acquired(&self) -> Option<TradeDate> {
        if !self.unpriced.0.is_zero() {
            return None;
        }
        self.acquisitions.lower_bound()
    }

    /// Ownership status as of a date, accounting for all quantity changes.
    #[must_use]
    pub fn ownership_at(&self, day: time::Date) -> Ownership {
        self.ownership.ownership_at(day)
    }

    /// The only entry point for a new lot: the acquisition history
    /// must be updated together with the lots, otherwise the ownership boundary
    /// will diverge from the journal.
    #[cfg(test)]
    fn push_lot(&mut self, lot: Lot) {
        self.push_lot_with_settlement(lot, SettlementKnowledge::Unbounded);
    }

    fn push_lot_with_settlement(&mut self, lot: Lot, settlement: SettlementKnowledge) {
        self.acquisitions.observe(lot.acquired);
        self.ownership.observe(lot.quantity, settlement);
        self.lots.push(lot);
    }

    /// A reconstructed lot goes to the front of the FIFO queue: it is older
    /// than everything else the system has seen.
    fn insert_restored_lot_with_settlement(&mut self, lot: Lot, settlement: SettlementKnowledge) {
        self.acquisitions.observe(lot.acquired);
        self.ownership.observe(lot.quantity, settlement);
        self.lots.insert(0, lot);
    }

    /// A known payment that cannot be attributed to a documented lot.
    #[must_use]
    pub const fn unallocated_income(&self) -> Option<Money> {
        self.unallocated_income
    }

    /// The share of known payments attributable to the reconstructed quantity.
    #[must_use]
    pub const fn unpriced_income(&self) -> Option<Money> {
        self.unpriced_income
    }

    /// Acquisition cost. Together with [`Self::released_basis`] it forms
    /// a verifiable identity: acquired = remaining + disposed.
    #[must_use]
    pub const fn acquired_basis(&self) -> Option<Money> {
        self.acquired_basis
    }

    #[must_use]
    pub const fn released_basis(&self) -> Option<Money> {
        self.released_basis
    }

    /// Cost of unsold lots.
    pub fn remaining_basis(&self) -> Result<Option<Money>, MoneyError> {
        let Some(first) = self.lots.first() else {
            return Ok(None);
        };
        let amounts: Vec<Money> = self.lots.iter().map(|lot| lot.cost_basis).collect();
        Money::sum(&amounts, first.cost_basis.currency()).map(Some)
    }

    /// Lots for modification within the module.
    ///
    /// An accessor method rather than direct access to the private field: modifying
    /// lots is a book operation, and its location is visible at call sites.
    fn lots_mut(&mut self) -> &mut [Lot] {
        &mut self.lots
    }

    /// Mark a gap in cost. The realised result then
    /// becomes uncomputable: one gap makes the entire instrument
    /// uncomputable, not «almost all of it».
    fn mark_basis_gap(&mut self, gap: BasisGap) {
        self.gap = Some(gap);
        self.realized = None;
    }

    /// Add to the realised result if it is still computable.
    fn add_realised(&mut self, amount: Money) -> Result<(), MoneyError> {
        if self.gap.is_some() {
            return Ok(());
        }
        self.realized = Some(match self.realized {
            Some(previous) => previous.try_add(amount)?,
            None => amount,
        });
        Ok(())
    }

    /// Add the disposed cost: the identity «acquired = remaining
    /// plus disposed» is checked by a projection invariant.
    fn add_released_basis(&mut self, amount: Money) -> Result<(), MoneyError> {
        self.released_basis = Some(match self.released_basis {
            Some(previous) => previous.try_add(amount)?,
            None => amount,
        });
        Ok(())
    }

    fn add_acquired_basis(&mut self, amount: Money) -> Result<(), MoneyError> {
        self.acquired_basis = Some(match self.acquired_basis {
            Some(previous) => previous.try_add(amount)?,
            None => amount,
        });
        Ok(())
    }

    /// Total quantity: lots plus the reconstructed balance.
    pub fn quantity(&self) -> Result<Quantity, NumericError> {
        self.lots
            .iter()
            .try_fold(self.unpriced.0, |acc, lot| acc.checked_add(lot.quantity.0))
            .map(Quantity)
    }
    /// Allocates an actual payment to lots in proportion to quantity.
    ///
    /// The denominator also includes the reconstructed quantity. Its share
    /// is retained as unallocated: it has no lot to which the payment
    /// can be attributed.
    fn add_received(&mut self, amount: Money) -> Result<(), LotError> {
        let total_quantity = self.quantity()?.0.inner();
        if total_quantity.is_zero() {
            self.unallocated_income = Some(match self.unallocated_income {
                Some(previous) => previous.try_add(amount)?,
                None => amount,
            });
            return Ok(());
        }

        let mut remaining_amount = amount;
        let mut remaining_quantity = total_quantity;
        if !self.unpriced.0.is_zero() {
            let unpriced_amount = split_basis(amount, self.unpriced.0.inner(), total_quantity)?;
            self.unpriced_income = Some(match self.unpriced_income {
                Some(previous) => previous.try_add(unpriced_amount)?,
                None => unpriced_amount,
            });
            remaining_amount = remaining_amount.try_sub(unpriced_amount)?;
            remaining_quantity = remaining_quantity
                .checked_sub(self.unpriced.0.inner())
                .ok_or(NumericError::Overflow)?;
        }

        for lot in self.lots_mut() {
            let lot_quantity = lot.quantity.0.inner();
            if lot_quantity.is_zero() {
                continue;
            }
            let received = split_basis(remaining_amount, lot_quantity, remaining_quantity)?;
            lot.received_to_date = Some(match lot.received_to_date {
                Some(previous) => previous.try_add(received)?,
                None => received,
            });
            remaining_amount = remaining_amount.try_sub(received)?;
            remaining_quantity = remaining_quantity
                .checked_sub(lot_quantity)
                .ok_or(NumericError::Overflow)?;
        }
        Ok(())
    }

    /// Groups the remaining lots by acquisition date.
    pub fn cohorts(&self) -> Result<Vec<Cohort>, CohortGap> {
        if !self.unpriced.0.is_zero() {
            return Err(CohortGap::AcquisitionDateUnknown);
        }
        if self.income_kind_unknown {
            return Err(CohortGap::IncomeKindUnknown);
        }
        if self.gap == Some(BasisGap::RestoredWithoutBasis) {
            return Err(CohortGap::RestoredWithoutBasis);
        }

        let mut grouped: BTreeMap<TradeDate, Vec<&Lot>> = BTreeMap::new();
        for lot in &self.lots {
            let acquired = lot.acquired.ok_or(CohortGap::AcquisitionDateUnknown)?;
            grouped.entry(acquired).or_default().push(lot);
        }

        let mut cohorts = Vec::with_capacity(grouped.len());
        for (acquired, lots) in grouped {
            let mut quantity = Dec::zero();
            let acquisition_basis = Self::sum_optional_money(&lots, |lot| lot.acquisition_basis)?;
            let mut cost_basis = Money::zero(lots[0].cost_basis.currency());
            for lot in &lots {
                quantity = quantity
                    .checked_add(lot.quantity.0)
                    .map_err(|_| CohortGap::InconsistentQuantity)?;
                cost_basis = cost_basis
                    .try_add(lot.cost_basis)
                    .map_err(|error| match error {
                        MoneyError::CurrencyMismatch { .. } => {
                            CohortGap::InconsistentCostBasisCurrency
                        }
                        MoneyError::Overflow | MoneyError::Numeric(_) => {
                            CohortGap::InconsistentCostBasisOverflow
                        }
                    })?;
            }
            cohorts.push(Cohort {
                acquired,
                quantity: Quantity(quantity),
                cost_basis,
                acquisition_basis,
                accrued_interest_paid: Self::sum_optional_money(&lots, |lot| {
                    lot.accrued_interest_paid
                })?,
                received_to_date: Self::sum_optional_money(&lots, |lot| lot.received_to_date)?,
            });
        }
        Ok(cohorts)
    }

    fn sum_optional_money<F>(lots: &[&Lot], select: F) -> Result<Option<Money>, CohortGap>
    where
        F: Fn(&Lot) -> Option<Money>,
    {
        let mut total: Option<Money> = None;
        for lot in lots {
            let Some(value) = select(lot) else {
                return Ok(None);
            };
            total = Some(match total {
                Some(previous) => previous.try_add(value).map_err(|error| match error {
                    MoneyError::CurrencyMismatch { .. } => {
                        CohortGap::InconsistentOptionalMoneyCurrency
                    }
                    MoneyError::Overflow | MoneyError::Numeric(_) => {
                        CohortGap::InconsistentOptionalMoneyOverflow
                    }
                })?,
                None => value,
            });
        }
        Ok(total)
    }
}

/// Trade facts needed by the lot book. A separate structure rather than eight
/// arguments: the `too-many-arguments-threshold = 6` threshold in `clippy.toml`
/// is enforced, and suppressing the lint is prohibited (§15.7).
#[derive(Debug, Clone, Copy, PartialEq)]
struct TradeFacts {
    side: TradeSide,
    instrument: InstrumentId,
    quantity: Quantity,
    gross: Money,
    fee: Option<Money>,
    accrued_interest: Option<Money>,
}

/// Amortisation facts needed by the lot book.
#[derive(Debug, Clone, PartialEq)]
struct AmortisationFacts {
    instrument: InstrumentId,
    quantity: Quantity,
    allocation: BasisAllocation,
    compensation: Money,
}
/// Substitution facts needed by the lot book.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ConversionFacts {
    predecessor: InstrumentId,
    successor: InstrumentId,
    ratio: Dec,
    quantity_in: Quantity,
    quantity_out: Quantity,
    basis_transfer: BasisTransferRule,
    effective_date: time::Date,
}
/// Data for one reconstructed position: instrument, quantity, cost, and
/// declared acquisition date. These four values describe the position itself,
/// so they are grouped together, while the event and settlement knowledge remain
/// circumstances of the record.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RestoreFacts {
    instrument: InstrumentId,
    quantity: Quantity,
    cost_basis: Option<Money>,
    acquired: Option<TradeDate>,
}

/// Data for one disposal: lot key, quantity, and proceeds. These values
/// constitute one disposal operation and are passed together, while the event,
/// rule, and settlement knowledge are its context.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DisposalFacts {
    key: LotKey,
    quantity: Quantity,
    proceeds: Money,
}

/// Lot book and applied rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LotBook {
    entries: BTreeMap<LotKey, InstrumentLots>,
    rule_version: LotRuleVersion,
    applied_rule: Option<RuleId>,
    /// Amortisation rule version. Separate from the disposal version:
    /// lot disposal is the owner's choice, while amortisation is an issuer event.
    ///
    /// `#[serde(default)]` is required: projection snapshots were written before E3.4
    /// and do not contain this field.
    #[serde(default = "default_amortisation_version")]
    amortisation_version: AmortisationRuleVersion,
    settlement_policy: SettlementLagPolicy,
}

/// Amortisation rule version in a snapshot written before E3.4.
///
/// One, not «unknown»: before E3.4 amortisation was not applied at all,
/// so continuing from such a snapshot does not recalculate anything
/// retroactively — it applies the rule to facts that did not previously exist.
fn default_amortisation_version() -> AmortisationRuleVersion {
    AmortisationRuleVersion(1)
}

impl LotBook {
    #[must_use]
    pub fn new(rule_version: LotRuleVersion) -> Self {
        Self {
            entries: BTreeMap::new(),
            rule_version,
            applied_rule: None,
            amortisation_version: default_amortisation_version(),
            settlement_policy: SettlementLagPolicy::default(),
        }
    }

    /// A book with an explicitly selected amortisation rule version.
    #[must_use]
    pub fn with_amortisation_version(mut self, version: AmortisationRuleVersion) -> Self {
        self.amortisation_version = version;
        self
    }
    /// Select the calculation-band table for this book.
    #[must_use]
    pub fn with_settlement_lag_policy(mut self, policy: SettlementLagPolicy) -> Self {
        self.settlement_policy = policy;
        self
    }

    #[must_use]
    pub const fn amortisation_version(&self) -> AmortisationRuleVersion {
        self.amortisation_version
    }

    #[must_use]
    pub const fn rule_version(&self) -> LotRuleVersion {
        self.rule_version
    }

    /// Identifier of the rule actually used to dispose of lots.
    /// Included in the report and audit trail: without it, the figure cannot be reproduced (§3.2).
    #[must_use]
    pub const fn applied_rule(&self) -> Option<&RuleId> {
        self.applied_rule.as_ref()
    }

    #[must_use]
    pub fn entry(&self, key: &LotKey) -> Option<&InstrumentLots> {
        self.entries.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&LotKey, &InstrumentLots)> {
        self.entries.iter()
    }

    /// Applying an event to the lot book.
    ///
    /// The dispatcher is exhaustive: a new event type must break the build
    /// here rather than silently fail to create a lot.
    pub fn apply(&mut self, event: &Event, rules: &RuleRegistry) -> Result<(), LotError> {
        let settlement = self
            .settlement_policy
            .knowledge(&event.dates, event.provenance.parser_version());
        match &event.kind {
            EventKind::Trade {
                side,
                instrument,
                quantity,
                gross,
                fee,
                accrued_interest,
            } => self.apply_trade(
                event,
                TradeFacts {
                    side: *side,
                    instrument: *instrument,
                    quantity: *quantity,
                    gross: *gross,
                    fee: *fee,
                    accrued_interest: *accrued_interest,
                },
                settlement,
                rules,
            ),
            // Assertions about the reconstructed opening state determine the
            // ownership boundary: the event date is the import time, not evidence
            // of the position's origin.
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis,
                assertions,
            } => {
                let acquired = match (
                    assertions.acquisition_date,
                    assertions.acquisition_date_certainty,
                ) {
                    (Some(day), DateCertainty::Known) => Some(TradeDate(day)),
                    // An estimated date must not become the cohort date:
                    // otherwise the owner's guess would again be presented as fact.
                    _ => None,
                };
                let settlement = match acquired {
                    Some(day) => SettlementKnowledge::Exact(day.0),
                    None => {
                        // An estimate does not become a proven start:
                        // continuity of ownership before the journal was opened
                        // is fundamentally unprovable (§3.5).
                        SettlementKnowledge::Unbounded
                    }
                };
                self.restore(
                    event,
                    RestoreFacts {
                        instrument: *instrument,
                        quantity: *quantity,
                        cost_basis: *cost_basis,
                        acquired,
                    },
                    settlement,
                )
            }
            EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. } => Ok(()),
            EventKind::Income {
                instrument: Some(instrument),
                gross,
                kind,
            } => self.apply_income(event, *instrument, *gross, *kind),
            EventKind::Income {
                instrument: None, ..
            } => Ok(()),
            EventKind::CorporateAction { action } => {
                self.apply_corporate_action(event, action, settlement, rules)
            }
            EventKind::OfferExercise { action } => {
                self.apply_offer_exercise(event, action, settlement, rules)
            }
        }
    }

    fn apply_income(
        &mut self,
        event: &Event,
        instrument: InstrumentId,
        amount: Money,
        kind: Option<IncomeKind>,
    ) -> Result<(), LotError> {
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let entry = self.entries.entry(key).or_default();
        match kind {
            Some(IncomeKind::Coupon) => entry.add_received(amount),
            Some(IncomeKind::Dividend | IncomeKind::DepositInterest) => Ok(()),
            None => {
                entry.income_kind_unknown = true;
                Ok(())
            }
        }
    }

    /// Acquisition cost includes commission and **excludes accrued coupon interest**:
    /// accrued coupon interest is returned through the coupon, not through the sale,
    /// so it is not part of the security's cost (§7.2). The tax
    /// basis under Art. 214.1 is calculated differently and will appear in E5 —
    /// which is why it is versioned by the rule.
    fn apply_trade(
        &mut self,
        event: &Event,
        trade: TradeFacts,
        settlement: SettlementKnowledge,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let TradeFacts {
            side,
            instrument,
            quantity,
            gross,
            fee,
            accrued_interest,
        } = trade;
        let key = LotKey {
            account: event.account,
            instrument,
        };
        match side {
            TradeSide::Buy => {
                let basis = match fee {
                    Some(f) => gross.try_add(f)?,
                    None => gross,
                };
                let entry = self.entries.entry(key).or_default();
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.push_lot_with_settlement(
                    Lot {
                        // The lot identifier is derived from the acquisition event:
                        // the core is pure and cannot contain random identifiers,
                        // otherwise reprojection of the same journal would produce a different
                        // result (§3.1, §15.3).
                        id: LotId(event.id.inner()),
                        instrument,
                        acquired: event.dates.trade,
                        quantity,
                        // Missing accrued coupon interest remains unknown rather than zero.
                        accrued_interest_paid: accrued_interest,
                        received_to_date: None,
                        cost_basis: basis,
                        acquisition_basis: Some(basis),
                    },
                    settlement,
                );
                Ok(())
            }
            TradeSide::Sell => {
                let proceeds = match fee {
                    Some(f) => gross.try_sub(f)?,
                    None => gross,
                };
                self.dispose(
                    event,
                    DisposalFacts {
                        key,
                        quantity,
                        proceeds,
                    },
                    rules,
                    settlement,
                )
            }
        }
    }

    /// Corporate action on a security (§4.7).
    ///
    /// The dispatcher is exhaustive: a new member of the family must break
    /// the build here rather than silently leave the lots unchanged.
    fn apply_corporate_action(
        &mut self,
        event: &Event,
        action: &CorporateAction,
        settlement: SettlementKnowledge,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        match action {
            CorporateAction::PartialRedemption {
                instrument,
                quantity,
                basis_allocation,
                compensation,
                ..
            } => self.apply_amortisation(
                event,
                AmortisationFacts {
                    instrument: *instrument,
                    quantity: *quantity,
                    allocation: basis_allocation.clone(),
                    compensation: *compensation,
                },
                rules,
            ),
            // Redemption returns the full face value and removes the security
            // from the position: this is a disposal of the entire position and is processed
            // through the same path as a sale. Disposal order for a full disposal
            // is irrelevant, so the owner's rule choice changes nothing
            // — but the audit trail remains the same.
            CorporateAction::Redemption {
                instrument,
                quantity,
                compensation,
                ..
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                self.require_whole_position(event, key, *quantity)?;
                self.dispose(
                    event,
                    DisposalFacts {
                        key,
                        quantity: *quantity,
                        proceeds: *compensation,
                    },
                    rules,
                    settlement,
                )
            }
            CorporateAction::Conversion {
                predecessor,
                successor,
                ratio,
                quantity_in,
                quantity_out,
                basis_transfer,
                effective_date,
                ..
            } => self.apply_conversion(
                event,
                ConversionFacts {
                    predecessor: *predecessor,
                    successor: *successor,
                    ratio: *ratio,
                    quantity_in: *quantity_in,
                    quantity_out: *quantity_out,
                    basis_transfer: *basis_transfer,
                    effective_date: *effective_date,
                },
                settlement,
            ),
        }
    }

    /// Offer execution (§3.5).
    fn apply_offer_exercise(
        &mut self,
        event: &Event,
        action: &OfferExerciseAction,
        settlement: SettlementKnowledge,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        match action {
            // Submitting and canceling the offer do not move lots: the security remains
            // with the owner until the repurchase occurs. Their state is managed by
            // the separate `super::offers::OfferBook` projection.
            OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => Ok(()),
            // A repurchase is a disposal: the security leaves and the money arrives.
            OfferExerciseAction::Settled {
                instrument,
                quantity,
                gross,
                fee,
                accrued_interest,
                ..
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                let mut proceeds = *gross;
                if let Some(interest) = accrued_interest {
                    proceeds = proceeds.try_add(*interest)?;
                }
                if let Some(fee) = fee {
                    proceeds = proceeds.try_sub(*fee)?;
                }
                self.dispose(
                    event,
                    DisposalFacts {
                        key,
                        quantity: *quantity,
                        proceeds,
                    },
                    rules,
                    settlement,
                )
            }
        }
    }

    /// Amortisation: the remaining face value decreases, while quantity does not (§6.5).
    ///
    /// Target the «account and security» pair: [`LotKey`] intentionally does not distinguish
    /// custody location, and custody from the event is a fact about the payment, not a lot
    /// selection key.
    fn apply_amortisation(
        &mut self,
        event: &Event,
        facts: AmortisationFacts,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let key = LotKey {
            account: event.account,
            instrument: facts.instrument,
        };
        self.require_whole_position(event, key, facts.quantity)?;
        let rule = rules.amortisation_rule(self.amortisation_version).ok_or(
            LotError::UnknownAmortisationRule {
                version: self.amortisation_version,
            },
        )?;
        let entry = self
            .entries
            .get(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: facts.instrument,
            })?;

        // Compute using a copy and replace the whole value: otherwise failure on the second
        // lot would leave the first one already modified, and a half-applied
        // fact is worse than an unapplied one.
        let mut next = entry.clone();
        let mut returned_total = Money::zero(facts.compensation.currency());
        match facts.allocation {
            BasisAllocation::Known { share, .. } => {
                for lot in next.lots_mut() {
                    let returned = rule.basis_returned(lot, share)?;
                    lot.cost_basis = lot.cost_basis.try_sub(returned)?;
                    returned_total = returned_total.try_add(returned)?;
                }
            }
            // The allocation ratio is unknown — the fact is still applied, while
            // the realised result becomes uncomputable (§4.9).
            // There was no basis for allocation, but the fact cannot
            // be rejected: the money arrived.
            BasisAllocation::Unknown(_) => {
                next.mark_basis_gap(BasisGap::AmortisationAllocationUnknown);
            }
        }
        next.add_received(facts.compensation)?;
        if !returned_total.is_zero() {
            next.add_released_basis(returned_total)?;
        }
        // A return of capital is not income: when
        // compensation equals the returned cost, the realised result is zero.
        next.add_realised(facts.compensation.try_sub(returned_total)?)?;
        self.entries.insert(key, next);
        Ok(())
    }

    /// Substitution: predecessor lots become successor lots.
    fn apply_conversion(
        &mut self,
        event: &Event,
        facts: ConversionFacts,
        settlement: SettlementKnowledge,
    ) -> Result<(), LotError> {
        let from = LotKey {
            account: event.account,
            instrument: facts.predecessor,
        };
        self.require_whole_position(event, from, facts.quantity_in)?;
        let quantity_in_delta = Quantity(facts.quantity_in.0.checked_neg()?);
        let source = self
            .entries
            .get(&from)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: facts.predecessor,
            })?
            .clone();

        let mut moved = Vec::with_capacity(source.lots().len());
        let mut assigned = Dec::zero();
        let mut carried = Vec::with_capacity(source.lots().len());
        for (index, lot) in source.lots().iter().enumerate() {
            let last = index + 1 == source.lots().len();
            // The final lot receives the remainder: the fraction was rounded
            // at the level of the entire substitution, and there is no basis
            // for allocating it back across lots. This makes the sum of lot
            // quantities exactly equal to the quantity from the fact, not approximately.
            let quantity = if last {
                Quantity(facts.quantity_out.0.checked_sub(assigned)?)
            } else {
                Quantity(lot.quantity.0.checked_mul(facts.ratio)?)
            };
            assigned = assigned.checked_add(quantity.0)?;
            carried.push(lot.cost_basis);
            moved.push(Lot {
                id: lot.id,
                instrument: facts.successor,
                acquired: match facts.basis_transfer {
                    // The holding period carries over in full: substitution
                    // is not an acquisition (§16.1).
                    BasisTransferRule::CarryOver => lot.acquired,
                    // The substitution is treated as a sale and purchase.
                    BasisTransferRule::Restart => Some(TradeDate(facts.effective_date)),
                },
                quantity,
                // The cost is carried over unchanged. Fractional compensation
                // is **not** deducted from it: how it affects the basis is
                // an E5 rule, and part 1 must not decide on its behalf.
                cost_basis: lot.cost_basis,
                acquisition_basis: lot.acquisition_basis,
                accrued_interest_paid: lot.accrued_interest_paid,
                received_to_date: lot.received_to_date,
            });
        }
        let currency = match carried.first() {
            Some(first) => first.currency(),
            // A position without lots: there is nothing to substitute, but this is not an error —
            // the quantity has already been reconciled with the fact.
            None => return Ok(()),
        };
        let carried_total = Money::sum(&carried, currency)?;

        let mut source = source;
        source.lots.clear();
        source.add_released_basis(carried_total)?;
        source.ownership.observe(quantity_in_delta, settlement);
        self.entries.insert(from, source);

        let to = LotKey {
            account: event.account,
            instrument: facts.successor,
        };
        let target = self.entries.entry(to).or_default();
        target.add_acquired_basis(carried_total)?;
        target.ownership.observe(facts.quantity_out, settlement);
        for lot in moved {
            // Moving a lot is not a new acquisition: the old
            // AcquisitionHistory is retained separately from the ownership delta.
            target.acquisitions.observe(lot.acquired);
            target.lots.push(lot);
        }
        Ok(())
    }

    /// A corporate action applies to the entire position in the security on the account.
    ///
    /// A discrepancy is a source defect, not a reason to reduce face value
    /// proportionally: scaling would present corrupted data
    /// as a correct calculation.
    fn require_whole_position(
        &self,
        event: &Event,
        key: LotKey,
        declared: Quantity,
    ) -> Result<(), LotError> {
        let entry = self
            .entries
            .get(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: key.instrument,
            })?;
        let held = entry.quantity()?;
        if held == declared {
            Ok(())
        } else {
            Err(LotError::QuantityMismatch { held, declared })
        }
    }

    fn restore(
        &mut self,
        event: &Event,
        facts: RestoreFacts,
        settlement: SettlementKnowledge,
    ) -> Result<(), LotError> {
        let RestoreFacts {
            instrument,
            quantity,
            cost_basis,
            acquired,
        } = facts;
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let entry = self.entries.entry(key).or_default();
        match cost_basis {
            // A reconstructed lot is older than everything else the system has seen,
            // so it goes to the front of the FIFO queue, not the back.
            Some(basis) => {
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.insert_restored_lot_with_settlement(
                    Lot {
                        id: LotId(event.id.inner()),
                        instrument,
                        acquired,
                        quantity,
                        accrued_interest_paid: None,
                        received_to_date: None,
                        cost_basis: basis,
                        acquisition_basis: Some(basis),
                    },
                    settlement,
                );
            }
            None => {
                entry.unpriced = Quantity(entry.unpriced.0.checked_add(quantity.0)?);
                entry.gap = Some(BasisGap::RestoredWithoutBasis);
                // The reconstructed quantity has no date and was acquired
                // before everything else the system has seen: the ownership boundary
                // for this pair remains unprovable even after the quantity
                // is disposed of.
                entry.acquisitions.observe(None);
                entry.ownership.observe(quantity, settlement);
            }
        }
        Ok(())
    }

    fn dispose(
        &mut self,
        event: &Event,
        facts: DisposalFacts,
        rules: &RuleRegistry,
        settlement: SettlementKnowledge,
    ) -> Result<(), LotError> {
        let DisposalFacts {
            key,
            quantity,
            proceeds,
        } = facts;
        let delta = Quantity(quantity.0.checked_neg()?);
        let rule = rules
            .disposal_rule(self.rule_version)
            .ok_or(LotError::UnknownRule {
                version: self.rule_version,
            })?;
        let entry = self
            .entries
            .get_mut(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: key.instrument,
            })?;

        // The reconstructed quantity is disposed of first: it was acquired
        // before everything else the system observed. It has no cost,
        // so the realised result for the instrument becomes
        // uncomputable — but the quantity is disposed of correctly.
        let from_unpriced = entry.unpriced.0.min(quantity.0);
        if !from_unpriced.is_zero() {
            entry.unpriced = Quantity(entry.unpriced.0.checked_sub(from_unpriced)?);
            entry.realized = None;
            entry.gap = Some(BasisGap::RestoredWithoutBasis);
        }
        let left = quantity.0.checked_sub(from_unpriced)?;
        if left.is_zero() {
            entry.ownership.observe(delta, settlement);
            return Ok(());
        }

        let result: DisposalResult = rule.apply(&DisposalInput {
            lots: entry.lots.clone(),
            quantity: Quantity(left),
        })?;
        entry.lots = result.remaining.clone();
        entry.released_basis = Some(match entry.released_basis {
            Some(previous) => previous.try_add(result.basis_released)?,
            None => result.basis_released,
        });
        self.applied_rule = Some(result.rule.clone());

        // Pretax realised result: proceeds minus disposed
        // cost. It is not added to an uncomputable result: one gap makes
        // the entire instrument uncomputable, not «almost all of it».
        if entry.gap.is_none() {
            let realized = proceeds.try_sub(result.basis_released)?;
            entry.realized = Some(match entry.realized {
                Some(previous) => previous.try_add(realized)?,
                None => realized,
            });
        }
        entry.ownership.observe(delta, settlement);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::CustodyId;
    use crate::money::PerUnitAmount;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;
    use time::Date;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    // --- corporate actions and offer ---

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    /// A bond position assembled directly: the purchase event
    /// does not know the face value, while the test needs it as the initial state.
    struct Bond {
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
    }
    fn known_allocation(returned: &str) -> BasisAllocation {
        BasisAllocation::Known {
            share: crate::rules::ReturnedShare::new(
                dec(returned)
                    .checked_div(dec("1000"))
                    .expect("test bond face value is nonzero"),
            )
            .expect("ratio is within the invariant"),
            evidence: crate::event::allocation::AllocationEvidence {
                inputs_hash: crate::event::allocation::AllocationInputsHash::new("a".repeat(64))
                    .expect("input hash"),
                knowledge_as_of: time::OffsetDateTime::UNIX_EPOCH,
                algorithm_version: crate::event::allocation::AllocationAlgorithmVersion(1),
            },
        }
    }

    impl Bond {
        fn new() -> Self {
            Self {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
            }
        }

        fn key(&self) -> LotKey {
            LotKey {
                account: self.account,
                instrument: self.instrument,
            }
        }

        fn amortisation(&self, units: i64, returned: &str, compensation: i64) -> Event {
            self.amortisation_with_allocation(
                units,
                returned,
                compensation,
                known_allocation(returned),
            )
        }

        fn unknown_amortisation(&self, units: i64, returned: &str, compensation: i64) -> Event {
            self.amortisation_with_allocation(
                units,
                returned,
                compensation,
                BasisAllocation::default(),
            )
        }

        fn amortisation_with_allocation(
            &self,
            units: i64,
            returned: &str,
            compensation: i64,
            basis_allocation: BasisAllocation,
        ) -> Event {
            event_with(
                self.account,
                date!(2026 - 06 - 15),
                5,
                EventKind::CorporateAction {
                    action: CorporateAction::PartialRedemption {
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(units),
                        principal_returned_per_unit: per_unit(returned),
                        compensation: rub(compensation),
                        effective_date: date!(2026 - 06 - 15),
                        record_date: None,
                        grounds: None,
                        basis_allocation,
                    },
                },
                vec![Leg::principal(
                    self.account,
                    self.instrument,
                    rub(compensation),
                )],
            )
        }
    }

    fn bond_lot(bond: &Bond, units: i64, basis: i64) -> Lot {
        Lot {
            id: LotId::new_random(),
            instrument: bond.instrument,
            acquired: Some(crate::dates::TradeDate(date!(2024 - 03 - 01))),
            quantity: qty(units),
            cost_basis: rub(basis),
            acquisition_basis: Some(rub(basis)),
            accrued_interest_paid: None,
            received_to_date: None,
        }
    }

    fn book_with_lots(bond: &Bond, lots: Vec<Lot>) -> LotBook {
        let mut book = LotBook::new(LotRuleVersion(1));
        let acquired: Vec<Money> = lots.iter().map(|lot| lot.cost_basis).collect();
        let entry = book.entries.entry(bond.key()).or_default();
        entry.acquired_basis = Money::sum(&acquired, CurrencyCode::Rub).ok();
        for lot in lots {
            entry.push_lot(lot);
        }
        book
    }

    fn lot_with_acquired_date(instrument: InstrumentId, acquired: Option<Date>) -> Lot {
        Lot {
            id: LotId::new_random(),
            instrument,
            acquired: acquired.map(TradeDate),
            quantity: qty(10),
            cost_basis: rub(100_000),
            acquisition_basis: Some(rub(100_000)),
            accrued_interest_paid: None,
            received_to_date: None,
        }
    }

    #[test]
    fn ownership_boundary_uses_earliest_acquisition_date() {
        // Lots are inserted with `push_lot`, not by assignment: the acquisition
        // history is updated only through it, and a test bypassing
        // it would test the fixture, not the lot book.
        let instrument = InstrumentId::new_random();
        let mut entry = InstrumentLots::default();
        entry.push_lot(lot_with_acquired_date(
            instrument,
            Some(date!(2025 - 07 - 01)),
        ));
        entry.push_lot(lot_with_acquired_date(
            instrument,
            Some(date!(2024 - 03 - 01)),
        ));

        assert_eq!(
            entry.earliest_acquired(),
            Some(TradeDate(date!(2024 - 03 - 01)))
        );
    }

    #[test]
    fn lot_without_acquisition_date_cannot_establish_ownership_boundary() {
        let instrument = InstrumentId::new_random();
        let mut entry = InstrumentLots::default();
        entry.push_lot(lot_with_acquired_date(
            instrument,
            Some(date!(2024 - 03 - 01)),
        ));
        entry.push_lot(lot_with_acquired_date(instrument, None));

        assert_eq!(entry.earliest_acquired(), None);
    }

    #[test]
    fn restored_quantity_cannot_establish_ownership_boundary() {
        // It was acquired before everything else the system has seen and has
        // no date: any boundary based on the remaining lots would be later
        // than the actual one and would hide the omission.
        let instrument = InstrumentId::new_random();
        let mut entry = InstrumentLots {
            unpriced: qty(5),
            ..Default::default()
        };
        entry.push_lot(lot_with_acquired_date(
            instrument,
            Some(date!(2024 - 03 - 01)),
        ));

        assert_eq!(entry.earliest_acquired(), None);
    }

    #[test]
    fn ownership_boundary_does_not_rise_after_early_lot_disposal() {
        // Bought in January, bought in April, sold the January lot.
        // The boundary must remain in January: the security was held
        // in March, and the missing March coupon must be identified.
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let january = Trade {
            account,
            instrument,
            day: date!(2026 - 01 - 10),
            units: 10,
            gross: 100_000,
        };
        let april = Trade {
            day: date!(2026 - 04 - 10),
            ..january
        };
        let sale = Trade {
            day: date!(2026 - 07 - 10),
            gross: 120_000,
            ..january
        };
        book.apply(&dated_buy(&january, 1), &rules).unwrap();
        book.apply(&dated_buy(&april, 2), &rules).unwrap();
        book.apply(&sell(&sale, 3), &rules).unwrap();

        let entry = book.entry(&key(&january)).unwrap();
        assert_eq!(entry.lots().len(), 1, "the January lot must be disposed of");
        assert_eq!(
            entry.earliest_acquired(),
            Some(TradeDate(date!(2026 - 01 - 10)))
        );
    }

    #[test]
    fn disposing_lot_without_date_does_not_make_ownership_boundary_known() {
        // An undated lot was acquired at an unknown time, and its sale
        // does not clarify this. Treating April as the boundary would mean
        // declaring known what the journal does not state (§4.9).
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let without_date = Trade {
            account,
            instrument,
            day: date!(2026 - 01 - 10),
            units: 10,
            gross: 100_000,
        };
        let april = Trade {
            day: date!(2026 - 04 - 10),
            ..without_date
        };
        let sale = Trade {
            day: date!(2026 - 07 - 10),
            gross: 120_000,
            ..without_date
        };
        book.apply(&buy(&without_date, 1), &rules).unwrap();
        book.apply(&dated_buy(&april, 2), &rules).unwrap();
        book.apply(&sell(&sale, 3), &rules).unwrap();

        let entry = book.entry(&key(&without_date)).unwrap();
        assert_eq!(entry.lots().len(), 1);
        assert_eq!(entry.earliest_acquired(), None);
    }

    #[test]
    fn amortisation_leaves_the_quantity_alone() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(10));
    }

    #[test]
    fn an_amortisation_for_a_different_quantity_is_an_error_not_a_scaling() {
        // Amortisation applies to all securities on the account. A mismatch is a source
        // defect, not a reason to reduce face value proportionally.
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);

        assert!(matches!(
            book.apply(&bond.amortisation(4, "200", 80_000), &rules),
            Err(LotError::QuantityMismatch { .. })
        ));
    }

    #[test]
    fn an_amortisation_returning_exactly_the_basis_realises_nothing() {
        // §6.5: a return of capital is not income.
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.realized(), Some(rub(0)));
        assert_eq!(entry.released_basis(), Some(rub(200_000)));
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(800_000)));
        assert_eq!(
            entry.lots()[0].acquisition_basis,
            Some(rub(1_000_000)),
            "amortisation does not reduce historical cost"
        );
    }

    #[test]
    fn amortisation_reduces_current_basis_but_preserves_historical_purchase_cost() {
        // The lifetime cash flow includes 200 already received and 800 to come.
        // If the denominator is taken from the reduced cost_basis (800 instead of
        // the historical 1000), HPR would incorrectly be 25 % instead of 0 %.
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let lot = &book.entry(&bond.key()).unwrap().lots()[0];
        assert_eq!(lot.cost_basis, rub(800_000));
        assert_eq!(lot.acquisition_basis, Some(rub(1_000_000)));
    }

    #[test]
    fn an_amortisation_paying_above_the_returned_basis_realises_the_difference() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        // Purchased at a discount: cost 900 000 at a face value of 1000 × 10.
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 900_000)]);

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        // One fifth of the cost was returned — 180 000; 200 000 was paid.
        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.released_basis(), Some(rub(180_000)));
        assert_eq!(entry.realized(), Some(rub(20_000)));
    }

    #[test]
    fn an_unknown_allocation_records_a_basis_gap_instead_of_failing() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);

        book.apply(&bond.unknown_amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.gap(), Some(BasisGap::AmortisationAllocationUnknown));
        assert_eq!(entry.realized(), None);
        // Quantity and cost are unchanged: there was no basis for calculation.
        assert_eq!(entry.quantity().unwrap(), qty(10));
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(1_000_000)));
    }

    #[test]
    fn an_amortisation_on_another_account_leaves_this_book_alone() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);
        let elsewhere = Bond {
            account: AccountId::new_random(),
            instrument: bond.instrument,
            custody: bond.custody,
        };

        // There is no position in that account at all: the fact does not apply to this book.
        assert!(matches!(
            book.apply(&elsewhere.amortisation(10, "200", 200_000), &rules),
            Err(LotError::SaleWithoutPosition { .. })
        ));
        assert_eq!(
            book.entry(&bond.key()).unwrap().quantity().unwrap(),
            qty(10)
        );
    }

    #[test]
    fn a_redemption_empties_the_position_and_releases_the_whole_basis() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 800_000)]);
        let redemption = event_with(
            bond.account,
            date!(2026 - 12 - 15),
            6,
            EventKind::CorporateAction {
                action: CorporateAction::Redemption {
                    instrument: bond.instrument,
                    custody: bond.custody,
                    quantity: qty(10),
                    principal_returned_per_unit: per_unit("800"),
                    compensation: rub(800_000),
                    effective_date: date!(2026 - 12 - 15),
                    record_date: None,
                    grounds: None,
                },
            },
            vec![
                Leg::principal(bond.account, bond.instrument, rub(800_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ],
        );

        book.apply(&redemption, &rules).unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(0));
        assert_eq!(entry.released_basis(), Some(rub(800_000)));
        assert_eq!(entry.realized(), Some(rub(0)));
    }

    #[test]
    fn a_submitted_offer_leaves_the_lots_alone() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);
        let before = book.clone();
        let submitted = event_with(
            bond.account,
            date!(2026 - 07 - 01),
            7,
            EventKind::OfferExercise {
                action: crate::event::offer::OfferExerciseAction::Submitted {
                    submission: crate::event::offer::OfferSubmissionId::new_random(),
                    window: crate::event::offer::OfferWindowId::new_random(),
                    instrument: bond.instrument,
                    quantity: qty(10),
                },
            },
            Vec::new(),
        );

        book.apply(&submitted, &rules).unwrap();
        assert_eq!(book, before);
    }

    #[test]
    fn a_settled_offer_disposes_the_bought_back_quantity() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000)]);
        let settled = event_with(
            bond.account,
            date!(2026 - 07 - 15),
            8,
            EventKind::OfferExercise {
                action: crate::event::offer::OfferExerciseAction::Settled {
                    submission: crate::event::offer::OfferSubmissionId::new_random(),
                    instrument: bond.instrument,
                    custody: bond.custody,
                    quantity: qty(4),
                    gross: rub(420_000),
                    fee: None,
                    accrued_interest: None,
                },
            },
            vec![
                Leg::cash(bond.account, rub(420_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-4)),
            ],
        );

        book.apply(&settled, &rules).unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(6));
        assert_eq!(entry.released_basis(), Some(rub(400_000)));
        assert_eq!(entry.realized(), Some(rub(20_000)));
    }

    struct Trade {
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        units: i64,
        gross: i64,
    }

    fn buy(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(-(trade.gross + 10_000));
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(trade.units),
                ),
            ],
        )
    }

    fn dated_buy(trade: &Trade, sequence: u32) -> Event {
        let mut event = buy(trade, sequence);
        event.dates.trade = Some(TradeDate(trade.day));
        event
    }

    fn sell(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(trade.gross - 10_000);
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(-trade.units),
                ),
            ],
        )
    }

    fn key(trade: &Trade) -> LotKey {
        LotKey {
            account: trade.account,
            instrument: trade.instrument,
        }
    }

    fn sample_trade() -> Trade {
        Trade {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
            day: date!(2025 - 03 - 01),
            units: 100,
            gross: 1_000_000,
        }
    }

    #[test]
    fn a_purchase_creates_a_lot_including_the_fee() {
        // Commission is included in acquisition cost; accrued coupon interest is not (§7.2).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots().len(), 1);
        assert_eq!(entry.lots()[0].cost_basis, rub(1_010_000));
        assert_eq!(entry.lots()[0].acquisition_basis, Some(rub(1_010_000)));
        assert_eq!(entry.quantity().unwrap(), qty(100));
    }

    #[test]
    fn lot_identity_comes_from_the_acquisition_event_not_from_randomness() {
        // The core is pure: reprojection of the same journal must produce
        // the same lot identifiers, otherwise snapshots are not comparable (§3.1).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let event = buy(&trade, 1);
        let mut first = LotBook::new(LotRuleVersion(1));
        let mut second = LotBook::new(LotRuleVersion(1));
        first.apply(&event, &rules).unwrap();
        second.apply(&event, &rules).unwrap();
        assert_eq!(
            first.entry(&key(&trade)).unwrap().lots()[0].id,
            second.entry(&key(&trade)).unwrap().lots()[0].id
        );
    }

    #[test]
    fn a_partial_sale_releases_basis_and_records_realized_result() {
        // Purchased 100 for 1 010 000, sold 40 for 500 000 minus commission.
        // Disposed cost: 1 010 000 * 40 / 100 = 404 000.
        // Realised: 490 000 − 404 000 = 86 000.
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let partial = Trade {
            units: 40,
            gross: 500_000,
            day: date!(2025 - 06 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(60));
        assert_eq!(entry.released_basis(), Some(rub(404_000)));
        assert_eq!(entry.realized(), Some(rub(86_000)));
        assert_eq!(
            book.applied_rule().map(|r| r.0.as_str()),
            Some("fifo/214.1/v1")
        );
    }

    #[test]
    fn remaining_basis_completes_the_identity_acquired_equals_remaining_plus_released() {
        // The monetary part of the §6.3 identity. Expected values were calculated manually:
        // purchased 100 for 1 010 000, sold 40 — disposed cost 404 000,
        // so 606 000 remains on the 60 unsold securities.
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        assert_eq!(
            book.entry(&key(&trade)).and_then(|entry| entry
                .remaining_basis()
                .expect("an empty book does not calculate cost")),
            None
        );

        book.apply(&buy(&trade, 1), &rules).unwrap();
        let partial = Trade {
            units: 40,
            gross: 500_000,
            day: date!(2025 - 06 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(606_000)));
        assert_eq!(
            entry.acquired_basis(),
            Some(rub(1_010_000)),
            "acquired = remaining + disposed"
        );
        assert_eq!(entry.released_basis(), Some(rub(404_000)));
    }

    #[test]
    fn a_restored_position_without_basis_does_not_become_a_zero_cost_lot() {
        // Zero cost would imply profit equal to the entire proceeds (§4.9).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let mut restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions {
                    acquisition_date: Some(date!(2024 - 01 - 02)),
                    acquisition_date_certainty: crate::event::kind::DateCertainty::Known,
                    ..crate::event::kind::OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        // The exact settlement date makes it possible to verify that the reconstructed
        // quantity participates in ownership in the same way as a documented lot.
        restored.dates.settled = Some(crate::dates::SettledDate(date!(2024 - 01 - 02)));
        book.apply(&restored, &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert!(entry.lots().is_empty());
        assert_eq!(entry.unpriced(), qty(50));
        assert_eq!(entry.gap(), Some(BasisGap::RestoredWithoutBasis));
        assert_eq!(entry.ownership_at(date!(2024 - 01 - 03)), Ownership::Owned);

        // Selling from the reconstructed quantity reduces the position,
        // but the realised result remains uncomputable.
        let partial = Trade {
            units: 20,
            gross: 300_000,
            day: date!(2025 - 02 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.unpriced(), qty(30));
        assert_eq!(entry.realized(), None);
    }
    #[test]
    fn a_known_opening_acquisition_date_proves_ownership_after_the_claimed_date() {
        use crate::event::kind::{DateCertainty, OpeningAssertions};

        let trade = sample_trade();
        let claimed = date!(2021 - 05 - 01);
        let mut restored = event_with(
            trade.account,
            date!(2026 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: Some(rub(500_000)),
                assertions: OpeningAssertions {
                    acquisition_date: Some(claimed),
                    acquisition_date_certainty: DateCertainty::Known,
                    ..OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        // The event date is the import date; it does not prove the position's origin.
        restored.dates.trade = Some(crate::dates::TradeDate(date!(2026 - 01 - 01)));

        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&restored, &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.ownership_at(date!(2022 - 06 - 15)), Ownership::Owned);
        assert_eq!(
            entry.lots()[0].acquired,
            Some(crate::dates::TradeDate(claimed))
        );
    }

    #[test]
    fn an_estimated_opening_acquisition_date_does_not_prove_ownership() {
        use crate::event::kind::{DateCertainty, OpeningAssertions};

        let trade = sample_trade();
        let mut restored = event_with(
            trade.account,
            date!(2026 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: Some(rub(500_000)),
                assertions: OpeningAssertions {
                    acquisition_date: Some(date!(2021 - 05 - 01)),
                    acquisition_date_certainty: DateCertainty::Estimated,
                    ..OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        // Even a plausible import date does not replace proof of the start.
        restored.dates.trade = Some(crate::dates::TradeDate(date!(2021 - 05 - 01)));

        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&restored, &rules).unwrap();

        assert_eq!(
            book.entry(&key(&trade))
                .unwrap()
                .ownership_at(date!(2022 - 06 - 15)),
            Ownership::Unknown
        );
    }

    #[test]
    fn selling_an_instrument_never_held_is_an_error() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        assert!(matches!(
            book.apply(&sell(&trade, 1), &rules),
            Err(LotError::SaleWithoutPosition { .. })
        ));
    }

    #[test]
    fn an_unknown_rule_version_is_an_error_not_a_silent_fallback() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(99));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        assert!(matches!(
            book.apply(&sell(&trade, 2), &rules),
            Err(LotError::UnknownRule { .. })
        ));
    }
    #[test]
    fn the_book_exposes_its_entries_and_the_cost_of_acquisitions() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();

        assert_eq!(book.iter().count(), 1, "the book must return entries");
        let (found_key, entry) = book.iter().next().unwrap();
        assert_eq!(*found_key, key(&trade));
        // Acquired = principal + commission; together with the disposed amount it forms
        // a verifiable cost-conservation identity.
        assert_eq!(entry.acquired_basis(), Some(rub(1_010_000)));
        assert_eq!(entry.released_basis(), None);
        assert_eq!(book.rule_version(), LotRuleVersion(1));
    }

    #[test]
    fn the_basis_gap_has_a_machine_readable_code() {
        // The code is exposed through the API: the agent parses it, not the text.
        assert_eq!(
            BasisGap::RestoredWithoutBasis.code(),
            "restored_without_basis"
        );
    }
    fn set_accrued_interest(event: &mut Event, value: Option<Money>) {
        match &mut event.kind {
            EventKind::Trade {
                accrued_interest, ..
            } => *accrued_interest = value,
            _ => panic!("expected trade event"),
        }
    }

    #[test]
    fn a_bond_purchase_records_paid_accrued_interest_and_preserves_unknown() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut with_interest = buy(&trade, 1);
        set_accrued_interest(&mut with_interest, Some(rub(12_345)));
        let mut with = LotBook::new(LotRuleVersion(1));
        with.apply(&with_interest, &rules).unwrap();
        assert_eq!(
            with.entry(&key(&trade)).unwrap().lots()[0].accrued_interest_paid,
            Some(rub(12_345))
        );

        let mut without = LotBook::new(LotRuleVersion(1));
        without.apply(&buy(&trade, 1), &rules).unwrap();
        assert_eq!(
            without.entry(&key(&trade)).unwrap().lots()[0].accrued_interest_paid,
            None
        );
    }

    #[test]
    fn an_opening_position_never_invents_paid_accrued_interest() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: Some(rub(500_000)),
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );

        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&restored, &rules).unwrap();
        assert_eq!(
            book.entry(&key(&trade)).unwrap().lots()[0].accrued_interest_paid,
            None
        );
    }
    #[test]
    fn a_coupon_is_distributed_between_lots_by_quantity() {
        let trade = sample_trade();
        let first = Trade {
            day: date!(2025 - 03 - 01),
            units: 30,
            gross: 300_000,
            ..trade
        };
        let second = Trade {
            day: date!(2025 - 04 - 01),
            units: 70,
            gross: 700_000,
            ..trade
        };
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&first, 1), &rules).unwrap();
        book.apply(&buy(&second, 2), &rules).unwrap();
        let coupon = event_with(
            trade.account,
            date!(2025 - 05 - 01),
            3,
            EventKind::Income {
                instrument: Some(trade.instrument),
                gross: rub(101),
                kind: Some(crate::event::kind::IncomeKind::Coupon),
            },
            vec![Leg::cash(trade.account, rub(101))],
        );

        book.apply(&coupon, &rules).unwrap();

        let lots = &book.entry(&key(&trade)).unwrap().lots;
        assert_eq!(lots[0].received_to_date, Some(rub(30)));
        assert_eq!(lots[1].received_to_date, Some(rub(71)));
        assert_eq!(
            lots.iter()
                .filter_map(|lot| lot.received_to_date)
                .try_fold(rub(0), |sum, amount| sum.try_add(amount))
                .unwrap(),
            rub(101)
        );
    }

    #[test]
    fn a_coupon_before_a_second_purchase_stays_with_the_first_lot() {
        let trade = sample_trade();
        let first = Trade { units: 30, ..trade };
        let second = Trade {
            day: date!(2025 - 04 - 01),
            units: 70,
            gross: 700_000,
            ..trade
        };
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&first, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 03 - 15),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();
        book.apply(&buy(&second, 3), &rules).unwrap();

        let lots = book.entry(&key(&trade)).unwrap().lots();
        assert_eq!(lots[0].received_to_date, Some(rub(100)));
        assert_eq!(lots[1].received_to_date, None);
    }

    #[test]
    fn amortisation_records_the_returned_cash_on_each_lot() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 30, 300_000), bond_lot(&bond, 70, 700_000)],
        );

        book.apply(&bond.amortisation(100, "1", 101), &rules)
            .unwrap();

        let lots = book.entry(&bond.key()).unwrap().lots();
        assert_eq!(lots[0].received_to_date, Some(rub(30)));
        assert_eq!(lots[1].received_to_date, Some(rub(71)));
    }

    #[test]
    fn received_cash_is_split_when_a_lot_is_partially_sold() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();
        let partial = Trade {
            units: 40,
            day: date!(2025 - 06 - 01),
            gross: 500_000,
            ..trade
        };
        book.apply(&sell(&partial, 3), &rules).unwrap();

        assert_eq!(
            book.entry(&key(&trade)).unwrap().lots()[0].received_to_date,
            Some(rub(60))
        );
    }

    #[test]
    fn an_unpriced_quantity_receives_its_share_of_a_coupon_as_unallocated() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        book.apply(&restored, &rules).unwrap();
        let priced = Trade {
            units: 50,
            gross: 500_000,
            ..trade
        };
        book.apply(&buy(&priced, 2), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                3,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots()[0].received_to_date, Some(rub(50)));
        assert_eq!(entry.unpriced_income(), Some(rub(50)));
        assert_eq!(entry.cohorts(), Err(CohortGap::AcquisitionDateUnknown));
    }

    #[test]
    fn an_income_without_an_approved_kind_refuses_cohorts() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: None,
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots()[0].received_to_date, None);
        assert!(entry.income_kind_unknown());
        assert_eq!(entry.cohorts(), Err(CohortGap::IncomeKindUnknown));
    }

    #[test]
    fn a_coupon_for_an_unknown_position_is_retained_as_a_marker() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                1,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert!(entry.lots().is_empty());
        assert_eq!(entry.unallocated_income(), Some(rub(100)));
        assert!(entry.cohorts().unwrap().is_empty());
    }

    #[test]
    fn cohorts_report_restored_without_basis_after_unpriced_quantity_is_sold() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        book.apply(&restored, &rules).unwrap();
        let sale = Trade {
            units: 50,
            day: date!(2025 - 02 - 01),
            gross: 500_000,
            ..trade
        };
        book.apply(&sell(&sale, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.unpriced(), qty(0));
        assert_eq!(entry.cohorts(), Err(CohortGap::RestoredWithoutBasis));
    }

    #[test]
    fn cohorts_group_same_acquisition_dates_and_keep_different_dates_separate() {
        let trade = sample_trade();
        let same_day_a = Trade {
            units: 30,
            gross: 300_000,
            ..trade
        };
        let same_day_b = Trade {
            units: 20,
            gross: 200_000,
            ..trade
        };
        let later = Trade {
            day: date!(2025 - 04 - 01),
            units: 50,
            gross: 500_000,
            ..trade
        };
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&dated_buy(&same_day_a, 1), &rules).unwrap();
        book.apply(&dated_buy(&same_day_b, 2), &rules).unwrap();
        book.apply(&dated_buy(&later, 3), &rules).unwrap();

        let cohorts = book.entry(&key(&trade)).unwrap().cohorts().unwrap();
        assert_eq!(cohorts.len(), 2);
        assert_eq!(cohorts[0].acquired, TradeDate(date!(2025 - 03 - 01)));
        assert_eq!(cohorts[0].quantity, qty(50));
        assert_eq!(cohorts[0].cost_basis, rub(520_000));
        assert_eq!(cohorts[0].acquisition_basis, Some(rub(520_000)));
        assert_eq!(cohorts[1].acquired, TradeDate(date!(2025 - 04 - 01)));
        assert_eq!(cohorts[1].quantity, qty(50));
        assert_eq!(cohorts[1].cost_basis, rub(510_000));
        assert_eq!(cohorts[1].acquisition_basis, Some(rub(510_000)));
    }

    #[test]
    fn a_dividend_does_not_block_cohort_construction() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&dated_buy(&trade, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Dividend),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert!(!entry.income_kind_unknown());
        assert_eq!(entry.cohorts().unwrap().len(), 1);
    }

    #[test]
    fn cohorts_report_mixed_cost_basis_currencies() {
        let bond = Bond::new();
        let mut foreign = bond_lot(&bond, 10, 1_000_000);
        foreign.cost_basis = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Usd);
        let book = book_with_lots(&bond, vec![bond_lot(&bond, 10, 1_000_000), foreign]);

        assert_eq!(
            book.entry(&bond.key()).unwrap().cohorts(),
            Err(CohortGap::InconsistentCostBasisCurrency)
        );
    }
    #[test]
    fn cohorts_refuse_missing_acquisition_date_and_cost_basis_overflow() {
        let bond = Bond::new();
        let mut missing_date = bond_lot(&bond, 1, 1_000);
        missing_date.acquired = None;
        assert_eq!(
            book_with_lots(&bond, vec![missing_date])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::AcquisitionDateUnknown)
        );

        let mut first = bond_lot(&bond, 1, i64::MAX);
        let mut second = bond_lot(&bond, 1, i64::MAX);
        first.acquisition_basis = None;
        second.acquisition_basis = None;
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentCostBasisOverflow)
        );
    }

    #[test]
    fn cohorts_refuse_quantity_and_optional_money_overflow_or_currency_mismatch() {
        let bond = Bond::new();
        let mut first = bond_lot(&bond, 1, 1_000);
        let mut second = bond_lot(&bond, 1, 1_000);
        first.quantity = Quantity(Dec::new(Decimal::MAX));
        second.quantity = Quantity(Dec::new(Decimal::MAX));
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentQuantity)
        );

        let mut first = bond_lot(&bond, 1, 1_000);
        let mut second = bond_lot(&bond, 1, 1_000);
        first.accrued_interest_paid = Some(rub(1));
        second.accrued_interest_paid = Some(Money::new(PostedMinor::new(1), CurrencyCode::Usd));
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentOptionalMoneyCurrency)
        );

        let mut first = bond_lot(&bond, 1, 1_000);
        let mut second = bond_lot(&bond, 1, 1_000);
        first.accrued_interest_paid = Some(rub(i64::MAX));
        second.accrued_interest_paid = Some(rub(i64::MAX));
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentOptionalMoneyOverflow)
        );
    }
}
