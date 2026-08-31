//! Normalized operation and its conversion into a journal event.

use iaam_core::dates::{
    CashPostedDate, EffectiveOrder, EventDates, PaidDate, SettledDate, TradeDate,
};
use iaam_core::event::kind::{EventKind, FeeOrigin, IncomeKind, OpeningAssertions, TradeSide};
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, CustodyId, EventId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::{CalcMoney, CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use serde::{Deserialize, Serialize};

use crate::verdict::Rejection;

/// Parsing version. Written to provenance: without it, an error in the source cannot be distinguished
/// from a parsing error corrected later (§4.1).
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OperationKind {
    Deposit {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Withdrawal {
        amount_minor: i64,
        currency: CurrencyCode,
    },
    Transfer {
        to: AccountId,
        amount_minor: i64,
        currency: CurrencyCode,
    },
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
    Income {
        instrument: Option<InstrumentId>,
        gross_minor: i64,
        currency: CurrencyCode,
        /// The income type, if named by the source. `None` means “not
        /// stated”: substituting a dividend where the source is silent
        /// means recording an invention in the journal (§4.9).
        kind: Option<IncomeKind>,
    },
    Fee {
        amount_minor: i64,
        currency: CurrencyCode,
        origin: FeeOrigin,
    },
    OpeningCash {
        amount_minor: i64,
        currency: CurrencyCode,
    },
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
    pub idempotency_key: Option<String>,
    /// Operation identifier in the source, if present.
    pub source_operation_id: Option<String>,
}

/// An event ready to be written plus a fingerprint of the raw record.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub event: Event,
}

/// Normalization context: who owns it and which source it came from.
///
/// There is intentionally no sequence number here: storage assigns it
/// in the same transaction as the insert. Ingestion uses number `1`
/// as deliberately temporary—storage will overwrite it (§4.8).
#[derive(Debug, Clone, Copy)]
pub struct NormalizationContext {
    pub owner: OwnerId,
    pub source: SourceId,
}

/// Converting an operation into a journal event.
///
/// Returns a rejection rather than panicking or supplying defaults: a row
/// with an unrecognized operation receives a verdict, and document processing continues
/// (§10.1).
pub fn normalize(
    operation: &SubmittedOperation,
    context: NormalizationContext,
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
                let base = Provenance::new(
                    context.source,
                    raw_hash,
                    ParserVersion(PARSER_VERSION.to_owned()),
                );
                match operation.source_operation_id.as_deref() {
                    Some(id) => base.with_source_operation_id(id),
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
