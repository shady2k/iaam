//! Building future cash flow from the bond schedule (§7.1).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::bond::BondSchedule;
use crate::bond::offer::{OfferChoice, ScheduleCompleteness};
use crate::instrument::CurrencyRoles;
use crate::money::{CalcMoney, CurrencyCode, Quantity};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Version of the future cash flow construction rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CashflowProjectionVersion(pub u32);

/// Reason why the future cash flow cannot be built in full.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CashflowError {
    #[error("coupon for period {period_start} is undefined")]
    CouponUndetermined { period_start: Date },
    #[error("initial face value is unknown")]
    PrincipalUnknown,
    #[error("schedule is incomplete: {reason}")]
    ScheduleIncomplete { reason: String },
    #[error("schedule completeness is unknown")]
    ScheduleCompletenessUnknown,
    #[error("principal repayment shares total {total:?}, not 100")]
    SharesDoNotSumToWhole { total: Dec },
    #[error("issuer has declared default")]
    IssuerDefaultDeclared,
    #[error("issuer has declared technical default")]
    IssuerTechnicalDefault,
    #[error("issue terms are unknown")]
    IssueTermsUnknown,
    #[error(transparent)]
    Numeric(#[from] NumericError),
    #[error("currency roles do not allow the formula to be applied: {roles:?}")]
    CurrencyFormulaUnknown { roles: Option<CurrencyRoles> },
    #[error("offer window {window:?} cannot be exercised")]
    OfferWindowNotExercisable { window: crate::bond::OfferWindowId },
}
/// Reason why the schedule cannot be trusted for reconciling past payments.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScheduleTrustError {
    #[error("schedule is incomplete: {reason}")]
    ScheduleIncomplete { reason: String },
    #[error("schedule completeness is unknown")]
    ScheduleCompletenessUnknown,
    #[error("issuer has declared default")]
    IssuerDefaultDeclared,
    #[error("issue terms are unknown")]
    IssueTermsUnknown,
}

impl From<ScheduleTrustError> for CashflowError {
    fn from(error: ScheduleTrustError) -> Self {
        match error {
            ScheduleTrustError::ScheduleIncomplete { reason } => {
                Self::ScheduleIncomplete { reason }
            }
            ScheduleTrustError::ScheduleCompletenessUnknown => Self::ScheduleCompletenessUnknown,
            ScheduleTrustError::IssuerDefaultDeclared => Self::IssuerDefaultDeclared,
            ScheduleTrustError::IssueTermsUnknown => Self::IssueTermsUnknown,
        }
    }
}

/// Inputs to the cash flow construction rule.
pub struct CashflowInput<'a> {
    pub schedule: &'a BondSchedule,
    pub quantity: Quantity,
    pub choice: &'a OfferChoice,
    pub as_of: Date,
    pub report_currency: CurrencyCode,
}

/// One expected cash payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedPosting {
    pub date: Date,
    pub amount: CalcMoney,
    pub kind: PostingKind,
}

/// Kind of expected payment.
///
/// Variant order defines `Ord`, which `ScheduledPosting` needs: without it
/// sorting `past` by date alone would leave same-day payments in
/// arbitrary order, while the core must be deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PostingKind {
    Coupon,
    PrincipalReturn,
    OfferSettlement,
}

/// A scheduled payment that is already due.
///
/// The kind is required: a coupon is confirmed by `Income`, principal repayment —
/// by `CorporateAction`, offer settlement — by `OfferExercise`. A date without
/// a kind cannot distinguish a missing coupon from a missing
/// principal repayment, and they must be sought in different journal events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScheduledPosting {
    pub date: Date,
    pub kind: PostingKind,
    /// Date determining entitlement to the payment.
    ///
    /// `None` — the source did not report the entitlement date. Inferring it from the
    /// payment date is prohibited in this case, and the decision is made by the
    /// matching rule, not plan construction.
    pub entitlement: Option<Date>,
}

/// Complete cash flow construction result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashflowPlan {
    pub postings: Vec<ExpectedPosting>,
    pub terminal_date: Date,
    /// Scheduled payments due no later than `as_of`. The rule
    /// does not and cannot know whether the money arrived; reconciliation with the journal
    /// is performed by the caller using the matching rule.
    pub past: Vec<ScheduledPosting>,
}

/// Future cash flow construction rule.
pub trait CashflowProjection: Send + Sync + std::fmt::Debug {
    fn future_postings(&self, input: &CashflowInput) -> Result<CashflowPlan, CashflowError>;
}

/// First version of the future cash flow construction rule.
#[derive(Debug, Default)]
pub struct CashflowProjectionV1;

fn dec_hundred() -> Dec {
    Dec::new(Decimal::ONE_HUNDRED)
}

fn amount_for_per_unit(
    value: crate::money::PerUnitAmount,
    quantity: Quantity,
    factor: Dec,
) -> Result<CalcMoney, CashflowError> {
    let total = value.value().checked_mul(quantity.0)?.checked_mul(factor)?;
    Ok(CalcMoney::new(total, value.currency()))
}

/// Past scheduled payments: dates and kinds, without amounts.
///
/// Separate from `CashflowProjection`, because it refuses to proceed when
/// face value is unknown, while reconciliation does not need it: it compares scheduled
/// dates with journal facts, not amounts. When the past was built by the scenario,
/// an unknown face value silently disabled reconciliation entirely.
///
/// Offers are intentionally excluded: they are the owner's right, not the issuer's obligation.
/// They are reconciled through `OfferBook` and the application state «submitted -> executed
/// -> settlement received», rather than the general schedule rule; this is a separate bid
/// `iaam-d8b.19`.
pub fn historical_schedule_postings(
    schedule: &BondSchedule,
    as_of: Date,
) -> Result<Vec<ScheduledPosting>, ScheduleTrustError> {
    match &schedule.completeness {
        ScheduleCompleteness::Validated => {}
        ScheduleCompleteness::Incomplete { reason } => {
            return Err(ScheduleTrustError::ScheduleIncomplete {
                reason: reason.clone(),
            });
        }
        ScheduleCompleteness::Unknown => {
            return Err(ScheduleTrustError::ScheduleCompletenessUnknown);
        }
    }

    match schedule.default_flags {
        Some(flags) => match (flags.declared, flags.technical) {
            (true, _) => return Err(ScheduleTrustError::IssuerDefaultDeclared),
            (false, true) => {
                // Technical default means a delay, not cancellation of the payment:
                // an overdue payment must still be included in reconciliation.
            }
            (false, false) => {}
        },
        None => return Err(ScheduleTrustError::IssueTermsUnknown),
    }

    let mut past = Vec::new();
    for period in &schedule.periods {
        if period.payment_date <= as_of {
            past.push(ScheduledPosting {
                date: period.payment_date,
                kind: PostingKind::Coupon,
                entitlement: period.record_date,
            });
        }
    }
    for principal_return in &schedule.principal_returns {
        if principal_return.repayment_date <= as_of {
            past.push(ScheduledPosting {
                date: principal_return.repayment_date,
                kind: PostingKind::PrincipalReturn,
                entitlement: None,
            });
        }
    }
    past.sort_by_key(|posting| (posting.date, posting.kind));
    Ok(past)
}

impl CashflowProjection for CashflowProjectionV1 {
    fn future_postings(&self, input: &CashflowInput) -> Result<CashflowPlan, CashflowError> {
        match &input.schedule.completeness {
            ScheduleCompleteness::Validated => {}
            ScheduleCompleteness::Incomplete { reason } => {
                return Err(CashflowError::ScheduleIncomplete {
                    reason: reason.clone(),
                });
            }
            ScheduleCompleteness::Unknown => {
                return Err(CashflowError::ScheduleCompletenessUnknown);
            }
        }

        match input.schedule.default_flags {
            Some(flags) if flags.declared => return Err(CashflowError::IssuerDefaultDeclared),
            Some(flags) if flags.technical => return Err(CashflowError::IssuerTechnicalDefault),
            Some(_) => {}
            None => return Err(CashflowError::IssueTermsUnknown),
        }

        let roles = input.schedule.currency_roles;
        let Some(roles_value) = roles else {
            return Err(CashflowError::CurrencyFormulaUnknown { roles });
        };
        if roles_value.denomination != roles_value.settlement
            || roles_value.settlement != roles_value.quote
            || roles_value.quote != input.report_currency
        {
            return Err(CashflowError::CurrencyFormulaUnknown { roles });
        }

        let original = input
            .schedule
            .initial_principal
            .ok_or(CashflowError::PrincipalUnknown)?;

        if original.currency() != input.report_currency
            || input.schedule.periods.iter().any(|period| {
                period
                    .coupon_per_unit
                    .is_some_and(|coupon| coupon.currency() != input.report_currency)
            })
        {
            return Err(CashflowError::CurrencyFormulaUnknown { roles });
        }

        let total_shares = Dec::sum(
            &input
                .schedule
                .principal_returns
                .iter()
                .map(|item| item.share_percent)
                .collect::<Vec<_>>(),
        )?;
        if total_shares != dec_hundred() {
            return Err(CashflowError::SharesDoNotSumToWhole {
                total: total_shares,
            });
        }

        let offer = match input.choice {
            OfferChoice::HoldToMaturity => None,
            OfferChoice::ExerciseAtOffer { window } => {
                let terms = input
                    .schedule
                    .offer_windows
                    .iter()
                    .find(|terms| terms.window == *window);
                let Some(terms) = terms else {
                    return Err(CashflowError::OfferWindowNotExercisable { window: *window });
                };
                match terms.right {
                    crate::bond::OfferRight::HolderPut => {
                        if terms.price_percent.is_none() {
                            return Err(CashflowError::OfferWindowNotExercisable {
                                window: *window,
                            });
                        }
                    }
                    crate::bond::OfferRight::HolderPutSettled
                    | crate::bond::OfferRight::IssuerCall
                    | crate::bond::OfferRight::Other => {
                        return Err(CashflowError::OfferWindowNotExercisable { window: *window });
                    }
                }
                Some(terms)
            }
        };
        let cutoff = offer.map(|terms| terms.execution_date);
        let terminal_date = offer.map_or_else(
            || {
                input
                    .schedule
                    .principal_returns
                    .iter()
                    .map(|item| item.repayment_date)
                    .max()
                    .unwrap_or(input.as_of)
            },
            |terms| terms.execution_date,
        );

        // A coupon beyond the horizon of this scenario does not affect its result.
        // For holding to maturity, the horizon is the final principal repayment.
        for period in &input.schedule.periods {
            if period.payment_date > input.as_of
                && period.payment_date <= terminal_date
                && period.coupon_per_unit.is_none()
            {
                return Err(CashflowError::CouponUndetermined {
                    period_start: period.period_start,
                });
            }
        }

        let mut postings = Vec::new();
        let mut past = Vec::new();
        // A coupon on the offer exercise date is included: `OfferSettlement`
        // contains only a percentage of face value, so there is no double counting.
        for period in &input.schedule.periods {
            if period.payment_date <= input.as_of {
                past.push(ScheduledPosting {
                    date: period.payment_date,
                    kind: PostingKind::Coupon,
                    entitlement: None,
                });
            } else if cutoff.is_none_or(|date| period.payment_date <= date) {
                let Some(coupon) = period.coupon_per_unit else {
                    return Err(CashflowError::CouponUndetermined {
                        period_start: period.period_start,
                    });
                };
                postings.push(ExpectedPosting {
                    date: period.payment_date,
                    amount: amount_for_per_unit(coupon, input.quantity, Dec::one())?,
                    kind: PostingKind::Coupon,
                });
            }
        }

        // Principal repayment on the offer exercise date is excluded:
        // `OfferSettlement` replaces redemption of the position at the offer price,
        // so a separate repayment would create a duplicate cash flow.
        // Unlike it, the coupon is added above, since
        // `OfferSettlement` does not include it.
        for principal_return in &input.schedule.principal_returns {
            if principal_return.repayment_date <= input.as_of {
                past.push(ScheduledPosting {
                    date: principal_return.repayment_date,
                    kind: PostingKind::PrincipalReturn,
                    entitlement: None,
                });
            } else if cutoff.is_none_or(|date| principal_return.repayment_date < date) {
                postings.push(ExpectedPosting {
                    date: principal_return.repayment_date,
                    amount: amount_for_per_unit(
                        original,
                        input.quantity,
                        principal_return.share_percent.checked_div(dec_hundred())?,
                    )?,
                    kind: PostingKind::PrincipalReturn,
                });
            }
        }

        if let Some(terms) = offer {
            let Some(price_percent) = terms.price_percent else {
                return Err(CashflowError::OfferWindowNotExercisable {
                    window: terms.window,
                });
            };
            let settlement = amount_for_per_unit(
                original,
                input.quantity,
                price_percent.checked_div(dec_hundred())?,
            )?;
            if terms.execution_date <= input.as_of {
                past.push(ScheduledPosting {
                    date: terms.execution_date,
                    kind: PostingKind::OfferSettlement,
                    entitlement: None,
                });
            } else {
                postings.push(ExpectedPosting {
                    date: terms.execution_date,
                    amount: settlement,
                    kind: PostingKind::OfferSettlement,
                });
            }
        }

        postings.sort_by_key(|posting| posting.date);
        past.sort();
        Ok(CashflowPlan {
            postings,
            terminal_date,
            past,
        })
    }
}

/// Second version of the cash flow rule, passing the entitlement date into `past`.
#[derive(Debug, Default)]
pub struct CashflowProjectionV2;

impl CashflowProjection for CashflowProjectionV2 {
    fn future_postings(&self, input: &CashflowInput) -> Result<CashflowPlan, CashflowError> {
        // Future V2 payments do not differ from V1: only the past changes,
        // enriched with the entitlement date. Copying V1 logic here would create
        // an unverifiable twin — the mutation run showed exactly that.
        let mut plan = CashflowProjectionV1.future_postings(input)?;

        // An offer is the owner's right, so `historical_schedule_postings`
        // intentionally does not return it. We replace only coupons and principal
        // repayments, preserving settlement for the selected window from the V1 plan.
        let offer_settlements = plan
            .past
            .iter()
            .copied()
            .filter(|posting| posting.kind == PostingKind::OfferSettlement)
            .collect::<Vec<_>>();
        let mut past = historical_schedule_postings(input.schedule, input.as_of)?;
        past.extend(offer_settlements);
        past.sort_by_key(|posting| (posting.date, posting.kind));
        plan.past = past;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::{AccrualPeriod, BondSchedule, PrincipalReturn};
    use crate::instrument::CurrencyRoles;
    use crate::money::{CurrencyCode, PerUnitAmount, Quantity};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn dec(value: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(value).expect("valid decimal"))
    }

    fn per_unit(value: &str) -> PerUnitAmount {
        PerUnitAmount::new(dec(value), CurrencyCode::Rub)
    }

    fn quantity(value: &str) -> Quantity {
        Quantity(dec(value))
    }

    fn period(start: time::Date, payment: time::Date, coupon: Option<&str>) -> AccrualPeriod {
        AccrualPeriod {
            period_start: start,
            accrual_end: payment,
            payment_date: payment,
            record_date: None,
            coupon_per_unit: coupon.map(per_unit),
        }
    }

    fn valid_schedule(
        periods: Vec<AccrualPeriod>,
        principal_returns: Vec<PrincipalReturn>,
    ) -> BondSchedule {
        BondSchedule {
            periods,
            principal_returns,
            completeness: ScheduleCompleteness::Validated,
            default_flags: Some(crate::bond::DefaultFlags {
                declared: false,
                technical: false,
            }),
            currency_roles: Some(CurrencyRoles::uniform(CurrencyCode::Rub)),
            initial_principal: Some(per_unit("100")),
            ..BondSchedule::default()
        }
    }

    fn input<'a>(
        schedule: &'a BondSchedule,
        choice: &'a OfferChoice,
        as_of: time::Date,
    ) -> CashflowInput<'a> {
        CashflowInput {
            schedule,
            quantity: quantity("1"),
            choice,
            as_of,
            report_currency: CurrencyCode::Rub,
        }
    }

    #[test]
    fn historical_postings_are_available_without_principal() {
        let schedule = valid_schedule(
            vec![AccrualPeriod {
                record_date: Some(date!(2026 - 07 - 30)),
                ..period(date!(2026 - 07 - 01), date!(2026 - 08 - 01), None)
            }],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 08 - 01),
                share_percent: dec("100"),
            }],
        );

        let past = historical_schedule_postings(&schedule, date!(2026 - 08 - 01))
            .expect("history must not depend on face value");

        assert_eq!(
            past,
            vec![
                ScheduledPosting {
                    date: date!(2026 - 08 - 01),
                    kind: PostingKind::Coupon,
                    entitlement: Some(date!(2026 - 07 - 30)),
                },
                ScheduledPosting {
                    date: date!(2026 - 08 - 01),
                    kind: PostingKind::PrincipalReturn,
                    entitlement: None,
                },
            ]
        );
    }

    #[test]
    fn historical_postings_reject_incomplete_schedule_with_reason() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.completeness = ScheduleCompleteness::Incomplete {
            reason: "no maturity date".to_owned(),
        };

        assert!(matches!(
            historical_schedule_postings(&schedule, date!(2026 - 08 - 01)),
            Err(ScheduleTrustError::ScheduleIncomplete { reason })
                if reason == "no maturity date"
        ));
    }

    #[test]
    fn historical_postings_reject_unknown_schedule_completeness() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.completeness = ScheduleCompleteness::Unknown;

        assert!(matches!(
            historical_schedule_postings(&schedule, date!(2026 - 08 - 01)),
            Err(ScheduleTrustError::ScheduleCompletenessUnknown)
        ));
    }

    #[test]
    fn historical_postings_reject_declared_default() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.default_flags.as_mut().unwrap().declared = true;

        assert!(matches!(
            historical_schedule_postings(&schedule, date!(2026 - 08 - 01)),
            Err(ScheduleTrustError::IssuerDefaultDeclared)
        ));
    }

    #[test]
    fn historical_postings_include_payments_for_technical_default() {
        let mut schedule = valid_schedule(
            vec![period(
                date!(2026 - 07 - 01),
                date!(2026 - 08 - 01),
                Some("10"),
            )],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 08 - 15),
                share_percent: dec("100"),
            }],
        );
        schedule.default_flags.as_mut().unwrap().technical = true;

        let past = historical_schedule_postings(&schedule, date!(2026 - 08 - 15))
            .expect("technical default delays payments but does not cancel them");

        assert_eq!(past.len(), 2);
        assert_eq!(past[0].kind, PostingKind::Coupon);
        assert_eq!(past[1].kind, PostingKind::PrincipalReturn);
    }

    #[test]
    fn historical_postings_exclude_unexercised_offer() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 07 - 15),
        );
        let mut schedule = valid_schedule(
            vec![],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 12 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 07 - 15),
            submission_start: None,
            submission_end: None,
            price_percent: Some(dec("100")),
        });
        let hold_to_maturity = OfferChoice::HoldToMaturity;
        let as_of = date!(2026 - 08 - 01);

        let hold_plan = CashflowProjectionV1
            .future_postings(&input(&schedule, &hold_to_maturity, as_of))
            .expect("hold scenario does not require offer exercise");
        // An offer is the owner's right, so without exercising it there is no
        // issuer-promised payment and no basis to require a settlement record.
        let historical = historical_schedule_postings(&schedule, as_of)
            .expect("a validated schedule provides history");

        assert!(
            hold_plan
                .past
                .iter()
                .all(|posting| posting.kind != PostingKind::OfferSettlement)
        );
        assert!(
            historical
                .iter()
                .all(|posting| posting.kind != PostingKind::OfferSettlement)
        );
        assert!(historical.is_empty());
    }

    #[test]
    fn v2_keeps_selected_past_offer_settlement() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 07 - 15),
        );
        let mut schedule = valid_schedule(
            vec![],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 12 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 07 - 15),
            submission_start: None,
            submission_end: None,
            price_percent: Some(dec("100")),
        });
        let choice = OfferChoice::ExerciseAtOffer { window };

        let v1 = CashflowProjectionV1
            .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01)))
            .expect("the selected offer must appear in the past");
        let v2 = CashflowProjectionV2
            .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01)))
            .expect("the selected offer must appear in the past");

        // Replacement of the past in V2 is partial: `historical_schedule_postings`
        // intentionally knows nothing about offers, so settlement for the selected window
        // must be preserved from the V1 plan.
        assert_eq!(v2.past, v1.past);
        assert_eq!(
            v2.past,
            vec![ScheduledPosting {
                date: date!(2026 - 07 - 15),
                kind: PostingKind::OfferSettlement,
                entitlement: None,
            }]
        );
    }

    #[test]
    fn historical_postings_match_v2_past_for_known_principal() {
        let schedule = valid_schedule(
            vec![AccrualPeriod {
                record_date: Some(date!(2026 - 06 - 30)),
                ..period(date!(2026 - 06 - 01), date!(2026 - 07 - 01), Some("10"))
            }],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 07 - 10),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;
        let as_of = date!(2026 - 08 - 01);

        let historical = historical_schedule_postings(&schedule, as_of)
            .expect("a validated schedule provides past payments");
        let v2 = CashflowProjectionV2
            .future_postings(&input(&schedule, &choice, as_of))
            .expect("a known face value provides a complete plan");

        assert_eq!(historical, v2.past);
    }

    #[test]
    fn v2_carries_coupon_record_date_into_entitlement() {
        let schedule = valid_schedule(
            vec![AccrualPeriod {
                record_date: Some(date!(2026 - 08 - 30)),
                ..period(date!(2026 - 07 - 01), date!(2026 - 09 - 01), Some("10"))
            }],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 10 - 01),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;

        let plan = CashflowProjectionV2
            .future_postings(&input(&schedule, &choice, date!(2026 - 09 - 15)))
            .expect("past coupon is computable");

        assert_eq!(
            plan.past,
            vec![ScheduledPosting {
                date: date!(2026 - 09 - 01),
                kind: PostingKind::Coupon,
                entitlement: Some(date!(2026 - 08 - 30)),
            }]
        );
    }

    #[test]
    fn v2_keeps_coupon_entitlement_unknown_when_record_date_is_missing() {
        let schedule = valid_schedule(
            vec![period(
                date!(2026 - 07 - 01),
                date!(2026 - 09 - 01),
                Some("10"),
            )],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 10 - 01),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;

        let plan = CashflowProjectionV2
            .future_postings(&input(&schedule, &choice, date!(2026 - 09 - 15)))
            .expect("past coupon is computable");

        assert_eq!(plan.past[0].entitlement, None);
    }

    #[test]
    fn historical_postings_reject_unknown_issue_terms() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.default_flags = None;

        assert!(matches!(
            historical_schedule_postings(&schedule, date!(2026 - 08 - 01)),
            Err(ScheduleTrustError::IssueTermsUnknown)
        ));
    }

    #[test]
    fn coupon_fractional_minor_units_stays_exact() {
        let schedule = valid_schedule(
            vec![period(
                date!(2026 - 08 - 01),
                date!(2026 - 09 - 01),
                Some("3.333"),
            )],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 10 - 01),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;
        let plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("3"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .expect("fractional coupon is computable");

        assert_eq!(plan.postings[0].amount.value(), dec("9.999"));
    }

    #[test]
    fn uniform_ruble_schedule_produces_complete_flow_and_whole_shares() {
        let schedule = valid_schedule(
            vec![period(
                date!(2026 - 08 - 01),
                date!(2026 - 09 - 01),
                Some("10"),
            )],
            vec![
                PrincipalReturn {
                    repayment_date: date!(2026 - 10 - 01),
                    share_percent: dec("60"),
                },
                PrincipalReturn {
                    repayment_date: date!(2026 - 11 - 01),
                    share_percent: dec("40"),
                },
            ],
        );
        let choice = OfferChoice::HoldToMaturity;
        let plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("2"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .expect("uniform ruble schedule is computable");

        assert_eq!(plan.postings.len(), 3);
        assert_eq!(plan.postings[0].kind, PostingKind::Coupon);
        assert_eq!(plan.postings[0].amount.value(), dec("20"));
        assert_eq!(plan.postings[1].kind, PostingKind::PrincipalReturn);
        assert_eq!(plan.postings[1].amount.value(), dec("120"));
        assert_eq!(plan.postings[2].amount.value(), dec("80"));
        assert_eq!(plan.terminal_date, date!(2026 - 11 - 01));
        assert!(plan.past.is_empty());
        assert_eq!(
            Dec::sum(
                &schedule
                    .principal_returns
                    .iter()
                    .map(|r| r.share_percent)
                    .collect::<Vec<_>>()
            )
            .unwrap(),
            dec("100")
        );
    }

    #[test]
    fn principal_return_fractional_minor_units_stays_exact() {
        let schedule = valid_schedule(
            vec![],
            vec![
                PrincipalReturn {
                    repayment_date: date!(2026 - 09 - 01),
                    share_percent: dec("33.333"),
                },
                PrincipalReturn {
                    repayment_date: date!(2026 - 10 - 01),
                    share_percent: dec("66.667"),
                },
            ],
        );
        let choice = OfferChoice::HoldToMaturity;
        let plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("3"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .expect("fractional principal return is computable");

        assert_eq!(plan.postings[0].amount.value(), dec("99.999"));
        assert_eq!(plan.postings[1].amount.value(), dec("200.001"));
    }

    #[test]
    fn principal_return_uses_original_not_remaining_nominal() {
        let schedule = valid_schedule(
            vec![],
            vec![
                PrincipalReturn {
                    repayment_date: date!(2026 - 07 - 01),
                    share_percent: dec("80"),
                },
                PrincipalReturn {
                    repayment_date: date!(2026 - 09 - 01),
                    share_percent: dec("20"),
                },
            ],
        );
        let choice = OfferChoice::HoldToMaturity;
        let plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("1"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .expect("known principal is computable");

        assert_eq!(plan.postings[0].amount.value(), dec("20"));
    }

    #[test]
    fn past_scheduled_dates_are_listed_separately_from_future_postings() {
        let schedule = valid_schedule(
            vec![period(
                date!(2026 - 07 - 01),
                date!(2026 - 08 - 01),
                Some("10"),
            )],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 08 - 01),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;
        let plan = CashflowProjectionV1
            .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01)))
            .expect("past payments are listed separately from future postings");

        assert!(plan.postings.is_empty());
        assert_eq!(
            plan.past,
            vec![
                ScheduledPosting {
                    date: date!(2026 - 08 - 01),
                    kind: PostingKind::Coupon,
                    entitlement: None,
                },
                ScheduledPosting {
                    date: date!(2026 - 08 - 01),
                    kind: PostingKind::PrincipalReturn,
                    entitlement: None,
                },
            ]
        );
    }

    fn plan_with_past_coupon_and_past_principal_return() -> CashflowPlan {
        let schedule = valid_schedule(
            vec![period(
                date!(2026 - 01 - 15),
                date!(2026 - 03 - 15),
                Some("10"),
            )],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 06 - 15),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;
        CashflowProjectionV1
            .future_postings(&input(&schedule, &choice, date!(2026 - 07 - 01)))
            .expect("past coupon and past principal return are computable")
    }

    #[test]
    fn past_postings_carry_their_kind_so_reconciliation_can_match_them() {
        // Coupons and principal repayments are confirmed by DIFFERENT journal events:
        // a coupon arrives as `Income`, amortization as `CorporateAction`. Without the payment
        // kind, reconciliation would match a coupon event to a principal repayment and
        // raise a false alarm for every amortizing bond.
        let plan = plan_with_past_coupon_and_past_principal_return();

        assert_eq!(
            plan.past,
            vec![
                ScheduledPosting {
                    date: date!(2026 - 03 - 15),
                    kind: PostingKind::Coupon,
                    entitlement: None,
                },
                ScheduledPosting {
                    date: date!(2026 - 06 - 15),
                    kind: PostingKind::PrincipalReturn,
                    entitlement: None,
                },
            ]
        );
    }

    #[test]
    fn offer_exercise_cuts_postings_and_sets_terminal_date() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 10 - 01),
        );
        let mut schedule = valid_schedule(
            vec![
                period(date!(2026 - 08 - 01), date!(2026 - 09 - 01), Some("10")),
                period(date!(2026 - 10 - 01), date!(2026 - 11 - 01), Some("20")),
            ],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 12 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 10 - 01),
            submission_start: None,
            submission_end: None,
            price_percent: Some(dec("110")),
        });
        let choice = OfferChoice::ExerciseAtOffer { window };
        let plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("2"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .expect("exercisable offer is computable");

        assert_eq!(plan.terminal_date, date!(2026 - 10 - 01));
        assert_eq!(plan.postings.len(), 2);
        assert_eq!(plan.postings[0].kind, PostingKind::Coupon);
        assert_eq!(plan.postings[1].kind, PostingKind::OfferSettlement);
        assert_eq!(plan.postings[1].amount.value(), dec("220"));
    }

    #[test]
    fn hold_to_maturity_and_offer_have_different_terminal_dates() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 10 - 01),
        );
        let mut schedule = valid_schedule(
            vec![],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 12 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 10 - 01),
            submission_start: None,
            submission_end: None,
            price_percent: Some(dec("100")),
        });
        let hold = OfferChoice::HoldToMaturity;
        let offer = OfferChoice::ExerciseAtOffer { window };
        let hold_plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("1"),
                choice: &hold,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .unwrap();
        let offer_plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("1"),
                choice: &offer,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .unwrap();

        assert_eq!(hold_plan.terminal_date, date!(2026 - 12 - 01));
        assert_eq!(offer_plan.terminal_date, date!(2026 - 10 - 01));
    }

    #[test]
    fn rejects_undetermined_future_coupon() {
        let schedule = valid_schedule(
            vec![period(date!(2026 - 08 - 01), date!(2026 - 09 - 01), None)],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 10 - 01),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::CouponUndetermined { period_start })
                if period_start == date!(2026 - 08 - 01)
        ));
    }

    #[test]
    fn a_schedule_without_a_face_value_cannot_build_a_flow() {
        // Face value is unknown — no cash flow is built. Substituting zero
        // is forbidden: «zero face value» and «unknown face value» require
        // different actions from the owner (§4.9).
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.initial_principal = None;
        let choice = OfferChoice::HoldToMaturity;
        assert_eq!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 01 - 01),))
                .unwrap_err(),
            CashflowError::PrincipalUnknown
        );
    }

    #[test]
    fn rejects_incomplete_schedule_with_reason() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.completeness = ScheduleCompleteness::Incomplete {
            reason: "no maturity date".to_owned(),
        };
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::ScheduleIncomplete { reason }) if reason == "no maturity date"
        ));
    }

    #[test]
    fn rejects_unknown_schedule_completeness() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.completeness = ScheduleCompleteness::Unknown;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::ScheduleCompletenessUnknown)
        ));
    }

    #[test]
    fn rejects_principal_shares_that_do_not_sum_to_whole() {
        let schedule = valid_schedule(
            vec![],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 09 - 01),
                share_percent: dec("99"),
            }],
        );
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::SharesDoNotSumToWhole { total }) if total == dec("99")
        ));
    }

    #[test]
    fn rejects_declared_default() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.default_flags.as_mut().unwrap().declared = true;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::IssuerDefaultDeclared)
        ));
    }

    #[test]
    fn rejects_technical_default() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.default_flags.as_mut().unwrap().technical = true;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::IssuerTechnicalDefault)
        ));
    }

    #[test]
    fn rejects_unknown_issue_terms() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.default_flags = None;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::IssueTermsUnknown)
        ));
    }

    #[test]
    fn rejects_missing_currency_roles() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.currency_roles = None;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::CurrencyFormulaUnknown { roles: None })
        ));
    }

    #[test]
    fn rejects_currency_roles_that_disagree_with_report_currency() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.currency_roles = Some(CurrencyRoles {
            denomination: CurrencyCode::Usd,
            settlement: CurrencyCode::Usd,
            quote: CurrencyCode::Usd,
        });
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("1"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            }),
            Err(CashflowError::CurrencyFormulaUnknown { roles: Some(roles) })
                if roles.denomination == CurrencyCode::Usd
        ));
    }

    #[test]
    fn rejects_a_single_currency_role_mismatch() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.currency_roles = Some(CurrencyRoles {
            denomination: CurrencyCode::Usd,
            settlement: CurrencyCode::Rub,
            quote: CurrencyCode::Rub,
        });
        let choice = OfferChoice::HoldToMaturity;

        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::CurrencyFormulaUnknown { roles: Some(roles) })
                if roles.denomination == CurrencyCode::Usd
                    && roles.settlement == CurrencyCode::Rub
                    && roles.quote == CurrencyCode::Rub
        ));
    }

    #[test]
    fn rejects_offer_without_known_settlement_terms() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 10 - 01),
        );
        let mut schedule = valid_schedule(
            vec![],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 12 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 10 - 01),
            submission_start: None,
            submission_end: None,
            price_percent: None,
        });
        let choice = OfferChoice::ExerciseAtOffer { window };
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::OfferWindowNotExercisable { window: actual }) if actual == window
        ));
    }

    #[test]
    fn rejects_offer_window_absent_from_schedule() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 10 - 01),
        );
        let schedule = valid_schedule(
            vec![],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 12 - 01),
                share_percent: dec("100"),
            }],
        );
        let choice = OfferChoice::ExerciseAtOffer { window };
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::OfferWindowNotExercisable { window: actual }) if actual == window
        ));
    }

    #[test]
    fn exercise_ignores_undetermined_coupons_after_its_terminal_date() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 10 - 01),
        );
        let mut schedule = valid_schedule(
            vec![
                period(date!(2026 - 08 - 01), date!(2026 - 09 - 01), Some("10")),
                period(date!(2040 - 01 - 01), date!(2040 - 07 - 01), None),
            ],
            vec![PrincipalReturn {
                repayment_date: date!(2050 - 01 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 10 - 01),
            submission_start: None,
            submission_end: None,
            price_percent: Some(dec("100")),
        });
        let offer = OfferChoice::ExerciseAtOffer { window };
        let offer_plan = CashflowProjectionV1
            .future_postings(&input(&schedule, &offer, date!(2026 - 08 - 01)))
            .expect("post-offer unknown coupons are outside the scenario horizon");
        assert_eq!(offer_plan.terminal_date, date!(2026 - 10 - 01));
        assert_eq!(offer_plan.postings.len(), 2);

        let hold = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                &hold,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::CouponUndetermined { period_start })
                if period_start == date!(2040 - 01 - 01)
        ));
    }

    #[test]
    fn coupon_on_offer_execution_date_is_kept_before_settlement() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 10 - 01),
        );
        let mut schedule = valid_schedule(
            vec![period(
                date!(2026 - 09 - 01),
                date!(2026 - 10 - 01),
                Some("10"),
            )],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 12 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 10 - 01),
            submission_start: None,
            submission_end: None,
            price_percent: Some(dec("100")),
        });
        let choice = OfferChoice::ExerciseAtOffer { window };
        let plan = CashflowProjectionV1
            .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01)))
            .expect("coupon and settlement are both due on execution date");

        assert_eq!(plan.postings.len(), 2);
        assert_eq!(plan.postings[0].kind, PostingKind::Coupon);
        assert_eq!(plan.postings[0].amount.value(), dec("10"));
        assert_eq!(plan.postings[1].kind, PostingKind::OfferSettlement);
        assert_eq!(plan.postings[1].amount.value(), dec("100"));
    }

    #[test]
    fn principal_return_on_offer_execution_date_is_replaced_by_settlement() {
        let window = crate::bond::OfferWindowId::derive(
            crate::ids::InstrumentId::new_random(),
            date!(2026 - 10 - 01),
        );
        let mut schedule = valid_schedule(
            vec![period(
                date!(2026 - 09 - 01),
                date!(2026 - 10 - 01),
                Some("10"),
            )],
            vec![PrincipalReturn {
                repayment_date: date!(2026 - 10 - 01),
                share_percent: dec("100"),
            }],
        );
        schedule.offer_windows.push(crate::bond::OfferWindowTerms {
            window,
            right: crate::bond::OfferRight::HolderPut,
            execution_date: date!(2026 - 10 - 01),
            submission_start: None,
            submission_end: None,
            price_percent: Some(dec("100")),
        });
        let choice = OfferChoice::ExerciseAtOffer { window };
        let plan = CashflowProjectionV1
            .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01)))
            .expect("principal return on execution date is replaced by settlement");

        assert_eq!(plan.postings.len(), 2);
        assert_eq!(plan.postings[0].kind, PostingKind::Coupon);
        assert_eq!(plan.postings[0].amount.value(), dec("10"));
        assert_eq!(plan.postings[1].kind, PostingKind::OfferSettlement);
        assert_eq!(plan.postings[1].amount.value(), dec("100"));
    }
    #[test]
    fn rejects_overflow_in_share_sum_and_posting_amounts() {
        let mut schedule = valid_schedule(
            vec![],
            vec![
                PrincipalReturn {
                    repayment_date: date!(2026 - 09 - 01),
                    share_percent: Dec::new(Decimal::MAX),
                },
                PrincipalReturn {
                    repayment_date: date!(2026 - 10 - 01),
                    share_percent: Dec::new(Decimal::MAX),
                },
            ],
        );
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::Numeric(_))
        ));

        schedule.principal_returns = vec![PrincipalReturn {
            repayment_date: date!(2026 - 09 - 01),
            share_percent: dec("100"),
        }];
        let original = PerUnitAmount::new(Dec::new(Decimal::MAX), CurrencyCode::Rub);
        schedule.initial_principal = Some(original);
        assert!(matches!(
            CashflowProjectionV1.future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("2"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            }),
            Err(CashflowError::Numeric(_))
        ));
    }

    #[test]
    fn rejects_currency_mismatch_in_roles_principal_and_coupon() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.currency_roles = Some(CurrencyRoles {
            denomination: CurrencyCode::Rub,
            settlement: CurrencyCode::Usd,
            quote: CurrencyCode::Usd,
        });
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::CurrencyFormulaUnknown { .. })
        ));

        let original = PerUnitAmount::new(dec("100"), CurrencyCode::Usd);
        schedule.initial_principal = Some(original);
        schedule.currency_roles = Some(CurrencyRoles::uniform(CurrencyCode::Rub));
        assert!(matches!(
            CashflowProjectionV1.future_postings(&CashflowInput {
                schedule: &schedule,
                quantity: quantity("1"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            }),
            Err(CashflowError::CurrencyFormulaUnknown { .. })
        ));

        schedule.periods = vec![AccrualPeriod {
            period_start: date!(2026 - 08 - 01),
            accrual_end: date!(2026 - 09 - 01),
            payment_date: date!(2026 - 09 - 01),
            record_date: None,
            coupon_per_unit: Some(PerUnitAmount::new(dec("1"), CurrencyCode::Usd)),
        }];
        assert!(matches!(
            CashflowProjectionV1
                .future_postings(&input(&schedule, &choice, date!(2026 - 08 - 01),)),
            Err(CashflowError::CurrencyFormulaUnknown { .. })
        ));
    }

    #[test]
    fn rejects_each_non_holder_offer_right() {
        for right in [
            crate::bond::OfferRight::HolderPutSettled,
            crate::bond::OfferRight::IssuerCall,
            crate::bond::OfferRight::Other,
        ] {
            let window = crate::bond::OfferWindowId::derive(
                crate::ids::InstrumentId::new_random(),
                date!(2026 - 10 - 01),
            );
            let mut schedule = valid_schedule(
                vec![],
                vec![PrincipalReturn {
                    repayment_date: date!(2026 - 12 - 01),
                    share_percent: dec("100"),
                }],
            );
            schedule.offer_windows.push(crate::bond::OfferWindowTerms {
                window,
                right,
                execution_date: date!(2026 - 10 - 01),
                submission_start: None,
                submission_end: None,
                price_percent: Some(dec("100")),
            });
            let choice = OfferChoice::ExerciseAtOffer { window };
            assert!(matches!(
                CashflowProjectionV1.future_postings(&input(
                    &schedule,
                    &choice,
                    date!(2026 - 08 - 01),
                )),
                Err(CashflowError::OfferWindowNotExercisable { window: actual })
                    if actual == window
            ));
        }
    }
}
