//! Normalized operation and its conversion into a journal event.

use iaam_core::dates::{
    CashPostedDate, EffectiveOrder, EventDates, PaidDate, SettledDate, TradeDate,
};
use iaam_core::event::kind::{
    EventKind, FeeOrigin, IncomeKind, OpeningAssertions, TaxOrigin, TradeSide,
};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CalcMoney, CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use serde::{Deserialize, Serialize};

use crate::verdict::Rejection;

/// The version of the reader for a row a caller stated itself.
///
/// Written to provenance: without it, an error in the source cannot be
/// distinguished from a parsing error corrected later (§4.1).
///
/// **It is not a default.** [`NormalizationContext`] has no default parser
/// version, and this constant is what a caller supplies when the reader really
/// was the caller — an operation posted as JSON, a row typed into an import
/// session. A caller that read a document supplies the version of whatever read
/// it, and a caller that forgets does not compile. This was once stamped by
/// `normalize` itself, so every row committed out of an import session claimed
/// to have been typed by hand whatever had actually read it (`iaam-h69n`).
pub const PARSER_VERSION: &str = "ingest/manual/1";

/// Operation dates. All are optional except the one that makes the operation dated: an event without a single date does not belong to any period.
/// Date-based: an event without a single date does not fall into any period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OperationDates {
    pub trade: Option<time::Date>,
    pub settled: Option<time::Date>,
    pub cash_posted: Option<time::Date>,
    pub paid: Option<time::Date>,
}

impl OperationDates {
    /// The day the row is dated by, when it carries one.
    ///
    /// Delegated to [`EventDates::effective_date`] rather than restated: an
    /// ordering written twice drifts, and the day a row is read as happening on
    /// must be the day the journal will date the fact it becomes.
    #[must_use]
    pub fn effective_date(self) -> Option<time::Date> {
        self.to_event_dates().effective_date()
    }

    fn to_event_dates(self) -> EventDates {
        EventDates {
            trade: self.trade.map(TradeDate),
            settled: self.settled.map(SettledDate),
            cash_posted: self.cash_posted.map(CashPostedDate),
            entitlement: None,
            paid: self.paid.map(PaidDate),
            tax_period_override: None,
        }
    }
}

/// What happened. Amounts are **positive**: the sign determines the operation type,
/// not the client.
///
/// [`Self::OpeningCash`] is the one exception, and it is an exception to the
/// reason rather than to the rule: it restores a balance rather than reporting a
/// movement, and a reconstructed balance may genuinely be below zero (§15.9).
/// Every other kind here refuses a negative amount through `positive`, naming
/// the field the client actually sent.
///
/// One row of a source becomes one operation. Nothing here is submitted twice:
/// where a movement has two sides — [`Self::Transfer`] — the second side is
/// written by `build`, not by the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OperationKind {
    /// Money the account received from **outside** the owner's own accounts.
    ///
    /// Becomes `CashIn`, whose flow endpoints say the other side is a
    /// counterparty the system does not observe. That is the whole difference
    /// from [`Self::Transfer`], and it is not a matter of wording: a report
    /// counts a deposit as money entering the perimeter and a transfer as money
    /// already inside it moving. Sending a deposit for the receiving side of a
    /// transfer between two of the owner's accounts overstates what came in.
    Deposit {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    /// Money the account paid out to **outside** the owner's own accounts.
    ///
    /// The mirror of [`Self::Deposit`], and the same caution: the sending side
    /// of a transfer between two of the owner's accounts is a
    /// [`Self::Transfer`], not a withdrawal.
    Withdrawal {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    /// Money a counterparty returned, reversing an earlier outflow.
    Refund {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    /// Money moved between two of the owner's own accounts, submitted **once**.
    ///
    /// The operation's own `account` is the side the money left and `to` is the
    /// side it arrived at. One submission states the whole movement:
    /// `build` writes both legs itself, `Leg::cash(account, -amount)` beside
    /// `Leg::cash(to, amount)`, and the caller writes neither.
    ///
    /// **A source that prints the movement twice is still one operation.** Two
    /// banks each print their own half, and submitting both records two
    /// transfers rather than the two halves of one: each account then moves by
    /// twice the sum, and it keeps multiplying with every export that overlaps.
    /// There is nothing to submit for the receiving side, because this variant
    /// has already said what happened there.
    ///
    /// `amount_minor` is positive like every other kind here. A negative amount
    /// is **refused**, not read as "the outgoing leg": direction is carried by
    /// `account` → `to`, so a sign has nothing left to say and a caller using
    /// one has a model of this variant that the code does not share.
    ///
    /// `to` must name an account other than `account`. A transfer to itself is
    /// refused on the `to` field before the amount is looked at — it moves
    /// nothing, and recording it would put two cancelling legs on one account
    /// for a movement that never happened.
    ///
    /// The event is a `CashTransfer` carrying both accounts and a freshly
    /// minted `TransferId`. Both accounts live on the event rather than being
    /// inferred from the legs (`iaam-core/src/event/kind.rs`): whether the
    /// movement crossed the contour boundary cannot be decided from one side,
    /// and the journal is append-only, so a one-sided record could not be
    /// repaired afterwards. That event is also what transfer pairing and the
    /// flow reports read — they see one movement between two accounts, never a
    /// pair of independent rows.
    Transfer {
        to: AccountId,
        amount_minor: i64,
        currency: CurrencyCode,
    },
    /// Money moved between this account and another account of the owner's
    /// that the source asserted and did not name.
    ///
    /// The one kind here whose direction is a field rather than the variant,
    /// and it is a field because it may be **absent**: a source that files a row
    /// as a movement within the owner's own holdings routinely prints no
    /// direction beside it, and there is no positive-amount convention that
    /// could stand in — [`ObservedRow::movement`] refuses to read the sign of a
    /// row whose source stated no direction, precisely because a bank that
    /// prints every amount positive would otherwise have every row read as an
    /// arrival.
    ///
    /// `amount_minor` is positive like every other kind here; the direction is
    /// `movement` and nothing else. `build` produces
    /// `EventKind::OwnAccountMovement` with a signed leg where the direction is
    /// stated and `EventKind::UnresolvedOwnAccountMovement` with no leg at all
    /// where it is not — **two** journal facts from one submission shape,
    /// because a submission may reasonably carry a maybe and a fact may not.
    ///
    /// It is not [`Self::Transfer`]. That kind names the far account and writes
    /// both legs, and it is refused outright when the two accounts are the
    /// same; here there is no far account to name. It is not
    /// [`Self::Deposit`] or [`Self::Withdrawal`] either: those say the money
    /// crossed into or out of the owner's own accounts, which is the opposite
    /// of what the source said.
    OwnAccountMovement {
        movement: Option<crate::classification::Movement>,
        amount_minor: i64,
        currency: CurrencyCode,
    },
    /// A purchase: cash leaves the account and the security arrives.
    ///
    /// `quantity` and `gross_minor` are both positive, as is the optional
    /// `fee_minor`; the negation belongs to `build`. What the account pays is
    /// `gross + accrued_interest + fee`, and that sum — not `gross_minor` — is
    /// the cash leg. A caller that reports the settled amount as `gross_minor`
    /// and repeats the fee in `fee_minor` charges the fee twice.
    ///
    /// `basis_fee` is deliberately **not** in that sum: it is a commission that
    /// belongs to the tax basis and moves no cash. The exact value is kept
    /// beside the rounded one on the event, so the rounding stays auditable.
    Buy {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        gross_minor: i64,
        fee_minor: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basis_fee: Option<CalcMoney>,
        accrued_interest_minor: Option<i64>,
        currency: CurrencyCode,
    },
    /// A sale: the security leaves the account and cash arrives.
    ///
    /// `quantity` is positive here too, exactly as for [`Self::Buy`]: the side
    /// is the variant, not the sign, and `build` negates the security leg. The
    /// event keeps the positive quantity while the leg carries the negative one,
    /// so a reader of the event is not looking at a short position.
    ///
    /// What the account receives is `gross + accrued_interest - fee`: the
    /// accrued coupon is money the buyer pays over, and the commission is
    /// deducted from the proceeds. This is the one place where the fee's sign
    /// differs in effect between the two sides, and it is why `fee_minor` is
    /// submitted positive on both.
    Sell {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        gross_minor: i64,
        fee_minor: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basis_fee: Option<CalcMoney>,
        accrued_interest_minor: Option<i64>,
        currency: CurrencyCode,
    },
    /// A coupon, dividend or interest payment actually received.
    ///
    /// `instrument` is optional because interest on a cash balance belongs to no
    /// security. Absence means the payment names none, never "the system will
    /// work out which".
    Income {
        instrument: Option<InstrumentId>,
        gross_minor: i64,
        currency: CurrencyCode,
        /// The income type, if named by the source. `None` means “not
        /// stated”: substituting a dividend where the source is silent
        /// means recording an invention in the journal (§4.9).
        kind: Option<IncomeKind>,
    },
    /// A charge not attached to a trade: custody, servicing, a transfer fee.
    ///
    /// Submitted positive and recorded negative, on a fee leg rather than a cash
    /// leg. A trade's own commission belongs in [`Self::Buy`] or [`Self::Sell`]
    /// as `fee_minor`, where it is part of the settled amount; sending it here
    /// as well charges the account twice for one commission.
    Fee {
        amount_minor: i64,
        currency: CurrencyCode,
        origin: FeeOrigin,
    },
    /// Tax, whether a broker withheld it or the owner paid it himself.
    ///
    /// Submitted positive and recorded negative, on a tax leg. `origin` is the
    /// distinction that matters afterwards, and it is stated rather than
    /// inferred: withheld tax has already left the account, and self-paid tax is
    /// a payment the owner made.
    Tax {
        amount_minor: i64,
        currency: CurrencyCode,
        origin: TaxOrigin,
    },
    /// The cash a reconstructed account already held before the journal begins.
    ///
    /// **The one kind whose amount may be negative**, and the sign is used as
    /// submitted. A restored balance can be below zero — an overdraft, a margin
    /// account — and refusing it here would force the owner to state a balance
    /// he does not have (§15.9). It is a starting position, not a movement:
    /// nothing entered or left the contour, so no report counts it as a flow.
    OpeningCash {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    /// A position a reconstructed account already held before the journal
    /// begins.
    ///
    /// `cost_basis_minor` is optional and, when given, positive: a basis of
    /// nothing is not a basis of zero, and the two must not be spelled the same
    /// way. Absence means the owner did not state one, and the tax reports say
    /// so rather than substituting the market value.
    OpeningPosition {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Dec,
        cost_basis_minor: Option<i64>,
        currency: CurrencyCode,
        /// What the owner knows about the restored position (§10.7).
        ///
        /// Absence means “nothing stated”: acceptance supplies
        /// a default in which everything is unknown, and **does not infer**
        /// confidence from the presence of other fields. A submitted value
        /// does not make the tax basis documented: a person confirms the document,
        /// not the fact that the field is filled in.
        #[serde(default)]
        assertions: Option<OpeningAssertions>,
    },
    /// A price for an instrument on a day, and **no movement at all**.
    ///
    /// The only kind that produces no legs: nothing was bought, sold or paid, so
    /// no account balance changes. `quality` says where the price came from, and
    /// it is carried rather than assumed — a valuation the owner estimated must
    /// not be read later as one an exchange published.
    Valuation {
        instrument: InstrumentId,
        price: Dec,
        currency: CurrencyCode,
        quality: PriceQuality,
    },
}

/// An operation received through an API or from a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmittedOperation {
    pub account: AccountId,
    pub kind: OperationKind,
    pub dates: OperationDates,
    /// Time-of-day reported by the source, if it names a moment.
    ///
    /// This is separate from the operation dates: a source can give a
    /// calendar date without asserting a moment (§4.9).
    #[serde(default)]
    pub source_time: Option<time::Time>,
    /// Client idempotency key (§10.6).
    ///
    /// It names the **fact**, not the submission slot. The key is matched
    /// before the operation is compared to anything, so a row re-sent under a
    /// key already recorded is a duplicate however its values changed: a
    /// corrected amount under a used key is discarded, and the journal keeps
    /// the number that was wrong. Nothing on this path retracts anything — a
    /// wrong fact is corrected through the correction scenario, and re-sending
    /// is not a retract-and-add.
    pub idempotency_key: Option<String>,
    /// Operation identifier in the source, if present.
    pub source_operation_id: Option<String>,
    /// The source's own word for what the operation was **for**, verbatim.
    ///
    /// Retained for later rule matching, and never the same field as
    /// [`Self::source_kind`] beside it: a category rule matches this one, and
    /// filling it with an operation word makes that rule fire on rows the owner
    /// was not describing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_category: Option<String>,
    /// The category the **owner himself** filed the row under, at the source,
    /// verbatim.
    ///
    /// Retained for the same reason as [`Self::source_category`] beside it and
    /// never the same field: that one is the institution's word, this one is a
    /// decision of his that the institution merely printed back. Kept so that a
    /// rule of his written on it goes on matching after the fact is recorded —
    /// a fact whose evidence was dropped on the way in looks, to recomputation,
    /// like a row whose source said nothing.
    ///
    /// `#[serde(default)]` because an import session written before this field
    /// existed said nothing about it, which is what `None` means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_category: Option<String>,
    /// The standardised code the source printed for the row, verbatim.
    ///
    /// Text and never a number: it is an identifier printed with leading zeros.
    /// `None` where the source printed none, which it does on rows it assigns
    /// no code to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_code: Option<String>,
    /// The source's own word for what the operation **was**, verbatim.
    ///
    /// `#[serde(default)]` because a row stored by an earlier build carries no
    /// such field: import sessions hold submitted operations as JSON, and one
    /// written before this field existed said nothing about the source's
    /// operation word — which is what `None` means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    /// Description or counterparty printed by the source, retained verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// An event ready to be written plus a fingerprint of the raw record.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub event: Event,
}

/// Normalization context: who owns it, which source it came from, and what
/// read it.
///
/// There is intentionally no sequence number here: storage assigns it
/// in the same transaction as the insert. Ingestion uses number `1`
/// as deliberately temporary—storage will overwrite it (§4.8).
///
/// **There is intentionally no default parser version either, and no
/// `Default`.** The field is a plain struct member of a type every caller
/// builds by literal, so a caller that does not say what read its rows fails to
/// compile. That is the whole point: a default is how every row committed out of
/// an import session came to claim it had been typed by hand (`iaam-h69n`), and
/// the recovery story for a buggy reader — the facts it wrote are a set you can
/// find and retract — is not true while the facts name the wrong reader.
///
/// The version belongs to the **batch** and not to the session or the import: a
/// session is opened per declaration, and a declaration names an account and a
/// label, never a reader.
#[derive(Debug, Clone)]
pub struct NormalizationContext {
    pub owner: OwnerId,
    pub source: SourceId,
    /// What read the rows this context normalises.
    ///
    /// [`PARSER_VERSION`] where the caller stated the row itself; the document
    /// reader's own version where a reader produced it.
    pub parser_version: ParserVersion,
}

/// Converting an operation into a journal event.
///
/// Returns a rejection rather than panicking or supplying defaults: a row
/// with an unrecognized operation receives a verdict, and document processing continues
/// (§10.1).
pub fn normalize(
    operation: &SubmittedOperation,
    context: &NormalizationContext,
) -> Result<Normalized, Rejection> {
    let dates = operation.dates.to_event_dates();
    let day = dates.effective_date().ok_or_else(|| Rejection {
        field: "dates".into(),
        expected: "at least one date: trade, settled, cash_posted, or paid".into(),
        actual: "none".into(),
    })?;

    let (kind, legs) = build(operation, &operation.kind)?;
    // The fingerprint is the same as for deduplication, and is calculated there as well:
    // a second copy of this function would silently diverge from the first, while
    // fingerprints have already been deduplicated (§10.6).
    let raw_hash = crate::dedup::fingerprint(operation);

    Ok(Normalized {
        event: Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: context.owner,
            account: operation.account,
            kind,
            dates,
            // Temporary number: storage assigns the final one.
            order: operation.source_time.map_or_else(
                || EffectiveOrder::new(day, 1),
                |time| EffectiveOrder::with_source_time(day, time, 1),
            ),
            legs,
            provenance: {
                // The version comes from the context and from nowhere else.
                // It used to be this constant, and a second place then
                // overwrote it for one channel out of five (`iaam-h69n`).
                let base =
                    Provenance::new(context.source, raw_hash, context.parser_version.clone());
                let base = match operation.source_operation_id.as_deref() {
                    Some(id) => base.with_source_operation_id(id),
                    None => base,
                };
                let base = match operation.source_category.as_deref() {
                    Some(category) => base.with_source_category(category),
                    None => base,
                };
                // The owner's own word and the network's code, each into its
                // own field for the reason the two above are separate: they are
                // statements by different parties about different things, and
                // one slot could carry only one of them.
                let base = match operation.owner_category.as_deref() {
                    Some(category) => base.with_owner_category(category),
                    None => base,
                };
                let base = match operation.source_code.as_deref() {
                    Some(code) => base.with_source_code(code),
                    None => base,
                };
                // Beside the category and never through it: the two are
                // different facts, and one slot could carry only one of them
                // (`iaam-p683`).
                let base = match operation.source_kind.as_deref() {
                    Some(kind) => base.with_source_kind(kind),
                    None => base,
                };
                match operation.description.as_deref() {
                    Some(description) => base.with_description(description),
                    None => base,
                }
            },
            relation: Relation::None,
            // `Confidence` describes **the value**, not verification (§4.9):
            // the owner entering a top-up manually knows its amount.
            // The lack of independent confirmation is a statement
            // about the account and interval (§10.3); it will appear in E2 as a separate
            // entity and is not an event field.
            confidence: Confidence::Known,
            idempotency_key: operation.idempotency_key.clone(),
        },
    })
}

/// Convert a decimal amount to minimum units **without rounding**.
///
/// An amount with greater precision than the currency's smallest unit is
/// not an “almost correct” amount but invalid input: rounding it would make the
/// system record a fact that did not occur (§3.4).
pub fn to_minor_units(
    value: rust_decimal::Decimal,
    currency: CurrencyCode,
    field: &str,
) -> Result<i64, Rejection> {
    let scale = currency.minor_units();
    if value.scale() > scale {
        return Err(Rejection {
            field: field.to_owned(),
            expected: format!(
                "no more than {scale} decimal places for {}",
                currency.code()
            ),
            actual: value.to_string(),
        });
    }
    let factor = rust_decimal::Decimal::from(10_i64.pow(scale));
    let scaled = value
        .checked_mul(factor)
        .ok_or_else(|| Rejection {
            field: field.to_owned(),
            expected: "representable amount".into(),
            actual: value.to_string(),
        })?
        .normalize();
    i64::try_from(scaled.mantissa())
        .ok()
        .filter(|_| scaled.scale() == 0)
        .ok_or_else(|| Rejection {
            field: field.to_owned(),
            expected: "an integer number of minor units".into(),
            actual: scaled.to_string(),
        })
}

fn money(minor: i64, currency: CurrencyCode) -> Money {
    Money::new(PostedMinor::new(minor), currency)
}

/// The value must be positive.
///
/// The field name and value in the rejection are exactly what the client sent: `amount`,
/// not `amount_minor`, and `-5.00`, not `-500`. A rejection that names
/// an internal name and internal units tells the client to fix a field
/// that it did not send (§10.4).
fn positive(value: i64, field: &str, currency: CurrencyCode) -> Result<i64, Rejection> {
    if value > 0 {
        Ok(value)
    } else {
        Err(Rejection {
            field: field.to_owned(),
            expected: "positive value".into(),
            actual: money(value, currency).to_calc_dec().inner().to_string(),
        })
    }
}

/// Constructing the event type and legs.
///
/// The dispatcher is exhaustive: a new operation kind must fail to compile.
fn build(
    operation: &SubmittedOperation,
    kind: &OperationKind,
) -> Result<(EventKind, Vec<Leg>), Rejection> {
    let account = operation.account;
    match kind {
        OperationKind::Deposit {
            amount_minor,
            currency,
        } => {
            let amount = money(positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::CashIn { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::Withdrawal {
            amount_minor,
            currency,
        } => {
            let amount = money(-positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::CashOut { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::Refund {
            amount_minor,
            currency,
        } => {
            let amount = money(positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::Refund { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::Transfer {
            to,
            amount_minor,
            currency,
        } => {
            if *to == account {
                return Err(Rejection {
                    field: "to".into(),
                    expected: "an account different from the operation account".into(),
                    actual: to.inner().to_string(),
                });
            }
            let amount = money(positive(*amount_minor, "amount", *currency)?, *currency);
            let outgoing = amount.checked_negate().map_err(|error| Rejection {
                field: "amount".into(),
                expected: "representable amount".into(),
                actual: error.to_string(),
            })?;
            Ok((
                EventKind::CashTransfer {
                    transfer_id: iaam_core::ids::TransferId::new_random(),
                    from: account,
                    to: *to,
                    amount,
                },
                vec![Leg::cash(account, outgoing), Leg::cash(*to, amount)],
            ))
        }
        OperationKind::OwnAccountMovement {
            movement,
            amount_minor,
            currency,
        } => {
            let magnitude = positive(*amount_minor, "amount", *currency)?;
            match movement {
                Some(crate::classification::Movement::Out) => {
                    let amount = money(-magnitude, *currency);
                    Ok((
                        EventKind::OwnAccountMovement { amount },
                        vec![Leg::cash(account, amount)],
                    ))
                }
                Some(crate::classification::Movement::In) => {
                    let amount = money(magnitude, *currency);
                    Ok((
                        EventKind::OwnAccountMovement { amount },
                        vec![Leg::cash(account, amount)],
                    ))
                }
                // No leg, and the amount stays a magnitude. A leg here would be
                // the journal asserting a direction on the strength of nothing,
                // which is the defect the whole shape answers.
                None => Ok((
                    EventKind::UnresolvedOwnAccountMovement {
                        amount: money(magnitude, *currency),
                    },
                    Vec::new(),
                )),
            }
        }
        OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            basis_fee,
            accrued_interest_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "amount", *currency)?, *currency);
            let fee = fee_money(*fee_minor, *currency)?;
            let basis_fee_exact = basis_fee.map(|value| {
                CalcMoney::new(Dec::new(value.value().inner().abs()), value.currency())
            });
            let basis_fee = basis_fee_money(basis_fee_exact, *currency)?;
            let accrued = accrued_interest_money(*accrued_interest_minor, *currency)?;
            let mut settlement = gross.amount().raw();
            settlement += accrued.map_or(0, |value| value.amount().raw());
            settlement += fee.map_or(0, |value| value.amount().raw());
            Ok((
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    gross,
                    fee,
                    basis_fee,
                    basis_fee_exact,
                    accrued_interest: accrued,
                },
                vec![
                    Leg::cash(account, money(-settlement, *currency)),
                    Leg::security(account, *custody, *instrument, Quantity(*quantity)),
                ],
            ))
        }
        OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            basis_fee,
            accrued_interest_minor,
            currency,
        } => {
            let gross = money(positive(*gross_minor, "amount", *currency)?, *currency);
            let fee = fee_money(*fee_minor, *currency)?;
            let basis_fee_exact = basis_fee.map(|value| {
                CalcMoney::new(Dec::new(value.value().inner().abs()), value.currency())
            });
            let basis_fee = basis_fee_money(basis_fee_exact, *currency)?;
            let accrued = accrued_interest_money(*accrued_interest_minor, *currency)?;
            let mut settlement = gross.amount().raw();
            settlement += accrued.map_or(0, |value| value.amount().raw());
            settlement -= fee.map_or(0, |value| value.amount().raw());
            let sold = quantity.checked_neg().map_err(|error| Rejection {
                field: "quantity".into(),
                expected: "representable quantity".into(),
                actual: error.to_string(),
            })?;
            Ok((
                EventKind::Trade {
                    side: TradeSide::Sell,
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    gross,
                    fee,
                    basis_fee,
                    basis_fee_exact,
                    accrued_interest: accrued,
                },
                vec![
                    Leg::cash(account, money(settlement, *currency)),
                    Leg::security(account, *custody, *instrument, Quantity(sold)),
                ],
            ))
        }
        OperationKind::Income {
            instrument,
            gross_minor,
            currency,
            kind,
        } => {
            let gross = money(positive(*gross_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::Income {
                    instrument: *instrument,
                    gross,
                    kind: *kind,
                },
                vec![Leg::cash(account, gross)],
            ))
        }
        OperationKind::Fee {
            amount_minor,
            currency,
            origin,
        } => {
            let amount = money(-positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::Fee {
                    amount,
                    origin: *origin,
                },
                vec![Leg::fee(account, amount)],
            ))
        }
        OperationKind::Tax {
            amount_minor,
            currency,
            origin,
        } => {
            let amount = money(-positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::Tax {
                    amount,
                    origin: *origin,
                },
                vec![Leg::tax(account, amount)],
            ))
        }
        OperationKind::OpeningCash {
            amount_minor,
            currency,
        } => {
            // The reconstructed balance may be negative (§15.9),
            // so zero is not required here; the sign is used as-is.
            let amount = money(*amount_minor, *currency);
            Ok((
                EventKind::OpeningCash { amount },
                vec![Leg::cash(account, amount)],
            ))
        }
        OperationKind::OpeningPosition {
            instrument,
            custody,
            quantity,
            cost_basis_minor,
            currency,
            assertions,
        } => {
            let cost_basis = match cost_basis_minor {
                Some(value) => Some(money(positive(*value, "cost_basis", *currency)?, *currency)),
                None => None,
            };
            Ok((
                EventKind::OpeningPosition {
                    instrument: *instrument,
                    quantity: Quantity(*quantity),
                    cost_basis,
                    assertions: assertions.unwrap_or_default(),
                },
                vec![Leg::security(
                    account,
                    *custody,
                    *instrument,
                    Quantity(*quantity),
                )],
            ))
        }
        OperationKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } => Ok((
            EventKind::Valuation {
                instrument: *instrument,
                price: *price,
                currency: *currency,
                quality: *quality,
            },
            vec![],
        )),
    }
}

/// Commission and accrued coupon income are positive: the sign is determined by `trade_settlement`
/// the core, and this decision must not be duplicated in ingestion.
fn fee_money(value: Option<i64>, currency: CurrencyCode) -> Result<Option<Money>, Rejection> {
    match value {
        None => Ok(None),
        Some(minor) => Ok(Some(money(positive(minor, "fee", currency)?, currency))),
    }
}

/// Accrued interest accepts a reported zero, unlike a charged fee.
fn accrued_interest_money(
    value: Option<i64>,
    currency: CurrencyCode,
) -> Result<Option<Money>, Rejection> {
    match value {
        None => Ok(None),
        Some(minor) if minor >= 0 => Ok(Some(money(minor, currency))),
        Some(minor) => Err(Rejection {
            field: "accrued_interest".into(),
            expected: "non-negative value".into(),
            actual: money(minor, currency).to_calc_dec().inner().to_string(),
        }),
    }
}

/// Convert a calculated source commission to the posted basis value.
///
/// The exact value remains on the event beside this rounded value. This is
/// distinct from settlement `fee_minor`: a basis-only fee never changes cash.
fn basis_fee_money(
    value: Option<CalcMoney>,
    currency: CurrencyCode,
) -> Result<Option<Money>, Rejection> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.currency() != currency {
        return Err(Rejection {
            field: "basis_fee".into(),
            expected: format!("currency {}", currency.code()),
            actual: value.currency().code().to_owned(),
        });
    }
    let magnitude = CalcMoney::new(Dec::new(value.value().inner().abs()), currency);
    let amount = magnitude.rounded_minor().map_err(|error| Rejection {
        field: "basis_fee".into(),
        expected: "representable rounded amount".into(),
        actual: error.to_string(),
    })?;
    Ok(Some(money(amount.raw(), currency)))
}
