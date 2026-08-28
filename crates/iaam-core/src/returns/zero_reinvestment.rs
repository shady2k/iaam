//! Метрики облигационного сценария без реинвестирования (§7.1).
//!
//! Все денежные величины остаются в расчётном режиме [`CalcMoney`].
//! Промежуточные выплаты сохраняются до конца горизонта под нулевой ставкой;
//! ряд до налога, пока E5 не добавит налоговую политику.

use crate::bond::offer::OfferChoice;
use crate::dates::TradeDate;
use crate::money::{CalcMoney, CurrencyCode, Money, Quantity};
use crate::numeric::approx::SolverPolicy;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverFlow, solve};
use crate::projection::lots::{Cohort, InstrumentLots};
use crate::rules::lot_disposal::PrincipalState;
use crate::rules::quotation::{QuotationError, QuotationRule, QuotationV1};
use crate::rules::{CashflowPlan, ExpectedPosting, PostingKind};
use crate::valuation::QuotationBasis;
use time::Date;

use super::{Computed, NotComputable};

/// Пять величин §7.1 для одного сценария.
#[derive(Debug, Clone, PartialEq)]
pub struct ZeroReinvestmentMetrics {
    pub postings: Vec<ExpectedPosting>,
    pub terminal_wealth: CalcMoney,
    pub surplus: CalcMoney,
    pub hpr: Computed<Dec>,
    pub cagr_0r: Computed<RateOutcome>,
    pub zero_reinvestment_assumed: bool,
    pub pre_tax: bool,
}

/// Подпись ставки на соответствующей проспективной координате.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrrLabel {
    YieldToMaturity,
    YieldToOffer,
}

/// Проспективная координата: удержание от `as_of`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProspectiveMetric {
    pub as_of: Date,
    pub terminal_date: Date,
    pub c0: Computed<CalcMoney>,
    pub metrics: Computed<ZeroReinvestmentMetrics>,
    pub irr: Computed<RateOutcome>,
    pub irr_label: IrrLabel,
}

/// Пожизненная метрика одной когорты.
#[derive(Debug, Clone, PartialEq)]
pub struct LifetimeCohortMetric {
    pub acquired: TradeDate,
    pub quantity: Quantity,
    pub terminal_date: Date,
    pub c0: Computed<CalcMoney>,
    pub metrics: Computed<ZeroReinvestmentMetrics>,
    pub irr_absent_because: &'static str,
}

/// Результат по одному идентификатору сценария.
#[derive(Debug, Clone, PartialEq)]
pub struct BondScenarioResult {
    pub choice: OfferChoice,
    pub prospective: ProspectiveMetric,
    pub lifetime: Computed<Vec<LifetimeCohortMetric>>,
}

/// Версия политики расходов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExpensePolicyVersion(pub u32);

/// Статус знания расходов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpenseTreatment {
    Known { amount: Money, on: Date },
    AbsentByPolicy,
    UnknownBoundedBy { upper: Money },
    Unknown,
}

/// Результат, который сохраняет диапазон при неизвестном, но ограниченном
/// расходе. Нельзя заменять этот тип точным числом с флагом рядом (§4.9).
#[derive(Debug, Clone, PartialEq)]
pub enum ExpenseMetrics {
    Exact(ZeroReinvestmentMetrics),
    Bounded {
        without_expense: ZeroReinvestmentMetrics,
        with_upper_bound: ZeroReinvestmentMetrics,
    },
    NotComputable { reason: NotComputable },
}

/// Рассчитать пять величин для будущего потока.
#[must_use]
pub fn zero_reinvestment_metrics(
    postings: Vec<ExpectedPosting>,
    c0: CalcMoney,
    coordinate: Date,
    terminal_date: Date,
) -> Computed<ZeroReinvestmentMetrics> {
    zero_reinvestment_metrics_with_policy(
        postings,
        c0,
        coordinate,
        terminal_date,
        SolverPolicy::returns_default(),
    )
}

/// Рассчитать пять величин с явно указанной политикой решателя CAGR.
#[must_use]
pub fn zero_reinvestment_metrics_with_policy(
    postings: Vec<ExpectedPosting>,
    c0: CalcMoney,
    coordinate: Date,
    terminal_date: Date,
    solver_policy: SolverPolicy,
) -> Computed<ZeroReinvestmentMetrics> {
    let mut terminal_wealth = CalcMoney::new(Dec::zero(), c0.currency());
    for posting in &postings {
        if posting.amount.currency() != c0.currency() {
            return Computed::NotComputable {
                reason: NotComputable::CurrencyMismatch {
                    expected: c0.currency(),
                    actual: posting.amount.currency(),
                },
            };
        }
        terminal_wealth = match terminal_wealth.checked_add(posting.amount) {
            Ok(value) => value,
            Err(_) => return numeric_metrics_refusal("terminal_wealth_sum"),
        };
    }
    metrics_from_terminal_wealth(
        postings,
        c0,
        coordinate,
        terminal_date,
        terminal_wealth,
        solver_policy,
    )
}

fn metrics_from_terminal_wealth(
    postings: Vec<ExpectedPosting>,
    c0: CalcMoney,
    coordinate: Date,
    terminal_date: Date,
    terminal_wealth: CalcMoney,
    solver_policy: SolverPolicy,
) -> Computed<ZeroReinvestmentMetrics> {
    let surplus = match terminal_wealth.checked_sub(c0) {
        Ok(value) => value,
        Err(_) => return numeric_metrics_refusal("surplus_sub"),
    };
    let hpr = if c0.value() > Dec::zero() {
        match terminal_wealth
            .value()
            .checked_div(c0.value())
            .and_then(|value| value.checked_sub(Dec::one()))
        {
            Ok(value) => Computed::Value(value),
            Err(_) => numeric_value_refusal("hpr_div"),
        }
    } else {
        Computed::NotComputable {
            reason: NotComputable::NonPositiveInitialCapital,
        }
    };
    let cagr_0r = if c0.value() <= Dec::zero() {
        rate_refusal(NotComputable::NonPositiveInitialCapital)
    } else if terminal_wealth.value().is_negative() {
        rate_refusal(NotComputable::NegativeTerminalWealth)
    } else {
        let days = (terminal_date - coordinate).whole_days();
        if days <= 0 {
            rate_refusal(NotComputable::NonPositiveDuration {
                coordinate,
                terminal_date,
            })
        } else if terminal_wealth.value().is_zero() {
            Computed::Value(RateOutcome::exact(-1.0, solver_policy, DayCount::Act365))
        } else {
            let negative_c0 = match c0.value().checked_neg() {
                Ok(value) => value,
                Err(_) => return numeric_metrics_refusal("c0_negate"),
            };
            match solve(
                &[
                    SolverFlow { day_offset: 0, amount: negative_c0 },
                    SolverFlow { day_offset: days, amount: terminal_wealth.value() },
                ],
                solver_policy,
                DayCount::Act365,
            ) {
                Ok(value) => Computed::Value(value),
                Err(refusal) => rate_refusal(NotComputable::SolverRefused { refusal }),
            }
        }
    };
    Computed::Value(ZeroReinvestmentMetrics {
        postings,
        terminal_wealth,
        surplus,
        hpr,
        cagr_0r,
        zero_reinvestment_assumed: true,
        pre_tax: true,
    })
}

fn numeric_metrics_refusal(code: &'static str) -> Computed<ZeroReinvestmentMetrics> {
    Computed::NotComputable {
        reason: NotComputable::Numeric { code },
    }
}

fn numeric_value_refusal<T>(code: &'static str) -> Computed<T> {
    Computed::NotComputable {
        reason: NotComputable::Numeric { code },
    }
}

fn rate_refusal(reason: NotComputable) -> Computed<RateOutcome> {
    Computed::NotComputable { reason }
}

/// Получить текущую грязную стоимость позиции через правило котировки.
///
/// `accrued_interest` должен быть полной суммой НКД позиции. Чистота цены
/// является предусловием входа: [`QuotationBasis`] различает деньги и процент
/// номинала, но не clean/dirty; безусловно прибавлять НКД к dirty-котировке
/// запрещено, поскольку это даст двойной счёт.
#[must_use]
pub fn prospective_c0(
    quantity: Quantity,
    basis: QuotationBasis,
    price: Dec,
    venue_currency: CurrencyCode,
    remaining_face: Option<crate::money::PerUnitAmount>,
    accrued_interest: CalcMoney,
) -> Computed<CalcMoney> {
    let (money_per_unit, currency) = match QuotationV1.money_per_unit(
        basis,
        price,
        venue_currency,
        remaining_face,
    ) {
        Ok(value) => value,
        Err(error) => return Computed::NotComputable { reason: quote_error(error) },
    };
    if currency != accrued_interest.currency() {
        return Computed::NotComputable {
            reason: NotComputable::CurrencyMismatch {
                expected: currency,
                actual: accrued_interest.currency(),
            },
        };
    }
    let position = match money_per_unit.checked_mul(quantity.0) {
        Ok(value) => CalcMoney::new(value, currency),
        Err(_) => return numeric_value_refusal("prospective_position_mul"),
    };
    match position.checked_add(accrued_interest) {
        Ok(value) => Computed::Value(value),
        Err(_) => numeric_value_refusal("prospective_c0_add"),
    }
}

fn quote_error(error: QuotationError) -> NotComputable {
    match error {
        QuotationError::BasisUnknown => NotComputable::Numeric { code: "quotation_basis_unknown" },
        QuotationError::PrincipalUnknown => NotComputable::PrincipalUnknown,
        QuotationError::Numeric(_) => NotComputable::Numeric { code: "quotation_numeric" },
    }
}

/// Построить проспективную координату и YTM/доходность к оферте.
#[must_use]
pub fn prospective_metric(
    as_of: Date,
    plan: &CashflowPlan,
    c0: Computed<CalcMoney>,
    choice: &OfferChoice,
) -> ProspectiveMetric {
    let irr_label = match choice {
        OfferChoice::HoldToMaturity => IrrLabel::YieldToMaturity,
        OfferChoice::ExerciseAtOffer { .. } => IrrLabel::YieldToOffer,
    };
    let metrics = match &c0 {
        Computed::Value(value) => zero_reinvestment_metrics(
            plan.postings.clone(),
            *value,
            as_of,
            plan.terminal_date,
        ),
        Computed::NotComputable { reason } => Computed::NotComputable { reason: reason.clone() },
    };
    let irr = match &c0 {
        Computed::Value(value) => irr_for_postings(&plan.postings, *value, as_of),
        Computed::NotComputable { reason } => Computed::NotComputable { reason: reason.clone() },
    };
    ProspectiveMetric {
        as_of,
        terminal_date: plan.terminal_date,
        c0,
        metrics,
        irr,
        irr_label,
    }
}

fn irr_for_postings(
    postings: &[ExpectedPosting],
    c0: CalcMoney,
    coordinate: Date,
) -> Computed<RateOutcome> {
    let negative_c0 = match c0.value().checked_neg() {
        Ok(value) => value,
        Err(_) => return rate_refusal(NotComputable::Numeric { code: "irr_c0_negate" }),
    };
    let mut flows = Vec::with_capacity(postings.len() + 1);
    flows.push(SolverFlow { day_offset: 0, amount: negative_c0 });
    for posting in postings {
        if posting.amount.currency() != c0.currency() {
            return rate_refusal(NotComputable::CurrencyMismatch {
                expected: c0.currency(),
                actual: posting.amount.currency(),
            });
        }
        flows.push(SolverFlow {
            day_offset: (posting.date - coordinate).whole_days(),
            amount: posting.amount.value(),
        });
    }
    match solve(&flows, SolverPolicy::returns_default(), DayCount::Act365) {
        Ok(value) => Computed::Value(value),
        Err(refusal) => rate_refusal(NotComputable::SolverRefused { refusal }),
    }
}

/// Рассчитать одну пожизненную метрику. Прошлые выплаты без дат входят в
/// терминальное богатство, но не в `postings`: исторический IRR отсутствует.
#[must_use]
pub fn lifetime_cohort_metric(
    cohort: Cohort,
    future_postings: Vec<ExpectedPosting>,
    terminal_date: Date,
) -> LifetimeCohortMetric {
    let c0 = match (cohort.acquisition_basis, cohort.accrued_interest_paid) {
        (None, _) => Computed::NotComputable { reason: NotComputable::AcquisitionBasisUnknown },
        (_, None) => Computed::NotComputable {
            reason: NotComputable::AccruedInterestAtAcquisitionUnknown,
        },
        (Some(acquisition), Some(accrued)) => match acquisition.try_add(accrued) {
            Ok(value) => Computed::Value(CalcMoney::new(value.to_calc_dec(), value.currency())),
            Err(_) => numeric_value_refusal("acquisition_c0_add"),
        },
    };
    let metrics = match (&c0, cohort.received_to_date) {
        (Computed::NotComputable { reason }, _) => Computed::NotComputable { reason: reason.clone() },
        (Computed::Value(_), None) => Computed::NotComputable {
            reason: NotComputable::HistoricalReceiptsUnknown,
        },
        (Computed::Value(c0_value), Some(received)) => {
            if received.currency() != c0_value.currency() {
                Computed::NotComputable {
                    reason: NotComputable::CurrencyMismatch {
                        expected: c0_value.currency(),
                        actual: received.currency(),
                    },
                }
            } else {
                let mut wealth = CalcMoney::new(received.to_calc_dec(), received.currency());
                let mut failure = None;
                for posting in &future_postings {
                    if posting.amount.currency() != wealth.currency() {
                        failure = Some(NotComputable::CurrencyMismatch {
                            expected: c0_value.currency(),
                            actual: posting.amount.currency(),
                        });
                        break;
                    }
                    wealth = match wealth.checked_add(posting.amount) {
                        Ok(value) => value,
                        Err(_) => {
                            failure =
                                Some(NotComputable::Numeric { code: "lifetime_wealth_add" });
                            break;
                        }
                    };
                }
                match failure {
                    Some(reason) => Computed::NotComputable { reason },
                    None => metrics_from_terminal_wealth(
                        future_postings,
                        *c0_value,
                        cohort.acquired.inner(),
                        terminal_date,
                        wealth,
                        SolverPolicy::returns_default(),
                    ),
                }
            }
        }
    };
    LifetimeCohortMetric {
        acquired: cohort.acquired,
        quantity: cohort.quantity,
        terminal_date,
        c0,
        metrics,
        irr_absent_because: "прошлые выплаты хранятся одной суммой без дат, поэтому ряд потоков для пожизненного IRR нельзя восстановить; YTM рассчитывается только для проспективного знаменателя",
    }
}

/// Рассчитать пожизненные метрики всех когорт, распределяя будущие выплаты
/// от текущего количества. Последняя когорта получает остаток каждой суммы.
#[must_use]
pub fn lifetime_cohort_metrics(
    cohorts: &[Cohort],
    plan: &CashflowPlan,
) -> Computed<Vec<LifetimeCohortMetric>> {
    let total = match cohorts.iter().try_fold(Dec::zero(), |sum, cohort| {
        sum.checked_add(cohort.quantity.0)
    }) {
        Ok(value) => value,
        Err(_) => return numeric_value_refusal("cohort_quantity_sum"),
    };
    if total.is_zero() {
        return Computed::Value(Vec::new());
    }
    let mut result = Vec::with_capacity(cohorts.len());
    let mut remaining_quantity = total;
    let mut remaining_postings = plan.postings.clone();
    for cohort in cohorts.iter().copied() {
        let (postings, remainder) = match split_postings(
            &remaining_postings,
            cohort.quantity.0,
            remaining_quantity,
        ) {
            Ok(value) => value,
            Err(reason) => return Computed::NotComputable { reason },
        };
        remaining_postings = remainder;
        remaining_quantity = match remaining_quantity.checked_sub(cohort.quantity.0) {
            Ok(value) => value,
            Err(_) => return numeric_value_refusal("cohort_remaining_quantity_sub"),
        };
        result.push(lifetime_cohort_metric(cohort, postings, plan.terminal_date));
    }
    Computed::Value(result)
}

/// Построить пожизненный результат прямо из книги лотов, сохраняя любой
/// `CohortGap` как различимый отказ вместо пустого списка когорт.
#[must_use]
pub fn lifetime_metrics_from_lots(
    lots: &InstrumentLots,
    plan: &CashflowPlan,
) -> Computed<Vec<LifetimeCohortMetric>> {
    match lots.cohorts() {
        Ok(cohorts) => lifetime_cohort_metrics(&cohorts, plan),
        Err(gap) => Computed::NotComputable {
            reason: NotComputable::CohortGap { gap },
        },
    }
}

fn split_postings(
    postings: &[ExpectedPosting],
    quantity: Dec,
    remaining_quantity: Dec,
) -> Result<(Vec<ExpectedPosting>, Vec<ExpectedPosting>), NotComputable> {
    let ratio = quantity
        .checked_div(remaining_quantity)
        .map_err(|_| NotComputable::Numeric { code: "cohort_quantity_div" })?;
    let mut result = Vec::with_capacity(postings.len());
    let mut remainder = Vec::with_capacity(postings.len());
    for posting in postings {
        let amount = posting
            .amount
            .checked_mul(ratio)
            .map_err(|_| NotComputable::Numeric { code: "cohort_posting_mul" })?;
        let remaining = posting
            .amount
            .checked_sub(amount)
            .map_err(|_| NotComputable::Numeric { code: "cohort_posting_remainder_sub" })?;
        result.push(ExpectedPosting { amount, ..*posting });
        remainder.push(ExpectedPosting {
            amount: remaining,
            ..*posting
        });
    }
    Ok((result, remainder))
}

/// Проверить, что все лоты используют одно состояние номинала.
pub fn common_principal_state(
    lots: &InstrumentLots,
    instrument: crate::ids::InstrumentId,
) -> Result<PrincipalState, NotComputable> {
    let mut state = None;
    for lot in lots.lots() {
        match state {
            None => state = Some(lot.principal),
            Some(previous) if previous == lot.principal => {}
            Some(_) => return Err(NotComputable::PrincipalStateAmbiguous { instrument }),
        }
    }
    if !lots.unpriced().0.is_zero() && state.is_some() {
        return Err(NotComputable::PrincipalStateAmbiguous { instrument });
    }
    Ok(match state {
        Some(value) => value,
        None => PrincipalState::Unknown,
    })
}

/// Добавить известный расход как датированный отрицательный поток.
pub fn apply_expense(
    mut postings: Vec<ExpectedPosting>,
    treatment: ExpenseTreatment,
) -> Result<Vec<ExpectedPosting>, NotComputable> {
    if let ExpenseTreatment::Known { amount, on } = treatment {
        let value = amount
            .to_calc_dec()
            .checked_neg()
            .map_err(|_| NotComputable::Numeric { code: "expense_apply" })?;
        postings.push(ExpectedPosting {
            date: on,
            amount: CalcMoney::new(value, amount.currency()),
            kind: PostingKind::OfferSettlement,
        });
        postings.sort_by_key(|posting| posting.date);
    }
    Ok(postings)
}
/// Рассчитать IRR с учётом политики расходов. Для неизвестного расхода
/// единственный честный результат — отказ: даты и места списания нет.
pub fn irr_for_expense_policy(
    postings: Vec<ExpectedPosting>,
    c0: CalcMoney,
    coordinate: Date,
    treatment: ExpenseTreatment,
) -> Computed<RateOutcome> {
    match treatment {
        ExpenseTreatment::Known { amount, on } => {
            match apply_expense(postings, ExpenseTreatment::Known { amount, on }) {
                Ok(adjusted) => irr_for_postings(&adjusted, c0, coordinate),
                Err(reason) => rate_refusal(reason),
            }
        }
        ExpenseTreatment::AbsentByPolicy => irr_for_postings(&postings, c0, coordinate),
        ExpenseTreatment::UnknownBoundedBy { upper } => {
            if upper.currency() != c0.currency() {
                rate_refusal(NotComputable::CurrencyMismatch {
                    expected: c0.currency(),
                    actual: upper.currency(),
                })
            } else {
                rate_refusal(NotComputable::ExpenseUnknown)
            }
        }
        ExpenseTreatment::Unknown => rate_refusal(NotComputable::ExpenseUnknown),
    }
}


/// Применить политику расходов, сохранив верхнюю и нижнюю границы.
pub fn expense_adjusted_metrics(
    postings: Vec<ExpectedPosting>,
    c0: CalcMoney,
    coordinate: Date,
    terminal_date: Date,
    treatment: ExpenseTreatment,
) -> ExpenseMetrics {
    match treatment {
        ExpenseTreatment::AbsentByPolicy => exact_expense_metrics(
            postings,
            c0,
            coordinate,
            terminal_date,
        ),
        ExpenseTreatment::Known { amount, on } => {
            let adjusted = match apply_expense(
                postings,
                ExpenseTreatment::Known { amount, on },
            ) {
                Ok(value) => value,
                Err(reason) => return ExpenseMetrics::NotComputable { reason },
            };
            exact_expense_metrics(adjusted, c0, coordinate, terminal_date)
        }
        ExpenseTreatment::UnknownBoundedBy { upper } => {
            let without_expense = match exact_expense_metrics(
                postings.clone(),
                c0,
                coordinate,
                terminal_date,
            ) {
                ExpenseMetrics::Exact(value) => value,
                ExpenseMetrics::NotComputable { reason } => {
                    return ExpenseMetrics::NotComputable { reason };
                }
                ExpenseMetrics::Bounded { .. } => {
                    return ExpenseMetrics::NotComputable {
                        reason: NotComputable::Numeric { code: "expense_bound_negate" },
                    };
                }
            };
            if upper.currency() != c0.currency() {
                return ExpenseMetrics::NotComputable {
                    reason: NotComputable::CurrencyMismatch {
                        expected: c0.currency(),
                        actual: upper.currency(),
                    },
                };
            }
            let negative_upper = match upper.to_calc_dec().checked_neg() {
                Ok(value) => value,
                Err(_) => {
                    return ExpenseMetrics::NotComputable {
                        reason: NotComputable::Numeric { code: "expense_bound_negate" },
                    };
                }
            };
            let mut adjusted = postings;
            adjusted.push(ExpectedPosting {
                date: terminal_date,
                amount: CalcMoney::new(negative_upper, upper.currency()),
                kind: PostingKind::OfferSettlement,
            });
            let with_upper_bound = match exact_expense_metrics(
                adjusted,
                c0,
                coordinate,
                terminal_date,
            ) {
                ExpenseMetrics::Exact(value) => value,
                ExpenseMetrics::NotComputable { reason } => {
                    return ExpenseMetrics::NotComputable { reason };
                }
                ExpenseMetrics::Bounded { .. } => {
                    return ExpenseMetrics::NotComputable {
                        reason: NotComputable::Numeric { code: "expense_bound_calculation" },
                    };
                }
            };
            ExpenseMetrics::Bounded {
                without_expense,
                with_upper_bound,
            }
        }
        ExpenseTreatment::Unknown => ExpenseMetrics::NotComputable {
            reason: NotComputable::ExpenseUnknown,
        },
    }
}

fn exact_expense_metrics(
    postings: Vec<ExpectedPosting>,
    c0: CalcMoney,
    coordinate: Date,
    terminal_date: Date,
) -> ExpenseMetrics {
    match zero_reinvestment_metrics(postings, c0, coordinate, terminal_date) {
        Computed::Value(value) => ExpenseMetrics::Exact(value),
        Computed::NotComputable { reason } => ExpenseMetrics::NotComputable { reason },
    }
}

/// Точное значение CAGR при полной потере капитала.
#[must_use]
pub fn exact_minus_one_rate() -> RateOutcome {
    RateOutcome::exact(-1.0, SolverPolicy::returns_default(), DayCount::Act365)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{PerUnitAmount, PostedMinor};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn dec(value: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(value).unwrap())
    }

    fn calc(value: &str) -> CalcMoney {
        CalcMoney::new(dec(value), CurrencyCode::Rub)
    }

    fn money(value: i64) -> Money {
        Money::new(PostedMinor::new(value), CurrencyCode::Rub)
    }

    fn posting(day: Date, value: &str) -> ExpectedPosting {
        ExpectedPosting {
            date: day,
            amount: calc(value),
            kind: PostingKind::Coupon,
        }
    }

    #[test]
    fn calculates_all_five_zero_reinvestment_values_without_rounding() {
        let result = zero_reinvestment_metrics(
            vec![
                posting(date!(2026 - 09 - 01), "3.333"),
                posting(date!(2027 - 09 - 01), "6.666"),
            ],
            calc("5"),
            date!(2026 - 08 - 28),
            date!(2027 - 09 - 01),
        );
        let Computed::Value(metrics) = result else { panic!("expected metrics") };
        assert_eq!(metrics.terminal_wealth.value(), dec("9.999"));
        assert_eq!(metrics.surplus.value(), dec("4.999"));
        assert_eq!(metrics.hpr, Computed::Value(dec("0.9998")));
        assert!(matches!(metrics.cagr_0r, Computed::Value(_)));
        assert!(metrics.zero_reinvestment_assumed);
        assert!(metrics.pre_tax);
    }

    #[test]
    fn terminal_date_and_cagr_follow_the_selected_offer_scenario() {
        let hold = zero_reinvestment_metrics(
            vec![posting(date!(2030 - 01 - 01), "200")],
            calc("100"),
            date!(2026 - 01 - 01),
            date!(2030 - 01 - 01),
        );
        let offer = zero_reinvestment_metrics(
            vec![posting(date!(2027 - 01 - 01), "120")],
            calc("100"),
            date!(2026 - 01 - 01),
            date!(2027 - 01 - 01),
        );
        let Computed::Value(hold) = hold else { panic!("hold") };
        let Computed::Value(offer) = offer else { panic!("offer") };
        assert_ne!(hold.cagr_0r, offer.cagr_0r);
    }

    #[test]
    fn quotation_money_per_unit_is_not_multiplied_by_face() {
        let c0 = prospective_c0(
            Quantity(dec("10")),
            QuotationBasis::MoneyPerUnit,
            dec("12.34"),
            CurrencyCode::Rub,
            None,
            calc("0"),
        );
        assert_eq!(c0, Computed::Value(calc("123.4")));
    }

    #[test]
    fn percentage_quotation_is_divided_by_one_hundred() {
        let c0 = prospective_c0(
            Quantity(dec("10")),
            QuotationBasis::PercentOfRemainingFace,
            dec("98.5"),
            CurrencyCode::Rub,
            Some(PerUnitAmount::new(dec("1000"), CurrencyCode::Rub)),
            calc("0"),
        );
        assert_eq!(c0, Computed::Value(calc("9850")));
    }

    #[test]
    fn non_positive_duration_refuses_only_cagr() {
        let Computed::Value(metrics) = zero_reinvestment_metrics(
            vec![posting(date!(2026 - 08 - 28), "110")],
            calc("100"),
            date!(2026 - 08 - 28),
            date!(2026 - 08 - 28),
        ) else { panic!("expected metrics") };
        assert_eq!(metrics.hpr, Computed::Value(dec("0.1")));
        assert!(matches!(metrics.cagr_0r, Computed::NotComputable { .. }));
    }

    #[test]
    fn zero_terminal_wealth_is_exactly_minus_one_hundred_percent() {
        let Computed::Value(metrics) = zero_reinvestment_metrics(
            Vec::new(),
            calc("100"),
            date!(2026 - 01 - 01),
            date!(2027 - 01 - 01),
        ) else { panic!("expected metrics") };
        assert_eq!(metrics.hpr, Computed::Value(dec("-1")));
        assert_eq!(metrics.cagr_0r, Computed::Value(exact_minus_one_rate()));
    }

    #[test]
    fn negative_terminal_wealth_refuses_cagr_but_keeps_hpr() {
        let Computed::Value(metrics) = zero_reinvestment_metrics(
            vec![posting(date!(2027 - 01 - 01), "-1")],
            calc("100"),
            date!(2026 - 01 - 01),
            date!(2027 - 01 - 01),
        ) else { panic!("expected metrics") };
        assert_eq!(metrics.hpr, Computed::Value(dec("-1.01")));
        assert!(matches!(metrics.cagr_0r, Computed::NotComputable { .. }));
    }

    #[test]
    fn non_positive_c0_refuses_hpr_and_cagr() {
        let Computed::Value(metrics) = zero_reinvestment_metrics(
            vec![posting(date!(2027 - 01 - 01), "1")],
            calc("0"),
            date!(2026 - 01 - 01),
            date!(2027 - 01 - 01),
        ) else { panic!("expected metrics") };
        assert!(matches!(metrics.hpr, Computed::NotComputable { .. }));
        assert!(matches!(metrics.cagr_0r, Computed::NotComputable { .. }));
    }

    #[test]
    fn lifetime_c0_uses_historical_acquisition_basis_after_amortisation() {
        let cohort = Cohort {
            acquired: TradeDate(date!(2026 - 01 - 01)),
            quantity: Quantity(dec("1")),
            cost_basis: money(80000),
            acquisition_basis: Some(money(100000)),
            accrued_interest_paid: Some(money(0)),
            received_to_date: Some(money(20000)),
        };
        let metric = lifetime_cohort_metric(
            cohort,
            vec![posting(date!(2027 - 01 - 01), "800")],
            date!(2027 - 01 - 01),
        );
        assert_eq!(metric.c0, Computed::Value(calc("1000")));
        let Computed::Value(metrics) = metric.metrics else { panic!("metrics") };
        assert_eq!(metrics.hpr, Computed::Value(dec("0")));
        assert_eq!(
            metric.irr_absent_because,
            "прошлые выплаты хранятся одной суммой без дат, поэтому ряд потоков для пожизненного IRR нельзя восстановить; YTM рассчитывается только для проспективного знаменателя"
        );
    }

    #[test]
    fn unknown_cohort_inputs_are_not_replaced_with_zero() {
        let cohort = Cohort {
            acquired: TradeDate(date!(2026 - 01 - 01)),
            quantity: Quantity(dec("1")),
            cost_basis: money(100000),
            acquisition_basis: None,
            accrued_interest_paid: None,
            received_to_date: None,
        };
        let metric = lifetime_cohort_metric(cohort, Vec::new(), date!(2027 - 01 - 01));
        assert!(matches!(metric.c0, Computed::NotComputable { .. }));
        assert!(matches!(metric.metrics, Computed::NotComputable { .. }));
    }

    #[test]
    fn known_expense_is_a_dated_negative_cashflow() {
        let result = apply_expense(
            vec![posting(date!(2027 - 01 - 01), "100")],
            ExpenseTreatment::Known { amount: money(1000), on: date!(2026 - 09 - 01) },
        ).unwrap();
        assert_eq!(result[0].amount.value(), dec("-10"));
        assert_eq!(result[0].date, date!(2026 - 09 - 01));
    }

    #[test]
    fn bounded_unknown_expense_returns_a_range_not_a_flagged_exact_value() {
        let result = expense_adjusted_metrics(
            vec![posting(date!(2027 - 01 - 01), "100")],
            calc("50"),
            date!(2026 - 01 - 01),
            date!(2027 - 01 - 01),
            ExpenseTreatment::UnknownBoundedBy { upper: money(1000) },
        );
        assert!(matches!(result, ExpenseMetrics::Bounded { .. }));
    }
    #[test]
    fn future_postings_are_split_by_current_cohort_quantity_without_loss() {
        let cohort = |quantity| Cohort {
            acquired: TradeDate(date!(2026 - 01 - 01)),
            quantity: Quantity(dec(quantity)),
            cost_basis: money(100000),
            acquisition_basis: Some(money(100000)),
            accrued_interest_paid: Some(money(0)),
            received_to_date: Some(money(0)),
        };
        let plan = CashflowPlan {
            postings: vec![posting(date!(2027 - 01 - 01), "100")],
            terminal_date: date!(2027 - 01 - 01),
            past: Vec::new(),
        };
        let Computed::Value(metrics) =
            lifetime_cohort_metrics(&[cohort("30"), cohort("70")], &plan)
        else {
            panic!("cohorts")
        };
        assert_eq!(metrics[0].metrics.value().unwrap().postings[0].amount.value(), dec("30"));
        assert_eq!(metrics[1].metrics.value().unwrap().postings[0].amount.value(), dec("70"));
    }

    #[test]
    fn offer_scenario_is_labelled_yield_to_offer() {
        let choice = OfferChoice::ExerciseAtOffer {
            window: crate::bond::offer::OfferWindowId::derive(
                crate::ids::InstrumentId::new_random(),
                date!(2027 - 01 - 01),
            ),
        };
        let plan = CashflowPlan {
            postings: vec![posting(date!(2027 - 01 - 01), "100")],
            terminal_date: date!(2027 - 01 - 01),
            past: Vec::new(),
        };
        let metric = prospective_metric(
            date!(2026 - 01 - 01),
            &plan,
            Computed::Value(calc("50")),
            &choice,
        );
        assert_eq!(metric.irr_label, IrrLabel::YieldToOffer);
        assert_eq!(metric.terminal_date, date!(2027 - 01 - 01));
    }
    #[test]
    fn unknown_expense_cannot_produce_an_irr() {
        let result = irr_for_expense_policy(
            vec![posting(date!(2027 - 01 - 01), "100")],
            calc("50"),
            date!(2026 - 01 - 01),
            ExpenseTreatment::Unknown,
        );
        assert_eq!(
            result.reason().map(NotComputable::code),
            Some("expense_unknown")
        );
    }
}
