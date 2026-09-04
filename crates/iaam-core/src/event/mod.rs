//! Journal event envelope (§4.1).

pub mod allocation;
pub mod corporate_action;
pub mod correction;
pub mod kind;
pub mod leg;
pub mod legs;
pub mod offer;
pub mod provenance;
pub mod source_row;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dates::{EffectiveOrder, EventDates};
use crate::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId};
use crate::money::{CurrencyCode, Money, MoneyError, Quantity};
use crate::numeric::decimal::Dec;
use corporate_action::{CorporateAction, FractionalTreatment};
use kind::{EventKind, TradeSide};
use leg::{Leg, LegKind};
use legs::LegExpectation;
use offer::OfferExerciseAction;
use provenance::Provenance;

/// Confidence in the recorded fact (§4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// The fact is confirmed by the source.
    Known,
    /// The value was reconstructed or estimated.
    Estimated,
    /// The value is unknown and must not be replaced with zero.
    Unknown,
}

/// Link to another event (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relation {
    /// Standalone event.
    None,
    /// Reversal of the specified event.
    Reversal { target: EventId },
    /// Replacement of the specified event. Always follows a reversal.
    Replacement { target: EventId },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventValidationError {
    #[error("for {kind} expected: {expected}; legs found: {found}")]
    LegCount {
        kind: &'static str,
        expected: &'static str,
        found: usize,
    },
    #[error("for {kind} monetary leg has the wrong sign: {amount} in {currency:?}")]
    WrongSign {
        kind: &'static str,
        amount: i64,
        currency: CurrencyCode,
    },
    #[error("leg total ({legs}) does not match event amount ({declared}) for {kind}")]
    AmountMismatch {
        kind: &'static str,
        legs: i64,
        declared: i64,
    },
    #[error("leg assigned to the wrong account: expected {expected:?}")]
    WrongAccount { expected: AccountId },
    #[error("transfer sides do not balance: residual {residual}")]
    TransferResidual { residual: i64 },
    #[error(
        "account {account:?} is both transfer source and recipient; \
         moving money within one account changes no balance \
         and therefore is not a movement fact"
    )]
    TransferToSelf { account: AccountId },
    #[error(
        "for {kind} leg does not match the event in field {field}: \
         the event says one thing, the leg another"
    )]
    LegDoesNotMatchEvent {
        kind: &'static str,
        field: &'static str,
    },
    #[error("for {kind} basis_fee and basis_fee_exact must be present together")]
    BasisFeePresenceMismatch { kind: &'static str },
    #[error(
        "for {kind} basis_fee amount {posted} does not match \
         basis_fee_exact rounded amount {exact}"
    )]
    BasisFeeAmountMismatch {
        kind: &'static str,
        posted: i64,
        exact: i64,
    },
    #[error("for {kind} the set {field} must name at least one element")]
    EmptySet {
        kind: &'static str,
        field: &'static str,
    },
    #[error(
        "for {kind} {field} must equal the union of refused row dimensions: \
         expected {expected}, actual {actual}"
    )]
    DimensionsMismatch {
        kind: &'static str,
        field: &'static str,
        expected: String,
        actual: String,
    },
    #[error("for {kind} value {field} must be positive, got {value}")]
    NonPositive {
        kind: &'static str,
        field: &'static str,
        value: String,
    },
    #[error("extra leg for {event}: expected {expected} legs, found {found}")]
    UnexpectedLeg {
        event: &'static str,
        expected: usize,
        found: usize,
    },
    #[error("missing {kind:?} leg for {event}: expected {expected} legs, found {found}")]
    MissingLeg {
        event: &'static str,
        kind: LegKind,
        expected: usize,
        found: usize,
    },
    #[error("for {event} leg {kind:?} did not match the expected field {field}")]
    LegMismatch {
        event: &'static str,
        kind: LegKind,
        field: &'static str,
    },
    #[error(transparent)]
    Numeric(#[from] crate::numeric::NumericError),
    #[error(transparent)]
    Money(#[from] MoneyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sign {
    Positive,
    Negative,
    Any,
}

/// Journal fact. Immutable once recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub schema_version: u32,
    pub owner: OwnerId,
    pub account: AccountId,
    pub kind: EventKind,
    pub dates: EventDates,
    pub order: EffectiveOrder,
    pub legs: Vec<Leg>,
    pub provenance: Provenance,
    pub relation: Relation,
    pub confidence: Confidence,
    /// Client idempotency key (§10.6).
    pub idempotency_key: Option<String>,
}

/// Current event schema version.
///
/// Version 2 differed from version 1 by the added variant
/// [`EventKind::Valuation`]; version 3 differs from version 2
/// by the added variant [`EventKind::ControlAssertion`]; version 4 —
/// by variants [`EventKind::CorporateAction`] and
/// [`EventKind::OfferExercise`], and by the income kind in `Income`.
/// Version 5 adds the optional source time inside [`EffectiveOrder`].
/// Version 6 adds optional basis-only trade fee fields; both default to absent
/// so older journal facts remain readable, while the schema number still
/// distinguishes software that understands the new fact.
/// Version 7 adds [`EventKind::ImportCoverageGap`].
/// Version 8 adds the refused rows inside that variant and the variant
/// [`EventKind::ImportRowResolution`]: a coverage gap now says WHICH rows are
/// missing, and a row is disposed of by an explicit fact rather than inferred
/// from the presence of an event.
/// Version 9 adds the variant [`EventKind::Tax`]: a self-paid tax is a fact of
/// its own rather than an unnamed outflow.
/// Version 10 adds the optional source description inside [`Provenance`]. It
/// defaults to absent, so facts already in the journal stay readable, while
/// the number still distinguishes software that understands the new field.
/// Version 11 adds the variant [`EventKind::Refund`]: money a counterparty
/// returns reverses spending, and reading it as an arrival reports income
/// nobody earned.
/// Version 12 adds the optional declaring principal inside [`Provenance`]. It
/// defaults to absent, so facts already in the journal stay readable — and the
/// absence is load-bearing rather than incidental: a retraction that may only
/// take back what its own caller declared must refuse a fact that names no
/// declarer, so the number is what tells a reader that «no principal» means
/// «written before anyone was recorded» rather than «written by nobody».
/// Version 13 adds [`EventKind::OwnAccountMovement`] and
/// [`EventKind::UnresolvedOwnAccountMovement`]: a movement whose far side the
/// source asserted to be the owner's and did not name is neither an external
/// flow nor a complete transfer, and until now it could be recorded only as one
/// of those two lies.
/// Version 14 adds the optional source operation word inside [`Provenance`],
/// beside the source category it used to be written through. The two are
/// different facts — what the operation *was* against what it was *for* — and
/// one slot could hold only one of them, so a category rule written on a
/// source's category never matched a row that came in as an observation. It
/// defaults to absent, so facts already in the journal stay readable, and
/// nothing rewrites them: a fact below this version whose `source_category`
/// holds an operation word keeps it, because provenance records what a path
/// meant at the time and a repair would be this software guessing what a
/// source said.
pub const SCHEMA_VERSION: u32 = 14;

/// The version from which [`provenance::Provenance::source_category`] holds a
/// source's **category** on every path, and nothing else.
///
/// This is the boundary decision 0020 §3 promised a reader, named so that the
/// readers who need it do not each spell the number themselves. Below it, on the
/// observation path, that field may hold the source's *operation word*: one slot
/// carried both facts, both paths stamped the same parser version, and §3
/// refused a migration because telling the two apart afterwards is not possible
/// and guessing would write, as the source's own category, a word the source
/// never used there.
///
/// So a rule the owner writes about a source's **category** must not be tested
/// against a fact below this version. That is not the same as rewriting the
/// fact: what the fact carries is what the path meant at the time, and this
/// merely declines to read it as evidence of something it may not be — exactly
/// as §3 already has recomputation reconsider such a row with no operation word
/// at all, `Provenance::source_kind` being `None` on every one of them.
pub const SOURCE_CATEGORY_IS_A_CATEGORY_FROM: u32 = 14;

/// Compare events for replay, preserving source-time semantics and making
/// equal-time imports independent of their insertion order.
pub(crate) fn compare_for_replay(left: &Event, right: &Event) -> std::cmp::Ordering {
    left.order
        .date()
        .cmp(&right.order.date())
        .then_with(
            || match (left.order.source_time(), right.order.source_time()) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            },
        )
        .then_with(|| {
            if left.order.source_time().is_some() {
                left.provenance
                    .raw_hash()
                    .as_str()
                    .cmp(right.provenance.raw_hash().as_str())
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .then_with(|| left.order.sequence().cmp(&right.order.sequence()))
        .then_with(|| left.id.cmp(&right.id))
}

impl Event {
    /// Total monetary effect of all legs in the specified currency.
    pub fn cash_effect(&self, currency: CurrencyCode) -> Result<Money, MoneyError> {
        let amounts: Vec<Money> = self
            .legs
            .iter()
            .filter_map(Leg::cash_effect)
            .filter(|m| m.currency() == currency)
            .collect();
        Money::sum(&amounts, currency)
    }

    /// The cash this event moves on one account, or `None` when it moves none.
    ///
    /// Here rather than in the shell, where the fold used to live: summing money
    /// is arithmetic, and every number a response carries has to come from the
    /// core (§3.1, §13). A shell that adds two `Money` values itself is a second
    /// place where currency mixing and overflow are decided, and the second
    /// place is the one that gets it wrong.
    ///
    /// The currency is the one the account's own cash legs carry. Legs in a
    /// different currency are not summed into it and are not silently dropped:
    /// a mixture is an error, not a total, and `Money::sum` says so.
    pub fn cash_effect_on(&self, account: AccountId) -> Result<Option<Money>, MoneyError> {
        let amounts: Vec<Money> = self
            .legs
            .iter()
            .filter(|leg| leg.account == account)
            .filter_map(Leg::cash_effect)
            .collect();
        let Some(currency) = amounts.first().map(Money::currency) else {
            return Ok(None);
        };
        Money::sum(&amounts, currency).map(Some)
    }

    fn legs_of_kind(&self, kind: LegKind) -> Vec<&Leg> {
        self.legs.iter().filter(|l| l.kind == kind).collect()
    }

    fn cash_legs(&self) -> Vec<&Leg> {
        self.legs_of_kind(LegKind::Cash)
    }

    fn security_legs(&self) -> Vec<&Leg> {
        self.legs_of_kind(LegKind::SecurityQuantity)
    }

    /// Structural event validation (§15.2).
    ///
    /// **This is not an accounting balance.** Event legs do not form
    /// double-entry records: they have no equity, income, or expense counteraccounts.
    /// Therefore no universal «leg total equals zero» rule exists —
    /// a fee recorded as one actual leg will never produce zero,
    /// and that is correct. Each event type has its own shape, which is validated.
    ///
    /// The body only dispatches by event type: each type's shape is validated
    /// by a separate function, or one branch could silently borrow another's conditions.
    pub fn validate_structure(&self) -> Result<(), EventValidationError> {
        let name = self.kind.discriminant();
        match &self.kind {
            EventKind::CashIn { amount } => self.expect_single_cash(name, *amount, Sign::Positive),
            EventKind::CashOut { amount } => self.expect_single_cash(name, *amount, Sign::Negative),
            EventKind::Refund { amount } => self.expect_single_cash(name, *amount, Sign::Positive),
            EventKind::OpeningCash { amount } => self.expect_single_cash(name, *amount, Sign::Any),
            EventKind::Income { gross, .. } => {
                self.expect_single_cash(name, *gross, Sign::Positive)
            }
            EventKind::Fee { amount, .. } => self.validate_fee(name, *amount),
            EventKind::Tax { amount, .. } => self.validate_tax(name, *amount),
            EventKind::OwnAccountMovement { amount } => {
                self.validate_own_account_movement(name, *amount)
            }
            EventKind::UnresolvedOwnAccountMovement { amount } => {
                self.validate_unresolved_own_account_movement(name, *amount)
            }
            EventKind::CashTransfer {
                from, to, amount, ..
            } => self.validate_transfer(name, *from, *to, *amount),
            EventKind::Trade {
                side,
                instrument,
                quantity,
                gross,
                fee,
                accrued_interest,
                basis_fee,
                basis_fee_exact,
                ..
            } => self.validate_trade(
                name,
                *side,
                TradeDeclaration {
                    instrument: *instrument,
                    quantity: *quantity,
                    gross: *gross,
                    fee: *fee,
                    accrued_interest: *accrued_interest,
                    basis_fee: *basis_fee,
                    basis_fee_exact: *basis_fee_exact,
                },
            ),
            EventKind::OpeningPosition {
                instrument,
                quantity,
                ..
            } => self.validate_opening_position(name, *instrument, *quantity),
            EventKind::Valuation { price, .. } => self.validate_valuation(name, *price),
            EventKind::ControlAssertion { period, claim } => {
                self.validate_control_assertion(name, *period, *claim)
            }
            EventKind::ImportCoverageGap {
                period,
                dimensions,
                refused,
                rows,
            } => self.validate_import_coverage_gap(name, *period, dimensions, *refused, rows),
            EventKind::CorporateAction { action } => self.validate_corporate_action(name, action),
            EventKind::OfferExercise { action } => self.validate_offer_exercise(name, action),
        }
    }

    fn expect_single_cash(
        &self,
        name: &'static str,
        declared: Money,
        sign: Sign,
    ) -> Result<(), EventValidationError> {
        let legs = self.cash_legs();
        let money = single_leg_money(name, &legs, "exactly one monetary leg")?;
        let raw = money.amount().raw();
        let ok = match sign {
            Sign::Positive => raw > 0,
            Sign::Negative => raw < 0,
            Sign::Any => true,
        };
        if !ok {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: raw,
                currency: money.currency(),
            });
        }
        require_equal(name, money, declared)
    }

    /// A fee is recorded with **one** actual leg: the model has no expense
    /// counteraccount, so the legs do not sum to zero, and that is correct.
    fn validate_fee(
        &self,
        name: &'static str,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        let fee_legs = self.legs_of_kind(LegKind::Fee);
        let money = single_leg_money(name, &fee_legs, "exactly one fee leg")?;
        if money.amount().raw() >= 0 {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: money.amount().raw(),
                currency: money.currency(),
            });
        }
        require_equal(name, money, declared)
    }

    /// Tax: exactly one negative tax leg, equal to the declared amount.
    ///
    /// Deliberately a separate function from `validate_fee` rather than a
    /// shared one parameterised by leg kind: the two shapes are equal today by
    /// coincidence, and a shared body would silently impose one's future
    /// conditions on the other.
    fn validate_tax(
        &self,
        name: &'static str,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        let tax_legs = self.legs_of_kind(LegKind::Tax);
        let money = single_leg_money(name, &tax_legs, "exactly one tax leg")?;
        if money.amount().raw() >= 0 {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: money.amount().raw(),
                currency: money.currency(),
            });
        }
        require_equal(name, money, declared)
    }

    /// An own-account movement: one monetary leg on the event's own account,
    /// equal to the declared amount, and not zero.
    ///
    /// `Sign::Any`, as for `OpeningCash`, because the sign **is** the direction
    /// here and both are ordinary. Zero is refused separately, because
    /// `Sign::Any` admits it and a movement of nothing is not a movement — the
    /// same reason `ObservedRow::magnitude` refuses a zero row rather than
    /// recording a movement of zero.
    ///
    /// The leg's account is checked, unlike `CashIn` and `CashOut`, whose
    /// single leg is unchecked because nothing else on those events names an
    /// account to check it against. Here the whole claim is «this account moved
    /// and the other side is unnamed», so a leg on some other account would
    /// make the event say something no reader could recover.
    fn validate_own_account_movement(
        &self,
        name: &'static str,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        if declared.amount().raw() == 0 {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: 0,
                currency: declared.currency(),
            });
        }
        self.expect_single_cash(name, declared, Sign::Any)?;
        let legs = self.cash_legs();
        let leg = legs.first().ok_or(EventValidationError::LegCount {
            kind: name,
            expected: "exactly one monetary leg",
            found: 0,
        })?;
        if leg.account != self.account {
            return Err(EventValidationError::WrongAccount {
                expected: self.account,
            });
        }
        Ok(())
    }

    /// An unresolved own-account movement: no legs at all, and a positive
    /// magnitude.
    ///
    /// No legs is the variant's entire meaning, so it is checked first and
    /// refused by count rather than by sign: an event that carried a leg would
    /// be posting a direction the source never stated, which is the one thing
    /// this variant exists so that nobody has to do.
    ///
    /// The magnitude is positive because it is a magnitude. A negative one
    /// would be a direction stated in the only place this variant has left to
    /// state one, and it would be stated where nothing reads it.
    fn validate_unresolved_own_account_movement(
        &self,
        name: &'static str,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        if !self.legs.is_empty() {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "no legs",
                found: self.legs.len(),
            });
        }
        if declared.amount().raw() <= 0 {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "amount",
                value: declared.amount().raw().to_string(),
            });
        }
        Ok(())
    }

    /// Transfer: two opposing monetary legs on the declared accounts.
    fn validate_transfer(
        &self,
        name: &'static str,
        from: AccountId,
        to: AccountId,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        // Checked BEFORE parsing the legs. Otherwise the rejection reason would depend on their
        // count: with two legs both `find` calls would return the same one, the residual
        // would double and the rejection would be `TransferResidual` — for an accidental
        // reason; while two zero legs would produce zero residual and the event
        // would pass validation.
        if from == to {
            return Err(EventValidationError::TransferToSelf { account: from });
        }
        let legs = self.cash_legs();
        if legs.len() != 2 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "exactly two monetary legs",
                found: legs.len(),
            });
        }
        let out = legs
            .iter()
            .find(|l| l.account == from)
            .ok_or(EventValidationError::WrongAccount { expected: from })?;
        let inn = legs
            .iter()
            .find(|l| l.account == to)
            .ok_or(EventValidationError::WrongAccount { expected: to })?;
        let out_money = leg_money(name, out)?;
        let in_money = leg_money(name, inn)?;
        let residual = out_money.try_add(in_money)?;
        if !residual.is_zero() {
            return Err(EventValidationError::TransferResidual {
                residual: residual.amount().raw(),
            });
        }
        require_equal(name, in_money, declared)
    }

    /// Trade: exactly one monetary and exactly one security leg, the monetary
    /// leg equals the settlement amount with the direction sign, **and the security
    /// leg says exactly the same thing as the event type**.
    ///
    /// The latter is not pedantry. Without this check an event saying «bought one hundred
    /// units of security X», whose leg credits one unit of security Y to another account, passes
    /// validation and enters the append-only journal forever. The projection
    /// invariant will stop the report, but the recorded fact can only be fixed
    /// by reversal: the input gate must reject
    /// the contradiction, not preserve it (§4.3, §4.8).
    ///
    /// A basis fee is retained both as the posted minor-unit amount and as its exact
    /// source value. Requiring the exact value to round half away from zero to the
    /// posted amount proves the journal cannot preserve two contradictory basis
    /// amounts that later lot accounting would interpret differently.
    fn validate_trade(
        &self,
        name: &'static str,
        side: TradeSide,
        declared: TradeDeclaration,
    ) -> Result<(), EventValidationError> {
        let TradeDeclaration {
            instrument,
            quantity,
            gross,
            fee,
            accrued_interest,
            basis_fee,
            basis_fee_exact,
        } = declared;
        require_positive(name, "gross", gross.amount().raw())?;
        require_positive_quantity(name, "quantity", quantity)?;
        if let Some(basis_fee) = basis_fee {
            require_positive(name, "basis_fee", basis_fee.amount().raw())?;
            if basis_fee.currency() != gross.currency() {
                return Err(EventValidationError::Money(MoneyError::CurrencyMismatch {
                    left: basis_fee.currency(),
                    right: gross.currency(),
                }));
            }
        }
        if let Some(basis_fee_exact) = basis_fee_exact {
            if basis_fee_exact.currency() != gross.currency() {
                return Err(EventValidationError::Money(MoneyError::CurrencyMismatch {
                    left: basis_fee_exact.currency(),
                    right: gross.currency(),
                }));
            }
        }
        match (basis_fee, basis_fee_exact) {
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                return Err(EventValidationError::BasisFeePresenceMismatch { kind: name });
            }
            (Some(basis_fee), Some(basis_fee_exact)) => {
                // Both fields are one source fact: the posted amount and its exact
                // audit value. Requiring the exact value to round to the posted
                // value proves the journal cannot preserve two contradictory basis
                // amounts that later lot accounting would interpret differently.
                let exact_rounded = basis_fee_exact.rounded_minor()?;
                if exact_rounded != basis_fee.amount() {
                    return Err(EventValidationError::BasisFeeAmountMismatch {
                        kind: name,
                        posted: basis_fee.amount().raw(),
                        exact: exact_rounded.raw(),
                    });
                }
            }
        }

        let cash = self.cash_legs();
        let cash_money = single_leg_money(name, &cash, "exactly one monetary leg")?;
        require_own_account(name, cash[0].account, self.account)?;
        let expected = trade_settlement(side, gross, fee, accrued_interest)?;
        require_equal(name, cash_money, expected)?;

        let security = self.security_legs();
        if security.len() != 1 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "exactly one security leg",
                found: security.len(),
            });
        }
        let leg = security[0];
        require_own_account(name, leg.account, self.account)?;
        require_same_instrument(name, leg.instrument, instrument)?;

        // A purchase increases the position, a sale decreases it. Shorts are out of
        // scope (§11), so the direction determines the sign unambiguously.
        let expected_quantity = match side {
            TradeSide::Buy => quantity,
            TradeSide::Sell => Quantity(quantity.0.checked_neg()?),
        };
        match leg.quantity {
            Some(actual) if actual == expected_quantity => Ok(()),
            _ => Err(EventValidationError::LegDoesNotMatchEvent {
                kind: name,
                field: "quantity",
            }),
        }
    }

    /// Corporate action shape (§4.7).
    ///
    /// Legs are enumerated **exactly**: an extraneous leg is rejected just like
    /// a missing one, — an event with movement it does not name
    /// is not the event it claims to be.
    fn validate_corporate_action(
        &self,
        name: &'static str,
        action: &CorporateAction,
    ) -> Result<(), EventValidationError> {
        match action {
            // Amortization pays cash, but the number of securities
            // does not change (§6.5). Hence **one** `Principal` leg and no
            // security leg: «quantity does not decrease» becomes
            // a shape invariant, not a promise.
            //
            // There is intentionally no «Cash + Principal» pair here: `Principal`
            // is already included in `cash_effect()` (`leg.rs`), and the pair would produce
            // a double monetary effect.
            CorporateAction::PartialRedemption {
                instrument,
                quantity,
                principal_returned_per_unit,
                compensation,
                ..
            } => {
                require_positive(name, "compensation", compensation.amount().raw())?;
                require_positive_quantity(name, "quantity", *quantity)?;
                // Principal repayment is checked here, not in the allocation
                // rule: that rule uses a dimensionless fraction and
                // no longer sees the event's raw monetary assertion.
                require_positive_per_unit(
                    name,
                    "principal_returned_per_unit",
                    *principal_returned_per_unit,
                )?;
                self.expect_legs(
                    name,
                    &[principal_leg(self.account, *instrument, *compensation)],
                )
            }
            // Redemption repays the principal in full, and the security leaves the position.
            // Zeroing the balance while retaining the quantity would create a position
            // in redeemed securities, which does not exist.
            CorporateAction::Redemption {
                instrument,
                custody,
                quantity,
                compensation,
                ..
            } => {
                require_positive(name, "compensation", compensation.amount().raw())?;
                require_positive_quantity(name, "quantity", *quantity)?;
                self.expect_legs(
                    name,
                    &[
                        principal_leg(self.account, *instrument, *compensation),
                        security_leg(
                            self.account,
                            *custody,
                            *instrument,
                            Quantity(quantity.0.checked_neg()?),
                        ),
                    ],
                )
            }
            CorporateAction::Conversion {
                predecessor,
                successor,
                custody,
                ratio,
                quantity_in,
                quantity_out,
                fractional,
                compensation,
                ..
            } => {
                require_positive_quantity(name, "quantity_in", *quantity_in)?;
                require_positive_quantity(name, "quantity_out", *quantity_out)?;
                require_positive_quantity(name, "ratio", Quantity(*ratio))?;
                require_conversion_ratio(name, *ratio, *quantity_in, *quantity_out, *fractional)?;
                require_fraction_compensation(name, *fractional, *compensation)?;
                let mut expected = vec![
                    security_leg(
                        self.account,
                        *custody,
                        *predecessor,
                        Quantity(quantity_in.0.checked_neg()?),
                    ),
                    security_leg(self.account, *custody, *successor, *quantity_out),
                ];
                if let Some(compensation) = compensation {
                    expected.push(cash_leg(self.account, *compensation));
                }
                self.expect_legs(name, &expected)
            }
        }
    }

    /// Offer fact shape (§3.5).
    fn validate_offer_exercise(
        &self,
        name: &'static str,
        action: &OfferExerciseAction,
    ) -> Result<(), EventValidationError> {
        require_positive_quantity(name, "quantity", action.quantity())?;
        match action {
            // Submission and withdrawal have no legs: they move neither money nor
            // securities — like a control assertion. Having no legs is
            // also a shape, and it is validated like the others.
            OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => {
                self.expect_legs(name, &[])
            }
            // Buyout: cash and a negative quantity. There is no `Principal`
            // leg — the security leaves the position rather than repaying principal.
            OfferExerciseAction::Settled {
                submission: _,
                instrument,
                custody,
                quantity,
                gross,
                fee,
                accrued_interest,
            } => {
                require_positive(name, "gross", gross.amount().raw())?;
                let settlement =
                    trade_settlement(TradeSide::Sell, *gross, *fee, *accrued_interest)?;
                self.expect_legs(
                    name,
                    &[
                        cash_leg(self.account, settlement),
                        security_leg(
                            self.account,
                            *custody,
                            *instrument,
                            Quantity(quantity.0.checked_neg()?),
                        ),
                    ],
                )
            }
        }
    }

    /// A reconstructed position describes only the security: no money moved in this
    /// event, otherwise reconstructing the balance would look
    /// like an actual purchase (§10.7).
    fn validate_opening_position(
        &self,
        name: &'static str,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Result<(), EventValidationError> {
        require_positive_quantity(name, "quantity", quantity)?;
        let cash = self.cash_legs();
        if !cash.is_empty() {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "no monetary legs",
                found: cash.len(),
            });
        }
        let security = self.security_legs();
        if security.len() != 1 {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "exactly one security leg",
                found: security.len(),
            });
        }
        let leg = security[0];
        require_own_account(name, leg.account, self.account)?;
        require_same_instrument(name, leg.instrument, instrument)?;
        match leg.quantity {
            Some(actual) if actual == quantity => Ok(()),
            _ => Err(EventValidationError::LegDoesNotMatchEvent {
                kind: name,
                field: "quantity",
            }),
        }
    }

    /// A valuation moves neither money nor securities: it is a price assertion.
    /// A leg here would mean that someone recorded revaluation as a movement
    /// fact, — but an unrealized result is not a movement.
    fn validate_valuation(
        &self,
        name: &'static str,
        price: crate::numeric::decimal::Dec,
    ) -> Result<(), EventValidationError> {
        // A zero or negative price produces a negative position value
        // and superficially plausible returns. A security may
        // become worthless — but that is a delisting fact (E3), not a price.
        if !price.is_positive() {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "price",
                value: price.inner().to_string(),
            });
        }
        if self.legs.is_empty() {
            Ok(())
        } else {
            Err(EventValidationError::LegCount {
                kind: name,
                expected: "no legs",
                found: self.legs.len(),
            })
        }
    }

    /// Control assertion: no legs, a valid interval, and values
    /// that must be magnitudes, — nonnegative.
    ///
    /// A negative cash balance is intentionally allowed: it is
    /// a valid state (§11). A negative security quantity is not:
    /// shorts are out of scope, and a minus here means either a short or
    /// a reversed sign during parsing.
    fn validate_control_assertion(
        &self,
        name: &'static str,
        period: crate::reconciliation::claim::AssertionPeriod,
        claim: crate::reconciliation::claim::ControlClaim,
    ) -> Result<(), EventValidationError> {
        use crate::reconciliation::claim::ControlClaim;

        if !period.is_well_formed() {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "period",
                value: format!("{} .. {}", period.from, period.to),
            });
        }
        if let ControlClaim::PositionQuantity { quantity, .. } = claim
            && quantity.0.is_negative()
        {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "quantity",
                value: quantity.0.inner().to_string(),
            });
        }
        if let Some((field, value)) = claim.non_negative_field()
            && value < 0
        {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field,
                value: value.to_string(),
            });
        }
        if self.legs.is_empty() {
            Ok(())
        } else {
            Err(EventValidationError::LegCount {
                kind: name,
                expected: "no legs",
                found: self.legs.len(),
            })
        }
    }

    fn validate_import_coverage_gap(
        &self,
        name: &'static str,
        period: crate::reconciliation::claim::AssertionPeriod,
        dimensions: &std::collections::BTreeSet<crate::reconciliation::Dimension>,
        refused: u32,
        rows: &[crate::event::source_row::RefusedRow],
    ) -> Result<(), EventValidationError> {
        if !period.is_well_formed() {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "period",
                value: format!("{} .. {}", period.from, period.to),
            });
        }
        if refused < 1 {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "refused",
                value: refused.to_string(),
            });
        }
        if dimensions.is_empty() {
            return Err(EventValidationError::EmptySet {
                kind: name,
                field: "dimensions",
            });
        }
        if !self.legs.is_empty() {
            return Err(EventValidationError::LegCount {
                kind: name,
                expected: "no legs",
                found: self.legs.len(),
            });
        }

        // Schema-aware on purpose. `validate_structure` runs on the READ path
        // too: the projection re-checks every effective event because the core
        // does not trust storage it did not write (crates/iaam-core/src/
        // projection/invariants.rs). Refusing an empty `rows` outright would
        // make every report fail on a journal that holds a gap written before
        // schema 8.
        if self.schema_version < 8 {
            return Ok(());
        }
        if rows.is_empty() {
            return Err(EventValidationError::EmptySet {
                kind: name,
                field: "rows",
            });
        }
        if rows.len() != refused as usize {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "refused",
                value: format!("{refused} declared, {} rows listed", rows.len()),
            });
        }
        let union: std::collections::BTreeSet<crate::reconciliation::Dimension> = rows
            .iter()
            .flat_map(|row| row.dimensions.iter().copied())
            .collect();
        if &union != dimensions {
            return Err(EventValidationError::DimensionsMismatch {
                kind: name,
                field: "rows",
                expected: format!("{dimensions:?}"),
                actual: format!("{union:?}"),
            });
        }
        Ok(())
    }
}

/// Trade settlement amount with the cash-leg sign (§7.2).
///
/// Principal plus accrued interest, then the fee: on purchase it increases the debit,
/// on sale it reduces the credit. The sign is set by trade direction —
/// a purchase debits cash, a sale credits it.
/// Expected outstanding principal leg.
fn principal_leg(account: AccountId, instrument: InstrumentId, money: Money) -> LegExpectation {
    LegExpectation {
        kind: LegKind::Principal,
        account,
        instrument: Some(instrument),
        custody: None,
        money: Some(money),
        quantity: None,
    }
}

/// Expected signed security leg.
fn security_leg(
    account: AccountId,
    custody: CustodyId,
    instrument: InstrumentId,
    quantity: Quantity,
) -> LegExpectation {
    LegExpectation {
        kind: LegKind::SecurityQuantity,
        account,
        instrument: Some(instrument),
        custody: Some(custody),
        money: None,
        quantity: Some(quantity),
    }
}

/// Expected monetary leg.
fn cash_leg(account: AccountId, money: Money) -> LegExpectation {
    LegExpectation {
        kind: LegKind::Cash,
        account,
        instrument: None,
        custody: None,
        money: Some(money),
        quantity: None,
    }
}

/// The replacement ratio is checked against the pair of quantities.
///
/// Without this check the ratio is an optional label on the numbers, while E5
/// will use it to transfer tax basis. The fractional part
/// is handled according to what was done with it: when the fraction is bought out or discarded,
/// the successor quantity is rounded down, and requiring exact
/// equality would reject valid replacements.
fn require_conversion_ratio(
    name: &'static str,
    ratio: Dec,
    quantity_in: Quantity,
    quantity_out: Quantity,
    fractional: FractionalTreatment,
) -> Result<(), EventValidationError> {
    let implied = ratio.checked_mul(quantity_in.0)?;
    let expected = match fractional {
        FractionalTreatment::NotApplicable => implied,
        FractionalTreatment::CashCompensated | FractionalTreatment::RoundedDown => {
            Dec::new(implied.inner().floor())
        }
    };
    if quantity_out.0 == expected {
        Ok(())
    } else {
        Err(EventValidationError::LegDoesNotMatchEvent {
            kind: name,
            field: "ratio",
        })
    }
}

/// Fractional compensation exists if and only if the fraction was bought out.
fn require_fraction_compensation(
    name: &'static str,
    fractional: FractionalTreatment,
    compensation: Option<Money>,
) -> Result<(), EventValidationError> {
    let expected = match fractional {
        FractionalTreatment::CashCompensated => true,
        // The fraction was discarded or never arose — there is nothing to pay for.
        FractionalTreatment::RoundedDown | FractionalTreatment::NotApplicable => false,
    };
    if compensation.is_some() == expected {
        Ok(())
    } else {
        Err(EventValidationError::LegDoesNotMatchEvent {
            kind: name,
            field: "compensation",
        })
    }
}

fn trade_settlement(
    side: TradeSide,
    gross: Money,
    fee: Option<Money>,
    accrued_interest: Option<Money>,
) -> Result<Money, MoneyError> {
    let mut settlement = gross;
    if let Some(ai) = accrued_interest {
        settlement = settlement.try_add(ai)?;
    }
    match side {
        TradeSide::Buy => {
            let with_fee = match fee {
                Some(f) => settlement.try_add(f)?,
                None => settlement,
            };
            with_fee.checked_negate()
        }
        TradeSide::Sell => match fee {
            Some(f) => settlement.try_sub(f),
            None => Ok(settlement),
        },
    }
}

fn leg_money(name: &'static str, leg: &Leg) -> Result<Money, EventValidationError> {
    leg.money.ok_or(EventValidationError::LegCount {
        kind: name,
        expected: "leg with the specified amount",
        found: 0,
    })
}

/// Declared trade terms. A separate structure because the
/// `too-many-arguments-threshold = 6` applies, and the lint cannot be suppressed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TradeDeclaration {
    instrument: InstrumentId,
    quantity: Quantity,
    gross: Money,
    fee: Option<Money>,
    accrued_interest: Option<Money>,
    basis_fee: Option<Money>,
    basis_fee_exact: Option<crate::money::CalcMoney>,
}

fn require_positive(
    name: &'static str,
    field: &'static str,
    value: i64,
) -> Result<(), EventValidationError> {
    if value > 0 {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: value.to_string(),
        })
    }
}

fn require_positive_per_unit(
    name: &'static str,
    field: &'static str,
    amount: crate::money::PerUnitAmount,
) -> Result<(), EventValidationError> {
    if amount.value().is_positive() {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: amount.value().inner().to_string(),
        })
    }
}

fn require_positive_quantity(
    name: &'static str,
    field: &'static str,
    quantity: Quantity,
) -> Result<(), EventValidationError> {
    if quantity.0.is_positive() {
        Ok(())
    } else {
        Err(EventValidationError::NonPositive {
            kind: name,
            field,
            value: quantity.0.inner().to_string(),
        })
    }
}

/// A leg must belong to the event account: otherwise one event would move
/// securities in another account, while lots would be calculated in its own.
fn require_own_account(
    name: &'static str,
    leg: AccountId,
    event: AccountId,
) -> Result<(), EventValidationError> {
    if leg == event {
        Ok(())
    } else {
        let _ = name;
        Err(EventValidationError::WrongAccount { expected: event })
    }
}

fn require_same_instrument(
    name: &'static str,
    leg: Option<InstrumentId>,
    declared: InstrumentId,
) -> Result<(), EventValidationError> {
    match leg {
        Some(actual) if actual == declared => Ok(()),
        _ => Err(EventValidationError::LegDoesNotMatchEvent {
            kind: name,
            field: "instrument",
        }),
    }
}

fn single_leg_money(
    name: &'static str,
    legs: &[&Leg],
    expected: &'static str,
) -> Result<Money, EventValidationError> {
    if legs.len() != 1 {
        return Err(EventValidationError::LegCount {
            kind: name,
            expected,
            found: legs.len(),
        });
    }
    leg_money(name, legs[0])
}

fn require_equal(
    name: &'static str,
    leg: Money,
    declared: Money,
) -> Result<(), EventValidationError> {
    if leg.currency() != declared.currency() {
        return Err(EventValidationError::Money(MoneyError::CurrencyMismatch {
            left: leg.currency(),
            right: declared.currency(),
        }));
    }
    if leg.amount().raw() != declared.amount().raw() {
        return Err(EventValidationError::AmountMismatch {
            kind: name,
            legs: leg.amount().raw(),
            declared: declared.amount().raw(),
        });
    }
    Ok(())
}

/// Event constructors for tests. Also available to other crate modules,
/// so they are kept outside the private `mod tests`.
#[cfg(test)]
pub(crate) mod test_support {
    use super::provenance::{ParserVersion, Provenance, RawHash};
    use super::*;
    use crate::dates::CashPostedDate;
    use crate::ids::SourceId;
    use crate::money::PostedMinor;
    use time::macros::date;

    /// Event of any type for core module tests.
    ///
    /// Exists so projection tests do not rewrite the event envelope
    /// in every module: a manually rewritten envelope can silently diverge
    /// from the real one, and the test starts testing the fixture rather than the code.
    pub(crate) fn event_with(
        account: AccountId,
        day: time::Date,
        sequence: u32,
        kind: EventKind,
        legs: Vec<Leg>,
    ) -> Event {
        let dates = EventDates::for_cash(CashPostedDate(day));
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates,
            order: EffectiveOrder::new(day, sequence),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"d".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    pub(crate) fn sample_event(sequence: u32) -> Event {
        sample_event_with(sequence, Relation::None)
    }

    pub(crate) fn sample_event_with(sequence: u32, relation: Relation) -> Event {
        let account = AccountId::new_random();
        // The amount is written as one number in minimal units:
        // grouping like `10_000_00` does not compile
        // (`clippy::inconsistent_digit_grouping` is included in `all`, and `all = deny`).
        let amount = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Rub);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 01))),
            order: EffectiveOrder::new(date!(2026 - 03 - 01), sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"b".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::kind::{FeeOrigin, TaxOrigin, TradeSide};
    use super::provenance::{ParserVersion, RawHash};
    use super::source_row::{RefusedRow, RowName, SourceRowKey};
    use super::*;
    use crate::dates::CashPostedDate;
    use crate::ids::{CustodyId, InstrumentId, SourceId, TransferId};
    use crate::money::{CalcMoney, PostedMinor, Quantity};
    use crate::reconciliation::Dimension;
    use time::macros::date;

    // Amounts are written in minimal units as one number: grouping
    // like `50_000_00` does not compile (`clippy::inconsistent_digit_grouping`
    // is included in `all`, and `all = deny`).
    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn usd(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Usd)
    }

    fn event(kind: EventKind, legs: Vec<Leg>, account: AccountId) -> Event {
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(date!(2026 - 03 - 01))),
            order: EffectiveOrder::new(date!(2026 - 03 - 01), 0),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"a".repeat(64)).unwrap(),
                ParserVersion("manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }
    fn march_period() -> crate::reconciliation::claim::AssertionPeriod {
        crate::reconciliation::claim::AssertionPeriod::between(
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        )
        .expect("well-formed period")
    }

    fn row_key(name: &str) -> SourceRowKey {
        SourceRowKey {
            source: SourceId::new_random(),
            row: RowName::Given(name.to_owned()),
        }
    }

    fn qty(units: i64) -> Quantity {
        Quantity(crate::numeric::decimal::Dec::new(
            rust_decimal::Decimal::from(units),
        ))
    }

    fn calc_rub(mantissa: i64, scale: u32) -> CalcMoney {
        CalcMoney::new(
            crate::numeric::decimal::Dec::new(rust_decimal::Decimal::new(mantissa, scale)),
            CurrencyCode::Rub,
        )
    }

    fn basis_trade(
        basis_fee: Option<Money>,
        basis_fee_exact: Option<CalcMoney>,
        cash: Money,
    ) -> Event {
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: Some(rub(120_000)),
                basis_fee,
                basis_fee_exact,
            },
            vec![
                Leg::cash(account, cash),
                security_leg(account, instrument, qty(100)),
            ],
            account,
        )
    }

    // --- shape of new facts (§4.7, §3.5) ---

    struct Bond {
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
    }

    impl Bond {
        fn new() -> Self {
            Self {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
            }
        }

        fn per_unit(text: &str) -> crate::money::PerUnitAmount {
            crate::money::PerUnitAmount::new(
                crate::numeric::decimal::Dec::new(
                    rust_decimal::Decimal::from_str_exact(text).unwrap(),
                ),
                CurrencyCode::Rub,
            )
        }

        fn amortisation(&self, legs: Vec<Leg>) -> Event {
            self.amortisation_returning("200", legs)
        }

        fn amortisation_returning(&self, returned: &str, legs: Vec<Leg>) -> Event {
            event(
                EventKind::CorporateAction {
                    action: CorporateAction::PartialRedemption {
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(10),
                        principal_returned_per_unit: Self::per_unit(returned),
                        compensation: rub(100_000),
                        effective_date: date!(2026 - 06 - 15),
                        record_date: None,
                        grounds: None,
                        basis_allocation: crate::event::allocation::BasisAllocation::default(),
                    },
                },
                legs,
                self.account,
            )
        }

        fn redemption(&self, legs: Vec<Leg>) -> Event {
            event(
                EventKind::CorporateAction {
                    action: CorporateAction::Redemption {
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(10),
                        principal_returned_per_unit: Self::per_unit("800"),
                        compensation: rub(1_000_000),
                        effective_date: date!(2026 - 12 - 15),
                        record_date: None,
                        grounds: None,
                    },
                },
                legs,
                self.account,
            )
        }

        fn offer_settled(&self, legs: Vec<Leg>) -> Event {
            event(
                EventKind::OfferExercise {
                    action: offer::OfferExerciseAction::Settled {
                        submission: offer::OfferSubmissionId::new_random(),
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(10),
                        gross: rub(1_000_000),
                        fee: None,
                        accrued_interest: None,
                    },
                },
                legs,
                self.account,
            )
        }
    }

    #[test]
    fn amortisation_carries_one_principal_leg_and_nothing_else() {
        let bond = Bond::new();
        assert_eq!(
            bond.amortisation(vec![Leg::principal(
                bond.account,
                bond.instrument,
                rub(100_000)
            )])
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_partial_redemption_returning_nothing_is_rejected() {
        let bond = Bond::new();
        let event = bond.amortisation_returning(
            "0",
            vec![Leg::principal(bond.account, bond.instrument, rub(100_000))],
        );
        // The field is named explicitly: rejection on compensation or quantity would pass
        // this test without proving anything about principal repayment.
        assert!(matches!(
            event.validate_structure().unwrap_err(),
            EventValidationError::NonPositive { field, .. } if field == "principal_returned_per_unit"
        ));
    }

    #[test]
    fn a_partial_redemption_returning_a_negative_principal_is_rejected() {
        let bond = Bond::new();
        let event = bond.amortisation_returning(
            "-100",
            vec![Leg::principal(bond.account, bond.instrument, rub(100_000))],
        );
        assert!(matches!(
            event.validate_structure().unwrap_err(),
            EventValidationError::NonPositive { field, .. } if field == "principal_returned_per_unit"
        ));
    }

    #[test]
    fn amortisation_with_a_security_quantity_leg_is_rejected() {
        // §6.5: amortization pays cash, but does not change the quantity.
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![
                Leg::principal(bond.account, bond.instrument, rub(100_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn amortisation_with_a_cash_leg_is_rejected() {
        // `Principal` is already a monetary leg: a pair would produce a double effect.
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![
                Leg::principal(bond.account, bond.instrument, rub(100_000)),
                Leg::cash(bond.account, rub(100_000)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn a_principal_leg_for_another_bond_is_rejected() {
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![Leg::principal(
                bond.account,
                InstrumentId::new_random(),
                rub(100_000),
            )])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn a_principal_leg_of_another_amount_is_rejected() {
        let bond = Bond::new();
        assert!(
            bond.amortisation(vec![Leg::principal(
                bond.account,
                bond.instrument,
                rub(99_999)
            )])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn final_redemption_carries_the_principal_and_the_leaving_quantity() {
        let bond = Bond::new();
        assert_eq!(
            bond.redemption(vec![
                Leg::principal(bond.account, bond.instrument, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ])
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn final_redemption_without_a_security_leg_is_rejected() {
        // Zeroing the principal while retaining the quantity would create a position
        // in redeemed securities, which does not exist.
        let bond = Bond::new();
        assert!(
            bond.redemption(vec![Leg::principal(
                bond.account,
                bond.instrument,
                rub(1_000_000)
            )])
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn final_redemption_with_a_positive_security_leg_is_rejected() {
        // The sign is not a typo: a positive quantity means a security
        // inflow, that is, the opposite movement.
        let bond = Bond::new();
        assert!(
            bond.redemption(vec![
                Leg::principal(bond.account, bond.instrument, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(10)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    /// Replacement sides. A separate structure: the
    /// `too-many-arguments-threshold = 6` also applies in tests.
    #[derive(Debug, Clone, Copy)]
    struct Swap {
        account: AccountId,
        predecessor: InstrumentId,
        successor: InstrumentId,
        custody: CustodyId,
    }

    impl Swap {
        fn new() -> Self {
            Self {
                account: AccountId::new_random(),
                predecessor: InstrumentId::new_random(),
                successor: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
            }
        }
    }

    fn conversion(
        swap: Swap,
        ratio: &str,
        quantity_out: i64,
        fractional: corporate_action::FractionalTreatment,
        compensation: Option<Money>,
        legs: Vec<Leg>,
    ) -> Event {
        event(
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: swap.predecessor,
                    successor: swap.successor,
                    custody: swap.custody,
                    ratio: crate::numeric::decimal::Dec::new(
                        rust_decimal::Decimal::from_str_exact(ratio).unwrap(),
                    ),
                    quantity_in: qty(10),
                    quantity_out: qty(quantity_out),
                    fractional,
                    compensation,
                    effective_date: date!(2026 - 09 - 01),
                    record_date: None,
                    grounds: None,
                    basis_transfer: corporate_action::BasisTransferRule::CarryOver,
                },
            },
            legs,
            swap.account,
        )
    }

    #[test]
    fn a_conversion_moves_the_quantity_between_two_instruments() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.5",
                15,
                corporate_action::FractionalTreatment::NotApplicable,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_conversion_whose_ratio_contradicts_the_quantities_is_rejected() {
        // The ratio is not a label on the numbers: E5 uses it to transfer
        // tax basis.
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "2",
                15,
                corporate_action::FractionalTreatment::NotApplicable,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                kind: "corporate_action",
                field: "ratio",
            })
        );
    }

    #[test]
    fn a_rounded_down_conversion_may_end_below_the_exact_ratio() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.55",
                15,
                corporate_action::FractionalTreatment::RoundedDown,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_cash_leg_without_a_bought_out_fraction_is_rejected() {
        // Cash in a replacement can only be fractional compensation.
        let swap = Swap::new();
        assert!(
            conversion(
                swap,
                "1.5",
                15,
                corporate_action::FractionalTreatment::NotApplicable,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                    Leg::cash(swap.account, rub(500)),
                ],
            )
            .validate_structure()
            .is_err()
        );
    }

    #[test]
    fn a_bought_out_fraction_without_compensation_is_rejected() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.55",
                15,
                corporate_action::FractionalTreatment::CashCompensated,
                None,
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                ],
            )
            .validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                kind: "corporate_action",
                field: "compensation",
            })
        );
    }

    #[test]
    fn a_bought_out_fraction_carries_its_cash_leg() {
        let swap = Swap::new();
        assert_eq!(
            conversion(
                swap,
                "1.55",
                15,
                corporate_action::FractionalTreatment::CashCompensated,
                Some(rub(500)),
                vec![
                    Leg::security(swap.account, swap.custody, swap.predecessor, qty(-10)),
                    Leg::security(swap.account, swap.custody, swap.successor, qty(15)),
                    Leg::cash(swap.account, rub(500)),
                ],
            )
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_submitted_offer_moves_nothing() {
        let bond = Bond::new();
        let submitted = event(
            EventKind::OfferExercise {
                action: offer::OfferExerciseAction::Submitted {
                    submission: offer::OfferSubmissionId::new_random(),
                    window: offer::OfferWindowId::new_random(),
                    instrument: bond.instrument,
                    quantity: qty(10),
                },
            },
            Vec::new(),
            bond.account,
        );
        assert_eq!(submitted.validate_structure(), Ok(()));
    }

    #[test]
    fn a_submitted_offer_with_a_leg_is_rejected() {
        let bond = Bond::new();
        let submitted = event(
            EventKind::OfferExercise {
                action: offer::OfferExerciseAction::Submitted {
                    submission: offer::OfferSubmissionId::new_random(),
                    window: offer::OfferWindowId::new_random(),
                    instrument: bond.instrument,
                    quantity: qty(10),
                },
            },
            vec![Leg::cash(bond.account, rub(1))],
            bond.account,
        );
        assert!(submitted.validate_structure().is_err());
    }

    #[test]
    fn a_settled_offer_carries_cash_and_the_leaving_quantity() {
        let bond = Bond::new();
        assert_eq!(
            bond.offer_settled(vec![
                Leg::cash(bond.account, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ])
            .validate_structure(),
            Ok(())
        );
    }

    #[test]
    fn a_settled_offer_has_no_principal_leg() {
        // The security leaves the position rather than repaying principal.
        let bond = Bond::new();
        assert!(
            bond.offer_settled(vec![
                Leg::cash(bond.account, rub(1_000_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
                Leg::principal(bond.account, bond.instrument, rub(1)),
            ])
            .validate_structure()
            .is_err()
        );
    }

    fn security_leg(account: AccountId, instrument: InstrumentId, quantity: Quantity) -> Leg {
        Leg::security(account, CustodyId::new_random(), instrument, quantity)
    }

    // --- Common test constructors ---

    #[test]
    fn sample_event_passes_structural_validation() {
        // The constructor from `test_support` is used by other crate modules
        // as a «normal event». An event that fails structural validation
        // is unsuitable for this role: correction tests would rely on a fact
        // that the journal would not accept.
        assert!(test_support::sample_event(0).validate_structure().is_ok());
    }

    // --- Fee ---

    #[test]
    fn fee_with_a_single_negative_leg_is_valid() {
        // A fee is one actual leg. The legs do not sum to zero,
        // and that is correct: the model has no expense counteraccount.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(-3_500),
                origin: FeeOrigin::Brokerage,
            },
            vec![Leg::fee(acc, rub(-3_500))],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn positive_fee_is_rejected() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(3_500),
                origin: FeeOrigin::Brokerage,
            },
            vec![Leg::fee(acc, rub(3_500))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    #[test]
    fn zero_fee_is_rejected() {
        // A zero fee is not a cash fact, but a missing source field.
        // The boundary is strict: `>= 0` rejects, `> 0` would allow it.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(0),
                origin: FeeOrigin::Depositary,
            },
            vec![Leg::fee(acc, rub(0))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongSign { amount: 0, .. })
        ));
    }

    #[test]
    fn fee_leg_must_match_the_declared_amount() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Fee {
                amount: rub(-3_500),
                origin: FeeOrigin::MarginInterest,
            },
            vec![Leg::fee(acc, rub(-3_600))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::AmountMismatch {
                legs: -3_600,
                declared: -3_500,
                ..
            })
        ));
    }

    #[test]
    fn fee_needs_exactly_one_fee_leg() {
        let acc = AccountId::new_random();
        let kind = EventKind::Fee {
            amount: rub(-3_500),
            origin: FeeOrigin::Other,
        };
        let none = event(kind.clone(), vec![Leg::cash(acc, rub(-3_500))], acc);
        assert!(matches!(
            none.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));

        let doubled = event(
            kind,
            vec![Leg::fee(acc, rub(-1_750)), Leg::fee(acc, rub(-1_750))],
            acc,
        );
        assert!(matches!(
            doubled.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    // --- Tax ---

    fn tax_event(amount: Money, leg: Money) -> Event {
        let account = AccountId::new_random();
        event(
            EventKind::Tax {
                amount,
                origin: TaxOrigin::SelfPaid,
            },
            vec![Leg::tax(account, leg)],
            account,
        )
    }

    #[test]
    fn a_tax_matches_its_single_negative_leg() {
        let event = tax_event(rub(-130_000), rub(-130_000));
        assert!(event.validate_structure().is_ok());
    }

    #[test]
    fn a_tax_positive_leg_is_rejected() {
        // A tax that increases the balance is not a tax. Taking the absolute
        // value here is how a refund silently becomes a charge.
        let event = tax_event(rub(130_000), rub(130_000));
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    #[test]
    fn a_tax_without_a_tax_leg_is_rejected() {
        let account = AccountId::new_random();
        let event = event(
            EventKind::Tax {
                amount: rub(-130_000),
                origin: TaxOrigin::WithheldAtSource,
            },
            vec![Leg::cash(account, rub(-130_000))],
            account,
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
    }

    #[test]
    fn a_tax_with_two_tax_legs_is_rejected() {
        let account = AccountId::new_random();
        let event = event(
            EventKind::Tax {
                amount: rub(-130_000),
                origin: TaxOrigin::SelfPaid,
            },
            vec![
                Leg::tax(account, rub(-65_000)),
                Leg::tax(account, rub(-65_000)),
            ],
            account,
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    #[test]
    fn a_tax_names_itself_and_stays_within_the_account() {
        let kind = EventKind::Tax {
            amount: rub(-1),
            origin: TaxOrigin::SelfPaid,
        };
        assert_eq!(kind.discriminant(), "tax");
        // WithinAccount, exactly like Fee: a tax is a cost borne by the
        // contour, not money crossing its boundary. Calling it an external
        // outflow would understate contributions in the returns path.
        assert_eq!(kind.flow_endpoints(), kind::FlowEndpoints::WithinAccount);
    }

    // --- External cash ---

    #[test]
    fn cash_in_must_be_positive_and_match_the_declared_amount() {
        let acc = AccountId::new_random();
        let ok = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let negative = event(
            EventKind::CashIn {
                amount: rub(-5_000_000),
            },
            vec![Leg::cash(acc, rub(-5_000_000))],
            acc,
        );
        assert!(matches!(
            negative.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));

        let mismatched = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(4_900_000))],
            acc,
        );
        assert!(matches!(
            mismatched.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));
    }

    #[test]
    fn zero_cash_in_is_rejected() {
        // Zero is not an inflow. The boundary is strict: `> 0`, not `>= 0`.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn { amount: rub(0) },
            vec![Leg::cash(acc, rub(0))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongSign { amount: 0, .. })
        ));
    }

    #[test]
    fn cash_in_needs_exactly_one_cash_leg() {
        let acc = AccountId::new_random();
        let kind = EventKind::CashIn {
            amount: rub(5_000_000),
        };
        let none = event(kind.clone(), vec![], acc);
        assert!(matches!(
            none.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));

        let split = event(
            kind,
            vec![
                Leg::cash(acc, rub(5_000_000)),
                Leg::cash(acc, rub(1_000_000)),
            ],
            acc,
        );
        assert!(matches!(
            split.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    #[test]
    fn cash_out_must_be_negative() {
        let acc = AccountId::new_random();
        let ok = event(
            EventKind::CashOut {
                amount: rub(-5_000_000),
            },
            vec![Leg::cash(acc, rub(-5_000_000))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let positive = event(
            EventKind::CashOut {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        assert!(matches!(
            positive.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));

        let zero = event(
            EventKind::CashOut { amount: rub(0) },
            vec![Leg::cash(acc, rub(0))],
            acc,
        );
        assert!(matches!(
            zero.validate_structure(),
            Err(EventValidationError::WrongSign { amount: 0, .. })
        ));
    }

    #[test]
    fn opening_cash_accepts_either_sign() {
        // A reconstructed balance may be negative (margin
        // debt) or zero: this is a state fact, not a movement.
        let acc = AccountId::new_random();
        for amount in [rub(5_000_000), rub(-5_000_000), rub(0)] {
            let ev = event(
                EventKind::OpeningCash { amount },
                vec![Leg::cash(acc, amount)],
                acc,
            );
            assert!(
                ev.validate_structure().is_ok(),
                "balance {} should be accepted",
                amount.amount().raw()
            );
        }
    }

    #[test]
    fn opening_cash_still_must_match_the_declared_amount() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::OpeningCash {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(4_900_000))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));
    }

    #[test]
    fn income_must_be_a_positive_cash_leg() {
        let acc = AccountId::new_random();
        let ok = event(
            EventKind::Income {
                instrument: Some(InstrumentId::new_random()),
                gross: rub(120_000),
                kind: None,
            },
            vec![Leg::cash(acc, rub(120_000))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());

        let negative = event(
            EventKind::Income {
                instrument: None,
                gross: rub(-120_000),
                kind: None,
            },
            vec![Leg::cash(acc, rub(-120_000))],
            acc,
        );
        assert!(matches!(
            negative.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    // --- Transfer ---

    // --- an own-account movement whose far side nobody named ---------------

    #[test]
    fn an_own_account_movement_carries_one_leg_on_its_own_account() {
        let account = AccountId::new_random();
        for amount in [rub(-250_000), rub(250_000)] {
            let event = event(
                EventKind::OwnAccountMovement { amount },
                vec![Leg::cash(account, amount)],
                account,
            );
            assert!(
                event.validate_structure().is_ok(),
                "both directions are ordinary: {amount:?}"
            );
        }
    }

    #[test]
    fn an_own_account_movement_of_zero_is_not_a_movement() {
        // `Sign::Any` admits zero, and a movement of nothing is not a movement
        // — the same refusal `ObservedRow::magnitude` makes one layer up.
        let account = AccountId::new_random();
        let event = event(
            EventKind::OwnAccountMovement { amount: rub(0) },
            vec![Leg::cash(account, rub(0))],
            account,
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    #[test]
    fn an_own_account_movement_posted_on_another_account_is_refused() {
        // The whole claim is «this account moved and the other side is
        // unnamed». A leg somewhere else makes the event say something no
        // reader could recover, because there is no second account on the fact
        // to check it against.
        let account = AccountId::new_random();
        let elsewhere = AccountId::new_random();
        let amount = rub(-250_000);
        let event = event(
            EventKind::OwnAccountMovement { amount },
            vec![Leg::cash(elsewhere, amount)],
            account,
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::WrongAccount { .. })
        ));
    }

    #[test]
    fn an_unresolved_own_account_movement_posts_nothing() {
        let account = AccountId::new_random();
        let event = event(
            EventKind::UnresolvedOwnAccountMovement {
                amount: rub(250_000),
            },
            Vec::new(),
            account,
        );
        assert!(event.validate_structure().is_ok());
    }

    #[test]
    fn an_unresolved_own_account_movement_with_a_leg_is_refused() {
        // A leg here would be the journal asserting a direction the source
        // never stated. That is the one thing this variant exists so that
        // nobody has to do, so it is refused by the type's own validation
        // rather than left to a convention.
        let account = AccountId::new_random();
        let event = event(
            EventKind::UnresolvedOwnAccountMovement {
                amount: rub(250_000),
            },
            vec![Leg::cash(account, rub(-250_000))],
            account,
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
    }

    #[test]
    fn an_unresolved_own_account_movement_states_a_magnitude() {
        // Negative would be a direction smuggled into the only field left, and
        // stated where nothing reads it.
        let account = AccountId::new_random();
        for amount in [rub(0), rub(-250_000)] {
            let event = event(
                EventKind::UnresolvedOwnAccountMovement { amount },
                Vec::new(),
                account,
            );
            assert!(
                matches!(
                    event.validate_structure(),
                    Err(EventValidationError::NonPositive { .. })
                ),
                "{amount:?} is not a magnitude"
            );
        }
    }

    #[test]
    fn transfer_requires_two_matching_sides() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        let ok = event(
            kind.clone(),
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert!(ok.validate_structure().is_ok());

        // 100 000,00 left, 99 000,00 arrived: residual −1 000,00, that is
        // −100 000 minimal units.
        let lopsided = event(
            kind,
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(9_900_000)),
            ],
            from,
        );
        assert!(matches!(
            lopsided.validate_structure(),
            Err(EventValidationError::TransferResidual { residual: -100_000 })
        ));
    }

    #[test]
    fn transfer_legs_must_sit_on_the_declared_accounts() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let stranger = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        let wrong_source = event(
            kind.clone(),
            vec![
                Leg::cash(stranger, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert_eq!(
            wrong_source.validate_structure(),
            Err(EventValidationError::WrongAccount { expected: from })
        );

        let wrong_target = event(
            kind,
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(stranger, rub(10_000_000)),
            ],
            from,
        );
        assert_eq!(
            wrong_target.validate_structure(),
            Err(EventValidationError::WrongAccount { expected: to })
        );
    }

    #[test]
    fn transfer_needs_exactly_two_cash_legs() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let kind = EventKind::CashTransfer {
            transfer_id: TransferId::new_random(),
            from,
            to,
            amount: rub(10_000_000),
        };
        let one_sided = event(kind.clone(), vec![Leg::cash(from, rub(-10_000_000))], from);
        assert!(matches!(
            one_sided.validate_structure(),
            Err(EventValidationError::LegCount { found: 1, .. })
        ));

        let three = event(
            kind,
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(5_000_000)),
                Leg::cash(to, rub(5_000_000)),
            ],
            from,
        );
        assert!(matches!(
            three.validate_structure(),
            Err(EventValidationError::LegCount { found: 3, .. })
        ));
    }

    #[test]
    fn transfer_to_the_same_account_is_not_a_movement() {
        // The same account on both sides is not a cash movement: no balance
        // changes. The rejection gives the actual reason, not that
        // the leg residual happened not to balance.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: acc,
                to: acc,
                amount: rub(10_000_000),
            },
            vec![
                Leg::cash(acc, rub(-10_000_000)),
                Leg::cash(acc, rub(10_000_000)),
            ],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::TransferToSelf { account: acc })
        );
    }

    #[test]
    fn a_transfer_to_self_of_nothing_is_rejected_too() {
        // Degenerate case: two zero legs on one account produce zero
        // residual, so the balance check would allow the event. Rejection must
        // come from the account check, not leg arithmetic.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: acc,
                to: acc,
                amount: rub(0),
            },
            vec![Leg::cash(acc, rub(0)), Leg::cash(acc, rub(0))],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::TransferToSelf { account: acc })
        );
    }

    #[test]
    fn the_self_transfer_check_runs_before_the_legs_are_read() {
        // There are no legs at all — the rejection still names the real reason,
        // not «exactly two monetary legs expected».
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: acc,
                to: acc,
                amount: rub(10_000_000),
            },
            vec![],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::TransferToSelf { account: acc })
        );
    }

    #[test]
    fn transfer_amount_must_match_the_incoming_side() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount: rub(9_000_000),
            },
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::AmountMismatch {
                legs: 10_000_000,
                declared: 9_000_000,
                ..
            })
        ));
    }

    #[test]
    fn a_principal_leg_does_not_disturb_the_transfer_check() {
        // `LegKind::Principal` is included in `cash_effect`, but transfer validation
        // considers only `Cash` legs: principal amortization must not
        // look like a third transfer party.
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount: rub(10_000_000),
            },
            vec![
                Leg::cash(from, rub(-10_000_000)),
                Leg::cash(to, rub(10_000_000)),
                Leg::principal(from, InstrumentId::new_random(), rub(1)),
            ],
            from,
        );
        assert!(ev.validate_structure().is_ok());
    }

    // --- Trade ---

    /// This is exactly the error class the former «exemption
    /// for events with a security leg» allowed through.
    #[test]
    fn buy_with_the_wrong_cash_sign_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let kind = EventKind::Trade {
            side: TradeSide::Buy,
            instrument,
            quantity: qty(100),
            gross: rub(5_000_000),
            fee: Some(rub(3_500)),
            accrued_interest: None,
            basis_fee: None,
            basis_fee_exact: None,
        };
        // A purchase must debit cash: −50 035,00.
        let wrong = event(
            kind.clone(),
            vec![
                Leg::cash(acc, rub(5_003_500)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(matches!(
            wrong.validate_structure(),
            Err(EventValidationError::AmountMismatch { .. })
        ));

        let right = event(
            kind,
            vec![
                Leg::cash(acc, rub(-5_003_500)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(right.validate_structure().is_ok());
    }

    #[test]
    fn buy_settlement_includes_accrued_interest() {
        // Accrued interest is paid to the seller on top of principal: 50 000 + 1 200 + 35 = 51 235.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: Some(rub(120_000)),
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(-5_123_500)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn sell_settlement_subtracts_the_fee() {
        // Sale: 50 000 + accrued interest 1 200 − fee 35 = 51 165 received.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: Some(rub(120_000)),
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(5_116_500)),
                security_leg(acc, instrument, qty(-100)),
            ],
            acc,
        );
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn a_trade_without_a_fee_settles_at_body_plus_accrued_interest() {
        // There is no fee — the settlement amount must neither add
        // nor subtract its default value.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let buy = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: Some(rub(120_000)),
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(-5_120_000)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(buy.validate_structure().is_ok());

        let sell = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                security_leg(acc, instrument, qty(-100)),
            ],
            acc,
        );
        assert!(sell.validate_structure().is_ok());
    }

    #[test]
    fn the_fee_moves_the_settlement_in_opposite_directions() {
        // The same fee increases the debit on purchase
        // and reduces the credit on sale: 50 035 versus 49 965.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let buy_at_sell_amount = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(-4_996_500)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(matches!(
            buy_at_sell_amount.validate_structure(),
            Err(EventValidationError::AmountMismatch {
                legs: -4_996_500,
                declared: -5_003_500,
                ..
            })
        ));

        let sell = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(4_996_500)),
                security_leg(acc, instrument, qty(-100)),
            ],
            acc,
        );
        assert!(sell.validate_structure().is_ok());
    }

    #[test]
    fn negative_basis_fee_is_rejected_by_field() {
        let ev = basis_trade(
            Some(rub(-3_500)),
            Some(calc_rub(-3_500, 2)),
            rub(-5_123_500),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive { field, .. }) if field == "basis_fee"
        ));
    }

    #[test]
    fn basis_fee_and_exact_must_use_gross_currency() {
        let foreign_posted = basis_trade(
            Some(usd(3_500)),
            Some(CalcMoney::new(
                crate::numeric::decimal::Dec::new(rust_decimal::Decimal::new(3_500, 2)),
                CurrencyCode::Usd,
            )),
            rub(-5_123_500),
        );
        assert!(matches!(
            foreign_posted.validate_structure(),
            Err(EventValidationError::Money(
                MoneyError::CurrencyMismatch { .. }
            ))
        ));

        let foreign_exact = basis_trade(
            Some(rub(3_500)),
            Some(CalcMoney::new(
                crate::numeric::decimal::Dec::new(rust_decimal::Decimal::new(3_500, 2)),
                CurrencyCode::Usd,
            )),
            rub(-5_123_500),
        );
        assert!(matches!(
            foreign_exact.validate_structure(),
            Err(EventValidationError::Money(
                MoneyError::CurrencyMismatch { .. }
            ))
        ));
    }

    #[test]
    fn basis_fee_fields_must_be_present_together() {
        let only_posted = basis_trade(Some(rub(3_500)), None, rub(-5_123_500));
        assert!(matches!(
            only_posted.validate_structure(),
            Err(EventValidationError::BasisFeePresenceMismatch { .. })
        ));

        let only_exact = basis_trade(None, Some(calc_rub(3_500, 2)), rub(-5_123_500));
        assert!(matches!(
            only_exact.validate_structure(),
            Err(EventValidationError::BasisFeePresenceMismatch { .. })
        ));
    }

    #[test]
    fn basis_fee_exact_must_round_to_the_posted_amount() {
        let ev = basis_trade(Some(rub(3_500)), Some(calc_rub(3_501, 2)), rub(-5_123_500));
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::BasisFeeAmountMismatch {
                posted: 3_500,
                exact: 3_501,
                ..
            })
        ));
    }

    #[test]
    fn valid_basis_fee_does_not_change_cash_settlement() {
        let ev = basis_trade(Some(rub(3_501)), Some(calc_rub(35_005, 3)), rub(-5_123_500));
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn trade_without_a_security_leg_is_rejected() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: InstrumentId::new_random(),
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![Leg::cash(acc, rub(-5_000_000))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));
    }

    #[test]
    fn trade_with_two_security_legs_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                security_leg(acc, instrument, qty(100)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    #[test]
    fn trade_without_a_cash_leg_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![security_leg(acc, instrument, qty(100))],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));
    }

    // --- Reconstructed position ---

    #[test]
    fn opening_position_is_a_single_security_leg_without_cash() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ok = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(100),
                cost_basis: Some(rub(5_000_000)),
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, qty(100))],
            acc,
        );
        assert!(ok.validate_structure().is_ok());
    }

    #[test]
    fn opening_position_with_a_cash_leg_is_rejected() {
        // Reconstructing a balance does not move cash: otherwise it would enter
        // cash flow as an actual purchase.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(100),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount { found: 1, .. })
        ));
    }

    #[test]
    fn opening_position_needs_exactly_one_security_leg() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let kind = EventKind::OpeningPosition {
            instrument,
            quantity: qty(100),
            cost_basis: None,
            assertions: kind::OpeningAssertions::default(),
        };
        let none = event(kind.clone(), vec![], acc);
        assert!(matches!(
            none.validate_structure(),
            Err(EventValidationError::LegCount { found: 0, .. })
        ));

        let two = event(
            kind,
            vec![
                security_leg(acc, instrument, qty(100)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert!(matches!(
            two.validate_structure(),
            Err(EventValidationError::LegCount { found: 2, .. })
        ));
    }

    #[test]
    fn a_schema_eight_gap_whose_rows_do_not_cover_its_dimensions_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 8;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash, Dimension::Positions]
                .into_iter()
                .collect(),
            refused: 1,
            rows: vec![RefusedRow {
                key: row_key("OP-1"),
                dimensions: [Dimension::Cash].into_iter().collect(),
            }],
        };
        assert!(event.validate_structure().is_err());
    }

    #[test]
    fn a_schema_eight_gap_whose_row_count_disagrees_with_refused_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 8;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash].into_iter().collect(),
            refused: 2,
            rows: vec![RefusedRow {
                key: row_key("OP-1"),
                dimensions: [Dimension::Cash].into_iter().collect(),
            }],
        };
        assert!(event.validate_structure().is_err());
    }

    #[test]
    fn a_legacy_gap_without_rows_still_validates() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 7;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash].into_iter().collect(),
            refused: 1,
            rows: Vec::new(),
        };
        assert!(event.validate_structure().is_ok());
    }

    #[test]
    fn a_schema_eight_gap_without_rows_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 8;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash].into_iter().collect(),
            refused: 1,
            rows: Vec::new(),
        };
        assert!(event.validate_structure().is_err());
    }

    #[test]
    fn a_legacy_gap_without_rows_still_deserialises() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 7;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash].into_iter().collect(),
            refused: 1,
            rows: vec![RefusedRow {
                key: row_key("OP-1"),
                dimensions: [Dimension::Cash].into_iter().collect(),
            }],
        };
        let mut value = serde_json::to_value(event).unwrap();
        value["kind"]["ImportCoverageGap"]
            .as_object_mut()
            .unwrap()
            .remove("rows");

        let deserialised: Event = serde_json::from_value(value).unwrap();
        assert!(matches!(
            deserialised.kind,
            EventKind::ImportCoverageGap { ref rows, .. } if rows.is_empty()
        ));
        assert!(deserialised.validate_structure().is_ok());
    }

    #[test]
    fn an_import_coverage_gap_requires_at_least_one_refused_row() {
        let account = AccountId::new_random();
        let period = crate::reconciliation::claim::AssertionPeriod::between(
            date!(2026 - 03 - 01),
            date!(2026 - 03 - 31),
        )
        .expect("well-formed period");
        let event = event(
            EventKind::ImportCoverageGap {
                period,
                dimensions: [crate::reconciliation::Dimension::Cash]
                    .into_iter()
                    .collect(),
                refused: 0,
                rows: Vec::new(),
            },
            Vec::new(),
            account,
        );

        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "refused",
                value,
                ..
            }) if value == "0"
        ));
    }

    // --- General shape rules ---

    #[test]
    fn a_leg_of_another_currency_is_not_silently_compared() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, usd(5_000_000))],
            acc,
        );
        assert_eq!(
            ev.validate_structure(),
            Err(EventValidationError::Money(MoneyError::CurrencyMismatch {
                left: CurrencyCode::Usd,
                right: CurrencyCode::Rub,
            }))
        );
    }

    #[test]
    fn a_cash_leg_without_an_amount_is_rejected() {
        // A `Cash` leg must carry an amount: `None` here is not zero,
        // but a missing fact (§4.9).
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg {
                kind: LegKind::Cash,
                account: acc,
                custody: None,
                instrument: None,
                money: None,
                quantity: None,
            }],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount {
                expected: "leg with the specified amount",
                ..
            })
        ));
    }

    #[test]
    fn a_transfer_leg_without_an_amount_is_rejected() {
        let from = AccountId::new_random();
        let to = AccountId::new_random();
        let ev = event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from,
                to,
                amount: rub(10_000_000),
            },
            vec![
                Leg {
                    kind: LegKind::Cash,
                    account: from,
                    custody: None,
                    instrument: None,
                    money: None,
                    quantity: None,
                },
                Leg::cash(to, rub(10_000_000)),
            ],
            from,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegCount {
                expected: "leg with the specified amount",
                ..
            })
        ));
    }

    // --- Event monetary effect ---

    #[test]
    fn cash_effect_sums_every_money_bearing_leg() {
        // A purchase for 50 000 with a fee of 35 reduces the balance by 50 035:
        // the security leg does not move cash, the fee does.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(5_000_000),
                fee: Some(rub(3_500)),
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(-5_000_000)),
                Leg::fee(acc, rub(-3_500)),
                security_leg(acc, instrument, qty(100)),
            ],
            acc,
        );
        assert_eq!(ev.cash_effect(CurrencyCode::Rub), Ok(rub(-5_003_500)));
    }

    #[test]
    fn cash_effect_counts_only_the_requested_currency() {
        // Legs in different currencies are neither added nor offset:
        // the requested currency is selected, the others are ignored.
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::OpeningCash {
                amount: rub(5_000_000),
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                Leg::cash(acc, usd(700_000)),
                Leg::tax(acc, rub(-130_000)),
            ],
            acc,
        );
        assert_eq!(ev.cash_effect(CurrencyCode::Rub), Ok(rub(4_870_000)));
        assert_eq!(ev.cash_effect(CurrencyCode::Usd), Ok(usd(700_000)));
    }

    #[test]
    fn cash_effect_of_an_event_without_money_is_zero_in_that_currency() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(100),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, qty(100))],
            acc,
        );
        assert_eq!(
            ev.cash_effect(CurrencyCode::Eur),
            Ok(Money::zero(CurrencyCode::Eur))
        );
    }

    // --- Envelope ---

    #[test]
    fn unknown_confidence_is_representable_without_a_placeholder() {
        // Unknown confidence is a distinct value, not zero (§4.9).
        let acc = AccountId::new_random();
        let mut ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        ev.confidence = Confidence::Unknown;
        assert_eq!(ev.confidence, Confidence::Unknown);
        assert_ne!(Confidence::Unknown, Confidence::Estimated);
        assert_ne!(Confidence::Unknown, Confidence::Known);
        // Event shape does not depend on confidence.
        assert!(ev.validate_structure().is_ok());
    }

    #[test]
    fn an_event_carries_the_current_schema_version() {
        let acc = AccountId::new_random();
        let ev = event(
            EventKind::CashIn {
                amount: rub(5_000_000),
            },
            vec![Leg::cash(acc, rub(5_000_000))],
            acc,
        );
        assert_eq!(ev.schema_version, SCHEMA_VERSION);
        // The literal is fixed intentionally: raising the schema version must be
        // a conscious decision, not a side effect of an edit. Every
        // change to this line requires answering whether previously recorded
        // facts from older versions remain readable (§4.1).
        //
        // 1 → 2: added `EventKind::Valuation`.
        // 2 → 3: added `EventKind::ControlAssertion` (§10.3).
        // 3 → 4: added `EventKind::CorporateAction` and
        //        `EventKind::OfferExercise`, and `Income` gained a kind (§4.7).
        // 4 → 5: `EffectiveOrder` gained an optional source time.
        // 5 → 6: `Trade` gained optional basis-only fee fields.
        // 6 → 7: added `EventKind::ImportCoverageGap` (§10.3).
        // 7 → 8: `ImportCoverageGap` gained `rows`; added
        //        `EventKind::ImportRowResolution` (§10.3).
        // 8 → 9: added `EventKind::Tax`, so that a tax stops being
        //        indistinguishable from ordinary spending in the flow report.
        //        Older facts stay readable: the version guard in
        //        `iaam-app/src/scenarios/ingest.rs` refuses a mismatched
        //        version only on the WRITE path, and nothing on the read path
        //        compares against the current version. The `< 8` allowance in
        //        `validate_import_coverage_gap` above is a historical threshold
        //        and is deliberately left alone.
        // 9 → 10: added the optional source description in `Provenance`.
        //        Older facts stay readable because the field defaults to absent.
        // 10 → 11: added the variant `EventKind::Refund`. Older facts stay
        //        readable — no existing variant changed shape — and the number
        //        tells software that does not know the variant that it cannot
        //        interpret every fact it may now meet.
        // 11 → 12: added the optional declaring principal in `Provenance`
        //        (iaam-rond). Older facts stay readable because the field
        //        defaults to absent — and here that absence is not merely
        //        tolerated, it is the answer: an agent may retract only an
        //        import it declared, so a fact naming no declarer must refuse
        //        rather than be claimed. The number is what tells a reader that
        //        «no principal» means «written before anyone was recorded».
        // 12 → 13: added the variants `EventKind::OwnAccountMovement` and
        //        `EventKind::UnresolvedOwnAccountMovement` (iaam-fmih). Older
        //        facts stay readable — no existing variant changed shape — and
        //        the number is what tells software that does not know them that
        //        it may now meet a fact it cannot place: one whose far side is
        //        an account of the owner's that no contour can prove it holds,
        //        and one that posts no leg at all.
        // 13 → 14: added the optional source operation word in `Provenance`,
        //        beside the source category it used to be written through
        //        (iaam-p683). Older facts stay readable because the field
        //        defaults to absent, and nothing rewrites them: a fact below
        //        this version whose `source_category` holds an operation word
        //        keeps it, because a repair would be this software guessing
        //        what a source said.
        assert_eq!(SCHEMA_VERSION, 14);
    }

    #[test]
    fn a_valuation_with_a_leg_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.kind = EventKind::Valuation {
            instrument: crate::ids::InstrumentId::new_random(),
            price: crate::numeric::decimal::Dec::one(),
            currency: CurrencyCode::Rub,
            quality: crate::valuation::PriceQuality::OwnerEstimate,
        };
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
        event.legs = vec![];
        assert!(event.validate_structure().is_ok());
    }

    // --- Matching legs to events and value signs ---
    //
    // Every rejection is tested separately: without this the mutation gate
    // shows that validation can be replaced with `Ok(())` and no
    // test notices (verified — that was indeed the case).

    fn buy_with(acc: AccountId, instrument: InstrumentId, quantity: Quantity, leg: Leg) -> Event {
        event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity,
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![Leg::cash(acc, rub(-5_000_000)), leg],
            acc,
        )
    }

    #[test]
    fn a_trade_of_zero_quantity_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            Quantity::zero(),
            security_leg(acc, instrument, Quantity::zero()),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_trade_of_negative_quantity_is_rejected() {
        // A negative purchase quantity is a short, and shorts are out of
        // scope (§11): their monetary effect is preserved as a separate
        // event type, not a negative trade.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(-10),
            security_leg(acc, instrument, qty(-10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_trade_of_zero_value_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(10),
                gross: rub(0),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(0)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::NonPositive { field: "gross", .. })
        ));
    }

    #[test]
    fn a_security_leg_of_another_instrument_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();
        let ev = buy_with(acc, instrument, qty(10), security_leg(acc, other, qty(10)));
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "instrument",
                ..
            })
        ));
    }

    #[test]
    fn a_security_leg_on_another_account_is_rejected() {
        let acc = AccountId::new_random();
        let stranger = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(stranger, instrument, qty(10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongAccount { .. })
        ));
    }

    #[test]
    fn a_cash_leg_on_another_account_is_rejected() {
        let acc = AccountId::new_random();
        let stranger = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(10),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(stranger, rub(-5_000_000)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::WrongAccount { .. })
        ));
    }

    #[test]
    fn a_leg_quantity_differing_from_the_event_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(acc, instrument, qty(9)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_purchase_whose_leg_reduces_the_position_is_rejected() {
        // The sign is determined by trade direction: a purchase increases the position.
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = buy_with(
            acc,
            instrument,
            qty(10),
            security_leg(acc, instrument, qty(-10)),
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_sale_whose_leg_increases_the_position_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let ev = event(
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument,
                quantity: qty(10),
                gross: rub(5_000_000),
                fee: None,
                accrued_interest: None,
                basis_fee: None,
                basis_fee_exact: None,
            },
            vec![
                Leg::cash(acc, rub(5_000_000)),
                security_leg(acc, instrument, qty(10)),
            ],
            acc,
        );
        assert!(matches!(
            ev.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn an_opening_position_disagreeing_with_its_leg_is_rejected() {
        let acc = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let other = InstrumentId::new_random();

        let wrong_quantity = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(10),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, qty(11))],
            acc,
        );
        assert!(matches!(
            wrong_quantity.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "quantity",
                ..
            })
        ));

        let wrong_instrument = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: qty(10),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, other, qty(10))],
            acc,
        );
        assert!(matches!(
            wrong_instrument.validate_structure(),
            Err(EventValidationError::LegDoesNotMatchEvent {
                field: "instrument",
                ..
            })
        ));

        let zero = event(
            EventKind::OpeningPosition {
                instrument,
                quantity: Quantity::zero(),
                cost_basis: None,
                assertions: kind::OpeningAssertions::default(),
            },
            vec![security_leg(acc, instrument, Quantity::zero())],
            acc,
        );
        assert!(matches!(
            zero.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }

    #[test]
    fn a_valuation_at_zero_or_below_is_rejected() {
        // A zero price gives a zero position value and plausible
        // returns. A worthless security is a delisting fact (E3),
        // not a price.
        let acc = AccountId::new_random();
        for price in [
            crate::numeric::decimal::Dec::zero(),
            crate::numeric::decimal::Dec::new(rust_decimal::Decimal::from(-1)),
        ] {
            let ev = event(
                EventKind::Valuation {
                    instrument: InstrumentId::new_random(),
                    price,
                    currency: CurrencyCode::Rub,
                    quality: crate::valuation::PriceQuality::OwnerEstimate,
                },
                vec![],
                acc,
            );
            assert!(matches!(
                ev.validate_structure(),
                Err(EventValidationError::NonPositive { field: "price", .. })
            ));
        }
    }

    #[test]
    fn a_replacement_points_at_the_event_it_replaces() {
        let target = EventId::new_random();
        assert_ne!(
            Relation::Replacement { target },
            Relation::Reversal { target }
        );
        assert_ne!(Relation::Replacement { target }, Relation::None);
    }

    #[test]
    fn a_control_assertion_carries_no_legs() {
        // An assertion about interval completeness does not move cash. A leg on it
        // would mean the report's control section entered the balance
        // a second time and doubled it.
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        let account = AccountId::new_random();
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(1_000_000),
            at: BalancePoint::Closing,
        };
        let kind = EventKind::ControlAssertion { period, claim };

        let clean =
            test_support::event_with(account, date!(2026 - 03 - 31), 1, kind.clone(), vec![]);
        assert!(clean.validate_structure().is_ok());

        let with_leg = test_support::event_with(
            account,
            date!(2026 - 03 - 31),
            2,
            kind,
            vec![Leg::cash(account, rub(1_000_000))],
        );
        assert!(matches!(
            with_leg.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
    }

    #[test]
    fn a_control_assertion_with_an_inverted_period_is_rejected() {
        // The constructor does not create such an interval, but the event also comes
        // from JSON, where the constructor was not called. Shape validation is
        // the second line of defense, and it must catch the state rather than rely
        // on the first line having worked.
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        assert!(AssertionPeriod::between(date!(2026 - 03 - 31), date!(2026 - 03 - 01)).is_none());

        let inverted = AssertionPeriod {
            from: date!(2026 - 03 - 31),
            to: date!(2026 - 03 - 01),
        };
        let kind = EventKind::ControlAssertion {
            period: inverted,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(1),
                at: BalancePoint::Opening,
            },
        };
        let event = test_support::event_with(
            AccountId::new_random(),
            date!(2026 - 03 - 01),
            1,
            kind,
            vec![],
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "period",
                ..
            })
        ));
    }

    #[test]
    fn negative_totals_are_rejected_but_a_negative_cash_balance_is_not() {
        // A negative balance is a valid state (§11): technical
        // overdrafts and settlement timing. A negative fee total
        // is not a valid state: it is a sign-parsing error,
        // and accepting it means recording it in the journal forever.
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};

        let account = AccountId::new_random();
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();

        let overdraft = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-5_000),
                at: BalancePoint::Closing,
            },
        };
        assert!(
            test_support::event_with(account, date!(2026 - 03 - 31), 1, overdraft, vec![])
                .validate_structure()
                .is_ok()
        );

        let negative_fees = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::FeesTotal {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-100),
            },
        };
        assert!(matches!(
            test_support::event_with(account, date!(2026 - 03 - 31), 2, negative_fees, vec![])
                .validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "amount",
                ..
            })
        ));
    }

    #[test]
    fn a_negative_position_quantity_is_outside_the_perimeter() {
        // Shorts are out of scope (§11). A negative quantity in the control
        // section means either a short or a reversed sign — neither
        // can be accepted.
        use crate::numeric::decimal::Dec;
        use crate::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
        use rust_decimal::Decimal;

        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        let kind = EventKind::ControlAssertion {
            period,
            claim: ControlClaim::PositionQuantity {
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: Quantity(Dec::new(Decimal::from(-10))),
                at: BalancePoint::Closing,
            },
        };
        assert!(matches!(
            test_support::event_with(
                AccountId::new_random(),
                date!(2026 - 03 - 31),
                1,
                kind,
                vec![]
            )
            .validate_structure(),
            Err(EventValidationError::NonPositive {
                field: "quantity",
                ..
            })
        ));
    }
}
