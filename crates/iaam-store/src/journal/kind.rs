//! `EventKind` mapped to and from rows (spec §4.3, §4.5).
//!
//! **No `..` in any match arm here.** Every field of every variant is
//! destructured by name, so a new field added to a variant breaks the build
//! instead of being silently dropped on write (spec §9). The two exhaustive
//! matches that carry this guarantee are [`kind_to_detail`], over
//! `&EventKind`, and the reverse dispatch inside [`from_rows`].
//!
//! Reconstruction of a value spec §4.5 lists as derived from a leg goes
//! through the typed helpers at the bottom of this file —
//! [`single_leg_money`], [`leg_money_for_account`], [`optional_leg_money`] —
//! and never through `legs[0]` or any other search by position: `ordinal`
//! carries no meaning (spec §4.2).

use std::collections::BTreeSet;

use iaam_core::dates::{
    CashPostedDate, EffectiveOrder, EntitlementDate, EventDates, PaidDate, SettledDate, TaxPeriod,
    TradeDate,
};
use iaam_core::event::allocation::{
    AllocationAlgorithmVersion, AllocationEvidence, AllocationGap, AllocationInputsHash,
    BasisAllocation,
};
use iaam_core::event::corporate_action::{BasisTransferRule, CorporateAction, FractionalTreatment};
use iaam_core::event::kind::{
    BasisCertainty, Certainty, DateCertainty, EventKind, FeeOrigin, IncomeKind, Knowledge,
    OpeningAssertions, TaxOrigin, TradeSide, Tristate,
};
use iaam_core::event::leg::{Leg, LegKind};
use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
use iaam_core::event::provenance::{
    ParserVersion, Provenance, RawHash, RowLocator, RuleSettlement,
};
use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
use iaam_core::event::{Confidence, Event, Relation};
use iaam_core::ids::{
    AccountId, ClassificationRuleId, CustodyId, EventId, ImportId, ImportSessionId, InstrumentId,
    OwnerId, PrincipalId, SourceId, TransferId,
};
use iaam_core::money::{CalcMoney, CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::reconciliation::Dimension;
use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
use iaam_core::rules::ReturnedShare;
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::macros::format_description;
use time::{Date, OffsetDateTime, Time};

use super::rows::{
    CashTransferRow, ControlAssertionRow, CorporateActionRow, CoverageGapDetail,
    CoverageGapDimensionRow, CoverageGapRow, CoverageGapSourceRow, DetailRows, EventRow, FeeRow,
    IncomeRow, LegRow, OfferExerciseRow, OpeningPositionRow, TaxRow, TradeRow,
    UnresolvedMovementRow, ValuationRow,
};
use crate::StoreError;

const SOURCE_TIME_FORMAT: &[time::format_description::FormatItem<'_>] =
    format_description!("[hour]:[minute]:[second].[subsecond digits:9]");

// --- Event -> rows -----------------------------------------------------

/// Every field of `event` becomes a header row, a leg row per leg, and a
/// family-specific detail row, or [`DetailRows::None`] for the five
/// variants that carry nothing beyond the legs.
pub(crate) fn to_rows(event: &Event) -> Result<(EventRow, Vec<LegRow>, DetailRows), StoreError> {
    let id = event.id.inner().to_string();

    let (relation_kind, relation_target) = match event.relation {
        Relation::None => ("none", None),
        Relation::Reversal { target } => ("reversal", Some(target.inner().to_string())),
        Relation::Replacement { target } => ("replacement", Some(target.inner().to_string())),
    };

    let (rule_settlement, settled_by_rule, settled_by_rule_version) =
        encode_rule_settlement(event.provenance.rule_settlement());

    let (row_document, row_sheet, row_number) = match event.provenance.row() {
        Some(row) => (
            Some(row.document.as_str().to_owned()),
            row.sheet.clone(),
            Some(encode_row_number(row.row)?),
        ),
        None => (None, None, None),
    };

    let header = EventRow {
        id: id.clone(),
        owner: event.owner.inner().to_string(),
        account: event.account.inner().to_string(),
        kind: event.kind.discriminant().to_owned(),
        effective_date: date_to_text(event.order.date()),
        source_time: event.order.source_time().map(source_time_to_text),
        sequence: i64::from(event.order.sequence()),
        confidence: confidence_code(event.confidence).to_owned(),
        relation_kind: relation_kind.to_owned(),
        relation_target,
        idempotency_key: event.idempotency_key.clone(),
        // `Event` carries no field for this: it is stamped by the write
        // path just before insert (Task 7).
        recorded_at: String::new(),

        date_trade: event.dates.trade.map(|d| date_to_text(d.inner())),
        date_settled: event.dates.settled.map(|d| date_to_text(d.inner())),
        date_cash_posted: event.dates.cash_posted.map(|d| date_to_text(d.inner())),
        date_entitlement: event.dates.entitlement.map(|d| date_to_text(d.inner())),
        date_paid: event.dates.paid.map(|d| date_to_text(d.inner())),
        tax_period_override: event.dates.tax_period_override.map(|p| i64::from(p.0)),

        source: event.provenance.source().inner().to_string(),
        raw_hash: event.provenance.raw_hash().as_str().to_owned(),
        parser_version: event.provenance.parser_version().0.clone(),
        source_operation_id: event.provenance.source_operation_id().map(str::to_owned),
        source_category: event.provenance.source_category().map(str::to_owned),
        source_kind: event.provenance.source_kind().map(str::to_owned),
        owner_category: event.provenance.owner_category().map(str::to_owned),
        source_code: event.provenance.source_code().map(str::to_owned),
        source_description: event.provenance.description().map(str::to_owned),
        import: event.provenance.import().map(|i| i.inner().to_string()),
        import_session: event
            .provenance
            .import_session()
            .map(|i| i.inner().to_string()),
        declared_by: event
            .provenance
            .declared_by()
            .map(|p| p.inner().to_string()),
        rule_settlement,
        settled_by_rule,
        settled_by_rule_version,
        row_document,
        row_sheet,
        row_number,
    };

    let legs = event
        .legs
        .iter()
        .enumerate()
        .map(|(ordinal, leg)| leg_to_row(&id, ordinal, leg))
        .collect();

    let detail = kind_to_detail(&id, &event.kind)?;

    Ok((header, legs, detail))
}

fn leg_to_row(event: &str, ordinal: usize, leg: &Leg) -> LegRow {
    LegRow {
        event: event.to_owned(),
        ordinal: ordinal as i64,
        kind: leg_kind_code(leg.kind).to_owned(),
        account: leg.account.inner().to_string(),
        custody: leg.custody.map(|c| c.inner().to_string()),
        instrument: leg.instrument.map(|i| i.inner().to_string()),
        amount: leg.money.map(|m| m.amount().raw()),
        currency: leg.money.map(|m| m.currency().code().to_owned()),
        quantity: leg.quantity.map(|q| dec_to_text(q.0)),
    }
}

/// The exhaustive match spec §9 requires: every `EventKind` variant is
/// matched by name, every field by name, and a new variant or field breaks
/// the build here before it can be written silently.
#[allow(clippy::too_many_lines)]
fn kind_to_detail(event: &str, kind: &EventKind) -> Result<DetailRows, StoreError> {
    match kind {
        EventKind::Trade {
            side,
            instrument,
            quantity,
            gross,
            fee,
            basis_fee,
            basis_fee_exact,
            accrued_interest,
        } => Ok(DetailRows::Trade(TradeRow {
            event: event.to_owned(),
            side: trade_side_code(*side).to_owned(),
            instrument: instrument.inner().to_string(),
            quantity: dec_to_text(quantity.0),
            gross_amount: gross.amount().raw(),
            gross_currency: gross.currency().code().to_owned(),
            fee_amount: fee.map(|m| m.amount().raw()),
            fee_currency: fee.map(|m| m.currency().code().to_owned()),
            basis_fee_amount: basis_fee.map(|m| m.amount().raw()),
            basis_fee_currency: basis_fee.map(|m| m.currency().code().to_owned()),
            basis_fee_exact_value: basis_fee_exact.map(|m| dec_to_text(m.value())),
            basis_fee_exact_currency: basis_fee_exact.map(|m| m.currency().code().to_owned()),
            accrued_interest_amount: accrued_interest.map(|m| m.amount().raw()),
            accrued_interest_currency: accrued_interest.map(|m| m.currency().code().to_owned()),
        })),
        // `CashIn.amount`, `CashOut.amount`, `Refund.amount`,
        // `OpeningCash.amount` and `OwnAccountMovement.amount` are all
        // reconstructed from the single cash leg (spec §4.5): nothing of
        // these five variants is stored beyond `events.kind` and the legs.
        EventKind::CashIn { amount: _ }
        | EventKind::CashOut { amount: _ }
        | EventKind::Refund { amount: _ }
        | EventKind::OpeningCash { amount: _ }
        | EventKind::OwnAccountMovement { amount: _ } => Ok(DetailRows::None),
        EventKind::CashTransfer {
            transfer_id,
            from,
            to,
            // Reconstructed from the positive `to` leg (spec §4.5).
            amount: _,
        } => Ok(DetailRows::CashTransfer(CashTransferRow {
            event: event.to_owned(),
            transfer_id: transfer_id.inner().to_string(),
            from_account: from.inner().to_string(),
            to_account: to.inner().to_string(),
        })),
        EventKind::UnresolvedOwnAccountMovement { amount } => {
            Ok(DetailRows::UnresolvedMovement(UnresolvedMovementRow {
                event: event.to_owned(),
                amount: amount.amount().raw(),
                currency: amount.currency().code().to_owned(),
            }))
        }
        EventKind::Income {
            instrument,
            // Reconstructed from the single cash leg (spec §4.5).
            gross: _,
            kind,
        } => Ok(DetailRows::Income(IncomeRow {
            event: event.to_owned(),
            instrument: instrument.map(|i| i.inner().to_string()),
            income_kind: kind.map(|k| income_kind_code(k).to_owned()),
        })),
        EventKind::Fee {
            // Reconstructed from the single fee leg (spec §4.5).
            amount: _,
            origin,
        } => Ok(DetailRows::Fee(FeeRow {
            event: event.to_owned(),
            origin: fee_origin_code(*origin).to_owned(),
        })),
        EventKind::Tax {
            // Reconstructed from the single tax leg (spec §4.5).
            amount: _,
            origin,
        } => Ok(DetailRows::Tax(TaxRow {
            event: event.to_owned(),
            origin: tax_origin_code(*origin).to_owned(),
        })),
        EventKind::OpeningPosition {
            instrument,
            quantity,
            cost_basis,
            assertions,
        } => {
            let OpeningAssertions {
                quantity: quantity_certainty,
                acquisition_date,
                acquisition_date_certainty,
                tax_basis,
                basis_currency,
                basis_rate,
                fees_included,
                ldv_eligibility,
                prior_corporate_actions,
            } = *assertions;
            Ok(DetailRows::OpeningPosition(Box::new(OpeningPositionRow {
                event: event.to_owned(),
                instrument: instrument.inner().to_string(),
                quantity: dec_to_text(quantity.0),
                cost_basis_amount: cost_basis.map(|m| m.amount().raw()),
                cost_basis_currency: cost_basis.map(|m| m.currency().code().to_owned()),
                quantity_certainty: certainty_code(quantity_certainty).to_owned(),
                acquisition_date: acquisition_date.map(date_to_text),
                acquisition_date_certainty: date_certainty_code(acquisition_date_certainty)
                    .to_owned(),
                tax_basis_certainty: basis_certainty_code(tax_basis).to_owned(),
                basis_currency: basis_currency.map(|c| c.code().to_owned()),
                basis_rate: basis_rate.map(dec_to_text),
                fees_included: tristate_code(fees_included).to_owned(),
                ldv_eligibility: knowledge_code(ldv_eligibility).to_owned(),
                prior_corporate_actions: knowledge_code(prior_corporate_actions).to_owned(),
            })))
        }
        EventKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } => Ok(DetailRows::Valuation(ValuationRow {
            event: event.to_owned(),
            instrument: instrument.inner().to_string(),
            price: dec_to_text(*price),
            currency: currency.code().to_owned(),
            quality: price_quality_code(*quality).to_owned(),
        })),
        EventKind::ControlAssertion { period, claim } => Ok(DetailRows::ControlAssertion(
            control_assertion_to_row(event, *period, *claim),
        )),
        EventKind::ImportCoverageGap {
            period,
            // Not stored: the union of the rows' own dimension sets (spec
            // §4.4).
            dimensions: _,
            // Not stored: `rows.len()` (spec §4.4).
            refused: _,
            rows,
        } => Ok(DetailRows::CoverageGap(coverage_gap_to_detail(
            event, *period, rows,
        ))),
        EventKind::CorporateAction { action } => Ok(DetailRows::CorporateAction(Box::new(
            corporate_action_to_row(event, action)?,
        ))),
        EventKind::OfferExercise { action } => Ok(DetailRows::OfferExercise(
            offer_exercise_to_row(event, action),
        )),
    }
}

fn control_assertion_to_row(
    event: &str,
    period: AssertionPeriod,
    claim: ControlClaim,
) -> ControlAssertionRow {
    let base = ControlAssertionRow {
        event: event.to_owned(),
        period_from: date_to_text(period.from),
        period_to: date_to_text(period.to),
        claim_kind: claim.discriminant().to_owned(),
        balance_point: None,
        currency: None,
        amount: None,
        instrument: None,
        custody: None,
        quantity: None,
        debit: None,
        credit: None,
    };
    match claim {
        ControlClaim::CashBalance {
            currency,
            amount,
            at,
        } => ControlAssertionRow {
            balance_point: Some(balance_point_code(at).to_owned()),
            currency: Some(currency.code().to_owned()),
            amount: Some(amount.raw()),
            ..base
        },
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at,
        } => ControlAssertionRow {
            balance_point: Some(balance_point_code(at).to_owned()),
            instrument: Some(instrument.inner().to_string()),
            custody: Some(custody.inner().to_string()),
            quantity: Some(dec_to_text(quantity.0)),
            ..base
        },
        ControlClaim::CashTurnover {
            currency,
            debit,
            credit,
        } => ControlAssertionRow {
            currency: Some(currency.code().to_owned()),
            debit: Some(debit.raw()),
            credit: Some(credit.raw()),
            ..base
        },
        ControlClaim::FeesTotal { currency, amount } => ControlAssertionRow {
            currency: Some(currency.code().to_owned()),
            amount: Some(amount.raw()),
            ..base
        },
        ControlClaim::IncomeTotal { currency, amount } => ControlAssertionRow {
            currency: Some(currency.code().to_owned()),
            amount: Some(amount.raw()),
            ..base
        },
        ControlClaim::TaxWithheldTotal { currency, amount } => ControlAssertionRow {
            currency: Some(currency.code().to_owned()),
            amount: Some(amount.raw()),
            ..base
        },
    }
}

fn coverage_gap_to_detail(
    event: &str,
    period: AssertionPeriod,
    rows: &[RefusedRow],
) -> CoverageGapDetail {
    let header = CoverageGapRow {
        event: event.to_owned(),
        period_from: date_to_text(period.from),
        period_to: date_to_text(period.to),
    };
    let mut sources = Vec::with_capacity(rows.len());
    let mut dimensions = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let ordinal = ordinal as i64;
        let (row_name_kind, row_name_value) = row_name_to_parts(&row.key.row);
        sources.push(CoverageGapSourceRow {
            event: event.to_owned(),
            ordinal,
            source: row.key.source.inner().to_string(),
            row_name_kind: row_name_kind.to_owned(),
            row_name_value,
        });
        for dimension in &row.dimensions {
            dimensions.push(CoverageGapDimensionRow {
                event: event.to_owned(),
                ordinal,
                dimension: dimension.code().to_owned(),
            });
        }
    }
    CoverageGapDetail {
        header,
        sources,
        dimensions,
    }
}

fn corporate_action_to_row(
    event: &str,
    action: &CorporateAction,
) -> Result<CorporateActionRow, StoreError> {
    let base = CorporateActionRow {
        event: event.to_owned(),
        action_kind: String::new(),
        instrument: None,
        predecessor: None,
        successor: None,
        custody: String::new(),
        quantity: None,
        quantity_in: None,
        quantity_out: None,
        ratio: None,
        principal_returned_per_unit_value: None,
        principal_returned_per_unit_currency: None,
        fractional: None,
        basis_transfer: None,
        effective_date: String::new(),
        record_date: None,
        grounds: None,
        allocation_kind: None,
        allocation_gap: None,
        allocation_share: None,
        allocation_inputs_hash: None,
        allocation_known_as_of: None,
        allocation_algorithm: None,
    };
    match action {
        CorporateAction::PartialRedemption {
            instrument,
            custody,
            quantity,
            principal_returned_per_unit,
            // Reconstructed from the `Principal` leg (spec §4.5).
            compensation: _,
            effective_date,
            record_date,
            grounds,
            basis_allocation,
        } => {
            let (
                allocation_kind,
                allocation_gap,
                allocation_share,
                allocation_inputs_hash,
                allocation_known_as_of,
                allocation_algorithm,
            ) = encode_basis_allocation(basis_allocation)?;
            Ok(CorporateActionRow {
                action_kind: "partial_redemption".to_owned(),
                instrument: Some(instrument.inner().to_string()),
                custody: custody.inner().to_string(),
                quantity: Some(dec_to_text(quantity.0)),
                principal_returned_per_unit_value: Some(dec_to_text(
                    principal_returned_per_unit.value(),
                )),
                principal_returned_per_unit_currency: Some(
                    principal_returned_per_unit.currency().code().to_owned(),
                ),
                effective_date: date_to_text(*effective_date),
                record_date: record_date.map(date_to_text),
                grounds: grounds.clone(),
                allocation_kind,
                allocation_gap,
                allocation_share,
                allocation_inputs_hash,
                allocation_known_as_of,
                allocation_algorithm,
                ..base
            })
        }
        CorporateAction::Redemption {
            instrument,
            custody,
            quantity,
            principal_returned_per_unit,
            // Reconstructed from the `Principal` leg (spec §4.5).
            compensation: _,
            effective_date,
            record_date,
            grounds,
        } => Ok(CorporateActionRow {
            action_kind: "redemption".to_owned(),
            instrument: Some(instrument.inner().to_string()),
            custody: custody.inner().to_string(),
            quantity: Some(dec_to_text(quantity.0)),
            principal_returned_per_unit_value: Some(dec_to_text(
                principal_returned_per_unit.value(),
            )),
            principal_returned_per_unit_currency: Some(
                principal_returned_per_unit.currency().code().to_owned(),
            ),
            effective_date: date_to_text(*effective_date),
            record_date: record_date.map(date_to_text),
            grounds: grounds.clone(),
            ..base
        }),
        CorporateAction::Conversion {
            predecessor,
            successor,
            custody,
            ratio,
            quantity_in,
            quantity_out,
            fractional,
            // Reconstructed from the optional `Cash` leg (spec §4.5).
            compensation: _,
            effective_date,
            record_date,
            grounds,
            basis_transfer,
        } => Ok(CorporateActionRow {
            action_kind: "conversion".to_owned(),
            predecessor: Some(predecessor.inner().to_string()),
            successor: Some(successor.inner().to_string()),
            custody: custody.inner().to_string(),
            quantity_in: Some(dec_to_text(quantity_in.0)),
            quantity_out: Some(dec_to_text(quantity_out.0)),
            ratio: Some(dec_to_text(*ratio)),
            fractional: Some(fractional_code(*fractional).to_owned()),
            basis_transfer: Some(basis_transfer_code(*basis_transfer).to_owned()),
            effective_date: date_to_text(*effective_date),
            record_date: record_date.map(date_to_text),
            grounds: grounds.clone(),
            ..base
        }),
    }
}

fn offer_exercise_to_row(event: &str, action: &OfferExerciseAction) -> OfferExerciseRow {
    let base = OfferExerciseRow {
        event: event.to_owned(),
        action_kind: String::new(),
        submission: String::new(),
        window: None,
        instrument: None,
        custody: None,
        quantity: String::new(),
        gross_amount: None,
        gross_currency: None,
        fee_amount: None,
        fee_currency: None,
        accrued_interest_amount: None,
        accrued_interest_currency: None,
    };
    match action {
        OfferExerciseAction::Submitted {
            submission,
            window,
            instrument,
            quantity,
        } => OfferExerciseRow {
            action_kind: "offer_submitted".to_owned(),
            submission: submission.inner().to_string(),
            window: Some(window.inner().to_string()),
            instrument: Some(instrument.inner().to_string()),
            quantity: dec_to_text(quantity.0),
            ..base
        },
        OfferExerciseAction::Cancelled {
            submission,
            quantity,
        } => OfferExerciseRow {
            action_kind: "offer_cancelled".to_owned(),
            submission: submission.inner().to_string(),
            quantity: dec_to_text(quantity.0),
            ..base
        },
        OfferExerciseAction::Settled {
            submission,
            instrument,
            custody,
            quantity,
            gross,
            fee,
            accrued_interest,
        } => OfferExerciseRow {
            action_kind: "offer_settled".to_owned(),
            submission: submission.inner().to_string(),
            instrument: Some(instrument.inner().to_string()),
            custody: Some(custody.inner().to_string()),
            quantity: dec_to_text(quantity.0),
            gross_amount: Some(gross.amount().raw()),
            gross_currency: Some(gross.currency().code().to_owned()),
            fee_amount: fee.map(|m| m.amount().raw()),
            fee_currency: fee.map(|m| m.currency().code().to_owned()),
            accrued_interest_amount: accrued_interest.map(|m| m.amount().raw()),
            accrued_interest_currency: accrued_interest.map(|m| m.currency().code().to_owned()),
            ..base
        },
    }
}

// --- Rows -> Event -------------------------------------------------------

/// Reassembles an [`Event`] from a header, its legs, and its detail rows.
///
/// Legs are reconstructed generically, in `ordinal` order, straight into
/// `Event::legs`. Everything spec §4.5 lists as reconstructed is then read
/// back out of that same leg list by kind and role — never by `ordinal` —
/// through the helpers at the bottom of this file.
pub(crate) fn from_rows(
    header: EventRow,
    mut legs: Vec<LegRow>,
    detail: DetailRows,
) -> Result<Event, StoreError> {
    let id = EventId(parse_uuid("id", &header.id)?);
    let owner = OwnerId(parse_uuid("owner", &header.owner)?);
    let account = AccountId(parse_uuid("account", &header.account)?);

    let relation = decode_relation(
        "relation_kind",
        &header.relation_kind,
        header.relation_target.as_deref(),
    )?;
    let confidence = parse_confidence(&header.confidence)?;

    let order = match header.source_time {
        Some(ref text) => EffectiveOrder::with_source_time(
            date_from_text("effective_date", &header.effective_date)?,
            source_time_from_text("source_time", text)?,
            u32_from_i64("sequence", header.sequence)?,
        ),
        None => EffectiveOrder::new(
            date_from_text("effective_date", &header.effective_date)?,
            u32_from_i64("sequence", header.sequence)?,
        ),
    };

    let dates = EventDates {
        trade: header
            .date_trade
            .as_deref()
            .map(|d| date_from_text("date_trade", d).map(TradeDate))
            .transpose()?,
        settled: header
            .date_settled
            .as_deref()
            .map(|d| date_from_text("date_settled", d).map(SettledDate))
            .transpose()?,
        cash_posted: header
            .date_cash_posted
            .as_deref()
            .map(|d| date_from_text("date_cash_posted", d).map(CashPostedDate))
            .transpose()?,
        entitlement: header
            .date_entitlement
            .as_deref()
            .map(|d| date_from_text("date_entitlement", d).map(EntitlementDate))
            .transpose()?,
        paid: header
            .date_paid
            .as_deref()
            .map(|d| date_from_text("date_paid", d).map(PaidDate))
            .transpose()?,
        tax_period_override: header
            .tax_period_override
            .map(|value| {
                Ok::<_, StoreError>(TaxPeriod(i32_from_i64("tax_period_override", value)?))
            })
            .transpose()?,
    };

    let provenance = decode_provenance(&header)?;

    legs.sort_by_key(|leg| leg.ordinal);
    let legs: Vec<Leg> = legs.into_iter().map(row_to_leg).collect::<Result<_, _>>()?;

    let kind = detail_to_kind(&header.kind, detail, &legs)?;

    Ok(Event {
        id,
        owner,
        account,
        kind,
        dates,
        order,
        legs,
        provenance,
        relation,
        confidence,
        idempotency_key: header.idempotency_key,
    })
}

fn row_to_leg(row: LegRow) -> Result<Leg, StoreError> {
    let kind = parse_leg_kind(&row.kind)?;
    let account = AccountId(parse_uuid("event_legs.account", &row.account)?);
    let custody = row
        .custody
        .as_deref()
        .map(|c| parse_uuid("event_legs.custody", c).map(CustodyId))
        .transpose()?;
    let instrument = row
        .instrument
        .as_deref()
        .map(|i| parse_uuid("event_legs.instrument", i).map(InstrumentId))
        .transpose()?;
    let money = optional_money("event_legs", row.amount, row.currency.as_deref())?;
    let quantity = row
        .quantity
        .as_deref()
        .map(|q| dec_from_text("event_legs.quantity", q).map(Quantity))
        .transpose()?;
    Ok(Leg {
        kind,
        account,
        custody,
        instrument,
        money,
        quantity,
    })
}

/// Dispatch on the stored discriminant and the detail shape. The `None`
/// detail is ambiguous between five kinds and is resolved by `kind` alone;
/// every other detail variant names its `EventKind` uniquely.
fn detail_to_kind(kind: &str, detail: DetailRows, legs: &[Leg]) -> Result<EventKind, StoreError> {
    match detail {
        DetailRows::Trade(row) => trade_from_row(row),
        DetailRows::CashTransfer(row) => cash_transfer_from_row(row, legs),
        DetailRows::UnresolvedMovement(row) => Ok(EventKind::UnresolvedOwnAccountMovement {
            amount: require_money(
                "event_unresolved_movement",
                Some(row.amount),
                Some(&row.currency),
            )?,
        }),
        DetailRows::Income(row) => income_from_row(row, legs),
        DetailRows::Fee(row) => Ok(EventKind::Fee {
            amount: single_leg_money(legs, LegKind::Fee)?,
            origin: parse_fee_origin(&row.origin)?,
        }),
        DetailRows::Tax(row) => Ok(EventKind::Tax {
            amount: single_leg_money(legs, LegKind::Tax)?,
            origin: parse_tax_origin(&row.origin)?,
        }),
        DetailRows::OpeningPosition(row) => opening_position_from_row(*row),
        DetailRows::Valuation(row) => valuation_from_row(row),
        DetailRows::ControlAssertion(row) => control_assertion_from_row(row),
        DetailRows::CoverageGap(detail) => coverage_gap_from_detail(detail),
        DetailRows::CorporateAction(row) => corporate_action_from_row(*row, legs),
        DetailRows::OfferExercise(row) => offer_exercise_from_row(row),
        DetailRows::None => none_detail_from_kind(kind, legs),
    }
}

fn none_detail_from_kind(kind: &str, legs: &[Leg]) -> Result<EventKind, StoreError> {
    match kind {
        "cash_in" => Ok(EventKind::CashIn {
            amount: single_leg_money(legs, LegKind::Cash)?,
        }),
        "cash_out" => Ok(EventKind::CashOut {
            amount: single_leg_money(legs, LegKind::Cash)?,
        }),
        "refund" => Ok(EventKind::Refund {
            amount: single_leg_money(legs, LegKind::Cash)?,
        }),
        "opening_cash" => Ok(EventKind::OpeningCash {
            amount: single_leg_money(legs, LegKind::Cash)?,
        }),
        "own_account_movement" => Ok(EventKind::OwnAccountMovement {
            amount: single_leg_money(legs, LegKind::Cash)?,
        }),
        other => Err(StoreError::InvalidValue {
            field: "kind",
            value: other.to_owned(),
        }),
    }
}

fn trade_from_row(row: TradeRow) -> Result<EventKind, StoreError> {
    Ok(EventKind::Trade {
        side: parse_trade_side(&row.side)?,
        instrument: InstrumentId(parse_uuid("event_trade.instrument", &row.instrument)?),
        quantity: Quantity(dec_from_text("event_trade.quantity", &row.quantity)?),
        gross: require_money(
            "event_trade.gross",
            Some(row.gross_amount),
            Some(&row.gross_currency),
        )?,
        fee: optional_money(
            "event_trade.fee",
            row.fee_amount,
            row.fee_currency.as_deref(),
        )?,
        basis_fee: optional_money(
            "event_trade.basis_fee",
            row.basis_fee_amount,
            row.basis_fee_currency.as_deref(),
        )?,
        basis_fee_exact: match (row.basis_fee_exact_value, row.basis_fee_exact_currency) {
            (None, None) => None,
            (Some(value), Some(currency)) => Some(CalcMoney::new(
                dec_from_text("event_trade.basis_fee_exact_value", &value)?,
                parse_currency("event_trade.basis_fee_exact_currency", &currency)?,
            )),
            _ => {
                return Err(StoreError::InvalidValue {
                    field: "event_trade.basis_fee_exact",
                    value: "amount/currency mismatch".to_owned(),
                });
            }
        },
        accrued_interest: optional_money(
            "event_trade.accrued_interest",
            row.accrued_interest_amount,
            row.accrued_interest_currency.as_deref(),
        )?,
    })
}

fn cash_transfer_from_row(row: CashTransferRow, legs: &[Leg]) -> Result<EventKind, StoreError> {
    let to = AccountId(parse_uuid(
        "event_cash_transfer.to_account",
        &row.to_account,
    )?);
    Ok(EventKind::CashTransfer {
        transfer_id: TransferId(parse_uuid(
            "event_cash_transfer.transfer_id",
            &row.transfer_id,
        )?),
        from: AccountId(parse_uuid(
            "event_cash_transfer.from_account",
            &row.from_account,
        )?),
        to,
        amount: leg_money_for_account(legs, LegKind::Cash, to)?,
    })
}

fn income_from_row(row: IncomeRow, legs: &[Leg]) -> Result<EventKind, StoreError> {
    Ok(EventKind::Income {
        instrument: row
            .instrument
            .as_deref()
            .map(|i| parse_uuid("event_income.instrument", i).map(InstrumentId))
            .transpose()?,
        gross: single_leg_money(legs, LegKind::Cash)?,
        kind: row
            .income_kind
            .as_deref()
            .map(parse_income_kind)
            .transpose()?,
    })
}

fn opening_position_from_row(row: OpeningPositionRow) -> Result<EventKind, StoreError> {
    let cost_basis = match (row.cost_basis_amount, row.cost_basis_currency) {
        (None, None) => None,
        (amount, currency) => Some(require_money(
            "event_opening_position.cost_basis",
            amount,
            currency.as_deref(),
        )?),
    };
    let assertions = OpeningAssertions {
        quantity: parse_certainty(&row.quantity_certainty)?,
        acquisition_date: row
            .acquisition_date
            .as_deref()
            .map(|d| date_from_text("event_opening_position.acquisition_date", d))
            .transpose()?,
        acquisition_date_certainty: parse_date_certainty(&row.acquisition_date_certainty)?,
        tax_basis: parse_basis_certainty(&row.tax_basis_certainty)?,
        basis_currency: row
            .basis_currency
            .as_deref()
            .map(|c| parse_currency("event_opening_position.basis_currency", c))
            .transpose()?,
        basis_rate: row
            .basis_rate
            .as_deref()
            .map(|r| dec_from_text("event_opening_position.basis_rate", r))
            .transpose()?,
        fees_included: parse_tristate(&row.fees_included)?,
        ldv_eligibility: parse_knowledge(&row.ldv_eligibility)?,
        prior_corporate_actions: parse_knowledge(&row.prior_corporate_actions)?,
    };
    Ok(EventKind::OpeningPosition {
        instrument: InstrumentId(parse_uuid(
            "event_opening_position.instrument",
            &row.instrument,
        )?),
        quantity: Quantity(dec_from_text(
            "event_opening_position.quantity",
            &row.quantity,
        )?),
        cost_basis,
        assertions,
    })
}

fn valuation_from_row(row: ValuationRow) -> Result<EventKind, StoreError> {
    Ok(EventKind::Valuation {
        instrument: InstrumentId(parse_uuid("event_valuation.instrument", &row.instrument)?),
        price: dec_from_text("event_valuation.price", &row.price)?,
        currency: parse_currency("event_valuation.currency", &row.currency)?,
        quality: parse_price_quality(&row.quality)?,
    })
}

fn control_assertion_from_row(row: ControlAssertionRow) -> Result<EventKind, StoreError> {
    let period = decode_period("event_control_assertion", &row.period_from, &row.period_to)?;
    let field = "event_control_assertion";
    let claim = match row.claim_kind.as_str() {
        "cash_balance" => ControlClaim::CashBalance {
            currency: parse_currency(field, &require_str(field, row.currency.as_deref())?)?,
            amount: PostedMinor::new(require(field, row.amount)?),
            at: parse_balance_point(&require_str(field, row.balance_point.as_deref())?)?,
        },
        "position_quantity" => ControlClaim::PositionQuantity {
            instrument: InstrumentId(parse_uuid(
                field,
                &require_str(field, row.instrument.as_deref())?,
            )?),
            custody: CustodyId(parse_uuid(
                field,
                &require_str(field, row.custody.as_deref())?,
            )?),
            quantity: Quantity(dec_from_text(
                field,
                &require_str(field, row.quantity.as_deref())?,
            )?),
            at: parse_balance_point(&require_str(field, row.balance_point.as_deref())?)?,
        },
        "cash_turnover" => ControlClaim::CashTurnover {
            currency: parse_currency(field, &require_str(field, row.currency.as_deref())?)?,
            debit: PostedMinor::new(require(field, row.debit)?),
            credit: PostedMinor::new(require(field, row.credit)?),
        },
        "fees_total" => ControlClaim::FeesTotal {
            currency: parse_currency(field, &require_str(field, row.currency.as_deref())?)?,
            amount: PostedMinor::new(require(field, row.amount)?),
        },
        "income_total" => ControlClaim::IncomeTotal {
            currency: parse_currency(field, &require_str(field, row.currency.as_deref())?)?,
            amount: PostedMinor::new(require(field, row.amount)?),
        },
        "tax_withheld_total" => ControlClaim::TaxWithheldTotal {
            currency: parse_currency(field, &require_str(field, row.currency.as_deref())?)?,
            amount: PostedMinor::new(require(field, row.amount)?),
        },
        other => {
            return Err(StoreError::InvalidValue {
                field: "claim_kind",
                value: other.to_owned(),
            });
        }
    };
    Ok(EventKind::ControlAssertion { period, claim })
}

fn coverage_gap_from_detail(detail: CoverageGapDetail) -> Result<EventKind, StoreError> {
    let CoverageGapDetail {
        header,
        mut sources,
        dimensions,
    } = detail;
    let period = decode_period("event_coverage_gap", &header.period_from, &header.period_to)?;
    sources.sort_by_key(|row| row.ordinal);
    let mut rows = Vec::with_capacity(sources.len());
    let mut union = BTreeSet::new();
    for source in &sources {
        let mut row_dimensions = BTreeSet::new();
        for dimension_row in &dimensions {
            if dimension_row.ordinal == source.ordinal {
                let dimension = parse_dimension(&dimension_row.dimension)?;
                row_dimensions.insert(dimension);
                union.insert(dimension);
            }
        }
        rows.push(RefusedRow {
            key: SourceRowKey {
                source: SourceId(parse_uuid(
                    "event_coverage_gap_rows.source",
                    &source.source,
                )?),
                row: parse_row_name(&source.row_name_kind, &source.row_name_value)?,
            },
            dimensions: row_dimensions,
        });
    }
    let refused = u32::try_from(rows.len()).map_err(|_| StoreError::InvalidValue {
        field: "refused",
        value: rows.len().to_string(),
    })?;
    Ok(EventKind::ImportCoverageGap {
        period,
        dimensions: union,
        refused,
        rows,
    })
}

fn corporate_action_from_row(
    row: CorporateActionRow,
    legs: &[Leg],
) -> Result<EventKind, StoreError> {
    let field = "event_corporate_action";
    let custody = CustodyId(parse_uuid(field, &row.custody)?);
    let effective_date = date_from_text(field, &row.effective_date)?;
    let record_date = row
        .record_date
        .as_deref()
        .map(|d| date_from_text(field, d))
        .transpose()?;
    let action = match row.action_kind.as_str() {
        "partial_redemption" => CorporateAction::PartialRedemption {
            instrument: InstrumentId(parse_uuid(
                field,
                &require_str(field, row.instrument.as_deref())?,
            )?),
            custody,
            quantity: Quantity(dec_from_text(
                field,
                &require_str(field, row.quantity.as_deref())?,
            )?),
            principal_returned_per_unit: PerUnitAmount::new(
                dec_from_text(
                    field,
                    &require_str(field, row.principal_returned_per_unit_value.as_deref())?,
                )?,
                parse_currency(
                    field,
                    &require_str(field, row.principal_returned_per_unit_currency.as_deref())?,
                )?,
            ),
            compensation: single_leg_money(legs, LegKind::Principal)?,
            effective_date,
            record_date,
            grounds: row.grounds,
            basis_allocation: decode_basis_allocation(
                row.allocation_kind.as_deref(),
                row.allocation_gap.as_deref(),
                row.allocation_share.as_deref(),
                row.allocation_inputs_hash.as_deref(),
                row.allocation_known_as_of.as_deref(),
                row.allocation_algorithm,
            )?,
        },
        "redemption" => CorporateAction::Redemption {
            instrument: InstrumentId(parse_uuid(
                field,
                &require_str(field, row.instrument.as_deref())?,
            )?),
            custody,
            quantity: Quantity(dec_from_text(
                field,
                &require_str(field, row.quantity.as_deref())?,
            )?),
            principal_returned_per_unit: PerUnitAmount::new(
                dec_from_text(
                    field,
                    &require_str(field, row.principal_returned_per_unit_value.as_deref())?,
                )?,
                parse_currency(
                    field,
                    &require_str(field, row.principal_returned_per_unit_currency.as_deref())?,
                )?,
            ),
            compensation: single_leg_money(legs, LegKind::Principal)?,
            effective_date,
            record_date,
            grounds: row.grounds,
        },
        "conversion" => CorporateAction::Conversion {
            predecessor: InstrumentId(parse_uuid(
                field,
                &require_str(field, row.predecessor.as_deref())?,
            )?),
            successor: InstrumentId(parse_uuid(
                field,
                &require_str(field, row.successor.as_deref())?,
            )?),
            custody,
            ratio: dec_from_text(field, &require_str(field, row.ratio.as_deref())?)?,
            quantity_in: Quantity(dec_from_text(
                field,
                &require_str(field, row.quantity_in.as_deref())?,
            )?),
            quantity_out: Quantity(dec_from_text(
                field,
                &require_str(field, row.quantity_out.as_deref())?,
            )?),
            fractional: parse_fractional(&require_str(field, row.fractional.as_deref())?)?,
            compensation: optional_leg_money(legs, LegKind::Cash)?,
            effective_date,
            record_date,
            grounds: row.grounds,
            basis_transfer: parse_basis_transfer(&require_str(
                field,
                row.basis_transfer.as_deref(),
            )?)?,
        },
        other => {
            return Err(StoreError::InvalidValue {
                field: "action_kind",
                value: other.to_owned(),
            });
        }
    };
    Ok(EventKind::CorporateAction { action })
}

fn offer_exercise_from_row(row: OfferExerciseRow) -> Result<EventKind, StoreError> {
    let field = "event_offer_exercise";
    let submission = OfferSubmissionId(parse_uuid(field, &row.submission)?);
    let quantity = Quantity(dec_from_text(field, &row.quantity)?);
    let action = match row.action_kind.as_str() {
        "offer_submitted" => OfferExerciseAction::Submitted {
            submission,
            window: OfferWindowId(parse_uuid(
                field,
                &require_str(field, row.window.as_deref())?,
            )?),
            instrument: InstrumentId(parse_uuid(
                field,
                &require_str(field, row.instrument.as_deref())?,
            )?),
            quantity,
        },
        "offer_cancelled" => OfferExerciseAction::Cancelled {
            submission,
            quantity,
        },
        "offer_settled" => OfferExerciseAction::Settled {
            submission,
            instrument: InstrumentId(parse_uuid(
                field,
                &require_str(field, row.instrument.as_deref())?,
            )?),
            custody: CustodyId(parse_uuid(
                field,
                &require_str(field, row.custody.as_deref())?,
            )?),
            quantity,
            gross: require_money(field, row.gross_amount, row.gross_currency.as_deref())?,
            fee: optional_money(field, row.fee_amount, row.fee_currency.as_deref())?,
            accrued_interest: optional_money(
                field,
                row.accrued_interest_amount,
                row.accrued_interest_currency.as_deref(),
            )?,
        },
        other => {
            return Err(StoreError::InvalidValue {
                field: "action_kind",
                value: other.to_owned(),
            });
        }
    };
    Ok(EventKind::OfferExercise { action })
}

fn decode_provenance(header: &EventRow) -> Result<Provenance, StoreError> {
    let source = SourceId(parse_uuid("source", &header.source)?);
    let raw_hash = RawHash::parse(&header.raw_hash).ok_or_else(|| StoreError::InvalidValue {
        field: "raw_hash",
        value: header.raw_hash.clone(),
    })?;
    let mut provenance = Provenance::new(
        source,
        raw_hash,
        ParserVersion(header.parser_version.clone()),
    );
    if let Some(value) = header.source_operation_id.clone() {
        provenance = provenance.with_source_operation_id(value);
    }
    if let Some(value) = header.source_category.clone() {
        provenance = provenance.with_source_category(value);
    }
    if let Some(value) = header.source_kind.clone() {
        provenance = provenance.with_source_kind(value);
    }
    if let Some(value) = header.owner_category.clone() {
        provenance = provenance.with_owner_category(value);
    }
    if let Some(value) = header.source_code.clone() {
        provenance = provenance.with_source_code(value);
    }
    if let Some(value) = header.source_description.clone() {
        provenance = provenance.with_description(value);
    }
    if let Some(ref value) = header.import {
        provenance = provenance.with_import(ImportId(parse_uuid("import", value)?));
    }
    if let Some(ref value) = header.import_session {
        provenance =
            provenance.with_import_session(ImportSessionId(parse_uuid("import_session", value)?));
    }
    if let Some(ref value) = header.declared_by {
        provenance = provenance.with_declared_by(PrincipalId(parse_uuid("declared_by", value)?));
    }
    if let Some(settlement) = decode_rule_settlement(
        header.rule_settlement.as_deref(),
        header.settled_by_rule.as_deref(),
        header.settled_by_rule_version,
    )? {
        provenance = provenance.with_rule_settlement(settlement);
    }
    if let Some(document) = header.row_document.clone() {
        let document = RawHash::parse(&document).ok_or_else(|| StoreError::InvalidValue {
            field: "row_document",
            value: document.clone(),
        })?;
        let row = u64_from_i64("row_number", require("row_number", header.row_number)?)?;
        provenance = provenance.with_row(RowLocator {
            document,
            sheet: header.row_sheet.clone(),
            row,
        });
    }
    Ok(provenance)
}

fn decode_relation(
    field: &'static str,
    kind: &str,
    target: Option<&str>,
) -> Result<Relation, StoreError> {
    match (kind, target) {
        ("none", None) => Ok(Relation::None),
        ("reversal", Some(target)) => Ok(Relation::Reversal {
            target: EventId(parse_uuid(field, target)?),
        }),
        ("replacement", Some(target)) => Ok(Relation::Replacement {
            target: EventId(parse_uuid(field, target)?),
        }),
        (other, _) => Err(StoreError::InvalidValue {
            field,
            value: other.to_owned(),
        }),
    }
}

fn decode_rule_settlement(
    kind: Option<&str>,
    rule: Option<&str>,
    version: Option<i64>,
) -> Result<Option<RuleSettlement>, StoreError> {
    match (kind, rule, version) {
        (None, None, None) => Ok(None),
        (Some("no_rule"), None, None) => Ok(Some(RuleSettlement::NoRule)),
        (Some("rule"), Some(rule), Some(version)) => Ok(Some(RuleSettlement::Rule {
            rule: ClassificationRuleId(parse_uuid("settled_by_rule", rule)?),
            version: u32_from_i64("settled_by_rule_version", version)?,
        })),
        (Some("answered_minting_rule"), Some(rule), Some(version)) => {
            Ok(Some(RuleSettlement::AnsweredMintingRule {
                rule: ClassificationRuleId(parse_uuid("settled_by_rule", rule)?),
                version: u32_from_i64("settled_by_rule_version", version)?,
            }))
        }
        _ => Err(StoreError::InvalidValue {
            field: "rule_settlement",
            value: format!("{kind:?}/{rule:?}/{version:?}"),
        }),
    }
}

fn encode_rule_settlement(
    settlement: Option<&RuleSettlement>,
) -> (Option<String>, Option<String>, Option<i64>) {
    match settlement {
        None => (None, None, None),
        Some(RuleSettlement::NoRule) => (Some("no_rule".to_owned()), None, None),
        Some(RuleSettlement::Rule { rule, version }) => (
            Some("rule".to_owned()),
            Some(rule.inner().to_string()),
            Some(i64::from(*version)),
        ),
        Some(RuleSettlement::AnsweredMintingRule { rule, version }) => (
            Some("answered_minting_rule".to_owned()),
            Some(rule.inner().to_string()),
            Some(i64::from(*version)),
        ),
    }
}

/// `(allocation_kind, allocation_gap, allocation_share, allocation_inputs_hash,
/// allocation_known_as_of, allocation_algorithm)` — the six
/// `event_corporate_action` columns spec §4.3 gives `BasisAllocation`.
type EncodedBasisAllocation = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

fn encode_basis_allocation(
    allocation: &BasisAllocation,
) -> Result<EncodedBasisAllocation, StoreError> {
    match allocation {
        BasisAllocation::Unknown(gap) => Ok((
            Some("unknown".to_owned()),
            Some(gap.code().to_owned()),
            None,
            None,
            None,
            None,
        )),
        BasisAllocation::Known { share, evidence } => Ok((
            Some("known".to_owned()),
            None,
            Some(dec_to_text(share.inner())),
            Some(evidence.inputs_hash.as_str().to_owned()),
            Some(offset_date_time_to_text(
                "allocation_known_as_of",
                evidence.knowledge_as_of,
            )?),
            Some(i64::from(evidence.algorithm_version.0)),
        )),
    }
}

fn decode_basis_allocation(
    kind: Option<&str>,
    gap: Option<&str>,
    share: Option<&str>,
    inputs_hash: Option<&str>,
    known_as_of: Option<&str>,
    algorithm: Option<i64>,
) -> Result<BasisAllocation, StoreError> {
    let field = "allocation_kind";
    match kind {
        Some("unknown") => Ok(BasisAllocation::Unknown(parse_allocation_gap(
            &require_str(field, gap)?,
        )?)),
        Some("known") => {
            let share = dec_from_text("allocation_share", &require_str(field, share)?)?;
            let share = ReturnedShare::new(share).map_err(|error| StoreError::InvalidValue {
                field: "allocation_share",
                value: error.to_string(),
            })?;
            let inputs_hash = require_str(field, inputs_hash)?;
            let inputs_hash = AllocationInputsHash::new(inputs_hash).map_err(|error| {
                StoreError::InvalidValue {
                    field: "allocation_inputs_hash",
                    value: error.to_string(),
                }
            })?;
            let knowledge_as_of = offset_date_time_from_text(
                "allocation_known_as_of",
                &require_str(field, known_as_of)?,
            )?;
            let algorithm_version = AllocationAlgorithmVersion(u16_from_i64(
                "allocation_algorithm",
                require("allocation_algorithm", algorithm)?,
            )?);
            Ok(BasisAllocation::Known {
                share,
                evidence: AllocationEvidence {
                    inputs_hash,
                    knowledge_as_of,
                    algorithm_version,
                },
            })
        }
        other => Err(StoreError::InvalidValue {
            field,
            value: other.unwrap_or("missing").to_owned(),
        }),
    }
}

fn decode_period(field: &'static str, from: &str, to: &str) -> Result<AssertionPeriod, StoreError> {
    let from = date_from_text(field, from)?;
    let to = date_from_text(field, to)?;
    AssertionPeriod::between(from, to).ok_or(StoreError::InvalidValue {
        field,
        value: format!("{from} .. {to}"),
    })
}

fn row_name_to_parts(name: &RowName) -> (&'static str, String) {
    match name {
        RowName::Given(value) => ("given", value.clone()),
        RowName::Fingerprint(value) => ("fingerprint", value.clone()),
    }
}

fn parse_row_name(kind: &str, value: &str) -> Result<RowName, StoreError> {
    match kind {
        "given" => Ok(RowName::Given(value.to_owned())),
        "fingerprint" => Ok(RowName::Fingerprint(value.to_owned())),
        other => Err(StoreError::InvalidValue {
            field: "row_name_kind",
            value: other.to_owned(),
        }),
    }
}

fn parse_dimension(text: &str) -> Result<Dimension, StoreError> {
    Dimension::all()
        .into_iter()
        .find(|dimension| dimension.code() == text)
        .ok_or_else(|| StoreError::InvalidValue {
            field: "dimension",
            value: text.to_owned(),
        })
}

fn parse_allocation_gap(text: &str) -> Result<AllocationGap, StoreError> {
    AllocationGap::ALL
        .into_iter()
        .find(|gap| gap.code() == text)
        .ok_or_else(|| StoreError::InvalidValue {
            field: "allocation_gap",
            value: text.to_owned(),
        })
}

fn balance_point_code(value: BalancePoint) -> &'static str {
    value.code()
}

fn parse_balance_point(text: &str) -> Result<BalancePoint, StoreError> {
    match text {
        "opening" => Ok(BalancePoint::Opening),
        "closing" => Ok(BalancePoint::Closing),
        other => Err(StoreError::InvalidValue {
            field: "balance_point",
            value: other.to_owned(),
        }),
    }
}

fn confidence_code(value: Confidence) -> &'static str {
    match value {
        Confidence::Known => "known",
        Confidence::Estimated => "estimated",
        Confidence::Unknown => "unknown",
    }
}

fn parse_confidence(text: &str) -> Result<Confidence, StoreError> {
    match text {
        "known" => Ok(Confidence::Known),
        "estimated" => Ok(Confidence::Estimated),
        "unknown" => Ok(Confidence::Unknown),
        other => Err(StoreError::InvalidValue {
            field: "confidence",
            value: other.to_owned(),
        }),
    }
}

fn leg_kind_code(value: LegKind) -> &'static str {
    match value {
        LegKind::Cash => "cash",
        LegKind::SecurityQuantity => "security_quantity",
        LegKind::Principal => "principal",
        LegKind::Fee => "fee",
        LegKind::Tax => "tax",
    }
}

fn parse_leg_kind(text: &str) -> Result<LegKind, StoreError> {
    match text {
        "cash" => Ok(LegKind::Cash),
        "security_quantity" => Ok(LegKind::SecurityQuantity),
        "principal" => Ok(LegKind::Principal),
        "fee" => Ok(LegKind::Fee),
        "tax" => Ok(LegKind::Tax),
        other => Err(StoreError::InvalidValue {
            field: "event_legs.kind",
            value: other.to_owned(),
        }),
    }
}

fn trade_side_code(value: TradeSide) -> &'static str {
    match value {
        TradeSide::Buy => "buy",
        TradeSide::Sell => "sell",
    }
}

fn parse_trade_side(text: &str) -> Result<TradeSide, StoreError> {
    match text {
        "buy" => Ok(TradeSide::Buy),
        "sell" => Ok(TradeSide::Sell),
        other => Err(StoreError::InvalidValue {
            field: "side",
            value: other.to_owned(),
        }),
    }
}

fn income_kind_code(value: IncomeKind) -> &'static str {
    match value {
        IncomeKind::Coupon => "coupon",
        IncomeKind::Dividend => "dividend",
        IncomeKind::DepositInterest => "deposit_interest",
    }
}

fn parse_income_kind(text: &str) -> Result<IncomeKind, StoreError> {
    match text {
        "coupon" => Ok(IncomeKind::Coupon),
        "dividend" => Ok(IncomeKind::Dividend),
        "deposit_interest" => Ok(IncomeKind::DepositInterest),
        other => Err(StoreError::InvalidValue {
            field: "income_kind",
            value: other.to_owned(),
        }),
    }
}

fn fee_origin_code(value: FeeOrigin) -> &'static str {
    match value {
        FeeOrigin::Brokerage => "brokerage",
        FeeOrigin::Depositary => "depositary",
        FeeOrigin::AccountMaintenance => "account_maintenance",
        FeeOrigin::MarginInterest => "margin_interest",
        FeeOrigin::Other => "other",
    }
}

fn parse_fee_origin(text: &str) -> Result<FeeOrigin, StoreError> {
    match text {
        "brokerage" => Ok(FeeOrigin::Brokerage),
        "depositary" => Ok(FeeOrigin::Depositary),
        "account_maintenance" => Ok(FeeOrigin::AccountMaintenance),
        "margin_interest" => Ok(FeeOrigin::MarginInterest),
        "other" => Ok(FeeOrigin::Other),
        other => Err(StoreError::InvalidValue {
            field: "origin",
            value: other.to_owned(),
        }),
    }
}

fn tax_origin_code(value: TaxOrigin) -> &'static str {
    match value {
        TaxOrigin::WithheldAtSource => "withheld_at_source",
        TaxOrigin::SelfPaid => "self_paid",
    }
}

fn parse_tax_origin(text: &str) -> Result<TaxOrigin, StoreError> {
    match text {
        "withheld_at_source" => Ok(TaxOrigin::WithheldAtSource),
        "self_paid" => Ok(TaxOrigin::SelfPaid),
        other => Err(StoreError::InvalidValue {
            field: "origin",
            value: other.to_owned(),
        }),
    }
}

fn certainty_code(value: Certainty) -> &'static str {
    match value {
        Certainty::Known => "known",
        Certainty::Estimated => "estimated",
    }
}

fn parse_certainty(text: &str) -> Result<Certainty, StoreError> {
    match text {
        "known" => Ok(Certainty::Known),
        "estimated" => Ok(Certainty::Estimated),
        other => Err(StoreError::InvalidValue {
            field: "quantity_certainty",
            value: other.to_owned(),
        }),
    }
}

fn date_certainty_code(value: DateCertainty) -> &'static str {
    match value {
        DateCertainty::Known => "known",
        DateCertainty::Estimated => "estimated",
        DateCertainty::Unknown => "unknown",
    }
}

fn parse_date_certainty(text: &str) -> Result<DateCertainty, StoreError> {
    match text {
        "known" => Ok(DateCertainty::Known),
        "estimated" => Ok(DateCertainty::Estimated),
        "unknown" => Ok(DateCertainty::Unknown),
        other => Err(StoreError::InvalidValue {
            field: "acquisition_date_certainty",
            value: other.to_owned(),
        }),
    }
}

fn basis_certainty_code(value: BasisCertainty) -> &'static str {
    match value {
        BasisCertainty::Documented => "documented",
        BasisCertainty::Estimated => "estimated",
        BasisCertainty::Unknown => "unknown",
    }
}

fn parse_basis_certainty(text: &str) -> Result<BasisCertainty, StoreError> {
    match text {
        "documented" => Ok(BasisCertainty::Documented),
        "estimated" => Ok(BasisCertainty::Estimated),
        "unknown" => Ok(BasisCertainty::Unknown),
        other => Err(StoreError::InvalidValue {
            field: "tax_basis_certainty",
            value: other.to_owned(),
        }),
    }
}

fn tristate_code(value: Tristate) -> &'static str {
    match value {
        Tristate::Yes => "yes",
        Tristate::No => "no",
        Tristate::Unknown => "unknown",
    }
}

fn parse_tristate(text: &str) -> Result<Tristate, StoreError> {
    match text {
        "yes" => Ok(Tristate::Yes),
        "no" => Ok(Tristate::No),
        "unknown" => Ok(Tristate::Unknown),
        other => Err(StoreError::InvalidValue {
            field: "fees_included",
            value: other.to_owned(),
        }),
    }
}

fn knowledge_code(value: Knowledge) -> &'static str {
    match value {
        Knowledge::Known => "known",
        Knowledge::Unknown => "unknown",
    }
}

fn parse_knowledge(text: &str) -> Result<Knowledge, StoreError> {
    match text {
        "known" => Ok(Knowledge::Known),
        "unknown" => Ok(Knowledge::Unknown),
        other => Err(StoreError::InvalidValue {
            field: "knowledge",
            value: other.to_owned(),
        }),
    }
}

fn price_quality_code(value: PriceQuality) -> &'static str {
    value.code()
}

fn parse_price_quality(text: &str) -> Result<PriceQuality, StoreError> {
    match text {
        "executable" => Ok(PriceQuality::Executable),
        "previous_close" => Ok(PriceQuality::PreviousClose),
        "carried_forward" => Ok(PriceQuality::CarriedForward),
        "stale" => Ok(PriceQuality::Stale),
        "owner_estimate" => Ok(PriceQuality::OwnerEstimate),
        other => Err(StoreError::InvalidValue {
            field: "quality",
            value: other.to_owned(),
        }),
    }
}

fn fractional_code(value: FractionalTreatment) -> &'static str {
    match value {
        FractionalTreatment::CashCompensated => "cash_compensated",
        FractionalTreatment::RoundedDown => "rounded_down",
        FractionalTreatment::NotApplicable => "not_applicable",
    }
}

fn parse_fractional(text: &str) -> Result<FractionalTreatment, StoreError> {
    match text {
        "cash_compensated" => Ok(FractionalTreatment::CashCompensated),
        "rounded_down" => Ok(FractionalTreatment::RoundedDown),
        "not_applicable" => Ok(FractionalTreatment::NotApplicable),
        other => Err(StoreError::InvalidValue {
            field: "fractional",
            value: other.to_owned(),
        }),
    }
}

fn basis_transfer_code(value: BasisTransferRule) -> &'static str {
    match value {
        BasisTransferRule::CarryOver => "carry_over",
        BasisTransferRule::Restart => "restart",
    }
}

fn parse_basis_transfer(text: &str) -> Result<BasisTransferRule, StoreError> {
    match text {
        "carry_over" => Ok(BasisTransferRule::CarryOver),
        "restart" => Ok(BasisTransferRule::Restart),
        other => Err(StoreError::InvalidValue {
            field: "basis_transfer",
            value: other.to_owned(),
        }),
    }
}

// --- Scalar codecs ---------------------------------------------------------

fn dec_to_text(value: Dec) -> String {
    value.inner().to_string()
}

fn dec_from_text(field: &'static str, text: &str) -> Result<Dec, StoreError> {
    Decimal::from_str_exact(text)
        .map(Dec::new)
        .map_err(|_| StoreError::InvalidValue {
            field,
            value: text.to_owned(),
        })
}

fn date_to_text(value: Date) -> String {
    value.to_string()
}

fn date_from_text(field: &'static str, text: &str) -> Result<Date, StoreError> {
    Date::parse(text, &Iso8601::DATE).map_err(|_| StoreError::InvalidValue {
        field,
        value: text.to_owned(),
    })
}

fn source_time_to_text(value: Time) -> String {
    value
        .format(SOURCE_TIME_FORMAT)
        .unwrap_or_else(|_| value.to_string())
}

fn source_time_from_text(field: &'static str, text: &str) -> Result<Time, StoreError> {
    Time::parse(text, SOURCE_TIME_FORMAT).map_err(|_| StoreError::InvalidValue {
        field,
        value: text.to_owned(),
    })
}

fn offset_date_time_to_text(
    field: &'static str,
    value: OffsetDateTime,
) -> Result<String, StoreError> {
    value
        .format(&Rfc3339)
        .map_err(|_| StoreError::InvalidValue {
            field,
            value: format!("{value:?}"),
        })
}

fn offset_date_time_from_text(
    field: &'static str,
    text: &str,
) -> Result<OffsetDateTime, StoreError> {
    OffsetDateTime::parse(text, &Rfc3339).map_err(|_| StoreError::InvalidValue {
        field,
        value: text.to_owned(),
    })
}

fn parse_uuid(field: &'static str, text: &str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(text).map_err(|_| StoreError::InvalidValue {
        field,
        value: text.to_owned(),
    })
}

fn parse_currency(field: &'static str, code: &str) -> Result<CurrencyCode, StoreError> {
    CurrencyCode::from_code(code).ok_or_else(|| StoreError::InvalidValue {
        field,
        value: code.to_owned(),
    })
}

fn require<T>(field: &'static str, value: Option<T>) -> Result<T, StoreError> {
    value.ok_or(StoreError::InvalidValue {
        field,
        value: "missing".to_owned(),
    })
}

fn require_str(field: &'static str, value: Option<&str>) -> Result<String, StoreError> {
    value.map(str::to_owned).ok_or(StoreError::InvalidValue {
        field,
        value: "missing".to_owned(),
    })
}

fn require_money(
    field: &'static str,
    amount: Option<i64>,
    currency: Option<&str>,
) -> Result<Money, StoreError> {
    match (amount, currency) {
        (Some(amount), Some(currency)) => Ok(Money::new(
            PostedMinor::new(amount),
            parse_currency(field, currency)?,
        )),
        _ => Err(StoreError::InvalidValue {
            field,
            value: "missing amount/currency".to_owned(),
        }),
    }
}

fn optional_money(
    field: &'static str,
    amount: Option<i64>,
    currency: Option<&str>,
) -> Result<Option<Money>, StoreError> {
    match (amount, currency) {
        (None, None) => Ok(None),
        (Some(amount), Some(currency)) => Ok(Some(Money::new(
            PostedMinor::new(amount),
            parse_currency(field, currency)?,
        ))),
        _ => Err(StoreError::InvalidValue {
            field,
            value: "amount/currency mismatch".to_owned(),
        }),
    }
}

fn encode_row_number(row: u64) -> Result<i64, StoreError> {
    i64::try_from(row).map_err(|_| StoreError::RowNumberOutOfRange { row })
}

fn u64_from_i64(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidValue {
        field,
        value: value.to_string(),
    })
}

fn u32_from_i64(field: &'static str, value: i64) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| StoreError::InvalidValue {
        field,
        value: value.to_string(),
    })
}

fn u16_from_i64(field: &'static str, value: i64) -> Result<u16, StoreError> {
    u16::try_from(value).map_err(|_| StoreError::InvalidValue {
        field,
        value: value.to_string(),
    })
}

fn i32_from_i64(field: &'static str, value: i64) -> Result<i32, StoreError> {
    i32::try_from(value).map_err(|_| StoreError::InvalidValue {
        field,
        value: value.to_string(),
    })
}

// --- Typed leg lookups (spec §4.5, §4.2) -----------------------------------
//
// Never a search by `ordinal`: a leg is found by kind, or by kind and
// account, and never by position.

/// Exactly one leg of `kind` is expected; its money is the reconstructed
/// value.
fn single_leg_money(legs: &[Leg], kind: LegKind) -> Result<Money, StoreError> {
    let matches: Vec<&Leg> = legs.iter().filter(|leg| leg.kind == kind).collect();
    match matches.as_slice() {
        [leg] => leg.money.ok_or(StoreError::InvalidValue {
            field: "event_legs.amount",
            value: format!("{kind:?} leg without money"),
        }),
        other => Err(StoreError::InvalidValue {
            field: "event_legs.kind",
            value: format!("expected exactly one {kind:?} leg, found {}", other.len()),
        }),
    }
}

/// At most one leg of `kind` may be present; its money is the reconstructed
/// value where one exists.
fn optional_leg_money(legs: &[Leg], kind: LegKind) -> Result<Option<Money>, StoreError> {
    let matches: Vec<&Leg> = legs.iter().filter(|leg| leg.kind == kind).collect();
    match matches.as_slice() {
        [] => Ok(None),
        [leg] => Ok(Some(leg.money.ok_or(StoreError::InvalidValue {
            field: "event_legs.amount",
            value: format!("{kind:?} leg without money"),
        })?)),
        other => Err(StoreError::InvalidValue {
            field: "event_legs.kind",
            value: format!("expected at most one {kind:?} leg, found {}", other.len()),
        }),
    }
}

/// The leg of `kind` on the named account: used for `CashTransfer`, whose
/// two `Cash` legs carry no role beyond the account they sit on.
fn leg_money_for_account(
    legs: &[Leg],
    kind: LegKind,
    account: AccountId,
) -> Result<Money, StoreError> {
    let matches: Vec<&Leg> = legs
        .iter()
        .filter(|leg| leg.kind == kind && leg.account == account)
        .collect();
    match matches.as_slice() {
        [leg] => leg.money.ok_or(StoreError::InvalidValue {
            field: "event_legs.amount",
            value: format!("{kind:?} leg on {account:?} without money"),
        }),
        other => Err(StoreError::InvalidValue {
            field: "event_legs.account",
            value: format!(
                "expected exactly one {kind:?} leg on {account:?}, found {}",
                other.len()
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use iaam_core::dates::{
        CashPostedDate, EntitlementDate, PaidDate, SettledDate, TaxPeriod, TradeDate,
    };
    use iaam_core::event::allocation::{
        AllocationAlgorithmVersion, AllocationEvidence, AllocationGap, AllocationInputsHash,
        BasisAllocation,
    };
    use iaam_core::event::corporate_action::{
        BasisTransferRule, CorporateAction, FractionalTreatment,
    };
    use iaam_core::event::kind::{
        BasisCertainty, Certainty, DateCertainty, EventKind, FeeOrigin, IncomeKind, Knowledge,
        OpeningAssertions, TaxOrigin, Tristate,
    };
    use iaam_core::event::leg::Leg;
    use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
    use iaam_core::event::provenance::{
        ParserVersion, Provenance, RawHash, RowLocator, RuleSettlement,
    };
    use iaam_core::event::source_row::{RefusedRow, RowName, SourceRowKey};
    use iaam_core::event::{Confidence, Event, Relation};
    use iaam_core::ids::{
        AccountId, ClassificationRuleId, CustodyId, EventId, ImportId, ImportSessionId,
        InstrumentId, OwnerId, PrincipalId, SourceId, TransferId,
    };
    use iaam_core::money::{CalcMoney, CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use iaam_core::reconciliation::Dimension;
    use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
    use iaam_core::rules::ReturnedShare;
    use iaam_core::valuation::PriceQuality;
    use rust_decimal::Decimal;
    use std::collections::BTreeSet;
    use time::macros::{date, datetime, time};

    use super::{from_rows, to_rows};

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn usd(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Usd)
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn qty(text: &str) -> Quantity {
        Quantity(dec(text))
    }

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(dec(text), CurrencyCode::Rub)
    }

    /// A base event envelope, overwritten field by field per fixture. Named
    /// `Main`/`Savings`/`Shop One`-style identifiers are minted fresh below,
    /// never derived from a real export.
    struct Fixture {
        account: AccountId,
        other_account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                account: AccountId::new_random(),
                other_account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
            }
        }

        /// A minimal, valid envelope around `kind` and `legs`. Individual
        /// fixtures below override `dates`, `relation`, `confidence`,
        /// `idempotency_key` and provenance fields as needed to cover every
        /// `Option` combination spec §9 requires.
        fn event(&self, kind: EventKind, legs: Vec<Leg>) -> Event {
            Event {
                id: EventId::new_random(),
                owner: OwnerId::new_random(),
                account: self.account,
                kind,
                dates: iaam_core::dates::EventDates::empty(),
                order: iaam_core::dates::EffectiveOrder::new(date!(2026 - 03 - 01), 7),
                legs,
                provenance: Provenance::new(
                    SourceId::new_random(),
                    RawHash::parse(&"a".repeat(64)).unwrap(),
                    ParserVersion("test/1".to_owned()),
                ),
                relation: Relation::None,
                confidence: Confidence::Known,
                idempotency_key: None,
            }
        }
    }

    fn assert_round_trip(event: &Event) {
        let (header, legs, detail) = to_rows(event).expect("to_rows");
        let back = from_rows(header, legs, detail).expect("from_rows");
        assert_eq!(
            &back,
            event,
            "round trip lost something in {}",
            event.kind.discriminant()
        );
    }

    // A long straight-line sequence of `.push()` calls reads better here
    // than a single `vec![]` literal spanning hundreds of lines interleaved
    // with `for` loops over each family's sub-variants.
    #[allow(clippy::vec_init_then_push)]
    fn every_event_shape() -> Vec<Event> {
        let f = Fixture::new();
        let mut events = Vec::new();

        // --- The five leg-only kinds -------------------------------------
        events.push(f.event(
            EventKind::CashIn {
                amount: rub(1_000_000),
            },
            vec![Leg::cash(f.account, rub(1_000_000))],
        ));
        events.push(f.event(
            EventKind::CashOut {
                amount: rub(-1_000_000),
            },
            vec![Leg::cash(f.account, rub(-1_000_000))],
        ));
        events.push(f.event(
            EventKind::Refund {
                amount: rub(50_000),
            },
            vec![Leg::cash(f.account, rub(50_000))],
        ));
        events.push(f.event(
            EventKind::OpeningCash { amount: rub(0) },
            vec![Leg::cash(f.account, rub(0))],
        ));
        events.push(f.event(
            EventKind::OpeningCash {
                amount: rub(i64::MIN),
            },
            vec![Leg::cash(f.account, rub(i64::MIN))],
        ));
        events.push(f.event(
            EventKind::OpeningCash {
                amount: rub(i64::MAX),
            },
            vec![Leg::cash(f.account, rub(i64::MAX))],
        ));
        events.push(f.event(
            EventKind::OwnAccountMovement {
                amount: rub(-2_500),
            },
            vec![Leg::cash(f.account, rub(-2_500))],
        ));

        // --- CashTransfer, both directions relative to `account` ----------
        events.push(f.event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: f.account,
                to: f.other_account,
                amount: rub(300_000),
            },
            vec![
                Leg::cash(f.account, rub(-300_000)),
                Leg::cash(f.other_account, rub(300_000)),
            ],
        ));
        events.push(f.event(
            EventKind::CashTransfer {
                transfer_id: TransferId::new_random(),
                from: f.other_account,
                to: f.account,
                amount: usd(700_000),
            },
            vec![
                Leg::cash(f.other_account, usd(-700_000)),
                Leg::cash(f.account, usd(700_000)),
            ],
        ));

        // --- UnresolvedOwnAccountMovement: no legs -------------------------
        events.push(f.event(
            EventKind::UnresolvedOwnAccountMovement { amount: rub(4_200) },
            vec![],
        ));

        // --- Income: every Option combination ------------------------------
        events.push(f.event(
            EventKind::Income {
                instrument: None,
                gross: rub(120_000),
                kind: None,
            },
            vec![Leg::cash(f.account, rub(120_000))],
        ));
        events.push(f.event(
            EventKind::Income {
                instrument: Some(f.instrument),
                gross: rub(120_000),
                kind: Some(IncomeKind::Coupon),
            },
            vec![Leg::cash(f.account, rub(120_000))],
        ));
        events.push(f.event(
            EventKind::Income {
                instrument: Some(f.instrument),
                gross: usd(1),
                kind: Some(IncomeKind::Dividend),
            },
            vec![Leg::cash(f.account, usd(1))],
        ));
        events.push(f.event(
            EventKind::Income {
                instrument: None,
                gross: rub(9),
                kind: Some(IncomeKind::DepositInterest),
            },
            vec![Leg::cash(f.account, rub(9))],
        ));

        // --- Fee: every origin ----------------------------------------------
        for origin in [
            FeeOrigin::Brokerage,
            FeeOrigin::Depositary,
            FeeOrigin::AccountMaintenance,
            FeeOrigin::MarginInterest,
            FeeOrigin::Other,
        ] {
            events.push(f.event(
                EventKind::Fee {
                    amount: rub(-350),
                    origin,
                },
                vec![Leg::fee(f.account, rub(-350))],
            ));
        }

        // --- Tax: every origin ------------------------------------------------
        for origin in [TaxOrigin::WithheldAtSource, TaxOrigin::SelfPaid] {
            events.push(f.event(
                EventKind::Tax {
                    amount: rub(-1_300),
                    origin,
                },
                vec![Leg::tax(f.account, rub(-1_300))],
            ));
        }

        // --- Trade: every Option combination on the extra fields -----------
        events.push(f.event(
            EventKind::Trade {
                side: iaam_core::event::kind::TradeSide::Buy,
                instrument: f.instrument,
                quantity: qty("100"),
                gross: rub(5_000_000),
                fee: None,
                basis_fee: None,
                basis_fee_exact: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(f.account, rub(-5_000_000)),
                Leg::security(f.account, f.custody, f.instrument, qty("100")),
            ],
        ));
        events.push(f.event(
            EventKind::Trade {
                side: iaam_core::event::kind::TradeSide::Sell,
                instrument: f.instrument,
                quantity: qty("50.5"),
                gross: rub(2_500_000),
                fee: Some(rub(-3_500)),
                basis_fee: Some(rub(-3_500)),
                basis_fee_exact: Some(CalcMoney::new(dec("35.001"), CurrencyCode::Rub)),
                accrued_interest: Some(rub(12_000)),
            },
            vec![
                Leg::cash(f.account, rub(2_508_500)),
                Leg::security(f.account, f.custody, f.instrument, qty("-50.5")),
            ],
        ));

        // --- OpeningPosition: default and fully-populated assertions -------
        events.push(f.event(
            EventKind::OpeningPosition {
                instrument: f.instrument,
                quantity: qty("10"),
                cost_basis: None,
                assertions: OpeningAssertions::default(),
            },
            vec![Leg::security(f.account, f.custody, f.instrument, qty("10"))],
        ));
        events.push(f.event(
            EventKind::OpeningPosition {
                instrument: f.instrument,
                quantity: qty("25.25"),
                cost_basis: Some(rub(1_000_000)),
                assertions: OpeningAssertions {
                    quantity: Certainty::Known,
                    acquisition_date: Some(date!(2020 - 01 - 15)),
                    acquisition_date_certainty: DateCertainty::Known,
                    tax_basis: BasisCertainty::Documented,
                    basis_currency: Some(CurrencyCode::Usd),
                    basis_rate: Some(dec("91.5000")),
                    fees_included: Tristate::Yes,
                    ldv_eligibility: Knowledge::Known,
                    prior_corporate_actions: Knowledge::Known,
                },
            },
            vec![Leg::security(
                f.account,
                f.custody,
                f.instrument,
                qty("25.25"),
            )],
        ));

        // --- Valuation: every quality --------------------------------------
        for quality in [
            PriceQuality::Executable,
            PriceQuality::PreviousClose,
            PriceQuality::CarriedForward,
            PriceQuality::Stale,
            PriceQuality::OwnerEstimate,
        ] {
            events.push(f.event(
                EventKind::Valuation {
                    instrument: f.instrument,
                    price: dec("123.4500"),
                    currency: CurrencyCode::Rub,
                    quality,
                },
                vec![],
            ));
        }

        // --- ControlAssertion: every ControlClaim variant --------------------
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        let claims = [
            ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(-500),
                at: BalancePoint::Opening,
            },
            ControlClaim::CashBalance {
                currency: CurrencyCode::Usd,
                amount: PostedMinor::new(2_000),
                at: BalancePoint::Closing,
            },
            ControlClaim::PositionQuantity {
                instrument: f.instrument,
                custody: f.custody,
                quantity: qty("15"),
                at: BalancePoint::Opening,
            },
            ControlClaim::PositionQuantity {
                instrument: f.instrument,
                custody: f.custody,
                quantity: qty("0"),
                at: BalancePoint::Closing,
            },
            ControlClaim::CashTurnover {
                currency: CurrencyCode::Rub,
                debit: PostedMinor::new(1_000),
                credit: PostedMinor::new(2_000),
            },
            ControlClaim::FeesTotal {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(700),
            },
            ControlClaim::IncomeTotal {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(900),
            },
            ControlClaim::TaxWithheldTotal {
                currency: CurrencyCode::Rub,
                amount: PostedMinor::new(300),
            },
        ];
        for claim in claims {
            events.push(f.event(EventKind::ControlAssertion { period, claim }, vec![]));
        }

        // --- ImportCoverageGap: empty and non-empty rows, given and
        //     fingerprint names, several dimensions ----------------------
        events.push(f.event(
            EventKind::ImportCoverageGap {
                period,
                dimensions: BTreeSet::from([Dimension::Cash]),
                refused: 1,
                rows: vec![RefusedRow {
                    key: SourceRowKey {
                        source: SourceId::new_random(),
                        row: RowName::Given("OP-1".to_owned()),
                    },
                    dimensions: BTreeSet::from([Dimension::Cash]),
                }],
            },
            vec![],
        ));
        events.push(f.event(
            EventKind::ImportCoverageGap {
                period,
                dimensions: BTreeSet::from([
                    Dimension::Cash,
                    Dimension::Positions,
                    Dimension::Income,
                ]),
                refused: 2,
                rows: vec![
                    RefusedRow {
                        key: SourceRowKey {
                            source: SourceId::new_random(),
                            row: RowName::Given("OP-2".to_owned()),
                        },
                        dimensions: BTreeSet::from([Dimension::Cash, Dimension::Positions]),
                    },
                    RefusedRow {
                        key: SourceRowKey {
                            source: SourceId::new_random(),
                            row: RowName::Fingerprint("f".repeat(64)),
                        },
                        dimensions: BTreeSet::from([Dimension::Income]),
                    },
                ],
            },
            vec![],
        ));

        // --- CorporateAction: PartialRedemption, with Unknown and Known
        //     basis allocation -------------------------------------------
        events.push(f.event(
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument: f.instrument,
                    custody: f.custody,
                    quantity: qty("10"),
                    principal_returned_per_unit: per_unit("200"),
                    compensation: rub(2_000_000),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation: BasisAllocation::Unknown(AllocationGap::NotComputed),
                },
            },
            vec![Leg::principal(f.account, f.instrument, rub(2_000_000))],
        ));
        events.push(f.event(
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument: f.instrument,
                    custody: f.custody,
                    quantity: qty("10"),
                    principal_returned_per_unit: per_unit("200.0000"),
                    compensation: rub(i64::MAX),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: Some(date!(2026 - 06 - 13)),
                    grounds: Some("decision no. 4".to_owned()),
                    basis_allocation: BasisAllocation::Known {
                        share: ReturnedShare::new(dec("0.2")).unwrap(),
                        evidence: AllocationEvidence {
                            inputs_hash: AllocationInputsHash::new("a".repeat(64)).unwrap(),
                            knowledge_as_of: datetime!(2026-06-15 12:00:00 UTC),
                            algorithm_version: AllocationAlgorithmVersion(1),
                        },
                    },
                },
            },
            vec![Leg::principal(f.account, f.instrument, rub(i64::MAX))],
        ));

        // --- CorporateAction: Redemption --------------------------------------
        events.push(f.event(
            EventKind::CorporateAction {
                action: CorporateAction::Redemption {
                    instrument: f.instrument,
                    custody: f.custody,
                    quantity: qty("10"),
                    principal_returned_per_unit: per_unit("800"),
                    compensation: rub(i64::MIN),
                    effective_date: date!(2026 - 12 - 15),
                    record_date: None,
                    grounds: None,
                },
            },
            vec![
                Leg::principal(f.account, f.instrument, rub(i64::MIN)),
                Leg::security(f.account, f.custody, f.instrument, qty("-10")),
            ],
        ));

        // --- CorporateAction: Conversion, with and without compensation ------
        let successor = InstrumentId::new_random();
        events.push(f.event(
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: f.instrument,
                    successor,
                    custody: f.custody,
                    ratio: dec("1.5"),
                    quantity_in: qty("10"),
                    quantity_out: qty("15"),
                    fractional: FractionalTreatment::NotApplicable,
                    compensation: None,
                    effective_date: date!(2026 - 09 - 01),
                    record_date: Some(date!(2026 - 08 - 30)),
                    grounds: None,
                    basis_transfer: BasisTransferRule::CarryOver,
                },
            },
            vec![
                Leg::security(f.account, f.custody, f.instrument, qty("-10")),
                Leg::security(f.account, f.custody, successor, qty("15")),
            ],
        ));
        events.push(f.event(
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor: f.instrument,
                    successor,
                    custody: f.custody,
                    ratio: dec("1.55"),
                    quantity_in: qty("10"),
                    quantity_out: qty("15"),
                    fractional: FractionalTreatment::CashCompensated,
                    compensation: Some(rub(500)),
                    effective_date: date!(2026 - 09 - 01),
                    record_date: None,
                    grounds: Some("grounds".to_owned()),
                    basis_transfer: BasisTransferRule::Restart,
                },
            },
            vec![
                Leg::security(f.account, f.custody, f.instrument, qty("-10")),
                Leg::security(f.account, f.custody, successor, qty("15")),
                Leg::cash(f.account, rub(500)),
            ],
        ));

        // --- OfferExercise: every OfferExerciseAction variant -----------------
        events.push(f.event(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Submitted {
                    submission: OfferSubmissionId::new_random(),
                    window: OfferWindowId::new_random(),
                    instrument: f.instrument,
                    quantity: qty("10"),
                },
            },
            vec![],
        ));
        events.push(f.event(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Cancelled {
                    submission: OfferSubmissionId::new_random(),
                    quantity: qty("4"),
                },
            },
            vec![],
        ));
        events.push(f.event(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: f.instrument,
                    custody: f.custody,
                    quantity: qty("6"),
                    gross: rub(6_000_000),
                    fee: None,
                    accrued_interest: None,
                },
            },
            vec![
                Leg::cash(f.account, rub(6_000_000)),
                Leg::security(f.account, f.custody, f.instrument, qty("-6")),
            ],
        ));
        events.push(f.event(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: f.instrument,
                    custody: f.custody,
                    quantity: qty("6"),
                    gross: rub(6_000_000),
                    fee: Some(rub(-1_000)),
                    accrued_interest: Some(rub(12_345)),
                },
            },
            vec![
                Leg::cash(f.account, rub(6_011_345)),
                Leg::security(f.account, f.custody, f.instrument, qty("-6")),
            ],
        ));

        // --- Envelope variation: every date, relation, confidence and
        //     provenance Option, layered onto one event of each shape ------
        let mut fully_dated = f.event(
            EventKind::CashIn { amount: rub(1) },
            vec![Leg::cash(f.account, rub(1))],
        );
        fully_dated.dates = iaam_core::dates::EventDates {
            trade: Some(TradeDate(date!(2025 - 12 - 30))),
            settled: Some(SettledDate(date!(2026 - 01 - 03))),
            cash_posted: Some(CashPostedDate(date!(2026 - 01 - 03))),
            entitlement: Some(EntitlementDate(date!(2025 - 12 - 29))),
            paid: Some(PaidDate(date!(2026 - 01 - 12))),
            tax_period_override: Some(TaxPeriod(2024)),
        };
        fully_dated.order = iaam_core::dates::EffectiveOrder::with_source_time(
            date!(2026 - 01 - 03),
            time!(9:30:00.123456789),
            42,
        );
        fully_dated.relation = Relation::Reversal {
            target: EventId::new_random(),
        };
        fully_dated.confidence = Confidence::Estimated;
        fully_dated.idempotency_key = Some("client-key-1".to_owned());
        fully_dated.provenance = Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"c".repeat(64)).unwrap(),
            ParserVersion("fixture/2".to_owned()),
        )
        .with_source_operation_id("OP-9001")
        .with_source_category("groceries")
        .with_source_kind("card_payment")
        .with_owner_category("food")
        .with_source_code("5411")
        .with_description("Shop One")
        .with_import(ImportId::declared(
            fully_dated.owner,
            fully_dated.account,
            "file",
            "january",
        ))
        .with_import_session(ImportSessionId::new_random())
        .with_declared_by(PrincipalId::new_random())
        .with_rule_settlement(RuleSettlement::Rule {
            rule: ClassificationRuleId::new_random(),
            version: 3,
        })
        .with_row(RowLocator {
            document: RawHash::parse(&"d".repeat(64)).unwrap(),
            sheet: Some("Operations".to_owned()),
            // The largest row number this schema promises to round-trip:
            // `row_number` is a signed SQLite `INTEGER`, so `u64::MAX` itself
            // is rejected by `encode_row_number` rather than stored.
            row: i64::MAX as u64,
        });
        events.push(fully_dated);

        let mut replaced = f.event(
            EventKind::CashOut { amount: rub(-1) },
            vec![Leg::cash(f.account, rub(-1))],
        );
        replaced.relation = Relation::Replacement {
            target: EventId::new_random(),
        };
        replaced.provenance = replaced
            .provenance
            .with_rule_settlement(RuleSettlement::NoRule);
        events.push(replaced);

        let mut minted = f.event(
            EventKind::Refund { amount: rub(5) },
            vec![Leg::cash(f.account, rub(5))],
        );
        minted.provenance =
            minted
                .provenance
                .with_rule_settlement(RuleSettlement::AnsweredMintingRule {
                    rule: ClassificationRuleId::new_random(),
                    version: 1,
                });
        events.push(minted);

        let mut unknown = f.event(
            EventKind::OpeningCash { amount: rub(0) },
            vec![Leg::cash(f.account, rub(0))],
        );
        unknown.confidence = Confidence::Unknown;
        events.push(unknown);

        events
    }

    #[test]
    fn every_variant_round_trips_through_rows() {
        for event in every_event_shape() {
            assert_round_trip(&event);
        }
    }

    #[test]
    fn all_seventeen_discriminants_are_exercised() {
        let discriminants: BTreeSet<&'static str> = every_event_shape()
            .iter()
            .map(|event| event.kind.discriminant())
            .collect();
        for expected in EventKind::discriminants() {
            assert!(
                discriminants.contains(expected),
                "fixture set is missing an event of kind {expected}"
            );
        }
    }
}
