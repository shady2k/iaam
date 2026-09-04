//! The flow of money across and inside the contour (spec §2).
//!
//! This module answers a household question — what came in, what went out,
//! what the capital earned, what moved into assets — and it deliberately does
//! **not** reclassify any event to do so. `flows.rs` answers a different
//! question, about contributions and withdrawals for the returns path, and
//! `EventKind::Income` is `WithinAccount` there for a correct reason: a coupon
//! is not a new contribution of capital. Moving it would corrupt XIRR.
//!
//! So the two projections read the same journal from two angles. The quantity
//! that makes this honest is `residual`: the difference between the cash the
//! contour actually gained and the six quantities that claim to explain it. A
//! non-zero residual is reported, never absorbed.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::category::CategoryAssignment;
use crate::contour::{ContourDefinition, FlowClass, classify};
use crate::event::Event;
use crate::event::kind::EventKind;
use crate::event::leg::LegKind;
use crate::ids::CategoryId;
use crate::ids::{AccountId, EventId, InstrumentId};
use crate::money::{CurrencyCode, Money, PostedMinor};

/// The interval a report covers, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateWindow {
    pub from: Date,
    pub to: Date,
}

impl DateWindow {
    /// Inclusive at both ends: a report for August includes the 1st and the 31st.
    #[must_use]
    pub fn covers(&self, on: Date) -> bool {
        self.from <= on && on <= self.to
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MoneyFlowError {
    #[error("event {event:?} moves money inside the window but has no date")]
    MovementWithoutDate { event: EventId },
    #[error("overflow while summing {quantity} for event {event:?}")]
    Overflow {
        quantity: &'static str,
        event: EventId,
    },
    #[error("overflow while aggregating {quantity}")]
    AggregateOverflow { quantity: &'static str },
}

/// The explanatory quantities and the cash they claim to explain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyFlow {
    came_in: Ledger,
    went_out: Ledger,
    earned_by_capital: Ledger,
    moved_into_assets: Ledger,
    fees: Ledger,
    taxes: Ledger,
    internal_transfers: Ledger,
    /// Cash that moved on a contour account towards or from an account of the
    /// owner's that the source did not name.
    ///
    /// The ninth quantity, and it is inside the identity: the money really did
    /// move, `cash_delta` has it, and something must explain it or every
    /// account carrying one of these rows shows a residual with no reason
    /// beside it. What it does not do is claim a direction across the boundary
    /// — it is neither `came_in`/`went_out` nor `internal_transfers`, because
    /// which of those it is depends on where the far side sits and nobody said.
    ///
    /// Signed, like `internal_transfers`, so each account's identity closes.
    indeterminate: Ledger,
    /// The magnitude of movements the source stated and left without a
    /// direction.
    ///
    /// **Outside the identity, and that is the point.** No cash moved as far as
    /// this journal is concerned — an unresolved own-account movement posts no
    /// leg — so folding it into the identity would make the identity fail by
    /// exactly the amount the journal declined to invent. It is reported beside
    /// the identity instead: the source says money moved here, the journal
    /// cannot say which way, and a reader comparing this report with a
    /// statement is entitled to see the difference named rather than discover
    /// it as a gap.
    unstated: Ledger,
    cash_delta: Ledger,
    went_out_by_category: CategoryLedger,
    earned_by_capital_by_source: EarningLedger,
    /// Per-account count and amount are retained so diagnostics can name the account,
    /// and per-cause so they can name a remedy only where one exists. Older serialized
    /// `MoneyFlow` values with the currency-only or causeless shape are not promised to
    /// deserialize.
    not_decomposed: (UndecomposedCounts, UndecomposedLedger),
}

/// Why an amount is in `not_decomposed`, which decides whether anything can fix it.
///
/// The two cases were one aggregate and are materially different. A spending row
/// nothing matched is waiting for a category rule the owner has not written. A
/// transfer out of the contour is not waiting for anything: the projection never
/// consults the category index for `CashTransfer`, so no rule the owner could write
/// would ever reach it. Reporting one remedy for both told the owner a falsehood
/// about every transfer-only account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UndecomposedCause {
    /// A `CashOut` or `Refund` row no category rule matched.
    NoRuleMatched,
    /// A `CashTransfer` that left the contour. Category assignment is never
    /// consulted for a transfer, so no rule applies to this amount.
    ExternalTransfer,
}

/// What produced an earning.
///
/// The account answers "which deposit or which card", the instrument "which
/// security", and the category "what sort of income" — cashback, interest, a
/// coupon. Three axes rather than one, because no single label answers "which
/// asset brought what" for both a savings account and a bond: for cash-like
/// assets the account **is** the asset, and for securities the instrument is.
///
/// The sort is a **category**, not an enum in the code. Cashback and interest
/// on a balance are the owner's vocabulary, they change over the years, and the
/// design already decided that a category is derived from versioned rules
/// rather than written onto an event. Putting them in `IncomeKind` would freeze
/// the owner's list into the schema and make renaming one a journal migration.
/// `IncomeKind` stays what it is for: whether a payment is a bond coupon, which
/// the schedule reconciliation needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EarningSource {
    pub account: AccountId,
    pub instrument: Option<InstrumentId>,
    /// `None` where no rule covers the row. Not a bucket: an undecomposed
    /// earning is shown as its own line, exactly as an undecomposed outflow is.
    pub category: Option<CategoryId>,
}

type EarningLedger = BTreeMap<(EarningSource, CurrencyCode), PostedMinor>;

/// Amounts kept per account **and** per currency.
///
/// Per currency, because currencies are never silently added. Per account,
/// because §2 requires the residual to name the account it belongs to: a
/// contour-wide zero built from one account short and another long is the
/// worst possible report — it looks correct and is wrong twice.
type Ledger = BTreeMap<(AccountId, CurrencyCode), PostedMinor>;

/// The undecomposed amounts and row counts, split by what put them there.
type UndecomposedLedger = BTreeMap<(AccountId, CurrencyCode, UndecomposedCause), PostedMinor>;
type UndecomposedCounts = BTreeMap<(AccountId, CurrencyCode, UndecomposedCause), u64>;

type CategoryLedger = BTreeMap<(CategoryId, CurrencyCode), PostedMinor>;

/// Resolves the owner's category for one journal event without coupling the
/// projection to the category store.
pub trait CategoryIndex {
    fn assignment(&self, event: &Event) -> CategoryAssignment;
}

/// An index that assigns nothing.
///
/// Not a stopgap: this is the honest state of a contour whose owner has written
/// no category rules yet. Every outflow lands in `not_decomposed`, and the
/// report says so rather than inventing a bucket.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCategories;

impl CategoryIndex for NoCategories {
    fn assignment(&self, _event: &Event) -> CategoryAssignment {
        CategoryAssignment::NotDecomposed
    }
}

impl MoneyFlow {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one event.
    ///
    /// `cash_delta` is accumulated from the legs, the same way `Balances` does
    /// it, so the two can never drift apart. The explanatory quantities are
    /// accumulated from the event kind and the leg kind. Their disagreement is
    /// what `residual` reports.
    ///
    /// This operation is not atomic: on `Err`, the projection may be partially
    /// updated and must be discarded rather than read.
    pub fn apply(
        &mut self,
        event: &Event,
        contour: &ContourDefinition,
        window: DateWindow,
        categories: &dyn CategoryIndex,
    ) -> Result<(), MoneyFlowError> {
        let flow_class = classify(contour, event);
        if !self.event_belongs(event, contour, window, flow_class)? {
            return Ok(());
        }
        // Every event whose money the report decomposes needs its category, not
        // just spending: a refund is subtracted from the category it was spent
        // in, and an earning is reported under the owner's own income category
        // — cashback, interest on a balance — exactly as an outflow is reported
        // under his spending one. Asking only for CashOut left both silently
        // uncategorised while the rules that covered them existed and matched.
        let category_assignment = if matches!(
            &event.kind,
            EventKind::CashOut { .. } | EventKind::Refund { .. } | EventKind::Income { .. }
        ) {
            Some(categories.assignment(event))
        } else {
            None
        };
        let mut not_decomposed_keys = BTreeSet::new();

        // Read from the kind and not from the legs, because there are none.
        // Every other quantity in this projection is accumulated inside the
        // loop below, and this one cannot be: an event with nothing posted is
        // invisible to a fold over legs, which is precisely why it needs
        // saying.
        if let EventKind::UnresolvedOwnAccountMovement { amount } = &event.kind {
            add(
                &mut self.unstated,
                event.account,
                *amount,
                "unstated",
                event.id,
            )?;
        }

        for leg in &event.legs {
            if !contour.contains(leg.account) {
                continue;
            }
            let Some(money) = leg.cash_effect() else {
                continue;
            };
            add(
                &mut self.cash_delta,
                leg.account,
                money,
                "cash_delta",
                event.id,
            )?;

            match leg.kind {
                LegKind::Fee => {
                    let amount = negated(money, "fees", event.id)?;
                    add(&mut self.fees, leg.account, amount, "fees", event.id)?;
                }
                LegKind::Tax => {
                    let amount = negated(money, "taxes", event.id)?;
                    add(&mut self.taxes, leg.account, amount, "taxes", event.id)?;
                }
                LegKind::Cash => match &event.kind {
                    EventKind::Trade { .. } => {
                        let amount = negated(money, "moved_into_assets", event.id)?;
                        add(
                            &mut self.moved_into_assets,
                            leg.account,
                            amount,
                            "moved_into_assets",
                            event.id,
                        )?;
                    }
                    EventKind::Income { instrument, .. } => {
                        add(
                            &mut self.earned_by_capital,
                            leg.account,
                            money,
                            "earned_by_capital",
                            event.id,
                        )?;
                        // The same amount, kept a second time against what
                        // produced it. Without this the report can say how much
                        // the capital earned and not which part of it earned
                        // anything, which is the question a household asks next.
                        let source = EarningSource {
                            account: leg.account,
                            instrument: *instrument,
                            category: match category_assignment {
                                Some(CategoryAssignment::Assigned { category, .. }) => {
                                    Some(category)
                                }
                                Some(CategoryAssignment::NotDecomposed) | None => None,
                            },
                        };
                        let slot = self
                            .earned_by_capital_by_source
                            .entry((source, money.currency()))
                            .or_insert_with(|| PostedMinor::new(0));
                        *slot =
                            slot.checked_add(money.amount())
                                .ok_or(MoneyFlowError::Overflow {
                                    quantity: "earned_by_capital_by_source",
                                    event: event.id,
                                })?;
                    }
                    EventKind::CashIn { .. } => {
                        add(&mut self.came_in, leg.account, money, "came_in", event.id)?;
                    }
                    // A refund reverses spending; it is not income. Its cash leg
                    // is positive, so it is subtracted from what went out and
                    // from the category the money was spent in — a month where
                    // a purchase is returned shows neither the purchase nor an
                    // earning. Adding it to `came_in` instead would report
                    // money arriving that nobody sent.
                    EventKind::Refund { .. } => {
                        let amount = negated(money, "went_out", event.id)?;
                        add(
                            &mut self.went_out,
                            leg.account,
                            amount,
                            "went_out",
                            event.id,
                        )?;
                        match category_assignment {
                            Some(CategoryAssignment::Assigned { category, .. }) => {
                                add_category(
                                    &mut self.went_out_by_category,
                                    category,
                                    amount,
                                    event.id,
                                )?;
                            }
                            Some(CategoryAssignment::NotDecomposed) | None => {
                                add_not_decomposed(
                                    &mut self.not_decomposed,
                                    &mut not_decomposed_keys,
                                    leg.account,
                                    amount,
                                    UndecomposedCause::NoRuleMatched,
                                    event.id,
                                )?;
                            }
                        }
                    }
                    EventKind::CashOut { .. } => {
                        let amount = negated(money, "went_out", event.id)?;
                        add(
                            &mut self.went_out,
                            leg.account,
                            amount,
                            "went_out",
                            event.id,
                        )?;
                        match category_assignment {
                            Some(CategoryAssignment::Assigned { category, .. }) => {
                                add_category(
                                    &mut self.went_out_by_category,
                                    category,
                                    amount,
                                    event.id,
                                )?;
                            }
                            Some(CategoryAssignment::NotDecomposed) | None => {
                                add_not_decomposed(
                                    &mut self.not_decomposed,
                                    &mut not_decomposed_keys,
                                    leg.account,
                                    amount,
                                    UndecomposedCause::NoRuleMatched,
                                    event.id,
                                )?;
                            }
                        }
                    }
                    // The far side is unnamed, so the report cannot say
                    // whether this left the contour. It goes to its own
                    // quantity rather than to `went_out` — which would call it
                    // spending and put it in a category — or to
                    // `internal_transfers`, which would call it a reallocation
                    // that changed nothing.
                    EventKind::OwnAccountMovement { .. } => {
                        add(
                            &mut self.indeterminate,
                            leg.account,
                            money,
                            "indeterminate",
                            event.id,
                        )?;
                    }
                    EventKind::UnresolvedOwnAccountMovement { .. } => {
                        // It has no legs, so this arm is unreachable through
                        // the loop. Named rather than joined to a catch-all so
                        // that a build which ever gave it one fails here
                        // instead of silently reporting nothing.
                    }
                    EventKind::CashTransfer { .. } => match flow_class {
                        FlowClass::Internal => {
                            add(
                                &mut self.internal_transfers,
                                leg.account,
                                money,
                                "internal_transfers",
                                event.id,
                            )?;
                        }
                        FlowClass::ExternalIn { .. } => {
                            add(&mut self.came_in, leg.account, money, "came_in", event.id)?;
                        }
                        FlowClass::ExternalOut { .. } => {
                            let amount = negated(money, "went_out", event.id)?;
                            add(
                                &mut self.went_out,
                                leg.account,
                                amount,
                                "went_out",
                                event.id,
                            )?;
                            add_not_decomposed(
                                &mut self.not_decomposed,
                                &mut not_decomposed_keys,
                                leg.account,
                                amount,
                                UndecomposedCause::ExternalTransfer,
                                event.id,
                            )?;
                        }
                        FlowClass::Indeterminate { .. } => {
                            // A `CashTransfer` names both accounts, so nothing
                            // classifies one as indeterminate.
                        }
                        FlowClass::Irrelevant => {
                            // Irrelevant transfers are rejected by event_belongs.
                        }
                    },
                    EventKind::Fee { .. } => {
                        // A fee's monetary leg is LegKind::Fee, not LegKind::Cash.
                    }
                    EventKind::Tax { .. } => {
                        // A tax's monetary leg is LegKind::Tax, not LegKind::Cash.
                    }
                    EventKind::OpeningPosition { .. } => {
                        // An opening position establishes state, not a cash flow.
                    }
                    EventKind::OpeningCash { .. } => {
                        // Opening cash is a starting point, not a flow.
                    }
                    EventKind::Valuation { .. } => {
                        // A valuation changes no cash.
                    }
                    EventKind::ControlAssertion { .. } => {
                        // A control assertion records evidence, not a cash flow.
                    }
                    EventKind::ImportCoverageGap { .. } => {
                        // A coverage gap records missing evidence, not a cash flow.
                    }
                    EventKind::CorporateAction { .. } | EventKind::OfferExercise { .. } => {
                        let amount = negated(money, "moved_into_assets", event.id)?;
                        add(
                            &mut self.moved_into_assets,
                            leg.account,
                            amount,
                            "moved_into_assets",
                            event.id,
                        )?;
                    }
                },
                LegKind::SecurityQuantity => {
                    // Security quantities have no cash effect.
                }
                LegKind::Principal => match &event.kind {
                    EventKind::CorporateAction { .. } | EventKind::OfferExercise { .. } => {
                        let amount = negated(money, "moved_into_assets", event.id)?;
                        add(
                            &mut self.moved_into_assets,
                            leg.account,
                            amount,
                            "moved_into_assets",
                            event.id,
                        )?;
                    }
                    EventKind::Trade { .. }
                    | EventKind::CashIn { .. }
                    | EventKind::Refund { .. }
                    | EventKind::CashOut { .. }
                    | EventKind::CashTransfer { .. }
                    | EventKind::OwnAccountMovement { .. }
                    | EventKind::UnresolvedOwnAccountMovement { .. }
                    | EventKind::Income { .. }
                    | EventKind::Fee { .. }
                    | EventKind::Tax { .. }
                    | EventKind::OpeningPosition { .. }
                    | EventKind::OpeningCash { .. }
                    | EventKind::Valuation { .. }
                    | EventKind::ControlAssertion { .. }
                    | EventKind::ImportCoverageGap { .. } => {
                        // Principal cash belongs to asset events only.
                    }
                },
            }
        }
        Ok(())
    }

    fn event_belongs(
        &self,
        event: &Event,
        contour: &ContourDefinition,
        window: DateWindow,
        flow_class: FlowClass,
    ) -> Result<bool, MoneyFlowError> {
        if matches!(flow_class, FlowClass::Irrelevant) {
            return Ok(false);
        }
        let Some(on) = event.dates.effective_date() else {
            if has_contour_cash_leg(event, contour) {
                return Err(MoneyFlowError::MovementWithoutDate { event: event.id });
            }
            return Ok(false);
        };
        Ok(window.covers(on))
    }

    pub fn came_in(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.came_in, currency, "came_in")
    }

    pub fn went_out(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.went_out, currency, "went_out")
    }

    /// Returns the outflow grouped by the owner's category.
    pub fn went_out_by_category(
        &self,
        currency: CurrencyCode,
    ) -> Result<Vec<(CategoryId, Money)>, MoneyFlowError> {
        let mut totals = BTreeMap::<CategoryId, i128>::new();
        for ((category, item_currency), amount) in &self.went_out_by_category {
            if *item_currency != currency {
                continue;
            }
            let total = totals.entry(*category).or_default();
            *total = total.checked_add(i128::from(amount.raw())).ok_or(
                MoneyFlowError::AggregateOverflow {
                    quantity: "went_out_by_category",
                },
            )?;
        }
        totals
            .into_iter()
            .filter(|(_, amount)| *amount != 0)
            .map(|(category, amount)| {
                Ok((
                    category,
                    Money::new(narrow(amount, "went_out_by_category")?, currency),
                ))
            })
            .collect()
    }

    /// Returns the total number and amount of outflow rows without a category.
    pub fn not_decomposed(&self, currency: CurrencyCode) -> Result<(u64, Money), MoneyFlowError> {
        let count = self
            .not_decomposed
            .0
            .iter()
            .filter(|((_, item_currency, _), _)| *item_currency == currency)
            .try_fold(0_u64, |total, (_, count)| {
                total
                    .checked_add(*count)
                    .ok_or(MoneyFlowError::AggregateOverflow {
                        quantity: "not_decomposed_count",
                    })
            })?;
        let amount = self
            .not_decomposed
            .1
            .iter()
            .filter(|((_, item_currency, _), _)| *item_currency == currency)
            .map(|(_, amount)| i128::from(amount.raw()))
            .sum::<i128>();
        Ok((
            count,
            Money::new(narrow(amount, "not_decomposed")?, currency),
        ))
    }

    /// Returns undecomposed outflow rows grouped by account, over every cause.
    pub fn not_decomposed_by_account(
        &self,
        currency: CurrencyCode,
    ) -> Result<Vec<(AccountId, u64, Money)>, MoneyFlowError> {
        let mut rows = BTreeMap::<AccountId, (u64, i128)>::new();
        for (account, _, count, amount) in self.undecomposed_rows(currency) {
            let row = rows.entry(account).or_default();
            row.0 = row
                .0
                .checked_add(count)
                .ok_or(MoneyFlowError::AggregateOverflow {
                    quantity: "not_decomposed_count",
                })?;
            row.1 = row
                .1
                .checked_add(amount)
                .ok_or(MoneyFlowError::AggregateOverflow {
                    quantity: "not_decomposed",
                })?;
        }
        rows.into_iter()
            .map(|(account, (count, amount))| {
                Ok((
                    account,
                    count,
                    Money::new(narrow(amount, "not_decomposed")?, currency),
                ))
            })
            .collect()
    }

    /// The same rows, kept apart by what left them undecomposed.
    ///
    /// The split exists because only one of the two causes has a remedy: a row no
    /// rule matched is answered by the owner writing one, and a transfer out of the
    /// contour is answered by nothing this API offers. A caller that must tell the
    /// owner what to do needs the difference, and summing it away here would oblige
    /// every such caller to guess.
    pub fn not_decomposed_by_account_and_cause(
        &self,
        currency: CurrencyCode,
    ) -> Result<Vec<(AccountId, UndecomposedCause, u64, Money)>, MoneyFlowError> {
        self.undecomposed_rows(currency)
            .map(|(account, cause, count, amount)| {
                Ok((
                    account,
                    cause,
                    count,
                    Money::new(narrow(amount, "not_decomposed")?, currency),
                ))
            })
            .collect()
    }

    /// Every undecomposed key in one currency, with its count and its amount.
    ///
    /// The counts map is the authority on which keys exist: an amount is only ever
    /// written beside a count, and a key whose rows cancelled to zero is still a key
    /// the owner has rows under.
    fn undecomposed_rows(
        &self,
        currency: CurrencyCode,
    ) -> impl Iterator<Item = (AccountId, UndecomposedCause, u64, i128)> + '_ {
        self.not_decomposed
            .0
            .iter()
            .filter(move |((_, item_currency, _), _)| *item_currency == currency)
            .map(move |(key, count)| {
                let amount = self
                    .not_decomposed
                    .1
                    .get(key)
                    .map_or(0, |amount| i128::from(amount.raw()));
                (key.0, key.2, *count, amount)
            })
    }

    pub fn earned_by_capital(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.earned_by_capital, currency, "earned_by_capital")
    }

    /// What the capital earned, split by what produced it.
    ///
    /// Sums to [`Self::earned_by_capital`] for the same currency: the same
    /// amounts read along another axis, never a second set of figures.
    pub fn earned_by_capital_by_source(
        &self,
        currency: CurrencyCode,
    ) -> Result<Vec<(EarningSource, Money)>, MoneyFlowError> {
        let mut totals = BTreeMap::<EarningSource, i128>::new();
        for ((source, item_currency), amount) in &self.earned_by_capital_by_source {
            if *item_currency != currency {
                continue;
            }
            let total = totals.entry(*source).or_default();
            *total = total.checked_add(i128::from(amount.raw())).ok_or(
                MoneyFlowError::AggregateOverflow {
                    quantity: "earned_by_capital_by_source",
                },
            )?;
        }
        totals
            .into_iter()
            .filter(|(_, amount)| *amount != 0)
            .map(|(source, amount)| {
                Ok((
                    source,
                    Money::new(narrow(amount, "earned_by_capital_by_source")?, currency),
                ))
            })
            .collect()
    }

    pub fn moved_into_assets(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.moved_into_assets, currency, "moved_into_assets")
    }

    pub fn fees(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.fees, currency, "fees")
    }

    pub fn taxes(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.taxes, currency, "taxes")
    }

    /// Returns the amount moved, using only positive signed ledger entries.
    ///
    /// The ledger is signed so each account's identity closes; this accessor is
    /// one-sided for the reader and reports how much moved between accounts.
    pub fn internal_transfers(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total_positive(&self.internal_transfers, currency, "internal_transfers")
    }

    /// Cash that moved towards or from an account of the owner's the source did
    /// not name, signed as the contour accounts saw it.
    pub fn indeterminate(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.indeterminate, currency, "indeterminate")
    }

    /// The magnitude the source stated for movements it gave no direction.
    ///
    /// Positive, and it explains nothing: it is what the journal was told and
    /// declined to post. A report showing a non-zero figure here is a report
    /// whose account of the month is short by at least this much in one
    /// direction or the other.
    pub fn unstated(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.unstated, currency, "unstated")
    }

    pub fn cash_delta(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        total(&self.cash_delta, currency, "cash_delta")
    }

    /// The currencies present in any quantity or cash delta.
    pub fn currencies(&self) -> impl Iterator<Item = CurrencyCode> + '_ {
        let mut currencies = BTreeSet::new();
        for ledger in self.ledgers() {
            currencies.extend(ledger.keys().map(|(_, currency)| *currency));
        }
        currencies.into_iter()
    }

    /// The cash the explanatory quantities fail to explain, for one account.
    ///
    /// `unstated` is deliberately absent from the sum. It is not cash this
    /// journal holds — the fact it comes from posts nothing — so adding it here
    /// would open a residual on every account that carries one.
    #[must_use]
    fn residual_of(&self, account: AccountId, currency: CurrencyCode) -> i128 {
        let at = |ledger: &Ledger| {
            ledger
                .get(&(account, currency))
                .map_or(0, |amount| i128::from(amount.raw()))
        };
        let explained = at(&self.came_in) - at(&self.went_out) + at(&self.earned_by_capital)
            - at(&self.moved_into_assets)
            - at(&self.fees)
            - at(&self.taxes)
            + at(&self.internal_transfers)
            + at(&self.indeterminate);
        at(&self.cash_delta) - explained
    }

    /// The contour-wide residual in a currency.
    pub fn residual(&self, currency: CurrencyCode) -> Result<Money, MoneyFlowError> {
        let total = self
            .accounts()
            .into_iter()
            .map(|account| self.residual_of(account, currency))
            .sum::<i128>();
        Ok(Money::new(narrow(total, "residual")?, currency))
    }

    /// Every account that does not close, with what it owes.
    ///
    /// Reported separately from `residual` on purpose. Two accounts wrong in
    /// opposite directions sum to zero, and a report that showed only the total
    /// would call that success while being wrong twice.
    pub fn residuals_by_account(&self) -> Result<Vec<(AccountId, Money)>, MoneyFlowError> {
        let currencies: Vec<_> = self.currencies().collect();
        let mut rows = Vec::new();
        for account in self.accounts() {
            for currency in &currencies {
                let residual = self.residual_of(account, *currency);
                if residual != 0 {
                    rows.push((
                        account,
                        Money::new(narrow(residual, "residuals_by_account")?, *currency),
                    ));
                }
            }
        }
        Ok(rows)
    }

    fn accounts(&self) -> BTreeSet<AccountId> {
        self.ledgers()
            .into_iter()
            .flat_map(|ledger| ledger.keys().map(|(account, _)| *account))
            .collect()
    }

    fn ledgers(&self) -> [&Ledger; 10] {
        [
            &self.came_in,
            &self.went_out,
            &self.earned_by_capital,
            &self.moved_into_assets,
            &self.fees,
            &self.taxes,
            &self.internal_transfers,
            &self.indeterminate,
            &self.unstated,
            &self.cash_delta,
        ]
    }
}

fn has_contour_cash_leg(event: &Event, contour: &ContourDefinition) -> bool {
    event
        .legs
        .iter()
        .any(|leg| contour.contains(leg.account) && leg.cash_effect().is_some())
}

fn add(
    ledger: &mut Ledger,
    account: AccountId,
    money: Money,
    quantity: &'static str,
    event: EventId,
) -> Result<(), MoneyFlowError> {
    let slot = ledger
        .entry((account, money.currency()))
        .or_insert_with(|| PostedMinor::new(0));
    *slot = slot
        .checked_add(money.amount())
        .ok_or(MoneyFlowError::Overflow { quantity, event })?;
    Ok(())
}

fn add_category(
    ledger: &mut CategoryLedger,
    category: CategoryId,
    money: Money,
    event: EventId,
) -> Result<(), MoneyFlowError> {
    let slot = ledger
        .entry((category, money.currency()))
        .or_insert_with(|| PostedMinor::new(0));
    *slot = slot
        .checked_add(money.amount())
        .ok_or(MoneyFlowError::Overflow {
            quantity: "went_out_by_category",
            event,
        })?;
    Ok(())
}

fn add_not_decomposed(
    decomposition: &mut (UndecomposedCounts, UndecomposedLedger),
    seen_keys: &mut BTreeSet<(AccountId, CurrencyCode, UndecomposedCause)>,
    account: AccountId,
    money: Money,
    cause: UndecomposedCause,
    event: EventId,
) -> Result<(), MoneyFlowError> {
    let key = (account, money.currency(), cause);
    let slot = decomposition
        .1
        .entry(key)
        .or_insert_with(|| PostedMinor::new(0));
    *slot = slot
        .checked_add(money.amount())
        .ok_or(MoneyFlowError::Overflow {
            quantity: "not_decomposed",
            event,
        })?;
    if seen_keys.insert(key) {
        let count = decomposition.0.entry(key).or_default();
        *count = count
            .checked_add(1)
            .ok_or(MoneyFlowError::AggregateOverflow {
                quantity: "not_decomposed_count",
            })?;
    }
    Ok(())
}

fn negated(money: Money, quantity: &'static str, event: EventId) -> Result<Money, MoneyFlowError> {
    let amount = money
        .amount()
        .checked_neg()
        .ok_or(MoneyFlowError::Overflow { quantity, event })?;
    Ok(Money::new(amount, money.currency()))
}

fn total(
    ledger: &Ledger,
    currency: CurrencyCode,
    quantity: &'static str,
) -> Result<Money, MoneyFlowError> {
    total_filtered(ledger, currency, quantity, |amount| amount != 0)
}

fn total_positive(
    ledger: &Ledger,
    currency: CurrencyCode,
    quantity: &'static str,
) -> Result<Money, MoneyFlowError> {
    total_filtered(ledger, currency, quantity, |amount| amount > 0)
}

fn total_filtered(
    ledger: &Ledger,
    currency: CurrencyCode,
    quantity: &'static str,
    include: impl Fn(i64) -> bool,
) -> Result<Money, MoneyFlowError> {
    let amount = ledger
        .iter()
        .filter(|((_, item_currency), amount)| *item_currency == currency && include(amount.raw()))
        .map(|(_, amount)| i128::from(amount.raw()))
        .sum::<i128>();
    Ok(Money::new(narrow(amount, quantity)?, currency))
}

fn narrow(amount: i128, quantity: &'static str) -> Result<PostedMinor, MoneyFlowError> {
    i64::try_from(amount)
        .map(PostedMinor::new)
        .map_err(|_| MoneyFlowError::AggregateOverflow { quantity })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::category::{CategoryAssignment, CategoryBasis};
    use crate::contour::{ContourDefinition, ContourId, ContourVersion};
    use crate::event::Event;
    use crate::event::corporate_action::CorporateAction;
    use crate::event::kind::{EventKind, TaxOrigin, TradeSide};
    use crate::event::leg::Leg;
    use crate::event::offer::{OfferExerciseAction, OfferSubmissionId};
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CategoryId, CategoryRuleId, InstrumentId, TransferId};
    use crate::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use time::{Date, macros::date};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }
    fn value(result: Result<Money, MoneyFlowError>) -> Money {
        result.expect("aggregate fits")
    }

    fn august() -> DateWindow {
        DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        }
    }

    /// Builds an event dated inside August with the given kind and legs.
    ///
    /// Wraps the crate's existing constructor rather than rewriting the event
    /// envelope: a hand-written envelope silently diverges from the real one,
    /// and the test then tests the fixture instead of the code — the reason
    /// `test_support` exists at all.
    fn event(kind: EventKind, legs: Vec<Leg>, on: Date) -> Event {
        let account = legs.first().expect("at least one leg").account;
        event_with(account, on, 1, kind, legs)
    }

    impl CategoryIndex for () {
        fn assignment(&self, _event: &Event) -> CategoryAssignment {
            CategoryAssignment::NotDecomposed
        }
    }

    struct FixedIndex(Vec<(&'static str, CategoryAssignment)>);

    impl FixedIndex {
        fn new(rows: Vec<(&'static str, CategoryAssignment)>) -> Self {
            Self(rows)
        }
    }

    impl CategoryIndex for FixedIndex {
        fn assignment(&self, event: &Event) -> CategoryAssignment {
            self.0
                .iter()
                .find(|(key, _)| event.provenance.source_operation_id() == Some(*key))
                .map_or(CategoryAssignment::NotDecomposed, |(_, assignment)| {
                    *assignment
                })
        }
    }

    struct AlwaysIndex(CategoryId);

    impl CategoryIndex for AlwaysIndex {
        fn assignment(&self, _event: &Event) -> CategoryAssignment {
            CategoryAssignment::Assigned {
                category: self.0,
                basis: CategoryBasis::SourceCategory {
                    rule: CategoryRuleId(uuid::Uuid::from_u128(1)),
                },
            }
        }
    }

    fn outflow(account: AccountId, row: &str, amount: Money) -> Event {
        let mut event = event(
            EventKind::CashOut { amount },
            vec![Leg::cash(account, amount)],
            date!(2026 - 08 - 01),
        );
        event.provenance = event.provenance.with_source_operation_id(row.to_owned());
        event
    }

    fn transfer(from: AccountId, to: AccountId, amount: Money) -> Event {
        event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount,
            },
            vec![
                Leg::cash(
                    from,
                    Money::new(PostedMinor::new(-amount.amount().raw()), amount.currency()),
                ),
                Leg::cash(to, amount),
            ],
            date!(2026 - 08 - 01),
        )
    }

    #[test]
    fn the_decomposition_sums_to_the_outflow_it_decomposes() {
        // The decomposition's own identity. Without asserting it, a filtering
        // bug drops a row from the breakdown while the headline stays right.
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let food = CategoryId(uuid::Uuid::from_u128(10));
        let index = FixedIndex::new(vec![
            (
                "row-1",
                CategoryAssignment::Assigned {
                    category: food,
                    basis: CategoryBasis::SourceCategory {
                        rule: CategoryRuleId(uuid::Uuid::from_u128(1)),
                    },
                },
            ),
            ("row-2", CategoryAssignment::NotDecomposed),
        ]);
        let mut flow = MoneyFlow::new();
        for (row, amount) in [("row-1", rub(-30_000)), ("row-2", rub(-12_000))] {
            flow.apply(&outflow(card, row, amount), &contour, august(), &index)
                .expect("applies");
        }

        let by_category = flow.went_out_by_category(CurrencyCode::Rub).expect("fits");
        let (count, undecomposed) = flow.not_decomposed(CurrencyCode::Rub).expect("fits");
        let decomposed: i64 = by_category
            .iter()
            .map(|(_, money)| money.amount().raw())
            .sum();

        assert_eq!(count, 1);
        assert_eq!(undecomposed.amount().raw(), 12_000);
        assert_eq!(
            decomposed + undecomposed.amount().raw(),
            flow.went_out(CurrencyCode::Rub)
                .expect("fits")
                .amount()
                .raw()
        );
    }

    #[test]
    fn a_movement_to_an_unnamed_own_account_is_neither_spending_nor_a_transfer() {
        // The defect this whole shape answers: read as `CashOut` the amount is
        // money spent, and read as an internal transfer it is money that stayed
        // inside. It is neither, because the far side is unnamed, so it has a
        // quantity of its own — and the identity still closes, because the cash
        // really did move.
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::OwnAccountMovement {
                    amount: rub(-480_000),
                },
                vec![Leg::cash(card, rub(-480_000))],
                date!(2025 - 08 - 12),
            ),
            &contour,
            august(),
            &NoCategories,
        )
        .expect("applies");

        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.internal_transfers(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.indeterminate(CurrencyCode::Rub)), rub(-480_000));
        // Never categorised: «what did I spend it on» is not a question anyone
        // can ask of a movement that may not have been spending at all.
        let (count, amount) = flow.not_decomposed(CurrencyCode::Rub).expect("fits");
        assert_eq!(count, 0);
        assert_eq!(amount.amount().raw(), 0);
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn a_movement_with_no_direction_is_reported_and_left_out_of_the_identity() {
        // It posts no leg, so no cash moved as far as this journal is
        // concerned. Folding its magnitude into the identity would open a
        // residual of exactly the amount the journal declined to invent; the
        // reader is told instead, by name.
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            // Built with `event_with` rather than the `event` helper beside it:
            // that helper takes the account from the first leg, and this fact
            // has none — which is the point of it.
            &event_with(
                card,
                date!(2025 - 08 - 12),
                1,
                EventKind::UnresolvedOwnAccountMovement {
                    amount: rub(480_000),
                },
                Vec::new(),
            ),
            &contour,
            august(),
            &NoCategories,
        )
        .expect("applies");

        assert_eq!(value(flow.unstated(CurrencyCode::Rub)), rub(480_000));
        assert_eq!(value(flow.cash_delta(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn an_internal_transfer_is_never_given_a_category() {
        // Asking "what did I spend it on" of a transfer to one's own deposit
        // is exactly what made transfers a spending category in Actual Budget.
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![card, deposit],
        );
        let food = CategoryId(uuid::Uuid::from_u128(10));
        let index = AlwaysIndex(food);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &transfer(card, deposit, rub(480_000)),
            &contour,
            august(),
            &index,
        )
        .expect("applies");

        assert!(
            flow.went_out_by_category(CurrencyCode::Rub)
                .expect("fits")
                .is_empty()
        );
        let (count, amount) = flow.not_decomposed(CurrencyCode::Rub).expect("fits");
        assert_eq!(count, 0);
        assert_eq!(amount.amount().raw(), 0);
    }

    /// The undecomposed total is one number made of two unlike things. A caller
    /// that must name a remedy needs them apart: a rule reaches the spending row
    /// and can never reach the transfer, because `apply` does not ask the category
    /// index about a `CashTransfer` at all.
    #[test]
    fn undecomposed_rows_are_kept_apart_by_what_left_them_undecomposed() {
        let card = AccountId::new_random();
        let outside = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &outflow(card, "row-1", rub(-700)),
            &contour,
            august(),
            &NoCategories,
        )
        .expect("applies");
        flow.apply(
            &event(
                EventKind::CashTransfer {
                    transfer_id: TransferId::new_random(),
                    from: card,
                    to: outside,
                    amount: rub(1_100),
                },
                vec![Leg::cash(card, rub(-1_100))],
                date!(2026 - 08 - 10),
            ),
            &contour,
            august(),
            &NoCategories,
        )
        .expect("applies");

        assert_eq!(
            flow.not_decomposed(CurrencyCode::Rub).expect("fits"),
            (2, rub(1_800)),
            "the total still counts both"
        );
        assert_eq!(
            flow.not_decomposed_by_account(CurrencyCode::Rub)
                .expect("fits"),
            vec![(card, 2, rub(1_800))],
            "and so does the per-account breakdown"
        );
        assert_eq!(
            flow.not_decomposed_by_account_and_cause(CurrencyCode::Rub)
                .expect("fits"),
            vec![
                (card, UndecomposedCause::NoRuleMatched, 1, rub(700)),
                (card, UndecomposedCause::ExternalTransfer, 1, rub(1_100)),
            ]
        );
    }

    #[test]
    fn an_internal_transfer_is_neither_income_nor_expense() {
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![card, deposit],
        );
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashTransfer {
                    transfer_id: TransferId::new_random(),
                    from: card,
                    to: deposit,
                    amount: rub(480_000),
                },
                vec![
                    Leg::cash(card, rub(-480_000)),
                    Leg::cash(deposit, rub(480_000)),
                ],
                date!(2026 - 08 - 10),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");

        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(0));
        assert_eq!(
            value(flow.internal_transfers(CurrencyCode::Rub)),
            rub(480_000)
        );
        assert_eq!(value(flow.cash_delta(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
        assert!(
            flow.residuals_by_account()
                .expect("aggregate fits")
                .is_empty()
        );
    }

    #[test]
    fn a_coupon_is_earned_by_the_capital_and_not_an_inflow() {
        let broker = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Income {
                    instrument: Some(InstrumentId::new_random()),
                    gross: rub(31_000),
                    kind: None,
                },
                vec![Leg::cash(broker, rub(31_000))],
                date!(2026 - 08 - 15),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");

        assert_eq!(
            value(flow.earned_by_capital(CurrencyCode::Rub)),
            rub(31_000)
        );
        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.cash_delta(CurrencyCode::Rub)), rub(31_000));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn a_purchase_moves_money_into_assets_and_its_fee_is_counted_once() {
        let broker = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: Quantity::zero(),
                    gross: rub(-100_000),
                    fee: Some(rub(-350)),
                    basis_fee: None,
                    basis_fee_exact: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(broker, rub(-100_000)),
                    Leg::fee(broker, rub(-350)),
                ],
                date!(2026 - 08 - 20),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");

        assert_eq!(
            value(flow.moved_into_assets(CurrencyCode::Rub)),
            rub(100_000)
        );
        assert_eq!(value(flow.fees(CurrencyCode::Rub)), rub(350));
        assert_eq!(value(flow.cash_delta(CurrencyCode::Rub)), rub(-100_350));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn a_self_paid_tax_is_not_ordinary_spending() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Tax {
                    amount: rub(-13_000),
                    origin: TaxOrigin::SelfPaid,
                },
                vec![Leg::tax(card, rub(-13_000))],
                date!(2026 - 08 - 25),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");

        assert_eq!(value(flow.taxes(CurrencyCode::Rub)), rub(13_000));
        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn salary_in_and_spending_out_close_the_identity() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        for (kind, legs, on) in [
            (
                EventKind::CashIn {
                    amount: rub(300_000),
                },
                vec![Leg::cash(card, rub(300_000))],
                date!(2026 - 08 - 05),
            ),
            (
                EventKind::CashOut {
                    amount: rub(-120_000),
                },
                vec![Leg::cash(card, rub(-120_000))],
                date!(2026 - 08 - 12),
            ),
        ] {
            flow.apply(&event(kind, legs, on), &contour, august(), &())
                .expect("applies");
        }

        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(300_000));
        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(120_000));
        assert_eq!(value(flow.cash_delta(CurrencyCode::Rub)), rub(180_000));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn earnings_are_split_by_what_produced_them_and_still_sum_to_the_total() {
        // "How much did the capital earn" is one number; "which asset brought
        // what" is the question asked immediately after, and one label cannot
        // answer it for both a savings account and a card programme. The split
        // is the same money along another axis, so it must sum back exactly.
        let deposit = AccountId::new_random();
        let card = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![deposit, card],
        );
        let mut flow = MoneyFlow::new();
        for (account, amount, on) in [
            (deposit, 6_000, date!(2026 - 08 - 27)),
            (deposit, 1_000, date!(2026 - 08 - 07)),
            (card, 3_500, date!(2026 - 08 - 25)),
        ] {
            flow.apply(
                &event(
                    EventKind::Income {
                        instrument: None,
                        gross: rub(amount),
                        kind: None,
                    },
                    vec![Leg::cash(account, rub(amount))],
                    on,
                ),
                &contour,
                august(),
                &(),
            )
            .expect("applies");
        }

        let split = flow
            .earned_by_capital_by_source(CurrencyCode::Rub)
            .expect("split");
        // Two sources, not three: the two payments on one account are one
        // source. No rules exist here, so neither carries a category — and an
        // undecomposed earning is its own line rather than a bucket.
        assert_eq!(split.len(), 2);
        let from_deposit = split
            .iter()
            .find(|(source, _)| source.account == deposit)
            .expect("deposit");
        assert_eq!(from_deposit.1, rub(7_000));
        assert!(from_deposit.0.category.is_none());
        let from_card = split
            .iter()
            .find(|(source, _)| source.account == card)
            .expect("card");
        assert_eq!(from_card.1, rub(3_500));

        let summed: i64 = split.iter().map(|(_, amount)| amount.amount().raw()).sum();
        assert_eq!(
            summed,
            value(flow.earned_by_capital(CurrencyCode::Rub))
                .amount()
                .raw()
        );
    }

    /// A category index that answers with one category for every event, so a
    /// test can tell "no rule matched" apart from "the projection never asked".
    struct AlwaysCategory(CategoryId);

    impl CategoryIndex for AlwaysCategory {
        fn assignment(&self, _event: &Event) -> CategoryAssignment {
            CategoryAssignment::Assigned {
                category: self.0,
                basis: crate::category::CategoryBasis::SourceCategory {
                    rule: crate::ids::CategoryRuleId::new_random(),
                },
            }
        }
    }

    #[test]
    fn an_earning_is_reported_under_the_owners_income_category() {
        // Income is decomposed by the same rules as spending: cashback and
        // interest on a balance are the owner's categories. Asking for a
        // category only on the way out left every earning uncategorised while
        // the matching rule existed.
        let savings = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![savings]);
        let category = CategoryId::new_random();
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Income {
                    instrument: None,
                    gross: rub(1_200),
                    kind: None,
                },
                vec![Leg::cash(savings, rub(1_200))],
                date!(2026 - 08 - 07),
            ),
            &contour,
            august(),
            &AlwaysCategory(category),
        )
        .expect("applies");

        let split = flow
            .earned_by_capital_by_source(CurrencyCode::Rub)
            .expect("split");
        assert_eq!(split.len(), 1);
        assert_eq!(split[0].0.category, Some(category));
        assert_eq!(split[0].1, rub(1_200));
    }

    #[test]
    fn a_refund_is_subtracted_from_the_category_it_was_spent_in() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let category = CategoryId::new_random();
        let mut flow = MoneyFlow::new();
        for (kind, amount, on) in [
            (
                EventKind::CashOut {
                    amount: rub(-4_000),
                },
                rub(-4_000),
                date!(2026 - 08 - 04),
            ),
            (
                EventKind::Refund { amount: rub(1_500) },
                rub(1_500),
                date!(2026 - 08 - 18),
            ),
        ] {
            flow.apply(
                &event(kind, vec![Leg::cash(card, amount)], on),
                &contour,
                august(),
                &AlwaysCategory(category),
            )
            .expect("applies");
        }

        let by_category = flow
            .went_out_by_category(CurrencyCode::Rub)
            .expect("categories");
        assert_eq!(by_category, vec![(category, rub(2_500))]);
        let (count, amount) = flow
            .not_decomposed(CurrencyCode::Rub)
            .expect("undecomposed");
        assert_eq!((count, amount), (0, rub(0)));
    }

    #[test]
    fn a_refund_reduces_what_went_out_and_is_never_income() {
        // A purchase and its return in the same month leave nothing spent and
        // nothing earned. Reading the return as an arrival would report income
        // nobody earned — the whole reason Refund is a kind of its own.
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        for (kind, legs, on) in [
            (
                EventKind::CashOut {
                    amount: rub(-80_000),
                },
                vec![Leg::cash(card, rub(-80_000))],
                date!(2026 - 08 - 12),
            ),
            (
                EventKind::Refund {
                    amount: rub(80_000),
                },
                vec![Leg::cash(card, rub(80_000))],
                date!(2026 - 08 - 20),
            ),
        ] {
            flow.apply(&event(kind, legs, on), &contour, august(), &())
                .expect("applies");
        }

        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.cash_delta(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn a_partial_refund_leaves_the_difference_as_what_was_spent() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        for (kind, legs, on) in [
            (
                EventKind::CashOut {
                    amount: rub(-5_000),
                },
                vec![Leg::cash(card, rub(-5_000))],
                date!(2026 - 08 - 03),
            ),
            (
                EventKind::Refund { amount: rub(2_000) },
                vec![Leg::cash(card, rub(2_000))],
                date!(2026 - 08 - 09),
            ),
        ] {
            flow.apply(&event(kind, legs, on), &contour, august(), &())
                .expect("applies");
        }

        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(0));
        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(3_000));
        assert_eq!(value(flow.cash_delta(CurrencyCode::Rub)), rub(-3_000));
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn an_event_outside_the_window_is_ignored() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashIn { amount: rub(1_000) },
                vec![Leg::cash(card, rub(1_000))],
                date!(2026 - 07 - 31),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");
        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(0));
        assert_eq!(flow.currencies().count(), 0);
    }

    #[test]
    fn two_accounts_wrong_in_opposite_directions_are_both_named() {
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![card, deposit],
        );
        let mut flow = MoneyFlow::new();
        for (account, amount) in [(card, rub(-5_000)), (deposit, rub(5_000))] {
            flow.apply(
                &event(
                    EventKind::OpeningCash { amount },
                    vec![Leg::cash(account, amount)],
                    date!(2026 - 08 - 18),
                ),
                &contour,
                august(),
                &(),
            )
            .expect("applies");
        }

        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
        let named = flow.residuals_by_account().expect("aggregate fits");
        assert_eq!(named.len(), 2, "both accounts must be named: {named:?}");
    }

    #[test]
    fn asset_redemption_cash_is_not_an_unexplained_residual() {
        let broker = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CorporateAction {
                    action: CorporateAction::PartialRedemption {
                        instrument,
                        custody: crate::ids::CustodyId::new_random(),
                        quantity: Quantity::zero(),
                        principal_returned_per_unit: PerUnitAmount::new(
                            Dec::one(),
                            CurrencyCode::Rub,
                        ),
                        compensation: rub(100_000),
                        effective_date: date!(2026 - 08 - 18),
                        record_date: None,
                        grounds: None,
                        basis_allocation: Default::default(),
                    },
                },
                vec![Leg::principal(broker, instrument, rub(100_000))],
                date!(2026 - 08 - 18),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");

        assert_eq!(
            value(flow.moved_into_assets(CurrencyCode::Rub)),
            rub(-100_000)
        );
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn settled_offer_cash_is_not_an_unexplained_residual() {
        let broker = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::OfferExercise {
                    action: OfferExerciseAction::Settled {
                        submission: OfferSubmissionId::new_random(),
                        instrument,
                        custody: crate::ids::CustodyId::new_random(),
                        quantity: Quantity::zero(),
                        gross: rub(100_000),
                        fee: None,
                        accrued_interest: None,
                    },
                },
                vec![Leg::cash(broker, rub(100_000))],
                date!(2026 - 08 - 19),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");

        assert_eq!(
            value(flow.moved_into_assets(CurrencyCode::Rub)),
            rub(-100_000)
        );
        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn aggregate_residual_arithmetic_is_checked_without_panicking() {
        let broker = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        for (kind, legs, on) in [
            (
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument: InstrumentId::new_random(),
                    quantity: Quantity::zero(),
                    gross: rub(-i64::MAX),
                    fee: None,
                    basis_fee: None,
                    basis_fee_exact: None,
                    accrued_interest: None,
                },
                vec![Leg::cash(broker, rub(-i64::MAX))],
                date!(2026 - 08 - 05),
            ),
            (
                EventKind::CashIn {
                    amount: rub(i64::MAX),
                },
                vec![Leg::cash(broker, rub(i64::MAX))],
                date!(2026 - 08 - 06),
            ),
            (
                EventKind::Income {
                    instrument: None,
                    gross: rub(i64::MAX),
                    kind: None,
                },
                vec![Leg::cash(broker, rub(i64::MAX))],
                date!(2026 - 08 - 07),
            ),
        ] {
            flow.apply(&event(kind, legs, on), &contour, august(), &())
                .expect("applies");
        }

        assert_eq!(value(flow.residual(CurrencyCode::Rub)), rub(0));
    }

    #[test]
    fn aggregate_quantity_overflow_is_reported() {
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![first, second],
        );
        let mut flow = MoneyFlow::new();
        for account in [first, second] {
            flow.apply(
                &event(
                    EventKind::CashIn {
                        amount: rub(i64::MAX),
                    },
                    vec![Leg::cash(account, rub(i64::MAX))],
                    date!(2026 - 08 - 05),
                ),
                &contour,
                august(),
                &(),
            )
            .expect("applies");
        }

        assert!(matches!(
            flow.came_in(CurrencyCode::Rub),
            Err(MoneyFlowError::AggregateOverflow {
                quantity: "came_in"
            })
        ));
    }

    #[test]
    fn external_outflow_transfers_are_undecomposed_and_not_resolved() {
        let card = AccountId::new_random();
        let outside = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let category = CategoryId(uuid::Uuid::from_u128(10));
        let mut flow = MoneyFlow::new();
        flow.apply(
            &transfer(card, outside, rub(7_500)),
            &contour,
            august(),
            &AlwaysIndex(category),
        )
        .expect("applies");

        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(7_500));
        assert!(
            flow.went_out_by_category(CurrencyCode::Rub)
                .expect("aggregate fits")
                .is_empty()
        );
        assert_eq!(
            flow.not_decomposed(CurrencyCode::Rub)
                .expect("aggregate fits"),
            (1, rub(7_500))
        );
    }

    #[test]
    fn category_totals_skip_other_currencies_and_zeroed_groups() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let category = CategoryId(uuid::Uuid::from_u128(10));
        let mut flow = MoneyFlow::new();
        for amount in [rub(-5_000), rub(5_000)] {
            flow.apply(
                &outflow(card, "same", amount),
                &contour,
                august(),
                &AlwaysIndex(category),
            )
            .expect("applies");
        }
        let usd = Money::new(PostedMinor::new(-2_000), CurrencyCode::Usd);
        flow.apply(
            &outflow(card, "usd", usd),
            &contour,
            august(),
            &AlwaysIndex(category),
        )
        .expect("applies");

        assert!(
            flow.went_out_by_category(CurrencyCode::Rub)
                .expect("aggregate fits")
                .is_empty()
        );
        assert_eq!(
            flow.went_out_by_category(CurrencyCode::Usd)
                .expect("aggregate fits"),
            vec![(
                category,
                Money::new(PostedMinor::new(2_000), CurrencyCode::Usd)
            )]
        );
    }

    #[test]
    fn no_categories_keeps_an_outflow_in_the_undecomposed_bucket() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &outflow(card, "row-1", rub(-9_000)),
            &contour,
            august(),
            &NoCategories,
        )
        .expect("applies");

        assert_eq!(value(flow.went_out(CurrencyCode::Rub)), rub(9_000));
        assert_eq!(
            flow.not_decomposed(CurrencyCode::Rub)
                .expect("aggregate fits"),
            (1, rub(9_000))
        );
    }

    #[test]
    fn undecomposed_breakdown_preserves_each_accounts_count_when_amounts_cancel() {
        let first = AccountId::new_random();
        let second = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [first, second]);
        let mut flow = MoneyFlow::new();
        for (account, amount) in [(first, -5_000), (first, 5_000), (second, -2_000)] {
            flow.apply(
                &outflow(account, "row", rub(amount)),
                &contour,
                august(),
                &NoCategories,
            )
            .expect("applies");
        }

        let by_account = flow
            .not_decomposed_by_account(CurrencyCode::Rub)
            .expect("aggregate fits");
        assert_eq!(by_account.len(), 2);
        assert_eq!(
            by_account
                .iter()
                .find(|(account, _, _)| *account == first)
                .copied(),
            Some((first, 2, rub(0)))
        );
        assert_eq!(
            by_account
                .iter()
                .find(|(account, _, _)| *account == second)
                .copied(),
            Some((second, 1, rub(2_000)))
        );
        assert_eq!(
            flow.not_decomposed(CurrencyCode::Rub)
                .expect("aggregate fits"),
            (3, rub(2_000))
        );
    }
    #[test]
    fn undecomposed_count_overflow_is_reported_without_panicking() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.not_decomposed.0.insert(
            (card, CurrencyCode::Rub, UndecomposedCause::NoRuleMatched),
            u64::MAX,
        );

        let error = flow
            .apply(
                &outflow(card, "row-1", rub(-1)),
                &contour,
                august(),
                &NoCategories,
            )
            .expect_err("count overflow must be reported");
        assert_eq!(
            error,
            MoneyFlowError::AggregateOverflow {
                quantity: "not_decomposed_count"
            }
        );
    }

    #[test]
    fn two_currencies_never_mix() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let usd = Money::new(PostedMinor::new(50_000), CurrencyCode::Usd);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashIn {
                    amount: rub(300_000),
                },
                vec![Leg::cash(card, rub(300_000))],
                date!(2026 - 08 - 05),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");
        flow.apply(
            &event(
                EventKind::CashIn { amount: usd },
                vec![Leg::cash(card, usd)],
                date!(2026 - 08 - 06),
            ),
            &contour,
            august(),
            &(),
        )
        .expect("applies");

        assert_eq!(value(flow.came_in(CurrencyCode::Rub)), rub(300_000));
        assert_eq!(value(flow.came_in(CurrencyCode::Usd)), usd);
        assert_eq!(flow.currencies().count(), 2);
    }
}
