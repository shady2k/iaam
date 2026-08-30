//! Построение будущего денежного потока по графику облигации (§7.1).

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

/// Версия правила построения будущего потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CashflowProjectionVersion(pub u32);

/// Причина, по которой будущий поток нельзя построить целиком.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CashflowError {
    #[error("купон периода {period_start} не определён")]
    CouponUndetermined { period_start: Date },
    #[error("первоначальный номинал неизвестен")]
    PrincipalUnknown,
    #[error("график неполон: {reason}")]
    ScheduleIncomplete { reason: String },
    #[error("полнота графика неизвестна")]
    ScheduleCompletenessUnknown,
    #[error("доли возврата номинала дают {total:?}, а не 100")]
    SharesDoNotSumToWhole { total: Dec },
    #[error("эмитент объявил дефолт")]
    IssuerDefaultDeclared,
    #[error("эмитент объявил технический дефолт")]
    IssuerTechnicalDefault,
    #[error("условия выпуска неизвестны")]
    IssueTermsUnknown,
    #[error(transparent)]
    Numeric(#[from] NumericError),
    #[error("валютные роли не позволяют применить формулу: {roles:?}")]
    CurrencyFormulaUnknown { roles: Option<CurrencyRoles> },
    #[error("окно оферты {window:?} нельзя исполнить")]
    OfferWindowNotExercisable { window: crate::bond::OfferWindowId },
}
/// Причина, по которой нельзя доверять графику для сверки прошлых выплат.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScheduleTrustError {
    #[error("график неполон: {reason}")]
    ScheduleIncomplete { reason: String },
    #[error("полнота графика неизвестна")]
    ScheduleCompletenessUnknown,
    #[error("эмитент объявил дефолт")]
    IssuerDefaultDeclared,
    #[error("условия выпуска неизвестны")]
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

/// Входы правила построения потока.
pub struct CashflowInput<'a> {
    pub schedule: &'a BondSchedule,
    pub principal: crate::rules::lot_disposal::PrincipalState,
    pub quantity: Quantity,
    pub choice: &'a OfferChoice,
    pub as_of: Date,
    pub report_currency: CurrencyCode,
}

/// Один ожидаемый денежный платёж.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedPosting {
    pub date: Date,
    pub amount: CalcMoney,
    pub kind: PostingKind,
}

/// Вид ожидаемого платежа.
///
/// Порядок вариантов задаёт `Ord`, а он нужен `ScheduledPosting`: без него
/// сортировка `past` по одной дате оставляла бы выплаты одного дня в
/// произвольном порядке, а ядро обязано быть детерминированным.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PostingKind {
    Coupon,
    PrincipalReturn,
    OfferSettlement,
}

/// Запланированная выплата, срок которой уже наступил.
///
/// Вид обязателен: купон подтверждается `Income`, возврат номинала —
/// `CorporateAction`, расчёт по оферте — `OfferExercise`. Одна дата без
/// вида не позволяет отличить неполученный купон от неполученного
/// возврата номинала, а искать их надо в разных событиях журнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScheduledPosting {
    pub date: Date,
    pub kind: PostingKind,
    /// Дата, на которую определяется право на выплату.
    ///
    /// `None` — источник не сообщил дату фиксации. Судить по дате
    /// платежа в этом случае запрещено, и решение принимает правило
    /// сопоставления, а не построение плана.
    pub entitlement: Option<Date>,
}

/// Полный результат построения потока.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashflowPlan {
    pub postings: Vec<ExpectedPosting>,
    pub terminal_date: Date,
    /// Запланированные выплаты, срок которых не позже `as_of`. Правило
    /// не знает и не может знать, пришли ли деньги; сверку с журналом
    /// делает вызывающая сторона правилом сопоставления.
    pub past: Vec<ScheduledPosting>,
}

/// Правило построения будущего денежного потока.
pub trait CashflowProjection: Send + Sync + std::fmt::Debug {
    fn future_postings(&self, input: &CashflowInput) -> Result<CashflowPlan, CashflowError>;
}

/// Первая версия правила построения будущего потока.
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

/// Прошлые выплаты по графику: даты и виды, без денег.
///
/// Отдельно от `CashflowProjection`, потому что тот отказывается при
/// неизвестном номинале, а сверке номинал не нужен: она сравнивает плановые
/// даты с фактами журнала, а не суммы. Пока прошлое строилось сценарием,
/// неизвестный номинал молча выключал сверку целиком.
///
/// Оферты намеренно не входят: это право владельца, а не обязанность эмитента.
/// Их сверка идёт по `OfferBook` и состоянию заявки «предъявлена -> исполнена
/// -> расчёт получен», а не общим правилом по графику; это отдельный бид
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
                // Технический дефолт означает задержку, а не отмену выплаты:
                // просрочка по сроку при нём всё равно должна попасть в сверку.
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

        let original = match input.principal {
            crate::rules::lot_disposal::PrincipalState::Unknown => {
                return Err(CashflowError::PrincipalUnknown);
            }
            crate::rules::lot_disposal::PrincipalState::Known {
                original_per_unit, ..
            } => original_per_unit,
        };

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

        // Купон вне горизонта этого сценария не влияет на его результат.
        // Для удержания до погашения горизонтом служит последний возврат номинала.
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
        // Купон в дату исполнения оферты включается: `OfferSettlement`
        // содержит только процент от номинала, поэтому двойного счёта нет.
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

        // Возврат номинала в дату исполнения оферты не включается:
        // `OfferSettlement` заменяет погашение позиции по цене оферты,
        // поэтому отдельный возврат дал бы двойной денежный поток.
        // В отличие от него купон добавляется выше, поскольку
        // `OfferSettlement` его не содержит.
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

/// Вторая версия правила потока, передающая дату права в `past`.
#[derive(Debug, Default)]
pub struct CashflowProjectionV2;

impl CashflowProjection for CashflowProjectionV2 {
    fn future_postings(&self, input: &CashflowInput) -> Result<CashflowPlan, CashflowError> {
        // Будущие выплаты V2 не отличаются от V1: меняется только прошлое,
        // которое обогащается датой права. Копия логики V1 здесь означала бы
        // непроверяемого близнеца — мутационный прогон это и показал.
        let mut plan = CashflowProjectionV1.future_postings(input)?;

        // Оферта — право владельца, поэтому `historical_schedule_postings`
        // намеренно не возвращает её. Подменяем только купоны и возвраты
        // номинала, сохраняя расчёт по выбранному окну из плана V1.
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
    use crate::rules::lot_disposal::PrincipalState;
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

    fn known_principal(original: &str, remaining: &str) -> PrincipalState {
        PrincipalState::known(per_unit(original), per_unit(remaining)).expect("valid principal")
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
            ..BondSchedule::default()
        }
    }

    fn input<'a>(
        schedule: &'a BondSchedule,
        principal: PrincipalState,
        choice: &'a OfferChoice,
        as_of: time::Date,
    ) -> CashflowInput<'a> {
        CashflowInput {
            schedule,
            principal,
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
            .expect("история не должна зависеть от номинала");

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
            reason: "нет даты погашения".to_owned(),
        };

        assert!(matches!(
            historical_schedule_postings(&schedule, date!(2026 - 08 - 01)),
            Err(ScheduleTrustError::ScheduleIncomplete { reason })
                if reason == "нет даты погашения"
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
            .expect("технический дефолт задерживает, но не отменяет выплаты");

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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &hold_to_maturity,
                as_of,
            ))
            .expect("сценарий удержания не требует исполнения оферты");
        // Оферта — право владельца, поэтому без предъявления по ней нет
        // обещанного эмитентом платежа и нет основания требовать факт расчёта.
        let historical = historical_schedule_postings(&schedule, as_of)
            .expect("проверенный график даёт историю");

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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            ))
            .expect("выбранная оферта должна попасть в прошлое");
        let v2 = CashflowProjectionV2
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            ))
            .expect("выбранная оферта должна попасть в прошлое");

        // Подмена прошлого в V2 частичная: `historical_schedule_postings`
        // намеренно не знает об офертах, поэтому расчёт по выбранному окну
        // обязан сохраниться из плана V1.
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
            .expect("проверенный график даёт прошлые выплаты");
        let v2 = CashflowProjectionV2
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                as_of,
            ))
            .expect("известный номинал даёт полный план");

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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 09 - 15),
            ))
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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 09 - 15),
            ))
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
                principal: known_principal("100", "100"),
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
                principal: known_principal("100", "70"),
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
                principal: known_principal("100", "100"),
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
                    repayment_date: date!(2026 - 09 - 01),
                    share_percent: dec("50"),
                },
                PrincipalReturn {
                    repayment_date: date!(2026 - 10 - 01),
                    share_percent: dec("50"),
                },
            ],
        );
        let choice = OfferChoice::HoldToMaturity;
        let plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                principal: known_principal("100", "20"),
                quantity: quantity("1"),
                choice: &choice,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .expect("known principal is computable");

        assert_eq!(plan.postings[0].amount.value(), dec("50"));
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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            ))
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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 07 - 01),
            ))
            .expect("past coupon and past principal return are computable")
    }

    #[test]
    fn past_postings_carry_their_kind_so_reconciliation_can_match_them() {
        // Купон и возврат номинала подтверждаются РАЗНЫМИ событиями журнала:
        // купон приходит `Income`, амортизация — `CorporateAction`. Без вида
        // выплаты сверка искала бы купонный факт под возврат номинала и
        // поднимала ложную тревогу на каждой амортизируемой облигации.
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
                principal: known_principal("100", "100"),
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
        let principal = known_principal("100", "100");
        let hold = OfferChoice::HoldToMaturity;
        let offer = OfferChoice::ExerciseAtOffer { window };
        let hold_plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                principal,
                quantity: quantity("1"),
                choice: &hold,
                as_of: date!(2026 - 08 - 01),
                report_currency: CurrencyCode::Rub,
            })
            .unwrap();
        let offer_plan = CashflowProjectionV1
            .future_postings(&CashflowInput {
                schedule: &schedule,
                principal,
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
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::CouponUndetermined { period_start })
                if period_start == date!(2026 - 08 - 01)
        ));
    }

    #[test]
    fn rejects_unknown_principal() {
        let schedule = valid_schedule(vec![], vec![]);
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                PrincipalState::Unknown,
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::PrincipalUnknown)
        ));
    }

    #[test]
    fn rejects_incomplete_schedule_with_reason() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.completeness = ScheduleCompleteness::Incomplete {
            reason: "нет даты погашения".to_owned(),
        };
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::ScheduleIncomplete { reason }) if reason == "нет даты погашения"
        ));
    }

    #[test]
    fn rejects_unknown_schedule_completeness() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.completeness = ScheduleCompleteness::Unknown;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
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
                known_principal("100", "100"),
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
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::IssuerDefaultDeclared)
        ));
    }

    #[test]
    fn rejects_technical_default() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.default_flags.as_mut().unwrap().technical = true;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::IssuerTechnicalDefault)
        ));
    }

    #[test]
    fn rejects_unknown_issue_terms() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.default_flags = None;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::IssueTermsUnknown)
        ));
    }

    #[test]
    fn rejects_missing_currency_roles() {
        let mut schedule = valid_schedule(vec![], vec![]);
        schedule.currency_roles = None;
        let choice = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
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
                principal: known_principal("100", "100"),
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
                known_principal("100", "100"),
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
                known_principal("100", "100"),
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
                known_principal("100", "100"),
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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &offer,
                date!(2026 - 08 - 01),
            ))
            .expect("post-offer unknown coupons are outside the scenario horizon");
        assert_eq!(offer_plan.terminal_date, date!(2026 - 10 - 01));
        assert_eq!(offer_plan.postings.len(), 2);

        let hold = OfferChoice::HoldToMaturity;
        assert!(matches!(
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            ))
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
            .future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            ))
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
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::Numeric(_))
        ));

        schedule.principal_returns = vec![PrincipalReturn {
            repayment_date: date!(2026 - 09 - 01),
            share_percent: dec("100"),
        }];
        let original = PerUnitAmount::new(Dec::new(Decimal::MAX), CurrencyCode::Rub);
        assert!(matches!(
            CashflowProjectionV1.future_postings(&CashflowInput {
                schedule: &schedule,
                principal: PrincipalState::known(original, original).unwrap(),
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
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
            Err(CashflowError::CurrencyFormulaUnknown { .. })
        ));

        let original = PerUnitAmount::new(dec("100"), CurrencyCode::Usd);
        schedule.currency_roles = Some(CurrencyRoles::uniform(CurrencyCode::Rub));
        assert!(matches!(
            CashflowProjectionV1.future_postings(&CashflowInput {
                schedule: &schedule,
                principal: PrincipalState::known(original, original).unwrap(),
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
            CashflowProjectionV1.future_postings(&input(
                &schedule,
                known_principal("100", "100"),
                &choice,
                date!(2026 - 08 - 01),
            )),
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
                    known_principal("100", "100"),
                    &choice,
                    date!(2026 - 08 - 01),
                )),
                Err(CashflowError::OfferWindowNotExercisable { window: actual })
                    if actual == window
            ));
        }
    }
}
