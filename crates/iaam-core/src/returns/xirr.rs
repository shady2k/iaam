//! Доменная обёртка решателя ставки (§6.1).
//!
//! Здесь живёт то, что решатель знать не должен: границы контура, валюты,
//! курсы, цены и знаковая конвенция. Сам решатель работает с парами
//! «смещение в днях, сумма» и о портфеле ничего не знает.

use time::Date;

use super::{Computed, NotComputable, ReturnsRequest};
use crate::money::CurrencyCode;
use crate::numeric::decimal::Dec;
use crate::numeric::xirr::{DayCount, RateOutcome, SolverFlow, solve};
use crate::projection::flows::FlowDirection;
use crate::projection::state::LedgerState;
use crate::valuation::convert;

/// Ряд потоков в валюте отчёта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSeries {
    /// Внесено за всю историю, положительная величина.
    pub contributed: Dec,
    /// Выведено за всю историю, положительная величина.
    pub withdrawn: Dec,
    /// Датированные суммы **в знаковой конвенции владельца**: внесение
    /// отрицательно, изъятие положительно. Это отрицание движения денег
    /// по контуру: то, что для контура приход, для владельца расход.
    pub flows: Vec<(Date, Dec)>,
}

/// Перевод внешних потоков в валюту отчёта.
pub fn flow_series(
    state: &LedgerState,
    request: &ReturnsRequest,
) -> Result<FlowSeries, NotComputable> {
    guard_state_not_newer(state, request.as_of)?;
    let mut contributed = Dec::zero();
    let mut withdrawn = Dec::zero();
    let mut flows = Vec::new();

    for flow in state.flows().external() {
        if flow.date > request.as_of {
            continue;
        }
        let converted = convert(flow.amount, request.report_currency, flow.date, request.fx)?;
        match flow.direction {
            FlowDirection::In => contributed = add(contributed, converted)?,
            FlowDirection::Out => withdrawn = sub(withdrawn, converted)?,
        }
        flows.push((flow.date, neg(converted)?));
    }
    flows.sort_by_key(|(date, _)| *date);
    Ok(FlowSeries {
        contributed,
        withdrawn,
        flows,
    })
}

/// Стоимость контура на дату отчёта: деньги плюс позиции по последней цене.
///
/// Это **ликвидационная** оценка в упрощённом виде (§5.1): комиссий
/// закрытия и налога к уплате в ней нет, потому что ни того, ни другого
/// этап 1 не считает. Разрыв с `contractual_hold_value` не вычисляется —
/// вклады и облигации целиком относятся к E3.
pub fn terminal_value(state: &LedgerState, request: &ReturnsRequest) -> Result<Dec, NotComputable> {
    guard_state_not_newer(state, request.as_of)?;
    let mut total = Dec::zero();

    for (account, money) in state.balances().iter_cash() {
        if !request.contour.contains(account) {
            continue;
        }
        total = add(
            total,
            convert(money, request.report_currency, request.as_of, request.fx)?,
        )?;
    }

    for (key, quantity) in state.balances().iter_positions() {
        if !request.contour.contains(key.account) {
            continue;
        }
        if quantity.0.is_zero() {
            continue;
        }
        let price = state
            .prices()
            .latest(key.instrument)
            .ok_or(NotComputable::MissingPrice {
                instrument: key.instrument,
            })?;
        let local = mul(quantity.0, price.price)?;
        total = add(total, in_report_currency(local, price.currency, request)?)?;
    }
    Ok(total)
}

/// Ставка по ряду потоков и терминальной стоимости.
pub fn rate(
    series: &Result<FlowSeries, NotComputable>,
    terminal: &Result<Dec, NotComputable>,
    request: &ReturnsRequest,
) -> Computed<RateOutcome> {
    let series = match series {
        Ok(series) => series,
        Err(reason) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    let terminal = match terminal {
        Ok(value) => *value,
        Err(reason) => {
            return Computed::NotComputable {
                reason: reason.clone(),
            };
        }
    };
    let Some((first_date, _)) = series.flows.first() else {
        return Computed::NotComputable {
            reason: NotComputable::NoExternalFlows,
        };
    };

    let mut solver_flows: Vec<SolverFlow> = series
        .flows
        .iter()
        .map(|(date, amount)| SolverFlow {
            day_offset: (*date - *first_date).whole_days(),
            amount: *amount,
        })
        .collect();
    solver_flows.push(SolverFlow {
        day_offset: (request.as_of - *first_date).whole_days(),
        amount: terminal,
    });

    match solve(&solver_flows, request.solver_policy, DayCount::Act365) {
        Ok(outcome) => Computed::Value(outcome),
        Err(refusal) => Computed::NotComputable {
            reason: NotComputable::SolverRefused { refusal },
        },
    }
}

/// Состояние обязано быть спроецировано **по дату отчёта**: фильтрацию
/// журнала делает оболочка при сборке среза, а не ядро. Событие позже
/// даты отчёта означает, что срез собран неверно, и молча посчитать
/// по нему — значит выдать отчёт на дату, которого на эту дату не было.
fn guard_state_not_newer(state: &LedgerState, as_of: Date) -> Result<(), NotComputable> {
    match state.coverage().last_event() {
        Some(last) if last > as_of => Err(NotComputable::StateNewerThanReport {
            last_event: last,
            as_of,
        }),
        _ => Ok(()),
    }
}

fn in_report_currency(
    amount: Dec,
    currency: CurrencyCode,
    request: &ReturnsRequest,
) -> Result<Dec, NotComputable> {
    let rate = request
        .fx
        .rate(currency, request.report_currency, request.as_of)
        .ok_or(NotComputable::MissingFxRate {
            from: currency,
            to: request.report_currency,
            date: request.as_of,
        })?;
    mul(amount, rate)
}

fn add(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_add(right).map_err(numeric)
}

fn sub(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_sub(right).map_err(numeric)
}

fn mul(left: Dec, right: Dec) -> Result<Dec, NotComputable> {
    left.checked_mul(right).map_err(numeric)
}

fn neg(value: Dec) -> Result<Dec, NotComputable> {
    value.checked_neg().map_err(numeric)
}

fn numeric(_: crate::numeric::NumericError) -> NotComputable {
    NotComputable::Numeric { code: "numeric" }
}
